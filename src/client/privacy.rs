use super::{config::Connection, raw_request, Session};
use crate::{private_metadata::PREFIX, ControlRequest, Result};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Account,
    Note,
    Project,
    Settings,
}

#[derive(Serialize, Deserialize)]
struct Object {
    kind: Kind,
    id: String,
    value: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dashboard {
    #[serde(default)]
    pub tab: usize,
    #[serde(default)]
    pub period: usize,
    #[serde(default)]
    pub group: usize,
    #[serde(default)]
    pub request_metric: bool,
}
impl Dashboard {
    fn validate(&self) -> Result<()> {
        if self.tab >= 5 || self.period >= 4 || self.group >= 4 {
            return Err("invalid dashboard preferences".into());
        }
        Ok(())
    }
}

fn key_path(connection: &Connection) -> Result<&Path> {
    connection
        .privacy_key
        .as_deref()
        .ok_or_else(|| "privacy key path is unavailable".into())
}
fn read_key(path: &Path) -> Result<[u8; 32]> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "privacy key unavailable; restore it from a protected backup")?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() != 32
    {
        return Err("privacy key must be an owner-only regular 32-byte file".into());
    }
    let mut key = [0; 32];
    file.read_exact(&mut key)?;
    Ok(key)
}
fn initialize(connection: &Connection) -> Result<()> {
    let path = key_path(connection)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "cannot create privacy key")?;
    let mut key = [0; 32];
    OsRng.fill_bytes(&mut key);
    file.write_all(&key)?;
    file.sync_all()?;
    Ok(())
}

fn ensure_key(connection: &Connection) -> Result<[u8; 32]> {
    let path = key_path(connection)?;
    if path.exists() {
        return read_key(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match initialize(connection) {
        Ok(()) => read_key(path),
        Err(_) if path.exists() => read_key(path),
        Err(error) => Err(error),
    }
}

struct Vault {
    key: [u8; 32],
    scope: Vec<u8>,
}
impl Vault {
    fn open(connection: &Connection) -> Result<Self> {
        let scope = if connection.mode == super::config::Mode::Standalone {
            serde_json::to_vec(&"standalone")?
        } else {
            serde_json::to_vec(&(
                "remote",
                &connection.host,
                connection.port,
                &connection.ssh_user,
            ))?
        };
        Ok(Self {
            key: ensure_key(connection)?,
            scope,
        })
    }
    fn aad(&self, context: &str) -> Vec<u8> {
        let mut aad = b"exetrouter/private/v1\0".to_vec();
        aad.extend(&self.scope);
        aad.push(0);
        aad.extend(context.as_bytes());
        aad
    }
    fn object_id(&self, kind: Kind, id: &str) -> Result<String> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)?;
        mac.update(&self.aad("object-id"));
        mac.update(&serde_json::to_vec(&(kind, id))?);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }
    fn encrypt(&self, context: &str, plaintext: &[u8]) -> Result<String> {
        let mut nonce = [0; 24];
        OsRng.fill_bytes(&mut nonce);
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let encrypted = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &self.aad(context),
                },
            )
            .map_err(|_| "private encryption failed")?;
        let mut bytes = nonce.to_vec();
        bytes.extend(encrypted);
        Ok(format!("{PREFIX}{}", STANDARD_NO_PAD.encode(bytes)))
    }
    fn decrypt(&self, context: &str, ciphertext: &str) -> Result<Vec<u8>> {
        crate::private_metadata::validate_envelope(ciphertext, 16384)?;
        let bytes = STANDARD_NO_PAD
            .decode(&ciphertext[PREFIX.len()..])
            .map_err(|_| "invalid private ciphertext")?;
        XChaCha20Poly1305::new((&self.key).into())
            .decrypt(
                XNonce::from_slice(&bytes[..24]),
                Payload {
                    msg: &bytes[24..],
                    aad: &self.aad(context),
                },
            )
            .map_err(|_| {
                "private data cannot be decrypted with this key and connection profile".into()
            })
    }
    fn names(&self, value: &mut Value) -> Result<()> {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.names(item)?;
                }
            }
            Value::Object(map) => {
                if let Some(name) = map.get_mut("name") {
                    if let Some(ciphertext) = name.as_str().filter(|name| name.starts_with(PREFIX))
                    {
                        *name = String::from_utf8(self.decrypt("token-name", ciphertext)?)
                            .map_err(|_| "invalid private name")?
                            .into();
                    }
                }
                for child in map.values_mut() {
                    self.names(child)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub(super) async fn request(args: &Session, mut request: ControlRequest) -> Result<Value> {
    if !matches!(
        request,
        ControlRequest::TokenCreate { .. }
            | ControlRequest::TokenList
            | ControlRequest::TokenShow { .. }
            | ControlRequest::TokenRotate { .. }
            | ControlRequest::Limits
            | ControlRequest::Doctor
    ) {
        return raw_request(args, request).await;
    }
    let vault = if args.connection.privacy_key.is_some() {
        match Vault::open(&args.connection) {
            Ok(vault) => Some(vault),
            Err(error)
                if matches!(
                    request,
                    ControlRequest::TokenCreate { .. } | ControlRequest::TokenRotate { .. }
                ) =>
            {
                return Err(error)
            }
            Err(_) => None,
        }
    } else {
        None
    };
    if let ControlRequest::TokenRotate { id } = &request {
        let mut token = raw_request(args, ControlRequest::TokenShow { id: id.clone() }).await?;
        if token["name"]
            .as_str()
            .is_some_and(|name| name.starts_with(PREFIX))
        {
            vault
                .as_ref()
                .ok_or("privacy key unavailable; encrypted token cannot be rotated")?
                .names(&mut token)?;
        }
    }
    if let ControlRequest::TokenCreate { name, .. } = &mut request {
        if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
            return Err("invalid token name".into());
        }
        *name = vault
            .as_ref()
            .ok_or("a saved client profile is required to create encrypted token names")?
            .encrypt("token-name", name.as_bytes())?;
    }
    let aliases = matches!(request, ControlRequest::Limits | ControlRequest::Doctor);
    let token_list = matches!(request, ControlRequest::TokenList);
    let token_names = matches!(
        request,
        ControlRequest::TokenList
            | ControlRequest::TokenShow { .. }
            | ControlRequest::TokenRotate { .. }
            | ControlRequest::TokenCreate { .. }
    );
    let mut result = raw_request(args, request).await?;
    if token_names {
        if token_list {
            // Verify every existing envelope before migrating any plaintext names.
            // A wrong device key must not leave a list encrypted with mixed keys.
            if let Some(vault) = vault.as_ref() {
                let mut verified = result.clone();
                vault.names(&mut verified)?;
            }
            if let Some(rows) = result.as_array_mut() {
                for row in rows {
                    let Some(name) = row["name"].as_str() else {
                        continue;
                    };
                    if name.starts_with(PREFIX) {
                        continue;
                    }
                    let Some(vault) = vault.as_ref() else {
                        continue;
                    };
                    let id = row["id"].as_str().ok_or("invalid token ID")?.to_owned();
                    let ciphertext = vault.encrypt("token-name", name.as_bytes())?;
                    raw_request(
                        args,
                        ControlRequest::TokenRename {
                            id: id.clone(),
                            name: ciphertext.clone(),
                        },
                    )
                    .await?;
                    row["name"] = ciphertext.into();
                }
            }
        }
        if let Some(vault) = vault.as_ref() {
            vault.names(&mut result)?;
        } else {
            hide_encrypted_names(&mut result);
        }
    }
    if aliases {
        if let Some(vault) = vault.as_ref() {
            let objects = raw_request(args, ControlRequest::PrivateList).await?;
            decorate(vault, &objects, &mut result)?;
        }
    }
    Ok(result)
}
fn hide_encrypted_names(value: &mut Value) {
    match value {
        Value::Array(rows) => {
            for row in rows {
                hide_encrypted_names(row)
            }
        }
        Value::Object(map) => {
            if map
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.starts_with(PREFIX))
            {
                map.insert("name".into(), "[Encrypted name]".into());
            }
            for child in map.values_mut() {
                hide_encrypted_names(child);
            }
        }
        _ => {}
    }
}
fn decorate(vault: &Vault, objects: &Value, value: &mut Value) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                decorate(vault, objects, item)?;
            }
        }
        Value::Object(map) => {
            // Diagnostic account rows contain an email and a numeric account identifier.
            if map.contains_key("email") || (map.contains_key("label") && map.contains_key("quota"))
            {
                if let Some(id) = map
                    .get("id")
                    .or_else(|| map.get("account_id"))
                    .and_then(Value::as_i64)
                {
                    let object_id = vault.object_id(Kind::Account, &id.to_string())?;
                    if let Some(ciphertext) = objects[&object_id].as_str() {
                        let object: Object =
                            serde_json::from_slice(&vault.decrypt(&object_id, ciphertext)?)
                                .map_err(|_| "invalid private object")?;
                        map.insert("display_name".into(), object.value.trim().into());
                    }
                }
            }
            for child in map.values_mut() {
                decorate(vault, objects, child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn seal(
    connection: &Connection,
    kind: Kind,
    id: &str,
    value: &str,
) -> Result<ControlRequest> {
    let vault = Vault::open(connection)?;
    let object_id = vault.object_id(kind, id)?;
    if value.len() > 8192 {
        return Err("personal text exceeds 8192 bytes".into());
    }
    if matches!(kind, Kind::Account | Kind::Project)
        && !value.is_empty()
        && (value.trim().is_empty() || value.len() > 256)
    {
        return Err("label must be 1–256 bytes".into());
    }
    let ciphertext = if value.is_empty() {
        None
    } else {
        Some(vault.encrypt(
            &object_id,
            &serde_json::to_vec(&Object {
                kind,
                id: id.into(),
                value: value.into(),
            })?,
        )?)
    };
    Ok(ControlRequest::PrivatePut {
        id: object_id,
        ciphertext,
    })
}

pub(super) async fn get(args: &Session, kind: Kind, id: &str) -> Result<Option<String>> {
    let vault = Vault::open(&args.connection)?;
    let object_id = vault.object_id(kind, id)?;
    let values = raw_request(args, ControlRequest::PrivateList).await?;
    let Some(ciphertext) = values[&object_id].as_str() else {
        return Ok(None);
    };
    let value: Object = serde_json::from_slice(&vault.decrypt(&object_id, ciphertext)?)
        .map_err(|_| "invalid personal data")?;
    Ok(Some(value.value))
}

pub(super) async fn local_accounts(args: &Session) -> Result<Value> {
    let mut value = args
        .connection
        .local
        .as_ref()
        .ok_or("standalone service unavailable")?
        .accounts()
        .await?;
    if let Ok(vault) = Vault::open(&args.connection) {
        let objects = raw_request(args, ControlRequest::PrivateList).await?;
        decorate(&vault, &objects, &mut value)?;
    }
    Ok(value)
}

pub(super) async fn dashboard(args: &Session) -> Result<Option<Dashboard>> {
    let vault = Vault::open(&args.connection)?;
    let id = vault.object_id(Kind::Settings, "dashboard")?;
    let objects = raw_request(args, ControlRequest::PrivateList).await?;
    let Some(ciphertext) = objects[&id].as_str() else {
        return Ok(None);
    };
    let object: Object = serde_json::from_slice(&vault.decrypt(&id, ciphertext)?)
        .map_err(|_| "invalid private object")?;
    let settings: Dashboard =
        serde_json::from_str(&object.value).map_err(|_| "invalid dashboard preferences")?;
    settings.validate()?;
    Ok(Some(settings))
}

pub(super) async fn save_dashboard(args: &Session, settings: &Dashboard) -> Result<()> {
    settings.validate()?;
    let vault = Vault::open(&args.connection)?;
    let id = vault.object_id(Kind::Settings, "dashboard")?;
    let ciphertext = vault.encrypt(
        &id,
        &serde_json::to_vec(&Object {
            kind: Kind::Settings,
            id: "dashboard".into(),
            value: serde_json::to_string(settings)?,
        })?,
    )?;
    raw_request(
        args,
        ControlRequest::PrivatePut {
            id,
            ciphertext: Some(ciphertext),
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_client_encrypts_before_storage_and_restores_dashboard() {
        let dir = tempfile::tempdir().unwrap();
        let local = crate::local::Local::open(
            &dir.path().join("state"),
            "127.0.0.1:0".parse().unwrap(),
            false,
        )
        .await
        .unwrap();
        let connection = Connection {
            mode: super::super::config::Mode::Standalone,
            privacy_key: Some(dir.path().join("privacy-key")),
            local: Some(local.clone()),
            ..Default::default()
        };
        initialize(&connection).unwrap();
        let session = Session {
            connection,
            json: false,
            command: super::super::CommandLine::Doctor,
        };
        let issued = request(
            &session,
            ControlRequest::TokenCreate {
                name: "synthetic private label".into(),
                expires_days: Some(1),
            },
        )
        .await
        .unwrap();
        assert_eq!(issued["token"]["name"], "synthetic private label");
        let raw = raw_request(&session, ControlRequest::TokenList)
            .await
            .unwrap();
        assert!(!raw.to_string().contains("synthetic private label"));
        assert!(raw[0]["name"].as_str().unwrap().starts_with(PREFIX));
        save_dashboard(
            &session,
            &Dashboard {
                tab: 2,
                period: 1,
                group: 3,
                request_metric: true,
            },
        )
        .await
        .unwrap();
        let restored = dashboard(&session).await.unwrap().unwrap();
        assert_eq!(
            (
                restored.tab,
                restored.period,
                restored.group,
                restored.request_metric
            ),
            (2, 1, 3, true)
        );
        let objects = raw_request(&session, ControlRequest::PrivateList)
            .await
            .unwrap();
        assert!(!objects.to_string().contains("request_metric"));
        let mut wrong = session.connection.clone();
        wrong.privacy_key = Some(dir.path().join("other-key"));
        initialize(&wrong).unwrap();
        let wrong = Session {
            connection: wrong,
            json: false,
            command: super::super::CommandLine::Doctor,
        };
        assert!(request(
            &wrong,
            ControlRequest::TokenRotate {
                id: issued["token"]["id"].as_str().unwrap().into()
            }
        )
        .await
        .is_err());
        assert_eq!(
            raw_request(&session, ControlRequest::TokenList)
                .await
                .unwrap(),
            raw
        );
        raw_request(
            &session,
            ControlRequest::TokenCreate {
                name: "legacy plaintext name".into(),
                expires_days: Some(1),
            },
        )
        .await
        .unwrap();
        let before = raw_request(&session, ControlRequest::TokenList)
            .await
            .unwrap();
        assert!(request(&wrong, ControlRequest::TokenList).await.is_err());
        assert_eq!(
            raw_request(&session, ControlRequest::TokenList)
                .await
                .unwrap(),
            before
        );
        let mut unavailable = session.connection.clone();
        unavailable.privacy_key = Some(dir.path().to_path_buf());
        let unavailable = Session {
            connection: unavailable,
            json: false,
            command: super::super::CommandLine::Doctor,
        };
        assert!(request(&unavailable, ControlRequest::Models).await.is_ok());
        assert!(request(
            &unavailable,
            ControlRequest::TokenRevoke {
                id: issued["token"]["id"].as_str().unwrap().into()
            },
        )
        .await
        .unwrap()["revoked"]
            .as_bool()
            .unwrap());
        let vault = Vault::open(&session.connection).unwrap();
        let id = vault.object_id(Kind::Account, "1").unwrap();
        let ciphertext = vault
            .encrypt(
                &id,
                &serde_json::to_vec(&Object {
                    kind: Kind::Account,
                    id: "1".into(),
                    value: "Private account label".into(),
                })
                .unwrap(),
            )
            .unwrap();
        let objects = serde_json::json!({id:ciphertext});
        let mut reply =
            serde_json::json!({"quota_accounts":[{"id":1,"label":"Issuer label","quota":{}}]});
        decorate(&vault, &objects, &mut reply).unwrap();
        assert_eq!(
            reply["quota_accounts"][0]["display_name"],
            "Private account label"
        );
        local.stop().await.unwrap();
    }
    #[test]
    fn ciphertext_rejects_wrong_key_scope_context_and_modification() {
        let vault = Vault {
            key: [7; 32],
            scope: b"fixture".to_vec(),
        };
        let value = vault.encrypt("token-name", b"private marker").unwrap();
        assert!(!value.contains("private marker"));
        assert_eq!(
            vault.decrypt("token-name", &value).unwrap(),
            b"private marker"
        );
        assert!(vault.decrypt("other", &value).is_err());
        assert!(Vault {
            key: [8; 32],
            scope: vault.scope.clone()
        }
        .decrypt("token-name", &value)
        .is_err());
        assert!(Vault {
            key: vault.key,
            scope: b"other".to_vec()
        }
        .decrypt("token-name", &value)
        .is_err());
        let mut bytes = STANDARD_NO_PAD.decode(&value[PREFIX.len()..]).unwrap();
        bytes[25] ^= 1;
        assert!(vault
            .decrypt(
                "token-name",
                &format!("{PREFIX}{}", STANDARD_NO_PAD.encode(bytes))
            )
            .is_err());
        assert_ne!(
            value,
            vault.encrypt("token-name", b"private marker").unwrap()
        );
    }
    #[test]
    fn key_files_reject_public_permissions_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let connection = Connection {
            privacy_key: Some(path.clone()),
            ..Default::default()
        };
        initialize(&connection).unwrap();
        assert!(read_key(&path).is_ok());
        assert!(initialize(&connection).is_err());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_key(&link).is_err());
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_key(&path).is_err());
    }
}
