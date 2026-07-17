// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod audio;
pub mod capture_stats;
pub mod chunking;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod dbus_service;
pub mod desktop_component;
pub mod encoding;
pub mod event_sender;
pub mod matching;
pub mod observer;
pub mod pipeline;
pub mod positions;
pub mod recovery;
pub mod restore_token;
pub mod rotation;
pub mod run;
pub mod segment;
pub mod session_env;
pub mod sources;
pub mod streams;
pub mod subscription;
pub mod sync;
pub mod sync_health;
pub mod tray;
pub mod tray_model;
pub mod upload;
pub mod video;

#[cfg(test)]
mod test_support;
