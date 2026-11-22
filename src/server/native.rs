//! The unary routes used by released Codex tools. Payloads remain request-local.
use super::*;
use crate::{
    affinity,
    upstream::native::Feature,
    usage::{self, Counters, RequestOutcome, RequestRecord},
};
use axum::body::Body;
use hmac::{Hmac, Mac};
use rusqlite::{params, OptionalExtension};
use sha2::Sha256;

const IMAGE_OUTPUT_BYTES: usize = 48 * 1024 * 1024;
const SEARCH_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const SEARCH_SESSIONS: i64 = 4096;

pub(super) async fn generate(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    handle(
        state,
        identity,
        id,
        headers,
        payload,
        "images/generations",
        None,
    )
    .await
}

pub(super) async fn edit(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    handle(state, identity, id, headers, payload, "images/edits", None).await
}

pub(super) async fn search(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    handle(state, identity, id, headers, payload, "alpha/search", None).await
}

pub(super) async fn mcp_image(
    state: Arc<AppState>,
    identity: Identity,
    id: RequestId,
    body: Value,
    path: &'static str,
    reply: mcp::ImageReply,
) -> Response {
    handle(
        state,
        identity,
        id,
        HeaderMap::new(),
        Ok(Json(body)),
        path,
        Some(reply),
    )
    .await
}

fn scoped(key: &[u8], user: i64, kind: &[u8], value: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    mac.update(b"exetrouter-native-tools-v1\0");
    mac.update(&user.to_be_bytes());
    mac.update(kind);
    mac.update(&[0]);
    mac.update(value.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn identity(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}

fn validate(body: &Value, feature: Feature, edit: bool) -> bool {
    let Some(fields) = body.as_object() else {
        return false;
    };
    if !identity(&body["model"]) || body["model"].as_str().is_some_and(|s| s.len() > 128) {
        return false;
    }
    let allowed: &[&str] = if feature == Feature::Search {
        &[
            "id",
            "model",
            "reasoning",
            "input",
            "commands",
            "settings",
            "max_output_tokens",
        ]
    } else {
        &[
            "model",
            "prompt",
            "background",
            "n",
            "quality",
            "size",
            "images",
        ]
    };
    if fields
        .keys()
        .any(|field| !allowed.contains(&field.as_str()))
    {
        return false;
    }
    if feature == Feature::Search {
        return identity(&body["id"])
            && body.get("commands").is_some_and(Value::is_object)
            && body
                .get("input")
                .is_none_or(|v| v.is_string() || v.is_array());
    }
    if !body["prompt"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty())
        || body.get("n").is_some_and(|v| v.as_u64() != Some(1))
    {
        return false;
    }
    if !edit {
        return !fields.contains_key("images");
    }
    body["images"].as_array().is_some_and(|images| {
        !images.is_empty()
            && images.len() <= 5
            && images.iter().all(|image| {
                image.as_object().is_some_and(|fields| fields.len() == 1)
                    && image["image_url"].as_str().is_some_and(|url| {
                        url.starts_with("data:image/") || url.starts_with("https://")
                    })
            })
    })
}

// Backend file references have not been issued/authorized by this router.
fn has_resource_reference(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.contains_key("file_id")
                || fields.contains_key("conversation")
                || fields.contains_key("previous_response_id")
                || fields.values().any(has_resource_reference)
        }
        Value::Array(values) => values.iter().any(has_resource_reference),
        _ => false,
    }
}

fn has_search_reference(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.get("ref_id").is_some_and(|v| {
                v.as_str()
                    .is_none_or(|s| !(s.starts_with("https://") || s.starts_with("http://")))
            }) || fields.values().any(has_search_reference)
        }
        Value::Array(values) => values.iter().any(has_search_reference),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle(
    state: Arc<AppState>,
    identity: Identity,
    id: RequestId,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
    path: &'static str,
    reply: Option<mcp::ImageReply>,
) -> Response {
    let feature = if path == "alpha/search" {
        Feature::Search
    } else {
        Feature::Images
    };
    let Ok(Json(mut body)) = payload else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid JSON request body",
        );
    };
    if !validate(&body, feature, path == "images/edits") || has_resource_reference(&body) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "unsupported or invalid native tool request",
        );
    }
    let model = body["model"].as_str().expect("validated").to_owned();
    if feature == Feature::Images && model != "gpt-image-2" {
        return error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            "image model is not supported by the reviewed native contract",
        );
    }
    let Some(upstream) = state.upstream.clone() else {
        return unavailable_get().await;
    };
    let _permit = match state.generation_slots.acquire(identity.user_id) {
        Ok(permit) => permit,
        Err(full) => {
            return error(
                if full == limits::Full::User {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                full.code(),
                full.message(),
            )
        }
    };
    // A separate output-buffer budget, not an input/context admission ceiling.
    let Ok(_output_permit) = state.native_slots.clone().try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_busy",
            "native tool capacity reached",
        );
    };
    let user = identity.user_id;
    let mut session = None;
    let mut pinned = None;
    if feature == Feature::Search {
        let digest = scoped(
            &state.key,
            user,
            b"search",
            body["id"].as_str().expect("validated"),
        );
        let lookup_digest = digest.clone();
        let refs = match affinity::references(&body["input"], &state.key, user) {
            Ok(refs) => refs,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "context_not_found",
                    "invalid native search context",
                )
            }
        };
        let bound = state.db.call(move |conn| {
            let account = conn.query_row("SELECT account_id FROM native_search_sessions WHERE user_id=?1 AND digest=?2", params![user,lookup_digest], |r| r.get::<_,i64>(0)).optional()?;
            Ok((account, affinity::lookup_references(conn, user, &refs)?))
        }).await;
        let Ok((account, owner)) = bound else {
            return storage_error();
        };
        let context_owner = match owner {
            affinity::Lookup::None => None,
            affinity::Lookup::Account(id) | affinity::Lookup::Portable(id) => Some(id),
            affinity::Lookup::Conflict => return context_conflict(),
        };
        if account.zip(context_owner).is_some_and(|(a, b)| a != b) {
            return context_conflict();
        }
        if account.is_none() && has_search_reference(&body["commands"]) {
            return error(
                StatusCode::BAD_REQUEST,
                "context_not_found",
                "search references require an existing session; start with a query or literal URL",
            );
        }
        pinned = account.or(context_owner);
        body["id"] = json!(hex::encode(&digest));
        session = Some(digest);
        body = match affinity::upstream_payload(&body, &state.key, user) {
            Ok(body) => body,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "context_not_found",
                    "invalid native search context",
                )
            }
        };
    }
    let hint = session.as_ref().map(hex::encode);
    let mut account = match upstream
        .select_native(feature, &model, hint.as_deref(), pinned)
        .await
    {
        Ok(account) => account,
        Err(err) => return selection_error(err),
    };
    if let Some(digest) = session {
        let selected = account.info.id;
        let bound = state.db.call(move |conn| {
            let tx = conn.transaction()?;
            let old = tx.query_row("SELECT account_id FROM native_search_sessions WHERE user_id=?1 AND digest=?2", params![user,digest], |row| row.get::<_,i64>(0)).optional()?;
            if let Some(old) = old { return Ok(Some(old)); }
            if tx.query_row("SELECT COUNT(*) FROM native_search_sessions", [], |row| row.get::<_,i64>(0))? >= SEARCH_SESSIONS { return Ok(None); }
            tx.execute("INSERT INTO native_search_sessions VALUES(?1,?2,?3)", params![user,digest,selected])?;
            tx.commit()?;
            Ok(Some(selected))
        }).await;
        match bound {
            Ok(Some(owner)) if owner != selected => {
                if pinned.is_some_and(|id| id != owner) {
                    return context_conflict();
                }
                account = match upstream
                    .select_native(feature, &model, hint.as_deref(), Some(owner))
                    .await
                {
                    Ok(account) => account,
                    Err(err) => return selection_error(err),
                };
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "search_session_capacity",
                    "search session registry is full",
                )
            }
            Err(_) => return storage_error(),
        }
    }
    let mut native_headers = HeaderMap::new();
    if let Some(turn) = headers.get("x-codex-image-turn-id") {
        let Ok(turn) = turn.to_str() else {
            return context_conflict();
        };
        if turn.is_empty() || turn.len() > 256 {
            return context_conflict();
        }
        native_headers.insert(
            "x-codex-image-turn-id",
            HeaderValue::from_str(&hex::encode(scoped(&state.key, user, b"image-turn", turn)))
                .expect("hex"),
        );
    }
    if let Some(metadata) = headers.get("x-codex-turn-metadata") {
        let Ok(metadata) = metadata.to_str() else {
            return context_conflict();
        };
        if metadata.len() > 65536 {
            return context_conflict();
        }
        let mut envelope = json!({"client_metadata":{"x-codex-turn-metadata":metadata}});
        let Ok(turn) = serde_json::from_str::<Value>(metadata) else {
            return context_conflict();
        };
        let session_id = hex::encode(scoped(
            &state.key,
            user,
            b"session",
            turn["session_id"]
                .as_str()
                .or(identity.cache_hint.as_deref())
                .unwrap_or(""),
        ));
        let thread_id = hex::encode(scoped(
            &state.key,
            user,
            b"thread",
            turn["thread_id"]
                .as_str()
                .or(identity.thread_hint.as_deref())
                .unwrap_or(""),
        ));
        if crate::cache::identity_metadata(&mut envelope, &session_id, &thread_id).is_err() {
            return context_conflict();
        }
        let Ok(value) = HeaderValue::from_str(
            envelope["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .expect("serialized"),
        ) else {
            return context_conflict();
        };
        native_headers.insert("x-codex-turn-metadata", value);
    }
    if upstream.native_eligible(&account, feature).await.is_err() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream_unavailable",
            "native account is no longer available",
        );
    }
    let record = RequestRecord {
        request_id: id.0.clone(),
        user_id: user,
        token_id: identity.token_id,
        account_id: account.info.id,
        surface: reply.as_ref().map_or(path, |reply| reply.surface),
        model,
        client_transport: "http",
    };
    let order = match state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            if !crate::platform::Policy::read(&tx)?.codex() {
                return Err("Codex routing is disabled".into());
            }
            usage::begin_request(&tx, &record)?;
            let order = crate::health::next(&tx)?;
            tx.commit()?;
            Ok(order)
        })
        .await
    {
        Ok(order) => order,
        Err(_) => return storage_error(),
    };
    let (mut send, receive) = tokio::sync::oneshot::channel();
    let jobs = state.jobs.clone();
    jobs.spawn(async move {
        let _permit = _permit;
        let start = Instant::now();
        let mut counters = Counters::default();
        let result = tokio::select! {
            _ = send.closed() => Err("interrupted"),
            _ = wait_for_stop(state.stopped.clone()) => Err("interrupted"),
            result = run(&state, &upstream, &account, &id, order, feature, path, &body, native_headers, &mut counters) => result,
        };
        let result=match (result,reply) {
            (Ok((status,value)),Some(reply)) if status.is_success()=>mcp::image_result(value,reply).map(|value|(status,value)),
            (result,_)=>result,
        };
        let status = match &result { Ok((status, _)) if status.is_success() => "completed", Ok(_) => "failed", Err(status) => status };
        let outcome = RequestOutcome { status, counters, upstream_transport: "http", upstream_request_id: None, upstream_response_id: None,
            duration_ms: start.elapsed().as_millis().min(i64::MAX as u128) as i64 };
        let request_id = id.0.clone();
        if state.db.call(move |conn| usage::finish_request(conn, &request_id, &outcome)).await.is_err() {
            tracing::error!(event="usage_finalize_failed",request_id=%id.0);
        }
        let response = match result {
            Ok((status, value)) => native_response(status, value, _output_permit),
            Err(_) => error(StatusCode::BAD_GATEWAY, "upstream_interrupted", "native tool interrupted; do not retry blindly"),
        };
        let _ = send.send(response);
    });
    receive.await.unwrap_or_else(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "upstream_interrupted",
            "native tool interrupted; do not retry blindly",
        )
    })
}

struct OutputBuffer {
    bytes: Vec<u8>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl AsRef<[u8]> for OutputBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

fn native_response(
    status: StatusCode,
    value: Value,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return storage_error();
    };
    // Keep the output slot while downstream/Hyper retains any response bytes,
    // not merely until inference or the handler finishes.
    let bytes = axum::body::Bytes::from_owner(OutputBuffer {
        bytes,
        _permit: permit,
    });
    (
        status,
        [
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
        ],
        Body::from(bytes),
    )
        .into_response()
}

#[allow(clippy::too_many_arguments)]
async fn run(
    state: &AppState,
    upstream: &crate::upstream::Upstream,
    account: &crate::oauth::Account,
    id: &RequestId,
    order: i64,
    feature: Feature,
    path: &'static str,
    body: &Value,
    headers: HeaderMap,
    counters: &mut Counters,
) -> std::result::Result<(StatusCode, Value), &'static str> {
    upstream
        .native_eligible(account, feature)
        .await
        .map_err(|_| "interrupted")?;
    let response = upstream
        .post_native(account, path, body, headers)
        .await
        .map_err(|_| "interrupted")?;
    let status = response.status();
    let response_headers = response.headers().clone();
    let limit = if status.is_success() {
        if feature == Feature::Images {
            IMAGE_OUTPUT_BYTES
        } else {
            SEARCH_OUTPUT_BYTES
        }
    } else {
        65536
    };
    let parsed = crate::oauth::read_json(response, limit).await;
    // Preserve validated counters even if a subsequent health write, response
    // shape check or client cancellation prevents successful delivery.
    if let Ok(value) = &parsed {
        if let Ok(observed) = Counters::from_json(value.get("usage")) {
            *counters = observed;
        }
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        let now = chrono::Utc::now().timestamp();
        let retry = response_headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| crate::quota::retry_at(v, now));
        let reset = parsed
            .as_ref()
            .ok()
            .and_then(|v| v.pointer("/error/resets_at"))
            .and_then(Value::as_i64)
            .filter(|at| *at > now && *at <= 253_402_300_799);
        // The reviewed parser establishes the general Codex header family,
        // not a particular image-meter ID. Never guess an image quota prefix.
        let prefix = "x-codex";
        let window_reset = ["primary", "secondary"]
            .iter()
            .filter_map(|kind| {
                if feature == Feature::Images {
                    return None;
                }
                let used = response_headers
                    .get(format!("{prefix}-{kind}-used-percent"))?
                    .to_str()
                    .ok()?
                    .parse::<f64>()
                    .ok()?;
                if !used.is_finite() || used < 100.0 {
                    return None;
                }
                response_headers
                    .get(format!("{prefix}-{kind}-reset-at"))?
                    .to_str()
                    .ok()?
                    .parse::<i64>()
                    .ok()
                    .filter(|at| *at > now && *at <= 253_402_300_799)
            })
            .max();
        let until = retry
            .into_iter()
            .chain(reset)
            .chain(window_reset)
            .max()
            .unwrap_or(
                now + if status == StatusCode::TOO_MANY_REQUESTS {
                    60
                } else {
                    5
                },
            );
        let (account_id, generation) = (account.info.id, account.info.generation);
        if state
            .db
            .call(move |conn| {
                crate::upstream::native::pause(
                    conn,
                    account_id,
                    generation,
                    feature,
                    until,
                    order,
                    status == StatusCode::TOO_MANY_REQUESTS,
                )
            })
            .await
            .is_err()
        {
            tracing::error!(event="quota_store_failed",request_id=%id.0);
        }
    }
    if status == StatusCode::UNAUTHORIZED {
        let (account_id, generation) = (account.info.id, account.info.generation);
        state
            .db
            .call(move |conn| {
                crate::health::unauthorized(
                    conn,
                    account_id,
                    generation,
                    crate::health::Scope::Responses,
                    order,
                    chrono::Utc::now().timestamp(),
                )
            })
            .await
            .map_err(|_| "interrupted")?;
        return Ok((
            StatusCode::BAD_GATEWAY,
            json!({"error":{"type":"upstream_authentication_error","message":"upstream credentials rejected; a separate request may refresh them"}}),
        ));
    }
    if !status.is_success() {
        return Ok((
            status,
            json!({"error":{"type":"upstream_rejected","message":"upstream rejected native tool request"}}),
        ));
    }
    let mut value = parsed.map_err(|_| "invalid_response")?;
    *counters = Counters::from_json(value.get("usage")).map_err(|_| "invalid_response")?;
    let valid = if feature == Feature::Images {
        value["created"].is_u64()
            && value["data"].as_array().is_some_and(|items| {
                items.len() == 1 && items[0]["b64_json"].as_str().is_some_and(valid_base64)
            })
    } else {
        value["output"].is_string()
            && value
                .get("results")
                .is_none_or(|v| v.is_null() || v.is_array())
    };
    if !valid || value.get("error").is_some_and(|v| !v.is_null()) {
        return Err("invalid_response");
    }
    // Native search currently consumes plaintext output, not encrypted_output.
    // Never expose a new unbound opaque continuation accidentally.
    if feature == Feature::Search {
        value
            .as_object_mut()
            .ok_or("invalid_response")?
            .remove("encrypted_output");
    }
    Ok((status, value))
}

fn valid_base64(value: &str) -> bool {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return false;
    }
    let (prefix, tail) = bytes.split_at(bytes.len() - 4);
    // Validate without allocating another image-sized buffer or decoding input
    // attachments. Only the final quartet can contain canonical padding.
    prefix
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
        && STANDARD.decode_slice(tail, &mut [0; 3]).is_ok()
}

fn storage_error() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_unavailable",
        "native tool storage unavailable",
    )
}
fn context_conflict() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "context_account_mismatch",
        "invalid or conflicting native context",
    )
}
fn selection_error(err: crate::upstream::SelectError) -> Response {
    match err {
        crate::upstream::SelectError::ModelUnavailable => error(StatusCode::NOT_FOUND, "model_not_found", "model is unavailable"),
        crate::upstream::SelectError::Cooldown(at) => (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":{"type":"upstream_cooldown","message":"native tool quota is cooling down","retry_at":at}}))).into_response(),
        crate::upstream::SelectError::Backoff(at) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":{"type":"upstream_backoff","message":"upstream is temporarily unavailable","retry_at":at}}))).into_response(),
        crate::upstream::SelectError::UpstreamUnavailable => error(StatusCode::SERVICE_UNAVAILABLE, "upstream_unavailable", "no eligible native tool account"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_canonical_without_an_image_sized_decode_buffer() {
        for value in ["aW1hZ2U=", "YQ==", "YWJj", "YWJjZA=="] {
            assert!(valid_base64(value));
        }
        for value in [
            "", "a", "YQ=", "YQ===", "YQ==YQ==", "YR==", "bad!", "YW Jj", "YWJj\n",
        ] {
            assert!(!valid_base64(value));
        }
    }

    #[test]
    fn output_permit_outlives_every_retained_body_clone() {
        let slots = Arc::new(Semaphore::new(1));
        let bytes = axum::body::Bytes::from_owner(OutputBuffer {
            bytes: vec![0; 8],
            _permit: slots.clone().try_acquire_owned().unwrap(),
        });
        let clone = bytes.clone();
        drop(bytes);
        assert_eq!(slots.available_permits(), 0);
        drop(clone);
        assert_eq!(slots.available_permits(), 1);
    }
}
