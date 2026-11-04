use std::env;
use std::path::PathBuf;

/// Resolved locations used by dsh and dsh-tui.
#[derive(Clone, Debug)]
pub struct Paths {
    pub dsh_home: PathBuf,
    pub data_dir: PathBuf,
    pub session_roots: Vec<PathBuf>,
    pub trash_dir: PathBuf,
}

impl Paths {
    pub fn discover(dsh_home: Option<PathBuf>, data_dir: Option<PathBuf>, sessions_roots: Vec<PathBuf>) -> Self {
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

        let trash_dir = data_dir.join("dst-session-manager-trash");

        Self {
            dsh_home,
            data_dir,
            session_roots: roots,
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
