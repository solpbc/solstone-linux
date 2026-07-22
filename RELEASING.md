# Native Rust release candidate rail

The native rail creates local release-candidate evidence. It does not tag, sign,
upload, publish, or approve publication. Publication is unavailable in this rail.
Only x86_64 is supported.

Four evidence activities remain deliberately separate:

1. The offline release-manifest validator checks a named manifest or an exact
   five-file payload directory.
2. The candidate transaction builds tar, Debian, and RPM artifacts from one
   committed immutable context and atomically promotes the five-file payload.
3. Three package-bound proofs install and verify the Debian, RPM, and tar
   artifacts in separate network-disabled environments.
4. The live FLAC checkpoint exercises the observer on a desktop after candidate
   proof. It is not part of `candidate-proven`.

## Operator preconditions

Use a clean checkout at the exact release commit. The compiler authority is
`rust-toolchain.toml`; Cargo operations use `Cargo.lock`.

Regenerate all four Ubuntu and Fedora release images from the locally pinned stock
bases, without pulling, then inspect their bare local image IDs:

```bash
make release-images
podman image inspect --format '{{.Id}}' localhost/solstone-linux-build-ubuntu
podman image inspect --format '{{.Id}}' localhost/solstone-linux-build-fedora
podman image inspect --format '{{.Id}}' localhost/solstone-linux-proof-ubuntu
podman image inspect --format '{{.Id}}' localhost/solstone-linux-proof-fedora
```

Commit those IDs as `sha256:<bare-id>` in the matching build and proof roles of
`packaging/release-policy.toml`. Rebuild the build-tool images when
`Cargo.lock`; dependency-affecting workspace or member manifests;
`rust-toolchain.toml`; the Rust, cargo-deb, or cargo-generate-rpm pins; the stock
base references; or `packaging/Containerfile.tools` changes. The Ubuntu image
carries Cargo state warmed for the committed lockfile. The Fedora image
deliberately carries no warmed Cargo registry because its offline metadata and RPM
generation path does not require one. Rebuild the proof images when their stock
base references, runtime dependency closure, or `packaging/Containerfile.tools`
changes.

Every `make release-images` run uses `--no-cache`, produces new local image IDs,
and requires all four observed IDs to be re-pinned. Locally regenerated images are
not guaranteed to be bit-for-bit reproducible: distro repositories, rustup
distribution content, and tool build environments can change while declared
versions remain the same. Prove each freshly regenerated image offline before
committing its observed ID.
Re-read and repin IDs only after `make release-images` exits successfully; after a
partial failure, rerun the complete target before pinning any image.

The `proof_debian`, `proof_rpm`, and `proof_tar` roles use clean OS images carrying
only the runtime dependency closure, with no compiler, Rust toolchain, Cargo, or
other build tooling. Stock bases cannot install or execute the dynamically linked
observer because it requires GLib, GStreamer, and PulseAudio libraries. Debian and
RPM proofs install into the disposable proof container's live root, so their
declared dependencies are genuinely enforced; the tar proof retains its dedicated
`/proof-root`. Keep all four images backing the five policy roles present locally
before transaction entry. For each proof image, observe and commit the exact normalized OS release,
package-manager output, install argv, version argv, executable path, and executable
mode. Commit those values before selecting `EXPECTED_RELEASE_COMMIT`. The proof
producer observes the same values inside the selected image and fails closed when
committed policy is wrong. The transaction never pulls an image.

Acquire the advisory database outside the transaction. It must be a clean local Git
worktree, including no untracked or ignored changes. Write a strict descriptor with
exactly these private operator inputs:

```json
{
  "schema_version": 1,
  "source_id": "privacy-safe cohort identifier",
  "db_path": "/absolute/canonical/path/to/local/advisory-db",
  "acquired_at": "2026-07-21T00:00:00Z"
}
```

The descriptor and database paths never enter public candidate evidence. Candidate
creation requires acquisition within 24 hours. Proof resume validates the retained
database identity without reapplying that freshness window.

## Candidate commands

Create the complete candidate and all three proofs:

```bash
make release-candidate \
  EXPECTED_RELEASE_COMMIT=<full-lowercase-git-commit> \
  ADVISORY_DESCRIPTOR=<descriptor.json>
```

`make release` delegates to this same target and requires the same variables. There
is no second native release state machine.

Resume missing proofs without rebuilding or changing valid retained evidence:

```bash
make release-candidate-prove \
  VERSION=<version> \
  ADVISORY_DESCRIPTOR=<descriptor.json>
```

Read-only recovery validation requires no advisory descriptor:

```bash
make release-candidate-recover VERSION=<version>
```

All mutating candidate commands use `dist/.rust-release-candidate.lock`. Commands do
not auto-clear a stale lock. After confirming no candidate process is running, the
operator may remove that exact lock file and retry. Do not remove any broader
directory as lock recovery.

## Outputs and meanings

The promoted payload is exactly:

- `dist/rust/solstone-linux-<VERSION>-linux-x86_64.tar.gz`
- `dist/rust/solstone-linux_<VERSION>-1_amd64.deb`
- `dist/rust/solstone-linux-<VERSION>-1.x86_64.rpm`
- `dist/rust/SHA256SUMS`
- `dist/rust/solstone-linux-<VERSION>-linux-x86_64.rust-release-manifest.json`

Retained evidence is versioned and disjoint from the payload:

- `dist/rust-evidence/<VERSION>/ledger.json`
- `dist/rust-evidence/<VERSION>/proof-runner`
- `dist/rust-evidence/<VERSION>/proofs/debian-amd64.json`
- `dist/rust-evidence/<VERSION>/proofs/rpm-x86_64.json`
- `dist/rust-evidence/<VERSION>/proofs/tar-x86_64.json`

`candidate-proven` means all local payload, ledger, policy, image, source, and three
package-install/version proofs validated together. `retained-candidate-valid` means
the retained candidate validates read-only in the matching clean checkout. Both are
local evidence statuses. Neither is publication approval, and neither includes the
live FLAC checkpoint.

The Debian and RPM proofs install only their bound local package. The tar proof runs
both installer dry-run and isolated-prefix installation. All three validate the
installed executable path, mode, hash, and exact version output with networking
disabled. Candidate creation builds one proof runner in the pinned Ubuntu build
environment and retains it with the evidence. Initial proof and proof resume both use
the runner at that reserved path after revalidating it as a no-follow regular
executable, so resume does not depend on the invoking host binary or a toolchain inside
a proof image. The runner bytes are not bound into the candidate ledger; treat the
retained candidate directory as trusted between creation and resume.

## Validator

Run the offline fixture and schema gate:

```bash
make check-rust-release-manifest
```

Validate a named manifest or classify an exact payload:

```bash
make check-rust-release-manifest MANIFEST=dist/rust/solstone-linux-<VERSION>-linux-x86_64.rust-release-manifest.json
make check-rust-release-manifest RELEASE_DIR=dist/rust
```

Named-manifest validation does not imply candidate readiness. Directory
classification requires exactly five regular files and rejects stale or extra
entries.

Release roots must be real confined directories. The portable, Debian, and RPM
packages must contain the same executable bytes.

## Non-candidate drift helper

The individual lane helper is deliberately outside candidate state:

```bash
bash scripts/build-release.sh deb
bash scripts/build-release.sh rpm
```

It writes only `dist/rust-drift/`, labels its output as drift evidence, and cannot
create or replace the candidate payload or readiness evidence. Drift output is not
retained candidate evidence.

## Blocking live FLAC checkpoint

After `candidate-proven`, install the relevant candidate artifact on a test Linux
desktop with the runtime dependencies in `packaging/INSTALL-NOTES`. Run the packaged
observer long enough to produce a new audio segment, confirm the observer remains
alive, and validate that exact segment with `flac -t` (including each split mono
FLAC when applicable). An encoder crash or decode failure blocks release handling.

This live checkpoint is separate operator evidence. It does not modify the ledger,
proofs, bundle digest, or candidate status.

## Host and advisory gates

`make ci` produces host evidence: formatting, lint, tests, shell checks, and the
offline licenses/bans/sources policy. It does not run target package proofs or the
live FLAC checkpoint.

`make audit` refreshes advisory data for a separate operator audit. Candidate
creation instead consumes the explicitly acquired descriptor cohort and leaves the
repository `deny.toml` unchanged.

## Release transparency

After delivery, publish the retained candidate with `make publish-transparency
RELEASE_DIR=<retained-candidate>`. This env-driven, retryable step never gates
delivery; see the retained candidate paths above rather than reconstructing its
inventory. Minisign is a development prerequisite. Version keys are one-shot and
permanent. The final `latest.json` body PUT is the commit boundary: before it the
pointer body is old and afterward it is new. The signature-first write can briefly
produce a pointer/signature mismatch; consumers retry that recognized transient.
The archive retains the candidate artifact bytes alongside the release evidence;
the public transparency surface carries evidence only, never artifact bytes.
If an entry was uploaded before its pointer, retry the same command; if
the version was permanently recorded against a superseded chain head, cut the next
version.

`make resign-transparency-pointer` is the freeze defense. It first verifies the
signed pointer, signed tip, product binding, and rollback protection against the
[transparency head log](transparency-head-log.jsonl), then renews only the pointer
signature and validity. It never re-attests a rolled-back or foreign pointer.

The surface attests what was released, that it is immutable, and that history is
publicly reconstructible — not that binaries provably match source. Publication is
operator-approved and separate from the retained legacy Python publisher, which is
not a native release path.
