// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Hand the local journal's first run to its own commands and browser page.

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
    run_with(|args, timeout| {
        // Non-interactive setup keeps its own admission checks and step deadlines.
        // Never accept an existing journal on the owner's behalf or read window stdin.
        solstone_linux::bounded_process::run(
            Command::new(&program).args(args).stdin(Stdio::null()),
            timeout,
            64 * 1024,
        )
        .is_ok_and(|output| output.status.success() && !output.timed_out)
    })
}

fn run_with(mut run: impl FnMut(&[&str], Option<Duration>) -> bool) -> Outcome {
    if !run(&["journal", "setup"], None) {
        return Outcome::SetupFailed;
    }
    // The journal mints its own one-time browser link. A bare /init address cannot
    // open a closed journal, and successful preparation isn't completed setup.
    if run(&["journal", "open", "/init"], Some(Duration::from_secs(30))) {
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
        let mut calls = Vec::new();
        let outcome = run_with(|args, _| {
            calls.push(args.join(" "));
            false
        });
        assert_eq!(outcome, Outcome::SetupFailed);
        assert_eq!(calls, ["journal setup"]);
    }

    #[test]
    fn browser_failure_is_distinct_from_preparation_failure() {
        let mut calls = Vec::new();
        let outcome = run_with(|args, _| {
            calls.push(args.join(" "));
            calls.len() == 1
        });
        assert_eq!(outcome, Outcome::OpenFailed);
        assert_eq!(calls, ["journal setup", "journal open /init"]);
        assert_eq!(run_with(|_, _| true), Outcome::Opened);
    }
}
