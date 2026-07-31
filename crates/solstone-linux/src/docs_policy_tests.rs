// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    cli::Args,
    policy_test_support::authority_vocabulary::{
        LEGACY_COMMANDS, LEGACY_ENVIRONMENT, LEGACY_OPTIONS, LEGACY_ORIGINS, PYTHON_SETUP,
    },
    release_rail_tests::workspace_root,
};
use clap::CommandFactory;
use std::{fmt, fs, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocsError {
    surface: String,
    line: usize,
    rule: &'static str,
    detail: String,
}

impl fmt::Display for DocsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "docs policy: surface={} line={} rule={} detail={}",
            self.surface, self.line, self.rule, self.detail
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChangelogSection {
    Current,
    Historical,
}

fn is_release_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("## [") else {
        return false;
    };
    let Some((version, date)) = rest.split_once("] - ") else {
        return false;
    };
    let semver = version.split('.').collect::<Vec<_>>();
    semver.len() == 3
        && semver
            .iter()
            .all(|component| !component.is_empty() && component.chars().all(|c| c.is_ascii_digit()))
        && date.len() == 10
        && date.chars().enumerate().all(|(index, character)| {
            matches!(index, 4 | 7) && character == '-'
                || !matches!(index, 4 | 7) && character.is_ascii_digit()
        })
}

fn current_changelog_lines(text: &str) -> Result<Vec<(usize, &str)>, DocsError> {
    let mut section = ChangelogSection::Current;
    let mut output = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.starts_with("## [") {
            section = if line == "## [Unreleased]" {
                ChangelogSection::Current
            } else if is_release_heading(line) {
                ChangelogSection::Historical
            } else {
                return Err(DocsError {
                    surface: "CHANGELOG.md".to_owned(),
                    line: index + 1,
                    rule: "changelog-heading",
                    detail: line.to_owned(),
                });
            };
        }
        if section == ChangelogSection::Current {
            output.push((index + 1, line));
        }
    }
    Ok(output)
}

fn normalize_markdown_line(line: &str) -> String {
    line.trim()
        .trim_start_matches(['-', '*', '>'])
        .replace(['`', '[', ']', '(', ')', '<', '>', '"', '\''], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn forbidden_current_text(normalized: &str) -> Option<(&'static str, String)> {
    for token in LEGACY_ENVIRONMENT
        .iter()
        .chain(LEGACY_OPTIONS)
        .chain(LEGACY_ORIGINS)
        .chain(LEGACY_COMMANDS)
        .chain(PYTHON_SETUP)
    {
        if normalized.contains(&token.to_ascii_lowercase()) {
            return Some(("legacy-instruction", (*token).to_owned()));
        }
    }
    let positive_fallback = (normalized.contains("direct fallback")
        || normalized.contains("fallback connection"))
        && !normalized.contains("no direct fallback")
        && !normalized.contains("there is no");
    if positive_fallback {
        return Some(("direct-fallback", "fallback".to_owned()));
    }
    let local_install = (normalized.contains("install")
        && (normalized.contains("local journal") || normalized.contains("local python")))
        && !normalized.contains("no local")
        && !normalized.contains("there is no");
    if local_install {
        return Some(("local-install", "local journal/python".to_owned()));
    }
    if normalized.contains("observer key")
        && !normalized.contains("no observer key")
        && !normalized.contains("there is no")
    {
        return Some(("observer-key-minting", "observer key".to_owned()));
    }
    None
}

fn scan_lines<'a>(
    surface: &str,
    lines: impl IntoIterator<Item = (usize, &'a str)>,
) -> Result<(), DocsError> {
    let mut fenced = false;
    let mut paragraph = String::new();
    let mut paragraph_line = 1;
    let inspect = |text: &str, line: usize, fenced: bool| {
        let normalized = normalize_markdown_line(text);
        forbidden_current_text(&normalized).map(|(rule, detail)| DocsError {
            surface: surface.to_owned(),
            line,
            rule,
            detail: if fenced {
                format!("command:{detail}")
            } else {
                detail
            },
        })
    };
    for (line_number, line) in lines {
        if line.trim_start().starts_with("```") {
            if let Some(error) = inspect(&paragraph, paragraph_line, fenced) {
                return Err(error);
            }
            paragraph.clear();
            fenced = !fenced;
            continue;
        }
        if line.trim().is_empty() || line.starts_with('#') {
            if let Some(error) = inspect(&paragraph, paragraph_line, fenced) {
                return Err(error);
            }
            paragraph.clear();
            if line.starts_with('#')
                && let Some(error) = inspect(line, line_number, fenced)
            {
                return Err(error);
            }
            continue;
        }
        if paragraph.is_empty() {
            paragraph_line = line_number;
        } else {
            paragraph.push(' ');
        }
        paragraph.push_str(line);
    }
    if let Some(error) = inspect(&paragraph, paragraph_line, fenced) {
        return Err(error);
    }
    Ok(())
}

fn scan_markdown(path: &Path) -> Result<(), DocsError> {
    let text = fs::read_to_string(path).unwrap();
    let surface = path.file_name().unwrap().to_string_lossy();
    scan_lines(
        &surface,
        text.lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line)),
    )
}

fn rendered_help() -> Vec<(String, String)> {
    fn collect(command: &clap::Command, output: &mut Vec<(String, String)>) {
        let mut command = command.clone();
        let name = command.get_name().to_owned();
        let mut bytes = Vec::new();
        command.write_long_help(&mut bytes).unwrap();
        output.push((name, String::from_utf8(bytes).unwrap()));
        for child in command.get_subcommands() {
            collect(child, output);
        }
    }
    let mut output = Vec::new();
    collect(&Args::command(), &mut output);
    output
}

#[test]
fn docs_current_instructions_have_no_legacy_authority() {
    let root = workspace_root();
    for relative in ["README.md", "INSTALL.md", "packaging/INSTALL-NOTES"] {
        scan_markdown(&root.join(relative)).unwrap();
    }
    let changelog = fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    scan_lines("CHANGELOG.md", current_changelog_lines(&changelog).unwrap()).unwrap();
    for (command, help) in rendered_help() {
        scan_lines(
            &format!("help:{command}"),
            help.lines()
                .enumerate()
                .map(|(index, line)| (index + 1, line)),
        )
        .unwrap();
    }
}

#[test]
fn docs_changelog_history_is_structural() {
    let changelog = "before\n## [Unreleased]\ncurrent\n## [1.2.3] - 2026-01-02\n--server-url\n";
    let lines = current_changelog_lines(changelog).unwrap();
    assert!(!lines.iter().any(|(_, line)| line.contains("--server-url")));
    let malformed = "## [next]\n";
    assert_eq!(
        current_changelog_lines(malformed).unwrap_err().rule,
        "changelog-heading"
    );
}

#[test]
fn docs_mutations_reject_prose_links_code_and_fenced_commands() {
    for text in [
        "use --server-url now",
        "[journal](http://localhost:5015)",
        "`SOLSTONE_TOKEN=x`",
        "```bash\npipx install solstone-linux\n```",
        "mint an observer key",
        "use a direct fallback connection",
    ] {
        assert!(
            scan_lines(
                "fixture",
                text.lines().enumerate().map(|(i, line)| (i + 1, line))
            )
            .is_err()
        );
    }
}
