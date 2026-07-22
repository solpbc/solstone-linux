// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub(crate) const PROOF_RUNNER_ENTRY_MARKER: &str = "solstone-proof-runner-entry-v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateLedger {
    pub schema_version: u64,
    pub product: String,
    pub version: String,
    pub source: LedgerSource,
    pub validator: LedgerValidator,
    pub target: LedgerTarget,
    pub policy: LedgerPolicy,
    pub advisory_cohort: LedgerAdvisory,
    pub images: LedgerImages,
    pub tools: BTreeMap<String, String>,
    pub payload: Vec<Artifact>,
    pub package_members: Vec<PackageMemberEvidence>,
    pub baseline_executable: ExecutableIdentity,
    pub expected_proof_ids: Vec<String>,
    pub candidate_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutableIdentity {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSource {
    pub commit: String,
    pub archive_sha256: String,
    pub cargo_lock_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerValidator {
    pub version: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerTarget {
    pub triple: String,
    pub profile: String,
    pub features: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerPolicy {
    pub cargo_deny_version: String,
    pub deterministic_gate: String,
    pub licenses_bans_sources: String,
    pub advisories: String,
    pub checked_at: String,
    pub active_exceptions: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerAdvisory {
    pub source_id: String,
    pub commit: String,
    pub archive_sha256: String,
    pub acquired_at: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerImages {
    pub engine: String,
    pub engine_version: String,
    pub ubuntu_image_id: String,
    pub fedora_image_id: String,
}

pub struct LedgerInput<'a> {
    pub root: &'a RepoRoot,
    pub context: &'a ImmutableContext,
    pub version: &'a str,
    pub payload_root: &'a Path,
    pub package_members: Vec<PackageMemberEvidence>,
    pub baseline_executable: ExecutableIdentity,
    pub cohort: &'a AdvisoryCohort,
    pub ubuntu: &'a ImageIdentity,
    pub fedora: &'a ImageIdentity,
    pub engine: ContainerEngine,
    pub engine_identity: String,
    pub tools: BTreeMap<String, String>,
}

pub fn construct_ledger(input: LedgerInput<'_>) -> Result<CandidateLedger> {
    let mut payload = fs::read_dir(input.payload_root)
        .map_err(display_error)?
        .map(|entry| artifact(&entry.map_err(display_error)?.path()))
        .collect::<Result<Vec<_>>>()?;
    payload.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    if payload.len() != 5 {
        return Err(Error::new(format!(
            "ledger payload mismatch: expected 5 files, actual {}",
            payload.len()
        )));
    }
    let candidate = candidate_digest(&payload)?;
    let active_exceptions = ordered_exceptions(input.root)?;
    let mut members = input.package_members;
    members.sort_by(|a, b| a.package_file.as_bytes().cmp(b.package_file.as_bytes()));
    let ledger = CandidateLedger {
        schema_version: 1,
        product: PRODUCT.into(),
        version: input.version.into(),
        source: LedgerSource {
            commit: input.context.commit.clone(),
            archive_sha256: input.context.archive_sha256.clone(),
            cargo_lock_sha256: input.context.cargo_lock_sha256.clone(),
        },
        validator: LedgerValidator {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        target: LedgerTarget {
            triple: TARGET_TRIPLE.into(),
            profile: "release".into(),
            features: Vec::new(),
        },
        policy: LedgerPolicy {
            cargo_deny_version: input.cohort.cargo_deny_version.clone(),
            deterministic_gate: input.cohort.deterministic_gate.clone(),
            licenses_bans_sources: input.cohort.licenses_bans_sources.clone(),
            advisories: input.cohort.advisories.clone(),
            checked_at: input.cohort.checked_at.clone(),
            active_exceptions,
        },
        advisory_cohort: LedgerAdvisory {
            source_id: input.cohort.source_id.clone(),
            commit: input.cohort.commit.clone(),
            archive_sha256: input.cohort.archive_sha256.clone(),
            acquired_at: input.cohort.acquired_at.clone(),
        },
        images: LedgerImages {
            engine: match input.engine {
                ContainerEngine::Podman => "podman",
                ContainerEngine::Docker => "docker",
            }
            .into(),
            engine_version: input.engine_identity,
            ubuntu_image_id: input.ubuntu.digest.clone(),
            fedora_image_id: input.fedora.digest.clone(),
        },
        tools: input.tools,
        payload,
        package_members: members,
        baseline_executable: input.baseline_executable,
        expected_proof_ids: PROOF_SPECS.iter().map(|spec| spec.id.to_owned()).collect(),
        candidate_digest: candidate,
    };
    validate_ledger(input.root, input.payload_root, &ledger)?;
    Ok(ledger)
}

pub fn ledger_bytes(
    root: &RepoRoot,
    payload_root: &Path,
    ledger: &CandidateLedger,
) -> Result<Vec<u8>> {
    // Schema and privacy validation precede serialization, so canonicalization
    // cannot discover a ledger-shape failure at the promotion boundary.
    validate_ledger(root, payload_root, ledger)?;
    canonical_json(&serde_json::to_value(ledger).map_err(display_error)?)
}

pub fn validate_ledger(
    root: &RepoRoot,
    payload_root: &Path,
    ledger: &CandidateLedger,
) -> Result<()> {
    verify_candidate_schemas()?;
    let value = serde_json::to_value(ledger).map_err(display_error)?;
    let schema: Value = serde_json::from_slice(ledger_schema_bytes()).map_err(display_error)?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(display_error)?;
    if let Err(error) = validator.validate(&value) {
        return Err(Error::new(format!("ledger schema mismatch: {error}")));
    }
    validate_version(&ledger.version)?;
    validate_timestamp("policy checked_at", &ledger.policy.checked_at)?;
    validate_timestamp("advisory acquired_at", &ledger.advisory_cohort.acquired_at)?;
    validate_evidence_text("ledger advisory source", &ledger.advisory_cohort.source_id)?;
    validate_native_tools(root, &ledger.tools)?;
    if ledger.validator.version != env!("CARGO_PKG_VERSION") {
        return Err(Error::new(format!(
            "ledger validator mismatch: expected {}, actual {}",
            env!("CARGO_PKG_VERSION"),
            ledger.validator.version
        )));
    }
    let ubuntu = ledger
        .images
        .ubuntu_image_id
        .strip_prefix("sha256:")
        .unwrap_or_default();
    let fedora = ledger
        .images
        .fedora_image_id
        .strip_prefix("sha256:")
        .unwrap_or_default();
    let expected_engine = if ledger.images.engine_version.starts_with("podman version ") {
        "podman"
    } else if ledger.images.engine_version.starts_with("docker ") {
        "docker"
    } else {
        ""
    };
    if ledger.tools.get("ubuntu_image_digest").map(String::as_str) != Some(ubuntu)
        || ledger.tools.get("fedora_image_digest").map(String::as_str) != Some(fedora)
        || ledger.tools.get("container_engine").map(String::as_str)
            != Some(ledger.images.engine_version.as_str())
        || ledger.images.engine != expected_engine
    {
        return Err(Error::new(
            "ledger image identity mismatch: expected tool evidence, actual different",
        ));
    }
    let mut actual = ledger
        .payload
        .iter()
        .map(|item| artifact(&payload_root.join(&item.path)))
        .collect::<Result<Vec<_>>>()?;
    actual.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    if actual != ledger.payload || candidate_digest(&actual)? != ledger.candidate_digest {
        return Err(Error::new(
            "ledger payload digest mismatch: expected promoted bytes, actual different",
        ));
    }
    let expected_proofs = PROOF_SPECS.iter().map(|spec| spec.id).collect::<Vec<_>>();
    if ledger.package_members.len() != PROOF_SPECS.len()
        || ledger.expected_proof_ids != expected_proofs
    {
        return Err(Error::new(
            "ledger evidence inventory mismatch: expected three package members and three proof IDs, actual different",
        ));
    }
    let mut members = package_members(payload_root, &ledger.version)?;
    members.sort_by(|a, b| a.package_file.as_bytes().cmp(b.package_file.as_bytes()));
    if members != ledger.package_members {
        return Err(Error::new(
            "ledger package member mismatch: expected package bytes, actual different",
        ));
    }
    validate_shared_executable(ledger)?;
    Ok(())
}

fn validate_shared_executable(ledger: &CandidateLedger) -> Result<()> {
    let expected = [
        (
            "tar",
            "/bin/solstone-linux",
            format!("solstone-linux-{}-linux-x86_64.tar.gz", ledger.version),
        ),
        (
            "deb",
            "/usr/bin/solstone-linux",
            format!("solstone-linux_{}-1_amd64.deb", ledger.version),
        ),
        (
            "rpm",
            "/usr/bin/solstone-linux",
            format!("solstone-linux-{}-1.x86_64.rpm", ledger.version),
        ),
    ];
    let mut identities = Vec::new();
    for (format, installed_path, package_file) in expected {
        let matches = ledger
            .package_members
            .iter()
            .filter(|member| member.format == format)
            .collect::<Vec<_>>();
        if matches.len() != 1
            || matches[0].installed_path != installed_path
            || matches[0].package_file != package_file
            || matches[0].mode != 0o755
        {
            return Err(Error::new(
                "candidate executable inventory mismatch: expected documented tar, deb, and rpm members, actual different",
            ));
        }
        identities.push((matches[0].sha256.as_str(), matches[0].bytes));
    }
    if identities[1..]
        .iter()
        .any(|identity| *identity != identities[0])
    {
        return Err(Error::new(
            "candidate executable identity mismatch: expected one measured baseline across tar, deb, and rpm, actual divergent package members",
        ));
    }
    let actual = identities[0];
    if actual.0 != ledger.baseline_executable.sha256 || actual.1 != ledger.baseline_executable.bytes
    {
        return Err(Error::new(format!(
            "candidate executable baseline mismatch: expected measured baseline {}/{}, actual package identity {}/{}",
            ledger.baseline_executable.sha256, ledger.baseline_executable.bytes, actual.0, actual.1
        )));
    }
    Ok(())
}

pub fn read_ledger(root: &RepoRoot, version: &str) -> Result<(CandidateLedger, Vec<u8>)> {
    let version = VersionComponent::new(version)?;
    let boundary = ReservedReleaseBoundary::new(root);
    let path = boundary
        .resolve_for_read(
            ReservedPath::EvidenceLedger(version),
            ExpectedLeaf::RegularFile,
        )?
        .absolute;
    let bytes = fs::read(&path).map_err(display_error)?;
    let ledger: CandidateLedger = serde_json::from_slice(&bytes).map_err(display_error)?;
    if canonical_json(&serde_json::to_value(&ledger).map_err(display_error)?)? != bytes {
        return Err(Error::new("ledger canonicalization mismatch"));
    }
    let payload = boundary
        .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)?
        .absolute;
    validate_ledger(root, &payload, &ledger)?;
    Ok((ledger, bytes))
}

pub fn atomic_write_0644(path: &Path, bytes: &[u8]) -> Result<FileIdentity> {
    atomic_write_0644_with_parent_sync(path, bytes, |parent| {
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(display_error)
    })
}

pub fn atomic_write_0755(path: &Path, bytes: &[u8]) -> Result<FileIdentity> {
    atomic_write_with_mode_and_post_rename(
        path,
        bytes,
        0o755,
        |_, _| Ok(()),
        |parent| {
            File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(display_error)
        },
    )
}

pub(crate) fn atomic_write_0644_with_parent_sync(
    path: &Path,
    bytes: &[u8],
    parent_sync: impl FnOnce(&Path) -> Result<()>,
) -> Result<FileIdentity> {
    atomic_write_0644_with_post_rename(path, bytes, |_, _| Ok(()), parent_sync)
}

pub(crate) fn atomic_write_0644_with_post_rename(
    path: &Path,
    bytes: &[u8],
    post_rename: impl FnOnce(&Path, FileIdentity) -> Result<()>,
    parent_sync: impl FnOnce(&Path) -> Result<()>,
) -> Result<FileIdentity> {
    atomic_write_with_mode_and_post_rename(path, bytes, 0o644, post_rename, parent_sync)
}

fn atomic_write_with_mode_and_post_rename(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    post_rename: impl FnOnce(&Path, FileIdentity) -> Result<()>,
    parent_sync: impl FnOnce(&Path) -> Result<()>,
) -> Result<FileIdentity> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("atomic output parent mismatch"))?;
    require_directory(parent, "atomic output parent")?;
    if path.symlink_metadata().is_ok() {
        return Err(Error::new(
            "atomic output mismatch: expected absent, actual present",
        ));
    }
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(OsStr::to_str).unwrap_or("output"),
        transaction_id()?
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)
        .map_err(display_error)?;
    // Capture ownership from the open temp handle before publication. After rename,
    // every failure path already has the token needed to reclaim only this inode.
    let metadata = match file.metadata().map_err(display_error) {
        Ok(metadata) => metadata,
        Err(error) => {
            drop(file);
            finish_atomic_publish(&temp, Err(error))?;
            unreachable!("failed metadata lookup cannot publish")
        }
    };
    let owned = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    let publish = (|| {
        file.write_all(bytes).map_err(display_error)?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(display_error)?;
        file.sync_all().map_err(display_error)?;
        fs::rename(&temp, path).map_err(display_error)
    })();
    drop(file);
    finish_atomic_publish(&temp, publish)?;
    reclaim_after_publish_failure(path, owned, post_rename(path, owned))?;
    reclaim_after_publish_failure(path, owned, parent_sync(parent))?;
    Ok(owned)
}

fn reclaim_after_publish_failure(
    path: &Path,
    owned: FileIdentity,
    result: Result<()>,
) -> Result<()> {
    if let Err(error) = result {
        let same_file =
            fs::symlink_metadata(path).is_ok_and(|metadata| same_file_identity(&metadata, owned));
        if same_file {
            fs::remove_file(path).map_err(|cleanup| {
                Error::new(format!(
                    "{error}\nerror: atomic output cleanup mismatch: expected published file absent, actual residue: {cleanup}\nrepair: inspect only the failed atomic output entry"
                ))
            })?;
        }
        return Err(error);
    }
    Ok(())
}

pub(crate) fn finish_atomic_publish(temp: &Path, publish: Result<()>) -> Result<()> {
    if let Err(error) = publish {
        if temp.symlink_metadata().is_ok() {
            fs::remove_file(temp).map_err(|cleanup| {
                Error::new(format!(
                    "{error}\nerror: atomic output cleanup mismatch: expected owned temporary absent, actual residue: {cleanup}\nrepair: inspect only the failed transaction temporary entry"
                ))
            })?;
        }
        return Err(error);
    }
    Ok(())
}

pub struct FinalizeInput<'a> {
    pub root: &'a RepoRoot,
    pub staging: &'a StagingLayout,
    pub context: &'a ImmutableContext,
    pub version: &'a str,
    pub deb: &'a LaneEvidence,
    pub rpm: &'a LaneEvidence,
    pub baseline_executable: ExecutableIdentity,
    pub cohort: &'a AdvisoryCohort,
    pub images: &'a ResolvedImages,
    pub engine: ContainerEngine,
    pub engine_identity: String,
    pub processes: &'a ProcessEnvironment,
}

pub struct FinalizedCandidate {
    pub ledger: CandidateLedger,
    pub ledger_bytes: Vec<u8>,
    pub payload_root: PathBuf,
    pub evidence_root: PathBuf,
    payload_identity: FileIdentity,
    evidence_identity: FileIdentity,
}

impl FinalizedCandidate {
    pub(crate) fn new(
        ledger: CandidateLedger,
        ledger_bytes: Vec<u8>,
        payload_root: PathBuf,
        evidence_root: PathBuf,
    ) -> Result<Self> {
        let payload_identity = FileIdentity::from_metadata(
            &fs::symlink_metadata(&payload_root).map_err(display_error)?,
        );
        let evidence_identity = FileIdentity::from_metadata(
            &fs::symlink_metadata(&evidence_root).map_err(display_error)?,
        );
        Ok(Self {
            ledger,
            ledger_bytes,
            payload_root,
            evidence_root,
            payload_identity,
            evidence_identity,
        })
    }
}

pub fn finalize_candidate(input: FinalizeInput<'_>) -> Result<FinalizedCandidate> {
    stage_payload(&input)?;
    classify_release(input.root, &input.staging.payload, false)?;
    recheck_source(input.root, input.context, &input.staging.payload)?;
    recheck_images(
        input.processes,
        input.engine,
        [&input.images.build_ubuntu, &input.images.build_fedora],
    )?;
    recheck_advisory_cohort(input.cohort, input.processes, Utc::now())?;
    let boundary = ReservedReleaseBoundary::new(input.root);
    let payload_root = boundary
        .resolve_for_create(ReservedPath::Payload, ExpectedLeaf::Absent)?
        .absolute;
    fs::rename(&input.staging.payload, &payload_root).map_err(display_error)?;
    let payload_identity = boundary
        .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)?
        .identity
        .expect("present payload identity");
    let version = VersionComponent::new(input.version)?;
    match boundary.presence(ReservedPath::EvidenceParent, ExpectedLeaf::Directory)? {
        ReservedPathPresence::Present(path) => path.absolute,
        ReservedPathPresence::Absent(path) => {
            fs::create_dir(&path.absolute).map_err(display_error)?;
            path.absolute
        }
    };
    let evidence_root = boundary
        .presence(
            ReservedPath::EvidenceVersion(version.clone()),
            ExpectedLeaf::Directory,
        )?
        .resolved()
        .absolute;
    let mut evidence_identity = None;
    let result = (|| {
        classify_release(input.root, &payload_root, false)?;
        let members = package_members(&payload_root, input.version)?;
        let tools = assemble_manifest_native_tools(
            input.root,
            input.deb,
            input.rpm,
            input.engine_identity.clone(),
        )?;
        let ledger = construct_ledger(LedgerInput {
            root: input.root,
            context: input.context,
            version: input.version,
            payload_root: &payload_root,
            package_members: members,
            baseline_executable: input.baseline_executable.clone(),
            cohort: input.cohort,
            ubuntu: &input.images.build_ubuntu,
            fedora: &input.images.build_fedora,
            engine: input.engine,
            engine_identity: input.engine_identity,
            tools,
        })?;
        let bytes = ledger_bytes(input.root, &payload_root, &ledger)?;
        if boundary
            .resolve_for_create(
                ReservedPath::EvidenceVersion(version.clone()),
                ExpectedLeaf::Absent,
            )
            .is_ok()
        {
            fs::create_dir_all(&evidence_root).map_err(display_error)?;
        } else {
            boundary.resolve_for_read(
                ReservedPath::EvidenceVersion(version.clone()),
                ExpectedLeaf::Directory,
            )?;
        }
        evidence_identity = boundary
            .resolve_for_read(
                ReservedPath::EvidenceVersion(version.clone()),
                ExpectedLeaf::Directory,
            )?
            .identity;
        let staged_runner = input.staging.proof_runner.join("proof-runner");
        let runner_metadata = fs::symlink_metadata(&staged_runner).map_err(display_error)?;
        if runner_metadata.file_type().is_symlink() || !runner_metadata.is_file() {
            return Err(Error::new(
                "staged proof runner mismatch: expected no-follow regular file, actual symlink or non-regular file",
            ));
        }
        if runner_metadata.mode() & 0o7000 != 0 || runner_metadata.mode() & 0o111 == 0 {
            return Err(Error::new(
                "staged proof runner mode mismatch: expected executable, actual non-executable",
            ));
        }
        let runner_path = boundary
            .resolve_for_create(
                ReservedPath::ProofRunner(version.clone()),
                ExpectedLeaf::Absent,
            )?
            .absolute;
        atomic_write_0755(
            &runner_path,
            &fs::read(staged_runner).map_err(display_error)?,
        )?;
        resolve_proof_runner(&boundary, version.clone())?;
        let ledger_path = boundary
            .resolve_for_create(
                ReservedPath::EvidenceLedger(version.clone()),
                ExpectedLeaf::Absent,
            )?
            .absolute;
        atomic_write_0644(&ledger_path, &bytes)?;
        recheck_source(input.root, input.context, &payload_root)?;
        recheck_images(
            input.processes,
            input.engine,
            [&input.images.build_ubuntu, &input.images.build_fedora],
        )?;
        recheck_advisory_cohort(input.cohort, input.processes, Utc::now())?;
        FinalizedCandidate::new(ledger, bytes, payload_root.clone(), evidence_root.clone())
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut entries = vec![CleanupEntry {
                path: ReservedPath::Payload,
                expected_type: ExpectedLeaf::Directory,
                expected_identity: payload_identity,
            }];
            if let Some(identity) = evidence_identity {
                entries.push(CleanupEntry {
                    path: ReservedPath::EvidenceVersion(version),
                    expected_type: ExpectedLeaf::Directory,
                    expected_identity: identity,
                });
            }
            match CleanupPlan::new(entries)
                .and_then(|plan| plan.preflight(boundary))
                .and_then(ValidatedCleanupPlan::execute)
            {
                Ok(_) => Err(error),
                Err(cleanup) => Err(Error::new(format!("{error}\n{cleanup}"))),
            }
        }
    }
}

fn stage_payload(input: &FinalizeInput<'_>) -> Result<()> {
    require_directory(&input.staging.payload, "staged payload")?;
    if fs::read_dir(&input.staging.payload)
        .map_err(display_error)?
        .next()
        .is_some()
    {
        return Err(Error::new(
            "staged payload mismatch: expected empty, actual populated",
        ));
    }
    let tar = artifact_by_kind(&input.deb.artifacts, "tar")?;
    let deb = artifact_by_kind(&input.deb.artifacts, "deb")?;
    let rpm = artifact_by_kind(&input.rpm.artifacts, "rpm")?;
    for (source_root, item) in [
        (&input.staging.deb_lane, tar),
        (&input.staging.deb_lane, deb),
        (&input.staging.rpm_lane, rpm),
    ] {
        let destination = input.staging.payload.join(&item.path);
        fs::copy(source_root.join(&item.path), &destination).map_err(display_error)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
            .map_err(display_error)?;
    }
    let tools = assemble_manifest_native_tools(
        input.root,
        input.deb,
        input.rpm,
        input.engine_identity.clone(),
    )?;
    let evidence = Evidence {
        schema_version: SCHEMA_VERSION,
        product: PRODUCT.into(),
        version: input.version.into(),
        source_commit: input.context.commit.clone(),
        source_dirty: false,
        cargo_lock_sha256: input.context.cargo_lock_sha256.clone(),
        rust: RustEvidence {
            rustc_verbose: input.deb.rustc_verbose.clone(),
            cargo_version: input.deb.cargo.clone(),
        },
        target: TargetEvidence::Compiled {
            triple: input.deb.target.clone(),
            profile: input.deb.profile.clone(),
            features: input.deb.features.clone(),
        },
        native_tools: tools,
        dependency_policy: DependencyPolicy {
            cargo_deny_version: input.cohort.cargo_deny_version.clone(),
            deterministic_gate: input.cohort.deterministic_gate.clone(),
            advisory_checked_at: input.cohort.checked_at.clone(),
        },
        active_exceptions: ordered_exceptions(input.root)?,
    };
    let manifest = render_manifest(input.root, evidence, &input.staging.payload)?;
    let parsed: Manifest = serde_json::from_str(&manifest).map_err(display_error)?;
    let sums = render_sha256sums(&parsed.artifacts)?;
    atomic_write_0644(&input.staging.payload.join(CHECKSUM_NAME), sums.as_bytes())?;
    atomic_write_0644(
        &input.staging.payload.join(manifest_name(input.version)),
        manifest.as_bytes(),
    )?;
    for entry in fs::read_dir(&input.staging.payload).map_err(display_error)? {
        let path = entry.map_err(display_error)?.path();
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(display_error)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).map_err(display_error)?;
    }
    File::open(&input.staging.payload)
        .and_then(|file| file.sync_all())
        .map_err(display_error)
}

fn package_members(payload: &Path, version: &str) -> Result<Vec<PackageMemberEvidence>> {
    let entries = fs::read_dir(payload)
        .map_err(display_error)?
        .map(|entry| entry.map(|entry| entry.path()));
    package_members_from_paths(entries, version)
}

pub(crate) fn package_members_from_paths(
    entries: impl IntoIterator<Item = std::io::Result<PathBuf>>,
    version: &str,
) -> Result<Vec<PackageMemberEvidence>> {
    let mut members = Vec::new();
    for path in entries {
        let path = path.map_err(display_error)?;
        if path
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| artifact_kind(name, Some(version)).ok())
            .is_some()
        {
            members.push(package_member_evidence(&path, version)?);
        }
    }
    Ok(members)
}

fn recheck_source(root: &RepoRoot, context: &ImmutableContext, payload: &Path) -> Result<()> {
    require_clean_tree(root.path(), payload)?;
    let commit = command(root.path(), &["git", "rev-parse", "HEAD"])?;
    if commit != context.commit {
        return Err(Error::new(format!(
            "release commit mismatch: expected {}, actual {commit}\nrepair: checkout the expected commit and retry",
            context.commit
        )));
    }
    let lock = digest(&fs::read(root.path().join("Cargo.lock")).map_err(display_error)?);
    if lock != context.cargo_lock_sha256 {
        return Err(Error::new(format!(
            "Cargo.lock digest mismatch: expected {}, actual {lock}\nrepair: restore the committed Cargo.lock and retry",
            context.cargo_lock_sha256
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateProof {
    pub schema_version: u64,
    pub platform: String,
    pub candidate_digest: String,
    pub ledger_sha256: String,
    pub source_commit: String,
    pub cargo_lock_sha256: String,
    pub artifact_basename: String,
    pub artifact_bytes: u64,
    pub artifact_sha256: String,
    pub proof_image_digest: String,
    pub os_release: String,
    pub package_manager_version: String,
    pub install_command: Vec<String>,
    pub install_exit_status: i64,
    pub version_command: Vec<String>,
    pub version_exit_status: i64,
    pub executable_path: String,
    pub executable_mode: u64,
    pub executable_sha256: String,
    pub version_output: String,
    pub result: String,
    pub proof_time: String,
    pub architecture: String,
    pub network: String,
    pub isolation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run_passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolated_prefix_passed: Option<bool>,
}

pub struct ProofRequest<'a> {
    pub root: &'a RepoRoot,
    pub ledger: &'a CandidateLedger,
    pub ledger_bytes: &'a [u8],
    pub platform: &'a str,
    pub image: &'a ImageIdentity,
    pub engine: ContainerEngine,
    pub processes: &'a ProcessEnvironment,
}

pub fn produce_or_retain_proof(request: &ProofRequest<'_>) -> Result<PathBuf> {
    let proof = ProofId::new(request.platform)?;
    let version = VersionComponent::new(&request.ledger.version)?;
    let boundary = ReservedReleaseBoundary::new(request.root);
    let runner = resolve_proof_runner(&boundary, version.clone())?;
    match boundary.presence(
        ReservedPath::Proofs(version.clone()),
        ExpectedLeaf::Directory,
    )? {
        ReservedPathPresence::Present(path) => path.absolute,
        ReservedPathPresence::Absent(path) => {
            fs::create_dir(&path.absolute).map_err(display_error)?;
            path.absolute
        }
    };
    let final_reserved = ReservedPath::Proof(version.clone(), proof.clone());
    let final_path = match boundary.presence(final_reserved, ExpectedLeaf::RegularFile)? {
        ReservedPathPresence::Present(path) => {
            validate_proof_file(request, &path.absolute)?;
            return Ok(path.absolute);
        }
        ReservedPathPresence::Absent(path) => path.absolute,
    };
    let transaction = TransactionComponent::new(&transaction_id()?)?;
    let attempt_reserved =
        ReservedPath::ProofAttempt(version.clone(), proof.clone(), transaction.clone());
    let attempt = boundary
        .resolve_for_create(attempt_reserved.clone(), ExpectedLeaf::Absent)?
        .absolute;
    fs::create_dir(&attempt).map_err(display_error)?;
    let attempt_identity = boundary
        .resolve_for_read(attempt_reserved.clone(), ExpectedLeaf::Directory)?
        .identity
        .expect("present proof attempt identity");
    #[cfg(test)]
    run_proof_attempt_test_barrier(request.root, &attempt_reserved);
    let mut published_identity = None;
    let result = (|| {
        let artifact = proof_artifact(request.ledger, request.platform)?;
        let payload = boundary
            .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)?
            .absolute;
        let artifact_path = payload.join(&artifact.path);
        require_regular(&artifact_path, "proof artifact")?;
        let relabel = if request.engine == ContainerEngine::Podman {
            ",relabel=shared"
        } else {
            ""
        };
        let output_arg = format!("type=bind,src={},dst=/evidence{relabel}", attempt.display());
        let artifact_arg = format!(
            "type=bind,src={},dst=/input/{},ro{relabel}",
            artifact_path.display(),
            artifact.path
        );
        let runner_arg = format!(
            "type=bind,src={},dst=/proof-runner,ro{relabel}",
            runner.display()
        );
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--pull=never".into(),
            "--network=none".into(),
            "--mount".into(),
            output_arg,
            "--mount".into(),
            artifact_arg,
            "--mount".into(),
            runner_arg,
        ];
        if request.platform == "tar-x86_64" {
            args.extend([
                "--mount".into(),
                format!(
                    "type=bind,src={},dst=/input/install.sh,ro{relabel}",
                    request.root.path().join("scripts/install.sh").display()
                ),
            ]);
        }
        args.extend([
            request.image.digest.clone(),
            "/proof-runner".into(),
            "proof-handoff".into(),
            "--platform".into(),
            request.platform.into(),
            "--artifact".into(),
            format!("/input/{}", artifact.path),
            "--output".into(),
            "/evidence/proof.json".into(),
            "--candidate-digest".into(),
            request.ledger.candidate_digest.clone(),
            "--ledger-sha256".into(),
            digest(request.ledger_bytes),
            "--source-commit".into(),
            request.ledger.source.commit.clone(),
            "--cargo-lock-sha256".into(),
            request.ledger.source.cargo_lock_sha256.clone(),
            "--proof-image-digest".into(),
            request.image.digest.clone(),
            "--version".into(),
            request.ledger.version.clone(),
        ]);
        run_proof_runner(
            request.processes,
            request.root.path(),
            request.engine.executable(),
            &args,
        )?;
        let produced = boundary
            .resolve_for_read(
                ReservedPath::ProofAttemptOutput(
                    version.clone(),
                    proof.clone(),
                    transaction.clone(),
                ),
                ExpectedLeaf::RegularFile,
            )?
            .absolute;
        validate_proof_file(request, &produced)?;
        let bytes = fs::read(&produced).map_err(display_error)?;
        CleanupPlan::new(vec![CleanupEntry {
            path: attempt_reserved.clone(),
            expected_type: ExpectedLeaf::Directory,
            expected_identity: attempt_identity,
        }])?
        .preflight(ReservedReleaseBoundary::new(request.root))?
        .execute()?;
        published_identity = Some(atomic_write_0644(&final_path, &bytes)?);
        validate_proof_file(request, &final_path)?;
        Ok(final_path.clone())
    })();
    result.map_err(|error| {
        finish_proof_attempt_cleanup(
            request.root,
            error,
            attempt_reserved,
            attempt_identity,
            ReservedPath::Proof(version, proof),
            published_identity,
        )
    })
}

fn resolve_proof_runner(
    boundary: &ReservedReleaseBoundary<'_>,
    version: VersionComponent,
) -> Result<PathBuf> {
    let runner = boundary
        .resolve_for_read(
            ReservedPath::ProofRunner(version),
            ExpectedLeaf::RegularFile,
        )?
        .absolute;
    let mode = fs::symlink_metadata(&runner).map_err(display_error)?.mode();
    if mode & 0o111 == 0 {
        return Err(Error::new(
            "proof runner mode mismatch: expected executable, actual non-executable",
        ));
    }
    Ok(runner)
}

pub(crate) fn run_proof_runner(
    processes: &ProcessEnvironment,
    root: &Path,
    program: &str,
    args: &[String],
) -> Result<()> {
    let output = processes
        .command(program)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(display_error)?;
    let marker = output.stderr.split(|byte| *byte == b'\n').any(|line| {
        line.strip_suffix(b"\r").unwrap_or(line) == PROOF_RUNNER_ENTRY_MARKER.as_bytes()
    });
    let stderr = sanitize_proof_runner_stderr(&output.stderr);
    let context = (!stderr.is_empty()).then(|| format!("\nstderr: {stderr}"));
    match (output.status.success(), marker) {
        (true, true) => Ok(()),
        (true, false) => Err(Error::new(
            "proof runner protocol mismatch: expected entry marker, actual absent",
        )),
        (false, false) => Err(Error::new(format!(
            "proof runner execution mismatch: expected entry marker, actual container command failed with {}{}\nrepair: use the candidate-retained proof runner built by the pinned Ubuntu build image",
            output.status,
            context.unwrap_or_default()
        ))),
        (false, true) => Err(Error::new(format!(
            "proof handoff mismatch: expected success after entry, actual {}{}",
            output.status,
            context.unwrap_or_default()
        ))),
    }
}

fn sanitize_proof_runner_stderr(bytes: &[u8]) -> String {
    let mut without_marker = Vec::with_capacity(bytes.len());
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line == PROOF_RUNNER_ENTRY_MARKER.as_bytes() {
            continue;
        }
        if !without_marker.is_empty() {
            without_marker.push(b'\n');
        }
        without_marker.extend_from_slice(line);
    }
    sanitize_process_stderr(&without_marker)
}

pub(crate) fn finish_proof_attempt_cleanup(
    root: &RepoRoot,
    error: Error,
    attempt: ReservedPath,
    attempt_identity: FileIdentity,
    published: ReservedPath,
    published_identity: Option<FileIdentity>,
) -> Error {
    let boundary = ReservedReleaseBoundary::new(root);
    let mut entries = Vec::new();
    let attempt_owned = match boundary.presence(attempt.clone(), ExpectedLeaf::Directory) {
        Ok(ReservedPathPresence::Present(path)) => path.identity == Some(attempt_identity),
        Ok(ReservedPathPresence::Absent(_)) => false,
        Err(boundary_error) => return Error::new(format!("{error}\n{boundary_error}")),
    };
    if attempt_owned {
        entries.push(CleanupEntry {
            path: attempt,
            expected_type: ExpectedLeaf::Directory,
            expected_identity: attempt_identity,
        });
    }
    if let Some(identity) = published_identity {
        let published_owned = match boundary.presence(published.clone(), ExpectedLeaf::RegularFile)
        {
            Ok(ReservedPathPresence::Present(path)) => path.identity == Some(identity),
            Ok(ReservedPathPresence::Absent(_)) => false,
            Err(boundary_error) => return Error::new(format!("{error}\n{boundary_error}")),
        };
        if published_owned {
            entries.push(CleanupEntry {
                path: published,
                expected_type: ExpectedLeaf::RegularFile,
                expected_identity: identity,
            });
        }
    }
    if entries.is_empty() {
        error
    } else {
        match CleanupPlan::new(entries) {
            Ok(plan) => plan.finish_error(boundary, error),
            Err(cleanup) => Error::new(format!("{error}\n{cleanup}")),
        }
    }
}

pub(crate) fn proof_artifact<'a>(
    ledger: &'a CandidateLedger,
    platform: &str,
) -> Result<&'a Artifact> {
    let kind = proof_spec(platform)?.artifact_kind;
    artifact_by_kind(&ledger.payload, kind)
}

pub(crate) fn proof_member<'a>(
    ledger: &'a CandidateLedger,
    platform: &str,
) -> Result<&'a PackageMemberEvidence> {
    let artifact = proof_artifact(ledger, platform)?;
    ledger
        .package_members
        .iter()
        .find(|item| item.package_file == artifact.path)
        .ok_or_else(|| {
            Error::new("proof package member mismatch: expected ledger member, actual missing")
        })
}

pub fn validate_proof_file(request: &ProofRequest<'_>, path: &Path) -> Result<CandidateProof> {
    require_regular(path, "candidate proof")?;
    let bytes = fs::read(path).map_err(display_error)?;
    let proof: CandidateProof = serde_json::from_slice(&bytes).map_err(display_error)?;
    if canonical_json(&serde_json::to_value(&proof).map_err(display_error)?)? != bytes {
        return Err(Error::new("proof canonicalization mismatch"));
    }
    let artifact = proof_artifact(request.ledger, request.platform)?;
    let member = proof_member(request.ledger, request.platform)?;
    let release_policy = ReleaseImages::from_root(request.root.path())?;
    let policy = release_policy.proof_policy(request.platform)?;
    if policy.image_digest != request.image.digest
        || policy.executable_path != member.installed_path
        || policy.executable_mode != member.mode
    {
        return Err(Error::new(
            "proof platform policy mismatch: expected ledger-bound image and executable, actual different",
        ));
    }
    let expected = ProofBindings {
        platform: request.platform.into(),
        candidate_digest: request.ledger.candidate_digest.clone(),
        ledger_sha256: digest(request.ledger_bytes),
        source_commit: request.ledger.source.commit.clone(),
        cargo_lock_sha256: request.ledger.source.cargo_lock_sha256.clone(),
        artifact_basename: artifact.path.clone(),
        artifact_bytes: artifact.bytes,
        artifact_sha256: artifact.sha256.clone(),
        proof_image_digest: request.image.digest.clone(),
        os_release: policy.os_release.clone(),
        package_manager_version: policy.package_manager_version.clone(),
        install_command: policy.install_command.clone(),
        install_exit_status: 0,
        version_command: policy.version_command.clone(),
        version_exit_status: 0,
        executable_path: policy.executable_path.clone(),
        executable_mode: policy.executable_mode,
        executable_sha256: member.sha256.clone(),
        version_output: format!("solstone-linux {}", request.ledger.version),
        result: "pass".into(),
        policy_checked_at: request.ledger.policy.checked_at.clone(),
        validation_time: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    validate_candidate_proof(
        &serde_json::to_value(&proof).map_err(display_error)?,
        &expected,
    )?;
    let spec = proof_spec(request.platform)?;
    if proof.network != "none"
        || proof.isolation != "fresh-container"
        || proof.architecture != spec.architecture
    {
        return Err(Error::new("proof environment mismatch"));
    }
    if spec.artifact_kind == "tar"
        && (proof.dry_run_passed != Some(true) || proof.isolated_prefix_passed != Some(true))
    {
        return Err(Error::new("tar proof installer mismatch"));
    }
    if spec.artifact_kind != "tar"
        && (proof.dry_run_passed.is_some() || proof.isolated_prefix_passed.is_some())
    {
        return Err(Error::new("native proof fields mismatch"));
    }
    validate_identity(
        if request.platform == "rpm-x86_64" {
            "fedora_os"
        } else {
            "ubuntu_os"
        },
        &proof.os_release,
    )?;
    let manager_ok = match request.platform {
        "debian-amd64" => {
            proof
                .package_manager_version
                .starts_with("Debian 'dpkg' package management program version ")
                || proof.package_manager_version.starts_with("dpkg ")
        }
        "rpm-x86_64" => {
            proof.package_manager_version.starts_with("RPM version ")
                || proof.package_manager_version.starts_with("rpm ")
        }
        "tar-x86_64" => proof.package_manager_version == "installer portable-tar",
        _ => false,
    };
    if !manager_ok {
        return Err(Error::new(
            "proof package manager mismatch: expected approved identity, actual different",
        ));
    }
    Ok(proof)
}

#[derive(Clone, Debug, Serialize)]
pub struct CandidateStatus {
    pub status: String,
    pub local_evidence_only: bool,
    pub publication_approval: bool,
    pub candidate_digest: String,
    pub bundle_digest: String,
    pub ledger_sha256: String,
    pub proofs: BTreeMap<String, String>,
    pub payload: Vec<Artifact>,
}

pub fn candidate_status(
    root: &RepoRoot,
    expected_ledger: &CandidateLedger,
    expected_ledger_bytes: &[u8],
) -> Result<CandidateStatus> {
    validate_version(&expected_ledger.version)?;
    let version = VersionComponent::new(&expected_ledger.version)?;
    let boundary = ReservedReleaseBoundary::new(root);
    let payload = boundary
        .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)?
        .absolute;
    let (disk_ledger, disk_ledger_bytes) = read_ledger(root, &expected_ledger.version)?;
    if disk_ledger_bytes != expected_ledger_bytes
        || disk_ledger_bytes != ledger_bytes(root, &payload, expected_ledger)?
    {
        return Err(Error::new(
            "candidate ledger bytes mismatch: expected promoted ledger, actual different",
        ));
    }
    let ledger = &disk_ledger;
    let promoted_ledger_bytes = disk_ledger_bytes.as_slice();
    let policy = ReleaseImages::from_root(root.path())?;
    if proof_image_identity(&policy.build_ubuntu).digest != ledger.images.ubuntu_image_id
        || proof_image_identity(&policy.build_fedora).digest != ledger.images.fedora_image_id
    {
        return Err(Error::new(
            "candidate image policy mismatch: expected ledger build images, actual committed policy differs",
        ));
    }
    let evidence_root = boundary
        .resolve_for_read(
            ReservedPath::EvidenceVersion(version.clone()),
            ExpectedLeaf::Directory,
        )?
        .absolute;
    let proofs_root = boundary
        .resolve_for_read(
            ReservedPath::Proofs(version.clone()),
            ExpectedLeaf::Directory,
        )?
        .absolute;
    let evidence_names = directory_names(&evidence_root, "candidate evidence")?;
    if evidence_names
        != BTreeSet::from(["ledger.json".into(), "proof-runner".into(), "proofs".into()])
    {
        return Err(Error::new(
            "candidate evidence inventory mismatch: expected ledger, proof runner, and proofs, actual different",
        ));
    }
    resolve_proof_runner(&boundary, version.clone())?;
    let expected_proof_names = PROOF_SPECS
        .iter()
        .map(|spec| format!("{}.json", spec.id))
        .collect::<BTreeSet<_>>();
    if directory_names(&proofs_root, "candidate proofs")? != expected_proof_names {
        return Err(Error::new(
            "candidate proof inventory mismatch: expected exactly three proofs, actual different",
        ));
    }
    let mut proof_bytes = BTreeMap::new();
    for (id, _, reference) in policy.proof_policies() {
        let path = boundary
            .resolve_for_read(
                ReservedPath::Proof(version.clone(), ProofId::new(id)?),
                ExpectedLeaf::RegularFile,
            )?
            .absolute;
        let image = proof_image_identity(reference);
        validate_proof_file(
            &ProofRequest {
                root,
                ledger,
                ledger_bytes: promoted_ledger_bytes,
                platform: id,
                image: &image,
                engine: ContainerEngine::Podman,
                processes: &ProcessEnvironment::default(),
            },
            &path,
        )?;
        proof_bytes.insert(id.into(), fs::read(path).map_err(display_error)?);
    }
    // The exact three proof IDs and every proof binding were validated above;
    // bundle construction therefore receives a complete, canonical inventory.
    let bundle = bundle_digest(
        &ledger.candidate_digest,
        promoted_ledger_bytes,
        &proof_bytes,
    )?;
    Ok(CandidateStatus {
        status: "candidate-proven".into(),
        local_evidence_only: true,
        publication_approval: false,
        candidate_digest: ledger.candidate_digest.clone(),
        bundle_digest: bundle,
        ledger_sha256: digest(promoted_ledger_bytes),
        proofs: proof_bytes
            .iter()
            .map(|(id, bytes)| (id.clone(), digest(bytes)))
            .collect(),
        payload: ledger.payload.clone(),
    })
}

fn directory_names(path: &Path, label: &str) -> Result<BTreeSet<String>> {
    require_directory(path, label)?;
    fs::read_dir(path)
        .map_err(display_error)?
        .map(|entry| {
            entry
                .map_err(display_error)?
                .file_name()
                .into_string()
                .map_err(|_| Error::new(format!("{label} contains non-UTF-8 path")))
        })
        .collect()
}

pub(crate) fn proof_image_identity(reference: &str) -> ImageIdentity {
    let digest = format!("sha256:{}", reference.rsplit_once("sha256:").unwrap().1);
    ImageIdentity {
        configured_reference: reference.into(),
        digest,
    }
}

pub fn recover_candidate(root: &RepoRoot, version: &str) -> Result<String> {
    let boundary = ReservedReleaseBoundary::new(root);
    if let ReservedPathPresence::Present(_) =
        boundary.presence(ReservedPath::Lock, ExpectedLeaf::RegularFile)?
    {
        return Err(Error::new(
            "candidate recovery lock mismatch: expected absent, actual present",
        ));
    }
    let dist = boundary
        .resolve_for_read(ReservedPath::Dist, ExpectedLeaf::Directory)?
        .absolute;
    let payload = boundary
        .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)?
        .absolute;
    let before = tree_snapshot(&dist)?;
    let (ledger, bytes) = read_ledger(root, version)?;
    require_clean_tree(root.path(), &payload)?;
    let commit = command(root.path(), &["git", "rev-parse", "HEAD"])?;
    if commit != ledger.source.commit {
        return Err(Error::new(format!(
            "recovery commit mismatch: expected {}, actual {commit}",
            ledger.source.commit
        )));
    }
    if digest(&fs::read(root.path().join("Cargo.lock")).map_err(display_error)?)
        != ledger.source.cargo_lock_sha256
    {
        return Err(Error::new("recovery Cargo.lock mismatch"));
    }
    classify_release(root, &payload, false)?;
    let _ = candidate_status(root, &ledger, &bytes)?;
    if tree_snapshot(&dist)? != before {
        return Err(Error::new("candidate recovery mutation mismatch"));
    }
    Ok("retained-candidate-valid".into())
}

fn tree_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    fn walk(base: &Path, path: &Path, out: &mut BTreeMap<PathBuf, String>) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(path).map_err(display_error)? {
            let entry = entry.map_err(display_error)?;
            let child = entry.path();
            let relative = child.strip_prefix(base).map_err(display_error)?.to_owned();
            let metadata = fs::symlink_metadata(&child).map_err(display_error)?;
            if metadata.file_type().is_symlink() {
                out.insert(relative, "symlink".into());
            } else if metadata.is_dir() {
                out.insert(relative, "dir".into());
                walk(base, &child, out)?;
            } else {
                out.insert(relative, digest(&fs::read(child).map_err(display_error)?));
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

pub struct ProofHandoffInput<'a> {
    pub platform: &'a str,
    pub artifact: &'a Path,
    pub output: &'a Path,
    pub candidate_digest: &'a str,
    pub ledger_sha256: &'a str,
    pub source_commit: &'a str,
    pub cargo_lock_sha256: &'a str,
    pub proof_image_digest: &'a str,
    pub version: &'a str,
}

pub fn emit_proof_handoff(input: &ProofHandoffInput<'_>) -> Result<()> {
    eprintln!("{PROOF_RUNNER_ENTRY_MARKER}");
    if Command::new("sh")
        .args(["-c", "command -v solstone-linux >/dev/null 2>&1"])
        .status()
        .map_err(display_error)?
        .success()
    {
        return Err(Error::new(
            "proof pre-existing install mismatch: expected absent, actual present",
        ));
    }
    let root = Path::new("/proof-root");
    let artifact_name = input
        .artifact
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::new("proof artifact basename mismatch"))?
        .to_owned();
    let artifact_record = artifact(input.artifact)?;
    let (
        install_command,
        executable_path,
        actual_executable,
        package_manager_version,
        architecture,
        dry,
        isolated,
    ) = match input.platform {
        "debian-amd64" => {
            let command = vec![
                "dpkg".into(),
                "--install".into(),
                input.artifact.display().to_string(),
            ];
            run_exact(&command)?;
            (
                command,
                "/usr/bin/solstone-linux",
                Path::new("/usr/bin/solstone-linux").to_path_buf(),
                command_line("dpkg", &["--version"])?,
                "amd64",
                None,
                None,
            )
        }
        "rpm-x86_64" => {
            let command = vec![
                "rpm".into(),
                "--install".into(),
                input.artifact.display().to_string(),
            ];
            run_exact(&command)?;
            (
                command,
                "/usr/bin/solstone-linux",
                Path::new("/usr/bin/solstone-linux").to_path_buf(),
                command_line("rpm", &["--version"])?,
                "x86_64",
                None,
                None,
            )
        }
        "tar-x86_64" => {
            if root.symlink_metadata().is_ok() {
                return Err(Error::new(
                    "proof isolation root mismatch: expected absent, actual present",
                ));
            }
            let script = Path::new("/input/install.sh");
            let dry_command = vec![
                script.display().to_string(),
                "--dry-run".into(),
                "--prefix".into(),
                "/proof-root".into(),
                input.artifact.display().to_string(),
            ];
            run_exact(&dry_command)?;
            if root.symlink_metadata().is_ok() {
                return Err(Error::new("tar proof dry-run mutation mismatch"));
            }
            let command = vec![
                script.display().to_string(),
                "--prefix".into(),
                "/proof-root".into(),
                input.artifact.display().to_string(),
            ];
            run_exact(&command)?;
            (
                command,
                "/bin/solstone-linux",
                root.join("bin/solstone-linux"),
                "installer portable-tar".into(),
                "x86_64",
                Some(true),
                Some(true),
            )
        }
        _ => return Err(Error::new("proof platform mismatch")),
    };
    require_regular_executable(&actual_executable, "proof installed executable")?;
    let metadata = fs::symlink_metadata(&actual_executable).map_err(display_error)?;
    let version_command = vec![executable_path.into(), "--version".into()];
    let output = Command::new(&actual_executable)
        .arg("--version")
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(
            "proof version command mismatch: expected success",
        ));
    }
    let version_output = String::from_utf8(output.stdout)
        .map_err(display_error)?
        .trim()
        .to_owned();
    let proof = CandidateProof {
        schema_version: 1,
        platform: input.platform.into(),
        candidate_digest: input.candidate_digest.into(),
        ledger_sha256: input.ledger_sha256.into(),
        source_commit: input.source_commit.into(),
        cargo_lock_sha256: input.cargo_lock_sha256.into(),
        artifact_basename: artifact_name,
        artifact_bytes: artifact_record.bytes,
        artifact_sha256: artifact_record.sha256,
        proof_image_digest: input.proof_image_digest.into(),
        os_release: proof_os_release()?,
        package_manager_version: package_manager_version
            .lines()
            .next()
            .unwrap_or_default()
            .into(),
        install_command,
        install_exit_status: 0,
        version_command,
        version_exit_status: 0,
        executable_path: executable_path.into(),
        executable_mode: metadata.permissions().mode() as u64 & 0o7777,
        executable_sha256: digest(&fs::read(actual_executable).map_err(display_error)?),
        version_output,
        result: "pass".into(),
        proof_time: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        architecture: architecture.into(),
        network: "none".into(),
        isolation: "fresh-container".into(),
        dry_run_passed: dry,
        isolated_prefix_passed: isolated,
    };
    let bytes = canonical_json(&serde_json::to_value(proof).map_err(display_error)?)?;
    fs::write(input.output, bytes).map_err(display_error)
}

fn run_exact(args: &[String]) -> Result<()> {
    let output = Command::new(&args[0])
        .args(&args[1..])
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "proof command mismatch: expected success, actual {}",
            output.status
        )));
    }
    Ok(())
}
fn command_line(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new("proof identity command mismatch"));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(display_error)
}
fn proof_os_release() -> Result<String> {
    let text = fs::read_to_string("/etc/os-release").map_err(display_error)?;
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|s| s.trim_matches(['\'', '"']).to_owned())
        .ok_or_else(|| Error::new("proof OS mismatch"))
}

pub(crate) fn finish_candidate_staging_owned<T>(
    root: &RepoRoot,
    staging: &StagingLayout,
    result: Result<T>,
) -> Result<T> {
    let transaction = staging
        .transaction
        .clone()
        .expect("production staging has a transaction");
    let cleanup = CleanupPlan::new(vec![CleanupEntry {
        path: ReservedPath::StagingInvocation(transaction),
        expected_type: ExpectedLeaf::Directory,
        expected_identity: staging.root_identity,
    }])
    .and_then(|plan| plan.preflight(ReservedReleaseBoundary::new(root)))
    .and_then(ValidatedCleanupPlan::execute);
    match (result, cleanup) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(Error::new(format!("{error}\n{cleanup}"))),
    }
}

pub(crate) fn finish_created_candidate<T>(
    finalized: &FinalizedCandidate,
    owned_proofs: &[PathBuf],
    result: Result<T>,
) -> Result<T> {
    let _ = owned_proofs;
    result.map_err(|error| {
        let root_path = finalized
            .payload_root
            .parent()
            .and_then(Path::parent)
            .expect("reserved payload is below repository root");
        let root = RepoRoot::validate_path(root_path).expect("existing validated repository root");
        let version = VersionComponent::new(&finalized.ledger.version)
            .expect("finalized version is validated");
        let entries = vec![
            CleanupEntry {
                path: ReservedPath::Payload,
                expected_type: ExpectedLeaf::Directory,
                expected_identity: finalized.payload_identity,
            },
            CleanupEntry {
                path: ReservedPath::EvidenceVersion(version),
                expected_type: ExpectedLeaf::Directory,
                expected_identity: finalized.evidence_identity,
            },
        ];
        match CleanupPlan::new(entries)
            .and_then(|plan| plan.preflight(ReservedReleaseBoundary::new(&root)))
            .and_then(ValidatedCleanupPlan::execute)
        {
            Ok(_) => error,
            Err(cleanup) => Error::new(format!("{error}\n{cleanup}")),
        }
    })
}

pub fn create_candidate(
    root: &RepoRoot,
    expected_commit: &str,
    descriptor: &Path,
    processes: &ProcessEnvironment,
) -> Result<CandidateStatus> {
    require_expected_commit(root, expected_commit)?;
    let lock = CandidateLock::acquire(root)?;
    let result = create_candidate_locked(root, expected_commit, descriptor, processes, &lock);
    finish_candidate_lock(lock, result)
}

fn create_candidate_locked(
    root: &RepoRoot,
    expected_commit: &str,
    descriptor: &Path,
    processes: &ProcessEnvironment,
    lock: &CandidateLock,
) -> Result<CandidateStatus> {
    let version = workspace_version(root)?;
    let payload = ReservedReleaseBoundary::new(root)
        .presence(ReservedPath::Payload, ExpectedLeaf::Any)?
        .resolved()
        .absolute;
    require_clean_tree(root.path(), &payload)?;
    clear_candidate_paths(root, &version)?;
    let staging = StagingLayout::create(root, lock)?;
    let result = (|| {
        let context = export_immutable_context(root, &staging.context)?;
        if context.commit != expected_commit {
            return Err(Error::new(format!(
                "release commit mismatch: expected {expected_commit}, actual {}\nrepair: checkout the expected commit and retry",
                context.commit
            )));
        }
        let image_policy = ReleaseImages::from_context(&context)?;
        let engine = detect_container_engine(processes)?;
        let engine_identity = observe_container_engine(processes, engine)?;
        let images = resolve_release_images(processes, engine, &image_policy)?;
        let cohort = run_advisory_cohort(&context, &staging, descriptor, processes)?;
        build_proof_runner(&ProofRunnerBuildRequest {
            repo: root,
            context: &context,
            engine,
            ubuntu: &images.build_ubuntu,
            fedora: &images.build_fedora,
            output: &staging.proof_runner,
            processes,
        })?;
        let invocation = staging
            .root
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| Error::new("candidate invocation mismatch"))?;
        let deb = build_lane(&LaneRequest {
            repo: root,
            context: &context,
            lane: Lane::Deb,
            engine,
            invocation_id: invocation,
            version: &version,
            ubuntu: &images.build_ubuntu,
            fedora: &images.build_fedora,
            output: &staging.deb_lane,
            processes,
        })?;
        let rpm = build_lane(&LaneRequest {
            repo: root,
            context: &context,
            lane: Lane::Rpm,
            engine,
            invocation_id: invocation,
            version: &version,
            ubuntu: &images.build_ubuntu,
            fedora: &images.build_fedora,
            output: &staging.rpm_lane,
            processes,
        })?;
        let baseline_executable =
            reconcile_lanes(&deb, &rpm, &staging.deb_lane, &staging.rpm_lane, &version)?;
        let finalized = finalize_candidate(FinalizeInput {
            root,
            staging: &staging,
            context: &context,
            version: &version,
            deb: &deb,
            rpm: &rpm,
            baseline_executable,
            cohort: &cohort,
            images: &images,
            engine,
            engine_identity,
            processes,
        })?;
        let mut owned = Vec::new();
        let proof_result = (|| {
            for (id, image) in images.proof_images() {
                owned.push(
                    finalized
                        .evidence_root
                        .join("proofs")
                        .join(format!("{id}.json")),
                );
                produce_or_retain_proof(&ProofRequest {
                    root,
                    ledger: &finalized.ledger,
                    ledger_bytes: &finalized.ledger_bytes,
                    platform: id,
                    image,
                    engine,
                    processes,
                })?;
            }
            recheck_all_images(processes, engine, &images)?;
            recheck_source(root, &context, &finalized.payload_root)?;
            recheck_advisory_cohort(&cohort, processes, Utc::now())?;
            candidate_status(root, &finalized.ledger, &finalized.ledger_bytes)
        })();
        finish_created_candidate(&finalized, &owned, proof_result)
    })();
    finish_candidate_staging_owned(root, &staging, result)
}

pub fn prove_candidate(
    root: &RepoRoot,
    version: &str,
    descriptor: &Path,
    processes: &ProcessEnvironment,
) -> Result<CandidateStatus> {
    validate_version(version)?;
    let lock = CandidateLock::acquire(root)?;
    let result = prove_candidate_locked(root, version, descriptor, processes);
    finish_candidate_lock(lock, result)
}

fn prove_candidate_locked(
    root: &RepoRoot,
    version: &str,
    descriptor: &Path,
    processes: &ProcessEnvironment,
) -> Result<CandidateStatus> {
    let boundary = ReservedReleaseBoundary::new(root);
    let payload = boundary
        .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)?
        .absolute;
    let (ledger, ledger_bytes) = read_ledger(root, version)?;
    require_clean_tree(root.path(), &payload)?;
    let commit = command(root.path(), &["git", "rev-parse", "HEAD"])?;
    let lock_digest = digest(&fs::read(root.path().join("Cargo.lock")).map_err(display_error)?);
    let archive_digest = digest(&command_bytes(
        root.path(),
        &["git", "archive", "--format=tar", "HEAD"],
    )?);
    if commit != ledger.source.commit
        || lock_digest != ledger.source.cargo_lock_sha256
        || archive_digest != ledger.source.archive_sha256
    {
        return Err(Error::new(
            "candidate resume source mismatch: expected ledger source, actual checkout differs",
        ));
    }
    classify_release(root, &payload, false)?;
    let advisory = validate_resume_advisory_identity(descriptor, processes)?;
    if advisory.source_id != ledger.advisory_cohort.source_id
        || advisory.commit != ledger.advisory_cohort.commit
        || advisory.archive_sha256 != ledger.advisory_cohort.archive_sha256
    {
        return Err(Error::new(
            "candidate resume advisory cohort mismatch: expected retained identity, actual descriptor differs\nrepair: provide the exact retained advisory database",
        ));
    }
    let policy = ReleaseImages::from_root(root.path())?;
    let engine = detect_container_engine(processes)?;
    let images = resolve_release_images(processes, engine, &policy)?;
    if images.build_ubuntu.digest != ledger.images.ubuntu_image_id
        || images.build_fedora.digest != ledger.images.fedora_image_id
    {
        return Err(Error::new("candidate resume build image mismatch"));
    }
    preflight_existing_proofs(root, &ledger, &ledger_bytes, &images, engine, processes)?;

    let context = ImmutableContext {
        commit,
        archive_sha256: archive_digest,
        cargo_lock_sha256: lock_digest,
        path: root.path().to_owned(),
    };
    for (id, image) in images.proof_images() {
        produce_or_retain_proof(&ProofRequest {
            root,
            ledger: &ledger,
            ledger_bytes: &ledger_bytes,
            platform: id,
            image,
            engine,
            processes,
        })?;
    }
    recheck_all_images(processes, engine, &images)?;
    recheck_source(root, &context, &payload)?;
    let final_advisory = validate_resume_advisory_identity(descriptor, processes)?;
    if final_advisory.source_id != advisory.source_id
        || final_advisory.commit != advisory.commit
        || final_advisory.archive_sha256 != advisory.archive_sha256
    {
        return Err(Error::new(
            "candidate resume advisory cohort mismatch: expected unchanged identity, actual changed",
        ));
    }
    candidate_status(root, &ledger, &ledger_bytes)
}

fn finish_candidate_lock<T>(lock: CandidateLock, result: Result<T>) -> Result<T> {
    match (result, lock.release()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(release)) => Err(release),
        (Err(primary), Err(release)) => Err(Error::new(format!("{primary}\n{release}"))),
    }
}

fn preflight_existing_proofs(
    root: &RepoRoot,
    ledger: &CandidateLedger,
    ledger_bytes: &[u8],
    images: &ResolvedImages,
    engine: ContainerEngine,
    processes: &ProcessEnvironment,
) -> Result<()> {
    let boundary = ReservedReleaseBoundary::new(root);
    let version = VersionComponent::new(&ledger.version)?;
    let proofs_root = match boundary.presence(
        ReservedPath::Proofs(version.clone()),
        ExpectedLeaf::Directory,
    )? {
        ReservedPathPresence::Absent(_) => return Ok(()),
        ReservedPathPresence::Present(path) => path.absolute,
    };
    let allowed = PROOF_SPECS
        .iter()
        .map(|spec| format!("{}.json", spec.id))
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(&proofs_root).map_err(display_error)? {
        let name = entry
            .map_err(display_error)?
            .file_name()
            .into_string()
            .map_err(|_| Error::new("candidate proof basename mismatch"))?;
        if !allowed.contains(&name) {
            return Err(Error::new(
                "candidate proof inventory mismatch: expected only retained proof IDs, actual extra entry",
            ));
        }
    }
    for (id, image) in images.proof_images() {
        let reserved = ReservedPath::Proof(version.clone(), ProofId::new(id)?);
        if let ReservedPathPresence::Present(path) =
            boundary.presence(reserved, ExpectedLeaf::RegularFile)?
        {
            validate_proof_file(
                &ProofRequest {
                    root,
                    ledger,
                    ledger_bytes,
                    platform: id,
                    image,
                    engine,
                    processes,
                },
                &path.absolute,
            )?;
        }
    }
    Ok(())
}

fn recheck_all_images(
    processes: &ProcessEnvironment,
    engine: ContainerEngine,
    images: &ResolvedImages,
) -> Result<()> {
    for image in [
        &images.build_ubuntu,
        &images.build_fedora,
        &images.proof_debian,
        &images.proof_rpm,
        &images.proof_tar,
    ] {
        let actual = inspect_image(processes, engine, &image.configured_reference)?;
        if actual.digest != image.digest {
            return Err(Error::new(format!(
                "image identity mismatch: expected {}, actual {}",
                image.digest, actual.digest
            )));
        }
    }
    Ok(())
}
fn workspace_version(root: &RepoRoot) -> Result<String> {
    let value: toml::Value =
        toml::from_str(&fs::read_to_string(root.path().join("Cargo.toml")).map_err(display_error)?)
            .map_err(display_error)?;
    let version = value["workspace"]["package"]["version"]
        .as_str()
        .ok_or_else(|| Error::new("workspace version mismatch"))?
        .to_owned();
    validate_version(&version)?;
    Ok(version)
}
pub(crate) fn require_expected_commit(root: &RepoRoot, expected: &str) -> Result<()> {
    require_commit(expected, "expected release commit")?;
    let actual = command(root.path(), &["git", "rev-parse", "HEAD"])?;
    if actual != expected {
        return Err(Error::new(format!(
            "release commit mismatch: expected {expected}, actual {actual}\nrepair: checkout the expected commit and retry"
        )));
    }
    Ok(())
}
fn clear_candidate_paths(root: &RepoRoot, version: &str) -> Result<()> {
    let version = VersionComponent::new(version)?;
    let boundary = ReservedReleaseBoundary::new(root);
    let ownership_error = || {
        Error::new(format!(
            "existing release candidate ownership mismatch: expected complete validated solstone-linux candidate {}, actual unowned or invalid reserved paths\nrepair: run candidate recover --version {} to inspect a retained candidate; move or remove only the named dist/rust and dist/rust-evidence/{} paths after independently confirming ownership",
            version.0, version.0, version.0
        ))
    };
    let ownership_presence = |result: Result<ReservedPathPresence>| {
        result.map_err(|error| {
            if error.to_string().contains("wrong type or symlink") {
                ownership_error()
            } else {
                error
            }
        })
    };
    let payload_presence =
        ownership_presence(boundary.presence(ReservedPath::Payload, ExpectedLeaf::Directory))?;
    let evidence_presence = ownership_presence(boundary.presence(
        ReservedPath::EvidenceVersion(version.clone()),
        ExpectedLeaf::Directory,
    ))?;
    let payload_present = matches!(payload_presence, ReservedPathPresence::Present(_));
    let evidence_present = matches!(evidence_presence, ReservedPathPresence::Present(_));
    if !payload_present && !evidence_present {
        return Ok(());
    }
    if !payload_present || !evidence_present {
        return Err(ownership_error());
    }
    let payload = boundary
        .resolve_for_read(ReservedPath::Payload, ExpectedLeaf::Directory)
        .map_err(|_| ownership_error())?;
    let evidence = boundary
        .resolve_for_read(
            ReservedPath::EvidenceVersion(version.clone()),
            ExpectedLeaf::Directory,
        )
        .map_err(|_| ownership_error())?;
    let (ledger, ledger_bytes) = read_ledger(root, &version.0).map_err(|_| ownership_error())?;
    validate_ledger(root, &payload.absolute, &ledger).map_err(|_| ownership_error())?;
    classify_release(root, &payload.absolute, false).map_err(|_| ownership_error())?;
    validate_retained_status_structure(root, &ledger, &ledger_bytes)
        .map_err(|_| ownership_error())?;
    let payload = boundary
        .resolve_for_replace(
            ReservedPath::Payload,
            ExpectedLeaf::Directory,
            payload.identity.expect("payload identity"),
        )
        .map_err(|_| ownership_error())?;
    let evidence = boundary
        .resolve_for_replace(
            ReservedPath::EvidenceVersion(version.clone()),
            ExpectedLeaf::Directory,
            evidence.identity.expect("evidence identity"),
        )
        .map_err(|_| ownership_error())?;
    CleanupPlan::new(vec![
        CleanupEntry {
            path: ReservedPath::Payload,
            expected_type: ExpectedLeaf::Directory,
            expected_identity: payload.identity.expect("payload identity"),
        },
        CleanupEntry {
            path: ReservedPath::EvidenceVersion(version),
            expected_type: ExpectedLeaf::Directory,
            expected_identity: evidence.identity.expect("evidence identity"),
        },
    ])?
    .preflight(boundary)?
    .execute()?;
    Ok(())
}

fn validate_retained_status_structure(
    root: &RepoRoot,
    ledger: &CandidateLedger,
    ledger_bytes: &[u8],
) -> Result<()> {
    let version = VersionComponent::new(&ledger.version)?;
    let boundary = ReservedReleaseBoundary::new(root);
    let evidence = boundary
        .resolve_for_read(
            ReservedPath::EvidenceVersion(version.clone()),
            ExpectedLeaf::Directory,
        )?
        .absolute;
    let proofs = boundary
        .resolve_for_read(
            ReservedPath::Proofs(version.clone()),
            ExpectedLeaf::Directory,
        )?
        .absolute;
    if directory_names(&evidence, "candidate evidence")?
        != BTreeSet::from(["ledger.json".into(), "proof-runner".into(), "proofs".into()])
    {
        return Err(Error::new(
            "candidate evidence inventory mismatch: expected ledger, proof runner, and proofs, actual different",
        ));
    }
    resolve_proof_runner(&boundary, version.clone())?;
    let policy = ReleaseImages::from_root(root.path())?;
    let allowed = PROOF_SPECS
        .iter()
        .map(|spec| format!("{}.json", spec.id))
        .collect::<BTreeSet<_>>();
    let present = directory_names(&proofs, "candidate proofs")?;
    if !present.is_subset(&allowed) {
        return Err(Error::new(
            "candidate proof inventory mismatch: expected retained proof IDs, actual different",
        ));
    }
    for (id, _, reference) in policy.proof_policies() {
        if !present.contains(&format!("{id}.json")) {
            continue;
        }
        let path = boundary
            .resolve_for_read(
                ReservedPath::Proof(version.clone(), ProofId::new(id)?),
                ExpectedLeaf::RegularFile,
            )?
            .absolute;
        validate_proof_file(
            &ProofRequest {
                root,
                ledger,
                ledger_bytes,
                platform: id,
                image: &proof_image_identity(reference),
                engine: ContainerEngine::Podman,
                processes: &ProcessEnvironment::default(),
            },
            &path,
        )?;
    }
    if present == allowed {
        let status = candidate_status(root, ledger, ledger_bytes)?;
        if !status.local_evidence_only || status.publication_approval {
            return Err(Error::new(
                "candidate status mismatch: expected local evidence without publication approval, actual different",
            ));
        }
    }
    Ok(())
}
