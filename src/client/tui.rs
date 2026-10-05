use super::{clipboard::Clipboard, config::Connection, render, ssh_request, Session};
use crate::{ControlRequest, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, GraphType, Padding, Paragraph, Row, Table,
        TableState, Tabs, Wrap,
    },
    Frame, Terminal,
};
use serde_json::Value;
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

const USAGE_PERIODS: [(&str, &str); 4] = [
    ("day", "Today"),
    ("24h", "Last 24 hours"),
    ("week", "This week"),
    ("month", "This month"),
];
const LIMIT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const OVERVIEW: usize = 0;
const USAGE: usize = 1;
const TOKENS: usize = 2;
const MODELS: usize = 3;
const SETTINGS: usize = 4;
const TABS: [&str; 5] = ["Overview", "Usage", "Tokens", "Models", "Settings"];
const HEADER_HEIGHT: u16 = 3;
const CONTROLS_HEIGHT: u16 = 3;
#[derive(Default)]
struct State {
    tab: usize,
    values: [Option<Value>; 5],
    selected: usize,
    account_selected: usize,
    model_selected: usize,
    scroll: u16,
    period: usize,
    group: usize,
    usage_selected: usize,
    request_metric: bool,
    personal: [String; 2],
    statuses: [String; 5],
    notices: [Option<String>; 5],
    forecasts: super::quota_forecast::Forecasts,
    modal: Option<Modal>,
    clipboard: Option<Clipboard>,
    update: Option<crate::update::Report>,
    update_checked: bool,
}
enum Modal {
    Actions {
        title: String,
        entries: Vec<(String, KeyCode)>,
        selected: usize,
    },
    Error {
        title: &'static str,
        message: String,
    },
    Routing {
        account: i64,
        fields: Vec<TextInput>,
        selected: usize,
        error: String,
        summary: String,
        locked: bool,
    },
    UpdateConfirm,
    Name(String),
    Personal {
        kind: super::privacy::Kind,
        id: String,
        input: TextInput,
    },
    Days {
        name: String,
        text: String,
    },
    Confirm {
        id: String,
        rotate: bool,
    },
    ClipboardRetry {
        secret: String,
        clipboard: Clipboard,
    },
    ResetConfirm(Value),
    Help,
}
struct UpdateJob {
    task: tokio::task::JoinHandle<Result<crate::update::Report>>,
    installing: bool,
}
impl Drop for UpdateJob {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn update_job(installing: bool) -> UpdateJob {
    UpdateJob {
        installing,
        task: tokio::spawn(async move {
            let report = crate::update::check().await?;
            if installing {
                crate::update::install("exr", &report).await?;
            }
            Ok(report)
        }),
    }
}
fn finish_update(report: &mut crate::update::Report) -> String {
    if report.available {
        report.current = report.latest.trim_start_matches('v').into();
        report.available = false;
        format!(
            "Updated exr to {}. Quit and reopen the dashboard.",
            report.latest
        )
    } else {
        format!("exr {} is already up to date.", report.current)
    }
}
fn update_text(state: &State) -> String {
    match &state.update {
        Some(r) if r.available => {
            format!("Updates · exr {} → {} available\n\n", r.current, r.latest)
        }
        Some(r) => format!("Updates · exr {} is up to date\n\n", r.current),
        None => format!(
            "Updates · exr {} · Not checked\n\n",
            env!("CARGO_PKG_VERSION")
        ),
    }
}
struct Screen;
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}
struct Pending {
    task: tokio::task::JoinHandle<Result<Value>>,
    tab: usize,
    mutation: bool,
    reset_prepare: bool,
    reset_confirm: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn request(state: &State) -> ControlRequest {
    match state.tab {
        TOKENS => ControlRequest::TokenList,
        MODELS => ControlRequest::Models,
        USAGE => ControlRequest::Usage {
            period: USAGE_PERIODS[state.period].0.into(),
            by: [None, Some("user"), Some("model"), Some("token")][state.group].map(str::to_owned),
            timezone: Some(render::system_timezone()),
        },
        _ => ControlRequest::Limits,
    }
}
fn start(connection: &Connection, req: ControlRequest, tab: usize, mutation: bool) -> Pending {
    let session = Session {
        connection: connection.clone(),
        json: false,
        command: super::CommandLine::Doctor,
    };
    let reset_prepare = matches!(req, ControlRequest::ResetPrepare { .. });
    let reset_confirm = matches!(req, ControlRequest::ResetConfirm { .. });
    Pending {
        reset_prepare,
        reset_confirm,
        task: tokio::spawn(async move {
            if tab == SETTINGS && !mutation {
                let accounts = if session.connection.local.is_some() {
                    super::privacy::local_accounts(&session).await?
                } else {
                    ssh_request(&session, ControlRequest::Doctor).await?
                };
                let personal = match (
                    super::privacy::get(&session, super::privacy::Kind::Note, "personal").await,
                    super::privacy::get(&session, super::privacy::Kind::Project, "personal").await,
                ) {
                    (Ok(note), Ok(project)) => {
                        Some([note.unwrap_or_default(), project.unwrap_or_default()])
                    }
                    _ => None,
                };
                return Ok(serde_json::json!({"accounts": accounts, "personal": personal}));
            }
            ssh_request(&session, req).await
        }),
        tab,
        mutation,
    }
}
fn body(state: &State) -> String {
    let Some(value) = &state.values[state.tab] else {
        return "No data yet. Press r to load this view.".into();
    };
    match state.tab {
        TOKENS => {
            let rows = value.as_array().cloned().unwrap_or_default();
            let mut text = String::new();
            let first = state.selected / 8 * 8;
            for (i, row) in rows.iter().enumerate().skip(first).take(8) {
                text.push_str(if i == state.selected { "▶ " } else { "  " });
                text.push_str(&format!(
                    "{}  {}\n",
                    render::safe(row["name"].as_str().unwrap_or("Unnamed")),
                    render::safe(row["id"].as_str().unwrap_or("Unknown"))
                ));
            }
            if rows.len() > 8 {
                text.push_str(&format!(
                    "\nShowing {}–{} of {}\n",
                    first + 1,
                    (first + 8).min(rows.len()),
                    rows.len()
                ));
            }
            if rows.is_empty() {
                text.push_str("No API tokens yet.\n");
            }
            if let Some(token) = rows.get(state.selected) {
                text.push_str("\nTOKEN INFORMATION\n");
                text.push_str(&render::response("token", token));
            }
            text
        }
        MODELS => render::response("models", value),
        USAGE => render::response("usage", value),
        _ => render::response("doctor", value),
    }
}
fn mode_badge(connection: &Connection) -> Span<'static> {
    let (label, background, foreground) = if connection.mode == super::config::Mode::Standalone {
        (
            " Standalone ",
            Color::Rgb(245, 165, 75),
            Color::Rgb(75, 35, 0),
        )
    } else {
        (
            " Remote Server ",
            Color::Rgb(177, 130, 230),
            Color::Rgb(40, 15, 65),
        )
    };
    Span::styled(
        label,
        Style::default()
            .fg(foreground)
            .bg(background)
            .add_modifier(Modifier::BOLD),
    )
}
fn connection_destination(connection: &Connection) -> String {
    if connection.mode == super::config::Mode::Standalone {
        connection
            .local
            .as_ref()
            .map(|local| local.address.to_string())
            .unwrap_or_else(|| connection.listen.to_string())
    } else {
        render::safe(&format!(
            "{}@{}:{}",
            connection.ssh_user, connection.host, connection.port
        ))
    }
}
fn header(frame: &mut Frame<'_>, area: Rect, state: &State, connection: &Connection) {
    let brand = format!("ExetRouter {}", env!("CARGO_PKG_VERSION"));
    let inner = Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1);
    let budget = usize::from(inner.width).saturating_sub(brand.len() + 2);
    let mut destination = connection_destination(connection);
    if Line::from(destination.as_str()).width() > budget {
        let mut shortened = String::new();
        for ch in destination.chars() {
            if Line::from(format!("{shortened}{ch}…")).width() > budget {
                break;
            }
            shortened.push(ch);
        }
        shortened.push('…');
        destination = shortened;
    }
    let columns = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(Line::from(destination.as_str()).width() as u16),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(brand).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(destination)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Right),
        columns[1],
    );
    frame.render_widget(
        Tabs::new(TABS.iter().enumerate().map(|(index, title)| {
            if index == SETTINGS && state.update.as_ref().is_some_and(|r| r.available) {
                Line::styled(
                    " Settings ↑ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(format!(" {title} "))
            }
        }))
        .select(state.tab)
        .padding("", "")
        .divider("  ")
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x + 1, area.y + 2, area.width.saturating_sub(2), 1),
    );
}
fn status_lines(state: &State, busy: bool, width: u16) -> Vec<String> {
    let notice = if busy {
        "◌ Working…".into()
    } else if let Some(notice) = &state.notices[state.tab] {
        render::safe(notice)
    } else if !state.statuses[state.tab].starts_with("Updated ") {
        render::safe(&state.statuses[state.tab])
    } else {
        String::new()
    };
    let width = usize::from(width.saturating_sub(4).max(1));
    let mut lines = vec![String::new()];
    for word in notice.split_inclusive(char::is_whitespace) {
        if !lines.last().unwrap().is_empty()
            && Line::from(format!("{}{word}", lines.last().unwrap())).width() > width
        {
            lines.push(String::new());
        }
        for ch in word.chars() {
            let line = lines.last_mut().unwrap();
            if Line::from(format!("{line}{ch}")).width() > width {
                lines.push(ch.to_string());
            } else {
                line.push(ch);
            }
        }
    }
    lines
}
fn overview_viewport(height: u16, width: u16, state: &State, busy: bool) -> u16 {
    height.saturating_sub(
        HEADER_HEIGHT + CONTROLS_HEIGHT + status_lines(state, busy, width).len() as u16,
    )
}
fn selection_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}
fn section_style() -> Style {
    Style::default()
        .bg(Color::Rgb(30, 42, 57))
        .fg(Color::Rgb(116, 190, 240))
        .add_modifier(Modifier::BOLD)
}
fn section_heading(text: &str, width: u16) -> Line<'static> {
    Line::styled(
        format!(
            "  {text:<width$}",
            width = usize::from(width).saturating_sub(2)
        ),
        section_style(),
    )
}
fn token_lines(state: &State, width: u16) -> Vec<Line<'static>> {
    body(state)
        .lines()
        .flat_map(|line| {
            if line == "TOKEN INFORMATION" {
                vec![section_heading(line, width)]
            } else if line.starts_with("▶ ") {
                vec![Line::styled(line.to_owned(), selection_style())]
            } else {
                // Token list rows already reserve two columns for the selection marker.
                let line = if line.starts_with("  ") {
                    line.to_owned()
                } else {
                    format!("  {line}")
                };
                styled_text(&line)
            }
        })
        .collect()
}
fn settings_lines(connection: &Connection, state: &State, width: u16) -> Vec<Line<'static>> {
    let heading = |text: &str| section_heading(text, width);
    let field = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("  {label:<14}"), Style::default().fg(Color::Gray)),
            Span::styled(render::safe(&value), Style::default().fg(Color::White)),
        ])
    };
    let mut lines = vec![
        Line::from(vec![Span::raw("  "), mode_badge(connection)]),
        Line::default(),
        heading("CONNECTION"),
    ];
    if connection.mode == super::config::Mode::Standalone {
        let address = connection
            .local
            .as_ref()
            .map_or(connection.listen, |local| local.address);
        lines.push(field("API endpoint", format!("http://{address}/v1")));
        lines.push(field("Private state", connection.state_dir.clone()));
        lines.push(Line::default());
        lines.push(heading("LOCAL ACCOUNTS"));
        let accounts = state.values[SETTINGS].as_ref().and_then(Value::as_array);
        if accounts.is_none_or(Vec::is_empty) {
            lines.push(Line::styled(
                "  No accounts yet. Add one to get started.",
                Style::default().fg(Color::Gray),
            ));
        }
        for (i, account) in accounts.into_iter().flatten().enumerate() {
            let selected = i == state.selected;
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{}{}",
                        if selected { "▶ " } else { "  " },
                        render::safe(
                            account
                                .get("display_name")
                                .unwrap_or(&account["email"])
                                .as_str()
                                .unwrap_or("Email unavailable")
                        )
                    ),
                    if selected {
                        selection_style()
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(
                    format!(
                        "  ·  {}",
                        render::safe(account["state"].as_str().unwrap_or("unknown"))
                    ),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
    } else {
        lines.push(field("SSH host", connection.host.clone()));
        lines.push(field("Port", connection.port.to_string()));
        lines.push(field("SSH username", connection.ssh_user.clone()));
        lines.push(field("Private key", connection.identity.clone()));
    }
    lines.push(Line::default());
    lines.push(heading("PERSONAL DATA · ENCRYPTED"));
    lines.push(field(
        "Local key",
        connection
            .privacy_key
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Unavailable".into()),
    ));
    lines.push(field(
        "Notes",
        if state.personal[0].is_empty() {
            "None".into()
        } else {
            state.personal[0].clone()
        },
    ));
    lines.push(field(
        "Projects",
        if state.personal[1].is_empty() {
            "None".into()
        } else {
            state.personal[1].clone()
        },
    ));
    lines.push(Line::default());
    lines.push(heading("SOFTWARE"));
    let updates = update_text(state);
    lines.push(Line::styled(
        format!("  {}", updates.trim().trim_start_matches("Updates · ")),
        Style::default().fg(if state.update.as_ref().is_some_and(|r| r.available) {
            Color::Yellow
        } else {
            Color::Green
        }),
    ));
    lines
}
fn model_rows(value: &Value) -> Vec<&Value> {
    value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .collect()
}
fn selected_model_id(state: &State) -> Option<&str> {
    state.values[MODELS]
        .as_ref()
        .and_then(|value| model_rows(value).get(state.model_selected).copied())
        .and_then(|row| row["id"].as_str())
        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
}
fn action_menu(state: &State, connection: &Connection) -> (String, Vec<(String, KeyCode)>) {
    let entries = match state.tab {
        OVERVIEW => {
            let Some(row) = state.values[OVERVIEW]
                .as_ref()
                .and_then(|v| v["quota_accounts"].as_array())
                .and_then(|r| r.get(state.account_selected))
            else {
                return ("Account unavailable".into(), vec![]);
            };
            let mut entries = if row["preference"]["enabled"].as_bool() == Some(false) {
                vec![]
            } else {
                vec![("Review reset credit".into(), KeyCode::Char('c'))]
            };
            if row["preference"]["locked"].as_bool() != Some(true) {
                entries.push((
                    if row["preference"]["enabled"].as_bool() == Some(false) {
                        "Activate for me"
                    } else {
                        "Deactivate for me"
                    }
                    .into(),
                    KeyCode::Char('D'),
                ));
            }
            entries.push(("Switching rules".into(), KeyCode::Char('S')));
            entries.push(("Edit personal account label".into(), KeyCode::Char('L')));
            return (
                format!(
                    "{}{}",
                    render::safe(
                        row.get("display_name")
                            .unwrap_or(&row["label"])
                            .as_str()
                            .unwrap_or("Account")
                    ),
                    if row["preference"]["locked"].as_bool() == Some(true) {
                        " · Locked by operator"
                    } else {
                        ""
                    }
                ),
                entries,
            );
        }
        TOKENS => {
            let mut entries = vec![("Create token".into(), KeyCode::Char('n'))];
            if state.values[TOKENS]
                .as_ref()
                .and_then(Value::as_array)
                .is_some_and(|rows| !rows.is_empty())
            {
                entries.insert(0, ("Rotate token".into(), KeyCode::Char('o')));
                entries.insert(1, ("Revoke token".into(), KeyCode::Char('x')));
            }
            entries
        }
        _ => {
            let mut entries = vec![
                ("Update exr".into(), KeyCode::Char('U')),
                ("Check updates".into(), KeyCode::Char('v')),
                ("Configure connection".into(), KeyCode::Char('e')),
            ];
            if connection.local.is_some() {
                entries.extend([
                    ("Add account".into(), KeyCode::Char('a')),
                    ("Reauthorize account".into(), KeyCode::Char('u')),
                ]);
            }
            entries.extend([
                ("Personal notes".into(), KeyCode::Char('N')),
                ("Project names".into(), KeyCode::Char('P')),
            ]);
            entries
        }
    };
    (
        if state.tab == TOKENS {
            "Token actions"
        } else {
            "Settings"
        }
        .into(),
        entries,
    )
}
fn selected_reset_request(state: &State) -> Option<ControlRequest> {
    state.values[OVERVIEW]
        .as_ref()
        .and_then(|value| value["quota_accounts"].as_array())
        .and_then(|rows| rows.get(state.account_selected))
        .and_then(|account| account["id"].as_i64())
        .map(|account| ControlRequest::ResetPrepare { account })
}
fn preserve_selection(state: &mut State, tab: usize, value: &Value) {
    if tab == OVERVIEW {
        let old_id = state.values[OVERVIEW]
            .as_ref()
            .and_then(|old| old["quota_accounts"].as_array())
            .and_then(|rows| rows.get(state.account_selected))
            .and_then(|account| account["id"].as_i64());
        let rows = value["quota_accounts"].as_array();
        state.account_selected = old_id
            .and_then(|id| rows?.iter().position(|row| row["id"].as_i64() == Some(id)))
            .unwrap_or(state.account_selected)
            .min(rows.map_or(0, |rows| rows.len().saturating_sub(1)));
    } else if tab == MODELS {
        let old_id = selected_model_id(state);
        let rows = model_rows(value);
        state.model_selected = old_id
            .and_then(|id| rows.iter().position(|row| row["id"].as_str() == Some(id)))
            .unwrap_or(state.model_selected)
            .min(rows.len().saturating_sub(1));
    }
}
fn credit_expiring_soon(account: &Value, now: i64) -> bool {
    account["reset_credits"]["available_count"]
        .as_i64()
        .is_some_and(|count| count > 0)
        && account["reset_credits"]["credits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|credit| credit["expires_at"].as_i64())
            .min()
            .is_some_and(|expiry| expiry <= now.saturating_add(7 * 24 * 60 * 60))
}
fn credit_warning(account: &Value) -> Line<'static> {
    let text = render::reset_credits(account);
    let gray = Style::default().fg(Color::Gray);
    if let Some((summary, expiry)) = text.split_once(" · next expires ") {
        let (expiry, historical) = expiry
            .strip_suffix(" · historical")
            .map_or((expiry, ""), |date| (date, " · historical"));
        Line::from(vec![
            Span::styled(format!("{summary} · "), gray),
            Span::styled(
                format!("next expires {expiry}"),
                Style::default().fg(Color::Rgb(245, 165, 75)),
            ),
            Span::styled(historical.to_owned(), gray),
        ])
    } else {
        Line::styled(text, gray)
    }
}
fn credit_summary(account: &Value) -> String {
    let text = render::reset_credits(account).replacen("Reset credits:", "resets:", 1);
    if let Some((summary, _)) = text.split_once(" · next expires ") {
        format!(
            "{summary}{}",
            if text.ends_with(" · historical") {
                " · historical"
            } else {
                ""
            }
        )
    } else {
        text
    }
}
fn quota_columns(width: u16) -> usize {
    if width >= 74 {
        2
    } else {
        1
    }
}
fn account_ranges(value: &Value, width: u16) -> Vec<(u16, u16)> {
    let mut start = render::response("doctor", value).lines().count() as u16 + 3;
    value["quota_accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|account| {
            let windows = render::visible_windows(&account["quota"]);
            let height =
                2 + u16::from(credit_expiring_soon(
                    account,
                    chrono::Utc::now().timestamp(),
                )) + u16::from(account["refresh_error"].is_string())
                    + u16::from(render::weekly_activation(account).is_some())
                    + if windows.is_empty() {
                        1
                    } else {
                        windows.len().div_ceil(quota_columns(width)) as u16 * 2
                    }
                    + u16::from(!account["quota"]["cooldown_until"].is_null());
            let range = (start, start.saturating_add(height));
            start = range.1;
            range
        })
        .collect()
}
fn select_item(state: &mut State, down: bool, viewport: u16, width: u16) {
    if state.tab == MODELS {
        let count = state.values[MODELS]
            .as_ref()
            .map_or(0, |value| model_rows(value).len());
        state.model_selected = if down {
            (state.model_selected + 1).min(count.saturating_sub(1))
        } else {
            state.model_selected.saturating_sub(1)
        };
    } else if state.tab == OVERVIEW {
        let ranges = state.values[OVERVIEW]
            .as_ref()
            .map(|value| account_ranges(value, width))
            .unwrap_or_default();
        state.account_selected = if down {
            (state.account_selected + 1).min(ranges.len().saturating_sub(1))
        } else {
            state.account_selected.saturating_sub(1)
        };
        if state.account_selected == 0 {
            state.scroll = 0;
        }
        keep_selected_account_visible(state, viewport, width);
    }
}
fn keep_selected_account_visible(state: &mut State, viewport: u16, width: u16) {
    if state.account_selected == 0 && state.scroll == 0 {
        return;
    }
    let ranges = state.values[OVERVIEW]
        .as_ref()
        .map(|value| account_ranges(value, width))
        .unwrap_or_default();
    if let Some(&(start, end)) = ranges.get(state.account_selected) {
        if start < state.scroll || end.saturating_sub(start) > viewport {
            state.scroll = start;
        } else if end > state.scroll.saturating_add(viewport) {
            state.scroll = end.saturating_sub(viewport);
        }
    }
}
fn models(frame: &mut Frame<'_>, area: Rect, state: &State, value: &Value) {
    let rows = model_rows(value);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No models available. Press r to refresh.").block(content_panel()),
            area,
        );
        return;
    }
    let rows = rows.into_iter().enumerate().map(|(index, row)| {
        Row::new(vec![
            format!(
                "{}{}",
                if index == state.model_selected {
                    "▶ "
                } else {
                    "  "
                },
                render::safe(row["id"].as_str().unwrap_or("Unknown"))
            ),
            render::safe(row["display_name"].as_str().unwrap_or("")),
            row["exetrouter"]["context_window"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(45),
            Constraint::Percentage(35),
            Constraint::Percentage(20),
        ],
    )
    .header(Row::new(["  MODEL ID", "NAME", "CONTEXT"]).style(section_style()))
    .row_highlight_style(selection_style())
    .block(content_panel());
    let mut selection = TableState::default().with_selected(Some(state.model_selected));
    frame.render_stateful_widget(table, area, &mut selection);
}
fn render(frame: &mut Frame<'_>, state: &State, connection: &Connection, busy: bool) {
    if frame.area().width < 72 || frame.area().height < 20 {
        frame.render_widget(
            Paragraph::new(
                "Terminal too small. Resize to at least 72 columns × 20 rows. Ctrl-C exits.",
            )
            .wrap(Wrap { trim: false }),
            frame.area(),
        );
        return;
    }
    let notice_lines = status_lines(state, busy, frame.area().width);
    let chunks = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Min(3),
        Constraint::Length(CONTROLS_HEIGHT + notice_lines.len().saturating_sub(1) as u16),
    ])
    .split(frame.area());
    header(frame, chunks[0], state, connection);
    let content = chunks[1];
    match (state.tab, &state.values[state.tab]) {
        (SETTINGS, _) => frame.render_widget(
            Paragraph::new(settings_lines(
                connection,
                state,
                content.width.saturating_sub(2),
            ))
            .wrap(Wrap { trim: false })
            .scroll((state.scroll, 0))
            .block(content_panel()),
            content,
        ),
        (TOKENS, _) => frame.render_widget(
            Paragraph::new(token_lines(state, content.width.saturating_sub(2)))
                .wrap(Wrap { trim: false })
                .scroll((state.scroll, 0))
                .block(content_panel()),
            content,
        ),
        (USAGE, Some(value)) => usage_chart(frame, content, state, value),
        (OVERVIEW, Some(value)) => overview(frame, content, state, value),
        (MODELS, Some(value)) => models(frame, content, state, value),
        _ => frame.render_widget(
            Paragraph::new(styled_text(&body(state)))
                .wrap(Wrap { trim: false })
                .scroll((state.scroll, 0))
                .block(content_panel().padding(Padding::new(2, 2, 1, 0))),
            content,
        ),
    }
    let mut footer: Vec<Line<'_>> = notice_lines
        .into_iter()
        .map(|line| {
            Line::styled(
                line,
                Style::default().fg(if busy { Color::Yellow } else { Color::Gray }),
            )
        })
        .collect();
    footer.push(keys(&[
        ("← / →", "view"),
        ("r", "refresh"),
        ("?", "help"),
        ("q", "quit"),
    ]));
    footer.push(match state.tab {
        TOKENS => keys(&[("↑ / ↓", "select"), ("Enter", "actions")]),
        OVERVIEW => keys(&[
            ("↑ / ↓", "select"),
            ("Enter", "account actions"),
            ("L", "personal label"),
            ("PgUp / PgDn", "page"),
        ]),
        USAGE if state.group != 0 => keys(&[
            ("p", "period"),
            ("b", "group"),
            ("m", "requests / tokens"),
            ("↑/↓", "select"),
        ]),
        USAGE => keys(&[("p", "period"), ("b", "group"), ("m", "requests / tokens")]),
        MODELS => keys(&[
            ("↑ / ↓", "select"),
            ("Enter", "copy model ID"),
            ("PgUp / PgDn", "page"),
        ]),
        SETTINGS if connection.local.is_some() => keys(&[
            ("↑ / ↓", "select"),
            ("Enter", "actions"),
            ("N", "notes"),
            ("P", "projects"),
        ]),
        SETTINGS => keys(&[
            ("Enter", "settings actions"),
            ("N", "notes"),
            ("P", "projects"),
        ]),
        _ => keys(&[("↑ / ↓", "scroll"), ("PgUp / PgDn", "page")]),
    });

    if let Some(Modal::Routing { locked, .. }) = &state.modal {
        footer = vec![keys(if *locked {
            &[("Esc / Enter", "close")]
        } else {
            &[
                ("Tab / ↑ / ↓", "field"),
                ("Enter", "save"),
                ("Esc", "cancel"),
            ]
        })];
    }
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().padding(Padding::horizontal(2))),
        chunks[2],
    );
    if let Some(modal) = &state.modal {
        let (title,text) = match modal {
            Modal::Actions { title, entries, selected } => (title.as_str(), format!("{}\n\n↑ / ↓: select   Enter: choose   Esc: close",entries.iter().enumerate().map(|(i,(name,_))|format!("{}{}",if i==*selected {"▶ "}else{"  "},name)).collect::<Vec<_>>().join("\n"))),
            Modal::Error { title, message } => (*title, format!("{message}\n\nEnter / Esc: close")),
            Modal::Routing { fields, selected, error, summary, locked, .. } => {
                let labels = ["Priority (-255..255 / default)", "All windows (0..100 / off / default)", "Short window (0..100 / off / default)", "Weekly window (0..100 / off / default)"];
                let form = labels.iter().zip(fields).enumerate().map(|(i,(label,field))|format!("{}{}: {}",if i == *selected { "▶ " } else { "  " },label,field.value)).collect::<Vec<_>>().join("\n");
                ("Switching rules", if *locked { format!("{summary}\n\nLocked by server operator") } else { format!("{form}\n\n{summary}\n\nDefault inherits operator settings. Thresholds are soft.\n{error}") })
            },
            Modal::UpdateConfirm => ("Update exr", "Install the latest published release in the existing installation?\nThe installation method is preserved; config, accounts and tokens stay in place.\nSource builds may take several minutes. Restart exr after completion.\n\nEnter: update   Esc: cancel.".into()),
            Modal::Name(text) => ("Create token",format!("Token name: {text}\n\nEnter: next   Esc: cancel")),
            Modal::Personal {kind,input,..} => (match kind {super::privacy::Kind::Account=>"Account label",super::privacy::Kind::Note=>"Personal note",super::privacy::Kind::Project=>"Project names",super::privacy::Kind::Settings=>"Preferences"},format!("{}\n\nEnter: save   Esc: cancel",input.value)),
            Modal::Days { name,text } => ("Create token",format!("Name: {name}\nExpires in days (1–365): {text}\n\nEnter: create   Esc: cancel")),
            Modal::Confirm { id,rotate } => (if *rotate {"Rotate token"}else{"Revoke token"}, format!("Token: {id}\n{}\n\nPress y to confirm, Esc to cancel.",if *rotate {"The old secret will stop working. The new secret will be copied to the clipboard."}else{"This token will stop working immediately."})),
            Modal::ClipboardRetry { .. } => ("Clipboard unavailable", "The token was issued, but copying failed.\nThe secret stays hidden. No token request will be repeated.\n\nc: retry copying   Esc: discard and rotate the token later".into()),
            Modal::ResetConfirm(value) => ("Use reset credit?",format!("{}\n\ny: use one credit   Esc / n: cancel",render::reset_confirmation(value))),
            Modal::Help => ("Keyboard help", keyboard_help(state, connection)),
        };
        let mut area = popup(frame.area());
        if matches!(modal, Modal::Routing { .. }) {
            area.height = area.height.min(chunks[2].y);
            area.y = area.y.min(chunks[2].y.saturating_sub(area.height));
        }
        frame.render_widget(Clear, area);
        let error = matches!(modal, Modal::Error { .. });
        let mut title = title_line(title);
        if error {
            title.spans[1].style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        }
        frame.render_widget(
            Paragraph::new(styled_text(&text))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title)
                        .border_style(Style::default().fg(if error {
                            Color::Red
                        } else {
                            Color::Cyan
                        })),
                ),
            area,
        );
        if let Modal::Routing {
            fields,
            selected,
            locked: false,
            ..
        } = modal
        {
            let labels = [
                "Priority (-255..255 / default)",
                "All windows (0..100 / off / default)",
                "Short window (0..100 / off / default)",
                "Weekly window (0..100 / off / default)",
            ];
            let field = &fields[*selected];
            let offset = Line::from(&field.value[..field.cursor]).width() as u16;
            let column = area.x + 1 + 4 + labels[*selected].len() as u16 + offset;
            frame.set_cursor_position((
                column.min(area.right().saturating_sub(2)),
                area.y + 1 + *selected as u16,
            ));
        }
    }
}

fn panel(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title_line(title))
}
fn title_line(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("─ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            title.to_owned(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ])
}
fn content_panel() -> Block<'static> {
    Block::default().padding(Padding::new(0, 2, 1, 0))
}
fn keys(items: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, label) in items {
        spans.push(Span::styled(
            format!("{key}: "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{label}   "),
            Style::default().fg(Color::Gray),
        ));
    }
    Line::from(spans)
}
fn styled_text(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| {
            let color = if line.starts_with("Free reset is less than 3 days away.") {
                Color::Rgb(245, 165, 75)
            } else if line.contains("reauth_required")
                || line.contains("expired")
                || line.contains("failed")
            {
                Color::Red
            } else if line.contains("stale")
                || line.contains("reset_elapsed")
                || line.contains("Cached")
            {
                Color::Yellow
            } else if line.trim_start().starts_with("▶")
                || line.contains("current")
                || line.contains("configured")
                || line.contains("Connected")
                || line.contains("Ready")
            {
                Color::Green
            } else if line.starts_with("  ")
                || line.starts_with("Only ")
                || line.starts_with("Refresh ")
            {
                Color::Gray
            } else {
                Color::White
            };
            Line::styled(
                line.to_owned(),
                if line.trim_start().starts_with("▶") {
                    selection_style()
                } else {
                    Style::default().fg(color)
                },
            )
        })
        .collect()
}
fn overview(frame: &mut Frame<'_>, area: Rect, state: &State, value: &Value) {
    let block = content_panel().padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let summary = render::response("doctor", value);
    let mut summary_lines = vec![section_heading("ROUTER STATUS", inner.width + 2)];
    summary_lines.extend(styled_text(&summary).into_iter().map(|mut line| {
        line.spans.insert(0, Span::raw("  "));
        line
    }));
    let summary_height = summary_lines.len() as u16 + 1;
    let visible = summary_height
        .saturating_sub(state.scroll)
        .min(inner.height);
    if visible > 0 {
        frame.render_widget(
            Paragraph::new(summary_lines).scroll((state.scroll, 0)),
            Rect::new(inner.x.saturating_sub(2), inner.y, inner.width + 2, visible),
        );
    }
    let mut y = inner.y as i32 + summary_height as i32 - state.scroll as i32;
    let end = (inner.y + inner.height) as i32;
    if y >= inner.y as i32 && y < end {
        frame.render_widget(
            Paragraph::new(section_heading("ACCOUNTS", inner.width + 2)),
            Rect::new(inner.x.saturating_sub(2), y as u16, inner.width + 2, 1),
        );
    }
    y += 1;
    let accounts = value["quota_accounts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if accounts.is_empty() {
        frame.render_widget(
            Paragraph::new(styled_text(&render::limits(value))).wrap(Wrap { trim: false }),
            Rect::new(
                inner.x,
                y.max(inner.y as i32).min(end) as u16,
                inner.width,
                (end - y.max(inner.y as i32)).max(0) as u16,
            ),
        );
        return;
    }
    for (index, account) in accounts.into_iter().enumerate() {
        let label = format!(
            "{}{} · Priority {}{}",
            render::safe(
                account
                    .get("display_name")
                    .unwrap_or(&account["label"])
                    .as_str()
                    .unwrap_or("Account")
            ),
            if account["preference"]["enabled"].as_bool() == Some(false) {
                " · Deactivated"
            } else {
                ""
            },
            account["preference"]["priority"]
                .as_i64()
                .map_or("unknown".into(), |n| n.to_string()),
            if account["preference"]["locked"].as_bool() == Some(true) {
                " · Locked"
            } else {
                ""
            }
        );
        let expiring = credit_expiring_soon(&account, chrono::Utc::now().timestamp());
        let label = if expiring {
            label
        } else {
            format!("{label} · {}", credit_summary(&account))
        };
        if y >= inner.y as i32 && y < end {
            frame.render_widget(
                Paragraph::new(format!(
                    "{}{}",
                    if index == state.account_selected {
                        "▶ "
                    } else {
                        "  "
                    },
                    label
                ))
                .style(if index == state.account_selected {
                    selection_style()
                } else {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                }),
                Rect::new(inner.x.saturating_sub(2), y as u16, inner.width + 2, 1),
            );
        }
        y += 1;
        for line in [
            expiring.then(|| credit_warning(&account)),
            render::weekly_activation(&account)
                .map(|text| Line::styled(text, Style::default().fg(Color::Gray))),
            account["refresh_error"]
                .as_str()
                .map(|text| Line::styled(render::safe(text), Style::default().fg(Color::Gray))),
        ]
        .into_iter()
        .flatten()
        {
            if y >= inner.y as i32 && y < end {
                frame.render_widget(
                    Paragraph::new(line),
                    Rect::new(inner.x, y as u16, inner.width, 1),
                );
            }
            y += 1;
        }
        let mut windows = render::visible_windows(&account["quota"]);
        windows.sort_by_key(|window| window["window_minutes"].as_i64().unwrap_or(i64::MAX));
        if windows.is_empty() {
            if y >= inner.y as i32 && y < end {
                frame.render_widget(
                    Paragraph::new("No subscription window reported")
                        .style(Style::default().fg(Color::DarkGray)),
                    Rect::new(inner.x, y as u16, inner.width, 1),
                );
            }
            y += 1;
        }
        for row in windows.chunks(quota_columns(inner.width)) {
            for (column, window) in row.iter().enumerate() {
                let gap = if row.len() == 2 { 2 } else { 0 };
                let left_width = (inner.width.saturating_sub(gap)) / row.len() as u16;
                let x = inner.x + column as u16 * (left_width + gap);
                let width = if column + 1 == row.len() {
                    inner.x + inner.width - x
                } else {
                    left_width
                };
                let remaining = window["remaining_percent"].as_f64();
                let now = chrono::Utc::now().timestamp();
                let stale = window["status"] != "current"
                    || window["observed_at"]
                        .as_i64()
                        .is_some_and(|at| at > now || now - at >= 60);
                let forecast = account["id"]
                    .as_i64()
                    .and_then(|id| state.forecasts.label(id, window, now));
                if y >= inner.y as i32 && y < end {
                    let label = format!(
                        "{}  ·  {}{}{}",
                        match window["window_minutes"].as_i64() {
                            Some(10080) => "week".into(),
                            Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
                            Some(minutes) => format!("{minutes}m"),
                            None => render::window_label(window),
                        },
                        render::percent(&window["remaining_percent"]),
                        forecast
                            .map(|text| format!("  ·  {text}"))
                            .unwrap_or_default(),
                        if stale { "  ·  historical" } else { "" }
                    );
                    quota_bar(frame, Rect::new(x, y as u16, width, 1), remaining, &label);
                }
                let reset_y = y + 1;
                if reset_y >= inner.y as i32 && reset_y < end {
                    let date = render::date(&window["reset_at"]);
                    let date = if date.chars().count() + 7 > usize::from(width) {
                        date.split_once(", ")
                            .map_or(date.as_str(), |(_, date)| date)
                    } else {
                        &date
                    };
                    frame.render_widget(
                        Paragraph::new(format!("Resets {date}"))
                            .alignment(Alignment::Center)
                            .style(Style::default().fg(Color::Gray)),
                        Rect::new(x, reset_y as u16, width, 1),
                    );
                }
            }
            y += 2;
        }
        if !account["quota"]["cooldown_until"].is_null() {
            if y >= inner.y as i32 && y < end {
                frame.render_widget(
                    Paragraph::new(format!(
                        "Paused until {}",
                        render::date(&account["quota"]["cooldown_until"])
                    ))
                    .style(Style::default().fg(Color::Red)),
                    Rect::new(inner.x, y as u16, inner.width, 1),
                );
            }
            y += 1;
        }
        y += 1;
    }
}
fn quota_palette(remaining: Option<f64>) -> (Color, Color) {
    match remaining {
        Some(p) if p <= 20. => (Color::Rgb(223, 104, 112), Color::Rgb(80, 15, 24)),
        Some(p) if p <= 50. => (Color::Rgb(232, 191, 82), Color::Rgb(90, 62, 0)),
        Some(_) => (Color::Rgb(93, 195, 143), Color::Rgb(14, 66, 40)),
        None => (Color::Rgb(52, 59, 70), Color::Rgb(174, 184, 200)),
    }
}
fn quota_bar(frame: &mut Frame<'_>, area: Rect, remaining: Option<f64>, label: &str) {
    let filled =
        (area.width as f64 * remaining.unwrap_or(0.).clamp(0., 100.) / 100.).round() as u16;
    let (background, foreground) = quota_palette(remaining);
    let empty_bg = Color::Rgb(52, 59, 70);
    let empty_fg = Color::Rgb(174, 184, 200);
    let chars = label.chars().take(area.width as usize).collect::<Vec<_>>();
    let start = (area.width as usize - chars.len()) / 2;
    let buffer = frame.buffer_mut();
    for x in 0..area.width {
        let style = if x < filled {
            Style::default().bg(background).fg(foreground)
        } else {
            Style::default().bg(empty_bg).fg(empty_fg)
        };
        let character = (x as usize)
            .checked_sub(start)
            .and_then(|i| chars.get(i))
            .copied()
            .unwrap_or(' ');
        buffer[(area.x + x, area.y)]
            .set_char(character)
            .set_style(style);
    }
}
fn usage_axis_max(value: f64) -> f64 {
    let magnitude = 10_f64.powf(value.max(1.).log10().floor());
    let normalized = value / magnitude;
    let rounded = [1., 2., 5., 10.]
        .into_iter()
        .find(|step| *step >= normalized)
        .unwrap_or(10.);
    rounded * magnitude
}

fn usage_chart(frame: &mut Frame<'_>, area: Rect, state: &State, value: &Value) {
    let block = content_panel().padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(inner);
    let rows = value["rows"].as_array().cloned().unwrap_or_default();
    let rows = if state.group != 0 && !rows.is_empty() {
        vec![rows[state.usage_selected % rows.len()].clone()]
    } else {
        rows
    };
    let requests: i64 = rows.iter().filter_map(|r| r["requests"].as_i64()).sum();
    let tokens: i64 = rows
        .iter()
        .flat_map(|r| [r["input_tokens"].as_i64(), r["output_tokens"].as_i64()])
        .flatten()
        .sum();
    let unknown: i64 = rows
        .iter()
        .filter_map(|r| r["unknown_usage"].as_i64())
        .sum();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("{requests} requests"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("   ·   "),
                Span::styled(
                    format!("{tokens} reported tokens"),
                    Style::default().fg(Color::Magenta),
                ),
            ]),
            Line::styled(
                format!(
                    "{} → {}",
                    render::date(&value["from_utc"]),
                    render::date(&value["to_utc"])
                ),
                Style::default().fg(Color::Gray),
            ),
        ]),
        chunks[0],
    );
    let timeline = value["timeline"].as_array().cloned().unwrap_or_default();
    if timeline.is_empty() {
        frame.render_widget(
            Paragraph::new("No timeline available from this server.").block(panel("Activity")),
            chunks[1],
        );
        return;
    }
    let mut names = rows
        .iter()
        .filter_map(|r| r["name"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if names.is_empty() && state.group == 0 {
        names.push("all".into());
    }
    let series = names
        .iter()
        .map(|name| {
            timeline
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    b["from_utc"]
                        .as_i64()
                        .is_some_and(|at| at <= chrono::Utc::now().timestamp())
                })
                .map(|(i, b)| {
                    let count = b["rows"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|r| r["name"].as_str() == Some(name))
                        .map(|r| {
                            if !state.request_metric {
                                r["input_tokens"].as_i64().unwrap_or(0)
                                    + r["output_tokens"].as_i64().unwrap_or(0)
                            } else {
                                r["requests"].as_i64().unwrap_or(0)
                            }
                        })
                        .sum::<i64>();
                    (i as f64, count as f64)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let max = usage_axis_max(series.iter().flatten().map(|(_, v)| *v).fold(1., f64::max));
    let palette = [
        Color::Cyan,
        Color::Magenta,
        Color::Green,
        Color::Yellow,
        Color::Blue,
    ];
    let datasets = names
        .iter()
        .zip(&series)
        .enumerate()
        .map(|(i, (name, data))| {
            Dataset::default()
                .name(format!("  {}  ", render::safe(name)))
                .marker(Marker::HalfBlock)
                .graph_type(GraphType::Line)
                .style(
                    Style::default()
                        .fg(palette[i % palette.len()])
                        .add_modifier(Modifier::BOLD),
                )
                .data(data)
        })
        .collect::<Vec<_>>();
    let last = timeline.len().saturating_sub(1);
    let labels = [0, last / 2, last]
        .into_iter()
        .map(|i| {
            let label = timeline[i]["from_utc"]
                .as_i64()
                .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format(match state.period {
                            0 => "%H:%M",
                            1 => "%b %-d %H:%M",
                            _ => "%b %-d",
                        })
                        .to_string()
                })
                .unwrap_or_default();
            Line::from(label)
        })
        .collect::<Vec<_>>();
    let metric = if !state.request_metric {
        "Reported tokens"
    } else {
        "Requests"
    };
    let period = USAGE_PERIODS[state.period].1;
    let grouping = ["total", "by user", "by model", "by token"][state.group];
    frame.render_widget(
        Chart::new(datasets)
            .hidden_legend_constraints((Constraint::Percentage(100), Constraint::Percentage(100)))
            .block(panel(&format!("{metric} · {period} · {grouping}")))
            .x_axis(
                Axis::default()
                    .bounds([0., last.max(1) as f64])
                    .labels(labels)
                    .style(Style::default().fg(Color::Gray)),
            )
            .y_axis(
                Axis::default()
                    .bounds([0., max])
                    .labels([
                        Line::from("0"),
                        Line::from(format!("{}", max / 2.)),
                        Line::from(format!("{max:.0}")),
                    ])
                    .style(Style::default().fg(Color::Gray)),
            ),
        chunks[1],
    );
    let note = if unknown > 0 {
        format!("{unknown} requests have incomplete counts. The token graph shows reported counts only.")
    } else {
        "Cached input and reasoning output are already included in token totals.".into()
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{} · {}\n{note}",
            render::system_timezone(),
            if state.period <= 1 {
                "Hourly activity"
            } else {
                "Daily activity"
            }
        ))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(if unknown > 0 {
            Color::Yellow
        } else {
            Color::Gray
        })),
        chunks[2],
    );
}
fn keyboard_help(state: &State, connection: &Connection) -> String {
    let actions = match state.tab {
        OVERVIEW => "↑/↓: select account\nPgUp/PgDn: page\nEnter: account actions\nc: review reset credit (≤5% remaining)\nP: set numeric priority; S: switching rules; D: toggle activation\nOperator locks apply; reset credits require confirmation.",
        USAGE if state.group == 0 => "p: change period\nb: change grouping\nm: switch tokens / requests",
        USAGE => "↑/↓: select user, model or token with recorded usage\np: change period\nb: change grouping\nm: switch tokens / requests",
        TOKENS => "↑/↓: select token\nEnter: token actions\nn: create; o: rotate; x: revoke\nRotation and revocation require confirmation.",
        MODELS => "↑/↓: select model\nPgUp/PgDn: page\nEnter: copy selected model ID",
        SETTINGS if connection.local.is_some() => "↑/↓: select account\nEnter: settings actions (including update)\ne: configure connection; v: check updates\na: add account; u: reauthorize; d: disable",
        _ => "Enter: settings actions (including update)\ne: configure connection\nv: check updates",
    };
    format!("{}\n\n←/→: switch views\nr: refresh this view\nq / Ctrl-C: quit\n\n{actions}\n\nEnter/Esc: close help", TABS[state.tab])
}

fn popup(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(96);
    let height = area.height.saturating_sub(4).min(18);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
pub(super) async fn run(original: &Session, config_path: &std::path::Path) -> Result<()> {
    let mut args = Session {
        connection: original.connection.clone(),
        json: false,
        command: super::CommandLine::Tui,
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("dashboard requires an interactive terminal; use exr doctor, tokens, models, usage or limits (add --json for scripts)".into());
    }
    if std::env::var("TERM").is_ok_and(|v| v == "dumb") {
        return Err("dashboard requires a terminal with cursor control".into());
    }
    enable_raw_mode()?;
    let _screen = Screen;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let result = async {
    let mut state = State::default();
    let mut private_settings_available = true;
    match super::privacy::dashboard(&args).await {
        Ok(Some(settings)) => { state.tab=settings.tab; state.period=settings.period; state.group=settings.group; state.request_metric=settings.request_metric; },
        Ok(None) => {},
        Err(_) => {private_settings_available=false; state.statuses[SETTINGS]="Private settings unavailable; restore the original privacy key and check server compatibility.".into();},
    }
    if private_settings_available {
        match (
            super::privacy::get(&args, super::privacy::Kind::Note, "personal").await,
            super::privacy::get(&args, super::privacy::Kind::Project, "personal").await,
        ) {
            (Ok(note), Ok(project)) => state.personal = [note.unwrap_or_default(), project.unwrap_or_default()],
            _ => private_settings_available = false,
        }
    }
    let mut next_limits_refresh = Instant::now() + LIMIT_REFRESH_INTERVAL;
    let mut pending = Some(start(&args.connection, request(&state), state.tab, false));
    let mut updater: Option<UpdateJob> = None;
    loop {
        if !state.update_checked && std::env::var_os("EXR_NO_UPDATE_CHECK").is_none() {
            state.update_checked = true; updater = Some(update_job(false));
        }
        if updater.as_ref().is_some_and(|job| job.task.is_finished()) {
            let mut job = updater.take().expect("update task");
            match (&mut job.task).await? {
                Ok(mut report) => {
                    if job.installing { state.notices[SETTINGS] = Some(finish_update(&mut report)); }
                    state.statuses[SETTINGS] = if report.available {
                        format!("Client update {} available.", report.latest)
                    } else {
                        format!("exr {} is up to date.", report.current)
                    };
                    state.update = Some(report);
                }
                Err(error) => { if job.installing { state.notices[SETTINGS] = None; } state.statuses[SETTINGS] = format!("Update: {error}"); },
            }
        }
        if pending.as_ref().is_some_and(|p| p.task.is_finished()) {
            let mut job = pending.take().expect("pending task");
            let result = (&mut job.task).await?;
            if job.tab == OVERVIEW {
                next_limits_refresh = Instant::now() + LIMIT_REFRESH_INTERVAL;
            }
            match result {
                Ok(_value) if job.mutation && job.tab == OVERVIEW && !job.reset_confirm => {
                    state.notices[OVERVIEW]=Some("Account preferences saved.".into());
                    pending=Some(start(&args.connection,ControlRequest::Limits,OVERVIEW,false));
                }
                Ok(value) if job.reset_prepare => {
                    state.modal = Some(Modal::ResetConfirm(value));
                    state.statuses[job.tab] =
                        "Live limit checked. A credit will only be used after confirmation.".into();
                }
                Ok(value) if job.reset_confirm => {
                    state.notices[job.tab] = Some(render::reset_outcome(&value));
                    state.values[OVERVIEW] = None;
                    pending = Some(start(
                        &args.connection,
                        ControlRequest::Limits,
                        OVERVIEW,
                        false,
                    ));
                }
                Ok(mut value) if job.mutation => {
                    if let Some(Value::String(secret)) = value.get_mut("secret").map(Value::take) {
                        if let Some(clipboard) = state.clipboard.take() {
                            match clipboard.copy(&secret).await {
                                Ok(()) => state.notices[job.tab] = Some("Secret copied to clipboard.".into()),
                                Err(_) => {
                                    state.modal = Some(Modal::ClipboardRetry { secret, clipboard })
                                }
                            }
                        } else {
                            return Err("token issued without a clipboard destination; rotate it before use".into());
                        }
                    } else if value.get("updated").and_then(Value::as_bool) == Some(true) {
                        state.notices[job.tab] = Some("Personal data saved.".into());
                    } else {
                        state.notices[job.tab] = Some(render::response("revoke", &value));
                    }
                    let refresh_tab=job.tab;
                    state.values[refresh_tab] = None;
                    if state.modal.is_none() {
                        let request=if matches!(value.get("updated").and_then(Value::as_bool),Some(true)){request(&state)}else{ControlRequest::TokenList};
                        pending = Some(start(&args.connection,request,refresh_tab,false));
                    }
                }
                Ok(mut value) => {
                    if job.tab == SETTINGS {
                        if let Some(personal) = value.get("personal") {
                            if personal.is_null() {
                                state.notices[SETTINGS] = Some("Personal data could not be refreshed; displayed values may be stale.".into());
                            } else {
                                state.personal = serde_json::from_value(personal.clone())?;
                            }
                            value = value["accounts"].take();
                        }
                    }
                    if job.tab == OVERVIEW {
                        state.forecasts.observe(&value, chrono::Utc::now().timestamp());
                    }
                    preserve_selection(&mut state, job.tab, &value);
                    if job.tab == TOKENS || job.tab == SETTINGS {
                        state.selected = state
                            .selected
                            .min(value.as_array().map_or(0, |v| v.len().saturating_sub(1)));
                    }
                    state.values[job.tab] = Some(value);
                    if job.tab == OVERVIEW && state.tab == OVERVIEW {
                        let size = terminal.size()?;
                        let viewport = overview_viewport(size.height, size.width, &state, false);
                        keep_selected_account_visible(&mut state, viewport, size.width.saturating_sub(4));
                    }
                    state.statuses[job.tab] = format!(
                        "Updated {} · {}",
                        TABS[job.tab],
                        chrono::Local::now().format("%H:%M:%S")
                    );
                }
                Err(error) if job.reset_prepare || job.reset_confirm => {
                    let error = render::safe(&error.to_string());
                    let message = if job.reset_confirm {
                        format!("{error}\n\nCheck subscription limits before trying again; the request may have completed.")
                    } else {
                        error.to_string()
                    };
                    state.modal = Some(Modal::Error {
                        title: if job.reset_confirm { "Reset credit failed" } else { "Reset credit unavailable" },
                        message,
                    });
                }
                Err(error) => {
                    state.statuses[job.tab] = format!(
                        "{}{}",
                        if job.mutation {
                            "Request failed; check tokens or limits before retrying. "
                        } else {
                            ""
                        },
                        error
                    );
                    if job.mutation && job.tab == SETTINGS {
                        state.notices[SETTINGS] = Some(state.statuses[SETTINGS].clone());
                        pending = Some(start(&args.connection, ControlRequest::Doctor, SETTINGS, false));
                    }
                }
            }
        }
        if state.tab == OVERVIEW
            && pending.is_none()
            && state.modal.is_none()
            && Instant::now() >= next_limits_refresh
        {
            pending = Some(start(
                &args.connection,
                ControlRequest::Limits,
                OVERVIEW,
                false,
            ));
            next_limits_refresh = Instant::now() + LIMIT_REFRESH_INTERVAL;
        }
        terminal.draw(|frame| render(frame, &state, &args.connection, pending.as_ref().is_some_and(|job| job.tab == state.tab) || (state.tab == SETTINGS && updater.is_some())))?;
        if !event::poll(Duration::from_millis(50))? {
            tokio::task::yield_now().await;
            continue;
        }
        let Event::Key(mut key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if updater.as_ref().is_some_and(|job| job.installing) { continue; }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if pending.as_ref().is_some_and(|p| p.mutation) {
                return Err("Mutation interrupted; it may have completed. Inspect tokens or limits before any further action".into());
            }
            if private_settings_available {super::privacy::save_dashboard(&args, &super::privacy::Dashboard {tab:state.tab,period:state.period,group:state.group,request_metric:state.request_metric}).await?;}
            return Ok(());
        }
        let size = terminal.size()?;
        let viewport = overview_viewport(size.height, size.width, &state, pending.as_ref().is_some_and(|job| job.tab == state.tab));
        if size.width < 72 || size.height < 20 {
            continue;
        }
        if pending.as_ref().is_some_and(|p| p.mutation) {
            continue;
        }
        if pending.is_some()
            && !matches!(
                key.code,
                KeyCode::Char('q')
                    | KeyCode::Esc
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Char('?')
            )
            && !(state.tab == SETTINGS && matches!(key.code, KeyCode::Enter | KeyCode::Char('e' | 'v')))
        {
            continue;
        }
        let mut menu_action=false;
        if let Some(Modal::Actions { entries, selected, .. }) = &mut state.modal {
            match key.code {
                KeyCode::Up => { *selected=selected.saturating_sub(1);continue; }
                KeyCode::Down => { *selected=(*selected+1).min(entries.len().saturating_sub(1));continue; }
                KeyCode::Esc => { state.modal=None;continue; }
                KeyCode::Enter => { key.code=entries[*selected].1;state.modal=None;menu_action=true; }
                _ => continue,
            }
        }
        let mut mutation = None;
        if let Some(modal) = &mut state.modal {
            match modal {
                Modal::Actions { .. } => unreachable!("action menus handled above"),
                Modal::Error { .. } => {
                    if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                        state.modal = None;
                    }
                }
                Modal::Routing { account, fields, selected, error, locked, .. } => {
                    if *locked {
                        if matches!(key.code,KeyCode::Esc|KeyCode::Enter) { state.modal = None; }
                    } else {
                        match key.code {
                            KeyCode::Esc => state.modal = None,
                            KeyCode::Tab | KeyCode::Down => *selected = (*selected + 1) % fields.len(),
                            KeyCode::BackTab | KeyCode::Up => *selected = (*selected + fields.len() - 1) % fields.len(),
                            KeyCode::Enter => match routing_form(fields) {
                                Ok(routing) => { mutation = Some(ControlRequest::AccountSet { account:*account, enabled:None, priority:None, routing:Some(routing) }); state.modal = None; },
                                Err(message) => *error = message.to_string(),
                            },
                            _ => {
                                if !matches!(key.code,KeyCode::Char(_)) || fields[*selected].value.len() < 16 || key.modifiers.contains(KeyModifiers::CONTROL) { fields[*selected].edit(key); }
                                error.clear();
                            },
                        }
                    }
                },
                Modal::UpdateConfirm => match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => state.modal = None,
                    KeyCode::Enter | KeyCode::Char('y') => {
                        updater = Some(update_job(true)); state.update_checked = true;
                        state.modal = None; state.statuses[state.tab] = "Installing update… Keep this dashboard open until completion.".into();
                        state.notices[state.tab] = Some("Updating exr; source builds may take several minutes.".into());
                    }
                    _ => {}
                },
                Modal::ResetConfirm(value) => match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => state.modal = None,
                    KeyCode::Char('y') => {
                        if let Some(confirmation) = value["confirmation"].as_str() {
                            mutation = Some(ControlRequest::ResetConfirm {
                                confirmation: confirmation.into(),
                            });
                            state.modal = None;
                        }
                    }
                    _ => {}
                },
                Modal::ClipboardRetry { secret, clipboard } => match key.code {
                    KeyCode::Char('c') => {
                        if clipboard.copy(secret).await.is_ok() {
                            state.modal = None;
                            state.notices[state.tab] = Some("Secret copied to clipboard.".into());
                            pending = Some(start(
                                &args.connection,
                                ControlRequest::TokenList,
                                TOKENS,
                                false,
                            ));
                        } else {
                            state.statuses[state.tab] = "Clipboard copy failed. Retry copying with c.".into();
                        }
                    }
                    KeyCode::Esc => {
                        state.modal = None;
                        state.notices[state.tab] = Some(
                            "Secret discarded. Rotate the issued token before using it.".into(),
                        );
                        pending = Some(start(
                            &args.connection,
                            ControlRequest::TokenList,
                            TOKENS,
                            false,
                        ));
                    }
                    _ => {}
                },
                Modal::Help => {
                    if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                        state.modal = None;
                    }
                }
                Modal::Name(text) => match key.code {
                    KeyCode::Esc => state.modal = None,
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char(c) if !c.is_control() && text.len() < 128 => text.push(c),
                    KeyCode::Enter if !text.trim().is_empty() => {
                        state.modal = Some(Modal::Days {
                            name: text.trim().into(),
                            text: "90".into(),
                        })
                    }
                    _ => {}
                },
                Modal::Personal {kind,id,input} => match key.code {
                    KeyCode::Esc => state.modal=None,
                    KeyCode::Enter => {
                        match super::privacy::seal(&args.connection,*kind,id,&input.value) {
                            Ok(request)=>{mutation=Some(request);},
                            Err(error)=>state.statuses[state.tab]=error.to_string(),
                        }
                        state.modal=None;
                    }
                    _=>input.edit(key),
                },
                Modal::Days { name, text } => match key.code {
                    KeyCode::Esc => state.modal = None,
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() && text.len() < 4 => text.push(c),
                    KeyCode::Enter => {
                        if let Ok(days) = text.parse::<i64>() {
                            if (1..=365).contains(&days) {
                                mutation = Some(ControlRequest::TokenCreate {
                                    name: name.clone(),
                                    expires_days: Some(days),
                                });
                                state.modal = None;
                            } else {
                                state.statuses[state.tab] = "Expiry must be 1–365 days.".into();
                            }
                        } else {
                            state.statuses[state.tab] = "Enter an expiry in days.".into();
                        }
                    }
                    _ => {}
                },
                Modal::Confirm { id, rotate } => match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => state.modal = None,
                    KeyCode::Char('y') => {
                        mutation = Some(if *rotate {
                            ControlRequest::TokenRotate { id: id.clone() }
                        } else {
                            ControlRequest::TokenRevoke { id: id.clone() }
                        });
                        state.modal = None;
                    }
                    _ => {}
                },
            }
        } else {
            let old_tab = state.tab;
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if private_settings_available {super::privacy::save_dashboard(&args, &super::privacy::Dashboard {tab:state.tab,period:state.period,group:state.group,request_metric:state.request_metric}).await?;}
                    if let Some(local) = &args.connection.local {
                        local.stop().await?;
                    }
                    return Ok(());
                }
                KeyCode::Char('?') => state.modal = Some(Modal::Help),
                KeyCode::Right => state.tab = (state.tab + 1) % TABS.len(),
                KeyCode::Left => state.tab = (state.tab + TABS.len() - 1) % TABS.len(),
                KeyCode::Char('v') if state.tab == SETTINGS && updater.is_none() => {
                    updater = Some(update_job(false)); state.update_checked = true;
                    state.statuses[state.tab] = "Checking GitHub releases…".into();
                }
                KeyCode::Char('U') if menu_action && state.tab == SETTINGS && updater.is_none() => { state.modal = Some(Modal::UpdateConfirm); }
                KeyCode::Char('e') if state.tab == SETTINGS => {
                    pending = None;
                    let mut changed = args.connection.clone();
                    changed.local=None;
                    match settings_wizard(&mut terminal, &mut changed, config_path, false).await {
                        Ok(()) => {
                            if let Some(local) = &args.connection.local {
                                local.stop().await?;
                            }
                            changed.local = None;
                            if changed.mode == super::config::Mode::Standalone {
                                match crate::local::Local::open(
                                    std::path::Path::new(&changed.state_dir),
                                    changed.listen,
                                    true,
                                )
                                .await
                                {
                                    Ok(local) => changed.local = Some(local),
                                    Err(error) => {
                                        state.statuses[state.tab] = format!("Settings unchanged: {error}");
                                        if args.connection.mode == super::config::Mode::Standalone {
                                            args.connection.local = Some(
                                                crate::local::Local::open(
                                                    std::path::Path::new(
                                                        &args.connection.state_dir,
                                                    ),
                                                    args.connection.listen,
                                                    true,
                                                )
                                                .await?,
                                            );
                                        }
                                        continue;
                                    }
                                }
                            }
                            super::config::save(config_path, &changed)?;
                            args.connection = changed;
                            state.values = Default::default();
                            state.statuses = Default::default();
                            state.notices = Default::default();
                            state.forecasts = Default::default();
                            state.statuses[state.tab] = "Settings saved.".into();
                            pending =
                                Some(start(&args.connection, request(&state), SETTINGS, false));
                        }
                        Err(error) if error.is::<super::SetupInterrupted>() => return Ok(()),
                        Err(error) => state.statuses[state.tab] = error.to_string(),
                    }
                    reset_screen(&mut terminal)?;
                }
                KeyCode::Char(c @ ('a' | 'u'))
                    if state.tab == SETTINGS && args.connection.local.is_some() =>
                {
                    let selected = state.values[SETTINGS]
                        .as_ref()
                        .and_then(Value::as_array)
                        .and_then(|rows| rows.get(state.selected))
                        .and_then(|row| row["id"].as_i64());
                    if c == 'u' && selected.is_none() {
                        state.statuses[state.tab] = "Select an account first.".into();
                        continue;
                    }
                    let local = args
                        .connection
                        .local
                        .as_ref()
                        .expect("local runtime")
                        .clone();
                    state.statuses[state.tab] = match login_dialog(
                        &mut terminal,
                        local,
                        if c == 'u' { selected } else { None },
                    )
                    .await
                    {
                        Ok(()) => "Account signed in.".into(),
                        Err(error) => error.to_string(),
                    };
                    reset_screen(&mut terminal)?;
                    state.values[OVERVIEW] = None;
                    state.values[MODELS] = None;
                    pending = Some(start(&args.connection, request(&state), SETTINGS, false));
                }
                KeyCode::Char('d') if state.tab == SETTINGS && args.connection.local.is_some() => {
                    if let Some(id) = state.values[SETTINGS]
                        .as_ref()
                        .and_then(Value::as_array)
                        .and_then(|rows| rows.get(state.selected))
                        .and_then(|row| row["id"].as_i64())
                    {
                        if confirm_dialog(&mut terminal,"Disable account?","New requests will stop using this account. Existing responses can finish. Reauthorize it to enable it again.")? {
                            args.connection.local.as_ref().expect("local").disable(id).await?;
                            state.values[OVERVIEW]=None;state.values[MODELS]=None;
                            pending=Some(start(&args.connection,request(&state),SETTINGS,false));
                        }
                        reset_screen(&mut terminal)?;
                    }
                }
                KeyCode::Down if state.tab == SETTINGS => {
                    let count = state.values[SETTINGS]
                        .as_ref()
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    state.selected = (state.selected + 1).min(count.saturating_sub(1));
                }
                KeyCode::Up if state.tab == SETTINGS => {
                    state.selected = state.selected.saturating_sub(1)
                }
                KeyCode::Char('r') => {
                    pending = Some(start(&args.connection, request(&state), state.tab, false))
                }
                KeyCode::Enter if matches!(state.tab,OVERVIEW|TOKENS|SETTINGS) => {
                    let (title, entries)=action_menu(&state,&args.connection);
                    if entries.is_empty() { state.modal=Some(Modal::Error{title:"Account unavailable",message:format!("{title}\n\nThis account is deactivated and locked by the server operator.")}); } else { state.modal=Some(Modal::Actions{title,entries,selected:0}); }
                }
                KeyCode::Char('S') if state.tab == OVERVIEW => {
                    if state.values[OVERVIEW].as_ref().is_none_or(|v| v["capabilities"]["account_routing_rules"] != 1) {
                        state.modal = Some(Modal::Error { title: "Server update required", message: "Account routing rules require an updated server and SSH gateway.".into() });
                    } else if let Some(row) = state.values[OVERVIEW].as_ref().and_then(|v|v["quota_accounts"].as_array()).and_then(|rows|rows.get(state.account_selected)) {
                        if let Some(account) = row["id"].as_i64() {
                            let preference = &row["preference"];
                            let names = ["priority","switch_at","switch_at_short","switch_at_weekly"];
                            let fields = names.iter().map(|name| TextInput::new(preference["settings"][*name].as_i64().map(|n|n.to_string()).or_else(||preference["settings"][*name].as_str().map(str::to_owned)).unwrap_or("default".into()))).collect();
                            state.modal = Some(Modal::Routing { account, fields, selected:0, error:String::new(), summary:format!("Effective priority: {}\n{}", preference["priority"],crate::account_preferences::describe(preference)), locked:preference["locked"] == true });
                        }
                    }
                }
                KeyCode::Char('D') if state.tab == OVERVIEW => {
                    if let Some(row)=state.values[OVERVIEW].as_ref().and_then(|v|v["quota_accounts"].as_array()).and_then(|r|r.get(state.account_selected)) {
                        if row["preference"]["locked"] == true {
                            state.modal = Some(Modal::Error {title:"Account settings locked",message:"Account settings are locked by the server operator.".into()});
                        } else if let Some(account)=row["id"].as_i64() {
                            mutation=Some(ControlRequest::AccountSet{account, enabled:Some(!row["preference"]["enabled"].as_bool().unwrap_or(true)), priority:None,routing:None});
                        }
                    }
                }
                KeyCode::Char('L') if state.tab == OVERVIEW => {
                    if let Some(row)=state.values[OVERVIEW].as_ref().and_then(|v|v["quota_accounts"].as_array()).and_then(|r|r.get(state.account_selected)) {
                        if let Some(id)=row["id"].as_i64() {
                            let value=row["display_name"].as_str().unwrap_or("").to_owned();
                            state.modal=Some(Modal::Personal {kind:super::privacy::Kind::Account,id:id.to_string(),input:TextInput::new(value)});
                        }
                    }
                }
                KeyCode::Char('N') if state.tab == SETTINGS => state.modal=Some(Modal::Personal {kind:super::privacy::Kind::Note,id:"personal".into(),input:TextInput::new(state.personal[0].clone())}),
                KeyCode::Char('P') if state.tab == SETTINGS => state.modal=Some(Modal::Personal {kind:super::privacy::Kind::Project,id:"personal".into(),input:TextInput::new(state.personal[1].clone())}),
                KeyCode::Char('c') if state.tab == OVERVIEW => {
                    if let Some(req) = selected_reset_request(&state) {
                        pending = Some(start(&args.connection, req, OVERVIEW, false));
                    } else {
                        state.notices[state.tab] = Some("No account selected. Refresh Overview to load accounts.".into());
                    }
                }
                KeyCode::Char('n') if state.tab == TOKENS => {
                    state.modal = Some(Modal::Name(String::new()))
                }
                KeyCode::Char(c @ ('o' | 'x')) if state.tab == TOKENS => {
                    if let Some(id) = state.values[TOKENS]
                        .as_ref()
                        .and_then(Value::as_array)
                        .and_then(|rows| rows.get(state.selected))
                        .and_then(|row| row["id"].as_str())
                    {
                        state.modal = Some(Modal::Confirm {
                            id: id.into(),
                            rotate: c == 'o',
                        });
                    }
                }
                KeyCode::Down if state.tab == TOKENS => {
                    let count = state.values[TOKENS]
                        .as_ref()
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    state.selected = (state.selected + 1).min(count.saturating_sub(1));
                    state.scroll = 0;
                }
                KeyCode::Up if state.tab == TOKENS => {
                    state.selected = state.selected.saturating_sub(1);
                    state.scroll = 0;
                }
                KeyCode::Down | KeyCode::Up if matches!(state.tab, OVERVIEW | MODELS) => {
                    select_item(&mut state, key.code == KeyCode::Down, viewport, size.width.saturating_sub(4));
                }
                KeyCode::Enter if state.tab == MODELS => {
                    if let Some(id) = selected_model_id(&state).map(str::to_owned) {
                        state.notices[state.tab] = Some(match Clipboard::detect() {
                            Ok(clipboard) => match clipboard.copy(&id).await {
                                Ok(()) => format!("Copied {id} to clipboard."),
                                Err(error) => format!("Could not copy model ID: {error}"),
                            },
                            Err(error) => format!("Could not copy model ID: {error}"),
                        });
                    }
                }
                KeyCode::Down | KeyCode::Up if state.tab == USAGE && state.group != 0 => {
                    let count = state.values[USAGE].as_ref().and_then(|v| v["rows"].as_array()).map_or(0, Vec::len);
                    if count > 0 {
                        state.usage_selected = if key.code == KeyCode::Down {
                            (state.usage_selected + 1) % count
                        } else {
                            (state.usage_selected + count - 1) % count
                        };
                    }
                }
                KeyCode::Down => state.scroll = state.scroll.saturating_add(1),
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::PageUp if state.tab == MODELS => {
                    for _ in 0..viewport.saturating_sub(1).max(1) {
                        select_item(&mut state, key.code == KeyCode::PageDown, viewport, size.width.saturating_sub(4));
                    }
                }
                KeyCode::PageDown if state.tab == OVERVIEW => {
                    let end = state.values[OVERVIEW].as_ref().and_then(|value| account_ranges(value, size.width.saturating_sub(4)).last().copied()).map_or(0, |(_, end)| end);
                    state.scroll = state.scroll.saturating_add(10).min(end.saturating_sub(viewport));
                }
                KeyCode::PageDown => state.scroll = state.scroll.saturating_add(10),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::Char('m') if state.tab == USAGE => {
                    state.request_metric = !state.request_metric
                }
                KeyCode::Char('p') if state.tab == USAGE => {
                    state.period = (state.period + 1) % USAGE_PERIODS.len();
                    state.usage_selected = 0;
                    state.values[USAGE] = None;
                    pending = Some(start(&args.connection, request(&state), state.tab, false));
                }
                KeyCode::Char('b') if state.tab == USAGE => {
                    state.group = (state.group + 1) % 4;
                    state.usage_selected = 0;
                    state.values[USAGE] = None;
                    pending = Some(start(&args.connection, request(&state), state.tab, false));
                }
                _ => {}
            }
            if state.tab != old_tab {
                pending = None;
                state.scroll = 0;
                if state.tab == OVERVIEW {
                    keep_selected_account_visible(&mut state, viewport, size.width.saturating_sub(4));
                }
                if state.tab == OVERVIEW || state.values[state.tab].is_none() {
                    pending = Some(start(&args.connection, request(&state), state.tab, false));
                }
            }
        }
        if let Some(req) = mutation {
            if matches!(
                req,
                ControlRequest::TokenCreate { .. } | ControlRequest::TokenRotate { .. }
            ) {
                match Clipboard::detect() {
                    Ok(clipboard) => state.clipboard = Some(clipboard),
                    Err(error) => {
                        state.statuses[state.tab] = format!("{error} Token was not issued.");
                        continue;
                    }
                }
            }
            state.notices[state.tab] = None;
            let tab = if matches!(req, ControlRequest::ResetConfirm { .. } | ControlRequest::AccountSet { .. }) {
                OVERVIEW
            } else if matches!(req,ControlRequest::PrivatePut {..}) {
                state.tab
            } else {
                TOKENS
            };
            pending = Some(start(&args.connection, req, tab, true));
        }
    }
    }.await;
    if let Some(local) = &args.connection.local {
        local.stop().await?;
    }
    result
}

fn settings_body(connection: &Connection, accounts: Option<&Value>, selected: usize) -> String {
    if connection.mode == super::config::Mode::Standalone {
        let address = connection
            .local
            .as_ref()
            .map_or(connection.listen, |local| local.address);
        let mut text=format!("Standalone\nAPI endpoint  http://{address}/v1\nPrivate state  {}\nAPI runs in exr or exr serve. Closing an attached dashboard keeps the headless API running.\nTokens: create and copy an API key in the Tokens view.\n\nLocal accounts\n",render::safe(&connection.state_dir));
        let rows = accounts.and_then(Value::as_array);
        if rows.is_none_or(Vec::is_empty) {
            text.push_str("No accounts. Press a to sign in.\n");
        }
        for (i, row) in rows.into_iter().flatten().enumerate() {
            text.push_str(&format!(
                "{}{} · {}\n",
                if i == selected { "▶ " } else { "  " },
                render::safe(
                    row.get("display_name")
                        .unwrap_or(&row["email"])
                        .as_str()
                        .unwrap_or("Email unavailable")
                ),
                render::safe(row["state"].as_str().unwrap_or("unknown"))
            ));
        }
        text
    } else {
        format!("Remote server\nHTTP API URL   {}\nSSH host       {}\nPort           {}\nSSH username   {}\nPrivate key    {}\n\nPress e to change connection or switch to standalone.\nRegister your public key with the operator and verify the SSH host key.\nUpstream accounts are managed by the server operator using exrd.\nNo private key or API secret is stored in the connection config.",render::safe(connection.api_url.as_deref().unwrap_or("Not configured")),render::safe(&connection.host),connection.port,render::safe(&connection.ssh_user),render::safe(&connection.identity))
    }
}
fn dialog(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    title: &str,
    text: &str,
) -> Result<()> {
    terminal.draw(|frame| {
        frame.render_widget(
            Paragraph::new(styled_text(text))
                .wrap(Wrap { trim: false })
                .block(panel(title)),
            frame.area(),
        )
    })?;
    Ok(())
}
fn confirm_dialog(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    title: &str,
    text: &str,
) -> Result<bool> {
    loop {
        dialog(
            terminal,
            title,
            &format!("{text}\n\ny: confirm   Esc / n: cancel"),
        )?;
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Err(super::SetupInterrupted.into());
            }
            match key.code {
                KeyCode::Char('y') => return Ok(true),
                KeyCode::Esc | KeyCode::Char('n') => return Ok(false),
                _ => {}
            }
        }
    }
}
pub(super) async fn setup(connection: &mut Connection, path: &std::path::Path) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "First-run setup requires an interactive terminal; use exr configure for scripts"
                .into(),
        );
    }
    enable_raw_mode()?;
    let _screen = Screen;
    execute!(io::stdout(), EnterAlternateScreen, crossterm::cursor::Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    reset_screen(&mut terminal)?;
    settings_wizard(&mut terminal, connection, path, true).await?;
    super::config::save(path, connection)
}
fn setup_cancelled(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn routing_form(fields: &[TextInput]) -> Result<crate::account_preferences::RoutingArgs> {
    use crate::account_preferences::{RoutingArgs, Setting};
    let parse =
        |i: usize| -> Result<Option<Setting>> { Ok(Some(fields[i].value.parse::<Setting>()?)) };
    let routing = RoutingArgs {
        priority: parse(0)?,
        switch_at: parse(1)?,
        switch_at_short: parse(2)?,
        switch_at_weekly: parse(3)?,
    };
    routing.validate()?;
    Ok(routing)
}

struct TextInput {
    value: String,
    cursor: usize,
}
impl TextInput {
    fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }
    fn edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            KeyCode::Left => {
                self.cursor = self.value[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            KeyCode::Right => {
                if let Some(c) = self.value[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Backspace => {
                if let Some((i, _)) = self.value[..self.cursor].char_indices().next_back() {
                    self.value.drain(i..self.cursor);
                    self.cursor = i;
                }
            }
            KeyCode::Delete => {
                if let Some(c) = self.value[self.cursor..].chars().next() {
                    self.value.drain(self.cursor..self.cursor + c.len_utf8());
                }
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => self.cursor = 0,
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.value.len()
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control()
                    && self.value.len() + c.len_utf8() <= 1024 =>
            {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
    }
    fn visible(&self, width: usize) -> (&str, u16) {
        let mut start = 0;
        while Line::from(&self.value[start..self.cursor]).width() >= width.max(1) {
            let Some(c) = self.value[start..self.cursor].chars().next() else {
                break;
            };
            start += c.len_utf8();
        }
        (
            &self.value[start..],
            Line::from(&self.value[start..self.cursor]).width() as u16,
        )
    }
}

fn connection_fields(
    frame: &mut Frame<'_>,
    fields: &[(&str, TextInput)],
    selected: usize,
    error: &str,
) {
    let area = frame.area();
    let block = panel("Connection settings");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    for (i, (label, input)) in fields.iter().enumerate() {
        let prefix = format!("{}{label}: ", if i == selected { "▶ " } else { "  " });
        let prefix_width = Line::from(prefix.as_str()).width() as u16;
        let (value, offset) = input.visible(inner.width.saturating_sub(prefix_width) as usize);
        let y = inner.y + i as u16;
        if y >= inner.bottom() {
            break;
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default().fg(if i == selected {
                        Color::Cyan
                    } else {
                        Color::Gray
                    }),
                ),
                Span::raw(render::safe(value)),
            ])),
            Rect::new(inner.x, y, inner.width, 1),
        );
        if i == selected && inner.width > prefix_width {
            frame.set_cursor_position((inner.x + prefix_width + offset, y));
        }
    }
    let y = inner.y + fields.len() as u16 + 1;
    if y < inner.bottom() {
        frame.render_widget(Paragraph::new(format!("{}\n\n←/→: cursor   Home/End: start/end   Backspace/Delete: erase\n↑/↓: field   Enter: review   Esc/Ctrl-C: cancel\nCtrl-U: clear field\nRemote: register your public key with the operator.\nNew SSH host keys are confirmed in this terminal before saving.", render::safe(error))).wrap(Wrap {trim: false}), Rect::new(inner.x,y,inner.width,inner.bottom()-y));
    }
}

async fn check_remote_connection(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    connection: &Connection,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    println!("Checking SSH connection. Verify any new host fingerprint with your operator before accepting it. Ctrl-C cancels setup.");
    let result = super::remote_request(connection, ControlRequest::Doctor, true).await;
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, crossterm::cursor::Hide)?;
    reset_screen(terminal)?;
    result.map(|_| ())
}

async fn settings_wizard(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    connection: &mut Connection,
    _path: &std::path::Path,
    first: bool,
) -> Result<()> {
    let mut selected = usize::from(!first && connection.mode == super::config::Mode::Remote);
    loop {
        dialog(terminal,if first {"Welcome to ExetRouter"}else{"Configure ExetRouter"},&format!("Choose how to use ExetRouter\n\n{}Standalone - manage your own accounts; local API, no SSH server\n{}Remote server - use an existing ExetRouter over restricted SSH\n\n↑ / ↓: select   Enter: continue   Esc: cancel",if selected==0 {"▶ "}else{"  "},if selected==1 {"▶ "}else{"  "}))?;
        if !event::poll(Duration::from_millis(50))? {
            tokio::task::yield_now().await;
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if setup_cancelled(key) {
                return Err(if key.code == KeyCode::Esc {
                    "Setup cancelled; no settings saved".into()
                } else {
                    super::SetupInterrupted.into()
                });
            }
            match key.code {
                KeyCode::Up | KeyCode::Down => selected = 1 - selected,
                KeyCode::Enter => break,
                _ => {}
            }
        }
    }
    connection.mode = if selected == 0 {
        super::config::Mode::Standalone
    } else {
        super::config::Mode::Remote
    };
    let mut fields = if selected == 0 {
        vec![
            ("Private state directory", connection.state_dir.clone()),
            ("Local API listen address", connection.listen.to_string()),
        ]
    } else {
        vec![
            ("SSH host", connection.host.clone()),
            ("SSH port", connection.port.to_string()),
            ("SSH username", connection.ssh_user.clone()),
            ("Private key path", connection.identity.clone()),
            (
                "HTTP API URL (optional)",
                connection.api_url.clone().unwrap_or_default(),
            ),
        ]
    };
    let mut fields = fields
        .drain(..)
        .map(|(label, value)| (label, TextInput::new(render::safe(&value))))
        .collect::<Vec<_>>();
    let mut field = 0usize;
    let mut error = String::new();
    loop {
        terminal.draw(|frame| connection_fields(frame, &fields, field, &error))?;
        if !event::poll(Duration::from_millis(50))? {
            tokio::task::yield_now().await;
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if setup_cancelled(key) {
            return Err(if key.code == KeyCode::Esc {
                "Setup cancelled; no settings saved".into()
            } else {
                super::SetupInterrupted.into()
            });
        }
        match key.code {
            KeyCode::Up => field = (field + fields.len() - 1) % fields.len(),
            KeyCode::Down => field = (field + 1) % fields.len(),
            KeyCode::Enter => {
                let validate = (|| -> Result<()> {
                    if selected == 0 {
                        connection.state_dir = super::config::expand(fields[0].1.value.clone())?;
                        connection.listen = fields[1].1.value.parse()?;
                    } else {
                        connection.host = fields[0].1.value.clone();
                        connection.port = fields[1].1.value.parse()?;
                        connection.ssh_user = fields[2].1.value.clone();
                        connection.identity = super::config::expand(fields[3].1.value.clone())?;
                        connection.api_url = (!fields[4].1.value.is_empty())
                            .then(|| super::config::normalize_api_url(&fields[4].1.value))
                            .transpose()?;
                        let meta = std::fs::metadata(&connection.identity)?;
                        use std::os::unix::fs::PermissionsExt;
                        if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                            return Err("Private key must be a regular file with mode 600".into());
                        }
                    }
                    connection.validate()
                })();
                match validate {
                    Err(e) => error = e.to_string(),
                    Ok(()) => {
                        let preview = settings_body(connection, None, 0);
                        if confirm_dialog(terminal, "Save these settings?", &preview)? {
                            if selected == 1 {
                                match check_remote_connection(terminal, connection).await {
                                    Ok(()) => return Ok(()),
                                    Err(e) if e.is::<super::SetupInterrupted>() => return Err(e),
                                    Err(e) => error = e.to_string(),
                                }
                            } else {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            _ => fields[field].1.edit(key),
        }
    }
}
async fn login_dialog(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    local: std::sync::Arc<crate::local::Local>,
    expected: Option<i64>,
) -> Result<()> {
    dialog(
        terminal,
        "Sign in to ChatGPT",
        "Requesting a one-time browser login code…",
    )?;
    let code = local.begin_login().await?;
    let text=format!("Open {}\n\nEnter this one-time code: {}\n\nWaiting for browser sign-in…\nEsc: cancel (credentials are never shown)",render::safe(&code.verification_url),render::safe(&code.user_code));
    let mut task = tokio::spawn(async move { local.finish_login(code, expected).await });
    loop {
        dialog(terminal, "Sign in to ChatGPT", &text)?;
        if task.is_finished() {
            (&mut task).await??;
            return Ok(());
        }
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc {
                    task.abort();
                    let _ = task.await;
                    return Err("Sign-in cancelled".into());
                }
            }
        }
        tokio::task::yield_now().await;
    }
}

fn reset_screen(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    // Full-screen dialogs do not need a cursor-position query (some PTYs never answer it).
    execute!(
        io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
        crossterm::cursor::MoveTo(0, 0)
    )?;
    *terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[tokio::test]
    async fn settings_refresh_reads_confirmed_personal_data() {
        let dir = tempfile::tempdir().unwrap();
        let local = crate::local::Local::open(
            &dir.path().join("state"),
            "127.0.0.1:0".parse().unwrap(),
            false,
        )
        .await
        .unwrap();
        let connection = Connection {
            mode: super::super::config::Mode::Standalone,
            privacy_key: Some(dir.path().join("privacy-key")),
            local: Some(local.clone()),
            ..Default::default()
        };
        let sealed = super::super::privacy::seal(
            &connection,
            super::super::privacy::Kind::Note,
            "personal",
            "Confirmed note",
        )
        .unwrap();
        let mut write = start(&connection, sealed, SETTINGS, true);
        assert_eq!((&mut write.task).await.unwrap().unwrap()["updated"], true);
        let mut refresh = start(&connection, ControlRequest::Doctor, SETTINGS, false);
        assert_eq!(
            (&mut refresh.task).await.unwrap().unwrap()["personal"][0],
            "Confirmed note"
        );
        let mut failed = start(
            &connection,
            ControlRequest::PrivatePut {
                id: "invalid".into(),
                ciphertext: None,
            },
            SETTINGS,
            true,
        );
        assert!((&mut failed.task).await.unwrap().is_err());
        let mut refresh = start(&connection, ControlRequest::Doctor, SETTINGS, false);
        assert_eq!(
            (&mut refresh.task).await.unwrap().unwrap()["personal"][0],
            "Confirmed note"
        );
        local.stop().await.unwrap();
    }
    #[test]
    fn routing_dialog_shows_all_fields_error_controls_and_cursor_on_minimum_terminal() {
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let preference =
            serde_json::to_value(crate::account_preferences::Preference::default()).unwrap();
        let state = State {
            modal: Some(Modal::Routing {
                account: 1,
                fields: ["1", "20", "off", "15"]
                    .map(|text| TextInput::new(text.into()))
                    .into(),
                selected: 3,
                error: "Switching threshold must be between 0 and 100, off or default".into(),
                summary: format!(
                    "Effective priority: 1\n{}",
                    crate::account_preferences::describe(&preference)
                ),
                locked: false,
            }),
            ..Default::default()
        };
        terminal
            .draw(|frame| render(frame, &state, &Connection::default(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for expected in [
            "All windows",
            "Short window",
            "Weekly window",
            "between 0 and 100",
            "Tab / ↑ / ↓",
            "Enter: save",
            "Esc: cancel",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(terminal.backend().cursor_visible());
    }
    #[test]
    fn routing_form_preserves_off_and_default_and_rejects_invalid_input() {
        let fields = ["-255", "20", "off", "default"].map(|text| TextInput::new(text.into()));
        let patch = routing_form(&fields).unwrap();
        assert_eq!(
            serde_json::to_value(patch).unwrap(),
            serde_json::json!({"priority":-255,"switch_at":20,"switch_at_short":"off","switch_at_weekly":"default"})
        );
        for invalid in ["256", "-256", "off", "1.5", ""] {
            let mut invalid_fields =
                ["-255", "20", "off", "default"].map(|text| TextInput::new(text.into()));
            invalid_fields[0] = TextInput::new(invalid.into());
            assert!(routing_form(&invalid_fields).is_err());
        }
        let mut default_fields = fields;
        default_fields[0] = TextInput::new("default".into());
        assert!(routing_form(&default_fields).is_ok());
    }
    #[test]
    fn connection_input_edits_unicode_at_the_cursor_and_scrolls() {
        let mut input = TextInput::new("ab界cd".into());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        input.edit(key(KeyCode::Home));
        input.edit(key(KeyCode::Right));
        input.edit(key(KeyCode::Right));
        input.edit(key(KeyCode::Delete));
        input.edit(key(KeyCode::Char('é')));
        assert_eq!(input.value, "abécd");
        input.edit(key(KeyCode::Backspace));
        assert_eq!(input.value, "abcd");
        input.edit(key(KeyCode::End));
        input.edit(key(KeyCode::Left));
        input.edit(key(KeyCode::Char('X')));
        assert_eq!(input.value, "abcXd");
        let (_, offset) = input.visible(3);
        assert!(offset < 3);
        input.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.value, "");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn connection_editor_shows_cursor_inside_long_fields() {
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let fields = [(
            "Private key path",
            TextInput::new(format!("/{}", "x".repeat(100))),
        )];
        terminal
            .draw(|frame| connection_fields(frame, &fields, 0, ""))
            .unwrap();
        assert!(terminal.backend().cursor_visible());
        let position = terminal.backend().cursor_position();
        assert_eq!(position.y, 1);
        assert!(position.x < 71);
    }

    #[test]
    fn keyboard_help_only_shows_current_tab_actions() {
        let connection = Connection::default();
        for (tab, action) in [
            (OVERVIEW, "account actions"),
            (USAGE, "change grouping"),
            (TOKENS, "token actions"),
            (MODELS, "copy selected model ID"),
            (SETTINGS, "settings actions"),
        ] {
            let state = State {
                tab,
                ..Default::default()
            };
            let text = keyboard_help(&state, &connection);
            assert!(text.contains(action));
            assert!(text.starts_with(TABS[tab]));
            for other in [
                "account actions",
                "change grouping",
                "token actions",
                "copy selected model ID",
                "settings actions",
            ] {
                if other != action {
                    assert!(!text.contains(other));
                }
            }
        }
    }

    #[test]
    fn usage_view_selects_users_models_and_tokens() {
        let now = chrono::Utc::now().timestamp();
        let rows = serde_json::json!([
            {"name":"tok_first","requests":3,"input_tokens":10,"output_tokens":20,"unknown_usage":0},
            {"name":"tok_second","requests":7,"input_tokens":null,"output_tokens":null,"unknown_usage":7}
        ]);
        let value = serde_json::json!({"rows":rows,"from_utc":now-3600,"to_utc":now,
            "timeline":[{"from_utc":now-3600,"rows":rows}]});
        for (group, by) in [(1, "user"), (2, "model"), (3, "token")] {
            for selected in 0..2 {
                let mut state = State {
                    tab: USAGE,
                    group,
                    usage_selected: selected,
                    ..Default::default()
                };
                state.values[USAGE] = Some(value.clone());
                assert!(
                    matches!(request(&state), ControlRequest::Usage { by: requested_by, .. } if requested_by.as_deref() == Some(by))
                );
                let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
                terminal
                    .draw(|frame| render(frame, &state, &Connection::default(), false))
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("Reported tokens · Today"));
                assert!(!text.contains("Selected:"));
                assert!(!text.contains("all"));
                if selected == 0 {
                    assert!(text.contains("3 requests"));
                    assert!(text.contains("30 reported tokens"));
                    assert!(text.contains("tok_first"));
                    assert!(!text.contains("tok_second"));
                } else {
                    assert!(text.contains("7 requests"));
                    assert!(text.contains("7 requests have incomplete counts"));
                    assert!(text.contains("tok_second"));
                    assert!(!text.contains("tok_first"));
                }
            }
        }
    }

    #[test]
    fn usage_axis_rounds_up_to_readable_limits() {
        for (value, expected) in [
            (0., 1.),
            (1., 1.),
            (3., 5.),
            (19., 20.),
            (21., 50.),
            (501., 1000.),
            (12345., 20000.),
            (50000., 50000.),
        ] {
            let upper = usage_axis_max(value);
            assert_eq!(upper, expected);
            assert!(upper >= value);
        }
    }

    #[test]
    fn usage_legend_shows_full_token_ids_on_small_terminals() {
        let now = chrono::Utc::now().timestamp();
        let id = "tok_0123456789abcdef";
        let rows = serde_json::json!([{"name":id,"requests":1,"input_tokens":10,"output_tokens":20,"unknown_usage":0}]);
        let value = serde_json::json!({"rows":rows,"from_utc":now-3600,"to_utc":now,
            "timeline":[{"from_utc":now-3600,"rows":rows}]});
        for (width, height) in [(72, 20), (120, 35)] {
            let mut state = State {
                tab: USAGE,
                group: 3,
                ..Default::default()
            };
            state.values[USAGE] = Some(value.clone());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &state, &Connection::default(), false))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                text.contains(id),
                "token legend missing at {width}x{height}"
            );
        }
    }

    #[test]
    fn overview_pairs_windows_on_wide_screens_and_stacks_them_when_narrow() {
        let now = chrono::Utc::now().timestamp();
        let value = serde_json::json!({"quota_accounts":[{"id":1,"label":"alice@example.com","quota":{"windows":[
            {"window_minutes":10080,"remaining_percent":50,"status":"current","observed_at":now,"reset_at":now+86400},
            {"window_minutes":300,"remaining_percent":50,"status":"current","observed_at":now,"reset_at":now+3600}
        ]}}]});
        for width in [72, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 35)).unwrap();
            let mut state = State::default();
            state.values[OVERVIEW] = Some(value.clone());
            terminal
                .draw(|frame| render(frame, &state, &Connection::default(), false))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let lines = buffer
                .content
                .chunks(usize::from(width))
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();
            let short = lines
                .iter()
                .position(|line| line.contains("5h  ·"))
                .unwrap();
            let weekly = lines
                .iter()
                .position(|line| line.contains("week  ·"))
                .unwrap();
            if width == 120 {
                assert_eq!(short, weekly);
                assert!(lines[short].find("5h").unwrap() < lines[short].find("week").unwrap());
            } else {
                assert!(short < weekly);
            }
            let ranges = account_ranges(&value, width - 6);
            assert_eq!(ranges[0].1 - ranges[0].0, if width == 120 { 4 } else { 6 });
        }
    }
    #[test]
    fn every_view_only_displays_its_own_status_and_notices() {
        let mut state = State::default();
        for (tab, title) in TABS.iter().enumerate() {
            state.statuses[tab] = format!("Updated {title} · 22:35:13");
            state.notices[tab] = Some(format!("{title} action completed"));
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
        for (tab, title) in TABS.iter().enumerate() {
            state.tab = tab;
            terminal
                .draw(|f| render(f, &state, &Connection::default(), false))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(!text.contains("Updated "));
            assert!(text.contains(&format!("{title} action completed")));
            for (other, other_title) in TABS.iter().enumerate() {
                if other != tab {
                    assert!(!text.contains(&format!("Updated {other_title}")));
                    assert!(!text.contains(&format!("{other_title} action completed")));
                }
            }
        }
    }
    #[test]
    fn overview_shows_recent_exhaustion_estimate_without_remaining_or_observation_date() {
        let now = chrono::Utc::now().timestamp();
        let mut state = State::default();
        let value = serde_json::json!({"quota_accounts":[{"id":1,"label":"alice@example.com","quota":{"windows":[{"kind":"primary","used_percent":12,"remaining_percent":88,"window_minutes":300,"reset_at":now+18000,"observed_at":now,"status":"current"}]}}]});
        let mut baseline = value.clone();
        baseline["quota_accounts"][0]["quota"]["windows"][0]["used_percent"] =
            serde_json::json!(10);
        baseline["quota_accounts"][0]["quota"]["windows"][0]["observed_at"] =
            serde_json::json!(now - 60);
        state.forecasts.observe(&baseline, now - 60);
        state.forecasts.observe(&value, now);
        state.values[OVERVIEW] = Some(value);
        let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
        terminal
            .draw(|f| render(f, &state, &Connection::default(), false))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("~44m to limit"));
        assert!(text.contains("Resets"));
        assert!(!text.contains("remaining"));
        assert!(!text.contains("Updated"));
    }
    #[test]
    fn usage_period_after_today_is_a_rolling_24_hour_report() {
        let state = State {
            tab: USAGE,
            period: 1,
            ..Default::default()
        };
        assert!(matches!(request(&state), ControlRequest::Usage { period, .. } if period == "24h"));
        assert_eq!(
            USAGE_PERIODS
                .iter()
                .map(|(_, title)| *title)
                .collect::<Vec<_>>(),
            ["Today", "Last 24 hours", "This week", "This month"]
        );
    }
    #[test]
    fn account_selection_scrolls_and_reset_targets_survive_reordered_refresh() {
        let mut state = State::default();
        state.values[OVERVIEW] = Some(serde_json::json!({"quota_accounts":[
            {"id":11,"label":"alice@example.com","quota":{"windows":[{"window_minutes":10080,"remaining_percent":5,"status":"current"}]}},
            {"id":22,"label":"bob@example.com","quota":{"windows":[{"window_minutes":10080,"remaining_percent":4,"status":"current"}]}}
        ]}));
        select_item(&mut state, true, 10, 66);
        assert_eq!(state.account_selected, 1);
        assert!(state.scroll > 0);
        assert!(matches!(
            selected_reset_request(&state),
            Some(ControlRequest::ResetPrepare { account: 22 })
        ));
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        terminal
            .draw(|f| render(f, &state, &Connection::default(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("▶ bob@example.com"));
        select_item(&mut state, false, 10, 66);
        assert_eq!(state.scroll, 0);
        select_item(&mut state, true, 10, 66);
        assert!(text.contains("Enter: account actions"));
        assert!(!text.contains("Subscription limits"));
        let mut refreshed = state.values[OVERVIEW].clone().unwrap();
        refreshed["quota_accounts"]
            .as_array_mut()
            .unwrap()
            .reverse();
        preserve_selection(&mut state, OVERVIEW, &refreshed);
        state.values[OVERVIEW] = Some(refreshed);
        assert_eq!(state.account_selected, 0);
        assert!(matches!(
            selected_reset_request(&state),
            Some(ControlRequest::ResetPrepare { account: 22 })
        ));
        select_item(&mut state, true, 10, 66);
        assert!(matches!(
            selected_reset_request(&state),
            Some(ControlRequest::ResetPrepare { account: 11 })
        ));
    }
    #[test]
    fn model_selection_matches_reversed_rows_and_keeps_selected_id_visible() {
        let mut state = State {
            tab: MODELS,
            ..Default::default()
        };
        state.values[MODELS] = Some(
            serde_json::json!({"data":(0..30).map(|i| serde_json::json!({"id":format!("model-{i:02}"),"display_name":format!("Model {i}")})).collect::<Vec<_>>()}),
        );
        assert_eq!(selected_model_id(&state), Some("model-29"));
        for _ in 0..20 {
            select_item(&mut state, true, 10, 66);
        }
        assert_eq!(selected_model_id(&state), Some("model-09"));
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        terminal
            .draw(|f| render(f, &state, &Connection::default(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("▶ model-09"));
        assert!(text.contains("Enter: copy model ID"));
        let mut refreshed = state.values[MODELS].clone().unwrap();
        refreshed["data"].as_array_mut().unwrap().reverse();
        preserve_selection(&mut state, MODELS, &refreshed);
        state.values[MODELS] = Some(refreshed);
        assert_eq!(selected_model_id(&state), Some("model-09"));
        state.values[MODELS] = Some(serde_json::json!({"data":[]}));
        select_item(&mut state, true, 10, 66);
        assert_eq!(selected_model_id(&state), None);
    }
    #[test]
    fn mode_badges_use_the_requested_backgrounds_and_header_keeps_version() {
        for (mode, title, color) in [
            (
                super::super::config::Mode::Remote,
                "Remote Server",
                Color::Rgb(177, 130, 230),
            ),
            (
                super::super::config::Mode::Standalone,
                "Standalone",
                Color::Rgb(245, 165, 75),
            ),
        ] {
            let connection = Connection {
                host: "router-with-a-long-hostname.example.com".into(),
                mode,
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
            terminal
                .draw(|f| render(f, &State::default(), &connection, false))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = buffer
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(!text.contains(title));
            let first_row = buffer.content[..72]
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            let second_row = buffer.content[144..216]
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(first_row.starts_with(&format!("  ExetRouter {}", env!("CARGO_PKG_VERSION"))));
            assert!(first_row.ends_with("  "));
            assert!(second_row.contains("Overview    Usage    Tokens    Models    Settings"));
            assert!(!second_row.contains('│'));
            for x in 1..11 {
                assert_eq!(buffer[(x, 2)].bg, Color::Cyan);
            }
            assert_eq!(buffer[(1, 2)].symbol(), " ");
            assert_eq!(buffer[(10, 2)].symbol(), " ");
            let state = State {
                tab: SETTINGS,
                ..Default::default()
            };
            terminal
                .draw(|f| render(f, &state, &connection, false))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = buffer
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains(title));
            assert!(text.contains(&format!("ExetRouter {}", env!("CARGO_PKG_VERSION"))));
            assert!(buffer
                .content
                .iter()
                .any(|c| c.bg == color && c.symbol() != " "));
        }
    }
    #[test]
    fn completed_updates_leave_a_clear_outcome_even_when_no_install_was_needed() {
        let mut report = crate::update::Report {
            current: "0.1.0".into(),
            latest: "v0.2.0".into(),
            available: true,
            release_url: String::new(),
        };
        assert!(finish_update(&mut report).contains("reopen"));
        assert!(!report.available);
        assert_eq!(report.current, "0.2.0");
        assert!(finish_update(&mut report).contains("already up to date"));
    }
    #[test]
    fn reset_dialog_shows_warning_and_confirmation_on_the_minimum_terminal() {
        assert_eq!(TABS, ["Overview", "Usage", "Tokens", "Models", "Settings"]);
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let state = State {
            modal: Some(Modal::ResetConfirm(
                serde_json::json!({"email":"test@example.com","remaining_percent":5,"available_count":2,"credit_title":"Full reset","free_reset_at":2000000000,"recommend_wait":true}),
            )),
            ..Default::default()
        };
        terminal
            .draw(|f| render(f, &state, &Connection::default(), false))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("less than 3 days"));
        assert!(text.contains("y: use one credit"));
        assert!(text.contains("Esc / n: cancel"));
    }
    #[test]
    fn quota_backgrounds_follow_percentages_with_contrasting_text() {
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        for remaining in [18., 38., 98.] {
            terminal
                .draw(|f| {
                    quota_bar(
                        f,
                        f.area(),
                        Some(remaining),
                        "Weekly · 38% remaining · historical",
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let (filled_bg, filled_fg) = quota_palette(Some(remaining));
            assert_eq!(buffer[(0, 0)].bg, filled_bg);
            assert_eq!(buffer[(0, 0)].symbol(), " ");
            for cell in &buffer.content {
                assert_ne!(cell.symbol(), "█");
                assert_ne!(cell.fg, Color::White);
                if cell.bg == filled_bg {
                    assert_eq!(cell.fg, filled_fg);
                } else {
                    assert_eq!(cell.fg, Color::Rgb(174, 184, 200));
                }
            }
        }
    }
    #[test]
    fn overview_contains_limits_without_a_limits_tab_and_scrolls_on_small_screens() {
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let mut state = State::default();
        state.values[OVERVIEW] = Some(
            serde_json::json!({"configuration_status":"configured","oauth":{"active_accounts":2},"catalog":{"status":"current"},"quota_accounts":[{"label":"Account 1","quota":{"windows":[{"window_minutes":10080,"remaining_percent":18,"status":"stale"}]}},{"label":"Account 2","quota":{"windows":[{"window_minutes":480,"remaining_percent":98,"status":"current"}]}}]}),
        );
        let connection = Connection::default();
        state.scroll = 8;
        terminal
            .draw(|f| render(f, &state, &connection, false))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("week"));
        assert!(text.contains("8h"));
        assert!(!TABS.contains(&"Limits"));
        assert!(text.contains("← / →: view"));
    }
    #[test]
    fn long_status_wraps_without_hiding_controls() {
        let mut state = State {
            tab: TOKENS,
            ..Default::default()
        };
        state.notices[TOKENS] = Some("The server connection was interrupted. Please check your network and refresh this view.".into());
        assert_eq!(status_lines(&state, false, 120).len(), 1);
        assert_eq!(status_lines(&state, false, 72).len(), 2);
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        terminal
            .draw(|f| render(f, &state, &Connection::default(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = buffer
            .content
            .chunks(72)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        assert!(rows[16].contains("connection was interrupted"));
        assert!(rows[17].contains("refresh this view."));
        assert!(rows[18].contains("q: quit"));
        assert!(rows[19].contains("Enter: actions"));
        assert_eq!(overview_viewport(20, 72, &state, false), 12);
    }
    #[test]
    fn contextual_controls_stay_in_footer_and_usage_has_a_plot() {
        for (width, height) in [(72, 20), (120, 35)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = State {
                tab: TOKENS,
                ..Default::default()
            };
            state.values[TOKENS] = Some(
                serde_json::json!([{"id":"tok_fixture","name":"Laptop","expires_at":2000000000,"revoked_at":null}]),
            );
            let connection = Connection::default();
            terminal
                .draw(|f| render(f, &state, &connection, false))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let row_text = |from: usize, to: usize| {
                buffer.content[from * width as usize..to * width as usize]
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
            };
            assert!(row_text(height as usize - 3, height as usize).contains("Enter: actions"));
            assert!(!row_text(0, height as usize - 3).contains("Enter: actions"));
            state.tab = USAGE;
            let now = chrono::Utc::now().timestamp();
            state.values[USAGE] = Some(
                serde_json::json!({"period":"day","from_utc":now-7200,"to_utc":now+3600,"rows":[{"name":"all","requests":6,"input_tokens":100,"output_tokens":10,"unknown_usage":1}],"timeline":[{"from_utc":now-7200,"rows":[{"name":"all","requests":2}]},{"from_utc":now-3600,"rows":[{"name":"all","requests":4}]},{"from_utc":now,"rows":[]}]}),
            );
            terminal
                .draw(|f| render(f, &state, &connection, false))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = buffer
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Reported tokens · Today"));
            assert!(text.contains("p: period"));
            assert!(text.contains("m: requests / tokens"));
            assert!(buffer
                .content
                .iter()
                .any(|c| c.symbol().chars().any(|ch| matches!(ch, '▀' | '▄' | '█'))));
            assert!(buffer.content.iter().any(|c| c.fg == Color::Cyan));
        }
    }
    #[test]
    fn dashboard_has_english_navigation_and_dismissed_secrets_leave_no_history() {
        let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
        let connection = Connection::default();
        let mut state = State {
            modal: Some(Modal::ClipboardRetry {
                secret: "synthetic-secret".into(),
                clipboard: Clipboard::fixture(),
            }),
            ..Default::default()
        };
        terminal
            .draw(|frame| render(frame, &state, &connection, false))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!text.contains("synthetic-secret"));
        assert!(text.contains("secret stays hidden"));
        state.modal = None;
        terminal
            .draw(|frame| render(frame, &state, &connection, false))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!text.contains("synthetic-secret"));
        assert!(text.contains("Overview"));
        assert!(text.contains("Tokens"));
        assert!(!text.contains("│ Limits"));
        assert!(text.contains("← / →: view"));
        assert!(text.contains("q: quit"));
    }
}
