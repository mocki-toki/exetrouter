//! Exact daily organization costs. Missing days remain absent, never zero-filled.
use crate::Result;
use chrono::{Datelike, TimeZone, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const DAY: i64 = 86400;
pub const NANOS: i64 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub from_utc: i64,
    pub to_utc: i64,
    pub observed_at: i64,
    pub usd_nanos: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct History {
    pub from_utc: i64,
    pub to_utc: i64,
    pub interval_seconds: i64,
    pub points: Vec<Point>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub from_utc: i64,
    pub to_utc: i64,
    pub observed_at: Option<i64>,
    pub basis: String,
    pub spent_usd_nanos: Option<i64>,
    pub budget_cents: Option<u64>,
    pub complete: bool,
}

/// Decimal/scientific JSON numbers to exact nanodollars; never sum binary floats.
fn nanos(value: &Value) -> Result<i64> {
    let number = value
        .as_number()
        .ok_or("invalid Platform cost amount")?
        .to_string();
    let (mantissa, exponent) = number.split_once(['e', 'E']).unwrap_or((&number, "0"));
    let exponent: i32 = exponent
        .parse()
        .map_err(|_| "invalid Platform cost precision")?;
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|c| c.is_ascii_digit())
    {
        return Err("invalid Platform cost amount".into());
    }
    let digits = format!("{whole}{fraction}");
    let mut value: i128 = digits.parse().map_err(|_| "Platform cost out of range")?;
    let shift = 9 + exponent - fraction.len() as i32;
    if !(-32..=18).contains(&shift) {
        return Err("invalid Platform cost precision".into());
    }
    if shift >= 0 {
        value = value
            .checked_mul(10_i128.pow(shift as u32))
            .ok_or("Platform cost out of range")?;
    } else {
        let divisor = 10_i128.pow((-shift) as u32);
        if value % divisor != 0 {
            return Err("invalid Platform cost precision".into());
        }
        value /= divisor;
    }
    i64::try_from(value)
        .ok()
        .filter(|v| *v <= 1_000_000_000 * NANOS)
        .ok_or_else(|| "Platform cost out of range".into())
}

pub(super) fn parse_page(value: &Value, from: i64, to: i64, now: i64) -> Result<Vec<Point>> {
    parse_range_page(value, from, to, now)
}

/// The query filters [from,to), including a partial first UTC day. Normalize
/// only the labels, never prorate the amount returned for that explicit query.
pub(super) fn parse_range_page(value: &Value, from: i64, to: i64, now: i64) -> Result<Vec<Point>> {
    if value["object"] != "page" || value["has_more"].as_bool().is_none() {
        return Err("invalid Platform costs page".into());
    }
    let rows = value["data"]
        .as_array()
        .filter(|r| r.len() <= 180)
        .ok_or("invalid Platform costs page")?;
    let mut points = Vec::new();
    for row in rows {
        let start = row["start_time"]
            .as_i64()
            .ok_or("invalid Platform cost interval")?;
        let end = row["end_time"]
            .as_i64()
            .ok_or("invalid Platform cost interval")?;
        if row["object"] != "bucket"
            || (start % DAY != 0 && (start - from) % DAY != 0)
            || start < from.div_euclid(DAY) * DAY
            || end <= start
            || start >= to
            || end > start + DAY
        {
            return Err("invalid Platform cost interval".into());
        }
        let mut amount = 0_i64;
        for result in row["results"]
            .as_array()
            .filter(|r| r.len() <= 1024)
            .ok_or("invalid Platform costs bucket")?
        {
            if result["object"] != "organization.costs.result"
                || result["amount"]["currency"] != "usd"
            {
                return Err("unsupported Platform cost result".into());
            }
            amount = amount
                .checked_add(nanos(&result["amount"]["value"])?)
                .ok_or("Platform cost out of range")?;
        }
        points.push(Point {
            from_utc: start.max(from),
            // The Costs query's end_time is exclusive. A provider may label
            // its final bucket with the full UTC day boundary; do not claim
            // observation beyond the requested cutoff or rescale its amount.
            to_utc: end.min(to),
            observed_at: now,
            usd_nanos: amount,
        });
    }
    Ok(points)
}

pub(super) fn save(
    conn: &Connection,
    account: i64,
    generation: i64,
    points: &[Point],
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for p in points {
        if !seen.insert(p.from_utc) {
            return Err("duplicate Platform costs bucket".into());
        }
        conn.execute("INSERT INTO platform_cost_history(account_id,generation,day,end_at,observed_at,usd_nanos) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,day) DO UPDATE SET generation=excluded.generation,end_at=excluded.end_at,observed_at=excluded.observed_at,usd_nanos=excluded.usd_nanos WHERE excluded.generation>generation OR (excluded.generation=generation AND excluded.observed_at>=observed_at)",params![account,generation,p.from_utc,p.to_utc,p.observed_at,p.usd_nanos])?;
    }
    Ok(())
}

pub fn history(
    conn: &Connection,
    account: i64,
    from: i64,
    to: i64,
    now: i64,
) -> Result<Option<History>> {
    let generation = conn
        .query_row(
            "SELECT generation FROM account_platform_credentials WHERE account_id=?1",
            [account],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    let Some(generation) = generation else {
        return Ok(None);
    };
    let from_day = from.div_euclid(DAY) * DAY;
    let mut stmt = conn.prepare("SELECT day,end_at,observed_at,usd_nanos FROM platform_cost_history WHERE account_id=?1 AND generation=?2 AND day>=?3 AND day<?4 AND observed_at<=?5 ORDER BY day LIMIT 367")?;
    let points = stmt
        .query_map(params![account, generation, from_day, to, now], |r| {
            Ok(Point {
                from_utc: r.get(0)?,
                to_utc: r.get(1)?,
                observed_at: r.get(2)?,
                usd_nanos: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // Never attribute the full daily amount to a shorter selected interval.
    let points = points
        .into_iter()
        .filter(|p| p.from_utc >= from && p.to_utc <= to)
        .collect();
    Ok(Some(History {
        from_utc: from,
        to_utc: to,
        interval_seconds: DAY,
        points,
    }))
}
use rusqlite::OptionalExtension;

pub fn snapshot(conn: &Connection, account: i64, now: i64) -> Result<Snapshot> {
    let subscription = crate::subscription::snapshot(conn, account)?;
    let day = subscription.as_ref().and_then(|s| s.billing_day);
    let from = reporting_start(now, day)?;
    let to: Option<i64> = conn.query_row("SELECT costs_to_utc FROM platform_refresh r JOIN account_platform_credentials c USING(account_id) WHERE r.account_id=?1 AND r.generation=c.generation AND r.observed_at<=?2", params![account,now],|r|r.get(0)).optional()?.flatten();
    let to = to.filter(|to| *to <= now && *to > from).unwrap_or(now);
    let history =
        history(conn, account, from, to, now)?.ok_or("Platform configuration unavailable")?;
    let mut expected = from;
    let mut spent = 0_i64;
    for p in &history.points {
        if p.from_utc != expected {
            break;
        }
        expected = p.to_utc;
        spent = spent
            .checked_add(p.usd_nanos)
            .ok_or("Platform cost out of range")?;
    }
    let complete = expected == to && !history.points.is_empty();
    Ok(Snapshot {
        from_utc: from,
        to_utc: to,
        observed_at: history.points.iter().map(|p| p.observed_at).max(),
        basis: if day.is_some() {
            "billing_day_utc"
        } else {
            "calendar_month_utc"
        }
        .into(),
        spent_usd_nanos: complete.then_some(spent),
        budget_cents: subscription.and_then(|s| s.effective_api_budget_cents),
        complete,
    })
}

/// A reporting convention, not an inferred credit-grant or renewal timestamp.
fn reporting_start(now: i64, day: Option<u8>) -> Result<i64> {
    let date = Utc
        .timestamp_opt(now, 0)
        .single()
        .ok_or("invalid Platform observation time")?;
    let start = |year, month| -> Result<i64> {
        let (next_year, next_month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        let next = Utc
            .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
            .single()
            .ok_or("invalid Platform reporting window")?;
        let last = (next - chrono::Duration::days(1)).day();
        Ok(Utc
            .with_ymd_and_hms(year, month, u32::from(day.unwrap_or(1)).min(last), 0, 0, 0)
            .single()
            .ok_or("invalid Platform reporting window")?
            .timestamp())
    };
    let current = start(date.year(), date.month())?;
    if current <= now {
        Ok(current)
    } else {
        let (year, month) = if date.month() == 1 {
            (date.year() - 1, 12)
        } else {
            (date.year(), date.month() - 1)
        };
        start(year, month)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exact_costs_do_not_accept_foreign_currencies_missing_amounts_or_unknown_results() {
        let base = 1_800_000_000 / DAY * DAY;
        let mut page = json!({"object":"page","has_more":false,"data":[{"object":"bucket","start_time":base,"end_time":base+DAY,"results":[{"object":"organization.costs.result","amount":{"currency":"usd","value":0.00000125}}]}]});
        assert_eq!(
            parse_page(&page, base, base + DAY, base + DAY).unwrap()[0].usd_nanos,
            1250
        );
        let partial = parse_page(&page, base, base + 3600, base + 3600).unwrap();
        assert_eq!(partial[0].to_utc, base + 3600);
        assert_eq!(
            partial[0].usd_nanos, 1250,
            "a filtered partial-day amount is not interpolated"
        );
        for invalid in [json!(null), json!(-1), json!(1e-10)] {
            page["data"][0]["results"][0]["amount"]["value"] = invalid;
            assert!(parse_page(&page, base, base + DAY, base + DAY).is_err());
        }
        assert_eq!(nanos(&json!(0.06)).unwrap(), 60_000_000);
    }
    #[test]
    fn reporting_days_clamp_short_months_without_claiming_a_grant() {
        let now = Utc
            .with_ymd_and_hms(2028, 2, 29, 12, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(reporting_start(now, Some(31)).unwrap(), now - 12 * 3600);
        assert_eq!(
            reporting_start(now, None).unwrap(),
            Utc.with_ymd_and_hms(2028, 2, 1, 0, 0, 0)
                .unwrap()
                .timestamp()
        );
    }
}
