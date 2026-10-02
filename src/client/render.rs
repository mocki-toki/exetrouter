use serde_json::Value;

pub(super) fn safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn field(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => safe(s),
        Value::Null => "—".into(),
        other => other.to_string(),
    }
}
pub(super) fn date(v: &Value) -> String {
    v.as_i64()
        .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
        .map(|d| {
            use chrono::Datelike;
            let local = d.with_timezone(&chrono::Local);
            let format = if local.year() == chrono::Local::now().year() {
                "%A, %b %-d · %H:%M"
            } else {
                "%A, %b %-d, %Y · %H:%M"
            };
            local.format(format).to_string()
        })
        .unwrap_or_else(|| "—".into())
}
pub(super) fn system_timezone() -> String {
    std::env::var("TZ")
        .ok()
        .filter(|name| name.parse::<chrono_tz::Tz>().is_ok())
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".into())
}
pub(super) fn percent(v: &Value) -> String {
    v.as_f64()
        .filter(|p| p.is_finite())
        .map(|p| {
            let text = format!("{p:.1}");
            format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
        })
        .unwrap_or_else(|| "—".into())
}
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return "No entries.\n".into();
    }
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
                .max(h.len())
        })
        .collect();
    let line = |row: &[String]| {
        row.iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "{}{}",
                    s,
                    " ".repeat(widths[i].saturating_sub(s.chars().count()))
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    std::iter::once(line(
        &headers.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
    ))
    .chain(rows.iter().map(|r| line(r)))
    .collect::<Vec<_>>()
    .join("\n")
        + "\n"
}
pub(super) fn response(kind: &str, v: &Value) -> String {
    match kind {
        "models" => {
            let rows = v["data"].as_array().into_iter().flatten().rev().map(|m| vec![field(m,"id"), field(m,"display_name"), field(&m["exetrouter"],"context_window")]).collect::<Vec<_>>();
            format!("Available models\n\n{}\nExport metadata: exr models --json --format <codex-json|opencode-jsonc>", table(&["MODEL","NAME","CONTEXT"], &rows))
        }
        "tokens" => {
            let rows = v.as_array().into_iter().flatten().map(|t| vec![field(t,"id"),field(t,"name"), token_status(t),date(&t["expires_at"]),date(&t["last_used_at"])]).collect::<Vec<_>>();
            format!("Your API tokens\n\n{}",table(&["ID","NAME","STATUS","EXPIRES","LAST USED"], &rows))
        }
        "token" => format!("Token       {}\nName        {}\nStatus      {}\nCreated     {}\nExpires     {}\nLast used   {}\n",field(v,"id"),field(v,"name"),token_status(v),date(&v["created_at"]),date(&v["expires_at"]),date(&v["last_used_at"])),
        "revoke" => if v["revoked"].as_bool()==Some(true) { "Token revoked.\n".into() } else { "Token was already revoked or does not exist.\n".into() },
        "usage" => usage(v),
        "doctor" => format!("Server       Connected\nRouting      {}\nOAuth        {} active accounts · {} require login\nModels       {} · {}\nGenerations  {} active / {} per user\nWebSockets   {} active / {} per user",routing(v),field(&v["oauth"],"active_accounts"),field(&v["oauth"],"reauth_required_accounts"),field(&v["catalog"],"models"),catalog_status(v),field(&v["limits"]["generations"],"active_for_user"),field(&v["limits"]["generations"],"per_user_limit"),field(&v["limits"]["websockets"],"active_for_user"),field(&v["limits"]["websockets"],"per_user_limit")),
        _ => "Request completed.\n".into(),
    }
}
pub(super) fn routing(v: &Value) -> String {
    if v["configuration_status"] == "configured" {
        return "Ready".into();
    }
    if v["oauth"]["active_accounts"].as_u64() == Some(0) {
        return "No active OAuth accounts".into();
    }
    if v["pool"]["cooldown_accounts"].as_u64().unwrap_or(0) > 0 {
        return "Subscription cooldown; check account limits".into();
    }
    if v["pool"]["backoff_accounts"].as_u64().unwrap_or(0) > 0 {
        return "Upstream retry delay".into();
    }
    if v["catalog"]["status"] == "stale" {
        return "Catalog refresh on next request".into();
    }
    "Setup required; check OAuth and models".into()
}
fn catalog_status(v: &Value) -> String {
    if v["catalog"]["status"] == "stale" {
        "Cached · refreshes on next request".into()
    } else {
        field(&v["catalog"], "status")
    }
}
fn usage(v: &Value) -> String {
    let rows = v["rows"].as_array().cloned().unwrap_or_default();
    let mut text = format!(
        "Usage · {}\n{} → {}\nLocal time · {}\n\n",
        match v["period"].as_str() {
            Some("day") => "Today".into(),
            Some("24h") => "Last 24 hours".into(),
            Some("week") => "This week".into(),
            Some("month") => "This month".into(),
            _ => field(v, "period"),
        },
        date(&v["from_utc"]),
        date(&v["to_utc"]),
        system_timezone()
    );
    if rows.is_empty() {
        text.push_str("No requests in this period.\n");
    }
    for row in rows {
        text.push_str(&format!(
            "{} · {} requests\n  Input {} · Output {} · Cached {}\n",
            field(&row, "name"),
            field(&row, "requests"),
            field(&row, "input_tokens"),
            field(&row, "output_tokens"),
            field(&row, "cached_input_tokens")
        ));
        if row["unknown_usage"].as_i64().unwrap_or(0) > 0 {
            text.push_str(&format!(
                "  {} requests have incomplete token counters\n",
                field(&row, "unknown_usage")
            ));
        }
    }
    let timeline = v["timeline"].as_array().cloned().unwrap_or_default();
    let counts = timeline
        .iter()
        .map(|b| {
            b["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["requests"].as_i64())
                .sum::<i64>()
        })
        .collect::<Vec<_>>();
    if counts.iter().any(|n| *n > 0) {
        text.push_str("\nRequests over time · active intervals\n");
        for (bucket, count) in timeline.iter().zip(counts) {
            if count == 0 {
                continue;
            }
            if bucket["from_utc"]
                .as_i64()
                .is_some_and(|at| at > chrono::Utc::now().timestamp())
            {
                continue;
            }
            let label = bucket["from_utc"]
                .as_i64()
                .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format(if v["period"] == "day" {
                            "%H:%M"
                        } else {
                            "%a, %b %-d"
                        })
                        .to_string()
                })
                .unwrap_or_default();
            text.push_str(&format!("{label}: {count} requests\n"));
        }
    }
    text
}
pub(super) fn terminal(text: &str) -> String {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal()
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var("TERM").is_ok_and(|s| s == "dumb")
    {
        return text.into();
    }
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let code = if i == 0 {
                "1;36"
            } else if line.contains("stale") || line.contains("expired") {
                "33"
            } else if line.contains("Ready") || line.contains("Connected") {
                "32"
            } else {
                "0"
            };
            format!("\x1b[{code}m{line}\x1b[0m")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn token_status(v: &Value) -> String {
    if !v["revoked_at"].is_null() {
        "revoked"
    } else if v["expires_at"]
        .as_i64()
        .is_some_and(|at| at <= chrono::Utc::now().timestamp())
    {
        "expired"
    } else {
        "active"
    }
    .into()
}
pub(super) fn window_label(w: &Value) -> String {
    match w["window_minutes"].as_i64() {
        Some(10080) => "Weekly".into(),
        Some(480) => "8-hour".into(),
        Some(n) if n > 0 && n % 60 == 0 => format!("{}-hour", n / 60),
        Some(n) if n > 0 => format!("{n}-minute"),
        _ => format!("{} (duration unknown)", field(w, "kind")),
    }
}
pub(super) fn visible_windows(quota: &Value) -> Vec<Value> {
    quota["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["window_minutes"].as_i64().is_some_and(|n| n > 0))
        .cloned()
        .collect()
}
pub(super) fn limits(v: &Value) -> String {
    let mut accounts = v["quota_accounts"].as_array().cloned().unwrap_or_default();
    // Older single-account servers only expose quota.
    if accounts.is_empty() && v["quota"].is_object() {
        accounts.push(serde_json::json!({"label":"Email unavailable","quota":v["quota"]}));
    }
    let mut text = String::from("Upstream subscription limits\n\n");
    if accounts.is_empty() {
        text.push_str("No account quota observations available.\n");
    }
    for account in &accounts {
        text.push_str(&format!("{}\n", field(account, "label")));
        let q = &account["quota"];
        let windows = visible_windows(q);
        if windows.is_empty() {
            text.push_str(&format!("  Limits not reported · {}\n", field(q, "status")));
        }
        for w in &windows {
            text.push_str(&format!(
                "  {:8} {} remaining · {} used · {}\n",
                window_label(w),
                percent(&w["remaining_percent"]),
                percent(&w["used_percent"]),
                field(w, "status")
            ));
            text.push_str(&format!(
                "           Resets:   {}\n           Observed: {}\n",
                date(&w["reset_at"]),
                date(&w["observed_at"])
            ));
        }
        if let Some(until) = q["cooldown_until"].as_i64() {
            text.push_str(&format!(
                "  Cooldown until {} · {}\n",
                date(&Value::from(until)),
                field(q, "cooldown_source")
            ));
        }
        text.push_str(&format!("  {}\n", reset_credits(account)));
        if let Some(error) = account["refresh_error"].as_str() {
            text.push_str(&format!("  {}\n", safe(error)));
        }
        text.push('\n');
    }
    text.push_str("Only windows with a reported duration appear. Stale/reset-elapsed values are historical.\nRefresh reads live subscription limits; no inference is generated.\nUse a credit: exr limits reset <email> (requires confirmation and ≤5% remaining).");
    text
}
pub(super) fn reset_credits(account: &Value) -> String {
    let credits = &account["reset_credits"];
    let Some(count) = credits["available_count"].as_i64() else {
        return "Reset credits: unavailable".into();
    };
    let mut text = format!("Reset credits: {count} available");
    if let Some(expiry) = credits["credits"]
        .as_array()
        .and_then(|v| v.first())
        .and_then(|c| c["expires_at"].as_i64())
    {
        text.push_str(&format!(" · next expires {}", date(&Value::from(expiry))));
    }
    if account["refresh_error"].is_string()
        || credits["observed_at"]
            .as_i64()
            .is_some_and(|at| chrono::Utc::now().timestamp().saturating_sub(at) > 60)
    {
        text.push_str(" · historical");
    }
    text
}
pub(super) fn reset_confirmation(v: &Value) -> String {
    let mut text = format!("Account: {}\n{} remaining · {} reset credits available\nUse one credit: {}\nFree reset: {}\nCredit expires: {}\n",
        field(v,"email"),percent(&v["remaining_percent"]),field(v,"available_count"),field(v,"credit_title"),date(&v["free_reset_at"]),
        if v["credit_expires_at"].is_null(){"No expiry reported".into()}else{date(&v["credit_expires_at"])});
    if v["recommend_wait"] == true {
        text.push_str("\nFree reset is less than 3 days away. We recommend saving this credit and waiting for the free reset.\n");
    }
    text.push_str("\nThis consumes one credit. Confirm within 2 minutes.");
    text
}
pub(super) fn reset_outcome(v: &Value) -> String {
    match v["code"].as_str() {
        Some("reset") => format!(
            "Reset credit used. {} subscription windows reset.",
            field(v, "windows_reset")
        ),
        Some("nothing_to_reset") => "No subscription windows needed a reset.".into(),
        Some("no_credit") => "No reset credit available. Refresh limits.".into(),
        Some("already_redeemed") => {
            "This reset request was already redeemed. Refresh limits.".into()
        }
        _ => "Reset outcome unknown. Refresh limits before any further action.".into(),
    }
}

pub(super) fn accounts(value: &Value) -> String {
    if let Some(rows) = value.as_array() {
        if rows.is_empty() {
            return "No local accounts. Open Settings and choose Add account.".into();
        }
        return rows
            .iter()
            .map(|row| {
                format!(
                    "{} (ID {}) - {}",
                    field(row, "email"),
                    field(row, "id"),
                    field(row, "state")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if value["disabled"] == true {
        "Account disabled.".into()
    } else if value["account_id"].is_number() {
        format!("Account {} signed in.", field(value, "account_id"))
    } else {
        "Account unchanged.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_display_reverses_the_catalog_order() {
        let value = serde_json::json!({"data":[{"id":"first"},{"id":"second"},{"id":"last"}]});
        let text = response("models", &value);
        assert!(text.find("last").unwrap() < text.find("second").unwrap());
        assert!(text.find("second").unwrap() < text.find("first").unwrap());
    }
    #[test]
    fn quota_labels_use_actual_durations_and_preserve_unknown_and_stale() {
        let data = serde_json::json!({"quota_accounts":[{"label":"Account 1","quota":{"windows":[{"kind":"primary","window_minutes":480,"used_percent":12.5,"remaining_percent":87.5,"status":"stale"},{"kind":"secondary","window_minutes":10080,"used_percent":50,"remaining_percent":50,"status":"current"},{"kind":"primary","window_minutes":300,"status":"reset_elapsed"}]}}]});
        let text = limits(&data);
        assert!(text.contains("8-hour"));
        assert!(text.contains("Weekly"));
        assert!(text.contains("5-hour"));
        assert!(text.contains("87.5%"));
        assert!(text.contains("stale"));
        assert!(text.contains("reset_elapsed"));
        assert!(!limits(&serde_json::json!({})).contains("8-hour"));
    }
    #[test]
    fn quota_hides_unknown_windows_and_formats_whole_percentages() {
        let data = serde_json::json!({"quota_accounts":[{"label":"Account 1","quota":{"windows":[{"kind":"primary","window_minutes":10080,"used_percent":2,"remaining_percent":98,"status":"stale"},{"kind":"secondary","window_minutes":null,"remaining_percent":100}]}}]});
        let text = limits(&data);
        assert!(text.contains("98%"));
        assert!(!text.contains("98.0%"));
        assert!(text.contains("Weekly"));
        assert!(!text.contains("secondary"));
        assert!(!text.contains("100%"));
        // Filtering is presentation-only; structured observations remain intact.
        assert_eq!(
            data["quota_accounts"][0]["quota"]["windows"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let status = serde_json::json!({"configuration_status":"needs_attention","oauth":{"active_accounts":2},"catalog":{"status":"stale"}});
        let overview = response("doctor", &status);
        assert!(overview.contains("Connected"));
        assert!(overview.contains("refresh on next request"));
        assert!(!overview.contains("needs_attention"));
    }
    #[test]
    fn human_output_is_not_json_and_cannot_inject_terminal_controls() {
        let text = response(
            "tokens",
            &serde_json::json!([{"id":"tok_1","name":"evil\u{1b}[2J\nname","expires_at":9999999999i64,"revoked_at":null}]),
        );
        assert!(text.contains("Your API tokens"));
        assert!(text.contains("tok_1"));
        assert!(!text.contains('\u{1b}'));
        assert!(!text.starts_with('['));
    }
}
