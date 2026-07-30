// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fmt,
    fs::{self, File},
    io::{self, Write},
    os::fd::AsFd,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DurableWriteStage {
    Create,
    Write,
    Fsync,
    Rename,
    DirSync,
}

pub(crate) trait DurableWriteFault: Send + Sync {
    fn before(&self, stage: DurableWriteStage) -> io::Result<()>;
}

pub(crate) struct NoWriteFault;

impl DurableWriteFault for NoWriteFault {
    fn before(&self, _stage: DurableWriteStage) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) enum PrivateFileError {
    InvalidTarget(&'static str),
    Io {
        target: &'static str,
        operation: &'static str,
        kind: io::ErrorKind,
    },
}

impl PrivateFileError {
    fn io(target: &'static str, operation: &'static str, error: io::Error) -> Self {
        Self::Io {
            target,
            operation,
            kind: error.kind(),
        }
    }
}

impl fmt::Debug for PrivateFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for PrivateFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(target) => write!(formatter, "InvalidTarget({target})"),
            Self::Io {
                target,
                operation,
                kind,
            } => write!(formatter, "Io({target}, {operation}, {kind:?})"),
        }
    }
}

impl std::error::Error for PrivateFileError {}

pub(crate) fn ensure_private_directory(path: &Path) -> Result<(), PrivateFileError> {
    if path.as_os_str().is_empty() || path.parent().is_none() {
        return Err(PrivateFileError::InvalidTarget("directory"));
    }
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(PrivateFileError::InvalidTarget("directory"));
            }
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .ok_or(PrivateFileError::InvalidTarget("directory"))?;
            }
            Err(error) => return Err(PrivateFileError::io("directory", "inspect", error)),
        }
    }
    for directory in missing.iter().rev() {
        fs::create_dir(directory)
            .map_err(|error| PrivateFileError::io("directory", "create", error))?;
        set_and_verify_mode(directory, 0o700, true)?;
    }
    set_and_verify_mode(path, 0o700, true)
}

fn set_and_verify_mode(path: &Path, mode: u32, directory: bool) -> Result<(), PrivateFileError> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
        | if directory {
            rustix::fs::OFlags::DIRECTORY
        } else {
            rustix::fs::OFlags::empty()
        };
    let descriptor = rustix::fs::openat(rustix::fs::CWD, path, flags, rustix::fs::Mode::empty())
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
                PrivateFileError::InvalidTarget("target")
            } else {
                PrivateFileError::io("target", "open", error.into())
            }
        })?;
    let expected = rustix::fs::Mode::from_raw_mode(mode);
    rustix::fs::fchmod(&descriptor, expected)
        .map_err(|error| PrivateFileError::io("target", "chmod", error.into()))?;
    let stat = rustix::fs::fstat(&descriptor)
        .map_err(|error| PrivateFileError::io("target", "inspect", error.into()))?;
    let valid_kind = if directory {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory
    } else {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile
    };
    if !valid_kind || rustix::fs::Mode::from_raw_mode(stat.st_mode) != expected {
        return Err(PrivateFileError::InvalidTarget("target"));
    }
    Ok(())
}

pub(crate) fn open_regular_readonly(path: &Path) -> Result<File, PrivateFileError> {
    open_regular_readonly_at(rustix::fs::CWD, path)
}

fn open_regular_readonly_at<Fd: AsFd>(
    directory: Fd,
    path: &Path,
) -> Result<File, PrivateFileError> {
    let descriptor = rustix::fs::openat(
        directory,
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            PrivateFileError::InvalidTarget("file")
        } else {
            PrivateFileError::io("file", "open", error.into())
        }
    })?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| PrivateFileError::io("file", "inspect", error))?
        .is_file()
    {
        return Err(PrivateFileError::InvalidTarget("file"));
    }
    Ok(file)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), PrivateFileError> {
    atomic_write_bytes_with_fault(path, bytes, &NoWriteFault)
}

pub(crate) fn atomic_write_bytes_with_fault(
    path: &Path,
    bytes: &[u8],
    fault: &dyn DurableWriteFault,
) -> Result<(), PrivateFileError> {
    let parent = path
        .parent()
        .ok_or(PrivateFileError::InvalidTarget("file"))?;
    let name = path
        .file_name()
        .ok_or(PrivateFileError::InvalidTarget("file"))?;
    let parent_descriptor = open_directory(parent)?;
    match open_regular_readonly_at(&parent_descriptor, Path::new(name)) {
        Ok(file) => drop(file),
        Err(PrivateFileError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => {}
        Err(error) => return Err(error),
    }
    let temporary = format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        name = name.to_string_lossy()
    );
    let result = write_temporary(&parent_descriptor, name, &temporary, bytes, fault);
    if result.is_err() {
        let _ = rustix::fs::unlinkat(
            &parent_descriptor,
            temporary.as_str(),
            rustix::fs::AtFlags::empty(),
        );
        // Before rename the target is untouched; after rename it contains the new
        // complete value. Rewriting either value after an error cannot be made safe.
    }
    result
}

fn open_directory(path: &Path) -> Result<File, PrivateFileError> {
    rustix::fs::openat(
        rustix::fs::CWD,
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
            PrivateFileError::InvalidTarget("directory")
        } else {
            PrivateFileError::io("directory", "open", error.into())
        }
    })
}

fn write_temporary(
    parent: &File,
    name: &std::ffi::OsStr,
    temporary: &str,
    bytes: &[u8],
    fault: &dyn DurableWriteFault,
) -> Result<(), PrivateFileError> {
    fault
        .before(DurableWriteStage::Create)
        .map_err(|error| PrivateFileError::io("file", "create", error))?;
    let descriptor = rustix::fs::openat(
        parent,
        temporary,
        rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(|error| PrivateFileError::io("file", "create", error.into()))?;
    let mut file = File::from(descriptor);
    rustix::fs::fchmod(&file, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR)
        .map_err(|error| PrivateFileError::io("file", "chmod", error.into()))?;
    fault
        .before(DurableWriteStage::Write)
        .map_err(|error| PrivateFileError::io("file", "write", error))?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .map_err(|error| PrivateFileError::io("file", "write", error))?;
    fault
        .before(DurableWriteStage::Fsync)
        .map_err(|error| PrivateFileError::io("file", "fsync", error))?;
    file.sync_all()
        .map_err(|error| PrivateFileError::io("file", "fsync", error))?;
    fault
        .before(DurableWriteStage::Rename)
        .map_err(|error| PrivateFileError::io("file", "rename", error))?;
    rustix::fs::renameat(parent, temporary, parent, name)
        .map_err(|error| PrivateFileError::io("file", "rename", error.into()))?;
    fault
        .before(DurableWriteStage::DirSync)
        .map_err(|error| PrivateFileError::io("directory", "fsync", error))?;
    parent
        .sync_all()
        .map_err(|error| PrivateFileError::io("directory", "fsync", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    struct FailAt(DurableWriteStage);
    impl DurableWriteFault for FailAt {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            if stage == self.0 {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn ensure_private_directory_creates_private_tree() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("a/b");
        ensure_private_directory(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn ensure_private_directory_rejects_wrong_kinds_without_following() {
        for leaf in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let referent = temp.path().join("referent");
            fs::create_dir(&referent).unwrap();
            fs::set_permissions(&referent, fs::Permissions::from_mode(0o755)).unwrap();
            let link = temp.path().join("link");
            symlink(&referent, &link).unwrap();
            let target = if leaf { link } else { link.join("child") };
            assert!(ensure_private_directory(&target).is_err());
            assert_eq!(fs::metadata(&referent).unwrap().mode() & 0o777, 0o755);
        }
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(ensure_private_directory(&file.join("child")).is_err());
    }

    #[test]
    fn open_regular_readonly_accepts_only_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(open_regular_readonly(&file).is_ok());
        assert!(open_regular_readonly(temp.path()).is_err());
        let link = temp.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(open_regular_readonly(&link).is_err());
    }

    #[test]
    fn atomic_write_is_exact_private_and_regular() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        atomic_write_bytes(&path, b"complete").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(fs::read(path).unwrap(), b"complete");
    }

    #[test]
    fn every_pre_rename_failure_preserves_previous_complete_file() {
        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("state");
            atomic_write_bytes(&path, b"previous").unwrap();
            assert!(atomic_write_bytes_with_fault(&path, b"partial", &FailAt(stage)).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"previous", "{stage:?}");
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            }));
        }
    }

    #[test]
    fn directory_sync_failure_leaves_one_complete_json_value() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        let previous = br#"{"value":"previous"}"#;
        let next = br#"{"value":"next"}"#;
        atomic_write_bytes(&path, previous).unwrap();
        assert!(
            atomic_write_bytes_with_fault(&path, next, &FailAt(DurableWriteStage::DirSync))
                .is_err()
        );
        let current = fs::read(&path).unwrap();
        assert!(current == previous || current == next);
        assert!(serde_json::from_slice::<serde_json::Value>(&current).is_ok());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn every_injected_error_cleans_up_temporary_files() {
        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
            DurableWriteStage::DirSync,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("state");
            assert!(atomic_write_bytes_with_fault(&path, b"complete", &FailAt(stage)).is_err());
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            }));
        }
    }

    #[test]
    fn failed_initial_write_never_leaves_partial_target_or_temporary() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        assert!(
            atomic_write_bytes_with_fault(&path, b"partial", &FailAt(DurableWriteStage::Write))
                .is_err()
        );
        assert!(!path.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn error_format_never_contains_paths() {
        let secret = "/secret/owner/path";
        let error = open_regular_readonly(Path::new(secret)).unwrap_err();
        assert!(!format!("{error}").contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }
}
