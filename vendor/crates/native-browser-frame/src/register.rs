// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::constants::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRender {
    pub channel: String,
    pub browser: String,
    pub os: String,
    pub host: String,
    pub filename: String,
    pub path: String,
    pub json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    UnknownChannel,
    UnknownBrowser,
    UnknownOs,
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::UnknownChannel => write!(f, "unknown_channel"),
            RegisterError::UnknownBrowser => write!(f, "unknown_browser"),
            RegisterError::UnknownOs => write!(f, "unknown_os"),
        }
    }
}

impl std::error::Error for RegisterError {}

pub fn render_registration(
    channel: &str,
    browser: &str,
    os: &str,
    exec_path: Option<&str>,
    config_root: Option<&str>,
) -> Result<RegistrationRender, RegisterError> {
    let (host, chrome_id, edge_id, firefox_id) = match channel {
        "production" => (PROD_HOST, PROD_CHROME_ID, PROD_EDGE_ID, PROD_FIREFOX_ID),
        "dev" => (DEV_HOST, DEV_CHROME_ID, DEV_EDGE_ID, DEV_FIREFOX_ID),
        _ => return Err(RegisterError::UnknownChannel),
    };

    let id = match browser {
        "chrome" => chrome_id,
        "edge" => edge_id,
        "firefox" => firefox_id,
        _ => return Err(RegisterError::UnknownBrowser),
    };

    let suffix_template = match (browser, os) {
        ("chrome", "linux") => ".config/google-chrome/NativeMessagingHosts/<host>.json",
        ("edge", "linux") => ".config/microsoft-edge/NativeMessagingHosts/<host>.json",
        ("firefox", "linux") => ".mozilla/native-messaging-hosts/<host>.json",
        ("chrome", "macos") => "Library/Application Support/Google/Chrome/NativeMessagingHosts/<host>.json",
        ("edge", "macos") => "Library/Application Support/Microsoft Edge/NativeMessagingHosts/<host>.json",
        ("firefox", "macos") => "Library/Application Support/Mozilla/NativeMessagingHosts/<host>.json",
        ("chrome", "windows") => "Software\\Google\\Chrome\\NativeMessagingHosts\\<host>",
        ("edge", "windows") => "Software\\Microsoft\\Edge\\NativeMessagingHosts\\<host>",
        ("firefox", "windows") => "Software\\Mozilla\\NativeMessagingHosts\\<host>",
        _ => return Err(RegisterError::UnknownOs),
    };

    let path_val = exec_path.unwrap_or(REGISTRATION_PATH_PLACEHOLDER);
    let mut manifest = serde_json::Map::new();
    manifest.insert("name".to_string(), serde_json::Value::String(host.to_string()));
    manifest.insert("description".to_string(), serde_json::Value::String(REGISTRATION_DESCRIPTION.to_string()));
    manifest.insert("path".to_string(), serde_json::Value::String(path_val.to_string()));
    manifest.insert("type".to_string(), serde_json::Value::String(REGISTRATION_TYPE.to_string()));

    if browser == "firefox" {
        manifest.insert("allowed_extensions".to_string(), serde_json::json!([id]));
    } else {
        manifest.insert("allowed_origins".to_string(), serde_json::json!([format!("chrome-extension://{}/", id)]));
    }

    let suffix = suffix_template.replace("<host>", host);
    let full_path = if let Some(root) = config_root {
        let trimmed = root.trim_end_matches(['/', '\\']);
        let sep = if os == "windows" { "\\" } else { "/" };
        format!("{}{}{}", trimmed, sep, suffix)
    } else {
        suffix
    };

    let filename = if os == "windows" {
        host.to_string()
    } else {
        format!("{}.json", host)
    };

    let json_val = serde_json::Value::Object(manifest);
    let mut json_str = serde_json::to_string_pretty(&json_val).unwrap();
    json_str.push('\n');

    Ok(RegistrationRender {
        channel: channel.to_string(),
        browser: browser.to_string(),
        os: os.to_string(),
        host: host.to_string(),
        filename,
        path: full_path,
        json: json_str,
    })
}
