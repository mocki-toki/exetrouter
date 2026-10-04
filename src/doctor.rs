//! Read-only configuration diagnostics from local metadata. Account email labels
//! are added by the service from encrypted credentials, without upstream requests.
use crate::{server::LimitSnapshot, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ServerReport {
    #[serde(default)]
    pub capabilities: serde_json::Value,
    pub schema_version: u8,
    pub version: String,
    pub checked_at: i64,
    pub configuration_status: String,
    pub oauth: OAuthReport,
    pub catalog: CatalogReport,
    pub limits: LimitsReport,
    #[serde(default)]
    pub quota: Option<crate::quota::Summary>,
    #[serde(default)]
    pub pool: Option<PoolReport>,
    #[serde(default)]
    pub quota_accounts: Vec<AccountQuota>,
    pub upstream_connectivity: String,
    pub inference: String,
}

#[derive(Serialize, Deserialize)]
pub struct AccountQuota {
    #[serde(default)]
    pub id: i64,
    pub label: String,
    #[serde(default)]
    pub reset_credits: Option<crate::reset::Credits>,
    #[serde(default)]
    pub refresh_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_activation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preference: Option<crate::account_preferences::Preference>,
    #[serde(default)]
    pub threshold_reached: bool,
    pub quota: crate::quota::Summary,
}

#[derive(Serialize, Deserialize)]
pub struct OAuthReport {
    pub status: String,
    pub active_accounts: usize,
    pub disabled_accounts: usize,
    pub reauth_required_accounts: usize,
    pub access_expires_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct CatalogReport {
    pub status: String,
    pub models: usize,
    pub updated_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct LimitsReport {
    pub generations: LimitSnapshot,
    pub websockets: LimitSnapshot,
}

#[derive(Serialize, Deserialize, Default)]
pub struct PoolReport {
    pub configured_accounts: usize,
    pub cooldown_accounts: usize,
    pub catalog_unavailable_accounts: usize,
    pub fresh_quota_accounts: usize,
    pub stale_quota_accounts: usize,
    pub unknown_quota_accounts: usize,
    pub earliest_cooldown_until: Option<i64>,
    #[serde(default)]
    pub backoff_accounts: usize,
    #[serde(default)]
    pub earliest_backoff_until: Option<i64>,
}

pub(crate) fn snapshot(
    conn: &Connection,
    configured: bool,
    limits: LimitsReport,
    now: i64,
) -> Result<ServerReport> {
    let (active, disabled, reauth, expires, updated): (
        usize,
        usize,
        usize,
        Option<i64>,
        Option<i64>,
    ) = conn.query_row(
        "SELECT COALESCE(SUM(state='active'),0), COALESCE(SUM(state='disabled'),0),
                COALESCE(SUM(state='reauth_required'),0),
                MIN(CASE WHEN state='active' THEN expires_at END),
                MIN(CASE WHEN state='active' THEN catalog_updated_at END)
         FROM oauth_accounts",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let models = conn.query_row(
        "SELECT COUNT(DISTINCT model) FROM oauth_models m JOIN oauth_accounts a ON a.id=m.account_id WHERE a.state='active'",
        [], |row| row.get::<_, usize>(0),
    )?;
    let mut pool = PoolReport::default();
    let mut quota_accounts = Vec::new();
    let mut current_catalogs = 0;
    let mut valid_access = 0;
    let mut stmt=conn.prepare("SELECT a.id,a.expires_at,a.catalog_updated_at,(SELECT COUNT(*) FROM oauth_models m WHERE m.account_id=a.id) FROM oauth_accounts a WHERE a.state='active' ORDER BY a.id")?;
    let accounts = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, usize>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, expires, updated, models) in accounts {
        let quota = crate::quota::summary(conn, id, now)?;
        let health = crate::health::paused(conn, id, now)?;
        if let Some(until) = health {
            pool.backoff_accounts += 1;
            pool.earliest_backoff_until = Some(
                pool.earliest_backoff_until
                    .map_or(until, |old| old.min(until)),
            );
        }
        let access = expires > now.saturating_add(60);
        let catalog =
            updated.is_some_and(|at| at <= now && now.saturating_sub(at) < 60) && models > 0;
        if access {
            valid_access += 1;
        }
        if catalog {
            current_catalogs += 1;
        } else {
            pool.catalog_unavailable_accounts += 1;
        }
        if let Some(until) = quota.cooldown_until {
            pool.cooldown_accounts += 1;
            pool.earliest_cooldown_until = Some(
                pool.earliest_cooldown_until
                    .map_or(until, |old| old.min(until)),
            );
        } else if health.is_none() && access && catalog && configured {
            pool.configured_accounts += 1;
        }
        if quota.windows.is_empty() {
            pool.unknown_quota_accounts += 1;
        } else if quota
            .windows
            .iter()
            .all(|window| window.status == "current")
        {
            pool.fresh_quota_accounts += 1;
        } else {
            pool.stale_quota_accounts += 1;
        }
        quota_accounts.push(AccountQuota {
            id,
            label: "Email unavailable".into(),
            preference: None,
            threshold_reached: false,
            reset_credits: None,
            refresh_error: None,
            weekly_activation: conn.query_row("SELECT CASE WHEN status='pending' AND attempted_at<=?3 THEN 'unknown' ELSE status END FROM quota_activation_attempts WHERE account_id=?1 AND attempted_at>?2 ORDER BY attempted_at DESC,id DESC LIMIT 1", rusqlite::params![id,now-604800,now-60], |row| row.get(0)).optional()?,
            quota,
        });
    }
    let oauth_status = if !configured {
        "encryption_unavailable"
    } else if active == 0 {
        if reauth > 0 {
            "reauth_required"
        } else {
            "no_active_account"
        }
    } else if valid_access == 0 {
        "refresh_due"
    } else {
        "configured"
    };
    let catalog_status = if active == 0 || !configured {
        "unavailable"
    } else if current_catalogs > 0 {
        "current"
    } else if updated.is_none() {
        "not_loaded"
    } else if updated.is_some_and(|at| at > now || now.saturating_sub(at) >= 60) {
        "stale"
    } else if models == 0 {
        "empty"
    } else {
        "current"
    };
    let quota = if active == 1 {
        let id = conn.query_row(
            "SELECT id FROM oauth_accounts WHERE state='active'",
            [],
            |row| row.get(0),
        )?;
        Some(crate::quota::summary(conn, id, now)?)
    } else {
        None
    };
    let ready = pool.configured_accounts > 0;
    Ok(ServerReport {
        capabilities: serde_json::Value::Null,
        schema_version: 1,
        version: env!("CARGO_PKG_VERSION").into(),
        checked_at: now,
        configuration_status: if ready {
            "configured"
        } else {
            "needs_attention"
        }
        .into(),
        oauth: OAuthReport {
            status: oauth_status.into(),
            active_accounts: active,
            disabled_accounts: disabled,
            reauth_required_accounts: reauth,
            access_expires_at: expires,
        },
        catalog: CatalogReport {
            status: catalog_status.into(),
            models,
            updated_at: updated,
        },
        limits,
        quota,
        pool: Some(pool),
        quota_accounts,
        upstream_connectivity: "not_checked".into(),
        inference: "not_checked".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(conn: &Connection, now: i64) -> ServerReport {
        snapshot(
            conn,
            true,
            LimitsReport {
                generations: LimitSnapshot {
                    global_limit: 64,
                    per_user_limit: 8,
                    active_for_user: 0,
                },
                websockets: LimitSnapshot {
                    global_limit: 64,
                    per_user_limit: 8,
                    active_for_user: 0,
                },
            },
            now,
        )
        .unwrap()
    }

    #[test]
    fn backoff_metadata_requires_no_credentials_and_respects_generation_and_expiry() {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute_batch("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,catalog_updated_at,created_at) VALUES('private-account','active',X'00',2000,1000,1000); INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'gpt-test','Test'); INSERT INTO oauth_health VALUES(1,'responses',0,1,1100,'transport',1);").unwrap();
        let paused = report(&conn, 1050);
        assert_eq!(paused.configuration_status, "needs_attention");
        assert_eq!(paused.pool.as_ref().unwrap().backoff_accounts, 1);
        assert_eq!(
            paused.pool.as_ref().unwrap().earliest_backoff_until,
            Some(1100)
        );
        assert!(!serde_json::to_string(&paused)
            .unwrap()
            .contains("private-account"));
        assert_eq!(report(&conn, 1100).pool.unwrap().backoff_accounts, 0);
        conn.execute("UPDATE oauth_accounts SET generation=1", [])
            .unwrap();
        assert_eq!(report(&conn, 1050).configuration_status, "configured");
    }
    #[test]
    fn metadata_reports_expiry_staleness_and_partial_pool_without_reading_credentials() {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        assert_eq!(report(&conn, 1000).oauth.status, "no_active_account");
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,catalog_updated_at,created_at) VALUES('private-account','active',X'00',2000,1000,1000)",[]).unwrap();
        conn.execute(
            "INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'gpt-test','Test')",
            [],
        )
        .unwrap();
        let current = report(&conn, 1059);
        assert_eq!(current.configuration_status, "configured");
        assert_eq!(current.upstream_connectivity, "not_checked");
        assert_eq!(current.inference, "not_checked");
        assert!(!serde_json::to_string(&current)
            .unwrap()
            .contains("private-account"));
        assert_eq!(report(&conn, 1060).catalog.status, "stale");
        assert_eq!(report(&conn, 999).catalog.status, "stale");
        assert_eq!(report(&conn, 1940).oauth.status, "refresh_due");
        conn.execute("UPDATE oauth_accounts SET state='reauth_required'", [])
            .unwrap();
        assert_eq!(report(&conn, 1059).oauth.status, "reauth_required");
        conn.execute("UPDATE oauth_accounts SET state='disabled'", [])
            .unwrap();
        assert_eq!(report(&conn, 1059).catalog.models, 0);
        conn.execute("UPDATE oauth_accounts SET state='active'", [])
            .unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('other-private-account','active',X'00',2000,1000)",[]).unwrap();
        let partial = report(&conn, 1059);
        assert_eq!(partial.oauth.status, "configured");
        assert_eq!(partial.configuration_status, "configured");
        assert_eq!(partial.pool.unwrap().catalog_unavailable_accounts, 1);
        assert!(partial.quota.is_none());
        conn.execute(
            "UPDATE oauth_accounts SET cooldown_until=2000 WHERE id=1",
            [],
        )
        .unwrap();
        let unavailable = report(&conn, 1059);
        assert_eq!(unavailable.configuration_status, "needs_attention");
        assert_eq!(unavailable.pool.unwrap().cooldown_accounts, 1);
    }
}
