use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
};
use tempfile::TempDir;

struct Client {
    dir: TempDir,
}
impl Client {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let key = dir.path().join("identity");
        fs::write(&key, "test-only identity").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(dir.path().join("ssh"),r#"#!/bin/sh
request=$(cat)
printf '%s\n' "$request" >> "$EXETROUTER_TEST_REQUESTS"
case "$request" in
  *'"action":"private_list"'*) printf '%s' '{"ok":true,"result":{}}' ;;
  *'"action":"private_put"'*) printf '%s' '{"ok":true,"result":{"updated":true}}' ;;
  *'"action":"token_show"'*) printf '%s' '{"ok":true,"result":{"id":"tok_test","name":"Laptop"}}' ;;
  *'"action":"token_rename"'*) printf '%s' '{"ok":true,"result":{"updated":true}}' ;;
  *'"action":"token_list"'*) printf '%s' '{"ok":true,"result":[{"id":"tok_test","name":"Laptop","expires_at":2000000000,"revoked_at":null,"last_used_at":null}]}' ;;
  *'"action":"models"'*) printf '%s' '{"ok":true,"result":{"data":[{"id":"gpt-test","object":"model","owned_by":"openai","display_name":"Test model","exetrouter":{"context_window":100000,"input_modalities":["text","image"],"supported_reasoning_levels":[{"effort":"low"}],"default_reasoning_level":"low"}},{"id":"gpt-other","object":"model","owned_by":"openai","display_name":"Other model","exetrouter":{"context_window":200000,"input_modalities":["text"],"supported_reasoning_levels":[{"effort":"high"}]}}]}}' ;;
  *'"action":"doctor"'*|*'"action":"limits"'*) printf '%s' '{"ok":true,"result":{"capabilities":{"account_routing_rules":1},"quota_accounts":[{"id":1,"label":"test@example.com","preference":{"enabled":true,"priority":1,"locked":false,"rules":{"switch_at":null,"switch_at_short":null,"switch_at_weekly":null},"settings":{}},"reset_credits":{"available_count":2,"credits":[]},"quota":{"status":"current","windows":[{"kind":"primary","window_minutes":480,"used_percent":20,"remaining_percent":80,"status":"current"},{"kind":"secondary","window_minutes":10080,"used_percent":30,"remaining_percent":70,"status":"stale"}]}}]}}' ;;
  *'"action":"account_set"'*) printf '%s' '{"ok":true,"result":{"enabled":true,"priority":-255,"locked":false,"rules":{"switch_at":20,"switch_at_short":-1,"switch_at_weekly":15},"settings":{"priority":-255,"switch_at":20,"switch_at_short":"off","switch_at_weekly":15}}}' ;;
  *'"action":"reset_prepare"'*) printf '%s' '{"ok":true,"result":{"confirmation":"synthetic-confirmation","email":"test@example.com","remaining_percent":5,"available_count":2,"credit_title":"Full reset","free_reset_at":2000000000,"recommend_wait":true,"credit_expires_at":null}}' ;;
  *'"action":"reset_confirm"'*) printf '%s' '{"ok":true,"result":{"code":"reset","windows_reset":2}}' ;;
  *'"action":"usage"'*) printf '%s' '{"ok":true,"result":{"period":"day","timezone":"Europe/Moscow","rows":[]}}' ;;
  *'"action":"token_create"'*) printf '%s' '{"ok":true,"result":{"token":{"id":"tok_created"},"secret":"SYNTHETIC_TEST_SECRET"}}' ;;
  *'"action":"token_rotate"'*) printf '%s' '{"ok":true,"result":{"token":{"id":"tok_rotated"},"secret":"SYNTHETIC_ROTATED_SECRET"}}' ;;
  *'"action":"token_revoke"'*) printf '%s' '{"ok":true,"result":{"revoked":true}}' ;;
  *) exit 1 ;;
esac
"#).unwrap();
        fs::set_permissions(dir.path().join("ssh"), fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["pbcopy", "wl-copy", "xclip", "xsel"] {
            let file = dir.path().join(name);
            fs::write(
                &file,
                r#"#!/bin/sh
[ ! -f "$EXETROUTER_TEST_CLIPBOARD_FAILURE" ] || exit 1
umask 077
cat > "$EXETROUTER_TEST_CLIPBOARD"
"#,
            )
            .unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { dir }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_exr"));
        command.env("EXR_NO_UPDATE_CHECK", "1");
        command
            .env("EXR_CONFIG", self.dir.path().join("config.json"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env(
                "EXETROUTER_TEST_REQUESTS",
                self.dir.path().join("requests.log"),
            )
            .env("TERM", "xterm-256color")
            .env("WAYLAND_DISPLAY", "synthetic-test-display")
            .env(
                "EXETROUTER_TEST_CLIPBOARD",
                self.dir.path().join("clipboard"),
            )
            .env(
                "EXETROUTER_TEST_CLIPBOARD_FAILURE",
                self.dir.path().join("clipboard-failure"),
            );
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn configured() -> Self {
        let client = Self::new();
        let key = client.dir.path().join("identity");
        let result = client.run(&[
            "configure",
            "--identity",
            key.to_str().unwrap(),
            "--host",
            "localhost",
            "--port",
            "2222",
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        client
    }
}
#[test]
fn saved_connection_removes_identity_flags_and_json_is_explicit() {
    let client = Client::configured();
    let output = client.run(&["tokens"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Laptop"));
    assert!(text.contains("Your API tokens"));
    assert!(!text.starts_with('['));
    let json = client.run(&["token", "list", "--json"]);
    assert!(json.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap()[0]["id"],
        "tok_test"
    );
    let models = client.run(&["models"]);
    let text = String::from_utf8(models.stdout).unwrap();
    assert!(text.contains("100000"));
    assert!(text.contains("gpt-test"));
    assert!(!client
        .run(&["models", "--format", "codex-json"])
        .status
        .success());
    let limits = client.run(&["limits"]);
    let text = String::from_utf8(limits.stdout).unwrap();
    assert!(text.contains("8-hour"));
    assert!(text.contains("Weekly"));
    assert!(text.contains("stale"));
    let limits = client.run(&["limits", "--json"]);
    assert!(serde_json::from_slice::<serde_json::Value>(&limits.stdout)
        .unwrap()
        .is_array());
    let before = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    assert!(!client
        .run(&["limits", "reset", "test@example.com", "--json"])
        .status
        .success());
    assert!(!client
        .run(&["limits", "reset", "test@example.com"])
        .status
        .success());
    assert_eq!(
        fs::read_to_string(client.dir.path().join("requests.log")).unwrap(),
        before
    );
    let output = client.run(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(!output.stdout.contains(&27));
    assert!(!client
        .run(&["token", "create", "--name", "test"])
        .status
        .success());
    assert!(!client
        .run(&["token", "revoke", "tok_test"])
        .status
        .success());
    assert!(client
        .run(&["token", "revoke", "tok_test", "--yes"])
        .status
        .success());
}

#[test]
fn human_dates_follow_client_timezone_without_changing_machine_timestamps() {
    let client = Client::configured();
    for (zone, expected) in [
        ("America/New_York", "Tuesday, May 17, 2033 · 23:33"),
        ("Asia/Tokyo", "Wednesday, May 18, 2033 · 12:33"),
    ] {
        let output = client
            .command()
            .env("TZ", zone)
            .args(["tokens"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains(" UTC"));
        let output = client
            .command()
            .env("TZ", zone)
            .args(["tokens", "--json"])
            .output()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value[0]["expires_at"], 2000000000);
    }
}

#[test]
fn terminal_cli_copies_without_printing_and_checks_clipboard_before_issuance() {
    use std::{
        io::Read,
        os::fd::FromRawFd,
        process::Stdio,
        time::{Duration, Instant},
    };
    fn terminal(mut command: Command) -> Output {
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut reader = unsafe { fs::File::from_raw_fd(master) };
        let writer = unsafe { fs::File::from_raw_fd(slave) };
        unsafe {
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let mut child = command
            .stdin(Stdio::from(writer.try_clone().unwrap()))
            .stdout(Stdio::from(writer.try_clone().unwrap()))
            .stderr(Stdio::from(writer.try_clone().unwrap()))
            .spawn()
            .unwrap();
        let mut stdout = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let mut bytes = [0; 8192];
            if let Ok(n) = reader.read(&mut bytes) {
                stdout.extend_from_slice(&bytes[..n]);
            }
            if let Some(status) = child.try_wait().unwrap() {
                while let Ok(n) = reader.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    stdout.extend_from_slice(&bytes[..n]);
                }
                return Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                };
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("CLI clipboard test timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let client = Client::configured();
    let mut command = client.command();
    command.args(["token", "create", "--name", "fixture"]);
    let output = terminal(command);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Secret copied to clipboard"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC_TEST_SECRET"));
    assert_eq!(
        fs::read_to_string(client.dir.path().join("clipboard")).unwrap(),
        "SYNTHETIC_TEST_SECRET"
    );
    for name in ["pbcopy", "wl-copy", "xclip", "xsel"] {
        fs::remove_file(client.dir.path().join(name)).unwrap();
    }
    let previous = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    let mut command = client.command();
    command
        .env("PATH", client.dir.path())
        .args(["token", "rotate", "tok_test"]);
    let output = terminal(command);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(client.dir.path().join("requests.log")).unwrap(),
        previous
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC_ROTATED_SECRET"));
}

#[test]
fn real_terminal_dashboard_copies_secrets_without_rendering_and_restores_screen() {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        process::Stdio,
        time::{Duration, Instant},
    };
    let client = Client::configured();
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 35,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    unsafe {
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let mut child = client
        .command()
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let mut output = Vec::new();
    // Ratatui sends cursor-addressed deltas; raw escape output does not contain
    // contiguous screen labels once styles and layouts change.
    fn wait(master: &mut fs::File, output: &mut Vec<u8>, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let mut chunk = [0u8; 8192];
            if let Ok(n) = master.read(&mut chunk) {
                output.extend_from_slice(&chunk[..n]);
            }
            if (needle.contains('\x1b') && String::from_utf8_lossy(output).contains(needle))
                || terminal_text(output).contains(needle)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("dashboard did not show expected text: {needle}");
    }
    wait(&mut master, &mut output, "Overview");
    master.write_all(b"\x1b[C\x1b[C").unwrap();
    wait(&mut master, &mut output, "Laptop");
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Create token");
    master.write_all(b"\x1b[B\x1b[B\r").unwrap();
    wait(&mut master, &mut output, "Token name");
    master.write_all(b"Test service\r").unwrap();
    wait(&mut master, &mut output, "Expires");
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Secret copied to clipboard");
    assert_eq!(
        fs::read_to_string(client.dir.path().join("clipboard")).unwrap(),
        "SYNTHETIC_TEST_SECRET"
    );
    wait(&mut master, &mut output, "Laptop");
    assert!(!String::from_utf8_lossy(&output).contains("SYNTHETIC_TEST_SECRET"));
    // Cancellation must not send a mutation; confirmation sends it once.
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Rotate token");
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Rotate");
    master.write_all(b"\x1b").unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("token_rotate"));
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Rotate token");
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Rotate");
    output.clear();
    fs::write(client.dir.path().join("clipboard-failure"), "").unwrap();
    master.write_all(b"y").unwrap();
    wait(&mut master, &mut output, "Clipboard unavailable");
    assert!(!String::from_utf8_lossy(&output).contains("SYNTHETIC_ROTATED_SECRET"));
    fs::remove_file(client.dir.path().join("clipboard-failure")).unwrap();
    output.clear();
    master.write_all(b"c").unwrap();
    wait(&mut master, &mut output, "Secret copied to clipboard");
    assert_eq!(
        fs::read_to_string(client.dir.path().join("clipboard")).unwrap(),
        "SYNTHETIC_ROTATED_SECRET"
    );
    assert!(!String::from_utf8_lossy(&output).contains("SYNTHETIC_ROTATED_SECRET"));
    wait(&mut master, &mut output, "Laptop");
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Revoke token");
    master.write_all(b"\x1b[B\r").unwrap();
    wait(&mut master, &mut output, "Revoke");
    output.clear();
    master.write_all(b"y").unwrap();
    wait(&mut master, &mut output, "revoked");
    output.clear();
    master.write_all(b"\x1b[D\x1b[D").unwrap();
    wait(&mut master, &mut output, "8h");
    wait(&mut master, &mut output, "week");
    // Cached account rows can be visible while refresh is still pending.
    // Wait for the working indicator to disappear before sending an action.
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let mut chunk = [0u8; 8192];
        if let Ok(n) = master.read(&mut chunk) {
            output.extend_from_slice(&chunk[..n]);
        }
        if !terminal_text(&output).contains("Working") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Overview refresh did not complete"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Review reset credit");
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "recommend saving");
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("reset_confirm"));
    master.write_all(b"n").unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("reset_confirm"));
    output.clear();
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "Review reset credit");
    master.write_all(b"\r").unwrap();
    wait(&mut master, &mut output, "recommend saving");
    output.clear();
    master.write_all(b"y").unwrap();
    wait(&mut master, &mut output, "Reset credit used");
    output.clear();
    master.write_all(b"q").unwrap();
    wait(&mut master, &mut output, "\x1b[?1049l");
    assert!(child.wait().unwrap().success());
    let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    assert_eq!(requests.matches("token_create").count(), 1);
    assert_eq!(requests.matches("token_rotate").count(), 1);
    assert_eq!(requests.matches("token_revoke").count(), 1);
    assert_eq!(requests.matches("reset_prepare").count(), 2);
    assert_eq!(requests.matches("reset_confirm").count(), 1);
    assert!(!requests.contains("Test service"));
    assert!(requests.contains("exrp1:"));
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()) },
        0
    );
    assert_ne!(unsafe { termios.assume_init() }.c_lflag & libc::ICANON, 0);
}

#[test]
fn operator_reports_are_human_by_default_and_json_is_opt_in() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_exrd"))
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["init"]).status.success());
    let user = run(&["admin", "user-create", "alice"]);
    assert!(user.status.success());
    assert!(String::from_utf8_lossy(&user.stdout).contains("User created"));
    let list = run(&["admin", "user-list"]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("alice"));
    assert!(!list.stdout.starts_with(b"["));
    let list = run(&["admin", "user-list", "--json"]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&list.stdout).unwrap()[0]["name"],
        "alice"
    );
    let vault = exetrouter::oauth::Vault::new(
        fs::read(dir.path().join("exetrouter.oauth.key"))
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let conn = rusqlite::Connection::open(dir.path().join("exetrouter.sqlite")).unwrap();
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"email":"alice@example.com"}"#);
    let access = format!("header.{claims}.signature");
    exetrouter::oauth::save(
        &conn,
        &vault,
        "synthetic-account",
        &exetrouter::oauth::Credentials {
            access_token: access.clone(),
            refresh_token: "synthetic-private-refresh".into(),
        },
        2000000000,
    )
    .unwrap();
    let list = run(&["admin", "oauth", "list"]);
    assert!(list.status.success());
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(text.contains("alice@example.com"));
    assert!(!text.contains(&access));
    assert!(!text.contains("synthetic-private-refresh"));
    let list = run(&["admin", "oauth", "list", "--json"]);
    assert!(list.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&list.stdout).unwrap()[0]["email"],
        "alice@example.com"
    );
}

fn terminal_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut chars = text.chars().peekable();
    let mut cells = vec![vec![' '; 120]; 35];
    let (mut row, mut col) = (0usize, 0usize);
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.next() != Some('[') {
                continue;
            }
            let mut parameters = String::new();
            let mut final_char = ' ';
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    final_char = ch;
                    break;
                }
                parameters.push(ch);
            }
            match final_char {
                'H' | 'f' => {
                    let mut parts = parameters
                        .split(';')
                        .map(|s| s.parse::<usize>().unwrap_or(1).saturating_sub(1));
                    row = parts.next().unwrap_or(0);
                    col = parts.next().unwrap_or(0);
                }
                'J' if parameters == "2" => cells.iter_mut().for_each(|r| r.fill(' ')),
                'K' if row < 35 => cells[row][col.min(120)..].fill(' '),
                _ => {}
            }
        } else if ch == '\r' {
            col = 0;
        } else if ch == '\n' {
            row += 1;
        } else if !ch.is_control() {
            if row < 35 && col < 120 {
                cells[row][col] = ch;
            }
            col += 1;
        }
    }
    cells
        .into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

struct Dashboard {
    master: fs::File,
    _slave: fs::File,
    child: std::process::Child,
    output: Vec<u8>,
}
impl Dashboard {
    fn start(mut command: Command) -> Self {
        use std::{
            os::fd::{AsRawFd, FromRawFd},
            process::Stdio,
        };
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 35,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::addr_of_mut!(size),
                )
            },
            0
        );
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        let child = command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .unwrap();
        Self {
            master,
            _slave: slave,
            child,
            output: Vec::new(),
        }
    }
    fn send(&mut self, keys: &[u8]) {
        use std::io::Write;
        self.master.write_all(keys).unwrap();
    }
    fn read(&mut self) {
        use std::io::Read;
        let mut chunk = [0; 16384];
        while let Ok(count) = self.master.read(&mut chunk) {
            if count == 0 {
                break;
            }
            self.output.extend_from_slice(&chunk[..count]);
        }
    }
    fn wait(&mut self, label: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            self.read();
            if terminal_text(&self.output).contains(label) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "dashboard exited before {label}: {}",
                terminal_text(&self.output)
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "dashboard did not show {label}: {}",
            terminal_text(&self.output)
        );
    }
    fn quit(&mut self) {
        self.send(b"q");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            self.read();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "dashboard did not stop"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
impl Drop for Dashboard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn dashboard_enter_copies_the_selected_model_and_usage_requests_last_24_hours() {
    let client = Client::configured();
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("Overview");
    dashboard.send(b"\x1b[C\x1b[C\x1b[C");
    dashboard.wait("Other model");
    dashboard.wait("Enter: copy model ID");
    dashboard.send(b"\x1b[B\r");
    dashboard.wait("Copied gpt-test to clipboard.");
    assert_eq!(
        fs::read_to_string(client.dir.path().join("clipboard")).unwrap(),
        "gpt-test"
    );
    dashboard.send(b"\x1b[D\x1b[D");
    dashboard.wait("No timeline available from this server.");
    dashboard.send(b"p");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        if fs::read_to_string(client.dir.path().join("requests.log"))
            .unwrap()
            .contains("\"period\":\"24h\"")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "dashboard did not request rolling usage"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    dashboard.quit();
}

#[test]
fn dashboard_reset_eligibility_error_requires_dismissal_and_never_confirms_a_credit() {
    let client = Client::configured();
    let ssh = client.dir.path().join("ssh");
    let original = fs::read_to_string(&ssh).unwrap();
    let rewritten = original.lines().map(|line| {
        if line.contains("action\":\"reset_prepare") {
            "  *'\"action\":\"reset_prepare\"'*) printf '%s' '{\"ok\":false,\"error\":\"Reset credits require a live subscription window with 5% or less remaining and a known future reset time\"}' ;;"
        } else {
            line
        }
    }).collect::<Vec<_>>().join("\n");
    fs::write(&ssh, rewritten).unwrap();
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("test@example.com");
    dashboard.send(b"c");
    dashboard.wait("Reset credit unavailable");
    dashboard.wait("Enter / Esc: close");
    dashboard.send(b"y\r\x1b[C\x1b[C\x1b[C");
    dashboard.wait("Enter: copy model ID");
    let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    assert_eq!(requests.matches("\"action\":\"reset_prepare\"").count(), 1);
    assert!(!requests.contains("\"action\":\"reset_confirm\""));
    dashboard.quit();
}

#[test]
fn setup_ctrl_c_cancels_every_screen_without_saving() {
    for screen in 0..3 {
        let client = Client::new();
        let state = client.dir.path().join("standalone");
        let mut command = client.command();
        command.args([
            "--state-dir",
            state.to_str().unwrap(),
            "--listen",
            "127.0.0.1:0",
        ]);
        let mut dashboard = Dashboard::start(command);
        dashboard.wait("Welcome to ExetRouter");
        if screen >= 1 {
            dashboard.send(b"\r");
            dashboard.wait("Private state directory");
        }
        if screen >= 2 {
            dashboard.send(b"\r");
            dashboard.wait("Save these settings?");
        }
        dashboard.send(b"\x03");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while dashboard.child.try_wait().unwrap().is_none() {
            dashboard.read();
            assert!(
                std::time::Instant::now() < deadline,
                "Ctrl-C did not cancel setup"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        dashboard.read();
        assert!(!client.dir.path().join("config.json").exists());
        assert!(!state.exists());
        assert!(
            String::from_utf8_lossy(&dashboard.output).contains("\x1b[?1049l"),
            "screen {screen}: {:?}",
            String::from_utf8_lossy(&dashboard.output)
        );
    }
}

#[test]
fn connection_settings_edit_in_the_middle_and_check_ssh_before_saving() {
    let client = Client::configured();
    let key = client.dir.path().join("identity");
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("Overview");
    dashboard.send(b"\x1b[D");
    dashboard.wait("Remote Server");
    dashboard.send(b"e");
    dashboard.wait("Configure ExetRouter");
    dashboard.send(b"\r");
    dashboard.wait("SSH host");
    dashboard.send(b"\x01route.test\x05\x1b[D\x1b[3~\x15router.test");
    dashboard.send(b"\x1b[B\x1b[B\x1b[B\x1b[Bhttps://api.example.com/v1");
    dashboard.send(b"\r");
    dashboard.wait("Save these settings?");
    dashboard.send(b"y");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        dashboard.read();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(client.dir.path().join("config.json")).unwrap())
                .unwrap();
        if saved["host"] == "router.test" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "connection was not saved after the SSH check"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    dashboard.quit();
    assert!(String::from_utf8_lossy(&dashboard.output).contains("Checking SSH connection."));
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(client.dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["host"], "router.test");
    assert_eq!(saved["identity"], key.to_str().unwrap());
    assert_eq!(saved["api_url"], "https://api.example.com/v1");
    let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    assert!(requests.contains("\"action\":\"doctor\""));
}

#[test]
fn first_run_wizard_saves_standalone_and_settings_work_without_ssh() {
    let client = Client::new();
    let state = client.dir.path().join("standalone");
    let mut command = client.command();
    command.args([
        "--state-dir",
        state.to_str().unwrap(),
        "--listen",
        "127.0.0.1:0",
    ]);
    let mut dashboard = Dashboard::start(command);
    dashboard.wait("Welcome to ExetRouter");
    dashboard.send(b"\r");
    dashboard.wait("Private state directory");
    dashboard.send(b"\r");
    dashboard.wait("Save these settings?");
    dashboard.send(b"y");
    dashboard.wait("Overview");
    dashboard.send(b"\x1b[D");
    dashboard.wait("LOCAL ACCOUNTS");
    dashboard.wait("No accounts yet. Add one to get started.");
    assert!(!client.dir.path().join("requests.log").exists());
    dashboard.send(b"e");
    dashboard.wait("Configure ExetRouter");
    dashboard.send(b"\r");
    dashboard.wait("Connection settings");
    dashboard.send(b"\r");
    dashboard.wait("Save these settings?");
    dashboard.send(b"y");
    dashboard.wait("SOFTWARE");
    dashboard.send(b"\r");
    dashboard.wait("Update exr");
    dashboard.send(b"\r");
    dashboard.wait("Update exr");
    dashboard.send(b"n");
    dashboard.quit();
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(client.dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["mode"], "standalone");
    assert_eq!(saved["state_dir"], state.to_str().unwrap());
    assert!(state.join("state.sqlite").exists());
    let listed = client.run(&["account", "list", "--json"]);
    assert!(listed.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed.stdout).unwrap(),
        serde_json::json!([])
    );
    let mut reopened = Dashboard::start(client.command());
    reopened.wait("Overview");
    reopened.send(b"\x1b[D");
    reopened.wait("LOCAL ACCOUNTS");
    reopened.quit();
}
#[test]
fn overview_automatically_refreshes_live_limits_without_mutations() {
    let client = Client::configured();
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("week");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(36);
    loop {
        dashboard.read();
        let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
        if requests.matches("\"action\":\"limits\"").count() >= 2 {
            assert!(!requests.contains("reset_confirm") && !requests.contains("token_create"));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Overview did not poll live limits"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    dashboard.quit();
}
#[test]
fn server_config_allows_short_admin_commands_and_cli_flags_override_it() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let config = dir.path().join("server.json");
    fs::write(&config,serde_json::json!({"db":dir.path().join("state.sqlite"),"key":dir.path().join("hmac.key"),"oauth_key":dir.path().join("oauth.key"),"control_socket":dir.path().join("control.sock")}).to_string()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_exrd"))
            .env("EXRD_CONFIG", &config)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["init"]).status.success());
    assert!(run(&["admin", "user-create", "fixture"]).status.success());
    let output = run(&["admin", "user-list", "--json"]);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()[0]["name"],
        "fixture"
    );
    let missing = dir.path().join("missing.key");
    assert!(
        !run(&["--key", missing.to_str().unwrap(), "admin", "user-list"])
            .status
            .success()
    );
}

#[test]
fn model_exports_select_native_opencode_v1_and_v2_formats() {
    let client = Client::configured();
    let v1 = client.run(&[
        "models",
        "--json",
        "--format",
        "opencode-v1-json",
        "--base-url",
        "https://api.example.com/v1/",
    ]);
    assert!(
        v1.status.success(),
        "{}",
        String::from_utf8_lossy(&v1.stderr)
    );
    let v1: serde_json::Value = serde_json::from_slice(&v1.stdout).unwrap();
    assert_eq!(
        v1["provider"]["exetrouter"]["models"]["gpt-test"]["options"]["reasoningEffort"],
        "low"
    );
    let v2 = client.run(&[
        "models",
        "--json",
        "--format",
        "opencode-v2-json",
        "--base-url",
        "https://api.example.com/v1",
    ]);
    assert!(v2.status.success());
    let v2: serde_json::Value = serde_json::from_slice(&v2.stdout).unwrap();
    assert_eq!(
        v2["providers"]["exetrouter"]["models"]["gpt-test"]["variants"][0]["id"],
        "low"
    );
    assert!(v1.get("providers").is_none());
    assert!(v2.get("provider").is_none());
    let one = &v1["provider"]["exetrouter"];
    let two = &v2["providers"]["exetrouter"];
    assert_eq!(one["name"], "ExetRouter");
    assert_eq!(two["name"], "ExetRouter");
    assert_eq!(one["options"]["baseURL"], "https://api.example.com/v1");
    assert_eq!(two["settings"]["baseURL"], one["options"]["baseURL"]);
    assert_eq!(one["options"]["apiKey"], "{env:EXETROUTER_TOKEN}");
    assert!(one.get("whitelist").is_none());
    assert!(two.get("canonical").is_none());
    assert_eq!(one["models"].as_object().unwrap().len(), 2);
    assert_eq!(two["models"].as_object().unwrap().len(), 2);
    assert!(v1["provider"].get("openai").is_none());
    assert!(v2["providers"].get("openai").is_none());
}

#[test]
fn opencode_connection_validation_precedes_catalog_requests() {
    let client = Client::configured();
    assert!(!client
        .run(&["models", "--json", "--format", "opencode-v1-json"])
        .status
        .success());
    for url in [
        "ftp://api.example.com/v1",
        "https://user:SYNTHETIC_SECRET@api.example.com/v1",
        "https://api.example.com/v1?token=SYNTHETIC_SECRET",
        "https://api.example.com/v1#fragment",
        "https://api.example.com",
    ] {
        let result = client.run(&[
            "models",
            "--json",
            "--format",
            "opencode-v2-json",
            "--base-url",
            url,
        ]);
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("SYNTHETIC_SECRET"));
    }
    assert!(!client
        .run(&[
            "models",
            "--json",
            "--format",
            "codex-json",
            "--base-url",
            "https://api.example.com/v1"
        ])
        .status
        .success());
    assert!(!client.dir.path().join("requests.log").exists());
}

#[test]
fn opencode_exports_reuse_saved_api_url_and_select_only_catalog_models() {
    let client = Client::configured();
    assert!(client
        .run(&["configure", "--api-url", "https://api.example.com/v1/"])
        .status
        .success());
    let v1 = client.run(&[
        "models",
        "--json",
        "--format",
        "opencode-v1-json",
        "--model",
        "exetrouter/gpt-test",
    ]);
    assert!(
        v1.status.success(),
        "{}",
        String::from_utf8_lossy(&v1.stderr)
    );
    let v1: serde_json::Value = serde_json::from_slice(&v1.stdout).unwrap();
    assert_eq!(
        v1["provider"]["exetrouter"]["options"]["baseURL"],
        "https://api.example.com/v1"
    );
    assert_eq!(v1["model"], "exetrouter/gpt-test");
    let v2 = client.run(&[
        "models",
        "--json",
        "--format",
        "opencode-v2-json",
        "--model",
        "exetrouter/gpt-other",
        "--base-url",
        "http://127.0.0.1:8787/v1",
    ]);
    assert!(v2.status.success());
    let v2: serde_json::Value = serde_json::from_slice(&v2.stdout).unwrap();
    assert_eq!(
        v2["providers"]["exetrouter"]["settings"]["baseURL"],
        "http://127.0.0.1:8787/v1"
    );
    assert_eq!(
        v2["model"],
        serde_json::json!({"providerID":"exetrouter","model":"gpt-other"})
    );
    let default = client.run(&["models", "--json", "--format", "opencode-v2-json"]);
    assert!(default.status.success());
    assert!(serde_json::from_slice::<serde_json::Value>(&default.stdout)
        .unwrap()
        .get("model")
        .is_none());
    assert!(!client
        .run(&[
            "models",
            "--json",
            "--format",
            "opencode-v2-json",
            "--model",
            "exetrouter/unavailable-model"
        ])
        .status
        .success());
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(client.dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["host"], "localhost");
    assert_eq!(saved["api_url"], "https://api.example.com/v1");
}

#[test]
fn account_cli_serializes_numeric_rules_and_rejects_invalid_values_before_mutation() {
    let client = Client::configured();
    let output = client.run(&[
        "account",
        "set",
        "1",
        "--priority",
        "-255",
        "--switch-at",
        "20",
        "--switch-at-short",
        "off",
        "--switch-at-weekly",
        "15",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    let mutation: serde_json::Value = serde_json::from_str(
        requests
            .lines()
            .find(|line| line.contains("account_set"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(mutation["routing"]["priority"], -255);
    assert_eq!(mutation["routing"]["switch_at"], 20);
    assert_eq!(mutation["routing"]["switch_at_short"], "off");
    assert_eq!(mutation["routing"]["switch_at_weekly"], 15);
    for (flag, value) in [
        ("--priority", "256"),
        ("--switch-at", "101"),
        ("--priority", "off"),
    ] {
        assert!(!client
            .run(&["account", "set", "1", flag, value])
            .status
            .success());
    }
    assert_eq!(
        fs::read_to_string(client.dir.path().join("requests.log")).unwrap(),
        requests
    );
}

#[test]
fn dashboard_edits_rules_validates_and_cancels_without_mutating() {
    let client = Client::configured();
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("Priority 1");
    std::thread::sleep(std::time::Duration::from_millis(200));
    dashboard.send(b"S");
    dashboard.wait("Switching rules");
    dashboard.send(b"\x15256\r");
    dashboard.wait("Priority must be between");
    dashboard.send(b"\x1b");
    dashboard.wait("Priority 1");
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("account_set"));
    dashboard.send(b"S");
    dashboard.wait("Switching rules");
    dashboard.send(b"\x15-255\t\x1520\t\x15off\t\x1515\r");
    dashboard.wait("Account preferences saved");
    let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    let mutations = requests
        .lines()
        .filter(|line| line.contains("account_set"))
        .collect::<Vec<_>>();
    assert_eq!(mutations.len(), 1);
    let mutation: serde_json::Value = serde_json::from_str(mutations[0]).unwrap();
    assert_eq!(mutation["routing"]["priority"], -255);
    assert_eq!(mutation["routing"]["switch_at"], 20);
    assert_eq!(mutation["routing"]["switch_at_short"], "off");
    assert_eq!(mutation["routing"]["switch_at_weekly"], 15);
    dashboard.quit();
}

#[test]
fn unsupported_remote_rules_are_explained_without_sending_a_mutation() {
    let client = Client::configured();
    let path = client.dir.path().join("ssh");
    let script = fs::read_to_string(&path)
        .unwrap()
        .replace("\"capabilities\":{\"account_routing_rules\":1},", "");
    fs::write(&path, script).unwrap();
    let output = client.run(&["account", "set", "1", "--switch-at", "20"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("updated server and SSH gateway"));
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("account_set"));
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("Priority 1");
    std::thread::sleep(std::time::Duration::from_millis(200));
    dashboard.send(b"S");
    dashboard.wait("Server update required");
    dashboard.send(b"\x1b");
    dashboard.wait("Priority 1");
    dashboard.quit();
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("account_set"));
    assert!(client
        .run(&["account", "set", "1", "--enabled", "false"])
        .status
        .success());
    let requests = fs::read_to_string(client.dir.path().join("requests.log")).unwrap();
    let mutation: serde_json::Value = serde_json::from_str(
        requests
            .lines()
            .find(|line| line.contains("account_set"))
            .unwrap(),
    )
    .unwrap();
    assert!(mutation.get("routing").is_none());
}

#[test]
fn dashboard_locked_rules_remain_visible_and_cannot_be_saved() {
    let client = Client::configured();
    let path = client.dir.path().join("ssh");
    let script = fs::read_to_string(&path)
        .unwrap()
        .replace("\"locked\":false", "\"locked\":true");
    fs::write(&path, script).unwrap();
    let mut dashboard = Dashboard::start(client.command());
    dashboard.wait("Priority 1");
    std::thread::sleep(std::time::Duration::from_millis(200));
    dashboard.send(b"S");
    dashboard.wait("Locked by server operator");
    dashboard.send(b"\x1520\r");
    dashboard.wait("Priority 1");
    dashboard.quit();
    assert!(!fs::read_to_string(client.dir.path().join("requests.log"))
        .unwrap()
        .contains("account_set"));
}
