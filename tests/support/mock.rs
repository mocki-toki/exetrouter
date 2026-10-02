use axum::{
    body::{Body, Bytes},
    extract::{ws::Message, Form, Query, State, WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Notify;

#[derive(Default)]
pub struct Mock {
    pub requests: Mutex<Vec<Value>>,
    pub turn_states: Mutex<Vec<Option<String>>>,
    pub compactions: Mutex<Vec<(String, Value)>>,
    pub request_accounts: Mutex<Vec<String>>,
    pub catalog_requests: Mutex<Vec<String>>,
    pub catalog_rows: Mutex<HashMap<String, Vec<Value>>>,
    pub sessions: Mutex<Vec<(String, String)>>,
    pub threads: Mutex<Vec<String>>,
    pub cache_keys: Mutex<Vec<String>>,
    pub handshakes: Mutex<Vec<String>>,
    pub catalog_started: Notify,
    pub accounts: Mutex<HashMap<String, Account>>,
    pub refreshes: AtomicUsize,
    pub lite_handshakes: AtomicUsize,
    pub invalid_grant: AtomicBool,
    pub refresh_wait: AtomicBool,
    pub refresh_started: Notify,
    pub tools: AtomicBool,
    pub device_account_in_id_token: AtomicBool,
    pub reject_websocket: AtomicBool,
    pub handshake_rate_limit: AtomicBool,
    pub handshake_unauthorized: AtomicBool,
    pub quota_headers: Mutex<HeaderMap>,
    pub quota_event: Mutex<Option<Value>>,
    pub mode: Mutex<String>,
    pub release: Notify,
    next_response: AtomicUsize,
}

#[derive(Clone)]
pub struct Account {
    pub access_token: String,
    pub refresh_token: String,
    pub models: Vec<String>,
    pub mode: String,
    pub invalid_grant: bool,
    pub catalog_error: bool,
    pub catalog_status: u16,
    pub catalog_wait: bool,
    pub refresh_unavailable: bool,
}

impl Account {
    pub fn new(access: &str, refresh: &str, models: &[&str]) -> Self {
        Self {
            access_token: access.into(),
            refresh_token: refresh.into(),
            models: models.iter().map(|model| (*model).into()).collect(),
            mode: String::new(),
            invalid_grant: false,
            catalog_error: false,
            catalog_status: 0,
            catalog_wait: false,
            refresh_unavailable: false,
        }
    }
}

pub fn router(state: Arc<Mock>) -> Router {
    Router::new()
        .route("/models", get(models))
        .route("/responses", get(websocket).post(responses))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .route("/oauth/token", post(token))
        .route("/api/accounts/deviceauth/usercode", post(device))
        .route("/api/accounts/deviceauth/token", post(device_poll))
        .with_state(state)
}

fn headers(state: &Mock, headers: &HeaderMap) -> String {
    let id = headers["chatgpt-account-id"].to_str().unwrap();
    if let Some(account) = state.accounts.lock().unwrap().get(id) {
        assert_eq!(
            headers["authorization"],
            format!("Bearer {}", account.access_token)
        );
    } else {
        assert_eq!(id, "upstream-account");
        assert_eq!(headers["authorization"], "Bearer upstream-access");
    }
    id.into()
}

async fn models(
    State(state): State<Arc<Mock>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let id = self::headers(&state, &headers);
    state.catalog_requests.lock().unwrap().push(id.clone());
    assert_eq!(
        query["client_version"],
        exetrouter::upstream::CODEX_CLIENT_VERSION
    );
    let profile = state.accounts.lock().unwrap().get(&id).cloned();
    if let Some(account) = profile {
        if account.catalog_wait {
            state.catalog_started.notify_one();
            state.release.notified().await;
        }
        if account.catalog_status != 0 {
            return StatusCode::from_u16(account.catalog_status)
                .unwrap()
                .into_response();
        }
        if account.catalog_error {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let rows = state
            .catalog_rows
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| {
                account
                    .models
                    .iter()
                    .map(|model| model_row(model, model, "list"))
                    .collect()
            });
        return Json(json!({"models":rows})).into_response();
    }
    Json(
        json!({"models":[model_row("gpt-test","Test model","list"),model_row("hidden","Hidden","hide")]}),
    ).into_response()
}

pub fn model_row(id: &str, name: &str, visibility: &str) -> Value {
    json!({"slug":id,"display_name":name,"description":"Fixture model","visibility":visibility,
    "supported_reasoning_levels":[{"effort":"low","description":"Low"},{"effort":"high","description":"High"}],
    "default_reasoning_level":"low","shell_type":"unified_exec","supported_in_api":true,"priority":1,
    "support_verbosity":false,"default_verbosity":null,"apply_patch_tool_type":null,
    "truncation_policy":{"mode":"tokens","limit":10000},"experimental_supported_tools":[],
    "context_window":64000,"max_context_window":128000,"auto_compact_token_limit":48000,
    "effective_context_window_percent":95,"input_modalities":["text"],
    "model_messages":{"instructions_template":"You are a coding assistant. Use the provided tools when asked to run a command.","instructions_variables":null},
    "use_responses_lite":false})
}

async fn token(
    State(state): State<Arc<Mock>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    assert_eq!(form["client_id"], exetrouter::oauth::CLIENT_ID);
    if form["grant_type"] == "refresh_token" {
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        if state.refresh_wait.load(Ordering::SeqCst) {
            state.refresh_started.notify_one();
            state.release.notified().await;
        }
        let pool_refresh = state
            .accounts
            .lock()
            .unwrap()
            .values()
            .find(|account| account.refresh_token == form["refresh_token"])
            .map(|account| {
                (
                    account.access_token.clone(),
                    account.refresh_token.clone(),
                    account.invalid_grant,
                    account.refresh_unavailable,
                )
            });
        if state.invalid_grant.load(Ordering::SeqCst)
            || pool_refresh
                .as_ref()
                .is_some_and(|(_, _, invalid, _)| *invalid)
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant","secret":"must-not-escape"})),
            )
                .into_response();
        }
        if pool_refresh
            .as_ref()
            .is_some_and(|(_, _, _, unavailable)| *unavailable)
        {
            return (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "17")]).into_response();
        }
        if let Some((access, refresh, _, _)) = pool_refresh {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            return Json(json!({"access_token":access,"refresh_token":refresh,"expires_in":3600}))
                .into_response();
        }
        assert_eq!(form["refresh_token"], "upstream-refresh");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        return Json(json!({"access_token":"upstream-access","refresh_token":"rotated-refresh","expires_in":3600})).into_response();
    }
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "mock-authorization");
    assert_eq!(form["code_verifier"], "mock-verifier");
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let claims = json!({"https://api.openai.com/auth":{"chatgpt_account_id":"upstream-account"},"exp":chrono::Utc::now().timestamp()+3600});
    let access = format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    if state.device_account_in_id_token.load(Ordering::SeqCst) {
        let without_identity = format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(json!({"exp":chrono::Utc::now().timestamp()+3600}).to_string())
        );
        return Json(json!({"access_token":without_identity,"id_token":access,"refresh_token":"upstream-refresh","expires_in":3600})).into_response();
    }
    Json(json!({"access_token":access,"refresh_token":"upstream-refresh","expires_in":3600}))
        .into_response()
}
async fn device(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["client_id"], exetrouter::oauth::CLIENT_ID);
    Json(json!({"device_auth_id":"device-fixture","user_code":"ABCD-EFGH","interval":"1"}))
}
async fn device_poll(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["device_auth_id"], "device-fixture");
    assert_eq!(body["user_code"], "ABCD-EFGH");
    Json(json!({"authorization_code":"mock-authorization","code_verifier":"mock-verifier"}))
}

async fn responses(
    State(state): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["accept"], "text/event-stream");
    if body["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["type"] == "additional_tools")
    {
        assert_eq!(headers["x-openai-internal-codex-responses-lite"], "true");
    }
    let account = self::headers(&state, &headers);
    state.sessions.lock().unwrap().push((
        "http_sse".into(),
        headers["session_id"].to_str().unwrap().into(),
    ));
    assert_eq!(headers["session-id"], headers["session_id"]);
    let thread = headers["thread-id"].to_str().unwrap();
    assert_eq!(thread.len(), 64);
    assert!(thread.bytes().all(|b| b.is_ascii_hexdigit()));
    state.threads.lock().unwrap().push(thread.to_owned());
    state
        .cache_keys
        .lock()
        .unwrap()
        .push(body["prompt_cache_key"].as_str().unwrap().to_owned());
    assert!(body["instructions"].is_string());
    assert!(!body["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["role"] == "system"));
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    if is_compaction(&body) {
        assert_eq!(
            body["input"].as_array().unwrap().last().unwrap()["type"],
            "compaction_trigger"
        );
        state
            .compactions
            .lock()
            .unwrap()
            .push((account.clone(), body.clone()));
    } else {
        state.requests.lock().unwrap().push(body.clone());
        state.request_accounts.lock().unwrap().push(account.clone());
    }
    let id = format!(
        "resp_{}",
        state.next_response.fetch_add(1, Ordering::SeqCst) + 1
    );
    state.turn_states.lock().unwrap().push(
        headers
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    );
    let mut mode = mode(&state, &body, &account);
    // Native OpenCode can generate an auxiliary title before its primary WS
    // turn. Inject failures only into the tool-bearing generation under test.
    if matches!(
        mode.as_str(),
        "early_close" | "disconnect" | "missing_terminal"
    ) && !body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        mode = String::new();
    }
    if mode == "invalid_no_content_type" {
        return axum::http::Response::new(Body::from("synthetic non-SSE body"));
    }
    if mode == "unauthorized_wait" {
        state.release.notified().await;
    }
    if mode == "unauthorized" || mode == "unauthorized_wait" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"private-upstream-secret"}})),
        )
            .into_response();
    }
    if mode == "upstream_503" {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", "17")],
            Json(json!({"error":{"message":"private-upstream-secret"}})),
        )
            .into_response();
    }
    if mode == "rate_limit" {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "17")],
            Json(json!({"error":{"message":"private upstream error"}})),
        )
            .into_response();
    }
    if mode == "quota_http" {
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-primary-used-percent", "100".parse().unwrap());
        headers.insert("x-codex-primary-window-minutes", "300".parse().unwrap());
        headers.insert(
            "x-codex-primary-reset-at",
            (chrono::Utc::now().timestamp() + 300)
                .to_string()
                .parse()
                .unwrap(),
        );
        return (StatusCode::TOO_MANY_REQUESTS, headers, Json(json!({"error":{"type":"usage_limit_reached","resets_at":chrono::Utc::now().timestamp()+300}}))).into_response();
    }
    if mode == "rate_limit_reset"
        || (mode == "quota_http_after_tool"
            && body["input"].as_array().is_some_and(|input| {
                input
                    .iter()
                    .any(|item| item["type"] == "function_call_output")
            }))
    {
        return (StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"type":"usage_limit_reached","resets_at":chrono::Utc::now().timestamp()+300,"message":"private upstream error"}}))).into_response();
    }
    let events = configured_events(&state, &body, &id, &mode, &account);
    let quota_headers = state.quota_headers.lock().unwrap().clone();
    let missing_content_type = mode == "no_content_type";
    let return_turn_state = mode == "turn_state";
    let stream = futures_util::stream::unfold(
        (events.into_iter(), state, 0usize, mode),
        |(mut events, state, index, mode)| async move {
            let event = events.next()?;
            if (mode == "wait" && index == 1)
                || (mode == "opaque_wait" && event["type"] == "response.completed")
            {
                state.release.notified().await;
            }
            let chunk = Bytes::from(format!(
                "event: {}\ndata: {}\n\n",
                event["type"].as_str().unwrap(),
                event
            ));
            Some((
                Ok::<_, std::convert::Infallible>(chunk),
                (events, state, index + 1, mode),
            ))
        },
    );
    let mut response = (
        [
            ("content-type", "text/event-stream"),
            ("x-request-id", "mock-upstream-request"),
        ],
        Body::from_stream(stream),
    )
        .into_response();
    response.headers_mut().extend(quota_headers);
    if return_turn_state {
        response.headers_mut().insert(
            "x-codex-turn-state",
            HeaderValue::from_static("private-synthetic-turn-state"),
        );
    }
    if missing_content_type {
        response.headers_mut().remove("content-type");
    }
    response
}

async fn websocket(
    State(state): State<Arc<Mock>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let account = self::headers(&state, &headers);
    let session = headers["session_id"].to_str().unwrap().to_owned();
    state
        .sessions
        .lock()
        .unwrap()
        .push(("websocket".into(), session.clone()));
    assert_eq!(headers["session-id"], headers["session_id"]);
    let thread = headers["thread-id"].to_str().unwrap();
    assert_eq!(thread.len(), 64);
    assert!(thread.bytes().all(|b| b.is_ascii_hexdigit()));
    state.threads.lock().unwrap().push(thread.to_owned());
    if headers
        .get("x-openai-internal-codex-responses-lite")
        .is_some_and(|value| value == "true")
    {
        state.lite_handshakes.fetch_add(1, Ordering::SeqCst);
    }
    state.handshakes.lock().unwrap().push(account.clone());
    assert_eq!(headers["openai-beta"], "responses_websockets=2026-02-06");
    if state.handshake_unauthorized.load(Ordering::SeqCst) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"private-upstream-secret"}})),
        )
            .into_response();
    }
    if state.handshake_rate_limit.load(Ordering::SeqCst) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "17")],
            Json(
                json!({"error":{"type":"usage_limit_reached","message":"private handshake error"}}),
            ),
        )
            .into_response();
    }
    if state.reject_websocket.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut state_headers = state.quota_headers.lock().unwrap().clone();
    if *state.mode.lock().unwrap() == "turn_state" {
        state_headers.insert(
            "x-codex-turn-state",
            HeaderValue::from_static("private-synthetic-ws-turn-state"),
        );
    }
    let mut upgrade = ws.on_upgrade(move |mut socket| async move {
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Text(text) => {
                    let body: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(body["type"], "response.create");
                    assert!(body.get("stream").is_none());
                    assert!(body["instructions"].is_string());
                    assert_eq!(body["store"], false);
                    let cache = body["prompt_cache_key"].as_str().unwrap();
                    assert_eq!(cache.len(), 64);
                    state.cache_keys.lock().unwrap().push(cache.to_owned());
                    state.requests.lock().unwrap().push(body.clone());
                    state.request_accounts.lock().unwrap().push(account.clone());
                    let id = format!(
                        "resp_{}",
                        state.next_response.fetch_add(1, Ordering::SeqCst) + 1
                    );
                    let mode = mode(&state, &body, &account);
                    let events = configured_events(&state, &body, &id, &mode, &account);
                    for (index, event) in events.into_iter().enumerate() {
                        if (mode == "wait" && index == 1)
                            || (mode == "opaque_wait" && event["type"] == "response.completed")
                        {
                            state.release.notified().await;
                        }
                        if socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    if matches!(
                        mode.as_str(),
                        "disconnect" | "early_close" | "missing_terminal"
                    ) && body["generate"] != false
                    {
                        return;
                    }
                }
                Message::Ping(bytes) => {
                    if socket.send(Message::Pong(bytes)).await.is_err() {
                        return;
                    }
                }
                _ => return,
            }
        }
    });
    upgrade.headers_mut().extend(state_headers);
    upgrade
}

fn mode(state: &Mock, body: &Value, account: &str) -> String {
    body.get("test_mode")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            state
                .accounts
                .lock()
                .unwrap()
                .get(account)
                .map(|account| account.mode.clone())
                .unwrap_or_else(|| state.mode.lock().unwrap().clone())
        })
}

fn is_compaction(body: &Value) -> bool {
    body["input"].as_array().is_some_and(|input| {
        input
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
    })
}
fn configured_events(
    state: &Mock,
    body: &Value,
    id: &str,
    mode: &str,
    account: &str,
) -> Vec<Value> {
    let mut events = if is_compaction(body) {
        let mut output = vec![
            json!({"type":"compaction","id":format!("cmp_{id}"),"encrypted_content":format!("synthetic-encrypted-state-{account}-{id}")}),
        ];
        if mode == "missing_checkpoint" {
            output.clear();
        }
        let mut events =
            vec![json!({"type":"response.created","response":{"id":id,"status":"in_progress"}})];
        for (index, item) in output
            .iter()
            .enumerate()
            .filter(|_| mode != "opaque_terminal")
        {
            events
                .push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
        }
        events.push(json!({"type":"response.completed","response":{"id":id,"status":"completed","output":if mode == "opaque_terminal" {output} else {vec![]},"usage":{"input_tokens":4,"output_tokens":1}}}));
        events
    } else {
        events(body, id, state.tools.load(Ordering::SeqCst))
    };
    if mode == "encrypted_tools" {
        for event in &mut events {
            if event["item"]["type"] == "function_call" {
                event["item"]["encrypted_function_args"] =
                    json!([format!("synthetic-tool-state-{account}")]);
            }
            if let Some(output) = event["response"]["output"].as_array_mut() {
                for item in output {
                    if item["type"] == "function_call" {
                        item["encrypted_function_args"] =
                            json!([format!("synthetic-tool-state-{account}")]);
                    }
                }
            }
        }
    }
    match mode {
        "native_extensions" => {
            let reasoning = json!({"type":"reasoning","id":"rs_extensions","summary":[],"encrypted_content":format!("synthetic-extension-state-{account}")});
            let custom = json!({"type":"custom_tool_call","id":"ct_extensions","call_id":"call_custom","name":"apply_patch","namespace":"functions","input":"*** Begin Patch\n*** End Patch","status":"completed"});
            let message = json!({"type":"message","id":"msg_extensions","role":"assistant","phase":"commentary","status":"completed","content":[{"type":"output_text","text":"Привет 🌍","annotations":[]}]});
            let function = json!({"type":"function_call","id":"fc_extensions","call_id":"call_function","name":"fixture","arguments":"{}","encrypted_function_args":[format!("synthetic-extension-tool-state-{account}")],"status":"completed"});
            let mut terminal = events.last().unwrap().clone();
            terminal["response"]["output"] = json!([reasoning, custom, message, function]);
            events.truncate(1);
            events.extend([
                json!({"type":"response.output_item.added","output_index":0,"item":reasoning}),
                json!({"type":"response.output_item.added","output_index":1,"item":{"type":"custom_tool_call","id":"ct_extensions","call_id":"call_custom","name":"apply_patch","namespace":"functions","input":"","status":"in_progress"}}),
                json!({"type":"response.custom_tool_call_input.delta","output_index":1,"item_id":"ct_extensions","delta":"*** Begin Patch\n"}),
                json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"rs_extensions","summary_index":0,"delta":"Synthetic reasoning summary"}),
                json!({"type":"response.output_text.delta","output_index":2,"item_id":"msg_extensions","content_index":0,"delta":"Привет 🌍"}),
                json!({"type":"response.custom_tool_call_input.delta","output_index":1,"item_id":"ct_extensions","delta":"*** End Patch"}),
                json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
                json!({"type":"response.output_item.done","output_index":1,"item":custom}),
                json!({"type":"response.output_item.done","output_index":2,"item":message}),
                json!({"type":"response.output_item.done","output_index":3,"item":function}),
                terminal,
            ]);
        }
        "empty_terminal_output" => events.last_mut().unwrap()["response"]["output"] = json!([]),
        "missing_terminal_output" => {
            events.last_mut().unwrap()["response"]
                .as_object_mut()
                .unwrap()
                .remove("output");
        }
        "unauthorized" => {
            events = vec![
                json!({"type":"error","status":401,"error":{"code":"invalid_token","message":"private-upstream-secret"}}),
            ]
        }
        "server_error" => {
            events = vec![
                json!({"type":"error","status":503,"headers":{"Retry-After":"17"},"error":{"code":"server_error","message":"private-upstream-secret"}}),
            ]
        }
        "opaque" | "opaque_wait" | "opaque_terminal" => {
            let reasoning = json!({"type":"reasoning","id":format!("rs_{id}"),"summary":[],"encrypted_content":format!("synthetic-reasoning-{id}")});
            let checkpoint = json!({"type":"context_compaction","id":format!("cmp_{id}"),"encrypted_content":format!("synthetic-context-{id}")});
            let last = events.last_mut().unwrap();
            last["response"]["output"]
                .as_array_mut()
                .unwrap()
                .extend([reasoning.clone(), checkpoint.clone()]);
            if mode != "opaque_terminal" {
                let index = events.len() - 1;
                events.splice(index..index,[json!({"type":"response.output_item.done","output_index":1,"item":reasoning}),json!({"type":"response.output_item.done","output_index":2,"item":checkpoint})]);
            }
        }
        "quota_error" | "quota_after_tool" | "late_quota_error"
            if mode != "quota_after_tool"
                || body["input"].as_array().is_some_and(|input| {
                    input
                        .iter()
                        .any(|item| item["type"] == "function_call_output")
                }) =>
        {
            let mut prefix = if mode == "late_quota_error" {
                events[..2].to_vec()
            } else {
                Vec::new()
            };
            events = vec![
                json!({"type":"error","status":429,"error":{"type":"usage_limit_reached","message":"synthetic quota rejection","resets_at":chrono::Utc::now().timestamp()+300},"headers":{"x-codex-primary-used-percent":"100","x-codex-primary-window-minutes":"300","x-codex-primary-reset-at":(chrono::Utc::now().timestamp()+300).to_string()}}),
            ];
            prefix.append(&mut events);
            events = prefix;
        }
        "disconnect" if body["generate"] != false => events.truncate(2),
        "early_close" if body["generate"] != false => events.clear(),
        "missing_terminal" if body["generate"] != false => {
            events.retain(|event| event["type"] != "response.completed")
        }
        "duplicate" => events.push(events.last().unwrap().clone()),
        "unknown_usage" => events.last_mut().unwrap()["response"]["usage"] = Value::Null,
        "partial_usage" => {
            events.last_mut().unwrap()["response"]["usage"] =
                json!({"input_tokens":10,"input_tokens_details":{"cached_tokens":3}})
        }
        "length" | "filtered" => {
            let terminal = events.last_mut().unwrap();
            terminal["type"] = json!("response.incomplete");
            terminal["response"]["status"] = json!("incomplete");
            terminal["response"]["incomplete_details"] =
                json!({"reason":if mode=="length" {"max_output_tokens"} else {"content_filter"}});
        }
        "failed" => {
            let terminal = events.last_mut().unwrap();
            terminal["type"] = json!("response.failed");
            terminal["response"]["status"] = json!("failed");
            terminal["response"]["error"] =
                json!({"code":"server_error","message":"private-upstream-secret"});
        }
        "refusal" => {
            let item = json!({"id":"msg_fixture","type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"Synthetic refusal"}]});
            let mut terminal = events.pop().unwrap();
            terminal["response"]["output"] = json!([item]);
            events.truncate(1);
            events.push(json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_fixture","type":"message","role":"assistant","content":[]}}));
            events.push(json!({"type":"response.refusal.delta","item_id":"msg_fixture","output_index":0,"content_index":0,"delta":"Synthetic refusal"}));
            events.push(json!({"type":"response.output_item.done","output_index":0,"item":item}));
            events.push(terminal);
        }
        "parallel_tools" => {
            let a = json!({"type":"function_call","id":"fc_a","call_id":"call_a","name":"hello_a","arguments":"{\"word\":\"привет\"}","status":"completed"});
            let b = json!({"type":"function_call","id":"fc_b","call_id":"call_b","name":"hello_b","arguments":"{\"word\":\"мир\"}","status":"completed"});
            let mut terminal = events.pop().unwrap();
            terminal["response"]["output"] = json!([a, b]);
            events.truncate(1);
            for (index, item) in [(0, &a), (1, &b)] {
                let mut added = item.clone();
                added["arguments"] = json!("");
                added["status"] = json!("in_progress");
                events.push(
                    json!({"type":"response.output_item.added","output_index":index,"item":added}),
                );
            }
            for (index, id, delta) in [
                (0, "fc_a", "{\"word\":"),
                (1, "fc_b", "{\"word\":"),
                (1, "fc_b", "\"мир\"}"),
                (0, "fc_a", "\"привет\"}"),
            ] {
                events.push(json!({"type":"response.function_call_arguments.delta","item_id":id,"output_index":index,"delta":delta}));
            }
            events.push(json!({"type":"response.output_item.done","output_index":0,"item":a}));
            events.push(json!({"type":"response.output_item.done","output_index":1,"item":b}));
            events.push(terminal);
        }
        "corrupt_terminal" => {
            events.last_mut().unwrap()["response"]["output"][0]["content"][0]["text"] =
                json!("changed terminal text")
        }
        "unsupported_output" => {
            let unsupported = json!({"id":"audio_1","type":"audio","data":"private-output"});
            events[1]["item"] = unsupported.clone();
            let terminal = events.pop().unwrap();
            events.truncate(2);
            events.push(terminal);
        }
        _ => {}
    }
    if let Some(event) = state.quota_event.lock().unwrap().clone() {
        events.insert(0, event);
    }
    events
}

fn safe_tool(body: &Value) -> Option<(String, Option<String>, Value)> {
    let mut tools = body
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in body["input"].as_array().into_iter().flatten() {
        if item["type"] == "additional_tools" {
            tools.extend(item["tools"].as_array()?.clone());
        }
    }
    for tool in &tools {
        let namespace = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|_| tool["type"] == "namespace");
        let candidates = if namespace.is_some() {
            tool.get("tools")?.as_array()?.clone()
        } else {
            vec![tool.clone()]
        };
        for candidate in candidates {
            let Some(name) = candidate.get("name").and_then(Value::as_str) else {
                continue;
            };
            if name == "exec_command" {
                return Some((
                    name.into(),
                    namespace.map(str::to_owned),
                    json!({"cmd":"echo EXETROUTER_TOOL_OK","max_output_tokens":1000}),
                ));
            }
            if name == "shell" {
                return Some((
                    name.into(),
                    namespace.map(str::to_owned),
                    json!({"command":"echo EXETROUTER_TOOL_OK","timeout":10000}),
                ));
            }
        }
    }
    None
}

pub fn events(body: &Value, id: &str, tools: bool) -> Vec<Value> {
    let mut response = json!({"id":id,"object":"response","created_at":chrono::Utc::now().timestamp(),"status":"in_progress","model":body["model"],"output":[],"usage":null,"error":null,"incomplete_details":null});
    let mut events = vec![json!({"type":"response.created","response":response})];
    if body["generate"] == false {
        response["status"] = json!("completed");
        response["usage"] = json!({"input_tokens":0,"output_tokens":0,"total_tokens":0});
        events.push(json!({"type":"response.completed","response":response}));
        return events;
    }
    let has_result = body
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|input| {
            input
                .iter()
                .any(|item| item["type"] == "function_call_output")
        });
    let tool = if tools && !has_result {
        safe_tool(body)
    } else {
        None
    };
    let item = if let Some((name, namespace, args)) = tool {
        let args = args.to_string();
        let mut item = json!({"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":name,"arguments":"","status":"in_progress"});
        if let Some(namespace) = namespace {
            item["namespace"] = json!(namespace);
        }
        events.push(json!({"type":"response.output_item.added","output_index":0,"item":item}));
        events.push(json!({"type":"response.function_call_arguments.delta","item_id":"fc_fixture","output_index":0,"delta":args}));
        events.push(json!({"type":"response.function_call_arguments.done","item_id":"fc_fixture","output_index":0,"arguments":args}));
        item["arguments"] = json!(args);
        item["status"] = json!("completed");
        item
    } else {
        let text = "EXETROUTER_SMOKE_OK";
        let part = json!({"type":"output_text","text":text,"annotations":[]});
        let item = json!({"id":"msg_fixture","type":"message","role":"assistant","status":"completed","content":[part]});
        events.push(json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_fixture","type":"message","role":"assistant","status":"in_progress","content":[]}}));
        events.push(json!({"type":"response.content_part.added","item_id":"msg_fixture","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}));
        events.push(json!({"type":"response.output_text.delta","item_id":"msg_fixture","output_index":0,"content_index":0,"delta":text}));
        events.push(json!({"type":"response.output_text.done","item_id":"msg_fixture","output_index":0,"content_index":0,"text":text}));
        events.push(json!({"type":"response.content_part.done","item_id":"msg_fixture","output_index":0,"content_index":0,"part":part}));
        item
    };
    events.push(json!({"type":"response.output_item.done","output_index":0,"item":item}));
    response["status"] = json!("completed");
    response["output"] = json!([item]);
    response["usage"] = json!({"input_tokens":10,"output_tokens":2,"total_tokens":12,"input_tokens_details":{"cached_tokens":3},"output_tokens_details":{"reasoning_tokens":1}});
    events.push(
        json!({"type":"response.completed","response":response,"fixture_extension":"preserved"}),
    );
    for (index, event) in events.iter_mut().enumerate() {
        event["sequence_number"] = json!(index);
    }
    events
}
