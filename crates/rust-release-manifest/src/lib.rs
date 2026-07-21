// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Offline validation and deterministic rendering for Rust release manifests.

use chrono::DateTime;
use flate2::bufread::GzDecoder as BufGzDecoder;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor, Read};
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use tar::{Archive, Entry, EntryType};
use xz2::read::XzDecoder;

pub const SCHEMA_VERSION: u64 = 1;
pub const SCHEMA_SHA256: &str = "d4eabf52bcc68b56945912d351f818e5444fe8c6461cb5c48b096f87b17a875c";
pub const CHECKSUM_NAME: &str = "SHA256SUMS";
pub const PRODUCT: &str = "solstone-linux";
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";
pub const CARGO_DENY_VERSION: &str = "0.20.2";
pub const MANIFEST_OK_MESSAGE: &str =
    "Named manifest and artifacts verified; this is NOT candidate-readiness classification.";
pub const RELEASE_DIR_OK_MESSAGE: &str =
    "Release directory verified as a complete five-file candidate.";

pub fn manifest_name(version: &str) -> String {
    format!("solstone-linux-{version}-linux-x86_64.rust-release-manifest.json")
}

const SCHEMA_BYTES: &[u8] =
    include_bytes!("../../../vendor/rust-release-manifest/rust-release-manifest.schema.json");
const TOOL_KEYS: [&str; 18] = [
    "cargo_deb",
    "cargo_generate_rpm",
    "container_engine",
    "dpkg_deb",
    "fedora_image_digest",
    "fedora_os",
    "manifest_validator",
    "rpm",
    "signing_mode",
    "ubuntu_cargo",
    "ubuntu_compiler",
    "ubuntu_glibc",
    "ubuntu_gzip",
    "ubuntu_image_digest",
    "ubuntu_linker",
    "ubuntu_os",
    "ubuntu_rustc",
    "ubuntu_tar",
];
#[cfg(test)]
const EXCEPTIONS: [&str; 2] = ["RUSTSEC-2026-0194", "RUSTSEC-2026-0195"];

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Evidence {
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

pub fn render_manifest(evidence: Evidence, release_dir: &Path) -> Result<String> {
    validate_evidence(&evidence)?;
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
    validate_manifest_bytes(output.as_bytes())?;
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

pub fn validate_manifest_bytes(bytes: &[u8]) -> Result<Manifest> {
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
    validate_manifest_policy(&manifest)?;
    Ok(manifest)
}

pub fn verify_manifest_mode(path: &Path) -> Result<()> {
    verify_manifest(path, true)
}

fn verify_manifest(path: &Path, bind_live: bool) -> Result<()> {
    require_regular(path, "manifest")?;
    let root = path
        .parent()
        .ok_or_else(|| Error::new("manifest parent missing"))?;
    let manifest = validate_manifest_bytes(&fs::read(path).map_err(display_error)?)?;
    let expected_name = manifest_name(&manifest.version);
    if path.file_name().and_then(OsStr::to_str) != Some(expected_name.as_str()) {
        return Err(Error::new(format!(
            "manifest basename mismatch: expected {expected_name}"
        )));
    }
    if bind_live {
        validate_live(&manifest, root)?;
    }
    verify_artifacts(&manifest, root)?;
    verify_checksums(&manifest, root)
}

pub fn classify_release_dir(root: &Path) -> Result<()> {
    classify_release(root, true)
}

fn classify_release(root: &Path, bind_live: bool) -> Result<()> {
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
    verify_manifest(&manifest_path, bind_live)
}

pub fn write_rendered(evidence: Evidence, release_dir: &Path) -> Result<()> {
    let manifest_text = render_manifest(evidence, release_dir)?;
    let manifest = validate_manifest_bytes(manifest_text.as_bytes())?;
    let sums = render_sha256sums(&manifest.artifacts)?;
    fs::write(
        release_dir.join(manifest_name(&manifest.version)),
        manifest_text,
    )
    .map_err(display_error)?;
    fs::write(release_dir.join(CHECKSUM_NAME), sums).map_err(display_error)
}

pub fn schema_bytes() -> &'static [u8] {
    SCHEMA_BYTES
}

fn validate_evidence(evidence: &Evidence) -> Result<()> {
    if evidence.schema_version != SCHEMA_VERSION
        || evidence.product != PRODUCT
        || evidence.source_dirty
    {
        return Err(Error::new("release evidence identity mismatch"));
    }
    if evidence.active_exceptions != ordered_exceptions()? {
        return Err(Error::new("release evidence active_exceptions mismatch"));
    }
    validate_evidence_text("rust.rustc_verbose", &evidence.rust.rustc_verbose)?;
    validate_evidence_text("rust.cargo_version", &evidence.rust.cargo_version)?;
    validate_native_tools(&evidence.native_tools)?;
    validate_timestamp(&evidence.dependency_policy.advisory_checked_at)
}

fn validate_manifest_policy(manifest: &Manifest) -> Result<()> {
    validate_version(&manifest.version)?;
    validate_evidence_text("rust.rustc_verbose", &manifest.rust.rustc_verbose)?;
    validate_evidence_text("rust.cargo_version", &manifest.rust.cargo_version)?;
    validate_native_tools(&manifest.native_tools)?;
    validate_timestamp(&manifest.dependency_policy.advisory_checked_at)?;
    validate_artifact_set(&manifest.artifacts)?;
    for artifact in &manifest.artifacts {
        artifact_kind(&artifact.path, Some(&manifest.version))?;
    }
    Ok(())
}

fn validate_evidence_text(field: &str, value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    let forbidden = [
        "token",
        "secret",
        "password",
        "bearer",
        "localhost",
        ".local",
        ".internal",
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
        token.len() >= 20
            && *token != TARGET_TRIPLE
            && token.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '+' | '/' | '=' | '_' | '-')
            })
    });
    let bad = value.is_empty()
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
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

fn validate_timestamp(value: &str) -> Result<()> {
    DateTime::parse_from_rfc3339(value).map_err(|_| Error::new("advisory time mismatch"))?;
    if !value.ends_with('Z') {
        return Err(Error::new(
            "advisory time mismatch: expected canonical UTC Z suffix",
        ));
    }
    Ok(())
}

fn validate_native_tools(tools: &BTreeMap<String, String>) -> Result<()> {
    let actual = tools.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = TOOL_KEYS.into_iter().collect::<BTreeSet<_>>();
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
    let rust_pin = rust_pin()?;
    exact_tool(tools, "ubuntu_rustc", &format!("rustc {rust_pin}"))?;
    exact_tool(tools, "ubuntu_cargo", &format!("cargo {rust_pin}"))?;
    for key in ["ubuntu_image_digest", "fedora_image_digest"] {
        let value = &tools[key];
        if !is_sha256(value) {
            return Err(Error::new(format!("native tool {key} mismatch")));
        }
    }
    for key in TOOL_KEYS {
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

fn validate_identity(key: &str, value: &str) -> Result<()> {
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
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    let private_identifier = tokens.iter().any(|token| {
        token.parse::<IpAddr>().is_ok()
            || (token.len() >= 32
                && token.contains('-')
                && token
                    .chars()
                    .all(|character| character.is_ascii_hexdigit() || character == '-'))
            || (token.len() >= 12 && token.chars().all(|character| character.is_ascii_digit()))
            || (token.len() >= 20
                && token.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '+' | '/' | '=' | '_' | '-')
                }))
    });
    let bad = !approved_prefixes
        .iter()
        .any(|prefix| value.starts_with(prefix))
        || value.trim() != value
        || value.contains("  ")
        || value.chars().any(|character| character.is_control())
        || value.contains(['$', '%', '@', '/', '\\'])
        || value.contains("://")
        || forbidden.iter().any(|word| lower.contains(word))
        || private_identifier
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

fn validate_live(manifest: &Manifest, payload_root: &Path) -> Result<()> {
    let root = workspace_root()?;
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
    let commit = command(&root, &["git", "rev-parse", "HEAD"])?;
    let lock_digest = digest(&fs::read(root.join("Cargo.lock")).map_err(display_error)?);
    let makefile = fs::read_to_string(root.join("Makefile")).map_err(display_error)?;
    let cargo_deny_version = makefile
        .lines()
        .find_map(|line| line.strip_prefix("CARGO_DENY_VERSION := "))
        .ok_or_else(|| Error::new("cargo-deny version authority missing"))?;
    let active_exceptions = ordered_exceptions()?;
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
    require_clean_tree(&root, payload_root)
}

fn ordered_exceptions() -> Result<Vec<String>> {
    let root = workspace_root()?;
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
    let payload_root = payload_root.canonicalize().map_err(display_error)?;
    let dist = root.join("dist");
    if payload_root != dist && !payload_root.starts_with(&dist) {
        return Err(Error::new(
            "release payload root mismatch: expected repository dist path",
        ));
    }
    let ignored = Command::new("git")
        .args(["check-ignore", "--quiet", "--"])
        .arg(&payload_root)
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

fn workspace_root() -> Result<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(display_error)
}

fn rust_pin() -> Result<String> {
    let root = workspace_root()?;
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
    let metadata = fs::symlink_metadata(path).map_err(display_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::new(format!(
            "{label} mismatch: expected no-follow regular file"
        )));
    }
    if metadata.mode() & 0o7111 != 0 {
        return Err(Error::new(format!("{label} mode mismatch")));
    }
    Ok(())
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

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn display_error(error: impl std::fmt::Display) -> Error {
    Error::new(error.to_string())
}

#[cfg(test)]
mod tests;
