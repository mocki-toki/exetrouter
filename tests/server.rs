use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Server {
    dir: TempDir,
    child: Child,
    address: String,
    identity: String,
}

impl Server {
    fn command(dir: &TempDir) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_exrd"));
        cmd.env("EXRD_CONFIG", dir.path().join("server-config.json"));
        cmd.arg("--json");
        cmd.args([
            "--db",
            dir.path().join("state.sqlite").to_str().unwrap(),
            "--key",
            dir.path().join("secret.key").to_str().unwrap(),
            "--oauth-key",
            dir.path().join("oauth.key").to_str().unwrap(),
            "--control-socket",
            dir.path().join("control.sock").to_str().unwrap(),
        ]);
        cmd
    }

    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("server-config.json");
        fs::write(&config, "{}").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let output = Self::command(&dir).arg("init").output().unwrap();
        assert!(output.status.success());
        let output = Self::command(&dir)
            .args(["admin", "user-create", "alice"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let mut blob = Vec::new();
        blob.extend_from_slice(&11u32.to_be_bytes());
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&[42; 32]);
        let pubkey = dir.path().join("test.pub");
        fs::write(&pubkey, format!("ssh-ed25519 {}", STANDARD.encode(blob))).unwrap();
        let output = Self::command(&dir)
            .args([
                "admin",
                "ssh-key-add",
                "--user-id",
                "1",
                "--public-key-file",
            ])
            .arg(pubkey)
            .output()
            .unwrap();
        assert!(output.status.success());
        let identity = serde_json::from_slice::<Value>(&output.stdout).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap().to_string();
        drop(reservation);
        let child = Self::command(&dir)
            .args([
                "serve",
                "--listen",
                &address,
                "--gateway-uid",
                &unsafe { libc::geteuid() }.to_string(),
                "--allow-same-uid-for-dev",
            ])
            .stdout(Stdio::null())
            .stderr(fs::File::create(dir.path().join("server.log")).unwrap())
            .spawn()
            .unwrap();
        let mut server = Self {
            dir,
            child,
            address,
            identity,
        };
        server.ready();
        server
    }

    fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited during startup"
            );
            if let Ok(mut stream) = TcpStream::connect(&self.address) {
                stream
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .unwrap();
                let mut response = String::new();
                if stream
                    .write_all(
                        b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                    )
                    .is_ok()
                    && stream.read_to_string(&mut response).is_ok()
                    && response.starts_with("HTTP/1.1 200")
                    && self.socket().exists()
                {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("server startup timed out");
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("control.sock")
    }

    fn control(&self, request: Value) -> Value {
        self.frame(
            json!({"identity": self.identity, "request": request})
                .to_string()
                .as_bytes(),
        )
    }

    fn frame(&self, frame: &[u8]) -> Value {
        let mut socket = UnixStream::connect(self.socket()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.write_all(frame).unwrap();
        if let Err(error) = socket.shutdown(std::net::Shutdown::Write) {
            // Oversize frames can be rejected and closed before the test finishes half-closing.
            assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        }
        let mut reply = String::new();
        socket.read_to_string(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    fn token(&self) -> Value {
        let reply =
            self.control(json!({"action":"token_create", "name":"laptop", "expires_days":1}));
        assert_eq!(reply["ok"], true);
        reply["result"].clone()
    }

    fn http(&self, method: &str, path: &str, bearer: Option<&str>, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let authorization = bearer
            .map(|b| format!("Authorization: Bearer {b}\r\n"))
            .unwrap_or_default();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n{authorization}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut reply = String::new();
        if let Err(error) = stream.read_to_string(&mut reply) {
            // macOS may report a reset after an early rejection with unread request bytes.
            // Accept only a complete HTTP response, never a truncated error/body.
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            let (headers, body) = reply.split_once("\r\n\r\n").expect("HTTP headers");
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .expect("framed HTTP response");
            assert_eq!(body.len(), length, "incomplete HTTP response after reset");
        }
        let status = reply.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, reply)
    }

    fn stop(&mut self) {
        self.signal(libc::SIGTERM);
    }

    fn signal(&mut self, signal: i32) {
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "server did not stop gracefully");
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("server did not terminate");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn http_and_control_preserve_token_scope_and_usage() {
    let server = Server::start();
    let issued = server.token();
    let secret = issued["secret"].as_str().unwrap();
    assert_eq!(server.http("GET", "/v1/models", None, "").0, 401);
    let (status, reply) = server.http("GET", "/v1/models", Some(secret), "");
    assert_eq!(status, 200);
    assert!(reply.contains("\"data\":[]"));
    for path in [
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/chat/completions",
    ] {
        let (status, reply) = server.http("POST", path, Some(secret), r#"{"model":"test-model"}"#);
        assert_eq!(status, 503);
        assert!(reply.contains("upstream_unavailable"));
    }
    let usage = server.control(json!({"action":"usage", "period":"day", "by":"user"}));
    assert_eq!(usage["result"]["rows"][0]["requests"], 3);
    assert_eq!(usage["result"]["rows"][0]["unknown_usage"], 3);
    assert_eq!(usage["result"]["rows"][0]["name"], "alice");
    assert_eq!(
        server.control(json!({"action":"token_revoke", "id":issued["token"]["id"]}))["result"]
            ["revoked"],
        true
    );
    assert_eq!(server.http("GET", "/v1/models", Some(secret), "").0, 401);
    assert!(!fs::read_to_string(server.dir.path().join("server.log"))
        .unwrap()
        .contains(secret));
}

#[test]
fn authentication_runs_before_json_extraction() {
    let server = Server::start();
    for path in [
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/chat/completions",
    ] {
        assert_eq!(server.http("POST", path, None, "{").0, 401);
        assert_eq!(server.http("POST", path, Some("bad-token"), "{").0, 401);
    }
    let token = server.token();
    assert_eq!(
        server
            .http("POST", "/v1/responses", token["secret"].as_str(), "{")
            .0,
        400
    );
}

#[test]
fn doctor_is_authenticated_read_only_and_redacts_account_data_through_the_cli() {
    let server = Server::start();
    let first = server.control(json!({"action":"doctor"}));
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"]["oauth"]["status"], "no_active_account");
    assert_eq!(
        first["result"]["limits"]["generations"]["per_user_limit"],
        8
    );
    let conn = rusqlite::Connection::open(server.dir.path().join("state.sqlite")).unwrap();
    let now = chrono::Utc::now().timestamp();
    // Invalid encrypted bytes prove diagnostics never need secret decryption.
    conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,catalog_updated_at,created_at) VALUES('private-upstream-account','active',X'00',?1,?2,?2)",rusqlite::params![now+3600,now]).unwrap();
    conn.execute(
        "INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'gpt-test','Test')",
        [],
    )
    .unwrap();
    let report = server.control(json!({"action":"doctor"}));
    assert_eq!(report["result"]["configuration_status"], "configured");
    assert_eq!(report["result"]["inference"], "not_checked");
    assert_eq!(report["result"]["upstream_connectivity"], "not_checked");
    assert!(!report.to_string().contains("private-upstream-account"));
    assert!(!report.to_string().contains("encrypted_credentials"));
    assert_eq!(
        server.control(json!({"action":"usage","period":"day"}))["result"]["rows"],
        json!([])
    );
    assert_eq!(server.http("GET", "/v1/doctor", None, "").0, 404);

    let bin = server.dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let ssh = bin.join("ssh");
    fs::write(&ssh,"#!/bin/sh\nexport SSH_ORIGINAL_COMMAND=exrd-gateway\nexec \"$EXETROUTER_TEST_GATEWAY\" --control-socket \"$EXETROUTER_TEST_SOCKET\" gateway --identity \"$EXETROUTER_TEST_IDENTITY\"\n").unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let key = server.dir.path().join("client-key");
    fs::write(&key, "synthetic fixture; fake SSH does not read this file").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_exr"))
            .args(["--identity", key.to_str().unwrap(), "doctor", "--json"])
            .env_clear()
            .env("PATH", &bin)
            .env("EXRD_CONFIG", server.dir.path().join("server-config.json"))
            .env("EXETROUTER_TEST_GATEWAY", env!("CARGO_BIN_EXE_exrd"))
            .env("EXETROUTER_TEST_SOCKET", server.socket())
            .env("EXETROUTER_TEST_IDENTITY", &server.identity)
            .env("EXETROUTER_TOKEN", "private-token-must-not-be-read")
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["local"]["management_ssh"], "ok");
    assert_eq!(value["server"]["catalog"]["models"], 1);
    assert_eq!(value["public_api"]["tls"], "not_checked");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-token"));
    conn.execute(
        "UPDATE oauth_accounts SET cooldown_until=?1,cooldown_source='upstream_reset' WHERE id=1",
        [now + 300],
    )
    .unwrap();
    conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(1,'primary',100,300,?1,?2,1)",rusqlite::params![now+300,now]).unwrap();
    let output = run();
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["server"]["configuration_status"], "needs_attention");
    assert_eq!(report["server"]["quota"]["status"], "cooldown");
    assert_eq!(
        report["server"]["quota"]["windows"][0]["window_minutes"],
        300
    );
    assert!(!report.to_string().contains("private-upstream-account"));
    conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,catalog_updated_at,created_at) VALUES('second-private-account','active',X'00',?1,?2,?2)",rusqlite::params![now+3600,now]).unwrap();
    conn.execute(
        "INSERT INTO oauth_models(account_id,model,display_name) VALUES(2,'gpt-test','Test')",
        [],
    )
    .unwrap();
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["server"]["configuration_status"], "configured");
    assert_eq!(report["server"]["pool"]["configured_accounts"], 1);
    assert_eq!(report["server"]["pool"]["cooldown_accounts"], 1);
    assert!(report["server"]["quota"].is_null());
    let accounts = report["server"]["quota_accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0]["label"], "Email unavailable");
    assert_eq!(accounts[1]["label"], "Email unavailable");
    assert_eq!(accounts[0]["quota"]["windows"][0]["window_minutes"], 300);
    assert!(accounts[1]["quota"]["windows"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(accounts
        .iter()
        .all(|account| account.get("account_id").is_none() && account["id"].as_i64().is_some()));
    assert!(!report.to_string().contains("second-private-account"));
    conn.execute(
        "UPDATE ssh_identities SET revoked_at=?1 WHERE id=?2",
        rusqlite::params![now, server.identity],
    )
    .unwrap();
    let output = run();
    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["local"]["management_ssh"],
        "unavailable"
    );
    assert_eq!(server.control(json!({"action":"doctor"}))["ok"], false);
    fs::remove_file(&key).unwrap();
    let output = run();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["local"]["ssh_identity"], "missing");
    assert_eq!(value["local"]["management_ssh"], "not_checked");
}

#[test]
fn sigterm_removes_socket_and_allows_restart() {
    let mut server = Server::start();
    let issued = server.token();
    server.stop();
    assert!(!server.socket().exists(), "socket remains after SIGTERM");
    server.child = Server::command(&server.dir)
        .args([
            "serve",
            "--listen",
            &server.address,
            "--gateway-uid",
            &unsafe { libc::geteuid() }.to_string(),
            "--allow-same-uid-for-dev",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    server.ready();
    assert_eq!(
        server
            .http("GET", "/v1/models", issued["secret"].as_str(), "")
            .0,
        200
    );
    server.signal(libc::SIGINT);
    assert!(!server.socket().exists());
}

#[test]
fn control_errors_are_structured_and_keys_are_revocable() {
    let server = Server::start();
    let reply = server.frame(b"{invalid secret-marker");
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["code"], "invalid_request");
    assert!(reply["request_id"].is_string());
    assert!(!reply.to_string().contains("secret-marker"));
    let reply = server.frame(&vec![b'x'; 32_769]);
    assert_eq!(reply["code"], "request_too_large");
    let reply = server.control(json!({"action":"token_show", "id":"unknown"}));
    assert_eq!(reply["ok"], false);
    let output = Server::command(&server.dir)
        .args(["admin", "ssh-key-revoke", &server.identity])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(server.control(json!({"action":"token_list"}))["ok"], false);
    let log = fs::read_to_string(server.dir.path().join("server.log")).unwrap();
    assert!(log.contains("control_request_failed"));
    assert!(!log.contains("secret-marker"));
}

#[test]
fn gateway_round_trip_requires_marker_and_returns_only_metadata() {
    let server = Server::start();
    let token = server.token();
    let mut child = Server::command(&server.dir)
        .args(["gateway", "--identity", &server.identity])
        .env("SSH_ORIGINAL_COMMAND", "exrd-gateway")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"action":"token_list"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["result"][0]["id"], token["token"]["id"]);
    assert!(!String::from_utf8(output.stdout)
        .unwrap()
        .contains(token["secret"].as_str().unwrap()));
    let output = Server::command(&server.dir)
        .args(["gateway", "--identity", &server.identity])
        .env("SSH_ORIGINAL_COMMAND", "unexpected")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn storage_failure_is_visible_without_leaking_request_data() {
    let server = Server::start();
    let token = server.token();
    let conn = rusqlite::Connection::open(server.dir.path().join("state.sqlite")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_usage BEFORE INSERT ON usage_events BEGIN SELECT RAISE(ABORT, 'test'); END;").unwrap();
    let (status, reply) = server.http(
        "POST",
        "/v1/responses",
        token["secret"].as_str(),
        r#"{"model":"test","input":"private-prompt-marker"}"#,
    );
    assert_eq!(status, 503);
    assert!(reply.contains("storage_unavailable"));
    assert!(reply.contains("x-request-id:"));
    let log = fs::read_to_string(server.dir.path().join("server.log")).unwrap();
    assert!(log.contains("usage_record_failed"));
    assert!(!log.contains("private-prompt-marker"));
    assert!(!log.contains(token["secret"].as_str().unwrap()));
}

#[test]
fn json_rejection_diagnostics_are_fixed_and_never_echo_payloads() {
    let server = Server::start();
    let token = server.token();
    let (status, reply) = server.http(
        "POST",
        "/v1/responses",
        token["secret"].as_str(),
        "{private-malformed-body-marker",
    );
    assert_eq!(status, 400);
    assert!(reply.contains("invalid JSON request body"));
    let log = fs::read_to_string(server.dir.path().join("server.log")).unwrap();
    assert!(log.contains("json_request_rejected"));
    assert!(log.contains("json_syntax"));
    assert!(log.contains("identity"));
    assert!(!log.contains("private-malformed-body-marker"));
    assert!(!log.contains(token["secret"].as_str().unwrap()));
    assert_eq!(
        server.control(json!({"action":"usage", "period":"day"}))["result"]["rows"],
        json!([])
    );
}

#[test]
fn oversized_http_body_has_json_error_and_no_usage() {
    let server = Server::start();
    let token = server.token();
    let (status, reply) = server.http(
        "POST",
        "/v1/responses",
        token["secret"].as_str(),
        &"x".repeat(16 * 1024 * 1024 + 1),
    );
    assert_eq!(status, 413);
    assert!(reply.contains("request_too_large"));
    assert!(reply.contains("16 MiB"));
    assert!(!reply.contains("invalid JSON"));
    assert_eq!(
        server.control(json!({"action":"usage", "period":"day"}))["result"]["rows"],
        json!([])
    );
}

#[test]
fn failed_second_listener_does_not_touch_running_socket() {
    let server = Server::start();
    let output = Server::command(&server.dir)
        .args([
            "serve",
            "--listen",
            &server.address,
            "--gateway-uid",
            &unsafe { libc::geteuid() }.to_string(),
            "--allow-same-uid-for-dev",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(server.socket().exists());
    assert_eq!(server.control(json!({"action":"token_list"}))["ok"], true);
}

#[test]
fn init_failure_cleans_only_files_it_created() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_exrd"))
        .arg("--json")
        .arg("--db")
        .arg(dir.path().join("missing/state.sqlite"))
        .arg("--key")
        .arg(dir.path().join("secret.key"))
        .arg("--oauth-key")
        .arg(dir.path().join("oauth.key"))
        .arg("init")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!dir.path().join("secret.key").exists());
    fs::write(dir.path().join("state.sqlite"), b"existing-data").unwrap();
    let output = Server::command(&dir).arg("init").output().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(dir.path().join("state.sqlite")).unwrap(),
        b"existing-data"
    );
    assert!(!dir.path().join("secret.key").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payloads_are_absent_from_real_process_logs_and_state_even_with_trace_requested() {
    use axum::{extract::WebSocketUpgrade, response::IntoResponse, routing::get, Json, Router};
    use exetrouter::oauth::{self, Credentials, Vault};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    const INPUT: &str = "PRIVATE_PAYLOAD_INPUT_40ec6";
    const IMAGE: &str = "PRIVATE_IMAGE_REFERENCE_a53d9";
    const SYSTEM: &str = "PRIVATE_SYSTEM_PROMPT_c5a8a";
    const OUTPUT: &str = "PRIVATE_GENERATED_TEXT_a9fa4";
    const TOOL: &str = "PRIVATE_TOOL_ARGUMENTS_f7270";
    const CHECKPOINT: &str = "PRIVATE_OPAQUE_CHECKPOINT_1549e";
    const ERROR: &str = "PRIVATE_UPSTREAM_ERROR_0291b";
    const HEADER: &str = "private-header-capture-marker";
    const CACHE: &str = "PRIVATE_CACHE_HINT_52844";
    const ACCESS: &str = "PRIVATE_OAUTH_ACCESS_930f0";
    const REFRESH: &str = "PRIVATE_OAUTH_REFRESH_4ac85";
    const UNKNOWN_MODEL: &str = "PRIVATE_UNVALIDATED_MODEL_09924";
    const EVENT_TYPE: &str = "PRIVATE_EVENT_TYPE_401a3";
    fn events(body: &Value) -> Vec<Value> {
        let created = json!({"type":"response.created","response":{"id":"resp_privacy_fixture","status":"in_progress","model":"gpt-test","created_at":1800000000}});
        if body["metadata"]["mode"] == "event_error" {
            return vec![
                created,
                json!({"type":"response.failed","response":{"id":"resp_privacy_fixture","status":"failed","error":{"code":"invalid_request_error","message":ERROR}}}),
            ];
        }
        let compact = body["input"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["type"] == "compaction_trigger"));
        let output = if compact {
            vec![json!({"type":"compaction","id":"cmp_fixture","encrypted_content":CHECKPOINT})]
        } else {
            let mut output = vec![
                json!({"id":"msg_fixture","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":OUTPUT,"annotations":[]}]}),
            ];
            if body["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
            {
                output.push(json!({"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":"fixture","arguments":json!({"content":TOOL}).to_string(),"status":"completed"}));
                output.push(json!({"type":"reasoning","id":"reason_fixture","encrypted_content":CHECKPOINT}));
            }
            output
        };
        let mut events = vec![created];
        if !compact {
            events.push(json!({"type":"response.output_text.delta","item_id":"msg_fixture","output_index":0,"content_index":0,"delta":OUTPUT}));
        }
        for (i, item) in output.iter().enumerate() {
            events.push(json!({"type":"response.output_item.done","output_index":i,"item":item}));
        }
        events.push(json!({"type":"response.completed","response":{"id":"resp_privacy_fixture","status":"completed","model":"gpt-test","created_at":1800000000,"output":output,"usage":{"input_tokens":10,"output_tokens":5}}}));
        if matches!(
            body["metadata"]["mode"].as_str(),
            Some("disconnect" | "unknown_disconnect")
        ) {
            events.truncate(2);
            if body["metadata"]["mode"] == "unknown_disconnect" {
                events.push(json!({"type":EVENT_TYPE,"content":OUTPUT}));
            }
        }
        events
    }
    async fn responses(Json(body): Json<Value>) -> axum::response::Response {
        if body["metadata"]["mode"] == "bad_header" {
            return ([("content-type", HEADER)], ERROR).into_response();
        }
        if body["metadata"]["mode"] == "http_error" {
            return (axum::http::StatusCode::BAD_GATEWAY, ERROR).into_response();
        }
        let frames = events(&body)
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>();
        ([("content-type", "text/event-stream")], frames).into_response()
    }
    async fn websocket(upgrade: WebSocketUpgrade) -> axum::response::Response {
        upgrade.on_upgrade(|mut socket| async move {
            while let Some(Ok(axum::extract::ws::Message::Text(text))) = socket.recv().await {
                let body: Value = serde_json::from_str(&text).unwrap();
                for event in events(&body) {
                    if socket
                        .send(axum::extract::ws::Message::Text(event.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let router=Router::new().route("/models",get(||async{Json(json!({"models":[{"slug":"gpt-test","display_name":"Fixture","supported_in_api":true,"visibility":"list"}]}))})).route("/responses",get(websocket).post(responses));
    let mock = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut server = Server::start();
    let issued = server.token();
    let secret = issued["secret"].as_str().unwrap().to_owned();
    // An unavailable upstream must not persist the unvalidated client model text.
    assert_eq!(
        server
            .http(
                "POST",
                "/v1/responses",
                Some(&secret),
                &json!({"model":UNKNOWN_MODEL,"input":INPUT}).to_string()
            )
            .0,
        503
    );
    server.stop();
    {
        let key: [u8; 32] = fs::read(server.dir.path().join("oauth.key"))
            .unwrap()
            .try_into()
            .unwrap();
        let conn = rusqlite::Connection::open(server.dir.path().join("state.sqlite")).unwrap();
        oauth::save(
            &conn,
            &Vault::new(key),
            "synthetic-account",
            &Credentials {
                access_token: ACCESS.into(),
                refresh_token: REFRESH.into(),
            },
            chrono::Utc::now().timestamp() + 3600,
        )
        .unwrap();
    }
    server.child = Server::command(&server.dir)
        .args([
            "--upstream-url",
            &upstream,
            "--oauth-issuer",
            &upstream,
            "--allow-mock-upstream",
            "serve",
            "--listen",
            &server.address,
            "--gateway-uid",
            &unsafe { libc::geteuid() }.to_string(),
            "--allow-same-uid-for-dev",
        ])
        .env("RUST_LOG", "trace")
        .stdout(fs::File::create(server.dir.path().join("stdout.log")).unwrap())
        .stderr(fs::File::create(server.dir.path().join("server.log")).unwrap())
        .spawn()
        .unwrap();
    server.ready();
    let body = json!({"model":"gpt-test","instructions":SYSTEM,"input":[{"role":"user","content":INPUT},{"type":"function_call_output","call_id":"earlier","output":TOOL}],"prompt_cache_key":CACHE,"tools":[{"type":"function","name":"fixture","description":TOOL,"parameters":{"type":"object"}}]});
    let client = reqwest::Client::new();
    let api = format!("http://{}", server.address);
    for (streaming, compressed) in [(false, false), (true, false), (false, true)] {
        let mut payload = body.clone();
        payload["stream"] = json!(streaming);
        let request = client
            .post(format!("{api}/v1/responses"))
            .bearer_auth(&secret)
            .header("x-private-test", INPUT);
        let request = if compressed {
            request
                .header("content-encoding", "zstd")
                .header("content-type", "application/json")
                .body(
                    zstd::stream::encode_all(&serde_json::to_vec(&payload).unwrap()[..], 3)
                        .unwrap(),
                )
        } else {
            request.json(&payload)
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 200);
        let text = response.text().await.unwrap();
        for marker in [OUTPUT, TOOL, CHECKPOINT] {
            assert!(
                text.contains(marker),
                "fixture output did not traverse the router"
            );
        }
    }
    let response = client
        .post(format!("{api}/v1/responses/compact"))
        .bearer_auth(&secret)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains(CHECKPOINT));
    let response=client.post(format!("{api}/v1/chat/completions")).bearer_auth(&secret).json(&json!({"model":"gpt-test","messages":[{"role":"system","content":SYSTEM},{"role":"user","content":[{"type":"text","text":INPUT},{"type":"image_url","image_url":{"url":format!("https://example.com/image.png?signature={IMAGE}"),"detail":"high"}}]}]})).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains(OUTPUT));
    let mut request = format!("ws://{}/v1/responses", server.address)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {secret}").parse().unwrap());
    let (mut websocket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let mut payload = body.clone();
    payload["type"] = json!("response.create");
    websocket
        .send(Message::Text(payload.to_string().into()))
        .await
        .unwrap();
    let mut received = String::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            received.push_str(&text);
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "response.completed" {
                break;
            }
        }
    }
    assert!(received.contains(OUTPUT) && received.contains(TOOL) && received.contains(CHECKPOINT));
    websocket.close(None).await.unwrap();
    drop(websocket);
    let response = client
        .post(format!("{api}/v1/responses"))
        .bearer_auth(&secret)
        .header("content-type", "application/json")
        .body(format!("{{\"private\":\"{INPUT}\", BROKEN"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    for mode in [
        "event_error",
        "bad_header",
        "http_error",
        "disconnect",
        "unknown_disconnect",
    ] {
        let mut payload = body.clone();
        payload["metadata"] = json!({"mode":mode});
        let disconnect = mode.ends_with("disconnect");
        payload["stream"] = json!(disconnect);
        let response = client
            .post(format!("{api}/v1/responses"))
            .bearer_auth(&secret)
            .json(&payload)
            .send()
            .await
            .unwrap();
        if mode == "event_error" {
            assert_eq!(response.status(), 200);
            assert!(response.text().await.unwrap().contains(ERROR));
        } else if disconnect {
            assert_eq!(response.status(), 200);
            let text = response.text().await.unwrap();
            assert!(text.contains("response.failed"));
            assert!(!text.contains("response.completed"));
        } else {
            assert!(
                response.status().is_server_error(),
                "{mode}: {}",
                response.status()
            );
        }
        // Failure backoff is operational metadata; clear it only in this synthetic fixture.
        rusqlite::Connection::open(server.dir.path().join("state.sqlite"))
            .unwrap()
            .execute("DELETE FROM oauth_health", [])
            .unwrap();
    }
    server.stop();
    let logs = fs::read_to_string(server.dir.path().join("server.log")).unwrap();
    assert!(logs.contains("model_request_finished") && logs.contains("upstream_protocol_rejected"));
    assert!(!logs.contains("reqwest") && !logs.contains("hyper") && !logs.contains("tungstenite"));
    let interruptions: Vec<Value> = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["fields"]["event"] == "upstream_stream_interrupted")
        .collect();
    assert_eq!(interruptions.len(), 2);
    for (line, kind, count) in [
        (&interruptions[0], "response.output_text.delta", 2),
        (&interruptions[1], "other", 3),
    ] {
        let fields = &line["fields"];
        assert_eq!(fields["reason"], "upstream_eof");
        assert_eq!(fields["last_event_kind"], kind);
        assert_eq!(fields["events_seen"], count);
        assert_eq!(fields["output_seen"], true);
        assert!(fields["last_event_at_ms"].as_i64().unwrap() > 0);
        assert!(fields["last_event_age_ms"].as_u64().is_some());
    }
    for entry in fs::read_dir(server.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if !(name.ends_with(".log") || name.starts_with("state.sqlite")) {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        for marker in [
            INPUT,
            IMAGE,
            SYSTEM,
            OUTPUT,
            TOOL,
            CHECKPOINT,
            ERROR,
            HEADER,
            CACHE,
            ACCESS,
            REFRESH,
            UNKNOWN_MODEL,
            EVENT_TYPE,
            secret.as_str(),
        ] {
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "payload marker retained in {name}"
            );
        }
    }
    mock.abort();
}
