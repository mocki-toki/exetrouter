//! Bounded two-account checks; all account identifiers stay inside the harness.
use super::*;
use hmac::{Hmac, Mac};
use rusqlite::{params, OptionalExtension};
use sha2::Sha256;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type HealthRow = (i64, i64, i64, String, i64);

// A short, explicitly synthetic pause. Restore only our exact observation so a
// newer real failure/success can never be replaced by the old snapshot.
struct Pause {
    db: Database,
    id: i64,
    generation: i64,
    order: i64,
    until: i64,
    old: Option<HealthRow>,
}
impl Pause {
    async fn apply(db: &Database, id: i64) -> Result<Self> {
        let (generation, order, until, old) = db.call(move |conn| {
            let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            let generation = tx.query_row("SELECT generation FROM oauth_accounts WHERE id=?1 AND state='active'", [id], |r| r.get::<_,i64>(0))?;
            let old: Option<HealthRow> = tx.query_row("SELECT generation,failures,retry_at,reason,last_order FROM oauth_health WHERE account_id=?1 AND scope='responses'", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let now = chrono::Utc::now().timestamp();
            if old.as_ref().is_some_and(|row|row.0 == generation && row.2 > now) {
                return Err("synthetic pause must not replace an existing real pause".into());
            }
            let order = tx.query_row("UPDATE upstream_operation_clock SET sequence=sequence+1 WHERE id=1 AND sequence<9223372036854775807 RETURNING sequence", [], |r| r.get::<_,i64>(0))?;
            let until = now + 300;
            tx.execute("INSERT INTO oauth_health VALUES(?1,'responses',?2,1,?3,'live_test_pause',?4) ON CONFLICT(account_id,scope) DO UPDATE SET generation=excluded.generation,failures=excluded.failures,retry_at=excluded.retry_at,reason=excluded.reason,last_order=excluded.last_order", params![id,generation,until,order])?;
            tx.commit()?;
            Ok((generation,order,until,old))
        }).await?;
        Ok(Self {
            db: db.clone(),
            id,
            generation,
            order,
            until,
            old,
        })
    }
    async fn restore(self) -> Result<bool> {
        self.db.call(move |conn| {
            let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            let removed = tx.execute("DELETE FROM oauth_health WHERE account_id=?1 AND scope='responses' AND generation=?2 AND failures=1 AND retry_at=?3 AND reason='live_test_pause' AND last_order=?4", params![self.id,self.generation,self.until,self.order])?;
            if removed == 1 {
                if let Some((generation,failures,retry,reason,order)) = self.old {
                    tx.execute("INSERT INTO oauth_health VALUES(?1,'responses',?2,?3,?4,?5,?6)", params![self.id,generation,failures,retry,reason,order])?;
                }
            }
            tx.commit()?;
            Ok(removed == 1)
        }).await
    }
}

fn cache_session(key: &[u8; 32], user: i64, model: &str, supplied: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts the test key");
    mac.update(b"exetrouter-cache-v1\0");
    mac.update(&user.to_be_bytes());
    mac.update(&(model.len() as u64).to_be_bytes());
    mac.update(model.as_bytes());
    mac.update(b"explicit\0");
    mac.update(supplied.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
async fn cache_keys(
    session: &Session,
    key: &[u8; 32],
    model: &str,
    ids: &[i64],
) -> Result<Vec<String>> {
    let mut keys = vec![None; ids.len()];
    for index in 0..64 {
        let supplied = format!("pool-check-{index}");
        let affinity = cache_session(key, session.user, model, &supplied);
        // Credential/catalog validation only; selection does not send inference.
        let selected = session
            .upstream
            .select_cached(model, Some(&affinity))
            .await
            .map_err(|_| "pool candidate is unavailable before inference")?;
        let index = ids
            .iter()
            .position(|id| *id == selected.info.id)
            .ok_or("pool selected an unexpected account")?;
        keys[index].get_or_insert(supplied);
        if keys.iter().all(Option::is_some) {
            return Ok(keys.into_iter().flatten().collect());
        }
    }
    Err("could not find cache preferences for both eligible accounts".into())
}
async fn open_socket(session: &Session) -> Result<Socket> {
    let mut upgrade = format!("{}/v1/responses", session.url.replace("http://", "ws://"))
        .into_client_request()?;
    upgrade.headers_mut().insert(
        "authorization",
        format!("Bearer {}", session.secret).parse()?,
    );
    connect_async(upgrade)
        .await
        .map(|(socket, _)| socket)
        .map_err(|_| "pool WebSocket Upgrade rejected".into())
}
async fn send(socket: &mut Socket, body: &Value) -> Result<()> {
    let mut create = body.clone();
    create["type"] = json!("response.create");
    create
        .as_object_mut()
        .ok_or("pool request must be an object")?
        .remove("stream");
    socket
        .send(Message::Text(create.to_string().into()))
        .await
        .map_err(|_| "pool WebSocket send interrupted; do not replay blindly".into())
}
async fn socket_error(socket: &mut Socket, code: &str, paused: bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = socket.next().await {
            match message.map_err(|_| "pool WebSocket error check interrupted")? {
                Message::Text(text) => {
                    let event: Value =
                        serde_json::from_str(&text).map_err(|_| "invalid pool error frame")?;
                    if event["type"] == "error" {
                        if event["error"]["code"] != code
                            || (paused
                                && (event["status"] != 503
                                    || event["error"]["retry_after"]
                                        .as_u64()
                                        .is_none_or(|n| n == 0)))
                        {
                            return Err("pool WebSocket returned an unexpected rejection".into());
                        }
                        return Ok(());
                    }
                    if event["type"]
                        .as_str()
                        .is_some_and(|kind| kind.starts_with("response."))
                    {
                        return Err("locally rejected pool frame reached inference".into());
                    }
                }
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
                Message::Close(_) => {
                    return Err("pool WebSocket lost its binding after local rejection".into())
                }
                _ => {}
            }
        }
        Err("pool WebSocket ended during rejection check".into())
    })
    .await
    .map_err(|_| "pool rejection check timed out")?
}
fn continuity(response: &Value, memory: Option<&str>) -> Result<()> {
    marker(response)?;
    if memory.is_some_and(|memory| {
        !response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .any(|part| {
                part["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(memory))
            })
    }) {
        return Err("pool continuation lost the synthetic continuity token".into());
    }
    Ok(())
}
async fn usage(session: &Session, expected: &[(i64, &str)]) -> Result<()> {
    for _ in 0..250 {
        let token = session.token_id.clone();
        let rows = session.db.call(move |conn| {
            let mut stmt = conn.prepare("SELECT account_id,status,upstream_transport,input_tokens,output_tokens FROM usage_events WHERE token_id=?1 ORDER BY id")?;
            let rows = stmt.query_map([token], |r|Ok((r.get::<_,Option<i64>>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,Option<i64>>(4)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(rows)
        }).await?;
        if rows.len() == expected.len() && rows.iter().all(|r| r.1 == "completed") {
            if rows
                .iter()
                .zip(expected)
                .any(|(row, (account, transport))| {
                    row.0 != Some(*account)
                        || row.2.as_deref() != Some(*transport)
                        || row.3.is_none()
                        || row.4.is_none()
                })
            {
                return Err("pool account, transport or usage differs from the scenario".into());
            }
            return Ok(());
        }
        if rows.len() > expected.len()
            || rows
                .iter()
                .any(|r| !matches!(r.1.as_str(), "accepted" | "sent" | "completed"))
        {
            return Err("pool request count or outcome differs from the scenario".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("pool durable usage did not finish".into())
}

async fn checks(
    session: &Session,
    key: &[u8; 32],
    model: &str,
    memory: Option<&str>,
) -> Result<Value> {
    let models = session.upstream.models().await?;
    exetrouter::catalog::codex(&models)?;
    exetrouter::catalog::opencode(&models)?;
    let requested = model.to_owned();
    let ids = session.db.call(move |conn| {
        let mut stmt = conn.prepare("SELECT a.id FROM oauth_accounts a JOIN oauth_models m ON m.account_id=a.id WHERE a.state='active' AND m.model=?1 ORDER BY a.id")?;
        let ids = stmt.query_map([requested], |r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(ids)
    }).await?;
    if ids.len() != 2 {
        return Err("pool model must be visible on both test accounts".into());
    }
    let keys = cache_keys(session, key, model, &ids).await?;
    let mut a = body(model);
    a["prompt_cache_key"] = json!(keys[0]);
    a["instructions"] = json!("Remember the conversation state. Follow the user instruction and return only the requested marker and continuity token when asked.");
    if let Some(memory) = memory {
        a["input"] = json!([{"role":"user","content":format!("The continuity token is {memory}. Remember it for future turns. Return exactly EXETROUTER_SMOKE_OK.")}]);
    }
    let first = session.response(&a).await?;
    marker(&first)?;
    let mut b = body(model);
    b["prompt_cache_key"] = json!(keys[1]);
    marker(&session.response(&b).await?)?;
    let mut expected = vec![(ids[0], "http_sse"), (ids[1], "http_sse")];
    usage(session, &expected).await?;
    println!("pool: both accounts completed HTTP requests; both catalog projections passed");

    let mut history = a["input"]
        .as_array()
        .ok_or("pool seed input missing")?
        .clone();
    history.extend(
        first["output"]
            .as_array()
            .ok_or("pool seed output missing")?
            .clone(),
    );
    let response = session.post("responses/compact", &json!({"model":model,"prompt_cache_key":keys[0],"instructions":"Preserve the conversation and its continuity token.","input":history})).await?;
    if !response.status().is_success() {
        return Err("pool compaction rejected".into());
    }
    let compact: Value = response
        .json()
        .await
        .map_err(|_| "invalid pool compaction JSON")?;
    let mut input = compact["output"]
        .as_array()
        .ok_or("pool checkpoint output missing")?
        .clone();
    if !input.iter().any(|item| {
        item["type"] == "compaction"
            && item["encrypted_content"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
    }) {
        return Err("pool compaction returned no checkpoint".into());
    }
    let follow_text = if memory.is_some() {
        "Return EXETROUTER_SMOKE_OK followed by the continuity token from our conversation."
    } else {
        "Return exactly EXETROUTER_SMOKE_OK."
    };
    input.push(json!({"role":"user","content":follow_text}));
    let mut follow = b.clone();
    follow["instructions"] = a["instructions"].clone();
    follow["input"] = json!(input);
    continuity(&session.response(&follow).await?, memory)?;
    expected.extend([(ids[0], "http_sse"), (ids[0], "http_sse")]);
    usage(session, &expected).await?;

    let mut socket = open_socket(session).await?;
    send(&mut socket, &follow).await?;
    let first_ws = ws_terminal(&mut socket).await?;
    continuity(&first_ws, memory)?;
    let mut delta = b.clone();
    delta["instructions"] = a["instructions"].clone();
    delta["input"] = json!([{"role":"user","content":follow_text}]);
    delta["previous_response_id"] = first_ws["id"].clone();
    send(&mut socket, &delta).await?;
    let last_ws = ws_terminal(&mut socket).await?;
    continuity(&last_ws, memory)?;
    delta["previous_response_id"] = last_ws["id"].clone();
    expected.extend([(ids[0], "websocket"), (ids[0], "websocket")]);
    usage(session, &expected).await?;
    let mut changed = delta.clone();
    changed["prompt_cache_key"] = json!(keys[0]);
    send(&mut socket, &changed).await?;
    socket_error(&mut socket, "cache_key_changed", false).await?;
    usage(session, &expected).await?;
    println!("pool: checkpoint overrides another account's cache preference over HTTP and WS; WS history retained");

    let pause = Pause::apply(&session.db, ids[0]).await?;
    let paused: Result<()> = async {
        let rejected = session.post("responses", &follow).await?;
        if rejected.status().as_u16() != 503
            || rejected
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_none_or(|n| n == 0)
        {
            return Err("paused checkpoint HTTP request did not return a bounded backoff".into());
        }
        let error: Value = rejected
            .json()
            .await
            .map_err(|_| "invalid paused pool rejection")?;
        if error["error"]["code"] != "upstream_backoff" {
            return Err("paused checkpoint HTTP error differs from the contract".into());
        }
        send(&mut socket, &delta).await?;
        socket_error(&mut socket, "upstream_backoff", true).await?;
        usage(session, &expected).await?;
        let mut independent = body(model);
        independent["prompt_cache_key"] = json!(keys[0]);
        marker(&session.response(&independent).await?)?;
        let mut healthy_socket = open_socket(session).await?;
        send(&mut healthy_socket, &independent).await?;
        marker(&ws_terminal(&mut healthy_socket).await?)?;
        healthy_socket.close(None).await?;
        expected.extend([(ids[1], "http_sse"), (ids[1], "websocket")]);
        usage(session, &expected).await?;
        Ok(())
    }
    .await;
    let restored = pause.restore().await?;
    paused?;
    if !restored {
        return Err(
            "synthetic pause was replaced by a newer observation; old state not restored".into(),
        );
    }
    println!("pool: local pause blocks checkpoint and bound WS without inference; independent HTTP/WS use the healthy account");

    send(&mut socket, &delta).await?;
    continuity(&ws_terminal(&mut socket).await?, memory)?;
    socket.close(None).await?;
    let mut independent = body(model);
    independent["prompt_cache_key"] = json!(keys[0]);
    marker(&session.response(&independent).await?)?;
    expected.extend([(ids[0], "websocket"), (ids[0], "http_sse")]);
    usage(session, &expected).await?;
    Ok(
        json!({"check":"two_account_pool","model":model,"accounts":2,"requests":10,"known_usage":10,"local_rejections":3,"account_requests":{"A":7,"B":3},"checkpoint_continuity":memory.is_some(),"pause":"synthetic_local_300s_restored","ws_recovered":true}),
    )
}

#[tokio::test]
#[ignore = "consumes two real accounts' quota; requires isolated EXETROUTER_LIVE_STATE and EXETROUTER_LIVE_MODEL"]
async fn live_two_account_pool() {
    let (directory, db, upstream, key) = live_state_accounts(2)
        .await
        .expect("invalid two-account live state");
    let model =
        std::env::var("EXETROUTER_LIVE_MODEL").expect("choose a model visible on both accounts");
    let session = Session::start(&directory, db, upstream, key)
        .await
        .expect("pool test service could not start");
    let memory = format!("{:032x}", rand::random::<u128>());
    let checked = checks(&session, &key, &model, Some(&memory)).await;
    session.close().await.expect("pool cleanup failed");
    println!(
        "{}",
        checked.expect("real pool check failed; inspect usage before another run")
    );
}

#[tokio::test]
async fn two_account_pipeline_routes_and_recovers_without_replay() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path().join("state.sqlite"))
        .await
        .unwrap();
    db.call(|conn| {
        for (id, access, refresh) in [
            ("pool-a", "access-a", "refresh-a"),
            ("pool-b", "access-b", "refresh-b"),
        ] {
            oauth::save(
                conn,
                &oauth::Vault::new([2; 32]),
                id,
                &oauth::Credentials {
                    access_token: access.into(),
                    refresh_token: refresh.into(),
                },
                chrono::Utc::now().timestamp() + 3600,
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let mock = Arc::new(mock::Mock::default());
    for (id, access, refresh) in [
        ("pool-a", "access-a", "refresh-a"),
        ("pool-b", "access-b", "refresh-b"),
    ] {
        mock.accounts.lock().unwrap().insert(
            id.into(),
            mock::Account::new(access, refresh, &["gpt-test"]),
        );
    }
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
    let checked = checks(&session, &[1; 32], "gpt-test", None).await;
    session.close().await.unwrap();
    mock_server.abort();
    assert_eq!(checked.unwrap()["requests"], 10);
    assert_eq!(mock.requests.lock().unwrap().len(), 9);
    assert_eq!(mock.compactions.lock().unwrap().len(), 1);
    {
        let accounts = mock.request_accounts.lock().unwrap();
        assert_eq!(
            accounts.iter().filter(|id| id.as_str() == "pool-a").count(),
            6
        );
        assert_eq!(
            accounts.iter().filter(|id| id.as_str() == "pool-b").count(),
            3
        );
    }
    assert!(db
        .call(move |conn| Ok(conn.query_row(
            "SELECT revoked_at IS NOT NULL FROM access_tokens WHERE id=?1",
            [token],
            |r| r.get::<_, bool>(0)
        )?))
        .await
        .unwrap());
    assert!(!directory.path().join("live-smoke.sock").exists());
}

#[tokio::test]
async fn pause_cleanup_preserves_newer_observations_and_restores_on_check_failure() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path().join("state.sqlite"))
        .await
        .unwrap();
    let id = db
        .call(|conn| {
            oauth::save(
                conn,
                &oauth::Vault::new([2; 32]),
                "fixture",
                &oauth::Credentials {
                    access_token: "access".into(),
                    refresh_token: "refresh".into(),
                },
                chrono::Utc::now().timestamp() + 3600,
            )
        })
        .await
        .unwrap();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO oauth_health VALUES(?1,'responses',0,0,0,'healthy',0)",
            [id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let pause = Pause::apply(&db, id).await.unwrap();
    let checked: Result<()> = Err("synthetic check failure".into());
    assert!(pause.restore().await.unwrap());
    assert!(checked.is_err());
    assert_eq!(
        db.call(move |conn| Ok(conn.query_row(
            "SELECT reason FROM oauth_health WHERE account_id=?1 AND scope='responses'",
            [id],
            |r| r.get::<_, String>(0)
        )?))
        .await
        .unwrap(),
        "healthy"
    );
    let pause = Pause::apply(&db, id).await.unwrap();
    db.call(move |conn| { conn.execute("UPDATE oauth_health SET last_order=last_order+1,reason='transport',retry_at=retry_at+60 WHERE account_id=?1 AND scope='responses'",[id])?; Ok(()) }).await.unwrap();
    assert!(!pause.restore().await.unwrap());
    assert!(Pause::apply(&db, id).await.is_err());
    assert_eq!(
        db.call(move |conn| Ok(conn.query_row(
            "SELECT reason FROM oauth_health WHERE account_id=?1 AND scope='responses'",
            [id],
            |r| r.get::<_, String>(0)
        )?))
        .await
        .unwrap(),
        "transport"
    );
}
