// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! How the app tells a screenshot from any other image: by what the desktop's
//! own screenshot tool leaves behind, never by the folder or the file's name.
//!
//! - GNOME Shell (and the older GNOME Screenshot app) writes `Software` =
//!   `gnome-screenshot` into every PNG it saves.
//! - KDE Spectacle writes nothing into the file, and records each image it saves
//!   in the desktop's recent-files list (`recently-used.xbel`) under its own
//!   application name, with the moment it saved it.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The value GNOME's screenshot code writes under the PNG `Software` key.
const GNOME_SOFTWARE: &str = "gnome-screenshot";

/// Application names a screenshot tool records an image it saved under.
const SCREENSHOT_APPS: [&str; 2] = ["org.kde.spectacle", "spectacle"];

/// How far into a PNG the text chunks are looked for. GNOME writes them before
/// the image data, so they sit in the first few hundred bytes.
const PNG_HEAD_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Proof {
    /// The file carries the GNOME screenshot mark.
    Gnome,
    /// The desktop's recent-files list says a screenshot tool saved it, at this moment.
    Recent { saved_ms: i64 },
}

/// The `Software` text of a PNG, read from its chunks before the image data.
pub(crate) fn png_software(head: &[u8]) -> Option<String> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let mut rest = head.strip_prefix(SIGNATURE)?;
    while rest.len() >= 8 {
        let length = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let kind = &rest[4..8];
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        let body = rest.get(8..8 + length)?;
        match kind {
            b"tEXt" => {
                if let Some(value) = body.strip_prefix(b"Software\0") {
                    return Some(String::from_utf8_lossy(value).into_owned());
                }
            }
            b"iTXt" => {
                // keyword \0 compression-flag compression-method lang \0 translated \0 text
                if let Some(after) = body.strip_prefix(b"Software\0")
                    && after.first() == Some(&0)
                {
                    let mut parts = after.get(2..)?.splitn(3, |byte| *byte == 0);
                    let _lang = parts.next()?;
                    let _translated = parts.next()?;
                    return Some(String::from_utf8_lossy(parts.next()?).into_owned());
                }
            }
            _ => {}
        }
        rest = rest.get(12 + length..)?;
    }
    None
}

/// Whether a PNG on disk carries the GNOME screenshot mark.
pub(crate) fn file_has_gnome_mark(path: &Path) -> std::io::Result<bool> {
    let file = std::fs::File::open(path)?;
    let mut head = Vec::with_capacity(4096);
    file.take(PNG_HEAD_LIMIT as u64).read_to_end(&mut head)?;
    Ok(png_software(&head).as_deref() == Some(GNOME_SOFTWARE))
}

/// The images the desktop's recent-files list says a screenshot tool saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Recents {
    saved: HashMap<PathBuf, i64>,
}

impl Recents {
    pub(crate) fn parse(text: &str) -> Self {
        let mut saved = HashMap::new();
        let Ok(document) = roxmltree::Document::parse(text) else {
            return Self { saved };
        };
        for bookmark in document
            .descendants()
            .filter(|node| node.has_tag_name("bookmark"))
        {
            let by_tool = bookmark.descendants().any(|node| {
                node.tag_name().name() == "application"
                    && node
                        .attribute("name")
                        .is_some_and(|name| SCREENSHOT_APPS.contains(&name))
            });
            if !by_tool {
                continue;
            }
            let Some(path) = bookmark.attribute("href").and_then(file_uri_path) else {
                continue;
            };
            let Some(added) = bookmark
                .attribute("added")
                .and_then(|added| chrono::DateTime::parse_from_rfc3339(added).ok())
            else {
                continue;
            };
            saved.insert(path, added.timestamp_millis());
        }
        Self { saved }
    }

    pub(crate) fn saved_ms(&self, path: &Path) -> Option<i64> {
        self.saved.get(path).copied()
    }
}

/// The local path a `file://` URI names, as the desktop writes them.
pub(crate) fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// What proves this file is a screenshot, if anything does.
pub(crate) fn prove(
    path: &Path,
    extension: &str,
    recents: &Recents,
) -> std::io::Result<Option<Proof>> {
    if let Some(saved_ms) = recents.saved_ms(path) {
        return Ok(Some(Proof::Recent { saved_ms }));
    }
    if extension == "png" && file_has_gnome_mark(path)? {
        return Ok(Some(Proof::Gnome));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GNOME_50: &[u8] = include_bytes!("../fixtures/screenshot-gnome-50.png");
    const SPECTACLE_6: &[u8] = include_bytes!("../fixtures/screenshot-spectacle-6.png");
    const SPECTACLE_RECENTS: &str = include_str!("../fixtures/recently-used-spectacle.xbel");

    #[test]
    fn a_gnome_shell_screenshot_carries_its_mark() {
        assert_eq!(png_software(GNOME_50).as_deref(), Some(GNOME_SOFTWARE));
    }

    #[test]
    fn a_spectacle_png_carries_nothing_in_the_file() {
        assert_eq!(png_software(SPECTACLE_6), None);
    }

    #[test]
    fn a_photo_or_a_png_from_elsewhere_is_not_a_screenshot() {
        let mut other = GNOME_50.to_vec();
        let at = other
            .windows(16)
            .position(|window| window == b"gnome-screenshot")
            .unwrap();
        other[at..at + 16].copy_from_slice(b"GIMP 3.0.4\0\0\0\0\0\0");
        assert_ne!(png_software(&other).as_deref(), Some(GNOME_SOFTWARE));
        assert_eq!(png_software(b"\xff\xd8\xff\xe0 a jpeg"), None);
    }

    #[test]
    fn spectacles_record_names_each_image_it_saved_and_when() {
        let recents = Recents::parse(SPECTACLE_RECENTS);
        let saved = recents
            .saved_ms(Path::new(
                "/home/owner/Pictures/Screenshots/Screenshot_20261113_024732.png",
            ))
            .unwrap();
        assert_eq!(
            chrono::DateTime::from_timestamp_millis(saved)
                .unwrap()
                .to_rfc3339(),
            "2026-11-13T09:47:32.224+00:00"
        );
        assert!(
            recents
                .saved_ms(Path::new(
                    "/home/owner/Pictures/Screenshots/Screenshot_20261113_0546.avif"
                ))
                .is_some()
        );
    }

    #[test]
    fn an_image_another_app_opened_is_not_a_screenshot() {
        let text = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks">
  <bookmark href="file:///home/owner/Pictures/IMG_2041.jpg" added="2026-11-01T10:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="org.gnome.Loupe" exec="loupe %u" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
  <bookmark href="file:///home/owner/Pictures/Screenshots/a%20b.png" added="2026-11-02T10:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="org.kde.spectacle" exec="spectacle %f" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
</xbel>"#;
        let recents = Recents::parse(text);
        assert_eq!(
            recents.saved_ms(Path::new("/home/owner/Pictures/IMG_2041.jpg")),
            None
        );
        assert!(
            recents
                .saved_ms(Path::new("/home/owner/Pictures/Screenshots/a b.png"))
                .is_some()
        );
    }

    #[test]
    fn a_file_uri_decodes_to_its_path() {
        assert_eq!(
            file_uri_path("file:///home/owner/Captures%20d%27%C3%A9cran/a%25b.png"),
            Some(PathBuf::from("/home/owner/Captures d'écran/a%b.png"))
        );
        assert_eq!(file_uri_path("https://example.com/a.png"), None);
        assert_eq!(file_uri_path("file:///bad%zz"), None);
    }

    #[test]
    fn a_broken_record_proves_nothing() {
        assert_eq!(Recents::parse("<xbel"), Recents::default());
    }
}
