// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The natively built release architectures. Every architecture-dependent
//! filename, package identity, ELF identity, image platform, and proof ID
//! derives from this one type.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReleaseArch {
    X86_64,
    Aarch64,
}

impl ReleaseArch {
    pub const ALL: [Self; 2] = [Self::X86_64, Self::Aarch64];

    /// The `uname -m` machine name, also the portable archive, manifest, and
    /// release policy table suffix.
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    pub const fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-linux-gnu",
            Self::Aarch64 => "aarch64-unknown-linux-gnu",
        }
    }

    pub const fn deb_arch(self) -> &'static str {
        match self {
            Self::X86_64 => "amd64",
            Self::Aarch64 => "arm64",
        }
    }

    pub const fn rpm_arch(self) -> &'static str {
        self.name()
    }

    /// The OCI image platform architecture reported by `image inspect`.
    pub const fn image_arch(self) -> &'static str {
        self.deb_arch()
    }

    /// ELF e_machine: EM_X86_64 = 62, EM_AARCH64 = 183.
    pub const fn elf_machine(self) -> u16 {
        match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
        }
    }

    pub const fn interpreter(self) -> &'static str {
        match self {
            Self::X86_64 => "/lib64/ld-linux-x86-64.so.2",
            Self::Aarch64 => "/lib/ld-linux-aarch64.so.1",
        }
    }

    pub const fn interpreter_soname(self) -> &'static str {
        match self {
            Self::X86_64 => "ld-linux-x86-64.so.2",
            Self::Aarch64 => "ld-linux-aarch64.so.1",
        }
    }

    pub fn proof_specs(self) -> [ProofSpec; 3] {
        let specs = ALL_PROOF_SPECS
            .into_iter()
            .filter(|spec| spec.arch == self)
            .collect::<Vec<_>>();
        [specs[0], specs[1], specs[2]]
    }

    pub fn proof_ids(self) -> [&'static str; 3] {
        self.proof_specs().map(|spec| spec.id)
    }

    pub fn manifest_name(self, version: &str) -> String {
        format!(
            "solstone-linux-{version}-linux-{}.rust-release-manifest.json",
            self.name()
        )
    }

    pub fn manifest_signature_name(self, version: &str) -> String {
        format!("{}.minisig", self.manifest_name(version))
    }

    /// Both architectures publish into one version directory, so only the
    /// original x86_64 set keeps the bare checksum name.
    pub const fn checksum_name(self) -> &'static str {
        match self {
            Self::X86_64 => CHECKSUM_NAME,
            Self::Aarch64 => "SHA256SUMS-aarch64",
        }
    }

    pub fn from_name(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|arch| arch.name() == value)
            .ok_or_else(|| {
                Error::new(format!(
                    "release architecture mismatch: expected x86_64 or aarch64, actual {value}\nrepair: run the release rail on a native x86_64 or aarch64 Linux host"
                ))
            })
    }

    pub fn from_triple(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|arch| arch.triple() == value)
            .ok_or_else(|| {
                Error::new(format!(
                    "release target mismatch: expected x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu, actual {value}"
                ))
            })
    }

    /// The machine name of the host running this process, from `uname -m`.
    pub fn host_machine() -> Result<String> {
        let output = Command::new("uname")
            .arg("-m")
            .output()
            .map_err(display_error)?;
        if !output.status.success() {
            return Err(Error::new(
                "host architecture mismatch: expected uname -m output, actual failure",
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn host() -> Result<Self> {
        Self::from_name(&Self::host_machine()?)
    }

    /// Native execution is the release proof: a candidate is never built or
    /// proven for an architecture other than the host's.
    pub fn require_native_on(self, host: Self) -> Result<()> {
        if self != host {
            return Err(Error::new(format!(
                "release architecture mismatch: expected native host architecture {}, actual requested {}\nrepair: run the {} release rail on a native {} host; cross-built candidates are not release evidence",
                host.name(),
                self.name(),
                self.name(),
                self.name()
            )));
        }
        Ok(())
    }

    pub fn require_native(self) -> Result<()> {
        self.require_native_on(Self::host()?)
    }
}

/// Resolve the requested release architecture against the host machine name.
/// Without a request the host architecture is selected.
pub fn select_release_arch(requested: Option<&str>, host_machine: &str) -> Result<ReleaseArch> {
    let host = ReleaseArch::from_name(host_machine)?;
    let Some(requested) = requested else {
        return Ok(host);
    };
    let requested = ReleaseArch::from_name(requested)?;
    requested.require_native_on(host)?;
    Ok(requested)
}
