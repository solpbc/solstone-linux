// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::{Deserialize, Serialize};

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
        let (journal_line, journal_current, journal_seen_at) = match saved {
            Some(saved)
                if crate::private_link::sanitize_journal_version(&saved.version).is_some() =>
            {
                let facts = saved.about.as_ref().filter(|facts| {
                    facts.valid()
                        && facts.version.trim_start_matches('v')
                            == saved.version.trim_start_matches('v')
                });
                let line = facts
                    .map(|facts| facts.about.clone())
                    .unwrap_or_else(|| render_line("journal", &saved.version, None, "", "", ""));
                let seen = (saved.observed_at.is_finite()
                    && saved.observed_at >= 0.0
                    && saved.observed_at < u64::MAX as f64)
                    .then_some(saved.observed_at as u64);
                (line, current, seen)
            }
            _ => ("journal unknown".into(), false, None),
        };
        let mut displayed = journal_line.clone();
        if !journal_current && let Some(seen) = journal_seen_at {
            displayed.push_str(SEPARATOR);
            displayed.push_str("last seen ");
            displayed.push_str(&age(now.saturating_sub(seen)));
        }
        let own = render_line(
            "linux desktop app",
            env!("CARGO_PKG_VERSION"),
            None,
            &host.os,
            &host.os_version,
            &host.arch,
        );
        Self {
            text: format!("{own}\n{displayed}"),
            host,
            journal_line,
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
    let (value, unit) = match seconds {
        0..60 => return "just now".into(),
        60..3600 => (seconds / 60, "minute"),
        3600..86400 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    format!("{value} {unit}{} ago", if value == 1 { "" } else { "s" })
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
