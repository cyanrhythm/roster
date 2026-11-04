//! Session mutations: rename (append a title frame), delete (move to trash),
//! and workspace change (rewrite header cwd + move the session directory).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::cache;
use crate::i18n::{t, tf};
use crate::paths::Paths;
use crate::store::Session;
use crate::zstd_frames::{compress_frame, decompress_frame, scan_frames};

pub fn rename(paths: &Paths, session: &Session, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!(t("err.title_empty"));
    }

    let bytes = fs::read(&session.log_path)?;
    let last_seq = last_seq(&bytes, session.compressed)?;
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
    let frame = if session.compressed {
        compress_frame(&line)?
    } else {
        line
    };

    let mut file = OpenOptions::new().append(true).open(&session.log_path)?;
    file.write_all(&frame)?;
    file.flush()?;
    file.sync_all()?;

    cache::rename_everywhere(paths, &session.id, title);
    Ok(())
}

pub fn delete(paths: &Paths, session: &Session) -> Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let target = paths.trash_dir.join(stamp).join(&session.ws).join(&session.id);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&session.dir, &target)
        .map_err(|error| anyhow!(tf("err.trash_move", &[("error", &error.to_string())])))?;

    cache::delete_everywhere(paths, &session.id);
    Ok(target)
}

pub fn change_workspace(paths: &Paths, session: &Session, new_cwd: &str) -> Result<String> {
    let new_cwd = normalize_cwd(new_cwd);
    if new_cwd.is_empty() {
        bail!(t("err.cwd_empty"));
    }
    if !is_absolute(&new_cwd) {
        bail!(t("err.cwd_absolute"));
    }
    if new_cwd == session.cwd {
        return Ok(new_cwd);
    }

    let key = project_key(&new_cwd);
    let new_dir = session.root.join(&key).join(&session.id);

    if new_dir == session.dir {
        let bytes = fs::read(&session.log_path)?;
        let rewritten = rewrite_header_bytes(&bytes, session.compressed, &new_cwd)?;
        cache::write_atomic(&session.log_path, &rewritten)?;
    } else {
        if new_dir.exists() {
            bail!(tf("err.target_exists", &[("path", &new_dir.display().to_string())]));
        }
        let parent = new_dir.parent().ok_or_else(|| anyhow!("invalid target directory"))?;
        fs::create_dir_all(parent)?;

        fs::rename(&session.dir, &new_dir).map_err(|error| anyhow!("cannot move session directory: {error}"))?;

        let new_log = new_dir.join(&session.log_name);
        let rewrite = (|| -> Result<()> {
            let bytes = fs::read(&new_log)?;
            let rewritten = rewrite_header_bytes(&bytes, session.compressed, &new_cwd)?;
            cache::write_atomic(&new_log, &rewritten)
        })();

        if let Err(error) = rewrite {
            let _ = fs::rename(&new_dir, &session.dir);
            return Err(error);
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

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

fn is_absolute(path: &str) -> bool {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
            || path.starts_with("\\\\")
    } else {
        path.starts_with('/')
    }
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
}
