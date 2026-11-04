//! The tool's own preferences, kept beside the dsh-tui data dir.
//!
//! Only the language is stored today. `null` / absent means "follow the
//! system", mirroring HaruClash's `Option<String>` configuration.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::cache;
use crate::i18n::Language;

const FILE: &str = "dst-session-manager.json";

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

pub fn load_language(data_dir: &Path) -> Option<Language> {
    let value = cache::read_json(&path(data_dir))?;
    let code = value.get("language")?.as_str()?;
    Language::from_code(code)
}

/// Stores an explicit language, or `None` to follow the system again.
pub fn save_choice(data_dir: &Path, choice: Option<Language>) -> Result<()> {
    let target = path(data_dir);
    let mut value = cache::read_json(&target).unwrap_or_else(|| json!({}));
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
    cache::write_atomic(&target, &bytes)
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
}
