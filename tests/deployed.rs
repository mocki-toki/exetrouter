//! Explicit public deployment check; secrets stay in memory and private profiles.
#[path = "support/native.rs"]
mod native;
#[path = "support/probe.rs"]
mod probe;
use exetrouter::Result;
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

async fn control(request: Value) -> Result<Value> {
    let identity = std::env::var("EXETROUTER_DEPLOY_IDENTITY")?;
    let host = std::env::var("EXETROUTER_DEPLOY_HOST")?;
    let user = std::env::var("EXETROUTER_DEPLOY_USER").unwrap_or_else(|_| "routercli".into());
    let port = std::env::var("EXETROUTER_DEPLOY_PORT").unwrap_or_else(|_| "2222".into());
    if host.is_empty()
        || host.starts_with('-')
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_:[]".contains(&c))
        || user.is_empty()
        || user.starts_with('-')
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || port.parse::<u16>().ok().is_none_or(|p| p == 0)
    {
        return Err("invalid deployment SSH connection".into());
    }
    let destination = format!("{user}@{host}");
    let mut child = Command::new("ssh")
        .args([
            "-T",
            "-p",
            &port,
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-i",
            &identity,
            &destination,
            "exrd-gateway",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("missing control stdin")?;
    stdin.write_all(&serde_json::to_vec(&request)?).await?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(45), child.wait_with_output()).await??;
    if !output.status.success() {
        return Err("deployment SSH failed".into());
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    if value["ok"] != true {
        return Err("deployment control rejected request".into());
    }
    Ok(value["result"].clone())
}

async fn verification_token() -> Result<Value> {
    if let Ok(secret) = std::env::var("EXETROUTER_DEPLOY_BEARER") {
        if secret.is_empty() || secret.len() > 4096 || secret.chars().any(char::is_control) {
            return Err("invalid externally managed verification bearer".into());
        }
        // The calling operator owns creation/revocation; no OAuth state is copied.
        return Ok(json!({"secret":secret,"token":{"id":null}}));
    }
    control(
        json!({"action":"token_create","name":"deployment-native-verification","expires_days":1}),
    )
    .await
}

async fn revoke_verification_token(issued: &Value) -> Result<()> {
    if !issued["token"]["id"].is_null() {
        let revoked = control(json!({"action":"token_revoke","id":issued["token"]["id"]})).await?;
        if revoked["revoked"] != true {
            return Err("verification bearer revocation failed".into());
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "contacts production OAuth; requires explicit EXETROUTER_DEPLOY_* settings"]
async fn deployed_native_matrix() -> Result<()> {
    let manifest: Value = serde_json::from_slice(&fs::read("target/compat/current-clients.json")?)?;
    for client in ["codex", "opencode"] {
        let binary = manifest[client]["binary"]
            .as_str()
            .ok_or("missing client binary")?;
        let reviewed = native::reviewed_version(client);
        if manifest[client]["version"] != reviewed {
            return Err("downloaded client does not match the reviewed source manifest".into());
        }
        native::version(binary, &reviewed).await?;
    }
    let issued = verification_token().await?;
    let result: Result<()> = async {
        let bearer=issued["secret"].as_str().ok_or("missing verification bearer")?;
        for (client, websocket) in [("codex",false),("codex",true),("opencode",false),("opencode",true)] {
            let binary=manifest[client]["binary"].as_str().ok_or("missing client binary")?;
            let version=manifest[client]["version"].as_str().ok_or("missing client version")?;
            let directory=tempfile::tempdir()?;
            fs::set_permissions(directory.path(),fs::Permissions::from_mode(0o700))?;
            let endpoint=std::env::var("EXETROUTER_DEPLOY_URL")?;
            let observer=probe::Probe::bounded(&endpoint,12).await?;
            let model=std::env::var("EXETROUTER_DEPLOY_MODEL").unwrap_or_else(|_|"gpt-6.1-sol".into());
            let output=native::Run {binary,client,url:&observer.url,model:&model,bearer,websocket,directory:directory.path(),prompt:"Run /bin/echo EXETROUTER_TOOL_OK exactly once. Then return exactly EXETROUTER_SMOKE_OK."}.execute().await.map_err(|_| { println!("{}",json!({"check":"native_run_failed","client":client,"websocket":websocket,"observation":observer.observed.lock().unwrap().metadata()})); "native run failed; inspect observed stages before another independent check" })?;
            let observed=observer.observed.lock().unwrap().metadata();
            let cycle=observed["tool_markers"].as_u64().unwrap_or(0)>0 || (observed["tool_results"].as_u64().unwrap_or(0)>0 && native::tool_result(&output.stdout));
            if !output.status.success() || !cycle || !String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK") {
                println!("deployment check failed: client={client}, websocket={websocket}, success={}, observation={observed}, diagnostic={}",output.status.success(),native::diagnostic(&output.stdout));
                return Err("public native tool cycle failed; inspect usage before retry".into());
            }
            println!("Public TLS native tool cycle passed: {client} {version}, websocket={websocket}");
        }
        Ok(())
    }.await;
    revoke_verification_token(&issued).await?;
    result
}

/// A supervising operator sends R only after a verified, drained restart.
async fn restart_acknowledgement() -> Result<()> {
    use std::io::{Read, Write};
    println!("{}", json!({"check":"restart_required"}));
    std::io::stdout().flush()?;
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let mut descriptor = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // Nonblocking readiness avoids an uninterruptible stdin worker.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
            if ready < 0 {
                return Err("restart acknowledgement poll failed".into());
            }
            if ready > 0 {
                let mut byte = [0];
                std::io::stdin()
                    .read_exact(&mut byte)
                    .map_err(|_| "restart acknowledgement unavailable")?;
                return if byte == *b"R" {
                    Ok(())
                } else {
                    Err("restart was not confirmed by the supervisor".into())
                };
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "restart acknowledgement timed out")?
}

#[tokio::test]
#[ignore = "real current-client compaction through deployment; consumes quota and optionally requires supervised restart"]
async fn deployed_native_continuity() -> Result<()> {
    let endpoint = std::env::var("EXETROUTER_DEPLOY_URL")?;
    if !endpoint.starts_with("https://") {
        return Err("deployment continuity requires verified HTTPS ingress".into());
    }
    let model = std::env::var("EXETROUTER_DEPLOY_MODEL")?;
    let selected = std::env::var("EXETROUTER_DEPLOY_CLIENT").ok();
    let restart = std::env::var("EXETROUTER_DEPLOY_RESTART_STDIN").as_deref() == Ok("1");
    let large = std::env::var("EXETROUTER_DEPLOY_LARGE_CONTEXT").as_deref() == Ok("1");
    let idle = std::env::var("EXETROUTER_DEPLOY_IDLE_SECONDS")
        .unwrap_or_else(|_| "0".into())
        .parse::<u64>()?;
    if idle > 7200 || (large && selected.is_none()) {
        return Err(
            "idle must be at most 7200 seconds; large-context mode requires one selected case"
                .into(),
        );
    }
    let manifest: Value = serde_json::from_slice(&fs::read("target/compat/current-clients.json")?)?;
    let cases = [
        ("codex-http", "codex", false),
        ("codex-ws", "codex", true),
        ("opencode-http", "opencode", false),
        ("opencode-ws", "opencode", true),
    ];
    if selected
        .as_ref()
        .is_some_and(|selected| !cases.iter().any(|(name, _, _)| name == selected))
    {
        return Err("unknown deployment continuity case".into());
    }
    for client in ["codex", "opencode"] {
        let binary = manifest[client]["binary"]
            .as_str()
            .ok_or("missing client binary")?;
        let reviewed = native::reviewed_version(client);
        if manifest[client]["version"] != reviewed {
            return Err("client source review mismatch".into());
        }
        native::version(binary, &reviewed).await?;
    }
    let issued = verification_token().await?;
    let result:Result<()> = async {
        let bearer=issued["secret"].as_str().ok_or("missing verification bearer")?;
        for (name,client,websocket) in cases {
            if selected.as_ref().is_some_and(|selected|selected!=name) {continue;}
            let directory=tempfile::tempdir()?;
            fs::set_permissions(directory.path(),fs::Permissions::from_mode(0o700))?;
            let observer=probe::Probe::bounded(&endpoint,if large {12} else {36}).await?;
            let memory=format!("EXETROUTER_MEMORY_{:016X}",rand::random::<u64>());
            observer.observed.lock().unwrap().expect_memory(&memory);
            let reference=(0..if large {2500} else {160}).map(|index|format!("Synthetic disposable record {index:04}: {:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}\n",rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>(),rand::random::<u64>())).collect::<String>();
            let mut threshold=if large {150_000} else {100_000};
            let steps=if large {2} else {5};
            for step in 0..steps {
                if step==3 && restart {restart_acknowledgement().await?;}
                if step==steps-1 && idle>0 {
                    println!("{}",json!({"case":name,"check":"idle_started","idle_seconds":idle}));
                    std::io::Write::flush(&mut std::io::stdout())?;
                    tokio::time::sleep(Duration::from_secs(idle)).await;
                }
                let prompt=match step {
                    0=>format!("Remember the exact continuity token {memory} for future turns and compaction. Keep it when history is compacted. The following reference is disposable. Do not call tools. Return exactly EXETROUTER_SMOKE_OK.\n{reference}"),
                    2=>format!("Preserve the remembered continuity token. Add this disposable reference to our history. Run /bin/echo EXETROUTER_TOOL_OK followed by that remembered token. Return EXETROUTER_SMOKE_OK followed by the token. Do not read files.\n{reference}"),
                    _=>"Run /bin/echo EXETROUTER_TOOL_OK followed by the exact continuity token I asked you to remember. Return EXETROUTER_SMOKE_OK followed by that token. Do not read files.".into(),
                };
                let before=observer.observed.lock().unwrap().compaction_requests;
                let started=std::time::Instant::now();
                let output=native::Run {binary:manifest[client]["binary"].as_str().unwrap(),client,url:&observer.url,model:&model,bearer,websocket,directory:directory.path(),prompt:&prompt}.continued(&native::Continuation {token_limit:threshold,resume:step>0}).await.map_err(|_| { println!("{}",json!({"check":"native_run_failed","case":name,"step":step,"observation":observer.observed.lock().unwrap().metadata()})); "native continuation failed; inspect observed stages before another independent check" })?;
                let metadata=observer.observed.lock().unwrap().metadata();
                let memory_ok=step==0 || native::tool_output(&output.stdout,&memory);
                let required_cycle=if large {step==1} else {matches!(step,1|3)};
                let cycle_ok=!required_cycle || metadata["compaction_requests"].as_u64().unwrap_or(0)>before as u64;
                println!("{}",json!({"case":name,"step":step,"exit_success":output.status.success(),"memory_in_tool":memory_ok,"compaction_cycle_observed":cycle_ok,"duration_ms":started.elapsed().as_millis(),"observation":metadata}));
                if !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("EXETROUTER_SMOKE_OK") || !memory_ok || !cycle_ok {return Err("deployment continuation failed; inspect accounting before another independent test".into());}
                threshold=if large {150_000} else if matches!(step,0|2) {metadata["last_input_tokens"].as_u64().ok_or("continuation input usage unknown")?.saturating_sub(512).max(8000)} else {100_000};
            }
            let metadata=observer.observed.lock().unwrap().metadata();
            if metadata["checkpoint_tool_results"].as_u64().unwrap_or(0)==0 {return Err("checkpoint tool continuation was not observed".into());}
            if large && metadata["max_input_tokens"].as_u64().unwrap_or(0)<200_000 {return Err("large-context acceptance did not reach 200000 observed input tokens".into());}
            println!("{}",json!({"case":name,"check":if large {"large_context_continuity"} else {"two_compaction_cycles"},"passed":true,"restart_supervised":restart&&!large,"idle_seconds":idle,"observation":metadata}));
        }
        Ok(())
    }.await;
    revoke_verification_token(&issued).await?;
    result
}
