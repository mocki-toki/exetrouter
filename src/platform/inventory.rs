//! Model availability is observed with the actual working key, not the Admin
//! Key. Listing a model establishes neither endpoint support nor a cash balance.
use super::{client::Client, keys};
use crate::{catalog, oauth::Vault, store::Database, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

const MAX_MODELS: usize = 2048;

#[derive(Serialize)]
pub(super) struct Model {
    pub id: String,
    object: &'static str,
    display_name: String,
    source: &'static str,
    capabilities: Vec<String>,
}

pub(super) fn parse(value: &Value) -> Result<Vec<Model>> {
    if value["object"] != "list" {
        return Err("invalid Platform model inventory".into());
    }
    let rows = value["data"]
        .as_array()
        .filter(|rows| rows.len() <= MAX_MODELS)
        .ok_or("invalid Platform model inventory")?;
    let mut ids = BTreeSet::new();
    let mut models = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row["id"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
            })
            .ok_or("invalid Platform model identity")?;
        if row["object"] != "model" || !ids.insert(id) {
            return Err("invalid Platform model inventory".into());
        }
        models.push(Model {
            id: id.into(),
            object: "model",
            display_name: catalog::display_name(id),
            source: "platform",
            capabilities: Vec::new(),
        });
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

pub(super) async fn read(
    db: &Database,
    vault: Vault,
    client: &Client,
    account: i64,
) -> Result<Value> {
    // Both checks fence credential/project replacement during the network read.
    let key = db
        .call(move |conn| {
            keys::working_secret(conn, &vault, account, chrono::Utc::now().timestamp())
        })
        .await?;
    let models = client.models(&key.secret).await?;
    let observed = models.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    let routing_enabled = db
        .call(move |conn| {
            super::routes::publish(
                conn,
                account,
                &key.identity,
                &observed,
                chrono::Utc::now().timestamp(),
            )
        })
        .await?;
    Ok(
        json!({"object":"list","data":models,"observed_at":chrono::Utc::now().timestamp(),"routing_enabled":routing_enabled}),
    )
}

pub(super) fn render(value: &Value) -> String {
    let mut text = format!(
        "Platform model availability\n{}\n",
        if value["routing_enabled"] == true {
            "Only reviewed Responses models are eligible for routing; other entries are availability only."
        } else {
            "Availability only; no reviewed Responses model is available for routing."
        }
    );
    for row in value["data"].as_array().into_iter().flatten() {
        if let (Some(id), Some(name)) = (row["id"].as_str(), row["display_name"].as_str()) {
            // The formatter is also safe for decoded synthetic operator replies.
            let safe = |s: &str| {
                s.chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
            };
            text.push_str(&format!("\n{}  {}", safe(id), safe(name)));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_is_bounded_atomic_and_never_infers_capabilities_or_persists_provider_fields() {
        let rows = json!({"object":"list","data":[
            {"object":"model","id":"gpt-z","owned_by":"private-owner-canary","secret":"private-body-canary","capabilities":["image_generation"]},
            {"object":"model","id":"gpt-a"}
        ]});
        let parsed = serde_json::to_value(parse(&rows).unwrap()).unwrap();
        assert_eq!(parsed[0]["id"], "gpt-a");
        assert_eq!(parsed[1]["display_name"], "GPT z");
        assert_eq!(parsed[1]["capabilities"], json!([]));
        assert!(!parsed.to_string().contains("canary"));
        for data in [
            json!([{"object":"model","id":"gpt-a"},{"object":"model","id":"gpt-a"}]),
            json!([{"object":"model","id":"bad\nmodel"}]),
            json!([{"id":"missing-object"}]),
            json!([{"object":"model","id":"../responses"}]),
            json!(vec![
                json!({"object":"model","id":"duplicate"});
                MAX_MODELS + 1
            ]),
        ] {
            assert!(parse(&json!({"object":"list","data":data})).is_err());
        }
        assert!(parse(&json!({"object":"list","data":[]}))
            .unwrap()
            .is_empty());
        assert!(parse(&json!({"object":"list"})).is_err());
    }
}
