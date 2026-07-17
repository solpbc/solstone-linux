// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{fs, io, path::Path};

pub fn load_restore_token(path: &Path) -> Option<String> {
    let token = fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

pub fn save_restore_token(path: &Path, token: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", token.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_is_tolerant_and_trims() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("token");
        assert_eq!(load_restore_token(&path), None);
        fs::write(&path, "  \n").unwrap();
        assert_eq!(load_restore_token(&path), None);
        fs::write(&path, b"\xff").unwrap();
        assert_eq!(load_restore_token(&path), None);
        fs::write(&path, "  token-1 \n").unwrap();
        assert_eq!(load_restore_token(&path).as_deref(), Some("token-1"));
    }

    #[test]
    fn save_creates_parents_trims_and_rotates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/token");
        save_restore_token(&path, " old ").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\n");
        save_restore_token(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    }
}
