//! Fenced operational observations.
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Catalog,
    Refresh,
    Responses,
}
impl Scope {
    fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Refresh => "refresh",
            Self::Responses => "responses",
        }
    }
}
pub(crate) fn next(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("UPDATE upstream_operation_clock SET sequence=sequence+1 WHERE id=1 AND sequence<9223372036854775807 RETURNING sequence",[],|row|row.get(0))?)
}
pub(crate) fn paused(conn: &Connection, id: i64, now: i64) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT MAX(h.retry_at) FROM oauth_health h JOIN oauth_accounts a ON a.id=h.account_id AND a.generation=h.generation WHERE a.id=?1 AND h.retry_at>?2",params![id,now],|row|row.get(0))?)
}
fn valid(conn: &Connection, id: i64, generation: i64) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')",params![id,generation],|row|row.get(0))?)
}
pub(crate) fn failure(
    conn: &Connection,
    account: (i64, i64),
    scope: Scope,
    order: i64,
    reason: &'static str,
    retry: Option<i64>,
    now: i64,
) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    failure_in_transaction(&tx, account, scope, order, reason, retry, now)?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn failure_in_transaction(
    tx: &Connection,
    account: (i64, i64),
    scope: Scope,
    order: i64,
    reason: &'static str,
    retry: Option<i64>,
    now: i64,
) -> Result<()> {
    let (id, generation) = account;
    debug_assert!(!tx.is_autocommit());
    if !valid(tx, id, generation)? {
        return Ok(());
    }
    let old=tx.query_row("SELECT generation,failures,retry_at,last_order FROM oauth_health WHERE account_id=?1 AND scope=?2",params![id,scope.name()],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,u32>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?))).optional()?;
    let old = old.filter(|old| old.0 == generation);
    if old.is_some_and(|old| old.3 >= order) {
        return Ok(());
    }
    let failures = old.map_or(1, |old| old.1.saturating_add(1).min(32));
    let delay = (5_i64.saturating_mul(1_i64 << failures.saturating_sub(1).min(6))).min(300);
    let until = now
        .saturating_add(delay)
        .max(retry.unwrap_or(now))
        .max(old.map_or(now, |old| old.2));
    tx.execute("INSERT INTO oauth_health VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id,scope) DO UPDATE SET generation=excluded.generation,failures=excluded.failures,retry_at=excluded.retry_at,reason=excluded.reason,last_order=excluded.last_order",params![id,scope.name(),generation,failures,until,reason,order])?;
    Ok(())
}
pub(crate) fn success(
    conn: &Connection,
    id: i64,
    generation: i64,
    scope: Scope,
    order: i64,
) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    if !valid(&tx, id, generation)? {
        return Ok(());
    }
    // Retain the observation order even after recovery: a delayed older failure
    // must not reintroduce a pause after a newer successful operation.
    let changed=tx.execute("INSERT INTO oauth_health VALUES(?1,?2,?3,0,0,'healthy',?4) ON CONFLICT(account_id,scope) DO UPDATE SET generation=excluded.generation,failures=0,retry_at=0,reason='healthy',last_order=excluded.last_order WHERE oauth_health.generation<>excluded.generation OR oauth_health.last_order<=excluded.last_order",params![id,scope.name(),generation,order])?;
    if changed == 0 {
        return Ok(());
    }
    if !matches!(scope, Scope::Refresh) {
        tx.execute("DELETE FROM oauth_auth_rejections WHERE account_id=?1 AND scope=?2 AND generation<=?3 AND last_order<=?4",params![id,scope.name(),generation,order])?;
    }
    tx.commit()?;
    Ok(())
}
pub(crate) fn unauthorized(
    conn: &Connection,
    id: i64,
    generation: i64,
    scope: Scope,
    order: i64,
    now: i64,
) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    if !valid(&tx, id, generation)? {
        return Ok(());
    }
    let newer=tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_health WHERE account_id=?1 AND scope=?2 AND generation=?3 AND last_order>?4)",params![id,scope.name(),generation,order],|row|row.get::<_,bool>(0))?;
    if newer {
        return Ok(());
    }
    let old=tx.query_row("SELECT generation,last_order FROM oauth_auth_rejections WHERE account_id=?1 AND scope=?2",params![id,scope.name()],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))).optional()?;
    if old.is_some_and(|old| old.1 > order) {
        return Ok(());
    }
    if old.is_some_and(|old| old.0 < generation) {
        tx.execute("UPDATE oauth_accounts SET state='reauth_required',refresh_owner=NULL,refresh_until=NULL WHERE id=?1 AND generation=?2",params![id,generation])?;
    } else {
        // Expiry schedules a single leased refresh before a separate new operation.
        tx.execute(
            "UPDATE oauth_accounts SET expires_at=MIN(expires_at,?1) WHERE id=?2 AND generation=?3",
            params![now, id, generation],
        )?;
    }
    tx.execute("INSERT INTO oauth_auth_rejections VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,scope) DO UPDATE SET generation=excluded.generation,last_order=excluded.last_order",params![id,scope.name(),generation,order])?;
    tx.commit()?;
    Ok(())
}
#[derive(Clone, Copy)]
pub(crate) enum Rejection {
    Authentication,
    Temporary,
}
pub(crate) fn rejection(event: &Value) -> Option<Rejection> {
    if !matches!(event["type"].as_str(), Some("error" | "response.failed")) {
        return None;
    }
    let error = event
        .get("error")
        .filter(|value| !value.is_null())
        .or_else(|| event.pointer("/response/error"));
    let status = event
        .get("status")
        .filter(|value| !value.is_null())
        .or_else(|| event.get("status_code"))
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
    let code = error.and_then(|error| {
        error
            .get("code")
            .and_then(Value::as_str)
            .or_else(|| error.get("type").and_then(Value::as_str))
    });
    if status == Some(401)
        || matches!(
            code,
            Some("authentication_error" | "invalid_token" | "token_expired")
        )
    {
        return Some(Rejection::Authentication);
    }
    if status.is_some_and(|status| status == 408 || (500..=599).contains(&status))
        || matches!(
            code,
            Some(
                "server_error"
                    | "internal_server_error"
                    | "service_unavailable"
                    | "overloaded_error"
            )
        )
    {
        return Some(Rejection::Temporary);
    }
    None
}

pub(crate) fn event_retry(event: &Value, now: i64) -> Option<i64> {
    event
        .get("headers")?
        .as_object()?
        .iter()
        .find_map(|(name, value)| {
            if name.eq_ignore_ascii_case("retry-after") {
                crate::quota::retry_at(value.as_str()?, now)
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('fixture','active',X'00',9999,0)",[]).unwrap();
        conn
    }
    #[test]
    fn ordered_backoff_is_durable_bounded_and_scoped_and_old_success_cannot_clear_it() {
        let conn = db();
        failure(
            &conn,
            (1, 0),
            Scope::Responses,
            2,
            "upstream_5xx",
            None,
            1000,
        )
        .unwrap();
        assert_eq!(paused(&conn, 1, 1000).unwrap(), Some(1005));
        success(&conn, 1, 0, Scope::Responses, 1).unwrap();
        success(&conn, 1, 0, Scope::Catalog, 3).unwrap();
        assert_eq!(paused(&conn, 1, 1000).unwrap(), Some(1005));
        failure(&conn, (1, 0), Scope::Responses, 2, "transport", None, 1001).unwrap();
        assert_eq!(paused(&conn, 1, 1000).unwrap(), Some(1005));
        for order in 3..15 {
            failure(
                &conn,
                (1, 0),
                Scope::Responses,
                order,
                "transport",
                None,
                1000,
            )
            .unwrap();
        }
        assert_eq!(paused(&conn, 1, 1000).unwrap(), Some(1300));
        assert_eq!(paused(&conn, 1, 1300).unwrap(), None);
        failure(
            &conn,
            (1, 0),
            Scope::Responses,
            15,
            "upstream_5xx",
            Some(1500),
            1000,
        )
        .unwrap();
        assert_eq!(paused(&conn, 1, 1000).unwrap(), Some(1500));
        success(&conn, 1, 0, Scope::Responses, 16).unwrap();
        assert_eq!(paused(&conn, 1, 1000).unwrap(), None);
        failure(&conn, (1, 0), Scope::Responses, 15, "transport", None, 1001).unwrap();
        unauthorized(&conn, 1, 0, Scope::Responses, 15, 1001).unwrap();
        assert_eq!(paused(&conn, 1, 1001).unwrap(), None);
        assert_eq!(
            conn.query_row("SELECT expires_at FROM oauth_accounts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            9999
        );
    }
    #[test]
    fn unauthorized_coalesces_expiry_and_repeated_rejection_after_refresh_requires_reauth() {
        let conn = db();
        unauthorized(&conn, 1, 0, Scope::Responses, 1, 1000).unwrap();
        unauthorized(&conn, 1, 0, Scope::Responses, 2, 1000).unwrap();
        assert_eq!(
            conn.query_row("SELECT state FROM oauth_accounts", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "active"
        );
        conn.execute("UPDATE oauth_accounts SET generation=1,expires_at=9999", [])
            .unwrap();
        unauthorized(&conn, 1, 0, Scope::Responses, 3, 1001).unwrap();
        success(&conn, 1, 1, Scope::Catalog, 4).unwrap();
        unauthorized(&conn, 1, 1, Scope::Responses, 5, 1001).unwrap();
        assert_eq!(
            conn.query_row("SELECT state FROM oauth_accounts", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "reauth_required"
        );
        failure(&conn, (1, 0), Scope::Responses, 6, "transport", None, 1001).unwrap();
        assert_eq!(paused(&conn, 1, 1001).unwrap(), None);
    }
    #[test]
    fn new_credentials_ignore_old_backoff_and_success_recovers_auth_only_in_its_scope() {
        let conn = db();
        unauthorized(&conn, 1, 0, Scope::Responses, 1, 1000).unwrap();
        failure(&conn, (1, 0), Scope::Catalog, 2, "catalog", None, 1000).unwrap();
        conn.execute("UPDATE oauth_accounts SET generation=1,expires_at=9999", [])
            .unwrap();
        assert_eq!(paused(&conn, 1, 1000).unwrap(), None);
        success(&conn, 1, 1, Scope::Responses, 3).unwrap();
        unauthorized(&conn, 1, 1, Scope::Responses, 4, 1000).unwrap();
        assert_eq!(
            conn.query_row("SELECT state FROM oauth_accounts", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "active"
        );
        assert!(next(&conn).unwrap() < next(&conn).unwrap());
    }
}
