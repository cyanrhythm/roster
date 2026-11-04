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
        Mode::Rename => draw_input_popup(frame, app, &t("popup.rename.title"), &t("popup.rename.hint")),
        Mode::Workspace => {
            let open = app
                .current()
                .map(|session| app.mounted.contains_key(&session.id))
                .unwrap_or(false);
            let hint = if open {
                t("popup.workspace.hint_open")
            } else {
                t("popup.workspace.hint")
            };
            draw_input_popup(frame, app, &t("popup.workspace.title"), &hint);
        }
        Mode::ConfirmDelete => draw_delete_popup(frame, app),
        Mode::Help => draw_help(frame),
        Mode::Normal => {}
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
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
        Span::styled(
            tf("header.language", &[("label", i18n::language().label())]),
            Style::default().fg(Color::DarkGray),
        ),
    ];
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

    let roots = app
        .paths
        .session_roots
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("  ·  ");

    let text = Text::from(vec![
        Line::from(spans),
        Line::from(Span::styled(
            tf("header.root", &[("roots", &roots)]),
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
            let open = if app.mounted.contains_key(&session.id) {
                t("list.open_badge")
            } else {
                String::new()
            };
            let title = Line::from(vec![Span::styled(
                format!("{}{}", session.display_title(), open),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            )]);
            let meta = Line::from(vec![Span::styled(
                format!(
                    "{}  ·  {}  ·  {}  ·  {}",
                    session.cwd,
                    session.short_id(),
                    human_size(session.size),
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

    let mut lines: Vec<Line> = vec![
        kv(&t("detail.id"), session.id.clone()),
        kv(&t("detail.title_label"), session.title.clone()),
        kv(&t("detail.workspace"), session.cwd.clone()),
        kv(&t("detail.created"), format_time(session.created_at)),
        kv(&t("detail.updated"), format_time(session.mtime_ms)),
        kv(&t("detail.size"), human_size(session.size)),
    ];
    let encoding = if session.compressed {
        t("detail.encoding.zstd")
    } else {
        t("detail.encoding.plain")
    };
    lines.push(kv(
        &t("detail.log"),
        tf(
            "detail.log_value",
            &[
                ("name", &session.log_name),
                ("encoding", &encoding),
                ("count", &session.frames.to_string()),
            ],
        ),
    ));
    if let Some(parent) = &session.parent {
        lines.push(kv(&t("detail.parent"), parent.clone()));
    }
    if let Some(preset) = &session.agent_preset {
        lines.push(kv(&t("detail.preset"), preset.clone()));
    }
    if let Some(model) = &session.model {
        lines.push(kv(&t("detail.model"), model.clone()));
    }
    lines.push(kv(&t("detail.title_source"), title_source_label(&session.title_source)));
    lines.push(kv(&t("detail.dir"), session.dir.display().to_string()));

    if let Some(pid) = app.mounted.get(&session.id) {
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
    let area = centered(frame.area(), 70, 9);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("popup.delete.title")))
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    if let Some(session) = app.current() {
        lines.push(Line::from(Span::styled(
            session.display_title(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("{}  ·  {}", session.id, session.cwd),
            Style::default().fg(Color::Gray),
        )));
        if let Some(pid) = app.mounted.get(&session.id) {
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
    frame.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), inner);
}

fn draw_help(frame: &mut Frame) {
    let area = centered(frame.area(), 72, 19);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("help.title")))
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let lines = vec![
        Line::from(Span::styled(t("help.nav"), header)),
        Line::from(t("help.nav_move")),
        Line::from(t("help.nav_top")),
        Line::from(t("help.nav_page")),
        Line::from(t("help.nav_filter")),
        Line::from(""),
        Line::from(Span::styled(t("help.actions"), header)),
        Line::from(t("help.action_rename")),
        Line::from(t("help.action_workspace")),
        Line::from(t("help.action_delete")),
        Line::from(t("help.action_reload")),
        Line::from(t("help.action_language")),
        Line::from(t("help.action_quit")),
        Line::from(""),
        Line::from(Span::styled(t("help.footer"), Style::default().fg(Color::DarkGray))),
    ];
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn title_source_label(source: &str) -> String {
    let key = match source {
        "renamed" => "title_source.renamed",
        "auto" => "title_source.auto",
        "log" => "title_source.log",
        "prompt" => "title_source.prompt",
        _ => "title_source.fallback",
    };
    t(key)
}

fn kv(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:>13}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
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

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::paths::Paths;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    #[test]
    fn renders_all_modes_without_panicking() {
        let paths = Paths::discover(
            Some(PathBuf::from("C:/dstsm-nonexistent-dsh")),
            Some(PathBuf::from("C:/dstsm-nonexistent-data")),
            vec![PathBuf::from("C:/dstsm-nonexistent-sessions")],
        );
        let mut app = App::new(paths, None);
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
    fn renders_in_both_languages() {
        let paths = Paths::discover(
            Some(PathBuf::from("C:/dstsm-nonexistent-dsh")),
            Some(PathBuf::from("C:/dstsm-nonexistent-data")),
            vec![PathBuf::from("C:/dstsm-nonexistent-sessions")],
        );
        let mut app = App::new(paths, None);
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
