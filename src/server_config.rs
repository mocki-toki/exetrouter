//! Persistent server paths. Command-line values override saved configuration.
use crate::Result;
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub db: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub oauth_key: Option<PathBuf>,
    pub control_socket: Option<PathBuf>,
    pub gateway_uid: Option<u32>,
    pub listen: Option<std::net::SocketAddr>,
}

pub fn path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit.or_else(|| std::env::var_os("EXRD_CONFIG").map(Into::into)) {
        return Ok(path);
    }
    let system = PathBuf::from("/etc/exetrouter/config.json");
    if system.exists() {
        return Ok(system);
    }
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    Ok(root.map_or(system, |root| root.join("exrd/config.json")))
}
pub fn load(path: &std::path::Path, required: bool) -> Result<Config> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => {
            return Ok(Config::default())
        }
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file() || metadata.permissions().mode() & 0o022 != 0 {
        return Err(
            "server configuration must be a regular file, not writable by group or others".into(),
        );
    }
    let bytes = fs::read(path)?;
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|_| "invalid server configuration")?;
    for value in [
        &config.db,
        &config.key,
        &config.oauth_key,
        &config.control_socket,
    ]
    .into_iter()
    .flatten()
    {
        if !value.is_absolute() {
            return Err("saved server paths must be absolute".into());
        }
    }
    Ok(config)
}
