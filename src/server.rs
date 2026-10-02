use crate::{
    authenticate, control, record_usage, resolve_ssh_identity, store::Database, ControlRequest,
    Result, UsageEvent,
};
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    future::Future,
    io,
    net::SocketAddr,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixListener, UnixStream},
    sync::{watch, Semaphore},
    task::JoinSet,
};
use tokio_util::task::TaskTracker;

mod continuation;
mod encoding;
mod limits;
mod output;
pub use limits::Snapshot as LimitSnapshot;
mod transport;

pub struct ServeConfig {
    pub gateway_uid: u32,
    pub listen: SocketAddr,
    pub socket: PathBuf,
    pub upstream: Option<Arc<crate::upstream::Upstream>>,
    pub generations_per_user: usize,
    pub websockets_per_user: usize,
}

// Full native-client histories and inline images can exceed the old 1 MiB limit.
// Keep inference input bounded separately from output and control messages.
pub(crate) use crate::payload::INFERENCE_REQUEST_BYTES as MAX_INFERENCE_REQUEST_BYTES;

const CONTROL_LIMIT: u64 = 32_768;
const CONTROL_REPLY_LIMIT: usize = 1_048_576;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

struct AppState {
    db: Database,
    key: Arc<[u8]>,
    gateway_uid: u32,
    http_slots: Arc<Semaphore>,
    decode_slots: Arc<Semaphore>,
    continuation_bytes: Arc<Semaphore>,
    generation_slots: limits::Slots,
    websocket_slots: limits::Slots,
    upstream: Option<Arc<crate::upstream::Upstream>>,
    jobs: TaskTracker,
    stopped: watch::Receiver<bool>,
}

#[derive(Clone)]
struct Identity {
    user_id: i64,
    token_id: String,
    bearer: String,
    cache_hint: Option<String>,
    thread_hint: Option<String>,
    turn_state: Option<HeaderValue>,
    lite_mode: Option<HeaderValue>,
}

#[derive(Clone)]
struct RequestId(String);

impl RequestId {
    fn new() -> Self {
        let mut bytes = [0; 16];
        OsRng.fill_bytes(&mut bytes);
        Self(hex::encode(bytes))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayMessage {
    pub identity: String,
    pub request: ControlRequest,
}

struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketGuard {
    fn bind(path: PathBuf) -> Result<(UnixListener, Self)> {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(
                    "control socket path already exists; inspect it before restarting".into(),
                )
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        let guard = Self {
            path,
            device: meta.dev(),
            inode: meta.ino(),
        };
        fs::set_permissions(&guard.path, fs::Permissions::from_mode(0o660))?;
        Ok((listener, guard))
    }

    fn cleanup(&self) -> io::Result<()> {
        match fs::symlink_metadata(&self.path) {
            Ok(meta)
                if meta.file_type().is_socket()
                    && meta.dev() == self.device
                    && meta.ino() == self.inode =>
            {
                fs::remove_file(&self.path)
            }
            Ok(_) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.cleanup().is_err() {
            tracing::error!(event = "socket_cleanup_failed");
        }
    }
}

/// The caller installs signal handlers before entering this function.
pub async fn serve(
    db: Database,
    key: Vec<u8>,
    config: ServeConfig,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<()> {
    if !(1..=limits::GLOBAL_LIMIT).contains(&config.generations_per_user)
        || !(1..=limits::GLOBAL_LIMIT).contains(&config.websockets_per_user)
    {
        return Err("per-user concurrency limits must be between 1 and 64".into());
    }
    let (stop, stopped) = watch::channel(false);
    let jobs = TaskTracker::new();
    let state = Arc::new(AppState {
        db,
        key: key.into(),
        gateway_uid: config.gateway_uid,
        http_slots: Arc::new(Semaphore::new(128)),
        decode_slots: Arc::new(Semaphore::new(4)),
        continuation_bytes: Arc::new(Semaphore::new(continuation::TOTAL_BYTES)),
        generation_slots: limits::Slots::new(config.generations_per_user),
        websocket_slots: limits::Slots::new(config.websockets_per_user),
        upstream: config.upstream,
        jobs: jobs.clone(),
        stopped: stopped.clone(),
    });
    let shutdown_db = state.db.clone();
    // A failed TCP bind must not leave a newly-created Unix socket behind.
    let listener = TcpListener::bind(config.listen).await?;
    let actual_address = listener.local_addr()?;
    let (control_listener, socket_guard) = SocketGuard::bind(config.socket)?;
    let recovered = state
        .db
        .call(|conn| crate::usage::recover_requests(conn))
        .await?;
    if recovered > 0 {
        tracing::warn!(event = "requests_recovered", count = recovered);
    }
    let mut tasks = JoinSet::new();
    let http_stop = stopped.clone();
    let app = router(state.clone());
    tasks.spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(wait_for_stop(http_stop))
            .await
            .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> { err.into() })
    });
    tasks.spawn(control_loop(control_listener, state, stopped));
    tracing::info!(event = "server_started", listen = %actual_address);
    tokio::pin!(shutdown);
    let first_result = tokio::select! {
        _ = &mut shutdown => Ok(()),
        result = tasks.join_next() => task_result(result.expect("server tasks exist")),
    };
    let _ = stop.send(true);
    let drained = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        while let Some(result) = tasks.join_next().await {
            task_result(result)?;
        }
        jobs.close();
        jobs.wait().await;
        shutdown_db.drain().await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    if drained.is_err() {
        tracing::warn!(event = "shutdown_deadline_exceeded");
    }
    // Also abort siblings if one failed while draining.
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    socket_guard.cleanup()?;
    tracing::info!(event = "server_stopped");
    first_result?;
    drained.map_err(|_| "server shutdown timed out")??;
    Ok(())
}

pub(crate) struct LocalStatus(Arc<AppState>);
impl LocalStatus {
    pub(crate) fn limits(&self, user: i64) -> crate::doctor::LimitsReport {
        crate::doctor::LimitsReport {
            generations: self.0.generation_slots.snapshot(user),
            websockets: self.0.websocket_slots.snapshot(user),
        }
    }
}
pub(crate) struct LocalApi {
    pub address: SocketAddr,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
    pub task: tokio::task::JoinHandle<Result<()>>,
    pub status: LocalStatus,
}

/// The standalone owner uses the API router in-process, without a control socket.
pub(crate) async fn local_api(
    db: Database,
    key: Vec<u8>,
    upstream: Arc<crate::upstream::Upstream>,
    address: SocketAddr,
) -> Result<LocalApi> {
    let listener = TcpListener::bind(address).await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = watch::channel(false);
    let jobs = TaskTracker::new();
    let state = Arc::new(AppState {
        db: db.clone(),
        key: key.into(),
        gateway_uid: unsafe { libc::geteuid() },
        http_slots: Arc::new(Semaphore::new(128)),
        decode_slots: Arc::new(Semaphore::new(4)),
        continuation_bytes: Arc::new(Semaphore::new(continuation::TOTAL_BYTES)),
        generation_slots: limits::Slots::new(8),
        websocket_slots: limits::Slots::new(8),
        upstream: Some(upstream),
        jobs: jobs.clone(),
        stopped: stopped.clone(),
    });
    db.call(|conn| crate::usage::recover_requests(conn)).await?;
    let status = LocalStatus(state.clone());
    let (shutdown, signal) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let app = router(state);
        let serving = std::future::IntoFuture::into_future(
            axum::serve(listener, app).with_graceful_shutdown(wait_for_stop(stopped)),
        );
        tokio::pin!(serving);
        let result = tokio::select! {
            result = &mut serving => result,
            _ = signal => {
                let _=stop.send(true);
                tokio::time::timeout(SHUTDOWN_TIMEOUT,&mut serving).await.map_err(|_|"Standalone shutdown timed out")?
            }
        };
        result?;
        jobs.close();
        tokio::time::timeout(SHUTDOWN_TIMEOUT, jobs.wait())
            .await
            .map_err(|_| "Standalone requests did not drain")?;
        db.drain().await?;
        Ok(())
    });
    Ok(LocalApi {
        address,
        shutdown,
        task,
        status,
    })
}
pub(crate) fn local_limits(user: i64) -> crate::doctor::LimitsReport {
    crate::doctor::LimitsReport {
        generations: limits::Slots::new(8).snapshot(user),
        websockets: limits::Slots::new(8).snapshot(user),
    }
}

fn task_result(result: std::result::Result<Result<()>, tokio::task::JoinError>) -> Result<()> {
    result??;
    Ok(())
}

async fn wait_for_stop(mut stopped: watch::Receiver<bool>) {
    while !*stopped.borrow() {
        if stopped.changed().await.is_err() {
            break;
        }
    }
}

async fn control_loop(
    listener: UnixListener,
    state: Arc<AppState>,
    mut stopped: watch::Receiver<bool>,
) -> Result<()> {
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            Some(result) = clients.join_next(), if !clients.is_empty() => {
                if result.is_err() { tracing::error!(event = "control_task_failed"); }
            }
            accepted = listener.accept(), if clients.len() < 32 => {
                let (stream, _) = accepted?;
                let state = state.clone();
                clients.spawn(async move {
                    let request_id = RequestId::new();
                    if handle_control(stream, state, request_id.clone()).await.is_err() {
                        tracing::warn!(event = "control_transport_failed", request_id = %request_id.0);
                    }
                });
            }
        }
    }
    drop(listener);
    while let Some(result) = clients.join_next().await {
        if result.is_err() {
            tracing::error!(event = "control_task_failed");
        }
    }
    Ok(())
}

async fn handle_control(
    mut stream: UnixStream,
    state: Arc<AppState>,
    request_id: RequestId,
) -> Result<()> {
    if stream.peer_cred()?.uid() != state.gateway_uid {
        return Err("unauthorized control peer".into());
    }
    let result = process_control(&mut stream, state).await;
    let reply = match result {
        Ok(value) => json!({"ok":true,"result":value,"request_id":request_id.0}),
        Err(err) => {
            tracing::warn!(event = "control_request_failed", code = err.code, request_id = %request_id.0);
            json!({"ok":false,"error":err.message,"code":err.code,"request_id":request_id.0})
        }
    };
    let mut bytes = serde_json::to_vec(&reply)?;
    if bytes.len() > CONTROL_REPLY_LIMIT {
        tracing::warn!(event = "control_reply_too_large", request_id = %request_id.0);
        bytes = serde_json::to_vec(
            &json!({"ok":false,"error":"control reply too large","code":"reply_too_large","request_id":request_id.0}),
        )?;
    }
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(&bytes)).await??;
    Ok(())
}

struct ControlError {
    code: &'static str,
    message: String,
}

async fn process_control(
    stream: &mut UnixStream,
    state: Arc<AppState>,
) -> std::result::Result<Value, ControlError> {
    let invalid = |code, message: &str| ControlError {
        code,
        message: message.to_owned(),
    };
    let mut input = Vec::new();
    tokio::time::timeout(
        IO_TIMEOUT,
        stream.take(CONTROL_LIMIT + 1).read_to_end(&mut input),
    )
    .await
    .map_err(|_| invalid("request_timeout", "control read timed out"))?
    .map_err(|_| invalid("invalid_request", "control read failed"))?;
    if input.len() as u64 > CONTROL_LIMIT {
        return Err(invalid("request_too_large", "control request too large"));
    }
    // Parser messages can contain request data; never return or log them.
    let frame: GatewayMessage = serde_json::from_slice(&input)
        .map_err(|_| invalid("invalid_request", "invalid control request"))?;
    let identity = frame.identity.clone();
    let key = state.key.clone();
    let models = matches!(frame.request, ControlRequest::Models);
    let doctor = matches!(
        frame.request,
        ControlRequest::Doctor | ControlRequest::Limits
    );
    let live_limits = matches!(frame.request, ControlRequest::Limits);
    let reset = matches!(
        frame.request,
        ControlRequest::ResetPrepare { .. } | ControlRequest::ResetConfirm { .. }
    );
    let (user_id, result, request) = state
        .db
        .call(move |conn| {
            let user_id = resolve_ssh_identity(conn, &frame.identity)?
                .ok_or("SSH identity revoked or unknown")?;
            Ok((
                user_id,
                if doctor || reset {
                    Value::Null
                } else {
                    control(conn, &key, user_id, frame.request.clone())?
                },
                frame.request,
            ))
        })
        .await
        .map_err(|err| ControlError {
            code: "control_failed",
            message: err.to_string(),
        })?;
    if reset {
        let upstream = state
            .upstream
            .as_ref()
            .ok_or_else(|| invalid("upstream_unavailable", "OAuth upstream unavailable"))?;
        return match request {
            ControlRequest::ResetPrepare { account } => {
                upstream.prepare_reset(user_id, account).await
            }
            ControlRequest::ResetConfirm { confirmation } => {
                upstream
                    .confirm_reset(user_id, &confirmation, &identity)
                    .await
            }
            _ => unreachable!(),
        }
        .map_err(|err| invalid("reset_failed", &err.to_string()));
    }
    if doctor {
        let configured = state.upstream.is_some();
        let limits = crate::doctor::LimitsReport {
            generations: state.generation_slots.snapshot(user_id),
            websockets: state.websocket_slots.snapshot(user_id),
        };
        let mut report = state
            .db
            .call(move |conn| {
                crate::doctor::snapshot(conn, configured, limits, chrono::Utc::now().timestamp())
            })
            .await
            .map_err(|_| invalid("storage_unavailable", "server diagnostics unavailable"))?;
        if let Some(upstream) = &state.upstream {
            upstream.account_metadata(&mut report, live_limits).await;
        }
        let mut value = serde_json::to_value(report)
            .map_err(|_| invalid("storage_unavailable", "server diagnostics unavailable"))?;
        value = state
            .db
            .call(move |conn| {
                crate::account_preferences::annotate(conn, user_id, &mut value)?;
                Ok(value)
            })
            .await
            .map_err(|_| invalid("storage_unavailable", "account preferences unavailable"))?;
        return Ok(value);
    }
    if models {
        if let Some(upstream) = &state.upstream {
            let models = upstream
                .models()
                .await
                .map_err(|_| invalid("upstream_unavailable", "model discovery unavailable"))?;
            return Ok(json!({"object":"list","data":models}));
        }
    }
    Ok(result)
}

fn router(state: Arc<AppState>) -> Router {
    let models = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/models/codex", get(codex_models))
        .route("/v1/models/opencode", get(opencode_models))
        .route("/v1/responses", get(transport::websocket).post(responses))
        .route("/v1/responses/compact", post(compact))
        .route("/v1/chat/completions", post(chat_completions))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize));
    Router::new()
        .merge(models)
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .layer(DefaultBodyLimit::max(MAX_INFERENCE_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            observe_request,
        ))
        .with_state(state)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"type":code,"message":message}})),
    )
        .into_response()
}

async fn observe_request(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let id = RequestId::new();
    request.extensions_mut().insert(id.clone());
    let start = Instant::now();
    let mut response = match state.http_slots.clone().try_acquire_owned() {
        Ok(_permit) => next.run(request).await,
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_busy",
            "too many active requests",
        ),
    };
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&id.0).expect("hex ID"),
    );
    tracing::info!(event = "http_request", request_id = %id.0, status = response.status().as_u16(), duration_ms = start.elapsed().as_millis() as u64);
    response
}

async fn authorize(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let Some(bearer) = bearer else {
        return error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing bearer token",
        );
    };
    let key = state.key.clone();
    let authenticating = bearer.clone();
    match state
        .db
        .call(move |conn| authenticate(conn, &key, &authenticating))
        .await
    {
        Ok(Some((user_id, token_id))) => {
            let cache_hint = [
                "session_id",
                "session-id",
                "x-session-id",
                "x-session-affinity",
            ]
            .iter()
            .find_map(|name| {
                request
                    .headers()
                    .get(*name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            });
            let thread_hint = request
                .headers()
                .get("thread-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let turn_state = request.headers().get("x-codex-turn-state").cloned();
            let lite_mode = request
                .headers()
                .get("x-openai-internal-codex-responses-lite")
                .cloned();
            request.extensions_mut().insert(Identity {
                user_id,
                token_id,
                bearer,
                cache_hint,
                thread_hint,
                turn_state,
                lite_mode,
            });
            let request = match encoding::prepare(&state, request).await {
                Ok(request) => request,
                Err(rejection) => return rejection.into_response(),
            };
            next.run(request).await
        }
        Ok(None) => error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid bearer token",
        ),
        Err(_) => {
            let id = request
                .extensions()
                .get::<RequestId>()
                .expect("request middleware");
            tracing::error!(event = "authentication_storage_failed", request_id = %id.0);
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
                "authentication storage unavailable",
            )
        }
    }
}

async fn models(State(state): State<Arc<AppState>>) -> Response {
    match &state.upstream {
        Some(upstream) => match upstream.models().await {
            Ok(models) => Json(json!({"object":"list","data":models})).into_response(),
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream_unavailable",
                "model discovery unavailable",
            ),
        },
        None => Json(json!({"object":"list","data":[]})).into_response(),
    }
}

async fn client_models(state: Arc<AppState>, codex: bool) -> Response {
    let Some(upstream) = &state.upstream else {
        return unavailable_get().await;
    };
    let result = match upstream.models().await {
        Ok(models) => {
            if codex {
                crate::catalog::codex(&models)
            } else {
                crate::catalog::opencode(&models)
            }
        }
        Err(err) => Err(err),
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "model_metadata_unavailable",
            "authoritative model metadata unavailable",
        ),
    }
}
async fn codex_models(State(state): State<Arc<AppState>>) -> Response {
    client_models(state, true).await
}
async fn opencode_models(State(state): State<Arc<AppState>>) -> Response {
    client_models(state, false).await
}

async fn unavailable_get() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "upstream_unavailable",
        "ChatGPT OAuth upstream is not configured",
    )
}

async fn responses(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    model_request(state, identity, id, headers, payload, "responses").await
}

async fn compact(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    model_request(state, identity, id, headers, payload, "responses_compact").await
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
) -> Response {
    model_request(state, identity, id, headers, payload, "chat_completions").await
}

async fn model_request(
    state: Arc<AppState>,
    identity: Identity,
    id: RequestId,
    headers: HeaderMap,
    payload: std::result::Result<Json<Value>, JsonRejection>,
    surface: &'static str,
) -> Response {
    let payload = match payload {
        Ok(Json(value)) => value,
        Err(err) => {
            let rejection_kind = match &err {
                JsonRejection::JsonSyntaxError(_) => "json_syntax",
                JsonRejection::JsonDataError(_) => "json_data",
                JsonRejection::MissingJsonContentType(_) => "content_type",
                JsonRejection::BytesRejection(_) => "body_read",
                _ => "unknown",
            };
            let encoding = match headers
                .get("content-encoding")
                .and_then(|v| v.to_str().ok())
            {
                None | Some("identity") => "identity",
                Some("zstd") => "zstd",
                Some("gzip") => "gzip",
                Some(_) => "other",
            };
            let declared_bytes = headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            tracing::info!(event = "json_request_rejected", request_id = %id.0,
                status = err.status().as_u16(), rejection_kind, encoding,
                declared_bytes, surface);
            if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
                return error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request_too_large",
                    "request body exceeds the 16 MiB limit",
                );
            }
            return error(
                err.status(),
                "invalid_request_error",
                "invalid JSON request body",
            );
        }
    };
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required",
        );
    };
    if model.len() > 128 || model.is_empty() || model.chars().any(char::is_control) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid model",
        );
    }
    let model = model.to_owned();
    if let Some(upstream) = &state.upstream {
        match upstream.has_active_account().await {
            Ok(true) => {
                return transport::http(state, identity, id, payload, model, surface, headers).await
            }
            Ok(false) => {}
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "account storage unavailable",
                )
            }
        }
    }
    let result = state
        .db
        .call(move |conn| {
            record_usage(
                conn,
                UsageEvent {
                    user_id: identity.user_id,
                    token_id: &identity.token_id,
                    surface,
                    model: "unavailable",
                    status: "upstream_unavailable",
                    input: None,
                    output: None,
                    cached_input: None,
                    reasoning_output: None,
                },
            )
        })
        .await;
    if result.is_err() {
        tracing::error!(event = "usage_record_failed", request_id = %id.0);
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "usage storage unavailable",
        );
    }
    unavailable_get().await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn control_socket_rejects_wrong_unix_uid() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("state.sqlite"))
            .await
            .unwrap();
        let state = Arc::new(AppState {
            db,
            key: vec![0; 32].into(),
            gateway_uid: unsafe { libc::geteuid() } + 1,
            http_slots: Arc::new(Semaphore::new(1)),
            decode_slots: Arc::new(Semaphore::new(4)),
            continuation_bytes: Arc::new(Semaphore::new(continuation::TOTAL_BYTES)),
            generation_slots: limits::Slots::new(1),
            websocket_slots: limits::Slots::new(1),
            upstream: None,
            jobs: TaskTracker::new(),
            stopped: watch::channel(false).1,
        });
        let (server, _client) = UnixStream::pair().unwrap();
        assert!(handle_control(server, state, RequestId::new())
            .await
            .is_err());
    }
    #[tokio::test]
    async fn cleanup_preserves_replacement_file_and_existing_listener() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let (listener, guard) = SocketGuard::bind(path.clone()).unwrap();
        assert!(SocketGuard::bind(path.clone()).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement").unwrap();
        drop(listener);
        drop(guard);
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert!(SocketGuard::bind(path).is_err());
    }
}
