// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pending changes: coalesced per note per window, held until their window closes.
//!
//! An append-only log on disk holds each entry's whole line; memory holds only an
//! index without the text, so a large first scan costs what the index costs, not
//! what the vault's text costs. Every record states which earlier entries it
//! replaces, so a torn tail only loses the last writes (which the next scan
//! re-derives from the table, which is saved only after the log is durable).

use crate::{
    clock::{Stamp, Window},
    store,
    wire::{Change, Line},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// `(window, vault, path)`: one line per note per window.
pub type Key = (Window, String, String);

/// Everything about a pending line except its text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Meta {
    pub change: Change,
    pub vault: String,
    pub path: String,
    pub previous_path: Option<String>,
    pub changed_at: Stamp,
    pub observed_at: Stamp,
    pub content_hash: Option<String>,
    /// The journal has never heard of this note.
    pub first_sight: bool,
}

#[derive(Serialize)]
struct RecordOut<'a> {
    replace: &'a [Key],
    entry: Option<EntryOut<'a>>,
}

#[derive(Serialize)]
struct EntryOut<'a> {
    window: &'a Window,
    #[serde(flatten)]
    meta: &'a Meta,
    text: Option<&'a str>,
}

#[derive(Deserialize)]
struct RecordMeta {
    replace: Vec<Key>,
    entry: Option<EntryMeta>,
}

#[derive(Deserialize)]
struct EntryMeta {
    window: Window,
    #[serde(flatten)]
    meta: Meta,
}

#[derive(Deserialize)]
struct RecordText {
    entry: Option<EntryText>,
}

#[derive(Deserialize)]
struct EntryText {
    text: Option<String>,
}

#[derive(Clone, Debug)]
struct Slot {
    meta: Meta,
    offset: u64,
    len: u32,
}

pub struct Pending {
    path: PathBuf,
    writer: BufWriter<File>,
    reader: File,
    index: BTreeMap<Key, Slot>,
    tail: u64,
    dead: u64,
    /// A write failed: what's on disk may not end where `tail` says.
    repair_needed: bool,
    #[cfg(test)]
    short_write_fault: Option<usize>,
}

const COMPACT_MIN_DEAD: u64 = 64 * 1024 * 1024;

impl Pending {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = store::private_open_options()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        let (index, tail, dead, valid_end) = Self::replay(&file)?;
        if valid_end != file.metadata()?.len() {
            truncate(path, valid_end)?;
        }
        let reader = File::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: BufWriter::new(file),
            reader,
            index,
            tail,
            dead,
            repair_needed: false,
            #[cfg(test)]
            short_write_fault: None,
        })
    }

    /// Bring the file back to exactly what has been accepted, so later offsets
    /// stay true after a failed write.
    fn repair(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        if self.writer.get_ref().metadata()?.len() > self.tail {
            truncate(&self.path, self.tail)?;
        }
        self.repair_needed = false;
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn replay(file: &File) -> io::Result<(BTreeMap<Key, Slot>, u64, u64, u64)> {
        let mut index: BTreeMap<Key, Slot> = BTreeMap::new();
        let mut reader = BufReader::new(file.try_clone()?);
        reader.seek(SeekFrom::Start(0))?;
        let mut offset = 0_u64;
        let mut dead = 0_u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 || !line.ends_with(b"\n") {
                break; // end, or a torn last record
            }
            // A whole line that doesn't decode costs only itself.
            let Ok(record) = serde_json::from_slice::<RecordMeta>(&line) else {
                dead += read as u64;
                offset += read as u64;
                continue;
            };
            for key in &record.replace {
                if let Some(slot) = index.remove(key) {
                    dead += u64::from(slot.len);
                }
            }
            match record.entry {
                Some(entry) => {
                    index.insert(
                        (
                            entry.window,
                            entry.meta.vault.clone(),
                            entry.meta.path.clone(),
                        ),
                        Slot {
                            meta: entry.meta,
                            offset,
                            len: u32::try_from(read).unwrap_or(u32::MAX),
                        },
                    );
                }
                None => dead += read as u64,
            }
            offset += read as u64;
        }
        Ok((index, offset, dead, offset))
    }

    /// Replace the entries at `replaced` with `entry` as one durable record.
    pub fn put(
        &mut self,
        replaced: &[Key],
        entry: Option<(&Window, &Meta, Option<&str>)>,
    ) -> io::Result<()> {
        let record = RecordOut {
            replace: replaced,
            entry: entry.map(|(window, meta, text)| EntryOut { window, meta, text }),
        };
        let mut bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if self.repair_needed {
            self.repair()?;
        }
        #[cfg(test)]
        if let Some(accepted) = self.short_write_fault.take() {
            self.writer.flush()?;
            self.writer.get_mut().write_all(&bytes[..accepted])?;
            self.repair_needed = true;
            return Err(io::Error::other("short write"));
        }
        if let Err(error) = self.writer.write_all(&bytes) {
            self.repair_needed = true;
            return Err(error);
        }
        for key in replaced {
            if let Some(slot) = self.index.remove(key) {
                self.dead += u64::from(slot.len);
            }
        }
        let len = bytes.len() as u64;
        if let Some((window, meta, _)) = entry {
            self.index.insert(
                (window.clone(), meta.vault.clone(), meta.path.clone()),
                Slot {
                    meta: meta.clone(),
                    offset: self.tail,
                    len: u32::try_from(len).unwrap_or(u32::MAX),
                },
            );
        } else {
            self.dead += len;
        }
        self.tail += len;
        Ok(())
    }

    pub fn get(&self, key: &Key) -> Option<&Meta> {
        self.index.get(key).map(|slot| &slot.meta)
    }

    /// Make everything written so far durable.
    pub fn sync(&mut self) -> io::Result<()> {
        if self.repair_needed {
            self.repair()?;
        }
        if let Err(error) = self.writer.flush() {
            self.repair_needed = true;
            return Err(error);
        }
        self.writer.get_ref().sync_data()
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn first_sight_count(&self) -> u64 {
        self.index
            .values()
            .filter(|slot| slot.meta.first_sight)
            .count() as u64
    }

    /// Windows whose every entry's window ended at least `grace_ms` ago.
    pub fn due_windows(&self, now_ms: i64, grace_ms: i64) -> Vec<Window> {
        let mut windows: BTreeMap<&Window, bool> = BTreeMap::new();
        for ((window, _, _), slot) in &self.index {
            let due = slot.meta.changed_at.window_end_ms() + grace_ms <= now_ms;
            windows
                .entry(window)
                .and_modify(|all_due| *all_due &= due)
                .or_insert(due);
        }
        windows
            .into_iter()
            .filter(|(_, due)| *due)
            .map(|(window, _)| window.clone())
            .collect()
    }

    /// A window's keys in the contract's line order, without reading any text.
    pub fn window_keys_sorted(&self, window: &Window) -> Vec<Key> {
        let mut keys: Vec<Key> = self
            .index
            .range((window.clone(), String::new(), String::new())..)
            .take_while(|((candidate, _, _), _)| candidate == window)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort_by_key(|key| {
            let meta = &self.index[key].meta;
            (
                meta.vault.clone(),
                meta.path.clone(),
                meta.changed_at.rfc3339(),
                meta.change.as_str(),
            )
        });
        keys
    }

    /// Build a key's full wire line by reading only its text from the log. A record
    /// that can't be decoded is `InvalidData`; anything else is worth retrying.
    pub fn read_line(&mut self, key: &Key) -> io::Result<(Line, bool)> {
        let slot = self
            .index
            .get(key)
            .ok_or_else(|| io::Error::other("pending entry vanished"))?
            .clone();
        // A removal has no text, so it needs nothing from the log.
        let text = if slot.meta.change == Change::Removed {
            None
        } else {
            self.writer.flush()?;
            self.reader.seek(SeekFrom::Start(slot.offset))?;
            let mut bytes = vec![0_u8; slot.len as usize];
            self.reader.read_exact(&mut bytes)?;
            let record: RecordText = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            record.entry.and_then(|entry| entry.text)
        };
        let meta = slot.meta;
        Ok((
            Line {
                change: meta.change,
                changed_at: meta.changed_at.rfc3339(),
                content_hash: meta.content_hash,
                observed_at: meta.observed_at.rfc3339(),
                path: meta.path,
                previous_path: meta.previous_path,
                text,
                v: 1,
                vault: meta.vault,
            },
            meta.first_sight,
        ))
    }

    /// Forget closed entries. When nothing is left, the log starts over.
    pub fn remove_keys(&mut self, keys: &[Key]) -> io::Result<()> {
        self.put(keys, None)?;
        if self.index.is_empty() {
            self.writer.flush()?;
            truncate(&self.path, 0)?;
            self.tail = 0;
            self.dead = 0;
        } else if self.dead >= COMPACT_MIN_DEAD && self.dead > self.tail / 2 {
            self.compact()?;
        }
        Ok(())
    }

    fn compact(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        let temp = self.path.with_extension("compact");
        let mut out = BufWriter::new(
            store::private_open_options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp)?,
        );
        let mut bytes = Vec::new();
        let mut new_index = BTreeMap::new();
        let mut offset = 0_u64;
        for (key, slot) in &self.index {
            bytes.resize(slot.len as usize, 0);
            self.reader.seek(SeekFrom::Start(slot.offset))?;
            self.reader.read_exact(&mut bytes)?;
            out.write_all(&bytes)?;
            new_index.insert(
                key.clone(),
                Slot {
                    meta: slot.meta.clone(),
                    offset,
                    len: slot.len,
                },
            );
            offset += u64::from(slot.len);
        }
        out.flush()?;
        out.get_ref().sync_all()?;
        drop(out);
        std::fs::rename(&temp, &self.path)?;
        let file = store::private_open_options()
            .read(true)
            .append(true)
            .open(&self.path)?;
        self.writer = BufWriter::new(file);
        self.reader = File::open(&self.path)?;
        self.index = new_index;
        self.tail = offset;
        self.dead = 0;
        Ok(())
    }
}

/// Through its own write handle: Windows refuses `set_len` on an append-only one.
fn truncate(path: &Path, len: u64) -> io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(len)?;
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(minute: u32) -> Window {
        Window {
            day: "20261108".into(),
            segment: format!("09{minute:02}00_300"),
        }
    }

    fn stamp(ms: i64) -> Stamp {
        Stamp::new(ms, -7 * 3600)
    }

    fn meta(path: &str, ms: i64) -> Meta {
        Meta {
            change: Change::Changed,
            vault: "Notes".into(),
            path: path.into(),
            previous_path: None,
            changed_at: stamp(ms),
            observed_at: stamp(ms),
            content_hash: Some(crate::wire::sha256_hex(b"x")),
            first_sight: true,
        }
    }

    #[test]
    fn entries_survive_a_restart_and_a_torn_tail_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pending.log");
        let key = (window(40), "Notes".to_owned(), "a.md".to_owned());
        {
            let mut pending = Pending::open(&path).unwrap();
            pending
                .put(&[], Some((&window(40), &meta("a.md", 1), Some("hello"))))
                .unwrap();
            pending.sync().unwrap();
        }
        // A crash mid-write leaves a torn record at the end.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"replace\":[")
            .unwrap();
        let mut pending = Pending::open(&path).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending.get(&key).unwrap().first_sight);
        let (line, first) = pending.read_line(&key).unwrap();
        assert_eq!(line.text.as_deref(), Some("hello"));
        assert!(first);
        // New records append after the trimmed tail.
        pending
            .put(
                std::slice::from_ref(&key),
                Some((&window(40), &meta("a.md", 2), Some("hello2"))),
            )
            .unwrap();
        pending.sync().unwrap();
        let pending2 = Pending::open(&path).unwrap();
        assert_eq!(pending2.len(), 1);
        assert_eq!(pending2.get(&key).unwrap().changed_at, stamp(2));
    }

    #[test]
    fn a_failed_write_leaves_later_records_readable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("p.log");
        let mut pending = Pending::open(&path).unwrap();
        let key = |name: &str| (window(40), "Notes".to_owned(), name.to_owned());
        pending
            .put(&[], Some((&window(40), &meta("a.md", 1), Some("first"))))
            .unwrap();
        pending.short_write_fault = Some(10);
        assert!(
            pending
                .put(&[], Some((&window(40), &meta("b.md", 1), Some("torn"))))
                .is_err()
        );
        pending
            .put(&[], Some((&window(40), &meta("c.md", 1), Some("third"))))
            .unwrap();
        pending.sync().unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.get(&key("b.md")).is_none());
        for _ in 0..2 {
            assert_eq!(
                pending.read_line(&key("a.md")).unwrap().0.text.as_deref(),
                Some("first")
            );
            assert_eq!(
                pending.read_line(&key("c.md")).unwrap().0.text.as_deref(),
                Some("third")
            );
            // And the same again from disk.
            pending = Pending::open(&path).unwrap();
        }
    }

    #[test]
    fn the_same_path_in_two_vaults_is_two_entries() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Pending::open(&temp.path().join("p.log")).unwrap();
        let other = Meta {
            vault: "Other".into(),
            ..meta("a.md", 1)
        };
        pending
            .put(&[], Some((&window(40), &meta("a.md", 1), Some("notes"))))
            .unwrap();
        pending
            .put(&[], Some((&window(40), &other, Some("other"))))
            .unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending.window_keys_sorted(&window(40)).len(), 2);
    }

    #[test]
    fn due_windows_close_ninety_seconds_after_they_end_and_not_before() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Pending::open(&temp.path().join("p.log")).unwrap();
        // 2026-11-08T09:42:10-07:00: window 09:40, ends 09:45 = 1_794_156_300_000 ms.
        let moment = 1_794_156_130_000;
        pending
            .put(
                &[],
                Some((&stamp(moment).window(), &meta("a.md", moment), Some("x"))),
            )
            .unwrap();
        let ends = stamp(moment).window_end_ms();
        assert!(pending.due_windows(ends + 89_999, 90_000).is_empty());
        assert_eq!(pending.due_windows(ends + 90_000, 90_000).len(), 1);
    }

    #[test]
    fn a_window_with_any_entry_not_yet_due_stays_open() {
        // Around a clock change, one window's entries can carry different offsets, and
        // so end at different instants. Each is read in its own offset.
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Pending::open(&temp.path().join("p.log")).unwrap();
        let shared = window(40);
        // The same wall clock either side of a fall-back: an hour apart as instants.
        let at = |offset_hours: i32| {
            Stamp::new(
                1_794_156_130_000 + i64::from(-6 - offset_hours) * 3_600_000,
                offset_hours * 3600,
            )
        };
        let entry = |path: &str, stamp: Stamp| Meta {
            changed_at: stamp,
            ..meta(path, 0)
        };
        for (path, stamp) in [("a.md", at(-6)), ("b.md", at(-7)), ("c.md", at(-6))] {
            pending
                .put(&[], Some((&shared, &entry(path, stamp), Some("x"))))
                .unwrap();
        }
        let earlier_end = at(-6).window_end_ms();
        let later_end = at(-7).window_end_ms();
        assert!(earlier_end < later_end);
        assert!(pending.due_windows(earlier_end + 90_000, 90_000).is_empty());
        assert!(pending.due_windows(later_end + 89_999, 90_000).is_empty());
        assert_eq!(
            pending.due_windows(later_end + 90_000, 90_000),
            vec![shared]
        );
    }

    #[test]
    fn compaction_keeps_live_entries_readable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("p.log");
        let mut pending = Pending::open(&path).unwrap();
        let keep = (window(40), "Notes".to_owned(), "keep.md".to_owned());
        pending
            .put(&[], Some((&window(40), &meta("keep.md", 1), Some("kept"))))
            .unwrap();
        for index in 0..20 {
            let key = (window(45), "Notes".to_owned(), format!("{index}.md"));
            pending
                .put(
                    &[],
                    Some((
                        &window(45),
                        &meta(&key.2, 1),
                        Some(&"y".repeat(4 * 1024 * 1024)),
                    )),
                )
                .unwrap();
        }
        let dropped: Vec<Key> = (0..20)
            .map(|index| (window(45), "Notes".to_owned(), format!("{index}.md")))
            .collect();
        pending.remove_keys(&dropped).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending.read_line(&keep).unwrap().0.text.as_deref(),
            Some("kept")
        );
        assert!(std::fs::metadata(&path).unwrap().len() < 1024);
        drop(pending);
        let mut reopened = Pending::open(&path).unwrap();
        assert_eq!(
            reopened.read_line(&keep).unwrap().0.text.as_deref(),
            Some("kept")
        );
    }
}
