//! The source-agnostic view of a session.
//!
//! Nothing here knows what DST or opencode are: a [`Session`] is the fields the
//! list and the detail pane always show, plus an opaque payload that only the
//! source that produced it may read back (see [`Session::data`]).

use std::any::Any;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::i18n::t;

/// One line of the detail pane.
///
/// Labels and values are built by the owning source on every draw, so they
/// follow a language switch without reloading anything.
pub struct DetailRow {
    pub label: String,
    pub value: String,
}

impl DetailRow {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// A session as every part of the tool sees it.
#[derive(Clone)]
pub struct Session {
    /// Id of the [`crate::source::SessionSource`] that owns this session.
    pub source: &'static str,
    pub id: String,
    pub title: String,
    /// Where the title came from, translated by the UI (`title_source.*`).
    pub title_source: String,
    pub cwd: String,
    pub created_at: u64,
    pub mtime_ms: u64,
    pub parent: Option<String>,
    pub agent_preset: Option<String>,
    pub model: Option<String>,
    /// Set by sources that can tell a session is being written right now.
    pub mounted_pid: Option<u32>,
    /// Whatever the source needs to mutate this session later.
    pub(crate) data: Arc<dyn Any>,
}

impl Session {
    /// The source-specific payload, or an error when the wrong source got the
    /// session (which would be a bug in this tool, not in the data).
    pub(crate) fn data<T: 'static>(&self) -> Result<&T> {
        self.data
            .downcast_ref::<T>()
            .ok_or_else(|| anyhow!(t("err.wrong_source")))
    }

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
            t("common.untitled")
        } else {
            self.title.clone()
        }
    }
}

/// Bytes in the unit the list has always used. Sources that measure something
/// else (message counts) format their own string instead.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> Session {
        Session {
            source: "test",
            id: id.to_string(),
            title: "  ".to_string(),
            title_source: "opencode".to_string(),
            cwd: String::new(),
            created_at: 0,
            mtime_ms: 0,
            parent: None,
            agent_preset: None,
            model: None,
            mounted_pid: None,
            data: Arc::new(42u8),
        }
    }

    #[test]
    fn short_ids_are_cut_at_a_character_boundary() {
        assert_eq!(session("ses_e022f4f16ffeAU4ghSq0a8dyM4").short_id(), "ses_e022f4f1…");
        assert_eq!(session("0123456789abc").short_id(), "0123456789abc");
    }

    #[test]
    fn an_empty_title_still_reads_as_something() {
        // The language is process-wide; hold the lock the i18n tests use so a
        // concurrent switch cannot slip between the two calls.
        let _guard = crate::i18n::TEST_LOCK.lock().unwrap();
        assert_eq!(session("x").display_title(), t("common.untitled"));
        assert!(!session("x").display_title().is_empty());
    }

    #[test]
    fn sizes_read_like_a_person_wrote_them() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_payload_can_only_be_read_back_by_its_owner() {
        let session = session("s");
        assert_eq!(*session.data::<u8>().unwrap(), 42);
        assert!(session.data::<String>().is_err());
    }
}
