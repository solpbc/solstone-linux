// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        && xdg.is_absolute()
    {
        return xdg;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/share")
}

fn source_dir() -> Option<PathBuf> {
    // 1. Check relative to current exe
    if let Ok(exe) = crate::service::current_executable()
        && let Some(prefix) = exe.parent().and_then(|p| p.parent())
    {
        let candidate = prefix.join("share/solstone-linux/activity-helpers");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    // 2. Check /usr/share
    let usr = PathBuf::from("/usr/share/solstone-linux/activity-helpers");
    if usr.is_dir() {
        return Some(usr);
    }
    // 3. Fallback to packaging/activity-helpers in repo
    let repo = PathBuf::from("packaging/activity-helpers");
    if repo.is_dir() {
        return Some(repo);
    }
    None
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn run_install_activity_helpers(output: &mut dyn Write, errors: &mut dyn Write) -> i32 {
    let Some(source) = source_dir() else {
        let _ = writeln!(errors, "error: activity helper assets directory not found");
        return 1;
    };

    let user_data = data_dir();
    let gnome_src = source.join("solstone-activity@solpbc.org");
    let gnome_dst = user_data.join("gnome-shell/extensions/solstone-activity@solpbc.org");
    let kwin_src = source.join("solstone-activity-kwin");
    let kwin_dst = user_data.join("kwin/scripts/solstone-activity-kwin");

    let mut copied_any = false;

    if gnome_src.is_dir() {
        if let Err(err) = copy_tree(&gnome_src, &gnome_dst) {
            let _ = writeln!(errors, "error: failed to copy GNOME activity helper: {err}");
            return 1;
        }
        copied_any = true;
    }

    if kwin_src.is_dir() {
        if let Err(err) = copy_tree(&kwin_src, &kwin_dst) {
            let _ = writeln!(errors, "error: failed to copy KWin activity helper: {err}");
            return 1;
        }
        copied_any = true;
    }

    if !copied_any {
        let _ = writeln!(
            errors,
            "error: no activity helper assets found in {source:?}"
        );
        return 1;
    }

    let _ = writeln!(
        output,
        "installed activity helper assets to user data directories."
    );
    let _ = writeln!(
        output,
        "note: you must explicitly enable the extension or script in your desktop session."
    );
    0
}
