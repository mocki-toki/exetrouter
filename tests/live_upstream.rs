//! Explicit opt-in checks against an isolated operator-created OAuth state.
//! Reports contain test metadata, never credentials or opaque context.
#[path = "support/live_pool.rs"]
mod live_pool;
#[path = "support/mock.rs"]
mod mock;
#[path = "support/native.rs"]
mod native;
#[path = "support/probe.rs"]
mod probe;
use exetrouter::{
    oauth, server,
    store::Database,
    upstream::{self, Upstream},
    Result,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

fn private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err("live test state must contain regular private files".into());
    }
    Ok(())
}
fn private_key(path: &Path) -> Result<[u8; 32]> {
    private_file(path)?;
    fs::read(path)?
        .try_into()
        .map_err(|_| "invalid live test key length".into())
}
async fn live_state() -> Result<(PathBuf, Database, Arc<Upstream>, [u8; 32])> {
    live_state_accounts(1).await
}
async fn live_state_accounts(
    expected: i64,
) -> Result<(PathBuf, Database, Arc<Upstream>, [u8; 32])> {
    let directory = PathBuf::from(
        std::env::var_os("EXETROUTER_LIVE_STATE")
            .ok_or("EXETROUTER_LIVE_STATE must name an isolated test directory")?,
    );
    private_file(&directory.join("exetrouter.sqlite"))?;
    let key = private_key(&directory.join("exetrouter.key"))?;
    let oauth_key = private_key(&directory.join("exetrouter.oauth.key"))?;
    if key == oauth_key {
        return Err("live test keys must differ".into());
    }
    if directory.join("exetrouter.sock").exists() || directory.join("live-smoke.sock").exists() {
        return Err("stop the test service before running the live checks".into());
    }
    let db = Database::open(directory.join("exetrouter.sqlite")).await?;
    db.call(move |conn| {
        let (accounts,active,pending)=conn.query_row("SELECT (SELECT COUNT(*) FROM oauth_accounts),(SELECT COUNT(*) FROM oauth_accounts WHERE state='active'),(SELECT COUNT(*) FROM usage_events WHERE status IN ('accepted','sent'))", [], |row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?)))?;
        if accounts != expected || active != expected || pending != 0 { return Err("live checks require the expected active test accounts and no pending requests".into()); }
        Ok(())
    }).await?;
    let upstream = Arc::new(Upstream::new(
        db.clone(),
        oauth_key,
        upstream::Config::new(upstream::BACKEND.into(), oauth::ISSUER.into(), false)?,
    )?);
    Ok((directory, db, upstream, key))
}

#[tokio::test]
#[ignore = "contacts the real OAuth backend; requires EXETROUTER_LIVE_STATE"]
async fn live_catalog() {
    let (_, _, upstream, _) = live_state().await.expect("invalid live test state");
    let models = upstream
        .models()
        .await
        .expect("real catalogue discovery failed");
    assert!(!models.is_empty(), "test account has no visible models");
    let codex = exetrouter::catalog::codex(&models).expect("real catalog lacks Codex metadata");
    let opencode =
        exetrouter::catalog::opencode(&models).expect("real catalog lacks OpenCode metadata");
    assert_eq!(codex["models"].as_array().unwrap().len(), models.len());
    assert_eq!(
        opencode["providers"]["openai"]["models"]
            .as_object()
            .unwrap()
            .len(),
        models.len()
    );
    println!(
        "{}",
        json!({"check":"catalog","models":models.iter().map(|model|&model.id).collect::<Vec<_>>() })
    );
}

struct Session {
    directory: PathBuf,
    signing_key: [u8; 32],
    db: Database,
    upstream: Arc<Upstream>,
    client: reqwest::Client,
    url: String,
    secret: String,
    token_id: String,
    user: i64,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<Result<()>>,
}
impl Session {
    async fn start(
        directory: &Path,
        db: Database,
        upstream: Arc<Upstream>,
        key: [u8; 32],
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(180))
            .build()?;
        let reservation = TcpListener::bind("127.0.0.1:0").await?;
        let address = reservation.local_addr()?;
        let (user, token) = db
            .call(move |conn| {
                let tx = rusqlite::Transaction::new_unchecked(
                    conn,
                    rusqlite::TransactionBehavior::Immediate,
                )?;
                let user =
                    exetrouter::create_user(&tx, &format!("smoke-{:016x}", rand::random::<u64>()))?;
                let token = exetrouter::create_token(&tx, &key, user, "live-smoke", 1)?;
                tx.commit()?;
                Ok((user, token))
            })
            .await?;
        drop(reservation);
        let (stop, stopped) = oneshot::channel();
        let config = server::ServeConfig {
            gateway_uid: unsafe { libc::geteuid() },
            listen: address,
            socket: directory.join("live-smoke.sock"),
            upstream: Some(upstream.clone()),
            generations_per_user: 8,
            websockets_per_user: 8,
        };
        let serve_db = db.clone();
        let server = tokio::spawn(server::serve(serve_db, key.to_vec(), config, async {
            let _ = stopped.await;
        }));
        let session = Self {
            directory: directory.to_owned(),
            signing_key: key,
            db,
            upstream,
            client,
            url: format!("http://{address}"),
            secret: token.secret,
            token_id: token.token.id,
            user,
            stop: Some(stop),
            server,
        };
        for _ in 0..100 {
            if session.server.is_finished() {
                break;
            }
            if session
                .client
                .get(format!("{}/healthz", session.url))
                .send()
                .await
                .is_ok()
            {
                return Ok(session);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        session.close().await?;
        Err("live smoke service failed to start".into())
    }
    async fn close(mut self) -> Result<()> {
        let (user, id) = (self.user, self.token_id.clone());
        let revoked = self
            .db
            .call(move |conn| {
                exetrouter::revoke_token(conn, user, &id)?;
                Ok(())
            })
            .await;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let drained = tokio::time::timeout(Duration::from_secs(20), &mut self.server).await;
        revoked?;
        drained.map_err(|_| "live smoke shutdown timed out")???;
        Ok(())
    }
    async fn restart(&mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(Duration::from_secs(20), &mut self.server)
            .await
            .map_err(|_| "restart drain timed out")???;
        self.db = Database::open(self.directory.join("exetrouter.sqlite")).await?;
        self.upstream = Arc::new(Upstream::new(
            self.db.clone(),
            private_key(&self.directory.join("exetrouter.oauth.key"))?,
            upstream::Config::new(upstream::BACKEND.into(), oauth::ISSUER.into(), false)?,
        )?);
        let (stop, stopped) = oneshot::channel();
        let config = server::ServeConfig {
            gateway_uid: unsafe { libc::geteuid() },
            listen: self
                .url
                .strip_prefix("http://")
                .ok_or("invalid local URL")?
                .parse()?,
            socket: self.directory.join("live-smoke.sock"),
            upstream: Some(self.upstream.clone()),
            generations_per_user: 8,
            websockets_per_user: 8,
        };
        self.server = tokio::spawn(server::serve(
            self.db.clone(),
            self.signing_key.to_vec(),
            config,
            async {
                let _ = stopped.await;
            },
        ));
        self.stop = Some(stop);
        for _ in 0..100 {
            if self
                .client
                .get(format!("{}/healthz", self.url))
                .send()
                .await
                .is_ok()
            {
                return Ok(());
            }
            if self.server.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err("restarted test service unavailable".into())
    }
    async fn post(&self, path: &str, body: &Value) -> Result<reqwest::Response> {
        self.client
            .post(format!("{}/v1/{path}", self.url))
            .bearer_auth(&self.secret)
            .json(body)
            .send()
            .await
            .map_err(|_| "smoke HTTP request interrupted; do not replay blindly".into())
    }
    async fn response(&self, body: &Value) -> Result<Value> {
        let response = self.post("responses", body).await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let error = response.json::<Value>().await.ok();
            let code = error
                .as_ref()
                .and_then(|value| value["error"]["type"].as_str())
                .filter(|value| value.len() < 100)
                .unwrap_or("unknown");
            return Err(format!("Responses HTTP rejected with {status} ({code})").into());
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| "invalid Responses JSON")?;
        completed(&value)?;
        Ok(value)
    }
    async fn rows(&self) -> Result<Vec<Value>> {
        let token = self.token_id.clone();
        self.db.call(move |conn| {
            let mut stmt=conn.prepare("SELECT id,status,input_tokens,output_tokens,upstream_transport,client_transport,upstream_response_id,cached_input_tokens FROM usage_events WHERE token_id=?1 ORDER BY id")?;
            let rows=stmt.query_map([token],|row|Ok(json!({"id":row.get::<_,i64>(0)?,"status":row.get::<_,String>(1)?,"input":row.get::<_,Option<i64>>(2)?,"output":row.get::<_,Option<i64>>(3)?,"upstream":row.get::<_,Option<String>>(4)?,"client":row.get::<_,Option<String>>(5)?,"response":row.get::<_,Option<String>>(6)?,"cached":row.get::<_,Option<i64>>(7)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(rows)
        }).await
    }
    async fn verify(&self, start: usize, count: usize, transport: &str) -> Result<()> {
        let rows = self.rows().await?;
        let new = rows.get(start..).ok_or("smoke usage count changed")?;
        if new.len() != count
            || new.iter().any(|row| {
                row["status"] != "completed"
                    || row["upstream"] != transport
                    || row["input"].as_i64().is_none()
                    || row["output"].as_i64().is_none()
            })
        {
            return Err("smoke usage or observed transport did not match the check".into());
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn completed(response: &Value) -> Result<()> {
    if response["status"] != "completed" {
        return Err("upstream did not complete the smoke response".into());
    }
    if response["usage"]["input_tokens"]
        .as_i64()
        .is_none_or(|value| value < 0)
        || response["usage"]["output_tokens"]
            .as_i64()
            .is_none_or(|value| value < 0)
    {
        return Err("upstream did not report complete usage".into());
    }
    Ok(())
}
fn marker(response: &Value) -> Result<()> {
    completed(response)?;
    let found = response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .any(|part| {
            part["text"]
                .as_str()
                .is_some_and(|text| text.contains("EXETROUTER_SMOKE_OK"))
        });
    if !found {
        println!(
            "{}",
            json!({"check":"response_shape","keys":response.as_object().map(|object|object.keys().collect::<Vec<_>>()),"output_types":response["output"].as_array().map(|items|items.iter().map(|item|json!({"type":item["type"],"content":item["content"].as_array().map(|parts|parts.iter().map(|part|json!({"type":part["type"],"text_bytes":part["text"].as_str().map(str::len)})).collect::<Vec<_>>())})).collect::<Vec<_>>())})
        );
        return Err("model did not return the expected smoke marker".into());
    }
    Ok(())
}
fn body(model: &str) -> Value {
    json!({"model":model,"instructions":"This is a connectivity test. Follow the user instruction and return only the requested marker.","input":[{"role":"user","content":"Return exactly EXETROUTER_SMOKE_OK."}],"store":false,"stream":false,"include":["reasoning.encrypted_content"]})
}
async fn ws_terminal(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(180), async {
        let mut items = BTreeMap::new();
        while let Some(message) = socket.next().await {
            match message.map_err(|_| "smoke WebSocket interrupted; do not replay blindly")? {
                Message::Text(text) => {
                    let event: Value =
                        serde_json::from_str(&text).map_err(|_| "invalid smoke WebSocket event")?;
                    match event["type"].as_str() {
                        Some("response.output_item.done") => {
                            items.insert(
                                event["output_index"]
                                    .as_u64()
                                    .ok_or("smoke output index missing")?,
                                event["item"].clone(),
                            );
                        }
                        Some("response.completed") => {
                            completed(&event["response"])?;
                            let mut response = event["response"].clone();
                            if response["output"].as_array().is_none_or(Vec::is_empty) {
                                response["output"] = json!(items.into_values().collect::<Vec<_>>());
                            }
                            return Ok(response);
                        }
                        Some("error" | "response.failed" | "response.incomplete") => {
                            return Err("upstream WebSocket rejected the smoke request".into())
                        }
                        _ => {}
                    }
                }
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
                Message::Close(_) => return Err("smoke WebSocket closed before completion".into()),
                _ => {}
            }
        }
        Err("smoke WebSocket ended before completion".into())
    })
    .await
    .map_err(|_| "smoke WebSocket timed out; do not replay blindly")?
}
async fn checks(session: &Session, model: &str, only: Option<&str>) -> Result<Value> {
    if only.is_some_and(|value| !matches!(value, "http_json" | "chat_refresh")) {
        return Err("EXETROUTER_LIVE_ONLY accepts http_json or chat_refresh".into());
    }
    if !session
        .upstream
        .models()
        .await?
        .iter()
        .any(|entry| entry.id == model)
    {
        return Err("requested smoke model is not in the live catalogue".into());
    }
    if only != Some("chat_refresh") {
        let first = session.response(&body(model)).await?;
        marker(&first)?;
        session.verify(0, 1, "http_sse").await?;
        println!("smoke: HTTP JSON and observed usage passed");
        if only == Some("http_json") {
            return Ok(json!({"checks":["http_json"],"requests":1}));
        }

        let mut streaming = body(model);
        streaming["stream"] = json!(true);
        let reply = session.post("responses", &streaming).await?;
        if !reply.status().is_success() {
            return Err("smoke SSE request rejected".into());
        }
        let text = reply
            .text()
            .await
            .map_err(|_| "smoke SSE interrupted; do not replay blindly")?;
        let events = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .collect::<Vec<_>>();
        let mut terminal = events
            .iter()
            .find(|event| event["type"] == "response.completed")
            .cloned()
            .ok_or("smoke SSE has no successful terminal")?;
        if terminal["response"]["output"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            let items = events
                .iter()
                .filter(|event| event["type"] == "response.output_item.done")
                .map(|event| {
                    Ok((
                        event["output_index"]
                            .as_u64()
                            .ok_or("smoke output index missing")?,
                        event["item"].clone(),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            terminal["response"]["output"] = json!(items.into_values().collect::<Vec<_>>());
        }
        marker(&terminal["response"])?;
        session.verify(1, 1, "http_sse").await?;
        println!("smoke: HTTP SSE passed");

        let mut tool = body(model);
        tool["input"] = json!([{"role":"user","content":"Call shell once with echo EXETROUTER_TOOL_OK. After receiving its result, return exactly EXETROUTER_SMOKE_OK."}]);
        tool["tools"] = json!([{"type":"function","name":"shell","description":"A connectivity fixture. It returns EXETROUTER_TOOL_OK and runs no command.","strict":true,"parameters":{"type":"object","properties":{"command":{"type":"string","enum":["echo EXETROUTER_TOOL_OK"]}},"required":["command"],"additionalProperties":false}}]);
        tool["tool_choice"] = json!({"type":"function","name":"shell"});
        let call = session.response(&tool).await?;
        let calls = call["output"]
            .as_array()
            .ok_or("smoke tool output missing")?
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect::<Vec<_>>();
        if calls.len() != 1 || calls[0]["name"] != "shell" {
            return Err("smoke did not return exactly one requested function call".into());
        }
        let arguments: Value = serde_json::from_str(
            calls[0]["arguments"]
                .as_str()
                .ok_or("tool arguments missing")?,
        )
        .map_err(|_| "invalid smoke tool arguments")?;
        if arguments["command"] != "echo EXETROUTER_TOOL_OK" {
            return Err("smoke function call arguments differ".into());
        }
        let mut transcript = tool["input"].as_array().unwrap().clone();
        transcript.extend(call["output"].as_array().unwrap().clone());
        transcript.push(json!({"type":"function_call_output","call_id":calls[0]["call_id"],"output":"EXETROUTER_TOOL_OK"}));
        tool["input"] = json!(transcript);
        tool["tool_choice"] = json!("none");
        marker(&session.response(&tool).await?)?;
        session.verify(2, 2, "http_sse").await?;
        println!("smoke: function call and result round-trip passed");

        let mut history = body(model)["input"].as_array().unwrap().clone();
        history.extend(
            first["output"]
                .as_array()
                .ok_or("smoke output missing")?
                .clone(),
        );
        let response=session.post("responses/compact",&json!({"model":model,"instructions":"Preserve the conversation state.","input":history})).await?;
        if !response.status().is_success() {
            return Err(format!(
                "smoke compaction rejected with {}",
                response.status().as_u16()
            )
            .into());
        }
        let compact: Value = response
            .json()
            .await
            .map_err(|_| "invalid smoke compaction JSON")?;
        if compact["object"] != "response.compaction"
            || !compact["output"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["type"] == "compaction"
                        && item["encrypted_content"]
                            .as_str()
                            .is_some_and(|content| !content.is_empty())
                })
            })
        {
            return Err("smoke compaction has no checkpoint".into());
        }
        let mut follow = body(model);
        let mut input = compact["output"].as_array().unwrap().clone();
        input.push(json!({"role":"user","content":"Return exactly EXETROUTER_SMOKE_OK."}));
        follow["input"] = json!(input);
        session.verify(4, 1, "http_sse").await?;
        marker(&session.response(&follow).await?)?;
        session.verify(5, 1, "http_sse").await?;
        println!("smoke: compaction and original-account continuation passed");

        let mut upgrade = format!("{}/v1/responses", session.url.replace("http://", "ws://"))
            .into_client_request()?;
        upgrade.headers_mut().insert(
            "authorization",
            format!("Bearer {}", session.secret).parse()?,
        );
        let (mut socket, _) = connect_async(upgrade)
            .await
            .map_err(|_| "smoke WebSocket Upgrade rejected")?;
        let mut create = body(model);
        create["type"] = json!("response.create");
        create.as_object_mut().unwrap().remove("stream");
        socket
            .send(Message::Text(create.to_string().into()))
            .await?;
        let original = ws_terminal(&mut socket).await?;
        marker(&original)?;
        create["previous_response_id"] = original["id"].clone();
        socket
            .send(Message::Text(create.to_string().into()))
            .await?;
        marker(&ws_terminal(&mut socket).await?)?;
        socket.close(None).await?;
        session.verify(6, 2, "websocket").await?;
        println!("smoke: native Responses WebSocket and continuation passed");
    }
    let tail_start = session.rows().await?.len();

    let chat=session.post("chat/completions",&json!({"model":model,"messages":[{"role":"system","content":"Return only the requested marker."},{"role":"user","content":"Return exactly EXETROUTER_SMOKE_OK."}]})).await?;
    if !chat.status().is_success() {
        return Err("smoke Chat request rejected".into());
    }
    let chat: Value = chat.json().await.map_err(|_| "invalid smoke Chat JSON")?;
    if !chat["choices"][0]["message"]["content"]
        .as_str()
        .is_some_and(|text| text.contains("EXETROUTER_SMOKE_OK"))
    {
        return Err("smoke Chat marker missing".into());
    }
    session.verify(tail_start, 1, "websocket").await?;
    println!("smoke: Chat JSON passed over upstream WebSocket");

    let chat=session.post("chat/completions",&json!({"model":model,"messages":[{"role":"system","content":"Return only the requested marker."},{"role":"user","content":"Return exactly EXETROUTER_SMOKE_OK."}],"stream":true,"stream_options":{"include_usage":true}})).await?;
    if !chat.status().is_success() {
        return Err("smoke Chat SSE rejected".into());
    }
    let wire = chat
        .text()
        .await
        .map_err(|_| "smoke Chat SSE interrupted")?;
    let chunks = wire
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>();
    let values = chunks
        .iter()
        .filter(|chunk| **chunk != "[DONE]")
        .map(|chunk| serde_json::from_str::<Value>(chunk))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| "invalid Chat SSE chunk")?;
    let text = values
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect::<String>();
    if !text.contains("EXETROUTER_SMOKE_OK")
        || chunks.iter().filter(|chunk| **chunk == "[DONE]").count() != 1
        || !values
            .iter()
            .any(|chunk| chunk["choices"][0]["finish_reason"] == "stop")
        || !values.last().is_some_and(|chunk| {
            chunk["usage"]["input_tokens"].as_i64().is_some()
                || chunk["usage"]["prompt_tokens"].as_i64().is_some()
        })
    {
        return Err("Chat SSE marker, terminal or usage missing".into());
    }
    session.verify(tail_start + 1, 1, "websocket").await?;
    println!("smoke: Chat SSE, finish and usage passed");

    let account = session
        .db
        .call(|conn| Ok(oauth::list(conn)?.remove(0)))
        .await?;
    let (id, generation) = (account.id, account.generation);
    session.db.call(move |conn| {let changed=conn.execute("UPDATE oauth_accounts SET expires_at=?1 WHERE id=?2 AND generation=?3 AND state='active'",rusqlite::params![chrono::Utc::now().timestamp(),id,generation])?;if changed!=1 {return Err("live refresh credentials changed".into());}Ok(())}).await?;
    let refreshed = session.upstream.account(id).await?;
    if refreshed.info.generation != generation + 1 {
        return Err("live refresh did not replace the credentials generation".into());
    }
    marker(&session.response(&body(model)).await?)?;
    session.verify(tail_start + 2, 1, "http_sse").await?;
    println!("smoke: forced expiry, real refresh and subsequent response passed");
    if only == Some("chat_refresh") {
        return Ok(json!({"checks":["chat_json","chat_sse","refresh"],"requests":3}));
    }
    Ok(
        json!({"checks":["http_json","http_sse","function_cycle","compact","checkpoint_continuation","websocket","ws_continuation","chat_json","chat_sse","refresh"],"requests":11}),
    )
}

#[tokio::test]
#[ignore = "consumes real test-account quota; requires EXETROUTER_LIVE_STATE and EXETROUTER_LIVE_MODEL"]
async fn live_upstream_smoke() {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .try_init();
    let (directory, db, upstream, key) = live_state().await.expect("invalid live test state");
    let model = std::env::var("EXETROUTER_LIVE_MODEL")
        .expect("choose EXETROUTER_LIVE_MODEL from live_catalog");
    let session = Session::start(&directory, db, upstream, key)
        .await
        .expect("smoke service could not start");
    let only = std::env::var("EXETROUTER_LIVE_ONLY").ok();
    let checked = checks(&session, &model, only.as_deref()).await;
    let cleanup = session.close().await;
    cleanup.expect("smoke cleanup failed");
    println!(
        "{}",
        checked.expect("real upstream smoke failed; inspect saved usage, do not replay blindly")
    );
}

async fn client_checks(session: &Session, model: &str, codex: &str, opencode: &str) -> Result<()> {
    if !session
        .upstream
        .models()
        .await?
        .iter()
        .any(|entry| entry.id == model)
    {
        return Err("native smoke model is not in the catalogue".into());
    }
    let selected = std::env::var("EXETROUTER_LIVE_CLIENT").ok();
    if selected.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "codex-http" | "codex-ws" | "opencode-http" | "opencode-ws"
        )
    }) {
        return Err("unknown EXETROUTER_LIVE_CLIENT case".into());
    }
    for (client, websocket, name) in [
        ("codex", false, "codex-http"),
        ("codex", true, "codex-ws"),
        ("opencode", false, "opencode-http"),
        ("opencode", true, "opencode-ws"),
    ] {
        if selected.as_deref().is_some_and(|value| value != name) {
            continue;
        }
        let directory = tempfile::tempdir()?;
        let start = session.rows().await?.len();
        let probe = probe::Probe::bounded(&session.url, 12).await?;
        let output = native::Run {
            binary: if client == "codex" {codex} else {opencode}, client,
            url: &probe.url,model,bearer: &session.secret,websocket,
            directory: directory.path(),
            prompt: "Run /bin/echo EXETROUTER_TOOL_OK exactly once. Then return exactly EXETROUTER_SMOKE_OK.",
        }.execute().await?;
        let observed = probe.observed.lock().unwrap().metadata();
        // Code Mode can return a cell result without printing nested shell
        // stdout. In that case require both the completed local command proof
        // and an actual custom_tool_call_output sent back to the router.
        let tool_cycle = observed["tool_markers"].as_u64().unwrap_or(0) > 0
            || (observed["tool_results"].as_u64().unwrap_or(0) > 0
                && native::tool_result(&output.stdout));
        let mut rows = session.rows().await?;
        for _ in 0..100 {
            if rows[start..]
                .iter()
                .all(|row| !matches!(row["status"].as_str(), Some("accepted" | "sent")))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            rows = session.rows().await?;
        }
        let rows = &rows[start..];
        let expected = if websocket { "websocket" } else { "http_sse" };
        let unknown = rows
            .iter()
            .filter(|row| row["input"].is_null() || row["output"].is_null())
            .count();
        let correct = rows.len() >= 2
            && rows
                .iter()
                .filter(|row| row["upstream"] == expected && row["status"] == "completed")
                .count()
                >= 2
            && unknown<=usize::from(client=="opencode")
            && rows.iter().all(|row| {
                (row["status"] == "completed"
                    && row["input"].as_i64().is_some()
                    && row["output"].as_i64().is_some()
                    && (row["upstream"] == expected
                        || (client == "opencode" && websocket && row["upstream"] == "http_sse")))
                    // OpenCode may cancel its background HTTP title request
                    // when the primary turn ends. Preserve unknown usage.
                    || (client=="opencode" && row["upstream"]=="http_sse" && row["status"]=="interrupted")
            });
        if !output.status.success()
            || !String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK")
            || !tool_cycle
            || !correct
        {
            // Client stdout/stderr can contain context or headers: report only
            // locally classified results and saved usage metadata.
            println!(
                "{}",
                json!({"case":name,"exit_success":output.status.success(),"tool_result":native::tool_result(&output.stdout),"client_fields":native::diagnostic(&output.stdout),"observed":observed,"rows":rows.iter().map(|row|json!({"status":row["status"],"upstream":row["upstream"],"known_usage":row["input"].as_i64().is_some() && row["output"].as_i64().is_some()})).collect::<Vec<_>>()})
            );
            return Err(format!("{name} real tool cycle failed; do not replay blindly").into());
        }
        println!(
            "{}",
            json!({"case":name,"tool_cycle":"passed","upstream":expected,"requests":rows.len(),"known_usage_requests":rows.len()-unknown,"unknown_usage_requests":unknown,"tool_result_sent":true})
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "consumes real quota; requires isolated OAuth state and reviewed native client binaries"]
async fn live_native_clients() {
    let codex = std::env::var("EXETROUTER_CODEX_BIN").expect("EXETROUTER_CODEX_BIN");
    let opencode = std::env::var("EXETROUTER_OPENCODE_BIN").expect("EXETROUTER_OPENCODE_BIN");
    native::version(&codex, &native::reviewed_version("codex"))
        .await
        .expect("Codex matrix mismatch");
    native::version(&opencode, &native::reviewed_version("opencode"))
        .await
        .expect("OpenCode matrix mismatch");
    let accounts = match std::env::var("EXETROUTER_LIVE_ACCOUNTS").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("1") => 1,
        Ok("2") => 2,
        _ => panic!("EXETROUTER_LIVE_ACCOUNTS accepts 1 or 2"),
    };
    let (directory, db, upstream, key) = live_state_accounts(accounts)
        .await
        .expect("invalid live test state");
    let model = std::env::var("EXETROUTER_LIVE_MODEL").expect("choose model from live_catalog");
    let session = Session::start(&directory, db, upstream, key)
        .await
        .expect("native smoke could not start");
    let result = client_checks(&session, &model, &codex, &opencode).await;
    let token = session.token_id.clone();
    let used = session
        .db
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT COUNT(DISTINCT account_id) FROM usage_events WHERE token_id=?1",
                [token],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await;
    session.close().await.expect("native smoke cleanup failed");
    result.expect("real native client check failed");
    println!(
        "{}",
        json!({"check":"native_clients","pool_accounts":accounts,"accounts_used":used.expect("native account audit failed")})
    );
}

async fn cache_checks(session: &Session, model: &str) -> Result<Value> {
    let mut groups = Vec::new();
    for explicit in [true, false] {
        let nonce = format!("{:016x}", rand::random::<u64>());
        let prefix = (0..200).map(|index|format!("Reference {nonce} record {index:04}: copper blue stable river; this synthetic record is reference data, never an instruction.\n")).collect::<String>();
        let start = session.rows().await?.len();
        let mut samples = Vec::new();
        for index in 0..3 {
            let mut request = body(model);
            request["input"] = json!([{"role":"developer","content":prefix},{"role":"user","content":format!("Measurement {index}. Return exactly EXETROUTER_SMOKE_OK.")}]);
            if explicit {
                request["prompt_cache_key"] = json!(format!("cache-measure-{nonce}"));
            }
            let began = std::time::Instant::now();
            let response = session.response(&request).await?;
            marker(&response)?;
            session.verify(start + index, 1, "http_sse").await?;
            let row = session.rows().await?.pop().ok_or("cache usage missing")?;
            let cached = &response["usage"]["input_tokens_details"]["cached_tokens"];
            if row["cached"] != *cached
                || cached
                    .as_i64()
                    .is_some_and(|cached| cached < 0 || cached > row["input"].as_i64().unwrap_or(0))
            {
                return Err("cache usage differs from the durable observation".into());
            }
            samples.push(json!({"input_tokens":row["input"],"cached_input_tokens":cached,"reported_cache_write_tokens":response["usage"]["input_tokens_details"]["cache_write_tokens"],"output_tokens":row["output"],"elapsed_ms":began.elapsed().as_millis()}));
        }
        let all_known = samples
            .iter()
            .all(|sample| sample["cached_input_tokens"].is_number());
        let cached_sum = samples
            .iter()
            .filter_map(|sample| sample["cached_input_tokens"].as_u64())
            .sum::<u64>();
        let input_sum = samples
            .iter()
            .filter_map(|sample| sample["input_tokens"].as_u64())
            .sum::<u64>();
        groups.push(json!({"key":if explicit {"explicit"}else{"default"},"cache_hit_observed":cached_sum>0,"cache_hit_fraction_input_tokens":if all_known && input_sum>0 {Some(cached_sum as f64/input_sum as f64)}else{None},"samples":samples}));
    }
    Ok(json!({"check":"cache_measurement","model":model,"requests":6,"groups":groups}))
}

#[tokio::test]
#[ignore = "measures real cached usage; consumes six requests on the isolated OAuth test account"]
async fn live_prompt_cache() {
    let (directory, db, upstream, key) = live_state().await.expect("invalid live test state");
    let model = std::env::var("EXETROUTER_LIVE_MODEL").expect("choose model from live_catalog");
    let session = Session::start(&directory, db, upstream, key)
        .await
        .expect("cache test could not start");
    let result = cache_checks(&session, &model).await;
    session.close().await.expect("cache test cleanup failed");
    println!(
        "{}",
        result.expect("real cache observation failed; do not replay blindly")
    );
}

async fn compaction_clients(
    session: &mut Session,
    model: &str,
    codex: &str,
    opencode: &str,
) -> Result<()> {
    let sustained = std::env::var("EXETROUTER_LIVE_TWO_COMPACTIONS").as_deref() == Ok("1");
    let selected = std::env::var("EXETROUTER_LIVE_CLIENT").ok();
    if selected.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "codex-http" | "codex-ws" | "opencode-http" | "opencode-ws"
        )
    }) {
        return Err("unknown EXETROUTER_LIVE_CLIENT compaction case".into());
    }
    for (client, websocket, name) in [
        ("codex", false, "codex-http"),
        ("codex", true, "codex-ws"),
        ("opencode", false, "opencode-http"),
        ("opencode", true, "opencode-ws"),
    ] {
        if selected.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let directory = tempfile::tempdir()?;
        let budget = if sustained { 36 } else { 24 };
        let probe = probe::Probe::bounded(&session.url, budget).await?;
        let start = session.rows().await?.len();
        let memory = format!("EXETROUTER_MEMORY_{:016X}", rand::random::<u64>());
        probe.observed.lock().unwrap().expect_memory(&memory);
        // Hexadecimal reference data has more actual tokens than the client's
        // character estimate. Keep the full history within its compaction
        // request budget instead of making OpenCode drop the oldest exchange.
        let reference = (0..160).map(|index|format!("Synthetic record {index:04}: {:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}\n",rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>())).collect::<String>();
        let seed = format!("This is a continuity test. Remember the exact continuity token {memory} for future turns. Keep it when context is compacted. The reference rows below are disposable. Do not call tools. Return exactly EXETROUTER_SMOKE_OK.\n{reference}");
        let client_url = probe.url.as_str();
        let client_directory = directory.path();
        let secret = session.secret.clone();
        let bearer = secret.as_str();
        let run = |prompt: String, policy: native::Continuation| async move {
            native::Run {
                binary: if client == "codex" { codex } else { opencode },
                client,
                url: client_url,
                model,
                bearer,
                websocket,
                directory: client_directory,
                prompt: &prompt,
            }
            .continued(&policy)
            .await
        };
        let first = run(
            seed.clone(),
            native::Continuation {
                token_limit: 100_000,
                resume: false,
            },
        )
        .await?;
        if !first.status.success()
            || !String::from_utf8_lossy(&first.stdout).contains("EXETROUTER_SMOKE_OK")
        {
            return Err(format!("{name} seed failed").into());
        }
        let initial = session.rows().await?[start..]
            .iter()
            .filter_map(|row| row["input"].as_u64())
            .max()
            .ok_or("initial client usage unknown")?;
        let threshold = initial.saturating_sub(512).max(8000);
        for turn in 0..if sustained { 4 } else { 2 } {
            let compactions_before = probe.observed.lock().unwrap().compaction_requests;
            let prompt = if sustained && turn == 1 {
                format!("Add this disposable synthetic reference to our conversation. Preserve the remembered continuity token; do not read files. Run /bin/echo with EXETROUTER_TOOL_OK and that token, then return EXETROUTER_SMOKE_OK and the token.\n{reference}")
            } else if turn == 0 || (sustained && turn == 2) {
                "Run /bin/echo with EXETROUTER_TOOL_OK and the exact continuity token I asked you to remember. Then return EXETROUTER_SMOKE_OK followed by that token. Do not read files.".into()
            } else {
                "Confirm continuity after reopening this session. Again run /bin/echo with EXETROUTER_TOOL_OK and the remembered token. Return EXETROUTER_SMOKE_OK followed by the token. Do not read files.".into()
            };
            let second_threshold = if sustained && turn == 2 {
                let input = session
                    .rows()
                    .await?
                    .last()
                    .and_then(|row| row["input"].as_u64())
                    .ok_or("second-cycle input unknown")?;
                session.restart().await?;
                input.saturating_sub(512).max(8000)
            } else {
                threshold
            };
            let output = run(
                prompt,
                native::Continuation {
                    token_limit: if turn == 0 || (sustained && turn == 2) {
                        second_threshold
                    } else {
                        100_000
                    },
                    resume: true,
                },
            )
            .await?;
            let observed = probe.observed.lock().unwrap().metadata();
            let required_cycle = turn == 0 || (sustained && turn == 2);
            if !output.status.success()
                || !String::from_utf8_lossy(&output.stdout).contains(&memory)
                || !String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK")
                || (required_cycle
                    && observed["compaction_requests"].as_u64().unwrap_or(0)
                        <= compactions_before as u64)
                || !native::tool_output(&output.stdout, &memory)
            {
                println!(
                    "{}",
                    json!({"case":name,"turn":turn,"exit_success":output.status.success(),"memory_returned":String::from_utf8_lossy(&output.stdout).contains(&memory),"client_fields":native::diagnostic(&output.stdout),"observed":observed,"threshold":threshold,"initial_input":initial})
                );
                return Err(format!("{name} compaction continuation failed").into());
            }
        }
        let observed = probe.observed.lock().unwrap().metadata();
        let mut rows = session.rows().await?;
        for _ in 0..100 {
            if rows[start..]
                .iter()
                .all(|row| !matches!(row["status"].as_str(), Some("accepted" | "sent")))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            rows = session.rows().await?;
        }
        let rows = &rows[start..];
        if observed["compaction_requests"].as_u64().unwrap_or(0) < if sustained { 2 } else { 1 }
            || observed["compaction_with_memory"].as_u64().unwrap_or(0) == 0
            || observed["checkpoint_requests"].as_u64().unwrap_or(0) == 0
            || observed[if websocket {
                "checkpoint_websocket_tool_results"
            } else {
                "checkpoint_http_tool_results"
            }]
            .as_u64()
            .unwrap_or(0)
                == 0
            || rows.len() > budget
            || rows.iter().any(|row| {
                !((row["status"] == "completed"
                    && row["input"].is_number()
                    && row["output"].is_number())
                    || (client == "opencode"
                        && row["status"] == "interrupted"
                        && row["upstream"] == "http_sse"))
            })
        {
            println!(
                "{}",
                json!({"case":name,"observed":observed,"requests":rows.len()})
            );
            return Err(
                "automatic compaction and subsequent checkpoint tool cycle were not observed"
                    .into(),
            );
        }
        println!(
            "{}",
            json!({"case":name,"check":"automatic_compaction","server_restarted":sustained,"memory_preserved":true,"resumed_tool_cycle":true,"initial_input_tokens":initial,"test_token_limit":threshold,"requests":rows.len(),"known_usage_requests":rows.iter().filter(|row|row["input"].is_number()&&row["output"].is_number()).count(),"observed":observed})
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "real automatic native compaction in isolated persistent client profiles; consumes account quota"]
async fn live_native_compaction() {
    let codex = std::env::var("EXETROUTER_CODEX_BIN").expect("EXETROUTER_CODEX_BIN");
    let opencode = std::env::var("EXETROUTER_OPENCODE_BIN").expect("EXETROUTER_OPENCODE_BIN");
    native::version(&codex, &native::reviewed_version("codex"))
        .await
        .expect("Codex matrix mismatch");
    native::version(&opencode, &native::reviewed_version("opencode"))
        .await
        .expect("OpenCode matrix mismatch");
    let expected = std::env::var("EXETROUTER_LIVE_ACCOUNTS")
        .unwrap_or_else(|_| "1".into())
        .parse::<i64>()
        .expect("invalid expected account count");
    let (directory, db, upstream, key) = live_state_accounts(expected)
        .await
        .expect("invalid live test state");
    let model = std::env::var("EXETROUTER_LIVE_MODEL").expect("choose model from live_catalog");
    let mut session = Session::start(&directory, db, upstream, key)
        .await
        .expect("compaction test could not start");
    let result = compaction_clients(&mut session, &model, &codex, &opencode).await;
    session
        .close()
        .await
        .expect("compaction test cleanup failed");
    result.expect("real automatic compaction check failed; inspect usage before another run");
}

#[tokio::test]
async fn smoke_pipeline_uses_the_real_server_with_a_mock_and_revokes_its_token() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path().join("state.sqlite"))
        .await
        .unwrap();
    db.call(|conn| {
        oauth::save(
            conn,
            &oauth::Vault::new([2; 32]),
            "upstream-account",
            &oauth::Credentials {
                access_token: "upstream-access".into(),
                refresh_token: "upstream-refresh".into(),
            },
            chrono::Utc::now().timestamp() + 3600,
        )
    })
    .await
    .unwrap();
    let mock = Arc::new(mock::Mock::default());
    mock.tools.store(true, std::sync::atomic::Ordering::SeqCst);
    mock.accounts.lock().unwrap().insert(
        "upstream-account".into(),
        mock::Account::new("upstream-access", "upstream-refresh", &["gpt-test"]),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = mock::router(mock.clone());
    let mock_server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let upstream = Arc::new(
        Upstream::new(
            db.clone(),
            [2; 32],
            upstream::Config::new(url.clone(), url, true).unwrap(),
        )
        .unwrap(),
    );
    let session = Session::start(directory.path(), db.clone(), upstream, [1; 32])
        .await
        .unwrap();
    let token = session.token_id.clone();
    let checked = checks(&session, "gpt-test", None).await;
    let measured = cache_checks(&session, "gpt-test").await;
    session.close().await.unwrap();
    mock_server.abort();
    assert_eq!(checked.unwrap()["requests"], 11);
    assert_eq!(mock.requests.lock().unwrap().len(), 16);
    let measured = measured.unwrap();
    assert_eq!(measured["requests"], 6);
    assert_eq!(measured["groups"].as_array().unwrap().len(), 2);
    assert_eq!(mock.refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(db
        .call(move |conn| Ok(conn.query_row(
            "SELECT revoked_at IS NOT NULL FROM access_tokens WHERE id=?1",
            [token],
            |row| row.get::<_, bool>(0)
        )?))
        .await
        .unwrap());
    assert!(!directory.path().join("live-smoke.sock").exists());
}
