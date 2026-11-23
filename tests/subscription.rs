//! Offline owner/operator CLI parity, with no upstream or valid credentials.
use std::{fs, path::Path, process::Command};

fn insert_account(path: &Path) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('synthetic','active',X'00',9999999999,0)", []).unwrap();
}

#[test]
fn standalone_owner_and_server_operator_share_billing_rules_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("standalone");
    let config = dir.path().join("client.json");
    let run_local = |fields: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_exr"))
            .env("EXR_NO_UPDATE_CHECK", "1")
            .args([
                "--config",
                config.to_str().unwrap(),
                "--standalone",
                "--state-dir",
                state.to_str().unwrap(),
            ])
            .args(fields)
            .output()
            .unwrap()
    };
    let init = run_local(&["account", "list", "--json"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    insert_account(&state.join("state.sqlite"));
    let db = dir.path().join("server.sqlite");
    let key = dir.path().join("server.key");
    let oauth_key = dir.path().join("oauth.key");
    fs::write(dir.path().join("server.json"), b"{}").unwrap();
    let run_server = |fields: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_exrd"))
            .env("EXR_NO_UPDATE_CHECK", "1")
            .args([
                "--config",
                dir.path().join("server.json").to_str().unwrap(),
                "--db",
                db.to_str().unwrap(),
                "--key",
                key.to_str().unwrap(),
                "--oauth-key",
                oauth_key.to_str().unwrap(),
            ])
            .args(fields)
            .output()
            .unwrap()
    };
    let init = run_server(&["init", "--json"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    insert_account(&db);
    let fields = [
        "1",
        "--billing-day",
        "8",
        "--plan-usd",
        "100",
        "--api-budget-usd",
        "125.50",
        "--json",
    ];
    let local = run_local(&[&["account", "subscription"][..], &fields].concat());
    let remote = run_server(&[&["admin", "oauth", "subscription"][..], &fields].concat());
    for output in [&local, &remote] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let local: serde_json::Value = serde_json::from_slice(&local.stdout).unwrap();
    let remote: serde_json::Value = serde_json::from_slice(&remote.stdout).unwrap();
    assert_eq!(local, remote);
    assert_eq!(local["billing_day"], 8);
    assert_eq!(local["effective_api_budget_cents"], 12_550);
    assert!(!local.to_string().contains("remaining"));
    let human = run_local(&["account", "subscription", "1", "--clear-api-budget"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Billing day: 8") && human.contains("$100 (not a credit balance)"));
    let admin_file = dir.path().join("admin-input");
    fs::write(
        &admin_file,
        concat!("sk-admin-", "synthetic-cli-only-invalid-key"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&admin_file, fs::Permissions::from_mode(0o600)).unwrap();
    let commands = [
        run_local(&[
            "account",
            "platform",
            "set-admin-key",
            "1",
            "--key-file",
            admin_file.to_str().unwrap(),
            "--json",
        ]),
        run_server(&[
            "admin",
            "oauth",
            "platform",
            "set-admin-key",
            "1",
            "--key-file",
            admin_file.to_str().unwrap(),
            "--json",
        ]),
    ];
    for output in commands {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["inference_enabled"], false);
        assert_eq!(result["credit_status"], "unknown");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("sk-"));
    }
    let remote = run_server(&["admin", "oauth", "platform", "status", "1", "--json"]);
    let local = run_local(&["account", "platform", "status", "1", "--json"]);
    for output in [local, remote] {
        assert!(output.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["credit_status"],
            "unknown"
        );
    }
    for amount in ["75.25", "0"] {
        for output in [
            run_local(&[
                "account",
                "platform",
                "set-balance",
                "1",
                "--usd",
                amount,
                "--json",
            ]),
            run_server(&[
                "admin",
                "oauth",
                "platform",
                "set-balance",
                "1",
                "--usd",
                amount,
                "--json",
            ]),
        ] {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let initial = exetrouter::subscription::parse_usd(amount).unwrap() * 10_000_000;
            assert_eq!(result["balance"]["initial_usd_nanos"], initial);
            assert_eq!(
                result["balance"]["exetrouter"]["remaining_usd_nanos"],
                initial
            );
            assert!(result["balance"]["organization"]["remaining_usd_nanos"].is_null());
            assert_eq!(
                result["balance"]["organization"]["status"],
                "awaiting_report"
            );
            assert_eq!(result["inference_enabled"], false);
        }
    }
    let human = run_local(&["account", "platform", "status", "1"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(
        human.contains("ExetRouter: $0 left") && human.contains("Platform organization: Unknown")
    );
    for output in [
        run_local(&["account", "platform", "set-balance", "1", "--usd", "-1"]),
        run_server(&[
            "admin",
            "oauth",
            "platform",
            "set-balance",
            "1",
            "--usd",
            "NaN",
        ]),
    ] {
        assert!(!output.status.success());
    }
    // Both native entry points reject discovery locally without a working key;
    // storing an Admin Key alone must never send a model or inference request.
    for output in [
        run_local(&["account", "platform", "models", "1", "--json"]),
        run_server(&["admin", "oauth", "platform", "models", "1", "--json"]),
    ] {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("no usable Platform working key"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("sk-"));
    }
    for (resources, expected) in [
        (&["platform"][..], serde_json::json!(["platform"])),
        (
            &["platform", "codex"][..],
            serde_json::json!(["codex", "platform"]),
        ),
        (&["codex"][..], serde_json::json!(["codex"])),
    ] {
        let mut local_args = vec!["account", "platform", "resources", "--json"];
        let mut server_args = vec!["admin", "oauth", "platform", "resources", "--json"];
        local_args.extend(resources);
        server_args.extend(resources);
        for output in [run_local(&local_args), run_server(&server_args)] {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let policy: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(policy["resources"], expected);
            assert_eq!(policy["legacy_platform_restriction"], false);
        }
    }
    for invalid in [&["codex", "codex"][..], &["unknown"][..]] {
        let mut args = vec!["admin", "oauth", "platform", "resources"];
        args.extend(invalid);
        assert!(!run_server(&args).status.success());
    }
    let persisted = run_server(&["admin", "oauth", "platform", "resources", "--json"]);
    assert!(persisted.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&persisted.stdout).unwrap()["resources"],
        serde_json::json!(["codex"])
    );
    for path in [state.join("state.sqlite"), db] {
        let db = rusqlite::Connection::open(path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "00"
        );
    }
    assert_eq!(fs::read(key).unwrap().len(), 32);
}
