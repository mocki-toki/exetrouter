mod clipboard;
mod config;
mod quota_forecast;
mod render;
mod tui;
use crate::{ControlRequest, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal},
    os::unix::fs::PermissionsExt,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

#[derive(Parser)]
#[command(
    name = "exr",
    version,
    about = "Use ExetRouter locally or over SSH. Run without a command to open the dashboard."
)]
struct Args {
    /// Use local accounts and an embedded API, without SSH or a remote server.
    #[arg(long, global = true)]
    standalone: bool,
    /// Saved operation mode.
    #[arg(long, global = true, value_enum)]
    mode: Option<config::Mode>,
    /// Private standalone data directory.
    #[arg(long, global = true)]
    state_dir: Option<String>,
    /// Standalone API listen address (loopback only).
    #[arg(long, global = true)]
    listen: Option<std::net::SocketAddr>,
    /// SSH server (saved by configure; default: localhost).
    #[arg(long, global = true)]
    host: Option<String>,
    /// SSH port (default: 2222).
    #[arg(long, global = true)]
    port: Option<u16>,
    /// SSH username (default: routercli).
    #[arg(long, global = true)]
    ssh_user: Option<String>,
    /// Private key path; configure once to save it.
    #[arg(long, global = true)]
    identity: Option<String>,
    /// Configuration file (default: $XDG_CONFIG_HOME/exr/config.json).
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,
    /// Structured output for scripts; disabled for secret issuance.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<CommandLine>,
}

#[derive(Subcommand)]
enum CommandLine {
    /// Check for or install the latest published release.
    Update {
        #[arg(long)]
        check: bool,
    },
    /// Save standalone or remote connection settings once.
    Configure,
    /// Run the standalone API until Ctrl-C, without opening the dashboard.
    Serve,
    /// Manage local OAuth accounts in standalone mode.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Open the interactive dashboard (also the default command).
    Tui,
    /// Show upstream subscription windows for each account.
    #[command(alias = "quota")]
    Limits {
        #[command(subcommand)]
        command: Option<LimitsCommand>,
    },
    /// Show usage totals for today, the last 24 hours, this week or this month.
    Usage {
        #[arg(long, default_value = "day", value_parser = ["day", "24h", "week", "month"])]
        period: String,
        #[arg(long, value_parser = ["user", "model"])]
        by: Option<String>,
    },
    /// List models, or export client metadata with --json.
    Models {
        #[arg(long, value_enum)]
        format: Option<ModelFormat>,
    },
    /// Inspect stored configuration without making upstream requests.
    Doctor,
    /// Manage your API tokens (defaults to list).
    #[command(visible_alias = "tokens")]
    Token {
        #[command(subcommand)]
        command: Option<TokenCommand>,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// Set reversible routing preferences for your user (remote or standalone).
    Set {
        id: i64,
        #[arg(long,action=clap::ArgAction::Set)]
        enabled: Option<bool>,
        #[arg(long)]
        priority: Option<i32>,
    },
    /// Reactivate an account for your user without signing in again.
    Enable { id: i64 },
    /// List local accounts by email and state, without credentials.
    List,
    /// Sign in through a browser device-code flow.
    Add,
    /// Reauthorize the same account through a browser.
    Reauth { id: i64 },
    /// Reversibly deactivate routing for your user; retain OAuth state.
    Disable {
        id: i64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum LimitsCommand {
    /// Consume one reset credit after a live check and mandatory confirmation.
    Reset { account: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum ModelFormat {
    OpenaiJson,
    CodexJson,
    OpencodeJsonc,
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Issue a token and copy its secret to the clipboard (interactive terminal only).
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(i64).range(1..=365))]
        expires_days: i64,
    },
    /// List metadata for your tokens.
    List,
    /// Inspect one token without revealing its secret.
    Show { id: String },
    /// Replace a token; the old secret stops working immediately.
    Rotate { id: String },
    /// Disable a token, with confirmation unless --yes is supplied.
    Revoke {
        id: String,
        #[arg(long)]
        yes: bool,
    },
}

pub async fn main() {
    if let Err(err) = run().await {
        eprintln!("exr: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut options = Args::parse();
    if let Some(CommandLine::Update { check }) = options.command {
        return crate::update::command("exr", check, options.json).await;
    }
    if let Some(CommandLine::Token { command }) = &mut options.command {
        if command.is_none() {
            *command = Some(TokenCommand::List);
        }
    }
    let mut connection = config::load(&options)?;
    if matches!(options.command, Some(CommandLine::Configure)) {
        config::configure(&options, &mut connection)?;
        return Ok(());
    }
    let dashboard = matches!(options.command, None | Some(CommandLine::Tui));
    if dashboard && connection.validate().is_err() {
        if options.json {
            return Err("--json requires a command; run exr interactively to configure".into());
        }
        tui::setup(&mut connection, &config::path(&options)?).await?;
    }
    connection.validate()?;
    let config_path = if dashboard {
        config::path(&options)?
    } else {
        std::path::PathBuf::new()
    };
    if connection.mode == config::Mode::Standalone {
        connection.local = Some(
            crate::local::Local::open(
                std::path::Path::new(&connection.state_dir),
                connection.listen,
                dashboard || matches!(options.command, Some(CommandLine::Serve)),
            )
            .await?,
        );
    }
    let args = Session {
        connection,
        json: options.json,
        command: options.command.unwrap_or(CommandLine::Tui),
    };
    let local = args.connection.local.clone();
    let result = run_session(&args, &config_path).await;
    if let Some(local) = local {
        local.stop().await?;
    }
    result
}
async fn run_session(args: &Session, config_path: &std::path::Path) -> Result<()> {
    if matches!(args.command, CommandLine::Serve) {
        let local = args
            .connection
            .local
            .as_ref()
            .ok_or("serve requires standalone mode; run exr configure --standalone")?;
        println!(
            "Standalone API running at http://{}/v1\nKeep this process running. Ctrl-C stops it.",
            local.address
        );
        if local.attached {
            return Err("Standalone API is already running; open exr to manage it".into());
        }
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result?,_=terminate.recv()=>{}}
        return Ok(());
    }
    if let CommandLine::Account { command } = &args.command {
        let preference = match command {
            AccountCommand::Set {
                id,
                enabled,
                priority,
            } => Some((*id, *enabled, *priority)),
            AccountCommand::Enable { id } => Some((*id, Some(true), None)),
            AccountCommand::Disable { id, yes } => {
                if !yes {
                    config::confirm("Deactivate this account for your user?")?;
                }
                Some((*id, Some(false), None))
            }
            _ => None,
        };
        if let Some((id, enabled, priority)) = preference {
            let value = ssh_request(
                args,
                ControlRequest::AccountSet {
                    account: id,
                    enabled,
                    priority,
                },
            )
            .await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!(
                    "Account preferences\n\nEnabled   {}\nPriority  {}\nLocked    {}",
                    value["enabled"], value["priority"], value["locked"]
                );
            }
            return Ok(());
        }
    }
    if let CommandLine::Account { command } = &args.command {
        let local = args.connection.local.as_ref().ok_or(
            "Account management belongs to standalone mode. Server operators use exrd admin oauth.",
        )?;
        let value = match command {
            AccountCommand::Set { .. }
            | AccountCommand::Enable { .. }
            | AccountCommand::Disable { .. } => unreachable!(),
            AccountCommand::List => local.accounts().await?,
            AccountCommand::Add | AccountCommand::Reauth { .. } => {
                if args.json || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                    return Err(
                        "OAuth login requires an interactive terminal; --json is disabled".into(),
                    );
                }
                let code = local.begin_login().await?;
                println!(
                    "Open {}\nEnter this one-time code: {}",
                    code.verification_url, code.user_code
                );
                local
                    .finish_login(
                        code,
                        match command {
                            AccountCommand::Reauth { id } => Some(*id),
                            _ => None,
                        },
                    )
                    .await?
            }
        };
        if args.json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            println!("{}", render::accounts(&value));
        }
        return Ok(());
    }
    if matches!(args.command, CommandLine::Tui) {
        if args.json {
            return Err(
                "--json requires a command such as doctor, tokens, models, usage or limits".into(),
            );
        }
        return tui::run(args, config_path).await;
    }
    if let CommandLine::Limits { command } = &args.command {
        if let Some(LimitsCommand::Reset { account }) = command {
            return reset_credit(args, account).await;
        }
        let value = ssh_request(args, ControlRequest::Limits).await?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &value.get("quota_accounts").cloned().unwrap_or_else(|| {
                        if value["quota"].is_object() {
                            serde_json::json!([{"label":"Account 1","quota":value["quota"]}])
                        } else {
                            serde_json::json!([])
                        }
                    })
                )?
            );
        } else {
            println!("{}", render::limits(&value));
        }
        return Ok(());
    }
    if let CommandLine::Models { format } = args.command {
        if !args.json && format.is_some() {
            return Err(
                "model exports require --json (for example: exr models --json --format codex-json)"
                    .into(),
            );
        }
    }
    if matches!(args.command, CommandLine::Doctor) {
        return doctor(args).await;
    }
    let reveals_secret = matches!(
        &args.command,
        CommandLine::Token {
            command: Some(TokenCommand::Create { .. } | TokenCommand::Rotate { .. })
        }
    );
    if reveals_secret && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        return Err("token creation and rotation require an interactive terminal".into());
    }
    if reveals_secret && args.json {
        return Err("--json is disabled when issuing a secret".into());
    }
    // Check support before issuing or rotating; never expose a fallback secret.
    let clipboard = if reveals_secret {
        Some(clipboard::Clipboard::detect()?)
    } else {
        None
    };
    let request = match &args.command {
        CommandLine::Usage { period, by } => ControlRequest::Usage {
            period: period.clone(),
            by: by.clone(),
            timezone: Some(render::system_timezone()),
        },
        CommandLine::Models { .. } => ControlRequest::Models,
        CommandLine::Update { .. }
        | CommandLine::Doctor
        | CommandLine::Limits { .. }
        | CommandLine::Configure
        | CommandLine::Serve
        | CommandLine::Account { .. }
        | CommandLine::Tui => {
            unreachable!("handled above")
        }
        CommandLine::Token { command } => match command.as_ref().expect("normalized token command")
        {
            TokenCommand::Create { name, expires_days } => ControlRequest::TokenCreate {
                name: name.clone(),
                expires_days: Some(*expires_days),
            },
            TokenCommand::List => ControlRequest::TokenList,
            TokenCommand::Show { id } => ControlRequest::TokenShow { id: id.clone() },
            TokenCommand::Rotate { id } => ControlRequest::TokenRotate { id: id.clone() },
            TokenCommand::Revoke { id, yes } => {
                if !yes {
                    config::confirm(&format!("Revoke token {id}?"))?;
                }
                ControlRequest::TokenRevoke { id: id.clone() }
            }
        },
    };
    let mut result = ssh_request(args, request).await?;
    if let CommandLine::Models { format } = &args.command {
        let format = format.unwrap_or(ModelFormat::OpenaiJson);
        if !matches!(format, ModelFormat::OpenaiJson) {
            let models: Vec<crate::catalog::Model> = serde_json::from_value(result["data"].clone())
                .map_err(|_| "invalid model metadata reply")?;
            result = match format {
                ModelFormat::CodexJson => crate::catalog::codex(&models)?,
                ModelFormat::OpencodeJsonc => crate::catalog::opencode(&models)?,
                ModelFormat::OpenaiJson => unreachable!(),
            };
        }
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if reveals_secret {
        let token = result.get("token").ok_or("missing token")?;
        let id = token
            .get("id")
            .and_then(Value::as_str)
            .ok_or("missing ID")?;
        let secret = result
            .get("secret")
            .and_then(Value::as_str)
            .ok_or("missing secret")?;
        clipboard.as_ref().ok_or("missing clipboard")?.copy(secret).await
            .map_err(|_| format!("Token {id} was issued, but clipboard copy failed. Rotate that token to try again."))?;
        println!("Token: {id}\nSecret copied to clipboard. Paste it into your service or password manager.");
    } else {
        println!(
            "{}",
            render::terminal(&render::response(&request_kind(&args.command), &result))
        );
    }
    Ok(())
}

async fn reset_credit(args: &Session, account: &str) -> Result<()> {
    if args.json || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "Reset credits require an interactive terminal and confirmation; --json is disabled"
                .into(),
        );
    }
    let limits = ssh_request(args, ControlRequest::Limits).await?;
    let rows = limits["quota_accounts"]
        .as_array()
        .ok_or("Account list unavailable")?;
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["label"]
                .as_str()
                .is_some_and(|email| email.eq_ignore_ascii_case(account))
                || row["id"]
                    .as_i64()
                    .is_some_and(|id| Some(id) == account.parse::<i64>().ok())
        })
        .collect();
    if matches.len() != 1 {
        return Err("Select one account by email (or numeric ID if the same email belongs to several accounts)".into());
    }
    let id = matches[0]["id"].as_i64().ok_or("Account ID unavailable")?;
    let preview = ssh_request(args, ControlRequest::ResetPrepare { account: id }).await?;
    println!(
        "{}",
        render::terminal(&render::reset_confirmation(&preview))
    );
    config::confirm("Use one reset credit?")?;
    let confirmation = preview["confirmation"]
        .as_str()
        .ok_or("Reset confirmation unavailable")?
        .into();
    let result = ssh_request(args, ControlRequest::ResetConfirm { confirmation }).await?;
    println!("{}", render::reset_outcome(&result));
    Ok(())
}

async fn ssh_request(args: &Session, request: ControlRequest) -> Result<Value> {
    if let Some(local) = &args.connection.local {
        return local.request(request).await;
    }
    let destination = format!("{}@{}", args.connection.ssh_user, args.connection.host);
    let mut ssh = Command::new("ssh");
    ssh.args([
        "-T",
        "-p",
        &args.connection.port.to_string(),
        "-o",
        "BatchMode=yes",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "IdentitiesOnly=yes",
        "-i",
        &args.connection.identity,
    ]);
    ssh.arg(destination)
        .arg("exrd-gateway")
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = ssh.spawn()?;
    let (status, output) = exchange(
        &mut child,
        serde_json::to_vec(&request)?,
        Duration::from_secs(45),
    )
    .await?;
    if !status.success() {
        return Err(format!("SSH connection failed ({status}). Check the saved connection, verified known_hosts entry and loaded SSH key in ssh-agent.").into());
    }
    let reply: Value = serde_json::from_slice(&output).map_err(|_| "invalid control reply")?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("control request failed")
            .to_owned()
            .into());
    }
    Ok(reply.get("result").ok_or("missing result")?.clone())
}

async fn doctor(args: &Session) -> Result<()> {
    if let Some(local) = &args.connection.local {
        let value = local.request(ControlRequest::Doctor).await?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            println!(
                "Standalone API: http://{}/v1\n{}",
                local.address,
                render::response("doctor", &value)
            );
        }
        if value["configuration_status"] != "configured" {
            return Err(
                "Standalone setup requires an OAuth account; open Settings or run exr account add"
                    .into(),
            );
        }
        return Ok(());
    }
    let identity = match fs::metadata(&args.connection.identity) {
        Ok(metadata) if !metadata.is_file() => "not_a_file",
        Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => "insecure_permissions",
        Ok(_) => "ok",
        Err(err) if err.kind() == io::ErrorKind::NotFound => "missing",
        Err(_) => "unavailable",
    };
    let (ssh, server) = if identity != "ok" {
        ("not_checked", None)
    } else {
        match ssh_request(args, ControlRequest::Doctor).await {
            Ok(value) => match serde_json::from_value::<crate::doctor::ServerReport>(value) {
                Ok(report) if report.schema_version == 1 => ("ok", Some(report)),
                _ => ("invalid_reply", None),
            },
            Err(_) => ("unavailable", None),
        }
    };
    let configured = identity == "ok"
        && ssh == "ok"
        && server
            .as_ref()
            .is_some_and(|report| report.configuration_status == "configured");
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version":1,
                "local":{"ssh_identity":identity,"management_ssh":ssh},
                "server":server,
                "public_api":{"tls":"not_checked","websocket":"not_checked"}
            }))?
        );
    } else {
        println!("SSH identity       {identity}\nManagement SSH     {ssh}");
        if identity != "ok" {
            println!("Next: select an accessible private key file readable only by its owner.");
        }
        if let Some(report) = &server {
            println!("Server config      {}\nOAuth              {} · {} active · {} reauth required\nCatalog            {} · {} models\nGenerations        {}/{} for this user · {} global limit\nWebSocket          {}/{} for this user · {} global limit",
                render::routing(&serde_json::to_value(report)?),report.oauth.status,report.oauth.active_accounts,report.oauth.reauth_required_accounts,
                report.catalog.status,report.catalog.models,
                report.limits.generations.active_for_user,report.limits.generations.per_user_limit,report.limits.generations.global_limit,
                report.limits.websockets.active_for_user,report.limits.websockets.per_user_limit,report.limits.websockets.global_limit);
            if let Some(pool) = &report.pool {
                println!("Account pool       {} configured · {} cooling down · {} without current catalog\nPool quota         {} fresh · {} stale · {} unknown",pool.configured_accounts,pool.cooldown_accounts,pool.catalog_unavailable_accounts,pool.fresh_quota_accounts,pool.stale_quota_accounts,pool.unknown_quota_accounts);
                if pool.backoff_accounts > 0 {
                    println!(
                        "Upstream backoff   {} accounts · earliest retry {}",
                        pool.backoff_accounts,
                        render::date(&serde_json::json!(pool.earliest_backoff_until))
                    );
                }
            }
            if report.catalog.status != "current" {
                println!("Next: run models to refresh the catalog; configure or reauthorize OAuth locally if required.");
            }
        }
        if let Some(report) = &server {
            println!(
                "{}",
                render::terminal(&render::limits(&serde_json::to_value(report)?))
            );
        }
        println!("Public TLS/WSS     not_checked\nUpstream network   not_checked\nInference          not_checked");
    }
    if !configured {
        return Err("doctor found configuration issues; see the report".into());
    }
    Ok(())
}

async fn exchange(
    child: &mut Child,
    request: Vec<u8>,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    let result = tokio::time::timeout(timeout, async {
        let mut stdin = child.stdin.take().ok_or("SSH stdin unavailable")?;
        stdin.write_all(&request).await?;
        drop(stdin);
        let stdout = child.stdout.take().ok_or("SSH stdout unavailable")?;
        let mut reply = Vec::new();
        stdout.take(1_048_577).read_to_end(&mut reply).await?;
        if reply.len() > 1_048_576 {
            return Err("control reply too large".into());
        }
        let status = child.wait().await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((status, reply))
    })
    .await;
    let result = result.unwrap_or_else(|_| {
        Err("SSH request timed out; inspect tokens or limits before repeating a mutation".into())
    });
    if result.is_err() {
        // Reap SSH on every failed exchange, including oversized replies.
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

struct Session {
    connection: config::Connection,
    json: bool,
    command: CommandLine,
}

fn request_kind(command: &CommandLine) -> String {
    match command {
        CommandLine::Models { .. } => "models",
        CommandLine::Usage { .. } => "usage",
        CommandLine::Token { command } => match command.as_ref().expect("normalized token command")
        {
            TokenCommand::List => "tokens",
            TokenCommand::Show { .. } => "token",
            TokenCommand::Revoke { .. } => "revoke",
            _ => "secret",
        },
        _ => "doctor",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_reaps_hung_child() {
        let mut child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let err = exchange(&mut child, b"{}".to_vec(), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn oversized_reply_reaps_child() {
        let mut child = Command::new("sh")
            .args(["-c", "exec yes x"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let err = exchange(&mut child, b"{}".to_vec(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "control reply too large");
        assert!(child.try_wait().unwrap().is_some());
    }
}
