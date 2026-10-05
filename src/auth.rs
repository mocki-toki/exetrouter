use crate::{IssuedToken, Result, SshIdentity, TokenInfo};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

fn read_ssh_string(blob: &[u8], offset: &mut usize) -> Result<Vec<u8>> {
    let end_len = offset.checked_add(4).ok_or("invalid SSH key")?;
    let len_bytes: [u8; 4] = blob
        .get(*offset..end_len)
        .ok_or("invalid SSH key")?
        .try_into()?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    let end = end_len.checked_add(len).ok_or("invalid SSH key")?;
    let data = blob.get(end_len..end).ok_or("invalid SSH key")?.to_vec();
    *offset = end;
    Ok(data)
}

pub fn parse_ssh_public_key(input: &str) -> Result<(String, String)> {
    let input = input.trim();
    if input.len() > 8192 || input.chars().any(char::is_control) {
        return Err("invalid SSH public key".into());
    }
    let mut fields = input.split_ascii_whitespace();
    if fields.next() != Some("ssh-ed25519") {
        return Err("only ssh-ed25519 public keys are supported".into());
    }
    let encoded = fields.next().ok_or("missing SSH public key data")?;
    let blob = BASE64.decode(encoded)?;
    let mut offset = 0;
    if read_ssh_string(&blob, &mut offset)? != b"ssh-ed25519" {
        return Err("invalid SSH key algorithm".into());
    }
    if read_ssh_string(&blob, &mut offset)?.len() != 32 || offset != blob.len() {
        return Err("invalid Ed25519 public key".into());
    }
    let canonical = format!("ssh-ed25519 {}", BASE64.encode(&blob));
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    );
    Ok((canonical, fingerprint))
}

pub fn register_ssh_key(conn: &Connection, user_id: i64, input: &str) -> Result<SshIdentity> {
    let (public_key, fingerprint) = parse_ssh_public_key(input)?;
    let mut id_bytes = [0u8; 8];
    OsRng.fill_bytes(&mut id_bytes);
    let id = format!("key_{}", hex::encode(id_bytes));
    let now = Utc::now().timestamp();
    conn.execute("INSERT INTO ssh_identities(id,user_id,fingerprint,public_key,created_at) VALUES(?1,?2,?3,?4,?5)", params![id,user_id,fingerprint,public_key,now])?;
    Ok(SshIdentity {
        id,
        user_id,
        fingerprint,
        public_key,
        created_at: now,
        revoked_at: None,
    })
}

pub fn list_ssh_keys(conn: &Connection) -> Result<Vec<SshIdentity>> {
    let mut stmt=conn.prepare("SELECT id,user_id,fingerprint,public_key,created_at,revoked_at FROM ssh_identities ORDER BY created_at,id")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SshIdentity {
                id: r.get(0)?,
                user_id: r.get(1)?,
                fingerprint: r.get(2)?,
                public_key: r.get(3)?,
                created_at: r.get(4)?,
                revoked_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn revoke_ssh_key(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE ssh_identities SET revoked_at=?1 WHERE id=?2 AND revoked_at IS NULL",
        params![Utc::now().timestamp(), id],
    )? == 1)
}

pub fn resolve_ssh_identity(conn: &Connection, id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT user_id FROM ssh_identities WHERE id=?1 AND revoked_at IS NULL",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

fn safe_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

pub fn authorized_keys(conn: &Connection, gateway_bin: &str, socket: &str) -> Result<String> {
    if !safe_absolute_path(gateway_bin) || !safe_absolute_path(socket) {
        return Err("gateway binary and socket must be absolute shell-safe paths".into());
    }
    let mut lines = String::new();
    for identity in list_ssh_keys(conn)?
        .into_iter()
        .filter(|key| key.revoked_at.is_none())
    {
        lines.push_str(&format!("restrict,command=\"{gateway_bin} --control-socket {socket} gateway --identity {}\" {} exetrouter-{}\n",identity.id,identity.public_key,identity.id));
    }
    Ok(lines)
}

pub fn create_user(conn: &Connection, name: &str) -> Result<i64> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("user name must be 1–64 ASCII letters, digits, '_' or '-'".into());
    }
    conn.execute(
        "INSERT INTO users(name, created_at) VALUES(?1, ?2)",
        params![name, Utc::now().timestamp()],
    )?;
    Ok(conn.last_insert_rowid())
}

fn digest(key: &[u8], secret: &str) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(secret.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

pub(crate) fn validate_token_name(name: &str) -> Result<()> {
    if name.starts_with(crate::private_metadata::PREFIX) {
        return crate::private_metadata::validate_envelope(name, 512);
    }
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err("invalid token name".into());
    }
    Ok(())
}

pub fn create_token(
    conn: &Connection,
    key: &[u8],
    user_id: i64,
    name: &str,
    days: i64,
) -> Result<IssuedToken> {
    validate_token_name(name)?;
    if !(1..=365).contains(&days) {
        return Err("expires_days must be 1..=365".into());
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1)",
        [user_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err("unknown user".into());
    }
    let mut id_bytes = [0u8; 8];
    let mut secret_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut id_bytes);
    OsRng.fill_bytes(&mut secret_bytes);
    let id = format!("tok_{}", hex::encode(id_bytes));
    let secret = format!("exr_{}_{}", id, hex::encode(secret_bytes));
    let now = Utc::now().timestamp();
    let expires_at = now + days * 86_400;
    conn.execute(
        "INSERT INTO access_tokens(id,user_id,name,digest,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, user_id, name, digest(key, &secret)?, now, expires_at],
    )?;
    Ok(IssuedToken {
        token: TokenInfo {
            id,
            user_id,
            name: name.to_owned(),
            created_at: now,
            expires_at,
            last_used_at: None,
            revoked_at: None,
        },
        secret,
    })
}

pub fn token_info(conn: &Connection, user_id: i64, id: &str) -> Result<Option<TokenInfo>> {
    Ok(conn.query_row(
        "SELECT id,user_id,name,created_at,expires_at,last_used_at,revoked_at FROM access_tokens WHERE id=?1 AND user_id=?2",
        params![id, user_id],
        |r| Ok(TokenInfo { id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, created_at: r.get(3)?, expires_at: r.get(4)?, last_used_at: r.get(5)?, revoked_at: r.get(6)? }),
    ).optional()?)
}

pub fn list_tokens(conn: &Connection, user_id: i64) -> Result<Vec<TokenInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM access_tokens WHERE user_id=?1 AND revoked_at IS NULL ORDER BY created_at DESC,id DESC",
    )?;
    let ids = stmt
        .query_map([user_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ids.iter()
        .map(|id| token_info(conn, user_id, id).map(|t| t.expect("listed token exists")))
        .collect()
}

pub fn revoke_token(conn: &Connection, user_id: i64, id: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE access_tokens SET revoked_at=?1 WHERE id=?2 AND user_id=?3 AND revoked_at IS NULL",
        params![Utc::now().timestamp(), id, user_id],
    )? == 1)
}

pub fn rotate_token(
    conn: &mut Connection,
    key: &[u8],
    user_id: i64,
    id: &str,
) -> Result<IssuedToken> {
    rotate_at(conn, key, user_id, id, Utc::now().timestamp())
}

fn rotate_at(
    conn: &mut Connection,
    key: &[u8],
    user_id: i64,
    id: &str,
    now: i64,
) -> Result<IssuedToken> {
    let tx = conn.transaction()?;
    let old = token_info(&tx, user_id, id)?.ok_or("token not found")?;
    if old.revoked_at.is_some() || old.expires_at <= now {
        return Err("token inactive".into());
    }
    let issued = create_token(&tx, key, user_id, &old.name, 90)?;
    tx.execute(
        "UPDATE access_tokens SET expires_at=MIN(expires_at,?1) WHERE id=?2",
        params![now + 86_400, id],
    )?;
    tx.commit()?;
    Ok(issued)
}

pub fn authenticate(conn: &Connection, key: &[u8], bearer: &str) -> Result<Option<(i64, String)>> {
    authenticate_at(conn, key, bearer, Utc::now().timestamp())
}

fn authenticate_at(
    conn: &Connection,
    key: &[u8],
    bearer: &str,
    now: i64,
) -> Result<Option<(i64, String)>> {
    let Some((id, hex_part)) = bearer.strip_prefix("exr_").and_then(|s| s.rsplit_once('_')) else {
        return Ok(None);
    };
    if id.len() != 20
        || !id.starts_with("tok_")
        || !id[4..].bytes().all(|b| b.is_ascii_hexdigit())
        || hex_part.len() != 64
        || !hex_part.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(None);
    }
    let found = conn.query_row(
        "SELECT user_id,digest FROM access_tokens WHERE id=?1 AND revoked_at IS NULL AND expires_at>?2",
        params![id, now], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
    ).optional()?;
    let Some((user_id, stored_digest)) = found else {
        return Ok(None);
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(bearer.as_bytes());
    if mac.verify_slice(&stored_digest).is_err() {
        return Ok(None);
    }
    conn.execute(
        "UPDATE access_tokens SET last_used_at=?1 WHERE id=?2",
        params![now, id],
    )?;
    Ok(Some((user_id, id.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init;

    #[test]
    fn tokens_expire_at_exact_boundary_and_rotation_does_not_extend_old_ttl() {
        let mut db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "alice").unwrap();
        let token = create_token(&db, &[1; 32], user, "laptop", 2).unwrap();
        assert!(
            authenticate_at(&db, &[1; 32], &token.secret, token.token.expires_at - 1)
                .unwrap()
                .is_some()
        );
        assert!(
            authenticate_at(&db, &[1; 32], &token.secret, token.token.expires_at)
                .unwrap()
                .is_none()
        );
        let now = token.token.created_at;
        rotate_at(&mut db, &[1; 32], user, &token.token.id, now).unwrap();
        assert_eq!(
            token_info(&db, user, &token.token.id)
                .unwrap()
                .unwrap()
                .expires_at,
            now + 86_400
        );
        assert!(authenticate_at(&db, &[1; 32], &token.secret, now + 86_400)
            .unwrap()
            .is_none());
        let short = create_token(&db, &[1; 32], user, "short", 1).unwrap();
        rotate_at(
            &mut db,
            &[1; 32],
            user,
            &short.token.id,
            short.token.created_at + 60,
        )
        .unwrap();
        assert_eq!(
            token_info(&db, user, &short.token.id)
                .unwrap()
                .unwrap()
                .expires_at,
            short.token.expires_at
        );
    }

    #[test]
    fn invalid_rotations_do_not_issue_tokens_or_change_expiry() {
        let mut db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let alice = create_user(&db, "alice").unwrap();
        let bob = create_user(&db, "bob").unwrap();
        let token = create_token(&db, &[1; 32], alice, "laptop", 1).unwrap();
        assert!(rotate_at(
            &mut db,
            &[1; 32],
            bob,
            &token.token.id,
            token.token.created_at
        )
        .is_err());
        assert!(rotate_at(
            &mut db,
            &[1; 32],
            alice,
            &token.token.id,
            token.token.expires_at
        )
        .is_err());
        assert_eq!(list_tokens(&db, alice).unwrap().len(), 1);
        assert_eq!(
            token_info(&db, alice, &token.token.id)
                .unwrap()
                .unwrap()
                .expires_at,
            token.token.expires_at
        );
        revoke_token(&db, alice, &token.token.id).unwrap();
        assert!(rotate_at(
            &mut db,
            &[1; 32],
            alice,
            &token.token.id,
            token.token.created_at
        )
        .is_err());
        assert!(create_token(&db, &[1; 32], alice, "laptop", 0).is_err());
        assert!(create_token(&db, &[1; 32], alice, "laptop", 366).is_err());
    }
}
