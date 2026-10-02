use super::Args;
#[derive(
    Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum, Default,
)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    #[default]
    Remote,
    Standalone,
}
use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Connection {
    pub mode: Mode,
    pub state_dir: String,
    pub listen: std::net::SocketAddr,
    #[serde(skip)]
    pub local: Option<std::sync::Arc<crate::local::Local>>,
    pub host: String,
    pub port: u16,
    pub ssh_user: String,
    pub identity: String,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            mode: Mode::Remote,
            state_dir: default_state_dir(),
            listen: "127.0.0.1:8787".parse().expect("valid loopback address"),
            local: None,
            host: "localhost".into(),
            port: 2222,
            ssh_user: "routercli".into(),
            identity: String::new(),
        }
    }
}
impl Connection {
    pub fn validate(&self) -> Result<()> {
        if self.mode == Mode::Standalone {
            if !Path::new(&self.state_dir).is_absolute() || !self.listen.ip().is_loopback() {
                return Err(
                    "Standalone requires an absolute state path and loopback listen address".into(),
                );
            }
            return Ok(());
        }
        if self.identity.is_empty() {
            return Err("SSH identity is not configured. Run: exr configure --identity ~/.ssh/exetrouter_ed25519".into());
        }
        if self.port == 0 || !valid_host(&self.host) || !valid_user(&self.ssh_user) {
            return Err("invalid SSH host, port or username".into());
        }
        if self.identity.chars().any(char::is_control) {
            return Err("invalid identity path".into());
        }
        Ok(())
    }
}
fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_:[]".contains(&b))
}
fn valid_user(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub(super) fn path(args: &Args) -> Result<PathBuf> {
    if let Some(path) = &args.config {
        return Ok(path.clone());
    }
    if let Some(path) = std::env::var_os("EXR_CONFIG") {
        return Ok(path.into());
    }
    let root = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME unavailable; use --config")?)
            .join(".config"),
    };
    Ok(root.join("exr/config.json"))
}
pub(super) fn expand(value: String) -> Result<String> {
    let value = if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME unavailable")?).join(rest)
    } else {
        PathBuf::from(value)
    };
    Ok(if value.is_absolute() {
        value
    } else {
        std::env::current_dir()?.join(value)
    }
    .to_string_lossy()
    .into_owned())
}
pub(super) fn load(args: &Args) -> Result<Connection> {
    let config_path = if args.config.is_none()
        && std::env::var_os("EXR_CONFIG").is_none()
        && std::env::var_os("XDG_CONFIG_HOME").is_none()
        && std::env::var_os("HOME").is_none()
    {
        None
    } else {
        Some(path(args)?)
    };
    let mut result: Connection = match config_path.map(fs::read).transpose() {
        Ok(Some(data)) => {
            serde_json::from_slice(&data).map_err(|_| "invalid connection configuration")?
        }
        Ok(None) => Connection::default(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Connection::default(),
        Err(e) => return Err(e.into()),
    };
    if let Some(value) = &args.host {
        result.host.clone_from(value);
    }
    if let Some(value) = args.port {
        result.port = value;
    }
    if let Some(value) = &args.ssh_user {
        result.ssh_user.clone_from(value);
    }
    if let Some(value) = &args.identity {
        result.identity = expand(value.clone())?;
    }
    if let Some(mode) = args.mode {
        result.mode = mode;
    }
    if args.standalone {
        result.mode = Mode::Standalone;
    }
    if let Some(value) = &args.state_dir {
        result.state_dir = expand(value.clone())?;
    }
    if let Some(value) = args.listen {
        result.listen = value;
    }
    Ok(result)
}
fn prompt(label: &str, current: &str) -> Result<String> {
    print!("{label} [{current}]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Err("configuration cancelled".into());
    }
    let answer = answer.trim();
    Ok(if answer.is_empty() {
        current.into()
    } else {
        answer.into()
    })
}
pub(super) fn confirm(label: &str) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "confirmation requires a terminal; use --yes for an authorized scripted revocation"
                .into(),
        );
    }
    if prompt(&format!("{label} Type yes to confirm"), "no")? != "yes" {
        return Err("cancelled".into());
    }
    Ok(())
}
pub(super) fn configure(args: &Args, result: &mut Connection) -> Result<()> {
    if result.mode == Mode::Standalone {
        result.validate()?;
        let destination = path(args)?;
        save(&destination, result)?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({"configured":true,"mode":"standalone","path":destination})
            );
        } else {
            println!("Standalone settings saved to {}\nOpen dashboard: exr\nRun API in foreground: exr serve",destination.display());
        }
        return Ok(());
    }
    if args.identity.is_none()
        && args.host.is_none()
        && args.port.is_none()
        && args.ssh_user.is_none()
    {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("use configure --identity PATH for non-interactive setup".into());
        }
        result.host = prompt("SSH host", &result.host)?;
        result.port = prompt("SSH port", &result.port.to_string())?.parse()?;
        result.ssh_user = prompt("SSH username", &result.ssh_user)?;
        result.identity = expand(prompt("Private key", &result.identity)?)?;
    }
    result.validate()?;
    let meta = fs::metadata(&result.identity)?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        return Err(
            "private key must be a regular file with owner-only permissions (chmod 600)".into(),
        );
    }
    let destination = path(args)?;
    save(&destination, result)?;
    if args.json {
        println!(
            "{}",
            serde_json::json!({"configured":true,"path":destination})
        );
    } else {
        println!(
            "Connection saved to {}\nNext: exr doctor\nOpen dashboard: exr",
            destination.display()
        );
    }
    Ok(())
}
pub(super) fn save(destination: &Path, value: &Connection) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    if fs::symlink_metadata(destination).is_ok_and(|m| !m.is_file()) {
        return Err("configuration must be a regular file, not a symlink".into());
    }
    let temp = parent.join(format!(
        ".exr-{}-{}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let outcome = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, destination)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(temp);
    }
    outcome
}

fn default_state_dir() -> String {
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    root.join("exr").to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saves_private_config_and_rejects_symlink_and_option_injection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut value = Connection {
            identity: "/tmp/key".into(),
            ..Default::default()
        };
        save(&path, &value).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_slice::<Connection>(&fs::read(&path).unwrap())
                .unwrap()
                .identity,
            "/tmp/key"
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(save(&link, &value).is_err());
        value.host = "-oProxyCommand=bad".into();
        assert!(value.validate().is_err());
    }
}
