//! Platform project identities are deliberately separate from OAuth accounts.
//! Negative internal IDs identify these routes; usage/display still uses the
//! positive parent account. No API key is stored as an OAuth credential.
use super::keys::{self, KeyIdentity};
use crate::{
    catalog,
    oauth::{Account, AccountInfo, Credentials, Vault},
    Result,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

pub(crate) fn route_id(owner: i64) -> Result<i64> {
    owner
        .checked_neg()
        .filter(|id| *id > 0)
        .ok_or_else(|| "invalid Platform route".into())
}

pub(crate) fn model_spending_permitted(conn: &Connection, model: &str) -> Result<bool> {
    if catalog::embedding_model(model) {
        return Ok(true); // No Codex embeddings endpoint, independent of its catalog freshness.
    }
    let codex = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_models WHERE model=?1)",
        [model],
        |r| r.get::<_, bool>(0),
    )?;
    if codex {
        return super::spending_permitted(conn, true);
    }
    if super::codex_fallback_allowed(conn)? {
        return Ok(true);
    }
    let now = chrono::Utc::now().timestamp();
    let incomplete=conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.state<>'disabled' AND COALESCE(p.enabled,1)=1 AND (a.state<>'active' OR a.catalog_updated_at IS NULL OR a.catalog_updated_at>?1 OR a.catalog_updated_at<=?1-?2))",params![now,catalog::CACHE_TTL_SECONDS],|r|r.get::<_,bool>(0))?;
    Ok(!incomplete)
}

/// Shared by admission and passive diagnostics: only actual native exhaustion
/// authorizes replacing a known Codex route; operational/auth failures do not.
pub(crate) fn fallback_eligible(conn: &Connection, model: &str, now: i64) -> Result<bool> {
    if catalog::embedding_model(model) {
        return Ok(true);
    }
    if !model_spending_permitted(conn, model)? {
        return Ok(false);
    }
    let known = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_models WHERE model=?1)",
        [model],
        |r| r.get::<_, bool>(0),
    )?;
    let incomplete=conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.state<>'disabled' AND COALESCE(p.enabled,1)=1 AND (a.state<>'active' OR a.catalog_updated_at IS NULL OR a.catalog_updated_at>?1 OR a.catalog_updated_at<=?1-?2))",params![now,catalog::CACHE_TTL_SECONDS],|r|r.get::<_,bool>(0))?;
    if !known {
        return Ok(!incomplete || super::codex_fallback_allowed(conn)?);
    }
    let mut stmt=conn.prepare("SELECT a.id,a.state,a.expires_at,a.catalog_updated_at FROM oauth_accounts a JOIN oauth_models m ON m.account_id=a.id LEFT JOIN account_policy p ON p.account_id=a.id WHERE m.model=?1 AND a.state<>'disabled' AND COALESCE(p.enabled,1)=1")?;
    let rows = stmt
        .query_map([model], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Ok(!incomplete);
    }
    for (id, state, expires, updated) in rows {
        if state != "active"
            || expires <= now + 60
            || !catalog::fresh(updated, now)
            || crate::health::paused(conn, id, now)?.is_some()
        {
            return Ok(false);
        }
        let summary = crate::quota::summary(conn, id, now)?;
        if summary.cooldown_until.is_none()
            && !summary.windows.iter().any(|w| {
                w.status == "current"
                    && w.used_percent >= 100.
                    && w.reset_at.is_some_and(|at| at > now)
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Read only observed metadata, not credential ciphertext, for doctor/CLI/UI.
pub(crate) fn observed(conn: &Connection, owner: i64, now: i64) -> Result<Vec<catalog::Model>> {
    let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM platform_routes r JOIN account_platform_credentials c ON c.account_id=r.account_id AND c.generation=r.credential_generation JOIN platform_settings s ON s.account_id=r.account_id AND s.project_id=r.project_id JOIN platform_working_keys k ON k.account_id=r.account_id AND k.generation=c.generation AND k.key_id=r.key_id AND k.project_id=r.project_id WHERE r.id=?1 AND k.state='active' AND k.expires_at>?2 AND r.observed_at<=?2 AND r.observed_at>?2-?3 AND NOT EXISTS(SELECT 1 FROM platform_key_operations o WHERE o.account_id=r.account_id AND o.status<>'confirmed'))",params![route_id(owner)?,now,catalog::CACHE_TTL_SECONDS],|r|r.get(0))?;
    if !valid || !enabled(conn, owner)? {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for model in models(conn, owner, now)? {
        if model_spending_permitted(conn, &model.id)? {
            result.push(model);
        }
    }
    Ok(result)
}

pub(crate) fn available(conn: &Connection, owner: i64, now: i64) -> Result<bool> {
    let generation = conn.query_row(
        "SELECT generation FROM platform_routes WHERE id=?1",
        [route_id(owner)?],
        |r| r.get::<_, i64>(0),
    )?;
    let (quota, health) = paused(conn, owner, generation, now)?;
    if quota.is_some() || health.is_some() {
        return Ok(false);
    }
    for model in observed(conn, owner, now)? {
        if fallback_eligible(conn, &model.id, now)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reviewed public Responses models, intersected with actual key availability.
/// Unknown inventory entries are never promoted by guessing endpoint support.
fn descriptor(id: &str) -> Option<catalog::Model> {
    if catalog::embedding_model(id) {
        return Some(catalog::Model {
            id: id.into(),
            object: "model".into(),
            owned_by: "openai".into(),
            display_name: catalog::display_name(id),
            source: "platform".into(),
            capabilities: vec!["embeddings".into()],
            metadata: None,
        });
    }
    let none = match id {
        "gpt-6.1-sol" | "gpt-6-astra" => false,
        "gpt-6.1-terra" | "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra"
        | "gpt-5.6-luna" | "gpt-5.5" => true,
        _ => return None,
    };
    let mut efforts = if none {
        vec!["none", "low", "medium", "high", "xhigh", "max"]
    } else {
        vec!["low", "medium", "high", "xhigh", "max"]
    };
    if id == "gpt-5.5" {
        efforts.retain(|effort| *effort != "max");
    }
    let mut model = catalog::Model::parse(&json!({
        "slug":id,"display_name":catalog::display_name(id),
        "context_window":1050000,"max_context_window":1050000,
        "auto_compact_token_limit":922000,"max_output_tokens":128000,
        "default_reasoning_level":"medium",
        "supported_reasoning_levels":efforts.into_iter().map(|effort| json!({"effort":effort,"description":effort})).collect::<Vec<_>>(),
        "shell_type":"default","visibility":"list","supported_in_api":true,
        "priority":1000,"support_verbosity":true,"truncation_policy":{"mode":"tokens","limit":10000},
        "experimental_supported_tools":[],"input_modalities":["text","image"]
    })).ok()?;
    model.source = "platform".into();
    model.present();
    Some(model)
}

pub(super) fn publish(
    conn: &Connection,
    account: i64,
    key: &KeyIdentity,
    ids: &[String],
    now: i64,
) -> Result<bool> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    keys::current_key(&tx, account, key, now)?;
    let models = ids
        .iter()
        .filter_map(|id| descriptor(id))
        .collect::<Vec<_>>();
    let catalog = serde_json::to_string(&models)?;
    tx.execute("INSERT INTO platform_routes(account_id,project_id,credential_generation,key_id,catalog,observed_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,project_id) DO UPDATE SET generation=platform_routes.generation+CASE WHEN platform_routes.credential_generation<>excluded.credential_generation OR platform_routes.key_id<>excluded.key_id THEN 1 ELSE 0 END,credential_generation=excluded.credential_generation,key_id=excluded.key_id,catalog=excluded.catalog,observed_at=excluded.observed_at", params![account,key.project,key.generation,key.id,catalog,now])?;
    // Provider quota belongs to the project, not the expiring credential. A
    // successful key/model refresh never erases its outstanding rejection.
    tx.execute("UPDATE platform_route_health SET generation=(SELECT generation FROM platform_routes WHERE id=route_id),failures=0,retry_at=0,last_order=0 WHERE route_id=(SELECT id FROM platform_routes WHERE account_id=?1 AND project_id=?2) AND generation<>(SELECT generation FROM platform_routes WHERE id=route_id)",params![account,key.project])?;
    tx.commit()?;
    Ok(!models.is_empty())
}

pub(crate) fn parent(conn: &Connection, owner: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT account_id FROM platform_routes WHERE id=?1",
        [route_id(owner)?],
        |r| r.get(0),
    )?)
}

pub(crate) fn enabled(conn: &Connection, owner: i64) -> Result<bool> {
    let id = parent(conn, owner)?;
    let active: bool = conn.query_row(
        "SELECT state='active' FROM oauth_accounts WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    Ok(active && crate::account_preferences::effective(conn, id)?.enabled)
}

pub(crate) fn account(conn: &Connection, vault: &Vault, owner: i64, now: i64) -> Result<Account> {
    let route = route_id(owner)?;
    let (parent, project, generation, credential_generation, key_id, observed_at): (i64,String,i64,i64,String,i64) = conn.query_row("SELECT account_id,project_id,generation,credential_generation,key_id,observed_at FROM platform_routes WHERE id=?1", [route], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    let key = keys::working_secret(conn, vault, parent, now)?;
    if key.identity.project != project
        || key.identity.id != key_id
        || key.identity.generation != credential_generation
    {
        return Err("Platform route credentials changed; refresh model availability".into());
    }
    let expires_at = conn.query_row(
        "SELECT expires_at FROM platform_working_keys WHERE account_id=?1",
        [parent],
        |r| r.get(0),
    )?;
    let (cooldown_until, health_until) = paused(conn, owner, generation, now)?;
    Ok(Account {
        info: AccountInfo {
            id: owner,
            account_id: project,
            state: "active".into(),
            expires_at,
            generation,
            catalog_updated_at: Some(observed_at),
            cooldown_until,
            cooldown_source: cooldown_until.map(|_| "platform".into()),
            health_until,
            health_reason: health_until.map(|_| "platform".into()),
        },
        credentials: Credentials {
            access_token: key.secret,
            refresh_token: String::new(),
        },
    })
}

pub(crate) fn models(conn: &Connection, owner: i64, now: i64) -> Result<Vec<catalog::Model>> {
    let (catalog, at): (String, i64) = conn.query_row(
        "SELECT catalog,observed_at FROM platform_routes WHERE id=?1",
        [route_id(owner)?],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !catalog::fresh(Some(at), now) {
        return Err("Platform model availability is stale".into());
    }
    Ok(serde_json::from_str(&catalog)?)
}

pub(crate) fn owners(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt=conn.prepare("SELECT -r.id FROM platform_routes r JOIN platform_settings s ON s.account_id=r.account_id AND s.project_id=r.project_id ORDER BY r.id")?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn paused(
    conn: &Connection,
    owner: i64,
    generation: i64,
    now: i64,
) -> Result<(Option<i64>, Option<i64>)> {
    let (quota,health)=conn.query_row("SELECT quota_until,retry_at FROM platform_route_health WHERE route_id=?1 AND generation=?2",params![route_id(owner)?,generation],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()?.unwrap_or_default();
    Ok((
        Some(quota).filter(|at| *at > now),
        Some(health).filter(|at| *at > now),
    ))
}

pub(crate) fn valid(conn: &Connection, owner: i64, generation: i64) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM platform_routes r JOIN account_platform_credentials c ON c.account_id=r.account_id AND c.generation=r.credential_generation JOIN platform_working_keys k ON k.account_id=r.account_id AND k.generation=c.generation AND k.key_id=r.key_id AND k.project_id=r.project_id JOIN platform_settings s ON s.account_id=r.account_id AND s.project_id=r.project_id WHERE r.id=?1 AND r.generation=?2)",params![route_id(owner)?,generation],|r|r.get(0))?)
}

pub(crate) fn quota(
    conn: &Connection,
    owner: i64,
    generation: i64,
    order: i64,
    until: i64,
) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    if !valid(&tx, owner, generation)? {
        return Ok(());
    }
    tx.execute("INSERT INTO platform_route_health(route_id,generation,quota_until,quota_order) VALUES(?1,?2,?3,?4) ON CONFLICT(route_id) DO UPDATE SET generation=excluded.generation,quota_until=CASE WHEN platform_route_health.generation=excluded.generation THEN MAX(platform_route_health.quota_until,excluded.quota_until) ELSE excluded.quota_until END,quota_order=MAX(platform_route_health.quota_order,excluded.quota_order),retry_at=CASE WHEN platform_route_health.generation=excluded.generation THEN platform_route_health.retry_at ELSE 0 END,failures=CASE WHEN platform_route_health.generation=excluded.generation THEN platform_route_health.failures ELSE 0 END,last_order=CASE WHEN platform_route_health.generation=excluded.generation THEN platform_route_health.last_order ELSE 0 END",params![route_id(owner)?,generation,until,order])?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn health(
    conn: &Connection,
    owner: i64,
    generation: i64,
    order: i64,
    rejection: Option<crate::health::Rejection>,
    retry: Option<i64>,
    now: i64,
) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    if !valid(&tx, owner, generation)? {
        return Ok(());
    }
    let old=tx.query_row("SELECT failures,retry_at,last_order FROM platform_route_health WHERE route_id=?1 AND generation=?2",params![route_id(owner)?,generation],|r|Ok((r.get::<_,u32>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).optional()?;
    if old.is_some_and(|old| old.2 >= order) {
        return Ok(());
    }
    let failures = if rejection.is_some() {
        old.map_or(1, |o| o.0.saturating_add(1).min(32))
    } else {
        0
    };
    let until = if rejection.is_some() {
        now.saturating_add(
            (5_i64.saturating_mul(1_i64 << failures.saturating_sub(1).min(6))).min(300),
        )
        .max(retry.unwrap_or(now))
        .max(old.map_or(now, |o| o.1))
    } else {
        0
    };
    tx.execute("INSERT INTO platform_route_health(route_id,generation,failures,retry_at,last_order) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(route_id) DO UPDATE SET generation=excluded.generation,failures=excluded.failures,retry_at=excluded.retry_at,last_order=excluded.last_order,quota_until=CASE WHEN platform_route_health.generation=excluded.generation THEN platform_route_health.quota_until ELSE 0 END,quota_order=CASE WHEN platform_route_health.generation=excluded.generation THEN platform_route_health.quota_order ELSE 0 END",params![route_id(owner)?,generation,failures,until,order])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_descriptors_are_explicit_responses_models_not_slug_inference() {
        let ids = [
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ];
        let models = ids
            .iter()
            .map(|id| descriptor(id).unwrap())
            .collect::<Vec<_>>();
        for model in &models {
            assert_eq!(model.source, "platform");
            assert_eq!(
                model.capabilities,
                vec!["reasoning", "tools", "image_input"]
            );
            assert!(!model.display_name.contains('-'));
            let meta = model.metadata.as_ref().unwrap();
            assert_eq!(meta["context_window"], 1050000);
            assert_eq!(meta["auto_compact_token_limit"], 922000);
            assert_eq!(meta["max_output_tokens"], 128000);
            let has_none = meta["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|level| level["effort"] == "none");
            assert_eq!(
                has_none,
                !matches!(model.id.as_str(), "gpt-6.1-sol" | "gpt-6-astra")
            );
            let has_max = meta["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|level| level["effort"] == "max");
            assert_eq!(has_max, model.id != "gpt-5.5");
        }
        catalog::codex(&models).unwrap();
        catalog::opencode_v1(&models).unwrap();
        catalog::opencode_v2(&models).unwrap();
        for id in ["gpt-6.1-sol-new", "gpt-image-2", "unknown-response-model"] {
            assert!(descriptor(id).is_none());
        }
    }
}
