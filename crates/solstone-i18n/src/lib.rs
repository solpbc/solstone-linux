// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Catalog presentation shared by the desktop app and terminal helper.

use fluent_langneg::{NegotiationStrategy, negotiate_languages};
use std::sync::OnceLock;
use unic_langid::LanguageIdentifier;

#[allow(dead_code, clippy::all)]
#[rustfmt::skip]
mod generated;
mod locales;

use locales::SUPPORTED_LOCALES;

pub use generated::Value;

const ENGLISH: &str = "en-US";
static LOCALE: OnceLock<String> = OnceLock::new();

fn parse_locale(value: &str) -> Option<LanguageIdentifier> {
    let base = value.split(['.', '@']).next()?.trim();
    base.replace('_', "-").parse().ok()
}

fn preferred_languages(
    language: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
) -> Vec<LanguageIdentifier> {
    let base = [lc_all, lc_messages, lang]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .unwrap_or("C");
    let locale_name = base.split(['.', '@']).next().unwrap_or(base);
    if matches!(locale_name, "C" | "POSIX") {
        return Vec::new();
    }
    language
        .into_iter()
        .flat_map(|list| list.split(':'))
        .chain(std::iter::once(base))
        .filter_map(parse_locale)
        .collect()
}

fn negotiate(preferred: &[LanguageIdentifier], supported: &[&str]) -> String {
    let available: Vec<LanguageIdentifier> = supported
        .iter()
        .map(|tag| tag.parse().expect("validated catalog locale"))
        .collect();
    negotiate_languages(preferred, &available, None, NegotiationStrategy::Lookup)
        .first()
        .map_or_else(|| ENGLISH.to_owned(), ToString::to_string)
}

pub fn locale() -> &'static str {
    LOCALE.get_or_init(|| {
        let language = std::env::var("LANGUAGE").ok();
        let lc_all = std::env::var("LC_ALL").ok();
        let lc_messages = std::env::var("LC_MESSAGES").ok();
        let lang = std::env::var("LANG").ok();
        let preferred = preferred_languages(
            language.as_deref(),
            lc_all.as_deref(),
            lc_messages.as_deref(),
            lang.as_deref(),
        );
        negotiate(&preferred, SUPPORTED_LOCALES)
    })
}

pub fn right_to_left() -> bool {
    locale()
        .parse::<LanguageIdentifier>()
        .is_ok_and(|lang| lang.character_direction() == unic_langid::CharacterDirection::RTL)
}

pub fn render(locale: &str, id: &str, args: &[(&str, Value<'_>)]) -> String {
    generated::render_dotted_with_options(locale, id, args, locale != ENGLISH)
        .or_else(|_| generated::render_dotted_with_options(ENGLISH, id, args, false))
        .unwrap_or_else(|error| {
            tracing::error!(message_id = id, ?error, "catalog render failed");
            id.to_owned()
        })
}

/// Render a message without dynamic arguments once per call site.
#[macro_export]
macro_rules! text {
    ($id:literal) => {{
        static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        TEXT.get_or_init(|| $crate::render($crate::locale(), $id, &[]))
            .as_str()
    }};
}

/// Render a message with named display arguments for the current process locale.
#[macro_export]
macro_rules! message {
    ($id:literal, $($arg:literal => $value:expr),+ $(,)?) => {
        $crate::render($crate::locale(), $id, &[
            $(($arg, $crate::Value::String(&$value.to_string()))),+
        ])
    };
}

/// The portable profile has a bounded selector. Keep large counts in the
/// high range with the same last six decimal places for plural selection.
pub fn cardinal(id: &str, count: i64, display_args: &[(&str, Value<'_>)]) -> String {
    let within_profile = if count > i64::from(i32::MAX) {
        1_000_000 + count % 1_000_000
    } else {
        count
    };
    let mut args = Vec::with_capacity(display_args.len() + 1);
    args.push(("count", Value::Cardinal(within_profile)));
    args.extend_from_slice(display_args);
    render(locale(), id, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_posix_message_precedence_and_gettext_language_order() {
        let supported = &["en-US", "es-419", "fr"];
        for (language, all, messages, lang, expected) in [
            (Some("de:fr"), None, None, Some("es_MX.UTF-8"), "fr"),
            (None, Some("es_MX.UTF-8"), Some("fr"), None, "es-419"),
            (None, Some(""), Some("es_MX"), Some("fr"), "es-419"),
            (None, None, None, Some("es_MX.UTF-8"), "es-419"),
            (Some("fr"), Some("C"), None, Some("es_MX"), "en-US"),
            (Some("fr"), Some("POSIX"), None, None, "en-US"),
            (None, None, None, Some("de_DE"), "en-US"),
        ] {
            let preferences = preferred_languages(language, all, messages, lang);
            assert_eq!(negotiate(&preferences, supported), expected);
            assert_eq!(negotiate(&preferences, &["en-US"]), "en-US");
        }
    }
}
