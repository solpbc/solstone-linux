// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::{Deserialize, Serialize};
use solstone_i18n::{message, text};

use crate::config::Config;

const SEPARATOR: &str = " · ";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct About {
    pub protocol_version: u32,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub about: String,
}

pub fn normalize_arch(raw: &str) -> &str {
    match raw {
        "aarch64" | "ARM64" | "arm64-v8a" | "arm64" => "arm64",
        "amd64" | "x64" | "AMD64" | "x86_64" => "x86_64",
        other => other,
    }
}

pub fn render_line(
    name: &str,
    version: &str,
    build: Option<&str>,
    os: &str,
    os_version: &str,
    arch: &str,
) -> String {
    let mut line = format!("{name} {}", version.trim_start_matches('v'));
    if let Some(build) = build.filter(|value| !value.is_empty()) {
        line.push_str(&format!(" ({build})"));
    }
    if !os.is_empty() {
        line.push_str(SEPARATOR);
        line.push_str(os);
        if !os_version.is_empty() {
            line.push(' ');
            line.push_str(os_version);
        }
    }
    if !arch.is_empty() {
        line.push_str(SEPARATOR);
        line.push_str(normalize_arch(arch));
    }
    line
}

fn display_line(
    name: &str,
    version: &str,
    build: Option<&str>,
    os: &str,
    os_version: &str,
    arch: &str,
) -> String {
    let version = version.trim_start_matches('v');
    let arch = (!arch.is_empty()).then(|| normalize_arch(arch));
    let os = (!os.is_empty()).then_some(os);
    let os_version = os.and((!os_version.is_empty()).then_some(os_version));
    let build = build.filter(|value| !value.is_empty());
    match (build, os, os_version, arch) {
        (None, None, None, None) => {
            message!("linux.about.version", "name" => name, "version" => version)
        }
        (None, None, None, Some(arch)) => {
            message!("linux.about.versionarch", "name" => name, "version" => version, "arch" => arch)
        }
        (None, Some(os), None, None) => {
            message!("linux.about.versionos", "name" => name, "version" => version, "os" => os)
        }
        (None, Some(os), None, Some(arch)) => {
            message!("linux.about.versionosarch", "name" => name, "version" => version, "os" => os, "arch" => arch)
        }
        (None, Some(os), Some(os_version), None) => {
            message!("linux.about.versionosversion", "name" => name, "version" => version, "os" => os, "osversion" => os_version)
        }
        (None, Some(os), Some(os_version), Some(arch)) => {
            message!("linux.about.versionosversionarch", "name" => name, "version" => version, "os" => os, "osversion" => os_version, "arch" => arch)
        }
        (Some(build), None, None, None) => {
            message!("linux.about.versionbuild", "name" => name, "version" => version, "build" => build)
        }
        (Some(build), None, None, Some(arch)) => {
            message!("linux.about.versionbuildarch", "name" => name, "version" => version, "build" => build, "arch" => arch)
        }
        (Some(build), Some(os), None, None) => {
            message!("linux.about.versionbuildos", "name" => name, "version" => version, "build" => build, "os" => os)
        }
        (Some(build), Some(os), None, Some(arch)) => {
            message!("linux.about.versionbuildosarch", "name" => name, "version" => version, "build" => build, "os" => os, "arch" => arch)
        }
        (Some(build), Some(os), Some(os_version), None) => {
            message!("linux.about.versionbuildosversion", "name" => name, "version" => version, "build" => build, "os" => os, "osversion" => os_version)
        }
        (Some(build), Some(os), Some(os_version), Some(arch)) => {
            message!("linux.about.versionbuildosversionarch", "name" => name, "version" => version, "build" => build, "os" => os, "osversion" => os_version, "arch" => arch)
        }
        (_, None, Some(_), _) => unreachable!("an OS version requires an OS name"),
    }
}

impl About {
    pub fn valid(&self) -> bool {
        self.protocol_version == 1
            && !self.version.is_empty()
            && self.build.as_ref().is_none_or(|build| !build.is_empty())
            && [
                &self.version,
                &self.os,
                &self.os_version,
                &self.arch,
                &self.about,
            ]
            .into_iter()
            .chain(self.build.iter())
            .all(|value| !value.chars().any(char::is_control))
            && self.about.len() <= 8192
            && self.about
                == render_line(
                    "journal",
                    &self.version,
                    self.build.as_deref(),
                    &self.os,
                    &self.os_version,
                    &self.arch,
                )
    }
}

pub fn decode_about(bytes: &[u8]) -> Option<About> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("build").is_some_and(|build| !build.is_string()) {
        return None;
    }
    let about: About = serde_json::from_value(value).ok()?;
    about.valid().then_some(about)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostFacts {
    pub os: String,
    pub os_version: String,
    pub arch: String,
}

pub fn parse_os_release(text: &str) -> (String, String) {
    let value = |key| {
        text.lines().find_map(|line| {
            let (found, value) = line.split_once('=')?;
            if found != key {
                return None;
            }
            let value = value.trim().trim_matches(['"', '\'']);
            (!value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
            .then(|| value.to_owned())
        })
    };
    (
        value("ID").unwrap_or_else(|| "linux".into()),
        value("VERSION_ID").unwrap_or_default(),
    )
}

pub fn host_facts() -> HostFacts {
    let uname = rustix::system::uname();
    let (os, os_version) = match std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
    {
        Ok(text) => parse_os_release(&text),
        Err(_) => (
            "linux".into(),
            uname.release().to_string_lossy().into_owned(),
        ),
    };
    HostFacts {
        os,
        os_version,
        arch: normalize_arch(&uname.machine().to_string_lossy()).into(),
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AboutBlock {
    pub text: String,
    pub host: HostFacts,
    pub journal_line: String,
    /// The paired journal's version, without a leading `v`, when it is known.
    pub journal_version: Option<String>,
    pub journal_current: bool,
    pub journal_seen_at: Option<u64>,
}

impl AboutBlock {
    pub fn unknown(host: HostFacts) -> Self {
        Self::from_version(host, None, false, now())
    }

    pub fn from_version(
        host: HostFacts,
        saved: Option<&crate::sync_health::PairedJournalVersion>,
        current: bool,
        now: u64,
    ) -> Self {
        let (journal_line, shown_line, journal_version, journal_current, journal_seen_at) =
            match saved {
                Some(saved)
                    if crate::private_link::sanitize_journal_version(&saved.version).is_some() =>
                {
                    let facts = saved.about.as_ref().filter(|facts| {
                        facts.valid()
                            && facts.version.trim_start_matches('v')
                                == saved.version.trim_start_matches('v')
                    });
                    let line = facts.map(|facts| facts.about.clone()).unwrap_or_else(|| {
                        render_line("journal", &saved.version, None, "", "", "")
                    });
                    let seen = (saved.observed_at.is_finite()
                        && saved.observed_at >= 0.0
                        && saved.observed_at < u64::MAX as f64)
                        .then_some(saved.observed_at as u64);
                    let version = saved.version.trim_start_matches('v').to_owned();
                    let journal = text!("linux.about.journal");
                    let shown = facts.map_or_else(
                        || display_line(journal, &version, None, "", "", ""),
                        |facts| {
                            display_line(
                                journal,
                                &version,
                                facts.build.as_deref(),
                                &facts.os,
                                &facts.os_version,
                                &facts.arch,
                            )
                        },
                    );
                    (line, shown, Some(version), current, seen)
                }
                _ => (
                    solstone_i18n::render("en-US", "linux.about.unknown", &[]),
                    text!("linux.about.unknown").into(),
                    None,
                    false,
                    None,
                ),
            };
        let displayed = match journal_seen_at {
            Some(seen) if !journal_current => {
                message!("linux.about.lastseen", "journal" => shown_line, "age" => age(now.saturating_sub(seen)))
            }
            _ => shown_line,
        };
        let own = display_line(
            text!("linux.about.app"),
            env!("CARGO_PKG_VERSION"),
            None,
            &host.os,
            &host.os_version,
            &host.arch,
        );
        Self {
            text: message!("linux.about.block", "app" => own, "journal" => displayed),
            host,
            journal_line,
            journal_version,
            journal_current,
            journal_seen_at,
        }
    }

    pub fn snapshot(config: &Config, host: &HostFacts, now: u64) -> Self {
        use crate::private_link::{
            PrivateStateLock, PrivateStateLockLiveness, journal_identity_key, load_credential,
        };
        let Ok(Some(credential)) = load_credential(&config.config_dir) else {
            return Self::unknown(host.clone());
        };
        let identity = journal_identity_key(&credential);
        let saved = crate::sync_health::load_paired_journal_version(&config.state_dir())
            .filter(|saved| saved.identity_key == identity);
        let liveness = PrivateStateLock::try_probe(&config.config_dir)
            .unwrap_or(PrivateStateLockLiveness::NoLiveOwner);
        let facts = crate::sync_health::load_facts_with_liveness(&config.state_dir(), liveness);
        let current = liveness == PrivateStateLockLiveness::LiveOwner
            && facts.link.as_ref().is_some_and(|link| {
                link.journal_version_observed && link.carrier_proven && !link.transport_unavailable
            });
        // Re-read identity after both sidecars. A concurrent re-pair cannot expose
        // observations for a destination that has already been replaced.
        if load_credential(&config.config_dir)
            .ok()
            .flatten()
            .is_none_or(|latest| journal_identity_key(&latest) != identity)
        {
            return Self::unknown(host.clone());
        }
        Self::from_version(host.clone(), saved.as_ref(), current, now)
    }

    pub fn native(&self) -> NativeAbout {
        NativeAbout {
            protocol_version: 1,
            os: self.host.os.clone(),
            os_version: self.host.os_version.clone(),
            arch: self.host.arch.clone(),
            journal_line: self.journal_line.clone(),
            journal_current: self.journal_current,
            journal_seen_at_epoch_secs: self.journal_seen_at,
        }
    }
}

fn age(seconds: u64) -> String {
    let (value, message) = match seconds {
        0..60 => return text!("linux.about.now").into(),
        60..3600 => (seconds / 60, "linux.about.ageminute"),
        3600..86400 => (seconds / 3600, "linux.about.agehour"),
        _ => (seconds / 86400, "linux.about.ageday"),
    };
    solstone_i18n::cardinal(
        message,
        value as i64,
        &[(
            "formatted",
            solstone_i18n::Value::String(&value.to_string()),
        )],
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeAbout {
    pub protocol_version: u32,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub journal_line: String,
    pub journal_current: bool,
    pub journal_seen_at_epoch_secs: Option<u64>,
}
