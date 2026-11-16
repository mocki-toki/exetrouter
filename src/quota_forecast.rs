//! Read-only quota forecasts. Percentages are not token counts or billing dates.
use crate::{quota::Window, quota_history::Point, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const INTERVAL: i64 = crate::quota_history::INTERVAL;
const DAYS: i64 = 30;
const DAY: i64 = 86400;
const MAX_POINTS: usize = (DAYS * DAY / INTERVAL * 2 + 2) as usize;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Projection {
    pub percent_per_hour: f64,
    /// None means the estimated depletion would not precede the reported reset.
    pub limit_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Warning {
    /// Unknown when ordinary consumption is zero, never infinity.
    pub rate_ratio: Option<f64>,
    pub limit_before_reset: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentStatus {
    RecentPace,
    InsufficientHistory,
    QuotaUnchanged,
    ResetChanged,
    ResetTimeMoving,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Forecast {
    pub kind: String,
    pub window_minutes: i64,
    pub reset_at: i64,
    pub observed_at: i64,
    pub used_percent: f64,
    pub current: Option<Projection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_status: Option<CurrentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_valid_until: Option<i64>,
    pub usual: Option<Projection>,
    pub warning: Option<Warning>,
    pub recent_seconds: i64,
    pub historical_intervals: usize,
    /// Legacy wire name: distinct reported reset deadlines, not verified cycles.
    pub historical_cycles: usize,
}

pub(crate) fn idle_timeout(minutes: i64) -> i64 {
    if minutes >= 10080 {
        1800
    } else {
        300
    }
}

/// Only an upper bound on the last increase is recoverable from bucket-last
/// samples. Using the bucket END prevents unchanged refreshes within that same
/// bucket from continuously renewing the estimate. No sub-bucket time is invented.
fn activity_deadline(points: &[&Point], minutes: i64, reset: i64) -> Option<i64> {
    points
        .windows(2)
        .rev()
        .find(|pair| pair[1].used_percent > pair[0].used_percent)
        .map(|pair| {
            pair[1]
                .hour
                .saturating_add(INTERVAL)
                .saturating_add(idle_timeout(minutes))
                .min(reset)
        })
}

fn unavailable_status(
    points: &[&Point],
    all: &[Point],
    minutes: i64,
    now: i64,
    deadline: Option<i64>,
) -> CurrentStatus {
    let first = points.first().copied();
    let last = points.last().copied();
    let plateau = first.zip(last).is_some_and(|(a, b)| {
        a.used_percent == b.used_percent && b.observed_at - a.observed_at >= idle_timeout(minutes)
    });
    if deadline.is_some_and(|until| now >= until) || plateau {
        return CurrentStatus::QuotaUnchanged;
    }
    if let Some((previous, first)) = first.and_then(|first| {
        all.iter()
            .rev()
            .find(|p| p.hour < first.hour)
            .map(|p| (p, first))
    }) {
        if valid(previous, now)
            && previous.window_minutes == Some(minutes)
            && first.hour - previous.hour == INTERVAL
            && previous.reset_at != first.reset_at
        {
            // This describes an observed moving unused deadline, not a claim
            // that the upstream window is unstarted or that two cycles are equal.
            let moving = previous.used_percent == 0.0
                && first.used_percent == 0.0
                && minutes.checked_mul(60).is_some_and(|duration| {
                    let a = previous.reset_at.unwrap_or_default() - previous.observed_at;
                    let b = first.reset_at.unwrap_or_default() - first.observed_at;
                    (duration.saturating_sub(60)..=duration).contains(&a)
                        && (duration.saturating_sub(60)..=duration).contains(&b)
                        && a.abs_diff(b) <= 1
                });
            return if moving {
                CurrentStatus::ResetTimeMoving
            } else {
                CurrentStatus::ResetChanged
            };
        }
    }
    CurrentStatus::InsufficientHistory
}

#[derive(Clone, Copy)]
struct Rate {
    per_hour: f64,
    seconds: i64,
    reset: i64,
    at: i64,
}

fn valid(p: &Point, now: i64) -> bool {
    matches!(p.kind.as_str(), "primary" | "secondary")
        && p.hour >= 0
        && p.hour % INTERVAL == 0
        && p.observed_at >= p.hour
        && p.observed_at < p.hour.saturating_add(INTERVAL)
        && p.observed_at <= now
        && p.used_percent.is_finite()
        && (0.0..100.0).contains(&p.used_percent)
        && p.window_minutes.is_some_and(|m| m > 0)
        && p.reset_at.is_some_and(|r| r > p.observed_at)
}

fn connected(a: &Point, b: &Point) -> bool {
    b.hour == a.hour + INTERVAL
        && b.observed_at > a.observed_at
        && b.observed_at - a.observed_at < 2 * INTERVAL
        && a.window_minutes == b.window_minutes
        && a.reset_at == b.reset_at
        && b.used_percent >= a.used_percent
}

fn rate(points: &[&Point], horizon: i64) -> Option<Rate> {
    let last = *points.last()?;
    let mut first = last;
    let mut count = 1;
    for previous in points.iter().rev().skip(1) {
        if !connected(previous, first) {
            break;
        }
        first = previous;
        count += 1;
        if last.observed_at - first.observed_at >= horizon {
            break;
        }
    }
    let seconds = last.observed_at - first.observed_at;
    (count >= 3 && seconds >= horizon * 2 / 3).then(|| Rate {
        per_hour: (last.used_percent - first.used_percent) * 3600.0 / seconds as f64,
        seconds,
        reset: last.reset_at.unwrap_or_default(),
        at: last.observed_at,
    })
}

fn quantile(sorted: &[f64], fraction: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * fraction).ceil() as usize]
}

fn projection(used: f64, per_hour: f64, reset: i64, at: i64) -> Projection {
    let seconds = (100.0 - used) / per_hour * 3600.0;
    let limit_at = (per_hour > 0.0 && seconds.is_finite() && seconds < (reset - at) as f64)
        .then(|| at + seconds.ceil().max(1.0) as i64)
        .filter(|at| *at < reset);
    Projection {
        percent_per_hour: per_hour,
        limit_at,
    }
}

/// Non-overlapping, fully observed blocks. Gaps, resets, exhausted plateaus and
/// unknown windows break the block; they never become zero-consumption time.
fn historical_rates(points: &[&Point], horizon: i64, before: i64) -> Vec<Rate> {
    let mut rates = Vec::new();
    let mut start = 0;
    for (i, p) in points.iter().enumerate() {
        if p.observed_at > before {
            break;
        }
        if i > 0 && !connected(points[i - 1], p) {
            start = i;
        }
        let first = points[start];
        let seconds = p.observed_at - first.observed_at;
        if seconds >= horizon {
            rates.push(Rate {
                per_hour: (p.used_percent - first.used_percent) * 3600.0 / seconds as f64,
                seconds,
                reset: p.reset_at.unwrap_or_default(),
                at: p.observed_at,
            });
            start = i;
        }
    }
    rates
}

/// Replay closed buckets, not refreshes of the mutable current bucket. Three
/// high-rate observations with new consumption open the warning; two low-rate
/// observations close it. Replaying makes this survive restart without writes.
fn accelerated(
    points: &[&Point],
    horizon: i64,
    median: f64,
    upper: f64,
    lower: f64,
    now: i64,
) -> bool {
    let closed = now.div_euclid(INTERVAL) * INTERVAL;
    let mut high = 0;
    let mut low = 0;
    let mut active = false;
    for (i, p) in points.iter().enumerate() {
        if p.hour >= closed {
            break;
        }
        if i == 0 || !connected(points[i - 1], p) {
            high = 0;
            low = 0;
            active = false;
            continue;
        }
        let Some(recent) = rate(&points[..=i], horizon) else {
            continue;
        };
        let growing = p.used_percent - points[i - 1].used_percent >= 0.01;
        let elevated = recent.per_hour > (median * 1.5).max(upper * 1.1)
            && recent.per_hour * recent.seconds as f64 / 3600.0 >= 1.0;
        let interval_rate = (p.used_percent - points[i - 1].used_percent) * 3600.0
            / (p.observed_at - points[i - 1].observed_at) as f64;
        if elevated && growing && interval_rate > median * 1.5 {
            high += 1;
            low = 0;
            if high >= 3 {
                active = true;
            }
        } else {
            high = 0;
            if !growing || recent.per_hour <= (median * 1.2).max(lower) {
                low += 1;
                if low >= 2 {
                    active = false;
                }
            } else {
                low = 0;
            }
        }
    }
    active
}

/// Pure, bounded forecast calculation, also used by the offline history replay.
/// Callers must supply only observations available at `now`; unknowns stay absent.
pub fn calculate(points: &[Point], windows: &[Window], now: i64) -> Vec<Forecast> {
    if points.len() > MAX_POINTS {
        return Vec::new();
    }
    windows
        .iter()
        .take(2)
        .filter_map(|w| {
            let minutes = w.window_minutes.filter(|m| *m > 0)?;
            let reset = w.reset_at.filter(|r| *r > now)?;
            if !matches!(w.kind.as_str(), "primary" | "secondary")
                || w.status != "current"
                || !(0..60).contains(&now.saturating_sub(w.observed_at))
                || w.observed_at < 0
                || !w.used_percent.is_finite()
                || !(0.0..100.0).contains(&w.used_percent)
            {
                return None;
            }
            let horizon = if minutes >= 10080 { 7200 } else { 1800 };
            // Invalid entries remain barriers instead of being filtered into fake continuity.
            let mut selected: Vec<_> = points
                .iter()
                .filter(|p| {
                    p.kind == w.kind
                        && p.hour >= now.div_euclid(INTERVAL) * INTERVAL - DAYS * DAY
                        && p.hour <= w.observed_at.div_euclid(INTERVAL) * INTERVAL
                })
                .cloned()
                .collect();
            selected.sort_by_key(|p| (p.hour, p.observed_at));
            let live = Point {
                kind: w.kind.clone(),
                hour: w.observed_at.div_euclid(INTERVAL) * INTERVAL,
                observed_at: w.observed_at,
                used_percent: w.used_percent,
                window_minutes: Some(minutes),
                reset_at: Some(reset),
            };
            // The live sample replaces, rather than duplicates, its ten-minute bucket.
            selected.retain(|p| p.hour != live.hour);
            selected.push(live);
            let mut runs = Vec::<Vec<&Point>>::new();
            for p in &selected {
                if !valid(p, now) || p.window_minutes != Some(minutes) {
                    runs.push(Vec::new());
                    continue;
                }
                if runs
                    .last()
                    .and_then(|r| r.last())
                    .is_none_or(|previous| !connected(previous, p))
                {
                    runs.push(Vec::new());
                }
                runs.last_mut()?.push(p);
            }
            let current_points = runs.last()?;
            // Exclude the most recent day so acceleration cannot immediately redefine normal.
            let rates: Vec<_> = runs
                .iter()
                .flat_map(|r| historical_rates(r, horizon, now - DAY))
                .collect();
            let cycles: BTreeSet<_> = rates.iter().map(|r| r.reset).collect();
            let days: BTreeSet<_> = rates.iter().map(|r| r.at.div_euclid(DAY)).collect();
            let supported = rates.len() >= 12 && cycles.len() >= 2 && days.len() >= 3;
            let mut values: Vec<_> = rates.iter().map(|r| r.per_hour).collect();
            values.sort_by(f64::total_cmp);
            // Average observed time per reported deadline, then use the median group.
            // A median of short intervals would call bursty-but-regular work idle.
            let mut cycle_rates = BTreeMap::<i64, (f64, i64)>::new();
            for r in &rates {
                let entry = cycle_rates.entry(r.reset).or_default();
                entry.0 += r.per_hour * r.seconds as f64;
                entry.1 += r.seconds;
            }
            let mut ordinary: Vec<_> = cycle_rates
                .values()
                .map(|(total, seconds)| total / *seconds as f64)
                .collect();
            ordinary.sort_by(f64::total_cmp);
            let usual_rate = supported.then(|| quantile(&ordinary, 0.5));
            let usual = usual_rate.map(|r| projection(w.used_percent, r, reset, w.observed_at));
            // A weekly smoother must not conceal sustained acceleration. Detect
            // it on comparable 30-minute historical blocks, then use that fresh
            // horizon until the closed-bucket warning clears.
            let mut fast_rates: Vec<_> = if horizon > 1800 && supported {
                runs.iter()
                    .flat_map(|r| historical_rates(r, 1800, now - DAY))
                    .map(|r| r.per_hour)
                    .collect()
            } else {
                values
            };
            fast_rates.sort_by(f64::total_cmp);
            let elevated = usual_rate.is_some_and(|median| {
                accelerated(
                    current_points,
                    1800,
                    median,
                    quantile(&fast_rates, 0.9),
                    quantile(&fast_rates, 0.75),
                    now,
                )
            });
            let recent = rate(current_points, if elevated { 1800 } else { horizon });
            let current_valid_until = activity_deadline(current_points, minutes, reset);
            let current = recent
                .filter(|r| {
                    r.per_hour > 0.0 && current_valid_until.is_some_and(|until| now < until)
                })
                .map(|r| projection(w.used_percent, r.per_hour, reset, w.observed_at));
            let current_status = Some(if current.is_some() {
                CurrentStatus::RecentPace
            } else {
                unavailable_status(current_points, &selected, minutes, now, current_valid_until)
            });
            let warning = usual_rate
                .filter(|_| current.is_some() && elevated)
                .map(|median| Warning {
                    rate_ratio: (median > 0.0)
                        .then(|| current.as_ref().unwrap().percent_per_hour / median)
                        .filter(|r| r.is_finite()),
                    limit_before_reset: current.as_ref().is_some_and(|p| p.limit_at.is_some()),
                });
            Some(Forecast {
                kind: w.kind.clone(),
                window_minutes: minutes,
                reset_at: reset,
                observed_at: w.observed_at,
                used_percent: w.used_percent,
                current,
                current_status,
                current_valid_until,
                usual,
                warning,
                recent_seconds: recent.map_or(0, |r| r.seconds),
                historical_intervals: rates.len(),
                historical_cycles: cycles.len(),
            })
        })
        .collect()
}

pub(crate) fn snapshot(
    conn: &Connection,
    account: i64,
    windows: &[Window],
    now: i64,
) -> Result<Vec<Forecast>> {
    let from = (now.div_euclid(INTERVAL) * INTERVAL - DAYS * DAY).max(0);
    let mut stmt = conn.prepare("SELECT kind,hour,observed_at,used_percent,window_minutes,reset_at FROM oauth_quota_history WHERE account_id=?1 AND kind IN ('primary','secondary') AND hour>=?2 AND observed_at<=?3 ORDER BY hour,kind LIMIT ?4")?;
    let points = stmt
        .query_map(params![account, from, now, MAX_POINTS as i64], |r| {
            Ok(Point {
                kind: r.get(0)?,
                hour: r.get(1)?,
                observed_at: r.get(2)?,
                used_percent: r.get(3)?,
                window_minutes: r.get(4)?,
                reset_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(calculate(&points, windows, now))
}

impl Forecast {
    pub fn matches(&self, window: &Window, now: i64) -> bool {
        window.kind == self.kind
            && window.window_minutes == Some(self.window_minutes)
            && window.reset_at == Some(self.reset_at)
            && window.observed_at == self.observed_at
            && window.used_percent == self.used_percent
            && window.status == "current"
            && (0..60).contains(&now.saturating_sub(self.observed_at))
            && self.reset_at > now
    }
    pub fn current_label(&self, now: i64) -> Option<String> {
        if self.blocked_current_label(now).is_some() {
            return None;
        }
        self.current.as_ref().map(|p| match p.limit_at {
            Some(at) => format!("At this pace: {} to limit", estimate_duration(at, now)),
            None => "At this pace: reset first".into(),
        })
    }
    pub fn blocked_current_label(&self, now: i64) -> Option<&'static str> {
        if self.current_status == Some(CurrentStatus::QuotaUnchanged)
            || self.current_valid_until.is_some_and(|until| now >= until)
        {
            Some("Pace unavailable: quota unchanged")
        } else if self
            .current
            .as_ref()
            .and_then(|p| p.limit_at)
            .is_some_and(|at| now >= at)
        {
            Some("Estimate needs fresh quota")
        } else {
            None
        }
    }
    pub fn status_label(&self, now: i64) -> Option<&'static str> {
        self.blocked_current_label(now)
            .or(match self.current_status {
                Some(CurrentStatus::InsufficientHistory) => Some("Building recent pace"),
                Some(CurrentStatus::ResetChanged) => Some("Reset changed; rebuilding pace"),
                Some(CurrentStatus::ResetTimeMoving) => Some("Reset time moving; no pace yet"),
                _ => None,
            })
    }
    pub fn usual_label(&self, now: i64) -> Option<String> {
        self.usual.as_ref().map(|p| match p.limit_at {
            Some(at) if at <= now => "Historical estimate needs fresh quota".into(),
            Some(at) => format!("Historical pace: {} to limit", estimate_duration(at, now)),
            None => "Historical pace: reset first".into(),
        })
    }
    pub fn warning_label(&self, now: i64) -> Option<String> {
        if self.blocked_current_label(now).is_some() {
            return None;
        }
        self.warning.as_ref().map(|w| {
            let ratio = w
                .rate_ratio
                .map(|r| format!(" · ~{r:.1}x normal"))
                .unwrap_or_default();
            format!(
                "Usage above usual{ratio}{}",
                if w.limit_before_reset {
                    " · may run out before reset"
                } else {
                    ""
                }
            )
        })
    }
}

fn estimate_duration(at: i64, now: i64) -> String {
    let minutes = ((at.saturating_sub(now).max(0) + 59) / 60).max(1);
    if minutes < 60 {
        format!("~{}m", (minutes + 4) / 5 * 5)
    } else if minutes < 1440 {
        let rounded = (minutes + 14) / 15 * 15;
        format!("~{}h {}m", rounded / 60, rounded % 60)
    } else {
        let hours = (minutes + 59) / 60;
        format!("~{}d {}h", hours / 24, hours % 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: i64 = 1_800_000_000 / DAY * DAY;
    fn point(at: i64, used: f64, reset: i64, minutes: i64) -> Point {
        Point {
            kind: "primary".into(),
            hour: at / INTERVAL * INTERVAL,
            observed_at: at,
            used_percent: used,
            window_minutes: Some(minutes),
            reset_at: Some(reset),
        }
    }
    fn window(p: &Point) -> Window {
        Window {
            kind: p.kind.clone(),
            used_percent: p.used_percent,
            remaining_percent: 100.0 - p.used_percent,
            window_minutes: p.window_minutes,
            reset_at: p.reset_at,
            observed_at: p.observed_at,
            status: "current".into(),
        }
    }
    fn history(minutes: i64) -> Vec<Point> {
        let mut points = Vec::new();
        for day in 1..=21 {
            let start = BASE - day * DAY;
            let reset = if minutes >= 10080 {
                BASE - ((day - 1) / 7) * 7 * DAY
            } else {
                start + minutes * 60
            };
            // Real observed idle intervals belong in normal behavior, not gaps.
            for n in 0..=36 {
                points.push(point(start + n * INTERVAL, n as f64 / 6.0, reset, minutes));
            }
        }
        points.sort_by_key(|p| p.hour);
        points
    }
    fn active(points: &mut Vec<Point>, minutes: i64, per_hour: f64, count: i64) -> Window {
        for n in 0..=count {
            points.push(point(
                BASE + n * INTERVAL,
                10.0 + n as f64 * per_hour / 6.0,
                BASE + minutes * 60,
                minutes,
            ));
        }
        window(points.last().unwrap())
    }
    #[test]
    fn durable_history_produces_separate_current_and_ordinary_estimates() {
        for minutes in [300, 480, 10080] {
            let mut points = history(minutes);
            let w = active(&mut points, minutes, 24.0, 12);
            let forecast = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
            assert_eq!(forecast.current.as_ref().unwrap().percent_per_hour, 24.0);
            assert!((forecast.usual.as_ref().unwrap().percent_per_hour - 1.0).abs() < 1e-9);
            assert!(forecast.historical_cycles >= 2);
            assert!(forecast.warning.is_some());
            assert!(forecast
                .current_label(w.observed_at)
                .unwrap()
                .contains("to limit"));
            assert!(forecast.usual_label(w.observed_at).is_some());
            assert!(forecast.matches(&w, w.observed_at));
            assert!(!forecast.matches(&w, w.observed_at + 60));
            assert_eq!(
                serde_json::to_value(&forecast).unwrap(),
                serde_json::to_value(
                    calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0)
                )
                .unwrap(),
                "restart needs no session state"
            );
        }
    }
    #[test]
    fn gaps_resets_decreases_unknowns_and_duration_changes_break_recent_trends() {
        for mode in 0..6 {
            let mut points = history(480);
            let mut w = active(&mut points, 480, 12.0, 6);
            let i = points.len() - 2;
            match mode {
                0 => {
                    points.remove(i);
                }
                1 => points[i].reset_at = Some(BASE + 12345),
                2 => points[i].used_percent = 99.0,
                3 => points[i].window_minutes = None,
                4 => points[i].window_minutes = Some(300),
                _ => {
                    w.reset_at = Some(BASE + 604800);
                }
            }
            let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
            assert!(f.current.is_none(), "mode {mode}");
            assert!(f.warning.is_none(), "mode {mode}");
        }
    }
    #[test]
    fn freshness_future_samples_and_account_scope_do_not_invent_evidence() {
        let mut points = history(480);
        let w = active(&mut points, 480, 12.0, 6);
        assert!(calculate(&points, std::slice::from_ref(&w), w.observed_at + 60).is_empty());
        assert!(calculate(&points, std::slice::from_ref(&w), w.observed_at - 1).is_empty());
        assert!(calculate(&[], std::slice::from_ref(&w), w.observed_at)[0]
            .usual
            .is_none());
        let expected =
            serde_json::to_value(calculate(&points, std::slice::from_ref(&w), w.observed_at))
                .unwrap();
        points.push(point(BASE + DAY, 90.0, BASE + 2 * DAY, 480));
        assert_eq!(
            expected,
            serde_json::to_value(calculate(&points, std::slice::from_ref(&w), w.observed_at))
                .unwrap()
        );
        let mut exhausted = w.clone();
        exhausted.used_percent = 100.0;
        assert!(calculate(&points, &[exhausted], w.observed_at).is_empty());
    }
    #[test]
    fn single_burst_and_repeated_current_bucket_refreshes_do_not_open_alert() {
        let mut points = history(480);
        let w = active(&mut points, 480, 1.0, 6);
        let i = points.len() - 4;
        for p in &mut points[i..] {
            p.used_percent += 10.0;
        }
        let w = Window {
            used_percent: points.last().unwrap().used_percent,
            ..w
        };
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!(f.warning.is_none());
        for seconds in 0..60 {
            let mut w = w.clone();
            w.observed_at += seconds;
            assert!(
                calculate(&points, std::slice::from_ref(&w), w.observed_at)[0]
                    .warning
                    .is_none()
            );
        }
    }
    #[test]
    fn alert_clears_after_observed_idle_intervals_without_smoothing_a_reset() {
        let mut points = history(480);
        let w = active(&mut points, 480, 12.0, 6);
        assert!(
            calculate(&points, std::slice::from_ref(&w), w.observed_at)[0]
                .warning
                .is_some()
        );
        for n in 1..=3 {
            points.push(point(
                w.observed_at + n * INTERVAL,
                w.used_percent,
                w.reset_at.unwrap(),
                480,
            ));
        }
        let idle = window(points.last().unwrap());
        assert!(
            calculate(&points, std::slice::from_ref(&idle), idle.observed_at)[0]
                .warning
                .is_none()
        );
        let reset = Window {
            reset_at: Some(BASE + DAY),
            used_percent: 0.0,
            ..idle
        };
        let f = calculate(&points, std::slice::from_ref(&reset), reset.observed_at).remove(0);
        assert!(f.current.is_none());
        assert!(f.warning.is_none());
    }
    #[test]
    fn observed_zero_is_normal_but_missing_history_is_unknown() {
        let mut points = history(480);
        for p in &mut points {
            p.used_percent = 10.0;
        }
        let w = active(&mut points, 480, 12.0, 6);
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert_eq!(f.usual.unwrap().percent_per_hour, 0.0);
        assert!(f.warning.unwrap().rate_ratio.is_none());
        points.retain(|p| p.observed_at >= BASE);
        assert!(
            calculate(&points, std::slice::from_ref(&w), w.observed_at)[0]
                .usual
                .is_none()
        );
    }
    #[test]
    fn walk_forward_replay_preserves_stable_pace_and_smooths_a_single_jump() {
        let mut points = history(480);
        let w = active(&mut points, 480, 12.0, 18);
        for i in points.len() - 12..points.len() {
            let p = &points[i];
            let f = calculate(&points[..=i], &[window(p)], p.observed_at).remove(0);
            let expected = p.observed_at + ((100.0 - p.used_percent) / 12.0 * 3600.0).ceil() as i64;
            assert_eq!(f.current.unwrap().limit_at, Some(expected));
        }
        let baseline = calculate(&points, std::slice::from_ref(&w), w.observed_at)[0]
            .current
            .as_ref()
            .unwrap()
            .percent_per_hour;
        let mut spike = w.clone();
        spike.observed_at += 30;
        spike.used_percent += 1.0;
        let f = calculate(&points, std::slice::from_ref(&spike), spike.observed_at).remove(0);
        let smoothed = f.current.unwrap().percent_per_hour;
        let instantaneous = 1.0 / 30.0 * 3600.0;
        assert!((smoothed - baseline).abs() < (instantaneous - baseline).abs() / 10.0);
    }

    #[test]
    fn ordinary_cycle_average_preserves_regular_bursts_and_observed_idle_time() {
        let mut points = history(480);
        for p in &mut points {
            let n = (p.observed_at % DAY) / INTERVAL;
            p.used_percent = if n < 18 { 0.0 } else { 6.0 };
        }
        let w = active(&mut points, 480, 12.0, 6);
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!((f.usual.unwrap().percent_per_hour - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sustained_weekly_acceleration_uses_fresh_pace_instead_of_the_slow_smoother() {
        let mut points = history(10080);
        let normal = active(&mut points, 10080, 1.0, 36);
        let f = calculate(&points, std::slice::from_ref(&normal), normal.observed_at).remove(0);
        assert!((f.current.unwrap().percent_per_hour - 1.0).abs() < 1e-9);
        assert!(f.warning.is_none());
        for n in 1..=6 {
            points.push(point(
                normal.observed_at + n * INTERVAL,
                normal.used_percent + n as f64 * 2.0,
                normal.reset_at.unwrap(),
                10080,
            ));
        }
        let w = window(points.last().unwrap());
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!(f.warning.is_some());
        assert_eq!(f.recent_seconds, 1800);
        assert!((f.current.unwrap().percent_per_hour - 12.0).abs() < 1e-9);
    }

    #[test]
    fn actual_observation_times_and_normal_variation_do_not_create_alerts() {
        let mut points = history(480);
        for n in 0..=18 {
            let at = BASE + n * INTERVAL + if n % 2 == 0 { 30 } else { 550 };
            points.push(point(
                at,
                10.0 + (at - BASE) as f64 / 3600.0,
                BASE + 28800,
                480,
            ));
        }
        let w = window(points.last().unwrap());
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!((f.current.unwrap().percent_per_hour - 1.0).abs() < 1e-9);
        assert!(f.warning.is_none());
    }

    #[test]
    fn insufficient_cycles_and_saturated_or_missing_intervals_are_not_normal_zero() {
        let mut points = history(480);
        for p in &mut points {
            p.reset_at = Some(BASE + DAY);
        }
        let w = active(&mut points, 480, 12.0, 6);
        assert!(
            calculate(&points, std::slice::from_ref(&w), w.observed_at)[0]
                .usual
                .is_none()
        );
        for p in &mut points {
            if p.observed_at < BASE {
                p.used_percent = 100.0;
            }
        }
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!(f.usual.is_none());
        assert_eq!(f.historical_intervals, 0);
    }

    #[test]
    fn sqlite_forecasts_are_read_only_account_scoped_bounded_and_restart_safe() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::init(&conn).unwrap();
        for account in ["synthetic-a", "synthetic-b"] {
            conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES(?1,'active',X'00',9999999999,0)", [account]).unwrap();
        }
        let mut points = history(480);
        let w = active(&mut points, 480, 12.0, 6);
        for p in &points {
            conn.execute("INSERT INTO oauth_quota_history(account_id,kind,hour,observed_at,used_percent,window_minutes,reset_at,request_order) VALUES(1,?1,?2,?3,?4,?5,?6,0)", params![p.kind,p.hour,p.observed_at,p.used_percent,p.window_minutes,p.reset_at]).unwrap();
        }
        let changes = conn.total_changes();
        let first = snapshot(&conn, 1, std::slice::from_ref(&w), w.observed_at).unwrap();
        assert!(first[0].usual.is_some());
        let again = snapshot(&conn, 1, std::slice::from_ref(&w), w.observed_at).unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(again).unwrap()
        );
        let other = snapshot(&conn, 2, std::slice::from_ref(&w), w.observed_at).unwrap();
        assert!(other[0].usual.is_none());
        assert!(other[0].current.is_none());
        assert_eq!(conn.total_changes(), changes);
        let too_many = vec![points[0].clone(); MAX_POINTS + 1];
        assert!(calculate(&too_many, std::slice::from_ref(&w), w.observed_at).is_empty());
    }

    #[test]
    fn unchanged_refreshes_do_not_renew_activity_and_coarse_idle_expires_current_pace() {
        for minutes in [300, 480, 10080] {
            let mut points = history(minutes);
            let active = active(&mut points, minutes, 12.0, 12);
            let expected = active.observed_at + INTERVAL + idle_timeout(minutes);
            for offset in [0, 30, 200, 599] {
                let refreshed = Window {
                    observed_at: active.observed_at + offset,
                    ..active.clone()
                };
                let f = calculate(
                    &points,
                    std::slice::from_ref(&refreshed),
                    refreshed.observed_at,
                )
                .remove(0);
                assert_eq!(f.current_valid_until, Some(expected));
            }
            for n in 1..=(expected - active.observed_at) / INTERVAL {
                points.push(point(
                    active.observed_at + n * INTERVAL,
                    active.used_percent,
                    active.reset_at.unwrap(),
                    minutes,
                ));
            }
            let before = Window {
                observed_at: expected - 1,
                ..active.clone()
            };
            let f = calculate(&points, std::slice::from_ref(&before), before.observed_at).remove(0);
            assert!(f.current.is_some());
            assert!(f
                .current_label(expected - 1)
                .unwrap()
                .starts_with("At this pace:"));
            assert!(f.current_label(expected).is_none());
            assert!(f.warning_label(expected).is_none());
            let after = Window {
                observed_at: expected,
                ..active
            };
            let f = calculate(&points, std::slice::from_ref(&after), expected).remove(0);
            assert!(f.current.is_none());
            assert_eq!(f.current_status, Some(CurrentStatus::QuotaUnchanged));
            assert_eq!(
                f.status_label(expected),
                Some("Pace unavailable: quota unchanged")
            );
            assert!(
                f.usual.is_some(),
                "historical pace is separate from recent activity"
            );
        }
    }

    #[test]
    fn even_small_reset_changes_break_continuity_and_floating_unused_resets_do_not_become_cycles() {
        let mut points = history(480);
        let w = active(&mut points, 480, 12.0, 6);
        let i = points.len() - 2;
        points[i].reset_at = Some(points[i].reset_at.unwrap() + 1);
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!(f.current.is_none());
        assert_eq!(f.current_status, Some(CurrentStatus::ResetChanged));
        assert_eq!(
            f.reset_at,
            w.reset_at.unwrap(),
            "display still matches the exact reported deadline"
        );
        let points: Vec<_> = (0..24)
            .map(|n| point(BASE + n * INTERVAL, 0.0, BASE + n * INTERVAL + 18000, 300))
            .collect();
        let w = window(points.last().unwrap());
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert_eq!(f.current_status, Some(CurrentStatus::ResetTimeMoving));
        assert!(f.current.is_none());
        assert!(f.usual.is_none());
        assert_eq!(f.historical_cycles, 0);
        assert_eq!(
            f.status_label(w.observed_at),
            Some("Reset time moving; no pace yet")
        );
    }

    #[test]
    fn constant_observations_without_a_known_increase_are_not_current_activity() {
        let points: Vec<_> = (0..12)
            .map(|n| point(BASE + n * INTERVAL, 20.0, BASE + 604800, 10080))
            .collect();
        let w = window(points.last().unwrap());
        let f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        assert!(f.current.is_none());
        assert_eq!(f.current_valid_until, None);
        assert_eq!(f.current_status, Some(CurrentStatus::QuotaUnchanged));
    }

    #[test]
    fn elapsed_conditional_deadlines_are_not_reissued_as_a_future_countdown() {
        let mut points = history(480);
        let w = active(&mut points, 480, 24.0, 12);
        let mut f = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        f.current.as_mut().unwrap().limit_at = Some(w.observed_at + 5);
        assert!(f.current_label(w.observed_at).is_some());
        assert!(f.current_label(w.observed_at + 5).is_none());
        assert_eq!(
            f.status_label(w.observed_at + 5),
            Some("Estimate needs fresh quota")
        );
        assert!(f.warning_label(w.observed_at + 5).is_none());
    }

    #[test]
    fn rereading_the_same_observation_does_not_push_the_depletion_deadline_forward() {
        let mut points = history(480);
        let w = active(&mut points, 480, 24.0, 12);
        let first = calculate(&points, std::slice::from_ref(&w), w.observed_at).remove(0);
        let again = calculate(&points, std::slice::from_ref(&w), w.observed_at + 30).remove(0);
        assert_eq!(
            first.current.as_ref().unwrap().limit_at,
            again.current.as_ref().unwrap().limit_at
        );
        assert_eq!(
            first.usual.as_ref().unwrap().limit_at,
            again.usual.as_ref().unwrap().limit_at
        );
        let nearly_empty = Window {
            used_percent: 99.99,
            remaining_percent: 0.01,
            ..w
        };
        let first = calculate(
            &points,
            std::slice::from_ref(&nearly_empty),
            nearly_empty.observed_at,
        )
        .remove(0);
        let deadline = first.current.as_ref().unwrap().limit_at.unwrap();
        assert!(deadline <= nearly_empty.observed_at + 10);
        let again = calculate(&points, std::slice::from_ref(&nearly_empty), deadline + 1).remove(0);
        assert_eq!(
            first.current.as_ref().unwrap().limit_at,
            again.current.as_ref().unwrap().limit_at
        );
        assert_eq!(
            again.status_label(deadline + 1),
            Some("Estimate needs fresh quota")
        );
    }
}
