// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[test]
fn actual_about_cli_is_readonly_and_version_bytes_are_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let executable = env!("CARGO_BIN_EXE_solstone-linux");
    let output = std::process::Command::new(executable)
        .arg("about")
        .env_clear()
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with(&format!(
        "linux desktop app {} · ",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(text.ends_with("\njournal unknown\n"));
    assert_eq!(text.lines().count(), 2);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    let version = std::process::Command::new(executable)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        version.stdout,
        format!("solstone-linux {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}
