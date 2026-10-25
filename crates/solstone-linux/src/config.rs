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
/// Desktop activity runs whenever screen intake runs, unless the owner turns it off.
pub const DEFAULT_ACTIVITY_SOURCE: bool = true;
pub const DEFAULT_ACTIVITY_TITLES: bool = true;
/// The address preference starts on. Addresses are read only once the owner has
/// also given the address consent, which turns on desktop accessibility.
pub const DEFAULT_ACTIVITY_URLS: bool = true;
pub const DEFAULT_ACTIVITY_THROUGH_PAUSE: bool = false;
const ACTIVITY_OWNER_VERSION: u64 = 1;
const SETTINGS_OWNER_VERSION: u64 = 2;
/// Saved files at this version keep a settings key when the owner set it.
///
/// Version 1 dropped desktop activity defaults that earlier releases wrote on
/// first run. Those choices, and the address consent, stay as stored once the
/// file is version 1 or later. Version 2 drops saved defaults of the other
/// settings keys. Each drop is pinned to its own version, so a later version
/// does not run either one again.
pub(crate) const CONFIG_VERSION: u64 = SETTINGS_OWNER_VERSION;
const CONFIG_VERSION_KEY: &str = "config_version";
const PRE_VERSION_ACTIVITY_KEYS: [&str; 3] =
    ["activity_source", "activity_titles", "activity_urls"];
pub(crate) const ACTIVITY_POLICY_KEYS: [&str; 6] = [
    "activity_source",
    "activity_titles",
    "activity_urls",
    "activity_exclude_apps",
    "activity_exclude_titles",
    "activity_through_pause",
];
pub(crate) const RETIRED_CONFIG_KEYS: [&str; 4] = [
    "server_url",
    "key",
    "chat_bridge_enabled",
    "cache_retention_days",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub stream: String,
    pub segment_interval: i64,
    pub sync_retry_delays: Vec<i64>,
    pub sync_max_retries: i64,
    pub sync_stale_threshold: i64,
    pub capture_framerate: i64,
    pub draw_cursor: bool,
    pub start_paused: bool,
    /// Whether the running app may offer to set up the GNOME panel icon. The owner
    /// turns this off from the offer itself, or by hand like every other key.
    pub panel_icon_offer: bool,
    /// Desktop activity choices are saved only when the owner makes them, so these
    /// fields are never serialized from a whole settings snapshot.
    #[serde(default, skip_serializing)]
    pub activity_source: bool,
    #[serde(default, skip_serializing)]
    pub activity_titles: bool,
    /// The effective address choice: the saved preference and the owner's address
    /// consent together.
    #[serde(default, skip_serializing)]
    pub activity_urls: bool,
    #[serde(default, skip_serializing)]
    pub activity_exclude_apps: Vec<String>,
    #[serde(default, skip_serializing)]
    pub activity_exclude_titles: Vec<String>,
    /// Desktop activity keeps going through the owner's pause.
    #[serde(default, skip_serializing)]
    pub activity_through_pause: bool,
    #[serde(default)]
    pub activity_only: bool,
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
            capture_framerate: 1,
            draw_cursor: true,
            start_paused: false,
            panel_icon_offer: true,
            activity_source: DEFAULT_ACTIVITY_SOURCE,
            activity_titles: DEFAULT_ACTIVITY_TITLES,
            // No address consent exists before the owner gives it.
            activity_urls: false,
            activity_exclude_apps: Vec::new(),
            activity_exclude_titles: Vec::new(),
            activity_through_pause: DEFAULT_ACTIVITY_THROUGH_PAUSE,
            activity_only: false,
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

fn load_activity_bool(value: Option<&Value>, default: bool) -> Result<bool, ()> {
    match value {
        None => Ok(default),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(()),
    }
}

fn load_activity_exclusions(value: Option<&Value>) -> Result<Vec<String>, ()> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(());
    };
    if items.len() > 64 {
        return Err(());
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(());
        };
        if text.is_empty() || text.len() > 256 || out.iter().any(|seen| seen == text) {
            return Err(());
        }
        out.push(text.to_string());
    }
    Ok(out)
}

/// Whether this file's source, title and address values are the owner's own.
fn activity_choices_are_owner_set(values: &Map<String, Value>) -> bool {
    values
        .get(CONFIG_VERSION_KEY)
        .and_then(Value::as_u64)
        .is_some_and(|version| version >= ACTIVITY_OWNER_VERSION)
}

fn saved_config_version(values: &Map<String, Value>) -> u64 {
    values
        .get(CONFIG_VERSION_KEY)
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

const FROZEN_STREAM: &str = "";
const FROZEN_SEGMENT_INTERVAL: i64 = 300;
const FROZEN_SYNC_RETRY_DELAYS: [i64; 4] = [5, 30, 120, 300];
const FROZEN_SYNC_MAX_RETRIES: i64 = 10;
const FROZEN_SYNC_STALE_THRESHOLD: i64 = 600;
const FROZEN_CAPTURE_FRAMERATE: i64 = 1;
const FROZEN_DRAW_CURSOR: bool = true;
const FROZEN_START_PAUSED: bool = false;
const FROZEN_PANEL_ICON_OFFER: bool = true;
const FROZEN_ACTIVITY_ONLY: bool = false;

fn is_matching_integer(value: &Value, expected: i64) -> bool {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64() == Some(expected),
        _ => false,
    }
}

fn is_matching_bool(value: &Value, expected: bool) -> bool {
    matches!(value, Value::Bool(b) if *b == expected)
}

fn is_matching_sync_retry_delays(value: &Value) -> bool {
    let Some(arr) = value.as_array() else {
        return false;
    };
    if arr.len() != FROZEN_SYNC_RETRY_DELAYS.len() {
        return false;
    }
    arr.iter()
        .zip(FROZEN_SYNC_RETRY_DELAYS.iter())
        .all(|(item, expected)| is_matching_integer(item, *expected))
}

fn strip_saved_setting_defaults(map: &mut Map<String, Value>) {
    if map.get("stream").and_then(Value::as_str) == Some(FROZEN_STREAM) {
        map.remove("stream");
    }
    if map
        .get("segment_interval")
        .is_some_and(|v| is_matching_integer(v, FROZEN_SEGMENT_INTERVAL))
    {
        map.remove("segment_interval");
    }
    if map
        .get("sync_retry_delays")
        .is_some_and(is_matching_sync_retry_delays)
    {
        map.remove("sync_retry_delays");
    }
    if map
        .get("sync_max_retries")
        .is_some_and(|v| is_matching_integer(v, FROZEN_SYNC_MAX_RETRIES))
    {
        map.remove("sync_max_retries");
    }
    if map
        .get("sync_stale_threshold")
        .is_some_and(|v| is_matching_integer(v, FROZEN_SYNC_STALE_THRESHOLD))
    {
        map.remove("sync_stale_threshold");
    }
    if map
        .get("capture_framerate")
        .is_some_and(|v| is_matching_integer(v, FROZEN_CAPTURE_FRAMERATE))
    {
        map.remove("capture_framerate");
    }
    if map
        .get("draw_cursor")
        .is_some_and(|v| is_matching_bool(v, FROZEN_DRAW_CURSOR))
    {
        map.remove("draw_cursor");
    }
    if map
        .get("start_paused")
        .is_some_and(|v| is_matching_bool(v, FROZEN_START_PAUSED))
    {
        map.remove("start_paused");
    }
    if map
        .get("panel_icon_offer")
        .is_some_and(|v| is_matching_bool(v, FROZEN_PANEL_ICON_OFFER))
    {
        map.remove("panel_icon_offer");
    }
    if map
        .get("activity_only")
        .is_some_and(|v| is_matching_bool(v, FROZEN_ACTIVITY_ONLY))
    {
        map.remove("activity_only");
    }
}

/// A saved desktop activity value, or `None` when the file holds no owner choice
/// for it. A missing key means the current default in code.
fn saved_activity_value<'a>(values: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    if PRE_VERSION_ACTIVITY_KEYS.contains(&key) && !activity_choices_are_owner_set(values) {
        return None;
    }
    values.get(key)
}

/// Whether the owner gave the address consent. Earlier releases recorded it on any
/// activity save, so in a file from before version 1 it counts only where the
/// owner had actually turned addresses on.
fn address_consent_given(values: &Map<String, Value>) -> bool {
    values.get("activity_urls_consent").and_then(Value::as_u64) == Some(1)
        && (activity_choices_are_owner_set(values)
            || values.get("activity_urls") == Some(&Value::Bool(true)))
}

/// Desktop activity choices as a file holds them, defaults filled in from code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedActivity {
    pub source: bool,
    pub titles: bool,
    /// The effective address choice: preference and the owner's address consent.
    pub urls: bool,
    pub exclude_apps: Vec<String>,
    pub exclude_titles: Vec<String>,
    pub through_pause: bool,
}

pub(crate) fn read_saved_activity(values: &Map<String, Value>) -> Result<SavedActivity, ()> {
    let source = load_activity_bool(
        saved_activity_value(values, "activity_source"),
        DEFAULT_ACTIVITY_SOURCE,
    )?;
    let titles = load_activity_bool(
        saved_activity_value(values, "activity_titles"),
        DEFAULT_ACTIVITY_TITLES,
    )?;
    let urls = load_activity_bool(
        saved_activity_value(values, "activity_urls"),
        DEFAULT_ACTIVITY_URLS,
    )?;
    let through_pause = load_activity_bool(
        values.get("activity_through_pause"),
        DEFAULT_ACTIVITY_THROUGH_PAUSE,
    )?;
    let exclude_apps = load_activity_exclusions(values.get("activity_exclude_apps"))?;
    let exclude_titles = load_activity_exclusions(values.get("activity_exclude_titles"))?;
    Ok(SavedActivity {
        source,
        titles,
        // Addresses need desktop accessibility, which only the owner's own address
        // choice may turn on. A preference alone never does.
        urls: urls && address_consent_given(values),
        exclude_apps,
        exclude_titles,
        through_pause,
    })
}

/// A malformed activity boolean or exclusion list disables the source in memory.
/// The file is left as it was. The warning carries no exclusion text.
fn apply_loaded_activity_policy(
    values: &Map<String, Value>,
    config: &mut Config,
    warnings: &mut Vec<ConfigWarning>,
) {
    if let Ok(saved) = read_saved_activity(values) {
        config.activity_source = saved.source;
        config.activity_titles = saved.titles;
        config.activity_urls = saved.urls;
        config.activity_exclude_apps = saved.exclude_apps;
        config.activity_exclude_titles = saved.exclude_titles;
        config.activity_through_pause = saved.through_pause;
        return;
    }
    config.activity_source = false;
    config.activity_titles = false;
    config.activity_urls = false;
    config.activity_through_pause = false;
    config.activity_exclude_apps.clear();
    config.activity_exclude_titles.clear();
    warnings.push(ConfigWarning {
        field: Some("activity_source"),
        message: "activity policy is malformed; desktop activity stays off".into(),
    });
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

pub(crate) fn resolve_config_paths(paths: &ConfigPaths) -> Config {
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
    config.capture_framerate = load_int(values, "capture_framerate", 1, &mut warnings).clamp(1, 10);
    config.draw_cursor = json_truthy(values.get("draw_cursor"), true);
    config.start_paused = json_truthy(values.get("start_paused"), false);
    config.panel_icon_offer = json_truthy(values.get("panel_icon_offer"), true);
    apply_loaded_activity_policy(values, &mut config, &mut warnings);
    config.activity_only = json_truthy(values.get("activity_only"), false);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettingsExplicit {
    pub capture_framerate: bool,
    pub draw_cursor: bool,
    pub start_paused: bool,
    pub segment_interval: bool,
}

impl SettingsExplicit {
    pub(crate) const NONE: Self = Self {
        capture_framerate: false,
        draw_cursor: false,
        start_paused: false,
        segment_interval: false,
    };
}

enum ConfigOverlay<'a> {
    Settings(&'a Config, SettingsExplicit),
    Stream(&'a str),
    /// One key, written from the running app. Deliberately narrow: the daemon holds an
    /// in-memory config that may be older than a hand edit, so it must never write its
    /// whole settings snapshot back over the file.
    PanelIconOffer(bool),
    GuidedField(&'a str, Value),
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
    migrate_locked(&resolved)?;
    if matches!(&overlay, ConfigOverlay::Settings(..)) {
        resolved.ensure_dirs()?;
    } else {
        ensure_private_directory(&resolved.config_dir).map_err(io::Error::other)?;
    }

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
    // One-time: earlier releases wrote these defaults on first run, so their saved
    // values are not owner choices. Pinned to files from before version 1.
    if !activity_choices_are_owner_set(&map) {
        // The address consent was also written on any save, so it stays only where
        // the owner had turned addresses on. Nothing else turns on accessibility.
        if !address_consent_given(&map) {
            map.remove("activity_urls_consent");
        }
        for key in PRE_VERSION_ACTIVITY_KEYS {
            map.remove(key);
        }
    }
    if saved_config_version(&map) < SETTINGS_OWNER_VERSION {
        strip_saved_setting_defaults(&mut map);
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
    let mut panel_icon_offer = if merge_basis.is_some() {
        json_truthy(map.get("panel_icon_offer"), true)
    } else {
        true
    };
    let mut activity_basis = Config::default();
    if merge_basis.is_some() {
        apply_loaded_activity_policy(&map, &mut activity_basis, &mut warnings);
    }
    let mut activity_source = activity_basis.activity_source;
    let mut activity_titles = activity_basis.activity_titles;
    let mut activity_urls = activity_basis.activity_urls;
    let mut activity_exclude_apps = activity_basis.activity_exclude_apps;
    let mut activity_exclude_titles = activity_basis.activity_exclude_titles;
    let mut activity_through_pause = activity_basis.activity_through_pause;
    let mut activity_only = if merge_basis.is_some() {
        json_truthy(map.get("activity_only"), false)
    } else {
        false
    };

    match overlay {
        ConfigOverlay::Settings(snapshot, explicit) => {
            // Desktop activity choices are saved only through their own fields, so
            // a whole settings snapshot never writes them or the address consent.
            let normalized_framerate = snapshot.capture_framerate.clamp(1, 10);
            if explicit.capture_framerate || normalized_framerate != capture_framerate {
                map.insert(
                    "capture_framerate".into(),
                    Value::Number(normalized_framerate.into()),
                );
            }
            capture_framerate = normalized_framerate;

            let normalized_draw_cursor = snapshot.draw_cursor;
            if explicit.draw_cursor || normalized_draw_cursor != draw_cursor {
                map.insert("draw_cursor".into(), Value::Bool(normalized_draw_cursor));
            }
            draw_cursor = normalized_draw_cursor;

            let normalized_start_paused = snapshot.start_paused;
            if explicit.start_paused || normalized_start_paused != start_paused {
                map.insert("start_paused".into(), Value::Bool(normalized_start_paused));
            }
            start_paused = normalized_start_paused;

            let normalized_segment_interval = snapshot.segment_interval;
            if explicit.segment_interval || normalized_segment_interval != segment_interval {
                map.insert(
                    "segment_interval".into(),
                    Value::Number(normalized_segment_interval.into()),
                );
            }
            segment_interval = normalized_segment_interval;

            let normalized_panel_icon_offer = snapshot.panel_icon_offer;
            if normalized_panel_icon_offer != panel_icon_offer {
                map.insert(
                    "panel_icon_offer".into(),
                    Value::Bool(normalized_panel_icon_offer),
                );
            }
            panel_icon_offer = normalized_panel_icon_offer;

            let normalized_activity_only = snapshot.activity_only;
            if normalized_activity_only != activity_only {
                map.insert(
                    "activity_only".into(),
                    Value::Bool(normalized_activity_only),
                );
            }
            activity_only = normalized_activity_only;

            if merge_basis.is_none() {
                if snapshot.stream != stream {
                    map.insert("stream".into(), Value::String(snapshot.stream.clone()));
                    stream = snapshot.stream.clone();
                }
                if snapshot.sync_retry_delays != sync_retry_delays {
                    map.insert(
                        "sync_retry_delays".into(),
                        Value::Array(
                            snapshot
                                .sync_retry_delays
                                .iter()
                                .copied()
                                .map(Value::from)
                                .collect(),
                        ),
                    );
                    sync_retry_delays = snapshot.sync_retry_delays.clone();
                }
                if snapshot.sync_max_retries != sync_max_retries {
                    map.insert(
                        "sync_max_retries".into(),
                        Value::Number(snapshot.sync_max_retries.into()),
                    );
                    sync_max_retries = snapshot.sync_max_retries;
                }
                if snapshot.sync_stale_threshold != sync_stale_threshold {
                    map.insert(
                        "sync_stale_threshold".into(),
                        Value::Number(snapshot.sync_stale_threshold.into()),
                    );
                    sync_stale_threshold = snapshot.sync_stale_threshold;
                }
            }
        }
        ConfigOverlay::Stream(new_stream) => {
            map.insert("stream".into(), Value::String(new_stream.to_owned()));
            stream = new_stream.to_owned();
        }
        ConfigOverlay::PanelIconOffer(value) => {
            map.insert("panel_icon_offer".into(), Value::Bool(value));
            panel_icon_offer = value;
        }
        ConfigOverlay::GuidedField(key, value) => {
            let normalized = match key {
                "capture_framerate" => {
                    let n = value.as_i64().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid capture_framerate")
                    })?;
                    let clamped = n.clamp(1, 10);
                    capture_framerate = clamped;
                    Value::Number(clamped.into())
                }
                "draw_cursor" => {
                    let b = value.as_bool().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid draw_cursor")
                    })?;
                    draw_cursor = b;
                    Value::Bool(b)
                }
                "start_paused" => {
                    let b = value.as_bool().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid start_paused")
                    })?;
                    start_paused = b;
                    Value::Bool(b)
                }
                "segment_interval" => {
                    let n = value.as_i64().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid segment_interval")
                    })?;
                    if n <= 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "segment_interval must be positive",
                        ));
                    }
                    segment_interval = n;
                    Value::Number(n.into())
                }
                "activity_source" => {
                    let b = value.as_bool().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_source")
                    })?;
                    activity_source = b;
                    Value::Bool(b)
                }
                "activity_titles" => {
                    let b = value.as_bool().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_titles")
                    })?;
                    activity_titles = b;
                    Value::Bool(b)
                }
                "activity_urls" => {
                    let b = value.as_bool().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_urls")
                    })?;
                    activity_urls = b;
                    map.insert("activity_urls_consent".into(), Value::from(1));
                    Value::Bool(b)
                }
                "activity_only" => {
                    let b = value.as_bool().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_only")
                    })?;
                    activity_only = b;
                    Value::Bool(b)
                }
                "activity_exclude_apps" => {
                    let arr = value.as_array().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_exclude_apps")
                    })?;
                    let mut items = Vec::new();
                    for item in arr {
                        let s = item.as_str().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "invalid activity_exclude_apps entry",
                            )
                        })?;
                        items.push(s.to_string());
                    }
                    activity_exclude_apps = items;
                    Value::Array(arr.clone())
                }
                "activity_exclude_titles" => {
                    let arr = value.as_array().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid activity_exclude_titles",
                        )
                    })?;
                    let mut items = Vec::new();
                    for item in arr {
                        let s = item.as_str().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "invalid activity_exclude_titles entry",
                            )
                        })?;
                        items.push(s.to_string());
                    }
                    activity_exclude_titles = items;
                    Value::Array(arr.clone())
                }
                "activity_policy" => {
                    // Only the choices the owner changed arrive here, and only those
                    // are written. A missing key keeps the current default.
                    let obj = value.as_object().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_policy")
                    })?;
                    if obj.is_empty()
                        || obj
                            .keys()
                            .any(|key| !ACTIVITY_POLICY_KEYS.contains(&key.as_str()))
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid activity_policy",
                        ));
                    }
                    let flag = |key: &str| -> io::Result<Option<bool>> {
                        obj.get(key)
                            .map(|value| {
                                value.as_bool().ok_or_else(|| {
                                    io::Error::new(
                                        io::ErrorKind::InvalidInput,
                                        "invalid activity_policy",
                                    )
                                })
                            })
                            .transpose()
                    };
                    let list = |key: &str| -> io::Result<Option<Vec<String>>> {
                        let Some(value) = obj.get(key) else {
                            return Ok(None);
                        };
                        let invalid = || {
                            io::Error::new(io::ErrorKind::InvalidInput, "invalid activity_policy")
                        };
                        let items = value.as_array().ok_or_else(invalid)?;
                        items
                            .iter()
                            .map(|item| item.as_str().map(str::to_string).ok_or_else(invalid))
                            .collect::<io::Result<Vec<_>>>()
                            .map(Some)
                    };
                    let src = flag("activity_source")?;
                    let tit = flag("activity_titles")?;
                    let url = flag("activity_urls")?;
                    let through = flag("activity_through_pause")?;
                    let apps = list("activity_exclude_apps")?;
                    let titles = list("activity_exclude_titles")?;

                    if let Some(src) = src {
                        activity_source = src;
                    }
                    if let Some(tit) = tit {
                        activity_titles = tit;
                    }
                    if let Some(url) = url {
                        activity_urls = url;
                        // The owner made the address choice itself, with the switch
                        // that says it turns on desktop accessibility.
                        map.insert("activity_urls_consent".into(), Value::from(1));
                    }
                    if let Some(through) = through {
                        activity_through_pause = through;
                    }
                    if let Some(apps) = apps {
                        activity_exclude_apps = apps;
                    }
                    if let Some(titles) = titles {
                        activity_exclude_titles = titles;
                    }
                    for (key, value) in obj {
                        map.insert(key.clone(), value.clone());
                    }
                    Value::Null
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unknown guided field",
                    ));
                }
            };
            if key != "activity_policy" {
                map.insert(key.to_owned(), normalized);
            }
        }
        ConfigOverlay::Sanitize => {}
    }

    let returned_config = Config {
        stream: stream.clone(),
        segment_interval,
        sync_retry_delays: sync_retry_delays.clone(),
        sync_max_retries,
        sync_stale_threshold,
        capture_framerate,
        draw_cursor,
        start_paused,
        panel_icon_offer,
        activity_source,
        activity_titles,
        activity_urls,
        activity_exclude_apps: activity_exclude_apps.clone(),
        activity_exclude_titles: activity_exclude_titles.clone(),
        activity_through_pause,
        activity_only,
        base_dir: resolved.base_dir.clone(),
        config_dir: resolved.config_dir.clone(),
    };

    map.insert(CONFIG_VERSION_KEY.into(), Value::from(CONFIG_VERSION));
    let mut bytes = serde_json::to_vec_pretty(&map).map_err(io::Error::other)?;
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
    save_settings_explicit_with_fault(config, SettingsExplicit::NONE, fault)
}

pub fn save_config(config: &Config) -> io::Result<()> {
    save_config_with_fault(config, &NoWriteFault)
}

pub(crate) fn save_settings_explicit(
    config: &Config,
    explicit: SettingsExplicit,
) -> io::Result<()> {
    save_settings_explicit_with_fault(config, explicit, &NoWriteFault)
}

pub(crate) fn save_settings_explicit_with_fault(
    config: &Config,
    explicit: SettingsExplicit,
    fault: &dyn DurableWriteFault,
) -> io::Result<()> {
    write_config_merge(
        &ConfigPaths {
            base_dir: Some(config.base_dir.clone()),
            config_dir: Some(config.config_dir.clone()),
        },
        ConfigOverlay::Settings(config, explicit),
        fault,
    )
    .map(|_| ())
}

/// Read the panel-icon offer preference and nothing else.
///
/// ⛔ Deliberately not `load_config`: that path runs legacy migration and takes the
/// config write lock, and the running app re-reads this every probe. A pure read cannot
/// contend with a settings write and cannot migrate anything as a side effect of a poll.
/// Absent, unreadable or malformed all mean the same thing here -- leave the offer on.
pub fn panel_icon_offer_enabled(paths: &ConfigPaths) -> bool {
    let resolved = resolve_config_paths(paths);
    let Ok(text) = fs::read_to_string(resolved.config_path()) else {
        return true;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .map(|map| json_truthy(map.get("panel_icon_offer"), true))
        })
        .unwrap_or(true)
}

/// Persist the panel-icon offer preference alone, leaving every other key -- modelled,
/// unmodelled or hand-added -- exactly as the file has it.
pub fn set_panel_icon_offer(paths: &ConfigPaths, value: bool) -> io::Result<Config> {
    write_config_merge(paths, ConfigOverlay::PanelIconOffer(value), &NoWriteFault)
}

pub(crate) fn set_guided_field(paths: &ConfigPaths, key: &str, value: Value) -> io::Result<Config> {
    set_guided_field_with_fault(paths, key, value, &NoWriteFault)
}

pub(crate) fn set_guided_field_with_fault(
    paths: &ConfigPaths,
    key: &str,
    value: Value,
    fault: &dyn DurableWriteFault,
) -> io::Result<Config> {
    write_config_merge(paths, ConfigOverlay::GuidedField(key, value), fault)
}

fn migrate(config: &Config) -> io::Result<()> {
    let lock = config_write_lock(config)?;
    let _guard = acquire_config_write_lock(&lock)?;
    migrate_locked(config)
}

fn migrate_locked(config: &Config) -> io::Result<()> {
    migrate_locked_with_hook(config, || {})
}

fn migrate_locked_with_hook(config: &Config, before_copy: impl FnOnce()) -> io::Result<()> {
    let old_dir = config.base_dir.join("config");
    if config.config_dir == old_dir || config.config_path().exists() {
        return Ok(());
    }
    let old_config = old_dir.join("config.json");
    if !old_config.exists() {
        return Ok(());
    }
    fs::create_dir_all(&config.config_dir)?;
    before_copy();
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
fn migrate_with_hook(config: &Config, before_copy: impl FnOnce()) -> io::Result<()> {
    let lock = config_write_lock(config)?;
    let _guard = acquire_config_write_lock(&lock)?;
    migrate_locked_with_hook(config, before_copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_file::DurableWriteStage;
    use serde_json::json;
    use std::{
        os::unix::fs::{MetadataExt, symlink},
        sync::mpsc,
        thread,
        time::Duration,
    };

    #[test]
    fn a_whole_settings_save_writes_no_activity_choice_or_address_consent() {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            config_dir: root.path().join("config"),
            base_dir: root.path().join("data"),
            activity_source: false,
            activity_titles: false,
            activity_urls: true,
            activity_through_pause: true,
            activity_exclude_apps: vec!["Excluded".into()],
            ..Config::default()
        };
        let paths = ConfigPaths {
            base_dir: Some(config.base_dir.clone()),
            config_dir: Some(config.config_dir.clone()),
        };
        for _ in 0..2 {
            save_config(&config).unwrap();
            let saved: Value =
                serde_json::from_slice(&fs::read(config.config_path()).unwrap()).unwrap();
            for key in ACTIVITY_POLICY_KEYS
                .iter()
                .copied()
                .chain(["activity_urls_consent"])
            {
                assert!(saved.get(key).is_none(), "{key} written: {saved}");
            }
            let loaded = load_config(paths.clone()).config;
            assert!(loaded.activity_source && loaded.activity_titles);
            assert!(!loaded.activity_urls && !loaded.activity_through_pause);
            assert!(loaded.activity_exclude_apps.is_empty());
        }
    }

    fn saved_map(root: &std::path::Path) -> Map<String, Value> {
        let text = fs::read_to_string(root.join("cfg").join("config.json")).unwrap();
        serde_json::from_str::<Value>(&text)
            .unwrap()
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn missing_activity_keys_read_as_the_defaults_in_code() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"config_version": CONFIG_VERSION}));
        let loaded = load(t.path()).config;
        assert!(loaded.activity_source, "on with screen intake");
        assert!(loaded.activity_titles);
        assert!(!loaded.activity_through_pause);
        assert!(
            !loaded.activity_urls,
            "addresses wait for their own consent"
        );
        // With the address consent given, the address preference starts on.
        write(
            t.path(),
            json!({"config_version": CONFIG_VERSION, "activity_urls_consent": 1}),
        );
        assert!(load(t.path()).config.activity_urls);
        // No file at all: the same defaults.
        let empty = tempfile::tempdir().unwrap();
        let fresh = load(empty.path()).config;
        assert!(fresh.activity_source && fresh.activity_titles && !fresh.activity_urls);
    }

    #[test]
    fn fresh_start_writes_no_activity_default() {
        let t = tempfile::tempdir().unwrap();
        sanitize_link_authority(&paths(t.path())).unwrap();
        sanitize_link_authority(&paths(t.path())).unwrap();
        let saved = saved_map(t.path());
        for key in ACTIVITY_POLICY_KEYS {
            assert!(saved.get(key).is_none(), "{key} written: {saved:?}");
        }
        assert_eq!(saved[CONFIG_VERSION_KEY], json!(CONFIG_VERSION));
    }

    #[test]
    fn earlier_saved_activity_defaults_migrate_once() {
        let t = tempfile::tempdir().unwrap();
        // What an earlier release wrote on first run, plus owner choices that stay.
        write(
            t.path(),
            json!({
                "stream": "desk",
                "activity_source": false,
                "activity_titles": false,
                "activity_urls": false,
                "activity_urls_consent": 1,
                "activity_exclude_apps": ["Excluded"],
                "activity_exclude_titles": ["Secret"],
                "activity_only": false,
                "capture_framerate": 4
            }),
        );
        // Before any write, the old values already read as unset, and a consent
        // written beside a saved `false` grants nothing.
        let before = load(t.path()).config;
        assert!(before.activity_source && before.activity_titles && !before.activity_urls);

        // The app's first write at this version migrates the file.
        sanitize_link_authority(&paths(t.path())).unwrap();
        let migrated = saved_map(t.path());
        for key in PRE_VERSION_ACTIVITY_KEYS {
            assert!(migrated.get(key).is_none(), "{key} kept: {migrated:?}");
        }
        assert_eq!(migrated[CONFIG_VERSION_KEY], json!(CONFIG_VERSION));
        assert!(migrated.get("activity_urls_consent").is_none());
        assert_eq!(migrated["activity_exclude_apps"], json!(["Excluded"]));
        assert_eq!(migrated["activity_exclude_titles"], json!(["Secret"]));
        assert_eq!(migrated["stream"], json!("desk"));
        assert_eq!(migrated["capture_framerate"], json!(4));
        let loaded = load(t.path()).config;
        assert!(loaded.activity_source && loaded.activity_titles);
        assert!(
            !loaded.activity_urls,
            "accessibility is not turned on at first start"
        );
        assert_eq!(loaded.activity_exclude_apps, ["Excluded"]);

        // An owner's off choice after the migration stays off through later writes.
        set_guided_field(
            &paths(t.path()),
            "activity_policy",
            json!({"activity_source": false, "activity_titles": false}),
        )
        .unwrap();
        sanitize_link_authority(&paths(t.path())).unwrap();
        save_linked_stream(&paths(t.path()), "desk-2").unwrap();
        let kept = load(t.path()).config;
        assert!(!kept.activity_source && !kept.activity_titles);
        let saved = saved_map(t.path());
        assert_eq!(saved["activity_source"], json!(false));
        assert_eq!(saved["activity_titles"], json!(false));
    }

    #[test]
    fn migration_keeps_the_address_consent_only_where_addresses_were_on() {
        for (saved_urls, kept) in [(true, true), (false, false)] {
            let t = tempfile::tempdir().unwrap();
            write(
                t.path(),
                json!({
                    "activity_source": true,
                    "activity_titles": true,
                    "activity_urls": saved_urls,
                    "activity_urls_consent": 1
                }),
            );
            assert_eq!(load(t.path()).config.activity_urls, kept);
            sanitize_link_authority(&paths(t.path())).unwrap();
            let migrated = saved_map(t.path());
            assert!(migrated.get("activity_urls").is_none());
            assert_eq!(migrated.contains_key("activity_urls_consent"), kept);
            assert_eq!(load(t.path()).config.activity_urls, kept);
            // A later write leaves the outcome as it is.
            sanitize_link_authority(&paths(t.path())).unwrap();
            assert_eq!(load(t.path()).config.activity_urls, kept);
        }
    }

    #[test]
    fn an_activity_policy_write_saves_only_the_choices_it_carries() {
        let t = tempfile::tempdir().unwrap();
        sanitize_link_authority(&paths(t.path())).unwrap();
        let saved = set_guided_field(
            &paths(t.path()),
            "activity_policy",
            json!({"activity_titles": false}),
        )
        .unwrap();
        assert!(saved.activity_source && !saved.activity_titles);
        let map = saved_map(t.path());
        assert_eq!(map["activity_titles"], json!(false));
        for key in [
            "activity_source",
            "activity_urls",
            "activity_urls_consent",
            "activity_through_pause",
            "activity_exclude_apps",
            "activity_exclude_titles",
        ] {
            assert!(map.get(key).is_none(), "{key} written: {map:?}");
        }
        // Only the owner's own address choice records the address consent.
        let saved = set_guided_field(
            &paths(t.path()),
            "activity_policy",
            json!({"activity_urls": true, "activity_through_pause": true}),
        )
        .unwrap();
        assert!(saved.activity_urls && saved.activity_through_pause);
        let map = saved_map(t.path());
        assert_eq!(map["activity_urls_consent"], json!(1));
        assert_eq!(map["activity_through_pause"], json!(true));
        assert!(map.get("activity_source").is_none());
        // Unknown or empty policies change nothing.
        for bad in [
            json!({}),
            json!({"activity_source": "yes"}),
            json!({"other": true}),
        ] {
            assert!(set_guided_field(&paths(t.path()), "activity_policy", bad).is_err());
        }
        assert_eq!(saved_map(t.path()), map);
    }

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
        CaptureFramerate,
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
        field: SettingsField::CaptureFramerate,
        setting: 9,
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
                    SettingsField::CaptureFramerate => {
                        config.capture_framerate = value.setting;
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
                SettingsField::CaptureFramerate => {
                    assert_eq!(config.capture_framerate, value.setting);
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
    #[test]
    fn malformed_activity_policy_disables_source_and_keeps_the_file() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("cfg");
        fs::create_dir_all(&d).unwrap();
        let raw = br#"{"activity_source":"yes","activity_exclude_apps":"SECRET_TITLE_SENTINEL","capture_framerate":4}"#;
        let path = d.join("config.json");
        fs::write(&path, raw).unwrap();
        let loaded = load(t.path());
        assert!(!loaded.config.activity_source);
        assert!(!loaded.config.activity_titles);
        assert!(!loaded.config.activity_urls);
        assert!(loaded.config.activity_exclude_apps.is_empty());
        assert_eq!(loaded.config.capture_framerate, 4);
        assert_eq!(fs::read(&path).unwrap(), raw);
        let messages: Vec<&str> = loaded.warnings.iter().map(|w| w.message.as_str()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("activity policy is malformed"))
        );
        assert!(
            messages
                .iter()
                .all(|m| !m.contains("SECRET_TITLE_SENTINEL"))
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
    #[test]
    fn panel_icon_offer_defaults_on_and_round_trips() {
        let t = tempfile::tempdir().unwrap();
        assert!(Config::default().panel_icon_offer);
        assert!(!round_trip(t.path(), |c| c.panel_icon_offer = false).panel_icon_offer);
    }

    #[test]
    fn panel_icon_offer_defaults_on_for_a_config_written_before_it_existed() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"stream": "old"}));
        assert!(load(t.path()).config.panel_icon_offer);
    }

    #[test]
    fn the_narrow_panel_icon_writer_touches_only_its_own_key() {
        // The running app writes this key while an owner may have hand-edited the file.
        // Everything else in it -- modelled and unmodelled alike -- has to survive.
        let t = tempfile::tempdir().unwrap();
        write(
            t.path(),
            json!({
                "stream": "linked-stream",
                "segment_interval": 900,
                "draw_cursor": false,
                "hand_added_key": {"kept": [1, 2]},
            }),
        );
        let returned = set_panel_icon_offer(&paths(t.path()), false).unwrap();
        assert!(!returned.panel_icon_offer);

        let text = fs::read_to_string(t.path().join("cfg/config.json")).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["panel_icon_offer"], json!(false));
        assert_eq!(value["stream"], json!("linked-stream"));
        assert_eq!(value["segment_interval"], json!(900));
        assert_eq!(value["draw_cursor"], json!(false));
        assert_eq!(value["hand_added_key"], json!({"kept": [1, 2]}));

        let reloaded = load(t.path()).config;
        assert!(!reloaded.panel_icon_offer);
        assert_eq!(reloaded.segment_interval, 900);
        assert!(!reloaded.draw_cursor);

        // ...and turning it back on by hand is the documented way back.
        assert!(
            set_panel_icon_offer(&paths(t.path()), true)
                .unwrap()
                .panel_icon_offer
        );
        assert!(load(t.path()).config.panel_icon_offer);
    }

    #[test]
    fn the_offer_read_is_pure_and_defaults_on_for_anything_it_cannot_parse() {
        let t = tempfile::tempdir().unwrap();
        // no file at all
        assert!(panel_icon_offer_enabled(&paths(t.path())));
        // present and true
        write(t.path(), json!({"panel_icon_offer": true}));
        assert!(panel_icon_offer_enabled(&paths(t.path())));
        // present and false
        write(t.path(), json!({"panel_icon_offer": false}));
        assert!(!panel_icon_offer_enabled(&paths(t.path())));
        // malformed, and the wrong type, both leave the offer on rather than silencing it
        fs::write(t.path().join("cfg/config.json"), "{not json").unwrap();
        assert!(panel_icon_offer_enabled(&paths(t.path())));
        write(t.path(), json!({"panel_icon_offer": "nonsense"}));
        assert!(panel_icon_offer_enabled(&paths(t.path())));
    }

    #[test]
    fn the_offer_read_never_creates_or_migrates_anything() {
        // It runs on every probe in the live app, so it must not be a writer.
        let t = tempfile::tempdir().unwrap();
        let legacy = t.path().join("config");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("config.json"), r#"{"stream":"legacy"}"#).unwrap();

        assert!(panel_icon_offer_enabled(&paths(t.path())));

        // load_config would have migrated this; the narrow read must leave it alone.
        assert!(!t.path().join("cfg/config.json").exists());
        assert!(legacy.join("config.json").exists());
    }

    #[test]
    fn a_stale_in_memory_config_cannot_undo_a_hand_edit_through_the_narrow_writer() {
        // The daemon loads once at start. If the owner then edits config.json, the offer
        // dismissal must not carry the daemon's stale view of the other keys back.
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"segment_interval": 300}));
        let stale = load(t.path()).config;
        assert_eq!(stale.segment_interval, 300);

        write(t.path(), json!({"segment_interval": 1200}));
        set_panel_icon_offer(&paths(t.path()), false).unwrap();

        let reloaded = load(t.path()).config;
        assert_eq!(reloaded.segment_interval, 1200);
        assert!(!reloaded.panel_icon_offer);
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
    // all nine persisted defaults.
    #[test]
    fn all_defaults() {
        let c = Config::default();
        assert_eq!(
            serde_json::to_value(c).unwrap(),
            json!({
                "stream": "",
                "segment_interval": 300,
                "sync_retry_delays": [5, 30, 120, 300],
                "sync_max_retries": 10,
                "sync_stale_threshold": 600,
                "capture_framerate": 1,
                "draw_cursor": true,
                "start_paused": false,
                "panel_icon_offer": true,
                "activity_only": false
            })
        );
    }
    // numeric coercion rejects bool and truncates floats, including list elements.
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
    // Python JSON truthiness mapping.
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
    // draw_cursor uses truthiness, not string parsing.
    #[test]
    fn cursor_truthiness() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"draw_cursor":"false"}));
        assert!(load(t.path()).config.draw_cursor);
        write(t.path(), json!({"draw_cursor":0}));
        assert!(!load(t.path()).config.draw_cursor);
    }
    // typed string deviation warns once and defaults empty.
    #[test]
    fn non_string_field() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"stream":7}));
        let x = load(t.path());
        assert_eq!(x.config.stream, "");
        assert_eq!(warning_fields(&x), vec![Some("stream")]);
        assert!(x.warnings[0].message.contains("stream=7"));
    }
    // unknown top-level keys round-trip as equal serde_json::Value through all writers.
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
        c1.segment_interval = 600;
        save_config(&c1).unwrap();
        let v1: Value =
            serde_json::from_str(&fs::read_to_string(c1.config_path()).unwrap()).unwrap();
        assert_eq!(v1["segment_interval"], 600);
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

    // migrate preserves arbitrary unknown content byte-for-byte.
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

    // settings save overlays only prompted fields and preserves concurrent valid disk fields & unknowns.
    #[test]
    fn settings_merge_preserves_unprompted_fields_and_unknowns() {
        let t = tempfile::tempdir().unwrap();
        let config_paths = paths(t.path());
        let initial = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("cfg"),
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

        // Snapshot applies 4 prompted fields:
        snapshot.capture_framerate = 5;
        snapshot.draw_cursor = false;
        snapshot.start_paused = true;
        snapshot.segment_interval = 600;

        // Stale snapshot has old stream "desktop-stream-1", old delays, etc.
        save_config(&snapshot).unwrap();

        let saved_val: Value =
            serde_json::from_str(&fs::read_to_string(&disk_path).unwrap()).unwrap();
        // 4 prompted fields win
        assert_eq!(saved_val["capture_framerate"], 5);
        assert_eq!(saved_val["draw_cursor"], false);
        assert_eq!(saved_val["start_paused"], true);
        assert_eq!(saved_val["segment_interval"], 600);

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
        modified_config_a.segment_interval = 99;
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
    // temp suffix replaces .json and no temp remains after atomic rename.
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

    // a stale whole-config writer preserves a newer linked stream.
    #[test]
    fn stale_settings_snapshot_preserves_linked_stream() {
        let t = tempfile::tempdir().unwrap();
        let initial = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("cfg"),
            sync_max_retries: 25,
            ..Config::default()
        };
        save_config(&initial).unwrap();
        let config_paths = paths(t.path());
        save_linked_stream(&config_paths, "desktop-old").unwrap();
        let mut stale_settings = load_config(config_paths.clone()).config;

        save_linked_stream(&config_paths, "desktop-new").unwrap();
        stale_settings.segment_interval = 600;
        save_config(&stale_settings).unwrap();

        let saved = load_config(config_paths).config;
        assert_eq!(saved.stream, "desktop-new");
        assert_eq!(saved.segment_interval, 600);
        let saved_val: Value =
            serde_json::from_slice(&fs::read(saved.config_path()).unwrap()).unwrap();
        assert_eq!(saved_val["sync_max_retries"], 25);
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
        fs::write(&legacy_config, b"{\n  \"stream\": \"legacy\"\n}\n").unwrap();

        let first_paths = config_paths(first_base, config_dir.clone());
        let first_config = resolve_config_paths(&first_paths);
        let second_base = temp.path().join("second-data");
        let second_paths = config_paths(second_base.clone(), alternate_config_dir.clone());
        let second_config = Config {
            base_dir: second_base,
            config_dir: second_paths.config_dir.clone().unwrap(),
            segment_interval: 444,
            ..Config::default()
        };

        let (migration_ready_tx, migration_ready_rx) = mpsc::channel();
        let (migration_release_tx, migration_release_rx) = mpsc::channel();
        let first = thread::spawn(move || {
            migrate_with_hook(&first_config, || {
                migration_ready_tx.send(()).unwrap();
                migration_release_rx.recv().unwrap();
            })
        });
        migration_ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("migration should pause while holding destination admission");
        let primary_metadata = fs::metadata(&config_dir).unwrap();
        let alternate_metadata = fs::metadata(&alternate_config_dir).unwrap();
        assert_eq!(
            (primary_metadata.dev(), primary_metadata.ino()),
            (alternate_metadata.dev(), alternate_metadata.ino())
        );
        let absent_before_second = !config_dir.join("config.json").exists();
        let second_result = save_config(&second_config);
        let absent_after_second = !config_dir.join("config.json").exists();
        migration_release_tx.send(()).unwrap();
        let first_result = first.join().unwrap();

        // A replacement-only lock leaves A's migrate outside admission, so B succeeds here
        // instead of timing out and writes the destination before A's copy resumes.
        assert!(absent_before_second);
        assert_config_write_timeout(second_result);
        assert!(absent_after_second);
        first_result.unwrap();

        save_config(&second_config).unwrap();
        let saved = load_config(second_paths).config;
        assert_eq!(saved.stream, "legacy");
        assert_eq!(saved.segment_interval, 444);
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
        assert_eq!(sanitized.capture_framerate, 4);
        assert!(!sanitized.draw_cursor);
        assert!(sanitized.start_paused);
        let value: Value =
            serde_json::from_slice(&fs::read(t.path().join("cfg/config.json")).unwrap()).unwrap();
        assert!(value.get("server_url").is_none());
        assert!(value.get("key").is_none());
        assert!(value.get("chat_bridge_enabled").is_none());
        assert!(value.get("cache_retention_days").is_none());
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
    fn fresh_sanitation_leaves_owner_defaults_absent() {
        let t = tempfile::tempdir().unwrap();
        let saved = sanitize_link_authority(&paths(t.path())).unwrap();
        let text = fs::read_to_string(saved.config_path()).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let map = value.as_object().unwrap();
        assert_eq!(
            map.get(CONFIG_VERSION_KEY),
            Some(&Value::from(CONFIG_VERSION))
        );
        for key in [
            "stream",
            "segment_interval",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "capture_framerate",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
            "activity_only",
        ] {
            assert!(
                map.get(key).is_none(),
                "expected key {key} to be absent from fresh config, found: {:?}",
                map.get(key)
            );
        }
        let reloaded = load_config(paths(t.path())).config;
        assert_eq!(reloaded.stream, "");
        assert_eq!(reloaded.segment_interval, 300);
        assert_eq!(reloaded.sync_retry_delays, vec![5, 30, 120, 300]);
        assert_eq!(reloaded.sync_max_retries, 10);
        assert_eq!(reloaded.sync_stale_threshold, 600);
        assert_eq!(reloaded.capture_framerate, 1);
        assert!(reloaded.draw_cursor);
        assert!(!reloaded.start_paused);
        assert!(reloaded.panel_icon_offer);
        assert!(!reloaded.activity_only);
    }

    #[test]
    fn sparse_current_save_leaves_owner_defaults_absent() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), json!({"config_version": SETTINGS_OWNER_VERSION}));
        let loaded = load(t.path());
        save_config(&loaded.config).unwrap();
        let map = saved_map(t.path());
        assert_eq!(
            map.get(CONFIG_VERSION_KEY),
            Some(&json!(SETTINGS_OWNER_VERSION))
        );
        for key in [
            "stream",
            "segment_interval",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "capture_framerate",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
            "activity_only",
        ] {
            assert!(
                map.get(key).is_none(),
                "key {key} was unexpectedly written: {:?}",
                map.get(key)
            );
        }
        let reloaded = load(t.path()).config;
        assert_eq!(reloaded.stream, "");
        assert_eq!(reloaded.segment_interval, 300);
        assert_eq!(reloaded.sync_retry_delays, vec![5, 30, 120, 300]);
        assert_eq!(reloaded.sync_max_retries, 10);
        assert_eq!(reloaded.sync_stale_threshold, 600);
        assert_eq!(reloaded.capture_framerate, 1);
        assert!(reloaded.draw_cursor);
        assert!(!reloaded.start_paused);
        assert!(reloaded.panel_icon_offer);
        assert!(!reloaded.activity_only);
    }

    #[test]
    fn snapshot_change_persists_and_unrelated_writers_leave_other_keys() {
        let t = tempfile::tempdir().unwrap();
        write(
            t.path(),
            json!({
                "config_version": SETTINGS_OWNER_VERSION,
                "capture_framerate": 5,
                "segment_interval": 300,
                "stream": "desk",
                "custom_key": "custom_val"
            }),
        );
        let mut config = load(t.path()).config;
        config.capture_framerate = 1;
        save_config(&config).unwrap();
        let map = saved_map(t.path());
        assert_eq!(map["capture_framerate"], json!(1));
        assert_eq!(map["segment_interval"], json!(300));
        assert_eq!(map["stream"], json!("desk"));
        assert_eq!(map["custom_key"], json!("custom_val"));
        for absent in [
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
            "activity_only",
        ] {
            assert!(
                map.get(absent).is_none(),
                "absent key {absent} was added: {:?}",
                map.get(absent)
            );
        }

        let base_map = json!({
            "config_version": SETTINGS_OWNER_VERSION,
            "segment_interval": 300,
            "stream": "desk",
            "custom_key": "custom_val"
        });

        // Subcase 1: sanitize_link_authority
        let t1 = tempfile::tempdir().unwrap();
        write(t1.path(), base_map.clone());
        sanitize_link_authority(&paths(t1.path())).unwrap();
        let m1 = saved_map(t1.path());
        assert_eq!(m1["segment_interval"], json!(300));
        assert_eq!(m1["stream"], json!("desk"));
        assert_eq!(m1["custom_key"], json!("custom_val"));
        for absent in [
            "capture_framerate",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
            "activity_only",
        ] {
            assert!(m1.get(absent).is_none(), "sanitize added {absent}");
        }

        // Subcase 2: save_linked_stream of a new nonempty name
        let t2 = tempfile::tempdir().unwrap();
        write(t2.path(), base_map.clone());
        save_linked_stream(&paths(t2.path()), "new-stream").unwrap();
        let m2 = saved_map(t2.path());
        assert_eq!(m2["stream"], json!("new-stream"));
        assert_eq!(m2["segment_interval"], json!(300));
        assert_eq!(m2["custom_key"], json!("custom_val"));
        for absent in [
            "capture_framerate",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
            "activity_only",
        ] {
            assert!(
                m2.get(absent).is_none(),
                "save_linked_stream added {absent}"
            );
        }

        // Subcase 3: set_panel_icon_offer(true)
        let t3 = tempfile::tempdir().unwrap();
        write(t3.path(), base_map.clone());
        set_panel_icon_offer(&paths(t3.path()), true).unwrap();
        let m3 = saved_map(t3.path());
        assert_eq!(m3["panel_icon_offer"], json!(true));
        assert_eq!(m3["stream"], json!("desk"));
        assert_eq!(m3["segment_interval"], json!(300));
        assert_eq!(m3["custom_key"], json!("custom_val"));
        for absent in [
            "capture_framerate",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "draw_cursor",
            "start_paused",
            "activity_only",
        ] {
            assert!(
                m3.get(absent).is_none(),
                "set_panel_icon_offer added {absent}"
            );
        }

        // Subcase 4: set_guided_field of activity_only: true
        let t4 = tempfile::tempdir().unwrap();
        write(t4.path(), base_map.clone());
        set_guided_field(&paths(t4.path()), "activity_only", json!(true)).unwrap();
        let m4 = saved_map(t4.path());
        assert_eq!(m4["activity_only"], json!(true));
        assert_eq!(m4["stream"], json!("desk"));
        assert_eq!(m4["segment_interval"], json!(300));
        assert_eq!(m4["custom_key"], json!("custom_val"));
        for absent in [
            "capture_framerate",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
        ] {
            assert!(m4.get(absent).is_none(), "set_guided_field added {absent}");
        }
    }

    #[test]
    fn unversioned_and_version_one_migrate_once() {
        for max_retries in [3, 0] {
            let t = tempfile::tempdir().unwrap();
            write(
                t.path(),
                json!({
                    "segment_interval": 17,
                    "sync_retry_delays": [2, 4],
                    "sync_max_retries": max_retries,
                    "sync_stale_threshold": 91,
                    "capture_framerate": 4,
                    "draw_cursor": false,
                    "start_paused": true,
                    "panel_icon_offer": false,
                    "activity_only": true,
                    "stream": "my-stream",
                    "custom_key": 123
                }),
            );
            sanitize_link_authority(&paths(t.path())).unwrap();
            let m = saved_map(t.path());
            assert_eq!(m[CONFIG_VERSION_KEY], json!(SETTINGS_OWNER_VERSION));
            assert_eq!(m["segment_interval"], json!(17));
            assert_eq!(m["sync_retry_delays"], json!([2, 4]));
            assert_eq!(m["sync_max_retries"], json!(max_retries));
            assert_eq!(m["sync_stale_threshold"], json!(91));
            assert_eq!(m["capture_framerate"], json!(4));
            assert_eq!(m["draw_cursor"], json!(false));
            assert_eq!(m["start_paused"], json!(true));
            assert_eq!(m["panel_icon_offer"], json!(false));
            assert_eq!(m["activity_only"], json!(true));
            assert_eq!(m["stream"], json!("my-stream"));
            assert_eq!(m["custom_key"], json!(123));

            set_guided_field(&paths(t.path()), "capture_framerate", json!(1)).unwrap();
            sanitize_link_authority(&paths(t.path())).unwrap();
            let m_after = saved_map(t.path());
            assert_eq!(m_after["capture_framerate"], json!(1));
        }

        let t_def = tempfile::tempdir().unwrap();
        write(
            t_def.path(),
            json!({
                "stream": "",
                "segment_interval": 300,
                "sync_retry_delays": [5, 30, 120, 300],
                "sync_max_retries": 10,
                "sync_stale_threshold": 600,
                "capture_framerate": 1,
                "draw_cursor": true,
                "start_paused": false,
                "panel_icon_offer": true,
                "activity_only": false,
                "custom_key": "kept"
            }),
        );
        sanitize_link_authority(&paths(t_def.path())).unwrap();
        let m_def = saved_map(t_def.path());
        assert_eq!(m_def[CONFIG_VERSION_KEY], json!(SETTINGS_OWNER_VERSION));
        assert_eq!(m_def["custom_key"], json!("kept"));
        for key in [
            "stream",
            "segment_interval",
            "sync_retry_delays",
            "sync_max_retries",
            "sync_stale_threshold",
            "capture_framerate",
            "draw_cursor",
            "start_paused",
            "panel_icon_offer",
            "activity_only",
        ] {
            assert!(m_def.get(key).is_none(), "key {key} was not removed");
        }

        for (source_val, stream_in, stream_expected_kept) in
            [(true, "nonempty-stream", true), (false, "", false)]
        {
            let t = tempfile::tempdir().unwrap();
            write(
                t.path(),
                json!({
                    "config_version": ACTIVITY_OWNER_VERSION,
                    "activity_source": source_val,
                    "activity_urls_consent": 1,
                    "stream": stream_in,
                    "segment_interval": 300
                }),
            );
            sanitize_link_authority(&paths(t.path())).unwrap();
            let m = saved_map(t.path());
            assert_eq!(m[CONFIG_VERSION_KEY], json!(SETTINGS_OWNER_VERSION));
            assert_eq!(m["activity_source"], json!(source_val));
            assert_eq!(m["activity_urls_consent"], json!(1));
            assert!(
                m.get("segment_interval").is_none(),
                "segment_interval: 300 should be removed"
            );
            if stream_expected_kept {
                assert_eq!(m["stream"], json!(stream_in));
            } else {
                assert!(m.get("stream").is_none(), "empty stream should be removed");
            }

            set_guided_field(&paths(t.path()), "segment_interval", json!(300)).unwrap();
            sanitize_link_authority(&paths(t.path())).unwrap();
            let m_post = saved_map(t.path());
            assert_eq!(m_post["segment_interval"], json!(300));
        }
    }

    #[test]
    fn truthy_standins_are_not_historical_defaults() {
        for (field, val) in [
            ("draw_cursor", json!(1)),
            ("draw_cursor", json!("false")),
            ("activity_only", json!(0)),
            ("sync_retry_delays", json!([5, 30, 120, 300, 1])),
            ("sync_retry_delays", json!([5.0, 30, 120, 300])),
            ("capture_framerate", json!(1.0)),
        ] {
            let t = tempfile::tempdir().unwrap();
            let mut obj = serde_json::Map::new();
            obj.insert(field.into(), val.clone());
            write(t.path(), Value::Object(obj));
            sanitize_link_authority(&paths(t.path())).unwrap();
            let m = saved_map(t.path());
            assert_eq!(
                m.get(field),
                Some(&val),
                "field {field} with value {val} was unexpectedly stripped"
            );
        }
    }

    #[test]
    fn migration_fault_keeps_one_complete_object() {
        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
        ] {
            let t_unver = tempfile::tempdir().unwrap();
            let unver_json = json!({"segment_interval": 300, "stream": "desk"});
            write(t_unver.path(), unver_json);
            let before_bytes = fs::read(t_unver.path().join("cfg/config.json")).unwrap();
            assert!(
                sanitize_link_authority_with_fault(&paths(t_unver.path()), &FailStage(stage))
                    .is_err()
            );
            let after_bytes = fs::read(t_unver.path().join("cfg/config.json")).unwrap();
            assert_eq!(
                after_bytes, before_bytes,
                "unversioned bytes changed on stage {stage:?}"
            );

            let t_v1 = tempfile::tempdir().unwrap();
            let v1_json = json!({
                "config_version": ACTIVITY_OWNER_VERSION,
                "segment_interval": 300,
                "stream": "desk"
            });
            write(t_v1.path(), v1_json);
            let before_v1_bytes = fs::read(t_v1.path().join("cfg/config.json")).unwrap();
            assert!(
                sanitize_link_authority_with_fault(&paths(t_v1.path()), &FailStage(stage)).is_err()
            );
            let after_v1_bytes = fs::read(t_v1.path().join("cfg/config.json")).unwrap();
            assert_eq!(
                after_v1_bytes, before_v1_bytes,
                "v1 bytes changed on stage {stage:?}"
            );
        }

        let t_sync = tempfile::tempdir().unwrap();
        write(
            t_sync.path(),
            json!({"segment_interval": 300, "stream": "desk"}),
        );
        assert!(
            sanitize_link_authority_with_fault(
                &paths(t_sync.path()),
                &FailStage(DurableWriteStage::DirSync)
            )
            .is_err()
        );
        let m_sync = saved_map(t_sync.path());
        assert_eq!(m_sync[CONFIG_VERSION_KEY], json!(SETTINGS_OWNER_VERSION));
        assert!(m_sync.get("segment_interval").is_none());
        assert_eq!(m_sync["stream"], json!("desk"));

        set_guided_field(&paths(t_sync.path()), "capture_framerate", json!(1)).unwrap();
        sanitize_link_authority(&paths(t_sync.path())).unwrap();
        let m_sync_after = saved_map(t_sync.path());
        assert_eq!(m_sync_after["capture_framerate"], json!(1));

        let t_explicit_fault = tempfile::tempdir().unwrap();
        write(
            t_explicit_fault.path(),
            json!({"stream": "desk", "segment_interval": 300}),
        );
        let orig_bytes = fs::read(t_explicit_fault.path().join("cfg/config.json")).unwrap();
        let mut cfg = load(t_explicit_fault.path()).config;
        cfg.capture_framerate = 1;
        let explicit = SettingsExplicit {
            capture_framerate: true,
            ..SettingsExplicit::NONE
        };
        assert!(
            save_settings_explicit_with_fault(
                &cfg,
                explicit,
                &FailStage(DurableWriteStage::Rename)
            )
            .is_err()
        );
        let final_bytes = fs::read(t_explicit_fault.path().join("cfg/config.json")).unwrap();
        assert_eq!(final_bytes, orig_bytes);
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
        assert!(value.get("cache_retention_days").is_none());
        assert_eq!(text, concat!("{\n", "  \"config_version\": 2\n", "}\n"));
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

    // migration failure is returned as one warning and leaves legacy data intact.
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
    // a second migration load is byte- and mode-stable.
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

    #[test]
    fn retired_cache_retention_days_loads_without_warning_and_save_omits() {
        for days in [-1, 0, 7] {
            let t = tempfile::tempdir().unwrap();
            let config_dir = t.path().join("cfg");
            fs::create_dir_all(&config_dir).unwrap();
            fs::write(
                config_dir.join("config.json"),
                format!("{{\"cache_retention_days\": {days}, \"stream\": \"test\"}}\n"),
            )
            .unwrap();
            let loaded = load_config(ConfigPaths {
                base_dir: Some(t.path().into()),
                config_dir: Some(config_dir.clone()),
            });
            assert!(
                !loaded
                    .warnings
                    .iter()
                    .any(|w| w.field == Some("cache_retention_days")
                        || w.message.contains("cache_retention_days"))
            );
            save_config(&loaded.config).unwrap();
            let saved_json = fs::read_to_string(config_dir.join("config.json")).unwrap();
            assert!(!saved_json.contains("cache_retention_days"));
        }
    }
}
