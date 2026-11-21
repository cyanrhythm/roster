use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::i18n::{self, t, tf};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(5), Constraint::Length(1)])
        .split(frame.area());

    draw_header(frame, app, chunks[0]);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(chunks[1]);
    draw_list(frame, app, columns[0]);
    draw_detail(frame, app, columns[1]);

    draw_status(frame, app, chunks[2]);

    match app.mode {
        Mode::Filter => draw_input_popup(frame, app, &t("popup.filter.title"), &t("popup.filter.hint")),
        Mode::Rename => {
            let title = popup_title(app, "popup.rename.title", "popup.rename.title_many");
            draw_input_popup(frame, app, &title, &t("popup.rename.hint"));
        }
        Mode::Workspace => {
            let open = app.targets().iter().any(|session| session.mounted_pid.is_some());
            let hint = if open {
                t("popup.workspace.hint_open")
            } else {
                t("popup.workspace.hint")
            };
            let title = popup_title(app, "popup.workspace.title", "popup.workspace.title_many");
            draw_input_popup(frame, app, &title, &hint);
        }
        Mode::ConfirmDelete => draw_delete_popup(frame, app),
        Mode::Help => draw_help(frame),
        Mode::Normal => {}
    }
}

/// "Rename session" for one, "Rename 3 sessions" for a batch.
fn popup_title(app: &App, single_key: &str, many_key: &str) -> String {
    let count = app.target_count();
    if count > 1 {
        tf(many_key, &[("count", &count.to_string())])
    } else {
        t(single_key)
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let breakdown = app
        .source_counts()
        .iter()
        .map(|(id, count)| format!("{} {}", app.registry.label(id), count))
        .collect::<Vec<_>>()
        .join(" · ");

    let mut spans = vec![
        Span::styled(
            format!(" {} ", t("app.title")),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            tf("header.sessions", &[("count", &app.sessions.len().to_string())]),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled(
            tf("header.shown", &[("count", &app.visible.len().to_string())]),
            Style::default().fg(Color::Gray),
        ),
        Span::raw("  "),
        Span::styled(breakdown, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(
            tf("header.source", &[("label", &app.source_label())]),
            Style::default().fg(if app.source.is_some() {
                Color::Cyan
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw("  "),
        Span::styled(
            tf("header.language", &[("label", i18n::language().label())]),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if app.marked_count() > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            tf("header.marked", &[("count", &app.marked_count().to_string())]),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    if !app.filter.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            tf("header.filter", &[("query", &app.filter)]),
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.mode == Mode::Filter {
        spans.push(Span::styled(
            format!("  {}", t("header.typing")),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !app.errors.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            tf("header.load_errors", &[("count", &app.errors.len().to_string())]),
            Style::default().fg(Color::Red),
        ));
    }

    let locations = app.locations.join("  ·  ");

    let text = Text::from(vec![
        Line::from(spans),
        Line::from(Span::styled(
            tf("header.locations", &[("locations", &locations)]),
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|index| {
            let session = &app.sessions[*index];
            let open = if session.mounted_pid.is_some() {
                t("list.open_badge")
            } else {
                String::new()
            };
            let accent = app.registry.accent(session.source);
            let badge = app.registry.badge(session.source);
            let marked = app.is_marked(&session.id);
            let marker = if marked { "[x]" } else { "[ ]" };
            let title = Line::from(vec![
                Span::styled(
                    format!("{marker} "),
                    Style::default().fg(if marked { Color::Yellow } else { Color::DarkGray }),
                ),
                Span::styled(
                    format!("{badge} "),
                    Style::default().fg(Color::Rgb(accent[0], accent[1], accent[2])),
                ),
                Span::styled(
                    format!("{}{}", session.display_title(), open),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
            ]);
            let meta = Line::from(vec![Span::styled(
                format!(
                    "{}  ·  {}  ·  {}  ·  {}",
                    session.cwd,
                    session.short_id(),
                    app.registry.measurement(session),
                    human_age(session.mtime_ms),
                ),
                Style::default().fg(Color::DarkGray),
            )]);
            ListItem::new(vec![title, meta])
        })
        .collect();

    let title = tf("list.title", &[("count", &app.visible.len().to_string())]);
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().bg(Color::Rgb(38, 56, 74)).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.current() {
        Some(session) => format!(" {} ", session.display_title()),
        None => format!(" {} ", t("detail.title")),
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    let Some(session) = app.current() else {
        frame.render_widget(
            Paragraph::new(Span::styled(t("detail.empty"), Style::default().fg(Color::DarkGray))).block(block),
            area,
        );
        return;
    };

    let mut rows: Vec<(String, String)> = vec![
        (t("detail.id"), session.id.clone()),
        (t("detail.title_label"), session.title.clone()),
        (t("detail.source"), app.registry.label(session.source)),
        (t("detail.workspace"), session.cwd.clone()),
        (t("detail.created"), format_time(session.created_at)),
        (t("detail.updated"), format_time(session.mtime_ms)),
    ];

    // Everything source-specific comes from the source itself; the core only
    // knows that a session may have extra rows.
    for row in app.registry.detail(session) {
        rows.push((row.label, row.value));
    }

    if let Some(parent) = &session.parent {
        rows.push((t("detail.parent"), parent.clone()));
    }
    if let Some(preset) = &session.agent_preset {
        rows.push((t("detail.preset"), preset.clone()));
    }
    if let Some(model) = &session.model {
        rows.push((t("detail.model"), model.clone()));
    }
    rows.push((t("detail.title_source"), title_source_label(&session.title_source)));

    // One column for every label, so the values line up no matter how wide the
    // translated label happens to be.
    let column = detail_column(&rows);
    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = detail_lines(rows, column, inner_width);

    if let Some(pid) = session.mounted_pid {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            tf("detail.mounted", &[("pid", &pid.to_string())]),
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(Span::styled(
            t("detail.mounted_hint"),
            Style::default().fg(Color::Yellow),
        )));
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let hints = match app.mode {
        Mode::Normal => t("status.hint.normal"),
        Mode::Filter => t("status.hint.filter"),
        Mode::Rename => t("status.hint.rename"),
        Mode::Workspace => t("status.hint.workspace"),
        Mode::ConfirmDelete => t("status.hint.confirm_delete"),
        Mode::Help => t("status.hint.help"),
    };

    let mut spans = Vec::new();
    if !app.status.is_empty() {
        spans.push(Span::styled(
            app.status.clone(),
            Style::default().fg(if app.status_error { Color::Red } else { Color::Green }),
        ));
        spans.push(Span::styled("  |  ", Style::default().fg(Color::DarkGray)));
    }
    spans.push(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_input_popup(frame: &mut Frame, app: &App, title: &str, hint: &str) {
    let area = centered(frame.area(), 70, 7);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let input_area = rows[0];
    let (before, after) = app.input.split_at(app.cursor.min(app.input.len()));
    let line = Line::from(vec![
        Span::styled(before.to_string(), Style::default().fg(Color::White)),
        Span::styled(after.to_string(), Style::default().fg(Color::Gray)),
    ]);
    frame.render_widget(Paragraph::new(line), input_area);

    frame.render_widget(
        Paragraph::new(Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray))),
        rows[1],
    );

    let offset = Line::from(before).width() as u16;
    let x = (input_area.x + offset).min(input_area.x + input_area.width.saturating_sub(1));
    frame.set_cursor_position(Position::new(x, input_area.y));
}

fn draw_delete_popup(frame: &mut Frame, app: &App) {
    let mut lines = Vec::new();
    let targets = app.targets();
    if targets.len() > 1 {
        lines.push(Line::from(Span::styled(
            tf("popup.delete.many", &[("count", &targets.len().to_string())]),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));
        for session in targets.iter().take(5) {
            lines.push(Line::from(Span::styled(
                format!("  {}  ·  {}", session.display_title(), session.cwd),
                Style::default().fg(Color::Gray),
            )));
        }
        if targets.len() > 5 {
            lines.push(Line::from(Span::styled(
                tf("popup.delete.more", &[("count", &(targets.len() - 5).to_string())]),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let mounted = targets.iter().filter(|session| session.mounted_pid.is_some()).count();
        if mounted > 0 {
            lines.push(Line::from(Span::styled(
                tf("popup.delete.mounted_many", &[("count", &mounted.to_string())]),
                Style::default().fg(Color::Yellow),
            )));
        }
    } else if let Some(session) = targets.first() {
        lines.push(Line::from(Span::styled(
            session.display_title(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("{}  ·  {}", session.id, session.cwd),
            Style::default().fg(Color::Gray),
        )));
        if let Some(pid) = session.mounted_pid {
            lines.push(Line::from(Span::styled(
                tf("popup.delete.mounted", &[("pid", &pid.to_string())]),
                Style::default().fg(Color::Yellow),
            )));
        }
        let children = app
            .sessions
            .iter()
            .filter(|other| other.parent.as_deref() == Some(session.id.as_str()))
            .count();
        if children > 0 {
            lines.push(Line::from(Span::styled(
                tf("popup.delete.children", &[("count", &children.to_string())]),
                Style::default().fg(Color::Yellow),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t("popup.delete.body"),
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::from(Span::styled(
        t("popup.delete.confirm"),
        Style::default().fg(Color::Yellow),
    )));

    // The batch listing makes the popup taller than the single-session one.
    let height = (lines.len() as u16 + 2).min(frame.area().height);
    let area = centered(frame.area(), 70, height);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("popup.delete.title")))
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), inner);
}

fn draw_help(frame: &mut Frame) {
    let header = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let lines = vec![
        Line::from(Span::styled(t("help.nav"), header)),
        Line::from(t("help.nav_move")),
        Line::from(t("help.nav_top")),
        Line::from(t("help.nav_page")),
        Line::from(t("help.nav_filter")),
        Line::from(t("help.nav_mark")),
        Line::from(t("help.nav_mark_all")),
        Line::from(""),
        Line::from(Span::styled(t("help.actions"), header)),
        Line::from(t("help.action_rename")),
        Line::from(t("help.action_workspace")),
        Line::from(t("help.action_delete")),
        Line::from(t("help.action_reload")),
        Line::from(t("help.action_source")),
        Line::from(t("help.action_language")),
        Line::from(t("help.action_quit")),
        Line::from(""),
        Line::from(Span::styled(t("help.footer"), Style::default().fg(Color::DarkGray))),
    ];

    let height = (lines.len() as u16 + 2).min(frame.area().height);
    let area = centered(frame.area(), 72, height);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("help.title")))
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// A source records where its title came from as a plain suffix; the label is
/// `title_source.<suffix>` in the locale tables. A source added later only has
/// to add its keys, and an unknown suffix still renders as something readable.
fn title_source_label(source: &str) -> String {
    let key = format!("title_source.{source}");
    let text = t(&key);
    if text == key {
        t("title_source.fallback")
    } else {
        text
    }
}

/// Width of the label column: the widest label plus its colon.
fn detail_column(rows: &[(String, String)]) -> usize {
    rows.iter()
        .map(|(label, _)| Line::from(label.as_str()).width() + 1)
        .max()
        .unwrap_or(0)
}

/// One row of the detail pane: the label starts at the left edge and is padded
/// to the shared column, so every value begins at the same place.
fn detail_line(label: &str, value: String, column: usize) -> Line<'static> {
    let used = Line::from(label).width() + 1;
    let gap = column.saturating_sub(used) + 2;
    Line::from(vec![
        Span::styled(format!("{label}:"), Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(gap)),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

/// Every row, with long values wrapped under the value column instead of
/// falling back to the left edge.
fn detail_lines(rows: Vec<(String, String)>, column: usize, inner_width: usize) -> Vec<Line<'static>> {
    let value_column = column + 2;
    let budget = inner_width.saturating_sub(value_column).max(1);
    let mut lines = Vec::new();

    for (label, value) in rows {
        let mut chunks = wrap_to_width(&value, budget);
        let first = chunks.remove(0);
        lines.push(detail_line(&label, first, column));
        for chunk in chunks {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(value_column)),
                Span::styled(chunk, Style::default().fg(Color::White)),
            ]));
        }
    }
    lines
}

/// Splits `text` into pieces no wider than `width` display cells. Display
/// width matters here because CJK labels and values are two cells per glyph.
fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    let mut pieces = vec![String::new()];
    let mut used = 0;
    for character in text.chars() {
        if character == '\n' {
            pieces.push(String::new());
            used = 0;
            continue;
        }
        let cell = Line::from(character.to_string()).width();
        if used + cell > width {
            pieces.push(String::new());
            used = 0;
            // A space that lands on the break is not part of the next line.
            if character == ' ' {
                continue;
            }
        }
        pieces.last_mut().expect("a piece always exists").push(character);
        used += cell;
    }
    pieces
}

fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    let width = area.width.saturating_mul(percent_x).saturating_div(100);
    let width = width.max(20).min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

fn format_time(ms: u64) -> String {
    if ms == 0 {
        return t("common.unknown");
    }
    match DateTime::<Utc>::from_timestamp_millis(ms as i64) {
        Some(time) => time.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string(),
        None => ms.to_string(),
    }
}

fn human_age(ms: u64) -> String {
    if ms == 0 {
        return t("common.unknown");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let seconds = now.saturating_sub(ms) / 1000;
    match seconds {
        0..=59 => t("time.just_now"),
        60..=3599 => tf("time.minutes_ago", &[("count", &(seconds / 60).to_string())]),
        3600..=86399 => tf("time.hours_ago", &[("count", &(seconds / 3600).to_string())]),
        86400..=2_591_999 => tf("time.days_ago", &[("count", &(seconds / 86400).to_string())]),
        _ => format_time(ms).split(' ').next().unwrap_or("").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::session::Session;
    use crate::source::Env;
    use crate::sources;
    use crate::sources::opencode::OpencodeData;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_app() -> App {
        let env = Env::isolated(
            PathBuf::from("C:/roster-nonexistent-home"),
            PathBuf::from("C:/roster-nonexistent-data"),
        );
        App::new(env, sources::registry(), None, None)
    }

    #[test]
    fn renders_all_modes_without_panicking() {
        let mut app = test_app();
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        app.mode = Mode::Help;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        app.mode = Mode::Rename;
        app.input = "new title".to_string();
        app.cursor = app.input.len();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        app.mode = Mode::ConfirmDelete;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }

    #[test]
    fn a_marked_session_is_drawn_with_a_checkbox() {
        let mut app = test_app();
        app.sessions = vec![Session {
            source: "opencode",
            id: "ses_marked".to_string(),
            cwd: "E:/code/x".to_string(),
            created_at: 1,
            mtime_ms: 2,
            title: "needs a checkbox".to_string(),
            title_source: "opencode".to_string(),
            parent: None,
            agent_preset: None,
            model: None,
            mounted_pid: None,
            data: Arc::new(()),
        }];
        app.visible = vec![0];
        app.list_state.select(Some(0));
        app.marked.insert("ses_marked".to_string());

        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("[x]"), "the marked row shows a checked box");
        assert!(
            text.contains("已选") || text.contains("marked"),
            "the header counts the marks"
        );
    }

    #[test]
    fn renders_an_opencode_session_in_the_detail_pane() {
        let mut app = test_app();
        app.sessions = vec![Session {
            source: "opencode",
            id: "ses_test".to_string(),
            cwd: "E:/code/x".to_string(),
            created_at: 1_700_000_000_000,
            mtime_ms: 1_700_000_100_000,
            title: "a database session".to_string(),
            title_source: "opencode".to_string(),
            parent: None,
            agent_preset: Some("build".to_string()),
            model: Some("deepseek/deepseek-flash".to_string()),
            mounted_pid: None,
            data: Arc::new(OpencodeData {
                db: PathBuf::from("C:/oc/opencode.db"),
                project_id: "proj".to_string(),
                worktree: Some("E:/code/x".to_string()),
                messages: 3,
                events: 9,
                cost: 0.0125,
                tokens_in: 10,
                tokens_out: 20,
                tokens_reasoning: 1,
                tokens_cache_read: 30,
                tokens_cache_write: 2,
            }),
        }];
        app.visible = vec![0];
        app.list_state.select(Some(0));

        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("deepseek/deepseek-flash"), "the model is shown");
        assert!(text.contains("OC"), "the source badge is shown");
        assert!(text.contains("Roster"), "the product name is in the header");

        // Every value starts in the same column, whatever the label's width.
        // The detail pane is the right 38% of a 120-column terminal.
        let detail_start = 74;
        let id_column = column_of(&terminal, "ses_test", detail_start).expect("the id is drawn");
        let cwd_column = column_of(&terminal, "E:/code/x", detail_start).expect("the workspace is drawn");
        let model_column = column_of(&terminal, "deepseek/deepseek-flash", detail_start).expect("the model is drawn");
        assert_eq!(id_column, cwd_column, "id and workspace values align");
        assert_eq!(id_column, model_column, "the model aligns with them too");
    }

    /// Display column at which `needle` starts, searching cells from `from_x`
    /// so a value shown in the list pane is not mistaken for the detail one.
    fn column_of(terminal: &Terminal<TestBackend>, needle: &str, from_x: u16) -> Option<usize> {
        let buffer = terminal.backend().buffer();
        let wanted: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
        for y in 0..buffer.area.height {
            for x in from_x..buffer.area.width {
                let fits = wanted.iter().enumerate().all(|(offset, symbol)| {
                    let at = x + offset as u16;
                    at < buffer.area.width && buffer[(at, y)].symbol() == symbol
                });
                if fits {
                    return Some(x as usize);
                }
            }
        }
        None
    }

    #[test]
    fn detail_rows_share_one_left_aligned_column() {
        let rows = vec![
            ("id".to_string(), "a".to_string()),
            ("创建时间".to_string(), "b".to_string()),
            ("messages / events".to_string(), "c".to_string()),
        ];
        let column = detail_column(&rows);
        assert_eq!(column, 18, "the widest label plus its colon");

        for (label, value) in &rows {
            let line = detail_line(label, value.clone(), column);
            let prefix: String = line.spans[..2].iter().map(|span| span.content.as_ref()).collect();
            assert_eq!(Line::from(prefix.as_str()).width(), column + 2, "{label}");
        }
    }

    #[test]
    fn long_values_wrap_under_the_value_column() {
        let rows = vec![("id".to_string(), "0123456789".repeat(5))];
        let column = detail_column(&rows);
        let inner_width = column + 2 + 12;
        let lines = detail_lines(rows, column, inner_width);

        assert!(lines.len() >= 4, "50 cells need several lines, got {}", lines.len());
        for continuation in &lines[1..] {
            let text: String = continuation.spans.iter().map(|span| span.content.as_ref()).collect();
            assert!(
                text.starts_with(&" ".repeat(column + 2)),
                "the wrapped part stays in the value column: {text:?}"
            );
            assert!(Line::from(text.as_str()).width() <= inner_width);
        }
    }

    #[test]
    fn wrapping_counts_display_cells_not_chars() {
        // Six CJK glyphs are twelve cells, so a twelve-cell line holds them all
        // while an eleven-cell line must break.
        assert_eq!(wrap_to_width("一二三四五六", 12), ["一二三四五六"]);
        assert_eq!(wrap_to_width("一二三四五六", 11), ["一二三四五", "六"]);
        assert_eq!(wrap_to_width("", 5), [""]);
    }

    #[test]
    fn renders_in_both_languages() {
        let mut app = test_app();
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();

        let _guard = i18n::TEST_LOCK.lock().unwrap();
        let before = i18n::language();
        i18n::set(i18n::Language::Chinese);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        i18n::set(i18n::Language::English);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        i18n::set(before);
    }
}
