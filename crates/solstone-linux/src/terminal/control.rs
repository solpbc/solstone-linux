// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The owner's terminal and desktop-capture commands.
//!
//! With a daemon running, each goes through the request protocol, so it applies
//! without a restart and works with no session bus. With none, the command takes
//! the locks in the daemon's order and writes the config the daemon will read.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::config::{Config, ConfigPaths, load_config, resolve_config_paths, set_guided_field};
use crate::control_request::{self, Applier, Command, Outcome, WaitResult};
use crate::private_link::{CaptureRootLock, PrivateStateLock};
use crate::session_env::Runner;
use crate::tray::TrayCommand;

use super::legacy_find::{self, LegacyRootSources};
use super::state::{EngineState, TerminalState};
use super::supervisor::{LegacyProbe, TerminalSwitches};

/// Exit code for a refused `terminal on`.
pub const REFUSED: i32 = 2;
/// Exit code for a request that could not be answered in time; safe to retry.
pub const TEMPORARY: i32 = 75;

const LEGACY_REFUSAL: &str = "the solstone tmux app is still installed on this computer, so terminal capture stays off for now";

fn running(config: &Config) -> bool {
    CaptureRootLock::is_held(&config.base_dir)
}

/// The config directory the daemon reads: the user manager's absolute
/// `XDG_CONFIG_HOME`, else `~/.config`. An explicit path always wins.
fn daemon_paths(paths: &ConfigPaths, runner: &dyn Runner) -> ConfigPaths {
    if paths.config_dir.is_some() {
        return paths.clone();
    }
    let manager = crate::session_env::manager_environment(runner);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let config_dir = manager
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("solstone-linux");
    ConfigPaths {
        base_dir: paths.base_dir.clone(),
        config_dir: Some(config_dir),
    }
}

fn write_without_daemon(paths: &ConfigPaths, field: &str, value: bool) -> Result<(), String> {
    let config = resolve_config_paths(paths);
    let capture = CaptureRootLock::try_acquire(&config.base_dir)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the solstone app is starting; try again in a moment".to_owned())?;
    let mut state =
        PrivateStateLock::acquire(&config.config_dir).map_err(|error| error.to_string())?;
    state.adopt_capture_root(capture);
    set_guided_field(paths, field, Value::Bool(value))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn report(outcome: Outcome, output: &mut dyn Write, errors: &mut dyn Write) -> i32 {
    let target: &mut dyn Write = if outcome.code == 0 { output } else { errors };
    let _ = writeln!(target, "{}", outcome.line);
    outcome.code
}

fn send(
    state_dir: &std::path::Path,
    command: &Command,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    match control_request::request(state_dir, command) {
        Ok(WaitResult::Answered(outcome)) => report(outcome, output, errors),
        Ok(WaitResult::TimedOut { .. }) => {
            let _ = writeln!(
                errors,
                "the solstone app did not answer in time; try again in a moment"
            );
            TEMPORARY
        }
        Err(error) => {
            let _ = writeln!(errors, "could not reach the solstone app: {error}");
            1
        }
    }
}

/// One owner on/off choice, as a request to the daemon or a saved field.
struct OwnerSwitch<'a> {
    command: Command,
    field: &'a str,
    value: bool,
    confirmation: &'a str,
}

fn set_owner_switch(
    paths: ConfigPaths,
    switch: OwnerSwitch<'_>,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let config = resolve_config_paths(&paths);
    if running(&config) {
        return send(&config.state_dir(), &switch.command, output, errors);
    }
    let target = daemon_paths(&paths, runner);
    match write_without_daemon(&target, switch.field, switch.value) {
        Ok(()) => {
            let confirmation = switch.confirmation;
            let _ = writeln!(
                output,
                "{confirmation} the solstone app isn't running; this takes effect when it starts."
            );
            0
        }
        Err(error) => {
            let _ = writeln!(errors, "could not save that: {error}");
            1
        }
    }
}

pub fn set_terminal(
    paths: ConfigPaths,
    enabled: bool,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let sources = LegacyRootSources::current(runner);
    set_terminal_with_sources(paths, enabled, &sources, runner, output, errors)
}

fn set_terminal_with_sources(
    paths: ConfigPaths,
    enabled: bool,
    sources: &LegacyRootSources,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    if enabled && legacy_find::find(sources).found() {
        let _ = writeln!(errors, "{LEGACY_REFUSAL}");
        return REFUSED;
    }
    let (command, confirmation) = if enabled {
        (Command::TerminalOn, "terminal capture is on.")
    } else {
        (Command::TerminalOff, "terminal capture is off.")
    };
    set_owner_switch(
        paths,
        OwnerSwitch {
            command,
            field: "terminal",
            value: enabled,
            confirmation,
        },
        runner,
        output,
        errors,
    )
}

pub fn set_desktop_capture(
    paths: ConfigPaths,
    enabled: bool,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let (command, confirmation) = if enabled {
        (Command::DesktopCaptureOn, "desktop capture is on.")
    } else {
        (Command::DesktopCaptureOff, "desktop capture is off.")
    };
    set_owner_switch(
        paths,
        OwnerSwitch {
            command,
            field: "desktop_capture",
            value: enabled,
            confirmation,
        },
        runner,
        output,
        errors,
    )
}

/// Pause or resume when the daemon has no bus name to reach.
pub fn control_without_bus(
    paths: ConfigPaths,
    command: Command,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let config = resolve_config_paths(&paths);
    if !running(&config) {
        let _ = writeln!(
            errors,
            "the solstone app is not running, so there is nothing to pause or resume."
        );
        return 1;
    }
    send(&config.state_dir(), &command, output, errors)
}

/// The terminal lines of `status` and `terminal status`.
pub fn status_lines(config: &Config, say_if_stopped: bool) -> Vec<String> {
    let daemon = running(config);
    let state = TerminalState::read(&config.state_dir());
    let engine_running = daemon
        && state
            .as_ref()
            .is_some_and(|s| s.state == EngineState::Running);
    let counts = super::layout::count_segments(&config.captures_dir(), engine_running);
    let on = if daemon {
        state
            .as_ref()
            .map_or(config.terminal, |state| state.terminal)
    } else {
        config.terminal
    };
    let mut lines = vec![format!("terminal: {}", if on { "on" } else { "off" })];
    match (daemon, &state) {
        (false, _) => {
            if say_if_stopped {
                lines.push("  the solstone app isn't running".to_owned());
            }
        }
        (true, Some(state)) => {
            lines.push(format!("  {}", state.engine_line()));
            if let Some(tmux) = &state.tmux_path {
                lines.push(format!("  tmux: {}", tmux.display()));
            }
            for key in &state.ignored_settings {
                lines.push(format!("  ignored setting: {key}"));
            }
        }
        (true, None) => {}
    }
    if counts.waiting != 0 {
        lines.push(format!("  {} segment(s) waiting to upload", counts.waiting));
    }
    if counts.awaiting_recovery != 0 {
        lines.push(format!(
            "  {} unfinished segment(s) waiting for terminal capture to run again",
            counts.awaiting_recovery
        ));
    }
    if counts.held != 0 {
        lines.push(format!("  {} segment(s) held", counts.held));
    }
    lines
}

/// The config whose `terminal` choice status reports. With no daemon running that
/// is the config the daemon will read, which `terminal on` wrote to.
pub fn config_for_status(config: &Config, runner: &dyn Runner) -> Config {
    if running(config) {
        return config.clone();
    }
    load_config(daemon_paths(&ConfigPaths::default(), runner)).config
}

/// Whether screen and audio can run, for `status`.
pub fn desktop_lines(config: &Config) -> Vec<String> {
    if !config.desktop_capture {
        return vec!["screen, audio and desktop activity: off".to_owned()];
    }
    let mut lines = Vec::new();
    let no_desktop = crate::desktop_session::doctor_resolver()
        .ok()
        .map(|mut resolver| resolver.resolve(std::time::Instant::now()).presence)
        == Some(crate::desktop_session::SessionPresence::NoDesktop);
    if no_desktop {
        lines.push("screen and audio: need your desktop session, and none is running".to_owned());
    }
    lines.extend(session_bus_line(config));
    lines
}

/// The line for a daemon that started without a session bus, for `status` and `doctor`.
pub fn session_bus_line(config: &Config) -> Option<String> {
    (running(config)
        && config
            .state_dir()
            .join(crate::desktop_component::NO_BUS_MARKER)
            .exists())
    .then(|| NO_BUS_LINE.to_owned())
}

/// Said by `status` and `doctor` when the daemon started without a session bus.
pub const NO_BUS_LINE: &str =
    "screen capture: restart the app once this computer has a session bus";

pub fn terminal_status(paths: ConfigPaths, output: &mut dyn Write) -> i32 {
    let config = load_config(paths).config;
    for line in status_lines(&config, true) {
        if writeln!(output, "{line}").is_err() {
            return 1;
        }
    }
    0
}

/// Applies a saved `terminal` or `desktop_capture` to the running app.
pub struct DaemonLiveSettings {
    pub terminal: TerminalSwitches,
    pub terminal_sync: super::TerminalSync,
    pub commands: Mutex<Sender<TrayCommand>>,
}

impl DaemonLiveSettings {
    fn send(&self, command: TrayCommand) {
        let sender = self.commands.lock().unwrap_or_else(|p| p.into_inner());
        if sender.send(command).is_err() {
            tracing::warn!("The solstone app is stopping; that change applies at the next start");
        }
    }
}

impl crate::window_control::LiveSettings for DaemonLiveSettings {
    fn terminal(&self, enabled: bool) {
        self.terminal_sync.set_enabled(enabled);
        self.terminal.set_enabled(enabled);
    }

    fn desktop_capture(&self, enabled: bool) {
        self.send(TrayCommand::SetDesktopCapture(enabled));
    }

    fn pause(&self, minutes: Option<u64>) {
        // Directly as well: the observer can be inside a portal prompt for minutes.
        self.terminal.set_paused(true);
        self.send(match minutes {
            Some(minutes) => TrayCommand::Pause(minutes.saturating_mul(60)),
            None => TrayCommand::PauseIndefinite,
        });
    }

    fn resume(&self) {
        self.terminal.set_paused(false);
        self.send(TrayCommand::Resume);
    }
}

/// Applies requests inside the running daemon.
pub struct DaemonApplier {
    pub paths: ConfigPaths,
    pub live: Arc<dyn crate::window_control::LiveSettings>,
    pub legacy: LegacyProbe,
}

impl DaemonApplier {
    fn save(&self, field: &str, value: bool) -> Result<(), Outcome> {
        set_guided_field(&self.paths, field, Value::Bool(value))
            .map(|_| ())
            .map_err(|error| Outcome::failed(1, format!("could not save that: {error}")))
    }
}

impl Applier for DaemonApplier {
    fn apply(&self, command: &Command) -> Outcome {
        let result = match command {
            Command::TerminalOn => {
                if (self.legacy)().found() {
                    return Outcome::failed(REFUSED, LEGACY_REFUSAL);
                }
                self.save("terminal", true).map(|()| {
                    self.live.terminal(true);
                    "terminal capture is on."
                })
            }
            Command::TerminalOff => self.save("terminal", false).map(|()| {
                self.live.terminal(false);
                "terminal capture is off."
            }),
            Command::DesktopCaptureOn => self.save("desktop_capture", true).map(|()| {
                self.live.desktop_capture(true);
                "desktop capture is on."
            }),
            Command::DesktopCaptureOff => self.save("desktop_capture", false).map(|()| {
                self.live.desktop_capture(false);
                "desktop capture is off."
            }),
            Command::Pause { minutes } => {
                self.live.pause(*minutes);
                Ok("paused.")
            }
            Command::Resume => {
                self.live.resume();
                Ok("resumed.")
            }
        };
        match result {
            Ok(line) => Outcome::ok(line),
            Err(outcome) => outcome,
        }
    }
}

/// The real legacy find, for the daemon.
pub fn production_legacy_probe() -> LegacyProbe {
    Arc::new(|| legacy_find::find(&LegacyRootSources::current(&crate::cli::SystemRunner)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_env::Output;
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    struct NoSystemctl;
    impl Runner for NoSystemctl {
        fn which(&self, _: &str) -> Option<String> {
            None
        }
        fn run(
            &self,
            _: &str,
            _: &[&str],
            _: Duration,
            _: &HashMap<String, String>,
        ) -> io::Result<Output> {
            Err(io::Error::other("no systemctl"))
        }
    }

    fn paths(root: &Path) -> ConfigPaths {
        ConfigPaths {
            base_dir: Some(root.join("data")),
            config_dir: Some(root.join("config")),
        }
    }

    fn sources(home: &Path, env: Vec<HashMap<String, String>>) -> LegacyRootSources {
        LegacyRootSources {
            environments: env,
            home: home.to_owned(),
        }
    }

    fn turn_on(root: &Path, sources: &LegacyRootSources) -> (i32, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code =
            set_terminal_with_sources(paths(root), true, sources, &NoSystemctl, &mut out, &mut err);
        (code, String::from_utf8(err).unwrap())
    }

    fn snapshot_config(root: &Path) -> Option<Vec<u8>> {
        fs::read(root.join("config/config.json")).ok()
    }

    #[derive(Default)]
    struct RecordingApplier(StdMutex<Vec<Command>>);

    impl control_request::Applier for RecordingApplier {
        fn apply(&self, command: &Command) -> Outcome {
            self.0.lock().unwrap().push(command.clone());
            Outcome::ok("applied")
        }
    }

    /// A daemon on `root`'s capture root, answering requests.
    fn running_daemon(
        root: &Path,
    ) -> (
        CaptureRootLock,
        control_request::RequestServer,
        std::sync::Arc<RecordingApplier>,
    ) {
        let config = resolve_config_paths(&paths(root));
        fs::create_dir_all(&config.base_dir).unwrap();
        let lock = CaptureRootLock::try_acquire(&config.base_dir)
            .unwrap()
            .unwrap();
        let applier = std::sync::Arc::new(RecordingApplier::default());
        let server = control_request::RequestServer::spawn(
            &config.state_dir(),
            applier.clone(),
            Duration::from_millis(10),
        )
        .unwrap();
        (lock, server, applier)
    }

    #[test]
    fn a_shell_with_another_config_home_still_reaches_the_daemon() {
        let temp = tempfile::tempdir().unwrap();
        let (_lock, server, applier) = running_daemon(temp.path());
        let shell_config = temp.path().join("shell-config");
        let shell_paths = ConfigPaths {
            base_dir: Some(temp.path().join("data")),
            config_dir: Some(shell_config.clone()),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = set_terminal_with_sources(
            shell_paths,
            true,
            &sources(&temp.path().join("home"), Vec::new()),
            &NoSystemctl,
            &mut out,
            &mut err,
        );
        server.stop();
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert_eq!(*applier.0.lock().unwrap(), [Command::TerminalOn]);
        assert!(!shell_config.join("config.json").exists());
    }

    #[test]
    fn pause_reaches_a_daemon_with_no_bus_name() {
        let temp = tempfile::tempdir().unwrap();
        let (_lock, server, applier) = running_daemon(temp.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = control_without_bus(
            paths(temp.path()),
            Command::Pause { minutes: Some(5) },
            &mut out,
            &mut err,
        );
        server.stop();
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert_eq!(
            *applier.0.lock().unwrap(),
            [Command::Pause { minutes: Some(5) }]
        );
    }

    #[test]
    fn pause_with_no_daemon_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = control_without_bus(paths(temp.path()), Command::Resume, &mut out, &mut err);
        assert_eq!(code, 1);
        assert!(!err.is_empty());
    }

    #[test]
    fn turn_on_refuses_for_each_sign_of_an_old_install_and_changes_nothing() {
        // The user unit alone.
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let unit = home.join(".config/systemd/user/solstone-tmux.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "[Unit]\n").unwrap();
        let before = snapshot_config(temp.path());
        let (code, _) = turn_on(temp.path(), &sources(&home, Vec::new()));
        assert_eq!(code, REFUSED);
        assert_eq!(snapshot_config(temp.path()), before);
        fs::remove_file(&unit).unwrap();

        // Only a legacy credential.
        let credentials = home.join(".config/solstone-tmux/credentials.json");
        fs::create_dir_all(credentials.parent().unwrap()).unwrap();
        fs::write(&credentials, "{}").unwrap();
        assert_eq!(turn_on(temp.path(), &sources(&home, Vec::new())).0, REFUSED);
        fs::remove_file(&credentials).unwrap();

        // Captures under an XDG_DATA_HOME only the user manager knows.
        let data = temp.path().join("manager-data");
        fs::create_dir_all(data.join("solstone-tmux/captures/20261108")).unwrap();
        let manager = HashMap::from([(
            "XDG_DATA_HOME".to_owned(),
            data.to_string_lossy().into_owned(),
        )]);
        assert_eq!(
            turn_on(temp.path(), &sources(&home, vec![manager])).0,
            REFUSED
        );
        assert_eq!(snapshot_config(temp.path()), before);
    }

    #[test]
    fn with_no_daemon_the_switch_is_saved_for_the_next_start() {
        let temp = tempfile::tempdir().unwrap();
        let (code, err) = turn_on(temp.path(), &sources(temp.path(), Vec::new()));
        assert_eq!(code, 0, "{err}");
        assert!(load_config(paths(temp.path())).config.terminal);
    }

    #[test]
    fn a_cli_write_that_creates_the_config_keeps_the_marker_rule() {
        let temp = tempfile::tempdir().unwrap();
        let config = resolve_config_paths(&paths(temp.path()));
        fs::create_dir_all(&config.base_dir).unwrap();
        fs::write(config.desktop_capture_marker_path(), b"").unwrap();
        assert_eq!(turn_on(temp.path(), &sources(temp.path(), Vec::new())).0, 0);
        let loaded = load_config(paths(temp.path())).config;
        assert!(loaded.terminal);
        assert!(!loaded.desktop_capture);
        assert!(
            config.desktop_capture_marker_path().exists(),
            "only the daemon removes the marker"
        );
    }

    #[derive(Default)]
    struct Recording {
        calls: StdMutex<Vec<String>>,
    }
    impl crate::window_control::LiveSettings for Recording {
        fn terminal(&self, enabled: bool) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("terminal {enabled}"));
        }
        fn desktop_capture(&self, enabled: bool) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("desktop {enabled}"));
        }
        fn pause(&self, minutes: Option<u64>) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("pause {minutes:?}"));
        }
        fn resume(&self) {
            self.calls.lock().unwrap().push("resume".to_owned());
        }
    }

    fn applier(root: &Path, live: Arc<Recording>, found: bool) -> DaemonApplier {
        DaemonApplier {
            paths: paths(root),
            live,
            legacy: Arc::new(move || {
                let mut findings = legacy_find::LegacyFindings::default();
                if found {
                    findings.credentials.push("/x/credentials.json".into());
                }
                findings
            }),
        }
    }

    #[test]
    fn applied_switches_are_saved_and_take_effect_live() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("config")).unwrap();
        let live = Arc::new(Recording::default());
        let applier = applier(temp.path(), live.clone(), false);
        assert_eq!(applier.apply(&Command::TerminalOn).code, 0);
        assert_eq!(applier.apply(&Command::DesktopCaptureOff).code, 0);
        assert_eq!(applier.apply(&Command::Pause { minutes: Some(5) }).code, 0);
        assert_eq!(applier.apply(&Command::Resume).code, 0);
        let saved = load_config(paths(temp.path())).config;
        assert!(saved.terminal, "a restart keeps it on");
        assert!(!saved.desktop_capture);
        assert_eq!(
            *live.calls.lock().unwrap(),
            ["terminal true", "desktop false", "pause Some(5)", "resume"]
        );
    }

    #[test]
    fn the_daemon_refuses_terminal_on_beside_an_old_install() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let applier = applier(temp.path(), live.clone(), true);
        assert_eq!(applier.apply(&Command::TerminalOn).code, REFUSED);
        assert!(live.calls.lock().unwrap().is_empty());
        assert!(!load_config(paths(temp.path())).config.terminal);
    }

    #[test]
    fn a_retained_incomplete_is_reported_as_held_while_the_engine_runs() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().join("data"),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let stream = config.captures_dir().join("20261108/_tmux");
        fs::create_dir_all(stream.join("101500.incomplete")).unwrap();
        let lines = status_lines(&config, true).join("\n");
        assert!(lines.contains("1 segment(s) held"), "{lines}");
        assert!(!lines.contains("waiting for terminal"), "{lines}");
    }
}
