//! The plug-in boundary: everything the tool needs from a session store.
//!
//! The core (`app`, `ui`, `main`) only ever talks to [`Registry`]; adding an
//! agent means writing one [`SessionSource`] implementation and registering it
//! in [`crate::sources::registry`]. Nothing in the core knows what a DST log or
//! an opencode database looks like.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::i18n::t;
use crate::session::{DetailRow, Session};

/// Where to look, resolved once from the command line and the environment.
///
/// Sources read only what matters to them: DST asks for a home and extra roots,
/// opencode for data directories. Keys are source ids, so a new source can be
/// pointed somewhere with `--source-dir <id>=<path>` without touching the CLI.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    /// The tool's own state: preferences and caches. Not session data.
    pub data_dir: PathBuf,
    homes: HashMap<String, PathBuf>,
    dirs: HashMap<String, Vec<PathBuf>>,
    /// Whether sources may fall back to `$DSH_HOME`, `$XDG_DATA_HOME` and
    /// friends. Tests turn this off so they never depend on the machine.
    use_environment: bool,
}

impl Env {
    pub fn new(home: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            home,
            data_dir,
            homes: HashMap::new(),
            dirs: HashMap::new(),
            use_environment: true,
        }
    }

    /// An `Env` that ignores the process environment, for tests.
    #[cfg(test)]
    pub fn isolated(home: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            use_environment: false,
            ..Self::new(home, data_dir)
        }
    }

    /// A source's own "home" directory (`--dsh-home`, `--source-home`).
    pub fn with_home(mut self, source: impl Into<String>, path: PathBuf) -> Self {
        self.homes.insert(source.into(), path);
        self
    }

    /// An extra root to scan (`--sessions-root`, `--opencode-dir`, `--source-dir`).
    pub fn with_dir(mut self, source: impl Into<String>, path: PathBuf) -> Self {
        self.dirs.entry(source.into()).or_default().push(path);
        self
    }

    pub fn home_for(&self, source: &str) -> Option<&Path> {
        self.homes.get(source).map(PathBuf::as_path)
    }

    pub fn dirs_for(&self, source: &str) -> &[PathBuf] {
        self.dirs.get(source).map(Vec::as_slice).unwrap_or_default()
    }

    /// An environment variable a source wants to honour, when allowed to.
    pub fn env_path(&self, key: &str) -> Option<PathBuf> {
        if self.use_environment {
            crate::source::env_path(key)
        } else {
            None
        }
    }
}

/// One source's answer to "what do you have?".
#[derive(Default)]
pub struct SourceLoad {
    pub sessions: Vec<Session>,
    pub errors: Vec<String>,
}

/// A kind of agent session store.
///
/// Implementations own their storage format completely: how sessions are
/// discovered, which fields they have, and how the three mutations work. The
/// default methods exist so a minimal source only has to implement `load` and
/// the mutations; the display hooks are what make the detail pane rich.
pub trait SessionSource {
    /// Stable slug used by `--source`, `--json`, and the i18n keys below.
    fn id(&self) -> &'static str;

    /// i18n key for the source's name in sentences (`source.dst`).
    fn label_key(&self) -> &'static str;

    /// i18n key for the short badge in the list (`list.badge.dst`).
    fn badge_key(&self) -> &'static str;

    /// RGB accent for the list badge.
    fn accent(&self) -> [u8; 3] {
        [0x4a, 0xa9, 0xdd]
    }

    /// Human-readable places this source looks in, shown in the header.
    fn locations(&self, _env: &Env) -> Vec<String> {
        Vec::new()
    }

    fn load(&self, env: &Env) -> SourceLoad;

    fn rename(&self, env: &Env, session: &Session, title: &str) -> Result<()>;

    fn delete(&self, env: &Env, session: &Session) -> Result<()>;

    fn change_workspace(&self, env: &Env, session: &Session, new_cwd: &str) -> Result<String>;

    /// Extra detail-pane rows, built fresh on every draw.
    fn detail(&self, _session: &Session) -> Vec<DetailRow> {
        Vec::new()
    }

    /// The third column of a list row: bytes for a log, message count for a
    /// database row. Empty when the source has nothing to say.
    fn measurement(&self, _session: &Session) -> String {
        String::new()
    }

    /// Source-specific fields for `--list --json`.
    fn json_fields(&self, _session: &Session) -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }
}

#[derive(Default)]
pub struct LoadResult {
    pub sessions: Vec<Session>,
    pub errors: Vec<String>,
}

/// Every source the tool knows about, in display order.
pub struct Registry {
    sources: Vec<Arc<dyn SessionSource>>,
}

impl Registry {
    pub fn new() -> Self {
        Self { sources: Vec::new() }
    }

    pub fn register(&mut self, source: impl SessionSource + 'static) -> &mut Self {
        self.sources.push(Arc::new(source));
        self
    }

    pub fn sources(&self) -> &[Arc<dyn SessionSource>] {
        &self.sources
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn SessionSource>> {
        self.sources.iter().find(|source| source.id() == id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources.iter().map(|source| source.id())
    }

    /// The source's name, translated.
    pub fn label(&self, id: &str) -> String {
        match self.get(id) {
            Some(source) => t(source.label_key()),
            None => id.to_string(),
        }
    }

    pub fn badge(&self, id: &str) -> String {
        match self.get(id) {
            Some(source) => t(source.badge_key()),
            None => id.to_uppercase(),
        }
    }

    pub fn accent(&self, id: &str) -> [u8; 3] {
        self.get(id).map(|source| source.accent()).unwrap_or([0x4a, 0xa9, 0xdd])
    }

    /// Loads every source. A session id belongs to the first source that
    /// produces it, which is what makes `--source-dir` overrides safe.
    pub fn load(&self, env: &Env) -> LoadResult {
        let mut result = LoadResult::default();
        let mut seen: HashSet<String> = HashSet::new();

        for source in &self.sources {
            let loaded = source.load(env);
            for session in loaded.sessions {
                if seen.insert(session.id.clone()) {
                    result.sessions.push(session);
                }
            }
            result.errors.extend(loaded.errors);
        }

        result
            .sessions
            .sort_by_key(|session| std::cmp::Reverse(session.mtime_ms));
        result
    }

    fn source_for(&self, session: &Session) -> Result<&Arc<dyn SessionSource>> {
        self.get(session.source).ok_or_else(|| anyhow!(t("err.wrong_source")))
    }

    pub fn locations(&self, env: &Env) -> Vec<String> {
        self.sources.iter().flat_map(|source| source.locations(env)).collect()
    }

    pub fn detail(&self, session: &Session) -> Vec<DetailRow> {
        match self.source_for(session) {
            Ok(source) => source.detail(session),
            Err(_) => Vec::new(),
        }
    }

    pub fn measurement(&self, session: &Session) -> String {
        match self.source_for(session) {
            Ok(source) => source.measurement(session),
            Err(_) => String::new(),
        }
    }

    pub fn json_fields(&self, session: &Session) -> serde_json::Map<String, Value> {
        match self.source_for(session) {
            Ok(source) => source.json_fields(session),
            Err(_) => serde_json::Map::new(),
        }
    }

    pub fn rename(&self, env: &Env, session: &Session, title: &str) -> Result<()> {
        self.source_for(session)?.rename(env, session, title)
    }

    pub fn delete(&self, env: &Env, session: &Session) -> Result<()> {
        self.source_for(session)?.delete(env, session)
    }

    pub fn change_workspace(&self, env: &Env, session: &Session, new_cwd: &str) -> Result<String> {
        self.source_for(session)?.change_workspace(env, session, new_cwd)
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// A path from an environment variable, if it is set and not empty.
pub fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Keeps the first occurrence of each path, compared case-insensitively so a
/// Windows drive letter cannot smuggle a duplicate through.
pub fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<String> = Vec::new();
    paths
        .into_iter()
        .filter(|path| {
            let key = path.display().to_string().to_lowercase();
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc as StdArc;

    use crate::session::Session;

    struct Stub {
        id: &'static str,
        sessions: Vec<Session>,
        errors: Vec<String>,
    }

    impl SessionSource for Stub {
        fn id(&self) -> &'static str {
            self.id
        }

        fn label_key(&self) -> &'static str {
            "source.dst"
        }

        fn badge_key(&self) -> &'static str {
            "list.badge.dst"
        }

        fn load(&self, _env: &Env) -> SourceLoad {
            SourceLoad {
                sessions: self.sessions.clone(),
                errors: self.errors.clone(),
            }
        }

        fn rename(&self, _env: &Env, _session: &Session, _title: &str) -> Result<()> {
            Ok(())
        }

        fn delete(&self, _env: &Env, _session: &Session) -> Result<()> {
            Ok(())
        }

        fn change_workspace(&self, _env: &Env, _session: &Session, new_cwd: &str) -> Result<String> {
            Ok(new_cwd.to_string())
        }
    }

    fn sample(id: &str, mtime_ms: u64) -> Session {
        Session {
            source: "one",
            id: id.to_string(),
            title: id.to_string(),
            title_source: "opencode".to_string(),
            cwd: String::new(),
            created_at: 0,
            mtime_ms,
            parent: None,
            agent_preset: None,
            model: None,
            mounted_pid: None,
            data: StdArc::new(()),
        }
    }

    fn registry() -> Registry {
        let mut registry = Registry::new();
        registry.register(Stub {
            id: "one",
            sessions: vec![sample("a", 10), sample("shared", 5)],
            errors: vec!["one: bad".to_string()],
        });
        registry.register(Stub {
            id: "two",
            sessions: vec![sample("shared", 99), sample("b", 20)],
            errors: Vec::new(),
        });
        registry
    }

    #[test]
    fn loading_merges_sources_newest_first_and_dedupes_ids() {
        let env = Env::new(PathBuf::from("/home"), PathBuf::from("/data"));
        let result = registry().load(&env);

        let ids: Vec<&str> = result.sessions.iter().map(|session| session.id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "shared"], "newest first, first source wins a tie");
        assert_eq!(result.errors, ["one: bad"]);
    }

    #[test]
    fn env_keeps_overrides_per_source() {
        let env = Env::new(PathBuf::from("/home"), PathBuf::from("/data"))
            .with_home("dst", PathBuf::from("/custom/dsh"))
            .with_dir("dst", PathBuf::from("/extra/one"))
            .with_dir("dst", PathBuf::from("/extra/two"))
            .with_dir("opencode", PathBuf::from("/oc"));

        assert_eq!(env.home_for("dst"), Some(Path::new("/custom/dsh")));
        assert_eq!(env.home_for("opencode"), None);
        assert_eq!(env.dirs_for("dst").len(), 2);
        assert_eq!(env.dirs_for("opencode"), [PathBuf::from("/oc")]);
        assert!(env.dirs_for("unknown").is_empty());
    }

    #[test]
    fn dispatch_uses_the_session_source_id() {
        let env = Env::new(PathBuf::from("/home"), PathBuf::from("/data"));
        let registry = registry();
        let session = sample("a", 1);
        assert_eq!(registry.change_workspace(&env, &session, "/new").unwrap(), "/new");
        assert!(registry.rename(&env, &session, "t").is_ok());

        let mut orphan = sample("a", 1);
        orphan.source = "nobody";
        assert!(registry.rename(&env, &orphan, "t").is_err());
        assert!(registry.detail(&orphan).is_empty());
    }

    #[test]
    fn names_fall_back_to_the_raw_id() {
        let registry = registry();
        assert_eq!(registry.label("two"), t("source.dst"));
        assert_eq!(registry.label("nobody"), "nobody");
        assert_eq!(registry.badge("nobody"), "NOBODY");
    }
}
