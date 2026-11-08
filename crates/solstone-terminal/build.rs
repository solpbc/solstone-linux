// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

fn main() {
    println!("cargo:rerun-if-env-changed=SOLSTONE_TERMINAL_SOURCE_COMMIT");
    let label = match std::env::var("SOLSTONE_TERMINAL_SOURCE_COMMIT") {
        Ok(commit) => {
            if commit.len() == 40 && commit.chars().all(|c| c.is_ascii_hexdigit()) {
                commit
            } else {
                panic!("SOLSTONE_TERMINAL_SOURCE_COMMIT must be unset or exactly 40 hex digits");
            }
        }
        Err(std::env::VarError::NotPresent) => "development".to_string(),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("SOLSTONE_TERMINAL_SOURCE_COMMIT must be valid unicode");
        }
    };
    println!("cargo:rustc-env=SOLSTONE_TERMINAL_SOURCE_LABEL={label}");
}
