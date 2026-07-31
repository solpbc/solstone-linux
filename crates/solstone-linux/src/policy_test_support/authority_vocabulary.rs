// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub(crate) const LEGACY_ENVIRONMENT: &[&str] = &["SOLSTONE_TOKEN"];
pub(crate) const LEGACY_OPTIONS: &[&str] = &["--server-url", "--token"];
pub(crate) const LEGACY_ORIGINS: &[&str] = &["localhost:5015", "127.0.0.1:5015", "[::1]:5015"];
pub(crate) const LEGACY_COMMANDS: &[&str] = &["journal observer create"];
pub(crate) const LEGACY_EXECUTABLES: &[&str] = &["sol"];
pub(crate) const PYTHON_SETUP: &[&str] = &[
    "python -m",
    "python3 -m",
    "pip install",
    "pip3 install",
    "pipx install",
];
