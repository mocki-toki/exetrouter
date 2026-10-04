use exetrouter::{
    backup::{self, Paths},
    init, oauth,
};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct State {
    directory: tempfile::TempDir,
    paths: Paths,
    conn: Connection,
    bearer: String,
    revoked: String,
}
fn write(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}
fn state() -> State {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = Paths::in_directory(&source);
    write(&paths.key, &[1; 32]);
    write(&paths.oauth_key, &[2; 32]);
    write(&paths.db, &[]);
    let mut conn = Connection::open(&paths.db).unwrap();
    init(&conn).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let user = exetrouter::create_user(&conn, "fixture").unwrap();
    let token = exetrouter::create_token(&conn, &[1; 32], user, "active", 90).unwrap();
    let revoked = exetrouter::create_token(&conn, &[1; 32], user, "revoked", 90).unwrap();
    exetrouter::revoke_token(&conn, user, &revoked.token.id).unwrap();
    let account = oauth::save(
        &conn,
        &oauth::Vault::new([2; 32]),
        "fixture-upstream",
        &oauth::Credentials {
            access_token: "fixture-access".into(),
            refresh_token: "fixture-refresh".into(),
        },
        chrono::Utc::now().timestamp() + 3600,
    )
    .unwrap();
    conn.execute("INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,request_id) VALUES(1,?1,?2,'responses','fixture','sent','fixture-request')",params![user,token.token.id]).unwrap();
    conn.execute(
        "INSERT INTO context_bindings VALUES(?1,?2,?3,9999999999)",
        params![user, vec![3u8; 32], account],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO oauth_quota_windows VALUES(?1,'primary',25,300,9999999999,1,1)",
        [account],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO oauth_health VALUES(?1,'responses',0,1,9999999999,'transport',1)",
        [account],
    )
    .unwrap();
    exetrouter::account_preferences::set_user_rules(
        &mut conn,
        user,
        account,
        None,
        None,
        &exetrouter::account_preferences::RoutingArgs {
            priority: Some(exetrouter::account_preferences::Setting::Number(255)),
            switch_at: Some(exetrouter::account_preferences::Setting::Number(20)),
            ..Default::default()
        },
    )
    .unwrap();
    State {
        directory,
        paths,
        conn,
        bearer: token.secret,
        revoked: revoked.secret,
    }
}
fn command(paths: &Paths) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_exrd"));
    cmd.arg("--json");
    cmd.arg("--db")
        .arg(&paths.db)
        .arg("--key")
        .arg(&paths.key)
        .arg("--oauth-key")
        .arg(&paths.oauth_key);
    cmd
}
struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(address: &str, path: &str, bearer: Option<&str>) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let text = if let Some(bearer) = bearer {
        format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nContent-Type: application/json\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{{")
    } else {
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
    };
    stream.write_all(text.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn cli_snapshot_includes_live_wal_keys_and_restores_a_restartable_state() {
    let state = state();
    assert!(
        fs::metadata(state.paths.db.with_extension("sqlite-wal"))
            .unwrap()
            .len()
            > 0
    );
    let snapshot = state.directory.path().join("snapshot");
    let output = command(&state.paths)
        .args(["admin", "backup", "create", "--to"])
        .arg(&snapshot)
        .output()
        .unwrap();
    assert!(output.status.success(), "backup CLI failed");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["snapshot"]["users"], 1);
    assert_eq!(report["snapshot"]["oauth_accounts"], 1);
    assert_eq!(report["snapshot"]["usage_events"], 1);
    assert_eq!(report["snapshot"]["pending_requests"], 1);
    for name in [
        "exetrouter.sqlite",
        "exetrouter.key",
        "exetrouter.oauth.key",
        "backup.json",
    ] {
        assert_eq!(
            fs::metadata(snapshot.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!snapshot.join("exetrouter.sqlite-wal").exists());
    // Verify/restore do not require a live database or keys in the caller's cwd.
    let empty = state.directory.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_exrd"))
        .arg("--json")
        .current_dir(&empty)
        .args(["admin", "backup", "verify", "--from"])
        .arg(&snapshot)
        .output()
        .unwrap();
    assert!(output.status.success(), "offline verify CLI failed");
    let restored = state.directory.path().join("restored");
    let output = Command::new(env!("CARGO_BIN_EXE_exrd"))
        .arg("--json")
        .current_dir(&empty)
        .args(["admin", "backup", "restore", "--from"])
        .arg(&snapshot)
        .arg("--to")
        .arg(&restored)
        .output()
        .unwrap();
    assert!(output.status.success(), "restore CLI failed");
    let paths = Paths::in_directory(&restored);
    assert_eq!(fs::read(&paths.key).unwrap(), vec![1; 32]);
    assert_eq!(fs::read(&paths.oauth_key).unwrap(), vec![2; 32]);
    let conn = Connection::open(&paths.db).unwrap();
    assert!(exetrouter::authenticate(&conn, &[1; 32], &state.bearer)
        .unwrap()
        .is_some());
    assert!(exetrouter::authenticate(&conn, &[1; 32], &state.revoked)
        .unwrap()
        .is_none());
    let account = oauth::load(&conn, &oauth::Vault::new([2; 32]), 1).unwrap();
    assert!(account.credentials.access_token == "fixture-access");
    assert!(account.credentials.refresh_token == "fixture-refresh");
    let preferences = exetrouter::account_preferences::effective(&conn, Some(1), 1).unwrap();
    assert_eq!(preferences.priority, 255);
    assert_eq!(preferences.rules.switch_at, Some(20));
    for table in ["context_bindings", "oauth_quota_windows", "oauth_health"] {
        assert_eq!(
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    drop(conn);
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let mut service = Service(
        command(&paths)
            .arg("--control-socket")
            .arg(restored.join("control.sock"))
            .args([
                "serve",
                "--listen",
                &address,
                "--gateway-uid",
                &unsafe { libc::geteuid() }.to_string(),
                "--allow-same-uid-for-dev",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    loop {
        assert!(
            service.0.try_wait().unwrap().is_none(),
            "restored service exited"
        );
        if TcpStream::connect(&address).is_ok() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(request(&address, "/healthz", None).starts_with("HTTP/1.1 200"));
    assert!(request(&address, "/v1/responses", Some(&state.bearer)).starts_with("HTTP/1.1 400"));
    assert!(request(&address, "/v1/responses", Some(&state.revoked)).starts_with("HTTP/1.1 401"));
    assert_eq!(
        unsafe { libc::kill(service.0.id() as i32, libc::SIGTERM) },
        0
    );
    assert!(service.0.wait().unwrap().success());
    assert!(!restored.join("control.sock").exists());
    let conn = Connection::open(&paths.db).unwrap();
    let (status, input, output): (String, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT status,input_tokens,output_tokens FROM usage_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "aborted_unknown");
    assert_eq!((input, output), (None, None));
    assert_eq!(
        state
            .conn
            .query_row("SELECT status FROM usage_events", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "sent"
    );
    assert!(backup::verify(&snapshot).is_ok());
}

#[test]
fn snapshots_reject_damage_sidecars_and_symlinks_without_overwriting_destinations() {
    let state = state();
    let snapshot = state.directory.path().join("snapshot");
    backup::create(&state.paths, &snapshot).unwrap();
    assert!(backup::create(&state.paths, &snapshot).is_err());
    let destination = state.directory.path().join("restored");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), b"keep").unwrap();
    assert!(backup::restore(&snapshot, &destination).is_err());
    assert_eq!(fs::read(destination.join("keep")).unwrap(), b"keep");
    write(&snapshot.join("exetrouter.sqlite-wal"), b"sidecar");
    assert!(backup::verify(&snapshot).is_err());
    fs::remove_file(snapshot.join("exetrouter.sqlite-wal")).unwrap();
    let saved = fs::read(snapshot.join("exetrouter.oauth.key")).unwrap();
    fs::write(snapshot.join("exetrouter.oauth.key"), [9; 32]).unwrap();
    let rejected = state.directory.path().join("rejected");
    assert!(backup::restore(&snapshot, &rejected).is_err());
    assert!(!rejected.exists());
    fs::write(snapshot.join("exetrouter.oauth.key"), &saved).unwrap();
    fs::remove_file(snapshot.join("exetrouter.key")).unwrap();
    symlink(&state.paths.key, snapshot.join("exetrouter.key")).unwrap();
    assert!(backup::verify(&snapshot).is_err());
    let alias = state.directory.path().join("alias");
    symlink(&snapshot, &alias).unwrap();
    assert!(backup::verify(&alias).is_err());
    assert!(!rejected.exists());
}

#[test]
fn online_snapshot_preserves_transaction_consistency_during_writes() {
    let state = state();
    state.conn.execute_batch("CREATE TABLE consistency(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER,padding BLOB); INSERT INTO consistency VALUES(1,0,0,zeroblob(3000000))").unwrap();
    let path = state.paths.db.clone();
    let (ready, started) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut conn = Connection::open(path).unwrap();
        conn.busy_timeout(Duration::from_secs(3)).unwrap();
        ready.send(()).unwrap();
        for n in 1..=40 {
            let tx = conn.transaction().unwrap();
            tx.execute("UPDATE consistency SET a=?1 WHERE id=1", [n])
                .unwrap();
            std::thread::sleep(Duration::from_millis(2));
            tx.execute("UPDATE consistency SET b=?1 WHERE id=1", [n])
                .unwrap();
            tx.commit().unwrap();
        }
    });
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    let snapshot = state.directory.path().join("online");
    let checked = backup::create(&state.paths, &snapshot);
    writer.join().unwrap();
    checked.unwrap();
    let conn = Connection::open(snapshot.join("exetrouter.sqlite")).unwrap();
    let (a, b): (i64, i64) = conn
        .query_row("SELECT a,b FROM consistency WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(a, b, "snapshot included a partial transaction");
    assert!((0..=40).contains(&a));
    drop(conn);
    backup::verify(&snapshot).unwrap();
}

#[test]
fn invalid_source_keys_schema_and_permissions_leave_no_incomplete_snapshot() {
    let state = state();
    let destination = state.directory.path().join("failed");
    fs::write(&state.paths.oauth_key, [9; 32]).unwrap();
    assert!(backup::create(&state.paths, &destination).is_err());
    assert!(!destination.exists());
    fs::write(&state.paths.oauth_key, [2; 32]).unwrap();
    state.conn.pragma_update(None, "user_version", 999).unwrap();
    assert!(backup::create(&state.paths, &destination).is_err());
    assert!(!destination.exists());
    state
        .conn
        .pragma_update(None, "user_version", exetrouter::store::SCHEMA_VERSION)
        .unwrap();
    fs::set_permissions(&state.paths.key, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(backup::create(&state.paths, &destination).is_err());
    assert!(!destination.exists());
    fs::set_permissions(&state.paths.key, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&state.paths.oauth_key, [1; 32]).unwrap();
    assert!(backup::create(&state.paths, &destination).is_err());
    assert!(!destination.exists());
    let untrusted_parent = state.directory.path().join("writable");
    fs::create_dir(&untrusted_parent).unwrap();
    fs::set_permissions(&untrusted_parent, fs::Permissions::from_mode(0o777)).unwrap();
    fs::write(&state.paths.oauth_key, [2; 32]).unwrap();
    assert!(backup::create(&state.paths, &untrusted_parent.join("failed")).is_err());
    assert!(!untrusted_parent.join("failed").exists());
}
