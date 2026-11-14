//! Thirty-day service-account keys. Every POST is durably fenced before send;
//! a lost one-time secret is an unknown outcome, never permission to create again.
use super::client::{current, identifier, Access, Client};
use crate::{oauth::Vault, store::Database, Result};
use reqwest::Method;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const LIFETIME: i64 = 30 * 86400;
// Default membership is established by service-account creation. Its mandatory
// initial key is never used for inference and must expire before a named key is
// provisioned. Keep its one-time secret encrypted until it is replaced.
const BOOTSTRAP_LIFETIME: i64 = 1;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub created_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub checked_at: Option<i64>,
}
#[derive(Clone, PartialEq)]
pub(super) struct KeyIdentity {
    pub id: String,
    pub project: String,
    pub generation: i64,
}
pub(super) struct WorkingKey {
    pub identity: KeyIdentity,
    pub secret: String,
}
fn usable_key(conn: &Connection, account: i64, now: i64) -> Result<(KeyIdentity, Vec<u8>)> {
    conn.query_row("SELECT k.key_id,k.project_id,k.generation,k.encrypted_key FROM platform_working_keys k JOIN account_platform_credentials c USING(account_id) JOIN platform_settings s USING(account_id) JOIN oauth_accounts a ON a.id=k.account_id LEFT JOIN account_policy p ON p.account_id=a.id WHERE k.account_id=?1 AND k.generation=c.generation AND s.project_id=k.project_id AND k.state='active' AND k.expires_at>?2 AND a.state='active' AND COALESCE(p.enabled,1)=1 AND NOT EXISTS(SELECT 1 FROM platform_key_operations o WHERE o.account_id=k.account_id AND o.status!='confirmed')",params![account,now],|r|Ok((KeyIdentity {id:r.get(0)?,project:r.get(1)?,generation:r.get(2)?},r.get(3)?)))
        .optional()?.ok_or_else(|| "no usable Platform working key; configure and refresh it first".into())
}
pub(super) fn working_secret(
    conn: &Connection,
    vault: &Vault,
    account: i64,
    now: i64,
) -> Result<WorkingKey> {
    let (identity, encrypted) = usable_key(conn, account, now)?;
    let bytes = vault.open_secret(
        aad(account, &identity.project, &identity.id).as_bytes(),
        &encrypted,
    )?;
    validate(&bytes)?;
    let secret = String::from_utf8(bytes).map_err(|_| "invalid Platform working key")?;
    Ok(WorkingKey { identity, secret })
}
pub(super) fn current_key(
    conn: &Connection,
    account: i64,
    identity: &KeyIdentity,
    now: i64,
) -> Result<()> {
    if usable_key(conn, account, now)?.0 != *identity {
        return Err("Platform configuration changed during model discovery".into());
    }
    Ok(())
}
fn aad(account: i64, project: &str, key: &str) -> String {
    format!("exetrouter:platform-working:v1:{account}:{project}:{key}")
}
pub(super) fn validate(key: &[u8]) -> Result<()> {
    if !(24..=512).contains(&key.len())
        || !key.starts_with(b"sk-")
        || key.starts_with(b"sk-admin-")
        || !key
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(b))
    {
        return Err("invalid Platform working key".into());
    }
    Ok(())
}
pub(super) fn snapshot(conn: &Connection, account: i64, now: i64) -> Result<Option<Status>> {
    let pending:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM platform_key_operations WHERE account_id=?1 AND status!='confirmed')",[account],|r|r.get(0))?;
    let key=conn.query_row("SELECT k.state,k.created_at,k.expires_at,k.last_used_at,k.checked_at FROM platform_working_keys k JOIN account_platform_credentials c USING(account_id) WHERE k.account_id=?1 AND k.generation=c.generation",[account],|r|Ok(Status {state:r.get(0)?,created_at:r.get(1)?,expires_at:r.get(2)?,last_used_at:r.get(3)?,checked_at:r.get(4)?})).optional()?;
    Ok(if pending {
        Some(Status {
            state: "operation_uncertain".into(),
            created_at: key.as_ref().and_then(|k| k.created_at),
            expires_at: key.as_ref().and_then(|k| k.expires_at),
            last_used_at: key.as_ref().and_then(|k| k.last_used_at),
            checked_at: key.and_then(|k| k.checked_at),
        })
    } else {
        key.map(|mut k| {
            if k.expires_at.is_some_and(|t| t <= now) {
                k.state = "expired".into();
            }
            k
        })
    })
}
pub(super) fn verify(conn: &Connection, vault: &Vault) -> Result<()> {
    let mut stmt = conn
        .prepare("SELECT account_id,project_id,key_id,encrypted_key FROM platform_working_keys")?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Vec<u8>>(3)?,
        ))
    })? {
        let (account, project, id, encrypted) = row?;
        validate(&vault.open_secret(aad(account, &project, &id).as_bytes(), &encrypted)?)?;
    }
    Ok(())
}

async fn begin(db: &Database, access: &Access, operation: &'static str) -> Result<()> {
    let a = access.account;
    let g = access.generation;
    let owner = access.owner.clone();
    let project = access
        .project
        .clone()
        .ok_or("Platform project must be configured")?;
    db.call(move|conn| {
        let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
        let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM account_platform_credentials c JOIN platform_refresh r USING(account_id) JOIN platform_settings s USING(account_id) JOIN oauth_accounts a ON a.id=c.account_id LEFT JOIN account_policy p ON p.account_id=a.id WHERE c.account_id=?1 AND c.generation=?2 AND r.generation=?2 AND r.owner=?3 AND s.project_id=?4 AND s.manage_keys=1 AND a.state='active' AND COALESCE(p.enabled,1)=1)",params![a,g,owner,project],|r|r.get(0))?;
        if !allowed {return Err("Platform key management is not authorized".into());}
        let changed=tx.execute("INSERT INTO platform_key_operations(account_id,project_id,operation,owner,attempted_at,status) VALUES(?1,?2,?3,?4,?5,'submitted') ON CONFLICT(account_id) DO UPDATE SET project_id=excluded.project_id,operation=excluded.operation,owner=excluded.owner,attempted_at=excluded.attempted_at,status='submitted' WHERE status='confirmed'",params![a,project,operation,owner,chrono::Utc::now().timestamp()])?;
        if changed!=1 {return Err("Platform key operation requires operator reconciliation".into());}
        tx.commit()?;Ok(())
    }).await
}
pub(super) async fn reconcile(
    db: &Database,
    vault: &Vault,
    client: &Client,
    access: &Access,
) -> Result<()> {
    let project = access
        .project
        .as_deref()
        .filter(|s| identifier(s))
        .ok_or("Platform project must be configured")?;
    let p = client
        .request(
            access,
            Method::GET,
            &format!("/organization/projects/{project}"),
            &[],
            None,
        )
        .await?;
    if p["id"] != project || p["status"] != "active" {
        return Err("Platform project is not active".into());
    }
    let a = access.account;
    let uncertain=db.call(move|conn| Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM platform_key_operations WHERE account_id=?1 AND status!='confirmed')",[a],|r|r.get::<_,bool>(0))?)).await?;
    if uncertain {
        return Err("Platform key operation requires operator reconciliation".into());
    }
    let services = client
        .list(
            access,
            &format!("/organization/projects/{project}/service_accounts"),
            false,
        )
        .await?;
    let matches: Vec<_> = services
        .iter()
        .filter(|s| s["name"] == "exetrouter")
        .collect();
    if matches.len() > 1 {
        return Err("ambiguous exetrouter service accounts".into());
    }
    let service = if let Some(service) = matches.first() {
        (*service).clone()
    } else {
        begin(db, access, "create_service_account").await?;
        let value = client
            .request(
                access,
                Method::POST,
                &format!("/organization/projects/{project}/service_accounts"),
                &[],
                Some(json!({"name":"exetrouter","expires_in_seconds":BOOTSTRAP_LIFETIME})),
            )
            .await?;
        if value["name"] != "exetrouter"
            || value["object"] != "organization.project.service_account"
            || value["role"] != "member"
            || !value["id"].as_str().is_some_and(identifier)
        {
            return Err("invalid Platform service account creation reply".into());
        }
        let service_id = value["id"]
            .as_str()
            .ok_or("invalid Platform service account ID")?;
        save_created(
            db,
            vault,
            access,
            project,
            service_id,
            value["api_key"].clone(),
            true,
        )
        .await?;
        value
    };
    let service_id = service["id"]
        .as_str()
        .filter(|id| identifier(id))
        .ok_or("invalid Platform service account ID")?;
    match service["role"].as_str() {
        Some("none") => {
            return Err(
                "Platform service account requires operator membership reconciliation".into(),
            )
        }
        Some("member" | "owner") => {}
        _ => return Err("invalid Platform service account role".into()),
    }
    let keys = client
        .list(
            access,
            &format!("/organization/projects/{project}/api_keys"),
            true,
        )
        .await?;
    let now = chrono::Utc::now().timestamp();
    let a = access.account;
    let project_owned = project.to_owned();
    let service_owned = service_id.to_owned();
    let stored=db.call(move|conn| Ok(conn.query_row("SELECT key_id,encrypted_key,created_at,expires_at,EXISTS(SELECT 1 FROM platform_key_operations o WHERE o.account_id=k.account_id AND o.project_id=k.project_id AND o.operation='create_service_account' AND o.status='confirmed') FROM platform_working_keys k WHERE account_id=?1 AND project_id=?2 AND service_account_id=?3",params![a,project_owned,service_owned],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,bool>(4)?))).optional()?)).await?;
    if let Some((id, encrypted, created, expires, bootstrap)) = stored {
        if bootstrap {
            if expires > created + BOOTSTRAP_LIFETIME {
                return Err("invalid Platform bootstrap key lifetime".into());
            }
            if let Some(key) = keys.iter().find(|key| key["id"] == id) {
                if key["owner"]["type"] != "service_account"
                    || key["owner"]["service_account"]["id"] != service_id
                    || key["expires_at"].as_i64() != Some(expires)
                {
                    return Err("Platform bootstrap key identity changed".into());
                }
            }
            if expires > now {
                return Err("Platform bootstrap key expiry pending".into());
            }
        } else if let Some(key) = keys.iter().find(|k| k["id"] == id) {
            let live = key["name"] == "exetrouter"
                && key["owner"]["type"] == "service_account"
                && key["owner"]["service_account"]["id"] == service_id
                && key["owner_project_access"] == "active"
                && key["expires_at"].as_i64() == Some(expires)
                && expires > now
                && expires <= created + LIFETIME;
            let state = if expires <= now {
                "expired"
            } else if live {
                "active"
            } else {
                "inactive"
            };
            let a = access.account;
            let g = access.generation;
            let checked = now;
            let last_used = key["last_used_at"]
                .as_i64()
                .filter(|t| *t >= created && *t <= now);
            // An inventory entry alone does not prove the secret can authenticate.
            // Publish active only after the non-generating read succeeds.
            let check = if live {
                let secret = String::from_utf8(
                    vault.open_secret(aad(access.account, project, &id).as_bytes(), &encrypted)?,
                )
                .map_err(|_| "invalid Platform working key")?;
                validate(secret.as_bytes())?;
                client.models(&secret).await.map(Some)
            } else {
                Ok(None)
            };
            let state = if check.is_err() { "inactive" } else { state };
            let id_owned = id.clone();
            let marker = Access {
                account: a,
                generation: g,
                owner: access.owner.clone(),
                admin: String::new(),
                project: None,
                manage_keys: false,
            };
            db.call(move|conn| {
                if !current(conn,&marker)? {return Err("Platform configuration changed during key check".into());}
                conn.execute("UPDATE platform_working_keys SET generation=?2,state=?3,checked_at=?4,last_used_at=?5 WHERE account_id=?1 AND key_id=?6",params![a,g,state,checked,last_used,id_owned])?;Ok(())
            }).await?;
            if let Some(models) = check? {
                let observed = models.into_iter().map(|m| m.id).collect::<Vec<_>>();
                let identity = KeyIdentity {
                    id: id.clone(),
                    project: project.to_owned(),
                    generation: g,
                };
                db.call(move |conn| super::routes::publish(conn, a, &identity, &observed, checked))
                    .await?;
            }
            if live {
                return Ok(());
            }
            if state == "inactive" {
                return Err("Platform working key is inactive".into());
            }
        }
    }
    // A live named key without its stored one-time secret is not recoverable.
    // Never delete it or create duplicates merely because this host lacks it.
    if keys
        .iter()
        .any(|k| k["name"] == "exetrouter" && k["expires_at"].as_i64().is_none_or(|t| t > now))
    {
        return Err("existing exetrouter key secret unavailable".into());
    }
    begin(db, access, "create_key").await?;
    let value=client.request(access,Method::POST,&format!("/organization/projects/{project}/service_accounts/{service_id}/api_keys"),&[],Some(json!({"name":"exetrouter","expires_in_seconds":LIFETIME,"scopes":["api.model.read","api.responses.write"]}))).await?;
    save_created(db, vault, access, project, service_id, value, false).await?;
    let checked = super::inventory::read(db, vault.clone(), client, access.account).await;
    if checked.is_err() {
        let marker = Access {
            account: access.account,
            generation: access.generation,
            owner: access.owner.clone(),
            admin: String::new(),
            project: None,
            manage_keys: false,
        };
        db.call(move|conn| {
            if current(conn,&marker)? {conn.execute("UPDATE platform_working_keys SET state='inactive' WHERE account_id=?1 AND generation=?2",params![marker.account,marker.generation])?;}
            Ok(())
        }).await?;
    }
    checked.map(|_| ())
}

async fn save_created(
    db: &Database,
    vault: &Vault,
    access: &Access,
    project: &str,
    service: &str,
    value: Value,
    bootstrap: bool,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let id = value["id"]
        .as_str()
        .filter(|id| identifier(id))
        .ok_or("invalid Platform key creation reply")?
        .to_owned();
    let secret = value["value"]
        .as_str()
        .ok_or("Platform key secret missing")?;
    validate(secret.as_bytes())?;
    let created = value["created_at"]
        .as_i64()
        .filter(|t| *t >= now - 60 && *t <= chrono::Utc::now().timestamp() + 60)
        .ok_or("invalid Platform key creation time")?;
    let expires = value["expires_at"]
        .as_i64()
        .filter(|t| {
            (if bootstrap {
                *t >= created
            } else {
                *t > created
            }) && *t
                <= created
                    + if bootstrap {
                        BOOTSTRAP_LIFETIME
                    } else {
                        LIFETIME
                    }
                && (bootstrap || *t > now)
        })
        .ok_or("Platform key expiration was not confirmed")?;
    if (!bootstrap && value["name"] != "exetrouter")
        || value["object"] != "organization.project.service_account.api_key"
    {
        return Err("invalid Platform key creation reply".into());
    }
    let encrypted = vault.seal_secret(
        aad(access.account, project, &id).as_bytes(),
        secret.as_bytes(),
    )?;
    let a = access.account;
    let g = access.generation;
    let project = project.to_owned();
    let service = service.to_owned();
    let owner = access.owner.clone();
    let state = if bootstrap { "inactive" } else { "active" };
    let operation = if bootstrap {
        "create_service_account"
    } else {
        "create_key"
    };
    db.call(move|conn| {
        let tx=rusqlite::Transaction::new_unchecked(conn,rusqlite::TransactionBehavior::Immediate)?;
        let access=Access{account:a,generation:g,owner:owner.clone(),admin:String::new(),project:None,manage_keys:false};
        if !current(&tx,&access)? {return Err("Platform configuration changed during key creation".into());}
        tx.execute("INSERT INTO platform_working_keys(account_id,generation,project_id,service_account_id,key_id,encrypted_key,created_at,expires_at,checked_at,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(account_id) DO UPDATE SET generation=excluded.generation,project_id=excluded.project_id,service_account_id=excluded.service_account_id,key_id=excluded.key_id,encrypted_key=excluded.encrypted_key,created_at=excluded.created_at,expires_at=excluded.expires_at,checked_at=excluded.checked_at,last_used_at=NULL,state=excluded.state",params![a,g,project,service,id,encrypted,created,expires,now,state])?;
        let changed=tx.execute("UPDATE platform_key_operations SET status='confirmed' WHERE account_id=?1 AND owner=?2 AND operation=?3 AND status='submitted'",params![a,owner,operation])?;
        if changed!=1 {return Err("Platform key creation fence changed".into());}
        tx.commit()?;Ok(())
    }).await
}
