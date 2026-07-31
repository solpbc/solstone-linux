// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    Error, Result, digest,
    elf64::{Elf64Linkage, parse_elf64},
};
use flate2::read::GzDecoder;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, File},
    io::{Cursor, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};
use tar::{Archive, EntryType};
use xz2::read::XzDecoder;

const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;
const INSTALL_NOTES: &[u8] = include_bytes!("../../../packaging/INSTALL-NOTES");

// Derived with:
// cargo build --locked --release -p solstone-linux
// SOLSTONE_ELF_DERIVE=<workspace>/target/release/solstone-linux \
//   cargo test --locked -p rust-release-manifest elf64::tests::derive_requested_release_binary -- --nocapture
// Source commit: 95ea20d9ae5a726f1b4058eab3122f07fbd86fbf.
const EXPECTED_ELF_TYPE: u16 = 3;
const EXPECTED_MACHINE: u16 = 62;
const EXPECTED_INTERPRETER: &str = "/lib64/ld-linux-x86-64.so.2";
const EXPECTED_NEEDED: [&str; 8] = [
    "libgstreamer-1.0.so.0",
    "libgobject-2.0.so.0",
    "libglib-2.0.so.0",
    "libgio-2.0.so.0",
    "libpulse.so.0",
    "libgcc_s.so.1",
    "libm.so.6",
    "libc.so.6",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PayloadRole {
    Executable,
    License,
    InstallNotes,
    Icon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PayloadAuthority {
    pub(crate) source: &'static str,
    pub(crate) installed: &'static str,
    pub(crate) mode: u32,
    pub(crate) role: PayloadRole,
}

pub(crate) const PAYLOAD_AUTHORITY: [PayloadAuthority; 16] = [
    PayloadAuthority {
        source: "target/release/solstone-linux",
        installed: "/usr/bin/solstone-linux",
        mode: 0o755,
        role: PayloadRole::Executable,
    },
    PayloadAuthority {
        source: "LICENSE",
        installed: "/usr/share/doc/solstone-linux/LICENSE",
        mode: 0o644,
        role: PayloadRole::License,
    },
    PayloadAuthority {
        source: "packaging/INSTALL-NOTES",
        installed: "/usr/share/doc/solstone-linux/INSTALL-NOTES",
        mode: 0o644,
        role: PayloadRole::InstallNotes,
    },
    icon(
        "contrib/icons/hicolor/16x16/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/16x16/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/24x24/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/24x24/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/32x32/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/32x32/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/48x48/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/48x48/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/64x64/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/64x64/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/128x128/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/128x128/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/256x256/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/256x256/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/512x512/apps/solstone-observer.png",
        "/usr/share/icons/hicolor/512x512/apps/solstone-observer.png",
    ),
    icon(
        "contrib/icons/hicolor/scalable/apps/solstone-observer.svg",
        "/usr/share/icons/hicolor/scalable/apps/solstone-observer.svg",
    ),
    icon(
        "contrib/icons/hicolor/scalable/status/solstone-error.svg",
        "/usr/share/icons/hicolor/scalable/status/solstone-error.svg",
    ),
    icon(
        "contrib/icons/hicolor/scalable/status/solstone-paused.svg",
        "/usr/share/icons/hicolor/scalable/status/solstone-paused.svg",
    ),
    icon(
        "contrib/icons/hicolor/scalable/status/solstone-recording.svg",
        "/usr/share/icons/hicolor/scalable/status/solstone-recording.svg",
    ),
    icon(
        "contrib/icons/hicolor/scalable/status/solstone-syncing.svg",
        "/usr/share/icons/hicolor/scalable/status/solstone-syncing.svg",
    ),
];

const fn icon(source: &'static str, installed: &'static str) -> PayloadAuthority {
    PayloadAuthority {
        source,
        installed,
        mode: 0o644,
        role: PayloadRole::Icon,
    }
}

#[derive(Clone, Debug)]
struct Member {
    path: String,
    mode: u32,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
enum Format {
    Tar,
    Deb,
    Rpm,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
        }
    }
}

fn audit_error(artifact: &Path, class: &str, token: &str, member: &str) -> Error {
    let artifact = artifact
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("<non-utf8>");
    let escape = |value: &str| {
        value
            .chars()
            .flat_map(char::escape_default)
            .collect::<String>()
    };
    Error::new(format!(
        "package audit: artifact={} class={} token={} member={} tool=rust-release-manifest",
        escape(artifact),
        escape(class),
        escape(token),
        escape(member)
    ))
}

fn regular_artifact(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| audit_error(path, "UnreadableArtifact", "metadata", "artifact"))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(audit_error(
            path,
            "InvalidArtifact",
            "regular-file-required",
            "artifact",
        ));
    }
    Ok(())
}

fn normalized_path(path: &Path) -> Option<String> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    path.to_str().map(str::to_owned)
}

fn tar_inventory<R: Read>(artifact: &Path, reader: R) -> Result<Vec<Member>> {
    let mut archive = Archive::new(reader);
    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    for entry in archive
        .entries()
        .map_err(|error| audit_error(artifact, "MalformedContainer", &error.to_string(), "tar"))?
    {
        let mut entry = entry.map_err(|error| {
            audit_error(artifact, "MalformedContainer", &error.to_string(), "tar")
        })?;
        let kind = entry.header().entry_type();
        if kind == EntryType::Directory {
            continue;
        }
        if kind != EntryType::Regular {
            return Err(audit_error(
                artifact,
                "UnsupportedMember",
                "non-regular",
                "tar",
            ));
        }
        let path = entry
            .path()
            .map_err(|_| audit_error(artifact, "MalformedContainer", "invalid-path", "tar"))?;
        let path = normalized_path(&path)
            .ok_or_else(|| audit_error(artifact, "PayloadClosure", "path-traversal", "tar"))?;
        if !seen.insert(path.clone()) {
            return Err(audit_error(artifact, "PayloadClosure", "duplicate", &path));
        }
        let mode = entry
            .header()
            .mode()
            .map_err(|_| audit_error(artifact, "MalformedContainer", "mode", &path))?;
        if entry.size() > MAX_MEMBER_BYTES {
            return Err(audit_error(artifact, "LimitExceeded", "member-size", &path));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|error| {
            audit_error(artifact, "MalformedContainer", &error.to_string(), &path)
        })?;
        members.push(Member { path, mode, bytes });
    }
    Ok(members)
}

fn tar_members(path: &Path) -> Result<Vec<Member>> {
    let reader = GzDecoder::new(
        File::open(path)
            .map_err(|error| audit_error(path, "UnreadableArtifact", &error.to_string(), "tar"))?,
    );
    let mut members = tar_inventory(path, reader)?;
    let root = members
        .first()
        .and_then(|member| member.path.split('/').next())
        .ok_or_else(|| audit_error(path, "PayloadClosure", "missing-root", "tar"))?
        .to_owned();
    for member in &mut members {
        member.path = member
            .path
            .strip_prefix(&format!("{root}/"))
            .ok_or_else(|| audit_error(path, "PayloadClosure", "multiple-roots", &member.path))?
            .to_owned();
    }
    Ok(members)
}

fn compressed_tar(path: &Path, name: &str, bytes: Vec<u8>) -> Result<Vec<Member>> {
    let reader: Box<dyn Read> = if name.ends_with(".gz") {
        Box::new(GzDecoder::new(Cursor::new(bytes)))
    } else if name.ends_with(".xz") {
        Box::new(XzDecoder::new(Cursor::new(bytes)))
    } else if name.ends_with(".zst") {
        Box::new(
            zstd::stream::read::Decoder::new(Cursor::new(bytes)).map_err(|error| {
                audit_error(path, "MalformedContainer", &error.to_string(), name)
            })?,
        )
    } else {
        return Err(audit_error(path, "UnsupportedCompression", name, "deb"));
    };
    tar_inventory(path, reader)
}

fn forbidden_dependency(value: &str) -> Option<&str> {
    value
        .split(|character: char| !(character.is_ascii_alphanumeric() || ".+-".contains(character)))
        .find(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "python" | "python2" | "python3" | "pip" | "pip3" | "pipx" | "sol" | "journal"
            )
        })
}

fn valid_deb_dependency_field(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(char::is_control)
        && value.split(',').all(|group| {
            !group.trim().is_empty()
                && group.split('|').all(|alternative| {
                    let alternative = alternative.trim();
                    let name = alternative
                        .split_once(char::is_whitespace)
                        .map_or(alternative, |(name, _)| name);
                    !name.is_empty()
                        && !name.starts_with('-')
                        && name.chars().all(|character| {
                            character.is_ascii_alphanumeric()
                                || matches!(character, '+' | '-' | '.' | ':' | '_')
                        })
                        && alternative.matches('(').count() == alternative.matches(')').count()
                })
        })
}

fn deb_members(path: &Path) -> Result<Vec<Member>> {
    let mut archive = ar::Archive::new(
        File::open(path)
            .map_err(|error| audit_error(path, "UnreadableArtifact", &error.to_string(), "deb"))?,
    );
    let mut names = BTreeSet::new();
    let mut marker = None;
    let mut control = None;
    let mut data = None;
    while let Some(entry) = archive.next_entry() {
        let mut entry = entry
            .map_err(|error| audit_error(path, "MalformedContainer", &error.to_string(), "ar"))?;
        let name = std::str::from_utf8(entry.header().identifier())
            .map_err(|_| audit_error(path, "MalformedContainer", "non-utf8-name", "ar"))?
            .trim_end_matches('/')
            .to_owned();
        if !names.insert(name.clone()) {
            return Err(audit_error(path, "MalformedContainer", "duplicate", &name));
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| audit_error(path, "MalformedContainer", &error.to_string(), &name))?;
        if name == "debian-binary" {
            marker = Some(bytes);
        } else if name.starts_with("control.tar.") {
            control = Some((name, bytes));
        } else if name.starts_with("data.tar.") {
            data = Some((name, bytes));
        } else {
            return Err(audit_error(
                path,
                "MalformedContainer",
                "extra-ar-member",
                &name,
            ));
        }
    }
    if marker.as_deref() != Some(b"2.0\n") || names.len() != 3 {
        return Err(audit_error(path, "MalformedContainer", "ar-closure", "deb"));
    }
    let (control_name, control_bytes) =
        control.ok_or_else(|| audit_error(path, "MalformedContainer", "missing-control", "deb"))?;
    let control = compressed_tar(path, &control_name, control_bytes)?;
    for member in &control {
        let name = member.path.trim_start_matches("./");
        if matches!(
            name,
            "preinst" | "postinst" | "prerm" | "postrm" | "config" | "triggers"
        ) {
            return Err(audit_error(
                path,
                "MaintainerScript",
                name,
                &format!("deb:control/{name}"),
            ));
        }
    }
    let control_member = control
        .iter()
        .find(|member| member.path.trim_start_matches("./") == "control")
        .ok_or_else(|| audit_error(path, "MalformedMetadata", "missing-control", "deb"))?;
    if !control
        .iter()
        .any(|member| member.path.trim_start_matches("./") == "md5sums")
    {
        return Err(audit_error(
            path,
            "MalformedMetadata",
            "missing-md5sums",
            "deb",
        ));
    }
    let control_body = std::str::from_utf8(&control_member.bytes)
        .map_err(|_| audit_error(path, "MalformedMetadata", "non-utf8-control", "deb"))?;
    let mut fields = BTreeMap::new();
    for line in control_body.lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| audit_error(path, "MalformedMetadata", "control-line", "deb"))?;
        if fields.insert(name, value.trim()).is_some() {
            return Err(audit_error(
                path,
                "MalformedMetadata",
                "duplicate-field",
                name,
            ));
        }
    }
    for required in ["Package", "Version", "Architecture"] {
        if !fields.contains_key(required) {
            return Err(audit_error(
                path,
                "MalformedMetadata",
                "missing-field",
                required,
            ));
        }
    }
    for dependency_field in ["Depends", "Pre-Depends", "Provides"] {
        if let Some(value) = fields.get(dependency_field) {
            if !valid_deb_dependency_field(value) {
                return Err(audit_error(
                    path,
                    "MalformedMetadata",
                    "dependency-grammar",
                    &format!("deb:control/{dependency_field}"),
                ));
            }
            if let Some(token) = forbidden_dependency(value) {
                return Err(audit_error(
                    path,
                    "ForbiddenDependency",
                    token,
                    &format!("deb:control/{dependency_field}"),
                ));
            }
        }
    }
    let (data_name, data_bytes) =
        data.ok_or_else(|| audit_error(path, "MalformedContainer", "missing-data", "deb"))?;
    compressed_tar(path, &data_name, data_bytes)
}

fn rpm_members(path: &Path) -> Result<Vec<Member>> {
    let package = rpm::Package::open(path)
        .map_err(|error| audit_error(path, "MalformedContainer", &error.to_string(), "rpm"))?;
    for dependency in package
        .metadata
        .get_requires()
        .map_err(|error| {
            audit_error(
                path,
                "MalformedMetadata",
                &error.to_string(),
                "rpm:Requires",
            )
        })?
        .into_iter()
        .chain(package.metadata.get_provides().map_err(|error| {
            audit_error(
                path,
                "MalformedMetadata",
                &error.to_string(),
                "rpm:Provides",
            )
        })?)
    {
        if let Some(token) = forbidden_dependency(&dependency.name) {
            return Err(audit_error(
                path,
                "ForbiddenDependency",
                token,
                "rpm:Requires/Provides",
            ));
        }
    }
    for (name, present) in [
        ("pre", package.metadata.get_pre_install_script().is_ok()),
        ("post", package.metadata.get_post_install_script().is_ok()),
        ("preun", package.metadata.get_pre_uninstall_script().is_ok()),
        (
            "postun",
            package.metadata.get_post_uninstall_script().is_ok(),
        ),
        ("pretrans", package.metadata.get_pre_trans_script().is_ok()),
        (
            "posttrans",
            package.metadata.get_post_trans_script().is_ok(),
        ),
        (
            "preuntrans",
            package.metadata.get_pre_untrans_script().is_ok(),
        ),
        (
            "postuntrans",
            package.metadata.get_post_untrans_script().is_ok(),
        ),
        ("verifyscript", package.metadata.get_verify_script().is_ok()),
    ] {
        if present {
            return Err(audit_error(path, "MaintainerScript", name, "rpm:script"));
        }
    }
    if !package
        .metadata
        .get_triggers()
        .map_err(|error| audit_error(path, "MalformedMetadata", &error.to_string(), "rpm:trigger"))?
        .is_empty()
        || !package
            .metadata
            .get_file_triggers()
            .map_err(|error| {
                audit_error(
                    path,
                    "MalformedMetadata",
                    &error.to_string(),
                    "rpm:file-trigger",
                )
            })?
            .is_empty()
        || !package
            .metadata
            .get_trans_file_triggers()
            .map_err(|error| {
                audit_error(
                    path,
                    "MalformedMetadata",
                    &error.to_string(),
                    "rpm:trans-trigger",
                )
            })?
            .is_empty()
    {
        return Err(audit_error(
            path,
            "MaintainerScript",
            "trigger",
            "rpm:trigger",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    for file in package
        .files()
        .map_err(|error| audit_error(path, "MalformedContainer", &error.to_string(), "rpm:cpio"))?
    {
        let file = file.map_err(|error| {
            audit_error(path, "MalformedContainer", &error.to_string(), "rpm:cpio")
        })?;
        if file.metadata.mode.file_type() != rpm::FileType::Regular {
            return Err(audit_error(
                path,
                "UnsupportedMember",
                "non-regular",
                "rpm:cpio",
            ));
        }
        let member = file
            .metadata
            .path
            .to_str()
            .ok_or_else(|| audit_error(path, "MalformedContainer", "non-utf8-path", "rpm"))?
            .to_owned();
        if !seen.insert(member.clone()) {
            return Err(audit_error(path, "PayloadClosure", "duplicate", &member));
        }
        members.push(Member {
            path: member,
            mode: u32::from(file.metadata.mode.permissions()),
            bytes: file.content,
        });
    }
    Ok(members)
}

fn expected_path(format: Format, authority: PayloadAuthority) -> String {
    match format {
        Format::Deb | Format::Rpm => authority.installed.trim_start_matches('/').to_owned(),
        Format::Tar => match authority.role {
            PayloadRole::Executable => "bin/solstone-linux".to_owned(),
            PayloadRole::License => "LICENSE".to_owned(),
            PayloadRole::InstallNotes => "INSTALL-NOTES".to_owned(),
            PayloadRole::Icon => authority
                .source
                .strip_prefix("contrib/icons/")
                .map(|value| format!("share/icons/{value}"))
                .unwrap_or_default(),
        },
    }
}

fn inspect_payload(
    path: &Path,
    format: Format,
    members: Vec<Member>,
) -> Result<(Vec<u8>, BTreeMap<String, String>)> {
    let mut by_path = members
        .into_iter()
        .map(|member| {
            (
                member
                    .path
                    .trim_start_matches("./")
                    .trim_start_matches('/')
                    .to_owned(),
                member,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut executable = None;
    let mut nonbinary = BTreeMap::new();
    for authority in PAYLOAD_AUTHORITY {
        let expected = expected_path(format, authority);
        let member = by_path
            .remove(&expected)
            .ok_or_else(|| audit_error(path, "PayloadClosure", "missing", &expected))?;
        if authority.role != PayloadRole::Executable && member.mode & 0o111 != 0 {
            return Err(audit_error(
                path,
                "ExtraExecutable",
                &format!("mode:{:04o}", member.mode & 0o7777),
                &expected,
            ));
        }
        if member.mode & 0o7777 != authority.mode {
            return Err(audit_error(
                path,
                "PayloadClosure",
                &format!("mode:{:04o}", member.mode),
                &expected,
            ));
        }
        match authority.role {
            PayloadRole::Executable => executable = Some(member.bytes),
            PayloadRole::InstallNotes => {
                let notes = std::str::from_utf8(&member.bytes)
                    .map_err(|_| audit_error(path, "StaleInstallNotes", "non-utf8", &expected))?;
                let normalized = notes.to_ascii_lowercase();
                for token in [
                    "--server-url",
                    "localhost:5015",
                    "127.0.0.1:5015",
                    "solstone_token",
                    "observer key",
                    "pip install",
                    "pipx",
                ] {
                    if normalized.contains(token) {
                        return Err(audit_error(path, "StaleInstallNotes", token, &expected));
                    }
                }
                if member.bytes != INSTALL_NOTES {
                    return Err(audit_error(path, "StaleInstallNotes", "digest", &expected));
                }
                nonbinary.insert(authority.source.to_owned(), digest(&member.bytes));
            }
            _ => {
                nonbinary.insert(authority.source.to_owned(), digest(&member.bytes));
            }
        }
    }
    if let Some((extra, member)) = by_path.into_iter().next() {
        let lower = extra.to_ascii_lowercase();
        let is_interpreter = member.bytes.starts_with(b"#!")
            || lower.ends_with(".py")
            || lower.contains("python")
            || parse_elf64(&member.bytes).is_ok();
        let class = if is_interpreter {
            "BundledSidecar"
        } else if member.mode & 0o111 != 0 {
            "ExtraExecutable"
        } else {
            "ExtraPayload"
        };
        let token = if member.bytes.starts_with(b"#!") {
            "shebang"
        } else if lower.contains("python") || lower.ends_with(".py") {
            "python"
        } else if parse_elf64(&member.bytes).is_ok() {
            "elf"
        } else if member.mode & 0o111 != 0 {
            "executable-bit"
        } else {
            "undeclared"
        };
        return Err(audit_error(path, class, token, &extra));
    }
    Ok((
        executable.ok_or_else(|| {
            audit_error(path, "PayloadClosure", "missing-executable", format.name())
        })?,
        nonbinary,
    ))
}

fn inspect_elf(path: &Path, bytes: &[u8]) -> Result<Elf64Linkage> {
    let parsed = parse_elf64(bytes).map_err(|error| {
        audit_error(
            path,
            "MalformedElf",
            &error.to_string(),
            "/usr/bin/solstone-linux",
        )
    })?;
    let expected_needed = EXPECTED_NEEDED.map(str::to_owned).to_vec();
    if parsed.elf_type != EXPECTED_ELF_TYPE
        || parsed.machine != EXPECTED_MACHINE
        || parsed.interpreter != EXPECTED_INTERPRETER
        || parsed.needed != expected_needed
        || parsed.soname.is_some()
        || parsed.rpath.is_some()
        || parsed.runpath.is_some()
    {
        return Err(audit_error(
            path,
            "UnexpectedImport",
            &format!("{parsed:?}"),
            "/usr/bin/solstone-linux",
        ));
    }
    Ok(parsed)
}

pub fn audit_packages(
    tar: &Path,
    deb: &Path,
    rpm: &Path,
    expected_executable_sha256: &str,
) -> Result<()> {
    if expected_executable_sha256.len() != 64
        || !expected_executable_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(audit_error(
            tar,
            "ProvenanceDigest",
            "expected-lowercase-sha256",
            "command-line",
        ));
    }
    for artifact in [tar, deb, rpm] {
        regular_artifact(artifact)?;
    }
    let tar_version = crate::tar_version(tar).map_err(|error| {
        audit_error(
            tar,
            "PackageIdentity",
            &error.to_string(),
            "filename-and-metadata",
        )
    })?;
    let deb_identity = crate::deb_identity(deb)
        .map_err(|error| audit_error(deb, "PackageIdentity", &error.to_string(), "control"))?;
    let rpm_identity = crate::rpm_identity(rpm)
        .map_err(|error| audit_error(rpm, "PackageIdentity", &error.to_string(), "header"))?;
    if deb_identity.name != "solstone-linux"
        || rpm_identity.name != deb_identity.name
        || deb_identity.version != tar_version
        || rpm_identity.version != tar_version
        || deb_identity.release.as_deref() != Some("1")
        || rpm_identity.release != deb_identity.release
        || deb_identity.arch != "amd64"
        || rpm_identity.arch != "x86_64"
    {
        return Err(audit_error(
            deb,
            "PackageIdentity",
            "name-version-release-architecture",
            "control/header/filename",
        ));
    }
    let inspected = [
        (tar, Format::Tar, tar_members(tar)?),
        (deb, Format::Deb, deb_members(deb)?),
        (rpm, Format::Rpm, rpm_members(rpm)?),
    ]
    .into_iter()
    .map(|(path, format, members)| {
        inspect_payload(path, format, members).map(|result| (path, result))
    })
    .collect::<Result<Vec<_>>>()?;
    let baseline = digest(&inspected[0].1.0);
    if baseline != expected_executable_sha256 {
        return Err(audit_error(
            inspected[0].0,
            "DivergentExecutable",
            &format!("sha256:{baseline}"),
            "executable",
        ));
    }
    for (path, (binary, nonbinary)) in &inspected {
        let binary_digest = digest(binary);
        if binary_digest != baseline {
            return Err(audit_error(
                path,
                "DivergentExecutable",
                &format!("sha256:{binary_digest}"),
                "executable",
            ));
        }
        if nonbinary != &inspected[0].1.1 {
            return Err(audit_error(
                path,
                "DivergentPayload",
                "nonbinary-digest",
                "payload",
            ));
        }
        inspect_elf(path, binary)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    use xz2::write::XzEncoder;

    fn artifact(format: Format) -> &'static Path {
        match format {
            Format::Tar => Path::new("fixture.tar.gz"),
            Format::Deb => Path::new("fixture.deb"),
            Format::Rpm => Path::new("fixture.rpm"),
        }
    }

    fn fixture_members(format: Format) -> Vec<Member> {
        PAYLOAD_AUTHORITY
            .into_iter()
            .map(|authority| {
                let bytes = match authority.role {
                    PayloadRole::Executable => crate::elf64::pinned_elf64_for_test(),
                    PayloadRole::InstallNotes => INSTALL_NOTES.to_vec(),
                    PayloadRole::License => b"license\n".to_vec(),
                    PayloadRole::Icon => authority.source.as_bytes().to_vec(),
                };
                Member {
                    path: expected_path(format, authority),
                    mode: authority.mode,
                    bytes,
                }
            })
            .collect()
    }

    fn exact(format: Format, class: &str, token: &str, member: &str, error: Error) {
        assert_eq!(
            error.to_string(),
            format!(
                "package audit: artifact={} class={class} token={token} member={member} tool=rust-release-manifest",
                artifact(format).file_name().unwrap().to_str().unwrap()
            )
        );
    }

    #[test]
    fn clean_payload_control_passes_in_all_formats() {
        for format in [Format::Tar, Format::Deb, Format::Rpm] {
            let (executable, nonbinary) =
                inspect_payload(artifact(format), format, fixture_members(format)).unwrap();
            inspect_elf(artifact(format), &executable).unwrap();
            assert_eq!(nonbinary.len(), PAYLOAD_AUTHORITY.len() - 1);
        }
    }

    #[test]
    fn extra_payload_is_rejected_in_all_formats() {
        for format in [Format::Tar, Format::Deb, Format::Rpm] {
            let mut members = fixture_members(format);
            members.push(Member {
                path: "usr/share/solstone-linux/extra".to_owned(),
                mode: 0o644,
                bytes: b"extra".to_vec(),
            });
            let error = inspect_payload(artifact(format), format, members).unwrap_err();
            exact(
                format,
                "ExtraPayload",
                "undeclared",
                "usr/share/solstone-linux/extra",
                error,
            );
        }
    }

    #[test]
    fn python_shebang_and_elf_sidecars_are_rejected_in_all_formats() {
        for (name, bytes, token) in [
            ("usr/libexec/helper.py", b"print('x')".to_vec(), "python"),
            (
                "usr/libexec/helper",
                b"#!/usr/bin/python3\n".to_vec(),
                "shebang",
            ),
            (
                "usr/libexec/helper-elf",
                crate::elf64::pinned_elf64_for_test(),
                "elf",
            ),
        ] {
            for format in [Format::Tar, Format::Deb, Format::Rpm] {
                let mut members = fixture_members(format);
                members.push(Member {
                    path: name.to_owned(),
                    mode: 0o644,
                    bytes: bytes.clone(),
                });
                let error = inspect_payload(artifact(format), format, members).unwrap_err();
                exact(format, "BundledSidecar", token, name, error);
            }
        }
    }

    #[test]
    fn extra_executable_and_wrong_mode_are_rejected_in_all_formats() {
        for format in [Format::Tar, Format::Deb, Format::Rpm] {
            let mut members = fixture_members(format);
            let icon = members
                .iter_mut()
                .find(|member| member.path.contains("16x16"))
                .unwrap();
            icon.mode = 0o755;
            let member = icon.path.clone();
            let error = inspect_payload(artifact(format), format, members).unwrap_err();
            exact(format, "ExtraExecutable", "mode:0755", &member, error);

            let mut members = fixture_members(format);
            let license = members
                .iter_mut()
                .find(|member| member.path.ends_with("LICENSE"))
                .unwrap();
            license.mode = 0o600;
            let member = license.path.clone();
            let error = inspect_payload(artifact(format), format, members).unwrap_err();
            exact(format, "PayloadClosure", "mode:0600", &member, error);
        }
    }

    #[test]
    fn missing_notes_and_executable_are_rejected_in_all_formats() {
        for (role, member) in [
            (PayloadRole::InstallNotes, "missing"),
            (PayloadRole::Executable, "missing"),
        ] {
            for format in [Format::Tar, Format::Deb, Format::Rpm] {
                let mut members = fixture_members(format);
                let authority = PAYLOAD_AUTHORITY
                    .iter()
                    .find(|authority| authority.role == role)
                    .unwrap();
                let expected = expected_path(format, *authority);
                members.retain(|candidate| candidate.path != expected);
                let error = inspect_payload(artifact(format), format, members).unwrap_err();
                exact(format, "PayloadClosure", member, &expected, error);
            }
        }
    }

    #[test]
    fn stale_notes_bytes_and_each_legacy_token_are_rejected_in_all_formats() {
        for mutation in [
            b"different notes".as_slice(),
            b"--server-url",
            b"localhost:5015",
            b"SOLSTONE_TOKEN",
            b"observer key",
            b"pip install",
            b"pipx",
        ] {
            for format in [Format::Tar, Format::Deb, Format::Rpm] {
                let mut members = fixture_members(format);
                let notes = members
                    .iter_mut()
                    .find(|member| member.path.ends_with("INSTALL-NOTES"))
                    .unwrap();
                notes.bytes = mutation.to_vec();
                let member = notes.path.clone();
                let error = inspect_payload(artifact(format), format, members).unwrap_err();
                let token = std::str::from_utf8(mutation).unwrap().to_ascii_lowercase();
                exact(
                    format,
                    "StaleInstallNotes",
                    if mutation == b"different notes" {
                        "digest"
                    } else {
                        &token
                    },
                    &member,
                    error,
                );
            }
        }
    }

    #[test]
    fn python_sol_and_journal_dependency_tokens_are_anchored() {
        for token in [
            "python", "python2", "python3", "pip", "pip3", "pipx", "sol", "journal",
        ] {
            assert_eq!(forbidden_dependency(token), Some(token));
        }
        for allowed in ["solstone-linux", "libsol", "journald", "libpython-free"] {
            assert_eq!(forbidden_dependency(allowed), None, "{allowed}");
        }
    }

    #[test]
    fn malformed_and_unexpected_elf_diagnostics_name_every_artifact() {
        for format in [Format::Tar, Format::Deb, Format::Rpm] {
            let error = inspect_elf(artifact(format), b"not ELF").unwrap_err();
            assert!(error.to_string().starts_with(&format!(
                "package audit: artifact={} class=MalformedElf token=",
                artifact(format).file_name().unwrap().to_str().unwrap()
            )));
            assert!(
                error
                    .to_string()
                    .ends_with(" member=/usr/bin/solstone-linux tool=rust-release-manifest")
            );

            let expected = EXPECTED_NEEDED;
            for elf in [
                crate::elf64::linkage_elf64_for_test(
                    "/unexpected/interpreter",
                    &expected,
                    None,
                    None,
                    None,
                ),
                crate::elf64::linkage_elf64_for_test(
                    EXPECTED_INTERPRETER,
                    &expected[..expected.len() - 1],
                    None,
                    None,
                    None,
                ),
                crate::elf64::linkage_elf64_for_test(
                    EXPECTED_INTERPRETER,
                    &[
                        "libgstreamer-1.0.so.0",
                        "libgobject-2.0.so.0",
                        "libglib-2.0.so.0",
                        "libgio-2.0.so.0",
                        "libpulse.so.0",
                        "libgcc_s.so.1",
                        "libm.so.6",
                        "libc.so.6",
                        "libpython3.so",
                    ],
                    None,
                    None,
                    None,
                ),
                crate::elf64::linkage_elf64_for_test(
                    EXPECTED_INTERPRETER,
                    &expected,
                    Some("solstone-linux"),
                    None,
                    None,
                ),
                crate::elf64::linkage_elf64_for_test(
                    EXPECTED_INTERPRETER,
                    &expected,
                    None,
                    Some("/tmp"),
                    None,
                ),
                crate::elf64::linkage_elf64_for_test(
                    EXPECTED_INTERPRETER,
                    &expected,
                    None,
                    None,
                    Some("/tmp"),
                ),
            ] {
                let error = inspect_elf(artifact(format), &elf).unwrap_err();
                assert!(error.to_string().starts_with(&format!(
                    "package audit: artifact={} class=UnexpectedImport token=",
                    artifact(format).file_name().unwrap().to_str().unwrap()
                )));
                assert!(
                    error
                        .to_string()
                        .ends_with(" member=/usr/bin/solstone-linux tool=rust-release-manifest")
                );
            }
        }
    }

    fn tar_fixture(entries: &[(&str, &[u8], EntryType, u32)]) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        for (path, body, kind, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(*kind);
            header.set_cksum();
            archive
                .append_data(&mut header, *path, *body)
                .expect("fixture tar member");
        }
        archive.into_inner().unwrap()
    }

    #[test]
    fn tar_inventory_rejects_duplicate_links_devices_and_truncation() {
        let artifact = Path::new("fixture.tar.gz");
        let duplicate = tar_fixture(&[
            ("member", b"a", EntryType::Regular, 0o644),
            ("member", b"b", EntryType::Regular, 0o644),
        ]);
        exact(
            Format::Tar,
            "PayloadClosure",
            "duplicate",
            "member",
            tar_inventory(artifact, Cursor::new(duplicate)).unwrap_err(),
        );
        for kind in [
            EntryType::Symlink,
            EntryType::Link,
            EntryType::Char,
            EntryType::Block,
            EntryType::Fifo,
        ] {
            let bytes = tar_fixture(&[("member", b"", kind, 0o644)]);
            exact(
                Format::Tar,
                "UnsupportedMember",
                "non-regular",
                "tar",
                tar_inventory(artifact, Cursor::new(bytes)).unwrap_err(),
            );
        }
        let mut traversal_builder = tar::Builder::new(Vec::new());
        let mut traversal_header = tar::Header::new_gnu();
        traversal_header.set_size(4);
        traversal_header.set_mode(0o644);
        traversal_header.as_mut_bytes()[..9].copy_from_slice(b"../escape");
        traversal_header.set_cksum();
        traversal_builder
            .append(&traversal_header, b"body".as_slice())
            .unwrap();
        let traversal = traversal_builder.into_inner().unwrap();
        exact(
            Format::Tar,
            "PayloadClosure",
            "path-traversal",
            "tar",
            tar_inventory(artifact, Cursor::new(traversal)).unwrap_err(),
        );
        let mut truncated = tar_fixture(&[("member", b"body", EntryType::Regular, 0o644)]);
        truncated.truncate(515);
        let error = tar_inventory(artifact, Cursor::new(truncated)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "package audit: artifact=fixture.tar.gz class=MalformedContainer token=unexpected EOF during skip member=tar tool=rust-release-manifest"
        );
    }

    #[test]
    fn deb_compression_readers_fail_closed_for_gzip_xz_zstd_and_unknown() {
        let artifact = Path::new("fixture.deb");
        let tar = tar_fixture(&[("control", b"body", EntryType::Regular, 0o644)]);

        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        let gzip = gzip.finish().unwrap();
        let mut xz = XzEncoder::new(Vec::new(), 6);
        xz.write_all(&tar).unwrap();
        let xz = xz.finish().unwrap();
        let zstd = zstd::stream::encode_all(Cursor::new(&tar), 3).unwrap();

        for (name, bytes, token) in [
            ("control.tar.gz", gzip, "unexpected end of file"),
            ("control.tar.xz", xz, "premature eof"),
            ("control.tar.zst", zstd, "incomplete frame"),
        ] {
            let truncated = bytes[..bytes.len() / 2].to_vec();
            let error = compressed_tar(artifact, name, truncated).unwrap_err();
            exact(Format::Deb, "MalformedContainer", token, "tar", error);
        }
        exact(
            Format::Deb,
            "UnsupportedCompression",
            "control.tar.bz2",
            "deb",
            compressed_tar(artifact, "control.tar.bz2", Vec::new()).unwrap_err(),
        );
    }
}
