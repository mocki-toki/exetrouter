//! Operator-only Platform credential provisioning. Allowances and observed costs
//! are not a balance; the provider is authoritative about available funds.
mod client;
pub(crate) use client::BASE;
#[cfg(test)]
pub(crate) fn validate_mock_endpoint(base: &str) -> Result<()> {
    client::Client::new(base, true).map(|_| ())
}

/// Translate router/native routing fields, never conversation content. Unknown
/// inference options still go to the provider for validation.
pub(crate) fn inference_payload(
    body: &serde_json::Value,
    compact: bool,
) -> Result<serde_json::Value> {
    let mut body = body.clone();
    let object = body.as_object_mut().ok_or("invalid Platform request")?;
    if object.get("generate") == Some(&serde_json::Value::Bool(false)) {
        return Err("Platform HTTP does not support non-generating warmup".into());
    }
    if object
        .get("type")
        .is_some_and(|value| value == "response.create")
    {
        object.remove("type");
    }
    object.remove("generate");
    if let Some(metadata) = object
        .get_mut("client_metadata")
        .and_then(serde_json::Value::as_object_mut)
    {
        for name in [
            "session_id",
            "thread_id",
            "x-codex-turn-state",
            "x-codex-turn-metadata",
        ] {
            metadata.remove(name);
        }
        if metadata.is_empty() {
            object.remove("client_metadata");
        }
    }
    if compact {
        let items = object
            .get_mut("input")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("compaction requires full input")?;
        if items
            .last()
            .is_some_and(|item| item["type"] == "compaction_trigger")
        {
            items.pop();
        }
        object.remove("stream");
        if object.get("store") == Some(&serde_json::Value::Bool(false)) {
            object.remove("store");
        }
    }
    if body["input"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
    }) {
        return Err("Codex compaction triggers cannot be sent to Platform Responses".into());
    }
    Ok(body)
}

/// Cross-backend fallback needs self-contained text/tool input, not opaque
/// state, project-owned files/containers or saved backend history.
pub(crate) fn transfer_safe(payload: &serde_json::Value) -> bool {
    fn input_safe(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(fields) => fields.iter().all(|(name, value)| {
                if name == "encrypted_function_args" && value.as_array().is_some_and(Vec::is_empty)
                {
                    return true;
                }
                if matches!(
                    name.as_str(),
                    "encrypted_content" | "encrypted_function_args" | "file_id" | "container_id"
                ) && !value.is_null()
                {
                    return false;
                }
                if name == "type" && value == "item_reference" {
                    return false;
                }
                input_safe(value)
            }),
            serde_json::Value::Array(items) => items.iter().all(input_safe),
            _ => true,
        }
    }
    payload["conversation"].is_null() && input_safe(&payload["input"])
}
pub mod balance;
pub mod costs;
#[cfg(test)]
#[path = "platform/tests.rs"]
mod integration_tests;
mod inventory;
mod keys;
pub(crate) mod routes;
use crate::{oauth::Vault, Result};
use futures_util::StreamExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) async fn route_catalog(
    db: &crate::store::Database,
    vault: &Vault,
    base: &str,
) -> Result<Vec<crate::catalog::Model>> {
    let accounts=db.call(|conn| {
        let mut stmt=conn.prepare("SELECT k.account_id FROM platform_working_keys k JOIN account_platform_credentials c USING(account_id) JOIN platform_settings s ON s.account_id=k.account_id AND s.project_id=k.project_id JOIN oauth_accounts a ON a.id=k.account_id LEFT JOIN account_policy p ON p.account_id=a.id WHERE k.generation=c.generation AND k.state='active' AND k.expires_at>?1 AND a.state='active' AND COALESCE(p.enabled,1)=1 ORDER BY k.account_id")?;
        let rows=stmt.query_map([chrono::Utc::now().timestamp()],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(rows)
    }).await?;
    let client = client::Client::new(base, base != BASE)?;
    let results=futures_util::stream::iter(accounts).map(|account| {
        let vault=vault.clone();let client=client.clone();let db=db.clone();
        async move {
            let check_vault=vault.clone();
            let cached=db.call(move|conn| {
                let owner=conn.query_row("SELECT -r.id FROM platform_routes r JOIN platform_settings s ON s.account_id=r.account_id AND s.project_id=r.project_id WHERE r.account_id=?1",[account],|r|r.get::<_,i64>(0)).optional()?;
                if let Some(owner)=owner {
                    if routes::account(conn,&check_vault,owner,chrono::Utc::now().timestamp()).is_ok() && routes::models(conn,owner,chrono::Utc::now().timestamp()).is_ok() {return Ok(true);}
                }
                Ok(false)
            }).await?;
            if !cached {inventory::read(&db,vault,&client,account).await?;}
            Ok::<(),Box<dyn std::error::Error+Send+Sync>>(())
        }
    }).buffer_unordered(4).collect::<Vec<_>>().await;
    for result in results {
        if result.is_err() {
            tracing::warn!(event = "platform_catalog_unavailable");
        }
    }
    let vault = vault.clone();
    db.call(move |conn| {
        let now = chrono::Utc::now().timestamp();
        let mut models = std::collections::BTreeMap::new();
        for owner in routes::owners(conn)? {
            if routes::account(conn, &vault, owner, now).is_err() {
                continue;
            }
            if let Ok(entries) = routes::models(conn, owner, now) {
                for entry in entries {
                    if routes::model_spending_permitted(conn, &entry.id)? {
                        models.entry(entry.id.clone()).or_insert(entry);
                    }
                }
            }
        }
        Ok(models.into_values().collect())
    })
    .await
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Replace the manual balance and restart both estimates from this moment.
    SetBalance {
        id: i64,
        /// Current remaining USD amount, for example 75.25.
        #[arg(long = "usd", value_name = "USD", value_parser = crate::subscription::parse_usd)]
        cents: u64,
    },
    /// Read model availability with the stored working key; never submits inference.
    Models { id: i64 },
    /// Permit/deny future Platform spending for Codex catalog models (default: false).
    CodexFallback {
        #[arg(action=clap::ArgAction::Set)]
        enabled: bool,
    },
    /// Bind an existing Platform project; optional automatic thirty-day key management.
    Configure {
        id: i64,
        #[arg(long)]
        project_id: String,
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
        manage_keys: bool,
    },
    /// Refresh real organization costs and configured working-key metadata (no inference).
    Refresh { id: i64 },
    /// Store or replace an Admin API Key from a private, owner-only file.
    SetAdminKey {
        id: i64,
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Remove local Platform secrets/history; does not revoke upstream keys.
    RemoveAdminKey { id: i64 },
    /// Inspect configuration without contacting Platform or revealing secrets.
    Status { id: i64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub configured_at: i64,
    pub inference_enabled: bool,
    pub credit_status: String,
    #[serde(default)]
    pub codex_fallback_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs: Option<costs::Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<balance::Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<keys::Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_error: Option<String>,
}

pub fn render(value: &serde_json::Value) -> String {
    if value.get("configured_at").is_none() && value.get("codex_fallback_allowed").is_some() {
        return format!(
            "Codex models may use Platform API credits: {}",
            if value["codex_fallback_allowed"] == true {
                "allowed"
            } else {
                "disabled"
            }
        );
    }
    if value.is_null() {
        return "No Platform credentials configured.".into();
    }
    if value["object"] == "list" && value.get("routing_enabled").is_some() {
        return inventory::render(value);
    }
    if value.get("removed").is_some() {
        return if value["removed"] == true {
            "Stored Admin Key removed. No upstream key was revoked."
        } else {
            "No stored Admin Key to remove. No upstream key was revoked."
        }
        .into();
    }
    let mut text = format!(
        "Platform API: Admin Key configured\nProvider balance: unavailable · API routing {}",
        if value["inference_enabled"] == true {
            "ready"
        } else {
            "not ready"
        }
    );
    if value["costs"].is_object() {
        let costs = &value["costs"];
        let spent = costs["spent_usd_nanos"]
            .as_i64()
            .map(|n| crate::money::usd(n as f64 / costs::NANOS as f64))
            .unwrap_or_else(|| "Unknown".into());
        let budget = costs["budget_cents"]
            .as_u64()
            .map(|n| crate::money::usd(n as f64 / 100.))
            .unwrap_or_else(|| "Unknown".into());
        text.push_str(&format!("\nOrganization costs: {spent} / {budget} reference · UTC reporting window (not a remaining balance)"));
    }
    text.push_str(&format!("\n{}", balance::detail(value)));
    if let Some(state) = value["key"]["state"].as_str() {
        // Only fixed local status strings, never provider text.
        if [
            "active",
            "expired",
            "inactive",
            "missing",
            "operation_uncertain",
        ]
        .contains(&state)
        {
            text.push_str(&format!("\nWorking key: {state}"));
        }
        let date = |value: &serde_json::Value| {
            use chrono::Datelike;
            value
                .as_i64()
                .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                .map(|at| {
                    let local = at.with_timezone(&chrono::Local);
                    local
                        .format(if local.year() == chrono::Local::now().year() {
                            "%b %-d"
                        } else {
                            "%b %-d, %Y"
                        })
                        .to_string()
                })
                .unwrap_or_else(|| "Unknown".into())
        };
        text.push_str(&format!(
            "\nCreated {} · Expires {} · Used {}",
            date(&value["key"]["created_at"]),
            date(&value["key"]["expires_at"]),
            if value["key"]["last_used_at"].is_null() {
                "Never".into()
            } else {
                date(&value["key"]["last_used_at"])
            }
        ));
    }
    text.push_str(if value["codex_fallback_allowed"] == true {
        "\nCodex fallback policy: allowed"
    } else {
        "\nCodex fallback policy: disabled"
    });
    if value["refresh_error"].is_string() {
        text.push_str("\nPlatform refresh unavailable; observations may be historical.");
    }
    text
}

fn aad(account: i64) -> String {
    format!("exetrouter:platform-admin:v1:{account}")
}

fn validate(key: &[u8]) -> Result<()> {
    if !(24..=512).contains(&key.len())
        || !key.starts_with(b"sk-admin-")
        || !key
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(c))
    {
        return Err("invalid Admin API Key format".into());
    }
    Ok(())
}

/// Never accept secret values as CLI arguments, environment, JSON or reports.
/// Open first, then validate the handle to avoid symlink and replacement races.
fn read_key(path: &Path) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "cannot open the private Admin Key file")?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Admin Key file must be regular, owned by you and private (mode 600)".into());
    }
    let mut key = Vec::new();
    (&mut file).take(515).read_to_end(&mut key)?;
    if key.ends_with(b"\r\n") {
        key.truncate(key.len() - 2);
    } else if key.ends_with(b"\n") {
        key.pop();
    }
    validate(&key)?;
    Ok(key)
}

pub fn configure(
    conn: &Connection,
    vault: &Vault,
    account: i64,
    key: &[u8],
    now: i64,
) -> Result<Status> {
    validate(key)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1)",
        [account],
        |r| r.get(0),
    )?;
    if !exists {
        return Err("account not found".into());
    }
    let encrypted = vault.seal_secret(aad(account).as_bytes(), key)?;
    conn.execute("INSERT INTO account_platform_credentials(account_id,encrypted_admin_key,generation,configured_at) VALUES(?1,?2,1,?3)
        ON CONFLICT(account_id) DO UPDATE SET encrypted_admin_key=excluded.encrypted_admin_key,generation=generation+1,configured_at=excluded.configured_at",
        params![account,encrypted,now])?;
    snapshot(conn, account)?.ok_or_else(|| "Platform configuration unavailable".into())
}

pub fn snapshot(conn: &Connection, account: i64) -> Result<Option<Status>> {
    let mut result = conn
        .query_row(
            "SELECT configured_at FROM account_platform_credentials WHERE account_id=?1",
            [account],
            |r| {
                Ok(Status {
                    configured_at: r.get(0)?,
                    inference_enabled: false,
                    credit_status: "unknown".into(),
                    codex_fallback_allowed: false,
                    costs: None,
                    balance: None,
                    key: None,
                    refresh_error: None,
                })
            },
        )
        .optional()?;
    if let Some(status) = &mut result {
        status.codex_fallback_allowed = codex_fallback_allowed(conn)?;
        let now = chrono::Utc::now().timestamp();
        status.costs = Some(costs::snapshot(conn, account, now)?);
        status.balance = balance::snapshot(conn, account, now)?;
        status.key = keys::snapshot(conn, account, now)?;
        status.inference_enabled=conn.query_row("SELECT EXISTS(SELECT 1 FROM platform_routes r JOIN account_platform_credentials c ON c.account_id=r.account_id AND c.generation=r.credential_generation JOIN platform_settings s ON s.account_id=r.account_id AND s.project_id=r.project_id JOIN platform_working_keys k ON k.account_id=r.account_id AND k.generation=c.generation AND k.key_id=r.key_id AND k.project_id=r.project_id JOIN oauth_accounts a ON a.id=r.account_id LEFT JOIN account_policy p ON p.account_id=a.id WHERE r.account_id=?1 AND k.state='active' AND k.expires_at>?2 AND a.state='active' AND COALESCE(p.enabled,1)=1 AND r.observed_at<=?2 AND r.observed_at>?2-?3 AND r.catalog<>'[]' AND NOT EXISTS(SELECT 1 FROM platform_key_operations o WHERE o.account_id=r.account_id AND o.status<>'confirmed'))",params![account,now,crate::catalog::CACHE_TTL_SECONDS],|r|r.get(0))?;
        status.refresh_error=conn.query_row("SELECT error FROM platform_refresh r JOIN account_platform_credentials c USING(account_id) WHERE r.account_id=?1 AND r.generation=c.generation",[account],|r|r.get::<_,Option<String>>(0)).optional()?.flatten();
    }
    Ok(result)
}

/// Used by backup verification. Decryption never returns a printable report.
pub fn verify_credentials(conn: &Connection, vault: &Vault) -> Result<()> {
    let mut stmt =
        conn.prepare("SELECT account_id,encrypted_admin_key FROM account_platform_credentials")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))? {
        let (account, encrypted) = row?;
        let key = vault.open_secret(aad(account).as_bytes(), &encrypted)?;
        validate(&key)?;
    }
    keys::verify(conn, vault)?;
    Ok(())
}

pub(crate) fn prune(conn: &Connection, now: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM platform_cost_history WHERE day<MAX(?1,COALESCE((SELECT MAX(h.day) FROM platform_cost_history h WHERE h.account_id=platform_cost_history.account_id),?1))-365*86400",
        [now.div_euclid(costs::DAY) * costs::DAY],
    )?;
    conn.execute("UPDATE platform_key_operations SET status='unknown' WHERE status='submitted' AND attempted_at<=?1",[now-120])?;
    Ok(())
}

pub fn set_codex_fallback(conn: &Connection, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE platform_policy SET codex_fallback_allowed=?1 WHERE singleton=1",
        [enabled],
    )?;
    Ok(())
}
pub fn codex_fallback_allowed(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT codex_fallback_allowed FROM platform_policy WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}
/// Available funds may be free or purchased. This policy only controls the
/// Codex-model exception; it does not claim that a key, model or balance is ready.
pub fn spending_permitted(conn: &Connection, codex_model: bool) -> Result<bool> {
    Ok(!codex_model || codex_fallback_allowed(conn)?)
}

pub async fn command(
    db: &crate::store::Database,
    vault: Vault,
    command: &Command,
) -> Result<serde_json::Value> {
    match command {
        Command::SetBalance { id, cents } => {
            let (id, cents) = (*id, *cents);
            db.call(move |conn| {
                balance::set(conn, id, cents, chrono::Utc::now().timestamp())?;
                Ok(())
            })
            .await?;
            Ok(serde_json::to_value(
                db.call(move |conn| snapshot(conn, id)).await?,
            )?)
        }
        Command::Models { id } => {
            inventory::read(db, vault, &client::Client::new(client::BASE, false)?, *id).await
        }
        Command::CodexFallback { enabled } => {
            let enabled = *enabled;
            db.call(move |conn| set_codex_fallback(conn, enabled))
                .await?;
            Ok(serde_json::json!({"codex_fallback_allowed":enabled}))
        }
        Command::Configure {
            id,
            project_id,
            manage_keys,
        } => {
            if !client::identifier(project_id) {
                return Err("invalid Platform project ID".into());
            }
            let id = *id;
            let project = project_id.clone();
            let manage = *manage_keys;
            db.call(move|conn| {
                let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
                if tx.execute("UPDATE account_platform_credentials SET generation=generation+1 WHERE account_id=?1",[id])?!=1 {return Err("configure an Admin Key first".into());}
                tx.execute("INSERT INTO platform_settings(account_id,project_id,manage_keys) VALUES(?1,?2,?3) ON CONFLICT(account_id) DO UPDATE SET project_id=excluded.project_id,manage_keys=excluded.manage_keys",params![id,project,manage])?;
                tx.commit()?;Ok(())
            }).await?;
            Ok(serde_json::to_value(
                db.call(move |conn| snapshot(conn, id)).await?,
            )?)
        }
        Command::Refresh { id } => {
            client::refresh(
                db,
                vault,
                &client::Client::new(client::BASE, false)?,
                *id,
                true,
            )
            .await?;
            let id = *id;
            Ok(serde_json::to_value(
                db.call(move |conn| snapshot(conn, id)).await?,
            )?)
        }
        Command::SetAdminKey { id, key_file } => {
            let key = read_key(key_file)?;
            let id = *id;
            Ok(serde_json::to_value(
                db.call(move |conn| {
                    configure(conn, &vault, id, &key, chrono::Utc::now().timestamp())
                })
                .await?,
            )?)
        }
        Command::RemoveAdminKey { id } => {
            let id = *id;
            let removed = db
                .call(move |conn| {
                    Ok(conn.execute(
                        "DELETE FROM account_platform_credentials WHERE account_id=?1",
                        [id],
                    )? == 1)
                })
                .await?;
            Ok(serde_json::json!({"removed":removed,"upstream_revoked":false}))
        }
        Command::Status { id } => {
            let id = *id;
            Ok(serde_json::to_value(
                db.call(move |conn| snapshot(conn, id)).await?,
            )?)
        }
    }
}

impl crate::upstream::Upstream {
    pub(crate) async fn maintain_accounts(&self, stop: tokio::sync::watch::Receiver<bool>) {
        tokio::join!(
            self.automatic_accounts(stop.clone()),
            client::worker(self.db.clone(), self.vault.clone(), stop)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    const KEY: &[u8] = concat!("sk-admin-", "synthetic-never-valid-test-key").as_bytes();
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        crate::init(&db).unwrap();
        for id in ["synthetic-a", "synthetic-b"] {
            db.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES(?1,'active',X'00',9999999999,0)", [id]).unwrap();
        }
        db
    }
    #[test]
    fn secret_is_encrypted_account_bound_and_does_not_enable_inference() {
        let db = database();
        let vault = Vault::new([1; 32]);
        assert!(snapshot(&db, 1).unwrap().is_none());
        let status = configure(&db, &vault, 1, KEY, 10).unwrap();
        assert!(!status.inference_enabled);
        assert_eq!(status.credit_status, "unknown");
        assert!(!serde_json::to_string(&status).unwrap().contains("sk-"));
        let encrypted: Vec<u8> = db
            .query_row(
                "SELECT encrypted_admin_key FROM account_platform_credentials",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!encrypted.windows(KEY.len()).any(|w| w == KEY));
        verify_credentials(&db, &vault).unwrap();
        assert!(verify_credentials(&db, &Vault::new([2; 32])).is_err());
        assert!(vault.open_secret(aad(2).as_bytes(), &encrypted).is_err());
        assert!(vault.open_secret(b"synthetic-a", &encrypted).is_err());
        configure(&db, &vault, 1, KEY, 20).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT generation FROM account_platform_credentials",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        db.pragma_update(None, "user_version", 16).unwrap();
        crate::init(&db).unwrap();
        verify_credentials(&db, &vault).unwrap();
        assert_eq!(snapshot(&db, 1).unwrap().unwrap().configured_at, 20);
        assert!(configure(&db, &vault, 99, KEY, 20).is_err());
    }
    #[test]
    fn private_input_rejects_symlinks_permissions_invalid_and_oversized_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, KEY).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_key(&path).unwrap(), KEY);
        let link = dir.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_key(&link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_key(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        for invalid in [
            b"sk-admin-invalid\n\n".to_vec(),
            vec![b'x'; 10000],
            concat!("sk-proj-", "synthetic-not-an-admin-key")
                .as_bytes()
                .to_vec(),
        ] {
            std::fs::write(&path, &invalid).unwrap();
            let err = read_key(&path).unwrap_err().to_string();
            assert!(!err.contains("sk-"));
        }
    }
}
