//! The tool's own preferences, kept beside the dsh-tui data dir.
//!
//! Only the language is stored today. `null` / absent means "follow the
//! system", mirroring HaruClash's `Option<String>` configuration.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::cache;
use crate::i18n::Language;

const FILE: &str = "roster.json";

/// The tool shipped as `dst-session-manager`, then `agent-session-manager`; a
/// file from those releases still holds the user's choice.
const LEGACY_FILES: &[&str] = &["agent-session-manager.json", "dst-session-manager.json"];

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

fn legacy_paths(data_dir: &Path) -> Vec<PathBuf> {
    LEGACY_FILES.iter().map(|name| data_dir.join(name)).collect()
}

pub fn load_language(data_dir: &Path) -> Option<Language> {
    let value = std::iter::once(path(data_dir))
        .chain(legacy_paths(data_dir))
        .find_map(|candidate| cache::read_json(&candidate))?;
    let code = value.get("language")?.as_str()?;
    Language::from_code(code)
}

/// Stores an explicit language, or `None` to follow the system again.
pub fn save_choice(data_dir: &Path, choice: Option<Language>) -> Result<()> {
    let target = path(data_dir);
    let mut value = std::iter::once(path(data_dir))
        .chain(legacy_paths(data_dir))
        .find_map(|candidate| cache::read_json(&candidate))
        .unwrap_or_else(|| json!({}));
    if !value.is_object() {
        value = json!({});
    }
    match choice {
        Some(language) => {
            value["language"] = Value::String(language.code().to_string());
        }
        None => {
            if let Some(object) = value.as_object_mut() {
                object.remove("language");
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&value)?;
    cache::write_atomic(&target, &bytes)?;
    // The old file has been carried over; drop it so the two cannot disagree.
    for legacy in legacy_paths(data_dir) {
        let _ = fs::remove_file(legacy);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_round_trips() {
        let dir = std::env::temp_dir().join(format!("dstsm-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(load_language(&dir), None);
        save_choice(&dir, Some(Language::English)).unwrap();
        assert_eq!(load_language(&dir), Some(Language::English));
        save_choice(&dir, Some(Language::Chinese)).unwrap();
        assert_eq!(load_language(&dir), Some(Language::Chinese));
        save_choice(&dir, None).unwrap();
        assert_eq!(load_language(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tool was renamed; old files must still be honoured, and then
    /// retired so the two cannot disagree later.
    #[test]
    fn the_old_file_names_are_honoured_and_migrated() {
        for legacy in LEGACY_FILES {
            let dir = std::env::temp_dir().join(format!("dstsm-settings-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let legacy = dir.join(legacy);
            std::fs::write(&legacy, br#"{"language":"en"}"#).unwrap();

            assert_eq!(load_language(&dir), Some(Language::English));

            save_choice(&dir, Some(Language::Chinese)).unwrap();
            assert_eq!(load_language(&dir), Some(Language::Chinese));
            assert!(path(&dir).exists(), "the new file is written");
            assert!(!legacy.exists(), "the old file is retired");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
