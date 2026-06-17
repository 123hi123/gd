use crate::i18n::Lang;
use anyhow::{bail, Result};
use gd_core::db::KeyStore;

/// A tunable preference. `allowed[0]` doubles as the canonical default for
/// every setting except `language`, whose default tracks the locale (see
/// [`Setting::effective_default`]).
pub struct Setting {
    pub key: &'static str,
    pub allowed: &'static [&'static str],
    pub default: &'static str,
    pub help_en: &'static str,
    pub help_zh: &'static str,
}

impl Setting {
    /// One-line description in the active language.
    pub fn help(&self, lang: Lang) -> &'static str {
        lang.pick(self.help_en, self.help_zh)
    }

    /// The value shown as "… (default)" when nothing is stored. `language`
    /// resolves through the environment so it matches what the UI actually uses.
    pub fn effective_default(&self) -> String {
        if self.key == "language" {
            Lang::resolve(None).code().to_string()
        } else {
            self.default.to_string()
        }
    }
}

/// Known settings, in display order.
pub const KNOWN: &[Setting] = &[
    Setting {
        key: "layout",
        allowed: &["inline", "centered", "traditional"],
        default: "inline",
        help_en: "picker layout — inline: compact block at the prompt, query and \
                  best match on the line your cursor is on, list flowing down (eye \
                  stays put); centered: full-screen, best in the middle, fanning \
                  out; traditional: full-screen, best on top",
        help_zh: "選單排版 — inline：貼著游標的精簡區塊，query 與最佳結果就在你打字那行、\
                  清單向下排（眼睛不用移動）；centered：全螢幕、最佳置中向外擴散；\
                  traditional：全螢幕、最佳在最上向下排列",
    },
    Setting {
        key: "language",
        allowed: &["en", "中文"],
        default: "en",
        help_en: "language for the picker and this config screen",
        help_zh: "目錄選單與這個設定畫面的語言",
    },
];

pub fn lookup(key: &str) -> Option<&'static Setting> {
    KNOWN.iter().find(|s| s.key == key)
}

fn known_keys() -> String {
    KNOWN.iter().map(|s| s.key).collect::<Vec<_>>().join(", ")
}

pub fn run(store: &mut KeyStore, args: &[String]) -> Result<()> {
    let lang = Lang::resolve(store.get_setting("language").as_deref());
    match args {
        // No arguments → the interactive editor when we have a terminal,
        // otherwise the plain text dump (pipes, scripts, no /dev/tty).
        [] => {
            if crate::tui::config::is_interactive() {
                crate::tui::config::edit(store)
            } else {
                show_all(store, lang);
                Ok(())
            }
        }
        [key] => show_one(store, key, lang),
        [key, value] => set(store, key, value),
        _ => bail!("usage: gd config [<key> [<value>]]"),
    }
}

fn show_all(store: &KeyStore, lang: Lang) {
    eprintln!("gd config:");
    for s in KNOWN {
        let (value, tag) = match store.get_setting(s.key) {
            Some(v) => (v, ""),
            None => (s.effective_default(), lang.pick(" (default)", "（預設）")),
        };
        eprintln!("  {} = {value}{tag}", s.key);
        eprintln!("      {}", s.help(lang));
        eprintln!("      {}: {}", lang.pick("values", "可選值"), s.allowed.join(" | "));
    }
    eprintln!();
    eprintln!(
        "{}",
        lang.pick(
            "set with:  gd config <key> <value>   e.g. gd config layout traditional",
            "設定方式：gd config <key> <value>   例如 gd config layout traditional",
        )
    );
}

fn show_one(store: &KeyStore, key: &str, lang: Lang) -> Result<()> {
    let Some(s) = lookup(key) else {
        bail!("unknown config key '{key}' (known: {})", known_keys());
    };
    match store.get_setting(s.key) {
        Some(v) => eprintln!("{} = {v}", s.key),
        None => eprintln!(
            "{} = {} {}",
            s.key,
            s.effective_default(),
            lang.pick("(default)", "（預設）")
        ),
    }
    Ok(())
}

fn set(store: &mut KeyStore, key: &str, value: &str) -> Result<()> {
    let Some(s) = lookup(key) else {
        bail!("unknown config key '{key}' (known: {})", known_keys());
    };
    if !s.allowed.contains(&value) {
        bail!(
            "invalid value '{value}' for {} (values: {})",
            s.key,
            s.allowed.join(", ")
        );
    }
    store.set_setting(s.key, value)?;
    store.save()?;
    eprintln!("{} = {value}", s.key);
    Ok(())
}
