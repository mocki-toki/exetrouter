//! Operator-owned billing metadata. An allowance is never proof of API credit.
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub billing_day: Option<u8>,
    pub plan_usd: Option<u32>,
    /// Explicit operator override; None uses the recognized plan default.
    pub api_budget_cents: Option<u64>,
    pub effective_api_budget_cents: Option<u64>,
}

#[derive(Clone, Debug, Default, clap::Args)]
pub struct Patch {
    /// Expected subscription renewal day, separate from Codex quota resets.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=31))]
    pub billing_day: Option<u8>,
    /// Monthly subscription price in USD (not a verified credit balance).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    pub plan_usd: Option<u32>,
    /// Override the API allowance in USD, for example 100.00; does not grant credits.
    #[arg(long = "api-budget-usd", value_parser = parse_usd, conflicts_with = "clear_api_budget")]
    pub api_budget_cents: Option<u64>,
    /// Remove the API budget override and return to the plan default.
    #[arg(long)]
    pub clear_api_budget: bool,
}

pub fn snapshot(conn: &Connection, account: i64) -> Result<Option<Subscription>> {
    let row = conn.query_row(
        "SELECT billing_day,plan_usd,api_budget_cents FROM account_subscriptions WHERE account_id=?1",
        [account],
        |r| Ok(Subscription { billing_day: r.get(0)?, plan_usd: r.get(1)?, api_budget_cents: r.get(2)?, effective_api_budget_cents: None }),
    ).optional()?;
    Ok(row.map(|mut s| {
        s.effective_api_budget_cents = s.api_budget_cents.or(match s.plan_usd {
            Some(100) => Some(10_000),
            Some(200) => Some(20_000),
            _ => None,
        });
        s
    }))
}

pub fn set(conn: &Connection, account: i64, patch: &Patch) -> Result<Subscription> {
    if patch
        .billing_day
        .is_some_and(|day| !(1..=31).contains(&day))
        || patch
            .plan_usd
            .is_some_and(|price| !(1..=100_000).contains(&price))
        || patch
            .api_budget_cents
            .is_some_and(|amount| amount > 1_000_000_000)
        || (patch.clear_api_budget && patch.api_budget_cents.is_some())
    {
        return Err("invalid subscription metadata".into());
    }
    if patch.billing_day.is_none()
        && patch.plan_usd.is_none()
        && patch.api_budget_cents.is_none()
        && !patch.clear_api_budget
    {
        return Err("provide a billing day, plan price or API budget change".into());
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_accounts WHERE id=?1)",
        [account],
        |r| r.get(0),
    )?;
    if !exists {
        return Err("account not found".into());
    }
    // One atomic upsert prevents concurrent partial patches losing other fields.
    conn.execute("INSERT INTO account_subscriptions(account_id,billing_day,plan_usd,api_budget_cents) VALUES(?1,?2,?3,?4)
        ON CONFLICT(account_id) DO UPDATE SET billing_day=COALESCE(?2,billing_day),plan_usd=COALESCE(?3,plan_usd),api_budget_cents=CASE WHEN ?5 THEN NULL ELSE COALESCE(?4,api_budget_cents) END",
        params![account,patch.billing_day,patch.plan_usd,patch.api_budget_cents,patch.clear_api_budget])?;
    snapshot(conn, account)?.ok_or_else(|| "subscription metadata unavailable".into())
}

/// Parse an explicit USD amount without floating-point rounding or locale guessing.
pub fn parse_usd(value: &str) -> std::result::Result<u64, String> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("use a nonnegative USD amount with at most two decimal places".into());
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|_| "USD amount is too large")?;
    let fraction = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().unwrap() * 10,
        _ => fraction.parse::<u64>().unwrap(),
    };
    whole
        .checked_mul(100)
        .and_then(|n| n.checked_add(fraction))
        .filter(|n| *n <= 1_000_000_000)
        .ok_or_else(|| "USD amount is too large".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        crate::init(&db).unwrap();
        db.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('synthetic','active',X'00',9999999999,0)", []).unwrap();
        db
    }
    #[test]
    fn unknown_is_not_zero_and_plan_allowances_are_not_credit_observations() {
        let db = database();
        assert_eq!(snapshot(&db, 1).unwrap(), None);
        for (price, allowance) in [(20, None), (100, Some(10_000)), (200, Some(20_000))] {
            let value = set(
                &db,
                1,
                &Patch {
                    billing_day: Some(8),
                    plan_usd: Some(price),
                    ..Patch::default()
                },
            )
            .unwrap();
            assert_eq!(value.effective_api_budget_cents, allowance);
            assert_eq!(value.billing_day, Some(8));
            assert!(!serde_json::to_string(&value).unwrap().contains("remaining"));
        }
        let v = set(
            &db,
            1,
            &Patch {
                api_budget_cents: Some(12_345),
                ..Patch::default()
            },
        )
        .unwrap();
        assert_eq!(v.effective_api_budget_cents, Some(12_345));
        let v = set(
            &db,
            1,
            &Patch {
                clear_api_budget: true,
                ..Patch::default()
            },
        )
        .unwrap();
        assert_eq!(v.effective_api_budget_cents, Some(20_000));
        assert_eq!(v.plan_usd, Some(200));
    }
    #[test]
    fn rejects_bad_input_before_mutation_and_does_not_modify_oauth_state() {
        let db = database();
        assert!(set(
            &db,
            1,
            &Patch {
                billing_day: Some(0),
                ..Patch::default()
            }
        )
        .is_err());
        assert!(set(
            &db,
            1,
            &Patch {
                billing_day: Some(32),
                ..Patch::default()
            }
        )
        .is_err());
        assert!(set(
            &db,
            2,
            &Patch {
                billing_day: Some(8),
                ..Patch::default()
            }
        )
        .is_err());
        assert_eq!(snapshot(&db, 1).unwrap(), None);
        set(
            &db,
            1,
            &Patch {
                billing_day: Some(31),
                ..Patch::default()
            },
        )
        .unwrap();
        assert_eq!(
            db.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "00"
        );
        crate::init(&db).unwrap();
        assert_eq!(snapshot(&db, 1).unwrap().unwrap().billing_day, Some(31));
    }
    #[test]
    fn usd_amounts_are_exact_and_bounded() {
        for (text, cents) in [
            ("100", 10_000),
            ("0", 0),
            ("0.01", 1),
            ("12.3", 1230),
            ("200.99", 20_099),
        ] {
            assert_eq!(parse_usd(text).unwrap(), cents);
        }
        for text in [
            "-1",
            "NaN",
            "inf",
            "1.001",
            "1e2",
            "1,000",
            " 100",
            "99999999999999999999",
        ] {
            assert!(parse_usd(text).is_err(), "{text}");
        }
    }
    #[test]
    fn reviewed_rollback_and_reupgrade_retain_billing_without_rotating_credentials() {
        let db = database();
        let expected = set(
            &db,
            1,
            &Patch {
                billing_day: Some(6),
                plan_usd: Some(200),
                api_budget_cents: Some(15_050),
                ..Patch::default()
            },
        )
        .unwrap();
        db.pragma_update(None, "user_version", 16).unwrap();
        crate::init(&db).unwrap();
        assert_eq!(snapshot(&db, 1).unwrap(), Some(expected));
        assert_eq!(
            db.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "00"
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(!db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
}
