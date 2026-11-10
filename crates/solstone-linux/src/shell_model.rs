// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Tile words, widths, the one attention row, and counter phrases. No toolkit.
//! The window calls [`deck_columns`]. A phrase here is a locked word.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};

use crate::browser::status::RUNNING_STALE_MS;
use crate::obsidian::State as VaultState;
use crate::source_intake::{Counter, IntakeFile, IntakeRows};
use crate::terminal::state::EngineState;
use crate::tray_model::TrayStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileId {
    Screen,
    Terminal,
    Browsers,
    Obsidian,
}

const TILE_ORDER: [TileId; 4] = [
    TileId::Screen,
    TileId::Terminal,
    TileId::Browsers,
    TileId::Obsidian,
];

/// A tile's state word. [`StateWord::Absent`] is the host-down reading: no word is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateWord {
    Off,
    NeedsAttention,
    Paused,
    On,
    SettingUp,
    ReadyToSetUp,
    Checking,
    Absent,
}

/// Which screen sub-line applies. The sentences live in the copy file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenSub {
    None,
    WithAudioAndActivity,
    WithAudio,
    ActivityStillOn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenTile {
    pub word: StateWord,
    /// Bound to `desktop_capture`. It can be on while [`Self::word`] is off.
    pub switch_on: bool,
    pub sub: ScreenSub,
}

/// What a press of the screen switch writes. It does not write `activity_only`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSwitchWrite {
    pub desktop_capture: bool,
    pub writes_activity_only: bool,
}

#[must_use]
pub fn screen_switch_write(turn_on: bool) -> ScreenSwitchWrite {
    ScreenSwitchWrite {
        desktop_capture: turn_on,
        writes_activity_only: false,
    }
}

#[must_use]
pub fn screen_tile(
    desktop_capture: bool,
    activity_only: bool,
    activity_running: bool,
    status: TrayStatus,
) -> ScreenTile {
    let switch_on = desktop_capture;
    let word = if activity_only || !desktop_capture {
        StateWord::Off
    } else if status.needs_attention() {
        StateWord::NeedsAttention
    } else if matches!(status, TrayStatus::Paused) {
        StateWord::Paused
    } else if matches!(
        status,
        TrayStatus::Recording | TrayStatus::Idle | TrayStatus::Waiting(_)
    ) {
        StateWord::On
    } else {
        StateWord::Off
    };
    let sub = if activity_running && matches!(word, StateWord::Off | StateWord::Paused) {
        ScreenSub::ActivityStillOn
    } else if word == StateWord::On && activity_running {
        ScreenSub::WithAudioAndActivity
    } else if word == StateWord::On {
        ScreenSub::WithAudio
    } else {
        ScreenSub::None
    };
    ScreenTile {
        word,
        switch_on,
        sub,
    }
}

#[must_use]
pub fn terminal_tile(
    switch_on: bool,
    daemon_up: bool,
    state: EngineState,
    restart_error: bool,
    app_paused: bool,
) -> StateWord {
    if !switch_on {
        return StateWord::Off;
    }
    if !daemon_up {
        return StateWord::Absent;
    }
    let word = match state {
        EngineState::Off => StateWord::Off,
        EngineState::Paused => StateWord::Paused,
        EngineState::Running => StateWord::On,
        EngineState::TmuxNotFound => StateWord::NeedsAttention,
        EngineState::WaitingForTmuxApp => StateWord::ReadyToSetUp,
        EngineState::Restarting if restart_error => StateWord::NeedsAttention,
        EngineState::Restarting => StateWord::Checking,
    };
    if app_paused && word == StateWord::On {
        StateWord::Paused
    } else {
        word
    }
}

/// `updated_at_ms` is `None` when `status.json` is missing. Older than
/// [`RUNNING_STALE_MS`] is down.
#[must_use]
pub fn browser_host_up(updated_at_ms: Option<i64>, now_ms: i64) -> bool {
    updated_at_ms.is_some_and(|at| now_ms - at < RUNNING_STALE_MS)
}

/// Connected browser names, in the order given. None when none are connected.
#[must_use]
pub fn browsers_subline(names: &[&str]) -> Option<String> {
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

#[must_use]
pub fn browsers_tile(host_up: bool, room_full: bool, capture_paused: bool) -> StateWord {
    if !host_up {
        StateWord::Absent
    } else if room_full {
        StateWord::NeedsAttention
    } else if capture_paused {
        StateWord::Paused
    } else {
        StateWord::On
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObsidianFacts {
    pub state: VaultState,
    pub vaults_found: bool,
    pub vault_chosen: bool,
}

#[must_use]
pub fn obsidian_tile(facts: ObsidianFacts) -> StateWord {
    match facts.state {
        VaultState::Off if facts.vaults_found && !facts.vault_chosen => StateWord::ReadyToSetUp,
        VaultState::Off => StateWord::Off,
        VaultState::Checking => StateWord::Checking,
        VaultState::Confirm => StateWord::ReadyToSetUp,
        VaultState::On => StateWord::On,
        VaultState::Paused => StateWord::Paused,
        VaultState::WaitingForJournalUpdate => StateWord::SettingUp,
        VaultState::NeedsAttention => StateWord::NeedsAttention,
    }
}

/// The first tile in deck order whose word is `needs attention`.
#[must_use]
pub fn attention_tile(words: &[StateWord; 4]) -> Option<TileId> {
    TILE_ORDER
        .into_iter()
        .zip(words)
        .find(|(_, word)| **word == StateWord::NeedsAttention)
        .map(|(tile, _)| tile)
}

/// The header keeps `pause` until this item's own menu is up. A probe error keeps it.
/// Watcher-name ownership is not an input.
#[must_use]
pub fn header_shows_pause(menu_up: bool, probe_error: bool) -> bool {
    probe_error || !menu_up
}

/// Device-local hour. The bands are morning 5–11, afternoon 12–16, evening otherwise.
#[must_use]
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "good morning",
        12..=16 => "good afternoon",
        _ => "good evening",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseLeft {
    Seconds(u32),
    UntilResume,
}

/// The paused line. A timed pause rounds whole minutes up. Under a minute stays the short line.
#[must_use]
pub fn pause_line(left: PauseLeft) -> String {
    match left {
        PauseLeft::UntilResume => "paused until you resume".to_string(),
        PauseLeft::Seconds(seconds) if seconds < 60 => {
            "paused, less than a minute left".to_string()
        }
        PauseLeft::Seconds(seconds) => {
            let minutes = seconds.div_ceil(60);
            format!("paused, {minutes} min left")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthClass {
    Full,
    Narrower,
    Phone,
}

#[must_use]
pub fn width_class(width: i32) -> WidthClass {
    if width >= 1100 {
        WidthClass::Full
    } else if width >= 720 {
        WidthClass::Narrower
    } else {
        WidthClass::Phone
    }
}

/// Deck columns from the width alone. Opening a page does not change this.
#[must_use]
pub fn deck_columns(width: i32) -> u8 {
    match width_class(width) {
        WidthClass::Full => 3,
        WidthClass::Narrower => 2,
        WidthClass::Phone => 1,
    }
}

/// Narrower and phone keep the button that opens the shelf.
#[must_use]
pub fn shelf_button(width: i32) -> bool {
    width_class(width) != WidthClass::Full
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShownTotal {
    NothingYet,
    Amount(String),
    Hidden,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastTaken {
    Absent,
    When(String),
    Hidden,
}

#[must_use]
pub fn screen_so_far(file: &IntakeFile) -> ShownTotal {
    so_far(file, |rows| &rows.screen, TotalUnit::Seconds)
}

#[must_use]
pub fn audio_so_far(file: &IntakeFile) -> ShownTotal {
    so_far(file, |rows| &rows.audio, TotalUnit::Seconds)
}

#[must_use]
pub fn terminal_so_far(file: &IntakeFile) -> ShownTotal {
    so_far(file, |rows| &rows.terminal, TotalUnit::Seconds)
}

#[must_use]
pub fn browsers_so_far(file: &IntakeFile) -> ShownTotal {
    so_far(file, |rows| &rows.browsers, TotalUnit::Pages)
}

#[must_use]
pub fn obsidian_so_far(file: &IntakeFile) -> ShownTotal {
    so_far(file, |rows| &rows.obsidian, TotalUnit::Notes)
}

fn so_far(
    file: &IntakeFile,
    pick: impl Fn(&IntakeRows) -> &Counter,
    unit: TotalUnit,
) -> ShownTotal {
    match file {
        IntakeFile::Missing => ShownTotal::NothingYet,
        IntakeFile::Refused => ShownTotal::Hidden,
        IntakeFile::Rows(rows) => {
            let counter = pick(rows);
            if counter.total == 0 {
                ShownTotal::NothingYet
            } else {
                ShownTotal::Amount(format_total(counter.total, unit))
            }
        }
    }
}

#[must_use]
pub fn screen_last(file: &IntakeFile, now: DateTime<FixedOffset>) -> LastTaken {
    match file {
        IntakeFile::Missing => LastTaken::Absent,
        IntakeFile::Refused => LastTaken::Hidden,
        IntakeFile::Rows(rows) => match rows.screen.last.as_deref() {
            Some(last) => LastTaken::When(format_relative(last, now)),
            None => LastTaken::Absent,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TotalUnit {
    Seconds,
    Pages,
    Notes,
}

fn format_total(total: u64, unit: TotalUnit) -> String {
    match unit {
        TotalUnit::Seconds => format_seconds(total),
        TotalUnit::Pages => format_count(total, "page", "pages"),
        TotalUnit::Notes => format_count(total, "note", "notes"),
    }
}

fn format_seconds(total: u64) -> String {
    if total < 60 {
        "less than a minute".to_owned()
    } else if total < 3600 {
        let minutes = total / 60;
        if minutes == 1 {
            "1 minute".to_owned()
        } else {
            format!("{minutes} minutes")
        }
    } else {
        let hours = total / 3600;
        if hours == 1 {
            "1 hour".to_owned()
        } else {
            format!("{} hours", grouped(hours))
        }
    }
}

fn format_count(total: u64, one: &str, many: &str) -> String {
    if total == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", grouped(total))
    }
}

/// English grouping. The locked phrases are English, so this does not follow
/// the process locale.
fn grouped(n: u64) -> String {
    let raw = n.to_string();
    let mut out = String::new();
    for (index, ch) in raw.char_indices() {
        let remaining = raw.len() - index;
        if index > 0 && remaining.is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[must_use]
pub fn format_started(started: &str, today: NaiveDate) -> Option<String> {
    let date = NaiveDate::parse_from_str(started, "%Y-%m-%d").ok()?;
    Some(format_date(date, today))
}

fn format_date(date: NaiveDate, today: NaiveDate) -> String {
    let month = month_name(date.month());
    if date.year() == today.year() {
        format!("{month} {}", date.day())
    } else {
        format!("{month} {}, {}", date.day(), date.year())
    }
}

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "january",
        2 => "february",
        3 => "march",
        4 => "april",
        5 => "may",
        6 => "june",
        7 => "july",
        8 => "august",
        9 => "september",
        10 => "october",
        11 => "november",
        _ => "december",
    }
}

#[must_use]
pub fn format_relative(last: &str, now: DateTime<FixedOffset>) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(last) else {
        return String::new();
    };
    let age = now.signed_duration_since(parsed).num_seconds();
    if age < 60 {
        return "just now".to_owned();
    }
    if age < 3600 {
        return format!("{} min ago", age / 60);
    }
    if age < 86_400 {
        let hours = age / 3600;
        return if hours == 1 {
            "1 hour ago".to_owned()
        } else {
            format!("{hours} hours ago")
        };
    }
    let last_local = parsed.with_timezone(now.offset());
    let today = now.date_naive();
    let last_day = last_local.date_naive();
    if today.pred_opt() == Some(last_day) {
        "yesterday".to_owned()
    } else {
        format_date(last_day, today)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_session::SessionPresence;
    use crate::source_intake::Counter;
    use chrono::{Duration, TimeZone};

    fn rows(screen: u64, audio: u64, screen_last: Option<&str>) -> IntakeFile {
        IntakeFile::Rows(Box::new(IntakeRows {
            audio: Counter {
                total: audio,
                started: Some("2026-01-01".to_owned()),
                last: None,
            },
            screen: Counter {
                total: screen,
                started: Some("2025-03-03".to_owned()),
                last: screen_last.map(str::to_owned),
            },
            terminal: Counter {
                total: 0,
                started: None,
                last: None,
            },
            browsers: Counter {
                total: 0,
                started: None,
                last: None,
            },
            obsidian: Counter {
                total: 0,
                started: None,
                last: None,
            },
        }))
    }

    #[test]
    fn screen_status_reads_the_screen_key_and_a_null_last_is_absent() {
        let swapped = rows(0, 3600, None);
        assert_eq!(screen_so_far(&swapped), ShownTotal::NothingYet);
        assert_eq!(
            audio_so_far(&swapped),
            ShownTotal::Amount("1 hour".to_owned())
        );
        let screen = rows(3600, 0, None);
        assert_eq!(
            screen_so_far(&screen),
            ShownTotal::Amount("1 hour".to_owned())
        );
        let now = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 10, 15, 0, 0)
            .unwrap();
        assert_eq!(screen_last(&screen, now), LastTaken::Absent);
        assert_eq!(screen_so_far(&IntakeFile::Missing), ShownTotal::NothingYet);
        assert_eq!(screen_so_far(&IntakeFile::Refused), ShownTotal::Hidden);
    }

    #[test]
    fn minute_floors_and_the_started_date() {
        assert_eq!(format_seconds(1), "less than a minute");
        assert_eq!(format_seconds(59), "less than a minute");
        assert_eq!(format_seconds(60), "1 minute");
        assert_eq!(format_seconds(90), "1 minute");
        assert_eq!(format_seconds(3599), "59 minutes");
        assert_eq!(format_seconds(3600), "1 hour");
        assert_eq!(format_seconds(7200), "2 hours");
        assert_eq!(format_seconds(3_600_000), "1,000 hours");
        assert_eq!(format_seconds(9_424_800), "2,618 hours");
        let today = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        assert_eq!(
            format_started("2025-03-03", today).as_deref(),
            Some("march 3, 2025")
        );
        assert_eq!(
            format_started("2026-03-03", today).as_deref(),
            Some("march 3")
        );
    }

    #[test]
    fn pages_and_notes_are_singular_at_one() {
        let empty = || Counter {
            total: 0,
            started: None,
            last: None,
        };
        let one = IntakeFile::Rows(Box::new(IntakeRows {
            audio: empty(),
            screen: empty(),
            terminal: empty(),
            browsers: Counter {
                total: 1,
                started: None,
                last: None,
            },
            obsidian: Counter {
                total: 1,
                started: None,
                last: None,
            },
        }));
        assert_eq!(
            browsers_so_far(&one),
            ShownTotal::Amount("1 page".to_owned())
        );
        assert_eq!(
            obsidian_so_far(&one),
            ShownTotal::Amount("1 note".to_owned())
        );
        let many = IntakeFile::Rows(Box::new(IntakeRows {
            audio: empty(),
            screen: empty(),
            terminal: Counter {
                total: 0,
                started: None,
                last: None,
            },
            browsers: Counter {
                total: 2618,
                started: None,
                last: None,
            },
            obsidian: Counter {
                total: 412,
                started: None,
                last: None,
            },
        }));
        assert_eq!(
            browsers_so_far(&many),
            ShownTotal::Amount("2,618 pages".to_owned())
        );
        assert_eq!(
            obsidian_so_far(&many),
            ShownTotal::Amount("412 notes".to_owned())
        );
        assert_eq!(terminal_so_far(&many), ShownTotal::NothingYet);
    }

    #[test]
    fn relative_phrases_follow_the_locked_order() {
        let zone = FixedOffset::east_opt(-7 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 10, 15, 0, 0).unwrap();
        let phrase =
            |seconds: i64| format_relative(&(now - Duration::seconds(seconds)).to_rfc3339(), now);
        assert_eq!(phrase(30), "just now");
        assert_eq!(phrase(90), "1 min ago");
        assert_eq!(phrase(3600), "1 hour ago");
        assert_eq!(phrase(86_400), "yesterday");
    }

    #[test]
    fn the_screen_switch_writes_desktop_capture_only() {
        assert_eq!(
            screen_switch_write(true),
            ScreenSwitchWrite {
                desktop_capture: true,
                writes_activity_only: false,
            }
        );
        assert!(!screen_switch_write(false).desktop_capture);
    }

    #[test]
    fn screen_words_follow_the_switch_and_the_fault() {
        let idle = screen_tile(true, false, true, TrayStatus::Idle);
        assert_eq!(idle.word, StateWord::On);
        assert_eq!(idle.sub, ScreenSub::WithAudioAndActivity);
        let quiet = screen_tile(true, false, false, TrayStatus::Idle);
        assert_eq!(quiet.sub, ScreenSub::WithAudio);
        let away = screen_tile(
            true,
            false,
            false,
            TrayStatus::Waiting(SessionPresence::NoDesktop),
        );
        assert_eq!(away.word, StateWord::On);
        let recording = screen_tile(true, false, false, TrayStatus::Recording);
        assert_eq!(recording.word, StateWord::On);
        assert_eq!(recording.sub, ScreenSub::WithAudio);
        let paused = screen_tile(true, false, false, TrayStatus::Paused);
        assert_eq!(paused.word, StateWord::Paused);
        assert_eq!(paused.sub, ScreenSub::None);
        let paused_activity = screen_tile(true, false, true, TrayStatus::Paused);
        assert_eq!(paused_activity.word, StateWord::Paused);
        assert_eq!(paused_activity.sub, ScreenSub::ActivityStillOn);
        let off_while_paused = screen_tile(false, false, true, TrayStatus::Paused);
        assert_eq!(off_while_paused.word, StateWord::Off);
        assert_eq!(off_while_paused.sub, ScreenSub::ActivityStillOn);
        let fault = screen_tile(
            true,
            false,
            false,
            TrayStatus::Waiting(SessionPresence::Unreachable),
        );
        assert_eq!(fault.word, StateWord::NeedsAttention);
        let login = screen_tile(
            true,
            false,
            false,
            TrayStatus::Waiting(SessionPresence::LoginUnavailable),
        );
        assert_eq!(login.word, StateWord::NeedsAttention);
        let held = screen_tile(
            false,
            false,
            false,
            TrayStatus::Waiting(SessionPresence::Unreachable),
        );
        assert_eq!(held.word, StateWord::Off);
        let activity = screen_tile(true, true, true, TrayStatus::Idle);
        assert_eq!(activity.word, StateWord::Off);
        assert!(activity.switch_on);
        assert_eq!(activity.sub, ScreenSub::ActivityStillOn);
        assert_ne!(activity.sub, ScreenSub::WithAudioAndActivity);
    }

    #[test]
    fn a_down_daemon_or_a_stale_browser_host_has_no_word() {
        assert_eq!(
            terminal_tile(true, false, EngineState::Running, false, false),
            StateWord::Absent
        );
        assert_eq!(
            terminal_tile(false, false, EngineState::Running, false, false),
            StateWord::Off
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::Off, false, false),
            StateWord::Off
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::Paused, false, false),
            StateWord::Paused
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::Running, false, false),
            StateWord::On
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::Running, false, true),
            StateWord::Paused
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::TmuxNotFound, false, true),
            StateWord::NeedsAttention
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::WaitingForTmuxApp, false, false),
            StateWord::ReadyToSetUp
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::Restarting, false, false),
            StateWord::Checking
        );
        assert_eq!(
            terminal_tile(true, true, EngineState::Restarting, true, false),
            StateWord::NeedsAttention
        );
        assert!(!browser_host_up(None, 1_000));
        assert!(!browser_host_up(Some(1_000), 1_000 + RUNNING_STALE_MS));
        assert!(browser_host_up(Some(1_000), 1_000 + RUNNING_STALE_MS - 1));
        assert_eq!(browsers_subline(&[]), None);
        assert_eq!(browsers_subline(&["Chrome"]).as_deref(), Some("Chrome"));
        assert_eq!(
            browsers_subline(&["Chrome", "Firefox"]).as_deref(),
            Some("Chrome, Firefox")
        );
        assert_eq!(browsers_tile(false, false, false), StateWord::Absent);
        assert_eq!(browsers_tile(true, true, true), StateWord::NeedsAttention);
        assert_eq!(browsers_tile(true, false, true), StateWord::Paused);
        assert_eq!(browsers_tile(true, false, false), StateWord::On);
    }

    #[test]
    fn obsidian_off_splits_three_ways_and_attention_is_one_row() {
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::Off,
                vaults_found: true,
                vault_chosen: false,
            }),
            StateWord::ReadyToSetUp
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::Off,
                vaults_found: false,
                vault_chosen: false,
            }),
            StateWord::Off
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::Off,
                vaults_found: true,
                vault_chosen: true,
            }),
            StateWord::Off
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::On,
                vaults_found: true,
                vault_chosen: true,
            }),
            StateWord::On
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::Checking,
                vaults_found: true,
                vault_chosen: false,
            }),
            StateWord::Checking
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::Confirm,
                vaults_found: true,
                vault_chosen: false,
            }),
            StateWord::ReadyToSetUp
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::Paused,
                vaults_found: true,
                vault_chosen: true,
            }),
            StateWord::Paused
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::WaitingForJournalUpdate,
                vaults_found: true,
                vault_chosen: true,
            }),
            StateWord::SettingUp
        );
        assert_eq!(
            obsidian_tile(ObsidianFacts {
                state: VaultState::NeedsAttention,
                vaults_found: true,
                vault_chosen: true,
            }),
            StateWord::NeedsAttention
        );
        let words = [
            StateWord::On,
            StateWord::NeedsAttention,
            StateWord::NeedsAttention,
            StateWord::Off,
        ];
        assert_eq!(attention_tile(&words), Some(TileId::Terminal));
        assert!(header_shows_pause(false, false));
        assert!(header_shows_pause(true, true));
        assert!(!header_shows_pause(true, false));
    }

    #[test]
    fn greeting_bands_and_the_pause_line() {
        assert_eq!(greeting(4), "good evening");
        assert_eq!(greeting(5), "good morning");
        assert_eq!(greeting(11), "good morning");
        assert_eq!(greeting(12), "good afternoon");
        assert_eq!(greeting(16), "good afternoon");
        assert_eq!(greeting(17), "good evening");
        assert_eq!(pause_line(PauseLeft::Seconds(61)), "paused, 2 min left");
        assert_eq!(
            pause_line(PauseLeft::Seconds(59)),
            "paused, less than a minute left"
        );
        assert_eq!(pause_line(PauseLeft::Seconds(60)), "paused, 1 min left");
        assert_eq!(
            pause_line(PauseLeft::UntilResume),
            "paused until you resume"
        );
    }

    #[test]
    fn columns_follow_the_width_and_the_phone_keeps_the_shelf_button() {
        assert_eq!(width_class(1100), WidthClass::Full);
        assert_eq!(width_class(1099), WidthClass::Narrower);
        assert_eq!(width_class(720), WidthClass::Narrower);
        assert_eq!(width_class(719), WidthClass::Phone);
        assert_eq!(deck_columns(1180), 3);
        assert_eq!(deck_columns(1100), 3);
        assert_eq!(deck_columns(1099), 2);
        assert_eq!(deck_columns(720), 2);
        assert_eq!(deck_columns(719), 1);
        assert_eq!(deck_columns(400), 1);
        assert!(shelf_button(400));
        assert!(!shelf_button(1180));
    }
}
