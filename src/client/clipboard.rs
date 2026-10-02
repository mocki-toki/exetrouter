use crate::Result;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Clone)]
pub(super) struct Clipboard {
    program: PathBuf,
    args: &'static [&'static str],
}

impl Clipboard {
    #[cfg(test)]
    pub(super) fn fixture() -> Self {
        Self {
            program: PathBuf::from("/bin/false"),
            args: &[],
        }
    }
    pub(super) fn detect() -> Result<Self> {
        let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
            &[("pbcopy", &[])]
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            &[("wl-copy", &[])]
        } else if std::env::var_os("DISPLAY").is_some() {
            &[
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ]
        } else {
            return Err(
                "No desktop clipboard available. Use a desktop terminal to access the clipboard."
                    .into(),
            );
        };
        for (name, args) in candidates {
            for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                let program = directory.join(name);
                if program
                    .metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                {
                    return Ok(Self { program, args });
                }
            }
        }
        Err("Clipboard helper unavailable. macOS needs pbcopy; Wayland needs wl-copy; X11 needs xclip or xsel.".into())
    }

    pub(super) async fn copy(&self, secret: &str) -> Result<()> {
        let mut child = Command::new(&self.program)
            .args(self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Could not start clipboard helper")?;
        let copying = async {
            let mut input = child.stdin.take().ok_or("missing clipboard input")?;
            input.write_all(secret.as_bytes()).await?;
            drop(input);
            if !child.wait().await?.success() {
                return Err("Clipboard helper failed".into());
            }
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(5), copying)
            .await
            .map_err(|_| "Clipboard copy timed out")?
    }
}
