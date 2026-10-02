//! Cache hints are scoped to a router user, never to a bearer or raw prompt.
use crate::Result;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

pub(crate) struct Affinity {
    pub session: String,
    pub identity_session: String,
    pub thread: String,
    pub preferred: bool,
}

/// The default separates users without pretending to identify a conversation.
/// Explicit client keys provide a soft account preference; opaque context wins.
pub(crate) fn prepare(
    key: &[u8],
    user: i64,
    payload: &mut Value,
    hint: Option<&str>,
    thread_hint: Option<&str>,
) -> Result<Affinity> {
    let model = payload["model"].as_str().ok_or("model is required")?;
    let supplied = match payload.get("prompt_cache_key") {
        None | Some(Value::Null) => hint,
        Some(Value::String(value))
            if !value.is_empty()
                && value.chars().count() <= 64
                && !value.chars().any(char::is_control) =>
        {
            Some(value.as_str())
        }
        _ => {
            return Err(
                "prompt_cache_key must be a nonempty string of at most 64 characters".into(),
            )
        }
    };
    if supplied.is_some_and(|value| {
        value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
    }) {
        return Err("invalid client session hint".into());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(b"exetrouter-cache-v1\0");
    mac.update(&user.to_be_bytes());
    mac.update(&(model.len() as u64).to_be_bytes());
    mac.update(model.as_bytes());
    mac.update(if supplied.is_some() {
        b"explicit\0"
    } else {
        b"default\0"
    });
    mac.update(supplied.unwrap_or("").as_bytes());
    let session = hex::encode(mac.finalize().into_bytes());
    let preferred = supplied.is_some();
    let identity_hint = match payload.pointer("/client_metadata/session_id") {
        None | Some(Value::Null) => hint,
        Some(Value::String(value)) => Some(value.as_str()),
        _ => return Err("invalid client session hint".into()),
    };
    let identity_session = if let Some(hint) = identity_hint {
        if hint.is_empty() || hint.len() > 256 || hint.chars().any(char::is_control) {
            return Err("invalid client session hint".into());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
        mac.update(b"exetrouter-cache-v1\0");
        mac.update(&user.to_be_bytes());
        mac.update(&(model.len() as u64).to_be_bytes());
        mac.update(model.as_bytes());
        mac.update(b"explicit\0");
        mac.update(hint.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    } else {
        session.clone()
    };
    let thread_hint = match payload.pointer("/client_metadata/thread_id") {
        None | Some(Value::Null) => thread_hint,
        Some(Value::String(value)) => Some(value.as_str()),
        _ => return Err("invalid client thread hint".into()),
    };
    let thread = if let Some(hint) = thread_hint {
        if hint.is_empty() || hint.len() > 256 || hint.chars().any(char::is_control) {
            return Err("invalid client thread hint".into());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
        mac.update(b"exetrouter-thread-v1\0");
        mac.update(&user.to_be_bytes());
        mac.update(&(model.len() as u64).to_be_bytes());
        mac.update(model.as_bytes());
        mac.update(hint.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    } else {
        session.clone()
    };
    payload["prompt_cache_key"] = json!(session);
    identity_metadata(payload, &identity_session, &thread)?;
    Ok(Affinity {
        session,
        identity_session,
        thread,
        preferred,
    })
}

/// Native identities appear both directly and inside serialized turn metadata.
/// Project the same scoped values without altering the tool inventory or flags.
pub(crate) fn identity_metadata(payload: &mut Value, session: &str, thread: &str) -> Result<()> {
    if let Some(metadata) = payload
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
    {
        if let Some(encoded) = metadata.get("x-codex-turn-metadata") {
            let mut turn: Value =
                serde_json::from_str(encoded.as_str().ok_or("invalid native turn metadata")?)
                    .map_err(|_| "invalid native turn metadata")?;
            let turn = turn.as_object_mut().ok_or("invalid native turn metadata")?;
            for (name, scoped) in [("session_id", session), ("thread_id", thread)] {
                if let Some(value) = turn.get(name).filter(|v| !v.is_null()) {
                    let value = value.as_str().ok_or("invalid native turn identity")?;
                    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
                    {
                        return Err("invalid native turn identity".into());
                    }
                    turn.insert(name.into(), json!(scoped));
                }
            }
            metadata.insert(
                "x-codex-turn-metadata".into(),
                json!(serde_json::to_string(turn)?),
            );
        }
        if metadata.contains_key("session_id") {
            metadata.insert("session_id".into(), json!(session));
        }
        if metadata.contains_key("thread_id") {
            metadata.insert("thread_id".into(), json!(thread));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialized_turn_identity_matches_scoped_frame_and_preserves_tool_inventory() {
        let inventory = json!({"namespaces":[{"name":"functions","tools":["exec"]}]});
        let mut body = json!({"model":"a","client_metadata":{"session_id":"raw-native-session","thread_id":"raw-native-thread","x-codex-turn-metadata":json!({"session_id":"raw-native-session","thread_id":"raw-native-thread","tool_namespaces_info":inventory,"request_kind":"model"}).to_string()}});
        let scoped = prepare(&[1; 32], 1, &mut body, None, None).unwrap();
        let turn: Value = serde_json::from_str(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(turn["session_id"], scoped.identity_session);
        assert_eq!(turn["thread_id"], scoped.thread);
        assert_eq!(turn["tool_namespaces_info"], inventory);
        assert_eq!(turn["request_kind"], "model");
        assert!(!body.to_string().contains("raw-native-"));
        let original = body.clone();
        identity_metadata(&mut body, &scoped.identity_session, &scoped.thread).unwrap();
        assert_eq!(body, original);
        identity_metadata(&mut body, "bound-session", "bound-thread").unwrap();
        let turn: Value = serde_json::from_str(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(turn["thread_id"], "bound-thread");
        assert_eq!(body["client_metadata"]["thread_id"], "bound-thread");
    }
    #[test]
    fn invalid_serialized_native_metadata_fails_without_echoing_values() {
        for encoded in [
            json!("private-invalid-json"),
            json!("[]"),
            json!("null"),
            json!(123),
            json!(json!({"thread_id":123}).to_string()),
            json!(json!({"session_id":"x".repeat(257)}).to_string()),
        ] {
            let error = prepare(
                &[1; 32],
                1,
                &mut json!({"model":"a","client_metadata":{"x-codex-turn-metadata":encoded}}),
                None,
                None,
            )
            .err()
            .unwrap();
            assert!(!error.to_string().contains("private-invalid-json"));
        }
    }
    #[test]
    fn keys_survive_rotation_and_restart_without_crossing_users_models_or_defaults() {
        let explicit = |user, model| {
            prepare(
                &[1; 32],
                user,
                &mut json!({"model":model,"prompt_cache_key":"session"}),
                None,
                None,
            )
            .unwrap()
            .session
        };
        assert_eq!(explicit(1, "a"), explicit(1, "a"));
        assert_ne!(explicit(1, "a"), explicit(2, "a"));
        assert_ne!(explicit(1, "a"), explicit(1, "b"));
        let mut body = json!({"model":"a"});
        let default = prepare(&[1; 32], 1, &mut body, None, None).unwrap();
        assert!(!default.preferred);
        assert_eq!(default.session.len(), 64);
        assert_ne!(default.session, explicit(1, "a"));
        assert_eq!(
            default.session,
            prepare(&[1; 32], 1, &mut json!({"model":"a"}), None, None)
                .unwrap()
                .session
        );
        for value in [json!(""), json!("x".repeat(65)), json!("a\nb"), json!(10)] {
            assert!(prepare(
                &[1; 32],
                1,
                &mut json!({"model":"a","prompt_cache_key":value}),
                None,
                None
            )
            .is_err());
        }
    }
}
