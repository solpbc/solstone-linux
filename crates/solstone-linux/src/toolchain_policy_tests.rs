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

#[derive(Debug, Default)]
struct ScanCounts {
    inspected: usize,
    resolving: usize,
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
    let mut variables = makefile
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(":=").or_else(|| line.split_once("?="))?;
            let name = name.trim();
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
                .then(|| (name.to_owned(), value.trim().to_owned()))
        })
        .collect::<HashMap<_, _>>();
    variables
        .entry("HOME".into())
        .or_insert("/home/user".into());
    variables.entry("MAKE".into()).or_insert("make".into());
    variables
        .entry("PATH".into())
        .or_insert("/usr/bin:/bin".into());
    variables
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

fn shell_variables(text: &str, base: &HashMap<String, String>) -> HashMap<String, String> {
    let mut variables = base.clone();
    for line in text.lines() {
        let Some((name, value)) = line.trim().split_once('=') else {
            continue;
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character.is_ascii_uppercase() || character == '_')
        {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']);
        let value = value
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .unwrap_or(value);
        if !value.contains('$') && !value.contains('`') {
            variables.insert(name.to_owned(), value.to_owned());
        }
    }
    variables
}

fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut start = 1;
    for (index, raw) in text.lines().enumerate() {
        if current.is_empty() {
            start = index + 1;
        }
        let trimmed = raw.trim_end();
        current.push_str(trimmed.strip_suffix('\\').unwrap_or(trimmed));
        if trimmed.ends_with('\\') {
            current.push(' ');
        } else {
            result.push((start, std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        result.push((start, current));
    }
    result
}

fn expand_shell_variables(text: String, variables: &HashMap<String, String>) -> String {
    let mut expanded = String::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' || chars.get(index + 1) == Some(&'$') {
            expanded.push(chars[index]);
            index += 1;
            continue;
        }
        let braced = chars.get(index + 1) == Some(&'{');
        let name_start = index + if braced { 2 } else { 1 };
        let mut name_end = name_start;
        while chars.get(name_end).is_some_and(|character| {
            character.is_ascii_uppercase() || *character == '_' || character.is_ascii_digit()
        }) {
            name_end += 1;
        }
        let reference_end = if braced {
            chars[name_end..]
                .iter()
                .position(|character| *character == '}')
                .map(|offset| name_end + offset)
        } else {
            Some(name_end)
        };
        if name_end == name_start || reference_end.is_none() {
            expanded.push(chars[index]);
            index += 1;
            continue;
        }
        let reference_end = reference_end.unwrap();
        let name = chars[name_start..name_end].iter().collect::<String>();
        if let Some(value) = variables.get(&name) {
            expanded.push_str(value);
            index = reference_end + usize::from(braced);
        } else {
            expanded.extend(chars[index..reference_end + usize::from(braced)].iter());
            index = reference_end + usize::from(braced);
        }
    }
    expanded
}

fn command_segments(line: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut single_quote = false;
    let mut double_quote = false;
    let mut conditional_test = false;
    while let Some(character) = chars.next() {
        match character {
            '\'' if !double_quote => single_quote = !single_quote,
            '"' if !single_quote => double_quote = !double_quote,
            '[' if !single_quote && !double_quote && chars.peek() == Some(&'[') => {
                conditional_test = true;
            }
            ']' if !single_quote && !double_quote && chars.peek() == Some(&']') => {
                conditional_test = false;
            }
            ';' if !single_quote && !double_quote && !conditional_test => {
                segments.push(std::mem::take(&mut current));
                continue;
            }
            '&' | '|'
                if !single_quote
                    && !double_quote
                    && !conditional_test
                    && chars.peek() == Some(&character) =>
            {
                chars.next();
                segments.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        segments.push(current);
    }
    segments
}

fn unresolved_reference(word: &str) -> Option<String> {
    for prefix in ["$(", "${"] {
        if let Some(start) = word.find(prefix) {
            let suffix = if prefix == "$(" { ')' } else { '}' };
            if let Some(end) = word[start + 2..].find(suffix) {
                let token = &word[start..start + 2 + end + 1];
                let name = &word[start + 2..start + 2 + end];
                if name
                    .chars()
                    .all(|character| character.is_ascii_uppercase() || character == '_')
                {
                    return Some(token.to_owned());
                }
            }
        }
    }
    if let Some(start) = word.find('$') {
        let name = word[start + 1..]
            .chars()
            .take_while(|character| character.is_ascii_uppercase() || *character == '_')
            .collect::<String>();
        if !name.is_empty() {
            return Some(format!("${name}"));
        }
    }
    None
}

fn command_position<'a>(words: &'a [&'a str]) -> Option<&'a str> {
    words.iter().copied().find(|word| {
        let word = word.trim_start_matches(['@', '-', '{', '}']);
        !word.is_empty()
            && !matches!(word, "RUN" | "if" | "then" | "elif" | "else" | "do")
            && !word.contains('=')
    })
}

fn after_case_pattern(fragment: &str) -> &str {
    let trimmed = fragment.trim_start();
    if trimmed.contains("$(") {
        return fragment;
    }
    trimmed
        .find(')')
        .map_or(fragment, |end| &trimmed[end + 1..])
}

fn is_cargo_word(word: &str) -> bool {
    let embedded = word
        .find("$(cargo")
        .map_or(word, |index| &word[index + 2..]);
    let cleaned = embedded.trim_matches(|character: char| {
        matches!(character, '@' | '"' | '\'' | '(' | ')' | '{' | '}' | ';')
    });
    PathBuf::from(cleaned)
        .file_name()
        .is_some_and(|name| name == "cargo")
}

struct CargoCommand<'a> {
    subcommand: &'a str,
    locked: bool,
    version_query: bool,
    deny_check: bool,
    deny_fetch: bool,
}

fn cargo_commands(fragment: &str) -> Vec<CargoCommand<'_>> {
    let words = fragment.split_whitespace().collect::<Vec<_>>();
    let cargo_indices = words
        .iter()
        .enumerate()
        .filter(|(_, word)| is_cargo_word(word))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    cargo_indices
        .iter()
        .enumerate()
        .filter_map(|(position, cargo)| {
            let end = cargo_indices
                .get(position + 1)
                .copied()
                .unwrap_or(words.len());
            let tail = &words[cargo + 1..end];
            let subcommand = tail
                .iter()
                .find(|word| !word.starts_with('+') && !word.starts_with('-'))?
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '-'
                });
            let has_word = |expected: &str| {
                tail.iter().any(|word| {
                    word.trim_matches(|character: char| {
                        !character.is_ascii_alphanumeric() && character != '-'
                    }) == expected
                })
            };
            Some(CargoCommand {
                subcommand,
                locked: has_word("--locked"),
                version_query: has_word("--version"),
                deny_check: has_word("check"),
                deny_fetch: has_word("fetch"),
            })
        })
        .collect()
}

fn scan_policy(
    makefile: &str,
    containerfile: &str,
    scripts: &[(String, String)],
) -> Result<ScanCounts, String> {
    let variables = make_variables(makefile);
    let mut counts = ScanCounts::default();
    let mut sources = vec![
        ("Makefile".to_owned(), makefile.to_owned()),
        ("Containerfile".to_owned(), containerfile.to_owned()),
    ];
    sources.extend(scripts.iter().cloned());

    for (source, text) in sources {
        let source_variables = shell_variables(&text, &variables);
        for (line_number, logical) in logical_lines(&text) {
            if (source == "Makefile" && !logical.starts_with('\t'))
                || (source == "Containerfile" && !logical.starts_with("RUN "))
                || logical.trim_start().starts_with('#')
            {
                continue;
            }
            let was_wrapper = logical.contains("$(CARGO)");
            if was_wrapper
                && logical.contains("$(CARGO_LOCKED)")
                && !variables.contains_key("CARGO_LOCKED")
            {
                return Err("unresolvable Make lock indirection".into());
            }
            let expanded =
                expand_shell_variables(expand_make(logical.clone(), &variables), &source_variables);
            for fragment in command_segments(&expanded) {
                let command_fragment = if source.starts_with("scripts/") {
                    after_case_pattern(&fragment)
                } else {
                    &fragment
                };
                let words = command_fragment.split_whitespace().collect::<Vec<_>>();
                if let Some(command) = command_position(&words)
                    && let Some(token) = unresolved_reference(command)
                {
                    return Err(format!(
                        "{source}:{line_number}: unresolved command token '{token}'"
                    ));
                }
                let command = command_position(&words).unwrap_or_default();
                if matches!(command.trim_matches(['@', '{', '}']), "echo" | "printf") {
                    continue;
                }
                for cargo in cargo_commands(&fragment) {
                    counts.inspected += 1;
                    if was_wrapper {
                        counts.make_wrapper += 1;
                    }
                    if source == "Containerfile" && logical.contains("&&") {
                        counts.nested_container += 1;
                    }
                    let resolving = !cargo.version_query
                        && (LOCKED_SUBCOMMANDS.contains(&cargo.subcommand)
                            || (cargo.subcommand == "deny" && cargo.deny_check));
                    if resolving {
                        counts.resolving += 1;
                    }
                    let exempt = matches!(
                        cargo.subcommand,
                        "fmt" | "generate-rpm" | "clean" | "update"
                    ) || (cargo.subcommand == "deny"
                        && (cargo.deny_fetch || cargo.version_query))
                        || cargo.version_query
                        || cargo.subcommand == "--version";
                    if resolving && !cargo.locked {
                        return Err(format!(
                            "{source}:{line_number}: resolving Cargo invocation lacks --locked: {fragment}"
                        ));
                    }
                    if !resolving && !exempt {
                        return Err(format!(
                            "{source}:{line_number}: unclassified Cargo invocation: {fragment}"
                        ));
                    }
                }
            }
        }
    }
    if counts.inspected == 0
        || counts.resolving == 0
        || counts.nested_container == 0
        || counts.make_wrapper == 0
    {
        return Err("policy scan did not inspect required command classes".into());
    }
    Ok(counts)
}

fn policy_sources() -> (String, String, Vec<(String, String)>) {
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
        .map(|path| {
            let name = path.strip_prefix(&root).unwrap().display().to_string();
            (name, fs::read_to_string(path).unwrap())
        })
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
        .env("CARGO_HOME", temp.path())
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

// AC: unresolved command indirection is a named hard failure, never an ignored command.
#[test]
fn locked_policy_fails_closed_on_unresolved_command_wrapper() {
    let (mut makefile, container, scripts) = policy_sources();
    makefile.push_str("\npolicy-probe:\n\t$(SOME_UNDEFINED_CMD) build --locked\n");
    let error = scan_policy(&makefile, &container, &scripts).unwrap_err();
    assert!(error.contains("Makefile:"));
    assert!(error.contains("$(SOME_UNDEFINED_CMD)"));
}

// AC: a scan containing only exempt Cargo commands cannot satisfy lock-policy coverage.
#[test]
fn locked_policy_requires_a_resolving_invocation() {
    let makefile = "CARGO := cargo\nprobe:\n\t$(CARGO) fmt\n";
    let container = "RUN cargo fmt && cargo generate-rpm -p crates/solstone-linux\n";
    let error = scan_policy(makefile, container, &[]).unwrap_err();
    assert!(error.contains("required command classes"));
}

// AC: shell variables, Make wrappers, paths, and compound commands cannot evade inspection.
#[test]
fn locked_policy_recognizes_general_cargo_command_forms() {
    let (mut makefile, mut container, mut scripts) = policy_sources();
    makefile.push_str("\nCARGO_CMD := /usr/bin/cargo\npolicy-probe:\n\t$(CARGO_CMD) build --locked\n\t$(CARGO_BIN_DIR)/cargo test --locked\n");
    container.push_str(
        "\nRUN /usr/bin/cargo build --locked; cargo test --locked || cargo clippy --locked\n",
    );
    scripts.push((
        "scripts/policy-probe.sh".into(),
        "$CARGO build --locked\n${CARGO} test --locked\n".into(),
    ));
    scan_policy(&makefile, &container, &scripts).unwrap();
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
