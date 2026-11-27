//! Scrollable account details with a fixed-cost, clipped chart header.
use super::{quota_forecast::Forecasts, render};
use crate::quota_history::{History, PlatformBalance};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Chart, Dataset, GraphType, Paragraph, Widget, Wrap,
    },
    Frame,
};
use serde_json::Value;

pub(super) const COLORS: [Color; 6] = [
    Color::Rgb(132, 190, 203),
    Color::Rgb(170, 163, 230),
    Color::Rgb(143, 201, 164),
    Color::Rgb(228, 189, 121),
    Color::Rgb(227, 147, 147),
    Color::Rgb(133, 168, 225),
];

fn history(account: &Value) -> Option<History> {
    let mut payload = account["quota_history"].clone();
    let optional = payload.as_object_mut()?.remove("platform_balance");
    // A bad optional series must not hide the Codex history.
    payload.as_object_mut()?.remove("platform");
    let mut h: History = serde_json::from_value(payload).ok()?;
    h.platform_balance = optional.and_then(|v| serde_json::from_value(v).ok());
    let mut seen = std::collections::BTreeSet::new();
    let interval = h.interval_seconds;
    let valid = matches!(interval, 600 | 3600)
        && h.from_utc >= 0
        && h.from_utc % interval == 0
        && h.to_utc % interval == 0
        && h.to_utc
            .checked_sub(h.from_utc)
            .is_some_and(|span| (1..=2160 * 3600).contains(&span))
        && h.points.len() <= (2160 * 3600 / interval * 2) as usize
        && h.points.iter().all(|p| {
            matches!(p.kind.as_str(), "primary" | "secondary")
                && p.hour >= h.from_utc
                && p.hour < h.to_utc
                && p.hour % interval == 0
                && p.observed_at >= p.hour
                && p.observed_at < p.hour + interval
                && p.used_percent.is_finite()
                && p.used_percent >= 0.0
                && seen.insert((p.kind.clone(), p.hour))
        });
    h.points.sort_by_key(|p| p.hour);
    valid.then_some(h)
}
pub(super) fn range(account: &Value) -> Option<(i64, i64, i64)> {
    history(account).map(|h| (h.from_utc, h.to_utc, h.interval_seconds))
}
pub(super) fn height(account: &Value, now: i64, width: u16) -> usize {
    (chart_header(account, width) + 20) as usize
        + Paragraph::new(render::account_details(account, now))
            .wrap(Wrap { trim: false })
            .line_count(width.max(1))
}

fn windows(account: &Value) -> Vec<&Value> {
    let mut windows: Vec<_> = account["quota"]["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["window_minutes"].as_i64().is_some_and(|m| m > 0))
        .take(2)
        .collect();
    windows.sort_by_key(|w| w["window_minutes"].as_i64());
    windows
}

fn card_columns(width: u16) -> usize {
    match width {
        102.. => 3,
        60.. => 2,
        _ => 1,
    }
}

fn chart_header(account: &Value, width: u16) -> u16 {
    let cards = windows(account).len() + usize::from(account["platform"].is_object());
    2 + 5 * cards.div_ceil(card_columns(width)) as u16
}

fn platform_history<'a>(account: &Value, history: &'a History) -> Option<&'a PlatformBalance> {
    let series = history.platform_balance.as_ref()?;
    let balance = &account["platform"]["balance"];
    let interval = history.interval_seconds;
    let first = series.points.first()?;
    (series.entered_at >= 0
        && balance["entered_at"].as_i64() == Some(series.entered_at)
        && balance["initial_usd_nanos"].as_i64() == Some(series.initial_usd_nanos)
        && balance["exetrouter"]["status"] != "configuration_changed"
        && series.initial_usd_nanos > 0
        && first.hour == (series.entered_at / interval * interval).max(history.from_utc)
        && series.through_bucket % interval == 0
        && series.through_bucket < history.to_utc
        && series.points.len() <= ((history.to_utc - history.from_utc) / interval) as usize
        && series.points.iter().all(|p| {
            p.hour >= first.hour
                && p.hour <= series.through_bucket
                && p.hour % interval == 0
                && (0.0..=100.0).contains(&p.remaining_percent)
        })
        && series
            .points
            .windows(2)
            .all(|p| p[0].hour < p[1].hour && p[0].remaining_percent >= p[1].remaining_percent))
    .then_some(series)
}

pub(super) fn platform_summary(platform: &Value) -> (String, Option<f64>) {
    let balance = &platform["balance"];
    let remaining = crate::platform::balance::display_remaining_nanos(balance);
    let initial = balance["initial_usd_nanos"].as_i64();
    let dollars = |n: i64| crate::money::usd(n as f64 / 1e9);
    let mut amount = remaining.map_or_else(|| "Unknown".into(), dollars);
    if let Some(initial) = initial {
        amount.push_str(&format!(" / {}", dollars(initial)));
    }
    if platform["refresh_error"].is_string() {
        amount.push_str(" · Platform refresh failed");
    }
    let percent = remaining
        .zip(initial)
        .filter(|(_, initial)| *initial > 0)
        .map(|(left, initial)| (left as f64 / initial as f64 * 100.0).clamp(0., 100.));
    (amount, percent)
}
fn put(buf: &mut Buffer, y: u16, value: impl Into<Line<'static>>, color: Color) {
    Paragraph::new(value.into())
        .style(Style::default().fg(color))
        .render(Rect::new(0, y, buf.area.width, 1), buf);
}

pub(super) fn meter(buf: &mut Buffer, area: Rect, remaining: Option<f64>) {
    if area.is_empty() {
        return;
    }
    let filled =
        (f64::from(area.width) * remaining.unwrap_or(0.).clamp(0., 100.) / 100.).round() as u16;
    let color = match remaining {
        Some(p) if p <= 20. => Color::Rgb(223, 104, 112),
        Some(p) if p <= 50. => Color::Rgb(232, 191, 82),
        Some(_) => Color::Rgb(93, 195, 143),
        None => Color::DarkGray,
    };
    for x in 0..area.width {
        buf[(area.x + x, area.y)]
            .set_char('━')
            .set_style(Style::default().fg(if x < filled { color } else { Color::DarkGray }));
    }
}

pub(super) fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    account: &Value,
    now: i64,
    scroll: u16,
    selected: Option<i64>,
    forecasts: &Forecasts,
) {
    if area.is_empty() {
        return;
    }
    let width = area.width;
    let header = chart_header(account, width);
    let top_height = header + 20;
    let mut buf = Buffer::empty(Rect::new(0, 0, width, top_height));
    put(
        &mut buf,
        0,
        Line::styled(
            render::safe(account["label"].as_str().unwrap_or("Account")),
            Style::default().fg(Color::White).bold(),
        ),
        Color::White,
    );
    let mut cards: Vec<_> = windows(account).into_iter().map(Some).collect();
    if account["platform"].is_object() {
        cards.push(None);
    }
    for (i, row) in cards.chunks(card_columns(width)).enumerate() {
        let y = 2 + 5 * i as u16;
        let col_width = (width - 2 * (row.len() as u16 - 1)) / row.len() as u16;
        for (col, window) in row.iter().enumerate() {
            let x = col as u16 * (col_width + 2);
            let wwidth = if col + 1 == row.len() {
                width - x
            } else {
                col_width
            };
            let line = |dy| Rect::new(x, y + dy, wwidth, 1);
            let (label, value) = if let Some(w) = window {
                let eta = forecasts
                    .label(account, w, now)
                    .map_or_else(String::new, |label| format!("  {label}"));
                let current = !account["refresh_error"].is_string()
                    && w["status"] == "current"
                    && w["observed_at"]
                        .as_i64()
                        .is_some_and(|at| crate::routing::fresh(at, now));
                meter(
                    &mut buf,
                    line(2),
                    current.then(|| w["remaining_percent"].as_f64()).flatten(),
                );
                Paragraph::new(format!("Resets {}", chart_date(w["reset_at"].as_i64())))
                    .style(Style::default().fg(Color::Gray))
                    .render(line(3), &mut buf);
                (
                    render::window_label(w),
                    format!("{} left{eta}", render::percent(&w["remaining_percent"])),
                )
            } else {
                let (value, percent) = platform_summary(&account["platform"]);
                meter(&mut buf, line(2), percent);
                Paragraph::new(value)
                    .style(Style::default().fg(Color::Gray))
                    .render(line(3), &mut buf);
                (
                    "Platform API".into(),
                    percent.map_or_else(
                        || "Unknown".into(),
                        |p| format!("{} left", render::percent_value(p)),
                    ),
                )
            };
            Paragraph::new(label)
                .style(Style::default().fg(Color::Gray))
                .render(line(0), &mut buf);
            Paragraph::new(value)
                .style(Style::default().fg(Color::White).bold())
                .render(line(1), &mut buf);
        }
    }
    put(
        &mut buf,
        header,
        Line::styled("Limit history", Style::default().bold()),
        Color::White,
    );
    let chart_area = Rect::new(0, header + 2, width, 12);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Left ");
    let history = history(account);
    let span = history.as_ref().map(|h| h.to_utc - h.from_utc);
    let mut periods = Vec::new();
    for (label, duration) in [
        ("24h", 86400),
        ("7d", 7 * 86400),
        ("30d", 30 * 86400),
        ("90d", 90 * 86400),
    ] {
        periods.push(Span::styled(
            format!(" {label} "),
            if span == Some(duration) {
                Style::default().fg(Color::Black).bg(Color::Gray)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
        periods.push(Span::raw(" "));
    }
    put(&mut buf, header + 1, Line::from(periods), Color::White);
    let platform = history.as_ref().and_then(|h| platform_history(account, h));
    if let Some(h) = history
        .as_ref()
        .filter(|h| !h.points.is_empty() || platform.is_some())
    {
        let mut kinds: Vec<_> = h.points.iter().map(|p| p.kind.as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds.sort_by_key(|kind| {
            h.points
                .iter()
                .rev()
                .find(|p| p.kind == *kind)
                .and_then(|p| p.window_minutes)
                .unwrap_or(i64::MAX)
        });
        let colors: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| {
                let weekly = h
                    .points
                    .iter()
                    .rev()
                    .find(|p| p.kind == *kind)
                    .and_then(|p| p.window_minutes)
                    .is_some_and(|m| m >= 10080);
                COLORS[if weekly { 1 } else { i % 2 }]
            })
            .collect();
        let interval = h.interval_seconds;
        let last = (h.to_utc - h.from_utc) / interval - 1;
        let selected = selected
            .unwrap_or(h.to_utc - interval)
            .clamp(h.from_utc, h.to_utc - interval);
        let cursor = [
            ((selected - h.from_utc) as f64 / interval as f64, 0.0),
            ((selected - h.from_utc) as f64 / interval as f64, 100.0),
        ];
        let mut segments: Vec<(usize, Vec<(f64, f64)>)> = Vec::new();
        for (index, kind) in kinds.iter().enumerate() {
            let mut points = Vec::new();
            let mut previous = None;
            for p in h.points.iter().filter(|p| p.kind == *kind) {
                if p.hour < h.from_utc || p.hour >= h.to_utc || !p.used_percent.is_finite() {
                    continue;
                }
                if previous.is_some_and(|(hour, minutes)| {
                    p.hour != hour + interval || p.window_minutes != minutes
                }) && !points.is_empty()
                {
                    segments.push((index, std::mem::take(&mut points)));
                }
                points.push((
                    (p.hour - h.from_utc) as f64 / interval as f64,
                    quota_left(p.used_percent),
                ));
                previous = Some((p.hour, p.window_minutes));
            }
            if !points.is_empty() {
                segments.push((index, points));
            }
        }
        let mut datasets = vec![Dataset::default()
            .data(&cursor)
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::DarkGray))];
        datasets.extend(segments.iter().map(|(i, points)| {
            Dataset::default()
                .data(points)
                .marker(Marker::Braille)
                .graph_type(if points.len() == 1 {
                    GraphType::Scatter
                } else {
                    GraphType::Line
                })
                .style(Style::default().fg(colors[*i]))
        }));
        let platform_points = platform.map(|series| {
            let x = |hour| (hour - h.from_utc) as f64 / interval as f64;
            let mut points = Vec::with_capacity(series.points.len() * 2);
            let mut previous = None;
            for p in &series.points {
                if let Some(left) = previous {
                    points.push((x(p.hour), left));
                }
                points.push((x(p.hour), p.remaining_percent));
                previous = Some(p.remaining_percent);
            }
            if let Some(last) = series
                .points
                .last()
                .filter(|p| p.hour < series.through_bucket)
            {
                points.push((x(series.through_bucket), last.remaining_percent));
            }
            points
        });
        if let Some(points) = &platform_points {
            datasets.push(
                Dataset::default()
                    .data(points)
                    .marker(Marker::Braille)
                    .graph_type(if points.len() == 1 {
                        GraphType::Scatter
                    } else {
                        GraphType::Line
                    })
                    .style(Style::default().fg(COLORS[3])),
            );
        }
        let label = |at: i64| {
            chrono::DateTime::from_timestamp(at, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format(if span == Some(86400) {
                            "%-I:%M %p"
                        } else {
                            "%b %-d"
                        })
                        .to_string()
                })
                .unwrap_or_default()
        };
        let labels = [0, last / 2, last].map(|i| Line::from(label(h.from_utc + i * interval)));
        Chart::new(datasets)
            .block(block)
            .x_axis(
                Axis::default()
                    .bounds([0.0, last.max(1) as f64])
                    .labels(labels)
                    .style(Style::default().fg(Color::Gray)),
            )
            .y_axis(
                Axis::default()
                    .bounds([0.0, 100.0])
                    .labels(["0%", "25%", "50%", "75%", "100%"])
                    .style(Style::default().fg(Color::Gray)),
            )
            .render(chart_area, &mut buf);
        let mut values = Vec::new();
        for (i, kind) in kinds.iter().enumerate() {
            let latest = h.points.iter().rev().find(|p| p.kind == *kind).unwrap();
            let w = serde_json::json!({"window_minutes":latest.window_minutes});
            let label = render::window_label(&w);
            let value = h
                .points
                .iter()
                .find(|p| p.kind == *kind && p.hour == selected)
                .map(|p| {
                    format!(
                        "{} left",
                        render::percent(&quota_left(p.used_percent).into())
                    )
                })
                .unwrap_or_else(|| "Not observed".into());
            values.push(Span::styled(
                format!("{label} {value}    "),
                Style::default().fg(colors[i]),
            ));
        }
        if let Some(series) = platform {
            let value = if selected <= series.through_bucket {
                let idx = series.points.partition_point(|p| p.hour <= selected);
                idx.checked_sub(1)
                    .map(|i| series.points[i].remaining_percent)
            } else {
                None
            };
            let text = value.map_or_else(
                || "Not observed".into(),
                |left| format!("{} left", render::percent(&left.into())),
            );
            values.push(Span::styled(
                format!("Platform API {text}"),
                Style::default().fg(COLORS[3]),
            ));
        }
        put(
            &mut buf,
            header + 14,
            chart_date(Some(selected)),
            Color::White,
        );
        if width < 85 {
            for (i, value) in values.into_iter().enumerate() {
                put(
                    &mut buf,
                    header + 15 + i as u16,
                    Line::from(value),
                    Color::White,
                );
            }
        } else {
            put(&mut buf, header + 15, Line::from(values), Color::White);
        }
    } else {
        Paragraph::new(if account["quota_history"].is_object() {
            "No recorded observations yet."
        } else {
            "Limit history requires an updated router."
        })
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::Gray))
        .block(block)
        .render(chart_area, &mut buf);
    }
    for y in 0..area.height {
        let source = y.saturating_add(scroll);
        if source >= top_height {
            break;
        }
        for x in 0..width {
            frame.buffer_mut()[(area.x + x, area.y + y)] = buf[(x, source)].clone();
        }
    }
    let top = top_height.saturating_sub(scroll).min(area.height);
    if top < area.height {
        frame.render_widget(
            Paragraph::new(render::account_details(account, now))
                .wrap(Wrap { trim: false })
                .scroll((scroll.saturating_sub(top_height), 0)),
            Rect::new(area.x, area.y + top, width, area.height - top),
        );
    }
}
fn quota_left(used: f64) -> f64 {
    (100.0 - used).clamp(0.0, 100.0)
}
fn chart_date(at: Option<i64>) -> String {
    at.and_then(|at| chrono::DateTime::from_timestamp(at, 0))
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%b %-d, %I:%M %p")
                .to_string()
        })
        .unwrap_or_else(|| "Unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use serde_json::json;
    #[test]
    fn both_windows_share_an_hourly_plot_and_missing_hours_stay_unknown() {
        let base = 1_800_000_000;
        let account = json!({"label":"Synthetic account","quota":{"windows":[{"window_minutes":480,"remaining_percent":64,"reset_at":base+3600},{"window_minutes":10080,"remaining_percent":23,"reset_at":base+604800}]},"quota_history":{"from_utc":base-167*3600,"to_utc":base+3600,"interval_seconds":3600,"points":[{"kind":"primary","hour":base,"observed_at":base,"used_percent":36.0,"window_minutes":480,"reset_at":base+3600},{"kind":"secondary","hour":base,"observed_at":base,"used_percent":77.0,"window_minutes":10080,"reset_at":base+604800}]}});
        for (width, height) in [(64, 18), (104, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| draw(f, f.area(), &account, base, 0, None, &Forecasts::default()))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Limit history"));
            assert!(text.contains("8-hour") && text.contains("Weekly"));
            if height >= 25 {
                assert!(text.contains(" Left "));
                assert!(text.contains("8-hour 64% left"));
                assert!(text.contains("Weekly 23% left"));
                assert!(!text.contains("Used quota") && !text.contains(" used"));
            }
            terminal
                .draw(|f| {
                    draw(
                        f,
                        f.area(),
                        &account,
                        base,
                        20,
                        Some(base - 3600),
                        &Forecasts::default(),
                    )
                })
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Not observed"));
        }
    }
    #[test]
    fn chart_uses_remaining_percent_with_full_and_exhausted_boundaries() {
        for (used, left) in [
            (0.0, 100.0),
            (36.0, 64.0),
            (77.0, 23.0),
            (100.0, 0.0),
            (125.0, 0.0),
        ] {
            assert_eq!(quota_left(used), left);
        }
    }

    #[test]
    fn platform_shares_the_quota_row_and_percent_plot_without_billing_prose() {
        let base = 1_800_000_000 / 86400 * 86400;
        let account = json!({"label":"Synthetic","subscription":{"plan_usd":20,"billing_day":14},"clients":{"count":0},"platform":{"balance":{"entered_at":base+123,"initial_usd_nanos":86_000_000_000_i64,"exetrouter":{"remaining_usd_nanos":43_000_000_000_i64,"status":"estimated"}}},"quota":{"windows":[{"window_minutes":10080,"remaining_percent":92}]},"quota_history":{"from_utc":base,"to_utc":base+86400,"interval_seconds":600,"points":[{"kind":"primary","hour":base+1200,"observed_at":base+1200,"used_percent":8,"window_minutes":10080,"reset_at":null}],"platform_balance":{"entered_at":base+123,"initial_usd_nanos":86_000_000_000_i64,"through_bucket":base+1800,"points":[{"hour":base,"remaining_percent":100},{"hour":base+600,"remaining_percent":0}]}}});
        let mut terminal = Terminal::new(TestBackend::new(104, 60)).unwrap();
        for (cursor, platform) in [(base + 1200, "0% left"), (base + 2400, "Not observed")] {
            terminal
                .draw(|f| {
                    draw(
                        f,
                        f.area(),
                        &account,
                        base + 86400,
                        0,
                        Some(cursor),
                        &Forecasts::default(),
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let titles: String = (0..104).map(|x| buffer[(x, 2)].symbol()).collect();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(titles.contains("Weekly") && titles.contains("Platform API"));
            let platform_x = titles.find("Platform API").unwrap() as u16;
            assert_eq!(buffer[(platform_x, 4)].fg, Color::Rgb(232, 191, 82));
            assert_eq!(buffer[(103, 4)].fg, Color::DarkGray);
            assert!(text.contains("50% left"));
            assert!(
                text.contains("$43 / $86") && text.contains(&format!("Platform API {platform}"))
            );
            assert_eq!(text.matches("Limit history").count(), 1);
            assert!(
                !text.contains("daily USD")
                    && !text.contains("Billing source")
                    && !text.contains("15 minutes")
            );
            assert!(text.contains("Subscription price: $20/month"));
        }
        let mut changed = account.clone();
        changed["platform"]["balance"]["entered_at"] = json!(base + 600);
        assert!(platform_history(&changed, &history(&changed).unwrap()).is_none());
        changed["quota_history"]["platform_balance"] = json!({"through_bucket":false});
        assert_eq!(history(&changed).unwrap().points.len(), 1);
        assert_eq!(chart_header(&account, 48), 12);
    }

    #[test]
    fn a_single_weekly_meter_uses_the_full_width() {
        let account = json!({"label":"Synthetic", "quota":{"windows":[{"window_minutes":10080,"remaining_percent":100,"observed_at":1_800_000_000,"status":"current"}]}});
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    f.area(),
                    &account,
                    1_800_000_000,
                    0,
                    None,
                    &Forecasts::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!((0..80).all(
            |x| buffer[(x, 4)].symbol() == "━" && buffer[(x, 4)].fg == Color::Rgb(93, 195, 143)
        ));
    }

    #[test]
    fn ten_minute_history_accepts_a_full_week_and_validates_bucket_bounds() {
        let base = 1_800_000_000;
        let points: Vec<_> = (0..1008).flat_map(|i| ["primary", "secondary"].map(|kind| json!({"kind":kind,"hour":base+i*600,"observed_at":base+i*600+599,"used_percent":20,"window_minutes":480,"reset_at":null}))).collect();
        let mut account = json!({"quota_history":{"from_utc":base,"to_utc":base+7*86400,"interval_seconds":600,"points":points}});
        assert_eq!(history(&account).unwrap().points.len(), 2016);
        assert_eq!(range(&account), Some((base, base + 7 * 86400, 600)));
        let mut terminal = Terminal::new(TestBackend::new(104, 40)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    f.area(),
                    &account,
                    base + 7 * 86400,
                    0,
                    Some(base + 600),
                    &Forecasts::default(),
                )
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(!text.contains("10-minute"));
        assert!(text.contains("80% left"));
        account["quota_history"]["points"][0]["observed_at"] = json!(base + 600);
        assert!(history(&account).is_none());
    }
}
