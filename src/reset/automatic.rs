use super::*;
use crate::routing::{weekly, DAY};
use rand::Rng;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

pub(super) struct Pending {
    digest: [u8; 32],
    cycle: [u8; 32],
    due: tokio::time::Instant,
}

pub(super) fn digest(id: &str) -> [u8; 32] {
    Sha256::digest([b"exetrouter-automatic-reset-v1\0".as_slice(), id.as_bytes()].concat()).into()
}
fn request_id(digest: &[u8; 32]) -> String {
    // All hosts share one key for this upstream account's weekly quota cycle,
    // even when a credit disappears before its quota recovery becomes visible.
    let mut bytes = digest[..16].to_vec();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}
fn eligible(current: &Live, now: i64) -> Option<&Credit> {
    eligible_metadata(&current.quota, current.credits.as_ref()?, now)
}
pub(super) fn eligible_metadata<'a>(
    quota: &crate::quota::Summary,
    credits: &'a Credits,
    now: i64,
) -> Option<&'a Credit> {
    if !credits.details_available
        || credits.available_count <= 0
        || !crate::routing::fresh(credits.observed_at, now)
    {
        return None;
    }
    let credit = credits.credits.first()?;
    let expiry = credit.expires_at?;
    if expiry <= now {
        return None;
    }
    let windows: Vec<_> = quota
        .windows
        .iter()
        .filter(|w| {
            w.status == "current"
                && crate::routing::fresh(w.observed_at, now)
                && w.window_minutes.is_some()
                && w.reset_at.is_some_and(|at| at > now)
        })
        .collect();
    // Final-day salvage is independent of the low-quota trigger, but never resets an unused account.
    if expiry - now <= DAY && windows.iter().any(|w| w.used_percent > 0.0) {
        return Some(credit);
    }
    if expiry - now >= 3 * DAY {
        return None;
    }
    // A full reset is not spent just because the renewable short window ran out.
    let week = weekly(quota)?;
    (week.remaining_percent <= 1.0 && windows.iter().any(|w| w.window_minutes == Some(10080)))
        .then_some(credit)
}

pub(super) fn authorized(conn: &Connection, id: i64, generation: i64, now: i64) -> Result<bool> {
    // Require an active operator-enabled account and at least one router user.
    let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.id=?1 AND a.generation=?2 AND a.state='active' AND COALESCE(p.enabled,1)=1 AND EXISTS(SELECT 1 FROM users))",params![id,generation],|r|r.get(0))?;
    if !valid || crate::health::paused(conn, id, now)?.is_some() {
        return Ok(false);
    }
    let quota = crate::quota::summary(conn, id, now)?;
    Ok(
        quota.cooldown_until.is_none()
            || quota.cooldown_source.as_deref() == Some("upstream_reset"),
    )
}

impl Upstream {
    /// One bounded worker for both server and standalone; no TUI dependency.
    pub(crate) async fn automatic_accounts(&self, mut stopped: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut next_refresh = tokio::time::Instant::now();
        loop {
            tokio::select! {biased; _=stopped.changed()=>break, _=tick.tick()=>{}}
            if *stopped.borrow() {
                break;
            }
            let refresh = tokio::time::Instant::now() >= next_refresh;
            let ids = if refresh {
                self.db
                    .call(|conn| {
                        let now = Utc::now().timestamp();
                        let mut ids = Vec::new();
                        for account in oauth::list(conn)? {
                            if account.state == "active"
                                && authorized(conn, account.id, account.generation, now)?
                            {
                                ids.push(account.id);
                            }
                        }
                        Ok(ids)
                    })
                    .await
                    .unwrap_or_default()
            } else {
                self.resets
                    .lock()
                    .await
                    .pending
                    .iter()
                    .filter(|(_, pending)| pending.due <= tokio::time::Instant::now())
                    .map(|(id, _)| *id)
                    .collect()
            };
            // Bounded metadata refresh is independent of dashboard activity.
            let observations = stream::iter(ids)
                .map(|id| async move { (id, self.live_limits(id).await) })
                .buffer_unordered(4)
                .collect::<Vec<_>>();
            let live =
                tokio::select! {biased; _=stopped.changed()=>return, live=observations=>live};
            let mut state = self.resets.lock().await;
            if refresh {
                next_refresh = tokio::time::Instant::now() + Duration::from_secs(30);
                state
                    .pending
                    .retain(|id, _| live.iter().any(|(active, _)| active == id));
            }
            drop(state);
            for (id, current) in live {
                if *stopped.borrow() {
                    return;
                }
                let Ok(current) = current else {
                    self.resets.lock().await.pending.remove(&id);
                    continue;
                };
                if self.automatic_reset_step(current).await.is_err() {
                    tracing::warn!(event = "automatic_reset_unavailable", account = id);
                }
            }
        }
    }
    async fn automatic_reset_step(&self, current: Live) -> Result<()> {
        let id = current.account.info.id;
        let generation = current.account.info.generation;
        let now = Utc::now().timestamp();
        let allowed = self
            .db
            .call(move |conn| authorized(conn, id, generation, now))
            .await?;
        if !allowed {
            self.resets.lock().await.pending.remove(&id);
            return Ok(());
        }
        let Some(attempt) = self.reset_attempt(&current, now).await? else {
            self.resets.lock().await.pending.remove(&id);
            return Ok(());
        };
        let hash = attempt.credit;
        let cycle = attempt.cycle;
        let mut state = self.resets.lock().await;
        let pending = state.pending.entry(id).or_insert_with(|| Pending {
            digest: hash,
            cycle,
            due: tokio::time::Instant::now()
                + Duration::from_millis(rand::thread_rng().gen_range(1..=60_000)),
        });
        if pending.digest != hash || pending.cycle != cycle {
            *pending = Pending {
                digest: hash,
                cycle,
                due: tokio::time::Instant::now()
                    + Duration::from_millis(rand::thread_rng().gen_range(1..=60_000)),
            };
        }
        if tokio::time::Instant::now() < pending.due {
            return Ok(());
        }
        state.pending.remove(&id);
        drop(state);
        // Re-read AFTER the randomized delay, not from the round that scheduled it.
        let fresh = self.live_limits(id).await?;
        let now = Utc::now().timestamp();
        let Some(attempt) = self
            .reset_attempt(&fresh, now)
            .await?
            .filter(|a| a.credit == hash && a.cycle == cycle)
        else {
            return Ok(());
        };
        if fresh.account.info.generation != generation {
            return Ok(());
        }
        let order = fresh.order;
        if !self
            .db
            .call(move |conn| ledger::claim(conn, id, generation, order, now, attempt))
            .await?
        {
            return Ok(());
        }
        let outcome = self
            .consume_automatic(
                &fresh.account,
                &eligible(&fresh, now).expect("verified eligibility").id,
                &cycle,
            )
            .await;
        let status = outcome.unwrap_or("unknown");
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE automatic_reset_attempts SET status=?1 WHERE cycle_digest=?2 AND status='submitted'",
                    params![status, cycle.as_slice()],
                )?;
                Ok(())
            })
            .await?;
        tracing::info!(event = "automatic_reset_finished", account = id, status);
        // A successful POST is not proof of observed quota recovery. Persist
        // recovery/inventory evidence before allowing any subsequent credit.
        if let Ok(current) = self.live_limits(id).await {
            let _ = self.reset_attempt(&current, Utc::now().timestamp()).await?;
        }
        Ok(())
    }
    async fn reset_attempt(&self, current: &Live, now: i64) -> Result<Option<ledger::Attempt>> {
        let Some(observation) = ledger::Observation::live(current, now) else {
            return Ok(None);
        };
        let salvage_until = observation.cohort(now);
        let id = current.account.info.id;
        let order = current.order;
        let gate = self
            .db
            .call(move |conn| ledger::observe(conn, id, order, observation, now))
            .await?;
        if gate.waiting
            || (gate.salvage_until > now
                && !weekly(&current.quota).is_some_and(|w| w.remaining_percent <= 1.0))
        {
            return Ok(None);
        }
        let Some(credit) = eligible(current, now) else {
            return Ok(None);
        };
        let Some(cycle) = ledger::cycle(current, now) else {
            return Ok(None);
        };
        let attempt = ledger::Attempt {
            cycle,
            credit: digest(&credit.id),
            snapshot: current.quota.windows.clone(),
            salvage_until,
        };
        self.db
            .call(move |conn| Ok(ledger::available(conn, &attempt, now)?.then_some(attempt)))
            .await
    }
    async fn consume_automatic(
        &self,
        account: &Account,
        credit: &str,
        hash: &[u8; 32],
    ) -> Result<&'static str> {
        let response = self
            .client
            .post(self.limits_url("rate-limit-reset-credits/consume"))
            .headers(account_headers(account)?)
            .timeout(Duration::from_secs(10))
            .json(&json!({"redeem_request_id":request_id(hash),"credit_id":credit}))
            .send()
            .await
            .map_err(|_| "Automatic reset outcome unknown")?;
        if !response.status().is_success() {
            return Ok(
                if response.status().is_server_error() || response.status().as_u16() == 408 {
                    "unknown"
                } else {
                    "rejected"
                },
            );
        }
        let value = oauth::read_json(response, 65_536).await?;
        Ok(match value["code"].as_str() {
            Some("reset") => "reset",
            Some("already_redeemed") => "already_redeemed",
            Some("nothing_to_reset") => "nothing_to_reset",
            Some("no_credit") => "no_credit",
            _ => "unknown",
        })
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
    use std::sync::{Arc, Mutex};

    struct Mock {
        usage: Mutex<Value>,
        details: Mutex<Value>,
        bodies: Mutex<Vec<Value>>,
        redeemed: Mutex<std::collections::BTreeSet<String>>,
        fail: std::sync::atomic::AtomicBool,
        defer_recovery: std::sync::atomic::AtomicBool,
        code: Mutex<Option<String>>,
        activations: std::sync::atomic::AtomicUsize,
        activation_bodies: Mutex<Vec<Value>>,
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
        m.bodies.lock().unwrap().push(body.clone());
        if m.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
        }
        if let Some(code) = m.code.lock().unwrap().as_ref() {
            return (StatusCode::OK, Json(json!({"code":code,"windows_reset":0})));
        }
        let key = body["redeem_request_id"].as_str().unwrap().to_owned();
        let inserted = m.redeemed.lock().unwrap().insert(key);
        if inserted {
            let mut details = m.details.lock().unwrap();
            let credit = details["credits"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|c| c["id"] == body["credit_id"] && c["status"] == "available")
                .expect("available selected credit");
            credit["status"] = json!("redeemed");
            let count = details["available_count"].as_i64().unwrap() - 1;
            details["available_count"] = json!(count);
            let mut usage = m.usage.lock().unwrap();
            usage["rate_limit_reset_credits"]["available_count"] = json!(count);
            if !m.defer_recovery.load(std::sync::atomic::Ordering::SeqCst) {
                restore(&mut usage);
            }
        }
        (
            StatusCode::OK,
            Json(json!({"code":if inserted{"reset"}else{"already_redeemed"},"windows_reset":2})),
        )
    }
    fn restore(usage: &mut Value) {
        for kind in ["primary_window", "secondary_window"] {
            let window = &mut usage["rate_limit"][kind];
            if let Some(seconds) = window["limit_window_seconds"].as_i64() {
                window["used_percent"] = json!(0);
                window["reset_at"] = json!(Utc::now().timestamp() + seconds);
            }
        }
    }
    async fn activate(AxumState(m): AxumState<Arc<Mock>>, Json(body): Json<Value>) -> String {
        m.activation_bodies.lock().unwrap().push(body);
        m.activations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":1}}}\n\n".into()
    }
    struct Fixture {
        upstream: Upstream,
        mock: Arc<Mock>,
        server: tokio::task::JoinHandle<()>,
        _dir: tempfile::TempDir,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }
    fn expiry(at: i64) -> String {
        DateTime::from_timestamp(at, 0).unwrap().to_rfc3339()
    }
    fn inventory(now: i64) -> Value {
        json!({"available_count":2,"credits":[
            {"id":"synthetic-later","reset_type":"codex_rate_limits","status":"available","expires_at":expiry(now+30*DAY)},
            {"id":"synthetic-earlier","reset_type":"codex_rate_limits","status":"available","expires_at":expiry(now+2*DAY)}
        ]})
    }
    fn usage_value(now: i64) -> Value {
        json!({"rate_limit":{"allowed":true,"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":now+3600},"secondary_window":{"used_percent":99,"limit_window_seconds":604800,"reset_at":now+5*DAY}},"rate_limit_reset_credits":{"available_count":2}})
    }

    #[tokio::test]
    async fn outlook_credit_preview_is_read_only_and_respects_pending_recovery() {
        let f = Fixture::new().await;
        let current = f.upstream.live_limits(1).await.unwrap();
        let now = Utc::now().timestamp();
        // Before the worker has observed inventory, the dashboard cannot assume
        // recovery/cohort fences have been checked.
        assert!(f
            .upstream
            .db
            .call(move |conn| crate::reset::preview(conn, 1, now))
            .await
            .unwrap()
            .is_none());
        let attempt = f
            .upstream
            .reset_attempt(&current, now)
            .await
            .unwrap()
            .unwrap();
        let expiry = current.credits.as_ref().unwrap().credits[0].expires_at;
        let generation = current.account.info.generation;
        let order = current.order;
        f.upstream
            .db
            .call(move |conn| {
                let before = conn.total_changes();
                assert_eq!(crate::reset::preview(conn, 1, now)?, expiry);
                assert_eq!(
                    conn.total_changes(),
                    before,
                    "preview never claims, observes or recovers a credit"
                );
                assert!(crate::reset::preview(conn, 1, now + 60)?.is_none());
                assert!(ledger::claim(conn, 1, generation, order, now, attempt)?);
                assert!(
                    crate::reset::preview(conn, 1, now)?.is_none(),
                    "submitted without recovery is never reserve capacity"
                );
                conn.execute("UPDATE automatic_reset_attempts SET status='unknown'", [])?;
                assert!(crate::reset::preview(conn, 1, now)?.is_none());
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            f.mock.bodies.lock().unwrap().is_empty(),
            "dashboard preview must not POST a reset"
        );
    }
    async fn upstream(url: &str, db: crate::store::Database) -> Upstream {
        db.call(|conn|{
            crate::create_user(conn,"standalone")?;
            oauth::save(conn,&oauth::Vault::new([2;32]),"synthetic-account",&oauth::Credentials{access_token:"synthetic-access".into(),refresh_token:"synthetic-refresh".into()},Utc::now().timestamp()+3600)?;
            conn.execute("INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'gpt-5.6-sol','Synthetic')",[])?;
            conn.execute("UPDATE oauth_accounts SET catalog_updated_at=?1",[Utc::now().timestamp()])?;
            Ok(())
        }).await.unwrap();
        Upstream::new(
            db,
            [2; 32],
            crate::upstream::Config::new(url.into(), url.into(), true).unwrap(),
        )
        .unwrap()
    }
    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = crate::store::Database::open(dir.path().join("state.sqlite"))
                .await
                .unwrap();
            let now = Utc::now().timestamp();
            let mock = Arc::new(Mock {
                usage: Mutex::new(usage_value(now)),
                details: Mutex::new(inventory(now)),
                bodies: Mutex::new(vec![]),
                redeemed: Mutex::new(Default::default()),
                fail: Default::default(),
                defer_recovery: Default::default(),
                code: Default::default(),
                activations: Default::default(),
                activation_bodies: Default::default(),
            });
            let app = Router::new()
                .route("/usage", get(usage))
                .route("/rate-limit-reset-credits", get(details))
                .route("/rate-limit-reset-credits/consume", post(consume))
                .route("/responses", post(activate))
                .with_state(mock.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                upstream: upstream(&url, db).await,
                mock,
                server,
                _dir: dir,
            }
        }
        async fn schedule(&self) {
            if let Ok(current) = self.upstream.live_limits(1).await {
                self.upstream.automatic_reset_step(current).await.unwrap();
            }
        }
        async fn due(upstream: &Upstream) {
            if let Some(p) = upstream.resets.lock().await.pending.get_mut(&1) {
                p.due = tokio::time::Instant::now();
            }
        }
        async fn consume(&self) {
            self.schedule().await;
            Self::due(&self.upstream).await;
            self.schedule().await;
        }
    }
    #[tokio::test]
    async fn exact_thresholds_preserve_strategic_resets_and_only_spend_expiring_ones() {
        let f = Fixture::new().await;
        let mut current = f.upstream.live_limits(1).await.unwrap();
        let now = Utc::now().timestamp();
        assert!(eligible(&current, now).is_some());
        current
            .quota
            .windows
            .iter_mut()
            .find(|w| w.window_minutes == Some(10080))
            .unwrap()
            .remaining_percent = 1.001;
        assert!(eligible(&current, now).is_none());
        current.credits.as_mut().unwrap().credits[0].expires_at = Some(now + DAY);
        assert!(eligible(&current, now).is_some());
        current.quota.windows.iter_mut().for_each(|w| {
            w.used_percent = 0.0;
            w.remaining_percent = 100.0;
        });
        assert!(eligible(&current, now).is_none());
        current
            .quota
            .windows
            .iter_mut()
            .find(|w| w.window_minutes == Some(10080))
            .unwrap()
            .remaining_percent = 1.0;
        current.credits.as_mut().unwrap().credits[0].expires_at = Some(now + 3 * DAY);
        assert!(eligible(&current, now).is_none());
        current.credits.as_mut().unwrap().credits[0].expires_at = Some(now + 3 * DAY - 1);
        assert!(eligible(&current, now).is_some());
        current.credits.as_mut().unwrap().credits[0].expires_at = None;
        assert!(eligible(&current, now).is_none());
        current.credits.as_mut().unwrap().credits[0].expires_at = Some(now);
        assert!(eligible(&current, now).is_none());
        assert!(f.mock.bodies.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn short_only_exhaustion_never_spends_a_two_day_reset_but_final_day_salvage_does() {
        let f = Fixture::new().await;
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(20);
        f.mock.usage.lock().unwrap()["rate_limit"]["primary_window"]["used_percent"] = json!(100);
        f.consume().await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        f.mock.details.lock().unwrap()["credits"][1]["expires_at"] =
            json!(expiry(Utc::now().timestamp() + DAY));
        f.consume().await;
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn delay_is_bounded_and_the_selected_credit_and_live_quota_are_rechecked() {
        let f = Fixture::new().await;
        f.schedule().await;
        let pending = f.upstream.resets.lock().await;
        let wait = pending.pending[&1]
            .due
            .saturating_duration_since(tokio::time::Instant::now());
        assert!(wait <= Duration::from_secs(60));
        drop(pending);
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        Fixture::due(&f.upstream).await;
        // A reset on another server restores capacity before our delayed preflight.
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(0);
        f.schedule().await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(99);
        f.schedule().await;
        Fixture::due(&f.upstream).await;
        f.mock.details.lock().unwrap()["credits"][1]["status"] = json!("redeemed");
        f.schedule().await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn independent_databases_use_the_same_credit_and_upstream_idempotency_key() {
        let f = Fixture::new().await;
        let dir = tempfile::tempdir().unwrap();
        let db = crate::store::Database::open(dir.path().join("independent.sqlite"))
            .await
            .unwrap();
        let second = upstream(&f.upstream.config.base_url, db).await;
        f.schedule().await;
        second
            .automatic_reset_step(second.live_limits(1).await.unwrap())
            .await
            .unwrap();
        Fixture::due(&f.upstream).await;
        Fixture::due(&second).await;
        let (a, b) = tokio::join!(
            f.upstream
                .automatic_reset_step(f.upstream.live_limits(1).await.unwrap()),
            second.automatic_reset_step(second.live_limits(1).await.unwrap())
        );
        a.unwrap();
        b.unwrap();
        let bodies = f.mock.bodies.lock().unwrap();
        assert!((1..=2).contains(&bodies.len()));
        assert!(bodies.iter().all(|body| body == &bodies[0]));
        assert_eq!(bodies[0]["credit_id"], "synthetic-earlier");
        assert_eq!(f.mock.redeemed.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn durable_claim_is_once_per_credit_and_unknown_outcome_blocks_the_next_credit() {
        let f = Fixture::new().await;
        f.mock.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        f.consume().await;
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
        let restarted =
            Upstream::new(f.upstream.db.clone(), [2; 32], f.upstream.config.clone()).unwrap();
        f.mock.details.lock().unwrap()["credits"][1]["status"] = json!("redeemed");
        f.mock.details.lock().unwrap()["credits"][0]["expires_at"] =
            json!(expiry(Utc::now().timestamp() + 2 * DAY));
        restarted
            .automatic_reset_step(restarted.live_limits(1).await.unwrap())
            .await
            .unwrap();
        assert!(restarted.resets.lock().await.pending.is_empty());
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
        // No upstream credit ID is ever persisted as plaintext metadata or audit state.
        let bytes = std::fs::read(f._dir.path().join("state.sqlite")).unwrap();
        assert!(!bytes
            .windows(b"synthetic-earlier".len())
            .any(|w| w == b"synthetic-earlier"));
    }

    #[tokio::test]
    async fn successful_reset_waits_for_observed_recovery_and_cohort_does_not_cascade_after_restart(
    ) {
        let f = Fixture::new().await;
        let now = Utc::now().timestamp();
        for credit in f.mock.details.lock().unwrap()["credits"]
            .as_array_mut()
            .unwrap()
        {
            credit["expires_at"] = json!(expiry(now + DAY - 1));
        }
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(20);
        f.mock
            .defer_recovery
            .store(true, std::sync::atomic::Ordering::SeqCst);
        f.consume().await;
        f.consume().await;
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
        let restarted =
            Upstream::new(f.upstream.db.clone(), [2; 32], f.upstream.config.clone()).unwrap();
        restore(&mut f.mock.usage.lock().unwrap());
        restarted
            .automatic_reset_step(restarted.live_limits(1).await.unwrap())
            .await
            .unwrap();
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(0.1);
        restarted
            .automatic_reset_step(restarted.live_limits(1).await.unwrap())
            .await
            .unwrap();
        assert!(restarted.resets.lock().await.pending.is_empty());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(99);
        restarted
            .automatic_reset_step(restarted.live_limits(1).await.unwrap())
            .await
            .unwrap();
        Fixture::due(&restarted).await;
        restarted
            .automatic_reset_step(restarted.live_limits(1).await.unwrap())
            .await
            .unwrap();
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 2);
        assert_eq!(f.mock.redeemed.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn different_hosts_and_different_credits_share_the_same_unrecovered_quota_cycle_key() {
        let f = Fixture::new().await;
        f.mock.details.lock().unwrap()["credits"][0]["expires_at"] =
            json!(expiry(Utc::now().timestamp() + 2 * DAY + 1));
        f.mock
            .defer_recovery
            .store(true, std::sync::atomic::Ordering::SeqCst);
        f.consume().await;
        let dir = tempfile::tempdir().unwrap();
        let second = upstream(
            &f.upstream.config.base_url,
            crate::store::Database::open(dir.path().join("second.sqlite"))
                .await
                .unwrap(),
        )
        .await;
        second
            .automatic_reset_step(second.live_limits(1).await.unwrap())
            .await
            .unwrap();
        Fixture::due(&second).await;
        second
            .automatic_reset_step(second.live_limits(1).await.unwrap())
            .await
            .unwrap();
        let bodies = f.mock.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_ne!(bodies[0]["credit_id"], bodies[1]["credit_id"]);
        assert_eq!(
            bodies[0]["redeem_request_id"],
            bodies[1]["redeem_request_id"]
        );
        assert_eq!(f.mock.redeemed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn confirmed_noop_can_retry_after_new_consumption_without_changing_the_cycle_key() {
        let f = Fixture::new().await;
        *f.mock.code.lock().unwrap() = Some("nothing_to_reset".into());
        f.consume().await;
        f.consume().await;
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
        f.upstream
            .db
            .call(|conn| {
                conn.execute(
                    "UPDATE automatic_reset_attempts SET attempted_at=attempted_at-31",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        f.consume().await;
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(100);
        *f.mock.code.lock().unwrap() = None;
        f.consume().await;
        let bodies = f.mock.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(f.mock.redeemed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn observing_another_hosts_inventory_change_blocks_salvage_until_new_depletion() {
        let f = Fixture::new().await;
        let now = Utc::now().timestamp();
        for credit in f.mock.details.lock().unwrap()["credits"]
            .as_array_mut()
            .unwrap()
        {
            credit["expires_at"] = json!(expiry(now + DAY - 1));
        }
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(20);
        f.schedule().await;
        f.mock.details.lock().unwrap()["credits"][1]["status"] = json!("redeemed");
        f.mock.details.lock().unwrap()["available_count"] = json!(1);
        f.schedule().await;
        assert!(f.upstream.resets.lock().await.pending.is_empty());
        restore(&mut f.mock.usage.lock().unwrap());
        f.schedule().await;
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(0.1);
        f.consume().await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(99);
        f.consume().await;
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn deactivation_backoff_unsupported_and_unknown_data_fail_closed() {
        let f = Fixture::new().await;
        f.upstream
            .db
            .call(|conn| {
                conn.execute(
                    "INSERT INTO account_preferences(user_id,account_id,enabled) VALUES(1,1,0)",
                    [],
                )?;
                assert!(authorized(conn, 1, 0, Utc::now().timestamp())?);
                crate::account_preferences::set_policy(conn, 1, Some(false), None, None)?;
                Ok(())
            })
            .await
            .unwrap();
        f.consume().await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        f.upstream
            .db
            .call(|conn| {
                crate::account_preferences::set_policy(conn, 1, Some(true), None, None)?;
                let order = crate::health::next(conn)?;
                crate::health::failure(
                    conn,
                    (1, 0),
                    crate::health::Scope::Responses,
                    order,
                    "transport",
                    None,
                    Utc::now().timestamp(),
                )
            })
            .await
            .unwrap();
        f.consume().await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        f.upstream
            .db
            .call(|conn| {
                conn.execute("DELETE FROM oauth_health", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let mut live = f.upstream.live_limits(1).await.unwrap();
        live.credits.as_mut().unwrap().details_available = false;
        assert!(eligible(&live, Utc::now().timestamp()).is_none());
        live.credits.as_mut().unwrap().details_available = true;
        live.quota
            .windows
            .iter_mut()
            .for_each(|w| w.status = "stale".into());
        assert!(eligible(&live, Utc::now().timestamp()).is_none());
    }
    #[tokio::test]
    async fn live_allowed_recovers_only_subscription_cooldown_and_diagnostics_never_redeem() {
        let f = Fixture::new().await;
        f.upstream
            .db
            .call(|conn| {
                conn.execute(
                    "UPDATE oauth_accounts SET cooldown_until=?1,cooldown_source='upstream_reset'",
                    [Utc::now().timestamp() + DAY],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        f.mock.usage.lock().unwrap()["rate_limit"]["allowed"] = json!(false);
        assert!(f
            .upstream
            .live_limits(1)
            .await
            .unwrap()
            .quota
            .cooldown_until
            .is_some());
        f.mock.usage.lock().unwrap()["rate_limit"]["allowed"] = json!(true);
        assert!(f
            .upstream
            .live_limits(1)
            .await
            .unwrap()
            .quota
            .cooldown_until
            .is_none());
        let mut report = f
            .upstream
            .db
            .call(|conn| {
                crate::doctor::snapshot(
                    conn,
                    true,
                    crate::server::local_limits(1),
                    Utc::now().timestamp(),
                )
            })
            .await
            .unwrap();
        f.upstream.account_metadata(&mut report, true, 1).await;
        assert!(f.mock.bodies.lock().unwrap().is_empty());
        assert_eq!(
            f.mock.activations.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
    #[tokio::test]
    async fn unknown_final_day_salvage_requires_real_recovery_not_unchanged_above_one_percent() {
        let f = Fixture::new().await;
        let now = Utc::now().timestamp();
        f.mock.details.lock().unwrap()["credits"][1]["expires_at"] = json!(expiry(now + DAY));
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(20);
        f.mock.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        f.consume().await;
        f.mock.details.lock().unwrap()["credits"][1]["status"] = json!("redeemed");
        f.mock.details.lock().unwrap()["credits"][0]["expires_at"] = json!(expiry(now + DAY));
        f.schedule().await;
        assert!(f.upstream.resets.lock().await.pending.is_empty());
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
        f.upstream
            .db
            .call(|conn| {
                assert_eq!(
                    conn.query_row("SELECT status FROM automatic_reset_attempts", [], |r| {
                        r.get::<_, String>(0)
                    })?,
                    "unknown"
                );
                Ok(())
            })
            .await
            .unwrap();
        // Every consumed window must improve before another credit can be scheduled.
        f.mock.usage.lock().unwrap()["rate_limit"]["primary_window"]["used_percent"] = json!(10);
        f.schedule().await;
        assert!(f.upstream.resets.lock().await.pending.is_empty());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(10);
        f.schedule().await;
        assert!(f.upstream.resets.lock().await.pending.is_empty());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["reset_at"] =
            json!(now + 7 * DAY);
        f.schedule().await;
        // Recovery clears uncertainty, not the final-day cohort guard.
        assert!(f.upstream.resets.lock().await.pending.is_empty());
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"]["used_percent"] = json!(99);
        f.schedule().await;
        assert_eq!(f.upstream.resets.lock().await.pending.len(), 1);
    }
    #[tokio::test]
    async fn durable_claim_serializes_shared_database_and_rejects_superseded_observations() {
        let f = Fixture::new().await;
        let live = f.upstream.live_limits(1).await.unwrap();
        let order = live.order;
        let generation = live.account.info.generation;
        let snapshot = live.quota.windows.clone();
        let other_snapshot = snapshot.clone();
        let hash = digest("synthetic-earlier");
        let now = Utc::now().timestamp();
        let cycle = ledger::cycle(&live, now).unwrap();
        let (a, b) = tokio::join!(
            f.upstream.db.call(move |conn| ledger::claim(
                conn,
                1,
                generation,
                order,
                now,
                ledger::Attempt {
                    cycle,
                    credit: hash,
                    snapshot,
                    salvage_until: 0
                }
            )),
            f.upstream.db.call(move |conn| ledger::claim(
                conn,
                1,
                generation,
                order,
                now,
                ledger::Attempt {
                    cycle,
                    credit: hash,
                    snapshot: other_snapshot,
                    salvage_until: 0
                }
            ))
        );
        assert_eq!(usize::from(a.unwrap()) + usize::from(b.unwrap()), 1);
        let later = f.upstream.live_limits(1).await.unwrap();
        let order = later.order;
        let snapshot = later.quota.windows.clone();
        f.upstream
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE automatic_reset_attempts SET status='recovered',recovered_at=1",
                    [],
                )?;
                conn.execute(
                    "UPDATE oauth_quota_windows SET request_order=?1",
                    [order + 1],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(!f
            .upstream
            .db
            .call(move |conn| ledger::claim(
                conn,
                1,
                generation,
                order,
                now,
                ledger::Attempt {
                    cycle: digest("new-cycle"),
                    credit: digest("synthetic-later"),
                    snapshot,
                    salvage_until: 0
                }
            ))
            .await
            .unwrap());
    }
    #[tokio::test]
    async fn runtime_worker_refreshes_without_a_dashboard_and_stops_without_inference() {
        let f = Fixture::new().await;
        f.mock.details.lock().unwrap()["credits"][1]["expires_at"] =
            json!(expiry(Utc::now().timestamp() + 30 * DAY));
        let (stop, stopped) = watch::channel(false);
        let upstream = Arc::new(
            Upstream::new(f.upstream.db.clone(), [2; 32], f.upstream.config.clone()).unwrap(),
        );
        let worker_upstream = upstream.clone();
        let worker = tokio::spawn(async move {
            worker_upstream.automatic_accounts(stopped).await;
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if upstream
                    .db
                    .call(|conn| {
                        Ok(conn.query_row(
                            "SELECT COUNT(*) FROM account_routing_metadata",
                            [],
                            |r| r.get::<_, i64>(0),
                        )?)
                    })
                    .await
                    .unwrap()
                    > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            f.mock.activations.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(f.mock.bodies.lock().unwrap().is_empty());
    }
    #[test]
    fn unsupported_expired_or_unknown_credit_details_are_never_invented() {
        let now = 1_800_000_000;
        let usage = usage_value(now);
        let partial = parse_credits(&usage, None, now).unwrap();
        assert!(!partial.details_available);
        assert!(partial.credits.is_empty());
        let mut details = inventory(now);
        details["credits"][0]["reset_type"] = json!("unsupported");
        details["credits"][1]["expires_at"] = json!(expiry(now));
        assert!(parse_credits(&usage, Some(&details), now)
            .unwrap()
            .credits
            .is_empty());
    }
    #[tokio::test]
    async fn short_window_presence_uses_explicit_reported_structure_not_missing_percentages() {
        let f = Fixture::new().await;
        f.mock.usage.lock().unwrap()["rate_limit"]["primary_window"]["used_percent"] = Value::Null;
        f.upstream.live_limits(1).await.unwrap();
        assert_eq!(
            f.upstream
                .db
                .call(|conn| Ok(crate::routing::metadata(conn, 1, Utc::now().timestamp())?.0))
                .await
                .unwrap(),
            Some(true)
        );
        f.mock.usage.lock().unwrap()["rate_limit"]["primary_window"] = Value::Null;
        f.upstream.live_limits(1).await.unwrap();
        assert_eq!(
            f.upstream
                .db
                .call(|conn| Ok(crate::routing::metadata(conn, 1, Utc::now().timestamp())?.0))
                .await
                .unwrap(),
            Some(false)
        );
        f.mock.usage.lock().unwrap()["rate_limit"]
            .as_object_mut()
            .unwrap()
            .remove("primary_window");
        f.upstream.live_limits(1).await.unwrap();
        assert_eq!(
            f.upstream
                .db
                .call(|conn| Ok(crate::routing::metadata(conn, 1, Utc::now().timestamp())?.0))
                .await
                .unwrap(),
            None
        );
    }
    #[tokio::test]
    async fn weekly_activation_remains_pinned_minimal_durable_and_separate_from_resets() {
        let f = Fixture::new().await;
        let now = Utc::now().timestamp();
        f.mock.usage.lock().unwrap()["rate_limit"]["secondary_window"] =
            json!({"used_percent":0,"limit_window_seconds":604800,"reset_at":now+7*DAY});
        let q = f.upstream.live_limits(1).await.unwrap().quota;
        let (a, b) = tokio::join!(
            f.upstream.activate_weekly_window(1, 1, &q),
            f.upstream.activate_weekly_window(1, 1, &q)
        );
        assert_eq!(usize::from(a.is_some()) + usize::from(b.is_some()), 1);
        assert_eq!(
            f.mock.activations.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let restarted =
            Upstream::new(f.upstream.db.clone(), [2; 32], f.upstream.config.clone()).unwrap();
        assert!(restarted.activate_weekly_window(1, 1, &q).await.is_none());
        let body = f.mock.activation_bodies.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["store"], false);
        assert!(body.get("tools").is_none());
        assert!(f.mock.bodies.lock().unwrap().is_empty());
    }
}
