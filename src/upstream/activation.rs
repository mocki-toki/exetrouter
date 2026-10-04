//! A single, durably guarded request to start an apparently inactive weekly window.
use super::{SseDecoder, Upstream};
use crate::{quota::Summary, usage::Counters, Result};
use chrono::Utc;
use futures_util::StreamExt;
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::json;
use std::time::Duration;

const WEEK: i64 = 7 * 24 * 60 * 60;
const MODEL: &str = "gpt-5.6-sol";

fn inactive_reset(quota: &Summary, now: i64) -> Option<i64> {
    if quota.cooldown_until.is_some_and(|until| until > now) {
        return None;
    }
    quota.windows.iter().find_map(|window| {
        let reset = window.reset_at?;
        (window.status == "current"
            && window.window_minutes == Some(10_080)
            && window.used_percent == 0.0
            && window.remaining_percent == 100.0
            && (0..=30).contains(&(now - window.observed_at))
            && reset.div_euclid(60) == (window.observed_at + WEEK).div_euclid(60))
        .then_some(reset)
    })
}

fn claim(
    conn: &mut Connection,
    account: i64,
    generation: i64,
    user: i64,
    now: i64,
) -> Result<Option<i64>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // A cancelled caller or a crashed process may have sent the request. Never replay it.
    tx.execute("UPDATE quota_activation_attempts SET status='unknown' WHERE status='pending' AND attempted_at<=?1", [now - 60])?;
    let quota = crate::quota::summary(&tx, account, now)?;
    let Some(reset) = inactive_reset(&quota, now) else {
        tx.commit()?;
        return Ok(None);
    };
    let blocked: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM quota_activation_attempts WHERE (account_id=?1 AND attempted_at>?2) OR status='pending') OR EXISTS(SELECT 1 FROM usage_events WHERE account_id=?1 AND at_utc>?3)",
        params![account, now-WEEK, now-60], |r| r.get(0))?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')", params![account,generation], |row| row.get(0))?;
    if !valid
        || blocked
        || !crate::account_preferences::effective(&tx, Some(user), account)?.enabled
        || crate::health::paused(&tx, account, now)?.is_some()
    {
        tx.commit()?;
        return Ok(None);
    }
    tx.execute("INSERT INTO quota_activation_attempts(account_id,user_id,attempted_at,reset_at,model,status) VALUES(?1,?2,?3,?4,?5,'pending')", params![account,user,now,reset,MODEL])?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(Some(id))
}

impl Upstream {
    pub(crate) async fn activate_weekly_window(
        &self,
        user: i64,
        account: i64,
        quota: &Summary,
    ) -> Option<String> {
        inactive_reset(quota, Utc::now().timestamp())?;
        let _permit = self.activation_gate.try_lock().ok()?;
        let selected = self
            .select_pinned_for_user(MODEL, account, Some(user))
            .await
            .ok()?;
        let generation = selected.info.generation;
        let attempt = self
            .db
            .call(move |conn| claim(conn, account, generation, user, Utc::now().timestamp()))
            .await
            .ok()??;
        let operation = async {
            let order = self.db.call(|conn| crate::health::next(conn)).await?;
            let body = json!({"model":MODEL,"instructions":"Reply with exactly OK.","input":[{"role":"user","content":[{"type":"input_text","text":"OK"}]}],"reasoning":{"effort":"low"},"store":false,"stream":true});
            let response = self
                .post(&selected, "responses", &body, None, order, None)
                .await?;
            let generation = selected.info.generation;
            let now = Utc::now().timestamp();
            let observation =
                crate::quota::headers(response.headers(), response.status().as_u16(), now);
            self.db
                .call(move |conn| {
                    crate::quota::save(conn, account, generation, order, observation, now)
                })
                .await?;
            if !response.status().is_success() {
                let status =
                    if response.status().is_server_error() || response.status().as_u16() == 408 {
                        "unknown"
                    } else {
                        "rejected"
                    };
                return Ok((status, Counters::default()));
            }
            let mut stream = response.bytes_stream();
            let mut decoder = SseDecoder::default();
            let mut bytes = 0usize;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                bytes = bytes.saturating_add(chunk.len());
                if bytes > 65_536 {
                    return Err("activation response too large".into());
                }
                for frame in decoder.push(&chunk)? {
                    if let Some(event) = frame.event {
                        let now = Utc::now().timestamp();
                        let observation = crate::quota::event(&event, now);
                        if !observation.is_empty() {
                            self.db
                                .call(move |conn| {
                                    crate::quota::save(
                                        conn,
                                        account,
                                        generation,
                                        order,
                                        observation,
                                        now,
                                    )
                                })
                                .await?;
                        }
                        let kind = event["type"].as_str().unwrap_or("");
                        if matches!(
                            kind,
                            "response.completed" | "response.incomplete" | "response.failed"
                        ) {
                            let counters = Counters::from_json(event.pointer("/response/usage"))?;
                            let status = if kind == "response.completed" {
                                "completed"
                            } else {
                                "incomplete"
                            };
                            if kind != "response.failed" {
                                let _ = self
                                    .success(&selected, crate::health::Scope::Responses, order)
                                    .await;
                            }
                            return Ok((status, counters));
                        }
                    }
                }
            }
            Err("activation outcome unknown".into())
        };
        let result: Result<(&str, Counters)> =
            tokio::time::timeout(Duration::from_secs(20), operation)
                .await
                .unwrap_or_else(|_| Err("activation outcome unknown".into()));
        let (status, counters) = result.unwrap_or(("unknown", Counters::default()));
        let saved = self.db.call(move |conn| {
            conn.execute("UPDATE quota_activation_attempts SET status=?1,input_tokens=?2,output_tokens=?3,cached_input_tokens=?4,reasoning_output_tokens=?5 WHERE id=?6", params![status,counters.input,counters.output,counters.cached_input,counters.reasoning_output,attempt])?;
            Ok(())
        }).await;
        Some(if saved.is_ok() { status } else { "unknown" }.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quota(now: i64) -> Summary {
        Summary {
            status: "current".into(),
            cooldown_until: None,
            cooldown_source: None,
            windows: vec![crate::quota::Window {
                kind: "secondary".into(),
                used_percent: 0.0,
                remaining_percent: 100.0,
                window_minutes: Some(10080),
                reset_at: Some(now + WEEK),
                observed_at: now,
                status: "current".into(),
            }],
        }
    }
    #[test]
    fn weekly_activation_requires_exact_minute_fresh_weekly_and_full_remaining() {
        let now = 1_800_000_020;
        let q = quota(now);
        assert_eq!(inactive_reset(&q, now), Some(now + WEEK));
        let mut q = q;
        q.windows[0].reset_at = Some(now + WEEK - 20);
        assert!(inactive_reset(&q, now).is_some());
        q.windows[0].reset_at = Some(now + WEEK + 60);
        assert!(inactive_reset(&q, now).is_none());
        q.windows[0].reset_at = Some(now - WEEK);
        assert!(inactive_reset(&q, now).is_none());
        q = quota(now);
        q.windows[0].used_percent = 0.001;
        assert!(inactive_reset(&q, now).is_none());
        q = quota(now);
        q.windows[0].remaining_percent = 99.9;
        assert!(inactive_reset(&q, now).is_none());
        q = quota(now);
        q.windows[0].window_minutes = Some(300);
        assert!(inactive_reset(&q, now).is_none());
        q = quota(now);
        assert!(inactive_reset(&q, now + 31).is_none());
        assert!(inactive_reset(&q, now - 1).is_none());
        q.cooldown_until = Some(now + 10);
        assert!(inactive_reset(&q, now).is_none());
    }
    #[test]
    fn weekly_activation_claim_survives_interruption_and_moving_reset() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::store::init(&conn).unwrap();
        let user = crate::create_user(&conn, "synthetic").unwrap();
        let account = crate::oauth::save(
            &conn,
            &crate::oauth::Vault::new([2; 32]),
            "synthetic",
            &crate::oauth::Credentials {
                access_token: "synthetic".into(),
                refresh_token: "synthetic".into(),
            },
            i64::MAX,
        )
        .unwrap();
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO oauth_quota_windows VALUES(?1,'secondary',0,10080,?2,?3,1)",
            params![account, now + WEEK, now],
        )
        .unwrap();
        assert!(claim(&mut conn, account, 0, user, now).unwrap().is_some());
        conn.execute(
            "UPDATE oauth_quota_windows SET observed_at=?1,reset_at=?2",
            params![now + 61, now + 61 + WEEK],
        )
        .unwrap();
        assert!(claim(&mut conn, account, 0, user, now + 61)
            .unwrap()
            .is_none());
        let status: String = conn
            .query_row("SELECT status FROM quota_activation_attempts", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "unknown");
        conn.execute(
            "UPDATE oauth_quota_windows SET observed_at=?1,reset_at=?2",
            params![now + WEEK, now + WEEK * 2],
        )
        .unwrap();
        assert!(claim(&mut conn, account, 0, user, now + WEEK)
            .unwrap()
            .is_some());
    }
}
