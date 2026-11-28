// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Deterministic file sorting, partial-download / junk filtering, symlink
//! confinement, and folder overlap detection.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::types::{DeliveryMode, FileKind, FolderKinds, OverlapTarget, SupportedList};

const PARTIAL_SUFFIXES: [&str; 6] = [
    ".crdownload",
    ".part",
    ".download",
    ".opdownload",
    ".filepart",
    ".tmp",
];

const JUNK_NAMES: [&str; 4] = ["thumbs.db", "desktop.ini", "ehthumbs.db", ".ds_store"];

/// Whether a filename looks like an in-progress download or temporary save
/// that must wait until finished.
#[must_use]
pub fn is_partial_download(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    PARTIAL_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// Whether a directory entry is a hidden dot entry, OS metadata junk, or an
/// office temporary lock file that is always skipped.
#[must_use]
pub fn is_junk_entry(file_name: &str) -> bool {
    if file_name.starts_with('.') || file_name.starts_with("~$") || file_name.ends_with('~') {
        return true;
    }
    let lower = file_name.to_ascii_lowercase();
    JUNK_NAMES.contains(&lower.as_str())
}

/// Whether a symlink resolves inside `canonical_root`. Broken symlinks or
/// targets escaping the chosen folder return `false`.
#[must_use]
pub fn symlink_stays_inside(canonical_root: &Path, entry_path: &Path) -> bool {
    let Ok(target) = std::fs::canonicalize(entry_path) else {
        return false;
    };
    target.starts_with(canonical_root)
}

/// Whether a `.md` or `.txt` file crosses triage's document threshold
/// (`long_prose`: >= 5,000 words with >= 70% prose; `structured_prose`: >= 1,500
/// words with >= 70% prose and >= 2 heading levels) or exceeds the note size cap.
#[must_use]
pub fn classify_text_file(bytes: &[u8], supported: &SupportedList) -> FileKind {
    if bytes.len() as u64 > supported.max_note_bytes {
        return FileKind::Document;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return FileKind::Other;
    };
    let mut total_words = 0usize;
    let mut prose_lines = 0usize;
    let mut non_empty_lines = 0usize;
    let mut heading_levels: BTreeSet<usize> = BTreeSet::new();
    let mut in_fence = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            non_empty_lines += 1;
            continue;
        }
        non_empty_lines += 1;
        if in_fence {
            continue;
        }
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
            heading_levels.insert(hashes);
            let rest = line[hashes + 1..].trim();
            total_words += rest.split_whitespace().count();
            prose_lines += 1;
            continue;
        }
        let words = line.split_whitespace().count();
        total_words += words;
        let is_list_or_table = line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("|")
            || line.starts_with("> ");
        if !is_list_or_table || words >= 6 {
            prose_lines += 1;
        }
    }

    let prose_ok = non_empty_lines > 0 && prose_lines * 100 >= non_empty_lines * 70;
    let long_prose = prose_ok && total_words >= supported.note_long_words;
    let structured_prose = prose_ok
        && total_words >= supported.note_structured_words
        && heading_levels.len() >= supported.note_heading_levels;

    if long_prose || structured_prose {
        FileKind::Document
    } else {
        FileKind::Note
    }
}

// Journal-advertised transcript extensions overlap notes and ordinary files.
// Admit structured exports only when their conversation layout is present.
fn transcript_layout(ext: &str, bytes: Option<&[u8]>) -> bool {
    if matches!(ext, "srt" | "vtt") {
        return true;
    }
    use super::structured_exports::*;
    let Some(text) = bytes.and_then(decode_structured_utf8) else {
        return false;
    };
    match ext {
        "json" => {
            parse_slack_json(text).is_some()
                || parse_telegram_json(text).is_some()
                || parse_whisper_json(text).is_some()
                || parse_macwhisper_json(text).is_some()
                || parse_podcast_transcript_json(text).is_some()
        }
        "csv" => parse_fireflies_csv(text).is_some(),
        "tsv" => parse_whisper_tsv(text).is_some(),
        "html" => parse_telegram_html(text).is_some(),
        "md" | "markdown" | "txt" | "log" => {
            let mut speakers = BTreeSet::new();
            for line in text.lines().take(256) {
                if let Some((role, body)) = line.trim().split_once(':') {
                    let role = role.trim_matches('*').to_ascii_lowercase();
                    if !body.trim_matches('*').trim().is_empty()
                        && matches!(
                            role.as_str(),
                            "user"
                                | "assistant"
                                | "human"
                                | "system"
                                | "chatgpt"
                                | "claude"
                                | "gemini"
                        )
                    {
                        speakers.insert(role);
                    }
                }
            }
            speakers.len() >= 2
        }
        _ => false,
    }
}

/// Deterministic sorting table: assigns a file kind and decides whether file
/// bytes are sent (`Content`), only metadata goes (`MetadataOnly`), or the
/// file is ignored (`Ignore`).
#[must_use]
pub fn classify_file(
    extension: Option<&str>,
    size_bytes: u64,
    is_screenshot: bool,
    note_bytes: Option<&[u8]>,
    kinds: FolderKinds,
    supported: &SupportedList,
) -> (FileKind, DeliveryMode) {
    let ext = extension.map(str::to_ascii_lowercase);
    let ext_str = ext.as_deref().unwrap_or("");

    let is_image = supported.accepted_image_extensions.contains(ext_str)
        || supported.convertible_image_extensions.contains(ext_str);

    let file_kind = if is_screenshot && is_image {
        FileKind::Screenshot
    } else if supported.transcript_extensions.contains(ext_str)
        && transcript_layout(ext_str, note_bytes)
    {
        FileKind::Transcript
    } else if supported.note_extensions.contains(ext_str) {
        if size_bytes > supported.max_note_bytes {
            FileKind::Document
        } else if let Some(bytes) = note_bytes {
            classify_text_file(bytes, supported)
        } else {
            FileKind::Note
        }
    } else if supported.document_extensions.contains(ext_str) {
        FileKind::Document
    } else if is_image {
        FileKind::Photo
    } else {
        FileKind::Other
    };

    let within_size = size_bytes <= supported.max_content_bytes;
    let delivery = if file_kind == FileKind::Other {
        if kinds.other {
            DeliveryMode::MetadataOnly
        } else {
            DeliveryMode::Ignore
        }
    } else if kinds.enabled_for(file_kind) && within_size {
        DeliveryMode::Content
    } else if kinds.enabled_for(file_kind) || kinds.other {
        DeliveryMode::MetadataOnly
    } else {
        DeliveryMode::Ignore
    };

    (file_kind, delivery)
}

/// Whether two canonical paths overlap (equal, ancestor, or descendant).
#[must_use]
pub fn paths_overlap(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// Overlap check against Obsidian, screenshots, and already-added folders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlapOutcome {
    None,
    AlreadyAdded { id: String, name: String },
    Overlap(OverlapTarget),
}

#[must_use]
pub fn detect_overlap(
    candidate: &Path,
    obsidian: Option<(&Path, &str)>,
    screenshots: Option<(&Path, &str)>,
    existing_folders: &[(&str, &Path, &str)],
) -> OverlapOutcome {
    let canonical = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());

    for (id, path, name) in existing_folders {
        let existing = norm(path);
        if canonical == existing {
            return OverlapOutcome::AlreadyAdded {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            };
        }
        if paths_overlap(&canonical, &existing) {
            return OverlapOutcome::Overlap(OverlapTarget::Folder {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            });
        }
    }
    if let Some((path, name)) = obsidian
        && paths_overlap(&canonical, &norm(path))
    {
        return OverlapOutcome::Overlap(OverlapTarget::Obsidian {
            name: name.to_owned(),
        });
    }
    if let Some((path, name)) = screenshots
        && paths_overlap(&canonical, &norm(path))
    {
        return OverlapOutcome::Overlap(OverlapTarget::Screenshots {
            name: name.to_owned(),
        });
    }
    OverlapOutcome::None
}

/// Friendly folder label relative to `home` using ` › ` separators.
#[must_use]
pub fn folder_label(folder: &Path, home: &Path) -> String {
    let shown = folder.strip_prefix(home).unwrap_or(folder);
    let parts = shown
        .components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        folder.display().to_string()
    } else {
        parts.join(" › ")
    }
}

/// Short display name for a folder path (its final path component).
#[must_use]
pub fn folder_name(folder: &Path) -> String {
    let raw = folder
        .file_name()
        .unwrap_or(folder.as_os_str())
        .to_string_lossy();
    bounded_folder_name(&raw)
}

pub(super) fn bounded_folder_name(raw: &str) -> String {
    let mut name = String::new();
    for ch in raw.trim().chars().filter(|ch| !ch.is_control()) {
        if name.len() + ch.len_utf8() > 128 {
            break;
        }
        name.push(ch);
    }
    if name.trim().is_empty() {
        "folder".into()
    } else {
        name
    }
}

/// Canonicalize a candidate path, preserving it verbatim if canonicalize fails.
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partials_and_junk_are_filtered_deterministically() {
        assert!(is_partial_download("firefox.tar.xz.part"));
        assert!(is_partial_download("installer.exe.crdownload"));
        assert!(is_partial_download("movie.mp4.download"));
        assert!(!is_partial_download("report.pdf"));

        assert!(is_junk_entry(".DS_Store"));
        assert!(is_junk_entry(".git"));
        assert!(is_junk_entry("Thumbs.db"));
        assert!(is_junk_entry("~$contract.docx"));
        assert!(!is_junk_entry("todo.md"));
    }

    #[test]
    fn sorting_table_routes_each_kind_and_respects_64_mib_and_switches() {
        let supported = SupportedList::default();
        let all_on = FolderKinds::default();

        assert_eq!(
            classify_file(
                Some("md"),
                120,
                false,
                Some(b"short note"),
                all_on,
                &supported
            ),
            (FileKind::Note, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("pdf"), 400_000, false, None, all_on, &supported),
            (FileKind::Document, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("vtt"), 8_000, false, None, all_on, &supported),
            (FileKind::Transcript, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("png"), 50_000, true, None, all_on, &supported),
            (FileKind::Screenshot, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("png"), 50_000, false, None, all_on, &supported),
            (FileKind::Photo, DeliveryMode::Content)
        );
        // Unsupported / audio / video -> metadata only when `other` is on.
        assert_eq!(
            classify_file(Some("exe"), 58_000_000, false, None, all_on, &supported),
            (FileKind::Other, DeliveryMode::MetadataOnly)
        );
        assert_eq!(
            classify_file(Some("mp4"), 10_000_000, false, None, all_on, &supported),
            (FileKind::Other, DeliveryMode::MetadataOnly)
        );
        // Over 64 MiB -> metadata only even for supported document.
        assert_eq!(
            classify_file(
                Some("pdf"),
                supported.max_content_bytes + 1,
                false,
                None,
                all_on,
                &supported
            ),
            (FileKind::Document, DeliveryMode::MetadataOnly)
        );
        // Turning off documents with `other` on falls back to metadata only.
        let mut no_docs = all_on;
        no_docs.documents = false;
        assert_eq!(
            classify_file(Some("pdf"), 10_000, false, None, no_docs, &supported),
            (FileKind::Document, DeliveryMode::MetadataOnly)
        );
        // Turning off both documents and `other` ignores PDFs completely.
        no_docs.other = false;
        assert_eq!(
            classify_file(Some("pdf"), 10_000, false, None, no_docs, &supported),
            (FileKind::Document, DeliveryMode::Ignore)
        );
    }

    #[test]
    fn structured_and_long_markdown_promote_to_document() {
        let supported = SupportedList::default();
        let mut structured = String::from("# Chapter One\n\n## Section A\n\n");
        for _ in 0..1_500 {
            structured.push_str("prose ");
        }
        assert_eq!(
            classify_text_file(structured.as_bytes(), &supported),
            FileKind::Document
        );

        let short = b"# Todo\n\n- send redlines\n- test build\n";
        assert_eq!(classify_text_file(short, &supported), FileKind::Note);
    }

    #[test]
    fn overlap_detects_obsidian_screenshots_parent_child_and_exact_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("Vault");
        let sub = vault.join("notes");
        let shots = dir.path().join("Pictures/Screenshots");
        let dl = dir.path().join("Downloads");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&shots).unwrap();
        std::fs::create_dir_all(&dl).unwrap();

        assert_eq!(
            detect_overlap(&sub, Some((&vault, "field notes")), None, &[]),
            OverlapOutcome::Overlap(OverlapTarget::Obsidian {
                name: "field notes".into()
            })
        );
        assert_eq!(
            detect_overlap(
                &dir.path().join("Pictures"),
                None,
                Some((&shots, "Screenshots")),
                &[]
            ),
            OverlapOutcome::Overlap(OverlapTarget::Screenshots {
                name: "Screenshots".into()
            })
        );
        assert_eq!(
            detect_overlap(&dl, None, None, &[("f1", &dl, "Downloads")]),
            OverlapOutcome::AlreadyAdded {
                id: "f1".into(),
                name: "Downloads".into()
            }
        );
    }

    #[test]
    fn symlink_escaping_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("chosen");
        let outside = dir.path().join("secret.txt");
        let inside = root.join("ok.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "secret").unwrap();
        std::fs::write(&inside, "ok").unwrap();
        let esc_link = root.join("escape.txt");
        let ok_link = root.join("local.txt");
        std::os::unix::fs::symlink(&outside, &esc_link).unwrap();
        std::os::unix::fs::symlink(&inside, &ok_link).unwrap();

        let canon_root = std::fs::canonicalize(&root).unwrap();
        assert!(!symlink_stays_inside(&canon_root, &esc_link));
        assert!(symlink_stays_inside(&canon_root, &ok_link));
    }
}

#[cfg(test)]
mod advertised_layout_tests {
    use super::*;
    #[test]
    fn structured_files_require_an_advertised_transcript_layout() {
        let mut list = SupportedList::default();
        let selected = FolderKinds {
            other: false,
            ..Default::default()
        };
        assert_eq!(
            classify_file(
                Some("pdf"),
                list.max_content_bytes + 1,
                false,
                None,
                selected,
                &list
            )
            .1,
            DeliveryMode::MetadataOnly
        );
        list.transcript_extensions
            .extend(["json".into(), "tsv".into()]);
        for (ext, bytes, expected) in [
            (
                "tsv",
                "start\tend\ttext\n0\t1000\tHello\n",
                FileKind::Transcript,
            ),
            ("tsv", "id\tvalue\n1\twidget\n", FileKind::Other),
            (
                "json",
                r#"{"text":"Hi","language":"en","segments":[{"start":0,"end":1,"text":"Hi"}]}"#,
                FileKind::Transcript,
            ),
            ("json", r#"{"name":"Hi","values":[1]}"#, FileKind::Other),
        ] {
            assert_eq!(
                classify_file(
                    Some(ext),
                    bytes.len() as u64,
                    false,
                    Some(bytes.as_bytes()),
                    FolderKinds::default(),
                    &list
                )
                .0,
                expected
            );
        }
    }
}
