//! Language of the messages: French or English. The interface follows the
//! language of macOS; the command line speaks English (main.rs).
//!
//! Texts are written in both languages where they are used:
//! `tr!("Installation de {file}", "Installing {file}")`, like `format!`.

use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Fr,
    En,
}

/// 0: not chosen yet (macOS's will be used), 1: French, 2: English.
static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    LANG.store(if lang == Lang::Fr { 1 } else { 2 }, Ordering::Relaxed);
}

pub fn get() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Fr,
        2 => Lang::En,
        _ => {
            let lang = system();
            set(lang);
            lang
        }
    }
}

/// French if the first language of macOS is French, English otherwise.
pub fn system() -> Lang {
    mac_languages().first().map_or(Lang::En, |code| of(code))
}

/// The language of a code such as `fr-FR` or `en-US`: French or, for all
/// the others, English.
pub fn of(code: &str) -> Lang {
    if code == "fr" || code.starts_with("fr-") {
        Lang::Fr
    } else {
        Lang::En
    }
}

/// Preferred languages of macOS, in order (`fr-FR`, `en-FR`…).
pub fn mac_languages() -> Vec<String> {
    let Ok(out) = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    else {
        return vec![];
    };
    // Output in text plist format: ( "fr-FR", "en-FR" ).
    String::from_utf8_lossy(&out.stdout)
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The text in the chosen language, formatted as by `format!`: variables
/// named in the text, or values after the two texts.
#[macro_export]
macro_rules! tr {
    ($fr:literal, $en:literal $(,)?) => {
        if $crate::i18n::get() == $crate::i18n::Lang::Fr {
            format!($fr)
        } else {
            format!($en)
        }
    };
    ($fr:literal, $en:literal, $($arg:expr),+ $(,)?) => {
        if $crate::i18n::get() == $crate::i18n::Lang::Fr {
            format!($fr, $($arg),+)
        } else {
            format!($en, $($arg),+)
        }
    };
}
