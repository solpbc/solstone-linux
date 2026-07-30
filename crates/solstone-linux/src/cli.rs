// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    capture_stats::{
        compute_quarantine_stats, compute_status_capture_stats, format_quarantine_line,
    },
    config::{
        Config, ConfigPaths, DEFAULT_SERVER_URL, load_config, save_config,
        save_config_with_identity,
    },
    session_env::{self, Output, Runner},
    streams::stream_name,
    sync_health::{derive_health, load_facts},
    upload::{UploadClient, key_prefix},
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
    #[command(about = "Interactive configuration")]
    Setup {
        #[arg(long, help = "Journal URL (skips prompt)")]
        server_url: Option<String>,
        #[arg(long, help = "Pre-issued registration key; skips journal registration")]
        token: Option<String>,
        #[arg(long, help = "Stream name (defaults to hostname-derived)")]
        stream_name: Option<String>,
        #[arg(long, help = "Fail instead of prompting for missing values")]
        non_interactive: bool,
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
        Commands::Setup {
            server_url,
            token,
            stream_name,
            non_interactive,
        } => cmd_setup(
            SetupOptions {
                server_url,
                token,
                stream_name,
                non_interactive,
            },
            ConfigPaths::default(),
            env::var("SOLSTONE_TOKEN").ok(),
            &mut RealRegistrar,
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
    server_url: Option<String>,
    token: Option<String>,
    stream_name: Option<String>,
    non_interactive: bool,
}

trait Registrar {
    fn register(&mut self, config: &mut Config, host: &str) -> Result<bool, String>;
}

struct RealRegistrar;
impl Registrar for RealRegistrar {
    fn register(&mut self, config: &mut Config, host: &str) -> Result<bool, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let _guard = runtime.enter();
        let client = UploadClient::new(
            config,
            host,
            "linux",
            env!("CARGO_PKG_VERSION"),
            std::sync::Arc::new(crate::run::SystemClock::new()),
        );
        Ok(runtime.block_on(client.ensure_registered(config)))
    }
}

fn write_line(output: &mut dyn Write, value: impl std::fmt::Display) -> io::Result<()> {
    writeln!(output, "{value}")
}

fn cmd_setup(
    options: SetupOptions,
    paths: ConfigPaths,
    env_token: Option<String>,
    registrar: &mut dyn Registrar,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    // Setup deliberately takes no PromptIo; only settings can reach stdin by construction.
    let cli_token = options.token.filter(|value| !value.is_empty());
    let env_token = env_token.filter(|value| !value.is_empty());
    let token = cli_token.clone().or(env_token);
    if cli_token.is_some()
        && write_line(
            errors,
            "warning: --token on the command line may be visible in shell history and /proc on shared computers",
        )
        .is_err()
    {
        return 1;
    }
    let loaded = load_config(paths);
    let mut config = loaded.config;
    config.server_url = options
        .server_url
        .filter(|value| !value.is_empty())
        .or_else(|| (!config.server_url.is_empty()).then(|| config.server_url.clone()))
        .unwrap_or_else(|| DEFAULT_SERVER_URL.into());
    if let Some(stream) = options.stream_name.filter(|value| !value.is_empty()) {
        config.stream = stream;
    } else if config.stream.is_empty() {
        let host = match hostname() {
            Ok(host) => host,
            Err(error) => {
                let _ = write_line(errors, format!("Error deriving stream name: {error}"));
                return 1;
            }
        };
        match stream_name(Some(&host), None, None) {
            Ok(stream) => config.stream = stream,
            Err(error) => {
                let _ = write_line(errors, format!("Error deriving stream name: {error}"));
                return 1;
            }
        }
    }
    if let Err(error) = config.ensure_dirs() {
        let _ = write_line(errors, format!("Error saving config: {error}"));
        return 1;
    }
    if let Some(token) = token {
        config.key = token;
        // The one-line API swap is unavoidable after save_config became identity-preserving:
        // setup still performs its original single whole-config write when given a token.
        if let Err(error) = save_config_with_identity(&config) {
            let _ = write_line(errors, format!("Error saving config: {error}"));
            return 1;
        }
        let result = write_line(output, format!("Journal: {}", config.server_url))
            .and_then(|()| write_line(output, format!("Stream: {}", config.stream)))
            .and_then(|()| write_line(output, "Using provided token; skipping registration."))
            .and_then(|()| setup_footer(output, &config));
        return if result.is_ok() { 0 } else { 1 };
    }
    if let Err(error) = save_config(&config) {
        let _ = write_line(errors, format!("Error saving config: {error}"));
        return 1;
    }
    let host = hostname().unwrap_or_else(|_| "linux".into());
    let result = if config.key.is_empty() {
        if write_line(output, "Registering with your journal...").is_err() {
            return 1;
        }
        match registrar.register(&mut config, &host) {
            Ok(true) => write_line(
                output,
                format!("Registered (key: {}...)", key_prefix(&config.key)),
            )
            .and_then(|()| write_line(output, format!("Stream: {}", config.stream))),
            Ok(false) => {
                let result = write_line(
                    output,
                    "Warning: registration failed. Run setup again when your journal is available.",
                );
                if options.non_interactive {
                    return 1;
                }
                result
            }
            Err(error) => {
                let _ = write_line(errors, format!("Registration failed: {error}"));
                return 1;
            }
        }
    } else {
        write_line(
            output,
            format!("Already registered (key: {}...)", key_prefix(&config.key)),
        )
        .and_then(|()| write_line(output, format!("Stream: {}", config.stream)))
    };
    let result = result.and_then(|()| setup_footer(output, &config));
    if result.is_ok() { 0 } else { 1 }
}

fn setup_footer(output: &mut dyn Write, config: &Config) -> io::Result<()> {
    write_line(
        output,
        format!("\nConfig saved to {}", config.config_path().display()),
    )?;
    write_line(
        output,
        format!("segments are kept in {}", config.captures_dir().display()),
    )?;
    write_line(
        output,
        "\nRun 'solstone-linux run' to start, or 'solstone-linux install-service' for systemd.",
    )
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
        config.chat_bridge_enabled =
            prompt_bool(prompt, "Chat bridge enabled", config.chat_bridge_enabled)?;
        config.cache_retention_days = prompt_retention(prompt, config.cache_retention_days)?;
        save_config(&config)?;
        prompt.write_line(&format!(
            "\nSettings saved to {}",
            config.config_path().display()
        ))
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
    let journal = if config.server_url.is_empty() {
        "(not configured)"
    } else {
        &config.server_url
    };
    let key = if config.key.is_empty() {
        "(not registered)".into()
    } else {
        format!("{}...", key_prefix(&config.key))
    };
    let stream = if config.stream.is_empty() {
        "(not set)"
    } else {
        &config.stream
    };
    let mut render = || -> io::Result<()> {
        write_line(
            output,
            format!("Config: {}", config.config_path().display()),
        )?;
        write_line(output, format!("Journal: {journal}"))?;
        write_line(output, format!("Key:    {key}"))?;
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
        let facts = load_facts(&config.state_dir());
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
    crate::run::run_observer(config, hostname().unwrap_or_else(|_| "linux".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::save_identity;
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

    struct FakeRegistrar {
        result: bool,
        calls: usize,
    }
    impl Registrar for FakeRegistrar {
        fn register(&mut self, config: &mut Config, _: &str) -> Result<bool, String> {
            self.calls += 1;
            if self.result {
                config.key = "newkey00".into();
                config.stream = "locked-stream".into();
                save_identity(
                    &ConfigPaths {
                        base_dir: Some(config.base_dir.clone()),
                        config_dir: Some(config.config_dir.clone()),
                    },
                    &config.key,
                    &config.stream,
                )
                .unwrap();
            }
            Ok(self.result)
        }
    }

    fn setup_options(token: Option<&str>, non_interactive: bool) -> SetupOptions {
        SetupOptions {
            server_url: Some("https://x".into()),
            token: token.map(str::to_owned),
            stream_name: Some("host-a".into()),
            non_interactive,
        }
    }

    // tests/test_cli.py::test_cmd_setup_non_interactive_happy_path
    #[test]
    fn setup_token_happy_path() {
        let t = tempfile::tempdir().unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            cmd_setup(
                setup_options(Some("t"), true),
                paths(&t),
                None,
                &mut registrar,
                &mut out,
                &mut err
            ),
            0
        );
        let config = load_config(paths(&t)).config;
        assert_eq!(
            (
                config.server_url.as_str(),
                config.key.as_str(),
                config.stream.as_str()
            ),
            ("https://x", "t", "host-a")
        );
        assert_eq!(registrar.calls, 0);
    }

    // tests/test_cli.py::test_cmd_setup_non_interactive_defaults_server_url
    #[test]
    fn setup_defaults_server_url() {
        let t = tempfile::tempdir().unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        let mut options = setup_options(None, true);
        options.server_url = None;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            cmd_setup(options, paths(&t), None, &mut registrar, &mut out, &mut err),
            0
        );
        assert_eq!(load_config(paths(&t)).config.server_url, DEFAULT_SERVER_URL);
        assert!(err.is_empty());
    }

    // tests/test_cli.py::test_cmd_setup_server_url_override_persists
    // tests/test_cli.py::test_cmd_setup_preserves_existing_server_url
    #[test]
    fn setup_url_precedence() {
        let t = tempfile::tempdir().unwrap();
        let mut config = load_config(paths(&t)).config;
        config.server_url = "https://saved.example".into();
        save_config(&config).unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        let mut options = setup_options(Some("token"), true);
        options.server_url = None;
        assert_eq!(
            cmd_setup(
                options,
                paths(&t),
                None,
                &mut registrar,
                &mut Vec::new(),
                &mut Vec::new()
            ),
            0
        );
        assert_eq!(
            load_config(paths(&t)).config.server_url,
            "https://saved.example"
        );
        let mut options = setup_options(Some("token"), true);
        options.server_url = Some("http://192.168.1.50:5015".into());
        assert_eq!(
            cmd_setup(
                options,
                paths(&t),
                None,
                &mut registrar,
                &mut Vec::new(),
                &mut Vec::new()
            ),
            0
        );
        assert_eq!(
            load_config(paths(&t)).config.server_url,
            "http://192.168.1.50:5015"
        );
    }

    // tests/test_cli.py::test_cmd_setup_flagged_interactive_empty_input_defaults
    // tests/test_cli.py::test_cmd_setup_interactive_legacy_empty_input_defaults
    #[test]
    fn setup_with_or_without_stream_flag_uses_default_url() {
        for stream in [Some("host-x"), None] {
            let t = tempfile::tempdir().unwrap();
            let mut registrar = FakeRegistrar {
                result: true,
                calls: 0,
            };
            let options = SetupOptions {
                server_url: None,
                token: None,
                stream_name: stream.map(str::to_owned),
                non_interactive: false,
            };
            assert_eq!(
                cmd_setup(
                    options,
                    paths(&t),
                    None,
                    &mut registrar,
                    &mut Vec::new(),
                    &mut Vec::new()
                ),
                0
            );
            assert_eq!(load_config(paths(&t)).config.server_url, DEFAULT_SERVER_URL);
        }
    }

    // tests/test_cli.py::test_cmd_setup_env_token_fallback
    // tests/test_cli.py::test_cmd_setup_cli_token_beats_env
    #[test]
    fn setup_token_precedence_and_warning() {
        let t = tempfile::tempdir().unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        let mut err = Vec::new();
        assert_eq!(
            cmd_setup(
                setup_options(None, true),
                paths(&t),
                Some("envtok".into()),
                &mut registrar,
                &mut Vec::new(),
                &mut err
            ),
            0
        );
        assert!(err.is_empty());
        assert_eq!(load_config(paths(&t)).config.key, "envtok");
        assert_eq!(
            cmd_setup(
                setup_options(Some("clitok"), true),
                paths(&t),
                Some("envtok".into()),
                &mut registrar,
                &mut Vec::new(),
                &mut err
            ),
            0
        );
        assert_eq!(load_config(paths(&t)).config.key, "clitok");
        assert!(String::from_utf8(err).unwrap().contains("shared computers"));
    }

    // AC: an empty SOLSTONE_TOKEN is absent and registration still runs.
    #[test]
    fn setup_empty_env_token_registers() {
        let t = tempfile::tempdir().unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        let mut output = Vec::new();
        assert_eq!(
            cmd_setup(
                setup_options(None, true),
                paths(&t),
                Some(String::new()),
                &mut registrar,
                &mut output,
                &mut Vec::new()
            ),
            0
        );
        assert_eq!(registrar.calls, 1);
        assert_eq!(load_config(paths(&t)).config.key, "newkey00");
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Registering with your journal..."));
        assert!(!output.contains("Using provided token"));
    }

    // tests/test_cli.py::test_cmd_setup_registers_via_http_when_no_token
    #[test]
    fn setup_registers_without_token() {
        let t = tempfile::tempdir().unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        assert_eq!(
            cmd_setup(
                setup_options(None, true),
                paths(&t),
                None,
                &mut registrar,
                &mut Vec::new(),
                &mut Vec::new()
            ),
            0
        );
        let config = load_config(paths(&t)).config;
        assert_eq!(
            (registrar.calls, config.key.as_str(), config.stream.as_str()),
            (1, "newkey00", "locked-stream")
        );
    }

    // AC: an existing key skips registration and prints the parity line.
    #[test]
    fn setup_already_registered_short_circuits() {
        let t = tempfile::tempdir().unwrap();
        let mut config = load_config(paths(&t)).config;
        config.server_url = "https://saved.example".into();
        config.key = "abcdefghijk".into();
        config.stream = "host-a".into();
        save_config(&config).unwrap();
        let mut registrar = FakeRegistrar {
            result: true,
            calls: 0,
        };
        let options = SetupOptions {
            server_url: None,
            token: None,
            stream_name: None,
            non_interactive: false,
        };
        let mut out = Vec::new();
        assert_eq!(
            cmd_setup(
                options,
                paths(&t),
                None,
                &mut registrar,
                &mut out,
                &mut Vec::new()
            ),
            0
        );
        assert_eq!(registrar.calls, 0);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with("Already registered (key: abcdefgh...)\nStream: host-a\n")
        );
    }

    // tests/test_cli.py::test_cmd_setup_http_register_failure_non_interactive_returns_1
    #[test]
    fn setup_noninteractive_failure_omits_footer() {
        let t = tempfile::tempdir().unwrap();
        let mut registrar = FakeRegistrar {
            result: false,
            calls: 0,
        };
        let mut out = Vec::new();
        assert_eq!(
            cmd_setup(
                setup_options(None, true),
                paths(&t),
                None,
                &mut registrar,
                &mut out,
                &mut Vec::new()
            ),
            1
        );
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("registration failed"));
        assert!(!out.contains("Config saved"));
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
        config.server_url = "https://id".into();
        config.key = "KKKK".into();
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
        let (config, _) = run_settings(&t, &["", "", "", "", "", ""]);
        assert_eq!(
            (
                config.capture_framerate,
                config.draw_cursor,
                config.start_paused,
                config.segment_interval,
                config.chat_bridge_enabled,
                config.cache_retention_days
            ),
            (2, true, false, 300, true, 7)
        );
        assert_eq!(
            (
                config.server_url.as_str(),
                config.key.as_str(),
                config.stream.as_str()
            ),
            ("https://id", "KKKK", "strm")
        );
    }

    // tests/test_cli.py::test_cmd_settings_changes_framerate
    #[test]
    fn settings_changes_framerate() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            run_settings(&t, &["5", "", "", "", "", ""])
                .0
                .capture_framerate,
            5
        );
    }

    // tests/test_cli.py::test_cmd_settings_framerate_clamped
    #[test]
    fn settings_framerate_clamped() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["99", "", "", "", "", ""]);
        assert_eq!(config.capture_framerate, 10);
        assert!(output.contains("(clamped to 10)"));
    }

    // tests/test_cli.py::test_cmd_settings_framerate_reprompts_on_invalid
    #[test]
    fn settings_framerate_reprompts() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["abc", "3", "", "", "", "", ""]);
        assert_eq!(config.capture_framerate, 3);
        assert!(output.contains("Enter an integer."));
    }

    // tests/test_cli.py::test_cmd_settings_toggles_bool
    #[test]
    fn settings_toggles_bool() {
        let t = tempfile::tempdir().unwrap();
        assert!(!run_settings(&t, &["", "n", "", "", "", ""]).0.draw_cursor);
    }

    // tests/test_cli.py::test_cmd_settings_retention_semantics
    #[test]
    fn settings_retention_accepts_negative() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            run_settings(&t, &["", "", "", "", "", "-1"])
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
        config.server_url = "https://test.example.com".into();
        config.key = "K123456789".into();
        config.stream = "test-stream".into();
        save_config(&config).unwrap();
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
            "Config: {}\nJournal: https://test.example.com\nKey:    K1234567...\nStream: test-stream\n\nCache:  {}\n        0 segments across 0 day(s), 0.0 MB\nRetain: 7 day(s)\nSync: offline — saving locally; pending unconfirmed (will retry)\n\nService: active\n",
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
        assert!(out.contains("        Quarantine: 1 rejected segment(s) held, oldest 0d"));
        assert!(!out.contains("Service:"));
    }

    // tests/test_cli.py::test_cmd_status_handles_corrupt_config
    #[test]
    fn status_handles_corrupt_config() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("config")).unwrap();
        fs::write(t.path().join("config/config.json"), "[]").unwrap();
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Journal: (not configured)")
        );
    }

    // AC: key truncation counts characters rather than UTF-8 bytes.
    #[test]
    fn status_key_prefix_is_character_based() {
        assert_eq!(key_prefix("ééééééééé"), "éééééééé");
    }
}
