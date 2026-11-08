// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::Duration;

use crate::support::TestDirectory;
use solstone_terminal::config::{CONFIG_FILENAME, CaptureSettings, ConfigError, RuntimeConfig};

#[test]
fn missing_config_uses_hostname_defaults() {
    let temporary = TestDirectory::new("config-defaults");
    let config = RuntimeConfig::load(temporary.path(), "Owner Host.example.com")
        .expect("default native config");

    assert_eq!(config.capture_interval, Duration::from_secs(5));
    assert_eq!(config.segment_interval, Duration::from_secs(300));
    assert!(config.status_indicator);
    assert!(config.pane_title);
    assert!(config.working_directory);
    assert!(config.executable);
    assert!(config.output_sessions.is_empty());
}

#[test]
fn present_config_defaults_status_indicator_to_true() {
    let temporary = TestDirectory::new("config-status-default");
    fs::write(
        temporary.path().join(CONFIG_FILENAME),
        br#"{"capture_interval":5,"segment_interval":300}"#,
    )
    .expect("write config without indicator field");

    let config =
        RuntimeConfig::load(temporary.path(), "ignored").expect("load defaulted indicator");

    assert!(config.status_indicator);
    assert!(config.pane_title);
    assert!(config.working_directory);
    assert!(config.executable);
}

#[test]
fn present_config_overrides_defaults_and_is_private() {
    let temporary = TestDirectory::new("config-present");
    let path = temporary.path().join(CONFIG_FILENAME);
    fs::write(
        &path,
        br#"{"capture_interval":2,"segment_interval":30,"status_indicator":false}"#,
    )
    .expect("write config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("set config mode");

    let config = RuntimeConfig::load(temporary.path(), "ignored").expect("native config");

    assert_eq!(config.capture_interval, Duration::from_secs(2));
    assert_eq!(config.segment_interval, Duration::from_secs(30));
    assert!(!config.status_indicator);
    assert!(config.pane_title);
    assert!(config.working_directory);
    assert!(config.executable);
    assert_eq!(
        fs::metadata(path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn invalid_stream_and_intervals_are_actionable() {
    let temporary = TestDirectory::new("config-invalid");
    let path = temporary.path().join(CONFIG_FILENAME);

    fs::write(&path, br#"{"capture_interval":0,"segment_interval":300}"#)
        .expect("write zero interval");
    assert!(matches!(
        RuntimeConfig::load(temporary.path(), "ignored"),
        Err(ConfigError::InvalidInterval {
            field: "capture_interval"
        })
    ));
}

#[test]
fn config_symlink_is_rejected_without_reading_referent() {
    let temporary = TestDirectory::new("config-symlink");
    let referent = temporary.path().join("referent.json");
    let path = temporary.path().join(CONFIG_FILENAME);
    fs::write(&referent, br#"{}"#).expect("write referent");
    symlink(&referent, &path).expect("config symlink");

    assert!(matches!(
        RuntimeConfig::load(temporary.path(), "ignored"),
        Err(ConfigError::InvalidTarget(target)) if target == path
    ));
    assert_eq!(fs::read(referent).expect("referent unchanged"), br#"{}"#);
}

#[test]
fn unknown_config_fields_are_still_rejected() {
    let temporary = TestDirectory::new("config-unknown-field");
    fs::write(
        temporary.path().join(CONFIG_FILENAME),
        br#"{"capture_interval":5,"segment_interval":300,"unknown":true}"#,
    )
    .expect("write unknown field");
    assert!(matches!(
        RuntimeConfig::load(temporary.path(), "ignored"),
        Err(ConfigError::InvalidConfig { .. })
    ));
}

#[test]
fn config_pane_context_switches_preserve_explicit_false_and_enable_independently() {
    let temporary = TestDirectory::new("config-pane-context");
    let path = temporary.path().join(CONFIG_FILENAME);
    fs::write(
        &path,
        br#"{"capture_interval":5,"segment_interval":300,"pane_title":false,"working_directory":false,"executable":false}"#,
    )
    .expect("write explicit false switches");
    let config = RuntimeConfig::load(temporary.path(), "ignored").expect("false switches");
    assert!(!config.pane_title && !config.working_directory && !config.executable);

    for (enabled, expected) in [
        ("pane_title", (true, false, false)),
        ("working_directory", (false, true, false)),
        ("executable", (false, false, true)),
    ] {
        let body = format!(
            "{{\"capture_interval\":5,\"segment_interval\":300,\"pane_title\":{},\"working_directory\":{},\"executable\":{}}}",
            enabled == "pane_title",
            enabled == "working_directory",
            enabled == "executable"
        );
        fs::write(&path, body).expect("write one enabled switch");
        let config = RuntimeConfig::load(temporary.path(), "ignored").expect("enabled switch");
        assert_eq!(
            (
                config.pane_title,
                config.working_directory,
                config.executable
            ),
            expected,
            "{enabled} is independent"
        );
    }
}

#[test]
fn output_sessions_are_read_and_empty_names_are_rejected() {
    let temporary = TestDirectory::new("config-output-sessions");
    let path = temporary.path().join(CONFIG_FILENAME);
    fs::write(&path, br#"{"output_sessions":["agent-*","build"]}"#).expect("write output sessions");
    let config = RuntimeConfig::load(temporary.path(), "ignored").expect("load output sessions");
    assert_eq!(config.output_sessions, ["agent-*", "build"]);

    fs::write(&path, br#"{"output_sessions":["agent-*",""]}"#).expect("write empty name");
    let error = RuntimeConfig::load(temporary.path(), "ignored").expect_err("empty name");
    assert!(matches!(error, ConfigError::InvalidOutputSessions));
    assert!(
        error
            .to_string()
            .contains("update output_sessions in config.json")
    );
}

#[test]
fn settings_file_that_does_not_exist_is_an_error() {
    let temporary = TestDirectory::new("settings-missing");
    let path = temporary.path().join("settings.json");

    assert!(CaptureSettings::load_file(&path).is_err());
    assert!(!path.exists());
}

#[test]
fn read_only_settings_file_is_read_and_left_as_it_is() {
    let temporary = TestDirectory::new("settings-read-only");
    let path = temporary.path().join("settings.json");
    let body = br#"{"capture_interval":2,"segment_interval":30}"#;
    fs::write(&path, body).expect("write settings");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("read-only");

    let settings = CaptureSettings::load_file(&path).expect("read-only settings");

    assert_eq!(settings.capture_interval, 2);
    assert_eq!(settings.segment_interval, 30);
    let metadata = fs::metadata(&path).expect("settings metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o444);
    assert_eq!(fs::read(&path).expect("settings bytes"), body);
}

#[test]
fn invalid_settings_file_is_an_error_and_is_left_as_it_is() {
    let temporary = TestDirectory::new("settings-invalid");
    let path = temporary.path().join("settings.json");
    for body in [
        br#"{"capture_interval":0}"#.as_slice(),
        br#"{"unknown":true}"#.as_slice(),
        br#"{not json"#.as_slice(),
    ] {
        fs::write(&path, body).expect("write settings");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("mode");

        assert!(CaptureSettings::load_file(&path).is_err());
        let metadata = fs::metadata(&path).expect("settings metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
        assert_eq!(fs::read(&path).expect("settings bytes"), body);
    }
    assert_eq!(fs::read_dir(temporary.path()).expect("dir").count(), 1);
}

#[test]
fn output_sessions_reject_only_empty_names() {
    assert!(matches!(
        CaptureSettings::parse_json(r#"{"output_sessions":[""]}"#),
        Err(ConfigError::InvalidOutputSessions)
    ));
    let settings =
        CaptureSettings::parse_json(r#"{"output_sessions":[" "]}"#).expect("a space is a name");
    assert_eq!(settings.output_sessions, vec![" ".to_owned()]);
}
