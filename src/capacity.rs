//! Read-only calibration for the desktop capacity outlook. No quota mutations,
//! request bodies, client identities, new polls or persistent forecast state.
use crate::{quota, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

pub const MAX_ACCOUNTS: usize = 64;
const TEXT: &str = "api_surface IN ('responses','responses_compact','chat_completions') AND status NOT IN ('local_rejected','storage_error','upstream_rejected')";

#[derive(Serialize, Deserialize)]
pub struct Demand {
    pub last_hour: i64,
    pub previous_hour: i64,
    pub in_flight: i64,
    pub complete: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Cost {
    pub kind: String,
    /// Account/window-specific percentage points per recent average text request.
    pub per_request: Option<f64>,
    pub from_utc: Option<i64>,
    pub to_utc: i64,
    pub requests: i64,
}

#[derive(Serialize, Deserialize)]
pub struct Profile {
    pub state: String,
    pub short_window: Option<bool>,
    pub models: Vec<String>,
    pub costs: Vec<Cost>,
    /// Only a currently eligible, ledger-checked credit; still not a reservation.
    pub reset_eligible_until: Option<i64>,
}

pub(crate) fn annotate(conn: &Connection, value: &mut serde_json::Value, now: i64) -> Result<()> {
    let counts: (i64, i64, i64) = conn.query_row(&format!("SELECT COALESCE(SUM(at_utc>?1-3600),0),COALESCE(SUM(at_utc<=?1-3600),0),COALESCE(SUM(status IN ('accepted','sent')),0) FROM usage_events WHERE at_utc>?1-7200 AND at_utc<=?1 AND {TEXT}"),[now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let complete = value["quota_accounts"]
        .as_array()
        .is_some_and(|a| a.len() <= MAX_ACCOUNTS);
    value["capacity"] = serde_json::to_value(Demand {
        last_hour: counts.0,
        previous_hour: counts.1,
        in_flight: counts.2,
        complete,
    })?;
    if !complete {
        return Ok(());
    }
    let configured = value["oauth"]["status"] != "encryption_unavailable";
    if let Some(accounts) = value["quota_accounts"].as_array_mut() {
        for row in accounts {
            let Some(id) = row["id"].as_i64() else {
                continue;
            };
            let q = quota::summary(conn, id, now)?;
            let enabled = crate::account_preferences::effective(conn, id)?.enabled;
            let (account_state, expires, catalog): (String, i64, Option<i64>) = conn.query_row(
                "SELECT state,expires_at,catalog_updated_at FROM oauth_accounts WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let mut stmt = conn.prepare(
                "SELECT model FROM oauth_models WHERE account_id=?1 ORDER BY model LIMIT 257",
            )?;
            let models = stmt
                .query_map([id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let state = if !configured {
                "unconfigured"
            } else if !enabled || account_state == "disabled" {
                "disabled"
            } else if account_state != "active" {
                "reauth_required"
            } else if crate::health::paused(conn, id, now)?.is_some() {
                "backoff"
            } else if expires <= now + 60 {
                "refresh_due"
            } else if !catalog.is_some_and(|at| at <= now && now - at < 60)
                || models.is_empty()
                || models.len() > 256
            {
                "catalog_unavailable"
            } else if q.cooldown_until.is_some()
                && q.cooldown_source.as_deref() != Some("upstream_reset")
            {
                "cooldown"
            } else {
                "available"
            };
            let (short, _) = crate::routing::metadata(conn, id, now)?;
            let costs = q
                .windows
                .iter()
                .map(|w| cost(conn, id, w, now))
                .collect::<Result<Vec<_>>>()?;
            row["capacity"] = serde_json::to_value(Profile {
                state: state.into(),
                short_window: short,
                models: models.into_iter().take(256).collect(),
                costs,
                reset_eligible_until: crate::reset::preview(conn, id, now)?,
            })?;
        }
    }
    value["capabilities"]["capacity_outlook"] = 1.into();
    Ok(())
}

fn cost(conn: &Connection, id: i64, window: &quota::Window, now: i64) -> Result<Cost> {
    let mut result = Cost {
        kind: window.kind.clone(),
        per_request: None,
        from_utc: None,
        to_utc: window.observed_at,
        requests: 0,
    };
    if window.status != "current"
        || !crate::routing::fresh(window.observed_at, now)
        || window.reset_at.is_none_or(|at| at <= now)
        || window.window_minutes.is_none()
    {
        return Ok(result);
    }
    // Last accepted hourly observations, plus the current sample. Only contiguous,
    // monotonic observations in this exact cycle can calibrate this account.
    let mut stmt=conn.prepare("SELECT observed_at,used_percent,window_minutes,reset_at FROM oauth_quota_history WHERE account_id=?1 AND kind=?2 AND hour>=?3 AND observed_at<?4 ORDER BY hour DESC LIMIT 7")?;
    let samples = stmt
        .query_map(
            params![
                id,
                window.kind,
                (now - 6 * 3600).max(0) / 3600 * 3600,
                window.observed_at
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let (mut at, mut used) = (window.observed_at, window.used_percent);
    for (previous, usage, minutes, reset) in samples {
        if previous >= at
            || at - previous > 5400
            || previous < now - 6 * 3600
            || usage > used
            || minutes != window.window_minutes
            || reset != window.reset_at
        {
            break;
        }
        at = previous;
        used = usage;
    }
    let delta = window.used_percent - used;
    if window.observed_at - at < 900 || delta < 1.0 {
        return Ok(result);
    }
    let (requests,known):(i64,i64)=conn.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(input_tokens IS NOT NULL AND output_tokens IS NOT NULL),0) FROM usage_events WHERE account_id=?1 AND at_utc>?2 AND at_utc<=?3 AND {TEXT}"),params![id,at,window.observed_at],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let activation:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM quota_activation_attempts WHERE account_id=?1 AND attempted_at>?2 AND attempted_at<=?3)",params![id,at,window.observed_at],|r|r.get(0))?;
    result.from_utc = Some(at);
    result.requests = requests;
    // Unknown outcomes aren't zero-cost. Too many invalidate calibration, rather
    // than turning missing counters or no traffic into an infinite reserve.
    if requests >= 5 && known * 10 >= requests * 9 && !activation {
        result.per_request = Some(delta / requests as f64);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        let now = 1_800_000_000;
        conn.execute(
            "INSERT INTO users(id,name,created_at) VALUES(1,'synthetic',0)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO access_tokens(id,user_id,name,digest,created_at,expires_at) VALUES('synthetic',1,'synthetic',X'01',0,9999999999)",[]).unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,catalog_updated_at,created_at) VALUES('synthetic','active',X'00',?1,?2,0)",params![now+3600,now]).unwrap();
        conn.execute("INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'synthetic','Synthetic')",[]).unwrap();
        crate::routing::save(&conn, 1, 0, 0, now, Some(false), None).unwrap();
        conn.execute(
            "INSERT INTO oauth_quota_windows VALUES(1,'secondary',60,10080,?1,?2,1)",
            params![now + 604800, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO oauth_quota_history VALUES(1,'secondary',?1,?1,50,10080,?2,0)",
            params![now - 3600, now + 604800],
        )
        .unwrap();
        for n in 1..=10 {
            conn.execute("INSERT INTO usage_events(at_utc,user_id,token_id,account_id,api_surface,model,status,input_tokens,output_tokens) VALUES(?1,1,'synthetic',1,'responses','synthetic','completed',10,5)",[now-3600+n*300]).unwrap();
        }
        (conn, now)
    }
    #[test]
    fn calibration_is_account_scoped_numeric_read_only_and_survives_typed_doctor() {
        let (conn, now) = fixture();
        let q = quota::summary(&conn, 1, now).unwrap();
        let c = cost(&conn, 1, &q.windows[0], now).unwrap();
        assert_eq!(c.per_request, Some(1.0));
        assert_eq!(c.requests, 10);
        let before = conn.total_changes();
        let mut v = serde_json::json!({"quota_accounts":[{"id":1}]});
        annotate(&conn, &mut v, now).unwrap();
        assert_eq!(v["capacity"]["last_hour"], 10);
        assert_eq!(v["quota_accounts"][0]["capacity"]["state"], "available");
        assert_eq!(conn.total_changes(), before);
        assert!(!v.to_string().contains("encrypted_credentials"));
        conn.execute("UPDATE oauth_quota_windows SET reset_at=reset_at+1", [])
            .unwrap();
        assert!(cost(
            &conn,
            1,
            &quota::summary(&conn, 1, now).unwrap().windows[0],
            now
        )
        .unwrap()
        .per_request
        .is_none());
    }
    #[test]
    fn unknown_stale_reset_and_gap_data_do_not_invent_capacity() {
        let (conn, now) = fixture();
        conn.execute("UPDATE usage_events SET output_tokens=NULL", [])
            .unwrap();
        let q = quota::summary(&conn, 1, now).unwrap();
        assert!(cost(&conn, 1, &q.windows[0], now)
            .unwrap()
            .per_request
            .is_none());
        conn.execute("UPDATE usage_events SET output_tokens=5", [])
            .unwrap();
        conn.execute("UPDATE oauth_quota_history SET used_percent=70", [])
            .unwrap();
        assert!(cost(&conn, 1, &q.windows[0], now)
            .unwrap()
            .per_request
            .is_none());
        conn.execute("UPDATE oauth_quota_history SET used_percent=50,observed_at=observed_at-7200,hour=hour-7200",[]).unwrap();
        assert!(cost(&conn, 1, &q.windows[0], now)
            .unwrap()
            .per_request
            .is_none());
        assert!(cost(&conn, 1, &q.windows[0], now + 60)
            .unwrap()
            .per_request
            .is_none());
    }

    #[test]
    fn late_deactivation_and_unconfigured_runtime_do_not_supply_capacity() {
        let (conn, now) = fixture();
        for state in ["disabled", "reauth_required"] {
            conn.execute("UPDATE oauth_accounts SET state=?1", [state])
                .unwrap();
            let mut value = serde_json::json!({"quota_accounts":[{"id":1}]});
            annotate(&conn, &mut value, now).unwrap();
            assert_eq!(value["quota_accounts"][0]["capacity"]["state"], state);
        }
        conn.execute("UPDATE oauth_accounts SET state='active'", [])
            .unwrap();
        let mut value = serde_json::json!({"oauth":{"status":"encryption_unavailable"},"quota_accounts":[{"id":1}]});
        annotate(&conn, &mut value, now).unwrap();
        assert_eq!(
            value["quota_accounts"][0]["capacity"]["state"],
            "unconfigured"
        );
    }
}
