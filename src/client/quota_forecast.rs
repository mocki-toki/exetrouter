//! Shared runtime forecasts with a session-local fallback for insufficient history.
use crate::quota_forecast::CurrentStatus;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_WINDOWS: usize = 128;
const MAX_SAMPLES: usize = 256;
const MIN_SPAN: i64 = 60;
const FRESH_SECONDS: i64 = 60;

pub(super) fn historical(
    account: &Value,
    window: &Value,
    now: i64,
) -> Option<crate::quota_forecast::Forecast> {
    if account["refresh_error"].is_string() {
        return None;
    }
    let window: crate::quota::Window = serde_json::from_value(window.clone()).ok()?;
    account["quota_forecast"]
        .as_array()?
        .iter()
        .take(2)
        .find_map(|value| {
            let mut forecast: crate::quota_forecast::Forecast =
                serde_json::from_value(value.clone()).ok()?;
            if let Some(calendar) = &forecast.calendar {
                let windows: Vec<crate::quota::Window> =
                    serde_json::from_value(account["quota"]["windows"].clone()).ok()?;
                if !calendar.matches(&windows, now) {
                    forecast.calendar = None;
                }
            }
            forecast.matches(&window, now).then_some(forecast)
        })
}

#[derive(Clone, Copy, PartialEq)]
struct Sample {
    at: i64,
    used: f64,
}
struct Trend {
    minutes: i64,
    reset: i64,
    samples: VecDeque<Sample>,
    last_change: Option<i64>,
}
type Key = (i64, String, String);

fn key(account: &Value, window: &Value) -> Option<Key> {
    Some((
        account["id"].as_i64()?,
        account["observation_key"]
            .as_str()
            .or_else(|| account["label"].as_str())
            .unwrap_or_default()
            .to_owned(),
        window["kind"]
            .as_str()
            .filter(|kind| matches!(*kind, "primary" | "secondary"))?
            .to_owned(),
    ))
}

struct Estimate {
    window: (Sample, i64, i64),
    deadline: f64,
    label: String,
}

#[derive(Default)]
pub(super) struct Forecasts {
    windows: BTreeMap<Key, Trend>,
    estimates: BTreeMap<Key, Estimate>,
}
fn idle_timeout(minutes: i64) -> i64 {
    crate::quota_forecast::idle_timeout(minutes)
}
fn sample(window: &Value, now: i64) -> Option<(Sample, i64, i64)> {
    let (point, minutes, reset) = current_window(window)?;
    ((0..FRESH_SECONDS).contains(&now.saturating_sub(point.at)) && reset > now)
        .then_some((point, minutes, reset))
}

fn current_window(window: &Value) -> Option<(Sample, i64, i64)> {
    let at = window["observed_at"].as_i64()?;
    let used = window["used_percent"].as_f64()?;
    let minutes = window["window_minutes"].as_i64()?;
    let reset = window["reset_at"].as_i64()?;
    (window["status"] == "current" && at >= 0 && used.is_finite() && used >= 0.0 && minutes > 0)
        .then_some((Sample { at, used }, minutes, reset))
}
impl Forecasts {
    fn is_idle(trend: &Trend, (point, minutes, reset): (Sample, i64, i64), now: i64) -> bool {
        let Some(last) = trend.samples.back() else {
            return false;
        };
        if trend.minutes != minutes
            || trend.reset != reset
            || point.at != last.at
            || point.used != last.used
        {
            return false;
        }
        trend
            .last_change
            .is_some_and(|at| now - at >= idle_timeout(minutes))
            || trend.samples.front().is_some_and(|first| {
                first.used == last.used && last.at - first.at >= idle_timeout(minutes)
            })
    }
    pub(super) fn observe(&mut self, report: &Value, received_at: i64) {
        let now = report["checked_at"].as_i64().unwrap_or(received_at);
        let mut present = BTreeSet::new();
        self.estimates.clear();
        for account in report["quota_accounts"].as_array().into_iter().flatten() {
            if account["refresh_error"].is_string() {
                continue;
            }
            for window in account["quota"]["windows"].as_array().into_iter().flatten() {
                let Some(key) = key(account, window) else {
                    continue;
                };
                present.insert(key.clone());
                let Some((point, minutes, reset)) = sample(window, now) else {
                    self.windows.remove(&key);
                    continue;
                };
                let room = self.windows.len() < MAX_WINDOWS;
                let trend = if room || self.windows.contains_key(&key) {
                    Some(self.windows.entry(key.clone()).or_insert_with(|| Trend {
                        minutes,
                        reset,
                        samples: VecDeque::new(),
                        last_change: None,
                    }))
                } else {
                    None
                };
                let sample = (point, minutes, reset);
                let trend = trend.map(|trend| {
                    trend.observe(sample);
                    &*trend
                });
                if trend.is_some_and(|trend| Self::is_idle(trend, sample, now)) {
                    continue;
                }
                if self.estimates.len() < MAX_WINDOWS {
                    if let Some(deadline) = deadline(account, window, sample, trend, now) {
                        let total = ((deadline - now as f64) / 60.0).ceil().max(1.0) as u64;
                        let label = if total >= 60 {
                            format!("-{}h {}m", total / 60, total % 60)
                        } else {
                            format!("-{total}m")
                        };
                        self.estimates.insert(
                            key,
                            Estimate {
                                window: sample,
                                deadline,
                                label,
                            },
                        );
                    }
                }
            }
        }
        self.windows.retain(|key, _| present.contains(key));
    }
    pub(super) fn clear_estimates(&mut self) {
        self.estimates.clear();
    }

    pub(super) fn label(&self, account: &Value, window: &Value, now: i64) -> Option<&str> {
        let estimate = self.estimates.get(&key(account, window)?)?;
        (!account["refresh_error"].is_string()
            && (now as f64) < estimate.deadline
            && current_window(window) == Some(estimate.window))
        .then_some(&estimate.label)
    }
}

impl Trend {
    fn observe(&mut self, (point, minutes, reset): (Sample, i64, i64)) {
        if let Some(previous) = self.samples.back() {
            if point.at < previous.at {
                return;
            }
            if self.minutes != minutes
                || self.reset != reset
                || point.used < previous.used
                || point.at - previous.at > FRESH_SECONDS
                || (point.at == previous.at && point.used != previous.used)
            {
                self.samples.clear();
                self.last_change = None;
            } else if point.at == previous.at {
                return;
            } else if point.used > previous.used {
                self.last_change = Some(point.at);
            }
        }
        self.minutes = minutes;
        self.reset = reset;
        self.samples.push_back(point);
        let cutoff = point.at - idle_timeout(minutes);
        while self.samples.get(1).is_some_and(|point| point.at <= cutoff)
            || self.samples.len() > MAX_SAMPLES
        {
            self.samples.pop_front();
        }
    }

    fn deadline(&self, (point, minutes, reset): (Sample, i64, i64), now: i64) -> Option<f64> {
        if self.minutes != minutes
            || self.reset != reset
            || now - self.last_change? >= idle_timeout(minutes)
        {
            return None;
        }
        let first = self.samples.front()?;
        let last = self.samples.back()?;
        if point.at != last.at || point.used != last.used || last.at - first.at < MIN_SPAN {
            return None;
        }
        let delta = last.used - first.used;
        let remaining = (100.0 - point.used).max(0.0);
        if delta <= 0.0 || remaining <= 0.0 {
            return None;
        }
        let seconds = remaining / delta * (last.at - first.at) as f64;
        seconds.is_finite().then_some(point.at as f64 + seconds)
    }
}

fn deadline(
    account: &Value,
    window: &Value,
    observation: (Sample, i64, i64),
    trend: Option<&Trend>,
    now: i64,
) -> Option<f64> {
    let (point, minutes, reset) = observation;
    if point.used >= 100.0
        || minutes >= 10080
            && account["quota"]["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|w| {
                    w["kind"] != window["kind"]
                        && w["kind"]
                            .as_str()
                            .is_some_and(|k| matches!(k, "primary" | "secondary"))
                        && sample(w, now).is_some_and(|(p, m, _)| m < 10080 && p.used >= 100.0)
                })
    {
        return None;
    }
    let runtime = historical(account, window, now);
    if runtime.as_ref().is_some_and(|f| {
        f.current_status == Some(CurrentStatus::QuotaUnchanged)
            || f.current_valid_until.is_some_and(|until| now >= until)
    }) {
        return None;
    }
    let current = runtime
        .as_ref()
        .and_then(|f| f.calendar_projection(now).or(f.current.as_ref()));
    let live = match current {
        Some(p) => p.limit_at? as f64,
        None => trend?.deadline(observation, now)?,
    };
    if live <= now as f64 || live >= reset as f64 {
        return None;
    }
    let historical = runtime
        .and_then(|f| f.usual?.limit_at)
        .filter(|&at| at > now && at < reset);
    Some(historical.map_or(live, |at| live + (at as f64 - live) / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const BASE: i64 = 1_800_000_000;
    fn report(at: i64, used: f64, minutes: i64) -> Value {
        json!({"quota_accounts":[{"id":1,"quota":{"windows":[{"kind":"primary","used_percent":used,"remaining_percent":100.0-used,"observed_at":at,"window_minutes":minutes,"reset_at":BASE+604800,"status":"current"}]}}]})
    }
    fn window(report: &Value) -> &Value {
        &report["quota_accounts"][0]["quota"]["windows"][0]
    }
    fn label<'a>(forecasts: &'a Forecasts, report: &Value, now: i64) -> Option<&'a str> {
        forecasts.label(&report["quota_accounts"][0], window(report), now)
    }
    #[test]
    fn estimates_need_elapsed_observations_and_expire_when_short_or_weekly_usage_stops() {
        for minutes in [300, 480, 10080] {
            let mut forecasts = Forecasts::default();
            let initial = report(BASE, 10.0, minutes);
            forecasts.observe(&initial, BASE);
            assert!(label(&forecasts, &initial, BASE).is_none());
            let active = report(BASE + 60, 12.0, minutes);
            forecasts.observe(&active, BASE + 60);
            assert_eq!(label(&forecasts, &active, BASE + 60), Some("-44m"));
            let timeout = idle_timeout(minutes);
            for elapsed in (30..=timeout).step_by(30) {
                let unchanged = report(BASE + 60 + elapsed, 12.0, minutes);
                forecasts.observe(&unchanged, BASE + 60 + elapsed);
                assert_eq!(
                    label(&forecasts, &unchanged, BASE + 60 + elapsed).is_some(),
                    elapsed < timeout
                );
            }
        }
    }
    #[test]
    fn stale_data_resets_gaps_and_other_accounts_cannot_create_a_forecast() {
        let mut forecasts = Forecasts::default();
        forecasts.observe(&report(BASE, 10.0, 300), BASE);
        let active = report(BASE + 60, 12.0, 300);
        forecasts.observe(&active, BASE + 60);
        let mut other = active.clone();
        other["quota_accounts"][0]["observation_key"] = json!("different");
        assert!(label(&forecasts, &other, BASE + 60).is_none());
        assert_eq!(label(&forecasts, &active, BASE + 120), Some("-44m"));
        assert!(label(&forecasts, &active, BASE + 2700).is_none());
        let mut stale = active.clone();
        stale["quota_accounts"][0]["quota"]["windows"][0]["status"] = json!("stale");
        forecasts.observe(&stale, BASE + 60);
        assert!(forecasts.windows.is_empty());
        forecasts.observe(&report(BASE, 10.0, 300), BASE);
        forecasts.observe(&active, BASE + 60);
        let reset = report(BASE + 90, 0.0, 300);
        forecasts.observe(&reset, BASE + 90);
        assert!(label(&forecasts, &reset, BASE + 90).is_none());
        let gap = report(BASE + 600, 50.0, 300);
        forecasts.observe(&gap, BASE + 600);
        assert!(label(&forecasts, &gap, BASE + 600).is_none());
    }
    #[test]
    fn reset_changes_and_exhaustion_after_a_free_reset_suppress_predictions() {
        let mut forecasts = Forecasts::default();
        let mut initial = report(BASE, 10.0, 300);
        initial["quota_accounts"][0]["quota"]["windows"][0]["reset_at"] = json!(BASE + 300);
        forecasts.observe(&initial, BASE);
        let mut active = initial.clone();
        active["quota_accounts"][0]["quota"]["windows"][0]["observed_at"] = json!(BASE + 60);
        active["quota_accounts"][0]["quota"]["windows"][0]["used_percent"] = json!(12.0);
        forecasts.observe(&active, BASE + 60);
        assert!(label(&forecasts, &active, BASE + 60).is_none());
        active["quota_accounts"][0]["quota"]["windows"][0]["reset_at"] = json!(BASE + 604800);
        forecasts.observe(&active, BASE + 60);
        assert!(label(&forecasts, &active, BASE + 60).is_none());
    }
    #[test]
    fn duplicate_samples_do_not_invent_activity_and_history_stays_bounded() {
        let mut forecasts = Forecasts::default();
        let initial = report(BASE, 10.0, 10080);
        for _ in 0..100 {
            forecasts.observe(&initial, BASE);
        }
        assert_eq!(forecasts.windows.values().next().unwrap().samples.len(), 1);
        assert!(label(&forecasts, &initial, BASE).is_none());
        for seconds in 1..1000 {
            let value = report(BASE + seconds, 10.0 + seconds as f64 / 100.0, 10080);
            forecasts.observe(&value, BASE + seconds);
        }
        assert_eq!(
            forecasts.windows.values().next().unwrap().samples.len(),
            MAX_SAMPLES
        );
        let mut failed = report(BASE + 1000, 20.0, 10080);
        failed["quota_accounts"][0]["refresh_error"] = json!("Live usage unavailable");
        forecasts.observe(&failed, BASE + 1000);
        assert!(forecasts.windows.is_empty());
    }

    #[test]
    fn fine_idle_evidence_overrides_coarse_runtime_pace_without_resurrecting_fallbacks() {
        for minutes in [300, 10080] {
            let mut forecasts = Forecasts::default();
            forecasts.observe(&report(BASE, 10.0, minutes), BASE);
            forecasts.observe(&report(BASE + 60, 12.0, minutes), BASE + 60);
            let until = BASE + 60 + idle_timeout(minutes);
            let mut latest = report(BASE + 60, 12.0, minutes);
            for at in (BASE + 90..=until).step_by(30) {
                latest = report(at, 12.0, minutes);
                forecasts.observe(&latest, at);
            }
            let w = window(&latest).clone();
            let account = &mut latest["quota_accounts"][0];
            account["quota_forecast"] = serde_json::json!([{"kind":"primary","window_minutes":minutes,"observed_at":until,"used_percent":12,"reset_at":BASE+604800,"current":{"percent_per_hour":88,"limit_at":until+3600},"usual":null,"warning":null,"recent_seconds":1800,"historical_intervals":0,"historical_cycles":0,"current_status":"recent_pace","current_valid_until":until+1000}]);
            assert!(historical(account, &w, until)
                .unwrap()
                .current_label(until)
                .is_some());
            forecasts.observe(&latest, until);
            assert!(label(&forecasts, &latest, until).is_none());
        }
    }
    #[test]
    fn weekly_fallback_is_unadjusted_and_calendar_requires_the_exact_companion() {
        let mut forecasts = Forecasts::default();
        forecasts.observe(&report(BASE, 10.0, 10080), BASE);
        let mut latest = report(BASE + 60, 12.0, 10080);
        forecasts.observe(&latest, BASE + 60);
        assert_eq!(label(&forecasts, &latest, BASE + 60), Some("-44m"));
        let account = &mut latest["quota_accounts"][0];
        let short = json!({"kind":"secondary","used_percent":50,"remaining_percent":50,"window_minutes":300,"observed_at":BASE+60,"reset_at":BASE+18000,"status":"current"});
        account["quota"]["windows"]
            .as_array_mut()
            .unwrap()
            .push(short.clone());
        account["quota_forecast"] = json!([{"kind":"primary","window_minutes":10080,"observed_at":BASE+60,"used_percent":12,"reset_at":BASE+604800,"current":{"percent_per_hour":88,"limit_at":BASE+3660},"usual":null,"warning":null,"recent_seconds":7200,"historical_intervals":0,"historical_cycles":0,"calendar":{"short":short,"current":{"percent_per_hour":88,"limit_at":BASE+21660},"valid_until":BASE+100,"pauses":1}}]);
        forecasts.observe(&latest, BASE + 60);
        assert_eq!(label(&forecasts, &latest, BASE + 60), Some("-6h 0m"));
        latest["quota_accounts"][0]["quota"]["windows"][1]["reset_at"] = json!(BASE + 18001);
        forecasts.observe(&latest, BASE + 60);
        assert_eq!(label(&forecasts, &latest, BASE + 60), Some("-1h 0m"));
        latest["quota_accounts"][0]["quota"]["windows"][1]["used_percent"] = json!(100);
        forecasts.observe(&latest, BASE + 60);
        assert!(label(&forecasts, &latest, BASE + 60).is_none());
    }

    #[test]
    fn history_only_refines_live_and_a_failed_snapshot_clears_the_cached_eta() {
        let now = BASE + 60;
        let mut report = report(now, 12.0, 300);
        report["checked_at"] = json!(now);
        report["quota_accounts"][0]["quota_forecast"] = json!([{"kind":"primary","window_minutes":300,"observed_at":now,"used_percent":12,"reset_at":BASE+604800,"current":{"percent_per_hour":3,"limit_at":now+21*3600},"usual":{"percent_per_hour":2,"limit_at":now+22*3600},"warning":null,"recent_seconds":1800,"historical_intervals":36,"historical_cycles":9}]);
        let mut forecasts = Forecasts::default();
        forecasts.observe(&report, now + 100);
        assert_eq!(label(&forecasts, &report, now + 100), Some("-21h 30m"));
        forecasts.clear_estimates();
        assert!(label(&forecasts, &report, now + 100).is_none());
        report["quota_accounts"][0]["quota_forecast"][0]["current"]["limit_at"] = Value::Null;
        forecasts.observe(&report, now);
        assert!(label(&forecasts, &report, now).is_none());
    }
}
