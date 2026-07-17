// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => println!("solstone-linux {}", env!("CARGO_PKG_VERSION")),
        _ => println!("solstone-linux Rust rebuild stub; Python remains the shipped observer"),
    }
}
