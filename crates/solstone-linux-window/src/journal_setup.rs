// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Hand the local journal's first run to its own commands and browser page.

use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::Duration;

use solstone_linux::cli::SystemRunner;
use solstone_linux::session_env::Runner;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Opened,
    NotInstalled,
    SetupFailed,
    OpenFailed,
}

pub fn run() -> Outcome {
    let Some(program) = SystemRunner.which("solstone") else {
        return Outcome::NotInstalled;
    };
    run_with(
        // Non-interactive setup retains its admission checks and step deadlines.
        || command(OsStr::new(&program), &["journal", "setup"], None).is_some(),
        || {
            // The journal mints its one-time link; the app's existing browser
            // helper handles it, so first run gets the same Konqueror repair.
            let Some(output) = command(
                OsStr::new(&program),
                &["journal", "open", "--print-url", "/init"],
                Some(Duration::from_secs(30)),
            ) else {
                return false;
            };
            let Some(target) = browser_target(&output.stdout) else {
                return false;
            };
            let Ok(window) = std::env::current_exe() else {
                return false;
            };
            let Some(directory) = window.parent() else {
                return false;
            };
            // Both executables ship beside one another in every package. Keep
            // the helper on this window's build instead of searching PATH.
            command(
                directory.join("solstone-linux").as_os_str(),
                &["open-journal-in-scope", target],
                Some(Duration::from_secs(30)),
            )
            .is_some()
        },
    )
}

fn command(
    program: &OsStr,
    args: &[&str],
    timeout: Option<Duration>,
) -> Option<solstone_linux::bounded_process::Output> {
    solstone_linux::bounded_process::run(
        Command::new(program).args(args).stdin(Stdio::null()),
        timeout,
        64 * 1024,
    )
    .ok()
    .filter(|output| output.status.success() && !output.timed_out)
}

fn browser_target(output: &[u8]) -> Option<&str> {
    let target = std::str::from_utf8(output).ok()?.trim();
    if target
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return None;
    }
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))?;
    // This is framing for the mint command's single output line, not a URL
    // constructor: retain every opaque byte for the existing browser helper.
    (!rest.is_empty() && !rest.starts_with(['/', '?', '#'])).then_some(target)
}

fn run_with(mut prepare: impl FnMut() -> bool, mut open: impl FnMut() -> bool) -> Outcome {
    if !prepare() {
        return Outcome::SetupFailed;
    }
    // The journal mints its own one-time browser link. A bare /init address cannot
    // open a closed journal, and successful preparation isn't completed setup.
    if open() {
        Outcome::Opened
    } else {
        Outcome::OpenFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_preparation_never_opens_a_journal() {
        let mut opened = false;
        let outcome = run_with(
            || false,
            || {
                opened = true;
                true
            },
        );
        assert_eq!(outcome, Outcome::SetupFailed);
        assert!(!opened);
    }

    #[test]
    fn browser_failure_is_distinct_from_preparation_failure() {
        let outcome = run_with(|| true, || false);
        assert_eq!(outcome, Outcome::OpenFailed);
        assert_eq!(run_with(|| true, || true), Outcome::Opened);
    }

    #[test]
    fn only_one_browser_address_leaves_the_mint_command() {
        let target = "http://localhost:5015/once?value=a%2Bb%26c#init";
        assert_eq!(
            browser_target(format!("{target}\n").as_bytes()),
            Some(target)
        );
        for invalid in [
            b"".as_slice(),
            b"file:///tmp/page",
            b"mailto:owner@example.invalid",
            b"https://example.invalid/\nextra",
            b"not a URL",
            b"\xff",
        ] {
            assert!(browser_target(invalid).is_none());
        }
    }
}
