//! Session mutations, dispatched on how the session is stored: DST sessions are
//! files on disk, opencode sessions are rows in a shared SQLite database.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::cache;
use crate::i18n::{t, tf};
use crate::paths::Paths;
use crate::store::{Location, Session};
use crate::zstd_frames::{compress_frame, decompress_frame, scan_frames};

pub fn rename(paths: &Paths, session: &Session, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!(t("err.title_empty"));
    }
    let Location::Dst {
        log_path, compressed, ..
    } = &session.location
    else {
        return crate::opencode::rename(session, title);
    };

    let bytes = fs::read(log_path)?;
    let last_seq = last_seq(&bytes, *compressed)?;
    let next_seq = last_seq.map(|seq| seq + 1).unwrap_or(0);

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
    let frame = if *compressed { compress_frame(&line)? } else { line };

    let mut file = OpenOptions::new().append(true).open(log_path)?;
    file.write_all(&frame)?;
    file.flush()?;
    file.sync_all()?;

    cache::rename_everywhere(paths, &session.id, title);
    Ok(())
}

/// Deletes a session for good. The confirmation dialog is the only safety net;
/// nothing is moved aside any more.
pub fn delete(paths: &Paths, session: &Session) -> Result<()> {
    let Location::Dst { dir, .. } = &session.location else {
        return crate::opencode::delete(session);
    };

    fs::remove_dir_all(dir).map_err(|error| anyhow!(tf("err.delete_dir", &[("error", &error.to_string())])))?;
    // The workspace directory is a bucket of sessions; it goes when the last
    // one does, and `remove_dir` refuses while anything else is inside.
    if let Some(parent) = dir.parent() {
        let _ = fs::remove_dir(parent);
    }

    cache::delete_everywhere(paths, &session.id);
    Ok(())
}

pub fn change_workspace(paths: &Paths, session: &Session, new_cwd: &str) -> Result<String> {
    let Location::Dst {
        root,
        dir,
        log_path,
        log_name,
        compressed,
        ..
    } = &session.location
    else {
        return crate::opencode::change_workspace(session, new_cwd);
    };

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

    // Build the new log in memory first: everything that can reject a malformed
    // file happens before the session is touched, so afterwards only I/O can
    // fail and the rollback window stays as narrow as it gets.
    let bytes = fs::read(log_path)?;
    let rewritten = rewrite_header_bytes(&bytes, *compressed, &new_cwd)?;

    let key = project_key(&new_cwd);
    let new_dir = root.join(&key).join(&session.id);

    if new_dir == *dir {
        cache::write_atomic(log_path, &rewritten)?;
    } else {
        if new_dir.exists() {
            bail!(tf("err.target_exists", &[("path", &new_dir.display().to_string())]));
        }
        let parent = new_dir.parent().ok_or_else(|| anyhow!(t("err.invalid_target")))?;
        fs::create_dir_all(parent)?;

        fs::rename(dir, &new_dir).map_err(|error| anyhow!(tf("err.dir_move", &[("error", &error.to_string())])))?;

        let new_log = new_dir.join(log_name);
        if let Err(error) = cache::write_atomic(&new_log, &rewritten) {
            // Move it back, but never hide the original failure: if that also
            // fails the session is stranded at `new_dir` with a stale header,
            // and the message has to say where it went.
            return Err(match fs::rename(&new_dir, dir) {
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

    cache::move_everywhere(paths, &session.id, &session.cwd, &new_cwd);
    Ok(new_cwd)
}

pub fn rewrite_header_bytes(bytes: &[u8], compressed: bool, new_cwd: &str) -> Result<Vec<u8>> {
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

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Trim what a paste tends to drag along and settle on one separator, so that
/// `C:/code/Siren/` and `C:\code\Siren` compare equal.
pub fn normalize_cwd(input: &str) -> String {
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
pub fn project_key(cwd: &str) -> String {
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
    use crate::store;

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

        let paths = Paths {
            dsh_home: base.join("dsh"),
            data_dir: base.join("data"),
            session_roots: vec![root.clone()],
            opencode_dirs: Vec::new(),
        };
        let loaded = store::load(&paths);
        assert_eq!(loaded.sessions.len(), 1);
        let session = loaded.sessions[0].clone();

        // A pasted path may carry a trailing separator; the move must not care.
        let separator = if cfg!(windows) { '\\' } else { '/' };
        let moved = change_workspace(&paths, &session, &format!("{new_cwd}{separator}")).unwrap();
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

    /// Deleting is final now: the directory really goes, and so does the
    /// workspace bucket once nothing else is left in it.
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

        let paths = Paths {
            dsh_home: base.join("dsh"),
            data_dir: base.join("data"),
            session_roots: vec![root.clone()],
            opencode_dirs: Vec::new(),
        };

        let loaded = store::load(&paths);
        assert_eq!(loaded.sessions.len(), 2);
        let first = loaded.sessions.iter().find(|s| s.id == "sess-1").unwrap().clone();
        delete(&paths, &first).unwrap();

        assert!(!workspace.join("sess-1").exists(), "the session directory is gone");
        assert!(workspace.exists(), "a workspace with a sibling stays");
        assert!(!paths.data_dir.join("roster-trash").exists(), "nothing is moved aside");

        let loaded = store::load(&paths);
        let second = loaded.sessions[0].clone();
        delete(&paths, &second).unwrap();
        assert!(!workspace.exists(), "the empty workspace goes with its last session");

        let _ = fs::remove_dir_all(&base);
    }

    fn workspace_pair() -> (String, String) {
        if cfg!(windows) {
            (r"E:\code\Old Place".to_string(), r"E:\code\New".to_string())
        } else {
            ("/code/old place".to_string(), "/code/new".to_string())
        }
    }
}
