use crate::Result;
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use chrono_tz::{Europe::Moscow, Tz};
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct UsageRow {
    pub name: String,
    /// Stored label (ciphertext for native clients), only in user-scoped token reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub requests: i64,
    pub unknown_usage: i64,
    pub known_usage: i64,
    pub partial_usage: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    /// Standard OpenAI API equivalent in USD, not ChatGPT subscription spend.
    pub estimated_usd: Option<f64>,
    /// Requests omitted from the estimate due to missing counters or pricing.
    pub unpriced_requests: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageReport {
    pub period: String,
    pub timezone: String,
    pub from_utc: i64,
    pub to_utc: i64,
    pub rows: Vec<UsageRow>,
    pub timeline: Vec<UsageBucket>,
}

#[derive(Debug, Serialize)]
pub struct UsageBucket {
    pub from_utc: i64,
    pub to_utc: i64,
    pub rows: Vec<UsageRow>,
}

pub struct UsageEvent<'a> {
    pub user_id: i64,
    pub token_id: &'a str,
    pub surface: &'a str,
    pub model: &'a str,
    pub status: &'a str,
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cached_input: Option<i64>,
    pub reasoning_output: Option<i64>,
}

#[derive(Clone, Copy, Default)]
pub struct Counters {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cached_input: Option<i64>,
    pub reasoning_output: Option<i64>,
}

impl Counters {
    pub fn from_json(value: Option<&serde_json::Value>) -> Result<Self> {
        let counter = |path: &str| -> Result<Option<i64>> {
            let Some(value) = value.and_then(|v| v.pointer(path)).filter(|v| !v.is_null()) else {
                return Ok(None);
            };
            Ok(Some(
                value
                    .as_i64()
                    .filter(|v| *v >= 0)
                    .ok_or("invalid upstream usage counter")?,
            ))
        };
        let counters = Self {
            input: counter("/input_tokens")?,
            output: counter("/output_tokens")?,
            cached_input: counter("/input_tokens_details/cached_tokens")?,
            reasoning_output: counter("/output_tokens_details/reasoning_tokens")?,
        };
        if counters
            .cached_input
            .zip(counters.input)
            .is_some_and(|(cached, input)| cached > input)
            || counters
                .reasoning_output
                .zip(counters.output)
                .is_some_and(|(reasoning, output)| reasoning > output)
        {
            return Err("invalid upstream usage details".into());
        }
        Ok(counters)
    }
}

pub struct RequestRecord {
    pub request_id: String,
    pub user_id: i64,
    pub token_id: String,
    pub account_id: i64,
    pub surface: &'static str,
    pub model: String,
    pub client_transport: &'static str,
}

pub fn begin_request(conn: &Connection, record: &RequestRecord) -> Result<()> {
    conn.execute("INSERT INTO usage_events(at_utc,user_id,token_id,account_id,api_surface,model,status,request_id,client_transport) VALUES(?1,?2,?3,?4,?5,?6,'accepted',?7,?8)",params![Utc::now().timestamp(),record.user_id,record.token_id,record.account_id,record.surface,record.model,record.request_id,record.client_transport])?;
    Ok(())
}

pub struct RequestOutcome {
    pub status: &'static str,
    pub counters: Counters,
    pub upstream_transport: &'static str,
    pub upstream_request_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub duration_ms: i64,
}

pub fn finish_request(conn: &Connection, request_id: &str, outcome: &RequestOutcome) -> Result<()> {
    let counters = &outcome.counters;
    let changed=conn.execute("UPDATE usage_events SET status=?1,input_tokens=?2,output_tokens=?3,cached_input_tokens=?4,reasoning_output_tokens=?5,upstream_transport=?6,upstream_request_id=?7,upstream_response_id=?8,duration_ms=?9 WHERE request_id=?10 AND status IN ('accepted','sent')",params![outcome.status,counters.input,counters.output,counters.cached_input,counters.reasoning_output,outcome.upstream_transport,outcome.upstream_request_id,outcome.upstream_response_id,outcome.duration_ms,request_id])?;
    if changed != 1 {
        return Err("request missing or already finalized".into());
    }
    Ok(())
}

pub fn recover_requests(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("UPDATE usage_events SET status='aborted_unknown' WHERE request_id IS NOT NULL AND status IN ('accepted','sent')",[])?)
}

pub(crate) const CLIENT_IDLE_SECONDS: i64 = 15 * 60;
const MAX_ACCOUNT_CLIENTS: usize = 128;

/// Server-wide recent inference tokens, not connected applications or sockets.
/// Only the viewer's token IDs/labels are exposed; other tokens get keyed aliases.
/// Long-running durable admissions remain visible until finalization/recovery.
pub(crate) fn clients_by_account(
    conn: &Connection,
    key: &[u8],
    user: i64,
    now: i64,
) -> Result<std::collections::BTreeMap<i64, serde_json::Value>> {
    let mut stmt = conn.prepare(
        "WITH clients AS (
           SELECT e.account_id,e.token_id,e.user_id=?1 AS owned,
             CASE WHEN e.user_id=?1 THEN t.name END AS name,
             MAX(e.at_utc) AS last_request_at,SUM(e.status IN ('accepted','sent')) AS in_flight
           FROM usage_events e JOIN access_tokens t ON t.id=e.token_id AND t.user_id=e.user_id
           JOIN oauth_accounts a ON a.id=e.account_id AND a.state='active'
           WHERE e.at_utc<=?2 AND (e.at_utc>?3 OR e.status IN ('accepted','sent'))
           GROUP BY e.account_id,e.token_id
         ), ranked AS (
           SELECT *,COUNT(*) OVER (PARTITION BY account_id) AS total,
             ROW_NUMBER() OVER (PARTITION BY account_id ORDER BY in_flight>0 DESC,last_request_at DESC,token_id) AS position FROM clients
         ) SELECT account_id,token_id,name,last_request_at,in_flight,total,owned FROM ranked WHERE position<=?4 ORDER BY account_id,position"
    )?;
    let rows=stmt.query_map(params![user,now,now.saturating_sub(CLIENT_IDLE_SECONDS),MAX_ACCOUNT_CLIENTS],|row| {
        Ok((row.get::<_,i64>(0)?,row.get::<_,usize>(5)?,serde_json::json!({"id":row.get::<_,String>(1)?,"name":row.get::<_,Option<String>>(2)?,"last_request_at":row.get::<_,i64>(3)?,"in_flight":row.get::<_,usize>(4)?,"owned":row.get::<_,bool>(6)?})))
    })?;
    let mut accounts = std::collections::BTreeMap::new();
    for row in rows {
        let (account, count, mut client) = row?;
        if client["owned"] == false {
            use hmac::{Hmac, Mac};
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key)?;
            mac.update(b"exetrouter-global-client-v1\0");
            mac.update(client["id"].as_str().expect("token ID").as_bytes());
            client["id"] =
                format!("client_{}", hex::encode(&mac.finalize().into_bytes()[..16])).into();
            client["name"] = "Other user's client".into();
        }
        let activity = accounts
            .entry(account)
            .or_insert_with(|| empty_clients(now));
        activity["count"] = count.into();
        activity["truncated"] = (count > MAX_ACCOUNT_CLIENTS).into();
        activity["rows"]
            .as_array_mut()
            .expect("client rows")
            .push(client);
    }
    Ok(accounts)
}

pub(crate) fn empty_clients(now: i64) -> serde_json::Value {
    serde_json::json!({"scope":"server","count":0,"rows":[],"idle_seconds":CLIENT_IDLE_SECONDS,"observed_at":now,"truncated":false})
}

#[cfg(test)]
fn account_clients(
    conn: &Connection,
    user: i64,
    account: i64,
    now: i64,
) -> Result<serde_json::Value> {
    Ok(clients_by_account(conn, &[1; 32], user, now)?
        .remove(&account)
        .unwrap_or_else(|| empty_clients(now)))
}

pub fn record_usage(conn: &Connection, event: UsageEvent<'_>) -> Result<()> {
    if [
        event.input,
        event.output,
        event.cached_input,
        event.reasoning_output,
    ]
    .into_iter()
    .flatten()
    .any(|value| value < 0)
        || event
            .cached_input
            .zip(event.input)
            .is_some_and(|(cached, input)| cached > input)
        || event
            .reasoning_output
            .zip(event.output)
            .is_some_and(|(reasoning, output)| reasoning > output)
    {
        return Err("invalid usage counters".into());
    }
    conn.execute(
        "INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,input_tokens,output_tokens,cached_input_tokens,reasoning_output_tokens)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![Utc::now().timestamp(),event.user_id,event.token_id,event.surface,event.model,event.status,event.input,event.output,event.cached_input,event.reasoning_output],
    )?;
    Ok(())
}

pub fn usage_report(conn: &Connection, period: &str, by: Option<&str>) -> Result<UsageReport> {
    report_in_zone(conn, period, by, Utc::now(), Moscow, None)
}

pub fn usage_report_in_timezone(
    conn: &Connection,
    period: &str,
    by: Option<&str>,
    timezone: &str,
) -> Result<UsageReport> {
    let zone: Tz = timezone.parse().map_err(|_| "invalid usage timezone")?;
    report_in_zone(conn, period, by, Utc::now(), zone, None)
}

/// Token grouping is private to the authenticated user; other groups remain shared.
pub fn usage_report_for_user(
    conn: &Connection,
    user_id: i64,
    period: &str,
    by: Option<&str>,
    timezone: Option<&str>,
) -> Result<UsageReport> {
    if by != Some("token") {
        return match timezone {
            Some(zone) => usage_report_in_timezone(conn, period, by, zone),
            None => usage_report(conn, period, by),
        };
    }
    let zone = match timezone {
        Some(value) => value.parse().map_err(|_| "invalid usage timezone")?,
        None => Moscow,
    };
    let scope = (by == Some("token")).then_some(user_id);
    report_in_zone(conn, period, by, Utc::now(), zone, scope)
}

#[cfg(test)]
fn report_at(
    conn: &Connection,
    period: &str,
    by: Option<&str>,
    now: DateTime<Utc>,
) -> Result<UsageReport> {
    report_in_zone(conn, period, by, now, Moscow, None)
}

fn report_in_zone(
    conn: &Connection,
    period: &str,
    by: Option<&str>,
    now: DateTime<Utc>,
    zone: Tz,
    user_id: Option<i64>,
) -> Result<UsageReport> {
    let now = now.with_timezone(&zone);
    let (from, to) = if period == "24h" {
        (now.timestamp() - 24 * 3600, now.timestamp())
    } else {
        let today = now.date_naive();
        let start = match period {
            "day" => today,
            "week" => today - Duration::days(today.weekday().num_days_from_monday() as i64),
            "month" => today.with_day(1).ok_or("invalid date")?,
            _ => return Err("period must be day, 24h, week or month".into()),
        };
        let end = match period {
            "day" => start + Duration::days(1),
            "week" => start + Duration::days(7),
            _ => {
                if start.month() == 12 {
                    start
                        .with_year(start.year() + 1)
                        .and_then(|d| d.with_month(1))
                        .ok_or("invalid date")?
                } else {
                    start.with_month(start.month() + 1).ok_or("invalid date")?
                }
            }
        };
        let from = zone
            .from_local_datetime(&start.and_hms_opt(0, 0, 0).ok_or("invalid time")?)
            .single()
            .ok_or("ambiguous time")?
            .timestamp();
        let to = zone
            .from_local_datetime(&end.and_hms_opt(0, 0, 0).ok_or("invalid time")?)
            .single()
            .ok_or("ambiguous time")?
            .timestamp();
        (from, to)
    };
    let group = match by {
        Some("user") => "u.name",
        Some("model") => "e.model",
        Some("token") => "e.token_id",
        None => "'all'",
        _ => return Err("by must be user, model or token".into()),
    };
    let rows = aggregate(conn, group, from, to, user_id)?;
    let mut timeline = Vec::new();
    let mut cursor = from;
    while cursor < to {
        let next = if matches!(period, "day" | "24h") {
            (cursor + 3600).min(to)
        } else {
            let date = DateTime::from_timestamp(cursor, 0)
                .ok_or("invalid bucket time")?
                .with_timezone(&zone)
                .date_naive()
                + Duration::days(1);
            zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or("invalid bucket time")?)
                .earliest()
                .ok_or("invalid local midnight")?
                .timestamp()
                .min(to)
        };
        timeline.push(UsageBucket {
            from_utc: cursor,
            to_utc: next,
            rows: aggregate(conn, group, cursor, next, user_id)?,
        });
        cursor = next;
    }
    Ok(UsageReport {
        period: period.to_owned(),
        timezone: zone.name().into(),
        from_utc: from,
        to_utc: to,
        rows,
        timeline,
    })
}

/// Fixed rolling windows, pageable with an exclusive Unix end timestamp.
pub fn usage_range_for_user(
    conn: &Connection,
    user: i64,
    period: &str,
    by: Option<&str>,
    timezone: Option<&str>,
    before: Option<i64>,
) -> Result<UsageReport> {
    let seconds = match period {
        "24h" => 86400,
        "7d" => 7 * 86400,
        "30d" => 30 * 86400,
        "90d" => 90 * 86400,
        _ => return Err("period must be 24h, 7d, 30d or 90d".into()),
    };
    let zone: Tz = timezone
        .unwrap_or("Europe/Moscow")
        .parse()
        .map_err(|_| "invalid usage timezone")?;
    let now = Utc::now().timestamp();
    let to = before.unwrap_or(now);
    if to < seconds || to > now {
        return Err("invalid range end".into());
    }
    let from = to - seconds;
    let group = match by {
        None => "'all'",
        Some("user") => "u.name",
        Some("model") => "e.model",
        Some("token") => "e.token_id",
        _ => return Err("by must be user, model or token".into()),
    };
    let scope = (by == Some("token")).then_some(user);
    let step = if period == "24h" { 3600 } else { 86400 };
    let mut timeline = Vec::new();
    let mut cursor = from;
    while cursor < to {
        let next = (cursor + step).min(to);
        timeline.push(UsageBucket {
            from_utc: cursor,
            to_utc: next,
            rows: aggregate(conn, group, cursor, next, scope)?,
        });
        cursor = next;
    }
    Ok(UsageReport {
        period: period.into(),
        timezone: zone.name().into(),
        from_utc: from,
        to_utc: to,
        rows: aggregate(conn, group, from, to, scope)?,
        timeline,
    })
}

fn aggregate(
    conn: &Connection,
    group: &str,
    from: i64,
    to: i64,
    user_id: Option<i64>,
) -> Result<Vec<UsageRow>> {
    // Activation is initiated by an authenticated SSH/local user, without an API token.
    let source = if group == "e.token_id" {
        "usage_events"
    } else {
        "(SELECT at_utc,user_id,token_id,model,input_tokens,output_tokens,cached_input_tokens,reasoning_output_tokens FROM usage_events UNION ALL SELECT attempted_at,user_id,NULL,model,input_tokens,output_tokens,cached_input_tokens,reasoning_output_tokens FROM quota_activation_attempts)"
    };
    let label = if group == "e.token_id" && user_id.is_some() {
        "(SELECT t.name FROM access_tokens t WHERE t.id=e.token_id AND t.user_id=e.user_id)"
    } else {
        "NULL"
    };
    // Rates are standard API USD per million tokens (not subscription charges).
    // Keep pricing at the request level: aggregating Astra and Sol tokens first
    // would erase both the model rate and the >272K long-context tier.
    let prices = "VALUES
        ('gpt-6-astra',10.0,50.0,1.0,20.0,75.0,2.0),
        ('gpt-6.1-sol',2.0,10.0,0.1,4.0,15.0,0.2),
        ('gpt-6.1-terra',1.0,5.0,0.05,2.0,7.5,0.1),
        ('gpt-6-luna',0.1,0.5,0.01,0.2,0.75,0.02),
        ('gpt-6-sol',2.0,10.0,0.2,4.0,15.0,0.4),
        ('gpt-5.6-sol',4.0,20.0,0.4,8.0,30.0,0.8),
        ('gpt-5.6-terra',2.0,12.0,0.2,4.0,18.0,0.4),
        ('gpt-5.6-luna',0.2,1.2,0.02,0.4,1.8,0.04),
        ('gpt-5.5',5.0,30.0,0.5,10.0,45.0,1.0)";
    let price = "CASE WHEN p.model IS NOT NULL AND e.input_tokens IS NOT NULL AND e.output_tokens IS NOT NULL
        AND e.cached_input_tokens IS NOT NULL AND e.cached_input_tokens BETWEEN 0 AND e.input_tokens
        THEN ((e.input_tokens-e.cached_input_tokens)*
          CASE WHEN e.input_tokens>272000 THEN p.long_input ELSE p.short_input END
          + e.cached_input_tokens*CASE WHEN e.input_tokens>272000 THEN p.long_cached ELSE p.short_cached END
          + e.output_tokens*CASE WHEN e.input_tokens>272000 THEN p.long_output ELSE p.short_output END)/1000000.0 END";
    let sql = format!("WITH p(model,short_input,short_output,short_cached,long_input,long_output,long_cached) AS ({prices}) SELECT {group},COUNT(*),
        SUM(CASE WHEN e.input_tokens IS NULL OR e.output_tokens IS NULL THEN 1 ELSE 0 END),
        SUM(e.input_tokens),SUM(e.output_tokens),SUM(e.cached_input_tokens),SUM(e.reasoning_output_tokens),
        SUM(CASE WHEN e.input_tokens IS NOT NULL AND e.output_tokens IS NOT NULL THEN 1 ELSE 0 END),
        SUM(CASE WHEN (e.input_tokens IS NULL) != (e.output_tokens IS NULL) THEN 1 ELSE 0 END),
        {label},SUM({price}),SUM({price} IS NULL)
        FROM {source} e JOIN users u ON u.id=e.user_id
        LEFT JOIN p ON p.model=e.model
        WHERE e.at_utc>=?1 AND e.at_utc<?2 AND (?3 IS NULL OR e.user_id=?3) GROUP BY {group} ORDER BY {group}");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![from, to, user_id], |r| {
            let input: Option<i64> = r.get(3)?;
            let output: Option<i64> = r.get(4)?;
            let total = input
                .zip(output)
                .map(|(i, o)| i.checked_add(o).ok_or(rusqlite::Error::InvalidQuery))
                .transpose()?;
            Ok(UsageRow {
                name: r.get(0)?,
                display_name: r.get(9)?,
                requests: r.get(1)?,
                unknown_usage: r.get(2)?,
                known_usage: r.get(7)?,
                partial_usage: r.get(8)?,
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: r.get(5)?,
                reasoning_output_tokens: r.get(6)?,
                total_tokens: total,
                estimated_usd: r.get(10)?,
                unpriced_requests: r.get(11)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{create_token, create_user, init};

    #[test]
    fn rolling_pages_have_exact_exclusive_boundaries_and_private_token_rows() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let alice = create_user(&db, "alice").unwrap();
        let bob = create_user(&db, "bob").unwrap();
        let first = create_token(&db, &[1; 32], alice, "first", 1).unwrap();
        let foreign = create_token(&db, &[1; 32], bob, "foreign", 1).unwrap();
        let end = 1_700_000_000;
        for (period, days, buckets) in [
            ("24h", 1, 24),
            ("7d", 7, 7),
            ("30d", 30, 30),
            ("90d", 90, 90),
        ] {
            db.execute("DELETE FROM usage_events", []).unwrap();
            let duration = days * 86400;
            for at in [
                end - 2 * duration,
                end - duration - 1,
                end - duration,
                end - 1,
                end,
            ] {
                for (user, token) in [(alice, &first.token.id), (bob, &foreign.token.id)] {
                    db.execute("INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,input_tokens) VALUES(?1,?2,?3,'responses','test','done',10)",params![at,user,token]).unwrap();
                }
            }
            let current = usage_range_for_user(
                &db,
                alice,
                period,
                Some("token"),
                Some("America/New_York"),
                Some(end),
            )
            .unwrap();
            let previous = usage_range_for_user(
                &db,
                alice,
                period,
                Some("token"),
                None,
                Some(current.from_utc),
            )
            .unwrap();
            assert_eq!(current.timeline.len(), buckets);
            assert_eq!(previous.to_utc, current.from_utc);
            assert_eq!(current.to_utc - current.from_utc, duration);
            assert_eq!(current.rows.len(), 1);
            assert_eq!(current.rows[0].name, first.token.id);
            assert_eq!(current.rows[0].requests, 2);
            assert_eq!(previous.rows[0].requests, 2);
            assert_eq!(current.rows[0].output_tokens, None);
            assert_eq!(current.rows[0].estimated_usd, None);
            assert_eq!(current.rows[0].unpriced_requests, 2);
            assert_eq!(
                current
                    .timeline
                    .iter()
                    .flat_map(|b| &b.rows)
                    .map(|r| r.requests)
                    .sum::<i64>(),
                2
            );
            assert!(current
                .timeline
                .windows(2)
                .all(|b| b[0].to_utc == b[1].from_utc));
            let shared = usage_range_for_user(&db, alice, period, None, None, Some(end)).unwrap();
            assert_eq!(shared.rows[0].requests, 4);
        }
        assert!(usage_range_for_user(&db, alice, "month", None, None, Some(end)).is_err());
        assert!(usage_range_for_user(&db, alice, "24h", None, None, Some(1)).is_err());
        assert!(usage_range_for_user(&db, alice, "24h", None, None, Some(i64::MAX)).is_err());
    }

    #[test]
    fn usd_estimates_price_each_model_and_long_context_before_aggregation() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "alice").unwrap();
        let token = create_token(&db, &[1; 32], user, "fixture", 1).unwrap();
        for (model, input, cached, output) in [
            (
                "gpt-6-astra",
                Some(1_000_000),
                Some(500_000),
                Some(1_000_000),
            ),
            (
                "gpt-6.1-sol",
                Some(1_000_000),
                Some(500_000),
                Some(1_000_000),
            ),
            ("gpt-6-luna", Some(100), Some(0), Some(100)),
            ("gpt-image-2", Some(100), Some(0), Some(100)),
            ("gpt-6-astra", None, None, None),
        ] {
            record_usage(
                &db,
                UsageEvent {
                    user_id: user,
                    token_id: &token.token.id,
                    surface: "responses",
                    model,
                    status: "completed",
                    input,
                    output,
                    cached_input: cached,
                    reasoning_output: None,
                },
            )
            .unwrap();
        }
        let report = usage_report_for_user(&db, user, "day", None, Some("UTC")).unwrap();
        let row = &report.rows[0];
        // Both million-input requests exceed the 272K long-context threshold.
        // Astra: 500K*20 + 500K*2 + 1M*75 = $86;
        // Sol: 500K*4 + 500K*0.2 + 1M*15 = $17.10.
        let expected = 103.10 + 100.0 * (0.1 + 0.5) / 1_000_000.0;
        assert!((row.estimated_usd.unwrap() - expected).abs() < 1e-9);
        assert_eq!(row.unpriced_requests, 2);
        let models = usage_report_for_user(&db, user, "day", Some("model"), Some("UTC")).unwrap();
        let image = models
            .rows
            .iter()
            .find(|r| r.name == "gpt-image-2")
            .unwrap();
        assert!(image.estimated_usd.is_none());
        assert_eq!(image.unpriced_requests, 1);
        let hourly = report
            .timeline
            .iter()
            .flat_map(|b| &b.rows)
            .find(|r| r.requests == 5)
            .unwrap();
        assert_eq!(hourly.unpriced_requests, 2);
        assert!((hourly.estimated_usd.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn token_grouping_is_private_and_retains_revoked_history() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let alice = create_user(&db, "alice").unwrap();
        let bob = create_user(&db, "bob").unwrap();
        let first = create_token(&db, &[1; 32], alice, "first", 1).unwrap();
        let second = create_token(&db, &[1; 32], alice, "second", 1).unwrap();
        let foreign = create_token(&db, &[1; 32], bob, "foreign", 1).unwrap();
        for (user, token, input) in [
            (alice, &first.token.id, Some(10)),
            (alice, &second.token.id, None),
            (bob, &foreign.token.id, Some(100)),
        ] {
            record_usage(
                &db,
                UsageEvent {
                    user_id: user,
                    token_id: token,
                    surface: "responses",
                    model: "test",
                    status: "done",
                    input,
                    output: input,
                    cached_input: None,
                    reasoning_output: None,
                },
            )
            .unwrap();
        }
        crate::revoke_token(&db, alice, &first.token.id).unwrap();
        let report = usage_report_for_user(&db, alice, "day", Some("token"), Some("UTC")).unwrap();
        assert_eq!(report.rows.len(), 2);
        assert!(report.rows.iter().all(|r| r.name != foreign.token.id));
        let known = report
            .rows
            .iter()
            .find(|r| r.name == first.token.id)
            .unwrap();
        assert_eq!(known.total_tokens, Some(20));
        assert_eq!(known.display_name.as_deref(), Some("first"));
        let unknown = report
            .rows
            .iter()
            .find(|r| r.name == second.token.id)
            .unwrap();
        assert_eq!(unknown.total_tokens, None);
        assert_eq!(unknown.unknown_usage, 1);
        assert_eq!(unknown.display_name.as_deref(), Some("second"));
        assert!(report
            .timeline
            .iter()
            .flat_map(|b| &b.rows)
            .all(|r| r.name != foreign.token.id));
        let shared = usage_report_for_user(&db, alice, "day", Some("user"), Some("UTC")).unwrap();
        assert_eq!(shared.rows.len(), 2);
        assert!(shared.rows.iter().all(|row| row.display_name.is_none()));
        let unscoped = usage_report(&db, "day", Some("token")).unwrap();
        assert!(unscoped.rows.iter().all(|row| row.display_name.is_none()));
        db.execute(
            "UPDATE access_tokens SET name='same label' WHERE user_id=?1",
            [alice],
        )
        .unwrap();
        let renamed = usage_report_for_user(&db, alice, "day", Some("token"), Some("UTC")).unwrap();
        assert_eq!(renamed.rows.len(), 2);
        assert!(renamed
            .rows
            .iter()
            .chain(renamed.timeline.iter().flat_map(|bucket| &bucket.rows))
            .all(|row| row.display_name.as_deref() == Some("same label")));
    }

    #[test]
    fn recovery_marks_pending_requests_unknown_and_finalization_is_unique() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "alice").unwrap();
        let token = create_token(&db, &[1; 32], user, "dev", 1).unwrap();
        let account = crate::oauth::save(
            &db,
            &crate::oauth::Vault::new([2; 32]),
            "fixture-account",
            &crate::oauth::Credentials {
                access_token: "access".into(),
                refresh_token: "refresh".into(),
            },
            i64::MAX,
        )
        .unwrap();
        let mut request = RequestRecord {
            request_id: "request-one".into(),
            user_id: user,
            token_id: token.token.id,
            account_id: account,
            surface: "responses",
            model: "test".into(),
            client_transport: "http_sse",
        };
        begin_request(&db, &request).unwrap();
        let outcome = RequestOutcome {
            status: "completed",
            counters: Counters {
                input: Some(0),
                output: Some(0),
                ..Counters::default()
            },
            upstream_transport: "http_sse",
            upstream_request_id: None,
            upstream_response_id: Some("response-one".into()),
            duration_ms: 10,
        };
        finish_request(&db, &request.request_id, &outcome).unwrap();
        assert!(finish_request(&db, &request.request_id, &outcome).is_err());
        request.request_id = "request-two".into();
        begin_request(&db, &request).unwrap();
        assert_eq!(recover_requests(&db).unwrap(), 1);
        assert_eq!(recover_requests(&db).unwrap(), 0);
        let state: String = db
            .query_row(
                "SELECT status FROM usage_events WHERE request_id='request-two'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "aborted_unknown");
        let report = usage_report(&db, "day", None).unwrap();
        assert_eq!(report.rows[0].requests, 2);
        assert_eq!(report.rows[0].known_usage, 1);
        assert_eq!(report.rows[0].unknown_usage, 1);
        assert_eq!(report.rows[0].total_tokens, Some(0));
    }

    #[test]
    fn account_clients_are_global_private_deduplicated_expire_and_recover_running_requests() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "client-owner").unwrap();
        let other = create_user(&db, "other-owner").unwrap();
        let observer = create_user(&db, "observer").unwrap();
        let recent = create_token(&db, &[1; 32], user, "shared label", 1)
            .unwrap()
            .token
            .id;
        let running = create_token(&db, &[1; 32], user, "shared label", 1)
            .unwrap()
            .token
            .id;
        let hidden = create_token(&db, &[1; 32], other, "foreign private name", 1)
            .unwrap()
            .token
            .id;
        for id in [1, 2] {
            db.execute("INSERT INTO oauth_accounts(id,account_id,state,encrypted_credentials,expires_at,created_at) VALUES(?1,?2,'active',X'00',999999,0)",params![id,format!("synthetic-{id}")]).unwrap();
        }
        let now = 100_000;
        let insert = |owner, token: &str, account, at, status: &str, request: &str| {
            db.execute("INSERT INTO usage_events(at_utc,user_id,token_id,account_id,api_surface,model,status,request_id) VALUES(?1,?2,?3,?4,'responses','synthetic',?5,?6)",params![at,owner,token,account,status,request]).unwrap();
        };
        insert(user, &recent, 1, now - 20, "completed", "recent-one");
        insert(user, &recent, 1, now - 10, "completed", "recent-two");
        insert(user, &recent, 2, now - 5, "completed", "second-account");
        insert(user, &running, 1, now - 3600, "sent", "long-request");
        insert(other, &hidden, 1, now - 1, "accepted", "foreign-request");
        insert(
            other,
            &hidden,
            2,
            now - 1,
            "completed",
            "foreign-second-account",
        );
        let report = account_clients(&db, user, 1, now).unwrap();
        assert_eq!(report["scope"], "server");
        assert_eq!(report["count"], 3);
        assert_eq!(report["rows"][0]["owned"], false);
        assert_eq!(report["rows"][0]["name"], "Other user's client");
        assert_eq!(report["rows"][0]["in_flight"], 1);
        assert_eq!(report["rows"][0]["last_request_at"], now - 1);
        assert_eq!(report["rows"][1]["id"], running);
        assert_eq!(report["rows"][1]["owned"], true);
        assert_eq!(report["rows"][1]["in_flight"], 1);
        assert_eq!(report["rows"][2]["last_request_at"], now - 10);
        assert_eq!(report["rows"][2]["name"], "shared label");
        assert!(!report.to_string().contains("foreign"));
        assert!(!report.to_string().contains(&hidden));
        let second = account_clients(&db, user, 2, now).unwrap();
        assert_eq!(second["count"], 2);
        assert_eq!(second["rows"][0]["id"], report["rows"][0]["id"]);
        assert_eq!(report, account_clients(&db, user, 1, now).unwrap());
        let observed = account_clients(&db, observer, 1, now).unwrap();
        assert_eq!(observed["count"], 3);
        assert_eq!(observed["rows"][0], report["rows"][0]);
        for id in [&recent, &running, &hidden] {
            assert!(!observed.to_string().contains(id));
        }
        let owned = account_clients(&db, other, 1, now).unwrap();
        assert_eq!(owned["count"], 3);
        assert_eq!(owned["rows"][0]["id"], hidden);
        assert_eq!(owned["rows"][0]["name"], "foreign private name");
        let different_key = clients_by_account(&db, &[2; 32], user, now).unwrap();
        assert_ne!(different_key[&1]["rows"][0]["id"], report["rows"][0]["id"]);
        assert_eq!(
            account_clients(&db, user, 1, now + CLIENT_IDLE_SECONDS - 10).unwrap()["count"],
            2
        );
        recover_requests(&db).unwrap();
        assert_eq!(
            account_clients(&db, user, 1, now + CLIENT_IDLE_SECONDS).unwrap()["count"],
            0
        );
        insert(
            user,
            &recent,
            1,
            now + CLIENT_IDLE_SECONDS,
            "completed",
            "returning-client",
        );
        assert_eq!(
            account_clients(&db, user, 1, now + CLIENT_IDLE_SECONDS).unwrap()["count"],
            1
        );
    }

    #[test]
    fn account_client_list_is_bounded_with_exact_count_and_running_tokens_first() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "synthetic").unwrap();
        let other = create_user(&db, "other").unwrap();
        db.execute("INSERT INTO oauth_accounts(id,account_id,state,encrypted_credentials,expires_at,created_at) VALUES(1,'synthetic','active',X'00',999999,0)",[]).unwrap();
        let now = 100_000;
        for i in 0..130 {
            let owner = if i % 2 == 0 { user } else { other };
            let token = create_token(&db, &[1; 32], owner, "Synthetic client", 1)
                .unwrap()
                .token
                .id;
            let (at, status) = if i == 129 {
                (now - 3600, "sent")
            } else {
                (now - i, "completed")
            };
            db.execute("INSERT INTO usage_events(at_utc,user_id,token_id,account_id,api_surface,model,status,request_id) VALUES(?1,?2,?3,1,'responses','synthetic',?4,?5)",params![at,owner,token,status,format!("synthetic-request-{i}")]).unwrap();
        }
        let report = account_clients(&db, user, 1, now).unwrap();
        assert_eq!(report["count"], 130);
        assert_eq!(report["rows"].as_array().unwrap().len(), 128);
        assert_eq!(report["truncated"], true);
        assert_eq!(report["rows"][0]["in_flight"], 1);
        assert_eq!(report["rows"][0]["owned"], false);
        assert_eq!(account_clients(&db, other, 1, now).unwrap()["count"], 130);
    }

    fn timestamp(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn rolling_24_hours_uses_exact_boundaries_and_hourly_buckets_across_dst() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "rolling").unwrap();
        let token = create_token(&db, &[1; 32], user, "fixture", 1).unwrap();
        let zone: Tz = "America/New_York".parse().unwrap();
        for now in ["2026-03-08T12:37:00Z", "2026-11-01T12:37:00Z"] {
            db.execute("DELETE FROM usage_events", []).unwrap();
            let now = timestamp(now);
            let from = now.timestamp() - 86400;
            for at in [
                from - 1,
                from,
                from + 3600,
                now.timestamp() - 1,
                now.timestamp(),
            ] {
                db.execute("INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,input_tokens,output_tokens) VALUES(?1,?2,?3,'responses','model-a','failed',10,NULL)", params![at,user,token.token.id]).unwrap();
            }
            let report = report_in_zone(&db, "24h", Some("model"), now, zone, None).unwrap();
            assert_eq!(report.from_utc, from);
            assert_eq!(report.to_utc, now.timestamp());
            assert_eq!(report.timeline.len(), 24);
            assert!(report
                .timeline
                .iter()
                .all(|bucket| bucket.to_utc - bucket.from_utc == 3600));
            assert_eq!(report.rows[0].requests, 3);
            assert_eq!(report.rows[0].unknown_usage, 3);
            assert_eq!(report.rows[0].total_tokens, None);
            assert_eq!(
                report
                    .timeline
                    .iter()
                    .flat_map(|bucket| &bucket.rows)
                    .map(|row| row.requests)
                    .sum::<i64>(),
                3
            );
            let day = report_in_zone(&db, "day", None, now, zone, None).unwrap();
            assert_ne!(day.from_utc, from);
        }
    }

    #[test]
    fn local_timeline_preserves_dst_and_unknown_counters() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "timeline").unwrap();
        let token = create_token(&db, &[1; 32], user, "fixture", 1).unwrap();
        let zone: Tz = "America/New_York".parse().unwrap();
        for (now, hours) in [("2026-03-08T12:00:00Z", 23), ("2026-11-01T12:00:00Z", 25)] {
            let report = report_in_zone(&db, "day", None, timestamp(now), zone, None).unwrap();
            assert_eq!(report.timeline.len(), hours);
            assert_eq!(report.to_utc - report.from_utc, hours as i64 * 3600);
        }
        let now = timestamp("2026-03-08T12:00:00Z");
        let day = report_in_zone(&db, "day", Some("model"), now, zone, None).unwrap();
        for at in [day.from_utc, day.from_utc + 3600, day.to_utc - 1] {
            db.execute("INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,input_tokens,output_tokens) VALUES(?1,?2,?3,'responses','model-a','failed',10,NULL)",params![at,user,token.token.id]).unwrap();
        }
        let report = report_in_zone(&db, "day", Some("model"), now, zone, None).unwrap();
        assert_eq!(report.timezone, "America/New_York");
        assert_eq!(
            report
                .timeline
                .iter()
                .flat_map(|b| &b.rows)
                .map(|r| r.requests)
                .sum::<i64>(),
            3
        );
        assert_eq!(report.timeline[0].rows[0].name, "model-a");
        assert_eq!(report.timeline[0].rows[0].total_tokens, None);
        assert_eq!(report.timeline[0].rows[0].unknown_usage, 1);
        let week = report_in_zone(&db, "week", None, now, zone, None).unwrap();
        assert_eq!(week.timeline.len(), 7);
        assert_eq!(
            week.timeline.last().unwrap().to_utc - week.timeline.last().unwrap().from_utc,
            23 * 3600
        );
        assert!(usage_report_in_timezone(&db, "day", None, "invalid/timezone").is_err());
    }

    #[test]
    fn reports_use_moscow_midnight_monday_and_calendar_months() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let now = timestamp("2026-12-31T21:00:00Z"); // January 1 in Moscow.
        for (period, from, to) in [
            ("day", "2026-12-31T21:00:00Z", "2027-01-01T21:00:00Z"),
            ("week", "2026-12-27T21:00:00Z", "2027-01-03T21:00:00Z"),
            ("month", "2026-12-31T21:00:00Z", "2027-01-31T21:00:00Z"),
        ] {
            let report = report_at(&db, period, None, now).unwrap();
            assert_eq!(report.from_utc, timestamp(from).timestamp());
            assert_eq!(report.to_utc, timestamp(to).timestamp());
        }
        let december = report_at(&db, "month", None, timestamp("2026-12-31T20:59:59Z")).unwrap();
        assert_eq!(
            december.from_utc,
            timestamp("2026-11-30T21:00:00Z").timestamp()
        );
        assert_eq!(december.to_utc, now.timestamp());
        let monday = report_at(&db, "week", None, timestamp("2026-09-27T21:00:00Z")).unwrap();
        assert_eq!(
            monday.from_utc,
            timestamp("2026-09-27T21:00:00Z").timestamp()
        );
    }

    #[test]
    fn report_includes_start_and_excludes_end() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "alice").unwrap();
        let token = create_token(&db, &[1; 32], user, "dev", 1).unwrap();
        let now = timestamp("2026-09-30T12:00:00Z");
        let range = report_at(&db, "day", None, now).unwrap();
        for at in [
            range.from_utc - 1,
            range.from_utc,
            range.to_utc - 1,
            range.to_utc,
        ] {
            db.execute("INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status) VALUES(?1,?2,?3,'responses','test','failed')", params![at,user,token.token.id]).unwrap();
        }
        let report = report_at(&db, "day", None, now).unwrap();
        assert_eq!(report.rows[0].requests, 2);
        assert_eq!(report.rows[0].known_usage, 0);
        assert_eq!(report.rows[0].input_tokens, None);
        assert_eq!(report.rows[0].total_tokens, None);
    }

    #[test]
    fn usage_preserves_unknown_partial_zero_and_subset_counters() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        let user = create_user(&db, "alice").unwrap();
        let token = create_token(&db, &[1; 32], user, "dev", 1).unwrap();
        for (input, output, cached, reasoning) in [
            (None, None, None, None),
            (Some(10), None, None, None),
            (Some(0), Some(0), Some(0), Some(0)),
            (Some(100), Some(20), Some(50), Some(5)),
        ] {
            record_usage(
                &db,
                UsageEvent {
                    user_id: user,
                    token_id: &token.token.id,
                    surface: "responses",
                    model: "test",
                    status: "done",
                    input,
                    output,
                    cached_input: cached,
                    reasoning_output: reasoning,
                },
            )
            .unwrap();
        }
        let report = usage_report(&db, "day", Some("user")).unwrap();
        let row = &report.rows[0];
        assert_eq!(
            (
                row.requests,
                row.known_usage,
                row.unknown_usage,
                row.partial_usage
            ),
            (4, 2, 2, 1)
        );
        assert_eq!(
            (row.input_tokens, row.output_tokens, row.total_tokens),
            (Some(110), Some(20), Some(130))
        );
        assert_eq!(
            (row.cached_input_tokens, row.reasoning_output_tokens),
            (Some(50), Some(5))
        );
        db.execute(
            "DELETE FROM usage_events WHERE input_tokens IS NOT 0 OR output_tokens IS NOT 0",
            [],
        )
        .unwrap();
        let zero = usage_report(&db, "day", None).unwrap();
        assert_eq!(zero.rows[0].total_tokens, Some(0));
    }
}
