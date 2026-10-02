//! Bounded, session-local estimates from fresh quota observations, not token counts.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_WINDOWS: usize = 128;
const MAX_SAMPLES: usize = 256;
const MIN_SPAN: i64 = 60;
const FRESH_SECONDS: i64 = 60;

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
    if minutes >= 10080 {
        1800
    } else {
        300
    }
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
        json!({"quota_accounts":[{"id":1,"quota":{"windows":[{"kind":"primary","used_percent":used,"observed_at":at,"window_minutes":minutes,"reset_at":BASE+604800,"status":"current"}]}}]})
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
}
