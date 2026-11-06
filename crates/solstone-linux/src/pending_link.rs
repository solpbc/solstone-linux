// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Carries a clicked pairing link from the launcher to the window.
//!
//! A pairing link is good until it's used, and a command line is readable by every
//! account on the computer for as long as the process lives. So the link waits in a
//! file only this account can read, and the window's command line never holds it.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FILE_NAME: &str = "clicked-pairing-link";
/// Longer than any pairing link stays good.
const NEWEST_USEFUL_AGE: Duration = Duration::from_secs(15 * 60);

/// The clicked link to hand over, when this argument is a pairing link.
pub fn clicked_link(argument: &str) -> Option<String> {
    crate::window_control::adapt_graphical_link(argument).ok()
}

pub fn default_path() -> PathBuf {
    crate::config::resolve_config_paths(&Default::default())
        .state_dir()
        .join(FILE_NAME)
}

pub fn store(path: &Path, link: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(link.as_bytes())?;
    drop(file);
    fs::rename(&temporary, path)
}

/// The link waiting for the window, once. One older than a link can last is dropped.
pub fn take(path: &Path) -> Option<String> {
    let age = fs::metadata(path).ok()?.modified().ok()?.elapsed();
    let link = fs::read_to_string(path).ok();
    let _ = fs::remove_file(path);
    match age {
        Ok(age) if age > NEWEST_USEFUL_AGE => None,
        _ => clicked_link(&link?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_stored_link_is_private_and_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join(FILE_NAME);
        store(&path, "solstone://pair#FRAG").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(take(&path).as_deref(), Some("solstone://pair#FRAG"));
        assert_eq!(take(&path), None);
    }

    #[test]
    fn only_pairing_links_are_handed_over() {
        assert!(clicked_link("solstone://pair#FRAG").is_some());
        assert!(clicked_link("solstone://pair/#FRAG").is_some());
        assert!(clicked_link("SOLSTONE://pair#FRAG").is_some());
        assert!(clicked_link("solstone://pair").is_none());
        assert!(clicked_link("solstone://open#FRAG").is_none());
        assert!(clicked_link("--in-scope").is_none());
    }

    #[test]
    fn something_else_in_the_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "not a link").unwrap();
        assert_eq!(take(&path), None);
        assert!(!path.exists());
    }
}
