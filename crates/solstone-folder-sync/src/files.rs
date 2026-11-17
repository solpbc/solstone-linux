// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Read-only folder access shared by desktop sources. Each source decides
//! what to visit, what to admit, and when it may read the contents.

use std::{fs, io, path::Path};

use sha2::{Digest, Sha256};

const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const RECALL_ON_OPEN: u32 = 0x0004_0000;
const OFFLINE: u32 = 0x0000_1000;

/// Whether Windows file attributes say the file's content is not local.
/// Pure so it is exercised on every platform.
pub fn attrs_are_placeholder(attributes: u32) -> bool {
    attributes & (RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN | OFFLINE) != 0
}

/// Whether reading this file would download it. Windows only: Linux desktop
/// file systems have no dataless files the app can detect.
pub fn is_placeholder(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        attrs_are_placeholder(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

/// Open for reading, and refuse a file whose content isn't here: the scan decided
/// this one was local, but it may have become a placeholder since, and opening a
/// placeholder for its data asks the cloud for it. `FILE_FLAG_OPEN_NO_RECALL`
/// doesn't stop that for a Cloud Files placeholder, so its attributes are read
/// first, from an attribute-only open that fetches nothing.
#[cfg(windows)]
pub fn open_local(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;
    if attrs_are_placeholder(fs::metadata(path)?.file_attributes()) {
        return Err(io::Error::other("this file's content isn't on this device"));
    }
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_NO_RECALL)
        .open(path)
}

#[cfg(not(windows))]
pub fn open_local(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

#[derive(Debug)]
pub enum ListingError {
    Missing,
    Incomplete(io::Error),
}

/// Open a directory for enumeration. Providers can supply directory metadata
/// without reading any file's contents.
pub fn open_directory(path: &Path) -> io::Result<fs::ReadDir> {
    fs::read_dir(path)
}

/// Visit one directory. A missing root differs from a listing interrupted
/// after it opened. Only a complete listing can establish what is absent.
pub fn visit_directory(
    path: &Path,
    mut visit: impl FnMut(fs::DirEntry) -> io::Result<()>,
) -> Result<(), ListingError> {
    let entries = open_directory(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ListingError::Missing
        } else {
            ListingError::Incomplete(error)
        }
    })?;
    for entry in entries {
        entry
            .and_then(&mut visit)
            .map_err(ListingError::Incomplete)?;
    }
    Ok(())
}

/// Read a file for identification without fetching cloud-only contents.
pub fn read_local(path: &Path) -> io::Result<Vec<u8>> {
    use io::Read;

    let mut bytes = Vec::new();
    open_local(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn settled(now_ms: i64, modified_ms: i64, quiet_ms: i64) -> bool {
    now_ms.saturating_sub(modified_ms) >= quiet_ms
}

/// The same lowercase SHA-256 encoding for local identity and wire names.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn hash_local_file(path: &Path) -> io::Result<String> {
    let mut file = open_local(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_placeholder_attributes_are_recognised() {
        assert!(attrs_are_placeholder(RECALL_ON_DATA_ACCESS));
        assert!(attrs_are_placeholder(RECALL_ON_OPEN));
        assert!(attrs_are_placeholder(OFFLINE));
        assert!(attrs_are_placeholder(OFFLINE | 0x20));
        assert!(!attrs_are_placeholder(0x20)); // archive only
        assert!(!attrs_are_placeholder(0));
    }

    #[test]
    fn a_failed_entry_is_not_a_complete_or_missing_listing() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            fs::write(dir.path().join(name), name).unwrap();
        }
        let mut visited = 0;
        let partial = visit_directory(dir.path(), |_| {
            visited += 1;
            if visited == 2 {
                return Err(io::ErrorKind::NotFound.into());
            }
            Ok(())
        });
        assert!(matches!(partial, Err(ListingError::Incomplete(_))));

        let mut names = Vec::new();
        visit_directory(dir.path(), |entry| {
            names.push(entry.file_name());
            Ok(())
        })
        .unwrap();
        assert_eq!(names.len(), 2);
        assert!(matches!(
            visit_directory(&dir.path().join("gone"), |_| Ok(())),
            Err(ListingError::Missing)
        ));
    }
}
