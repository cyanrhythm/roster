use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::i18n::t;
use crate::paths::Paths;
use crate::zstd_frames::{decompress_frame, scan_frames};

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub root: PathBuf,
    pub ws: String,
    pub dir: PathBuf,
    pub log_path: PathBuf,
    pub log_name: String,
    pub compressed: bool,
    pub cwd: String,
    pub created_at: u64,
    pub mtime_ms: u64,
    pub size: u64,
    pub frames: usize,
    pub title: String,
    pub title_source: String,
    pub parent: Option<String>,
    pub agent_preset: Option<String>,
    pub model: Option<String>,
}

impl Session {
    pub fn short_id(&self) -> String {
        if self.id.chars().count() > 14 {
            let mut out: String = self.id.chars().take(12).collect();
            out.push('…');
            out
        } else {
            self.id.clone()
        }
    }

    pub fn display_title(&self) -> String {
        if self.title.trim().is_empty() {
            crate::i18n::t("common.untitled")
        } else {
            self.title.clone()
        }
    }
}

#[derive(Clone, Debug, Default)]
struct IndexEntry {
    title: String,
    source: String,
    model: Option<String>,
}

#[derive(Default)]
pub struct LoadResult {
    pub sessions: Vec<Session>,
    pub errors: Vec<String>,
    pub mounted: HashMap<String, u32>,
}

pub fn load(paths: &Paths) -> LoadResult {
    let index = load_index(&paths.data_dir);
    let mounted = load_mounts(&paths.data_dir);

    let mut sessions = Vec::new();
    let mut errors = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for root in &paths.session_roots {
        let workspaces = match fs::read_dir(root) {
            Ok(iter) => iter,
            Err(error) => {
                if root.exists() {
                    errors.push(format!("{}: {error}", root.display()));
                }
                continue;
            }
        };

        for workspace in workspaces.flatten() {
            let workspace_path = workspace.path();
            if !workspace_path.is_dir() {
                continue;
            }
            let ws_name = workspace.file_name().to_string_lossy().to_string();
            let sessions_dir = match fs::read_dir(&workspace_path) {
                Ok(iter) => iter,
                Err(error) => {
                    errors.push(format!("{}: {error}", workspace_path.display()));
                    continue;
                }
            };
            for entry in sessions_dir.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let id = entry.file_name().to_string_lossy().to_string();
                if seen.contains(&id) {
                    continue;
                }
                match read_session(root, &ws_name, &dir, &id, &index) {
                    Ok(session) => {
                        seen.insert(id);
                        sessions.push(session);
                    }
                    Err(error) => errors.push(format!("{}: {error}", dir.display())),
                }
            }
        }
    }

    sessions.sort_by_key(|session| std::cmp::Reverse(session.mtime_ms));

    LoadResult {
        sessions,
        errors,
        mounted,
    }
}

fn read_session(root: &Path, ws: &str, dir: &Path, id: &str, index: &HashMap<String, IndexEntry>) -> Result<Session> {
    let (log_path, log_name, _version, compressed) = find_log(dir).ok_or_else(|| anyhow!("no session log found"))?;
    let bytes = fs::read(&log_path)?;
    let metadata = fs::metadata(&log_path)?;
    let size = metadata.len();
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);

    let (header_value, frames) = if compressed {
        let frames = scan_frames(&bytes)?;
        let first = frames.first().ok_or_else(|| anyhow!(t("err.empty_log")))?;
        let plain = decompress_frame(&bytes[first.start..first.end])?;
        let line = first_line(&plain).ok_or_else(|| anyhow!(t("err.empty_header")))?;
        (serde_json::from_slice::<Value>(line)?, frames.len())
    } else {
        let line = first_line(&bytes).ok_or_else(|| anyhow!(t("err.empty_log")))?;
        (serde_json::from_slice::<Value>(line)?, 0)
    };

    let cwd = header_value
        .get("cwd")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!(t("err.no_cwd")))?
        .to_string();
    let created_at = header_value
        .get("createdAt")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let parent = header_value
        .get("parentSession")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let agent_preset = header_value
        .get("agentPreset")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let (title, title_source, model) = if let Some(entry) = index.get(id) {
        (sanitize_title(&entry.title), entry.source.clone(), entry.model.clone())
    } else if let Some((title, source)) = derive_title(&bytes, compressed) {
        (sanitize_title(&title), source, None)
    } else {
        (fallback_title(&cwd), "fallback".to_string(), None)
    };

    Ok(Session {
        id: id.to_string(),
        root: root.to_path_buf(),
        ws: ws.to_string(),
        dir: dir.to_path_buf(),
        log_path,
        log_name,
        compressed,
        cwd,
        created_at,
        mtime_ms,
        size,
        frames,
        title,
        title_source,
        parent,
        agent_preset,
        model,
    })
}

fn sanitize_title(title: &str) -> String {
    title.replace(['\n', '\r'], " ").trim().to_string()
}

fn first_line(buf: &[u8]) -> Option<&[u8]> {
    let end = buf.iter().position(|byte| *byte == b'\n').unwrap_or(buf.len());
    let line = &buf[..end];
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

fn find_log(dir: &Path) -> Option<(PathBuf, String, u32, bool)> {
    let entries = fs::read_dir(dir).ok()?;
    let mut best: Option<(u32, bool, PathBuf, String)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((version, compressed)) = parse_log_name(&name) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((best_version, best_compressed, _, _)) => {
                version > *best_version || (version == *best_version && compressed && !*best_compressed)
            }
        };
        if better {
            best = Some((version, compressed, entry.path(), name));
        }
    }
    best.map(|(version, compressed, path, name)| (path, name, version, compressed))
}

fn parse_log_name(name: &str) -> Option<(u32, bool)> {
    let compressed = name.ends_with(".zstd");
    let base = if compressed {
        &name[..name.len() - ".zstd".len()]
    } else {
        name
    };
    let core = base.strip_suffix(".jsonl")?;
    if core == "session" {
        return Some((0, compressed));
    }
    let version = core.strip_prefix("session.v")?;
    if version.is_empty() || version.starts_with('0') || !version.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((version.parse().ok()?, compressed))
}

fn derive_title(bytes: &[u8], compressed: bool) -> Option<(String, String)> {
    let mut last_title: Option<String> = None;
    let mut first_user: Option<String> = None;

    if compressed {
        let frames = scan_frames(bytes).ok()?;
        for frame in &frames {
            let Ok(plain) = decompress_frame(&bytes[frame.start..frame.end]) else {
                continue;
            };
            scan_title_lines(&plain, &mut last_title, &mut first_user);
        }
    } else {
        scan_title_lines(bytes, &mut last_title, &mut first_user);
    }

    if let Some(title) = last_title {
        Some((title, "log".to_string()))
    } else {
        first_user.map(|title| (title, "prompt".to_string()))
    }
}

fn scan_title_lines(plain: &[u8], last_title: &mut Option<String>, first_user: &mut Option<String>) {
    for line in plain.split(|byte| *byte == b'\n') {
        if line.len() < 10 {
            continue;
        }
        if !contains(line, b"session/title") && !contains(line, b"user/message") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|item| item.as_str()) {
            Some("session/title") => {
                if let Some(title) = value
                    .get("data")
                    .and_then(|data| data.get("title"))
                    .and_then(|item| item.as_str())
                {
                    let title = title.trim();
                    if !title.is_empty() {
                        *last_title = Some(title.to_string());
                    }
                }
            }
            Some("user/message") if first_user.is_none() => {
                if let Some(text) = first_user_text(&value) {
                    *first_user = Some(text);
                }
            }
            _ => {}
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn first_user_text(value: &Value) -> Option<String> {
    let content = value.get("data")?.get("content")?;
    if let Some(text) = content.as_str() {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if block.get("type").and_then(|item| item.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|item| item.as_str()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }
    None
}

fn fallback_title(cwd: &str) -> String {
    cwd.rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

fn load_index(data_dir: &Path) -> HashMap<String, IndexEntry> {
    let mut map = HashMap::new();
    let path = data_dir.join("session-index.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return map;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return map;
    };
    let Some(entries) = value.get("entries").and_then(|item| item.as_object()) else {
        return map;
    };
    for (id, entry) in entries {
        let Some(derived) = entry.get("derived") else {
            continue;
        };
        let title = derived
            .get("title")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            continue;
        }
        let source = derived
            .get("titleSource")
            .and_then(|item| item.as_str())
            .unwrap_or("fallback")
            .to_string();
        let model = derived.get("model").and_then(|item| item.as_str()).map(str::to_string);
        map.insert(id.clone(), IndexEntry { title, source, model });
    }
    map
}

fn load_mounts(data_dir: &Path) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    let path = data_dir.join("session-mounts.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return map;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return map;
    };
    let Some(owners) = value.get("owners").and_then(|item| item.as_array()) else {
        return map;
    };
    for owner in owners {
        let pid = owner.get("pid").and_then(|item| item.as_u64()).unwrap_or(0) as u32;
        if let Some(ids) = owner.get("sessionIds").and_then(|item| item.as_array()) {
            for id in ids {
                if let Some(id) = id.as_str() {
                    map.insert(id.to_string(), pid);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_generation_log_names() {
        assert_eq!(parse_log_name("session.v4.jsonl.zstd"), Some((4, true)));
        assert_eq!(parse_log_name("session.jsonl.zstd"), Some((0, true)));
        assert_eq!(parse_log_name("session.v3.jsonl"), Some((3, false)));
        assert_eq!(parse_log_name("session.v0.jsonl.zstd"), None);
        assert_eq!(parse_log_name("session.migration.1.jsonl.zstd"), None);
    }
}
