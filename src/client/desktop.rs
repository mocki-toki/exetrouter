//! Private stdio transport for a trusted local desktop client, not an HTTP API.
//! Secrets stay here and are copied by the same clipboard implementation as TUI.
use super::{clipboard::Clipboard, config, privacy, ssh_request, CommandLine, Session};
use crate::{ControlRequest, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::IsTerminal, os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    input: Action,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Profile {},
    TokenCreate {
        name: String,
        days: i64,
    },
    TokenRename {
        id: String,
        name: String,
    },
    TokenRotate {
        id: String,
        confirmed: bool,
    },
    TokenRevoke {
        id: String,
        confirmed: bool,
    },
    CopyRetry {},
    CopyDiscard {},
    Configure {
        connection: ConnectionInput,
        confirmed: bool,
    },
    LoginStart {
        account: Option<i64>,
    },
    LoginStatus {},
    LoginCancel {},
    AccountDisable {
        account: i64,
        confirmed: bool,
    },
    UpdateInstall {
        confirmed: bool,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionInput {
    mode: config::Mode,
    host: String,
    port: u16,
    ssh_user: String,
    identity: String,
    api_url: Option<String>,
    state_dir: String,
    listen: std::net::SocketAddr,
}

struct Desktop {
    session: Session,
    path: PathBuf,
    secret: Option<(String, Clipboard)>,
    login: Option<tokio::task::JoinHandle<Result<Value>>>,
}

fn confirmed(value: bool) -> Result<()> {
    if !value {
        return Err("confirmation required".into());
    }
    Ok(())
}

fn reply(status: &str) -> Value {
    json!({"status":status,"copied":false,"retryCopy":false,"login":null,"profile":null,"update":null})
}

impl Desktop {
    async fn request(&mut self, request: ControlRequest) -> Result<Value> {
        self.session.connection.validate()?;
        if self.session.connection.mode == config::Mode::Standalone {
            self.local().await?;
        }
        ssh_request(&self.session, request).await
    }
    async fn local(&mut self) -> Result<std::sync::Arc<crate::local::Local>> {
        let c = &mut self.session.connection;
        if c.mode != config::Mode::Standalone {
            return Err("standalone required".into());
        }
        if c.local.is_none() {
            c.validate()?;
            c.local = Some(
                crate::local::Local::open(std::path::Path::new(&c.state_dir), c.listen, true)
                    .await?,
            );
        }
        Ok(c.local.as_ref().expect("opened local runtime").clone())
    }

    async fn profile(&mut self) -> Result<Value> {
        let accounts = if self.session.connection.mode == config::Mode::Standalone {
            self.local().await?.accounts().await?
        } else {
            json!([])
        };
        let c = &self.session.connection;
        let standalone = c.mode == config::Mode::Standalone;
        let address = c.local.as_ref().map_or(c.listen, |local| local.address);
        let fields = if standalone {
            json!([{"label":"API endpoint","value":format!("http://{address}/v1")},{"label":"Private state","value":c.state_dir}])
        } else {
            json!([{"label":"HTTP API URL","value":c.api_url.as_deref().unwrap_or("Not configured")},{"label":"SSH host","value":c.host},{"label":"Port","value":c.port.to_string()},{"label":"SSH username","value":c.ssh_user},{"label":"Private key","value":c.identity}])
        };
        Ok(
            json!({"mode":if standalone {"Standalone"} else {"Remote Server"},
            "destination":if standalone {address.to_string()} else {format!("{}@{}:{}",c.ssh_user,c.host,c.port)},
            "fields":fields,"accounts":accounts,"clientVersion":env!("CARGO_PKG_VERSION"),
            "connection":{"mode":c.mode,"host":c.host,"port":c.port,"ssh_user":c.ssh_user,"identity":c.identity,"api_url":c.api_url,"state_dir":c.state_dir,"listen":c.listen.to_string()}}),
        )
    }

    async fn copy(&mut self) -> Value {
        let copied = if let Some((secret, clipboard)) = &self.secret {
            clipboard.copy(secret).await.is_ok()
        } else {
            false
        };
        if copied {
            self.secret = None;
        }
        let mut value = reply(if copied { "copied" } else { "copy_failed" });
        value["copied"] = copied.into();
        value["retryCopy"] = self.secret.is_some().into();
        value
    }

    async fn issue(&mut self, request: ControlRequest) -> Result<Value> {
        if self.secret.is_some() {
            return Err("copy or discard the previous secret first".into());
        }
        // Detect before submitting; never retry issuance to recover a clipboard failure.
        let clipboard = match Clipboard::detect() {
            Ok(clipboard) => clipboard,
            Err(_) => return Ok(reply("clipboard_unavailable")),
        };
        let mut result = self.request(request).await?;
        let secret = result
            .as_object_mut()
            .and_then(|o| o.remove("secret"))
            .and_then(|s| s.as_str().map(str::to_owned))
            .ok_or("missing secret")?;
        self.secret = Some((secret, clipboard));
        Ok(self.copy().await)
    }

    fn cancel_login(&mut self) {
        if let Some(job) = self.login.take() {
            job.abort();
        }
    }

    async fn action(&mut self, action: Action) -> Result<Value> {
        match action {
            Action::Profile {} => {
                let mut value = reply("ready");
                value["profile"] = self.profile().await?;
                Ok(value)
            }
            Action::TokenCreate { name, days } => {
                privacy::validate_token_label(name.trim())?;
                if !(1..=365).contains(&days) {
                    return Err("invalid expiry".into());
                }
                self.issue(ControlRequest::TokenCreate {
                    name: name.trim().into(),
                    expires_days: Some(days),
                })
                .await
            }
            Action::TokenRotate { id, confirmed: yes } => {
                confirmed(yes)?;
                self.issue(ControlRequest::TokenRotate { id }).await
            }
            Action::TokenRename { id, name } => {
                privacy::validate_token_label(name.trim())?;
                self.request(ControlRequest::TokenRename {
                    id,
                    name: name.trim().into(),
                })
                .await?;
                Ok(reply("saved"))
            }
            Action::TokenRevoke { id, confirmed: yes } => {
                confirmed(yes)?;
                self.request(ControlRequest::TokenRevoke { id }).await?;
                Ok(reply("revoked"))
            }
            Action::CopyRetry {} => Ok(self.copy().await),
            Action::CopyDiscard {} => {
                self.secret = None;
                Ok(reply("discarded"))
            }
            Action::Configure {
                connection: input,
                confirmed: yes,
            } => {
                confirmed(yes)?;
                if self.secret.is_some() {
                    return Err("copy or discard the previous secret first".into());
                }
                let old = self.session.connection.clone();
                let mut next = old.clone();
                next.local = None;
                next.mode = input.mode;
                next.host = input.host;
                next.port = input.port;
                next.ssh_user = input.ssh_user;
                next.identity = if input.identity.is_empty() {
                    String::new()
                } else {
                    config::expand(input.identity)?
                };
                next.api_url = input
                    .api_url
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| config::normalize_api_url(&s))
                    .transpose()?;
                next.state_dir = config::expand(input.state_dir)?;
                next.listen = input.listen;
                next.validate()?;
                if next.mode == config::Mode::Remote {
                    let meta = std::fs::metadata(&next.identity)?;
                    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                        return Err("private key must be owner-only".into());
                    }
                }
                self.cancel_login();
                if let Some(local) = &old.local {
                    local.stop().await?;
                }
                self.session.connection = next;
                let saved = async {
                    if self.session.connection.mode == config::Mode::Standalone {
                        self.local().await?;
                    }
                    config::save(&self.path, &self.session.connection)
                }
                .await;
                if let Err(error) = saved {
                    if let Some(local) = &self.session.connection.local {
                        let _ = local.stop().await;
                    }
                    self.session.connection = old;
                    self.session.connection.local = None;
                    if self.session.connection.mode == config::Mode::Standalone {
                        self.local().await?;
                    }
                    return Err(error);
                }
                let mut value = reply("saved");
                value["profile"] = self.profile().await?;
                Ok(value)
            }
            Action::LoginStart { account } => {
                if self.login.is_some() {
                    return Err("sign-in already pending".into());
                }
                let local = self.local().await?;
                if let Some(id) = account {
                    if !local
                        .accounts()
                        .await?
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|row| row["id"].as_i64() == Some(id)))
                    {
                        return Err("account not found".into());
                    }
                }
                let code = local.begin_login().await?;
                let mut value = reply("pending");
                value["login"] = json!({"url":code.verification_url,"code":code.user_code});
                self.login = Some(tokio::spawn(async move {
                    local.finish_login(code, account).await
                }));
                Ok(value)
            }
            Action::LoginStatus {} => {
                if self.login.as_ref().is_some_and(|job| job.is_finished()) {
                    self.login.take().expect("finished login").await??;
                    Ok(reply("complete"))
                } else {
                    Ok(reply(if self.login.is_some() {
                        "pending"
                    } else {
                        "idle"
                    }))
                }
            }
            Action::LoginCancel {} => {
                self.cancel_login();
                Ok(reply("cancelled"))
            }
            Action::AccountDisable {
                account,
                confirmed: yes,
            } => {
                confirmed(yes)?;
                self.local().await?.disable(account).await?;
                Ok(reply("disabled"))
            }
            Action::UpdateInstall { confirmed: yes } => {
                confirmed(yes)?;
                if std::env::var_os("EXR_NO_UPDATE_CHECK").is_some() {
                    return Err("update checks disabled".into());
                }
                let report = crate::update::check().await?;
                crate::update::install("exr", &report).await?;
                let mut value = reply("updated");
                value["update"] = json!({"current":if report.available {&report.latest} else {&report.current},"latest":report.latest,"available":false});
                Ok(value)
            }
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.cancel_login();
    }
}

pub(super) async fn run(connection: config::Connection, path: PathBuf) -> Result<()> {
    if std::io::stdin().is_terminal() || std::io::stdout().is_terminal() {
        return Err("desktop bridge requires private stdio pipes".into());
    }
    let mut desktop = Desktop {
        session: Session {
            connection,
            json: false,
            command: CommandLine::Desktop,
        },
        path,
        secret: None,
        login: None,
    };
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let result = async {
        loop {
            // Bound each JSON line before parsing, including malformed input without a newline.
            let mut line = Vec::new();
            let n = (&mut input).take(65_537).read_until(b'\n', &mut line).await?;
            if n == 0 { return Ok(()); }
            if n > 65_536 || !line.ends_with(b"\n") { return Err("invalid desktop frame".into()); }
            let request: Request = serde_json::from_slice(&line).map_err(|_| "invalid desktop request")?;
            let value = match desktop.action(request.input).await {
                Ok(value) => json!({"id":request.id,"value":value}),
                // Never expose arbitrary native/upstream errors, credentials or request data.
                Err(_) => json!({"id":request.id,"error":"Operation failed. The outcome may be unknown; refresh metadata before repeating a mutation."}),
            };
            let mut bytes = serde_json::to_vec(&value)?;
            if bytes.len() > 1_048_576 { return Err("desktop response too large".into()); }
            bytes.push(b'\n'); output.write_all(&bytes).await?; output.flush().await?;
        }
    }.await;
    desktop.cancel_login();
    if let Some(local) = &desktop.session.connection.local {
        local.stop().await?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_protocol_is_closed_and_destructive_actions_require_confirmation() {
        for input in [
            r#"{"action":"limits"}"#,
            r#"{"action":"token_rotate","id":"tok_test"}"#,
            r#"{"action":"profile","secret":"never"}"#,
        ] {
            assert!(serde_json::from_str::<Action>(input).is_err());
        }
        assert!(confirmed(false).is_err());
        assert!(confirmed(true).is_ok());
        assert!(!reply("copy_failed").to_string().contains("secret"));
    }
    #[tokio::test]
    async fn unconfirmed_mutations_do_not_open_state_or_submit_requests() {
        let mut desktop = Desktop {
            session: Session {
                connection: config::Connection::default(),
                json: false,
                command: CommandLine::Desktop,
            },
            path: PathBuf::new(),
            secret: None,
            login: None,
        };
        for action in [
            Action::TokenRotate {
                id: "tok_test".into(),
                confirmed: false,
            },
            Action::TokenRevoke {
                id: "tok_test".into(),
                confirmed: false,
            },
            Action::AccountDisable {
                account: 1,
                confirmed: false,
            },
            Action::UpdateInstall { confirmed: false },
        ] {
            assert!(desktop.action(action).await.is_err());
            assert!(desktop.session.connection.local.is_none());
        }
    }
    #[tokio::test]
    async fn failed_clipboard_copy_retains_only_one_secret_for_retry_without_issuance() {
        let mut desktop = Desktop {
            session: Session {
                connection: config::Connection::default(),
                json: false,
                command: CommandLine::Desktop,
            },
            path: PathBuf::new(),
            secret: Some(("synthetic-secret".into(), Clipboard::fixture())),
            login: None,
        };
        let value = desktop.copy().await;
        assert_eq!(value["retryCopy"], true);
        assert!(!value.to_string().contains("synthetic-secret"));
        assert!(desktop
            .issue(ControlRequest::TokenRotate {
                id: "tok_test".into()
            })
            .await
            .is_err());
        desktop.action(Action::CopyDiscard {}).await.unwrap();
        assert!(desktop.secret.is_none());
    }
}
