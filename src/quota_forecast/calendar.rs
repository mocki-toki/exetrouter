//! Conditional wall-clock scenario, not a prediction of future routing or work.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calendar {
    /// Exact companion observation; None means this snapshot reports weekly only.
    pub short: Option<Window>,
    pub current: Option<Projection>,
    pub valid_until: Option<i64>,
    pub pauses: usize,
}

impl Calendar {
    pub fn matches(&self, windows: &[Window], now: i64) -> bool {
        match &self.short {
            Some(s) => windows.iter().any(|w| {
                w.kind == s.kind
                    && w.window_minutes == s.window_minutes
                    && w.reset_at == s.reset_at
                    && w.observed_at == s.observed_at
                    && w.used_percent == s.used_percent
                    && fresh(w, now)
            }),
            None => windows.len() == 1,
        }
    }
    pub(super) fn projection(&self, now: i64) -> Option<&Projection> {
        (self.valid_until.is_some_and(|until| now < until)
            && self.short.as_ref().is_none_or(|s| fresh(s, now)))
        .then_some(self.current.as_ref())
        .flatten()
    }
    pub(super) fn short_blocked(&self, now: i64) -> bool {
        self.short.as_ref().is_some_and(|s| {
            fresh(s, now)
                && s.window_minutes.is_some_and(|m| m > 0 && m < 10080)
                && s.used_percent >= 100.0
        })
    }
}

fn fresh(w: &Window, now: i64) -> bool {
    w.status == "current"
        && (0..60).contains(&now.saturating_sub(w.observed_at))
        && w.reset_at.is_some_and(|r| r > now)
        && w.used_percent.is_finite()
        && (0.0..=100.0).contains(&w.used_percent)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn calculate(
    points: &[Point],
    weekly: &[&Point],
    windows: &[Window],
    week: &Window,
    current: Option<&Projection>,
    valid_until: Option<i64>,
    horizon: i64,
    now: i64,
) -> Calendar {
    let mut result = Calendar {
        short: windows.iter().find(|w| w.kind != week.kind).cloned(),
        current: None,
        valid_until: None,
        pauses: 0,
    };
    if windows.len() == 1 {
        result.current = current.cloned();
        result.valid_until = current.and(valid_until);
        return result;
    }
    let Some(short) = result.short.as_ref() else {
        return result;
    };
    let Some(minutes) = short.window_minutes.filter(|m| *m > 0 && *m < 10080) else {
        return result;
    };
    if current.is_none()
        || !fresh(short, now)
        || short.used_percent >= 100.0
        || short.observed_at != week.observed_at
        || short.reset_at.unwrap() - short.observed_at > minutes * 60
    {
        return result;
    }
    let live = Point {
        kind: short.kind.clone(),
        hour: short.observed_at.div_euclid(INTERVAL) * INTERVAL,
        observed_at: short.observed_at,
        used_percent: short.used_percent,
        window_minutes: Some(minutes),
        reset_at: short.reset_at,
    };
    let mut by_bucket: BTreeMap<_, _> = points
        .iter()
        .filter(|p| p.kind == short.kind && p.hour < live.hour && p.observed_at <= now)
        .map(|p| (p.hour, p))
        .collect();
    by_bucket.insert(live.hour, &live);
    let mut paired_week = Vec::new();
    let mut paired_short: Vec<&Point> = Vec::new();
    for w in weekly.iter().rev() {
        let Some(s) = by_bucket.get(&w.hour).copied() else {
            break;
        };
        // Identical endpoints avoid mixing rates measured across different work
        // or counting a previously blocked interval and then adding it again.
        if !valid(s, now)
            || s.observed_at != w.observed_at
            || s.window_minutes != Some(minutes)
            || s.reset_at != short.reset_at
            || paired_short.last().is_some_and(|next| !connected(s, next))
        {
            break;
        }
        paired_week.push(*w);
        paired_short.push(s);
        if week.observed_at - w.observed_at >= horizon {
            break;
        }
    }
    paired_week.reverse();
    paired_short.reverse();
    let (Some(wr), Some(sr)) = (rate(&paired_week, horizon), rate(&paired_short, horizon)) else {
        return result;
    };
    let until = activity_deadline(&paired_short, minutes, short.reset_at.unwrap())
        .zip(valid_until)
        .map(|(s, w)| s.min(w));
    if wr.per_hour <= 0.0 || sr.per_hour <= 0.0 || !until.is_some_and(|t| now < t) {
        return result;
    }
    let (projection, pauses) = schedule(week, short, wr.per_hour, sr.per_hour);
    result.current = Some(projection);
    result.valid_until = until;
    result.pauses = pauses;
    result
}

/// Constant-time repeated-window scenario: immediate resumption at each reset,
/// unchanged paired consumption rates and unchanged full short-window capacity.
/// No future provider reset is persisted or used for routing.
fn schedule(
    week: &Window,
    short: &Window,
    weekly_rate: f64,
    short_rate: f64,
) -> (Projection, usize) {
    let at = week.observed_at;
    let until_reset = (short.reset_at.unwrap() - at) as f64;
    let mut active = (100.0 - week.used_percent) / weekly_rate * 3600.0;
    let first_budget = (100.0 - short.used_percent) / short_rate * 3600.0;
    let full_budget = 100.0 / short_rate * 3600.0;
    let duration = (short.window_minutes.unwrap() * 60) as f64;
    let seconds = if active <= first_budget.min(until_reset) {
        active
    } else {
        let first_active = first_budget.min(until_reset);
        active -= first_active;
        if full_budget >= duration {
            until_reset + active
        } else {
            // Finishing exactly at exhaustion does not require waiting for the
            // next reset. Do not loop across future windows.
            let complete = (active / full_budget).ceil().max(1.0) - 1.0;
            until_reset + complete * duration + (active - complete * full_budget)
        }
    };
    let reset = week.reset_at.unwrap();
    // Count only pauses actually reached before depletion or the weekly reset,
    // not hypothetical pauses after this weekly window has already renewed.
    let horizon = seconds.min((reset - at) as f64);
    let mut pauses = usize::from(first_budget < until_reset && first_budget < horizon);
    if full_budget < duration && until_reset + full_budget < horizon {
        pauses += ((horizon - until_reset - full_budget) / duration).ceil() as usize;
    }
    let limit_at = (seconds.is_finite() && seconds < (reset - at) as f64)
        .then(|| at + seconds.ceil().max(1.0) as i64)
        .filter(|t| *t < reset);
    (
        Projection {
            percent_per_hour: weekly_rate,
            limit_at,
        },
        pauses,
    )
}

#[cfg(test)]
mod tests;
