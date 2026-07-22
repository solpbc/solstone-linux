// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use std::os::unix::fs::symlink;

fn validate_manifest_bytes(bytes: &[u8]) -> Result<Manifest> {
    let repo = crate::candidate_tests::fixture();
    super::validate_manifest_bytes(&repo.root, bytes)
}

fn render_manifest(evidence: Evidence, release_dir: &Path) -> Result<String> {
    let repo = crate::candidate_tests::fixture();
    super::render_manifest(&repo.root, evidence, release_dir)
}

fn verify_manifest(path: &Path, bind_live: bool) -> Result<()> {
    let repo = crate::candidate_tests::fixture();
    super::verify_manifest(&repo.root, path, bind_live)
}

fn classify_release(path: &Path, bind_live: bool) -> Result<()> {
    let repo = crate::candidate_tests::fixture();
    super::classify_release(&repo.root, path, bind_live)
}

fn classify_release_dir(path: &Path) -> Result<()> {
    let repo = crate::candidate_tests::fixture();
    super::classify_release_dir(&repo.root, path)
}

pub(super) fn write_rendered(evidence: Evidence, release_dir: &Path) -> Result<()> {
    let text = render_manifest(evidence, release_dir)?;
    let manifest = validate_manifest_bytes(text.as_bytes())?;
    fs::write(release_dir.join(manifest_name(&manifest.version)), text).map_err(display_error)?;
    fs::write(
        release_dir.join(CHECKSUM_NAME),
        render_sha256sums(&manifest.artifacts)?,
    )
    .map_err(display_error)
}

pub(super) fn tools() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("container_engine".into(), "podman 5.4.0".into()),
        ("ubuntu_image_digest".into(), "a".repeat(64)),
        ("ubuntu_os".into(), "Ubuntu 22.04".into()),
        ("ubuntu_rustc".into(), "rustc 1.97.1".into()),
        ("ubuntu_cargo".into(), "cargo 1.97.1".into()),
        ("ubuntu_compiler".into(), "gcc 11.4.0".into()),
        ("ubuntu_linker".into(), "GNU ld 2.38".into()),
        ("ubuntu_glibc".into(), "glibc 2.35".into()),
        ("ubuntu_tar".into(), "GNU tar 1.34".into()),
        ("ubuntu_gzip".into(), "gzip 1.10".into()),
        ("cargo_deb".into(), "3.7.0".into()),
        ("dpkg_deb".into(), "dpkg-deb 1.21.1".into()),
        ("fedora_image_digest".into(), "b".repeat(64)),
        ("fedora_os".into(), "Fedora 42".into()),
        ("cargo_generate_rpm".into(), "0.21.0".into()),
        ("rpm".into(), "RPM 4.20.0".into()),
        (
            "manifest_validator".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
        ("signing_mode".into(), "unsigned".into()),
    ])
}

pub(super) fn evidence() -> Evidence {
    let repo = crate::candidate_tests::fixture();
    let root = repo.root.path();
    let version: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    Evidence {
        schema_version: 1,
        product: PRODUCT.into(),
        version: version["workspace"]["package"]["version"]
            .as_str()
            .unwrap()
            .into(),
        source_commit: command(root, &["git", "rev-parse", "HEAD"]).unwrap(),
        source_dirty: false,
        cargo_lock_sha256: digest(&fs::read(root.join("Cargo.lock")).unwrap()),
        rust: RustEvidence {
            rustc_verbose: "rustc 1.97.1 (abcdef012 2026-06-30)\nbinary: rustc\ncommit-hash: abcdef0123456789abcdef0123456789abcdef01\ncommit-date: 2026-06-30\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\nLLVM version: 18.1.0".into(),
            cargo_version: "cargo 1.97.1 (c980f4866 2026-06-30)".into(),
        },
        target: TargetEvidence::Compiled {
            triple: TARGET_TRIPLE.into(),
            profile: "release".into(),
            features: vec![],
        },
        native_tools: tools(),
        dependency_policy: DependencyPolicy {
            cargo_deny_version: CARGO_DENY_VERSION.into(),
            deterministic_gate: "pass".into(),
            advisory_checked_at: "2026-07-20T12:34:56Z".into(),
        },
        active_exceptions: crate::candidate_tests::TEST_EXCEPTIONS
            .iter()
            .map(|value| (*value).into())
            .collect(),
    }
}

#[test]
fn real_rustc_verbose_shape_is_privacy_safe() {
    let banner = "rustc 1.97.1 (abcdef012 2026-06-30)\nbinary: rustc\ncommit-hash: abcdef0123456789abcdef0123456789abcdef01\ncommit-date: 2026-06-30\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\nLLVM version: 18.1.0";
    validate_evidence_text("rust.rustc_verbose", banner).unwrap();
    assert!(validate_evidence_text("other evidence", &"a".repeat(40)).is_err());
    assert!(
        validate_evidence_text(
            "other evidence",
            &format!("commit-hash: {}", "a".repeat(40)),
        )
        .is_err()
    );
    for malformed in [
        banner.replace("host: x86_64-unknown-linux-gnu\n", ""),
        banner.replace("release: 1.97.1", "release: 1.97.1\nrelease: 1.97.1"),
        banner.replace(
            "commit-hash: abcdef0123456789abcdef0123456789abcdef01",
            "commit-hash: short",
        ),
    ] {
        assert!(validate_evidence_text("rust.rustc_verbose", &malformed).is_err());
    }
}

fn tarball(root: &Path, version: &str) -> PathBuf {
    let name = format!("solstone-linux-{version}-linux-x86_64.tar.gz");
    let path = root.join(name);
    let encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let bytes = b"fixture";
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            format!("solstone-linux-{version}-linux-x86_64/LICENSE"),
            &bytes[..],
        )
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    path
}

struct RawTarEntry<'a> {
    name: &'a [u8],
    body: &'a [u8],
    entry_type: u8,
    mode: u32,
    link_name: &'a [u8],
}

fn raw_entry<'a>(name: &'a [u8], body: &'a [u8]) -> RawTarEntry<'a> {
    RawTarEntry {
        name,
        body,
        entry_type: b'0',
        mode: 0o644,
        link_name: b"",
    }
}

fn tar_bytes(entries: &[RawTarEntry<'_>]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(entry.body.len() as u64);
        header.set_mode(entry.mode);
        header.set_entry_type(tar::EntryType::new(entry.entry_type));
        if entry.entry_type == b'S' {
            header.as_gnu_mut().unwrap().set_real_size(0);
        }
        assert!(entry.name.len() < 100);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..entry.name.len()].copy_from_slice(entry.name);
        header.set_link_name_literal(entry.link_name).unwrap();
        header.set_cksum();
        builder.append(&header, entry.body).unwrap();
    }
    builder.into_inner().unwrap()
}

fn gz_file(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(name);
    let mut encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap();
    path
}

fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let payload = format!("{key}={value}\n");
    let mut length = payload.len() + 2;
    loop {
        let record = format!("{length} {payload}");
        if record.len() == length {
            return record.into_bytes();
        }
        length = record.len();
    }
}

fn raw_tarball(root: &Path, name: &str, entries: &[RawTarEntry<'_>]) -> PathBuf {
    gz_file(root, name, &tar_bytes(entries))
}

pub(super) const FIXTURE_EXECUTABLE_BYTES: &[u8] = b"fixture executable";

fn producer_tar_bytes_with(version: &str, executable: &[u8]) -> Vec<u8> {
    let member_root = format!("solstone-linux-{version}-linux-x86_64");
    let names = [
        format!("{member_root}/"),
        format!("{member_root}/bin/"),
        format!("{member_root}/share/"),
        format!("{member_root}/share/icons/"),
        format!("{member_root}/bin/solstone-linux"),
        format!("{member_root}/LICENSE"),
        format!("{member_root}/INSTALL-NOTES"),
        format!("{member_root}/share/icons/solstone.png"),
    ];
    let entries = names
        .iter()
        .enumerate()
        .map(|(index, name)| RawTarEntry {
            name: name.as_bytes(),
            body: if index < 4 {
                b""
            } else if index == 4 {
                executable
            } else {
                b"fixture"
            },
            entry_type: if index < 4 { b'5' } else { b'0' },
            mode: if index == 4 { 0o755 } else { 0o644 },
            link_name: b"",
        })
        .collect::<Vec<_>>();
    tar_bytes(&entries)
}

fn producer_tar_bytes(version: &str) -> Vec<u8> {
    producer_tar_bytes_with(version, FIXTURE_EXECUTABLE_BYTES)
}

fn producer_tarball_with(root: &Path, version: &str, executable: &[u8]) -> PathBuf {
    gz_file(
        root,
        &format!("solstone-linux-{version}-linux-x86_64.tar.gz"),
        &producer_tar_bytes_with(version, executable),
    )
}

fn producer_tarball(root: &Path, version: &str) -> PathBuf {
    producer_tarball_with(root, version, FIXTURE_EXECUTABLE_BYTES)
}

fn control_tar(version: &str) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let body = format!("Package: solstone-linux\nVersion: {version}-1\nArchitecture: amd64\n");
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "./control", body.as_bytes())
        .unwrap();
    archive.into_inner().unwrap()
}

fn control_tar_bodies(bodies: &[String]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for body in bodies {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "./control", body.as_bytes())
            .unwrap();
    }
    archive.into_inner().unwrap()
}

fn data_tar(executable: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(executable.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "./usr/bin/solstone-linux", executable)
        .unwrap();
    archive.into_inner().unwrap()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn deb_members(root: &Path, name: &str, members: &[(&str, &[u8])]) -> PathBuf {
    let path = root.join(name);
    let mut archive = ar::Builder::new(File::create(&path).unwrap());
    for (member_name, bytes) in members {
        let header = ar::Header::new(member_name.as_bytes().to_vec(), bytes.len() as u64);
        archive.append(&header, *bytes).unwrap();
    }
    path
}

fn deb_with(root: &Path, version: &str, executable: &[u8]) -> PathBuf {
    let path = root.join(format!("solstone-linux_{version}-1_amd64.deb"));
    let mut archive = ar::Builder::new(File::create(&path).unwrap());
    let marker = b"2.0\n";
    let header = ar::Header::new(b"debian-binary".to_vec(), marker.len() as u64);
    archive.append(&header, &marker[..]).unwrap();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&control_tar(version)).unwrap();
    let compressed = encoder.finish().unwrap();
    let header = ar::Header::new(b"control.tar.gz".to_vec(), compressed.len() as u64);
    archive.append(&header, &compressed[..]).unwrap();
    let compressed = gzip(&data_tar(executable));
    let header = ar::Header::new(b"data.tar.gz".to_vec(), compressed.len() as u64);
    archive.append(&header, &compressed[..]).unwrap();
    path
}

fn rpm_file_with(root: &Path, version: &str, executable: &[u8]) -> PathBuf {
    let path = root.join(format!("solstone-linux-{version}-1.x86_64.rpm"));
    let package = rpm::PackageBuilder::new(PRODUCT, version, "AGPL-3.0-only", "x86_64", "fixture")
        .with_file_contents(
            executable,
            rpm::FileOptions::new("/usr/bin/solstone-linux").permissions(0o755),
        )
        .unwrap()
        .build()
        .unwrap();
    package.write(&mut File::create(&path).unwrap()).unwrap();
    path
}

#[test]
fn package_member_evidence_is_bound_to_all_three_formats() {
    let fixture = release_fixture();
    let mut members = artifact_paths(fixture.path(), "1.0.0")
        .unwrap()
        .into_iter()
        .map(|path| package_member_evidence(&path, "1.0.0").unwrap())
        .collect::<Vec<_>>();
    members.sort_by(|left, right| left.format.cmp(&right.format));
    for member in &members {
        assert_eq!(member.mode, 0o755);
        let expected_path = if member.format == "tar" {
            "/bin/solstone-linux"
        } else {
            "/usr/bin/solstone-linux"
        };
        assert_eq!(member.installed_path, expected_path);
    }
    assert_eq!(members.len(), 3);
    assert!(
        members
            .iter()
            .all(|member| member.sha256 == digest(FIXTURE_EXECUTABLE_BYTES))
    );
    assert!(
        members
            .iter()
            .all(|member| member.bytes == FIXTURE_EXECUTABLE_BYTES.len() as u64)
    );
}

pub(super) fn release_fixture() -> tempfile::TempDir {
    release_fixture_with([
        FIXTURE_EXECUTABLE_BYTES,
        FIXTURE_EXECUTABLE_BYTES,
        FIXTURE_EXECUTABLE_BYTES,
    ])
}

pub(super) fn release_fixture_with(executables: [&[u8]; 3]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let version = evidence().version;
    producer_tarball_with(temp.path(), &version, executables[0]);
    deb_with(temp.path(), &version, executables[1]);
    rpm_file_with(temp.path(), &version, executables[2]);
    temp
}

#[test]
fn rust_release_manifest_conformance() {
    verify_schema().unwrap();
    let repo = crate::candidate_tests::fixture();
    let vendor = repo.root.path().join("vendor/rust-release-manifest");
    let entries = fs::read_dir(&vendor)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entries,
        BTreeSet::from(["rust-release-manifest.schema.json".into()])
    );
    let descriptor: Value = serde_json::from_slice(
        &fs::read(
            repo.root
                .path()
                .join("contracts/rust-release-manifest-import.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(descriptor["schema_version"], 1);
    assert_eq!(descriptor["schema_sha256"], SCHEMA_SHA256);
    assert_eq!(
        descriptor["schema_id"],
        "https://solpbc.org/schemas/rust-release-manifest/v1.json"
    );
    assert_eq!(
        descriptor["schema_dialect"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(
        descriptor["schema_path"],
        "rust-release-manifest.schema.json"
    );
    assert_eq!(descriptor["vendored_root"], "vendor/rust-release-manifest");
    assert!(descriptor.get("authority_repository").is_none());
    assert!(descriptor.get("authority_commit").is_none());
    assert_eq!(descriptor.as_object().unwrap().len(), 6);
    let temp = release_fixture();
    let first = render_manifest(evidence(), temp.path()).unwrap();
    let second = render_manifest(evidence(), temp.path()).unwrap();
    assert_eq!(first, second);
    let other = tempfile::tempdir().unwrap();
    for entry in fs::read_dir(temp.path()).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), other.path().join(entry.file_name())).unwrap();
    }
    assert_eq!(first, render_manifest(evidence(), other.path()).unwrap());
    let manifest = validate_manifest_bytes(first.as_bytes()).unwrap();
    assert_eq!(manifest.artifacts.len(), 3);
    assert_eq!(
        manifest
            .artifacts
            .iter()
            .map(|item| item.path.clone())
            .collect::<Vec<_>>(),
        {
            let mut names = manifest
                .artifacts
                .iter()
                .map(|item| item.path.clone())
                .collect::<Vec<_>>();
            names.sort();
            names
        }
    );
    for item in &manifest.artifacts {
        verify_package_identity(&temp.path().join(&item.path), &evidence().version).unwrap();
    }
    let sums = render_sha256sums(&manifest.artifacts).unwrap();
    assert_eq!(sums.lines().count(), 3);
    assert!(sums.ends_with('\n'));

    let mut bad: Value = serde_json::from_str(&first).unwrap();
    bad["dependency_policy"]["advisory_checked_at"] =
        Value::String("2026-07-20T12:34:56+00:00".into());
    assert!(validate_manifest_bytes(serde_json::to_string(&bad).unwrap().as_bytes()).is_err());
    bad["dependency_policy"]["advisory_checked_at"] = Value::String("not-a-date".into());
    assert!(validate_manifest_bytes(serde_json::to_string(&bad).unwrap().as_bytes()).is_err());

    for key in TOOL_SPECS.iter().map(|spec| spec.key) {
        let mut value: Value = serde_json::from_str(&first).unwrap();
        value["native_tools"].as_object_mut().unwrap().remove(key);
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }
    for canary in [
        "$VERSION",
        "${VERSION}",
        "%VERSION%",
        "tool 1.0 token=abc",
        "tool 1.0 builder.internal",
        "tool 1.0 /usr/bin/tool",
        "tool 1.0 staging",
        "tool 1.0\nnext",
        "arbitrary prose with version 1",
    ] {
        let mut value: Value = serde_json::from_str(&first).unwrap();
        value["native_tools"]["container_engine"] = Value::String(canary.into());
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }
    let mut value: Value = serde_json::from_str(&first).unwrap();
    value["native_tools"]["ubuntu_image_digest"] =
        Value::String(format!("sha256:{}", "a".repeat(64)));
    assert!(validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err());
}

#[test]
fn named_manifest_requires_versioned_basename() {
    let temp = release_fixture();
    let version = evidence().version;
    write_rendered(evidence(), temp.path()).unwrap();
    let correct = temp.path().join(manifest_name(&version));
    verify_manifest(&correct, false).unwrap();

    for wrong_name in [
        "rust-release-manifest.json".to_owned(),
        manifest_name("9.9.9"),
    ] {
        let wrong = temp.path().join(wrong_name);
        fs::copy(&correct, &wrong).unwrap();
        let error = verify_manifest(&wrong, false).unwrap_err();
        assert!(error.to_string().contains("manifest basename mismatch"));
    }
}

#[test]
fn renderer_requires_authoritative_ordered_exceptions() {
    let temp = release_fixture();
    let mut cases = Vec::new();

    let mut dropped = evidence();
    dropped.active_exceptions.pop();
    cases.push(dropped);

    let mut added = evidence();
    added.active_exceptions.push("RUSTSEC-2026-9999".to_owned());
    cases.push(added);

    let mut reordered = evidence();
    reordered.active_exceptions.reverse();
    cases.push(reordered);

    for candidate in cases {
        assert!(render_manifest(candidate, temp.path()).is_err());
    }
}

#[test]
fn package_readers_reject_malformed_and_stale_bytes() {
    let temp = release_fixture();
    let version = evidence().version;
    let stale = gz_file(
        temp.path(),
        &format!("solstone-linux-{version}-linux-x86_64.tar.gz"),
        &producer_tar_bytes("9.9.9"),
    );
    assert!(verify_package_identity(&stale, &version).is_err());
    fs::write(&stale, b"not gzip").unwrap();
    assert!(tar_version(&stale).is_err());
    let bad_deb = temp.path().join("solstone-linux_9.9.9-1_amd64.deb");
    fs::write(&bad_deb, b"truncated").unwrap();
    assert!(deb_identity(&bad_deb).is_err());
    let bad_rpm = temp.path().join("solstone-linux-9.9.9-1.x86_64.rpm");
    fs::write(&bad_rpm, b"truncated").unwrap();
    assert!(rpm_identity(&bad_rpm).is_err());

    for (name, entries) in [
        ("traversal.tar.gz", vec![raw_entry(b"../evil", &b"x"[..])]),
        (
            "absolute.tar.gz",
            vec![raw_entry(b"/etc/passwd", &b"x"[..])],
        ),
        (
            "multiple-roots.tar.gz",
            vec![
                raw_entry(b"one/file", &b"x"[..]),
                raw_entry(b"two/file", &b"y"[..]),
            ],
        ),
    ] {
        assert!(tar_version(&raw_tarball(temp.path(), name, &entries)).is_err());
    }

    let body = format!("Package: solstone-linux\nVersion: {version}-1\nArchitecture: amd64\n");
    let control = gzip(&control_tar_bodies(std::slice::from_ref(&body)));
    let marker = b"2.0\n";
    assert!(
        deb_identity(&deb_members(
            temp.path(),
            "missing-marker.deb",
            &[("control.tar.gz", &control)],
        ))
        .is_err()
    );
    assert!(
        deb_identity(&deb_members(
            temp.path(),
            "wrong-marker.deb",
            &[("debian-binary", b"2.1\n"), ("control.tar.gz", &control)],
        ))
        .is_err()
    );
    assert!(
        deb_identity(&deb_members(
            temp.path(),
            "duplicate-control-archive.deb",
            &[
                ("debian-binary", marker),
                ("control.tar.gz", &control),
                ("control.tar.gz", &control),
            ],
        ))
        .is_err()
    );

    let duplicate_entries = gzip(&control_tar_bodies(&[body.clone(), body.clone()]));
    assert!(
        deb_identity(&deb_members(
            temp.path(),
            "duplicate-control-entry.deb",
            &[
                ("debian-binary", marker),
                ("control.tar.gz", &duplicate_entries),
            ],
        ))
        .is_err()
    );
    let duplicate_field = format!("{body}Version: {version}-1\n");
    let duplicate_field = gzip(&control_tar_bodies(&[duplicate_field]));
    assert!(
        deb_identity(&deb_members(
            temp.path(),
            "duplicate-control-field.deb",
            &[
                ("debian-binary", marker),
                ("control.tar.gz", &duplicate_field),
            ],
        ))
        .is_err()
    );
}

#[test]
fn tar_reader_accepts_safe_producer_and_file_only_shapes() {
    let temp = tempfile::tempdir().unwrap();
    let version = evidence().version;
    assert_eq!(
        tar_version(&producer_tarball(temp.path(), &version)).unwrap(),
        version
    );
    assert_eq!(
        tar_version(&tarball(temp.path(), &version)).unwrap(),
        version
    );

    let long_path = format!(
        "solstone-linux-{version}-linux-x86_64/share/{}",
        "long-name-".repeat(12)
    );
    let mut long_path_body = long_path.into_bytes();
    long_path_body.push(0);
    let placeholder = format!("solstone-linux-{version}-linux-x86_64/placeholder");
    let long_name = RawTarEntry {
        name: b"././@LongLink",
        body: &long_path_body,
        entry_type: b'L',
        mode: 0o644,
        link_name: b"",
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "long-name-positive.tar.gz",
            &[long_name, raw_entry(placeholder.as_bytes(), b"x")],
        ))
        .unwrap(),
        version
    );
}

#[test]
fn tar_reader_rejects_truncated_and_checksum_invalid_archives() {
    let temp = tempfile::tempdir().unwrap();
    let mut truncated = producer_tar_bytes("1.0.0");
    truncated.truncate(truncated.len() / 2 + 100);
    assert!(tar_version(&gz_file(temp.path(), "truncated.tar.gz", &truncated,)).is_err());

    let mut checksum_invalid = producer_tar_bytes("1.0.0");
    checksum_invalid[148] ^= 1;
    assert_eq!(
        tar_version(&gz_file(
            temp.path(),
            "checksum-invalid.tar.gz",
            &checksum_invalid,
        ))
        .unwrap_err()
        .to_string(),
        "tar member header mismatch: index 0"
    );

    let safe = [raw_entry(
        b"solstone-linux-1.0.0-linux-x86_64/LICENSE",
        b"x",
    )];
    let unsafe_after_end = [raw_entry(b"../CANARYMARKER", b"x")];
    let mut after_end = tar_bytes(&safe);
    after_end.extend_from_slice(&tar_bytes(&unsafe_after_end));
    assert!(tar_version(&gz_file(temp.path(), "member-after-end.tar.gz", &after_end,)).is_err());

    let mut bad_trailer = gzip(&tar_bytes(&safe));
    let trailer_byte = bad_trailer.len() - 1;
    bad_trailer[trailer_byte] ^= 1;
    let bad_trailer_path = temp.path().join("bad-trailer.tar.gz");
    fs::write(&bad_trailer_path, bad_trailer).unwrap();
    assert!(tar_version(&bad_trailer_path).is_err());

    let mut trailing_garbage = gzip(&tar_bytes(&safe));
    trailing_garbage.extend_from_slice(b"CANARYMARKER");
    let trailing_garbage_path = temp.path().join("trailing-garbage.tar.gz");
    fs::write(&trailing_garbage_path, trailing_garbage).unwrap();
    assert!(tar_version(&trailing_garbage_path).is_err());
}

#[test]
fn tar_reader_rejects_unsupported_member_metadata_and_types() {
    let temp = tempfile::tempdir().unwrap();
    let root = b"solstone-linux-1.0.0-linux-x86_64/file";
    for (case, entry_type) in b"gS8123467".iter().copied().enumerate() {
        let entry = RawTarEntry {
            entry_type,
            ..raw_entry(root, b"")
        };
        let error = tar_version(&raw_tarball(
            temp.path(),
            &format!("unsupported-{case}.tar.gz"),
            &[entry],
        ))
        .unwrap_err();
        assert_eq!(error.to_string(), "tar member type mismatch: index 0");
    }

    for (case, mode) in [("setuid", 0o4644), ("setgid", 0o2644)] {
        let entry = RawTarEntry {
            mode,
            ..raw_entry(root, b"x")
        };
        assert_eq!(
            tar_version(&raw_tarball(
                temp.path(),
                &format!("{case}.tar.gz"),
                &[entry],
            ))
            .unwrap_err()
            .to_string(),
            "tar member mode mismatch: index 0"
        );
    }

    let pax = RawTarEntry {
        name: b"pax",
        body: b"13 comment=x\n",
        entry_type: b'x',
        mode: 0o644,
        link_name: b"",
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "pax.tar.gz",
            &[pax, raw_entry(root, b"x")],
        ))
        .unwrap_err()
        .to_string(),
        "tar member metadata mismatch: index 0"
    );

    let pax_path = RawTarEntry {
        name: b"pax",
        body: b"16 path=../evil\n",
        entry_type: b'x',
        mode: 0o644,
        link_name: b"",
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "pax-path.tar.gz",
            &[pax_path, raw_entry(root, b"x")],
        ))
        .unwrap_err()
        .to_string(),
        "tar member metadata mismatch: index 0"
    );

    let long_link = RawTarEntry {
        name: b"././@LongLink",
        body: b"target\0",
        entry_type: b'K',
        mode: 0o644,
        link_name: b"",
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "long-link.tar.gz",
            &[long_link, raw_entry(root, b"x")],
        ))
        .unwrap_err()
        .to_string(),
        "tar member link mismatch: index 0"
    );

    let raw_link = RawTarEntry {
        link_name: b"target",
        ..raw_entry(root, b"x")
    };
    assert_eq!(
        tar_version(&raw_tarball(temp.path(), "raw-link.tar.gz", &[raw_link],))
            .unwrap_err()
            .to_string(),
        "tar member link mismatch: index 0"
    );

    let directory_payload = RawTarEntry {
        entry_type: b'5',
        ..raw_entry(b"solstone-linux-1.0.0-linux-x86_64/", b"x")
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "directory-payload.tar.gz",
            &[directory_payload],
        ))
        .unwrap_err()
        .to_string(),
        "tar member payload mismatch: index 0"
    );
}

#[test]
fn tar_reader_rejects_nonportable_member_paths() {
    let temp = tempfile::tempdir().unwrap();
    let bad_paths: &[&[u8]] = &[
        b"",
        b"/absolute",
        b"\\absolute",
        b"root/bad\\name",
        b"root/control\n",
        b"root/./file",
        b"root/../file",
        b"root//file",
        b"root/name.",
        b"root/name ",
        b"root/name?",
        b"root/CON.txt",
        b"root/\xff",
    ];
    for (index, name) in bad_paths.iter().enumerate() {
        let error = tar_version(&raw_tarball(
            temp.path(),
            &format!("bad-path-{index}.tar.gz"),
            &[raw_entry(name, b"x")],
        ))
        .unwrap_err();
        assert_eq!(error.to_string(), "tar member path mismatch: index 0");
    }

    for (name, entry_type) in [
        (b"solstone-linux-1.0.0-linux-x86_64/file/".as_slice(), b'0'),
        (b"solstone-linux-1.0.0-linux-x86_64//".as_slice(), b'5'),
    ] {
        let entry = RawTarEntry {
            entry_type,
            ..raw_entry(name, b"")
        };
        assert_eq!(
            tar_version(&raw_tarball(temp.path(), "bad-slash.tar.gz", &[entry]))
                .unwrap_err()
                .to_string(),
            "tar member path mismatch: index 0"
        );
    }

    let long_name = RawTarEntry {
        name: b"././@LongLink",
        body: b"../CANARYMARKER/evil\0",
        entry_type: b'L',
        mode: 0o644,
        link_name: b"",
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "long-name-traversal.tar.gz",
            &[long_name, raw_entry(b"placeholder", b"x")],
        ))
        .unwrap_err()
        .to_string(),
        "tar member path mismatch: index 0"
    );
}

#[test]
fn tar_reader_diagnostics_are_bounded_ascii_and_private() {
    let temp = tempfile::tempdir().unwrap();
    let root = b"solstone-linux-1.0.0-linux-x86_64";
    let file = b"solstone-linux-1.0.0-linux-x86_64/file";
    let mut errors = Vec::new();

    let invalid_gzip = temp.path().join("diagnostic-invalid-gzip.tar.gz");
    fs::write(&invalid_gzip, b"CANARYMARKER").unwrap();
    errors.push(tar_version(&invalid_gzip).unwrap_err().to_string());

    for (name, entry) in [
        (
            "diagnostic-type.tar.gz",
            RawTarEntry {
                entry_type: b'8',
                ..raw_entry(b"CANARYMARKER", b"")
            },
        ),
        (
            "diagnostic-mode.tar.gz",
            RawTarEntry {
                mode: 0o4644,
                ..raw_entry(b"CANARYMARKER", b"x")
            },
        ),
        (
            "diagnostic-link.tar.gz",
            RawTarEntry {
                link_name: b"CANARYMARKER",
                ..raw_entry(file, b"x")
            },
        ),
        (
            "diagnostic-payload.tar.gz",
            RawTarEntry {
                entry_type: b'5',
                ..raw_entry(b"CANARYMARKER/", b"x")
            },
        ),
        (
            "diagnostic-path.tar.gz",
            raw_entry(b"root/CANARYMARKER?", b"x"),
        ),
        (
            "diagnostic-utf8.tar.gz",
            raw_entry(b"root/CANARYMARKER\xff", b"x"),
        ),
    ] {
        errors.push(
            tar_version(&raw_tarball(temp.path(), name, &[entry]))
                .unwrap_err()
                .to_string(),
        );
    }

    let pax_body = pax_record("comment", "CANARYMARKER");
    let pax = RawTarEntry {
        name: b"pax",
        body: &pax_body,
        entry_type: b'x',
        mode: 0o644,
        link_name: b"",
    };
    errors.push(
        tar_version(&raw_tarball(
            temp.path(),
            "diagnostic-pax.tar.gz",
            &[pax, raw_entry(file, b"x")],
        ))
        .unwrap_err()
        .to_string(),
    );

    let long_name = RawTarEntry {
        name: b"././@LongLink",
        body: b"../CANARYMARKER/evil\0",
        entry_type: b'L',
        mode: 0o644,
        link_name: b"",
    };
    errors.push(
        tar_version(&raw_tarball(
            temp.path(),
            "diagnostic-long-name.tar.gz",
            &[long_name, raw_entry(b"placeholder", b"x")],
        ))
        .unwrap_err()
        .to_string(),
    );

    for (name, entries) in [
        (
            "diagnostic-duplicate.tar.gz",
            vec![raw_entry(file, b"x"), raw_entry(file, b"y")],
        ),
        (
            "diagnostic-collision.tar.gz",
            vec![
                raw_entry(file, b"x"),
                raw_entry(b"solstone-linux-1.0.0-linux-x86_64/FILE", b"y"),
            ],
        ),
        ("diagnostic-topology.tar.gz", vec![raw_entry(root, b"x")]),
        (
            "diagnostic-roots.tar.gz",
            vec![raw_entry(b"one/file", b"x"), raw_entry(b"two/file", b"y")],
        ),
        (
            "diagnostic-embedded.tar.gz",
            vec![raw_entry(b"not-canonical/file", b"x")],
        ),
    ] {
        errors.push(
            tar_version(&raw_tarball(temp.path(), name, &entries))
                .unwrap_err()
                .to_string(),
        );
    }

    for error in errors {
        assert!(!error.contains("CANARYMARKER"));
        assert!(
            error
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic()),
            "non-printable diagnostic"
        );
    }
}

#[test]
fn tar_reader_rejects_ambiguous_member_topology() {
    let temp = tempfile::tempdir().unwrap();
    let root = b"solstone-linux-1.0.0-linux-x86_64";
    let file = b"solstone-linux-1.0.0-linux-x86_64/file";
    let child = b"solstone-linux-1.0.0-linux-x86_64/file/child";

    let root_file = raw_entry(root, b"x");
    assert_eq!(
        tar_version(&raw_tarball(temp.path(), "root-file.tar.gz", &[root_file]))
            .unwrap_err()
            .to_string(),
        "tar member topology mismatch: index 0"
    );

    for (name, entries, expected) in [
        (
            "duplicate.tar.gz",
            vec![raw_entry(file, b"x"), raw_entry(file, b"y")],
            "tar member duplicate mismatch: index 1",
        ),
        (
            "collision.tar.gz",
            vec![
                raw_entry(file, b"x"),
                raw_entry(b"solstone-linux-1.0.0-linux-x86_64/FILE", b"y"),
            ],
            "tar member collision mismatch: index 1",
        ),
        (
            "ancestor.tar.gz",
            vec![raw_entry(child, b"y"), raw_entry(file, b"x")],
            "tar member topology mismatch: index 0",
        ),
    ] {
        assert_eq!(
            tar_version(&raw_tarball(temp.path(), name, &entries))
                .unwrap_err()
                .to_string(),
            expected
        );
    }

    let directory = RawTarEntry {
        entry_type: b'5',
        ..raw_entry(b"solstone-linux-1.0.0-linux-x86_64/file/", b"")
    };
    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "file-directory.tar.gz",
            &[directory, raw_entry(file, b"x")],
        ))
        .unwrap_err()
        .to_string(),
        "tar member topology mismatch: index 1"
    );

    assert_eq!(
        tar_version(&raw_tarball(
            temp.path(),
            "noncanonical-root.tar.gz",
            &[raw_entry(b"not-canonical/file", b"x")],
        ))
        .unwrap_err()
        .to_string(),
        "tar embedded version mismatch"
    );
}

#[test]
fn checksum_and_complete_inventory_mutations_fail() {
    let temp = release_fixture();
    let text = render_manifest(evidence(), temp.path()).unwrap();
    let manifest = validate_manifest_bytes(text.as_bytes()).unwrap();
    let manifest_path = temp.path().join(manifest_name(&manifest.version));
    fs::write(&manifest_path, text).unwrap();
    fs::write(
        temp.path().join(CHECKSUM_NAME),
        render_sha256sums(&manifest.artifacts).unwrap(),
    )
    .unwrap();
    for item in &manifest.artifacts {
        verify_package_identity(&temp.path().join(&item.path), &manifest.version).unwrap();
    }
    verify_checksums(&manifest, temp.path()).unwrap();
    classify_release(temp.path(), false).unwrap();
    let stale_manifest = temp.path().join(manifest_name("2.0.0"));
    fs::rename(&manifest_path, &stale_manifest).unwrap();
    assert!(classify_release(temp.path(), false).is_err());
    fs::rename(stale_manifest, &manifest_path).unwrap();
    let original = fs::read_to_string(temp.path().join(CHECKSUM_NAME)).unwrap();
    let mut lines = original.lines().collect::<Vec<_>>();
    lines.swap(0, 1);
    fs::write(
        temp.path().join(CHECKSUM_NAME),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
    assert!(verify_checksums(&manifest, temp.path()).is_err());
    fs::write(temp.path().join("extra"), b"extra").unwrap();
    assert!(classify_release_dir(temp.path()).is_err());
}

#[test]
fn schema_rejects_required_forbidden_and_path_mutations() {
    let temp = release_fixture();
    let text = render_manifest(evidence(), temp.path()).unwrap();
    let original: Value = serde_json::from_str(&text).unwrap();
    for key in [
        "schema_version",
        "product",
        "version",
        "source_commit",
        "source_dirty",
        "cargo_lock_sha256",
        "rust",
        "target",
        "native_tools",
        "dependency_policy",
        "active_exceptions",
        "artifacts",
    ] {
        let mut value = original.clone();
        value.as_object_mut().unwrap().remove(key);
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }
    for path in ["/absolute", "C:drive", "../escape", "bad\\name"] {
        let mut value = original.clone();
        value["artifacts"][0]["path"] = Value::String(path.into());
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }
    let mut value = original;
    value["unexpected"] = Value::Bool(true);
    assert!(validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err());
}

fn rendered_manifest() -> (tempfile::TempDir, Manifest) {
    let temp = release_fixture();
    let text = render_manifest(evidence(), temp.path()).unwrap();
    let manifest = validate_manifest_bytes(text.as_bytes()).unwrap();
    (temp, manifest)
}

#[test]
fn live_semantic_drift_is_rejected_field_by_field() {
    let (outside, manifest) = rendered_manifest();
    let repo = crate::candidate_tests::fixture();
    let reject = |candidate: &Manifest| {
        assert!(validate_live(&repo.root, candidate, outside.path()).is_err());
    };

    let mut candidate = manifest.clone();
    candidate.product = "other-observer".into();
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.version = "2.0.0".into();
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.source_commit = "0".repeat(40);
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.cargo_lock_sha256 = "0".repeat(64);
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.dependency_policy.cargo_deny_version = "0.20.3".into();
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.dependency_policy.deterministic_gate = "fail".into();
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.source_dirty = true;
    reject(&candidate);

    let mut candidate = manifest.clone();
    candidate.active_exceptions.pop();
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.active_exceptions.push("RUSTSEC-2026-9999".into());
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.active_exceptions[0] = "RUSTSEC-2026-9998".into();
    reject(&candidate);
    let mut candidate = manifest.clone();
    candidate.active_exceptions.reverse();
    reject(&candidate);

    for target in [
        TargetEvidence::Compiled {
            triple: "aarch64-unknown-linux-gnu".into(),
            profile: "release".into(),
            features: vec![],
        },
        TargetEvidence::Compiled {
            triple: TARGET_TRIPLE.into(),
            profile: "debug".into(),
            features: vec![],
        },
        TargetEvidence::Compiled {
            triple: TARGET_TRIPLE.into(),
            profile: "release".into(),
            features: vec!["foo".into()],
        },
        TargetEvidence::Source,
    ] {
        let mut candidate = manifest.clone();
        candidate.target = target;
        reject(&candidate);
    }
}

#[test]
fn native_tool_exact_identity_and_digest_mutations_fail() {
    let (temp, _) = rendered_manifest();
    let original: Value =
        serde_json::from_str(&render_manifest(evidence(), temp.path()).unwrap()).unwrap();
    let reject = |mutated: &Value| {
        assert!(
            validate_manifest_bytes(serde_json::to_string(mutated).unwrap().as_bytes()).is_err()
        );
    };

    for key in ["unknown_tool", "cargo_deny"] {
        let mut value = original.clone();
        value["native_tools"][key] = Value::String("tool 1.0".into());
        reject(&value);
    }
    for (key, wrong) in [
        ("cargo_deb", "3.7.1"),
        ("cargo_generate_rpm", "0.21.1"),
        ("manifest_validator", "9.9.9"),
        ("signing_mode", "signed"),
        ("ubuntu_rustc", "rustc 1.97.2"),
        ("ubuntu_cargo", "cargo 1.97.2"),
    ] {
        let mut value = original.clone();
        value["native_tools"][key] = Value::String(wrong.into());
        reject(&value);
    }
    for key in [
        "container_engine",
        "ubuntu_os",
        "ubuntu_compiler",
        "ubuntu_linker",
        "ubuntu_glibc",
        "ubuntu_tar",
        "ubuntu_gzip",
        "fedora_os",
        "dpkg_deb",
        "rpm",
    ] {
        let mut value = original.clone();
        value["native_tools"][key] = Value::String("wrong-tool 1.0".into());
        reject(&value);
    }
    for key in ["ubuntu_image_digest", "fedora_image_digest"] {
        for malformed in [
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            format!("{}g", "a".repeat(63)),
        ] {
            let mut value = original.clone();
            value["native_tools"][key] = Value::String(malformed);
            reject(&value);
        }
    }
}

#[test]
fn artifact_file_path_and_checksum_mutations_fail() {
    let (temp, manifest) = rendered_manifest();
    fs::write(
        temp.path().join(manifest_name(&manifest.version)),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    let sums = render_sha256sums(&manifest.artifacts).unwrap();
    fs::write(temp.path().join(CHECKSUM_NAME), &sums).unwrap();

    let artifact_path = temp.path().join(&manifest.artifacts[0].path);
    let artifact_bytes = fs::read(&artifact_path).unwrap();
    fs::write(&artifact_path, b"mutated after render").unwrap();
    assert!(verify_artifacts(&manifest, temp.path()).is_err());
    assert!(verify_checksums(&manifest, temp.path()).is_err());
    fs::write(&artifact_path, artifact_bytes).unwrap();

    let (missing, missing_manifest) = rendered_manifest();
    fs::remove_file(missing.path().join(&missing_manifest.artifacts[0].path)).unwrap();
    assert!(verify_artifacts(&missing_manifest, missing.path()).is_err());
    assert!(classify_release(missing.path(), false).is_err());

    let (linked, linked_manifest) = rendered_manifest();
    let linked_path = linked.path().join(&linked_manifest.artifacts[0].path);
    let target = linked.path().join("link-target");
    fs::rename(&linked_path, &target).unwrap();
    symlink(&target, &linked_path).unwrap();
    assert!(verify_artifacts(&linked_manifest, linked.path()).is_err());

    let variants = {
        let lines = sums.lines().collect::<Vec<_>>();
        let uppercase = format!("{}\n", sums.to_ascii_uppercase().trim_end());
        let short = format!(
            "{}\n",
            lines
                .iter()
                .map(|line| &line[1..])
                .collect::<Vec<_>>()
                .join("\n")
        );
        let duplicate = format!("{}\n{}\n", sums.trim_end(), lines[0]);
        let missing = format!("{}\n", lines[..2].join("\n"));
        let extra = format!("{}{}  extra.rpm\n", sums, "c".repeat(64));
        let crlf = sums.replace('\n', "\r\n");
        let no_final_lf = sums.trim_end_matches('\n').to_owned();
        [
            uppercase,
            short,
            duplicate,
            missing,
            extra,
            crlf,
            no_final_lf,
        ]
    };
    for variant in variants {
        fs::write(temp.path().join(CHECKSUM_NAME), variant).unwrap();
        assert!(verify_checksums(&manifest, temp.path()).is_err());
    }

    let mut directory_path = manifest.artifacts.clone();
    directory_path[0].path = "sub/file.tar.gz".into();
    assert!(validate_artifact_set(&directory_path).is_err());
    assert!(validate_artifact_set(&manifest.artifacts[..2]).is_err());
}

#[test]
fn schema_target_commit_and_datetime_boundaries_are_enforced() {
    let (temp, _) = rendered_manifest();
    let original: Value =
        serde_json::from_str(&render_manifest(evidence(), temp.path()).unwrap()).unwrap();

    let mut source = original.clone();
    source["target"] = serde_json::json!({"kind":"source"});
    assert!(validate_manifest_bytes(serde_json::to_string(&source).unwrap().as_bytes()).is_ok());

    for target in [
        serde_json::json!({"kind":"compiled","triple":TARGET_TRIPLE,"profile":"release"}),
        serde_json::json!({"kind":"source","triple":TARGET_TRIPLE}),
    ] {
        let mut value = original.clone();
        value["target"] = target;
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }

    let mut commit64 = original.clone();
    commit64["source_commit"] = Value::String("a".repeat(64));
    assert!(validate_manifest_bytes(serde_json::to_string(&commit64).unwrap().as_bytes()).is_ok());
    for commit in ["a".repeat(39), "a".repeat(41), "A".repeat(40)] {
        let mut value = original.clone();
        value["source_commit"] = Value::String(commit);
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }

    for timestamp in ["2026-07-20T12:34:56z", "2026-07-20T12:34:56+01:00"] {
        let mut value = original.clone();
        value["dependency_policy"]["advisory_checked_at"] = Value::String(timestamp.into());
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }
}

#[test]
fn manifest_mode_success_message_disclaims_candidate_readiness() {
    assert!(MANIFEST_OK_MESSAGE.contains("NOT candidate-readiness classification"));
}

#[test]
fn strict_semver_and_canonical_artifact_names_are_enforced() {
    for version in [
        "0.0.0",
        "1.2.3",
        "1.2.3-alpha.1",
        "1.2.3+build.5",
        "1.2.3-alpha+build",
    ] {
        validate_version(version).unwrap();
        artifact_kind(
            &format!("solstone-linux-{version}-linux-x86_64.tar.gz"),
            Some(version),
        )
        .unwrap();
    }
    for version in [
        "1", "1.2", "01.2.3", "1.02.3", "1.2.03", "1.2.3-01", "1.2.3-", "1.2.3+", " 1.2.3",
        "-1.2.3",
    ] {
        assert!(validate_version(version).is_err(), "accepted {version}");
    }
    assert!(
        artifact_kind(
            "prefix-solstone-linux-1.0.0-linux-x86_64.tar.gz",
            Some("1.0.0")
        )
        .is_err()
    );
    assert!(artifact_kind("solstone-linux-2.0.0-linux-x86_64.tar.gz", Some("1.0.0")).is_err());
}

#[test]
fn privacy_canaries_reject_network_account_and_opaque_tokens() {
    let (temp, _) = rendered_manifest();
    let original: Value =
        serde_json::from_str(&render_manifest(evidence(), temp.path()).unwrap()).unwrap();
    for canary in [
        "podman 5.4.0 10.0.0.1",
        "podman 5.4.0 2001:db8::1",
        "podman 5.4.0 123e4567-e89b-12d3-a456-426614174000",
        "podman 5.4.0 YWJjZGVmZ2hpamtsbW5vcHFyc3R1",
        "podman 5.4.0 ${ENGINE_VERSION}",
        "podman 5.4.0 %ENGINE_VERSION%",
    ] {
        let mut value = original.clone();
        value["native_tools"]["container_engine"] = Value::String(canary.into());
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err(),
            "accepted {canary}"
        );
    }
}

#[test]
fn rust_evidence_privacy_canaries_are_rejected() {
    let temp = release_fixture();
    let canaries = [
        "builder.internal",
        "/home/build/rustc",
        "10.0.0.5",
        "${RUSTC}",
        "builder@example.invalid",
        "YWJjZGVmZ2hpamtsbW5vcHFyc3R1",
    ];
    for field in ["rustc_verbose", "cargo_version"] {
        for canary in canaries {
            let mut candidate = evidence();
            if field == "rustc_verbose" {
                candidate.rust.rustc_verbose = canary.to_owned();
            } else {
                candidate.rust.cargo_version = canary.to_owned();
            }
            assert!(
                render_manifest(candidate, temp.path()).is_err(),
                "accepted {field} canary"
            );
        }
    }

    let rendered = render_manifest(evidence(), temp.path()).unwrap();
    let mut manifest: Value = serde_json::from_str(&rendered).unwrap();
    manifest["rust"]["rustc_verbose"] = Value::String("2001:db8::1".into());
    assert!(validate_manifest_bytes(serde_json::to_string(&manifest).unwrap().as_bytes()).is_err());
}

#[test]
fn clean_tree_policy_distinguishes_source_from_ignored_outputs() {
    let repo = tempfile::tempdir().unwrap();
    command(repo.path(), &["git", "init"]).unwrap();
    command(
        repo.path(),
        &["git", "config", "user.email", "fixture@example.com"],
    )
    .unwrap();
    command(repo.path(), &["git", "config", "user.name", "Fixture User"]).unwrap();
    fs::write(repo.path().join(".gitignore"), "dist/\n").unwrap();
    fs::write(repo.path().join("source.txt"), "committed\n").unwrap();
    command(repo.path(), &["git", "add", ".gitignore", "source.txt"]).unwrap();
    command(repo.path(), &["git", "commit", "-m", "fixture"]).unwrap();

    let payload = repo.path().join("dist/rust");
    fs::create_dir_all(&payload).unwrap();
    fs::write(payload.join("artifact"), "ignored\n").unwrap();
    require_clean_tree(repo.path(), &payload).unwrap();

    fs::write(repo.path().join("source.txt"), "modified\n").unwrap();
    assert!(require_clean_tree(repo.path(), &payload).is_err());
    fs::write(repo.path().join("source.txt"), "committed\n").unwrap();

    let untracked = repo.path().join("untracked.txt");
    fs::write(&untracked, "untracked\n").unwrap();
    assert!(require_clean_tree(repo.path(), &payload).is_err());
    fs::remove_file(untracked).unwrap();

    let outside = tempfile::tempdir().unwrap();
    assert!(require_clean_tree(repo.path(), outside.path()).is_err());

    fs::write(repo.path().join(".gitignore"), "other/\n").unwrap();
    command(repo.path(), &["git", "add", ".gitignore"]).unwrap();
    command(repo.path(), &["git", "commit", "-m", "change ignores"]).unwrap();
    assert!(require_clean_tree(repo.path(), &payload).is_err());
}

#[test]
fn regular_file_mode_helpers_distinguish_executables() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("file");
    fs::write(&path, b"contents").unwrap();

    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    require_regular_executable(&path, "fixture executable").unwrap();
    assert!(require_regular(&path, "fixture data").is_err());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    require_regular(&path, "fixture data").unwrap();
    assert!(require_regular_executable(&path, "fixture executable").is_err());
}
