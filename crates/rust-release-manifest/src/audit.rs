// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use base64::Engine;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use std::ffi::OsString;
use std::process::Output;

const APPROVED_KEY_ID: &str = "5FCC81CD3DE12315";
const APPROVED_PUBKEY_SHA256: &str =
    "c9fb713fe57791afbdebddde7b334e950ce1efcc167d49daf4cc1cbd930bb122";
const SOURCE_COHORT: &str = "sol-controlled-rustsec-mirror-v1";
const MAX_AGE: u64 = 86_400;
const FUTURE_SKEW_SECONDS: i64 = 5 * 60;

#[derive(Clone, Debug)]
pub struct AuditRequest<'a> {
    pub bundle: &'a Path,
    pub receipt: &'a Path,
    pub public_key: &'a Path,
    pub locator: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuditStatus {
    pub product: String,
    pub source_cohort: String,
    pub synced_commit: String,
    pub utc: String,
    pub max_age: u64,
    pub checked_at: String,
    pub cargo_lock_sha256: String,
    pub cargo_deny_version: String,
    pub verdict: String,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) max_age: u64,
    pub(crate) synced_commit: String,
    pub(crate) utc: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SensitiveValues {
    values: Vec<String>,
}

impl SensitiveValues {
    pub(crate) fn new(request: &AuditRequest<'_>, signature: &Path) -> Self {
        let mut values = vec![
            request.locator.to_owned(),
            request.bundle.to_string_lossy().into_owned(),
            request.receipt.to_string_lossy().into_owned(),
            request.public_key.to_string_lossy().into_owned(),
            signature.to_string_lossy().into_owned(),
        ];
        values.retain(|value| !value.is_empty());
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        Self { values }
    }

    fn add_path(&mut self, path: &Path) {
        let value = path.to_string_lossy().into_owned();
        if !value.is_empty() {
            self.values.push(value);
            self.values
                .sort_by_key(|value| std::cmp::Reverse(value.len()));
            self.values.dedup();
        }
    }

    pub(crate) fn redact(&self, value: &str) -> String {
        self.values
            .iter()
            .fold(value.to_owned(), |text, sensitive| {
                text.replace(sensitive, "[REDACTED]")
            })
    }

    fn error(&self, gate: &str, expected: &str, actual: &str, repair: &str) -> Error {
        Error::new(self.redact(&format!(
            "{gate} mismatch: expected {expected}, actual {actual}\nrepair: {repair}"
        )))
    }
}

pub fn run_audit(request: &AuditRequest<'_>) -> Result<AuditStatus> {
    run_audit_mode(
        request,
        &ProcessEnvironment::default(),
        Utc::now(),
        APPROVED_PUBKEY_SHA256,
        APPROVED_KEY_ID,
        None,
    )
}

fn signature_path(receipt: &Path) -> Result<PathBuf> {
    let name = receipt.file_name().ok_or_else(|| {
        Error::new(
            "audit input gate mismatch: expected receipt filename, actual unsafe\nrepair: provide a regular receipt file",
        )
    })?;
    let mut signature_name = OsString::from(name);
    signature_name.push(".minisig");
    Ok(receipt.with_file_name(signature_name))
}

fn require_nonempty_regular(path: &Path, label: &str, sensitive: &SensitiveValues) -> Result<()> {
    require_regular(path, label).map_err(|_| {
        sensitive.error(
            "audit input gate",
            "present nonempty no-follow regular files",
            "missing or unsafe",
            "provide the required local packet files",
        )
    })?;
    if fs::metadata(path).map_err(display_error)?.len() == 0 {
        return Err(sensitive.error(
            "audit input gate",
            "present nonempty no-follow regular files",
            "empty",
            "provide the required local packet files",
        ));
    }
    Ok(())
}

pub(crate) fn validate_inputs(
    request: &AuditRequest<'_>,
    signature: &Path,
    sensitive: &SensitiveValues,
) -> Result<()> {
    if request.locator.is_empty() {
        return Err(sensitive.error(
            "audit input gate",
            "nonempty locator",
            "empty",
            "provide all four audit inputs",
        ));
    }
    for (path, label) in [
        (request.bundle, "audit bundle"),
        (request.receipt, "audit receipt"),
        (request.public_key, "audit public key"),
        (signature, "audit receipt signature"),
    ] {
        require_nonempty_regular(path, label, sensitive)?;
    }
    Ok(())
}

pub(crate) fn validate_public_key(
    bytes: &[u8],
    expected_digest: &str,
    expected_key_id: &str,
    sensitive: &SensitiveValues,
) -> Result<()> {
    if digest(bytes) != expected_digest {
        return Err(sensitive.error(
            "audit public-key digest gate",
            "approved public key",
            "different key",
            "restore the approved audit public key",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        sensitive.error(
            "audit key-id gate",
            "canonical minisign public key",
            "invalid bytes",
            "restore the approved audit public key",
        )
    })?;
    let mut lines = text.lines();
    let comment = lines.next().unwrap_or_default();
    let encoded = lines.next().unwrap_or_default();
    if !text.ends_with('\n')
        || text.contains('\r')
        || !comment.starts_with("untrusted comment: ")
        || encoded.is_empty()
        || lines.next().is_some()
    {
        return Err(sensitive.error(
            "audit key-id gate",
            "two-line minisign public key",
            "malformed",
            "restore the approved audit public key",
        ));
    }
    let packet = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            sensitive.error(
                "audit key-id gate",
                "canonical minisign public key packet",
                "malformed",
                "restore the approved audit public key",
            )
        })?;
    if packet.len() != 42 || &packet[..2] != b"Ed" {
        return Err(sensitive.error(
            "audit key-id gate",
            "42-byte Ed public key packet",
            "different packet",
            "restore the approved audit public key",
        ));
    }
    let key_bytes: [u8; 8] = packet[2..10].try_into().expect("fixed key ID slice");
    if format!("{:016X}", u64::from_le_bytes(key_bytes)) != expected_key_id {
        return Err(sensitive.error(
            "audit key-id gate",
            "approved key ID",
            "different key ID",
            "restore the approved audit public key",
        ));
    }
    Ok(())
}

pub(crate) fn parse_receipt(
    bytes: &[u8],
    now: DateTime<Utc>,
    sensitive: &SensitiveValues,
) -> Result<Receipt> {
    let receipt: Receipt = serde_json::from_slice(bytes).map_err(|_| {
        sensitive.error(
            "audit receipt-body gate",
            "canonical signed receipt",
            "malformed",
            "replace the receipt with a canonical mirror receipt",
        )
    })?;
    if receipt.max_age != MAX_AGE
        || receipt.synced_commit.len() != 40
        || require_commit(&receipt.synced_commit, "audit synced commit").is_err()
    {
        return Err(sensitive.error(
            "audit receipt-body gate",
            "pinned max age and 40-character commit",
            "different values",
            "replace the receipt with a canonical mirror receipt",
        ));
    }
    let canonical = format!(
        "{{\"max_age\":{},\"synced_commit\":{},\"utc\":{}}}\n",
        receipt.max_age,
        serde_json::to_string(&receipt.synced_commit).expect("string serialization"),
        serde_json::to_string(&receipt.utc).expect("string serialization")
    );
    if canonical.as_bytes() != bytes {
        return Err(sensitive.error(
            "audit receipt-body gate",
            "byte-exact canonical receipt",
            "noncanonical",
            "replace the receipt with a canonical mirror receipt",
        ));
    }
    if validate_timestamp("audit receipt utc", &receipt.utc).is_err() {
        return Err(sensitive.error(
            "audit freshness gate",
            "canonical RFC3339 UTC seconds",
            "noncanonical time",
            "obtain a fresh signed mirror receipt",
        ));
    }
    let observed = DateTime::parse_from_rfc3339(&receipt.utc)
        .map_err(|_| {
            sensitive.error(
                "audit freshness gate",
                "canonical RFC3339 UTC seconds",
                "invalid time",
                "obtain a fresh signed mirror receipt",
            )
        })?
        .with_timezone(&Utc);
    if observed > now + Duration::seconds(FUTURE_SKEW_SECONDS)
        || now.signed_duration_since(observed) > Duration::seconds(MAX_AGE as i64)
    {
        return Err(sensitive.error(
            "audit freshness gate",
            "receipt within the authenticated time window",
            "stale or future",
            "obtain a fresh signed mirror receipt",
        ));
    }
    Ok(receipt)
}

pub(crate) fn validate_locator(locator: &str, sensitive: &SensitiveValues) -> Result<()> {
    let invalid_shape = locator.is_empty()
        || locator.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '\\' | '"')
        })
        || locator.ends_with('/')
        || locator.contains(['?', '#'])
        || locator.starts_with('-');
    let lower = locator.to_ascii_lowercase();
    let normalized = lower
        .replace("://", "/")
        .replace(':', "/")
        .trim_start_matches("git@")
        .trim_start_matches('/')
        .to_owned();
    let normalized_public_github =
        normalized
            .split('/')
            .collect::<Vec<_>>()
            .windows(3)
            .any(|parts| {
                parts[0].split('@').next_back() == Some("github.com")
                    && parts[1] == "rustsec"
                    && matches!(parts[2], "advisory-db" | "advisory-db.git")
            });
    let public_github = lower.match_indices("github.com").any(|(index, _)| {
        let boundary = index == 0
            || lower.as_bytes()[index - 1] == b'/'
            || lower.as_bytes()[index - 1] == b'@'
            || lower.as_bytes()[index - 1] == b':';
        if !boundary {
            return false;
        }
        let mut tail = &lower[index + "github.com".len()..];
        if let Some(port) = tail.strip_prefix(':') {
            let digits = port.bytes().take_while(u8::is_ascii_digit).count();
            tail = if digits > 0 { &port[digits..] } else { port };
        }
        let tail = tail.trim_start_matches(['/', ':']);
        matches!(tail, "rustsec/advisory-db" | "rustsec/advisory-db.git")
    }) || normalized_public_github;
    let parts = locator.split('/').collect::<Vec<_>>();
    let terminal = parts.last().copied().unwrap_or_default();
    let accepted_terminal = matches!(terminal, "advisory-db" | "rustsec-advisory-db.git");
    let path_part = locator
        .split_once("://")
        .map_or(locator, |(_, remainder)| remainder);
    let malformed_path = path_part.contains("//")
        || path_part
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."));
    if invalid_shape || public_github || parts.len() < 2 || malformed_path || !accepted_terminal {
        return Err(sensitive.error(
            "audit locator gate",
            "approved private advisory database identity",
            "rejected locator",
            "provide the approved non-public mirror locator",
        ));
    }
    Ok(())
}

fn audit_db_directory(locator: &str) -> String {
    let lower = locator.to_ascii_lowercase();
    let terminal = lower
        .split('/')
        .next_back()
        .expect("validated locator has terminal segment");
    format!("{terminal}-{:016x}", xxh64(0xca80_de71, lower.as_bytes()))
}

fn process_output(
    processes: &ProcessEnvironment,
    root: &Path,
    program: &str,
    args: &[&str],
    env: &[(&str, &OsStr)],
    gate: &str,
    sensitive: &SensitiveValues,
) -> Result<Output> {
    let mut command = processes.command(program);
    command.args(args).current_dir(root);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().map_err(|_| {
        sensitive.error(
            gate,
            "local command available",
            "unavailable",
            "install the pinned local audit tools",
        )
    })?;
    if !output.status.success() {
        return Err(sensitive.error(
            gate,
            "local command success",
            "command failure",
            "repair the local signed audit inputs and retry",
        ));
    }
    Ok(output)
}

fn output_text(output: Output, gate: &str, sensitive: &SensitiveValues) -> Result<String> {
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| {
            sensitive.error(
                gate,
                "UTF-8 command output",
                "invalid bytes",
                "repair the local signed audit inputs and retry",
            )
        })
}

fn run_materialized_audit(
    request: &AuditRequest<'_>,
    receipt: &Receipt,
    staging: &StagingLayout,
    context: &ImmutableContext,
    processes: &ProcessEnvironment,
    cargo_deny_version: &str,
    sensitive: &mut SensitiveValues,
) -> Result<AuditStatus> {
    sensitive.add_path(&staging.root);
    let checkout = staging
        .advisory_db
        .join(audit_db_directory(request.locator));
    sensitive.add_path(&checkout);
    let bundle = request.bundle.to_str().ok_or_else(|| {
        sensitive.error(
            "audit bundle-integrity gate",
            "UTF-8 local path",
            "invalid path",
            "provide a safe local bundle path",
        )
    })?;
    process_output(
        processes,
        &context.path,
        "git",
        &["bundle", "verify", bundle],
        &[],
        "audit bundle-integrity gate",
        sensitive,
    )?;
    let heads = output_text(
        process_output(
            processes,
            &context.path,
            "git",
            &["bundle", "list-heads", bundle],
            &[],
            "audit bundle-heads gate",
            sensitive,
        )?,
        "audit bundle-heads gate",
        sensitive,
    )?;
    let expected = BTreeSet::from([
        format!("{} HEAD", receipt.synced_commit),
        format!("{} refs/heads/main", receipt.synced_commit),
    ]);
    if heads.lines().map(str::to_owned).collect::<BTreeSet<_>>() != expected
        || heads.lines().count() != 2
    {
        return Err(sensitive.error(
            "audit bundle-heads gate",
            "exact HEAD and refs/heads/main",
            "different advertised heads",
            "obtain the bundle named by the signed receipt",
        ));
    }
    let destination = checkout.to_str().ok_or_else(|| {
        sensitive.error(
            "audit checkout-identity gate",
            "UTF-8 staging path",
            "invalid path",
            "retry from a safe local release workspace",
        )
    })?;
    process_output(
        processes,
        &context.path,
        "git",
        &["clone", bundle, destination],
        &[],
        "audit checkout-identity gate",
        sensitive,
    )?;
    let head = output_text(
        process_output(
            processes,
            &checkout,
            "git",
            &["rev-parse", "HEAD"],
            &[],
            "audit checkout-identity gate",
            sensitive,
        )?,
        "audit checkout-identity gate",
        sensitive,
    )?;
    if head != receipt.synced_commit {
        return Err(sensitive.error(
            "audit checkout-identity gate",
            "signed commit",
            "different commit",
            "obtain the bundle named by the signed receipt",
        ));
    }
    let status = output_text(
        process_output(
            processes,
            &checkout,
            "git",
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored=matching",
            ],
            &[],
            "audit checkout-cleanliness gate",
            sensitive,
        )?,
        "audit checkout-cleanliness gate",
        sensitive,
    )?;
    if !status.is_empty() {
        return Err(sensitive.error(
            "audit checkout-cleanliness gate",
            "clean checkout",
            "dirty checkout",
            "obtain an immutable clean advisory bundle",
        ));
    }
    let archive = process_output(
        processes,
        &checkout,
        "git",
        &["archive", "--format=tar", "HEAD"],
        &[],
        "audit archive gate",
        sensitive,
    )?
    .stdout;
    // Re-verify HEAD's tree fully materializes at the signed commit. The archive
    // digest is an internal integrity value, intentionally not surfaced: the
    // authenticated synced_commit is the public database identity.
    let _archive_sha256 = digest(&archive);

    let config_path = staging.root.join("audit-deny.toml");
    sensitive.add_path(&config_path);
    let policy = fs::read_to_string(context.path.join("deny.toml")).map_err(|_| {
        sensitive.error(
            "audit cargo-deny gate",
            "immutable dependency policy",
            "unavailable",
            "restore the committed dependency policy",
        )
    })?;
    let config = advisory_config(&policy, &staging.advisory_db, request.locator)?;
    fs::write(&config_path, config).map_err(|_| {
        sensitive.error(
            "audit cargo-deny gate",
            "isolated audit configuration",
            "write failure",
            "retry from a writable local release workspace",
        )
    })?;
    let config_text = config_path.to_str().ok_or_else(|| {
        sensitive.error(
            "audit cargo-deny gate",
            "UTF-8 isolated config path",
            "invalid path",
            "retry from a safe local release workspace",
        )
    })?;
    process_output(
        processes,
        &context.path,
        "cargo",
        &[
            "deny",
            "--locked",
            "--offline",
            "--config",
            config_text,
            "check",
            "advisories",
        ],
        &[("CARGO_NET_OFFLINE", OsStr::new("true"))],
        "audit cargo-deny gate",
        sensitive,
    )?;
    Ok(AuditStatus {
        product: PRODUCT.into(),
        source_cohort: SOURCE_COHORT.into(),
        synced_commit: receipt.synced_commit.clone(),
        utc: receipt.utc.clone(),
        max_age: receipt.max_age,
        checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        cargo_lock_sha256: context.cargo_lock_sha256.clone(),
        cargo_deny_version: cargo_deny_version.into(),
        verdict: "pass".into(),
    })
}

pub(crate) fn run_audit_mode(
    request: &AuditRequest<'_>,
    processes: &ProcessEnvironment,
    now: DateTime<Utc>,
    expected_pubkey_digest: &str,
    expected_key_id: &str,
    root_override: Option<&RepoRoot>,
) -> Result<AuditStatus> {
    let signature = signature_path(request.receipt)?;
    let mut sensitive = SensitiveValues::new(request, &signature);
    let result = (|| {
        validate_inputs(request, &signature, &sensitive)?;
        let minisign = observed_version("minisign", "-v").map_err(|_| {
            sensitive.error(
                "audit minisign-version gate",
                "minisign 0.11 or 0.12",
                "unavailable",
                "install minisign 0.12 and retry",
            )
        })?;
        validate_minisign_version(&minisign).map_err(|_| {
            sensitive.error(
                "audit minisign-version gate",
                "minisign 0.11 or 0.12",
                "unsupported",
                "install minisign 0.12 and retry",
            )
        })?;
        let public_key = fs::read(request.public_key).map_err(display_error)?;
        validate_public_key(
            &public_key,
            expected_pubkey_digest,
            expected_key_id,
            &sensitive,
        )?;
        let body = fs::read(request.receipt).map_err(display_error)?;
        let signature_bytes = fs::read(&signature).map_err(display_error)?;
        verify_minisign_bytes(
            request.receipt,
            request.public_key,
            &body,
            &signature_bytes,
            "audit receipt signature",
        )
        .map_err(|_| {
            sensitive.error(
                "audit signature gate",
                "valid approved signature",
                "verification failure",
                "restore the signed mirror receipt",
            )
        })?;
        let receipt = parse_receipt(&body, now, &sensitive)?;
        let comment = format!(
            "solpbc-advisory-mirror-v1 synced_commit={} utc={} max_age={MAX_AGE}",
            receipt.synced_commit, receipt.utc
        );
        verify_trusted_comment(&signature_bytes, &comment, "audit trusted comment").map_err(
            |_| {
                sensitive.error(
                    "audit trusted-comment gate",
                    "receipt-bound trusted comment",
                    "different comment",
                    "restore the signed mirror receipt",
                )
            },
        )?;
        validate_locator(request.locator, &sensitive)?;
        let cargo_deny_version = output_text(
            process_output(
                processes,
                request.receipt.parent().unwrap_or_else(|| Path::new(".")),
                "cargo",
                &["deny", "--version"],
                &[],
                "audit cargo-deny gate",
                &sensitive,
            )?,
            "audit cargo-deny gate",
            &sensitive,
        )?;
        if cargo_deny_version != format!("cargo-deny {CARGO_DENY_VERSION}") {
            return Err(sensitive.error(
                "audit cargo-deny gate",
                "cargo-deny 0.20.2",
                "unsupported version",
                "install cargo-deny 0.20.2 and retry",
            ));
        }
        let resolved_root;
        let root = if let Some(root) = root_override {
            root
        } else {
            resolved_root = RepoRoot::resolve().map_err(|_| {
                sensitive.error(
                    "audit isolation gate",
                    "solstone-linux repository root",
                    "unavailable",
                    "run from a clean local solstone-linux checkout",
                )
            })?;
            &resolved_root
        };
        sensitive.add_path(root.path());
        let lock = CandidateLock::acquire(root).map_err(|_| {
            sensitive.error(
                "audit isolation gate",
                "exclusive audit lock",
                "held or unavailable",
                "retry after the concurrent release transaction completes",
            )
        })?;
        let staging = StagingLayout::create(root, &lock).map_err(|_| {
            sensitive.error(
                "audit isolation gate",
                "isolated audit staging",
                "unavailable",
                "inspect only reserved release staging and retry",
            )
        })?;
        sensitive.add_path(&staging.root);
        let outcome = (|| {
            let context = export_immutable_context(root, &staging.context).map_err(|_| {
                sensitive.error(
                    "audit isolation gate",
                    "immutable release context",
                    "unavailable",
                    "retry from a clean local release workspace",
                )
            })?;
            run_materialized_audit(
                request,
                &receipt,
                &staging,
                &context,
                processes,
                &cargo_deny_version,
                &mut sensitive,
            )
        })();
        let audit = finish_candidate_staging_owned(root, &staging, outcome).map_err(|error| {
            let message = error.to_string();
            if message.starts_with("audit ") && message.lines().count() == 2 {
                error
            } else {
                sensitive.error(
                    "audit isolation gate",
                    "staging cleanup before witness",
                    "cleanup failure",
                    "inspect only reserved audit staging and retry",
                )
            }
        })?;
        lock.release().map_err(|_| {
            sensitive.error(
                "audit isolation gate",
                "audit lock release",
                "cleanup failure",
                "inspect only the reserved audit lock and retry",
            )
        })?;
        Ok(audit)
    })();
    result.map_err(|error| Error::new(sensitive.redact(&error.to_string())))
}
