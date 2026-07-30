// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_SERVER_URL: &str = "http://localhost:5015";
pub const DEFAULT_SYNC_STALE_THRESHOLD: i64 = 600;
const DEFAULT_RETRY_DELAYS: [i64; 4] = [5, 30, 120, 300];
const CONFIG_WRITE_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const CONFIG_WRITE_LOCK_POLL: Duration = Duration::from_micros(100);
static CONFIG_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static CONFIG_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub key: String,
    pub stream: String,
    pub segment_interval: i64,
    pub sync_retry_delays: Vec<i64>,
    pub sync_max_retries: i64,
    pub sync_stale_threshold: i64,
    pub cache_retention_days: i64,
    pub chat_bridge_enabled: bool,
    pub capture_framerate: i64,
    pub draw_cursor: bool,
    pub start_paused: bool,
    #[serde(skip)]
    pub base_dir: PathBuf,
    #[serde(skip)]
    pub config_dir: PathBuf,
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn config_dir_for(home: PathBuf, xdg: Option<PathBuf>) -> PathBuf {
    xdg.filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("solstone-linux")
}

impl Default for Config {
    fn default() -> Self {
        let home = home_dir();
        Self {
            server_url: String::new(),
            key: String::new(),
            stream: String::new(),
            segment_interval: 300,
            sync_retry_delays: DEFAULT_RETRY_DELAYS.to_vec(),
            sync_max_retries: 10,
            sync_stale_threshold: DEFAULT_SYNC_STALE_THRESHOLD,
            cache_retention_days: 7,
            chat_bridge_enabled: true,
            capture_framerate: 1,
            draw_cursor: true,
            start_paused: false,
            base_dir: home.join(".local/share/solstone-linux"),
            config_dir: config_dir_for(home, env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)),
        }
    }
}

impl Config {
    pub fn captures_dir(&self) -> PathBuf {
        self.base_dir.join("captures")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.base_dir.join("state")
    }
    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }
    pub fn restore_token_path(&self) -> PathBuf {
        self.config_dir.join("restore_token")
    }
    pub fn ensure_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(self.captures_dir())?;
        fs::create_dir_all(&self.config_dir)?;
        fs::create_dir_all(self.state_dir())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWarning {
    pub field: Option<&'static str>,
    pub message: String,
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<ConfigWarning>,
}

#[derive(Clone, Default)]
pub struct ConfigPaths {
    pub base_dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
}

fn warning(field: &'static str, value: &Value, default: impl std::fmt::Debug) -> ConfigWarning {
    ConfigWarning {
        field: Some(field),
        message: format!("Invalid config value for {field}={value}; using default {default:?}"),
    }
}

fn load_int(
    values: &Map<String, Value>,
    field: &'static str,
    default: i64,
    warnings: &mut Vec<ConfigWarning>,
) -> i64 {
    match values.get(field) {
        None => default,
        // Rust's typed equivalent narrows otherwise-valid Python integers to i64.
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value as i64))
            .unwrap_or(default),
        Some(value) => {
            warnings.push(warning(field, value, default));
            default
        }
    }
}

fn load_int_list(
    values: &Map<String, Value>,
    field: &'static str,
    default: &[i64],
    warnings: &mut Vec<ConfigWarning>,
) -> Vec<i64> {
    let Some(value) = values.get(field) else {
        return default.to_vec();
    };
    let Some(items) = value.as_array() else {
        warnings.push(warning(field, value, default));
        return default.to_vec();
    };
    if !items.iter().all(Value::is_number) {
        warnings.push(warning(field, value, default));
        return default.to_vec();
    }
    items
        .iter()
        .map(|item| {
            item.as_i64()
                .or_else(|| item.as_f64().map(|value| value as i64))
                .unwrap()
        })
        .collect()
}

fn load_string(
    values: &Map<String, Value>,
    field: &'static str,
    warnings: &mut Vec<ConfigWarning>,
) -> String {
    match values.get(field) {
        None => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(value) => {
            // Named deviation: Python retains raw values; Rust warns and preserves the typed string contract.
            warnings.push(warning(field, value, ""));
            String::new()
        }
    }
}

fn json_truthy(value: Option<&Value>, default: bool) -> bool {
    match value {
        None => default,
        Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
    }
}

pub fn load_config(paths: ConfigPaths) -> LoadedConfig {
    let mut config = Config::default();
    if let Some(base_dir) = paths.base_dir {
        config.base_dir = base_dir;
    }
    if let Some(config_dir) = paths.config_dir {
        config.config_dir = config_dir;
    }
    let mut warnings = Vec::new();
    if let Err(error) = migrate(&config) {
        warnings.push(ConfigWarning {
            field: None,
            message: format!("Config migration failed: {error}"),
        });
    }
    let text = match fs::read_to_string(config.config_path()) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return LoadedConfig { config, warnings };
        }
        Err(error) => {
            warnings.push(ConfigWarning {
                field: None,
                message: format!(
                    "Failed to load config from {}: {error}",
                    config.config_path().display()
                ),
            });
            return LoadedConfig { config, warnings };
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            warnings.push(ConfigWarning {
                field: None,
                message: format!(
                    "Failed to load config from {}: {error}",
                    config.config_path().display()
                ),
            });
            return LoadedConfig { config, warnings };
        }
    };
    let Some(values) = value.as_object() else {
        warnings.push(ConfigWarning {
            field: None,
            message: "Config is not a JSON object; using defaults".into(),
        });
        return LoadedConfig { config, warnings };
    };
    config.server_url = load_string(values, "server_url", &mut warnings);
    config.key = load_string(values, "key", &mut warnings);
    config.stream = load_string(values, "stream", &mut warnings);
    config.segment_interval = load_int(values, "segment_interval", 300, &mut warnings);
    config.sync_retry_delays = load_int_list(
        values,
        "sync_retry_delays",
        &DEFAULT_RETRY_DELAYS,
        &mut warnings,
    );
    config.sync_max_retries = load_int(values, "sync_max_retries", 10, &mut warnings);
    config.sync_stale_threshold = load_int(
        values,
        "sync_stale_threshold",
        DEFAULT_SYNC_STALE_THRESHOLD,
        &mut warnings,
    );
    config.cache_retention_days = load_int(values, "cache_retention_days", 7, &mut warnings);
    // Python stores this raw, but all consumers use truthiness, so typed coercion is behaviorally equivalent.
    config.chat_bridge_enabled = json_truthy(values.get("chat_bridge_enabled"), true);
    config.capture_framerate = load_int(values, "capture_framerate", 1, &mut warnings).clamp(1, 10);
    config.draw_cursor = json_truthy(values.get("draw_cursor"), true);
    config.start_paused = json_truthy(values.get("start_paused"), false);
    LoadedConfig { config, warnings }
}

fn acquire_config_write_lock() -> io::Result<MutexGuard<'static, ()>> {
    // The guard protects filesystem syscalls only (read, write, chmod, and rename), never an
    // await or interactive prompt. Normal contention is therefore sub-millisecond; the bounded
    // deadline is a backstop for a stalled writer rather than an expected wait.
    let lock = CONFIG_WRITE_LOCK.get_or_init(|| Mutex::new(()));
    let deadline = Instant::now() + CONFIG_WRITE_LOCK_TIMEOUT;
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(CONFIG_WRITE_LOCK_POLL);
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting to write config",
                ));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("config write lock is poisoned"));
            }
        }
    }
}

fn write_config(config: &Config) -> io::Result<()> {
    config.ensure_dirs()?;
    let path = config.config_path();
    let sequence = CONFIG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), sequence));
    let mut text = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(&temporary, text)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)
}

enum IdentityWrite<'a> {
    PreserveDisk,
    Provided { key: &'a str, stream: &'a str },
}

fn save_config_inner(
    paths: &ConfigPaths,
    source: Option<&Config>,
    identity: IdentityWrite<'_>,
) -> io::Result<()> {
    let _guard = acquire_config_write_lock()?;
    let mut merged = source
        .cloned()
        .unwrap_or_else(|| load_config(paths.clone()).config);
    match identity {
        IdentityWrite::PreserveDisk if merged.config_path().exists() => {
            let disk = load_config(paths.clone()).config;
            merged.key = disk.key;
            merged.stream = disk.stream;
        }
        // On the first write there is no disk identity to preserve, so save_config necessarily
        // writes the caller's identity. Existing configs remain safe-by-default.
        IdentityWrite::PreserveDisk => {}
        IdentityWrite::Provided { key, stream } => {
            merged.key = key.to_owned();
            merged.stream = stream.to_owned();
        }
    }
    write_config(&merged)
}

pub fn save_config(config: &Config) -> io::Result<()> {
    save_config_inner(
        &ConfigPaths {
            base_dir: Some(config.base_dir.clone()),
            config_dir: Some(config.config_dir.clone()),
        },
        Some(config),
        IdentityWrite::PreserveDisk,
    )
}

pub fn save_config_with_identity(config: &Config) -> io::Result<()> {
    save_config_inner(
        &ConfigPaths {
            base_dir: Some(config.base_dir.clone()),
            config_dir: Some(config.config_dir.clone()),
        },
        Some(config),
        IdentityWrite::Provided {
            key: &config.key,
            stream: &config.stream,
        },
    )
}

pub fn save_identity(paths: &ConfigPaths, key: &str, stream: &str) -> io::Result<()> {
    save_config_inner(paths, None, IdentityWrite::Provided { key, stream })
}

fn migrate(config: &Config) -> io::Result<()> {
    let old_dir = config.base_dir.join("config");
    if config.config_dir == old_dir || config.config_path().exists() {
        return Ok(());
    }
    let old_config = old_dir.join("config.json");
    if !old_config.exists() {
        return Ok(());
    }
    fs::create_dir_all(&config.config_dir)?;
    fs::copy(&old_config, config.config_path())?;
    fs::set_permissions(config.config_path(), fs::Permissions::from_mode(0o600))?;
    let old_token = old_dir.join("restore_token");
    if old_token.exists() {
        fs::copy(&old_token, config.restore_token_path())?;
    }
    tracing::info!("Migrated config to {}", config.config_dir.display());
    let _ = fs::remove_file(old_config);
    let _ = fs::remove_file(old_token);
    let _ = fs::remove_dir(old_dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn paths(root: &std::path::Path) -> ConfigPaths {
        ConfigPaths {
            base_dir: Some(root.to_owned()),
            config_dir: Some(root.join("cfg")),
        }
    }
    fn write(root: &std::path::Path, value: Value) {
        let dir = root.join("cfg");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.json"),
            serde_json::to_string(&value).unwrap(),
        )
        .unwrap();
    }
    fn load(root: &std::path::Path) -> LoadedConfig {
        load_config(paths(root))
    }
    fn round_trip(root: &std::path::Path, change: impl FnOnce(&mut Config)) -> Config {
        let mut config = Config {
            base_dir: root.to_owned(),
            config_dir: root.join("cfg"),
            ..Config::default()
        };
        change(&mut config);
        save_config(&config).unwrap();
        load(root).config
    }
    fn warning_fields(loaded: &LoadedConfig) -> Vec<Option<&'static str>> {
        loaded
            .warnings
            .iter()
            .map(|warning| warning.field)
            .collect()
    }

    // tests/test_config.py::test_defaults
    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.server_url, "");
        assert_eq!(c.key, "");
        assert_eq!(c.segment_interval, 300);
    }
    // tests/test_config.py::test_captures_dir
    #[test]
    fn captures_dir() {
        let c = Config::default();
        assert_eq!(c.captures_dir(), c.base_dir.join("captures"));
    }
    // tests/test_config.py::test_restore_token_path
    #[test]
    fn restore_token_path() {
        let c = Config::default();
        assert_eq!(c.restore_token_path(), c.config_dir.join("restore_token"));
    }
    // tests/test_config.py::test_config_dir_uses_absolute_xdg
    #[test]
    fn absolute_xdg() {
        assert_eq!(
            config_dir_for("/home/u".into(), Some("/tmp/x".into())),
            PathBuf::from("/tmp/x/solstone-linux")
        );
    }
    // tests/test_config.py::test_config_dir_ignores_relative_xdg
    #[test]
    fn relative_xdg() {
        assert_eq!(
            config_dir_for("/home/u".into(), Some("relative/path".into())),
            PathBuf::from("/home/u/.config/solstone-linux")
        );
    }
    // tests/test_config.py::test_config_dir_falls_back_when_xdg_unset
    #[test]
    fn unset_xdg() {
        assert_eq!(
            config_dir_for("/home/u".into(), None),
            PathBuf::from("/home/u/.config/solstone-linux")
        );
    }
    // tests/test_config.py::test_round_trip
    #[test]
    fn round_trip_core() {
        let t = tempfile::tempdir().unwrap();
        let c = round_trip(t.path(), |c| {
            c.server_url = "https://example.com".into();
            c.key = "key".into();
            c.stream = "archon".into();
            c.segment_interval = 600
        });
        assert_eq!(
            (c.server_url, c.key, c.stream, c.segment_interval),
            (
                "https://example.com".into(),
                "key".into(),
                "archon".into(),
                600
            )
        );
    }
    // tests/test_config.py::test_load_missing
    #[test]
    fn load_missing() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(load(t.path()).config.server_url, "");
    }
    // tests/test_config.py::test_load_corrupt
    #[test]
    fn load_corrupt() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("cfg");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("config.json"), "not json!").unwrap();
        let x = load(t.path());
        assert_eq!(x.config.server_url, "");
        assert_eq!(x.warnings.len(), 1);
        assert!(
            x.warnings[0]
                .message
                .contains(&d.join("config.json").display().to_string())
        );
    }
    macro_rules! invalid {($name:ident,$field:literal,$value:expr,$expected:expr)=>{
        #[test]fn $name(){let t=tempfile::tempdir().unwrap();write(t.path(),json!({$field:$value}));let x=load(t.path());assert_eq!(warning_fields(&x),vec![Some($field)]);assert_eq!(serde_json::to_value(&x.config).unwrap()[$field],json!($expected));}}}
    // tests/test_config.py::test_load_invalid_typed_fields_warn_and_default[capture_framerate]
    invalid!(invalid_framerate, "capture_framerate", "abc", 1);
    // tests/test_config.py::test_load_invalid_typed_fields_warn_and_default[segment_interval]
    invalid!(invalid_interval, "segment_interval", "300", 300);
    // tests/test_config.py::test_load_invalid_typed_fields_warn_and_default[sync_retry_delays]
    invalid!(
        invalid_retry_delays,
        "sync_retry_delays",
        "oops",
        [5, 30, 120, 300]
    );
    // tests/test_config.py::test_load_invalid_typed_fields_warn_and_default[sync_max_retries]
    invalid!(invalid_max_retries, "sync_max_retries", "many", 10);
    // tests/test_config.py::test_load_invalid_typed_fields_warn_and_default[cache_retention_days]
    invalid!(invalid_retention, "cache_retention_days", json!([]), 7);
    // tests/test_config.py::test_load_non_object_json_warns_and_defaults
    #[test]
    fn non_object() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!([]));
        let x = load(t.path());
        assert_eq!(x.warnings.len(), 1);
        assert_eq!(
            x.config,
            Config {
                base_dir: t.path().into(),
                config_dir: t.path().join("cfg"),
                ..Config::default()
            }
        );
    }
    // tests/test_config.py::test_permissions
    #[test]
    fn permissions() {
        let t = tempfile::tempdir().unwrap();
        round_trip(t.path(), |_| {});
        assert_eq!(
            fs::metadata(t.path().join("cfg/config.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    // tests/test_config.py::test_sync_config_roundtrip
    #[test]
    fn sync_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let c = round_trip(t.path(), |c| {
            c.sync_retry_delays = vec![10, 60, 300];
            c.sync_max_retries = 5
        });
        assert_eq!(c.sync_retry_delays, vec![10, 60, 300]);
        assert_eq!(c.sync_max_retries, 5);
    }
    // tests/test_config.py::test_cache_retention_days_roundtrip
    #[test]
    fn retention_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            round_trip(t.path(), |c| c.cache_retention_days = 14).cache_retention_days,
            14
        );
    }
    // tests/test_config.py::test_cache_retention_days_default
    #[test]
    fn retention_default() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"server_url":"http://test"}));
        assert_eq!(load(t.path()).config.cache_retention_days, 7);
    }
    // tests/test_config.py::test_capture_framerate_default
    #[test]
    fn framerate_default() {
        assert_eq!(Config::default().capture_framerate, 1);
    }
    // tests/test_config.py::test_draw_cursor_default
    #[test]
    fn cursor_default() {
        assert!(Config::default().draw_cursor);
    }
    // tests/test_config.py::test_capture_framerate_roundtrip
    #[test]
    fn framerate_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            round_trip(t.path(), |c| c.capture_framerate = 2).capture_framerate,
            2
        );
    }
    // tests/test_config.py::test_draw_cursor_roundtrip
    #[test]
    fn cursor_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        assert!(!round_trip(t.path(), |c| c.draw_cursor = false).draw_cursor);
    }
    // tests/test_config.py::test_capture_framerate_defaults_on_old_config
    #[test]
    fn old_framerate_defaults() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"server_url":"http://test"}));
        let c = load(t.path()).config;
        assert_eq!(c.capture_framerate, 1);
        assert!(c.draw_cursor);
    }
    // tests/test_config.py::test_capture_framerate_clamped_to_max
    #[test]
    fn framerate_max() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"capture_framerate":999}));
        assert_eq!(load(t.path()).config.capture_framerate, 10);
    }
    // tests/test_config.py::test_capture_framerate_clamped_to_min
    #[test]
    fn framerate_min() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"capture_framerate":0}));
        assert_eq!(load(t.path()).config.capture_framerate, 1);
    }
    // tests/test_config.py::test_start_paused_default
    #[test]
    fn paused_default() {
        assert!(!Config::default().start_paused);
    }
    // tests/test_config.py::test_start_paused_roundtrip
    #[test]
    fn paused_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        assert!(round_trip(t.path(), |c| c.start_paused = true).start_paused);
    }
    // tests/test_config.py::test_start_paused_defaults_on_old_config
    #[test]
    fn old_paused_default() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"server_url":"http://test"}));
        assert!(!load(t.path()).config.start_paused);
    }
    // tests/test_config.py::test_migrates_legacy_config
    #[test]
    fn migrates_legacy() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("config");
        fs::create_dir(&old).unwrap();
        fs::write(
            old.join("config.json"),
            r#"{"server_url":"https://example.com"}"#,
        )
        .unwrap();
        fs::write(old.join("restore_token"), "tok").unwrap();
        let x = load_config(ConfigPaths {
            base_dir: Some(t.path().into()),
            config_dir: Some(t.path().join("new")),
        });
        assert_eq!(x.config.server_url, "https://example.com");
        assert_eq!(
            fs::read_to_string(t.path().join("new/restore_token")).unwrap(),
            "tok"
        );
        assert!(!old.exists());
    }
    // tests/test_config.py::test_no_migration_when_config_dir_is_legacy
    #[test]
    fn no_same_dir_migration() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("config");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("config.json"), r#"{"capture_framerate":4}"#).unwrap();
        let x = load_config(ConfigPaths {
            base_dir: Some(t.path().into()),
            config_dir: Some(old.clone()),
        });
        assert_eq!(x.config.capture_framerate, 4);
        assert!(old.exists());
    }
    // AC: all twelve persisted defaults.
    #[test]
    fn all_defaults() {
        let c = Config::default();
        assert_eq!(
            serde_json::to_value(c).unwrap(),
            json!({"server_url":"","key":"","stream":"","segment_interval":300,"sync_retry_delays":[5,30,120,300],"sync_max_retries":10,"sync_stale_threshold":600,"cache_retention_days":7,"chat_bridge_enabled":true,"capture_framerate":1,"draw_cursor":true,"start_paused":false})
        );
    }
    // AC: numeric coercion rejects bool and truncates floats, including list elements.
    #[test]
    fn numeric_coercion() {
        let t = tempfile::tempdir().unwrap();
        write(
            t.path(),
            json!({"segment_interval":3.9,"sync_max_retries":true,"sync_retry_delays":[1.9,2]}),
        );
        let x = load(t.path());
        assert_eq!(x.config.segment_interval, 3);
        assert_eq!(x.config.sync_retry_delays, vec![1, 2]);
        assert_eq!(x.config.sync_max_retries, 10);
        assert_eq!(warning_fields(&x), vec![Some("sync_max_retries")]);
    }
    // AC: Python JSON truthiness mapping.
    #[test]
    fn truthiness() {
        for (value, expected) in [
            (json!(false), false),
            (Value::Null, false),
            (json!(0), false),
            (json!(0.0), false),
            (json!(-0.0), false),
            (json!(""), false),
            (json!([]), false),
            (json!({}), false),
            (json!("false"), true),
            (json!(1), true),
        ] {
            assert_eq!(json_truthy(Some(&value), true), expected);
        }
        assert!(json_truthy(None, true));
    }
    // AC: draw_cursor uses truthiness, not string parsing.
    #[test]
    fn cursor_truthiness() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"draw_cursor":"false"}));
        assert!(load(t.path()).config.draw_cursor);
        write(t.path(), json!({"draw_cursor":0}));
        assert!(!load(t.path()).config.draw_cursor);
    }
    // AC: typed string deviation warns once and defaults empty.
    #[test]
    fn non_string_field() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"server_url":7}));
        let x = load(t.path());
        assert_eq!(x.config.server_url, "");
        assert_eq!(warning_fields(&x), vec![Some("server_url")]);
        assert!(x.warnings[0].message.contains("server_url=7"));
    }
    // AC: save schema is exact and unknown keys are dropped.
    #[test]
    fn exact_keys_and_unknown_drop() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"unknown":1}));
        let c = load(t.path()).config;
        save_config(&c).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(c.config_path()).unwrap()).unwrap();
        let keys: BTreeSet<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "server_url",
                "key",
                "stream",
                "segment_interval",
                "sync_retry_delays",
                "sync_max_retries",
                "sync_stale_threshold",
                "cache_retention_days",
                "chat_bridge_enabled",
                "capture_framerate",
                "draw_cursor",
                "start_paused"
            ])
        );
    }
    // AC: temp suffix replaces .json and no temp remains after atomic rename.
    #[test]
    fn atomic_temp_name() {
        let t = tempfile::tempdir().unwrap();
        let c = round_trip(t.path(), |_| {});
        assert!(c.config_path().exists());
        assert!(
            !c.config_path()
                .with_extension(format!("{}.tmp", std::process::id()))
                .exists()
        );
    }

    // AC 11/17: identity-only recovery and a stale whole-config writer preserve both change sets.
    #[test]
    fn stale_settings_snapshot_preserves_recovered_identity() {
        let t = tempfile::tempdir().unwrap();
        let initial = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("cfg"),
            server_url: "https://journal".into(),
            cache_retention_days: 7,
            ..Config::default()
        };
        save_config(&initial).unwrap();
        let config_paths = paths(t.path());
        save_identity(&config_paths, "STALE-KEY", "desktop-old").unwrap();
        let mut stale_settings = load_config(config_paths.clone()).config;

        save_identity(&config_paths, "NEW-KEY", "desktop-new").unwrap();
        stale_settings.cache_retention_days = 30;
        save_config(&stale_settings).unwrap();

        let saved = load_config(config_paths).config;
        assert_eq!(saved.key, "NEW-KEY");
        assert_eq!(saved.stream, "desktop-new");
        assert_eq!(saved.cache_retention_days, 30);
    }

    // AC 12: recovery writes identity only and cannot revert newer non-identity disk state.
    #[test]
    fn save_identity_preserves_newer_server_url() {
        let t = tempfile::tempdir().unwrap();
        let config_paths = paths(t.path());
        let mut config = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("cfg"),
            server_url: "https://old".into(),
            ..Config::default()
        };
        save_config(&config).unwrap();
        save_identity(&config_paths, "STALE-KEY", "desktop").unwrap();
        config = load_config(config_paths.clone()).config;
        config.server_url = "https://new".into();
        save_config(&config).unwrap();

        save_identity(&config_paths, "NEW-KEY", "desktop-new").unwrap();
        let saved = load_config(config_paths).config;
        assert_eq!(saved.server_url, "https://new");
        assert_eq!(saved.key, "NEW-KEY");
        assert_eq!(saved.stream, "desktop-new");
    }

    // AC: migration failure is returned as one warning and leaves legacy data intact.
    #[test]
    fn migration_warning() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("config");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("config.json"), "{}").unwrap();
        let blocker = t.path().join("new");
        fs::write(&blocker, "file").unwrap();
        let x = load_config(ConfigPaths {
            base_dir: Some(t.path().into()),
            config_dir: Some(blocker),
        });
        assert!(
            x.warnings
                .iter()
                .any(|warning| warning.message.starts_with("Config migration failed:"))
        );
        assert!(x.warnings.iter().all(|warning| warning.field.is_none()));
        assert!(old.join("config.json").exists());
    }
    // AC: a second migration load is byte- and mode-stable.
    #[test]
    fn migration_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("config");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("config.json"), "{}\n").unwrap();
        let paths = || ConfigPaths {
            base_dir: Some(t.path().into()),
            config_dir: Some(t.path().join("new")),
        };
        let first = load_config(paths());
        let bytes = fs::read(first.config.config_path()).unwrap();
        let mode = fs::metadata(first.config.config_path())
            .unwrap()
            .permissions()
            .mode();
        let second = load_config(paths());
        assert!(second.warnings.is_empty());
        assert_eq!(fs::read(second.config.config_path()).unwrap(), bytes);
        assert_eq!(
            fs::metadata(second.config.config_path())
                .unwrap()
                .permissions()
                .mode(),
            mode
        );
    }
}
