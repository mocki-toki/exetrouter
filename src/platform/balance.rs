//! Manual reconciliation, never an admission budget or a provider balance claim.
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub remaining_usd_nanos: Option<i64>,
    pub spent_usd_nanos: Option<i64>,
    pub through_utc: i64,
    pub observed_at: Option<i64>,
    pub unpriced_requests: i64,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub entered_at: i64,
    pub initial_usd_nanos: i64,
    pub exetrouter: Estimate,
    pub organization: Estimate,
}

pub(super) fn anchor(conn: &Connection, account: i64) -> Result<Option<(String, i64, i64)>> {
    Ok(conn
        .query_row(
            "SELECT epoch,entered_at,generation FROM platform_balance_anchors WHERE account_id=?1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

pub fn set(conn: &Connection, account: i64, cents: u64, now: i64) -> Result<Snapshot> {
    let initial = cents
        .checked_mul(10_000_000)
        .filter(|_| cents <= 1_000_000_000)
        .ok_or("invalid manual USD balance")? as i64;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let generation: i64 = tx
        .query_row(
            "SELECT generation FROM account_platform_credentials WHERE account_id=?1",
            [account],
            |r| r.get(0),
        )
        .optional()?
        .ok_or("configure an Admin Key first")?;
    if now < 0 {
        return Err("invalid manual balance time".into());
    }
    let epoch = format!("{:032x}", rand::random::<u128>());
    tx.execute("INSERT INTO platform_balance_anchors(account_id,epoch,generation,entered_at,initial_usd_nanos,usage_after_id) VALUES(?1,?2,?3,?4,?5,COALESCE((SELECT MAX(id) FROM usage_events),0)) ON CONFLICT(account_id) DO UPDATE SET epoch=excluded.epoch,generation=excluded.generation,entered_at=excluded.entered_at,initial_usd_nanos=excluded.initial_usd_nanos,usage_after_id=excluded.usage_after_id,organization_spent_nanos=NULL,organization_to_utc=NULL,organization_observed_at=NULL",params![account,epoch,generation,now,initial])?;
    let value = snapshot(&tx, account, now)?.ok_or("manual balance unavailable")?;
    tx.commit()?;
    Ok(value)
}

pub fn snapshot(conn: &Connection, account: i64, now: i64) -> Result<Option<Snapshot>> {
    let row = conn.query_row("SELECT b.entered_at,b.initial_usd_nanos,b.usage_after_id,b.generation=c.generation,b.organization_spent_nanos,b.organization_to_utc,b.organization_observed_at FROM platform_balance_anchors b JOIN account_platform_credentials c USING(account_id) WHERE b.account_id=?1",[account],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,bool>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<i64>>(5)?,r.get::<_,Option<i64>>(6)?))).optional()?;
    let Some((entered, initial, after, current, org_spent, org_to, org_observed)) = row else {
        return Ok(None);
    };
    let (known,unpriced):(i64,i64) = conn.query_row("SELECT COALESCE(SUM(c.usd_nanos),0),COALESCE(SUM(c.usd_nanos IS NULL),0) FROM usage_events e LEFT JOIN platform_request_costs c ON c.usage_id=e.id WHERE e.account_id=?1 AND e.id>?2 AND e.upstream_transport IN ('platform_http_sse','platform_http_json')",params![account,after],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let valid = current && now >= entered;
    let state = if !valid {
        "configuration_changed"
    } else if unpriced > 0 {
        "unpriced_requests"
    } else {
        "estimated"
    };
    let exetrouter = Estimate {
        remaining_usd_nanos: (valid && unpriced == 0)
            .then(|| initial.checked_sub(known))
            .flatten(),
        spent_usd_nanos: valid.then_some(known),
        through_utc: now.max(entered),
        observed_at: Some(now),
        unpriced_requests: unpriced,
        status: state.into(),
    };
    let organization = Estimate {
        remaining_usd_nanos: valid
            .then(|| org_spent.and_then(|spent| initial.checked_sub(spent)))
            .flatten(),
        spent_usd_nanos: valid.then_some(org_spent).flatten(),
        through_utc: org_to.unwrap_or(entered),
        observed_at: org_observed,
        unpriced_requests: 0,
        status: if !valid {
            "configuration_changed"
        } else if org_observed.is_none() {
            "awaiting_report"
        } else if org_to.is_some_and(|to| to >= now) {
            "reported"
        } else {
            "delayed_report"
        }
        .into(),
    };
    Ok(Some(Snapshot {
        entered_at: entered,
        initial_usd_nanos: initial,
        exetrouter,
        organization,
    }))
}

/// Only publish the contiguous prefix of the explicitly anchored query. Daily
/// buckets must never be prorated, zero-filled or added to the router estimate.
pub(super) fn reported(points: &[super::costs::Point], from: i64) -> Option<(i64, i64)> {
    let mut expected = from;
    let mut amount = 0_i64;
    let mut rows = points.iter().collect::<Vec<_>>();
    rows.sort_by_key(|p| p.from_utc);
    for p in rows {
        if p.from_utc != expected || p.to_utc <= expected {
            return None;
        }
        amount = amount.checked_add(p.usd_nanos)?;
        expected = p.to_utc;
    }
    (expected > from).then_some((amount, expected))
}

pub(super) fn save_report(
    conn: &Connection,
    account: i64,
    epoch: &str,
    generation: i64,
    amount: i64,
    through: i64,
    observed: i64,
) -> Result<()> {
    conn.execute("UPDATE platform_balance_anchors SET organization_spent_nanos=?4,organization_to_utc=?5,organization_observed_at=?6 WHERE account_id=?1 AND epoch=?2 AND generation=?3 AND EXISTS(SELECT 1 FROM account_platform_credentials c WHERE c.account_id=?1 AND c.generation=?3) AND ?5>=entered_at AND ?6>=COALESCE(organization_observed_at,0)",params![account,epoch,generation,amount,through,observed])?;
    Ok(())
}

pub fn headline(platform: &serde_json::Value) -> String {
    let b = &platform["balance"];
    let dollars = |v: &serde_json::Value| {
        v.as_i64()
            .map(|n| crate::money::usd(n as f64 / 1e9))
            .unwrap_or_else(|| "Unknown".into())
    };
    if b.is_object() {
        format!(
            "Platform API  ·  {} / {} left · estimated",
            dollars(&b["exetrouter"]["remaining_usd_nanos"]),
            dollars(&b["initial_usd_nanos"])
        )
    } else {
        "Platform API  ·  Set a manual balance to estimate remaining funds".into()
    }
}

pub fn detail(platform: &serde_json::Value) -> String {
    let b = &platform["balance"];
    if !b.is_object() {
        return "Manual balance not set; provider balance is not available through Admin API."
            .into();
    }
    let org = &b["organization"];
    let dollars = |v: &serde_json::Value| {
        v.as_i64()
            .map(|n| crate::money::usd(n as f64 / 1e9))
            .unwrap_or_else(|| "Unknown".into())
    };
    let date = |v: &serde_json::Value| {
        v.as_i64()
            .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
            .map(|at| {
                at.with_timezone(&chrono::Local)
                    .format("%b %-d, %Y %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "Unknown".into())
    };
    format!("Manual {} · {}\nExetRouter: {} left · standard token estimate; excludes external usage, tool fees and tier adjustments · {} unpriced\nPlatform organization: {} left · {} · through {}",dollars(&b["initial_usd_nanos"]),date(&b["entered_at"]),dollars(&b["exetrouter"]["remaining_usd_nanos"]),b["exetrouter"]["unpriced_requests"].as_i64().unwrap_or(0),dollars(&org["remaining_usd_nanos"]),org["status"].as_str().unwrap_or("awaiting_report").replace('_'," "),date(&org["through_utc"]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{self, Counters, RequestOutcome, RequestRecord};

    fn database() -> (Connection, String) {
        let conn = Connection::open_in_memory().unwrap();
        crate::init(&conn).unwrap();
        let user = crate::create_user(&conn, "synthetic").unwrap();
        let token = crate::create_token(&conn, &[1; 32], user, "fixture", 1).unwrap();
        for account in ["a", "b"] {
            conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES(?1,'active',X'00',9999999999,0)",[account]).unwrap();
        }
        for id in [1, 2] {
            conn.execute("INSERT INTO account_platform_credentials(account_id,encrypted_admin_key,generation,configured_at) VALUES(?1,X'00',1,1)",[id]).unwrap();
        }
        (conn, token.token.id)
    }
    fn begin(conn: &Connection, token: &str, id: &str, account: i64, model: &str, transport: &str) {
        usage::begin_request(
            conn,
            &RequestRecord {
                request_id: id.into(),
                user_id: 1,
                token_id: token.into(),
                account_id: account,
                surface: "responses",
                model: model.into(),
                client_transport: "http_sse",
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE usage_events SET status='sent',upstream_transport=?2 WHERE request_id=?1",
            params![id, transport],
        )
        .unwrap();
    }
    fn finish(conn: &Connection, id: &str, transport: &'static str, input: Option<i64>) {
        usage::finish_request(
            conn,
            id,
            &RequestOutcome {
                status: "completed",
                counters: Counters {
                    input,
                    output: Some(10),
                    cached_input: Some(2),
                    reasoning_output: None,
                },
                upstream_transport: transport,
                upstream_request_id: None,
                upstream_response_id: None,
                duration_ms: 1,
            },
        )
        .unwrap();
    }
    #[test]
    fn entry_replaces_both_estimates_excludes_native_other_accounts_and_pre_anchor_requests() {
        let (conn, token) = database();
        begin(&conn, &token, "before", 1, "gpt-5.5", "platform_http_sse");
        let first = set(&conn, 1, 10000, 100).unwrap();
        assert_eq!(first.exetrouter.remaining_usd_nanos, Some(100_000_000_000));
        assert_eq!(first.organization.remaining_usd_nanos, None);
        finish(&conn, "before", "platform_http_sse", Some(100));
        for (id, account, transport) in [
            ("native", 1, "http_sse"),
            ("foreign", 2, "platform_http_sse"),
            ("after", 1, "platform_http_sse"),
        ] {
            begin(&conn, &token, id, account, "gpt-5.5", transport);
            finish(&conn, id, transport, Some(100));
        }
        let b = snapshot(&conn, 1, 200).unwrap().unwrap();
        assert_eq!(b.exetrouter.spent_usd_nanos, Some(791000));
        assert_eq!(
            b.exetrouter.remaining_usd_nanos,
            Some(100_000_000_000 - 791000)
        );
        let old = anchor(&conn, 1).unwrap().unwrap();
        save_report(&conn, 1, &old.0, 1, 900_000_000, 190, 200).unwrap();
        let b = snapshot(&conn, 1, 200).unwrap().unwrap();
        assert_eq!(b.organization.remaining_usd_nanos, Some(99_100_000_000));
        assert_eq!(
            b.exetrouter.spent_usd_nanos,
            Some(791000),
            "alternative estimates are never added together"
        );
        let second = set(&conn, 1, 5000, 200).unwrap();
        assert_eq!(second.exetrouter.remaining_usd_nanos, Some(50_000_000_000));
        assert_eq!(second.organization.remaining_usd_nanos, None);
        assert_ne!(anchor(&conn, 1).unwrap().unwrap().0, old.0);
        save_report(&conn, 1, &old.0, 1, 1, 210, 210).unwrap();
        assert_eq!(
            snapshot(&conn, 1, 210)
                .unwrap()
                .unwrap()
                .organization
                .remaining_usd_nanos,
            None
        );
        crate::init(&conn).unwrap();
        assert_eq!(
            snapshot(&conn, 1, 200 + 40 * 86400)
                .unwrap()
                .unwrap()
                .exetrouter
                .remaining_usd_nanos,
            Some(50_000_000_000),
            "no calendar-based refill"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM usage_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
    #[test]
    fn unknown_usage_stays_unknown_across_recovery_until_new_manual_entry() {
        let (conn, token) = database();
        set(&conn, 1, 0, 100).unwrap();
        begin(&conn, &token, "pending", 1, "gpt-5.5", "platform_http_sse");
        assert_eq!(
            snapshot(&conn, 1, 101)
                .unwrap()
                .unwrap()
                .exetrouter
                .remaining_usd_nanos,
            None
        );
        usage::recover_requests(&conn).unwrap();
        crate::init(&conn).unwrap();
        let b = snapshot(&conn, 1, 102).unwrap().unwrap();
        assert_eq!(b.exetrouter.remaining_usd_nanos, None);
        assert_eq!(b.exetrouter.unpriced_requests, 1);
        set(&conn, 1, 0, 103).unwrap();
        begin(&conn, &token, "known", 1, "gpt-5.5", "platform_http_json");
        finish(&conn, "known", "platform_http_json", Some(100));
        assert_eq!(
            snapshot(&conn, 1, 104)
                .unwrap()
                .unwrap()
                .exetrouter
                .remaining_usd_nanos,
            Some(-791000),
            "overspend is not hidden or clamped to zero"
        );
        assert!(
            crate::platform::routes::model_spending_permitted(&conn, "text-embedding-3-small")
                .unwrap(),
            "manual balance does not gate routing"
        );
        assert!(usage::finish_request(
            &conn,
            "known",
            &RequestOutcome {
                status: "completed",
                counters: Counters::default(),
                upstream_transport: "platform_http_json",
                upstream_request_id: None,
                upstream_response_id: None,
                duration_ms: 1
            }
        )
        .is_err());
    }
    #[test]
    fn changes_and_reupgrade_are_fenced_without_rotating_or_repricing_secrets() {
        let (conn, token) = database();
        set(&conn, 1, 10000, 100).unwrap();
        begin(&conn, &token, "known", 1, "gpt-5.5", "platform_http_sse");
        finish(&conn, "known", "platform_http_sse", Some(100));
        let before = snapshot(&conn, 1, 110).unwrap();
        conn.pragma_update(None, "user_version", 19).unwrap();
        crate::init(&conn).unwrap();
        assert_eq!(snapshot(&conn, 1, 110).unwrap(), before);
        conn.execute(
            "UPDATE usage_events SET model='unreviewed' WHERE request_id='known'",
            [],
        )
        .unwrap();
        assert_eq!(
            snapshot(&conn, 1, 110).unwrap(),
            before,
            "finalized costs are frozen, not repriced on read"
        );
        conn.execute(
            "UPDATE account_platform_credentials SET generation=generation+1 WHERE account_id=1",
            [],
        )
        .unwrap();
        let b = snapshot(&conn, 1, 110).unwrap().unwrap();
        assert_eq!(b.exetrouter.remaining_usd_nanos, None);
        assert_eq!(b.exetrouter.status, "configuration_changed");
        assert_eq!(b.organization.remaining_usd_nanos, None);
        assert!(set(&conn, 99, 10000, 100).is_err());
        assert!(set(&conn, 1, 1_000_000_001, 100).is_err());
        assert_eq!(
            conn.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts WHERE id=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "00"
        );
    }
    #[test]
    fn organization_costs_use_exact_requested_partial_day_and_never_fill_holes() {
        let day = 1800000000 / 86400 * 86400;
        let from = day + 1234;
        let page = serde_json::json!({"object":"page","has_more":false,"data":[{"object":"bucket","start_time":day,"end_time":day+86400,"results":[{"object":"organization.costs.result","amount":{"value":0.25,"currency":"usd"}}]}]});
        let points =
            super::super::costs::parse_range_page(&page, from, day + 86400, day + 86400).unwrap();
        assert_eq!(
            reported(&points, from),
            Some((250_000_000, day + 86400)),
            "explicit query amount is not prorated"
        );
        assert_eq!(reported(&points, from + 1), None);
        assert_eq!(reported(&[], from), None);
        assert_eq!(
            reported(&[points[0].clone(), points[0].clone()], from),
            None
        );
    }
    #[test]
    fn standard_price_estimate_is_exact_for_cached_long_and_unknown_counters() {
        assert_eq!(
            usage::token_cost_nanos(
                "gpt-5.5",
                &Counters {
                    input: Some(300000),
                    output: Some(100),
                    cached_input: Some(200000),
                    reasoning_output: None
                }
            ),
            Some(1_204_500_000)
        );
        assert_eq!(
            usage::token_cost_nanos("gpt-5.5", &Counters::default()),
            None
        );
        assert_eq!(
            usage::token_cost_nanos(
                "unknown",
                &Counters {
                    input: Some(0),
                    output: Some(0),
                    cached_input: Some(0),
                    reasoning_output: None
                }
            ),
            None
        );
    }
}
