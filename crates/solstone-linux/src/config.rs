// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Component, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    thread,
    time::{Duration, Instant},
};

use crate::private_file::{
    DurableWriteFault, NoWriteFault, PrivateFileError, atomic_write_bytes_guarded,
    ensure_private_directory, open_regular_readonly,
};

pub const DEFAULT_SYNC_STALE_THRESHOLD: i64 = 600;
const DEFAULT_RETRY_DELAYS: [i64; 4] = [5, 30, 120, 300];
const CONFIG_WRITE_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const CONFIG_WRITE_LOCK_POLL: Duration = Duration::from_micros(100);
static CONFIG_WRITE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
pub(crate) const RETIRED_CONFIG_KEYS: [&str; 3] = ["server_url", "key", "chat_bridge_enabled"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub stream: String,
    pub segment_interval: i64,
    pub sync_retry_delays: Vec<i64>,
    pub sync_max_retries: i64,
    pub sync_stale_threshold: i64,
    pub cache_retention_days: i64,
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
            stream: String::new(),
            segment_interval: 300,
            sync_retry_delays: DEFAULT_RETRY_DELAYS.to_vec(),
            sync_max_retries: 10,
            sync_stale_threshold: DEFAULT_SYNC_STALE_THRESHOLD,
            cache_retention_days: 7,
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

fn resolve_config_paths(paths: &ConfigPaths) -> Config {
    let mut config = Config::default();
    if let Some(base_dir) = &paths.base_dir {
        config.base_dir = base_dir.clone();
    }
    if let Some(config_dir) = &paths.config_dir {
        config.config_dir = config_dir.clone();
    }
    config
}

pub fn load_config(paths: ConfigPaths) -> LoadedConfig {
    load_resolved_config(resolve_config_paths(&paths))
}

fn load_resolved_config(mut config: Config) -> LoadedConfig {
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
    let mut values = values.clone();
    for legacy in RETIRED_CONFIG_KEYS {
        values.remove(legacy);
    }
    let values = &values;
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
    config.capture_framerate = load_int(values, "capture_framerate", 1, &mut warnings).clamp(1, 10);
    config.draw_cursor = json_truthy(values.get("draw_cursor"), true);
    config.start_paused = json_truthy(values.get("start_paused"), false);
    LoadedConfig { config, warnings }
}

fn config_write_lock_key(config: &Config) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in config.config_path().components() {
        if component != Component::CurDir {
            normalized.push(component.as_os_str());
        }
    }
    normalized
}

fn config_write_lock(config: &Config) -> io::Result<Arc<Mutex<()>>> {
    let locks = CONFIG_WRITE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| io::Error::other("config write lock registry is poisoned"))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    let key = config_write_lock_key(config);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

fn acquire_config_write_lock(lock: &Mutex<()>) -> io::Result<MutexGuard<'_, ()>> {
    // The per-destination guard protects filesystem syscalls only (read, write, chmod, and
    // rename), never an await or interactive prompt. Independent destinations do not contend;
    // normal same-destination contention is therefore sub-millisecond, and the bounded deadline
    // remains a backstop for a stalled writer rather than an expected wait.
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

enum ConfigOverlay<'a> {
    Settings(&'a Config),
    Stream(&'a str),
    Sanitize,
}

fn write_config_merge(
    paths: &ConfigPaths,
    overlay: ConfigOverlay<'_>,
    fault: &dyn DurableWriteFault,
) -> io::Result<Config> {
    let resolved = resolve_config_paths(paths);
    let lock = config_write_lock(&resolved)?;
    let _guard = acquire_config_write_lock(&lock)?;
    let path = resolved.config_path();
    ensure_private_directory(&resolved.config_dir).map_err(io::Error::other)?;

    let (merge_basis, mut map) = match open_regular_readonly(&path) {
        Ok(mut file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let Some(map) = value.as_object() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Config is not a JSON object",
                ));
            };
            (Some(bytes), map.clone())
        }
        Err(PrivateFileError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => (None, Map::new()),
        Err(err) => {
            return Err(io::Error::other(err));
        }
    };

    for retired in RETIRED_CONFIG_KEYS {
        map.remove(retired);
    }

    let mut warnings = Vec::new();
    let mut stream = if merge_basis.is_some() {
        load_string(&map, "stream", &mut warnings)
    } else {
        String::new()
    };
    let mut segment_interval = if merge_basis.is_some() {
        load_int(&map, "segment_interval", 300, &mut warnings)
    } else {
        300
    };
    let mut sync_retry_delays = if merge_basis.is_some() {
        load_int_list(
            &map,
            "sync_retry_delays",
            &DEFAULT_RETRY_DELAYS,
            &mut warnings,
        )
    } else {
        DEFAULT_RETRY_DELAYS.to_vec()
    };
    let mut sync_max_retries = if merge_basis.is_some() {
        load_int(&map, "sync_max_retries", 10, &mut warnings)
    } else {
        10
    };
    let mut sync_stale_threshold = if merge_basis.is_some() {
        load_int(
            &map,
            "sync_stale_threshold",
            DEFAULT_SYNC_STALE_THRESHOLD,
            &mut warnings,
        )
    } else {
        DEFAULT_SYNC_STALE_THRESHOLD
    };
    let mut cache_retention_days = if merge_basis.is_some() {
        load_int(&map, "cache_retention_days", 7, &mut warnings)
    } else {
        7
    };
    let mut capture_framerate = if merge_basis.is_some() {
        load_int(&map, "capture_framerate", 1, &mut warnings).clamp(1, 10)
    } else {
        1
    };
    let mut draw_cursor = if merge_basis.is_some() {
        json_truthy(map.get("draw_cursor"), true)
    } else {
        true
    };
    let mut start_paused = if merge_basis.is_some() {
        json_truthy(map.get("start_paused"), false)
    } else {
        false
    };

    match overlay {
        ConfigOverlay::Settings(snapshot) => {
            capture_framerate = snapshot.capture_framerate.clamp(1, 10);
            draw_cursor = snapshot.draw_cursor;
            start_paused = snapshot.start_paused;
            segment_interval = snapshot.segment_interval;
            cache_retention_days = snapshot.cache_retention_days;
            if merge_basis.is_none() {
                stream = snapshot.stream.clone();
                sync_retry_delays = snapshot.sync_retry_delays.clone();
                sync_max_retries = snapshot.sync_max_retries;
                sync_stale_threshold = snapshot.sync_stale_threshold;
            }
        }
        ConfigOverlay::Stream(new_stream) => {
            stream = new_stream.to_owned();
        }
        ConfigOverlay::Sanitize => {}
    }

    let returned_config = Config {
        stream: stream.clone(),
        segment_interval,
        sync_retry_delays: sync_retry_delays.clone(),
        sync_max_retries,
        sync_stale_threshold,
        cache_retention_days,
        capture_framerate,
        draw_cursor,
        start_paused,
        base_dir: resolved.base_dir.clone(),
        config_dir: resolved.config_dir.clone(),
    };

    let mut bytes = if merge_basis.is_none() {
        serde_json::to_vec_pretty(&returned_config).map_err(io::Error::other)?
    } else {
        map.insert("stream".into(), Value::String(stream));
        map.insert(
            "segment_interval".into(),
            Value::Number(segment_interval.into()),
        );
        map.insert(
            "sync_retry_delays".into(),
            Value::Array(sync_retry_delays.into_iter().map(Value::from).collect()),
        );
        map.insert(
            "sync_max_retries".into(),
            Value::Number(sync_max_retries.into()),
        );
        map.insert(
            "sync_stale_threshold".into(),
            Value::Number(sync_stale_threshold.into()),
        );
        map.insert(
            "cache_retention_days".into(),
            Value::Number(cache_retention_days.into()),
        );
        map.insert(
            "capture_framerate".into(),
            Value::Number(capture_framerate.into()),
        );
        map.insert("draw_cursor".into(), Value::Bool(draw_cursor));
        map.insert("start_paused".into(), Value::Bool(start_paused));
        serde_json::to_vec_pretty(&map).map_err(io::Error::other)?
    };
    bytes.push(b'\n');

    let is_current = || match &merge_basis {
        None => matches!(
            open_regular_readonly(&path),
            Err(PrivateFileError::Io {
                kind: io::ErrorKind::NotFound,
                ..
            })
        ),
        Some(basis_bytes) => match open_regular_readonly(&path) {
            Ok(mut file) => {
                use std::io::Read;
                let mut current = Vec::new();
                file.read_to_end(&mut current).is_ok() && current == *basis_bytes
            }
            _ => false,
        },
    };

    atomic_write_bytes_guarded(&path, &bytes, fault, &is_current).map_err(io::Error::other)?;

    Ok(returned_config)
}

pub(crate) fn sanitize_link_authority(paths: &ConfigPaths) -> io::Result<Config> {
    sanitize_link_authority_with_fault(paths, &NoWriteFault)
}

pub(crate) fn sanitize_link_authority_with_fault(
    paths: &ConfigPaths,
    fault: &dyn DurableWriteFault,
) -> io::Result<Config> {
    write_config_merge(paths, ConfigOverlay::Sanitize, fault)
}

pub(crate) fn save_linked_stream(paths: &ConfigPaths, stream: &str) -> io::Result<Config> {
    save_linked_stream_with_fault(paths, stream, &NoWriteFault)
}

pub(crate) fn save_linked_stream_with_fault(
    paths: &ConfigPaths,
    stream: &str,
    fault: &dyn DurableWriteFault,
) -> io::Result<Config> {
    write_config_merge(paths, ConfigOverlay::Stream(stream), fault)
}

pub(crate) fn save_config_with_fault(
    config: &Config,
    fault: &dyn DurableWriteFault,
) -> io::Result<()> {
    write_config_merge(
        &ConfigPaths {
            base_dir: Some(config.base_dir.clone()),
            config_dir: Some(config.config_dir.clone()),
        },
        ConfigOverlay::Settings(config),
        fault,
    )
    .map(|_| ())
}

pub fn save_config(config: &Config) -> io::Result<()> {
    save_config_with_fault(config, &NoWriteFault)
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
    use crate::private_file::DurableWriteStage;
    use serde_json::json;
    use std::{
        os::unix::fs::{MetadataExt, symlink},
        process::Command,
        thread,
        time::Duration,
    };

    struct FailStage(DurableWriteStage);

    impl DurableWriteFault for FailStage {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            if stage == self.0 {
                Err(io::Error::other("injected durable write failure"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ConfigWriteFlow {
        Settings,
        Linked,
    }

    #[derive(Clone, Copy)]
    enum SettingsField {
        SegmentInterval,
        CacheRetentionDays,
    }

    #[derive(Clone, Copy)]
    struct ConfigWriteValue {
        stream: &'static str,
        field: SettingsField,
        setting: i64,
    }

    const FIRST_WRITE: ConfigWriteValue = ConfigWriteValue {
        stream: "linked-first",
        field: SettingsField::SegmentInterval,
        setting: 111,
    };
    const SECOND_WRITE: ConfigWriteValue = ConfigWriteValue {
        stream: "linked-second",
        field: SettingsField::CacheRetentionDays,
        setting: 22,
    };

    #[derive(Clone, Copy)]
    enum DestinationRelation {
        Distinct,
        SameLexicalAlias,
    }

    fn config_paths(base_dir: PathBuf, config_dir: PathBuf) -> ConfigPaths {
        ConfigPaths {
            base_dir: Some(base_dir),
            config_dir: Some(config_dir),
        }
    }

    fn run_config_write_flow(
        flow: ConfigWriteFlow,
        paths: &ConfigPaths,
        value: ConfigWriteValue,
    ) -> io::Result<()> {
        match flow {
            ConfigWriteFlow::Settings => {
                let mut config = load_config(paths.clone()).config;
                match value.field {
                    SettingsField::SegmentInterval => config.segment_interval = value.setting,
                    SettingsField::CacheRetentionDays => {
                        config.cache_retention_days = value.setting;
                    }
                }
                save_config(&config)
            }
            ConfigWriteFlow::Linked => save_linked_stream(paths, value.stream).map(drop),
        }
    }

    fn assert_config_write_value(flow: ConfigWriteFlow, value: ConfigWriteValue, config: &Config) {
        match flow {
            ConfigWriteFlow::Settings => match value.field {
                SettingsField::SegmentInterval => {
                    assert_eq!(config.segment_interval, value.setting);
                }
                SettingsField::CacheRetentionDays => {
                    assert_eq!(config.cache_retention_days, value.setting);
                }
            },
            ConfigWriteFlow::Linked => assert_eq!(config.stream, value.stream),
        }
    }

    fn assert_config_write_timeout(result: io::Result<()>) {
        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "timed out waiting to write config");
    }

    fn assert_config_write_admission_case(
        relation: DestinationRelation,
        first: ConfigWriteFlow,
        second: ConfigWriteFlow,
        mut run: impl FnMut(ConfigWriteFlow, &ConfigPaths, ConfigWriteValue) -> io::Result<()>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let first_dir = temp.path().join("first-config");
        let second_dir = match relation {
            DestinationRelation::Distinct => temp.path().join("second-config"),
            DestinationRelation::SameLexicalAlias => first_dir.join("."),
        };
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let first_paths = config_paths(temp.path().join("first-data"), first_dir.clone());
        let second_paths = config_paths(temp.path().join("second-data"), second_dir.clone());

        if matches!(relation, DestinationRelation::SameLexicalAlias) {
            let first_metadata = fs::metadata(&first_dir).unwrap();
            let second_metadata = fs::metadata(&second_dir).unwrap();
            assert_eq!(
                (first_metadata.dev(), first_metadata.ino()),
                (second_metadata.dev(), second_metadata.ino())
            );
            fs::write(first_dir.join("config.json"), b"{\"stream\":\"seed\"}\n").unwrap();
        }

        let resolved = resolve_config_paths(&first_paths);
        let lock = config_write_lock(&resolved).unwrap();
        let guard = acquire_config_write_lock(&lock).unwrap();
        let blocked = run(second, &second_paths, SECOND_WRITE);
        match relation {
            DestinationRelation::Distinct => {
                blocked.unwrap();
                assert_config_write_value(
                    second,
                    SECOND_WRITE,
                    &load_config(second_paths.clone()).config,
                );
            }
            DestinationRelation::SameLexicalAlias => {
                assert_config_write_timeout(blocked);
                assert_eq!(
                    fs::read(first_dir.join("config.json")).unwrap(),
                    b"{\"stream\":\"seed\"}\n"
                );
            }
        }
        drop(guard);

        run(first, &first_paths, FIRST_WRITE).unwrap();
        run(second, &second_paths, SECOND_WRITE).unwrap();

        match relation {
            DestinationRelation::Distinct => {
                assert_config_write_value(first, FIRST_WRITE, &load_config(first_paths).config);
                assert_config_write_value(second, SECOND_WRITE, &load_config(second_paths).config);
            }
            DestinationRelation::SameLexicalAlias => {
                let saved = load_config(first_paths).config;
                assert_config_write_value(second, SECOND_WRITE, &saved);
                // Both linked writes set `stream`, so the later write legitimately wins.
                if !(first == ConfigWriteFlow::Linked && second == ConfigWriteFlow::Linked) {
                    assert_config_write_value(first, FIRST_WRITE, &saved);
                }
            }
        }
    }

    fn for_each_config_write_pair(mut check: impl FnMut(ConfigWriteFlow, ConfigWriteFlow)) {
        for (first, second) in [
            (ConfigWriteFlow::Settings, ConfigWriteFlow::Linked),
            (ConfigWriteFlow::Linked, ConfigWriteFlow::Settings),
            (ConfigWriteFlow::Settings, ConfigWriteFlow::Settings),
            (ConfigWriteFlow::Linked, ConfigWriteFlow::Linked),
        ] {
            check(first, second);
        }
    }

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
        assert_eq!(c.stream, "");
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
            c.stream = "archon".into();
            c.segment_interval = 600
        });
        assert_eq!((c.stream, c.segment_interval), ("archon".into(), 600));
    }
    // tests/test_config.py::test_load_missing
    #[test]
    fn load_missing() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            load(t.path()).config,
            Config {
                base_dir: t.path().into(),
                config_dir: t.path().join("cfg"),
                ..Config::default()
            }
        );
    }
    // tests/test_config.py::test_load_corrupt
    #[test]
    fn load_corrupt() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("cfg");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("config.json"), "not json!").unwrap();
        let x = load(t.path());
        assert_eq!(x.config.stream, "");
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
        write(t.path(), json!({"stream":"old"}));
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
        write(t.path(), json!({"stream":"old"}));
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
        write(t.path(), json!({"stream":"old"}));
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
            r#"{"server_url":"https://sentinel.invalid","stream":"desktop"}"#,
        )
        .unwrap();
        fs::write(old.join("restore_token"), "tok").unwrap();
        let x = load_config(ConfigPaths {
            base_dir: Some(t.path().into()),
            config_dir: Some(t.path().join("new")),
        });
        assert_eq!(x.config.stream, "desktop");
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
    // AC: all nine persisted defaults.
    #[test]
    fn all_defaults() {
        let c = Config::default();
        assert_eq!(
            serde_json::to_value(c).unwrap(),
            json!({"stream":"","segment_interval":300,"sync_retry_delays":[5,30,120,300],"sync_max_retries":10,"sync_stale_threshold":600,"cache_retention_days":7,"capture_framerate":1,"draw_cursor":true,"start_paused":false})
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
        write(t.path(), json!({"stream":7}));
        let x = load(t.path());
        assert_eq!(x.config.stream, "");
        assert_eq!(warning_fields(&x), vec![Some("stream")]);
        assert!(x.warnings[0].message.contains("stream=7"));
    }
    // AC: unknown top-level keys round-trip as equal serde_json::Value through all writers.
    #[test]
    fn unknown_keys_preserved_through_all_writers() {
        let nested_value = json!({
            "custom_scalar": "some-value",
            "custom_number": 42,
            "custom_bool": true,
            "custom_null": null,
            "custom_array": [1, "two", {"three": 3}],
            "custom_map": {
                "nested_a": "alpha",
                "nested_b": [true, false]
            }
        });

        // 1. save_config
        let t1 = tempfile::tempdir().unwrap();
        let mut initial_map1 = nested_value.as_object().unwrap().clone();
        initial_map1.insert("stream".into(), json!("initial-stream"));
        write(t1.path(), Value::Object(initial_map1));
        let mut c1 = load(t1.path()).config;
        c1.cache_retention_days = 14;
        save_config(&c1).unwrap();
        let v1: Value =
            serde_json::from_str(&fs::read_to_string(c1.config_path()).unwrap()).unwrap();
        assert_eq!(v1["cache_retention_days"], 14);
        assert_eq!(v1["stream"], "initial-stream");
        for (k, v) in nested_value.as_object().unwrap() {
            assert_eq!(&v1[k], v, "save_config preserved key {k}");
        }

        // 2. save_linked_stream
        let t2 = tempfile::tempdir().unwrap();
        let mut initial_map2 = nested_value.as_object().unwrap().clone();
        initial_map2.insert("segment_interval".into(), json!(600));
        write(t2.path(), Value::Object(initial_map2));
        let paths2 = paths(t2.path());
        save_linked_stream(&paths2, "linked-stream").unwrap();
        let v2: Value =
            serde_json::from_str(&fs::read_to_string(t2.path().join("cfg/config.json")).unwrap())
                .unwrap();
        assert_eq!(v2["stream"], "linked-stream");
        assert_eq!(v2["segment_interval"], 600);
        for (k, v) in nested_value.as_object().unwrap() {
            assert_eq!(&v2[k], v, "save_linked_stream preserved key {k}");
        }

        // 3. sanitize_link_authority
        let t3 = tempfile::tempdir().unwrap();
        let mut initial_map3 = nested_value.as_object().unwrap().clone();
        initial_map3.insert("server_url".into(), json!("https://legacy.test"));
        initial_map3.insert("key".into(), json!("secret-key"));
        initial_map3.insert("chat_bridge_enabled".into(), json!(true));
        write(t3.path(), Value::Object(initial_map3));
        let paths3 = paths(t3.path());
        let res3 = sanitize_link_authority(&paths3).unwrap();
        let v3: Value =
            serde_json::from_str(&fs::read_to_string(res3.config_path()).unwrap()).unwrap();
        assert!(v3.get("server_url").is_none());
        assert!(v3.get("key").is_none());
        assert!(v3.get("chat_bridge_enabled").is_none());
        for (k, v) in nested_value.as_object().unwrap() {
            assert_eq!(&v3[k], v, "sanitize_link_authority preserved key {k}");
        }
    }

    // AC: migrate preserves arbitrary unknown content byte-for-byte.
    #[test]
    fn migration_preserves_unknown_bytes_verbatim() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("config");
        fs::create_dir(&old).unwrap();
        let legacy_bytes =
            b"{\n  \"unknown_sentinel\": \"PRESERVE-ME\",\n  \"nested\": [1, 2, 3]\n}\n";
        fs::write(old.join("config.json"), legacy_bytes).unwrap();
        let paths = ConfigPaths {
            base_dir: Some(t.path().into()),
            config_dir: Some(t.path().join("cfg")),
        };
        let loaded = load_config(paths);
        let dest_bytes = fs::read(loaded.config.config_path()).unwrap();
        assert_eq!(dest_bytes, legacy_bytes);
    }

    // AC: settings save overlays only prompted fields and preserves concurrent valid disk fields & unknowns.
    #[test]
    fn settings_merge_preserves_unprompted_fields_and_unknowns() {
        let t = tempfile::tempdir().unwrap();
        let config_paths = paths(t.path());
        let initial = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("cfg"),
            cache_retention_days: 7,
            segment_interval: 300,
            ..Config::default()
        };
        save_config(&initial).unwrap();
        save_linked_stream(&config_paths, "desktop-stream-1").unwrap();

        // Stale snapshot captured by CLI settings prompt
        let mut snapshot = load_config(config_paths.clone()).config;

        // Concurrent background or external edit on disk
        let disk_path = t.path().join("cfg/config.json");
        let mut disk_val: Value =
            serde_json::from_str(&fs::read_to_string(&disk_path).unwrap()).unwrap();
        disk_val["stream"] = json!("desktop-stream-2");
        disk_val["sync_retry_delays"] = json!([10, 20]);
        disk_val["sync_max_retries"] = json!(25);
        disk_val["sync_stale_threshold"] = json!(1200);
        disk_val["custom_plugin_field"] = json!({"active": true});
        fs::write(&disk_path, serde_json::to_string_pretty(&disk_val).unwrap()).unwrap();

        // Snapshot applies 5 prompted fields:
        snapshot.capture_framerate = 5;
        snapshot.draw_cursor = false;
        snapshot.start_paused = true;
        snapshot.segment_interval = 600;
        snapshot.cache_retention_days = 30;

        // Stale snapshot has old stream "desktop-stream-1", old delays, etc.
        save_config(&snapshot).unwrap();

        let saved_val: Value =
            serde_json::from_str(&fs::read_to_string(&disk_path).unwrap()).unwrap();
        // 5 prompted fields win
        assert_eq!(saved_val["capture_framerate"], 5);
        assert_eq!(saved_val["draw_cursor"], false);
        assert_eq!(saved_val["start_paused"], true);
        assert_eq!(saved_val["segment_interval"], 600);
        assert_eq!(saved_val["cache_retention_days"], 30);

        // Disk unprompted fields & unknowns win
        assert_eq!(saved_val["stream"], "desktop-stream-2");
        assert_eq!(saved_val["sync_retry_delays"], json!([10, 20]));
        assert_eq!(saved_val["sync_max_retries"], 25);
        assert_eq!(saved_val["sync_stale_threshold"], 1200);
        assert_eq!(saved_val["custom_plugin_field"], json!({"active": true}));
    }

    struct ModeGuard(PathBuf, u32);
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1));
        }
    }

    #[test]
    fn fail_closed_on_corrupt_non_object_symlink_directory_and_unreadable_destinations() {
        // 1. Corrupt JSON
        let t1 = tempfile::tempdir().unwrap();
        let path1 = t1.path().join("cfg/config.json");
        fs::create_dir_all(t1.path().join("cfg")).unwrap();
        fs::write(&path1, b"not json content").unwrap();
        let c1 = Config {
            base_dir: t1.path().into(),
            config_dir: t1.path().join("cfg"),
            ..Config::default()
        };
        assert!(save_config(&c1).is_err());
        assert_eq!(fs::read(&path1).unwrap(), b"not json content");
        assert!(save_linked_stream(&paths(t1.path()), "stream").is_err());
        assert_eq!(fs::read(&path1).unwrap(), b"not json content");
        assert!(sanitize_link_authority(&paths(t1.path())).is_err());
        assert_eq!(fs::read(&path1).unwrap(), b"not json content");

        // 2. Non-object JSON
        let t2 = tempfile::tempdir().unwrap();
        let path2 = t2.path().join("cfg/config.json");
        fs::create_dir_all(t2.path().join("cfg")).unwrap();
        fs::write(&path2, b"[\"not\", \"an\", \"object\"]").unwrap();
        let c2 = Config {
            base_dir: t2.path().into(),
            config_dir: t2.path().join("cfg"),
            ..Config::default()
        };
        assert!(save_config(&c2).is_err());
        assert_eq!(fs::read(&path2).unwrap(), b"[\"not\", \"an\", \"object\"]");
        assert!(save_linked_stream(&paths(t2.path()), "stream").is_err());
        assert_eq!(fs::read(&path2).unwrap(), b"[\"not\", \"an\", \"object\"]");
        assert!(sanitize_link_authority(&paths(t2.path())).is_err());
        assert_eq!(fs::read(&path2).unwrap(), b"[\"not\", \"an\", \"object\"]");

        // 3. Symlink destination
        let t3 = tempfile::tempdir().unwrap();
        let config_dir3 = t3.path().join("cfg");
        fs::create_dir_all(&config_dir3).unwrap();
        let referent3 = t3.path().join("referent.json");
        fs::write(&referent3, br#"{"stream":"external"}"#).unwrap();
        symlink(&referent3, config_dir3.join("config.json")).unwrap();
        let c3 = Config {
            base_dir: t3.path().into(),
            config_dir: config_dir3.clone(),
            ..Config::default()
        };
        assert!(save_config(&c3).is_err());
        assert_eq!(fs::read(&referent3).unwrap(), br#"{"stream":"external"}"#);
        assert!(save_linked_stream(&paths(t3.path()), "stream").is_err());
        assert_eq!(fs::read(&referent3).unwrap(), br#"{"stream":"external"}"#);
        assert!(sanitize_link_authority(&paths(t3.path())).is_err());
        assert_eq!(fs::read(&referent3).unwrap(), br#"{"stream":"external"}"#);

        // 4. Directory destination (wrong kind)
        let t4 = tempfile::tempdir().unwrap();
        let dir_path4 = t4.path().join("cfg/config.json");
        fs::create_dir_all(&dir_path4).unwrap();
        let c4 = Config {
            base_dir: t4.path().into(),
            config_dir: t4.path().join("cfg"),
            ..Config::default()
        };
        assert!(save_config(&c4).is_err());
        assert!(dir_path4.is_dir());
        assert!(save_linked_stream(&paths(t4.path()), "stream").is_err());
        assert!(dir_path4.is_dir());
        assert!(sanitize_link_authority(&paths(t4.path())).is_err());
        assert!(dir_path4.is_dir());

        // 5. Unreadable regular file destination (chmod 000)
        // If the test runs with euid 0 (root), mode 000 might still allow read access;
        // if save_config / link writers error (non-root), assert original bytes remain untouched.
        let t5 = tempfile::tempdir().unwrap();
        let unreadable_path5 = t5.path().join("cfg/config.json");
        fs::create_dir_all(t5.path().join("cfg")).unwrap();
        let initial_bytes = br#"{"stream":"unreadable-origin"}"#;
        fs::write(&unreadable_path5, initial_bytes).unwrap();
        fs::set_permissions(&unreadable_path5, fs::Permissions::from_mode(0o000)).unwrap();
        let _guard = ModeGuard(unreadable_path5.clone(), 0o600);
        let c5 = Config {
            base_dir: t5.path().into(),
            config_dir: t5.path().join("cfg"),
            ..Config::default()
        };
        if save_config(&c5).is_err() {
            let _ = fs::set_permissions(&unreadable_path5, fs::Permissions::from_mode(0o600));
            assert_eq!(fs::read(&unreadable_path5).unwrap(), initial_bytes);
        }
    }

    struct InjectExternalEditFault {
        path: PathBuf,
        bytes_to_write: Vec<u8>,
    }

    impl DurableWriteFault for InjectExternalEditFault {
        fn before(&self, stage: DurableWriteStage) -> Result<(), io::Error> {
            if stage == DurableWriteStage::Rename {
                fs::write(&self.path, &self.bytes_to_write)?;
            }
            Ok(())
        }
    }

    #[test]
    fn currency_guard_rejects_concurrent_modifications() {
        // Case A: dest existed and was replaced concurrently
        let t_a = tempfile::tempdir().unwrap();
        let config_a = Config {
            base_dir: t_a.path().into(),
            config_dir: t_a.path().join("cfg"),
            ..Config::default()
        };
        save_config(&config_a).unwrap();
        let path_a = config_a.config_path();
        let external_bytes_a = b"{\n  \"stream\": \"external-racing-write\"\n}\n".to_vec();
        let fault_a = InjectExternalEditFault {
            path: path_a.clone(),
            bytes_to_write: external_bytes_a.clone(),
        };
        let mut modified_config_a = config_a.clone();
        modified_config_a.cache_retention_days = 99;
        let res_a = save_config_with_fault(&modified_config_a, &fault_a);
        assert!(res_a.is_err());
        assert_eq!(fs::read(&path_a).unwrap(), external_bytes_a);

        // Case B: dest was missing and was created concurrently
        let t_b = tempfile::tempdir().unwrap();
        let config_b = Config {
            base_dir: t_b.path().into(),
            config_dir: t_b.path().join("cfg"),
            ..Config::default()
        };
        let path_b = config_b.config_path();
        let external_bytes_b = b"{\n  \"stream\": \"created-while-missing\"\n}\n".to_vec();
        let fault_b = InjectExternalEditFault {
            path: path_b.clone(),
            bytes_to_write: external_bytes_b.clone(),
        };
        let res_b = save_linked_stream_with_fault(&paths(t_b.path()), "stream-b", &fault_b);
        assert!(res_b.is_err());
        assert_eq!(fs::read(&path_b).unwrap(), external_bytes_b);
    }

    #[test]
    fn mixed_fixture_strips_retired_keys_and_preserves_unknowns_without_diagnostic_leak() {
        let fixture = json!({
            "server_url": "https://legacy-auth.test/sentinel",
            "key": "SUPER-SECRET-LEGACY-KEY-SENTINEL",
            "chat_bridge_enabled": "LEGACY-CHAT-SENTINEL",
            "ordinary_unknown": "PRESERVED-ORDINARY-SENTINEL",
            "stream": "mixed-stream"
        });

        // 1. save_config
        {
            let t = tempfile::tempdir().unwrap();
            write(t.path(), fixture.clone());
            let paths = paths(t.path());
            let loaded = load_config(paths);
            let debug_loaded = format!("{:?}", loaded.config);
            assert!(!debug_loaded.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!debug_loaded.contains("https://legacy-auth.test/sentinel"));
            assert!(!debug_loaded.contains("LEGACY-CHAT-SENTINEL"));
            assert!(!debug_loaded.contains("PRESERVED-ORDINARY-SENTINEL"));

            save_config(&loaded.config).unwrap();
            let debug_saved = format!("{:?}", loaded.config);
            assert!(!debug_saved.contains("PRESERVED-ORDINARY-SENTINEL"));

            let disk_text = fs::read_to_string(loaded.config.config_path()).unwrap();
            assert!(!disk_text.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!disk_text.contains("https://legacy-auth.test/sentinel"));
            assert!(!disk_text.contains("LEGACY-CHAT-SENTINEL"));
            assert!(disk_text.contains("PRESERVED-ORDINARY-SENTINEL"));

            let disk_val: Value = serde_json::from_str(&disk_text).unwrap();
            assert!(disk_val.get("server_url").is_none());
            assert!(disk_val.get("key").is_none());
            assert!(disk_val.get("chat_bridge_enabled").is_none());
            assert_eq!(disk_val["ordinary_unknown"], "PRESERVED-ORDINARY-SENTINEL");
        }

        // 2. save_linked_stream
        {
            let t = tempfile::tempdir().unwrap();
            write(t.path(), fixture.clone());
            let paths = paths(t.path());
            let saved = save_linked_stream(&paths, "new-stream-name").unwrap();
            let debug_saved = format!("{:?}", saved);
            assert!(!debug_saved.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!debug_saved.contains("https://legacy-auth.test/sentinel"));
            assert!(!debug_saved.contains("LEGACY-CHAT-SENTINEL"));
            assert!(!debug_saved.contains("PRESERVED-ORDINARY-SENTINEL"));

            let disk_text = fs::read_to_string(saved.config_path()).unwrap();
            assert!(!disk_text.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!disk_text.contains("https://legacy-auth.test/sentinel"));
            assert!(!disk_text.contains("LEGACY-CHAT-SENTINEL"));
            assert!(disk_text.contains("PRESERVED-ORDINARY-SENTINEL"));

            let disk_val: Value = serde_json::from_str(&disk_text).unwrap();
            assert!(disk_val.get("server_url").is_none());
            assert!(disk_val.get("key").is_none());
            assert!(disk_val.get("chat_bridge_enabled").is_none());
            assert_eq!(disk_val["stream"], "new-stream-name");
            assert_eq!(disk_val["ordinary_unknown"], "PRESERVED-ORDINARY-SENTINEL");
        }

        // 3. sanitize_link_authority
        {
            let t = tempfile::tempdir().unwrap();
            write(t.path(), fixture.clone());
            let paths = paths(t.path());
            let saved = sanitize_link_authority(&paths).unwrap();
            let debug_saved = format!("{:?}", saved);
            assert!(!debug_saved.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!debug_saved.contains("https://legacy-auth.test/sentinel"));
            assert!(!debug_saved.contains("LEGACY-CHAT-SENTINEL"));
            assert!(!debug_saved.contains("PRESERVED-ORDINARY-SENTINEL"));

            let disk_text = fs::read_to_string(saved.config_path()).unwrap();
            assert!(!disk_text.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!disk_text.contains("https://legacy-auth.test/sentinel"));
            assert!(!disk_text.contains("LEGACY-CHAT-SENTINEL"));
            assert!(disk_text.contains("PRESERVED-ORDINARY-SENTINEL"));

            let disk_val: Value = serde_json::from_str(&disk_text).unwrap();
            assert!(disk_val.get("server_url").is_none());
            assert!(disk_val.get("key").is_none());
            assert!(disk_val.get("chat_bridge_enabled").is_none());
            assert_eq!(disk_val["ordinary_unknown"], "PRESERVED-ORDINARY-SENTINEL");
        }

        // 4. Writer Err Display strings omit sentinels
        {
            let t = tempfile::tempdir().unwrap();
            let corrupt_bytes = b"not json content";
            let cfg_path = t.path().join("cfg/config.json");
            fs::create_dir_all(t.path().join("cfg")).unwrap();
            fs::write(&cfg_path, corrupt_bytes).unwrap();
            let paths = paths(t.path());
            let err = sanitize_link_authority(&paths).unwrap_err();
            let err_display = format!("{err}");
            assert!(!err_display.contains("SUPER-SECRET-LEGACY-KEY-SENTINEL"));
            assert!(!err_display.contains("PRESERVED-ORDINARY-SENTINEL"));
        }
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

    // AC: a stale whole-config writer preserves a newer linked stream.
    #[test]
    fn stale_settings_snapshot_preserves_linked_stream() {
        let t = tempfile::tempdir().unwrap();
        let initial = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("cfg"),
            cache_retention_days: 7,
            ..Config::default()
        };
        save_config(&initial).unwrap();
        let config_paths = paths(t.path());
        save_linked_stream(&config_paths, "desktop-old").unwrap();
        let mut stale_settings = load_config(config_paths.clone()).config;

        save_linked_stream(&config_paths, "desktop-new").unwrap();
        stale_settings.cache_retention_days = 30;
        save_config(&stale_settings).unwrap();

        let saved = load_config(config_paths).config;
        assert_eq!(saved.stream, "desktop-new");
        assert_eq!(saved.cache_retention_days, 30);
    }

    #[test]
    fn config_write_key_is_destination_scoped_and_lexically_normalized() {
        let first = Config {
            base_dir: PathBuf::from("first-data"),
            config_dir: PathBuf::from("./config-root//cfg/."),
            ..Config::default()
        };
        let second = Config {
            base_dir: PathBuf::from("second-data"),
            config_dir: PathBuf::from("config-root/cfg"),
            ..Config::default()
        };
        let parent = Config {
            config_dir: PathBuf::from("config-root/cfg/.."),
            ..Config::default()
        };
        assert_eq!(
            config_write_lock_key(&first),
            config_write_lock_key(&second)
        );
        assert_ne!(
            config_write_lock_key(&first),
            config_write_lock_key(&parent)
        );
    }

    #[test]
    fn distinct_destinations_do_not_contend() {
        for_each_config_write_pair(|first, second| {
            assert_config_write_admission_case(
                DestinationRelation::Distinct,
                first,
                second,
                run_config_write_flow,
            );
        });
    }

    #[test]
    fn same_destination_regular_alias_admission_times_out_then_recovers() {
        for_each_config_write_pair(|first, second| {
            assert_config_write_admission_case(
                DestinationRelation::SameLexicalAlias,
                first,
                second,
                run_config_write_flow,
            );
        });
    }

    #[test]
    fn same_destination_serializes_across_migrate_park() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        let alternate_config_dir = config_dir.join(".");

        let first_base = temp.path().join("first-data");
        let legacy_dir = first_base.join("config");
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_config = legacy_dir.join("config.json");
        let status = Command::new("mkfifo").arg(&legacy_config).status().unwrap();
        assert!(status.success());

        let first_paths = config_paths(first_base, config_dir.clone());
        let second_base = temp.path().join("second-data");
        let second_paths = config_paths(second_base.clone(), alternate_config_dir.clone());
        let second_config = Config {
            base_dir: second_base,
            config_dir: second_paths.config_dir.clone().unwrap(),
            cache_retention_days: 44,
            ..Config::default()
        };

        let first = thread::spawn(move || save_linked_stream(&first_paths, "migrate-first"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !config_dir.exists() {
            assert!(
                Instant::now() < deadline,
                "migration should create the destination directory before opening the FIFO"
            );
            thread::yield_now();
        }
        let primary_metadata = fs::metadata(&config_dir).unwrap();
        let alternate_metadata = fs::metadata(&alternate_config_dir).unwrap();
        assert_eq!(
            (primary_metadata.dev(), primary_metadata.ino()),
            (alternate_metadata.dev(), alternate_metadata.ino())
        );
        let absent_before_second = !config_dir.join("config.json").exists();
        let second_result = save_config(&second_config);
        let absent_after_second = !config_dir.join("config.json").exists();
        drop(
            fs::OpenOptions::new()
                .write(true)
                .open(legacy_config)
                .unwrap(),
        );
        let first_result = first.join().unwrap();

        // A replacement-only lock leaves A's load/migrate outside admission, so B succeeds
        // here instead of timing out; B never reads the migration FIFO, so the test fails rather
        // than hangs.
        assert!(absent_before_second);
        assert_config_write_timeout(second_result);
        assert!(absent_after_second);
        first_result.unwrap();

        save_config(&second_config).unwrap();
        let saved = load_config(second_paths).config;
        assert_eq!(saved.stream, "migrate-first");
        assert_eq!(saved.cache_retention_days, 44);
    }

    #[test]
    fn representative_legacy_configs_preserve_settings_and_stream_while_stripping_authority() {
        let t = tempfile::tempdir().unwrap();
        write(
            t.path(),
            json!({
                "server_url": "https://VALID-URL-SENTINEL.invalid",
                "key": "VALID-KEY-SENTINEL",
                "stream": "desktop",
                "segment_interval": 17,
                "sync_retry_delays": [2, 4],
                "sync_max_retries": 3,
                "sync_stale_threshold": 91,
                "cache_retention_days": 12,
                "chat_bridge_enabled": false,
                "capture_framerate": 4,
                "draw_cursor": false,
                "start_paused": true
            }),
        );

        let sanitized = sanitize_link_authority(&paths(t.path())).unwrap();
        assert_eq!(sanitized.stream, "desktop");
        assert_eq!(sanitized.segment_interval, 17);
        assert_eq!(sanitized.sync_retry_delays, vec![2, 4]);
        assert_eq!(sanitized.sync_max_retries, 3);
        assert_eq!(sanitized.sync_stale_threshold, 91);
        assert_eq!(sanitized.cache_retention_days, 12);
        assert_eq!(sanitized.capture_framerate, 4);
        assert!(!sanitized.draw_cursor);
        assert!(sanitized.start_paused);
        let value: Value =
            serde_json::from_slice(&fs::read(t.path().join("cfg/config.json")).unwrap()).unwrap();
        assert!(value.get("server_url").is_none());
        assert!(value.get("key").is_none());
        assert!(value.get("chat_bridge_enabled").is_none());
    }

    #[test]
    fn legacy_values_are_discarded_before_warnings_and_never_serialized() {
        for legacy in [
            json!({
                "server_url": "VALID-URL-SENTINEL",
                "key": "VALID-KEY-SENTINEL",
                "chat_bridge_enabled": "VALID-CHAT-SENTINEL",
                "stream": "desktop"
            }),
            json!({
                "server_url": {"secret": "WRONG-URL-SENTINEL"},
                "key": ["WRONG-KEY-SENTINEL"],
                "chat_bridge_enabled": {"secret": "WRONG-CHAT-SENTINEL"},
                "stream": "desktop"
            }),
        ] {
            let t = tempfile::tempdir().unwrap();
            write(t.path(), legacy);
            let loaded = load(t.path());
            assert_eq!(loaded.config.stream, "desktop");
            let warning_text = loaded
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            let debug = format!("{:?}", loaded.config);
            let serialized = serde_json::to_string(&loaded.config).unwrap();
            for sentinel in [
                "VALID-URL-SENTINEL",
                "VALID-KEY-SENTINEL",
                "VALID-CHAT-SENTINEL",
                "WRONG-URL-SENTINEL",
                "WRONG-KEY-SENTINEL",
                "WRONG-CHAT-SENTINEL",
            ] {
                assert!(!warning_text.contains(sentinel));
                assert!(!debug.contains(sentinel));
                assert!(!serialized.contains(sentinel));
            }
            assert!(loaded.warnings.is_empty());
        }
    }

    #[test]
    fn fresh_config_serializes_authority_free_schema() {
        let t = tempfile::tempdir().unwrap();
        let saved = sanitize_link_authority(&paths(t.path())).unwrap();
        let text = fs::read_to_string(saved.config_path()).unwrap();
        assert!(text.ends_with('\n'));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert!(value.get("server_url").is_none());
        assert!(value.get("key").is_none());
        assert!(value.get("chat_bridge_enabled").is_none());
        assert_eq!(value["stream"], "");
        assert_eq!(
            text,
            concat!(
                "{\n",
                "  \"stream\": \"\",\n",
                "  \"segment_interval\": 300,\n",
                "  \"sync_retry_delays\": [\n",
                "    5,\n    30,\n    120,\n    300\n",
                "  ],\n",
                "  \"sync_max_retries\": 10,\n",
                "  \"sync_stale_threshold\": 600,\n",
                "  \"cache_retention_days\": 7,\n",
                "  \"capture_framerate\": 1,\n",
                "  \"draw_cursor\": true,\n",
                "  \"start_paused\": false\n",
                "}\n"
            )
        );
    }

    #[test]
    fn sanitation_rejects_symlinked_or_wrong_kind_root_without_touching_referent() {
        let t = tempfile::tempdir().unwrap();
        let referent = t.path().join("referent");
        fs::create_dir(&referent).unwrap();
        fs::write(referent.join("sentinel"), "unchanged").unwrap();
        let linked = t.path().join("linked");
        symlink(&referent, &linked).unwrap();
        let linked_paths = ConfigPaths {
            base_dir: None,
            config_dir: Some(linked),
        };
        assert!(sanitize_link_authority(&linked_paths).is_err());
        assert_eq!(
            fs::read_to_string(referent.join("sentinel")).unwrap(),
            "unchanged"
        );

        let wrong = t.path().join("wrong");
        fs::write(&wrong, "unchanged").unwrap();
        let wrong_paths = ConfigPaths {
            base_dir: None,
            config_dir: Some(wrong.clone()),
        };
        assert!(sanitize_link_authority(&wrong_paths).is_err());
        assert_eq!(fs::read_to_string(wrong).unwrap(), "unchanged");
    }

    #[test]
    fn sanitation_rejects_symlinked_or_wrong_kind_config_file_without_touching_referent() {
        let t = tempfile::tempdir().unwrap();
        let config_dir = t.path().join("cfg");
        fs::create_dir(&config_dir).unwrap();
        let referent = t.path().join("referent.json");
        fs::write(&referent, r#"{"stream":"external"}"#).unwrap();
        symlink(&referent, config_dir.join("config.json")).unwrap();
        assert!(sanitize_link_authority(&paths(t.path())).is_err());
        assert_eq!(
            fs::read_to_string(&referent).unwrap(),
            r#"{"stream":"external"}"#
        );

        fs::remove_file(config_dir.join("config.json")).unwrap();
        fs::create_dir(config_dir.join("config.json")).unwrap();
        assert!(sanitize_link_authority(&paths(t.path())).is_err());
        assert!(config_dir.join("config.json").is_dir());
    }

    #[test]
    fn sanitation_fault_preserves_last_complete_config() {
        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
            DurableWriteStage::DirSync,
        ] {
            let t = tempfile::tempdir().unwrap();
            write(
                t.path(),
                json!({"server_url":"VALID-URL-SENTINEL","key":"VALID-KEY-SENTINEL","chat_bridge_enabled":"VALID-CHAT-SENTINEL","stream":"old"}),
            );
            let path = t.path().join("cfg/config.json");
            let before = fs::read(&path).unwrap();
            let result = sanitize_link_authority_with_fault(&paths(t.path()), &FailStage(stage));
            assert!(result.is_err(), "{stage:?}");
            if stage == DurableWriteStage::DirSync {
                let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(value["stream"], "old");
                assert!(value.get("server_url").is_none());
                assert!(value.get("key").is_none());
                assert!(value.get("chat_bridge_enabled").is_none());
            } else {
                assert_eq!(fs::read(path).unwrap(), before, "{stage:?}");
            }
        }
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
