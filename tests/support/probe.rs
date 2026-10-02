//! Loopback-only observation of synthetic native-client tool results. Bodies
//! and headers are forwarded in memory and never saved or printed.
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocketUpgrade},
        Request, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    Router,
};
use exetrouter::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    io::Read,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as UpstreamMessage};

#[derive(Default)]
pub struct Observed {
    pub results: usize,
    pub markers: usize,
    pub categories: Vec<&'static str>,
    pub tool_names: Vec<&'static str>,
    pub additional_tools: usize,
    pub request_tools: usize,
    pub primary_requests: usize,
    pub compaction_requests: usize,
    pub checkpoint_requests: usize,
    pub checkpoint_tool_results: usize,
    pub requests: usize,
    memory: Option<String>,
    pub compaction_with_memory: usize,
    pub checkpoint_websocket_tool_results: usize,
    pub checkpoint_http_tool_results: usize,
    pub usage: Vec<(Option<u64>, Option<u64>)>,
    pub http_received: usize,
    pub http_bodies_read: usize,
    pub http_forwarded: usize,
    pub http_headers_received: usize,
}
impl Observed {
    pub fn expect_memory(&mut self, memory: &str) {
        self.memory = Some(memory.into());
    }
    fn see(&mut self, body: &[u8], previous_checkpoint: bool) {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return;
        };
        self.request_tools += value["tools"].as_array().map_or(0, Vec::len);
        let items = value["input"].as_array();
        let checkpoint = (previous_checkpoint
            && value["previous_response_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()))
            || items.is_some_and(|items| {
                items.iter().any(|item| {
                    item["type"] == "compaction"
                        && item["encrypted_content"]
                            .as_str()
                            .is_some_and(|content| !content.is_empty())
                })
            });
        self.compaction_requests += usize::from(items.is_some_and(|items| {
            items
                .iter()
                .any(|item| item["type"] == "compaction_trigger")
        }));
        if items.is_some_and(|items| {
            items
                .iter()
                .any(|item| item["type"] == "compaction_trigger")
        }) && self
            .memory
            .as_ref()
            .is_some_and(|memory| value["input"].to_string().contains(memory))
        {
            self.compaction_with_memory += 1;
        }
        self.checkpoint_requests += usize::from(checkpoint);
        if value["generate"] != false
            && (value["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
                || value["input"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["type"] == "additional_tools"
                            && item["tools"]
                                .as_array()
                                .is_some_and(|tools| !tools.is_empty())
                    })
                }))
        {
            self.primary_requests += 1;
        }
        for item in value["input"].as_array().into_iter().flatten() {
            if item["type"] == "additional_tools" {
                self.additional_tools += 1;
            }
            if matches!(
                item["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            ) {
                self.results += 1;
                self.checkpoint_tool_results += usize::from(checkpoint);
                let output = item["output"].to_string().to_ascii_lowercase();
                self.markers += usize::from(output.contains("exetrouter_tool_ok"));
                for term in [
                    "unknown tool",
                    "not found",
                    "namespace",
                    "invalid",
                    "arguments",
                    "sandbox",
                    "permission",
                    "not permitted",
                    "error",
                ] {
                    if output.contains(term) && !self.categories.contains(&term) {
                        self.categories.push(term);
                    }
                }
            }
            if matches!(
                item["type"].as_str(),
                Some("function_call" | "custom_tool_call")
            ) {
                for name in [
                    "exec_command",
                    "functions.exec_command",
                    "shell",
                    "functions.shell",
                    "exec",
                    "functions.exec",
                    "shell_command",
                    "functions.shell_command",
                ] {
                    if item["name"] == name && !self.tool_names.contains(&name) {
                        self.tool_names.push(name);
                    }
                }
            }
        }
    }
    pub fn metadata(&self) -> Value {
        json!({"tool_results":self.results,"tool_markers":self.markers,"categories":self.categories,"tool_names":self.tool_names,"additional_tools":self.additional_tools,"request_tools":self.request_tools,"primary_requests":self.primary_requests,"compaction_requests":self.compaction_requests,"compaction_with_memory":self.compaction_with_memory,"checkpoint_requests":self.checkpoint_requests,"checkpoint_tool_results":self.checkpoint_tool_results,"checkpoint_websocket_tool_results":self.checkpoint_websocket_tool_results,"checkpoint_http_tool_results":self.checkpoint_http_tool_results,"submitted_requests":self.requests,"terminal_usage_samples":self.usage.len(),"known_usage_samples":self.usage.iter().filter(|(input,output)|input.is_some()&&output.is_some()).count(),"last_input_tokens":self.usage.last().and_then(|(input,_)|*input),"max_input_tokens":self.usage.iter().filter_map(|(input,_)|*input).max(),"http_received":self.http_received,"http_bodies_read":self.http_bodies_read,"http_forwarded":self.http_forwarded,"http_headers_received":self.http_headers_received})
    }
    fn response(&mut self, value: &Value) {
        if self.usage.len() >= 128 {
            return;
        }
        let response = if value["type"] == "response.completed" {
            &value["response"]
        } else if matches!(
            value["object"].as_str(),
            Some("response" | "response.compaction")
        ) {
            value
        } else {
            return;
        };
        self.usage.push((
            response["usage"]["input_tokens"].as_u64(),
            response["usage"]["output_tokens"].as_u64(),
        ));
    }
}
const BODY_LIMIT: usize = 16 * 1024 * 1024;
struct ResponseWatch {
    buffer: Vec<u8>,
    sse: bool,
    disabled: bool,
    observed: Arc<Mutex<Observed>>,
}
impl ResponseWatch {
    fn push(&mut self, bytes: &[u8]) {
        if self.disabled {
            return;
        }
        for byte in bytes {
            if self.buffer.len() == BODY_LIMIT {
                self.disabled = true;
                self.buffer.clear();
                return;
            }
            self.buffer.push(*byte);
            if self.sse && (self.buffer.ends_with(b"\n\n") || self.buffer.ends_with(b"\r\n\r\n")) {
                let frame = std::mem::take(&mut self.buffer);
                if let Ok(text) = std::str::from_utf8(&frame) {
                    let data = text
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix("data:")
                                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Ok(value) = serde_json::from_str::<Value>(&data) {
                        self.observed.lock().unwrap().response(&value);
                    }
                }
            }
        }
    }
    fn finish(&mut self) {
        if !self.sse && !self.disabled {
            if let Ok(value) = serde_json::from_slice::<Value>(&self.buffer) {
                self.observed.lock().unwrap().response(&value);
            }
        }
    }
}
struct StateData {
    target: String,
    client: reqwest::Client,
    observed: Arc<Mutex<Observed>>,
    request_limit: usize,
}
pub struct Probe {
    pub url: String,
    pub observed: Arc<Mutex<Observed>>,
    job: JoinHandle<()>,
}
impl Probe {
    pub async fn bounded(target: &str, request_limit: usize) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let observed = Arc::new(Mutex::new(Observed::default()));
        let state = Arc::new(StateData {
            target: target.into(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            observed: observed.clone(),
            request_limit,
        });
        let router = Router::new().fallback(forward).with_state(state);
        let job = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self { url, observed, job })
    }
}
fn admit(state: &StateData, body: &[u8], websocket: bool, previous_checkpoint: bool) -> bool {
    let inference = serde_json::from_slice::<Value>(body)
        .ok()
        .is_some_and(|value| value["model"].is_string() && value["input"].is_array());
    let mut observed = state.observed.lock().unwrap();
    if inference {
        if observed.requests >= state.request_limit {
            return false;
        }
        observed.requests += 1;
    }
    let before = observed.checkpoint_tool_results;
    observed.see(body, websocket && previous_checkpoint);
    let results = observed.checkpoint_tool_results - before;
    if websocket {
        observed.checkpoint_websocket_tool_results += results;
    } else {
        observed.checkpoint_http_tool_results += results;
    }
    true
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.job.abort();
    }
}
async fn forward(State(state): State<Arc<StateData>>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();
    let url = format!("{}{}", state.target, parts.uri);
    if let Ok(upgrade) =
        <WebSocketUpgrade as axum::extract::FromRequestParts<Arc<StateData>>>::from_request_parts(
            &mut parts, &state,
        )
        .await
    {
        let Ok(mut upstream_request) = url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
            .into_client_request()
        else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        for name in [
            "authorization",
            "openai-beta",
            "session_id",
            "session-id",
            "x-session-id",
            "x-session-affinity",
            "thread-id",
            "x-codex-turn-state",
            "x-openai-internal-codex-responses-lite",
        ] {
            if let Some(value) = parts.headers.get(name) {
                upstream_request.headers_mut().insert(name, value.clone());
            }
        }
        let Ok((mut upstream, _)) = tokio_tungstenite::connect_async(upstream_request).await else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        return upgrade.on_upgrade(move |mut client|async move {
            let mut checkpoint_on_connection = false;
            loop {
                tokio::select! {
                    message=client.recv()=>{
                        let Some(Ok(message))=message else {break;};
                        let message=match message {
                            Message::Text(text)=>{
                                if !admit(&state,text.as_bytes(),true,checkpoint_on_connection) {let _=client.send(Message::Text(json!({"type":"error","error":{"type":"test_request_budget_exhausted"}}).to_string().into())).await;break;}
                                checkpoint_on_connection=serde_json::from_slice::<Value>(text.as_bytes()).ok().is_some_and(|value|(checkpoint_on_connection && value["previous_response_id"].is_string()) || value["input"].as_array().is_some_and(|items|items.iter().any(|item|item["type"]=="compaction" && item["encrypted_content"].is_string())));
                                UpstreamMessage::Text(text.to_string().into())
                            },
                            Message::Binary(bytes)=>UpstreamMessage::Binary(bytes),
                            Message::Ping(bytes)=>UpstreamMessage::Ping(bytes),
                            Message::Pong(bytes)=>UpstreamMessage::Pong(bytes),
                            Message::Close(_)=>break,
                        };
                        if upstream.send(message).await.is_err() {break;}
                    },
                    message=upstream.next()=>{
                        let Some(Ok(message))=message else {break;};
                        let message=match message {
                            UpstreamMessage::Text(text)=>{
                                if let Ok(value)=serde_json::from_str::<Value>(&text) { state.observed.lock().unwrap().response(&value); }
                                Message::Text(text.to_string().into())
                            },
                            UpstreamMessage::Binary(bytes)=>Message::Binary(bytes),
                            UpstreamMessage::Ping(bytes)=>Message::Ping(bytes),
                            UpstreamMessage::Pong(bytes)=>Message::Pong(bytes),
                            UpstreamMessage::Close(_)=>break,
                            UpstreamMessage::Frame(_)=>continue,
                        };
                        if client.send(message).await.is_err() {break;}
                    },
                }
            }
            let _=upstream.close(None).await;
        }).into_response();
    }
    state.observed.lock().unwrap().http_received += 1;
    let Ok(bytes) = axum::body::to_bytes(body, BODY_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    state.observed.lock().unwrap().http_bodies_read += 1;
    let decoded;
    let observation = if parts
        .headers
        .get("content-encoding")
        .is_some_and(|v| v == "zstd")
    {
        let Ok(mut decoder) = zstd::stream::read::Decoder::new(bytes.as_ref()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        if decoder.window_log_max(23).is_err() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let mut output = Vec::new();
        if decoder
            .take(BODY_LIMIT as u64 + 1)
            .read_to_end(&mut output)
            .is_err()
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if output.len() > BODY_LIMIT {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        decoded = output;
        decoded.as_slice()
    } else {
        bytes.as_ref()
    };
    if !admit(&state, observation, false, false) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error":{"type":"test_request_budget_exhausted"}})),
        )
            .into_response();
    }
    let mut headers = parts.headers;
    for name in ["host", "content-length", "connection", "transfer-encoding"] {
        headers.remove(name);
    }
    state.observed.lock().unwrap().http_forwarded += 1;
    let Ok(response) = state
        .client
        .request(parts.method, url)
        .headers(headers)
        .body(bytes)
        .send()
        .await
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    state.observed.lock().unwrap().http_headers_received += 1;
    let status = response.status();
    let mut headers = response.headers().clone();
    headers.remove("transfer-encoding");
    let sse = headers
        .get("content-type")
        .is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"));
    let watch = ResponseWatch {
        buffer: Vec::new(),
        sse,
        disabled: false,
        observed: state.observed.clone(),
    };
    let stream = futures_util::stream::unfold(
        (response.bytes_stream(), watch),
        |(mut stream, mut watch)| async move {
            match stream.next().await {
                Some(result) => {
                    if let Ok(bytes) = &result {
                        watch.push(bytes);
                    }
                    Some((result, (stream, watch)))
                }
                None => {
                    watch.finish();
                    None
                }
            }
        },
    );
    let mut result = Response::new(Body::from_stream(stream));
    *result.headers_mut() = headers;
    *result.status_mut() = status;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_response_observation_keeps_only_numeric_usage() {
        let observed = Arc::new(Mutex::new(Observed::default()));
        let mut watch = ResponseWatch {
            buffer: Vec::new(),
            sse: true,
            disabled: false,
            observed: observed.clone(),
        };
        let frame=b"data: {\r\ndata: \"type\":\"response.completed\",\"response\":{\"output\":\"private synthetic text\",\"usage\":{\"input_tokens\":42,\"output_tokens\":7}}}\r\n\r\n";
        for chunk in frame.chunks(3) {
            watch.push(chunk);
        }
        watch.finish();
        assert_eq!(observed.lock().unwrap().usage, vec![(Some(42), Some(7))]);
        assert!(!observed
            .lock()
            .unwrap()
            .metadata()
            .to_string()
            .contains("private synthetic text"));
        watch.sse = false;
        watch.push(br#"{"object":"response.compaction","usage":{"input_tokens":50}}"#);
        watch.finish();
        assert_eq!(
            observed.lock().unwrap().usage.last(),
            Some(&(Some(50), None))
        );
        watch.buffer = vec![b' '; BODY_LIMIT];
        watch.push(b"x");
        assert!(watch.disabled && watch.buffer.is_empty());
    }
    #[test]
    fn request_budget_blocks_forwarding_and_observations_retain_only_proof() {
        let observed = Arc::new(Mutex::new(Observed::default()));
        observed
            .lock()
            .unwrap()
            .expect_memory("synthetic-private-memory");
        let state = StateData {
            target: "http://127.0.0.1:1".into(),
            client: reqwest::Client::new(),
            observed: observed.clone(),
            request_limit: 4,
        };
        assert!(admit(&state, b"", false, false));
        let compact=json!({"model":"test","input":[{"role":"user","content":"synthetic-private-memory"},{"type":"compaction_trigger"}]}).to_string();
        assert!(admit(&state, compact.as_bytes(), true, false));
        let delta=json!({"model":"test","previous_response_id":"synthetic-response","input":[{"type":"function_call_output","output":"EXETROUTER_TOOL_OK"}]}).to_string();
        assert!(admit(&state, delta.as_bytes(), true, false));
        let follow=json!({"model":"test","input":[{"type":"compaction","encrypted_content":"synthetic-opaque-checkpoint"},{"type":"function_call_output","output":"EXETROUTER_TOOL_OK"}]}).to_string();
        assert!(admit(&state, follow.as_bytes(), true, false));
        assert!(admit(&state, delta.as_bytes(), true, true));
        assert!(!admit(&state, compact.as_bytes(), true, true));
        let meta = observed.lock().unwrap().metadata();
        assert_eq!(meta["compaction_with_memory"], 1);
        assert_eq!(meta["checkpoint_tool_results"], 2);
        assert_eq!(meta["checkpoint_websocket_tool_results"], 2);
        assert_eq!(observed.lock().unwrap().requests, 4);
        assert!(!meta.to_string().contains("synthetic-private-memory"));
        assert!(!meta.to_string().contains("synthetic-opaque-checkpoint"));
    }
}
