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
        let work = self.directory.join("workspace");
        let config = self.directory.join("client-config");
        fs::create_dir_all(&work)?;
        fs::create_dir_all(&config)?;
        let catalog_url = format!(
            "{}/v1/models/{}",
            self.url,
            if self.client == "codex" {
                "codex"
            } else {
                "opencode"
            }
        );
        let catalog: Value = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
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
        let marker = config.join("retry-hook.ready");
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
                fs::write(config.join("config.toml"), format!("model = {}\nmodel_provider = \"exetrouter\"\nmodel_catalog_json = {}\napproval_policy = \"never\"\n{compaction}[model_providers.exetrouter]\nname = {}\nbase_url = {}\nenv_key = \"EXETROUTER_TOKEN\"\nwire_api = \"responses\"\nsupports_websockets = {}\nrequest_max_retries = 0\nstream_max_retries = 0\n",json!(self.model),json!(catalog_path),json!(name), json!(format!("{}/v1",self.url)),self.websocket))?;
                command.env("CODEX_HOME", &config).args([
                    "exec",
                    "--ignore-rules",
                    "--skip-git-repo-check",
                    "--color",
                    "never",
                    "--json",
                    "--sandbox",
                    "read-only",
                ]);
                if policy.is_none() {
                    command.arg("--ephemeral");
                } else if policy.is_some_and(|policy| policy.resume) {
                    command.args(["resume", "--last"]);
                }
                command.arg(self.prompt);
            }
            "opencode-v1" | "opencode-v1-bridge" => {
                // V1 continuation checks process reopening only. Its summarizing
                // compaction protocol is not part of this fixture.
                if self.websocket {
                    return Err("OpenCode V1 fixture only supports HTTP".into());
                }
                let provider = if self.client == "opencode-v1-bridge" {
                    "exetrouter"
                } else {
                    "openai"
                };
                let model = format!("{provider}/{}", self.model);
                let mut options = json!({"model":model,"enabled_providers":[provider],"provider":{provider:{"npm":"@ai-sdk/openai","env":["EXETROUTER_TOKEN"],"options":{"baseURL":format!("{}/v1",self.url)},"models":{self.model:{"name":"Synthetic fixture","limit":{"context":128000,"output":8192},"options":{"store":false}}}}},"permission":{"*":"allow"}});
                if self.client == "opencode-v1-bridge" {
                    let plugin = Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("clients/opencode-v1/exetrouter.mjs");
                    options["plugin"] = json!([format!("file://{}", plugin.display())]);
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
                let plugin = config.join("no-retries");
                fs::create_dir_all(&plugin)?;
                fs::write(
                    plugin.join("package.json"),
                    r#"{"type":"module","main":"index.js"}"#,
                )?;
                let bridge = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("clients/opencode/exetrouter/index.js");
                fs::write(plugin.join("index.js"),format!("import {{ writeFile }} from 'node:fs/promises';\nimport bridge from {};\nexport default {{ id: 'exetrouter-test-bridge', async setup(context) {{ const dispose = await bridge.setup(context); await writeFile({}, 'ready', {{mode:0o600}}); return dispose; }} }};\n",json!(format!("file://{}",bridge.display())),json!(marker)))?;
                let model = format!("exetrouter/{}", self.model);
                if !catalog["providers"]["exetrouter"]["models"].is_object() {
                    return Err("invalid OpenCode model catalog".into());
                }
                let mut options = json!({"model":model,"plugins":[plugin],"providers":{"exetrouter":{"name":"ExetRouter","canonical":"openai","package":"@opencode/ai/providers/openai/responses","env":["EXETROUTER_TOKEN"],"settings":{"baseURL":format!("{}/v1",self.url),"transport":if self.websocket {"websocket"} else {"http"},"store":false,"compaction":{"type":"native"}},"models":catalog["providers"]["exetrouter"]["models"]}}});
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
        if self.client == "opencode" && !marker.is_file() {
            return Err("OpenCode test retry hook did not load".into());
        }
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
