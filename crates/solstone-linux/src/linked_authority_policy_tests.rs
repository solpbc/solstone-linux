// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{fs, path::PathBuf};

const L3_MARKER: &str = "L3-CLEANUP(spl-cutover)";

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn production_prefix(source: &str) -> &str {
    source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(source)
}

#[test]
fn active_production_paths_have_no_legacy_direct_authority() {
    let root = source_root();
    let active_modules = [
        "cli.rs",
        "event_sender.rs",
        "observer.rs",
        "private_link.rs",
        "recovery.rs",
        "run.rs",
        "sync.rs",
        "sync_health.rs",
        "upload.rs",
    ];
    let forbidden = [
        ".bearer_auth(",
        "localhost:5015",
        "Command::new(\"sol\")",
        "Command::new(\"sol\"",
    ];

    for module in active_modules {
        let source = fs::read_to_string(root.join(module)).unwrap();
        let production = production_prefix(&source);
        let lines = production.lines().collect::<Vec<_>>();
        for needle in forbidden {
            for (index, line) in lines.iter().enumerate() {
                if line.contains(needle) {
                    let nearby = lines[..=index].iter().rev().take(16).any(|candidate| {
                        candidate.contains("#[cfg(test)]") || candidate.contains(L3_MARKER)
                    });
                    assert!(
                        nearby,
                        "{module}:{} contains active legacy authority `{needle}`",
                        index + 1
                    );
                }
            }
        }
    }

    for module in ["cli.rs", "run.rs", "sync.rs", "upload.rs"] {
        let source = fs::read_to_string(root.join(module)).unwrap();
        let production = production_prefix(&source);
        for needle in ["config.server_url", "config.key"] {
            let lines = production.lines().collect::<Vec<_>>();
            for (index, line) in lines.iter().enumerate() {
                if line.contains(needle) {
                    assert!(
                        lines[..=index]
                            .iter()
                            .rev()
                            .take(32)
                            .any(|candidate| candidate.contains("#[cfg(test)]")),
                        "{module}:{} reads configured legacy authority `{needle}`",
                        index + 1
                    );
                }
            }
        }
    }
}

#[test]
fn l3_authority_is_confined_to_explicit_unreachable_surfaces() {
    let root = source_root();
    for module in [
        "chat_bridge.rs",
        "cli.rs",
        "config.rs",
        "dbus_service.rs",
        "desktop_component.rs",
    ] {
        let source = fs::read_to_string(root.join(module)).unwrap();
        assert!(source.contains(L3_MARKER), "{module} lacks the L3 marker");
    }
}
