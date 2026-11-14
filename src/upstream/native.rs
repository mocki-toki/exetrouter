//! Unary native Codex tools. No Responses emulation, model substitution or replay.
use super::{account_headers, Account, SelectError, Selection, Upstream};
use crate::{pool::Candidate, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feature {
    Images,
    Search,
}

impl Feature {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Images => "images",
            Self::Search => "search",
        }
    }
}

pub(crate) fn paused(
    conn: &Connection,
    account: i64,
    feature: Feature,
) -> Result<Option<(i64, bool)>> {
    Ok(conn.query_row(
        "SELECT retry_at,kind='quota' FROM native_feature_pauses WHERE account_id=?1 AND feature=?2 AND retry_at>?3",
        params![account, feature.name(), Utc::now().timestamp()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?)
}

pub(crate) fn pause(
    conn: &Connection,
    account: i64,
    generation: i64,
    feature: Feature,
    until: i64,
    order: i64,
    quota: bool,
) -> Result<()> {
    // Success never clears a quota refusal; a concurrent older refusal can only
    // extend a pause. Credential generations fence stale writes; refreshing a
    // bearer does not replenish feature quota or erase an existing pause.
    conn.execute(
        "INSERT INTO native_feature_pauses SELECT ?1,?2,?3,?4,?5,?6 WHERE EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?3 AND state='active') ON CONFLICT(account_id,feature) DO UPDATE SET generation=excluded.generation,kind=CASE WHEN excluded.retry_at>=native_feature_pauses.retry_at THEN excluded.kind ELSE native_feature_pauses.kind END,retry_at=MAX(native_feature_pauses.retry_at,excluded.retry_at),last_order=MAX(native_feature_pauses.last_order,excluded.last_order)",
        params![account, feature.name(), generation, until, order, if quota { "quota" } else { "backoff" }],
    )?;
    Ok(())
}

impl Upstream {
    pub(crate) async fn select_native(
        &self,
        feature: Feature,
        model: &str,
        hint: Option<&str>,
        pinned: Option<i64>,
    ) -> std::result::Result<Selection, SelectError> {
        let blocked = self.db.call(move |conn| {
            let mut stmt = conn.prepare("SELECT p.account_id,p.retry_at,p.kind='quota' FROM native_feature_pauses p JOIN oauth_accounts a ON a.id=p.account_id WHERE a.state='active' AND p.feature=?1 AND p.retry_at>?2")?;
            let rows = stmt.query_map(params![feature.name(), Utc::now().timestamp()], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,i64>(1)?, row.get::<_,bool>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(rows)
        }).await.map_err(|_| SelectError::UpstreamUnavailable)?;
        if let Some(id) = pinned {
            if let Some((_, until, quota)) = blocked.iter().find(|(account, _, _)| *account == id) {
                return Err(wait(*until, *quota));
            }
            return self.select_pinned(model, id).await;
        }
        if feature == Feature::Search {
            let excluded = blocked.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
            return match self.select_codex_excluding(model, hint, &excluded).await {
                Err(SelectError::UpstreamUnavailable) if !blocked.is_empty() => {
                    let (_, at, quota) = blocked
                        .iter()
                        .min_by_key(|(_, at, _)| at)
                        .expect("nonempty");
                    Err(wait(*at, *quota))
                }
                result => result,
            };
        }
        // Image models are not conversational catalog entries. Do not invent
        // aliases there or require a host LLM; ordinary text quota is unrelated.
        let (mut candidates, cooldown) = self
            .db
            .call(move |conn| {
                let mut candidates = Vec::new();
                let mut cooldown: Option<(i64, bool)> = None;
                for info in crate::oauth::list(conn)? {
                    if info.state == "active"
                        && crate::account_preferences::effective(conn, info.id)?.enabled
                        && info
                            .health_until
                            .is_none_or(|at| at <= Utc::now().timestamp())
                    {
                        if let Some((_, until, quota)) =
                            blocked.iter().find(|(id, _, _)| *id == info.id)
                        {
                            if cooldown.is_none_or(|old| *until < old.0) {
                                cooldown = Some((*until, *quota));
                            }
                            continue;
                        }
                        candidates.push(Candidate {
                            id: info.id,
                            quota: None,
                            routing: Default::default(),
                        });
                    }
                }
                Ok((candidates, cooldown))
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        while let Some(reservation) = self.pool.reserve_affinity(&candidates, hint) {
            candidates.retain(|candidate| candidate.id != reservation.id);
            let Ok(account) = self.account(reservation.id).await else {
                continue;
            };
            if free_plan(&account.credentials.access_token) {
                continue;
            }
            if self.native_eligible(&account, feature).await.is_ok() {
                return Ok(Selection {
                    account,
                    _reservation: reservation,
                });
            }
        }
        Err(
            cooldown.map_or(SelectError::UpstreamUnavailable, |(at, quota)| {
                wait(at, quota)
            }),
        )
    }

    pub(crate) async fn native_eligible(&self, account: &Account, feature: Feature) -> Result<()> {
        let (id, generation) = (account.info.id, account.info.generation);
        self.db.call(move |conn| {
            let valid = conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')", params![id,generation], |row| row.get::<_,bool>(0))?;
            if !valid || !crate::account_preferences::effective(conn, id)?.enabled
                || crate::health::paused(conn, id, Utc::now().timestamp())?.is_some()
                || paused(conn, id, feature)?.is_some()
                || (feature == Feature::Search && crate::quota::blocked_until(conn, id, Utc::now().timestamp())?.is_some())
            { return Err("native account unavailable".into()); }
            Ok(())
        }).await
    }

    pub(crate) async fn post_native(
        &self,
        account: &Account,
        path: &'static str,
        body: &Value,
        headers: reqwest::header::HeaderMap,
    ) -> Result<reqwest::Response> {
        // path is selected by a router handler, never by the caller.
        Ok(self
            .inference_client
            .post(format!("{}/{path}", self.config.base_url))
            .headers(account_headers(account)?)
            .headers(headers)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(body)
            .send()
            .await?)
    }
}

fn free_plan(token: &str) -> bool {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|claims| claims["https://api.openai.com/auth"]["chatgpt_plan_type"] == "free")
}

fn wait(at: i64, quota: bool) -> SelectError {
    if quota {
        SelectError::Cooldown(at)
    } else {
        SelectError::Backoff(at)
    }
}
