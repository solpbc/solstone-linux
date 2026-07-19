// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::release_rail_tests::{command_path, read_toml, workspace_root};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

const LOCKED_SUBCOMMANDS: &[&str] = &[
    "build", "check", "clippy", "test", "install", "metadata", "deb", "run",
];

#[derive(Default)]
struct ScanCounts {
    inspected: usize,
    nested_container: usize,
    make_wrapper: usize,
}

fn toolchain() -> toml::Value {
    read_toml(&workspace_root().join("rust-toolchain.toml"))
}

fn pin() -> String {
    toolchain()["toolchain"]["channel"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn make_variables(makefile: &str) -> HashMap<String, String> {
    makefile
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(":=").or_else(|| line.split_once("?="))?;
            let name = name.trim();
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
                .then(|| (name.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn expand_make(mut line: String, variables: &HashMap<String, String>) -> String {
    for _ in 0..variables.len().max(1) {
        let before = line.clone();
        for (name, value) in variables {
            line = line.replace(&format!("$({name})"), value);
        }
        if line == before {
            break;
        }
    }
    line
}

fn logical_lines(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for raw in text.lines() {
        let trimmed = raw.trim_end();
        current.push_str(trimmed.strip_suffix('\\').unwrap_or(trimmed));
        if trimmed.ends_with('\\') {
            current.push(' ');
        } else {
            result.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn cargo_command(fragment: &str) -> Option<(&str, bool)> {
    let words = fragment.split_whitespace().collect::<Vec<_>>();
    let cargo = words.iter().position(|word| {
        word.trim_matches(|c: char| matches!(c, '@' | '"' | '\'')) == "cargo"
            || word.contains("$(cargo")
            || word.contains("$(CARGO)")
    })?;
    let tail = &words[cargo + 1..];
    let first = tail
        .iter()
        .find(|word| !word.starts_with('+') && !word.starts_with('-'))?;
    let subcommand = first.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
    Some((
        subcommand,
        fragment.split_whitespace().any(|word| {
            word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-') == "--locked"
        }),
    ))
}

fn scan_policy(
    makefile: &str,
    containerfile: &str,
    scripts: &[String],
) -> Result<ScanCounts, String> {
    let variables = make_variables(makefile);
    let mut counts = ScanCounts::default();
    let mut sources = vec![
        ("Makefile", makefile.to_owned()),
        ("Containerfile", containerfile.to_owned()),
    ];
    sources.extend(scripts.iter().cloned().map(|text| ("script", text)));

    for (source, text) in sources {
        for logical in logical_lines(&text) {
            let was_wrapper = logical.contains("$(CARGO)");
            if was_wrapper
                && logical.contains("$(CARGO_LOCKED)")
                && !variables.contains_key("CARGO_LOCKED")
            {
                return Err("unresolvable Make lock indirection".into());
            }
            let expanded = expand_make(logical.clone(), &variables);
            if expanded.contains("$(CARGO)") || expanded.contains("$(CARGO_LOCKED)") {
                return Err(format!("unresolvable Cargo indirection in {source}"));
            }
            for fragment in expanded.split("&&") {
                let Some((subcommand, locked)) = cargo_command(fragment) else {
                    continue;
                };
                counts.inspected += 1;
                if was_wrapper {
                    counts.make_wrapper += 1;
                }
                if source == "Containerfile" && logical.contains("&&") {
                    counts.nested_container += 1;
                }
                let version_query = fragment.contains("--version");
                let resolving = !version_query
                    && (LOCKED_SUBCOMMANDS.contains(&subcommand)
                        || (subcommand == "deny" && fragment.contains(" check ")));
                let exempt = matches!(subcommand, "fmt" | "generate-rpm" | "clean" | "update")
                    || (subcommand == "deny" && (fragment.contains(" fetch ") || version_query))
                    || version_query
                    || subcommand == "--version";
                if resolving && !locked {
                    return Err(format!(
                        "resolving Cargo invocation lacks --locked: {fragment}"
                    ));
                }
                if !resolving && !exempt {
                    return Err(format!("unclassified Cargo invocation: {fragment}"));
                }
            }
        }
    }
    if counts.inspected == 0 || counts.nested_container == 0 || counts.make_wrapper == 0 {
        return Err("policy scan did not inspect required command classes".into());
    }
    Ok(counts)
}

fn policy_sources() -> (String, String, Vec<String>) {
    let root = workspace_root();
    let makefile = fs::read_to_string(root.join("Makefile")).unwrap();
    let containerfile = fs::read_to_string(root.join("packaging/Containerfile")).unwrap();
    let mut paths = fs::read_dir(root.join("scripts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sh"))
        .collect::<Vec<_>>();
    paths.sort();
    let scripts = paths
        .into_iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    (makefile, containerfile, scripts)
}

fn make_with_fake_cargo(version: Option<&str>) -> Output {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let cargo = bin.join("cargo");
    let body = version.map_or_else(
        || "#!/bin/sh\nexit 127\n".to_owned(),
        |version| format!("#!/bin/sh\nif [ \"$1 $2\" = \"deny --version\" ]; then echo '{version}'; exit 0; fi\nexit 97\n"),
    );
    fs::write(&cargo, body).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    Command::new(command_path("make"))
        .arg("--no-print-directory")
        .arg("check-cargo-deny")
        .current_dir(workspace_root())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap()
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

// AC: the exact committed Rust toolchain declaration is mandatory and complete.
#[test]
fn toolchain_file_is_required() {
    let config = toolchain();
    let selected = &config["toolchain"];
    assert!(
        selected["channel"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(selected["profile"].as_str(), Some("minimal"));
    assert_eq!(
        selected["components"].as_array().unwrap(),
        &[
            toml::Value::String("rustfmt".into()),
            toml::Value::String("clippy".into())
        ]
    );
    assert_eq!(
        selected["targets"].as_array().unwrap(),
        &[toml::Value::String("x86_64-unknown-linux-gnu".into())]
    );
}

// AC: every compiler declaration is derived from the toolchain-file authority.
#[test]
fn compiler_declarations_match_toolchain_authority() {
    let root = workspace_root();
    let expected = pin();
    let manifest = read_toml(&root.join("Cargo.toml"));
    assert_eq!(
        manifest["workspace"]["package"]["rust-version"].as_str(),
        Some(expected.as_str())
    );
    let container = fs::read_to_string(root.join("packaging/Containerfile")).unwrap();
    let declarations = container
        .lines()
        .filter_map(|line| line.strip_prefix("ARG RUST_VERSION="))
        .collect::<Vec<_>>();
    assert_eq!(declarations, vec![expected.as_str(), expected.as_str()]);
}

// AC: dependency-resolving commands stay locked across Make, containers, and scripts.
#[test]
fn locked_policy_covers_nested_container_commands() {
    let (makefile, container, scripts) = policy_sources();
    let counts = scan_policy(&makefile, &container, &scripts).unwrap();
    assert!(counts.inspected > 0);
    assert!(counts.nested_container > 0);
}

// AC: Make command wrappers and wrapper-carried lock flags cannot evade policy.
#[test]
fn locked_policy_resolves_make_wrappers() {
    let (makefile, container, scripts) = policy_sources();
    let counts = scan_policy(&makefile, &container, &scripts).unwrap();
    assert!(counts.make_wrapper > 0);
}

// AC: every workspace member inherits the workspace lint floor.
#[test]
fn workspace_members_inherit_workspace_lints() {
    let root = workspace_root();
    let workspace = read_toml(&root.join("Cargo.toml"));
    assert_eq!(
        workspace["workspace"]["lints"]["rust"]["unsafe_code"].as_str(),
        Some("deny")
    );
    for member in workspace["workspace"]["members"].as_array().unwrap() {
        let manifest = read_toml(&root.join(member.as_str().unwrap()).join("Cargo.toml"));
        assert_eq!(manifest["lints"]["workspace"].as_bool(), Some(true));
    }
}

// AC: an absent cargo-deny executable is a named hard failure, never a skip.
#[test]
fn cargo_deny_missing_fails_loudly() {
    let output = make_with_fake_cargo(None);
    assert!(!output.status.success());
    assert!(combined_output(&output).contains("cargo-deny not found"));
}

// AC: a cargo-deny version skew is a named hard failure.
#[test]
fn cargo_deny_version_skew_fails_loudly() {
    let output = make_with_fake_cargo(Some("cargo-deny 0.20.1"));
    assert!(!output.status.success());
    assert!(combined_output(&output).contains("cargo-deny version mismatch"));
}

// AC: dependency policy cannot regain a success-producing missing-tool branch.
#[test]
fn cargo_deny_cannot_skip_dependency_policy() {
    let makefile = fs::read_to_string(workspace_root().join("Makefile")).unwrap();
    assert!(!makefile.contains("skipping cargo deny"));
    assert!(makefile.contains("cargo deny $(CARGO_LOCKED) --offline check licenses bans sources"));
}

// AC: ambient toolchain skew is rejected before any Cargo gate work can run.
#[test]
fn ambient_toolchain_override_cannot_escape_preflight() {
    let output = Command::new(command_path("make"))
        .args(["--no-print-directory", "ci"])
        .current_dir(workspace_root())
        .env("RUSTUP_TOOLCHAIN", "stable")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = combined_output(&output);
    assert!(text.contains(&format!("expected {}", pin())));
    assert!(text.contains("unset RUSTUP_TOOLCHAIN"));
    assert!(!text.contains("cargo clippy"));
}

// AC: unsafe Rust remains confined to the reviewed startup environment seam.
#[test]
fn unsafe_code_is_confined_to_session_environment_wrapper() {
    let source = workspace_root().join("crates/solstone-linux/src");
    let mut unsafe_blocks = Vec::<PathBuf>::new();
    let mut allowances = Vec::<PathBuf>::new();
    for entry in fs::read_dir(&source).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "rs")
            || path.file_name().unwrap() == "toolchain_policy_tests.rs"
        {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        unsafe_blocks.extend(std::iter::repeat_n(
            path.clone(),
            text.matches("unsafe {").count(),
        ));
        allowances.extend(std::iter::repeat_n(
            path.clone(),
            text.matches("#[allow(unsafe_code)]").count(),
        ));
    }
    let cli = source.join("cli.rs");
    assert_eq!(unsafe_blocks, vec![cli.clone(), cli.clone()]);
    assert_eq!(allowances, vec![cli.clone(), cli]);
}

// AC: package-tool mirrors equal their Makefile authorities without test literals.
#[test]
fn package_tool_versions_match_authority() {
    let root = workspace_root();
    let makefile = fs::read_to_string(root.join("Makefile")).unwrap();
    let variables = make_variables(&makefile);
    let container = fs::read_to_string(root.join("packaging/Containerfile")).unwrap();
    for (make_name, container_name) in [
        ("CARGO_DEB_VERSION", "CARGO_DEB_VERSION"),
        ("CARGO_GENERATE_RPM_VERSION", "CARGO_GENERATE_RPM_VERSION"),
    ] {
        let value = &variables[make_name];
        assert!(
            container
                .lines()
                .any(|line| line == format!("ARG {container_name}={value}"))
        );
    }
    assert!(!variables["CARGO_DENY_VERSION"].is_empty());
    assert!(makefile.contains("cargo-deny $(CARGO_DENY_VERSION)"));
}

// AC: dependency policy retains explicit wildcard and unknown-source denial.
#[test]
fn dependency_policy_denies_wildcards_and_unknown_sources() {
    let deny = read_toml(&workspace_root().join("deny.toml"));
    assert_eq!(deny["bans"]["wildcards"].as_str(), Some("deny"));
    assert_eq!(deny["sources"]["unknown-registry"].as_str(), Some("deny"));
    assert_eq!(deny["sources"]["unknown-git"].as_str(), Some("deny"));
    for ignored in deny["advisories"]["ignore"].as_array().unwrap() {
        let reason = ignored["reason"].as_str().unwrap();
        assert!(reason.contains("Owner: sol pbc engineering."));
        assert!(reason.contains("Build-only via wayland-scanner; it parses crate-bundled protocol XML, not runtime or untrusted input."));
        assert!(reason.contains("Remove when wayland-scanner accepts quick-xml >=0.41."));
    }
}
