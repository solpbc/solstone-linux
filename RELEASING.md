# Native Rust release rail

The shipping release rail is operator-run. The retained Python/PyPI rail is
not part of the shipping product rail, though its commands remain functional.
It produces portable, Debian, and RPM artifacts; it does not publish, tag, or
create a hosted release. Releases remain an operator-run process.

## 1. Host prerequisites

Run from a clean checkout with Git and either Podman or Docker. ShellCheck is
required by `make ci`. The canonical `make release` path also runs the host
Rust preflight, so it requires rustup plus the compiler and Cargo selected by
`rust-toolchain.toml`. Invoking `scripts/build-release.sh` directly requires
Git and the container engine but not host Cargo; the crate version is read
inside the build container.

Only x86_64 is supported. The build and install scripts refuse every other
architecture rather than placing an x86_64 binary under a misleading name.

The compiler authority is `rust-toolchain.toml` (`1.97.1`). Native package
tools are pinned to cargo-deb `3.7.0` and cargo-generate-rpm `0.21.0`; their
exact single-line version banners are asserted in the build container before
packaging starts.

## 2. Version source and output names

The Rust version comes from `[workspace.package].version` and the member's
`version.workspace = true`. It is independent of the Python package version.
Every artifact is written below `dist/rust/` and contains the Rust version:

- `solstone-linux-<VERSION>-linux-x86_64.tar.gz`
- `solstone-linux_<VERSION>-1_amd64.deb`
- `solstone-linux-<VERSION>-1.x86_64.rpm`

## 3. Build commands

The canonical command builds both native package families after its host
toolchain preflight:

```bash
make release
```

To invoke either container build directly without the host preflight, use:

Build each package family explicitly:

```bash
scripts/build-release.sh deb
scripts/build-release.sh rpm
```

Both commands also produce the same Ubuntu 22.04 baseline tarball. The whole
repository is the container build context because the Rust build script reads
and rasterizes `contrib/icons/`.

The Debian package is built inside Ubuntu. In particular, cargo-deb's
`depends = "$auto"` must run where `dpkg-shlibdeps` exists; an openSUSE host
does not provide it. The RPM stage runs cargo-generate-rpm's automatic
requirements scan in its native packaging environment but packages the binary
copied from the Ubuntu stage, preserving the glibc 2.35 floor.

## 4. Inspect artifacts

Before distributing anything, inspect metadata and contents:

```bash
tar -tzf dist/rust/solstone-linux-*-linux-x86_64.tar.gz
dpkg-deb -I dist/rust/solstone-linux_*-1_amd64.deb
dpkg-deb -c dist/rust/solstone-linux_*-1_amd64.deb
rpm -qpi dist/rust/solstone-linux-*-1.x86_64.rpm
rpm -qpl dist/rust/solstone-linux-*-1.x86_64.rpm
sha256sum dist/rust/*
```

Confirm that each artifact contains the binary, LICENSE, INSTALL-NOTES, and the
icon set declared in the member package manifest. It must contain no systemd
unit and no desktop file.

## 5. Blocking first-release FLAC validation

This checkpoint is mandatory. Do not release based only on a successful link.

The release binary statically links the bundled libFLAC, so it has no direct
cross-distribution libFLAC runtime dependency. A distro's PulseAudio stack may
independently load its own libFLAC through libsndfile. The pre-release soak must
still exercise real encoded output through the shipped binary:

1. Install the produced artifact on a test Linux desktop with the runtime
   dependencies from `packaging/INSTALL-NOTES`.
2. Run the packaged `solstone-linux` observer long enough to produce a new
   audio segment.
3. Confirm the observer remains alive and validate that exact segment with
   `flac -t path/to/new/audio.flac` (or each split mono FLAC).
4. Treat any encoder crash or decode failure as a release blocker.

If the checkpoint fails, stop and diagnose the bundled encoder before release.

## 6. Portable installer

Preview a local tarball installation without writes:

```bash
scripts/install.sh --dry-run "dist/rust/solstone-linux-<VERSION>-linux-x86_64.tar.gz"
```

Install to the default `$HOME/.local` prefix:

```bash
scripts/install.sh "dist/rust/solstone-linux-<VERSION>-linux-x86_64.tar.gz"
```

The script reports when `$HOME/.local/bin` is not on PATH. A different prefix
requires explicit `--prefix PATH`; the script never silently invokes sudo.
Unknown distribution families stop without making changes.

Run `solstone-linux install-service` after installing the binary. The native
command writes the user unit and desktop autostart entry, reloads systemd, and
enables and starts the observer service.

## 7. Runtime dependencies

The canonical cross-distribution list is committed once in
`packaging/INSTALL-NOTES` and is included in every artifact. Verify it against
the intended test machine before the soak.

## 8. Manual release handoff

After both builds, artifact inspection, checksums, and the blocking FLAC soak
succeed, upload the three versioned files and checksum list through the chosen
manual release surface. Do not reuse the Python version, Python tag/publish
script, or PyPI release artifacts.

Release-note bodies come only from the matching `CHANGELOG.md` block. Extract
that block with `scripts/extract_changelog.sh <VERSION>`; do not create a
separate engineering-tone release-note template here.

## 9. Known constraints

- x86_64 only
- glibc 2.35 baseline
- no packaged unit file or desktop file
- native service files are installed at runtime rather than packaged
- release panics unwind; reconsider abort only at the Rust cutover

## 10. Failure recovery

Container builds and local installs do not publish, tag, or push. Fix the
reported problem, remove only the affected files under `dist/rust/`, and rerun
the relevant `deb` or `rpm` command. Never relabel an artifact built for a
different architecture or version.

## 11. Evidence classes

| Evidence class | What it proves | What it does not prove |
|---|---|---|
| Host evidence | Source formatting, lint, tests, and offline dependency policy | Target-distribution packaging or runtime behavior |
| Target-package drift evidence | Container compiler/tool pins and distro-native package construction | Installed-artifact behavior or the release soak |
| Shipped-artifact proof | Artifact contents, linkage, installation, and the manual FLAC soak | Behavior outside the tested artifact and environment |

`make ci` names itself as host evidence. Container package gates name the
target-package class. Neither may claim the blocking FLAC soak ran; only the
operator completing section 5 has shipped-artifact proof.

## 12. Dependency policy

`make ci` runs cargo-deny offline for licenses, bans, and sources. It does not
fetch or inspect advisories. `make audit` first refreshes the RustSec database
and stops nonzero if refresh fails, then performs the locked advisory check.
This prevents stale cached data from being presented as freshly audited.
