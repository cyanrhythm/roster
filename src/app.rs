use std::collections::BTreeSet;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::i18n;
use crate::session::Session;
use crate::settings;
use crate::source::{Env, Registry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Rename,
    Workspace,
    ConfirmDelete,
    Help,
}

/// Outcome of running one mutation over every selected session.
struct Batch {
    done: usize,
    failed: usize,
    first_error: Option<String>,
    /// Whatever the first successful action wanted shown (a normalised path).
    first_value: Option<String>,
}

pub struct App {
    pub env: Env,
    pub registry: Registry,
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
    pub errors: Vec<String>,
    /// Where every source looked, resolved on reload (language-independent).
    pub locations: Vec<String>,
    /// Sessions picked out for a batch action, by id so they survive reloads.
    pub marked: BTreeSet<String>,
    pub language_choice: Option<i18n::Language>,
    /// Id of the source to show; `None` shows every one.
    pub source: Option<&'static str>,
    pub quit: bool,
}

impl App {
    pub fn new(
        env: Env,
        registry: Registry,
        language_choice: Option<i18n::Language>,
        source: Option<&'static str>,
    ) -> Self {
        let mut app = Self {
            env,
            registry,
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
            errors: Vec::new(),
            locations: Vec::new(),
            marked: BTreeSet::new(),
            language_choice,
            source,
            quit: false,
        };
        app.reload(None);
        app
    }

    /// What an action applies to: the marks when there are any, otherwise just
    /// the one under the cursor. This is why single-session behaviour is
    /// unchanged when nothing is marked.
    pub fn targets(&self) -> Vec<Session> {
        if self.marked.is_empty() {
            return self.current().cloned().into_iter().collect();
        }
        self.sessions
            .iter()
            .filter(|session| self.marked.contains(&session.id))
            .cloned()
            .collect()
    }

    pub fn target_count(&self) -> usize {
        if self.marked.is_empty() {
            usize::from(self.current().is_some())
        } else {
            self.marked.len()
        }
    }

    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    pub fn is_marked(&self, id: &str) -> bool {
        self.marked.contains(id)
    }

    /// The value every target agrees on, or `None` when they disagree — an
    /// empty field is the honest prompt for "you have to tell me".
    fn common_target_value(&self, value: impl Fn(&Session) -> &str) -> Option<String> {
        let targets = self.targets();
        let first = targets.first()?;
        let shared = value(first).to_string();
        targets.iter().all(|session| value(session) == shared).then_some(shared)
    }

    /// How many sessions each registered source produced, in registry order.
    pub fn source_counts(&self) -> Vec<(&'static str, usize)> {
        let mut counts: Vec<(&'static str, usize)> =
            self.registry.sources().iter().map(|source| (source.id(), 0)).collect();
        for session in &self.sessions {
            if let Some(entry) = counts.iter_mut().find(|(id, _)| *id == session.source) {
                entry.1 += 1;
            }
        }
        counts
    }

    pub fn source_label(&self) -> String {
        match self.source {
            None => i18n::t("source.all"),
            Some(id) => self.registry.label(id),
        }
    }

    pub fn reload(&mut self, keep_id: Option<String>) {
        let result = self.registry.load(&self.env);
        self.sessions = result.sessions;
        self.errors = result.errors;
        self.locations = self.registry.locations(&self.env);
        // Marks follow ids, so anything deleted (here or elsewhere) drops out.
        let sessions = &self.sessions;
        self.marked
            .retain(|id| sessions.iter().any(|session| &session.id == id));
        self.rebuild_visible();
        if let Some(id) = keep_id {
            self.select_id(&id);
        }
        if !self.errors.is_empty() {
            let count = self.errors.len().to_string();
            self.set_error(i18n::tf("msg.load_errors", &[("count", &count)]));
        }
    }

    /// Picks the session under the cursor in or out of the batch.
    fn toggle_mark(&mut self) {
        let Some(session) = self.current().cloned() else {
            return;
        };
        if !self.marked.remove(&session.id) {
            self.marked.insert(session.id.clone());
        }
        self.report_marks();
    }

    /// `a`: everything currently listed, or nothing when it is all marked.
    fn toggle_mark_all(&mut self) {
        let visible: Vec<String> = self
            .visible
            .iter()
            .map(|index| self.sessions[*index].id.clone())
            .collect();
        if !visible.is_empty() && visible.iter().all(|id| self.marked.contains(id)) {
            for id in &visible {
                self.marked.remove(id);
            }
        } else {
            self.marked.extend(visible);
        }
        self.report_marks();
    }

    fn report_marks(&mut self) {
        let count = self.marked.len().to_string();
        let key = if self.marked.is_empty() {
            "msg.marked_none"
        } else {
            "msg.marked"
        };
        self.set_info(i18n::tf(key, &[("count", &count)]));
    }

    /// Runs one mutation over the targets, collecting failures instead of
    /// stopping at the first one: a batch should not leave the rest untouched
    /// because one session went missing.
    fn run_batch(
        &self,
        targets: &[Session],
        mut action: impl FnMut(&Registry, &Env, &Session) -> Result<Option<String>>,
    ) -> Batch {
        let mut batch = Batch {
            done: 0,
            failed: 0,
            first_error: None,
            first_value: None,
        };
        for session in targets {
            match action(&self.registry, &self.env, session) {
                Ok(value) => {
                    batch.done += 1;
                    if batch.first_value.is_none() {
                        batch.first_value = value;
                    }
                }
                Err(error) => {
                    batch.failed += 1;
                    if batch.first_error.is_none() {
                        batch.first_error = Some(error.to_string());
                    }
                }
            }
        }
        batch
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
            .filter(|(_, session)| self.source.is_none_or(|id| id == session.source))
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
            KeyCode::Char('s') => self.cycle_source(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('a') => self.toggle_mark_all(),
            KeyCode::Char('L') => self.toggle_language(),
            KeyCode::Char('/') => {
                self.filter_backup = self.filter.clone();
                self.begin_input(Mode::Filter, self.filter.clone());
            }
            KeyCode::Char('r') => {
                if self.current().is_some() {
                    // Several sessions at once get the title they share, or an
                    // empty field when they do not share one.
                    let title = self
                        .common_target_value(|session| session.title.as_str())
                        .unwrap_or_default();
                    self.begin_input(Mode::Rename, title);
                }
            }
            KeyCode::Char('w') => {
                if self.current().is_some() {
                    let cwd = self
                        .common_target_value(|session| session.cwd.as_str())
                        .unwrap_or_default();
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
                if key.modifiers.contains(KeyModifiers::CONTROL) && character.eq_ignore_ascii_case(&'w') =>
            {
                self.clear_word();
                self.filter = self.input.clone();
                self.rebuild_visible();
            }
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

        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let keep = self.current().map(|session| session.id.clone());

        match mode {
            Mode::Rename => {
                let title = value.trim().to_string();
                let batch = self.run_batch(&targets, |registry, env, session| {
                    registry.rename(env, session, &title).map(|_| None)
                });
                self.reload(keep);
                let done = batch.done.to_string();
                if batch.failed > 0 {
                    self.report_partial(&batch);
                } else if batch.done > 1 {
                    self.set_info(i18n::tf("msg.renamed_many", &[("count", &done), ("title", &title)]));
                } else {
                    self.set_info(i18n::tf("msg.renamed", &[("title", &title)]));
                }
            }
            Mode::Workspace => {
                let batch = self.run_batch(&targets, |registry, env, session| {
                    registry.change_workspace(env, session, &value).map(Some)
                });
                self.reload(keep);
                let done = batch.done.to_string();
                let cwd = batch.first_value.clone().unwrap_or_else(|| value.trim().to_string());
                if batch.failed > 0 {
                    self.report_partial(&batch);
                } else if batch.done > 1 {
                    self.set_info(i18n::tf(
                        "msg.workspace_changed_many",
                        &[("count", &done), ("cwd", &cwd)],
                    ));
                } else {
                    self.set_info(i18n::tf("msg.workspace_changed", &[("cwd", &cwd)]));
                }
            }
            _ => {}
        }
    }

    /// all → first source → … → last source → all.
    fn cycle_source(&mut self) {
        let ids: Vec<&'static str> = self.registry.ids().collect();
        let next = match self.source {
            None => ids.first().copied(),
            Some(current) => {
                let position = ids.iter().position(|id| *id == current).unwrap_or(0);
                ids.get(position + 1).copied()
            }
        };
        self.source = next;
        self.rebuild_visible();
        let label = self.source_label();
        self.set_info(i18n::tf("msg.source", &[("label", &label)]));
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
        let _ = settings::save_choice(&self.env.data_dir, next);
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
                let targets = self.targets();
                if targets.is_empty() {
                    self.mode = Mode::Normal;
                    return;
                }
                self.mode = Mode::Normal;
                let single_title = targets[0].display_title();
                let batch = self.run_batch(&targets, |registry, env, session| {
                    registry.delete(env, session).map(|_| None)
                });
                self.reload(None);
                if batch.failed > 0 {
                    self.report_partial(&batch);
                } else if batch.done > 1 {
                    let count = batch.done.to_string();
                    self.set_info(i18n::tf("msg.deleted_many", &[("count", &count)]));
                } else {
                    self.set_info(i18n::tf("msg.deleted", &[("title", &single_title)]));
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn report_partial(&mut self, batch: &Batch) {
        let done = batch.done.to_string();
        let failed = batch.failed.to_string();
        let error = batch.first_error.clone().unwrap_or_default();
        self.set_error(i18n::tf(
            "msg.batch_partial",
            &[("done", &done), ("failed", &failed), ("error", &error)],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::session::Session;
    use crate::source::{SessionSource, SourceLoad};

    /// A source that keeps its sessions in memory and records what was done to
    /// them, so the batch logic can be checked without touching a disk.
    #[derive(Clone)]
    struct Stub {
        sessions: Vec<Session>,
        deleted: Arc<Mutex<Vec<String>>>,
        moved: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl SessionSource for Stub {
        fn id(&self) -> &'static str {
            "stub"
        }

        fn label_key(&self) -> &'static str {
            "source.dst"
        }

        fn badge_key(&self) -> &'static str {
            "list.badge.dst"
        }

        fn load(&self, _env: &Env) -> SourceLoad {
            let deleted = self.deleted.lock().unwrap();
            SourceLoad {
                sessions: self
                    .sessions
                    .iter()
                    .filter(|session| !deleted.contains(&session.id))
                    .cloned()
                    .collect(),
                errors: Vec::new(),
            }
        }

        fn rename(&self, _env: &Env, _session: &Session, _title: &str) -> Result<()> {
            Ok(())
        }

        fn delete(&self, _env: &Env, session: &Session) -> Result<()> {
            self.deleted.lock().unwrap().push(session.id.clone());
            Ok(())
        }

        fn change_workspace(&self, _env: &Env, session: &Session, cwd: &str) -> Result<String> {
            self.moved.lock().unwrap().push((session.id.clone(), cwd.to_string()));
            Ok(cwd.to_string())
        }
    }

    fn session(id: &str, cwd: &str) -> Session {
        Session {
            source: "stub",
            id: id.to_string(),
            title: format!("title {id}"),
            title_source: "log".to_string(),
            cwd: cwd.to_string(),
            created_at: 0,
            mtime_ms: 0,
            parent: None,
            agent_preset: None,
            model: None,
            mounted_pid: None,
            data: Arc::new(()),
        }
    }

    struct Recorder {
        deleted: Arc<Mutex<Vec<String>>>,
        moved: Arc<Mutex<Vec<(String, String)>>>,
    }

    fn app_with(sessions: Vec<Session>) -> (App, Recorder) {
        let recorder = Recorder {
            deleted: Arc::new(Mutex::new(Vec::new())),
            moved: Arc::new(Mutex::new(Vec::new())),
        };
        let mut registry = Registry::new();
        registry.register(Stub {
            sessions,
            deleted: recorder.deleted.clone(),
            moved: recorder.moved.clone(),
        });
        let env = Env::isolated(
            PathBuf::from("C:/roster-nonexistent"),
            PathBuf::from("C:/roster-nonexistent"),
        );
        let app = App::new(env, registry, None, None);
        (app, recorder)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn targets_are_the_marks_or_just_the_cursor() {
        let (mut app, _) = app_with(vec![session("a", "E:/x"), session("b", "E:/x"), session("c", "E:/y")]);
        assert_eq!(app.targets().len(), 1, "nothing marked: the cursor is the target");
        assert_eq!(app.targets()[0].id, "a");

        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' '));
        let ids: Vec<String> = app.targets().iter().map(|session| session.id.clone()).collect();
        assert_eq!(ids, ["a", "b"]);

        // The marks win even when the cursor is somewhere else.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.targets().len(), 2);
        assert_eq!(app.target_count(), 2);

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.targets().len(), 3, "c joins the batch");

        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.targets().len(), 1, "clearing the marks leaves just the cursor");
        assert_eq!(app.targets()[0].id, "c");
    }

    #[test]
    fn marking_everything_twice_clears_the_selection() {
        let (mut app, _) = app_with(vec![session("a", "E:/x"), session("b", "E:/y")]);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.marked_count(), 2);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.marked_count(), 0);
    }

    #[test]
    fn a_shared_workspace_is_prefilled_and_a_mixed_one_is_not() {
        let (mut app, _) = app_with(vec![
            session("a", "E:/same"),
            session("b", "E:/same"),
            session("c", "E:/other"),
        ]);

        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.mode, Mode::Workspace);
        assert_eq!(app.input, "E:/same", "both live in the same workspace");

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.mode, Mode::Workspace);
        assert_eq!(app.input, "", "the workspaces differ, so the field starts empty");
    }

    #[test]
    fn moving_a_selection_touches_every_marked_session() {
        let (mut app, recorder) = app_with(vec![session("a", "E:/old"), session("b", "E:/old")]);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.input, "E:/old");
        app.input = "E:/new".to_string();
        app.cursor = app.input.len();
        press(&mut app, KeyCode::Enter);

        let moved = recorder.moved.lock().unwrap().clone();
        assert_eq!(moved.len(), 2);
        assert!(moved.iter().all(|(_, cwd)| cwd == "E:/new"));
        assert!(
            app.status.contains("2"),
            "the status reports the batch size: {}",
            app.status
        );
    }

    #[test]
    fn deleting_a_selection_removes_every_marked_session() {
        let (mut app, recorder) = app_with(vec![session("a", "E:/x"), session("b", "E:/x"), session("c", "E:/y")]);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.mode, Mode::ConfirmDelete);
        press(&mut app, KeyCode::Char('y'));

        let deleted = recorder.deleted.lock().unwrap().clone();
        assert_eq!(deleted, ["a", "b"], "everything marked, not the cursor only");
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].id, "c");
        assert_eq!(app.marked_count(), 0, "marks of deleted sessions are pruned");
    }

    #[test]
    fn deleting_with_nothing_marked_still_deletes_one() {
        let (mut app, recorder) = app_with(vec![session("a", "E:/x"), session("b", "E:/y")]);
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(recorder.deleted.lock().unwrap().clone(), ["a"]);
        assert_eq!(app.sessions.len(), 1);
    }
}
