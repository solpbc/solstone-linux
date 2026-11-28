// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Platform file metadata on Linux: timestamps, extension/MIME guess, and safe
//! extended attribute reads (`user.xdg.origin.url` and `user.xdg.referrer.url`)
//! without `unsafe` code.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use rustix::fs::getxattr;
use rustix::io::Errno;

use super::types::PlatformMetadata;

const XDG_ORIGIN_URL: &str = "user.xdg.origin.url";
const XDG_REFERRER_URL: &str = "user.xdg.referrer.url";
const MAX_XATTR_BYTES: usize = 2048;

/// Read a UTF-8 extended attribute on Linux safely through `rustix`.
fn read_xattr_utf8(path: &Path, name: &str) -> Option<String> {
    let mut size = 512usize;
    loop {
        let mut buf = vec![0u8; size];
        match getxattr(path, name, &mut buf) {
            Ok(written) => {
                buf.truncate(written);
                let text = String::from_utf8(buf).ok()?;
                let trimmed = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
                return (!trimmed.is_empty() && !trimmed.chars().any(char::is_control))
                    .then(|| trimmed.to_owned());
            }
            Err(Errno::RANGE) if size < MAX_XATTR_BYTES => {
                size = (size * 2).min(MAX_XATTR_BYTES);
            }
            Err(_) => return None,
        }
    }
}

/// Read the `user.xdg.origin.url` extended attribute written by Linux browsers
/// and downloaders.
#[must_use]
pub fn read_xdg_origin_url(path: &Path) -> Option<String> {
    read_xattr_utf8(path, XDG_ORIGIN_URL)
}

/// Read the `user.xdg.referrer.url` extended attribute if present.
#[must_use]
pub fn read_xdg_referrer_url(path: &Path) -> Option<String> {
    read_xattr_utf8(path, XDG_REFERRER_URL)
}

/// Extract a clean host domain (dropping a leading `www.`) from an origin URL.
#[must_use]
pub fn origin_domain(origin_url: &str) -> Option<String> {
    let trimmed = origin_url.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host_raw = if host_port.starts_with('[') {
        let end = host_port.find(']')?;
        &host_port[..=end]
    } else {
        host_port.split(':').next()?
    };
    let host = host_raw.trim().to_ascii_lowercase();
    if host.is_empty() || host.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let stripped = host.strip_prefix("www.").unwrap_or(&host);
    (!stripped.is_empty()).then(|| stripped.to_owned())
}

/// Lowercase file extension without the leading dot.
#[must_use]
pub fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|ext| !ext.is_empty() && ext.len() <= 64 && !ext.chars().any(char::is_control))
}

/// Best-effort MIME type from file extension for deterministic activity lines.
#[must_use]
pub fn guess_mime(extension: Option<&str>) -> Option<String> {
    let mime = match extension? {
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "odt" => "application/vnd.oasis.opendocument.text",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        "html" | "htm" => "text/html",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "csv" => "text/csv",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "jxl" => "image/jxl",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "iso" => "application/x-iso9660-image",
        "deb" => "application/vnd.debian.binary-package",
        "rpm" => "application/x-rpm",
        "exe" => "application/vnd.microsoft.portable-executable",
        "json" => "application/json",
        _ => return None,
    };
    Some(mime.to_owned())
}

pub(crate) fn system_time_ms(time: std::time::SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(err) => -i64::try_from(err.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// Collect platform metadata for one local file.
#[must_use]
pub fn collect_metadata(
    path: &Path,
    attrs_path: &Path,
    fs_meta: &fs::Metadata,
) -> PlatformMetadata {
    let modified_ms = fs_meta.modified().map(system_time_ms).unwrap_or(0);
    let created_ms = fs_meta.created().ok().map(system_time_ms);
    let extension = extension_lower(path);
    let mime = guess_mime(extension.as_deref());
    let origin_url = read_xdg_origin_url(attrs_path);
    let referrer_url = read_xdg_referrer_url(attrs_path);
    let origin_domain = origin_url.as_deref().and_then(origin_domain);
    PlatformMetadata {
        size_bytes: fs_meta.len(),
        modified_ms,
        created_ms,
        extension,
        mime,
        origin_url,
        referrer_url,
        origin_domain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_domain_strips_www_and_ignores_invalid_urls() {
        assert_eq!(
            origin_domain("https://www.mozilla.org/en-US/firefox/download/"),
            Some("mozilla.org".into())
        );
        assert_eq!(
            origin_domain("https://app.docusign.net/documents/123?token=abc"),
            Some("app.docusign.net".into())
        );
        assert_eq!(origin_domain("not a url"), None);
    }

    #[test]
    fn xdg_origin_url_round_trips_when_filesystem_supports_xattrs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("installer.deb");
        fs::write(&file, b"binary").unwrap();
        let url = "https://download.mozilla.org/?product=firefox";
        // tmpfs or restricted mount may return EOPNOTSUPP; only assert readback
        // when setxattr succeeded.
        if rustix::fs::setxattr(
            &file,
            XDG_ORIGIN_URL,
            url.as_bytes(),
            rustix::fs::XattrFlags::empty(),
        )
        .is_ok()
        {
            assert_eq!(read_xdg_origin_url(&file).as_deref(), Some(url));
            let meta = collect_metadata(&file, &file, &fs::metadata(&file).unwrap());
            assert_eq!(meta.origin_domain.as_deref(), Some("download.mozilla.org"));
        }
    }
}
