//! Client-carried ownership and transfer proofs. Legacy overrides are read-only.
use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

// Preserve the v1 envelope/database layout and compatibility with older readers,
// but ownership is not a lease. Every request still requires a valid bearer and
// an authorized, eligible account; elapsed time must not invalidate its history.
const RETAINED: i64 = i64::MAX;
// Long native histories can contain one opaque reasoning item per turn and
// multiple encrypted tool arguments. Bound each current snapshot, not the
// lifetime number of sessions or transfers.
const ITEMS: usize = 16384;
// Only retained legacy transfers occupy the registry. New transfers never write.
const TOTAL: i64 = 65536;
const TRANSFER_PREFIX: &str = "exrtr1.";
const TRANSFER_PLAIN_BYTES: usize = 16 + ITEMS * 32;
pub(crate) const TRANSFER_OUTPUTS: usize = 4;
pub(crate) const TRANSFER_OUTPUT_BYTES: usize = 1024 * 1024;
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
    Replay,
}
impl Kind {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::Content => b"exetrouter/context-affinity/v1\0",
            Self::Arguments => b"exetrouter/function-affinity/v1\0",
            Self::Turn => b"exetrouter/turn-affinity/v1\0",
            Self::Conversation => b"exetrouter/conversation-affinity/v1\0",
            Self::Replay => b"exetrouter/client-replay-affinity/v1\0",
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
pub(crate) fn issue(raw: &str, kind: Kind, key: &[u8], user: i64, account: i64) -> Result<String> {
    seal(raw, kind, key, user, account, RETAINED)
}
fn seal(
    raw: &str,
    kind: Kind,
    key: &[u8],
    user: i64,
    account: i64,
    legacy_expiry: i64,
) -> Result<String> {
    validate_value(raw, kind)?;
    let mut metadata = Vec::with_capacity(24);
    metadata.extend(user.to_be_bytes());
    metadata.extend(account.to_be_bytes());
    metadata.extend(legacy_expiry.to_be_bytes());
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
    account: i64,
    portable: bool,
    receipt: bool,
}
pub(crate) fn reference(value: &str, kind: Kind, key: &[u8], user: i64) -> Result<Reference> {
    let (raw, account) = open(value, kind, key, user)?;
    validate_value(raw, kind)?;
    Ok(Reference {
        digest: value_digest(raw, kind, key, user)?,
        account,
        portable: false,
        receipt: false,
    })
}

pub(crate) fn reasoning_owner(value: &str, key: &[u8], user: i64) -> Result<i64> {
    Ok(open(value, Kind::Content, key, user)?.1)
}

pub(crate) fn replay_owner(value: &str, scope: &str, key: &[u8], user: i64) -> Result<i64> {
    let (actual, account) = open(value, Kind::Replay, key, user)?;
    if actual != scope {
        return Err("replay scope mismatch".into());
    }
    Ok(account)
}
fn open<'a>(value: &'a str, kind: Kind, key: &[u8], user: i64) -> Result<(&'a str, i64)> {
    let encoded = value
        .strip_prefix(PROOF_PREFIX)
        .ok_or("context proof required")?;
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
    // The authenticated legacy expiry remains in the format, but is no longer
    // an authorization condition, including for already-issued proofs.
    if owner != user || account == 0 || account == i64::MIN {
        return Err("context proof invalid".into());
    }
    Ok((raw, account))
}

pub(crate) fn is_transfer_item(item: &Value) -> bool {
    item["encrypted_content"]
        .as_str()
        .is_some_and(|value| value.starts_with(TRANSFER_PREFIX))
}

fn transfer_token(item: &Value) -> Result<&str> {
    let object = item.as_object().ok_or("invalid transfer receipt")?;
    if item["type"] != "reasoning"
        || !item["summary"].as_array().is_some_and(Vec::is_empty)
        || object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "type"
                    | "id"
                    | "summary"
                    | "encrypted_content"
                    | "status"
                    | "content"
                    | "internal_chat_message_metadata_passthrough"
            )
        })
        || object.get("id").is_some_and(|id| !id.is_string())
        || object
            .get("status")
            .is_some_and(|status| status != "completed")
        || object.get("content").is_some_and(|content| {
            !content.is_null() && !content.as_array().is_some_and(Vec::is_empty)
        })
    {
        return Err("invalid transfer receipt".into());
    }
    item["encrypted_content"]
        .as_str()
        .ok_or_else(|| "invalid transfer receipt".into())
}

fn transfer_key(key: &[u8]) -> Result<[u8; 32]> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
    mac.update(b"exetrouter/context-transfer/encryption/v1\0");
    Ok(mac.finalize().into_bytes().into())
}

fn open_transfer(item: &Value, key: &[u8], user: i64) -> Result<(i64, Vec<u8>)> {
    let token = transfer_token(item)?
        .strip_prefix(TRANSFER_PREFIX)
        .ok_or("invalid transfer receipt")?;
    // Reject excessive encrypted metadata before allocating or decoding it.
    if token.len() > (TRANSFER_PLAIN_BYTES + 40).div_ceil(3) * 4 {
        return Err("excessive transfer receipt".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| "invalid transfer receipt")?;
    if bytes.len() < 56 || bytes.len() > TRANSFER_PLAIN_BYTES + 40 {
        return Err("invalid transfer receipt".into());
    }
    let plain = XChaCha20Poly1305::new((&transfer_key(key)?).into())
        .decrypt(
            XNonce::from_slice(&bytes[..24]),
            Payload {
                msg: &bytes[24..],
                aad: &user.to_be_bytes(),
            },
        )
        .map_err(|_| "invalid transfer receipt")?;
    if plain.len() < 16
        || (plain.len() - 16) % 32 != 0
        || i64::from_be_bytes(plain[..8].try_into()?) != user
    {
        return Err("invalid transfer receipt".into());
    }
    let account = i64::from_be_bytes(plain[8..16].try_into()?);
    if account <= 0 {
        return Err("invalid transfer receipt".into());
    }
    Ok((account, plain))
}

/// A router-only, empty reasoning carrier. It contains no model text and is
/// removed before dispatch. Native clients already preserve encrypted reasoning.
pub(crate) fn transfer_item(
    key: &[u8],
    user: i64,
    account: i64,
    digests: &[Digest],
) -> Result<Value> {
    if digests.len() > ITEMS || account <= 0 {
        return Err("excessive transfer context".into());
    }
    let mut plain = Vec::with_capacity(16 + digests.len() * 32);
    plain.extend(user.to_be_bytes());
    plain.extend(account.to_be_bytes());
    for digest in digests {
        plain.extend(digest);
    }
    let mut nonce = [0; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let sealed = XChaCha20Poly1305::new((&transfer_key(key)?).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: &user.to_be_bytes(),
            },
        )
        .map_err(|_| "transfer receipt unavailable")?;
    let mut bytes = nonce.to_vec();
    bytes.extend(sealed);
    Ok(
        serde_json::json!({"type":"reasoning","id":format!("rs_exr_{}", URL_SAFE_NO_PAD.encode(nonce)),"summary":[],"encrypted_content":format!("{TRANSFER_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))}),
    )
}

pub(crate) fn receipt_owners(refs: &[Reference]) -> BTreeSet<i64> {
    refs.iter()
        .filter(|item| item.portable && !item.receipt)
        .map(|item| item.account)
        .collect()
}
pub(crate) fn retired_turn(refs: &[Reference], turn: &Reference) -> bool {
    receipt_owners(refs).contains(&turn.account)
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
pub(crate) fn references(value: &Value, key: &[u8], user: i64) -> Result<Vec<Reference>> {
    // Retain existing structural and per-snapshot bounds, including compaction.
    digests(value, key, user)?;
    fn collect(
        value: &Value,
        key: &[u8],
        user: i64,
        found: &mut BTreeMap<Digest, Reference>,
    ) -> Result<()> {
        let mut add = |raw: &str, kind| -> Result<()> {
            let item = reference(raw, kind, key, user)?;
            if found.get(&item.digest).is_some_and(|old| {
                old.account != item.account && (old.account < 0 || item.account < 0)
            }) {
                return Err("conflicting backend context".into());
            }
            found.entry(item.digest).or_insert(item);
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
                        collect(child, key, user, found)?;
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, key, user, found)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    let mut receipts = Vec::new();
    if let Some(items) = value.as_array() {
        for item in items {
            if is_transfer_item(item) {
                receipts.push(item);
                if receipts.len() > ITEMS {
                    return Err("too many transfer receipts".into());
                }
            } else {
                collect(item, key, user, &mut found)?;
            }
        }
    } else {
        collect(value, key, user, &mut found)?;
    }
    let mut hints = Vec::new();
    for item in receipts {
        let (account, plain) = open_transfer(item, key, user)?;
        // Retain only matches in this bounded current snapshot, not every digest
        // carried by historical receipts that compaction may have made obsolete.
        for digest in plain[16..].as_chunks::<32>().0 {
            if let Some(item) = found.get_mut(digest) {
                if item.account < 0 {
                    return Err("Platform context cannot use a Codex transfer receipt".into());
                }
                item.portable = true;
            }
        }
        hints.push(Reference {
            digest: value_digest(transfer_token(item)?, Kind::Content, key, user)?,
            account,
            portable: true,
            receipt: true,
        });
    }
    let mut refs: Vec<_> = found.into_values().collect();
    refs.extend(hints);
    Ok(refs)
}
pub(crate) fn wrap_output(value: &mut Value, key: &[u8], user: i64, account: i64) -> Result<()> {
    digests(value, key, user)?;
    opaque_values(value, &mut |raw, kind| issue(raw, kind, key, user, account))
}
/// Decode only at dispatch. The recoverable window and retry payload keep proofs
/// so authorization still works across quota retries and replacement sockets.
pub(crate) fn upstream_payload(value: &Value, key: &[u8], user: i64) -> Result<Value> {
    let mut value = value.clone();
    if let Some(turn) = value
        .pointer("/client_metadata/x-codex-turn-state")
        .and_then(Value::as_str)
    {
        let turn = reference(turn, Kind::Turn, key, user)?;
        if retired_turn(&references(&value["input"], key, user)?, &turn) {
            value["client_metadata"]
                .as_object_mut()
                .expect("validated metadata")
                .remove("x-codex-turn-state");
        }
    }
    if let Some(input) = value["input"].as_array_mut() {
        for item in input.iter().filter(|item| is_transfer_item(item)) {
            open_transfer(item, key, user)?;
        }
        input.retain(|item| !is_transfer_item(item));
    }
    opaque_values(&mut value["input"], &mut |raw, kind| {
        Ok(open(raw, kind, key, user)?.0.to_owned())
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
            )?
            .0
            .to_owned(),
        );
    }
    Ok(value)
}
pub(crate) fn upstream_turn(value: &str, key: &[u8], user: i64) -> Result<String> {
    Ok(open(value, Kind::Turn, key, user)?.0.to_owned())
}
pub(crate) fn lookup_references(
    conn: &Connection,
    user: i64,
    refs: &[Reference],
) -> Result<Lookup> {
    let mut owner = None;
    let mut portable_owner = None;
    for item in refs {
        if item.portable {
            if item.receipt {
                portable_owner = Some(item.account);
            }
            continue;
        }
        let transferred = if item.account < 0 {
            None
        } else {
            conn.query_row(
            "SELECT b.account_id FROM context_bindings b JOIN context_bindings p ON p.user_id=b.user_id AND p.digest=?3 AND p.account_id=b.account_id WHERE b.user_id=?1 AND b.digest=?2",
            params![user,item.digest.as_slice(),portability_digest(&item.digest).as_slice()],
            |row| row.get::<_,i64>(0),
        ).optional()?
        };
        let (account, portable) = transferred.map_or((item.account, false), |id| (id, true));
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
/// Validate the selected credential generation, then return only newly portable
/// digests. Existing client receipts and legacy overrides need no duplication.
/// This transaction never creates or updates a context-binding row.
pub(crate) fn transfer_references(
    conn: &mut Connection,
    user: i64,
    account: i64,
    generation: i64,
    refs: &[Reference],
) -> Result<Vec<Digest>> {
    if !can_transfer(refs, account) {
        return Err("opaque context cannot cross Platform projects or backends".into());
    }
    if account < 0 {
        if !crate::platform::routes::valid(conn, account, generation)?
            || !crate::platform::routes::enabled(conn, account)?
        {
            return Err("Platform route changed before context transfer".into());
        }
        return Ok(Vec::new());
    }
    let tx = conn.transaction()?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')", params![account,generation], |r| r.get(0))?;
    if !valid || !crate::account_preferences::effective(&tx, account)?.enabled {
        return Err("account changed before context transfer".into());
    }
    let mut digests = Vec::new();
    for item in refs.iter().filter(|item| !item.portable) {
        if matches!(
            lookup_references(&tx, user, std::slice::from_ref(item))?,
            Lookup::Account(_)
        ) {
            digests.push(item.digest);
        }
    }
    tx.commit()?;
    Ok(digests)
}

/// Ownership proofs establish provenance, not provider-side portability.
pub(crate) fn can_transfer(refs: &[Reference], target: i64) -> bool {
    refs.iter().all(|item| item.account >= 0 && target >= 0)
}

pub(crate) fn digests(value: &Value, key: &[u8], user: i64) -> Result<Vec<Digest>> {
    fn visit(value: &Value, key: &[u8], user: i64, found: &mut BTreeSet<Digest>) -> Result<()> {
        match value {
            Value::Object(object) => {
                if is_transfer_item(value) {
                    transfer_token(value)?;
                    return Ok(());
                }
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
    Conflict,
}

fn portability_digest(digest: &Digest) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"exetrouter/context-portability/v1\0");
    hash.update(digest);
    hash.finalize().into()
}

/// Retain paired transfer records, including those with a legacy expiry. Losing
/// a portability marker would break mixed-account histories and concurrent forks.
/// Unpaired rows from the former per-output registry cannot authorize context
/// and should not consume capacity.
/// Run once at service startup, before accepting requests; no schema change.
pub(crate) fn prune_transfers(conn: &mut Connection) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut removed = 0;
    let rows: BTreeMap<(i64, Digest), i64> = {
        let mut statement =
            tx.prepare("SELECT user_id,digest,account_id FROM context_bindings LIMIT ?1")?;
        let rows = statement
            .query_map([TOTAL + 1], |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, Digest>(1)?),
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };
    if rows.len() > TOTAL as usize {
        return Err("context transfer registry exceeds capacity".into());
    }
    let mut keep = BTreeSet::new();
    for (&(user, digest), &account) in &rows {
        let marker = portability_digest(&digest);
        if rows.get(&(user, marker)) == Some(&account) {
            keep.insert((user, digest));
            keep.insert((user, marker));
        }
    }
    let mut delete = tx.prepare("DELETE FROM context_bindings WHERE user_id=?1 AND digest=?2")?;
    for &(user, digest) in rows.keys() {
        if !keep.contains(&(user, digest)) {
            removed += delete.execute(params![user, digest.as_slice()])?;
        }
    }
    drop(delete);
    tx.execute(
        "UPDATE context_bindings SET expires_at=?1 WHERE expires_at<>?1",
        [RETAINED],
    )?;
    tx.commit()?;
    Ok(removed)
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
    fn proofs_are_stateless_user_scoped_authenticated_and_not_time_limited() {
        let key = [7; 32];
        let proof = issue("opaque.value", Kind::Content, &key, 1, 2).unwrap();
        assert_eq!(
            proof,
            seal("opaque.value", Kind::Content, &key, 1, 2, i64::MAX).unwrap()
        );
        assert_eq!(
            open(&proof, Kind::Content, &key, 1).unwrap(),
            ("opaque.value", 2)
        );
        assert!(open(&proof, Kind::Content, &key, 2).is_err());
        assert!(open(&proof, Kind::Arguments, &key, 1).is_err());
        assert!(open(&proof, Kind::Content, &[8; 32], 1).is_err());
        assert!(open(&(proof.clone() + "tampered"), Kind::Content, &key, 1).is_err());
        let conn = database();
        let refs = references(&json!([{"encrypted_content":proof}]), &key, 1).unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &refs).unwrap(),
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
    fn legacy_expiry_does_not_revoke_authenticated_context_of_any_kind() {
        let key = [7; 32];
        for kind in [
            Kind::Content,
            Kind::Arguments,
            Kind::Turn,
            Kind::Conversation,
        ] {
            // This exact v1 envelope would have expired in January 1970.
            let proof = seal("legacy.context", kind, &key, 1, 2, 1).unwrap();
            assert_eq!(open(&proof, kind, &key, 1).unwrap(), ("legacy.context", 2));
            assert_eq!(reference(&proof, kind, &key, 1).unwrap().account, 2);
            assert!(reference(&proof, kind, &key, 2).is_err());
            assert!(reference(&proof, kind, &[8; 32], 1).is_err());
            assert!(reference(&(proof + "tampered"), kind, &key, 1).is_err());
        }
        let payload = json!({
            "input":[{"encrypted_content":seal("reasoning", Kind::Content, &key, 1, 2, 1).unwrap(),
                "encrypted_function_args":[seal("arguments", Kind::Arguments, &key, 1, 2, 1).unwrap()]}],
            "conversation":{"id":seal("conversation", Kind::Conversation, &key, 1, 2, 1).unwrap()},
            "client_metadata":{"x-codex-turn-state":seal("turn", Kind::Turn, &key, 1, 2, 1).unwrap()}
        });
        assert_eq!(
            upstream_payload(&payload, &key, 1).unwrap(),
            json!({
                "input":[{"encrypted_content":"reasoning","encrypted_function_args":["arguments"]}],
                "conversation":{"id":"conversation"},"client_metadata":{"x-codex-turn-state":"turn"}
            })
        );
    }

    #[test]
    fn full_legacy_registry_does_not_limit_new_stateless_transfers() {
        let mut conn = database();
        conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<65536) INSERT INTO context_bindings SELECT 1,CAST(printf('%032d',x) AS BLOB),1,1 FROM n; CREATE TRIGGER no_new_context BEFORE INSERT ON context_bindings BEGIN SELECT RAISE(ABORT,'no context writes'); END;").unwrap();
        let key = [7; 32];
        for n in 0..256 {
            let proof = issue(&format!("state-{n}"), Kind::Content, &key, 1, 1).unwrap();
            let mut input = json!([{"type":"reasoning","encrypted_content":proof}]);
            let refs = references(&input, &key, 1).unwrap();
            let digests = transfer_references(&mut conn, 1, 2, 0, &refs).unwrap();
            input
                .as_array_mut()
                .unwrap()
                .push(transfer_item(&key, 1, 2, &digests).unwrap());
            assert_eq!(
                lookup_references(&conn, 1, &references(&input, &key, 1).unwrap()).unwrap(),
                Lookup::Portable(2)
            );
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            TOTAL
        );
    }

    #[test]
    fn proof_transfer_preserves_legacy_ancestors_and_independent_forks() {
        let mut conn = database();
        let key = [7; 32];
        let ancestor = seal("ancestor", Kind::Content, &key, 1, 1, 1).unwrap();
        let refs = references(&json!([{"encrypted_content":ancestor}]), &key, 1).unwrap();
        let digests = transfer_references(&mut conn, 1, 2, 0, &refs).unwrap();
        let receipt = transfer_item(&key, 1, 2, &digests).unwrap();
        let refs = references(&json!([{"encrypted_content":ancestor},receipt]), &key, 1).unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &refs).unwrap(),
            Lookup::Portable(2)
        );
        let output = issue("fork-output", Kind::Content, &key, 1, 1).unwrap();
        let fork = references(
            &json!([{"encrypted_content":ancestor},receipt,{"encrypted_content":output}]),
            &key,
            1,
        )
        .unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &fork).unwrap(),
            Lookup::Account(1)
        );
        let other_output = issue("other-fork-output", Kind::Content, &key, 1, 2).unwrap();
        let other_fork = references(
            &json!([{"encrypted_content":ancestor},receipt,{"encrypted_content":other_output}]),
            &key,
            1,
        )
        .unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &other_fork).unwrap(),
            Lookup::Account(2)
        );
        // Repeated transfers do not repeat already-proven ancestors or mutate forks.
        assert!(transfer_references(&mut conn, 1, 1, 0, &refs)
            .unwrap()
            .is_empty());
        assert_eq!(
            lookup_references(&conn, 1, &other_fork).unwrap(),
            Lookup::Account(2)
        );
        assert_eq!(
            lookup_references(&conn, 1, &fork).unwrap(),
            Lookup::Account(1)
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn receipts_authenticate_exact_context_owner_and_field_without_authorizing_extra_history() {
        let mut conn = database();
        let key = [7; 32];
        let ancestor = issue("ancestor", Kind::Content, &key, 1, 1).unwrap();
        let args = issue("arguments", Kind::Arguments, &key, 1, 1).unwrap();
        let original = json!([{"type":"reasoning","encrypted_content":ancestor},
            {"type":"function_call","encrypted_function_args":[args]}]);
        let refs = references(&original, &key, 1).unwrap();
        let digests = transfer_references(&mut conn, 1, 2, 0, &refs).unwrap();
        let receipt = transfer_item(&key, 1, 2, &digests).unwrap();
        let mut input = original.clone();
        input.as_array_mut().unwrap().push(receipt.clone());
        assert_eq!(
            lookup_references(&conn, 1, &references(&input, &key, 1).unwrap()).unwrap(),
            Lookup::Portable(2)
        );
        assert_eq!(
            upstream_payload(&json!({"input":input}), &key, 1).unwrap()["input"],
            json!([{"type":"reasoning","encrypted_content":"ancestor"},{"type":"function_call","encrypted_function_args":["arguments"]}])
        );
        assert!(references(&input, &key, 2).is_err());
        assert!(references(&input, &[8; 32], 1).is_err());
        for mutation in ["ciphertext", "content", "type", "summary", "extra"] {
            let mut changed = input.clone();
            match mutation {
                "ciphertext" => {
                    let token = changed[2]["encrypted_content"].as_str().unwrap().to_owned();
                    changed[2]["encrypted_content"] = json!(token + "x");
                }
                "content" => changed[0]["encrypted_content"] = json!(ancestor.clone() + "x"),
                "type" => changed[2]["type"] = json!("compaction"),
                "summary" => {
                    changed[2]["summary"] =
                        json!([{"type":"summary_text","text":"not routing metadata"}])
                }
                _ => changed[2]["arbitrary"] = json!(true),
            }
            assert!(references(&changed, &key, 1).is_err(), "{mutation}");
        }
        let unrelated = issue("untransferred-ancestor", Kind::Content, &key, 1, 1).unwrap();
        let output = issue("new-output", Kind::Content, &key, 1, 2).unwrap();
        input.as_array_mut().unwrap().extend([
            json!({"encrypted_content":unrelated}),
            json!({"encrypted_content":output}),
        ]);
        assert_eq!(
            lookup_references(&conn, 1, &references(&input, &key, 1).unwrap()).unwrap(),
            Lookup::Conflict
        );
        // The receipt cannot stand in for an unsigned upstream value.
        assert!(references(&json!([receipt,{"encrypted_content":"ancestor"}]), &key, 1).is_err());
    }

    #[test]
    fn maximum_snapshot_receipt_is_bounded_and_repeated_transfers_only_prove_new_values() {
        let mut conn = database();
        let key = [7; 32];
        let mut input = Value::Array((0..ITEMS).map(|id| json!({"encrypted_content":issue(&id.to_string(), Kind::Content, &key, 1, 1).unwrap()})).collect());
        let refs = references(&input, &key, 1).unwrap();
        let digests = transfer_references(&mut conn, 1, 2, 0, &refs).unwrap();
        assert_eq!(digests.len(), ITEMS);
        let receipt = transfer_item(&key, 1, 2, &digests).unwrap();
        assert!(serde_json::to_vec(&receipt).unwrap().len() < TRANSFER_OUTPUT_BYTES);
        input.as_array_mut().unwrap().push(receipt);
        let refs = references(&input, &key, 1).unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &refs).unwrap(),
            Lookup::Portable(2)
        );
        assert!(transfer_references(&mut conn, 1, 1, 0, &refs)
            .unwrap()
            .is_empty());
        assert!(transfer_item(&key, 1, 2, &vec![[0; 32]; ITEMS + 1]).is_err());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
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
            (0..=ITEMS)
                .map(|id| json!({"encrypted_content":id.to_string()}))
                .collect(),
        );
        assert!(digests(&input, &[1; 32], 1).is_err());
        let mut input = input;
        input.as_array_mut().unwrap().pop();
        assert_eq!(digests(&input, &[1; 32], 1).unwrap().len(), ITEMS);
    }
    #[test]
    fn long_native_history_preserves_ownership_and_unwraps_all_context() {
        let key = [7; 32];
        let input = Value::Array(
            (0..256)
                .map(|id| json!({"type":"reasoning","encrypted_content":format!("reasoning-{id}"),"encrypted_function_args":[format!("args-{id}")]}))
                .collect(),
        );
        let mut signed = input.clone();
        wrap_output(&mut signed, &key, 1, 2).unwrap();
        let refs = references(&signed, &key, 1).unwrap();
        assert_eq!(refs.len(), 512);
        assert_eq!(
            lookup_references(&database(), 1, &refs).unwrap(),
            Lookup::Account(2)
        );
        assert!(references(&signed, &key, 3).is_err());
        assert_eq!(
            upstream_payload(&json!({"input":signed}), &key, 1).unwrap()["input"],
            input
        );
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
        let mut signed = input.clone();
        wrap_output(&mut signed, &[1; 32], 1, 2).unwrap();
        let refs = references(&signed, &[1; 32], 1).unwrap();
        assert_eq!(
            lookup_references(&database(), 1, &refs).unwrap(),
            Lookup::Account(2)
        );
        assert!(references(&signed, &[1; 32], 2).is_err());
        for bad in [json!([10]), json!("private-tool-state"), json!([""])] {
            assert!(digests(&json!({"encrypted_function_args":bad}), &[1; 32], 1).is_err());
        }
        assert!(digests(&json!({"encrypted_function_args":[]}), &[1; 32], 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn raw_context_is_rejected_even_when_its_digest_is_registered() {
        let conn = database();
        for kind in [
            Kind::Content,
            Kind::Arguments,
            Kind::Turn,
            Kind::Conversation,
        ] {
            let digest = value_digest("old-raw-value", kind, &[7; 32], 1).unwrap();
            conn.execute(
                "INSERT INTO context_bindings VALUES(1,?1,1,2000)",
                [digest.as_slice()],
            )
            .unwrap();
            assert!(reference("old-raw-value", kind, &[7; 32], 1).is_err());
            assert!(open("old-raw-value", kind, &[7; 32], 1).is_err());
        }
    }

    #[test]
    fn startup_prunes_unpaired_rows_but_preserves_legacy_expired_transfers() {
        let mut conn = database();
        let proof = seal("transferred", Kind::Content, &[7; 32], 1, 1, 1).unwrap();
        let refs = references(&json!({"encrypted_content":proof}), &[7; 32], 1).unwrap();
        for digest in [refs[0].digest, portability_digest(&refs[0].digest)] {
            conn.execute(
                "INSERT INTO context_bindings VALUES(1,?1,2,1)",
                [digest.as_slice()],
            )
            .unwrap();
        }
        conn.execute_batch(
            "UPDATE context_bindings SET expires_at=1; INSERT INTO context_bindings VALUES(1,zeroblob(32),1,2000),(2,zeroblob(32),2,1000);",
        )
        .unwrap();
        assert_eq!(
            lookup_references(&conn, 1, &refs).unwrap(),
            Lookup::Portable(2)
        );
        assert_eq!(prune_transfers(&mut conn).unwrap(), 2);
        assert_eq!(prune_transfers(&mut conn).unwrap(), 0);
        assert_eq!(
            lookup_references(&conn, 1, &refs).unwrap(),
            Lookup::Portable(2)
        );
        assert_eq!(
            conn.query_row("SELECT MIN(expires_at) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            RETAINED
        );
    }

    #[test]
    fn transfer_checks_generation_and_active_state_atomically() {
        let mut conn = database();
        let refs = references(
            &json!({"encrypted_content":issue("state", Kind::Content, &[7;32], 1, 1).unwrap()}),
            &[7; 32],
            1,
        )
        .unwrap();
        assert!(transfer_references(&mut conn, 1, 2, 1, &refs).is_err());
        conn.execute("UPDATE oauth_accounts SET state='disabled' WHERE id=2", [])
            .unwrap();
        assert!(transfer_references(&mut conn, 1, 2, 0, &refs).is_err());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
