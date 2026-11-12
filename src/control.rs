use crate::{create_token, list_tokens, revoke_token, rotate_token, token_info, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequest {
    TokenCreate {
        name: String,
        expires_days: Option<i64>,
    },
    TokenList,
    TokenRename {
        id: String,
        name: String,
    },
    /// None inherits the operator setting; true/false override it for this token.
    TokenFastMode {
        id: String,
        enabled: Option<bool>,
    },
    TokenSetEnabled {
        id: String,
        enabled: bool,
    },
    PrivateList,
    PrivatePut {
        id: String,
        ciphertext: Option<String>,
    },
    TokenShow {
        id: String,
    },
    TokenRotate {
        id: String,
    },
    TokenRevoke {
        id: String,
    },
    Usage {
        period: String,
        by: Option<String>,
        #[serde(default)]
        timezone: Option<String>,
    },
    Models,
    UsageRange {
        period: String,
        by: Option<String>,
        timezone: Option<String>,
        before: Option<i64>,
    },
    QuotaHistory {
        account: i64,
        period: String,
        before: Option<i64>,
    },
    Doctor,
    Limits,
    ResetPrepare {
        account: i64,
    },
    ResetConfirm {
        confirmation: String,
    },
}

pub fn control(
    conn: &mut Connection,
    key: &[u8],
    user_id: i64,
    req: ControlRequest,
) -> Result<serde_json::Value> {
    let value = match req {
        ControlRequest::TokenCreate { name, expires_days } => serde_json::to_value(create_token(
            conn,
            key,
            user_id,
            &name,
            expires_days.unwrap_or(90),
        )?)?,
        ControlRequest::TokenList => serde_json::to_value(list_tokens(conn, user_id)?)?,
        ControlRequest::TokenRename { id, name } => {
            crate::auth::validate_token_name(&name)?;
            if conn.execute(
                "UPDATE access_tokens SET name=?1 WHERE id=?2 AND user_id=?3",
                rusqlite::params![name, id, user_id],
            )? != 1
            {
                return Err("token not found".into());
            }
            serde_json::json!({"updated":true})
        }
        ControlRequest::TokenFastMode { id, enabled } => {
            if conn.execute("UPDATE access_tokens SET fast_mode=?1 WHERE id=?2 AND user_id=?3 AND revoked_at IS NULL AND expires_at>?4", rusqlite::params![enabled,id,user_id,chrono::Utc::now().timestamp()])? != 1 {
                return Err("token not found or inactive".into());
            }
            serde_json::json!({"updated":true,"fast_mode":enabled})
        }
        ControlRequest::TokenSetEnabled { id, enabled } => {
            if conn.execute("UPDATE access_tokens SET disabled=?1 WHERE id=?2 AND user_id=?3 AND revoked_at IS NULL AND expires_at>?4", rusqlite::params![!enabled,id,user_id,chrono::Utc::now().timestamp()])? != 1 {
                return Err("token not found or inactive".into());
            }
            serde_json::json!({"updated":true,"enabled":enabled})
        }
        ControlRequest::PrivateList => {
            let mut statement = conn.prepare("SELECT object_id,ciphertext FROM private_metadata WHERE user_id=?1 ORDER BY object_id")?;
            let rows = statement.query_map([user_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut objects = serde_json::Map::new();
            for row in rows {
                let (id, ciphertext) = row?;
                objects.insert(id, ciphertext.into());
            }
            serde_json::Value::Object(objects)
        }
        ControlRequest::PrivatePut { id, ciphertext } => {
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid private object ID".into());
            }
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if let Some(ciphertext) = ciphertext {
                crate::private_metadata::validate_envelope(&ciphertext, 16384)?;
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM private_metadata WHERE user_id=?1 AND object_id<>?2",
                    rusqlite::params![user_id, id],
                    |row| row.get(0),
                )?;
                if count >= 128 {
                    return Err("private metadata capacity reached".into());
                }
                let bytes: i64 = tx.query_row("SELECT COALESCE(SUM(length(ciphertext)),0) FROM private_metadata WHERE user_id=?1 AND object_id<>?2", rusqlite::params![user_id,id], |row| row.get(0))?;
                if bytes + ciphertext.len() as i64 > 524288 {
                    return Err("private metadata capacity reached".into());
                }
                tx.execute("INSERT INTO private_metadata(user_id,object_id,ciphertext) VALUES(?1,?2,?3) ON CONFLICT(user_id,object_id) DO UPDATE SET ciphertext=excluded.ciphertext",rusqlite::params![user_id,id,ciphertext])?;
            } else {
                tx.execute(
                    "DELETE FROM private_metadata WHERE user_id=?1 AND object_id=?2",
                    rusqlite::params![user_id, id],
                )?;
            }
            tx.commit()?;
            serde_json::json!({"updated":true})
        }
        ControlRequest::TokenShow { id } => {
            serde_json::to_value(token_info(conn, user_id, &id)?.ok_or("token not found")?)?
        }
        ControlRequest::TokenRotate { id } => {
            serde_json::to_value(rotate_token(conn, key, user_id, &id)?)?
        }
        ControlRequest::TokenRevoke { id } => {
            serde_json::json!({"revoked":revoke_token(conn,user_id,&id)?})
        }
        ControlRequest::Usage {
            period,
            by,
            timezone,
        } => serde_json::to_value(crate::usage::usage_report_for_user(
            conn,
            user_id,
            &period,
            by.as_deref(),
            timezone.as_deref(),
        )?)?,
        ControlRequest::Models => serde_json::json!({"data":[],"object":"list"}),
        ControlRequest::UsageRange {
            period,
            by,
            timezone,
            before,
        } => serde_json::to_value(crate::usage::usage_range_for_user(
            conn,
            user_id,
            &period,
            by.as_deref(),
            timezone.as_deref(),
            before,
        )?)?,
        ControlRequest::QuotaHistory {
            account,
            period,
            before,
        } => serde_json::to_value(crate::quota_history::range(
            conn,
            account,
            &period,
            before,
            chrono::Utc::now().timestamp(),
        )?)?,
        ControlRequest::ResetPrepare { .. } | ControlRequest::ResetConfirm { .. } => {
            return Err(
                "Reset credits are managed automatically; manual redemption is no longer supported"
                    .into(),
            )
        }
        ControlRequest::Doctor | ControlRequest::Limits => {
            return Err("request requires the running service".into())
        }
    };
    Ok(value)
}
