use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::i18n;
use crate::mutate;
use crate::paths::Paths;
use crate::settings;
use crate::store::{self, Session};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Rename,
    Workspace,
    ConfirmDelete,
    Help,
}

pub struct App {
    pub paths: Paths,
    pub sessions: Vec<Session>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub list_state: ListState,
    pub filter: String,
    pub mode: Mode,
    pub input: String,
    pub cursor: usize,
    filter_backup: String,
    pub status: String,
    pub status_error: bool,
    pub mounted: HashMap<String, u32>,
    pub errors: Vec<String>,
    pub language_choice: Option<i18n::Language>,
    pub quit: bool,
}

impl App {
    pub fn new(paths: Paths, language_choice: Option<i18n::Language>) -> Self {
        let mut app = Self {
            paths,
            sessions: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            filter: String::new(),
            mode: Mode::Normal,
            input: String::new(),
            cursor: 0,
            filter_backup: String::new(),
            status: String::new(),
            status_error: false,
            mounted: HashMap::new(),
            errors: Vec::new(),
            language_choice,
            quit: false,
        };
        app.reload(None);
        app
    }

    pub fn reload(&mut self, keep_id: Option<String>) {
        let result = store::load(&self.paths);
        self.sessions = result.sessions;
        self.errors = result.errors;
        self.mounted = result.mounted;
        self.rebuild_visible();
        if let Some(id) = keep_id {
            self.select_id(&id);
        }
        if !self.errors.is_empty() {
            let count = self.errors.len().to_string();
            self.set_error(i18n::tf("msg.load_errors", &[("count", &count)]));
        }
    }

    pub fn current(&self) -> Option<&Session> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.sessions.get(*index))
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                needle.is_empty()
                    || session.title.to_lowercase().contains(&needle)
                    || session.cwd.to_lowercase().contains(&needle)
                    || session.id.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.visible.is_empty() {
            self.selected = 0;
            self.list_state.select(None);
            return;
        }
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
        self.list_state.select(Some(self.selected));
    }

    fn select_id(&mut self, id: &str) {
        if let Some(position) = self.visible.iter().position(|index| self.sessions[*index].id == id) {
            self.selected = position;
        }
        self.clamp_selection();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let max = self.visible.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, max);
        self.selected = next as usize;
        self.list_state.select(Some(self.selected));
    }

    fn set_info(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_error = false;
    }

    fn set_error(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_error = true;
    }

    fn begin_input(&mut self, mode: Mode, value: String) {
        self.input = value;
        self.cursor = self.input.len();
        self.mode = mode;
    }

    fn insert_char(&mut self, character: char) {
        self.input.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.input[..self.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap();
        self.input.remove(previous);
        self.cursor = previous;
    }

    fn delete_forward(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(index, _)| index)
                .unwrap();
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.input.len() {
            let character = self.input[self.cursor..].chars().next().unwrap();
            self.cursor += character.len_utf8();
        }
    }

    fn clear_word(&mut self) {
        while self.cursor > 0 {
            let previous = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(index, _)| index)
                .unwrap();
            let character = self.input[previous..self.cursor].chars().next().unwrap();
            if character.is_whitespace() {
                break;
            }
            self.input.remove(previous);
            self.cursor = previous;
        }
        while self.cursor > 0 {
            let previous = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(index, _)| index)
                .unwrap();
            let character = self.input[previous..self.cursor].chars().next().unwrap();
            if !character.is_whitespace() {
                break;
            }
            self.input.remove(previous);
            self.cursor = previous;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.quit = true;
            return;
        }

        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Filter => self.handle_filter(key),
            Mode::Rename | Mode::Workspace => self.handle_text_mode(key),
            Mode::ConfirmDelete => self.handle_delete_confirm(key),
            Mode::Help => self.mode = Mode::Normal,
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Char('g') | KeyCode::Home => {
                self.selected = 0;
                self.clamp_selection();
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.selected = self.visible.len().saturating_sub(1);
                self.clamp_selection();
            }
            KeyCode::Char('R') => {
                self.reload(self.current().map(|session| session.id.clone()));
                self.set_info(i18n::t("msg.reloaded"));
            }
            KeyCode::Char('L') => self.toggle_language(),
            KeyCode::Char('/') => {
                self.filter_backup = self.filter.clone();
                self.begin_input(Mode::Filter, self.filter.clone());
            }
            KeyCode::Char('r') => {
                if let Some(session) = self.current() {
                    let title = session.title.clone();
                    self.begin_input(Mode::Rename, title);
                }
            }
            KeyCode::Char('w') => {
                if let Some(session) = self.current() {
                    let cwd = session.cwd.clone();
                    self.begin_input(Mode::Workspace, cwd);
                }
            }
            KeyCode::Char('d') => {
                if self.current().is_some() {
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn handle_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter = self.filter_backup.clone();
                self.mode = Mode::Normal;
                self.rebuild_visible();
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.backspace();
                self.filter = self.input.clone();
                self.rebuild_visible();
            }
            KeyCode::Delete => {
                self.delete_forward();
                self.filter = self.input.clone();
                self.rebuild_visible();
            }
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Char(character)
                if key.modifiers.contains(KeyModifiers::CONTROL) && character.eq_ignore_ascii_case(&'u') =>
            {
                self.input.clear();
                self.cursor = 0;
                self.filter = self.input.clone();
                self.rebuild_visible();
            }
            KeyCode::Char(character) => {
                self.insert_char(character);
                self.filter = self.input.clone();
                self.rebuild_visible();
            }
            _ => {}
        }
    }

    fn handle_text_mode(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Char(character)
                if key.modifiers.contains(KeyModifiers::CONTROL) && character.eq_ignore_ascii_case(&'w') =>
            {
                self.clear_word();
            }
            KeyCode::Char(character)
                if key.modifiers.contains(KeyModifiers::CONTROL) && character.eq_ignore_ascii_case(&'u') =>
            {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert_char(character);
            }
            _ => {}
        }
    }

    fn submit_input(&mut self) {
        let value = self.input.clone();
        let mode = self.mode;
        self.mode = Mode::Normal;

        match mode {
            Mode::Rename => {
                let Some(session) = self.current().cloned() else {
                    return;
                };
                match mutate::rename(&self.paths, &session, &value) {
                    Ok(()) => {
                        let id = session.id.clone();
                        let title = value.trim().to_string();
                        self.reload(Some(id));
                        self.set_info(i18n::tf("msg.renamed", &[("title", &title)]));
                    }
                    Err(error) => self.set_error(i18n::tf("msg.rename_failed", &[("error", &error.to_string())])),
                }
            }
            Mode::Workspace => {
                let Some(session) = self.current().cloned() else {
                    return;
                };
                match mutate::change_workspace(&self.paths, &session, &value) {
                    Ok(new_cwd) => {
                        let id = session.id.clone();
                        self.reload(Some(id));
                        self.set_info(i18n::tf("msg.workspace_changed", &[("cwd", &new_cwd)]));
                    }
                    Err(error) => self.set_error(i18n::tf("msg.workspace_failed", &[("error", &error.to_string())])),
                }
            }
            _ => {}
        }
    }

    fn toggle_language(&mut self) {
        let next = match self.language_choice {
            None => Some(i18n::Language::ALL[0]),
            Some(current) => {
                let index = i18n::Language::ALL
                    .iter()
                    .position(|item| *item == current)
                    .unwrap_or(0);
                i18n::Language::ALL.get(index + 1).copied()
            }
        };
        let active = next.unwrap_or_else(i18n::Language::detect);
        i18n::set(active);
        let _ = settings::save_choice(&self.paths.data_dir, next);
        self.language_choice = next;

        let label = active.label().to_string();
        let key = if next.is_some() {
            "msg.language"
        } else {
            "msg.language_auto"
        };
        self.set_info(i18n::tf(key, &[("label", &label)]));
    }

    fn handle_delete_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let Some(session) = self.current().cloned() else {
                    self.mode = Mode::Normal;
                    return;
                };
                self.mode = Mode::Normal;
                match mutate::delete(&self.paths, &session) {
                    Ok(trash) => {
                        self.reload(None);
                        self.set_info(i18n::tf("msg.deleted", &[("path", &trash.display().to_string())]));
                    }
                    Err(error) => self.set_error(i18n::tf("msg.delete_failed", &[("error", &error.to_string())])),
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }
}
