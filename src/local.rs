//! Single-owner in-process runtime for exr. No SSH or remote management daemon.
use crate::{
    oauth::{self, Vault},
    server,
    store::Database,
    upstream::{self, Upstream},
    ControlRequest, Result,
};
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    net::SocketAddr,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::Path,
    sync::Arc,
};

pub(crate) struct Local {
    pub db: Database,
    pub upstream: Arc<Upstream>,
    pub key: Vec<u8>,
    pub vault: Vault,
    pub user: i64,
    pub address: SocketAddr,
    pub issuer: String,
    pub attached: bool,
    status: Option<server::LocalStatus>,
    _lock: std::sync::Mutex<Option<fs::File>>,
    shutdown: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
}
fn private_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err("local state must be owned by you with private permissions".into());
    }
    Ok(())
}
fn key(path: &Path) -> Result<[u8; 32]> {
    if !path.exists() {
        let mut value = [0; 32];
        OsRng.fill_bytes(&mut value);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&value)?;
        file.sync_all()?;
    }
    private_file(path)?;
    fs::read(path)?
        .try_into()
        .map_err(|_| "local key must contain 32 bytes".into())
}
impl Local {
    pub async fn open(root: &Path, address: SocketAddr, serve: bool) -> Result<Arc<Self>> {
        Self::open_with(
            root,
            address,
            serve,
            upstream::Config::new(upstream::BACKEND.into(), oauth::ISSUER.into(), false)?,
        )
        .await
    }
    pub(crate) async fn open_with(
        root: &Path,
        address: SocketAddr,
        serve: bool,
        config: upstream::Config,
    ) -> Result<Arc<Self>> {
        if !address.ip().is_loopback() {
            return Err("Standalone API must listen on loopback".into());
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let meta = fs::symlink_metadata(root)?;
        if !meta.is_dir()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err("Standalone state directory must be owned by you with mode 700".into());
        }
        let mut attached = false;
        let lock = if serve {
            let path = root.join("runtime.lock");
            let file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            private_file(&path)?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                attached = true;
                None
            } else {
                Some(file)
            }
        } else {
            None
        };
        let hmac = key(&root.join("hmac.key"))?;
        let oauth_key = key(&root.join("oauth.key"))?;
        if hmac == oauth_key {
            return Err("Local encryption keys must differ".into());
        }
        let db_path = root.join("state.sqlite");
        if !db_path.exists() {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db_path)?;
        }
        private_file(&db_path)?;
        let db = Database::open(db_path).await?;
        let user = db
            .call(|conn| {
                let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
                if count == 0 {
                    return crate::create_user(conn, "standalone");
                }
                if count != 1 {
                    return Err("Standalone requires its own single-owner state directory".into());
                }
                let (id, name): (i64, String) =
                    conn.query_row("SELECT id,name FROM users", [], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
                if name != "standalone" {
                    return Err("Standalone cannot reuse a managed server database".into());
                }
                Ok(id)
            })
            .await?;
        let issuer = config.issuer.clone();
        let upstream = Arc::new(Upstream::new(db.clone(), oauth_key, config)?);
        let requested_address = address;
        let address = if attached {
            let endpoint = root.join("endpoint.json");
            private_file(&endpoint)?;
            let address: SocketAddr = serde_json::from_slice::<Value>(&fs::read(endpoint)?)
                .map_err(|_| "Local endpoint metadata unavailable")?["address"]
                .as_str()
                .ok_or("Local endpoint metadata unavailable")?
                .parse()?;
            if requested_address.port() != 0 && requested_address != address {
                return Err(
                    "Stop the existing standalone API before changing its listen address".into(),
                );
            }
            if !address.ip().is_loopback() {
                return Err("Invalid local API endpoint".into());
            }
            address
        } else {
            address
        };
        let (address, shutdown, task, status) = if serve && !attached {
            let runtime =
                server::local_api(db.clone(), hmac.to_vec(), upstream.clone(), address).await?;
            let endpoint = root.join("endpoint.json");
            let temp = root.join(format!("endpoint-{}.tmp", rand::random::<u64>()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(
                json!({"address":runtime.address.to_string()})
                    .to_string()
                    .as_bytes(),
            )?;
            file.sync_all()?;
            fs::rename(&temp, &endpoint)?;
            (
                runtime.address,
                Some(runtime.shutdown),
                Some(runtime.task),
                Some(runtime.status),
            )
        } else {
            (address, None, None, None)
        };
        Ok(Arc::new(Self {
            db,
            upstream,
            vault: Vault::new(oauth_key),
            key: hmac.to_vec(),
            user,
            address,
            issuer,
            attached,
            status,
            _lock: std::sync::Mutex::new(lock),
            shutdown: std::sync::Mutex::new(shutdown),
            task: tokio::sync::Mutex::new(task),
        }))
    }
    pub async fn request(&self, request: ControlRequest) -> Result<Value> {
        match request {
            ControlRequest::Models => {
                Ok(json!({"object":"list","data":self.upstream.models().await?}))
            }
            ControlRequest::Doctor | ControlRequest::Limits => {
                let live = matches!(request, ControlRequest::Limits);
                let user = self.user;
                let limits = self
                    .status
                    .as_ref()
                    .map_or_else(|| server::local_limits(user), |status| status.limits(user));
                let mut report = self
                    .db
                    .call(move |conn| {
                        crate::doctor::snapshot(conn, true, limits, chrono::Utc::now().timestamp())
                    })
                    .await?;
                self.upstream
                    .account_metadata(&mut report, live, user)
                    .await;
                let mut value = serde_json::to_value(report)?;
                self.db
                    .call(move |conn| {
                        crate::account_preferences::annotate(conn, user, &mut value)?;
                        Ok(value)
                    })
                    .await
            }
            ControlRequest::ResetPrepare { account } => {
                self.upstream.prepare_reset(self.user, account).await
            }
            ControlRequest::ResetConfirm { confirmation } => {
                self.upstream
                    .confirm_local_reset(self.user, &confirmation)
                    .await
            }
            other => {
                let key = self.key.clone();
                let user = self.user;
                self.db
                    .call(move |conn| crate::control(conn, &key, user, other))
                    .await
            }
        }
    }
    pub async fn accounts(&self) -> Result<Value> {
        let vault = self.vault.clone();
        self.db.call(move |conn| {
            let mut values = Vec::new();
            for info in oauth::list(conn)? {
                let account = oauth::load(conn,&vault,info.id)?;
                values.push(json!({"id":info.id,"email":oauth::token_email(&account.credentials.access_token).unwrap_or_else(||"Email unavailable".into()),"state":info.state}));
            }
            Ok(json!(values))
        }).await
    }
    pub async fn disable(&self, id: i64) -> Result<Value> {
        self.db
            .call(move |conn| Ok(json!({"disabled":oauth::disable(conn,id)?})))
            .await
    }
    pub async fn begin_login(&self) -> Result<oauth::DeviceCode> {
        oauth::begin_device(&upstream::http_client()?, &self.issuer).await
    }
    pub async fn finish_login(
        &self,
        code: oauth::DeviceCode,
        expected: Option<i64>,
    ) -> Result<Value> {
        let expected = if let Some(id) = expected {
            let vault = self.vault.clone();
            Some(
                self.db
                    .call(move |conn| Ok(oauth::load(conn, &vault, id)?.info.account_id))
                    .await?,
            )
        } else {
            None
        };
        let (account, credentials, expires) =
            oauth::complete_device(&upstream::http_client()?, &self.issuer, code).await?;
        if expected.is_some_and(|expected| expected != account) {
            return Err("Reauthorization returned a different account".into());
        }
        let vault = self.vault.clone();
        let id = self
            .db
            .call(move |conn| oauth::save(conn, &vault, &account, &credentials, expires))
            .await?;
        Ok(json!({"account_id":id,"state":"active"}))
    }
    pub async fn stop(&self) -> Result<()> {
        if let Some(stop) = self
            .shutdown
            .lock()
            .map_err(|_| "Local shutdown lock failed")?
            .take()
        {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.lock().await.take() {
            task.await??;
        }
        self.db.drain().await?;
        self._lock
            .lock()
            .map_err(|_| "Local runtime lock failed")?
            .take();
        Ok(())
    }
}
impl Drop for Local {
    fn drop(&mut self) {
        if let Ok(shutdown) = self.shutdown.get_mut() {
            if let Some(stop) = shutdown.take() {
                let _ = stop.send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock;
    use super::*;
    #[tokio::test]
    async fn embedded_api_routes_without_ssh_and_preserves_accounts_and_usage() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mock = Arc::new(mock::Mock::default());
        mock.accounts.lock().unwrap().insert(
            "upstream-account".into(),
            mock::Account::new("upstream-access", "upstream-refresh", &["gpt-test"]),
        );
        let app = mock::router(mock);
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = upstream::Config::new(url.clone(), url, true).unwrap();
        let local = Local::open_with(
            root.path(),
            "127.0.0.1:0".parse().unwrap(),
            true,
            config.clone(),
        )
        .await
        .unwrap();
        let vault = local.vault.clone();
        local
            .db
            .call(move |conn| {
                oauth::save(
                    conn,
                    &vault,
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
        let token = local
            .request(ControlRequest::TokenCreate {
                name: "fixture".into(),
                expires_days: Some(1),
            })
            .await
            .unwrap();
        let secret = token["secret"].as_str().unwrap();
        let api = format!("http://{}", local.address);
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{api}/v1/models"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert!(local.request(ControlRequest::Models).await.unwrap()["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "gpt-test"));
        let response = client
            .post(format!("{api}/v1/responses"))
            .bearer_auth(secret)
            .json(&json!({"model":"gpt-test","input":"Synthetic local test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("EXETROUTER_SMOKE_OK"));
        let usage = local
            .request(ControlRequest::Usage {
                period: "day".into(),
                by: None,
                timezone: Some("UTC".into()),
            })
            .await
            .unwrap();
        assert_eq!(usage["rows"][0]["requests"], 1);
        assert_eq!(local.accounts().await.unwrap().as_array().unwrap().len(), 1);
        let attached = Local::open_with(
            root.path(),
            "127.0.0.1:0".parse().unwrap(),
            true,
            config.clone(),
        )
        .await
        .unwrap();
        assert_eq!(attached.address, local.address);
        attached.stop().await.unwrap();
        assert_eq!(
            client
                .get(format!("{api}/healthz"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        drop(attached);
        local.stop().await.unwrap();
        drop(local);
        let reopened = Local::open_with(root.path(), "127.0.0.1:0".parse().unwrap(), true, config)
            .await
            .unwrap();
        assert_eq!(
            reopened.accounts().await.unwrap().as_array().unwrap().len(),
            1
        );
        assert_eq!(
            reopened
                .request(ControlRequest::TokenList)
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            reopened
                .db
                .call(|conn| Ok(conn.query_row(
                    "SELECT COUNT(*) FROM ssh_identities",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .await
                .unwrap()
                == 0
        );
        reopened.stop().await.unwrap();
        upstream_task.abort();
    }
    #[tokio::test]
    async fn standalone_refuses_exposed_or_shared_state() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Local::open(root.path(), "0.0.0.0:8787".parse().unwrap(), false)
                .await
                .is_err()
        );
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            Local::open(root.path(), "127.0.0.1:0".parse().unwrap(), false)
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "../tests/support/mock.rs"]
mod mock;
