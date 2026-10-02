//! Passive Codex quota observations, independent of local token usage.
use crate::Result;
use reqwest::header::HeaderMap;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BACKOFF_SECONDS: i64 = 60;
const MAX_TIMESTAMP: i64 = 253_402_300_799;

#[derive(Clone, Serialize, Deserialize)]
pub struct Window {
    pub kind: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub window_minutes: Option<i64>,
    pub reset_at: Option<i64>,
    pub observed_at: i64,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Summary {
    pub status: String,
    pub windows: Vec<Window>,
    pub cooldown_until: Option<i64>,
    pub cooldown_source: Option<String>,
}

pub(crate) struct Sample {
    kind: &'static str,
    used: f64,
    minutes: Option<i64>,
    reset: Option<i64>,
}

#[derive(Default)]
pub(crate) struct Observation {
    windows: Vec<Sample>,
    cooldown: Option<(i64, &'static str)>,
}

fn timestamp(value: Option<i64>) -> Option<i64> {
    value.filter(|value| (0..=MAX_TIMESTAMP).contains(value))
}

fn sample(
    kind: &'static str,
    used: Option<f64>,
    minutes: Option<i64>,
    reset: Option<i64>,
) -> Option<Sample> {
    Some(Sample {
        kind,
        used: used.filter(|value| value.is_finite() && *value >= 0.0)?,
        minutes: minutes.filter(|value| *value > 0),
        reset: timestamp(reset),
    })
}

pub(crate) fn retry_at(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        if seconds < 0 {
            return None;
        }
        return timestamp(now.checked_add(seconds));
    }
    timestamp(
        chrono::DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|date| date.timestamp()),
    )
}

impl Observation {
    /// Forward only validated numeric general-meter metadata. Never proxy
    /// arbitrary header names/values or fabricate absent windows/credits.
    pub(crate) fn response_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for window in &self.windows {
            let used = window.used.to_string();
            if used.len() > 64 {
                continue;
            }
            for (field, value) in [
                ("used-percent", Some(used)),
                (
                    "window-minutes",
                    window.minutes.map(|value| value.to_string()),
                ),
                ("reset-at", window.reset.map(|value| value.to_string())),
            ] {
                if let Some(value) = value {
                    let name = reqwest::header::HeaderName::from_bytes(
                        format!("x-codex-{}-{field}", window.kind).as_bytes(),
                    )
                    .expect("fixed quota header");
                    headers.insert(name, value.parse().expect("validated numeric quota value"));
                }
            }
        }
        headers
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.cooldown.is_none()
    }
    pub(crate) fn cooldown_until(&self) -> Option<i64> {
        self.cooldown.map(|(until, _)| until)
    }
    fn rejection(&mut self, retry: Option<i64>, reset: Option<i64>, now: i64) {
        let window_reset = self
            .windows
            .iter()
            .filter(|window| window.used >= 100.0)
            .filter_map(|window| window.reset)
            .filter(|at| *at > now)
            .max();
        let upstream_reset = timestamp(reset)
            .filter(|at| *at > now)
            .into_iter()
            .chain(window_reset)
            .max();
        self.cooldown = Some(match (retry.filter(|at| *at >= now), upstream_reset) {
            (Some(retry), Some(reset)) if reset > retry => (reset, "upstream_reset"),
            (Some(retry), _) => (retry, "retry_after"),
            (None, Some(reset)) => (reset, "upstream_reset"),
            _ => (now.saturating_add(BACKOFF_SECONDS), "local_backoff"),
        });
    }
}

pub(crate) fn headers(headers: &HeaderMap, status: u16, now: i64) -> Observation {
    let mut result = Observation::default();
    for kind in ["primary", "secondary"] {
        let text = |suffix: &str| {
            headers
                .get(format!("x-codex-{kind}-{suffix}"))?
                .to_str()
                .ok()
        };
        if let Some(window) = sample(
            kind,
            text("used-percent").and_then(|v| v.parse().ok()),
            text("window-minutes").and_then(|v| v.parse().ok()),
            text("reset-at").and_then(|v| v.parse().ok()),
        ) {
            result.windows.push(window);
        }
    }
    if status == 429 {
        result.rejection(
            headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_at(v, now)),
            None,
            now,
        );
    }
    result
}

pub(crate) fn http(
    headers: &HeaderMap,
    status: u16,
    body: Option<&Value>,
    now: i64,
) -> Observation {
    let mut result = self::headers(headers, status, now);
    if status == 429 {
        result.rejection(
            headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_at(v, now)),
            body.and_then(|body| body.pointer("/error/resets_at"))
                .and_then(Value::as_i64),
            now,
        );
    }
    result
}

pub(crate) fn event(event: &Value, now: i64) -> Observation {
    let mut result = Observation::default();
    if event.get("type").and_then(Value::as_str) == Some("codex.rate_limits") {
        let limit = event
            .get("metered_limit_name")
            .and_then(Value::as_str)
            .or_else(|| event.get("limit_name").and_then(Value::as_str));
        // Model-specific meters need a model-to-bucket mapping before they can
        // be used as an account-wide quota. Only the shared Codex meter is read.
        if limit.is_none_or(|value| value.trim().eq_ignore_ascii_case("codex")) {
            for kind in ["primary", "secondary"] {
                if let Some(window) = event.get("rate_limits").and_then(|limits| limits.get(kind)) {
                    if let Some(window) = sample(
                        kind,
                        window.get("used_percent").and_then(Value::as_f64),
                        window.get("window_minutes").and_then(Value::as_i64),
                        window.get("reset_at").and_then(Value::as_i64),
                    ) {
                        result.windows.push(window);
                    }
                }
            }
        }
    }
    let error = event
        .get("error")
        .filter(|error| error.is_object())
        .or_else(|| {
            event
                .pointer("/response/error")
                .filter(|error| error.is_object())
        });
    let rate_error = error.is_some_and(|error| {
        ["type", "code"].iter().any(|key| {
            matches!(
                error.get(*key).and_then(Value::as_str),
                Some(
                    "usage_limit_reached"
                        | "rate_limit_exceeded"
                        | "slow_down"
                        | "rate_limit_error"
                )
            )
        })
    });
    if event.get("status").and_then(Value::as_u64) == Some(429) || rate_error {
        let mut parsed = HeaderMap::new();
        if let Some(values) = event.get("headers").and_then(Value::as_object) {
            for (name, value) in values {
                let known = name.eq_ignore_ascii_case("retry-after")
                    || ["primary", "secondary"].iter().any(|kind| {
                        ["used-percent", "window-minutes", "reset-at"]
                            .iter()
                            .any(|field| {
                                name.eq_ignore_ascii_case(&format!("x-codex-{kind}-{field}"))
                            })
                    });
                if !known {
                    continue;
                }
                let text = match value {
                    Value::String(value) => value.clone(),
                    Value::Number(number) => number.to_string(),
                    _ => continue,
                };
                if text.len() > 128 {
                    continue;
                }
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                    reqwest::header::HeaderValue::from_str(&text),
                ) {
                    parsed.insert(name, value);
                }
            }
        }
        result = headers(&parsed, 200, now);
        result.rejection(
            parsed
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_at(v, now)),
            error
                .and_then(|error| error.get("resets_at"))
                .and_then(Value::as_i64),
            now,
        );
    }
    result
}

pub(crate) fn save(
    conn: &mut Connection,
    account: i64,
    generation: i64,
    order: i64,
    observation: Observation,
    now: i64,
) -> Result<()> {
    if observation.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    let valid: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')",params![account,generation],|row|row.get(0))?;
    if !valid {
        return Ok(());
    }
    for window in observation.windows {
        tx.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(account_id,kind) DO UPDATE SET used_percent=excluded.used_percent,window_minutes=excluded.window_minutes,reset_at=excluded.reset_at,observed_at=excluded.observed_at,request_order=excluded.request_order
            WHERE excluded.request_order >= oauth_quota_windows.request_order",
            params![account,window.kind,window.used,window.minutes,window.reset,now,order])?;
    }
    if let Some((until, source)) = observation.cooldown {
        // Concurrent successful requests never clear a rejection. Multiple
        // rejections may extend cooldown but cannot shorten it.
        tx.execute("UPDATE oauth_accounts SET cooldown_until=?1,cooldown_source=?2 WHERE id=?3 AND (cooldown_until IS NULL OR cooldown_until<=?1)",params![until,source,account])?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn blocked_until(conn: &Connection, account: i64, now: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT cooldown_until FROM oauth_accounts WHERE id=?1",
            [account],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .filter(|at| *at > now))
}

pub fn summary(conn: &Connection, account: i64, now: i64) -> Result<Summary> {
    let (until, source): (Option<i64>, Option<String>) = conn.query_row(
        "SELECT cooldown_until,cooldown_source FROM oauth_accounts WHERE id=?1",
        [account],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let until = until.filter(|at| *at > now);
    let mut stmt=conn.prepare("SELECT kind,used_percent,window_minutes,reset_at,observed_at FROM oauth_quota_windows WHERE account_id=?1 ORDER BY kind")?;
    let windows = stmt
        .query_map([account], |row| {
            let used = row.get::<_, f64>(1)?;
            let reset = row.get::<_, Option<i64>>(3)?;
            let observed = row.get::<_, i64>(4)?;
            Ok(Window {
                kind: row.get(0)?,
                used_percent: used,
                remaining_percent: (100.0 - used).max(0.0),
                window_minutes: row.get(2)?,
                reset_at: reset,
                observed_at: observed,
                status: if reset.is_some_and(|at| at <= now) {
                    "reset_elapsed"
                } else if observed > now || now.saturating_sub(observed) >= 60 {
                    "stale"
                } else {
                    "current"
                }
                .into(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Summary {
        status: if until.is_some() {
            "cooldown"
        } else if windows.is_empty() {
            "unknown"
        } else if windows.iter().any(|window| window.status != "current") {
            "stale"
        } else {
            "observed"
        }
        .into(),
        windows,
        cooldown_until: until,
        cooldown_source: if until.is_some() { source } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('private-account','active',X'00',9999,1000)", []).unwrap();
        conn
    }
    fn rate(used: f64, reset: i64) -> Value {
        json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":used,"window_minutes":7200,"reset_at":reset}}})
    }
    fn put(conn: &mut Connection, order: i64, value: Value) {
        save(conn, 1, 0, order, event(&value, 1000), 1000).unwrap();
    }

    #[test]
    fn windows_keep_reported_duration_and_unknown_fields_without_inventing_resets() {
        let mut conn = database();
        put(&mut conn, 1, rate(100.5, 2000));
        let report = summary(&conn, 1, 1001).unwrap();
        assert_eq!(report.windows[0].window_minutes, Some(7200));
        assert_eq!(report.windows[0].used_percent, 100.5);
        assert_eq!(report.windows[0].remaining_percent, 0.0);
        assert_eq!(report.status, "observed");
        assert!(blocked_until(&conn, 1, 1001).unwrap().is_none());
        put(
            &mut conn,
            2,
            json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":0,"window_minutes":-1,"reset_at":-1}}}),
        );
        let window = summary(&conn, 1, 1001).unwrap().windows.remove(0);
        assert_eq!(window.used_percent, 0.0);
        assert_eq!(window.window_minutes, None);
        assert_eq!(window.reset_at, None);
        for value in [
            json!({}),
            json!({"used_percent":-1}),
            json!({"used_percent":"99"}),
        ] {
            assert!(event(
                &json!({"type":"codex.rate_limits","rate_limits":{"secondary":value}}),
                1000
            )
            .is_empty());
        }
        let mut other = rate(90.0, 3000);
        other["metered_limit_name"] = json!("gpt-test");
        assert!(event(&other, 1000).is_empty());
        other["metered_limit_name"] = Value::Null;
        other["limit_name"] = json!("gpt-test");
        assert!(event(&other, 1000).is_empty());
        assert!(sample("primary", Some(f64::NAN), None, None).is_none());
        assert!(sample("primary", Some(f64::INFINITY), None, None).is_none());
    }

    #[test]
    fn retry_after_dates_zero_and_body_reset_choose_actual_safe_retry_time() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "17".parse().unwrap());
        assert_eq!(
            self::headers(&headers, 429, 1000).cooldown,
            Some((1017, "retry_after"))
        );
        let body = json!({"error":{"type":"usage_limit_reached","resets_at":1010}});
        assert_eq!(
            http(&HeaderMap::new(), 429, Some(&body), 1000).cooldown,
            Some((1010, "upstream_reset"))
        );
        assert_eq!(
            http(&headers, 429, Some(&body), 1000).cooldown,
            Some((1017, "retry_after"))
        );
        let body = json!({"error":{"resets_at":1100}});
        assert_eq!(
            http(&headers, 429, Some(&body), 1000).cooldown,
            Some((1100, "upstream_reset"))
        );
        headers.insert("retry-after", "0".parse().unwrap());
        assert_eq!(
            self::headers(&headers, 429, 1000).cooldown,
            Some((1000, "retry_after"))
        );
        assert_eq!(
            retry_at("Wed, 30 Sep 2026 10:00:00 GMT", 1000),
            Some(1790762400)
        );
        for value in ["-1", "9223372036854775807", "not-a-date"] {
            headers.insert("retry-after", value.parse().unwrap());
            assert_eq!(
                self::headers(&headers, 429, 1000).cooldown,
                Some((1060, "local_backoff"))
            );
        }
        assert_eq!(self::headers(&headers, 503, 1000).cooldown, None);
    }

    #[test]
    fn wrapped_errors_read_only_known_headers_and_block_without_response_created() {
        let mut conn = database();
        put(
            &mut conn,
            1,
            json!({"type":"error","status":429,"headers":{"X-Codex-Primary-Used-Percent":"100","x-codex-primary-window-minutes":300,"x-codex-primary-reset-at":1200,"authorization":"private-secret"},"error":{"type":"usage_limit_reached","resets_at":1100}}),
        );
        let report = summary(&conn, 1, 1000).unwrap();
        assert_eq!(report.cooldown_until, Some(1200));
        assert_eq!(report.cooldown_source.as_deref(), Some("upstream_reset"));
        assert_eq!(report.windows[0].window_minutes, Some(300));
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("private-secret"));
        assert_eq!(event(&json!({"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded"}}}),1000).cooldown,Some((1060,"local_backoff")));
        assert_eq!(event(&json!({"type":"response.failed","error":null,"response":{"error":{"code":"rate_limit_exceeded"}}}),1000).cooldown,Some((1060,"local_backoff")));
        assert!(event(
            &json!({"type":"error","error":{"code":"invalid_request_error"}}),
            1000
        )
        .is_empty());
    }

    #[test]
    fn older_requests_cannot_overwrite_windows_and_success_never_clears_cooldown() {
        let mut conn = database();
        put(&mut conn, 10, rate(75.0, 2000));
        put(&mut conn, 9, rate(20.0, 2000));
        assert_eq!(
            summary(&conn, 1, 1000).unwrap().windows[0].used_percent,
            75.0
        );
        put(&mut conn, 10, rate(76.0, 2000));
        assert_eq!(
            summary(&conn, 1, 1000).unwrap().windows[0].used_percent,
            76.0
        );
        put(
            &mut conn,
            11,
            json!({"type":"error","error":{"type":"usage_limit_reached","resets_at":2000}}),
        );
        put(&mut conn, 12, rate(10.0, 3000));
        put(
            &mut conn,
            13,
            json!({"type":"error","error":{"type":"usage_limit_reached","resets_at":1500}}),
        );
        assert_eq!(blocked_until(&conn, 1, 1999).unwrap(), Some(2000));
        assert_eq!(blocked_until(&conn, 1, 2000).unwrap(), None);
        assert_eq!(summary(&conn, 1, 2000).unwrap().cooldown_source, None);
        let elapsed = summary(&conn, 1, 3000).unwrap();
        assert_eq!(elapsed.status, "stale");
        assert_eq!(elapsed.windows[0].status, "reset_elapsed");
        assert_eq!(elapsed.windows[0].used_percent, 10.0);
    }

    #[test]
    fn generation_and_account_fences_discard_stale_credentials_observations() {
        let mut conn = database();
        conn.execute("UPDATE oauth_accounts SET generation=1", [])
            .unwrap();
        put(&mut conn, 1, rate(80.0, 2000));
        assert!(summary(&conn, 1, 1000).unwrap().windows.is_empty());
        save(&mut conn, 1, 1, 1, event(&rate(80.0, 2000), 1000), 1000).unwrap();
        conn.execute("UPDATE oauth_accounts SET state='disabled'", [])
            .unwrap();
        save(&mut conn, 1, 1, 2, event(&rate(90.0, 2000), 1000), 1000).unwrap();
        assert_eq!(
            summary(&conn, 1, 1000).unwrap().windows[0].used_percent,
            80.0
        );
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('other','active',X'00',9999,1000)",[]).unwrap();
        assert_eq!(summary(&conn, 2, 1000).unwrap().status, "unknown");
    }

    #[test]
    fn quota_write_failure_rolls_back_partial_window_and_cooldown_update() {
        let mut conn = database();
        conn.execute_batch("CREATE TRIGGER reject_cooldown BEFORE UPDATE OF cooldown_until ON oauth_accounts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let value = json!({"type":"error","status":429,"headers":{"x-codex-primary-used-percent":"100","x-codex-primary-reset-at":"2000"}});
        assert!(save(&mut conn, 1, 0, 1, event(&value, 1000), 1000).is_err());
        assert!(summary(&conn, 1, 1000).unwrap().windows.is_empty());
        assert!(blocked_until(&conn, 1, 1000).unwrap().is_none());
    }
}
