use super::{clipboard::Clipboard, config::Connection, render, ssh_request, Session};
use crate::{ControlRequest, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span, Text},
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
    ("24h", "Last 24 hours"),
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
    ("90d", "Last 90 days"),
];
const LIMIT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const OVERVIEW: usize = 0;
const USAGE: usize = 1;
const TOKENS: usize = 2;
const MODELS: usize = 3;
const SETTINGS: usize = 4;
const TABS: [&str; 5] = ["Overview", "Usage", "Tokens", "Models", "Settings"];
const HEADER_HEIGHT: u16 = 3;
mod palette {
    use ratatui::style::Color;

    pub const BASE: Color = Color::Rgb(24, 24, 24);
    pub const SURFACE: Color = Color::Rgb(30, 30, 30);
    pub const RAISED: Color = Color::Rgb(37, 37, 37);
    pub const BORDER: Color = Color::Rgb(52, 52, 52);
    pub const TEXT: Color = Color::Rgb(237, 237, 235);
    pub const MUTED: Color = Color::Rgb(160, 159, 155);
    pub const ACCENT: Color = Color::Rgb(136, 186, 198);
    pub const SELECTION: Color = Color::Rgb(41, 52, 56);
}
const HISTORY_PERIODS: [(&str, i64); 4] = [
    ("7d", 7 * 86400),
    ("24h", 86400),
    ("30d", 30 * 86400),
    ("90d", 90 * 86400),
];

// Terminals normally report characters, not physical keys. Keep text input untouched;
// translate Russian ЙЦУКЕН characters only while interpreting command shortcuts.
fn shortcut_key(mut key: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(ch) = key.code {
        let russian = "йцукенгшщзхъфывапролджэячсмитьбюё";
        let english = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        if let Some(index) = russian.chars().position(|candidate| candidate == lower) {
            let mapped = english
                .chars()
                .nth(index)
                .expect("matching keyboard layouts");
            key.code = KeyCode::Char(if ch.is_uppercase() {
                mapped.to_ascii_uppercase()
            } else {
                mapped
            });
        } else if matches!(ch, '/' | '.' | ',') {
            key.code = KeyCode::Char('?');
        }
    }
    key
}

fn command_input(state: &State) -> bool {
    !matches!(
        state.modal,
        Some(Modal::Name(_) | Modal::Rename { .. } | Modal::Search { .. } | Modal::Days { .. })
    )
}

fn input_key(state: &State, key: KeyEvent) -> KeyEvent {
    if command_input(state) || key.modifiers.contains(KeyModifiers::CONTROL) {
        shortcut_key(key)
    } else {
        key
    }
}

fn shortcut_label(key: KeyCode) -> String {
    match key {
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Char('?') => "? /".into(),
        KeyCode::Char(ch) => ch.to_string(),
        _ => format!("{key:?}"),
    }
}

fn button_text(label: &str, key: KeyCode) -> String {
    format!(" {label} {} ", shortcut_label(key))
}

fn dashboard_buttons(state: &State) -> Vec<(&'static str, KeyCode)> {
    let mut entries = match state.tab {
        USAGE => vec![
            (USAGE_PERIODS[state.period].1, KeyCode::Char('p')),
            (
                ["Total", "By user", "By model", "By token"][state.group],
                KeyCode::Char('b'),
            ),
            (
                if state.usd_metric {
                    "USD estimate"
                } else if state.request_metric {
                    "Requests"
                } else {
                    "Tokens"
                },
                KeyCode::Char('m'),
            ),
            ("Breakdown", KeyCode::Enter),
        ],
        MODELS => vec![
            ("Copy ID", KeyCode::Enter),
            ("Search", KeyCode::Char('s')),
            ("Capabilities", KeyCode::Char('f')),
            ("Clear filters", KeyCode::Char('0')),
        ],
        TOKENS => vec![
            ("Actions", KeyCode::Enter),
            ("Create token", KeyCode::Char('n')),
            ("Search", KeyCode::Char('s')),
            ("Clear search", KeyCode::Char('0')),
        ],
        SETTINGS => vec![],
        _ => vec![("Details", KeyCode::Enter)],
    };
    if state.tab == USAGE {
        if all_usage_series_available(state) {
            entries.push((
                if state.usage_all {
                    "Selected series"
                } else {
                    "All series"
                },
                KeyCode::Char('a'),
            ));
        }
        entries.extend([
            ("Previous", KeyCode::Char('[')),
            ("Next", KeyCode::Char(']')),
        ]);
    }
    entries.extend([
        ("Refresh", KeyCode::Char('r')),
        ("Help", KeyCode::Char('?')),
        ("Quit", KeyCode::Char('q')),
    ]);
    entries
}

fn button_rows(entries: &[(&str, KeyCode)], width: u16) -> u16 {
    let mut rows = 1;
    let mut used = 0;
    for (label, key) in entries {
        let size = Line::from(button_text(label, *key)).width() as u16;
        if used > 0 && used + size > width {
            rows += 1;
            used = 0;
        }
        used += size + 1;
    }
    rows
}

fn controls_height(state: &State, busy: bool, width: u16) -> u16 {
    status_lines(state, busy, width).len() as u16
        + button_rows(&dashboard_buttons(state), width.saturating_sub(4))
        + 2
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum MouseAction {
    Key(KeyCode),
    Tab(usize),
    Select(usize),
    Menu(usize),
    Cursor,
}
#[derive(Clone, Copy, Debug)]
struct Hit {
    area: Rect,
    action: MouseAction,
}

fn mouse_key(mouse: MouseEvent, hits: &[Hit], state: &mut State) -> Option<KeyEvent> {
    if hits.is_empty() {
        return None;
    }
    let key = match mouse.kind {
        MouseEventKind::ScrollDown => KeyCode::Down,
        MouseEventKind::ScrollUp => KeyCode::Up,
        MouseEventKind::Down(MouseButton::Left) => {
            let hit = hits
                .iter()
                .rev()
                .find(|hit| hit.area.contains((mouse.column, mouse.row).into()))?;
            match hit.action {
                MouseAction::Key(key) => {
                    if state.tab == SETTINGS && state.modal.is_none() {
                        match key {
                            KeyCode::Char('v' | 'U') => state.settings_selected = 0,
                            KeyCode::Char('e') => state.settings_selected = 1,
                            KeyCode::Char('a') => state.settings_selected = 2,
                            KeyCode::Char('u') => state.settings_selected = 3,
                            _ => {}
                        }
                    }
                    key
                }
                MouseAction::Tab(tab) => KeyCode::Char(char::from(b'1' + tab as u8)),
                MouseAction::Select(index) => {
                    match state.tab {
                        OVERVIEW => state.account_selected = index,
                        MODELS => state.model_selected = index,
                        USAGE => state.usage_selected = index,
                        SETTINGS => {
                            state.selected = index;
                            state.settings_selected = 4 + index;
                        }
                        TOKENS => state.token_selected = index,
                        _ => state.selected = index,
                    }
                    if matches!(state.tab, OVERVIEW | SETTINGS | TOKENS | MODELS) {
                        KeyCode::Enter
                    } else {
                        return None;
                    }
                }
                MouseAction::Menu(index) => {
                    if let Some(Modal::Actions { selected, .. }) = &mut state.modal {
                        *selected = index;
                    }
                    KeyCode::Enter
                }
                MouseAction::Cursor => {
                    if let Some(Modal::Rename { input, .. } | Modal::Search { input, .. }) =
                        &mut state.modal
                    {
                        input.place_cursor(mouse.column.saturating_sub(hit.area.x) as usize);
                    }
                    return None;
                }
            }
        }
        _ => return None,
    };
    Some(KeyEvent::new(key, KeyModifiers::NONE))
}

fn buttons(frame: &mut Frame<'_>, area: Rect, entries: &[(&str, KeyCode)], hits: &mut Vec<Hit>) {
    let mut x = area.x;
    let mut y = area.y;
    for (label, key) in entries {
        let text = button_text(label, *key);
        let width = Line::from(text.as_str()).width() as u16;
        if x.saturating_add(width) > area.right() {
            x = area.x;
            y += 1;
        }
        if y >= area.bottom() || width > area.width {
            break;
        }
        let rect = Rect::new(x, y, width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(format!(" {label} ")),
                Span::styled(shortcut_label(*key), Style::default().fg(palette::ACCENT)),
                Span::raw(" "),
            ]))
            .style(Style::default().fg(palette::TEXT).bg(palette::RAISED)),
            rect,
        );
        hits.push(Hit {
            area: rect,
            action: MouseAction::Key(*key),
        });
        x += width + 1;
    }
}
#[derive(Default)]
struct State {
    settings_selected: usize,
    tab: usize,
    values: [Option<Value>; 5],
    selected: usize,
    token_selected: usize,
    account_selected: usize,
    account_hour: Option<i64>,
    history_period: usize,
    history_before: Option<i64>,
    history_account: Option<i64>,
    model_selected: usize,
    model_query: String,
    model_features: Vec<String>,
    token_query: String,
    scroll: u16,
    period: usize,
    group: usize,
    usage_selected: usize,
    usage_all: bool,
    usage_cursor: Option<i64>,
    request_metric: bool,
    usd_metric: bool,
    usage_before: Option<i64>,
    statuses: [String; 5],
    notices: [Option<String>; 5],
    forecasts: super::quota_forecast::Forecasts,
    modal: Option<Modal>,
    clipboard: Option<Clipboard>,
    update: Option<crate::update::Report>,
    update_checked: bool,
}
#[derive(Clone, Copy)]
enum SearchKind {
    Models,
    Tokens,
}
enum Modal {
    Usage {
        scroll: u16,
    },
    TokenPolicy {
        id: String,
        enabled: Option<bool>,
        fast: Option<Option<bool>>,
    },
    Search {
        kind: SearchKind,
        input: TextInput,
    },
    Account {
        id: i64,
        scroll: u16,
    },
    Actions {
        title: String,
        entries: Vec<(String, KeyCode)>,
        selected: usize,
    },
    UpdateConfirm,
    Name(String),
    Rename {
        id: String,
        input: TextInput,
        error: String,
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
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}
struct Pending {
    task: tokio::task::JoinHandle<Result<Value>>,
    tab: usize,
    mutation: bool,
    history_account: Option<i64>,
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
        USAGE => ControlRequest::UsageRange {
            period: USAGE_PERIODS[state.period].0.into(),
            by: [None, Some("user"), Some("model"), Some("token")][state.group].map(str::to_owned),
            timezone: Some(render::system_timezone()),
            before: state.usage_before,
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
    Pending {
        task: tokio::spawn(async move {
            if tab == SETTINGS && !mutation {
                let accounts = if let Some(local) = &session.connection.local {
                    local.accounts().await?
                } else {
                    ssh_request(&session, ControlRequest::Doctor).await?
                };
                return Ok(accounts);
            }
            ssh_request(&session, req).await
        }),
        tab,
        mutation,
        history_account: None,
    }
}
fn start_history(connection: &Connection, state: &State, account: i64) -> Pending {
    let mut pending = start(
        connection,
        ControlRequest::QuotaHistory {
            account,
            period: HISTORY_PERIODS[state.history_period].0.into(),
            before: state.history_before,
        },
        OVERVIEW,
        false,
    );
    pending.history_account = Some(account);
    pending
}
fn replace_history(state: &mut State, account: i64, history: Value) -> Result<()> {
    let wrapper = serde_json::json!({"quota_history":history});
    if super::account_details::range(&wrapper).is_none() {
        return Err("invalid subscription history report".into());
    }
    if let Some(row) = state.values[OVERVIEW]
        .as_mut()
        .and_then(|value| value["quota_accounts"].as_array_mut())
        .and_then(|rows| {
            rows.iter_mut()
                .find(|row| row["id"].as_i64() == Some(account))
        })
    {
        row["quota_history"] = wrapper["quota_history"].clone();
    }
    Ok(())
}
fn body(state: &State) -> String {
    let Some(value) = &state.values[state.tab] else {
        return "No data yet. Press r to load this view.".into();
    };
    match state.tab {
        TOKENS => render::response("tokens", value),
        MODELS => render::response("models", value),
        USAGE => render::response("usage", value),
        _ => render::response("doctor", value),
    }
}
fn mode_badge(connection: &Connection) -> Span<'static> {
    let (label, background, foreground) = if connection.mode == super::config::Mode::Standalone {
        (
            " Standalone ",
            Color::Rgb(59, 48, 32),
            Color::Rgb(217, 181, 114),
        )
    } else {
        (
            " Remote Server ",
            Color::Rgb(43, 40, 55),
            Color::Rgb(170, 163, 230),
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
                .fg(palette::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(destination)
            .style(Style::default().fg(palette::MUTED))
            .alignment(Alignment::Right),
        columns[1],
    );
    frame.render_widget(
        Tabs::new(TABS.iter().enumerate().map(|(index, title)| {
            if index == SETTINGS && state.update.as_ref().is_some_and(|r| r.available) {
                Line::styled(
                    " Settings ↑ 5 ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(format!(" {title} {} ", index + 1))
            }
        }))
        .select(state.tab)
        .padding("", "")
        .divider("  ")
        .style(Style::default().fg(palette::MUTED))
        .highlight_style(
            Style::default()
                .fg(palette::BASE)
                .bg(palette::TEXT)
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
    height.saturating_sub(HEADER_HEIGHT + controls_height(state, busy, width) + 1)
}
fn selection_style() -> Style {
    Style::default().fg(palette::TEXT).bg(palette::SELECTION)
}
fn section_style() -> Style {
    Style::default().bg(palette::SURFACE).fg(palette::MUTED)
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
fn filtered_token_rows<'a>(state: &State, value: &'a Value) -> Vec<&'a Value> {
    let query = state.token_query.trim().to_lowercase();
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|token| {
            format!(
                "{} {}",
                token["name"].as_str().unwrap_or(""),
                token["id"].as_str().unwrap_or("")
            )
            .to_lowercase()
            .contains(&query)
        })
        .collect()
}
fn selected_token(state: &State) -> Option<&Value> {
    filtered_token_rows(state, state.values[TOKENS].as_ref()?)
        .get(state.token_selected)
        .copied()
}
fn apply_search(state: &mut State, kind: SearchKind, query: &str) {
    match kind {
        SearchKind::Models => {
            state.model_query = query.trim().into();
            state.model_selected = 0;
        }
        SearchKind::Tokens => {
            state.token_query = query.trim().into();
            state.token_selected = 0;
        }
    }
    state.scroll = 0;
}
fn token_filter_height(state: &State) -> u16 {
    u16::from(!state.token_query.is_empty())
}
fn token_scroll(state: &State, viewport: u16) -> u16 {
    let start = (state.token_selected % 4 * 4) as u16 + token_filter_height(state);
    state
        .scroll
        .min(start)
        .max(start.saturating_add(3).saturating_sub(viewport))
}
fn navigate_tokens(state: &mut State, key: KeyCode, viewport: u16) -> bool {
    if state.tab != TOKENS
        || !matches!(
            key,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
        )
    {
        return false;
    }
    let count = state.values[TOKENS]
        .as_ref()
        .map_or(0, |value| filtered_token_rows(state, value).len());
    state.token_selected = match key {
        KeyCode::Home => 0,
        KeyCode::End => count.saturating_sub(1),
        KeyCode::Up => state.token_selected.saturating_sub(1),
        KeyCode::PageUp => state.token_selected.saturating_sub(4),
        KeyCode::PageDown => state.token_selected.saturating_add(4),
        _ => state.token_selected.saturating_add(1),
    }
    .min(count.saturating_sub(1));
    state.scroll = token_scroll(state, viewport);
    true
}
fn token_lines(state: &State, _width: u16) -> Vec<Line<'static>> {
    let Some(value) = state.values[TOKENS].as_ref() else {
        return vec![Line::from("  No data yet. Press r to load this view.")];
    };
    let rows = filtered_token_rows(state, value);
    let mut lines = Vec::new();
    if !state.token_query.is_empty() {
        lines.push(Line::styled(
            format!(
                "  Search: {} · {} {}",
                render::safe(&state.token_query),
                rows.len(),
                if rows.len() == 1 { "match" } else { "matches" }
            ),
            Style::default().fg(Color::Gray),
        ));
    }
    if rows.is_empty() {
        lines.push(Line::from(if state.token_query.is_empty() {
            "  No API tokens yet."
        } else {
            "  No matching tokens. Clear search with 0 or try another name or ID."
        }));
        return lines;
    }
    let first = state.token_selected / 4 * 4;
    for (i, token) in rows.iter().enumerate().skip(first).take(4) {
        let row_style = if i == state.token_selected {
            selection_style()
        } else {
            Style::default().fg(palette::TEXT).bg(palette::SURFACE)
        };
        let status = match render::token_status(token).as_str() {
            "active" => "Active",
            "disabled" => "Disabled",
            "revoked" => "Revoked",
            "expired" => "Expired",
            _ => "Unknown",
        };
        let (fg, bg) = match status {
            "Active" => (Color::Rgb(145, 188, 145), Color::Rgb(34, 46, 36)),
            "Disabled" | "Revoked" | "Expired" => {
                (Color::Rgb(223, 150, 144), Color::Rgb(54, 37, 35))
            }
            _ => (Color::Gray, Color::Rgb(35, 35, 35)),
        };
        let mut header = vec![
            Span::raw(format!(
                "{}{}  ",
                if i == state.token_selected {
                    "▶ "
                } else {
                    "  "
                },
                render::safe(token["name"].as_str().unwrap_or("Unnamed"))
            )),
            Span::styled(format!(" {status} "), Style::default().fg(fg).bg(bg)),
        ];
        if let Some(allow) = token["fast_mode"].as_bool() {
            header.push(Span::raw(" "));
            header.push(Span::styled(
                if allow {
                    " Fast: Allow "
                } else {
                    " Fast: Block "
                },
                Style::default()
                    .fg(Color::Rgb(217, 181, 114))
                    .bg(Color::Rgb(51, 44, 32)),
            ));
        }
        lines.push(Line::from(header).style(row_style));
        lines.push(Line::styled(
            format!(
                "  {}",
                render::safe(token["id"].as_str().unwrap_or("Unknown"))
            ),
            row_style.fg(palette::MUTED),
        ));
        lines.push(Line::styled(
            format!("  {}", render::token_dates(token)),
            row_style.fg(palette::MUTED),
        ));
        lines.push(Line::from(""));
    }
    if rows.len() > 4 {
        lines.push(Line::styled(
            format!(
                "  Showing {}–{} of {}",
                first + 1,
                (first + 4).min(rows.len()),
                rows.len()
            ),
            Style::default().fg(Color::Gray),
        ));
    }
    lines
}
fn settings_actions(state: &State, connection: &Connection) -> Vec<(&'static str, KeyCode)> {
    let mut actions = vec![
        if state.update.as_ref().is_some_and(|report| report.available) {
            ("Update", KeyCode::Char('U'))
        } else {
            ("Check updates", KeyCode::Char('v'))
        },
        ("Configure connection", KeyCode::Char('e')),
    ];
    if connection.local.is_some() {
        actions.extend([
            ("Add account", KeyCode::Char('a')),
            ("Reauthorize account", KeyCode::Char('u')),
        ]);
    }
    actions
}

fn settings_lines(connection: &Connection, state: &State, width: u16) -> Vec<Line<'static>> {
    let heading = |text: &str| section_heading(text, width);
    let field = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("  {label:<14}"), Style::default().fg(Color::Gray)),
            Span::styled(render::safe(&value), Style::default().fg(Color::White)),
        ])
    };
    let actions = settings_actions(state, connection);
    let button = |index: usize| {
        Line::styled(
            format!(
                "{}[ {} ]",
                if state.settings_selected == index {
                    "▶ "
                } else {
                    "  "
                },
                actions[index].0
            ),
            if state.settings_selected == index {
                selection_style()
            } else {
                Style::default().fg(Color::White)
            },
        )
    };
    let mut lines = vec![
        button(0),
        Line::from(format!(
            "  {}",
            update_text(state).trim().trim_start_matches("Updates · ")
        )),
        Line::default(),
        heading("CONNECTION"),
        Line::from(vec![
            Span::styled("  Connection type", Style::default().fg(Color::Gray)),
            mode_badge(connection),
        ]),
    ];
    if connection.mode == super::config::Mode::Standalone {
        let address = connection
            .local
            .as_ref()
            .map_or(connection.listen, |local| local.address);
        lines.push(field("API endpoint", format!("http://{address}/v1")));
        lines.push(field("Private state", connection.state_dir.clone()));
        lines.push(button(1));
        lines.push(Line::default());
        lines.push(heading("LOCAL ACCOUNTS"));
        if connection.local.is_some() {
            lines.push(button(2));
            lines.push(button(3));
        }
        let accounts = state.values[SETTINGS].as_ref().and_then(Value::as_array);
        if accounts.is_none_or(Vec::is_empty) {
            lines.push(Line::styled(
                "  No accounts yet. Add one to get started.",
                Style::default().fg(Color::Gray),
            ));
        }
        for (i, account) in accounts.into_iter().flatten().enumerate() {
            let selected = i == state.selected && state.settings_selected >= actions.len();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{}{}",
                        if selected { "▶ " } else { "  " },
                        render::safe(account["email"].as_str().unwrap_or("Email unavailable"))
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
        lines.push(button(1));
    }
    lines
}
fn account_actions(state: &State) -> Option<Modal> {
    let row = state.values[SETTINGS]
        .as_ref()?
        .as_array()?
        .get(state.selected)?;
    let mut entries = vec![("Reauthorize account".into(), KeyCode::Char('u'))];
    if row["state"] != "disabled" {
        entries.push(("Disable account".into(), KeyCode::Char('d')));
    }
    Some(Modal::Actions {
        title: render::safe(row["email"].as_str().unwrap_or("Account actions")),
        entries,
        selected: 0,
    })
}
fn keep_settings_selection_visible(
    state: &mut State,
    connection: &Connection,
    viewport: u16,
    width: u16,
) {
    let mut start = 0_u16;
    for line in settings_lines(connection, state, width) {
        let selected = line.to_string().starts_with("▶ ");
        let height = Paragraph::new(line)
            .wrap(Wrap { trim: false })
            .line_count(width)
            .min(usize::from(u16::MAX)) as u16;
        let end = start.saturating_add(height);
        if selected {
            if start < state.scroll {
                state.scroll = start;
            } else if end > state.scroll.saturating_add(viewport) {
                state.scroll = end.saturating_sub(viewport);
            }
            break;
        }
        start = end;
    }
}
fn model_rows(value: &Value) -> Vec<&Value> {
    render::model_rows(value)
}
fn filtered_model_rows<'a>(state: &State, value: &'a Value) -> Vec<&'a Value> {
    model_rows(value)
        .into_iter()
        .filter(|row| render::model_matches(row, &state.model_query, &state.model_features))
        .collect()
}
fn model_filter_height(state: &State) -> u16 {
    u16::from(!state.model_query.is_empty() || !state.model_features.is_empty())
}
fn selected_model_id(state: &State) -> Option<&str> {
    state.values[MODELS]
        .as_ref()
        .and_then(|value| {
            filtered_model_rows(state, value)
                .get(state.model_selected)
                .copied()
        })
        .and_then(|row| row["id"].as_str())
        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
}
fn account_detail_text(state: &State, id: i64) -> String {
    state.values[OVERVIEW]
        .as_ref()
        .and_then(|value| value["quota_accounts"].as_array())
        .and_then(|rows| rows.iter().find(|row| row["id"].as_i64() == Some(id)))
        .map_or_else(
            || "Account unavailable. Refresh Overview.".into(),
            |row| render::account_details(row, chrono::Utc::now().timestamp()),
        )
}
fn account_row(state: &State, id: i64) -> Option<&Value> {
    state.values[OVERVIEW]
        .as_ref()?
        .get("quota_accounts")?
        .as_array()?
        .iter()
        .find(|row| row["id"].as_i64() == Some(id))
}
fn action_menu(state: &State, _connection: &Connection) -> (String, Vec<(String, KeyCode)>) {
    let entries = match state.tab {
        TOKENS => {
            let mut entries = vec![("Create token".into(), KeyCode::Char('n'))];
            if let Some(token) = selected_token(state) {
                entries.insert(0, ("Rename token".into(), KeyCode::Char('l')));
                entries.insert(1, ("Rotate token".into(), KeyCode::Char('o')));
                entries.insert(2, ("Revoke token".into(), KeyCode::Char('x')));
                entries.insert(3, ("Copy token ID".into(), KeyCode::Char('c')));
                if token_policy_available(token) {
                    entries.insert(
                        3,
                        (
                            if token["disabled"] == true {
                                "Enable token"
                            } else {
                                "Disable token"
                            }
                            .into(),
                            KeyCode::Char('d'),
                        ),
                    );
                    entries.insert(4, ("Fast mode".into(), KeyCode::Char('f')));
                }
            }
            entries
        }
        _ => vec![],
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
fn token_rename_modal(state: &State) -> Option<Modal> {
    let row = selected_token(state)?;
    Some(Modal::Rename {
        id: row["id"].as_str()?.into(),
        input: TextInput::new(row["name"].as_str()?.into()),
        error: String::new(),
    })
}

fn token_policy_available(token: &Value) -> bool {
    matches!(render::token_status(token).as_str(), "active" | "disabled")
}

fn token_rename_request(id: &str, input: &TextInput) -> Result<ControlRequest> {
    let name = input.value.trim();
    super::privacy::validate_token_label(name)?;
    Ok(ControlRequest::TokenRename {
        id: id.into(),
        name: name.into(),
    })
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
        let rows = filtered_model_rows(state, value);
        state.model_selected = old_id
            .and_then(|id| rows.iter().position(|row| row["id"].as_str() == Some(id)))
            .unwrap_or(state.model_selected)
            .min(rows.len().saturating_sub(1));
    } else if tab == TOKENS {
        let old_id = selected_token(state).and_then(|row| row["id"].as_str());
        let rows = filtered_token_rows(state, value);
        state.token_selected = old_id
            .and_then(|id| rows.iter().position(|row| row["id"].as_str() == Some(id)))
            .unwrap_or(state.token_selected)
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
    let mut start = render::overview_summary(value).lines().count() as u16 + 1;
    value["quota_accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|account| {
            let windows = render::visible_windows(&account["quota"]);
            let height =
                3 + u16::from(credit_expiring_soon(
                    account,
                    chrono::Utc::now().timestamp(),
                )) + u16::from(account["refresh_error"].is_string())
                    + u16::from(render::weekly_activation(account).is_some())
                    + super::quota_forecast::notes(account, chrono::Utc::now().timestamp()).len()
                        as u16
                    + if account["platform"].is_object() {
                        3
                    } else {
                        0
                    }
                    + if windows.is_empty() {
                        1
                    } else {
                        windows.len().div_ceil(quota_columns(width)) as u16 * 3
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
            .map_or(0, |value| filtered_model_rows(state, value).len());
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
fn models(frame: &mut Frame<'_>, area: Rect, state: &State, value: &Value) -> usize {
    let rows = filtered_model_rows(state, value);
    let filter_height = model_filter_height(state);
    if filter_height > 0 {
        let features = state
            .model_features
            .iter()
            .filter_map(|id| render::capability_label(id))
            .collect::<Vec<_>>()
            .join(" + ");
        frame.render_widget(
            Paragraph::new(format!("Search: {} · {features}", state.model_query))
                .style(Style::default().fg(Color::Gray)),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }
    let area = Rect::new(
        area.x,
        area.y + filter_height,
        area.width,
        area.height.saturating_sub(filter_height),
    );
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching models. Clear filters with 0 or press r to refresh.")
                .block(content_panel()),
            area,
        );
        return 0;
    }
    let rows = rows.into_iter().enumerate().map(|(index, row)| {
        let id = row["id"].as_str().unwrap_or("Unknown");
        let name = crate::catalog::display_name(
            row["display_name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(id),
        );
        let context = row["exetrouter"]["context_window"]
            .as_u64()
            .map(|n| format!(" · {n} context"))
            .unwrap_or_default();
        let mut capabilities = vec![Span::raw("  ")];
        for id in row["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if let Some(label) = render::capability_label(id) {
                capabilities.push(Span::styled(
                    format!(" {label} "),
                    Style::default().fg(palette::MUTED).bg(palette::RAISED),
                ));
                capabilities.push(Span::raw(" "));
            }
        }
        if row["source"] == "platform" {
            capabilities.push(Span::styled(
                " Platform API ",
                Style::default()
                    .fg(Color::Rgb(227, 169, 117))
                    .bg(Color::Rgb(59, 48, 32)),
            ));
        }
        Row::new(vec![Text::from(vec![
            Line::from(vec![
                Span::styled(
                    format!(
                        "{}{}",
                        if index == state.model_selected {
                            "▶ "
                        } else {
                            "  "
                        },
                        render::safe(&name)
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(context, Style::default().fg(palette::MUTED)),
            ]),
            Line::styled(
                format!("  {}", render::safe(id)),
                Style::default().fg(palette::MUTED),
            ),
            Line::from(capabilities),
        ])])
        .height(3)
        .bottom_margin(1)
        .style(if index == state.model_selected {
            selection_style()
        } else {
            Style::default().fg(palette::TEXT).bg(palette::SURFACE)
        })
    });
    let table = Table::new(rows, [Constraint::Min(0)]).block(content_panel());
    let mut selection = TableState::default().with_selected(Some(state.model_selected));
    frame.render_stateful_widget(table, area, &mut selection);
    selection.offset()
}
#[cfg(test)]
fn render(frame: &mut Frame<'_>, state: &State, connection: &Connection, busy: bool) {
    render_hits(frame, state, connection, busy);
}

fn render_hits(
    frame: &mut Frame<'_>,
    state: &State,
    connection: &Connection,
    busy: bool,
) -> Vec<Hit> {
    let mut hits = Vec::new();
    frame.render_widget(
        Block::default().style(Style::default().fg(palette::TEXT).bg(palette::BASE)),
        frame.area(),
    );
    if frame.area().width < 72 || frame.area().height < 20 {
        frame.render_widget(
            Paragraph::new(
                "Terminal too small. Resize to at least 72 columns × 20 rows. Ctrl-C exits.",
            )
            .wrap(Wrap { trim: false }),
            frame.area(),
        );
        return hits;
    }
    let notice_lines = status_lines(state, busy, frame.area().width);
    let chunks = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Min(3),
        Constraint::Length(controls_height(state, busy, frame.area().width)),
    ])
    .split(frame.area());
    header(frame, chunks[0], state, connection);
    let mut x = chunks[0].x + 1;
    for (index, title) in TABS.iter().enumerate() {
        let width = title.len() as u16
            + 4
            + u16::from(index == SETTINGS && state.update.as_ref().is_some_and(|r| r.available))
                * 2;
        hits.push(Hit {
            area: Rect::new(x, chunks[0].y + 2, width, 1),
            action: MouseAction::Tab(index),
        });
        x += width + 2;
    }
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
                .scroll((
                    token_scroll(state, content_panel().inner(content).height),
                    0,
                ))
                .block(content_panel()),
            content,
        ),
        (USAGE, Some(value)) => usage_chart(frame, content, state, value),
        (OVERVIEW, Some(value)) => overview(frame, content, state, value),
        (MODELS, Some(value)) => {
            let offset = models(frame, content, state, value);
            let inner = content_panel().inner(content);
            let filter_height = model_filter_height(state);
            for (row, index) in (offset..filtered_model_rows(state, value).len())
                .take(inner.height.saturating_sub(filter_height).div_ceil(4) as usize)
                .enumerate()
            {
                hits.push(Hit {
                    area: Rect::new(
                        inner.x,
                        inner.y + filter_height + row as u16 * 4,
                        inner.width,
                        inner
                            .bottom()
                            .saturating_sub(inner.y + filter_height + row as u16 * 4)
                            .min(3),
                    ),
                    action: MouseAction::Select(index),
                });
            }
        }
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
    let entries = dashboard_buttons(state);
    let toolbar_rows = button_rows(&entries, chunks[2].width.saturating_sub(4));
    footer.push(Line::styled(
        "Controls",
        Style::default().fg(palette::MUTED),
    ));
    for _ in 0..toolbar_rows {
        footer.push(Line::default());
    }
    footer.push(match state.tab {
        TOKENS => keys(&[("← / →", "view"), ("↑ / ↓", "select")]),
        OVERVIEW => keys(&[
            ("← / →", "view"),
            ("↑ / ↓", "select"),
            ("PgUp / PgDn", "page"),
        ]),
        USAGE if state.group != 0 => keys(&[
            ("← / →", "view"),
            ("↑ / ↓", "select"),
            ("Shift+←/→", "chart"),
        ]),
        USAGE => keys(&[
            ("← / →", "view"),
            ("Shift+←/→", "chart"),
            ("Home/End", "first/latest"),
        ]),
        MODELS => keys(&[
            ("← / →", "view"),
            ("↑ / ↓", "select"),
            ("PgUp / PgDn", "page"),
        ]),
        SETTINGS if connection.local.is_some() => keys(&[("← / →", "view"), ("↑ / ↓", "select")]),
        SETTINGS => keys(&[
            ("← / →", "view"),
            ("↑ / ↓", "select"),
            ("Enter", "activate"),
        ]),
        _ => keys(&[("↑ / ↓", "scroll"), ("PgUp / PgDn", "page")]),
    });

    if let Some(modal) = &state.modal {
        footer = vec![match modal {
            Modal::Account { .. } => keys(&[
                ("← / →", "hour"),
                ("↑ / ↓", "scroll"),
                ("Home / End", "first / latest"),
            ]),
            Modal::Usage { .. } => keys(&[
                ("↑ / ↓", "scroll"),
                ("PgUp / PgDn", "page"),
                ("Home / End", "first / last"),
            ]),
            Modal::Actions { .. } => keys(&[("↑ / ↓", "select")]),
            Modal::Rename { .. } => keys(&[("← / →", "cursor"), ("Ctrl-U", "clear")]),
            Modal::Search { .. } => keys(&[("← / →", "cursor"), ("Ctrl-U", "clear")]),
            _ => Line::default(),
        }];
    }
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().padding(Padding::horizontal(2))),
        chunks[2],
    );
    let toolbar = Rect::new(
        chunks[2].x + 2,
        chunks[2].bottom() - 1 - toolbar_rows,
        chunks[2].width.saturating_sub(4),
        toolbar_rows,
    );
    if state.modal.is_none() {
        buttons(frame, toolbar, &entries, &mut hits);
    }
    if state.tab == OVERVIEW {
        if let Some(value) = &state.values[OVERVIEW] {
            let inner = content_panel()
                .padding(Padding::new(2, 2, 1, 0))
                .inner(content);
            for (index, (start, end)) in account_ranges(value, inner.width).into_iter().enumerate()
            {
                let top = (inner.y as i32 + start as i32 - state.scroll as i32).max(inner.y as i32);
                let bottom =
                    (inner.y as i32 + end as i32 - state.scroll as i32).min(inner.bottom() as i32);
                if bottom > top {
                    hits.push(Hit {
                        area: Rect::new(inner.x, top as u16, inner.width, (bottom - top) as u16),
                        action: MouseAction::Select(index),
                    });
                }
            }
        }
    }
    if state.tab == TOKENS {
        let inner = content_panel().inner(content);
        let first = state.token_selected / 4 * 4;
        let count = state.values[TOKENS]
            .as_ref()
            .map_or(0, |value| filtered_token_rows(state, value).len());
        let filter_height = token_filter_height(state);
        let mut y = inner.y as i32 + i32::from(filter_height)
            - i32::from(token_scroll(state, inner.height));
        for line in token_lines(state, inner.width)
            .into_iter()
            .skip(usize::from(filter_height))
            .take(count.saturating_sub(first).min(4) * 4)
            .enumerate()
        {
            let height = 1;
            let top = y.max(inner.y as i32);
            let bottom = (y + height).min(inner.bottom() as i32);
            if bottom > top {
                hits.push(Hit {
                    area: Rect::new(inner.x, top as u16, inner.width, (bottom - top) as u16),
                    action: MouseAction::Select(first + line.0 / 4),
                });
            }
            y += height;
        }
    }
    if state.tab == SETTINGS {
        let inner = content_panel().inner(content);
        let count = state.values[SETTINGS]
            .as_ref()
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let mut y = inner.y as i32 - state.scroll as i32;
        for (index, line) in settings_lines(connection, state, inner.width)
            .into_iter()
            .enumerate()
        {
            let action =
                settings_actions(state, connection)
                    .into_iter()
                    .find_map(|(label, key)| {
                        line.to_string()
                            .contains(&format!("[ {label} ]"))
                            .then_some(key)
                    });
            let height = Paragraph::new(line)
                .wrap(Wrap { trim: false })
                .line_count(inner.width) as i32;
            let top = y.max(inner.y as i32);
            let bottom = (y + height).min(inner.bottom() as i32);
            if connection.mode == super::config::Mode::Standalone
                && (11 + 2 * usize::from(connection.local.is_some())
                    ..11 + 2 * usize::from(connection.local.is_some()) + count)
                    .contains(&index)
                && bottom > top
            {
                hits.push(Hit {
                    area: Rect::new(inner.x, top as u16, inner.width, (bottom - top) as u16),
                    action: MouseAction::Select(
                        index - 11 - 2 * usize::from(connection.local.is_some()),
                    ),
                });
            }
            if let Some(key) = action {
                if bottom > top {
                    hits.push(Hit {
                        area: Rect::new(inner.x, top as u16, inner.width, (bottom - top) as u16),
                        action: MouseAction::Key(key),
                    });
                }
            }
            y += height;
        }
    }
    if let Some(modal) = &state.modal {
        hits.clear();
        let (title,text) = match modal {
            Modal::Usage { .. } => ("Usage breakdown", usage_breakdown(state)),
            Modal::TokenPolicy { id, enabled, fast } => ("Change token policy", format!("Token: {id}\n{}\nThe existing secret is unchanged. Confirm this change?", if let Some(enabled)=enabled { if *enabled {"Enable token"} else {"Disable token"} } else { match fast {Some(Some(true))=>"Fast: Allow",Some(Some(false))=>"Fast: Block",_=>"Fast: inherit server default"} })),
            Modal::Search { kind, input } => match kind {
                SearchKind::Models => ("Search models", format!("{}\n\nNames, IDs and capabilities", input.value)),
                SearchKind::Tokens => ("Search tokens", format!("{}\n\nNames and token IDs", input.value)),
            },
            Modal::Account { id, .. } => ("Account details", account_detail_text(state,*id)),
            Modal::Actions { title, entries, selected } => (title.as_str(), entries.iter().enumerate().map(|(i,(name,key))|format!("{}{} {}",if i==*selected {"▶ "}else{"  "},name,shortcut_label(*key))).collect::<Vec<_>>().join("\n")),
            Modal::UpdateConfirm => ("Update exr", "Install the latest published release in the existing installation?\nThe installation method is preserved; config, accounts and tokens stay in place.\nSource builds may take several minutes. Restart exr after completion.".into()),
            Modal::Name(text) => ("Create token",format!("Token name: {text}")),
            Modal::Rename { input, error, .. } => ("Rename token", format!("{}\n\n{error}", input.value)),
            Modal::Days { name,text } => ("Create token",format!("Name: {name}\nExpires in days (1–365): {text}")),
            Modal::Confirm { id,rotate } => (if *rotate {"Rotate token"}else{"Revoke token"}, format!("Token: {id}\n{}",if *rotate {"The old secret will stop working. The new secret will be copied to the clipboard."}else{"This token will stop working immediately."})),
            Modal::ClipboardRetry { .. } => ("Clipboard unavailable", "The token was issued, but copying failed.\nThe secret stays hidden. No token request will be repeated.\nDiscarding it requires rotating the token later.".into()),
            Modal::Help => ("Keyboard help", keyboard_help(state, connection)),
        };
        let mut area = if matches!(modal, Modal::Account { .. } | Modal::Usage { .. }) {
            account_popup(frame.area())
        } else {
            popup(frame.area())
        };
        let lines = if let Modal::Account { id, .. } = modal {
            account_row(state, *id).map_or(1, |row| {
                super::account_details::height(
                    row,
                    chrono::Utc::now().timestamp(),
                    area.width.saturating_sub(4),
                )
            })
        } else {
            Paragraph::new(styled_text(&text))
                .wrap(Wrap { trim: false })
                .line_count(
                    area.width
                        .saturating_sub(if matches!(modal, Modal::Usage { .. }) {
                            4
                        } else {
                            2
                        }),
                )
        };
        let needed = lines.saturating_add(4).clamp(6, usize::from(area.height));
        area.height = needed as u16;
        area.y = frame.area().y + (frame.area().height - area.height) / 2;
        frame.render_widget(Clear, area);
        let title = title_line(title);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(palette::TEXT).bg(palette::BASE))
            .border_style(Style::default().fg(palette::BORDER));
        if let Modal::Account { id, scroll } = modal {
            frame.render_widget(block, area);
            let content = Rect::new(
                area.x + 2,
                area.y + 1,
                area.width.saturating_sub(4),
                area.height.saturating_sub(4),
            );
            let max = lines
                .saturating_sub(content.height as usize)
                .min(u16::MAX as usize) as u16;
            if let Some(row) = account_row(state, *id) {
                super::account_details::draw(
                    frame,
                    content,
                    row,
                    chrono::Utc::now().timestamp(),
                    (*scroll).min(max),
                    state.account_hour,
                );
            } else {
                frame.render_widget(Paragraph::new(text), content);
            }
        } else if let Modal::Usage { scroll } = modal {
            frame.render_widget(block, area);
            let max = lines
                .saturating_sub(usize::from(area.height.saturating_sub(4)))
                .min(usize::from(u16::MAX)) as u16;
            frame.render_widget(
                Paragraph::new(styled_text(&text))
                    .wrap(Wrap { trim: false })
                    .scroll(((*scroll).min(max), 0)),
                Rect::new(
                    area.x + 2,
                    area.y + 1,
                    area.width.saturating_sub(4),
                    area.height.saturating_sub(4),
                ),
            );
        } else {
            frame.render_widget(
                Paragraph::new(styled_text(&text))
                    .wrap(Wrap { trim: false })
                    .block(block),
                area,
            );
        }
        match modal {
            Modal::Actions { entries, .. } => {
                let mut y = area.y + 1;
                for (index, (label, key)) in entries.iter().enumerate() {
                    let height = Paragraph::new(format!("  {label} {}", shortcut_label(*key)))
                        .wrap(Wrap { trim: false })
                        .line_count(area.width.saturating_sub(2))
                        as u16;
                    if y + height <= area.bottom().saturating_sub(2) {
                        hits.push(Hit {
                            area: Rect::new(area.x + 1, y, area.width - 2, height),
                            action: MouseAction::Menu(index),
                        });
                    }
                    y += height;
                }
            }
            Modal::Rename { input, .. } | Modal::Search { input, .. }
                if Line::from(input.value.as_str()).width()
                    <= area.width.saturating_sub(2) as usize =>
            {
                hits.push(Hit {
                    area: Rect::new(area.x + 1, area.y + 1, area.width - 2, 1),
                    action: MouseAction::Cursor,
                });
            }
            _ => {}
        }
        let controls: &[(&str, KeyCode)] = match modal {
            Modal::Account { .. } => &[
                ("Period", KeyCode::Char('h')),
                ("Previous", KeyCode::Char('[')),
                ("Next", KeyCode::Char(']')),
                ("Refresh", KeyCode::Char('r')),
                ("Close", KeyCode::Esc),
            ],
            Modal::Confirm { rotate: true, .. } => &[
                ("Rotate token", KeyCode::Char('y')),
                ("Cancel", KeyCode::Esc),
            ],
            Modal::TokenPolicy { .. } => &[("Save", KeyCode::Char('y')), ("Cancel", KeyCode::Esc)],
            Modal::Confirm { rotate: false, .. } => &[
                ("Revoke token", KeyCode::Char('y')),
                ("Cancel", KeyCode::Esc),
            ],
            Modal::ClipboardRetry { .. } => &[
                ("Retry copy", KeyCode::Char('c')),
                ("Discard", KeyCode::Esc),
            ],
            Modal::Help | Modal::Usage { .. } => &[("Close", KeyCode::Esc)],
            Modal::Actions { .. } => &[("Close", KeyCode::Esc)],
            Modal::UpdateConfirm => &[("Update", KeyCode::Enter), ("Cancel", KeyCode::Esc)],
            Modal::Rename { .. } => &[("Save", KeyCode::Enter), ("Cancel", KeyCode::Esc)],
            Modal::Search { .. } => &[("Search", KeyCode::Enter), ("Cancel", KeyCode::Esc)],
            Modal::Days { .. } => &[("Create token", KeyCode::Enter), ("Cancel", KeyCode::Esc)],
            _ => &[("Continue", KeyCode::Enter), ("Cancel", KeyCode::Esc)],
        };
        buttons(
            frame,
            Rect::new(
                area.x + 2,
                area.bottom() - 2,
                area.width.saturating_sub(4),
                1,
            ),
            controls,
            &mut hits,
        );
        if let Modal::Rename { input, .. } | Modal::Search { input, .. } = modal {
            if Line::from(input.value.as_str()).width() <= area.width.saturating_sub(2) as usize {
                frame.set_cursor_position((
                    area.x + 1 + Line::from(&input.value[..input.cursor]).width() as u16,
                    area.y + 1,
                ));
            }
        }
    }
    hits
}

fn panel(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::BORDER))
        .title(title_line(title))
}
fn title_line(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title.to_owned(),
            Style::default()
                .fg(palette::TEXT)
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
                .fg(palette::ACCENT)
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
            if line.starts_with("▶ ") {
                return Line::styled(line.to_owned(), selection_style());
            }
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
    let summary = render::overview_summary(value);
    let summary_lines = styled_text(&summary);
    let summary_height = summary_lines.len() as u16 + 1;
    let visible = summary_height
        .saturating_sub(state.scroll)
        .min(inner.height);
    if visible > 0 {
        frame.render_widget(
            Paragraph::new(summary_lines).scroll((state.scroll, 0)),
            Rect::new(inner.x, inner.y, inner.width, visible),
        );
    }
    let mut y = inner.y as i32 + summary_height as i32 - state.scroll as i32;
    let end = (inner.y + inner.height) as i32;
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
    let ranges = account_ranges(value, inner.width);
    for (index, account) in accounts.into_iter().enumerate() {
        let style = if index == state.account_selected {
            selection_style()
        } else {
            Style::default().fg(palette::TEXT).bg(palette::SURFACE)
        };
        let top = y.max(i32::from(inner.y));
        let bottom = (y + i32::from(ranges[index].1 - ranges[index].0 - 1)).min(end);
        if bottom > top {
            frame.render_widget(
                Block::default().style(style),
                Rect::new(
                    inner.x.saturating_sub(2),
                    top as u16,
                    inner.width + 2,
                    (bottom - top) as u16,
                ),
            );
        }
        let label = render::safe(account["label"].as_str().unwrap_or("Account"));
        let mut label = vec![Span::raw(format!(
            "{}{}",
            if index == state.account_selected {
                "▶ "
            } else {
                "  "
            },
            label
        ))];
        let mut metadata = Vec::new();
        let routing = render::account_routing(&account);
        if let Some(plan) = render::subscription_plan(&account) {
            label.push(Span::styled(
                format!(" · {plan}"),
                Style::default()
                    .fg(palette::MUTED)
                    .remove_modifier(Modifier::BOLD),
            ));
        }
        if account["preference"]["enabled"] == false {
            label.push(Span::raw(" "));
            label.push(Span::styled(
                " Deactivated ",
                Style::default()
                    .fg(Color::Rgb(223, 150, 144))
                    .bg(Color::Rgb(54, 37, 35)),
            ));
        }
        if render::best_account(&account, chrono::Utc::now().timestamp()) {
            label.push(Span::raw(" · "));
            label.push(Span::styled(
                " Best ",
                Style::default()
                    .fg(Color::Rgb(145, 188, 145))
                    .bg(Color::Rgb(34, 46, 36))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(count) = account["clients"]["count"].as_u64() {
            let stale = account["clients"]["observed_at"]
                .as_i64()
                .is_some_and(|at| !crate::routing::fresh(at, chrono::Utc::now().timestamp()));
            metadata.push(Span::raw(format!(
                "Clients: {count}{} · ",
                if stale { " (historical)" } else { "" }
            )));
        }
        if let Some(reason) = routing.strip_prefix("Priority") {
            label.push(Span::raw(" · "));
            label.push(Span::styled(
                " Priority ",
                Style::default()
                    .fg(Color::Rgb(223, 150, 144))
                    .bg(Color::Rgb(54, 37, 35))
                    .add_modifier(Modifier::BOLD),
            ));
            metadata.push(Span::raw(reason.trim_start_matches(" · ").to_owned()));
        } else if !routing.is_empty() {
            metadata.push(Span::raw(routing));
        }
        if account["preference"]["locked"].as_bool() == Some(true) {
            label.push(Span::raw(" "));
            label.push(Span::styled(
                " Locked ",
                Style::default()
                    .fg(Color::Rgb(217, 181, 114))
                    .bg(Color::Rgb(51, 44, 32)),
            ));
        }
        let expiring = credit_expiring_soon(&account, chrono::Utc::now().timestamp());
        if !expiring {
            metadata.push(Span::raw(format!(" · {}", credit_summary(&account))));
        }
        if y >= inner.y as i32 && y < end {
            frame.render_widget(
                Paragraph::new(Line::from(label)).style(style.add_modifier(Modifier::BOLD)),
                Rect::new(inner.x.saturating_sub(2), y as u16, inner.width + 2, 1),
            );
        }
        y += 1;
        if y >= inner.y as i32 && y < end {
            frame.render_widget(
                Paragraph::new(Line::from(metadata)).style(style.fg(palette::MUTED)),
                Rect::new(inner.x, y as u16, inner.width, 1),
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
        .chain(
            super::quota_forecast::notes(&account, chrono::Utc::now().timestamp())
                .into_iter()
                .map(|(text, warning)| {
                    Line::styled(
                        text,
                        Style::default().fg(if warning { Color::Yellow } else { Color::Gray }),
                    )
                }),
        ) {
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
                let forecast =
                    super::quota_forecast::label(&account, window, now, &state.forecasts);
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
                    frame.render_widget(
                        Paragraph::new(label).style(Style::default().fg(if stale {
                            Color::Gray
                        } else {
                            Color::White
                        })),
                        Rect::new(x, y as u16, width, 1),
                    );
                }
                if y + 1 >= inner.y as i32 && y + 1 < end {
                    super::account_details::meter(
                        frame.buffer_mut(),
                        Rect::new(x, (y + 1) as u16, width, 1),
                        if stale { None } else { remaining },
                    );
                }
                let reset_y = y + 2;
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
                            .style(Style::default().fg(Color::Gray)),
                        Rect::new(x, reset_y as u16, width, 1),
                    );
                }
            }
            y += 3;
        }
        if account["platform"].is_object() {
            let p = &account["platform"]["balance"];
            let remaining = p["exetrouter"]["remaining_usd_nanos"]
                .as_i64()
                .map(|n| n as f64 / 1e9);
            let budget = p["initial_usd_nanos"].as_i64().map(|n| n as f64 / 1e9);
            if y >= inner.y as i32 && y < end {
                frame.render_widget(
                    Paragraph::new(crate::platform::balance::headline(&account["platform"])),
                    Rect::new(inner.x, y as u16, inner.width, 1),
                );
            }
            if y + 1 >= inner.y as i32 && y + 1 < end {
                super::account_details::cost_meter(
                    frame.buffer_mut(),
                    Rect::new(inner.x, (y + 1) as u16, inner.width, 1),
                    remaining
                        .zip(budget)
                        .filter(|(_, b)| *b > 0.)
                        .map(|(s, b)| s / b * 100.),
                );
            }
            if y + 2 >= inner.y as i32 && y + 2 < end {
                frame.render_widget(
                    Paragraph::new(if p.is_object() {
                        format!(
                            "ExetRouter only · {} unpriced requests",
                            p["exetrouter"]["unpriced_requests"].as_i64().unwrap_or(0)
                        )
                    } else {
                        "Manual balance not set".into()
                    })
                    .style(Style::default().fg(Color::Gray)),
                    Rect::new(inner.x, (y + 2) as u16, inner.width, 1),
                );
            }
            y += 3;
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
fn usage_axis_max(value: f64) -> f64 {
    let magnitude = 10_f64.powf(value.max(1.).log10().floor());
    let normalized = value / magnitude;
    let rounded = [1., 2., 5., 10.]
        .into_iter()
        .find(|step| *step >= normalized)
        .unwrap_or(10.);
    rounded * magnitude
}

fn all_usage_series_available(state: &State) -> bool {
    state.group != 0
        && state.values[USAGE]
            .as_ref()
            .and_then(|value| value["rows"].as_array())
            .is_some_and(|rows| (2..=8).contains(&rows.len()))
}
fn usage_rows(state: &State, value: &Value) -> Vec<Value> {
    let rows = value["rows"].as_array().cloned().unwrap_or_default();
    if state.group != 0
        && !(state.usage_all && all_usage_series_available(state))
        && !rows.is_empty()
    {
        vec![rows[state.usage_selected % rows.len()].clone()]
    } else {
        rows
    }
}
fn usage_breakdown(state: &State) -> String {
    let Some(value) = &state.values[USAGE] else {
        return "No data yet. Close and refresh Usage.".into();
    };
    let report = serde_json::json!({"period":value["period"],"from_utc":value["from_utc"],"to_utc":value["to_utc"],"rows":usage_rows(state,value)});
    let text = render::response("usage", &report);
    text.split_once('\n')
        .map_or(text.clone(), |(_, body)| body.to_owned())
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
    let rows = usage_rows(state, value);
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
    let palette = super::account_details::COLORS;
    let mut identities: Vec<_> = value["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["name"].as_str())
        .collect();
    identities.sort_unstable();
    identities.dedup();
    let color = |name: &str| {
        palette[(identities.iter().position(|n| *n == name).unwrap_or(0)
            + usize::from(!state.request_metric))
            % palette.len()]
    };
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
                Span::raw("   ·   "),
                Span::styled(
                    match rows
                        .iter()
                        .filter_map(|r| r["estimated_usd"].as_f64())
                        .reduce(|a, b| a + b)
                    {
                        Some(usd) => format!(
                            "{} API equivalent{}",
                            crate::money::usd(usd),
                            if rows
                                .iter()
                                .any(|r| r["unpriced_requests"].as_u64().unwrap_or(1) > 0)
                            {
                                " (partial)"
                            } else {
                                ""
                            }
                        ),
                        None if rows.is_empty() => "$0 API equivalent".into(),
                        None => "API equivalent unknown".into(),
                    },
                    Style::default().fg(Color::Gray),
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
            Line::from(
                rows.iter()
                    .map(|r| {
                        Span::styled(
                            format!("● {}   ", render::safe(render::usage_name(r))),
                            Style::default().fg(color(r["name"].as_str().unwrap_or("all"))),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        ]),
        chunks[0],
    );
    let timeline: Vec<_> = value["timeline"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| {
            b["from_utc"]
                .as_i64()
                .is_some_and(|at| at <= chrono::Utc::now().timestamp())
        })
        .cloned()
        .collect();
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
                .map(|(i, b)| usage_metric(b, name, state).map(|v| (i as f64, v)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let max = usage_axis_max(
        series
            .iter()
            .flatten()
            .flatten()
            .map(|(_, v)| *v)
            .fold(1., f64::max),
    );
    let mut segments = Vec::new();
    for (i, line) in series.iter().enumerate() {
        let mut points = Vec::new();
        for p in line {
            if let Some(p) = p {
                points.push(*p)
            } else if !points.is_empty() {
                segments.push((i, std::mem::take(&mut points)));
            }
        }
        if !points.is_empty() {
            segments.push((i, points));
        }
    }
    let last = timeline.len().saturating_sub(1);
    let selected = state
        .usage_cursor
        .and_then(|at| {
            timeline
                .iter()
                .position(|b| b["from_utc"].as_i64() == Some(at))
        })
        .unwrap_or(last);
    let cursor = [(selected as f64, 0.0), (selected as f64, max)];
    let grids: Vec<_> = (1..=4)
        .map(|i| {
            [
                (0.0, max * i as f64 / 4.0),
                (last.max(1) as f64, max * i as f64 / 4.0),
            ]
        })
        .collect();
    let mut datasets: Vec<_> = grids
        .iter()
        .map(|points| {
            Dataset::default()
                .data(points)
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::DarkGray))
        })
        .collect();
    datasets.push(
        Dataset::default()
            .data(&cursor)
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::DarkGray)),
    );
    datasets.extend(segments.iter().map(|(i, data)| {
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(if data.len() == 1 {
                GraphType::Scatter
            } else {
                GraphType::Line
            })
            .style(Style::default().fg(color(&names[*i])))
            .data(data)
    }));
    let labels = [0, last / 2, last]
        .into_iter()
        .map(|i| {
            let label = timeline[i]["from_utc"]
                .as_i64()
                .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format(match state.period {
                            0 => "%b %-d %H:%M",
                            _ => "%b %-d",
                        })
                        .to_string()
                })
                .unwrap_or_default();
            Line::from(label)
        })
        .collect::<Vec<_>>();
    let metric = if state.usd_metric {
        "Standard API equivalent (USD)"
    } else if !state.request_metric {
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
                    .labels([0., 0.25, 0.5, 0.75, 1.].map(|ratio| {
                        Line::from(if state.usd_metric {
                            crate::money::usd(max * ratio)
                        } else {
                            format!("{}", max * ratio)
                        })
                    }))
                    .style(Style::default().fg(Color::Gray)),
            ),
        chunks[1],
    );
    let note = if state.usd_metric {
        "Published standard API price estimate, not subscription spending. Unpriced requests are excluded, not $0.".into()
    } else if unknown > 0 {
        format!("{unknown} requests have incomplete counts. The token graph shows reported counts only.")
    } else {
        "Cached input and reasoning output are already included in token totals.".into()
    };
    let bucket = &timeline[selected];
    let at = if state.period == 0 {
        bucket["to_utc"]
            .as_i64()
            .or_else(|| bucket["from_utc"].as_i64())
    } else {
        bucket["from_utc"].as_i64()
    };
    let label = at
        .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format(if state.period == 0 {
                    "%b %-d, %I:%M %p"
                } else {
                    "%b %-d"
                })
                .to_string()
        })
        .unwrap_or_else(|| "Unknown time".into());
    let values = names.iter().map(|name| {
        let rows: Vec<_> = bucket["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["name"].as_str() == Some(name))
            .collect();
        let value = if state.usd_metric {
            usage_metric(bucket, name, state)
                .map(|v| {
                    format!(
                        "{}{}",
                        crate::money::usd(v),
                        if rows
                            .iter()
                            .any(|r| r["unpriced_requests"].as_u64().unwrap_or(1) > 0)
                        {
                            " (partial)"
                        } else {
                            ""
                        }
                    )
                })
                .unwrap_or_else(|| "Unknown".into())
        } else if state.request_metric {
            rows.iter()
                .filter_map(|r| r["requests"].as_i64())
                .sum::<i64>()
                .to_string()
        } else if !rows.is_empty()
            && rows
                .iter()
                .all(|r| r["input_tokens"].is_null() && r["output_tokens"].is_null())
        {
            "Unknown".into()
        } else {
            rows.iter()
                .flat_map(|r| [r["input_tokens"].as_i64(), r["output_tokens"].as_i64()])
                .flatten()
                .sum::<i64>()
                .to_string()
        };
        Span::styled(
            format!(
                "  {value} {}",
                if state.usd_metric {
                    "estimated"
                } else if state.request_metric {
                    "requests"
                } else {
                    "tokens"
                }
            ),
            Style::default().fg(color(name)),
        )
    });
    let selection = Line::from(
        std::iter::once(Span::styled(label, Style::default().fg(Color::White)))
            .chain(values)
            .collect::<Vec<_>>(),
    );
    frame.render_widget(
        Paragraph::new(vec![
            selection,
            Line::from(format!(
                "{} · {}",
                render::system_timezone(),
                if state.period == 0 {
                    "Hourly activity"
                } else {
                    "Daily activity"
                }
            )),
            Line::from(note),
        ])
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(if unknown > 0 {
            Color::Yellow
        } else {
            Color::Gray
        })),
        chunks[2],
    );
}
fn seek_usage(state: &mut State, key: KeyEvent) -> bool {
    if state.tab != USAGE
        || !matches!(key.code, KeyCode::Home | KeyCode::End)
            && !(matches!(key.code, KeyCode::Left | KeyCode::Right)
                && key.modifiers.contains(KeyModifiers::SHIFT))
    {
        return false;
    }
    let hours: Vec<_> = state.values[USAGE]
        .as_ref()
        .into_iter()
        .flat_map(|v| v["timeline"].as_array())
        .flatten()
        .filter_map(|b| b["from_utc"].as_i64())
        .filter(|at| *at <= chrono::Utc::now().timestamp())
        .collect();
    if !hours.is_empty() {
        let last = hours.len() - 1;
        let current = state
            .usage_cursor
            .and_then(|at| hours.iter().position(|h| *h == at))
            .unwrap_or(last);
        let index = match key.code {
            KeyCode::Home => 0,
            KeyCode::End => last,
            KeyCode::Left => current.saturating_sub(1),
            _ => (current + 1).min(last),
        };
        state.usage_cursor = Some(hours[index]);
    }
    true
}
fn usage_value(bucket: &Value, name: &str, requests: bool) -> Option<f64> {
    let rows: Vec<_> = bucket["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["name"].as_str() == Some(name))
        .collect();
    if requests {
        return Some(
            rows.iter()
                .filter_map(|r| r["requests"].as_i64())
                .sum::<i64>() as f64,
        );
    }
    if !rows.is_empty()
        && rows
            .iter()
            .all(|r| r["input_tokens"].is_null() && r["output_tokens"].is_null())
    {
        return None;
    }
    Some(
        rows.iter()
            .flat_map(|r| [r["input_tokens"].as_i64(), r["output_tokens"].as_i64()])
            .flatten()
            .sum::<i64>() as f64,
    )
}
fn usage_metric(bucket: &Value, name: &str, state: &State) -> Option<f64> {
    if !state.usd_metric {
        return usage_value(bucket, name, state.request_metric);
    }
    let rows: Vec<_> = bucket["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["name"].as_str() == Some(name))
        .collect();
    if rows.is_empty() {
        return Some(0.);
    }
    let priced: Vec<_> = rows
        .iter()
        .filter_map(|r| r["estimated_usd"].as_f64())
        .filter(|v| v.is_finite() && *v >= 0.)
        .collect();
    (!priced.is_empty()).then(|| priced.iter().sum())
}

fn page_usage(state: &mut State, older: bool) {
    let seconds = [86400, 7 * 86400, 30 * 86400, 90 * 86400][state.period];
    let now = chrono::Utc::now().timestamp();
    let end = state
        .usage_before
        .or_else(|| {
            state.values[USAGE]
                .as_ref()
                .and_then(|v| v["to_utc"].as_i64())
        })
        .unwrap_or(now);
    state.usage_before = if older {
        Some(end.saturating_sub(seconds).max(seconds))
    } else {
        end.checked_add(seconds).filter(|end| *end < now)
    };
    state.usage_cursor = None;
    state.values[USAGE] = None;
}
fn keyboard_help(state: &State, connection: &Connection) -> String {
    let actions = match state.tab {
        OVERVIEW => "↑/↓: select account\nPgUp/PgDn: page\nEnter: subscription history, billing and clients\nAccount availability is managed by the operator.",
        USAGE if state.group == 0 => "p: rolling 24-hour / 7-day / 30-day / 90-day range\n[/]: previous / next range\nb: change grouping\nm: tokens / requests / USD estimate\nEnter: detailed counter breakdown",
        USAGE => "↑/↓: select user, model or token with recorded usage\na: toggle all series (up to eight groups)\nEnter: detailed counter breakdown\np: change rolling period\n[/]: previous / next range\nb: change grouping\nm: tokens / requests / USD estimate",
        TOKENS => "↑/↓: select token; PgUp/PgDn: page; Home/End: first/last\nEnter: token actions\ns: search names and IDs; 0: clear search; c: copy token ID\nn: create; l: rename; o: rotate; x: revoke\nRotation and revocation require confirmation.",
        MODELS => "↑/↓: select model\nPgUp/PgDn: page\nEnter: copy selected model ID\ns: search names, IDs and capabilities\nf: require capabilities (match all)\n0: clear search and capability filters",
        SETTINGS if connection.local.is_some() => "↑/↓: select settings actions or account\nEnter: activate selected action; reauthorize selected account\ne: configure connection; v: check updates; U: update\na: add account; u: reauthorize; d: disable",
        _ => "↑/↓: select settings actions\nEnter: activate selected action\ne: configure connection; v: check updates; U: update",
    };
    format!("{}\n\n←/→ or 1–5: switch views\nr: refresh this view\n? or /: help\nq / Ctrl-C: quit\n\nClick accounts to open details. Wheel selects items.\nEN/RU shortcuts work without changing typed text.\nShift + drag usually selects terminal text.\n\n{actions}", TABS[state.tab])
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
fn account_popup(area: Rect) -> Rect {
    let mut result = popup(area);
    result.height = area.height.saturating_sub(6).min(36);
    result.y = area.y + (area.height - result.height) / 2;
    result
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
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let result = async {
    let mut state = State::default();
    let mut private_settings_available = true;
    match super::privacy::dashboard(&args).await {
        Ok(Some(settings)) => { state.tab=settings.tab; state.period=settings.rolling_period.unwrap_or([0,0,1,2][settings.period]); state.group=settings.group; state.request_metric=settings.request_metric; state.usd_metric=settings.usd_metric; },
        Ok(None) => {},
        Err(_) => {private_settings_available=false; state.statuses[SETTINGS]="Private settings unavailable; restore the original privacy key and check server compatibility.".into();},
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
                Ok(value) if job.history_account.is_some() => {
                    if let Err(error) = replace_history(&mut state, job.history_account.unwrap(), value) {
                        state.statuses[OVERVIEW] = error.to_string();
                    }
                }
                Ok(mut value) if job.mutation => {
                    if job.tab == TOKENS {
                        // Usage labels are resolved when the report is loaded.
                        state.values[USAGE] = None;
                    }
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
                        state.notices[job.tab] = Some("Token updated.".into());
                    } else {
                        state.notices[job.tab] = Some(render::response("revoke", &value));
                    }
                    let refresh_tab=job.tab;
                    if state.modal.is_none() {
                        let request=if matches!(value.get("updated").and_then(Value::as_bool),Some(true)){request(&state)}else{ControlRequest::TokenList};
                        pending = Some(start(&args.connection,request,refresh_tab,false));
                    }
                }
                Ok(mut value) => {
                    if job.tab == OVERVIEW && (state.history_period != 0 || state.history_before.is_some()) {
                        if let Some(id) = state.history_account {
                            if let Some(history) = account_row(&state,id).map(|row|row["quota_history"].clone()) {
                                if let Some(row) = value["quota_accounts"].as_array_mut().and_then(|rows|rows.iter_mut().find(|row|row["id"].as_i64()==Some(id))) {
                                    row["quota_history"] = history;
                                }
                            }
                        }
                    }
                    if job.tab == OVERVIEW {
                        state.forecasts.observe(&value, chrono::Utc::now().timestamp());
                    }
                    preserve_selection(&mut state, job.tab, &value);
                    if job.tab == SETTINGS {
                        state.selected = state
                            .selected
                            .min(value.as_array().map_or(0, |v| v.len().saturating_sub(1)));
                        let actions = settings_actions(&state, &args.connection).len();
                        let count = value.as_array().map_or(0, Vec::len);
                        state.settings_selected = state.settings_selected.min((actions + count).saturating_sub(1));
                        if state.settings_selected >= actions { state.selected = state.settings_selected - actions; }
                    }
                    state.values[job.tab] = Some(value);
                    if job.tab == OVERVIEW && state.tab == OVERVIEW {
                        let size = terminal.size()?;
                        let viewport = overview_viewport(size.height, size.width, &state, false);
                        keep_selected_account_visible(&mut state, viewport, size.width.saturating_sub(4));
                    } else if job.tab == SETTINGS && state.tab == SETTINGS {
                        let size = terminal.size()?;
                        let viewport = overview_viewport(size.height, size.width, &state, false);
                        keep_settings_selection_visible(&mut state, &args.connection, viewport, size.width.saturating_sub(2));
                    }
                    state.statuses[job.tab] = format!(
                        "Updated {} · {}",
                        TABS[job.tab],
                        chrono::Local::now().format("%H:%M:%S")
                    );
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
            && (state.modal.is_none() || matches!(state.modal,Some(Modal::Account{..})))
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
        let mut hits = Vec::new();
        terminal.draw(|frame| { hits = render_hits(frame, &state, &args.connection, pending.as_ref().is_some_and(|job| job.tab == state.tab) || (state.tab == SETTINGS && updater.is_some())); })?;
        if !event::poll(Duration::from_millis(50))? {
            tokio::task::yield_now().await;
            continue;
        }
        let mut key = match event::read()? {
            Event::Key(key) => key,
            Event::Resize(width,height) => {
                let viewport = overview_viewport(height, width, &state, pending.as_ref().is_some_and(|job| job.tab == state.tab));
                if state.tab == SETTINGS { keep_settings_selection_visible(&mut state, &args.connection, viewport, width.saturating_sub(2)); }
                if state.tab == OVERVIEW { keep_selected_account_visible(&mut state, viewport, width.saturating_sub(4)); }
                continue;
            }
            Event::Mouse(mouse) if !pending.as_ref().is_some_and(|p| p.mutation) && !updater.as_ref().is_some_and(|job| job.installing) => {
                let Some(key) = mouse_key(mouse, &hits, &mut state) else { continue; };
                key
            }
            _ => continue,
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        key = input_key(&state, key);
        if updater.as_ref().is_some_and(|job| job.installing) { continue; }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if pending.as_ref().is_some_and(|p| p.mutation) {
                return Err("Mutation interrupted; it may have completed. Inspect tokens or limits before any further action".into());
            }
            if private_settings_available {super::privacy::save_dashboard(&args, &super::privacy::Dashboard {tab:state.tab,period:state.period,group:state.group,request_metric:state.request_metric,usd_metric:state.usd_metric,rolling_period:Some(state.period)}).await?;}
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
                    | KeyCode::Char('1'..='5')
            )
            && !(state.tab == SETTINGS && matches!(key.code, KeyCode::Enter | KeyCode::Char('e' | 'v')))
        {
            continue;
        }
        let mut menu_action=false;
        if state.tab == SETTINGS && state.modal.is_none() && key.code == KeyCode::Enter {
            let actions = settings_actions(&state, &args.connection);
            if let Some((_, action)) = actions.get(state.settings_selected) {
                key.code = *action;
                menu_action = true;
            } else if args.connection.local.is_some() {
                state.modal = account_actions(&state);
                continue;
            }
        }
        if let Some(Modal::Actions { entries, selected, .. }) = &mut state.modal {
            match key.code {
                KeyCode::Up => { *selected=selected.saturating_sub(1);continue; }
                KeyCode::Down => { *selected=(*selected+1).min(entries.len().saturating_sub(1));continue; }
                KeyCode::Esc => { state.modal=None;continue; }
                KeyCode::Enter => { key.code=entries[*selected].1;state.modal=None;menu_action=true; }
                code if entries.iter().any(|(_, shortcut)| *shortcut == code) => { state.modal=None;menu_action=true; }
                _ => continue,
            }
        }
        let mut mutation = None;
        if let Some(modal) = &mut state.modal {
            match modal {
                Modal::Usage { scroll } => match key.code {
                    KeyCode::Esc => state.modal = None,
                    KeyCode::Up | KeyCode::PageUp => *scroll = scroll.saturating_sub(if key.code == KeyCode::PageUp {10} else {1}),
                    KeyCode::Down | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
                        let current = *scroll;
                        let area = account_popup(Rect::new(0,0,size.width,size.height));
                        let max = Paragraph::new(usage_breakdown(&state)).wrap(Wrap {trim:false}).line_count(area.width.saturating_sub(4)).saturating_sub(usize::from(area.height.saturating_sub(4))).min(usize::from(u16::MAX)) as u16;
                        let next = match key.code {KeyCode::Home=>0,KeyCode::End=>max,KeyCode::PageDown=>current.saturating_add(10).min(max),_=>current.saturating_add(1).min(max)};
                        state.modal = Some(Modal::Usage {scroll:next});
                    }
                    _ => {}
                },
                Modal::TokenPolicy { id, enabled, fast } => match key.code {
                    KeyCode::Esc => state.modal=None,
                    KeyCode::Char('y') => {
                        mutation=Some(if let Some(enabled)=enabled {ControlRequest::TokenSetEnabled{id:id.clone(),enabled:*enabled}} else {ControlRequest::TokenFastMode{id:id.clone(),enabled:fast.unwrap_or(None)}});
                        state.modal=None;
                    }
                    _=>{}
                },
                Modal::Search { kind, input } => match key.code {
                    KeyCode::Esc => state.modal = None,
                    KeyCode::Enter => {
                        let kind = *kind;
                        let query = input.value.clone();
                        apply_search(&mut state, kind, &query);
                        state.modal = None;
                    }
                    _ => { if input.value.len() < 512 || !matches!(key.code, KeyCode::Char(_)) { input.edit(key); } }
                },
                Modal::Account { id, scroll } => match key.code {
                    KeyCode::Esc => state.modal = None,
                    KeyCode::Down | KeyCode::PageDown => {
                        let step=if key.code==KeyCode::PageDown {10} else {1};
                        let account=*id;
                        let next=scroll.saturating_add(step);
                        let size=terminal.size()?;
                        let area=account_popup(Rect::new(0,0,size.width,size.height));
                        let max=account_row(&state,account).map_or(1,|row|super::account_details::height(row,chrono::Utc::now().timestamp(),area.width.saturating_sub(4))).saturating_sub(area.height.saturating_sub(4) as usize).min(u16::MAX as usize) as u16;
                        state.modal=Some(Modal::Account{id:account,scroll:next.min(max)});
                    }
                    KeyCode::Up | KeyCode::PageUp => *scroll=scroll.saturating_sub(if key.code==KeyCode::PageUp {10} else {1}),
                    KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                        let id=*id;
                        if let Some((from,to,interval))=account_row(&state,id).and_then(super::account_details::range) {
                            let last=to-interval;
                            let current=state.account_hour.unwrap_or(last);
                            state.account_hour=Some(match key.code {KeyCode::Home=>from,KeyCode::End=>last,KeyCode::Left=>(current-interval).clamp(from,last),_=>(current+interval).clamp(from,last)});
                        }
                    }
                    KeyCode::Char('r') if pending.is_none() => pending=Some(start(&args.connection,ControlRequest::Limits,OVERVIEW,false)),
                    KeyCode::Char('h' | '[' | ']') if pending.is_none() => {
                        let account = *id;
                        let end = account_row(&state,account).and_then(super::account_details::range).map(|(_,to,_)|to);
                        let latest = chrono::Utc::now().timestamp().div_euclid(600)*600+600;
                        if key.code == KeyCode::Char('h') {
                            state.history_period=(state.history_period+1)%HISTORY_PERIODS.len();
                            state.history_before=None;
                        } else {
                            let to=state.history_before.or(end).unwrap_or(latest);
                            let span=HISTORY_PERIODS[state.history_period].1;
                            state.history_before=if key.code==KeyCode::Char('[') { Some(to-span) } else if to+span>=latest {None} else {Some(to+span)};
                        }
                        state.history_account=Some(account);
                        pending=Some(start_history(&args.connection,&state,account));
                    }
                    _ => {}
                },
                Modal::Actions { .. } => unreachable!("action menus handled above"),
                Modal::UpdateConfirm => match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => state.modal = None,
                    KeyCode::Enter | KeyCode::Char('y') => {
                        updater = Some(update_job(true)); state.update_checked = true;
                        state.modal = None; state.statuses[state.tab] = "Installing update… Keep this dashboard open until completion.".into();
                        state.notices[state.tab] = Some("Updating exr; source builds may take several minutes.".into());
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
                Modal::Rename { id, input, error } => match key.code {
                    KeyCode::Esc => state.modal = None,
                    KeyCode::Enter => match token_rename_request(id, input) {
                        Ok(request) => {
                            mutation = Some(request);
                            state.modal = None;
                        }
                        Err(reason) => *error = reason.to_string(),
                    },
                    _ => input.edit(key),
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
            if seek_usage(&mut state,key) {continue}
            if navigate_tokens(&mut state, key.code, viewport) {continue}
            let old_tab = state.tab;
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if private_settings_available {super::privacy::save_dashboard(&args, &super::privacy::Dashboard {tab:state.tab,period:state.period,group:state.group,request_metric:state.request_metric,usd_metric:state.usd_metric,rolling_period:Some(state.period)}).await?;}
                    if let Some(local) = &args.connection.local {
                        local.stop().await?;
                    }
                    return Ok(());
                }
                KeyCode::Char('?') => state.modal = Some(Modal::Help),
                KeyCode::Enter if state.tab == USAGE => state.modal = Some(Modal::Usage { scroll:0 }),
                KeyCode::Char('a') if state.tab == USAGE && all_usage_series_available(&state) => state.usage_all = !state.usage_all,
                KeyCode::Right => state.tab = (state.tab + 1) % TABS.len(),
                KeyCode::Left => state.tab = (state.tab + TABS.len() - 1) % TABS.len(),
                KeyCode::Char(c @ '1'..='5') => state.tab = (c as u8 - b'1') as usize,
                KeyCode::Char('v') if state.tab == SETTINGS && updater.is_none() => {
                    updater = Some(update_job(false)); state.update_checked = true;
                    state.statuses[state.tab] = "Checking GitHub releases…".into();
                }
                KeyCode::Char('U') if (menu_action || state.modal.is_none()) && state.tab == SETTINGS && updater.is_none() && state.update.as_ref().is_some_and(|report| report.available) => { state.modal = Some(Modal::UpdateConfirm); }
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
                    let actions = settings_actions(&state, &args.connection).len();
                    state.settings_selected = (state.settings_selected + 1).min((actions + count).saturating_sub(1));
                    if state.settings_selected >= actions {
                        state.selected = state.settings_selected - actions;
                    }
                    keep_settings_selection_visible(&mut state, &args.connection, viewport, size.width.saturating_sub(2));
                }
                KeyCode::Up if state.tab == SETTINGS => {
                    state.settings_selected = state.settings_selected.saturating_sub(1);
                    let actions = settings_actions(&state, &args.connection).len();
                    if state.settings_selected >= actions {
                        state.selected = state.settings_selected - actions;
                    }
                    keep_settings_selection_visible(&mut state, &args.connection, viewport, size.width.saturating_sub(2));
                }
                KeyCode::Char('r') => {
                    pending = Some(start(&args.connection, request(&state), state.tab, false))
                }
                KeyCode::Char('s') if matches!(state.tab, MODELS | TOKENS) => {
                    let (kind, query) = if state.tab == MODELS {(SearchKind::Models, &state.model_query)} else {(SearchKind::Tokens, &state.token_query)};
                    state.modal = Some(Modal::Search { kind, input: TextInput::new(query.clone()) });
                }
                KeyCode::Char('f') if state.tab == MODELS => {
                    let entries = [("reasoning",'R'),("tools",'T'),("image_input",'V'),("image_generation",'G'),("image_editing",'E')].into_iter().map(|(id,key)| (format!("{}{}",render::capability_label(id).unwrap(),if state.model_features.iter().any(|v| v==id) {" ✓"}else{""}),KeyCode::Char(key))).collect();
                    state.modal = Some(Modal::Actions { title:"Capabilities · match all".into(), entries, selected:0 });
                }
                KeyCode::Char(c @ ('R'|'T'|'V'|'G'|'E')) if state.tab == MODELS => {
                    let id = match c { 'R'=>"reasoning",'T'=>"tools",'V'=>"image_input",'G'=>"image_generation",_=>"image_editing" };
                    if state.model_features.iter().any(|v| v==id) { state.model_features.retain(|v| v!=id); } else { state.model_features.push(id.into()); }
                    state.model_selected=0;
                }
                KeyCode::Char('0') if state.tab == MODELS => {
                    state.model_query.clear(); state.model_features.clear(); state.model_selected=0;
                }
                KeyCode::Enter if matches!(state.tab,OVERVIEW|TOKENS) => {
                    if state.tab==OVERVIEW {
                        if let Some(id)=state.values[OVERVIEW].as_ref().and_then(|v|v["quota_accounts"].as_array()).and_then(|rows|rows.get(state.account_selected)).and_then(|row|row["id"].as_i64()) {
                            state.account_hour=None;
                            if state.history_account != Some(id) { state.history_period=0;state.history_before=None; }
                            state.history_account=Some(id);state.modal=Some(Modal::Account{id,scroll:0});
                            pending=Some(start_history(&args.connection,&state,id));
                        }
                    } else {
                        let (title,entries)=action_menu(&state,&args.connection);
                        state.modal=Some(Modal::Actions{title,entries,selected:0});
                    }
                }
                KeyCode::Char('n') if state.tab == TOKENS => {
                    state.modal = Some(Modal::Name(String::new()))
                }
                KeyCode::Char('0') if state.tab == TOKENS => apply_search(&mut state, SearchKind::Tokens, ""),
                KeyCode::Char('c') if state.tab == TOKENS => {
                    if let Some(id) = selected_token(&state).and_then(|row| row["id"].as_str()).map(str::to_owned) {
                        state.notices[TOKENS] = Some(match Clipboard::detect() {
                            Ok(clipboard) => match clipboard.copy(&id).await {
                                Ok(()) => format!("Copied {id} to clipboard."),
                                Err(error) => format!("Could not copy token ID: {error}"),
                            },
                            Err(error) => format!("Could not copy token ID: {error}"),
                        });
                    }
                }
                KeyCode::Char('d') if state.tab == TOKENS => {
                    if let Some(row)=selected_token(&state).filter(|row|token_policy_available(row)) {
                        if let Some(id)=row["id"].as_str() { state.modal=Some(Modal::TokenPolicy{id:id.into(),enabled:Some(row["disabled"]==true),fast:None}); }
                    }
                }
                KeyCode::Char('f') if state.tab == TOKENS => {
                    if selected_token(&state).is_some_and(token_policy_available) {
                        state.modal=Some(Modal::Actions{title:"Fast mode".into(),entries:vec![("Default".into(),KeyCode::Char('D')),("Allow".into(),KeyCode::Char('A')),("Block".into(),KeyCode::Char('B'))],selected:0});
                    }
                }
                KeyCode::Char(c @ ('D'|'A'|'B')) if state.tab == TOKENS => {
                    if let Some(id)=selected_token(&state).filter(|row|token_policy_available(row)).and_then(|row|row["id"].as_str()) {
                        state.modal=Some(Modal::TokenPolicy{id:id.into(),enabled:None,fast:Some(match c {'A'=>Some(true),'B'=>Some(false),_=>None})});
                    }
                }
                KeyCode::Char('l') if state.tab == TOKENS => {
                    state.modal = token_rename_modal(&state);
                }
                KeyCode::Char(c @ ('o' | 'x')) if state.tab == TOKENS => {
                    if let Some(id) = selected_token(&state)
                        .and_then(|row| row["id"].as_str())
                    {
                        state.modal = Some(Modal::Confirm {
                            id: id.into(),
                            rotate: c == 'o',
                        });
                    }
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
                    state.usage_all = false;
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
                    for _ in 0..viewport.saturating_sub(model_filter_height(&state)).checked_div(4).unwrap_or(0).max(1) {
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
                    if state.usd_metric {
                        state.usd_metric = false;
                        state.request_metric = false;
                    } else if state.request_metric {
                        state.request_metric = false;
                        state.usd_metric = true;
                    } else {
                        state.request_metric = true;
                    }
                }
                KeyCode::Char('p') if state.tab == USAGE => {
                    state.period = (state.period + 1) % USAGE_PERIODS.len();
                    state.usage_before = None;
                    state.usage_cursor = None;
                    state.usage_selected = 0;
                    state.values[USAGE] = None;
                    pending = Some(start(&args.connection, request(&state), state.tab, false));
                }
                KeyCode::Char('[' | ']') if state.tab == USAGE => {
                    page_usage(&mut state, key.code == KeyCode::Char('['));
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
                let viewport = overview_viewport(size.height, size.width, &state, false);
                if state.tab == OVERVIEW {
                    keep_selected_account_visible(&mut state, viewport, size.width.saturating_sub(4));
                } else if state.tab == SETTINGS {
                    keep_settings_selection_visible(&mut state, &args.connection, viewport, size.width.saturating_sub(2));
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
            pending = Some(start(&args.connection, req, TOKENS, true));
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
                render::safe(row["email"].as_str().unwrap_or("Email unavailable")),
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
            let key = shortcut_key(key);
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
    let key = shortcut_key(key);
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
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
        let key = if key.modifiers.contains(KeyModifiers::CONTROL) {
            shortcut_key(key)
        } else {
            key
        };
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
    fn place_cursor(&mut self, column: usize) {
        self.cursor = self
            .value
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(self.value.len()))
            .take_while(|index| Line::from(&self.value[..*index]).width() <= column)
            .last()
            .unwrap_or(0);
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
    execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    println!("Checking SSH connection. Verify any new host fingerprint with your operator before accepting it. Ctrl-C cancels setup.");
    let result = super::remote_request(connection, ControlRequest::Doctor, true).await;
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
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
    #[test]
    fn model_search_and_all_capability_filters_share_cli_semantics_and_copy_exact_ids() {
        let mut state = State {
            tab: MODELS,
            model_query: "sol".into(),
            model_features: vec!["reasoning".into(), "tools".into()],
            ..Default::default()
        };
        let value = serde_json::json!({"data":[
            {"id":"sol-base","display_name":"Sol Base","source":"codex","capabilities":["tools"]},
            {"id":"gpt-sol","display_name":"GPT Sol","source":"codex","capabilities":["tools","reasoning"]},
            {"id":"platform-sol","display_name":"Platform Sol","source":"platform","capabilities":["tools","reasoning"]}
        ]});
        state.values[MODELS] = Some(value.clone());
        assert_eq!(filtered_model_rows(&state, &value).len(), 2);
        assert_eq!(selected_model_id(&state), Some("gpt-sol"));
        state.model_selected = 1;
        assert_eq!(selected_model_id(&state), Some("platform-sol"));
        state.model_query = "missing".into();
        assert_eq!(selected_model_id(&state), None);
    }
    #[test]
    fn account_history_accepts_ninety_days_and_retains_other_account_metadata() {
        let mut state = State::default();
        state.values[OVERVIEW] = Some(
            serde_json::json!({"quota_accounts":[{"id":1,"label":"Synthetic","subscription":{"billing_day":8},"quota":{"windows":[]}}]}),
        );
        let from = 1_800_000_000;
        let history = serde_json::json!({"from_utc":from,"to_utc":from+90*86400,"interval_seconds":600,"points":[]});
        replace_history(&mut state, 1, history.clone()).unwrap();
        let row = account_row(&state, 1).unwrap();
        assert_eq!(row["subscription"]["billing_day"], 8);
        assert_eq!(row["quota_history"], history);
        assert!(replace_history(&mut state,1,serde_json::json!({"from_utc":from,"to_utc":from+91*86400,"interval_seconds":600,"points":[]})).is_err());
        assert_eq!(account_row(&state, 1).unwrap()["quota_history"], history);
    }
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn shortcuts_accept_russian_layout_and_both_help_keys_without_changing_text() {
        let state = State::default();
        for (typed, expected) in [
            ('й', 'q'),
            ('к', 'r'),
            ('т', 'n'),
            ('н', 'y'),
            ('с', 'c'),
            ('Ы', 'S'),
            ('В', 'D'),
            ('Д', 'L'),
            ('Т', 'N'),
            ('З', 'P'),
            ('Г', 'U'),
            ('/', '?'),
            ('?', '?'),
            ('.', '?'),
            (',', '?'),
        ] {
            assert_eq!(
                input_key(
                    &state,
                    KeyEvent::new(KeyCode::Char(typed), KeyModifiers::NONE)
                )
                .code,
                KeyCode::Char(expected)
            );
        }
        for modal in [
            Modal::Name(String::new()),
            Modal::Search {
                kind: SearchKind::Tokens,
                input: TextInput::new(String::new()),
            },
            Modal::Search {
                kind: SearchKind::Models,
                input: TextInput::new(String::new()),
            },
            Modal::Rename {
                id: "tok_fixture".into(),
                input: TextInput::new(String::new()),
                error: String::new(),
            },
        ] {
            let state = State {
                modal: Some(modal),
                ..Default::default()
            };
            for typed in ['н', 'т', '/', '.', ','] {
                let key = KeyEvent::new(KeyCode::Char(typed), KeyModifiers::NONE);
                assert_eq!(input_key(&state, key), key);
            }
            assert_eq!(
                input_key(
                    &state,
                    KeyEvent::new(KeyCode::Char('с'), KeyModifiers::CONTROL)
                )
                .code,
                KeyCode::Char('c')
            );
        }
    }

    #[test]
    fn every_dashboard_button_shows_its_shortcut_and_wraps_without_duplicate_hints() {
        for tab in 0..TABS.len() {
            for width in [72, 120] {
                let state = State {
                    tab,
                    period: 1,
                    group: 3,
                    ..Default::default()
                };
                let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
                let mut hits = Vec::new();
                terminal
                    .draw(|frame| {
                        hits = render_hits(frame, &state, &Connection::default(), false);
                    })
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                for (label, key) in dashboard_buttons(&state) {
                    assert!(
                        text.contains(button_text(label, key).trim()),
                        "missing {label} {} on tab {tab} width {width}",
                        shortcut_label(key)
                    );
                    assert!(hits.iter().any(|hit| hit.action == MouseAction::Key(key)));
                }
                for duplicate in [
                    "Enter: actions",
                    "Enter: account actions",
                    "L: personal label",
                    "Enter: copy model ID",
                    "p: period",
                    "b: group",
                    "m: requests / tokens",
                ] {
                    assert!(!text.contains(duplicate), "duplicate {duplicate}");
                }
            }
        }
    }

    #[test]
    fn settings_actions_are_inline() {
        let connection = Connection::default();
        let mut state = State {
            tab: SETTINGS,
            ..Default::default()
        };
        let lines = settings_lines(&connection, &state, 80);
        assert!(lines[0].to_string().contains("[ Check updates ]"));
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("[ Configure connection ]"));
        assert!(action_menu(&state, &connection).1.is_empty());
        let hits = dashboard_hits(&state, 100, 30);
        for key in ['v', 'e'] {
            let hit = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Key(KeyCode::Char(key)))
                .unwrap();
            assert_eq!(
                mouse_key(click(hit), &hits, &mut state).unwrap().code,
                KeyCode::Char(key)
            );
        }
        assert_eq!(state.settings_selected, 1);
    }

    #[test]
    fn mouse_account_and_token_clicks_select_and_activate() {
        let hits = [Hit {
            area: Rect::new(5, 5, 10, 1),
            action: MouseAction::Select(2),
        }];
        for tab in [OVERVIEW, SETTINGS] {
            let mut state = State {
                tab,
                ..Default::default()
            };
            assert_eq!(
                mouse_key(click(&hits[0]), &hits, &mut state).unwrap().code,
                KeyCode::Enter
            );
            assert_eq!(
                if tab == OVERVIEW {
                    state.account_selected
                } else {
                    state.selected
                },
                2
            );
        }
        let mut state = State {
            tab: TOKENS,
            ..Default::default()
        };
        assert_eq!(
            mouse_key(click(&hits[0]), &hits, &mut state).unwrap().code,
            KeyCode::Enter
        );
        assert_eq!(state.token_selected, 2);
    }

    fn click(hit: &Hit) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.area.x,
            row: hit.area.y,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_token_rows_follow_wrapping_pagination_and_scroll() {
        let mut state = State {
            tab: TOKENS,
            token_selected: 8,
            scroll: 1,
            ..Default::default()
        };
        state.values[TOKENS] = Some(serde_json::json!((0..12).map(|index| serde_json::json!({"id": format!("tok-{index}"), "name": "Long token name ".repeat(8)})).collect::<Vec<_>>()));
        let hits = dashboard_hits(&state, 72, 24);
        for index in 8..12 {
            let hit = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Select(index))
                .unwrap();
            assert_eq!(
                mouse_key(click(hit), &hits, &mut state).unwrap().code,
                KeyCode::Enter
            );
            assert_eq!(state.token_selected, index);
        }
        assert!(!hits.iter().any(|hit| hit.action == MouseAction::Select(0)));
    }

    #[test]
    fn mouse_cursor_placement_respects_unicode_width_and_byte_boundaries() {
        let mut input = TextInput::new("a界é".into());
        input.place_cursor(2);
        assert_eq!(input.cursor, 1);
        input.place_cursor(3);
        assert_eq!(input.cursor, 4);
        input.place_cursor(100);
        assert_eq!(input.cursor, input.value.len());
    }

    fn dashboard_hits(state: &State, width: u16, height: u16) -> Vec<Hit> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                hits = render_hits(frame, state, &Connection::default(), false);
            })
            .unwrap();
        hits
    }

    #[test]
    fn mouse_tabs_buttons_and_model_rows_follow_rendered_geometry() {
        for (width, height) in [(72, 20), (120, 35)] {
            let mut state = State {
                tab: MODELS,
                model_selected: 25,
                ..Default::default()
            };
            state.values[MODELS] = Some(
                serde_json::json!({"data": (0..30).map(|i| serde_json::json!({"id": format!("model-{i:02}")})).collect::<Vec<_>>() }),
            );
            let hits = dashboard_hits(&state, width, height);
            let tab = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Tab(SETTINGS))
                .unwrap();
            assert_eq!(
                mouse_key(click(tab), &hits, &mut state).unwrap().code,
                KeyCode::Char('5')
            );
            let row = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Select(25))
                .unwrap();
            state.model_selected = 0;
            assert_eq!(
                mouse_key(click(row), &hits, &mut state).unwrap().code,
                KeyCode::Enter
            );
            assert_eq!(state.model_selected, 25);
            let copy = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Key(KeyCode::Enter))
                .unwrap();
            assert_eq!(
                mouse_key(click(copy), &hits, &mut state).unwrap().code,
                KeyCode::Enter
            );
        }
    }

    #[test]
    fn mouse_dialogs_block_background_and_require_separate_confirmation() {
        let mut state = State {
            tab: TOKENS,
            modal: Some(Modal::Confirm {
                id: "fixture".into(),
                rotate: false,
            }),
            ..Default::default()
        };
        let hits = dashboard_hits(&state, 72, 20);
        assert!(!hits
            .iter()
            .any(|hit| matches!(hit.action, MouseAction::Tab(_) | MouseAction::Select(_))));
        let confirm = hits
            .iter()
            .find(|hit| hit.action == MouseAction::Key(KeyCode::Char('y')))
            .unwrap();
        assert_eq!(
            mouse_key(click(confirm), &hits, &mut state).unwrap().code,
            KeyCode::Char('y')
        );
        let outside = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        assert!(mouse_key(outside, &hits, &mut state).is_none());
        assert!(state.modal.is_some());
        assert!(dashboard_hits(&state, 50, 10).is_empty());
    }

    #[test]
    fn mouse_menu_activates_only_clicked_entry_and_wheel_uses_keyboard_navigation() {
        let mut state = State {
            modal: Some(Modal::Actions {
                title: "Actions".into(),
                entries: vec![
                    ("First".into(), KeyCode::Char('a')),
                    ("Second".into(), KeyCode::Char('b')),
                ],
                selected: 0,
            }),
            ..Default::default()
        };
        let hits = dashboard_hits(&state, 72, 20);
        let second = hits
            .iter()
            .find(|hit| hit.action == MouseAction::Menu(1))
            .unwrap();
        assert_eq!(
            mouse_key(click(second), &hits, &mut state).unwrap().code,
            KeyCode::Enter
        );
        assert!(matches!(
            state.modal,
            Some(Modal::Actions { selected: 1, .. })
        ));
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_key(wheel, &hits, &mut state).unwrap().code,
            KeyCode::Down
        );
    }

    #[test]
    fn account_details_have_no_mutation_menu() {
        let mut state = State::default();
        state.values[OVERVIEW] = Some(
            serde_json::json!({"quota_accounts":[{"id":1,"preference":{"enabled":true,"locked":false}}]}),
        );
        let (_, actions) = action_menu(&state, &Connection::default());
        assert!(actions.is_empty());
        state.modal = Some(Modal::Account { id: 1, scroll: 0 });
        let hits = dashboard_hits(&state, 120, 40);
        assert!(hits
            .iter()
            .any(|hit| hit.action == MouseAction::Key(KeyCode::Char('r'))));
        assert!(!hits.iter().any(|hit| matches!(
            hit.action,
            MouseAction::Menu(_) | MouseAction::Key(KeyCode::Enter | KeyCode::Char('D' | 'L'))
        )));
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
            (OVERVIEW, "subscription history, billing and clients"),
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
                "subscription history, billing and clients",
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
    fn token_rename_prefills_the_label_and_validates_before_submission() {
        let connection = Connection::default();
        let mut state = State {
            tab: TOKENS,
            ..Default::default()
        };
        assert!(token_rename_modal(&state).is_none());
        assert!(!action_menu(&state, &connection)
            .1
            .iter()
            .any(|(_, key)| *key == KeyCode::Char('l')));
        state.values[TOKENS] = Some(serde_json::json!([
            {"id":"tok_first","name":"First label"},
            {"id":"tok_second","name":"Второй лейбл"}
        ]));
        state.token_selected = 1;
        assert!(action_menu(&state, &connection)
            .1
            .iter()
            .any(|(name, key)| name == "Rename token" && *key == KeyCode::Char('l')));
        let Some(Modal::Rename { id, mut input, .. }) = token_rename_modal(&state) else {
            panic!("rename dialog expected");
        };
        assert_eq!(id, "tok_second");
        assert_eq!(input.value, "Второй лейбл");
        assert_eq!(input.cursor, input.value.len());
        input.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(token_rename_request(&id, &input).is_err());
        input = TextInput::new("  New label  ".into());
        assert!(matches!(token_rename_request(&id, &input).unwrap(),
            ControlRequest::TokenRename { id, name } if id == "tok_second" && name == "New label"));
        input = TextInput::new("界".repeat(27));
        assert!(token_rename_request(&id, &input).is_err());
        state.modal = token_rename_modal(&state);
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
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
        assert!(text.contains("Rename token"));
        assert!(text.contains("Save Enter"));
        assert!(text.contains("Cancel Esc"));
    }

    #[test]
    fn token_search_maps_all_actions_and_mouse_rows_to_filtered_ids_and_preserves_refresh_selection(
    ) {
        let mut state = State {
            tab: TOKENS,
            ..Default::default()
        };
        state.values[TOKENS] = Some(serde_json::json!((0..12).map(|i| serde_json::json!({
            "id":format!("tok_{i:02}"),"name":if i % 2 == 0 {"Work"} else {"CI runner"},"expires_at":9999999999_i64,
            "disabled":false,"fast_mode":null
        })).collect::<Vec<_>>()));
        apply_search(&mut state, SearchKind::Tokens, "  cI  ");
        assert_eq!(
            filtered_token_rows(&state, state.values[TOKENS].as_ref().unwrap()).len(),
            6
        );
        state.token_selected = 4;
        let hits = dashboard_hits(&state, 72, 20);
        let hit = hits
            .iter()
            .find(|hit| hit.action == MouseAction::Select(5))
            .unwrap();
        assert_eq!(
            mouse_key(click(hit), &hits, &mut state).unwrap().code,
            KeyCode::Enter
        );
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_11");
        let Some(Modal::Rename { id, .. }) = token_rename_modal(&state) else {
            panic!("rename expected")
        };
        assert_eq!(id, "tok_11");
        assert!(action_menu(&state, &Connection::default())
            .1
            .iter()
            .any(|(_, key)| *key == KeyCode::Char('d')));
        assert!(action_menu(&state, &Connection::default())
            .1
            .iter()
            .any(|(_, key)| *key == KeyCode::Char('c')));
        let mut refreshed = state.values[TOKENS].clone().unwrap();
        refreshed.as_array_mut().unwrap().reverse();
        preserve_selection(&mut state, TOKENS, &refreshed);
        state.values[TOKENS] = Some(refreshed);
        assert_eq!(state.token_selected, 0);
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_11");
        apply_search(&mut state, SearchKind::Tokens, "TOK_01");
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_01");
        apply_search(&mut state, SearchKind::Tokens, "nothing matches");
        assert!(selected_token(&state).is_none());
        assert!(token_rename_modal(&state).is_none());
        assert_eq!(
            action_menu(&state, &Connection::default()).1,
            vec![("Create token".into(), KeyCode::Char('n'))]
        );
        assert!(!dashboard_hits(&state, 72, 20)
            .iter()
            .any(|hit| matches!(hit.action, MouseAction::Select(_))));
        assert!(token_lines(&state, 70)
            .iter()
            .any(|line| line.to_string().contains("No matching tokens")));
        apply_search(&mut state, SearchKind::Tokens, "");
        assert_eq!(state.scroll, 0);
        assert_eq!(state.token_selected, 0);
    }

    #[test]
    fn model_cards_keep_names_ids_capabilities_and_click_targets_separate() {
        let mut state = State {
            tab: MODELS,
            ..Default::default()
        };
        state.values[MODELS] = Some(serde_json::json!({"data":[{
            "id":"gpt-synthetic-mini","display_name":"gpt-synthetic-mini","source":"platform", "capabilities":["reasoning","tools"],
            "exetrouter":{"context_window":400000}
        }]}));
        for width in [72, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 25)).unwrap();
            let mut hits = Vec::new();
            terminal
                .draw(|frame| {
                    hits = render_hits(frame, &state, &Connection::default(), false);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            for label in [
                "GPT synthetic Mini",
                "gpt-synthetic-mini",
                "400000 context",
                "Reasoning",
                "Tools",
                "Platform API",
                "Controls",
            ] {
                assert!(text.contains(label), "missing {label} at width {width}");
            }
            let hit = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Select(0))
                .unwrap();
            assert_eq!(hit.area.height, 3);
            for y in hit.area.y..hit.area.bottom() {
                let event = MouseEvent {
                    row: y,
                    ..click(hit)
                };
                assert_eq!(
                    mouse_key(event, &hits, &mut state).unwrap().code,
                    KeyCode::Enter
                );
                assert_eq!(selected_model_id(&state), Some("gpt-synthetic-mini"));
            }
            assert_eq!(buffer[(width - 1, 0)].bg, palette::BASE);
            for (label, background) in [
                ("Reasoning", palette::RAISED),
                ("Platform API", Color::Rgb(59, 48, 32)),
            ] {
                let cells = buffer
                    .content
                    .windows(label.len())
                    .find(|cells| {
                        cells.iter().map(|cell| cell.symbol()).collect::<String>() == label
                    })
                    .unwrap();
                assert!(cells.iter().all(|cell| cell.bg == background));
            }
        }
    }

    #[test]
    fn filtered_token_navigation_pages_and_resizing_keep_the_selected_entry_visible() {
        let mut state = State {
            tab: TOKENS,
            ..Default::default()
        };
        state.values[TOKENS] = Some(serde_json::json!((0..30).map(|i|serde_json::json!({"id":format!("tok_{i}"),"name":if i % 2 == 0 {"Selected group"}else{"Other"}})).collect::<Vec<_>>()));
        apply_search(&mut state, SearchKind::Tokens, "Selected group");
        assert!(navigate_tokens(&mut state, KeyCode::PageDown, 15));
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_8");
        assert!(navigate_tokens(&mut state, KeyCode::End, 15));
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_28");
        assert!(navigate_tokens(&mut state, KeyCode::Up, 15));
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_26");
        assert!(navigate_tokens(&mut state, KeyCode::PageUp, 15));
        assert_eq!(selected_token(&state).unwrap()["id"], "tok_18");
        // Force the fourth card onto a shorter viewport without another input event.
        state.token_selected = 11;
        state.scroll = 0;
        for height in [35, 20] {
            let mut terminal = Terminal::new(TestBackend::new(72, height)).unwrap();
            let mut hits = Vec::new();
            terminal
                .draw(|frame| {
                    hits = render_hits(frame, &state, &Connection::default(), false);
                })
                .unwrap();
            let hit = hits
                .iter()
                .find(|hit| hit.action == MouseAction::Select(11))
                .unwrap();
            assert_eq!(hit.area.height, 1);
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("▶ Selected group"));
            assert!(text.contains("tok_22"));
        }
        assert!(navigate_tokens(&mut state, KeyCode::Home, 10));
        assert_eq!(state.token_selected, 0);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn standalone_account_menu_retains_disable_and_selected_settings_scroll_into_view() {
        let connection = Connection {
            mode: super::super::config::Mode::Standalone,
            ..Default::default()
        };
        let mut state = State {
            tab: SETTINGS,
            selected: 11,
            settings_selected: 15,
            ..Default::default()
        };
        state.values[SETTINGS] = Some(serde_json::json!((0..12).map(|i|serde_json::json!({"id":i,"email":format!("account-{i}@example.com"),"state":"active"})).collect::<Vec<_>>()));
        let Some(Modal::Actions { title, entries, .. }) = account_actions(&state) else {
            panic!("account menu expected")
        };
        assert_eq!(title, "account-11@example.com");
        assert_eq!(
            entries.iter().map(|(_, key)| *key).collect::<Vec<_>>(),
            vec![KeyCode::Char('u'), KeyCode::Char('d')]
        );
        state.values[SETTINGS].as_mut().unwrap()[11]["state"] = "disabled".into();
        let Some(Modal::Actions { entries, .. }) = account_actions(&state) else {
            panic!("account menu expected")
        };
        assert_eq!(entries.len(), 1);
        keep_settings_selection_visible(&mut state, &connection, 10, 70);
        assert!(state.scroll > 0);
        let visible = settings_lines(&connection, &state, 70)
            .into_iter()
            .skip(usize::from(state.scroll))
            .take(10)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible.contains("▶ account-11@example.com"));
        state.settings_selected = 0;
        keep_settings_selection_visible(&mut state, &connection, 10, 70);
        assert_eq!(state.scroll, 0);
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
                    matches!(request(&state), ControlRequest::UsageRange { by: requested_by, .. } if requested_by.as_deref() == Some(by))
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
                assert!(text.contains("Reported tokens · Last 24 hours"));
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
    fn usage_all_series_and_read_only_breakdown_preserve_unknown_counters_and_prices() {
        let mut state = State {
            tab: USAGE,
            group: 3,
            usage_all: true,
            ..Default::default()
        };
        state.values[USAGE] = Some(
            serde_json::json!({"period":"7d","from_utc":1800000000,"to_utc":1800604800,"rows":[
                {"name":"tok_work","display_name":"Workstation","requests":2,"input_tokens":100,"output_tokens":20,"cached_input_tokens":50,"estimated_usd":5000.,"unpriced_requests":1},
                {"name":"tok_unknown","display_name":"Unknown counters","requests":1,"input_tokens":null,"output_tokens":null,"cached_input_tokens":null,"estimated_usd":null,"unknown_usage":1}
            ]}),
        );
        assert!(all_usage_series_available(&state));
        assert_eq!(
            usage_rows(&state, state.values[USAGE].as_ref().unwrap()).len(),
            2
        );
        let report = usage_breakdown(&state);
        for label in [
            "Workstation",
            "Input 100",
            "Output 20",
            "Cached 50",
            "$5k",
            "partial",
            "Unknown counters",
            "Input —",
            "API equivalent Unknown",
        ] {
            assert!(report.contains(label), "missing {label}");
        }
        assert!(!report.contains("tok_work"));
        assert!(
            matches!(request(&state), ControlRequest::UsageRange {by:Some(by),..} if by == "token")
        );
        state.usage_all = false;
        state.usage_selected = 1;
        assert!(!usage_breakdown(&state).contains("Workstation"));
        state.modal = Some(Modal::Usage { scroll: u16::MAX });
        let hits = dashboard_hits(&state, 72, 20);
        assert!(hits
            .iter()
            .all(|hit| hit.action == MouseAction::Key(KeyCode::Esc)));
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
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
        assert!(text.contains("Usage breakdown"));
        assert!(text.contains("API equivalent Unknown"));
        state.modal = None;
        state.usage_all = true;
        let row = state.values[USAGE].as_ref().unwrap()["rows"][0].clone();
        state.values[USAGE].as_mut().unwrap()["rows"] = serde_json::json!(vec![row; 9]);
        assert!(!all_usage_series_available(&state));
        assert_eq!(
            usage_rows(&state, state.values[USAGE].as_ref().unwrap()).len(),
            1
        );
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
    fn usage_cursor_keeps_tab_navigation_and_unknown_counts_intact() {
        let now = chrono::Utc::now().timestamp();
        let mut state = State {
            tab: USAGE,
            ..Default::default()
        };
        state.values[USAGE] = Some(
            serde_json::json!({"timeline":[{"from_utc":now-7200},{"from_utc":now-3600},{"from_utc":now}]}),
        );
        assert!(seek_usage(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)
        ));
        assert_eq!(state.usage_cursor, Some(now - 3600));
        assert_eq!(state.tab, USAGE);
        assert!(!seek_usage(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)
        ));
        assert!(seek_usage(
            &mut state,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)
        ));
        assert_eq!(state.usage_cursor, Some(now - 7200));
        assert!(seek_usage(
            &mut state,
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE)
        ));
        assert_eq!(state.usage_cursor, Some(now));
        let unknown = serde_json::json!({"rows":[{"name":"synthetic","requests":1,"input_tokens":null,"output_tokens":null}]});
        assert_eq!(usage_value(&unknown, "synthetic", false), None);
        assert_eq!(usage_value(&unknown, "synthetic", true), Some(1.0));
        assert_eq!(usage_value(&unknown, "absent", false), Some(0.0));
    }

    #[test]
    fn usage_legend_shows_token_labels_on_small_terminals() {
        let now = chrono::Utc::now().timestamp();
        let id = "tok_0123456789abcdef";
        let label = "Personal laptop";
        let rows = serde_json::json!([{"name":id,"display_name":label,"requests":1,"input_tokens":10,"output_tokens":20,"unknown_usage":0}]);
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
                text.contains(label),
                "token legend missing at {width}x{height}"
            );
            assert!(!text.contains(id));
        }
    }

    #[test]
    fn account_subscription_is_plain_text_independently_of_platform_access() {
        for (price, expected) in [(20, "Plus $20"), (100, "Pro $100"), (200, "Pro $200")] {
            for platform in [Value::Null, serde_json::json!({"inference_enabled":false})] {
                let mut state = State::default();
                state.values[OVERVIEW] = Some(serde_json::json!({"quota_accounts":[{
                    "id":1,"label":"synthetic@example.com","subscription":{"plan_usd":price},
                    "platform":platform,"quota":{"windows":[]}
                }]}));
                let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
                terminal
                    .draw(|frame| render(frame, &state, &Connection::default(), false))
                    .unwrap();
                let lines = terminal
                    .backend()
                    .buffer()
                    .content
                    .chunks(120)
                    .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                    .collect::<Vec<_>>();
                let header = lines
                    .iter()
                    .find(|line| line.contains("synthetic@example.com"))
                    .unwrap();
                assert!(header.contains(expected));
                assert!(!header.contains("Platform API"));
                let cells = terminal
                    .backend()
                    .buffer()
                    .content
                    .windows(expected.len())
                    .find(|cells| {
                        cells.iter().map(|cell| cell.symbol()).collect::<String>() == expected
                    })
                    .unwrap();
                assert!(cells
                    .iter()
                    .all(|cell| cell.bg == selection_style().bg.unwrap()
                        && cell.fg == palette::MUTED
                        && !cell.modifier.contains(Modifier::BOLD)));
            }
        }
        assert_eq!(render::subscription_plan(&serde_json::json!({})), None);
        assert_eq!(
            render::subscription_plan(&serde_json::json!({"subscription":{"plan_usd":5000}})),
            Some("$5k/month".into())
        );
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
            assert_eq!(ranges[0].1 - ranges[0].0, if width == 120 { 6 } else { 9 });
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
    fn overview_uses_shared_forecasts_immediately_and_hides_failed_refresh_estimates() {
        let now = chrono::Utc::now().timestamp();
        let value = serde_json::json!({"quota_accounts":[{"id":1,"label":"account@example.com","quota":{"windows":[{"kind":"primary","used_percent":50,"remaining_percent":50,"window_minutes":480,"reset_at":now+28800,"observed_at":now,"status":"current"}]},"quota_forecast":[{"kind":"primary","window_minutes":480,"reset_at":now+28800,"observed_at":now,"used_percent":50,"current":{"percent_per_hour":25,"limit_at":now+7200},"usual":{"percent_per_hour":12.5,"limit_at":now+14400},"warning":{"rate_ratio":2,"limit_before_reset":true},"recent_seconds":1800,"historical_intervals":24,"historical_cycles":3}]}]});
        let mut state = State::default();
        state.values[OVERVIEW] = Some(value.clone());
        let mut terminal = Terminal::new(TestBackend::new(150, 35)).unwrap();
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
        assert!(text.contains("~2h 0m to limit"));
        assert!(text.contains("Historical pace: ~4h 0m to limit"));
        assert!(text.contains("Usage above usual · ~2.0x normal"));
        let cli = super::super::render::limits(&value);
        assert!(cli.contains("Historical pace:"));
        assert!(cli.contains("may run out before reset"));
        let mut failed = value.clone();
        failed["quota_accounts"][0]["refresh_error"] = "Live metadata unavailable".into();
        let success_height = account_ranges(&value, 150)[0].1;
        assert_eq!(success_height - account_ranges(&failed, 150)[0].1, 1);
        state.values[OVERVIEW] = Some(failed);
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
        assert!(!text.contains("Historical pace:"));
        assert!(!text.contains("Usage above usual"));
        assert!(!text.contains("to limit"));
    }

    #[test]
    fn overview_describes_moving_reset_and_idle_estimates_instead_of_inventing_an_eta() {
        let now = chrono::Utc::now().timestamp();
        let mut value = serde_json::json!({"quota_accounts":[{"id":1,"label":"account@example.com","quota":{"windows":[{"kind":"primary","used_percent":0,"remaining_percent":100,"window_minutes":300,"reset_at":now+18000,"observed_at":now,"status":"current"}]},"quota_forecast":[{"kind":"primary","window_minutes":300,"reset_at":now+18000,"observed_at":now,"used_percent":0,"current":null,"usual":null,"warning":null,"current_status":"reset_time_moving","recent_seconds":0,"historical_intervals":0,"historical_cycles":0}]}]});
        let mut state = State::default();
        state.values[OVERVIEW] = Some(value.clone());
        let mut terminal = Terminal::new(TestBackend::new(150, 35)).unwrap();
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
        assert!(text.contains("Reset time moving; no pace yet"));
        assert!(!text.contains("to limit"));
        value["quota_accounts"][0]["quota_forecast"][0]["current_status"] =
            "quota_unchanged".into();
        let cli = super::super::render::limits(&value);
        assert!(cli.contains("Pace unavailable: quota unchanged"));
        assert!(!cli.contains("to limit"));
    }
    #[test]
    fn usage_uses_the_same_rolling_ranges_as_desktop() {
        let state = State {
            tab: USAGE,
            period: 0,
            ..Default::default()
        };
        assert!(
            matches!(request(&state), ControlRequest::UsageRange { period, .. } if period == "24h")
        );
        assert_eq!(
            USAGE_PERIODS
                .iter()
                .map(|(_, title)| *title)
                .collect::<Vec<_>>(),
            [
                "Last 24 hours",
                "Last 7 days",
                "Last 30 days",
                "Last 90 days"
            ]
        );
    }
    #[test]
    fn account_selection_scrolls_and_survives_reordered_refresh() {
        let mut state = State::default();
        state.values[OVERVIEW] = Some(serde_json::json!({"quota_accounts":[
            {"id":11,"label":"alice@example.com","quota":{"windows":[{"window_minutes":10080,"remaining_percent":5,"status":"current"}]}},
            {"id":22,"label":"bob@example.com","quota":{"windows":[{"window_minutes":10080,"remaining_percent":4,"status":"current"}]}}
        ]}));
        select_item(&mut state, true, 10, 66);
        assert_eq!(state.account_selected, 1);
        assert!(state.scroll > 0);
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
        assert!(text.contains("Details Enter"));
        assert!(!text.contains("Enter: account actions"));
        assert!(!text.contains("Subscription limits"));
        let mut refreshed = state.values[OVERVIEW].clone().unwrap();
        refreshed["quota_accounts"]
            .as_array_mut()
            .unwrap()
            .reverse();
        preserve_selection(&mut state, OVERVIEW, &refreshed);
        state.values[OVERVIEW] = Some(refreshed);
        assert_eq!(state.account_selected, 0);
        select_item(&mut state, true, 10, 66);
        assert_eq!(state.account_selected, 1);
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
        assert!(text.contains("▶ Model 9"));
        assert!(text.contains("model-09"));
        assert!(text.contains("Copy ID Enter"));
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
    fn best_clients_and_scrollable_account_details_are_visible() {
        let now = chrono::Utc::now().timestamp();
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let mut state = State::default();
        state.values[OVERVIEW] = Some(serde_json::json!({"quota_accounts":[{
            "id":1,"label":"Synthetic","preference":{"enabled":true},
            "routing":{"best":true,"observed_at":now,"reason":"weekly_reset_soon","target_share_percent":100.0},
            "quota":{"windows":[]},"clients":{"scope":"server","count":2,"observed_at":now,"rows":[{"id":"tok_synthetic","name":"Laptop - Codex","last_request_at":now-12,"in_flight":1},{"id":"client_synthetic","name":"Other user's client","owned":false,"last_request_at":now-20,"in_flight":0}]}
        }]}));
        terminal
            .draw(|f| render(f, &state, &Connection::default(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Best"));
        assert!(text.contains("Clients: 2"));
        let badge = buffer
            .content
            .windows(4)
            .find(|cells| cells.iter().map(|c| c.symbol()).collect::<String>() == "Best")
            .unwrap();
        assert!(badge.iter().all(|c| c.bg == Color::Rgb(34, 46, 36)));
        let details = account_detail_text(&state, 1);
        assert!(details.contains("Laptop - Codex"));
        assert!(details.contains("12s ago"));
        assert!(details.contains("Running: 1"));
        assert!(details.contains("all users' recently active tokens"));
        assert!(details.contains("Other user's client"));
        state.modal = Some(Modal::Account { id: 1, scroll: 32 });
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
        assert!(text.contains("Account details"));
        assert!(text.contains("Laptop - Codex"));
        assert!(text.contains("Refresh r"));
        state.values[OVERVIEW].as_mut().unwrap()["quota_accounts"][0]["routing"]["observed_at"] =
            (now - 60).into();
        assert!(!render::best_account(
            &state.values[OVERVIEW].as_ref().unwrap()["quota_accounts"][0],
            now
        ));
    }
    #[test]
    fn mode_badges_and_selected_tabs_use_the_approved_muted_palette() {
        for (mode, title, color) in [
            (
                super::super::config::Mode::Remote,
                "Remote Server",
                Color::Rgb(43, 40, 55),
            ),
            (
                super::super::config::Mode::Standalone,
                "Standalone",
                Color::Rgb(59, 48, 32),
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
            assert!(
                second_row.contains("Overview 1    Usage 2    Tokens 3    Models 4    Settings 5")
            );
            assert!(!second_row.contains('│'));
            for x in 1..11 {
                assert_eq!(buffer[(x, 2)].bg, palette::TEXT);
                assert_eq!(buffer[(x, 2)].fg, palette::BASE);
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
    fn automatic_routing_status_is_visible_on_the_minimum_terminal() {
        assert_eq!(TABS, ["Overview", "Usage", "Tokens", "Models", "Settings"]);
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let mut state = State::default();
        state.values[OVERVIEW] = Some(
            serde_json::json!({"quota_accounts":[{"id":1,"label":"synthetic","routing":{"reason":"reset_expiring"},"automatic_reset":{"status":"reset"},"quota":{"windows":[]}}]}),
        );
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
        assert!(text.contains("Priority"));
        let priority = terminal
            .backend()
            .buffer()
            .content
            .windows("Priority".len())
            .find(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>() == "Priority")
            .unwrap();
        assert!(priority
            .iter()
            .all(|cell| cell.bg == Color::Rgb(54, 37, 35)));
        assert!(text.contains("auto reset: reset"));
        assert!(!text.contains("Use credit"));
    }
    #[test]
    fn quota_tracks_use_thin_full_width_lines_and_semantic_colors() {
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        for (remaining, color) in [
            (18., Color::Rgb(223, 104, 112)),
            (38., Color::Rgb(232, 191, 82)),
            (98., Color::Rgb(93, 195, 143)),
        ] {
            terminal
                .draw(|f| {
                    let area = f.area();
                    super::super::account_details::meter(f.buffer_mut(), area, Some(remaining));
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert!(buffer
                .content
                .iter()
                .all(|c| c.symbol() == "━" && c.bg == Color::Reset));
            assert_eq!(buffer[(0, 0)].fg, color);
            assert_eq!(buffer[(79, 0)].fg, Color::DarkGray);
            assert_eq!(
                buffer.content.iter().filter(|c| c.fg == color).count(),
                (80.0_f64 * remaining / 100.).round() as usize
            );
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
        state.scroll = account_ranges(state.values[OVERVIEW].as_ref().unwrap(), 68)[0].0;
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
        assert!(text.contains("Refresh"));
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
        let start = 20 - usize::from(controls_height(&state, false, 72));
        assert!(rows[start].contains("connection was interrupted"));
        assert!(rows[start + 1].contains("refresh this view."));
        assert!(rows[start + 2].contains("Controls"));
        assert!(rows[start + 3..19].iter().any(|row| row.contains("Quit")));
        assert!(rows[start + 3].contains("Actions Enter"));
        assert!(!rows[19].contains("Enter: actions"));
        assert_eq!(
            overview_viewport(20, 72, &state, false),
            start as u16 - HEADER_HEIGHT - 1
        );
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
            assert!(row_text(height as usize - 3, height as usize).contains("Actions Enter"));
            assert!(!row_text(0, height as usize - 3).contains("Actions Enter"));
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
            assert!(text.contains("Reported tokens · Last 24 hours"));
            assert!(text.contains("Last 24 hours p"));
            assert!(text.contains("Tokens m"));
            assert!(buffer.content.iter().any(|c| c
                .symbol()
                .chars()
                .any(|ch| matches!(ch, '▀' | '▄' | '█' | '\u{2801}'..='\u{28ff}'))));
            assert!(buffer.content.iter().any(|c| c.fg == Color::Cyan));
        }
    }
    #[test]
    fn usd_chart_preserves_unknown_and_partial_prices_and_uses_passive_paging() {
        let mut state = State {
            tab: USAGE,
            usd_metric: true,
            ..Default::default()
        };
        let bucket = serde_json::json!({"rows":[{"name":"known","estimated_usd":0.25,"unpriced_requests":1},{"name":"unknown","estimated_usd":null}]});
        assert_eq!(usage_metric(&bucket, "known", &state), Some(0.25));
        assert_eq!(usage_metric(&bucket, "unknown", &state), None);
        assert_eq!(usage_metric(&bucket, "absent", &state), Some(0.));
        state.values[USAGE] = Some(serde_json::json!({"to_utc":1_800_000_000}));
        page_usage(&mut state, true);
        assert_eq!(state.usage_before, Some(1_800_000_000 - 86400));
        assert!(matches!(
            request(&state),
            ControlRequest::UsageRange {
                before: Some(1_799_913_600),
                ..
            }
        ));
    }

    #[test]
    fn token_dates_have_one_line_and_disabled_and_fast_overrides_have_background_badges() {
        let mut state = State {
            tab: TOKENS,
            ..Default::default()
        };
        state.values[TOKENS] = Some(
            serde_json::json!([{"id":"synthetic-token","name":"Synthetic","disabled":true,"fast_mode":null,"created_at":1800000000,"expires_at":9999999999_i64,"last_used_at":null}]),
        );
        let lines = token_lines(&state, 70);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.to_string().contains("Created"))
                .count(),
            1
        );
        assert!(lines[2].to_string().contains("Expires") && lines[2].to_string().contains("Used"));
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.contains("Disabled") && s.style.bg == Some(Color::Rgb(54, 37, 35))));
        assert!(!lines.iter().any(|l| l.to_string().contains("Fast:")));
        state.values[TOKENS].as_mut().unwrap()[0]["fast_mode"] = serde_json::json!(false);
        assert!(token_lines(&state, 70)[0]
            .spans
            .iter()
            .any(|s| s.content.contains("Fast: Block") && s.style.bg.is_some()));
        assert!(token_policy_available(
            &state.values[TOKENS].as_ref().unwrap()[0]
        ));
        state.values[TOKENS].as_mut().unwrap()[0]["expires_at"] = serde_json::json!(1);
        assert!(!token_policy_available(
            &state.values[TOKENS].as_ref().unwrap()[0]
        ));
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
        assert!(text.contains("Refresh"));
        assert!(text.contains("Quit"));
    }
}
