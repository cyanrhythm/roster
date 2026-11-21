use std::env;
use std::path::PathBuf;

/// Resolved locations used by DST / dsh-tui and by opencode.
#[derive(Clone, Debug)]
pub struct Paths {
    /// DSH home: session logs live in `sessions/`, dsh-tui state in `storages/`.
    pub dsh_home: PathBuf,
    /// dsh-tui data directory: our own preferences, caches and trash live here.
    pub data_dir: PathBuf,
    /// Every directory scanned for DST session logs.
    pub session_roots: Vec<PathBuf>,
    /// Every opencode data directory that may hold an `opencode.db`.
    pub opencode_dirs: Vec<PathBuf>,
    pub trash_dir: PathBuf,
}

impl Paths {
    pub fn discover(
        dsh_home: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        sessions_roots: Vec<PathBuf>,
        opencode_dirs: Vec<PathBuf>,
    ) -> Self {
        let home = home_dir();
        let dsh_home = dsh_home
            .or_else(|| env_path("DSH_HOME"))
            .unwrap_or_else(|| home.join(".dsh"));
        let data_dir = data_dir.unwrap_or_else(|| home.join(".dsh-tui"));

        let mut roots = sessions_roots;
        if let Some(root) = env_path("DSH_TUI_SESSION_ROOT") {
            roots.push(root);
        }
        roots.push(dsh_home.join("sessions"));
        roots.push(home.join(".dsh-tui").join("sessions"));

        let mut seen: Vec<String> = Vec::new();
        roots.retain(|root| {
            let key = root.display().to_string().to_lowercase();
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });

        // opencode keeps its state in an XDG data directory, on Windows too.
        let mut data_dirs = opencode_dirs;
        if let Some(dir) = env_path("OPENCODE_DATA_DIR") {
            data_dirs.push(dir);
        }
        if let Some(base) = env_path("XDG_DATA_HOME") {
            data_dirs.push(base.join("opencode"));
        }
        data_dirs.push(home.join(".local").join("share").join("opencode"));
        if cfg!(windows) {
            if let Some(local) = env_path("LOCALAPPDATA") {
                data_dirs.push(local.join("opencode"));
            }
        }

        let mut seen_dirs: Vec<String> = Vec::new();
        data_dirs.retain(|dir| {
            let key = dir.display().to_string().to_lowercase();
            if seen_dirs.contains(&key) {
                false
            } else {
                seen_dirs.push(key);
                true
            }
        });

        let trash_dir = data_dir.join("roster-trash");

        Self {
            dsh_home,
            data_dir,
            session_roots: roots,
            opencode_dirs: data_dirs,
            trash_dir,
        }
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key).filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn home_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(profile) = env_path("USERPROFILE") {
            return profile;
        }
    }
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from("."))
}
