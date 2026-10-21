// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod about;
pub mod activity;
pub mod activity_address;
pub mod activity_helper_install;
pub mod activity_helpers;
pub mod activity_live;
pub mod activity_prefs;
pub mod activity_source;
pub mod activity_writer;
pub mod audio;
pub mod bounded_process;
pub mod browser;
pub mod capture_stats;
pub mod chunking;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod copy;
pub mod dbus_client;
pub mod dbus_service;
pub mod desktop_component;
pub mod desktop_session;
pub mod doctor;
pub mod encoding;
mod journal_mark;
pub mod matching;
pub mod observer;
pub mod panel_icon;
pub mod pause_hold;
pub mod pipeline;
pub mod positions;
mod private_file;
mod private_link;
mod private_link_access;
mod private_link_metadata;
mod private_link_optional;
pub mod recovery;
pub mod restore_token;
pub mod rotation;
pub mod run;
pub mod segment;
pub mod service;
pub mod session_env;
mod shell;
pub mod sources;
pub mod streams;
pub mod subscription;
pub mod support;
pub mod sync;
pub mod sync_health;
pub mod tray;
pub mod tray_model;
pub mod upload;
pub mod video;
pub mod window_control;

#[cfg(test)]
mod docs_policy_tests;
#[cfg(test)]
mod linked_authority_policy_tests;
#[cfg(test)]
mod observer_contract_tests;
#[cfg(test)]
mod policy_test_support;
#[cfg(test)]
mod private_link_test_peer;
#[cfg(all(test, target_arch = "x86_64"))]
mod release_installer_tests;
#[cfg(test)]
mod release_rail_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod toolchain_policy_tests;
#[cfg(test)]
mod unsafe_policy_tests;

#[cfg(test)]
mod about_tests;
