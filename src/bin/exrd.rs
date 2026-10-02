use clap::{parser::ValueSource, CommandFactory, FromArgMatches, Parser, Subcommand};
use exetrouter::{
    authorized_keys, create_user, init, list_ssh_keys,
    oauth::{self, Vault},
    register_ssh_key, revoke_ssh_key,
    server::{self, GatewayMessage, ServeConfig},
    store::Database,
    upstream::{self, Upstream},
    ControlRequest, Result,
};
use rand::{rngs::OsRng, RngCore};
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs,
    io::{IsTerminal, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(
    version,
    about = "Run and administer an ExetRouter server",
    after_help = "Configuration: --config PATH, EXRD_CONFIG, /etc/exetrouter/config.json, then ~/.config/exrd/config.json. Saved paths remove repeated flags. Start with exrd --help or exrd admin --help."
)]
struct Args {
    /// Server configuration containing paths, listen address and gateway UID.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Print structured reports for scripts.
    #[arg(long, global = true)]
    json: bool,
    /// Private SQLite state (overrides saved configuration).
    #[arg(long, default_value = "exetrouter.sqlite")]
    db: PathBuf,
    /// Key used to verify router API tokens.
    #[arg(long, default_value = "exetrouter.key")]
    key: PathBuf,
    /// Separate key encrypting upstream OAuth credentials.
    #[arg(long, default_value = "exetrouter.oauth.key")]
    oauth_key: PathBuf,
    #[arg(long, default_value = upstream::BACKEND)]
    upstream_url: String,
    #[arg(long, default_value = oauth::ISSUER)]
    oauth_issuer: String,
    #[arg(long)]
    allow_mock_upstream: bool,
    /// Private Unix socket for the restricted SSH gateway.
    #[arg(long, default_value = "exetrouter.sock")]
    control_socket: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check for or install the latest published release; Docker uses the host launcher.
    Update {
        #[arg(long)]
        check: bool,
    },
    /// Initialize a new database and two independent private keys; refuses overwrites.
    Init,
    /// Manage local users, SSH public keys, OAuth accounts and backups.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Restricted SSH bridge; normally invoked only by a forced command.
    Gateway {
        #[arg(long)]
        identity: String,
    },
    /// Run the model API and authenticated Unix management socket.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: SocketAddr,
        #[arg(long)]
        gateway_uid: Option<u32>,
        /// Permit a wildcard listener inside an isolated Docker network only.
        #[arg(long)]
        allow_container_listen: bool,
        #[arg(long)]
        allow_same_uid_for_dev: bool,
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u16).range(1..=64))]
        generations_per_user: u16,
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u16).range(1..=64))]
        websockets_per_user: u16,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Snapshot, verify or restore private state; backups contain secrets.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    /// Add, reauthorize, list or disable upstream ChatGPT OAuth accounts.
    Oauth {
        #[command(subcommand)]
        command: OauthCommand,
    },
    /// Add a router user; tokens and SSH keys belong to this user.
    UserCreate { name: String },
    /// List router users and their local IDs.
    UserList,
    /// Register a user's Ed25519 public key for restricted management access.
    SshKeyAdd {
        #[arg(long)]
        user_id: i64,
        #[arg(long)]
        public_key_file: PathBuf,
    },
    /// List public-key bindings without private keys.
    SshKeyList,
    /// Immediately deny management access for one registered key.
    SshKeyRevoke { id: String },
    /// Export forced-command authorized_keys for the dedicated sshd.
    SshAuthorizedKeys {
        #[arg(long)]
        gateway_bin: PathBuf,
    },
}

#[derive(Subcommand, Clone)]
enum BackupCommand {
    /// Create a consistent SQLite snapshot with both state keys.
    Create {
        #[arg(long)]
        to: PathBuf,
    },
    /// Check file hashes, SQLite integrity and OAuth decryption offline.
    Verify {
        #[arg(long)]
        from: PathBuf,
    },
    /// Restore into a new private state directory without overwriting files.
    Restore {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
}

#[derive(Subcommand)]
enum OauthCommand {
    /// Create the OAuth encryption key for an existing pre-OAuth installation.
    InitKey,
    /// Set reversible routing availability/priority for everyone and lock client changes.
    Policy {
        id: i64,
        #[arg(long, action=clap::ArgAction::Set)]
        enabled: Option<bool>,
        #[arg(long)]
        priority: Option<i32>,
        #[arg(long, action=clap::ArgAction::Set)]
        locked: Option<bool>,
    },
    /// Sign in through the browser device-code flow and save encrypted credentials.
    Add {
        #[arg(long, required = true)]
        device: bool,
    },
    /// Sign in again for the same account; refuses a different account.
    Reauth {
        id: i64,
        #[arg(long, required = true)]
        device: bool,
    },
    /// List account state and expiry without credentials.
    List,
    /// Reversibly deactivate routing for everyone and lock client changes.
    Disable { id: i64 },
    /// Reactivate routing for everyone; existing lock remains in effect.
    Enable { id: i64 },
}

#[tokio::main]
async fn main() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};
    // Do not let environment filters enable third-party HTTP/TLS payload traces.
    let targets = tracing_subscriber::filter::Targets::new()
        .with_target("exetrouter", tracing::Level::INFO)
        .with_target("exrd", tracing::Level::INFO);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::stderr)
                .with_filter(targets),
        )
        .init();
    if let Err(err) = run().await {
        eprintln!("exrd: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;
    if let Command::Update { check } = args.command {
        return exetrouter::update::command("exrd", check, args.json).await;
    }
    let saved_path = exetrouter::server_config::path(args.config.clone())?;
    let saved = exetrouter::server_config::load(
        &saved_path,
        args.config.is_some() || std::env::var_os("EXRD_CONFIG").is_some(),
    )?;
    for (name, target, value) in [
        ("db", &mut args.db, saved.db),
        ("key", &mut args.key, saved.key),
        ("oauth_key", &mut args.oauth_key, saved.oauth_key),
        (
            "control_socket",
            &mut args.control_socket,
            saved.control_socket,
        ),
    ] {
        if matches.value_source(name) != Some(ValueSource::CommandLine) {
            if let Some(value) = value {
                *target = value;
            }
        }
    }
    if let Command::Serve {
        gateway_uid,
        listen,
        ..
    } = &mut args.command
    {
        if gateway_uid.is_none() {
            *gateway_uid = saved.gateway_uid;
        }
        if matches
            .subcommand_matches("serve")
            .is_some_and(|m| m.value_source("listen") != Some(ValueSource::CommandLine))
        {
            if let Some(value) = saved.listen {
                *listen = value;
            }
        }
    }
    if matches!(&args.command, Command::Init) {
        if args.key.exists() {
            return Err("key file already exists; refusing to overwrite".into());
        }
        if args.db.exists() {
            return Err("database already exists; refusing to reinitialize".into());
        }
        if args.oauth_key.exists() {
            return Err("OAuth key already exists; refusing to overwrite".into());
        }
        initialize(&args.db, &args.key, &args.oauth_key)?;
        print_report("State initialized", &json!({"database":args.db}), args.json)?;
        return Ok(());
    }
    if let Command::Admin {
        command: AdminCommand::Backup { command },
    } = &args.command
    {
        let operation = match command {
            BackupCommand::Create { .. } => "created",
            BackupCommand::Verify { .. } => "verified",
            BackupCommand::Restore { .. } => "restored",
        };
        let command = command.clone();
        let paths = exetrouter::backup::Paths {
            db: args.db,
            key: args.key,
            oauth_key: args.oauth_key,
        };
        let report = tokio::task::spawn_blocking(move || match command {
            BackupCommand::Create { to } => exetrouter::backup::create(&paths, &to),
            BackupCommand::Verify { from } => exetrouter::backup::verify(&from),
            BackupCommand::Restore { from, to } => exetrouter::backup::restore(&from, &to),
        })
        .await??;
        print_report(
            "Backup complete",
            &json!({"operation":operation,"snapshot":report}),
            args.json,
        )?;
        return Ok(());
    }
    if let Command::Gateway { identity } = &args.command {
        if std::env::var("SSH_ORIGINAL_COMMAND").as_deref() != Ok("exrd-gateway") {
            return Err("gateway requires the expected SSH command".into());
        }
        let mut input = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::io::stdin().take(32_769).read_to_end(&mut input),
        )
        .await
        .map_err(|_| "gateway input timed out")??;
        if input.len() > 32_768 {
            return Err("control request too large".into());
        }
        let request: ControlRequest =
            serde_json::from_slice(&input).map_err(|_| "invalid control request")?;
        let frame = serde_json::to_vec(&GatewayMessage {
            identity: identity.clone(),
            request,
        })?;
        let reply = tokio::time::timeout(Duration::from_secs(40), async {
            let mut stream = tokio::net::UnixStream::connect(&args.control_socket).await?;
            stream.write_all(&frame).await?;
            stream.shutdown().await?;
            let mut reply = Vec::new();
            stream.take(1_048_577).read_to_end(&mut reply).await?;
            if reply.len() > 1_048_576 {
                return Err("control reply too large".into());
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(reply)
        })
        .await
        .map_err(|_| {
            "gateway request timed out; inspect tokens or limits before repeating a mutation"
        })??;
        std::io::stdout().write_all(&reply)?;
        return Ok(());
    }
    check_private_file(&args.key)?;
    check_private_file(&args.db)?;
    let key = fs::read(&args.key)?;
    if key.len() != 32 {
        return Err("key file must contain exactly 32 bytes".into());
    }
    if let Command::Serve {
        listen,
        gateway_uid,
        allow_container_listen,
        allow_same_uid_for_dev,
        generations_per_user,
        websockets_per_user,
    } = args.command
    {
        let gateway_uid = gateway_uid
            .ok_or("Set gateway_uid in the server config, or supply serve --gateway-uid UID")?;
        if !listen.ip().is_loopback() && !(allow_container_listen && listen.ip().is_unspecified()) {
            return Err("exrd only permits a loopback listener".into());
        }
        let own_uid = unsafe { libc::geteuid() };
        if own_uid == 0 {
            return Err("exrd must not run as root".into());
        }
        if gateway_uid == own_uid && !allow_same_uid_for_dev {
            return Err("gateway UID must differ from service UID (or use --allow-same-uid-for-dev locally)".into());
        }
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let shutdown = async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
        };
        let db = Database::open(args.db).await?;
        let config = upstream::Config::new(
            args.upstream_url,
            args.oauth_issuer,
            args.allow_mock_upstream,
        )?;
        let configured = if args.oauth_key.exists() {
            let oauth_key = read_oauth_key(&args.oauth_key)?;
            if oauth_key.as_slice() == key {
                return Err(
                    "OAuth encryption key must differ from the client-token HMAC key".into(),
                );
            }
            Some(Arc::new(Upstream::new(db.clone(), oauth_key, config)?))
        } else {
            let count = db
                .call(|conn| {
                    Ok(
                        conn.query_row("SELECT COUNT(*) FROM oauth_accounts", [], |r| {
                            r.get::<_, i64>(0)
                        })?,
                    )
                })
                .await?;
            if count > 0 {
                return Err("OAuth key missing".into());
            }
            None
        };
        return server::serve(
            db,
            key,
            ServeConfig {
                gateway_uid,
                listen,
                socket: args.control_socket,
                upstream: configured,
                generations_per_user: usize::from(generations_per_user),
                websockets_per_user: usize::from(websockets_per_user),
            },
            shutdown,
        )
        .await;
    }
    if let Command::Admin {
        command: AdminCommand::Oauth { command },
    } = args.command
    {
        let config = upstream::Config::new(
            args.upstream_url,
            args.oauth_issuer,
            args.allow_mock_upstream,
        )?;
        let db = Database::open(args.db).await?;
        if matches!(command, OauthCommand::InitKey) {
            let mut key = [0; 32];
            OsRng.fill_bytes(&mut key);
            let mut file = NewPrivateFile::create(&args.oauth_key, &key)?;
            file.committed = true;
            print_report(
                "OAuth key initialized",
                &json!({"initialized":true}),
                args.json,
            )?;
            return Ok(());
        }
        match command {
            OauthCommand::List => {
                let vault = Vault::new(read_oauth_key(&args.oauth_key)?);
                print_report(
                    "OAuth accounts",
                    &db.call(move |conn| {
                        let mut rows = oauth::list_with_email(conn, &vault)?;
                        for row in &mut rows {
                            if let Some(id) = row["id"].as_i64() {
                                row["routing_policy"] = serde_json::to_value(
                                    exetrouter::account_preferences::effective(conn, None, id)?,
                                )?;
                            }
                        }
                        Ok(rows)
                    })
                    .await?,
                    args.json,
                )?;
            }
            OauthCommand::Policy {
                id,
                enabled,
                priority,
                locked,
            } => print_report(
                "Account routing policy",
                &db.call(move |conn| {
                    Ok(serde_json::to_value(
                        exetrouter::account_preferences::set_policy(
                            conn, id, enabled, priority, locked,
                        )?,
                    )?)
                })
                .await?,
                args.json,
            )?,
            OauthCommand::Disable { id } => print_report(
                "OAuth account deactivated",
                &db.call(move |conn| {
                    Ok(serde_json::to_value(
                        exetrouter::account_preferences::set_policy(
                            conn,
                            id,
                            Some(false),
                            None,
                            Some(true),
                        )?,
                    )?)
                })
                .await?,
                args.json,
            )?,
            OauthCommand::Enable { id } => print_report(
                "OAuth account activated",
                &db.call(move |conn| {
                    Ok(serde_json::to_value(
                        exetrouter::account_preferences::set_policy(
                            conn,
                            id,
                            Some(true),
                            None,
                            None,
                        )?,
                    )?)
                })
                .await?,
                args.json,
            )?,
            OauthCommand::Add { .. } | OauthCommand::Reauth { .. } => {
                if args.json {
                    return Err("--json is disabled during interactive OAuth login".into());
                }
                if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                    return Err("OAuth login requires an interactive local terminal".into());
                }
                let oauth_key = read_oauth_key(&args.oauth_key)?;
                if oauth_key.as_slice() == key {
                    return Err(
                        "OAuth encryption key must differ from the client-token HMAC key".into(),
                    );
                }
                let vault = Vault::new(oauth_key);
                let expected = match command {
                    OauthCommand::Reauth { id, .. } => {
                        let vault = vault.clone();
                        Some(
                            db.call(move |conn| Ok(oauth::load(conn, &vault, id)?.info.account_id))
                                .await?,
                        )
                    }
                    _ => None,
                };
                let client = upstream::http_client()?;
                let code = oauth::begin_device(&client, &config.issuer).await?;
                println!(
                    "Open {}\nEnter this one-time code: {}",
                    code.verification_url, code.user_code
                );
                let (account, credentials, expiry) =
                    oauth::complete_device(&client, &config.issuer, code).await?;
                if expected
                    .as_ref()
                    .is_some_and(|expected| expected != &account)
                {
                    return Err("reauthorization returned a different account".into());
                }
                let id = db
                    .call(move |conn| oauth::save(conn, &vault, &account, &credentials, expiry))
                    .await?;
                print_report(
                    "OAuth login complete",
                    &json!({"account_id":id,"state":"active"}),
                    args.json,
                )?;
            }
            OauthCommand::InitKey => unreachable!(),
        }
        return Ok(());
    }
    let conn = Connection::open(&args.db)?;
    init(&conn)?;
    match args.command {
        Command::Update { .. } => unreachable!("updates run before opening server state"),
        Command::Init | Command::Gateway { .. } | Command::Serve { .. } => unreachable!(),
        Command::Admin { command } => match command {
            AdminCommand::Backup { .. } => unreachable!(),
            AdminCommand::Oauth { .. } => unreachable!(),
            AdminCommand::UserCreate { name } => {
                print_report(
                    "User created",
                    &json!({"id":create_user(&conn,&name)?,"name":name}),
                    args.json,
                )?;
            }
            AdminCommand::UserList => {
                let mut stmt = conn.prepare("SELECT id,name,created_at FROM users ORDER BY id")?;
                let rows=stmt.query_map([],|r| Ok(json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
                print_report("Users", &rows, args.json)?;
            }
            AdminCommand::SshKeyAdd {
                user_id,
                public_key_file,
            } => {
                let input = fs::read_to_string(public_key_file)?;
                print_report(
                    "SSH key registered",
                    &register_ssh_key(&conn, user_id, &input)?,
                    args.json,
                )?;
            }
            AdminCommand::SshKeyList => {
                print_report("SSH keys", &list_ssh_keys(&conn)?, args.json)?;
            }
            AdminCommand::SshKeyRevoke { id } => {
                print_report(
                    "SSH key revocation",
                    &json!({"revoked":revoke_ssh_key(&conn,&id)?}),
                    args.json,
                )?;
            }
            AdminCommand::SshAuthorizedKeys { gateway_bin } => {
                let export = authorized_keys(
                    &conn,
                    gateway_bin.to_str().ok_or("invalid gateway binary path")?,
                    args.control_socket
                        .to_str()
                        .ok_or("invalid control socket path")?,
                )?;
                if args.json {
                    print_report("Authorized keys", &json!({"authorized_keys":export}), true)?;
                } else {
                    print!("{export}");
                }
            }
        },
    }
    Ok(())
}

struct NewPrivateFile {
    file: fs::File,
    path: PathBuf,
    committed: bool,
}

impl NewPrivateFile {
    fn create(path: &PathBuf, contents: &[u8]) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let mut owned = Self {
            file,
            path: path.clone(),
            committed: false,
        };
        owned.file.write_all(contents)?;
        owned.file.sync_all()?;
        Ok(owned)
    }
}

impl Drop for NewPrivateFile {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if self.committed {
            return;
        }
        if let (Ok(created), Ok(current)) = (self.file.metadata(), fs::symlink_metadata(&self.path))
        {
            if created.dev() == current.dev()
                && created.ino() == current.ino()
                && fs::remove_file(&self.path).is_err()
            {
                tracing::error!(event = "init_cleanup_failed");
            }
        }
    }
}

fn initialize(db: &PathBuf, key_path: &PathBuf, oauth_path: &PathBuf) -> Result<()> {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    let mut key_file = NewPrivateFile::create(key_path, &key)?;
    OsRng.fill_bytes(&mut key);
    let mut oauth_file = NewPrivateFile::create(oauth_path, &key)?;
    let mut db_file = NewPrivateFile::create(db, &[])?;
    let conn = Connection::open(db)?;
    init(&conn)?;
    key_file.committed = true;
    db_file.committed = true;
    oauth_file.committed = true;
    Ok(())
}

fn read_oauth_key(path: &PathBuf) -> Result<[u8; 32]> {
    check_private_file(path)?;
    fs::read(path)?
        .try_into()
        .map_err(|_| "OAuth key must contain exactly 32 bytes".into())
}

fn check_private_file(path: &PathBuf) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()).into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{} must not be readable or writable by group/others",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn print_report(title: &str, value: &impl serde::Serialize, machine: bool) -> Result<()> {
    let value = serde_json::to_value(value)?;
    if machine {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{title}\n");
        print_value(&value, 0);
    }
    Ok(())
}
fn print_value(value: &serde_json::Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        serde_json::Value::Array(rows) => {
            if rows.is_empty() {
                println!("{indent}No entries.");
            }
            for row in rows {
                print_value(row, depth);
                println!();
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let label = key.replace('_', " ");
                if value.is_object() || value.is_array() {
                    println!("{indent}{label}:");
                    print_value(value, depth + 1);
                } else {
                    let text = match value {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => "—".into(),
                        _ => value.to_string(),
                    };
                    let text: String = text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();
                    println!("{indent}{label:18} {text}");
                }
            }
        }
        _ => println!("{indent}{value}"),
    }
}
