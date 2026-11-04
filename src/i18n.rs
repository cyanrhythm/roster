//! UI copy in more than one language.
//!
//! Two flat YAML tables under `locales/`, parsed once and looked up by key.
//! This follows the `haru-i18n` / HaruClash approach: a table plus a global
//! language, so every module can call `t()` / `tf()` without setting up a
//! per-crate lookup backend.
//!
//! ```ignore
//! i18n::t("app.title");
//! i18n::tf("header.sessions", &[("count", "12")]);
//! ```
//!
//! Missing keys fall back to Chinese and then to the key itself: a visible
//! `popup.delete.title` is easier to find than an empty label.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

/// The languages shipped with the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// 简体中文 — the original copy.
    #[default]
    Chinese,
    English,
}

impl Language {
    /// In the order the switcher offers them.
    pub const ALL: [Self; 2] = [Self::Chinese, Self::English];

    /// The code the tables are named after.
    pub fn code(self) -> &'static str {
        match self {
            Self::Chinese => "zh-CN",
            Self::English => "en",
        }
    }

    /// What to call it in the switcher — in the language itself.
    pub fn label(self) -> &'static str {
        match self {
            Self::Chinese => "简体中文",
            Self::English => "English",
        }
    }

    /// Reads a code from the configuration, being generous about what a user
    /// may have written by hand: `zh`, `zh_CN`, `en-US` all work.
    pub fn from_code(code: &str) -> Option<Self> {
        let code = code.trim().to_ascii_lowercase();
        if code.starts_with("zh") {
            Some(Self::Chinese)
        } else if code.starts_with("en") {
            Some(Self::English)
        } else {
            None
        }
    }

    /// The language the system is set to.
    ///
    /// Only Chinese is recognised; everything else gets English.
    pub fn detect() -> Self {
        if system_speaks_chinese() {
            Self::Chinese
        } else {
            Self::English
        }
    }
}

#[cfg(windows)]
fn system_speaks_chinese() -> bool {
    // 0x04 is `LANG_CHINESE`; the low byte of the UI language is the primary
    // language. `windows-sys` would say the same thing with more ceremony.
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    let primary = unsafe { GetUserDefaultUILanguage() } & 0x00FF;
    primary == 0x04
}

#[cfg(not(windows))]
fn system_speaks_chinese() -> bool {
    ["LANG", "LC_ALL", "LC_MESSAGES"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| value.to_ascii_lowercase().starts_with("zh")))
}

/// Every key of one table.
fn table(language: Language) -> &'static BTreeMap<String, String> {
    static TABLES: OnceLock<[BTreeMap<String, String>; 2]> = OnceLock::new();

    let tables = TABLES.get_or_init(|| {
        [
            parse(include_str!("../locales/zh-CN.yml")),
            parse(include_str!("../locales/en.yml")),
        ]
    });

    match language {
        Language::Chinese => &tables[0],
        Language::English => &tables[1],
    }
}

fn parse(source: &str) -> BTreeMap<String, String> {
    serde_yaml::from_str(source).expect("the locale tables are compiled in and valid YAML")
}

/// The language the window is drawn in. Started from the system's, then owned
/// by whoever calls [`set`].
static CURRENT: OnceLock<RwLock<Language>> = OnceLock::new();

fn current() -> &'static RwLock<Language> {
    CURRENT.get_or_init(|| RwLock::new(Language::detect()))
}

/// Switches the tool's language. The caller repaints.
pub fn set(language: Language) {
    let mut cell = current()
        .write()
        .expect("the language lock is never held across a panic");
    *cell = language;
}

/// The language in use right now.
pub fn language() -> Language {
    *current()
        .read()
        .expect("the language lock is never held across a panic")
}

/// The copy for `key`, in the current language.
pub fn t(key: &str) -> String {
    t_in(language(), key)
}

/// The copy for `key`, in `language`.
pub fn t_in(language: Language, key: &str) -> String {
    if let Some(text) = table(language).get(key) {
        return text.clone();
    }
    if let Some(text) = table(Language::Chinese).get(key) {
        return text.clone();
    }
    key.to_owned()
}

/// The copy for `key`, with `%{name}` placeholders filled in.
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    fill(&t(key), args)
}

/// The copy for `key` in `language`, with `%{name}` placeholders filled in.
#[allow(dead_code)]
pub fn tf_in(language: Language, key: &str, args: &[(&str, &str)]) -> String {
    fill(&t_in(language, key), args)
}

fn fill(text: &str, args: &[(&str, &str)]) -> String {
    let mut out = text.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("%{{{name}}}"), value);
    }
    out
}

/// Serializes tests that assert on the process-wide language.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tables_carry_the_same_keys() {
        let zh = table(Language::Chinese);
        let en = table(Language::English);

        let missing_in_english: Vec<_> = zh.keys().filter(|key| !en.contains_key(*key)).collect();
        let extra_in_english: Vec<_> = en.keys().filter(|key| !zh.contains_key(*key)).collect();
        assert!(missing_in_english.is_empty(), "untranslated: {missing_in_english:?}");
        assert!(extra_in_english.is_empty(), "no Chinese original: {extra_in_english:?}");
        assert!(zh.len() > 70, "the table looks truncated: {} keys", zh.len());
    }

    #[test]
    fn every_placeholder_survives_translation() {
        let zh = table(Language::Chinese);
        for (key, original) in zh {
            let translated = table(Language::English)
                .get(key)
                .expect("both tables carry the same keys");

            let mut wanted = placeholders(original);
            let mut found = placeholders(translated);
            wanted.sort();
            found.sort();
            assert_eq!(wanted, found, "{key}");
        }
    }

    fn placeholders(text: &str) -> Vec<String> {
        text.split("%{")
            .skip(1)
            .filter_map(|rest| rest.split('}').next().map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_readable_string_comes_back_in_both_languages() {
        assert_eq!(t_in(Language::Chinese, "app.title"), "DST 会话管理器");
        assert_eq!(t_in(Language::English, "app.title"), "DST Session Manager");
    }

    #[test]
    fn placeholders_are_filled() {
        let text = tf_in(Language::English, "header.sessions", &[("count", "12")]);
        assert_eq!(text, "12 sessions");
        assert!(!text.contains("%{"), "{text}");

        let text = tf_in(Language::Chinese, "header.sessions", &[("count", "12")]);
        assert_eq!(text, "12 个会话");
    }

    #[test]
    fn a_key_that_does_not_exist_says_so_out_loud() {
        assert_eq!(t_in(Language::Chinese, "nope.missing"), "nope.missing");
        assert_eq!(t_in(Language::English, "nope.missing"), "nope.missing");
    }

    #[test]
    fn codes_are_read_generously() {
        assert_eq!(Language::from_code("zh-CN"), Some(Language::Chinese));
        assert_eq!(Language::from_code("zh_cn"), Some(Language::Chinese));
        assert_eq!(Language::from_code("zh-Hant"), Some(Language::Chinese));
        assert_eq!(Language::from_code(" en-US "), Some(Language::English));
        assert_eq!(Language::from_code("fr"), None);
        assert_eq!(Language::from_code(""), None);
    }

    #[test]
    fn every_language_has_a_code_and_a_label() {
        for language in Language::ALL {
            assert_eq!(Language::from_code(language.code()), Some(language));
            assert!(!language.label().is_empty());
            assert!(!table(language).is_empty(), "{}", language.code());
        }
    }

    #[test]
    fn switching_takes_effect_for_t() {
        let _guard = TEST_LOCK.lock().unwrap();
        let before = language();
        set(Language::English);
        assert_eq!(t("app.title"), "DST Session Manager");
        set(Language::Chinese);
        assert_eq!(t("app.title"), "DST 会话管理器");
        set(before);
    }
}
