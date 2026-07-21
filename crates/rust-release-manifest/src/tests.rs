// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use std::os::unix::fs::symlink;

fn tools() -> BTreeMap<String, String> {
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

fn evidence() -> Evidence {
    let root = workspace_root().unwrap();
    let version: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    Evidence {
        schema_version: 1,
        product: PRODUCT.into(),
        version: version["workspace"]["package"]["version"]
            .as_str()
            .unwrap()
            .into(),
        source_commit: command(&root, &["git", "rev-parse", "HEAD"]).unwrap(),
        source_dirty: false,
        cargo_lock_sha256: digest(&fs::read(root.join("Cargo.lock")).unwrap()),
        rust: RustEvidence {
            rustc_verbose: "rustc 1.97.1".into(),
            cargo_version: "cargo 1.97.1".into(),
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
        active_exceptions: EXCEPTIONS.iter().map(|value| (*value).into()).collect(),
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

fn deb(root: &Path, version: &str) -> PathBuf {
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
    path
}

fn rpm_file(root: &Path, version: &str) -> PathBuf {
    let path = root.join(format!("solstone-linux-{version}-1.x86_64.rpm"));
    let package = rpm::PackageBuilder::new(PRODUCT, version, "AGPL-3.0-only", "x86_64", "fixture")
        .build()
        .unwrap();
    package.write(&mut File::create(&path).unwrap()).unwrap();
    path
}

fn release_fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let version = evidence().version;
    tarball(temp.path(), &version);
    deb(temp.path(), &version);
    rpm_file(temp.path(), &version);
    temp
}

#[test]
fn rust_release_manifest_conformance() {
    verify_schema().unwrap();
    let vendor = workspace_root()
        .unwrap()
        .join("vendor/rust-release-manifest");
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
            workspace_root()
                .unwrap()
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

    for key in TOOL_KEYS {
        let mut value: Value = serde_json::from_str(&first).unwrap();
        value["native_tools"].as_object_mut().unwrap().remove(key);
        assert!(
            validate_manifest_bytes(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
        );
    }
    for canary in [
        "$VERSION",
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
fn package_readers_reject_malformed_and_stale_bytes() {
    let temp = release_fixture();
    let version = evidence().version;
    let stale = tarball(temp.path(), "9.9.9");
    assert!(verify_package_identity(&stale, &version).is_err());
    fs::write(&stale, b"not gzip").unwrap();
    assert!(tar_version(&stale).is_err());
    let bad_deb = temp.path().join("solstone-linux_9.9.9-1_amd64.deb");
    fs::write(&bad_deb, b"truncated").unwrap();
    assert!(deb_identity(&bad_deb).is_err());
    let bad_rpm = temp.path().join("solstone-linux-9.9.9-1.x86_64.rpm");
    fs::write(&bad_rpm, b"truncated").unwrap();
    assert!(rpm_identity(&bad_rpm).is_err());
}

#[test]
fn checksum_and_complete_inventory_mutations_fail() {
    let temp = release_fixture();
    let text = render_manifest(evidence(), temp.path()).unwrap();
    let manifest = validate_manifest_bytes(text.as_bytes()).unwrap();
    fs::write(temp.path().join(MANIFEST_NAME), text).unwrap();
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
    let reject = |candidate: &Manifest| {
        assert!(validate_live(candidate, outside.path()).is_err());
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
    ] {
        let mut value = original.clone();
        value["native_tools"][key] = Value::String(wrong.into());
        reject(&value);
    }
    for key in [
        "container_engine",
        "ubuntu_os",
        "ubuntu_rustc",
        "ubuntu_cargo",
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
        temp.path().join(MANIFEST_NAME),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    let sums = render_sha256sums(&manifest.artifacts).unwrap();
    fs::write(temp.path().join(CHECKSUM_NAME), &sums).unwrap();

    let artifact_path = temp.path().join(&manifest.artifacts[0].path);
    fs::write(&artifact_path, b"mutated after render").unwrap();
    assert!(verify_artifacts(&manifest, temp.path()).is_err());
    assert!(
        verify_checksums(&manifest, temp.path()).is_err()
            || verify_artifacts(&manifest, temp.path()).is_err()
    );

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
