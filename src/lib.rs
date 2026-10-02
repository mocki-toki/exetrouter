#[cfg(test)]
extern crate self as exetrouter;
pub mod account_preferences;
mod affinity;
mod auth;
pub mod backup;
mod cache;
pub mod catalog;
mod chat;
pub mod client;
mod control;
pub mod doctor;
mod health;
mod local;
pub mod oauth;
mod payload;
mod pool;
pub mod quota;
pub mod reset;
pub mod server;
pub mod server_config;
pub mod store;
pub mod update;
pub mod upstream;
mod usage;

pub use auth::*;
pub use control::{control, ControlRequest};
use serde::Serialize;
pub use store::init;
pub use usage::{record_usage, usage_report, UsageEvent, UsageReport, UsageRow};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct IssuedToken {
    pub token: TokenInfo,
    pub secret: String,
}

#[derive(Debug, Serialize)]
pub struct SshIdentity {
    pub id: String,
    pub user_id: i64,
    pub fingerprint: String,
    pub public_key: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use rusqlite::Connection;
    #[test]
    fn token_is_one_time_and_scoped() {
        let mut db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let a = create_user(&db, "alice").unwrap();
        let b = create_user(&db, "bob").unwrap();
        let key = [7u8; 32];
        let t = create_token(&db, &key, a, "laptop", 90).unwrap();
        assert_eq!(authenticate(&db, &key, &t.secret).unwrap().unwrap().0, a);
        assert!(authenticate(&db, &[8u8; 32], &t.secret).unwrap().is_none());
        let mut forged = t.secret.clone();
        forged.replace_range(forged.len() - 1.., "0");
        if forged == t.secret {
            forged.replace_range(forged.len() - 1.., "1");
        }
        assert!(authenticate(&db, &key, &forged).unwrap().is_none());
        assert!(token_info(&db, b, &t.token.id).unwrap().is_none());
        assert!(
            !serde_json::to_string(&token_info(&db, a, &t.token.id).unwrap())
                .unwrap()
                .contains(&t.secret)
        );
        let rotated = rotate_token(&mut db, &key, a, &t.token.id).unwrap();
        assert_ne!(rotated.secret, t.secret);
        assert!(revoke_token(&db, a, &t.token.id).unwrap());
        let listed = list_tokens(&db, a).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, rotated.token.id);
        assert!(token_info(&db, a, &t.token.id)
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some());
        assert!(authenticate(&db, &key, &t.secret).unwrap().is_none());
        assert_eq!(
            authenticate(&db, &key, &rotated.secret).unwrap().unwrap().0,
            a
        );
    }
    #[test]
    fn usage_unknown_not_zero_request() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "alice").unwrap();
        let t = create_token(&db, &[1; 32], user, "dev", 90).unwrap();
        record_usage(
            &db,
            UsageEvent {
                user_id: user,
                token_id: &t.token.id,
                surface: "responses",
                model: "test-model",
                status: "failed",
                input: None,
                output: None,
                cached_input: None,
                reasoning_output: None,
            },
        )
        .unwrap();
        let report = usage_report(&db, "day", Some("user")).unwrap();
        assert_eq!(report.rows[0].unknown_usage, 1);
        assert_eq!(report.rows[0].requests, 1);
    }

    fn test_public_key() -> String {
        let mut blob = Vec::new();
        blob.extend_from_slice(&11u32.to_be_bytes());
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&[42u8; 32]);
        format!("ssh-ed25519 {} alice@laptop\n", BASE64.encode(blob))
    }

    #[test]
    fn ssh_identity_is_bound_and_revocable() {
        let mut db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let alice = create_user(&db, "alice").unwrap();
        let bob = create_user(&db, "bob").unwrap();
        let key = register_ssh_key(&db, alice, &test_public_key()).unwrap();
        assert_eq!(resolve_ssh_identity(&db, &key.id).unwrap(), Some(alice));
        let owner = resolve_ssh_identity(&db, &key.id).unwrap().unwrap();
        let issued = control(
            &mut db,
            &[9; 32],
            owner,
            ControlRequest::TokenCreate {
                name: "laptop".into(),
                expires_days: None,
            },
        )
        .unwrap();
        let token_id = issued["token"]["id"].as_str().unwrap();
        assert!(token_info(&db, bob, token_id).unwrap().is_none());
        let lines =
            authorized_keys(&db, "/usr/local/bin/exrd", "/run/exetrouter/control.sock").unwrap();
        assert!(lines.starts_with("restrict,command=\"/usr/local/bin/exrd --control-socket /run/exetrouter/control.sock gateway --identity key_"));
        assert!(lines.contains(&key.public_key));
        assert!(!lines.contains("--user-id"));
        assert!(revoke_ssh_key(&db, &key.id).unwrap());
        assert_eq!(resolve_ssh_identity(&db, &key.id).unwrap(), None);
        assert!(
            authorized_keys(&db, "/usr/local/bin/exrd", "/run/exetrouter/control.sock")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn ssh_key_and_command_injection_are_rejected() {
        assert!(parse_ssh_public_key("ssh-ed25519 AAAA\ncommand=evil").is_err());
        assert!(parse_ssh_public_key("ssh-rsa AAAA").is_err());
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        assert!(authorized_keys(
            &db,
            "/usr/local/bin/exrd;evil",
            "/run/exetrouter/control.sock"
        )
        .is_err());
    }
}
