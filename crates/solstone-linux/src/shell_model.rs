// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Tile words, widths, one attention row per source, and counter phrases. No toolkit.
//! The window calls [`deck_columns`]. A phrase here is a locked word.

use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, Timelike};

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

/// A tile's state word. Every tile shows one. There is no blank word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateWord {
    Off,
    NeedsAttention,
    Paused,
    On,
    SettingUp,
    ReadyToSetUp,
    Checking,
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

/// Whether desktop activity is running now. It runs with or without screen and
/// sound (`activity only` keeps it running alone). A pause holds it unless the
/// owner chose to keep it on through the pause.
#[must_use]
pub fn activity_running(activity_source: bool, paused: bool, through_pause: bool) -> bool {
    activity_source && (!paused || through_pause)
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
    // Needs attention, on, and checking say `with audio and activity` while activity
    // is running, and `with audio` when it is not. Off or paused keeps
    // `activity is still on.` while that intake is running. `activity only` never
    // says `with audio and activity`.
    let sub = if activity_only {
        if activity_running {
            ScreenSub::ActivityStillOn
        } else {
            ScreenSub::WithAudio
        }
    } else if activity_running && matches!(word, StateWord::Off | StateWord::Paused) {
        ScreenSub::ActivityStillOn
    } else if word == StateWord::On && !activity_running {
        ScreenSub::WithAudio
    } else {
        ScreenSub::WithAudioAndActivity
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
        return StateWord::NeedsAttention;
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

/// A browser host reading. `updated_at_ms` is `None` when `status.json` is missing.
/// `seen_report` stays true after a file has been read, including one that later disappears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserHost {
    pub updated_at_ms: Option<i64>,
    pub now_ms: i64,
    pub seen_report: bool,
    pub room_full: bool,
    pub capture_paused: bool,
}

/// Before any report, the word is `checking…`. A missing or stale file after a report
/// is `needs attention`. A timestamp counts as a report, even when it is older than
/// [`RUNNING_STALE_MS`].
#[must_use]
pub fn browsers_tile(host: BrowserHost) -> StateWord {
    let reported = host.seen_report || host.updated_at_ms.is_some();
    if browser_host_up(host.updated_at_ms, host.now_ms) {
        if host.room_full {
            StateWord::NeedsAttention
        } else if host.capture_paused {
            StateWord::Paused
        } else {
            StateWord::On
        }
    } else if reported {
        StateWord::NeedsAttention
    } else {
        StateWord::Checking
    }
}

/// The shipped browsers line, only for a host that is down after it has reported.
#[must_use]
pub fn browser_host_reason(host: BrowserHost) -> Option<&'static str> {
    let down = !browser_host_up(host.updated_at_ms, host.now_ms);
    (down && browsers_tile(host) == StateWord::NeedsAttention)
        .then_some("browser pages can't start right now.")
}

/// The terminal line when the switch is on and the daemon is down.
#[must_use]
pub fn terminal_daemon_reason(switch_on: bool, daemon_up: bool) -> Option<&'static str> {
    (switch_on && !daemon_up).then_some("terminal sessions can't start right now.")
}

/// Shown under technical details when `source-intake.json` is present and will not parse.
pub const INTAKE_UNREADABLE: &str = "the record of how much has been taken in could not be read.";

/// The locked state word, including the ellipsis on `checking…`.
#[must_use]
pub fn state_label(word: StateWord) -> &'static str {
    match word {
        StateWord::Off => "off",
        StateWord::NeedsAttention => "needs attention",
        StateWord::Paused => "paused",
        StateWord::On => "on",
        StateWord::SettingUp => "setting up",
        StateWord::ReadyToSetUp => "ready to set up",
        StateWord::Checking => "checking…",
    }
}

#[must_use]
pub fn screen_sub_label(sub: ScreenSub) -> Option<&'static str> {
    match sub {
        ScreenSub::None => None,
        ScreenSub::WithAudioAndActivity => Some("with audio and activity"),
        ScreenSub::WithAudio => Some("with audio"),
        ScreenSub::ActivityStillOn => Some("activity is still on."),
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

/// Every tile in deck order whose word is `needs attention`.
#[must_use]
pub fn attention_tiles(words: &[StateWord; 4]) -> Vec<TileId> {
    TILE_ORDER
        .into_iter()
        .zip(words)
        .filter(|(_, word)| **word == StateWord::NeedsAttention)
        .map(|(tile, _)| tile)
        .collect()
}

/// The first tile in deck order whose word is `needs attention`.
#[must_use]
pub fn attention_tile(words: &[StateWord; 4]) -> Option<TileId> {
    attention_tiles(words).into_iter().next()
}

/// The header keeps `pause` until this item's own menu is up. A probe error keeps it.
/// Watcher-name ownership is not an input.
#[must_use]
pub fn header_shows_pause(menu_up: bool, probe_error: bool) -> bool {
    probe_error || !menu_up
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuId {
    Attention(TileId),
    Status,
    Pause,
    Resume,
    OpenSettings,
    OpenJournal,
    About,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuRow {
    pub id: MenuId,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuInput {
    pub words: [StateWord; 4],
    pub paused: bool,
    pub journal_paired: bool,
    pub journal_reachable: bool,
}

/// What activating a row does. The status row does nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuTarget {
    Page(TileId),
    Deck,
    Resume,
    Journal,
    Help,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseFor {
    Seconds(u32),
    UntilResume,
}

#[must_use]
pub fn tile_name(tile: TileId) -> &'static str {
    match tile {
        TileId::Screen => "screen",
        TileId::Terminal => "terminal",
        TileId::Browsers => "browsers",
        TileId::Obsidian => "Obsidian",
    }
}

#[must_use]
pub fn attention_label(tile: TileId) -> String {
    format!("{} needs attention", tile_name(tile))
}

/// The panel menu. One attention row per source that needs it, in deck order.
/// Pause or resume, and `open journal` only while paired.
/// There is no source row and no switch row.
#[must_use]
pub fn menu_rows(input: MenuInput) -> Vec<MenuRow> {
    let mut rows = Vec::new();
    for tile in attention_tiles(&input.words) {
        rows.push(MenuRow {
            id: MenuId::Attention(tile),
            enabled: true,
        });
    }
    rows.push(MenuRow {
        id: MenuId::Status,
        enabled: false,
    });
    rows.push(if input.paused {
        MenuRow {
            id: MenuId::Resume,
            enabled: true,
        }
    } else {
        MenuRow {
            id: MenuId::Pause,
            enabled: true,
        }
    });
    rows.push(MenuRow {
        id: MenuId::OpenSettings,
        enabled: true,
    });
    if input.journal_paired {
        rows.push(MenuRow {
            id: MenuId::OpenJournal,
            enabled: input.journal_reachable,
        });
    }
    rows.push(MenuRow {
        id: MenuId::About,
        enabled: true,
    });
    rows.push(MenuRow {
        id: MenuId::Quit,
        enabled: true,
    });
    rows
}

#[must_use]
pub fn menu_target(id: MenuId) -> Option<MenuTarget> {
    match id {
        MenuId::Attention(tile) => Some(MenuTarget::Page(tile)),
        MenuId::Status | MenuId::Pause => None,
        MenuId::Resume => Some(MenuTarget::Resume),
        MenuId::OpenSettings => Some(MenuTarget::Deck),
        MenuId::OpenJournal => Some(MenuTarget::Journal),
        MenuId::About => Some(MenuTarget::Help),
        MenuId::Quit => Some(MenuTarget::Quit),
    }
}

/// Today's pause commands: 15 minutes, 30 minutes, 1 hour, until I resume.
#[must_use]
pub fn pause_menu() -> [(&'static str, PauseFor); 4] {
    [
        ("15 minutes", PauseFor::Seconds(900)),
        ("30 minutes", PauseFor::Seconds(1800)),
        ("1 hour", PauseFor::Seconds(3600)),
        ("until I resume", PauseFor::UntilResume),
    ]
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

#[must_use]
pub fn greeting_now() -> &'static str {
    let now = Local::now();
    let minutes = crate::sun_arc::pinned_sun_minutes(f64::from(now.hour() * 60 + now.minute()));
    let hour = minutes.div_euclid(60.0);
    if (0.0..24.0).contains(&hour) {
        greeting(hour as u32)
    } else {
        greeting(now.hour())
    }
}

#[must_use]
pub fn source_last_now(file: &IntakeFile, last: Option<&str>) -> LastTaken {
    source_last(file, last, Local::now().fixed_offset())
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
/// Phone width is two compact columns. The page replaces the deck there.
#[must_use]
pub fn deck_columns(width: i32) -> u8 {
    match width_class(width) {
        WidthClass::Full => 3,
        WidthClass::Narrower | WidthClass::Phone => 2,
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
    let last = match file {
        IntakeFile::Rows(rows) => rows.screen.last.as_deref(),
        IntakeFile::Missing | IntakeFile::Refused => None,
    };
    source_last(file, last, now)
}

/// `last taken in` for one counter. Absent and a refused file have no phrase.
#[must_use]
pub fn source_last(file: &IntakeFile, last: Option<&str>, now: DateTime<FixedOffset>) -> LastTaken {
    match file {
        IntakeFile::Missing => LastTaken::Absent,
        IntakeFile::Refused => LastTaken::Hidden,
        IntakeFile::Rows(_) => match last {
            Some(last) if !last.is_empty() => LastTaken::When(format_relative(last, now)),
            _ => LastTaken::Absent,
        },
    }
}

#[must_use]
pub fn total_text(total: &ShownTotal) -> Option<&str> {
    match total {
        ShownTotal::NothingYet => Some("nothing yet"),
        ShownTotal::Amount(text) => Some(text),
        ShownTotal::Hidden => None,
    }
}

/// The source page's `so far`, dated from today.
#[must_use]
pub fn page_total_now(total: &ShownTotal, started: Option<&str>) -> Option<String> {
    page_total_text(total, started, Local::now().date_naive())
}

/// The source page's `so far`. A real total names the day it started.
#[must_use]
pub fn page_total_text(
    total: &ShownTotal,
    started: Option<&str>,
    today: NaiveDate,
) -> Option<String> {
    match total {
        ShownTotal::Hidden => None,
        ShownTotal::NothingYet => Some("nothing yet".to_owned()),
        ShownTotal::Amount(text) => match started.and_then(|day| format_started(day, today)) {
            Some(day) => Some(format!("{text}, since {day}")),
            None => Some(text.clone()),
        },
    }
}

#[must_use]
pub fn last_text(last: &LastTaken) -> Option<&str> {
    match last {
        LastTaken::When(text) => Some(text),
        LastTaken::Absent | LastTaken::Hidden => None,
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

/// A unix instant, as the same relative words `last taken in` uses. Absent when the
/// instant is not a real time.
#[must_use]
pub fn relative_unix(at: f64, now: DateTime<FixedOffset>) -> Option<String> {
    if !at.is_finite() {
        return None;
    }
    let whole = at.trunc() as i64;
    let when = DateTime::from_timestamp(whole, 0)?;
    let text = format_relative(&when.to_rfc3339(), now);
    if text.is_empty() { None } else { Some(text) }
}

#[must_use]
pub fn relative_unix_now(at: f64) -> Option<String> {
    relative_unix(at, Local::now().fixed_offset())
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
        let at = now.timestamp() as f64 - 30.0;
        assert_eq!(relative_unix(at, now).as_deref(), Some("just now"));
        assert_eq!(relative_unix(f64::NAN, now), None);
    }

    #[test]
    fn the_page_total_names_the_day_it_started() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        assert_eq!(
            page_total_text(
                &ShownTotal::Amount("212 hours".to_owned()),
                Some("2026-03-03"),
                today
            )
            .as_deref(),
            Some("212 hours, since march 3")
        );
        assert_eq!(
            page_total_text(
                &ShownTotal::Amount("1 hour".to_owned()),
                Some("2025-03-03"),
                today
            )
            .as_deref(),
            Some("1 hour, since march 3, 2025")
        );
        assert_eq!(
            page_total_text(&ShownTotal::NothingYet, None, today).as_deref(),
            Some("nothing yet")
        );
        assert!(page_total_text(&ShownTotal::Hidden, None, today).is_none());
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
    fn a_pause_holds_activity_unless_it_is_kept_on() {
        assert!(activity_running(true, false, false));
        assert!(!activity_running(true, true, false));
        assert!(activity_running(true, true, true));
        assert!(!activity_running(false, true, true));
        // Kept off through the pause, the paused screen tile does not say activity is on.
        let held = screen_tile(
            true,
            false,
            activity_running(true, true, false),
            TrayStatus::Paused,
        );
        assert_eq!(held.word, StateWord::Paused);
        assert_ne!(held.sub, ScreenSub::ActivityStillOn);
        let kept = screen_tile(
            true,
            false,
            activity_running(true, true, true),
            TrayStatus::Paused,
        );
        assert_eq!(kept.sub, ScreenSub::ActivityStillOn);
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
        assert_eq!(paused.sub, ScreenSub::WithAudioAndActivity);
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
        assert_eq!(fault.sub, ScreenSub::WithAudioAndActivity);
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
        // `activity only`: screen and sound are off while activity keeps running, so
        // the tile reads off and says activity is still on.
        let activity = screen_tile(
            true,
            true,
            activity_running(true, false, false),
            TrayStatus::Idle,
        );
        assert_eq!(activity.word, StateWord::Off);
        assert!(activity.switch_on);
        assert_eq!(activity.sub, ScreenSub::ActivityStillOn);
        assert_ne!(activity.sub, ScreenSub::WithAudioAndActivity);
        // With activity itself off, `activity only` leaves nothing running.
        let none = screen_tile(
            true,
            true,
            activity_running(false, false, false),
            TrayStatus::Idle,
        );
        assert_eq!(none.sub, ScreenSub::WithAudio);
    }

    #[test]
    fn a_down_daemon_needs_attention_and_a_browser_host_starts_at_checking() {
        assert_eq!(
            terminal_tile(true, false, EngineState::Running, false, false),
            StateWord::NeedsAttention
        );
        assert_eq!(
            terminal_daemon_reason(true, false),
            Some("terminal sessions can't start right now.")
        );
        assert_eq!(terminal_daemon_reason(false, false), None);
        assert_eq!(terminal_daemon_reason(true, true), None);
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
        let host = |updated_at_ms, seen_report, room_full, capture_paused| BrowserHost {
            updated_at_ms,
            now_ms: 1_000 + RUNNING_STALE_MS,
            seen_report,
            room_full,
            capture_paused,
        };
        let not_yet = host(None, false, false, false);
        assert_eq!(browsers_tile(not_yet), StateWord::Checking);
        assert_eq!(browser_host_reason(not_yet), None);
        let stale = host(Some(1_000), false, false, false);
        assert_eq!(browsers_tile(stale), StateWord::NeedsAttention);
        assert_eq!(
            browser_host_reason(stale),
            Some("browser pages can't start right now.")
        );
        let gone = host(None, true, false, false);
        assert_eq!(browsers_tile(gone), StateWord::NeedsAttention);
        assert_eq!(
            browser_host_reason(gone),
            Some("browser pages can't start right now.")
        );
        let fresh = BrowserHost {
            updated_at_ms: Some(1_000 + RUNNING_STALE_MS),
            now_ms: 1_000 + RUNNING_STALE_MS,
            seen_report: true,
            room_full: false,
            capture_paused: false,
        };
        assert_eq!(browsers_tile(fresh), StateWord::On);
        assert_eq!(browser_host_reason(fresh), None);
        assert_eq!(
            browsers_tile(BrowserHost {
                room_full: true,
                capture_paused: true,
                ..fresh
            }),
            StateWord::NeedsAttention
        );
        assert_eq!(
            browser_host_reason(BrowserHost {
                room_full: true,
                ..fresh
            }),
            None
        );
        assert_eq!(
            browsers_tile(BrowserHost {
                capture_paused: true,
                ..fresh
            }),
            StateWord::Paused
        );
        assert_eq!(state_label(StateWord::Checking), "checking…");
        assert_eq!(state_label(StateWord::NeedsAttention), "needs attention");
        assert!(
            state_label(StateWord::Off)
                .chars()
                .any(|ch| !ch.is_whitespace())
        );
        assert_eq!(
            screen_sub_label(ScreenSub::WithAudioAndActivity),
            Some("with audio and activity")
        );
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
    fn each_source_that_needs_attention_gets_its_own_row() {
        let rows = menu_rows(MenuInput {
            words: [
                StateWord::NeedsAttention,
                StateWord::NeedsAttention,
                StateWord::Off,
                StateWord::Off,
            ],
            paused: false,
            journal_paired: false,
            journal_reachable: true,
        });
        let attention: Vec<_> = rows
            .iter()
            .filter(|row| matches!(row.id, MenuId::Attention(_)))
            .collect();
        assert_eq!(attention.len(), 2);
        assert_eq!(attention[0].id, MenuId::Attention(TileId::Screen));
        assert_eq!(attention[1].id, MenuId::Attention(TileId::Terminal));
        assert_eq!(
            menu_target(attention[0].id),
            Some(MenuTarget::Page(TileId::Screen))
        );
        assert_eq!(
            menu_target(attention[1].id),
            Some(MenuTarget::Page(TileId::Terminal))
        );
        assert_eq!(attention_label(TileId::Screen), "screen needs attention");
        assert_eq!(
            attention_label(TileId::Terminal),
            "terminal needs attention"
        );
        assert!(rows.iter().any(|row| row.id == MenuId::Pause));
        assert!(rows.iter().all(|row| row.id != MenuId::Resume));
        assert!(rows.iter().all(|row| row.id != MenuId::OpenJournal));
        assert!(
            rows.iter()
                .any(|row| row.id == MenuId::Status && !row.enabled)
        );

        let later = menu_rows(MenuInput {
            words: [
                StateWord::On,
                StateWord::NeedsAttention,
                StateWord::NeedsAttention,
                StateWord::Off,
            ],
            paused: true,
            journal_paired: true,
            journal_reachable: false,
        });
        assert_eq!(
            later
                .iter()
                .filter_map(|row| match row.id {
                    MenuId::Attention(tile) => Some(tile),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![TileId::Terminal, TileId::Browsers]
        );
        assert!(later.iter().any(|row| row.id == MenuId::Resume));
        assert!(later.iter().all(|row| row.id != MenuId::Pause));
        let journal = later
            .iter()
            .find(|row| row.id == MenuId::OpenJournal)
            .unwrap();
        assert!(!journal.enabled);
        let open = menu_rows(MenuInput {
            words: [
                StateWord::Off,
                StateWord::Off,
                StateWord::Off,
                StateWord::Off,
            ],
            paused: false,
            journal_paired: true,
            journal_reachable: true,
        });
        assert!(
            open.iter()
                .any(|row| row.id == MenuId::OpenJournal && row.enabled)
        );
        assert!(
            open.iter()
                .all(|row| !matches!(row.id, MenuId::Attention(_)))
        );
        assert_eq!(
            pause_menu().map(|(label, _)| label),
            ["15 minutes", "30 minutes", "1 hour", "until I resume"]
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
        assert_eq!(deck_columns(719), 2);
        assert_eq!(deck_columns(400), 2);
        assert!(shelf_button(400));
        assert!(!shelf_button(1180));
    }
}
