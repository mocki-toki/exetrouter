use super::{costs, keys};
use crate::{oauth::Vault, store::Database, Result};
use reqwest::{Client as Http, Method, Url};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::{collections::BTreeSet, net::IpAddr, time::Duration};

pub(crate) const BASE: &str = "https://api.openai.com/v1";
#[derive(Clone)]
pub(super) struct Client {
    http: Http,
    base: String,
}
pub(super) struct Access {
    pub account: i64,
    pub generation: i64,
    pub owner: String,
    pub admin: String,
    pub project: Option<String>,
    pub manage_keys: bool,
}

pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl Client {
    pub fn new(base: &str, allow_mock: bool) -> Result<Self> {
        let url = Url::parse(base).map_err(|_| "invalid Platform endpoint")?;
        let local = url
            .host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if !(base == BASE || allow_mock && local && url.scheme() == "http")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Platform requires the official API; explicit test overrides must be literal loopback HTTP".into());
        }
        Ok(Self {
            http: crate::upstream::http_client()?,
            base: base.trim_end_matches('/').into(),
        })
    }
    pub async fn request(
        &self,
        access: &Access,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let parts = path.split('/').collect::<Vec<_>>();
        let allowed = match parts.as_slice() {
            ["", "organization", "costs"] => method == Method::GET,
            ["", "organization", "projects", project] => {
                identifier(project) && method == Method::GET
            }
            ["", "organization", "projects", project, "api_keys"] => {
                identifier(project) && method == Method::GET
            }
            ["", "organization", "projects", project, "service_accounts"] => {
                identifier(project) && matches!(method, Method::GET | Method::POST)
            }
            ["", "organization", "projects", project, "service_accounts", service, "api_keys"] => {
                identifier(project) && identifier(service) && method == Method::POST
            }
            _ => false,
        };
        if !allowed {
            return Err("Platform management client cannot submit inference".into());
        }
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&access.admin)
            .query(query)
            .timeout(Duration::from_secs(10));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Platform request unavailable")?;
        if !response.status().is_success() {
            return Err(format!(
                "Platform request rejected (HTTP {})",
                response.status().as_u16()
            )
            .into());
        }
        crate::oauth::read_json(response, 1024 * 1024)
            .await
            .map_err(|_| "invalid Platform reply".into())
    }
    pub async fn models(&self, working_key: &str) -> Result<Vec<super::inventory::Model>> {
        keys::validate(working_key.as_bytes())?;
        let response = self
            .http
            .get(format!("{}/models", self.base))
            .bearer_auth(working_key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|_| "Platform model inventory unavailable")?;
        if !response.status().is_success() {
            return Err("Platform working key model access rejected".into());
        }
        let value = crate::oauth::read_json(response, 1024 * 1024)
            .await
            .map_err(|_| "invalid Platform model inventory")?;
        super::inventory::parse(&value)
    }
    pub async fn list(&self, access: &Access, path: &str, any_access: bool) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        let mut after: Option<String> = None;
        let mut seen = BTreeSet::new();
        for _ in 0..10 {
            let mut query = vec![("limit".into(), "100".into())];
            if any_access {
                query.push(("owner_project_access".into(), "any".into()));
            }
            if let Some(after) = &after {
                query.push(("after".into(), after.clone()));
            }
            let value = self
                .request(access, Method::GET, path, &query, None)
                .await?;
            if value["object"] != "list" {
                return Err("invalid Platform list".into());
            }
            let rows = value["data"]
                .as_array()
                .filter(|r| r.len() <= 100)
                .ok_or("invalid Platform list")?;
            for row in rows {
                let id = row["id"]
                    .as_str()
                    .filter(|id| identifier(id))
                    .ok_or("invalid Platform object ID")?;
                if !seen.insert(id.to_owned()) {
                    return Err("duplicate Platform object".into());
                }
            }
            result.extend(rows.iter().cloned());
            match value["has_more"].as_bool() {
                Some(false) => return Ok(result),
                Some(true) => {
                    after = Some(
                        value["last_id"]
                            .as_str()
                            .filter(|id| {
                                identifier(id)
                                    && Some(*id) == rows.last().and_then(|r| r["id"].as_str())
                            })
                            .ok_or("invalid Platform cursor")?
                            .to_owned(),
                    );
                }
                None => return Err("invalid Platform list".into()),
            }
        }
        Err("Platform list exceeds the bounded scan".into())
    }
    async fn costs(&self, access: &Access, now: i64) -> Result<Vec<costs::Point>> {
        let from = (now.div_euclid(costs::DAY) - 90) * costs::DAY;
        self.costs_range(access, from, now).await
    }
    async fn costs_range(&self, access: &Access, from: i64, now: i64) -> Result<Vec<costs::Point>> {
        if from >= now || now - from > 366 * costs::DAY {
            return Err("invalid Platform costs range".into());
        }
        let mut page: Option<String> = None;
        let mut seen = BTreeSet::new();
        let mut points = Vec::new();
        for _ in 0..4 {
            let mut query = vec![
                ("start_time".into(), from.to_string()),
                ("end_time".into(), now.to_string()),
                ("bucket_width".into(), "1d".into()),
                ("limit".into(), "180".into()),
            ];
            if let Some(page) = &page {
                query.push(("page".into(), page.clone()));
            }
            let value = self
                .request(access, Method::GET, "/organization/costs", &query, None)
                .await?;
            points.extend(costs::parse_page(
                &value,
                from,
                now,
                chrono::Utc::now().timestamp(),
            )?);
            if points.len() > 367 {
                return Err("Platform costs exceed the requested range".into());
            }
            if value["has_more"] == false {
                return Ok(points);
            }
            let next = value["next_page"]
                .as_str()
                .filter(|p| p.len() <= 512 && !p.is_empty() && !p.chars().any(char::is_control))
                .ok_or("invalid Platform costs cursor")?;
            if !seen.insert(next.to_owned()) {
                return Err("repeated Platform costs cursor".into());
            }
            page = Some(next.to_owned());
        }
        Err("Platform costs exceed the bounded scan".into())
    }
}

async fn claim(db: &Database, vault: Vault, account: i64, force: bool) -> Result<Option<Access>> {
    db.call(move|conn| {
        let now=chrono::Utc::now().timestamp();
        let Some((generation,encrypted))=conn.query_row("SELECT generation,encrypted_admin_key FROM account_platform_credentials WHERE account_id=?1",[account],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?))).optional()? else{return Ok(None)};
        let admin=String::from_utf8(vault.open_secret(super::aad(account).as_bytes(),&encrypted)?).map_err(|_|"invalid Platform credentials")?;
        let settings=conn.query_row("SELECT project_id,manage_keys FROM platform_settings WHERE account_id=?1",[account],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,bool>(1)?))).optional()?.unwrap_or_default();
        let owner=format!("{:032x}",rand::random::<u128>());
        let changed=conn.execute("INSERT INTO platform_refresh(account_id,generation,owner,started_at) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id) DO UPDATE SET generation=excluded.generation,owner=excluded.owner,started_at=excluded.started_at,error=NULL WHERE generation!=?2 OR started_at<=?4-?5",params![account,generation,owner,now,if force {60}else{900}])?;
        Ok((changed==1).then_some(Access{account,generation,owner,admin,project:settings.0,manage_keys:settings.1}))
    }).await
}

pub(super) fn current(conn: &rusqlite::Connection, access: &Access) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM account_platform_credentials c JOIN platform_refresh r USING(account_id) WHERE c.account_id=?1 AND c.generation=?2 AND r.generation=?2 AND r.owner=?3)",params![access.account,access.generation,access.owner],|r|r.get(0))?)
}

pub(super) async fn refresh(
    db: &Database,
    vault: Vault,
    client: &Client,
    account: i64,
    force: bool,
) -> Result<()> {
    let Some(access) = claim(db, vault.clone(), account, force).await? else {
        return Ok(());
    };
    let now = chrono::Utc::now().timestamp();
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        let points = client.costs(&access, now).await?;
        let a = Access {
            account: access.account,
            generation: access.generation,
            owner: access.owner.clone(),
            admin: String::new(),
            project: None,
            manage_keys: false,
        };
        db.call(move |conn| {
            let tx = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            if !current(&tx, &a)? {
                return Err("Platform configuration changed during refresh".into());
            }
            costs::save(&tx, a.account, a.generation, &points)?;
            tx.execute(
                "UPDATE platform_refresh SET observed_at=?2,costs_to_utc=?2 WHERE account_id=?1",
                params![a.account, now],
            )?;
            super::prune(&tx, now)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        if access.manage_keys {
            keys::reconcile(db, &vault, client, &access).await?;
        }
        // Separate anchored organization estimate. It must never block key
        // management or cause a mutation retry when billing data is delayed.
        let id = access.account;
        if let Some((epoch, from, generation)) = db
            .call(move |conn| super::balance::anchor(conn, id))
            .await?
        {
            if generation == access.generation && from < now {
                if let Ok(points) = client.costs_range(&access, from, now).await {
                    if let Some((amount, through)) = super::balance::reported(&points, from) {
                        db.call(move |conn| {
                            super::balance::save_report(
                                conn, id, &epoch, generation, amount, through, now,
                            )
                        })
                        .await?;
                    }
                }
            }
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
    .unwrap_or_else(|_| Err("Platform refresh timed out".into()));
    let error = result
        .as_ref()
        .err()
        .map(|error| match error.to_string().as_str() {
            "existing exetrouter key secret unavailable" => "key_secret_unavailable",
            "Platform key operation requires operator reconciliation" => "key_operation_uncertain",
            "Platform project must be configured" => "project_required",
            "Platform working key is inactive" => "key_inactive",
            "Platform bootstrap key expiry pending" => "bootstrap_expiring",
            "Platform service account requires operator membership reconciliation" => {
                "membership_required"
            }
            _ => "unavailable",
        });
    let a = access.account;
    let owner = access.owner;
    db.call(move |conn| {
        conn.execute(
            "UPDATE platform_refresh SET error=?3 WHERE account_id=?1 AND owner=?2",
            params![a, owner, error],
        )?;
        Ok(())
    })
    .await?;
    result
}

pub(super) async fn worker(
    db: Database,
    vault: Vault,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let Ok(client) = Client::new(BASE, false) else {
        return;
    };
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {biased;_=stop.changed()=>break,_=tick.tick()=>{}}
        if *stop.borrow() {
            break;
        }
        let ids=db.call(|conn| {
            let mut stmt=conn.prepare("SELECT c.account_id FROM account_platform_credentials c JOIN oauth_accounts a ON a.id=c.account_id LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.state='active' AND COALESCE(p.enabled,1)=1 ORDER BY c.account_id")?;
            let ids=stmt.query_map([],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;Ok(ids)
        }).await.unwrap_or_default();
        for id in ids {
            tokio::select! {biased;_=stop.changed()=>return,_=refresh(&db,vault.clone(),&client,id,false)=>{}}
        }
    }
}
