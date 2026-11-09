// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The Obsidian vault source: the engine shared by the desktop apps.
//!
//! A scan compares a vault against the table of what was last established, yields
//! new, modified, renamed and deleted notes, and turns them into the note contract's
//! lines, coalesced per note per window, held until the window closes and then
//! delivered oldest window first. It is read-only against the vault and carries no
//! journal identity in anything it holds. It knows nothing about any app's window,
//! pairing or transport: the caller drives it, delivers each file it offers, and
//! reports the journal's answer.

pub mod clock;
pub mod discover;
pub mod engine;
pub mod outbox;
pub mod pending;
pub mod platform;
pub mod scan;
mod store;
pub mod table;
pub mod wire;

pub use clock::{Clock, Stamp, SystemClock, Window};
pub use discover::{FoundVault, Refusal, check_vault_folder, found_vaults, linux_candidates};
pub use engine::{
    Attention, Engine, ListedVault, Phase, ScanOutcome, State, Status, next_scan_delay,
};
pub use outbox::Delivery;
pub use wire::{Answer, Change, Line, MAX_FILE_BYTES, MAX_FILE_LINES, MAX_NOTE_BYTES};

#[cfg(test)]
mod tests;
