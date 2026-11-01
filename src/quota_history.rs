//! Bounded hourly subscription metadata, never derived from inference token counts.
use crate::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

pub const HOURS: i64 = 168;
pub const INTERVAL: i64 = 3600;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Point {
    pub kind: String,
    pub hour: i64,
    pub observed_at: i64,
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    pub reset_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    pub from_utc: i64,
    pub to_utc: i64,
    pub interval_seconds: i64,
    pub points: Vec<Point>,
}

pub(crate) fn prune(conn: &Connection, now: i64) -> Result<()> {
    let from = (now.div_euclid(INTERVAL) - HOURS + 1).max(0) * INTERVAL;
    conn.execute("DELETE FROM oauth_quota_history WHERE hour<?1", [from])?;
    Ok(())
}

pub(crate) fn snapshot(conn: &Connection, account: i64, now: i64) -> Result<History> {
    let hour = now.max(0).div_euclid(INTERVAL) * INTERVAL;
    let from = (hour - (HOURS - 1) * INTERVAL).max(0);
    let mut stmt = conn.prepare("SELECT kind,hour,observed_at,used_percent,window_minutes,reset_at FROM oauth_quota_history WHERE account_id=?1 AND hour>=?2 AND hour<=?3 AND observed_at<=?4 ORDER BY hour,kind LIMIT 336")?;
    let points = stmt
        .query_map(params![account, from, hour, now], |row| {
            Ok(Point {
                kind: row.get(0)?,
                hour: row.get(1)?,
                observed_at: row.get(2)?,
                used_percent: row.get(3)?,
                window_minutes: row.get(4)?,
                reset_at: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(History {
        from_utc: from,
        to_utc: hour + INTERVAL,
        interval_seconds: INTERVAL,
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('synthetic','active',X'1234',9999999999,0)", []).unwrap();
        conn
    }
    fn save(conn: &mut Connection, at: i64, order: i64, generation: i64, used: f64) {
        let value = json!({"type":"codex.rate_limits","rate_limits":{
            "primary":{"used_percent":used,"window_minutes":480,"reset_at":at+28800},
            "secondary":{"used_percent":used/2.0,"window_minutes":10080,"reset_at":at+604800}
        }});
        crate::quota::save(
            conn,
            1,
            generation,
            order,
            crate::quota::event(&value, at),
            at,
        )
        .unwrap();
    }
    #[test]
    fn hourly_last_samples_follow_order_and_generation_and_never_fill_gaps() {
        let mut conn = database();
        let base = 1_800_000_000 / INTERVAL * INTERVAL;
        save(&mut conn, base, 10, 0, 20.0);
        save(&mut conn, base + 60, 11, 0, 30.0);
        save(&mut conn, base + 120, 9, 0, 90.0);
        save(&mut conn, base + 180, 12, 1, 95.0);
        save(&mut conn, base + 2 * INTERVAL, 13, 0, 5.0);
        let h = snapshot(&conn, 1, base + 2 * INTERVAL).unwrap();
        assert_eq!(h.points.len(), 4);
        assert_eq!(h.points[0].used_percent, 30.0);
        assert_eq!(h.points[0].observed_at, base + 60);
        assert_eq!(h.points[1].window_minutes, Some(10080));
        assert_eq!(
            h.points[2].used_percent, 5.0,
            "quota reset is not smoothed away"
        );
        assert!(!h.points.iter().any(|p| p.hour == base + INTERVAL));
        assert!(snapshot(&conn, 2, base + 2 * INTERVAL)
            .unwrap()
            .points
            .is_empty());
        assert_eq!(
            snapshot(&conn, 1, base + 30).unwrap().points.len(),
            0,
            "future observations stay hidden"
        );
    }
    #[test]
    fn history_is_bounded_durable_and_additive_rollback_is_reversible() {
        let mut conn = database();
        let base = chrono::Utc::now().timestamp() / INTERVAL * INTERVAL;
        for hour in 0..200 {
            save(
                &mut conn,
                base + hour * INTERVAL,
                hour + 1,
                0,
                (hour % 100) as f64,
            );
        }
        let at = base + 199 * INTERVAL;
        let h = snapshot(&conn, 1, at).unwrap();
        assert_eq!(h.points.len(), 336);
        assert_eq!(h.from_utc, base + 32 * INTERVAL);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM oauth_quota_history", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            336
        );
        conn.pragma_update(None, "user_version", 13).unwrap();
        crate::init(&conn).unwrap();
        assert_eq!(snapshot(&conn, 1, at).unwrap().points.len(), 336);
        assert_eq!(
            conn.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "1234"
        );
        assert_eq!(
            conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(!conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
        assert!(snapshot(&conn, 1, at + HOURS * INTERVAL)
            .unwrap()
            .points
            .is_empty());
        prune(&conn, at + HOURS * INTERVAL).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM oauth_quota_history", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn historical_write_failure_rolls_back_current_quota_too() {
        let mut conn = database();
        conn.execute_batch("CREATE TRIGGER reject_history BEFORE INSERT ON oauth_quota_history BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let at = 1_800_000_000;
        let value = json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":20,"window_minutes":480}}});
        assert!(
            crate::quota::save(&mut conn, 1, 0, 1, crate::quota::event(&value, at), at).is_err()
        );
        assert!(crate::quota::summary(&conn, 1, at)
            .unwrap()
            .windows
            .is_empty());
        assert!(snapshot(&conn, 1, at).unwrap().points.is_empty());
    }
    #[test]
    fn backward_clock_and_explicit_absence_cannot_grow_or_resurrect_history() {
        let mut conn = database();
        let base = 1_800_000_000 / INTERVAL * INTERVAL;
        for hour in 0..200 {
            save(&mut conn, base - hour * INTERVAL, hour + 1, 0, 20.0);
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM oauth_quota_history", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            336
        );
        let absent = crate::quota::Observation::default()
            .with_live_absent_windows(vec!["primary", "secondary"]);
        crate::quota::save(&mut conn, 1, 0, 300, absent, base).unwrap();
        let before = snapshot(&conn, 1, base).unwrap().points.len();
        save(&mut conn, base + 3600, 250, 0, 99.0);
        let after = snapshot(&conn, 1, base + 3600).unwrap();
        assert!(after.points.len() <= before);
        assert!(!after.points.iter().any(|p| p.hour == base + 3600));
        assert!(crate::quota::summary(&conn, 1, base)
            .unwrap()
            .windows
            .is_empty());
    }
}
