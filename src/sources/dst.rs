//! DST / dsh-tui: one directory per session, a JSONL log inside it.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::cache;
use crate::i18n::{t, tf};
use crate::session::{human_size, DetailRow, Session};
use crate::source::{now_ms, Env, SessionSource, SourceLoad};
use crate::zstd_frames::{compress_frame, decompress_frame, scan_frames};

/// The DST source: session logs written by dsh / dsh-tui.
pub struct Dst;

const ID: &str = "dst";

/// What the DST source needs later to mutate one session.
#[derive(Clone, Debug)]
pub(crate) struct DstData {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub log_path: PathBuf,
    pub log_name: String,
    pub compressed: bool,
    pub frames: usize,
    pub size: u64,
}

impl SessionSource for Dst {
    fn id(&self) -> &'static str {
        ID
    }

    fn label_key(&self) -> &'static str {
        "source.dst"
    }

    fn badge_key(&self) -> &'static str {
        "list.badge.dst"
    }

    fn accent(&self) -> [u8; 3] {
        [0x12, 0x96, 0xdb]
    }

    fn locations(&self, env: &Env) -> Vec<String> {
        let roots = roots(env);
        // Only the ones that exist: a header listing four directories of which
        // two were never created is just noise. When none exist, show all of
        // them so it is obvious where sessions would be picked up from.
        let existing: Vec<PathBuf> = roots.iter().filter(|root| root.is_dir()).cloned().collect();
        let shown = if existing.is_empty() { roots } else { existing };
        shown.iter().map(|root| root.display().to_string()).collect()
    }

    fn load(&self, env: &Env) -> SourceLoad {
        load_sessions(env)
    }

    fn rename(&self, env: &Env, session: &Session, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            bail!(t("err.title_empty"));
        }
        let data = session.data::<DstData>()?;

        let bytes = fs::read(&data.log_path)?;
        let last_seq = last_seq(&bytes, data.compressed)?;
        let next_seq = last_seq.map(|seq| seq + 1).unwrap_or(0);

        // A rename is an appended event, never a rewrite: the reader replays it.
        let event = json!({
            "type": "session/title",
            "seq": next_seq,
            "time": now_ms(),
            "data": {
                "title": title,
                "messageSeqs": [],
                "source": { "kind": "user" }
            }
        });
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        let frame = if data.compressed { compress_frame(&line)? } else { line };

        let mut file = OpenOptions::new().append(true).open(&data.log_path)?;
        file.write_all(&frame)?;
        file.flush()?;
        file.sync_all()?;

        cache::rename_everywhere(&env.data_dir, &dsh_home(env), &session.id, title);
        Ok(())
    }

    fn delete(&self, env: &Env, session: &Session) -> Result<()> {
        let data = session.data::<DstData>()?;
        fs::remove_dir_all(&data.dir)
            .map_err(|error| anyhow!(tf("err.delete_dir", &[("error", &error.to_string())])))?;
        // The workspace directory is a bucket of sessions; it goes when the last
        // one does, and `remove_dir` refuses while anything else is inside.
        if let Some(parent) = data.dir.parent() {
            let _ = fs::remove_dir(parent);
        }

        cache::delete_everywhere(&env.data_dir, &dsh_home(env), &session.id);
        Ok(())
    }

    fn change_workspace(&self, env: &Env, session: &Session, new_cwd: &str) -> Result<String> {
        let data = session.data::<DstData>()?;
        let new_cwd = normalize_cwd(new_cwd);
        if new_cwd.is_empty() {
            bail!(t("err.cwd_empty"));
        }
        if !Path::new(&new_cwd).is_absolute() {
            bail!(t("err.cwd_absolute"));
        }
        if new_cwd == session.cwd {
            return Ok(new_cwd);
        }

        // Build the new log in memory first: everything that can reject a
        // malformed file happens before the session is touched, so afterwards
        // only I/O can fail and the rollback window stays as narrow as it gets.
        let bytes = fs::read(&data.log_path)?;
        let rewritten = rewrite_header_bytes(&bytes, data.compressed, &new_cwd)?;

        let key = project_key(&new_cwd);
        let new_dir = data.root.join(&key).join(&session.id);

        if new_dir == data.dir {
            cache::write_atomic(&data.log_path, &rewritten)?;
        } else {
            if new_dir.exists() {
                bail!(tf("err.target_exists", &[("path", &new_dir.display().to_string())]));
            }
            let parent = new_dir.parent().ok_or_else(|| anyhow!(t("err.invalid_target")))?;
            fs::create_dir_all(parent)?;

            fs::rename(&data.dir, &new_dir)
                .map_err(|error| anyhow!(tf("err.dir_move", &[("error", &error.to_string())])))?;

            let new_log = new_dir.join(&data.log_name);
            if let Err(error) = cache::write_atomic(&new_log, &rewritten) {
                // Move it back, but never hide the original failure: if that
                // also fails the session is stranded at that path with a stale
                // header, and the message has to say where it went.
                return Err(match fs::rename(&new_dir, &data.dir) {
                    Ok(()) => error,
                    Err(rollback) => anyhow!(tf(
                        "err.rollback_failed",
                        &[
                            ("path", &new_dir.display().to_string()),
                            ("error", &format!("{error}; rollback: {rollback}")),
                        ],
                    )),
                });
            }
        }

        cache::move_everywhere(&env.data_dir, &dsh_home(env), &session.id, &session.cwd, &new_cwd);
        Ok(new_cwd)
    }

    fn detail(&self, session: &Session) -> Vec<DetailRow> {
        let Ok(data) = session.data::<DstData>() else {
            return Vec::new();
        };
        let encoding = if data.compressed {
            t("detail.encoding.zstd")
        } else {
            t("detail.encoding.plain")
        };
        vec![
            DetailRow::new(t("detail.size"), human_size(data.size)),
            DetailRow::new(
                t("detail.log"),
                tf(
                    "detail.log_value",
                    &[
                        ("name", &data.log_name),
                        ("encoding", &encoding),
                        ("count", &data.frames.to_string()),
                    ],
                ),
            ),
            DetailRow::new(t("detail.dir"), data.dir.display().to_string()),
        ]
    }

    fn measurement(&self, session: &Session) -> String {
        match session.data::<DstData>() {
            Ok(data) => human_size(data.size),
            Err(_) => String::new(),
        }
    }

    fn json_fields(&self, session: &Session) -> serde_json::Map<String, Value> {
        let mut fields = serde_json::Map::new();
        if let Ok(data) = session.data::<DstData>() {
            fields.insert("root".to_string(), json!(data.root.display().to_string()));
            fields.insert("dir".to_string(), json!(data.dir.display().to_string()));
            fields.insert("log".to_string(), json!(data.log_path.display().to_string()));
            fields.insert("logName".to_string(), json!(data.log_name));
            fields.insert("compressed".to_string(), json!(data.compressed));
            fields.insert("frames".to_string(), json!(data.frames));
            fields.insert("size".to_string(), json!(data.size));
        }
        fields
    }
}

/// `--dsh-home`, `$DSH_HOME`, then `~/.dsh`.
fn dsh_home(env: &Env) -> PathBuf {
    env.home_for(ID)
        .map(Path::to_path_buf)
        .or_else(|| env.env_path("DSH_HOME"))
        .unwrap_or_else(|| env.home.join(".dsh"))
}

/// Every directory that may contain `<workspace>/<session>/` trees.
fn roots(env: &Env) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = env.dirs_for(ID).to_vec();
    if let Some(root) = env.env_path("DSH_TUI_SESSION_ROOT") {
        roots.push(root);
    }
    roots.push(dsh_home(env).join("sessions"));
    roots.push(env.home.join(".dsh-tui").join("sessions"));
    crate::source::dedupe_paths(roots)
}

#[derive(Clone, Debug, Default)]
struct IndexEntry {
    title: String,
    source: String,
    model: Option<String>,
}

/// Titles that had to be scraped out of a log, remembered until that log
/// changes.
///
/// The durable session log stays authoritative: an entry is only reused while
/// both its size and its mtime still match, so any append — including a rename
/// made outside this tool — sends the session back through `derive_title`.
/// Without it every start of the TUI (and every `R`) re-reads whole logs just
/// to learn a title that has not changed.
#[derive(Default)]
struct TitleCache {
    entries: HashMap<String, CachedTitle>,
    dirty: bool,
}

#[derive(Clone)]
struct CachedTitle {
    mtime_ms: u64,
    size: u64,
    title: String,
    source: String,
}

impl TitleCache {
    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("roster-titles.json")
    }

    fn load(data_dir: &Path) -> Self {
        let mut cache = Self::default();
        let Some(value) = crate::cache::read_json(&Self::path(data_dir)) else {
            return cache;
        };
        let Some(entries) = value.get("entries").and_then(|item| item.as_object()) else {
            return cache;
        };
        for (id, entry) in entries {
            let (Some(mtime_ms), Some(size), Some(title)) = (
                entry.get("mtime").and_then(|item| item.as_u64()),
                entry.get("size").and_then(|item| item.as_u64()),
                entry.get("title").and_then(|item| item.as_str()),
            ) else {
                continue;
            };
            // A rewrite is what bumps these numbers; anything else is noise.
            if id.is_empty() || title.is_empty() {
                continue;
            }
            let source = entry.get("source").and_then(|item| item.as_str()).unwrap_or("log");
            cache.entries.insert(
                id.clone(),
                CachedTitle {
                    mtime_ms,
                    size,
                    title: title.to_string(),
                    source: source.to_string(),
                },
            );
        }
        cache
    }

    fn get(&self, id: &str, mtime_ms: u64, size: u64) -> Option<&CachedTitle> {
        self.entries
            .get(id)
            .filter(|entry| entry.mtime_ms == mtime_ms && entry.size == size)
    }

    fn put(&mut self, id: &str, mtime_ms: u64, size: u64, title: &str, source: &str) {
        let already_known = self.entries.get(id).is_some_and(|entry| {
            entry.mtime_ms == mtime_ms && entry.size == size && entry.title == title && entry.source == source
        });
        if already_known {
            return;
        }
        self.entries.insert(
            id.to_string(),
            CachedTitle {
                mtime_ms,
                size,
                title: title.to_string(),
                source: source.to_string(),
            },
        );
        self.dirty = true;
    }

    fn save(&self, data_dir: &Path) {
        if !self.dirty {
            return;
        }
        let entries: serde_json::Map<String, Value> = self
            .entries
            .iter()
            .map(|(id, entry)| {
                let value = serde_json::json!({
                    "mtime": entry.mtime_ms,
                    "size": entry.size,
                    "title": entry.title,
                    "source": entry.source,
                });
                (id.clone(), value)
            })
            .collect();
        let value = serde_json::json!({ "version": 1, "entries": entries });
        // A cache that cannot be written is not an error; the next run just
        // pays for the scan again.
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            let _ = crate::cache::write_atomic(&Self::path(data_dir), &bytes);
        }
    }
}

fn load_sessions(env: &Env) -> SourceLoad {
    let index = load_index(&env.data_dir);
    let mounted = load_mounts(&env.data_dir);
    let mut titles = TitleCache::load(&env.data_dir);

    let mut sessions = Vec::new();
    let mut errors = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for root in roots(env) {
        let workspaces = match fs::read_dir(&root) {
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
                match read_session(&root, &dir, &id, &index, &mounted, &mut titles) {
                    Ok(session) => {
                        seen.insert(id);
                        sessions.push(session);
                    }
                    Err(error) => errors.push(format!("{}: {error}", dir.display())),
                }
            }
        }
    }

    titles.save(&env.data_dir);
    SourceLoad { sessions, errors }
}

fn read_session(
    root: &Path,
    dir: &Path,
    id: &str,
    index: &HashMap<String, IndexEntry>,
    mounted: &HashMap<String, u32>,
    titles: &mut TitleCache,
) -> Result<Session> {
    let (log_path, log_name, _version, compressed) = find_log(dir).ok_or_else(|| anyhow!(t("err.no_log")))?;
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
    } else if let Some(cached) = titles.get(id, mtime_ms, size) {
        (sanitize_title(&cached.title), cached.source.clone(), None)
    } else {
        let (title, source) = match derive_title(&bytes, compressed) {
            Some((title, source)) => (sanitize_title(&title), source),
            None => (fallback_title(&cwd), "fallback".to_string()),
        };
        titles.put(id, mtime_ms, size, &title, &source);
        (title, source, None)
    };

    Ok(Session {
        source: ID,
        id: id.to_string(),
        cwd,
        created_at,
        mtime_ms,
        title,
        title_source,
        parent,
        agent_preset,
        model,
        mounted_pid: mounted.get(id).copied(),
        data: std::sync::Arc::new(DstData {
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
            log_path,
            log_name,
            compressed,
            frames,
            size,
        }),
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

/// The newest `session/title` event, else the first thing the user said.
///
/// Neither half reads the whole log. Titles are appended at the end — a rename
/// *is* the last frame — so the container is walked backwards and stops at the
/// first frame carrying one. A session that was never titled pays one forward
/// pass to reach the opening prompt, which lives in the first event frame. Only
/// a log with neither is still decompressed in full.
fn derive_title(bytes: &[u8], compressed: bool) -> Option<(String, String)> {
    let frames = if compressed {
        scan_frames(bytes).ok()?
    } else {
        Vec::new()
    };

    let last_title = if compressed {
        frames.iter().rev().find_map(|frame| {
            let plain = decompress_frame(&bytes[frame.start..frame.end]).ok()?;
            last_title_in(&plain)
        })
    } else {
        last_title_in(bytes)
    };

    if let Some(title) = last_title {
        return Some((title, "log".to_string()));
    }

    let first_user = if compressed {
        frames.iter().find_map(|frame| {
            let plain = decompress_frame(&bytes[frame.start..frame.end]).ok()?;
            first_user_in(&plain)
        })
    } else {
        first_user_in(bytes)
    };

    first_user.map(|title| (title, "prompt".to_string()))
}

/// The last title event inside one frame's plain text.
fn last_title_in(plain: &[u8]) -> Option<String> {
    let mut found = None;
    for line in plain.split(|byte| *byte == b'\n') {
        if let Some(title) = title_of(line) {
            found = Some(title);
        }
    }
    found
}

fn title_of(line: &[u8]) -> Option<String> {
    if line.len() < 10 || !contains(line, b"session/title") {
        return None;
    }
    let value = serde_json::from_slice::<Value>(line).ok()?;
    if value.get("type").and_then(|item| item.as_str()) != Some("session/title") {
        return None;
    }
    let title = value.get("data")?.get("title")?.as_str()?.trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// The first user message inside one frame's plain text.
fn first_user_in(plain: &[u8]) -> Option<String> {
    for line in plain.split(|byte| *byte == b'\n') {
        if line.len() < 10 || !contains(line, b"user/message") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|item| item.as_str()) != Some("user/message") {
            continue;
        }
        if let Some(text) = first_user_text(&value) {
            return Some(text);
        }
    }
    None
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

/// Rewrites only the header frame, leaving every event frame byte-for-byte as
/// it was. The header must stay the first frame and exactly one line.
fn rewrite_header_bytes(bytes: &[u8], compressed: bool, new_cwd: &str) -> Result<Vec<u8>> {
    if compressed {
        let frames = scan_frames(bytes)?;
        let first = frames.first().ok_or_else(|| anyhow!(t("err.empty_log")))?;
        let plain = decompress_frame(&bytes[first.start..first.end])?;
        let text = std::str::from_utf8(&plain)?;
        let line = text.trim_end_matches('\n');
        if line.is_empty() {
            bail!(t("err.empty_header"));
        }
        if line.contains('\n') {
            bail!(t("err.header_frame"));
        }
        let mut header: Value = serde_json::from_str(line)?;
        header["cwd"] = Value::String(new_cwd.to_string());
        let mut encoded = serde_json::to_vec(&header)?;
        encoded.push(b'\n');
        let frame = compress_frame(&encoded)?;
        let mut out = frame;
        out.extend_from_slice(&bytes[first.end..]);
        Ok(out)
    } else {
        let end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(bytes.len());
        let text = std::str::from_utf8(&bytes[..end])?;
        let line = text.trim_end_matches('\n');
        let mut header: Value = serde_json::from_str(line)?;
        header["cwd"] = Value::String(new_cwd.to_string());
        let mut out = serde_json::to_vec(&header)?;
        out.push(b'\n');
        out.extend_from_slice(&bytes[end..]);
        Ok(out)
    }
}

fn last_seq(bytes: &[u8], compressed: bool) -> Result<Option<u64>> {
    let mut maximum: Option<u64> = None;

    if compressed {
        let frames = scan_frames(bytes)?;
        for frame in frames {
            let plain = decompress_frame(&bytes[frame.start..frame.end])?;
            scan_seq_lines(&plain, &mut maximum);
        }
    } else {
        scan_seq_lines(bytes, &mut maximum);
    }

    Ok(maximum)
}

fn scan_seq_lines(plain: &[u8], maximum: &mut Option<u64>) {
    for line in plain.split(|byte| *byte == b'\n') {
        if !line.windows(5).any(|window| window == b"\"seq\"") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(seq) = value.get("seq").and_then(|item| item.as_u64()) {
            let current = maximum.unwrap_or(seq);
            *maximum = Some(current.max(seq));
        }
    }
}

/// Trim what a paste tends to drag along and settle on one separator, so that
/// `C:/code/Siren/` and `C:\code\Siren` compare equal.
fn normalize_cwd(input: &str) -> String {
    let mut value = input.trim().trim_matches('"').to_string();
    if cfg!(windows) {
        value = value.replace('/', "\\");
        while value.len() > 3 && value.ends_with('\\') {
            value.pop();
        }
    } else {
        while value.len() > 1 && value.ends_with('/') {
            value.pop();
        }
    }
    value
}

/// Reproduce the DSH project directory key: separators collapse to `-`, other
/// unsafe code units become `~XXXX`, wrapped in `--...--`.
fn project_key(cwd: &str) -> String {
    if cwd.is_empty() {
        return "--root--".to_string();
    }
    let dash = b'-' as u16;
    let tilde = b'~' as u16;
    let mut units: Vec<u16> = Vec::new();
    let mut separator_run = false;
    for code in cwd.encode_utf16() {
        let character = char::from_u32(code as u32);
        match character {
            Some('/') | Some('\\') | Some(':') => {
                if !separator_run {
                    units.push(dash);
                }
                separator_run = true;
            }
            Some(ch) if ch != '~' && (ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-') => {
                units.push(code);
                separator_run = false;
            }
            _ => {
                units.push(tilde);
                for byte in format!("{code:04X}").bytes() {
                    units.push(byte as u16);
                }
                separator_run = false;
            }
        }
    }
    while units.first() == Some(&dash) {
        units.remove(0);
    }
    if units.is_empty() {
        units.extend("root".bytes().map(|byte| byte as u16));
    }
    units.truncate(251);
    format!("--{}--", String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zstd_frames::compress_frame;

    #[test]
    fn recognizes_generation_log_names() {
        assert_eq!(parse_log_name("session.v4.jsonl.zstd"), Some((4, true)));
        assert_eq!(parse_log_name("session.jsonl.zstd"), Some((0, true)));
        assert_eq!(parse_log_name("session.v3.jsonl"), Some((3, false)));
        assert_eq!(parse_log_name("session.v0.jsonl.zstd"), None);
        assert_eq!(parse_log_name("session.migration.1.jsonl.zstd"), None);
    }

    /// Frame order still decides: scanning backwards must not stop at an older
    /// title that sits in a later-numbered but still-earlier frame.
    #[test]
    fn the_newest_title_wins_across_frames() {
        let header = br#"{"type":"session","cwd":"C:/x"}"#;
        let early = br#"{"type":"session/title","seq":1,"data":{"title":"old"}}"#;
        let late = br#"{"type":"session/title","seq":9,"data":{"title":"new"}}"#;

        let container = container(&[header, early, late]);
        assert_eq!(
            derive_title(&container, true),
            Some(("new".to_string(), "log".to_string()))
        );
    }

    #[test]
    fn falls_back_to_the_first_prompt_when_no_title_was_ever_written() {
        let header = br#"{"type":"session","cwd":"C:/x"}"#;
        let prompt = br#"{"type":"user/message","seq":0,"data":{"content":"hello there"}}"#;
        let later = br#"{"type":"user/message","seq":5,"data":{"content":"not this one"}}"#;

        let container = container(&[header, prompt, later]);
        assert_eq!(
            derive_title(&container, true),
            Some(("hello there".to_string(), "prompt".to_string()))
        );
    }

    #[test]
    fn plain_logs_use_the_same_rules() {
        let header = br#"{"type":"session","cwd":"C:/x"}"#;
        let title = br#"{"type":"session/title","seq":2,"data":{"title":" plain "}}"#;

        let mut log = Vec::new();
        for line in [&header[..], &title[..]] {
            log.extend_from_slice(line);
            log.push(b'\n');
        }
        assert_eq!(
            derive_title(&log, false),
            Some(("plain".to_string(), "log".to_string()))
        );
    }

    #[test]
    fn the_title_cache_only_trusts_a_log_it_has_seen() {
        let dir = std::env::temp_dir().join(format!("roster-titles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut cache = TitleCache::load(&dir);
        assert!(cache.get("s1", 100, 500).is_none());
        cache.put("s1", 100, 500, "cached", "log");
        assert!(cache.dirty);
        cache.save(&dir);

        let cache = TitleCache::load(&dir);
        assert_eq!(
            cache.get("s1", 100, 500).map(|entry| entry.title.as_str()),
            Some("cached")
        );
        assert!(cache.get("s1", 101, 500).is_none(), "an appended log is never trusted");
        assert!(cache.get("s1", 100, 501).is_none(), "a longer log is never trusted");
        assert!(cache.get("s2", 100, 500).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_cache_is_not_rewritten() {
        let dir = std::env::temp_dir().join(format!("roster-titles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut cache = TitleCache::load(&dir);
        cache.put("s1", 1, 2, "t", "log");
        cache.save(&dir);

        let mut again = TitleCache::load(&dir);
        again.put("s1", 1, 2, "t", "log");
        assert!(!again.dirty, "identical data must not rewrite the file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_load_reads_the_title_from_the_cache() {
        let base = std::env::temp_dir().join(format!("roster-store-{}", uuid::Uuid::new_v4()));
        let root = base.join("sessions");
        let dir = root.join("--E-code-x--").join("sess-1");
        std::fs::create_dir_all(&dir).unwrap();

        let mut log = Vec::new();
        for line in [
            &br#"{"type":"session","version":4,"id":"sess-1","createdAt":1,"cwd":"E:/code/x"}"#[..],
            &br#"{"type":"session/title","seq":1,"data":{"title":"scraped"}}"#[..],
        ] {
            log.extend_from_slice(line);
            log.push(b'\n');
        }
        std::fs::write(dir.join("session.v4.jsonl"), &log).unwrap();

        let env = Env::isolated(base.join("home"), base.join("data")).with_dir(ID, root);

        let first = Dst.load(&env);
        assert_eq!(first.sessions[0].title, "scraped");
        assert_eq!(first.sessions[0].title_source, "log");
        assert!(TitleCache::path(&env.data_dir).exists(), "the scrape is remembered");

        let second = Dst.load(&env);
        assert_eq!(second.sessions[0].title, "scraped");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn project_key_matches_dsh_encoding() {
        assert_eq!(project_key(r"E:\home\alice\code\Siren"), "--E-home-alice-code-Siren--");
        assert_eq!(project_key(r"E:\code\Rhine Music"), "--E-code-Rhine~0020Music--");
    }

    #[test]
    fn rewrites_only_the_header_frame() {
        let header = br#"{"type":"session","version":4,"id":"x","createdAt":1,"cwd":"C:/old","isSeeded":false,"delegationDepth":0}"#;
        let mut header_frame = header.to_vec();
        header_frame.push(b'\n');
        let body = b"{\"type\":\"user/message\",\"seq\":0}\n";
        let mut container = compress_frame(&header_frame).unwrap();
        container.extend_from_slice(&compress_frame(body).unwrap());

        let rewritten = rewrite_header_bytes(&container, true, "E:/new").unwrap();
        let frames = scan_frames(&rewritten).unwrap();
        assert_eq!(frames.len(), 2);

        let plain = decompress_frame(&rewritten[frames[0].start..frames[0].end]).unwrap();
        let value: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(value["cwd"], "E:/new");
        assert_eq!(
            &rewritten[frames[0].end..],
            &container[scan_frames(&container).unwrap()[0].end..]
        );
    }

    /// A real tree on disk, so the directory move and the header rewrite are
    /// checked together instead of against a hand-built `Session`.
    #[test]
    fn moves_the_workspace_directory_and_rewrites_the_header() {
        let base = std::env::temp_dir().join(format!("roster-mutate-{}", uuid::Uuid::new_v4()));
        let root = base.join("sessions");
        let (old_cwd, new_cwd) = workspace_pair();
        let log_name = "session.v4.jsonl.zstd";

        let from = root.join(project_key(&old_cwd)).join("sess-1");
        fs::create_dir_all(&from).unwrap();
        let mut header = serde_json::to_vec(&json!({
            "type": "session",
            "version": 4,
            "id": "sess-1",
            "createdAt": 1,
            "cwd": old_cwd,
        }))
        .unwrap();
        header.push(b'\n');
        let mut body = br#"{"type":"assistant/message","seq":1}"#.to_vec();
        body.push(b'\n');
        let mut container = compress_frame(&header).unwrap();
        container.extend_from_slice(&compress_frame(&body).unwrap());
        fs::write(from.join(log_name), &container).unwrap();

        let env = Env::isolated(base.join("home"), base.join("data")).with_dir(ID, root.clone());
        let loaded = Dst.load(&env);
        assert_eq!(loaded.sessions.len(), 1);
        let session = loaded.sessions[0].clone();

        // A pasted path may carry a trailing separator; the move must not care.
        let separator = if cfg!(windows) { '\\' } else { '/' };
        let moved = Dst
            .change_workspace(&env, &session, &format!("{new_cwd}{separator}"))
            .unwrap();
        assert_eq!(moved, new_cwd);

        assert!(!from.exists(), "the old directory is gone");
        let to = root.join(project_key(&moved)).join("sess-1");
        let bytes = fs::read(to.join(log_name)).unwrap();
        let frames = scan_frames(&bytes).unwrap();
        assert_eq!(frames.len(), 2, "the body frame travels along");

        let plain = decompress_frame(&bytes[frames[0].start..frames[0].end]).unwrap();
        let header: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(header["cwd"], json!(moved));

        let _ = fs::remove_dir_all(&base);
    }

    /// Deleting is final: the directory really goes, and so does the workspace
    /// bucket once nothing else is left in it.
    #[test]
    fn deleting_a_session_removes_the_directory_and_the_empty_workspace() {
        let base = std::env::temp_dir().join(format!("roster-mutate-{}", uuid::Uuid::new_v4()));
        let root = base.join("sessions");
        let cwd = if cfg!(windows) { r"E:\code\x" } else { "/code/x" };
        let workspace = root.join(project_key(cwd));

        for id in ["sess-1", "sess-2"] {
            let dir = workspace.join(id);
            fs::create_dir_all(&dir).unwrap();
            let mut log = serde_json::to_vec(&json!({
                "type": "session",
                "version": 4,
                "id": id,
                "createdAt": 1,
                "cwd": cwd,
            }))
            .unwrap();
            log.push(b'\n');
            log.extend_from_slice(br#"{"type":"assistant/message","seq":1}"#);
            log.push(b'\n');
            fs::write(dir.join("session.v4.jsonl"), &log).unwrap();
        }

        let env = Env::isolated(base.join("home"), base.join("data")).with_dir(ID, root.clone());

        let loaded = Dst.load(&env);
        assert_eq!(loaded.sessions.len(), 2);
        let first = loaded.sessions.iter().find(|s| s.id == "sess-1").unwrap().clone();
        Dst.delete(&env, &first).unwrap();

        assert!(!workspace.join("sess-1").exists(), "the session directory is gone");
        assert!(workspace.exists(), "a workspace with a sibling stays");
        assert!(!env.data_dir.join("roster-trash").exists(), "nothing is moved aside");

        let loaded = Dst.load(&env);
        let second = loaded.sessions[0].clone();
        Dst.delete(&env, &second).unwrap();
        assert!(!workspace.exists(), "the empty workspace goes with its last session");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn detail_rows_come_from_the_log_not_the_core() {
        let base = std::env::temp_dir().join(format!("roster-detail-{}", uuid::Uuid::new_v4()));
        let root = base.join("sessions");
        let dir = root.join("--E-code-x--").join("sess-1");
        fs::create_dir_all(&dir).unwrap();
        let mut log = br#"{"type":"session","version":4,"id":"sess-1","createdAt":1,"cwd":"E:/code/x"}"#.to_vec();
        log.push(b'\n');
        fs::write(dir.join("session.v4.jsonl"), &log).unwrap();

        let env = Env::isolated(base.join("home"), base.join("data")).with_dir(ID, root);
        let session = Dst.load(&env).sessions.remove(0);

        let rows = Dst.detail(&session);
        assert_eq!(rows.len(), 3, "size, log, dir");
        assert!(!Dst.measurement(&session).is_empty());
        assert_eq!(Dst.json_fields(&session)["logName"], json!("session.v4.jsonl"));

        let _ = fs::remove_dir_all(&base);
    }

    fn workspace_pair() -> (String, String) {
        if cfg!(windows) {
            (r"E:\code\Old Place".to_string(), r"E:\code\New".to_string())
        } else {
            ("/code/old place".to_string(), "/code/new".to_string())
        }
    }

    /// One zstd frame per line, the way DSH writes a log.
    fn container(lines: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            let mut encoded = line.to_vec();
            encoded.push(b'\n');
            out.extend_from_slice(&compress_frame(&encoded).unwrap());
        }
        assert_eq!(scan_frames(&out).unwrap().len(), lines.len());
        out
    }
}
