// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The browser path: the extension talks to this binary in host mode over native
//! messaging, host mode relays to the running app over a same-user unix socket, and
//! the app keeps what it accepts on this computer until it goes into the journal as
//! the `browser` source.
//!
//! The whole path is compiled in every build so the routine gate exercises it, but it
//! is reachable only in builds with the `browser` feature: without it there is no host
//! mode, no endpoint and no browser registration.

pub mod argv;
pub mod custody;
pub mod host;
pub mod registration;
pub mod server;
pub mod status;

use std::{
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

/// Whether this build carries the browser path.
pub const ENABLED: bool = cfg!(feature = "browser");
/// Whether this build also admits the unpacked development extension. No release
/// target sets it.
pub const DEV_ENABLED: bool = cfg!(feature = "browser-dev-host");

/// The local stream folder that holds finalized browser periods. A stream name must
/// start with a lowercase letter or digit, so this can never collide with one.
pub const STREAM_DIR: &str = "_browser";
/// The journal-side source name for browser text.
pub const SOURCE: &str = "browser";
/// The one payload file of every browser period.
pub const PAGES_FILENAME: &str = "browser_pages.jsonl";

const ENDPOINT_DIR: &str = "solstone-linux";
const ENDPOINT_NAME: &str = "browser-host.sock";

/// The app's endpoint: `$XDG_RUNTIME_DIR/solstone-linux/browser-host.sock`, falling
/// back to the systemd per-user runtime directory when the variable is unset.
pub fn endpoint_path(runtime_dir: Option<OsString>, uid: u32) -> io::Result<PathBuf> {
    let root = runtime_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    let path = root.join(ENDPOINT_DIR).join(ENDPOINT_NAME);
    // sun_path holds 108 bytes including the terminator.
    if path.as_os_str().len() > 107 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "browser endpoint path is too long",
        ));
    }
    Ok(path)
}

pub fn production_endpoint_path() -> io::Result<PathBuf> {
    endpoint_path(
        env::var_os("XDG_RUNTIME_DIR"),
        rustix::process::getuid().as_raw(),
    )
}

/// Where browser custody lives that is not yet a finalized segment.
pub fn custody_root(base_dir: &Path) -> PathBuf {
    base_dir.join("browser")
}

/// Whether a capture segment directory is a browser period.
pub fn is_browser_segment(segment_dir: &Path) -> bool {
    segment_dir
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == STREAM_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_uses_runtime_dir_and_falls_back_to_the_user_runtime() {
        assert_eq!(
            endpoint_path(Some("/run/user/1000".into()), 1000).unwrap(),
            PathBuf::from("/run/user/1000/solstone-linux/browser-host.sock")
        );
        assert_eq!(
            endpoint_path(None, 42).unwrap(),
            PathBuf::from("/run/user/42/solstone-linux/browser-host.sock")
        );
        assert_eq!(
            endpoint_path(Some("relative".into()), 42).unwrap(),
            PathBuf::from("/run/user/42/solstone-linux/browser-host.sock")
        );
        assert!(endpoint_path(Some(format!("/{}", "x".repeat(120)).into()), 1).is_err());
    }

    fn vendored(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor")
            .join(relative)
    }

    /// The vendored contract and frame crate are byte-for-byte the pinned bundle.
    #[test]
    fn the_vendored_browser_contract_is_the_pinned_bundle() {
        use sha2::{Digest, Sha256};
        let sha = |path: &Path| format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()));
        let manifest_path = vendored("contracts/native-browser/manifest.json");
        let adoption: serde_json::Value = serde_json::from_slice(
            &std::fs::read(vendored("contracts/native-browser/adoption.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(adoption["manifest_sha256"], sha(&manifest_path));
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["wire_protocol"],
            native_browser_frame::WIRE_PROTOCOL
        );
        assert_eq!(
            manifest["bundle_version"],
            native_browser_frame::BUNDLE_VERSION
        );
        for (relative, expected) in manifest["artifacts"].as_object().unwrap() {
            assert_eq!(&sha(&vendored(relative)), expected, "{relative} drifted");
        }
    }

    /// The shared vector corpus decodes here exactly as it does for the extension and
    /// the Mac app.
    #[test]
    fn the_shared_vector_corpus_decodes_as_specified() {
        use native_browser_frame::{DecodeOutcome, Direction, decode};
        let corpus: Vec<serde_json::Value> = serde_json::from_slice(
            &std::fs::read(vendored("contracts/native-browser/corpus.json")).unwrap(),
        )
        .unwrap();
        assert!(!corpus.is_empty());
        for vector in &corpus {
            let id = vector["id"].as_str().unwrap();
            let direction = match vector["direction"].as_str().unwrap() {
                "extension_to_host" => Direction::ExtensionToHost,
                _ => Direction::HostToExtension,
            };
            let bytes = vector
                .get("payload")
                .or_else(|| vector.get("raw"))
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .as_bytes();
            match (vector["expect"].as_str().unwrap(), decode(bytes, direction)) {
                ("accept", DecodeOutcome::Accept(_)) => {}
                ("unsupported", DecodeOutcome::Unsupported { behind, .. }) => {
                    if let Some(expected) = vector.get("behind").and_then(|v| v.as_str()) {
                        assert_eq!(behind, expected, "{id}");
                    }
                }
                ("refuse", DecodeOutcome::Refuse(error)) => {
                    if let Some(code) = vector.get("code").and_then(|v| v.as_str()) {
                        assert_eq!(error.code, code, "{id}");
                    }
                }
                (expected, outcome) => panic!("{id}: expected {expected}, got {outcome:?}"),
            }
        }
    }

    #[test]
    fn browser_segments_are_recognised_by_their_stream_folder() {
        assert!(is_browser_segment(Path::new(
            "/c/20261002/_browser/101500_300"
        )));
        assert!(!is_browser_segment(Path::new(
            "/c/20261002/host/101500_300"
        )));
    }
}
