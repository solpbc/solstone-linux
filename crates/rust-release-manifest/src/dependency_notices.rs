// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub const NOTICES_SCHEMA_VERSION: &str = "solstone-linux.rust-dependency-notices.v1";
pub const VENDORED_PREBUILTS_SCHEMA: &str = "solstone-linux.vendored-prebuilts.v1";
pub const ROOT_PACKAGE: &str = "solstone-linux";
pub const FILTER_PLATFORM: &str = "x86_64-unknown-linux-gnu";
pub const POPULATION_QUERY: &str = "cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-unknown-linux-gnu";

pub const NOTICES_HEADER: &str = "Rust dependency notices\n\nthis file reproduces the license texts of the crates statically linked into\nthe solstone app for linux. solstone's own code is agpl-3.0-only; see LICENSE.\n";

const PREBUILT_EXTENSIONS: &[&str] = &[
    ".lib",
    ".a",
    ".dll",
    ".so",
    ".dylib",
    ".o",
    ".obj",
    ".onnx",
    ".pt",
    ".pth",
    ".safetensors",
    ".gguf",
    ".npz",
    ".tflite",
    ".weights",
];

#[derive(Clone, Debug)]
pub struct DependencyNoticesPaths {
    pub workspace_root: PathBuf,
    pub cargo_lock: PathBuf,
    pub notices: PathBuf,
    pub index: PathBuf,
    pub overrides_dir: PathBuf,
    pub vendored_prebuilts: PathBuf,
}

impl DependencyNoticesPaths {
    pub fn from_workspace_root(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            cargo_lock: workspace_root.join("Cargo.lock"),
            notices: workspace_root.join("RUST_DEPENDENCY_NOTICES.txt"),
            index: workspace_root.join("licensing/rust-dependency-notices.index.json"),
            overrides_dir: workspace_root.join("licensing/overrides"),
            vendored_prebuilts: workspace_root.join("licensing/vendored-prebuilts.toml"),
            workspace_root,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyNoticesIndex {
    pub schema: String,
    pub cargo_lock_sha256: String,
    pub notices_sha256: String,
    pub population_query: String,
    pub root_package: String,
    pub filter_platform: String,
    pub external_package_count: usize,
    pub packages: Vec<PackageNoticesEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageNoticesEntry {
    pub name: String,
    pub version: String,
    pub source: String,
    pub license: Option<String>,
    pub members: Vec<NoticeMemberEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeMemberEntry {
    pub sha256: String,
    pub start: usize,
    pub end: usize,
    pub origin: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
struct VendoredPrebuiltsConfig {
    schema: String,
    #[serde(default, rename = "package")]
    packages: Vec<VendoredPrebuiltPackage>,
}

#[derive(Clone, Debug, Deserialize)]
struct VendoredPrebuiltPackage {
    name: String,
    version: String,
    suffixes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PrebuiltScanTarget {
    pub name: String,
    pub version: String,
    pub crate_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct ExternalCrate {
    name: String,
    version: String,
    source: String,
    license: Option<String>,
    crate_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct DiscoveredMember {
    origin: String,
    path: String,
    bytes: Vec<u8>,
    sha256: String,
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn digest_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|error| {
        Error::new(format!(
            "failed to read file for digest '{}': {error}",
            path.display()
        ))
    })?;
    Ok(digest_bytes(&bytes))
}

fn query_metadata(workspace_root: &Path) -> Result<Value> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            FILTER_PLATFORM,
        ])
        .current_dir(workspace_root)
        .output()
        .map_err(|error| {
            Error::new(format!(
                "cargo metadata invocation failed in '{}': {error}",
                workspace_root.display()
            ))
        })?;

    if !output.status.success() {
        let status = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        return Err(Error::new(format!(
            "cargo metadata mismatch: expected an offline resolve of the locked graph, actual exit {status}\ncargo: {}\nrepair: run 'cargo fetch --locked' once in this checkout; the offline resolve needs every crate in the lockfile cached, including ones that never build on this platform",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    serde_json::from_slice::<Value>(&output.stdout).map_err(|error| {
        Error::new(format!(
            "cargo metadata mismatch: expected a JSON document carrying packages, actual unparseable: {error}\nrepair: run 'cargo metadata --locked --offline --format-version 1' in this checkout and inspect its output"
        ))
    })
}

fn resolve_external_crates(metadata: &Value) -> Result<Vec<ExternalCrate>> {
    let packages_array = metadata["packages"]
        .as_array()
        .ok_or_else(|| Error::new("cargo metadata missing packages array"))?;
    let mut packages_by_id = BTreeMap::new();
    for pkg in packages_array {
        if let Some(id) = pkg["id"].as_str() {
            packages_by_id.insert(id, pkg);
        }
    }

    let resolve_nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or_else(|| Error::new("cargo metadata missing resolve nodes"))?;
    let mut nodes_by_id = BTreeMap::new();
    for node in resolve_nodes {
        if let Some(id) = node["id"].as_str() {
            nodes_by_id.insert(id, node);
        }
    }

    let root_pkg = packages_array
        .iter()
        .find(|pkg| pkg["name"].as_str() == Some(ROOT_PACKAGE))
        .ok_or_else(|| {
            Error::new(format!(
                "cargo metadata missing root package '{ROOT_PACKAGE}'"
            ))
        })?;
    let root_id = root_pkg["id"]
        .as_str()
        .ok_or_else(|| Error::new("root package missing id"))?;

    let mut visited = BTreeSet::new();
    let mut queue = vec![root_id];
    visited.insert(root_id);

    while let Some(curr) = queue.pop() {
        if let Some(node) = nodes_by_id.get(curr)
            && let Some(deps) = node["deps"].as_array()
        {
            for dep in deps {
                let dep_kinds = dep["dep_kinds"].as_array();
                let is_non_dev = match dep_kinds {
                    None => true,
                    Some(kinds) => {
                        kinds.is_empty()
                            || kinds.iter().any(|k| {
                                let kind_str = k["kind"].as_str();
                                kind_str.is_none()
                                    || kind_str == Some("normal")
                                    || kind_str == Some("build")
                            })
                    }
                };
                if is_non_dev
                    && let Some(dep_pkg_id) = dep["pkg"].as_str()
                    && visited.insert(dep_pkg_id)
                {
                    queue.push(dep_pkg_id);
                }
            }
        }
    }

    let mut external_crates = Vec::new();
    for pid in visited {
        let pkg = packages_by_id
            .get(pid)
            .ok_or_else(|| Error::new(format!("package {pid} missing in packages list")))?;
        let source = pkg["source"].as_str();
        if let Some(source_str) = source {
            let name = pkg["name"]
                .as_str()
                .ok_or_else(|| Error::new("package name missing"))?
                .to_owned();
            let version = pkg["version"]
                .as_str()
                .ok_or_else(|| Error::new("package version missing"))?
                .to_owned();
            let license = pkg["license"].as_str().map(|s| s.to_owned());
            let manifest_path = pkg["manifest_path"]
                .as_str()
                .ok_or_else(|| Error::new("package manifest_path missing"))?;
            let manifest_buf = PathBuf::from(manifest_path);
            let crate_dir = manifest_buf
                .parent()
                .ok_or_else(|| Error::new("manifest_path has no parent directory"))?
                .to_owned();

            external_crates.push(ExternalCrate {
                name,
                version,
                source: source_str.to_owned(),
                license,
                crate_dir,
            });
        }
    }

    if external_crates.is_empty() {
        return Err(Error::new(
            "cargo metadata resolve found zero external packages; expected non-empty closure",
        ));
    }

    external_crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(external_crates)
}

fn is_licence_file_match(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    if lower == "unlicense" || lower.starts_with("unlicense") {
        return false;
    }
    if lower == "copyright" {
        return true;
    }
    lower.starts_with("license")
        || lower.starts_with("licence")
        || lower.starts_with("copying")
        || lower.starts_with("notice")
}

fn walk_crate_dir_licences(dir: &Path, base: &Path) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    let entries = fs::read_dir(dir).map_err(|error| {
        Error::new(format!(
            "failed to read directory '{}': {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry
            .map_err(|error| Error::new(format!("failed to read directory entry: {error}")))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            Error::new(format!(
                "failed to get file type for '{}': {error}",
                path.display()
            ))
        })?;
        if file_type.is_dir() {
            let mut sub = walk_crate_dir_licences(&path, base)?;
            matches.append(&mut sub);
        } else if file_type.is_file()
            && let Some(file_name) = path.file_name().and_then(|n| n.to_str())
            && is_licence_file_match(file_name)
        {
            let rel = path
                .strip_prefix(base)
                .map_err(|error| Error::new(format!("failed to strip prefix: {error}")))?;
            matches.push(rel.to_path_buf());
        }
    }
    Ok(matches)
}

fn collect_package_licence_members(
    pkg: &ExternalCrate,
    overrides_dir: &Path,
) -> Result<Vec<DiscoveredMember>> {
    if !pkg.crate_dir.exists() {
        return Err(Error::new(format!(
            "crate directory missing for {} {}",
            pkg.name, pkg.version
        )));
    }

    let mut found_paths = walk_crate_dir_licences(&pkg.crate_dir, &pkg.crate_dir)?;
    found_paths.sort();

    let mut members = Vec::new();
    if !found_paths.is_empty() {
        for rel_path in found_paths {
            let full_path = pkg.crate_dir.join(&rel_path);
            let bytes = fs::read(&full_path).map_err(|error| {
                Error::new(format!(
                    "failed to read licence file '{}' for {} {}: {error}",
                    full_path.display(),
                    pkg.name,
                    pkg.version
                ))
            })?;
            let path_str = rel_path.to_string_lossy().replace('\\', "/");
            let sha256 = digest_bytes(&bytes);
            members.push(DiscoveredMember {
                origin: "crate-archive".to_owned(),
                path: path_str,
                bytes,
                sha256,
            });
        }
    } else {
        let pkg_override_dir = overrides_dir.join(&pkg.name).join(&pkg.version);
        if !pkg_override_dir.is_dir() {
            return Err(Error::new(format!(
                "missing committed license override for {} {}",
                pkg.name, pkg.version
            )));
        }
        let entries = fs::read_dir(&pkg_override_dir).map_err(|error| {
            Error::new(format!(
                "failed to read override directory '{}' for {} {}: {error}",
                pkg_override_dir.display(),
                pkg.name,
                pkg.version
            ))
        })?;
        let mut override_files = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|error| Error::new(format!("failed to read override entry: {error}")))?;
            let path = entry.path();
            if path.is_file()
                && let Some(file_name) = path.file_name().and_then(|n| n.to_str())
            {
                override_files.push((file_name.to_owned(), path));
            }
        }
        override_files.sort_by(|a, b| a.0.cmp(&b.0));
        if override_files.is_empty() {
            return Err(Error::new(format!(
                "missing committed license override for {} {}",
                pkg.name, pkg.version
            )));
        }
        for (file_name, full_path) in override_files {
            let bytes = fs::read(&full_path).map_err(|error| {
                Error::new(format!(
                    "failed to read override file '{}' for {} {}: {error}",
                    full_path.display(),
                    pkg.name,
                    pkg.version
                ))
            })?;
            let sha256 = digest_bytes(&bytes);
            members.push(DiscoveredMember {
                origin: "committed-override".to_owned(),
                path: file_name,
                bytes,
                sha256,
            });
        }
    }

    Ok(members)
}

pub fn scan_non_test_prebuilts(
    targets: &[PrebuiltScanTarget],
) -> Result<BTreeMap<(String, String), BTreeSet<String>>> {
    let mut discovered: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for pkg in targets {
        if !pkg.crate_dir.exists() {
            return Err(Error::new(format!(
                "crate directory missing for {} {}",
                pkg.name, pkg.version
            )));
        }
        scan_dir_prebuilts(&pkg.crate_dir, &pkg.crate_dir, pkg, &mut discovered)?;
    }
    Ok(discovered)
}

fn scan_dir_prebuilts(
    dir: &Path,
    base: &Path,
    pkg: &PrebuiltScanTarget,
    discovered: &mut BTreeMap<(String, String), BTreeSet<String>>,
) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|error| {
        Error::new(format!(
            "failed to read directory '{}': {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry
            .map_err(|error| Error::new(format!("failed to read directory entry: {error}")))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            Error::new(format!(
                "failed to get file type for '{}': {error}",
                path.display()
            ))
        })?;
        if file_type.is_dir() {
            scan_dir_prebuilts(&path, base, pkg, discovered)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(base)
                .map_err(|error| Error::new(format!("failed to strip prefix: {error}")))?;
            let is_test = rel.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s.eq_ignore_ascii_case("test") || s.eq_ignore_ascii_case("tests")
            });
            if is_test {
                continue;
            }
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                for &ext in PREBUILT_EXTENSIONS {
                    if file_name.ends_with(ext) {
                        discovered
                            .entry((pkg.name.clone(), pkg.version.clone()))
                            .or_default()
                            .insert(ext.to_owned());
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn verify_prebuilts_allowlist(
    discovered: &BTreeMap<(String, String), BTreeSet<String>>,
    allowlist_path: &Path,
) -> Result<()> {
    if discovered.is_empty() {
        return Err(Error::new(
            "vendored prebuilt scan found no prebuilts; expected non-empty scan",
        ));
    }

    let content = fs::read_to_string(allowlist_path).map_err(|error| {
        Error::new(format!(
            "failed to read vendored prebuilts allowlist '{}': {error}",
            allowlist_path.display()
        ))
    })?;
    let config: VendoredPrebuiltsConfig = toml::from_str(&content).map_err(|error| {
        Error::new(format!(
            "failed to parse vendored prebuilts allowlist '{}': {error}",
            allowlist_path.display()
        ))
    })?;

    if config.schema != VENDORED_PREBUILTS_SCHEMA {
        return Err(Error::new(format!(
            "vendored prebuilts allowlist schema mismatch: expected '{VENDORED_PREBUILTS_SCHEMA}', actual '{}'",
            config.schema
        )));
    }

    let mut allowlist_map: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for pkg in config.packages {
        let set: BTreeSet<String> = pkg.suffixes.into_iter().collect();
        allowlist_map.insert((pkg.name, pkg.version), set);
    }

    for (pkg_id, disc_suffixes) in discovered {
        match allowlist_map.get(pkg_id) {
            None => {
                return Err(Error::new(format!(
                    "unexpected vendored prebuilt package: {} {}",
                    pkg_id.0, pkg_id.1
                )));
            }
            Some(allow_suffixes) => {
                if disc_suffixes != allow_suffixes {
                    return Err(Error::new(format!(
                        "vendored prebuilt suffix mismatch for {} {}: discovered {:?}, allowlist {:?}",
                        pkg_id.0, pkg_id.1, disc_suffixes, allow_suffixes
                    )));
                }
            }
        }
    }

    for pkg_id in allowlist_map.keys() {
        if !discovered.contains_key(pkg_id) {
            return Err(Error::new(format!(
                "stale vendored prebuilt allowlist entry: {} {}",
                pkg_id.0, pkg_id.1
            )));
        }
    }

    Ok(())
}

pub fn generate_dependency_notices(paths: &DependencyNoticesPaths) -> Result<()> {
    let metadata = query_metadata(&paths.workspace_root)?;
    let external_crates = resolve_external_crates(&metadata)?;

    let prebuilt_targets: Vec<PrebuiltScanTarget> = external_crates
        .iter()
        .map(|c| PrebuiltScanTarget {
            name: c.name.clone(),
            version: c.version.clone(),
            crate_dir: c.crate_dir.clone(),
        })
        .collect();
    let discovered_prebuilts = scan_non_test_prebuilts(&prebuilt_targets)?;
    verify_prebuilts_allowlist(&discovered_prebuilts, &paths.vendored_prebuilts)?;

    let mut package_members: BTreeMap<(String, String), Vec<DiscoveredMember>> = BTreeMap::new();
    let mut unique_bodies: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for pkg in &external_crates {
        let members = collect_package_licence_members(pkg, &paths.overrides_dir)?;
        for m in &members {
            unique_bodies.insert(m.sha256.clone(), m.bytes.clone());
        }
        package_members.insert((pkg.name.clone(), pkg.version.clone()), members);
    }

    // Build notices buffer
    let mut notices_bytes = Vec::new();
    notices_bytes.extend_from_slice(NOTICES_HEADER.as_bytes());
    notices_bytes.push(b'\n'); // additional blank line

    let mut body_ranges: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let unique_count = unique_bodies.len();

    for (idx, (sha256, body)) in unique_bodies.iter().enumerate() {
        let start = notices_bytes.len();
        notices_bytes.extend_from_slice(body);
        let end = notices_bytes.len();
        body_ranges.insert(sha256.clone(), (start, end));

        if idx + 1 < unique_count {
            if body.ends_with(b"\n") {
                notices_bytes.push(b'\n');
            } else {
                notices_bytes.extend_from_slice(b"\n\n");
            }
        }
    }

    let notices_sha256 = digest_bytes(&notices_bytes);
    let cargo_lock_sha256 = digest_file(&paths.cargo_lock)?;

    let mut packages_index = Vec::new();
    for pkg in external_crates {
        let members = package_members
            .get(&(pkg.name.clone(), pkg.version.clone()))
            .ok_or_else(|| {
                Error::new(format!(
                    "missing member record for {} {}",
                    pkg.name, pkg.version
                ))
            })?;

        let mut member_entries = Vec::new();
        for m in members {
            let (start, end) = body_ranges.get(&m.sha256).copied().ok_or_else(|| {
                Error::new(format!(
                    "missing body range for member {} of {} {}",
                    m.path, pkg.name, pkg.version
                ))
            })?;
            member_entries.push(NoticeMemberEntry {
                sha256: m.sha256.clone(),
                start,
                end,
                origin: m.origin.clone(),
                path: m.path.clone(),
            });
        }

        packages_index.push(PackageNoticesEntry {
            name: pkg.name,
            version: pkg.version,
            source: pkg.source,
            license: pkg.license,
            members: member_entries,
        });
    }

    let index_data = DependencyNoticesIndex {
        schema: NOTICES_SCHEMA_VERSION.to_owned(),
        cargo_lock_sha256,
        notices_sha256,
        population_query: POPULATION_QUERY.to_owned(),
        root_package: ROOT_PACKAGE.to_owned(),
        filter_platform: FILTER_PLATFORM.to_owned(),
        external_package_count: packages_index.len(),
        packages: packages_index,
    };

    if let Some(parent) = paths.notices.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::new(e.to_string()))?;
    }
    fs::write(&paths.notices, &notices_bytes).map_err(|error| {
        Error::new(format!(
            "failed to write notices file '{}': {error}",
            paths.notices.display()
        ))
    })?;

    if let Some(parent) = paths.index.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::new(e.to_string()))?;
    }
    let index_json = serde_json::to_string_pretty(&index_data)
        .map_err(|error| Error::new(format!("failed to serialize notices index: {error}")))?
        + "\n";
    fs::write(&paths.index, index_json).map_err(|error| {
        Error::new(format!(
            "failed to write notices index '{}': {error}",
            paths.index.display()
        ))
    })?;

    Ok(())
}

pub fn check_dependency_notices(paths: &DependencyNoticesPaths) -> Result<()> {
    if !paths.index.exists() {
        return Err(Error::new(format!(
            "dependency notices index missing: '{}'",
            paths.index.display()
        )));
    }
    let index_raw = fs::read_to_string(&paths.index).map_err(|error| {
        Error::new(format!(
            "failed to read dependency notices index '{}': {error}",
            paths.index.display()
        ))
    })?;
    let index_data: DependencyNoticesIndex = serde_json::from_str(&index_raw).map_err(|error| {
        Error::new(format!(
            "failed to parse dependency notices index '{}': {error}",
            paths.index.display()
        ))
    })?;

    if index_data.schema != NOTICES_SCHEMA_VERSION {
        return Err(Error::new(format!(
            "dependency notices schema mismatch: expected '{NOTICES_SCHEMA_VERSION}', actual '{}'",
            index_data.schema
        )));
    }
    if index_data.population_query != POPULATION_QUERY {
        return Err(Error::new(format!(
            "dependency notices population_query mismatch: expected '{POPULATION_QUERY}', actual '{}'",
            index_data.population_query
        )));
    }
    if index_data.root_package != ROOT_PACKAGE {
        return Err(Error::new(format!(
            "dependency notices root_package mismatch: expected '{ROOT_PACKAGE}', actual '{}'",
            index_data.root_package
        )));
    }
    if index_data.filter_platform != FILTER_PLATFORM {
        return Err(Error::new(format!(
            "dependency notices filter_platform mismatch: expected '{FILTER_PLATFORM}', actual '{}'",
            index_data.filter_platform
        )));
    }

    // Lock hash check first
    if !paths.cargo_lock.exists() {
        return Err(Error::new(format!(
            "Cargo.lock missing at '{}'",
            paths.cargo_lock.display()
        )));
    }
    let actual_lock_sha256 = digest_file(&paths.cargo_lock)?;
    if actual_lock_sha256 != index_data.cargo_lock_sha256 {
        return Err(Error::new(format!(
            "Cargo.lock digest mismatch: index expected {}, actual {}",
            index_data.cargo_lock_sha256, actual_lock_sha256
        )));
    }

    // Notices hash check
    if !paths.notices.exists() {
        return Err(Error::new(format!(
            "RUST_DEPENDENCY_NOTICES.txt missing at '{}'",
            paths.notices.display()
        )));
    }
    let notices_bytes = fs::read(&paths.notices).map_err(|error| {
        Error::new(format!(
            "failed to read notices file '{}': {error}",
            paths.notices.display()
        ))
    })?;
    let actual_notices_sha256 = digest_bytes(&notices_bytes);
    if actual_notices_sha256 != index_data.notices_sha256 {
        return Err(Error::new(format!(
            "RUST_DEPENDENCY_NOTICES.txt digest mismatch: index expected {}, actual {}",
            index_data.notices_sha256, actual_notices_sha256
        )));
    }

    // Re-query metadata and re-read crate dirs
    let metadata = query_metadata(&paths.workspace_root)?;
    let external_crates = resolve_external_crates(&metadata)?;

    let prebuilt_targets: Vec<PrebuiltScanTarget> = external_crates
        .iter()
        .map(|c| PrebuiltScanTarget {
            name: c.name.clone(),
            version: c.version.clone(),
            crate_dir: c.crate_dir.clone(),
        })
        .collect();
    let discovered_prebuilts = scan_non_test_prebuilts(&prebuilt_targets)?;
    verify_prebuilts_allowlist(&discovered_prebuilts, &paths.vendored_prebuilts)?;

    if external_crates.len() != index_data.external_package_count {
        return Err(Error::new(format!(
            "external package count mismatch: metadata has {}, index recorded {}",
            external_crates.len(),
            index_data.external_package_count
        )));
    }

    let mut index_packages_by_id = BTreeMap::new();
    for pkg in &index_data.packages {
        index_packages_by_id.insert((pkg.name.clone(), pkg.version.clone()), pkg);
    }

    for live_pkg in &external_crates {
        let pkg_id = (live_pkg.name.clone(), live_pkg.version.clone());
        let index_pkg = index_packages_by_id.get(&pkg_id).ok_or_else(|| {
            Error::new(format!(
                "package {} {} present in metadata closure but absent from notices index",
                live_pkg.name, live_pkg.version
            ))
        })?;

        if live_pkg.source != index_pkg.source {
            return Err(Error::new(format!(
                "source mismatch for {} {}: metadata '{}', index '{}'",
                live_pkg.name, live_pkg.version, live_pkg.source, index_pkg.source
            )));
        }

        let live_members = collect_package_licence_members(live_pkg, &paths.overrides_dir)?;
        if live_members.len() != index_pkg.members.len() {
            return Err(Error::new(format!(
                "notice member count mismatch for {} {}: live found {}, index recorded {}",
                live_pkg.name,
                live_pkg.version,
                live_members.len(),
                index_pkg.members.len()
            )));
        }

        for (live_m, index_m) in live_members.iter().zip(&index_pkg.members) {
            if live_m.origin != index_m.origin {
                return Err(Error::new(format!(
                    "notice member origin mismatch for {} {} {}: live '{}', index '{}'",
                    live_pkg.name, live_pkg.version, live_m.path, live_m.origin, index_m.origin
                )));
            }
            if live_m.path != index_m.path {
                return Err(Error::new(format!(
                    "notice member path mismatch for {} {}: live '{}', index '{}'",
                    live_pkg.name, live_pkg.version, live_m.path, index_m.path
                )));
            }
            if live_m.sha256 != index_m.sha256 {
                return Err(Error::new(format!(
                    "notice member sha256 mismatch for {} {} {}: live '{}', index '{}'",
                    live_pkg.name, live_pkg.version, live_m.path, live_m.sha256, index_m.sha256
                )));
            }

            if index_m.end > notices_bytes.len() || index_m.start > index_m.end {
                return Err(Error::new(format!(
                    "invalid byte range [{}..{}] for member {} {} in notices (len {})",
                    index_m.start,
                    index_m.end,
                    live_pkg.name,
                    live_pkg.version,
                    notices_bytes.len()
                )));
            }

            let slice = &notices_bytes[index_m.start..index_m.end];
            if digest_bytes(slice) != index_m.sha256 {
                return Err(Error::new(format!(
                    "notices byte range [{}..{}] sha256 does not match recorded member sha256 for {} {} {}",
                    index_m.start, index_m.end, live_pkg.name, live_pkg.version, live_m.path
                )));
            }
            if slice != live_m.bytes {
                return Err(Error::new(format!(
                    "notices byte range [{}..{}] bytes do not match live file bytes for {} {} {}",
                    index_m.start, index_m.end, live_pkg.name, live_pkg.version, live_m.path
                )));
            }
        }
    }

    Ok(())
}
