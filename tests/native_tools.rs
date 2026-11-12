//! Offline contracts from Codex 0.165.0; never uses ChatGPT or real credentials.
#[path = "support/mock.rs"]
mod mock;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use exetrouter::{
    oauth::{self, Credentials, Vault},
    server::{self, ServeConfig},
    store::Database,
    upstream::{self, Upstream},
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{oneshot, Notify},
    task::JoinHandle,
};

struct Native {
    calls: Mutex<Vec<(String, HeaderMap, Value)>>,
    reply: Mutex<(StatusCode, HeaderMap, Value)>,
    wait: AtomicBool,
    started: Notify,
    release: Notify,
}
impl Default for Native {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: Mutex::new((
                StatusCode::OK,
                HeaderMap::new(),
                json!({"created":123,"data":[{"b64_json":"aW1hZ2U=","generation_id":"fixture-generation"}]}),
            )),
            wait: AtomicBool::new(false),
            started: Notify::new(),
            release: Notify::new(),
        }
    }
}
async fn generate(
    State(state): State<Arc<Native>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    native(state, "images/generations", headers, body).await
}
async fn edit(
    State(state): State<Arc<Native>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    native(state, "images/edits", headers, body).await
}
async fn search(
    State(state): State<Arc<Native>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    native(state, "alpha/search", headers, body).await
}
async fn native(state: Arc<Native>, path: &str, headers: HeaderMap, body: Value) -> Response {
    state
        .calls
        .lock()
        .unwrap()
        .push((path.into(), headers, body));
    state.started.notify_one();
    if state.wait.load(Ordering::SeqCst) {
        state.release.notified().await;
    }
    let (status, headers, value) = state.reply.lock().unwrap().clone();
    (status, headers, Json(value)).into_response()
}

struct Fixture {
    dir: TempDir,
    db: Database,
    upstream: Arc<Upstream>,
    native: Arc<Native>,
    mock: Arc<mock::Mock>,
    issuer: String,
    url: String,
    secret: String,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<exetrouter::Result<()>>,
    backend: JoinHandle<()>,
}
impl Fixture {
    fn mcp(&self, body: &Value) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .post(format!("{}/mcp", self.url))
            .bearer_auth(&self.secret)
            .header("accept", "application/json, text/event-stream")
            .json(body)
    }
    async fn initialize_mcp(&self) -> String {
        let response=self.mcp(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(
            value["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        assert!(value["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("generate_image"));
        session
    }
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("state.sqlite"))
            .await
            .unwrap();
        let secret = db
            .call(|conn| {
                let user = exetrouter::create_user(conn, "alice")?;
                let token = exetrouter::create_token(conn, &[1; 32], user, "fixture", 1)?;
                oauth::save(
                    conn,
                    &Vault::new([2; 32]),
                    "upstream-account",
                    &Credentials {
                        access_token: "upstream-access".into(),
                        refresh_token: "upstream-refresh".into(),
                    },
                    chrono::Utc::now().timestamp() + 3600,
                )?;
                Ok(token.secret)
            })
            .await
            .unwrap();
        let native = Arc::new(Native::default());
        let mock = Arc::new(mock::Mock::default());
        mock.accounts.lock().unwrap().insert(
            "upstream-account".into(),
            mock::Account::new("upstream-access", "upstream-refresh", &["a"]),
        );
        let app = mock::router(mock.clone()).merge(
            Router::new()
                .route("/images/generations", post(generate))
                .route("/images/edits", post(edit))
                .route("/alpha/search", post(search))
                .layer(axum::extract::DefaultBodyLimit::disable())
                .with_state(native.clone()),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let backend = tokio::spawn(async move {
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}");
        let (stop, stopped) = oneshot::channel();
        let config = ServeConfig {
            account_maintenance: false,
            gateway_uid: unsafe { libc::geteuid() },
            listen: address,
            socket: dir.path().join("control.sock"),
            upstream: Some(upstream.clone()),
            generations_per_user: 8,
            websockets_per_user: 8,
            fast_mode: false,
        };
        let server = tokio::spawn(server::serve(db.clone(), vec![1; 32], config, async {
            let _ = stopped.await;
        }));
        let fixture = Self {
            dir,
            db,
            upstream,
            native,
            mock,
            issuer,
            url,
            secret,
            stop: Some(stop),
            server,
            backend,
        };
        fixture.ready().await;
        fixture
    }
    async fn ready(&self) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if client
                .get(format!("{}/healthz", self.url))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            assert!(
                !self.server.is_finished(),
                "fixture server stopped before becoming ready"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("server did not become ready");
    }
    fn request(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .post(format!("{}/v1/{path}", self.url))
            .bearer_auth(&self.secret)
            .json(body)
    }
    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.request(path, &body).send().await.unwrap()
    }
    fn reply(&self, status: StatusCode, headers: HeaderMap, value: Value) {
        *self.native.reply.lock().unwrap() = (status, headers, value);
    }
    async fn rows(&self) -> Vec<Value> {
        self.db.call(|conn| {
            let mut stmt = conn.prepare("SELECT status,model,api_surface,account_id,input_tokens,output_tokens FROM usage_events ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok(json!({"status":r.get::<_,String>(0)?,"model":r.get::<_,String>(1)?,"surface":r.get::<_,String>(2)?,"account":r.get::<_,i64>(3)?,"input":r.get::<_,Option<i64>>(4)?,"output":r.get::<_,Option<i64>>(5)?})))?.collect::<Result<Vec<_>,_>>()?;
            Ok(rows)
        }).await.unwrap()
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
            account_maintenance: false,
            gateway_uid: unsafe { libc::geteuid() },
            listen: self.url.strip_prefix("http://").unwrap().parse().unwrap(),
            socket: self.dir.path().join("control.sock"),
            upstream: Some(self.upstream.clone()),
            generations_per_user: 8,
            websockets_per_user: 8,
            fast_mode: false,
        };
        self.server = tokio::spawn(server::serve(self.db.clone(), vec![1; 32], config, async {
            let _ = stopped.await;
        }));
        self.ready().await;
    }
}

const PNG: &str="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==";
fn mcp_call(id: i64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

#[tokio::test]
async fn mcp_official_sdk_discovers_and_calls_images_when_installed() {
    let Ok(sdk) = std::env::var("EXR_MCP_SDK_ROOT") else {
        return;
    };
    let fixture = Fixture::start().await;
    fixture.reply(
        StatusCode::OK,
        HeaderMap::new(),
        json!({"created":123,"data":[{"b64_json":PNG}]}),
    );
    let mut command = tokio::process::Command::new("node");
    command
        .args([
            "--input-type=module",
            "-e",
            include_str!("support/mcp-sdk.mjs"),
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("EXR_MCP_SDK_ROOT", sdk)
        .env("EXR_MCP_TEST_URL", format!("{}/mcp", fixture.url))
        .env("EXR_MCP_TEST_TOKEN", &fixture.secret)
        .current_dir(fixture.dir.path())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .expect("SDK fixture timed out")
        .expect("SDK fixture could not start");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 2);
    assert_eq!(fixture.rows().await.len(), 2);
}

#[tokio::test]
async fn codex_0165_discovers_mcp_image_tools_without_inference_when_installed() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let Ok(binary) = std::env::var("EXR_MCP_CODEX_BIN") else {
        return;
    };
    let version = tokio::process::Command::new(&binary)
        .arg("--version")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "codex-cli 0.165.0",
        "Use the reviewed isolated Codex version"
    );
    let fixture = Fixture::start().await;
    let home = fixture.dir.path().join("codex-home");
    std::fs::create_dir(&home).unwrap();
    for modern in [false, true] {
        let config = format!(
            r#"model = "a"
model_provider = "exetrouter"
[features]
api_key_model_discovery = true
mcp_2026_07_28 = {modern}
[model_providers.exetrouter]
name = "ExetRouter fixture"
base_url = "{}/v1"
model_catalog_url = "{}/v1/models/codex"
wire_api = "responses"
env_key = "EXR_MCP_TEST_TOKEN"
[mcp_servers.exetrouter]
url = "{}/mcp"
bearer_token_env_var = "EXR_MCP_TEST_TOKEN"
startup_timeout_sec = 10
[analytics]
enabled = false
[feedback]
enabled = false
"#,
            fixture.url, fixture.url, fixture.url
        );
        std::fs::write(home.join("config.toml"), config).unwrap();
        let mut command = tokio::process::Command::new(&binary);
        command
            .arg("app-server")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("CODEX_HOME", &home)
            .env("EXR_MCP_TEST_TOKEN", &fixture.secret)
            .current_dir(fixture.dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let messages = [
            json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"exr_mcp_fixture","version":"1"},"capabilities":{"experimentalApi":true}}}),
            json!({"method":"initialized"}),
            json!({"id":2,"method":"mcpServerStatus/list","params":{"serverName":"exetrouter"}}),
        ];
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            for message in messages {
                input
                    .write_all(format!("{message}\n").as_bytes())
                    .await
                    .unwrap();
                input.flush().await.unwrap();
            }
            loop {
                let mut line = String::new();
                assert!(
                    output.read_line(&mut line).await.unwrap() > 0,
                    "Codex app-server closed"
                );
                assert!(line.len() < 1024 * 1024);
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["id"] == 2 {
                    return value;
                }
            }
        })
        .await;
        child.kill().await.ok();
        child.wait().await.ok();
        let result = result.expect("Codex MCP discovery timed out");
        assert!(result.get("error").is_none(), "Codex MCP discovery failed");
        let servers = result["result"]["data"]
            .as_array()
            .expect("Codex MCP status page");
        let server = servers
            .iter()
            .find(|server| server["name"] == "exetrouter")
            .expect("ExetRouter MCP server");
        assert!(
            server["tools"].get("generate_image").is_some(),
            "Codex did not discover generate_image (modern={modern})"
        );
        assert!(
            server["tools"].get("edit_image").is_some(),
            "Codex did not discover edit_image"
        );
    }
    assert!(fixture.native.calls.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
}
async fn mcp_value(response: reqwest::Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    if response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream")
    {
        let text = response.text().await.unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("MCP response event");
        serde_json::from_str(data).unwrap()
    } else {
        response.json().await.unwrap()
    }
}
fn modern_request(method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"fixture","version":"1"}});
    json!({"jsonrpc":"2.0","id":7,"method":method,"params":params})
}

#[tokio::test]
async fn mcp_discovers_images_without_inference_or_upstream_credentials() {
    let fixture = Fixture::start().await;
    let session = fixture.initialize_mcp().await;
    assert_eq!(
        fixture
            .mcp(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let value = mcp_value(
        fixture
            .mcp(&json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}))
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let tools = value["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["generate_image", "edit_image"]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool["annotations"]["idempotentHint"], false);
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
    }
    assert!(fixture.native.calls.lock().unwrap().is_empty());
    assert!(fixture.mock.catalog_requests.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
    assert!(!value.to_string().contains("upstream-access"));
}

#[tokio::test]
async fn mcp_generate_and_edit_return_actual_image_blocks_with_shared_native_accounting() {
    let fixture = Fixture::start().await;
    fixture.reply(StatusCode::OK,HeaderMap::new(),json!({"created":123,"data":[{"b64_json":PNG}],"usage":{"input_tokens":10,"output_tokens":20}}));
    let session = fixture.initialize_mcp().await;
    for (id, name, args, path) in [
        (
            1,
            "generate_image",
            json!({"prompt":"synthetic-private-mcp-prompt","quality":"high"}),
            "images/generations",
        ),
        (
            2,
            "edit_image",
            json!({"prompt":"synthetic-private-mcp-edit","image_urls":[format!("data:image/png;base64,{PNG}"),"https://example.com/image.png"]}),
            "images/edits",
        ),
    ] {
        let response = fixture
            .mcp(&mcp_call(id, name, args))
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let value = mcp_value(response).await;
        assert_eq!(value["id"], id);
        assert_eq!(value["result"]["isError"], false);
        assert_eq!(
            value["result"]["content"][0],
            json!({"type":"image","data":PNG,"mimeType":"image/png"})
        );
        let calls = fixture.native.calls.lock().unwrap();
        let last = calls.last().unwrap();
        assert_eq!(last.0, path);
        assert_eq!(last.2["model"], "gpt-image-2");
        assert!(!last.1.contains_key("mcp-session-id"));
    }
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["surface"], "mcp/generate_image");
    assert_eq!(rows[1]["surface"], "mcp/edit_image");
    assert_eq!(rows[0]["output"], 20);
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    for entry in std::fs::read_dir(fixture.dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            for marker in [
                "synthetic-private-mcp-prompt",
                "synthetic-private-mcp-edit",
                PNG,
            ] {
                assert!(!bytes.windows(marker.len()).any(|w| w == marker.as_bytes()));
            }
        }
    }
}

#[tokio::test]
async fn modern_mcp_discovery_and_calls_validate_metadata_and_do_not_require_a_session() {
    let fixture = Fixture::start().await;
    for method in ["server/discover", "tools/list", "ping"] {
        let value = mcp_value(
            fixture
                .mcp(&modern_request(method, json!({})))
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", method)
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(value["result"]["resultType"], "complete");
        if method != "ping" {
            assert_eq!(value["result"]["ttlMs"], 0);
            assert_eq!(value["result"]["cacheScope"], "private");
        }
        assert_eq!(
            value["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "exetrouter"
        );
    }
    fixture.reply(
        StatusCode::OK,
        HeaderMap::new(),
        json!({"created":123,"data":[{"b64_json":PNG}]}),
    );
    let body = modern_request(
        "tools/call",
        json!({"name":"generate_image","arguments":{"prompt":"synthetic-private-modern-prompt"}}),
    );
    let value = mcp_value(
        fixture
            .mcp(&body)
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", "tools/call")
            .header("mcp-name", "=?base64?Z2VuZXJhdGVfaW1hZ2U=?=")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["result"]["resultType"], "complete");
    assert_eq!(value["result"]["content"][0]["data"], PNG);
    let response = fixture
        .mcp(&body)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/list")
        .header("mcp-name", "generate_image")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        -32020
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn mcp_rejects_invalid_calls_before_usage_and_honors_read_only_context() {
    let fixture = Fixture::start().await;
    let session = fixture.initialize_mcp().await;
    for args in [
        json!({"prompt":""}),
        json!({"prompt":"x","model":"arbitrary"}),
        json!({"prompt":"x","quality":"invalid"}),
        json!({"prompt":"x","output":"/private/server-file"}),
    ] {
        let value = mcp_value(
            fixture
                .mcp(&mcp_call(1, "generate_image", args))
                .header("mcp-session-id", &session)
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(value["result"]["isError"], true);
    }
    let value = mcp_value(
        fixture
            .mcp(&mcp_call(
                1,
                "edit_image",
                json!({"prompt":"x","image_urls":["file:///private/server-file"]}),
            ))
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["result"]["isError"], true);
    let value = mcp_value(
        fixture
            .mcp(&mcp_call(1, "unknown-private-tool", json!({})))
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["error"]["code"], -32602);
    assert!(!value.to_string().contains("unknown-private-tool"));
    let value=mcp_value(fixture.mcp(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"openai/readOnly":true}}})).header("mcp-session-id",&session).send().await.unwrap()).await;
    assert_eq!(value["result"]["tools"], json!([]));
    let mut body = mcp_call(1, "generate_image", json!({"prompt":"x"}));
    body["params"]["_meta"] = json!({"openai/readOnly":true});
    let value = mcp_value(
        fixture
            .mcp(&body)
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["result"]["isError"], true);
    assert!(fixture.native.calls.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn mcp_authentication_origins_protocol_and_methods_fail_closed() {
    let fixture = Fixture::start().await;
    let session = fixture.initialize_mcp().await;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/mcp", fixture.url))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .mcp(&body)
            .header("origin", "https://untrusted.example")
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut invalid = fixture
        .mcp(&body)
        .header("mcp-session-id", &session)
        .build()
        .unwrap();
    invalid
        .headers_mut()
        .insert("accept", "application/json".parse().unwrap());
    assert_eq!(
        reqwest::Client::new()
            .execute(invalid)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_ACCEPTABLE
    );
    assert_eq!(
        fixture.mcp(&body).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture
            .mcp(&body)
            .header("mcp-session-id", "forged-session")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .mcp(&body)
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", "1900-01-01")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    for method in [reqwest::Method::GET, reqwest::Method::DELETE] {
        assert_eq!(
            reqwest::Client::new()
                .request(method, format!("{}/mcp", fixture.url))
                .bearer_auth(&fixture.secret)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    assert!(fixture.native.calls.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn mcp_errors_do_not_echo_upstream_payload_or_claim_success_or_replay() {
    for (status, reply) in [
        (
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error":{"message":"private-mcp-upstream-error"}}),
        ),
        (
            StatusCode::OK,
            json!({"created":123,"data":[{"b64_json":"aW1hZ2U="}],"usage":{"input_tokens":7}}),
        ),
    ] {
        let fixture = Fixture::start().await;
        let session = fixture.initialize_mcp().await;
        fixture.reply(status, HeaderMap::new(), reply);
        let value = mcp_value(
            fixture
                .mcp(&mcp_call(1, "generate_image", json!({"prompt":"x"})))
                .header("mcp-session-id", &session)
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(value["result"]["isError"], true);
        assert!(!value.to_string().contains("private-mcp-upstream-error"));
        assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
        let rows = fixture.rows().await;
        assert_ne!(rows[0]["status"], "completed");
        if status == StatusCode::OK {
            assert_eq!(rows[0]["input"], 7);
        }
    }
}

async fn wait_interrupted(fixture: &Fixture) {
    for _ in 0..100 {
        if fixture
            .rows()
            .await
            .iter()
            .any(|r| r["status"] == "interrupted")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("MCP cancellation did not finalize usage");
}

#[tokio::test]
async fn mcp_disconnect_cancels_upstream_wait_and_releases_native_capacity() {
    let fixture = Fixture::start().await;
    let session = fixture.initialize_mcp().await;
    fixture.native.wait.store(true, Ordering::SeqCst);
    let response = fixture
        .mcp(&mcp_call(1, "generate_image", json!({"prompt":"x"})))
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), fixture.native.started.notified())
        .await
        .unwrap();
    drop(response);
    wait_interrupted(&fixture).await;
    fixture.native.wait.store(false, Ordering::SeqCst);
    fixture.reply(
        StatusCode::OK,
        HeaderMap::new(),
        json!({"created":123,"data":[{"b64_json":PNG}]}),
    );
    let value = mcp_value(
        fixture
            .mcp(&mcp_call(
                2,
                "generate_image",
                json!({"prompt":"separate new work"}),
            ))
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["result"]["isError"], false);
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn legacy_mcp_cancel_ids_are_scoped_to_user_and_issued_client_session() {
    let fixture = Fixture::start().await;
    let session = fixture.initialize_mcp().await;
    let other = fixture.initialize_mcp().await;
    fixture.native.wait.store(true, Ordering::SeqCst);
    let response = fixture
        .mcp(&mcp_call(1, "generate_image", json!({"prompt":"x"})))
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), fixture.native.started.notified())
        .await
        .unwrap();
    let cancel = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"reason":"private-cancel-reason"}});
    assert_eq!(
        fixture
            .mcp(&cancel)
            .header("mcp-session-id", &other)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(fixture.rows().await[0]["status"], "accepted");
    let bob = fixture
        .db
        .call(|conn| {
            let user = exetrouter::create_user(conn, "bob")?;
            Ok(exetrouter::create_token(conn, &[1; 32], user, "fixture", 1)?.secret)
        })
        .await
        .unwrap();
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/mcp", fixture.url))
            .bearer_auth(bob)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .json(&cancel)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .mcp(&cancel)
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    wait_interrupted(&fixture).await;
    drop(response);
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn mcp_sessions_survive_router_restart_but_do_not_bypass_bearer_revocation() {
    let mut fixture = Fixture::start().await;
    let session = fixture.initialize_mcp().await;
    fixture.restart().await;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    assert_eq!(
        fixture
            .mcp(&body)
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE access_tokens SET revoked_at=?1",
                [chrono::Utc::now().timestamp()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .mcp(&body)
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(fixture.native.calls.lock().unwrap().is_empty());
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        self.backend.abort();
    }
}
fn image() -> Value {
    json!({"model":"gpt-image-2","prompt":"synthetic-private-image-prompt","quality":"auto","size":"auto","background":"transparent"})
}
fn query(id: &str) -> Value {
    json!({"id":id,"model":"a","commands":{"search_query":[{"q":"synthetic-private-search-query"}],"response_length":"short"}})
}
fn search_reply() -> Value {
    json!({"output":"synthetic-private-page-output","results":[],"encrypted_output":"unbound-opaque-value"})
}

#[tokio::test]
async fn image_authentication_refresh_belongs_to_a_separate_request() {
    let fixture = Fixture::start().await;
    fixture.reply(
        StatusCode::UNAUTHORIZED,
        HeaderMap::new(),
        json!({"error":{"message":"private-auth-error"}}),
    );
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 0);
    fixture.reply(
        StatusCode::OK,
        HeaderMap::new(),
        json!({"created":123,"data":[{"b64_json":"aW1hZ2U="}]}),
    );
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::OK
    );
    assert_eq!(fixture.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn feature_operational_backoff_is_not_reported_as_quota_exhaustion() {
    let fixture = Fixture::start().await;
    fixture.reply(
        StatusCode::INTERNAL_SERVER_ERROR,
        HeaderMap::new(),
        json!({"error":{"message":"private-server-error"}}),
    );
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let response = fixture.post("images/generations", image()).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["type"],
        "upstream_backoff"
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
    assert!(fixture.upstream.select("a").await.is_ok());
}

#[tokio::test]
async fn search_refusal_does_not_pause_images_or_move_an_existing_session() {
    let fixture = Fixture::start().await;
    fixture.reply(
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after".parse().unwrap(), "60".parse().unwrap())]
            .into_iter()
            .collect(),
        json!({"error":{"message":"private-limit-error"}}),
    );
    assert_eq!(
        fixture
            .post("alpha/search", query("session"))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        fixture
            .post("alpha/search", query("session"))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
    fixture.reply(
        StatusCode::OK,
        HeaderMap::new(),
        json!({"created":123,"data":[{"b64_json":"aW1hZ2U="}]}),
    );
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::OK
    );
    assert!(fixture.upstream.select("a").await.is_ok());
}

#[tokio::test]
async fn native_images_forward_exactly_without_catalog_or_host_model_and_account_unknown_usage() {
    let fixture = Fixture::start().await;
    let response = fixture.request("images/generations", &image())
        .header("chatgpt-account-id","caller-must-not-select")
        .header("x-unreviewed-secret","must-not-forward")
        .header("x-codex-image-turn-id","raw-image-turn")
        .header("x-codex-turn-metadata",json!({"session_id":"raw-session","thread_id":"raw-thread","tool_namespaces_info":{"tools":["imagegen"]}}).to_string())
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap()["data"][0]["b64_json"],
        "aW1hZ2U="
    );
    let calls = fixture.native.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "images/generations");
    assert_eq!(calls[0].2, image());
    assert_eq!(calls[0].1["authorization"], "Bearer upstream-access");
    assert_eq!(calls[0].1["chatgpt-account-id"], "upstream-account");
    assert!(!calls[0].1.contains_key("x-unreviewed-secret"));
    assert_eq!(
        calls[0].1["x-codex-image-turn-id"].to_str().unwrap().len(),
        64
    );
    assert!(!calls[0].1["x-codex-turn-metadata"]
        .to_str()
        .unwrap()
        .contains("raw-"));
    assert!(fixture.mock.catalog_requests.lock().unwrap().is_empty());
    assert!(fixture.mock.requests.lock().unwrap().is_empty());
    assert_eq!(
        fixture.rows().await,
        vec![
            json!({"status":"completed","model":"gpt-image-2","surface":"images/generations","account":1,"input":null,"output":null})
        ]
    );
}

#[tokio::test]
async fn native_edit_keeps_inline_and_url_images_and_accepts_large_unary_output() {
    let fixture = Fixture::start().await;
    let mut body = image();
    body["images"] = json!([{"image_url":"data:image/png;base64,aW1hZ2U="},{"image_url":"https://example.com/fixture.png"}]);
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(vec![7; 1024 * 1024 + 10]);
    fixture.reply(StatusCode::OK,HeaderMap::new(),json!({"created":123,"data":[{"b64_json":encoded}],"usage":{"input_tokens":10,"output_tokens":20}}));
    let response = fixture.post("images/edits", body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["data"][0]["b64_json"],
        encoded
    );
    assert_eq!(fixture.native.calls.lock().unwrap()[0].2, body);
    assert_eq!(fixture.rows().await[0]["output"], 20);
}

#[tokio::test]
async fn validation_and_bearer_rejection_submit_nothing_and_do_not_record_payloads() {
    let fixture = Fixture::start().await;
    for (path, body) in [
        (
            "images/generations",
            json!({"model":"gpt-image-2","prompt":"x","stream":true}),
        ),
        (
            "images/generations",
            json!({"model":"gpt-image-2","prompt":"x","n":2}),
        ),
        (
            "images/edits",
            json!({"model":"gpt-image-2","prompt":"x","images":[{"file_id":"foreign-file"}]}),
        ),
        (
            "alpha/search",
            json!({"id":"s","model":"a","commands":{},"input":[{"encrypted_content":"unwrapped-foreign-value"}]}),
        ),
        (
            "alpha/search",
            json!({"id":"s","model":"a","commands":{"open":[{"ref_id":"turn0search0"}]}}),
        ),
    ] {
        assert_eq!(
            fixture.post(path, body).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        fixture
            .post(
                "images/generations",
                json!({"model":"not-an-image-model","prompt":"x"})
            )
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/v1/images/generations", fixture.url))
            .json(&image())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(fixture.native.calls.lock().unwrap().is_empty());
    assert!(fixture.rows().await.is_empty());
}

#[tokio::test]
async fn image_cooldown_is_durable_feature_scoped_and_survives_credential_refresh() {
    let mut fixture = Fixture::start().await;
    fixture.reply(
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after".parse().unwrap(), "600".parse().unwrap())]
            .into_iter()
            .collect(),
        json!({"error":{"message":"synthetic-private-error"}}),
    );
    let response = fixture.post("images/generations", image()).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(!response.text().await.unwrap().contains("synthetic-private"));
    fixture
        .db
        .call(|conn| {
            conn.execute("UPDATE oauth_accounts SET generation=generation+1", [])?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.restart().await;
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
    assert!(fixture.upstream.select("a").await.is_ok());
    fixture.reply(StatusCode::OK, HeaderMap::new(), search_reply());
    assert_eq!(
        fixture
            .post("alpha/search", query("separate-search"))
            .await
            .status(),
        StatusCode::OK
    );
    fixture
        .db
        .call(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT cooldown_until FROM oauth_accounts WHERE id=1",
                    [],
                    |r| r.get::<_, Option<i64>>(0)
                )?,
                None
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM automatic_reset_attempts", [], |r| r
                    .get::<_, i64>(
                    0
                ))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn images_ignore_text_quota_and_retired_preferences_but_respect_operator_deactivation() {
    let fixture = Fixture::start().await;
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "UPDATE oauth_accounts SET cooldown_until=?1",
                [chrono::Utc::now().timestamp() + 600],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::OK
    );
    fixture
        .db
        .call(|conn| {
            conn.execute(
                "INSERT INTO account_policy(account_id,enabled,priority,locked) VALUES(1,0,0,1)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    fixture.db.call(|conn| {conn.execute("UPDATE account_policy SET enabled=1,locked=0",[])?;conn.execute("INSERT INTO account_preferences(user_id,account_id,enabled,priority) VALUES(1,1,0,0)",[])?;Ok(())}).await.unwrap();
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::OK
    );
    fixture
        .db
        .call(|conn| {
            oauth::disable(conn, 1)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn search_sessions_are_user_scoped_restart_stable_and_never_migrate_backend_references() {
    let mut fixture = Fixture::start().await;
    fixture.reply(StatusCode::OK, HeaderMap::new(), search_reply());
    let response = fixture
        .post("alpha/search", query("same-private-session"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .json::<Value>()
        .await
        .unwrap()
        .get("encrypted_output")
        .is_none());
    let original = fixture.native.calls.lock().unwrap()[0].2.clone();
    assert_ne!(original["id"], "same-private-session");
    assert_eq!(original["commands"], query("s")["commands"]);
    fixture.restart().await;
    let mut open = query("same-private-session");
    open["commands"] = json!({"open":[{"ref_id":"turn0search0"}]});
    assert_eq!(
        fixture.post("alpha/search", open.clone()).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.native.calls.lock().unwrap()[1].2["id"],
        original["id"]
    );
    let bob = fixture
        .db
        .call(|conn| {
            let user = exetrouter::create_user(conn, "bob")?;
            Ok(exetrouter::create_token(conn, &[1; 32], user, "fixture", 1)?.secret)
        })
        .await
        .unwrap();
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/v1/alpha/search", fixture.url))
            .bearer_auth(&bob)
            .json(&open)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/v1/alpha/search", fixture.url))
            .bearer_auth(bob)
            .json(&query("same-private-session"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_ne!(
        fixture.native.calls.lock().unwrap()[2].2["id"],
        original["id"]
    );
    fixture.mock.accounts.lock().unwrap().insert(
        "upstream-account-2".into(),
        mock::Account::new("upstream-access-2", "upstream-refresh-2", &["a"]),
    );
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
            oauth::disable(conn, 1)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!fixture
        .post("alpha/search", open)
        .await
        .status()
        .is_success());
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 3);
    assert_eq!(
        fixture
            .post("alpha/search", query("new-independent-session"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.native.calls.lock().unwrap()[3].1["chatgpt-account-id"],
        "upstream-account-2"
    );
    fixture
        .db
        .call(|conn| {
            let mut stmt = conn.prepare("SELECT digest FROM native_search_sessions")?;
            for digest in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
                assert_eq!(digest?.len(), 32);
            }
            Ok(())
        })
        .await
        .unwrap();
    for entry in std::fs::read_dir(fixture.dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            for marker in [
                "same-private-session",
                "synthetic-private-search-query",
                "synthetic-private-page-output",
                "unbound-opaque-value",
            ] {
                assert!(
                    !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
                    "persisted {marker}"
                );
            }
        }
    }
}

#[tokio::test]
async fn rejections_are_redacted_never_replayed_and_invalid_success_is_not_completed() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::OK,
    ] {
        let fixture = Fixture::start().await;
        fixture.reply(status,HeaderMap::new(),json!({"error":{"message":"synthetic-private-backend-error"},"usage":{"input_tokens":7}}));
        let response = fixture.post("images/generations", image()).await;
        assert!(!response.status().is_success());
        assert!(!response.text().await.unwrap().contains("synthetic-private"));
        assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
        let rows = fixture.rows().await;
        assert_ne!(rows[0]["status"], "completed");
        assert_eq!(rows[0]["input"], 7);
    }
}

#[tokio::test]
async fn shutdown_cancels_wait_and_finalizes_unknown_usage_without_replay() {
    let mut fixture = Fixture::start().await;
    fixture.native.wait.store(true, Ordering::SeqCst);
    let request = fixture.request("images/generations", &image());
    let task = tokio::spawn(async move { request.send().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.native.started.notified())
        .await
        .unwrap();
    fixture.stop.take().unwrap().send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), &mut fixture.server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap().status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 1);
    let rows = fixture.rows().await;
    assert_eq!(rows[0]["status"], "interrupted");
    assert!(rows[0]["output"].is_null());
}

#[tokio::test]
async fn native_output_capacity_is_bounded_without_queuing_or_extra_submission() {
    let fixture = Fixture::start().await;
    fixture.native.wait.store(true, Ordering::SeqCst);
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let request = fixture.request("images/generations", &image());
        tasks.push(tokio::spawn(async move { request.send().await.unwrap() }));
        tokio::time::timeout(Duration::from_secs(2), fixture.native.started.notified())
            .await
            .unwrap();
    }
    assert_eq!(
        fixture.post("images/generations", image()).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.native.calls.lock().unwrap().len(), 4);
    fixture.native.wait.store(false, Ordering::SeqCst);
    fixture.native.release.notify_waiters();
    for task in tasks {
        assert_eq!(task.await.unwrap().status(), StatusCode::OK);
    }
}
