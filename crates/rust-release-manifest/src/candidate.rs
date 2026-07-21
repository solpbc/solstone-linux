// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use std::ffi::{OsStr, OsString};
use std::process::Output;

const ADVISORY_URL: &str = "file://localhost/advisory-db";
const DAY_SECONDS: i64 = 24 * 60 * 60;
pub(crate) const LANE_EVIDENCE_NAME: &str = "lane-evidence.json";
pub const LANE_HANDOFF: &str = ".lane-evidence-handoff.json";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseImages {
    pub build_ubuntu: String,
    pub build_fedora: String,
    pub proof_debian: String,
    pub proof_rpm: String,
    pub proof_tar: String,
    #[serde(rename = "debian-amd64")]
    pub debian_amd64: ProofPlatformPolicy,
    #[serde(rename = "rpm-x86_64")]
    pub rpm_x86_64: ProofPlatformPolicy,
    #[serde(rename = "tar-x86_64")]
    pub tar_x86_64: ProofPlatformPolicy,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProofPlatformPolicy {
    pub image_digest: String,
    pub os_release: String,
    pub package_manager_version: String,
    pub install_command: Vec<String>,
    pub version_command: Vec<String>,
    pub executable_path: String,
    pub executable_mode: u64,
}

impl ReleaseImages {
    pub fn from_context(context: &ImmutableContext) -> Result<Self> {
        Self::from_root(&context.path)
    }

    pub fn from_root(root: &Path) -> Result<Self> {
        let path = root.join("packaging/release-policy.toml");
        require_regular(&path, "release policy authority")?;
        let images: Self = toml::from_str(&fs::read_to_string(path).map_err(display_error)?)
            .map_err(|_| {
                Error::new(
                    "release policy mismatch: expected exact image and proof policy, actual invalid\nrepair: restore packaging/release-policy.toml from the release commit",
                )
            })?;
        for (role, value) in images.roles() {
            validate_image_reference(role, value)?;
        }
        for (id, policy, image) in images.proof_policies() {
            require_image_digest(&policy.image_digest)?;
            let expected = format!("sha256:{}", image.rsplit_once("sha256:").unwrap().1);
            if policy.image_digest != expected
                || policy.install_command.is_empty()
                || policy.version_command.is_empty()
                || policy.executable_mode > 0o7777
            {
                return Err(Error::new(format!(
                    "release proof policy {id} mismatch: expected image-bound exact values, actual invalid\nrepair: commit values observed from the provisioned proof image"
                )));
            }
            for value in std::iter::once(policy.os_release.as_str())
                .chain(std::iter::once(policy.package_manager_version.as_str()))
                .chain(policy.install_command.iter().map(String::as_str))
                .chain(policy.version_command.iter().map(String::as_str))
                .chain(std::iter::once(policy.executable_path.as_str()))
            {
                if value.contains('/') {
                    if value.chars().any(char::is_control)
                        || !(value.starts_with("/input/")
                            || value.starts_with("/proof-root")
                            || value.starts_with("--root=/proof-root")
                            || value.starts_with("/usr/bin/")
                            || value.starts_with("/bin/"))
                    {
                        return Err(Error::new(
                            "release proof policy path mismatch: expected stable in-container path, actual invalid",
                        ));
                    }
                } else {
                    validate_evidence_text("release proof policy", value)?;
                }
            }
        }
        Ok(images)
    }

    pub fn proof_policies(&self) -> [(&'static str, &ProofPlatformPolicy, &str); 3] {
        [
            (PROOF_SPECS[0].id, &self.debian_amd64, &self.proof_debian),
            (PROOF_SPECS[1].id, &self.rpm_x86_64, &self.proof_rpm),
            (PROOF_SPECS[2].id, &self.tar_x86_64, &self.proof_tar),
        ]
    }

    pub fn proof_policy(&self, id: &str) -> Result<&ProofPlatformPolicy> {
        self.proof_policies()
            .into_iter()
            .find_map(|(actual, policy, _)| (actual == id).then_some(policy))
            .ok_or_else(|| Error::new("proof platform policy mismatch"))
    }

    pub fn roles(&self) -> [(&'static str, &str); 5] {
        [
            ("build_ubuntu", &self.build_ubuntu),
            ("build_fedora", &self.build_fedora),
            ("proof_debian", &self.proof_debian),
            ("proof_rpm", &self.proof_rpm),
            ("proof_tar", &self.proof_tar),
        ]
    }
}

fn validate_image_reference(role: &str, value: &str) -> Result<()> {
    let digest = value
        .strip_prefix("sha256:")
        .or_else(|| value.rsplit_once("@sha256:").map(|(_, digest)| digest));
    let named = value.rsplit_once("@sha256:").map(|(name, _)| name);
    if digest.is_none_or(|digest| !is_sha256(digest))
        || named.is_some_and(|name| {
            name.is_empty()
                || name.starts_with('-')
                || name.chars().any(|character| {
                    !(character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '_' | '-' | ':' | '/'))
                })
        })
    {
        return Err(Error::new(format!(
            "release image {role} mismatch: expected immutable digest reference, actual invalid\nrepair: commit the locally provisioned image digest for {role} in packaging/release-policy.toml"
        )));
    }
    if let Some(name) = named {
        validate_evidence_text(&format!("release image {role}"), name).map_err(|_| {
            Error::new(format!("release image {role} mismatch: expected privacy-safe immutable reference, actual invalid\nrepair: commit a privacy-safe digest reference in packaging/release-policy.toml"))
        })?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ResolvedImages {
    pub build_ubuntu: ImageIdentity,
    pub build_fedora: ImageIdentity,
    pub proof_debian: ImageIdentity,
    pub proof_rpm: ImageIdentity,
    pub proof_tar: ImageIdentity,
}

impl ResolvedImages {
    pub fn proof_images(&self) -> [(&'static str, &ImageIdentity); 3] {
        [
            (PROOF_SPECS[0].id, &self.proof_debian),
            (PROOF_SPECS[1].id, &self.proof_rpm),
            (PROOF_SPECS[2].id, &self.proof_tar),
        ]
    }
}

pub fn resolve_release_images(
    processes: &ProcessEnvironment,
    engine: ContainerEngine,
    images: &ReleaseImages,
) -> Result<ResolvedImages> {
    let resolve = |role: &str, reference: &str| {
        let identity = inspect_image(processes, engine, reference).map_err(|_| {
            Error::new(format!(
                "release image {role} mismatch: expected provisioned local image, actual unavailable\nrepair: provision {reference} locally before candidate entry"
            ))
        })?;
        let expected = reference.rsplit_once("sha256:").unwrap().1;
        if identity.digest != format!("sha256:{expected}") {
            return Err(Error::new(format!(
                "release image {role} mismatch: expected sha256:{expected}, actual {}\nrepair: provision the committed digest locally before candidate entry",
                identity.digest
            )));
        }
        Ok(identity)
    };
    Ok(ResolvedImages {
        build_ubuntu: resolve("build_ubuntu", &images.build_ubuntu)?,
        build_fedora: resolve("build_fedora", &images.build_fedora)?,
        proof_debian: resolve("proof_debian", &images.proof_debian)?,
        proof_rpm: resolve("proof_rpm", &images.proof_rpm)?,
        proof_tar: resolve("proof_tar", &images.proof_tar)?,
    })
}

#[derive(Clone, Debug, Default)]
pub struct ProcessEnvironment {
    path: Option<OsString>,
    git_canaries: Option<(OsString, OsString)>,
}

impl ProcessEnvironment {
    pub fn with_path(path: &OsStr) -> Self {
        Self {
            path: Some(path.to_owned()),
            git_canaries: None,
        }
    }

    pub fn with_git_canaries(mut self, git_dir: &OsStr, git_work_tree: &OsStr) -> Self {
        self.git_canaries = Some((git_dir.to_owned(), git_work_tree.to_owned()));
        self
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        if let Some((git_dir, git_work_tree)) = &self.git_canaries {
            command
                .env("GIT_DIR", git_dir)
                .env("GIT_WORK_TREE", git_work_tree);
        }
        // Candidate Git operations must always discover the worktree selected by
        // current_dir, never an ambient caller override.
        command.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
        command
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryDescriptor {
    pub schema_version: u64,
    pub source_id: String,
    pub db_path: PathBuf,
    pub acquired_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryIdentity {
    pub source_id: String,
    pub commit: String,
    pub archive_sha256: String,
    pub acquired_at: String,
    snapshot_path: PathBuf,
}

pub fn validate_advisory_descriptor_identity(
    descriptor_path: &Path,
    processes: &ProcessEnvironment,
) -> Result<AdvisoryIdentity> {
    validate_advisory_descriptor_identity_mode(descriptor_path, processes, true)
}

pub fn validate_resume_advisory_identity(
    descriptor_path: &Path,
    processes: &ProcessEnvironment,
) -> Result<AdvisoryIdentity> {
    validate_advisory_descriptor_identity_mode(descriptor_path, processes, false)
}

fn validate_advisory_descriptor_identity_mode(
    descriptor_path: &Path,
    processes: &ProcessEnvironment,
    enforce_freshness: bool,
) -> Result<AdvisoryIdentity> {
    require_regular(descriptor_path, "advisory descriptor")?;
    let descriptor: AdvisoryDescriptor = strict_json_file(descriptor_path).map_err(|_| {
        Error::new(
            "advisory descriptor mismatch: expected strict four-field JSON, actual invalid\nrepair: provide the retained cohort descriptor",
        )
    })?;
    if descriptor.schema_version != 1 {
        return Err(Error::new(format!(
            "advisory descriptor schema mismatch: expected 1, actual {}\nrepair: provide a schema-version 1 descriptor",
            descriptor.schema_version
        )));
    }
    validate_evidence_text("advisory source_id", &descriptor.source_id)?;
    validate_timestamp("advisory acquired_at", &descriptor.acquired_at)?;
    let acquired = DateTime::parse_from_rfc3339(&descriptor.acquired_at).map_err(display_error)?;
    let age = Utc::now().signed_duration_since(acquired);
    if enforce_freshness && (age.num_seconds() < 0 || age.num_seconds() > DAY_SECONDS) {
        return Err(Error::new(
            "advisory acquisition time mismatch: expected within 24 hours, actual stale\nrepair: acquire and commit a current advisory cohort before finalization",
        ));
    }
    if !descriptor.db_path.is_absolute()
        || descriptor.db_path.as_os_str().as_encoded_bytes().first() == Some(&b'-')
    {
        return Err(Error::new(
            "advisory database path mismatch: expected absolute non-option path, actual invalid\nrepair: provide an absolute local advisory database path",
        ));
    }
    let metadata = fs::symlink_metadata(&descriptor.db_path).map_err(|_| {
        Error::new(
            "advisory database mismatch: expected present no-follow directory, actual unavailable\nrepair: provision the retained advisory database locally",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::new(
            "advisory database mismatch: expected no-follow directory, actual other\nrepair: provide a canonical regular directory",
        ));
    }
    let snapshot_path = descriptor.db_path.canonicalize().map_err(|_| {
        Error::new("advisory database mismatch: expected canonical directory, actual invalid")
    })?;
    if snapshot_path != descriptor.db_path {
        return Err(Error::new(
            "advisory database path mismatch: expected canonical path, actual noncanonical\nrepair: use the canonical absolute database path",
        ));
    }
    if run_stdout(
        processes,
        &snapshot_path,
        "git",
        &["rev-parse", "--is-inside-work-tree"],
    )? != "true"
    {
        return Err(Error::new(
            "advisory database mismatch: expected git worktree, actual other",
        ));
    }
    let status = run_stdout(
        processes,
        &snapshot_path,
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    if !status.is_empty() {
        return Err(Error::new(
            "advisory database status mismatch: expected clean, actual dirty\nrepair: restore the retained advisory database to its committed state",
        ));
    }
    let commit = run_stdout(processes, &snapshot_path, "git", &["rev-parse", "HEAD"])?;
    require_commit(&commit, "advisory database commit")?;
    let archive = run_output(
        processes,
        &snapshot_path,
        "git",
        &["archive", "--format=tar", "HEAD"],
    )?
    .stdout;
    Ok(AdvisoryIdentity {
        source_id: descriptor.source_id,
        commit,
        archive_sha256: digest(&archive),
        acquired_at: descriptor.acquired_at,
        snapshot_path,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryCohort {
    pub source_id: String,
    pub commit: String,
    pub archive_sha256: String,
    pub acquired_at: String,
    pub cargo_deny_version: String,
    pub deterministic_gate: String,
    pub licenses_bans_sources: String,
    pub advisories: String,
    pub checked_at: String,
    snapshot_path: PathBuf,
}

pub fn run_advisory_cohort(
    context: &ImmutableContext,
    staging: &StagingLayout,
    descriptor_path: &Path,
    processes: &ProcessEnvironment,
) -> Result<AdvisoryCohort> {
    run_advisory_cohort_mode(context, staging, descriptor_path, processes)
}

fn run_advisory_cohort_mode(
    context: &ImmutableContext,
    staging: &StagingLayout,
    descriptor_path: &Path,
    processes: &ProcessEnvironment,
) -> Result<AdvisoryCohort> {
    let identity = validate_advisory_descriptor_identity(descriptor_path, processes)?;
    let snapshot_path = identity.snapshot_path;
    let commit = identity.commit;
    let archive_sha256 = identity.archive_sha256;

    let db_root = &staging.advisory_db;
    require_directory(db_root, "isolated advisory database root")?;
    let derived = db_root.join(advisory_db_directory(ADVISORY_URL)?);
    if derived.exists() {
        return Err(Error::new(
            "isolated advisory database mismatch: expected absent, actual present",
        ));
    }
    let source = snapshot_path
        .to_str()
        .ok_or_else(|| Error::new("advisory database path mismatch: expected UTF-8"))?;
    let destination = derived
        .to_str()
        .ok_or_else(|| Error::new("advisory database path mismatch: expected UTF-8"))?;
    run_success(
        processes,
        &context.path,
        "git",
        &["clone", "--no-hardlinks", source, destination],
    )?;
    if run_stdout(processes, &derived, "git", &["rev-parse", "HEAD"])? != commit
        || !run_stdout(
            processes,
            &derived,
            "git",
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored=matching",
            ],
        )?
        .is_empty()
        || digest(
            &run_output(
                processes,
                &derived,
                "git",
                &["archive", "--format=tar", "HEAD"],
            )?
            .stdout,
        ) != archive_sha256
    {
        return Err(Error::new(
            "materialized advisory database mismatch: expected validated source, actual different",
        ));
    }

    let config_path = staging.root.join("advisory-deny.toml");
    let deny = fs::read_to_string(context.path.join("deny.toml")).map_err(display_error)?;
    let config = advisory_config(&deny, db_root)?;
    fs::write(&config_path, config).map_err(display_error)?;

    run_cargo_deny(
        processes,
        &context.path,
        &context.path.join("deny.toml"),
        &["licenses", "bans", "sources"],
    )?;
    run_cargo_deny(processes, &context.path, &config_path, &["advisories"])?;
    let checked_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(AdvisoryCohort {
        source_id: identity.source_id,
        commit,
        archive_sha256,
        acquired_at: identity.acquired_at,
        cargo_deny_version: CARGO_DENY_VERSION.into(),
        deterministic_gate: "pass".into(),
        licenses_bans_sources: "pass".into(),
        advisories: "pass".into(),
        checked_at,
        snapshot_path,
    })
}

pub fn recheck_advisory_cohort(
    cohort: &AdvisoryCohort,
    processes: &ProcessEnvironment,
    validation_time: DateTime<Utc>,
) -> Result<()> {
    let acquired = DateTime::parse_from_rfc3339(&cohort.acquired_at).map_err(display_error)?;
    let age = validation_time.signed_duration_since(acquired);
    if age.num_seconds() < 0 || age.num_seconds() > DAY_SECONDS {
        return Err(Error::new(
            "advisory acquisition time mismatch: expected within 24 hours, actual stale",
        ));
    }
    let commit = run_stdout(
        processes,
        &cohort.snapshot_path,
        "git",
        &["rev-parse", "HEAD"],
    )?;
    let status = run_stdout(
        processes,
        &cohort.snapshot_path,
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    let archive = run_output(
        processes,
        &cohort.snapshot_path,
        "git",
        &["archive", "--format=tar", "HEAD"],
    )?;
    if commit != cohort.commit
        || !status.is_empty()
        || digest(&archive.stdout) != cohort.archive_sha256
    {
        return Err(Error::new(
            "advisory snapshot mismatch: expected finalized cohort, actual changed",
        ));
    }
    Ok(())
}

fn advisory_config(source: &str, db_root: &Path) -> Result<String> {
    let marker = "[advisories]\n";
    let index = source.find(marker).ok_or_else(|| {
        Error::new("advisory policy mismatch: expected [advisories], actual missing")
    })? + marker.len();
    let root = db_root
        .to_str()
        .ok_or_else(|| Error::new("advisory db-path mismatch: expected UTF-8"))?;
    if root.contains(['"', '\n', '\r']) {
        return Err(Error::new("advisory db-path mismatch: expected safe path"));
    }
    let additions = format!(
        "db-path = \"{root}\"\ndb-urls = [\"{ADVISORY_URL}\"]\nmaximum-db-staleness = \"1d\"\n"
    );
    Ok(format!(
        "{}{}{}",
        &source[..index],
        additions,
        &source[index..]
    ))
}

fn run_cargo_deny(
    processes: &ProcessEnvironment,
    root: &Path,
    config: &Path,
    checks: &[&str],
) -> Result<()> {
    let config = config
        .to_str()
        .ok_or_else(|| Error::new("cargo-deny config mismatch: expected UTF-8"))?;
    let mut args = vec!["deny", "--locked", "--offline", "--config", config, "check"];
    args.extend_from_slice(checks);
    run_success(processes, root, "cargo", &args)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerEngine {
    Podman,
    Docker,
}

impl ContainerEngine {
    pub(crate) fn executable(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageIdentity {
    pub configured_reference: String,
    pub digest: String,
}

pub fn inspect_image(
    processes: &ProcessEnvironment,
    engine: ContainerEngine,
    reference: &str,
) -> Result<ImageIdentity> {
    validate_image_reference("inspect", reference)?;
    if engine == ContainerEngine::Docker {
        run_success(processes, Path::new("."), "docker", &["buildx", "version"])?;
    }
    let output = run_output(
        processes,
        Path::new("."),
        engine.executable(),
        &["image", "inspect", reference],
    )?;
    let values: Vec<Value> = serde_json::from_slice(&output.stdout).map_err(display_error)?;
    if values.len() != 1 {
        return Err(Error::new(format!(
            "image inspect result mismatch: expected 1, actual {}",
            values.len()
        )));
    }
    let image = &values[0];
    let id = image["Id"]
        .as_str()
        .ok_or_else(|| Error::new("image ID mismatch: expected 64 lowercase hex, actual missing"))?
        .strip_prefix("sha256:")
        .unwrap_or_else(|| image["Id"].as_str().unwrap());
    if !is_sha256(id) {
        return Err(Error::new(
            "image ID mismatch: expected 64 lowercase hex, actual invalid",
        ));
    }
    if image["Os"] != "linux" || image["Architecture"] != "amd64" {
        return Err(Error::new(format!(
            "image platform mismatch: expected linux/amd64, actual {}/{}",
            image["Os"].as_str().unwrap_or("missing"),
            image["Architecture"].as_str().unwrap_or("missing")
        )));
    }
    Ok(ImageIdentity {
        configured_reference: reference.into(),
        digest: format!("sha256:{id}"),
    })
}

pub fn observe_container_engine(
    processes: &ProcessEnvironment,
    engine: ContainerEngine,
) -> Result<String> {
    let raw = run_stdout(
        processes,
        Path::new("."),
        engine.executable(),
        &["--version"],
    )?;
    let identity = match engine {
        ContainerEngine::Podman => raw,
        ContainerEngine::Docker => {
            let version = raw
                .strip_prefix("Docker version ")
                .and_then(|tail| tail.split(',').next())
                .ok_or_else(|| Error::new("container engine mismatch: expected Docker version"))?;
            format!("docker {version}")
        }
    };
    validate_identity("container_engine", &identity)?;
    Ok(identity)
}

pub fn detect_container_engine(processes: &ProcessEnvironment) -> Result<ContainerEngine> {
    if run_stdout(processes, Path::new("."), "podman", &["--version"]).is_ok() {
        return Ok(ContainerEngine::Podman);
    }
    if run_stdout(processes, Path::new("."), "docker", &["--version"]).is_ok() {
        return Ok(ContainerEngine::Docker);
    }
    Err(Error::new(
        "container engine mismatch: expected local podman or docker, actual unavailable\nrepair: provision a supported local container engine before candidate entry",
    ))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Deb,
    Rpm,
}

impl Lane {
    fn target(self) -> &'static str {
        match self {
            Self::Deb => "deb",
            Self::Rpm => "rpm",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LaneEvidence {
    pub invocation_id: String,
    pub lane: Lane,
    pub source_commit: String,
    pub source_archive_sha256: String,
    pub cargo_lock_sha256: String,
    pub version: String,
    pub target: String,
    pub profile: String,
    pub features: Vec<String>,
    pub rustc_verbose: String,
    pub cargo: String,
    pub baseline_executable_sha256: String,
    pub image_digest: String,
    pub packaging_tool: String,
    pub native_tools: LaneNativeTools,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LaneNativeTools {
    Ubuntu(UbuntuLaneTools),
    Fedora(FedoraLaneTools),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UbuntuLaneTools {
    pub cargo_deb: String,
    pub dpkg_deb: String,
    pub signing_mode: String,
    pub ubuntu_cargo: String,
    pub ubuntu_compiler: String,
    pub ubuntu_glibc: String,
    pub ubuntu_gzip: String,
    pub ubuntu_image_digest: String,
    pub ubuntu_linker: String,
    pub ubuntu_os: String,
    pub ubuntu_rustc: String,
    pub ubuntu_tar: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FedoraLaneTools {
    pub cargo_generate_rpm: String,
    pub fedora_image_digest: String,
    pub fedora_os: String,
    pub rpm: String,
    pub signing_mode: String,
}

pub struct LaneRequest<'a> {
    pub repo: &'a RepoRoot,
    pub context: &'a ImmutableContext,
    pub lane: Lane,
    pub engine: ContainerEngine,
    pub invocation_id: &'a str,
    pub version: &'a str,
    pub ubuntu: &'a ImageIdentity,
    pub fedora: &'a ImageIdentity,
    pub output: &'a Path,
    pub processes: &'a ProcessEnvironment,
}

pub struct LaneEmitRequest<'a> {
    pub lane: Lane,
    pub invocation_id: &'a str,
    pub source_commit: &'a str,
    pub source_archive_sha256: &'a str,
    pub expected_cargo_lock_sha256: &'a str,
    pub version: &'a str,
    pub target: &'a str,
    pub profile: &'a str,
    pub features: Vec<String>,
    pub image_digest: &'a str,
    pub baseline_executable: &'a Path,
    pub artifacts: Vec<PathBuf>,
    pub output: &'a Path,
}

pub fn emit_lane_handoff(request: &LaneEmitRequest<'_>) -> Result<()> {
    emit_lane_handoff_in(request, Path::new("."))
}

pub(crate) fn emit_lane_handoff_in(
    request: &LaneEmitRequest<'_>,
    context_root: &Path,
) -> Result<()> {
    require_image_digest(request.image_digest)?;
    let binding: ContextBinding = strict_json_file(&context_root.join(CONTEXT_BINDING_NAME))?;
    let archive = fs::read(context_root.join(CONTEXT_ARCHIVE_NAME)).map_err(display_error)?;
    let observed_commit = git_archive_commit(&archive)?;
    let observed_archive_sha256 = digest(&archive);
    require_commit(&observed_commit, "lane source commit")?;
    for (label, value) in [
        ("source archive digest", observed_archive_sha256.as_str()),
        ("Cargo.lock digest", binding.cargo_lock_sha256.as_str()),
    ] {
        if !is_sha256(value) {
            return Err(Error::new(format!(
                "lane {label} mismatch: expected 64 lowercase hex, actual {value}"
            )));
        }
    }
    if binding.source_commit != observed_commit
        || binding.source_archive_sha256 != observed_archive_sha256
        || observed_commit != request.source_commit
        || observed_archive_sha256 != request.source_archive_sha256
        || binding.cargo_lock_sha256 != request.expected_cargo_lock_sha256
    {
        return Err(Error::new(
            "lane immutable context binding mismatch: expected exported context authority, actual carried arguments differ",
        ));
    }
    let workspace: toml::Value = toml::from_str(
        &fs::read_to_string(context_root.join("Cargo.toml")).map_err(display_error)?,
    )
    .map_err(display_error)?;
    let actual_version = workspace["workspace"]["package"]["version"]
        .as_str()
        .ok_or_else(|| Error::new("lane version mismatch: expected workspace version"))?
        .to_owned();
    validate_version(&actual_version)?;
    if actual_version != request.version {
        return Err(Error::new(format!(
            "lane version mismatch: expected {actual_version}, actual {}",
            request.version
        )));
    }
    let actual_lock = digest(&fs::read(context_root.join("Cargo.lock")).map_err(display_error)?);
    if actual_lock != binding.cargo_lock_sha256 {
        return Err(Error::new(format!(
            "Cargo.lock digest mismatch: expected {}, actual {actual_lock}",
            binding.cargo_lock_sha256
        )));
    }
    if !request.features.is_empty() {
        return Err(Error::new(
            "lane features mismatch: expected build command's empty feature set, actual nonempty",
        ));
    }
    let features = Vec::new();
    let rustc_verbose = command_evidence("rustc", &["--version", "--verbose"])?;
    let actual_target = rustc_verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| Error::new("lane target mismatch: expected rustc host"))?
        .to_owned();
    if actual_target != request.target {
        return Err(Error::new(format!(
            "lane target mismatch: expected {actual_target}, actual {}",
            request.target
        )));
    }
    let executable_path = request.baseline_executable.to_string_lossy();
    let actual_profile = if executable_path.contains("/release/")
        || executable_path.starts_with("target/release/")
    {
        "release"
    } else {
        return Err(Error::new(
            "lane profile mismatch: expected release build output, actual other",
        ));
    };
    if actual_profile != request.profile {
        return Err(Error::new(
            "lane profile mismatch: expected observed release output",
        ));
    }
    let cargo = command_evidence("cargo", &["--version"])?;
    let packaging_tool = match request.lane {
        Lane::Deb => command_evidence("cargo", &["deb", "--version"]),
        Lane::Rpm => command_evidence("cargo-generate-rpm", &["--version"]),
    }?;
    let image_id = request.image_digest.strip_prefix("sha256:").unwrap();
    let native_package = request
        .artifacts
        .iter()
        .find(|path| {
            path.extension().and_then(OsStr::to_str)
                == Some(match request.lane {
                    Lane::Deb => "deb",
                    Lane::Rpm => "rpm",
                })
        })
        .ok_or_else(|| {
            Error::new("lane native artifact mismatch: expected package, actual missing")
        })?;
    let signing_mode = observe_signing_mode(request.lane, native_package)?;
    let native_tools = match request.lane {
        Lane::Deb => LaneNativeTools::Ubuntu(UbuntuLaneTools {
            cargo_deb: version_token(&packaging_tool, "cargo-deb")?,
            dpkg_deb: dpkg_deb_identity()?,
            signing_mode,
            ubuntu_cargo: two_word_identity(&cargo, "cargo")?,
            ubuntu_compiler: command_first_line("cc", &["--version"])?,
            ubuntu_glibc: command_evidence("getconf", &["GNU_LIBC_VERSION"])?,
            ubuntu_gzip: command_first_line("gzip", &["--version"])?,
            ubuntu_image_digest: image_id.into(),
            ubuntu_linker: command_first_line("ld", &["--version"])?,
            ubuntu_os: os_pretty_name()?,
            ubuntu_rustc: two_word_identity(&rustc_verbose, "rustc")?,
            ubuntu_tar: command_first_line("tar", &["--version"])?,
        }),
        Lane::Rpm => LaneNativeTools::Fedora(FedoraLaneTools {
            cargo_generate_rpm: version_token(&packaging_tool, "cargo-generate-rpm")?,
            fedora_image_digest: image_id.into(),
            fedora_os: os_pretty_name()?,
            rpm: command_evidence("rpm", &["--version"])?,
            signing_mode,
        }),
    };
    let evidence = LaneEvidence {
        invocation_id: request.invocation_id.into(),
        lane: request.lane,
        // The invocation nonce and base-image digest are carried authorities: the
        // nonce has no in-container source, while the digest is the same value
        // consumed by FROM and cannot be introspected from a build container.
        source_commit: observed_commit,
        source_archive_sha256: observed_archive_sha256,
        cargo_lock_sha256: actual_lock,
        version: actual_version,
        target: actual_target,
        profile: actual_profile.into(),
        features,
        rustc_verbose,
        cargo,
        baseline_executable_sha256: digest(
            &fs::read(request.baseline_executable).map_err(display_error)?,
        ),
        image_digest: request.image_digest.into(),
        packaging_tool,
        native_tools,
        artifacts: request
            .artifacts
            .iter()
            .map(|path| artifact(path))
            .collect::<Result<Vec<_>>>()?,
    };
    let bytes = canonical_json(&serde_json::to_value(evidence).map_err(display_error)?)?;
    fs::write(request.output, bytes).map_err(display_error)
}

fn command_evidence(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "{program} evidence command mismatch: expected success, actual {}",
            output.status
        )));
    }
    let field = if program == "rustc" && args == ["--version", "--verbose"] {
        "lane rustc verbose"
    } else {
        program
    };
    normalize_command_evidence(
        field,
        String::from_utf8(output.stdout).map_err(display_error)?,
    )
}

fn command_first_line(program: &str, args: &[&str]) -> Result<String> {
    command_evidence(program, args)?
        .lines()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| Error::new(format!("{program} identity mismatch: expected output")))
}

fn two_word_identity(value: &str, expected_name: &str) -> Result<String> {
    let mut words = value.split_whitespace();
    let name = words.next().unwrap_or_default();
    let version = words.next().unwrap_or_default();
    if name != expected_name || version.is_empty() {
        return Err(Error::new(format!(
            "{expected_name} identity mismatch: expected name and version, actual {value}"
        )));
    }
    Ok(format!("{name} {version}"))
}

fn version_token(value: &str, expected_name: &str) -> Result<String> {
    Ok(two_word_identity(value, expected_name)?
        .split_once(' ')
        .unwrap()
        .1
        .to_owned())
}

fn dpkg_deb_identity() -> Result<String> {
    let output = command_first_line("dpkg-deb", &["--version"])?;
    let version = output
        .split_once(" version ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .ok_or_else(|| {
            Error::new("dpkg-deb identity mismatch: expected version, actual invalid")
        })?;
    Ok(format!("dpkg-deb {version}"))
}

fn os_pretty_name() -> Result<String> {
    let body = fs::read_to_string("/etc/os-release").map_err(display_error)?;
    let value = body
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches(['\'', '"']).to_owned())
        .ok_or_else(|| Error::new("OS identity mismatch: expected PRETTY_NAME, actual missing"))?;
    validate_evidence_text("lane OS", &value)?;
    Ok(value)
}

fn observe_signing_mode(lane: Lane, package: &Path) -> Result<String> {
    let output = match lane {
        Lane::Deb => Command::new("ar").arg("t").arg(package).output(),
        Lane::Rpm => Command::new("rpm").arg("--checksig").arg(package).output(),
    }
    .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "package signature check mismatch: expected success, actual {}",
            output.status
        )));
    }
    let text = String::from_utf8(output.stdout).map_err(display_error)?;
    let lower = text.to_ascii_lowercase();
    let unsigned = match lane {
        Lane::Deb => !text.lines().any(|line| line.starts_with("_gpg")),
        Lane::Rpm => {
            lower.contains("digests ok")
                && !lower.contains("signature")
                && !lower.contains("pgp")
                && !lower.contains("gpg")
        }
    };
    if !unsigned {
        return Err(Error::new(
            "package signing mode mismatch: expected unsigned, actual signed or unknown",
        ));
    }
    Ok("unsigned".into())
}

pub fn build_lane(request: &LaneRequest<'_>) -> Result<LaneEvidence> {
    require_image_digest(&request.ubuntu.digest)?;
    require_image_digest(&request.fedora.digest)?;
    if request.context.path == request.repo.path()
        || request.context.path.starts_with(request.repo.path())
            && !request.context.path.starts_with(
                request
                    .repo
                    .path()
                    .join("dist/.rust-release-candidate-staging"),
            )
    {
        return Err(Error::new(
            "container build context mismatch: expected immutable export, actual live repository",
        ));
    }
    require_directory(request.output, "lane output")?;
    let file = request.context.path.join("packaging/Containerfile");
    require_regular(&file, "immutable Containerfile")?;
    let file = path_text(&file, "Containerfile")?;
    let output = path_text(request.output, "lane output")?;
    let context = path_text(&request.context.path, "immutable context")?;
    let mut args = match request.engine {
        ContainerEngine::Podman => vec![
            "build".into(),
            "--pull=never".into(),
            "--network=none".into(),
            "--file".into(),
            file.into(),
            "--target".into(),
            request.lane.target().into(),
            "--output".into(),
            format!("type=local,dest={output}"),
        ],
        ContainerEngine::Docker => {
            run_success(
                request.processes,
                request.repo.path(),
                "docker",
                &["buildx", "version"],
            )?;
            vec![
                "buildx".into(),
                "build".into(),
                "--pull=false".into(),
                "--network=none".into(),
                "--file".into(),
                file.into(),
                "--target".into(),
                request.lane.target().into(),
                "--output".into(),
                format!("type=local,dest={output}"),
            ]
        }
    };
    for (key, value) in [
        ("UBUNTU_TOOL_BASE", request.ubuntu.digest.as_str()),
        ("FEDORA_TOOL_BASE", request.fedora.digest.as_str()),
        ("INVOCATION_ID", request.invocation_id),
        ("SOURCE_COMMIT", request.context.commit.as_str()),
        (
            "SOURCE_ARCHIVE_SHA256",
            request.context.archive_sha256.as_str(),
        ),
        (
            "CARGO_LOCK_SHA256",
            request.context.cargo_lock_sha256.as_str(),
        ),
        ("RELEASE_VERSION", request.version),
    ] {
        args.extend(["--build-arg".into(), format!("{key}={value}")]);
    }
    args.push(context.into());
    run_success_owned(
        request.processes,
        &request.context.path,
        request.engine.executable(),
        &args,
    )?;
    let evidence = consume_lane_handoff(request)?;
    validate_lane_inventory(request, LANE_EVIDENCE_NAME)?;
    Ok(evidence)
}

fn require_image_digest(value: &str) -> Result<()> {
    if value
        .strip_prefix("sha256:")
        .is_none_or(|id| !is_sha256(id))
    {
        return Err(Error::new(format!(
            "image digest mismatch: expected sha256:<64 lowercase hex>, actual {value}"
        )));
    }
    Ok(())
}

fn consume_lane_handoff(request: &LaneRequest<'_>) -> Result<LaneEvidence> {
    let handoff = request.output.join(LANE_HANDOFF);
    validate_lane_inventory(request, LANE_HANDOFF)?;
    let evidence: LaneEvidence = strict_json_file(&handoff)?;
    validate_lane_evidence(&evidence, request)?;
    fs::rename(&handoff, request.output.join(LANE_EVIDENCE_NAME)).map_err(display_error)?;
    Ok(evidence)
}

fn normalize_command_evidence(label: &str, value: String) -> Result<String> {
    let normalized = value.trim_end_matches(['\r', '\n']).replace("\r\n", "\n");
    validate_evidence_text(label, &normalized)?;
    Ok(normalized)
}

fn validate_lane_inventory(request: &LaneRequest<'_>, evidence_name: &str) -> Result<()> {
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(request.output).map_err(display_error)? {
        let entry = entry.map_err(display_error)?;
        require_regular(&entry.path(), "lane output")?;
        actual.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| Error::new("lane output mismatch: expected UTF-8 basename"))?,
        );
    }
    let mut expected = BTreeSet::from([evidence_name.to_owned()]);
    expected.extend(expected_artifact_names(request.lane, request.version)?);
    if actual != expected {
        return Err(Error::new(format!(
            "lane output inventory mismatch: expected {expected:?}, actual {actual:?}"
        )));
    }
    Ok(())
}

fn expected_artifact_names(lane: Lane, version: &str) -> Result<[String; 2]> {
    let tar = artifact_name("tar", version)?;
    let native = artifact_name(
        match lane {
            Lane::Deb => "deb",
            Lane::Rpm => "rpm",
        },
        version,
    )?;
    Ok([tar, native])
}

pub(crate) fn validate_lane_evidence(
    evidence: &LaneEvidence,
    request: &LaneRequest<'_>,
) -> Result<()> {
    let expected_image = match request.lane {
        Lane::Deb => &request.ubuntu.digest,
        Lane::Rpm => &request.fedora.digest,
    };
    let expected_tool = match request.lane {
        Lane::Deb => "cargo-deb 3.7.0",
        Lane::Rpm => "cargo-generate-rpm 0.21.0",
    };
    let checks = [
        (
            evidence.invocation_id == request.invocation_id,
            "invocation_id",
        ),
        (evidence.lane == request.lane, "lane"),
        (
            evidence.source_commit == request.context.commit,
            "source_commit",
        ),
        (
            evidence.source_archive_sha256 == request.context.archive_sha256,
            "source_archive_sha256",
        ),
        (
            evidence.cargo_lock_sha256 == request.context.cargo_lock_sha256,
            "cargo_lock_sha256",
        ),
        (evidence.version == request.version, "version"),
        (evidence.target == TARGET_TRIPLE, "target"),
        (evidence.profile == "release", "profile"),
        (evidence.features.is_empty(), "features"),
        (&evidence.image_digest == expected_image, "image_digest"),
        (evidence.packaging_tool == expected_tool, "packaging_tool"),
    ];
    if let Some((_, field)) = checks.into_iter().find(|(valid, _)| !valid) {
        return Err(Error::new(format!("lane evidence {field} mismatch")));
    }
    validate_evidence_text("lane rustc verbose", &evidence.rustc_verbose)?;
    validate_evidence_text("lane cargo", &evidence.cargo)?;
    let rustc_lines = evidence.rustc_verbose.lines().collect::<BTreeSet<_>>();
    if !evidence.rustc_verbose.starts_with("rustc 1.97.1 (")
        || !rustc_lines.contains("host: x86_64-unknown-linux-gnu")
        || !rustc_lines.contains("release: 1.97.1")
    {
        return Err(Error::new(
            "lane rustc identity mismatch: expected 1.97.1 linux/amd64 verbose banner, actual different",
        ));
    }
    if !evidence.cargo.starts_with("cargo 1.97.1 (") {
        return Err(Error::new(
            "lane Cargo identity mismatch: expected 1.97.1 banner, actual different",
        ));
    }
    if !is_sha256(&evidence.baseline_executable_sha256) {
        return Err(Error::new("lane baseline executable digest mismatch"));
    }
    validate_lane_native_tools(evidence, request)?;
    let expected_kinds = match request.lane {
        Lane::Deb => BTreeSet::from(["deb", "tar"]),
        Lane::Rpm => BTreeSet::from(["rpm", "tar"]),
    };
    let mut kinds = BTreeSet::new();
    for artifact_record in &evidence.artifacts {
        let kind = artifact_kind(&artifact_record.path, Some(request.version))?;
        require_regular(
            &request.output.join(&artifact_record.path),
            &artifact_record.path,
        )?;
        if artifact(&request.output.join(&artifact_record.path))? != *artifact_record {
            return Err(Error::new(format!(
                "lane artifact mismatch: expected evidence, actual {}",
                artifact_record.path
            )));
        }
        if !kinds.insert(kind) {
            return Err(Error::new("lane artifact mismatch: expected unique kinds"));
        }
    }
    if kinds != expected_kinds || evidence.artifacts.len() != 2 {
        return Err(Error::new("lane artifact inventory mismatch"));
    }
    Ok(())
}

fn validate_lane_native_tools(evidence: &LaneEvidence, request: &LaneRequest<'_>) -> Result<()> {
    let actual = serde_json::from_value::<BTreeMap<String, String>>(
        serde_json::to_value(&evidence.native_tools).map_err(display_error)?,
    )
    .map_err(display_error)?;
    let expected = TOOL_SPECS
        .iter()
        .filter(|spec| {
            spec.source == ToolSource::BothLanes
                || matches!(
                    (request.lane, spec.source),
                    (Lane::Deb, ToolSource::Ubuntu) | (Lane::Rpm, ToolSource::Fedora)
                )
        })
        .map(|spec| spec.key)
        .collect::<BTreeSet<_>>();
    if actual.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return Err(Error::new(
            "lane native tool inventory mismatch: expected typed lane authority, actual different",
        ));
    }
    match (&evidence.native_tools, request.lane) {
        (LaneNativeTools::Ubuntu(tools), Lane::Deb) => {
            for (actual, expected, key) in [
                (tools.cargo_deb.as_str(), "3.7.0", "cargo_deb"),
                (tools.signing_mode.as_str(), "unsigned", "signing_mode"),
                (tools.ubuntu_rustc.as_str(), "rustc 1.97.1", "ubuntu_rustc"),
                (tools.ubuntu_cargo.as_str(), "cargo 1.97.1", "ubuntu_cargo"),
            ] {
                if actual != expected {
                    return Err(Error::new(format!(
                        "lane native tool {key} mismatch: expected {expected}, actual {actual}"
                    )));
                }
            }
            let expected_image = request.ubuntu.digest.strip_prefix("sha256:").unwrap();
            if tools.ubuntu_image_digest != expected_image {
                return Err(Error::new(format!(
                    "lane native tool ubuntu_image_digest mismatch: expected {expected_image}, actual {}",
                    tools.ubuntu_image_digest
                )));
            }
            for (key, value) in [
                ("dpkg_deb", tools.dpkg_deb.as_str()),
                ("ubuntu_os", tools.ubuntu_os.as_str()),
                ("ubuntu_compiler", tools.ubuntu_compiler.as_str()),
                ("ubuntu_linker", tools.ubuntu_linker.as_str()),
                ("ubuntu_glibc", tools.ubuntu_glibc.as_str()),
                ("ubuntu_tar", tools.ubuntu_tar.as_str()),
                ("ubuntu_gzip", tools.ubuntu_gzip.as_str()),
            ] {
                validate_identity(key, value)?;
            }
        }
        (LaneNativeTools::Fedora(tools), Lane::Rpm) => {
            for (actual, expected, key) in [
                (
                    tools.cargo_generate_rpm.as_str(),
                    "0.21.0",
                    "cargo_generate_rpm",
                ),
                (tools.signing_mode.as_str(), "unsigned", "signing_mode"),
            ] {
                if actual != expected {
                    return Err(Error::new(format!(
                        "lane native tool {key} mismatch: expected {expected}, actual {actual}"
                    )));
                }
            }
            let expected_image = request.fedora.digest.strip_prefix("sha256:").unwrap();
            if tools.fedora_image_digest != expected_image {
                return Err(Error::new(format!(
                    "lane native tool fedora_image_digest mismatch: expected {expected_image}, actual {}",
                    tools.fedora_image_digest
                )));
            }
            for (key, value) in [
                ("fedora_os", tools.fedora_os.as_str()),
                ("rpm", tools.rpm.as_str()),
            ] {
                validate_identity(key, value)?;
            }
        }
        _ => {
            return Err(Error::new(
                "lane native tools mismatch: expected lane-specific evidence, actual cross-wired",
            ));
        }
    }
    Ok(())
}

pub fn assemble_manifest_native_tools(
    root: &RepoRoot,
    deb: &LaneEvidence,
    rpm: &LaneEvidence,
    container_engine: String,
) -> Result<BTreeMap<String, String>> {
    validate_identity("container_engine", &container_engine)?;
    let (LaneNativeTools::Ubuntu(ubuntu), LaneNativeTools::Fedora(fedora)) =
        (&deb.native_tools, &rpm.native_tools)
    else {
        return Err(Error::new(
            "manifest native tools mismatch: expected Ubuntu and Fedora lanes, actual cross-wired",
        ));
    };
    if ubuntu.signing_mode != fedora.signing_mode {
        return Err(Error::new(format!(
            "signing mode mismatch: expected {}, actual {}",
            ubuntu.signing_mode, fedora.signing_mode
        )));
    }
    let mut tools = serde_json::from_value::<BTreeMap<String, String>>(
        serde_json::to_value(ubuntu).map_err(display_error)?,
    )
    .map_err(display_error)?;
    tools.extend(
        serde_json::from_value::<BTreeMap<String, String>>(
            serde_json::to_value(fedora).map_err(display_error)?,
        )
        .map_err(display_error)?,
    );
    tools.insert("container_engine".into(), container_engine);
    tools.insert(
        "manifest_validator".into(),
        env!("CARGO_PKG_VERSION").into(),
    );
    validate_native_tools(root, &tools)?;
    Ok(tools)
}

pub fn reconcile_lanes(
    deb: &LaneEvidence,
    rpm: &LaneEvidence,
    deb_root: &Path,
    rpm_root: &Path,
) -> Result<()> {
    if deb.lane != Lane::Deb || rpm.lane != Lane::Rpm || deb.invocation_id != rpm.invocation_id {
        return Err(Error::new(
            "lane reconciliation mismatch: expected paired lanes",
        ));
    }
    for (matches, field) in [
        (deb.rustc_verbose == rpm.rustc_verbose, "rustc_verbose"),
        (deb.cargo == rpm.cargo, "cargo"),
        (deb.target == rpm.target, "target"),
        (deb.profile == rpm.profile, "profile"),
        (deb.features == rpm.features, "features"),
        (deb.source_commit == rpm.source_commit, "source_commit"),
        (
            deb.source_archive_sha256 == rpm.source_archive_sha256,
            "source_archive_sha256",
        ),
        (
            deb.cargo_lock_sha256 == rpm.cargo_lock_sha256,
            "cargo_lock_sha256",
        ),
        (
            deb.baseline_executable_sha256 == rpm.baseline_executable_sha256,
            "baseline_executable_sha256",
        ),
    ] {
        if !matches {
            return Err(Error::new(format!("lane reconciliation {field} mismatch")));
        }
    }
    let deb_tar = artifact_by_kind(&deb.artifacts, "tar")?;
    let rpm_tar = artifact_by_kind(&rpm.artifacts, "tar")?;
    if deb_tar != rpm_tar
        || fs::read(deb_root.join(&deb_tar.path)).map_err(display_error)?
            != fs::read(rpm_root.join(&rpm_tar.path)).map_err(display_error)?
    {
        return Err(Error::new(
            "lane tar mismatch: expected byte-identical, actual different",
        ));
    }
    Ok(())
}

pub fn recheck_images(
    processes: &ProcessEnvironment,
    engine: ContainerEngine,
    expected: [&ImageIdentity; 2],
) -> Result<()> {
    for identity in expected {
        let actual = inspect_image(processes, engine, &identity.configured_reference)?;
        if actual.digest != identity.digest {
            return Err(Error::new(format!(
                "image identity mismatch: expected {}, actual {}",
                identity.digest, actual.digest
            )));
        }
    }
    Ok(())
}

fn strict_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    require_regular(path, "JSON input")?;
    serde_json::from_slice(&fs::read(path).map_err(display_error)?).map_err(display_error)
}

pub(crate) fn require_commit(value: &str, label: &str) -> Result<()> {
    if !is_git_commit(value) {
        return Err(Error::new(format!(
            "{label} mismatch: expected 40 or 64 lowercase hexadecimal characters, actual {value}"
        )));
    }
    Ok(())
}

fn path_text<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    path.to_str()
        .ok_or_else(|| Error::new(format!("{label} mismatch: expected UTF-8 path")))
}

fn run_success(
    processes: &ProcessEnvironment,
    root: &Path,
    program: &str,
    args: &[&str],
) -> Result<()> {
    run_output(processes, root, program, args).map(|_| ())
}

pub(crate) fn run_success_owned(
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
    if !output.status.success() {
        return Err(Error::new(format!(
            "{program} command mismatch: expected success, actual {}",
            output.status
        )));
    }
    Ok(())
}

fn run_stdout(
    processes: &ProcessEnvironment,
    root: &Path,
    program: &str,
    args: &[&str],
) -> Result<String> {
    String::from_utf8(run_output(processes, root, program, args)?.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(display_error)
}

fn run_output(
    processes: &ProcessEnvironment,
    root: &Path,
    program: &str,
    args: &[&str],
) -> Result<Output> {
    let output = processes
        .command(program)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "{program} command mismatch: expected success, actual {}",
            output.status
        )));
    }
    Ok(output)
}
