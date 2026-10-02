//! Durable, user-scoped routing for opaque output; no context bodies are stored.
use crate::Result;
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;

pub(crate) const TTL: i64 = 24 * 60 * 60;
const ITEMS: usize = 128;
const PER_USER: i64 = 4096;
const TOTAL: i64 = 65536;
pub(crate) type Digest = [u8; 32];

/// Hash only; the upstream routing token remains in transient header memory.
pub(crate) fn turn_digest(value: &str, key: &[u8], user: i64) -> Result<Digest> {
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| (33..=126).contains(&b)) {
        return Err("invalid turn routing state".into());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(b"exetrouter/turn-affinity/v1\0");
    mac.update(&user.to_be_bytes());
    mac.update(value.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

pub(crate) fn digests(value: &Value, key: &[u8], user: i64) -> Result<Vec<Digest>> {
    fn visit(value: &Value, key: &[u8], user: i64, found: &mut BTreeSet<Digest>) -> Result<()> {
        match value {
            Value::Object(object) => {
                let content = object
                    .get("encrypted_content")
                    .filter(|value| !value.is_null());
                if matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("compaction" | "compaction_summary")
                ) && content.is_none()
                {
                    return Err("compaction requires encrypted content".into());
                }
                if let Some(content) = content {
                    let content = content
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .ok_or("invalid encrypted content")?;
                    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
                    mac.update(b"exetrouter/context-affinity/v1\0");
                    mac.update(&user.to_be_bytes());
                    mac.update(content.as_bytes());
                    found.insert(mac.finalize().into_bytes().into());
                    if found.len() > ITEMS {
                        return Err("too many opaque context items".into());
                    }
                }
                if let Some(args) = object
                    .get("encrypted_function_args")
                    .filter(|value| !value.is_null())
                {
                    let args = args
                        .as_array()
                        .ok_or("invalid encrypted function arguments")?;
                    for value in args {
                        let value = value
                            .as_str()
                            .filter(|value| !value.is_empty())
                            .ok_or("invalid encrypted function arguments")?;
                        let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
                        mac.update(b"exetrouter/function-affinity/v1\0");
                        mac.update(&user.to_be_bytes());
                        mac.update(value.as_bytes());
                        found.insert(mac.finalize().into_bytes().into());
                        if found.len() > ITEMS {
                            return Err("too many opaque context items".into());
                        }
                    }
                }
                for (name, child) in object {
                    if name != "encrypted_content" && name != "encrypted_function_args" {
                        visit(child, key, user, found)?;
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, key, user, found)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut found = BTreeSet::new();
    visit(value, key, user, &mut found)?;
    Ok(found.into_iter().collect())
}

#[derive(Debug, PartialEq)]
pub(crate) enum Lookup {
    None,
    Account(i64),
    Portable(i64),
    Missing,
    Conflict,
}

pub(crate) fn lookup(conn: &Connection, user: i64, digests: &[Digest], now: i64) -> Result<Lookup> {
    let mut account = None;
    let mut portable_account = None;
    for digest in digests {
        let found = conn.query_row("SELECT account_id FROM context_bindings WHERE user_id=?1 AND digest=?2 AND expires_at>?3", params![user,digest.as_slice(),now], |row| row.get::<_,i64>(0)).optional()?;
        let Some(found) = found else {
            return Ok(Lookup::Missing);
        };
        let portable: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM context_bindings WHERE user_id=?1 AND digest=?2 AND expires_at>?3)", params![user, portability_digest(digest).as_slice(),now], |row|row.get(0))?;
        if portable {
            portable_account.get_or_insert(found);
            continue;
        }
        if account.is_some_and(|account| account != found) {
            return Ok(Lookup::Conflict);
        }
        account = Some(found);
    }
    Ok(account.map_or_else(
        || portable_account.map_or(Lookup::None, Lookup::Portable),
        Lookup::Account,
    ))
}

fn portability_digest(digest: &Digest) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"exetrouter/context-portability/v1\0");
    hash.update(digest);
    hash.finalize().into()
}

pub(crate) fn save(
    conn: &mut Connection,
    user: i64,
    account: i64,
    generation: i64,
    digests: &[Digest],
    now: i64,
) -> Result<()> {
    if digests.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')", params![account,generation], |row| row.get(0))?;
    if !valid {
        return Err("account changed before context binding".into());
    }
    tx.execute("DELETE FROM context_bindings WHERE expires_at<=?1", [now])?;
    let mut added = 0;
    for digest in digests {
        let old = tx
            .query_row(
                "SELECT account_id FROM context_bindings WHERE user_id=?1 AND digest=?2",
                params![user, digest.as_slice()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        match old {
            Some(old) if old != account => {
                let portable: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM context_bindings WHERE user_id=?1 AND digest=?2 AND expires_at>?3)", params![user,portability_digest(digest).as_slice(),now], |row|row.get(0))?;
                if !portable {
                    return Err("opaque context account conflict".into());
                }
            }
            None => added += 1,
            _ => {}
        }
    }
    let (own, total): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(user_id=?1),0),COUNT(*) FROM context_bindings",
        [user],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if own + added > PER_USER || total + added > TOTAL {
        return Err("context binding capacity reached".into());
    }
    for digest in digests {
        tx.execute("INSERT INTO context_bindings(user_id,digest,account_id,expires_at) VALUES(?1,?2,?3,?4) ON CONFLICT(user_id,digest) DO UPDATE SET account_id=excluded.account_id,expires_at=MAX(expires_at,excluded.expires_at)",params![user,digest.as_slice(),account,now.saturating_add(TTL)])?;
        tx.execute("UPDATE context_bindings SET expires_at=MAX(expires_at,?1) WHERE user_id=?2 AND digest=?3", params![now.saturating_add(TTL),user,portability_digest(digest).as_slice()])?;
    }
    tx.commit()?;
    Ok(())
}

/// Move already authorized input after a quota-only selection. Merely reading
/// context must neither create a binding nor extend its lifetime.
pub(crate) fn transfer(
    conn: &mut Connection,
    user: i64,
    account: i64,
    generation: i64,
    digests: &[Digest],
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')", params![account,generation], |row| row.get(0))?;
    if !valid {
        return Err("account changed before context transfer".into());
    }
    tx.execute("DELETE FROM context_bindings WHERE expires_at<=?1", [now])?;
    for digest in digests {
        if tx.execute("UPDATE context_bindings SET account_id=?1 WHERE user_id=?2 AND digest=?3 AND expires_at>?4", params![account,user,digest.as_slice(),now])? != 1 {
            return Err("context unavailable for transfer".into());
        }
        tx.execute("INSERT INTO context_bindings(user_id,digest,account_id,expires_at) SELECT user_id,?1,account_id,expires_at FROM context_bindings WHERE user_id=?2 AND digest=?3 ON CONFLICT(user_id,digest) DO UPDATE SET account_id=excluded.account_id,expires_at=MAX(expires_at,excluded.expires_at)",params![portability_digest(digest).as_slice(),user,digest.as_slice()])?;
    }
    let (own, total): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(user_id=?1),0),COUNT(*) FROM context_bindings",
        [user],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if own > PER_USER || total > TOTAL {
        return Err("context binding capacity reached".into());
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute_batch("INSERT INTO users VALUES(1,'alice',0),(2,'bob',0); INSERT INTO oauth_accounts(id,account_id,state,encrypted_credentials,expires_at,created_at) VALUES(1,'first','active',X'00',9999,0),(2,'second','active',X'00',9999,0);").unwrap();
        conn
    }
    #[test]
    fn quota_transfer_requires_live_user_ownership_and_preserves_expiry_atomically() {
        let mut conn = database();
        let first = [[1; 32]];
        save(&mut conn, 1, 1, 0, &first, 1000).unwrap();
        assert!(transfer(&mut conn, 2, 2, 0, &first, 1001).is_err());
        assert!(transfer(&mut conn, 1, 2, 0, &[first[0], [2; 32]], 1001).is_err());
        assert_eq!(lookup(&conn, 1, &first, 1001).unwrap(), Lookup::Account(1));
        transfer(&mut conn, 1, 2, 0, &first, 1001).unwrap();
        assert_eq!(lookup(&conn, 1, &first, 1001).unwrap(), Lookup::Portable(2));
        assert_eq!(
            lookup(&conn, 1, &first, 1000 + TTL).unwrap(),
            Lookup::Missing
        );
        assert!(transfer(&mut conn, 1, 1, 0, &first, 1000 + TTL).is_err());
        assert!(transfer(&mut conn, 1, 2, 1, &first, 1001).is_err());
    }
    #[test]
    fn transferred_shared_checkpoint_does_not_move_concurrent_fork_outputs() {
        let mut conn = database();
        save(&mut conn, 1, 1, 0, &[[1; 32]], 1000).unwrap();
        transfer(&mut conn, 1, 2, 0, &[[1; 32]], 1001).unwrap();
        save(&mut conn, 1, 2, 0, &[[2; 32]], 1001).unwrap();
        transfer(&mut conn, 1, 1, 0, &[[1; 32]], 1002).unwrap();
        save(&mut conn, 1, 1, 0, &[[3; 32]], 1002).unwrap();
        assert_eq!(
            lookup(&conn, 1, &[[1; 32], [2; 32]], 1003).unwrap(),
            Lookup::Account(2)
        );
        assert_eq!(
            lookup(&conn, 1, &[[1; 32], [3; 32]], 1003).unwrap(),
            Lookup::Account(1)
        );
        assert_eq!(
            lookup(&conn, 1, &[[2; 32], [3; 32]], 1003).unwrap(),
            Lookup::Conflict
        );
        save(&mut conn, 1, 2, 0, &[[1; 32]], 1004).unwrap();
        assert_eq!(
            lookup(&conn, 1, &[[1; 32]], 1005).unwrap(),
            Lookup::Portable(2)
        );
        assert_eq!(lookup(&conn, 2, &[[1; 32]], 1005).unwrap(), Lookup::Missing);
    }
    #[test]
    fn nested_content_is_scoped_deduplicated_and_bounded() {
        let input = json!([{"type":"reasoning","encrypted_content":"private-checkpoint"},{"type":"message","content":[{"type":"encrypted_content","encrypted_content":"private-checkpoint"}]}]);
        let digest = digests(&input, &[1; 32], 1).unwrap();
        assert_eq!(digest.len(), 1);
        assert_ne!(digest, digests(&input, &[1; 32], 2).unwrap());
        assert_ne!(digest, digests(&input, &[2; 32], 1).unwrap());
        assert!(digests(&json!({"type":"compaction"}), &[1; 32], 1).is_err());
        assert!(digests(&json!({"encrypted_content":false}), &[1; 32], 1).is_err());
        assert!(digests(&json!({"encrypted_content":""}), &[1; 32], 1).is_err());
        assert!(digests(&json!({"encrypted_content":null}), &[1; 32], 1)
            .unwrap()
            .is_empty());
        let input = Value::Array(
            (0..129)
                .map(|id| json!({"encrypted_content":id.to_string()}))
                .collect(),
        );
        assert!(digests(&input, &[1; 32], 1).is_err());
    }
    #[test]
    fn encrypted_tool_calls_bind_their_owner_without_storing_arguments() {
        let input = json!({"type":"function_call","encrypted_function_args":["private-tool-state","private-tool-state"]});
        let a = digests(&input, &[1; 32], 1).unwrap();
        assert_eq!(a.len(), 1);
        assert_ne!(a, digests(&input, &[1; 32], 2).unwrap());
        let reasoning = digests(
            &json!({"encrypted_content":"private-tool-state"}),
            &[1; 32],
            1,
        )
        .unwrap();
        assert_ne!(a, reasoning);
        let mut conn = database();
        save(&mut conn, 1, 2, 0, &a, 1000).unwrap();
        assert_eq!(lookup(&conn, 1, &a, 1001).unwrap(), Lookup::Account(2));
        assert_eq!(lookup(&conn, 2, &a, 1001).unwrap(), Lookup::Missing);
        for bad in [json!([10]), json!("private-tool-state"), json!([""])] {
            assert!(digests(&json!({"encrypted_function_args":bad}), &[1; 32], 1).is_err());
        }
        assert!(digests(&json!({"encrypted_function_args":[]}), &[1; 32], 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn bindings_expire_at_the_boundary_and_cannot_mix_or_move_accounts() {
        let mut conn = database();
        let a = [[1; 32]];
        let b = [[2; 32]];
        save(&mut conn, 1, 1, 0, &a, 1000).unwrap();
        save(&mut conn, 1, 2, 0, &b, 1000).unwrap();
        assert_eq!(lookup(&conn, 1, &a, 1001).unwrap(), Lookup::Account(1));
        assert_eq!(lookup(&conn, 2, &a, 1001).unwrap(), Lookup::Missing);
        assert_eq!(
            lookup(&conn, 1, &[a[0], b[0]], 1001).unwrap(),
            Lookup::Conflict
        );
        assert!(save(&mut conn, 1, 2, 0, &a, 1001).is_err());
        assert_eq!(lookup(&conn, 1, &a, 1000 + TTL).unwrap(), Lookup::Missing);
        save(&mut conn, 1, 2, 0, &a, 1000 + TTL).unwrap();
        assert_eq!(
            lookup(&conn, 1, &a, 1000 + TTL).unwrap(),
            Lookup::Account(2)
        );
        conn.execute("UPDATE oauth_accounts SET generation=1 WHERE id=2", [])
            .unwrap();
        assert!(save(&mut conn, 1, 2, 0, &[[3; 32]], 1000 + TTL).is_err());
        assert_eq!(
            lookup(&conn, 1, &[[3; 32]], 1000 + TTL).unwrap(),
            Lookup::Missing
        );
    }
    #[test]
    fn capacity_and_conflicting_batches_roll_back_without_evicting_live_context() {
        let mut conn = database();
        conn.execute(
            "INSERT INTO context_bindings VALUES(1,?1,1,?2)",
            params![[1_u8; 32].as_slice(), 1000 + TTL],
        )
        .unwrap();
        assert!(save(&mut conn, 1, 2, 0, &[[2; 32], [1; 32]], 1000).is_err());
        assert_eq!(lookup(&conn, 1, &[[2; 32]], 1000).unwrap(), Lookup::Missing);
        conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<4095) INSERT INTO context_bindings SELECT 1,CAST(printf('%032d',x) AS BLOB),1,999999 FROM n;").unwrap();
        assert!(save(&mut conn, 1, 1, 0, &[[3; 32]], 1000).is_err());
        save(&mut conn, 1, 1, 0, &[[1; 32]], 1001).unwrap();
        assert_eq!(
            lookup(&conn, 1, &[[1; 32]], 1000 + TTL).unwrap(),
            Lookup::Account(1)
        );
        assert_eq!(lookup(&conn, 1, &[[3; 32]], 1000).unwrap(), Lookup::Missing);
        let mut conn = database();
        conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<65536) INSERT INTO context_bindings SELECT 2,CAST(printf('%032d',x) AS BLOB),1,999999 FROM n;").unwrap();
        assert!(save(&mut conn, 1, 1, 0, &[[3; 32]], 1000).is_err());
        assert_eq!(lookup(&conn, 1, &[[3; 32]], 1000).unwrap(), Lookup::Missing);
    }
}
