//! Explicit GitHub release checks and updates; never sends router configuration or credentials.
use crate::Result;
use serde::{Deserialize, Serialize};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

pub const REPO: &str = "mocki-toki/exetrouter";
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Report {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub release_url: String,
}
fn version(value: &str) -> Result<[u64; 3]> {
    let parts = value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err("release version must be vMAJOR.MINOR.PATCH".into());
    }
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid release version".into());
        }
        result[index] = part.parse()?;
    }
    Ok(result)
}
pub async fn check() -> Result<Report> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("ExetRouter-update-check")
        .build()?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if matches!(response.status().as_u16(), 403 | 429) {
        let response = client
            .head(format!("https://github.com/{REPO}/releases/latest"))
            .send()
            .await?
            .error_for_status()?;
        let tag = release_tag_from_url(response.url().as_str())?;
        return report(
            &serde_json::json!({"tag_name": tag}),
            env!("CARGO_PKG_VERSION"),
        );
    }
    let response = response.error_for_status()?;
    if response.content_length().is_some_and(|n| n > 1024 * 1024) {
        return Err("release metadata exceeds size limit".into());
    }
    let body = response.bytes().await?;
    if body.len() > 1024 * 1024 {
        return Err("release metadata exceeds size limit".into());
    }
    let release: serde_json::Value = serde_json::from_slice(&body)?;
    report(&release, env!("CARGO_PKG_VERSION"))
}
fn release_tag_from_url(url: &str) -> Result<&str> {
    let tag = url
        .strip_prefix("https://github.com/mocki-toki/exetrouter/releases/tag/")
        .ok_or("unexpected latest release redirect")?;
    version(tag)?;
    Ok(tag)
}
fn report(release: &serde_json::Value, current: &str) -> Result<Report> {
    if release["draft"] == true || release["prerelease"] == true {
        return Err("release is not a published stable release".into());
    }
    let tag = release["tag_name"]
        .as_str()
        .ok_or("release has no version")?;
    let available = version(tag)? > version(current)?;
    Ok(Report {
        current: current.into(),
        latest: tag.into(),
        available,
        release_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
    })
}
pub async fn install(binary: &str, release: &Report) -> Result<()> {
    if !release.available {
        return Ok(());
    }
    version(&release.latest)?;
    if !matches!(binary, "exr" | "exrd") {
        return Err("unknown update role".into());
    }
    if Path::new("/.dockerenv").exists() {
        return Err("container binaries are read-only; run exrd update on the host".into());
    }
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or("cannot locate installed binary")?;
    if directory.file_name().is_none_or(|name| name != "bin")
        || executable.file_name().is_none_or(|name| name != binary)
    {
        return Err("this is not a standard bin installation; reinstall using scripts/install.sh or use your deployment's update command".into());
    }
    let prefix = directory
        .parent()
        .ok_or("cannot locate installation prefix")?;
    use std::os::unix::fs::OpenOptionsExt;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join(format!(".{binary}-update.lock")))?;
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another update is running for this installation".into());
    }
    let installed = tokio::process::Command::new(&executable)
        .arg("--version")
        .output()
        .await?;
    if !installed.status.success() {
        return Err("cannot verify the currently installed binary".into());
    }
    let output = String::from_utf8(installed.stdout)?;
    let installed_version = output
        .split_whitespace()
        .last()
        .ok_or("installed version unavailable")?;
    if version(installed_version)? > version(&release.latest)? {
        return Err("a newer version is already installed; refusing to downgrade".into());
    }
    let source = source_install(prefix, binary)?;
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-s")
        .arg("--")
        .args([
            "--role",
            if binary == "exr" { "client" } else { "server" },
            "--prefix",
        ])
        .arg(prefix)
        .args(["--version", &release.latest]);
    if source {
        command.arg("--from-source");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn()?;
    let group = ProcessGroup(child.id().ok_or("update process has no ID")?);
    child
        .stdin
        .take()
        .ok_or("update stdin unavailable")?
        .write_all(include_bytes!("../scripts/install.sh"))
        .await?;
    let status = child.wait().await?;
    drop(group);
    if !status.success() {
        return Err("update failed; existing config and state were preserved. Retry scripts/install.sh in a terminal to inspect the installation error".into());
    }
    Ok(())
}
fn source_install(prefix: &Path, binary: &str) -> Result<bool> {
    let receipt = prefix.join(".crates2.json");
    if !receipt.is_file() {
        if prefix.join(".crates.toml").is_file() {
            return Err("legacy Cargo receipt found; update with cargo install --locked --force --git https://github.com/mocki-toki/exetrouter".into());
        }
        return Ok(false);
    }
    let bytes = std::fs::read(receipt)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Cargo receipt exceeds size limit".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if let Some(installs) = value["installs"].as_object() {
        for (package, installed) in installs {
            if installed["bins"]
                .as_array()
                .is_some_and(|bins| bins.iter().any(|b| b.as_str() == Some(binary)))
            {
                if package.split_whitespace().next() != Some("exetrouter") {
                    return Err("binary is owned by a different Cargo package; update through that package manager".into());
                }
                return Ok(true);
            }
        }
    }
    Ok(false)
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGTERM);
        }
    }
}
pub async fn command(binary: &str, check_only: bool, json: bool) -> Result<()> {
    let mut report = check().await?;
    let applied = !check_only && report.available;
    if applied {
        if !json {
            println!("Updating {binary} to {}…", report.latest);
        }
        install(binary, &report).await?;
        report.current = report.latest.trim_start_matches('v').into();
        report.available = false;
    }
    if json {
        let mut value = serde_json::to_value(&report)?;
        value["updated"] = applied.into();
        println!("{}", serde_json::to_string(&value)?);
    } else if applied {
        println!(
            "Updated {binary} to {}. Restart the dashboard or service to use it.",
            report.latest
        );
    } else if !report.available {
        println!("{binary} {} is up to date.", report.current);
    } else {
        println!(
            "Update available: {} → {}\nRun {binary} update to install.",
            report.current, report.latest
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_accepts_only_this_repository_stable_release() {
        assert_eq!(
            release_tag_from_url("https://github.com/mocki-toki/exetrouter/releases/tag/v0.2.2")
                .unwrap(),
            "v0.2.2"
        );
        for url in [
            "https://github.com/other/repo/releases/tag/v0.2.2",
            "http://github.com/mocki-toki/exetrouter/releases/tag/v0.2.2",
            "https://github.com/mocki-toki/exetrouter/releases/tag/v0.2.2-beta",
            "https://github.com/mocki-toki/exetrouter/releases/tag/v0.2.2?x=1",
        ] {
            assert!(release_tag_from_url(url).is_err());
        }
    }
    #[test]
    fn cargo_receipts_preserve_source_updates_and_do_not_claim_unrelated_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".crates2.json");
        std::fs::write(&p, r#"{"installs":{"exetrouter 0.1.0 (git+https://github.com/mocki-toki/exetrouter)":{"bins":["exr"]}}}"#).unwrap();
        assert!(source_install(dir.path(), "exr").unwrap());
        assert!(!source_install(dir.path(), "exrd").unwrap());
        std::fs::write(p, r#"{"installs":{"other 1.0.0":{"bins":["exr"]}}}"#).unwrap();
        assert!(source_install(dir.path(), "exr").is_err());
    }
    #[test]
    fn release_versions_are_numeric_and_untrusted_tags_are_rejected() {
        assert!(version("v0.10.0").unwrap() > version("0.9.9").unwrap());
        for bad in ["v1.0.0;id", "../../main", "v1.0.0-beta", "1.0", "v1.0.0\n"] {
            assert!(version(bad).is_err());
        }
        let r = serde_json::json!({"tag_name":"v0.2.0","draft":false,"prerelease":false});
        assert!(report(&r, "0.1.0").unwrap().available);
        assert!(!report(&r, "0.3.0").unwrap().available);
        assert!(report(
            &serde_json::json!({"tag_name":"v0.2.0","prerelease":true}),
            "0.1.0"
        )
        .is_err());
    }
}
