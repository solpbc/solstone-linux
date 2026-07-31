// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub const LEGACY_ENVIRONMENT: &[&str] = &["SOLSTONE_TOKEN"];
pub const LEGACY_OPTIONS: &[&str] = &["--server-url", "--token"];
pub const LEGACY_ORIGINS: &[&str] = &["localhost:5015", "127.0.0.1:5015", "[::1]:5015"];
pub const LEGACY_COMMANDS: &[&str] = &["journal observer create"];
pub const LEGACY_EXECUTABLES: &[&str] = &["sol"];
pub const PYTHON_SETUP: &[&str] = &[
    "python -m",
    "python3 -m",
    "pip install",
    "pip3 install",
    "pipx install",
];
