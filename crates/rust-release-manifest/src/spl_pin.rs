// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::{Error, RepoRoot, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const SPL_SOURCE: &str = "https://github.com/solpbc/spl-rust";
const PACKAGES: [&str; 2] = ["spl-core", "spl-transport"];
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

struct WorkspacePins {
    revisions: BTreeMap<String, String>,
    versions: BTreeMap<String, String>,
}

fn error(message: String) -> Error {
    Error::new(message)
}

fn read_toml(root: &Path, relative: &Path, subject: &str) -> Result<toml::Value> {
    let text = fs::read_to_string(root.join(relative)).map_err(|cause| {
        error(format!(
            "{subject} parse mismatch: expected valid TOML, actual {}: {cause}\nrepair: restore valid TOML in {}",
            relative.display(),
            relative.display()
        ))
    })?;
    toml::from_str(&text).map_err(|cause| {
        error(format!(
            "{subject} parse mismatch: expected valid TOML, actual {}: {cause}\nrepair: restore valid TOML in {}",
            relative.display(),
            relative.display()
        ))
    })
}

fn package_identity(key: &str, value: &toml::Value) -> String {
    value
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_str)
        .unwrap_or(key)
        .to_owned()
}

fn dependency_tables(manifest: &toml::Value) -> Vec<&toml::value::Table> {
    let mut tables = DEPENDENCY_TABLES
        .iter()
        .filter_map(|name| manifest.get(*name).and_then(toml::Value::as_table))
        .collect::<Vec<_>>();
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            tables.extend(
                DEPENDENCY_TABLES
                    .iter()
                    .filter_map(|name| target.get(*name).and_then(toml::Value::as_table)),
            );
        }
    }
    tables
}

fn member_path(root: &Path, member: &str) -> Result<PathBuf> {
    let relative = Path::new(member);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(error(format!(
            "workspace manifest parse mismatch: expected valid TOML, actual Cargo.toml: invalid workspace member {member}\nrepair: restore valid TOML in Cargo.toml"
        )));
    }
    let manifest = relative.join("Cargo.toml");
    if !root.join(&manifest).starts_with(root) {
        return Err(error(format!(
            "workspace manifest parse mismatch: expected valid TOML, actual Cargo.toml: invalid workspace member {member}\nrepair: restore valid TOML in Cargo.toml"
        )));
    }
    Ok(manifest)
}

fn selector_actual(table: &toml::value::Table) -> String {
    let selectors = ["rev", "branch", "tag"]
        .into_iter()
        .filter(|key| table.contains_key(*key))
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        "version-only".to_owned()
    } else {
        selectors.join(",")
    }
}

fn validate_workspace(root: &toml::Value) -> Result<WorkspacePins> {
    let dependencies = root
        .get("workspace")
        .and_then(|value| value.get("dependencies"))
        .and_then(toml::Value::as_table);
    let mut revisions = BTreeMap::new();
    let mut versions = BTreeMap::new();
    for package in PACKAGES {
        let matches = dependencies
            .into_iter()
            .flat_map(|table| table.iter())
            .filter(|(key, value)| package_identity(key, value) == package)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            let actual = if matches.is_empty() {
                "missing".to_owned()
            } else {
                matches.len().to_string()
            };
            return Err(error(format!(
                "SPL package {package} workspace declaration mismatch: expected exactly one declaration, actual {actual}\nrepair: declare {package} once in root [workspace.dependencies]"
            )));
        }
        let table = matches[0].1.as_table().ok_or_else(|| {
            error(format!(
                "SPL package {package} source mismatch: expected {SPL_SOURCE}, actual non-table\nrepair: declare {package} from the approved SPL Git source in root Cargo.toml"
            ))
        })?;
        if table.contains_key("path") {
            return Err(error(format!(
                "SPL package {package} path dependency mismatch: expected absent, actual Cargo.toml:{}\nrepair: remove the local path route for {package} from Cargo.toml",
                matches[0].0
            )));
        }
        let version = table
            .get("version")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                error(format!(
                    "SPL package {package} version mismatch: expected declared version, actual missing\nrepair: declare {package} with the resolved version in root Cargo.toml"
                ))
            })?;
        let source = table
            .get("git")
            .and_then(toml::Value::as_str)
            .unwrap_or("missing");
        if source != SPL_SOURCE {
            return Err(error(format!(
                "SPL package {package} source mismatch: expected {SPL_SOURCE}, actual {source}\nrepair: declare {package} from the approved SPL Git source in root Cargo.toml"
            )));
        }
        if selector_actual(table) != "rev" {
            return Err(error(format!(
                "SPL package {package} selector mismatch: expected rev, actual {}\nrepair: select {package} with only rev in root Cargo.toml",
                selector_actual(table)
            )));
        }
        let revision = table
            .get("rev")
            .and_then(toml::Value::as_str)
            .unwrap_or("missing");
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(error(format!(
                "SPL package {package} revision mismatch: expected 40 lowercase hexadecimal characters, actual {revision}\nrepair: declare the full approved {package} commit in root Cargo.toml"
            )));
        }
        revisions.insert(package.to_owned(), revision.to_owned());
        versions.insert(package.to_owned(), version.to_owned());
    }
    if revisions["spl-core"] != revisions["spl-transport"] {
        return Err(error(format!(
            "SPL package spl-transport revision alignment mismatch: expected {}, actual {}\nrepair: pin spl-core and spl-transport to the same revision in root Cargo.toml",
            revisions["spl-core"], revisions["spl-transport"]
        )));
    }
    if let Some(dependencies) = dependencies {
        for (key, dependency) in dependencies {
            let package = package_identity(key, dependency);
            if !PACKAGES.contains(&package.as_str())
                && let Some(source) = dependency.get("git").and_then(toml::Value::as_str)
            {
                return Err(error(format!(
                    "Cargo package {package} Git source mismatch: expected no unapproved member Git dependency, actual Cargo.toml:{key}={source}\nrepair: remove the unapproved Git dependency from Cargo.toml"
                )));
            }
        }
    }
    Ok(WorkspacePins {
        revisions,
        versions,
    })
}

fn validate_member_dependencies(manifests: &[(PathBuf, toml::Value)]) -> Result<()> {
    let mut inherited = BTreeMap::from([
        ("spl-core", BTreeSet::new()),
        ("spl-transport", BTreeSet::new()),
    ]);
    for (path, manifest) in manifests {
        for table in dependency_tables(manifest) {
            for (key, dependency) in table {
                let package = package_identity(key, dependency);
                let Some(details) = dependency.as_table() else {
                    if PACKAGES.contains(&package.as_str()) {
                        return Err(error(format!(
                            "SPL package {package} leaf inheritance mismatch: expected workspace = true, actual not inherited\nrepair: inherit {package} from root [workspace.dependencies] in {}",
                            path.display()
                        )));
                    }
                    continue;
                };
                if PACKAGES.contains(&package.as_str()) {
                    if details.contains_key("path") {
                        return Err(error(format!(
                            "SPL package {package} path dependency mismatch: expected absent, actual {}:{key}\nrepair: remove the local path route for {package} from {}",
                            path.display(),
                            path.display()
                        )));
                    }
                    let forbidden = ["git", "rev", "path", "tag", "branch", "version"]
                        .into_iter()
                        .filter(|name| details.contains_key(*name))
                        .collect::<Vec<_>>();
                    if details.get("workspace").and_then(toml::Value::as_bool) != Some(true) {
                        return Err(error(format!(
                            "SPL package {package} leaf inheritance mismatch: expected workspace = true, actual not inherited\nrepair: inherit {package} from root [workspace.dependencies] in {}",
                            path.display()
                        )));
                    }
                    if !forbidden.is_empty() {
                        return Err(error(format!(
                            "SPL package {package} leaf inheritance mismatch: expected only workspace = true, actual keys {}\nrepair: remove local source and version keys from {package} in {}",
                            forbidden.join(","),
                            path.display()
                        )));
                    }
                    inherited
                        .get_mut(package.as_str())
                        .unwrap()
                        .insert(path.clone());
                } else if let Some(source) = details.get("git").and_then(toml::Value::as_str) {
                    return Err(error(format!(
                        "Cargo package {package} Git source mismatch: expected no unapproved member Git dependency, actual {}:{key}={source}\nrepair: remove the unapproved Git dependency from {}",
                        path.display(),
                        path.display()
                    )));
                }
            }
        }
    }
    for package in PACKAGES {
        let manifests = &inherited[package];
        if manifests.is_empty() {
            return Err(error(format!(
                "SPL package {package} leaf inheritance mismatch: expected workspace = true, actual missing\nrepair: inherit {package} from root [workspace.dependencies] in a workspace member"
            )));
        }
        if manifests.len() > 1 {
            let actual = manifests
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(",");
            return Err(error(format!(
                "SPL package {package} leaf inheritance mismatch: expected exactly one inheriting workspace member, actual {actual}\nrepair: inherit {package} from root [workspace.dependencies] in only one workspace member"
            )));
        }
    }
    Ok(())
}

fn validate_overrides(root: &toml::Value) -> Result<()> {
    if let Some(patches) = root.get("patch").and_then(toml::Value::as_table) {
        for (source, entries) in patches {
            if let Some(entries) = entries.as_table() {
                for (key, value) in entries {
                    let package = package_identity(key, value);
                    if PACKAGES.contains(&package.as_str()) {
                        return Err(error(format!(
                            "SPL package {package} patch override mismatch: expected absent, actual [patch.{source}]\nrepair: remove the {package} patch override from root Cargo.toml"
                        )));
                    }
                }
            }
        }
    }
    if let Some(replacements) = root.get("replace").and_then(toml::Value::as_table) {
        // Cargo identifies the replaced package in the package-ID key; there is no rename key.
        for package_id in replacements.keys() {
            let package = package_id.split(':').next().unwrap_or(package_id);
            if PACKAGES.contains(&package) {
                return Err(error(format!(
                    "SPL package {package} replacement mismatch: expected absent, actual {package_id}\nrepair: remove the {package} replacement from root Cargo.toml"
                )));
            }
        }
    }
    Ok(())
}

fn validate_config(repo: &Path) -> Result<()> {
    for relative in [Path::new(".cargo/config.toml"), Path::new(".cargo/config")] {
        if !repo.join(relative).exists() {
            continue;
        }
        let config = read_toml(repo, relative, "Cargo configuration")?;
        if let Some(sources) = config.get("source").and_then(toml::Value::as_table) {
            for (source, value) in sources {
                if let Some(replacement) = value.get("replace-with").and_then(toml::Value::as_str) {
                    return Err(error(format!(
                        "Cargo configuration source replacement mismatch: expected absent, actual {}:{source}->{replacement}\nrepair: remove the replace-with route so the SPL packages resolve from the declared workspace source",
                        relative.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_in_tree(repo: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*Cargo.toml"])
        .current_dir(repo)
        .output()
        .map_err(|cause| error(format!("tracked manifest inventory mismatch: expected git ls-files, actual {cause}\nrepair: restore the Git checkout before validating SPL pins")))?;
    if !output.status.success() {
        return Err(error(format!(
            "tracked manifest inventory mismatch: expected git ls-files, actual {}\nrepair: restore the Git checkout before validating SPL pins",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let relative = PathBuf::from(String::from_utf8(bytes.to_vec()).map_err(|cause| error(format!("tracked manifest inventory mismatch: expected UTF-8 paths, actual {cause}\nrepair: restore the Git checkout before validating SPL pins")))?);
        let manifest = read_toml(repo, &relative, "tracked manifest")?;
        if let Some(package) = manifest
            .get("package")
            .and_then(|value| value.get("name"))
            .and_then(toml::Value::as_str)
            && PACKAGES.contains(&package)
        {
            return Err(error(format!(
                "SPL package {package} in-tree implementation mismatch: expected absent, actual {}\nrepair: remove or rename the tracked in-tree crate implementing {package}",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn validate_lock(repo: &Path, pins: &WorkspacePins) -> Result<()> {
    let text = fs::read_to_string(repo.join("Cargo.lock")).map_err(|cause| error(format!("lockfile parse mismatch: expected valid TOML, actual {cause}\nrepair: restore a valid Cargo.lock before validating the SPL pin")))?;
    let lock: toml::Value = toml::from_str(&text).map_err(|cause| error(format!("lockfile parse mismatch: expected valid TOML, actual {cause}\nrepair: restore a valid Cargo.lock before validating the SPL pin")))?;
    let records = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for package in PACKAGES {
        let matches = records
            .iter()
            .filter(|record| record.get("name").and_then(toml::Value::as_str) == Some(package))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            if matches.is_empty() {
                return Err(error(format!(
                    "SPL package {package} lockfile record mismatch: expected exactly one package record, actual missing\nrepair: regenerate Cargo.lock with the approved {package} workspace pin"
                )));
            }
            return Err(error(format!(
                "SPL package {package} lockfile record mismatch: expected exactly one package record, actual {}\nrepair: regenerate Cargo.lock with one resolved {package} package",
                matches.len()
            )));
        }
        let lock_version = matches[0]
            .get("version")
            .and_then(toml::Value::as_str)
            .unwrap_or("missing");
        if lock_version != pins.versions[package] {
            return Err(error(format!(
                "SPL package {package} version mismatch: expected {lock_version}, actual {}\nrepair: declare {package} at the resolved version in root Cargo.toml",
                pins.versions[package]
            )));
        }
        let source = matches[0]
            .get("source")
            .and_then(toml::Value::as_str)
            .unwrap_or("missing");
        let Some(rest) = source.strip_prefix("git+") else {
            return Err(error(format!(
                "SPL package {package} lockfile source mismatch: expected {SPL_SOURCE}, actual {source}\nrepair: regenerate Cargo.lock from the approved {package} workspace source"
            )));
        };
        let (before_fragment, fragment) = rest.rsplit_once('#').unwrap_or((rest, "missing"));
        let (url, query) = before_fragment
            .split_once('?')
            .unwrap_or((before_fragment, ""));
        if url != SPL_SOURCE {
            return Err(error(format!(
                "SPL package {package} lockfile source mismatch: expected {SPL_SOURCE}, actual {url}\nrepair: regenerate Cargo.lock from the approved {package} workspace source"
            )));
        }
        let pairs = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect::<Vec<_>>();
        let selectors = pairs.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        if selectors != ["rev"] {
            let actual = if selectors.is_empty() {
                "missing".to_owned()
            } else {
                selectors.join(",")
            };
            return Err(error(format!(
                "SPL package {package} lockfile selector mismatch: expected rev, actual {actual}\nrepair: regenerate Cargo.lock from the rev-selected {package} workspace declaration"
            )));
        }
        let query_revision = pairs[0].1;
        if query_revision != pins.revisions[package] {
            return Err(error(format!(
                "SPL package {package} lockfile revision query mismatch: expected {}, actual {query_revision}\nrepair: regenerate Cargo.lock from the approved {package} workspace revision",
                pins.revisions[package]
            )));
        }
        if fragment != pins.revisions[package] {
            return Err(error(format!(
                "SPL package {package} lockfile resolved revision mismatch: expected {}, actual {fragment}\nrepair: regenerate Cargo.lock so {package} resolves to the approved workspace revision",
                pins.revisions[package]
            )));
        }
    }
    Ok(())
}

pub fn validate_spl_pin(repo: &RepoRoot) -> Result<()> {
    let root = read_toml(repo.path(), Path::new("Cargo.toml"), "workspace manifest")?;
    let pins = validate_workspace(&root)?;
    validate_overrides(&root)?;
    let members = root.get("workspace").and_then(|value| value.get("members")).and_then(toml::Value::as_array).ok_or_else(|| error("workspace manifest parse mismatch: expected valid TOML, actual Cargo.toml: missing workspace.members\nrepair: restore valid TOML in Cargo.toml".to_owned()))?;
    let mut manifests = Vec::new();
    for member in members {
        let member = member.as_str().ok_or_else(|| error("workspace manifest parse mismatch: expected valid TOML, actual Cargo.toml: non-string workspace member\nrepair: restore valid TOML in Cargo.toml".to_owned()))?;
        let path = member_path(repo.path(), member)?;
        manifests.push((
            path.clone(),
            read_toml(repo.path(), &path, "workspace manifest")?,
        ));
    }
    validate_member_dependencies(&manifests)?;
    validate_config(repo.path())?;
    validate_in_tree(repo.path())?;
    validate_lock(repo.path(), &pins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_checkout_spl_pin_is_valid() {
        let repo = RepoRoot::resolve().unwrap();
        validate_spl_pin(&repo).unwrap();
    }
}
