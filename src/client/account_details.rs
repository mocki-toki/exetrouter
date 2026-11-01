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
        Axis, Block, BorderType, Borders, Chart, Dataset, Gauge, GraphType, Paragraph, Widget, Wrap,
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
    let valid = h.interval_seconds == 3600
        && h.from_utc >= 0
        && h.from_utc % 3600 == 0
        && h.to_utc % 3600 == 0
        && h.to_utc
            .checked_sub(h.from_utc)
            .is_some_and(|span| (1..=168 * 3600).contains(&span))
        && h.points.len() <= 336
        && h.points.iter().all(|p| {
            matches!(p.kind.as_str(), "primary" | "secondary")
                && p.hour >= h.from_utc
                && p.hour < h.to_utc
                && p.hour % 3600 == 0
                && p.observed_at >= p.hour
                && p.observed_at < p.hour + 3600
                && p.used_percent.is_finite()
                && p.used_percent >= 0.0
                && seen.insert((p.kind.clone(), p.hour))
        });
    h.points.sort_by_key(|p| p.hour);
    valid.then_some(h)
}
pub(super) fn range(account: &Value) -> Option<(i64, i64)> {
    history(account).map(|h| (h.from_utc, h.to_utc))
}
fn details(account: &Value, now: i64) -> String {
    let details = render::account_details(account, now)
        .lines()
        .skip(2)
        .collect::<Vec<_>>()
        .join("\n");
    format!("Routing\n{details}")
}
pub(super) fn height(account: &Value, now: i64, width: u16) -> usize {
    TOP as usize
        + Paragraph::new(details(account, now))
            .wrap(Wrap { trim: false })
            .line_count(width.max(1))
}
fn put(buf: &mut Buffer, y: u16, value: impl Into<Line<'static>>, color: Color) {
    Paragraph::new(value.into())
        .style(Style::default().fg(color))
        .render(Rect::new(0, y, buf.area.width, 1), buf);
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
    let mut buf = Buffer::empty(Rect::new(0, 0, width, TOP));
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
        let x = i as u16 * width / columns;
        let wwidth = (width / columns).saturating_sub(2);
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
        if let Some(left) = w["remaining_percent"].as_f64() {
            Gauge::default()
                .ratio((left / 100.0).clamp(0.0, 1.0))
                .label("")
                .gauge_style(Style::default().fg(COLORS[i]).bg(Color::DarkGray))
                .render(Rect::new(x, 4, wwidth, 1), &mut buf);
        }
        Paragraph::new(format!("Resets {}", chart_date(w["reset_at"].as_i64())))
            .style(Style::default().fg(Color::Gray))
            .render(Rect::new(x, 5, wwidth, 1), &mut buf);
    }
    put(
        &mut buf,
        7,
        Line::styled(
            "Limit history · last 7 days · hourly",
            Style::default().bold(),
        ),
        Color::White,
    );
    let chart_area = Rect::new(0, 9, width, 12);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Used quota ");
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
        let last = (h.to_utc - h.from_utc) / 3600 - 1;
        let selected = selected
            .unwrap_or(h.to_utc - 3600)
            .clamp(h.from_utc, h.to_utc - 3600);
        let cursor = [
            ((selected - h.from_utc) as f64 / 3600.0, 0.0),
            ((selected - h.from_utc) as f64 / 3600.0, 100.0),
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
                    p.hour != hour + 3600 || p.window_minutes != minutes
                }) && !points.is_empty()
                {
                    segments.push((index, std::mem::take(&mut points)));
                }
                points.push((
                    (p.hour - h.from_utc) as f64 / 3600.0,
                    p.used_percent.clamp(0.0, 100.0),
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
        let labels = [0, last / 2, last].map(|i| Line::from(label(h.from_utc + i * 3600)));
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
                        "{} used / {} left",
                        render::percent(&p.used_percent.into()),
                        render::percent(&(100.0 - p.used_percent).max(0.0).into())
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
            .render(Rect::new(0, 8, width, 1), &mut buf);
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
            "Hourly history requires an updated router."
        })
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::Gray))
        .block(block)
        .render(chart_area, &mut buf);
    }
    for y in 0..area.height {
        let source = y.saturating_add(scroll);
        if source >= TOP {
            break;
        }
        for x in 0..width {
            frame.buffer_mut()[(area.x + x, area.y + y)] = buf[(x, source)].clone();
        }
    }
    let top = TOP.saturating_sub(scroll).min(area.height);
    if top < area.height {
        frame.render_widget(
            Paragraph::new(details(account, now))
                .wrap(Wrap { trim: false })
                .scroll((scroll.saturating_sub(TOP), 0)),
            Rect::new(area.x, area.y + top, width, area.height - top),
        );
    }
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
            assert!(text.contains("Limit history"));
            assert!(text.contains("8-hour") && text.contains("Weekly"));
            if height >= 25 {
                assert!(text.contains("36% used / 64% left"));
                assert!(text.contains("77% used / 23% left"));
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
}
