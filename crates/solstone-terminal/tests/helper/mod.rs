// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![cfg(feature = "full-tests")]

use std::fs::{self, Permissions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crate::support::TestDirectory;
use serde_json::Value;
use solstone_terminal::instance_lock::InstanceLock;

const FAKE_TMUX_SCRIPT: &str = r##"#!/usr/bin/env python3
import sys, os, time, json

log_path = os.environ.get("FAKE_TMUX_LOG")
if log_path:
    with open(log_path, "a") as f:
        f.write(" ".join(sys.argv[1:]) + "\n")

state_path = os.environ.get("FAKE_TMUX_STATE")
def load_state():
    if state_path and os.path.exists(state_path):
        try:
            with open(state_path, "r") as f:
                return json.load(f)
        except Exception:
            pass
    return {}

def save_state(s):
    if state_path:
        tmp = state_path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(s, f)
        os.replace(tmp, state_path)

args = sys.argv[1:]
if not args:
    sys.exit(0)

# display-message
if args[0] == "display-message":
    if len(args) >= 3 and args[1] == "-p" and "#{pid} #{start_time}" in args[2]:
        sys.stdout.write("1234 1000\n")
        sys.exit(0)
    elif len(args) >= 5 and args[1] == "-p" and args[2] == "-t":
        fmt = args[4]
        if "#{pane_title}" in fmt:
            sys.stdout.write("fake pane title\n")
        elif "#{pane_current_path}" in fmt:
            sys.stdout.write("/tmp\n")
        elif "#{pane_pid}" in fmt:
            sys.stdout.write(f"{os.getppid()}\n")
        else:
            sys.stdout.write("\n")
        sys.exit(0)

# show-options
if args[0] == "show-options":
    if len(args) >= 3 and args[1] == "-gv":
        name = args[2]
        st = load_state()
        if name in st:
            sys.stdout.write(f"{st[name]}\n")
            sys.exit(0)
        else:
            sys.exit(1)
    elif len(args) >= 4 and args[1] == "-v" and args[2] == "-t":
        name = args[4] if len(args) > 4 else ""
        if name == "pane-border-status":
            sys.stdout.write("off\n")
        elif name == "pane-border-format":
            sys.stdout.write("\n")
        sys.exit(0)

# set-option
if args[0] == "set-option":
    if len(args) >= 4 and args[1] == "-g":
        name = args[2]
        val = args[3]
        st = load_state()
        st[name] = val
        save_state(st)
        sys.exit(0)
    elif len(args) >= 3 and args[1] == "-gu":
        name = args[2]
        st = load_state()
        st.pop(name, None)
        save_state(st)
        sys.exit(0)

# list-clients
if args[0] == "list-clients":
    if os.environ.get("FAKE_TMUX_FAIL_POLLS"):
        sys.stderr.write("server exited unexpectedly\n")
        sys.exit(1)
    now = int(time.time())
    sys.stdout.write(f"main {now}\n")
    sys.exit(0)

# list-sessions
if args[0] == "list-sessions":
    now = int(time.time())
    sys.stdout.write(f"main {now}\n")
    sys.exit(0)

# list-windows
if args[0] == "list-windows":
    sys.stdout.write("1 @0 0 main\n")
    sys.exit(0)

# list-panes
if args[0] == "list-panes":
    sys.stdout.write("%0 0 0 0 80 24 1 0 0 1 0 0\n")
    sys.exit(0)

# capture-pane
if args[0] == "capture-pane":
    sys.stdout.write("hello terminal capture\n")
    sys.exit(0)

sys.exit(0)
"##;

struct FakeTmuxEnvironment {
    tmux_bin: PathBuf,
    log_path: PathBuf,
    state_path: PathBuf,
}

impl FakeTmuxEnvironment {
    fn new(dir: &Path) -> Self {
        let bin_dir = dir.join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        let tmux_bin = bin_dir.join("fake-tmux");
        fs::write(&tmux_bin, FAKE_TMUX_SCRIPT).expect("write fake tmux");
        fs::set_permissions(&tmux_bin, Permissions::from_mode(0o755)).expect("chmod fake tmux");

        let log_path = dir.join("fake_tmux.log");
        let state_path = dir.join("fake_tmux_state.json");

        Self {
            tmux_bin,
            log_path,
            state_path,
        }
    }

    fn init_state(&self, status_left: &str, status_left_length: &str) {
        let mut map = serde_json::Map::new();
        map.insert(
            "status-left".to_string(),
            Value::String(status_left.to_string()),
        );
        map.insert(
            "status-left-length".to_string(),
            Value::String(status_left_length.to_string()),
        );
        fs::write(&self.state_path, Value::Object(map).to_string())
            .expect("write initial fake tmux state");
    }

    fn read_state(&self) -> Value {
        let bytes = fs::read(&self.state_path).unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }
}

/// The fake tmux's panes have no terminal, so the executable lookup is off: it
/// would warn on stderr at every poll.
fn write_settings(dir: &Path, capture_interval: u64, segment_interval: u64) -> PathBuf {
    let path = dir.join("settings.json");
    let json = format!(
        r#"{{"capture_interval":{capture_interval},"segment_interval":{segment_interval},"executable":false}}"#
    );
    fs::write(&path, json).expect("write settings");
    fs::set_permissions(&path, Permissions::from_mode(0o600)).expect("chmod settings");
    path
}

struct TestProcess {
    child: Child,
    stdout_rx: Receiver<String>,
    stderr_rx: Receiver<String>,
}

impl TestProcess {
    fn spawn(spool: &Path, tmux: &FakeTmuxEnvironment, settings: Option<&Path>) -> Self {
        Self::spawn_with_env(spool, tmux, settings, &[])
    }

    fn spawn_with_env(
        spool: &Path,
        tmux: &FakeTmuxEnvironment,
        settings: Option<&Path>,
        env: &[(&str, &str)],
    ) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"));
        cmd.envs(env.iter().copied());
        cmd.arg("run")
            .arg("--spool")
            .arg(spool)
            .arg("--tmux")
            .arg(&tmux.tmux_bin)
            .env("FAKE_TMUX_LOG", &tmux.log_path)
            .env("FAKE_TMUX_STATE", &tmux.state_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(s) = settings {
            cmd.arg("--settings").arg(s);
        }

        let mut child = cmd.spawn().expect("spawn solstone-terminal");

        let stdout = child.stdout.take().expect("child stdout");
        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = stdout_tx.send(line);
            }
        });

        let stderr = child.stderr.take().expect("child stderr");
        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let _ = stderr_tx.send(line);
            }
        });

        Self {
            child,
            stdout_rx,
            stderr_rx,
        }
    }

    fn write_stdin(&mut self, text: &str) {
        if let Some(stdin) = &mut self.child.stdin {
            stdin.write_all(text.as_bytes()).expect("write stdin");
            stdin.flush().expect("flush stdin");
        }
    }

    fn write_stdin_bytes(&mut self, bytes: &[u8]) {
        if let Some(stdin) = &mut self.child.stdin {
            stdin.write_all(bytes).expect("write stdin");
            stdin.flush().expect("flush stdin");
        }
    }

    /// Waits for the process to exit, failing the test after `timeout`.
    fn wait_within(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll child") {
                return status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("solstone-terminal did not exit within {timeout:?}");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn close_stdin(&mut self) {
        drop(self.child.stdin.take());
    }

    fn recv_stdout_line(&self, timeout: Duration) -> String {
        self.stdout_rx
            .recv_timeout(timeout)
            .expect("receive stdout line within timeout")
    }

    fn recv_stdout_json(&self, timeout: Duration) -> Value {
        let line = self.recv_stdout_line(timeout);
        serde_json::from_str(&line).expect("parse json from stdout line")
    }

    fn collect_remaining_stdout(&self) -> Vec<Value> {
        let mut list = Vec::new();
        while let Ok(line) = self.stdout_rx.recv_timeout(Duration::from_millis(200)) {
            if let Ok(val) = serde_json::from_str(&line) {
                list.push(val);
            }
        }
        list
    }

    fn collect_remaining_stderr(&self) -> Vec<String> {
        let mut list = Vec::new();
        while let Ok(line) = self.stderr_rx.recv_timeout(Duration::from_millis(200)) {
            list.push(line);
        }
        list
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        self.child.wait().expect("wait on child")
    }
}

#[test]
fn version_line_with_and_without_source_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("--version")
        .output()
        .expect("run solstone-terminal --version");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert_eq!(stdout, "solstone-terminal 2.0.36 (source development)\n");
}

#[test]
fn run_prints_ready_then_a_rotation_segment() {
    let temp = TestDirectory::new("run-rotation");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    fake_tmux.init_state("#[fg=white]#S#[default]", "40");

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));

    // 1. Ready event
    let ready = proc.recv_stdout_json(Duration::from_secs(3));
    assert_eq!(ready["event"], "ready");
    assert_eq!(ready["version"], "2.0.36");

    // 2. Health event
    let health = proc.recv_stdout_json(Duration::from_secs(3));
    assert_eq!(health["event"], "health");
    assert_eq!(health["tmux"], "found");

    // 3. Rotation segment event arrives within 3 seconds
    let segment = proc.recv_stdout_json(Duration::from_secs(4));
    assert_eq!(segment["event"], "segment");
    let dir_str = segment["dir"].as_str().expect("dir is string");
    let seg_dir = PathBuf::from(dir_str);
    assert!(seg_dir.is_dir(), "segment directory must exist: {dir_str}");

    // Check directory contains tmux_*_screen.jsonl and *_desktop_activity.jsonl
    let entries = fs::read_dir(&seg_dir)
        .expect("read segment dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        entries
            .iter()
            .any(|e| e.starts_with("tmux_") && e.ends_with("_screen.jsonl")),
        "must contain tmux_*_screen.jsonl, got: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.ends_with("_desktop_activity.jsonl")),
        "must contain *_desktop_activity.jsonl, got: {entries:?}"
    );

    proc.close_stdin();
    let status = proc.wait();
    assert!(status.success(), "process must exit 0, got: {status:?}");
}

#[test]
fn sync_working_sets_solstone_to_syncing() {
    let temp = TestDirectory::new("sync-working");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    fake_tmux.init_state("#[fg=white]#S#[default]", "40");

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));

    let ready = proc.recv_stdout_json(Duration::from_secs(3));
    assert_eq!(ready["event"], "ready");

    // Send working sync state
    proc.write_stdin("{\"cmd\":\"sync\",\"state\":\"working\"}\n");

    // Poll until @solstone is recorded as syncing
    let mut found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_millis(50));
        let state = fake_tmux.read_state();
        if state.get("@solstone").and_then(|v| v.as_str()) == Some("syncing") {
            found = true;
            break;
        }
    }
    assert!(found, "fake tmux state must show @solstone = syncing");

    proc.close_stdin();
    assert!(proc.wait().success());
}

#[test]
fn stdin_close_prints_segment_then_stopped_and_restores() {
    let temp = TestDirectory::new("stdin-close");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    let initial_status_left = "#[fg=yellow]#S ";
    let initial_length = "42";
    fake_tmux.init_state(initial_status_left, initial_length);

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));

    let ready = proc.recv_stdout_json(Duration::from_secs(3));
    assert_eq!(ready["event"], "ready");

    // Let at least one capture happen
    thread::sleep(Duration::from_millis(1500));

    // Close stdin
    proc.close_stdin();
    let status = proc.wait();
    assert!(status.success(), "process must exit 0 on stdin close");

    // Collect all remaining stdout events
    let events = proc.collect_remaining_stdout();
    assert!(
        events.len() >= 2,
        "must have at least segment and stopped events, got: {events:?}"
    );

    let last = &events[events.len() - 1];
    assert_eq!(last["event"], "stopped");

    let second_last = &events[events.len() - 2];
    assert_eq!(second_last["event"], "segment");

    // Options restored
    let state = fake_tmux.read_state();
    assert_eq!(state["status-left"], initial_status_left);
    assert_eq!(state["status-left-length"], initial_length);
    assert!(
        state.get("@solstone").is_none(),
        "@solstone option must be unset upon stop"
    );
}

#[test]
fn sigterm_and_sigint_do_the_same() {
    for signal in [rustix::process::Signal::TERM, rustix::process::Signal::INT] {
        let temp = TestDirectory::new("signal-test");
        let spool = temp.path().join("spool");
        fs::create_dir_all(&spool).expect("create spool");

        let fake_tmux = FakeTmuxEnvironment::new(temp.path());
        let initial_status_left = "#[fg=blue]#S ";
        let initial_length = "38";
        fake_tmux.init_state(initial_status_left, initial_length);

        let settings = write_settings(temp.path(), 1, 1);
        let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));

        let ready = proc.recv_stdout_json(Duration::from_secs(3));
        assert_eq!(ready["event"], "ready");

        thread::sleep(Duration::from_millis(1500));

        let pid = proc.child.id() as i32;
        rustix::process::kill_process(rustix::process::Pid::from_raw(pid).unwrap(), signal)
            .expect("send signal to child");

        let status = proc.wait();
        assert!(status.success(), "child must exit 0 after {signal:?}");

        let events = proc.collect_remaining_stdout();
        assert!(
            events.len() >= 2,
            "must have at least segment and stopped events after {signal:?}, got: {events:?}"
        );
        let last = &events[events.len() - 1];
        assert_eq!(last["event"], "stopped");
        let second_last = &events[events.len() - 2];
        assert_eq!(second_last["event"], "segment");

        let state = fake_tmux.read_state();
        assert_eq!(state["status-left"], initial_status_left);
        assert_eq!(state["status-left-length"], initial_length);
        assert!(state.get("@solstone").is_none());
    }
}

#[test]
fn closed_stdout_and_stdin_exits_0_and_finalizes() {
    let temp = TestDirectory::new("closed-stdout");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    let initial_status_left = "#[fg=cyan]#S ";
    let initial_length = "40";
    fake_tmux.init_state(initial_status_left, initial_length);

    let settings = write_settings(temp.path(), 1, 1);

    let mut child = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("run")
        .arg("--spool")
        .arg(&spool)
        .arg("--tmux")
        .arg(&fake_tmux.tmux_bin)
        .arg("--settings")
        .arg(&settings)
        .env("FAKE_TMUX_LOG", &fake_tmux.log_path)
        .env("FAKE_TMUX_STATE", &fake_tmux.state_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn solstone-terminal");

    let mut stdout = child.stdout.take().expect("child stdout");
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(&mut stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read ready line");
    assert!(line.contains("ready"), "line must contain ready: {line}");

    thread::sleep(Duration::from_millis(1500));

    // Close stdout pipe on reader side
    drop(stdout);
    // Close stdin
    drop(child.stdin.take());

    let status = child.wait().expect("child wait");
    assert!(
        status.success(),
        "process must exit 0 without panic on closed stdout, got: {status:?}"
    );

    // Verify open segment is finalized on disk
    let mut found_finalized = false;
    for day in fs::read_dir(&spool)
        .expect("read spool")
        .filter_map(Result::ok)
    {
        if day.file_type().map(|f| f.is_dir()).unwrap_or(false) {
            for seg in fs::read_dir(day.path())
                .expect("read day")
                .filter_map(Result::ok)
            {
                let name = seg.file_name().to_string_lossy().to_string();
                if name.contains('_')
                    && !name.ends_with(".incomplete")
                    && !name.ends_with(".json")
                    && seg.path().join("tmux_main_screen.jsonl").is_file()
                {
                    found_finalized = true;
                    break;
                }
            }
        }
    }
    assert!(found_finalized, "finalized segment must exist on disk");

    // Options restored
    let state = fake_tmux.read_state();
    assert_eq!(state["status-left"], initial_status_left);
    assert_eq!(state["status-left-length"], initial_length);
    assert!(state.get("@solstone").is_none());
}

#[test]
fn malformed_stdin_is_one_stderr_line_and_capture_continues() {
    let temp = TestDirectory::new("malformed-stdin");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    fake_tmux.init_state("#[fg=white]#S#[default]", "40");

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));

    let ready = proc.recv_stdout_json(Duration::from_secs(3));
    assert_eq!(ready["event"], "ready");

    // Write malformed line
    proc.write_stdin("this is not json\n");

    // Let capture continue
    thread::sleep(Duration::from_millis(1500));

    // Stop process
    proc.write_stdin("{\"cmd\":\"stop\"}\n");
    let status = proc.wait();
    assert!(status.success(), "process must exit 0, got: {status:?}");

    let stderr_lines = proc.collect_remaining_stderr();
    assert_eq!(stderr_lines.len(), 1, "{stderr_lines:?}");
    assert!(stderr_lines[0].starts_with("solstone-terminal: "));
}

#[test]
fn second_run_exits_75() {
    let temp = TestDirectory::new("second-run-75");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    fake_tmux.init_state("#[fg=white]#S#[default]", "40");

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc1 = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));

    let ready = proc1.recv_stdout_json(Duration::from_secs(3));
    assert_eq!(ready["event"], "ready");

    // Spawn second run with same spool
    let mut proc2 = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));
    let status2 = proc2.wait();
    assert_eq!(
        status2.code(),
        Some(75),
        "second process must exit 75, got: {status2:?}"
    );

    assert_eq!(proc2.collect_remaining_stderr().len(), 1);

    // Stop proc1 cleanly
    proc1.close_stdin();
    assert!(proc1.wait().success());
}

#[test]
fn unknown_settings_key_exits_64() {
    let temp = TestDirectory::new("unknown-key-64");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());

    let settings_path = temp.path().join("settings.json");
    fs::write(&settings_path, r#"{"invalid_key": true}"#).unwrap();
    fs::set_permissions(&settings_path, Permissions::from_mode(0o600)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("run")
        .arg("--spool")
        .arg(&spool)
        .arg("--tmux")
        .arg(&fake_tmux.tmux_bin)
        .arg("--settings")
        .arg(&settings_path)
        .output()
        .expect("run command");

    assert_eq!(output.status.code(), Some(64));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("solstone-terminal: "),
        "stderr must start with solstone-terminal:, got: {stderr}"
    );
    assert!(
        stderr.contains(&settings_path.display().to_string()),
        "stderr must name settings path, got: {stderr}"
    );

    // Spool must be unchanged (no lock file, no day directory)
    assert_eq!(
        fs::read_dir(&spool).unwrap().count(),
        0,
        "spool must be unchanged"
    );
}

#[test]
fn zero_interval_exits_64() {
    let temp = TestDirectory::new("zero-interval-64");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");

    let fake_tmux = FakeTmuxEnvironment::new(temp.path());

    let settings_path = temp.path().join("settings.json");
    fs::write(&settings_path, r#"{"capture_interval": 0}"#).unwrap();
    fs::set_permissions(&settings_path, Permissions::from_mode(0o600)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("run")
        .arg("--spool")
        .arg(&spool)
        .arg("--tmux")
        .arg(&fake_tmux.tmux_bin)
        .arg("--settings")
        .arg(&settings_path)
        .output()
        .expect("run command");

    assert_eq!(output.status.code(), Some(64));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("solstone-terminal: "),
        "stderr must start with solstone-terminal:, got: {stderr}"
    );
    assert!(
        stderr.contains(&settings_path.display().to_string()),
        "stderr must name settings path, got: {stderr}"
    );

    // Spool must be unchanged
    assert_eq!(
        fs::read_dir(&spool).unwrap().count(),
        0,
        "spool must be unchanged"
    );
}

#[test]
fn legacy_missing_root_exits_0() {
    let temp = TestDirectory::new("legacy-missing");
    let missing_path = temp.path().join("does-not-exist");

    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("legacy")
        .arg("--from")
        .arg(&missing_path)
        .output()
        .expect("run legacy");

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&missing_path.display().to_string()),
        "{stderr}"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let val: Value = serde_json::from_str(&stdout).expect("parse stdout json");
    assert_eq!(
        val,
        serde_json::json!({"event":"done","segments":0,"held":0,"problems":0})
    );

    assert!(!missing_path.exists(), "missing path must not be created");
}

#[test]
fn legacy_held_lock_exits_75() {
    let temp = TestDirectory::new("legacy-held-lock");
    let spool = temp.path().join("legacy-data");
    fs::create_dir_all(&spool).expect("create spool");

    let lock_path = spool.join(".solstone-tmux.lock");
    let lock = InstanceLock::acquire_path(&lock_path).expect("acquire lock in test");

    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("legacy")
        .arg("--from")
        .arg(&spool)
        .output()
        .expect("run legacy");

    assert_eq!(output.status.code(), Some(75));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&lock_path.display().to_string()),
        "{stderr}"
    );

    drop(lock);

    // Check tree unchanged (only .solstone-tmux.lock exists)
    let files = fs::read_dir(&spool)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert_eq!(files, vec![".solstone-tmux.lock".to_string()]);
}

fn find_incomplete_screen(spool: &Path) -> Option<PathBuf> {
    for day in fs::read_dir(spool).ok()?.filter_map(Result::ok) {
        if !day.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        for entry in fs::read_dir(day.path()).ok()?.filter_map(Result::ok) {
            let screen = entry.path().join("tmux_main_screen.jsonl");
            if entry.file_name().to_string_lossy().ends_with(".incomplete") && screen.is_file() {
                return Some(day.path());
            }
        }
    }
    None
}

#[test]
fn engine_error_mid_run_exits_1_restores_and_unlocks() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let temp = TestDirectory::new("engine-error-mid-run");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");
    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    let initial_status_left = "#[fg=magenta]#S ";
    fake_tmux.init_state(initial_status_left, "40");

    let settings = write_settings(temp.path(), 1, 2);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));
    assert_eq!(
        proc.recv_stdout_json(Duration::from_secs(3))["event"],
        "ready"
    );

    let mut day = None;
    for _ in 0..100 {
        day = find_incomplete_screen(&spool);
        if day.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let day = day.expect("an open segment with a frame");
    // The finalize at the next rotation cannot write into the day directory.
    fs::set_permissions(&day, Permissions::from_mode(0o555)).expect("lock day directory");
    let status = proc.wait_within(Duration::from_secs(10));
    fs::set_permissions(&day, Permissions::from_mode(0o700)).expect("unlock day directory");

    assert_eq!(status.code(), Some(1), "{status:?}");
    let stderr = proc.collect_remaining_stderr();
    assert_eq!(stderr.len(), 1, "{stderr:?}");
    assert!(stderr[0].starts_with("solstone-terminal: "));
    let events = proc.collect_remaining_stdout();
    assert!(
        !events.iter().any(|event| event["event"] == "stopped"),
        "{events:?}"
    );
    let state = fake_tmux.read_state();
    assert_eq!(state["status-left"], initial_status_left);
    assert_eq!(state["status-left-length"], "40");
    assert!(state.get("@solstone").is_none());
    let lock = InstanceLock::acquire_path(&spool.join(".solstone-terminal.lock"));
    assert!(lock.is_ok(), "the lock is released");
}

#[test]
fn missing_settings_file_exits_64() {
    let temp = TestDirectory::new("missing-settings-64");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");
    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    let settings_path = temp.path().join("absent-settings.json");

    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("run")
        .arg("--spool")
        .arg(&spool)
        .arg("--tmux")
        .arg(&fake_tmux.tmux_bin)
        .arg("--settings")
        .arg(&settings_path)
        .output()
        .expect("run command");

    assert_eq!(output.status.code(), Some(64));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&settings_path.display().to_string()),
        "{stderr}"
    );
    assert_eq!(fs::read_dir(&spool).unwrap().count(), 0);
    assert!(!settings_path.exists());
}

#[test]
fn non_utf8_stdin_line_is_ignored_and_capture_continues() {
    let temp = TestDirectory::new("non-utf8-stdin");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");
    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    fake_tmux.init_state("#[fg=white]#S#[default]", "40");

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));
    assert_eq!(
        proc.recv_stdout_json(Duration::from_secs(3))["event"],
        "ready"
    );

    proc.write_stdin_bytes(b"\xff\xfe not utf-8\n");
    proc.write_stdin("{\"cmd\":\"sync\",\"state\":\"working\"}\n");
    let mut syncing = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_millis(50));
        if fake_tmux
            .read_state()
            .get("@solstone")
            .and_then(|v| v.as_str())
            == Some("syncing")
        {
            syncing = true;
            break;
        }
    }
    assert!(syncing, "the helper kept reading stdin after the bad line");

    proc.write_stdin("{\"cmd\":\"stop\"}\n");
    assert!(proc.wait_within(Duration::from_secs(10)).success());
    let stderr = proc.collect_remaining_stderr();
    assert_eq!(stderr.len(), 1, "{stderr:?}");
    assert!(stderr[0].starts_with("solstone-terminal: "));
}

#[test]
fn sigterm_right_after_ready_stops_cleanly() {
    let temp = TestDirectory::new("sigterm-after-ready");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");
    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    let initial_status_left = "#[fg=red]#S ";
    fake_tmux.init_state(initial_status_left, "40");

    let settings = write_settings(temp.path(), 1, 1);
    let mut proc = TestProcess::spawn(&spool, &fake_tmux, Some(&settings));
    assert_eq!(
        proc.recv_stdout_json(Duration::from_secs(3))["event"],
        "ready"
    );
    let pid = rustix::process::Pid::from_raw(proc.child.id() as i32).expect("pid");
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("signal");

    let status = proc.wait_within(Duration::from_secs(10));
    assert!(status.success(), "{status:?}");
    let events = proc.collect_remaining_stdout();
    assert_eq!(
        events.last().map(|event| &event["event"]),
        Some(&Value::from("stopped"))
    );
    let state = fake_tmux.read_state();
    assert_eq!(state["status-left"], initial_status_left);
    assert!(state.get("@solstone").is_none());
}

#[test]
fn diagnostics_go_to_stderr() {
    let temp = TestDirectory::new("diagnostics-stderr");
    let spool = temp.path().join("spool");
    fs::create_dir_all(&spool).expect("create spool");
    let fake_tmux = FakeTmuxEnvironment::new(temp.path());
    fake_tmux.init_state("#[fg=white]#S#[default]", "40");

    let settings = write_settings(temp.path(), 1, 60);
    let mut proc = TestProcess::spawn_with_env(
        &spool,
        &fake_tmux,
        Some(&settings),
        &[("FAKE_TMUX_FAIL_POLLS", "1")],
    );
    assert_eq!(
        proc.recv_stdout_json(Duration::from_secs(3))["event"],
        "ready"
    );
    let first = proc
        .stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a failed poll is reported on stderr");
    assert!(first.starts_with("solstone-terminal: "), "{first}");

    proc.close_stdin();
    assert!(proc.wait_within(Duration::from_secs(10)).success());
}

#[test]
fn legacy_reports_each_path_once() {
    let temp = TestDirectory::new("legacy-each-path-once");
    let data_root = temp.path().join("legacy-data");
    let captures = data_root.join("captures");
    let retained = captures.join("20260701/stream.tmux/110000.incomplete");
    fs::create_dir_all(&retained).expect("retained segment");
    fs::write(
        retained.join("tmux_main_screen.jsonl"),
        b"{\"frame_id\": 1}\n",
    )
    .expect("frame");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("outside");
    let linked_day = captures.join("20260702");
    std::os::unix::fs::symlink(&outside, &linked_day).expect("symlinked day");

    let output = Command::new(env!("CARGO_BIN_EXE_solstone-terminal"))
        .arg("legacy")
        .arg("--from")
        .arg(&data_root)
        .output()
        .expect("run legacy");

    assert!(output.status.success(), "{output:?}");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json line"))
        .collect::<Vec<_>>();
    let held = events
        .iter()
        .filter(|event| event["event"] == "held")
        .collect::<Vec<_>>();
    assert_eq!(held.len(), 1, "{events:?}");
    assert_eq!(held[0]["dir"], retained.display().to_string());
    assert_eq!(held[0]["reason"], "retained");
    let problems = events
        .iter()
        .filter(|event| event["event"] == "problem")
        .collect::<Vec<_>>();
    assert_eq!(problems.len(), 1, "{events:?}");
    assert_eq!(problems[0]["path"], linked_day.display().to_string());
    assert_eq!(
        events.last(),
        Some(&serde_json::json!({"event":"done","segments":0,"held":1,"problems":1}))
    );
}
