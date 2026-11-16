//! Scrollable account details with a fixed-cost, clipped chart header.
use super::render;
use crate::quota_history::History;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Chart, Dataset, GraphType, Paragraph, Widget, Wrap,
    },
    Frame,
};
use serde_json::Value;

const TOP: u16 = 26;
pub(super) const COLORS: [Color; 6] = [
    Color::Rgb(132, 190, 203),
    Color::Rgb(170, 163, 230),
    Color::Rgb(143, 201, 164),
    Color::Rgb(228, 189, 121),
    Color::Rgb(227, 147, 147),
    Color::Rgb(133, 168, 225),
];

fn history(account: &Value) -> Option<History> {
    let mut h: History = serde_json::from_value(account["quota_history"].clone()).ok()?;
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
fn details(account: &Value, now: i64) -> String {
    render::account_details(account, now)
        .lines()
        .skip(2)
        .collect::<Vec<_>>()
        .join("\n")
}
pub(super) fn height(account: &Value, now: i64, width: u16) -> usize {
    top_height(account) as usize
        + Paragraph::new(details(account, now))
            .wrap(Wrap { trim: false })
            .line_count(width.max(1))
}

fn top_height(account: &Value) -> u16 {
    TOP + if account["platform"].is_object() {
        4
    } else {
        0
    } + if platform_history(account).is_some() {
        15
    } else {
        0
    }
}
fn platform_history(account: &Value) -> Option<crate::platform::costs::History> {
    if !account["platform"].is_object() {
        return None;
    }
    let h: crate::platform::costs::History =
        serde_json::from_value(account["quota_history"]["platform"].clone()).ok()?;
    let mut seen = std::collections::BTreeSet::new();
    (h.interval_seconds == 86400
        && h.to_utc > h.from_utc
        && h.to_utc - h.from_utc <= 90 * 86400
        && h.points.len() <= 92
        && h.points.iter().all(|p| {
            p.from_utc >= h.from_utc
                && p.from_utc % 86400 == 0
                && p.to_utc <= h.to_utc
                && p.to_utc > p.from_utc
                && p.to_utc <= p.from_utc + 86400
                && p.observed_at >= p.to_utc
                && p.usd_nanos >= 0
                && seen.insert(p.from_utc)
        }))
    .then_some(h)
}
pub(super) fn cost_meter(buf: &mut Buffer, area: Rect, percent: Option<f64>) {
    if area.is_empty() {
        return;
    }
    let filled =
        (f64::from(area.width) * percent.unwrap_or(0.).clamp(0., 100.) / 100.).round() as u16;
    for x in 0..area.width {
        buf[(area.x + x, area.y)]
            .set_char('━')
            .set_style(Style::default().fg(if x < filled {
                Color::Rgb(210, 180, 125)
            } else {
                Color::DarkGray
            }));
    }
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
) {
    if area.is_empty() {
        return;
    }
    let width = area.width;
    let top_height = top_height(account);
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
    let windows: Vec<_> = account["quota"]["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["window_minutes"].as_i64().is_some())
        .take(2)
        .collect();
    let columns = windows.len().max(1) as u16;
    for (i, w) in windows.iter().enumerate() {
        let gap = if columns > 1 { 2 } else { 0 };
        let col_width = width.saturating_sub(gap) / columns;
        let x = i as u16 * (col_width + gap);
        let wwidth = if i + 1 == windows.len() {
            width - x
        } else {
            col_width
        };
        let label = render::window_label(w);
        Paragraph::new(label)
            .style(Style::default().fg(Color::Gray))
            .render(Rect::new(x, 2, wwidth, 1), &mut buf);
        Paragraph::new(format!(
            "{} remaining",
            render::percent(&w["remaining_percent"])
        ))
        .style(Style::default().fg(COLORS[i]).bold())
        .render(Rect::new(x, 3, wwidth, 1), &mut buf);
        meter(
            &mut buf,
            Rect::new(x, 4, wwidth, 1),
            w["remaining_percent"].as_f64(),
        );
        Paragraph::new(format!("Resets {}", chart_date(w["reset_at"].as_i64())))
            .style(Style::default().fg(Color::Gray))
            .render(Rect::new(x, 5, wwidth, 1), &mut buf);
    }
    put(
        &mut buf,
        7,
        Line::styled(
            if account["quota_history"]["interval_seconds"] == 3600 {
                "Subscription history · hourly"
            } else {
                "Subscription history · 10-minute steps"
            },
            Style::default().bold(),
        ),
        Color::White,
    );
    let chart_area = Rect::new(0, 9, width, 12);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Quota left ");
    let span = range(account).map(|(from, to, _)| to - from);
    let mut periods = Vec::new();
    for (label, duration) in [
        ("24 hours", 86400),
        ("7 days", 7 * 86400),
        ("30 days", 30 * 86400),
        ("90 days", 90 * 86400),
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
    put(&mut buf, 8, Line::from(periods), Color::White);
    if let Some(h) = history(account).filter(|h| !h.points.is_empty()) {
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
        let label = |at: i64| {
            chrono::DateTime::from_timestamp(at, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%b %-d").to_string())
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
        let mut legend = Vec::new();
        let mut values = Vec::new();
        for (i, kind) in kinds.iter().enumerate() {
            let latest = h.points.iter().rev().find(|p| p.kind == *kind).unwrap();
            let w = serde_json::json!({"window_minutes":latest.window_minutes});
            let label = render::window_label(&w);
            legend.push(Span::styled(
                format!("● {label}   "),
                Style::default().fg(colors[i]),
            ));
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
        Paragraph::new(Line::from(legend))
            .alignment(Alignment::Right)
            .render(Rect::new(0, 25, width, 1), &mut buf);
        put(&mut buf, 22, chart_date(Some(selected)), Color::White);
        if width < 80 {
            for (i, value) in values.into_iter().take(2).enumerate() {
                put(&mut buf, 23 + i as u16, Line::from(value), Color::White);
            }
        } else {
            put(&mut buf, 23, Line::from(values), Color::White);
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
    let shift = if account["platform"].is_object() {
        4
    } else {
        0
    };
    if shift > 0 {
        for y in (7..TOP).rev() {
            for x in 0..width {
                buf[(x, y + shift)] = buf[(x, y)].clone();
            }
        }
        for y in 7..11 {
            for x in 0..width {
                buf[(x, y)].reset();
            }
        }
        let c = &account["platform"]["balance"];
        let remaining = c["exetrouter"]["remaining_usd_nanos"]
            .as_i64()
            .map(|n| n as f64 / 1e9);
        let budget = c["initial_usd_nanos"].as_i64().map(|n| n as f64 / 1e9);
        put(
            &mut buf,
            7,
            crate::platform::balance::headline(&account["platform"]),
            COLORS[3],
        );
        cost_meter(
            &mut buf,
            Rect::new(0, 8, width, 1),
            remaining
                .zip(budget)
                .filter(|(_, b)| *b > 0.)
                .map(|(s, b)| s / b * 100.),
        );
        put(
            &mut buf,
            9,
            "ExetRouter token estimate · organization alternative below",
            Color::Gray,
        );
    }
    if let Some(h) = platform_history(account) {
        let y = TOP + shift;
        put(
            &mut buf,
            y,
            "Platform API costs · daily USD · unavailable days remain gaps",
            COLORS[3],
        );
        let points: Vec<_> = h
            .points
            .iter()
            .map(|p| {
                (
                    (p.from_utc - h.from_utc) as f64 / 86400.,
                    p.usd_nanos as f64 / 1e9,
                )
            })
            .collect();
        let maximum = points.iter().map(|(_, usd)| *usd).fold(0.01, f64::max);
        let data = Dataset::default()
            .data(&points)
            .marker(Marker::Braille)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(COLORS[3]));
        let label = |at: i64| {
            chrono::DateTime::from_timestamp(at, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%b %-d").to_string())
                .unwrap_or_default()
        };
        Chart::new(vec![data])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .x_axis(
                Axis::default()
                    .bounds([0., ((h.to_utc - h.from_utc) as f64 / 86400.).max(1.)])
                    .labels([label(h.from_utc), label(h.to_utc)]),
            )
            .y_axis(Axis::default().bounds([0., maximum]).labels([
                crate::money::usd(0.),
                crate::money::usd(maximum / 2.),
                crate::money::usd(maximum),
            ]))
            .render(Rect::new(0, y + 1, width, 11), &mut buf);
        let selected = selected.unwrap_or(h.to_utc - 1);
        let point = h
            .points
            .iter()
            .find(|p| p.from_utc <= selected && selected < p.to_utc);
        put(
            &mut buf,
            y + 12,
            format!(
                "{} · {}",
                chart_date(Some(selected)),
                point
                    .map(|p| crate::money::usd(p.usd_nanos as f64 / 1e9))
                    .unwrap_or_else(|| "Not observed".into())
            ),
            Color::Gray,
        );
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
            Paragraph::new(details(account, now))
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
                .draw(|f| draw(f, f.area(), &account, base, 0, None))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Subscription history"));
            assert!(text.contains("8-hour") && text.contains("Weekly"));
            if height >= 25 {
                assert!(text.contains("Quota left"));
                assert!(text.contains("8-hour 64% left"));
                assert!(text.contains("Weekly 23% left"));
                assert!(!text.contains("Used quota") && !text.contains(" used"));
            }
            terminal
                .draw(|f| draw(f, f.area(), &account, base, 20, Some(base - 3600)))
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
    fn platform_costs_have_a_full_width_meter_and_separate_daily_dollar_history() {
        let base = 1_800_000_000 / 86400 * 86400;
        let account = json!({"label":"Synthetic","platform":{"costs":{"spent_usd_nanos":5_000_000_000_i64,"budget_cents":10000}},"quota":{"windows":[{"window_minutes":10080,"remaining_percent":100}]},"quota_history":{"from_utc":base,"to_utc":base+86400,"interval_seconds":600,"points":[],"platform":{"from_utc":base,"to_utc":base+86400,"interval_seconds":86400,"points":[{"from_utc":base,"to_utc":base+86400,"observed_at":base+86400,"usd_nanos":5_000_000_000_i64}]}}});
        let mut terminal = Terminal::new(TestBackend::new(104, 60)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), &account, base + 86400, 0, None))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("$5 / $100") && text.contains("daily USD"));
        assert!((0..104).all(|x| terminal.backend().buffer()[(x, 8)].symbol() == "━"));
        let mut disconnected = account;
        disconnected.as_object_mut().unwrap().remove("platform");
        assert!(platform_history(&disconnected).is_none());
    }

    #[test]
    fn a_single_weekly_meter_uses_the_full_width() {
        let account = json!({"label":"Synthetic", "quota":{"windows":[{"window_minutes":10080,"remaining_percent":100}]}});
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), &account, 1_800_000_000, 0, None))
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
            .draw(|f| draw(f, f.area(), &account, base + 7 * 86400, 0, Some(base + 600)))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("10-minute steps"));
        assert!(text.contains("80% left"));
        account["quota_history"]["points"][0]["observed_at"] = json!(base + 600);
        assert!(history(&account).is_none());
    }
}
