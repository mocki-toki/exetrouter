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

const WS_PING_INTERVAL: Duration = Duration::from_secs(15);
const WS_PRE_RESPONSE_SILENCE: Duration = Duration::from_secs(90);
const WS_UPSTREAM_IDLE: Duration = Duration::from_secs(30);
const WS_UPSTREAM_MAX_AGE: Duration = Duration::from_secs(300);
const WS_CLIENT_IDLE: Duration = Duration::from_secs(300);

fn ws_retirement_deadline(connected: Instant, idle_since: Instant) -> Instant {
    (connected + WS_UPSTREAM_MAX_AGE).min(idle_since + WS_UPSTREAM_IDLE)
}

// Pongs and control frames prove life, not acceptance or semantic progress.
struct WsLiveness {
    last_frame: Instant,
    last_peer_frame: Option<Instant>,
    last_pong: Option<Instant>,
    response_started: bool,
    pings: u64,
    pongs: u64,
}

impl WsLiveness {
    fn new(now: Instant) -> Self {
        Self {
            last_frame: now,
            last_peer_frame: None,
            last_pong: None,
            response_started: false,
            pings: 0,
            pongs: 0,
        }
    }

    fn deadline(&self) -> Instant {
        self.last_frame
            + if self.response_started {
                crate::upstream::INFERENCE_IDLE_TIMEOUT
            } else {
                WS_PRE_RESPONSE_SILENCE
            }
    }

    fn received(&mut self, now: Instant, pong: bool) {
        self.last_frame = now;
        self.last_peer_frame = Some(now);
        if pong {
            self.pongs = self.pongs.saturating_add(1);
            self.last_pong = Some(now);
        }
    }
}

// Never retain an error payload or a WebSocket close reason in diagnostics.
#[derive(Debug)]
struct WsInterruption {
    reason: &'static str,
    stage: &'static str,
    transport_error_kind: Option<&'static str>,
    close_code: Option<u16>,
}

impl WsInterruption {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            stage: "receive",
            transport_error_kind: None,
            close_code: None,
        }
    }

    fn at(mut self, stage: &'static str) -> Self {
        self.stage = stage;
        self
    }

    fn failure_side(&self) -> &'static str {
        if self.reason.starts_with("upstream_")
            || matches!(self.reason, "read_timeout" | "pre_response_silence_timeout")
        {
            "upstream"
        } else if self.reason.starts_with("client_") {
            "client"
        } else {
            "router"
        }
    }

    fn client_error(&self, id: &RequestId) -> String {
        let close = self
            .close_code
            .map(|code| format!(", close_code={code}"))
            .unwrap_or_default();
        ws_error(
            "upstream_interrupted",
            &format!(
                "connection interrupted (reason={}{close}, stage={}, request={})",
                self.reason, self.stage, id.0
            ),
        )
    }

    fn transport(reason: &'static str, error: &tokio_tungstenite::tungstenite::Error) -> Self {
        use std::io::ErrorKind;
        use tokio_tungstenite::tungstenite::{error::ProtocolError, Error};
        let kind = match error {
            Error::ConnectionClosed => "connection_closed",
            Error::AlreadyClosed => "already_closed",
            Error::Io(error) => match error.kind() {
                ErrorKind::ConnectionReset => "connection_reset",
                ErrorKind::ConnectionAborted => "connection_aborted",
                ErrorKind::BrokenPipe => "broken_pipe",
                ErrorKind::TimedOut => "timed_out",
                ErrorKind::UnexpectedEof => "unexpected_eof",
                _ => "io_other",
            },
            Error::Tls(_) => "tls",
            Error::Capacity(_) => "capacity",
            Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
                "reset_without_close_handshake"
            }
            Error::Protocol(_) => "protocol",
            Error::WriteBufferFull(_) => "write_buffer_full",
            Error::Utf8(_) => "utf8",
            _ => "other",
        };
        Self {
            transport_error_kind: Some(kind),
            ..Self::new(reason)
        }
    }
}

impl std::fmt::Display for WsInterruption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for WsInterruption {}

// Fixed event names only: unknown upstream types may contain private content.
#[derive(Default)]
struct LastUpstreamEvent {
    received: Option<(&'static str, i64, Instant)>,
    response_event: Option<Instant>,
    output_event: Option<Instant>,
    terminal_event_seen: bool,
    response_completed_seen: bool,
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
        // Classify only the allowlisted name, never arbitrary upstream strings.
        // Metadata/control events and pongs are not response/output progress.
        if kind.starts_with("response.") && kind != "response.metadata" {
            self.response_event = Some(now);
            if !matches!(
                kind,
                "response.created"
                    | "response.in_progress"
                    | "response.completed"
                    | "response.incomplete"
                    | "response.failed"
            ) {
                self.output_event = Some(now);
            }
        }
        self.terminal_event_seen |= matches!(
            kind,
            "response.completed" | "response.incomplete" | "response.failed" | "error"
        );
        self.response_completed_seen |= kind == "response.completed";
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

fn observed_age_ms(received: Option<Instant>, now: Instant) -> Option<u64> {
    received.map(|received| {
        now.saturating_duration_since(received)
            .as_millis()
            .min(u64::MAX as u128) as u64
    })
}

struct WsRequestDiagnostics<'a> {
    client_connection_id: &'a str,
    frame_sequence: u64,
    connection_id: &'a str,
    connected: Instant,
    finished_requests: u64,
    liveness: &'a WsLiveness,
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
    context_expiry: i64,
    health_order: i64,
    authentication_rejected: bool,
    events_seen: u64,
    output_seen: bool,
    last_event: LastUpstreamEvent,
}

enum JsonReply {
    Response(Value),
    BackendError(StatusCode, Value),
    Quota(i64),
    Authentication,
}

fn backend_event_error(event: &Value) -> Option<(StatusCode, Value)> {
    if !matches!(event["type"].as_str(), Some("error" | "response.failed")) {
        return None;
    }
    event
        .get("error")
        .filter(|error| error.is_object())
        .or_else(|| {
            event
                .pointer("/response/error")
                .filter(|error| error.is_object())
        })?;
    let status = event["status"]
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .filter(|status| status.is_client_error() || status.is_server_error())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    Some((
        status,
        json!({"error":{"type":"upstream_rejected","code":"upstream_rejected","message":"upstream rejected request"}}),
    ))
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
            context_expiry: chrono::Utc::now().timestamp() + affinity::TTL,
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
    fn remember(&self, output: &mut Value) -> Result<()> {
        affinity::wrap_output(
            output,
            &self.context_key,
            self.user_id,
            self.account_id,
            self.context_expiry,
        )
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
    fn ws_interrupted(&self, failure: &WsInterruption, socket: Option<&WsRequestDiagnostics<'_>>) {
        let now = Instant::now();
        let (last_event_kind, last_event_at_ms, last_event_age_ms) = self.last_event.fields(now);
        tracing::info!(event="upstream_stream_interrupted",request_id=%self.id,
            upstream_transport=self.transport,response_created=self.created,
            events_seen=self.events_seen,output_seen=self.output_seen,
            duration_ms=self.start.elapsed().as_millis() as u64,
            last_event_kind,last_event_at_ms,last_event_age_ms,
            last_response_event_age_ms=observed_age_ms(self.last_event.response_event, now),
            last_output_event_age_ms=observed_age_ms(self.last_event.output_event, now),
            terminal_event_seen=self.last_event.terminal_event_seen,
            response_completed_seen=self.last_event.response_completed_seen,
            failure_side=failure.failure_side(),
            client_connection_id=socket.map(|socket| socket.client_connection_id),
            frame_sequence=socket.map(|socket| socket.frame_sequence),
            connection_id=socket.map(|socket| socket.connection_id),
            connection_age_ms=socket.map(|socket| now.saturating_duration_since(socket.connected).as_millis() as u64),
            finished_requests=socket.map(|socket| socket.finished_requests),
            pings=socket.map(|socket| socket.liveness.pings),
            pongs=socket.map(|socket| socket.liveness.pongs),
            response_started=socket.map(|socket| socket.liveness.response_started),
            last_peer_frame_age_ms=socket.and_then(|socket| observed_age_ms(socket.liveness.last_peer_frame, now)),
            last_pong_age_ms=socket.and_then(|socket| observed_age_ms(socket.liveness.last_pong, now)),
            reason=failure.reason,stage=failure.stage,
            transport_error_kind=failure.transport_error_kind,close_code=failure.close_code);
    }
    async fn observe(&mut self, event: &mut Value) -> Result<bool> {
        let received = chrono::Utc::now();
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| WsInterruption::new("upstream_event_type_missing"))?
            .to_owned();
        let kind = kind.as_str();
        self.last_event
            .observe(kind, received.timestamp_millis(), Instant::now());
        let now = received.timestamp();
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
                return Err(WsInterruption::new("upstream_duplicate_response_created").into());
            }
            self.created = true;
        } else if kind.starts_with("response.") && kind != "response.metadata" && !self.created {
            return Err(WsInterruption::new("upstream_event_before_response_created").into());
        }
        if matches!(
            kind,
            "response.created" | "response.completed" | "response.incomplete" | "response.failed"
        ) && event
            .pointer("/response/id")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(WsInterruption::new("upstream_response_id_missing").into());
        }
        if let Some(id) = event.pointer("/response/id").and_then(Value::as_str) {
            if id.len() > 256 || id.chars().any(char::is_control) {
                return Err(WsInterruption::new("upstream_response_id_invalid").into());
            }
            if self.response_id.as_ref().is_some_and(|old| old != id) {
                return Err(WsInterruption::new("upstream_response_id_changed").into());
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
            "response.completed" | "response.incomplete" => event.pointer_mut("/response/output"),
            "response.output_item.added" | "response.output_item.done" => event.get_mut("item"),
            _ => None,
        };
        if let Some(output) = output {
            self.remember(output)?;
        }
        if matches!(
            kind,
            "response.created" | "response.completed" | "response.incomplete"
        ) {
            if let Some(conversation) = event
                .pointer_mut("/response/conversation")
                .filter(|v| !v.is_null())
            {
                let id = if conversation.is_string() {
                    conversation
                } else {
                    conversation
                        .get_mut("id")
                        .ok_or("invalid conversation reference")?
                };
                let raw = id.as_str().ok_or("invalid conversation reference")?;
                *id = Value::String(affinity::issue(
                    raw,
                    affinity::Kind::Conversation,
                    &self.context_key,
                    self.user_id,
                    self.account_id,
                    self.context_expiry,
                )?);
            }
        }
        for path in ["/headers", "/response/headers"] {
            if let Some(headers) = event.pointer_mut(path).and_then(Value::as_object_mut) {
                for (name, value) in headers {
                    if name.eq_ignore_ascii_case("x-codex-turn-state") {
                        *value =
                            Value::String(self.remember_turn(
                                value.as_str().ok_or("invalid upstream turn state")?,
                            )?);
                    }
                }
            }
        }
        Ok(status.is_some())
    }

    fn remember_turn(&self, value: &str) -> Result<String> {
        affinity::issue(
            value,
            affinity::Kind::Turn,
            &self.context_key,
            self.user_id,
            self.account_id,
            self.context_expiry,
        )
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
    // This backend accepts developer instructions but rejects the public API's
    // system role. Preserve message ordering and content during translation.
    normalize_messages(payload);
    let object = payload
        .as_object_mut()
        .ok_or("request must be a JSON object")?;
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
        object.entry("store").or_insert(json!(false));
        object.insert("stream".into(), json!(true));
        return Ok(false);
    }
    let streaming = match object.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err("stream must be a boolean".into()),
    };
    object.entry("store").or_insert(json!(false));
    object.insert("stream".into(), json!(true));
    Ok(streaming)
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
    TransferFull,
}
impl ContextError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request_error",
            Self::Missing => "context_not_found",
            Self::Conflict => "context_account_mismatch",
            Self::Storage => "storage_unavailable",
            Self::TransferFull => "context_transfer_storage_full",
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
            Self::TransferFull => {
                "context transfer storage full; wait for existing transfers to expire"
            }
        }
    }
    fn response(&self) -> Response {
        let storage = matches!(self, Self::Storage | Self::TransferFull);
        (if storage { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST },Json(json!({"error":{"type":if storage {"storage_unavailable"} else {"invalid_request_error"},"code":self.code(),"message":self.message()}}))).into_response()
    }
}
async fn context_account(
    state: &AppState,
    identity: &Identity,
    payload: &Value,
) -> std::result::Result<(Option<i64>, bool), ContextError> {
    let now = chrono::Utc::now().timestamp();
    affinity::digests(&payload["input"], &state.key, identity.user_id)
        .map_err(|_| ContextError::Invalid)?;
    let mut refs = affinity::references(&payload["input"], &state.key, identity.user_id, now)
        .map_err(|_| ContextError::Missing)?;
    if !payload["conversation"].is_null() {
        let id = payload["conversation"]
            .as_str()
            .or_else(|| payload["conversation"]["id"].as_str())
            .ok_or(ContextError::Invalid)?;
        refs.push(
            affinity::reference(
                id,
                affinity::Kind::Conversation,
                &state.key,
                identity.user_id,
                now,
            )
            .map_err(|_| ContextError::Missing)?,
        );
    }
    if let Some(turn) = payload
        .pointer("/client_metadata/x-codex-turn-state")
        .filter(|v| !v.is_null())
    {
        refs.push(
            affinity::reference(
                turn.as_str().ok_or(ContextError::Invalid)?,
                affinity::Kind::Turn,
                &state.key,
                identity.user_id,
                now,
            )
            .map_err(|_| ContextError::Missing)?,
        );
    }
    if refs.is_empty() {
        return Ok((None, false));
    }
    let user = identity.user_id;
    match state
        .db
        .call(move |conn| {
            affinity::lookup_references(conn, user, &refs, chrono::Utc::now().timestamp())
        })
        .await
        .map_err(|_| ContextError::Storage)?
    {
        affinity::Lookup::None => Ok((None, false)),
        affinity::Lookup::Account(id) => Ok((Some(id), false)),
        affinity::Lookup::Portable(id) => Ok((Some(id), true)),
        affinity::Lookup::Conflict => Err(ContextError::Conflict),
    }
}

async fn select_context_account(
    upstream: &crate::upstream::Upstream,
    model: &str,
    session: Option<&str>,
    user: i64,
    owner: Option<i64>,
    payload: &Value,
) -> std::result::Result<crate::upstream::Selection, crate::upstream::SelectError> {
    if !payload["conversation"].is_null() {
        let id = owner.ok_or(crate::upstream::SelectError::UpstreamUnavailable)?;
        upstream.select_pinned_for_user(model, id, Some(user)).await
    } else {
        upstream
            .select_continuation(model, session, user, owner)
            .await
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
) -> std::result::Result<(), ContextError> {
    if !payload["conversation"].is_null() {
        return Err(ContextError::Conflict);
    }
    let refs = affinity::references(
        &payload["input"],
        &state.key,
        identity.user_id,
        chrono::Utc::now().timestamp(),
    )
    .map_err(|_| ContextError::Missing)?;
    let (user, id, generation) = (identity.user_id, account.info.id, account.info.generation);
    state
        .db
        .call(move |conn| {
            affinity::transfer_references(
                conn,
                user,
                id,
                generation,
                &refs,
                chrono::Utc::now().timestamp(),
            )
        })
        .await
        .map_err(|err| {
            if err.is::<affinity::TransferStorageFull>() {
                ContextError::TransferFull
            } else {
                ContextError::Storage
            }
        })?;
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
    let value = value.to_str().map_err(|_| ContextError::Invalid)?;
    let item = affinity::reference(
        value,
        affinity::Kind::Turn,
        &state.key,
        identity.user_id,
        chrono::Utc::now().timestamp(),
    )
    .map_err(|_| ContextError::Missing)?;
    let user = identity.user_id;
    match state
        .db
        .call(move |conn| {
            affinity::lookup_references(conn, user, &[item], chrono::Utc::now().timestamp())
        })
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
    let selected = select_context_account(
        &upstream,
        &model,
        cache.preferred.then_some(cache.session.as_str()),
        identity.user_id,
        owner,
        &payload,
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
        Err(crate::upstream::SelectError::Backoff(until)) => return backoff_error(until, &id),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream_unavailable",
                "OAuth account or model catalog unavailable",
            )
        }
    };
    if owner.is_some_and(|id| id != account.info.id) {
        if let Err(err) = transfer_context(&state, &identity, &mut payload, &account).await {
            return err.response();
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
    let dispatch_payload =
        match affinity::upstream_payload(&payload, &state.key, identity.user_id, 0) {
            Ok(value) => value,
            Err(_) => return ContextError::Missing.response(),
        };
    let socket = if chat_request {
        tokio::select! {
            _=wait_for_stop(stopped.clone())=> {
                let _=log.finish("interrupted", Counters::default()).await;
                return error(StatusCode::SERVICE_UNAVAILABLE, "service_stopping", "service stopping");
            },
            socket=upstream.websocket(&account,(&session,&thread),true,&dispatch_payload)=>match socket {
                Ok((socket, _headers))=>Some(socket),
                Err(crate::upstream::SocketError::Cooldown(until))=> {
                    let _=log.finish("local_rejected", Counters::default()).await;
                    return cooldown_error(until);
                },
                Err(crate::upstream::SocketError::Storage)=> {
                    let _=log.finish("storage_error", Counters::default()).await;
                    return error(StatusCode::SERVICE_UNAVAILABLE,"storage_unavailable","quota storage unavailable");
                },
                Err(crate::upstream::SocketError::Backoff(until))=> {let _=log.finish("local_rejected",Counters::default()).await;return backoff_error(until, &id);},
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
            crate::upstream::SelectError::Backoff(until) => backoff_error(until, &id),
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
            _=wait_for_stop(stopped.clone())=>Err(WsInterruption::new("service_stopping")),
            sent=send_upstream(&mut socket,UpstreamMessage::Text(affinity::upstream_payload(&payload, &state.key, identity.user_id, 0).expect("validated context").to_string().into()))=>sent,
        };
        if let Err(failure) = sent {
            log.ws_interrupted(&failure.at("request_write"), None);
            if !*stopped.borrow() {
                let _ = log
                    .health(Some(crate::health::Rejection::Temporary), "transport")
                    .await;
            }
            let _ = log.finish("interrupted", Counters::default()).await;
            return error(
                StatusCode::BAD_GATEWAY,
                "upstream_connection_error",
                "upstream write failed",
            );
        }
        Source::WebSocket(Box::new(socket))
    } else {
        let mut excluded = Vec::new();
        let response = loop {
            let dispatch_payload =
                match affinity::upstream_payload(&payload, &state.key, identity.user_id, 0) {
                    Ok(value) => value,
                    Err(_) => return ContextError::Missing.response(),
                };
            let dispatch_turn = turn_state
                .as_ref()
                .and_then(|v| v.to_str().ok())
                .and_then(|v| affinity::upstream_turn(v, &state.key, identity.user_id, 0).ok())
                .and_then(|v| HeaderValue::from_str(&v).ok());
            let response = tokio::select! {
                _=wait_for_stop(stopped.clone())=>Err("service stopping".into()),
                response=upstream.post(&account,path,&dispatch_payload,Some((&session,&thread)),log.health_order,dispatch_turn.as_ref())=>response,
            };
            let response = match response {
                Ok(response) => response,
                Err(_) => {
                    let _ = log.finish("interrupted", Counters::default()).await;
                    return error(
                        StatusCode::BAD_GATEWAY,
                        "upstream_connection_error",
                        "upstream interrupted",
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
                // Backend validation evolves independently of the router. Read a
                // bounded structured error without logging or persisting its body.
                let body = tokio::time::timeout(
                    Duration::from_secs(5),
                    crate::oauth::read_json(response, 65_536),
                )
                .await
                .ok()
                .and_then(std::result::Result::ok);
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
                    && payload["conversation"].is_null()
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
                            if let Err(err) =
                                transfer_context(&state, &identity, &mut payload, &alternate).await
                            {
                                return err.response();
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
                let response_status = if status.as_u16() == 401 {
                    StatusCode::BAD_GATEWAY
                } else {
                    status
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
            response_turn_state = value
                .to_str()
                .ok()
                .and_then(|v| log.remember_turn(v).ok())
                .and_then(|v| HeaderValue::from_str(&v).ok());
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
                            frame.wire = format!("event: {}\ndata: {}\n\n", event["type"].as_str().unwrap_or("error"), event).into_bytes();
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
                        if let Some((status, value)) = frame.event.as_ref().and_then(backend_event_error) {
                            if chat_request || frame.event.as_ref().is_some_and(|event| event["type"] == "error") {
                                terminal = Some(JsonReply::BackendError(status, value));
                                return Ok(());
                            }
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
                            (Some(_), Some(event)) if backend_event_error(event).is_some() => {
                                vec![format!("data: {}\n\n", backend_event_error(event).expect("checked").1).into_bytes()]
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
            Ok(Ok(JsonReply::BackendError(status, value))) => (status, Json(value)).into_response(),
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
fn backoff_error(until: i64, id: &RequestId) -> Response {
    tracing::info!(event="upstream_request_deferred",request_id=%id.0,
        reason="operational_backoff",retry_after=until.saturating_sub(chrono::Utc::now().timestamp()).max(0));
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
    Extension(id): Extension<RequestId>,
    ws: std::result::Result<
        WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Response {
    tracing::info!(event="client_websocket_upgrade_received",client_connection_id=%id.0);
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
        Err(crate::upstream::SelectError::Backoff(until)) => return backoff_error(until, &id),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream_unavailable",
                "OAuth account unavailable",
            )
        }
    }
    let jobs = state.jobs.clone();
    let upgrade_id = id.clone();
    ws.max_message_size(MAX_INFERENCE_REQUEST_BYTES)
        .max_frame_size(MAX_INFERENCE_REQUEST_BYTES)
        .on_failed_upgrade(move |_| {
            tracing::info!(event="client_websocket_upgrade_failed",client_connection_id=%upgrade_id.0);
        })
        .on_upgrade(move |client| {
            jobs.track_future(ws_loop(
                client,
                WsSession {
                    state,
                    identity,
                    upstream,
                    client_connection_id: id,
                    _permit: permit,
                },
            ))
        })
}

async fn send_client(
    client: &mut WebSocket,
    value: String,
) -> std::result::Result<(), WsInterruption> {
    send_client_message(client, Message::Text(value.into())).await
}

async fn send_client_message(
    client: &mut WebSocket,
    message: Message,
) -> std::result::Result<(), WsInterruption> {
    tokio::time::timeout(Duration::from_secs(10), client.send(message))
        .await
        .map_err(|_| WsInterruption::new("client_write_timeout"))?
        .map_err(|_| WsInterruption::new("client_write_error"))
}

async fn send_upstream(
    socket: &mut UpstreamSocket,
    message: UpstreamMessage,
) -> std::result::Result<(), WsInterruption> {
    tokio::time::timeout(Duration::from_secs(10), socket.send(message))
        .await
        .map_err(|_| WsInterruption::new("upstream_write_timeout"))?
        .map_err(|error| WsInterruption::transport("upstream_write_error", &error))
}

struct WsSession {
    state: Arc<AppState>,
    identity: Identity,
    upstream: Arc<Upstream>,
    client_connection_id: RequestId,
    _permit: limits::Permit,
}

// Fixed reasons and generated IDs only; never retain frame or close-reason text.
struct ClientWsLog {
    id: RequestId,
    opened: Instant,
    frames: u64,
    reason: &'static str,
    close_code: Option<u16>,
}

impl Drop for ClientWsLog {
    fn drop(&mut self) {
        tracing::info!(event="client_websocket_closed",client_connection_id=%self.id.0,
            duration_ms=self.opened.elapsed().as_millis() as u64,frames_received=self.frames,
            reason=self.reason,close_code=self.close_code);
    }
}

struct WsBinding {
    account: crate::upstream::Selection,
    socket: Option<UpstreamSocket>,
    metadata: Option<HeaderMap>,
    // At most one bounded idle metadata notification, delivered after created.
    pending_metadata: Option<String>,
    connection_id: RequestId,
    connected: Instant,
    idle_since: Instant,
    finished: u64,
    responses: HashSet<String>,
}

impl WsBinding {
    fn new(
        account: crate::upstream::Selection,
        socket: UpstreamSocket,
        metadata: HeaderMap,
    ) -> Self {
        Self {
            account,
            socket: Some(socket),
            metadata: Some(metadata),
            pending_metadata: None,
            connection_id: RequestId::new(),
            connected: Instant::now(),
            idle_since: Instant::now(),
            finished: 0,
            responses: HashSet::new(),
        }
    }

    async fn close(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
        }
        self.metadata = None;
        self.pending_metadata = None;
    }

    fn idle_closed(&self, failure: &WsInterruption) {
        tracing::info!(event="upstream_websocket_idle_closed",connection_id=%self.connection_id.0,
            connection_age_ms=self.connected.elapsed().as_millis() as u64,
            finished_requests=self.finished, reason=failure.reason,
            transport_error_kind=failure.transport_error_kind,close_code=failure.close_code);
    }
}

async fn ws_loop(mut client: WebSocket, session: WsSession) {
    let WsSession {
        state,
        identity,
        upstream,
        client_connection_id,
        _permit,
    } = session;
    tracing::info!(event="client_websocket_opened",client_connection_id=%client_connection_id.0);
    let mut client_log = ClientWsLog {
        id: client_connection_id,
        opened: Instant::now(),
        frames: 0,
        reason: "session_ended",
        close_code: None,
    };
    let mut responses = HashSet::new();
    let mut retired_accounts = HashSet::new();
    let mut binding: Option<WsBinding> = None;
    let mut window: Option<super::continuation::Window> = None;
    let mut idle_log: Option<RequestLog> = None;
    let mut upstream_session: Option<String> = None;
    let mut upstream_thread: Option<String> = None;
    let mut upstream_identity_session: Option<String> = None;
    let stopped = state.stopped.clone();
    let mut idle_deadline = Instant::now() + WS_CLIENT_IDLE;
    let mut client_seen = Instant::now();
    let mut keepalive = tokio::time::interval(WS_PING_INTERVAL);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    keepalive.tick().await;
    'frames: loop {
        let message = loop {
            let retirement = binding
                .as_ref()
                .filter(|bound| bound.socket.is_some() && window.is_some())
                .map(|bound| ws_retirement_deadline(bound.connected, bound.idle_since));
            tokio::select! {
                // Consume queued upstream closure before a simultaneously ready new request.
                biased;
                _ = wait_for_stop(stopped.clone()) => {
                    client_log.reason = "service_stopping";
                    break 'frames;
                },
                _ = tokio::time::sleep_until(idle_deadline.into()) => {
                    client_log.reason = "client_idle_timeout";
                    break 'frames;
                },
                _ = tokio::time::sleep_until((client_seen + WS_PRE_RESPONSE_SILENCE).into()) => {
                    client_log.reason = "client_liveness_timeout";
                    break 'frames;
                },
                _ = async {
                    match retirement {
                        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let bound = binding.as_mut().expect("retirement binding");
                    let reason = if bound.connected.elapsed() >= WS_UPSTREAM_MAX_AGE {
                        "connection_max_age"
                    } else { "connection_idle_expired" };
                    bound.idle_closed(&WsInterruption::new(reason));
                    bound.close().await;
                },
                _ = keepalive.tick() => {
                    if send_client_message(&mut client, Message::Ping(Bytes::new())).await.is_err() {
                        client_log.reason = "client_ping_write_failed";
                        break 'frames;
                    }
                },
                incoming = async {
                    match binding.as_mut().and_then(|bound| bound.socket.as_mut()) {
                        Some(socket) => socket.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let bound = binding.as_mut().expect("polled binding");
                    let failure = match incoming {
                        Some(Ok(UpstreamMessage::Ping(bytes))) => {
                            send_upstream(bound.socket.as_mut().expect("polled socket"), UpstreamMessage::Pong(bytes))
                                .await.err()
                        }
                        Some(Ok(UpstreamMessage::Pong(_))) => None,
                        Some(Ok(UpstreamMessage::Text(text))) => {
                            let event = serde_json::from_str::<Value>(&text).ok();
                            if let Some(mut event) = event.filter(|event| matches!(event["type"].as_str(),
                                Some("ping" | "codex.rate_limits" | "codex.response.metadata" | "response.metadata" | "responsesapi.websocket_timing"))) {
                                if let Some(log) = &mut idle_log {
                                    if log.observe(&mut event).await.is_err() {
                                        bound.idle_closed(&WsInterruption::new("event_observation_failed"));
                                        break 'frames;
                                    }
                                }
                                if matches!(event["type"].as_str(), Some("codex.response.metadata" | "response.metadata")) {
                                    if text.len() > 64 * 1024 {
                                        bound.idle_closed(&WsInterruption::new("idle_metadata_capacity_exceeded"));
                                        break 'frames;
                                    }
                                    bound.pending_metadata = Some(event.to_string());
                                } else if send_client(&mut client, text.to_string()).await.is_err() {
                                    break 'frames;
                                }
                                None
                            } else {
                                // Never treat a late terminal/error as a refusal of the next request.
                                bound.idle_closed(&WsInterruption::new("upstream_unexpected_idle_event"));
                                break 'frames;
                            }
                        }
                        Some(Ok(UpstreamMessage::Close(frame))) => Some(WsInterruption {
                            close_code: frame.map(|frame| u16::from(frame.code)),
                            ..WsInterruption::new("upstream_close")
                        }),
                        Some(Err(error)) => Some(WsInterruption::transport("upstream_read_error", &error)),
                        None => Some(WsInterruption::new("upstream_eof")),
                        _ => {
                            bound.idle_closed(&WsInterruption::new("upstream_invalid_frame"));
                            break 'frames;
                        }
                    };
                    if let Some(failure) = failure {
                        bound.idle_closed(&failure);
                        bound.close().await;
                    }
                }
                incoming = client.next() => match incoming {
                    Some(Ok(message)) => {
                        client_seen = Instant::now();
                        break message;
                    },
                    Some(Err(_)) => {
                        client_log.reason = "client_read_error";
                        break 'frames;
                    },
                    None => {
                        client_log.reason = "client_eof";
                        break 'frames;
                    },
                },
            }
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Close(frame) => {
                client_log.reason = "client_close";
                client_log.close_code = frame.map(|frame| frame.code);
                break;
            }
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
        client_log.frames = client_log.frames.saturating_add(1);
        idle_deadline = Instant::now() + WS_CLIENT_IDLE;
        let frame_received = Instant::now();
        tracing::info!(event="client_websocket_frame_received",client_connection_id=%client_log.id.0,
            frame_sequence=client_log.frames,frame_bytes=text.len());
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
        normalize_messages(&mut payload);
        let body = payload.as_object_mut().expect("validated object");
        body.entry("store").or_insert(json!(false));
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
        tracing::info!(event="client_websocket_generation_admitted",client_connection_id=%client_log.id.0,
            frame_sequence=client_log.frames,duration_ms=frame_received.elapsed().as_millis() as u64);
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
            'migration: {
                if let Some(bound) = &binding {
                    let id = bound.account.info.id;
                    if upstream
                        .account_enabled(identity.user_id, id)
                        .await
                        .unwrap_or(false)
                        && (upstream.quota_exhausted(id).await.unwrap_or(false)
                            || upstream
                                .threshold_reached(identity.user_id, id)
                                .await
                                .unwrap_or(false))
                    {
                        if let Ok(account) = select_context_account(
                            &upstream,
                            &model,
                            None,
                            identity.user_id,
                            Some(id),
                            &payload,
                        )
                        .await
                        {
                            if account.info.id != id {
                                let exhausted = upstream.quota_exhausted(id).await.unwrap_or(true);
                                let previous = payload
                                    .get("previous_response_id")
                                    .filter(|value| !value.is_null())
                                    .cloned();
                                let delta_len = payload["input"].as_array().map_or(0, Vec::len);
                                if payload
                                    .get("previous_response_id")
                                    .is_some_and(|v| !v.is_null())
                                    && !window
                                        .as_ref()
                                        .is_some_and(|history| history.expand(&mut payload))
                                {
                                    // A configured soft threshold must not interrupt an
                                    // otherwise usable session without recovery history.
                                    if !upstream.quota_exhausted(id).await.unwrap_or(true) {
                                        break 'migration;
                                    }
                                    let _ = send_client(&mut client, ws_error("context_recovery_unavailable", "quota exhausted; reconnect with the full current context to switch accounts")).await;
                                    continue 'frames;
                                }
                                // Keep only bounded routing metadata to restore a soft
                                // transfer whose handshake fails, never clone full history.
                                let old_turn = payload
                                    .get_mut("client_metadata")
                                    .and_then(Value::as_object_mut)
                                    .and_then(|metadata| metadata.remove("x-codex-turn-state"));
                                let native_session =
                                    upstream_identity_session.as_deref().expect("bound session");
                                let thread = upstream_thread.as_deref().expect("bound thread");
                                let replacement = if exhausted {
                                    upstream
                                        .websocket_with_quota_fallback(
                                            account,
                                            identity.user_id,
                                            &model,
                                            (native_session, thread),
                                            &mut affinity::upstream_payload(
                                                &payload,
                                                &state.key,
                                                identity.user_id,
                                                0,
                                            )
                                            .expect("validated context"),
                                        )
                                        .await
                                } else {
                                    // A proactive preference may fall back to the existing
                                    // healthy connection if its alternative refuses the handshake.
                                    upstream
                                        .websocket(
                                            &account,
                                            (native_session, thread),
                                            false,
                                            &affinity::upstream_payload(
                                                &payload,
                                                &state.key,
                                                identity.user_id,
                                                0,
                                            )
                                            .expect("validated context"),
                                        )
                                        .await
                                        .map(|(socket, metadata)| (account, socket, metadata))
                                };
                                match replacement {
                                    Ok((account, socket, metadata)) => {
                                        if let Err(err) = transfer_context(
                                            &state,
                                            &identity,
                                            &mut payload,
                                            &account,
                                        )
                                        .await
                                        {
                                            let _ = send_client(
                                                &mut client,
                                                ws_error(err.code(), err.message()),
                                            )
                                            .await;
                                            continue 'frames;
                                        }
                                        let mut old = binding
                                            .replace(WsBinding::new(account, socket, metadata))
                                            .expect("bound socket");
                                        old.close().await;
                                        frame_turn = None;
                                        migrated = true;
                                        retired_accounts.insert(id);
                                    }
                                    Err(ref failure)
                                        if !exhausted
                                            && !matches!(
                                                failure,
                                                crate::upstream::SocketError::Storage
                                            )
                                            && upstream
                                                .select_pinned_for_user(
                                                    &model,
                                                    id,
                                                    Some(identity.user_id),
                                                )
                                                .await
                                                .is_ok() =>
                                    {
                                        if let Some(previous) = previous {
                                            let input = payload["input"]
                                                .as_array_mut()
                                                .expect("expanded input");
                                            let delta = input.split_off(input.len() - delta_len);
                                            payload["input"] = json!(delta);
                                            payload["previous_response_id"] = previous;
                                        }
                                        if let Some(turn) = old_turn {
                                            payload["client_metadata"]
                                                .as_object_mut()
                                                .expect("validated metadata")
                                                .insert("x-codex-turn-state".into(), turn);
                                        }
                                        break 'migration;
                                    }
                                    Err(crate::upstream::SocketError::Cooldown(until)) => {
                                        let _ = send_client(
                                            &mut client,
                                            cooldown_frame(until).to_string(),
                                        )
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
                let selected = select_context_account(
                    &upstream,
                    &model,
                    cache.preferred.then_some(cache.session.as_str()),
                    identity.user_id,
                    context,
                    &payload,
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
                        &mut affinity::upstream_payload(&payload, &state.key, identity.user_id, 0)
                            .expect("validated context"),
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
                        let _=send_client(&mut client,ws_error("upstream_websocket_unavailable","upstream handshake failed before inference; reconnect with full context")).await;
                        break 'frames;
                    }
                };
                if initial_id != account.info.id {
                    if let Err(err) =
                        transfer_context(&state, &identity, &mut payload, &account).await
                    {
                        let _ = send_client(&mut client, ws_error(err.code(), err.message())).await;
                        continue 'frames;
                    }
                    frame_turn = None;
                    retired_accounts.insert(initial_id);
                }
                upstream_identity_session = Some(cache.identity_session.clone());
                upstream_session = Some(cache.session.clone());
                upstream_thread = Some(cache.thread.clone());
                binding = Some(WsBinding::new(account, socket, metadata));
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
                pending_metadata,
                connection_id,
                connected,
                idle_since,
                finished,
                responses: upstream_responses,
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
            // Old socket IDs require recovery even after a replacement socket is open.
            if payload
                .get("previous_response_id")
                .and_then(Value::as_str)
                .is_some_and(|id| socket.is_none() || !upstream_responses.contains(id))
                && !window
                    .as_ref()
                    .is_some_and(|history| history.expand(&mut payload))
            {
                let _ = send_client(
                    &mut client,
                    ws_error(
                        "context_recovery_unavailable",
                        "upstream connection closed; reconnect with the full current context",
                    ),
                )
                .await;
                continue 'frames;
            }
            if socket.is_none() {
                // This is a new, unsent operation after a completed response, not a retry.
                if let Some(meta) = payload
                    .get_mut("client_metadata")
                    .and_then(Value::as_object_mut)
                {
                    meta.remove("x-codex-turn-state");
                }
                match upstream
                    .websocket(
                        account,
                        (
                            upstream_identity_session.as_deref().expect("bound session"),
                            upstream_thread.as_deref().expect("bound thread"),
                        ),
                        false,
                        &affinity::upstream_payload(&payload, &state.key, identity.user_id, 0)
                            .expect("validated context"),
                    )
                    .await
                {
                    Ok((replacement, headers)) => {
                        *socket = Some(replacement);
                        *metadata = Some(headers);
                        *pending_metadata = None;
                        *connection_id = RequestId::new();
                        *connected = Instant::now();
                        *idle_since = Instant::now();
                        *finished = 0;
                        upstream_responses.clear();
                        tracing::info!(event="upstream_websocket_reconnected", connection_id=%connection_id.0);
                    }
                    Err(crate::upstream::SocketError::Cooldown(until)) => {
                        let _ = send_client(&mut client, cooldown_frame(until).to_string()).await;
                        continue 'frames;
                    }
                    Err(crate::upstream::SocketError::Backoff(until)) => {
                        let _ = send_client(&mut client, backoff_frame(until).to_string()).await;
                        continue 'frames;
                    }
                    Err(crate::upstream::SocketError::Authentication) => {
                        let _ = send_client(
                            &mut client,
                            ws_error(
                                "upstream_authentication_error",
                                "upstream credentials rejected before inference",
                            ),
                        )
                        .await;
                        break 'frames;
                    }
                    Err(_) => {
                        let _ = send_client(&mut client, ws_error("upstream_websocket_unavailable",
                            "replacement handshake failed before inference; reconnect with full context")).await;
                        continue 'frames;
                    }
                }
            }
            let socket = socket.as_mut().expect("connected socket");
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
            let mut liveness = WsLiveness::new(Instant::now());
            let result: std::result::Result<(), WsInterruption> = async {
                let mut handshake_metadata = None;
                if let Some(headers) = metadata.take() {
                    let mut safe = response_metadata(&headers);
                    if let Some(value) = headers.get("x-codex-turn-state") {
                        let turn = value.to_str().map_err(|_| {
                            WsInterruption::new("invalid_turn_state").at("handshake_metadata")
                        })?;
                        let turn = log.remember_turn(turn).map_err(|_| {
                            WsInterruption::new("turn_binding_failed").at("handshake_metadata")
                        })?;
                        safe.insert("x-codex-turn-state", HeaderValue::from_str(&turn).map_err(|_| WsInterruption::new("invalid_turn_state"))?);
                    }
                    if !safe.is_empty() {
                        let headers = safe.iter().map(|(name, value)| {
                            (name.as_str().to_owned(), json!(value.to_str().expect("validated metadata")))
                        }).collect::<serde_json::Map<_, _>>();
                        handshake_metadata = Some(json!({"type":"response.metadata","headers":headers}).to_string());
                    }
                }
                log.sent().await.map_err(|_| WsInterruption::new("request_storage_failed").at("before_submission"))?;
                tracing::info!(event="upstream_websocket_request_started",request_id=%log.id,
                    connection_id=%connection_id.0,connection_age_ms=connected.elapsed().as_millis() as u64,
                    finished_requests=*finished,client_connection_id=%client_log.id.0,
                    frame_sequence=client_log.frames,preparation_ms=frame_received.elapsed().as_millis() as u64);
                let write_started = Instant::now();
                if let Err(failure) = send_upstream(socket, UpstreamMessage::Text(affinity::upstream_payload(&payload, &state.key, identity.user_id, 0).map_err(|_| WsInterruption::new("context_proof_invalid"))?.to_string().into())).await {
                    let _ = log.health(Some(crate::health::Rejection::Temporary), "transport").await;
                    return Err(failure.at("request_write"));
                }
                tracing::info!(event="upstream_websocket_request_sent",request_id=%log.id,
                    connection_id=%connection_id.0,duration_ms=write_started.elapsed().as_millis() as u64);
                liveness.last_frame = Instant::now();
                let mut heartbeat = tokio::time::interval(WS_PING_INTERVAL);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                heartbeat.tick().await;
                loop {
                    let message = tokio::select! {
                        _ = heartbeat.tick() => {
                            send_upstream(socket, UpstreamMessage::Ping(Bytes::new())).await
                                .map_err(|failure| failure.at("upstream_ping_write"))?;
                            liveness.pings = liveness.pings.saturating_add(1);
                            send_client_message(&mut client, Message::Ping(Bytes::new())).await
                                .map_err(|failure| failure.at("client_ping_write"))?;
                            send_client(&mut client, json!({"type":"ping"}).to_string()).await
                                .map_err(|failure| failure.at("heartbeat_write"))?;
                            continue;
                        },
                        _ = wait_for_stop(stopped.clone()) => return Err(WsInterruption::new("service_stopping")),
                        message = tokio::time::timeout_at(liveness.deadline().into(), socket.next()) => match message {
                            Ok(Some(Ok(message))) => {
                                if matches!(message, UpstreamMessage::Pong(_)) && liveness.pongs == 0 {
                                    tracing::info!(event="upstream_websocket_peer_alive",request_id=%log.id,
                                        connection_id=%connection_id.0,pings=liveness.pings,
                                        duration_ms=write_started.elapsed().as_millis() as u64);
                                }
                                // Closing is not evidence that the peer was alive
                                // immediately before closure; retain its prior activity.
                                if !matches!(message, UpstreamMessage::Close(_)) {
                                    liveness.received(Instant::now(), matches!(message, UpstreamMessage::Pong(_)));
                                }
                                message
                            },
                            result => {
                                let failure = match result {
                                    Err(_) => {
                                        tracing::info!(event="upstream_websocket_silence_timeout",request_id=%log.id,
                                            response_started=liveness.response_started,pings=liveness.pings,pongs=liveness.pongs,
                                            last_frame_age_ms=liveness.last_frame.elapsed().as_millis() as u64);
                                        WsInterruption::new(if liveness.response_started { "read_timeout" } else { "pre_response_silence_timeout" })
                                    },
                                    Ok(None) => WsInterruption::new("upstream_eof"),
                                    Ok(Some(Err(error))) => WsInterruption::transport("upstream_read_error", &error),
                                    Ok(Some(Ok(_))) => unreachable!(),
                                };
                                let _ = log.health(Some(crate::health::Rejection::Temporary), "transport").await;
                                return Err(failure);
                            }
                        },
                        incoming = client.next() => {
                            client_seen = Instant::now();
                            match incoming {
                            Some(Ok(Message::Ping(bytes))) => {
                                send_client_message(&mut client, Message::Pong(bytes)).await
                                    .map_err(|failure| failure.at("client_pong_write"))?;
                                continue;
                            },
                            Some(Ok(Message::Pong(_))) => continue,
                            Some(Ok(Message::Close(frame))) => {
                                return Err(WsInterruption {
                                    close_code: frame.map(|frame| frame.code),
                                    ..WsInterruption::new("client_close")
                                });
                            },
                            None => return Err(WsInterruption::new("client_eof")),
                            Some(Err(_)) => return Err(WsInterruption::new("client_read_error")),
                            Some(Ok(Message::Text(_))) => return Err(WsInterruption::new("client_concurrent_request")),
                            Some(Ok(Message::Binary(_))) => return Err(WsInterruption::new("client_invalid_frame")),
                            }
                        },
                    };
                    let text = match message {
                        UpstreamMessage::Text(text) => text,
                        UpstreamMessage::Ping(bytes) => {
                            send_upstream(socket, UpstreamMessage::Pong(bytes)).await
                                .map_err(|failure| failure.at("upstream_pong_write"))?;
                            continue;
                        },
                        UpstreamMessage::Pong(_) => continue,
                        message => {
                            let failure = match message {
                                UpstreamMessage::Close(frame) => WsInterruption {
                                    close_code: frame.map(|frame| u16::from(frame.code)),
                                    ..WsInterruption::new("upstream_close")
                                },
                                _ => WsInterruption::new("upstream_invalid_frame"),
                            };
                            let _ = log.health(Some(crate::health::Rejection::Temporary), "transport").await;
                            return Err(failure);
                        },
                    };
                    let mut event: Value = match serde_json::from_str(&text) {
                        Ok(event) => event,
                        Err(_) => {
                            let _ = log.health(Some(crate::health::Rejection::Temporary), "protocol").await;
                            return Err(WsInterruption::new("upstream_invalid_json"));
                        }
                    };
                    let pre_generation_quota = safe_to_switch && payload["conversation"].is_null() && !log.created && quota_refusal(&event);
                    safe_to_switch &= matches!(event["type"].as_str(), Some("ping" | "codex.rate_limits" | "codex.response.metadata" | "response.metadata" | "responsesapi.websocket_timing"));
                    liveness.response_started |= !safe_to_switch;
                    let terminal = log.observe(&mut event).await
                        .map_err(|error| {
                            error.downcast::<WsInterruption>()
                                .map(|failure| failure.at("event_observation"))
                                .unwrap_or_else(|_| WsInterruption::new("event_observation_failed").at("event_observation"))
                        })?;
                    if log.events_seen == 1 {
                        tracing::info!(event="upstream_websocket_first_event",request_id=%log.id,
                            connection_id=%connection_id.0,duration_ms=write_started.elapsed().as_millis() as u64);
                    }
                    if pre_generation_quota && terminal {
                        rejected_quota = Some(text.to_string());
                        return Ok(());
                    }
                    send_client(&mut client, if log.authentication_rejected {
                        ws_error("upstream_authentication_error", "upstream credentials rejected; reconnect with full context using a separate new request")
                    } else {
                        event.to_string()
                    }).await.map_err(|failure| failure.at("event_write"))?;
                    // OpenCode's sequential driver rejects response.* notifications
                    // before response.created; Codex also accepts metadata here.
                    if event["type"] == "response.created" {
                        if let Some(metadata) = handshake_metadata.take() {
                            send_client(&mut client, metadata).await
                                .map_err(|failure| failure.at("metadata_write"))?;
                        }
                        if let Some(metadata) = pending_metadata.take() {
                            send_client(&mut client, metadata).await
                                .map_err(|failure| failure.at("metadata_write"))?;
                        }
                    }
                    if let Some(output) = &mut recovery_output {
                        if output.observe(&mut event).is_err() {
                            recovery_output = None;
                        }
                    }
                    if terminal {
                        if event["type"] == "response.completed" && recovery_output.is_some() {
                            recovered = event.get_mut("response").map(Value::take);
                        }
                        return Ok(());
                    }
                }
            }.await;
            if let Err(failure) = result {
                log.ws_interrupted(
                    &failure,
                    Some(&WsRequestDiagnostics {
                        client_connection_id: &client_log.id.0,
                        frame_sequence: client_log.frames,
                        connection_id: &connection_id.0,
                        connected: *connected,
                        finished_requests: *finished,
                        liveness: &liveness,
                    }),
                );
                client_log.reason = failure.reason;
                if failure.reason == "client_close" {
                    client_log.close_code = failure.close_code;
                }
                let _ = log.finish("interrupted", Counters::default()).await;
                let _ = send_client(&mut client, failure.client_error(&id)).await;
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
                                    &mut affinity::upstream_payload(
                                        &payload,
                                        &state.key,
                                        identity.user_id,
                                        0,
                                    )
                                    .expect("validated context"),
                                )
                                .await
                            {
                                if let Err(err) =
                                    transfer_context(&state, &identity, &mut payload, &alternate)
                                        .await
                                {
                                    let _ = send_client(
                                        &mut client,
                                        ws_error(err.code(), err.message()),
                                    )
                                    .await;
                                    break 'frames;
                                }
                                let mut old = binding
                                    .replace(WsBinding::new(alternate, socket, metadata))
                                    .expect("bound socket");
                                old.close().await;
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
            *finished = finished.saturating_add(1);
            *idle_since = Instant::now();
            if let Some(response) = log.response_id.clone() {
                if responses.len() >= 1024 {
                    break 'frames;
                }
                upstream_responses.insert(response.clone());
                responses.insert(response);
            }
            idle_log = Some(log);
            break 'attempt;
        }
        drop(permit);
        idle_deadline = Instant::now() + WS_CLIENT_IDLE;
    }
    if let Some(mut binding) = binding {
        binding.close().await;
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
    fn websocket_liveness_distinguishes_silence_from_live_slow_generation() {
        let start = Instant::now();
        let mut live = WsLiveness::new(start);
        assert_eq!(live.deadline(), start + Duration::from_secs(90));
        live.received(start + Duration::from_secs(80), true);
        assert!(!live.response_started);
        assert_eq!(live.pongs, 1);
        assert_eq!(live.deadline(), start + Duration::from_secs(170));
        live.response_started = true;
        assert_eq!(live.deadline(), start + Duration::from_secs(980));
        live.pings += 1;
        assert_eq!(live.deadline(), start + Duration::from_secs(980));
    }

    #[test]
    fn upstream_retirement_uses_idle_time_and_never_renews_total_age() {
        let start = Instant::now();
        assert_eq!(
            ws_retirement_deadline(start, start),
            start + Duration::from_secs(30)
        );
        assert_eq!(
            ws_retirement_deadline(start, start + Duration::from_secs(290)),
            start + Duration::from_secs(300)
        );
        assert!(
            ws_retirement_deadline(start, start + Duration::from_secs(400))
                < start + Duration::from_secs(400)
        );
    }

    #[test]
    fn websocket_transport_diagnostics_discard_error_payloads() {
        use tokio_tungstenite::tungstenite::{error::ProtocolError, Error};
        let private = "synthetic-private-error-content";
        for (error, expected) in [
            (
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    private,
                )),
                "connection_reset",
            ),
            (Error::Utf8(private.into()), "utf8"),
            (
                Error::WriteBufferFull(Box::new(UpstreamMessage::Text(private.into()))),
                "write_buffer_full",
            ),
            (
                Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                "reset_without_close_handshake",
            ),
        ] {
            let diagnostic = WsInterruption::transport("upstream_read_error", &error);
            assert_eq!(diagnostic.transport_error_kind, Some(expected));
            assert!(!format!("{diagnostic:?}").contains(private));
            assert_eq!(diagnostic.to_string(), "upstream_read_error");
        }
    }

    #[test]
    fn websocket_interruption_error_preserves_code_and_generated_correlation() {
        let id = RequestId::new();
        for (reason, side) in [
            ("upstream_close", "upstream"),
            ("upstream_read_error", "upstream"),
            ("read_timeout", "upstream"),
            ("pre_response_silence_timeout", "upstream"),
            ("client_close", "client"),
            ("client_write_timeout", "client"),
            ("request_storage_failed", "router"),
            ("service_stopping", "router"),
        ] {
            let failure = WsInterruption::new(reason).at("request_write");
            assert_eq!(failure.failure_side(), side);
            let event: Value = serde_json::from_str(&failure.client_error(&id)).unwrap();
            assert_eq!(event["type"], "error");
            assert_eq!(event["error"]["code"], "upstream_interrupted");
            let message = event["error"]["message"].as_str().unwrap();
            assert!(message.contains(reason));
            assert!(message.contains(&id.0));
            assert!(message.contains("stage=request_write"));
            assert!(!message.contains("close_code="));
        }
        let failure = WsInterruption {
            close_code: Some(1000),
            ..WsInterruption::new("upstream_close")
        };
        assert!(failure.client_error(&id).contains("close_code=1000"));
    }

    #[test]
    fn response_progress_is_independent_of_control_frames_and_transport_liveness() {
        let start = Instant::now();
        let mut event = LastUpstreamEvent::default();
        let mut live = WsLiveness::new(start);
        assert_eq!(observed_age_ms(event.response_event, start), None);
        assert_eq!(observed_age_ms(event.output_event, start), None);
        assert_eq!(observed_age_ms(live.last_peer_frame, start), None);
        event.observe("response.created", 1000, start);
        assert_eq!(observed_age_ms(event.output_event, start), None);
        event.observe(
            "response.output_item.done",
            2000,
            start + Duration::from_secs(1),
        );
        for kind in [
            "codex.rate_limits",
            "response.metadata",
            "ping",
            "private-event-type",
        ] {
            event.observe(kind, 9000, start + Duration::from_secs(9));
        }
        live.received(start + Duration::from_secs(10), true);
        let now = start + Duration::from_secs(13);
        assert_eq!(observed_age_ms(event.response_event, now), Some(12000));
        assert_eq!(observed_age_ms(event.output_event, now), Some(12000));
        assert_eq!(observed_age_ms(live.last_peer_frame, now), Some(3000));
        assert_eq!(observed_age_ms(live.last_pong, now), Some(3000));
        assert!(!event.terminal_event_seen);
        assert!(!event.response_completed_seen);
        event.observe("response.completed", 14000, start + Duration::from_secs(14));
        assert!(event.terminal_event_seen);
        assert!(event.response_completed_seen);
        assert_eq!(
            observed_age_ms(event.output_event, start + Duration::from_secs(14)),
            Some(13000)
        );
        for kind in ["response.failed", "response.incomplete", "error"] {
            let mut terminal = LastUpstreamEvent::default();
            terminal.observe(kind, 1000, start);
            assert!(terminal.terminal_event_seen);
            assert!(!terminal.response_completed_seen);
        }
    }

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
