//! Two-language strings for the interactive surfaces — the `gd config` editor
//! and the directory picker. Scope is deliberately small: only the screens the
//! user actually drives are translated; every other command stays English.
//!
//! The active language is the stored `language` setting. When it is unset we
//! fall back to `$LANG`, so a fresh install already speaks the user's locale.

/// UI language. Stored in the DB as the `language` setting (`en` / `中文`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    /// Resolve the stored `language` value. `None` or anything unrecognised
    /// falls back to environment detection, so the default tracks the locale.
    pub fn resolve(stored: Option<&str>) -> Self {
        match stored {
            Some("中文" | "zh") => Lang::Zh,
            Some("en") => Lang::En,
            _ => Self::detect(),
        }
    }

    /// Best-effort locale sniff: a `zh*` locale picks Chinese, everything else
    /// (including an unset environment) picks English.
    fn detect() -> Self {
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        if locale.to_ascii_lowercase().starts_with("zh") {
            Lang::Zh
        } else {
            Lang::En
        }
    }

    /// Canonical value stored in the DB for this language.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "中文",
        }
    }

    /// Pick the English or Chinese variant of a string for this language.
    pub fn pick(self, en: &'static str, zh: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Zh => zh,
        }
    }
}
