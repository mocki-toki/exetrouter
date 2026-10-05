//! Client-carried ownership proofs and bounded legacy/transfer overrides.
use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;

pub(crate) const TTL: i64 = 24 * 60 * 60;
const ITEMS: usize = 128;
// Only legacy bindings and explicit quota-transfer overrides occupy the registry.
const PER_USER: i64 = 32768;
const TOTAL: i64 = 65536;
pub(crate) type Digest = [u8; 32];

/// Opaque client-carried proof. Encrypt routing metadata so internal account IDs
/// never become public. Domain-separated keys and AAD bind the exact upstream
/// value, field kind and router user. No upstream content is stored here.
const PROOF_PREFIX: &str = "exrctx1.";
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Content,
    Arguments,
    Turn,
    Conversation,
}
impl Kind {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::Content => b"exetrouter/context-affinity/v1\0",
            Self::Arguments => b"exetrouter/function-affinity/v1\0",
            Self::Turn => b"exetrouter/turn-affinity/v1\0",
            Self::Conversation => b"exetrouter/conversation-affinity/v1\0",
        }
    }
}
fn validate_value(raw: &str, kind: Kind) -> Result<()> {
    if raw.is_empty() {
        return Err("invalid context value".into());
    }
    match kind {
        Kind::Turn if raw.len() > 4096 || !raw.bytes().all(|b| (33..=126).contains(&b)) => {
            Err("invalid turn state".into())
        }
        Kind::Conversation if raw.len() > 256 || raw.chars().any(char::is_control) => {
            Err("invalid conversation reference".into())
        }
        _ => Ok(()),
    }
}
fn value_digest(raw: &str, kind: Kind, key: &[u8], user: i64) -> Result<Digest> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
    mac.update(kind.domain());
    mac.update(&user.to_be_bytes());
    mac.update(raw.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}
fn proof_key(key: &[u8]) -> Result<[u8; 32]> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
    mac.update(b"exetrouter/context-proof/encryption/v1\0");
    Ok(mac.finalize().into_bytes().into())
}
pub(crate) fn issue(
    raw: &str,
    kind: Kind,
    key: &[u8],
    user: i64,
    account: i64,
    expiry: i64,
) -> Result<String> {
    validate_value(raw, kind)?;
    let mut metadata = Vec::with_capacity(24);
    metadata.extend(user.to_be_bytes());
    metadata.extend(account.to_be_bytes());
    metadata.extend(expiry.to_be_bytes());
    // Deterministic per exact plaintext/AAD: added/done/completed events for the
    // same item must carry the same proof. Separate PRF and encryption domains.
    let mut nonce_mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
    nonce_mac.update(b"exetrouter/context-proof/nonce/v1\0");
    nonce_mac.update(kind.domain());
    nonce_mac.update(&metadata);
    nonce_mac.update(raw.as_bytes());
    let nonce = nonce_mac.finalize().into_bytes();
    let digest = value_digest(raw, kind, key, user)?;
    let cipher = XChaCha20Poly1305::new((&proof_key(key)?).into());
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(&nonce[..24]),
            Payload {
                msg: &metadata,
                aad: &digest,
            },
        )
        .map_err(|_| "context proof unavailable")?;
    let mut header = nonce[..24].to_vec();
    header.extend(sealed);
    Ok(format!(
        "{PROOF_PREFIX}{}.{raw}",
        URL_SAFE_NO_PAD.encode(header)
    ))
}
#[derive(Clone)]
pub(crate) struct Reference {
    digest: Digest,
    owner: Option<(i64, i64)>,
}
pub(crate) fn reference(
    value: &str,
    kind: Kind,
    key: &[u8],
    user: i64,
    now: i64,
) -> Result<Reference> {
    let (raw, owner) = open(value, kind, key, user, now)?;
    validate_value(raw, kind)?;
    Ok(Reference {
        digest: value_digest(raw, kind, key, user)?,
        owner,
    })
}
fn open<'a>(
    value: &'a str,
    kind: Kind,
    key: &[u8],
    user: i64,
    now: i64,
) -> Result<(&'a str, Option<(i64, i64)>)> {
    let Some(encoded) = value.strip_prefix(PROOF_PREFIX) else {
        return Ok((value, None));
    };
    let (header, raw) = encoded.split_once('.').ok_or("context proof invalid")?;
    if header.len() != 86 || raw.is_empty() {
        return Err("context proof invalid".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(header)
        .map_err(|_| "context proof invalid")?;
    if bytes.len() != 64 {
        return Err("context proof invalid".into());
    }
    let cipher = XChaCha20Poly1305::new((&proof_key(key)?).into());
    let digest = value_digest(raw, kind, key, user)?;
    let meta = cipher
        .decrypt(
            XNonce::from_slice(&bytes[..24]),
            Payload {
                msg: &bytes[24..],
                aad: &digest,
            },
        )
        .map_err(|_| "context proof invalid")?;
    if meta.len() != 24 {
        return Err("context proof invalid".into());
    }
    let owner = i64::from_be_bytes(meta[..8].try_into()?);
    let account = i64::from_be_bytes(meta[8..16].try_into()?);
    let expiry = i64::from_be_bytes(meta[16..24].try_into()?);
    if owner != user || account <= 0 || expiry <= now {
        return Err("context proof invalid".into());
    }
    Ok((raw, Some((account, expiry))))
}

fn opaque_values(
    value: &mut Value,
    apply: &mut impl FnMut(&str, Kind) -> Result<String>,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (name, child) in object {
                if name == "encrypted_content" && !child.is_null() {
                    let raw = child
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("invalid encrypted content")?;
                    *child = Value::String(apply(raw, Kind::Content)?);
                } else if name == "encrypted_function_args" && !child.is_null() {
                    for arg in child
                        .as_array_mut()
                        .ok_or("invalid encrypted function arguments")?
                    {
                        let raw = arg
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or("invalid encrypted function arguments")?;
                        *arg = Value::String(apply(raw, Kind::Arguments)?);
                    }
                } else {
                    opaque_values(child, apply)?;
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                opaque_values(value, apply)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(crate) fn references(value: &Value, key: &[u8], user: i64, now: i64) -> Result<Vec<Reference>> {
    // Retain existing structural and per-snapshot bounds, including compaction.
    digests(value, key, user)?;
    fn collect(
        value: &Value,
        key: &[u8],
        user: i64,
        now: i64,
        found: &mut Vec<Reference>,
    ) -> Result<()> {
        let mut add = |raw: &str, kind| -> Result<()> {
            let item = reference(raw, kind, key, user, now)?;
            if !found.iter().any(|old| old.digest == item.digest) {
                found.push(item);
            }
            Ok(())
        };
        match value {
            Value::Object(object) => {
                if let Some(raw) = object.get("encrypted_content").and_then(Value::as_str) {
                    add(raw, Kind::Content)?;
                }
                if let Some(args) = object
                    .get("encrypted_function_args")
                    .and_then(Value::as_array)
                {
                    for raw in args {
                        add(
                            raw.as_str().ok_or("invalid encrypted function arguments")?,
                            Kind::Arguments,
                        )?;
                    }
                }
                for (name, child) in object {
                    if name != "encrypted_content" && name != "encrypted_function_args" {
                        collect(child, key, user, now, found)?;
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, key, user, now, found)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut found = Vec::new();
    collect(value, key, user, now, &mut found)?;
    Ok(found)
}
pub(crate) fn wrap_output(
    value: &mut Value,
    key: &[u8],
    user: i64,
    account: i64,
    expiry: i64,
) -> Result<()> {
    digests(value, key, user)?;
    opaque_values(value, &mut |raw, kind| {
        issue(raw, kind, key, user, account, expiry)
    })
}
/// Decode only at dispatch. The recoverable window and retry payload keep proofs
/// so authorization still works across quota retries and replacement sockets.
pub(crate) fn upstream_payload(value: &Value, key: &[u8], user: i64, now: i64) -> Result<Value> {
    let mut value = value.clone();
    opaque_values(&mut value["input"], &mut |raw, kind| {
        Ok(open(raw, kind, key, user, now)?.0.to_owned())
    })?;
    if let Some(turn) = value
        .pointer_mut("/client_metadata/x-codex-turn-state")
        .filter(|v| !v.is_null())
    {
        *turn = Value::String(
            open(
                turn.as_str().ok_or("invalid turn state")?,
                Kind::Turn,
                key,
                user,
                now,
            )?
            .0
            .to_owned(),
        );
    }
    if let Some(conversation) = value.get_mut("conversation").filter(|v| !v.is_null()) {
        let id = if conversation.is_string() {
            conversation
        } else {
            conversation
                .get_mut("id")
                .ok_or("invalid conversation reference")?
        };
        *id = Value::String(
            open(
                id.as_str().ok_or("invalid conversation reference")?,
                Kind::Conversation,
                key,
                user,
                now,
            )?
            .0
            .to_owned(),
        );
    }
    Ok(value)
}
pub(crate) fn upstream_turn(value: &str, key: &[u8], user: i64, now: i64) -> Result<String> {
    Ok(open(value, Kind::Turn, key, user, now)?.0.to_owned())
}
pub(crate) fn lookup_references(
    conn: &Connection,
    user: i64,
    refs: &[Reference],
    now: i64,
) -> Result<Lookup> {
    let mut owner = None;
    let mut portable_owner = None;
    for item in refs {
        let result = lookup(conn, user, &[item.digest], now)?;
        let (account, portable) = match result {
            Lookup::Portable(id) => (id, true),
            _ if item.owner.is_some() => (item.owner.expect("signed owner").0, false),
            Lookup::Account(id) => (id, false),
            _ => return Ok(Lookup::Missing),
        };
        if portable {
            portable_owner.get_or_insert(account);
        } else if owner.is_some_and(|id| id != account) {
            return Ok(Lookup::Conflict);
        } else {
            owner = Some(account);
        }
    }
    Ok(owner.map_or_else(
        || portable_owner.map_or(Lookup::None, Lookup::Portable),
        Lookup::Account,
    ))
}
#[derive(Debug)]
pub(crate) struct TransferStorageFull;
impl std::fmt::Display for TransferStorageFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("context transfer storage full")
    }
}
impl std::error::Error for TransferStorageFull {}

/// Only explicit, already-authorized transfers create records for new proofs.
/// These overrides preserve immutable ancestor proofs across concurrent forks.
pub(crate) fn transfer_references(
    conn: &mut Connection,
    user: i64,
    account: i64,
    generation: i64,
    refs: &[Reference],
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')", params![account,generation], |r| r.get(0))?;
    if !valid {
        return Err("account changed before context transfer".into());
    }
    tx.execute("DELETE FROM context_bindings WHERE expires_at<=?1", [now])?;
    for item in refs {
        let expiry = match item.owner {
            Some((_, expiry)) => expiry,
            None => tx.query_row("SELECT expires_at FROM context_bindings WHERE user_id=?1 AND digest=?2 AND expires_at>?3", params![user,item.digest.as_slice(),now], |r| r.get::<_,i64>(0))?,
        };
        for digest in [item.digest, portability_digest(&item.digest)] {
            tx.execute("INSERT INTO context_bindings(user_id,digest,account_id,expires_at) VALUES(?1,?2,?3,?4) ON CONFLICT(user_id,digest) DO UPDATE SET account_id=excluded.account_id,expires_at=MAX(expires_at,excluded.expires_at)", params![user,digest.as_slice(),account,expiry])?;
        }
    }
    let (own, total): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(user_id=?1),0),COUNT(*) FROM context_bindings",
        [user],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if own > PER_USER || total > TOTAL {
        return Err(TransferStorageFull.into());
    }
    tx.commit()?;
    Ok(())
}

/// Hash only; the upstream routing token remains in transient header memory.
pub(crate) fn turn_digest(value: &str, key: &[u8], user: i64) -> Result<Digest> {
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| (33..=126).contains(&b)) {
        return Err("invalid turn routing state".into());
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
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
                    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
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
                        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
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

#[cfg(test)]
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
#[cfg(test)]
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
    fn proofs_are_stateless_user_scoped_authenticated_and_expire_exactly() {
        let key = [7; 32];
        let proof = issue("opaque.value", Kind::Content, &key, 1, 2, 2000).unwrap();
        assert_eq!(
            proof,
            issue("opaque.value", Kind::Content, &key, 1, 2, 2000).unwrap()
        );
        assert_eq!(
            open(&proof, Kind::Content, &key, 1, 1999).unwrap(),
            ("opaque.value", Some((2, 2000)))
        );
        assert!(open(&proof, Kind::Content, &key, 1, 2000).is_err());
        assert!(open(&proof, Kind::Content, &key, 2, 1000).is_err());
        assert!(open(&proof, Kind::Arguments, &key, 1, 1000).is_err());
        assert!(open(&proof, Kind::Content, &[8; 32], 1, 1000).is_err());
        assert!(open(&(proof.clone() + "tampered"), Kind::Content, &key, 1, 1000).is_err());
        let conn = database();
        let refs = references(&json!([{"encrypted_content":proof}]), &key, 1, 1000).unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &refs, 1000).unwrap(),
            Lookup::Account(2)
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn full_transfer_registry_rolls_back_without_affecting_stateless_continuations() {
        let mut conn = database();
        conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<65536) INSERT INTO context_bindings SELECT 2,CAST(printf('%032d',x) AS BLOB),1,999999 FROM n;").unwrap();
        let proof = issue("new-state", Kind::Content, &[7; 32], 1, 1, 2000).unwrap();
        let refs = references(&json!([{"encrypted_content":proof}]), &[7; 32], 1, 1000).unwrap();
        let err = transfer_references(&mut conn, 1, 2, 0, &refs, 1000).unwrap_err();
        assert!(err.is::<TransferStorageFull>());
        assert_eq!(
            lookup_references(&conn, 1, &refs, 1001).unwrap(),
            Lookup::Account(1)
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            65536
        );
    }

    #[test]
    fn proof_transfer_preserves_expiry_legacy_context_and_independent_forks() {
        let mut conn = database();
        let key = [7; 32];
        let ancestor = issue("ancestor", Kind::Content, &key, 1, 1, 2000).unwrap();
        let refs = references(&json!([{"encrypted_content":ancestor}]), &key, 1, 1000).unwrap();
        transfer_references(&mut conn, 1, 2, 0, &refs, 1000).unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &refs, 1001).unwrap(),
            Lookup::Portable(2)
        );
        let output = issue("fork-output", Kind::Content, &key, 1, 1, 2100).unwrap();
        let fork = references(
            &json!([{"encrypted_content":ancestor},{"encrypted_content":output}]),
            &key,
            1,
            1001,
        )
        .unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &fork, 1001).unwrap(),
            Lookup::Account(1)
        );
        assert_eq!(
            conn.query_row("SELECT MAX(expires_at) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2000
        );
        let legacy = reference("old-raw-value", Kind::Content, &key, 1, 1000).unwrap();
        save(&mut conn, 1, 1, 0, &[legacy.digest], 1000).unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &[legacy], 1001).unwrap(),
            Lookup::Account(1)
        );
        assert!(reference(&ancestor, Kind::Content, &key, 1, 2000).is_err());
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
    fn busy_sessions_can_bind_beyond_the_old_daily_limit() {
        let mut conn = database();
        conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<4096) INSERT INTO context_bindings SELECT 1,CAST(printf('%032d',x) AS BLOB),1,999999 FROM n;").unwrap();
        let turn = turn_digest("new-turn", b"test-key", 1).unwrap();
        save(&mut conn, 1, 1, 0, &[turn, [3; 32]], 1000).unwrap();
        assert_eq!(
            lookup(&conn, 1, &[turn, [3; 32]], 1001).unwrap(),
            Lookup::Account(1)
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            4098
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
        conn.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<?1) INSERT INTO context_bindings SELECT 1,CAST(printf('%032d',x) AS BLOB),1,999999 FROM n", [PER_USER - 1]).unwrap();
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
