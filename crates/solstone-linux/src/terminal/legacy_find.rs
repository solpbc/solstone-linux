// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Whether a solstone-tmux install is on this computer.
//!
//! One shared, read-only test, used by the daemon before every engine start and
//! on its re-check, and by `terminal on`. It only inspects metadata and lists one
//! directory: it never opens the old app's lock or reads its spool, so it can run
//! beside a live solstone-tmux. An install is found when any of these exists under
//! a candidate root: the user unit, a `credentials.json`, or anything at all in
//! `captures/`.
//!
//! Candidate roots come from each environment in [`LegacyRootSources`], in order
//! (each one's absolute `XDG_CONFIG_HOME` and `XDG_DATA_HOME`), then the `$HOME`
//! defaults. Two spellings of one directory count once, by device and inode.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::session_env::Runner;

pub const APP_DIR: &str = "solstone-tmux";
pub const UNIT_NAME: &str = "solstone-tmux.service";
pub const CREDENTIALS_FILENAME: &str = "credentials.json";
pub const CAPTURES_DIR: &str = "captures";

/// Where candidate legacy roots come from, in candidate order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyRootSources {
    pub environments: Vec<HashMap<String, String>>,
    pub home: PathBuf,
}

impl LegacyRootSources {
    /// This process's environment, then the user manager's, then `$HOME`.
    pub fn current(runner: &dyn Runner) -> Self {
        let process: HashMap<String, String> = std::env::vars().collect();
        let home = process.get("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            environments: vec![process, crate::session_env::manager_environment(runner)],
            home,
        }
    }

    fn homes(&self, variable: &str, default: &str) -> Vec<PathBuf> {
        let mut homes: Vec<PathBuf> = self
            .environments
            .iter()
            .filter_map(|environment| environment.get(variable))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .collect();
        if !self.home.as_os_str().is_empty() {
            homes.push(self.home.join(default));
        }
        homes
    }

    /// Candidate `XDG_CONFIG_HOME`s, in order, including the default.
    pub fn config_homes(&self) -> Vec<PathBuf> {
        self.homes("XDG_CONFIG_HOME", ".config")
    }

    /// Candidate `XDG_DATA_HOME`s, in order, including the default.
    pub fn data_homes(&self) -> Vec<PathBuf> {
        self.homes("XDG_DATA_HOME", ".local/share")
    }
}

/// What the find saw. Every list holds existing paths, one per distinct directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyFindings {
    pub units: Vec<PathBuf>,
    pub credentials: Vec<PathBuf>,
    /// `captures/` directories with at least one entry.
    pub captures: Vec<PathBuf>,
    /// Existing `solstone-tmux` config roots, in candidate order.
    pub config_roots: Vec<PathBuf>,
    /// Existing `solstone-tmux` data roots, in candidate order.
    pub data_roots: Vec<PathBuf>,
}

impl LegacyFindings {
    pub fn found(&self) -> bool {
        !self.units.is_empty() || !self.credentials.is_empty() || !self.captures.is_empty()
    }
}

/// Existing directories among `candidates`, first spelling of each kept.
fn distinct_directories(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for candidate in candidates {
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_dir() && seen.insert((metadata.dev(), metadata.ino())) {
            kept.push(candidate);
        }
    }
    kept
}

fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn has_any_entry(directory: &Path) -> bool {
    fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_some())
}

pub fn find(sources: &LegacyRootSources) -> LegacyFindings {
    let config_homes = sources.config_homes();
    let unit_dirs = distinct_directories(
        config_homes
            .iter()
            .map(|home| home.join("systemd").join("user")),
    );
    let config_roots = distinct_directories(config_homes.iter().map(|home| home.join(APP_DIR)));
    let data_roots =
        distinct_directories(sources.data_homes().iter().map(|home| home.join(APP_DIR)));
    LegacyFindings {
        units: unit_dirs
            .iter()
            .map(|dir| dir.join(UNIT_NAME))
            .filter(|unit| present(unit))
            .collect(),
        credentials: config_roots
            .iter()
            .map(|root| root.join(CREDENTIALS_FILENAME))
            .filter(|credentials| present(credentials))
            .collect(),
        captures: data_roots
            .iter()
            .map(|root| root.join(CAPTURES_DIR))
            .filter(|captures| has_any_entry(captures))
            .collect(),
        config_roots,
        data_roots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(home: &Path, environments: Vec<HashMap<String, String>>) -> LegacyRootSources {
        LegacyRootSources {
            environments,
            home: home.to_owned(),
        }
    }

    fn env(pairs: &[(&str, &Path)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn nothing_found_on_a_clean_home() {
        let temp = tempfile::tempdir().unwrap();
        // An empty captures directory and stray files are not an install.
        fs::create_dir_all(temp.path().join(".local/share/solstone-tmux/captures")).unwrap();
        fs::create_dir_all(temp.path().join(".config/solstone-tmux")).unwrap();
        fs::write(temp.path().join(".config/solstone-tmux/config.json"), b"{}").unwrap();
        let findings = find(&sources(temp.path(), Vec::new()));
        assert!(!findings.found(), "{findings:?}");
    }

    #[test]
    fn each_sign_alone_is_found() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let unit = home.join(".config/systemd/user/solstone-tmux.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, b"[Unit]\n").unwrap();
        assert_eq!(
            find(&sources(home, Vec::new())).units,
            std::slice::from_ref(&unit)
        );
        fs::remove_file(&unit).unwrap();

        let credentials = home.join(".config/solstone-tmux/credentials.json");
        fs::create_dir_all(credentials.parent().unwrap()).unwrap();
        fs::write(&credentials, b"{}").unwrap();
        let findings = find(&sources(home, Vec::new()));
        assert!(findings.found());
        assert_eq!(findings.credentials, std::slice::from_ref(&credentials));
        fs::remove_file(&credentials).unwrap();

        let segment = home.join(".local/share/solstone-tmux/captures/20261108");
        fs::create_dir_all(&segment).unwrap();
        assert!(find(&sources(home, Vec::new())).found());
    }

    #[test]
    fn xdg_roots_from_either_environment_are_searched() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let shell_data = temp.path().join("shell-data");
        let manager_data = temp.path().join("manager-data");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(manager_data.join("solstone-tmux/captures/20261108")).unwrap();
        let process = env(&[("XDG_DATA_HOME", &shell_data)]);
        let manager = env(&[("XDG_DATA_HOME", &manager_data)]);
        let findings = find(&sources(&home, vec![process.clone(), manager.clone()]));
        assert_eq!(
            findings.captures,
            [manager_data.join("solstone-tmux/captures")]
        );
        assert!(!find(&sources(&home, vec![process])).found());
        // A relative value is not a root.
        let relative = HashMap::from([("XDG_DATA_HOME".to_owned(), "data".to_owned())]);
        assert_eq!(
            sources(&home, vec![relative]).data_homes(),
            [home.join(".local/share")]
        );
    }

    #[test]
    fn one_directory_spelled_twice_counts_once() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real-home");
        let link = temp.path().join("link-home");
        fs::create_dir_all(real.join(".config/solstone-tmux")).unwrap();
        fs::write(real.join(".config/solstone-tmux/credentials.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let findings = find(&sources(
            &real,
            vec![env(&[("XDG_CONFIG_HOME", &link.join(".config"))])],
        ));
        assert_eq!(findings.credentials.len(), 1);
        assert_eq!(findings.config_roots.len(), 1);
    }

    #[test]
    fn the_find_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".local/share/solstone-tmux");
        let segment = root.join("captures/20261108/host.tmux/101500_300");
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("terminal.jsonl"), b"{}\n").unwrap();
        let before = snapshot(temp.path());
        assert!(find(&sources(temp.path(), Vec::new())).found());
        assert_eq!(snapshot(temp.path()), before);
        assert!(!root.join(".solstone-tmux.lock").exists());
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_owned()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.push((path.clone(), Vec::new()));
                    stack.push(path);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}
