// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Where this computer's screenshots save: the owner's Pictures folder, from the
//! desktop's own user-dirs record, and in it the Screenshots folder under the name
//! the desktop gives it in the owner's language.
//!
//! GNOME Shell saves to `Pictures/<Screenshots>`, translated through its own
//! catalog. KDE Spectacle saves to the folder set in its settings, or to
//! `Pictures/<Screenshots>`, and writes the translated name it used into its
//! settings file. A folder that isn't there yet (no screenshot saved so far) is
//! still the folder: the desktop makes it on the first save.

use std::path::{Path, PathBuf};

/// The folder's name before translation.
const SCREENSHOTS: &str = "Screenshots";

/// Where to find it, read from the owner's own files.
pub(crate) struct Places {
    pub home: PathBuf,
    pub config: PathBuf,
    /// `XDG_DATA_HOME`, where the desktop keeps its recent-files list.
    pub data: PathBuf,
    /// Directories holding `<lang>/LC_MESSAGES/<domain>.mo`.
    pub locale_dirs: Vec<PathBuf>,
    /// The owner's languages, most preferred first.
    pub languages: Vec<String>,
}

impl Places {
    pub(crate) fn from_env(home: PathBuf) -> Self {
        let from_env = |name: &str, fallback: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };
        let config = from_env("XDG_CONFIG_HOME", ".config");
        let data = from_env("XDG_DATA_HOME", ".local/share");
        Self {
            home,
            config,
            data,
            locale_dirs: vec![
                PathBuf::from("/usr/share/locale"),
                PathBuf::from("/usr/share/locale-langpack"),
            ],
            languages: languages_from_env(|name| std::env::var(name).ok()),
        }
    }
}

/// The Screenshots folder this computer saves to, or `None` when there is no
/// Pictures folder to look in.
pub(crate) fn resolve(places: &Places) -> Option<PathBuf> {
    if let Some(chosen) = spectacle_location(&places.config) {
        return Some(chosen);
    }
    let pictures = pictures_dir(places)?;
    let mut names = Vec::new();
    if let Some(name) = spectacle_translated_name(&places.config) {
        names.push(name);
    }
    if let Some(name) = translated(places, "gnome-shell", SCREENSHOTS) {
        names.push(name);
    }
    names.push(SCREENSHOTS.to_owned());
    // The one already there wins. Before any is, the desktop's own name.
    names
        .iter()
        .map(|name| pictures.join(name))
        .find(|path| path.is_dir())
        .or_else(|| names.first().map(|name| pictures.join(name)))
}

/// `XDG_PICTURES_DIR` from `user-dirs.dirs`, or `~/Pictures`. A Pictures folder
/// set to the home folder itself means none, as the spec says.
fn pictures_dir(places: &Places) -> Option<PathBuf> {
    let text = std::fs::read_to_string(places.config.join("user-dirs.dirs")).ok();
    let configured = text.as_deref().and_then(|text| {
        text.lines().find_map(|line| {
            let value = line.trim().strip_prefix("XDG_PICTURES_DIR=")?;
            let value = value.trim().trim_matches('"');
            if let Some(rest) = value.strip_prefix("$HOME") {
                Some(places.home.join(rest.trim_start_matches('/')))
            } else if value.starts_with('/') {
                Some(PathBuf::from(value))
            } else {
                None
            }
        })
    });
    let pictures = configured.unwrap_or_else(|| places.home.join("Pictures"));
    (pictures != places.home && pictures.is_dir()).then_some(pictures)
}

fn spectaclerc(config: &Path) -> Option<String> {
    std::fs::read_to_string(config.join("spectaclerc")).ok()
}

/// A value from one group of a KDE settings file.
fn ini_value(text: &str, group: &str, key: &str) -> Option<String> {
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_group = name == group;
            continue;
        }
        if in_group
            && let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            let value = value.trim();
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }
    None
}

/// The save folder the owner set in Spectacle's settings.
fn spectacle_location(config: &Path) -> Option<PathBuf> {
    let text = spectaclerc(config)?;
    let value = ini_value(&text, "ImageSave", "imageSaveLocation")?;
    if value.starts_with('/') {
        return Some(PathBuf::from(value));
    }
    super::proof::file_uri_path(&value)
}

/// The name Spectacle gave the Screenshots folder in the owner's language.
fn spectacle_translated_name(config: &Path) -> Option<String> {
    let text = spectaclerc(config)?;
    ini_value(&text, "ImageSave", "translatedScreenshotsFolder").filter(|name| !name.contains('/'))
}

/// The owner's languages in gettext's order: `LANGUAGE` (unless the locale is C),
/// then `LC_ALL`, `LC_MESSAGES`, `LANG`.
fn languages_from_env(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()));
    let plain = |value: &str| matches!(value, "C" | "POSIX") || value.starts_with("C.");
    let mut languages = Vec::new();
    if let Some(locale) = locale.as_deref()
        && plain(locale)
    {
        return languages;
    }
    if let Some(list) = var("LANGUAGE") {
        languages.extend(
            list.split(':')
                .filter(|item| !item.is_empty())
                .map(str::to_owned),
        );
    }
    if let Some(locale) = locale {
        languages.push(locale);
    }
    languages
}

/// The forms gettext tries for one locale name, most specific first.
fn variants(locale: &str) -> Vec<String> {
    let (base, modifier) = match locale.split_once('@') {
        Some((base, modifier)) => (base, Some(modifier)),
        None => (locale, None),
    };
    let base = base.split('.').next().unwrap_or(base);
    let language = base.split('_').next().unwrap_or(base);
    let mut forms = Vec::new();
    for name in [base, language] {
        if let Some(modifier) = modifier {
            forms.push(format!("{name}@{modifier}"));
        }
        forms.push(name.to_owned());
    }
    forms.dedup();
    forms
}

/// `msgid` in `domain`'s catalog for the owner's first language that has one.
fn translated(places: &Places, domain: &str, msgid: &str) -> Option<String> {
    for language in &places.languages {
        for form in variants(language) {
            if form == "en" || form.starts_with("en_") {
                return None;
            }
            for dir in &places.locale_dirs {
                let path = dir
                    .join(&form)
                    .join("LC_MESSAGES")
                    .join(format!("{domain}.mo"));
                if let Ok(bytes) = std::fs::read(&path) {
                    return mo_lookup(&bytes, msgid).filter(|name| !name.contains('/'));
                }
            }
        }
    }
    None
}

/// Look `msgid` up in a gettext `.mo` catalog, without a context.
fn mo_lookup(bytes: &[u8], msgid: &str) -> Option<String> {
    let word = |at: usize, big: bool| -> Option<usize> {
        let raw: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        } as usize)
    };
    let big = match bytes.get(..4)? {
        [0xde, 0x12, 0x04, 0x95] => false,
        [0x95, 0x04, 0x12, 0xde] => true,
        _ => return None,
    };
    let count = word(8, big)?;
    let originals = word(12, big)?;
    let translations = word(16, big)?;
    let entry = |table: usize, index: usize| -> Option<&[u8]> {
        let length = word(table + index * 8, big)?;
        let offset = word(table + index * 8 + 4, big)?;
        bytes.get(offset..offset.checked_add(length)?)
    };
    for index in 0..count {
        if entry(originals, index)? == msgid.as_bytes() {
            let text = entry(translations, index)?;
            let first = text.split(|byte| *byte == 0).next()?;
            return std::str::from_utf8(first)
                .ok()
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const CATALOG: &[u8] = include_bytes!("../fixtures/gnome-shell-de.mo");

    fn places(temp: &Path, languages: &[&str]) -> Places {
        let home = temp.join("home");
        fs::create_dir_all(home.join(".config")).unwrap();
        Places {
            config: home.join(".config"),
            data: home.join(".local/share"),
            home,
            locale_dirs: vec![temp.join("locale")],
            languages: languages.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    #[test]
    fn a_catalog_gives_the_folders_name_and_ignores_a_context() {
        assert_eq!(
            mo_lookup(CATALOG, "Screenshots").as_deref(),
            Some("Bildschirmfotos")
        );
        assert_eq!(mo_lookup(CATALOG, "Pictures"), None);
        assert_eq!(mo_lookup(b"not a catalog", "Screenshots"), None);
    }

    #[test]
    fn english_with_no_record_is_pictures_screenshots_before_it_exists() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path(), &["en_US.UTF-8"]);
        fs::create_dir_all(places.home.join("Pictures")).unwrap();
        assert_eq!(
            resolve(&places),
            Some(places.home.join("Pictures/Screenshots"))
        );
    }

    #[test]
    fn the_user_dirs_record_and_the_desktops_translation_decide() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path(), &["de_DE.UTF-8"]);
        fs::create_dir_all(places.home.join("Bilder")).unwrap();
        fs::write(
            places.config.join("user-dirs.dirs"),
            "# written by xdg-user-dirs-update\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\n",
        )
        .unwrap();
        let catalog = temp.path().join("locale/de/LC_MESSAGES");
        fs::create_dir_all(&catalog).unwrap();
        fs::write(catalog.join("gnome-shell.mo"), CATALOG).unwrap();
        assert_eq!(
            resolve(&places),
            Some(places.home.join("Bilder/Bildschirmfotos"))
        );
        // An English folder already there wins over a translated one not made yet.
        fs::create_dir_all(places.home.join("Bilder/Screenshots")).unwrap();
        assert_eq!(
            resolve(&places),
            Some(places.home.join("Bilder/Screenshots"))
        );
    }

    #[test]
    fn spectacles_settings_name_the_folder() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path(), &["fr_FR.UTF-8"]);
        fs::create_dir_all(places.home.join("Pictures")).unwrap();
        fs::write(
            places.config.join("spectaclerc"),
            "[ImageSave]\ntranslatedScreenshotsFolder=Captures d'écran\n",
        )
        .unwrap();
        assert_eq!(
            resolve(&places),
            Some(places.home.join("Pictures/Captures d'écran"))
        );
        fs::write(
            places.config.join("spectaclerc"),
            "[General]\nimageSaveLocation=file:///elsewhere\n[ImageSave]\nimageSaveLocation=file:///home/owner/Desktop/shots\n",
        )
        .unwrap();
        assert_eq!(
            resolve(&places),
            Some(PathBuf::from("/home/owner/Desktop/shots"))
        );
    }

    #[test]
    fn no_pictures_folder_resolves_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path(), &[]);
        assert_eq!(resolve(&places), None);
        fs::write(
            places.config.join("user-dirs.dirs"),
            "XDG_PICTURES_DIR=\"$HOME/\"\n",
        )
        .unwrap();
        assert_eq!(resolve(&places), None);
    }

    #[test]
    fn languages_follow_gettexts_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            languages_from_env(env(&[("LANGUAGE", "pt_BR:pt"), ("LANG", "pt_BR.UTF-8")])),
            vec!["pt_BR", "pt", "pt_BR.UTF-8"]
        );
        assert!(languages_from_env(env(&[("LANGUAGE", "de"), ("LC_ALL", "C.UTF-8")])).is_empty());
        assert_eq!(
            variants("sr_RS.UTF-8@latin"),
            vec!["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
    }
}
