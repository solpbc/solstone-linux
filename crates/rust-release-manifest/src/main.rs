// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use clap::{Args, Parser, Subcommand};
use rust_release_manifest::{
    AuditRequest, Lane, LaneEmitRequest, MANIFEST_OK_MESSAGE, ProcessEnvironment,
    ProofHandoffInput, RELEASE_DIR_OK_MESSAGE, RepoRoot, SPL_PIN_OK_MESSAGE, audit_packages,
    classify_release_dir, create_candidate, emit_lane_handoff, emit_proof_handoff, prove_candidate,
    publish_transparency, recover_candidate, resign_transparency_pointer, run_audit,
    sign_release_manifest, validate_spl_pin, verify_manifest_mode, verify_release_signature,
};
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    AuditPackages {
        #[arg(long)]
        tar: PathBuf,
        #[arg(long)]
        deb: PathBuf,
        #[arg(long)]
        rpm: PathBuf,
        #[arg(long)]
        expected_executable_sha256: String,
    },
    Audit {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        pubkey: PathBuf,
        #[arg(long)]
        locator: String,
    },
    Validate {
        #[arg(long, conflicts_with = "release_dir")]
        manifest: Option<PathBuf>,
        #[arg(long, conflicts_with = "manifest")]
        release_dir: Option<PathBuf>,
    },
    ValidateSplPin,
    SignReleaseManifest {
        #[arg(long)]
        release_dir: PathBuf,
        #[arg(long)]
        secret_key: PathBuf,
        #[arg(long)]
        passphrase_file: PathBuf,
    },
    VerifyReleaseSignature {
        #[arg(long)]
        release_dir: PathBuf,
    },
    #[command(hide = true)]
    LaneHandoff(Box<LaneHandoffArgs>),
    #[command(hide = true)]
    ProofHandoff(Box<ProofHandoffArgs>),
    Candidate {
        #[command(subcommand)]
        command: CandidateCommand,
    },
    Transparency {
        #[command(subcommand)]
        command: TransparencyCommand,
    },
}

#[derive(Subcommand)]
enum TransparencyCommand {
    Publish {
        #[arg(long)]
        release_dir: PathBuf,
    },
    ResignPointer,
}

#[derive(Subcommand)]
enum CandidateCommand {
    Create {
        #[arg(long)]
        expected_release_commit: String,
        #[arg(long)]
        advisory_descriptor: PathBuf,
    },
    Prove {
        #[arg(long)]
        version: String,
        #[arg(long)]
        advisory_descriptor: PathBuf,
    },
    Recover {
        #[arg(long)]
        version: String,
    },
}

#[derive(Args)]
struct LaneHandoffArgs {
    #[arg(long)]
    lane: String,
    #[arg(long)]
    invocation_id: String,
    #[arg(long)]
    source_commit: String,
    #[arg(long)]
    source_archive_sha256: String,
    #[arg(long)]
    cargo_lock_sha256: String,
    #[arg(long)]
    version: String,
    #[arg(long)]
    target: String,
    #[arg(long)]
    profile: String,
    #[arg(long)]
    feature: Vec<String>,
    #[arg(long)]
    image_digest: String,
    #[arg(long)]
    baseline_executable: PathBuf,
    #[arg(long)]
    artifact: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Args)]
struct ProofHandoffArgs {
    #[arg(long)]
    platform: String,
    #[arg(long)]
    artifact: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    candidate_digest: String,
    #[arg(long)]
    ledger_sha256: String,
    #[arg(long)]
    source_commit: String,
    #[arg(long)]
    cargo_lock_sha256: String,
    #[arg(long)]
    proof_image_digest: String,
    #[arg(long)]
    version: String,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let command = Cli::parse().command;
    match command {
        Command::AuditPackages {
            tar,
            deb,
            rpm,
            expected_executable_sha256,
        } => audit_packages(&tar, &deb, &rpm, &expected_executable_sha256)?,
        Command::Audit {
            bundle,
            receipt,
            pubkey,
            locator,
        } => {
            let status = run_audit(&AuditRequest {
                bundle: &bundle,
                receipt: &receipt,
                public_key: &pubkey,
                locator: &locator,
            })?;
            println!("{}", serde_json::to_string(&status)?);
        }
        Command::Validate {
            manifest: Some(path),
            release_dir: None,
        } => {
            let root = RepoRoot::resolve()?;
            verify_manifest_mode(&root, &path)?;
            println!("{MANIFEST_OK_MESSAGE}");
        }
        Command::Validate {
            manifest: None,
            release_dir: Some(path),
        } => {
            let root = RepoRoot::resolve()?;
            classify_release_dir(&root, &path)?;
            println!("{RELEASE_DIR_OK_MESSAGE}");
        }
        Command::Validate { .. } => return Err("exactly one validation path is required".into()),
        Command::ValidateSplPin => {
            let root = RepoRoot::resolve()?;
            validate_spl_pin(&root)?;
            println!("{SPL_PIN_OK_MESSAGE}");
        }
        Command::SignReleaseManifest {
            release_dir,
            secret_key,
            passphrase_file,
        } => {
            let root = RepoRoot::resolve()?;
            let signature =
                sign_release_manifest(&root, &release_dir, &secret_key, &passphrase_file)?;
            println!("{}", signature.display());
        }
        Command::VerifyReleaseSignature { release_dir } => {
            let root = RepoRoot::resolve()?;
            verify_release_signature(&root, &release_dir)?;
            println!("Release manifest signature and artifacts verified.");
        }
        Command::LaneHandoff(args) => {
            let LaneHandoffArgs {
                lane,
                invocation_id,
                source_commit,
                source_archive_sha256,
                cargo_lock_sha256,
                version,
                target,
                profile,
                feature,
                image_digest,
                baseline_executable,
                artifact,
                output,
            } = *args;
            let lane = match lane.as_str() {
                "deb" => Lane::Deb,
                "rpm" => Lane::Rpm,
                _ => return Err("lane mismatch: expected deb or rpm, actual invalid".into()),
            };
            emit_lane_handoff(&LaneEmitRequest {
                lane,
                invocation_id: &invocation_id,
                source_commit: &source_commit,
                source_archive_sha256: &source_archive_sha256,
                expected_cargo_lock_sha256: &cargo_lock_sha256,
                version: &version,
                target: &target,
                profile: &profile,
                features: feature,
                image_digest: &image_digest,
                baseline_executable: &baseline_executable,
                artifacts: artifact,
                output: &output,
            })?;
        }
        Command::ProofHandoff(args) => {
            emit_proof_handoff(&ProofHandoffInput {
                platform: &args.platform,
                artifact: &args.artifact,
                output: &args.output,
                candidate_digest: &args.candidate_digest,
                ledger_sha256: &args.ledger_sha256,
                source_commit: &args.source_commit,
                cargo_lock_sha256: &args.cargo_lock_sha256,
                proof_image_digest: &args.proof_image_digest,
                version: &args.version,
            })?;
        }
        Command::Candidate { command } => {
            let root = RepoRoot::resolve()?;
            let processes = ProcessEnvironment::default();
            let status = match command {
                CandidateCommand::Create {
                    expected_release_commit,
                    advisory_descriptor,
                } => create_candidate(
                    &root,
                    &expected_release_commit,
                    &advisory_descriptor,
                    &processes,
                )?,
                CandidateCommand::Prove {
                    version,
                    advisory_descriptor,
                } => prove_candidate(&root, &version, &advisory_descriptor, &processes)?,
                CandidateCommand::Recover { version } => {
                    println!("{}", recover_candidate(&root, &version)?);
                    return Ok(());
                }
            };
            println!("{}", serde_json::to_string(&status)?);
        }
        Command::Transparency { command } => match command {
            TransparencyCommand::Publish { release_dir } => publish_transparency(&release_dir)?,
            TransparencyCommand::ResignPointer => resign_transparency_pointer()?,
        },
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
