// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Linux clipboard subprocess dispatch.
//! `arboard` is intentionally not used: its Windows-only BSL-1.0 dependencies fail the
//! project license policy, and it cannot provide Mutter's missing data-control protocol.

use std::{
    io::{self, Write},
    process::{Command, Stdio},
};

pub const AGENT_INSTRUCTIONS: &str = "solstone app for linux (repo: solstone-linux)\nSource: https://github.com/solpbc/solstone-linux\nRead INSTALL.md at https://github.com/solpbc/solstone-linux/blob/main/INSTALL.md for setup and architecture.\nConfig: {config_path}\nSegments: {captures_dir}\nLogs: journalctl --user -u solstone-linux -f\nService: systemctl --user status solstone-linux";

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

fn dispatch(
    text: &str,
    wayland: bool,
    mut spawn: impl FnMut(&str, &[&str], &str) -> io::Result<bool>,
    mut log: impl FnMut(&str),
) -> io::Result<bool> {
    let primary = if wayland {
        spawn("wl-copy", &[], text)
    } else {
        spawn("xclip", &["-selection", "clipboard"], text)
    };
    match primary {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match spawn("xsel", &["--clipboard", "--input"], text) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    log("No clipboard tool found (wl-copy, xclip, or xsel)");
                    Err(error)
                }
                result => result,
            }
        }
        result => result,
    }
}

pub fn copy(text: &str, wayland: bool) -> io::Result<bool> {
    dispatch(text, wayland, invoke, |message| {
        tracing::error!("{message}")
    })
}

pub fn is_wayland(
    session_type: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
) -> bool {
    session_type.is_some_and(|value| value == "wayland") || display.is_some()
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
        assert_eq!(
            s,
            "solstone app for linux (repo: solstone-linux)\nSource: https://github.com/solpbc/solstone-linux\nRead INSTALL.md at https://github.com/solpbc/solstone-linux/blob/main/INSTALL.md for setup and architecture.\nConfig: /c/config.json\nSegments: /d/captures\nLogs: journalctl --user -u solstone-linux -f\nService: systemctl --user status solstone-linux"
        );
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Call(String, Vec<String>);
    fn run(
        wayland: bool,
        outcomes: Vec<io::Result<bool>>,
    ) -> (io::Result<bool>, Vec<Call>, Vec<String>) {
        let mut outcomes = outcomes.into_iter();
        let mut calls = Vec::new();
        let mut logs = Vec::new();
        let result = dispatch(
            "text",
            wayland,
            |program, args, _| {
                calls.push(Call(
                    program.into(),
                    args.iter().map(|v| (*v).into()).collect(),
                ));
                outcomes
                    .next()
                    .unwrap_or_else(|| Err(io::Error::other("missing outcome")))
            },
            |message| logs.push(message.into()),
        );
        (result, calls, logs)
    }
    #[test]
    fn wayland_dispatches_to_wl_copy() {
        let (_, calls, _) = run(true, vec![Ok(true)]);
        assert_eq!(calls, [Call("wl-copy".into(), vec![])]);
    }
    #[test]
    fn x11_dispatches_to_xclip_with_selection() {
        let (_, calls, _) = run(false, vec![Ok(true)]);
        assert_eq!(
            calls,
            [Call(
                "xclip".into(),
                vec!["-selection".into(), "clipboard".into()]
            )]
        );
    }
    #[test]
    fn either_missing_primary_falls_back_to_xsel() {
        for wayland in [true, false] {
            let (_, calls, _) = run(
                wayland,
                vec![Err(io::Error::from(io::ErrorKind::NotFound)), Ok(true)],
            );
            assert_eq!(calls.last().map(|c| c.0.as_str()), Some("xsel"));
            assert_eq!(
                calls.last().map(|c| c.1.clone()),
                Some(vec!["--clipboard".into(), "--input".into()])
            );
        }
    }
    #[test]
    fn nonzero_exit_does_not_fall_through() {
        let (result, calls, _) = run(true, vec![Ok(false)]);
        assert_eq!(result.ok(), Some(false));
        assert_eq!(calls.len(), 1);
    }
    #[test]
    fn final_missing_tool_logs_guidance() {
        let (_, _, logs) = run(
            true,
            vec![
                Err(io::Error::from(io::ErrorKind::NotFound)),
                Err(io::Error::from(io::ErrorKind::NotFound)),
            ],
        );
        assert_eq!(logs, ["No clipboard tool found (wl-copy, xclip, or xsel)"]);
    }
    #[test]
    fn session_detection_uses_type_or_display() {
        use std::ffi::OsStr;
        assert!(is_wayland(Some(OsStr::new("wayland")), None));
        assert!(is_wayland(None, Some(OsStr::new("wayland-0"))));
        assert!(!is_wayland(Some(OsStr::new("x11")), None));
    }
}
