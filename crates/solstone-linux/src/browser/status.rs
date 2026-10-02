// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What `status` and `doctor` say about the browser path. The running app writes a
//! small status file; everything else is read from disk.

use super::custody::{self, Layout};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const RUNNING_FILE: &str = "status.json";
/// The running app rewrites its status every five seconds.
const RUNNING_STALE_MS: i64 = 30_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub updated_at_ms: i64,
    pub capture: String,
    pub delivery: String,
    pub failure: Option<String>,
    pub held_bytes: u64,
    pub full: bool,
    pub browsers: Vec<String>,
}

pub fn write_running(layout: &Layout, status: &Running) -> io::Result<()> {
    let path = layout.root.join(RUNNING_FILE);
    let temporary = layout.root.join(format!(".{RUNNING_FILE}.tmp"));
    fs::write(
        &temporary,
        serde_json::to_vec(status).map_err(io::Error::other)?,
    )?;
    fs::rename(temporary, path)
}

pub fn clear_running(layout: &Layout) {
    let _ = fs::remove_file(layout.root.join(RUNNING_FILE));
}

fn read_running(layout: &Layout, now_ms: i64) -> Option<Running> {
    let status: Running =
        serde_json::from_slice(&fs::read(layout.root.join(RUNNING_FILE)).ok()?).ok()?;
    (now_ms - status.updated_at_ms < RUNNING_STALE_MS).then_some(status)
}

fn size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", (bytes as f64 / 1024.0).ceil())
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Lines for `solstone-linux status`.
pub fn status_lines(base_dir: &Path, now_ms: i64) -> Vec<String> {
    let layout = Layout::new(base_dir);
    let mut lines = Vec::new();
    match read_running(&layout, now_ms) {
        Some(running) => {
            let mut browsers = running.browsers.clone();
            browsers.sort();
            browsers.dedup();
            lines.push(if browsers.is_empty() {
                "Browser: no browser connected".to_owned()
            } else {
                format!("Browser: {} connected", browsers.join(", "))
            });
            if running.full {
                lines.push(
                    "        the room for browser pages on this computer is full, so no new pages are taken in until some go into your journal"
                        .to_owned(),
                );
            } else if running.capture == "paused" {
                lines.push("        paused, so no new pages are taken in".to_owned());
            }
            if running.held_bytes > 0 {
                lines.push(format!(
                    "        {} of browser pages on this computer, waiting to go into your journal",
                    size(running.held_bytes)
                ));
            }
        }
        None => {
            let held = custody::held_bytes(&layout);
            lines.push("Browser: the solstone app is not running".to_owned());
            if held > 0 {
                lines.push(format!(
                    "        {} of browser pages on this computer, waiting to go into your journal",
                    size(held)
                ));
            }
        }
    }
    lines.extend(retired_lines(&layout));
    lines
}

fn retired_lines(layout: &Layout) -> Vec<String> {
    let retired = custody::retired_summary(layout);
    if retired.periods == 0 {
        return Vec::new();
    }
    vec![
        format!(
            "        {} of browser pages were kept for a journal this computer was paired with before.",
            size(retired.bytes)
        ),
        "        they won't go into any journal, and they stay on this computer until you discard them:"
            .to_owned(),
        "        solstone-linux discard-browser-pages".to_owned(),
    ]
}

/// What a browser install can and cannot do, for `doctor`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowserInstalls {
    pub chromium_snap: bool,
    pub firefox_snap: bool,
    /// What the owner answered when snap Firefox asked to start the solstone app.
    pub firefox_snap_decision: SnapDecision,
    pub flatpak_chromium_family: Vec<String>,
}

/// Snap Firefox reaches the app through the desktop's WebExtensions portal, which
/// asks the owner once and remembers the answer in the portal permission store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SnapDecision {
    #[default]
    NotAsked,
    /// The answer could not be read on this desktop.
    Unknown,
    Allowed,
    Refused,
}

/// The portal permission store, as `gdbus` reaches it.
pub const PERMISSION_STORE: [&str; 6] = [
    "call",
    "--session",
    "--dest",
    "org.freedesktop.impl.portal.PermissionStore",
    "--object-path",
    "/org/freedesktop/impl/portal/PermissionStore",
];
pub const PERMISSION_TABLE: &str = "webextensions";
pub const SNAP_FIREFOX: &str = "snap.firefox";

/// Read the answer out of a `PermissionStore.Lookup` reply such as
/// `({'snap.firefox': ['no']}, <byte 0x00>)`. No reply means it never asked.
pub fn parse_snap_decision(lookup: Option<&str>) -> SnapDecision {
    let Some(reply) = lookup else {
        return SnapDecision::NotAsked;
    };
    let Some(start) = reply.find(&format!("'{SNAP_FIREFOX}'")) else {
        return SnapDecision::NotAsked;
    };
    let rest = &reply[start..];
    let answer = &rest[..rest.find(']').unwrap_or(rest.len())];
    if answer.contains("'yes'") {
        SnapDecision::Allowed
    } else if answer.contains("'no'") {
        SnapDecision::Refused
    } else {
        SnapDecision::NotAsked
    }
}

pub fn detect_installs(snap_root: &Path, flatpak_roots: &[PathBuf]) -> BrowserInstalls {
    let flatpak_ids = [
        "com.google.Chrome",
        "org.chromium.Chromium",
        "com.microsoft.Edge",
        "com.brave.Browser",
    ];
    let mut flatpak_chromium_family = Vec::new();
    for id in flatpak_ids {
        if flatpak_roots.iter().any(|root| root.join(id).is_dir()) {
            flatpak_chromium_family.push(id.to_owned());
        }
    }
    BrowserInstalls {
        chromium_snap: snap_root.join("chromium").is_dir(),
        firefox_snap: snap_root.join("firefox").is_dir(),
        firefox_snap_decision: SnapDecision::NotAsked,
        flatpak_chromium_family,
    }
}

/// Lines for `solstone-linux doctor`: whether each registration is in place, and the
/// browser installs that cannot reach the app by construction.
pub fn doctor_lines(home: &Path, binary: &Path, installs: &BrowserInstalls) -> Vec<String> {
    let mut lines = Vec::new();
    let checks = super::registration::check(home, binary);
    let missing: Vec<_> = checks
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(browser, _)| *browser)
        .collect();
    if missing.is_empty() {
        lines.push(
            "ok    browser registration          chrome, chromium, edge and firefox".to_owned(),
        );
    } else {
        lines.push(format!(
            "warn  browser registration          missing or out of date for {}; start the solstone app to write them again",
            missing.join(", ")
        ));
    }
    if installs.firefox_snap {
        lines.push(match installs.firefox_snap_decision {
            SnapDecision::NotAsked => {
                "ok    firefox (snap)                asks you once to let it start the solstone app".to_owned()
            }
            SnapDecision::Unknown => {
                "warn  firefox (snap)                asks you once to let it start the solstone app; doctor couldn't read what you answered".to_owned()
            }
            SnapDecision::Allowed => {
                "ok    firefox (snap)                allowed to start the solstone app".to_owned()
            }
            SnapDecision::Refused => format!(
                "warn  firefox (snap)                you told your desktop not to let firefox start the solstone app, so nothing is taken in from it. to be asked again, run:\n      gdbus {} --method org.freedesktop.impl.portal.PermissionStore.DeletePermission {PERMISSION_TABLE} {} {SNAP_FIREFOX}",
                PERMISSION_STORE.join(" "),
                native_browser_frame::PROD_HOST
            ),
        });
    }
    if installs.chromium_snap {
        lines.push(
            "warn  chromium (snap)               can't reach the solstone app; a snap of chromium has no way to start it. use chrome, edge or firefox"
                .to_owned(),
        );
    }
    for id in &installs.flatpak_chromium_family {
        lines.push(format!(
            "warn  {id:<28}  can't reach the solstone app from inside a flatpak; use chrome, edge or firefox installed outside flatpak"
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_the_running_app_and_falls_back_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path());
        fs::create_dir_all(&layout.root).unwrap();
        assert_eq!(
            status_lines(temp.path(), 1_000_000),
            vec!["Browser: the solstone app is not running".to_owned()]
        );
        write_running(
            &layout,
            &Running {
                updated_at_ms: 1_000_000,
                capture: "permitted".into(),
                delivery: "kept_locally".into(),
                failure: None,
                held_bytes: 2048,
                full: false,
                browsers: vec!["firefox".into(), "chrome".into(), "chrome".into()],
            },
        )
        .unwrap();
        assert_eq!(
            status_lines(temp.path(), 1_005_000),
            vec![
                "Browser: chrome, firefox connected".to_owned(),
                "        2 KB of browser pages on this computer, waiting to go into your journal"
                    .to_owned(),
            ]
        );
        // A status file the app stopped refreshing is not a running app.
        assert_eq!(
            status_lines(temp.path(), 1_000_000 + RUNNING_STALE_MS)[0],
            "Browser: the solstone app is not running"
        );
    }

    #[test]
    fn the_snap_firefox_answer_is_read_from_the_portal_store() {
        assert_eq!(parse_snap_decision(None), SnapDecision::NotAsked);
        assert_eq!(
            parse_snap_decision(Some("({'snap.firefox': ['no']}, <byte 0x00>)")),
            SnapDecision::Refused
        );
        assert_eq!(
            parse_snap_decision(Some("({'snap.firefox': ['yes']}, <byte 0x00>)")),
            SnapDecision::Allowed
        );
        assert_eq!(
            parse_snap_decision(Some("({'snap.other': ['yes']}, <byte 0x00>)")),
            SnapDecision::NotAsked
        );
        let refused = BrowserInstalls {
            firefox_snap: true,
            firefox_snap_decision: SnapDecision::Refused,
            ..Default::default()
        };
        let lines = doctor_lines(
            Path::new("/nonexistent"),
            Path::new("/usr/bin/solstone-linux"),
            &refused,
        );
        assert!(lines[1].starts_with("warn  firefox (snap)"));
        assert!(
            lines[1].contains("DeletePermission webextensions app.solstone.browser snap.firefox")
        );
    }

    #[test]
    fn doctor_names_installs_that_cannot_reach_the_app() {
        let temp = tempfile::tempdir().unwrap();
        let snap = temp.path().join("snap");
        fs::create_dir_all(snap.join("chromium")).unwrap();
        fs::create_dir_all(snap.join("firefox")).unwrap();
        let flatpak = temp.path().join("flatpak");
        fs::create_dir_all(flatpak.join("com.brave.Browser")).unwrap();
        let installs = detect_installs(&snap, &[flatpak]);
        assert!(installs.chromium_snap && installs.firefox_snap);
        assert_eq!(installs.flatpak_chromium_family, vec!["com.brave.Browser"]);
        let home = temp.path().join("home");
        let binary = Path::new("/usr/bin/solstone-linux");
        let lines = doctor_lines(&home, binary, &installs);
        assert!(lines[0].starts_with("warn  browser registration"));
        assert!(lines.iter().any(|line| line.contains("chromium (snap)")));
        assert!(lines.iter().any(|line| line.contains("com.brave.Browser")));
        crate::browser::registration::write_all(&home, binary, false).unwrap();
        assert!(doctor_lines(&home, binary, &BrowserInstalls::default())[0].starts_with("ok"));
    }
}
