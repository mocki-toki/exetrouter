//! Account-visible model metadata and explicit client-specific projections.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

// Model-owned fields from the pinned Codex ModelInfo schema. Never expose
// account/credential fields or arbitrary additions to an upstream response.
const FIELDS: &[&str] = &[
    "slug",
    "display_name",
    "description",
    "default_reasoning_level",
    "supported_reasoning_levels",
    "shell_type",
    "visibility",
    "supported_in_api",
    "priority",
    "additional_speed_tiers",
    "service_tiers",
    "default_service_tier",
    "available_access_programs",
    "availability_nux",
    "upgrade",
    "model_messages",
    "include_skills_usage_instructions",
    "include_plugin_usage_instructions",
    "include_apps_usage_instructions",
    "supports_reasoning_summary_parameter",
    "default_reasoning_summary",
    "support_verbosity",
    "default_verbosity",
    "apply_patch_tool_type",
    "web_search_tool_type",
    "truncation_policy",
    "supports_image_detail_original",
    "context_window",
    "max_context_window",
    "auto_compact_token_limit",
    "comp_hash",
    "effective_context_window_percent",
    "experimental_supported_tools",
    "input_modalities",
    "supports_search_tool",
    "supports_experimental_context",
    "use_responses_lite",
    "supports_reasoning_effort_updates",
    "node_repl_auto_review_required",
    "node_repl_disabled",
    "auto_review_model_override",
    "model_specialty",
    "tool_mode",
    "multi_agent_version",
    "multi_agent_reasoning_effort",
    "guardian",
    "prefer_websockets",
    "supports_parallel_tool_calls",
    "max_output_tokens",
];
const REQUIRED: &[&str] = &[
    "supported_reasoning_levels",
    "shell_type",
    "visibility",
    "supported_in_api",
    "priority",
    "support_verbosity",
    "truncation_policy",
    "experimental_supported_tools",
    "input_modalities",
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub object: String,
    pub owned_by: String,
    pub display_name: String,
    #[serde(
        default,
        rename = "exetrouter",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<Value>,
}

impl Model {
    pub(crate) fn parse(row: &Value) -> Result<Self> {
        let id = row["slug"].as_str().ok_or("catalog model ID missing")?;
        let name = row["display_name"].as_str().unwrap_or(id);
        if id.is_empty()
            || id.len() > 128
            || id.chars().any(char::is_control)
            || name.len() > 256
            || name.chars().any(char::is_control)
        {
            return Err("invalid catalog model identity".into());
        }
        let fields: Map<String, Value> = FIELDS
            .iter()
            .filter_map(|field| {
                row.get(*field)
                    .map(|value| ((*field).into(), value.clone()))
            })
            .collect();
        let mut metadata = Value::Object(fields);
        metadata["display_name"] = json!(name);
        if serde_json::to_vec(&metadata)?.len() > 262_144 {
            return Err("model metadata exceeds 256 KiB".into());
        }
        for field in [
            "context_window",
            "max_context_window",
            "auto_compact_token_limit",
            "max_output_tokens",
        ] {
            if let Some(value) = metadata.get(field).filter(|v| !v.is_null()) {
                if !value.as_u64().is_some_and(|n| n > 0 && n <= 100_000_000) {
                    return Err("invalid model token limit".into());
                }
            }
        }
        if let Some(value) = metadata.get("effective_context_window_percent") {
            if !value.as_u64().is_some_and(|n| n > 0 && n <= 100) {
                return Err("invalid model context percentage".into());
            }
        }
        Ok(Self {
            id: id.into(),
            object: "model".into(),
            owned_by: "openai".into(),
            display_name: name.into(),
            metadata: Some(metadata),
        })
    }

    /// Intersect numeric limits and advertised capabilities across accounts.
    /// Incompatible model protocols cannot produce an authoritative descriptor.
    pub(crate) fn merge(&mut self, other: Self) {
        let (Some(left), Some(right)) = (&mut self.metadata, other.metadata) else {
            self.metadata = None;
            return;
        };
        let Some(fields) = left.as_object_mut() else {
            self.metadata = None;
            return;
        };
        for field in FIELDS {
            if matches!(
                *field,
                "context_window"
                    | "max_context_window"
                    | "auto_compact_token_limit"
                    | "effective_context_window_percent"
                    | "max_output_tokens"
            ) {
                let default = if *field == "effective_context_window_percent" {
                    Some(95)
                } else {
                    None
                };
                let a = fields.get(*field).and_then(Value::as_u64).or(default);
                let b = right[*field].as_u64().or(default);
                match (a, b) {
                    (Some(a), Some(b)) => {
                        fields.insert((*field).into(), json!(a.min(b)));
                    }
                    _ => {
                        fields.remove(*field);
                    }
                }
            } else if *field == "supports_experimental_context" {
                // An account-specific opt-in is a capability, not a protocol
                // conflict. Advertise it only when every account enables it.
                let supported = fields.get(*field).and_then(Value::as_bool) == Some(true)
                    && right[*field].as_bool() == Some(true);
                fields.insert((*field).into(), json!(supported));
            } else if matches!(
                *field,
                "supported_reasoning_levels" | "input_modalities" | "experimental_supported_tools"
            ) {
                if let (Some(a), Some(b)) = (
                    fields.get(*field).and_then(Value::as_array),
                    right[*field].as_array(),
                ) {
                    let common = a
                        .iter()
                        .filter(|a| {
                            b.iter().any(|b| {
                                if *field == "supported_reasoning_levels" {
                                    a["effort"] == b["effort"]
                                } else {
                                    *a == b
                                }
                            })
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    fields.insert((*field).into(), json!(common));
                } else {
                    self.metadata = None;
                    return;
                }
            } else if matches!(
                *field,
                "display_name"
                    | "description"
                    | "priority"
                    | "availability_nux"
                    | "upgrade"
                    | "default_reasoning_level"
            ) {
                // Presentation fields do not change request shaping.
            } else if fields.get(*field) != right.get(*field) {
                // Do not advertise a tool mode, instructions or service tier that
                // depends on which account later receives the request.
                self.metadata = None;
                return;
            }
        }
        if !fields
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| {
                levels
                    .iter()
                    .any(|level| fields.get("default_reasoning_level") == level.get("effort"))
            })
        {
            let effort = fields
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .and_then(|levels| levels.first())
                .and_then(|level| level.get("effort"))
                .cloned()
                .unwrap_or(Value::Null);
            fields.insert("default_reasoning_level".into(), effort);
        }
    }
}

pub fn codex(models: &[Model]) -> Result<Value> {
    let metadata = models
        .iter()
        .map(|model| {
            let value = model
                .metadata
                .as_ref()
                .ok_or("model metadata differs across accounts")?;
            if REQUIRED.iter().any(|field| value.get(*field).is_none()) {
                return Err("authoritative model metadata unavailable".into());
            }
            Ok(value.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"models":metadata}))
}

/// Existing HTTP projection remains V2 for compatibility.
pub fn opencode(models: &[Model]) -> Result<Value> {
    opencode_v2(models)
}

pub fn opencode_v1(models: &[Model]) -> Result<Value> {
    opencode_projection(models, true)
}

pub fn opencode_v2(models: &[Model]) -> Result<Value> {
    opencode_projection(models, false)
}

fn opencode_projection(models: &[Model], v1: bool) -> Result<Value> {
    let mut entries = Map::new();
    for model in models {
        let meta = model
            .metadata
            .as_ref()
            .ok_or("model metadata differs across accounts")?;
        let context = meta["context_window"]
            .as_u64()
            .or_else(|| meta["max_context_window"].as_u64())
            .ok_or("model context window unavailable")?;
        let percent = meta["effective_context_window_percent"]
            .as_u64()
            .unwrap_or(95);
        let input = (context * percent / 100)
            .min(meta["auto_compact_token_limit"].as_u64().unwrap_or(context));
        let modalities = meta["input_modalities"]
            .as_array()
            .ok_or("model input modalities unavailable")?;
        let levels = meta["supported_reasoning_levels"]
            .as_array()
            .ok_or("model reasoning levels unavailable")?;
        let variants = levels
            .iter()
            .filter_map(|level| level["effort"].as_str())
            .map(|effort| json!({"id":effort,"settings":{"reasoningEffort":effort}}))
            .collect::<Vec<_>>();
        let mut settings = json!({});
        if let Some(effort) = meta["default_reasoning_level"].as_str() {
            settings["reasoningEffort"] = json!(effort);
        }
        if let Some(summary) = meta["default_reasoning_summary"].as_str() {
            settings["reasoningSummary"] = json!(summary);
        }
        if meta["support_verbosity"] == true {
            if let Some(verbosity) = meta["default_verbosity"].as_str() {
                settings["textVerbosity"] = json!(verbosity);
            }
        }
        if v1 {
            let variants = levels
                .iter()
                .filter_map(|level| level["effort"].as_str())
                .map(|effort| (effort.to_owned(), json!({"reasoningEffort":effort})))
                .collect::<Map<String, Value>>();
            let reasoning = levels
                .iter()
                .filter_map(|level| level["effort"].as_str())
                .any(|effort| effort != "none");
            settings["store"] = json!(false);
            entries.insert(model.id.clone(),json!({
                "name":model.display_name,
                "limit":{"context":context,"input":input,"output":meta["max_output_tokens"].as_u64().unwrap_or(0)},
                "tool_call":true,"reasoning":reasoning,
                "modalities":{"input":modalities,"output":["text"]},
                "options":settings,"variants":variants
            }));
            continue;
        }
        entries.insert(model.id.clone(),json!({
            "name":model.display_name,
            "limit":{"context":context,"input":input,"output":meta["max_output_tokens"].as_u64().unwrap_or(0)},
            "capabilities":{"tools":true,"input":modalities,"output":["text"]},
            "variants":variants,"settings":settings
        }));
    }
    Ok(if v1 {
        json!({"provider":{"openai":{"models":entries}}})
    } else {
        json!({"providers":{"openai":{"models":entries}}})
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opencode_versions_export_their_native_shapes_with_reported_defaults() {
        let model=Model::parse(&json!({"slug":"gpt-test","display_name":"Test model","context_window":100000,
            "effective_context_window_percent":90,"auto_compact_token_limit":85000,"supported_reasoning_levels":[{"effort":"none"},{"effort":"low"},{"effort":"high"}],
            "default_reasoning_level":"low","default_reasoning_summary":"none","input_modalities":["text","image"],
            "support_verbosity":true,"default_verbosity":"low"})).unwrap();
        let v1 = opencode_v1(std::slice::from_ref(&model)).unwrap();
        let v2 = opencode_v2(std::slice::from_ref(&model)).unwrap();
        let one = &v1["provider"]["openai"]["models"]["gpt-test"];
        let two = &v2["providers"]["openai"]["models"]["gpt-test"];
        assert_eq!(
            one["limit"],
            json!({"context":100000,"input":85000,"output":0})
        );
        assert_eq!(one["limit"], two["limit"]);
        assert_eq!(one["modalities"]["input"], json!(["text", "image"]));
        assert_eq!(one["options"]["store"], false);
        assert_eq!(one["options"]["reasoningEffort"], "low");
        assert_eq!(one["options"]["textVerbosity"], "low");
        assert_eq!(one["variants"]["high"]["reasoningEffort"], "high");
        assert_eq!(one["reasoning"], true);
        assert!(v1.get("providers").is_none());
        assert!(one.get("capabilities").is_none());
        assert!(two["variants"].is_array());
        assert_eq!(two["capabilities"]["input"], one["modalities"]["input"]);
        assert!(v2.get("provider").is_none());
        assert!(two.get("options").is_none());
        assert_eq!(opencode(&[model]).unwrap(), v2);
    }
    #[test]
    fn projections_keep_real_limits_and_never_invent_an_output_cap() {
        let row = json!({"slug":"a","display_name":"A","visibility":"list","context_window":100_000,"effective_context_window_percent":95,"supported_reasoning_levels":[{"effort":"high","description":"High"}],"input_modalities":["text"],"private_account":"do-not-expose"});
        let model = Model::parse(&row).unwrap();
        assert!(model
            .metadata
            .as_ref()
            .unwrap()
            .get("private_account")
            .is_none());
        assert!(codex(std::slice::from_ref(&model)).is_err());
        let profile = opencode(&[model]).unwrap();
        let value = &profile["providers"]["openai"]["models"]["a"];
        assert_eq!(
            value["limit"],
            json!({"context":100000,"input":95000,"output":0})
        );
        assert_eq!(value["variants"][0]["settings"]["reasoningEffort"], "high");
    }
    #[test]
    fn pool_catalog_intersects_limits_and_refuses_incompatible_protocols() {
        let row = json!({"slug":"a","display_name":"A","context_window":100_000,"supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],"input_modalities":["text","image"],"experimental_supported_tools":[],"shell_type":"shell_command"});
        let mut left = Model::parse(&row).unwrap();
        let mut row = row.clone();
        row["context_window"] = json!(80000);
        row["input_modalities"] = json!(["text"]);
        row["supported_reasoning_levels"] = json!([{"effort":"low"}]);
        left.merge(Model::parse(&row).unwrap());
        assert_eq!(left.metadata.as_ref().unwrap()["context_window"], 80000);
        assert_eq!(
            left.metadata.as_ref().unwrap()["input_modalities"],
            json!(["text"])
        );
        row["shell_type"] = json!("default");
        left.merge(Model::parse(&row).unwrap());
        assert!(left.metadata.is_none());
    }

    #[test]
    fn experimental_context_requires_every_account_without_hiding_the_catalog() {
        let row = json!({"slug":"a","display_name":"A","visibility":"list","context_window":100000,"max_context_window":1000000,"supported_reasoning_levels":[{"effort":"low"}],"input_modalities":["text"],"experimental_supported_tools":[],"shell_type":"shell_command","supported_in_api":true,"priority":1,"support_verbosity":false,"truncation_policy":{"mode":"tokens","limit":10000},"supports_experimental_context":true});
        for support in [Some(true), Some(false), None] {
            let mut other = row.clone();
            if let Some(support) = support {
                other["supports_experimental_context"] = json!(support);
            } else {
                other
                    .as_object_mut()
                    .unwrap()
                    .remove("supports_experimental_context");
            }
            for reverse in [false, true] {
                let mut model = Model::parse(if reverse { &other } else { &row }).unwrap();
                model.merge(Model::parse(if reverse { &row } else { &other }).unwrap());
                let catalog = codex(std::slice::from_ref(&model)).unwrap();
                assert_eq!(
                    catalog["models"][0]["supports_experimental_context"],
                    support == Some(true)
                );
                assert_eq!(catalog["models"][0]["context_window"], 100000);
                assert_eq!(
                    opencode(&[model]).unwrap()["providers"]["openai"]["models"]["a"]["limit"]
                        ["context"],
                    100000
                );
            }
        }
    }
}
