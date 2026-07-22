// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Offline validation and deterministic rendering for Rust release manifests.

use chrono::DateTime;
use flate2::bufread::GzDecoder as BufGzDecoder;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Cursor, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use tar::{Archive, Entry, EntryType};
use xz2::read::XzDecoder;

mod candidate;
pub use candidate::*;
mod transaction;
pub use transaction::*;
mod transparency;
pub use transparency::*;

pub const SCHEMA_VERSION: u64 = 1;
pub const SCHEMA_SHA256: &str = "d4eabf52bcc68b56945912d351f818e5444fe8c6461cb5c48b096f87b17a875c";
pub const CHECKSUM_NAME: &str = "SHA256SUMS";
pub const PRODUCT: &str = "solstone-linux";
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";
pub const CARGO_DENY_VERSION: &str = "0.20.2";
pub(crate) const CONTEXT_BINDING_NAME: &str = ".release-context.json";
pub(crate) const CONTEXT_ARCHIVE_NAME: &str = ".release-context.tar";
pub const MANIFEST_OK_MESSAGE: &str =
    "Named manifest and artifacts verified; this is NOT candidate-readiness classification.";
pub const RELEASE_DIR_OK_MESSAGE: &str =
    "Release directory verified as a complete five-file candidate.";
pub const LEDGER_SCHEMA_SHA256: &str =
    "4b387f19d8018752c6d016a4c0c74343ed80d2b64a3ff9480aa75b04fa66882d";
pub const PROOF_SCHEMA_SHA256: &str =
    "3009eab983eea832961220406f19c7459ed1db7fffc352af6ffaf664f9cd7dcf";

pub fn manifest_name(version: &str) -> String {
    format!("solstone-linux-{version}-linux-x86_64.rust-release-manifest.json")
}

const SCHEMA_BYTES: &[u8] =
    include_bytes!("../../../vendor/rust-release-manifest/rust-release-manifest.schema.json");
const LEDGER_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../vendor/rust-release-candidate-ledger/rust-release-candidate-ledger.schema.json"
);
const PROOF_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../vendor/rust-release-candidate-proof/rust-release-candidate-proof.schema.json"
);
const EXPECTED_LAYOUT: [&str; 9] = [
    "Cargo.toml",
    "Cargo.lock",
    "deny.toml",
    "Makefile",
    "rust-toolchain.toml",
    "packaging/Containerfile",
    "packaging/release-policy.toml",
    "crates/solstone-linux/Cargo.toml",
    "crates/rust-release-manifest/Cargo.toml",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSource {
    Host,
    Ubuntu,
    Fedora,
    BothLanes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolSpec {
    pub key: &'static str,
    pub source: ToolSource,
}

pub(crate) const TOOL_SPECS: [ToolSpec; 18] = [
    ToolSpec {
        key: "cargo_deb",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "cargo_generate_rpm",
        source: ToolSource::Fedora,
    },
    ToolSpec {
        key: "container_engine",
        source: ToolSource::Host,
    },
    ToolSpec {
        key: "dpkg_deb",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "fedora_image_digest",
        source: ToolSource::Fedora,
    },
    ToolSpec {
        key: "fedora_os",
        source: ToolSource::Fedora,
    },
    ToolSpec {
        key: "manifest_validator",
        source: ToolSource::Host,
    },
    ToolSpec {
        key: "rpm",
        source: ToolSource::Fedora,
    },
    ToolSpec {
        key: "signing_mode",
        source: ToolSource::BothLanes,
    },
    ToolSpec {
        key: "ubuntu_cargo",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_compiler",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_glibc",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_gzip",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_image_digest",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_linker",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_os",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_rustc",
        source: ToolSource::Ubuntu,
    },
    ToolSpec {
        key: "ubuntu_tar",
        source: ToolSource::Ubuntu,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofSpec {
    pub id: &'static str,
    pub artifact_kind: &'static str,
    pub architecture: &'static str,
}

pub const PROOF_SPECS: [ProofSpec; 3] = [
    ProofSpec {
        id: "debian-amd64",
        artifact_kind: "deb",
        architecture: "amd64",
    },
    ProofSpec {
        id: "rpm-x86_64",
        artifact_kind: "rpm",
        architecture: "x86_64",
    },
    ProofSpec {
        id: "tar-x86_64",
        artifact_kind: "tar",
        architecture: "x86_64",
    },
];

pub fn proof_spec(id: &str) -> Result<ProofSpec> {
    PROOF_SPECS
        .into_iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| Error::new("proof platform mismatch: expected known proof ID"))
}
#[derive(Debug)]
pub struct Error(String);

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

pub(crate) fn sanitize_process_stderr(bytes: &[u8]) -> String {
    const LIMIT: usize = 4096;
    let mut sanitized = String::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if index > 0 && !sanitized.is_empty() {
            sanitized.push('\n');
        }
        for byte in line {
            let value = match byte {
                b' '..=b'~' => char::from(*byte).to_string(),
                b'\t' => " ".into(),
                other => format!("\\x{other:02x}"),
            };
            if sanitized.len() + value.len() > LIMIT {
                sanitized.push_str("...");
                return sanitized;
            }
            sanitized.push_str(&value);
        }
    }
    sanitized.trim().to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn same_file_identity(metadata: &fs::Metadata, identity: FileIdentity) -> bool {
    metadata.dev() == identity.device && metadata.ino() == identity.inode
}

#[derive(Clone, Debug)]
pub struct RepoRoot {
    path: PathBuf,
    identity: FileIdentity,
}

impl RepoRoot {
    pub fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir().map_err(display_error)?;
        let value = command(&cwd, &["git", "rev-parse", "--show-toplevel"]).map_err(|error| {
            Error::new(format!(
                "repository root mismatch: expected solstone-linux Git checkout, actual {error}\nrepair: run from the expected solstone-linux checkout"
            ))
        })?;
        Self::validate_path(Path::new(&value)).map_err(|error| {
            Error::new(format!(
                "{error}\nrepair: run from the expected solstone-linux checkout"
            ))
        })
    }

    fn validate_path(path: &Path) -> Result<Self> {
        let root = path.canonicalize().map_err(display_error)?;
        for relative in EXPECTED_LAYOUT {
            require_regular(&root.join(relative), relative)?;
        }
        let workspace: toml::Value =
            toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).map_err(display_error)?)
                .map_err(display_error)?;
        let members = workspace["workspace"]["members"]
            .as_array()
            .ok_or_else(|| {
                Error::new("workspace layout mismatch: expected members, actual missing")
            })?
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from(["crates/solstone-linux", "crates/rust-release-manifest"]);
        if !expected.is_subset(&members) {
            return Err(Error::new(
                "workspace layout mismatch: expected release workspace members, actual incomplete",
            ));
        }
        let metadata = fs::symlink_metadata(&root).map_err(display_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::new(
                "repository root mismatch: expected no-follow directory, actual other",
            ));
        }
        Ok(Self {
            path: root,
            identity: FileIdentity::from_metadata(&metadata),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionComponent(String);

impl VersionComponent {
    pub(crate) fn new(value: &str) -> Result<Self> {
        validate_version(value)?;
        portable_path_component(value)?;
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TransactionComponent(pub(crate) String);

impl TransactionComponent {
    pub(crate) fn new(value: &str) -> Result<Self> {
        portable_path_component(value)?;
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::new(
                "candidate transaction mismatch: expected portable identifier, actual invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProofId(String);

impl ProofId {
    pub(crate) fn new(value: &str) -> Result<Self> {
        proof_spec(value)?;
        portable_path_component(value)?;
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReservedPath {
    Dist,
    Payload,
    EvidenceParent,
    EvidenceVersion(VersionComponent),
    EvidenceLedger(VersionComponent),
    Lock,
    StagingParent,
    StagingInvocation(TransactionComponent),
    StagingContext(TransactionComponent),
    StagingDebLane(TransactionComponent),
    StagingRpmLane(TransactionComponent),
    StagingProofRunner(TransactionComponent),
    StagingAdvisoryDb(TransactionComponent),
    StagingPayload(TransactionComponent),
    Proofs(VersionComponent),
    ProofRunner(VersionComponent),
    Proof(VersionComponent, ProofId),
    ProofAttempt(VersionComponent, ProofId, TransactionComponent),
    ProofAttemptOutput(VersionComponent, ProofId, TransactionComponent),
    Quarantine(Box<ReservedPath>, TransactionComponent),
}

impl ReservedPath {
    pub(crate) fn components(&self) -> Vec<OsString> {
        let fixed = |items: &[&str]| items.iter().map(OsString::from).collect();
        match self {
            Self::Dist => fixed(&["dist"]),
            Self::Payload => fixed(&["dist", "rust"]),
            Self::EvidenceParent => fixed(&["dist", "rust-evidence"]),
            Self::EvidenceVersion(version) => {
                let mut value = fixed(&["dist", "rust-evidence"]);
                value.push((&version.0).into());
                value
            }
            Self::EvidenceLedger(version) => {
                let mut value = Self::EvidenceVersion(version.clone()).components();
                value.push("ledger.json".into());
                value
            }
            Self::Lock => fixed(&["dist", ".rust-release-candidate.lock"]),
            Self::StagingParent => fixed(&["dist", ".rust-release-candidate-staging"]),
            Self::StagingInvocation(transaction) => {
                let mut value = Self::StagingParent.components();
                value.push((&transaction.0).into());
                value
            }
            Self::StagingContext(transaction) => {
                child(Self::StagingInvocation(transaction.clone()), "context")
            }
            Self::StagingDebLane(transaction) => {
                child(Self::StagingInvocation(transaction.clone()), "lane-deb")
            }
            Self::StagingRpmLane(transaction) => {
                child(Self::StagingInvocation(transaction.clone()), "lane-rpm")
            }
            Self::StagingProofRunner(transaction) => {
                child(Self::StagingInvocation(transaction.clone()), "proof-runner")
            }
            Self::StagingAdvisoryDb(transaction) => {
                child(Self::StagingInvocation(transaction.clone()), "advisory-db")
            }
            Self::StagingPayload(transaction) => {
                child(Self::StagingInvocation(transaction.clone()), "payload")
            }
            Self::Proofs(version) => child(Self::EvidenceVersion(version.clone()), "proofs"),
            Self::ProofRunner(version) => {
                child(Self::EvidenceVersion(version.clone()), "proof-runner")
            }
            Self::Proof(version, proof) => {
                child(Self::Proofs(version.clone()), &format!("{}.json", proof.0))
            }
            Self::ProofAttempt(version, proof, transaction) => child(
                Self::Proofs(version.clone()),
                &format!(".{}.{}.attempt", proof.0, transaction.0),
            ),
            Self::ProofAttemptOutput(version, proof, transaction) => child(
                Self::ProofAttempt(version.clone(), proof.clone(), transaction.clone()),
                "proof.json",
            ),
            Self::Quarantine(path, transaction) => {
                let mut value = path.components();
                let name = value.pop().expect("reserved paths are nonempty");
                value.push(
                    format!(".{}.{}.quarantine", name.to_string_lossy(), transaction.0).into(),
                );
                value
            }
        }
    }

    pub(crate) fn relative(&self) -> PathBuf {
        self.components().into_iter().collect()
    }

    #[cfg(test)]
    pub(crate) fn test_cases(
        version: VersionComponent,
        transaction: TransactionComponent,
    ) -> Vec<ReservedPathCase> {
        let proof = ProofId::new("debian-amd64").expect("fixed proof ID");
        let paths = vec![
            Self::Dist,
            Self::Payload,
            Self::EvidenceParent,
            Self::EvidenceVersion(version.clone()),
            Self::EvidenceLedger(version.clone()),
            Self::Lock,
            Self::StagingParent,
            Self::StagingInvocation(transaction.clone()),
            Self::StagingContext(transaction.clone()),
            Self::StagingDebLane(transaction.clone()),
            Self::StagingRpmLane(transaction.clone()),
            Self::StagingProofRunner(transaction.clone()),
            Self::StagingAdvisoryDb(transaction.clone()),
            Self::StagingPayload(transaction.clone()),
            Self::Proofs(version.clone()),
            Self::ProofRunner(version.clone()),
            Self::Proof(version.clone(), proof.clone()),
            Self::ProofAttempt(version.clone(), proof.clone(), transaction.clone()),
            Self::ProofAttemptOutput(version, proof, transaction.clone()),
            Self::Quarantine(Box::new(Self::Payload), transaction),
        ];
        paths
            .into_iter()
            .map(|path| {
                let expected = match &path {
                    Self::EvidenceLedger(_)
                    | Self::Lock
                    | Self::Proof(_, _)
                    | Self::ProofRunner(_)
                    | Self::ProofAttemptOutput(_, _, _) => ExpectedLeaf::RegularFile,
                    Self::Dist
                    | Self::Payload
                    | Self::EvidenceParent
                    | Self::EvidenceVersion(_)
                    | Self::StagingParent
                    | Self::StagingInvocation(_)
                    | Self::StagingContext(_)
                    | Self::StagingDebLane(_)
                    | Self::StagingRpmLane(_)
                    | Self::StagingProofRunner(_)
                    | Self::StagingAdvisoryDb(_)
                    | Self::StagingPayload(_)
                    | Self::Proofs(_)
                    | Self::ProofAttempt(_, _, _)
                    | Self::Quarantine(_, _) => ExpectedLeaf::Directory,
                };
                let action = match &path {
                    Self::Dist
                    | Self::Payload
                    | Self::EvidenceParent
                    | Self::EvidenceVersion(_)
                    | Self::StagingParent
                    | Self::StagingInvocation(_)
                    | Self::StagingContext(_)
                    | Self::StagingDebLane(_)
                    | Self::StagingRpmLane(_)
                    | Self::StagingProofRunner(_)
                    | Self::StagingAdvisoryDb(_)
                    | Self::StagingPayload(_) => ReservedPathAction::Create,
                    Self::ProofRunner(_) => ReservedPathAction::Create,
                    Self::EvidenceLedger(_) | Self::Proofs(_) | Self::Proof(_, _) => {
                        ReservedPathAction::Status
                    }
                    Self::Lock => ReservedPathAction::Recover,
                    Self::ProofAttempt(_, _, _) | Self::ProofAttemptOutput(_, _, _) => {
                        ReservedPathAction::Prove
                    }
                    Self::Quarantine(_, _) => ReservedPathAction::Unreachable(
                        "quarantine names are transaction-scoped cleanup internals and are reached only after a production command has atomically renamed an owned target",
                    ),
                };
                ReservedPathCase {
                    path,
                    expected,
                    action,
                }
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) struct ReservedPathCase {
    pub(crate) path: ReservedPath,
    pub(crate) expected: ExpectedLeaf,
    pub(crate) action: ReservedPathAction,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReservedPathAction {
    Create,
    Prove,
    Recover,
    Status,
    Unreachable(&'static str),
}

fn child(parent: ReservedPath, name: &str) -> Vec<OsString> {
    let mut value = parent.components();
    value.push(name.into());
    value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedLeaf {
    Any,
    Absent,
    Directory,
    RegularFile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataState {
    Absent,
    Present(FileIdentity, bool, bool),
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedReservedPath {
    pub(crate) relative: PathBuf,
    pub(crate) absolute: PathBuf,
    pub(crate) parent_identity: FileIdentity,
    pub(crate) identity: Option<FileIdentity>,
}

pub(crate) enum ReservedPathPresence {
    Absent(ResolvedReservedPath),
    Present(ResolvedReservedPath),
}

impl ReservedPathPresence {
    pub(crate) fn resolved(self) -> ResolvedReservedPath {
        match self {
            Self::Absent(path) | Self::Present(path) => path,
        }
    }
}

pub(crate) struct ReservedReleaseBoundary<'a> {
    root: &'a RepoRoot,
}

impl<'a> ReservedReleaseBoundary<'a> {
    pub(crate) fn new(root: &'a RepoRoot) -> Self {
        Self { root }
    }

    fn metadata(&self, path: &Path, relative: &Path) -> Result<MetadataState> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(MetadataState::Present(
                FileIdentity::from_metadata(&metadata),
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                metadata.is_file() && !metadata.file_type().is_symlink(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MetadataState::Absent),
            Err(error) => Err(Error::new(format!(
                "release boundary metadata mismatch: expected readable {}, actual {:?} error\nrepair: inspect only the named reserved path",
                relative.display(),
                error.kind()
            ))),
        }
    }

    fn resolve_presence(
        &self,
        reserved: ReservedPath,
        expected: ExpectedLeaf,
        allow_absent: bool,
    ) -> Result<ReservedPathPresence> {
        let root_metadata = fs::symlink_metadata(self.root.path()).map_err(display_error)?;
        if !root_metadata.is_dir() || !same_file_identity(&root_metadata, self.root.identity) {
            return Err(Error::new(
                "release boundary anchor mismatch: expected original checkout directory, actual replaced",
            ));
        }
        let components = reserved.components();
        let relative: PathBuf = components.iter().collect();
        let mut current = self.root.path().to_owned();
        let mut parent_identity = self.root.identity;
        for (index, component) in components.iter().enumerate() {
            portable_path_component(&component.to_string_lossy())?;
            let parent_before = fs::symlink_metadata(&current).map_err(display_error)?;
            if !parent_before.is_dir() || !same_file_identity(&parent_before, parent_identity) {
                return Err(boundary_type_error(
                    &relative,
                    "stable directory ancestor",
                    "replaced",
                    false,
                ));
            }
            current.push(component);
            let component_relative: PathBuf = components[..=index].iter().collect();
            let state = self.metadata(&current, &component_relative)?;
            let parent_after =
                fs::symlink_metadata(current.parent().unwrap()).map_err(display_error)?;
            if !same_file_identity(&parent_after, parent_identity) {
                return Err(boundary_type_error(
                    &relative,
                    "stable directory ancestor",
                    "replaced",
                    false,
                ));
            }
            let leaf = index + 1 == components.len();
            match (leaf, state) {
                (false, MetadataState::Present(identity, true, _)) => parent_identity = identity,
                (false, MetadataState::Absent) if allow_absent => {
                    current.extend(&components[index + 1..]);
                    return Ok(ReservedPathPresence::Absent(ResolvedReservedPath {
                        relative,
                        absolute: current,
                        parent_identity,
                        identity: None,
                    }));
                }
                (false, MetadataState::Absent) => {
                    return Err(boundary_type_error(
                        &component_relative,
                        "directory",
                        "absent",
                        false,
                    ));
                }
                (false, _) => {
                    return Err(boundary_type_error(
                        &component_relative,
                        "directory",
                        "non-directory or symlink",
                        false,
                    ));
                }
                (true, MetadataState::Absent)
                    if expected == ExpectedLeaf::Absent || allow_absent =>
                {
                    return Ok(ReservedPathPresence::Absent(ResolvedReservedPath {
                        relative,
                        absolute: current,
                        parent_identity,
                        identity: None,
                    }));
                }
                (true, MetadataState::Present(identity, true, _))
                    if expected == ExpectedLeaf::Directory =>
                {
                    return Ok(ReservedPathPresence::Present(ResolvedReservedPath {
                        relative,
                        absolute: current,
                        parent_identity,
                        identity: Some(identity),
                    }));
                }
                (true, MetadataState::Present(identity, _, true))
                    if expected == ExpectedLeaf::RegularFile =>
                {
                    return Ok(ReservedPathPresence::Present(ResolvedReservedPath {
                        relative,
                        absolute: current,
                        parent_identity,
                        identity: Some(identity),
                    }));
                }
                (true, MetadataState::Present(identity, _, _)) if expected == ExpectedLeaf::Any => {
                    return Ok(ReservedPathPresence::Present(ResolvedReservedPath {
                        relative,
                        absolute: current,
                        parent_identity,
                        identity: Some(identity),
                    }));
                }
                (true, MetadataState::Absent) => {
                    return Err(boundary_type_error(
                        &relative,
                        leaf_name(expected),
                        "absent",
                        false,
                    ));
                }
                (true, _) => {
                    return Err(boundary_type_error(
                        &relative,
                        leaf_name(expected),
                        "wrong type or symlink",
                        false,
                    ));
                }
            }
        }
        unreachable!()
    }

    pub(crate) fn presence(
        &self,
        path: ReservedPath,
        leaf: ExpectedLeaf,
    ) -> Result<ReservedPathPresence> {
        self.resolve_presence(path, leaf, true)
    }

    pub(crate) fn resolve_for_read(
        &self,
        path: ReservedPath,
        leaf: ExpectedLeaf,
    ) -> Result<ResolvedReservedPath> {
        match self.resolve_presence(path, leaf, false)? {
            ReservedPathPresence::Present(path) | ReservedPathPresence::Absent(path) => Ok(path),
        }
    }
    pub(crate) fn resolve_for_create(
        &self,
        path: ReservedPath,
        leaf: ExpectedLeaf,
    ) -> Result<ResolvedReservedPath> {
        match self.resolve_presence(path, leaf, false)? {
            ReservedPathPresence::Present(path) | ReservedPathPresence::Absent(path) => Ok(path),
        }
    }
    pub(crate) fn resolve_for_replace(
        &self,
        path: ReservedPath,
        leaf: ExpectedLeaf,
        expected: FileIdentity,
    ) -> Result<ResolvedReservedPath> {
        self.resolve_identity(path, leaf, expected)
    }
    pub(crate) fn resolve_for_delete(
        &self,
        path: ReservedPath,
        leaf: ExpectedLeaf,
        expected: FileIdentity,
    ) -> Result<ResolvedReservedPath> {
        self.resolve_identity(path, leaf, expected)
    }

    fn resolve_identity(
        &self,
        path: ReservedPath,
        leaf: ExpectedLeaf,
        expected: FileIdentity,
    ) -> Result<ResolvedReservedPath> {
        let resolved = match self.resolve_presence(path, leaf, false)? {
            ReservedPathPresence::Present(path) | ReservedPathPresence::Absent(path) => path,
        };
        if resolved.identity != Some(expected) {
            return Err(boundary_type_error(
                &resolved.relative,
                "owned identity",
                "replaced",
                false,
            ));
        }
        Ok(resolved)
    }
}

fn leaf_name(expected: ExpectedLeaf) -> &'static str {
    match expected {
        ExpectedLeaf::Any => "reserved leaf",
        ExpectedLeaf::Absent => "absent leaf",
        ExpectedLeaf::Directory => "directory",
        ExpectedLeaf::RegularFile => "regular file",
    }
}

fn boundary_type_error(path: &Path, expected: &str, actual: &str, cleanup_begun: bool) -> Error {
    Error::new(format!(
        "release boundary {} mismatch: expected {expected}, actual {actual}; cleanup begun: {cleanup_begun}\nrepair: inspect only the named reserved path",
        path.display()
    ))
}

#[derive(Clone, Debug)]
pub(crate) struct CleanupEntry {
    pub(crate) path: ReservedPath,
    pub(crate) expected_type: ExpectedLeaf,
    pub(crate) expected_identity: FileIdentity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CleanupReport {
    pub(crate) attempted: Vec<ReservedPath>,
    pub(crate) deleted: Vec<ReservedPath>,
    pub(crate) preserved: Vec<ReservedPath>,
    pub(crate) residual: Vec<ReservedPath>,
}

pub(crate) struct CleanupPlan {
    entries: Vec<CleanupEntry>,
    transaction: TransactionComponent,
}

pub(crate) struct ValidatedCleanupPlan<'a> {
    boundary: ReservedReleaseBoundary<'a>,
    entries: Vec<CleanupEntry>,
    transaction: TransactionComponent,
}

impl CleanupPlan {
    pub(crate) fn new(entries: Vec<CleanupEntry>) -> Result<Self> {
        Ok(Self {
            entries,
            transaction: TransactionComponent::new(&transaction_id()?)?,
        })
    }

    pub(crate) fn preflight<'a>(
        self,
        boundary: ReservedReleaseBoundary<'a>,
    ) -> Result<ValidatedCleanupPlan<'a>> {
        let mut names = BTreeSet::new();
        for entry in &self.entries {
            let relative = entry.path.relative();
            if !names.insert(relative.clone()) {
                return Err(boundary_type_error(
                    &relative,
                    "unique cleanup target",
                    "duplicate",
                    false,
                ));
            }
            if names.iter().any(|other| {
                other != &relative && (other.starts_with(&relative) || relative.starts_with(other))
            }) {
                return Err(boundary_type_error(
                    &relative,
                    "non-overlapping cleanup target",
                    "ancestor or descendant overlap",
                    false,
                ));
            }
            boundary.resolve_for_delete(
                entry.path.clone(),
                entry.expected_type,
                entry.expected_identity,
            )?;
        }
        Ok(ValidatedCleanupPlan {
            boundary,
            entries: self.entries,
            transaction: self.transaction,
        })
    }

    pub(crate) fn finish_error(
        self,
        boundary: ReservedReleaseBoundary<'_>,
        original: Error,
    ) -> Error {
        match self
            .preflight(boundary)
            .and_then(ValidatedCleanupPlan::execute)
        {
            Ok(_) => original,
            Err(cleanup) => Error::new(format!("{original}\n{cleanup}")),
        }
    }
}

impl ValidatedCleanupPlan<'_> {
    pub(crate) fn execute(self) -> Result<CleanupReport> {
        self.execute_with(|_, _| Ok(()), |_, _| Ok(()))
    }

    pub(crate) fn execute_with(
        self,
        mut final_barrier: impl FnMut(&ReservedPath, &Path) -> Result<()>,
        mut quarantine_barrier: impl FnMut(&ReservedPath, &Path) -> Result<()>,
    ) -> Result<CleanupReport> {
        let mut report = CleanupReport::default();
        for (index, entry) in self.entries.iter().enumerate() {
            report.attempted.push(entry.path.clone());
            let mut residual_path = entry.path.clone();
            let result = (|| {
                let resolved = self.boundary.resolve_for_delete(
                    entry.path.clone(),
                    entry.expected_type,
                    entry.expected_identity,
                )?;
                final_barrier(&entry.path, &resolved.absolute)?;
                let resolved = self.boundary.resolve_for_delete(
                    entry.path.clone(),
                    entry.expected_type,
                    entry.expected_identity,
                )?;
                let parent = fs::symlink_metadata(
                    resolved
                        .absolute
                        .parent()
                        .expect("reserved path has parent"),
                )
                .map_err(display_error)?;
                if !same_file_identity(&parent, resolved.parent_identity) {
                    return Err(boundary_type_error(
                        &resolved.relative,
                        "stable parent identity",
                        "replaced",
                        true,
                    ));
                }
                let quarantine = ReservedPath::Quarantine(
                    Box::new(entry.path.clone()),
                    self.transaction.clone(),
                );
                let quarantine_path = self
                    .boundary
                    .resolve_for_create(quarantine.clone(), ExpectedLeaf::Absent)?
                    .absolute;
                // Safe std pathname APIs leave a transient rename exposure to a hostile
                // privileged parent replacement; parent-FD syscalls are out of scope.
                fs::rename(&resolved.absolute, &quarantine_path).map_err(display_error)?;
                residual_path = quarantine.clone();
                quarantine_barrier(&quarantine, &quarantine_path)?;
                let quarantined = self.boundary.resolve_for_delete(
                    quarantine.clone(),
                    entry.expected_type,
                    entry.expected_identity,
                )?;
                match entry.expected_type {
                    ExpectedLeaf::Directory => {
                        fs::remove_dir_all(&quarantined.absolute).map_err(display_error)?
                    }
                    ExpectedLeaf::RegularFile => {
                        fs::remove_file(&quarantined.absolute).map_err(display_error)?
                    }
                    ExpectedLeaf::Any | ExpectedLeaf::Absent => {
                        unreachable!("cleanup entries have exact types")
                    }
                }
                report.deleted.push(entry.path.clone());
                Ok(())
            })();
            if let Err(error) = result {
                report.residual.push(residual_path);
                report.preserved.extend(
                    self.entries[index + 1..]
                        .iter()
                        .map(|item| item.path.clone()),
                );
                return Err(cleanup_error(error, &report));
            }
        }
        Ok(report)
    }
}

fn cleanup_error(error: Error, report: &CleanupReport) -> Error {
    let names = |paths: &[ReservedPath]| {
        paths
            .iter()
            .map(|path| path.relative().display().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    Error::new(format!(
        "{error}\ncleanup incomplete: attempted=[{}]; deleted=[{}]; preserved=[{}]; residual=[{}]\nrepair: inspect only the preserved and residual reserved paths",
        names(&report.attempted),
        names(&report.deleted),
        names(&report.preserved),
        names(&report.residual)
    ))
}

#[derive(Debug)]
pub struct CandidateLock {
    path: PathBuf,
    file: File,
    released: bool,
}

impl CandidateLock {
    pub fn acquire(root: &RepoRoot) -> Result<Self> {
        let boundary = ReservedReleaseBoundary::new(root);
        match boundary.presence(ReservedPath::Dist, ExpectedLeaf::Directory)? {
            ReservedPathPresence::Present(path) => path.absolute,
            ReservedPathPresence::Absent(path) => {
                let dist = path.absolute;
                fs::create_dir(&dist).map_err(display_error)?;
                boundary.resolve_for_read(ReservedPath::Dist, ExpectedLeaf::Directory)?;
                dist
            }
        };
        let path = boundary
            .resolve_for_create(ReservedPath::Lock, ExpectedLeaf::Absent)?
            .absolute;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| {
                Error::new(format!(
                    "release candidate lock mismatch: expected exclusive owner, actual {error}\nrepair: confirm no candidate process is running, then remove only dist/.rust-release-candidate.lock"
                ))
            })?;
        Ok(Self {
            path,
            file,
            released: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn release(mut self) -> Result<()> {
        self.remove_owned()?;
        self.released = true;
        Ok(())
    }

    fn remove_owned(&self) -> Result<()> {
        let owned = self.file.metadata().map_err(display_error)?;
        let metadata = fs::symlink_metadata(&self.path).map_err(display_error)?;
        if metadata.dev() != owned.dev() || metadata.ino() != owned.ino() {
            return Err(Error::new(
                "release candidate lock cleanup mismatch: expected owned lock, actual replaced\nrepair: inspect dist/.rust-release-candidate.lock and remove it only after confirming no candidate process is running",
            ));
        }
        fs::remove_file(&self.path).map_err(|error| {
            Error::new(format!(
                "release candidate lock cleanup mismatch: expected owned lock removed, actual {error}\nrepair: remove only dist/.rust-release-candidate.lock after confirming no candidate process is running"
            ))
        })
    }
}

impl Drop for CandidateLock {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let owned = self.file.metadata();
        let same_file = fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            owned
                .as_ref()
                .is_ok_and(|owned| metadata.dev() == owned.dev() && metadata.ino() == owned.ino())
        });
        if same_file {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
pub struct StagingLayout {
    pub root: PathBuf,
    pub context: PathBuf,
    pub deb_lane: PathBuf,
    pub rpm_lane: PathBuf,
    pub proof_runner: PathBuf,
    pub advisory_db: PathBuf,
    pub payload: PathBuf,
    pub(crate) transaction: Option<TransactionComponent>,
    pub(crate) root_identity: FileIdentity,
}

impl StagingLayout {
    pub fn create(root: &RepoRoot, _lock: &CandidateLock) -> Result<Self> {
        let transaction = TransactionComponent::new(&transaction_id()?)?;
        let boundary = ReservedReleaseBoundary::new(root);
        match boundary.presence(ReservedPath::StagingParent, ExpectedLeaf::Directory)? {
            ReservedPathPresence::Present(path) => path.absolute,
            ReservedPathPresence::Absent(path) => {
                fs::create_dir(&path.absolute).map_err(display_error)?;
                path.absolute
            }
        };
        let staging = boundary
            .resolve_for_create(
                ReservedPath::StagingInvocation(transaction.clone()),
                ExpectedLeaf::Absent,
            )?
            .absolute;
        fs::create_dir(&staging).map_err(display_error)?;
        let root_identity = boundary
            .resolve_for_read(
                ReservedPath::StagingInvocation(transaction.clone()),
                ExpectedLeaf::Directory,
            )?
            .identity
            .expect("present staging identity");
        #[cfg(test)]
        run_staging_test_barrier(root, &transaction);
        Self::initialize_reserved(root, staging, transaction, root_identity)
    }

    pub(crate) fn initialize_reserved(
        root: &RepoRoot,
        staging: PathBuf,
        transaction: TransactionComponent,
        root_identity: FileIdentity,
    ) -> Result<Self> {
        let boundary = ReservedReleaseBoundary::new(root);
        boundary.resolve_for_replace(
            ReservedPath::StagingInvocation(transaction.clone()),
            ExpectedLeaf::Directory,
            root_identity,
        )?;
        let layout = Self {
            context: boundary
                .presence(
                    ReservedPath::StagingContext(transaction.clone()),
                    ExpectedLeaf::Any,
                )?
                .resolved()
                .absolute,
            deb_lane: boundary
                .presence(
                    ReservedPath::StagingDebLane(transaction.clone()),
                    ExpectedLeaf::Any,
                )?
                .resolved()
                .absolute,
            rpm_lane: boundary
                .presence(
                    ReservedPath::StagingRpmLane(transaction.clone()),
                    ExpectedLeaf::Any,
                )?
                .resolved()
                .absolute,
            proof_runner: boundary
                .presence(
                    ReservedPath::StagingProofRunner(transaction.clone()),
                    ExpectedLeaf::Any,
                )?
                .resolved()
                .absolute,
            advisory_db: boundary
                .presence(
                    ReservedPath::StagingAdvisoryDb(transaction.clone()),
                    ExpectedLeaf::Any,
                )?
                .resolved()
                .absolute,
            payload: boundary
                .presence(
                    ReservedPath::StagingPayload(transaction.clone()),
                    ExpectedLeaf::Any,
                )?
                .resolved()
                .absolute,
            root: staging,
            transaction: Some(transaction.clone()),
            root_identity,
        };
        let setup = (|| {
            for (reserved, directory) in [
                (
                    ReservedPath::StagingContext(transaction.clone()),
                    &layout.context,
                ),
                (
                    ReservedPath::StagingDebLane(transaction.clone()),
                    &layout.deb_lane,
                ),
                (
                    ReservedPath::StagingRpmLane(transaction.clone()),
                    &layout.rpm_lane,
                ),
                (
                    ReservedPath::StagingProofRunner(transaction.clone()),
                    &layout.proof_runner,
                ),
                (
                    ReservedPath::StagingAdvisoryDb(transaction.clone()),
                    &layout.advisory_db,
                ),
                (
                    ReservedPath::StagingPayload(transaction.clone()),
                    &layout.payload,
                ),
            ] {
                boundary.resolve_for_create(reserved, ExpectedLeaf::Absent)?;
                fs::create_dir(directory).map_err(display_error)?;
            }
            Ok(())
        })();
        match setup {
            Ok(()) => Ok(layout),
            Err(primary) => {
                let cleanup = CleanupPlan::new(vec![CleanupEntry {
                    path: ReservedPath::StagingInvocation(transaction),
                    expected_type: ExpectedLeaf::Directory,
                    expected_identity: root_identity,
                }])
                .and_then(|plan| plan.preflight(boundary))
                .and_then(ValidatedCleanupPlan::execute);
                match cleanup {
                    Ok(_) => Err(primary),
                    Err(cleanup) => Err(Error::new(format!("{primary}\n{cleanup}"))),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImmutableContext {
    pub commit: String,
    pub archive_sha256: String,
    pub cargo_lock_sha256: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextBinding {
    pub source_commit: String,
    pub source_archive_sha256: String,
    pub cargo_lock_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Evidence {
    pub schema_version: u64,
    pub product: String,
    pub version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub cargo_lock_sha256: String,
    pub rust: RustEvidence,
    pub target: TargetEvidence,
    pub native_tools: BTreeMap<String, String>,
    pub dependency_policy: DependencyPolicy,
    pub active_exceptions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RustEvidence {
    pub rustc_verbose: String,
    pub cargo_version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TargetEvidence {
    Compiled {
        triple: String,
        profile: String,
        features: Vec<String>,
    },
    Source,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DependencyPolicy {
    pub cargo_deny_version: String,
    pub deterministic_gate: String,
    pub advisory_checked_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub schema_version: u64,
    pub product: String,
    pub version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub cargo_lock_sha256: String,
    pub rust: RustEvidence,
    pub target: TargetEvidence,
    pub native_tools: BTreeMap<String, String>,
    pub dependency_policy: DependencyPolicy,
    pub active_exceptions: Vec<String>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, PartialEq, Eq)]
struct PackageIdentity {
    name: String,
    version: String,
    release: Option<String>,
    arch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PackageMemberEvidence {
    pub package_file: String,
    pub format: String,
    pub installed_path: String,
    pub mode: u64,
    pub bytes: u64,
    pub sha256: String,
}

pub(crate) fn render_manifest(
    root: &RepoRoot,
    evidence: Evidence,
    release_dir: &Path,
) -> Result<String> {
    validate_evidence(root, &evidence)?;
    validate_version(&evidence.version)?;
    let artifacts = artifact_paths(release_dir, &evidence.version)?
        .into_iter()
        .map(|path| artifact(&path))
        .collect::<Result<Vec<_>>>()?;
    let manifest = Manifest {
        schema_version: evidence.schema_version,
        product: evidence.product,
        version: evidence.version,
        source_commit: evidence.source_commit,
        source_dirty: evidence.source_dirty,
        cargo_lock_sha256: evidence.cargo_lock_sha256,
        rust: evidence.rust,
        target: evidence.target,
        native_tools: evidence.native_tools,
        dependency_policy: evidence.dependency_policy,
        active_exceptions: evidence.active_exceptions,
        artifacts,
    };
    let mut output = serde_json::to_string_pretty(&manifest).map_err(display_error)?;
    output.push('\n');
    validate_manifest_bytes(root, output.as_bytes())?;
    Ok(output)
}

pub fn render_sha256sums(artifacts: &[Artifact]) -> Result<String> {
    let mut sorted = artifacts.to_vec();
    sorted.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    validate_artifact_set(&sorted)?;
    Ok(sorted
        .iter()
        .map(|item| format!("{}  {}\n", item.sha256, item.path))
        .collect())
}

pub fn validate_manifest_bytes(root: &RepoRoot, bytes: &[u8]) -> Result<Manifest> {
    verify_schema()?;
    let value: Value = serde_json::from_slice(bytes).map_err(display_error)?;
    let schema: Value = serde_json::from_slice(SCHEMA_BYTES).map_err(display_error)?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(display_error)?;
    if let Err(error) = validator.validate(&value) {
        return Err(Error::new(format!("manifest schema mismatch: {error}")));
    }
    let manifest: Manifest = serde_json::from_value(value).map_err(display_error)?;
    validate_manifest_policy(root, &manifest)?;
    Ok(manifest)
}

pub fn verify_manifest_mode(repo: &RepoRoot, path: &Path) -> Result<()> {
    verify_manifest(repo, path, true)
}

fn verify_manifest(repo: &RepoRoot, path: &Path, bind_live: bool) -> Result<()> {
    require_regular(path, "manifest")?;
    let root = path
        .parent()
        .ok_or_else(|| Error::new("manifest parent missing"))?;
    let manifest = validate_manifest_bytes(repo, &fs::read(path).map_err(display_error)?)?;
    let expected_name = manifest_name(&manifest.version);
    if path.file_name().and_then(OsStr::to_str) != Some(expected_name.as_str()) {
        return Err(Error::new(format!(
            "manifest basename mismatch: expected {expected_name}"
        )));
    }
    if bind_live {
        validate_live(repo, &manifest, root)?;
    }
    verify_artifacts(&manifest, root)?;
    verify_checksums(&manifest, root)
}

pub fn classify_release_dir(repo: &RepoRoot, root: &Path) -> Result<()> {
    classify_release(repo, root, true)
}

fn classify_release(repo: &RepoRoot, root: &Path, bind_live: bool) -> Result<()> {
    require_directory(root, "release root")?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(root).map_err(display_error)? {
        let entry = entry.map_err(display_error)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::new("release inventory contains non-UTF-8 path"))?;
        portable_path(&name)?;
        require_regular(&entry.path(), &name)?;
        if !names.insert(name.clone()) {
            return Err(Error::new(format!("duplicate release path: {name}")));
        }
    }
    let manifests = names
        .iter()
        .filter(|name| name.ends_with(".rust-release-manifest.json"))
        .collect::<Vec<_>>();
    if manifests.len() != 1 || names.len() != 5 {
        return Err(Error::new(format!(
            "release inventory mismatch: expected 5 files, actual {}",
            names.len()
        )));
    }
    let manifest_path = root.join(manifests[0]);
    verify_manifest(repo, &manifest_path, bind_live)
}

pub fn schema_bytes() -> &'static [u8] {
    SCHEMA_BYTES
}

pub fn ledger_schema_bytes() -> &'static [u8] {
    LEDGER_SCHEMA_BYTES
}

pub fn proof_schema_bytes() -> &'static [u8] {
    PROOF_SCHEMA_BYTES
}

pub fn verify_candidate_schemas() -> Result<()> {
    verify_pinned_schema(
        LEDGER_SCHEMA_BYTES,
        LEDGER_SCHEMA_SHA256,
        "https://solpbc.org/schemas/rust-release-candidate-ledger/v1.json",
        "ledger",
    )?;
    verify_pinned_schema(
        PROOF_SCHEMA_BYTES,
        PROOF_SCHEMA_SHA256,
        "https://solpbc.org/schemas/rust-release-candidate-proof/v1.json",
        "proof",
    )
}

pub fn canonical_json(value: &Value) -> Result<Vec<u8>> {
    fn normalize(value: &Value, field: Option<&str>) -> Result<Value> {
        match value {
            Value::Object(object) => Ok(Value::Object(
                object
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), normalize(value, Some(key))?)))
                    .collect::<Result<_>>()?,
            )),
            Value::Array(array) => {
                let mut normalized = array
                    .iter()
                    .map(|value| normalize(value, None))
                    .collect::<Result<Vec<_>>>()?;
                match field {
                    Some("features" | "active_exceptions" | "expected_proof_ids") => {
                        normalized.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
                    }
                    Some("payload") => normalized.sort_by(|left, right| {
                        left.get("path")
                            .and_then(Value::as_str)
                            .cmp(&right.get("path").and_then(Value::as_str))
                    }),
                    Some("package_members") => normalized.sort_by(|left, right| {
                        left.get("package_file")
                            .and_then(Value::as_str)
                            .cmp(&right.get("package_file").and_then(Value::as_str))
                    }),
                    _ => {}
                }
                Ok(Value::Array(normalized))
            }
            _ => Ok(value.clone()),
        }
    }

    let normalized = normalize(value, None)?;
    let mut bytes = serde_json::to_vec(&normalized).map_err(display_error)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn candidate_digest_input(payload: &[Artifact]) -> Result<Vec<u8>> {
    if payload.len() != 5 {
        return Err(Error::new(format!(
            "candidate payload mismatch: expected 5 files, actual {}",
            payload.len()
        )));
    }
    let mut payload = payload.to_vec();
    payload.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut previous: Option<&str> = None;
    let mut output = Vec::new();
    for file in &payload {
        portable_path(&file.path)?;
        if Path::new(&file.path).file_name() != Some(OsStr::new(&file.path)) {
            return Err(Error::new(
                "candidate payload path mismatch: expected basename, actual path",
            ));
        }
        if previous == Some(&file.path) {
            return Err(Error::new(
                "candidate payload path mismatch: expected unique, actual duplicate",
            ));
        }
        if !is_sha256(&file.sha256) || file.bytes == 0 {
            return Err(Error::new(format!(
                "candidate payload metadata mismatch: {}",
                file.path
            )));
        }
        previous = Some(&file.path);
        output.extend_from_slice(
            format!("{}  {}  {}\n", file.sha256, file.bytes, file.path).as_bytes(),
        );
    }
    Ok(output)
}

pub fn candidate_digest(payload: &[Artifact]) -> Result<String> {
    Ok(digest(&candidate_digest_input(payload)?))
}

#[derive(Serialize)]
struct BundleDigestInput<'a> {
    candidate_digest: &'a str,
    ledger_sha256: String,
    proofs: BTreeMap<&'a str, String>,
}

pub fn bundle_digest_input(
    candidate: &str,
    ledger: &[u8],
    proofs: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    if !is_sha256(candidate) {
        return Err(Error::new(
            "candidate digest mismatch: expected sha256, actual invalid",
        ));
    }
    let expected = PROOF_SPECS
        .iter()
        .map(|spec| spec.id)
        .collect::<BTreeSet<_>>();
    let actual = proofs.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(Error::new(format!(
            "proof inventory mismatch: expected 3 proof IDs, actual {}",
            actual.len()
        )));
    }
    let input = BundleDigestInput {
        candidate_digest: candidate,
        ledger_sha256: digest(ledger),
        proofs: proofs
            .iter()
            .map(|(id, bytes)| (id.as_str(), digest(bytes)))
            .collect(),
    };
    serde_json::to_vec(&input).map_err(display_error)
}

pub fn bundle_digest(
    candidate: &str,
    ledger: &[u8],
    proofs: &BTreeMap<String, Vec<u8>>,
) -> Result<String> {
    Ok(digest(&bundle_digest_input(candidate, ledger, proofs)?))
}

#[derive(Clone, Debug)]
pub struct ProofBindings {
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
    pub policy_checked_at: String,
    pub validation_time: String,
}

pub fn validate_candidate_proof(value: &Value, expected: &ProofBindings) -> Result<()> {
    verify_candidate_schemas()?;
    let schema: Value = serde_json::from_slice(PROOF_SCHEMA_BYTES).map_err(display_error)?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(display_error)?;
    if let Err(error) = validator.validate(value) {
        return Err(Error::new(format!("proof schema mismatch: {error}")));
    }
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("proof mismatch: expected object, actual other"))?;
    let expected_fields = [
        ("platform", Value::String(expected.platform.clone())),
        (
            "candidate_digest",
            Value::String(expected.candidate_digest.clone()),
        ),
        (
            "ledger_sha256",
            Value::String(expected.ledger_sha256.clone()),
        ),
        (
            "source_commit",
            Value::String(expected.source_commit.clone()),
        ),
        (
            "cargo_lock_sha256",
            Value::String(expected.cargo_lock_sha256.clone()),
        ),
        (
            "artifact_basename",
            Value::String(expected.artifact_basename.clone()),
        ),
        ("artifact_bytes", Value::from(expected.artifact_bytes)),
        (
            "artifact_sha256",
            Value::String(expected.artifact_sha256.clone()),
        ),
        (
            "proof_image_digest",
            Value::String(expected.proof_image_digest.clone()),
        ),
        ("os_release", Value::String(expected.os_release.clone())),
        (
            "package_manager_version",
            Value::String(expected.package_manager_version.clone()),
        ),
        (
            "install_command",
            serde_json::to_value(&expected.install_command).map_err(display_error)?,
        ),
        (
            "install_exit_status",
            Value::from(expected.install_exit_status),
        ),
        (
            "version_command",
            serde_json::to_value(&expected.version_command).map_err(display_error)?,
        ),
        (
            "version_exit_status",
            Value::from(expected.version_exit_status),
        ),
        (
            "executable_path",
            Value::String(expected.executable_path.clone()),
        ),
        ("executable_mode", Value::from(expected.executable_mode)),
        (
            "executable_sha256",
            Value::String(expected.executable_sha256.clone()),
        ),
        (
            "version_output",
            Value::String(expected.version_output.clone()),
        ),
        ("result", Value::String(expected.result.clone())),
    ];
    for (field, expected_value) in expected_fields {
        if object.get(field) != Some(&expected_value) {
            return Err(Error::new(format!("proof {field} mismatch")));
        }
    }
    let os_release = object["os_release"]
        .as_str()
        .ok_or_else(|| Error::new("proof os_release mismatch"))?;
    validate_identity(
        if expected.platform == "rpm-x86_64" {
            "fedora_os"
        } else {
            "ubuntu_os"
        },
        os_release,
    )?;
    let manager = object["package_manager_version"]
        .as_str()
        .ok_or_else(|| Error::new("proof package_manager_version mismatch"))?;
    let manager_ok = match expected.platform.as_str() {
        "debian-amd64" => {
            manager.starts_with("Debian 'dpkg' package management program version ")
                || manager.starts_with("dpkg ")
        }
        "rpm-x86_64" => manager.starts_with("RPM version ") || manager.starts_with("rpm "),
        "tar-x86_64" => manager == "installer portable-tar",
        _ => false,
    };
    if !manager_ok {
        return Err(Error::new(
            "proof package manager mismatch: expected platform policy, actual different",
        ));
    }
    let proof_time = object["proof_time"]
        .as_str()
        .ok_or_else(|| Error::new("proof proof_time mismatch"))?;
    validate_timestamp("proof_time", proof_time)?;
    validate_timestamp("policy checked_at", &expected.policy_checked_at)?;
    validate_timestamp("proof validation time", &expected.validation_time)?;
    let proof_time = DateTime::parse_from_rfc3339(proof_time).map_err(display_error)?;
    let checked_at =
        DateTime::parse_from_rfc3339(&expected.policy_checked_at).map_err(display_error)?;
    let validation_time =
        DateTime::parse_from_rfc3339(&expected.validation_time).map_err(display_error)?;
    if proof_time < checked_at || proof_time > validation_time {
        return Err(Error::new(
            "proof proof_time mismatch: expected advisory window, actual outside",
        ));
    }
    Ok(())
}

pub fn advisory_db_directory(url: &str) -> Result<String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("file://") {
        return Err(Error::new(
            "advisory database URL mismatch: expected file URL, actual other",
        ));
    }
    let name = lower
        .split('/')
        .next_back()
        .filter(|value| !value.is_empty())
        .unwrap_or("empty_");
    portable_path_component(name)?;
    Ok(format!(
        "{name}-{:016x}",
        xxh64(0xca80_de71, lower.as_bytes())
    ))
}

pub fn export_immutable_context(root: &RepoRoot, destination: &Path) -> Result<ImmutableContext> {
    require_directory(destination, "immutable context destination")?;
    if fs::read_dir(destination)
        .map_err(display_error)?
        .next()
        .is_some()
    {
        return Err(Error::new(
            "immutable context mismatch: expected empty destination, actual populated",
        ));
    }
    let commit = command(root.path(), &["git", "rev-parse", "HEAD"])?;
    if !is_git_commit(&commit) {
        return Err(Error::new(
            "release commit mismatch: expected 40 or 64 lowercase hexadecimal characters, actual invalid",
        ));
    }
    let archive = command_bytes(root.path(), &["git", "archive", "--format=tar", "HEAD"])?;
    if git_archive_commit(&archive)? != commit {
        return Err(Error::new(
            "git archive commit mismatch: expected checkout HEAD, actual archive differs",
        ));
    }
    extract_context(&archive, destination, &commit)?;
    let source_lock = digest(&fs::read(root.path().join("Cargo.lock")).map_err(display_error)?);
    let extracted_lock = digest(&fs::read(destination.join("Cargo.lock")).map_err(display_error)?);
    if source_lock != extracted_lock {
        return Err(Error::new(format!(
            "Cargo.lock digest mismatch: expected {source_lock}, actual {extracted_lock}"
        )));
    }
    for relative in EXPECTED_LAYOUT {
        require_regular(&destination.join(relative), relative)?;
    }
    let binding = ContextBinding {
        source_commit: commit.clone(),
        source_archive_sha256: digest(&archive),
        cargo_lock_sha256: source_lock.clone(),
    };
    let binding_bytes = canonical_json(&serde_json::to_value(binding).map_err(display_error)?)?;
    fs::write(destination.join(CONTEXT_BINDING_NAME), binding_bytes).map_err(display_error)?;
    fs::write(destination.join(CONTEXT_ARCHIVE_NAME), &archive).map_err(display_error)?;
    Ok(ImmutableContext {
        commit,
        archive_sha256: digest(&archive),
        cargo_lock_sha256: source_lock,
        path: destination.to_owned(),
    })
}

pub(crate) fn git_archive_commit(archive: &[u8]) -> Result<String> {
    if archive.len() < 1024 || archive[156] != b'g' {
        return Err(Error::new(
            "git archive identity mismatch: expected global PAX header",
        ));
    }
    let size = std::str::from_utf8(&archive[124..136])
        .map_err(display_error)?
        .trim_matches(['\0', ' ']);
    let size = usize::from_str_radix(size, 8).map_err(display_error)?;
    let body = archive
        .get(512..512 + size)
        .ok_or_else(|| Error::new("git archive identity mismatch: expected complete PAX header"))?;
    let body = std::str::from_utf8(body).map_err(display_error)?;
    let commits = body
        .lines()
        .filter_map(|line| line.split_once(" comment=").map(|(_, value)| value))
        .filter(|value| is_git_commit(value))
        .collect::<Vec<_>>();
    if commits.len() != 1 {
        return Err(Error::new(
            "git archive identity mismatch: expected one commit comment",
        ));
    }
    Ok(commits[0].to_owned())
}

fn validate_evidence(root: &RepoRoot, evidence: &Evidence) -> Result<()> {
    if evidence.schema_version != SCHEMA_VERSION
        || evidence.product != PRODUCT
        || evidence.source_dirty
    {
        return Err(Error::new("release evidence identity mismatch"));
    }
    if evidence.active_exceptions != ordered_exceptions(root)? {
        return Err(Error::new("release evidence active_exceptions mismatch"));
    }
    validate_evidence_text("rust.rustc_verbose", &evidence.rust.rustc_verbose)?;
    validate_evidence_text("rust.cargo_version", &evidence.rust.cargo_version)?;
    validate_native_tools(root, &evidence.native_tools)?;
    validate_timestamp(
        "advisory checked_at",
        &evidence.dependency_policy.advisory_checked_at,
    )
}

fn validate_manifest_policy(root: &RepoRoot, manifest: &Manifest) -> Result<()> {
    validate_version(&manifest.version)?;
    validate_evidence_text("rust.rustc_verbose", &manifest.rust.rustc_verbose)?;
    validate_evidence_text("rust.cargo_version", &manifest.rust.cargo_version)?;
    validate_native_tools(root, &manifest.native_tools)?;
    validate_timestamp(
        "advisory checked_at",
        &manifest.dependency_policy.advisory_checked_at,
    )?;
    validate_artifact_set(&manifest.artifacts)?;
    for artifact in &manifest.artifacts {
        artifact_kind(&artifact.path, Some(&manifest.version))?;
    }
    Ok(())
}

fn validate_evidence_text(field: &str, value: &str) -> Result<()> {
    let compiler_commit = if matches!(field, "rust.rustc_verbose" | "lane rustc verbose") {
        Some(validate_rustc_verbose_banner(field, value)?)
    } else {
        None
    };
    validate_privacy(field, value, true, compiler_commit.as_deref())
}

fn validate_rustc_verbose_banner(field: &str, value: &str) -> Result<String> {
    let mut lines = value.lines();
    let banner = lines.next().unwrap_or_default();
    let banner_parts = banner
        .strip_prefix("rustc ")
        .and_then(|body| body.strip_suffix(')'))
        .and_then(|body| body.split_once(" ("));
    let Some((banner_release, revision)) = banner_parts else {
        return Err(Error::new(format!(
            "evidence field {field} mismatch: expected complete rustc verbose banner"
        )));
    };
    let Some((short_commit, banner_date)) = revision.split_once(' ') else {
        return Err(Error::new(format!(
            "evidence field {field} mismatch: expected complete rustc verbose banner"
        )));
    };
    let mut fields = BTreeMap::new();
    for line in lines {
        let (key, value) = line.split_once(": ").ok_or_else(|| {
            Error::new(format!(
                "evidence field {field} mismatch: expected rustc verbose key-value line"
            ))
        })?;
        if fields.insert(key, value).is_some() {
            return Err(Error::new(format!(
                "evidence field {field} mismatch: expected unique rustc verbose lines"
            )));
        }
    }
    let expected = BTreeSet::from([
        "LLVM version",
        "binary",
        "commit-date",
        "commit-hash",
        "host",
        "release",
    ]);
    if fields.keys().copied().collect::<BTreeSet<_>>() != expected
        || fields["binary"] != "rustc"
        || !is_git_commit(fields["commit-hash"])
        || short_commit.len() != 9
        || !short_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || !fields["commit-hash"].starts_with(short_commit)
        || banner_release != fields["release"]
        || banner_date != fields["commit-date"]
        || fields["host"].is_empty()
        || fields["release"].is_empty()
        || fields["LLVM version"].is_empty()
        || DateTime::parse_from_rfc3339(&format!("{}T00:00:00Z", fields["commit-date"])).is_err()
    {
        return Err(Error::new(format!(
            "evidence field {field} mismatch: expected complete rustc verbose banner"
        )));
    }
    Ok(fields["commit-hash"].to_owned())
}

fn validate_privacy(
    field: &str,
    value: &str,
    allow_multiline: bool,
    allowed_opaque: Option<&str>,
) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    let forbidden = [
        "token",
        "secret",
        "password",
        "bearer",
        "localhost",
        ".local",
        ".internal",
        "staging",
        "sandbox",
        "preview",
        " dev ",
        " test ",
        "socket",
        "pipe:",
        "ipc:",
    ];
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    let network_literal = tokens.iter().any(|token| {
        let token = token
            .trim_matches(|character: char| matches!(character, '(' | ')' | '[' | ']' | ',' | ';'));
        let ipv4 = {
            let groups = token.split('.').collect::<Vec<_>>();
            groups.len() == 4
                && groups.iter().all(|group| {
                    !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit())
                })
        };
        let ipv6 = token.contains("::") || {
            let groups = token.split(':').collect::<Vec<_>>();
            groups.len() >= 3
                && groups.iter().all(|group| {
                    !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        };
        ipv4 || ipv6
    });
    let opaque_blob = tokens.iter().any(|token| {
        (token.len() >= 12 && token.chars().all(|character| character.is_ascii_digit()))
            || (token.len() >= 20
                && *token != TARGET_TRIPLE
                && Some(*token) != allowed_opaque
                && token.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '+' | '/' | '=' | '_' | '-')
                }))
    });
    let bad = value.is_empty()
        || value.chars().any(|character| {
            character.is_control() && !(allow_multiline && matches!(character, '\n' | '\t'))
        })
        || value.contains(['$', '%', '\\', '/', '@'])
        || value.contains("://")
        || forbidden.iter().any(|word| lower.contains(word))
        || network_literal
        || opaque_blob;
    if bad {
        return Err(Error::new(format!(
            "evidence field {field} privacy mismatch"
        )));
    }
    Ok(())
}

fn validate_timestamp(field: &str, value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    let canonical_shape = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        });
    if !canonical_shape {
        return Err(Error::new(format!(
            "{field} mismatch: expected canonical UTC seconds with Z suffix"
        )));
    }
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| Error::new(format!("{field} mismatch: expected canonical UTC time")))?;
    if parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) != value {
        return Err(Error::new(format!(
            "{field} mismatch: expected canonical UTC seconds with Z suffix"
        )));
    }
    Ok(())
}

fn validate_native_tools(root: &RepoRoot, tools: &BTreeMap<String, String>) -> Result<()> {
    let actual = tools.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = TOOL_SPECS
        .iter()
        .map(|spec| spec.key)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(Error::new(format!(
            "native tools mismatch: expected 18 keys, actual {}",
            actual.len()
        )));
    }
    exact_tool(tools, "cargo_deb", "3.7.0")?;
    exact_tool(tools, "cargo_generate_rpm", "0.21.0")?;
    exact_tool(tools, "manifest_validator", env!("CARGO_PKG_VERSION"))?;
    exact_tool(tools, "signing_mode", "unsigned")?;
    let rust_pin = rust_pin(root)?;
    exact_tool(tools, "ubuntu_rustc", &format!("rustc {rust_pin}"))?;
    exact_tool(tools, "ubuntu_cargo", &format!("cargo {rust_pin}"))?;
    for key in ["ubuntu_image_digest", "fedora_image_digest"] {
        let value = &tools[key];
        if !is_sha256(value) {
            return Err(Error::new(format!("native tool {key} mismatch")));
        }
    }
    for key in TOOL_SPECS.iter().map(|spec| spec.key) {
        if !matches!(
            key,
            "cargo_deb"
                | "cargo_generate_rpm"
                | "manifest_validator"
                | "signing_mode"
                | "ubuntu_rustc"
                | "ubuntu_cargo"
                | "ubuntu_image_digest"
                | "fedora_image_digest"
        ) {
            validate_identity(key, &tools[key])?;
        }
    }
    Ok(())
}

fn exact_tool(tools: &BTreeMap<String, String>, key: &str, expected: &str) -> Result<()> {
    if tools[key] != expected {
        return Err(Error::new(format!(
            "native tool {key} mismatch: expected {expected}"
        )));
    }
    Ok(())
}

pub(crate) fn validate_identity(key: &str, value: &str) -> Result<()> {
    validate_privacy(&format!("native tool {key}"), value, false, None)
        .map_err(|_| Error::new(format!("native tool {key} identity mismatch")))?;
    let approved_prefixes: &[&str] = match key {
        "container_engine" => &["podman ", "docker "],
        "ubuntu_os" => &["Ubuntu "],
        "ubuntu_compiler" => &["gcc ", "cc "],
        "ubuntu_linker" => &["GNU ld ", "ld "],
        "ubuntu_glibc" => &["glibc "],
        "ubuntu_tar" => &["GNU tar ", "tar "],
        "ubuntu_gzip" => &["gzip "],
        "fedora_os" => &["Fedora "],
        "dpkg_deb" => &["dpkg-deb "],
        "rpm" => &["RPM ", "rpm "],
        _ => &[],
    };
    let bad = !approved_prefixes
        .iter()
        .any(|prefix| value.starts_with(prefix))
        || value.trim() != value
        || value.contains("  ")
        || value.chars().any(|character| character.is_control())
        || !value.chars().any(|character| character.is_ascii_digit())
        || value.split_whitespace().count() > 6;
    if bad {
        return Err(Error::new(format!("native tool {key} identity mismatch")));
    }
    Ok(())
}

fn validate_artifact_set(artifacts: &[Artifact]) -> Result<()> {
    if artifacts.len() != 3 {
        return Err(Error::new(format!(
            "artifact inventory mismatch: expected 3, actual {}",
            artifacts.len()
        )));
    }
    let mut previous: Option<&str> = None;
    let mut kinds = BTreeSet::new();
    for artifact in artifacts {
        portable_path(&artifact.path)?;
        if Path::new(&artifact.path).file_name() != Some(OsStr::new(&artifact.path)) {
            return Err(Error::new("artifact path mismatch: expected basename"));
        }
        if !is_sha256(&artifact.sha256) || artifact.bytes == 0 {
            return Err(Error::new(format!(
                "artifact metadata mismatch: {}",
                artifact.path
            )));
        }
        if previous.is_some_and(|name| name.as_bytes() >= artifact.path.as_bytes()) {
            return Err(Error::new("artifact ordering mismatch"));
        }
        previous = Some(&artifact.path);
        kinds.insert(artifact_kind(&artifact.path, None)?);
    }
    if kinds != BTreeSet::from(["deb", "rpm", "tar"]) {
        return Err(Error::new("artifact type inventory mismatch"));
    }
    Ok(())
}

fn artifact_kind(name: &str, expected_version: Option<&str>) -> Result<&'static str> {
    let (kind, version) = if let Some(version) = name
        .strip_prefix("solstone-linux-")
        .and_then(|value| value.strip_suffix("-linux-x86_64.tar.gz"))
    {
        ("tar", version)
    } else if let Some(version) = name
        .strip_prefix("solstone-linux_")
        .and_then(|value| value.strip_suffix("-1_amd64.deb"))
    {
        ("deb", version)
    } else if let Some(version) = name
        .strip_prefix("solstone-linux-")
        .and_then(|value| value.strip_suffix("-1.x86_64.rpm"))
    {
        ("rpm", version)
    } else {
        return Err(Error::new("artifact basename mismatch"));
    };
    validate_version(version)?;
    if expected_version.is_some_and(|expected| version != expected) {
        return Err(Error::new(format!(
            "artifact version mismatch: expected {}",
            expected_version.unwrap()
        )));
    }
    Ok(kind)
}

pub(crate) fn artifact_name(kind: &str, version: &str) -> Result<String> {
    validate_version(version)?;
    match kind {
        "tar" => Ok(format!("solstone-linux-{version}-linux-x86_64.tar.gz")),
        "deb" => Ok(format!("solstone-linux_{version}-1_amd64.deb")),
        "rpm" => Ok(format!("solstone-linux-{version}-1.x86_64.rpm")),
        _ => Err(Error::new(
            "artifact kind mismatch: expected tar, deb, or rpm",
        )),
    }
}

pub(crate) fn artifact_by_kind<'a>(artifacts: &'a [Artifact], kind: &str) -> Result<&'a Artifact> {
    artifacts
        .iter()
        .find(|artifact| artifact_kind(&artifact.path, None).ok() == Some(kind))
        .ok_or_else(|| Error::new(format!("{kind} artifact mismatch: expected present")))
}

fn artifact_paths(root: &Path, version: &str) -> Result<Vec<PathBuf>> {
    require_directory(root, "release root")?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(display_error)? {
        let entry = entry.map_err(display_error)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::new("artifact path is not UTF-8"))?;
        if artifact_kind(&name, Some(version)).is_ok() {
            require_regular(&entry.path(), &name)?;
            paths.push(entry.path());
        }
    }
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    if paths.len() != 3 {
        return Err(Error::new(format!(
            "artifact inventory mismatch: expected 3, actual {}",
            paths.len()
        )));
    }
    Ok(paths)
}

fn artifact(path: &Path) -> Result<Artifact> {
    require_regular(path, "artifact")?;
    let bytes = fs::read(path).map_err(display_error)?;
    Ok(Artifact {
        path: path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| Error::new("artifact basename missing"))?
            .to_owned(),
        sha256: digest(&bytes),
        bytes: u64::try_from(bytes.len()).map_err(display_error)?,
    })
}

fn verify_artifacts(manifest: &Manifest, root: &Path) -> Result<()> {
    for item in &manifest.artifacts {
        let path = root.join(&item.path);
        require_regular(&path, &item.path)?;
        let actual = artifact(&path)?;
        if actual != *item {
            return Err(Error::new(format!(
                "artifact bytes mismatch: {}",
                item.path
            )));
        }
        verify_package_identity(&path, &manifest.version)?;
    }
    Ok(())
}

pub fn verify_checksums(manifest: &Manifest, root: &Path) -> Result<()> {
    for expected in &manifest.artifacts {
        let actual = artifact(&root.join(&expected.path))?;
        if actual != *expected {
            return Err(Error::new(format!(
                "artifact checksum mismatch: {}",
                expected.path
            )));
        }
    }
    let path = root.join(CHECKSUM_NAME);
    require_regular(&path, CHECKSUM_NAME)?;
    let actual = fs::read_to_string(path).map_err(display_error)?;
    let expected = render_sha256sums(&manifest.artifacts)?;
    if actual != expected {
        return Err(Error::new("checksum inventory mismatch"));
    }
    Ok(())
}

fn verify_package_identity(path: &Path, version: &str) -> Result<()> {
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    validate_version(version)?;
    let expected = match artifact_kind(name, Some(version))? {
        "tar" => PackageIdentity {
            name: PRODUCT.to_owned(),
            version: tar_version(path)?,
            release: None,
            arch: "x86_64".to_owned(),
        },
        "deb" => deb_identity(path)?,
        "rpm" => rpm_identity(path)?,
        _ => unreachable!(),
    };
    if expected.name != PRODUCT
        || expected.version != version
        || !matches!(expected.arch.as_str(), "x86_64" | "amd64")
        || expected
            .release
            .as_deref()
            .is_some_and(|release| release != "1")
    {
        return Err(Error::new(format!("package metadata mismatch: {name}")));
    }
    Ok(())
}

fn tar_version(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(display_error)?;
    let decoder = BufGzDecoder::new(BufReader::new(file));
    let mut archive = Archive::new(decoder);
    archive.set_ignore_zeros(true);
    let mut members = Vec::new();
    for (index, entry) in archive
        .entries()
        .map_err(|_| Error::new("tar archive mismatch"))?
        .enumerate()
    {
        let mut entry = entry.map_err(|_| tar_member_error("header", index))?;
        members.push(tar_member(&mut entry, index)?);
    }
    let decoder = archive.into_inner();
    let mut reader = decoder.into_inner();
    if !reader
        .fill_buf()
        .map_err(|_| Error::new("tar archive mismatch"))?
        .is_empty()
    {
        return Err(Error::new("tar archive mismatch"));
    }

    let roots = members
        .iter()
        .filter_map(|member| member.components.first())
        .cloned()
        .collect::<BTreeSet<_>>();
    if roots.len() != 1 {
        return Err(Error::new("tar root inventory mismatch"));
    }
    let root = roots
        .into_iter()
        .next()
        .ok_or_else(|| Error::new("tar root inventory mismatch"))?;

    let mut paths = BTreeMap::new();
    let mut folded_paths = BTreeMap::new();
    let regular_paths = members
        .iter()
        .filter(|member| member.kind == TarMemberKind::Regular)
        .map(|member| member.components.clone())
        .collect::<BTreeSet<_>>();
    for member in &members {
        if member.kind == TarMemberKind::Regular && member.components.len() == 1 {
            return Err(tar_member_error("topology", member.index));
        }
        if let Some(previous_kind) = paths.insert(member.components.clone(), member.kind) {
            let category = if previous_kind == member.kind {
                "duplicate"
            } else {
                "topology"
            };
            return Err(tar_member_error(category, member.index));
        }
        let folded = member
            .components
            .iter()
            .map(|component| component.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if folded_paths.insert(folded, member.index).is_some() {
            return Err(tar_member_error("collision", member.index));
        }
        for depth in 1..member.components.len() {
            if regular_paths.contains(&member.components[..depth]) {
                return Err(tar_member_error("topology", member.index));
            }
        }
    }

    root.strip_prefix("solstone-linux-")
        .and_then(|value| value.strip_suffix("-linux-x86_64"))
        .map(str::to_owned)
        .ok_or_else(|| Error::new("tar embedded version mismatch"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TarMemberKind {
    Regular,
    Directory,
}

struct TarMember {
    components: Vec<String>,
    kind: TarMemberKind,
    index: usize,
}

fn tar_member<R: Read>(entry: &mut Entry<'_, R>, index: usize) -> Result<TarMember> {
    let kind = match entry.header().entry_type() {
        EntryType::Regular => TarMemberKind::Regular,
        EntryType::Directory => TarMemberKind::Directory,
        _ => return Err(tar_member_error("type", index)),
    };
    if entry
        .header()
        .mode()
        .map_err(|_| tar_member_error("mode", index))?
        & 0o6000
        != 0
    {
        return Err(tar_member_error("mode", index));
    }
    let pax_record = match entry
        .pax_extensions()
        .map_err(|_| tar_member_error("metadata", index))?
    {
        Some(mut extensions) => extensions
            .next()
            .transpose()
            .map_err(|_| tar_member_error("metadata", index))?,
        None => None,
    };
    if pax_record.is_some() {
        return Err(tar_member_error("metadata", index));
    }
    if entry
        .link_name_bytes()
        .is_some_and(|link_name| !link_name.is_empty())
    {
        return Err(tar_member_error("link", index));
    }
    if kind == TarMemberKind::Directory && entry.size() != 0 {
        return Err(tar_member_error("payload", index));
    }

    let path = entry.path().map_err(|_| tar_member_error("path", index))?;
    let path = path
        .to_str()
        .ok_or_else(|| tar_member_error("path", index))?;
    let normalized = match kind {
        TarMemberKind::Directory => path.strip_suffix('/').unwrap_or(path),
        TarMemberKind::Regular if path.ends_with('/') => {
            return Err(tar_member_error("path", index));
        }
        TarMemberKind::Regular => path,
    };
    if normalized.is_empty()
        || normalized.starts_with(['/', '\\'])
        || normalized.contains('\\')
        || normalized.chars().any(char::is_control)
        || Path::new(normalized)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(tar_member_error("path", index));
    }
    let components = normalized
        .split('/')
        .map(|component| {
            portable_path_component(component)
                .map(|()| component.to_owned())
                .map_err(|_| tar_member_error("path", index))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TarMember {
        components,
        kind,
        index,
    })
}

fn tar_member_error(category: &str, index: usize) -> Error {
    Error::new(format!("tar member {category} mismatch: index {index}"))
}

fn deb_identity(path: &Path) -> Result<PackageIdentity> {
    let file = File::open(path).map_err(display_error)?;
    let mut archive = ar::Archive::new(file);
    let mut control = None;
    let mut marker_count = 0;
    let mut control_count = 0;
    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.map_err(display_error)?;
        let name = std::str::from_utf8(entry.header().identifier())
            .map_err(display_error)?
            .trim_end_matches('/')
            .to_owned();
        if name == "debian-binary" {
            marker_count += 1;
            let mut marker = Vec::new();
            entry.read_to_end(&mut marker).map_err(display_error)?;
            if marker != b"2.0\n" {
                return Err(Error::new("deb format marker mismatch: expected 2.0"));
            }
        } else if name.starts_with("control.tar.") {
            control_count += 1;
            let mut compressed = Vec::new();
            entry.read_to_end(&mut compressed).map_err(display_error)?;
            control = Some(read_control_archive(&name, compressed)?);
        }
    }
    if marker_count != 1 {
        return Err(Error::new(format!(
            "deb format marker count mismatch: expected 1, actual {marker_count}"
        )));
    }
    if control_count != 1 {
        return Err(Error::new(format!(
            "deb control archive count mismatch: expected 1, actual {control_count}"
        )));
    }
    parse_deb_control(&control.ok_or_else(|| Error::new("deb control archive missing"))?)
}

fn read_control_archive(name: &str, bytes: Vec<u8>) -> Result<String> {
    let reader: Box<dyn Read> = if name.ends_with(".xz") {
        Box::new(XzDecoder::new(Cursor::new(bytes)))
    } else if name.ends_with(".gz") {
        Box::new(GzDecoder::new(Cursor::new(bytes)))
    } else if name.ends_with(".zst") {
        Box::new(zstd::stream::read::Decoder::new(Cursor::new(bytes)).map_err(display_error)?)
    } else {
        return Err(Error::new("deb control compression mismatch"));
    };
    let mut archive = Archive::new(reader);
    let mut control = None;
    let mut control_count = 0;
    for entry in archive.entries().map_err(display_error)? {
        let mut entry = entry.map_err(display_error)?;
        let path = entry.path().map_err(display_error)?;
        if matches!(path.to_str(), Some("control" | "./control")) {
            control_count += 1;
            let mut body = String::new();
            entry.read_to_string(&mut body).map_err(display_error)?;
            control = Some(body);
        }
    }
    if control_count != 1 {
        return Err(Error::new(format!(
            "deb control metadata count mismatch: expected 1, actual {control_count}"
        )));
    }
    control.ok_or_else(|| Error::new("deb control metadata missing"))
}

fn parse_deb_control(body: &str) -> Result<PackageIdentity> {
    let mut fields = BTreeMap::new();
    for line in body.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if fields.insert(key, value.trim()).is_some() {
            return Err(Error::new(format!("deb control field duplicate: {key}")));
        }
    }
    let version = field(&fields, "Version")?;
    let (version, release) = version
        .rsplit_once('-')
        .ok_or_else(|| Error::new("deb version mismatch"))?;
    Ok(PackageIdentity {
        name: field(&fields, "Package")?.to_owned(),
        version: version.to_owned(),
        release: Some(release.to_owned()),
        arch: field(&fields, "Architecture")?.to_owned(),
    })
}

fn field<'a>(fields: &'a BTreeMap<&str, &str>, key: &str) -> Result<&'a str> {
    let value = fields
        .get(key)
        .copied()
        .ok_or_else(|| Error::new(format!("package field missing: {key}")))?;
    if value.is_empty()
        || value.trim() != value
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err(Error::new(format!("package field mismatch: {key}")));
    }
    Ok(value)
}

fn rpm_identity(path: &Path) -> Result<PackageIdentity> {
    let file = File::open(path).map_err(display_error)?;
    let mut reader = BufReader::new(file);
    let package = rpm::PackageMetadata::parse(&mut reader).map_err(display_error)?;
    Ok(PackageIdentity {
        name: package.get_name().map_err(display_error)?.to_owned(),
        version: package.get_version().map_err(display_error)?.to_owned(),
        release: Some(package.get_release().map_err(display_error)?.to_owned()),
        arch: package.get_arch().map_err(display_error)?.to_owned(),
    })
}

pub fn package_member_evidence(path: &Path, version: &str) -> Result<PackageMemberEvidence> {
    verify_package_identity(path, version)?;
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    match artifact_kind(name, Some(version))? {
        "tar" => tar_executable_member(path, name),
        "deb" => deb_executable_member(path, name),
        "rpm" => rpm_executable_member(path, name),
        _ => unreachable!(),
    }
}

fn member_record(
    package: &str,
    format: &str,
    installed: &str,
    mode: u64,
    bytes: Vec<u8>,
) -> Result<PackageMemberEvidence> {
    if mode != 0o755 || bytes.is_empty() {
        return Err(Error::new(format!(
            "package executable mismatch: expected mode 0755 and nonempty bytes, actual {mode:o}"
        )));
    }
    Ok(PackageMemberEvidence {
        package_file: package.into(),
        format: format.into(),
        installed_path: installed.into(),
        mode,
        bytes: u64::try_from(bytes.len()).map_err(display_error)?,
        sha256: digest(&bytes),
    })
}

fn tar_executable_member(path: &Path, package: &str) -> Result<PackageMemberEvidence> {
    let decoder = BufGzDecoder::new(BufReader::new(File::open(path).map_err(display_error)?));
    let mut archive = Archive::new(decoder);
    let mut found = None;
    for entry in archive.entries().map_err(display_error)? {
        let mut entry = entry.map_err(display_error)?;
        let member = entry
            .path()
            .map_err(display_error)?
            .to_string_lossy()
            .into_owned();
        if member.ends_with("/bin/solstone-linux") {
            if found.is_some() || entry.header().entry_type() != EntryType::Regular {
                return Err(Error::new(
                    "tar executable member mismatch: expected unique regular file",
                ));
            }
            let mode = u64::from(entry.header().mode().map_err(display_error)?);
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(display_error)?;
            found = Some(member_record(
                package,
                "tar",
                "/bin/solstone-linux",
                mode,
                bytes,
            )?);
        }
    }
    found.ok_or_else(|| {
        Error::new("tar executable member mismatch: expected executable, actual missing")
    })
}

fn deb_executable_member(path: &Path, package: &str) -> Result<PackageMemberEvidence> {
    let mut archive = ar::Archive::new(File::open(path).map_err(display_error)?);
    let mut found = None;
    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.map_err(display_error)?;
        let name = std::str::from_utf8(entry.header().identifier())
            .map_err(display_error)?
            .trim_end_matches('/')
            .to_owned();
        if name.starts_with("data.tar.") {
            if found.is_some() {
                return Err(Error::new(
                    "deb data archive mismatch: expected unique member",
                ));
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(display_error)?;
            found = Some((name, bytes));
        }
    }
    let (name, bytes) = found
        .ok_or_else(|| Error::new("deb data archive mismatch: expected member, actual missing"))?;
    let reader: Box<dyn Read> = if name.ends_with(".xz") {
        Box::new(XzDecoder::new(Cursor::new(bytes)))
    } else if name.ends_with(".gz") {
        Box::new(GzDecoder::new(Cursor::new(bytes)))
    } else if name.ends_with(".zst") {
        Box::new(zstd::stream::read::Decoder::new(Cursor::new(bytes)).map_err(display_error)?)
    } else {
        return Err(Error::new("deb data compression mismatch"));
    };
    let mut tar = Archive::new(reader);
    let mut executable = None;
    for entry in tar.entries().map_err(display_error)? {
        let mut entry = entry.map_err(display_error)?;
        let member = entry
            .path()
            .map_err(display_error)?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_owned();
        if member == "usr/bin/solstone-linux" {
            if executable.is_some() || entry.header().entry_type() != EntryType::Regular {
                return Err(Error::new("deb executable member mismatch"));
            }
            let mode = u64::from(entry.header().mode().map_err(display_error)?);
            let mut body = Vec::new();
            entry.read_to_end(&mut body).map_err(display_error)?;
            executable = Some(member_record(
                package,
                "deb",
                "/usr/bin/solstone-linux",
                mode,
                body,
            )?);
        }
    }
    executable.ok_or_else(|| {
        Error::new("deb executable member mismatch: expected executable, actual missing")
    })
}

fn rpm_executable_member(path: &Path, package: &str) -> Result<PackageMemberEvidence> {
    let rpm = rpm::Package::open(path).map_err(display_error)?;
    let mut executable = None;
    for file in rpm.files().map_err(display_error)? {
        let file = file.map_err(display_error)?;
        if file.metadata.path == Path::new("/usr/bin/solstone-linux") {
            if executable.is_some() {
                return Err(Error::new(
                    "rpm executable member mismatch: expected unique file",
                ));
            }
            let mode = u64::from(file.metadata.mode.permissions());
            executable = Some(member_record(
                package,
                "rpm",
                "/usr/bin/solstone-linux",
                mode,
                file.content,
            )?);
        }
    }
    executable.ok_or_else(|| {
        Error::new("rpm executable member mismatch: expected executable, actual missing")
    })
}

fn validate_live(repo: &RepoRoot, manifest: &Manifest, payload_root: &Path) -> Result<()> {
    let root = repo.path();
    let root_toml: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).map_err(display_error)?)
            .map_err(display_error)?;
    let member_toml: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("crates/solstone-linux/Cargo.toml"))
            .map_err(display_error)?,
    )
    .map_err(display_error)?;
    let version = root_toml["workspace"]["package"]["version"]
        .as_str()
        .ok_or_else(|| Error::new("workspace version missing"))?;
    let product = member_toml["package"]["name"]
        .as_str()
        .ok_or_else(|| Error::new("product name missing"))?;
    let commit = command(root, &["git", "rev-parse", "HEAD"])?;
    let lock_digest = digest(&fs::read(root.join("Cargo.lock")).map_err(display_error)?);
    let makefile = fs::read_to_string(root.join("Makefile")).map_err(display_error)?;
    let cargo_deny_version = makefile
        .lines()
        .find_map(|line| line.strip_prefix("CARGO_DENY_VERSION := "))
        .ok_or_else(|| Error::new("cargo-deny version authority missing"))?;
    let active_exceptions = ordered_exceptions(repo)?;
    let checks = [
        (manifest.product == product, "product"),
        (manifest.version == version, "version"),
        (manifest.source_commit == commit, "source_commit"),
        (!manifest.source_dirty, "source_dirty"),
        (
            manifest.cargo_lock_sha256 == lock_digest,
            "cargo_lock_sha256",
        ),
        (
            manifest.dependency_policy.cargo_deny_version == cargo_deny_version,
            "cargo_deny_version",
        ),
        (
            manifest.dependency_policy.deterministic_gate == "pass",
            "deterministic_gate",
        ),
        (
            manifest.active_exceptions == active_exceptions,
            "active_exceptions",
        ),
        (
            matches!(&manifest.target, TargetEvidence::Compiled { triple, profile, features }
                if triple == TARGET_TRIPLE && profile == "release" && features.is_empty()),
            "target",
        ),
    ];
    if let Some((_, field)) = checks.into_iter().find(|(matches, _)| !matches) {
        return Err(Error::new(format!(
            "live release evidence mismatch: {field}"
        )));
    }
    require_clean_tree(root, payload_root)
}

fn ordered_exceptions(repo: &RepoRoot) -> Result<Vec<String>> {
    let root = repo.path();
    let deny: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("deny.toml")).map_err(display_error)?)
            .map_err(display_error)?;
    deny["advisories"]["ignore"]
        .as_array()
        .ok_or_else(|| Error::new("advisory exception authority missing"))?
        .iter()
        .map(|entry| {
            entry["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("advisory exception id missing"))
        })
        .collect()
}

fn require_clean_tree(root: &Path, payload_root: &Path) -> Result<()> {
    let root = root.canonicalize().map_err(display_error)?;
    let ignored = Command::new("git")
        .args(["check-ignore", "--quiet", "--"])
        .arg(payload_root)
        .current_dir(&root)
        .status()
        .map_err(display_error)?;
    if !ignored.success() {
        return Err(Error::new(
            "release payload ignore mismatch: expected git-ignored dist path",
        ));
    }
    let status = command(
        &root,
        &[
            "git",
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=no",
        ],
    )?;
    if !status.is_empty() {
        return Err(Error::new("source dirty mismatch: expected clean tree"));
    }
    Ok(())
}

fn rust_pin(repo: &RepoRoot) -> Result<String> {
    let root = repo.path();
    let toolchain: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("rust-toolchain.toml")).map_err(display_error)?,
    )
    .map_err(display_error)?;
    toolchain["toolchain"]["channel"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::new("Rust pin authority missing"))
}

fn command(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(args[0])
        .args(&args[1..])
        .current_dir(root)
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(format!("command mismatch: {}", args[0])));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(display_error)
}

fn verify_schema() -> Result<()> {
    if SCHEMA_BYTES.len() != 4416 || digest(SCHEMA_BYTES) != SCHEMA_SHA256 {
        return Err(Error::new("vendored schema bytes mismatch"));
    }
    let schema: Value = serde_json::from_slice(SCHEMA_BYTES).map_err(display_error)?;
    if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema"
        || schema["$id"] != "https://solpbc.org/schemas/rust-release-manifest/v1.json"
    {
        return Err(Error::new("vendored schema identity mismatch"));
    }
    Ok(())
}

fn verify_pinned_schema(bytes: &[u8], expected_digest: &str, id: &str, label: &str) -> Result<()> {
    if digest(bytes) != expected_digest {
        return Err(Error::new(format!("{label} schema bytes mismatch")));
    }
    let schema: Value = serde_json::from_slice(bytes).map_err(display_error)?;
    if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema" || schema["$id"] != id {
        return Err(Error::new(format!("{label} schema identity mismatch")));
    }
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(display_error)?;
    Ok(())
}

fn transaction_id() -> Result<String> {
    #[cfg(test)]
    if let Some(value) = TEST_TRANSACTION_ID.with(|slot| slot.borrow_mut().take()) {
        return Ok(value);
    }
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(display_error)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
type StagingTestBarrier = Box<dyn FnOnce(&RepoRoot, &TransactionComponent)>;
#[cfg(test)]
type ProofAttemptTestBarrier = Box<dyn FnOnce(&RepoRoot, &ReservedPath)>;

#[cfg(test)]
thread_local! {
    static TEST_TRANSACTION_ID: RefCell<Option<String>> = const { RefCell::new(None) };
    static STAGING_TEST_BARRIER: RefCell<Option<StagingTestBarrier>> = const { RefCell::new(None) };
    static PROOF_ATTEMPT_TEST_BARRIER: RefCell<Option<ProofAttemptTestBarrier>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_boundary_test_transaction(value: &str) {
    TEST_TRANSACTION_ID.with(|slot| *slot.borrow_mut() = Some(value.to_owned()));
}

#[cfg(test)]
pub(crate) fn set_staging_test_barrier(
    barrier: impl FnOnce(&RepoRoot, &TransactionComponent) + 'static,
) {
    STAGING_TEST_BARRIER.with(|slot| *slot.borrow_mut() = Some(Box::new(barrier)));
}

#[cfg(test)]
pub(crate) fn set_proof_attempt_test_barrier(
    barrier: impl FnOnce(&RepoRoot, &ReservedPath) + 'static,
) {
    PROOF_ATTEMPT_TEST_BARRIER.with(|slot| *slot.borrow_mut() = Some(Box::new(barrier)));
}

#[cfg(test)]
pub(crate) fn run_proof_attempt_test_barrier(root: &RepoRoot, attempt: &ReservedPath) {
    PROOF_ATTEMPT_TEST_BARRIER.with(|slot| {
        if let Some(barrier) = slot.borrow_mut().take() {
            barrier(root, attempt);
        }
    });
}

#[cfg(test)]
fn run_staging_test_barrier(root: &RepoRoot, transaction: &TransactionComponent) {
    STAGING_TEST_BARRIER.with(|slot| {
        if let Some(barrier) = slot.borrow_mut().take() {
            barrier(root, transaction);
        }
    });
}

fn extract_context(bytes: &[u8], destination: &Path, expected_commit: &str) -> Result<()> {
    let mut archive = Archive::new(Cursor::new(bytes));
    let mut paths = BTreeSet::new();
    for entry in archive.entries().map_err(display_error)? {
        let mut entry = entry.map_err(display_error)?;
        if entry.header().entry_type().is_pax_global_extensions() {
            let mut body = String::new();
            entry.read_to_string(&mut body).map_err(display_error)?;
            let record_length = expected_commit.len() + 12;
            if body != format!("{record_length} comment={expected_commit}\n") {
                return Err(Error::new(
                    "immutable context metadata mismatch: expected commit binding, actual other",
                ));
            }
            continue;
        }
        let path = entry.path().map_err(display_error)?;
        let path = path
            .to_str()
            .ok_or_else(|| Error::new("immutable context path mismatch: expected UTF-8"))?;
        let path = if entry.header().entry_type().is_dir() {
            path.strip_suffix('/').unwrap_or(path)
        } else {
            path
        };
        portable_path(path)?;
        if !paths.insert(path.to_owned()) {
            return Err(Error::new(format!(
                "immutable context path mismatch: expected unique, actual duplicate {path}"
            )));
        }
        let output = destination.join(path);
        if !output.starts_with(destination) {
            return Err(Error::new("immutable context path mismatch"));
        }
        let mode = entry.header().mode().map_err(display_error)? & 0o7777;
        if mode & 0o6000 != 0 {
            return Err(Error::new("immutable context mode mismatch"));
        }
        match entry.header().entry_type() {
            EntryType::Directory => {
                fs::create_dir_all(&output).map_err(display_error)?;
                fs::set_permissions(&output, fs::Permissions::from_mode(mode))
                    .map_err(display_error)?;
            }
            EntryType::Regular => {
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent).map_err(display_error)?;
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&output)
                    .map_err(display_error)?;
                std::io::copy(&mut entry, &mut file).map_err(display_error)?;
            }
            EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(display_error)?
                    .ok_or_else(|| Error::new("immutable context symlink target missing"))?;
                let target = target
                    .to_str()
                    .ok_or_else(|| Error::new("immutable context symlink target mismatch"))?;
                portable_path(target)?;
                if target.contains('/') {
                    return Err(Error::new("immutable context symlink target mismatch"));
                }
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent).map_err(display_error)?;
                }
                symlink(target, output).map_err(display_error)?;
            }
            other => {
                return Err(Error::new(format!(
                    "immutable context member type mismatch: actual {}",
                    other.as_byte()
                )));
            }
        }
    }
    Ok(())
}

fn command_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(args[0])
        .args(&args[1..])
        .current_dir(root)
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err(Error::new(format!("command mismatch: {}", args[0])));
    }
    Ok(output.stdout)
}

fn xxh64(seed: u64, bytes: &[u8]) -> u64 {
    const P1: u64 = 11_400_714_785_074_694_791;
    const P2: u64 = 14_029_467_366_897_019_727;
    const P3: u64 = 1_609_587_929_392_839_161;
    const P4: u64 = 9_650_029_242_287_828_579;
    const P5: u64 = 2_870_177_450_012_600_261;

    fn round(accumulator: u64, lane: u64) -> u64 {
        (accumulator.wrapping_add(lane.wrapping_mul(P2)))
            .rotate_left(31)
            .wrapping_mul(P1)
    }

    let mut offset = 0;
    let mut hash = if bytes.len() >= 32 {
        let mut lanes = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while offset + 32 <= bytes.len() {
            for lane in &mut lanes {
                let value = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
                *lane = round(*lane, value);
                offset += 8;
            }
        }
        let mut value = lanes[0]
            .rotate_left(1)
            .wrapping_add(lanes[1].rotate_left(7))
            .wrapping_add(lanes[2].rotate_left(12))
            .wrapping_add(lanes[3].rotate_left(18));
        for lane in lanes {
            value ^= round(0, lane);
            value = value.wrapping_mul(P1).wrapping_add(P4);
        }
        value
    } else {
        seed.wrapping_add(P5)
    };
    hash = hash.wrapping_add(bytes.len() as u64);
    while offset + 8 <= bytes.len() {
        let lane = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        hash ^= round(0, lane);
        hash = hash.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        offset += 8;
    }
    if offset + 4 <= bytes.len() {
        let lane = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        hash ^= u64::from(lane).wrapping_mul(P1);
        hash = hash.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        offset += 4;
    }
    for byte in &bytes[offset..] {
        hash ^= u64::from(*byte).wrapping_mul(P5);
        hash = hash.rotate_left(11).wrapping_mul(P1);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(P3);
    hash ^ (hash >> 32)
}

fn portable_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with(['/', '\\'])
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Error::new("unsafe path"));
    }
    for part in path.split('/') {
        portable_path_component(part)?;
    }
    Ok(())
}

fn portable_path_component(part: &str) -> Result<()> {
    if part.is_empty()
        || part.ends_with(['.', ' '])
        || part.contains(['<', '>', ':', '"', '|', '?', '*'])
    {
        return Err(Error::new("non-portable path"));
    }
    let stem = part
        .split('.')
        .next()
        .unwrap()
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        return Err(Error::new("reserved path"));
    }
    Ok(())
}

fn require_regular(path: &Path, label: &str) -> Result<()> {
    let metadata = require_no_follow_regular(path, label)?;
    if metadata.mode() & 0o7111 != 0 {
        return Err(Error::new(format!(
            "{label} mode mismatch: expected non-executable regular file, actual executable or special mode\nrepair: replace {label} with a non-executable regular file"
        )));
    }
    Ok(())
}

fn require_regular_executable(path: &Path, label: &str) -> Result<()> {
    let metadata = require_no_follow_regular(path, label)?;
    if metadata.mode() & 0o111 == 0 {
        return Err(Error::new(format!(
            "{label} mode mismatch: expected executable regular file, actual non-executable\nrepair: install {label} with at least one execute bit"
        )));
    }
    Ok(())
}

fn require_no_follow_regular(path: &Path, label: &str) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(display_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::new(format!(
            "{label} mismatch: expected no-follow regular file, actual symlink or non-regular file\nrepair: replace {label} with a no-follow regular file"
        )));
    }
    Ok(metadata)
}

fn require_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(display_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::new(format!(
            "{label} mismatch: expected no-follow directory"
        )));
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<()> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value.matches('+').count() > 1
    {
        return Err(Error::new("version mismatch"));
    }
    let (without_build, build) = value
        .split_once('+')
        .map_or((value, None), |(core, build)| (core, Some(build)));
    if build.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, false)) {
        return Err(Error::new("version mismatch"));
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    if prerelease.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, true)) {
        return Err(Error::new("version mismatch"));
    }
    let parts = core.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
                || part.parse::<u64>().is_err()
        })
    {
        return Err(Error::new("version mismatch"));
    }
    Ok(())
}

fn valid_semver_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_numeric_leading_zero
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || identifier.len() == 1
                    || !identifier.starts_with('0'))
        })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(crate) fn is_git_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn display_error(error: impl std::fmt::Display) -> Error {
    Error::new(error.to_string())
}

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod candidate_tests;
#[cfg(test)]
mod proof_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transparency_tests;
