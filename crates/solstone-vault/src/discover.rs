// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Finding vaults on this computer from Obsidian's own list, and checking a folder
//! the owner chose. Read-only.

use crate::wire::MAX_VAULT_NAME_BYTES;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A vault the owner can pick.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FoundVault {
    pub name: String,
    pub path: String,
}

/// Why a chosen folder can't be used.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// No `.obsidian` directory.
    NotAVault,
    /// The folder's name can't be a vault name in the contract.
    UnusableName,
}

/// Where Obsidian keeps its list of vaults on Linux, in preference order.
pub fn linux_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/obsidian/obsidian.json"),
        home.join(".var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json"),
        home.join("snap/obsidian/current/.config/obsidian/obsidian.json"),
    ]
}

/// The vaults named in the first readable candidate lists, whose folders still
/// have `.obsidian`, deduplicated by path and sorted by name.
pub fn found_vaults(candidates: &[PathBuf]) -> Vec<FoundVault> {
    let mut found: Vec<FoundVault> = Vec::new();
    for candidate in candidates {
        let Ok(bytes) = fs::read(candidate) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let Some(vaults) = value.get("vaults") else {
            continue;
        };
        // Obsidian writes an object keyed by vault id; an array is accepted too.
        let entries: Vec<&serde_json::Value> = match vaults {
            serde_json::Value::Object(map) => map.values().collect(),
            serde_json::Value::Array(list) => list.iter().collect(),
            _ => continue,
        };
        for entry in entries {
            let Some(path) = entry.get("path").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !Path::new(path).join(".obsidian").is_dir() {
                continue;
            }
            let Ok(name) = vault_name(Path::new(path)) else {
                continue;
            };
            if !found.iter().any(|vault| vault.path == path) {
                found.push(FoundVault {
                    name,
                    path: path.to_owned(),
                });
            }
        }
    }
    found.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    found
}

/// The name a folder would carry on the wire: its folder name, if the contract
/// can carry that.
pub fn vault_name(root: &Path) -> Result<String, Refusal> {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Refusal::UnusableName)?;
    if name.is_empty() || name.len() > MAX_VAULT_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(Refusal::UnusableName);
    }
    Ok(name.to_owned())
}

/// Check a folder the owner chose: its name, then that it has `.obsidian`.
pub fn check_vault_folder(root: &Path) -> Result<String, Refusal> {
    let name = vault_name(root)?;
    if !root.join(".obsidian").is_dir() {
        return Err(Refusal::NotAVault);
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_vaults_reads_the_lists_and_keeps_only_real_vaults() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let real = home.join("Documents/Notes");
        fs::create_dir_all(real.join(".obsidian")).unwrap();
        let flatpak_real = home.join("Second Vault");
        fs::create_dir_all(flatpak_real.join(".obsidian")).unwrap();
        let stale = home.join("Gone");

        fs::create_dir_all(home.join(".config/obsidian")).unwrap();
        fs::write(
            home.join(".config/obsidian/obsidian.json"),
            serde_json::json!({
                "vaults": {
                    "abc": {"path": real.to_str().unwrap(), "ts": 1},
                    "dup": {"path": real.to_str().unwrap(), "ts": 2},
                    "stale": {"path": stale.to_str().unwrap()}
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::create_dir_all(home.join(".var/app/md.obsidian.Obsidian/config/obsidian")).unwrap();
        fs::write(
            home.join(".var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json"),
            serde_json::json!({"vaults": [{"path": flatpak_real.to_str().unwrap()}]}).to_string(),
        )
        .unwrap();
        let found = found_vaults(&linux_candidates(home));
        assert_eq!(
            found.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            vec!["Notes", "Second Vault"]
        );
    }

    #[test]
    fn a_broken_list_finds_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let bad = temp.path().join("obsidian.json");
        fs::write(&bad, "not json").unwrap();
        assert!(found_vaults(&[bad]).is_empty());
        assert!(found_vaults(&[temp.path().join("absent.json")]).is_empty());
    }

    #[test]
    fn a_folder_without_dot_obsidian_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("just a folder");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(check_vault_folder(&plain), Err(Refusal::NotAVault));
        fs::create_dir_all(plain.join(".obsidian")).unwrap();
        assert_eq!(check_vault_folder(&plain).unwrap(), "just a folder");
    }

    #[test]
    fn an_unusable_folder_name_is_refused() {
        let long = format!("/tmp/{}", "x".repeat(129));
        assert_eq!(vault_name(Path::new(&long)), Err(Refusal::UnusableName));
        assert_eq!(vault_name(Path::new("/")), Err(Refusal::UnusableName));
        assert_eq!(vault_name(Path::new("/tmp/ok")).unwrap(), "ok");
    }
}
