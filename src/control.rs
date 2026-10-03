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
    AccountSet {
        account: i64,
        enabled: Option<bool>,
        priority: Option<i32>,
    },
    Models,
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
    let value =
        match req {
            ControlRequest::TokenCreate { name, expires_days } => serde_json::to_value(
                create_token(conn, key, user_id, &name, expires_days.unwrap_or(90))?,
            )?,
            ControlRequest::TokenList => serde_json::to_value(list_tokens(conn, user_id)?)?,
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
            ControlRequest::AccountSet {
                account,
                enabled,
                priority,
            } => serde_json::to_value(crate::account_preferences::set_user(
                conn, user_id, account, enabled, priority,
            )?)?,
            ControlRequest::Models => serde_json::json!({"data":[],"object":"list"}),
            ControlRequest::Doctor
            | ControlRequest::Limits
            | ControlRequest::ResetPrepare { .. }
            | ControlRequest::ResetConfirm { .. } => {
                return Err("request requires the running service".into())
            }
        };
    Ok(value)
}
