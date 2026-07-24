// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::audit::*;
use super::*;
use base64::Engine;
use chrono::{Duration, SecondsFormat, Utc};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::process::{Command, Output, Stdio};

fn request(root: &Path, locator: &'static str) -> (AuditRequest<'static>, PathBuf) {
    let receipt = root.join("freshness.json");
    let signature = root.join("freshness.json.minisig");
    (
        AuditRequest {
            bundle: Box::leak(root.join("advisories.bundle").into_boxed_path()),
            receipt: Box::leak(receipt.into_boxed_path()),
            public_key: Box::leak(root.join("audit.pub").into_boxed_path()),
            locator,
        },
        signature,
    )
}

fn sensitive(locator: &'static str) -> (tempfile::TempDir, SensitiveValues) {
    let temp = tempfile::tempdir().unwrap();
    let (request, signature) = request(temp.path(), locator);
    let values = SensitiveValues::new(&request, &signature);
    (temp, values)
}

fn canonical_receipt(commit: &str, utc: &str) -> Vec<u8> {
    format!("{{\"max_age\":86400,\"synced_commit\":\"{commit}\",\"utc\":\"{utc}\"}}\n").into_bytes()
}

#[test]
fn audit_receipt_requires_canonical_bytes_commit_and_time_window() {
    let now = Utc::now();
    let utc = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let commit = "a".repeat(40);
    let (_temp, sensitive) = sensitive("file://mirror.invalid/advisory-db");
    let receipt = parse_receipt(&canonical_receipt(&commit, &utc), now, &sensitive).unwrap();
    assert_eq!(receipt.synced_commit, commit);
    assert_eq!(receipt.max_age, 86_400);

    for malformed in [
        format!(
            "{{\"synced_commit\":\"{}\",\"max_age\":86400,\"utc\":\"{}\"}}\n",
            "a".repeat(40),
            utc
        ),
        format!(
            "{{\"max_age\":86401,\"synced_commit\":\"{}\",\"utc\":\"{}\"}}\n",
            "a".repeat(40),
            utc
        ),
        format!(
            "{{\"max_age\":86400,\"synced_commit\":\"{}\",\"utc\":\"{}\",\"extra\":1}}\n",
            "a".repeat(40),
            utc
        ),
    ] {
        assert!(parse_receipt(malformed.as_bytes(), now, &sensitive).is_err());
    }
    for invalid_time in [
        (now + Duration::seconds(301)).to_rfc3339_opts(SecondsFormat::Secs, true),
        (now - Duration::seconds(86_401)).to_rfc3339_opts(SecondsFormat::Secs, true),
        now.to_rfc3339(),
    ] {
        assert!(
            parse_receipt(
                &canonical_receipt(&"a".repeat(40), &invalid_time),
                now,
                &sensitive
            )
            .is_err()
        );
    }
}

#[test]
fn audit_locator_accepts_only_private_terminal_names_and_rejects_github_spellings() {
    for accepted in [
        "file://mirror.invalid/advisory-db",
        "ssh://mirror.invalid/team/rustsec-advisory-db.git",
    ] {
        let (_temp, sensitive) = sensitive(accepted);
        validate_locator(accepted, &sensitive).unwrap();
    }
    for rejected in [
        "",
        "advisory-db",
        "https://github.com/rustsec/advisory-db",
        "HTTPS://GITHUB.COM/RUSTSEC/ADVISORY-DB",
        "ssh://git@github.com/RustSec/advisory-db.git",
        "ssh://git@github.com:22/RustSec/advisory-db",
        "git@github.com:RustSec/advisory-db.git",
        "github.com/rustsec/advisory-db",
        "file://mirror.invalid/advisory-db/",
        "file://mirror.invalid/advisory-db?ref=main",
        "file://mirror.invalid/advisory-db#main",
        "file://mirror.invalid/advisory db",
        "file://mir\"ror/advisory-db",
        "file://mirror.invalid/other",
    ] {
        let (_temp, sensitive) = sensitive(rejected);
        assert!(
            validate_locator(rejected, &sensitive).is_err(),
            "{rejected}"
        );
    }
}

#[test]
fn audit_public_key_requires_digest_packet_prefix_and_little_endian_key_id() {
    let mut packet = Vec::from(*b"Ed");
    packet.extend_from_slice(&0x5fcc81cd3de12315_u64.to_le_bytes());
    packet.extend_from_slice(&[7; 32]);
    let key = format!(
        "untrusted comment: audit fixture\n{}\n",
        base64::engine::general_purpose::STANDARD.encode(packet)
    );
    let (_temp, sensitive) = sensitive("file://mirror.invalid/advisory-db");
    validate_public_key(
        key.as_bytes(),
        &digest(key.as_bytes()),
        "5FCC81CD3DE12315",
        &sensitive,
    )
    .unwrap();
    assert!(
        validate_public_key(
            key.as_bytes(),
            &digest(key.as_bytes()),
            "0000000000000000",
            &sensitive
        )
        .is_err()
    );
    assert!(
        validate_public_key(
            key.as_bytes(),
            &"0".repeat(64),
            "5FCC81CD3DE12315",
            &sensitive
        )
        .is_err()
    );
}

#[test]
fn audit_success_object_has_exact_order_and_no_archive_digest() {
    let status = AuditStatus {
        product: "solstone-linux".into(),
        source_cohort: "sol-controlled-rustsec-mirror-v1".into(),
        synced_commit: "a".repeat(40),
        utc: "2026-07-23T00:00:00Z".into(),
        max_age: 86_400,
        checked_at: "2026-07-23T00:00:01Z".into(),
        cargo_lock_sha256: "b".repeat(64),
        cargo_deny_version: "cargo-deny 0.20.2".into(),
        verdict: "pass".into(),
    };
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(
        json,
        format!(
            "{{\"product\":\"solstone-linux\",\"source_cohort\":\"sol-controlled-rustsec-mirror-v1\",\"synced_commit\":\"{}\",\"utc\":\"2026-07-23T00:00:00Z\",\"max_age\":86400,\"checked_at\":\"2026-07-23T00:00:01Z\",\"cargo_lock_sha256\":\"{}\",\"cargo_deny_version\":\"cargo-deny 0.20.2\",\"verdict\":\"pass\"}}",
            "a".repeat(40),
            "b".repeat(64)
        )
    );
    assert!(!json.contains("archive"));
}

#[test]
fn audit_input_gate_rejects_missing_empty_and_symlink_packet_files() {
    let temp = tempfile::tempdir().unwrap();
    let (request, signature) = request(temp.path(), "file://mirror.invalid/advisory-db");
    let sensitive = SensitiveValues::new(&request, &signature);
    assert!(validate_inputs(&request, &signature, &sensitive).is_err());

    for path in [
        request.bundle,
        request.receipt,
        request.public_key,
        signature.as_path(),
    ] {
        fs::write(path, b"x").unwrap();
    }
    validate_inputs(&request, &signature, &sensitive).unwrap();
    fs::write(request.receipt, b"").unwrap();
    assert!(validate_inputs(&request, &signature, &sensitive).is_err());
    fs::write(request.receipt, b"x").unwrap();
    fs::remove_file(&signature).unwrap();
    let target = temp.path().join("signature-target");
    fs::write(&target, b"x").unwrap();
    symlink(&target, &signature).unwrap();
    assert!(validate_inputs(&request, &signature, &sensitive).is_err());
}

#[test]
fn audit_redaction_removes_operator_values_before_errors_are_rendered() {
    let temp = tempfile::tempdir().unwrap();
    let locator = "file://private.invalid/team/advisory-db";
    let (request, signature) = request(temp.path(), locator);
    let sensitive = SensitiveValues::new(&request, &signature);
    let raw = format!(
        "locator={locator} receipt={} signature={}",
        request.receipt.display(),
        signature.display()
    );
    let redacted = sensitive.redact(&raw);
    assert!(!redacted.contains(locator));
    assert!(!redacted.contains(request.receipt.to_string_lossy().as_ref()));
    assert!(!redacted.contains(signature.to_string_lossy().as_ref()));
    assert!(redacted.contains("[REDACTED]"));
}

fn run_with_input(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn initialize_advisory_repo(root: &Path, package: &str) -> String {
    fs::create_dir(root).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "audit@invalid.example"][..],
        &["config", "user.name", "Audit Fixture"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
    let advisory = root
        .join("crates")
        .join(package)
        .join("RUSTSEC-2099-0003.md");
    fs::create_dir_all(advisory.parent().unwrap()).unwrap();
    fs::write(
        advisory,
        format!(
            "```toml\n[advisory]\nid = \"RUSTSEC-2099-0003\"\npackage = \"{package}\"\ndate = \"2099-01-03\"\nurl = \"https://example.invalid/RUSTSEC-2099-0003\"\n\n[versions]\npatched = []\n```\n"
        ),
    )
    .unwrap();
    for args in [
        &["add", "--all"][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned()
}

fn create_bundle(repo: &Path, bundle: &Path, extra_head: bool) {
    if extra_head {
        assert!(
            Command::new("git")
                .args(["branch", "extra"])
                .current_dir(repo)
                .status()
                .unwrap()
                .success()
        );
    }
    let mut command = Command::new("git");
    command
        .args(["bundle", "create"])
        .arg(bundle)
        .args(["HEAD", "refs/heads/main"]);
    if extra_head {
        command.arg("refs/heads/extra");
    }
    assert!(command.current_dir(repo).status().unwrap().success());
}

fn sign_receipt(secret: &Path, receipt: &Path, commit: &str) -> String {
    let utc = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    fs::write(receipt, canonical_receipt(commit, &utc)).unwrap();
    let signature = receipt.with_file_name(format!(
        "{}.minisig",
        receipt.file_name().unwrap().to_string_lossy()
    ));
    let comment =
        format!("solpbc-advisory-mirror-v1 synced_commit={commit} utc={utc} max_age=86400");
    let signed = run_with_input(
        Command::new("minisign")
            .args(["-S", "-s"])
            .arg(secret)
            .arg("-m")
            .arg(receipt)
            .arg("-x")
            .arg(signature)
            .args(["-t", &comment]),
        b"audit-passphrase\n",
    );
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    utc
}

fn packet_inventory(request: &AuditRequest<'_>) -> BTreeMap<PathBuf, String> {
    let signature = request.receipt.with_file_name(format!(
        "{}.minisig",
        request.receipt.file_name().unwrap().to_string_lossy()
    ));
    [
        request.bundle,
        request.receipt,
        request.public_key,
        signature.as_path(),
    ]
    .into_iter()
    .map(|path| (path.to_owned(), digest(&fs::read(path).unwrap())))
    .collect()
}

fn audit_error(
    request: &AuditRequest<'_>,
    public_bytes: &[u8],
    key_id: &str,
    fixture: &crate::candidate_tests::TestRepo,
) -> String {
    run_audit_mode(
        request,
        &ProcessEnvironment::default(),
        Utc::now(),
        &digest(public_bytes),
        key_id,
        Some(&fixture.root),
    )
    .unwrap_err()
    .to_string()
}

#[test]
#[ignore = "dedicated real minisign and cargo-deny audit gate"]
fn real_signed_packet_local_audit() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = crate::candidate_tests::fixture();
    let repo = temp.path().join("green-db");
    let commit = initialize_advisory_repo(&repo, "definitely-absent-solstone-fixture");
    let bundle = temp.path().join("advisories.bundle");
    create_bundle(&repo, &bundle, false);

    let public = temp.path().join("audit.pub");
    let secret = temp.path().join("audit.key");
    let receipt = temp.path().join("freshness.json");
    let generated = run_with_input(
        Command::new("minisign")
            .args(["-G", "-p"])
            .arg(&public)
            .arg("-s")
            .arg(&secret),
        b"audit-passphrase\naudit-passphrase\n",
    );
    assert!(generated.status.success());
    sign_receipt(&secret, &receipt, &commit);
    let public_bytes = fs::read(&public).unwrap();
    let encoded = std::str::from_utf8(&public_bytes)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap();
    let packet = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let key_id = format!(
        "{:016X}",
        u64::from_le_bytes(packet[2..10].try_into().unwrap())
    );
    let request = AuditRequest {
        bundle: &bundle,
        receipt: &receipt,
        public_key: &public,
        locator: "file://mirror.invalid/advisory-db",
    };
    let packet_before = packet_inventory(&request);
    let cargo_lock_before = digest(&fs::read(fixture.root.path().join("Cargo.lock")).unwrap());
    let dist = fixture.root.path().join("dist");
    let dist_before = fs::read_dir(&dist)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let status = run_audit_mode(
        &request,
        &ProcessEnvironment::default(),
        Utc::now(),
        &digest(&public_bytes),
        &key_id,
        Some(&fixture.root),
    )
    .unwrap();
    assert_eq!(status.synced_commit, commit);
    assert_eq!(status.verdict, "pass");
    assert_eq!(packet_inventory(&request), packet_before);
    assert_eq!(
        digest(&fs::read(fixture.root.path().join("Cargo.lock")).unwrap()),
        cargo_lock_before
    );
    let dist_after = fs::read_dir(&dist)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let staging_parent = dist.join(".rust-release-candidate-staging");
    let staging_entries = fs::read_dir(&staging_parent)
        .map(|entries| entries.count())
        .unwrap_or_default();
    assert_eq!(staging_entries, 0);
    let mut expected_dist = dist_before;
    expected_dist.insert(OsString::from(".rust-release-candidate-staging"));
    assert_eq!(dist_after, expected_dist);
    assert!(!dist.join(".rust-release-candidate.lock").exists());
    assert!(
        !dist_after.iter().any(|name| {
            let name = name.to_string_lossy();
            name != ".rust-release-candidate-staging"
                && (name.contains("staging") || name.contains("advisory"))
        }),
        "{dist_after:?}"
    );

    let mut tampered = fs::read(&receipt).unwrap();
    tampered[0] ^= 1;
    fs::write(&receipt, tampered).unwrap();
    let error = audit_error(&request, &public_bytes, &key_id, &fixture);
    assert!(error.contains("audit signature gate mismatch"), "{error}");
    assert!(!error.contains("\"verdict\":\"pass\""), "{error}");

    let vulnerable_repo = temp.path().join("vulnerable-db");
    let vulnerable_commit = initialize_advisory_repo(&vulnerable_repo, "serde_json");
    let vulnerable_bundle = temp.path().join("vulnerable.bundle");
    create_bundle(&vulnerable_repo, &vulnerable_bundle, false);
    let vulnerable_receipt = temp.path().join("vulnerable.json");
    sign_receipt(&secret, &vulnerable_receipt, &vulnerable_commit);
    let vulnerable_request = AuditRequest {
        bundle: &vulnerable_bundle,
        receipt: &vulnerable_receipt,
        public_key: &public,
        locator: "file://mirror.invalid/advisory-db",
    };
    let error = audit_error(&vulnerable_request, &public_bytes, &key_id, &fixture);
    assert!(error.contains("audit cargo-deny gate mismatch"), "{error}");
    assert!(!error.contains("\"verdict\":\"pass\""), "{error}");

    let extra_repo = temp.path().join("extra-head-db");
    let extra_commit = initialize_advisory_repo(&extra_repo, "definitely-absent-solstone-fixture");
    let extra_bundle = temp.path().join("extra-head.bundle");
    create_bundle(&extra_repo, &extra_bundle, true);
    let extra_receipt = temp.path().join("extra-head.json");
    sign_receipt(&secret, &extra_receipt, &extra_commit);
    let extra_request = AuditRequest {
        bundle: &extra_bundle,
        receipt: &extra_receipt,
        public_key: &public,
        locator: "file://mirror.invalid/advisory-db",
    };
    let error = audit_error(&extra_request, &public_bytes, &key_id, &fixture);
    assert!(
        error.contains("audit bundle-heads gate mismatch"),
        "{error}"
    );
    assert!(!error.contains("\"verdict\":\"pass\""), "{error}");

    let mismatch_receipt = temp.path().join("commit-mismatch.json");
    sign_receipt(&secret, &mismatch_receipt, &"a".repeat(40));
    let mismatch_request = AuditRequest {
        bundle: &bundle,
        receipt: &mismatch_receipt,
        public_key: &public,
        locator: "file://mirror.invalid/advisory-db",
    };
    let error = audit_error(&mismatch_request, &public_bytes, &key_id, &fixture);
    assert!(
        error.contains("audit bundle-heads gate mismatch")
            || error.contains("audit checkout-identity gate mismatch"),
        "{error}"
    );
    assert!(!error.contains("\"verdict\":\"pass\""), "{error}");

    // Cleanup-failure suppression is shared with finish_candidate_staging_owned and
    // regression-covered by candidate/transaction tests. Network rejection is by
    // construction: audit invokes only local bundle verbs and locked/offline cargo-deny.
}
