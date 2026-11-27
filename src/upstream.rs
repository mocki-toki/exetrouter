mod activation;
pub(crate) mod native;
mod platform;
use crate::{
    oauth::{self, Account, Vault},
    store::Database,
    Result,
};
use chrono::Utc;
use codex_websocket::{
    connect_async_with_config,
    tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig},
    MaybeTlsStream, WebSocketStream,
};
use futures_util::StreamExt;
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client, Url,
};
use rusqlite::params;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    ops::{Deref, DerefMut},
    time::Duration,
};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

// Catalog discovery revision; this does not claim full native-client compatibility.
pub const CODEX_CLIENT_VERSION: &str = "0.169.0";
pub const BACKEND: &str = "https://chatgpt.com/backend-api/codex";

fn io_error_kind(error: &std::io::Error) -> Option<&'static str> {
    use std::io::ErrorKind::*;
    Some(match error.kind() {
        ConnectionRefused => "connection_refused",
        ConnectionReset => "connection_reset",
        ConnectionAborted => "connection_aborted",
        BrokenPipe => "broken_pipe",
        TimedOut => "timed_out",
        UnexpectedEof => "unexpected_eof",
        NetworkUnreachable => "network_unreachable",
        HostUnreachable => "host_unreachable",
        _ => return None,
    })
}

pub(crate) fn http_error_kind(error: &reqwest::Error) -> &'static str {
    use std::error::Error;
    if error.is_timeout() {
        return "timed_out";
    }
    let connecting = error.is_connect();
    let mut source = error.source();
    while let Some(cause) = source {
        if cause.is::<rustls::Error>() {
            return "tls";
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if let Some(kind) = io_error_kind(io) {
                return kind;
            }
            // io::Error::source skips the wrapped error itself (including rustls).
            source = io.get_ref().map(|e| e as &dyn Error);
            continue;
        }
        if let Some(http) = cause.downcast_ref::<hyper::Error>() {
            if http.is_incomplete_message() {
                return "incomplete_http_message";
            }
            if http.is_parse() {
                return "http_protocol";
            }
        }
        // hyper-util's private ConnectError exposes only this fixed DNS marker.
        if connecting && cause.source().is_some() && cause.to_string() == "dns error" {
            return "dns";
        }
        source = cause.source();
    }
    if connecting {
        "connect"
    } else if error.is_builder() {
        "request_build"
    } else if error.is_body() || error.is_decode() {
        "http_body"
    } else {
        "http_request"
    }
}

#[derive(Debug)]
pub(crate) struct HttpPostFailure {
    pub(crate) kind: &'static str,
    pub(crate) elapsed_ms: u64,
}

impl std::fmt::Display for HttpPostFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upstream connection failed")
    }
}
impl std::error::Error for HttpPostFailure {}

async fn send_inference_post(
    request: reqwest::RequestBuilder,
) -> std::result::Result<reqwest::Response, HttpPostFailure> {
    let start = std::time::Instant::now();
    request.send().await.map_err(|error| HttpPostFailure {
        elapsed_ms: start.elapsed().as_millis() as u64,
        kind: http_error_kind(&error),
    })
}

pub(crate) fn websocket_error_kind(error: &codex_tungstenite::Error) -> &'static str {
    use codex_websocket::tungstenite::{error::ProtocolError, Error};
    match error {
        Error::ConnectionClosed => "connection_closed",
        Error::AlreadyClosed => "already_closed",
        Error::Io(error)
            if error
                .get_ref()
                .is_some_and(|cause| cause.is::<rustls::Error>()) =>
        {
            "tls"
        }
        Error::Io(error) => io_error_kind(error).unwrap_or("io_other"),
        Error::Tls(_) => "tls",
        Error::Capacity(_) => "capacity",
        Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
            "reset_without_close_handshake"
        }
        Error::Protocol(_) => "protocol",
        Error::WriteBufferFull(_) => "write_buffer_full",
        Error::Http(_) => "http_status",
        Error::HttpFormat(_) => "http_format",
        Error::Url(_) => "url",
        Error::Utf8(_) => "utf8",
        _ => "other",
    }
}

pub struct UpstreamSocket {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pub(crate) compressed: bool,
}

impl Deref for UpstreamSocket {
    type Target = WebSocketStream<MaybeTlsStream<TcpStream>>;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl DerefMut for UpstreamSocket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.socket
    }
}

pub(crate) enum ResponsesConnection {
    WebSocket(Box<UpstreamSocket>),
    Http,
}

impl ResponsesConnection {
    pub(crate) fn socket_mut(&mut self) -> Option<&mut UpstreamSocket> {
        match self {
            Self::WebSocket(socket) => Some(socket),
            Self::Http => None,
        }
    }

    pub(crate) fn is_http(&self) -> bool {
        matches!(self, Self::Http)
    }
}

/// A transport preference, never an input admission limit. Compression and
/// fragmentation do not remove upstream whole-message limits. Count without
/// allocating a second serialized request, stopping once HTTP is preferable.
pub(crate) fn prefer_http(body: &Value) -> bool {
    if body["generate"] == false {
        // Non-generating warmup is a native WS operation, not an HTTP inference.
        return false;
    }
    struct Size(usize);
    impl std::io::Write for Size {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 >= 16 * 1024 * 1024 {
                return Err(std::io::Error::other("prefer HTTP transport"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Size(0), body).is_err()
}

#[derive(Default)]
pub struct InferenceHeaders<'a> {
    pub turn_state: Option<&'a HeaderValue>,
    pub responses_lite: bool,
}

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub issuer: String,
    platform_base: String,
}

impl Config {
    #[cfg(test)]
    pub(crate) fn mock_platform(mut self, base: &str) -> Result<Self> {
        crate::platform::validate_mock_endpoint(base)?;
        self.platform_base = base.trim_end_matches('/').into();
        Ok(self)
    }
    pub fn new(base_url: String, issuer: String, allow_mock: bool) -> Result<Self> {
        for (value, production) in [(&base_url, BACKEND), (&issuer, oauth::ISSUER)] {
            let url = Url::parse(value)?;
            let loopback = url
                .host_str()
                .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            if !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || !(value.trim_end_matches('/') == production
                    || (allow_mock && loopback && url.scheme() == "http"))
            {
                return Err("upstream must use the Codex endpoints; mock overrides require a literal loopback HTTP address and --allow-mock-upstream".into());
            }
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').into(),
            issuer: issuer.trim_end_matches('/').into(),
            platform_base: crate::platform::BASE.into(),
        })
    }
}

pub(crate) const INFERENCE_IDLE_TIMEOUT: Duration = Duration::from_secs(900);

pub fn http_client() -> Result<Client> {
    client_with_read_timeout(Duration::from_secs(300))
}

fn client_with_read_timeout(read_timeout: Duration) -> Result<Client> {
    let builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(read_timeout)
        .user_agent(concat!("exetrouter/", env!("CARGO_PKG_VERSION")));
    // Reqwest otherwise sets a 30-second TCP_USER_TIMEOUT on these platforms.
    // Unacknowledged data/keepalive probes must not cut a recoverable network
    // stall short of the operation's read budget (900s inference, 300s metadata).
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    let builder = builder.tcp_user_timeout(read_timeout);
    Ok(builder.build()?)
}

pub use crate::catalog::Model;

#[derive(Debug)]
pub enum SelectError {
    ModelUnavailable,
    UpstreamUnavailable,
    Cooldown(i64),
    Backoff(i64),
}

pub enum SocketError {
    Unavailable,
    Storage,
    Cooldown(i64),
    Backoff(i64),
    Authentication,
}

pub struct Selection {
    account: Account,
    _reservation: crate::pool::Reservation,
}

impl Deref for Selection {
    type Target = Account;
    fn deref(&self) -> &Account {
        &self.account
    }
}

pub struct Upstream {
    pub(crate) db: Database,
    pub(crate) vault: Vault,
    pub(crate) config: Config,
    pub(crate) client: Client,
    inference_client: Client,
    catalog_lock: Mutex<()>,
    activation_gate: Mutex<()>,
    pub(crate) resets: Mutex<crate::reset::State>,
    pool: crate::pool::Pool,
}

impl Upstream {
    pub fn new(db: Database, key: [u8; 32], config: Config) -> Result<Self> {
        Ok(Self {
            db,
            vault: Vault::new(key),
            config,
            client: http_client()?,
            inference_client: client_with_read_timeout(INFERENCE_IDLE_TIMEOUT)?,
            catalog_lock: Mutex::new(()),
            activation_gate: Mutex::new(()),
            resets: Mutex::new(crate::reset::State::default()),
            pool: crate::pool::Pool::default(),
        })
    }

    pub async fn account(&self, id: i64) -> Result<Account> {
        if id < 0 {
            let vault = self.vault.clone();
            return self
                .db
                .call(move |conn| {
                    crate::platform::routes::account(conn, &vault, id, Utc::now().timestamp())
                })
                .await;
        }
        oauth::access(&self.db, &self.vault, &self.client, &self.config.issuer, id).await
    }

    pub async fn has_active_account(&self) -> Result<bool> {
        self.db
            .call(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE state='active')",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    pub async fn models(&self) -> Result<Vec<Model>> {
        let _guard = self.catalog_lock.lock().await;
        let accounts = self
            .db
            .call(|conn| {
                if !crate::platform::Policy::read(conn)?.codex() {
                    return Ok(Vec::new());
                }
                Ok(oauth::list(conn)?
                    .into_iter()
                    .filter(|info| info.state == "active")
                    .map(|info| info.id)
                    .collect::<Vec<_>>())
            })
            .await?;
        let has_codex_accounts = !accounts.is_empty();
        let mut results = futures_util::stream::iter(accounts)
            .map(|id| async move { (id, self.account_models(id).await) })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(id, _)| *id);
        let mut available: BTreeMap<String, Model> = BTreeMap::new();
        let mut successful = 0;
        for (_, result) in results {
            match result {
                Ok(models) => {
                    successful += 1;
                    for model in models
                        .into_iter()
                        .filter(|m| !crate::catalog::embedding_model(&m.id))
                    {
                        match available.entry(model.id.clone()) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(model);
                            }
                            std::collections::btree_map::Entry::Occupied(mut entry) => {
                                entry.get_mut().merge(model);
                            }
                        }
                    }
                }
                Err(_) => tracing::warn!(event = "account_catalog_unavailable"),
            }
        }
        let platform =
            crate::platform::route_catalog(&self.db, &self.vault, &self.config.platform_base)
                .await?;
        let policy = self
            .db
            .call(|conn| crate::platform::Policy::read(conn))
            .await?;
        if policy.codex() && has_codex_accounts && successful == 0 && platform.is_empty() {
            return Err("all active account catalogs unavailable".into());
        }
        if !policy.codex() {
            available.clear();
        }
        let mut direct_platform = Vec::new();
        if policy.platform() {
            for model in platform {
                if let Some(native) = available.get_mut(&model.id) {
                    // The route catalog already applied the operator policy. Keep
                    // the native descriptor, including its limits and priority.
                    native.platform_fallback = true;
                } else {
                    direct_platform.push(model);
                }
            }
        }
        let mut models = available
            .into_values()
            .map(|mut model| {
                model.present();
                model
            })
            .collect::<Vec<_>>();
        models.extend(direct_platform);
        Ok(models)
    }

    async fn account_models(&self, id: i64) -> Result<Vec<Model>> {
        if id < 0 {
            let fresh = self
                .db
                .call(move |conn| crate::platform::routes::models(conn, id, Utc::now().timestamp()))
                .await;
            if let Ok(models) = fresh {
                return Ok(models);
            }
            let _guard = self.catalog_lock.lock().await;
            crate::platform::route_catalog(&self.db, &self.vault, &self.config.platform_base)
                .await?;
            return self
                .db
                .call(move |conn| crate::platform::routes::models(conn, id, Utc::now().timestamp()))
                .await;
        }
        let account = self.account(id).await?;
        let id = account.info.id;
        let now = Utc::now().timestamp();
        if crate::catalog::fresh(account.info.catalog_updated_at, now) {
            return self.db.call(move |conn| {
                let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')",params![id,account.info.generation],|row|row.get(0))?;
                if !valid { return Err("OAuth account changed during catalog read".into()); }
                let mut stmt=conn.prepare("SELECT model,display_name,metadata FROM oauth_models WHERE account_id=?1 ORDER BY model")?;
                let rows=stmt.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
                let rows=rows.into_iter().map(|(id,display_name,metadata)|Ok(Model { id,display_name,object:"model".into(),owned_by:"openai".into(),source:"codex".into(),platform_fallback:false,capabilities:Vec::new(),metadata:metadata.map(|value|serde_json::from_str(&value)).transpose()? })).collect::<Result<Vec<_>>>()?;
                Ok(rows)
            }).await;
        }
        let order = self.db.call(|conn| crate::health::next(conn)).await?;
        let fetched = self
            .client
            .get(format!("{}/models", self.config.base_url))
            .query(&[("client_version", CODEX_CLIENT_VERSION)])
            .headers(account_headers(&account)?)
            .timeout(Duration::from_secs(20))
            .send()
            .await;
        let response = match fetched {
            Ok(response) => response,
            Err(_) => {
                self.failure(
                    &account,
                    crate::health::Scope::Catalog,
                    order,
                    "catalog",
                    None,
                )
                .await?;
                return Err("model discovery unavailable".into());
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            if status == 401 {
                self.unauthorized(&account, crate::health::Scope::Catalog, order)
                    .await?;
            } else {
                self.failure(
                    &account,
                    crate::health::Scope::Catalog,
                    order,
                    "catalog",
                    retry_at(response.headers()),
                )
                .await?;
            }
            return Err("model discovery rejected".into());
        }
        let result: Result<Vec<Model>> = async {
            let value = oauth::read_json(response, 1_048_576).await?;
            let rows = value
                .get("models")
                .and_then(Value::as_array)
                .ok_or("invalid model catalog")?;
            let mut models = Vec::new();
            for row in rows {
                if row.get("visibility").and_then(Value::as_str) != Some("list") {
                    continue;
                }
                models.push(Model::parse(row)?);
            }
            Ok(models)
        }
        .await;
        let models = match result {
            Ok(models) => models,
            Err(_) => {
                self.failure(
                    &account,
                    crate::health::Scope::Catalog,
                    order,
                    "catalog",
                    None,
                )
                .await?;
                return Err("invalid model discovery reply".into());
            }
        };
        self.success(&account, crate::health::Scope::Catalog, order)
            .await?;
        let cache = models.clone();
        let generation = account.info.generation;
        self.db
            .call(move |conn| {
                store_model_catalog(conn, id, generation, cache, Utc::now().timestamp())
            })
            .await?;
        Ok(models)
    }

    pub async fn account_enabled(&self, id: i64) -> crate::Result<bool> {
        self.db
            .call(move |conn| {
                if !crate::platform::Policy::read(conn)?.allows(id) {
                    return Ok(false);
                }
                if id < 0 {
                    return crate::platform::routes::enabled(conn, id);
                }
                // Explicit deactivation permits transfer; auth and backoff do not.
                let disabled = conn.query_row(
                    "SELECT state='disabled' FROM oauth_accounts WHERE id=?1",
                    [id],
                    |row| row.get::<_, bool>(0),
                )?;
                Ok(!disabled && crate::account_preferences::effective(conn, id)?.enabled)
            })
            .await
    }

    pub async fn select(&self, model: &str) -> std::result::Result<Selection, SelectError> {
        self.select_cached(model, None).await
    }

    pub async fn select_cached(
        &self,
        model: &str,
        affinity: Option<&str>,
    ) -> std::result::Result<Selection, SelectError> {
        self.select_excluding(model, affinity, &[]).await
    }

    pub(crate) async fn select_codex_excluding(
        &self,
        model: &str,
        affinity: Option<&str>,
        excluded: &[i64],
    ) -> std::result::Result<Selection, SelectError> {
        if !self.routing_policy().await?.codex() {
            return Err(SelectError::ModelUnavailable);
        }
        let cached_model = model.to_owned();
        let cached_excluded = excluded.to_vec();
        let cached=self.db.call(move |conn| {
            let now=Utc::now().timestamp();
            let mut stmt=conn.prepare("SELECT a.id FROM oauth_accounts a JOIN oauth_models m ON m.account_id=a.id WHERE a.state='active' AND m.model=?1 AND a.expires_at>?2+60 AND a.catalog_updated_at<=?2 AND a.catalog_updated_at>?2-?3 AND (a.cooldown_until IS NULL OR a.cooldown_until<=?2) AND NOT EXISTS(SELECT 1 FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.retry_at>?2)")?;
            for id in stmt.query_map(params![cached_model,now,crate::catalog::CACHE_TTL_SECONDS],|row|row.get::<_,i64>(0))? {
                let id=id?;
                if !cached_excluded.contains(&id) && crate::account_preferences::effective(conn,id)?.enabled {
                    return Ok(true);
                }
            }
            Ok(false)
        }).await.map_err(|_|SelectError::UpstreamUnavailable)?;
        if !cached {
            let models = match self.models().await {
                Ok(models) => models,
                Err(_) => return Err(self.failure_wait(model).await),
            };
            if !models.iter().any(|m| m.id == model) {
                let requested = model.to_owned();
                let incomplete = self.db.call(move |conn| {
                let now=Utc::now().timestamp();
                Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts a WHERE state='active' AND (catalog_updated_at IS NULL OR catalog_updated_at>?1 OR catalog_updated_at<=?1-?3 OR EXISTS(SELECT 1 FROM oauth_models m WHERE m.account_id=a.id AND m.model=?2)))",params![now,requested,crate::catalog::CACHE_TTL_SECONDS],|row|row.get::<_,bool>(0))?)
            }).await.map_err(|_|SelectError::UpstreamUnavailable)?;
                if incomplete {
                    return Err(self.failure_wait(model).await);
                }
                return Err(SelectError::ModelUnavailable);
            }
        }
        let selected_model = model.to_owned();
        let model = selected_model.clone();
        let (mut candidates, earliest, exhausted, thresholds) = self.db.call(move |conn| {
            let now = Utc::now().timestamp();
            let mut stmt=conn.prepare("SELECT a.id,a.cooldown_until,(SELECT MAX(h.retry_at) FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation),a.expires_at FROM oauth_accounts a JOIN oauth_models m ON m.account_id=a.id WHERE a.state='active' AND m.model=?1 AND a.catalog_updated_at<=?2 AND a.catalog_updated_at>?2-?3 ORDER BY a.id")?;
            let rows=stmt.query_map(params![model,now,crate::catalog::CACHE_TTL_SECONDS],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,Option<i64>>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,i64>(3)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let mut candidates=Vec::new();
            let mut exhausted=Vec::new();
            let mut thresholds=Vec::new();
            let mut earliest=None;
            for (id,until,health,expires) in rows {
                let preference=crate::account_preferences::effective(conn,id)?;
                if !preference.enabled { continue; }
                let cooldown=until.filter(|until|*until>now);
                let health=health.filter(|until|*until>now);
                let wait=match (cooldown,health) { (Some(c),Some(h))=>Some((c.max(h),c>=h)),(Some(c),None)=>Some((c,true)),(None,Some(h))=>Some((h,false)),_=>None };
                if let Some(wait)=wait { earliest=Some(earliest.map_or(wait,|old:(i64,bool)|if wait.0<old.0 {wait} else {old}));continue; }
                if expires<=now+60 { continue; }
                let summary=crate::quota::summary(conn,id,now)?;
                let routing=crate::routing::decision(conn,id,now)?;
                if routing.reached(&summary) { thresholds.push(id); }
                if summary.windows.iter().any(|window| window.status=="current" && window.used_percent>=100.0 && window.reset_at.is_some_and(|at|at>now)) { exhausted.push(id); }
                let quota=if summary.status=="observed" { summary.windows.iter().map(|window|window.used_percent).reduce(f64::max) } else { None };
                candidates.push(crate::pool::Candidate { id,quota,routing });
            }
            Ok((candidates,earliest,exhausted,thresholds))
        }).await.map_err(|_|SelectError::UpstreamUnavailable)?;
        candidates.retain(|candidate| !excluded.contains(&candidate.id));
        if !excluded.is_empty()
            || candidates
                .iter()
                .any(|candidate| !exhausted.contains(&candidate.id))
        {
            candidates.retain(|candidate| !exhausted.contains(&candidate.id));
        }
        if candidates.is_empty() {
            return Err(wait_error(earliest));
        }
        let mut retry = earliest;
        while !candidates.is_empty() {
            // Prefer above-threshold capacity, but retain the other tier if
            // credential/catalog validation rejects every preferred candidate.
            let preferred = candidates
                .iter()
                .filter(|c| !thresholds.contains(&c.id))
                .cloned()
                .collect::<Vec<_>>();
            let Some(reservation) = self.pool.reserve_affinity(
                if preferred.is_empty() {
                    &candidates
                } else {
                    &preferred
                },
                affinity,
            ) else {
                break;
            };
            let id = reservation.id;
            candidates.retain(|candidate| candidate.id != id);
            // Only credential/catalog checks may move to another candidate.
            // No inference or upstream WebSocket has been sent at this point.
            if let Ok(account) = self.account(id).await {
                match self.validate(&account, &selected_model).await {
                    Ok(()) => {
                        return Ok(Selection {
                            account,
                            _reservation: reservation,
                        })
                    }
                    Err(SelectError::Cooldown(until)) => {
                        retry = Some(retry.map_or((until, true), |old| {
                            if until < old.0 {
                                (until, true)
                            } else {
                                old
                            }
                        }))
                    }
                    Err(SelectError::Backoff(until)) => {
                        retry = Some(retry.map_or((until, false), |old| {
                            if until < old.0 {
                                (until, false)
                            } else {
                                old
                            }
                        }))
                    }
                    Err(_) => {}
                }
            }
        }
        Err(wait_error(retry))
    }

    /// Percentages alone do not block an account: credits may still permit it.
    pub(crate) async fn quota_exhausted(&self, id: i64) -> std::result::Result<bool, SelectError> {
        if id < 0 {
            let account = self
                .account(id)
                .await
                .map_err(|_| SelectError::UpstreamUnavailable)?;
            return Ok(account.info.cooldown_until.is_some());
        }
        self.db
            .call(move |conn| {
                let now = Utc::now().timestamp();
                let summary = crate::quota::summary(conn, id, now)?;
                Ok(summary.cooldown_until.is_some()
                    || summary.windows.iter().any(|window| {
                        window.status == "current"
                            && window.used_percent >= 100.0
                            && window.reset_at.is_some_and(|at| at > now)
                    }))
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)
    }

    pub(crate) async fn threshold_reached(
        &self,
        id: i64,
    ) -> std::result::Result<bool, SelectError> {
        if id < 0 {
            return Ok(false);
        }
        self.db
            .call(move |conn| {
                if !crate::account_preferences::effective(conn, id)?.enabled {
                    return Ok(false);
                }
                let now = Utc::now().timestamp();
                Ok(crate::routing::decision(conn, id, now)?
                    .reached(&crate::quota::summary(conn, id, now)?))
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)
    }

    pub(crate) async fn select_continuation(
        &self,
        model: &str,
        affinity: Option<&str>,
        owner: Option<i64>,
    ) -> std::result::Result<Selection, SelectError> {
        let Some(id) = owner else {
            return self.select_cached(model, affinity).await;
        };
        if id < 0 {
            if let Some(parent) = self.recovered_codex(model, id).await {
                return Ok(parent);
            }
            // Cross-resource replay is decided by transport after validating the input.
            if !self
                .account_enabled(id)
                .await
                .map_err(|_| SelectError::UpstreamUnavailable)?
            {
                return self.select_excluding(model, affinity, &[id]).await;
            }
            let pinned = self.select_pinned(model, id).await;
            if matches!(&pinned, Err(SelectError::Cooldown(_))) {
                if let Ok(alternate) = self.select_excluding(model, affinity, &[id]).await {
                    return Ok(alternate);
                }
            }
            return pinned;
        }
        if !self
            .account_enabled(id)
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?
        {
            return match self.select_excluding(model, affinity, &[id]).await {
                Ok(alternate) if alternate.info.id < 0 => self.linked_platform(model, id).await,
                result => result,
            };
        }
        let pinned = self.select_pinned(model, id).await;
        let exhausted = self.quota_exhausted(id).await?;
        let threshold = self.threshold_reached(id).await?;
        let preempted = self.routing_preempted(model, id).await?;
        if (exhausted || threshold || preempted)
            && matches!(pinned, Ok(_) | Err(SelectError::Cooldown(_)))
        {
            if let Ok(alternate) = self.select_excluding(model, affinity, &[id]).await {
                // Avoid leaving credit-backed capacity for another exhausted subscription.
                let better = if !exhausted && !threshold {
                    let alternative = alternate.info.id;
                    self.db
                        .call(move |conn| {
                            let now = Utc::now().timestamp();
                            Ok(crate::routing::decision(conn, alternative, now)?
                                .compare(&crate::routing::decision(conn, id, now)?)
                                .is_lt())
                        })
                        .await
                        .map_err(|_| SelectError::UpstreamUnavailable)?
                } else {
                    true
                };
                if better
                    && !self.quota_exhausted(alternate.info.id).await?
                    && (exhausted
                        || !threshold
                        || !self.threshold_reached(alternate.info.id).await?)
                {
                    if alternate.info.id > 0 {
                        return Ok(alternate);
                    }
                    if let Ok(project) = self.linked_platform(model, id).await {
                        return Ok(project);
                    }
                }
            }
        }
        pinned
    }

    /// A better economic tier may preempt a healthy session only through the
    /// existing full-context soft-transfer path, never during a generation.
    pub(crate) async fn routing_preempted(
        &self,
        model: &str,
        owner: i64,
    ) -> std::result::Result<bool, SelectError> {
        if owner < 0 {
            return Ok(false);
        }
        let requested = model.to_owned();
        let ids=self.db.call(move |conn|{
            let now=Utc::now().timestamp();
            let current=crate::routing::decision(conn,owner,now)?;
            let mut stmt=conn.prepare("SELECT a.id FROM oauth_accounts a JOIN oauth_models m ON m.account_id=a.id WHERE a.id<>?1 AND a.state='active' AND m.model=?2 AND a.expires_at>?3+60 AND (a.cooldown_until IS NULL OR a.cooldown_until<=?3) AND NOT EXISTS(SELECT 1 FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation AND h.retry_at>?3)")?;
            let ids=stmt.query_map(params![owner,requested,now],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let mut urgent=Vec::new();
            for id in ids {
                if !crate::account_preferences::effective(conn,id)?.enabled {continue;}
                let candidate=crate::routing::decision(conn,id,now)?;
                let quota=crate::quota::summary(conn,id,now)?;
                if candidate.compare(&current).is_lt() && !candidate.reached(&quota) && quota.status=="observed" {urgent.push((candidate,id));}
            }
            urgent.sort_by(|(a,aid),(b,bid)|a.compare(b).then_with(||aid.cmp(bid)));
            Ok(urgent.into_iter().map(|(_,id)|id).take(4).collect::<Vec<_>>())
        }).await.map_err(|_|SelectError::UpstreamUnavailable)?;
        // An idle alternative's catalog may have aged out during a long owned
        // session. Refresh it on demand, not with background inference/probes.
        for id in ids {
            if self
                .account_models(id)
                .await
                .is_ok_and(|models| models.iter().any(|m| m.id == model))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn check_available(&self) -> std::result::Result<(), SelectError> {
        let native = self
            .db
            .call(|conn| {
                let now = Utc::now().timestamp();
                if !crate::platform::Policy::read(conn)?.codex() {
                    return Ok(Err(SelectError::UpstreamUnavailable));
                }
                let accounts = oauth::list(conn)?;
                let mut wait = None;
                for account in accounts
                    .into_iter()
                    .filter(|account| account.state == "active")
                {
                    if !crate::account_preferences::effective(conn, account.id)?.enabled {
                        continue;
                    }
                    let cooldown = account.cooldown_until.filter(|until| *until > now);
                    let health = account.health_until.filter(|until| *until > now);
                    let candidate = match (cooldown, health) {
                        (Some(c), Some(h)) => (c.max(h), c >= h),
                        (Some(c), None) => (c, true),
                        (None, Some(h)) => (h, false),
                        _ => return Ok(Ok(())),
                    };
                    wait = Some(wait.map_or(candidate, |old: (i64, bool)| {
                        if candidate.0 < old.0 {
                            candidate
                        } else {
                            old
                        }
                    }));
                }
                Ok(Err(wait_error(wait)))
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        if native.is_ok() {
            return native;
        }
        let _ =
            crate::platform::route_catalog(&self.db, &self.vault, &self.config.platform_base).await;
        let vault = self.vault.clone();
        let platform = self
            .db
            .call(move |conn| {
                let now = Utc::now().timestamp();
                let policy = crate::platform::Policy::read(conn)?;
                if !policy.platform() {
                    return Ok(false);
                }
                for owner in crate::platform::routes::owners(conn)? {
                    let Ok(account) = crate::platform::routes::account(conn, &vault, owner, now)
                    else {
                        continue;
                    };
                    if account.info.cooldown_until.is_some() || account.info.health_until.is_some()
                    {
                        continue;
                    }
                    if let Ok(models) = crate::platform::routes::models(conn, owner, now) {
                        for model in models {
                            if crate::platform::routes::fallback_eligible(conn, &model.id, now)? {
                                return Ok(true);
                            }
                        }
                    }
                }
                Ok(false)
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        if platform {
            Ok(())
        } else {
            native
        }
    }
    async fn failure_wait(&self, model: &str) -> SelectError {
        let model = model.to_owned();
        let wait=self.db.call(move |conn| {
            let now=Utc::now().timestamp(); let mut wait=None;
            let mut stmt=conn.prepare("SELECT a.cooldown_until,(SELECT MAX(h.retry_at) FROM oauth_health h WHERE h.account_id=a.id AND h.generation=a.generation) FROM oauth_accounts a JOIN oauth_models m ON m.account_id=a.id WHERE a.state='active' AND m.model=?1")?;
            let rows=stmt.query_map([model],|row|Ok((row.get::<_,Option<i64>>(0)?,row.get::<_,Option<i64>>(1)?)))?;
            for row in rows { let (c,h)=row?; let candidate=match(c.filter(|at|*at>now),h.filter(|at|*at>now)) { (Some(c),Some(h))=>Some((c.max(h),c>=h)),(Some(c),None)=>Some((c,true)),(None,Some(h))=>Some((h,false)),_=>None };
                if let Some(candidate)=candidate {wait=Some(wait.map_or(candidate,|old:(i64,bool)|if candidate.0<old.0 {candidate} else {old}));}
            }
            Ok(wait)
        }).await;
        wait_error(wait.ok().flatten())
    }

    pub async fn select_pinned(
        &self,
        model: &str,
        id: i64,
    ) -> std::result::Result<Selection, SelectError> {
        if !self.routing_policy().await?.allows(id) {
            return Err(SelectError::UpstreamUnavailable);
        }
        if id < 0 {
            let account = self
                .account(id)
                .await
                .map_err(|_| SelectError::UpstreamUnavailable)?;
            self.validate(&account, model).await?;
            let reservation = self
                .pool
                .reserve(&[crate::pool::Candidate {
                    id,
                    quota: None,
                    routing: Default::default(),
                }])
                .expect("one candidate");
            return Ok(Selection {
                account,
                _reservation: reservation,
            });
        }
        let enabled = self
            .db
            .call(move |conn| Ok(crate::account_preferences::effective(conn, id)?.enabled))
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        if !enabled {
            return Err(SelectError::UpstreamUnavailable);
        }
        let paused = self
            .db
            .call(move |conn| {
                let now = Utc::now().timestamp();
                let cooldown = crate::quota::blocked_until(conn, id, now)?;
                let health = crate::health::paused(conn, id, now)?;
                Ok(match (cooldown, health) {
                    (Some(c), Some(h)) => Some((c.max(h), c >= h)),
                    (Some(c), None) => Some((c, true)),
                    (None, Some(h)) => Some((h, false)),
                    _ => None,
                })
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        if paused.is_some() {
            return Err(wait_error(paused));
        }
        let reservation = self
            .pool
            .reserve(&[crate::pool::Candidate {
                id,
                quota: None,
                routing: Default::default(),
            }])
            .expect("one candidate");
        let account = self
            .account(id)
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        self.validate(&account, model).await?;
        Ok(Selection {
            account,
            _reservation: reservation,
        })
    }

    pub async fn validate(
        &self,
        account: &Account,
        model: &str,
    ) -> std::result::Result<(), SelectError> {
        if crate::catalog::embedding_model(model) {
            return Err(SelectError::ModelUnavailable);
        }
        self.validate_model(account, model).await
    }

    async fn validate_model(
        &self,
        account: &Account,
        model: &str,
    ) -> std::result::Result<(), SelectError> {
        if account.info.id < 0 {
            let requested = model.to_owned();
            let permitted = self
                .db
                .call(move |conn| {
                    crate::platform::routes::model_spending_permitted(conn, &requested)
                })
                .await
                .map_err(|_| SelectError::UpstreamUnavailable)?;
            if !permitted {
                let _ = self.models().await;
                let requested = model.to_owned();
                if !self
                    .db
                    .call(move |conn| {
                        crate::platform::routes::model_spending_permitted(conn, &requested)
                    })
                    .await
                    .map_err(|_| SelectError::UpstreamUnavailable)?
                {
                    return Err(SelectError::UpstreamUnavailable);
                }
            }
        }
        self.check_cooldown(account).await?;
        if !self
            .account_models(account.info.id)
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?
            .iter()
            .any(|entry| entry.id == model)
        {
            return Err(SelectError::ModelUnavailable);
        }
        self.check_cooldown(account).await
    }

    pub async fn check_cooldown(&self, account: &Account) -> std::result::Result<(), SelectError> {
        let id = account.info.id;
        let generation = account.info.generation;
        if id < 0 {
            if !self.routing_policy().await?.platform() {
                return Err(SelectError::UpstreamUnavailable);
            }
            let current = self
                .account(id)
                .await
                .map_err(|_| SelectError::UpstreamUnavailable)?;
            if current.info.generation != generation {
                return Err(SelectError::UpstreamUnavailable);
            }
            return match (current.info.cooldown_until, current.info.health_until) {
                (Some(c), Some(h)) => Err(if c >= h {
                    SelectError::Cooldown(c)
                } else {
                    SelectError::Backoff(h)
                }),
                (Some(c), None) => Err(SelectError::Cooldown(c)),
                (None, Some(h)) => Err(SelectError::Backoff(h)),
                (None, None) => Ok(()),
            };
        }
        match self
            .db
            .call(move |conn| {
                if !crate::platform::Policy::read(conn)?.codex() { return Err("Codex routing is disabled".into()); }
                let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')",params![id,generation],|row|row.get(0))?;
                if !valid { return Err("OAuth account changed".into()); }
                let now=Utc::now().timestamp();
                Ok((crate::quota::blocked_until(conn,id,now)?,crate::health::paused(conn,id,now)?))
            })
            .await
        {
            Ok((Some(c),Some(h)))=>Err(if c>=h {SelectError::Cooldown(c)} else {SelectError::Backoff(h)}),
            Ok((Some(until),None)) => Err(SelectError::Cooldown(until)),
            Ok((None,Some(until)))=>Err(SelectError::Backoff(until)),
            Ok((None,None)) => Ok(()),
            Err(_) => Err(SelectError::UpstreamUnavailable),
        }
    }

    pub async fn post(
        &self,
        account: &Account,
        path: &str,
        body: &Value,
        session: Option<(&str, &str)>,
        order: i64,
        options: InferenceHeaders<'_>,
    ) -> Result<reqwest::Response> {
        let embedding = path == "embeddings";
        if embedding
            != body["model"]
                .as_str()
                .is_some_and(crate::catalog::embedding_model)
            || (embedding && account.info.id >= 0)
        {
            return Err("model unavailable for this endpoint".into());
        }
        if account.info.id < 0 {
            // Fixed public Responses endpoint, working key only. Codex account,
            // turn/session/Lite headers and the caller's bearer never cross over.
            if !matches!(path, "responses" | "responses/compact" | "embeddings")
                || body["generate"] == false
            {
                return Err("unsupported Platform operation".into());
            }
            self.check_cooldown(account)
                .await
                .map_err(|_| "Platform route is unavailable")?;
            let model = body["model"]
                .as_str()
                .ok_or("model is required")?
                .to_owned();
            if !self
                .db
                .call(move |conn| crate::platform::routes::model_spending_permitted(conn, &model))
                .await?
            {
                return Err("Platform routing is disabled for this model".into());
            }
            let prepared;
            let body = if embedding {
                body
            } else {
                prepared = crate::platform::inference_payload(body, path == "responses/compact")?;
                &prepared
            };
            let request = self
                .inference_client
                .post(format!("{}/{path}", self.config.platform_base))
                .bearer_auth(&account.credentials.access_token)
                .header(
                    reqwest::header::ACCEPT,
                    if path != "responses" {
                        "application/json"
                    } else {
                        "text/event-stream"
                    },
                )
                .json(body);
            return match send_inference_post(request).await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if status == 401 {
                        self.unauthorized(account, crate::health::Scope::Responses, order)
                            .await?;
                    } else if status == 408 || (500..=599).contains(&status) {
                        self.failure(
                            account,
                            crate::health::Scope::Responses,
                            order,
                            "upstream_5xx",
                            retry_at(response.headers()),
                        )
                        .await?;
                    }
                    Ok(response)
                }
                Err(failure) => {
                    let _ = self
                        .failure(
                            account,
                            crate::health::Scope::Responses,
                            order,
                            "transport",
                            None,
                        )
                        .await;
                    Err(failure.into())
                }
            };
        }
        let mut headers = inference_headers(account, body, options.responses_lite)?;
        if let Some(value) = options.turn_state {
            headers.insert("x-codex-turn-state", value.clone());
        }
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static(if path == "responses" {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        if let Some(session) = session {
            session_headers(&mut headers, session)?;
        }
        let request = self
            .inference_client
            .post(format!("{}/{path}", self.config.base_url))
            .headers(headers)
            .json(body);
        match send_inference_post(request).await {
            Ok(response) => {
                let status = response.status().as_u16();
                let observed = if status == 401 {
                    self.unauthorized(account, crate::health::Scope::Responses, order)
                        .await
                } else if status == 408 || (500..=599).contains(&status) {
                    self.failure(
                        account,
                        crate::health::Scope::Responses,
                        order,
                        "upstream_5xx",
                        retry_at(response.headers()),
                    )
                    .await
                } else {
                    Ok(())
                };
                if observed.is_err() {
                    tracing::error!(event = "health_store_failed");
                }
                Ok(response)
            }
            Err(failure) => {
                let _ = self
                    .failure(
                        account,
                        crate::health::Scope::Responses,
                        order,
                        "transport",
                        None,
                    )
                    .await;
                Err(failure.into())
            }
        }
    }

    /// Handshake quota refusals prove no response.create was submitted.
    pub(crate) async fn connect_with_quota_fallback(
        &self,
        mut account: Selection,
        model: &str,
        identity: (&str, &str),
        payload: &mut Value,
        responses_lite: bool,
        same_pair: bool,
    ) -> std::result::Result<(Selection, ResponsesConnection, HeaderMap), SocketError> {
        let mut excluded = Vec::new();
        loop {
            match self
                .connect(&account, identity, payload, responses_lite)
                .await
            {
                Ok((socket, headers)) => return Ok((account, socket, headers)),
                Err(SocketError::Cooldown(until)) => {
                    excluded.push(account.info.id);
                    if excluded.len() >= 4 {
                        return Err(SocketError::Cooldown(until));
                    }
                    let alternate = self
                        .select_after_quota_refusal(model, account.info.id, &excluded, same_pair)
                        .await
                        .map_err(|_| SocketError::Cooldown(until))?;
                    if (account.info.id < 0 || alternate.info.id < 0)
                        && !crate::platform::transfer_safe(payload)
                        && !crate::platform::replay_safe(payload)
                    {
                        return Err(SocketError::Cooldown(until));
                    }
                    account = alternate;
                    if let Some(metadata) = payload
                        .get_mut("client_metadata")
                        .and_then(Value::as_object_mut)
                    {
                        metadata.remove("x-codex-turn-state");
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) async fn connect(
        &self,
        account: &Account,
        session: (&str, &str),
        body: &Value,
        responses_lite: bool,
    ) -> std::result::Result<(ResponsesConnection, HeaderMap), SocketError> {
        if account.info.id < 0 && body["generate"] == false {
            // Public HTTP cannot emulate a native non-generating warmup.
            return Err(SocketError::Unavailable);
        }
        if account.info.id < 0 || prefer_http(body) {
            match self.check_cooldown(account).await {
                Ok(()) => return Ok((ResponsesConnection::Http, HeaderMap::new())),
                Err(SelectError::Cooldown(until)) => return Err(SocketError::Cooldown(until)),
                Err(SelectError::Backoff(until)) => return Err(SocketError::Backoff(until)),
                Err(_) => return Err(SocketError::Storage),
            }
        }
        self.websocket(account, session, false, body, responses_lite)
            .await
            .map(|(socket, headers)| (ResponsesConnection::WebSocket(Box::new(socket)), headers))
    }

    pub async fn websocket(
        &self,
        account: &Account,
        session: (&str, &str),
        http_fallback: bool,
        body: &Value,
        responses_lite: bool,
    ) -> std::result::Result<(UpstreamSocket, HeaderMap), SocketError> {
        if account.info.id < 0 {
            return Err(SocketError::Unavailable);
        }
        match self.check_cooldown(account).await {
            Ok(()) => {}
            Err(SelectError::Cooldown(until)) => return Err(SocketError::Cooldown(until)),
            Err(SelectError::Backoff(until)) => return Err(SocketError::Backoff(until)),
            Err(_) => return Err(SocketError::Storage),
        }
        let started = std::time::Instant::now();
        let log_failure = |reason: &'static str, http_status: Option<u16>| {
            tracing::info!(
                event = "upstream_websocket_handshake_failed",
                reason,
                http_status,
                elapsed_ms = started.elapsed().as_millis() as u64,
            );
        };
        let request: Result<_> = (|| {
            let mut url = Url::parse(&format!("{}/responses", self.config.base_url))?;
            url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
                .map_err(|_| "invalid WebSocket scheme")?;
            let mut request = url.as_str().into_client_request()?;
            request
                .headers_mut()
                .extend(inference_headers(account, body, responses_lite)?);
            session_headers(request.headers_mut(), session)?;
            request.headers_mut().insert(
                "openai-beta",
                HeaderValue::from_static("responses_websockets=2026-02-06"),
            );
            Ok(request)
        })();
        let request = request.map_err(|_| {
            log_failure("invalid_request", None);
            SocketError::Unavailable
        })?;
        let mut config = WebSocketConfig::default()
            .max_message_size(Some(crate::payload::UPSTREAM_WS_MESSAGE_BYTES))
            .max_frame_size(Some(crate::payload::UPSTREAM_WS_MESSAGE_BYTES));
        // Match the released Codex client's RFC 7692 negotiation. Incoming
        // decompressed events retain the independent 1 MiB message bound.
        config.extensions.permessage_deflate =
            Some(codex_tungstenite::extensions::compression::deflate::DeflateConfig::default());
        let order = self
            .db
            .call(|conn| crate::health::next(conn))
            .await
            .map_err(|_| SocketError::Storage)?;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(request, Some(config), false),
        )
        .await;
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                log_failure("timed_out", None);
                if !http_fallback {
                    self.failure(
                        account,
                        crate::health::Scope::Responses,
                        order,
                        "transport",
                        None,
                    )
                    .await
                    .map_err(|_| SocketError::Storage)?;
                }
                return Err(SocketError::Unavailable);
            }
        };
        if let Err(error) = &result {
            let status = match error {
                codex_tungstenite::Error::Http(response) => Some(response.status().as_u16()),
                _ => None,
            };
            log_failure(websocket_error_kind(error), status);
        }
        let (socket, response) = match result {
            Ok(value) => value,
            Err(codex_websocket::tungstenite::Error::Http(response))
                if response.status().as_u16() == 429 =>
            {
                let body = response
                    .body()
                    .as_ref()
                    .filter(|body| body.len() <= 65_536)
                    .and_then(|body| serde_json::from_slice::<Value>(body).ok());
                self.observe_handshake(account, response.headers().clone(), 429, body)
                    .await?;
                return match self.check_cooldown(account).await {
                    Err(SelectError::Cooldown(until)) => Err(SocketError::Cooldown(until)),
                    Err(_) => Err(SocketError::Storage),
                    Ok(()) => Err(SocketError::Cooldown(Utc::now().timestamp())),
                };
            }
            Err(codex_websocket::tungstenite::Error::Http(response)) => {
                if response.status().as_u16() == 401 {
                    self.unauthorized(account, crate::health::Scope::Responses, order)
                        .await
                        .map_err(|_| SocketError::Storage)?;
                    return Err(SocketError::Authentication);
                }
                if !http_fallback
                    && (response.status().as_u16() == 408 || response.status().is_server_error())
                {
                    self.failure(
                        account,
                        crate::health::Scope::Responses,
                        order,
                        "upstream_5xx",
                        retry_at(response.headers()),
                    )
                    .await
                    .map_err(|_| SocketError::Storage)?;
                }
                return Err(SocketError::Unavailable);
            }
            Err(_) => {
                if !http_fallback {
                    self.failure(
                        account,
                        crate::health::Scope::Responses,
                        order,
                        "transport",
                        None,
                    )
                    .await
                    .map_err(|_| SocketError::Storage)?;
                }
                return Err(SocketError::Unavailable);
            }
        };
        self.observe_handshake(account, response.headers().clone(), 200, None)
            .await?;
        let compressed = response
            .headers()
            .get_all("sec-websocket-extensions")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|extension| {
                extension
                    .split(';')
                    .next()
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case("permessage-deflate"))
            });
        tracing::info!(
            event = "upstream_websocket_transport",
            permessage_deflate = compressed
        );
        Ok((
            UpstreamSocket { socket, compressed },
            response.headers().clone(),
        ))
    }

    async fn observe_handshake(
        &self,
        account: &Account,
        headers: HeaderMap,
        status: u16,
        body: Option<Value>,
    ) -> std::result::Result<(), SocketError> {
        let id = account.info.id;
        let generation = account.info.generation;
        self.db
            .call(move |conn| {
                let now = Utc::now().timestamp();
                // Handshakes precede inference and never outrank request observations.
                crate::quota::save(
                    conn,
                    id,
                    generation,
                    0,
                    crate::quota::http(&headers, status, body.as_ref(), now),
                    now,
                )
            })
            .await
            .map_err(|_| SocketError::Storage)
    }
    async fn failure(
        &self,
        account: &Account,
        scope: crate::health::Scope,
        order: i64,
        reason: &'static str,
        retry: Option<i64>,
    ) -> Result<()> {
        let (id, generation) = (account.info.id, account.info.generation);
        self.db
            .call(move |conn| {
                crate::health::failure(
                    conn,
                    (id, generation),
                    scope,
                    order,
                    reason,
                    retry,
                    Utc::now().timestamp(),
                )
            })
            .await
    }
    async fn success(
        &self,
        account: &Account,
        scope: crate::health::Scope,
        order: i64,
    ) -> Result<()> {
        let (id, generation) = (account.info.id, account.info.generation);
        self.db
            .call(move |conn| crate::health::success(conn, id, generation, scope, order))
            .await
    }
    async fn unauthorized(
        &self,
        account: &Account,
        scope: crate::health::Scope,
        order: i64,
    ) -> Result<()> {
        let (id, generation) = (account.info.id, account.info.generation);
        self.db
            .call(move |conn| {
                crate::health::unauthorized(
                    conn,
                    id,
                    generation,
                    scope,
                    order,
                    Utc::now().timestamp(),
                )
            })
            .await
    }
}

fn wait_error(wait: Option<(i64, bool)>) -> SelectError {
    match wait {
        Some((until, true)) => SelectError::Cooldown(until),
        Some((until, false)) => SelectError::Backoff(until),
        None => SelectError::UpstreamUnavailable,
    }
}
fn retry_at(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| crate::quota::retry_at(value, Utc::now().timestamp()))
}

pub(crate) fn account_headers(account: &Account) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let mut bearer = HeaderValue::from_str(&format!("Bearer {}", account.credentials.access_token))
        .map_err(|_| "invalid OAuth access token")?;
    bearer.set_sensitive(true);
    headers.insert("authorization", bearer);
    headers.insert(
        "chatgpt-account-id",
        HeaderValue::from_str(&account.info.account_id).map_err(|_| "invalid OAuth account ID")?,
    );
    headers.insert("originator", HeaderValue::from_static("exetrouter"));
    headers.insert(
        "x-codex-beta-features",
        HeaderValue::from_static("remote_compaction_v2"),
    );
    Ok(headers)
}

fn inference_headers(account: &Account, body: &Value, responses_lite: bool) -> Result<HeaderMap> {
    let mut headers = account_headers(account)?;
    // Tool-free compaction/continuation may carry only the explicit Lite mode.
    // Other native clients identify it through input or WS frame metadata.
    let lite = responses_lite
        || body["input"]
            .as_array()
            .is_some_and(|input| input.iter().any(|item| item["type"] == "additional_tools"))
        || body["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"]
            == "true";
    if lite {
        headers.insert(
            "x-openai-internal-codex-responses-lite",
            HeaderValue::from_static("true"),
        );
    }
    Ok(headers)
}

fn session_headers(headers: &mut HeaderMap, session: (&str, &str)) -> Result<()> {
    // Native Codex sends both session and thread identities. Use the router's
    // existing user/model-scoped digest, never a raw client header or thread ID.
    let value = HeaderValue::from_str(session.0)?;
    for name in ["session_id", "session-id"] {
        headers.insert(name, value.clone());
    }
    headers.insert("thread-id", HeaderValue::from_str(session.1)?);
    Ok(())
}

/// Parse SSE frames without buffering an entire response. The wire bytes are
/// retained so tool arguments, extensions and ordering pass through unchanged.
#[derive(Default)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
}
pub(crate) struct SseFrame {
    pub wire: Vec<u8>,
    pub event: Option<Value>,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>> {
        let mut frames = Vec::new();
        for byte in chunk {
            self.buffer.push(*byte);
            if self.buffer.len() > crate::payload::UPSTREAM_SSE_EVENT_BYTES {
                return Err("upstream event too large".into());
            }
            if self.buffer.ends_with(b"\n\n") || self.buffer.ends_with(b"\r\n\r\n") {
                let wire = std::mem::take(&mut self.buffer);
                let text = std::str::from_utf8(&wire).map_err(|_| "invalid SSE encoding")?;
                let data = text
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|s| s.strip_prefix(' ').unwrap_or(s))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let event = if data.is_empty() || data == "[DONE]" {
                    None
                } else {
                    Some(serde_json::from_str(&data).map_err(|_| "invalid SSE event")?)
                };
                frames.push(SseFrame { wire, event });
            }
        }
        Ok(frames)
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

fn store_model_catalog(
    conn: &mut rusqlite::Connection,
    id: i64,
    generation: i64,
    models: Vec<Model>,
    updated_at: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    let valid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1 AND generation=?2 AND state='active')",
        params![id, generation],
        |row| row.get(0),
    )?;
    if !valid {
        return Err("OAuth account changed during model discovery".into());
    }
    let mut incoming = models
        .into_iter()
        .map(|model| {
            (
                model.id,
                model.display_name,
                model.metadata.map(|value| value.to_string()),
            )
        })
        .collect::<Vec<_>>();
    incoming.sort();
    let existing = {
        let mut statement = tx.prepare(
            "SELECT model,display_name,metadata FROM oauth_models WHERE account_id=?1 ORDER BY model",
        )?;
        let rows = statement
            .query_map([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    // The generation fence and comparison share the write transaction. A normal
    // refresh must renew freshness without rewriting an unchanged catalog/index.
    if existing != incoming {
        tx.execute("DELETE FROM oauth_models WHERE account_id=?1", [id])?;
        for (model, display_name, metadata) in incoming {
            tx.execute(
                "INSERT INTO oauth_models(account_id,model,display_name,metadata) VALUES(?1,?2,?3,?4)",
                params![id, model, display_name, metadata],
            )?;
        }
    }
    tx.execute(
        "UPDATE oauth_accounts SET catalog_updated_at=?1 WHERE id=?2",
        params![updated_at, id],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_dns_lookup_has_a_fixed_http_category() {
        use reqwest::dns::{Name, Resolve, Resolving};
        struct UnavailableDns;
        impl Resolve for UnavailableDns {
            fn resolve(&self, _: Name) -> Resolving {
                Box::pin(async { Err("private resolver detail".into()) })
            }
        }
        let client = Client::builder()
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(UnavailableDns))
            .build()
            .unwrap();
        let failure = send_inference_post(client.post("http://upstream.invalid/responses"))
            .await
            .unwrap_err();
        assert_eq!(failure.kind, "dns");
        assert_eq!(failure.to_string(), "upstream connection failed");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn http_tcp_timeout_matches_read_budget() {
        use tokio::io::AsyncWriteExt;

        for budget in [Duration::from_secs(300), INFERENCE_IDLE_TIMEOUT] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n")
                    .await
                    .unwrap();
                socket
            });
            let client = client_with_read_timeout(budget).unwrap();
            let response = client
                .get(format!("http://{address}"))
                .send()
                .await
                .unwrap();
            // Keep both ends open, with the response body still pending, while
            // inspecting the actual client's socket rather than builder Debug.
            let socket = server.await.unwrap();
            let client_port = socket.peer_addr().unwrap().port();
            let timeout = std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .find_map(|entry| {
                    let fd: libc::c_int = entry.ok()?.file_name().to_str()?.parse().ok()?;
                    // SAFETY: zero is valid for sockaddr_in; both calls receive
                    // writable buffers with their correct sizes. Invalid or
                    // non-socket descriptors are harmlessly rejected by libc.
                    let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                    let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
                    let result = unsafe {
                        libc::getsockname(
                            fd,
                            (&mut address as *mut libc::sockaddr_in).cast(),
                            &mut length,
                        )
                    };
                    if result != 0
                        || address.sin_family != libc::AF_INET as libc::sa_family_t
                        || u16::from_be(address.sin_port) != client_port
                    {
                        return None;
                    }
                    let mut timeout: libc::c_uint = 0;
                    let mut length = std::mem::size_of_val(&timeout) as libc::socklen_t;
                    let result = unsafe {
                        libc::getsockopt(
                            fd,
                            libc::IPPROTO_TCP,
                            libc::TCP_USER_TIMEOUT,
                            (&mut timeout as *mut libc::c_uint).cast(),
                            &mut length,
                        )
                    };
                    (result == 0).then_some(Duration::from_millis(u64::from(timeout)))
                })
                .expect("pending HTTP client socket");
            assert_eq!(timeout, budget);
            drop(response);
            drop(socket);
        }
    }

    fn catalog_database() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE oauth_accounts(id INTEGER PRIMARY KEY,generation INTEGER,state TEXT,catalog_updated_at INTEGER);
             CREATE TABLE oauth_models(account_id INTEGER,model TEXT,display_name TEXT,metadata TEXT,PRIMARY KEY(account_id,model));
             INSERT INTO oauth_accounts VALUES(1,2,'active',NULL),(2,2,'active',NULL);",
        )
        .unwrap();
        conn
    }

    fn catalog_model(id: &str) -> Model {
        Model {
            id: id.into(),
            display_name: id.into(),
            object: "model".into(),
            owned_by: "openai".into(),
            source: "codex".into(),
            platform_fallback: false,
            capabilities: Vec::new(),
            metadata: None,
        }
    }

    #[test]
    fn unchanged_catalog_only_renews_freshness_even_when_reordered() {
        let mut conn = catalog_database();
        let models = vec![catalog_model("model-a"), catalog_model("model-b")];
        store_model_catalog(&mut conn, 1, 2, models.clone(), 100).unwrap();
        store_model_catalog(&mut conn, 2, 2, models.clone(), 101).unwrap();
        let before = conn.total_changes();
        store_model_catalog(&mut conn, 1, 2, models.into_iter().rev().collect(), 160).unwrap();
        assert_eq!(conn.total_changes() - before, 1);
        let timestamps: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT catalog_updated_at FROM oauth_accounts WHERE id=1),(SELECT catalog_updated_at FROM oauth_accounts WHERE id=2)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(timestamps, (160, 101));
    }

    #[test]
    fn catalog_changes_replace_metadata_names_membership_and_empty_sets() {
        let mut conn = catalog_database();
        let mut model = catalog_model("model-a");
        for revision in 0..4 {
            if revision == 1 {
                model.display_name = "Renamed".into();
            } else if revision == 2 {
                model.metadata = Some(serde_json::json!({"context_window":1234}));
            } else if revision == 3 {
                model.id = "model-b".into();
            }
            let before = conn.total_changes();
            store_model_catalog(&mut conn, 1, 2, vec![model.clone()], 100 + revision).unwrap();
            assert_eq!(
                conn.total_changes() - before,
                if revision == 0 { 2 } else { 3 }
            );
            let stored: (String, String, Option<String>) = conn
                .query_row(
                    "SELECT model,display_name,metadata FROM oauth_models WHERE account_id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                stored,
                (
                    model.id.clone(),
                    model.display_name.clone(),
                    model.metadata.as_ref().map(Value::to_string)
                )
            );
        }
        store_model_catalog(&mut conn, 1, 2, vec![], 200).unwrap();
        let before = conn.total_changes();
        store_model_catalog(&mut conn, 1, 2, vec![], 260).unwrap();
        assert_eq!(conn.total_changes() - before, 1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM oauth_models", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn stale_or_disabled_catalog_refresh_cannot_mutate_rows_or_freshness() {
        let mut conn = catalog_database();
        store_model_catalog(&mut conn, 1, 2, vec![catalog_model("model-a")], 100).unwrap();
        for (generation, state) in [(3, "active"), (2, "disabled")] {
            conn.execute(
                "UPDATE oauth_accounts SET generation=?1,state=?2 WHERE id=1",
                params![generation, state],
            )
            .unwrap();
            let before = conn.total_changes();
            assert!(store_model_catalog(&mut conn, 1, 2, vec![], 200).is_err());
            assert_eq!(conn.total_changes(), before);
            assert_eq!(
                conn.query_row(
                    "SELECT catalog_updated_at FROM oauth_accounts WHERE id=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                100
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM oauth_models WHERE account_id=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
    }

    #[test]
    fn invalid_duplicate_catalog_rolls_back_the_previous_catalog() {
        let mut conn = catalog_database();
        store_model_catalog(&mut conn, 1, 2, vec![catalog_model("model-a")], 100).unwrap();
        assert!(store_model_catalog(
            &mut conn,
            1,
            2,
            vec![catalog_model("model-b"), catalog_model("model-b")],
            200
        )
        .is_err());
        let stored: (String, i64) = conn.query_row("SELECT model,catalog_updated_at FROM oauth_models JOIN oauth_accounts ON account_id=id WHERE id=1", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(stored, ("model-a".into(), 100));
    }

    #[test]
    fn http_preference_is_not_an_input_limit_or_a_generating_warmup() {
        let mut body = serde_json::json!({"input":[{"role":"user","content":"small"}]});
        assert!(!prefer_http(&body));
        body["input"][0]["content"] = serde_json::json!("x".repeat(16 * 1024 * 1024));
        assert!(prefer_http(&body));
        body["generate"] = serde_json::json!(false);
        assert!(!prefer_http(&body));
        body["generate"] = serde_json::json!(true);
        assert!(prefer_http(&body));
    }

    #[test]
    fn sse_event_budget_is_independent_of_large_inference_input() {
        let limit = crate::payload::UPSTREAM_SSE_EVENT_BYTES;
        let mut decoder = SseDecoder::default();
        let mut comment = vec![b'x'; limit - 2];
        comment[0] = b':';
        // A complete comment at the event limit is accepted and releases its buffer.
        assert!(decoder.push(&comment).unwrap().is_empty());
        assert_eq!(decoder.push(b"\n\n").unwrap().len(), 1);
        assert!(decoder.is_empty());
        // Chunking cannot bypass the limit on a single unfinished event.
        assert!(decoder.push(&vec![b'x'; limit]).unwrap().is_empty());
        assert!(decoder.push(b"x").is_err());
    }
    #[test]
    fn sse_survives_fragmentation_and_multiline_data() {
        let input=b": heartbeat\r\n\r\ndata: {\r\ndata: \"type\":\"response.completed\"}\r\n\r\ndata: [DONE]\n\n";
        let mut decoder = SseDecoder::default();
        let mut frames = Vec::new();
        for chunk in input.chunks(3) {
            frames.extend(decoder.push(chunk).unwrap());
        }
        assert_eq!(frames.len(), 3);
        assert_eq!(
            frames[1].event.as_ref().unwrap()["type"],
            "response.completed"
        );
        assert!(decoder.is_empty());
        assert_eq!(
            frames.into_iter().flat_map(|f| f.wire).collect::<Vec<_>>(),
            input
        );
    }
    #[test]
    fn mock_endpoint_overrides_cannot_leak_credentials_remotely() {
        assert!(Config::new(BACKEND.into(), oauth::ISSUER.into(), false).is_ok());
        assert!(Config::new(
            "http://127.0.0.1:9999/v1".into(),
            "http://127.0.0.1:9999".into(),
            true
        )
        .is_ok());
        for url in [
            "http://example.com/v1",
            "http://localhost/v1",
            "http://127.0.0.1@evil.example/v1",
            "http://127.0.0.1/v1?token=x",
        ] {
            assert!(Config::new(url.into(), oauth::ISSUER.into(), true).is_err());
        }
    }
}
