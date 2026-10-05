//! Durable user preferences and operator-enforced account policy; no credentials.
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// A missing patch leaves a field unchanged; default removes its override.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Setting {
    Number(i32),
    Mode(Mode),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    Default,
}
impl std::str::FromStr for Setting {
    type Err = String;
    fn from_str(value: &str) -> std::result::Result<Self, String> {
        match value.trim() {
            "off" => Ok(Self::Mode(Mode::Off)),
            "default" => Ok(Self::Mode(Mode::Default)),
            value => value
                .parse()
                .map(Self::Number)
                .map_err(|_| "Enter a whole number, off or default".into()),
        }
    }
}
impl Setting {
    fn quota_value(&self) -> Result<Option<i32>> {
        match self {
            Self::Number(n) if (0..=100).contains(n) => Ok(Some(*n)),
            Self::Mode(Mode::Off) => Ok(Some(-1)),
            Self::Mode(Mode::Default) => Ok(None),
            _ => Err("Switching threshold must be between 0 and 100, off or default".into()),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, clap::Args)]
#[serde(deny_unknown_fields)]
pub struct RoutingArgs {
    /// Numeric priority (-255..255), or default to remove the override.
    #[arg(long, allow_hyphen_values = true)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Setting>,
    /// Switch when any current window has this percentage remaining (0..100, off, default).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_at: Option<Setting>,
    /// Override the threshold for windows shorter than one week.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_at_short: Option<Setting>,
    /// Override the threshold for a reported 10080-minute window.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_at_weekly: Option<Setting>,
}
impl RoutingArgs {
    pub fn validate(&self) -> Result<()> {
        if let Some(priority) = &self.priority {
            match priority {
                Setting::Number(n) if (-255..=255).contains(n) => {}
                Setting::Mode(Mode::Default) => {}
                _ => return Err("Priority must be between -255 and 255, or default".into()),
            }
        }
        for setting in [
            &self.switch_at,
            &self.switch_at_short,
            &self.switch_at_weekly,
        ]
        .into_iter()
        .flatten()
        {
            setting.quota_value()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Rules {
    pub switch_at: Option<i32>,
    pub switch_at_short: Option<i32>,
    pub switch_at_weekly: Option<i32>,
}
impl Rules {
    pub fn threshold(&self, minutes: Option<i64>) -> Option<i32> {
        match minutes {
            Some(10080) => self.switch_at_weekly.or(self.switch_at),
            Some(n) if (1..10080).contains(&n) => self.switch_at_short.or(self.switch_at),
            _ => self.switch_at,
        }
        .filter(|n| *n >= 0)
    }
    pub fn reached(&self, quota: &crate::quota::Summary) -> bool {
        quota.windows.iter().any(|w| {
            w.status == "current"
                && self
                    .threshold(w.window_minutes)
                    .is_some_and(|n| w.remaining_percent <= f64::from(n))
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preference {
    pub enabled: bool,
    pub priority: i32,
    pub locked: bool,
    #[serde(default)]
    pub rules: Rules,
    /// Raw settings for this user (or global policy), for editing inheritance.
    #[serde(default)]
    pub settings: RoutingArgs,
}
impl Default for Preference {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 1,
            locked: false,
            rules: Rules::default(),
            settings: RoutingArgs::default(),
        }
    }
}
fn settings(conn: &Connection, user: Option<i64>, account: i64) -> Result<RoutingArgs> {
    let raw: Option<StoredSettings> = if let Some(user) = user {
        conn.query_row("SELECT priority,switch_at,switch_at_short,switch_at_weekly FROM account_preferences WHERE user_id=?1 AND account_id=?2",params![user,account],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?
    } else {
        conn.query_row("SELECT priority,switch_at,switch_at_short,switch_at_weekly FROM account_policy WHERE account_id=?1",[account],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?
    };
    let (priority, general, short, weekly) = raw.unwrap_or_default();
    let setting = |n: Option<i32>| {
        n.map(|n| {
            if n == -1 {
                Setting::Mode(Mode::Off)
            } else {
                Setting::Number(n)
            }
        })
    };
    Ok(RoutingArgs {
        priority: priority.map(Setting::Number),
        switch_at: setting(general),
        switch_at_short: setting(short),
        switch_at_weekly: setting(weekly),
    })
}
pub fn effective(conn: &Connection, user: Option<i64>, account: i64) -> Result<Preference> {
    let mut preference = conn.query_row("SELECT COALESCE(p.enabled,1) AND CASE WHEN COALESCE(p.locked,0)=1 THEN 1 ELSE COALESCE(u.enabled,1) END, CASE WHEN COALESCE(p.locked,0)=1 THEN COALESCE(p.priority,1) ELSE COALESCE(u.priority,p.priority,1) END, COALESCE(p.locked,0), CASE WHEN COALESCE(p.locked,0)=1 THEN p.switch_at ELSE COALESCE(u.switch_at,p.switch_at) END, CASE WHEN COALESCE(p.locked,0)=1 THEN p.switch_at_short ELSE COALESCE(u.switch_at_short,p.switch_at_short) END, CASE WHEN COALESCE(p.locked,0)=1 THEN p.switch_at_weekly ELSE COALESCE(u.switch_at_weekly,p.switch_at_weekly) END FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id LEFT JOIN account_preferences u ON u.account_id=a.id AND u.user_id=?1 WHERE a.id=?2", params![user,account], |r| Ok(Preference {enabled:r.get(0)?,priority:r.get(1)?,locked:r.get(2)?,rules:Rules{switch_at:r.get(3)?,switch_at_short:r.get(4)?,switch_at_weekly:r.get(5)?},settings:RoutingArgs::default()}))?;
    preference.settings = settings(conn, user, account)?;
    Ok(preference)
}
pub fn set_user(
    conn: &mut Connection,
    user: i64,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
) -> Result<Preference> {
    set_user_rules(
        conn,
        user,
        account,
        enabled,
        priority,
        &RoutingArgs::default(),
    )
}
pub fn set_user_rules(
    conn: &mut Connection,
    user: i64,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
    patch: &RoutingArgs,
) -> Result<Preference> {
    patch.validate()?;
    if priority.is_some() && patch.priority.is_some() {
        return Err("Specify priority only once".into());
    }
    let tx = conn.transaction()?;
    if effective(&tx, Some(user), account)?.locked {
        return Err("Account settings are locked by the server operator".into());
    }
    let mut own = settings(&tx, Some(user), account)?;
    apply(&mut own, patch);
    if let Some(n) = priority {
        own.priority = Some(Setting::Number(n));
    }
    own.validate()?;
    let enabled = enabled.or(tx
        .query_row(
            "SELECT enabled FROM account_preferences WHERE user_id=?1 AND account_id=?2",
            params![user, account],
            |r| r.get::<_, Option<bool>>(0),
        )
        .optional()?
        .flatten());
    let (priority, general, short, weekly) = values(&own)?;
    tx.execute("INSERT INTO account_preferences(user_id,account_id,enabled,priority,switch_at,switch_at_short,switch_at_weekly) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(user_id,account_id) DO UPDATE SET enabled=excluded.enabled,priority=excluded.priority,switch_at=excluded.switch_at,switch_at_short=excluded.switch_at_short,switch_at_weekly=excluded.switch_at_weekly",params![user,account,enabled,priority,general,short,weekly])?;
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
    set_policy_rules(
        conn,
        account,
        enabled,
        priority,
        locked,
        &RoutingArgs::default(),
    )
}
pub fn set_policy_rules(
    conn: &mut Connection,
    account: i64,
    enabled: Option<bool>,
    priority: Option<i32>,
    locked: Option<bool>,
    patch: &RoutingArgs,
) -> Result<Preference> {
    patch.validate()?;
    if priority.is_some() && patch.priority.is_some() {
        return Err("Specify priority only once".into());
    }
    let tx = conn.transaction()?;
    let previous = effective(&tx, None, account)?;
    let mut own = previous.settings;
    apply(&mut own, patch);
    if let Some(n) = priority {
        own.priority = Some(Setting::Number(n));
    }
    own.validate()?;
    let (priority, general, short, weekly) = values(&own)?;
    tx.execute("INSERT INTO account_policy(account_id,enabled,priority,locked,switch_at,switch_at_short,switch_at_weekly) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id) DO UPDATE SET enabled=excluded.enabled,priority=excluded.priority,locked=excluded.locked,switch_at=excluded.switch_at,switch_at_short=excluded.switch_at_short,switch_at_weekly=excluded.switch_at_weekly",params![account,enabled.unwrap_or(previous.enabled),priority.unwrap_or(1),locked.unwrap_or(previous.locked),general.unwrap_or(-1),short,weekly])?;
    let result = effective(&tx, None, account)?;
    tx.commit()?;
    Ok(result)
}
fn apply(own: &mut RoutingArgs, patch: &RoutingArgs) {
    for (field, update) in [
        (&mut own.priority, &patch.priority),
        (&mut own.switch_at, &patch.switch_at),
        (&mut own.switch_at_short, &patch.switch_at_short),
        (&mut own.switch_at_weekly, &patch.switch_at_weekly),
    ] {
        if let Some(update) = update {
            *field = if *update == Setting::Mode(Mode::Default) {
                None
            } else {
                Some(update.clone())
            };
        }
    }
}
type StoredSettings = (Option<i32>, Option<i32>, Option<i32>, Option<i32>);
fn values(settings: &RoutingArgs) -> Result<StoredSettings> {
    let priority = match &settings.priority {
        Some(Setting::Number(n)) => Some(*n),
        _ => None,
    };
    let quota = |setting: &Option<Setting>| -> Result<Option<i32>> {
        setting.as_ref().map_or(Ok(None), Setting::quota_value)
    };
    Ok((
        priority,
        quota(&settings.switch_at)?,
        quota(&settings.switch_at_short)?,
        quota(&settings.switch_at_weekly)?,
    ))
}
pub fn annotate(conn: &Connection, user: i64, value: &mut serde_json::Value) -> Result<()> {
    value["capabilities"]["account_routing_rules"] = serde_json::json!(1);
    if let Some(rows) = value["quota_accounts"].as_array_mut() {
        for row in rows {
            if let Some(id) = row["id"].as_i64() {
                let preference = effective(conn, Some(user), id)?;
                row["threshold_reached"] = serde_json::json!(preference.rules.reached(
                    &crate::quota::summary(conn, id, chrono::Utc::now().timestamp())?
                ));
                row["preference"] = serde_json::to_value(preference)?;
            }
        }
    }
    Ok(())
}

/// Human-readable effective rules, including inheritance and operator policy.
pub fn describe(value: &serde_json::Value) -> String {
    let threshold = |name: &str| {
        let rules = &value["rules"];
        rules[name]
            .as_i64()
            .or_else(|| rules["switch_at"].as_i64())
            .filter(|n| *n >= 0)
            .map_or("off".into(), |n| format!("≤{n}% remaining"))
    };
    let source = |name: &str| {
        if value["locked"] == true {
            "operator locked"
        } else if value["settings"][name].is_null() {
            "inherited"
        } else {
            "configured"
        }
    };
    format!(
        "Switching  {} ({})\nShort      {} ({})\nWeekly     {} ({})\nPriority source: {}",
        threshold("switch_at"),
        source("switch_at"),
        threshold("switch_at_short"),
        source("switch_at_short"),
        threshold("switch_at_weekly"),
        source("switch_at_weekly"),
        source("priority")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute(
            "INSERT INTO users(id,name,created_at) VALUES(1,'one',0),(2,'two',0)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO oauth_accounts(id,account_id,encrypted_credentials,state,expires_at,generation,created_at) VALUES(1,'synthetic',X'00','active',2000000000,0,0)",[]).unwrap();
        conn
    }
    #[test]
    fn routing_inheritance_off_default_and_locks_are_transactional() {
        let mut conn = fixture();
        assert_eq!(effective(&conn, Some(1), 1).unwrap().priority, 1);
        set_policy_rules(
            &mut conn,
            1,
            None,
            None,
            None,
            &RoutingArgs {
                priority: Some(Setting::Number(-255)),
                switch_at: Some(Setting::Number(20)),
                switch_at_weekly: Some(Setting::Number(15)),
                ..Default::default()
            },
        )
        .unwrap();
        let own = set_user_rules(
            &mut conn,
            1,
            1,
            None,
            None,
            &RoutingArgs {
                priority: Some(Setting::Number(255)),
                switch_at_short: Some(Setting::Mode(Mode::Off)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(own.priority, 255);
        assert_eq!(own.rules.threshold(Some(300)), None);
        assert_eq!(own.rules.threshold(Some(10080)), Some(15));
        assert_eq!(own.rules.threshold(Some(20000)), Some(20));
        assert_eq!(effective(&conn, Some(2), 1).unwrap().priority, -255);
        assert!(set_user_rules(
            &mut conn,
            1,
            1,
            Some(false),
            None,
            &RoutingArgs {
                switch_at: Some(Setting::Number(101)),
                ..Default::default()
            }
        )
        .is_err());
        assert!(effective(&conn, Some(1), 1).unwrap().enabled);
        let own = set_user_rules(
            &mut conn,
            1,
            1,
            None,
            None,
            &RoutingArgs {
                priority: Some(Setting::Mode(Mode::Default)),
                switch_at_short: Some(Setting::Mode(Mode::Default)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(own.priority, -255);
        assert_eq!(own.rules.threshold(Some(480)), Some(20));
        set_policy(&mut conn, 1, None, None, Some(true)).unwrap();
        assert!(set_user_rules(
            &mut conn,
            1,
            1,
            None,
            None,
            &RoutingArgs {
                switch_at: Some(Setting::Mode(Mode::Off)),
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(
            effective(&conn, Some(1), 1)
                .unwrap()
                .rules
                .threshold(Some(300)),
            Some(20)
        );
        set_policy_rules(
            &mut conn,
            1,
            None,
            None,
            None,
            &RoutingArgs {
                priority: Some(Setting::Mode(Mode::Default)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(effective(&conn, Some(1), 1).unwrap().priority, 1);
    }
    #[test]
    fn thresholds_use_current_windows_and_reported_durations() {
        let rules = Rules {
            switch_at: Some(20),
            switch_at_short: Some(-1),
            switch_at_weekly: Some(15),
        };
        let mut quota = crate::quota::Summary {
            status: "observed".into(),
            cooldown_until: None,
            cooldown_source: None,
            windows: vec![crate::quota::Window {
                kind: "primary".into(),
                used_percent: 85.,
                remaining_percent: 15.,
                window_minutes: Some(10080),
                reset_at: Some(2000),
                observed_at: 1000,
                status: "current".into(),
            }],
        };
        assert!(rules.reached(&quota));
        quota.windows[0].remaining_percent = 15.01;
        assert!(!rules.reached(&quota));
        quota.windows[0].remaining_percent = 0.;
        for status in ["stale", "reset_elapsed"] {
            quota.windows[0].status = status.into();
            assert!(!rules.reached(&quota));
        }
        quota.windows[0].status = "current".into();
        quota.windows[0].window_minutes = Some(480);
        assert!(!rules.reached(&quota));
        quota.windows[0].window_minutes = None;
        assert!(rules.reached(&quota));
        quota.windows.clear();
        assert!(!rules.reached(&quota));
    }
    #[test]
    fn legacy_wire_requests_omit_new_fields() {
        let old: Preference =
            serde_json::from_value(serde_json::json!({"enabled":true,"priority":0,"locked":false}))
                .unwrap();
        assert_eq!(old.priority, 0);
        assert!(old.rules.switch_at.is_none());
        let request = crate::ControlRequest::AccountSet {
            account: 1,
            enabled: None,
            priority: Some(5),
            routing: None,
        };
        let value = serde_json::to_value(request).unwrap();
        assert!(value.get("routing").is_none());
        let request: crate::ControlRequest = serde_json::from_value(value).unwrap();
        assert!(matches!(
            request,
            crate::ControlRequest::AccountSet { routing: None, .. }
        ));
    }
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
        assert!(set_user(&mut conn, 1, 1, None, Some(256)).is_err());
    }
}
