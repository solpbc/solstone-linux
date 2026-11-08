// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::instance_lock::{InstanceLock, InstanceLockError, LOCK_FILENAME};
use crate::name::{DerivedName, derive_component, valid_activity_filename};
use crate::recovery::{RecoveryAction, RecoveryOptions, recover_stream_inner, valid_date_name};
use crate::storage::{CaptureTime, CaptureTimeLoad, capture_time_path, load_capture_time};

#[derive(Debug)]
pub struct Guard {
    root: PathBuf,
    lock: Option<InstanceLock>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacySegment {
    pub day: String,
    pub segment: String,
    pub dir: PathBuf,
    pub files: Vec<String>,
    pub zone: Option<CaptureTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeldReason {
    Failed,
    Retained,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldEntry {
    pub day: String,
    pub segment: String,
    pub dir: PathBuf,
    pub reason: HeldReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub path: PathBuf,
    pub reason: String,
}

/// What recovery left held, and the entries it could not recover.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyRecovery {
    pub held: Vec<HeldEntry>,
    pub problems: Vec<Problem>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyListing {
    pub segments: Vec<LegacySegment>,
    pub held: Vec<HeldEntry>,
    pub problems: Vec<Problem>,
}

pub fn open(legacy_data_root: &Path) -> Result<Guard, LegacyError> {
    match fs::symlink_metadata(legacy_data_root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(LegacyError::UnreadableRoot(legacy_data_root.to_owned()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Guard {
                root: legacy_data_root.to_owned(),
                lock: None,
            });
        }
        Err(source) => {
            return Err(LegacyError::Io {
                operation: "inspect legacy root",
                path: legacy_data_root.to_owned(),
                source,
            });
        }
    }

    let lock_path = legacy_data_root.join(LOCK_FILENAME);
    match InstanceLock::acquire(legacy_data_root) {
        Ok(lock) => Ok(Guard {
            root: legacy_data_root.to_owned(),
            lock: Some(lock),
        }),
        Err(InstanceLockError::AlreadyLocked(_)) => Err(LegacyError::StillRunning(lock_path)),
        Err(InstanceLockError::InvalidTarget(_)) => Err(LegacyError::UnusableLock(lock_path)),
        Err(InstanceLockError::Io {
            operation,
            path,
            source,
        }) => Err(LegacyError::Io {
            operation,
            path,
            source,
        }),
    }
}

impl Guard {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn lock(&self) -> Option<&InstanceLock> {
        self.lock.as_ref()
    }

    /// Runs 2.0.28's start-up recovery over every stream under `captures/<day>/`.
    /// One bad entry does not hide the rest: an error on a day or stream is a
    /// problem naming its path, and recovery goes on. Segments recovery retained,
    /// could not finalize or quarantined are returned as held.
    pub fn recover(&self, segment_interval: u64) -> Result<LegacyRecovery, LegacyError> {
        let mut report = LegacyRecovery::default();
        let Some(days) = self.capture_days(&mut report.problems)? else {
            return Ok(report);
        };
        let interval = Duration::from_secs(segment_interval);
        for (day, day_path) in days {
            for stream_dir in stream_directories(&day_path, &mut report.problems) {
                match recover_stream_inner(
                    &self.root,
                    &stream_dir,
                    interval,
                    RecoveryOptions::default(),
                ) {
                    Ok(records) => {
                        for record in records {
                            let reason = match record.action {
                                RecoveryAction::Retain => HeldReason::Retained,
                                RecoveryAction::Failed | RecoveryAction::Quarantine => {
                                    HeldReason::Failed
                                }
                                _ => continue,
                            };
                            let segment = record
                                .candidate
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or_default()
                                .to_owned();
                            report.held.push(HeldEntry {
                                day: day.clone(),
                                segment,
                                dir: record.candidate,
                                reason,
                            });
                        }
                    }
                    Err(error) => report.problems.push(Problem {
                        path: stream_dir,
                        reason: error.to_string(),
                    }),
                }
            }
        }
        Ok(report)
    }

    /// Lists the spool with 2.0.28's sync enumeration and per-segment
    /// classification. Empty segments are removed with their zone sibling.
    pub fn segments(&self) -> Result<LegacyListing, LegacyError> {
        let mut listing = LegacyListing::default();
        let Some(days) = self.capture_days(&mut listing.problems)? else {
            return Ok(listing);
        };
        for (day, day_path) in days {
            for stream_dir in stream_directories(&day_path, &mut listing.problems) {
                self.scan_stream_directory(&day, &stream_dir, &mut listing);
            }
        }
        Ok(listing)
    }

    /// The `captures/<day>` directories, by 2.0.28's day rule. `None` when there
    /// is no `captures` directory; an error when it cannot be read at all.
    fn capture_days(
        &self,
        problems: &mut Vec<Problem>,
    ) -> Result<Option<Vec<(String, PathBuf)>>, LegacyError> {
        let captures = self.root.join("captures");
        match fs::symlink_metadata(&captures) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(LegacyError::UnreadableCaptures(captures));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(LegacyError::Io {
                    operation: "inspect captures directory",
                    path: captures,
                    source,
                });
            }
        }
        let entries = fs::read_dir(&captures).map_err(|source| LegacyError::Io {
            operation: "scan capture dates",
            path: captures.clone(),
            source,
        })?;
        let mut days = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    problems.push(Problem {
                        path: captures.clone(),
                        reason: format!("could not read a capture date entry: {source}"),
                    });
                    continue;
                }
            };
            let Some(day) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !valid_date_name(&day) {
                continue;
            }
            if !plain_directory(&entry.path()) {
                problems.push(Problem {
                    path: entry.path(),
                    reason: "day directory is a symlink or special target".to_owned(),
                });
                continue;
            }
            days.push((day, entry.path()));
        }
        days.sort();
        Ok(Some(days))
    }

    fn scan_stream_directory(&self, day: &str, stream_dir: &Path, listing: &mut LegacyListing) {
        let entries = match fs::read_dir(stream_dir) {
            Ok(entries) => entries,
            Err(source) => {
                listing.problems.push(Problem {
                    path: stream_dir.to_owned(),
                    reason: format!("could not scan stream directory: {source}"),
                });
                return;
            }
        };

        let mut entries_by_name = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => entries_by_name.push(entry),
                Err(source) => listing.problems.push(Problem {
                    path: stream_dir.to_owned(),
                    reason: format!("could not read a stream directory entry: {source}"),
                }),
            }
        }
        entries_by_name.sort_by_key(|entry| entry.file_name());

        for entry in entries_by_name {
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                listing.problems.push(Problem {
                    path,
                    reason: "entry name is not UTF-8".to_owned(),
                });
                continue;
            };

            if name.ends_with(".meta") || name.ends_with(".zone.json") {
                continue;
            }

            if name.ends_with(".failed") || name.ends_with(".incomplete") {
                if plain_directory(&path) {
                    listing.held.push(HeldEntry {
                        day: day.to_owned(),
                        segment: name.clone(),
                        dir: path,
                        reason: if name.ends_with(".failed") {
                            HeldReason::Failed
                        } else {
                            HeldReason::Retained
                        },
                    });
                }
                continue;
            }

            if !valid_segment_name(&name) {
                continue;
            }
            if !plain_directory(&path) {
                listing.problems.push(Problem {
                    path,
                    reason: "segment entry is a symlink or not a directory".to_owned(),
                });
                continue;
            }
            match resolve_segment_files(&path, &name) {
                ResolvedFiles::Found(files) => {
                    let zone_path = capture_time_path(stream_dir, &name);
                    let zone = match load_capture_time(&zone_path) {
                        CaptureTimeLoad::Present(capture_time) => Some(capture_time),
                        _ => None,
                    };
                    listing.segments.push(LegacySegment {
                        day: day.to_owned(),
                        segment: name,
                        dir: path,
                        files,
                        zone,
                    });
                }
                ResolvedFiles::Empty => match fs::remove_dir(&path) {
                    // As 2.0.28: the zone sibling goes only once the directory is gone.
                    Ok(()) => {
                        let _ = fs::remove_file(capture_time_path(stream_dir, &name));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let _ = fs::remove_file(capture_time_path(stream_dir, &name));
                    }
                    Err(source) => listing.problems.push(Problem {
                        path,
                        reason: format!("could not remove the empty segment: {source}"),
                    }),
                },
                ResolvedFiles::Invalid => {
                    listing.held.push(HeldEntry {
                        day: day.to_owned(),
                        segment: name,
                        dir: path,
                        reason: HeldReason::Invalid,
                    });
                }
            }
        }
    }
}

/// The stream directories under one day, by 2.0.28's rule: any entry whose name
/// is a valid component and that is not a regular file. One that is a symlink or
/// special target is a problem.
fn stream_directories(day_path: &Path, problems: &mut Vec<Problem>) -> Vec<PathBuf> {
    let entries = match fs::read_dir(day_path) {
        Ok(entries) => entries,
        Err(source) => {
            problems.push(Problem {
                path: day_path.to_owned(),
                reason: format!("could not scan stream directories: {source}"),
            });
            return Vec::new();
        }
    };
    let mut streams = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                problems.push(Problem {
                    path: day_path.to_owned(),
                    reason: format!("could not read a stream entry: {source}"),
                });
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(stream) = derive_component(&name) else {
            continue;
        };
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if !plain_directory(&path) {
            problems.push(Problem {
                path,
                reason: "stream directory is a symlink or special target".to_owned(),
            });
            continue;
        }
        match stream.join_checked(day_path) {
            Ok(stream_path) if stream_path == path => streams.push(stream_path),
            _ => problems.push(Problem {
                path,
                reason: "stream directory name is not in canonical form".to_owned(),
            }),
        }
    }
    streams.sort();
    streams
}

fn plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

enum ResolvedFiles {
    Found(Vec<String>),
    Empty,
    Invalid,
}

fn resolve_segment_files(segment_dir: &Path, segment_name: &str) -> ResolvedFiles {
    let entries = match fs::read_dir(segment_dir) {
        Ok(entries) => entries,
        Err(_) => return ResolvedFiles::Invalid,
    };

    let stem = segment_name.split_once('_').map(|(stem, _)| stem);
    let mut files = Vec::new();

    for entry in entries {
        let Ok(entry) = entry else {
            return ResolvedFiles::Invalid;
        };
        let Ok(file_type) = entry.file_type() else {
            return ResolvedFiles::Invalid;
        };
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            return ResolvedFiles::Invalid;
        };
        if file_type.is_symlink()
            || !file_type.is_file()
            || metadata.file_type().is_symlink()
            || !metadata.is_file()
        {
            return ResolvedFiles::Invalid;
        }

        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            return ResolvedFiles::Invalid;
        };

        let valid = valid_capture_filename(&filename)
            || stem.is_some_and(|s| valid_activity_filename(&filename, s));
        if !valid {
            return ResolvedFiles::Invalid;
        }

        files.push(filename);
    }

    if files.is_empty() {
        return ResolvedFiles::Empty;
    }

    files.sort();
    ResolvedFiles::Found(files)
}

fn valid_capture_filename(value: &str) -> bool {
    let Some(session) = value
        .strip_prefix("tmux_")
        .and_then(|val| val.strip_suffix("_screen.jsonl"))
    else {
        return false;
    };
    exact_component(value).is_some() && exact_component(session).is_some()
}

fn exact_component(value: &str) -> Option<DerivedName> {
    derive_component(value)
        .ok()
        .filter(|component| component.as_str() == value)
}

pub(crate) fn valid_segment_name(value: &str) -> bool {
    let Some((time_part, duration_part)) = value.split_once('_') else {
        return false;
    };
    if time_part.len() != 6
        || duration_part.len() < 3
        || !time_part.bytes().all(|byte| byte.is_ascii_digit())
        || !duration_part.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let hour = time_part[0..2].parse::<u8>().ok();
    let minute = time_part[2..4].parse::<u8>().ok();
    let second = time_part[4..6].parse::<u8>().ok();
    matches!(
        (hour, minute, second),
        (Some(h), Some(m), Some(s)) if time::Time::from_hms(h, m, s).is_ok()
    )
}

#[derive(Debug)]
pub enum LegacyError {
    StillRunning(PathBuf),
    UnusableLock(PathBuf),
    UnreadableRoot(PathBuf),
    UnreadableCaptures(PathBuf),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for LegacyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StillRunning(path) => {
                write!(formatter, "{}: still running", path.display())
            }
            Self::UnusableLock(path) => {
                write!(formatter, "{}: unusable lock", path.display())
            }
            Self::UnreadableRoot(path) => {
                write!(formatter, "{}: unreadable directory", path.display())
            }
            Self::UnreadableCaptures(path) => {
                write!(
                    formatter,
                    "{}: unreadable captures directory",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => {
                write!(
                    formatter,
                    "{operation} failed for {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for LegacyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
