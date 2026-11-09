// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Finished window files, waiting for delivery oldest window first. The directory
//! is the only index: each file sits under its window with its counts in the
//! parent's name, so nothing else needs to stay in step with it. Nothing here
//! says which journal it is for.

use crate::{clock::Window, store, wire::NoteFile};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// One file ready to send.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub window: Window,
    pub name: String,
    pub path: PathBuf,
    pub lines: u32,
    pub first_sight: u32,
}

pub struct Outbox {
    root: PathBuf,
    aside: PathBuf,
    entries: BTreeMap<(Window, String), Delivery>,
    aside_count: u64,
}

impl Outbox {
    pub fn open(root: &Path, aside: &Path) -> io::Result<Self> {
        store::ensure_dir(root)?;
        store::ensure_dir(aside)?;
        let mut entries = BTreeMap::new();
        for delivery in Self::list(root)? {
            entries.insert((delivery.window.clone(), delivery.name.clone()), delivery);
        }
        let aside_count = Self::list(aside)?.len() as u64;
        Ok(Self {
            root: root.to_path_buf(),
            aside: aside.to_path_buf(),
            entries,
            aside_count,
        })
    }

    fn list(root: &Path) -> io::Result<Vec<Delivery>> {
        let mut found = Vec::new();
        for day_entry in fs::read_dir(root)? {
            let day_dir = day_entry?;
            let Ok(day) = day_dir.file_name().into_string() else {
                continue;
            };
            for segment_entry in fs::read_dir(day_dir.path())? {
                let segment_dir = segment_entry?;
                let Ok(segment) = segment_dir.file_name().into_string() else {
                    continue;
                };
                for counts_entry in fs::read_dir(segment_dir.path())? {
                    let counts_dir = counts_entry?;
                    let Ok(counts) = counts_dir.file_name().into_string() else {
                        continue;
                    };
                    let Some((lines, first_sight)) = counts.split_once('-') else {
                        continue;
                    };
                    let (Ok(lines), Ok(first_sight)) = (lines.parse(), first_sight.parse()) else {
                        continue;
                    };
                    for file_entry in fs::read_dir(counts_dir.path())? {
                        let file = file_entry?;
                        let Ok(name) = file.file_name().into_string() else {
                            continue;
                        };
                        if !name.starts_with("obsidian_") || !name.ends_with(".jsonl") {
                            continue;
                        }
                        found.push(Delivery {
                            window: Window {
                                day: day.clone(),
                                segment: segment.clone(),
                            },
                            name,
                            path: file.path(),
                            lines,
                            first_sight,
                        });
                    }
                }
            }
        }
        Ok(found)
    }

    /// Durably add one file. The same bytes arriving twice lands on the same name.
    pub fn write(&mut self, file: &NoteFile) -> io::Result<()> {
        // Each directory made here is fsynced into its parent, so the file can't
        // outlive a crash in name only.
        let mut dir = self.root.clone();
        for part in [
            file.window.day.clone(),
            file.window.segment.clone(),
            format!("{}-{}", file.lines, file.first_sight),
        ] {
            let next = dir.join(part);
            if !next.is_dir() {
                store::ensure_dir(&next)?;
                store::sync_dir(&dir);
            }
            dir = next;
        }
        let temp = dir.join(format!(".{}.tmp", file.name));
        {
            let mut out = store::private_open_options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp)?;
            out.write_all(&file.bytes)?;
            out.sync_all()?;
        }
        let final_path = dir.join(&file.name);
        fs::rename(&temp, &final_path)?;
        store::sync_dir(&dir);
        self.entries.insert(
            (file.window.clone(), file.name.clone()),
            Delivery {
                window: file.window.clone(),
                name: file.name.clone(),
                path: final_path,
                lines: file.lines,
                first_sight: file.first_sight,
            },
        );
        Ok(())
    }

    /// Everything waiting, oldest window first.
    pub fn iter(&self) -> impl Iterator<Item = &Delivery> {
        self.entries.values()
    }

    /// The oldest window's first file.
    pub fn oldest(&self) -> Option<&Delivery> {
        self.entries.values().next()
    }

    pub fn lines_waiting(&self) -> u64 {
        self.entries.values().map(|d| u64::from(d.lines)).sum()
    }

    pub fn aside_count(&self) -> u64 {
        self.aside_count
    }

    /// Delivered: delete the file.
    pub fn remove(&mut self, delivery: &Delivery) -> io::Result<()> {
        match fs::remove_file(&delivery.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = delivery.path.parent() {
            store::remove_empty_ancestors(parent, &self.root);
        }
        self.entries
            .remove(&(delivery.window.clone(), delivery.name.clone()));
        Ok(())
    }

    /// Give every set-aside file another try. A file that can't be moved stays
    /// aside and is still counted.
    pub fn restore_aside(&mut self) -> io::Result<()> {
        let mut remaining = 0;
        for delivery in Self::list(&self.aside)? {
            let relative = delivery
                .path
                .strip_prefix(&self.aside)
                .map_err(io::Error::other)?;
            let target = self.root.join(relative);
            let moved = target
                .parent()
                .map_or(Ok(()), store::ensure_dir)
                .and_then(|()| fs::rename(&delivery.path, &target));
            if moved.is_err() {
                remaining += 1;
                continue;
            }
            if let Some(parent) = delivery.path.parent() {
                store::remove_empty_ancestors(parent, &self.aside);
            }
            self.entries.insert(
                (delivery.window.clone(), delivery.name.clone()),
                Delivery {
                    path: target,
                    ..delivery
                },
            );
        }
        self.aside_count = remaining;
        Ok(())
    }

    /// Refused by the journal as invalid: move aside and keep going.
    pub fn set_aside(&mut self, delivery: &Delivery) -> io::Result<()> {
        let relative = delivery
            .path
            .strip_prefix(&self.root)
            .map_err(io::Error::other)?;
        let target = self.aside.join(relative);
        if let Some(parent) = target.parent() {
            store::ensure_dir(parent)?;
        }
        fs::rename(&delivery.path, &target)?;
        if let Some(parent) = delivery.path.parent() {
            store::remove_empty_ancestors(parent, &self.root);
        }
        self.entries
            .remove(&(delivery.window.clone(), delivery.name.clone()));
        self.aside_count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_file(day: &str, segment: &str, name: &str, lines: u32) -> NoteFile {
        NoteFile {
            window: Window {
                day: day.into(),
                segment: segment.into(),
            },
            name: format!("obsidian_{name}.jsonl"),
            bytes: b"{}\n".to_vec(),
            lines,
            first_sight: lines,
        }
    }

    #[test]
    fn delivery_is_oldest_window_first_and_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("outbox");
        let aside = temp.path().join("aside");
        {
            let mut outbox = Outbox::open(&root, &aside).unwrap();
            outbox
                .write(&note_file("20261108", "101500_300", "bbb", 2))
                .unwrap();
            outbox
                .write(&note_file("20260101", "101500_300", "aaa", 1))
                .unwrap();
            outbox
                .write(&note_file("20261108", "094000_300", "ccc", 3))
                .unwrap();
        }
        let mut outbox = Outbox::open(&root, &aside).unwrap();
        assert_eq!(outbox.lines_waiting(), 6);
        let oldest = outbox.oldest().unwrap().clone();
        assert_eq!(oldest.window.day, "20260101");
        assert_eq!((oldest.lines, oldest.first_sight), (1, 1));
        outbox.remove(&oldest).unwrap();
        assert_eq!(outbox.oldest().unwrap().window.segment, "094000_300");
        // An invalid file is set aside and the rest carry on.
        let next = outbox.oldest().unwrap().clone();
        outbox.set_aside(&next).unwrap();
        assert_eq!(outbox.aside_count(), 1);
        assert_eq!(outbox.oldest().unwrap().name, "obsidian_bbb.jsonl");
        // Empty directories are cleaned, and the aside file is counted on reopen.
        let reopened = Outbox::open(&root, &aside).unwrap();
        assert_eq!(reopened.aside_count(), 1);
        assert_eq!(reopened.lines_waiting(), 2);
        assert!(!root.join("20260101").exists());
    }
}
