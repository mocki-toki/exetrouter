//! Operator-only credentials and the Codex device-code grant. The distinct
//! ChatGPT plan token-sharing grant is deliberately not used here.
use crate::{store::Database, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use chrono::Utc;
use rand::{rngs::OsRng, RngCore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

// Public OAuth client identifier used by Codex 0.159.3 and OpenCode 2.0.21.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";

#[derive(Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Debug, Serialize)]
pub struct AccountInfo {
    pub id: i64,
    pub account_id: String,
    pub state: String,
    pub expires_at: i64,
    pub generation: i64,
    pub catalog_updated_at: Option<i64>,
    pub cooldown_until: Option<i64>,
    pub cooldown_source: Option<String>,
    pub health_until: Option<i64>,
    pub health_reason: Option<String>,
}

pub struct Account {
    pub info: AccountInfo,
    pub credentials: Credentials,
}

#[derive(Clone)]
pub struct Vault(Arc<[u8; 32]>);

impl Vault {
    pub fn new(key: [u8; 32]) -> Self {
        Self(Arc::new(key))
    }

    fn encrypt(&self, account_id: &str, credentials: &Credentials) -> Result<Vec<u8>> {
        let cipher = XChaCha20Poly1305::new(self.0.as_ref().into());
        let mut nonce = [0; 24];
        OsRng.fill_bytes(&mut nonce);
        let plaintext = serde_json::to_vec(credentials)?;
        let encrypted = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: account_id.as_bytes(),
                },
            )
            .map_err(|_| "OAuth encryption failed")?;
        let mut result = nonce.to_vec();
        result.extend(encrypted);
        Ok(result)
    }

    fn decrypt(&self, account_id: &str, encrypted: &[u8]) -> Result<Credentials> {
        if encrypted.len() < 40 {
            return Err("invalid encrypted OAuth credentials".into());
        }
        let cipher = XChaCha20Poly1305::new(self.0.as_ref().into());
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&encrypted[..24]),
                Payload {
                    msg: &encrypted[24..],
                    aad: account_id.as_bytes(),
                },
            )
            .map_err(|_| "OAuth decryption failed; check the OAuth key")?;
        serde_json::from_slice(&plaintext).map_err(|_| "invalid OAuth credentials".into())
    }
}

pub fn list(conn: &Connection) -> Result<Vec<AccountInfo>> {
    let mut stmt = conn.prepare("SELECT id,account_id,state,expires_at,generation,catalog_updated_at,cooldown_until,cooldown_source,(SELECT MAX(h.retry_at) FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.failures>0),(SELECT reason FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.failures>0 ORDER BY retry_at DESC LIMIT 1) FROM oauth_accounts a ORDER BY id")?;
    let rows = stmt
        .query_map([], info)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Operator report: expose account metadata and email, never credentials.
pub fn list_with_email(conn: &Connection, vault: &Vault) -> Result<Vec<Value>> {
    list(conn)?
        .into_iter()
        .map(|info| {
            let account = load(conn, vault, info.id)?;
            let mut value = serde_json::to_value(info)?;
            value["email"] = json!(token_email(&account.credentials.access_token));
            Ok(value)
        })
        .collect()
}

fn info(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccountInfo> {
    Ok(AccountInfo {
        id: row.get(0)?,
        account_id: row.get(1)?,
        state: row.get(2)?,
        expires_at: row.get(3)?,
        generation: row.get(4)?,
        catalog_updated_at: row.get(5)?,
        cooldown_until: row.get(6)?,
        cooldown_source: row.get(7)?,
        health_until: row.get(8)?,
        health_reason: row.get(9)?,
    })
}

pub fn load(conn: &Connection, vault: &Vault, id: i64) -> Result<Account> {
    let row = conn.query_row("SELECT id,account_id,state,expires_at,generation,catalog_updated_at,cooldown_until,cooldown_source,(SELECT MAX(h.retry_at) FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.failures>0),(SELECT reason FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.failures>0 ORDER BY retry_at DESC LIMIT 1),encrypted_credentials FROM oauth_accounts a WHERE id=?1", [id], |row| Ok((info(row)?, row.get::<_, Vec<u8>>(10)?))).optional()?.ok_or("OAuth account not found")?;
    let credentials = vault.decrypt(&row.0.account_id, &row.1)?;
    Ok(Account {
        info: row.0,
        credentials,
    })
}

pub fn save(
    conn: &Connection,
    vault: &Vault,
    account_id: &str,
    credentials: &Credentials,
    expires_at: i64,
) -> Result<i64> {
    if account_id.is_empty()
        || account_id.len() > 256
        || account_id.chars().any(char::is_control)
        || credentials.access_token.is_empty()
        || credentials.refresh_token.is_empty()
    {
        return Err("invalid OAuth account".into());
    }
    let encrypted = vault.encrypt(account_id, credentials)?;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES(?1,'active',?2,?3,?4)
        ON CONFLICT(account_id) DO UPDATE SET state='active',encrypted_credentials=excluded.encrypted_credentials,expires_at=excluded.expires_at,generation=generation+1,refresh_owner=NULL,refresh_until=NULL,catalog_updated_at=NULL", params![account_id,encrypted,expires_at,Utc::now().timestamp()])?;
    let id = tx.query_row(
        "SELECT id FROM oauth_accounts WHERE account_id=?1",
        [account_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "DELETE FROM oauth_auth_rejections WHERE account_id=?1",
        [id],
    )?;
    tx.execute("DELETE FROM oauth_health WHERE account_id=?1", [id])?;
    tx.commit()?;
    Ok(id)
}

pub fn disable(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("UPDATE oauth_accounts SET state='disabled',generation=generation+1,refresh_owner=NULL,refresh_until=NULL WHERE id=?1", [id])? == 1)
}

pub async fn access(
    db: &Database,
    vault: &Vault,
    client: &reqwest::Client,
    issuer: &str,
    id: i64,
) -> Result<Account> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let vault_read = vault.clone();
        let account = db
            .call(move |conn| {
                if crate::health::paused(conn, id, Utc::now().timestamp())?.is_some() {
                    return Err("OAuth account temporarily unavailable".into());
                }
                let account = load(conn, &vault_read, id)?;
                if account.info.state != "active" {
                    return Err("OAuth account is not active".into());
                }
                Ok(account)
            })
            .await?;
        if account.info.expires_at > Utc::now().timestamp() + 60 {
            return Ok(account);
        }
        let id = account.info.id;
        let generation = account.info.generation;
        let owner = random_id();
        let lease_owner = owner.clone();
        let claimed = db.call(move |conn| {
            let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
            let now=Utc::now().timestamp();
            let changed=tx.execute("UPDATE oauth_accounts SET refresh_owner=?1,refresh_until=?2 WHERE id=?3 AND generation=?4 AND state='active' AND (refresh_until IS NULL OR refresh_until<?5) AND NOT EXISTS(SELECT 1 FROM oauth_health h WHERE h.account_id=?3 AND h.generation=?4 AND h.retry_at>?5)",params![lease_owner,now+60,id,generation,now])?;
            let order=if changed==1 {Some(crate::health::next(&tx)?)} else {None};
            tx.commit()?; Ok(order)
        }).await?;
        let Some(health_order) = claimed else {
            if tokio::time::Instant::now() >= deadline {
                return Err("OAuth refresh is busy".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let refreshed = refresh(client, issuer, &account.credentials.refresh_token).await;
        let vault_write = vault.clone();
        let account_id = account.info.account_id;
        db.call(move |conn| {
            // Fence the UPDATE itself: a separate SELECT cannot protect
            // against an operator reauthorization in another process.
            let refreshed=refreshed.and_then(|(credentials,expires_at)| {
                let mismatch=token_account_id(&credentials.access_token).is_some_and(|id|id!=account_id);
                if mismatch { Err((true, None)) } else { Ok((credentials,expires_at)) }
            });
            match refreshed {
                Ok((credentials, expires_at)) => {
                    let encrypted = vault_write.encrypt(&account_id, &credentials)?;
                    let changed=conn.execute("UPDATE oauth_accounts SET encrypted_credentials=?1,expires_at=?2,generation=generation+1,refresh_owner=NULL,refresh_until=NULL WHERE id=?3 AND generation=?4 AND refresh_owner=?5 AND state='active'", params![encrypted,expires_at,id,generation,owner])?;
                    Ok(changed==1)
                }
                Err((permanent, retry)) => {
                    // Publish the pause atomically with releasing the lease so
                    // another worker cannot start a refresh in between.
                    let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
                    let changed=tx.execute("UPDATE oauth_accounts SET state=CASE WHEN ?1 THEN 'reauth_required' ELSE state END,refresh_owner=NULL,refresh_until=NULL WHERE id=?2 AND generation=?3 AND refresh_owner=?4 AND state='active'", params![permanent,id,generation,owner])?;
                    if changed==0 { return Ok(false); }
                    if !permanent { crate::health::failure_in_transaction(&tx,(id,generation),crate::health::Scope::Refresh,health_order,"refresh",retry,Utc::now().timestamp())?; }
                    tx.commit()?;
                    Err(if permanent { "OAuth reauthorization required" } else { "OAuth refresh unavailable" }.into())
                }
            }
        }).await?;
        // Reload after a refresh or a competing operator reauthorization.
    }
}

async fn refresh(
    client: &reqwest::Client,
    issuer: &str,
    old_refresh: &str,
) -> std::result::Result<(Credentials, i64), (bool, Option<i64>)> {
    let response = client
        .post(format!("{issuer}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", old_refresh),
        ])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| (false, None))?;
    if !response.status().is_success() {
        let status = response.status();
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| crate::quota::retry_at(v, Utc::now().timestamp()));
        let body = read_json(response, 65_536).await.ok();
        let code = body
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.as_str().or_else(|| e.get("code").and_then(Value::as_str)));
        return Err((
            status.as_u16() == 401
                || matches!(
                    code,
                    Some(
                        "invalid_grant"
                            | "refresh_token_expired"
                            | "refresh_token_reused"
                            | "refresh_token_invalidated"
                    )
                ),
            retry,
        ));
    }
    let value = read_json(response, 65_536)
        .await
        .map_err(|_| (false, None))?;
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or((false, None))?;
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or(old_refresh);
    Ok((
        Credentials {
            access_token: access.into(),
            refresh_token: refresh.into(),
        },
        expires_at(&value, access).map_err(|_| (false, None))?,
    ))
}

pub struct DeviceCode {
    pub verification_url: String,
    pub user_code: String,
    device_auth_id: String,
    interval: u64,
}

pub async fn begin_device(client: &reqwest::Client, issuer: &str) -> Result<DeviceCode> {
    let response = client
        .post(format!("{issuer}/api/accounts/deviceauth/usercode"))
        .json(&json!({"client_id":CLIENT_ID}))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| "device authorization unavailable")?;
    if !response.status().is_success() {
        return Err("device authorization rejected".into());
    }
    let value = read_json(response, 65_536).await?;
    let user_code = value
        .get("user_code")
        .or_else(|| value.get("usercode"))
        .and_then(Value::as_str)
        .ok_or("invalid device authorization reply")?
        .to_owned();
    if user_code.len() > 64 || user_code.chars().any(char::is_control) {
        return Err("invalid device code".into());
    }
    let device_auth_id = value
        .get("device_auth_id")
        .and_then(Value::as_str)
        .ok_or("missing device authorization ID")?
        .to_owned();
    let interval = value
        .get("interval")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(5)
        .clamp(1, 60);
    Ok(DeviceCode {
        verification_url: format!("{issuer}/codex/device"),
        user_code,
        device_auth_id,
        interval,
    })
}

pub async fn complete_device(
    client: &reqwest::Client,
    issuer: &str,
    code: DeviceCode,
) -> Result<(String, Credentials, i64)> {
    tokio::time::timeout(Duration::from_secs(900), async {
        let authorized = loop {
            let response = client
                .post(format!("{issuer}/api/accounts/deviceauth/token"))
                .json(&json!({"device_auth_id":code.device_auth_id,"user_code":code.user_code}))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|_| "device poll unavailable")?;
            match response.status().as_u16() {
                200..=299 => break read_json(response, 65_536).await?,
                403 | 404 => tokio::time::sleep(Duration::from_secs(code.interval)).await,
                _ => return Err("device authorization rejected".into()),
            }
        };
        let authorization_code = authorized
            .get("authorization_code")
            .and_then(Value::as_str)
            .ok_or("missing authorization code")?;
        let verifier = authorized
            .get("code_verifier")
            .and_then(Value::as_str)
            .ok_or("missing PKCE verifier")?;
        let redirect = format!("{issuer}/deviceauth/callback");
        let response = client
            .post(format!("{issuer}/oauth/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", authorization_code),
                ("code_verifier", verifier),
                ("redirect_uri", &redirect),
            ])
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| "OAuth exchange unavailable")?;
        if !response.status().is_success() {
            return Err("OAuth exchange rejected".into());
        }
        let value = read_json(response, 65_536).await?;
        let access = value
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or("missing OAuth access token")?;
        let refresh = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .ok_or("missing OAuth refresh token")?;
        // These claims arrive directly from the TLS-authenticated issuer, not
        // from an untrusted caller. They identify storage; they grant no access.
        let account_id = value
            .get("id_token")
            .and_then(Value::as_str)
            .and_then(token_account_id)
            .or_else(|| token_account_id(access))
            .ok_or("OAuth account ID missing")?;
        Ok((
            account_id,
            Credentials {
                access_token: access.into(),
                refresh_token: refresh.into(),
            },
            expires_at(&value, access)?,
        ))
    })
    .await
    .map_err(|_| "device authorization timed out")?
}

/// Display metadata from issuer-provided credentials; never an authorization claim.
pub(crate) fn token_email(token: &str) -> Option<String> {
    let claims = claims(token).ok()?;
    claims
        .pointer("/https:~1~1api.openai.com~1profile/email")
        .or_else(|| claims.get("email"))
        .and_then(Value::as_str)
        .filter(|email| {
            email.contains('@') && email.len() <= 320 && !email.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn token_account_id(token: &str) -> Option<String> {
    let claims = claims(token).ok()?;
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .map(str::to_owned)
}

fn claims(token: &str) -> Result<Value> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or("invalid OAuth token claims")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "invalid OAuth token claims")?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid OAuth token claims".into())
}

fn expires_at(response: &Value, access: &str) -> Result<i64> {
    if let Some(seconds) = response
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|s| *s > 60 && *s < 31_536_000)
    {
        return Ok(Utc::now().timestamp() + seconds);
    }
    claims(access)?
        .get("exp")
        .and_then(Value::as_i64)
        .filter(|t| *t > Utc::now().timestamp() + 60)
        .ok_or("OAuth expiry missing or invalid".into())
}

pub(crate) async fn read_json(mut response: reqwest::Response, limit: usize) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "upstream read failed")? {
        if bytes.len() + chunk.len() > limit {
            return Err("upstream reply too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid upstream JSON".into())
}

fn random_id() -> String {
    let mut bytes = [0; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_list_includes_email_without_credentials() {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        let vault = Vault::new([1; 32]);
        let claims = URL_SAFE_NO_PAD.encode(br#"{"email":"alice@example.com"}"#);
        let access_token = format!("header.{claims}.signature");
        let credentials = Credentials {
            access_token: access_token.clone(),
            refresh_token: "private-refresh-canary".into(),
        };
        let id = save(&conn, &vault, "account-a", &credentials, 100).unwrap();
        let rows = list_with_email(&conn, &vault).unwrap();
        assert_eq!(rows[0]["email"], "alice@example.com");
        assert_eq!(rows[0]["id"], id);
        let report = serde_json::to_string(&rows).unwrap();
        assert!(!report.contains(&access_token));
        assert!(!report.contains("private-refresh-canary"));
        assert!(!report.contains("encrypted_credentials"));
        save(
            &conn,
            &vault,
            "account-b",
            &Credentials {
                access_token: "no-email".into(),
                refresh_token: "private-refresh".into(),
            },
            100,
        )
        .unwrap();
        assert!(list_with_email(&conn, &vault).unwrap()[1]["email"].is_null());
    }
    #[test]
    fn vault_authenticates_ciphertext_and_account_identity() {
        let vault = Vault::new([1; 32]);
        let credentials = Credentials {
            access_token: "private-access".into(),
            refresh_token: "private-refresh".into(),
        };
        let mut ciphertext = vault.encrypt("account-a", &credentials).unwrap();
        assert!(!ciphertext.windows(14).any(|w| w == b"private-access"));
        assert_eq!(
            vault
                .decrypt("account-a", &ciphertext)
                .unwrap()
                .refresh_token,
            "private-refresh"
        );
        assert!(vault.decrypt("account-b", &ciphertext).is_err());
        assert!(Vault::new([2; 32])
            .decrypt("account-a", &ciphertext)
            .is_err());
        ciphertext[25] ^= 1;
        assert!(vault.decrypt("account-a", &ciphertext).is_err());
    }
    #[test]
    fn reauthorization_updates_one_account_and_preserves_identity() {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        let vault = Vault::new([1; 32]);
        let credentials = Credentials {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
        };
        let id = save(&conn, &vault, "account-a", &credentials, 100).unwrap();
        disable(&conn, id).unwrap();
        assert_eq!(
            save(&conn, &vault, "account-a", &credentials, 200).unwrap(),
            id
        );
        let accounts = list(&conn).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].generation, 2);
        assert_eq!(accounts[0].state, "active");
        assert!(!serde_json::to_string(&accounts)
            .unwrap()
            .contains("refresh"));
    }
}
