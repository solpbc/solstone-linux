// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::about::*;
use crate::sync_health::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn host() -> HostFacts {
    HostFacts {
        os: "ubuntu".into(),
        os_version: "24.04".into(),
        arch: "x86_64".into(),
    }
}
fn facts(version: &str) -> About {
    decode_about(serde_json::to_string(&json!({"protocol_version":1,"version":version,"os":"macos","os_version":"26.5","arch":"aarch64","about":render_line("journal", version, None, "macos", "26.5", "aarch64")})).unwrap().as_bytes()).unwrap()
}
fn saved(version: &str, time: f64) -> PairedJournalVersion {
    PairedJournalVersion {
        identity_key: "PRIVATE_identity".into(),
        version: version.into(),
        name: Some("PRIVATE_name".into()),
        observed_at: time,
        about: Some(facts(version)),
        about_observed_at: Some(1.0),
    }
}

#[test]
fn imported_authority_is_exact_and_literals_drive_production_behavior() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/contracts");
    let import: Value =
        serde_json::from_slice(&std::fs::read(root.join("about-contract-import.json")).unwrap())
            .unwrap();
    assert_eq!(
        import["authority_commit"],
        "ec1983799b66d3616708851d01803e4f3d6f0a20"
    );
    let vendor = root.join("about-contract");
    let bytes = std::fs::read(vendor.join("manifest.json")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        import["manifest_sha256"]
    );
    let manifest: Value = serde_json::from_slice(&bytes).unwrap();
    let artifacts = manifest["artifacts"].as_object().unwrap();
    for (name, digest) in artifacts {
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(vendor.join(name)).unwrap())
            ),
            digest.as_str().unwrap()
        );
    }
    let expected: BTreeSet<_> = artifacts
        .keys()
        .cloned()
        .chain(std::iter::once("manifest.json".into()))
        .collect();
    let actual: BTreeSet<_> = std::fs::read_dir(&vendor)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(actual, expected);
    let contract: Value =
        serde_json::from_slice(&std::fs::read(vendor.join("contract.json")).unwrap()).unwrap();
    for fixture in contract["fixtures"].as_array().unwrap() {
        assert_eq!(
            render_line(
                "journal",
                fixture["version"].as_str().unwrap(),
                fixture["build"].as_str(),
                fixture["os"].as_str().unwrap(),
                fixture["os_version"].as_str().unwrap(),
                fixture["arch"].as_str().unwrap()
            ),
            fixture["about"]
        );
    }
    for (canonical, aliases) in contract["arch_aliases"].as_object().unwrap() {
        for alias in aliases.as_array().unwrap() {
            assert_eq!(normalize_arch(alias.as_str().unwrap()), canonical);
        }
    }
    let resources: Value =
        serde_json::from_slice(&std::fs::read(vendor.join("resources.json")).unwrap()).unwrap();
    for resource in resources["valid"].as_array().unwrap() {
        assert!(decode_about(&serde_json::to_vec(resource).unwrap()).is_some());
    }
    for resource in resources["invalid"].as_array().unwrap() {
        assert!(decode_about(&serde_json::to_vec(resource).unwrap()).is_none());
    }
}

#[test]
fn current_stale_legacy_crossed_and_unknown_versions_are_canonical() {
    let mut saved = saved("v1.2.3", 0.0);
    let current = AboutBlock::from_version(host(), Some(&saved), true, 172800);
    assert_eq!(
        current.text,
        format!(
            "linux desktop app {} · ubuntu 24.04 · x86_64\njournal 1.2.3 · macos 26.5 · arm64",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert_eq!(current.native().journal_seen_at_epoch_secs, Some(0));
    let stale = AboutBlock::from_version(host(), Some(&saved), false, 172800);
    assert_eq!(stale.journal_line, "journal 1.2.3 · macos 26.5 · arm64");
    assert!(stale.text.ends_with(" · last seen 2 days ago"));
    assert!(!stale.native().journal_current);
    assert!(!stale.native().journal_line.contains("last seen"));
    saved.version = "2.0.29".into();
    assert!(
        AboutBlock::from_version(host(), Some(&saved), true, 172800)
            .text
            .ends_with("\njournal 2.0.29")
    );
    saved.about = None;
    saved.observed_at = -1.0;
    assert!(
        AboutBlock::from_version(host(), Some(&saved), false, 172800)
            .text
            .ends_with("\njournal 2.0.29")
    );
    assert_eq!(
        AboutBlock::from_version(host(), None, true, 172800).journal_line,
        "journal unknown"
    );
    assert_eq!(
        parse_os_release("NAME=PRIVATE_OWNER\nID=ubuntu\nVERSION_ID=\"24.04\"\n"),
        ("ubuntu".into(), "24.04".into())
    );
    assert_eq!(normalize_arch("aarch64"), "arm64");
    assert_eq!(
        render_line("linux desktop app", "v2.0.11", None, "ubuntu", "24.04", ""),
        "linux desktop app 2.0.11 · ubuntu 24.04"
    );
}

#[test]
fn cache_preserves_version_on_bad_optional_facts_and_never_renews_fact_time() {
    let temp = tempfile::tempdir().unwrap();
    save_paired_journal_version(temp.path(), "PRIVATE_identity", "1.2.3", None).unwrap();
    save_paired_journal_about_guarded(
        temp.path(),
        "PRIVATE_identity",
        &facts("1.2.3"),
        &crate::private_file::NoWriteFault,
        &|| true,
    )
    .unwrap();
    let first = load_paired_journal_version(temp.path()).unwrap();
    assert!(first.about.is_some());
    save_paired_journal_version(temp.path(), "PRIVATE_identity", "v1.2.3", None).unwrap();
    let second = load_paired_journal_version(temp.path()).unwrap();
    assert_eq!(first.about, second.about);
    assert_eq!(first.about_observed_at, second.about_observed_at);
    assert!(second.observed_at >= first.observed_at);
    save_paired_journal_version(temp.path(), "PRIVATE_identity", "2.0.29", None).unwrap();
    let third = load_paired_journal_version(temp.path()).unwrap();
    assert!(third.about.is_none());
    assert!(third.about_observed_at.is_none());
    let bytes = std::fs::read(paired_journal_path(temp.path())).unwrap();
    assert!(
        save_paired_journal_about_guarded(
            temp.path(),
            "PRIVATE_identity",
            &facts("1.2.3"),
            &crate::private_file::NoWriteFault,
            &|| true
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(paired_journal_path(temp.path())).unwrap(),
        bytes
    );
    assert!(
        save_paired_journal_about_guarded(
            temp.path(),
            "another identity",
            &facts("2.0.29"),
            &crate::private_file::NoWriteFault,
            &|| true
        )
        .is_err()
    );
    assert!(
        save_paired_journal_about_guarded(
            temp.path(),
            "PRIVATE_identity",
            &facts("2.0.29"),
            &crate::private_file::NoWriteFault,
            &|| false
        )
        .is_err()
    );
    let mut bad: Value = serde_json::from_slice(&bytes).unwrap();
    bad["about"] = json!({"bad":true});
    bad["about_observed_at"] = json!("bad");
    bad.as_object_mut().unwrap().remove("observed_at");
    std::fs::write(
        paired_journal_path(temp.path()),
        serde_json::to_vec(&bad).unwrap(),
    )
    .unwrap();
    let legacy = load_paired_journal_version(temp.path()).unwrap();
    assert_eq!(legacy.version, "2.0.29");
    assert_eq!(legacy.observed_at, -1.0);
    assert!(legacy.about.is_none());
    let config = crate::config::Config {
        base_dir: temp.path().into(),
        config_dir: temp.path().join("unpaired"),
        ..Default::default()
    };
    assert_eq!(
        AboutBlock::snapshot(&config, &host(), 10).journal_line,
        "journal unknown"
    );
}

#[test]
fn native_and_report_project_only_public_fields_and_preserve_snapshot_bytes() {
    let mut resource = serde_json::to_value(facts("1.2.3")).unwrap();
    for field in [
        "name",
        "hostname",
        "owner_label",
        "account",
        "path",
        "instance_id",
        "address",
        "provider",
        "model",
    ] {
        resource[field] = json!(format!("PRIVATE_{field}"));
    }
    assert!(resource.to_string().contains("PRIVATE_hostname"));
    let mut stored = saved("1.2.3", 0.0);
    stored.about = decode_about(&serde_json::to_vec(&resource).unwrap());
    let block = AboutBlock::from_version(host(), Some(&stored), false, 172800);
    let native = serde_json::to_value(block.native()).unwrap();
    let keys: BTreeSet<_> = native
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "protocol_version",
            "os",
            "os_version",
            "arch",
            "journal_line",
            "journal_current",
            "journal_seen_at_epoch_secs"
        ])
    );
    let facts = crate::browser::custody::Facts {
        capture: "permitted",
        delivery: "idle",
        failure: None,
        generation: Some("fixture-a".into()),
        period_id: Some("fixture-period".into()),
        full: false,
        held_bytes: 0,
    };
    for kind in ["hello_ack", "state"] {
        let envelope =
            crate::browser::server::state_message_with_about(kind, &facts, &block.native());
        assert_eq!(envelope["about"], native);
        assert_eq!(envelope["type"], kind);
    }
    let unknown = AboutBlock::unknown(host()).native();
    let vendor = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/contracts/about-contract/native-about.json");
    let fixtures: Value = serde_json::from_slice(&std::fs::read(vendor).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(unknown).unwrap(), fixtures["valid"][1]);
    let url = crate::support::report_url("offline", &block);
    let parsed = reqwest::Url::parse(&url).unwrap();
    let fields = reqwest::Url::parse(&format!(
        "https://example.invalid/?{}",
        parsed.fragment().unwrap()
    ))
    .unwrap();
    assert_eq!(
        fields
            .query_pairs()
            .find(|(key, _)| key == "about")
            .unwrap()
            .1,
        block.text
    );
    assert!(parsed.query().is_none());
    for forbidden in [
        "PRIVATE_",
        "hostname",
        "instance_id",
        "provider",
        "build=",
        "%2Fhome%2F",
        "%2FUsers%2F",
    ] {
        assert!(!url.contains(forbidden));
        assert!(!native.to_string().contains(forbidden));
    }
    let producer = include_str!("support.rs")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    for forbidden in [
        "SOLSTONE_SOURCE_COMMIT",
        ".name",
        ".instance_id",
        ".hostname",
        ".address",
        ".provider",
        ".model",
    ] {
        assert!(!producer.contains(forbidden));
    }
}
