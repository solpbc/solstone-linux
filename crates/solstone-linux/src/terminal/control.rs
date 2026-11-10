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
use std::time::Duration;

use serde_json::Value;

use crate::config::{Config, ConfigPaths, load_config, resolve_config_paths, set_guided_field};
use crate::control_request::{self, Applier, Command, Outcome, WaitResult};
use crate::private_link::{CaptureRootLock, PrivateStateLock};
use crate::session_env::Runner;
use crate::tray::TrayCommand;

use super::legacy_find::{self, LegacyFindings, LegacyRootSources};
use super::state::{EngineState, TerminalState};
use super::supervisor::{LegacyProbe, TerminalSwitches};
use super::takeover::{self, StepError, TakeoverHost};
use super::takeover_state::{self, Continuity, Phase, Stored, TakeoverState};

/// Exit code for a refused `terminal on`.
pub const REFUSED: i32 = 2;
/// Exit code for a request that could not be answered in time; safe to retry.
pub const TEMPORARY: i32 = 75;
/// The move needs the owner's agreement and it was not given.
pub const NEEDS_CONSENT: i32 = 3;

const LEGACY_REFUSAL: &str =
    "solstone for tmux is still installed on this computer, so terminal stays off for now";

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
    consent: &mut Consent<'_>,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let sources = LegacyRootSources::current(runner);
    set_terminal_with_sources(paths, enabled, consent, &sources, runner, output, errors)
}

/// How the owner agrees to moving an old install in.
pub struct Consent<'a> {
    pub yes: bool,
    pub tty: bool,
    pub read_answer: &'a mut dyn FnMut() -> Option<String>,
}

impl Consent<'_> {
    fn granted(&mut self, output: &mut dyn Write) -> bool {
        if self.yes {
            return true;
        }
        if !self.tty {
            return false;
        }
        let _ = write!(output, "{CONSENT_QUESTION} ");
        let _ = output.flush();
        (self.read_answer)()
            .is_some_and(|answer| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
    }
}

const CONSENT_QUESTION: &str = "this moves solstone for tmux into this app, and it stops for good. what it still has waiting goes into your journal through this app, and anything that can't go stays on this computer. if this app isn't paired yet, it pairs with the journal solstone for tmux uses. move it now? [y/N]";
const CONSENT_NEEDED: &str = "moving solstone for tmux needs your ok. run this in a terminal, or add --yes to `terminal adopt-tmux`.";

/// `terminal adopt-tmux`: moves an old install in, or hands that to the daemon.
pub fn adopt_tmux(
    paths: ConfigPaths,
    consent: &mut Consent<'_>,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let sources = LegacyRootSources::current(runner);
    adopt_tmux_with(
        paths,
        consent,
        false,
        &sources,
        runner,
        control_request::LONGEST_BOUND,
        output,
        errors,
    )
}

fn unusable_record_line(path: &std::path::Path) -> String {
    format!(
        "the record of moving solstone for tmux can't be read: {}. please contact support, and don't delete it.",
        path.display()
    )
}

#[allow(clippy::too_many_arguments)]
fn adopt_tmux_with(
    paths: ConfigPaths,
    consent: &mut Consent<'_>,
    terminal_on: bool,
    sources: &LegacyRootSources,
    runner: &dyn Runner,
    bound: Duration,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let config = resolve_config_paths(&paths);
    let state_dir = config.state_dir();
    match takeover_state::load(&state_dir) {
        Stored::Unusable(path) => {
            let _ = writeln!(errors, "{}", unusable_record_line(&path));
            return 1;
        }
        Stored::Present(state) if state.is_done() => {
            let _ = writeln!(output, "solstone for tmux has already moved in.");
            return 0;
        }
        Stored::Present(_) => {}
        Stored::Absent => {
            if !legacy_find::find(sources).found() {
                let _ = writeln!(output, "no solstone for tmux to move in.");
                return 0;
            }
            if !consent.granted(output) {
                let _ = writeln!(errors, "{CONSENT_NEEDED}");
                return NEEDS_CONSENT;
            }
        }
    }
    if running(&config) {
        adopt_through_daemon(&state_dir, terminal_on, bound, output, errors)
    } else {
        adopt_with_no_daemon(&paths, terminal_on, sources, runner, output, errors)
    }
}

const WAIT_POLL: Duration = Duration::from_millis(100);

fn adopt_through_daemon(
    state_dir: &std::path::Path,
    terminal_on: bool,
    bound: Duration,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let deadline = std::time::Instant::now() + bound;
    let command = Command::AdoptTmux { terminal_on };
    let temporary = |errors: &mut dyn Write| {
        let _ = writeln!(
            errors,
            "the solstone app is still moving solstone for tmux in; it finishes on its own."
        );
        TEMPORARY
    };
    loop {
        match control_request::daemon_advertises(state_dir, &command) {
            control_request::Advertised::Supported => break,
            control_request::Advertised::Unsupported => {
                let _ = writeln!(
                    errors,
                    "the solstone app needs restarting before it can move solstone for tmux in."
                );
                return TEMPORARY;
            }
            control_request::Advertised::NotYet => {}
        }
        if std::time::Instant::now() >= deadline {
            let _ = writeln!(
                errors,
                "the solstone app did not answer, so nothing changed; try again in a moment."
            );
            return TEMPORARY;
        }
        std::thread::sleep(WAIT_POLL);
    }
    let accepted = match control_request::submit(state_dir, &command) {
        Ok(id) => control_request::wait(
            state_dir,
            &id,
            deadline.saturating_duration_since(std::time::Instant::now()),
            WAIT_POLL,
        ),
        Err(error) => {
            let _ = writeln!(errors, "could not reach the solstone app: {error}");
            return 1;
        }
    };
    match accepted {
        WaitResult::Answered(outcome) if outcome.code != 0 => {
            return report(outcome, output, errors);
        }
        WaitResult::Answered(_) => {}
        WaitResult::TimedOut { withdrawn: true } => {
            let _ = writeln!(
                errors,
                "the solstone app did not answer, so nothing changed; try again in a moment."
            );
            return TEMPORARY;
        }
        WaitResult::TimedOut { withdrawn: false } => return temporary(errors),
    }
    loop {
        if let Stored::Present(state) = takeover_state::load(state_dir)
            && state.phase >= Phase::Recovered
            && state.settings_applied
            && state.terminal_settled
        {
            let _ = writeln!(output, "solstone for tmux has moved in.");
            return 0;
        }
        if std::time::Instant::now() >= deadline {
            return temporary(errors);
        }
        std::thread::sleep(WAIT_POLL);
    }
}

fn adopt_with_no_daemon(
    paths: &ConfigPaths,
    terminal_on: bool,
    sources: &LegacyRootSources,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let config = resolve_config_paths(&daemon_paths(paths, runner));
    let failed = |errors: &mut dyn Write, error: String| {
        let _ = writeln!(errors, "could not move solstone for tmux in: {error}");
        1
    };
    let capture = match CaptureRootLock::try_acquire(&config.base_dir) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            let _ = writeln!(
                errors,
                "the solstone app is starting; try again in a moment."
            );
            return TEMPORARY;
        }
        Err(error) => return failed(errors, error.to_string()),
    };
    let mut lock = match PrivateStateLock::acquire(&config.config_dir) {
        Ok(lock) => lock,
        Err(error) => return failed(errors, error.to_string()),
    };
    lock.adopt_capture_root(capture);
    let state_dir = config.state_dir();
    let host = TakeoverHost::new(runner, sources.clone());
    let step = |result: Result<(), StepError>, errors: &mut dyn Write| match result {
        Ok(()) => None,
        Err(StepError::Temporary(text)) => {
            let _ = writeln!(errors, "{text}");
            Some(TEMPORARY)
        }
        Err(StepError::Refused(text)) => {
            let _ = writeln!(errors, "{text}");
            Some(1)
        }
    };
    let mut state = match takeover_state::load(&state_dir) {
        Stored::Present(state) => *state,
        Stored::Unusable(path) => {
            let _ = writeln!(errors, "{}", unusable_record_line(&path));
            return 1;
        }
        Stored::Absent => match takeover::prepare(&host) {
            Ok(state) => state,
            Err(error) => return step(Err(error), errors).unwrap_or(1),
        },
    };
    state.owner_turned_on |= terminal_on;
    if let Err(error) = takeover_state::save(&state_dir, &state) {
        return failed(errors, error.to_string());
    }
    if state.phase == Phase::Consented {
        if let Some(code) = step(takeover::stop_for_good(&host, &mut state), errors) {
            return code;
        }
        if let Err(error) = takeover_state::save(&state_dir, &state) {
            let _ = writeln!(errors, "could not record the move yet: {error}");
            return TEMPORARY;
        }
    }
    crate::source_intake::carry_tmux_roots(&state_dir, &state.data_roots);
    if let Some(code) = step(takeover::recover_roots(&mut state), errors) {
        return code;
    }
    if let Err(error) = takeover_state::save(&state_dir, &state) {
        let _ = writeln!(errors, "could not record the move yet: {error}");
        return TEMPORARY;
    }
    let _ = writeln!(
        output,
        "solstone for tmux has moved in. the solstone app finishes the rest when it starts."
    );
    0
}

/// The owner's latest word stands: a move still under way must not turn terminal back on.
fn keep_terminal_off(state: &mut TakeoverState) {
    if !state.terminal_settled {
        state.service_was_on = false;
        state.owner_turned_on = false;
    }
}

fn set_terminal_with_sources(
    paths: ConfigPaths,
    enabled: bool,
    consent: &mut Consent<'_>,
    sources: &LegacyRootSources,
    runner: &dyn Runner,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    if enabled {
        let state_dir = resolve_config_paths(&paths).state_dir();
        let stored = takeover_state::load(&state_dir);
        // A move that has not reached the app yet is finished first.
        let unfinished = stored
            .present()
            .is_some_and(|state| state.phase < Phase::Recovered);
        if unfinished || (stored == Stored::Absent && legacy_find::find(sources).found()) {
            return adopt_tmux_with(
                paths,
                consent,
                true,
                sources,
                runner,
                control_request::LONGEST_BOUND,
                output,
                errors,
            );
        }
    }
    let (command, confirmation) = if enabled {
        (Command::TerminalOn, "terminal is on.")
    } else {
        // A daemon does this as it applies the request; with none running, this is the only place.
        let state_dir = resolve_config_paths(&paths).state_dir();
        if !running(&resolve_config_paths(&paths)) {
            let _ = takeover_state::update(&state_dir, keep_terminal_off);
        }
        (Command::TerminalOff, "terminal is off.")
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
        (
            Command::DesktopCaptureOn,
            "screen, audio and desktop activity are on.",
        )
    } else {
        (
            Command::DesktopCaptureOff,
            "screen, audio and desktop activity are off.",
        )
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

fn takeover_lines(config: &Config) -> Vec<String> {
    let state = match takeover_state::load(&config.state_dir()) {
        Stored::Absent => return Vec::new(),
        Stored::Unusable(path) => return vec![format!("  {}", unusable_record_line(&path))],
        Stored::Present(state) => state,
    };
    let mut lines = Vec::new();
    let phase = match state.phase {
        Phase::Consented => Some("stopping the old app"),
        Phase::Stopped => Some("bringing over what's waiting"),
        Phase::Recovered => Some("finishing"),
        Phase::Done => None,
    };
    if let Some(phase) = phase {
        lines.push(format!("  moving solstone for tmux in: {phase}"));
    }
    match &state.continuity {
        Some(Continuity::Adopted) => {
            lines.push("  terminal history: continued, under the old pairing".to_owned());
        }
        Some(Continuity::Continued) => {
            lines.push("  terminal history: continued".to_owned());
        }
        Some(Continuity::NewStream {
            reason,
            needs_attention,
        }) => {
            lines.push(format!("  new terminal history from here, and what's already in your journal stays ({reason})"));
            if *needs_attention {
                lines.push("  this needs attention: please contact support".to_owned());
            }
        }
        None => {}
    }
    if !state.service_was_on && !state.owner_turned_on && state.terminal_settled {
        lines.push("  solstone for tmux had been stopped, so terminal stays off".to_owned());
    }
    if let Some(answer) = &state.last_answer {
        lines.push(format!("  waiting: {answer}"));
    }
    for path in &state.conflicting_credentials {
        lines.push(format!("  more than one old pairing: {}", path.display()));
    }
    for (root, record) in &state.roots {
        if let Some(problem) = &record.problem {
            lines.push(format!("  couldn't use {}: {problem}", root.display()));
        }
        if record.waiting != 0 {
            lines.push(format!(
                "  {} segment(s) from solstone for tmux waiting to upload",
                record.waiting
            ));
        }
        if record.held != 0 {
            lines.push(format!("  {} old segment(s) held back", record.held));
        }
        for path in &record.problem_paths {
            lines.push(format!("  problem: {}", path.display()));
        }
    }
    lines
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
    lines.extend(takeover_lines(config));
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
            "  {} unfinished segment(s) waiting for terminal to run again",
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
    "screen and audio: restart the app after you log in to your desktop here";

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

    fn terminal_settings(&self, settings: Value) {
        self.terminal
            .set_settings(super::settings::TerminalSettings::from_saved(Some(
                &settings,
            )));
    }

    fn recheck_terminal(&self) {
        self.terminal.recheck();
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

/// Reads what an old install is, without changing anything.
pub type TakeoverPrepare = Arc<dyn Fn() -> Result<TakeoverState, StepError> + Send + Sync>;

/// Applies requests inside the running daemon.
pub struct DaemonApplier {
    pub paths: ConfigPaths,
    pub live: Arc<dyn crate::window_control::LiveSettings>,
    pub legacy: LegacyProbe,
    pub state_dir: PathBuf,
    pub prepare: TakeoverPrepare,
    pub wake_takeover: Arc<dyn Fn() + Send + Sync>,
}

impl DaemonApplier {
    /// Records the owner's agreement; the takeover's own thread does the work.
    fn adopt(&self, terminal_on: bool) -> Outcome {
        match takeover_state::load(&self.state_dir) {
            Stored::Unusable(path) => Outcome::failed(1, unusable_record_line(&path)),
            Stored::Present(_) => {
                if terminal_on {
                    let _ = takeover_state::update(&self.state_dir, |state| {
                        state.owner_turned_on = true;
                    });
                }
                (self.wake_takeover)();
                Outcome::ok("moving solstone for tmux in.")
            }
            Stored::Absent => {
                if !(self.legacy)().found() {
                    return Outcome::failed(1, "no solstone for tmux to move in.");
                }
                let mut state = match (self.prepare)() {
                    Ok(state) => state,
                    Err(StepError::Refused(text)) => return Outcome::failed(1, text),
                    Err(StepError::Temporary(text)) => return Outcome::failed(TEMPORARY, text),
                };
                state.owner_turned_on = terminal_on;
                if let Err(error) = takeover_state::save_if_absent(&self.state_dir, &state) {
                    return Outcome::failed(1, format!("could not save that: {error}"));
                }
                (self.wake_takeover)();
                Outcome::ok("moving solstone for tmux in.")
            }
        }
    }

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
                    "terminal is on."
                })
            }
            Command::TerminalOff => self.save("terminal", false).map(|()| {
                let _ = takeover_state::update(&self.state_dir, keep_terminal_off);
                self.live.terminal(false);
                "terminal is off."
            }),
            Command::DesktopCaptureOn => self.save("desktop_capture", true).map(|()| {
                self.live.desktop_capture(true);
                "screen, audio and desktop activity are on."
            }),
            Command::DesktopCaptureOff => self.save("desktop_capture", false).map(|()| {
                self.live.desktop_capture(false);
                "screen, audio and desktop activity are off."
            }),
            Command::Pause { minutes } => {
                self.live.pause(*minutes);
                Ok("paused.")
            }
            Command::Resume => {
                self.live.resume();
                Ok("resumed.")
            }
            Command::AdoptTmux { terminal_on } => return self.adopt(*terminal_on),
        };
        match result {
            Ok(line) => Outcome::ok(line),
            Err(outcome) => outcome,
        }
    }
}

/// What holds the engine back. Once the old spool is recovered the move no longer
/// does; with an unreadable record, only a service that may still be running does.
pub fn engine_hold(state_dir: &std::path::Path, sources: &LegacyRootSources) -> LegacyFindings {
    match takeover_state::load(state_dir) {
        Stored::Present(state) if state.phase >= Phase::Recovered && state.settings_applied => {
            LegacyFindings::default()
        }
        Stored::Unusable(_) => LegacyFindings {
            units: legacy_find::find(sources).units,
            ..LegacyFindings::default()
        },
        Stored::Present(_) | Stored::Absent => legacy_find::find(sources),
    }
}

/// The real legacy find, for the daemon.
pub fn production_legacy_probe(state_dir: PathBuf) -> LegacyProbe {
    Arc::new(move || {
        engine_hold(
            &state_dir,
            &LegacyRootSources::current(&crate::cli::SystemRunner),
        )
    })
}

/// Whether turning terminal on from the window must first ask to move an old
/// install in.
pub fn window_move_needed() -> bool {
    let config = resolve_config_paths(&ConfigPaths::default());
    takeover_state::load(&config.state_dir()) == Stored::Absent
        && legacy_find::find(&LegacyRootSources::current(&crate::cli::SystemRunner)).found()
}

/// The window's agreement, through the same request a shell would send. Returns
/// the line to show when it could not be handed over.
pub fn window_request_move() -> Option<String> {
    let config = resolve_config_paths(&ConfigPaths::default());
    if !running(&config) {
        return Some("the solstone app isn't running; start it and try again.".to_owned());
    }
    match control_request::request(
        &config.state_dir(),
        &Command::AdoptTmux { terminal_on: true },
    ) {
        Ok(WaitResult::Answered(outcome)) if outcome.code == 0 => None,
        Ok(WaitResult::Answered(outcome)) => Some(outcome.line),
        Ok(WaitResult::TimedOut { .. }) => {
            Some("the solstone app did not answer in time; try again in a moment.".to_owned())
        }
        Err(error) => Some(format!("could not reach the solstone app: {error}")),
    }
}

/// The real read of an old install, for the daemon.
pub fn production_takeover_prepare() -> TakeoverPrepare {
    Arc::new(|| {
        let runner = crate::cli::SystemRunner;
        let sources = LegacyRootSources::current(&runner);
        takeover::prepare(&TakeoverHost::new(&runner, sources))
    })
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
        let mut no_answer = || None;
        let mut consent = Consent {
            yes: false,
            tty: false,
            read_answer: &mut no_answer,
        };
        let code = set_terminal_with_sources(
            paths(root),
            true,
            &mut consent,
            sources,
            &NoSystemctl,
            &mut out,
            &mut err,
        );
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
        let mut no_answer = || None;
        let mut consent = Consent {
            yes: false,
            tty: false,
            read_answer: &mut no_answer,
        };
        let code = set_terminal_with_sources(
            shell_paths,
            true,
            &mut consent,
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
    fn turn_on_asks_first_for_each_sign_of_an_old_install_and_changes_nothing() {
        // The user unit alone.
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let unit = home.join(".config/systemd/user/solstone-tmux.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "[Unit]\n").unwrap();
        let before = snapshot_config(temp.path());
        let (code, _) = turn_on(temp.path(), &sources(&home, Vec::new()));
        assert_eq!(code, NEEDS_CONSENT);
        assert_eq!(snapshot_config(temp.path()), before);
        fs::remove_file(&unit).unwrap();

        // Only a legacy credential.
        let credentials = home.join(".config/solstone-tmux/credentials.json");
        fs::create_dir_all(credentials.parent().unwrap()).unwrap();
        fs::write(&credentials, "{}").unwrap();
        assert_eq!(
            turn_on(temp.path(), &sources(&home, Vec::new())).0,
            NEEDS_CONSENT
        );
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
            NEEDS_CONSENT
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
            state_dir: root.join("takeover-state"),
            prepare: Arc::new(|| Ok(TakeoverState::default())),
            wake_takeover: Arc::new(|| {}),
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

    fn adopt(root: &Path, sources: &LegacyRootSources, yes: bool) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut no_answer = || None;
        let mut consent = Consent {
            yes,
            tty: false,
            read_answer: &mut no_answer,
        };
        let code = adopt_tmux_with(
            paths(root),
            &mut consent,
            false,
            sources,
            &NoSystemctl,
            Duration::from_secs(5),
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn old_install(home: &Path) -> PathBuf {
        let credentials = home.join(".config/solstone-tmux/credentials.json");
        fs::create_dir_all(credentials.parent().unwrap()).unwrap();
        fs::write(&credentials, "{}").unwrap();
        fs::create_dir_all(home.join(".local/share/solstone-tmux/captures")).unwrap();
        credentials
    }

    #[test]
    fn with_no_agreement_nothing_changes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let credentials = old_install(&home);
        let (code, _, _) = adopt(temp.path(), &sources(&home, Vec::new()), false);
        assert_eq!(code, NEEDS_CONSENT);
        assert!(credentials.exists());
        let state_dir = resolve_config_paths(&paths(temp.path())).state_dir();
        assert_eq!(takeover_state::load(&state_dir), Stored::Absent);
    }

    #[test]
    fn nothing_to_move_is_success() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        assert_eq!(adopt(temp.path(), &sources(&home, Vec::new()), true).0, 0);
    }

    #[test]
    fn with_no_daemon_it_records_and_recovers_and_leaves_the_rest_for_the_daemon() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        old_install(&home);
        let (code, _, err) = adopt(temp.path(), &sources(&home, Vec::new()), true);
        assert_eq!(code, 0, "{err}");
        let state_dir = resolve_config_paths(&paths(temp.path())).state_dir();
        let Stored::Present(state) = takeover_state::load(&state_dir) else {
            panic!("no record");
        };
        assert_eq!(state.phase, Phase::Recovered);
        assert!(!state.settings_applied);
        assert!(!state.terminal_settled);
        assert!(!load_config(paths(temp.path())).config.terminal);
    }

    #[test]
    fn a_running_daemon_is_handed_the_move() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        old_install(&home);
        let config = resolve_config_paths(&paths(temp.path()));
        fs::create_dir_all(&config.base_dir).unwrap();
        let _lock = CaptureRootLock::try_acquire(&config.base_dir)
            .unwrap()
            .unwrap();
        let state_dir = config.state_dir();
        struct Finishing(PathBuf);
        impl control_request::Applier for Finishing {
            fn apply(&self, command: &Command) -> Outcome {
                assert!(matches!(command, Command::AdoptTmux { .. }));
                let state = TakeoverState {
                    phase: Phase::Recovered,
                    settings_applied: true,
                    terminal_settled: true,
                    ..TakeoverState::default()
                };
                takeover_state::save(&self.0, &state).unwrap();
                Outcome::ok("moving")
            }
        }
        let server = control_request::RequestServer::spawn(
            &state_dir,
            Arc::new(Finishing(state_dir.clone())),
            Duration::from_millis(10),
        )
        .unwrap();
        let (code, _, err) = adopt(temp.path(), &sources(&home, Vec::new()), true);
        server.stop();
        assert_eq!(code, 0, "{err}");
    }

    #[test]
    fn the_daemon_records_consent_and_answers_at_once() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut applier = applier(temp.path(), live, true);
        let flag = woken.clone();
        applier.wake_takeover = Arc::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(
            applier
                .apply(&Command::AdoptTmux { terminal_on: true })
                .code,
            0
        );
        assert!(woken.load(std::sync::atomic::Ordering::SeqCst));
        assert!(matches!(
            takeover_state::load(&applier.state_dir),
            Stored::Present(_)
        ));
    }

    #[test]
    fn an_unreadable_record_holds_the_engine_only_while_the_service_exists() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        old_install(&home);
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(takeover_state::state_path(&state_dir), b"{").unwrap();
        let sources = sources(&home, Vec::new());
        assert!(!engine_hold(&state_dir, &sources).found());
        let unit = home.join(".config/systemd/user/solstone-tmux.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "[Unit]\n").unwrap();
        assert!(engine_hold(&state_dir, &sources).found());
    }

    #[test]
    fn once_recovered_the_old_install_no_longer_holds_the_engine() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        old_install(&home);
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let sources = sources(&home, Vec::new());
        assert!(engine_hold(&state_dir, &sources).found());
        let mut state = TakeoverState {
            phase: Phase::Recovered,
            ..TakeoverState::default()
        };
        takeover_state::save(&state_dir, &state).unwrap();
        assert!(
            engine_hold(&state_dir, &sources).found(),
            "the engine waits for the carried settings"
        );
        state.settings_applied = true;
        takeover_state::save(&state_dir, &state).unwrap();
        assert!(!engine_hold(&state_dir, &sources).found());
    }

    #[test]
    fn turning_terminal_off_during_a_move_keeps_it_off() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let applier = applier(temp.path(), live, false);
        let state = TakeoverState {
            service_was_on: true,
            owner_turned_on: true,
            ..TakeoverState::default()
        };
        takeover_state::save(&applier.state_dir, &state).unwrap();
        assert_eq!(applier.apply(&Command::TerminalOff).code, 0);
        let Stored::Present(after) = takeover_state::load(&applier.state_dir) else {
            panic!("no record");
        };
        assert!(!after.service_was_on && !after.owner_turned_on);
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
