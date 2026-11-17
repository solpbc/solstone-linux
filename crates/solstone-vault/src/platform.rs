// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The operating system's say in what a scan sees: a file's stable id, whether a
//! file is a placeholder for content not on this device, and opening a file for
//! reading without touching it.

use std::{collections::HashMap, fs, path::Path, time::UNIX_EPOCH};

#[cfg(target_os = "linux")]
use std::io;

/// `device:inode`, then the creation time where the file system has one: inode
/// numbers are reused, so a new note can otherwise look like a deleted one renamed.
#[cfg(unix)]
fn compose_file_id(dev: u64, ino: u64, created: Option<std::time::SystemTime>) -> String {
    match created.and_then(|created| created.duration_since(UNIX_EPOCH).ok()) {
        Some(created) => format!("{dev}:{ino}:{}", created.as_nanos()),
        None => format!("{dev}:{ino}"),
    }
}

/// Where the platform's file metadata doesn't carry an id (Windows), a caller may
/// supply one directory at a time: every entry's stable id, by name, read without
/// opening any entry. An empty map means no ids here, and pairing falls back to
/// content.
pub type DirFileIds = dyn Fn(&Path) -> HashMap<String, String> + Send + Sync;

/// A stable identity for the file, where its metadata carries one.
pub fn file_id(metadata: &fs::Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(compose_file_id(
            metadata.dev(),
            metadata.ino(),
            metadata.created().ok(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// The file's modification time in milliseconds, before the epoch as negative.
pub fn mtime_ms(metadata: &fs::Metadata) -> i64 {
    let Ok(modified) = metadata.modified() else {
        return 0;
    };
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Open for reading. On Linux, without updating the file's access time where the
/// kernel allows it; a file the app doesn't own opens normally.
#[cfg(target_os = "linux")]
pub fn open_for_read(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOATIME.bits() as i32;
    match fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
    {
        Ok(file) => return Ok(file),
        Err(error) if error.raw_os_error() != Some(rustix::io::Errno::PERM.raw_os_error()) => {
            return Err(error);
        }
        Err(_) => {}
    }
    fs::File::open(path)
}

#[cfg(not(target_os = "linux"))]
pub use solstone_folder_sync::files::open_local as open_for_read;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_reused_inode_with_a_new_creation_time_is_a_different_file() {
        let first = UNIX_EPOCH + std::time::Duration::from_secs(100);
        let second = UNIX_EPOCH + std::time::Duration::from_secs(200);
        assert_ne!(
            compose_file_id(1, 2, Some(first)),
            compose_file_id(1, 2, Some(second))
        );
        assert_eq!(
            compose_file_id(1, 2, Some(first)),
            compose_file_id(1, 2, Some(first))
        );
        assert_eq!(compose_file_id(1, 2, None), "1:2");
    }

    #[cfg(unix)]
    #[test]
    fn a_unix_file_has_a_stable_id_and_a_hardlink_shares_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, b"x").unwrap();
        fs::hard_link(&a, &b).unwrap();
        let id_a = file_id(&fs::metadata(&a).unwrap());
        let id_b = file_id(&fs::metadata(&b).unwrap());
        assert!(id_a.is_some());
        assert_eq!(id_a, id_b);
    }
}
