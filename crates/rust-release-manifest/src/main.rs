// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use clap::{Parser, Subcommand};
use rust_release_manifest::{
    Evidence, MANIFEST_OK_MESSAGE, RELEASE_DIR_OK_MESSAGE, classify_release_dir,
    verify_manifest_mode, write_rendered,
};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Validate {
        #[arg(long, conflicts_with = "release_dir")]
        manifest: Option<PathBuf>,
        #[arg(long, conflicts_with = "manifest")]
        release_dir: Option<PathBuf>,
    },
    Render {
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        release_dir: PathBuf,
    },
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Validate {
            manifest: Some(path),
            release_dir: None,
        } => {
            verify_manifest_mode(&path)?;
            println!("{MANIFEST_OK_MESSAGE}");
        }
        Command::Validate {
            manifest: None,
            release_dir: Some(path),
        } => {
            classify_release_dir(&path)?;
            println!("{RELEASE_DIR_OK_MESSAGE}");
        }
        Command::Validate { .. } => return Err("exactly one validation path is required".into()),
        Command::Render {
            evidence,
            release_dir,
        } => {
            let evidence: Evidence = serde_json::from_slice(&fs::read(evidence)?)?;
            write_rendered(evidence, &release_dir)?;
            println!("Rendered deterministic release manifest and SHA256SUMS.");
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
