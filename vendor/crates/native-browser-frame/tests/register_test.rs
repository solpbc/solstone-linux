// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use native_browser_frame::*;

#[test]
fn test_all_18_registration_rows() {
    let channels = ["production", "dev"];
    let browsers = ["chrome", "edge", "firefox"];
    let oses = ["linux", "macos", "windows"];

    for channel in channels {
        for browser in browsers {
            for os in oses {
                let rendered = render_registration(channel, browser, os, None, None).unwrap();
                let template_path = format!(
                    "{}/../../contracts/native-browser/registration/{}/{}/{}.json",
                    env!("CARGO_MANIFEST_DIR"),
                    channel, browser, os
                );
                let template_bytes = std::fs::read(&template_path).unwrap();
                let template_str = String::from_utf8(template_bytes).unwrap();

                let rendered_val: serde_json::Value = serde_json::from_str(&rendered.json).unwrap();
                let template_val: serde_json::Value = serde_json::from_str(&template_str).unwrap();

                assert_eq!(rendered_val, template_val, "mismatch in {}", template_path);

                if browser == "firefox" {
                    assert!(rendered_val.get("allowed_extensions").is_some());
                    assert!(rendered_val.get("allowed_origins").is_none());
                } else {
                    assert!(rendered_val.get("allowed_origins").is_some());
                    assert!(rendered_val.get("allowed_extensions").is_none());
                }

                if os == "windows" {
                    assert!(!rendered.filename.ends_with(".json"));
                } else {
                    assert!(rendered.filename.ends_with(".json"));
                }
            }
        }
    }
}

#[test]
fn test_registration_config_root_and_paths() {
    let rendered_linux = render_registration("production", "chrome", "linux", Some("/usr/bin/host"), Some("/custom/home")).unwrap();
    assert_eq!(
        rendered_linux.path,
        "/custom/home/.config/google-chrome/NativeMessagingHosts/app.solstone.browser.json"
    );
    assert_eq!(rendered_linux.filename, "app.solstone.browser.json");

    let rendered_win = render_registration("dev", "edge", "windows", Some("C:\\host.exe"), Some("HKCU")).unwrap();
    assert_eq!(
        rendered_win.path,
        "HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts\\app.solstone.browser.dev"
    );
    assert_eq!(rendered_win.filename, "app.solstone.browser.dev");
}
