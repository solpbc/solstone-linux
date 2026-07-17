// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{collections::HashMap, io, time::Duration};

pub const EXIT_TEMPFAIL: i32 = 75;
const SESSION_TIMEOUT: Duration = Duration::from_secs(5);
const NEEDED: [&str; 3] = ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"];

#[derive(Clone, Debug)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
}

pub trait Runner {
    fn which(&self, program: &str) -> Option<String>;
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> io::Result<Output>;
}

fn missing(environment: &HashMap<String, String>, name: &str) -> bool {
    environment.get(name).is_none_or(String::is_empty)
}

pub fn recover_session_env(
    environment: &mut HashMap<String, String>,
    uid: u32,
    runner: &dyn Runner,
) {
    let missing_names: Vec<_> = NEEDED
        .into_iter()
        .filter(|name| missing(environment, name))
        .collect();
    if missing_names.is_empty() {
        return;
    }

    environment
        .entry("XDG_RUNTIME_DIR".into())
        .or_insert_with(|| format!("/run/user/{uid}"));
    let Ok(output) = runner.run(
        "systemctl",
        &["--user", "show-environment"],
        SESSION_TIMEOUT,
    ) else {
        return;
    };
    if !output.success {
        return;
    }

    for line in output.stdout.lines() {
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        if missing_names.contains(&key) && !value.is_empty() {
            environment.insert(key.into(), value.into());
        }
    }
}

pub fn check_session_ready(
    environment: &HashMap<String, String>,
    runner: &dyn Runner,
) -> Option<&'static str> {
    if missing(environment, "DISPLAY") && missing(environment, "WAYLAND_DISPLAY") {
        return Some("no display server (DISPLAY/WAYLAND_DISPLAY not set)");
    }
    if missing(environment, "DBUS_SESSION_BUS_ADDRESS") {
        return Some("no DBus session bus (DBUS_SESSION_BUS_ADDRESS not set)");
    }
    if let Some(pactl) = runner.which("pactl")
        && !runner
            .run(&pactl, &["info"], SESSION_TIMEOUT)
            .is_ok_and(|output| output.success)
    {
        return Some("audio server not responding (pactl info failed)");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };

    struct FakeRunner {
        found: Option<String>,
        results: RefCell<VecDeque<io::Result<Output>>>,
        calls: Cell<usize>,
    }

    impl FakeRunner {
        fn new(found: Option<&str>, results: Vec<io::Result<Output>>) -> Self {
            Self {
                found: found.map(str::to_owned),
                results: RefCell::new(results.into()),
                calls: Cell::new(0),
            }
        }
        fn success(stdout: &str) -> Self {
            Self::new(
                None,
                vec![Ok(Output {
                    success: true,
                    stdout: stdout.into(),
                })],
            )
        }
    }

    impl Runner for FakeRunner {
        fn which(&self, _: &str) -> Option<String> {
            self.found.clone()
        }
        fn run(&self, _: &str, _: &[&str], _: Duration) -> io::Result<Output> {
            self.calls.set(self.calls.get() + 1);
            self.results
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::NotFound, "missing")))
        }
    }

    fn ready_env() -> HashMap<String, String> {
        HashMap::from([
            ("DISPLAY".into(), ":0".into()),
            ("DBUS_SESSION_BUS_ADDRESS".into(), "unix:path=/bus".into()),
        ])
    }

    // tests/test_session_env.py::TestCheckSessionReady::test_no_display_server
    #[test]
    fn no_display_server() {
        let env = HashMap::from([("DBUS_SESSION_BUS_ADDRESS".into(), "bus".into())]);
        assert!(
            check_session_ready(&env, &FakeRunner::new(None, vec![]))
                .unwrap()
                .contains("display server")
        );
    }
    // tests/test_session_env.py::TestCheckSessionReady::test_no_dbus
    #[test]
    fn no_dbus() {
        let env = HashMap::from([("DISPLAY".into(), ":0".into())]);
        assert!(
            check_session_ready(&env, &FakeRunner::new(None, vec![]))
                .unwrap()
                .contains("DBus")
        );
    }
    // tests/test_session_env.py::TestCheckSessionReady::test_ready_with_display_and_dbus
    #[test]
    fn ready_without_pactl() {
        assert_eq!(
            check_session_ready(&ready_env(), &FakeRunner::new(None, vec![])),
            None
        );
    }
    // AC: only missing and empty values are recovered; existing values win.
    #[test]
    fn recovers_only_missing() {
        let mut env = HashMap::from([
            ("DISPLAY".into(), ":existing".into()),
            ("WAYLAND_DISPLAY".into(), String::new()),
        ]);
        let runner = FakeRunner::success(
            "DISPLAY=:new\nWAYLAND_DISPLAY=wayland-0\nDBUS_SESSION_BUS_ADDRESS=bus\n",
        );
        recover_session_env(&mut env, 1000, &runner);
        assert_eq!(env["DISPLAY"], ":existing");
        assert_eq!(env["WAYLAND_DISPLAY"], "wayland-0");
        assert_eq!(env["DBUS_SESSION_BUS_ADDRESS"], "bus");
    }
    // AC: empty systemctl values are not assigned.
    #[test]
    fn ignores_empty_output_value() {
        let mut env = HashMap::new();
        recover_session_env(&mut env, 1000, &FakeRunner::success("DISPLAY=\n"));
        assert!(!env.contains_key("DISPLAY"));
    }
    // AC: all-present environment short-circuits without systemctl.
    #[test]
    fn all_present_short_circuits() {
        let mut env = HashMap::from([
            ("DISPLAY".into(), ":0".into()),
            ("WAYLAND_DISPLAY".into(), "wayland".into()),
            ("DBUS_SESSION_BUS_ADDRESS".into(), "bus".into()),
        ]);
        let runner = FakeRunner::new(None, vec![]);
        recover_session_env(&mut env, 1000, &runner);
        assert_eq!(runner.calls.get(), 0);
    }
    // AC: runtime directory is synthesized before systemctl and never overwritten.
    #[test]
    fn runtime_dir_fallback() {
        let mut env = HashMap::new();
        recover_session_env(&mut env, 1234, &FakeRunner::success(""));
        assert_eq!(env["XDG_RUNTIME_DIR"], "/run/user/1234");
        env.insert("XDG_RUNTIME_DIR".into(), "/custom".into());
        recover_session_env(&mut env, 999, &FakeRunner::success(""));
        assert_eq!(env["XDG_RUNTIME_DIR"], "/custom");
    }
    // AC: non-zero systemctl status is a silent recovery failure.
    #[test]
    fn systemctl_nonzero() {
        let mut env = HashMap::new();
        recover_session_env(
            &mut env,
            1,
            &FakeRunner::new(
                None,
                vec![Ok(Output {
                    success: false,
                    stdout: "DISPLAY=:0".into(),
                })],
            ),
        );
        assert!(!env.contains_key("DISPLAY"));
    }
    // AC: missing systemctl binary is a silent recovery failure.
    #[test]
    fn systemctl_missing() {
        let mut env = HashMap::new();
        recover_session_env(
            &mut env,
            1,
            &FakeRunner::new(
                None,
                vec![Err(io::Error::new(io::ErrorKind::NotFound, "missing"))],
            ),
        );
        assert!(!env.contains_key("DISPLAY"));
    }
    // AC: systemctl timeout is a silent recovery failure.
    #[test]
    fn systemctl_timeout() {
        let mut env = HashMap::new();
        recover_session_env(
            &mut env,
            1,
            &FakeRunner::new(
                None,
                vec![Err(io::Error::new(io::ErrorKind::TimedOut, "timeout"))],
            ),
        );
        assert!(!env.contains_key("DISPLAY"));
    }
    // AC: pactl failure reports the audio readiness message.
    #[test]
    fn pactl_failure() {
        let runner = FakeRunner::new(
            Some("/bin/pactl"),
            vec![Ok(Output {
                success: false,
                stdout: String::new(),
            })],
        );
        assert_eq!(
            check_session_ready(&ready_env(), &runner),
            Some("audio server not responding (pactl info failed)")
        );
    }
    // AC: pactl timeout reports the same audio readiness message.
    #[test]
    fn pactl_timeout() {
        let runner = FakeRunner::new(
            Some("/bin/pactl"),
            vec![Err(io::Error::new(io::ErrorKind::TimedOut, "timeout"))],
        );
        assert_eq!(
            check_session_ready(&ready_env(), &runner),
            Some("audio server not responding (pactl info failed)")
        );
    }
}
