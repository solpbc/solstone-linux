// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Private, durable files in the app's own data directory. Nothing here ever
//! touches a vault.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub(crate) fn ensure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_private_dir(path)
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn private_open_options() -> fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = fs::OpenOptions::new();
    options.mode(0o600);
    options
}

#[cfg(not(unix))]
pub(crate) fn private_open_options() -> fs::OpenOptions {
    fs::OpenOptions::new()
}

/// Write a file so a reader sees the whole old file or the whole new one.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("a stored file needs a parent directory"))?;
    let temp = dir.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut file = private_open_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    sync_dir(dir);
    Ok(())
}

/// Best effort: a directory fsync is not available everywhere.
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Remove `start` and its now-empty parents, stopping at `stop` (kept).
pub(crate) fn remove_empty_ancestors(start: &Path, stop: &Path) {
    let mut current = start.to_path_buf();
    while current != stop && current.starts_with(stop) {
        if fs::remove_dir(&current).is_err() {
            return;
        }
        if !current.pop() {
            return;
        }
    }
}
