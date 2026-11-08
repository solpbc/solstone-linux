// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::json;
use solstone_terminal::{
    CaptureSettings, ClosedSegment, Diagnostic, EngineConfig, EngineError, LegacyError,
    SyncActivity, TmuxHealth, read_lenient_legacy_segment_interval, start as start_engine,
};

const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (source ",
    env!("SOLSTONE_TERMINAL_SOURCE_LABEL"),
    ")"
);

#[derive(Debug, Parser)]
#[command(name = "solstone-terminal", version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Run {
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        tmux: PathBuf,
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    Legacy {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        legacy_config: Option<PathBuf>,
    },
}

fn write_stderr_line(msg: &str) {
    let _ = writeln!(std::io::stderr(), "solstone-terminal: {msg}");
}

fn write_stdout_json(val: &serde_json::Value) -> bool {
    let mut stdout = std::io::stdout().lock();
    if let Ok(bytes) = serde_json::to_vec(val)
        && (stdout.write_all(&bytes).is_err()
            || stdout.write_all(b"\n").is_err()
            || stdout.flush().is_err())
    {
        return false;
    }
    true
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run {
            spool,
            tmux,
            settings,
        } => {
            cmd_run(spool, tmux, settings).await;
        }
        Commands::Legacy {
            from,
            legacy_config,
        } => {
            cmd_legacy(from, legacy_config).await;
        }
    }
}

enum StdinLine {
    Text(String),
    NotUtf8,
}

/// How the run loop ended.
enum RunEnd {
    /// Stop was asked for: stdin EOF, a signal or `{"cmd":"stop"}`.
    Stop,
    /// The engine ended on its own.
    Engine(Result<(), EngineError>),
}

async fn cmd_run(spool: PathBuf, tmux: PathBuf, settings: Option<PathBuf>) {
    let capture_settings = match &settings {
        Some(path) => match CaptureSettings::load_file(path) {
            Ok(s) => s,
            Err(err) => {
                write_stderr_line(&format!("{}: {err}", path.display()));
                std::process::exit(64);
            }
        },
        None => CaptureSettings::default(),
    };

    // Installed before anything is written, so a signal after `ready` always stops.
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        Ok(signal) => signal,
        Err(err) => {
            write_stderr_line(&format!("could not install the SIGTERM handler: {err}"));
            std::process::exit(1);
        }
    };
    let mut sigint = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    {
        Ok(signal) => signal,
        Err(err) => {
            write_stderr_line(&format!("could not install the SIGINT handler: {err}"));
            std::process::exit(1);
        }
    };

    let lock_path = spool.join(".solstone-terminal.lock");
    let spool_for_layout = spool.clone();
    let layout = Arc::new(move |day: &str| spool_for_layout.join(day));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

    let event_tx_health = event_tx.clone();
    let health_sink = Arc::new(move |health: TmuxHealth| {
        let status = match health {
            TmuxHealth::Found => "found",
            TmuxHealth::Missing => "missing",
        };
        let _ = event_tx_health.send(json!({
            "event": "health",
            "tmux": status,
        }));
    });

    let event_tx_segment = event_tx.clone();
    let segment_sink = Arc::new(move |segment: ClosedSegment| {
        let _ = event_tx_segment.send(json!({
            "event": "segment",
            "day": segment.day,
            "segment": segment.segment,
            "dir": segment.dir,
        }));
    });

    let diag_sink = Arc::new(|diagnostic: Diagnostic| write_stderr_line(&diagnostic.to_string()));

    let config = EngineConfig {
        root: spool.clone(),
        layout,
        lock_path: lock_path.clone(),
        tmux_path: tmux.clone(),
        settings: capture_settings,
        closed_segments: segment_sink,
        diagnostics: diag_sink,
        health: health_sink,
    };

    let mut handle = match start_engine(config).await {
        Ok(handle) => handle,
        Err(EngineError::AlreadyRunning(path)) => {
            write_stderr_line(&format!("{}: still running", path.display()));
            std::process::exit(75);
        }
        Err(EngineError::ExecutableMustBeAbsolute(path)) => {
            write_stderr_line(&format!("{}: executable must be absolute", path.display()));
            std::process::exit(1);
        }
        Err(err) => {
            write_stderr_line(&format!("{}: {err}", spool.display()));
            std::process::exit(1);
        }
    };

    write_stdout_json(&json!({
        "event": "ready",
        "version": env!("CARGO_PKG_VERSION"),
    }));

    // Start-up events, in the order the engine reported them.
    while let Ok(event) = event_rx.try_recv() {
        write_stdout_json(&event);
    }

    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<StdinLine>(32);
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut stdin = std::io::stdin().lock();
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match stdin.read_until(b'\n', &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = match String::from_utf8(buffer.clone()) {
                Ok(text) => StdinLine::Text(text),
                Err(_) => StdinLine::NotUtf8,
            };
            if stdin_tx.blocking_send(line).is_err() {
                break;
            }
        }
    });

    let end = loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                write_stdout_json(&event);
            }
            result = handle.wait() => {
                break RunEnd::Engine(result);
            }
            line = stdin_rx.recv() => {
                match line {
                    Some(StdinLine::Text(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match parse_command(trimmed) {
                            Some(Command::Sync(activity)) => {
                                let _ = handle.set_sync_activity(activity).await;
                            }
                            Some(Command::Stop) => break RunEnd::Stop,
                            None => write_stderr_line("malformed command"),
                        }
                    }
                    Some(StdinLine::NotUtf8) => write_stderr_line("malformed command"),
                    None => break RunEnd::Stop,
                }
            }
            _ = sigterm.recv() => break RunEnd::Stop,
            _ = sigint.recv() => break RunEnd::Stop,
        }
    };

    let result = match end {
        RunEnd::Stop => handle.stop().await,
        RunEnd::Engine(result) => result,
    };

    // The last segment and anything else the engine reported on its way out.
    while let Ok(event) = event_rx.try_recv() {
        write_stdout_json(&event);
    }

    match result {
        Ok(()) => {
            write_stdout_json(&json!({
                "event": "stopped",
            }));
            std::process::exit(0);
        }
        Err(err) => {
            write_stderr_line(&format!("{}: {err}", spool.display()));
            std::process::exit(1);
        }
    }
}

enum Command {
    Sync(SyncActivity),
    Stop,
}

fn parse_command(line: &str) -> Option<Command> {
    let command = serde_json::from_str::<serde_json::Value>(line).ok()?;
    match command.get("cmd").and_then(|value| value.as_str())? {
        "sync" => match command.get("state").and_then(|value| value.as_str())? {
            "working" => Some(Command::Sync(SyncActivity::Working)),
            "idle" => Some(Command::Sync(SyncActivity::Idle)),
            _ => None,
        },
        "stop" => Some(Command::Stop),
        _ => None,
    }
}

async fn cmd_legacy(from: PathBuf, legacy_config: Option<PathBuf>) {
    let config_path = legacy_config.unwrap_or_else(|| from.join("config.json"));
    let segment_interval = read_lenient_legacy_segment_interval(&config_path);

    let guard = match solstone_terminal::open_legacy(&from) {
        Ok(guard) => guard,
        Err(LegacyError::StillRunning(path)) => {
            write_stderr_line(&format!("{}: still running", path.display()));
            std::process::exit(75);
        }
        Err(err) => {
            write_stderr_line(&format!("{}: {err}", from.display()));
            std::process::exit(1);
        }
    };

    if guard.lock().is_none() {
        write_stderr_line(&format!("{}: nothing here", from.display()));
        write_stdout_json(&json!({
            "event": "done",
            "segments": 0,
            "held": 0,
            "problems": 0,
        }));
        std::process::exit(0);
    }

    let recovery = match guard.recover(segment_interval) {
        Ok(recovery) => recovery,
        Err(err) => {
            write_stderr_line(&format!("{}: {err}", from.display()));
            std::process::exit(1);
        }
    };

    let listing = match guard.segments() {
        Ok(listing) => listing,
        Err(err) => {
            write_stderr_line(&format!("{}: {err}", from.display()));
            std::process::exit(1);
        }
    };
    drop(guard);

    // Recovery and listing can see the same entry (a retained `.incomplete`, a
    // quarantined `.failed`, a symlinked day). Each path is reported once, with
    // what recovery said about it.
    let mut held = Vec::new();
    let mut held_paths = HashSet::new();
    for entry in recovery.held.into_iter().chain(listing.held) {
        if held_paths.insert(entry.dir.clone()) {
            held.push(entry);
        }
    }
    let mut problems = Vec::new();
    let mut problem_paths = HashSet::new();
    for problem in recovery.problems.into_iter().chain(listing.problems) {
        if !held_paths.contains(&problem.path) && problem_paths.insert(problem.path.clone()) {
            problems.push(problem);
        }
    }

    for seg in &listing.segments {
        write_stdout_json(&json!({
            "event": "segment",
            "day": seg.day,
            "segment": seg.segment,
            "dir": seg.dir,
            "files": seg.files,
            "zone": seg.zone,
        }));
    }

    for entry in &held {
        write_stdout_json(&json!({
            "event": "held",
            "day": entry.day,
            "segment": entry.segment,
            "dir": entry.dir,
            "reason": entry.reason,
        }));
    }

    for problem in &problems {
        write_stdout_json(&json!({
            "event": "problem",
            "path": problem.path,
            "reason": problem.reason,
        }));
    }

    write_stdout_json(&json!({
        "event": "done",
        "segments": listing.segments.len(),
        "held": held.len(),
        "problems": problems.len(),
    }));
    std::process::exit(0);
}
