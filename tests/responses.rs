#[path = "support/mock.rs"]
mod mock;
#[path = "support/native.rs"]
mod native;
#[path = "support/probe.rs"]
mod probe;
use exetrouter::{
    oauth::{self, Credentials, Vault},
    server::{self, ServeConfig},
    store::Database,
    upstream::{self, Upstream},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

struct Fixture {
    dir: TempDir,
    db: Database,
    upstream: Arc<Upstream>,
    mock: Arc<mock::Mock>,
    url: String,
    issuer: String,
    secret: String,
    token_id: String,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<exetrouter::Result<()>>,
    mock_server: JoinHandle<()>,
}
impl Fixture {
    async fn expire_health(&self) {
        self.db
            .call(|conn| {
                conn.execute("UPDATE oauth_health SET retry_at=0", [])?;
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn add_account(&self, models: &[&str], expired: bool) -> i64 {
        self.mock.accounts.lock().unwrap().insert(
            "upstream-account-2".into(),
            mock::Account::new("upstream-access-2", "upstream-refresh-2", models),
        );
        self.db
            .call(move |conn| {
                oauth::save(
                    conn,
                    &Vault::new([2; 32]),
                    "upstream-account-2",
                    &Credentials {
                        access_token: "upstream-access-2".into(),
                        refresh_token: "upstream-refresh-2".into(),
                    },
                    chrono::Utc::now().timestamp() + if expired { -1 } else { 3600 },
                )
            })
            .await
            .unwrap()
    }
    fn first_profile(&self, models: &[&str], mode: &str) {
        let mut profile = mock::Account::new("upstream-access", "upstream-refresh", models);
        profile.mode = mode.into();
        self.mock
            .accounts
            .lock()
            .unwrap()
            .insert("upstream-account".into(), profile);
    }
    async fn start(expired: bool) -> Self {
        Self::with_limits(expired, 8).await
    }
    async fn with_limits(expired: bool, limit: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("state.sqlite"))
            .await
            .unwrap();
        let (secret, token_id) = db
            .call(move |conn| {
                let user = exetrouter::create_user(conn, "alice")?;
                let token = exetrouter::create_token(conn, &[1; 32], user, "test", 1)?;
                oauth::save(
                    conn,
                    &Vault::new([2; 32]),
                    "upstream-account",
                    &Credentials {
                        access_token: "upstream-access".into(),
                        refresh_token: "upstream-refresh".into(),
                    },
                    chrono::Utc::now().timestamp() + if expired { -1 } else { 3600 },
                )?;
                Ok((token.secret, token.token.id))
            })
            .await
            .unwrap();
        let mock = Arc::new(mock::Mock::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let app = mock::router(mock.clone());
        let mock_server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = Arc::new(
            Upstream::new(
                db.clone(),
                [2; 32],
                upstream::Config::new(issuer.clone(), issuer.clone(), true).unwrap(),
            )
            .unwrap(),
        );
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let url = format!("http://{address}");
        let (stop, stopped) = oneshot::channel();
        let serve_db = db.clone();
        let config = ServeConfig {
            gateway_uid: unsafe { libc::geteuid() },
            listen: address,
            socket: dir.path().join("control.sock"),
            upstream: Some(upstream.clone()),
            generations_per_user: limit,
            websockets_per_user: limit,
        };
        let server = tokio::spawn(server::serve(serve_db, vec![1; 32], config, async {
            let _ = stopped.await;
        }));
        for _ in 0..100 {
            if reqwest::get(format!("{url}/healthz")).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            dir,
            db,
            upstream,
            mock,
            url,
            issuer,
            secret,
            token_id,
            stop: Some(stop),
            server,
            mock_server,
        }
    }
    async fn post(&self, value: Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}/v1/responses", self.url))
            .bearer_auth(&self.secret)
            .json(&value)
            .send()
            .await
            .unwrap()
    }
    async fn chat(&self, value: Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(&self.secret)
            .json(&value)
            .send()
            .await
            .unwrap()
    }
    async fn compact(&self, value: Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}/v1/responses/compact", self.url))
            .bearer_auth(&self.secret)
            .json(&value)
            .send()
            .await
            .unwrap()
    }
    async fn restart(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), &mut self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        self.db = Database::open(self.dir.path().join("state.sqlite"))
            .await
            .unwrap();
        self.upstream = Arc::new(
            Upstream::new(
                self.db.clone(),
                [2; 32],
                upstream::Config::new(self.issuer.clone(), self.issuer.clone(), true).unwrap(),
            )
            .unwrap(),
        );
        let (stop, stopped) = oneshot::channel();
        self.stop = Some(stop);
        let config = ServeConfig {
            gateway_uid: unsafe { libc::geteuid() },
            listen: self.url.strip_prefix("http://").unwrap().parse().unwrap(),
            socket: self.dir.path().join("control.sock"),
            upstream: Some(self.upstream.clone()),
            generations_per_user: 8,
            websockets_per_user: 8,
        };
        self.server = tokio::spawn(server::serve(self.db.clone(), vec![1; 32], config, async {
            let _ = stopped.await;
        }));
        for _ in 0..100 {
            if reqwest::get(format!("{}/healthz", self.url)).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("restarted server did not become ready");
    }
    async fn rows(&self) -> Vec<Value> {
        self.db.call(|conn| {
            let mut stmt=conn.prepare("SELECT request_id,status,input_tokens,output_tokens,cached_input_tokens,reasoning_output_tokens,upstream_transport,upstream_response_id,api_surface,client_transport,user_id,token_id FROM usage_events ORDER BY id")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,Option<String>>(0)?,"status":r.get::<_,String>(1)?,"input":r.get::<_,Option<i64>>(2)?,"output":r.get::<_,Option<i64>>(3)?,"cached":r.get::<_,Option<i64>>(4)?,"reasoning":r.get::<_,Option<i64>>(5)?,"transport":r.get::<_,Option<String>>(6)?,"response":r.get::<_,Option<String>>(7)?,"surface":r.get::<_,String>(8)?,"client_transport":r.get::<_,Option<String>>(9)?,"user":r.get::<_,i64>(10)?,"token":r.get::<_,String>(11)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;Ok(rows)
        }).await.unwrap()
    }
    async fn ws(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut request = format!("{}/v1/responses", self.url.replace("http://", "ws://"))
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.secret).parse().unwrap(),
        );
        connect_async(request).await.unwrap().0
    }
    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), &mut self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!self.dir.path().join("control.sock").exists());
        self.mock_server.abort();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        self.mock_server.abort();
    }
}

fn request() -> Value {
    json!({"model":"gpt-test","input":[{"role":"user","content":"synthetic fixture"}],"store":false,"stream":true,"tools":[{"type":"function","name":"fixture","parameters":{"type":"object"}}]})
}

#[tokio::test]
async fn compressed_native_bodies_are_decoded_once_and_invalid_frames_never_submit() {
    let fixture = Fixture::start(false).await;
    let client = reqwest::Client::new();
    let mut body = request();
    body["stream"] = json!(false);
    body["input"][0]["content"] = json!("synthetic-compressed-marker ".repeat(90000));
    let frame = zstd::stream::encode_all(&serde_json::to_vec(&body).unwrap()[..], 3).unwrap();
    let submit = |bytes: Vec<u8>, coding: &str, authorized: bool| {
        client
            .post(format!("{}/v1/responses", fixture.url))
            .bearer_auth(if authorized {
                &fixture.secret
            } else {
                "invalid"
            })
            .header("content-type", "application/json")
            .header("content-encoding", coding)
            .body(bytes)
            .send()
    };
    let response = submit(frame.clone(), "zstd", true).await.unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();
    assert_eq!(
        fixture.mock.requests.lock().unwrap()[0]["input"],
        body["input"]
    );
    let bomb = zstd::stream::encode_all(&vec![b' '; 16 * 1024 * 1024 + 1][..], 3).unwrap();
    for (bytes, coding, status) in [
        (frame[..frame.len() - 1].to_vec(), "zstd", 400),
        (b"private-invalid-compression".to_vec(), "zstd", 400),
        (bomb, "zstd", 413),
        (vec![0; 16 * 1024 * 1024 + 1], "zstd", 413),
        (frame.clone(), "gzip", 415),
        (frame.clone(), "zstd, identity", 415),
    ] {
        let response = submit(bytes, coding, true).await.unwrap();
        assert_eq!(response.status(), status);
        let error = response.text().await.unwrap();
        assert!(!error.contains("private-invalid-compression"));
        assert!(!error.contains("synthetic-compressed-marker"));
    }
    assert_eq!(
        submit(frame, "unsupported", false).await.unwrap().status(),
        401
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.rows().await.len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn shared_cache_threads_are_distinct_scoped_and_stable_across_restart_and_transports() {
    let mut fixture = Fixture::start(false).await;
    let mut body = request();
    body["stream"] = json!(false);
    body["prompt_cache_key"] = json!("shared-parent-cache");
    let client = reqwest::Client::new();
    for thread in ["parent-thread", "child-thread", "parent-thread"] {
        let response = client
            .post(format!("{}/v1/responses", fixture.url))
            .bearer_auth(&fixture.secret)
            .header("thread-id", thread)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.bytes().await.unwrap();
    }
    let threads = fixture.mock.threads.lock().unwrap().clone();
    let sessions = fixture.mock.sessions.lock().unwrap().clone();
    assert_ne!(threads[0], threads[1]);
    assert_eq!(threads[0], threads[2]);
    assert!(sessions.iter().all(|s| s.1 == sessions[0].1));
    let token = fixture.token_id.clone();
    let (rotated, foreign) = fixture
        .db
        .call(move |conn| {
            let rotated = exetrouter::rotate_token(conn, &[1; 32], 1, &token)?.secret;
            let user = exetrouter::create_user(conn, "other-thread-user")?;
            Ok((
                rotated,
                exetrouter::create_token(conn, &[1; 32], user, "fixture", 1)?.secret,
            ))
        })
        .await
        .unwrap();
    fixture.secret = rotated;
    fixture.restart().await;
    body["type"] = json!("response.create");
    body["client_metadata"] = json!({"thread_id":"child-thread","session_id":"child-thread","x-codex-turn-metadata":json!({"thread_id":"child-thread","session_id":"child-thread","request_kind":"model","tool_namespaces_info":{"namespaces":[{"name":"functions","tools":["exec"]}]}}).to_string()});
    let mut socket = fixture.ws().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    loop {
        let event: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if event["type"] == "response.completed" {
            break;
        }
    }
    assert_eq!(
        fixture.mock.threads.lock().unwrap().last().unwrap(),
        &threads[1]
    );
    let forwarded = fixture
        .mock
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(forwarded["client_metadata"]["thread_id"], threads[1]);
    assert_eq!(
        forwarded["client_metadata"]["session_id"],
        fixture.mock.sessions.lock().unwrap().last().unwrap().1
    );
    assert_ne!(forwarded["client_metadata"]["session_id"], sessions[0].1);
    let turn: Value = serde_json::from_str(
        forwarded["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(turn["thread_id"], threads[1]);
    assert_eq!(
        turn["session_id"],
        forwarded["client_metadata"]["session_id"]
    );
    assert_eq!(
        turn["tool_namespaces_info"]["namespaces"][0]["tools"][0],
        "exec"
    );
    assert_eq!(
        forwarded["prompt_cache_key"],
        fixture.mock.cache_keys.lock().unwrap()[0]
    );
    body["client_metadata"]["thread_id"] = json!("parent-thread");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let event: Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(event["error"]["code"], "thread_changed");
    socket.close(None).await.unwrap();
    let response = client
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(foreign)
        .header("thread-id", "parent-thread")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();
    assert_ne!(
        fixture.mock.threads.lock().unwrap().last().unwrap(),
        &threads[0]
    );
    fixture.stop().await;
}

#[tokio::test]
async fn ws_turn_metadata_pins_http_and_new_ws_continuations_without_leaking_headers() {
    let mut fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    *fixture.mock.mode.lock().unwrap() = "turn_state".into();
    {
        let mut headers = fixture.mock.quota_headers.lock().unwrap();
        headers.insert("openai-model", "gpt-test".parse().unwrap());
        headers.insert("x-reasoning-included", "true".parse().unwrap());
        headers.insert("authorization", "private-upstream-header".parse().unwrap());
        headers.insert("x-models-etag", "x".repeat(129).parse().unwrap());
    }
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let created: Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(created["type"], "response.created");
    let metadata: Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(metadata["type"], "response.metadata");
    let state = metadata["headers"]["x-codex-turn-state"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(metadata["headers"]["openai-model"], "gpt-test");
    assert!(metadata["headers"].get("authorization").is_none());
    assert!(metadata["headers"].get("x-models-etag").is_none());
    loop {
        let event: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if event["type"] == "response.completed" {
            break;
        }
    }
    socket.close(None).await.unwrap();
    fixture.restart().await;
    body["stream"] = json!(false);
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(&fixture.secret)
        .header("x-codex-turn-state", &state)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["openai-model"], "gpt-test");
    assert!(!response.headers().contains_key("authorization"));
    response.bytes().await.unwrap();
    let mut socket = fixture.ws().await;
    body["client_metadata"] = json!({"x-codex-turn-state":state});
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    loop {
        let event: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if event["type"] == "response.completed" {
            break;
        }
    }
    body["client_metadata"]["x-codex-turn-state"] = json!("unknown-turn-state");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let error: Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(error["error"]["code"], "context_not_found");
    assert_eq!(fixture.mock.request_accounts.lock().unwrap().len(), 3);
    socket.close(None).await.unwrap();
    let mut upgrade = format!("{}/v1/responses", fixture.url.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    upgrade.headers_mut().insert(
        "authorization",
        format!("Bearer {}", fixture.secret).parse().unwrap(),
    );
    upgrade
        .headers_mut()
        .insert("x-codex-turn-state", state.parse().unwrap());
    let (mut socket, _) = connect_async(upgrade).await.unwrap();
    body.as_object_mut().unwrap().remove("client_metadata");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    loop {
        let event: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if event["type"] == "response.completed" {
            break;
        }
    }
    assert_eq!(fixture.mock.request_accounts.lock().unwrap().len(), 4);
    assert_eq!(
        fixture.mock.requests.lock().unwrap().last().unwrap()["client_metadata"]
            ["x-codex-turn-state"],
        state
    );
    let accounts = fixture.mock.request_accounts.lock().unwrap().clone();
    assert!(accounts.iter().all(|account| account == &accounts[0]));
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn backend_options_are_forwarded_over_http_and_websocket() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let options = json!({"max_output_tokens":64,"stream_id":"parallel","background":true,
        "conversation":null,"context_management":[{"type":"compaction","compact_threshold":100000}],
        "future_option":{"enabled":true},"store":true});
    let mut body = request();
    for (field, value) in options.as_object().unwrap() {
        body[field] = value.clone();
    }
    assert_eq!(fixture.post(body.clone()).await.status(), 200);
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    loop {
        let event: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if event["type"] == "response.completed" {
            break;
        }
    }
    let sent = fixture.mock.requests.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    for request in sent {
        for (field, value) in options.as_object().unwrap() {
            assert_eq!(&request[field], value, "{field}");
        }
    }
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
#[ignore = "requires the reviewed isolated Codex/OpenCode runtimes; no real upstream"]
async fn codex_does_not_replay_submitted_ws_interruptions() {
    for client in ["codex"] {
        let binary = std::env::var(if client == "codex" {
            "EXETROUTER_CODEX_BIN"
        } else {
            "EXETROUTER_OPENCODE_BIN"
        })
        .unwrap();
        native::version(&binary, &native::reviewed_version(client))
            .await
            .unwrap();
        for mode in ["early_close", "disconnect", "missing_terminal"] {
            let fixture = Fixture::start(false).await;
            *fixture.mock.mode.lock().unwrap() = mode.into();
            fixture.mock.tools.store(true, Ordering::SeqCst);
            let probe = probe::Probe::bounded(&fixture.url, 12).await.unwrap();
            let output = native::Run {
                binary: &binary,
                client,
                url: &probe.url,
                model: "gpt-test",
                bearer: &fixture.secret,
                websocket: true,
                directory: fixture.dir.path(),
                prompt: "Run the local compatibility fixture and report the result.",
            }
            .execute()
            .await
            .unwrap();
            if mode != "missing_terminal" {
                assert!(!native::tool_result(&output.stdout));
            }
            // Native clients may execute a completed tool item before the response
            // terminal arrives. Assert no duplicate generation, not no local work.
            let calls = fixture.mock.requests.lock().unwrap().clone();
            let primary = calls
                .iter()
                .filter(|call| {
                    call["generate"] != false
                        && call
                            .get("tools")
                            .and_then(Value::as_array)
                            .is_some_and(|tools| !tools.is_empty())
                })
                .collect::<Vec<_>>();
            assert_eq!(primary.len(), 1, "{client} {mode}: expected exactly one submission; requests={}, diagnostic={}, stderr={}", calls.len(), native::diagnostic(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert_eq!(primary[0]["type"], "response.create");
            let rows = fixture.rows().await;
            assert!(rows
                .iter()
                .any(|row| row["status"] == "interrupted" && row["input"].is_null()));
            println!("{client} {mode}: one submitted generation, no HTTP replay");
            fixture.stop().await;
        }
    }
}

#[tokio::test]
#[ignore = "requires the reviewed isolated Codex runtime; no real upstream"]
async fn codex_completes_ws_tool_cycles_with_idle_ping_and_idle_reconnection() {
    let binary = std::env::var("EXETROUTER_CODEX_BIN").unwrap();
    native::version(&binary, &native::reviewed_version("codex"))
        .await
        .unwrap();
    for mode in ["idle_ping", "idle_close"] {
        let fixture = Fixture::start(false).await;
        *fixture.mock.mode.lock().unwrap() = mode.into();
        fixture.mock.tools.store(true, Ordering::SeqCst);
        let probe = probe::Probe::bounded(&fixture.url, 12).await.unwrap();
        let output = native::Run {
            binary: &binary,
            client: "codex",
            url: &probe.url,
            model: "gpt-test",
            bearer: &fixture.secret,
            websocket: true,
            directory: fixture.dir.path(),
            prompt: "Run the local compatibility fixture and report the result.",
        }
        .execute()
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            native::diagnostic(&output.stdout)
        );
        assert!(native::tool_result(&output.stdout), "{mode}");
        let calls = fixture.mock.requests.lock().unwrap().clone();
        assert!(calls.iter().all(|call| call["type"] == "response.create"));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call["generate"] != false)
                .count(),
            2
        );
        assert!(calls.last().unwrap()["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output"));
        assert!(fixture
            .rows()
            .await
            .iter()
            .all(|row| row["status"] == "completed"));
        let handshakes = fixture.mock.handshakes.lock().unwrap().clone();
        assert!(handshakes
            .iter()
            .all(|account| account == "upstream-account"));
        if mode == "idle_ping" {
            assert_eq!(handshakes.len(), 1);
            assert_eq!(fixture.mock.idle_closes.load(Ordering::SeqCst), 0);
            assert!(fixture.mock.idle_pongs.load(Ordering::SeqCst) >= 2);
        } else {
            assert_eq!(handshakes.len(), calls.len());
            assert!(calls
                .iter()
                .all(|call| call.get("previous_response_id").is_none()));
        }
        println!("Codex {mode}: WS tool cycle completed, two generations, no HTTP fallback");
        fixture.stop().await;
    }
}

#[tokio::test]
async fn large_histories_reach_upstream_over_http_compact_chat_and_websocket() {
    let fixture = Fixture::start(false).await;
    let history = "synthetic-long-history ".repeat(150_000);
    let mut body = request();
    body["input"][0]["content"] = json!(history);
    assert!(body.to_string().len() > 2 * 1024 * 1024);

    let response = fixture.post(body.clone()).await;
    assert_eq!(response.status(), 200);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    assert_eq!(
        fixture.mock.requests.lock().unwrap()[0]["input"][0]["content"],
        history
    );

    let compact = fixture.compact(body.clone()).await;
    assert_eq!(compact.status(), 200);
    assert_eq!(
        compact.json::<Value>().await.unwrap()["object"],
        "response.compaction"
    );
    assert_eq!(
        fixture.mock.compactions.lock().unwrap()[0].1["input"][0]["content"],
        history
    );

    let mut chat = chat_request(false);
    chat["messages"][0]["content"] = json!(history);
    let response = fixture.chat(chat).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["object"],
        "chat.completion"
    );

    let mut socket = fixture.ws().await;
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(
        fixture.mock.requests.lock().unwrap().last().unwrap()["input"][0]["content"],
        history
    );
    socket.close(None).await.unwrap();
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|row| row["status"] == "completed"));
    fixture.stop().await;
}

#[tokio::test]
async fn oversized_websocket_history_closes_before_submission() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    body["input"][0]["content"] = json!("x".repeat(16 * 1024 * 1024));
    // A rejected frame may close the socket while the client is still writing it.
    let _ = socket.send(Message::Text(body.to_string().into())).await;
    loop {
        let received = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap();
        match received {
            None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            other => panic!("unexpected response to oversized message: {other:?}"),
        }
    }
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn oversized_and_malformed_http_requests_never_submit_or_record_usage() {
    let fixture = Fixture::start(false).await;
    let large = json!({"model":"gpt-test","input":"x".repeat(16 * 1024 * 1024)}).to_string();
    for path in ["responses", "responses/compact", "chat/completions"] {
        let response = reqwest::Client::new()
            .post(format!("{}/v1/{path}", fixture.url))
            .bearer_auth(&fixture.secret)
            .header("content-type", "application/json")
            .body(large.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 413);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["type"],
            "request_too_large"
        );
    }
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(&fixture.secret)
        .header("content-type", "application/json")
        .body("{private-synthetic-malformed-marker")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let error = response.text().await.unwrap();
    assert!(error.contains("invalid JSON request body"));
    assert!(!error.contains("private-synthetic-malformed-marker"));
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    assert!(fixture.mock.compactions.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
    fixture.stop().await;
}

fn chat_request(stream: bool) -> Value {
    json!({"model":"gpt-test","messages":[{"role":"user","content":"synthetic SDK fixture"}],"stream":stream})
}

#[tokio::test]
async fn catalog_backoff_keeps_cached_healthy_inference_independent_of_slow_accounts() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "");
    fixture.add_account(&["gpt-test"], false).await;
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account-2")
        .unwrap()
        .catalog_error = true;
    assert_eq!(fixture.upstream.models().await.unwrap().len(), 1);
    assert_eq!(fixture.mock.catalog_requests.lock().unwrap().len(), 2);
    fixture.upstream.models().await.unwrap();
    assert_eq!(fixture.mock.catalog_requests.lock().unwrap().len(), 2);
    // Even after the failed account becomes eligible for probing, a request
    // with a fresh healthy catalogue must not wait on that account's discovery.
    fixture.expire_health().await;
    {
        let mut accounts = fixture.mock.accounts.lock().unwrap();
        let other = accounts.get_mut("upstream-account-2").unwrap();
        other.catalog_error = false;
        other.catalog_wait = true;
    }
    let reply = tokio::time::timeout(Duration::from_secs(1), fixture.post(request()))
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    assert!(reply.text().await.unwrap().contains("response.completed"));
    assert_eq!(fixture.mock.catalog_requests.lock().unwrap().len(), 2);
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account"]
    );
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn transient_refresh_is_coalesced_and_backoff_survives_restart() {
    let mut fixture = Fixture::start(true).await;
    fixture.first_profile(&["gpt-test"], "");
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .refresh_unavailable = true;
    let other_worker = Database::open(fixture.dir.path().join("state.sqlite"))
        .await
        .unwrap();
    // Bypass the catalogue lock and contend through independent SQLite workers.
    let results = futures_util::future::join_all((0..8).map(|index| {
        let db = if index % 2 == 0 {
            fixture.db.clone()
        } else {
            other_worker.clone()
        };
        let issuer = fixture.issuer.clone();
        async move {
            oauth::access(
                &db,
                &Vault::new([2; 32]),
                &reqwest::Client::new(),
                &issuer,
                1,
            )
            .await
        }
    }))
    .await;
    assert!(results.iter().all(Result::is_err));
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    let info = fixture
        .db
        .call(|conn| Ok(oauth::list(conn)?.remove(0)))
        .await
        .unwrap();
    assert_eq!(info.state, "active");
    assert_eq!(info.health_reason.as_deref(), Some("refresh"));
    assert!(info.health_until.unwrap() >= chrono::Utc::now().timestamp() + 15);
    fixture.restart().await;
    assert!(fixture.upstream.models().await.is_err());
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert!(fixture.rows().await.is_empty());
    fixture.expire_health().await;
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .refresh_unavailable = false;
    assert!(fixture
        .post(request())
        .await
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 2);
    let info = fixture
        .db
        .call(|conn| Ok(oauth::list(conn)?.remove(0)))
        .await
        .unwrap();
    assert_eq!(info.generation, 1);
    assert!(info.health_until.is_none());
    fixture.stop().await;
}

#[tokio::test]
async fn response_backoff_is_durable_and_checkpoint_remains_on_its_original_account() {
    let mut fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "");
    fixture.add_account(&["gpt-test"], false).await;
    fixture.upstream.models().await.unwrap();
    let checkpoint = fixture
        .compact(json!({"model":"gpt-test","input":[]}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    let mut pinned = request();
    pinned["input"] = checkpoint["output"].clone();
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .mode = "upstream_503".into();
    let rejected = fixture.post(pinned.clone()).await;
    assert_eq!(rejected.status(), 503);
    assert!(!rejected
        .text()
        .await
        .unwrap()
        .contains("private-upstream-secret"));
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account"]
    );
    assert!(fixture
        .post(request())
        .await
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account", "upstream-account-2"]
    );
    for restart in [false, true] {
        if restart {
            fixture.restart().await;
        }
        let reply = fixture.post(pinned.clone()).await;
        assert_eq!(reply.status(), 503);
        assert!(
            reply.headers()["retry-after"]
                .to_str()
                .unwrap()
                .parse::<u32>()
                .unwrap()
                > 0
        );
        assert_eq!(
            reply.json::<Value>().await.unwrap()["error"]["code"],
            "upstream_backoff"
        );
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
        assert_eq!(fixture.rows().await.len(), 3); // compaction plus two actual requests
    }
    fixture.expire_health().await;
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .mode
        .clear();
    assert!(fixture
        .post(pinned)
        .await
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    let info = fixture
        .db
        .call(|conn| Ok(oauth::list(conn)?.remove(0)))
        .await
        .unwrap();
    assert!(info.health_until.is_none());
    assert_eq!(
        fixture
            .mock
            .request_accounts
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        "upstream-account"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn native_ws_temporary_failure_preserves_history_without_replay_or_moving_accounts() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "");
    fixture.add_account(&["gpt-test"], false).await;
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let first = terminal(&mut socket).await;
    body["previous_response_id"] = first["response"]["id"].clone();
    body["test_mode"] = json!("server_error");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["status"], 503);
    body.as_object_mut().unwrap().remove("test_mode");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let paused = terminal(&mut socket).await;
    assert_eq!(paused["error"]["code"], "upstream_backoff");
    assert!(paused["error"]["retry_after"].as_i64().unwrap() >= 15);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert!(fixture
        .post(request())
        .await
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    fixture.expire_health().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        [
            "upstream-account",
            "upstream-account",
            "upstream-account-2",
            "upstream-account"
        ]
    );
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn unauthorized_is_sanitized_and_refresh_waits_for_a_new_request_on_all_transports() {
    for surface in [
        "responses_json",
        "responses_sse",
        "chat_json",
        "chat_sse",
        "native_ws",
    ] {
        let fixture = Fixture::start(false).await;
        fixture.first_profile(&["gpt-test"], "unauthorized");
        let error = if surface == "native_ws" {
            let mut socket = fixture.ws().await;
            let mut body = request();
            body["type"] = json!("response.create");
            socket
                .send(Message::Text(body.to_string().into()))
                .await
                .unwrap();
            let error = terminal(&mut socket).await.to_string();
            let close = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap();
            assert!(matches!(close, Some(Ok(Message::Close(_)))));
            error
        } else {
            let reply = if surface.starts_with("chat") {
                fixture.chat(chat_request(surface == "chat_sse")).await
            } else {
                let mut body = request();
                body["stream"] = json!(surface == "responses_sse");
                fixture.post(body).await
            };
            assert_eq!(
                reply.status(),
                if surface == "chat_sse" { 200 } else { 502 }
            );
            reply.text().await.unwrap()
        };
        assert!(
            error.contains("upstream_authentication_error"),
            "{surface}: {error}"
        );
        assert!(!error.contains("private-upstream-secret"));
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
        fixture
            .mock
            .accounts
            .lock()
            .unwrap()
            .get_mut("upstream-account")
            .unwrap()
            .mode
            .clear();
        assert!(fixture
            .post(request())
            .await
            .text()
            .await
            .unwrap()
            .contains("response.completed"));
        assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
        assert_eq!(
            fixture.rows().await[0]["status"],
            if surface.starts_with("responses") {
                "upstream_rejected"
            } else {
                "failed"
            }
        );
        // Success resets the auth rejection streak, so a later unrelated 401
        // schedules another refresh rather than permanently disabling access.
        fixture
            .mock
            .accounts
            .lock()
            .unwrap()
            .get_mut("upstream-account")
            .unwrap()
            .mode = "unauthorized".into();
        assert_eq!(fixture.post(request()).await.status(), 502);
        fixture
            .mock
            .accounts
            .lock()
            .unwrap()
            .get_mut("upstream-account")
            .unwrap()
            .mode
            .clear();
        assert_eq!(fixture.post(request()).await.status(), 200);
        assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 2);
        assert_eq!(
            fixture
                .db
                .call(|conn| Ok(oauth::list(conn)?.remove(0).state))
                .await
                .unwrap(),
            "active"
        );
        fixture.stop().await;
    }
}

#[tokio::test]
async fn repeated_unauthorized_after_refresh_requires_reauth_and_operator_save_recovers() {
    let mut fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "unauthorized");
    assert_eq!(fixture.post(request()).await.status(), 502);
    fixture.restart().await;
    assert_eq!(fixture.post(request()).await.status(), 502);
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert_eq!(
        fixture
            .db
            .call(|conn| Ok(oauth::list(conn)?.remove(0).state))
            .await
            .unwrap(),
        "reauth_required"
    );
    assert_eq!(fixture.post(request()).await.status(), 503);
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture
        .db
        .call(|conn| {
            oauth::save(
                conn,
                &Vault::new([2; 32]),
                "upstream-account",
                &Credentials {
                    access_token: "upstream-access".into(),
                    refresh_token: "upstream-refresh".into(),
                },
                chrono::Utc::now().timestamp() + 3600,
            )
        })
        .await
        .unwrap();
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .mode
        .clear();
    assert!(fixture
        .post(request())
        .await
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .db
            .call(|conn| Ok(conn.query_row(
                "SELECT COUNT(*) FROM oauth_auth_rejections",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
    );
    fixture.stop().await;
}

#[tokio::test]
async fn unauthorized_handshake_never_falls_back_or_sends_inference() {
    for chat in [false, true] {
        let fixture = Fixture::start(false).await;
        fixture.first_profile(&["gpt-test"], "");
        fixture
            .mock
            .handshake_unauthorized
            .store(true, Ordering::SeqCst);
        if chat {
            let reply = fixture.chat(chat_request(false)).await;
            assert_eq!(reply.status(), 502);
            assert_eq!(
                reply.json::<Value>().await.unwrap()["error"]["type"],
                "upstream_authentication_error"
            );
            assert_eq!(fixture.rows().await[0]["status"], "local_rejected");
        } else {
            let mut socket = fixture.ws().await;
            let mut body = request();
            body["type"] = json!("response.create");
            socket
                .send(Message::Text(body.to_string().into()))
                .await
                .unwrap();
            assert_eq!(
                terminal(&mut socket).await["error"]["code"],
                "upstream_authentication_error"
            );
            assert!(fixture.rows().await.is_empty());
        }
        assert_eq!(fixture.mock.handshakes.lock().unwrap().len(), 1);
        assert!(fixture.mock.requests.lock().unwrap().is_empty());
        assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
        fixture
            .mock
            .handshake_unauthorized
            .store(false, Ordering::SeqCst);
        assert!(fixture
            .post(request())
            .await
            .text()
            .await
            .unwrap()
            .contains("response.completed"));
        assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn catalog_unauthorized_refreshes_only_on_next_discovery_and_then_requires_reauth() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "");
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .catalog_status = 401;
    assert!(fixture.upstream.models().await.is_err());
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
    assert!(fixture.upstream.models().await.is_err());
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert!(fixture.upstream.models().await.unwrap().is_empty());
    assert_eq!(fixture.mock.catalog_requests.lock().unwrap().len(), 2);
    assert_eq!(
        fixture
            .db
            .call(|conn| Ok(oauth::list(conn)?.remove(0).state))
            .await
            .unwrap(),
        "reauth_required"
    );
    assert!(fixture.rows().await.is_empty());
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn delayed_unauthorized_does_not_invalidate_operator_reauthorization() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "unauthorized_wait");
    let mut pending = Box::pin(fixture.post(request()));
    tokio::select! {
        _ = &mut pending => panic!("upstream replied before release"),
        _ = async {
            while fixture.mock.requests.lock().unwrap().is_empty() { tokio::time::sleep(Duration::from_millis(10)).await; }
        } => {}
    }
    let expiry = chrono::Utc::now().timestamp() + 3600;
    fixture
        .db
        .call(move |conn| {
            oauth::save(
                conn,
                &Vault::new([2; 32]),
                "upstream-account",
                &Credentials {
                    access_token: "upstream-access".into(),
                    refresh_token: "upstream-refresh".into(),
                },
                expiry,
            )
        })
        .await
        .unwrap();
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .mode
        .clear();
    fixture.mock.release.notify_one();
    assert_eq!(pending.await.status(), 502);
    let info = fixture
        .db
        .call(|conn| Ok(oauth::list(conn)?.remove(0)))
        .await
        .unwrap();
    assert_eq!(info.expires_at, expiry);
    assert_eq!(info.state, "active");
    assert_eq!(info.generation, 1);
    assert!(fixture
        .post(request())
        .await
        .text()
        .await
        .unwrap()
        .contains("response.completed"));
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn older_success_cannot_clear_a_newer_response_backoff() {
    let fixture = Fixture::start(false).await;
    let mut body = request();
    body["test_mode"] = json!("wait");
    let mut older = fixture.post(body).await;
    assert!(std::str::from_utf8(&older.chunk().await.unwrap().unwrap())
        .unwrap()
        .contains("response.created"));
    let mut body = request();
    body["test_mode"] = json!("upstream_503");
    assert_eq!(fixture.post(body).await.status(), 503);
    fixture.mock.release.notify_one();
    assert!(older.text().await.unwrap().contains("response.completed"));
    let paused = fixture.post(request()).await;
    assert_eq!(paused.status(), 503);
    assert_eq!(
        paused.json::<Value>().await.unwrap()["error"]["code"],
        "upstream_backoff"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert_eq!(fixture.rows().await.len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn health_storage_failure_does_not_discard_known_terminal_usage() {
    let fixture = Fixture::start(false).await;
    fixture.upstream.models().await.unwrap();
    fixture.db.call(|conn| {
        conn.execute_batch("CREATE TRIGGER reject_health BEFORE INSERT ON oauth_health WHEN NEW.scope='responses' BEGIN SELECT RAISE(FAIL,'fixture health unavailable'); END;")?;
        Ok(())
    }).await.unwrap();
    let mut body = request();
    body["test_mode"] = json!("failed");
    assert!(fixture
        .post(body)
        .await
        .text()
        .await
        .unwrap()
        .contains("response.failed"));
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "failed");
    assert_eq!(rows[0]["input"], 10);
    assert_eq!(rows[0]["output"], 2);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn compact_unauthorized_is_not_replayed_and_next_compaction_refreshes() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "unauthorized");
    let body = json!({"model":"gpt-test","input":[]});
    let rejected = fixture.compact(body.clone()).await;
    assert_eq!(rejected.status(), 502);
    let error = rejected.text().await.unwrap();
    assert!(error.contains("upstream_authentication_error"));
    assert!(!error.contains("private-upstream-secret"));
    assert_eq!(fixture.mock.compactions.lock().unwrap().len(), 1);
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .mode
        .clear();
    let response = fixture.compact(body).await.json::<Value>().await.unwrap();
    assert_eq!(response["object"], "response.compaction");
    assert_eq!(fixture.mock.compactions.lock().unwrap().len(), 2);
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.rows().await[0]["status"], "upstream_rejected");
    assert_eq!(fixture.rows().await[1]["input"], 4);
    fixture.stop().await;
}

fn chat_chunks(body: &str) -> (Vec<Value>, usize) {
    let mut chunks = Vec::new();
    let mut done = 0;
    for line in body.lines().filter(|line| !line.is_empty()) {
        let data = line
            .strip_prefix("data: ")
            .expect("Chat SSE must be data-only");
        if data == "[DONE]" {
            done += 1;
        } else {
            chunks.push(serde_json::from_str(data).unwrap());
        }
    }
    (chunks, done)
}

#[tokio::test]
async fn chat_images_preserve_mixed_input_over_json_sse_and_upstream_transports() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    // Deliberately synthetic bytes: this proves forwarding, not image recognition.
    let inline = format!(
        "data:image/png;base64,{}",
        STANDARD.encode(vec![0u8; 900_000])
    );
    for http_upstream in [false, true] {
        let fixture = Fixture::start(false).await;
        fixture
            .mock
            .reject_websocket
            .store(http_upstream, Ordering::SeqCst);
        for streaming in [false, true] {
            let response = fixture.chat(json!({"model":"gpt-test","stream":streaming,"messages":[
                {"role":"system","content":"synthetic instruction"},
                {"role":"user","content":[
                    {"type":"text","text":"before"},
                    {"type":"image_url","image_url":{"url":"https://example.com/image.png?signature=synthetic-image-canary","detail":"original"}},
                    {"type":"text","text":"between"},
                    {"type":"image_url","image_url":{"url":inline}},
                    {"type":"text","text":"after"}
                ]}
            ]})).await;
            assert_eq!(response.status(), 200);
            if streaming {
                let (chunks, done) = chat_chunks(&response.text().await.unwrap());
                assert_eq!(done, 1);
                assert!(chunks
                    .iter()
                    .any(|chunk| chunk["choices"][0]["finish_reason"] == "stop"));
            } else {
                let response = response.json::<Value>().await.unwrap();
                assert_eq!(response["choices"][0]["finish_reason"], "stop");
            }
            let calls = fixture.mock.requests.lock().unwrap();
            assert_eq!(calls.len(), if streaming { 2 } else { 1 });
            let call = calls.last().unwrap();
            assert!(call.to_string().len() > 1024 * 1024);
            assert_eq!(call["input"][0]["role"], "developer");
            assert_eq!(
                call["input"][1]["content"],
                json!([
                    {"type":"input_text","text":"before"},
                    {"type":"input_image","image_url":"https://example.com/image.png?signature=synthetic-image-canary","detail":"original"},
                    {"type":"input_text","text":"between"},
                    {"type":"input_image","image_url":inline},
                    {"type":"input_text","text":"after"}
                ])
            );
        }
        let rows = fixture.rows().await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row["status"] == "completed"
            && row["transport"]
                == if http_upstream {
                    "http_sse"
                } else {
                    "websocket"
                }));
        assert!(!serde_json::to_string(&rows)
            .unwrap()
            .contains("synthetic-image-canary"));
        fixture.stop().await;
    }
}

#[tokio::test]
async fn chat_invalid_images_fail_before_submission_and_usage() {
    let fixture = Fixture::start(false).await;
    for (role, image, suffix) in [
        ("assistant", json!({"url":"private-image-canary"}), "type"),
        ("system", json!({"url":"private-image-canary"}), "type"),
        ("user", json!("private-image-canary"), "image_url"),
        ("user", json!({"url":""}), "image_url.url"),
        (
            "user",
            json!({"url":"private-image-canary","detail":"private-image-canary"}),
            "image_url.detail",
        ),
        (
            "user",
            json!({"url":"private-image-canary","detail":null}),
            "image_url.detail",
        ),
        (
            "user",
            json!({"url":"private-image-canary","file_id":"private-image-canary"}),
            "image_url.file_id",
        ),
    ] {
        let response = fixture
            .chat(
                json!({"model":"gpt-test","messages":[{"role":role,"content":[
                    {"type":"image_url","image_url":image}
                ]}]}),
            )
            .await;
        assert_eq!(response.status(), 400);
        let error = response.json::<Value>().await.unwrap();
        assert_eq!(
            error["error"]["param"],
            format!("messages[0].content[0].{suffix}")
        );
        assert!(!error.to_string().contains("private-image-canary"));
    }
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn chat_json_replays_function_results_and_attributes_one_record_per_request() {
    let fixture = Fixture::start(false).await;
    fixture.mock.tools.store(true, Ordering::SeqCst);
    let messages = json!([
        {"role":"system","content":"Keep this exact system instruction."},
        {"role":"developer","content":[{"type":"text","text":"Keep developer order."}]},
        {"role":"user","content":"Request a synthetic function."}
    ]);
    let tools = json!([{"type":"function","function":{"name":"shell","description":"synthetic function","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}]);
    let response = fixture.chat(json!({"model":"gpt-test","messages":messages,"tools":tools,"tool_choice":{"type":"function","function":{"name":"shell"}},"parallel_tool_calls":false,"reasoning_effort":"low","response_format":{"type":"json_schema","json_schema":{"name":"fixture","schema":{"type":"object"}}}})).await;
    assert_eq!(response.status(), 200);
    let response = response.json::<Value>().await.unwrap();
    assert_eq!(response["object"], "chat.completion");
    assert_eq!(response["choices"][0]["finish_reason"], "tool_calls");
    assert!(response["choices"][0]["message"]["content"].is_null());
    let tool = &response["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(tool["id"], "call_fixture");
    assert_eq!(tool["function"]["name"], "shell");
    assert_eq!(
        serde_json::from_str::<Value>(tool["function"]["arguments"].as_str().unwrap()).unwrap()
            ["command"],
        "echo EXETROUTER_TOOL_OK"
    );
    assert_eq!(response["usage"]["prompt_tokens"], 10);
    assert_eq!(response["usage"]["completion_tokens"], 2);
    assert_eq!(response["usage"]["total_tokens"], 12);
    assert_eq!(
        response["usage"]["prompt_tokens_details"]["cached_tokens"],
        3
    );
    assert_eq!(
        response["usage"]["completion_tokens_details"]["reasoning_tokens"],
        1
    );
    let mut messages = messages.as_array().unwrap().clone();
    messages.push(response["choices"][0]["message"].clone());
    messages.push(json!({"role":"tool","tool_call_id":tool["id"],"content":[{"type":"text","text":"EXETROUTER_"},{"type":"text","text":"TOOL_OK"}]}));
    let reply = fixture
        .chat(json!({"model":"gpt-test","messages":messages,"tools":tools}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(reply["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        reply["choices"][0]["message"]["content"],
        "EXETROUTER_SMOKE_OK"
    );
    let calls = fixture.mock.requests.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["type"], "response.create");
    assert_eq!(calls[0]["input"][0]["role"], "developer");
    assert_eq!(
        calls[0]["input"][0]["content"][0]["text"],
        "Keep this exact system instruction."
    );
    assert_eq!(calls[0]["input"][1]["role"], "developer");
    assert_eq!(calls[0]["tools"][0]["name"], "shell");
    assert_eq!(calls[0]["tools"][0]["strict"], false);
    assert_eq!(
        calls[0]["tool_choice"],
        json!({"type":"function","name":"shell"})
    );
    assert_eq!(calls[0]["text"]["format"]["name"], "fixture");
    assert_eq!(calls[0]["text"]["format"]["strict"], false);
    assert_eq!(calls[0]["reasoning"]["effort"], "low");
    assert_eq!(calls[1]["input"][3]["type"], "function_call");
    assert_eq!(
        calls[1]["input"][4],
        json!({"type":"function_call_output","call_id":"call_fixture","output":"EXETROUTER_TOOL_OK"})
    );
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["surface"] == "chat_completions"
        && row["transport"] == "websocket"
        && row["client_transport"] == "http_json"
        && row["status"] == "completed"
        && row["user"] == 1
        && row["token"] == fixture.token_id));
    fixture.stop().await;
}

#[tokio::test]
async fn chat_stream_is_incremental_and_preserves_interleaved_tool_arguments() {
    let fixture = Fixture::start(false).await;
    *fixture.mock.mode.lock().unwrap() = "wait".into();
    let response = fixture.chat(chat_request(true)).await;
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut stream = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(first.to_vec()).unwrap();
    assert!(text.contains("assistant"));
    assert!(!text.contains("[DONE]"));
    fixture.mock.release.notify_one();
    let mut bytes = first.to_vec();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    let (chunks, done) = chat_chunks(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(done, 1);
    assert!(chunks.iter().all(|chunk| chunk.get("usage").is_none()));
    assert_eq!(
        chunks.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
    assert!(chunks
        .iter()
        .any(|chunk| chunk["choices"][0]["delta"]["content"] == "EXETROUTER_SMOKE_OK"));
    *fixture.mock.mode.lock().unwrap() = "parallel_tools".into();
    let mut request = chat_request(true);
    request["stream_options"] = json!({"include_usage":true,"include_obfuscation":false});
    let body = fixture.chat(request).await.text().await.unwrap();
    let (chunks, done) = chat_chunks(&body);
    assert_eq!(done, 1);
    let mut arguments = [String::new(), String::new()];
    let mut identities = Vec::new();
    let mut order = Vec::new();
    for chunk in &chunks {
        assert_eq!(chunk["object"], "chat.completion.chunk");
        if let Some(tools) = chunk
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for tool in tools {
                let index = tool["index"].as_u64().unwrap() as usize;
                arguments[index].push_str(tool["function"]["arguments"].as_str().unwrap());
                if tool.get("id").is_some() {
                    identities.push((index, tool["id"].clone(), tool["function"]["name"].clone()));
                } else {
                    order.push(index);
                }
            }
        }
    }
    assert_eq!(
        identities,
        vec![
            (0, json!("call_a"), json!("hello_a")),
            (1, json!("call_b"), json!("hello_b"))
        ]
    );
    assert_eq!(order, vec![0, 1, 1, 0]);
    assert_eq!(arguments, ["{\"word\":\"привет\"}", "{\"word\":\"мир\"}"]);
    let terminal = &chunks[chunks.len() - 2];
    assert_eq!(terminal["choices"][0]["finish_reason"], "tool_calls");
    let usage = chunks.last().unwrap();
    assert!(usage["choices"].as_array().unwrap().is_empty());
    assert_eq!(usage["usage"]["total_tokens"], 12);
    assert!(chunks[..chunks.len() - 1]
        .iter()
        .all(|chunk| chunk["usage"].is_null()));
    assert_eq!(fixture.rows().await.len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn chat_terminal_states_and_partial_usage_are_not_invented() {
    let fixture = Fixture::start(false).await;
    for mode in [
        "unknown_usage",
        "partial_usage",
        "length",
        "filtered",
        "refusal",
    ] {
        *fixture.mock.mode.lock().unwrap() = mode.into();
        let response = fixture.chat(chat_request(false)).await;
        assert_eq!(response.status(), 200);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(
            value["choices"][0]["finish_reason"],
            match mode {
                "length" => "length",
                "filtered" => "content_filter",
                _ => "stop",
            }
        );
        match mode {
            "unknown_usage" => assert!(value["usage"].is_null()),
            "partial_usage" => {
                assert_eq!(value["usage"]["prompt_tokens"], 10);
                assert!(value["usage"]["completion_tokens"].is_null());
                assert!(value["usage"]["total_tokens"].is_null());
            }
            "refusal" => {
                assert_eq!(
                    value["choices"][0]["message"]["refusal"],
                    "Synthetic refusal"
                );
                assert!(value["choices"][0]["message"]["content"].is_null());
            }
            _ => {}
        }
    }
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 5);
    assert!(rows[0]["input"].is_null());
    assert_eq!(rows[1]["input"], 10);
    assert!(rows[1]["output"].is_null());
    assert_eq!(rows[2]["status"], "incomplete");
    for (mode, reason) in [
        ("length", "length"),
        ("refusal", "stop"),
        ("unknown_usage", "stop"),
    ] {
        *fixture.mock.mode.lock().unwrap() = mode.into();
        let mut request = chat_request(true);
        request["stream_options"] = json!({"include_usage":true});
        let body = fixture.chat(request).await.text().await.unwrap();
        let (chunks, done) = chat_chunks(&body);
        assert_eq!(done, 1);
        assert_eq!(
            chunks[chunks.len() - 2]["choices"][0]["finish_reason"],
            reason
        );
        if mode == "refusal" {
            assert!(chunks
                .iter()
                .any(|chunk| chunk["choices"][0]["delta"]["refusal"] == "Synthetic refusal"));
        }
        if mode == "unknown_usage" {
            assert!(chunks.last().unwrap()["usage"].is_null());
        }
    }
    assert_eq!(fixture.rows().await.len(), 8);
    fixture.stop().await;
}

#[tokio::test]
async fn chat_failures_never_emit_success_or_repeat_inference() {
    let fixture = Fixture::start(false).await;
    for mode in [
        "disconnect",
        "failed",
        "corrupt_terminal",
        "unsupported_output",
    ] {
        // Each injected case is a separate probe after the previous pause.
        fixture.expire_health().await;
        *fixture.mock.mode.lock().unwrap() = mode.into();
        let body = fixture.chat(chat_request(true)).await.text().await.unwrap();
        let (chunks, done) = chat_chunks(&body);
        assert_eq!(done, 0);
        assert!(chunks
            .iter()
            .any(|chunk| chunk["error"]["code"] == "upstream_interrupted"
                || chunk["error"]["code"] == "upstream_rejected"));
        assert!(chunks.iter().all(|chunk| chunk
            .pointer("/choices/0/finish_reason")
            .is_none_or(Value::is_null)));
        assert!(!body.contains("private-upstream-secret"));
        assert!(!body.contains("private-output"));
    }
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 4);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["status"], "interrupted");
    assert!(rows[0]["input"].is_null());
    assert_eq!(rows[1]["status"], "failed");
    assert_eq!(rows[1]["input"], 10);
    fixture.stop().await;
}

#[tokio::test]
async fn chat_falls_back_only_before_inference_and_preserves_rate_limits() {
    let fixture = Fixture::start(false).await;
    fixture.mock.reject_websocket.store(true, Ordering::SeqCst);
    let reply = fixture
        .chat(chat_request(false))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        reply["choices"][0]["message"]["content"],
        "EXETROUTER_SMOKE_OK"
    );
    assert_eq!(fixture.rows().await[0]["transport"], "http_sse");
    *fixture.mock.mode.lock().unwrap() = "rate_limit".into();
    let reply = fixture.chat(chat_request(false)).await;
    assert_eq!(reply.status(), 429);
    assert_eq!(reply.headers()["retry-after"], "17");
    assert!(!reply
        .text()
        .await
        .unwrap()
        .contains("private upstream error"));
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert_eq!(fixture.rows().await.len(), 2);
    let reply = fixture.chat(chat_request(false)).await;
    assert_eq!(reply.status(), 429);
    assert!(
        reply.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            <= 17
    );
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "upstream_cooldown"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert_eq!(fixture.rows().await.len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn quotas_observe_http_headers_and_ws_events_without_blocking_at_one_hundred_percent() {
    let fixture = Fixture::start(false).await;
    let reset = chrono::Utc::now().timestamp() + 600;
    {
        let mut headers = fixture.mock.quota_headers.lock().unwrap();
        headers.insert("x-codex-secondary-used-percent", "42.5".parse().unwrap());
        headers.insert("x-codex-secondary-window-minutes", "7200".parse().unwrap());
        headers.insert(
            "x-codex-secondary-reset-at",
            reset.to_string().parse().unwrap(),
        );
    }
    let response = fixture.post(request()).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-codex-secondary-used-percent"], "42.5");
    assert_eq!(
        response.headers()["x-codex-secondary-window-minutes"],
        "7200"
    );
    assert_eq!(
        response.headers()["x-codex-secondary-reset-at"],
        reset.to_string()
    );

    *fixture.mock.quota_event.lock().unwrap() = Some(
        json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":100,"window_minutes":300,"reset_at":reset}},"metered_limit_name":"codex"}),
    );
    let reply = fixture.chat(chat_request(true)).await.text().await.unwrap();
    assert!(reply.contains("EXETROUTER_SMOKE_OK"));
    assert!(reply.contains("[DONE]"));
    assert!(!reply.contains("codex.rate_limits"));
    let reply = fixture.post(request()).await.text().await.unwrap();
    assert!(reply.contains("codex.rate_limits"));
    let summary = fixture
        .db
        .call(move |conn| exetrouter::quota::summary(conn, 1, chrono::Utc::now().timestamp()))
        .await
        .unwrap();
    assert_eq!(summary.status, "observed");
    assert_eq!(summary.windows.len(), 2);
    assert_eq!(summary.windows[0].used_percent, 100.0);
    assert_eq!(summary.windows[1].used_percent, 42.5);
    assert_eq!(summary.windows[1].window_minutes, Some(7200));
    assert_eq!(summary.cooldown_until, None);
    assert_eq!(fixture.rows().await.len(), 3);
    fixture.stop().await;
}

#[tokio::test]
async fn http_quota_rejection_persists_and_blocks_new_inference_across_transports() {
    let fixture = Fixture::start(false).await;
    let mut body = request();
    body["test_mode"] = json!("rate_limit_reset");
    let reply = fixture.post(body).await;
    assert_eq!(reply.status(), 429);
    assert!(!reply
        .text()
        .await
        .unwrap()
        .contains("private upstream error"));
    let summary = fixture
        .db
        .call(|conn| exetrouter::quota::summary(conn, 1, chrono::Utc::now().timestamp()))
        .await
        .unwrap();
    let until = summary.cooldown_until.unwrap();
    assert_eq!(summary.cooldown_source.as_deref(), Some("upstream_reset"));
    // A fresh worker and upstream instance read the persisted gate, rather than
    // relying on process-local state.
    let reopened = Database::open(fixture.dir.path().join("state.sqlite"))
        .await
        .unwrap();
    let fresh = Upstream::new(
        reopened,
        [2; 32],
        upstream::Config::new(fixture.issuer.clone(), fixture.issuer.clone(), true).unwrap(),
    )
    .unwrap();
    assert!(
        matches!(fresh.select("gpt-test").await,Err(upstream::SelectError::Cooldown(at)) if at==until)
    );
    for reply in [
        fixture.post(request()).await,
        fixture.chat(chat_request(false)).await,
    ] {
        assert_eq!(reply.status(), 429);
        assert!(reply.headers().contains_key("retry-after"));
        assert_eq!(
            reply.json::<Value>().await.unwrap()["error"]["code"],
            "upstream_cooldown"
        );
    }
    let mut upgrade = format!("{}/v1/responses", fixture.url.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    upgrade.headers_mut().insert(
        "authorization",
        format!("Bearer {}", fixture.secret).parse().unwrap(),
    );
    match connect_async(upgrade).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 429);
            assert!(response.headers().contains_key("retry-after"));
        }
        _ => panic!("expected cooldown rejection before WS upgrade"),
    }
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.rows().await.len(), 1);
    assert_eq!(fixture.rows().await[0]["status"], "upstream_rejected");
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=?1",
                [chrono::Utc::now().timestamp() - 1],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(fixture.post(request()).await.status(), 200);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn websocket_quota_errors_keep_history_and_do_not_replay_after_cooldown() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let mut previous = None;
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let event: Value = serde_json::from_str(&text).unwrap();
        if event["type"] == "response.completed" {
            previous = event["response"]["id"].as_str().map(str::to_owned);
            break;
        }
    }
    body["previous_response_id"] = json!(previous.unwrap());
    body["test_mode"] = json!("quota_error");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let event: Value =
        serde_json::from_str(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(event["error"]["type"], "usage_limit_reached");
    body.as_object_mut().unwrap().remove("test_mode");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let event: Value =
        serde_json::from_str(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(event["error"]["code"], "upstream_cooldown");
    assert!(event["error"]["retry_after"].as_i64().unwrap() > 0);
    assert_eq!(fixture.rows().await.len(), 2);
    assert_eq!(fixture.rows().await[1]["status"], "failed");
    assert!(fixture.rows().await[1]["input"].is_null());
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=?1",
                [chrono::Utc::now().timestamp() - 1],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let mut completed = false;
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let event: Value = serde_json::from_str(&text).unwrap();
        assert_ne!(event["type"], "error");
        if event["type"] == "response.completed" {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 3);
    assert_eq!(fixture.rows().await.len(), 3);
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn chat_quota_error_reports_json_429_or_stream_error_without_a_success_terminal() {
    for streaming in [false, true] {
        let fixture = Fixture::start(false).await;
        *fixture.mock.mode.lock().unwrap() = "quota_error".into();
        let reply = fixture.chat(chat_request(streaming)).await;
        if streaming {
            assert_eq!(reply.status(), 200);
            let body = reply.text().await.unwrap();
            let (chunks, done) = chat_chunks(&body);
            assert_eq!(done, 0);
            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0]["error"]["code"], "upstream_cooldown");
            assert!(!body.contains("synthetic quota rejection"));
        } else {
            assert_eq!(reply.status(), 429);
            assert_eq!(
                reply.json::<Value>().await.unwrap()["error"]["code"],
                "upstream_cooldown"
            );
        }
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.rows().await.len(), 1);
        assert_eq!(fixture.rows().await[0]["status"], "failed");
        assert!(fixture.rows().await[0]["input"].is_null());
        fixture.stop().await;
    }
}

#[tokio::test]
async fn quota_metadata_failure_does_not_lose_completed_usage_or_repeat_generation() {
    let fixture = Fixture::start(false).await;
    fixture.db.call(|conn| {
        conn.execute_batch("CREATE TRIGGER reject_quota BEFORE INSERT ON oauth_quota_windows BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).await.unwrap();
    *fixture.mock.quota_event.lock().unwrap() = Some(
        json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":90,"window_minutes":300,"reset_at":chrono::Utc::now().timestamp()+300}}}),
    );
    let reply = fixture.chat(chat_request(false)).await;
    assert_eq!(reply.status(), 200);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["choices"][0]["message"]["content"],
        "EXETROUTER_SMOKE_OK"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "completed");
    assert_eq!(rows[0]["input"], 10);
    assert_eq!(rows[0]["output"], 2);
    fixture.stop().await;
}

#[tokio::test]
async fn quota_cooldown_does_not_interrupt_an_already_sent_generation() {
    let fixture = Fixture::start(false).await;
    let mut body = request();
    body["test_mode"] = json!("wait");
    let mut stream = fixture.post(body).await;
    assert!(stream.chunk().await.unwrap().is_some());
    let mut rejection = request();
    rejection["test_mode"] = json!("rate_limit");
    assert_eq!(fixture.post(rejection).await.status(), 429);
    assert_eq!(fixture.post(request()).await.status(), 429);
    fixture.mock.release.notify_one();
    assert!(stream.text().await.unwrap().contains("response.completed"));
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["status"], "completed");
    assert_eq!(rows[0]["input"], 10);
    assert_eq!(rows[1]["status"], "upstream_rejected");
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn websocket_handshake_quota_rejection_never_falls_back_to_generation() {
    let fixture = Fixture::start(false).await;
    fixture
        .mock
        .handshake_rate_limit
        .store(true, Ordering::SeqCst);
    let reply = fixture.chat(chat_request(false)).await;
    assert_eq!(reply.status(), 429);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "upstream_cooldown"
    );
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.rows().await.len(), 1);
    assert_eq!(fixture.rows().await[0]["status"], "local_rejected");
    assert!(fixture.rows().await[0]["input"].is_null());
    fixture.stop().await;
}

#[tokio::test]
async fn chat_rejects_malformed_translated_structures_before_generation() {
    let fixture = Fixture::start(false).await;
    let invalids = [
        ("stream", json!("true")),
        ("stream_options", json!({"include_usage":true})),
        ("tool_choice", json!("required")),
        (
            "tools",
            json!([{"type":"custom","custom":{"name":"fixture"}}]),
        ),
        (
            "messages",
            json!([{"role":"tool","tool_call_id":"foreign-id","content":"private-body"}]),
        ),
        (
            "messages",
            json!([{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.com/image.png","unsupported":"private-body"}}]}]),
        ),
        (
            "messages",
            json!([{"role":"assistant","content":null,"tool_calls":[{"id":"call-a","type":"function","function":{"name":"fixture","arguments":"{}"}}]}]),
        ),
        (
            "response_format",
            json!({"type":"json_schema","json_schema":{"name":"fixture","schema":[],"strict":true}}),
        ),
    ];
    for (field, value) in invalids {
        let mut request = chat_request(false);
        request[field] = value;
        let response = fixture.chat(request).await;
        assert_eq!(response.status(), 400, "{field}");
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert!(value["error"]["param"].as_str().unwrap().starts_with(field));
        assert!(!value.to_string().contains("private-body"));
    }
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
    fixture.db.call(|conn| { conn.execute_batch("CREATE TRIGGER reject_requests BEFORE INSERT ON usage_events BEGIN SELECT RAISE(ABORT,'fixture-storage'); END;")?;Ok(()) }).await.unwrap();
    assert_eq!(fixture.chat(chat_request(false)).await.status(), 503);
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn responses_accept_sdk_string_input() {
    let fixture = Fixture::start(false).await;
    let reply = fixture
        .post(json!({"model":"gpt-test","input":"synthetic SDK input","store":false}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(reply["status"], "completed");
    assert_eq!(
        fixture.mock.requests.lock().unwrap()[0]["input"],
        json!([{"role":"user","content":"synthetic SDK input"}])
    );
    fixture.stop().await;
}

#[tokio::test]
async fn responses_lite_tools_keep_their_input_and_enable_the_required_http_and_ws_header() {
    let fixture = Fixture::start(false).await;
    fixture.mock.tools.store(true, Ordering::SeqCst);
    let tools = json!({"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"exec_command","parameters":{"type":"object"}}]}]});
    let mut body = request();
    let unsupported = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(&fixture.secret)
        .header("x-openai-internal-codex-responses-lite", "true")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(unsupported.status(), 400);
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    body.as_object_mut().unwrap().remove("tools");
    body["stream"] = json!(false);
    body["input"] = json!([tools,{"role":"system","content":"Keep this instruction."},{"role":"user","content":"fixture"}]);
    let reply = fixture
        .post(body.clone())
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(reply["output"][0]["type"], "function_call");
    let seen = fixture.mock.requests.lock().unwrap()[0].clone();
    assert_eq!(seen["input"][0], body["input"][0]);
    assert_eq!(seen["input"][1]["role"], "developer");
    assert_eq!(seen["input"][1]["content"], "Keep this instruction.");
    let mut socket = fixture.ws().await;
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let _ = terminal(&mut socket).await;
    assert_eq!(fixture.mock.lite_handshakes.load(Ordering::SeqCst), 1);
    assert!(fixture
        .rows()
        .await
        .iter()
        .all(|row| row["status"] == "completed"));
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn native_custom_namespace_phase_images_and_opaque_items_keep_order_and_owner() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    let prefix = json!({"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"apply_patch","format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}]}]});
    let mut body = request();
    body.as_object_mut().unwrap().remove("tools");
    body["test_mode"] = json!("native_extensions");
    body["input"] = json!([prefix,{"role":"user","content":[{"type":"input_text","text":"synthetic image ordering"},{"type":"input_image","image_url":"data:image/png;base64,c3ludGhldGlj","detail":"original"},{"type":"input_text","text":"after image"}]}]);
    let wire = fixture.post(body.clone()).await.text().await.unwrap();
    let events = wire
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let deltas = events
        .iter()
        .filter(|event| event["type"] == "response.custom_tool_call_input.delta")
        .map(|event| event["delta"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(deltas, "*** Begin Patch\n*** End Patch");
    let output = events.last().unwrap()["response"]["output"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(output[1]["type"], "custom_tool_call");
    assert_eq!(output[1]["namespace"], "functions");
    assert_eq!(output[2]["phase"], "commentary");
    assert!(output[3]["encrypted_function_args"].is_array());
    body["input"].as_array_mut().unwrap().extend(output.clone());
    let large_output = "synthetic custom output ".repeat(90000);
    body["input"].as_array_mut().unwrap().extend([json!({"type":"custom_tool_call_output","call_id":"call_custom","output":large_output}),json!({"type":"function_call_output","call_id":"call_function","output":"synthetic function result"})]);
    body["type"] = json!("response.create");
    let mut socket = fixture.ws().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let terminal = terminal(&mut socket).await;
    assert_eq!(terminal["response"]["output"], json!(output));
    let calls = fixture.mock.requests.lock().unwrap().clone();
    assert_eq!(calls[0]["input"][0], prefix);
    assert_eq!(calls[0]["input"][1], body["input"][1]);
    assert_eq!(calls[1]["input"], body["input"]);
    let accounts = fixture.mock.request_accounts.lock().unwrap().clone();
    assert_eq!(accounts[0], accounts[1]);
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn chat_caps_are_translated_and_other_options_are_forwarded() {
    let fixture = Fixture::start(false).await;
    for field in ["max_tokens", "max_completion_tokens"] {
        let mut body = chat_request(false);
        body[field] = json!(64);
        body["temperature"] = json!(0.2);
        body["future_option"] = json!({"mode":"new"});
        body["n"] = json!(2);
        body["reasoning_effort"] = json!("future_effort");
        assert_eq!(fixture.chat(body).await.status(), 200);
        let sent = fixture
            .mock
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(sent["max_output_tokens"], 64);
        assert!(sent.get(field).is_none());
        assert_eq!(sent["temperature"], 0.2);
        assert_eq!(sent["future_option"], json!({"mode":"new"}));
        assert_eq!(sent["n"], 2);
        assert_eq!(sent["reasoning"]["effort"], "future_effort");
    }
    fixture.stop().await;
}

#[tokio::test]
async fn backend_validation_status_is_retained_but_errors_are_redacted_without_retry() {
    let fixture = Fixture::start(false).await;
    *fixture.mock.mode.lock().unwrap() = "backend_parameter_error".into();
    fixture.mock.reject_websocket.store(true, Ordering::SeqCst);
    for chat in [false, true] {
        let reply = if chat {
            fixture.chat(chat_request(false)).await
        } else {
            fixture.post(request()).await
        };
        assert_eq!(reply.status(), 422);
        assert_eq!(reply.headers()["retry-after"], "3");
        assert_eq!(
            reply.json::<Value>().await.unwrap(),
            json!({"error":{"message":"upstream rejected the request","type":"upstream_rejected"}})
        );
    }
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|r| r["status"] == "upstream_rejected" && r["input"].is_null()));
    fixture.stop().await;
}

#[tokio::test]
async fn backend_error_bodies_are_redacted_and_remain_bounded() {
    let fixture = Fixture::start(false).await;
    for (mode, expected) in [
        (
            "backend_detail_error",
            json!({"detail":"Unsupported parameter: synthetic_option"}),
        ),
        (
            "upstream_503",
            json!({"error":{"message":"Synthetic backend failure","type":"server_error","code":"server_error"}}),
        ),
    ] {
        fixture.expire_health().await;
        let mut body = request();
        body["test_mode"] = json!(mode);
        let reply = fixture.post(body).await;
        assert_eq!(
            reply.status(),
            if mode == "upstream_503" { 503 } else { 400 }
        );
        let value = reply.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "upstream_rejected");
        assert!(!value.to_string().contains(expected.to_string().as_str()));
        assert!(!value.to_string().contains("Synthetic"));
    }
    for mode in ["backend_invalid_error", "backend_oversized_error"] {
        fixture.expire_health().await;
        let mut body = request();
        body["test_mode"] = json!(mode);
        let reply = fixture.post(body).await;
        assert_eq!(reply.status(), 400);
        let value = reply.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "upstream_rejected");
        assert!(!value.to_string().contains("Synthetic non-JSON"));
        assert!(!value.to_string().contains("synthetic large"));
    }
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 4);
    assert_eq!(fixture.rows().await.len(), 4);
    fixture.stop().await;
}

#[tokio::test]
async fn backend_websocket_errors_survive_chat_conversion_and_native_forwarding() {
    let fixture = Fixture::start(false).await;
    *fixture.mock.mode.lock().unwrap() = "backend_parameter_error".into();
    let expected = json!({"error":{"message":"Synthetic option is invalid","type":"invalid_request_error","code":"backend_changed_validation","param":"future_option"}});
    let reply = fixture.chat(chat_request(false)).await;
    assert_eq!(reply.status(), 422);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "upstream_rejected"
    );
    let body = fixture.chat(chat_request(true)).await.text().await.unwrap();
    let (chunks, done) = chat_chunks(&body);
    assert_eq!(done, 0);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0]["error"]["code"], "upstream_rejected");
    assert!(!body.contains("Synthetic option"));
    let mut ws = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    ws.send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let event = terminal(&mut ws).await;
    assert_eq!(event["status"], 422);
    assert_eq!(event["error"], expected["error"]);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 3);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 3);
    assert!(rows
        .iter()
        .all(|row| row["status"] == "failed" && row["input"].is_null()));
    ws.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn missing_content_type_drains_valid_sse_but_never_accepts_a_non_sse_body() {
    let fixture = Fixture::start(false).await;
    for streaming in [false, true] {
        let mut body = request();
        body["test_mode"] = json!("no_content_type");
        body["stream"] = json!(streaming);
        let response = fixture.post(body).await;
        assert_eq!(response.status(), 200);
        if streaming {
            assert!(response
                .text()
                .await
                .unwrap()
                .contains("response.completed"));
        } else {
            assert_eq!(
                response.json::<Value>().await.unwrap()["status"],
                "completed"
            );
        }
    }
    let mut body = request();
    body["test_mode"] = json!("invalid_no_content_type");
    body["stream"] = json!(false);
    let response = fixture.post(body).await;
    assert_eq!(response.status(), 502);
    assert!(!response
        .text()
        .await
        .unwrap()
        .contains("synthetic non-SSE body"));
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 3);
    let rows = fixture.rows().await;
    assert_eq!(rows[0]["input"], 10);
    assert_eq!(rows[1]["status"], "completed");
    assert_eq!(rows[2]["status"], "interrupted");
    assert!(rows[2]["input"].is_null());
    fixture.stop().await;
}

#[tokio::test]
async fn completed_items_supply_responses_json_and_chat_when_terminal_output_is_empty() {
    let fixture = Fixture::start(false).await;
    for mode in ["empty_terminal_output", "missing_terminal_output"] {
        *fixture.mock.mode.lock().unwrap() = mode.into();
        let mut body = request();
        body["stream"] = json!(false);
        let response = fixture.post(body).await.json::<Value>().await.unwrap();
        assert_eq!(
            response["output"][0]["content"][0]["text"],
            "EXETROUTER_SMOKE_OK"
        );
        let response = fixture
            .chat(chat_request(false))
            .await
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(
            response["choices"][0]["message"]["content"],
            "EXETROUTER_SMOKE_OK"
        );
        let response = fixture.chat(chat_request(true)).await.text().await.unwrap();
        assert!(response.contains("EXETROUTER_SMOKE_OK"));
        assert!(response.contains("[DONE]"));
    }
    let response = fixture.post(request()).await.text().await.unwrap();
    let terminal = response
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find(|event| event["type"] == "response.completed")
        .unwrap();
    assert!(
        terminal["response"].get("output").is_none(),
        "raw Responses SSE must preserve the upstream event"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 7);
    assert!(fixture
        .rows()
        .await
        .iter()
        .all(|row| row["status"] == "completed" && row["input"] == 10));
    fixture.stop().await;
}

#[tokio::test]
async fn user_limits_span_tokens_http_and_websocket_without_blocking_other_users() {
    let fixture = Fixture::with_limits(false, 1).await;
    let (alice, bob) = fixture
        .db
        .call(|conn| {
            let alice = exetrouter::create_token(conn, &[1; 32], 1, "second-device", 1)?;
            let bob = exetrouter::create_user(conn, "bob")?;
            let bob = exetrouter::create_token(conn, &[1; 32], bob, "test", 1)?;
            Ok((alice.secret, bob.secret))
        })
        .await
        .unwrap();
    let mut waiting = request();
    waiting["test_mode"] = json!("wait");
    let mut active = fixture.post(waiting).await;
    active.chunk().await.unwrap();
    let rejected = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", fixture.url))
        .bearer_auth(&alice)
        .json(&chat_request(false))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 429);
    assert_eq!(
        rejected.json::<Value>().await.unwrap()["error"]["type"],
        "user_concurrency_limit"
    );
    let bob_reply = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(&bob)
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(bob_reply.status(), 200);
    bob_reply.text().await.unwrap();

    let mut socket = fixture.ws().await;
    let mut upgrade = format!("{}/v1/responses", fixture.url.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    upgrade
        .headers_mut()
        .insert("authorization", format!("Bearer {alice}").parse().unwrap());
    let failure = connect_async(upgrade.clone()).await.err().unwrap();
    let tokio_tungstenite::tungstenite::Error::Http(failure) = failure else {
        panic!("expected rejected Upgrade")
    };
    assert_eq!(failure.status(), 429);
    upgrade
        .headers_mut()
        .insert("authorization", format!("Bearer {bob}").parse().unwrap());
    let (mut bob_socket, _) = connect_async(upgrade.clone()).await.unwrap();
    let mut create = request();
    create["type"] = json!("response.create");
    socket
        .send(Message::Text(create.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "user_concurrency_limit"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert_eq!(fixture.rows().await.len(), 2);

    fixture.mock.release.notify_one();
    active.text().await.unwrap();
    socket
        .send(Message::Text(create.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while socket.next().await.is_some() {}
    })
    .await
    .unwrap();
    drop(socket);
    upgrade
        .headers_mut()
        .insert("authorization", format!("Bearer {alice}").parse().unwrap());
    let mut replacement = None;
    for _ in 0..100 {
        match connect_async(upgrade.clone()).await {
            Ok((socket, _)) => {
                replacement = Some(socket);
                break;
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(reply)) if reply.status() == 429 => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(error) => panic!("unexpected reconnect error: {error}"),
        }
    }
    let mut replacement = replacement.expect("WS user permit was not released");
    replacement.close(None).await.unwrap();
    bob_socket.close(None).await.unwrap();
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 3);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row["status"] == "completed"));
    fixture.stop().await;
}

#[tokio::test]
async fn client_disconnect_releases_user_generation_capacity() {
    let fixture = Fixture::with_limits(false, 1).await;
    let mut waiting = request();
    waiting["test_mode"] = json!("wait");
    let mut response = fixture.post(waiting).await;
    response.chunk().await.unwrap();
    drop(response);
    for _ in 0..100 {
        if fixture.rows().await[0]["status"] == "interrupted" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fixture.rows().await[0]["status"], "interrupted");
    let info = fixture
        .db
        .call(|conn| Ok(oauth::list(conn)?.remove(0)))
        .await
        .unwrap();
    assert!(
        info.health_until.is_none(),
        "client cancellation must not penalize the upstream account"
    );
    let reply = fixture.chat(chat_request(false)).await;
    assert_eq!(reply.status(), 200);
    reply.json::<Value>().await.unwrap();
    assert_eq!(fixture.rows().await.len(), 2);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn responses_streams_before_terminal_and_records_exact_usage() {
    let fixture = Fixture::start(false).await;
    let mut value = request();
    value["test_mode"] = json!("wait");
    let mut response = fixture.post(value.clone()).await;
    assert_eq!(response.status(), 200);
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let chunk = tokio::time::timeout(Duration::from_secs(1), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&chunk)
        .unwrap()
        .contains("response.created"));
    assert_eq!(fixture.rows().await[0]["status"], "sent");
    fixture.mock.release.notify_one();
    let rest = response.text().await.unwrap();
    assert!(rest.contains("EXETROUTER_SMOKE_OK"));
    assert!(rest.contains("fixture_extension"));
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["status"], "completed");
    assert_eq!(rows[0]["input"], 10);
    assert_eq!(rows[0]["output"], 2);
    assert_eq!(rows[0]["cached"], 3);
    assert_eq!(rows[0]["reasoning"], 1);
    value["instructions"] = json!("");
    let mut forwarded = fixture.mock.requests.lock().unwrap()[0].clone();
    assert_eq!(forwarded["prompt_cache_key"].as_str().unwrap().len(), 64);
    forwarded
        .as_object_mut()
        .unwrap()
        .remove("prompt_cache_key");
    assert_eq!(forwarded, value);
    fixture.stop().await;
}

#[tokio::test]
async fn disconnect_is_not_retried_and_duplicate_terminal_is_counted_once() {
    let fixture = Fixture::start(false).await;
    let mut value = request();
    value["test_mode"] = json!("disconnect");
    let body = fixture.post(value).await.text().await.unwrap();
    assert!(body.contains("upstream_interrupted"));
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.post(request()).await.status(), 503);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    fixture.expire_health().await;
    let mut value = request();
    value["test_mode"] = json!("duplicate");
    fixture.post(value).await.text().await.unwrap();
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["status"], "interrupted");
    assert!(rows[0]["input"].is_null());
    assert_eq!(rows[1]["status"], "completed");
    fixture.stop().await;
}

#[tokio::test]
async fn json_compaction_and_unknown_models_follow_the_contract() {
    let fixture = Fixture::start(false).await;
    let catalog = reqwest::Client::new()
        .get(format!("{}/v1/models", fixture.url))
        .bearer_auth(&fixture.secret)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(catalog["data"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["data"][0]["id"], "gpt-test");
    let mut value = request();
    value["stream"] = json!(false);
    let response = fixture.post(value).await.json::<Value>().await.unwrap();
    assert_eq!(response["status"], "completed");
    assert_eq!(response["usage"]["total_tokens"], 12);
    let compact = reqwest::Client::new()
        .post(format!("{}/v1/responses/compact", fixture.url))
        .bearer_auth(&fixture.secret)
        .json(&json!({"model":"gpt-test","input":[]}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(compact["object"], "response.compaction");
    assert_eq!(fixture.rows().await.len(), 2);
    let invalid = reqwest::Client::new()
        .post(format!("{}/v1/responses/compact", fixture.url))
        .bearer_auth(&fixture.secret)
        .json(&json!({"model":"gpt-test","input":[],"test_mode":"missing_checkpoint"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 502);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2]["status"], "completed");
    assert_eq!(rows[2]["input"], 4);
    fixture.expire_health().await;
    let mut value = request();
    value["model"] = json!("hidden");
    assert_eq!(fixture.post(value).await.status(), 404);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    fixture.stop().await;
}

async fn terminal(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            let event: Value = serde_json::from_str(&text).unwrap();
            if event["type"] == "response.completed" || event["type"] == "error" {
                return event;
            }
        }
    }
}

#[tokio::test]
async fn pool_native_ws_handshake_failure_sends_an_error_without_inference_or_account_replay() {
    for limited in [false, true] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        fixture
            .mock
            .reject_websocket
            .store(!limited, Ordering::SeqCst);
        fixture
            .mock
            .handshake_rate_limit
            .store(limited, Ordering::SeqCst);
        // Client Upgrade succeeds before the first model chooses an upstream account.
        let mut socket = fixture.ws().await;
        let mut body = request();
        body["type"] = json!("response.create");
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        let error = terminal(&mut socket).await;
        assert_eq!(
            error["error"]["code"],
            if limited {
                "upstream_cooldown"
            } else {
                "upstream_websocket_unavailable"
            }
        );
        assert!(!error.to_string().contains("private handshake error"));
        if limited {
            assert_eq!(error["status"], 429);
            assert!(error["error"]["retry_after"].as_i64().unwrap() > 0);
            let cooldown = fixture
                .db
                .call(|conn| {
                    Ok(
                        exetrouter::quota::summary(conn, 1, chrono::Utc::now().timestamp())?
                            .cooldown_until,
                    )
                })
                .await
                .unwrap();
            assert!(cooldown.is_some());
        } else {
            let close = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap();
            assert!(matches!(close, Some(Ok(Message::Close(_)))));
        }
        assert!(fixture.mock.requests.lock().unwrap().is_empty());
        assert!(fixture.rows().await.is_empty());
        fixture.mock.reject_websocket.store(false, Ordering::SeqCst);
        fixture
            .mock
            .handshake_rate_limit
            .store(false, Ordering::SeqCst);
        if limited {
            assert_eq!(
                *fixture.mock.handshakes.lock().unwrap(),
                ["upstream-account", "upstream-account-2"]
            );
            fixture
                .db
                .call(|conn| {
                    conn.execute(
                        "UPDATE oauth_accounts SET cooldown_until=NULL WHERE id=2",
                        [],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        // Only a new independent request may use the healthy second account.
        let reply = fixture.post(request()).await;
        assert_eq!(reply.status(), 200);
        assert!(reply.text().await.unwrap().contains("response.completed"));
        assert_eq!(
            *fixture.mock.request_accounts.lock().unwrap(),
            ["upstream-account-2"]
        );
        assert_eq!(fixture.rows().await.len(), 1);
        assert_eq!(fixture.rows().await[0]["status"], "completed");
        if limited {
            socket.close(None).await.unwrap();
        }
        fixture.stop().await;
    }
}

#[tokio::test]
async fn pool_compaction_routes_http_and_new_ws_to_its_account_after_restart() {
    let mut fixture = Fixture::start(false).await;
    fixture
        .add_account(&["gpt-test", "gpt-second"], false)
        .await;
    let reply = fixture
        .compact(json!({"model":"gpt-second","input":[]}))
        .await;
    assert_eq!(reply.status(), 200);
    let checkpoint = reply.json::<Value>().await.unwrap()["output"].clone();
    fixture.db.call(|conn| {
        let now=chrono::Utc::now().timestamp();
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,observed_at,request_order) VALUES(1,'primary',0,?1,0),(2,'primary',99,?1,0)",[now])?;
        Ok(())
    }).await.unwrap();
    fixture.restart().await;
    let mut body = request();
    body["input"] = checkpoint.clone();
    body["stream"] = json!(false);
    assert_eq!(fixture.post(body.clone()).await.status(), 200);
    let compact = fixture
        .compact(json!({"model":"gpt-test","input":checkpoint}))
        .await;
    assert_eq!(compact.status(), 200);
    let checkpoint = compact.json::<Value>().await.unwrap()["output"].clone();
    assert!(fixture
        .mock
        .compactions
        .lock()
        .unwrap()
        .iter()
        .all(|(account, _)| account == "upstream-account-2"));
    body["input"] = checkpoint;
    body["type"] = json!("response.create");
    let mut socket = fixture.ws().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    socket.close(None).await.unwrap();
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account-2", "upstream-account-2"]
    );
    fixture.db.call(|conn| {
        assert_eq!(conn.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0))?,exetrouter::store::SCHEMA_VERSION as i64);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM context_bindings WHERE length(digest)=32 AND account_id=2",[],|row|row.get::<_,i64>(0))?,2);
        oauth::disable(conn,2)?; Ok(())
    }).await.unwrap();
    body.as_object_mut().unwrap().remove("type");
    assert_eq!(fixture.post(body).await.status(), 503);
    assert_eq!(fixture.rows().await.len(), 4);
    let reply = fixture.post(request()).await;
    assert_eq!(reply.status(), 200);
    let _ = reply.text().await.unwrap();
    assert_eq!(
        fixture
            .mock
            .request_accounts
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        "upstream-account"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn pool_context_is_user_scoped_and_rejects_unknown_mixed_and_expired_items() {
    let fixture = Fixture::start(false).await;
    fixture
        .add_account(&["gpt-test", "gpt-second"], false)
        .await;
    let first = fixture
        .compact(json!({"model":"gpt-test","input":[]}))
        .await
        .json::<Value>()
        .await
        .unwrap()["output"][0]
        .clone();
    let second = fixture
        .compact(json!({"model":"gpt-second","input":[]}))
        .await
        .json::<Value>()
        .await
        .unwrap()["output"][0]
        .clone();
    let mut body = request();
    body["stream"] = json!(false);
    body["input"] = json!([first, second]);
    assert_eq!(
        fixture
            .post(body.clone())
            .await
            .json::<Value>()
            .await
            .unwrap()["error"]["code"],
        "context_account_mismatch"
    );
    body["input"] = json!([{"type":"reasoning","encrypted_content":"unknown-private-context"}]);
    let reply = fixture.post(body.clone()).await;
    assert_eq!(reply.status(), 400);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "context_not_found"
    );
    let (same, foreign) = fixture
        .db
        .call(|conn| {
            let bob = exetrouter::create_user(conn, "bob")?;
            Ok((
                exetrouter::create_token(conn, &[1; 32], 1, "second-device", 1)?.secret,
                exetrouter::create_token(conn, &[1; 32], bob, "bob-device", 1)?.secret,
            ))
        })
        .await
        .unwrap();
    body["input"] = json!([first]);
    let reply = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(foreign)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 400);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "context_not_found"
    );
    let reply = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(same)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    let _ = reply.json::<Value>().await.unwrap();
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE context_bindings SET expires_at=?1",
                [chrono::Utc::now().timestamp()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(fixture.post(body.clone()).await.status(), 400);
    body["type"] = json!("response.create");
    let mut socket = fixture.ws().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "context_not_found"
    );
    socket.close(None).await.unwrap();
    assert_eq!(fixture.rows().await.len(), 3);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn pinned_ws_rejects_context_from_another_account_without_moving_its_history() {
    let fixture = Fixture::start(false).await;
    fixture
        .add_account(&["gpt-test", "gpt-second"], false)
        .await;
    let checkpoint = fixture
        .compact(json!({"model":"gpt-second","input":[]}))
        .await
        .json::<Value>()
        .await
        .unwrap()["output"]
        .clone();
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    body["previous_response_id"] = terminal(&mut socket).await["response"]["id"].clone();
    body["input"] = checkpoint;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "context_account_mismatch"
    );
    body["input"] = request()["input"].clone();
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account", "upstream-account"]
    );
    assert_eq!(fixture.rows().await.len(), 3);
    socket.close(None).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn context_affinity_survives_refresh_and_never_bypasses_cooldown_or_missing_models() {
    let fixture = Fixture::start(false).await;
    fixture
        .add_account(&["gpt-test", "gpt-second"], false)
        .await;
    let checkpoint = fixture
        .compact(json!({"model":"gpt-second","input":[]}))
        .await
        .json::<Value>()
        .await
        .unwrap()["output"]
        .clone();
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET expires_at=?1 WHERE id=2",
                [chrono::Utc::now().timestamp() - 1],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut body = request();
    body["stream"] = json!(false);
    body["input"] = checkpoint.clone();
    assert_eq!(fixture.post(body.clone()).await.status(), 200);
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=?1 WHERE id=2",
                [chrono::Utc::now().timestamp() + 60],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    body["model"] = json!("gpt-second");
    assert_eq!(fixture.post(body.clone()).await.status(), 429);
    assert_eq!(
        fixture
            .compact(json!({"model":"gpt-second","input":checkpoint}))
            .await
            .status(),
        429
    );
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=NULL,catalog_updated_at=NULL WHERE id=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account-2")
        .unwrap()
        .models = vec!["gpt-second".into()];
    body["model"] = json!("gpt-test");
    assert_eq!(fixture.post(body.clone()).await.status(), 404);
    let mut socket = fixture.ws().await;
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "model_not_found"
    );
    socket.close(None).await.unwrap();
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.rows().await.len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn streamed_opaque_output_is_bound_before_forwarding_and_survives_disconnect() {
    let fixture = Fixture::start(false).await;
    fixture
        .add_account(&["gpt-test", "gpt-second"], false)
        .await;
    let mut body = request();
    body["test_mode"] = json!("opaque_wait");
    let mut stream = fixture.post(body).await;
    let mut wire = String::new();
    while !wire.contains("synthetic-context-") {
        let chunk = tokio::time::timeout(Duration::from_secs(3), stream.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        wire.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    let item = wire
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event["type"] == "response.output_item.done" && event["item"]["type"] == "reasoning"
        })
        .unwrap()["item"]
        .clone();
    let mut body = request();
    body["stream"] = json!(false);
    body["input"] = json!([item]);
    assert_eq!(fixture.post(body).await.status(), 200);
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account", "upstream-account"]
    );
    drop(stream);
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    body["model"] = json!("gpt-second");
    body["test_mode"] = json!("opaque");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let output = terminal(&mut socket).await["response"]["output"].clone();
    socket.close(None).await.unwrap();
    let mut body = request();
    body["stream"] = json!(false);
    body["input"] = output;
    assert_eq!(fixture.post(body).await.status(), 200);
    assert_eq!(
        fixture
            .mock
            .request_accounts
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        "upstream-account-2"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn context_storage_failure_preserves_known_usage_and_never_returns_an_unbound_terminal() {
    for surface in ["compact", "json", "sse", "ws"] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        fixture.db.call(|conn| { conn.execute_batch("CREATE TRIGGER reject_context BEFORE INSERT ON context_bindings BEGIN SELECT RAISE(ABORT,'private storage failure'); END;")?; Ok(()) }).await.unwrap();
        let wire = if surface == "compact" {
            let reply = fixture
                .compact(json!({"model":"gpt-test","input":[],"test_mode":"opaque_terminal"}))
                .await;
            assert_eq!(reply.status(), 502);
            reply.text().await.unwrap()
        } else if surface == "ws" {
            let mut socket = fixture.ws().await;
            let mut body = request();
            body["type"] = json!("response.create");
            body["test_mode"] = json!("opaque_terminal");
            socket
                .send(Message::Text(body.to_string().into()))
                .await
                .unwrap();
            let event = terminal(&mut socket).await;
            assert_eq!(event["error"]["code"], "upstream_interrupted");
            event.to_string()
        } else {
            let mut body = request();
            body["test_mode"] = json!("opaque_terminal");
            body["stream"] = json!(surface == "sse");
            let reply = fixture.post(body).await;
            assert_eq!(reply.status(), if surface == "sse" { 200 } else { 502 });
            reply.text().await.unwrap()
        };
        assert!(!wire.contains("synthetic-reasoning-"));
        assert!(!wire.contains("private storage failure"));
        assert!(!wire.contains("response.completed"));
        let rows = fixture.rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["status"], "completed");
        assert_eq!(rows[0]["input"], if surface == "compact" { 4 } else { 10 });
        assert_eq!(
            fixture.mock.requests.lock().unwrap().len()
                + fixture.mock.compactions.lock().unwrap().len(),
            1
        );
        fixture.stop().await;
    }
}

#[tokio::test]
async fn context_lookup_storage_failure_rejects_before_usage_or_inference() {
    let fixture = Fixture::start(false).await;
    fixture
        .db
        .call(|conn| {
            conn.execute_batch("DROP TABLE context_bindings;")?;
            Ok(())
        })
        .await
        .unwrap();
    let mut body = request();
    body["input"] = json!([{"encrypted_content":"unknown-private-context"}]);
    let reply = fixture.post(body.clone()).await;
    assert_eq!(reply.status(), 503);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "storage_unavailable"
    );
    body["type"] = json!("response.create");
    let mut socket = fixture.ws().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "storage_unavailable"
    );
    assert!(fixture.rows().await.is_empty());
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn pool_catalog_is_a_union_and_the_first_ws_model_selects_its_account() {
    let fixture = Fixture::start(false).await;
    fixture
        .add_account(&["gpt-test", "gpt-second"], false)
        .await;
    let models = fixture.upstream.models().await.unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-second", "gpt-test"]
    );
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    body["model"] = json!("gpt-second");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["response"]["model"],
        "gpt-second"
    );
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account-2"]
    );
    let account = fixture
        .db
        .call(|conn| {
            Ok(
                conn.query_row("SELECT account_id FROM usage_events", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(account, 2);
    fixture
        .db
        .call(|conn| {
            oauth::disable(conn, 2)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .upstream
            .models()
            .await
            .unwrap()
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-test"]
    );
    let mut invalid = request();
    invalid["model"] = json!("gpt-second");
    assert_eq!(fixture.post(invalid).await.status(), 404);
    assert_eq!(fixture.rows().await.len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn pool_ws_keeps_affinity_until_quota_then_recovers_on_another_account() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.db.call(|conn| {
        let now=chrono::Utc::now().timestamp();
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(1,'primary',70,300,?1,?2,0),(2,'primary',20,300,?1,?2,0)",rusqlite::params![now+300,now])?;
        Ok(())
    }).await.unwrap();
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let previous = terminal(&mut socket).await["response"]["id"].clone();
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account-2"]
    );
    fixture.db.call(|conn| {
        conn.execute("UPDATE oauth_quota_windows SET used_percent=CASE account_id WHEN 1 THEN 0 ELSE 99 END",[])?;
        Ok(())
    }).await.unwrap();
    body["previous_response_id"] = previous;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let second = terminal(&mut socket).await;
    assert_eq!(second["type"], "response.completed");
    body["previous_response_id"] = second["response"]["id"].clone();
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account-2", "upstream-account-2"]
    );
    fixture.db.call(|conn| {
        conn.execute("UPDATE oauth_accounts SET cooldown_until=?1,cooldown_source='retry_after' WHERE id=2",[chrono::Utc::now().timestamp()+300])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(fixture.post(request()).await.status(), 200);
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let switched = terminal(&mut socket).await;
    assert_eq!(switched["type"], "response.completed");
    body["previous_response_id"] = switched["response"]["id"].clone();
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        [
            "upstream-account-2",
            "upstream-account-2",
            "upstream-account",
            "upstream-account"
        ]
    );
    assert_eq!(fixture.rows().await.len(), 4);
    let forwarded = fixture.mock.requests.lock().unwrap().clone();
    assert!(forwarded[3].get("previous_response_id").is_none());
    assert_eq!(forwarded[3]["input"].as_array().unwrap().len(), 5);
    drop(forwarded);
    fixture
        .db
        .call(|conn| {
            oauth::disable(conn, 1)?;
            Ok(())
        })
        .await
        .unwrap();
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "upstream_unavailable"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 4);
    fixture.stop().await;
}

#[tokio::test]
async fn pool_unknown_or_stale_quota_balances_live_streams_and_releases_reservations() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.db.call(|conn| {
        let now=chrono::Utc::now().timestamp();
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(1,'primary',99,300,?1,?2,0)",rusqlite::params![now+300,now-60])?;
        Ok(())
    }).await.unwrap();
    let mut body = request();
    body["test_mode"] = json!("wait");
    let mut first = fixture.post(body.clone()).await;
    assert!(first.chunk().await.unwrap().is_some());
    let mut second = fixture.post(body).await;
    assert!(second.chunk().await.unwrap().is_some());
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account", "upstream-account-2"]
    );
    fixture.mock.release.notify_waiters();
    assert!(first.text().await.unwrap().contains("response.completed"));
    assert!(second.text().await.unwrap().contains("response.completed"));
    assert_eq!(
        fixture.upstream.select("gpt-test").await.unwrap().info.id,
        1
    );
    assert_eq!(fixture.rows().await.len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn pool_cooldown_only_filters_matching_models_and_returns_the_earliest_reset() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test", "gpt-only-first"], "");
    fixture.add_account(&["gpt-test"], false).await;
    let now = chrono::Utc::now().timestamp();
    fixture.db.call(move |conn| {
        conn.execute("UPDATE oauth_accounts SET cooldown_until=CASE id WHEN 1 THEN ?1 ELSE ?2 END,cooldown_source='retry_after'",rusqlite::params![now+600,now+300])?;
        Ok(())
    }).await.unwrap();
    assert!(
        matches!(fixture.upstream.select("gpt-test").await,Err(upstream::SelectError::Cooldown(at)) if at==now+300)
    );
    assert!(
        matches!(fixture.upstream.select("gpt-only-first").await,Err(upstream::SelectError::Cooldown(at)) if at==now+600)
    );
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=NULL WHERE id=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.upstream.select("gpt-test").await.unwrap().info.id,
        2
    );
    assert!(matches!(
        fixture.upstream.select("gpt-only-first").await,
        Err(upstream::SelectError::Cooldown(_))
    ));
    assert!(fixture.rows().await.is_empty());
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn pool_429_never_replays_inference_and_a_separate_new_request_uses_a_healthy_account() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-test"], "rate_limit");
    fixture.add_account(&["gpt-test"], false).await;
    assert_eq!(fixture.post(request()).await.status(), 429);
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account"]
    );
    assert_eq!(fixture.rows().await.len(), 1);
    assert_eq!(fixture.post(request()).await.status(), 200);
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account", "upstream-account-2"]
    );
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["status"], "upstream_rejected");
    assert_eq!(rows[1]["status"], "completed");
    let accounts = fixture
        .db
        .call(|conn| {
            let mut stmt = conn.prepare("SELECT account_id FROM usage_events ORDER BY id")?;
            let rows = stmt
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(accounts, [1, 2]);
    fixture.stop().await;
}

#[tokio::test]
async fn pool_failed_catalog_or_credentials_do_not_hide_healthy_models_or_fake_a_404() {
    let fixture = Fixture::start(false).await;
    fixture.first_profile(&["gpt-only-first"], "");
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account")
        .unwrap()
        .catalog_error = true;
    fixture.add_account(&["gpt-test"], false).await;
    assert_eq!(fixture.post(request()).await.status(), 200);
    let mut unknown = request();
    unknown["model"] = json!("gpt-only-first");
    assert_eq!(fixture.post(unknown).await.status(), 503);
    assert_eq!(fixture.rows().await.len(), 1);
    fixture
        .mock
        .accounts
        .lock()
        .unwrap()
        .get_mut("upstream-account-2")
        .unwrap()
        .catalog_error = true;
    fixture
        .db
        .call(|conn| {
            conn.execute("UPDATE oauth_accounts SET catalog_updated_at=NULL", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(fixture.post(request()).await.status(), 503);
    fixture.stop().await;

    let fixture = Fixture::start(true).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.mock.invalid_grant.store(true, Ordering::SeqCst);
    assert_eq!(fixture.post(request()).await.status(), 200);
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account-2"]
    );
    assert_eq!(
        fixture
            .db
            .call(|conn| Ok(oauth::list(conn)?[0].state.clone()))
            .await
            .unwrap(),
        "reauth_required"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn pool_reauth_or_model_change_never_moves_a_pinned_socket_to_another_account() {
    for reauth in [true, false] {
        let fixture = Fixture::start(false).await;
        fixture
            .add_account(&["gpt-test", "gpt-second"], false)
            .await;
        let mut socket = fixture.ws().await;
        let mut body = request();
        body["type"] = json!("response.create");
        body["model"] = json!("gpt-second");
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        body["previous_response_id"] = terminal(&mut socket).await["response"]["id"].clone();
        body["model"] = json!("gpt-test");
        if reauth {
            fixture
                .db
                .call(|conn| {
                    oauth::save(
                        conn,
                        &Vault::new([2; 32]),
                        "upstream-account-2",
                        &Credentials {
                            access_token: "upstream-access-2".into(),
                            refresh_token: "upstream-refresh-2".into(),
                        },
                        chrono::Utc::now().timestamp() + 3600,
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        } else {
            fixture
                .mock
                .accounts
                .lock()
                .unwrap()
                .get_mut("upstream-account-2")
                .unwrap()
                .models
                .clear();
            fixture
                .db
                .call(|conn| {
                    conn.execute(
                        "UPDATE oauth_accounts SET catalog_updated_at=NULL WHERE id=2",
                        [],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        assert_eq!(
            terminal(&mut socket).await["error"]["code"],
            if reauth {
                "upstream_unavailable"
            } else {
                "model_not_found"
            }
        );
        assert_eq!(
            *fixture.mock.request_accounts.lock().unwrap(),
            ["upstream-account-2"]
        );
        assert_eq!(fixture.rows().await.len(), 1);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn pool_disconnect_releases_an_http_reservation_after_an_unknown_outcome() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    let mut body = request();
    body["test_mode"] = json!("wait");
    let mut reply = fixture.post(body).await;
    assert!(reply.chunk().await.unwrap().is_some());
    assert_eq!(
        fixture.upstream.select("gpt-test").await.unwrap().info.id,
        2
    );
    drop(reply);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let selected = fixture.upstream.select("gpt-test").await.unwrap();
            let released = selected.info.id == 1;
            drop(selected);
            if released {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.rows().await.len(), 1);
    assert_eq!(fixture.rows().await[0]["status"], "interrupted");
    assert!(fixture.rows().await[0]["input"].is_null());
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn pool_refresh_leases_are_independent_and_coalesce_per_account() {
    let fixture = Fixture::start(true).await;
    fixture.first_profile(&["gpt-test"], "");
    fixture.add_account(&["gpt-test"], true).await;
    let mut jobs = tokio::task::JoinSet::new();
    for id in [1, 2, 1, 2, 1, 2, 1, 2] {
        let upstream = fixture.upstream.clone();
        jobs.spawn(async move {
            let account = upstream.account(id).await.unwrap();
            assert_eq!(account.info.id, id);
            assert_eq!(account.info.generation, 1);
        });
    }
    while let Some(result) = jobs.join_next().await {
        result.unwrap();
    }
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 2);
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn websocket_services_upstream_ping_between_requests() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    body["generate"] = json!(false);
    body["test_mode"] = json!("idle_ping");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let warmup = terminal(&mut socket).await;
    assert_eq!(warmup["type"], "response.completed");
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.mock.idle_pongs.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("upstream ping was not serviced while the client was idle");
    assert_eq!(fixture.mock.idle_closes.load(Ordering::SeqCst), 0);
    body.as_object_mut().unwrap().remove("generate");
    body.as_object_mut().unwrap().remove("test_mode");
    body["previous_response_id"] = warmup["response"]["id"].clone();
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(fixture.mock.handshakes.lock().unwrap().len(), 1);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    assert!(fixture
        .rows()
        .await
        .iter()
        .all(|row| row["status"] == "completed"));
    fixture.stop().await;
}

#[tokio::test]
async fn websocket_reconnects_idle_closed_upstream_before_submitting_continuation() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    body["test_mode"] = json!("idle_close");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let first = terminal(&mut socket).await;
    assert_eq!(first["type"], "response.completed");
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.mock.idle_closes.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    body.as_object_mut().unwrap().remove("test_mode");
    body["previous_response_id"] = first["response"]["id"].clone();
    body["input"] = json!([{"role":"user","content":"continue after an idle closure"}]);
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    let requests = fixture.mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].get("previous_response_id").is_none());
    assert_eq!(requests[1]["input"][0], requests[0]["input"][0]);
    assert_eq!(
        requests[1]["input"].as_array().unwrap().last().unwrap(),
        &body["input"][0]
    );
    assert_eq!(
        fixture.mock.handshakes.lock().unwrap().as_slice(),
        &["upstream-account", "upstream-account"]
    );
    let sessions = fixture.mock.sessions.lock().unwrap().clone();
    assert_eq!(sessions[0].1, sessions[1].1);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["status"] == "completed"));
    // An older branch cannot silently use an ID belonging to the retired socket.
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "context_recovery_unavailable"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn websocket_defers_idle_metadata_until_the_next_response_created() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["type"] = json!("response.create");
    body["test_mode"] = json!("idle_metadata");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let first = terminal(&mut socket).await;
    assert_eq!(first["type"], "response.completed");
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.mock.idle_pongs.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    body.as_object_mut().unwrap().remove("test_mode");
    body["previous_response_id"] = first["response"]["id"].clone();
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let created = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&created).unwrap()["type"],
        "response.created"
    );
    let metadata = socket.next().await.unwrap().unwrap().into_text().unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["type"], "response.metadata");
    let state = metadata["headers"]["x-codex-turn-state"].clone();
    assert_eq!(state, "private-synthetic-idle-turn-state");
    let second = terminal(&mut socket).await;
    assert_eq!(second["type"], "response.completed");
    body["previous_response_id"] = second["response"]["id"].clone();
    body["client_metadata"] = json!({"x-codex-turn-state":state});
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 3);
    assert_eq!(fixture.mock.handshakes.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn websocket_idle_recovery_rechecks_revocation_deactivation_and_backoff() {
    for guard in ["revoked", "disabled", "backoff"] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        let mut socket = fixture.ws().await;
        let mut body = request();
        body["type"] = json!("response.create");
        body["test_mode"] = json!("idle_close");
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        let first = terminal(&mut socket).await;
        assert_eq!(first["type"], "response.completed");
        let token = fixture.token_id.clone();
        fixture.db.call(move |conn| {
            match guard {
                "revoked" => { exetrouter::revoke_token(conn, 1, &token)?; }
                "disabled" => {
                    exetrouter::account_preferences::set_policy(conn, 1, Some(false), None, Some(true))?;
                }
                "backoff" => {
                    conn.execute("UPDATE oauth_health SET retry_at=?1, reason='transport', failures=1 WHERE account_id=1 AND scope='responses'",
                        [chrono::Utc::now().timestamp()+60])?;
                }
                _ => unreachable!(),
            }
            Ok(())
        }).await.unwrap();
        body.as_object_mut().unwrap().remove("test_mode");
        body["previous_response_id"] = first["response"]["id"].clone();
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        let expected = match guard {
            "revoked" => "authentication_error",
            "disabled" => "account_disabled",
            _ => "upstream_backoff",
        };
        assert_eq!(
            terminal(&mut socket).await["error"]["code"],
            expected,
            "{guard}"
        );
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.mock.handshakes.lock().unwrap().len(), 1);
        assert_eq!(fixture.rows().await.len(), 1);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn websocket_continuations_are_owned_and_revocation_is_rechecked() {
    let fixture = Fixture::start(false).await;
    let mut socket = fixture.ws().await;
    let mut value = request();
    value["type"] = json!("response.create");
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
    let first = terminal(&mut socket).await;
    assert_eq!(first["fixture_extension"], "preserved");
    let previous = first["response"]["id"].clone();
    value["previous_response_id"] = previous;
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    let mut stranger = fixture.ws().await;
    stranger
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut stranger).await["error"]["code"],
        "previous_response_not_found"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    let id = fixture.token_id.clone();
    fixture
        .db
        .call(move |conn| {
            exetrouter::revoke_token(conn, 1, &id)?;
            Ok(())
        })
        .await
        .unwrap();
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "authentication_error"
    );
    assert_eq!(fixture.rows().await.len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn concurrent_refresh_rotates_once_and_invalid_grant_requires_reauth() {
    let fixture = Fixture::start(true).await;
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let upstream = fixture.upstream.clone();
        jobs.spawn(async move { upstream.account(1).await.unwrap().credentials.refresh_token });
    }
    while let Some(job) = jobs.join_next().await {
        assert_eq!(job.unwrap(), "rotated-refresh");
    }
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    fixture
        .db
        .call(|conn| {
            oauth::save(
                conn,
                &Vault::new([2; 32]),
                "upstream-account",
                &Credentials {
                    access_token: "upstream-access".into(),
                    refresh_token: "upstream-refresh".into(),
                },
                0,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.mock.invalid_grant.store(true, Ordering::SeqCst);
    let error = fixture.upstream.account(1).await.err().unwrap().to_string();
    assert_eq!(error, "OAuth reauthorization required");
    assert!(!error.contains("must-not-escape"));
    let state = fixture
        .db
        .call(|conn| Ok(oauth::list(conn)?[0].state.clone()))
        .await
        .unwrap();
    assert_eq!(state, "reauth_required");
    fixture.stop().await;
}

#[tokio::test]
async fn device_grant_and_database_failure_are_checked_without_real_accounts() {
    let fixture = Fixture::start(false).await;
    let client = upstream::http_client().unwrap();
    let code = oauth::begin_device(&client, &fixture.issuer).await.unwrap();
    assert_eq!(code.user_code, "ABCD-EFGH");
    let (account, credentials, _) = oauth::complete_device(&client, &fixture.issuer, code)
        .await
        .unwrap();
    assert_eq!(account, "upstream-account");
    assert_eq!(credentials.refresh_token, "upstream-refresh");
    fixture
        .mock
        .device_account_in_id_token
        .store(true, Ordering::SeqCst);
    let code = oauth::begin_device(&client, &fixture.issuer).await.unwrap();
    let (account, _, _) = oauth::complete_device(&client, &fixture.issuer, code)
        .await
        .unwrap();
    assert_eq!(account, "upstream-account");
    fixture.db.call(|conn| { conn.execute_batch("CREATE TRIGGER reject_requests BEFORE INSERT ON usage_events BEGIN SELECT RAISE(ABORT,'storage-test'); END;")?;Ok(()) }).await.unwrap();
    assert_eq!(fixture.post(request()).await.status(), 503);
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn operator_reauthorization_wins_over_an_in_flight_refresh() {
    let fixture = Fixture::start(true).await;
    fixture.mock.refresh_wait.store(true, Ordering::SeqCst);
    let upstream = fixture.upstream.clone();
    let refresh = tokio::spawn(async move { upstream.account(1).await.unwrap() });
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.mock.refresh_started.notified(),
    )
    .await
    .unwrap();
    fixture
        .db
        .call(|conn| {
            oauth::save(
                conn,
                &Vault::new([2; 32]),
                "upstream-account",
                &Credentials {
                    access_token: "operator-access".into(),
                    refresh_token: "operator-refresh".into(),
                },
                chrono::Utc::now().timestamp() + 7200,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.mock.release.notify_one();
    let account = refresh.await.unwrap();
    assert_eq!(account.credentials.refresh_token, "operator-refresh");
    assert_eq!(account.credentials.access_token, "operator-access");
    assert_eq!(account.info.generation, 1);
    fixture.stop().await;
}

#[tokio::test]
async fn shutdown_finalizes_active_streams_and_preserves_unknown_usage() {
    let mut fixture = Fixture::start(false).await;
    let mut value = request();
    value["test_mode"] = json!("wait");
    let mut response = fixture.post(value).await;
    response.chunk().await.unwrap();
    fixture.stop.take().unwrap().send(()).unwrap();
    let body = response.text().await.unwrap();
    assert!(body.contains("upstream_interrupted"));
    tokio::time::timeout(Duration::from_secs(3), &mut fixture.server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fixture.rows().await[0]["status"], "interrupted");
    assert!(fixture.rows().await[0]["input"].is_null());
}

#[tokio::test]
#[ignore = "requires current Codex and OpenCode V2 binaries; see docs/compatibility.md"]
async fn current_clients_complete_tool_cycles_over_http_and_websocket() {
    let codex = std::env::var("EXETROUTER_CODEX_BIN").expect("EXETROUTER_CODEX_BIN");
    let opencode = std::env::var("EXETROUTER_OPENCODE_BIN").expect("EXETROUTER_OPENCODE_BIN");
    native::version(&codex, &native::reviewed_version("codex"))
        .await
        .unwrap();
    native::version(&opencode, &native::reviewed_version("opencode"))
        .await
        .unwrap();
    for (client, websocket) in [
        ("codex", false),
        ("codex", true),
        ("opencode", false),
        ("opencode", true),
    ] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        fixture.mock.tools.store(true, Ordering::SeqCst);
        // Real handshake headers must be emitted after created for both clients.
        fixture
            .mock
            .quota_headers
            .lock()
            .unwrap()
            .insert("openai-model", "gpt-test".parse().unwrap());
        let probe = probe::Probe::bounded(&fixture.url, 12).await.unwrap();
        let output = native::Run {
            binary: if client == "codex" { &codex } else { &opencode },
            client,
            url: &probe.url,
            model: "gpt-test",
            bearer: &fixture.secret,
            websocket,
            directory: fixture.dir.path(),
            prompt: "Run the local compatibility fixture and report the result.",
        }
        .execute()
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            probe.observed.lock().unwrap().metadata()["tool_markers"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            output.status.success()
                && stdout.contains("EXETROUTER_SMOKE_OK")
                && native::tool_result(&output.stdout),
            "{client} websocket={websocket}: {} {}\n{stdout}\n{stderr}",
            output.status,
            native::diagnostic(&output.stdout)
        );
        let calls = fixture.mock.requests.lock().unwrap().clone();
        let tools=calls.iter().filter_map(|call|call.get("tools")).flat_map(|tools|tools.as_array().into_iter().flatten()).map(|tool|json!({"name":tool["name"],"type":tool["type"],"nested":tool.get("tools").and_then(Value::as_array).map(|tools|tools.iter().map(|tool|tool["name"].clone()).collect::<Vec<_>>())})).collect::<Vec<_>>();
        let outputs = calls
            .iter()
            .filter_map(|call| call.get("input").and_then(Value::as_array))
            .flatten()
            .filter(|item| item["type"] == "function_call_output")
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            calls.iter().any(
                |call| call
                    .get("input")
                    .and_then(Value::as_array)
                    .is_some_and(|input| input
                        .iter()
                        .any(|item| item["type"] == "function_call_output"
                            && item["output"].to_string().contains("EXETROUTER_TOOL_OK")))
            ),
            "{client} did not execute and return the tool result; tools={tools:?}; outputs={outputs:?}; stdout={stdout}; stderr={stderr}"
        );
        let rows = fixture.rows().await;
        assert!(rows.iter().all(|row| row["status"] == "completed"));
        let expected = if websocket { "websocket" } else { "http_sse" };
        let primary = calls
            .iter()
            .filter(|call| {
                call.get("tools")
                    .and_then(Value::as_array)
                    .is_some_and(|tools| !tools.is_empty())
            })
            .collect::<Vec<_>>();
        assert!(primary.len() >= 2);
        assert!(
            primary
                .iter()
                .all(|call| (call["type"] == "response.create") == websocket),
            "{client} primary steps did not use the selected transport"
        );
        assert!(
            rows.iter().all(|row| row["transport"] == expected
                || (client == "opencode" && websocket && row["transport"] == "http_sse")),
            "{client} used an unexpected transport: {rows:?}"
        );
        println!(
            "{client} transport={expected}: tool round-trip passed ({} requests)",
            rows.len()
        );
        fixture.stop().await;
    }
    for (client, binary) in [("codex", &codex)] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        *fixture.mock.mode.lock().unwrap() = "upstream_503".into();
        for account in fixture.mock.accounts.lock().unwrap().values_mut() {
            account.mode = "upstream_503".into();
        }
        let probe = probe::Probe::bounded(&fixture.url, 12).await.unwrap();
        let output = native::Run {
            binary,
            client,
            url: &probe.url,
            model: "gpt-test",
            bearer: &fixture.secret,
            websocket: false,
            directory: fixture.dir.path(),
            prompt: "Run the local compatibility fixture and report the result.",
        }
        .execute()
        .await
        .unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK"));
        assert_eq!(
            probe.observed.lock().unwrap().metadata()["primary_requests"],
            1,
            "{client} repeated a primary request after 503"
        );
        println!("{client}: HTTP 503 caused one primary request and no client replay");
        fixture.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires isolated Python/JavaScript OpenAI SDK installations; see docs/compatibility.md"]
async fn openai_sdks_complete_json_stream_and_tool_cycles() {
    let python = std::env::var("EXETROUTER_PYTHON_BIN").expect("EXETROUTER_PYTHON_BIN");
    let node = std::env::var("EXETROUTER_NODE_BIN").expect("EXETROUTER_NODE_BIN");
    let module = std::env::var("EXETROUTER_OPENAI_JS_MODULE").expect("EXETROUTER_OPENAI_JS_MODULE");
    for (client, failed) in [
        ("python", false),
        ("javascript", false),
        ("python", true),
        ("javascript", true),
    ] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        fixture.mock.tools.store(true, Ordering::SeqCst);
        if failed {
            *fixture.mock.mode.lock().unwrap() = "failed".into();
        }
        let mut command =
            tokio::process::Command::new(if client == "python" { &python } else { &node });
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(if client == "python" {
            "tests/support/openai-sdk.py"
        } else {
            "tests/support/openai-sdk.mjs"
        });
        command
            .arg(script)
            .current_dir(fixture.dir.path())
            .kill_on_drop(true)
            .env_clear()
            .env("EXETROUTER_TOKEN", &fixture.secret)
            .env("EXETROUTER_TEST_URL", &fixture.url)
            .env(
                "EXETROUTER_TEST_MODE",
                if failed { "failed" } else { "success" },
            )
            .env("EXETROUTER_OPENAI_JS_MODULE", &module);
        let output = tokio::time::timeout(Duration::from_secs(30), command.output())
            .await
            .expect("SDK timed out")
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let marker = if failed {
            "SDK_ERROR_OK"
        } else {
            "SDK_ROUNDTRIP_OK"
        };
        assert!(
            output.status.success() && stdout.contains(marker),
            "{client} failed={failed}: {stdout}\n{stderr}"
        );
        let rows = fixture.rows().await;
        assert_eq!(rows.len(), if failed { 1 } else { 8 });
        assert_eq!(fixture.mock.requests.lock().unwrap().len(), rows.len());
        assert!(rows
            .iter()
            .all(|row| row["status"] == if failed { "failed" } else { "completed" }));
        println!("{client}: {marker} ({} requests)", rows.len());
        fixture.stop().await;
    }
}

#[tokio::test]
async fn cache_affinity_survives_rotation_restart_and_transport_without_crossing_users() {
    let mut fixture = Fixture::start(false).await;
    let mut body = request();
    body["stream"] = json!(false);
    body["prompt_cache_key"] = json!("thread-1");
    assert_eq!(fixture.post(body.clone()).await.status(), 200);
    let session = fixture.mock.sessions.lock().unwrap()[0].1.clone();
    assert_ne!(session, "thread-1");
    assert_eq!(session.len(), 64);
    let token = fixture.token_id.clone();
    let (rotated, other) = fixture
        .db
        .call(move |conn| {
            let rotated = exetrouter::rotate_token(conn, &[1; 32], 1, &token)?.secret;
            let user = exetrouter::create_user(conn, "bob-cache")?;
            let other = exetrouter::create_token(conn, &[1; 32], user, "test", 1)?.secret;
            Ok((rotated, other))
        })
        .await
        .unwrap();
    for (bearer, equal) in [(&rotated, true), (&other, false)] {
        let response = reqwest::Client::new()
            .post(format!("{}/v1/responses", fixture.url))
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.bytes().await.unwrap();
        assert_eq!(
            fixture.mock.sessions.lock().unwrap().last().unwrap().1 == session,
            equal
        );
    }
    fixture.restart().await;
    assert_eq!(fixture.post(body.clone()).await.status(), 200);
    assert_eq!(
        fixture.mock.sessions.lock().unwrap().last().unwrap().1,
        session
    );
    let mut chat = chat_request(false);
    chat["prompt_cache_key"] = json!("thread-1");
    assert_eq!(fixture.chat(chat).await.status(), 200);
    assert_eq!(
        fixture.mock.sessions.lock().unwrap().last().unwrap().1,
        session
    );
    let mut no_key = body.clone();
    no_key.as_object_mut().unwrap().remove("prompt_cache_key");
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(&fixture.secret)
        .header("session-id", "thread-1")
        .header("thread-id", "untrusted-client-thread")
        .json(&no_key)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();
    assert_eq!(
        fixture.mock.sessions.lock().unwrap().last().unwrap().1,
        session
    );
    for _ in 0..2 {
        assert_eq!(fixture.post(no_key.clone()).await.status(), 200);
    }
    let default = fixture
        .mock
        .sessions
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .clone();
    {
        let sessions = fixture.mock.sessions.lock().unwrap();
        assert_eq!(sessions[sessions.len() - 2].1, default);
        assert_ne!(default, session);
    }
    let mut ws = fixture.ws().await;
    body["type"] = json!("response.create");
    ws.send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if message.to_text().unwrap().contains("response.completed") {
            break;
        }
    }
    assert_eq!(
        fixture.mock.sessions.lock().unwrap().last().unwrap().1,
        session
    );
    let requests = fixture.mock.requests.lock().unwrap().len();
    body["prompt_cache_key"] = json!("another-thread");
    ws.send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let error = ws.next().await.unwrap().unwrap();
    assert!(error.to_text().unwrap().contains("cache_key_changed"));
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), requests);
    ws.close(None).await.unwrap();
    let rows = fixture.rows().await.len();
    for key in [json!(""), json!("a".repeat(65)), json!(false)] {
        let mut invalid = body.clone();
        invalid["prompt_cache_key"] = key;
        assert_eq!(fixture.post(invalid).await.status(), 400);
    }
    assert_eq!(fixture.rows().await.len(), rows);
    fixture.stop().await;
}

#[tokio::test]
async fn cache_account_preference_is_stable_but_never_overrides_checkpoint_or_health() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    let mut body = request();
    body["stream"] = json!(false);
    body["prompt_cache_key"] = json!("affinity");
    for _ in 0..3 {
        assert_eq!(fixture.post(body.clone()).await.status(), 200);
    }
    let accounts = fixture.mock.request_accounts.lock().unwrap().clone();
    assert!(accounts.iter().all(|id| id == &accounts[0]));
    let source = if accounts[0] == "upstream-account" {
        1
    } else {
        2
    };
    fixture
        .db
        .call(move |conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=?1 WHERE id=?2",
                rusqlite::params![chrono::Utc::now().timestamp() + 300, source],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(fixture.post(body.clone()).await.status(), 200);
    let other = fixture
        .mock
        .request_accounts
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_ne!(other, accounts[0]);
    let compact = reqwest::Client::new()
        .post(format!("{}/v1/responses/compact", fixture.url))
        .bearer_auth(&fixture.secret)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let checkpoint = compact["output"].clone();
    assert!(checkpoint.is_array());
    fixture
        .db
        .call(|conn| {
            conn.execute("UPDATE oauth_accounts SET cooldown_until=NULL", [])?;
            Ok(())
        })
        .await
        .unwrap();
    // Restoring the preferred account does not move the checkpoint's history.
    body["input"] = checkpoint;
    assert_eq!(fixture.post(body).await.status(), 200);
    assert_eq!(
        *fixture
            .mock
            .request_accounts
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        other
    );
    fixture.stop().await;
}

#[tokio::test]
async fn model_metadata_is_authenticated_persistent_and_conservative_across_accounts() {
    let mut fixture = Fixture::start(false).await;
    let client = reqwest::Client::new();
    for path in ["codex", "opencode-v1", "opencode-v2"] {
        assert_eq!(
            client
                .get(format!("{}/v1/models/{path}", fixture.url))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let get = |url: String, secret: String| {
        let client = &client;
        async move {
            client
                .get(url)
                .bearer_auth(secret)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let codex = get(
        format!("{}/v1/models/codex", fixture.url),
        fixture.secret.clone(),
    )
    .await;
    assert_eq!(codex["models"][0]["context_window"], 64000);
    assert_eq!(codex["models"][0]["auto_compact_token_limit"], 48000);
    assert_eq!(
        codex["models"][0]["supported_reasoning_levels"][1]["effort"],
        "high"
    );
    fixture.restart().await;
    assert_eq!(
        get(
            format!("{}/v1/models/codex", fixture.url),
            fixture.secret.clone()
        )
        .await,
        codex
    );
    assert_eq!(fixture.mock.catalog_requests.lock().unwrap().len(), 1);
    fixture.add_account(&["gpt-test"], false).await;
    let mut row = mock::model_row("gpt-test", "Test model", "list");
    row["context_window"] = json!(32000);
    row["supported_reasoning_levels"] = json!([{"effort":"low","description":"Low"}]);
    row["private_account"] = json!("do-not-expose");
    fixture
        .mock
        .catalog_rows
        .lock()
        .unwrap()
        .insert("upstream-account-2".into(), vec![row.clone()]);
    let merged = get(
        format!("{}/v1/models/codex", fixture.url),
        fixture.secret.clone(),
    )
    .await;
    assert_eq!(merged["models"][0]["context_window"], 32000);
    assert_eq!(
        merged["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!merged.to_string().contains("do-not-expose"));
    let open = get(
        format!("{}/v1/models/opencode-v2", fixture.url),
        fixture.secret.clone(),
    )
    .await;
    assert_eq!(
        open["providers"]["exetrouter"]["models"]["gpt-test"]["limit"],
        json!({"context":32000,"input":30400,"output":0})
    );
    let v1 = get(
        format!("{}/v1/models/opencode-v1", fixture.url),
        fixture.secret.clone(),
    )
    .await;
    assert_eq!(
        v1["provider"]["exetrouter"]["models"]["gpt-test"]["limit"],
        open["providers"]["exetrouter"]["models"]["gpt-test"]["limit"]
    );
    assert!(v1["provider"]["exetrouter"]["models"]["gpt-test"]["variants"].is_object());
    row["shell_type"] = json!("shell_command");
    fixture
        .mock
        .catalog_rows
        .lock()
        .unwrap()
        .insert("upstream-account-2".into(), vec![row]);
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET catalog_updated_at=NULL WHERE id=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let rejected = client
        .get(format!("{}/v1/models/codex", fixture.url))
        .bearer_auth(&fixture.secret)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 503);
    assert!(rejected
        .text()
        .await
        .unwrap()
        .contains("model_metadata_unavailable"));
    assert!(fixture.rows().await.is_empty());
    fixture.stop().await;
}

#[tokio::test]
async fn account_preferences_route_by_user_and_operator_deactivation_blocks_bound_ws() {
    let fixture = Fixture::start(false).await;
    let second = fixture.add_account(&["gpt-test"], false).await;
    fixture.upstream.models().await.unwrap();
    fixture
        .db
        .call(move |conn| {
            exetrouter::control(
                conn,
                &[1; 32],
                1,
                exetrouter::ControlRequest::AccountSet {
                    account: second,
                    enabled: Some(true),
                    priority: Some(10),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let response = fixture.post(request()).await;
    assert!(response.status().is_success());
    response.text().await.unwrap();
    let selected = fixture
        .db
        .call(|conn| {
            Ok(conn.query_row(
                "SELECT account_id FROM usage_events ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(selected, second);
    fixture
        .db
        .call(move |conn| {
            exetrouter::account_preferences::set_user(conn, 1, second, Some(false), None)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .upstream
            .select_for_user("gpt-test", None, Some(1))
            .await
            .unwrap()
            .info
            .id,
        1
    );
    // Existing user preference is reversible without changing OAuth state.
    fixture
        .db
        .call(move |conn| {
            exetrouter::account_preferences::set_user(conn, 1, second, Some(true), None)?;
            Ok(())
        })
        .await
        .unwrap();
    let mut socket = fixture.ws().await;
    let mut value = request();
    value["type"] = json!("response.create");
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    let before = fixture.mock.requests.lock().unwrap().len();
    fixture
        .db
        .call(move |conn| {
            exetrouter::account_preferences::set_policy(
                conn,
                second,
                Some(false),
                None,
                Some(true),
            )?;
            assert!(exetrouter::control(
                conn,
                &[1; 32],
                1,
                exetrouter::ControlRequest::AccountSet {
                    account: second,
                    enabled: Some(true),
                    priority: None
                }
            )
            .is_err());
            Ok(())
        })
        .await
        .unwrap();
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal(&mut socket).await["error"]["code"],
        "account_disabled"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), before);
    assert_eq!(
        fixture
            .upstream
            .select_for_user("gpt-test", None, Some(1))
            .await
            .unwrap()
            .info
            .id,
        1
    );
    fixture.stop().await;
}

#[tokio::test]
async fn turn_state_survives_http_continuation_and_pins_only_its_owner_account() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    let mut body = request();
    body["test_mode"] = json!("turn_state");
    body["stream"] = json!(true);
    let first = fixture.post(body.clone()).await;
    assert_eq!(first.status(), 200);
    let state = first.headers()["x-codex-turn-state"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(first.text().await.unwrap().contains("response.completed"));
    let client = reqwest::Client::new();
    let submit = |secret: String, value: String| {
        client
            .post(format!("{}/v1/responses", fixture.url))
            .bearer_auth(secret)
            .header("x-codex-turn-state", value)
            .json(&body)
            .send()
    };
    let second = submit(fixture.secret.clone(), state.clone()).await.unwrap();
    assert_eq!(second.status(), 200);
    assert!(second.text().await.unwrap().contains("response.completed"));
    assert_eq!(
        fixture.mock.turn_states.lock().unwrap().as_slice(),
        &[None, Some(state.clone())]
    );
    let accounts = fixture.mock.request_accounts.lock().unwrap().clone();
    assert_eq!(accounts[0], accounts[1]);
    let foreign = fixture
        .db
        .call(|conn| {
            let user = exetrouter::create_user(conn, "turn-state-other-user")?;
            Ok(exetrouter::create_token(conn, &[1; 32], user, "fixture", 1)?.secret)
        })
        .await
        .unwrap();
    for (secret, value) in [
        (fixture.secret.clone(), "unknown-private-state".to_owned()),
        (foreign, state.clone()),
    ] {
        let response = submit(secret, value.clone()).await.unwrap();
        assert_eq!(response.status(), 400);
        let error = response.text().await.unwrap();
        assert!(error.contains("context_not_found"));
        assert!(!error.contains(&value));
    }
    fixture
        .db
        .call(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM context_bindings WHERE length(digest)=32",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            conn.execute("UPDATE context_bindings SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        submit(fixture.secret.clone(), state)
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(fixture.rows().await.len(), 2);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn encrypted_function_arguments_preserve_tool_owner_and_reject_foreign_replay() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.mock.tools.store(true, Ordering::SeqCst);
    let mut body = request();
    body["test_mode"] = json!("encrypted_tools");
    body["tools"] =
        json!([{"type":"function","name":"exec_command","parameters":{"type":"object"}}]);
    body["stream"] = json!(false);
    let first = fixture
        .post(body.clone())
        .await
        .json::<Value>()
        .await
        .unwrap();
    let call = first["output"][0].clone();
    assert!(call["encrypted_function_args"].is_array());
    body["input"] = json!([call,{"type":"function_call_output","call_id":call["call_id"],"output":"synthetic result"}]);
    let second = fixture.post(body.clone()).await;
    assert_eq!(second.status(), 200);
    assert!(second.json::<Value>().await.unwrap()["output"].is_array());
    assert_eq!(
        fixture.mock.requests.lock().unwrap()[1]["input"][0]["encrypted_function_args"],
        call["encrypted_function_args"]
    );
    let accounts = fixture.mock.request_accounts.lock().unwrap().clone();
    assert_eq!(accounts[0], accounts[1]);
    let foreign = fixture
        .db
        .call(|conn| {
            let user = exetrouter::create_user(conn, "foreign-tool-owner")?;
            Ok(exetrouter::create_token(conn, &[1; 32], user, "fixture", 1)?.secret)
        })
        .await
        .unwrap();
    let reply = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(foreign)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 400);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "context_not_found"
    );
    assert_eq!(fixture.rows().await.len(), 2);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn quota_only_refusals_fail_over_but_accepted_or_ambiguous_requests_never_replay() {
    for mode in [
        "quota_error",
        "quota_http",
        "late_quota_error",
        "early_close",
        "disconnect",
    ] {
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        fixture.first_profile(&["gpt-test"], mode);
        let mut body = request();
        if mode == "quota_http" {
            fixture
                .mock
                .quota_headers
                .lock()
                .unwrap()
                .insert("x-codex-primary-used-percent", "20".parse().unwrap());
            fixture
                .mock
                .quota_headers
                .lock()
                .unwrap()
                .insert("x-codex-primary-window-minutes", "300".parse().unwrap());

            body["stream"] = json!(false);
            let response = fixture.post(body).await;
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["x-codex-primary-used-percent"], "20");

            assert_eq!(
                response.json::<Value>().await.unwrap()["status"],
                "completed"
            );
        } else {
            body["type"] = json!("response.create");
            let mut socket = fixture.ws().await;
            socket
                .send(Message::Text(body.to_string().into()))
                .await
                .unwrap();
            let response = terminal(&mut socket).await;
            assert_eq!(
                response["type"] == "response.completed",
                mode == "quota_error",
                "{mode}"
            );
        }
        let succeeds = matches!(mode, "quota_error" | "quota_http");
        assert_eq!(
            fixture.mock.requests.lock().unwrap().len(),
            if succeeds { 2 } else { 1 },
            "{mode}"
        );
        let rows = fixture.rows().await;
        assert_eq!(rows.len(), if succeeds { 2 } else { 1 });
        if succeeds {
            assert_eq!(rows[1]["status"], "completed");
            assert_eq!(
                *fixture.mock.request_accounts.lock().unwrap(),
                ["upstream-account", "upstream-account-2"]
            );
        }
        fixture.stop().await;
    }
}

#[tokio::test]
async fn quota_refusal_recovers_incremental_tool_result_on_the_same_client_socket() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.first_profile(&["gpt-test"], "quota_after_tool");
    fixture.mock.tools.store(true, Ordering::SeqCst);
    let mut socket = fixture.ws().await;
    let mut body = request();
    body["tools"] =
        json!([{ "type":"function", "name":"exec_command", "parameters":{"type":"object"} }]);
    body["type"] = json!("response.create");
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let first = terminal(&mut socket).await;
    let call = first["response"]["output"][0].clone();
    assert_eq!(call["type"], "function_call");
    body["previous_response_id"] = first["response"]["id"].clone();
    body["input"] = json!([{"type":"function_call_output","call_id":call["call_id"],"output":"synthetic result"}]);
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let response = terminal(&mut socket).await;
    assert_eq!(response["type"], "response.completed");
    let requests = fixture.mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].get("previous_response_id").is_some());
    assert!(requests[2].get("previous_response_id").is_none());
    assert_eq!(
        requests[2]["input"],
        json!([requests[0]["input"][0], call, body["input"][0]])
    );
    assert_eq!(
        requests[0]["prompt_cache_key"],
        requests[2]["prompt_cache_key"]
    );
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account", "upstream-account", "upstream-account-2"]
    );
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["status"], "completed");
    assert_eq!(rows[1]["status"], "failed");
    assert_eq!(rows[2]["status"], "completed");
    fixture.stop().await;
}

#[tokio::test]
async fn exhausted_subscription_moves_known_checkpoint_without_rewriting_it_or_expiry() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    let checkpoint = fixture
        .compact(json!({"model":"gpt-test","input":[{"role":"user","content":"synthetic canary"}]}))
        .await
        .json::<Value>()
        .await
        .unwrap()["output"][0]
        .clone();
    let expiry = fixture
        .db
        .call(|conn| {
            Ok(conn.query_row(
                "SELECT expires_at FROM context_bindings LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    fixture.db.call(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(1,'primary',100,300,?1,?2,0)",rusqlite::params![now+300,now])?;
        Ok(())
    }).await.unwrap();
    let mut body = request();
    body["stream"] = json!(false);
    body["input"] = json!([checkpoint,{"role":"user","content":"continue"}]);
    assert_eq!(fixture.post(body).await.status(), 200);
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        ["upstream-account-2"]
    );
    assert_eq!(
        fixture.mock.requests.lock().unwrap()[0]["input"][0],
        checkpoint
    );
    fixture
        .db
        .call(move |conn| {
            let (owner, actual): (i64, i64) = conn.query_row(
                "SELECT account_id,expires_at FROM context_bindings LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!((owner, actual), (2, expiry));
            Ok(())
        })
        .await
        .unwrap();
    fixture.stop().await;
}

#[tokio::test]
#[ignore = "requires reviewed native binaries; synthetic quota refusal, no real upstream"]
async fn current_clients_continue_tools_across_a_quota_account_switch() {
    let codex = std::env::var("EXETROUTER_CODEX_BIN").expect("EXETROUTER_CODEX_BIN");
    let opencode = std::env::var("EXETROUTER_OPENCODE_BIN").expect("EXETROUTER_OPENCODE_BIN");
    for (client, websocket) in [
        ("codex", false),
        ("codex", true),
        ("opencode", false),
        ("opencode", true),
    ] {
        let binary = if client == "codex" { &codex } else { &opencode };
        native::version(binary, &native::reviewed_version(client))
            .await
            .unwrap();
        let fixture = Fixture::start(false).await;
        fixture.add_account(&["gpt-test"], false).await;
        fixture.first_profile(
            &["gpt-test"],
            if websocket {
                "quota_after_tool"
            } else {
                "quota_http_after_tool"
            },
        );
        fixture
            .db
            .call(|conn| {
                exetrouter::account_preferences::set_user(conn, 1, 1, None, Some(10))?;
                Ok(())
            })
            .await
            .unwrap();
        fixture.mock.tools.store(true, Ordering::SeqCst);
        let probe = probe::Probe::bounded(&fixture.url, 12).await.unwrap();
        let output = native::Run {
            binary,
            client,
            url: &probe.url,
            model: "gpt-test",
            bearer: &fixture.secret,
            websocket,
            directory: fixture.dir.path(),
            prompt: "Run the local compatibility fixture and report the result.",
        }
        .execute()
        .await
        .unwrap();
        assert!(
            output.status.success()
                && native::tool_result(&output.stdout)
                && String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK"),
            "{client} ws={websocket}: {}",
            native::diagnostic(&output.stdout)
        );
        let calls = fixture.mock.requests.lock().unwrap().clone();
        let accounts = fixture.mock.request_accounts.lock().unwrap().clone();
        let primary = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| {
                call["input"].as_array().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item["type"] == "function_call_output")
                })
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(
            primary.len(),
            2,
            "exactly one rejected result and one accepted continuation"
        );
        assert_eq!(accounts[primary[0]], "upstream-account");
        assert_eq!(accounts[primary[1]], "upstream-account-2");
        assert!(calls[primary[1]].get("previous_response_id").is_none());
        assert!(calls[primary[1]]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call"));
        let rows = fixture.rows().await;
        assert_eq!(
            rows.iter()
                .filter(|row| row["status"] != "completed")
                .count(),
            1
        );
        assert_eq!(rows.last().unwrap()["status"], "completed");
        println!(
            "{}",
            json!({"check":"native_quota_switch","client":client,"websocket":websocket,"requests":rows.len(),"accepted_continuations":1})
        );
        drop(probe);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn lite_quota_transfer_preserves_prefix_encrypted_tool_state_and_new_account_limits() {
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.mock.tools.store(true, Ordering::SeqCst);
    let prefix = json!({"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"exec_command","parameters":{"type":"object"}}]}]});
    let mut body = request();
    body.as_object_mut().unwrap().remove("tools");
    body["type"] = json!("response.create");
    body["test_mode"] = json!("encrypted_tools");
    body["input"] = json!([prefix,{"role":"user","content":"synthetic tool fixture"}]);
    let mut socket = fixture.ws().await;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let first = terminal(&mut socket).await;
    let call = first["response"]["output"][0].clone();
    assert!(call["encrypted_function_args"].is_array());
    fixture.db.call(|conn| {
        let now=chrono::Utc::now().timestamp();
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(1,'primary',100,300,?1,?2,0)",rusqlite::params![now+300,now])?;
        Ok(())
    }).await.unwrap();
    *fixture.mock.quota_event.lock().unwrap() = Some(
        json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":20,"window_minutes":300,"reset_at":chrono::Utc::now().timestamp()+300}}}),
    );
    body["previous_response_id"] = first["response"]["id"].clone();
    body["input"] = json!([{"type":"function_call_output","call_id":call["call_id"],"output":"synthetic result"}]);
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let mut observed_limit = false;
    loop {
        let event: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        observed_limit |= event["type"] == "codex.rate_limits"
            && event["rate_limits"]["primary"]["used_percent"] == 20;
        assert_ne!(event["type"], "error");
        if event["type"] == "response.completed" {
            body["previous_response_id"] = event["response"]["id"].clone();
            break;
        }
    }
    assert!(observed_limit);
    let forwarded = fixture.mock.requests.lock().unwrap().clone();
    assert_eq!(forwarded[1]["input"][0], prefix);
    assert_eq!(forwarded[1]["input"][2], call);
    assert!(forwarded[1].get("previous_response_id").is_none());
    assert_eq!(
        forwarded[0]["prompt_cache_key"],
        forwarded[1]["prompt_cache_key"]
    );
    body["input"] = json!([{"role":"user","content":"continue"}]);
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(
        *fixture.mock.request_accounts.lock().unwrap(),
        [
            "upstream-account",
            "upstream-account-2",
            "upstream-account-2"
        ]
    );
    fixture.stop().await;
}

#[tokio::test]
#[ignore = "requires exact reviewed OpenCode V1 binary; synthetic upstream only"]
async fn opencode_v1_http_compatibility_probe() {
    check_opencode_v1_http("opencode-v1").await;
}

async fn check_opencode_v1_http(client: &str) {
    let binary = std::env::var("EXETROUTER_OPENCODE_V1_BIN").unwrap();
    native::version(&binary, &native::reviewed_version("opencode-v1"))
        .await
        .unwrap();
    let fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    fixture.mock.tools.store(true, Ordering::SeqCst);
    let probe = probe::Probe::bounded(&fixture.url, 12).await.unwrap();
    let run = native::Run {
        binary: &binary,
        client,
        url: &probe.url,
        model: "gpt-test",
        bearer: &fixture.secret,
        websocket: false,
        directory: fixture.dir.path(),
        prompt: "Run the local compatibility fixture and report the result.",
    };
    let output = run.execute().await.unwrap();
    println!(
        "{client}: status={} diagnostic={} metadata={} stderr_bytes={}",
        output.status,
        native::diagnostic(&output.stdout),
        probe.observed.lock().unwrap().metadata(),
        output.stderr.len()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXETROUTER_SMOKE_OK") && native::tool_result(&output.stdout),
        "{}",
        stdout
    );
    assert!(fixture
        .mock
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["input"].as_array().is_some_and(|input| input
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["output"].to_string().contains("EXETROUTER_TOOL_OK")))));
    let output = run
        .continued(&native::Continuation {
            token_limit: 512,
            resume: true,
        })
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(fixture
        .rows()
        .await
        .iter()
        .all(|row| row["status"] == "completed"));
    println!("{client}: resumed session passed");
    fixture.stop().await;
}

#[tokio::test]
async fn saved_conversations_are_user_owned_pinned_and_never_migrate_on_quota() {
    let mut fixture = Fixture::start(false).await;
    fixture.add_account(&["gpt-test"], false).await;
    *fixture.mock.mode.lock().unwrap() = "saved_conversation".into();
    let mut initial = request();
    initial["stream"] = json!(false);
    let reply = fixture.post(initial).await;
    assert_eq!(reply.status(), 200);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["conversation"]["id"],
        "conv_synthetic_owned"
    );
    let first = fixture.mock.request_accounts.lock().unwrap()[0].clone();
    for conversation in [
        json!("conv_synthetic_owned"),
        json!({"id":"conv_synthetic_owned"}),
    ] {
        let mut body = request();
        body["stream"] = json!(false);
        body["conversation"] = conversation.clone();
        let reply = fixture.post(body.clone()).await;
        assert_eq!(reply.status(), 200);
        assert_eq!(
            fixture
                .mock
                .request_accounts
                .lock()
                .unwrap()
                .last()
                .unwrap(),
            &first
        );
        assert_eq!(
            fixture.mock.requests.lock().unwrap().last().unwrap()["conversation"],
            conversation
        );
    }
    let mut unknown = request();
    unknown["conversation"] = json!("conv_foreign");
    let count = fixture.mock.requests.lock().unwrap().len();
    assert_eq!(fixture.post(unknown.clone()).await.status(), 400);
    let mut ws = fixture.ws().await;
    unknown["type"] = json!("response.create");
    ws.send(Message::Text(unknown.to_string().into()))
        .await
        .unwrap();
    let error: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(error["error"]["code"], "context_not_found");
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), count);
    ws.close(None).await.unwrap();
    let foreign_token = fixture
        .db
        .call(|conn| {
            let user = exetrouter::create_user(conn, "foreign-user")?;
            Ok(exetrouter::create_token(conn, &[1; 32], user, "foreign-device", 1)?.secret)
        })
        .await
        .unwrap();
    let mut stolen = request();
    stolen["conversation"] = json!("conv_synthetic_owned");
    let rejected = reqwest::Client::new()
        .post(format!("{}/v1/responses", fixture.url))
        .bearer_auth(foreign_token)
        .json(&stolen)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 400);
    assert_eq!(
        rejected.json::<Value>().await.unwrap()["error"]["code"],
        "context_not_found"
    );
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), count);
    fixture.restart().await;
    let mut owned = request();
    owned["stream"] = json!(false);
    owned["conversation"] = json!("conv_synthetic_owned");
    assert_eq!(fixture.post(owned.clone()).await.status(), 200);
    let mut socket = fixture.ws().await;
    let mut frame = owned.clone();
    frame["type"] = json!("response.create");
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut socket).await["type"], "response.completed");
    assert_eq!(
        fixture
            .mock
            .request_accounts
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        &first
    );
    socket.close(None).await.unwrap();
    // Synthetic pre-generation refusal must not send backend-owned history elsewhere.
    *fixture.mock.mode.lock().unwrap() = "quota_http".into();
    let count = fixture.mock.requests.lock().unwrap().len();
    assert_eq!(fixture.post(owned).await.status(), 429);
    assert_eq!(fixture.mock.requests.lock().unwrap().len(), count + 1);
    assert_eq!(
        fixture
            .mock
            .request_accounts
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        &first
    );
    fixture.stop().await;
}
