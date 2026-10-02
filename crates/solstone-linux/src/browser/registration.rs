// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Per-user native-messaging registrations. Written by `install-service` and by every
//! `run`, so a browser installed after the app is covered at the next start, and a
//! tampered or moved manifest is repaired. Removed by `uninstall-service`.

use native_browser_frame::render_registration;
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// The browsers a registration is written for, and where each one looks. Brave reads
/// Chrome's directory, and Chromium reads its own; both take the Chromium manifest.
pub const TARGETS: &[(&str, &str, &str)] = &[
    (
        "chrome",
        "chrome",
        ".config/google-chrome/NativeMessagingHosts",
    ),
    (
        "chromium",
        "chrome",
        ".config/chromium/NativeMessagingHosts",
    ),
    (
        "edge",
        "edge",
        ".config/microsoft-edge/NativeMessagingHosts",
    ),
    ("firefox", "firefox", ".mozilla/native-messaging-hosts"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub browser: &'static str,
    pub path: PathBuf,
    pub json: String,
}

fn channels(dev_enabled: bool) -> &'static [&'static str] {
    if dev_enabled {
        &["production", "dev"]
    } else {
        &["production"]
    }
}

/// Every manifest this build writes, pointing at `binary`.
pub fn manifests(home: &Path, binary: &Path, dev_enabled: bool) -> Vec<Manifest> {
    let binary = binary.to_string_lossy();
    let mut out = Vec::new();
    for channel in channels(dev_enabled) {
        for (browser, contract_browser, directory) in TARGETS {
            let rendered =
                render_registration(channel, contract_browser, "linux", Some(&binary), None)
                    .expect("the contract renders every linux registration");
            out.push(Manifest {
                browser,
                path: home.join(directory).join(&rendered.filename),
                json: rendered.json,
            });
        }
    }
    out
}

/// Manifests this build must not leave behind: the development registrations, unless
/// the build admits the development extension.
fn stale_paths(home: &Path, dev_enabled: bool) -> Vec<PathBuf> {
    if dev_enabled {
        return Vec::new();
    }
    TARGETS
        .iter()
        .map(|(_, contract_browser, directory)| {
            let rendered = render_registration("dev", contract_browser, "linux", None, None)
                .expect("the contract renders every linux registration");
            home.join(directory).join(rendered.filename)
        })
        .collect()
}

/// Write every registration whose content differs, creating directories as needed.
/// Directory existence is not an install detector: a browser installed later finds
/// its registration already in place.
pub fn write_all(home: &Path, binary: &Path, dev_enabled: bool) -> io::Result<usize> {
    let mut written = 0;
    let mut first_error = None;
    for manifest in manifests(home, binary, dev_enabled) {
        match write_if_changed(&manifest.path, manifest.json.as_bytes()) {
            Ok(true) => written += 1,
            Ok(false) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    for path in stale_paths(home, dev_enabled) {
        if let Err(error) = remove_if_present(&path) {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(written),
    }
}

pub fn remove_all(home: &Path) -> io::Result<()> {
    let mut first_error = None;
    for path in manifests(home, Path::new("/"), true)
        .into_iter()
        .map(|manifest| manifest.path)
    {
        if let Err(error) = remove_if_present(&path) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Which registrations are in place and current, for `status` and `doctor`.
pub fn check(home: &Path, binary: &Path) -> Vec<(&'static str, bool)> {
    manifests(home, binary, false)
        .into_iter()
        .map(|manifest| {
            let current =
                fs::read(&manifest.path).is_ok_and(|bytes| bytes == manifest.json.as_bytes());
            (manifest.browser, current)
        })
        .collect()
}

fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(false);
    }
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("registration path has no parent"))?;
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(contents)?;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    Ok(true)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The path the registrations name: the stable command, never a versioned install
/// directory. The platform installer keeps each version under
/// `<prefix>/opt/solstone/desktop/<version>/bin/` and publishes `<prefix>/bin/` as the
/// stable pointer; a package installs `/usr/bin/solstone-linux` directly.
pub fn stable_binary(current_exe: &Path) -> PathBuf {
    let components: Vec<_> = current_exe.components().collect();
    let name = current_exe.file_name();
    if components.len() >= 6 {
        let tail = &components[components.len() - 6..];
        let tail: Vec<_> = tail.iter().map(|part| part.as_os_str()).collect();
        if tail[0] == "opt" && tail[1] == "solstone" && tail[4] == "bin" {
            let prefix: PathBuf = components[..components.len() - 6].iter().collect();
            if let Some(name) = name {
                return prefix.join("bin").join(name);
            }
        }
    }
    current_exe.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_builds_write_production_registrations_only() {
        let temp = tempfile::tempdir().unwrap();
        let binary = Path::new("/usr/bin/solstone-linux");
        assert_eq!(write_all(temp.path(), binary, false).unwrap(), 4);
        let chrome = temp
            .path()
            .join(".config/google-chrome/NativeMessagingHosts/app.solstone.browser.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&chrome).unwrap()).unwrap();
        assert_eq!(manifest["path"], "/usr/bin/solstone-linux");
        assert_eq!(
            manifest["allowed_origins"],
            serde_json::json!(["chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim/"])
        );
        let firefox: serde_json::Value = serde_json::from_slice(
            &fs::read(
                temp.path()
                    .join(".mozilla/native-messaging-hosts/app.solstone.browser.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            firefox["allowed_extensions"],
            serde_json::json!(["browser@solstone.app"])
        );
        assert!(
            temp.path()
                .join(".config/chromium/NativeMessagingHosts/app.solstone.browser.json")
                .is_file()
        );
        assert!(
            temp.path()
                .join(".config/microsoft-edge/NativeMessagingHosts/app.solstone.browser.json")
                .is_file()
        );
        assert!(
            !temp
                .path()
                .join(".config/google-chrome/NativeMessagingHosts/app.solstone.browser.dev.json")
                .exists()
        );
        assert!(check(temp.path(), binary).iter().all(|(_, ok)| *ok));
        // Unchanged content is not rewritten.
        assert_eq!(write_all(temp.path(), binary, false).unwrap(), 0);
    }

    #[test]
    fn a_tampered_registration_is_repaired_and_a_dev_leftover_removed() {
        let temp = tempfile::tempdir().unwrap();
        let binary = Path::new("/usr/bin/solstone-linux");
        write_all(temp.path(), binary, true).unwrap();
        let dev = temp
            .path()
            .join(".mozilla/native-messaging-hosts/app.solstone.browser.dev.json");
        assert!(dev.is_file());
        let firefox = temp
            .path()
            .join(".mozilla/native-messaging-hosts/app.solstone.browser.json");
        fs::write(&firefox, b"{}").unwrap();
        assert!(!check(temp.path(), binary).iter().all(|(_, ok)| *ok));
        assert_eq!(write_all(temp.path(), binary, false).unwrap(), 1);
        assert!(!dev.exists());
        assert!(check(temp.path(), binary).iter().all(|(_, ok)| *ok));
        remove_all(temp.path()).unwrap();
        assert!(!firefox.exists());
    }

    #[test]
    fn the_registered_path_is_the_stable_command() {
        assert_eq!(
            stable_binary(Path::new(
                "/home/o/.local/opt/solstone/desktop/2.0.12-0123456789ab/bin/solstone-linux"
            )),
            PathBuf::from("/home/o/.local/bin/solstone-linux")
        );
        assert_eq!(
            stable_binary(Path::new("/usr/bin/solstone-linux")),
            PathBuf::from("/usr/bin/solstone-linux")
        );
        assert_eq!(
            stable_binary(Path::new("/home/o/.local/bin/solstone-linux")),
            PathBuf::from("/home/o/.local/bin/solstone-linux")
        );
    }
}
