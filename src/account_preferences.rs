//! Operator-owned availability only. Routing economics cannot be overridden.
use crate::Result;
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Content-free correlation only: not an account selector or authorization proof.
/// Domain/user scoping keeps inference metadata opaque outside this user's doctor.
pub(crate) fn observation_key(key: &[u8], user: i64, account: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(b"exetrouter/account-observation/v1\0");
    mac.update(&user.to_be_bytes());
    mac.update(&account.to_be_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Decode retired wire fields only to explicitly reject old-client mutations.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_at: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_at_short: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_at_weekly: Option<serde_json::Value>,
}
impl RoutingArgs {
    pub fn validate(&self) -> Result<()> {
        if [
            &self.priority,
            &self.switch_at,
            &self.switch_at_short,
            &self.switch_at_weekly,
        ]
        .iter()
        .any(|v| v.is_some())
        {
            return Err("Account routing is automatic; manual priorities and switching rules are no longer supported".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preference {
    pub enabled: bool,
    pub locked: bool,
}
impl Default for Preference {
    fn default() -> Self {
        Self {
            enabled: true,
            locked: false,
        }
    }
}
pub fn effective(conn: &Connection, account: i64) -> Result<Preference> {
    Ok(conn.query_row("SELECT COALESCE(p.enabled,1),COALESCE(p.locked,0) FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.id=?1",[account],|r|Ok(Preference{enabled:r.get(0)?,locked:r.get(1)?}))?)
}
pub fn set_policy(
    conn: &mut Connection,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
    locked: Option<bool>,
) -> Result<Preference> {
    set_policy_rules(
        conn,
        account,
        enabled,
        priority,
        locked,
        &RoutingArgs::default(),
    )
}
pub fn set_policy_rules(
    conn: &mut Connection,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
    locked: Option<bool>,
    patch: &RoutingArgs,
) -> Result<Preference> {
    patch.validate()?;
    if priority.is_some() {
        return Err("Account routing is automatic; manual priority is no longer supported".into());
    }
    let tx = conn.transaction()?;
    let previous = effective(&tx, account)?;
    tx.execute("INSERT INTO account_policy(account_id,enabled,locked) VALUES(?1,?2,?3) ON CONFLICT(account_id) DO UPDATE SET enabled=excluded.enabled,locked=excluded.locked",params![account,enabled.unwrap_or(previous.enabled),locked.unwrap_or(previous.locked)])?;
    let result = effective(&tx, account)?;
    tx.commit()?;
    Ok(result)
}
pub fn annotate(
    conn: &Connection,
    key: &[u8],
    user: i64,
    value: &mut serde_json::Value,
) -> Result<()> {
    annotate_at(conn, key, user, value, chrono::Utc::now().timestamp())
}

fn annotate_at(
    conn: &Connection,
    key: &[u8],
    user: i64,
    value: &mut serde_json::Value,
    now: i64,
) -> Result<()> {
    value["capabilities"]["automatic_account_routing"] = serde_json::json!(1);
    value["capabilities"]["automatic_reset_credits"] = serde_json::json!(1);
    value["capabilities"]["global_account_clients"] = serde_json::json!(1);
    if let Some(rows) = value["quota_accounts"].as_array_mut() {
        let mut candidates = Vec::new();
        // Scan activity once per snapshot, not once per account.
        let mut clients = crate::usage::clients_by_account(conn, key, user, now)?;
        for row in rows.iter_mut() {
            if let Some(id) = row["id"].as_i64() {
                row["observation_key"] = observation_key(key, user, id).into();
                let preference = effective(conn, id)?;
                let quota = crate::quota::summary(conn, id, now)?;
                row["preference"] = serde_json::to_value(&preference)?;
                let routing = crate::routing::decision(conn, id, now)?;
                let reached = routing.reached(&quota);
                let available = preference.enabled && !reached && quota.cooldown_until.is_none()
                    && conn.query_row("SELECT a.state='active' AND a.expires_at>?2+60 AND a.catalog_updated_at IS NOT NULL AND a.catalog_updated_at<=?2 AND a.catalog_updated_at>?2-60 AND EXISTS(SELECT 1 FROM oauth_models m WHERE m.account_id=a.id) AND NOT EXISTS(SELECT 1 FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.retry_at>?2) FROM oauth_accounts a WHERE a.id=?1",params![id,now],|r|r.get::<_,bool>(0))?;
                if available {
                    candidates.push(crate::pool::Candidate {
                        id,
                        quota: if quota.status == "observed" {
                            quota
                                .windows
                                .iter()
                                .map(|w| w.used_percent)
                                .reduce(f64::max)
                        } else {
                            None
                        },
                        routing: routing.clone(),
                    });
                }
                row["threshold_reached"] = serde_json::json!(reached);
                row["routing"] = serde_json::to_value(routing)?;
                row["routing"]["scope"] = "server".into();
                row["routing"]["best"] = false.into();
                row["routing"]["observed_at"] = now.into();
                row["routing"]["valid_until"] = quota
                    .windows
                    .iter()
                    .fold(now.saturating_add(60), |until, window| {
                        until
                            .min(window.observed_at.saturating_add(60))
                            .min(window.reset_at.unwrap_or(i64::MAX))
                    })
                    .into();
                row["routing"]["target_share_percent"] = serde_json::Value::Null;
                row["clients"] = clients
                    .remove(&id)
                    .unwrap_or_else(|| crate::usage::empty_clients(now));
                let status:Option<String>=conn.query_row("SELECT status FROM automatic_reset_attempts WHERE account_id=?1 ORDER BY attempted_at DESC LIMIT 1",[id],|r|r.get(0)).ok();
                row["automatic_reset"] =
                    serde_json::json!({"status":status.unwrap_or_else(||"idle".into())});
            }
        }
        let recommendations = crate::pool::recommendations(&candidates);
        for row in rows {
            if let Some(recommendation) = recommendations
                .iter()
                .find(|r| row["id"].as_i64() == Some(r.id))
            {
                row["routing"]["best"] = recommendation.best.into();
                row["routing"]["target_share_percent"] = serde_json::json!(recommendation.share);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn best_and_shares_are_global_and_follow_fresh_capacity_and_operator_availability() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::store::init(&conn).unwrap();
        let user = crate::create_user(&conn, "synthetic").unwrap();
        let other = crate::create_user(&conn, "other").unwrap();
        let now = chrono::Utc::now().timestamp();
        for (id, used, days) in [(1, 40.0, 6), (2, 20.0, 4)] {
            conn.execute("INSERT INTO oauth_accounts(id,account_id,state,encrypted_credentials,expires_at,catalog_updated_at,created_at) VALUES(?1,?2,'active',X'00',?3,?4,0)",params![id,format!("synthetic-{id}"),now+3600,now]).unwrap();
            conn.execute("INSERT INTO oauth_models(account_id,model,display_name) VALUES(?1,'synthetic','Synthetic')",[id]).unwrap();
            crate::routing::save(&conn, id, 0, 0, now, Some(true), None).unwrap();
            conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(?1,'secondary',?2,10080,?3,?4,0)",params![id,used,now+days*86400,now]).unwrap();
        }
        conn.execute(
            "INSERT INTO account_preferences(user_id,account_id,enabled) VALUES(?1,1,0),(?2,2,0)",
            params![user, other],
        )
        .unwrap();
        let snapshot = |conn: &Connection| {
            let mut value = serde_json::json!({"quota_accounts":[{"id":1},{"id":2}]});
            annotate_at(conn, &[1; 32], user, &mut value, now).unwrap();
            let mut other_value = serde_json::json!({"quota_accounts":[{"id":1},{"id":2}]});
            annotate_at(conn, &[1; 32], other, &mut other_value, now).unwrap();
            for i in 0..2 {
                assert_ne!(
                    value["quota_accounts"][i]["observation_key"],
                    other_value["quota_accounts"][i]["observation_key"]
                );
                assert_eq!(value["quota_accounts"][i]["routing"]["scope"], "server");
                assert_eq!(
                    value["quota_accounts"][i]["routing"],
                    other_value["quota_accounts"][i]["routing"]
                );
                assert_eq!(
                    value["quota_accounts"][i]["clients"]["count"],
                    other_value["quota_accounts"][i]["clients"]["count"]
                );
            }
            value
        };
        let value = snapshot(&conn);
        assert_eq!(value["quota_accounts"][0]["routing"]["best"], false);
        assert_eq!(value["quota_accounts"][1]["routing"]["best"], true);
        let share = value["quota_accounts"][1]["routing"]["target_share_percent"]
            .as_f64()
            .unwrap();
        assert!((share - 200.0 / 3.0).abs() < 0.01);
        set_policy(&mut conn, 2, Some(false), None, None).unwrap();
        let value = snapshot(&conn);
        assert_eq!(value["quota_accounts"][0]["routing"]["best"], true);
        assert_eq!(
            value["quota_accounts"][0]["routing"]["target_share_percent"],
            100.0
        );
        assert_eq!(value["quota_accounts"][1]["routing"]["best"], false);
        set_policy(&mut conn, 2, Some(true), None, None).unwrap();
        for restriction in [
            format!("cooldown_until={}", now + 60),
            "state='reauth_required'".into(),
            format!("expires_at={now}"),
            "catalog_updated_at=NULL".into(),
            format!("catalog_updated_at={}", now - 60),
        ] {
            conn.execute(
                &format!("UPDATE oauth_accounts SET {restriction} WHERE id=2"),
                [],
            )
            .unwrap();
            assert_eq!(
                snapshot(&conn)["quota_accounts"][0]["routing"]["best"],
                true
            );
            conn.execute("UPDATE oauth_accounts SET state='active',cooldown_until=NULL,expires_at=?1,catalog_updated_at=?2 WHERE id=2",params![now+3600,now]).unwrap();
        }
        conn.execute("INSERT INTO oauth_health(account_id,scope,generation,failures,retry_at,reason,last_order) VALUES(2,'responses',0,1,?1,'unavailable',0)",[now+60]).unwrap();
        assert_eq!(
            snapshot(&conn)["quota_accounts"][0]["routing"]["best"],
            true
        );
        conn.execute("DELETE FROM oauth_health WHERE account_id=2", [])
            .unwrap();
        conn.execute(
            "UPDATE oauth_quota_windows SET used_percent=100 WHERE account_id=2",
            [],
        )
        .unwrap();
        assert_eq!(
            snapshot(&conn)["quota_accounts"][0]["routing"]["best"],
            true
        );
        conn.execute("UPDATE oauth_quota_windows SET observed_at=?1", [now - 60])
            .unwrap();
        assert!(snapshot(&conn)["quota_accounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["routing"]["best"] == false));
    }
    #[test]
    fn legacy_personal_preferences_are_ignored_and_only_operator_policy_applies() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::store::init(&conn).unwrap();
        crate::create_user(&conn, "synthetic").unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('synthetic','active',X'00',9999,0)",[]).unwrap();
        conn.execute(
            "INSERT INTO account_policy(account_id,priority,switch_at) VALUES(1,255,100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO account_preferences(user_id,account_id,enabled,priority) VALUES(1,1,0,10)",
            [],
        )
        .unwrap();
        assert!(effective(&conn, 1).unwrap().enabled);
        let legacy: RoutingArgs =
            serde_json::from_value(serde_json::json!({"switch_at":20})).unwrap();
        assert!(set_policy_rules(&mut conn, 1, None, None, None, &legacy).is_err());
        assert!(set_policy(&mut conn, 1, None, Some(10), None).is_err());
        assert!(
            set_policy(&mut conn, 1, Some(true), None, Some(true))
                .unwrap()
                .locked
        );
        assert!(effective(&conn, 1).unwrap().enabled);
        set_policy(&mut conn, 1, None, None, Some(false)).unwrap();
        assert!(effective(&conn, 1).unwrap().enabled);
        set_policy(&mut conn, 1, Some(false), None, None).unwrap();
        assert!(!effective(&conn, 1).unwrap().enabled);
    }
}
