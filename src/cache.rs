//! Best-effort updates to dsh-tui caches and preferences.
//!
//! The durable session log is authoritative. These files are derived caches
//! and local preferences; a failure to update one must never abort a mutation.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

/// The two dsh-tui directories these helpers read and write.
#[derive(Clone, Copy)]
struct DshPaths<'a> {
    data_dir: &'a Path,
    dsh_home: &'a Path,
}

pub fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write_atomic(path, &bytes)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file");
    let temporary = path.with_file_name(format!("{file_name}.roster.tmp"));
    fs::write(&temporary, bytes)?;
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = fs::remove_file(path);
            fs::rename(&temporary, path).map_err(Into::into)
        }
    }
}

pub fn rename_everywhere(data_dir: &Path, dsh_home: &Path, id: &str, title: &str) {
    let paths = DshPaths { data_dir, dsh_home };
    rename_index(paths, id, title);
    rename_listings(paths, id, title);
}

pub fn delete_everywhere(data_dir: &Path, dsh_home: &Path, id: &str) {
    let paths = DshPaths { data_dir, dsh_home };
    delete_index(paths, id);
    delete_listings(paths, id);
    delete_projcache(paths, id);
    delete_prefs(paths, id);
    workspace_registry_update(paths, id, None);
}

pub fn move_everywhere(data_dir: &Path, dsh_home: &Path, id: &str, old_cwd: &str, new_cwd: &str) {
    let paths = DshPaths { data_dir, dsh_home };
    move_listings(paths, id, new_cwd);
    move_projcache(paths, id, new_cwd);
    workspace_registry_update(paths, id, Some((old_cwd, new_cwd)));
}

fn rename_index(paths: DshPaths, id: &str, title: &str) {
    let path = paths.data_dir.join("session-index.json");
    let Some(mut value) = read_json(&path) else {
        return;
    };
    let mut changed = false;
    if let Some(entry) = value
        .get_mut("entries")
        .and_then(|item| item.as_object_mut())
        .and_then(|entries| entries.get_mut(id))
    {
        if let Some(derived) = entry.get_mut("derived").and_then(|item| item.as_object_mut()) {
            derived.insert("title".to_string(), json!(title));
            derived.insert("titleSource".to_string(), json!("renamed"));
            changed = true;
        }
    }
    if changed {
        let _ = write_json(&path, &value);
    }
}

fn delete_index(paths: DshPaths, id: &str) {
    let path = paths.data_dir.join("session-index.json");
    let Some(mut value) = read_json(&path) else {
        return;
    };
    let removed = value
        .get_mut("entries")
        .and_then(|item| item.as_object_mut())
        .and_then(|entries| entries.remove(id))
        .is_some();
    if removed {
        let _ = write_json(&path, &value);
    }
}

fn listing_files(paths: DshPaths) -> Vec<PathBuf> {
    let dir = paths.data_dir.join("session-lists");
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect()
}

fn rename_listings(paths: DshPaths, id: &str, title: &str) {
    for path in listing_files(paths) {
        let Some(mut value) = read_json(&path) else {
            continue;
        };
        let Some(rows) = value.get_mut("rows").and_then(|item| item.as_array_mut()) else {
            continue;
        };
        let mut changed = false;
        for row in rows.iter_mut() {
            if row.get("id").and_then(|item| item.as_str()) == Some(id) {
                row["title"] = json!({ "text": title, "source": "renamed" });
                changed = true;
            }
        }
        if changed {
            let _ = write_json(&path, &value);
        }
    }
}

fn delete_listings(paths: DshPaths, id: &str) {
    for path in listing_files(paths) {
        let Some(mut value) = read_json(&path) else {
            continue;
        };
        let Some(rows) = value.get_mut("rows").and_then(|item| item.as_array_mut()) else {
            continue;
        };
        let before = rows.len();
        rows.retain(|row| row.get("id").and_then(|item| item.as_str()) != Some(id));
        if rows.len() != before {
            let _ = write_json(&path, &value);
        }
    }
}

fn move_listings(paths: DshPaths, id: &str, new_cwd: &str) {
    for path in listing_files(paths) {
        let Some(mut value) = read_json(&path) else {
            continue;
        };
        let Some(rows) = value.get_mut("rows").and_then(|item| item.as_array_mut()) else {
            continue;
        };
        let mut changed = false;
        for row in rows.iter_mut() {
            if row.get("id").and_then(|item| item.as_str()) == Some(id) {
                row["cwd"] = json!(new_cwd);
                changed = true;
            }
        }
        if changed {
            let _ = write_json(&path, &value);
        }
    }
}

fn projcache_dir(paths: DshPaths) -> PathBuf {
    paths
        .dsh_home
        .join("storages")
        .join("session_projcache")
        .join("sessions")
}

fn projcache_matches(stem: &str, id: &str) -> bool {
    stem == id || stem == format!("session-{id}")
}

fn delete_projcache(paths: DshPaths, id: &str) {
    let dir = projcache_dir(paths);
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if projcache_matches(stem, id) {
                let _ = fs::remove_file(path);
            }
        }
    }

    let path = paths.dsh_home.join("storages").join("session_projcache.json");
    let Some(mut value) = read_json(&path) else {
        return;
    };
    let mut changed = false;
    if let Some(sessions) = value
        .get_mut("tables")
        .and_then(|item| item.get_mut("sessions"))
        .and_then(|item| item.as_object_mut())
    {
        let keys: Vec<String> = sessions
            .keys()
            .filter(|key| projcache_matches(key, id))
            .cloned()
            .collect();
        for key in keys {
            sessions.remove(&key);
            changed = true;
        }
    }
    if changed {
        let _ = write_json(&path, &value);
    }
}

fn move_projcache(paths: DshPaths, id: &str, new_cwd: &str) {
    let dir = projcache_dir(paths);
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if !projcache_matches(stem, id) {
                continue;
            }
            let Some(mut value) = read_json(&path) else {
                continue;
            };
            if set_identity_cwd(&mut value, new_cwd) {
                let _ = write_json(&path, &value);
            }
        }
    }

    let path = paths.dsh_home.join("storages").join("session_projcache.json");
    let Some(mut value) = read_json(&path) else {
        return;
    };
    let mut changed = false;
    if let Some(sessions) = value
        .get_mut("tables")
        .and_then(|item| item.get_mut("sessions"))
        .and_then(|item| item.as_object_mut())
    {
        for (key, session) in sessions.iter_mut() {
            if projcache_matches(key, id) && set_identity_cwd(session, new_cwd) {
                changed = true;
            }
        }
    }
    if changed {
        let _ = write_json(&path, &value);
    }
}

fn set_identity_cwd(value: &mut Value, new_cwd: &str) -> bool {
    let Some(identity) = value
        .pointer_mut("/record/identity")
        .and_then(|item| item.as_object_mut())
    else {
        return false;
    };
    identity.insert("cwd".to_string(), json!(new_cwd));
    true
}

fn delete_prefs(paths: DshPaths, id: &str) {
    let last_used = paths.data_dir.join("last-used.json");
    if let Some(mut value) = read_json(&last_used) {
        if value
            .as_object_mut()
            .map(|map| map.remove(id).is_some())
            .unwrap_or(false)
        {
            let _ = write_json(&last_used, &value);
        }
    }

    let resume = paths.data_dir.join("resume.txt");
    if let Ok(text) = fs::read_to_string(&resume) {
        if text.trim() == id {
            let _ = fs::write(&resume, "");
        }
    }

    let agent_view = paths.data_dir.join("agent-view-sessions.json");
    if let Some(mut value) = read_json(&agent_view) {
        if value
            .as_object_mut()
            .map(|map| map.remove(id).is_some())
            .unwrap_or(false)
        {
            let _ = write_json(&agent_view, &value);
        }
    }

    let pins = paths.data_dir.join("session-pins.json");
    if let Some(mut value) = read_json(&pins) {
        if let Some(ids) = value.as_array_mut() {
            let before = ids.len();
            ids.retain(|item| item.as_str() != Some(id));
            if ids.len() != before {
                let _ = write_json(&pins, &value);
            }
        }
    }

    let mounts = paths.data_dir.join("session-mounts.json");
    if let Some(mut value) = read_json(&mounts) {
        let mut changed = false;
        if let Some(owners) = value.get_mut("owners").and_then(|item| item.as_array_mut()) {
            for owner in owners.iter_mut() {
                if let Some(ids) = owner.get_mut("sessionIds").and_then(|item| item.as_array_mut()) {
                    let before = ids.len();
                    ids.retain(|item| item.as_str() != Some(id));
                    if ids.len() != before {
                        changed = true;
                    }
                }
            }
            let before = owners.len();
            owners.retain(|owner| {
                owner
                    .get("sessionIds")
                    .and_then(|item| item.as_array())
                    .map(|ids| !ids.is_empty())
                    .unwrap_or(true)
            });
            if owners.len() != before {
                changed = true;
            }
        }
        if changed {
            let _ = write_json(&mounts, &value);
        }
    }
}

fn workspace_registry_update(paths: DshPaths, id: &str, moved: Option<(&str, &str)>) {
    let path = paths.dsh_home.join("storages").join("workspace.json");
    let Some(mut value) = read_json(&path) else {
        return;
    };
    let now = iso_now();
    let mut new_workspace_id: Option<String> = None;

    {
        let Some(workspaces) = value
            .pointer_mut("/tables/workspaces")
            .and_then(|item| item.as_object_mut())
        else {
            return;
        };

        for workspace in workspaces.values_mut() {
            if let Some(ids) = workspace.get_mut("sessionIds").and_then(|item| item.as_array_mut()) {
                ids.retain(|item| item.as_str() != Some(id));
            }
        }

        if let Some((_old_cwd, new_cwd)) = moved {
            let existing = workspaces.iter().find_map(|(workspace_id, workspace)| {
                let path_matches = workspace
                    .get("path")
                    .and_then(|item| item.as_str())
                    .map(|workspace_path| path_eq(workspace_path, new_cwd))
                    .unwrap_or(false);
                path_matches.then(|| workspace_id.clone())
            });

            match existing {
                Some(workspace_id) => {
                    if let Some(workspace) = workspaces.get_mut(&workspace_id) {
                        if let Some(ids) = workspace.get_mut("sessionIds").and_then(|item| item.as_array_mut()) {
                            if !ids.iter().any(|item| item.as_str() == Some(id)) {
                                ids.push(json!(id));
                            }
                        }
                        workspace["updatedAt"] = json!(now);
                    }
                    new_workspace_id = Some(workspace_id);
                }
                None => {
                    let workspace_id = uuid::Uuid::new_v4().to_string();
                    workspaces.insert(
                        workspace_id.clone(),
                        json!({
                            "path": new_cwd,
                            "title": title_of_path(new_cwd),
                            "sessionIds": [id],
                            "createdAt": now,
                            "updatedAt": now,
                        }),
                    );
                    new_workspace_id = Some(workspace_id);
                }
            }
        }
    }

    if let Some(workspace_id) = new_workspace_id {
        if let Some(global) = value.get_mut("global").and_then(|item| item.as_object_mut()) {
            if let Some(ids) = global.get_mut("workspaceIds").and_then(|item| item.as_array_mut()) {
                if !ids.iter().any(|item| item.as_str() == Some(&workspace_id)) {
                    ids.push(json!(workspace_id));
                }
            }
        }
    }

    if moved.is_none() {
        if let Some(global) = value.get_mut("global").and_then(|item| item.as_object_mut()) {
            for key in ["pinnedSessionIds", "archivedSessionIds"] {
                if let Some(ids) = global.get_mut(key).and_then(|item| item.as_array_mut()) {
                    ids.retain(|item| item.as_str() != Some(id));
                }
            }
        }
    }

    let _ = write_json(&path, &value);
}

fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn title_of_path(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn path_eq(a: &str, b: &str) -> bool {
    normalize_cmp(a) == normalize_cmp(b)
}

fn normalize_cmp(path: &str) -> String {
    let mut value = path.replace('/', "\\");
    while value.len() > 3 && value.ends_with('\\') {
        value.pop();
    }
    if cfg!(windows) {
        value.make_ascii_lowercase();
    }
    value
}
