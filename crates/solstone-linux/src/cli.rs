// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    capture_stats::{
        compute_quarantine_stats, compute_status_capture_stats, format_quarantine_line,
    },
    config::{Config, ConfigPaths, load_config, sanitize_link_authority, save_config},
    private_link::{
        PrivateIoOperation, PrivateStateError, PrivateStateLock, PrivateStateLockLiveness,
        load_credential, setup_with_stream,
    },
    session_env::{self, Output, Runner},
    streams::stream_name,
    sync_health::{ProcessEpoch, SyncFacts, derive_health, load_facts_with_liveness, save_facts},
};
use clap::{Parser, Subcommand};
use spl_transport::credential::Credential;
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
    about = "the solstone app for linux takes in what you share with it, and all of it goes into your journal. part of solstone.",
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
    #[command(about = "start the solstone app")]
    Run {
        #[arg(long, help = "Segment duration in seconds (default: 300)")]
        interval: Option<i64>,
    },
    #[command(
        about = "pair this device with your journal",
        long_about = "pair this device with your journal.\n\nExit codes:\n  0  paired and confirmed, or already confirmed\n  1  not paired, nothing changed, or no terminal\n  2  usage error\n  5  held"
    )]
    Setup {
        #[arg(long, help = crate::journal_mark::MARK_HELP)]
        mark: Option<String>,
        #[arg(long, help = "Stream name (defaults to hostname-derived)")]
        stream_name: Option<String>,
    },
    #[command(
        about = "confirm your journal's mark",
        long_about = "confirm your journal's mark.\n\nExit codes:\n  0  paired and confirmed, or already confirmed\n  1  not paired, nothing changed, or no terminal\n  2  usage error\n  5  held"
    )]
    Confirm {
        #[arg(long, help = crate::journal_mark::MARK_HELP)]
        mark: Option<String>,
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
    #[command(
        name = "panel-icon",
        about = "set up the GNOME panel icon, where pause and resume live"
    )]
    PanelIcon,
    #[command(about = "pause intake")]
    Pause {
        #[arg(
            long,
            help = "How many minutes to pause for (default: until you resume)"
        )]
        minutes: Option<u64>,
    },
    #[command(about = "resume intake")]
    Resume,
    #[cfg(feature = "browser")]
    #[command(
        name = "discard-browser-pages",
        about = "discard browser pages kept for a journal this computer was paired with before"
    )]
    DiscardBrowserPages,
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

pub(crate) fn hostname() -> io::Result<String> {
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
    // A browser's native-messaging launch is recognised from argv alone, before any
    // other startup work: no logging, no config, and no credential lock.
    if crate::browser::ENABLED {
        use crate::browser::argv::{Recognition, recognize};
        let arguments: Vec<_> = env::args_os().skip(1).collect();
        match recognize(&arguments, crate::browser::DEV_ENABLED) {
            Recognition::Host(invocation) => return crate::browser::host::run(invocation),
            Recognition::Refused => return crate::browser::host::refuse(&mut io::stdout()),
            Recognition::NotHost => {}
        }
    }
    let args = Args::parse();
    setup_logging(args.verbose);
    match effective_command(args.command) {
        Commands::Run { interval } => cmd_run(interval),
        Commands::Setup { mark, stream_name } => cmd_setup(
            SetupOptions { mark, stream_name },
            ConfigPaths::default(),
            &mut io::stdin().lock(),
            &mut io::stdout(),
            &mut io::stderr(),
        ),
        Commands::Confirm { mark } => cmd_confirm(
            mark,
            ConfigPaths::default(),
            &mut io::stdout(),
            &mut io::stderr(),
        ),
        Commands::Settings => cmd_settings(ConfigPaths::default(), &mut ConsolePrompt),
        Commands::Status => cmd_status(ConfigPaths::default(), &SystemRunner, &mut io::stdout()),
        Commands::PanelIcon => cmd_panel_icon(&mut io::stdout(), &mut io::stderr()),
        Commands::Pause { minutes } => cmd_control(
            Control::Pause(minutes),
            &mut io::stdout(),
            &mut io::stderr(),
        ),
        Commands::Resume => cmd_control(Control::Resume, &mut io::stdout(), &mut io::stderr()),
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
        #[cfg(feature = "browser")]
        Commands::DiscardBrowserPages => {
            cmd_discard_browser_pages(ConfigPaths::default(), &mut io::stdout(), &mut io::stderr())
        }
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
    mark: Option<String>,
    stream_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Pause(Option<u64>),
    Resume,
}

/// Pause and resume from a terminal.
///
/// This is the control an owner has when no panel icon can be reached at all -- no
/// StatusNotifier host, a desktop that blocks extension installs, or a headless box.
/// It drives the same Observer1 methods the panel icon menu does.
fn cmd_control(control: Control, output: &mut dyn Write, errors: &mut dyn Write) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = write_line(errors, format!("could not start: {error}"));
            return 1;
        }
    };
    runtime.block_on(async {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = write_line(
                    errors,
                    format!("no session bus, so the solstone app cannot be reached: {error}"),
                );
                return 1;
            }
        };
        let outcome = match control {
            Control::Pause(minutes) => crate::panel_icon::pause(&connection, minutes).await,
            Control::Resume => crate::panel_icon::resume(&connection).await,
        };
        let (message, code) = crate::panel_icon::control_result(&outcome);
        let _ = write_line(if code == 0 { output } else { errors }, message);
        code
    })
}

/// The path an owner can always reach, including where the in-app offer cannot run --
/// dismissed, no notification daemon, or a managed desktop that blocks the offer.
fn cmd_panel_icon(output: &mut dyn Write, errors: &mut dyn Write) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = write_line(errors, format!("could not start: {error}"));
            return 1;
        }
    };
    runtime.block_on(async {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = write_line(
                    errors,
                    format!("no session bus, so there is no desktop to set up: {error}"),
                );
                return 1;
            }
        };
        let readiness = crate::panel_icon::probe(&connection).await;
        let _ = write_line(output, crate::panel_icon::command_preamble(readiness));
        let outcome = crate::panel_icon::set_up(&connection, readiness).await;
        let (message, code) = crate::panel_icon::command_result(&outcome);
        let _ = write_line(if code == 0 { output } else { errors }, message);
        code
    })
}

fn write_line(output: &mut dyn Write, value: impl std::fmt::Display) -> io::Result<()> {
    writeln!(output, "{value}")
}

// SOLSTONE_LINUX_MARK_TTY is a test seam, not a trust boundary.
fn open_terminal() -> Option<fs::File> {
    if let Some(path) = env::var_os("SOLSTONE_LINUX_MARK_TTY").filter(|p| !p.is_empty()) {
        return rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(fs::File::from)
        .ok();
    }
    if cfg!(test) {
        None
    } else {
        rustix::fs::open(
            "/dev/tty",
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(fs::File::from)
        .ok()
    }
}

fn cmd_setup(
    options: SetupOptions,
    paths: ConfigPaths,
    input: &mut dyn Read,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    use std::os::fd::AsFd;
    let host = hostname().unwrap_or_else(|_| "linux".into());
    let stream = options
        .stream_name
        .filter(|value| !value.is_empty())
        .or_else(|| stream_name(Some(&host), None, None).ok());
    let config = load_config(paths).config;
    let config_root = config.config_dir.clone();
    let state_dir = config.state_dir();
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
    let _runtime_guard = runtime.enter();
    let tty = open_terminal();
    let result = runtime.block_on(setup_with_stream(
        &config_root,
        &state_dir,
        &host,
        stream.as_deref(),
        options.mark.as_deref(),
        tty.as_ref().map(|f| f.as_fd()),
        input,
    ));
    render_setup_result(result, output, errors)
}

fn cmd_confirm(
    mark: Option<String>,
    paths: ConfigPaths,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    use std::os::fd::AsFd;
    let config = load_config(paths).config;
    let config_root = config.config_dir.clone();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = write_line(errors, format!("Error: {error}"));
            return 1;
        }
    };
    let _runtime_guard = runtime.enter();
    let tty = open_terminal();
    runtime.block_on(confirm_async(
        &config_root,
        mark.as_deref(),
        tty.as_ref().map(|f| f.as_fd()),
        output,
        errors,
    ))
}

pub(crate) fn reload_same_pairing(
    config_root: &std::path::Path,
    pairing_id: &str,
) -> Result<Credential, PrivateStateError> {
    match load_credential(config_root)? {
        Some(credential) => {
            if crate::private_link::compute_pairing_id(&credential.client_cert_pem) != pairing_id {
                Err(PrivateStateError::CredentialChanged)
            } else {
                Ok(credential)
            }
        }
        None => Err(PrivateStateError::Io {
            operation: PrivateIoOperation::Read,
            source: io::Error::new(io::ErrorKind::NotFound, "credential absent"),
        }),
    }
}

pub(crate) fn drop_same_pairing(
    config_root: &std::path::Path,
    pairing_id: &str,
) -> Result<(), PrivateStateError> {
    reload_same_pairing(config_root, pairing_id)?;
    fs::remove_file(config_root.join(crate::private_link::CREDENTIALS_FILENAME)).map_err(|source| {
        PrivateStateError::Io {
            operation: PrivateIoOperation::Remove,
            source,
        }
    })
}

pub(crate) async fn confirm_async<Fd: std::os::fd::AsFd>(
    config_root: &std::path::Path,
    mark: Option<&str>,
    terminal_fd: Option<Fd>,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let mark_words = if let Some(mark_str) = mark {
        match crate::journal_mark::parse_mark_words(mark_str) {
            Some(words) => Some(words),
            None => {
                let _ = write_line(output, crate::journal_mark::MARK_USAGE);
                return 2;
            }
        }
    } else {
        None
    };

    let answer_lock = match crate::journal_mark::AnswerLock::acquire(config_root).await {
        Ok(lock) => lock,
        Err(error) => {
            let _ = write_line(errors, format!("Error: {error}"));
            return 1;
        }
    };

    if let Err(error) = crate::journal_mark::grandfather_answer_file(config_root) {
        let _ = write_line(errors, format!("Error: {error}"));
        return 1;
    }

    let credential = match load_credential(config_root) {
        Ok(Some(cred)) => cred,
        Ok(None) => {
            let _ = write_line(output, crate::journal_mark::CONFIRM_UNPAIRED);
            return 1;
        }
        Err(error) => {
            let _ = write_line(errors, format!("Error: {error}"));
            return 1;
        }
    };

    let pairing_id = crate::private_link::compute_pairing_id(&credential.client_cert_pem);
    if crate::journal_mark::is_pairing_confirmed(config_root, &pairing_id) {
        let _ = write_line(output, crate::journal_mark::CONFIRM_DONE);
        return 0;
    }

    if mark_words.is_none() && terminal_fd.is_none() {
        let _ = write_line(output, crate::journal_mark::CONFIRM_NO_TERMINAL);
        return 1;
    }

    if let Some(words) = mark_words {
        match crate::journal_mark::compare_mark_to_credential(&credential, &words) {
            Err(()) => {
                let _ = write_line(output, crate::journal_mark::CONFIRM_UNVERIFIABLE_LINE);
                5
            }
            Ok(true) => {
                if let Err(error) = reload_same_pairing(config_root, &pairing_id) {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                if let Err(error) =
                    crate::journal_mark::write_pairing_answer(config_root, &pairing_id)
                {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                let _ = write_line(output, crate::journal_mark::SUCCESS_LINE);
                0
            }
            Ok(false) => {
                drop(answer_lock);
                crate::journal_mark::retire_client_registration(&credential).await;
                let _reacquired_lock =
                    match crate::journal_mark::AnswerLock::acquire(config_root).await {
                        Ok(lock) => lock,
                        Err(error) => {
                            let _ = write_line(errors, format!("Error: {error}"));
                            return 1;
                        }
                    };
                if let Err(error) = drop_same_pairing(config_root, &pairing_id) {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                let _ = write_line(output, crate::journal_mark::NOT_PAIRED);
                let _ = write_line(output, crate::journal_mark::MARK_MISMATCH_LINE);
                1
            }
        }
    } else {
        drop(answer_lock);
        let outcome =
            crate::journal_mark::ask_terminal_question(terminal_fd.unwrap(), &credential).await;
        match outcome {
            crate::journal_mark::QuestionOutcome::Confirmed => {
                let _lock = match crate::journal_mark::AnswerLock::acquire(config_root).await {
                    Ok(lock) => lock,
                    Err(error) => {
                        let _ = write_line(errors, format!("Error: {error}"));
                        return 1;
                    }
                };
                if let Err(error) = reload_same_pairing(config_root, &pairing_id) {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                if let Err(error) =
                    crate::journal_mark::write_pairing_answer(config_root, &pairing_id)
                {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                let _ = write_line(output, crate::journal_mark::SUCCESS_LINE);
                0
            }
            crate::journal_mark::QuestionOutcome::No => {
                crate::journal_mark::retire_client_registration(&credential).await;
                let _lock = match crate::journal_mark::AnswerLock::acquire(config_root).await {
                    Ok(lock) => lock,
                    Err(error) => {
                        let _ = write_line(errors, format!("Error: {error}"));
                        return 1;
                    }
                };
                if let Err(error) = drop_same_pairing(config_root, &pairing_id) {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                let _ = write_line(output, crate::journal_mark::NOT_PAIRED);
                let _ = write_line(output, crate::journal_mark::MISMATCH_BODY);
                1
            }
            crate::journal_mark::QuestionOutcome::Cancel => {
                crate::journal_mark::retire_client_registration(&credential).await;
                let _lock = match crate::journal_mark::AnswerLock::acquire(config_root).await {
                    Ok(lock) => lock,
                    Err(error) => {
                        let _ = write_line(errors, format!("Error: {error}"));
                        return 1;
                    }
                };
                if let Err(error) = drop_same_pairing(config_root, &pairing_id) {
                    let _ = write_line(errors, format!("Error: {error}"));
                    return 1;
                }
                let _ = write_line(output, crate::journal_mark::CANCEL_LINE);
                1
            }
            crate::journal_mark::QuestionOutcome::WalkedAway => {
                let _ = write_line(output, crate::journal_mark::HELD_BOTH_SENTENCES);
                let _ = write_line(output, crate::journal_mark::RUN_LINE);
                5
            }
        }
    }
}

pub(crate) fn render_setup_result(
    result: Result<crate::private_link::SetupOutcome, PrivateStateError>,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    match result {
        Ok(crate::private_link::SetupOutcome::Confirmed) => {
            let _ = write_line(output, crate::journal_mark::SUCCESS_LINE);
            0
        }
        Ok(crate::private_link::SetupOutcome::MarkUsage) => {
            let _ = write_line(output, crate::journal_mark::MARK_USAGE);
            2
        }
        Ok(crate::private_link::SetupOutcome::NoTerminal) => {
            let _ = write_line(output, crate::journal_mark::SETUP_NO_TERMINAL);
            1
        }
        Ok(crate::private_link::SetupOutcome::TerminalNo) => {
            let _ = write_line(output, crate::journal_mark::NOT_PAIRED);
            let _ = write_line(output, crate::journal_mark::MISMATCH_BODY);
            1
        }
        Ok(crate::private_link::SetupOutcome::TerminalCancel) => {
            let _ = write_line(output, crate::journal_mark::CANCEL_LINE);
            1
        }
        Ok(crate::private_link::SetupOutcome::MarkMismatch) => {
            let _ = write_line(output, crate::journal_mark::NOT_PAIRED);
            let _ = write_line(output, crate::journal_mark::MARK_MISMATCH_LINE);
            1
        }
        Ok(crate::private_link::SetupOutcome::SetupUnverifiable) => {
            let _ = write_line(output, crate::journal_mark::COULDNT_VERIFY);
            let _ = write_line(output, crate::journal_mark::SETUP_UNVERIFIABLE_LINE);
            1
        }
        Ok(crate::private_link::SetupOutcome::WalkedAwayHeld) => {
            let _ = write_line(output, crate::journal_mark::HELD_BOTH_SENTENCES);
            let _ = write_line(output, crate::journal_mark::RUN_LINE);
            5
        }
        Ok(crate::private_link::SetupOutcome::WalkedAwayConfirmedAlready) => {
            let _ = write_line(output, crate::journal_mark::CANCEL_LINE);
            1
        }
        Err(PrivateStateError::PairInputInvalid) => {
            let _ = write_line(errors, "Setup failed: the pair link was not valid.");
            1
        }
        Err(PrivateStateError::PairingFailed) => {
            let _ = write_line(
                errors,
                "Setup failed: the solstone app could not connect to your journal.",
            );
            1
        }
        Err(PrivateStateError::LockContended) => {
            let _ = write_line(
                errors,
                "Setup could not start because the solstone app is running. Stop the solstone app first and try again. No input was consumed; app state, config, and private state are unchanged.",
            );
            1
        }
        Err(
            error @ PrivateStateError::Io {
                operation: crate::private_link::PrivateIoOperation::Lock,
                ..
            },
        ) => {
            let _ = write_line(errors, format!("Setup failed: {error}"));
            1
        }
        Err(error @ (PrivateStateError::Io { .. } | PrivateStateError::InvalidTarget { .. })) => {
            let _ = write_line(
                errors,
                "Setup failed before pairing because the solstone app could not safely update its config.",
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_setup_with_pairer_for_test<R: Read>(
    pairer: &dyn crate::private_link::Pairer,
    config_root: &std::path::Path,
    state_dir: &std::path::Path,
    stream: &str,
    mark: Option<&str>,
    input: R,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let result = crate::private_link::setup_with_pairer_for_test(
        pairer,
        config_root,
        state_dir,
        "linux",
        Some(stream),
        mark,
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

fn cmd_settings(paths: ConfigPaths, prompt: &mut dyn PromptIo) -> i32 {
    let loaded = load_config(paths);
    let mut config = loaded.config;
    let result = (|| -> io::Result<()> {
        config.capture_framerate = prompt_framerate(prompt, config.capture_framerate)?;
        config.draw_cursor = prompt_bool(prompt, "Draw cursor", config.draw_cursor)?;
        config.start_paused = prompt_bool(prompt, "Start paused", config.start_paused)?;
        config.segment_interval =
            prompt_positive_int(prompt, "Segment interval seconds", config.segment_interval)?;
        save_config(&config)?;
        prompt.write_line(&format!(
            "\nSettings saved to {}",
            config.config_path().display()
        ))?;
        // Config is read once at startup and never re-read, so a running sol keeps the
        // old values. Saying only "saved" invites the owner to believe otherwise.
        prompt.write_line("These take effect the next time the solstone app starts.")?;
        prompt.write_line("  systemctl --user restart solstone-linux")
    })();
    if let Err(error) = result {
        eprintln!("Error editing settings: {error}");
        1
    } else {
        0
    }
}

fn escape_display_version(raw: &str) -> String {
    let mut clean = String::with_capacity(raw.len());
    let mut in_escape = false;
    for ch in raw.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        if ch.is_ascii_control() || ch == '\r' || ch == '\n' {
            continue;
        }
        clean.push(ch);
    }
    clean
}

#[cfg_attr(not(feature = "browser"), allow(dead_code))]
fn cmd_discard_browser_pages(
    paths: ConfigPaths,
    output: &mut dyn Write,
    errors: &mut dyn Write,
) -> i32 {
    let config = load_config(paths).config;
    let layout = crate::browser::custody::Layout::new(&config.base_dir);
    match crate::browser::custody::discard_retired(&layout) {
        Ok(summary) if summary.periods == 0 => {
            let _ = write_line(
                output,
                "there are no browser pages kept for a journal this computer was paired with before",
            );
            0
        }
        Ok(_) => {
            let _ = write_line(
                output,
                "discarded the browser pages kept for a journal this computer was paired with before",
            );
            0
        }
        Err(error) => {
            let _ = write_line(
                errors,
                format!("could not discard the browser pages: {error}"),
            );
            1
        }
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
    let liveness = PrivateStateLock::try_probe(&config.config_dir)
        .unwrap_or(PrivateStateLockLiveness::NoLiveOwner);
    let mut facts = load_facts_with_liveness(&config.state_dir(), liveness);
    if liveness == PrivateStateLockLiveness::LiveOwner {
        if let Some(link) = facts.link.as_mut() {
            crate::journal_mark::apply_live_owner_journal_mark_held(link, &config.config_dir);
        }
    } else if crate::journal_mark::journal_mark_held_on_disk(&config.config_dir) {
        facts.link = Some(crate::private_link::LinkFactState {
            journal_mark_held: true,
            ..Default::default()
        });
    }

    let (link_line, version_line) = if crate::private_link::credential_present(&config.config_dir) {
        let version_str = match crate::private_link::load_credential(&config.config_dir) {
            Ok(Some(credential)) => {
                let current_key = crate::private_link::journal_identity_key(&credential);
                let stored = crate::sync_health::load_paired_journal_version(&config.state_dir());
                match stored {
                    Some(entry) if entry.identity_key == current_key => {
                        let is_current = liveness == PrivateStateLockLiveness::LiveOwner
                            && facts.link.as_ref().is_some_and(|l| {
                                l.journal_version_observed
                                    && l.carrier_proven
                                    && !l.transport_unavailable
                            });
                        if is_current {
                            escape_display_version(&entry.version)
                        } else {
                            format!("{} (last known)", escape_display_version(&entry.version))
                        }
                    }
                    _ => "unknown".to_owned(),
                }
            }
            _ => "unknown".to_owned(),
        };
        (
            "Journal link: managed privately",
            Some(format!("Journal version: {version_str}")),
        )
    } else {
        ("Journal link: not paired", None)
    };
    let mut render = || -> io::Result<()> {
        write_line(
            output,
            format!("Config: {}", config.config_path().display()),
        )?;
        write_line(output, link_line)?;
        if let Some(line) = &version_line {
            write_line(output, line)?;
        }
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        write_line(
            output,
            derive_health(&facts, now, config.sync_stale_threshold as f64).cli,
        )?;
        if crate::browser::ENABLED {
            let now_ms = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            )
            .unwrap_or(i64::MAX);
            for line in crate::browser::status::status_lines(&config.base_dir, now_ms) {
                write_line(output, line)?;
            }
        }
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
    crate::run::run_observer(config, state_lock, transport_enabled, process_epoch)
}

fn run_preparation_error_guidance(error: &PrivateStateError) -> &'static str {
    match error {
        PrivateStateError::LockContended => "Linked private state is already in use",
        PrivateStateError::CaptureRootInUse => {
            "Another copy of the solstone app is already running for this login, so this one did not start. Stop the other copy, then try again."
        }
        PrivateStateError::HealthInitializationFailed => {
            "Startup could not continue because the solstone app could not clear the sync status from the previous run. Make sure the solstone app can write its local data, then try again."
        }
        _ => {
            "Startup could not continue because the solstone app could not safely prepare its local data. Make sure the solstone app can write its local data, then try again."
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
    state_lock.hold_capture_root(&config.base_dir)?;
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
    use crate::journal_mark::sample_credential;
    use clap::CommandFactory;
    use std::{
        cell::Cell,
        os::fd::AsFd,
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

    // root help pins the complete Python CLI subcommand surface.
    #[test]
    fn root_help_surface() {
        let command = Args::command();
        command.clone().debug_assert();
        let names: Vec<_> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect();
        let mut expected = vec![
            "run",
            "setup",
            "confirm",
            "doctor",
            "settings",
            "install-service",
            "uninstall-service",
            "status",
            "panel-icon",
            "pause",
            "resume",
        ];
        if crate::browser::ENABLED {
            expected.push("discard-browser-pages");
        }
        assert_eq!(names, expected);
    }

    // the panel-icon command's own help text is owner-visible.
    // the control commands name what they pause, not "the solstone app": the
    // process keeps running and must, in order to receive Resume.
    #[test]
    fn control_help_surface_names_intake_not_the_app() {
        let command = Args::command();
        for (name, expected) in [("pause", "pause intake"), ("resume", "resume intake")] {
            let about = command
                .find_subcommand(name)
                .unwrap()
                .get_about()
                .unwrap()
                .to_string();
            assert_eq!(about, expected);
            assert!(!about.contains("the solstone app"), "{name}");
        }
    }

    #[test]
    fn panel_icon_help_surface() {
        let command = Args::command();
        let panel_icon = command.find_subcommand("panel-icon").unwrap();
        assert_eq!(
            panel_icon.get_about().unwrap().to_string(),
            "set up the GNOME panel icon, where pause and resume live"
        );
    }
    // run help pins its interval option and exact help text.
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
    // verbose raises logging verbosity.
    #[test]
    fn verbose_flag() {
        assert!(
            Args::try_parse_from(["solstone-linux", "-v"])
                .unwrap()
                .verbose
        );
        assert!(!Args::try_parse_from(["solstone-linux"]).unwrap().verbose);
    }
    // the safe wrapper assigns the exact value and leaves the process environment as found.
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
    // bare invocation is run parity.
    #[test]
    fn bare_is_run() {
        let args = Args::try_parse_from(["solstone-linux"]).unwrap();
        assert_eq!(
            effective_command(args.command),
            Commands::Run { interval: None }
        );
    }
    // truthy interval overrides while zero does not.
    #[test]
    fn interval_semantics() {
        let mut config = Config::default();
        apply_interval(&mut config, Some(600));
        assert_eq!(config.segment_interval, 600);
        apply_interval(&mut config, Some(0));
        assert_eq!(config.segment_interval, 600);
    }
    // a genuinely unrecoverable session maps to EX_TEMPFAIL 75.
    #[test]
    fn not_ready_exit_75() {
        let runner = FakeRunner {
            output: Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
            calls: Cell::new(0),
        };
        let mut environment = HashMap::new();
        assert_eq!(exit_code(session_gate(&mut environment, 1000, &runner)), 75);
    }
    // non-session failures map to exit one.
    #[test]
    fn other_failure_exit_1() {
        assert_eq!(exit_code(Err(RunFailure::Other)), 1);
    }
    // PATH lookup requires an executable regular file.
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
    // proc hostname input is trimmed before stream validation.
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
    fn setup_help_exposes_only_mark_and_stream_name() {
        let command = Args::command();
        let setup = command.find_subcommand("setup").unwrap();
        let arguments = setup
            .get_arguments()
            .map(|argument| argument.get_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(arguments, vec!["mark", "stream_name"]);
        for removed in ["--server-url", "--token", "--non-interactive"] {
            assert!(Args::try_parse_from(["solstone-linux", "setup", removed]).is_err());
        }
    }

    #[test]
    fn confirm_help_exposes_only_mark() {
        let command = Args::command();
        let confirm = command.find_subcommand("confirm").unwrap();
        let arguments = confirm
            .get_arguments()
            .map(|argument| argument.get_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(arguments, vec!["mark"]);
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
                    mark: Some("bramble quokka".into()),
                    stream_name: Some("host-a".into()),
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
        assert!(input.position() > 0);
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
                    mark: None,
                    stream_name: Some("host-a".into()),
                },
                ConfigPaths {
                    base_dir: None,
                    config_dir: Some(config_root.clone()),
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
            "Setup could not start because the solstone app is running. Stop the solstone app first and try again. No input was consumed; app state, config, and private state are unchanged.\n"
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
            "Startup could not continue because the solstone app could not clear the sync status from the previous run. Make sure the solstone app can write its local data, then try again."
        );
        assert_eq!(
            generic,
            "Startup could not continue because the solstone app could not safely prepare its local data. Make sure the solstone app can write its local data, then try again."
        );
        assert_ne!(contention, initialization);
        assert_ne!(generic, initialization);
    }

    #[tokio::test]
    async fn setup_surfaces_never_disclose_pair_material() {
        let temp = tempfile::tempdir().unwrap();
        let config_root = temp.path().join("config");
        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&config_root).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        let pair_count = Arc::new(AtomicUsize::new(0));
        struct RecordingPairer {
            count: Arc<AtomicUsize>,
            cred: spl_transport::credential::Credential,
        }
        impl crate::private_link::Pairer for RecordingPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                self.count.fetch_add(1, Ordering::SeqCst);
                let cred = self.cred.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        let mark = spl_core::mark::mark_from_jid(&cred.instance_id).unwrap();
        let spec = mark.to_render_spec();
        let mark_str = format!("{} {}", spec.words[0], spec.words[1]);

        let pairer = RecordingPairer {
            count: pair_count.clone(),
            cred,
        };

        let link = crate::private_link::DIRECT_PAIR_LINK_FOR_TEST;
        let mut input = std::io::Cursor::new(link.as_bytes());
        let mut output = Vec::new();
        let mut errors = Vec::new();

        let status = dispatch_setup_with_pairer_for_test(
            &pairer,
            &config_root,
            &state_dir,
            "desktop",
            Some(&mark_str),
            &mut input,
            &mut output,
            &mut errors,
        )
        .await;

        assert_eq!(status, 0);
        assert_eq!(pair_count.load(Ordering::SeqCst), 1);
        let surfaces = format!(
            "{}{}",
            String::from_utf8(output).unwrap(),
            String::from_utf8(errors).unwrap()
        );
        assert!(!surfaces.contains(link.trim()));
    }

    #[tokio::test]
    async fn setup_sanitation_failure_reads_stdin() {
        let temp = tempfile::tempdir().unwrap();
        let config_root = temp.path().join("config");
        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&config_root).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_file_as_dir = config_root.join("config.json");
        std::fs::create_dir_all(&config_file_as_dir).unwrap();

        let reads = Arc::new(AtomicUsize::new(0));
        let mut input = CountingInput {
            bytes: std::io::Cursor::new(
                crate::private_link::DIRECT_PAIR_LINK_FOR_TEST
                    .as_bytes()
                    .to_vec(),
            ),
            reads: reads.clone(),
        };

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        let mark = spl_core::mark::mark_from_jid(&cred.instance_id).unwrap();
        let spec = mark.to_render_spec();
        let mark_str = format!("{} {}", spec.words[0], spec.words[1]);

        let mut output = Vec::new();
        let mut errors = Vec::new();

        let status = dispatch_setup_with_pairer_for_test(
            &DirectPairer(cred),
            &config_root,
            &state_dir,
            "desktop",
            Some(&mark_str),
            &mut input,
            &mut output,
            &mut errors,
        )
        .await;

        assert_eq!(status, 1);
        assert!(reads.load(Ordering::SeqCst) > 0);
        let errors = String::from_utf8(errors).unwrap();
        assert!(errors.contains("Config update error:"));
        let out = String::from_utf8(output).unwrap();
        assert!(!out.contains("paired."));
        assert!(!config_root.join("credentials.json").exists());
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
        let (config, _) = run_settings(&t, &["", "", "", ""]);
        assert_eq!(
            (
                config.capture_framerate,
                config.draw_cursor,
                config.start_paused,
                config.segment_interval,
            ),
            (2, true, false, 300)
        );
        assert_eq!(config.stream, "strm");
    }

    // tests/test_cli.py::test_cmd_settings_changes_framerate
    #[test]
    fn settings_changes_framerate() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(run_settings(&t, &["5", "", "", ""]).0.capture_framerate, 5);
    }

    // tests/test_cli.py::test_cmd_settings_framerate_clamped
    #[test]
    fn settings_framerate_clamped() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["99", "", "", ""]);
        assert_eq!(config.capture_framerate, 10);
        assert!(output.contains("(clamped to 10)"));
    }

    // tests/test_cli.py::test_cmd_settings_framerate_reprompts_on_invalid
    #[test]
    fn settings_framerate_reprompts() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["abc", "3", "", "", ""]);
        assert_eq!(config.capture_framerate, 3);
        assert!(output.contains("Enter an integer."));
    }

    // tests/test_cli.py::test_cmd_settings_toggles_bool
    #[test]
    fn settings_toggles_bool() {
        let t = tempfile::tempdir().unwrap();
        assert!(!run_settings(&t, &["", "n", "", ""]).0.draw_cursor);
    }

    // tests/test_cli.py::test_cmd_settings_retention_semantics
    #[test]
    fn settings_omits_retention_prompt_and_strips_key() {
        let t = tempfile::tempdir().unwrap();
        let (config, output) = run_settings(&t, &["", "", "", ""]);
        assert!(!output.contains("retention"));
        assert!(!output.contains("Retention"));
        let disk_text = fs::read_to_string(config.config_path()).unwrap();
        assert!(!disk_text.contains("cache_retention_days"));
    }

    // prompt failure leaves the persisted settings unchanged.
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
        let browser = if crate::browser::ENABLED {
            "Browser: the solstone app is not running\n"
        } else {
            ""
        };
        let expected = format!(
            "Config: {}\nJournal link: managed privately\nJournal version: unknown\nStream: test-stream\n\nCache:  {}\n        0 segments across 0 day(s), 0.0 MB\nSync: offline; held on this device; will retry\n{browser}\nService: active\n",
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
        let failed = config
            .captures_dir()
            .join("20260101/test-stream/120000_300.failed");
        fs::create_dir_all(&failed).unwrap();
        // Only a segment holding real media is reported as held; a metadata-only stub
        // captured nothing and is not unsent content.
        fs::write(failed.join("audio.flac"), b"x").unwrap();
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

    // the link line reports the link that exists, so it cannot contradict a sync line
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

    #[test]
    fn escape_display_version_cleans_dangerous_formatting() {
        assert_eq!(escape_display_version("1.4.0"), "1.4.0");
        assert_eq!(escape_display_version("\x1b[31m1.4.0\x1b[0m"), "1.4.0");
        assert_eq!(escape_display_version("1.4.0\r\n"), "1.4.0");
        assert_eq!(escape_display_version("1.4.0\x07\x08"), "1.4.0");
    }

    #[test]
    fn status_journal_version_rendering_states() {
        use spl_transport::credential::Credential;

        let t = tempfile::tempdir().unwrap();
        let config = status_config(&t);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        // 1. Unpaired -> no Journal version line
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("Journal link: not paired"));
        assert!(!out_str.contains("Journal version:"));

        // Set up credential
        let cred = Credential {
            instance_id: "inst-42".to_string(),
            ca_fp_prefix: vec![0xaa, 0xbb],
            endpoints: vec![],
            local_endpoints: None,
            client_cert_pem: String::new(),
            client_key_pem: String::new(),
            ca_chain_pem: vec![],
            home_label: "home".into(),
            home_attestation: None,
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        crate::private_link::persist_credential(&config.config_dir, &cred).unwrap();
        let identity_key = crate::private_link::journal_identity_key(&cred);

        // 2. Paired but no paired_journal.json -> "Journal version: unknown"
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("Journal link: managed privately"));
        assert!(out_str.contains("Journal version: unknown"));

        // Save version in paired_journal.json
        crate::sync_health::save_paired_journal_version(
            &config.state_dir(),
            &identity_key,
            "1.4.0",
            None,
        )
        .unwrap();

        // 3. Paired, cached (no live carrier) -> "Journal version: 1.4.0 (last known)"
        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("Journal version: 1.4.0 (last known)"));

        // 4. Paired, live owner + carrier_proven but NOT yet journal_version_observed -> still "(last known)"
        let mut state_lock =
            crate::private_link::PrivateStateLock::acquire(&config.config_dir).unwrap();
        state_lock.mark_ready().unwrap();
        let facts = SyncFacts {
            link: Some(crate::private_link::LinkFactState {
                carrier_proven: true,
                journal_version_observed: false,
                ..Default::default()
            }),
            link_epoch: Some(crate::sync_health::ProcessEpoch::for_test(1)),
            ..Default::default()
        };
        crate::sync_health::save_facts(&config.state_dir(), &facts).unwrap();

        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("Journal version: 1.4.0 (last known)"));

        // 5. Paired, live owner + journal_version_observed -> "Journal version: 1.4.0" (current)
        let facts_observed = SyncFacts {
            link: Some(crate::private_link::LinkFactState {
                carrier_proven: true,
                journal_version_observed: true,
                ..Default::default()
            }),
            link_epoch: Some(crate::sync_health::ProcessEpoch::for_test(1)),
            ..Default::default()
        };
        crate::sync_health::save_facts(&config.state_dir(), &facts_observed).unwrap();

        let mut out = Vec::new();
        assert_eq!(cmd_status(paths(&t), &StatusRunner(None), &mut out), 0);
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("Journal version: 1.4.0"));
        assert!(!out_str.contains("(last known)"));
    }

    #[tokio::test]
    async fn setup_pairing_clears_stale_paired_journal_sidecar() {
        use spl_transport::credential::Credential;

        let t = tempfile::tempdir().unwrap();
        let config = status_config(&t);
        let cred = Credential {
            instance_id: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
            ca_fp_prefix: vec![0xaa, 0xbb],
            endpoints: vec![],
            local_endpoints: None,
            client_cert_pem: String::new(),
            client_key_pem: String::new(),
            ca_chain_pem: vec![],
            home_label: "home".into(),
            home_attestation: None,
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        crate::private_link::persist_credential(&config.config_dir, &cred).unwrap();
        let identity_key = crate::private_link::journal_identity_key(&cred);

        // Populate stale paired_journal.json
        crate::sync_health::save_paired_journal_version(
            &config.state_dir(),
            &identity_key,
            "1.2.0",
            None,
        )
        .unwrap();
        assert!(crate::sync_health::load_paired_journal_version(&config.state_dir()).is_some());

        struct DirectPairer(Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move { Ok(self.0.clone()) })
            }
        }

        let mark = spl_core::mark::mark_from_jid(&cred.instance_id)
            .expect("test jid must resolve to a mark");
        let spec = mark.to_render_spec();
        let expected_mark = format!("{} {}", spec.words[0], spec.words[1]);

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = dispatch_setup_with_pairer_for_test(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some(&expected_mark),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(status, 0);

        // After successful pairing, paired_journal.json MUST have been deleted
        assert!(crate::sync_health::load_paired_journal_version(&config.state_dir()).is_none());
    }

    #[tokio::test]
    async fn setup_pairing_with_matching_mark_confirms_successfully() {
        use spl_transport::credential::Credential;

        let t = tempfile::tempdir().unwrap();
        let config = status_config(&t);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));
        let cred = Credential {
            instance_id: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
            ca_fp_prefix: vec![0xaa, 0xbb],
            endpoints: vec![],
            local_endpoints: None,
            client_cert_pem: String::new(),
            client_key_pem: String::new(),
            ca_chain_pem: vec![],
            home_label: "home".into(),
            home_attestation: None,
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        let mark = spl_core::mark::mark_from_jid(&cred.instance_id)
            .expect("test jid must resolve to a mark");
        let spec = mark.to_render_spec();
        let expected_mark = format!("{} {}", spec.words[0], spec.words[1]);

        struct DirectPairer(Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move { Ok(self.0.clone()) })
            }
        }

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = dispatch_setup_with_pairer_for_test(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some(&expected_mark),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(status, 0);

        let out_str = String::from_utf8(out).unwrap();
        assert_eq!(
            out_str,
            format!("{}\n", crate::journal_mark::SUCCESS_LINE),
            "setup success output should match exact success line: {out_str}"
        );
        assert!(!out_str.contains("Paste the pair link"));
        assert!(!out_str.contains("check it matches"));
    }

    #[tokio::test]
    async fn setup_no_retires_the_new_cert() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred = peer.credential();

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer.write_all(b"no\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();

        let res = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;

        let status = render_setup_result(res, &mut out, &mut err);
        assert_eq!(status, 1);
        assert!(!config.config_dir.join("credentials.json").exists());

        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(crate::journal_mark::NOT_PAIRED));
        assert!(out_str.contains(crate::journal_mark::MISMATCH_BODY));

        let hex = spl_core::ca::sha256_hex(peer.client_der());
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_cancel_retires_the_new_cert() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let mut cred = peer.credential();
        cred.instance_id = "invalid-jid".into();

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer.write_all(b"cancel\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();

        let res = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;

        let status = render_setup_result(res, &mut out, &mut err);
        assert_eq!(status, 1);
        assert!(!config.config_dir.join("credentials.json").exists());

        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(crate::journal_mark::CANCEL_LINE));

        let hex = spl_core::ca::sha256_hex(peer.client_der());
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_mark_mismatch_retires_the_new_cert() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred = peer.credential();

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let mut out = Vec::new();
        let mut err = Vec::new();

        let status = dispatch_setup_with_pairer_for_test(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("wrong words"),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            &mut out,
            &mut err,
        )
        .await;

        assert_eq!(status, 1);
        assert!(!config.config_dir.join("credentials.json").exists());

        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(crate::journal_mark::NOT_PAIRED));
        assert!(out_str.contains(crate::journal_mark::MARK_MISMATCH_LINE));

        let hex = spl_core::ca::sha256_hex(peer.client_der());
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_unverifiable_mark_retires_and_saves_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let mut cred = peer.credential();
        cred.instance_id = "invalid-jid".into();

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let mut out = Vec::new();
        let mut err = Vec::new();

        let status = dispatch_setup_with_pairer_for_test(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("bramble quokka"),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            &mut out,
            &mut err,
        )
        .await;

        assert_eq!(status, 1);
        assert!(!config.config_dir.join("credentials.json").exists());

        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("couldn't verify."));
        assert!(out_str.contains(crate::journal_mark::SETUP_UNVERIFIABLE_LINE));

        let hex = spl_core::ca::sha256_hex(peer.client_der());
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_walk_away_keeps_a_confirmed_pairing() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);

        let cred_x = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-x-pem");
        crate::private_link::persist_credential(&config.config_dir, &cred_x).unwrap();
        let id_x = crate::private_link::compute_pairing_id(&cred_x.client_cert_pem);
        crate::journal_mark::write_pairing_answer(&config.config_dir, &id_x).unwrap();

        let cred_bytes_before = fs::read(config.config_dir.join("credentials.json")).unwrap();
        let config_bytes_before = fs::read(config.config_dir.join("config.json")).unwrap();

        let peer_y = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred_y = peer_y.credential();

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let (_tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(_tty_peer);

        let mut out = Vec::new();
        let mut err = Vec::new();

        let res = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &DirectPairer(cred_y.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;

        let status = render_setup_result(res, &mut out, &mut err);
        assert_eq!(status, 1);
        assert_eq!(
            fs::read(config.config_dir.join("credentials.json")).unwrap(),
            cred_bytes_before
        );
        assert_eq!(
            fs::read(config.config_dir.join("config.json")).unwrap(),
            config_bytes_before
        );
        assert!(crate::journal_mark::is_pairing_confirmed(
            &config.config_dir,
            &id_x
        ));

        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(crate::journal_mark::CANCEL_LINE));

        let hex = spl_core::ca::sha256_hex(peer_y.client_der());
        let requests = peer_y.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );
        peer_y.shutdown().await;
    }

    #[tokio::test]
    async fn setup_walk_away_without_confirmation_saves_held() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        let cred_y = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-y-pem");
        let id_y = crate::private_link::compute_pairing_id(&cred_y.client_cert_pem);

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        let (_tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(_tty_peer);

        let mut out = Vec::new();
        let mut err = Vec::new();

        let res = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &DirectPairer(cred_y.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;

        let status = render_setup_result(res, &mut out, &mut err);
        assert_eq!(status, 5);
        assert!(config.config_dir.join("credentials.json").exists());
        let answer = crate::journal_mark::read_pairing_answer(&config.config_dir)
            .unwrap()
            .unwrap();
        assert_ne!(answer.confirmed, id_y);

        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(crate::journal_mark::HELD_BOTH_SENTENCES));
        assert!(out_str.contains(crate::journal_mark::RUN_LINE));
    }

    #[tokio::test]
    async fn setup_y_confirms_and_c_does_not_continue() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);

        struct DirectPairer(spl_transport::credential::Credential);
        impl crate::private_link::Pairer for DirectPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                let cred = self.0.clone();
                Box::pin(async move { Ok(cred) })
            }
        }

        // 'y' confirms
        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer.write_all(b"y\n").unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let res = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;
        let status = render_setup_result(res, &mut out, &mut err);
        assert_eq!(status, 0);
        assert_eq!(
            crate::journal_mark::read_pairing_answer(&config.config_dir)
                .unwrap()
                .unwrap()
                .confirmed,
            pairing_id
        );

        // 'c' on identified branch does not continue
        let _ = fs::remove_file(config.config_dir.join("credentials.json"));
        let _ = fs::remove_file(
            config
                .config_dir
                .join(crate::journal_mark::PAIRING_ANSWER_FILENAME),
        );
        let (mut tty_peer2, tty_child2) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer2.write_all(b"c\n").unwrap();
        drop(tty_peer2);
        let mut out2 = Vec::new();
        let mut err2 = Vec::new();
        let res2 = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &DirectPairer(cred.clone()),
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child2.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;
        let status2 = render_setup_result(res2, &mut out2, &mut err2);
        assert_eq!(status2, 5);
        let answer2 = crate::journal_mark::read_pairing_answer(&config.config_dir)
            .unwrap()
            .unwrap();
        assert_ne!(answer2.confirmed, pairing_id);
    }

    #[tokio::test]
    async fn repair_leaves_confirmed_pairing_byte_identical() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);

        let cred_x = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-x-pem");
        crate::private_link::persist_credential(&config.config_dir, &cred_x).unwrap();
        let id_x = crate::private_link::compute_pairing_id(&cred_x.client_cert_pem);
        crate::journal_mark::write_pairing_answer(&config.config_dir, &id_x).unwrap();

        let cred_bytes_initial = fs::read(config.config_dir.join("credentials.json")).unwrap();
        let config_bytes_initial = fs::read(config.config_dir.join("config.json")).unwrap();

        let peer_y = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred_y = peer_y.credential();

        struct CustomPairer {
            cred: spl_transport::credential::Credential,
            fail: bool,
        }
        impl crate::private_link::Pairer for CustomPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                if self.fail {
                    Box::pin(
                        async move { Err(crate::private_link::PrivateStateError::PairingFailed) },
                    )
                } else {
                    let cred = self.cred.clone();
                    Box::pin(async move { Ok(cred) })
                }
            }
        }

        // 1. no
        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer.write_all(b"no\n").unwrap();
        let _ = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &CustomPairer {
                cred: cred_y.clone(),
                fail: false,
            },
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;
        assert_eq!(
            fs::read(config.config_dir.join("credentials.json")).unwrap(),
            cred_bytes_initial
        );
        assert_eq!(
            fs::read(config.config_dir.join("config.json")).unwrap(),
            config_bytes_initial
        );

        // 2. cancel
        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut unparseable_cred = cred_y.clone();
        unparseable_cred.instance_id = "invalid-jid".into();
        tty_peer.write_all(b"cancel\n").unwrap();
        let _ = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &CustomPairer {
                cred: unparseable_cred,
                fail: false,
            },
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;
        assert_eq!(
            fs::read(config.config_dir.join("credentials.json")).unwrap(),
            cred_bytes_initial
        );
        assert_eq!(
            fs::read(config.config_dir.join("config.json")).unwrap(),
            config_bytes_initial
        );

        // 3. mark mismatch
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = dispatch_setup_with_pairer_for_test(
            &CustomPairer {
                cred: cred_y.clone(),
                fail: false,
            },
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("wrong words"),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(
            fs::read(config.config_dir.join("credentials.json")).unwrap(),
            cred_bytes_initial
        );
        assert_eq!(
            fs::read(config.config_dir.join("config.json")).unwrap(),
            config_bytes_initial
        );

        // 4. walk away
        let (_tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(_tty_peer);
        let _ = crate::private_link::setup_with_pairer_and_stream_with_fault(
            &CustomPairer {
                cred: cred_y.clone(),
                fail: false,
            },
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("desktop"),
            None,
            Some(tty_child.as_fd()),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            None,
            None,
        )
        .await;
        assert_eq!(
            fs::read(config.config_dir.join("credentials.json")).unwrap(),
            cred_bytes_initial
        );
        assert_eq!(
            fs::read(config.config_dir.join("config.json")).unwrap(),
            config_bytes_initial
        );

        // 5. pairer error
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = dispatch_setup_with_pairer_for_test(
            &CustomPairer {
                cred: cred_y.clone(),
                fail: true,
            },
            &config.config_dir,
            &config.state_dir(),
            "desktop",
            Some("bramble quokka"),
            std::io::Cursor::new(crate::private_link::DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(
            fs::read(config.config_dir.join("credentials.json")).unwrap(),
            cred_bytes_initial
        );
        assert_eq!(
            fs::read(config.config_dir.join("config.json")).unwrap(),
            config_bytes_initial
        );

        peer_y.shutdown().await;
    }

    #[test]
    fn status_prints_held_line_once_when_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        crate::private_link::persist_credential(&config.config_dir, &cred).unwrap();
        crate::journal_mark::write_pairing_answer(&config.config_dir, "").unwrap();

        let mut out = Vec::new();
        let status = cmd_status(paths(&temp), &StatusRunner(None), &mut out);
        assert_eq!(status, 0);
        let out_str = String::from_utf8(out).unwrap();
        assert_eq!(
            out_str
                .matches(crate::journal_mark::HELD_BOTH_SENTENCES)
                .count(),
            1
        );
        assert_eq!(out_str.matches(crate::journal_mark::RUN_LINE).count(), 1);

        // Absent answer file: those strings are absent
        let _ = fs::remove_file(
            config
                .config_dir
                .join(crate::journal_mark::PAIRING_ANSWER_FILENAME),
        );
        let mut out2 = Vec::new();
        let status2 = cmd_status(paths(&temp), &StatusRunner(None), &mut out2);
        assert_eq!(status2, 0);
        let out_str2 = String::from_utf8(out2).unwrap();
        assert_eq!(
            out_str2
                .matches(crate::journal_mark::HELD_BOTH_SENTENCES)
                .count(),
            0
        );
        assert_eq!(out_str2.matches(crate::journal_mark::RUN_LINE).count(), 0);
    }

    #[tokio::test]
    async fn setup_early_checks_without_reading_stdin_or_pairing() {
        use clap::Parser;

        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let config_bytes_before = fs::read(config.config_dir.join("config.json")).unwrap();

        struct CountingPairer {
            calls: Arc<AtomicUsize>,
        }
        impl crate::private_link::Pairer for CountingPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                crate::private_link::PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Err(crate::private_link::PrivateStateError::PairingFailed) })
            }
        }

        // 1. No --mark, no tty: exit 1, reads 0, pair calls 0, config unchanged, stdout SETUP_NO_TERMINAL
        {
            let reads = Arc::new(AtomicUsize::new(0));
            let pair_calls = Arc::new(AtomicUsize::new(0));
            let mut input = CountingInput {
                bytes: std::io::Cursor::new(
                    crate::private_link::DIRECT_PAIR_LINK_FOR_TEST
                        .as_bytes()
                        .to_vec(),
                ),
                reads: reads.clone(),
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            let pairer = CountingPairer {
                calls: pair_calls.clone(),
            };
            let status = dispatch_setup_with_pairer_for_test(
                &pairer,
                &config.config_dir,
                &config.state_dir(),
                "desktop",
                None,
                &mut input,
                &mut out,
                &mut err,
            )
            .await;
            assert_eq!(status, 1);
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            assert_eq!(pair_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                fs::read(config.config_dir.join("config.json")).unwrap(),
                config_bytes_before
            );
            let out_str = String::from_utf8(out).unwrap();
            assert_eq!(out_str.trim(), crate::journal_mark::SETUP_NO_TERMINAL);
        }

        // 2. --mark with 1 word, 3 words, and "": exit 2, reads 0, pair calls 0, stdout MARK_USAGE
        for bad_mark in ["word", "one two three", ""] {
            let reads = Arc::new(AtomicUsize::new(0));
            let pair_calls = Arc::new(AtomicUsize::new(0));
            let mut input = CountingInput {
                bytes: std::io::Cursor::new(
                    crate::private_link::DIRECT_PAIR_LINK_FOR_TEST
                        .as_bytes()
                        .to_vec(),
                ),
                reads: reads.clone(),
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            let pairer = CountingPairer {
                calls: pair_calls.clone(),
            };
            let status = dispatch_setup_with_pairer_for_test(
                &pairer,
                &config.config_dir,
                &config.state_dir(),
                "desktop",
                Some(bad_mark),
                &mut input,
                &mut out,
                &mut err,
            )
            .await;
            assert_eq!(status, 2);
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            assert_eq!(pair_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                fs::read(config.config_dir.join("config.json")).unwrap(),
                config_bytes_before
            );
            let out_str = String::from_utf8(out).unwrap();
            assert_eq!(out_str.trim(), crate::journal_mark::MARK_USAGE);
        }

        // 3. Repeated flag: clap returns ArgumentConflict with exit code 2
        let parse_result = Args::try_parse_from([
            "solstone-linux",
            "setup",
            "--mark",
            "bramble quokka",
            "--mark",
            "bramble quokka",
        ]);
        let err = parse_result.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn status_and_doctor_in_process_live_owner() {
        use crate::doctor::DoctorChecks;

        let temp = tempfile::tempdir().unwrap();
        let config = status_config(&temp);
        let paths = paths(&temp);

        let mut lock = crate::private_link::PrivateStateLock::acquire(&config.config_dir).unwrap();
        lock.mark_ready().unwrap();

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);

        let make_facts =
            |link_state: crate::private_link::LinkFactState| crate::sync_health::SyncFacts {
                link_epoch: Some(crate::sync_health::ProcessEpoch::for_test(1)),
                link: Some(link_state),
                ..Default::default()
            };

        // 1. Facts journal_mark_held: false, credential present, answer names a different id
        crate::private_link::persist_credential(&config.config_dir, &cred).unwrap();
        crate::journal_mark::write_pairing_answer(&config.config_dir, "other-pairing-id").unwrap();

        let base_link = crate::private_link::LinkFactState {
            pairing_required: false,
            journal_mark_held: false,
            private_state_invalid: false,
            config_sanitation_failed: false,
            listener_ready: true,
            carrier_proven: false,
            observer_registered: false,
            transport_unavailable: false,
            terminal_revocation: false,
            token_persistence_failure: false,
            journal_version_observed: false,
            dial_generation: 0,
            optional_dial: false,
            unknown_journals: vec![],
            paired_jid: None,
            unknown_spoken_marks: vec![],
            paired_spoken_mark: None,
        };
        crate::sync_health::save_facts(&config.state_dir(), &make_facts(base_link.clone()))
            .unwrap();

        let mut out1 = Vec::new();
        let status1 = cmd_status(paths.clone(), &StatusRunner(None), &mut out1);
        assert_eq!(status1, 0);
        let out_str1 = String::from_utf8(out1).unwrap();
        assert_eq!(
            out_str1
                .matches(crate::journal_mark::HELD_BOTH_SENTENCES)
                .count(),
            1
        );

        let mut doctor1 = crate::doctor::RealDoctor::with_paths(&StatusRunner(None), paths.clone());
        let doctor_res1 = doctor1.sync_health();
        assert!(
            doctor_res1
                .detail
                .contains(crate::journal_mark::HELD_BOTH_SENTENCES)
        );

        // 2. Facts journal_mark_held: true, answer names this credential's id
        crate::journal_mark::write_pairing_answer(&config.config_dir, &pairing_id).unwrap();
        let held_link = crate::private_link::LinkFactState {
            journal_mark_held: true,
            ..base_link.clone()
        };
        crate::sync_health::save_facts(&config.state_dir(), &make_facts(held_link)).unwrap();

        let mut out2 = Vec::new();
        let status2 = cmd_status(paths.clone(), &StatusRunner(None), &mut out2);
        assert_eq!(status2, 0);
        let out_str2 = String::from_utf8(out2).unwrap();
        assert!(!out_str2.contains(crate::journal_mark::HELD_BOTH_SENTENCES));

        let mut doctor2 = crate::doctor::RealDoctor::with_paths(&StatusRunner(None), paths.clone());
        let doctor_res2 = doctor2.sync_health();
        assert!(
            !doctor_res2
                .detail
                .contains(crate::journal_mark::HELD_BOTH_SENTENCES)
        );

        // 3. Facts journal_mark_held: true, credential bytes are not a credential
        fs::write(config.config_dir.join("credentials.json"), b"invalid json").unwrap();
        let mut out3 = Vec::new();
        let status3 = cmd_status(paths.clone(), &StatusRunner(None), &mut out3);
        assert_eq!(status3, 0);
        let out_str3 = String::from_utf8(out3).unwrap();
        assert!(out_str3.contains(crate::journal_mark::HELD_BOTH_SENTENCES));

        let mut doctor3 = crate::doctor::RealDoctor::with_paths(&StatusRunner(None), paths.clone());
        let doctor_res3 = doctor3.sync_health();
        assert!(
            doctor_res3
                .detail
                .contains(crate::journal_mark::HELD_BOTH_SENTENCES)
        );

        // 4. Facts private_state_invalid: true, carrier_proven: true, journal_version_observed: true, and disk held
        crate::private_link::persist_credential(&config.config_dir, &cred).unwrap();
        crate::journal_mark::write_pairing_answer(&config.config_dir, "other-pairing-id").unwrap();

        let unsafe_link = crate::private_link::LinkFactState {
            private_state_invalid: true,
            carrier_proven: true,
            journal_version_observed: true,
            journal_mark_held: false,
            ..base_link
        };
        crate::sync_health::save_facts(&config.state_dir(), &make_facts(unsafe_link)).unwrap();

        let mut out4 = Vec::new();
        let status4 = cmd_status(paths.clone(), &StatusRunner(None), &mut out4);
        assert_eq!(status4, 0);
        let out_str4 = String::from_utf8(out4).unwrap();
        assert!(out_str4.contains(
            "Sync: pairing unsafe; repair this device's pairing and restart the solstone app"
        ));
        assert!(!out_str4.contains(crate::journal_mark::HELD_FIRST_SENTENCE));

        let mut doctor4 = crate::doctor::RealDoctor::with_paths(&StatusRunner(None), paths.clone());
        let doctor_res4 = doctor4.sync_health();
        assert!(doctor_res4.detail.contains("pairing unsafe"));
        assert!(
            !doctor_res4
                .detail
                .contains(crate::journal_mark::HELD_FIRST_SENTENCE)
        );

        let re_read = crate::sync_health::load_facts_with_liveness(
            &config.state_dir(),
            crate::private_link::PrivateStateLockLiveness::LiveOwner,
        );
        let re_read_link = re_read.link.unwrap();
        assert!(re_read_link.carrier_proven);
        assert!(re_read_link.journal_version_observed);
    }
}
