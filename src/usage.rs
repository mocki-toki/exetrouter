use crate::Result;
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use chrono_tz::{Europe::Moscow, Tz};
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct UsageRow {
    pub name: String,
    pub requests: i64,
    pub unknown_usage: i64,
    pub known_usage: i64,
    pub partial_usage: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
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

fn aggregate(
    conn: &Connection,
    group: &str,
    from: i64,
    to: i64,
    user_id: Option<i64>,
) -> Result<Vec<UsageRow>> {
    let sql = format!("SELECT {group},COUNT(*),
        SUM(CASE WHEN e.input_tokens IS NULL OR e.output_tokens IS NULL THEN 1 ELSE 0 END),
        SUM(e.input_tokens),SUM(e.output_tokens),SUM(e.cached_input_tokens),SUM(e.reasoning_output_tokens),
        SUM(CASE WHEN e.input_tokens IS NOT NULL AND e.output_tokens IS NOT NULL THEN 1 ELSE 0 END),
        SUM(CASE WHEN (e.input_tokens IS NULL) != (e.output_tokens IS NULL) THEN 1 ELSE 0 END)
        FROM usage_events e JOIN users u ON u.id=e.user_id
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
                requests: r.get(1)?,
                unknown_usage: r.get(2)?,
                known_usage: r.get(7)?,
                partial_usage: r.get(8)?,
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: r.get(5)?,
                reasoning_output_tokens: r.get(6)?,
                total_tokens: total,
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
        let unknown = report
            .rows
            .iter()
            .find(|r| r.name == second.token.id)
            .unwrap();
        assert_eq!(unknown.total_tokens, None);
        assert_eq!(unknown.unknown_usage, 1);
        assert!(report
            .timeline
            .iter()
            .flat_map(|b| &b.rows)
            .all(|r| r.name != foreign.token.id));
        let shared = usage_report_for_user(&db, alice, "day", Some("user"), Some("UTC")).unwrap();
        assert_eq!(shared.rows.len(), 2);
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
