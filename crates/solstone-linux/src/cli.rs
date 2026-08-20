// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    capture_stats::{
        compute_quarantine_stats, compute_status_capture_stats, format_quarantine_line,
    },
    config::{Config, ConfigPaths, load_config, sanitize_link_authority, save_config},
    private_link::{
        PrivateStateError, PrivateStateLock, PrivateStateLockLiveness, setup_with_stream,
    },
    session_env::{self, Output, Runner},
    streams::stream_name,
    sync_health::{ProcessEpoch, SyncFacts, derive_health, load_facts_with_liveness, save_facts},
};
use clap::{Parser, Subcommand};
use std::{
    collections::HashMap,
    env, fs, io,
    io::{BufRead, Read, Write},
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    #[command(about = "Pair sol with your journal from standard input")]
    Setup {
        #[arg(long, help = "Stream name (defaults to hostname-derived)")]
        stream_name: Option<String>,
    },
    #[command(about = "Verify install prerequisites")]
    Doctor,
    #[command(about = "edit settings")]
    Settings,
    #[command(name = "install-service", about = "Install systemd user service")]
    InstallService,
    #[command(name = "uninstall-service", about = "Uninstall systemd user service")]
    UninstallService,
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
        // Only `success` and stdout reach the caller, so an inherited stderr cannot inform
        // any decision — it can only interleave a probe's complaint into our own output.
        // `gnome-extensions list` on a non-GNOME desktop is the standard case: it prints
        // "Failed to connect to GNOME Shell" in the middle of the doctor report while the
        // check itself correctly resolves to "not applicable".
        let mut child = Command::new(program)
            .args(args)
            .envs(environment)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
            set_session_environment_variable(name, value);
        }
    }
}

#[allow(unsafe_code)]
fn set_session_environment_variable(name: &str, value: &str) {
    // SAFETY: cmd_run performs session recovery during single-threaded startup,
    // before the observer starts any worker threads.
    unsafe { ::std::env::set_var(name, value) };
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
        Commands::Setup { stream_name } => cmd_setup(
            SetupOptions { stream_name },
            ConfigPaths::default(),
            &mut io::stdin().lock(),
            &mut io::stdout(),
            &mut io::stderr(),
        ),
        Commands::Settings => cmd_settings(ConfigPaths::default(), &mut ConsolePrompt),
        Commands::Status => cmd_status(ConfigPaths::default(), &SystemRunner, &mut io::stdout()),
        Commands::Doctor => crate::doctor::run_doctor(
            &mut crate::doctor::RealDoctor::new(&SystemRunner),
            &mut io::stdout(),
        ),
        Commands::InstallService => match crate::service::ServicePaths::production() {
            Ok(paths) => crate::service::install(&paths, &SystemRunner, &mut io::stdout()),
            Err(error) => {
                eprintln!("Error: {error}");
                1
            }
        },
        Commands::UninstallService => match crate::service::ServicePaths::production() {
            Ok(paths) => crate::service::uninstall(&paths, &SystemRunner, &mut io::stdout()),
            Err(error) => {
                eprintln!("Error: {error}");
                1
            }
        },
    }
}

struct SetupOptions {
    stream_name: Option<String>,
}

fn write_line(output: &mut dyn Write, value: impl std::fmt::Display) -> io::Result<()> {
    writeln!(output, "{value}")
}

fn cmd_setup(
    options: SetupOptions,
    paths: ConfigPaths,
    input: &mut dyn Read,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let host = hostname().unwrap_or_else(|_| "linux".into());
    let stream = options
        .stream_name
        .filter(|value| !value.is_empty())
        .or_else(|| stream_name(Some(&host), None, None).ok());
    let config_root = paths
        .config_dir
        .unwrap_or_else(|| Config::default().config_dir);
    if write_line(
        output,
        "Paste the pair link from your journal, then press Enter:",
    )
    .is_err()
    {
        return 1;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = write_line(errors, format!("Setup failed: {error}"));
            return 1;
        }
    };
    render_setup_result(
        runtime.block_on(setup_with_stream(
            &config_root,
            &host,
            stream.as_deref(),
            input,
        )),
        output,
        errors,
    )
}

fn render_setup_result(
    result: Result<(), PrivateStateError>,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    match result {
        Ok(()) => {
            let _ = write_line(output, "sol can now connect to your journal.");
            0
        }
        Err(PrivateStateError::PairInputInvalid) => {
            let _ = write_line(errors, "Setup failed: the pair link was not valid.");
            1
        }
        Err(PrivateStateError::PairingFailed) => {
            let _ = write_line(
                errors,
                "Setup failed: sol could not connect to your journal.",
            );
            1
        }
        Err(PrivateStateError::LockContended) => {
            let _ = write_line(
                errors,
                "Setup could not start because sol is running. Stop sol first and try again. No input was consumed; capture, config, and private state are unchanged.",
            );
            1
        }
        Err(error @ (PrivateStateError::Io { .. } | PrivateStateError::InvalidTarget { .. })) => {
            let _ = write_line(
                errors,
                "Setup failed before pairing because sol could not safely update its config.",
            );
            let _ = write_line(errors, format!("Config update error: {error}"));
            1
        }
        Err(error) => {
            let _ = write_line(errors, format!("Setup failed: {error}"));
            1
        }
    }
}

#[cfg(test)]
pub(crate) async fn dispatch_setup_with_pairer_for_test<R: Read>(
    pairer: &dyn crate::private_link::Pairer,
    config_root: &std::path::Path,
    stream: &str,
    input: R,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    if write_line(
        output,
        "Paste the pair link from your journal, then press Enter:",
    )
    .is_err()
    {
        return 1;
    }
    let result = crate::private_link::setup_with_pairer_for_test(
        pairer,
        config_root,
        "linux",
        Some(stream),
        input,
    )
    .await;
    render_setup_result(result, output, errors)
}

trait PromptIo {
    fn read_line(&mut self, prompt: &str) -> io::Result<String>;
    fn write_line(&mut self, line: &str) -> io::Result<()>;
}

struct ConsolePrompt;
impl PromptIo for ConsolePrompt {
    fn read_line(&mut self, prompt: &str) -> io::Result<String> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut value = String::new();
        io::stdin().lock().read_line(&mut value)?;
        Ok(value)
    }
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        println!("{line}");
        Ok(())
    }
}

fn prompt_bool(io: &mut dyn PromptIo, label: &str, current: bool) -> io::Result<bool> {
    loop {
        let value = io.read_line(&format!("{label} [{}]: ", if current { "y" } else { "n" }))?;
        match value.trim().to_lowercase().as_str() {
            "" => return Ok(current),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => io.write_line("Enter y or n.")?,
        }
    }
}

fn prompt_positive_int(io: &mut dyn PromptIo, label: &str, current: i64) -> io::Result<i64> {
    loop {
        let value = io.read_line(&format!("{label} [{current}]: "))?;
        if value.trim().is_empty() {
            return Ok(current);
        }
        if let Ok(value) = value.trim().parse::<i64>()
            && value > 0
        {
            return Ok(value);
        }
        io.write_line("Enter a positive integer.")?;
    }
}

fn prompt_framerate(io: &mut dyn PromptIo, current: i64) -> io::Result<i64> {
    loop {
        let value = io.read_line(&format!("Framerate [{current}]: "))?;
        if value.trim().is_empty() {
            return Ok(current);
        }
        let Ok(value) = value.trim().parse::<i64>() else {
            io.write_line("Enter an integer.")?;
            continue;
        };
        let clamped = value.clamp(1, 10);
        if clamped != value {
            io.write_line(&format!("(clamped to {clamped})"))?;
        }
        return Ok(clamped);
    }
}

fn prompt_retention(io: &mut dyn PromptIo, current: i64) -> io::Result<i64> {
    loop {
        let value = io.read_line(&format!("Cache retention days (-1 = keep forever, 0 = delete synced segments after the day ends, N = keep N days) [{current}]: "))?;
        if value.trim().is_empty() {
            return Ok(current);
        }
        match value.trim().parse() {
            Ok(value) => return Ok(value),
            Err(_) => io.write_line("Enter an integer.")?,
        }
    }
}

fn cmd_settings(paths: ConfigPaths, prompt: &mut dyn PromptIo) -> i32 {
    let loaded = load_config(paths);
    let mut config = loaded.config;
    let result = (|| -> io::Result<()> {
        config.capture_framerate = prompt_framerate(prompt, config.capture_framerate)?;
        config.draw_cursor = prompt_bool(prompt, "Draw cursor", config.draw_cursor)?;
        config.start_paused = prompt_bool(prompt, "Start paused", config.start_paused)?;
        config.segment_interval =
            prompt_positive_int(prompt, "Segment interval seconds", config.segment_interval)?;
        config.cache_retention_days = prompt_retention(prompt, config.cache_retention_days)?;
        save_config(&config)?;
        prompt.write_line(&format!(
            "\nSettings saved to {}",
            config.config_path().display()
        ))?;
        // Config is read once at startup and never re-read, so a running sol keeps the
        // old values. Saying only "saved" invites the owner to believe otherwise.
        prompt.write_line("These take effect the next time sol starts.")?;
        prompt.write_line("  systemctl --user restart solstone-linux")
    })();
    if let Err(error) = result {
        eprintln!("Error editing settings: {error}");
        1
    } else {
        0
    }
}

fn cmd_status(paths: ConfigPaths, runner: &dyn Runner, output: &mut dyn Write) -> i32 {
    let loaded = load_config(paths);
    let config = loaded.config;
    let stream = if config.stream.is_empty() {
        "(not set)"
    } else {
        &config.stream
    };
    // Resolved before the first line is written: "managed privately" describes a link that
    // exists. Printing it while sol is telling the owner to pair contradicts the sync line
    // two lines below it.
    let link_line = if crate::private_link::credential_present(&config.config_dir) {
        "Journal link: managed privately"
    } else {
        "Journal link: not paired"
    };
    let mut render = || -> io::Result<()> {
        write_line(
            output,
            format!("Config: {}", config.config_path().display()),
        )?;
        write_line(output, link_line)?;
        write_line(output, format!("Stream: {stream}"))?;
        write_line(output, "")?;
        let captures = config.captures_dir();
        if captures.exists() {
            let stats = compute_status_capture_stats(&captures);
            write_line(output, format!("Cache:  {}", captures.display()))?;
            write_line(
                output,
                format!(
                    "        {} segments across {} day(s), {:.1} MB",
                    stats.segment_count, stats.day_count, stats.size_mb
                ),
            )?;
            if stats.incomplete_count != 0 {
                write_line(
                    output,
                    format!("        {} incomplete segment(s)", stats.incomplete_count),
                )?;
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            if let Some(line) = format_quarantine_line(&compute_quarantine_stats(&captures, now)) {
                write_line(output, format!("        {line}"))?;
            }
        } else {
            write_line(
                output,
                format!("Cache:  {} (not created yet)", captures.display()),
            )?;
        }
        match config.cache_retention_days {
            value if value < 0 => write_line(output, "Retain: forever")?,
            0 => write_line(output, "Retain: delete synced segments after the day ends")?,
            value => write_line(output, format!("Retain: {value} day(s)"))?,
        }
        let liveness = PrivateStateLock::try_probe(&config.config_dir)
            .unwrap_or(PrivateStateLockLiveness::NoLiveOwner);
        let facts = load_facts_with_liveness(&config.state_dir(), liveness);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        write_line(
            output,
            derive_health(&facts, now, config.sync_stale_threshold as f64).cli,
        )?;
        if let Some(systemctl) = runner.which("systemctl")
            && let Ok(result) = runner.run(
                &systemctl,
                &["--user", "is-active", "solstone-linux.service"],
                Duration::from_secs(5),
                &HashMap::new(),
            )
        {
            write_line(output, format!("\nService: {}", result.stdout.trim()))?;
        }
        Ok(())
    };
    if render().is_ok() { 0 } else { 1 }
}

fn cmd_run(interval: Option<i64>) -> i32 {
    let paths = ConfigPaths::default();
    let (state_lock, mut config, transport_enabled, process_epoch) = match prepare_run_config(paths)
    {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::error!(%error, "{}", run_preparation_error_guidance(&error));
            return 1;
        }
    };
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
    crate::run::run_observer(
        config,
        hostname().unwrap_or_else(|_| "linux".into()),
        state_lock,
        transport_enabled,
        process_epoch,
    )
}

fn run_preparation_error_guidance(error: &PrivateStateError) -> &'static str {
    match error {
        PrivateStateError::LockContended => "Linked private state is already in use",
        PrivateStateError::HealthInitializationFailed => {
            "Startup could not continue because sol could not clear the sync status from the previous run. Make sure sol can write its local data, then try again."
        }
        _ => {
            "Startup could not continue because sol could not safely prepare its local data. Make sure sol can write its local data, then try again."
        }
    }
}

pub(crate) fn prepare_run_config(
    paths: ConfigPaths,
) -> Result<
    (PrivateStateLock, Config, bool, Option<ProcessEpoch>),
    crate::private_link::PrivateStateError,
> {
    let config_root = paths
        .config_dir
        .clone()
        .unwrap_or_else(|| Config::default().config_dir);
    let mut state_lock = PrivateStateLock::acquire(&config_root)?;
    let loaded = load_config(paths.clone());
    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    let mut config = loaded.config;
    let transport_enabled = match sanitize_link_authority(&paths) {
        Ok(sanitized) => {
            config = sanitized;
            true
        }
        Err(error) => {
            tracing::error!(%error, "Could not safely update linked config; capture will continue");
            false
        }
    };
    let process_epoch = match ProcessEpoch::generate() {
        Ok(epoch) => Some(epoch),
        Err(error) => {
            tracing::error!(%error, "Failed to create process epoch; linked work disabled");
            None
        }
    };
    let reset = SyncFacts {
        link: Some(Default::default()),
        link_epoch: process_epoch.clone(),
        ..Default::default()
    };
    save_facts(&config.state_dir(), &reset)
        .map_err(|_| PrivateStateError::HealthInitializationFailed)?;
    state_lock.mark_ready()?;
    Ok((
        state_lock,
        config,
        transport_enabled && process_epoch.is_some(),
        process_epoch,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::key_prefix;
    use clap::CommandFactory;
    use std::{
        cell::Cell,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

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
                "uninstall-service",
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
    // tests/test_cli.py::test_main_version_flag
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
    // AC: the safe wrapper assigns the exact value and leaves the process environment as found.
    #[test]
    #[allow(unsafe_code)]
    fn session_environment_wrapper_assigns_and_restores() {
        const NAME: &str = "SOLSTONE_LINUX_TEST_SAFE_ENVIRONMENT_WRAPPER";
        const VALUE: &str = "known-wrapper-value";
        // Compile-time proof: this coercion fails if the wrapper becomes an unsafe function.
        let wrapper: fn(&str, &str) = set_session_environment_variable;
        let previous = env::var_os(NAME);

        wrapper(NAME, VALUE);
        assert_eq!(env::var(NAME).as_deref(), Ok(VALUE));

        match previous {
            Some(value) => wrapper(NAME, &value.to_string_lossy()),
            // SAFETY: this test restores its uniquely named variable after the assertion,
            // and no other test or runtime path reads or writes that variable.
            None => unsafe { ::std::env::remove_var(NAME) },
        }
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

    fn paths(t: &tempfile::TempDir) -> ConfigPaths {
        ConfigPaths {
            base_dir: Some(t.path().join("data")),
            config_dir: Some(t.path().join("config")),
        }
    }

    #[test]
    fn setup_help_exposes_only_stream_name() {
        let command = Args::command();
        let setup = command.find_subcommand("setup").unwrap();
        let arguments = setup
            .get_arguments()
            .map(|argument| argument.get_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(arguments, vec!["stream_name"]);
        for removed in ["--server-url", "--token", "--non-interactive"] {
            assert!(Args::try_parse_from(["solstone-linux", "setup", removed]).is_err());
        }
    }

    #[test]
    fn setup_ignores_no_legacy_token_environment() {
        assert!(
            !include_str!("cli.rs").contains("env::var(\"SOLSTONE_TOKEN\")"),
            "setup must not read SOLSTONE_TOKEN"
        );
    }

    struct CountingInput {
        bytes: std::io::Cursor<Vec<u8>>,
        reads: Arc<AtomicUsize>,
    }

    impl Read for CountingInput {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.bytes.read(buffer)
        }
    }

    #[test]
    fn setup_consumes_exactly_one_bounded_stdin_link() {
        let temp = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut input = std::io::Cursor::new(vec![b'a'; 4097]);
        assert_eq!(
            cmd_setup(
                SetupOptions {
                    stream_name: Some("host-a".into())
                },
                paths(&temp),
                &mut input,
                &mut output,
                &mut errors,
            ),
            1
        );
        assert_eq!(
            String::from_utf8(errors).unwrap(),
            "Setup failed: the pair link was not valid.\n"
        );
        assert!(input.position() <= 4097);
    }

    #[test]
    fn setup_lock_loser_does_not_consume_input_or_mutate_state() {
        let temp = tempfile::tempdir().unwrap();
        let config_root = temp.path().join("config");
        let lock = crate::private_link::PrivateStateLock::acquire(&config_root).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let mut input = CountingInput {
            bytes: std::io::Cursor::new(b"pair-secret".to_vec()),
            reads: reads.clone(),
        };
        let before = std::fs::read_dir(&config_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        assert_eq!(
            cmd_setup(
                SetupOptions {
                    stream_name: Some("host-a".into())
                },
                ConfigPaths {
                    base_dir: None,
                    config_dir: Some(config_root.clone())
                },
                &mut input,
                &mut Vec::new(),
                &mut errors,
            ),
            1
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(
            String::from_utf8(errors).unwrap(),
            "Setup could not start because sol is running. Stop sol first and try again. No input was consumed; capture, config, and private state are unchanged.\n"
        );
        let after = std::fs::read_dir(&config_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(after, before);
        drop(lock);
    }

    #[test]
    fn prepare_run_config_resets_prior_connected_facts_before_returning() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(&temp);
        let config = load_config(paths.clone()).config;
        let prior = SyncFacts {
            pending_confirmed: Some(0),
            link: Some(crate::private_link::LinkFactState {
                listener_ready: true,
                carrier_proven: true,
                observer_registered: true,
                ..Default::default()
            }),
            link_epoch: Some(ProcessEpoch::for_test(9)),
            ..Default::default()
        };
        save_facts(&config.state_dir(), &prior).unwrap();

        let (_lock, config, _, process_epoch) = prepare_run_config(paths).unwrap();
        let liveness = PrivateStateLock::try_probe(&config.config_dir).unwrap();
        assert_eq!(liveness, PrivateStateLockLiveness::LiveOwner);
        let current = load_facts_with_liveness(&config.state_dir(), liveness);
        assert_eq!(current.link_epoch, process_epoch);
        let link = current.link.unwrap();
        assert!(!link.pairing_required);
        assert!(!link.private_state_invalid);
        assert!(!link.config_sanitation_failed);
        assert!(!link.listener_ready);
        assert!(!link.carrier_proven);
        assert!(!link.observer_registered);
        assert!(!link.transport_unavailable);
        assert!(!link.terminal_revocation);
        assert!(!link.token_persistence_failure);
    }

    #[test]
    fn live_unready_owner_does_not_expose_prior_connected_facts() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(&temp);
        let config = load_config(paths).config;
        save_facts(
            &config.state_dir(),
            &SyncFacts {
                pending_confirmed: Some(0),
                link: Some(crate::private_link::LinkFactState {
                    listener_ready: true,
                    carrier_proven: true,
                    observer_registered: true,
                    ..Default::default()
                }),
                link_epoch: Some(ProcessEpoch::for_test(8)),
                ..Default::default()
            },
        )
        .unwrap();
        let _lock = PrivateStateLock::acquire(&config.config_dir).unwrap();
        let liveness = PrivateStateLock::try_probe(&config.config_dir).unwrap();
        assert_eq!(liveness, PrivateStateLockLiveness::LiveOwnerNotReady);
        let facts = load_facts_with_liveness(&config.state_dir(), liveness);
        assert!(facts.link.is_none());
        assert!(!matches!(
            derive_health(&facts, 1_000.0, 600.0).state,
            crate::sync_health::HealthState::ListenerReady
                | crate::sync_health::HealthState::Syncing
                | crate::sync_health::HealthState::Connected
        ));
    }

    #[test]
    fn run_preparation_errors_have_distinct_owner_guidance() {
        let contention = run_preparation_error_guidance(&PrivateStateError::LockContended);
        let initialization =
            run_preparation_error_guidance(&PrivateStateError::HealthInitializationFailed);
        let generic = run_preparation_error_guidance(&PrivateStateError::BridgeUnavailable);
        assert_eq!(contention, "Linked private state is already in use");
        assert_eq!(
            initialization,
            "Startup could not continue because sol could not clear the sync status from the previous run. Make sure sol can write its local data, then try again."
        );
        assert_eq!(
            generic,
            "Startup could not continue because sol could not safely prepare its local data. Make sure sol can write its local data, then try again."
        );
        assert_ne!(contention, initialization);
        assert_ne!(generic, initialization);
    }

    #[test]
    fn setup_surfaces_never_disclose_pair_material() {
        let temp = tempfile::tempdir().unwrap();
        let secret = "pair-material-must-not-appear ";
        let mut input = std::io::Cursor::new(secret.as_bytes());
        let mut output = Vec::new();
        let mut errors = Vec::new();
        assert_eq!(
            cmd_setup(
                SetupOptions {
                    stream_name: Some("host-a".into())
                },
                paths(&temp),
                &mut input,
                &mut output,
                &mut errors,
            ),
            1
        );
        let surfaces = format!(
            "{}{}",
            String::from_utf8(output).unwrap(),
            String::from_utf8(errors).unwrap()
        );
        assert!(!surfaces.contains(secret.trim()));
    }

    #[test]
    fn setup_sanitation_failure_precedes_stdin() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("config");
        std::fs::write(&blocker, "not a directory").unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let mut input = CountingInput {
            bytes: std::io::Cursor::new(b"pair-secret".to_vec()),
            reads: reads.clone(),
        };
        let mut errors = Vec::new();
        assert_eq!(
            cmd_setup(
                SetupOptions {
                    stream_name: Some("host-a".into())
                },
                ConfigPaths {
                    base_dir: None,
                    config_dir: Some(blocker)
                },
                &mut input,
                &mut Vec::new(),
                &mut errors,
            ),
            1
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        let errors = String::from_utf8(errors).unwrap();
        assert!(errors.contains(
            "Setup failed before pairing because sol could not safely update its config."
        ));
        assert!(errors.contains("Config update error:"));
        assert!(!errors.contains("pair-secret"));
    }

    #[test]
    fn setup_source_policy_uses_private_link_and_excludes_legacy_registration() {
        let source = include_str!("cli.rs");
        assert!(source.contains("setup_with_stream("));
        assert!(!source.contains(&["UploadClient", "::new("].concat()));
        assert!(!source.contains(&["/app/devices", "/register"].concat()));
        assert!(!source.contains(&[".bearer_", "auth("].concat()));
    }

    struct ScriptedPrompt {
        inputs: std::collections::VecDeque<io::Result<String>>,
        output: String,
    }
    impl ScriptedPrompt {
        fn new(values: &[&str]) -> Self {
            Self {
                inputs: values.iter().map(|value| Ok((*value).into())).collect(),
                output: String::new(),
            }
        }
    }
    impl PromptIo for ScriptedPrompt {
        fn read_line(&mut self, prompt: &str) -> io::Result<String> {
            self.output.push_str(prompt);
            self.inputs.pop_front().expect("scripted input exhausted")
        }
        fn write_line(&mut self, line: &str) -> io::Result<()> {
            self.output.push_str(line);
            self.output.push('\n');
            Ok(())
        }
    }
    fn settings_config(t: &tempfile::TempDir) {
        let mut config = load_config(paths(t)).config;
        config.stream = "strm".into();
        config.capture_framerate = 2;
        save_config(&config).unwrap();
    }
    fn run_settings(t: &tempfile::TempDir, inputs: &[&str]) -> (Config, String) {
        settings_config(t);
        let mut prompt = ScriptedPrompt::new(inputs);
        assert_eq!(cmd_settings(paths(t), &mut prompt), 0);
        (load_config(paths(t)).config, prompt.output)
    }

    // tests/test_cli.py::test_cmd_settings_enter_keeps_all
    #[test]
    fn settings_enter_keeps_all() {
        let t = tempfile::tempdir().unwrap();
        let (config, _) = run_settings(&t, &["", "", "", "", ""]);
        assert_eq!(
            (
                config.capture_framerate,
                config.draw_cursor,
                config.start_paused,
                config.segment_interval,
                config.cache_retention_days
            ),
            (2, true, false, 300, 7)
        );
        assert_eq!(config.stream, "strm");
    }

    // tests/test_cli.py::test_cmd_settings_changes_framerate
    #[test]
    fn settings_changes_framerate() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            run_settings(&t, &["5", "", "", "", ""]).0.capture_framerate,
            5
        );
    }

    // tests/test_cli.py::test_cmd_settings_framerate_clamped
    #[test]
    fn settings_framerate_clamped() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["99", "", "", "", ""]);
        assert_eq!(config.capture_framerate, 10);
        assert!(output.contains("(clamped to 10)"));
    }

    // tests/test_cli.py::test_cmd_settings_framerate_reprompts_on_invalid
    #[test]
    fn settings_framerate_reprompts() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["abc", "3", "", "", "", ""]);
        assert_eq!(config.capture_framerate, 3);
        assert!(output.contains("Enter an integer."));
    }

    // tests/test_cli.py::test_cmd_settings_toggles_bool
    #[test]
    fn settings_toggles_bool() {
        let t = tempfile::tempdir().unwrap();
        assert!(!run_settings(&t, &["", "n", "", "", ""]).0.draw_cursor);
    }

    // tests/test_cli.py::test_cmd_settings_retention_semantics
    #[test]
    fn settings_retention_accepts_negative() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            run_settings(&t, &["", "", "", "", "-1"])
                .0
                .cache_retention_days,
            -1
        );
    }

    // AC: prompt failure leaves the persisted settings unchanged.
    #[test]
    fn settings_prompt_failure_does_not_save() {
        let t = tempfile::tempdir().unwrap();
        settings_config(&t);
        let mut prompt = ScriptedPrompt {
            inputs: [Ok("5".into()), Err(io::Error::other("boom"))].into(),
            output: String::new(),
        };
        assert_eq!(cmd_settings(paths(&t), &mut prompt), 1);
        assert_eq!(load_config(paths(&t)).config.capture_framerate, 2);
    }

    struct StatusRunner(Option<&'static str>);
    impl Runner for StatusRunner {
        fn which(&self, _: &str) -> Option<String> {
            self.0.map(|_| "/usr/bin/systemctl".into())
        }
        fn run(
            &self,
            _: &str,
            _: &[&str],
            _: Duration,
            _: &HashMap<String, String>,
        ) -> io::Result<Output> {
            Ok(Output {
                success: true,
                stdout: self.0.unwrap_or_default().into(),
            })
        }
    }

    fn status_config(t: &tempfile::TempDir) -> Config {
        let mut config = load_config(paths(t)).config;
        config.stream = "test-stream".into();
        save_config(&config).unwrap();
        // These fixtures model a configured observer, which is a paired one. The link line
        // is presence-only, so the bytes never have to be a real credential.
        fs::write(config.config_dir.join("credentials.json"), "{}").unwrap();
        config
    }

    // tests/test_cli.py::test_cmd_status_prints_sync_health
    #[test]
    fn status_prints_sync_health_and_exact_layout() {
        use crate::sync_health::{ErrorType, SyncFacts, save_facts};
        let t = tempfile::tempdir().unwrap();
        let config = status_config(&t);
        save_facts(
            &config.state_dir(),
            &SyncFacts {
                last_error_class: Some(ErrorType::Transient),
                ..Default::default()
            },
        )
        .unwrap();
        let mut out = Vec::new();
        assert_eq!(
            cmd_status(paths(&t), &StatusRunner(Some("active\n")), &mut out),
            0
        );
        let expected = format!(
            "Config: {}\nJournal link: managed privately\nStream: test-stream\n\nCache:  {}\n        0 segments across 0 day(s), 0.0 MB\nRetain: 7 day(s)\nSync: offline; held on this device; will retry\n\nService: active\n",
            config.config_path().display(),
            config.captures_dir().display()
        );
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    // tests/test_cli.py::test_cmd_status_prints_quarantine_line
    #[test]
    fn status_prints_quarantine_line() {
        let t = tempfile::tempdir().unwrap();
        let config = status_config(&t);
        fs::create_dir_all(
            config
                .captures_dir()
                .join("20260101/test-stream/120000_300.failed"),
        )
        .unwrap();
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("        Held: 1 segment(s) not sent, oldest 0d"));
        assert!(!out.contains("Service:"));
    }

    // tests/test_cli.py::test_cmd_status_handles_corrupt_config
    #[test]
    fn status_handles_corrupt_config() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("config")).unwrap();
        fs::write(t.path().join("config/config.json"), "[]").unwrap();
        fs::write(t.path().join("config/credentials.json"), "{}").unwrap();
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Journal link: managed privately")
        );
    }

    // AC: the link line reports the link that exists, so it cannot contradict a sync line
    // telling the owner to pair. This is the upgrade shape — config present, never paired.
    #[test]
    fn status_reports_an_absent_link_as_not_paired() {
        let t = tempfile::tempdir().unwrap();
        let mut config = load_config(paths(&t)).config;
        config.stream = "test-stream".into();
        save_config(&config).unwrap();
        assert!(!config.config_dir.join("credentials.json").exists());
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Journal link: not paired"));
        assert!(!out.contains("managed privately"));
    }

    #[test]
    fn status_never_surfaces_discarded_legacy_values() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("config")).unwrap();
        fs::write(
            t.path().join("config/config.json"),
            r#"{
                "server_url":{"secret":"STATUS-URL-SENTINEL"},
                "key":["STATUS-KEY-SENTINEL"],
                "chat_bridge_enabled":{"secret":"STATUS-CHAT-SENTINEL"},
                "stream":"desktop"
            }"#,
        )
        .and_then(|()| fs::write(t.path().join("config/credentials.json"), "{}"))
        .unwrap();
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Journal link: managed privately"));
        for sentinel in [
            "STATUS-URL-SENTINEL",
            "STATUS-KEY-SENTINEL",
            "STATUS-CHAT-SENTINEL",
        ] {
            assert!(!out.contains(sentinel));
        }
    }

    // AC: key truncation counts characters rather than UTF-8 bytes.
    #[test]
    fn status_key_prefix_is_character_based() {
        assert_eq!(key_prefix("ééééééééé"), "éééééééé");
    }
}
