// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use solstone_terminal::command::{CommandError, CommandInvocation, CommandOutput, CommandRunner};
use solstone_terminal::model::{CaptureResult, PaneInfo, WindowInfo};
use solstone_terminal::tmux::WarningSink;

#[derive(Clone, Debug)]
pub enum FixtureOutcome {
    Output(CommandOutput),
    Timeout,
    SpawnFailure,
}

#[derive(Clone, Debug)]
pub struct ExpectedInvocation {
    pub invocation: CommandInvocation,
    pub outcome: FixtureOutcome,
}

#[derive(Clone, Debug, Default)]
pub struct FixtureRunner {
    expected: Arc<Mutex<VecDeque<ExpectedInvocation>>>,
    calls: Arc<Mutex<Vec<CommandInvocation>>>,
    mismatches: Arc<Mutex<Vec<String>>>,
}

impl FixtureRunner {
    pub fn new(expected: impl IntoIterator<Item = ExpectedInvocation>) -> Self {
        Self {
            expected: Arc::new(Mutex::new(expected.into_iter().collect())),
            ..Self::default()
        }
    }

    pub fn push(&self, invocation: ExpectedInvocation) {
        self.expected
            .lock()
            .expect("fixture queue poisoned")
            .push_back(invocation);
    }

    pub fn extend(&self, invocations: impl IntoIterator<Item = ExpectedInvocation>) {
        self.expected
            .lock()
            .expect("fixture queue poisoned")
            .extend(invocations);
    }

    pub fn calls(&self) -> Vec<CommandInvocation> {
        self.calls.lock().expect("fixture calls poisoned").clone()
    }

    pub fn assert_finished(&self) -> Result<(), String> {
        let expected = self.expected.lock().expect("fixture queue poisoned");
        let mismatches = self.mismatches.lock().expect("fixture errors poisoned");
        if !mismatches.is_empty() {
            return Err(mismatches.join("; "));
        }
        if !expected.is_empty() {
            return Err(format!(
                "{} expected invocation(s) were unused",
                expected.len()
            ));
        }
        Ok(())
    }
}

impl CommandRunner for FixtureRunner {
    fn run<'a>(
        &'a self,
        invocation: CommandInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<CommandOutput, CommandError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("fixture calls poisoned")
                .push(invocation.clone());
            let Some(expected) = self
                .expected
                .lock()
                .expect("fixture queue poisoned")
                .pop_front()
            else {
                let message = format!("unexpected invocation: {invocation:?}");
                self.mismatches
                    .lock()
                    .expect("fixture errors poisoned")
                    .push(message.clone());
                return Err(spawn_error(message));
            };
            if expected.invocation != invocation {
                let message = format!(
                    "invocation mismatch: expected {:?}, got {:?}",
                    expected.invocation, invocation
                );
                self.mismatches
                    .lock()
                    .expect("fixture errors poisoned")
                    .push(message.clone());
                return Err(spawn_error(message));
            }
            match expected.outcome {
                FixtureOutcome::Output(output) => Ok(output),
                FixtureOutcome::Timeout => Err(CommandError::Timeout {
                    operation: invocation.operation,
                    duration: invocation.timeout,
                }),
                FixtureOutcome::SpawnFailure => {
                    Err(spawn_error("fixture requested a spawn failure"))
                }
            }
        })
    }
}

fn spawn_error(message: impl Into<String>) -> CommandError {
    CommandError::Spawn(std::io::Error::other(message.into()))
}

#[derive(Clone, Debug, Default)]
pub struct RecordingWarnings(Arc<Mutex<Vec<String>>>);

impl RecordingWarnings {
    pub fn messages(&self) -> Vec<String> {
        self.0.lock().expect("warnings poisoned").clone()
    }
}

impl WarningSink for RecordingWarnings {
    fn warn(&self, message: &str) {
        self.0
            .lock()
            .expect("warnings poisoned")
            .push(message.to_owned());
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopWarnings;

impl WarningSink for NoopWarnings {
    fn warn(&self, _msg: &str) {}
}

pub fn output(stdout: impl Into<Vec<u8>>) -> FixtureOutcome {
    FixtureOutcome::Output(CommandOutput {
        stdout: stdout.into(),
        stderr: Vec::new(),
        status: 0,
    })
}

pub fn nonzero(status: i32) -> FixtureOutcome {
    FixtureOutcome::Output(CommandOutput {
        stdout: Vec::new(),
        stderr: b"fixture failure".to_vec(),
        status,
    })
}

pub fn command_output(stdout: &[u8], stderr: &[u8], status: i32) -> FixtureOutcome {
    FixtureOutcome::Output(CommandOutput {
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
        status,
    })
}

pub struct TestDirectory(PathBuf);

impl TestDirectory {
    pub fn new(label: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::path::Path::new("/var/tmp").join(format!(
            "solstone-terminal-{label}-{}-{number}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn test_identity(label: &str) -> solstone_terminal::instance_lock::RunIdentity {
    solstone_terminal::instance_lock::RunIdentity {
        run_id: format!("test-{label}"),
        lock_inode: 1,
    }
}

pub fn golden_capture(session: &str) -> CaptureResult {
    let active_window = WindowInfo {
        id: "@7".to_owned(),
        index: 2,
        name: "dev café".to_owned(),
        active: true,
    };
    CaptureResult {
        session: session.to_owned(),
        window: active_window.clone(),
        windows: vec![
            active_window,
            WindowInfo {
                id: "@8".to_owned(),
                index: 3,
                name: "logs".to_owned(),
                active: false,
            },
        ],
        panes: vec![
            PaneInfo {
                id: "%11".to_owned(),
                index: 0,
                left: 0,
                top: 0,
                width: 60,
                height: 24,
                active: true,
                cursor_x: 0,
                cursor_y: 0,
                cursor_visible: true,
                alternate_on: false,
                pane_in_mode: false,
                content: "\u{1b}[31mRED\u{1b}[0m café\n".to_owned(),
            },
            PaneInfo {
                id: "%12".to_owned(),
                index: 1,
                left: 61,
                top: 0,
                width: 59,
                height: 24,
                active: false,
                cursor_x: 0,
                cursor_y: 0,
                cursor_visible: true,
                alternate_on: false,
                pane_in_mode: false,
                content: "right pane\n".to_owned(),
            },
        ],
        output_only: false,
    }
}

pub fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_length = (input.len() as u64) * 8;
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for block in padded.chunks_exact(64) {
        let mut schedule = [0_u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes(word.try_into().expect("four-byte word"));
        }
        for index in 16..64 {
            let first = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let second = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(first)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(second);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum_one = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temporary_one = h
                .wrapping_add(sum_one)
                .wrapping_add(choice)
                .wrapping_add(ROUND[index])
                .wrapping_add(schedule[index]);
            let sum_zero = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary_two = sum_zero.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary_one);
            d = c;
            c = b;
            b = a;
            a = temporary_one.wrapping_add(temporary_two);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut digest = [0_u8; 32];
    for (chunk, word) in digest.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

use solstone_terminal::foreground::{ExeFailure, ForegroundLookup};

#[derive(Clone)]
pub struct FakeLookup {
    pub pids: Arc<Mutex<Vec<u32>>>,
    pub result: Result<String, ExeFailure>,
}

impl FakeLookup {
    pub fn new(pids: Arc<Mutex<Vec<u32>>>, result: Result<String, ExeFailure>) -> Self {
        Self { pids, result }
    }
}

impl ForegroundLookup for FakeLookup {
    fn lookup<'a>(
        &'a self,
        pane_pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String, ExeFailure>> + Send + 'a>> {
        Box::pin(async move {
            self.pids.lock().expect("pids lock").push(pane_pid);
            self.result.clone()
        })
    }
}

/// What a fake tmux server holds: its identity and its global options.
#[derive(Clone, Debug)]
pub struct FakeServer {
    pub identity: String,
    pub options: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct FakeTmuxState {
    server: Option<FakeServer>,
    servers_started: u32,
    content: u64,
    fail_polls: bool,
    fail_indicator: bool,
    calls: Vec<Vec<String>>,
    on_unset: Option<Box<dyn FnMut() + Send>>,
}

/// A tmux that answers every command the engine sends from its own state: a
/// server that can be started, stopped and replaced, its options, one session
/// with one pane, and switches that make polls or ☼ writes fail.
#[derive(Clone, Default)]
pub struct FakeTmux {
    state: Arc<Mutex<FakeTmuxState>>,
}

const NO_SERVER: &[u8] = b"no server running on /tmp/tmux-1000/default\n";
// A client activity later than any test clock, so every poll sees the client.
const CLIENT_ACTIVITY: i64 = 4_102_444_800;

impl FakeTmux {
    /// A fake with a running server holding these options.
    pub fn with_server(options: &[(&str, &str)]) -> Self {
        let fake = Self::default();
        fake.start_server(options);
        fake
    }

    /// Starts a new server, replacing any running one.
    pub fn start_server(&self, options: &[(&str, &str)]) {
        let mut state = self.lock();
        state.servers_started += 1;
        let identity = format!(
            "{} 1000{}",
            4000 + state.servers_started,
            state.servers_started
        );
        state.server = Some(FakeServer {
            identity,
            options: options
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        });
    }

    pub fn stop_server(&self) {
        self.lock().server = None;
    }

    pub fn option(&self, name: &str) -> Option<String> {
        self.lock()
            .server
            .as_ref()
            .and_then(|server| server.options.get(name).cloned())
    }

    /// Makes the next captures differ, so each poll appends a frame.
    pub fn change_content(&self) {
        self.lock().content += 1;
    }

    pub fn set_fail_polls(&self, fail: bool) {
        self.lock().fail_polls = fail;
    }

    pub fn set_fail_indicator(&self, fail: bool) {
        self.lock().fail_indicator = fail;
    }

    /// Runs `hook` when `@solstone` is unset, which the engine does last when it
    /// restores the owner's options.
    pub fn on_unset(&self, hook: impl FnMut() + Send + 'static) {
        self.lock().on_unset = Some(Box::new(hook));
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.lock().calls.clone()
    }

    /// How many times `name` was set with `set-option -g`.
    pub fn writes_of(&self, name: &str) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|args| args.len() == 4 && args[0] == "set-option" && args[1] == "-g")
            .filter(|args| args[2] == name)
            .count()
    }

    pub fn polls(&self) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|args| {
                args.first()
                    .is_some_and(|command| command == "list-clients")
            })
            .count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeTmuxState> {
        self.state.lock().expect("fake tmux poisoned")
    }

    fn answer(&self, args: &[String]) -> CommandOutput {
        let mut state = self.lock();
        state.calls.push(args.to_vec());
        let content = state.content;
        let fail_polls = state.fail_polls;
        let fail_indicator = state.fail_indicator;
        let Some(server) = state.server.as_mut() else {
            return CommandOutput {
                stdout: Vec::new(),
                stderr: NO_SERVER.to_vec(),
                status: 1,
            };
        };
        let strs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let lost_server = || CommandOutput {
            stdout: Vec::new(),
            stderr: b"lost server\n".to_vec(),
            status: 1,
        };
        let ok = |stdout: String| CommandOutput {
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
            status: 0,
        };
        match strs.as_slice() {
            ["display-message", "-p", "#{pid} #{start_time}"] => {
                if fail_indicator {
                    lost_server()
                } else {
                    ok(format!("{}\n", server.identity))
                }
            }
            ["show-options", "-gv", name] => match server.options.get(*name) {
                Some(value) => ok(format!("{value}\n")),
                None => CommandOutput {
                    stdout: Vec::new(),
                    stderr: format!("invalid option: {name}\n").into_bytes(),
                    status: 1,
                },
            },
            ["set-option", "-g", name, value] => {
                if fail_indicator {
                    lost_server()
                } else {
                    server
                        .options
                        .insert((*name).to_owned(), (*value).to_owned());
                    ok(String::new())
                }
            }
            ["set-option", "-gu", name] => {
                server.options.remove(*name);
                if let Some(hook) = state.on_unset.as_mut() {
                    hook();
                }
                ok(String::new())
            }
            ["list-clients", ..] => {
                if fail_polls {
                    CommandOutput {
                        stdout: Vec::new(),
                        stderr: b"server exited unexpectedly\n".to_vec(),
                        status: 1,
                    }
                } else {
                    ok(format!("main {CLIENT_ACTIVITY}\n"))
                }
            }
            ["list-windows", ..] => ok("1 @0 0 shell\n".to_owned()),
            ["list-panes", ..] => ok("%0 0 0 0 80 24 1 0 0 1 0 0\n".to_owned()),
            ["capture-pane", ..] => ok(format!("fake pane content {content}\n")),
            ["display-message", "-p", "-t", _, "#{pane_title}"] => ok("editor\n".to_owned()),
            ["display-message", "-p", "-t", _, "#{pane_current_path}"] => {
                ok("/home/user/project\n".to_owned())
            }
            ["display-message", "-p", "-t", _, "#{pane_pid}"] => ok("4242\n".to_owned()),
            _ => CommandOutput {
                stdout: Vec::new(),
                stderr: format!("fake tmux does not know {args:?}\n").into_bytes(),
                status: 1,
            },
        }
    }
}

impl CommandRunner for FakeTmux {
    fn run<'a>(
        &'a self,
        invocation: CommandInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<CommandOutput, CommandError>> + Send + 'a>> {
        Box::pin(async move {
            let args = invocation
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            Ok(self.answer(&args))
        })
    }
}

/// A clock that moves only when told: wall time is the base plus the seconds
/// advanced, and monotonic time is the seconds advanced.
pub struct SteppedClock {
    base_wall: time::OffsetDateTime,
    seconds: std::sync::atomic::AtomicU64,
}

impl SteppedClock {
    pub fn new(base_wall: time::OffsetDateTime) -> Self {
        Self {
            base_wall,
            seconds: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn advance(&self, seconds: u64) {
        self.seconds
            .fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
    }

    fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.seconds.load(std::sync::atomic::Ordering::SeqCst))
    }
}

impl solstone_terminal::clock::Clock for SteppedClock {
    fn wall_now(&self) -> time::OffsetDateTime {
        self.base_wall + self.elapsed()
    }

    fn monotonic_now(&self) -> std::time::Duration {
        self.elapsed()
    }

    fn offset_at(&self, _instant: time::OffsetDateTime) -> time::UtcOffset {
        time::UtcOffset::UTC
    }
}

pub struct UtcZoneSource;

impl solstone_terminal::clock::ZoneSource for UtcZoneSource {
    fn read(&mut self) -> Result<solstone_terminal::clock::Zone, String> {
        Ok(solstone_terminal::clock::Zone::utc())
    }
}

pub const OWNER_STATUS_LEFT: &str = "#[fg=green]#S#[default]";
pub const OWNER_STATUS_LEFT_LENGTH: &str = "40";

pub fn owner_options() -> [(&'static str, &'static str); 2] {
    [
        ("status-left", OWNER_STATUS_LEFT),
        ("status-left-length", OWNER_STATUS_LEFT_LENGTH),
    ]
}

/// Lets spawned tasks run until they wait on time or on a channel.
pub async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

/// An engine driven through its public API against a [`FakeTmux`], on paused
/// tokio time and a [`SteppedClock`] that move together. Segments go to
/// `<root>/<day>/<stream>`.
pub struct EngineHarness {
    pub temp: TestDirectory,
    pub tmux: PathBuf,
    pub root: PathBuf,
    pub stream: String,
    pub lock_path: PathBuf,
    pub clock: Arc<SteppedClock>,
    pub fake: FakeTmux,
    closed: Arc<Mutex<Vec<solstone_terminal::ClosedSegment>>>,
    diagnostics: Arc<Mutex<Vec<solstone_terminal::Diagnostic>>>,
    health: Arc<Mutex<Vec<solstone_terminal::TmuxHealth>>>,
}

impl EngineHarness {
    /// Starts tokio's clock paused at 2026-07-01 12:00:00 UTC and writes an
    /// executable at the tmux path.
    pub fn new(label: &str, fake: FakeTmux) -> Self {
        tokio::time::pause();
        let temp = TestDirectory::new(label);
        let tmux = temp.path().join("bin/tmux");
        write_executable(&tmux);
        let root = temp.path().join("data");
        let lock_path = temp.path().join("engine.lock");
        let base = time::PrimitiveDateTime::new(
            time::Date::from_calendar_date(2026, time::Month::July, 1).expect("date"),
            time::Time::from_hms(12, 0, 0).expect("time"),
        )
        .assume_utc();
        Self {
            temp,
            tmux,
            root,
            stream: "stream.tmux".to_owned(),
            lock_path,
            clock: Arc::new(SteppedClock::new(base)),
            fake,
            closed: Arc::default(),
            diagnostics: Arc::default(),
            health: Arc::default(),
        }
    }

    pub fn settings() -> solstone_terminal::CaptureSettings {
        solstone_terminal::CaptureSettings {
            capture_interval: 5,
            segment_interval: 300,
            status_indicator: true,
            pane_title: true,
            working_directory: true,
            executable: true,
            output_sessions: Vec::new(),
        }
    }

    pub fn stream_dir(&self, day: &str) -> PathBuf {
        self.root.join(day).join(&self.stream)
    }

    pub async fn start(
        &self,
        settings: solstone_terminal::CaptureSettings,
    ) -> Result<solstone_terminal::EngineHandle, solstone_terminal::EngineError> {
        self.start_with_zone(settings, Box::new(UtcZoneSource))
            .await
    }

    pub async fn start_with_zone(
        &self,
        settings: solstone_terminal::CaptureSettings,
        zone_source: Box<dyn solstone_terminal::clock::ZoneSource>,
    ) -> Result<solstone_terminal::EngineHandle, solstone_terminal::EngineError> {
        let root = self.root.clone();
        let stream = self.stream.clone();
        let closed = Arc::clone(&self.closed);
        let diagnostics = Arc::clone(&self.diagnostics);
        let health = Arc::clone(&self.health);
        let config = solstone_terminal::EngineConfig {
            root: self.root.clone(),
            layout: Arc::new(move |day: &str| root.join(day).join(&stream)),
            lock_path: self.lock_path.clone(),
            tmux_path: self.tmux.clone(),
            settings,
            closed_segments: Arc::new(move |segment| {
                closed.lock().expect("closed poisoned").push(segment);
            }),
            diagnostics: Arc::new(move |diagnostic| {
                diagnostics
                    .lock()
                    .expect("diagnostics poisoned")
                    .push(diagnostic);
            }),
            health: Arc::new(move |state| {
                health.lock().expect("health poisoned").push(state);
            }),
        };
        let deps = solstone_terminal::EngineDependencies {
            runner: Arc::new(self.fake.clone()),
            clock: Arc::clone(&self.clock) as Arc<dyn solstone_terminal::clock::Clock>,
            zone_source,
            lookup: Some(Arc::new(FakeLookup::new(
                Arc::default(),
                Ok("vim".to_owned()),
            ))),
        };
        let handle = solstone_terminal::start_with_dependencies(config, deps).await?;
        // The engine task sets its timers when it first runs. Paused tokio time
        // fires a timer only once time passes its deadline, rounded up to the next
        // millisecond: this lets the first tick run.
        settle().await;
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        settle().await;
        Ok(handle)
    }

    /// Moves both clocks forward one capture interval at a time.
    pub async fn advance(&self, seconds: u64, capture_interval: u64) {
        let mut remaining = seconds;
        while remaining > 0 {
            let step = remaining.min(capture_interval);
            self.clock.advance(step);
            tokio::time::advance(std::time::Duration::from_secs(step)).await;
            settle().await;
            remaining -= step;
        }
    }

    pub fn closed(&self) -> Vec<solstone_terminal::ClosedSegment> {
        self.closed.lock().expect("closed poisoned").clone()
    }

    pub fn diagnostics(&self) -> Vec<solstone_terminal::Diagnostic> {
        self.diagnostics
            .lock()
            .expect("diagnostics poisoned")
            .clone()
    }

    pub fn health(&self) -> Vec<solstone_terminal::TmuxHealth> {
        self.health.lock().expect("health poisoned").clone()
    }

    pub fn lock_is_free(&self) -> bool {
        solstone_terminal::instance_lock::InstanceLock::acquire_path(&self.lock_path).is_ok()
    }
}

pub fn write_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create executable parent");
    }
    std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make executable");
}

/// Whether permission bits can make a directory unwritable for this process.
pub fn permissions_are_enforced() -> bool {
    !rustix::process::geteuid().is_root()
}

pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("set mode");
}
