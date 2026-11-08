// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum PathError {
    InvalidTarget(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(path) => {
                write!(
                    formatter,
                    "{} is a symlink or wrong file type",
                    path.display()
                )
            }
            Self::Io { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn ensure_private_directory(path: &Path) -> Result<(), PathError> {
    ensure_directory(path, true)
}

fn ensure_directory(path: &Path, enforce_existing_mode: bool) -> Result<(), PathError> {
    if path.as_os_str().is_empty() || path.parent().is_none() {
        return Err(PathError::InvalidTarget(path.to_owned()));
    }
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(PathError::InvalidTarget(current.to_owned()));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_owned());
                current = current
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .ok_or_else(|| PathError::InvalidTarget(path.to_owned()))?;
            }
            Err(source) => {
                return Err(PathError::Io {
                    path: current.to_owned(),
                    source,
                });
            }
        }
    }

    for directory in missing.iter().rev() {
        fs::create_dir(directory).map_err(|source| PathError::Io {
            path: directory.clone(),
            source,
        })?;
        set_and_verify_mode(directory, 0o700, true)?;
    }
    if enforce_existing_mode || !missing.is_empty() {
        set_and_verify_mode(path, 0o700, true)?;
    }
    Ok(())
}

fn set_and_verify_mode(path: &Path, mode: u32, directory: bool) -> Result<(), PathError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        PathError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| PathError::Io {
        path: path.to_owned(),
        source,
    })?;
    let correct_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if metadata.file_type().is_symlink()
        || !correct_type
        || metadata.permissions().mode() & 0o777 != mode
    {
        return Err(PathError::InvalidTarget(path.to_owned()));
    }
    Ok(())
}
