//! SOLO: picking between the English and Chinese text of Solo's own UI
//! strings, by the system UI language. `SOLO_LANGUAGE=zh|en` overrides it.

use std::sync::OnceLock;

/// Whether Solo's own UI text is shown in Chinese.
pub fn is_chinese() -> bool {
    static CHINESE: OnceLock<bool> = OnceLock::new();
    *CHINESE.get_or_init(|| {
        let language = std::env::var("SOLO_LANGUAGE")
            .ok()
            .or_else(sys_locale::get_locale)
            .unwrap_or_default();
        language.to_ascii_lowercase().starts_with("zh")
    })
}

/// `zh` when Solo's UI text is shown in Chinese, `en` otherwise.
pub fn tr(en: &'static str, zh: &'static str) -> &'static str {
    if is_chinese() { zh } else { en }
}
