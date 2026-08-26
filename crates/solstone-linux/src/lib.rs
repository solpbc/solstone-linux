// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod activity;
pub mod audio;
pub mod capture_stats;
pub mod chunking;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod dbus_service;
pub mod desktop_component;
pub mod doctor;
pub mod encoding;
pub mod matching;
pub mod observer;
pub mod pipeline;
pub mod positions;
mod private_file;
mod private_link;
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
pub mod sync;
pub mod sync_health;
pub mod tray;
pub mod tray_model;
pub mod upload;
pub mod video;

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
#[cfg(test)]
mod release_rail_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod toolchain_policy_tests;
#[cfg(test)]
mod unsafe_policy_tests;
