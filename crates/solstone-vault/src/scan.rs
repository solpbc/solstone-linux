// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The walk of a vault: what is a note, what is skipped, and what is only a
//! placeholder. Reads directory entries and file metadata; opens no note.

use crate::{
    platform::{self, DirFileIds},
    wire::MAX_NOTE_BYTES,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// A note the walk saw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    pub mtime_ms: i64,
    pub size: u64,
    pub file_id: Option<String>,
}

/// What one walk found. Paths are vault-relative and `/`-separated.
#[derive(Debug, Default)]
pub struct Walked {
    pub notes: BTreeMap<String, Note>,
    /// Not local yet: never read, never empty, never deleted.
    pub placeholders: BTreeMap<String, u64>,
    /// Over the note size limit by file size.
    pub too_large: BTreeSet<String>,
    /// The file ids of those, so a note that grew and moved isn't taken for deleted.
    pub too_large_ids: BTreeSet<String>,
    /// Directories that could not be fully listed or examined, as `prefix/`:
    /// nothing under one counts as missing.
    pub unreadable_dirs: Vec<String>,
    pub cancelled: bool,
}

impl Walked {
    pub fn saw(&self, path: &str) -> bool {
        self.notes.contains_key(path)
            || self.placeholders.contains_key(path)
            || self.too_large.contains(path)
    }

    pub fn under_unreadable_dir(&self, path: &str) -> bool {
        self.unreadable_dirs
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
    }
}

/// The root or its `.obsidian` could not be read: conclude nothing.
#[derive(Debug, Eq, PartialEq)]
pub struct VaultUnreadable;

/// Whether the vault can be read right now. Listing the root and `.obsidian`
/// proves more than a stat does: an unmounted volume can leave an empty mount
/// point that still stats as a directory.
pub fn vault_is_readable(root: &Path) -> bool {
    fs::read_dir(root).is_ok() && fs::read_dir(root.join(".obsidian")).is_ok()
}

/// A directory name the walk does not descend into.
pub fn dir_is_skipped(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("templates")
        || name.eq_ignore_ascii_case("_templates")
}

/// A file name that counts as a note: ends `.md` in any case, no leading dot.
pub fn name_is_note(name: &str) -> bool {
    !name.starts_with('.') && name.to_lowercase().ends_with(".md")
}

/// Whether a vault-relative path can go on the wire at all: the walk guarantees
/// the note rules; this catches a name the contract can't carry.
pub fn path_is_reportable(path: &str) -> bool {
    !path.is_empty() && !path.contains('\\') && !path.chars().any(char::is_control)
}

/// Walk the vault, or say it can't be read.
pub fn walk(root: &Path, cancel: &AtomicBool) -> Result<Walked, VaultUnreadable> {
    walk_with_ids(root, cancel, None)
}

/// [`walk`], reading each directory's file ids from `dir_ids` where the platform's
/// metadata has none.
pub fn walk_with_ids(
    root: &Path,
    cancel: &AtomicBool,
    dir_ids: Option<&DirFileIds>,
) -> Result<Walked, VaultUnreadable> {
    if !vault_is_readable(root) {
        return Err(VaultUnreadable);
    }
    let mut walked = Walked::default();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut visited = 0_usize;
    while let Some((dir, prefix)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                if prefix.is_empty() {
                    return Err(VaultUnreadable);
                }
                walked.unreadable_dirs.push(prefix);
                continue;
            }
        };
        let ids = dir_ids.map_or_else(Default::default, |read| read(&dir));
        let mut reachable = true;
        for entry in entries {
            visited += 1;
            if visited.is_multiple_of(1024) && cancel.load(Ordering::Relaxed) {
                walked.cancelled = true;
                return Ok(walked);
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    reachable = false;
                    break;
                }
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    reachable = false;
                    break;
                }
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue; // not valid UTF-8, so no contract path can name it
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !dir_is_skipped(&name) && path_is_reportable(&name) {
                    stack.push((entry.path(), format!("{prefix}{name}/")));
                }
                continue;
            }
            if !kind.is_file() || !name_is_note(&name) {
                continue;
            }
            let rel = format!("{prefix}{name}");
            if !path_is_reportable(&rel) {
                continue;
            }
            match entry.metadata() {
                Ok(metadata) if platform::is_placeholder(&metadata) => {
                    walked.placeholders.insert(rel, metadata.len());
                }
                Ok(metadata) if metadata.len() > MAX_NOTE_BYTES as u64 => {
                    walked.too_large.insert(rel);
                    walked
                        .too_large_ids
                        .extend(platform::file_id(&metadata).or_else(|| ids.get(&name).cloned()));
                }
                Ok(metadata) => {
                    walked.notes.insert(
                        rel,
                        Note {
                            mtime_ms: platform::mtime_ms(&metadata),
                            size: metadata.len(),
                            file_id: platform::file_id(&metadata)
                                .or_else(|| ids.get(&name).cloned()),
                        },
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    reachable = false;
                    break;
                }
            }
        }
        if !reachable {
            if prefix.is_empty() {
                return Err(VaultUnreadable);
            }
            walked.unreadable_dirs.push(prefix);
        }
    }
    // A drive lost partway through reads as folders that are simply gone: Windows
    // reports a missing drive or network path as not found. Ask again before
    // believing anything this walk didn't see.
    if !vault_is_readable(root) {
        return Err(VaultUnreadable);
    }
    Ok(walked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_and_skipped_folders_follow_the_rules() {
        assert!(name_is_note("a.md"));
        assert!(name_is_note("A.MD"));
        assert!(name_is_note("a.Md"));
        assert!(!name_is_note(".hidden.md"));
        assert!(!name_is_note("a.txt"));
        assert!(!name_is_note(".md"));
        assert!(dir_is_skipped(".obsidian"));
        assert!(dir_is_skipped(".trash"));
        assert!(dir_is_skipped("templates"));
        assert!(dir_is_skipped("Templates"));
        assert!(dir_is_skipped("_TEMPLATES"));
        assert!(!dir_is_skipped("my templates"));
    }

    #[test]
    fn a_name_the_contract_can_not_carry_is_not_reportable() {
        assert!(!path_is_reportable("a\\b.md"));
        assert!(!path_is_reportable("a\nb.md"));
        assert!(!path_is_reportable(""));
        assert!(path_is_reportable("sub/dir/a b.md"));
    }

    #[test]
    fn an_absent_vault_reports_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            walk(&temp.path().join("gone"), &cancel).unwrap_err(),
            VaultUnreadable
        );
        // A folder that exists with no .obsidian is not a vault either.
        assert_eq!(walk(temp.path(), &cancel).unwrap_err(), VaultUnreadable);
    }

    #[test]
    fn the_walk_skips_dot_folders_templates_non_notes_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::write(root.join(".obsidian/workspace.md"), "x").unwrap();
        fs::create_dir_all(root.join(".trash")).unwrap();
        fs::write(root.join(".trash/deleted.md"), "x").unwrap();
        fs::create_dir_all(root.join("Templates")).unwrap();
        fs::write(root.join("Templates/t.md"), "x").unwrap();
        fs::create_dir_all(root.join("_templates/deep")).unwrap();
        fs::write(root.join("_templates/deep/t.md"), "x").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/keep.md"), "x").unwrap();
        fs::write(root.join("sub/UPPER.MD"), "x").unwrap();
        fs::write(root.join("sub/.hidden.md"), "x").unwrap();
        fs::write(root.join("sub/image.png"), "x").unwrap();
        fs::write(root.join("note.md"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(root.join("note.md"), root.join("link.md")).unwrap();
            symlink(root.join("sub"), root.join("dirlink")).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let walked = walk(root, &cancel).unwrap();
        let notes: Vec<&str> = walked.notes.keys().map(String::as_str).collect();
        assert_eq!(notes, vec!["note.md", "sub/UPPER.MD", "sub/keep.md"]);
    }

    #[test]
    fn a_vault_lost_during_the_walk_reports_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Notes");
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/a.md"), "x").unwrap();
        let moved = temp.path().join("elsewhere");
        let (from, to) = (root.clone(), moved);
        let lose_it: Box<DirFileIds> = Box::new(move |_dir: &Path| {
            let _ = fs::rename(&from, &to);
            Default::default()
        });
        let cancel = AtomicBool::new(false);
        assert_eq!(
            walk_with_ids(&root, &cancel, Some(lose_it.as_ref())).unwrap_err(),
            VaultUnreadable
        );
    }

    #[test]
    fn a_note_over_the_limit_is_only_counted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::write(root.join("big.md"), vec![b'x'; MAX_NOTE_BYTES + 1]).unwrap();
        fs::write(root.join("ok.md"), vec![b'x'; MAX_NOTE_BYTES]).unwrap();
        let cancel = AtomicBool::new(false);
        let walked = walk(root, &cancel).unwrap();
        assert!(walked.too_large.contains("big.md"));
        assert!(walked.notes.contains_key("ok.md"));
        assert!(!walked.notes.contains_key("big.md"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_subfolder_concludes_nothing_about_it() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::create_dir_all(root.join("locked")).unwrap();
        fs::write(root.join("locked/a.md"), "x").unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(root.join("locked")).is_ok() {
            // Root reads anything, so the premise can't be set up here.
            fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }
        let cancel = AtomicBool::new(false);
        let walked = walk(root, &cancel).unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(walked.unreadable_dirs, vec!["locked/".to_owned()]);
        assert!(walked.under_unreadable_dir("locked/a.md"));
        assert!(!walked.under_unreadable_dir("lockedish/a.md"));
    }
}
