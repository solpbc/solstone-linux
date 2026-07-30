// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fs,
    path::{Path, PathBuf},
};

const L3_MARKER: &str = "L3-CLEANUP(spl-cutover)";
const TEST_ONLY_FILES: &[&str] = &[
    "linked_authority_policy_tests.rs",
    "observer_contract_tests.rs",
    "private_link_test_peer.rs",
    "release_rail_tests.rs",
    "test_support.rs",
    "toolchain_policy_tests.rs",
    "unsafe_policy_tests.rs",
];

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !TEST_ONLY_FILES
                    .iter()
                    .any(|name| path.file_name().is_some_and(|file| file == *name))
            {
                sources.push(path);
            }
        }
    }
    sources.sort();
    sources
}

fn production_prefix(source: &str) -> &str {
    source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(source)
}

fn marked_items(lines: &[&str], marker: &str) -> Vec<bool> {
    let mut marked = vec![false; lines.len()];
    let mut pending = false;
    let mut depth = 0_i64;
    for (index, line) in lines.iter().enumerate() {
        if line.contains(marker) {
            pending = true;
            marked[index] = true;
            continue;
        }
        if depth > 0 {
            marked[index] = true;
            depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
            continue;
        }
        if pending {
            marked[index] = true;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("#[") {
                continue;
            }
            depth = line.matches('{').count() as i64 - line.matches('}').count() as i64;
            pending = depth == 0 && !line.contains(';');
        }
    }
    marked
}

fn constructs_origin_relative_url(line: &str) -> bool {
    let journal_route = line.contains("/app/") || line.contains("/api/");
    journal_route
        && (line.contains("format!(") || line.contains(".join(") || line.contains("push_str("))
}

#[test]
fn active_production_paths_have_no_legacy_direct_authority() {
    let root = source_root();
    let forbidden = [".bearer_auth(", "localhost:5015", "Command::new(\"sol\")"];

    for path in rust_sources(&root) {
        if path
            .file_name()
            .is_some_and(|name| name == "chat_bridge.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        let lines = production_prefix(&source).lines().collect::<Vec<_>>();
        let l3_marked = marked_items(&lines, L3_MARKER);
        let test_only = marked_items(&lines, "#[cfg(test)]");
        for (index, line) in lines.iter().enumerate() {
            assert!(
                !constructs_origin_relative_url(line)
                    || path
                        .file_name()
                        .is_some_and(|name| name == "private_link.rs")
                    || l3_marked[index]
                    || test_only[index],
                "{}:{} constructs an origin-relative Journal URL",
                path.display(),
                index + 1
            );
            for needle in forbidden {
                assert!(
                    !line.contains(needle) || l3_marked[index] || test_only[index],
                    "{}:{} contains active legacy authority `{needle}`",
                    path.display(),
                    index + 1
                );
            }
            for needle in [".server_url", "config.key"] {
                assert!(
                    !line.contains(needle) || l3_marked[index] || test_only[index],
                    "{}:{} reads configured legacy authority `{needle}`",
                    path.display(),
                    index + 1
                );
            }
        }
    }
}

#[test]
fn l3_authority_is_confined_to_explicit_unreachable_surfaces() {
    let root = source_root();
    for module in ["chat_bridge.rs", "config.rs"] {
        let source = fs::read_to_string(root.join(module)).unwrap();
        assert!(source.contains(L3_MARKER), "{module} lacks the L3 marker");
    }
}
