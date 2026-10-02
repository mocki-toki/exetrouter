//! Live subscription limits and explicitly confirmed, single-use reset credits.
//! Credentials and upstream credit IDs stay on the server. No inference is generated.
use crate::{
    doctor::ServerReport,
    oauth::{self, Account},
    upstream::{account_headers, Upstream},
    Result,
};
use chrono::{DateTime, Utc};
use futures_util::{stream, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credit {
    pub title: String,
    pub expires_at: Option<i64>,
    #[serde(skip)]
    id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credits {
    pub available_count: i64,
    pub credits: Vec<Credit>,
    pub details_available: bool,
    pub observed_at: i64,
}
struct Confirmation {
    user: i64,
    account: i64,
    generation: i64,
    credit: Option<String>,
    expires: i64,
    recommend_wait: bool,
}
#[derive(Default)]
pub(crate) struct State {
    credits: HashMap<i64, Credits>,
    confirmations: HashMap<String, Confirmation>,
}
struct Live {
    account: Account,
    quota: crate::quota::Summary,
    credits: Option<Credits>,
}

fn usage_observation(value: &Value, now: i64) -> crate::quota::Observation {
    let mut limits = json!({});
    for kind in ["primary", "secondary"] {
        let w = &value["rate_limit"][format!("{kind}_window")];
        let seconds = w["limit_window_seconds"].as_i64().filter(|s| *s > 0);
        limits[kind] = json!({
            "used_percent": w["used_percent"],
            "window_minutes": seconds.map(|s| s.saturating_add(59) / 60),
            "reset_at": w["reset_at"]
        });
    }
    crate::quota::event(
        &json!({"type":"codex.rate_limits", "rate_limits":limits}),
        now,
    )
}
fn eligible(quota: &crate::quota::Summary, now: i64) -> Result<(f64, i64)> {
    let windows: Vec<_> = quota
        .windows
        .iter()
        .filter(|w| {
            w.status == "current"
                && w.window_minutes.is_some()
                && w.remaining_percent <= 5.0
                && w.reset_at.is_some_and(|at| at > now)
        })
        .collect();
    let remaining = windows.iter().map(|w| w.remaining_percent).reduce(f64::min)
        .ok_or("Reset credits require a live subscription window with 5% or less remaining and a known future reset time")?;
    let reset = windows
        .iter()
        .filter_map(|w| w.reset_at)
        .min()
        .ok_or("reset time unavailable")?;
    Ok((remaining, reset))
}
fn parse_credits(usage: &Value, details: Option<&Value>, now: i64) -> Option<Credits> {
    let count = details
        .and_then(|v| v["available_count"].as_i64())
        .or_else(|| usage["rate_limit_reset_credits"]["available_count"].as_i64())?;
    let mut credits: Vec<_> = details
        .and_then(|v| v["credits"].as_array())
        .into_iter()
        .flatten()
        .filter(|c| c["status"] == "available" && c["reset_type"] == "codex_rate_limits")
        .filter_map(|c| {
            let expires = if c["expires_at"].is_null() {
                None
            } else {
                Some(
                    DateTime::parse_from_rfc3339(c["expires_at"].as_str()?)
                        .ok()?
                        .timestamp(),
                )
            };
            if expires.is_some_and(|at| at <= now) {
                return None;
            }
            let id = c["id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 512)?;
            Some(Credit {
                id: id.into(),
                title: c["title"].as_str().unwrap_or("Full reset").into(),
                expires_at: expires,
            })
        })
        .collect();
    credits.sort_by_key(|c| c.expires_at.unwrap_or(i64::MAX));
    Some(Credits {
        available_count: count.max(0),
        credits,
        details_available: details.is_some_and(|v| v["credits"].is_array()),
        observed_at: now,
    })
}

impl Upstream {
    fn limits_url(&self, path: &str) -> String {
        // Production inference lives at /backend-api/codex; usage/reset operations at /wham.
        let base = if self.config.base_url == crate::upstream::BACKEND {
            "https://chatgpt.com/backend-api/wham"
        } else {
            &self.config.base_url
        };
        format!("{base}/{path}")
    }
    async fn limits_get(&self, account: &Account, path: &str, seconds: u64) -> Result<Value> {
        let response = self
            .client
            .get(self.limits_url(path))
            .headers(account_headers(account)?)
            .timeout(Duration::from_secs(seconds))
            .send()
            .await
            .map_err(|_| "Subscription limits unavailable")?;
        if !response.status().is_success() {
            return Err("Subscription limits rejected by upstream".into());
        }
        oauth::read_json(response, 262_144).await
    }
    async fn live_limits(&self, id: i64) -> Result<Live> {
        let account = self.account(id).await?;
        let generation = account.info.generation;
        // Allocate before GET so concurrent later inference observations always win in storage.
        let order = self.db.call(|conn| crate::health::next(conn)).await?;
        let usage = self.limits_get(&account, "usage", 8).await?;
        let now = Utc::now().timestamp();
        let observation = usage_observation(&usage, now);
        let mut quota = self
            .db
            .call(move |conn| {
                crate::quota::save(conn, id, generation, order, observation, now)?;
                crate::quota::summary(conn, id, now)
            })
            .await?;
        // Never use stored historical percentages as authorization for redemption.
        quota.windows = ["primary", "secondary"]
            .into_iter()
            .filter_map(|kind| {
                let w = &usage["rate_limit"][format!("{kind}_window")];
                let used = w["used_percent"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0)?;
                let seconds = w["limit_window_seconds"].as_i64().filter(|s| *s > 0);
                let reset = w["reset_at"]
                    .as_i64()
                    .filter(|at| (0..=253_402_300_799).contains(at));
                Some(crate::quota::Window {
                    kind: kind.into(),
                    used_percent: used,
                    remaining_percent: (100.0 - used).max(0.0),
                    window_minutes: seconds.map(|s| s.saturating_add(59) / 60),
                    reset_at: reset,
                    observed_at: now,
                    status: if reset.is_some_and(|at| at <= now) {
                        "reset_elapsed"
                    } else {
                        "current"
                    }
                    .into(),
                })
            })
            .collect();
        quota.status = if quota.windows.is_empty() {
            "unknown"
        } else {
            "current"
        }
        .into();
        let details = self
            .limits_get(&account, "rate-limit-reset-credits", 3)
            .await
            .ok();
        let credits = parse_credits(&usage, details.as_ref(), now);
        Ok(Live {
            account,
            quota,
            credits,
        })
    }
    pub(crate) async fn account_metadata(&self, report: &mut ServerReport, live: bool) {
        stream::iter(report.quota_accounts.iter_mut())
            .for_each_concurrent(Some(4), |row| async move {
                let id = row.id;
                let vault = self.vault.clone();
                let email = self
                    .db
                    .call(move |conn| {
                        Ok(oauth::load(conn, &vault, id)
                            .ok()
                            .and_then(|a| oauth::token_email(&a.credentials.access_token)))
                    })
                    .await
                    .ok()
                    .flatten();
                if let Some(email) = email {
                    row.label = email;
                }
                if live {
                    match self.live_limits(id).await {
                        Ok(current) => {
                            row.quota = current.quota;
                            row.reset_credits = current.credits.clone();
                            let mut state = self.resets.lock().await;
                            if let Some(credits) = current.credits {
                                state.credits.insert(id, credits);
                            } else {
                                state.credits.remove(&id);
                            }
                        }
                        Err(_) => {
                            row.refresh_error = Some(
                                "Live subscription limits unavailable; showing stored observations"
                                    .into(),
                            )
                        }
                    }
                } else {
                    row.reset_credits = self.resets.lock().await.credits.get(&id).cloned();
                }
            })
            .await;
        if live && report.quota_accounts.len() == 1 {
            report.quota = Some(report.quota_accounts[0].quota.clone());
        }
    }
    pub(crate) async fn prepare_reset(&self, user: i64, id: i64) -> Result<Value> {
        if !self.account_enabled(user, id).await? {
            return Err("Account is deactivated".into());
        }
        let mut state = self.resets.lock().await;
        let current = self.live_limits(id).await?;
        let now = Utc::now().timestamp();
        let (remaining, reset_at) = eligible(&current.quota, now)?;
        let credits = current
            .credits
            .ok_or("Reset credit availability is unknown")?;
        if credits.available_count == 0 {
            return Err("No reset credits available".into());
        }
        if credits.details_available && credits.credits.is_empty() {
            return Err("No unexpired supported reset credit available".into());
        }
        let selected = credits.credits.first();
        let mut random = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut random);
        // UUID v4 is accepted by the upstream redeem_request_id contract.
        random[6] = (random[6] & 0x0f) | 0x40;
        random[8] = (random[8] & 0x3f) | 0x80;
        let h = hex::encode(random);
        let confirmation = format!(
            "{}-{}-{}-{}-{}",
            &h[..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..]
        );
        state.confirmations.retain(|_, c| c.expires > now);
        if state.confirmations.len() >= 128 {
            return Err("Too many pending reset confirmations; try again later".into());
        }
        state.confirmations.insert(
            confirmation.clone(),
            Confirmation {
                user,
                account: id,
                generation: current.account.info.generation,
                credit: selected.map(|c| c.id.clone()),
                expires: now + 120,
                recommend_wait: reset_at - now < 3 * 86400,
            },
        );
        Ok(
            json!({"confirmation":confirmation,"email":oauth::token_email(&current.account.credentials.access_token).unwrap_or_else(||"Email unavailable".into()),
            "remaining_percent":remaining,"free_reset_at":reset_at,"recommend_wait":reset_at-now < 3*86400,
            "credit_title":selected.map(|c| c.title.as_str()).unwrap_or("Full reset"),
            "credit_expires_at":selected.and_then(|c|c.expires_at),"available_count":credits.available_count,"expires_at":now+120}),
        )
    }
    pub(crate) async fn confirm_reset(
        &self,
        user: i64,
        confirmation: &str,
        identity: &str,
    ) -> Result<Value> {
        self.confirm_reset_authorized(user, confirmation, Some(identity))
            .await
    }
    pub(crate) async fn confirm_local_reset(&self, user: i64, confirmation: &str) -> Result<Value> {
        self.confirm_reset_authorized(user, confirmation, None)
            .await
    }
    async fn confirm_reset_authorized(
        &self,
        user: i64,
        confirmation: &str,
        identity: Option<&str>,
    ) -> Result<Value> {
        let mut state = self.resets.lock().await;
        let preview = state.confirmations.get(confirmation).ok_or(
            "Reset confirmation expired or already submitted; refresh limits before starting again",
        )?;
        if preview.user != user {
            return Err("Reset confirmation belongs to another user".into());
        }
        let preview = state
            .confirmations
            .remove(confirmation)
            .expect("checked confirmation");
        if preview.expires <= Utc::now().timestamp() {
            return Err("Reset confirmation expired; start again".into());
        }
        if !self.account_enabled(user, preview.account).await? {
            return Err("Account is deactivated".into());
        }
        let current = self.live_limits(preview.account).await?;
        let now = Utc::now().timestamp();
        let (_, free_reset) = eligible(&current.quota, now)?;
        if free_reset - now < 3 * 86400 && !preview.recommend_wait {
            return Err("Free reset is now less than 3 days away; start again to review the recommendation to wait".into());
        }
        if current.account.info.generation != preview.generation {
            return Err("OAuth account changed; start again".into());
        }
        let credits = current
            .credits
            .ok_or("Reset credit availability is unknown")?;
        if credits.available_count == 0 {
            return Err("No reset credits available".into());
        }
        if let Some(id) = &preview.credit {
            if !credits.details_available
                || !credits
                    .credits
                    .iter()
                    .any(|c| &c.id == id && c.expires_at.is_none_or(|at| at > now))
            {
                return Err("Selected credit is no longer available; start again".into());
            }
        } else if credits.details_available && credits.credits.is_empty() {
            return Err("No unexpired supported reset credit available".into());
        }
        let identity = identity.map(str::to_owned);
        let id = preview.account;
        let generation = preview.generation;
        self.db.call(move |conn| {
            if let Some(identity) = &identity {
                if crate::resolve_ssh_identity(conn, identity)? != Some(user) { return Err("SSH identity revoked or unknown".into()); }
            } else {
                let owner: bool = conn.query_row("SELECT COUNT(*)=1 AND MIN(name)='standalone' AND MIN(id)=?1 FROM users",[user],|r|r.get(0))?;
                if !owner {return Err("Standalone owner changed".into());}
            }
            let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')",rusqlite::params![id,generation],|r|r.get(0))?;
            if !valid { return Err("OAuth account changed; start again".into()); }
            Ok(())
        }).await?;
        let mut body = json!({"redeem_request_id":confirmation});
        if let Some(id) = preview.credit {
            body["credit_id"] = json!(id);
        }
        let response = self.client.post(self.limits_url("rate-limit-reset-credits/consume"))
            .headers(account_headers(&current.account)?).timeout(Duration::from_secs(10)).json(&body)
            .send().await.map_err(|_| "Reset outcome unknown. Refresh limits before any further action; the request will not be retried")?;
        if !response.status().is_success() {
            return Err(
                if response.status().is_server_error() || response.status().as_u16() == 408 {
                    "Reset outcome unknown. Refresh limits; the request will not be retried"
                } else {
                    "Reset request rejected; refresh limits before trying again"
                }
                .into(),
            );
        }
        let outcome = oauth::read_json(response, 65_536).await.map_err(|_| {
            "Reset outcome unknown. Refresh limits; the request will not be retried"
        })?;
        let code = outcome["code"]
            .as_str()
            .ok_or("Reset outcome unknown; refresh limits")?;
        if !matches!(
            code,
            "reset" | "nothing_to_reset" | "no_credit" | "already_redeemed"
        ) {
            return Err("Reset outcome unknown; refresh limits".into());
        }
        state.credits.remove(&preview.account);
        if code == "reset" {
            // A confirmed reset may release a subscription cooldown, never a transport backoff.
            if let Some(until) = current.account.info.cooldown_until {
                let id = preview.account;
                let generation = preview.generation;
                let order = self
                    .db
                    .call(|conn| crate::health::next(conn))
                    .await
                    .unwrap_or(0);
                let usage = self.limits_get(&current.account, "usage", 8).await.ok();
                if let Some(usage) = usage.filter(|v| v["rate_limit"]["allowed"] == true) {
                    let observation = usage_observation(&usage, Utc::now().timestamp());
                    self.db.call(move |conn| {
                        crate::quota::save(conn,id,generation,order,observation,Utc::now().timestamp())?;
                        conn.execute("UPDATE oauth_accounts SET cooldown_until=NULL,cooldown_source=NULL WHERE id=?1 AND generation=?2 AND cooldown_until=?3 AND cooldown_source='upstream_reset' AND NOT EXISTS(SELECT 1 FROM oauth_quota_windows WHERE account_id=?1 AND request_order>?4)", rusqlite::params![id,generation,until,order])?;
                        Ok(())
                    }).await.ok();
                }
            }
        }
        Ok(json!({"code":code,"windows_reset":outcome["windows_reset"]}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State as AxumState,
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use base64::{
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
        Engine,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    struct Mock {
        usage: Mutex<Value>,
        details: Mutex<Value>,
        bodies: Mutex<Vec<Value>>,
        status: AtomicUsize,
    }
    struct Fixture {
        upstream: Upstream,
        mock: Arc<Mock>,
        identity: String,
        server: tokio::task::JoinHandle<()>,
        _dir: tempfile::TempDir,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }
    async fn usage(AxumState(m): AxumState<Arc<Mock>>) -> Json<Value> {
        Json(m.usage.lock().unwrap().clone())
    }
    async fn details(AxumState(m): AxumState<Arc<Mock>>) -> Json<Value> {
        Json(m.details.lock().unwrap().clone())
    }
    async fn consume(
        AxumState(m): AxumState<Arc<Mock>>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        m.bodies.lock().unwrap().push(body);
        (
            StatusCode::from_u16(m.status.load(Ordering::SeqCst) as u16).unwrap(),
            Json(json!({"code":"reset","windows_reset":2})),
        )
    }
    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = crate::store::Database::open(dir.path().join("state.sqlite"))
                .await
                .unwrap();
            let identity = db.call(|conn| {
                let user = crate::create_user(conn,"test")?;
                let mut blob=Vec::new(); blob.extend_from_slice(&11u32.to_be_bytes()); blob.extend_from_slice(b"ssh-ed25519"); blob.extend_from_slice(&32u32.to_be_bytes());blob.extend_from_slice(&[42;32]);
                let identity=crate::register_ssh_key(conn,user,&format!("ssh-ed25519 {}",STANDARD.encode(blob)))?;
                let claims=json!({"https://api.openai.com/profile":{"email":"test@example.com"},"chatgpt_account_id":"test-account"});
                let access=format!("x.{}.x",URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?));
                oauth::save(conn,&oauth::Vault::new([2;32]),"test-account",&oauth::Credentials {access_token:access,refresh_token:"synthetic".into()},Utc::now().timestamp()+3600)?;
                Ok(identity.id)
            }).await.unwrap();
            let now = Utc::now().timestamp();
            let mock = Arc::new(Mock {
                usage: Mutex::new(
                    json!({"rate_limit":{"allowed":true,"primary_window":{"used_percent":0,"limit_window_seconds":18000,"reset_at":now+3600},"secondary_window":{"used_percent":95,"limit_window_seconds":604800,"reset_at":now+2*86400}},"rate_limit_reset_credits":{"available_count":2}}),
                ),
                details: Mutex::new(
                    json!({"available_count":2,"credits":[{"id":"later","title":"Full reset","status":"available","reset_type":"codex_rate_limits","expires_at":"2099-01-01T00:00:00Z"},{"id":"earlier","title":"Full reset","status":"available","reset_type":"codex_rate_limits","expires_at":"2098-01-01T00:00:00Z"}]}),
                ),
                bodies: Mutex::new(Vec::new()),
                status: AtomicUsize::new(200),
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new()
                .route("/usage", get(usage))
                .route("/rate-limit-reset-credits", get(details))
                .route("/rate-limit-reset-credits/consume", post(consume))
                .with_state(mock.clone());
            let server = tokio::spawn(async {
                axum::serve(listener, app).await.unwrap();
            });
            let upstream = Upstream::new(
                db,
                [2; 32],
                crate::upstream::Config::new(url.clone(), url, true).unwrap(),
            )
            .unwrap();
            Self {
                upstream,
                mock,
                identity,
                server,
                _dir: dir,
            }
        }
        fn used(&self, used: f64) {
            self.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] =
                json!(used);
        }
        async fn preview(&self) -> Value {
            self.upstream.prepare_reset(1, 1).await.unwrap()
        }
    }
    #[tokio::test]
    async fn credit_requires_confirmation_belongs_to_user_and_is_submitted_once_with_idempotency() {
        let f = Fixture::new().await;
        let preview = f.preview().await;
        assert_eq!(preview["email"], "test@example.com");
        assert_eq!(preview["remaining_percent"].as_f64(), Some(5.0));
        assert_eq!(preview["recommend_wait"], true);
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        let key = preview["confirmation"].as_str().unwrap();
        assert!(f.upstream.confirm_reset(2, key, &f.identity).await.is_err());
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        let outcome = f.upstream.confirm_reset(1, key, &f.identity).await.unwrap();
        assert_eq!(outcome["code"], "reset");
        assert!(f.upstream.confirm_reset(1, key, &f.identity).await.is_err());
        let bodies = f.mock.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["redeem_request_id"], key);
        assert_eq!(bodies[0]["credit_id"], "earlier");
    }
    #[tokio::test]
    async fn fresh_threshold_is_checked_on_prepare_and_again_on_confirm() {
        let f = Fixture::new().await;
        let preview = f.preview().await;
        f.used(94.999); // Stored observation is still eligible; the live preflight must win.
        assert!(f.upstream.prepare_reset(1, 1).await.is_err());
        assert!(f
            .upstream
            .confirm_reset(1, preview["confirmation"].as_str().unwrap(), &f.identity)
            .await
            .is_err());
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        f.used(100.0);
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["reset_at"] =
            json!(Utc::now().timestamp() - 1);
        assert!(f.upstream.prepare_reset(1, 1).await.is_err());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"] = Value::Null;
        assert!(f.upstream.prepare_reset(1, 1).await.is_err());
    }
    #[tokio::test]
    async fn expired_confirmation_credit_or_revoked_ssh_identity_never_consumes() {
        let f = Fixture::new().await;
        let preview = f.preview().await;
        let key = preview["confirmation"].as_str().unwrap();
        f.upstream
            .resets
            .lock()
            .await
            .confirmations
            .get_mut(key)
            .unwrap()
            .expires = 0;
        assert!(f.upstream.confirm_reset(1, key, &f.identity).await.is_err());
        let preview = f.preview().await;
        f.mock.details.lock().unwrap()["credits"][1]["expires_at"] = json!("2000-01-01T00:00:00Z");
        assert!(f
            .upstream
            .confirm_reset(1, preview["confirmation"].as_str().unwrap(), &f.identity)
            .await
            .is_err());
        let preview = f.preview().await;
        let identity = f.identity.clone();
        f.upstream
            .db
            .call(move |conn| {
                crate::revoke_ssh_key(conn, &identity)?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(f
            .upstream
            .confirm_reset(1, preview["confirmation"].as_str().unwrap(), &f.identity)
            .await
            .is_err());
        assert!(f.mock.bodies.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn ambiguous_response_is_not_replayed_and_far_free_reset_has_no_wait_warning() {
        let f = Fixture::new().await;
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["reset_at"] =
            json!(Utc::now().timestamp() + 4 * 86400);
        let preview = f.preview().await;
        assert_eq!(preview["recommend_wait"], false);
        f.mock.status.store(500, Ordering::SeqCst);
        let key = preview["confirmation"].as_str().unwrap();
        assert!(f
            .upstream
            .confirm_reset(1, key, &f.identity)
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown"));
        assert!(f.upstream.confirm_reset(1, key, &f.identity).await.is_err());
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn unsupported_or_empty_credit_inventory_fails_closed() {
        let f = Fixture::new().await;
        f.mock.details.lock().unwrap()["available_count"] = json!(0);
        assert!(f.upstream.prepare_reset(1, 1).await.is_err());
        f.mock.details.lock().unwrap()["available_count"] = json!(2);
        for row in f.mock.details.lock().unwrap()["credits"]
            .as_array_mut()
            .unwrap()
        {
            row["reset_type"] = json!("unknown");
        }
        assert!(f.upstream.prepare_reset(1, 1).await.is_err());
        assert!(f.mock.bodies.lock().unwrap().is_empty());
    }
}
