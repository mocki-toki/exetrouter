//! All inference in this module targets a literal-loopback synthetic provider.
use super::*;
use crate::{
    affinity, catalog,
    upstream::{Config, SelectError, Upstream},
};
use std::time::Duration;

async fn fixture() -> (
    tempfile::TempDir,
    crate::store::Database,
    Mock,
    Arc<Upstream>,
) {
    let (dir, db, vault) = database().await;
    let mock = Mock::open().await;
    mock.state.lock().unwrap().response_models = vec![
        "gpt-6.1-sol".into(),
        "gpt-6-astra".into(),
        "unknown-do-not-route".into(),
    ];
    client::refresh(
        &db,
        vault,
        &client::Client::new(&mock.base, true).unwrap(),
        1,
        true,
    )
    .await
    .unwrap();
    db.call(|conn| {
        let now = chrono::Utc::now().timestamp();
        let mut native = routes::models(conn, -1, now)?
            .into_iter()
            .find(|m| m.id == "gpt-6.1-sol")
            .unwrap();
        native.source = "codex".into();
        native.metadata.as_mut().unwrap()["context_window"] = json!(500000);
        conn.execute(
            "INSERT INTO oauth_models(account_id,model,display_name,metadata) VALUES(1,?1,?2,?3)",
            params![
                native.id,
                native.display_name,
                native.metadata.unwrap().to_string()
            ],
        )?;
        conn.execute(
            "UPDATE oauth_accounts SET catalog_updated_at=?1 WHERE id=1",
            [now],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let config = Config::new(mock.base.clone(), mock.base.clone(), true)
        .unwrap()
        .mock_platform(&mock.base)
        .unwrap();
    let upstream = Arc::new(Upstream::new(db.clone(), [7; 32], config).unwrap());
    (dir, db, mock, upstream)
}

#[tokio::test]
async fn additive_route_reupgrade_retains_identity_health_secrets_and_never_uses_allowance_as_admission(
) {
    let (_dir, db, _mock, upstream) = fixture().await;
    db.call(|conn| {
        conn.execute("INSERT INTO account_subscriptions(account_id,plan_usd,api_budget_cents) VALUES(1,200,0)",[])?;
        let now=chrono::Utc::now().timestamp();
        routes::quota(conn,-1,1,10,now+100)?;
        conn.pragma_update(None,"user_version",18)?;
        crate::store::init(conn)?;
        assert_eq!(conn.query_row("PRAGMA user_version",[],|r|r.get::<_,usize>(0))?,crate::store::SCHEMA_VERSION);
        assert_eq!(routes::parent(conn,-1)?,1);
        assert_eq!(routes::paused(conn,-1,1,now)?.0,Some(now+100));
        verify_credentials(conn,&Vault::new([7;32]))?;
        conn.execute("DELETE FROM platform_route_health",[])?;
        Ok(())
    }).await.unwrap();
    assert!(
        upstream.select("gpt-6-astra").await.unwrap().info.id < 0,
        "zero display allowance is not an API spending cap"
    );
    exhausted(&db).await;
    db.call(|conn| Resources::Both.set(conn)).await.unwrap();
    assert!(upstream.select("gpt-6.1-sol").await.unwrap().info.id < 0);
}

#[tokio::test]
async fn every_eligible_native_subscription_must_be_exhausted_and_owned_platform_catalogs_refresh_without_inference(
) {
    let (_dir, db, mock, upstream) = fixture().await;
    db.call(|conn| {
        let vault=Vault::new([7;32]);
        crate::oauth::save(conn,&vault,"second-synthetic-account",&crate::oauth::Credentials {access_token:"synthetic-access".into(),refresh_token:"synthetic-refresh".into()},9999999999)?;
        conn.execute("INSERT INTO oauth_models(account_id,model,display_name,metadata) SELECT 2,model,display_name,metadata FROM oauth_models WHERE account_id=1",[])?;
        conn.execute("UPDATE oauth_accounts SET catalog_updated_at=?1 WHERE id=2",[chrono::Utc::now().timestamp()])?;
        apply_legacy_fallback(conn,true)
    }).await.unwrap();
    exhausted(&db).await;
    assert_eq!(
        upstream.select("gpt-6.1-sol").await.unwrap().info.id,
        2,
        "unknown remaining capacity is not exhausted"
    );
    db.call(|conn| {
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) SELECT 2,kind,used_percent,window_minutes,reset_at,observed_at,request_order FROM oauth_quota_windows WHERE account_id=1",[])?;
        conn.execute("UPDATE platform_routes SET observed_at=observed_at-901",[])?;
        Ok(())
    }).await.unwrap();
    assert!(
        upstream.select_pinned("gpt-6-astra", -1).await.is_ok(),
        "project-owned continuations refresh stale availability without changing identity"
    );
    let api = upstream.select("gpt-6.1-sol").await.unwrap();
    assert!(api.info.id < 0);
    db.call(|conn| apply_legacy_fallback(conn, false))
        .await
        .unwrap();
    assert!(
        upstream.validate(&api, "gpt-6.1-sol").await.is_err(),
        "disabling Codex API spending also applies to owned routes"
    );
    assert!(upstream.select_pinned("gpt-6-astra", -1).await.is_ok());
    assert_eq!(mock.state.lock().unwrap().inferences.len(), 0);
}

#[tokio::test]
async fn passive_diagnostics_include_platform_readiness_and_catalog_when_native_accounts_are_paused(
) {
    let (_dir, db, mock, _upstream) = fixture().await;
    db.call(|conn| {
        let now=chrono::Utc::now().timestamp();
        conn.execute("UPDATE oauth_accounts SET cooldown_until=?1,cooldown_source='local_backoff' WHERE id=1",[now+60])?;
        let limits=||crate::doctor::LimitsReport {generations:crate::server::LimitSnapshot {global_limit:64,per_user_limit:8,active_for_user:0},websockets:crate::server::LimitSnapshot {global_limit:64,per_user_limit:8,active_for_user:0}};
        let report=crate::doctor::snapshot(conn,true,limits(),now)?;
        assert_eq!(report.configuration_status,"configured");
        assert_eq!(report.catalog.models,2);
        assert_eq!(report.pool.as_ref().unwrap().configured_accounts,0,"OAuth pool metrics remain native, not fabricated API subscriptions");
        let report=serde_json::to_value(report)?;
        assert_eq!(report["quota_accounts"][0]["platform"]["inference_enabled"],true);
        assert!(!report.to_string().contains("sk-"));
        conn.execute("UPDATE platform_working_keys SET state='inactive'",[])?;
        let unavailable=crate::doctor::snapshot(conn,true,limits(),now)?;
        assert_eq!(unavailable.configuration_status,"needs_attention");
        assert_eq!(unavailable.catalog.models,1);
        Ok(())
    }).await.unwrap();
    assert!(mock.state.lock().unwrap().inferences.is_empty());
}

async fn exhausted(db: &crate::store::Database) {
    db.call(|conn| {
        let now=chrono::Utc::now().timestamp();
        conn.execute("INSERT INTO oauth_quota_windows(account_id,kind,used_percent,window_minutes,reset_at,observed_at,request_order) VALUES(1,'secondary',100,10080,?1,?2,1)",params![now+86400,now])?;
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn platform_catalog_is_observed_source_ordered_and_fallback_is_only_for_all_codex_quota() {
    let (_dir, db, mock, upstream) = fixture().await;
    let models = upstream.models().await.unwrap();
    assert_eq!(
        models
            .iter()
            .map(|m| (m.id.as_str(), m.source.as_str()))
            .collect::<Vec<_>>(),
        vec![("gpt-6.1-sol", "codex"), ("gpt-6-astra", "platform")]
    );
    assert_eq!(
        models[0].metadata.as_ref().unwrap()["context_window"],
        500000
    );
    assert_eq!(
        models[1].metadata.as_ref().unwrap()["context_window"],
        1050000
    );
    assert!(models[1].capabilities.contains(&"tools".into()));
    assert!(serde_json::to_value(&models[0])
        .unwrap()
        .get("platform_fallback")
        .is_none());
    catalog::codex(&models).unwrap();
    catalog::opencode_v1(&models).unwrap();
    catalog::opencode_v2(&models).unwrap();
    assert!(upstream.select("gpt-6-astra").await.unwrap().info.id < 0);
    assert_eq!(upstream.select("gpt-6.1-sol").await.unwrap().info.id, 1);
    exhausted(&db).await;
    assert_eq!(
        upstream.select("gpt-6.1-sol").await.unwrap().info.id,
        1,
        "default policy preserves credit-backed Codex"
    );
    db.call(|conn| Resources::Both.set(conn)).await.unwrap();
    let models = upstream.models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].source, "codex");
    assert_eq!(
        models[0].metadata.as_ref().unwrap()["context_window"],
        500000
    );
    assert_eq!(
        serde_json::to_value(&models[0]).unwrap()["platform_fallback"],
        true
    );
    assert!(!models[1].platform_fallback);
    assert!(upstream.select("gpt-6.1-sol").await.unwrap().info.id < 0);
    db.call(|conn| {
        crate::health::failure(
            conn,
            (1, 0),
            crate::health::Scope::Responses,
            10,
            "transport",
            None,
            chrono::Utc::now().timestamp(),
        )
    })
    .await
    .unwrap();
    assert!(
        matches!(
            upstream.select("gpt-6.1-sol").await,
            Err(SelectError::Backoff(_))
        ),
        "API must not bypass native operational failure"
    );
    db.call(|conn| {
        conn.execute(
            "UPDATE oauth_accounts SET state='reauth_required' WHERE id=1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(upstream.select("gpt-6.1-sol").await.is_err());
    assert_eq!(
        mock.state.lock().unwrap().inferences.len(),
        0,
        "catalog and selection never submit inference"
    );
}

#[tokio::test]
async fn newly_supported_platform_models_are_callable_only_when_observed() {
    let (dir, db, mock, upstream) = fixture().await;
    let ids = [
        "gpt-6.2-astra",
        "gpt-6.2-sol",
        "gpt-5.4",
        "gpt-5.4-pro",
        "gpt-5.4-mini",
        "gpt-5.2",
        "gpt-5.2-pro",
        "chat-latest",
    ];
    assert!(!upstream
        .models()
        .await
        .unwrap()
        .iter()
        .any(|m| ids.contains(&m.id.as_str())));
    mock.state
        .lock()
        .unwrap()
        .response_models
        .extend(ids.map(str::to_owned));
    inventory::read(
        &db,
        Vault::new([7; 32]),
        &client::Client::new(&mock.base, true).unwrap(),
        1,
    )
    .await
    .unwrap();
    let api = Api::open(&dir, &db, upstream.clone()).await;
    let models = upstream.models().await.unwrap();
    assert_eq!(mock.state.lock().unwrap().inferences.len(), 0);
    assert!(!models.iter().any(|m| m.id == "unknown-do-not-route"));
    for id in ids {
        assert!(models.iter().any(|m| m.id == id && m.source == "platform"));
        let response = api
            .post(
                "responses",
                &json!({"model":id,"input":"synthetic Platform fixture"}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{id}");
    }
    assert_eq!(mock.state.lock().unwrap().inferences.len(), ids.len());
}

#[tokio::test]
async fn project_identity_survives_key_rotation_and_rejects_old_credentials_or_cross_backend_receipts(
) {
    let (_dir, db, _mock, upstream) = fixture().await;
    let a = upstream.account(-1).await.unwrap();
    let key = [7; 32];
    let value = affinity::issue("same-opaque", affinity::Kind::Content, &key, 1, -1).unwrap();
    let input = json!([{"type":"reasoning","summary":[],"encrypted_content":value}]);
    let refs = affinity::references(&input, &key, 1).unwrap();
    db.call(move |conn| {
        assert!(matches!(
            affinity::lookup_references(conn, 1, &refs)?,
            affinity::Lookup::Account(-1)
        ));
        assert!(!affinity::can_transfer(&refs, 1));
        assert!(!affinity::can_transfer(&refs, -2));
        let platform_digest = affinity::digests(
            &json!([{"type":"reasoning","encrypted_content":"same-opaque"}]),
            &key,
            1,
        )?;
        let receipt = affinity::transfer_item(&key, 1, 1, &platform_digest)?;
        let mut mixed = input.as_array().unwrap().clone();
        mixed.push(receipt);
        assert!(affinity::references(&json!(mixed), &key, 1).is_err());
        Ok(())
    })
    .await
    .unwrap();
    let vault = Vault::new(key);
    let until = db
        .call(move |conn| {
            let until = chrono::Utc::now().timestamp() + 90;
            routes::quota(conn, -1, 1, 5, until)?;
            conn.execute(
            "UPDATE account_platform_credentials SET generation=generation+1 WHERE account_id=1",
            [],
        )?;
            conn.execute(
                "UPDATE platform_working_keys SET generation=generation+1 WHERE account_id=1",
                [],
            )?;
            let working = keys::working_secret(conn, &vault, 1, chrono::Utc::now().timestamp())?;
            routes::publish(
                conn,
                1,
                &working.identity,
                &["gpt-6-astra".into()],
                chrono::Utc::now().timestamp(),
            )?;
            Ok(until)
        })
        .await
        .unwrap();
    let rotated = upstream.account(-1).await.unwrap();
    assert_eq!(rotated.info.id, a.info.id);
    assert!(rotated.info.generation > a.info.generation);
    assert_eq!(
        rotated.info.cooldown_until,
        Some(until),
        "working-key refresh cannot erase a project quota rejection"
    );
    assert!(matches!(
        upstream.select_pinned("gpt-6-astra", -1).await,
        Err(SelectError::Cooldown(_))
    ));
    db.call(|conn| {
        conn.execute("UPDATE platform_route_health SET quota_until=0", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(upstream.check_cooldown(&a).await.is_err());
    assert!(upstream.select_pinned("gpt-6-astra", -1).await.is_ok());
    db.call(|conn| {
        let now = chrono::Utc::now().timestamp();
        routes::quota(conn, -1, 2, 9, now + 90)?;
        routes::health(
            conn,
            -1,
            2,
            10,
            Some(crate::health::Rejection::Temporary),
            None,
            now,
        )?;
        routes::health(conn, -1, 2, 8, None, None, now)?;
        assert_eq!(
            routes::paused(conn, -1, 2, now)?,
            (Some(now + 90), Some(now + 5))
        );
        routes::health(conn, -1, 2, 11, None, None, now)?;
        assert_eq!(routes::paused(conn, -1, 2, now)?, (Some(now + 90), None));
        routes::quota(conn, -1, 1, 99, now + 999)?;
        assert_eq!(routes::paused(conn, -1, 2, now)?.0, Some(now + 90));
        assert!(crate::quota::summary(conn, 1, now)?
            .cooldown_until
            .is_none());
        assert!(crate::health::paused(conn, 1, now)?.is_none());
        Ok(())
    })
    .await
    .unwrap();
}

struct Api {
    base: String,
    token: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<crate::Result<()>>,
}
impl Drop for Api {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}
impl Api {
    async fn open(
        dir: &tempfile::TempDir,
        db: &crate::store::Database,
        upstream: Arc<Upstream>,
    ) -> Self {
        let token = db
            .call(|conn| {
                let user = crate::create_user(conn, "synthetic-platform-user")?;
                Ok(crate::create_token(conn, &[7; 32], user, "fixture", 90)?.secret)
            })
            .await
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = listener.local_addr().unwrap();
        drop(listener);
        let config = crate::server::ServeConfig {
            account_maintenance: false,
            gateway_uid: unsafe { libc::geteuid() },
            listen,
            socket: dir.path().join("control.sock"),
            upstream: Some(upstream),
            generations_per_user: 8,
            websockets_per_user: 8,
            fast_mode: false,
        };
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let db = db.clone();
        let task = tokio::spawn(crate::server::serve(db, vec![7; 32], config, async {
            let _ = stopped.await;
        }));
        let base = format!("http://{listen}");
        let http = reqwest::Client::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if http.get(format!("{base}/healthz")).send().await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Self {
            base,
            token,
            stop: Some(stop),
            task,
        }
    }
    fn post(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .post(format!("{}/v1/{path}", self.base))
            .bearer_auth(&self.token)
            .header("x-exetrouter-observe-account", "true")
            .json(body)
    }
}

#[tokio::test]
async fn live_resource_selection_filters_catalog_and_never_submits_to_the_excluded_backend() {
    use codex_websocket::tungstenite::{client::IntoClientRequest, Message};
    use futures_util::{SinkExt, StreamExt};
    let (dir, db, mock, upstream) = fixture().await;
    mock.state
        .lock()
        .unwrap()
        .response_models
        .push("text-embedding-3-small".into());
    inventory::read(
        &db,
        Vault::new([7; 32]),
        &client::Client::new(&mock.base, true).unwrap(),
        1,
    )
    .await
    .unwrap();
    db.call(|conn| {
        conn.execute("INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'gpt-codex-synthetic','Codex fixture')", [])?;
        Resources::Platform.set(conn)
    }).await.unwrap();
    let api = Api::open(&dir, &db, upstream.clone()).await;
    let models = upstream.models().await.unwrap();
    assert!(models.iter().all(|m| m.source == "platform"));
    assert_eq!(models.len(), 3);
    assert!(models.iter().any(|m| m.id == "gpt-6.1-sol"));
    assert!(upstream.select("gpt-codex-synthetic").await.is_err());
    assert!(upstream
        .select_native(
            crate::upstream::native::Feature::Images,
            "gpt-image-2",
            None,
            None
        )
        .await
        .is_err());
    let body = json!({"model":"gpt-6.1-sol","input":"synthetic resource switch"});
    let response = api.post("responses", &body).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let output: Value = response.json().await.unwrap();
    let codex_proof =
        affinity::issue("synthetic-context", affinity::Kind::Content, &[7; 32], 1, 1).unwrap();
    let opaque = json!({"model":"gpt-6.1-sol","input":[{"type":"reasoning","summary":[],"encrypted_content":codex_proof}]});
    assert!(!api
        .post("responses", &opaque)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());

    let url = api.base.replace("http://", "ws://") + "/v1/responses";
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", api.token).parse().unwrap(),
    );
    let (mut ws, _) = codex_websocket::connect_async(request).await.unwrap();
    let frame = json!({"type":"response.create","model":"gpt-6.1-sol","input":[{"role":"user","content":"synthetic WS"}]});
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
    async fn terminal(
        ws: &mut codex_websocket::WebSocketStream<
            codex_websocket::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let frame = ws.next().await.unwrap().unwrap();
                if frame.is_text() {
                    let value: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    if value["type"] == "response.completed" || value["type"] == "error" {
                        return value;
                    }
                }
            }
        })
        .await
        .unwrap()
    }
    assert_eq!(terminal(&mut ws).await["type"], "response.completed");
    db.call(|conn| Resources::Codex.set(conn)).await.unwrap();
    let models = upstream.models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|m| m.source == "codex"));
    let pinned = json!({"model":"gpt-6.1-sol","input":[output["output"][0]]});
    assert!(!api
        .post("responses", &pinned)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    assert_eq!(
        api.post(
            "embeddings",
            &json!({"model":"text-embedding-3-small","input":"synthetic"})
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
    let mut delta = frame;
    delta["previous_response_id"] = json!("resp_platform_2");
    ws.send(Message::Text(delta.to_string().into()))
        .await
        .unwrap();
    assert_eq!(terminal(&mut ws).await["type"], "error");

    // A fresh shared-model request goes to Codex and its refusal cannot use Platform.
    assert_eq!(
        api.post("responses", &body).send().await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let observed = mock.state.lock().unwrap();
    assert_eq!(observed.native_inferences, 1);
    assert_eq!(observed.inferences.len(), 2);
}

#[tokio::test]
async fn embeddings_are_discovered_and_charged_only_to_the_observed_project() {
    let (dir, db, mock, upstream) = fixture().await;
    mock.state.lock().unwrap().response_models.extend([
        "text-embedding-3-small".into(),
        "text-embedding-3-large".into(),
    ]);
    inventory::read(
        &db,
        Vault::new([7; 32]),
        &client::Client::new(&mock.base, true).unwrap(),
        1,
    )
    .await
    .unwrap();
    let api = Api::open(&dir, &db, upstream).await;
    let models: Value = reqwest::Client::new()
        .get(format!("{}/v1/models", api.base))
        .bearer_auth(&api.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries: Vec<_> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["capabilities"] == json!(["embeddings"]))
        .collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|m| m["source"] == "platform"));
    let body = json!({"model":"text-embedding-3-small","input":[[1,2,3]],"encoding_format":"base64","dimensions":1});
    let result = api.post("embeddings", &body).send().await.unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    assert!(result.headers().contains_key("x-exetrouter-account"));
    assert_eq!(result.headers()["x-exetrouter-resource"], "platform");
    assert_eq!(
        result.json::<Value>().await.unwrap()["data"][0]["embedding"],
        "AACAPw=="
    );
    for (path, model) in [
        ("responses", "text-embedding-3-small"),
        ("embeddings", "gpt-6.1-sol"),
        ("embeddings", "text-embedding-ada-002"),
    ] {
        assert_eq!(
            api.post(path, &json!({"model":model,"input":"test"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    db.call(|conn| {let row:(i64,String,i64,i64,i64)=conn.query_row("SELECT account_id,upstream_transport,input_tokens,output_tokens,usd_nanos FROM usage_events JOIN platform_request_costs ON usage_id=usage_events.id WHERE api_surface='embeddings'",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;assert_eq!(row,(1,"platform_http_json".into(),7,0,140));Ok(())}).await.unwrap();
    let observed = mock.state.lock().unwrap();
    assert_eq!(observed.inferences, vec![body]);
    assert_eq!(observed.native_inferences, 0);
}

#[tokio::test]
async fn shared_api_routes_platform_json_sse_chat_compaction_and_ws_with_parent_usage_and_context()
{
    use codex_websocket::tungstenite::{client::IntoClientRequest, Message};
    use futures_util::{SinkExt, StreamExt};
    let (dir, db, mock, upstream) = fixture().await;
    let api = Api::open(&dir, &db, upstream).await;
    let body = json!({"model":"gpt-6-astra","input":"synthetic request","max_output_tokens":32,"client_metadata":{"session_id":"synthetic-session","thread_id":"synthetic-thread"}});
    let response = api.post("responses", &body).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    let opaque = value["output"][0]["encrypted_content"].as_str().unwrap();
    assert!(opaque.starts_with("exrctx1"));
    let input = json!([{"role":"user","content":"synthetic prior request"},value["output"][0].clone(),{"role":"user","content":"synthetic continuation"}]);
    let continued: Value = api
        .post("responses", &json!({"model":"gpt-6-astra","input":input}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(continued["status"], "completed");
    assert_eq!(
        mock.state.lock().unwrap().inferences[1]["input"][1]["encrypted_content"],
        "platform-opaque-fixture"
    );
    let sse = api
        .post(
            "responses",
            &json!({"model":"gpt-6-astra","input":"synthetic stream","stream":true}),
        )
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(sse.contains("response.completed"));
    let chat = api
        .post(
            "chat/completions",
            &json!({"model":"gpt-6-astra","messages":[{"role":"user","content":"synthetic chat"}],"max_tokens":16,"max_completion_tokens":64}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(chat.status(), StatusCode::OK);
    assert_eq!(
        chat.json::<Value>().await.unwrap()["choices"][0]["message"]["content"],
        "synthetic Platform output"
    );
    {
        let state = mock.state.lock().unwrap();
        assert_eq!(state.inferences[0]["max_output_tokens"], 32);
        assert_eq!(state.inferences[3]["max_output_tokens"], 64);
    }
    let compact = api
        .post(
            "responses/compact",
            &json!({"model":"gpt-6-astra","input":input}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(compact.status(), StatusCode::OK);
    assert_eq!(compact.headers()["x-exetrouter-resource"], "platform");
    let compact: Value = compact.json().await.unwrap();
    assert_eq!(compact["object"], "response.compaction");
    assert_eq!(
        compact["output"].as_array().unwrap().len(),
        2,
        "Platform visible prefix is not discarded"
    );
    let compact_body = mock.state.lock().unwrap().inferences[4].clone();
    assert!(compact_body.get("stream").is_none() && compact_body.get("store").is_none());
    assert!(!compact_body.to_string().contains("compaction_trigger"));
    assert!(compact["output"][1]["encrypted_content"]
        .as_str()
        .unwrap()
        .starts_with("exrctx1"));
    let url = api.base.replace("http://", "ws://") + "/v1/responses";
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", api.token).parse().unwrap(),
    );
    let (mut socket, _) = codex_websocket::connect_async(request).await.unwrap();
    socket.send(Message::Text(json!({"type":"response.create","model":"gpt-6-astra","input":[{"role":"user","content":"synthetic WS"}],"generate":false}).to_string().into())).await.unwrap();
    let warm: Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(warm["error"]["code"], "unsupported_parameter");
    for turn in 0..2 {
        let frame = if turn == 0 {
            json!({"type":"response.create","model":"gpt-6-astra","input":[{"role":"user","content":"synthetic WS"}]})
        } else {
            json!({"type":"response.create","model":"gpt-6-astra","previous_response_id":"resp_platform_6","input":[{"role":"user","content":"synthetic WS delta"}]})
        };
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if !frame.is_text() {
                continue;
            }
            let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_ne!(event["type"], "error", "{event}");
            if event["type"] == "response.completed" {
                break;
            }
        }
    }
    socket.close(None).await.unwrap();
    assert_eq!(
        mock.state.lock().unwrap().inferences.len(),
        7,
        "warmup never generates"
    );
    assert!(mock.state.lock().unwrap().inferences[6]
        .get("previous_response_id")
        .is_none());
    db.call(|conn| {
        let rows:i64=conn.query_row("SELECT COUNT(*) FROM usage_events WHERE account_id=1 AND status='completed' AND upstream_transport IN ('platform_http_sse','platform_http_json')",[],|r|r.get(0))?;
        assert_eq!(rows,7);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM usage_events WHERE account_id<0",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM oauth_quota_windows",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn native_handshake_quota_fallback_keeps_downstream_ws_and_recovers_platform_history() {
    use codex_websocket::tungstenite::{client::IntoClientRequest, Message};
    use futures_util::{SinkExt, StreamExt};
    let (dir, db, mock, upstream) = fixture().await;
    db.call(|conn| Resources::Both.set(conn)).await.unwrap();
    let api = Api::open(&dir, &db, upstream).await;
    let url = api.base.replace("http://", "ws://") + "/v1/responses";
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", api.token).parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("x-exetrouter-observe-account", "true".parse().unwrap());
    let (mut ws, _) = codex_websocket::connect_async(request).await.unwrap();
    let parent = crate::account_preferences::observation_key(&[7; 32], 1, 1);
    for turn in 0..2 {
        let mut body = json!({"type":"response.create","model":"gpt-6.1-sol","input":[{"role":"user","content":"synthetic fallback WS context"}]});
        if turn == 1 {
            body["previous_response_id"] = json!("resp_platform_1");
        }
        ws.send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        let mut observed = false;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if !frame.is_text() {
                continue;
            }
            let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_ne!(event["type"], "error", "{event}");
            if event["type"] == "response.metadata" {
                assert_eq!(event["headers"]["x-exetrouter-account"], parent);
                assert_eq!(event["headers"]["x-exetrouter-resource"], "platform");
                observed = true;
            }
            if event["type"] == "response.completed" {
                assert!(observed);
                break;
            }
        }
    }
    ws.close(None).await.unwrap();
    let state = mock.state.lock().unwrap();
    assert_eq!(
        state.native_inferences, 1,
        "only the refused native handshake"
    );
    assert_eq!(state.inferences.len(), 2);
    assert_eq!(
        state.inferences[1]["input"][1]["encrypted_content"],
        "platform-opaque-fixture"
    );
    assert!(state.inferences[1].get("previous_response_id").is_none());
}

#[tokio::test]
async fn quota_refusal_falls_back_same_model_only_when_enabled_and_keeps_each_attempt_durable() {
    let (dir, db, mock, upstream) = fixture().await;
    let api = Api::open(&dir, &db, upstream.clone()).await;
    let body = json!({"model":"gpt-6.1-sol","input":"synthetic native quota refusal","max_output_tokens":32});
    let refused = api.post("responses", &body).send().await.unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(mock.state.lock().unwrap().inferences.is_empty());
    db.call(|conn| Resources::Both.set(conn)).await.unwrap();
    upstream.check_available().await.unwrap();
    let response = api.post("responses", &body).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["status"],
        "completed"
    );
    assert_eq!(mock.state.lock().unwrap().native_inferences, 1);
    assert_eq!(mock.state.lock().unwrap().inferences.len(), 1);
    db.call(|conn| {
        conn.execute(
            "UPDATE oauth_accounts SET cooldown_until=NULL,cooldown_source=NULL WHERE id=1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    // This request starts on native Codex, gets a definitive 429, then uses API
    // in the same downstream request with a separate durable attempt.
    let fallback = api.post("responses", &body).send().await.unwrap();
    assert_eq!(fallback.status(), StatusCode::OK);
    assert_eq!(
        fallback.json::<Value>().await.unwrap()["status"],
        "completed"
    );
    assert_eq!(mock.state.lock().unwrap().native_inferences, 2);
    assert_eq!(mock.state.lock().unwrap().inferences.len(), 2);
    assert_eq!(
        mock.state.lock().unwrap().inferences[1]["max_output_tokens"],
        32
    );
    let proof = affinity::issue(
        "synthetic-codex-checkpoint",
        affinity::Kind::Content,
        &[7; 32],
        1,
        1,
    )
    .unwrap();
    let blocked=api.post("responses",&json!({"model":"gpt-6.1-sol","input":[{"type":"reasoning","summary":[],"encrypted_content":proof}]})).send().await.unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        mock.state.lock().unwrap().inferences.len(),
        2,
        "opaque Codex context must not reach Platform"
    );
    db.call(|conn| {
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM usage_events WHERE status='upstream_rejected' AND account_id=1 AND input_tokens IS NULL",[],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM usage_events WHERE status='completed' AND account_id=1 AND upstream_transport='platform_http_sse' AND input_tokens=11",[],|r|r.get::<_,i64>(0))?,2);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn codex_validation_refusal_never_uses_platform_even_with_an_output_limit() {
    let (dir, db, mock, upstream) = fixture().await;
    db.call(|conn| Resources::Both.set(conn)).await.unwrap();
    mock.state.lock().unwrap().native_inference_bad_request = true;
    let api = Api::open(&dir, &db, upstream).await;
    let reply = api
        .post(
            "responses",
            &json!({"model":"gpt-6.1-sol","input":"synthetic invalid request","max_output_tokens":32}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::BAD_REQUEST);
    assert!(!reply
        .text()
        .await
        .unwrap()
        .contains("synthetic invalid field"));
    let state = mock.state.lock().unwrap();
    assert_eq!(state.native_inferences, 1);
    assert!(state.inferences.is_empty());
}

#[tokio::test]
async fn platform_authentication_and_interruption_never_refresh_oauth_or_replay_inference() {
    for interrupted in [false, true] {
        let (dir, db, mock, upstream) = fixture().await;
        let api = Api::open(&dir, &db, upstream.clone()).await;
        {
            let mut state = mock.state.lock().unwrap();
            if interrupted {
                state.interrupt_inference = true;
            } else {
                state.inference_status = Some(StatusCode::UNAUTHORIZED);
            }
        }
        let response = api
            .post(
                "responses",
                &json!({"model":"gpt-6-astra","input":"synthetic failed request"}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response
            .text()
            .await
            .unwrap()
            .contains("platform-private-payload"));
        assert_eq!(mock.state.lock().unwrap().inferences.len(), 1);
        assert!(matches!(
            upstream.select_pinned("gpt-6-astra", -1).await,
            Err(SelectError::Backoff(_))
        ));
        let blocked = api
            .post(
                "responses",
                &json!({"model":"gpt-6-astra","input":"synthetic separate blocked request"}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(mock.state.lock().unwrap().inferences.len(), 1);
        db.call(|conn| {
            let state:(String,i64,i64)=conn.query_row("SELECT state,generation,expires_at FROM oauth_accounts WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!(state,("active".into(),0,9999999999));
            assert!(crate::health::paused(conn,1,chrono::Utc::now().timestamp())?.is_none());
            assert!(crate::quota::summary(conn,1,chrono::Utc::now().timestamp())?.cooldown_until.is_none());
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM usage_events WHERE account_id=1 AND input_tokens IS NULL AND output_tokens IS NULL",[],|r|r.get::<_,i64>(0))?,1);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM usage_events WHERE status IN ('accepted','sent')",[],|r|r.get::<_,i64>(0))?,0);
            Ok(())
        }).await.unwrap();
    }
}

#[tokio::test]
async fn project_changes_and_route_deletion_never_reassign_an_issued_context_owner() {
    let (_dir, db, _mock, upstream) = fixture().await;
    let old = upstream.account(-1).await.unwrap();
    let new_owner=db.call(|conn| {
        let vault=Vault::new([7;32]);
        let encrypted=vault.seal_secret(b"exetrouter:platform-working:v1:1:proj_second:key_second",WORKING.as_bytes())?;
        conn.execute("UPDATE platform_settings SET project_id='proj_second' WHERE account_id=1",[])?;
        conn.execute("UPDATE platform_working_keys SET project_id='proj_second',key_id='key_second',encrypted_key=?1 WHERE account_id=1",[encrypted])?;
        let working=keys::working_secret(conn,&vault,1,chrono::Utc::now().timestamp())?;
        routes::publish(conn,1,&working.identity,&["gpt-6-astra".into()],chrono::Utc::now().timestamp())?;
        let owner=routes::owners(conn)?[0];
        assert_ne!(owner,-1);
        assert_eq!(routes::parent(conn,owner)?,1);
        conn.execute("DELETE FROM platform_routes WHERE id=?1",[routes::route_id(owner)?])?;
        routes::publish(conn,1,&working.identity,&["gpt-6-astra".into()],chrono::Utc::now().timestamp())?;
        let replacement=routes::owners(conn)?[0];
        assert!(replacement<owner,"route IDs are never reused");
        Ok(replacement)
    }).await.unwrap();
    assert!(upstream.check_cooldown(&old).await.is_err());
    assert!(upstream.select_pinned("gpt-6-astra", -1).await.is_err());
    assert!(upstream
        .select_pinned("gpt-6-astra", new_owner)
        .await
        .is_ok());
}

#[test]
fn platform_adaptation_never_drops_context_and_cross_backend_checks_ignore_tool_schemas() {
    let body = json!({"model":"gpt-6-astra","type":"response.create","generate":true,"stream":true,"store":false,"input":[{"role":"user","content":"synthetic input"},{"type":"function_call","name":"sample","call_id":"c1","arguments":"{}"},{"type":"function_call_output","call_id":"c1","output":"synthetic result","encrypted_function_args":[]}],"tools":[{"type":"function","name":"sample","parameters":{"type":"object","properties":{"file_id":{"type":"string"}}}}],"client_metadata":{"session_id":"scoped-session","x-codex-turn-state":"native-state"}});
    assert!(transfer_safe(&body));
    let adapted = inference_payload(&body, false).unwrap();
    assert_eq!(adapted["input"], body["input"]);
    assert_eq!(adapted["tools"], body["tools"]);
    assert!(adapted.get("client_metadata").is_none());
    for item in [
        json!({"type":"reasoning","encrypted_content":"opaque"}),
        json!({"type":"input_image","file_id":"file_fixture"}),
        json!({"type":"item_reference","id":"item_fixture"}),
    ] {
        let payload = json!({"input":[item]});
        assert!(!transfer_safe(&payload));
    }
    assert!(inference_payload(&json!({"input":[],"generate":false}), false).is_err());
}
