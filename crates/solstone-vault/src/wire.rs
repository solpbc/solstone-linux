// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The note contract: one change as a compact JSON line, and the deterministic files
//! those lines become.

use crate::clock::Window;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Most lines in one file.
pub const MAX_FILE_LINES: usize = 5_000;
/// Most bytes in one file.
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Most bytes of text in one note.
pub const MAX_NOTE_BYTES: usize = 4 * 1024 * 1024;
/// A vault's name is 1 to 128 bytes with no control characters.
pub const MAX_VAULT_NAME_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Changed,
    Removed,
    Renamed,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Removed => "removed",
            Self::Renamed => "renamed",
        }
    }
}

/// A line on the wire. The fields are declared in sorted key order, so the
/// compact serialization is the contract's sorted-keys form.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Line {
    pub change: Change,
    pub changed_at: String,
    pub content_hash: Option<String>,
    pub observed_at: String,
    pub path: String,
    pub previous_path: Option<String>,
    pub text: Option<String>,
    pub v: u8,
    pub vault: String,
}

impl Line {
    /// The sort order within a file: vault, path, changed_at, then change.
    pub fn sort_key(&self) -> (&str, &str, &str, &str) {
        (
            &self.vault,
            &self.path,
            &self.changed_at,
            self.change.as_str(),
        )
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a line always serializes")
    }
}

/// 64-character lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A finished file: `obsidian_<id>.jsonl`, where id is the SHA-256 of `bytes`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteFile {
    pub window: Window,
    pub name: String,
    pub bytes: Vec<u8>,
    pub lines: u32,
    pub first_sight: u32,
}

/// Accumulates sorted lines into files under the contract's limits.
pub struct FileBuilder {
    window: Window,
    bytes: Vec<u8>,
    lines: u32,
    first_sight: u32,
}

impl FileBuilder {
    pub fn new(window: Window) -> Self {
        Self {
            window,
            bytes: Vec::new(),
            lines: 0,
            first_sight: 0,
        }
    }

    /// Add one line; if this line would break a limit the current file is returned
    /// first, finished, and the line starts the next.
    pub fn push(&mut self, line: &[u8], first_sight: bool) -> Option<NoteFile> {
        debug_assert!(
            line.len() < MAX_FILE_BYTES,
            "too-large lines never get here"
        );
        let mut finished = None;
        if self.lines as usize == MAX_FILE_LINES
            || self.bytes.len() + line.len() + 1 > MAX_FILE_BYTES
        {
            finished = self.take_file();
        }
        self.bytes.extend_from_slice(line);
        self.bytes.push(b'\n');
        self.lines += 1;
        if first_sight {
            self.first_sight += 1;
        }
        finished
    }

    /// The last file, if any lines are held.
    pub fn finish(mut self) -> Option<NoteFile> {
        self.take_file()
    }

    fn take_file(&mut self) -> Option<NoteFile> {
        if self.lines == 0 {
            return None;
        }
        let bytes = std::mem::take(&mut self.bytes);
        let file = NoteFile {
            window: self.window.clone(),
            name: format!("obsidian_{}.jsonl", sha256_hex(&bytes)),
            bytes,
            lines: self.lines,
            first_sight: self.first_sight,
        };
        self.lines = 0;
        self.first_sight = 0;
        Some(file)
    }
}

/// Build the contract's files for a window from its lines, in sorted order.
pub fn build_files(window: &Window, lines: &[Line]) -> Vec<NoteFile> {
    let mut sorted: Vec<&Line> = lines.iter().collect();
    sorted.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let mut builder = FileBuilder::new(window.clone());
    let mut files = Vec::new();
    for line in sorted {
        if let Some(file) = builder.push(&line.to_json(), false) {
            files.push(file);
        }
    }
    files.extend(builder.finish());
    files
}

/// What the journal answered about one file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Answer {
    /// `ok`, `duplicate`, `collision` or `segment_removed`: delete the file.
    Delivered,
    /// `obsidian_note_ahead`: hold the file and send it again later.
    Ahead,
    /// `obsidian_note_invalid`: keep the file aside and carry on.
    Invalid,
    /// Anything else: retry with backoff.
    Retry,
}

impl Answer {
    /// The answer from an ingest reply's status word and reason code.
    pub fn from_reply(status: Option<&str>, reason_code: Option<&str>) -> Self {
        match reason_code {
            Some("segment_removed") => return Self::Delivered,
            Some("obsidian_note_ahead") => return Self::Ahead,
            Some("obsidian_note_invalid") => return Self::Invalid,
            _ => {}
        }
        match status {
            Some("ok" | "duplicate" | "collision") => Self::Delivered,
            _ => Self::Retry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Window {
        Window {
            day: "20261108".into(),
            segment: "094000_300".into(),
        }
    }

    /// The contract's golden file, reproduced from parsed values. Line 3 of the
    /// golden keeps `previous_path` before `text` by key order: that is `Line`'s
    /// field order, which is why it must stay sorted.
    #[test]
    fn golden_file_bytes_and_name() {
        let changed = Line {
            change: Change::Changed,
            changed_at: "2026-11-08T09:42:10-07:00".into(),
            content_hash: Some(
                "f0b347123a86bae3e1d4d8c48a7f0430bed090b37d8ed1873a63a97c0b13dfa9".into(),
            ),
            observed_at: "2026-11-08T09:42:33-07:00".into(),
            path: "daily/2026-11-08.md".into(),
            previous_path: None,
            text: Some("---\ntags: [walk]\n---\nMet Alex by the river.\n".into()),
            v: 1,
            vault: "Notes".into(),
        };
        let removed = Line {
            change: Change::Removed,
            changed_at: "2026-11-08T09:44:33-07:00".into(),
            content_hash: None,
            observed_at: "2026-11-08T09:44:33-07:00".into(),
            path: "inbox/old idea.md".into(),
            previous_path: None,
            text: None,
            v: 1,
            vault: "Notes".into(),
        };
        let renamed = Line {
            change: Change::Renamed,
            changed_at: "2026-11-08T09:43:02-07:00".into(),
            content_hash: Some(
                "14fece546a747b07dec10e2658310b2d224447158a8c6e1b3a65d467b9de22a3".into(),
            ),
            observed_at: "2026-11-08T09:43:33-07:00".into(),
            path: "people/Alex Rivera.md".into(),
            previous_path: Some("inbox/Alex.md".into()),
            text: Some("Alex likes rivers.\n".into()),
            v: 1,
            vault: "Notes".into(),
        };
        // Handed in out of order: the file sorts them.
        let files = build_files(&window(), &[renamed, removed, changed]);
        assert_eq!(files.len(), 1);
        let expected = concat!(
            r#"{"change":"changed","changed_at":"2026-11-08T09:42:10-07:00","content_hash":"f0b347123a86bae3e1d4d8c48a7f0430bed090b37d8ed1873a63a97c0b13dfa9","observed_at":"2026-11-08T09:42:33-07:00","path":"daily/2026-11-08.md","previous_path":null,"text":"---\ntags: [walk]\n---\nMet Alex by the river.\n","v":1,"vault":"Notes"}"#,
            "\n",
            r#"{"change":"removed","changed_at":"2026-11-08T09:44:33-07:00","content_hash":null,"observed_at":"2026-11-08T09:44:33-07:00","path":"inbox/old idea.md","previous_path":null,"text":null,"v":1,"vault":"Notes"}"#,
            "\n",
            r#"{"change":"renamed","changed_at":"2026-11-08T09:43:02-07:00","content_hash":"14fece546a747b07dec10e2658310b2d224447158a8c6e1b3a65d467b9de22a3","observed_at":"2026-11-08T09:43:33-07:00","path":"people/Alex Rivera.md","previous_path":"inbox/Alex.md","text":"Alex likes rivers.\n","v":1,"vault":"Notes"}"#,
            "\n"
        );
        assert_eq!(String::from_utf8_lossy(&files[0].bytes), expected);
        assert_eq!(
            files[0].name,
            "obsidian_3e100d06fb18b4f45302cb3cf2cad1881e7183360649f0543ce4acf565b9f81e.jsonl"
        );
    }

    #[test]
    fn answers_map_as_the_contract_says() {
        assert_eq!(Answer::from_reply(Some("ok"), None), Answer::Delivered);
        assert_eq!(
            Answer::from_reply(Some("duplicate"), None),
            Answer::Delivered
        );
        assert_eq!(
            Answer::from_reply(Some("collision"), None),
            Answer::Delivered
        );
        assert_eq!(
            Answer::from_reply(Some("failed"), Some("segment_removed")),
            Answer::Delivered
        );
        assert_eq!(
            Answer::from_reply(None, Some("obsidian_note_ahead")),
            Answer::Ahead
        );
        assert_eq!(
            Answer::from_reply(None, Some("obsidian_note_invalid")),
            Answer::Invalid
        );
        assert_eq!(Answer::from_reply(Some("failed"), None), Answer::Retry);
        assert_eq!(Answer::from_reply(None, None), Answer::Retry);
    }

    #[test]
    fn a_large_set_splits_into_consecutive_files_under_the_limits() {
        let line = |i: usize| Line {
            change: Change::Changed,
            changed_at: "2026-11-08T09:42:10-07:00".into(),
            content_hash: Some(sha256_hex(b"x")),
            observed_at: "2026-11-08T09:42:33-07:00".into(),
            path: format!("n{i:05}.md"),
            previous_path: None,
            text: Some("x".into()),
            v: 1,
            vault: "Notes".into(),
        };
        let lines: Vec<Line> = (0..5_001).map(line).collect();
        let files = build_files(&window(), &lines);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].lines, 5_000);
        assert_eq!(files[1].lines, 1);
        // Sorted across the split: the last line of the first file sorts before
        // the first line of the second.
        assert!(files[0].bytes.ends_with(b"\n"));
        assert!(String::from_utf8_lossy(&files[1].bytes).contains("n05000"));

        let big = |i: usize| Line {
            text: Some("y".repeat(3 * 1024 * 1024)),
            ..line(i)
        };
        let files = build_files(&window(), &[big(0), big(1), big(2)]);
        assert_eq!(files.len(), 2, "two 3 MiB lines fit, the third starts over");
        assert!(files.iter().all(|file| file.bytes.len() <= MAX_FILE_BYTES));
    }
}
