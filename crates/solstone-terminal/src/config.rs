// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const DEFAULT_CAPTURE_INTERVAL_SECONDS: u64 = 5;
pub const DEFAULT_SEGMENT_INTERVAL_SECONDS: u64 = 300;

fn default_capture_interval() -> u64 {
    DEFAULT_CAPTURE_INTERVAL_SECONDS
}

fn default_segment_interval() -> u64 {
    DEFAULT_SEGMENT_INTERVAL_SECONDS
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureSettings {
    #[serde(default = "default_capture_interval")]
    pub capture_interval: u64,
    #[serde(default = "default_segment_interval")]
    pub segment_interval: u64,
    #[serde(default = "default_true")]
    pub status_indicator: bool,
    #[serde(default = "default_true")]
    pub pane_title: bool,
    #[serde(default = "default_true")]
    pub working_directory: bool,
    #[serde(default = "default_true")]
    pub executable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_sessions: Vec<String>,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            capture_interval: DEFAULT_CAPTURE_INTERVAL_SECONDS,
            segment_interval: DEFAULT_SEGMENT_INTERVAL_SECONDS,
            status_indicator: true,
            pane_title: true,
            working_directory: true,
            executable: true,
            output_sessions: Vec::new(),
        }
    }
}

impl CaptureSettings {
    /// Reads a settings file the caller owns. The file is only read, never
    /// written or re-permissioned; a missing, unreadable or invalid file is an
    /// error.
    pub fn load_file(path: &Path) -> Result<Self, SettingsError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| SettingsError::Io {
            operation: "inspect settings file",
            path: path.to_owned(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SettingsError::InvalidTarget(path.to_owned()));
        }
        let bytes = fs::read(path).map_err(|source| SettingsError::Io {
            operation: "read settings file",
            path: path.to_owned(),
            source,
        })?;
        Self::parse_slice(&bytes, path)
    }

    pub fn parse_json(json: &str) -> Result<Self, SettingsError> {
        Self::parse_slice(json.as_bytes(), Path::new("inline.json"))
    }

    fn parse_slice(bytes: &[u8], path: &Path) -> Result<Self, SettingsError> {
        let settings: Self =
            serde_json::from_slice(bytes).map_err(|source| SettingsError::InvalidConfig {
                path: path.to_owned(),
                detail: source.to_string(),
            })?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.capture_interval == 0 {
            return Err(SettingsError::InvalidInterval {
                field: "capture_interval",
            });
        }
        if self.segment_interval == 0 {
            return Err(SettingsError::InvalidInterval {
                field: "segment_interval",
            });
        }
        if self.output_sessions.iter().any(String::is_empty) {
            return Err(SettingsError::InvalidOutputSessions);
        }
        Ok(())
    }

    pub fn capture_interval_duration(&self) -> Duration {
        Duration::from_secs(self.capture_interval)
    }

    pub fn segment_interval_duration(&self) -> Duration {
        Duration::from_secs(self.segment_interval)
    }
}

pub fn read_lenient_legacy_segment_interval(path: &Path) -> u64 {
    let Ok(bytes) = fs::read(path) else {
        return DEFAULT_SEGMENT_INTERVAL_SECONDS;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return DEFAULT_SEGMENT_INTERVAL_SECONDS;
    };
    if let Some(interval) = value.get("segment_interval").and_then(|v| v.as_u64())
        && interval > 0
    {
        return interval;
    }
    DEFAULT_SEGMENT_INTERVAL_SECONDS
}

pub const CONFIG_FILENAME: &str = "config.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub capture_interval: Duration,
    pub segment_interval: Duration,
    pub status_indicator: bool,
    pub pane_title: bool,
    pub working_directory: bool,
    pub executable: bool,
    pub output_sessions: Vec<String>,
}

impl RuntimeConfig {
    /// 2.0.28's capture settings from its own `config.json` under `config_root`:
    /// an absent file means the defaults, and a present one is made private
    /// before it is read, as 2.0.28 did with the file it owned. A caller's
    /// settings file goes through [`CaptureSettings::load_file`], which only reads.
    pub fn load(config_root: &Path, _hostname: &str) -> Result<Self, ConfigError> {
        let path = config_root.join(CONFIG_FILENAME);
        let settings = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ConfigError::InvalidTarget(path));
                }
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(
                    |source| ConfigError::Io {
                        operation: "set native config permissions",
                        path: path.clone(),
                        source,
                    },
                )?;
                CaptureSettings::load_file(&path)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                CaptureSettings::default()
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    operation: "inspect native config",
                    path,
                    source,
                });
            }
        };
        Ok(Self {
            capture_interval: Duration::from_secs(settings.capture_interval),
            segment_interval: Duration::from_secs(settings.segment_interval),
            status_indicator: settings.status_indicator,
            pane_title: settings.pane_title,
            working_directory: settings.working_directory,
            executable: settings.executable,
            output_sessions: settings.output_sessions,
        })
    }
}

#[derive(Debug)]
pub enum ConfigError {
    InvalidTarget(PathBuf),
    InvalidConfig {
        path: PathBuf,
        detail: String,
    },
    InvalidInterval {
        field: &'static str,
    },
    InvalidOutputSessions,
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

pub type SettingsError = ConfigError;

impl Clone for ConfigError {
    fn clone(&self) -> Self {
        match self {
            Self::InvalidTarget(path) => Self::InvalidTarget(path.clone()),
            Self::InvalidConfig { path, detail } => Self::InvalidConfig {
                path: path.clone(),
                detail: detail.clone(),
            },
            Self::InvalidInterval { field } => Self::InvalidInterval { field },
            Self::InvalidOutputSessions => Self::InvalidOutputSessions,
            Self::Io {
                operation,
                path,
                source,
            } => Self::Io {
                operation,
                path: path.clone(),
                source: std::io::Error::new(source.kind(), source.to_string()),
            },
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(path) => write!(
                formatter,
                "native config {} must be a regular file, not a symlink or special target",
                path.display()
            ),
            Self::InvalidConfig { path, detail } => {
                write!(
                    formatter,
                    "native config {} is invalid: {detail}",
                    path.display()
                )
            }
            Self::InvalidInterval { field } => {
                write!(
                    formatter,
                    "native config {field} must be greater than zero seconds"
                )
            }
            Self::InvalidOutputSessions => write!(
                formatter,
                "native config output_sessions must list nonempty session names or patterns; update output_sessions in config.json"
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation} failed for {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
