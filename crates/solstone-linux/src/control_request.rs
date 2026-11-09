// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Live control of the running app without a session bus.
//!
//! Each request is its own file under `state/requests/`, named by a unique id and
//! written atomically, carrying the command, its arguments and its creation time.
//! The daemon checks every few seconds, claims a request by renaming it before
//! acting, applies it on its own thread (so a long request never holds up a later
//! one), and writes a result file for that id: an exit code and one owner-readable
//! line. The CLI waits for its own result within the command's bound; on timeout
//! it withdraws its request if still unclaimed. A claimed request may still apply,
//! which is safe because every command is idempotent.
//!
//! The daemon ignores an unclaimed request older than its bound, answers one it
//! cannot read with an error result, and deletes result files older than the
//! longest bound.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const REQUESTS_DIR: &str = "requests";
/// The bound for a setting change, a pause and a resume.
pub const SETTING_BOUND: Duration = Duration::from_secs(15);
/// The longest bound any command has; results older than this are deleted.
pub const LONGEST_BOUND: Duration = Duration::from_secs(180);
/// How often the daemon looks for requests.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The daemon's advertisement of what it accepts.
pub const DAEMON_FILENAME: &str = "daemon.json";

const REQUEST_SUFFIX: &str = ".request";
const CLAIMED_SUFFIX: &str = ".claimed";
const RESULT_SUFFIX: &str = ".result";
/// The code a request that could not be read is answered with.
pub const MALFORMED_CODE: i32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Command {
    TerminalOn,
    TerminalOff,
    DesktopCaptureOn,
    DesktopCaptureOff,
    Pause {
        #[serde(default)]
        minutes: Option<u64>,
    },
    Resume,
}

impl Command {
    /// How long the CLI waits for the result, and how long an unclaimed request
    /// stays valid.
    pub fn bound(&self) -> Duration {
        SETTING_BOUND
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::TerminalOn => "terminal-on",
            Self::TerminalOff => "terminal-off",
            Self::DesktopCaptureOn => "desktop-capture-on",
            Self::DesktopCaptureOff => "desktop-capture-off",
            Self::Pause { .. } => "pause",
            Self::Resume => "resume",
        }
    }

    /// Every command this daemon accepts, as advertised.
    pub fn supported() -> Vec<&'static str> {
        vec![
            Self::TerminalOn.name(),
            Self::TerminalOff.name(),
            Self::DesktopCaptureOn.name(),
            Self::DesktopCaptureOff.name(),
            Self::Pause { minutes: None }.name(),
            Self::Resume.name(),
        ]
    }
}

/// The daemon's answer: an exit code and one line for the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub code: i32,
    pub line: String,
}

impl Outcome {
    pub fn ok(line: impl Into<String>) -> Self {
        Self {
            code: 0,
            line: line.into(),
        }
    }

    pub fn failed(code: i32, line: impl Into<String>) -> Self {
        Self {
            code,
            line: line.into(),
        }
    }
}

/// Applies a claimed request: persists any setting change and starts its effect.
pub trait Applier: Send + Sync {
    fn apply(&self, command: &Command) -> Outcome;
}

#[derive(Serialize, Deserialize)]
struct ResultDocument {
    #[serde(flatten)]
    outcome: Outcome,
    written_at: f64,
}

#[derive(Serialize, Deserialize)]
struct DaemonDocument {
    pid: u32,
    start_time: Option<u64>,
    commands: Vec<String>,
}

pub fn requests_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(REQUESTS_DIR)
}

fn wall_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn new_id() -> io::Result<String> {
    use std::io::Read;
    let mut bytes = [0_u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ensure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::private_file::atomic_write_bytes(path, bytes).map_err(io::Error::other)
}

/// Writes a request and returns its id.
pub fn submit(state_dir: &Path, command: &Command) -> io::Result<String> {
    submit_at(state_dir, command, wall_seconds())
}

fn submit_at(state_dir: &Path, command: &Command, created_at: f64) -> io::Result<String> {
    let dir = requests_dir(state_dir);
    ensure_dir(&dir)?;
    let id = new_id()?;
    let mut document = serde_json::to_value(command).map_err(io::Error::other)?;
    if let Some(map) = document.as_object_mut() {
        map.insert("created_at".into(), Value::from(created_at));
    }
    let mut bytes = serde_json::to_vec(&document).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_atomic(&dir.join(format!("{id}{REQUEST_SUFFIX}")), &bytes)?;
    Ok(id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitResult {
    Answered(Outcome),
    /// No answer within the bound. `withdrawn` is true when the request was still
    /// unclaimed and has been removed, so it will never apply.
    TimedOut {
        withdrawn: bool,
    },
}

/// Waits for the answer to `id`, polling every `poll`, for at most `bound`.
pub fn wait(state_dir: &Path, id: &str, bound: Duration, poll: Duration) -> WaitResult {
    let dir = requests_dir(state_dir);
    let result_path = dir.join(format!("{id}{RESULT_SUFFIX}"));
    let deadline = Instant::now() + bound;
    loop {
        if let Ok(bytes) = fs::read(&result_path)
            && let Ok(document) = serde_json::from_slice::<ResultDocument>(&bytes)
        {
            let _ = fs::remove_file(&result_path);
            return WaitResult::Answered(document.outcome);
        }
        if Instant::now() >= deadline {
            let withdrawn = fs::remove_file(dir.join(format!("{id}{REQUEST_SUFFIX}"))).is_ok();
            return WaitResult::TimedOut { withdrawn };
        }
        thread::sleep(poll);
    }
}

/// Submits `command` and waits for its answer within its bound.
pub fn request(state_dir: &Path, command: &Command) -> io::Result<WaitResult> {
    let id = submit(state_dir, command)?;
    Ok(wait(
        state_dir,
        &id,
        command.bound(),
        Duration::from_millis(100),
    ))
}

/// The daemon side: claims and applies requests on its own thread.
pub struct RequestServer {
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RequestServer {
    /// Advertises the supported commands, then starts looking for requests.
    pub fn spawn(
        state_dir: &Path,
        applier: Arc<dyn Applier>,
        interval: Duration,
    ) -> io::Result<Self> {
        let dir = requests_dir(state_dir);
        ensure_dir(&dir)?;
        advertise(&dir)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let thread = thread::Builder::new()
            .name("solstone-requests".into())
            .spawn(move || {
                let in_flight = Arc::new(Mutex::new(Vec::new()));
                while !worker_stopped.load(Ordering::Acquire) {
                    serve_once(&dir, &applier, wall_seconds(), &in_flight);
                    let deadline = Instant::now() + interval;
                    while Instant::now() < deadline && !worker_stopped.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(50).min(interval));
                    }
                }
                let handles: Vec<thread::JoinHandle<()>> =
                    std::mem::take(&mut *in_flight.lock().unwrap_or_else(|p| p.into_inner()));
                for handle in handles {
                    let _ = handle.join();
                }
            })?;
        Ok(Self {
            stopped,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RequestServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn advertise(dir: &Path) -> io::Result<()> {
    let document = DaemonDocument {
        pid: std::process::id(),
        start_time: process_start_time(),
        commands: Command::supported()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    let mut bytes = serde_json::to_vec(&document).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_atomic(&dir.join(DAEMON_FILENAME), &bytes)
}

/// This process's start time in clock ticks since boot, from `/proc/self/stat`.
fn process_start_time() -> Option<u64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let after_name = stat.rsplit_once(')')?.1;
    after_name.split_whitespace().nth(19)?.parse().ok()
}

fn parse_request(bytes: &[u8]) -> Option<(Command, f64)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let created_at = value.get("created_at")?.as_f64()?;
    let command = serde_json::from_value(value).ok()?;
    Some((command, created_at))
}

fn write_result(dir: &Path, id: &str, outcome: Outcome) {
    let document = ResultDocument {
        outcome,
        written_at: wall_seconds(),
    };
    let result = serde_json::to_vec(&document)
        .map_err(io::Error::other)
        .and_then(|mut bytes| {
            bytes.push(b'\n');
            write_atomic(&dir.join(format!("{id}{RESULT_SUFFIX}")), &bytes)
        });
    if let Err(error) = result {
        tracing::warn!(%error, "Could not answer a control request");
    }
}

fn age(now: f64, then: f64) -> Duration {
    Duration::try_from_secs_f64((now - then).max(0.0)).unwrap_or(Duration::MAX)
}

fn file_written_at(path: &Path) -> Option<f64> {
    let bytes = fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("written_at")
        .or_else(|| value.get("created_at"))
        .and_then(Value::as_f64)
}

fn modified_at(path: &Path) -> Option<f64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|age| age.as_secs_f64())
}

/// One look at the requests directory.
fn serve_once(
    dir: &Path,
    applier: &Arc<dyn Applier>,
    now: f64,
    in_flight: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    in_flight
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|handle| !handle.is_finished());
    for name in names {
        let path = dir.join(&name);
        if let Some(id) = name.strip_suffix(REQUEST_SUFFIX) {
            if !valid_id(id) {
                let _ = fs::remove_file(&path);
                continue;
            }
            let parsed = fs::read(&path).ok().map(|bytes| parse_request(&bytes));
            let Some(parsed) = parsed else {
                // Withdrawn between the listing and the read.
                continue;
            };
            if let Some((command, created_at)) = &parsed
                && age(now, *created_at) > command.bound()
            {
                tracing::info!(
                    command = command.name(),
                    "Ignoring an expired control request"
                );
                let _ = fs::remove_file(&path);
                continue;
            }
            let claimed = dir.join(format!("{id}{CLAIMED_SUFFIX}"));
            if fs::rename(&path, &claimed).is_err() {
                continue;
            }
            let Some((command, _)) = parsed else {
                write_result(
                    dir,
                    id,
                    Outcome::failed(MALFORMED_CODE, "the request could not be read"),
                );
                let _ = fs::remove_file(&claimed);
                continue;
            };
            let applier = Arc::clone(applier);
            let dir = dir.to_owned();
            let id = id.to_owned();
            let worker = thread::Builder::new()
                .name("solstone-request".into())
                .spawn(move || {
                    let outcome = applier.apply(&command);
                    write_result(&dir, &id, outcome);
                    let _ = fs::remove_file(&claimed);
                });
            match worker {
                Ok(handle) => in_flight
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(handle),
                Err(error) => tracing::warn!(%error, "Could not apply a control request"),
            }
        } else if name.ends_with(RESULT_SUFFIX) || name.ends_with(CLAIMED_SUFFIX) {
            let written = file_written_at(&path).or_else(|| modified_at(&path));
            if written.is_some_and(|written| age(now, written) > LONGEST_BOUND) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recording {
        applied: Mutex<Vec<Command>>,
    }

    impl Applier for Recording {
        fn apply(&self, command: &Command) -> Outcome {
            self.applied.lock().unwrap().push(command.clone());
            Outcome::ok(format!("applied {}", command.name()))
        }
    }

    fn serve(dir: &Path, applier: Arc<dyn Applier>, now: f64) {
        let in_flight = Arc::new(Mutex::new(Vec::new()));
        serve_once(&requests_dir(dir), &applier, now, &in_flight);
        for handle in std::mem::take(&mut *in_flight.lock().unwrap()) {
            handle.join().unwrap();
        }
    }

    #[test]
    fn commands_round_trip_with_their_arguments() {
        for command in [
            Command::TerminalOn,
            Command::TerminalOff,
            Command::DesktopCaptureOn,
            Command::DesktopCaptureOff,
            Command::Pause { minutes: Some(30) },
            Command::Pause { minutes: None },
            Command::Resume,
        ] {
            let mut value = serde_json::to_value(&command).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("created_at".into(), Value::from(5.0));
            let parsed = parse_request(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(parsed, (command, 5.0));
        }
    }

    #[test]
    fn two_requests_at_once_each_get_their_own_answer() {
        let temp = tempfile::tempdir().unwrap();
        let applier = Arc::new(Recording::default());
        let first = submit(temp.path(), &Command::TerminalOn).unwrap();
        let second = submit(temp.path(), &Command::Pause { minutes: Some(5) }).unwrap();
        serve(temp.path(), applier.clone(), wall_seconds());
        let mut applied = applier.applied.lock().unwrap().clone();
        applied.sort_by_key(|command| command.name());
        assert_eq!(
            applied,
            [Command::Pause { minutes: Some(5) }, Command::TerminalOn]
        );
        let poll = Duration::from_millis(10);
        assert_eq!(
            wait(temp.path(), &first, SETTING_BOUND, poll),
            WaitResult::Answered(Outcome::ok("applied terminal-on"))
        );
        assert_eq!(
            wait(temp.path(), &second, SETTING_BOUND, poll),
            WaitResult::Answered(Outcome::ok("applied pause"))
        );
        assert_eq!(
            fs::read_dir(requests_dir(temp.path())).unwrap().count(),
            0,
            "nothing is left behind once both are answered"
        );
    }

    #[test]
    fn an_expired_request_is_never_applied() {
        let temp = tempfile::tempdir().unwrap();
        let applier = Arc::new(Recording::default());
        let now = wall_seconds();
        let id = submit_at(
            temp.path(),
            &Command::TerminalOn,
            now - SETTING_BOUND.as_secs_f64() - 1.0,
        )
        .unwrap();
        serve(temp.path(), applier.clone(), now);
        assert!(applier.applied.lock().unwrap().is_empty());
        assert!(
            !requests_dir(temp.path())
                .join(format!("{id}.request"))
                .exists()
        );
    }

    #[cfg(feature = "full-tests")]
    #[test]
    fn a_request_withdrawn_on_timeout_is_not_applied_later() {
        let temp = tempfile::tempdir().unwrap();
        let id = submit(temp.path(), &Command::TerminalOn).unwrap();
        assert_eq!(
            wait(
                temp.path(),
                &id,
                Duration::from_millis(30),
                Duration::from_millis(10)
            ),
            WaitResult::TimedOut { withdrawn: true }
        );
        // The daemon starts afterwards.
        let applier = Arc::new(Recording::default());
        serve(temp.path(), applier.clone(), wall_seconds());
        assert!(applier.applied.lock().unwrap().is_empty());
    }

    #[test]
    fn a_malformed_request_is_answered_with_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let dir = requests_dir(temp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("abcd.request"), b"{\"command\":\"reboot\"}").unwrap();
        let applier = Arc::new(Recording::default());
        serve(temp.path(), applier.clone(), wall_seconds());
        assert!(applier.applied.lock().unwrap().is_empty());
        match wait(
            temp.path(),
            "abcd",
            Duration::from_millis(10),
            Duration::from_millis(5),
        ) {
            WaitResult::Answered(outcome) => assert_eq!(outcome.code, MALFORMED_CODE),
            other => panic!("expected an error result, got {other:?}"),
        }
    }

    #[test]
    fn results_and_claims_older_than_the_longest_bound_are_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let dir = requests_dir(temp.path());
        fs::create_dir_all(&dir).unwrap();
        let now = wall_seconds();
        let old = now - LONGEST_BOUND.as_secs_f64() - 1.0;
        let recent = now - 1.0;
        fs::write(
            dir.join("aa.result"),
            format!("{{\"code\":0,\"line\":\"x\",\"written_at\":{old}}}"),
        )
        .unwrap();
        fs::write(
            dir.join("bb.result"),
            format!("{{\"code\":0,\"line\":\"x\",\"written_at\":{recent}}}"),
        )
        .unwrap();
        fs::write(
            dir.join("cc.claimed"),
            format!("{{\"command\":\"resume\",\"created_at\":{old}}}"),
        )
        .unwrap();
        serve(temp.path(), Arc::new(Recording::default()), now);
        assert!(!dir.join("aa.result").exists());
        assert!(dir.join("bb.result").exists());
        assert!(!dir.join("cc.claimed").exists());
    }

    #[cfg(feature = "full-tests")]
    #[test]
    fn a_pause_is_answered_while_a_long_request_runs() {
        use std::sync::mpsc;
        /// Blocks `terminal on` until released, as a takeover-length request would.
        struct Slow {
            release: Mutex<Option<mpsc::Receiver<()>>>,
        }
        impl Applier for Slow {
            fn apply(&self, command: &Command) -> Outcome {
                if *command == Command::TerminalOn
                    && let Some(release) = self.release.lock().unwrap().take()
                {
                    let _ = release.recv();
                }
                Outcome::ok(command.name())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let (release, held) = mpsc::channel();
        let server = RequestServer::spawn(
            temp.path(),
            Arc::new(Slow {
                release: Mutex::new(Some(held)),
            }),
            Duration::from_millis(50),
        )
        .unwrap();
        let long = submit(temp.path(), &Command::TerminalOn).unwrap();
        thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        assert_eq!(
            request(temp.path(), &Command::Pause { minutes: None }).unwrap(),
            WaitResult::Answered(Outcome::ok("pause"))
        );
        assert!(started.elapsed() < SETTING_BOUND);
        release.send(()).unwrap();
        assert_eq!(
            wait(temp.path(), &long, SETTING_BOUND, Duration::from_millis(20)),
            WaitResult::Answered(Outcome::ok("terminal-on"))
        );
        server.stop();
    }

    #[cfg(feature = "full-tests")]
    #[test]
    fn the_server_advertises_its_commands_and_process() {
        let temp = tempfile::tempdir().unwrap();
        let server = RequestServer::spawn(
            temp.path(),
            Arc::new(Recording::default()),
            Duration::from_millis(50),
        )
        .unwrap();
        let document: DaemonDocument = serde_json::from_slice(
            &fs::read(requests_dir(temp.path()).join(DAEMON_FILENAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(document.pid, std::process::id());
        assert!(document.start_time.is_some());
        assert!(document.commands.contains(&"terminal-on".to_owned()));
        server.stop();
    }
}
