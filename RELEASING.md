# Rust release rail

The Rust release rail is operator-run and separate from the Python/PyPI rail.
It produces portable, Debian, and RPM artifacts; it does not publish, tag, or
create a hosted release. Releases remain manual by policy—do not add CI/CD
publishing for this repository.

## 1. Host prerequisites

Run from a clean checkout with Git and either Podman or Docker. ShellCheck is
required by `make ci`. Cargo and jq are not host requirements: the crate
version is read with Cargo inside the build container, where Cargo is already
needed to compile the program.

Only x86_64 is supported. The build and install scripts refuse every other
architecture rather than placing an x86_64 binary under a misleading name.

## 2. Version source and output names

The Rust version comes from `[workspace.package].version` and the member's
`version.workspace = true`. It is independent of the Python package version.
Every artifact is written below `dist/rust/` and contains the Rust version:

- `solstone-linux-0.1.0-linux-x86_64.tar.gz`
- `solstone-linux_0.1.0-1_amd64.deb`
- `solstone-linux-0.1.0-1.x86_64.rpm`

## 3. Build commands

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

Confirm that each artifact contains the binary, LICENSE, INSTALL-NOTES, and all
13 icons. It must contain no systemd unit and no desktop file.

## 5. Blocking first-release FLAC validation

This checkpoint is mandatory. Do not release based only on a successful link.

`flac-bound` 0.5 documents system libFLAC 1.4 or newer for its
`libflac-nobuild` feature. Ubuntu 22.04 supplies libFLAC 1.3.3 with soname 8.
The code uses a small encoder surface, but an ABI mismatch could still crash at
runtime. The pre-release soak must therefore exercise real output through the
shipped binary:

1. Install the produced artifact on a test Linux desktop with the runtime
   dependencies from `packaging/INSTALL-NOTES`.
2. Run the packaged `solstone-linux` observer long enough to produce a new
   audio segment.
3. Confirm the observer remains alive and validate that exact segment with
   `flac -t path/to/new/audio.flac` (or each split mono FLAC).
4. Treat any encoder crash or decode failure as a release blocker.

If the checkpoint fails, stop. The follow-up is a source-built libFLAC >=1.4
layer in `packaging/Containerfile`; changing the crate feature or raising the
Ubuntu baseline is not part of this rail.

## 6. Portable installer

Preview a local tarball installation without writes:

```bash
scripts/install.sh --dry-run dist/rust/solstone-linux-0.1.0-linux-x86_64.tar.gz
```

Install to the default `$HOME/.local` prefix:

```bash
scripts/install.sh dist/rust/solstone-linux-0.1.0-linux-x86_64.tar.gz
```

The script reports when `$HOME/.local/bin` is not on PATH. A different prefix
requires explicit `--prefix PATH`; the script never silently invokes sudo.
Unknown distribution families stop without making changes.

The Rust `install-service` command is not implemented yet. It is an optional
future step, and its current failure must not invalidate a binary install.

## 7. Runtime dependencies

The canonical cross-distribution list is committed once in
`packaging/INSTALL-NOTES` and is included in every artifact. Verify it against
the intended test machine before the soak.

## 8. Manual release handoff

After both builds, artifact inspection, checksums, and the blocking FLAC soak
succeed, upload the three versioned files and checksum list through the chosen
manual release surface. Do not reuse the Python version, Python tag/publish
script, or PyPI release artifacts.

## 9. Known constraints

- x86_64 only
- glibc 2.35 baseline
- no packaged unit file or desktop file
- Rust `install-service` remains a stub
- release panics unwind; reconsider abort only at the Rust cutover

## 10. Failure recovery

Container builds and local installs do not publish, tag, or push. Fix the
reported problem, remove only the affected files under `dist/rust/`, and rerun
the relevant `deb` or `rpm` command. Never relabel an artifact built for a
different architecture or version.
