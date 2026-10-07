//! Isolated profiles shared by mock and explicit real-backend client checks.
use exetrouter::Result;
use serde_json::{json, Value};
use std::{fs, path::Path, process::Output, time::Duration};

pub fn reviewed_version(client: &str) -> String {
    let manifest: Value = serde_json::from_str(include_str!("../../docs/protocol-sources.json"))
        .expect("reviewed protocol source manifest");
    manifest["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["id"] == client)
        .and_then(|project| project["version"].as_str())
        .expect("client must have a reviewed source version")
        .to_owned()
}

pub async fn version(binary: &str, expected: &str) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(binary)
            .kill_on_drop(true)
            .arg("--version")
            .output(),
    )
    .await
    .map_err(|_| "client version check timed out")??;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .any(|part| part.trim_start_matches('v') == expected)
    {
        return Err("client version does not match the reviewed matrix".into());
    }
    Ok(())
}

pub struct Run<'a> {
    pub binary: &'a str,
    pub client: &'a str,
    pub url: &'a str,
    pub model: &'a str,
    pub bearer: &'a str,
    pub websocket: bool,
    pub directory: &'a Path,
    pub prompt: &'a str,
}
/// Test-only compaction policy. Real model limits remain in the catalog.
pub struct Continuation {
    pub token_limit: u64,
    pub resume: bool,
}

fn codex_sandbox_mode(requested: bool, isolated: bool, url: &str, model: &str) -> Result<&'static str> {
    if !requested {
        return Ok("read-only");
    }
    let endpoint = reqwest::Url::parse(url)
        .map_err(|_| "outer sandbox mode requires a synthetic loopback fixture")?;
    let loopback = endpoint.host_str().is_some_and(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    });
    if !isolated
        || model != "gpt-test"
        || endpoint.scheme() != "http"
        || !loopback
        || endpoint.port().is_none_or(|port| port == 0)
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.path() != "/"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err("outer sandbox mode requires an isolated synthetic loopback fixture".into());
    }
    // The controller's non-root, networkless, capability-free, read-only Docker
    // container is the isolation boundary. Do not ask bwrap to nest namespaces.
    Ok("danger-full-access")
}

fn outer_sandbox_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/.dockerenv").is_file() && unsafe { libc::geteuid() } != 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

impl Run<'_> {
    pub async fn execute(&self) -> Result<Output> {
        self.execute_with(None).await
    }

    // Used by the live_upstream integration target; this helper is also
    // compiled separately by the short responses matrix.
    #[allow(dead_code)]
    pub async fn continued(&self, policy: &Continuation) -> Result<Output> {
        self.execute_with(Some(policy)).await
    }

    async fn execute_with(&self, policy: Option<&Continuation>) -> Result<Output> {
        let sandbox = if self.client == "codex" {
            codex_sandbox_mode(
                std::env::var("EXETROUTER_NATIVE_OUTER_SANDBOX").as_deref() == Ok("1"),
                outer_sandbox_present(),
                self.url,
                self.model,
            )?
        } else {
            "read-only"
        };
        let work = self.directory.join("workspace");
        let config = self.directory.join("client-config");
        fs::create_dir_all(&work)?;
        fs::create_dir_all(&config)?;
        let catalog_url = format!(
            "{}/v1/models/{}",
            self.url,
            if self.client == "codex" {
                "codex"
            } else if self.client == "opencode-v1" {
                "opencode-v1"
            } else {
                "opencode-v2"
            }
        );
        let catalog: Value = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()?
            .get(catalog_url)
            .bearer_auth(self.bearer)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut command = tokio::process::Command::new(self.binary);
        command
            .current_dir(&work)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").ok_or("client PATH missing")?,
            )
            .env("HOME", self.directory)
            .env("LANG", "en_US.UTF-8")
            .env("EXETROUTER_TOKEN", self.bearer);
        match self.client {
            "codex" => {
                let catalog_path = config.join("models.json");
                fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
                if !Path::new(self.binary)
                    .parent()
                    .is_some_and(|directory| directory.join("codex-code-mode-host").is_file())
                {
                    return Err("Codex test installation lacks its code mode host; fetch the full distribution".into());
                }
                let compaction = policy
                    .map(|policy| {
                        format!("model_auto_compact_token_limit = {}\nmodel_auto_compact_token_limit_scope = \"total\"\n", policy.token_limit)
                    })
                    .unwrap_or_default();
                // Codex currently gates remote V2 by provider display name.
                // Keep the custom provider ID and router bearer/base URL.
                let name = "OpenAI";
                fs::write(config.join("config.toml"), format!("model = {}\nmodel_provider = \"exetrouter\"\nmodel_catalog_json = {}\napproval_policy = \"never\"\n{compaction}[model_providers.exetrouter]\nname = {}\nbase_url = {}\nenv_key = \"EXETROUTER_TOKEN\"\nwire_api = \"responses\"\nsupports_websockets = {}\n",json!(self.model),json!(catalog_path),json!(name), json!(format!("{}/v1",self.url)),self.websocket))?;
                command.env("CODEX_HOME", &config).args([
                    "exec",
                    "--ignore-rules",
                    "--skip-git-repo-check",
                    "--color",
                    "never",
                    "--json",
                    "--sandbox",
                    sandbox,
                ]);
                if policy.is_none() {
                    command.arg("--ephemeral");
                } else if policy.is_some_and(|policy| policy.resume) {
                    command.args(["resume", "--last"]);
                }
                command.arg(self.prompt);
            }
            "opencode-v1" => {
                // V1 continuation checks process reopening only. Its summarizing
                // compaction protocol is not part of this fixture.
                if self.websocket {
                    return Err("OpenCode V1 fixture only supports HTTP".into());
                }
                let provider = "exetrouter";
                let model = format!("{provider}/{}", self.model);
                let mut options = catalog.clone();
                options["model"] = json!(model);
                options["enabled_providers"] = json!([provider]);
                options["provider"][provider]["options"]["baseURL"] =
                    json!(format!("{}/v1", self.url));
                options["permission"] = json!({"*":"allow"});
                command
                    .env("XDG_CONFIG_HOME", &config)
                    .env("XDG_DATA_HOME", self.directory.join("data"))
                    .env("XDG_STATE_HOME", self.directory.join("state"))
                    .env("XDG_CACHE_HOME", self.directory.join("cache"))
                    .env("OPENCODE_CONFIG_DIR", &config)
                    .env("OPENCODE_CONFIG_CONTENT", options.to_string())
                    .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
                    .env("OPENCODE_DISABLE_MODELS_FETCH", "true")
                    .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
                    .args([
                        "run",
                        "--auto",
                        "--format",
                        "json",
                        "--model",
                        &model,
                        "--title",
                        "Compatibility fixture",
                    ]);
                if policy.is_some_and(|policy| policy.resume) {
                    command.arg("--continue");
                }
                command.arg(self.prompt);
            }
            "opencode" => {
                let model = format!("exetrouter/{}", self.model);
                if !catalog["providers"]["exetrouter"]["models"].is_object() {
                    return Err("invalid OpenCode model catalog".into());
                }
                let mut options = catalog.clone();
                options["model"] = json!(model);
                options["providers"]["exetrouter"]["settings"]["baseURL"] =
                    json!(format!("{}/v1", self.url));
                options["providers"]["exetrouter"]["settings"]["transport"] =
                    json!(if self.websocket { "websocket" } else { "http" });
                if let Some(policy) = policy {
                    let window = options["providers"]["exetrouter"]["models"][self.model]["limit"]
                        ["input"]
                        .as_u64()
                        .ok_or("OpenCode input limit unavailable")?;
                    if policy.token_limit >= window {
                        return Err("test compaction threshold exceeds input limit".into());
                    }
                    options["compaction"] =
                        json!({"auto":true,"buffer":window-policy.token_limit,"keep":{"tokens":0}});
                }
                command
                    .env("XDG_CONFIG_HOME", &config)
                    .env("XDG_DATA_HOME", self.directory.join("data"))
                    .env("XDG_STATE_HOME", self.directory.join("state"))
                    .env("XDG_CACHE_HOME", self.directory.join("cache"))
                    .env("OPENCODE_CONFIG_DIR", &config)
                    .env("OPENCODE_CONFIG_CONTENT", options.to_string())
                    .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
                    .env("OPENCODE_DISABLE_MODELS_FETCH", "true")
                    .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
                    .args([
                        "run",
                        "--standalone",
                        "--auto",
                        "--format",
                        "json",
                        "--model",
                        &model,
                    ]);
                if policy.is_some_and(|policy| policy.resume) {
                    command.arg("--continue");
                }
                command.arg(self.prompt);
            }
            _ => return Err("unknown native test client".into()),
        }
        let output = tokio::time::timeout(Duration::from_secs(180), command.output())
            .await
            .map_err(|_| "native client timed out; inspect usage before another run")??;
        Ok(output)
    }
}

pub fn tool_result(stdout: &[u8]) -> bool {
    tool_output(stdout, "EXETROUTER_TOOL_OK")
}
pub fn tool_output(stdout: &[u8], marker: &str) -> bool {
    fn visit(value: &Value, marker: &str) -> bool {
        match value {
            Value::Object(fields) => fields.iter().any(|(name, value)| {
                (matches!(name.as_str(), "aggregated_output" | "output")
                    && (name != "aggregated_output"
                        || fields.get("exit_code").and_then(Value::as_i64) == Some(0))
                    && value.as_str().is_some_and(|text| {
                        text.contains("EXETROUTER_TOOL_OK") && text.contains(marker)
                    }))
                    || visit(value, marker)
            }),
            Value::Array(values) => values.iter().any(|value| visit(value, marker)),
            _ => false,
        }
    }
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|value| visit(&value, marker))
}

pub fn diagnostic(stdout: &[u8]) -> Value {
    let text = String::from_utf8_lossy(stdout);
    let events: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    // No arbitrary keys, event names, messages, tool arguments or text enter diagnostics.
    json!({"bytes":stdout.len(),"json_events":events.len(),
        "error_events":events.iter().filter(|e|e["type"]=="error").count(),
        "tool_marker":text.contains("EXETROUTER_TOOL_OK"),
        "smoke_marker":text.contains("EXETROUTER_SMOKE_OK")})
}

#[cfg(test)]
mod tests {
    use super::codex_sandbox_mode;

    #[test]
    fn normal_native_runs_keep_codex_read_only() {
        assert_eq!(
            codex_sandbox_mode(false, false, "https://router.example", "real-model").unwrap(),
            "read-only"
        );
    }

    #[test]
    fn outer_sandbox_opt_in_is_limited_to_isolated_synthetic_loopback() {
        for url in ["http://127.0.0.1:8080", "http://[::1]:8080/"] {
            assert_eq!(
                codex_sandbox_mode(true, true, url, "gpt-test").unwrap(),
                "danger-full-access"
            );
        }
        assert!(codex_sandbox_mode(true, false, "http://127.0.0.1:8080", "gpt-test").is_err());
        assert!(codex_sandbox_mode(true, true, "http://127.0.0.1:8080", "real-model").is_err());
        for url in [
            "https://127.0.0.1:8080",
            "http://router.example:8080",
            "http://localhost:8080",
            "http://0.0.0.0:8080",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://user@127.0.0.1:8080",
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/?secret=test",
            "http://127.0.0.1:8080/#fragment",
            "invalid",
        ] {
            assert!(codex_sandbox_mode(true, true, url, "gpt-test").is_err());
        }
    }
}
