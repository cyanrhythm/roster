//! Reading and mutating opencode sessions.
//!
//! opencode keeps everything in one SQLite database, `<data_dir>/opencode.db`.
//! The `session` table is a materialised view of an event log: every change is
//! appended to `event` under the session id as aggregate, with a per-aggregate
//! sequence in `event_sequence`. A write that only touches the `session` row
//! would therefore be forgotten the next time opencode replays its events, so
//! mutations here update the row *and* append a `session.*` event carrying the
//! same change, inside one transaction.
//!
//! Columns have come and gone between opencode releases (`cost`, the token
//! counters and `path` are all fairly new), so every read goes through a
//! column-name lookup and simply skips what the database does not have.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde_json::{json, Value};

use crate::i18n::t;
use crate::mutate::now_ms;
use crate::store::{LoadResult, Location, Session};

const DB_NAME: &str = "opencode.db";

/// One directory with a database, or nothing at all.
pub fn load(data_dir: &Path) -> LoadResult {
    let db = data_dir.join(DB_NAME);
    if !db.is_file() {
        return LoadResult::default();
    }
    match load_db(&db) {
        Ok(sessions) => LoadResult {
            sessions,
            ..LoadResult::default()
        },
        Err(error) => LoadResult {
            errors: vec![format!("{}: {error}", db.display())],
            ..LoadResult::default()
        },
    }
}

fn load_db(db: &Path) -> Result<Vec<Session>> {
    let conn = connect(db, true)?;
    let columns = column_indexes(&conn, "session")?;
    if columns.is_empty() {
        bail!(t("err.opencode_schema"));
    }

    let projects = load_projects(&conn).unwrap_or_default();
    let messages = grouped_counts(&conn, "message", "session_id", "COUNT(*)").unwrap_or_default();
    let events = grouped_counts(&conn, "event_sequence", "aggregate_id", "MAX(seq) + 1").unwrap_or_default();

    let mut stmt = conn.prepare("SELECT * FROM session ORDER BY time_updated DESC")?;
    let mut rows = stmt.query([])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        sessions.push(read_row(row, &columns, &projects, &messages, &events, db)?);
    }
    Ok(sessions)
}

#[derive(Clone, Debug, Default)]
struct Project {
    worktree: Option<String>,
}

fn load_projects(conn: &Connection) -> Result<HashMap<String, Project>> {
    let columns = column_indexes(conn, "project")?;
    if columns.is_empty() {
        return Ok(HashMap::new());
    }
    let mut stmt = conn.prepare("SELECT * FROM project")?;
    let mut rows = stmt.query([])?;
    let mut projects = HashMap::new();
    while let Some(row) = rows.next()? {
        let Some(id) = text(row, &columns, "id") else {
            continue;
        };
        projects.insert(
            id,
            Project {
                worktree: text(row, &columns, "worktree"),
            },
        );
    }
    Ok(projects)
}

/// `SELECT <key>, <agg> FROM <table> GROUP BY <key>` as a map, or nothing when
/// the table is missing in this opencode version.
fn grouped_counts(conn: &Connection, table: &str, key: &str, aggregate: &str) -> Result<HashMap<String, u64>> {
    if column_indexes(conn, table)?.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!("SELECT {key}, {aggregate} FROM {table} GROUP BY {key}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut counts = HashMap::new();
    while let Some(row) = rows.next()? {
        let key: String = match row.get(0) {
            Ok(key) => key,
            Err(_) => continue,
        };
        let count: i64 = row.get(1).unwrap_or(0);
        counts.insert(key, count.max(0) as u64);
    }
    Ok(counts)
}

fn read_row(
    row: &Row,
    columns: &HashMap<String, usize>,
    projects: &HashMap<String, Project>,
    messages: &HashMap<String, u64>,
    events: &HashMap<String, u64>,
    db: &Path,
) -> Result<Session> {
    let id = text(row, columns, "id").ok_or_else(|| anyhow!(t("err.opencode_schema")))?;
    let project_id = text(row, columns, "project_id").unwrap_or_default();
    let project = projects.get(&project_id);
    let worktree = project.and_then(|project| project.worktree.clone());

    let raw_title = text(row, columns, "title").unwrap_or_default();
    let slug = text(row, columns, "slug").unwrap_or_default();
    let (title, title_source) = if raw_title.trim().is_empty() {
        (slug, "slug")
    } else {
        (raw_title, "opencode")
    };

    let directory = text(row, columns, "directory")
        .filter(|dir| !dir.trim().is_empty())
        .or_else(|| worktree.clone())
        .unwrap_or_default();

    let messages = messages.get(&id).copied().unwrap_or(0);
    let events = events.get(&id).copied().unwrap_or(0);

    Ok(Session {
        cwd: directory,
        created_at: integer(row, columns, "time_created").unwrap_or(0).max(0) as u64,
        mtime_ms: integer(row, columns, "time_updated").unwrap_or(0).max(0) as u64,
        title: title.trim().to_string(),
        title_source: title_source.to_string(),
        parent: text(row, columns, "parent_id").filter(|id| !id.is_empty()),
        agent_preset: text(row, columns, "agent").filter(|agent| !agent.trim().is_empty()),
        model: text(row, columns, "model").as_deref().and_then(model_label),
        id,
        location: Location::Opencode {
            db: db.to_path_buf(),
            project_id,
            worktree,
            messages,
            events,
            cost: real(row, columns, "cost").unwrap_or(0.0),
            tokens_in: integer(row, columns, "tokens_input").unwrap_or(0).max(0) as u64,
            tokens_out: integer(row, columns, "tokens_output").unwrap_or(0).max(0) as u64,
            tokens_reasoning: integer(row, columns, "tokens_reasoning").unwrap_or(0).max(0) as u64,
            tokens_cache_read: integer(row, columns, "tokens_cache_read").unwrap_or(0).max(0) as u64,
            tokens_cache_write: integer(row, columns, "tokens_cache_write").unwrap_or(0).max(0) as u64,
        },
    })
}

/// `{"providerID":"deepseek","id":"deepseek-flash"}` is unreadable in a list.
fn model_label(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Some(raw.to_string());
    };
    let id = value.get("id").and_then(|item| item.as_str());
    let provider = value.get("providerID").and_then(|item| item.as_str());
    match (provider, id) {
        (Some(provider), Some(id)) => Some(format!("{provider}/{id}")),
        (None, Some(id)) => Some(id.to_string()),
        _ => Some(raw.to_string()),
    }
}

fn text(row: &Row, columns: &HashMap<String, usize>, name: &str) -> Option<String> {
    let index = *columns.get(name)?;
    row.get::<usize, Option<String>>(index).ok().flatten()
}

fn integer(row: &Row, columns: &HashMap<String, usize>, name: &str) -> Option<i64> {
    let index = *columns.get(name)?;
    row.get::<usize, Option<i64>>(index).ok().flatten()
}

fn real(row: &Row, columns: &HashMap<String, usize>, name: &str) -> Option<f64> {
    let index = *columns.get(name)?;
    row.get::<usize, Option<f64>>(index).ok().flatten()
}

fn column_indexes(conn: &Connection, table: &str) -> Result<HashMap<String, usize>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut columns = HashMap::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        let index: i64 = row.get(0)?;
        columns.insert(name, index.max(0) as usize);
    }
    Ok(columns)
}

fn connect(db: &Path, read_only: bool) -> Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let conn = Connection::open_with_flags(db, flags)?;
    // opencode may be running; wait for its writer instead of failing.
    conn.busy_timeout(Duration::from_millis(2000))?;
    Ok(conn)
}

fn open_for_write(db: &Path) -> Result<Connection> {
    let conn = connect(db, false)?;
    // Deletes rely on `ON DELETE CASCADE`, which SQLite leaves off by default.
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

fn expect(session: &Session) -> Result<(&Path, &str, Option<&str>)> {
    match &session.location {
        Location::Opencode {
            db,
            project_id,
            worktree,
            ..
        } => Ok((db.as_path(), project_id.as_str(), worktree.as_deref())),
        Location::Dst { .. } => bail!(t("err.wrong_source")),
    }
}

pub fn rename(session: &Session, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!(t("err.title_empty"));
    }
    let (db, _, _) = expect(session)?;
    let mut conn = open_for_write(db)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();

    let changed = tx.execute(
        "UPDATE session SET title = ?1, time_updated = ?2 WHERE id = ?3",
        params![title, now as i64, session.id],
    )?;
    if changed == 0 {
        bail!(t("err.session_missing"));
    }
    append_session_event(&tx, &session.id, |info| {
        info["title"] = json!(title);
    })?;

    tx.commit()?;
    Ok(())
}

pub fn change_workspace(session: &Session, new_cwd: &str) -> Result<String> {
    let new_dir = slash_path(new_cwd.trim().trim_matches('"'));
    if new_dir.is_empty() {
        bail!(t("err.cwd_empty"));
    }
    if !Path::new(&new_dir).is_absolute() {
        bail!(t("err.cwd_absolute"));
    }
    let (db, project_id, worktree) = expect(session)?;
    if path_key(&new_dir) == path_key(&session.cwd) {
        return Ok(new_dir);
    }

    let mut conn = open_for_write(db)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // A session belongs to the project whose worktree contains it. Moving
    // inside the same repository is the normal case; when the new directory
    // belongs to no known project, keep the old link rather than inventing a
    // project id (opencode derives those from the repository's root commit).
    let target = find_project(&tx, &new_dir)?;
    let (project_id, worktree) = match target {
        Some((project_id, worktree)) => (project_id, worktree),
        None => (project_id.to_string(), worktree.map(str::to_string)),
    };
    let relative = relative_path(worktree.as_deref(), &new_dir);
    let now = now_ms();

    let changed = tx.execute(
        "UPDATE session SET directory = ?1, project_id = ?2, path = ?3, time_updated = ?4 WHERE id = ?5",
        params![new_dir, project_id, relative, now as i64, session.id],
    )?;
    if changed == 0 {
        bail!(t("err.session_missing"));
    }
    append_session_event(&tx, &session.id, |info| {
        info["directory"] = json!(new_dir);
        info["projectID"] = json!(project_id);
        info["path"] = json!(relative);
    })?;

    tx.commit()?;
    Ok(new_dir)
}

/// Deletes the session and everything hanging off it. There is no copy kept:
/// the confirmation dialog is the only safety net.
pub fn delete(session: &Session) -> Result<()> {
    let (db, _, _) = expect(session)?;

    let mut conn = open_for_write(db)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Delete the satellites by hand: leaning on `ON DELETE CASCADE` would
    // require every historical schema to have declared the same constraints.
    for (table, column) in [
        ("part", "session_id"),
        ("message", "session_id"),
        ("todo", "session_id"),
        ("session_share", "session_id"),
        ("session_message", "session_id"),
        ("session_input", "session_id"),
        ("session_context_epoch", "session_id"),
        ("event", "aggregate_id"),
        ("event_sequence", "aggregate_id"),
    ] {
        let _ = tx.execute(&format!("DELETE FROM {table} WHERE {column} = ?1"), params![session.id]);
    }
    tx.execute("DELETE FROM session WHERE id = ?1", params![session.id])?;
    tx.commit()?;

    // Derived artifact opencode writes beside the database, not inside it.
    if let Some(dir) = db.parent() {
        let artifact = dir
            .join("storage")
            .join("session_diff")
            .join(format!("{}.json", session.id));
        let _ = fs::remove_file(artifact);
    }
    Ok(())
}

/// The newest `session.*` event of a session, which is the state opencode would
/// replay from.
fn latest_session_event(tx: &Transaction, id: &str) -> Result<Option<(String, String)>> {
    let mut stmt = tx.prepare(
        "SELECT type, data FROM event WHERE aggregate_id = ?1 AND type LIKE 'session.%' ORDER BY seq DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let kind: String = row.get(0)?;
    let data: String = row.get(1)?;
    Ok(Some((kind, data)))
}

/// The next sequence number of an aggregate.
///
/// `event.seq` is shared by every event type in the aggregate, so counting from
/// the newest `session.*` event would collide with the message events that came
/// after it. The counter table and the highest row are both consulted, in case
/// one of them was left behind by a crashed writer.
fn next_sequence(tx: &Transaction, id: &str) -> Result<i64> {
    let highest: Option<i64> = tx.query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    let counter: Option<i64> = tx
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(highest.max(counter).map(|seq| seq + 1).unwrap_or(0))
}

/// Appends the same change opencode itself would append, keeping the event log
/// and the materialised row in step. Sessions that predate the event log have
/// nothing to append to and are left alone.
fn append_session_event(tx: &Transaction, id: &str, patch: impl FnOnce(&mut Value)) -> Result<()> {
    let Some((kind, data)) = latest_session_event(tx, id)? else {
        return Ok(());
    };
    let mut event: Value = serde_json::from_str(&data)?;
    if let Some(info) = event.get_mut("info") {
        if let Some(time) = info.get_mut("time").and_then(|time| time.as_object_mut()) {
            time.insert("updated".to_string(), json!(now_ms()));
        }
        patch(info);
    } else {
        patch(&mut event);
    }

    let next = next_sequence(tx, id)?;
    let updated = tx.execute(
        "UPDATE event_sequence SET seq = ?1 WHERE aggregate_id = ?2",
        params![next, id],
    )?;
    if updated == 0 {
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2)",
            params![id, next],
        )?;
    }
    tx.execute(
        "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            format!("evt_{}", uuid::Uuid::new_v4().simple()),
            id,
            next,
            kind,
            event.to_string()
        ],
    )?;
    Ok(())
}

/// Finds the project a directory belongs to, most specific worktree first.
fn find_project(tx: &Transaction, directory: &str) -> Result<Option<(String, Option<String>)>> {
    let columns = column_indexes(tx, "project")?;
    if columns.is_empty() {
        return Ok(None);
    }
    let wants = path_key(directory);
    let mut stmt = tx.prepare("SELECT * FROM project")?;
    let mut rows = stmt.query([])?;
    let mut best: Option<(usize, String, Option<String>)> = None;
    while let Some(row) = rows.next()? {
        let Some(id) = text(row, &columns, "id") else {
            continue;
        };
        let worktree = text(row, &columns, "worktree").filter(|path| !path.trim().is_empty());
        let Some(worktree) = worktree else {
            continue;
        };
        let key = path_key(&worktree);
        if key.is_empty() || !(wants == key || wants.starts_with(&format!("{key}/"))) {
            continue;
        }
        let length = key.len();
        if best.as_ref().map(|(best, _, _)| length > *best).unwrap_or(true) {
            best = Some((length, id, Some(worktree)));
        }
    }
    Ok(best.map(|(_, id, worktree)| (id, worktree)))
}

/// `path` column: the directory relative to its project worktree, in the
/// forward-slash form opencode stores.
fn relative_path(worktree: Option<&str>, directory: &str) -> String {
    let Some(worktree) = worktree else {
        return String::new();
    };
    let base = path_key(worktree);
    let target = path_key(directory);
    if base.is_empty() {
        return String::new();
    }
    if target == base {
        return String::new();
    }
    target
        .strip_prefix(&format!("{base}/"))
        .map(str::to_string)
        .unwrap_or_default()
}

/// opencode writes forward slashes on every platform.
fn slash_path(path: &str) -> String {
    let mut value = path.replace('\\', "/");
    while value.len() > 3 && value.ends_with('/') {
        value.pop();
    }
    value
}

/// Comparison key: forward slashes, no trailing slash, case-insensitive on
/// Windows.
fn path_key(path: &str) -> String {
    let mut key = slash_path(path);
    if cfg!(windows) {
        key.make_ascii_lowercase();
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::store::Kind;

    fn fixture(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let db = dir.join(DB_NAME);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (
                id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
                time_created integer NOT NULL, time_updated integer NOT NULL
            );
            CREATE TABLE session (
                id text PRIMARY KEY, project_id text NOT NULL, parent_id text, slug text NOT NULL,
                directory text NOT NULL, title text NOT NULL, version text NOT NULL,
                time_created integer NOT NULL, time_updated integer NOT NULL, path text,
                agent text, model text, cost real DEFAULT 0 NOT NULL,
                tokens_input integer DEFAULT 0 NOT NULL, tokens_output integer DEFAULT 0 NOT NULL,
                tokens_reasoning integer DEFAULT 0 NOT NULL,
                tokens_cache_read integer DEFAULT 0 NOT NULL, tokens_cache_write integer DEFAULT 0 NOT NULL
            );
            CREATE TABLE message (
                id text PRIMARY KEY, session_id text NOT NULL, time_created integer NOT NULL,
                time_updated integer NOT NULL, data text NOT NULL,
                FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE
            );
            CREATE TABLE part (
                id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
                time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL,
                FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE
            );
            CREATE TABLE todo (
                session_id text NOT NULL, content text NOT NULL, status text NOT NULL,
                priority text NOT NULL, position integer NOT NULL,
                time_created integer NOT NULL, time_updated integer NOT NULL,
                PRIMARY KEY (session_id, position),
                FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE
            );
            CREATE TABLE event_sequence (aggregate_id text PRIMARY KEY, seq integer NOT NULL, owner_id text);
            CREATE TABLE event (
                id text PRIMARY KEY, aggregate_id text NOT NULL, seq integer NOT NULL,
                type text NOT NULL, data text NOT NULL
            );
            INSERT INTO project VALUES ('proj-old', 'E:/code/old', 'git', NULL, 1, 1);
            INSERT INTO project VALUES ('proj-new', 'E:/code/new', 'git', NULL, 1, 1);
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated, path, agent, model)
            VALUES ('ses_one', 'proj-old', 'calm-circuit', 'E:/code/old', 'first title', '1.0.0', 1000, 2000, '', 'build',
                    '{"providerID":"deepseek","id":"deepseek-flash"}');
            INSERT INTO message VALUES ('msg_1', 'ses_one', 1, 1, '{"role":"user"}');
            INSERT INTO message VALUES ('msg_2', 'ses_one', 2, 2, '{"role":"assistant"}');
            INSERT INTO part VALUES ('prt_1', 'msg_1', 'ses_one', 1, 1, '{"text":"hi"}');
            INSERT INTO event_sequence VALUES ('ses_one', 2, NULL);
            INSERT INTO event VALUES ('evt_1', 'ses_one', 0, 'session.created.1',
                '{"sessionID":"ses_one","info":{"id":"ses_one","title":"first title","time":{"created":1000,"updated":2000},"directory":"E:/code/old","projectID":"proj-old"}}');
            INSERT INTO event VALUES ('evt_2', 'ses_one', 1, 'message.updated.1', '{"x":1}');
            INSERT INTO event VALUES ('evt_3', 'ses_one', 2, 'session.updated.1',
                '{"sessionID":"ses_one","info":{"id":"ses_one","title":"first title","time":{"created":1000,"updated":2000},"directory":"E:/code/old","projectID":"proj-old"}}');
            "#,
        )
        .unwrap();
        db
    }

    fn one(db: &Path) -> Session {
        let loaded = load(db.parent().unwrap());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.sessions.len(), 1);
        let session = loaded.sessions[0].clone();
        assert_eq!(session.kind(), Kind::Opencode);
        session
    }

    #[test]
    fn reads_sessions_with_counts_and_model() {
        let dir = std::env::temp_dir().join(format!("roster-oc-{}", uuid::Uuid::new_v4()));
        let db = fixture(&dir);
        let session = one(&db);

        assert_eq!(session.id, "ses_one");
        assert_eq!(session.title, "first title");
        assert_eq!(session.cwd, "E:/code/old");
        assert_eq!(session.model.as_deref(), Some("deepseek/deepseek-flash"));
        assert_eq!(session.agent_preset.as_deref(), Some("build"));
        match &session.location {
            Location::Opencode {
                messages,
                events,
                worktree,
                ..
            } => {
                assert_eq!(*messages, 2);
                assert_eq!(*events, 3);
                assert_eq!(worktree.as_deref(), Some("E:/code/old"));
            }
            other => panic!("wrong location: {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_updates_the_row_and_appends_an_event() {
        let dir = std::env::temp_dir().join(format!("roster-oc-{}", uuid::Uuid::new_v4()));
        let db = fixture(&dir);
        let session = one(&db);

        // The aggregate keeps counting for every event type, so the newest
        // session event is not the newest event: the message events that come
        // after it already own those sequence numbers.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO event VALUES ('evt_msg', 'ses_one', 3, 'message.updated.1', '{}')",
                [],
            )
            .unwrap();
            conn.execute("UPDATE event_sequence SET seq = 3 WHERE aggregate_id='ses_one'", [])
                .unwrap();
        }

        rename(&session, "  renamed  ").unwrap();

        let conn = Connection::open(&db).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM session WHERE id='ses_one'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "renamed");

        let (seq, kind, data): (i64, String, String) = conn
            .query_row(
                "SELECT seq, type, data FROM event WHERE aggregate_id='ses_one' ORDER BY seq DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(seq, 4, "the event log gets the next sequence number");
        assert_eq!(kind, "session.updated.1");
        let value: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(value["info"]["title"], "renamed");
        assert_eq!(value["info"]["directory"], "E:/code/old", "untouched fields survive");

        let stored: i64 = conn
            .query_row(
                "SELECT seq FROM event_sequence WHERE aggregate_id='ses_one'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_a_workspace_relinks_the_project_and_keeps_the_slash_form() {
        let dir = std::env::temp_dir().join(format!("roster-oc-{}", uuid::Uuid::new_v4()));
        let db = fixture(&dir);
        let session = one(&db);

        let moved = change_workspace(&session, r"E:\code\new\sub").unwrap();
        assert_eq!(moved, "E:/code/new/sub");

        let conn = Connection::open(&db).unwrap();
        let (directory, project, path): (String, String, Option<String>) = conn
            .query_row(
                "SELECT directory, project_id, path FROM session WHERE id='ses_one'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(directory, "E:/code/new/sub");
        assert_eq!(project, "proj-new");
        assert_eq!(path.as_deref(), Some("sub"));

        let data: String = conn
            .query_row(
                "SELECT data FROM event WHERE aggregate_id='ses_one' ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let value: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(value["info"]["directory"], "E:/code/new/sub");
        assert_eq!(value["info"]["projectID"], "proj-new");
        assert_eq!(value["info"]["path"], "sub");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_removes_every_row_and_the_side_artifact() {
        let dir = std::env::temp_dir().join(format!("roster-oc-{}", uuid::Uuid::new_v4()));
        let db = fixture(&dir);
        let session = one(&db);

        // opencode keeps per-session diffs next to the database, not inside it.
        let artifacts = dir.join("storage").join("session_diff");
        fs::create_dir_all(&artifacts).unwrap();
        let artifact = artifacts.join("ses_one.json");
        fs::write(&artifact, b"{}").unwrap();

        delete(&session).unwrap();

        let conn = Connection::open(&db).unwrap();
        for (table, column) in [("session", "id"), ("message", "session_id"), ("part", "session_id")] {
            let left: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column}='ses_one'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(left, 0, "{table} rows are gone");
        }
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM event WHERE aggregate_id='ses_one'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(events, 0, "the event log is cleared with the session");
        assert!(!artifact.exists(), "the side artifact is removed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_database_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("roster-oc-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let loaded = load(&dir);
        assert!(loaded.sessions.is_empty());
        assert!(loaded.errors.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_labels_are_readable() {
        assert_eq!(
            model_label(r#"{"providerID":"deepseek","id":"deepseek-flash"}"#).as_deref(),
            Some("deepseek/deepseek-flash")
        );
        assert_eq!(model_label(r#"{"id":"solo"}"#).as_deref(), Some("solo"));
        assert_eq!(model_label("plain").as_deref(), Some("plain"));
        assert_eq!(model_label("  "), None);
    }

    #[test]
    fn paths_are_compared_the_way_opencode_writes_them() {
        assert_eq!(path_key(r"E:\code\New\"), path_key("e:/code/new"));
        assert_eq!(relative_path(Some("E:/code/new"), "E:/code/new/sub"), "sub");
        assert_eq!(relative_path(Some("E:/code/new"), "E:/code/new"), "");
        assert_eq!(relative_path(Some("E:/code/new"), "E:/elsewhere"), "");
    }
}
