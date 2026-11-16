//! Shared runtime forecasts with a session-local fallback for insufficient history.
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

pub(super) fn notes(account: &Value, now: i64) -> Vec<(String, bool)> {
    account["quota"]["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .take(2)
        .flat_map(|window| {
            let Some(f) = historical(account, window, now) else {
                return Vec::new();
            };
            let name = super::render::window_label(window);
            let mut lines = Vec::new();
            if let Some(label) = f.usual_label(now) {
                lines.push((format!("{name} · {label}"), false));
            }
            if let Some(label) = f.warning_label(now) {
                lines.push((format!("{name} · {label}"), true));
            }
            lines
        })
        .collect()
}

pub(super) fn label(
    account: &Value,
    window: &Value,
    now: i64,
    fallback: &Forecasts,
) -> Option<String> {
    if account["refresh_error"].is_string() {
        return None;
    }
    let id = account["id"].as_i64();
    if window["window_minutes"]
        .as_i64()
        .is_some_and(|m| m >= 10080)
        && sample(window, now).is_some_and(|(p, _, _)| p.used < 100.0)
        && account["quota"]["windows"]
            .as_array()
            .is_some_and(|windows| {
                windows.iter().any(|w| {
                    w["kind"] != window["kind"]
                        && w["kind"]
                            .as_str()
                            .is_some_and(|k| matches!(k, "primary" | "secondary"))
                        && sample(w, now).is_some_and(|(p, m, _)| m < 10080 && p.used >= 100.0)
                })
            })
    {
        return Some("Weekly pace paused: short window exhausted".into());
    }
    let runtime = historical(account, window, now);
    if let Some(text) = runtime.as_ref().and_then(|f| f.current_label(now)) {
        if text == "Weekly pace paused: short window exhausted" {
            return Some(text);
        }
    }
    if id.is_some_and(|id| fallback.is_idle(id, window, now)) {
        return Some("Pace unavailable: quota unchanged".into());
    }
    if let Some(text) = runtime.as_ref().and_then(|f| f.blocked_current_label(now)) {
        return Some(text.into());
    }
    runtime
        .as_ref()
        .and_then(|f| f.current_label(now))
        .or_else(|| {
            id.and_then(|id| fallback.label(id, window, now))
                .map(|label| {
                    if window["window_minutes"]
                        .as_i64()
                        .is_some_and(|m| m >= 10080)
                    {
                        format!("Without future pauses: {label}")
                    } else {
                        format!("At this pace: {label}")
                    }
                })
        })
        .or_else(|| {
            runtime
                .as_ref()
                .and_then(|f| f.status_label(now))
                .map(str::to_owned)
        })
}

#[derive(Clone, Copy)]
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
#[derive(Default)]
pub(super) struct Forecasts {
    windows: BTreeMap<(i64, String), Trend>,
}
fn idle_timeout(minutes: i64) -> i64 {
    crate::quota_forecast::idle_timeout(minutes)
}
fn sample(window: &Value, now: i64) -> Option<(Sample, i64, i64)> {
    let at = window["observed_at"].as_i64()?;
    let used = window["used_percent"].as_f64()?;
    let minutes = window["window_minutes"].as_i64()?;
    let reset = window["reset_at"].as_i64()?;
    (window["status"] == "current"
        && at >= 0
        && now
            .checked_sub(at)
            .is_some_and(|age| (0..FRESH_SECONDS).contains(&age))
        && used.is_finite()
        && used >= 0.0
        && minutes > 0
        && reset > now)
        .then_some((Sample { at, used }, minutes, reset))
}
impl Forecasts {
    pub(super) fn is_idle(&self, account: i64, window: &Value, now: i64) -> bool {
        let Some((point, minutes, reset)) = sample(window, now) else {
            return false;
        };
        if point.used >= 100.0 {
            return false;
        }
        let Some(trend) = window["kind"]
            .as_str()
            .and_then(|kind| self.windows.get(&(account, kind.to_owned())))
        else {
            return false;
        };
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
    pub(super) fn observe(&mut self, report: &Value, now: i64) {
        let mut present = BTreeSet::new();
        for account in report["quota_accounts"].as_array().into_iter().flatten() {
            if account["refresh_error"].is_string() {
                continue;
            }
            let Some(id) = account["id"].as_i64() else {
                continue;
            };
            for window in account["quota"]["windows"].as_array().into_iter().flatten() {
                let Some(kind) = window["kind"]
                    .as_str()
                    .filter(|kind| matches!(*kind, "primary" | "secondary"))
                else {
                    continue;
                };
                let key = (id, kind.to_owned());
                present.insert(key.clone());
                let Some((point, minutes, reset)) = sample(window, now) else {
                    self.windows.remove(&key);
                    continue;
                };
                if !self.windows.contains_key(&key) && self.windows.len() >= MAX_WINDOWS {
                    continue;
                }
                let trend = self.windows.entry(key).or_insert_with(|| Trend {
                    minutes,
                    reset,
                    samples: VecDeque::new(),
                    last_change: None,
                });
                if let Some(previous) = trend.samples.back() {
                    if point.at < previous.at {
                        continue;
                    }
                    if trend.minutes != minutes
                        || trend.reset != reset
                        || point.used < previous.used
                        || point.at - previous.at > FRESH_SECONDS
                    {
                        trend.samples.clear();
                        trend.last_change = None;
                    } else if point.at == previous.at {
                        // A changed value at the same timestamp has no measurable elapsed time.
                        if point.used == previous.used {
                            continue;
                        }
                        trend.samples.clear();
                        trend.last_change = None;
                    } else if point.used > previous.used {
                        trend.last_change = Some(point.at);
                    }
                }
                trend.minutes = minutes;
                trend.reset = reset;
                trend.samples.push_back(point);
                let cutoff = point.at - idle_timeout(minutes);
                while trend.samples.get(1).is_some_and(|point| point.at <= cutoff)
                    || trend.samples.len() > MAX_SAMPLES
                {
                    trend.samples.pop_front();
                }
            }
        }
        self.windows.retain(|key, _| present.contains(key));
    }
    pub(super) fn label(&self, account: i64, window: &Value, now: i64) -> Option<String> {
        let (point, minutes, reset) = sample(window, now)?;
        let trend = self
            .windows
            .get(&(account, window["kind"].as_str()?.to_owned()))?;
        if trend.minutes != minutes
            || trend.reset != reset
            || now - trend.last_change? >= idle_timeout(minutes)
        {
            return None;
        }
        let first = trend.samples.front()?;
        let last = trend.samples.back()?;
        if point.at != last.at || point.used != last.used || last.at - first.at < MIN_SPAN {
            return None;
        }
        let delta = last.used - first.used;
        let remaining = (100.0 - point.used).max(0.0);
        if delta <= 0.0 || remaining <= 0.0 {
            return None;
        }
        let seconds = remaining / delta * (last.at - first.at) as f64;
        if !seconds.is_finite() || seconds >= (reset - now) as f64 {
            return None;
        }
        let minutes = (seconds / 60.0).ceil().max(1.0) as u64;
        let duration = if minutes >= 1440 {
            format!("{}d {}h", minutes / 1440, minutes % 1440 / 60)
        } else if minutes >= 60 {
            format!("{}h {}m", minutes / 60, minutes % 60)
        } else {
            format!("{minutes}m")
        };
        Some(format!("~{duration} to limit"))
    }
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
    #[test]
    fn estimates_need_elapsed_observations_and_expire_when_short_or_weekly_usage_stops() {
        for minutes in [300, 480, 10080] {
            let mut forecasts = Forecasts::default();
            let initial = report(BASE, 10.0, minutes);
            forecasts.observe(&initial, BASE);
            assert!(forecasts.label(1, window(&initial), BASE).is_none());
            let active = report(BASE + 60, 12.0, minutes);
            forecasts.observe(&active, BASE + 60);
            assert_eq!(
                forecasts.label(1, window(&active), BASE + 60).as_deref(),
                Some("~44m to limit")
            );
            let timeout = idle_timeout(minutes);
            for elapsed in (30..=timeout).step_by(30) {
                let unchanged = report(BASE + 60 + elapsed, 12.0, minutes);
                forecasts.observe(&unchanged, BASE + 60 + elapsed);
                assert_eq!(
                    forecasts
                        .label(1, window(&unchanged), BASE + 60 + elapsed)
                        .is_some(),
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
        assert!(forecasts.label(2, window(&active), BASE + 60).is_none());
        assert!(forecasts.label(1, window(&active), BASE + 120).is_none());
        let mut stale = active.clone();
        stale["quota_accounts"][0]["quota"]["windows"][0]["status"] = json!("stale");
        forecasts.observe(&stale, BASE + 60);
        assert!(forecasts.windows.is_empty());
        forecasts.observe(&report(BASE, 10.0, 300), BASE);
        forecasts.observe(&active, BASE + 60);
        let reset = report(BASE + 90, 0.0, 300);
        forecasts.observe(&reset, BASE + 90);
        assert!(forecasts.label(1, window(&reset), BASE + 90).is_none());
        let gap = report(BASE + 600, 50.0, 300);
        forecasts.observe(&gap, BASE + 600);
        assert!(forecasts.label(1, window(&gap), BASE + 600).is_none());
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
        assert!(forecasts.label(1, window(&active), BASE + 60).is_none());
        active["quota_accounts"][0]["quota"]["windows"][0]["reset_at"] = json!(BASE + 604800);
        forecasts.observe(&active, BASE + 60);
        assert!(forecasts.label(1, window(&active), BASE + 60).is_none());
    }
    #[test]
    fn duplicate_samples_do_not_invent_activity_and_history_stays_bounded() {
        let mut forecasts = Forecasts::default();
        let initial = report(BASE, 10.0, 10080);
        for _ in 0..100 {
            forecasts.observe(&initial, BASE);
        }
        assert_eq!(forecasts.windows.values().next().unwrap().samples.len(), 1);
        assert!(forecasts.label(1, window(&initial), BASE).is_none());
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
            assert_eq!(
                label(account, &w, until, &forecasts).as_deref(),
                Some("Pace unavailable: quota unchanged")
            );
            assert!(!forecasts.is_idle(2, &w, until));
            let mut changed = w.clone();
            changed["reset_at"] = serde_json::json!(BASE + 604801);
            assert!(!forecasts.is_idle(1, &changed, until));
        }
    }
    #[test]
    fn weekly_fallback_is_unadjusted_and_calendar_requires_the_exact_companion() {
        let mut forecasts = Forecasts::default();
        forecasts.observe(&report(BASE, 10.0, 10080), BASE);
        let mut latest = report(BASE + 60, 12.0, 10080);
        forecasts.observe(&latest, BASE + 60);
        let w = window(&latest).clone();
        let account = &mut latest["quota_accounts"][0];
        assert_eq!(
            label(account, &w, BASE + 60, &forecasts).as_deref(),
            Some("Without future pauses: ~44m to limit")
        );
        let short = json!({"kind":"secondary","used_percent":50,"remaining_percent":50,"window_minutes":300,"observed_at":BASE+60,"reset_at":BASE+18000,"status":"current"});
        account["quota"]["windows"]
            .as_array_mut()
            .unwrap()
            .push(short.clone());
        account["quota_forecast"] = json!([{"kind":"primary","window_minutes":10080,"observed_at":BASE+60,"used_percent":12,"reset_at":BASE+604800,"current":{"percent_per_hour":88,"limit_at":BASE+3660},"usual":null,"warning":null,"recent_seconds":7200,"historical_intervals":0,"historical_cycles":0,"calendar":{"short":short,"current":{"percent_per_hour":88,"limit_at":BASE+21660},"valid_until":BASE+100,"pauses":1}}]);
        assert!(label(account, &w, BASE + 60, &forecasts)
            .unwrap()
            .contains("incl. short-window pauses"));
        account["quota"]["windows"][1]["reset_at"] = json!(BASE + 18001);
        assert_eq!(
            label(account, &w, BASE + 60, &forecasts).as_deref(),
            Some("Without future pauses: ~1h 0m to limit")
        );
        account["quota"]["windows"][1]["used_percent"] = json!(100);
        assert_eq!(
            label(account, &w, BASE + 60, &forecasts).as_deref(),
            Some("Weekly pace paused: short window exhausted")
        );
        let mut exhausted = w.clone();
        exhausted["used_percent"] = json!(100);
        assert!(label(account, &exhausted, BASE + 60, &forecasts).is_none());
    }
}
