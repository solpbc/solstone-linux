// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Linux clipboard subprocess dispatch.
//! `arboard` is intentionally not used: its Windows-only BSL-1.0 dependencies fail the
//! project license policy, and it cannot provide Mutter's missing data-control protocol.

use std::{
    io::{self, Write},
    process::{Command, Stdio},
};

pub const AGENT_INSTRUCTIONS: &str = "sol for Linux (repo: solstone-linux)\nSource: https://github.com/solpbc/solstone-linux\nRead INSTALL.md at https://github.com/solpbc/solstone-linux/blob/main/INSTALL.md for setup and architecture.\nConfig: {config_path}\nCaptures: {captures_dir}\nLogs: journalctl --user -u solstone-linux -f\nService: systemctl --user status solstone-linux";

fn invoke(program: &str, args: &[&str], text: &str) -> io::Result<bool> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()?;
    if let Some(mut input) = child.stdin.take() {
        input.write_all(text.as_bytes())?;
    }
    Ok(child.wait()?.success())
}

pub fn copy(text: &str, wayland: bool) -> io::Result<bool> {
    if wayland {
        return invoke("wl-copy", &[], text);
    }
    match invoke("xclip", &["-selection", "clipboard"], text) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            invoke("xsel", &["--clipboard", "--input"], text)
        }
        result => result,
    }
}

pub fn agent_instructions(config_path: &str, captures_dir: &str) -> String {
    AGENT_INSTRUCTIONS
        .replace("{config_path}", config_path)
        .replace("{captures_dir}", captures_dir)
}

// Python clipboard provenance (1/1):
// tests/test_tray.py::test_agent_instructions_template_uses_config_values
//   -> clipboard::tests::instructions_use_config_values.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instructions_use_config_values() {
        let s = agent_instructions("/c/config.json", "/d/captures");
        assert!(s.contains("Config: /c/config.json"));
        assert!(s.contains("Captures: /d/captures"));
        assert!(s.contains("Source: https://github.com/solpbc/solstone-linux"));
    }
}
