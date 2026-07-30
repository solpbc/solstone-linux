// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Native prerequisite checks. The Python doctor had 12 checks; Rust has eight.
//! Retired Python-install checks are `check_python_version`, `check_gtk4_typelib`,
//! `check_cairo`, and `check_pipx`. The external `xrandr` binary mechanism is
//! retired, while its "cannot query RandR" failure behavior is ported through
//! x11rb. The `gst-inspect-1.0` probe is retired in favor of the in-process registry.

use crate::{
    audio::pulse,
    capture_stats::{compute_quarantine_stats, format_quarantine_line},
    config::{Config, ConfigPaths, load_config},
    session_env::{Output, Runner},
    sync_health::{SyncHealth, derive_health, load_facts},
    video::{
        gstreamer::ensure_initialized,
        x11::{RandrOutputProvider, X11OutputProvider},
    },
};
use gstreamer as gst;
use std::{
    collections::HashMap,
    env, io,
    panic::{AssertUnwindSafe, catch_unwind},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PORTAL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Fail,
}
impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub name: &'static str,
    pub severity: Severity,
    pub detail: String,
}
impl CheckResult {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            severity: Severity::Ok,
            detail: detail.into(),
        }
    }
    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            severity: Severity::Warn,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            severity: Severity::Fail,
            detail: detail.into(),
        }
    }
}

pub trait DoctorChecks {
    fn session_type(&mut self) -> CheckResult;
    fn portal(&mut self) -> CheckResult;
    fn pulse(&mut self) -> CheckResult;
    fn gstreamer(&mut self) -> CheckResult;
    fn x11(&mut self) -> CheckResult;
    fn systemd(&mut self) -> CheckResult;
    fn appindicator(&mut self) -> CheckResult;
    fn sync_health(&mut self) -> CheckResult;
    fn quarantine(&mut self) -> Option<String>;
}

pub fn run_doctor(checks: &mut dyn DoctorChecks, output: &mut dyn io::Write) -> i32 {
    let names = [
        "session type",
        "xdg-desktop-portal",
        "pipewire (pulse)",
        "gstreamer",
        "x11 capture",
        "systemd --user",
        "appindicator ext (soft)",
        "sync health",
    ];
    let functions: [fn(&mut dyn DoctorChecks) -> CheckResult; 8] = [
        |v| v.session_type(),
        |v| v.portal(),
        |v| v.pulse(),
        |v| v.gstreamer(),
        |v| v.x11(),
        |v| v.systemd(),
        |v| v.appindicator(),
        |v| v.sync_health(),
    ];
    let mut failures = 0;
    let mut warnings = 0;
    for (name, function) in names.into_iter().zip(functions) {
        let result = catch_unwind(AssertUnwindSafe(|| function(checks))).unwrap_or_else(|_| {
            CheckResult::fail(name, "probe panicked; inspect service logs and retry")
        });
        let result = if result.name == name {
            result
        } else {
            CheckResult { name, ..result }
        };
        let _ = writeln!(
            output,
            "{:<4}  {:<28}  {}",
            result.severity.label(),
            result.name,
            result.detail
        );
        match result.severity {
            Severity::Fail => failures += 1,
            Severity::Warn => warnings += 1,
            Severity::Ok => {}
        }
    }
    if let Some(line) = checks.quarantine() {
        let _ = writeln!(output, "{line}");
    }
    let _ = writeln!(
        output,
        "\ndoctor: 8 checks, {failures} failed, {warnings} warnings"
    );
    if failures == 0 { 0 } else { 1 }
}

pub struct RealDoctor<'a> {
    pub runner: &'a dyn Runner,
    config: Option<Config>,
}
impl<'a> RealDoctor<'a> {
    pub fn new(runner: &'a dyn Runner) -> Self {
        Self {
            runner,
            config: None,
        }
    }
    fn config(&mut self) -> &Config {
        self.config
            .get_or_insert_with(|| load_config(ConfigPaths::default()).config)
    }
}

fn session_type_result(value: Option<&str>) -> CheckResult {
    let value = value.unwrap_or_default().to_lowercase();
    match value.as_str() {
        "wayland" => CheckResult::ok("session type", "wayland"),
        "x11" => CheckResult::ok("session type", "x11 (using ximagesrc capture)"),
        "" => CheckResult::warn(
            "session type",
            "XDG_SESSION_TYPE not set; Wayland or X11 required",
        ),
        _ => CheckResult::warn(
            "session type",
            format!("unrecognized session type '{value}'; Wayland or X11 required"),
        ),
    }
}

fn portal_result(result: Result<bool, String>, x11: bool) -> CheckResult {
    match result {
        Ok(true) => CheckResult::ok(
            "xdg-desktop-portal",
            "org.freedesktop.portal.Desktop registered on session bus",
        ),
        Ok(false) if x11 => CheckResult::warn(
            "xdg-desktop-portal",
            "not registered — not needed on X11 (using ximagesrc)",
        ),
        Ok(false) => CheckResult::fail(
            "xdg-desktop-portal",
            "org.freedesktop.portal.Desktop not registered on session bus; install/start xdg-desktop-portal",
        ),
        Err(error) => CheckResult::fail(
            "xdg-desktop-portal",
            format!(
                "session bus unreachable: {error}; start a graphical session and ensure xdg-desktop-portal is installed"
            ),
        ),
    }
}

fn appindicator_result(desktop: &str, output: Option<&Output>) -> CheckResult {
    if !desktop.contains("GNOME") {
        return CheckResult::ok(
            "appindicator ext (soft)",
            "not applicable (non-GNOME desktop)",
        );
    }
    match output {
        Some(value) if value.success && value.stdout.to_lowercase().contains("appindicator") => {
            CheckResult::ok("appindicator ext (soft)", "appindicator extension present")
        }
        _ => CheckResult::warn(
            "appindicator ext (soft)",
            "install gnome-shell-extension-appindicator",
        ),
    }
}

fn sync_health_result(health: SyncHealth) -> CheckResult {
    let severity = match health.doctor_severity.as_str() {
        "ok" => Severity::Ok,
        "warn" => Severity::Warn,
        _ => Severity::Fail,
    };
    CheckResult {
        name: "sync health",
        severity,
        detail: health.doctor_detail,
    }
}

async fn portal_owned() -> Result<bool, String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    let proxy = zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|error| error.to_string())?;
    let name = zbus::names::BusName::try_from("org.freedesktop.portal.Desktop")
        .map_err(|error| error.to_string())?;
    proxy
        .name_has_owner(name)
        .await
        .map_err(|error| error.to_string())
}

async fn portal_with_timeout<F>(future: F, timeout: Duration) -> Result<Result<bool, String>, ()>
where
    F: std::future::Future<Output = Result<bool, String>>,
{
    tokio::time::timeout(timeout, future).await.map_err(|_| ())
}

fn element_check(names: &[&str]) -> Result<Vec<String>, String> {
    ensure_initialized()?;
    Ok(names
        .iter()
        .filter(|name| gst::ElementFactory::find(name).is_none())
        .map(|name| (*name).to_owned())
        .collect())
}

fn gstreamer_result(missing: Result<Vec<String>, String>) -> CheckResult {
    match missing {
        Ok(missing) if missing.is_empty() => {
            CheckResult::ok("gstreamer", "pipewiresrc, vp8enc, and webmmux available")
        }
        Ok(missing) => CheckResult::fail(
            "gstreamer",
            format!(
                "GStreamer element(s) {} missing; install the PipeWire and good/base plugin packages",
                missing.join(", ")
            ),
        ),
        Err(error) => CheckResult::fail(
            "gstreamer",
            format!("GStreamer initialization failed: {error}; install GStreamer 1.x"),
        ),
    }
}

fn pulse_result(result: Result<(), String>) -> CheckResult {
    match result {
        Ok(()) => CheckResult::ok("pipewire (pulse)", "PulseAudio-compatible server reachable"),
        Err(error) => CheckResult::fail(
            "pipewire (pulse)",
            format!(
                "PulseAudio-compatible server unreachable: {error}; start PipeWire Pulse or PulseAudio"
            ),
        ),
    }
}

fn systemd_result(result: Result<Option<String>, String>) -> CheckResult {
    match result {
        Ok(Some(detail)) => CheckResult::ok("systemd --user", detail),
        Ok(None) => CheckResult::fail(
            "systemd --user",
            "systemctl --user not reachable; run inside a systemd user session",
        ),
        Err(error) => CheckResult::fail(
            "systemd --user",
            format!("systemctl --user not reachable: {error}; run inside a systemd user session"),
        ),
    }
}

fn x11_result(
    session: &str,
    display: Option<&str>,
    randr: Result<(), String>,
    missing: Result<Vec<String>, String>,
) -> CheckResult {
    if session.eq_ignore_ascii_case("wayland") {
        return CheckResult::ok("x11 capture", "not applicable (wayland session)");
    }
    if display.is_none() {
        return if session.eq_ignore_ascii_case("x11") {
            CheckResult::fail(
                "x11 capture",
                "DISPLAY not set; start solstone-linux from the X11 graphical session",
            )
        } else {
            CheckResult::ok("x11 capture", "not applicable (no X11 display)")
        };
    }
    if let Err(error) = randr {
        return CheckResult::fail(
            "x11 capture",
            format!("X11 RandR unavailable: {error}; check DISPLAY and X server RandR support"),
        );
    }
    // Named deviation: in-process registry probing cannot produce Python's gst-inspect warning.
    match missing {
        Ok(missing) if missing.is_empty() => {
            CheckResult::ok("x11 capture", "X11 RandR and ximagesrc available")
        }
        Ok(_) => CheckResult::fail(
            "x11 capture",
            "ximagesrc missing; install the GStreamer good plugins package",
        ),
        Err(error) => CheckResult::fail(
            "x11 capture",
            format!("GStreamer initialization failed: {error}; install GStreamer 1.x"),
        ),
    }
}

fn display_value(value: Option<String>) -> Option<String> {
    value.filter(|display| !display.is_empty())
}

impl DoctorChecks for RealDoctor<'_> {
    fn session_type(&mut self) -> CheckResult {
        session_type_result(env::var("XDG_SESSION_TYPE").ok().as_deref())
    }
    fn portal(&mut self) -> CheckResult {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(value) => value,
            Err(error) => {
                return CheckResult::fail(
                    "xdg-desktop-portal",
                    format!(
                        "session bus probe failed: {error}; start a graphical session and ensure xdg-desktop-portal is installed"
                    ),
                );
            }
        };
        match runtime.block_on(portal_with_timeout(portal_owned(), PORTAL_TIMEOUT)) {
            Ok(result) => portal_result(
                result,
                env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("x11")),
            ),
            Err(()) => CheckResult::fail(
                "xdg-desktop-portal",
                "timed out after 2s; check the user D-Bus session and xdg-desktop-portal",
            ),
        }
    }
    fn pulse(&mut self) -> CheckResult {
        pulse_result(pulse::probe_server())
    }
    fn gstreamer(&mut self) -> CheckResult {
        gstreamer_result(element_check(&["pipewiresrc", "vp8enc", "webmmux"]))
    }
    fn x11(&mut self) -> CheckResult {
        let session = env::var("XDG_SESSION_TYPE")
            .unwrap_or_default()
            .to_lowercase();
        let display = display_value(env::var("DISPLAY").ok());
        let applicable = session != "wayland" && display.is_some();
        let randr = if applicable {
            RandrOutputProvider.outputs().map(|_| ())
        } else {
            Ok(())
        };
        let missing = if applicable && randr.is_ok() {
            element_check(&["ximagesrc"])
        } else {
            Ok(Vec::new())
        };
        x11_result(&session, display.as_deref(), randr, missing)
    }
    fn systemd(&mut self) -> CheckResult {
        let Some(program) = self.runner.which("systemctl") else {
            return systemd_result(Ok(None));
        };
        let result = self.runner.run(
            &program,
            &["--user", "is-system-running"],
            Duration::from_secs(5),
            &HashMap::new(),
        );
        systemd_result(match result {
            Ok(output) => Ok(output
                .stdout
                .trim()
                .lines()
                .next()
                .filter(|detail| !detail.is_empty())
                .map(str::to_owned)),
            Err(error) => Err(error.to_string()),
        })
    }
    fn appindicator(&mut self) -> CheckResult {
        let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let output = self.runner.which("gnome-extensions").and_then(|program| {
            self.runner
                .run(&program, &["list"], Duration::from_secs(5), &HashMap::new())
                .ok()
        });
        appindicator_result(&desktop, output.as_ref())
    }
    fn sync_health(&mut self) -> CheckResult {
        let config = self.config().clone();
        let facts = load_facts(&config.state_dir());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let health = derive_health(&facts, now, config.sync_stale_threshold as f64);
        sync_health_result(health)
    }
    fn quarantine(&mut self) -> Option<String> {
        let root = self.config().captures_dir();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        format_quarantine_line(&compute_quarantine_stats(&root, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct FakeChecks {
        values: VecDeque<CheckResult>,
        quarantine: Option<String>,
    }
    impl FakeChecks {
        fn all(severity: Severity) -> Self {
            let names = [
                "session type",
                "xdg-desktop-portal",
                "pipewire (pulse)",
                "gstreamer",
                "x11 capture",
                "systemd --user",
                "appindicator ext (soft)",
                "sync health",
            ];
            Self {
                values: names
                    .into_iter()
                    .map(|name| CheckResult {
                        name,
                        severity,
                        detail: "detail".into(),
                    })
                    .collect(),
                quarantine: None,
            }
        }
        fn take(&mut self) -> CheckResult {
            self.values.pop_front().unwrap()
        }
    }
    impl DoctorChecks for FakeChecks {
        fn session_type(&mut self) -> CheckResult {
            self.take()
        }
        fn portal(&mut self) -> CheckResult {
            self.take()
        }
        fn pulse(&mut self) -> CheckResult {
            self.take()
        }
        fn gstreamer(&mut self) -> CheckResult {
            self.take()
        }
        fn x11(&mut self) -> CheckResult {
            self.take()
        }
        fn systemd(&mut self) -> CheckResult {
            self.take()
        }
        fn appindicator(&mut self) -> CheckResult {
            self.take()
        }
        fn sync_health(&mut self) -> CheckResult {
            self.take()
        }
        fn quarantine(&mut self) -> Option<String> {
            self.quarantine.clone()
        }
    }
    // tests/test_doctor.py::test_run_doctor_all_pass_returns_zero
    #[test]
    fn all_pass_and_alignment() {
        let mut checks = FakeChecks::all(Severity::Ok);
        let mut out = Vec::new();
        assert_eq!(run_doctor(&mut checks, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("ok    session type                  detail\n"));
        assert!(out.contains("doctor: 8 checks, 0 failed, 0 warnings"));
    }
    // tests/test_doctor.py::test_run_doctor_any_fail_returns_one
    #[test]
    fn any_fail_returns_one() {
        let mut checks = FakeChecks::all(Severity::Ok);
        checks.values[3].severity = Severity::Fail;
        assert_eq!(run_doctor(&mut checks, &mut Vec::new()), 1);
    }
    // tests/test_doctor.py::test_run_doctor_warn_only_returns_zero
    #[test]
    fn warn_only_returns_zero() {
        let mut checks = FakeChecks::all(Severity::Ok);
        checks.values[0].severity = Severity::Warn;
        assert_eq!(run_doctor(&mut checks, &mut Vec::new()), 0);
    }
    // tests/test_doctor.py::test_run_doctor_prints_quarantine_line
    // tests/test_doctor.py::test_run_doctor_omits_empty_quarantine_line
    #[test]
    fn quarantine_optional() {
        for value in [Some("Quarantine: 1 rejected segment(s) held".into()), None] {
            let mut checks = FakeChecks::all(Severity::Ok);
            checks.quarantine = value.clone();
            let mut out = Vec::new();
            run_doctor(&mut checks, &mut out);
            assert_eq!(
                String::from_utf8(out).unwrap().contains("Quarantine:"),
                value.is_some()
            );
        }
    }
    // AC: aggregation keeps warnings non-fatal and failures fatal for each warn-capable slot.
    // The decision-core tests below exercise the native warning branches themselves.
    #[test]
    fn warn_capable_matrix() {
        for index in [0, 1, 6, 7] {
            for severity in [Severity::Warn, Severity::Ok, Severity::Fail] {
                let mut checks = FakeChecks::all(Severity::Ok);
                checks.values[index].severity = severity;
                let mut out = Vec::new();
                let code = run_doctor(&mut checks, &mut out);
                assert_eq!(code, if severity == Severity::Fail { 1 } else { 0 });
            }
        }
    }
    // tests/test_doctor.py::test_session_type_wayland_ok
    // tests/test_doctor.py::test_session_type_x11_ok
    // tests/test_doctor.py::test_session_type_unset_warns
    // tests/test_doctor.py::test_session_type_unknown_warns
    #[test]
    fn session_warn_and_ok_matrix() {
        assert_eq!(session_type_result(Some("wayland")).severity, Severity::Ok);
        assert_eq!(session_type_result(Some("x11")).severity, Severity::Ok);
        assert_eq!(session_type_result(None).severity, Severity::Warn);
        assert_eq!(session_type_result(Some("tty")).severity, Severity::Warn);
    }
    // tests/test_doctor.py::TestCheckX11Capture::test_wayland_session_not_applicable
    // tests/test_doctor.py::TestCheckX11Capture::test_no_display_no_x11_session_not_applicable
    // tests/test_doctor.py::TestCheckX11Capture::test_x11_session_no_display_fails
    // tests/test_doctor.py::TestCheckX11Capture::test_display_set_xrandr_missing_fails
    // tests/test_doctor.py::TestCheckX11Capture::test_display_set_ximagesrc_missing_fails
    // tests/test_doctor.py::TestCheckX11Capture::test_all_present_ok
    #[test]
    fn x11_capture_decision_matrix() {
        let wayland = x11_result(
            "wayland",
            None,
            Err("ignored".into()),
            Err("ignored".into()),
        );
        assert_eq!(wayland.severity, Severity::Ok);
        assert_eq!(wayland.detail, "not applicable (wayland session)");

        let no_display = x11_result("", None, Err("ignored".into()), Err("ignored".into()));
        assert_eq!(no_display.severity, Severity::Ok);
        assert_eq!(no_display.detail, "not applicable (no X11 display)");

        let missing_display = x11_result("x11", None, Ok(()), Ok(Vec::new()));
        assert_eq!(missing_display.severity, Severity::Fail);
        assert!(missing_display.detail.starts_with("DISPLAY not set;"));

        let randr = x11_result(
            "x11",
            Some(":0"),
            Err("extension unavailable".into()),
            Ok(Vec::new()),
        );
        assert_eq!(randr.severity, Severity::Fail);
        assert!(
            randr
                .detail
                .contains("X11 RandR unavailable: extension unavailable")
        );

        let ximagesrc = x11_result("x11", Some(":0"), Ok(()), Ok(vec!["ximagesrc".into()]));
        assert_eq!(ximagesrc.severity, Severity::Fail);
        assert!(ximagesrc.detail.starts_with("ximagesrc missing;"));

        let present = x11_result("x11", Some(":0"), Ok(()), Ok(Vec::new()));
        assert_eq!(present.severity, Severity::Ok);
        assert_eq!(present.detail, "X11 RandR and ximagesrc available");
    }
    // AC: X11's in-process GStreamer initialization failure names the failure and remedy.
    #[test]
    fn x11_capture_gstreamer_initialization_failure() {
        let result = x11_result(
            "x11",
            Some(":0"),
            Ok(()),
            Err("registry unavailable".into()),
        );
        assert_eq!(result.severity, Severity::Fail);
        assert!(result.detail.contains("registry unavailable"));
        assert!(result.detail.contains("install GStreamer 1.x"));
    }
    // AC: native GStreamer decision copy covers success, ordered missing names, and init failure.
    #[test]
    fn gstreamer_decision_matrix() {
        let present = gstreamer_result(Ok(Vec::new()));
        assert_eq!(present.severity, Severity::Ok);
        assert_eq!(present.detail, "pipewiresrc, vp8enc, and webmmux available");

        let one = gstreamer_result(Ok(vec!["vp8enc".into()]));
        assert_eq!(one.severity, Severity::Fail);
        assert!(one.detail.contains("element(s) vp8enc missing"));

        let many = gstreamer_result(Ok(vec!["pipewiresrc".into(), "webmmux".into()]));
        assert_eq!(many.severity, Severity::Fail);
        assert!(
            many.detail
                .contains("element(s) pipewiresrc, webmmux missing")
        );

        let init = gstreamer_result(Err("registry unavailable".into()));
        assert_eq!(init.severity, Severity::Fail);
        assert!(init.detail.contains("registry unavailable"));
        assert!(init.detail.contains("install GStreamer 1.x"));
    }
    // AC: the native Pulse decision reports both reachability outcomes with a remedy.
    #[test]
    fn pulse_decision_matrix() {
        let reachable = pulse_result(Ok(()));
        assert_eq!(reachable.severity, Severity::Ok);
        assert!(reachable.detail.contains("server reachable"));

        let unreachable = pulse_result(Err("connection refused".into()));
        assert_eq!(unreachable.severity, Severity::Fail);
        assert!(unreachable.detail.contains("connection refused"));
        assert!(
            unreachable
                .detail
                .contains("start PipeWire Pulse or PulseAudio")
        );
    }
    // AC: the native systemd decision covers success, empty output, and runner errors.
    #[test]
    fn systemd_decision_matrix() {
        let running = systemd_result(Ok(Some("running".into())));
        assert_eq!(running.severity, Severity::Ok);
        assert_eq!(running.detail, "running");

        let empty = systemd_result(Ok(None));
        assert_eq!(empty.severity, Severity::Fail);
        assert!(empty.detail.contains("run inside a systemd user session"));

        let failed = systemd_result(Err("timed out".into()));
        assert_eq!(failed.severity, Severity::Fail);
        assert!(failed.detail.contains("timed out"));
        assert!(failed.detail.contains("run inside a systemd user session"));
    }
    // tests/test_doctor.py::TestCheckX11Capture::test_no_display_no_x11_session_not_applicable
    // AC: Python treats an empty DISPLAY value as absent.
    #[test]
    fn empty_display_is_absent() {
        assert_eq!(display_value(Some(String::new())), None);
        assert_eq!(display_value(Some(":0".into())).as_deref(), Some(":0"));
    }
    // tests/test_doctor.py::test_check_portal_registered_returns_ok
    // tests/test_doctor.py::test_check_portal_x11_not_registered_returns_warn
    // tests/test_doctor.py::test_check_portal_not_registered_returns_fail
    // tests/test_doctor.py::test_check_portal_bus_unreachable_returns_fail
    #[test]
    fn portal_warn_ok_fail_matrix() {
        assert_eq!(portal_result(Ok(true), false).severity, Severity::Ok);
        assert_eq!(portal_result(Ok(false), true).severity, Severity::Warn);
        assert_eq!(portal_result(Ok(false), false).severity, Severity::Fail);
        let unreachable = portal_result(Err("no bus".into()), false);
        assert_eq!(unreachable.severity, Severity::Fail);
        assert!(unreachable.detail.contains("unreachable"));
        assert!(unreachable.detail.contains("no bus"));
    }
    // tests/test_doctor.py::test_check_portal_timeout_returns_fail
    #[tokio::test]
    async fn portal_timeout_is_bounded() {
        let result = portal_with_timeout(
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(true)
            },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(result, Err(()));
    }
    // tests/test_doctor.py::test_appindicator_non_gnome_is_ok_not_applicable
    // AC: GNOME AppIndicator presence and absence exercise both soft-check outcomes.
    #[test]
    fn appindicator_warn_and_ok_matrix() {
        let present = Output {
            success: true,
            stdout: "ubuntu-appindicators@ubuntu.com".into(),
        };
        assert_eq!(appindicator_result("KDE", None).severity, Severity::Ok);
        assert_eq!(
            appindicator_result("GNOME", Some(&present)).severity,
            Severity::Ok
        );
        assert_eq!(appindicator_result("GNOME", None).severity, Severity::Warn);
    }
    // tests/test_doctor.py::test_check_sync_health_update_needed
    // AC: sync health covers connected, warning, and failing doctor surfaces.
    #[test]
    fn sync_health_warn_ok_fail_matrix() {
        use crate::{
            private_link::LinkFactState,
            sync_health::{ErrorType, SyncFacts},
        };
        let connected = SyncFacts {
            pending_confirmed: Some(0),
            last_successful_contact: Some(100.0),
            last_successful_sync: Some(100.0),
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                ..LinkFactState::default()
            }),
            ..Default::default()
        };
        let offline = SyncFacts {
            last_error_class: Some(ErrorType::Transient),
            ..Default::default()
        };
        let update = SyncFacts {
            last_error_class: Some(ErrorType::Incompatible),
            last_error_code: Some(404),
            ..Default::default()
        };
        assert_eq!(
            sync_health_result(derive_health(&connected, 100.0, 600.0)).severity,
            Severity::Ok
        );
        assert_eq!(
            sync_health_result(derive_health(&offline, 100.0, 600.0)).severity,
            Severity::Warn
        );
        assert_eq!(
            sync_health_result(derive_health(&SyncFacts::default(), 100.0, 600.0)).severity,
            Severity::Warn
        );
        assert_eq!(
            sync_health_result(derive_health(&update, 100.0, 600.0)).severity,
            Severity::Fail
        );
    }
    // tests/test_doctor.py::test_check_exception_renders_as_fail
    #[test]
    fn panic_renders_fail() {
        struct Panic(FakeChecks);
        impl DoctorChecks for Panic {
            fn session_type(&mut self) -> CheckResult {
                panic!("boom")
            }
            fn portal(&mut self) -> CheckResult {
                self.0.take()
            }
            fn pulse(&mut self) -> CheckResult {
                self.0.take()
            }
            fn gstreamer(&mut self) -> CheckResult {
                self.0.take()
            }
            fn x11(&mut self) -> CheckResult {
                self.0.take()
            }
            fn systemd(&mut self) -> CheckResult {
                self.0.take()
            }
            fn appindicator(&mut self) -> CheckResult {
                self.0.take()
            }
            fn sync_health(&mut self) -> CheckResult {
                self.0.take()
            }
            fn quarantine(&mut self) -> Option<String> {
                None
            }
        }
        let mut value = Panic(FakeChecks::all(Severity::Ok));
        assert_eq!(run_doctor(&mut value, &mut Vec::new()), 1);
    }
}
