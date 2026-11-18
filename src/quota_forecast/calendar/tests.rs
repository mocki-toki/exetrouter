use super::*;

const BASE: i64 = 1_800_000_000;
fn window(kind: &str, minutes: i64, used: f64, reset: i64) -> Window {
    Window {
        kind: kind.into(),
        window_minutes: Some(minutes),
        used_percent: used,
        remaining_percent: 100.0 - used,
        reset_at: Some(reset),
        observed_at: BASE,
        status: "current".into(),
    }
}
fn setup() -> (Vec<Point>, Vec<Window>) {
    let mut points = Vec::new();
    let mut windows = Vec::new();
    for (kind, minutes, rate, reset) in [
        ("primary", 300, 36.0, BASE + 18000),
        ("secondary", 10080, 12.0, BASE + 604800),
    ] {
        for n in 0..=12 {
            points.push(Point {
                kind: kind.into(),
                hour: BASE + n * 600,
                observed_at: BASE + n * 600,
                used_percent: 10.0 + n as f64 * rate / 6.0,
                window_minutes: Some(minutes),
                reset_at: Some(reset),
            });
        }
        let p = points.last().unwrap();
        windows.push(Window {
            observed_at: p.observed_at,
            ..window(kind, minutes, p.used_percent, reset)
        });
    }
    (points, windows)
}

#[test]
fn repeated_short_exhaustion_adds_pauses_without_changing_raw_or_historical_pace() {
    let (points, windows) = setup();
    let now = windows[1].observed_at;
    let f = super::super::calculate(&points, &windows, now)
        .pop()
        .unwrap();
    assert_eq!(f.current.as_ref().unwrap().limit_at, Some(now + 19800));
    let c = f.calendar.as_ref().unwrap();
    assert_eq!(c.pauses, 2);
    assert_eq!(c.current.as_ref().unwrap().limit_at, Some(now + 36800));
    assert!(f
        .current_label(now)
        .unwrap()
        .contains("incl. short-window pauses"));
    assert!(c.matches(&windows, now));
    assert_eq!(
        serde_json::to_value(&f).unwrap(),
        serde_json::to_value(
            super::super::calculate(&points, &windows, now + 20)
                .pop()
                .unwrap()
        )
        .unwrap()
    );
    let mut changed = windows.clone();
    changed[0].reset_at = Some(changed[0].reset_at.unwrap() + 1);
    assert!(!c.matches(&changed, now));
    assert!(!c.matches(&windows, now + 60));
}

#[test]
fn scheduling_handles_no_pause_renewal_first_and_exact_exhaustion_boundaries() {
    let short = window("primary", 300, 50.0, BASE + 7200);
    let mut week = window("secondary", 10080, 90.0, BASE + 604800);
    let (p, n) = schedule(&week, &short, 10.0, 25.0);
    assert_eq!((p.limit_at, n), (Some(BASE + 3600), 0));
    week.used_percent = 50.0;
    let (p, n) = schedule(&week, &short, 10.0, 25.0);
    // Exactly two hours available now, renewal at two hours, three hours after.
    assert_eq!((p.limit_at, n), (Some(BASE + 18000), 0));
    let short = window("primary", 300, 50.0, BASE + 18000);
    week.used_percent = 80.0;
    let (p, n) = schedule(&week, &short, 10.0, 25.0);
    assert_eq!((p.limit_at, n), (Some(BASE + 7200), 0));
    week.used_percent = 40.0;
    let (p, n) = schedule(&week, &short, 10.0, 25.0);
    assert_eq!((p.limit_at, n), (Some(BASE + 32400), 1));
    let short = window("primary", 300, 0.0, BASE + 3600);
    let (p, n) = schedule(&week, &short, 10.0, 10.0);
    assert_eq!((p.limit_at, n), (Some(BASE + 21600), 0));
}

#[test]
fn delayed_depletion_at_or_after_weekly_reset_is_not_a_before_reset_warning() {
    let short = window("primary", 300, 80.0, BASE + 18000);
    let week = window("secondary", 10080, 50.0, BASE + 18000);
    let (p, n) = schedule(&week, &short, 20.0, 100.0);
    assert_eq!(p.limit_at, None);
    assert!(n > 0);
    let week = Window {
        reset_at: Some(BASE + 3600),
        used_percent: 80.0,
        ..week
    };
    let short = window("primary", 300, 0.0, BASE + 18000);
    assert_eq!(schedule(&week, &short, 20.0, 10.0).0.limit_at, None);
    let (p, n) = schedule(&week, &short, 20.0, 50.0);
    assert!(p.limit_at.is_none());
    assert_eq!(n, 0, "a pause after the weekly reset is irrelevant");
}

#[test]
fn gaps_resets_exhausted_or_unaligned_intervals_never_invent_a_calendar_rate() {
    for mode in 0..7 {
        let (mut points, mut windows) = setup();
        let i = points
            .iter()
            .position(|p| p.kind == "primary" && p.hour == BASE + 6600)
            .unwrap();
        match mode {
            0 => {
                points.remove(i);
            }
            1 => points[i].reset_at = Some(BASE + 18001),
            2 => points[i].used_percent = 100.0,
            3 => points[i].observed_at += 1,
            4 => windows[0].observed_at -= 60,
            5 => windows[0].window_minutes = None,
            _ => windows[0].reset_at = Some(BASE + 7200),
        }
        let now = windows[1].observed_at;
        let f = super::super::calculate(&points, &windows, now)
            .pop()
            .unwrap();
        assert!(f.current.is_some(), "mode {mode}");
        assert!(
            f.calendar.as_ref().unwrap().current.is_none(),
            "mode {mode}"
        );
        assert!(
            f.current_label(now)
                .unwrap()
                .starts_with("Without future pauses:"),
            "mode {mode}"
        );
    }
}

#[test]
fn exhausted_short_window_pauses_weekly_and_weekly_only_needs_no_adjustment() {
    let (points, mut windows) = setup();
    let now = windows[1].observed_at;
    windows[0].used_percent = 100.0;
    windows[0].remaining_percent = 0.0;
    let mut f = super::super::calculate(&points, &windows, now)
        .pop()
        .unwrap();
    assert_eq!(
        f.current_label(now).as_deref(),
        Some("Weekly pace paused: short window exhausted")
    );
    f.warning = Some(Warning {
        rate_ratio: Some(2.0),
        limit_before_reset: true,
    });
    assert!(f.warning_label(now).is_none());
    let f = super::super::calculate(&points, &windows[1..], now)
        .pop()
        .unwrap();
    assert!(f.calendar.as_ref().unwrap().short.is_none());
    assert_eq!(
        f.current.as_ref().unwrap().limit_at,
        f.calendar
            .as_ref()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .limit_at
    );
    assert!(!f.current_label(now).unwrap().contains("pauses"));
}

#[test]
fn short_activity_expiry_downgrades_the_calendar_without_renewing_it_on_reread() {
    let (points, windows) = setup();
    let now = windows[1].observed_at;
    let mut f = super::super::calculate(&points, &windows, now)
        .pop()
        .unwrap();
    let until = f.calendar.as_ref().unwrap().valid_until.unwrap();
    assert_eq!(until, now + 900);
    // Keep the exact windows fresh to isolate expiry from report staleness.
    f.calendar
        .as_mut()
        .unwrap()
        .short
        .as_mut()
        .unwrap()
        .observed_at = until;
    assert!(f
        .current_label(until)
        .unwrap()
        .starts_with("Without future pauses:"));
    f.calendar
        .as_mut()
        .unwrap()
        .current
        .as_mut()
        .unwrap()
        .limit_at = None;
    f.calendar.as_mut().unwrap().valid_until = Some(now + 1);
    f.warning = Some(Warning {
        rate_ratio: Some(2.0),
        limit_before_reset: true,
    });
    f.calendar
        .as_mut()
        .unwrap()
        .short
        .as_mut()
        .unwrap()
        .observed_at = now;
    assert!(!f.warning_label(now).unwrap().contains("may run out"));
}

#[test]
fn closed_form_matches_a_bounded_event_simulation_of_repeated_windows() {
    for minutes in [60, 300, 480] {
        for short_used in [0.0, 50.0, 99.0] {
            for weekly_used in [0.0, 90.0] {
                for short_rate in [1.0, 20.0, 100.0] {
                    for weekly_rate in [1.0, 20.0, 100.0] {
                        for offset in [60, 1800, minutes * 60] {
                            let short = window("primary", minutes, short_used, BASE + offset);
                            let week = window("secondary", 10080, weekly_used, BASE + 604800);
                            let (p, _) = schedule(&week, &short, weekly_rate, short_rate);
                            let mut active = (100.0 - weekly_used) / weekly_rate * 3600.0;
                            let mut time = 0.0;
                            let mut reset = offset as f64;
                            let mut budget = (100.0 - short_used) / short_rate * 3600.0;
                            let expected = loop {
                                let available = budget.min(reset - time);
                                if active <= available {
                                    break Some(BASE + (time + active).ceil() as i64);
                                }
                                active -= available;
                                time = reset;
                                if time >= 604800.0 {
                                    break None;
                                }
                                reset += minutes as f64 * 60.0;
                                budget = 100.0 / short_rate * 3600.0;
                            }
                            .filter(|t| *t < BASE + 604800);
                            match (p.limit_at, expected) {
                                (Some(a), Some(b)) => assert!(a.abs_diff(b) <= 1),
                                (a, b) => assert_eq!(a, b),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn window_order_and_actual_short_duration_are_not_inferred_from_kind_names() {
    let (mut points, mut windows) = setup();
    for p in &mut points {
        p.kind = if p.kind == "primary" {
            "secondary"
        } else {
            "primary"
        }
        .into();
        if p.window_minutes == Some(300) {
            p.window_minutes = Some(480);
        }
    }
    for w in &mut windows {
        w.kind = if w.kind == "primary" {
            "secondary"
        } else {
            "primary"
        }
        .into();
        if w.window_minutes == Some(300) {
            w.window_minutes = Some(480);
        }
    }
    windows.reverse();
    let f = super::super::calculate(&points, &windows, windows[0].observed_at).remove(0);
    assert_eq!(f.window_minutes, 10080);
    let c = f.calendar.unwrap();
    assert_eq!(c.short.unwrap().window_minutes, Some(480));
    assert_eq!(c.pauses, 2);
    assert!(c.current.unwrap().limit_at.unwrap() > windows[0].observed_at + 19800);
}
