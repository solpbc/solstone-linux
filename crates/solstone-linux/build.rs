// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("../../contrib/icons/hicolor/scalable/status");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default()).join("tray_icons.rs");
    let mut generated = String::new();
    for (constant, file) in [
        ("RECORDING", "solstone-recording.svg"),
        ("PAUSED", "solstone-paused.svg"),
        ("SYNCING", "solstone-syncing.svg"),
        ("ERROR", "solstone-error.svg"),
    ] {
        let path = root.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(bytes) = fs::read(&path) else {
            panic!("cannot read canonical tray icon {}", path.display())
        };
        let Ok(tree) = resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default())
        else {
            panic!("invalid canonical tray icon {}", path.display())
        };
        let mut pixmap = resvg::tiny_skia::Pixmap::new(64, 64)
            .unwrap_or_else(|| panic!("cannot allocate tray icon pixmap"));
        let size = tree.size();
        let scale = (64.0 / size.width()).min(64.0 / size.height());
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let mut argb = Vec::with_capacity(64 * 64 * 4);
        for rgba in pixmap.data().chunks_exact(4) {
            argb.extend_from_slice(&[rgba[3], rgba[0], rgba[1], rgba[2]]);
        }
        generated.push_str(&format!("pub static {constant}: &[u8] = &{argb:?};\n"));
    }
    if let Err(error) = fs::write(out, generated) {
        panic!("cannot write generated tray icons: {error}");
    }
}
