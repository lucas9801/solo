//! SOLO: the language of Solo's interface, English or Chinese.
//!
//! The `ui_language` setting picks it ([`set_language`]); "system" follows the
//! system UI language. `SOLO_LANGUAGE=zh|en` overrides both.
//!
//! Solo's own strings choose their text with [`tr`]. Zed's strings stay in
//! English in the code and are translated where they're shown (labels, menus,
//! tooltips) with [`translate`], from `assets/locales/zh-CN.json`.

use std::{
    collections::HashMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

/// The interface language the `ui_language` setting asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    System,
    English,
    Chinese,
}

const SYSTEM: u8 = 0;
const ENGLISH: u8 = 1;
const CHINESE: u8 = 2;

static LANGUAGE: AtomicU8 = AtomicU8::new(SYSTEM);

/// Sets the interface language, from the `ui_language` setting.
pub fn set_language(language: Language) {
    let value = match language {
        Language::System => SYSTEM,
        Language::English => ENGLISH,
        Language::Chinese => CHINESE,
    };
    LANGUAGE.store(value, Ordering::Relaxed);
}

fn system_is_chinese() -> bool {
    static CHINESE: OnceLock<bool> = OnceLock::new();
    *CHINESE.get_or_init(|| {
        sys_locale::get_locale()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .starts_with("zh")
    })
}

fn overridden() -> Option<bool> {
    static OVERRIDE: OnceLock<Option<bool>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        let language = std::env::var("SOLO_LANGUAGE").ok()?.to_ascii_lowercase();
        Some(language.starts_with("zh"))
    })
}

/// Whether Solo's interface is shown in Chinese.
pub fn is_chinese() -> bool {
    if let Some(chinese) = overridden() {
        return chinese;
    }
    match LANGUAGE.load(Ordering::Relaxed) {
        ENGLISH => false,
        CHINESE => true,
        _ => system_is_chinese(),
    }
}

/// `zh` when Solo's interface is shown in Chinese, `en` otherwise.
pub fn tr(en: &'static str, zh: &'static str) -> &'static str {
    if is_chinese() { zh } else { en }
}

fn translations() -> &'static HashMap<String, String> {
    static TRANSLATIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
    TRANSLATIONS.get_or_init(|| {
        serde_json::from_str(include_str!("../../../assets/locales/zh-CN.json")).unwrap_or_else(
            |error| {
                log::error!("Invalid zh-CN.json: {error}");
                HashMap::default()
            },
        )
    })
}

/// The Chinese of the English interface text `text`, when the interface is
/// shown in Chinese and it's translated. Text ending in "…", "...", or ":"
/// uses the translation of the text before it.
pub fn translate(text: &str) -> Option<&'static str> {
    if !is_chinese() || text.is_empty() {
        return None;
    }
    translate_text(text)
}

fn translate_text(text: &str) -> Option<&'static str> {
    let translations = translations();
    if let Some(translation) = translations.get(text) {
        return Some(translation);
    }
    for suffix in ["…", "...", ":"] {
        if let Some(stem) = text.strip_suffix(suffix)
            && let Some(translation) = translations.get(stem)
        {
            return Some(cached(format!("{translation}{suffix}")));
        }
    }
    None
}

/// Keeps `text` for the rest of the run: translations with a suffix are
/// built once.
fn cached(text: String) -> &'static str {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(text) = cache.get(&text) {
        return text;
    }
    let leaked: &'static str = Box::leak(text.clone().into_boxed_str());
    cache.insert(text, leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translations_parse() {
        assert!(!translations().is_empty());
        assert_eq!(translate_text("Open Settings"), Some("打开设置"));
        assert_eq!(translate_text("Open Settings…"), Some("打开设置…"));
        assert_eq!(translate_text("not a translated string"), None);
    }
}
