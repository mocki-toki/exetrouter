use super::*;
use crate::{
    affinity, chat,
    oauth::Account,
    upstream::{SseDecoder, SseFrame, Upstream, UpstreamSocket},
    usage::{self, Counters, RequestOutcome, RequestRecord},
};
use axum::{
    body::{Body, Bytes},
    extract::{
        ws::{Message, WebSocket},
        WebSocketUpgrade,
    },
    http::{header, HeaderMap, HeaderValue},
};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

// Fixed event names only: unknown upstream types may contain private content.
#[derive(Default)]
struct LastUpstreamEvent {
    received: Option<(&'static str, i64, Instant)>,
}

impl LastUpstreamEvent {
    fn observe(&mut self, kind: &str, at_ms: i64, now: Instant) {
        const KINDS: &[&str] = &[
            "response.created",
            "response.in_progress",
            "response.metadata",
            "response.output_item.added",
            "response.output_item.done",
            "response.content_part.added",
            "response.content_part.done",
            "response.output_text.delta",
            "response.output_text.done",
            "response.refusal.delta",
            "response.refusal.done",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.custom_tool_call_input.delta",
            "response.custom_tool_call_input.done",
            "response.reasoning_text.delta",
            "response.reasoning_text.done",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_part.done",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.completed",
            "response.incomplete",
            "response.failed",
            "codex.response.metadata",
            "codex.rate_limits",
            "responsesapi.websocket_timing",
            "error",
            "ping",
        ];
        let kind = KINDS
            .iter()
            .copied()
            .find(|known| *known == kind)
            .unwrap_or("other");
        self.received = Some((kind, at_ms, now));
    }

    fn fields(&self, now: Instant) -> (&'static str, Option<i64>, Option<u64>) {
        match self.received {
            Some((kind, at_ms, received)) => (
                kind,
                Some(at_ms),
                Some(
                    now.saturating_duration_since(received)
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                ),
            ),
            None => ("none", None, None),
        }
    }
}

struct RequestLog {
    db: Database,
    id: String,
    start: Instant,
    transport: &'static str,
    request_id: Option<String>,
    response_id: Option<String>,
    created: bool,
    finalized: bool,
    account_id: i64,
    account_generation: i64,
    order: i64,
    rejection_until: Option<i64>,
    user_id: i64,
    context_key: Arc<[u8]>,
    health_order: i64,
    authentication_rejected: bool,
    events_seen: u64,
    output_seen: bool,
    last_event: LastUpstreamEvent,
}

enum JsonReply {
    Response(Value),
    Quota(i64),
    Authentication,
}

impl RequestLog {
    async fn begin(
        state: &AppState,
        record: RequestRecord,
        transport: &'static str,
        account: &Account,
    ) -> Result<Self> {
        let id = record.request_id.clone();
        let user_id = record.user_id;
        let (order, health_order) = state
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                usage::begin_request(&tx, &record)?;
                let order = tx.query_row(
                    "SELECT id FROM usage_events WHERE request_id=?1",
                    [&record.request_id],
                    |row| row.get(0),
                )?;
                let health_order = crate::health::next(&tx)?;
                tx.commit()?;
                Ok((order, health_order))
            })
            .await?;
        Ok(Self {
            db: state.db.clone(),
            id,
            start: Instant::now(),
            transport,
            request_id: None,
            response_id: None,
            created: false,
            finalized: false,
            account_id: account.info.id,
            account_generation: account.info.generation,
            order,
            rejection_until: None,
            user_id,
            context_key: state.key.clone(),
            health_order,
            authentication_rejected: false,
            events_seen: 0,
            output_seen: false,
            last_event: LastUpstreamEvent::default(),
        })
    }
    async fn quota(&self, observation: crate::quota::Observation, now: i64) -> Result<()> {
        if observation.is_empty() {
            return Ok(());
        }
        let (account, generation, order) = (self.account_id, self.account_generation, self.order);
        let result = self
            .db
            .call(move |conn| {
                crate::quota::save(conn, account, generation, order, observation, now)
            })
            .await;
        if result.is_err() {
            tracing::error!(event="quota_store_failed", request_id=%self.id);
        }
        result
    }
    async fn sent(&self) -> Result<()> {
        let id = self.id.clone();
        let transport = self.transport;
        self.db.call(move |conn| {
            if conn.execute("UPDATE usage_events SET status='sent',upstream_transport=?1 WHERE request_id=?2 AND status='accepted'",rusqlite::params![transport,id])?!=1 { return Err("request state changed".into()); }
            Ok(())
        }).await
    }
    async fn health(
        &self,
        rejection: Option<crate::health::Rejection>,
        reason: &'static str,
    ) -> Result<()> {
        self.health_retry(rejection, reason, None).await
    }
    async fn health_retry(
        &self,
        rejection: Option<crate::health::Rejection>,
        reason: &'static str,
        retry: Option<i64>,
    ) -> Result<()> {
        let (id, generation, order) = (self.account_id, self.account_generation, self.health_order);
        let observed = self
            .db
            .call(move |conn| match rejection {
                Some(crate::health::Rejection::Authentication) => crate::health::unauthorized(
                    conn,
                    id,
                    generation,
                    crate::health::Scope::Responses,
                    order,
                    chrono::Utc::now().timestamp(),
                ),
                Some(crate::health::Rejection::Temporary) => crate::health::failure(
                    conn,
                    (id, generation),
                    crate::health::Scope::Responses,
                    order,
                    reason,
                    retry,
                    chrono::Utc::now().timestamp(),
                ),
                None => crate::health::success(
                    conn,
                    id,
                    generation,
                    crate::health::Scope::Responses,
                    order,
                ),
            })
            .await;
        if observed.is_err() {
            tracing::error!(event="health_store_failed",request_id=%self.id);
        }
        observed
    }
    async fn remember(&self, output: &Value) -> Result<()> {
        let digests = affinity::digests(output, &self.context_key, self.user_id)?;
        if digests.is_empty() {
            return Ok(());
        }
        let (user, account, generation) = (self.user_id, self.account_id, self.account_generation);
        let saved = self
            .db
            .call(move |conn| {
                affinity::save(
                    conn,
                    user,
                    account,
                    generation,
                    &digests,
                    chrono::Utc::now().timestamp(),
                )
            })
            .await;
        if saved.is_err() {
            tracing::error!(event="context_binding_failed",request_id=%self.id);
        }
        saved
    }
    async fn finish(&mut self, status: &'static str, counters: Counters) -> Result<()> {
        if self.finalized {
            return Ok(());
        }
        let outcome = RequestOutcome {
            status,
            counters,
            upstream_transport: self.transport,
            upstream_request_id: self.request_id.clone(),
            upstream_response_id: self.response_id.clone(),
            duration_ms: self.start.elapsed().as_millis().min(i64::MAX as u128) as i64,
        };
        let id = self.id.clone();
        let result = self
            .db
            .call(move |conn| usage::finish_request(conn, &id, &outcome))
            .await;
        self.finalized = true;
        if result.is_err() {
            tracing::error!(event="usage_finalize_failed",request_id=%self.id);
        } else {
            let (last_event_kind, last_event_at_ms, last_event_age_ms) =
                self.last_event.fields(Instant::now());
            tracing::info!(event="model_request_finished",request_id=%self.id,status,upstream_transport=self.transport,duration_ms=self.start.elapsed().as_millis() as u64,
                response_created=self.created,events_seen=self.events_seen,output_seen=self.output_seen,
                last_event_kind,last_event_at_ms,last_event_age_ms);
        }
        result
    }
    fn interrupted(&self, reason: &'static str) {
        let (last_event_kind, last_event_at_ms, last_event_age_ms) =
            self.last_event.fields(Instant::now());
        tracing::info!(event="upstream_stream_interrupted",request_id=%self.id,
            upstream_transport=self.transport,response_created=self.created,
            events_seen=self.events_seen,output_seen=self.output_seen,
            last_event_kind,last_event_at_ms,last_event_age_ms,reason);
    }
    async fn observe(&mut self, event: &Value) -> Result<bool> {
        let received = chrono::Utc::now();
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or("upstream event type missing")?;
        self.last_event
            .observe(kind, received.timestamp_millis(), Instant::now());
        let now = received.timestamp();
        for headers in [event.get("headers"), event.pointer("/response/headers")]
            .into_iter()
            .flatten()
        {
            if let Some(headers) = headers.as_object() {
                for (name, value) in headers {
                    if name.eq_ignore_ascii_case("x-codex-turn-state") {
                        self.remember_turn(value.as_str().ok_or("invalid upstream turn state")?)
                            .await?;
                    }
                }
            }
        }
        // Quota metadata must not prevent finalizing known usage or forwarding
        // the terminal event if its separate storage operation fails.
        let observation = crate::quota::event(event, now);
        if matches!(
            event.get("type").and_then(Value::as_str),
            Some("error" | "response.failed")
        ) {
            self.rejection_until = observation.cooldown_until();
        }
        let _ = self.quota(observation, now).await;
        self.events_seen = self.events_seen.saturating_add(1);
        self.output_seen |= matches!(
            kind,
            "response.output_item.added"
                | "response.output_text.delta"
                | "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta"
        );
        if matches!(kind, "error" | "response.failed") {
            // Only fixed events and server-generated IDs enter operational logs.
            tracing::warn!(event="upstream_response_rejected",request_id=%self.id);
        }
        if kind == "response.created" {
            if self.created {
                return Err("duplicate response.created".into());
            }
            self.created = true;
        } else if kind.starts_with("response.") && kind != "response.metadata" && !self.created {
            return Err("response event preceded response.created".into());
        }
        if matches!(
            kind,
            "response.created" | "response.completed" | "response.incomplete" | "response.failed"
        ) && event
            .pointer("/response/id")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err("upstream response ID missing".into());
        }
        if let Some(id) = event.pointer("/response/id").and_then(Value::as_str) {
            if id.len() > 256 || id.chars().any(char::is_control) {
                return Err("invalid upstream response ID".into());
            }
            if self.response_id.as_ref().is_some_and(|old| old != id) {
                return Err("upstream response ID changed".into());
            }
            self.response_id = Some(id.into());
        }
        let status = match kind {
            "response.completed" => Some("completed"),
            "response.incomplete" => Some("incomplete"),
            "response.failed" | "error" => Some("failed"),
            _ => None,
        };
        if let Some(status) = status {
            let counters = Counters::from_json(event.pointer("/response/usage"))?;
            // A binding failure must not discard already known terminal usage.
            self.finish(status, counters).await?;
        }
        if let Some(rejection) = crate::health::rejection(event) {
            self.authentication_rejected =
                matches!(rejection, crate::health::Rejection::Authentication);
            let _ = self
                .health_retry(
                    Some(rejection),
                    "upstream_5xx",
                    crate::health::event_retry(event, now),
                )
                .await;
        } else if matches!(kind, "response.completed" | "response.incomplete") {
            let _ = self.health(None, "success").await;
        }
        let output = match kind {
            "response.completed" | "response.incomplete" => event.pointer("/response/output"),
            "response.output_item.added" | "response.output_item.done" => event.get("item"),
            _ => None,
        };
        if let Some(output) = output {
            self.remember(output).await?;
        }
        Ok(status.is_some())
    }

    async fn remember_turn(&self, value: &str) -> Result<()> {
        let digest = affinity::turn_digest(value, &self.context_key, self.user_id)?;
        let (user, account, generation) = (self.user_id, self.account_id, self.account_generation);
        self.db
            .call(move |conn| {
                affinity::save(
                    conn,
                    user,
                    account,
                    generation,
                    &[digest],
                    chrono::Utc::now().timestamp(),
                )
            })
            .await
    }
}

fn normalize_messages(payload: &mut Value) {
    if let Some(input) = payload.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item["role"] == "system" && item.get("type").is_none_or(|kind| kind == "message") {
                item["role"] = json!("developer");
            }
        }
    }
}
fn prepare(payload: &mut Value, compact: bool) -> Result<bool> {
    capabilities(payload)?;
    // This backend accepts developer instructions but rejects the public API's
    // system role. Preserve message ordering and content during translation.
    normalize_messages(payload);
    let object = payload
        .as_object_mut()
        .ok_or("request must be a JSON object")?;
    if object.contains_key("max_output_tokens") {
        return Err("max_output_tokens is unsupported by this OAuth backend".into());
    }
    if let Some(Value::String(text)) = object.get("input") {
        let text = text.clone();
        object.insert("input".into(), json!([{"role":"user","content":text}]));
    }
    if !object.get("input").is_some_and(Value::is_array) {
        return Err("input must be text or an array".into());
    }
    object.entry("instructions").or_insert(json!(""));
    if object
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Err("HTTP continuation requires full input; previous_response_id is supported only on the original WebSocket connection".into());
    }
    if compact {
        let input = object
            .get_mut("input")
            .and_then(Value::as_array_mut)
            .expect("validated input");
        if input
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
        {
            return Err("compaction input must not contain a compaction trigger".into());
        }
        input.push(json!({"type":"compaction_trigger"}));
        object.entry("instructions").or_insert(json!(""));
        object.insert("store".into(), json!(false));
        object.insert("stream".into(), json!(true));
        return Ok(false);
    }
    if object.get("store").is_some_and(|v| v != &json!(false)) {
        return Err("store must be false".into());
    }
    let streaming = match object.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err("stream must be a boolean".into()),
    };
    object.insert("store".into(), json!(false));
    object.insert("stream".into(), json!(true));
    Ok(streaming)
}

fn capabilities(payload: &Value) -> Result<()> {
    if ["stream_id", "conversation", "context_management"]
        .iter()
        .any(|name| payload.get(name).is_some_and(|v| !v.is_null()))
        || payload
            .get("background")
            .is_some_and(|v| v != &json!(false) && !v.is_null())
    {
        return Err("named streams, saved conversations, context_management and background generation are unsupported by this OAuth backend".into());
    }
    Ok(())
}

fn lite_mode(identity: &Identity, payload: &Value) -> Result<()> {
    if let Some(mode) = &identity.lite_mode {
        let witnessed = payload["input"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["type"] == "additional_tools"))
            || payload
                .pointer(
                    "/client_metadata/ws_request_header_x_openai_internal_codex_responses_lite",
                )
                .and_then(Value::as_str)
                == Some("true");
        if mode != "true" || !witnessed {
            return Err("Responses Lite requires the native additional_tools prefix or recognized Lite frame metadata".into());
        }
    }
    Ok(())
}

fn record(
    identity: &Identity,
    id: &RequestId,
    account: &Account,
    model: &str,
    surface: &'static str,
    client_transport: &'static str,
) -> RequestRecord {
    RequestRecord {
        request_id: id.0.clone(),
        user_id: identity.user_id,
        token_id: identity.token_id.clone(),
        account_id: account.info.id,
        surface,
        model: model.into(),
        client_transport,
    }
}

enum ContextError {
    Invalid,
    Missing,
    Conflict,
    Storage,
}
impl ContextError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request_error",
            Self::Missing => "context_not_found",
            Self::Conflict => "context_account_mismatch",
            Self::Storage => "storage_unavailable",
        }
    }
    fn message(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid or excessive opaque context",
            Self::Missing => {
                "opaque context is unknown or expired; send full text and tool context"
            }
            Self::Conflict => {
                "opaque context belongs to different accounts; send full text and tool context"
            }
            Self::Storage => "context binding storage unavailable",
        }
    }
    fn response(&self) -> Response {
        let storage = matches!(self, Self::Storage);
        (if storage { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST },Json(json!({"error":{"type":if storage {"storage_unavailable"} else {"invalid_request_error"},"code":self.code(),"message":self.message()}}))).into_response()
    }
}
async fn context_account(
    state: &AppState,
    identity: &Identity,
    payload: &Value,
) -> std::result::Result<(Option<i64>, bool), ContextError> {
    let digests = affinity::digests(&payload["input"], &state.key, identity.user_id)
        .map_err(|_| ContextError::Invalid)?;
    if digests.is_empty() {
        return Ok((None, false));
    }
    let user = identity.user_id;
    match state
        .db
        .call(move |conn| affinity::lookup(conn, user, &digests, chrono::Utc::now().timestamp()))
        .await
        .map_err(|_| ContextError::Storage)?
    {
        affinity::Lookup::None => Ok((None, false)),
        affinity::Lookup::Account(id) => Ok((Some(id), false)),
        affinity::Lookup::Portable(id) => Ok((Some(id), true)),
        affinity::Lookup::Missing => Err(ContextError::Missing),
        affinity::Lookup::Conflict => Err(ContextError::Conflict),
    }
}

fn quota_refusal(event: &Value) -> bool {
    event["type"] == "error"
        && event.get("response").is_none()
        && event.get("response_id").is_none()
        && event.get("usage").is_none()
        && ["code", "type"].iter().any(|key| {
            matches!(
                event["error"][*key].as_str(),
                Some(
                    "usage_limit_reached"
                        | "rate_limit_exceeded"
                        | "slow_down"
                        | "rate_limit_error"
                )
            )
        })
}

async fn transfer_context(
    state: &AppState,
    identity: &Identity,
    payload: &mut Value,
    account: &Account,
) -> Result<()> {
    let digests = affinity::digests(&payload["input"], &state.key, identity.user_id)?;
    let (user, id, generation) = (identity.user_id, account.info.id, account.info.generation);
    state
        .db
        .call(move |conn| {
            affinity::transfer(
                conn,
                user,
                id,
                generation,
                &digests,
                chrono::Utc::now().timestamp(),
            )
        })
        .await?;
    if let Some(metadata) = payload
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
    {
        metadata.remove("x-codex-turn-state");
    }
    Ok(())
}

async fn turn_account(
    state: &AppState,
    identity: &Identity,
    turn: Option<&HeaderValue>,
) -> std::result::Result<Option<i64>, ContextError> {
    let Some(value) = turn else {
        return Ok(None);
    };
    let digest = value
        .to_str()
        .ok()
        .and_then(|value| affinity::turn_digest(value, &state.key, identity.user_id).ok())
        .ok_or(ContextError::Invalid)?;
    let user = identity.user_id;
    match state
        .db
        .call(move |conn| affinity::lookup(conn, user, &[digest], chrono::Utc::now().timestamp()))
        .await
    {
        Ok(affinity::Lookup::Account(account)) => Ok(Some(account)),
        Ok(_) => Err(ContextError::Missing),
        Err(_) => Err(ContextError::Storage),
    }
}

fn response_metadata(headers: &HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    result.extend(
        crate::quota::headers(headers, 200, chrono::Utc::now().timestamp()).response_headers(),
    );
    for name in [
        "openai-model",
        "x-openai-model",
        "x-models-etag",
        "x-reasoning-included",
    ] {
        if let Some(value) = headers.get(name) {
            if value.to_str().ok().is_some_and(|text| {
                !text.is_empty()
                    && text.len() <= 128
                    && text.bytes().all(|b| b.is_ascii_graphic())
                    && (name != "x-reasoning-included" || matches!(text, "true" | "false"))
            }) {
                result.insert(header::HeaderName::from_static(name), value.clone());
            }
        }
    }
    result
}

pub(super) async fn http(
    state: Arc<AppState>,
    identity: Identity,
    id: RequestId,
    mut payload: Value,
    model: String,
    surface: &'static str,
    headers: HeaderMap,
) -> Response {
    let chat_request = surface == "chat_completions";
    if let Err(err) = lite_mode(&identity, &payload) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            &err.to_string(),
        );
    }
    let compact = surface == "responses_compact";
    let mut chat_stream = None;
    let prepared = if chat_request {
        match chat::prepare(payload) {
            Ok(request) => {
                payload = request.payload;
                chat_stream = Some(chat::Stream::new(request.include_usage));
                Ok(request.streaming)
            }
            Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"error":{"type":"invalid_request_error","code":"invalid_parameter","param":err.param,"message":err.message}}))).into_response(),
        }
    } else {
        prepare(&mut payload, compact)
    };
    let streaming = match prepared {
        Ok(value) => value,
        Err(err) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &err.to_string(),
            )
        }
    };
    let cache = match crate::cache::prepare(
        &state.key,
        identity.user_id,
        &mut payload,
        identity.cache_hint.as_deref(),
        identity.thread_hint.as_deref(),
    ) {
        Ok(value) => value,
        Err(err) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &err.to_string(),
            )
        }
    };
    let permit = match state.generation_slots.acquire(identity.user_id) {
        Ok(permit) => permit,
        Err(full) => return concurrency_error(full),
    };
    let upstream = state
        .upstream
        .as_ref()
        .expect("configured upstream")
        .clone();
    let mut turn_state = headers.get("x-codex-turn-state").cloned();
    let mut turn_account = match turn_account(&state, &identity, turn_state.as_ref()).await {
        Ok(account) => account,
        Err(err) => return err.response(),
    };
    let (mut context, portable) = match context_account(&state, &identity, &payload).await {
        Ok(context) => context,
        Err(err) => return err.response(),
    };
    if portable && turn_account.is_some() {
        context = turn_account;
    }
    if context.zip(turn_account).is_some_and(|(a, b)| a != b)
        && upstream
            .quota_exhausted(turn_account.expect("conflicting owner"))
            .await
            .unwrap_or(false)
    {
        turn_account = None;
        turn_state = None;
        if let Some(metadata) = payload
            .get_mut("client_metadata")
            .and_then(Value::as_object_mut)
        {
            metadata.remove("x-codex-turn-state");
        }
    }
    if context.zip(turn_account).is_some_and(|(a, b)| a != b) {
        return ContextError::Conflict.response();
    }
    let owner = context.or(turn_account);
    let selected = upstream
        .select_continuation(
            &model,
            cache.preferred.then_some(cache.session.as_str()),
            identity.user_id,
            owner,
        )
        .await;
    let mut account = match selected {
        Ok(account) => account,
        Err(crate::upstream::SelectError::ModelUnavailable) => {
            return error(
                StatusCode::NOT_FOUND,
                "model_not_found",
                "model unavailable",
            )
        }
        Err(crate::upstream::SelectError::Cooldown(until)) => return cooldown_error(until),
        Err(crate::upstream::SelectError::Backoff(until)) => return backoff_error(until),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream_unavailable",
                "OAuth account or model catalog unavailable",
            )
        }
    };
    if owner.is_some_and(|id| id != account.info.id) {
        if transfer_context(&state, &identity, &mut payload, &account)
            .await
            .is_err()
        {
            return ContextError::Storage.response();
        }
        turn_state = None;
    }
    let mut log = match RequestLog::begin(
        &state,
        record(
            &identity,
            &id,
            &account,
            &model,
            surface,
            if streaming { "http_sse" } else { "http_json" },
        ),
        if chat_request {
            "websocket"
        } else {
            "http_sse"
        },
        &account,
    )
    .await
    {
        Ok(log) => log,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
                "request storage unavailable",
            )
        }
    };
    let session = cache.identity_session;
    let thread = cache.thread;
    let stopped = state.stopped.clone();
    let socket = if chat_request {
        tokio::select! {
            _=wait_for_stop(stopped.clone())=> {
                let _=log.finish("interrupted", Counters::default()).await;
                return error(StatusCode::SERVICE_UNAVAILABLE, "service_stopping", "service stopping");
            },
            socket=upstream.websocket(&account,(&session,&thread),true,&payload)=>match socket {
                Ok((socket, _headers))=>Some(socket),
                Err(crate::upstream::SocketError::Cooldown(until))=> {
                    let _=log.finish("local_rejected", Counters::default()).await;
                    return cooldown_error(until);
                },
                Err(crate::upstream::SocketError::Storage)=> {
                    let _=log.finish("storage_error", Counters::default()).await;
                    return error(StatusCode::SERVICE_UNAVAILABLE,"storage_unavailable","quota storage unavailable");
                },
                Err(crate::upstream::SocketError::Backoff(until))=> {let _=log.finish("local_rejected",Counters::default()).await;return backoff_error(until);},
                Err(crate::upstream::SocketError::Authentication)=> {let _=log.finish("local_rejected",Counters::default()).await;return error(StatusCode::BAD_GATEWAY,"upstream_authentication_error","upstream credentials rejected; a separate new request may refresh them");},
                Err(_)=> {
                    log.transport="http_sse";
                    tracing::info!(event="websocket_handshake_failed",request_id=%id.0,fallback="upstream_http");
                    None
                }
            },
        }
    } else {
        None
    };
    if let Err(err) = upstream.check_cooldown(&account).await {
        let _ = log.finish("local_rejected", Counters::default()).await;
        return match err {
            crate::upstream::SelectError::Cooldown(until) => cooldown_error(until),
            crate::upstream::SelectError::Backoff(until) => backoff_error(until),
            _ => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
                "quota storage unavailable",
            ),
        };
    }
    if log.sent().await.is_err() {
        let _ = log.finish("storage_error", Counters::default()).await;
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "request storage unavailable",
        );
    }
    let path = "responses";
    let mut response_turn_state: Option<HeaderValue> = None;
    let mut response_headers = HeaderMap::new();
    let mut source = if let Some(mut socket) = socket {
        payload["type"] = json!("response.create");
        // Streaming is inherent to response.create; Codex WS rejects the HTTP
        // stream parameter. Keep it only when using the HTTP fallback below.
        payload
            .as_object_mut()
            .expect("validated payload")
            .remove("stream");
        let sent = tokio::select! {
            _=wait_for_stop(stopped.clone())=>Err("service stopping".into()),
            sent=send_upstream(&mut socket,UpstreamMessage::Text(payload.to_string().into()))=>sent,
        };
        if sent.is_err() {
            if !*stopped.borrow() {
                let _ = log
                    .health(Some(crate::health::Rejection::Temporary), "transport")
                    .await;
            }
            let _ = log.finish("interrupted", Counters::default()).await;
            return error(
                StatusCode::BAD_GATEWAY,
                "upstream_connection_error",
                "upstream write failed; request outcome may be unknown",
            );
        }
        Source::WebSocket(Box::new(socket))
    } else {
        let mut excluded = Vec::new();
        let response = loop {
            let response = tokio::select! {
                _=wait_for_stop(stopped.clone())=>Err("service stopping".into()),
                response=upstream.post(&account,path,&payload,Some((&session,&thread)),log.health_order,turn_state.as_ref())=>response,
            };
            let response = match response {
                Ok(response) => response,
                Err(_) => {
                    let _ = log.finish("interrupted", Counters::default()).await;
                    return error(
                        StatusCode::BAD_GATEWAY,
                        "upstream_connection_error",
                        "upstream interrupted; request outcome may be unknown",
                    );
                }
            };
            response_headers = response_metadata(response.headers());
            log.request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.len() <= 256)
                .map(str::to_owned);
            let now = chrono::Utc::now().timestamp();
            if !response.status().is_success() {
                let status = response.status();
                let headers = response.headers().clone();
                let body = if status.as_u16() == 429 {
                    tokio::time::timeout(
                        Duration::from_secs(5),
                        crate::oauth::read_json(response, 65_536),
                    )
                    .await
                    .ok()
                    .and_then(std::result::Result::ok)
                } else {
                    None
                };
                let quota_saved = log
                    .quota(
                        crate::quota::http(&headers, status.as_u16(), body.as_ref(), now),
                        now,
                    )
                    .await;
                let accounted = log.finish("upstream_rejected", Counters::default()).await;
                let mut refusal = body.clone().unwrap_or(Value::Null);
                if let Some(object) = refusal.as_object_mut() {
                    object.insert("type".into(), json!("error"));
                }
                if quota_saved.is_ok()
                    && accounted.is_ok()
                    && status.as_u16() == 429
                    && quota_refusal(&refusal)
                    && upstream
                        .account_enabled(identity.user_id, account.info.id)
                        .await
                        .unwrap_or(false)
                {
                    excluded.push(account.info.id);
                    if excluded.len() < 4 {
                        if let Ok(alternate) = upstream
                            .select_excluding(&model, None, Some(identity.user_id), &excluded)
                            .await
                        {
                            if transfer_context(&state, &identity, &mut payload, &alternate)
                                .await
                                .is_err()
                            {
                                return ContextError::Storage.response();
                            }
                            account = alternate;
                            turn_state = None;
                            log = match RequestLog::begin(
                                &state,
                                record(
                                    &identity,
                                    &RequestId::new(),
                                    &account,
                                    &model,
                                    surface,
                                    if streaming { "http_sse" } else { "http_json" },
                                ),
                                "http_sse",
                                &account,
                            )
                            .await
                            {
                                Ok(log) => log,
                                Err(_) => return ContextError::Storage.response(),
                            };
                            if log.sent().await.is_err() {
                                let _ = log.finish("storage_error", Counters::default()).await;
                                return ContextError::Storage.response();
                            }
                            continue;
                        }
                    }
                }
                let response_status = if matches!(status.as_u16(), 400 | 404 | 429) {
                    status
                } else {
                    StatusCode::BAD_GATEWAY
                };
                let mut failure = error(
                    response_status,
                    if status.as_u16() == 401 {
                        "upstream_authentication_error"
                    } else {
                        "upstream_rejected"
                    },
                    if status.as_u16() == 401 {
                        "upstream credentials rejected; a separate new request may refresh them"
                    } else {
                        "upstream rejected the request"
                    },
                );
                if let Some(value) = headers.get("retry-after") {
                    failure.headers_mut().insert("retry-after", value.clone());
                }
                return failure;
            }
            break response;
        };
        let now = chrono::Utc::now().timestamp();
        // The successful inference must still be drained and accounted for if
        // quota persistence fails; it cannot safely be retried.
        let _ = log
            .quota(
                crate::quota::headers(response.headers(), response.status().as_u16(), now),
                now,
            )
            .await;
        // The Codex backend can omit Content-Type. Its missing header must not
        // discard a possibly completed inference: the bounded SSE decoder and
        // response lifecycle below still validate the actual event stream.
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| {
                !s.split(';')
                    .next()
                    .unwrap_or(s)
                    .trim()
                    .eq_ignore_ascii_case("text/event-stream")
            })
        {
            tracing::error!(event="upstream_protocol_rejected",upstream_status=response.status().as_u16(),request_id=%log.id);
            let _ = log
                .health(Some(crate::health::Rejection::Temporary), "protocol")
                .await;
            let _ = log.finish("invalid_response", Counters::default()).await;
            return error(
                StatusCode::BAD_GATEWAY,
                "upstream_protocol_error",
                "expected an SSE stream",
            );
        }
        if let Some(value) = response.headers().get("x-codex-turn-state") {
            if let Some(digest) = value
                .to_str()
                .ok()
                .and_then(|value| affinity::turn_digest(value, &state.key, identity.user_id).ok())
            {
                let (user, account_id, generation) =
                    (identity.user_id, account.info.id, account.info.generation);
                // Continue draining/accounting even when the binding cannot be saved.
                if state
                    .db
                    .call(move |conn| {
                        affinity::save(
                            conn,
                            user,
                            account_id,
                            generation,
                            &[digest],
                            chrono::Utc::now().timestamp(),
                        )
                    })
                    .await
                    .is_ok()
                {
                    response_turn_state = Some(value.clone());
                } else {
                    tracing::error!(event="turn_binding_failed",request_id=%log.id);
                }
            }
        }
        tracing::info!(event="upstream_stream_opened",request_id=%log.id,turn_state_returned=response_turn_state.is_some());
        Source::Http(response)
    };
    let (send, receive) = mpsc::channel::<Bytes>(crate::payload::SSE_QUEUE_CHUNKS);
    let (mut json_send, json_receive) = tokio::sync::oneshot::channel::<Result<JsonReply>>();
    state.jobs.spawn(async move {
        let _permit = permit;
        let _selection = account;
        let mut decoder = SseDecoder::default();
        let mut output = (!streaming || chat_request).then(super::output::CompletionOutput::default);
        let mut terminal = None;
        let result: Result<()> = async {
            loop {
                let frames = tokio::select! {
                    _=wait_for_stop(stopped.clone())=>return Err("service stopping".into()),
                    _=send.closed(),if streaming=>return Err("client disconnected".into()),
                    _=json_send.closed(),if !streaming=>return Err("client disconnected".into()),
                    frames=source.next(&mut decoder)=>match frames {
                        Ok(Some(frames))=>Some(frames),
                        Ok(None)=> {
                            log.interrupted("upstream_eof");
                            let _=log.health(Some(crate::health::Rejection::Temporary),"transport").await;
                            None
                        },
                        Err(err)=> {
                            log.interrupted(if err.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_timeout) {"read_timeout"} else {"transport_or_protocol"});
                            let _=log.health(Some(crate::health::Rejection::Temporary),"transport").await;
                            return Err(err);
                        }
                    },
                };
                let Some(frames) = frames else {
                    if !decoder.is_empty() {
                        return Err("upstream stream ended inside an SSE event".into());
                    }
                    return Err("upstream stream ended without a terminal event".into());
                };
                for mut frame in frames {
                    let ended = match &mut frame.event {
                        Some(event) => {
                            let ended=log.observe(event).await?;
                            if let Some(output)=&mut output {output.observe(event)?;}
                            ended
                        },
                        None => false,
                    };
                    if ended && !streaming {
                        if log.authentication_rejected {terminal=Some(JsonReply::Authentication);return Ok(());}
                        if let Some(until) = log.rejection_until {
                            terminal = Some(JsonReply::Quota(until));
                            return Ok(());
                        }
                        let value = frame
                            .event
                            .as_ref()
                            .and_then(|event| event.get("response"))
                            .cloned();
                        terminal = if compact {
                            Some(super::output::compaction(&value.ok_or("upstream returned no response")?)?)
                        } else if chat_request {
                            Some(chat::completion(
                                &value.ok_or("upstream returned no response")?,
                            )?)
                        } else {
                            value
                        }
                        .map(JsonReply::Response);
                    }
                    if streaming {
                        let chunks = match (&mut chat_stream, &frame.event) {
                            (adapter,_) if log.authentication_rejected=>vec![if adapter.is_some() {format!("data: {}\n\n",ws_error("upstream_authentication_error","upstream credentials rejected")).into_bytes()} else {sse_error("upstream_authentication_error","upstream credentials rejected").into_bytes()}],
                            (Some(_), Some(_)) if ended && log.rejection_until.is_some() => {
                                vec![format!(
                                    "data: {}\n\n",
                                    cooldown_frame(log.rejection_until.expect("checked"))
                                )
                                .into_bytes()]
                            }
                            (Some(adapter), Some(event)) => adapter.event(event)?,
                            (Some(_), None) => Vec::new(),
                            (None, _) => vec![frame.wire],
                        };
                        for chunk in chunks {
                            tokio::time::timeout(
                                Duration::from_secs(10),
                                send.send(Bytes::from(chunk)),
                            )
                            .await
                            .map_err(|_| "client too slow")?
                            .map_err(|_| "client disconnected")?;
                        }
                    }
                    if ended {
                        return Ok(());
                    }
                }
            }
        }
        .await;
        if result.is_err() {
            let _ = log.finish("interrupted", Counters::default()).await;
            if streaming {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    send.send(Bytes::from(if chat_request {
                        chat::stream_error()
                    } else {
                        sse_error(
                            "upstream_interrupted",
                            "stream interrupted; do not retry blindly",
                        )
                        .into_bytes()
                    })),
                )
                .await;
            }
        }
        let _ = json_send.send(if result.is_ok() {
            terminal.ok_or("upstream returned no response".into())
        } else {
            Err("upstream interrupted".into())
        });
    });
    let mut response = if streaming {
        let body = Body::from_stream(futures_util::stream::unfold(
            receive,
            |mut receive| async move {
                next_sse_chunk(&mut receive, Duration::from_secs(15))
                    .await
                    .map(|chunk| (Ok::<_, std::convert::Infallible>(chunk), receive))
            },
        ));
        (
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
                (header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            body,
        )
            .into_response()
    } else {
        drop(receive);
        match json_receive.await {
            Ok(Ok(JsonReply::Response(value))) => Json(value).into_response(),
            Ok(Ok(JsonReply::Quota(until))) => cooldown_error(until),
            Ok(Ok(JsonReply::Authentication)) => error(
                StatusCode::BAD_GATEWAY,
                "upstream_authentication_error",
                "upstream credentials rejected; a separate new request may refresh them",
            ),
            _ => error(
                StatusCode::BAD_GATEWAY,
                "upstream_interrupted",
                "upstream response interrupted",
            ),
        }
    };
    response.headers_mut().extend(response_headers);
    if let Some(value) = response_turn_state {
        response.headers_mut().insert("x-codex-turn-state", value);
    }
    response
}

enum Source {
    Http(reqwest::Response),
    WebSocket(Box<UpstreamSocket>),
}

impl Source {
    async fn next(&mut self, decoder: &mut SseDecoder) -> Result<Option<Vec<SseFrame>>> {
        match self {
            Self::Http(response) => response
                .chunk()
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .map(|chunk| decoder.push(&chunk))
                .transpose(),
            Self::WebSocket(socket) => match crate::upstream::next_ws(socket).await? {
                Some(UpstreamMessage::Text(text)) => {
                    let event =
                        serde_json::from_str(&text).map_err(|_| "invalid upstream frame")?;
                    Ok(Some(vec![SseFrame {
                        wire: Vec::new(),
                        event: Some(event),
                    }]))
                }
                Some(UpstreamMessage::Ping(bytes)) => {
                    send_upstream(socket, UpstreamMessage::Pong(bytes)).await?;
                    Ok(Some(Vec::new()))
                }
                Some(UpstreamMessage::Pong(_)) => Ok(Some(Vec::new())),
                Some(UpstreamMessage::Close(_)) | None => Ok(None),
                _ => Err("unsupported upstream frame".into()),
            },
        }
    }
}

fn ws_error(code: &str, message: &str) -> String {
    if code == "upstream_interrupted" {
        // Codex maps a wrapped HTTP 400 to a terminal InvalidRequest. An
        // unclassified Stream error triggers WS-to-HTTP replay even at zero retries.
        return json!({"type":"error","status":400,"error":{"type":"invalid_request_error","code":code,"message":message}}).to_string();
    }
    json!({"type":"error","error":{"type":code,"code":code,"message":message}}).to_string()
}

fn concurrency_error(full: limits::Full) -> Response {
    error(
        if full == limits::Full::User {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        full.code(),
        full.message(),
    )
}
fn sse_error(code: &str, message: &str) -> String {
    // Keep the generic error for existing SDK readers; Codex recognizes the
    // terminal response.failed envelope rather than arbitrary error events.
    let failed = json!({"type":"response.failed","response":{"status":"failed","error":{"type":code,"code":code,"message":message}}});
    format!(
        "event: error\ndata: {}\n\nevent: response.failed\ndata: {failed}\n\n",
        ws_error(code, message)
    )
}

fn cooldown_frame(until: i64) -> Value {
    json!({"type":"error","status":429,"error":{"type":"upstream_cooldown","code":"upstream_cooldown","message":"upstream limit reached; wait before starting a new request","retry_after":until.saturating_sub(chrono::Utc::now().timestamp()).max(0)}})
}

fn backoff_frame(until: i64) -> Value {
    json!({"type":"error","status":503,"error":{"type":"upstream_backoff","code":"upstream_backoff","message":"upstream account temporarily unavailable; wait before starting a new request","retry_after":until.saturating_sub(chrono::Utc::now().timestamp()).max(0)}})
}
fn backoff_error(until: i64) -> Response {
    let value = backoff_frame(until);
    let retry = value["error"]["retry_after"].to_string();
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error":value["error"]})),
    )
        .into_response();
    if let Ok(retry) = retry.parse() {
        response.headers_mut().insert(header::RETRY_AFTER, retry);
    }
    response
}

fn cooldown_error(until: i64) -> Response {
    let value = cooldown_frame(until);
    let retry = value["error"]["retry_after"].to_string();
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"error":value["error"]})),
    )
        .into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        retry.parse().expect("nonnegative integer"),
    );
    response
}

pub(super) async fn websocket(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    ws: std::result::Result<
        WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Response {
    let Some(upstream) = state.upstream.as_ref().cloned() else {
        return unavailable_get().await;
    };
    let ws = match ws {
        Ok(ws) => ws,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "WebSocket upgrade required",
            )
        }
    };
    let permit = match state.websocket_slots.acquire(identity.user_id) {
        Ok(permit) => permit,
        Err(full) => return concurrency_error(full),
    };
    match upstream.check_available().await {
        Ok(()) => {}
        Err(crate::upstream::SelectError::Cooldown(until)) => return cooldown_error(until),
        Err(crate::upstream::SelectError::Backoff(until)) => return backoff_error(until),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream_unavailable",
                "OAuth account unavailable",
            )
        }
    }
    let jobs = state.jobs.clone();
    ws.max_message_size(MAX_INFERENCE_REQUEST_BYTES)
        .max_frame_size(MAX_INFERENCE_REQUEST_BYTES)
        .on_upgrade(move |client| {
            jobs.track_future(ws_loop(
                client,
                WsSession {
                    state,
                    identity,
                    upstream,
                    _permit: permit,
                },
            ))
        })
}

async fn send_client(client: &mut WebSocket, value: String) -> Result<()> {
    send_client_message(client, Message::Text(value.into())).await
}

async fn send_client_message(client: &mut WebSocket, message: Message) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), client.send(message))
        .await
        .map_err(|_| "client too slow")?
        .map_err(|_| "client disconnected".into())
}

async fn send_upstream(socket: &mut UpstreamSocket, message: UpstreamMessage) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), socket.send(message))
        .await
        .map_err(|_| "upstream write timed out")?
        .map_err(|_| "upstream write failed".into())
}

struct WsSession {
    state: Arc<AppState>,
    identity: Identity,
    upstream: Arc<Upstream>,
    _permit: limits::Permit,
}

struct WsBinding {
    account: crate::upstream::Selection,
    socket: UpstreamSocket,
    metadata: Option<HeaderMap>,
}

async fn ws_loop(mut client: WebSocket, session: WsSession) {
    let WsSession {
        state,
        identity,
        upstream,
        _permit,
    } = session;
    let mut responses = HashSet::new();
    let mut retired_accounts = HashSet::new();
    let mut binding: Option<WsBinding> = None;
    let mut window: Option<super::continuation::Window> = None;
    let mut upstream_session: Option<String> = None;
    let mut upstream_thread: Option<String> = None;
    let mut upstream_identity_session: Option<String> = None;
    let stopped = state.stopped.clone();
    'frames: loop {
        let message = tokio::select! {
            _=wait_for_stop(stopped.clone())=>break,
            message=tokio::time::timeout(Duration::from_secs(300),client.next())=>match message { Ok(Some(Ok(message)))=>message,_=>break },
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            Message::Ping(bytes) => {
                if send_client_message(&mut client, Message::Pong(bytes))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            Message::Pong(_) => continue,
            _ => {
                let _ = send_client(
                    &mut client,
                    ws_error("invalid_request_error", "text JSON frames required"),
                )
                .await;
                continue;
            }
        };
        let bearer = identity.bearer.clone();
        let key = state.key.clone();
        let authentication = state
            .db
            .call(move |conn| authenticate(conn, &key, &bearer))
            .await;
        if !matches!(authentication, Ok(Some(_))) {
            let _ = send_client(
                &mut client,
                ws_error(
                    "authentication_error",
                    "token expired, revoked or authentication storage unavailable",
                ),
            )
            .await;
            break;
        }
        let mut payload: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => {
                let _ = send_client(
                    &mut client,
                    ws_error("invalid_request_error", "invalid JSON frame"),
                )
                .await;
                continue;
            }
        };
        let valid = payload.as_object().is_some_and(|body| {
            body.get("type").and_then(Value::as_str) == Some("response.create")
                && body.get("input").is_some_and(Value::is_array)
        });
        if !valid {
            let _ = send_client(
                &mut client,
                ws_error(
                    "invalid_request_error",
                    "expected response.create with input array",
                ),
            )
            .await;
            continue;
        }
        if let Err(err) = capabilities(&payload) {
            let _ = send_client(
                &mut client,
                ws_error("unsupported_capability", &err.to_string()),
            )
            .await;
            continue;
        }
        if let Err(err) = lite_mode(&identity, &payload) {
            let _ = send_client(
                &mut client,
                ws_error("invalid_request_error", &err.to_string()),
            )
            .await;
            continue;
        }
        if let Some(previous) = payload.get("previous_response_id").filter(|v| !v.is_null()) {
            if !previous.as_str().is_some_and(|id| responses.contains(id)) {
                let _ = send_client(
                    &mut client,
                    ws_error(
                        "previous_response_not_found",
                        "response is not owned by this connection",
                    ),
                )
                .await;
                continue;
            }
        }
        let Some(model) = payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            let _ = send_client(
                &mut client,
                ws_error("invalid_request_error", "model is required"),
            )
            .await;
            continue;
        };
        if payload.get("max_output_tokens").is_some() {
            let _ = send_client(
                &mut client,
                ws_error(
                    "invalid_request_error",
                    "max_output_tokens is unsupported by this OAuth backend",
                ),
            )
            .await;
            continue;
        }
        if payload.get("store").is_some_and(|v| v != &json!(false)) {
            let _ = send_client(
                &mut client,
                ws_error("invalid_request_error", "store must be false"),
            )
            .await;
            continue;
        }
        normalize_messages(&mut payload);
        let body = payload.as_object_mut().expect("validated object");
        body.insert("store".into(), json!(false));
        body.entry("instructions").or_insert(json!(""));
        body.remove("stream");
        let explicit_cache = payload
            .get("prompt_cache_key")
            .is_some_and(|value| !value.is_null());
        let cache = match crate::cache::prepare(
            &state.key,
            identity.user_id,
            &mut payload,
            identity.cache_hint.as_deref(),
            identity.thread_hint.as_deref(),
        ) {
            Ok(value) => value,
            Err(err) => {
                if send_client(
                    &mut client,
                    ws_error("invalid_request_error", &err.to_string()),
                )
                .await
                .is_err()
                {
                    break;
                }
                continue;
            }
        };
        if payload.pointer("/client_metadata/thread_id").is_some()
            && binding.is_some()
            && upstream_thread.as_deref() != Some(cache.thread.as_str())
        {
            let _ = send_client(&mut client, ws_error("thread_changed", "thread identity is fixed for this WebSocket; open a new connection with full context")).await;
            continue;
        }
        if explicit_cache
            && binding.is_some()
            && upstream_session.as_deref() != Some(cache.session.as_str())
        {
            if send_client(&mut client,ws_error("cache_key_changed","cache key is fixed for this WebSocket; open a new connection with full context")).await.is_err() {break;}
            continue;
        }
        let permit = match state.generation_slots.acquire(identity.user_id) {
            Ok(permit) => permit,
            Err(full) => {
                let _ = send_client(&mut client, ws_error(full.code(), full.message())).await;
                continue;
            }
        };
        if retired_accounts.len() >= 1024 {
            let _ = send_client(
                &mut client,
                ws_error(
                    "context_recovery_unavailable",
                    "connection recovery capacity reached; reconnect with full context",
                ),
            )
            .await;
            break 'frames;
        }
        let mut rejected_accounts = Vec::new();
        'attempt: loop {
            let (mut context, portable) = match context_account(&state, &identity, &payload).await {
                Ok(context) => context,
                Err(err) => {
                    if send_client(&mut client, ws_error(err.code(), err.message()))
                        .await
                        .is_err()
                    {
                        break 'frames;
                    }
                    if matches!(err, ContextError::Storage) {
                        break 'frames;
                    }
                    continue 'frames;
                }
            };
            let mut frame_turn = match payload.pointer("/client_metadata/x-codex-turn-state") {
                None | Some(Value::Null) => identity.turn_state.clone(),
                Some(Value::String(value)) => match HeaderValue::from_str(value) {
                    Ok(value) => Some(value),
                    Err(_) => {
                        let _ = send_client(
                            &mut client,
                            ws_error("invalid_request_error", "invalid turn state"),
                        )
                        .await;
                        continue 'frames;
                    }
                },
                _ => {
                    let _ = send_client(
                        &mut client,
                        ws_error("invalid_request_error", "invalid turn state"),
                    )
                    .await;
                    continue 'frames;
                }
            };
            let mut owner = match turn_account(&state, &identity, frame_turn.as_ref()).await {
                Ok(owner) => owner,
                Err(err) => {
                    let _ = send_client(&mut client, ws_error(err.code(), err.message())).await;
                    continue 'frames;
                }
            };
            if owner.is_some_and(|id| retired_accounts.contains(&id)) {
                owner = None;
                frame_turn = None;
                if let Some(metadata) = payload
                    .get_mut("client_metadata")
                    .and_then(Value::as_object_mut)
                {
                    metadata.remove("x-codex-turn-state");
                }
            }
            if portable {
                if let Some(bound) = &binding {
                    context = Some(bound.account.info.id);
                } else if owner.is_some() {
                    context = owner;
                }
            }
            if context.zip(owner).is_some_and(|(a, b)| a != b) {
                let _ = send_client(
                    &mut client,
                    ws_error(
                        "context_account_mismatch",
                        "turn state and opaque context belong to different accounts",
                    ),
                )
                .await;
                continue 'frames;
            }
            let context = context.or(owner);
            if let Some(value) = &frame_turn {
                let metadata = payload
                    .as_object_mut()
                    .expect("validated object")
                    .entry("client_metadata")
                    .or_insert_with(|| json!({}));
                let Some(metadata) = metadata.as_object_mut() else {
                    let _ = send_client(
                        &mut client,
                        ws_error("invalid_request_error", "client_metadata must be an object"),
                    )
                    .await;
                    continue 'frames;
                };
                metadata.insert(
                    "x-codex-turn-state".into(),
                    json!(value.to_str().expect("validated turn state")),
                );
            }
            let mut migrated = false;
            if let Some(bound) = &binding {
                let id = bound.account.info.id;
                if upstream
                    .account_enabled(identity.user_id, id)
                    .await
                    .unwrap_or(false)
                    && upstream.quota_exhausted(id).await.unwrap_or(false)
                {
                    if let Ok(account) = upstream
                        .select_continuation(&model, None, identity.user_id, Some(id))
                        .await
                    {
                        if account.info.id != id {
                            if payload
                                .get("previous_response_id")
                                .is_some_and(|v| !v.is_null())
                                && !window
                                    .as_ref()
                                    .is_some_and(|history| history.expand(&mut payload))
                            {
                                let _ = send_client(&mut client, ws_error("context_recovery_unavailable", "quota exhausted; reconnect with the full current context to switch accounts")).await;
                                continue 'frames;
                            }
                            // Account-specific turn state must not cross an upstream handshake.
                            if let Some(metadata) = payload
                                .get_mut("client_metadata")
                                .and_then(Value::as_object_mut)
                            {
                                metadata.remove("x-codex-turn-state");
                            }
                            let native_session =
                                upstream_identity_session.as_deref().expect("bound session");
                            let thread = upstream_thread.as_deref().expect("bound thread");
                            match upstream
                                .websocket_with_quota_fallback(
                                    account,
                                    identity.user_id,
                                    &model,
                                    (native_session, thread),
                                    &mut payload,
                                )
                                .await
                            {
                                Ok((account, socket, metadata)) => {
                                    if transfer_context(&state, &identity, &mut payload, &account)
                                        .await
                                        .is_err()
                                    {
                                        let _ = send_client(
                                            &mut client,
                                            ws_error(
                                                "storage_unavailable",
                                                "context transfer unavailable",
                                            ),
                                        )
                                        .await;
                                        continue 'frames;
                                    }
                                    let mut old = binding
                                        .replace(WsBinding {
                                            account,
                                            socket,
                                            metadata: Some(metadata),
                                        })
                                        .expect("bound socket");
                                    let _ = tokio::time::timeout(
                                        Duration::from_secs(1),
                                        old.socket.close(None),
                                    )
                                    .await;
                                    frame_turn = None;
                                    migrated = true;
                                    retired_accounts.insert(id);
                                }
                                Err(crate::upstream::SocketError::Cooldown(until)) => {
                                    let _ =
                                        send_client(&mut client, cooldown_frame(until).to_string())
                                            .await;
                                    continue 'frames;
                                }
                                Err(_) => {
                                    let _ = send_client(&mut client, ws_error("upstream_websocket_unavailable", "replacement handshake failed before inference; reconnect with full context")).await;
                                    continue 'frames;
                                }
                            }
                        }
                    }
                }
            }
            if binding.as_ref().is_some_and(|binding| {
                context.is_some_and(|id| id != binding.account.info.id) && !migrated
            }) {
                if send_client(
                    &mut client,
                    ws_error(
                        "context_account_mismatch",
                        "opaque context belongs to a different account than this connection",
                    ),
                )
                .await
                .is_err()
                {
                    break 'frames;
                }
                continue 'frames;
            }
            if binding.is_none() {
                let selected = upstream
                    .select_continuation(
                        &model,
                        cache.preferred.then_some(cache.session.as_str()),
                        identity.user_id,
                        context,
                    )
                    .await;
                let account = match selected {
                    Ok(account) => account,
                    Err(crate::upstream::SelectError::Cooldown(until)) => {
                        if send_client(&mut client, cooldown_frame(until).to_string())
                            .await
                            .is_err()
                        {
                            break 'frames;
                        }
                        continue 'frames;
                    }
                    Err(crate::upstream::SelectError::Backoff(until)) => {
                        if send_client(&mut client, backoff_frame(until).to_string())
                            .await
                            .is_err()
                        {
                            break 'frames;
                        }
                        continue 'frames;
                    }
                    Err(crate::upstream::SelectError::ModelUnavailable) => {
                        if send_client(
                            &mut client,
                            ws_error("model_not_found", "model unavailable"),
                        )
                        .await
                        .is_err()
                        {
                            break 'frames;
                        }
                        continue 'frames;
                    }
                    Err(_) => {
                        let _ = send_client(
                            &mut client,
                            ws_error(
                                "upstream_unavailable",
                                "no available account or model catalog",
                            ),
                        )
                        .await;
                        break 'frames;
                    }
                };
                let initial_id = context.unwrap_or(account.info.id);
                if initial_id != account.info.id {
                    if let Some(metadata) = payload
                        .get_mut("client_metadata")
                        .and_then(Value::as_object_mut)
                    {
                        metadata.remove("x-codex-turn-state");
                    }
                }

                let (account, socket, metadata) = match upstream
                    .websocket_with_quota_fallback(
                        account,
                        identity.user_id,
                        &model,
                        (&cache.identity_session, &cache.thread),
                        &mut payload,
                    )
                    .await
                {
                    Ok(socket) => socket,
                    Err(crate::upstream::SocketError::Cooldown(until)) => {
                        if send_client(&mut client, cooldown_frame(until).to_string())
                            .await
                            .is_err()
                        {
                            break 'frames;
                        }
                        continue 'frames;
                    }
                    Err(crate::upstream::SocketError::Storage) => {
                        let _ = send_client(
                            &mut client,
                            ws_error("storage_unavailable", "quota storage unavailable"),
                        )
                        .await;
                        break 'frames;
                    }
                    Err(crate::upstream::SocketError::Backoff(until)) => {
                        if send_client(&mut client, backoff_frame(until).to_string())
                            .await
                            .is_err()
                        {
                            break 'frames;
                        }
                        continue 'frames;
                    }
                    Err(crate::upstream::SocketError::Authentication) => {
                        let _=send_client(&mut client,ws_error("upstream_authentication_error","upstream credentials rejected; reconnect with full context using a separate new request")).await;
                        break 'frames;
                    }
                    Err(_) => {
                        let _=send_client(&mut client,ws_error("upstream_websocket_unavailable","upstream handshake failed before inference; reconnect using HTTP with full context")).await;
                        break 'frames;
                    }
                };
                if initial_id != account.info.id {
                    if transfer_context(&state, &identity, &mut payload, &account)
                        .await
                        .is_err()
                    {
                        let _ = send_client(
                            &mut client,
                            ws_error("storage_unavailable", "context transfer unavailable"),
                        )
                        .await;
                        continue 'frames;
                    }
                    frame_turn = None;
                    retired_accounts.insert(initial_id);
                }
                upstream_identity_session = Some(cache.identity_session.clone());
                upstream_session = Some(cache.session.clone());
                upstream_thread = Some(cache.thread.clone());
                binding = Some(WsBinding {
                    account,
                    socket,
                    metadata: Some(metadata),
                });
            }
            // A socket's cache/session header cannot change after its handshake.
            // Keep the same upstream key through subsequent frames, even when a
            // client omits it in continuation requests. Account/history pin is unchanged.
            payload["prompt_cache_key"] =
                json!(upstream_session.as_ref().expect("bound cache session"));
            if let Err(err) = crate::cache::identity_metadata(
                &mut payload,
                upstream_identity_session
                    .as_deref()
                    .expect("bound native session"),
                upstream_thread.as_deref().expect("bound native thread"),
            ) {
                let _ = send_client(
                    &mut client,
                    ws_error("invalid_request_error", &err.to_string()),
                )
                .await;
                continue 'frames;
            }
            if let Some(metadata) = payload
                .get_mut("client_metadata")
                .and_then(Value::as_object_mut)
            {
                if metadata.contains_key("session_id") {
                    metadata.insert("session_id".into(), json!(upstream_identity_session));
                }
                if metadata.contains_key("thread_id") {
                    metadata.insert("thread_id".into(), json!(upstream_thread));
                }
                if let Some(value) = &frame_turn {
                    metadata.insert(
                        "x-codex-turn-state".into(),
                        json!(value.to_str().expect("validated turn state")),
                    );
                }
            }
            let WsBinding {
                account,
                socket,
                metadata,
            } = binding.as_mut().expect("initialized binding");
            if !upstream
                .account_enabled(identity.user_id, account.info.id)
                .await
                .unwrap_or(false)
            {
                let _ = send_client(
                    &mut client,
                    ws_error(
                        "account_disabled",
                        "this account is deactivated for your user or by the server operator",
                    ),
                )
                .await;
                break 'frames;
            }
            match upstream.validate(account, &model).await {
                Ok(()) => {}
                Err(crate::upstream::SelectError::Cooldown(until)) => {
                    if send_client(&mut client, cooldown_frame(until).to_string())
                        .await
                        .is_err()
                    {
                        break 'frames;
                    }
                    continue 'frames;
                }
                Err(crate::upstream::SelectError::Backoff(until)) => {
                    if send_client(&mut client, backoff_frame(until).to_string())
                        .await
                        .is_err()
                    {
                        break 'frames;
                    }
                    continue 'frames;
                }
                Err(crate::upstream::SelectError::ModelUnavailable) => {
                    if send_client(&mut client,ws_error("model_not_found","model unavailable on this connection's account; reconnect with full context to select another account")).await.is_err() { break 'frames; }
                    continue 'frames;
                }
                Err(_) => {
                    let _ = send_client(
                        &mut client,
                        ws_error(
                            "upstream_unavailable",
                            "account changed or unavailable; reconnect with full context",
                        ),
                    )
                    .await;
                    break 'frames;
                }
            }
            let id = RequestId::new();
            let mut log = match RequestLog::begin(
                &state,
                record(&identity, &id, account, &model, "responses", "websocket"),
                "websocket",
                account,
            )
            .await
            {
                Ok(log) => log,
                Err(_) => {
                    let _ = send_client(
                        &mut client,
                        ws_error("storage_unavailable", "request storage unavailable"),
                    )
                    .await;
                    continue 'frames;
                }
            };
            let mut rejected_quota = None;
            let mut safe_to_switch = true;
            let mut recovered = None;
            let mut recovery_output = Some(super::output::CompletionOutput::default());
            let result:Result<()>=async {
            let mut handshake_metadata = None;
            if let Some(headers) = metadata.take() {
                let mut safe = response_metadata(&headers);
                if let Some(value) = headers.get("x-codex-turn-state") {
                    log.remember_turn(value.to_str().map_err(|_| "invalid upstream turn state")?).await?;
                    safe.insert("x-codex-turn-state", value.clone());
                }
                if !safe.is_empty() {
                    let headers = safe.iter().map(|(name,value)| (name.as_str().to_owned(), json!(value.to_str().expect("validated metadata")))).collect::<serde_json::Map<_,_>>();
                    handshake_metadata = Some(json!({"type":"response.metadata","headers":headers}).to_string());
                }
            }
            log.sent().await?;
            if send_upstream(socket, UpstreamMessage::Text(payload.to_string().into())).await.is_err() {let _=log.health(Some(crate::health::Rejection::Temporary),"transport").await;return Err("upstream write failed; outcome unknown".into());}
            let mut upstream_deadline = tokio::time::Instant::now() + crate::upstream::INFERENCE_IDLE_TIMEOUT;
            let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            heartbeat.tick().await;
            loop {
                let message=tokio::select! {
                    _ = heartbeat.tick() => { send_client(&mut client, json!({"type":"ping"}).to_string()).await?; continue; },
                    _=wait_for_stop(stopped.clone())=>return Err("service stopping".into()),
                    message=tokio::time::timeout_at(upstream_deadline, socket.next())=>match message {
                        Ok(Some(Ok(message)))=> { upstream_deadline = tokio::time::Instant::now() + crate::upstream::INFERENCE_IDLE_TIMEOUT; message },
                        result=> {log.interrupted(match result { Err(_) => "read_timeout", Ok(None) => "upstream_eof", _ => "transport_or_protocol" });let _=log.health(Some(crate::health::Rejection::Temporary),"transport").await;return Err("upstream closed before terminal event".into());}
                    },
                    incoming=client.next()=>match incoming {
                        Some(Ok(Message::Ping(bytes)))=> { send_client_message(&mut client, Message::Pong(bytes)).await?;continue; },
                        Some(Ok(Message::Pong(_)))=>continue,
                        _=>return Err("client disconnected or sent concurrent request".into()),
                    },
                };
                let text=match message { UpstreamMessage::Text(text)=>text,UpstreamMessage::Ping(bytes)=> { send_upstream(socket, UpstreamMessage::Pong(bytes)).await?;continue; },UpstreamMessage::Pong(_)=>continue,_=> {let _=log.health(Some(crate::health::Rejection::Temporary),"transport").await;return Err("upstream closed or sent invalid frame".into());} };
                let mut event:Value=match serde_json::from_str(&text) {Ok(event)=>event,Err(_)=> {let _=log.health(Some(crate::health::Rejection::Temporary),"protocol").await;return Err("invalid upstream frame".into());} };
                let pre_generation_quota = safe_to_switch && !log.created && quota_refusal(&event);
                safe_to_switch &= matches!(event["type"].as_str(), Some("ping" | "codex.rate_limits" | "codex.response.metadata" | "response.metadata" | "responsesapi.websocket_timing"));
                let terminal=log.observe(&event).await?;
                if pre_generation_quota && terminal { rejected_quota = Some(text.to_string()); return Ok(()); }
                send_client(&mut client,if log.authentication_rejected {ws_error("upstream_authentication_error","upstream credentials rejected; reconnect with full context using a separate new request")} else {text.to_string()}).await?;
                // OpenCode's sequential driver rejects response.* notifications
                // before response.created; Codex also accepts metadata here.
                if event["type"] == "response.created" {
                    if let Some(metadata) = handshake_metadata.take() { send_client(&mut client, metadata).await?; }
                }
                if let Some(output) = &mut recovery_output {
                    if output.observe(&mut event).is_err() { recovery_output = None; }
                }
                if terminal {
                    if event["type"] == "response.completed" && recovery_output.is_some() {
                        recovered = event.get_mut("response").map(Value::take);
                    }
                    return Ok(());
                }
            }
        }.await;
            if result.is_err() {
                let _ = log.finish("interrupted", Counters::default()).await;
                let _ = send_client(
                    &mut client,
                    ws_error(
                        "upstream_interrupted",
                        "connection interrupted; response outcome may be unknown",
                    ),
                )
                .await;
                break 'frames;
            }
            if let Some(refusal) = rejected_quota {
                let old_id = account.info.id;
                rejected_accounts.push(old_id);
                if rejected_accounts.len() < 4
                    && upstream
                        .account_enabled(identity.user_id, old_id)
                        .await
                        .unwrap_or(false)
                {
                    if let Ok(alternate) = upstream
                        .select_excluding(&model, None, Some(identity.user_id), &rejected_accounts)
                        .await
                    {
                        let recoverable = payload
                            .get("previous_response_id")
                            .is_none_or(Value::is_null)
                            || window
                                .as_ref()
                                .is_some_and(|history| history.expand(&mut payload));
                        if recoverable && super::continuation::Window::fits(&payload) {
                            if let Some(meta) = payload
                                .get_mut("client_metadata")
                                .and_then(Value::as_object_mut)
                            {
                                meta.remove("x-codex-turn-state");
                            }
                            if let Ok((alternate, socket, metadata)) = upstream
                                .websocket_with_quota_fallback(
                                    alternate,
                                    identity.user_id,
                                    &model,
                                    (
                                        upstream_identity_session
                                            .as_deref()
                                            .expect("bound session"),
                                        upstream_thread.as_deref().expect("bound thread"),
                                    ),
                                    &mut payload,
                                )
                                .await
                            {
                                if transfer_context(&state, &identity, &mut payload, &alternate)
                                    .await
                                    .is_err()
                                {
                                    let _ = send_client(
                                        &mut client,
                                        ws_error(
                                            "storage_unavailable",
                                            "context transfer unavailable",
                                        ),
                                    )
                                    .await;
                                    break 'frames;
                                }
                                let mut old = binding
                                    .replace(WsBinding {
                                        account: alternate,
                                        socket,
                                        metadata: Some(metadata),
                                    })
                                    .expect("bound socket");
                                let _ = tokio::time::timeout(
                                    Duration::from_secs(1),
                                    old.socket.close(None),
                                )
                                .await;
                                retired_accounts.insert(old_id);
                                continue 'attempt;
                            }
                        }
                    }
                }
                // No capacity/recovery or an ambiguous replacement failure: expose
                // the original confirmed refusal, never fabricate a completion.
                if send_client(&mut client, refusal).await.is_err() {
                    break 'frames;
                }
            }
            if let Some(mut response) = recovered {
                window = super::continuation::Window::completed(
                    window.take(),
                    &mut payload,
                    &mut response,
                    &state.continuation_bytes,
                );
            } else if log.created {
                window = None;
            }
            if log.authentication_rejected {
                break 'frames;
            }
            if let Some(response) = log.response_id {
                if responses.len() >= 1024 {
                    break 'frames;
                }
                responses.insert(response);
            }
            break 'attempt;
        }
        drop(permit);
    }
    if let Some(mut binding) = binding {
        let _ = tokio::time::timeout(Duration::from_secs(1), binding.socket.close(None)).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), client.close()).await;
}

// Data events, unlike SSE comments, wake Codex's event-level idle timer.
// This downstream heartbeat never resets the upstream read deadline.
async fn next_sse_chunk(receive: &mut mpsc::Receiver<Bytes>, interval: Duration) -> Option<Bytes> {
    match tokio::time::timeout(interval, receive.recv()).await {
        Ok(chunk) => chunk,
        Err(_) => Some(Bytes::from_static(
            b"event: ping\ndata: {\"type\":\"ping\"}\n\n",
        )),
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;

    #[test]
    fn last_event_uses_fixed_names_and_monotonic_age() {
        let start = Instant::now();
        let mut event = LastUpstreamEvent::default();
        assert_eq!(event.fields(start), ("none", None, None));
        event.observe("response.function_call_arguments.delta", 2000, start);
        assert_eq!(
            event.fields(start + Duration::from_secs(13)),
            (
                "response.function_call_arguments.delta",
                Some(2000),
                Some(13000)
            )
        );
        for kind in [
            "response.custom_tool_call_input.delta",
            "response.custom_tool_call_input.done",
            "codex.response.metadata",
            "codex.rate_limits",
            "responsesapi.websocket_timing",
        ] {
            event.observe(kind, 2000, start);
            assert_eq!(event.fields(start).0, kind);
        }
        // Neither an arbitrary event type nor upstream-supplied timestamps are retained.
        event.observe(
            "private-content-marker",
            1000,
            start + Duration::from_secs(14),
        );
        assert_eq!(
            event.fields(start + Duration::from_secs(16)),
            ("other", Some(1000), Some(2000))
        );
    }

    #[test]
    fn interruption_has_a_codex_terminal_failure_without_success() {
        let wire = sse_error(
            "upstream_interrupted",
            "stream interrupted; do not retry blindly",
        );
        let mut decoder = SseDecoder::default();
        let frames = decoder.push(wire.as_bytes()).unwrap();
        assert_eq!(frames.len(), 2);
        let failed = frames[1].event.as_ref().unwrap();
        assert_eq!(failed["type"], "response.failed");
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(failed["response"]["error"]["code"], "upstream_interrupted");
        assert!(!wire.contains("response.completed"));
    }

    #[tokio::test]
    async fn heartbeat_preserves_chunks_and_stops_when_producer_closes() {
        let (send, mut receive) = mpsc::channel(1);
        let interval = Duration::from_millis(10);
        let heartbeat = next_sse_chunk(&mut receive, interval).await.unwrap();
        assert_eq!(heartbeat, "event: ping\ndata: {\"type\":\"ping\"}\n\n");
        let terminal = Bytes::from_static(b"event: response.completed\ndata: {}\n\n");
        send.send(terminal.clone()).await.unwrap();
        drop(send);
        assert_eq!(next_sse_chunk(&mut receive, interval).await, Some(terminal));
        assert_eq!(next_sse_chunk(&mut receive, interval).await, None);
    }
}
