// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Which tmux the engine runs.
//!
//! The owner's `terminal_settings.tmux_path` when it is absolute and executable,
//! else the first `tmux` under an absolute entry of the service's `PATH`. Relative
//! entries are skipped, and the path is passed as found, never canonicalized.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use solstone_terminal::{TmuxHealth, check_tmux_health};

fn usable(path: &Path) -> bool {
    path.is_absolute() && check_tmux_health(path) == TmuxHealth::Found
}

/// `None` is "tmux not found".
pub fn resolve(configured: Option<&Path>, path_env: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(configured) = configured
        && usable(configured)
    {
        return Some(configured.to_owned());
    }
    let path_env = path_env?;
    std::env::split_paths(path_env)
        .filter(|entry| entry.is_absolute())
        .map(|entry| entry.join("tmux"))
        .find(|candidate| usable(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn configured_wins_then_path_order_then_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let configured = temp.path().join("opt/tmux");
        let first = temp.path().join("a/tmux");
        let second = temp.path().join("b/tmux");
        executable(&first);
        executable(&second);
        let path_env =
            std::env::join_paths([first.parent().unwrap(), second.parent().unwrap()]).unwrap();

        // A configured path that does not exist falls back to PATH.
        assert_eq!(
            resolve(Some(&configured), Some(&path_env)),
            Some(first.clone())
        );
        executable(&configured);
        assert_eq!(
            resolve(Some(&configured), Some(&path_env)),
            Some(configured.clone())
        );
        fs::remove_file(&configured).unwrap();
        assert_eq!(resolve(Some(&configured), Some(&path_env)), Some(first));
        assert_eq!(resolve(None, None), None);
    }

    #[test]
    fn relative_entries_and_non_executables_are_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("plain/tmux");
        fs::create_dir_all(plain.parent().unwrap()).unwrap();
        fs::write(&plain, b"").unwrap();
        let path_env = std::env::join_paths([
            PathBuf::from("relative/bin"),
            plain.parent().unwrap().to_owned(),
        ])
        .unwrap();
        assert_eq!(resolve(None, Some(&path_env)), None);
        assert_eq!(resolve(Some(Path::new("bin/tmux")), Some(&path_env)), None);
    }

    #[test]
    fn a_symlink_is_passed_as_found() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real/tmux-3.4");
        executable(&real);
        let link_dir = temp.path().join("bin");
        fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(&real, link_dir.join("tmux")).unwrap();
        assert_eq!(
            resolve(None, Some(link_dir.as_os_str())),
            Some(link_dir.join("tmux"))
        );
    }
}
