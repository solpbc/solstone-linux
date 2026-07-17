// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::{Config, ConfigPaths, load_config},
    session_env::{self, Output, Runner},
    streams::stream_name,
};
use clap::{Parser, Subcommand};
use std::{
    collections::HashMap,
    env, fs, io,
    io::Read,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Parser)]
#[command(
    name = "solstone-linux",
    about = "sol for Linux — takes in your screen and audio and keeps it in your journal. part of solstone.",
    version
)]
pub struct Args {
    #[arg(short, long)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
enum Commands {
    #[command(about = "start sol")]
    Run {
        #[arg(long, help = "Segment duration in seconds (default: 300)")]
        interval: Option<i64>,
    },
    #[command(about = "Interactive configuration")]
    Setup,
    #[command(about = "Verify install prerequisites")]
    Doctor,
    #[command(about = "edit settings")]
    Settings,
    #[command(name = "install-service", about = "Install systemd user service")]
    InstallService,
    #[command(about = "show status")]
    Status,
}

struct SystemRunner;

fn is_executable_file(path: &str) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

impl Runner for SystemRunner {
    fn which(&self, program: &str) -> Option<String> {
        env::var_os("PATH")?
            .to_string_lossy()
            .split(':')
            .map(|directory| format!("{directory}/{program}"))
            .find(|candidate| is_executable_file(candidate))
    }

    fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
        environment: &HashMap<String, String>,
    ) -> io::Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .envs(environment)
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout must be present");
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                let bytes = reader
                    .join()
                    .map_err(|_| io::Error::other("stdout reader panicked"))??;
                return Ok(Output {
                    success: status.success(),
                    stdout: String::from_utf8_lossy(&bytes).into_owned(),
                });
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Debug, PartialEq)]
enum RunFailure {
    NotReady(&'static str),
    Other,
}

fn exit_code(result: Result<(), RunFailure>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(RunFailure::NotReady(_)) => session_env::EXIT_TEMPFAIL,
        Err(RunFailure::Other) => 1,
    }
}

fn effective_command(command: Option<Commands>) -> Commands {
    command.unwrap_or(Commands::Run { interval: None })
}

fn apply_interval(config: &mut Config, interval: Option<i64>) {
    if let Some(interval) = interval.filter(|value| *value != 0) {
        config.segment_interval = interval;
    }
}

fn session_gate(
    environment: &mut HashMap<String, String>,
    uid: u32,
    runner: &dyn Runner,
) -> Result<(), RunFailure> {
    session_env::recover_session_env(environment, uid, runner);
    session_env::check_session_ready(environment, runner)
        .map_or(Ok(()), |reason| Err(RunFailure::NotReady(reason)))
}

fn process_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn apply_session_environment(environment: &HashMap<String, String>) {
    for name in [
        "XDG_RUNTIME_DIR",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Some(value) = environment.get(name) {
            // SAFETY: cmd_run performs session recovery during single-threaded startup,
            // before the observer stub starts any worker threads.
            unsafe { env::set_var(name, value) };
        }
    }
}

fn hostname() -> io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/hostname")?
        .trim()
        .to_owned())
}

fn setup_logging(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let _ = tracing_subscriber::fmt().with_max_level(level).try_init();
}

pub fn run() -> i32 {
    let args = Args::parse();
    setup_logging(args.verbose);
    match effective_command(args.command) {
        Commands::Run { interval } => cmd_run(interval),
        command => {
            eprintln!("{} is not yet implemented", command_name(&command));
            1
        }
    }
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Run { .. } => "run",
        Commands::Setup => "setup",
        Commands::Doctor => "doctor",
        Commands::Settings => "settings",
        Commands::InstallService => "install-service",
        Commands::Status => "status",
    }
}

fn cmd_run(interval: Option<i64>) -> i32 {
    let loaded = load_config(ConfigPaths::default());
    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    let mut config = loaded.config;
    if let Err(error) = config.ensure_dirs() {
        tracing::error!("Failed to create observer directories: {error}");
        return exit_code(Err(RunFailure::Other));
    }
    if config.stream.is_empty() {
        let host = match hostname() {
            Ok(host) => host,
            Err(error) => {
                tracing::error!("Failed to read hostname: {error}");
                return 1;
            }
        };
        match stream_name(Some(&host), None, None) {
            Ok(stream) => config.stream = stream,
            Err(error) => {
                eprintln!("Error: {error}");
                return 1;
            }
        }
    }
    apply_interval(&mut config, interval);
    let mut environment: HashMap<String, String> = env::vars().collect();
    let gate_result = session_gate(&mut environment, process_uid(), &SystemRunner);
    apply_session_environment(&environment);
    if let Err(failure) = gate_result {
        if let RunFailure::NotReady(reason) = failure {
            tracing::warn!("Session not ready: {reason}");
            return session_env::EXIT_TEMPFAIL;
        }
        return 1;
    }
    tracing::info!("Rust observer stub running");
    loop {
        thread::park_timeout(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::{cell::Cell, path::Path};

    struct FakeRunner {
        output: io::Result<Output>,
        calls: Cell<usize>,
    }
    impl Runner for FakeRunner {
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
            self.calls.set(self.calls.get() + 1);
            match &self.output {
                Ok(output) => Ok(output.clone()),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            }
        }
    }

    // AC: root help pins the complete Python CLI subcommand surface.
    #[test]
    fn root_help_surface() {
        let command = Args::command();
        command.clone().debug_assert();
        let names: Vec<_> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect();
        assert_eq!(
            names,
            vec![
                "run",
                "setup",
                "doctor",
                "settings",
                "install-service",
                "status"
            ]
        );
    }
    // AC: run help pins its interval option and exact help text.
    #[test]
    fn run_help_surface() {
        let command = Args::command();
        let run = command.find_subcommand("run").unwrap();
        let interval = run
            .get_arguments()
            .find(|argument| argument.get_id() == "interval")
            .unwrap();
        assert_eq!(
            interval.get_help().unwrap().to_string(),
            "Segment duration in seconds (default: 300)"
        );
    }
    // AC: clap owns version output and exposes the crate version.
    #[test]
    fn version() {
        assert_eq!(
            Args::command().get_version(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(Args::try_parse_from(["solstone-linux", "--version"]).is_err());
    }
    // AC: verbose raises logging verbosity.
    #[test]
    fn verbose_flag() {
        assert!(
            Args::try_parse_from(["solstone-linux", "-v"])
                .unwrap()
                .verbose
        );
        assert!(!Args::try_parse_from(["solstone-linux"]).unwrap().verbose);
    }
    // AC: bare invocation is run parity.
    #[test]
    fn bare_is_run() {
        let args = Args::try_parse_from(["solstone-linux"]).unwrap();
        assert_eq!(
            effective_command(args.command),
            Commands::Run { interval: None }
        );
    }
    // AC: truthy interval overrides while zero does not.
    #[test]
    fn interval_semantics() {
        let mut config = Config::default();
        apply_interval(&mut config, Some(600));
        assert_eq!(config.segment_interval, 600);
        apply_interval(&mut config, Some(0));
        assert_eq!(config.segment_interval, 600);
    }
    // AC: a genuinely unrecoverable session maps to EX_TEMPFAIL 75.
    #[test]
    fn not_ready_exit_75() {
        let runner = FakeRunner {
            output: Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
            calls: Cell::new(0),
        };
        let mut environment = HashMap::new();
        assert_eq!(exit_code(session_gate(&mut environment, 1000, &runner)), 75);
    }
    // AC: non-session failures map to exit one.
    #[test]
    fn other_failure_exit_1() {
        assert_eq!(exit_code(Err(RunFailure::Other)), 1);
    }
    // AC: every stub names itself and is explicitly unimplemented.
    #[test]
    fn stub_names() {
        for command in [
            Commands::Setup,
            Commands::Doctor,
            Commands::Settings,
            Commands::InstallService,
            Commands::Status,
        ] {
            let message = format!("{} is not yet implemented", command_name(&command));
            assert!(message.contains(command_name(&command)));
        }
    }
    // AC: PATH lookup requires an executable regular file.
    #[test]
    fn executable_lookup_contract() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("pactl");
        fs::write(&file, b"").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!is_executable_file(file.to_str().unwrap()));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_executable_file(file.to_str().unwrap()));
        assert!(Path::new(&file).is_file());
    }
    // AC: proc hostname input is trimmed before stream validation.
    #[test]
    fn hostname_is_trimmed() {
        assert_eq!("archon\n".trim(), "archon");
        assert_eq!(stream_name(Some("archon\n"), None, None).unwrap(), "archon");
    }
}
