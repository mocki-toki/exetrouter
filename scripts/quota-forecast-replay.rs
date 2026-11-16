//! Offline replay of the actual runtime estimator, never a second implementation.
use clap::Parser;
use exetrouter::{quota::Window, quota_forecast, quota_history::Point};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{collections::BTreeSet, fs, io::Write, path::PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(about = "Replay private recorded quota metadata without contacting any service")]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Deserialize)]
struct Dataset {
    schema_version: u8,
    collected_at: i64,
    accounts: Vec<Account>,
}

#[derive(Deserialize)]
struct Account {
    account: String,
    interval_seconds: i64,
    points: Vec<Point>,
}

#[derive(Serialize)]
struct Record {
    account: String,
    point: Point,
    forecast: Option<quota_forecast::Forecast>,
}

fn validate(data: &Dataset) -> Result<()> {
    if data.schema_version != 1 || data.collected_at < 0 || data.accounts.len() > 128 {
        return Err("invalid dataset".into());
    }
    let mut names = BTreeSet::new();
    for a in &data.accounts {
        let alias = a.account.strip_prefix("account-").unwrap_or_default();
        if alias.is_empty()
            || alias.len() > 16
            || !alias.bytes().all(|b| b.is_ascii_digit())
            || !names.insert(&a.account)
            || a.interval_seconds != 600
            || a.points.len() > 25920
        {
            return Err("invalid dataset".into());
        }
        let mut seen = BTreeSet::new();
        for p in &a.points {
            if !matches!(p.kind.as_str(), "primary" | "secondary")
                || p.hour < 0
                || p.hour % 600 != 0
                || !(p.hour..p.hour.saturating_add(600)).contains(&p.observed_at)
                || p.observed_at > data.collected_at
                || !p.used_percent.is_finite()
                || p.used_percent < 0.0
                || p.window_minutes.is_some_and(|m| m <= 0)
                || p.reset_at.is_some_and(|at| at < 0)
                || !seen.insert((&p.kind, p.hour))
            {
                return Err("invalid dataset".into());
            }
        }
    }
    Ok(())
}

fn records(a: &Account, collected_at: i64) -> Vec<Record> {
    let mut output = Vec::new();
    for kind in ["primary", "secondary"] {
        let mut points: Vec<_> = a
            .points
            .iter()
            .filter(|p| p.kind == kind && p.hour + 600 <= collected_at)
            .cloned()
            .collect();
        points.sort_by_key(|p| p.observed_at);
        let mut history = Vec::new();
        for p in points {
            let cutoff = p.hour - 30 * 86400;
            history.retain(|previous: &Point| previous.hour >= cutoff);
            history.push(p.clone());
            let window = Window {
                kind: p.kind.clone(),
                used_percent: p.used_percent,
                remaining_percent: (100.0 - p.used_percent).max(0.0),
                window_minutes: p.window_minutes,
                reset_at: p.reset_at,
                observed_at: p.observed_at,
                status: if p.reset_at.is_some_and(|r| r > p.observed_at) {
                    "current"
                } else {
                    "unknown"
                }
                .into(),
            };
            let forecast = quota_forecast::calculate(&history, &[window], p.observed_at).pop();
            output.push(Record {
                account: a.account.clone(),
                point: p,
                forecast,
            });
        }
    }
    output
}

fn run(args: Args) -> Result<()> {
    let metadata = fs::symlink_metadata(&args.input)?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
        return Err("invalid input".into());
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("input must be private".into());
    }
    let data: Dataset = serde_json::from_slice(&fs::read(args.input)?)?;
    validate(&data)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = std::io::BufWriter::new(options.open(args.output)?);
    let mut count = 0;
    for a in &data.accounts {
        for record in records(a, data.collected_at) {
            serde_json::to_writer(&mut file, &record)?;
            file.write_all(b"\n")?;
            count += 1;
        }
    }
    file.flush()?;
    println!("Replayed {count} recorded observations; no service contacted.");
    Ok(())
}

fn main() {
    if run(Args::parse()).is_err() {
        eprintln!("Forecast replay failed. Check private numeric input and output paths.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account() -> Account {
        Account {
            account: "account-001".into(),
            interval_seconds: 600,
            points: (0..12)
                .map(|n| Point {
                    kind: "primary".into(),
                    hour: 1800000000 + n * 600,
                    observed_at: 1800000000 + n * 600,
                    used_percent: 10.0 + n as f64,
                    window_minutes: Some(480),
                    reset_at: Some(1800028800),
                })
                .collect(),
        }
    }
    #[test]
    fn adding_future_observations_never_changes_past_predictions() {
        let a = account();
        let mut prefix = account();
        prefix.points.truncate(6);
        let before = records(&prefix, 1800100000);
        let after = records(&a, 1800100000);
        for (left, right) in before.iter().zip(&after) {
            assert_eq!(
                serde_json::to_value(left).unwrap(),
                serde_json::to_value(right).unwrap()
            );
        }
    }
    #[test]
    fn mutable_current_buckets_and_private_identity_fields_are_not_accepted() {
        let a = account();
        assert_eq!(records(&a, a.points[5].hour + 599).len(), 5);
        let mut data = Dataset {
            schema_version: 1,
            collected_at: 1800100000,
            accounts: vec![a],
        };
        assert!(validate(&data).is_ok());
        data.accounts[0].account = "owner@example.com".into();
        assert!(validate(&data).is_err());
    }
}
