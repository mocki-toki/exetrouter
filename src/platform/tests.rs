use super::*;
use axum::{
    body::Bytes,
    extract::State as HttpState,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
#[path = "routing_tests.rs"]
mod routing_tests;

const ADMIN: &str = concat!("sk-admin-", "synthetic-platform-fixture-only");
const WORKING: &str = concat!("sk-proj-", "synthetic-platform-fixture-only");
const BOOTSTRAP: &str = concat!("sk-proj-", "synthetic-bootstrap-fixture-only");
#[derive(Default)]
struct Observations {
    posts: usize,
    reads: usize,
    service: bool,
    active: bool,
    key: bool,
    created_at: Option<i64>,
    lose_key_reply: bool,
    lose_service_reply: bool,
    bootstrap_created_at: Option<i64>,
    bootstrap_pending: bool,
    bootstrap_long_lived: bool,
    bootstrap_zero_lived: bool,
    bad_costs: bool,
    bad_models: bool,
    models_status: Option<StatusCode>,
    pause_costs: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    pause_models: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    response_models: Vec<String>,
    inferences: Vec<Value>,
    inference_status: Option<StatusCode>,
    native_inferences: usize,
    interrupt_inference: bool,
}
struct Mock {
    base: String,
    state: Arc<Mutex<Observations>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn open() -> Self {
        let state = Arc::new(Mutex::new(Observations::default()));
        let router = Router::new().fallback(any(reply)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { base, state, task }
    }
}
async fn reply(
    HttpState(state): HttpState<Arc<Mutex<Observations>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let pause = if uri.path() == "/organization/costs" {
        state.lock().unwrap().pause_costs.clone()
    } else if uri.path() == "/models" {
        state.lock().unwrap().pause_models.clone()
    } else {
        None
    };
    if let Some((entered, release)) = pause {
        entered.notify_one();
        release.notified().await;
    }
    let mut state = state.lock().unwrap();
    let now = chrono::Utc::now().timestamp();
    if method == Method::POST {
        state.posts += 1;
    } else {
        state.reads += 1;
    }
    let path = uri.path();
    if matches!(path, "/responses" | "/responses/compact") {
        if headers.get("authorization").unwrap() == "Bearer synthetic-access" {
            state.native_inferences += 1;
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error":{"type":"rate_limit_error","code":"rate_limit_exceeded"}})),
            )
                .into_response();
        }
        assert_eq!(
            headers.get("authorization").unwrap(),
            &format!("Bearer {WORKING}")
        );
        for name in [
            "chatgpt-account-id",
            "x-codex-turn-state",
            "session-id",
            "thread-id",
            "session_id",
            "openai-beta",
            "x-openai-internal-codex-responses-lite",
        ] {
            assert!(!headers.contains_key(name));
        }
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body.get("client_metadata").is_none());
        assert!(body.get("generate").is_none());
        let model = body["model"].clone();
        state.inferences.push(body);
        if let Some(status) = state.inference_status {
            return (status,Json(json!({"error":{"code":"invalid_api_key","message":"platform-private-payload"}}))).into_response();
        }
        if path == "/responses/compact" {
            return Json(json!({"object":"response.compaction","id":"cmp_fixture","created_at":now,"output":[{"type":"message","role":"user","content":[{"type":"input_text","text":"synthetic compacted prefix"}]},{"type":"compaction","encrypted_content":"platform-checkpoint"}],"usage":{"input_tokens":8,"output_tokens":3,"total_tokens":11}})).into_response();
        }
        let id = format!("resp_platform_{}", state.inferences.len());
        let events = [
            json!({"type":"response.created","response":{"id":id,"model":model,"created_at":now,"status":"in_progress"}}),
            json!({"type":"response.completed","response":{"id":id,"model":model,"created_at":now,"status":"completed","output":[{"id":"rs_fixture","type":"reasoning","summary":[],"encrypted_content":"platform-opaque-fixture"},{"id":"msg_fixture","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"synthetic Platform output","annotations":[]}]}],"usage":{"input_tokens":11,"output_tokens":5,"input_tokens_details":{"cached_tokens":2},"output_tokens_details":{"reasoning_tokens":1},"total_tokens":16}}}),
        ];
        let events = if state.interrupt_inference {
            &events[..1]
        } else {
            &events[..]
        };
        let sse = events
            .iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect::<String>();
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    if path == "/models" && headers.get("authorization").unwrap() == "Bearer synthetic-access" {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {}", if path == "/models" { WORKING } else { ADMIN })
    );
    let value = if path == "/organization/costs" {
        let url = reqwest::Url::parse(&format!("http://localhost{uri}")).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let from = query["start_time"].parse::<i64>().unwrap();
        let to = query["end_time"].parse::<i64>().unwrap();
        let points:Vec<_>=(from..to).step_by(86400).map(|day|json!({"object":"bucket","start_time":day,"end_time":(day+86400).min(to),"results":[{"object":"organization.costs.result","amount":{"value":0.25,"currency":if state.bad_costs {"invalid"}else{"usd"}},"private_canary":"platform-private-payload"}]})).collect();
        json!({"object":"page","has_more":false,"next_page":null,"data":points})
    } else if path == "/organization/projects/proj_fixture" {
        json!({"id":"proj_fixture","status":"active","name":"Synthetic"})
    } else if path.ends_with("/service_accounts") {
        if method == Method::POST {
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body, json!({"name":"exetrouter","expires_in_seconds":1}));
            state.service = true;
            state.active = true;
            let created = if state.bootstrap_pending {
                now + 30
            } else {
                now - 2
            };
            state.bootstrap_created_at = Some(created);
            if state.lose_service_reply {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("platform-private-payload {BOOTSTRAP}"),
                )
                    .into_response();
            }
            json!({"object":"organization.project.service_account","id":"svc_fixture","name":"exetrouter","role":"member","api_key":{"object":"organization.project.service_account.api_key","id":"key_bootstrap","name":"Secret Key","value":BOOTSTRAP,"created_at":created,"expires_at":created+if state.bootstrap_long_lived {86400}else if state.bootstrap_zero_lived {0}else{1}}})
        } else {
            json!({"object":"list","has_more":false,"data":if state.service {vec![json!({"object":"organization.project.service_account","id":"svc_fixture","name":"exetrouter","role":if state.active {"member"}else{"none"}})]}else{vec![]}})
        }
    } else if path.ends_with("/service_accounts/svc_fixture") {
        return StatusCode::NOT_FOUND.into_response();
    } else if path.ends_with("/service_accounts/svc_fixture/api_keys") && method == Method::POST {
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["expires_in_seconds"], 30 * 86400);
        assert_eq!(body["name"], "exetrouter");
        assert_eq!(
            body["scopes"],
            json!(["api.model.read", "api.responses.write"])
        );
        state.key = true;
        state.created_at = Some(now);
        if state.lose_key_reply {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("platform-private-payload {WORKING}"),
            )
                .into_response();
        }
        json!({"object":"organization.project.service_account.api_key","id":"key_fixture","name":"exetrouter","value":WORKING,"created_at":now,"expires_at":now+30*86400})
    } else if path.ends_with("/api_keys") {
        let mut rows = Vec::new();
        if let Some(created) = state.bootstrap_created_at {
            rows.push(json!({"id":"key_bootstrap","name":"Secret Key","created_at":created,"expires_at":created+if state.bootstrap_zero_lived {0}else{1},"owner_project_access":"active","owner":{"type":"service_account","service_account":{"id":"svc_fixture"}}}));
        }
        if state.key {
            rows.push(json!({"id":"key_fixture","name":"exetrouter","created_at":state.created_at.unwrap(),"expires_at":state.created_at.unwrap()+30*86400,"owner_project_access":"active","owner":{"type":"service_account","service_account":{"id":"svc_fixture"}},"last_used_at":null}));
        }
        json!({"object":"list","has_more":false,"data":rows})
    } else if path == "/models" {
        if let Some(status) = state.models_status {
            return (status, "platform-private-payload").into_response();
        }
        if state.bad_models {
            json!({"object":"list","data":[{"id":"invalid"}]})
        } else if !state.response_models.is_empty() {
            json!({"object":"list","data":state.response_models.iter().map(|id|json!({"object":"model","id":id})).collect::<Vec<_>>()})
        } else {
            json!({"object":"list","data":[{"object":"model","id":"synthetic-response-model","owned_by":"platform-private-payload","secret":"platform-private-payload"}]})
        }
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(value).into_response()
}
async fn database() -> (tempfile::TempDir, crate::store::Database, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Database::open(dir.path().join("state.sqlite"))
        .await
        .unwrap();
    let vault = Vault::new([7; 32]);
    let v = vault.clone();
    db.call(move|conn| {
        crate::oauth::save(conn,&v,"fixture-account",&crate::oauth::Credentials{access_token:"synthetic-access".into(),refresh_token:"synthetic-refresh".into()},9999999999)?;
        configure(conn,&v,1,ADMIN.as_bytes(),chrono::Utc::now().timestamp())?;
        conn.execute("INSERT INTO platform_settings(account_id,project_id,manage_keys) VALUES(1,'proj_fixture',1)",[])?;Ok(())
    }).await.unwrap();
    (dir, db, vault)
}
async fn next_refresh(db: &crate::store::Database) {
    db.call(|conn| {
        conn.execute("UPDATE platform_refresh SET started_at=started_at-1000", [])?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn roleless_accounts_and_legacy_uncertain_role_operations_are_never_automatically_replaced() {
    for uncertain in [false, true] {
        let (_dir, db, vault) = database().await;
        let mock = Mock::open().await;
        {
            let mut observed = mock.state.lock().unwrap();
            observed.service = true;
        }
        if uncertain {
            db.call(|conn| {conn.execute("INSERT INTO platform_key_operations(account_id,project_id,operation,owner,attempted_at,status) VALUES(1,'proj_fixture','activate_service_account','previous-owner',0,'unknown')",[])?;Ok(())}).await.unwrap();
        }
        let client = client::Client::new(&mock.base, true).unwrap();
        assert!(client::refresh(&db, vault, &client, 1, true).await.is_err());
        assert_eq!(
            mock.state.lock().unwrap().posts,
            0,
            "never retry role assignment, delete accounts or clear existing fences"
        );
    }
}

#[tokio::test]
async fn bootstrap_is_encrypted_never_routable_and_must_expire_before_the_named_key() {
    let (dir, db, vault) = database().await;
    let mock = Mock::open().await;
    mock.state.lock().unwrap().bootstrap_pending = true;
    let client = client::Client::new(&mock.base, true).unwrap();
    assert!(client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 1);
    assert!(!mock.state.lock().unwrap().key);
    let v = vault.clone();
    db.call(move |conn| {
        assert_eq!(
            keys::snapshot(conn, 1, chrono::Utc::now().timestamp())?
                .unwrap()
                .state,
            "inactive"
        );
        assert!(keys::working_secret(conn, &v, 1, chrono::Utc::now().timestamp()).is_err());
        verify_credentials(conn, &v)?;
        Ok(())
    })
    .await
    .unwrap();
    let created = chrono::Utc::now().timestamp() - 2;
    mock.state.lock().unwrap().bootstrap_created_at = Some(created);
    db.call(move |conn| {
        conn.execute(
            "UPDATE platform_working_keys SET created_at=?1,expires_at=?2",
            params![created, created + 1],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    next_refresh(&db).await;
    client::refresh(&db, vault, &client, 1, true).await.unwrap();
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    for path in [
        dir.path().join("state.sqlite"),
        dir.path().join("state.sqlite-wal"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            assert!(!bytes
                .windows(BOOTSTRAP.len())
                .any(|value| value == BOOTSTRAP.as_bytes()));
        }
    }
}

#[tokio::test]
async fn rounded_already_expired_bootstrap_key_does_not_block_named_key_provisioning() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    mock.state.lock().unwrap().bootstrap_zero_lived = true;
    let client = client::Client::new(&mock.base, true).unwrap();
    client::refresh(&db, vault, &client, 1, true).await.unwrap();
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    assert_eq!(
        db.call(|conn| snapshot(conn, 1))
            .await
            .unwrap()
            .unwrap()
            .key
            .unwrap()
            .state,
        "active"
    );
}

#[tokio::test]
async fn lost_or_unbounded_initial_key_reply_is_fenced_without_a_second_service_account() {
    for lost in [true, false] {
        let (_dir, db, vault) = database().await;
        let mock = Mock::open().await;
        mock.state.lock().unwrap().lose_service_reply = lost;
        mock.state.lock().unwrap().bootstrap_long_lived = !lost;
        let client = client::Client::new(&mock.base, true).unwrap();
        assert!(client::refresh(&db, vault.clone(), &client, 1, true)
            .await
            .is_err());
        assert_eq!(mock.state.lock().unwrap().posts, 1);
        assert!(mock.state.lock().unwrap().active);
        assert!(!mock.state.lock().unwrap().key);
        next_refresh(&db).await;
        assert!(client::refresh(&db, vault, &client, 1, true).await.is_err());
        assert_eq!(
            mock.state.lock().unwrap().posts,
            1,
            "an unknown creation POST still requires explicit operator reconciliation"
        );
    }
}

#[tokio::test]
async fn real_costs_and_thirty_day_key_lifecycle_are_shared_bounded_encrypted_and_non_generating() {
    let (dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    let (first, second) = tokio::join!(
        client::refresh(&db, vault.clone(), &client, 1, true),
        client::refresh(&db, vault.clone(), &client, 1, true)
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    let value = db
        .call(|conn| Ok(serde_json::to_value(snapshot(conn, 1)?)?))
        .await
        .unwrap();
    assert_eq!(value["key"]["state"], "active");
    assert_eq!(value["costs"]["complete"], true);
    assert!(value["costs"]["spent_usd_nanos"].as_i64().unwrap() > 0);
    assert_eq!(value["inference_enabled"], false);
    assert!(
        !value.to_string().contains("platform-private-payload")
            && !value.to_string().contains("sk-")
    );
    next_refresh(&db).await;
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    assert_eq!(
        mock.state.lock().unwrap().posts,
        2,
        "a live stored key must not be recreated"
    );
    let v = vault.clone();
    db.call(move |conn| {
        verify_credentials(conn, &v)?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM platform_cost_history", [], |r| {
            r.get(0)
        })?;
        assert!(count <= 91);
        let h = crate::quota_history::range(conn, 1, "90d", None, chrono::Utc::now().timestamp())?;
        assert!(h.platform.unwrap().points.len() > 80);
        assert!(spending_permitted(conn, false)?);
        assert!(!spending_permitted(conn, true)?);
        set_codex_fallback(conn, true)?;
        assert!(spending_permitted(conn, true)?);
        assert!(spending_permitted(conn, false)?);
        Ok(())
    })
    .await
    .unwrap();
    for path in [
        dir.path().join("state.sqlite"),
        dir.path().join("state.sqlite-wal"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            for secret in [ADMIN, WORKING, BOOTSTRAP, "platform-private-payload"] {
                assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
            }
        }
    }
}
#[tokio::test]
async fn lost_one_time_secret_is_never_recreated_after_restart_admin_rotation_or_removal() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    mock.state.lock().unwrap().lose_key_reply = true;
    let client = client::Client::new(&mock.base, true).unwrap();
    let err = client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains(WORKING) && !err.contains("platform-private-payload"));
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    db.call(|conn| {
        conn.execute(
            "UPDATE platform_key_operations SET attempted_at=attempted_at-121",
            [],
        )?;
        crate::init(conn)?;
        Ok(())
    })
    .await
    .unwrap();
    next_refresh(&db).await;
    assert!(client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .is_err());
    command(&db, vault.clone(), &Command::RemoveAdminKey { id: 1 })
        .await
        .unwrap();
    let v = vault.clone();
    db.call(move|conn|{configure(conn,&v,1,ADMIN.as_bytes(),chrono::Utc::now().timestamp())?;conn.execute("INSERT INTO platform_settings(account_id,project_id,manage_keys) VALUES(1,'proj_fixture',1)",[])?;Ok(())}).await.unwrap();
    assert!(client::refresh(&db, vault, &client, 1, true).await.is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    let value = db.call(|conn| snapshot(conn, 1)).await.unwrap().unwrap();
    assert_eq!(value.key.unwrap().state, "operation_uncertain");
}

#[tokio::test]
async fn manual_balance_org_refresh_uses_an_independent_exact_range_without_inference() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    let from = chrono::Utc::now().timestamp() - 1234;
    db.call(move |conn| {
        balance::set(conn, 1, 10000, from)?;
        Ok(())
    })
    .await
    .unwrap();
    client::refresh(&db, vault, &client, 1, true).await.unwrap();
    let b = db
        .call(|conn| balance::snapshot(conn, 1, chrono::Utc::now().timestamp()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.exetrouter.remaining_usd_nanos, Some(100_000_000_000));
    assert_eq!(b.organization.spent_usd_nanos, Some(250_000_000));
    assert_eq!(b.organization.remaining_usd_nanos, Some(99_750_000_000));
    assert!(mock.state.lock().unwrap().inferences.is_empty());
}
#[tokio::test]
async fn expired_managed_keys_renew_once_and_deactivation_forbids_new_mutations() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    let created = chrono::Utc::now().timestamp() - 31 * 86400;
    mock.state.lock().unwrap().created_at = Some(created);
    db.call(move |conn| {
        conn.execute(
            "UPDATE platform_working_keys SET created_at=?1,expires_at=?2",
            params![created, created + 30 * 86400],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    next_refresh(&db).await;
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    assert_eq!(mock.state.lock().unwrap().posts, 3);
    db.call(|conn| {
        crate::account_preferences::set_policy(conn, 1, Some(false), None, None)?;
        conn.execute("DELETE FROM platform_working_keys", [])?;
        Ok(())
    })
    .await
    .unwrap();
    mock.state.lock().unwrap().key = false;
    next_refresh(&db).await;
    assert!(client::refresh(&db, vault, &client, 1, true).await.is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 3);
}

#[tokio::test]
async fn migration_rollback_keeps_unknown_mutations_keys_and_policy_and_retention_is_clock_safe() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    db.call(move|conn| {
        set_codex_fallback(conn,true)?;
        conn.pragma_update(None,"user_version",17)?;crate::init(conn)?;
        verify_credentials(conn,&vault)?;assert!(codex_fallback_allowed(conn)?);
        let day=chrono::Utc::now().timestamp()/86400*86400;
        for n in 0..800 {conn.execute("INSERT OR REPLACE INTO platform_cost_history(account_id,generation,day,end_at,observed_at,usd_nanos) VALUES(1,1,?1,?2,?2,1)",params![day+n*86400,day+(n+1)*86400])?;}
        prune(conn,day+800*86400)?;prune(conn,day)?;
        assert!(conn.query_row("SELECT COUNT(*) FROM platform_cost_history",[],|r|r.get::<_,i64>(0))?<=366);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn credentials_changed_during_a_read_cannot_publish_or_start_mutations() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    mock.state.lock().unwrap().pause_costs = Some((entered.clone(), release.clone()));
    let client = client::Client::new(&mock.base, true).unwrap();
    let task_db = db.clone();
    let task_vault = vault.clone();
    let task =
        tokio::spawn(async move { client::refresh(&task_db, task_vault, &client, 1, true).await });
    entered.notified().await;
    db.call(move |conn| {
        configure(
            conn,
            &vault,
            1,
            ADMIN.as_bytes(),
            chrono::Utc::now().timestamp(),
        )?;
        Ok(())
    })
    .await
    .unwrap();
    release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 0);
    db.call(|conn| {
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM platform_cost_history", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn backup_key_verification_authenticates_working_key_identity_and_ciphertext() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    db.call(move |conn| {
        verify_credentials(conn, &vault)?;
        conn.execute(
            "UPDATE platform_working_keys SET key_id='different_fixture'",
            [],
        )?;
        assert!(verify_credentials(conn, &vault).is_err());
        conn.execute(
            "UPDATE platform_working_keys SET key_id='key_fixture',encrypted_key=zeroblob(100)",
            [],
        )?;
        assert!(verify_credentials(conn, &vault).is_err());
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_costs_are_unknown_not_zero_and_cannot_trigger_key_mutations() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    mock.state.lock().unwrap().bad_costs = true;
    let client = client::Client::new(&mock.base, true).unwrap();
    assert!(client::refresh(&db, vault, &client, 1, true).await.is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 0);
    let value = db.call(|conn| snapshot(conn, 1)).await.unwrap().unwrap();
    assert_eq!(value.costs.unwrap().spent_usd_nanos, None);
}
#[test]
fn endpoints_are_fixed_and_never_accept_credentials_or_nonliteral_mock_hosts() {
    for base in [
        "https://example.com/v1",
        "http://localhost/v1",
        "http://127.0.0.1:1234/v1?secret=x",
        "http://user@127.0.0.1/v1",
    ] {
        assert!(client::Client::new(base, true).is_err());
    }
}

#[tokio::test]
async fn model_availability_uses_the_working_key_and_never_advertises_routing_or_guessed_capabilities(
) {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    let posts = mock.state.lock().unwrap().posts;
    let value = inventory::read(&db, vault.clone(), &client, 1)
        .await
        .unwrap();
    assert_eq!(value["data"][0]["id"], "synthetic-response-model");
    assert_eq!(value["data"][0]["display_name"], "synthetic response model");
    assert_eq!(value["data"][0]["source"], "platform");
    assert_eq!(value["data"][0]["capabilities"], json!([]));
    assert_eq!(value["routing_enabled"], false);
    assert!(!value.to_string().contains("platform-private-payload"));
    assert!(super::render(&value).contains("Availability only"));
    assert_eq!(mock.state.lock().unwrap().posts, posts);
    assert!(client.models(ADMIN).await.is_err());
    db.call(|conn| {
        conn.execute(
            "UPDATE account_platform_credentials SET generation=generation+1 WHERE account_id=1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let reads = mock.state.lock().unwrap().reads;
    assert!(inventory::read(&db, vault, &client, 1).await.is_err());
    assert_eq!(mock.state.lock().unwrap().reads, reads);
}

#[tokio::test]
async fn model_discovery_cannot_publish_after_credential_replacement() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    mock.state.lock().unwrap().pause_models = Some((entered.clone(), release.clone()));
    let discovery = inventory::read(&db, vault.clone(), &client, 1);
    let replace = async {
        entered.notified().await;
        db.call(move |conn| {
            configure(
                conn,
                &vault,
                1,
                ADMIN.as_bytes(),
                chrono::Utc::now().timestamp(),
            )?;
            Ok(())
        })
        .await
        .unwrap();
        release.notify_one();
    };
    let (result, ()) = tokio::join!(discovery, replace);
    assert!(result.is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 2);
}

#[tokio::test]
async fn failed_model_authentication_never_leaves_a_key_active_or_recreates_its_secret() {
    let (_dir, db, vault) = database().await;
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    mock.state.lock().unwrap().models_status = Some(StatusCode::UNAUTHORIZED);
    assert!(client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .is_err());
    let value = db.call(|conn| snapshot(conn, 1)).await.unwrap().unwrap();
    assert_eq!(value.key.unwrap().state, "inactive");
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    db.call(|conn| {
        let status: String = conn.query_row(
            "SELECT status FROM platform_key_operations WHERE account_id=1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(status, "confirmed");
        Ok(())
    })
    .await
    .unwrap();
    mock.state.lock().unwrap().models_status = None;
    next_refresh(&db).await;
    client::refresh(&db, vault.clone(), &client, 1, true)
        .await
        .unwrap();
    assert_eq!(
        db.call(|conn| snapshot(conn, 1))
            .await
            .unwrap()
            .unwrap()
            .key
            .unwrap()
            .state,
        "active"
    );
    assert_eq!(mock.state.lock().unwrap().posts, 2);
    mock.state.lock().unwrap().bad_models = true;
    next_refresh(&db).await;
    assert!(client::refresh(&db, vault, &client, 1, true).await.is_err());
    assert_eq!(
        db.call(|conn| snapshot(conn, 1))
            .await
            .unwrap()
            .unwrap()
            .key
            .unwrap()
            .state,
        "inactive"
    );
    assert_eq!(mock.state.lock().unwrap().posts, 2);
}

#[tokio::test]
async fn management_allowlist_rejects_url_normalization_and_unrequested_mutation_paths_before_send()
{
    let mock = Mock::open().await;
    let client = client::Client::new(&mock.base, true).unwrap();
    let access = client::Access {
        account: 1,
        generation: 1,
        owner: "synthetic".into(),
        admin: ADMIN.into(),
        project: None,
        manage_keys: false,
    };
    for path in [
        "/models",
        "/responses",
        "/organization/../responses",
        "/organization/projects/proj_fixture/../../responses",
        "/organization/projects/%2e%2e/responses",
        "/organization/projects/proj_fixture?x=y",
        "/organization/projects/proj_fixture/response",
        "/organization/admin_api_keys",
        "/projects/proj_fixture/roles",
        "/projects/proj_fixture/users/svc_fixture/roles",
    ] {
        assert!(client
            .request(&access, Method::GET, path, &[], None)
            .await
            .is_err());
    }
    assert!(client
        .request(
            &access,
            Method::DELETE,
            "/organization/projects/proj_fixture",
            &[],
            None
        )
        .await
        .is_err());
    assert_eq!(mock.state.lock().unwrap().posts, 0);
    assert_eq!(mock.state.lock().unwrap().reads, 0);
}
