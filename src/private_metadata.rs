use crate::Result;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};

pub(crate) const PREFIX: &str = "exrp1:";

// The service validates framing only. It never has a user decryption key.
pub(crate) fn validate_envelope(value: &str, limit: usize) -> Result<()> {
    if value.len() > limit {
        return Err("private ciphertext too large".into());
    }
    let bytes = STANDARD_NO_PAD
        .decode(
            value
                .strip_prefix(PREFIX)
                .ok_or("invalid private ciphertext")?,
        )
        .map_err(|_| "invalid private ciphertext")?;
    if bytes.len() < 40 {
        return Err("invalid private ciphertext".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{control, create_user, ControlRequest};
    #[test]
    fn storage_is_user_scoped_bounded_and_preserves_token_rotation() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::init(&db).unwrap();
        let alice = create_user(&db, "alice").unwrap();
        let bob = create_user(&db, "bob").unwrap();
        let envelope = format!("{PREFIX}{}", STANDARD_NO_PAD.encode([7; 64]));
        let id = "a".repeat(64);
        control(
            &mut db,
            &[1; 32],
            alice,
            ControlRequest::PrivatePut {
                id: id.clone(),
                ciphertext: Some(envelope.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            control(&mut db, &[1; 32], bob, ControlRequest::PrivateList).unwrap(),
            serde_json::json!({})
        );
        control(
            &mut db,
            &[1; 32],
            bob,
            ControlRequest::PrivatePut {
                id: id.clone(),
                ciphertext: None,
            },
        )
        .unwrap();
        assert_eq!(
            control(&mut db, &[1; 32], alice, ControlRequest::PrivateList).unwrap()[&id],
            envelope
        );
        assert!(control(
            &mut db,
            &[1; 32],
            alice,
            ControlRequest::PrivatePut {
                id: id.clone(),
                ciphertext: Some("plaintext private marker".into())
            }
        )
        .is_err());
        let token = crate::create_token(&db, &[1; 32], alice, &envelope, 90).unwrap();
        assert!(control(
            &mut db,
            &[1; 32],
            bob,
            ControlRequest::TokenRename {
                id: token.token.id.clone(),
                name: envelope.clone()
            }
        )
        .is_err());
        let rotated = crate::rotate_token(&mut db, &[1; 32], alice, &token.token.id).unwrap();
        assert_eq!(rotated.token.name, envelope);
        for index in 0..127 {
            control(
                &mut db,
                &[1; 32],
                alice,
                ControlRequest::PrivatePut {
                    id: format!("{index:064x}"),
                    ciphertext: Some(envelope.clone()),
                },
            )
            .unwrap();
        }
        assert!(control(
            &mut db,
            &[1; 32],
            alice,
            ControlRequest::PrivatePut {
                id: "b".repeat(64),
                ciphertext: Some(envelope.clone())
            }
        )
        .is_err());
        control(
            &mut db,
            &[1; 32],
            alice,
            ControlRequest::PrivatePut {
                id,
                ciphertext: None,
            },
        )
        .unwrap();
        control(
            &mut db,
            &[1; 32],
            alice,
            ControlRequest::PrivatePut {
                id: "b".repeat(64),
                ciphertext: Some(envelope),
            },
        )
        .unwrap();
    }
}
