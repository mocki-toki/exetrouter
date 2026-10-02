//! Durable user preferences and operator-enforced account policy; no credentials.
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preference {
    pub enabled: bool,
    pub priority: i32,
    pub locked: bool,
}
impl Default for Preference {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 0,
            locked: false,
        }
    }
}
pub fn effective(conn: &Connection, user: Option<i64>, account: i64) -> Result<Preference> {
    Ok(conn.query_row("SELECT COALESCE(p.enabled,1) AND CASE WHEN COALESCE(p.locked,0)=1 THEN 1 ELSE COALESCE(u.enabled,1) END, CASE WHEN COALESCE(p.locked,0)=1 THEN COALESCE(p.priority,0) ELSE COALESCE(u.priority,p.priority,0) END, COALESCE(p.locked,0) FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id LEFT JOIN account_preferences u ON u.account_id=a.id AND u.user_id=?1 WHERE a.id=?2", params![user,account], |r| Ok(Preference {enabled:r.get(0)?,priority:r.get(1)?,locked:r.get(2)?}))?)
}
pub fn set_user(
    conn: &mut Connection,
    user: i64,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
) -> Result<Preference> {
    let tx = conn.transaction()?;
    let previous = effective(&tx, Some(user), account)?;
    if previous.locked {
        return Err("Account settings are locked by the server operator".into());
    }
    let own: (Option<bool>, Option<i32>) = tx
        .query_row(
            "SELECT enabled,priority FROM account_preferences WHERE user_id=?1 AND account_id=?2",
            params![user, account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((None, None));
    let enabled = enabled.or(own.0);
    let priority = priority.or(own.1);
    if priority.is_some_and(|priority| !(-100..=100).contains(&priority)) {
        return Err("Priority must be between -100 and 100".into());
    }
    tx.execute("INSERT INTO account_preferences(user_id,account_id,enabled,priority) VALUES(?1,?2,?3,?4) ON CONFLICT(user_id,account_id) DO UPDATE SET enabled=excluded.enabled,priority=excluded.priority",params![user,account,enabled,priority])?;
    let result = effective(&tx, Some(user), account)?;
    tx.commit()?;
    Ok(result)
}
pub fn set_policy(
    conn: &mut Connection,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
    locked: Option<bool>,
) -> Result<Preference> {
    let tx = conn.transaction()?;
    effective(&tx, None, account)?;
    let old=tx.query_row("SELECT COALESCE(p.enabled,1),COALESCE(p.priority,0),COALESCE(p.locked,0) FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.id=?1",[account],|r|Ok(Preference{enabled:r.get(0)?,priority:r.get(1)?,locked:r.get(2)?}))?;
    let priority = priority.unwrap_or(old.priority);
    if !(-100..=100).contains(&priority) {
        return Err("Priority must be between -100 and 100".into());
    }
    tx.execute("INSERT INTO account_policy(account_id,enabled,priority,locked) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id) DO UPDATE SET enabled=excluded.enabled,priority=excluded.priority,locked=excluded.locked",params![account,enabled.unwrap_or(old.enabled),priority,locked.unwrap_or(old.locked)])?;
    let result = effective(&tx, None, account)?;
    tx.commit()?;
    Ok(result)
}
pub fn annotate(conn: &Connection, user: i64, value: &mut serde_json::Value) -> Result<()> {
    if let Some(rows) = value["quota_accounts"].as_array_mut() {
        for row in rows {
            if let Some(id) = row["id"].as_i64() {
                row["preference"] = serde_json::to_value(effective(conn, Some(user), id)?)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_are_user_scoped_reversible_and_operator_enforced() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute(
            "INSERT INTO users(id,name,created_at) VALUES(1,'one',0),(2,'two',0)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO oauth_accounts(id,account_id,encrypted_credentials,state,expires_at,generation,created_at) VALUES(1,'synthetic',X'00','active',2000000000,0,0)",[]).unwrap();
        set_user(&mut conn, 1, 1, Some(false), Some(10)).unwrap();
        assert!(!effective(&conn, Some(1), 1).unwrap().enabled);
        assert!(effective(&conn, Some(2), 1).unwrap().enabled);
        set_user(&mut conn, 1, 1, Some(true), None).unwrap();
        assert_eq!(effective(&conn, Some(1), 1).unwrap().priority, 10);
        set_policy(&mut conn, 1, Some(false), Some(5), Some(true)).unwrap();
        for user in [1, 2] {
            let effective = effective(&conn, Some(user), 1).unwrap();
            assert!(!effective.enabled);
            assert!(effective.locked);
            assert_eq!(effective.priority, 5);
            assert!(set_user(&mut conn, user, 1, Some(true), Some(99)).is_err());
        }
        set_policy(&mut conn, 1, Some(true), None, Some(false)).unwrap();
        assert!(effective(&conn, Some(1), 1).unwrap().enabled);
        assert_eq!(effective(&conn, Some(1), 1).unwrap().priority, 10);
        assert_eq!(
            conn.query_row("SELECT state FROM oauth_accounts WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "active"
        );
        assert!(set_user(&mut conn, 1, 1, None, Some(101)).is_err());
    }
}
