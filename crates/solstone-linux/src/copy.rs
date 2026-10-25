// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub const WINDOW_TITLE: &str = "solstone";
pub const NAV_STATUS: &str = "status";
pub const NAV_JOURNAL: &str = "journal";
pub const NAV_SETTINGS: &str = "settings";
pub const NAV_HELP: &str = "help";
pub const ACTION_OPEN_JOURNAL: &str = "open your journal";
pub const STATUS_ON: &str = "on";
pub const STATUS_ON_DETAIL: &str = "intake is running.";
pub const STATUS_SCREEN_WAITING: &str = "screen intake is waiting";
pub const STATUS_SCREEN_WAITING_DETAIL: &str = "audio intake can continue.";
pub const STATUS_PAUSED: &str = "paused";
pub const STATUS_PAUSED_DETAIL: &str = "intake is paused until you resume.";
pub const STATUS_STARTING: &str = "starting…";
pub const STATUS_OFF: &str = "off";
pub const STATUS_OFF_DETAIL: &str = "intake isn't running.";
pub const STATUS_ATTENTION: &str = "needs attention";
pub const STATUS_UNAVAILABLE: &str = "status unavailable";
pub const STATUS_UNAVAILABLE_DETAIL: &str =
    "couldn't read the solstone app's status. check whether it's running, then try again.";
pub const STATUS_CHECKING: &str = "checking status…";
pub const STATUS_INTAKE_LABEL: &str = "intake";
pub const STATUS_SCREEN_LABEL: &str = "screen";
pub const STATUS_AUDIO_LABEL: &str = "audio";
pub const STATUS_JOURNAL_LABEL: &str = "journal";
pub const STATUS_WAITING_LABEL: &str = "waiting to sync";
pub const STATUS_ATTENTION_LABEL: &str = "need attention";
pub const STATUS_LAST_DELIVERY: &str = "last delivery";
pub const STATUS_VERSION: &str = "app version";
pub const STATUS_DETAILS: &str = "health details";
pub const ACTION_PAUSE: &str = "pause";
pub const ACTION_PAUSING: &str = "pausing…";
pub const ACTION_RESUME: &str = "resume";
pub const ACTION_RESUMING: &str = "resuming…";
pub const ACTION_RETRY: &str = "try again";
pub const ACTION_CHECK_STATUS: &str = "check status";
pub const ACTION_VIEW_JOURNAL: &str = "journal details";
pub const STATUS_PAUSE_FAILED: &str = "couldn't pause intake. try again.";
pub const STATUS_RESUME_FAILED: &str = "couldn't resume intake. try again.";
pub const BROWSER_OPEN_FAILED: &str =
    "couldn't open your browser. try again, or open your journal from its usual address.";
pub const JOURNAL_CONNECTED: &str = "connected";
pub const JOURNAL_OFFLINE: &str = "not reaching your journal right now";
pub const JOURNAL_OFFLINE_DETAIL: &str = "anything waiting to sync stays on this computer. the solstone app will try again when your journal is reachable.";
pub const JOURNAL_NOT_PAIRED: &str = "not paired";
pub const JOURNAL_PAIR_TITLE: &str = "pair a device";
pub const JOURNAL_PAIR_HELP: &str = "open the network app in your journal and get a pairing link.";
pub const JOURNAL_LINK_LABEL: &str = "pairing link";
pub const JOURNAL_LINK_PLACEHOLDER: &str = "paste pairing link";
pub const ACTION_PAIR: &str = "pair a device";
pub const ACTION_PAIRING: &str = "pairing…";
pub const PAIRING_EMPTY: &str = "paste a pairing link from your journal.";
pub const PAIRING_BAD_LINK: &str = "that pairing link isn't one the solstone app can read. get a new pairing link from your journal and try again.";
pub const PAIRING_EXPIRED: &str =
    "the pairing window closed. get a new pairing link from your journal, then try again.";
pub const PAIRING_FAILED: &str =
    "pairing didn't go through. get a new pairing link from your journal and try again.";
pub const PAIRING_HELD: &str = "waiting for you to confirm your journal's mark";
pub const PAIRING_HELD_DETAIL: &str = "nothing waiting goes into your journal until you do.";
pub const PAIRING_REPLACEMENT_HELD_DETAIL: &str =
    "your current pairing stays in use until you confirm this mark.";
pub const PAIRING_QUESTION: &str = "does this match your journal?";
pub const PAIRING_QUESTION_DETAIL: &str =
    "your journal shows this same mark in its network app. it should match, exactly.";
pub const PAIRING_CONFIRM: &str = "yes, this is my journal";
pub const PAIRING_REJECT: &str = "that doesn't match";
pub const PAIRING_CANCEL: &str = "cancel pairing";
pub const PAIRING_CONFIRMING: &str = "confirming…";
pub const PAIRING_SUCCESS: &str = "the solstone app can now connect to your journal.";
pub const PAIRING_REJECTED_TITLE: &str = "not connected";
pub const PAIRING_REJECTED_DETAIL: &str = "you said this mark doesn't match the one your journal shows, so this computer isn't paired. get a fresh pairing link from your journal, or email support@solstone.app and we'll help.";
pub const PAIRING_REPLACEMENT_REJECTED_TITLE: &str = "new pairing cancelled";
pub const PAIRING_REPLACEMENT_REJECTED_DETAIL: &str = "you said this mark doesn't match the one your journal shows. your current pairing stays in use. get a fresh pairing link from your journal, or email support@solstone.app and we'll help.";
pub const PAIRING_CANCELLED: &str =
    "pairing cancelled. get a fresh pairing link from your journal when you're ready.";
pub const PAIRING_REPLACEMENT_CANCELLED: &str =
    "new pairing cancelled. your current pairing stays in use.";
pub const PAIRING_UNAVAILABLE_TITLE: &str = "couldn't verify";
pub const PAIRING_UNAVAILABLE_DETAIL: &str = "couldn't work out your journal's mark, so there's nothing to compare. nothing waiting goes into your journal until you confirm its mark.";
pub const PAIRING_REPLACEMENT_UNAVAILABLE_DETAIL: &str = "couldn't work out this mark, so there's nothing to compare. your current pairing stays in use.";
pub const PAIRING_MARK_UNAVAILABLE: &str = "mark unavailable";
pub const PAIRING_NEW_LINK: &str = "get a fresh link";
pub const PAIRING_STALE: &str = "the pairing changed while you were checking its mark. check the current mark before confirming.";
pub const PAIRING_SAVE_FAILED: &str = "couldn't save your answer. try again.";
pub const PAIRING_OUTCOME_UNAVAILABLE: &str =
    "couldn't check whether pairing finished. check status before trying again.";
pub const PAIRING_CONFIRMED_START_FAILED: &str =
    "your journal's mark is confirmed, but the solstone app couldn't restart. try again.";
pub const PAIRING_CONFIRMED_UNAVAILABLE: &str = "your journal's mark is confirmed, but the solstone app needs attention. check the details, then try again.";
pub const PAIRING_CHECK_STATUS: &str = "check status";
pub const PAIRING_CONFIRM_ACTION: &str = "confirm your journal's mark";
pub const JOURNAL_MARK_LABEL: &str = "mark";
pub const JOURNAL_MARK_SPEECH_TEMPLATE: &str = "{tint1}, {tint2} · {word1}·{word2}";
pub const SETTINGS_HEADING: &str = "settings";
pub const SETTINGS_FRAME_RATE: &str = "screen frame rate";
pub const SETTINGS_FRAME_RATE_HELP: &str = "frames per second · 1–10";
pub const SETTINGS_CURSOR: &str = "draw cursor";
pub const SETTINGS_START_PAUSED: &str = "start paused";
pub const SETTINGS_SEGMENT: &str = "segment interval";
pub const SETTINGS_SEGMENT_HELP: &str = "seconds";
pub const SETTINGS_RESTART_HELP: &str = "these take effect the next time the solstone app starts.";
pub const SETTINGS_SAVE: &str = "save settings";
pub const SETTINGS_SAVING: &str = "saving…";
pub const SETTINGS_SAVED: &str = "settings saved.";
pub const SETTINGS_RESTART_NEEDED: &str = "restart needed";
pub const SETTINGS_RESTART_DETAIL: &str =
    "your settings are saved. restart the solstone app to use them now.";
pub const SETTINGS_RESTART: &str = "restart the solstone app";
pub const SETTINGS_RESTARTING: &str = "restarting…";
pub const SETTINGS_RESTARTED: &str = "the solstone app restarted with your saved settings.";
pub const SETTINGS_RESTART_FAILED: &str =
    "your settings are saved, but the solstone app couldn't restart. try again.";
pub const SETTINGS_RESTART_RECOVERY_FAILED: &str =
    "couldn't restart the solstone app. check the details, then try again.";
pub const SETTINGS_SAVE_FAILED: &str =
    "couldn't save your settings. check the details, then try again.";
pub const SETTINGS_SAVE_INCOMPLETE: &str =
    "couldn't save all your settings. your edits are still here. try again.";
pub const SETTINGS_SAVE_UNAVAILABLE: &str =
    "couldn't check whether all your settings were saved. read them again before trying again.";
pub const SETTINGS_LOAD_FAILED: &str =
    "couldn't read your settings. try again before making changes.";
pub const SETTINGS_FRAME_INVALID: &str = "enter a whole number from 1 to 10.";
pub const SETTINGS_SEGMENT_INVALID: &str = "enter a whole number greater than 0.";
pub const SETTINGS_FILE: &str = "config file";
pub const SETTINGS_CONFLICT: &str =
    "your settings changed outside this window. read them again before saving.";
pub const SETTINGS_RELOAD: &str = "read settings again";
pub const ACTIVITY_WRITE_FAILED: &str = "couldn't save activity intake. try again.";
pub const SETTINGS_ACTIVITY_SOURCE: &str = "desktop activity";
pub const SETTINGS_ACTIVITY_THROUGH_PAUSE: &str = "keep desktop activity on while paused";
pub const SETTINGS_ACTIVITY_THROUGH_PAUSE_HELP: &str =
    "screen and audio still pause. desktop activity keeps going.";
pub const SETTINGS_ACTIVITY_TITLES: &str = "window titles";
pub const SETTINGS_ACTIVITY_URLS: &str = "web addresses (turns on desktop accessibility)";
pub const SETTINGS_ACTIVITY_ONLY: &str = "activity only";
pub const SETTINGS_ACTIVITY_ONLY_HELP: &str =
    "desktop activity only, with screen and audio intake off.";
pub const SETTINGS_ACTIVITY_PRIVATE_WINDOWS_NOTE: &str = "linux does not classify private windows.";
pub const SETTINGS_ACTIVITY_EXCLUDE_APPS: &str = "apps left out of desktop activity";
pub const SETTINGS_ACTIVITY_EXCLUDE_TITLES: &str = "window titles left out of desktop activity";
pub const SETTINGS_ACTIVITY_ROUTE: &str = "activity route";
pub const SETTINGS_ACTIVITY_EXPLAIN: &str = "desktop activity is on whenever the solstone app takes in your screen, and pausing pauses both. it takes in the app, window title, web addresses, and active or idle time you share with it, and all of it goes into your journal.";
pub const HELP_HEADING: &str = "get help";
pub const HELP_INTRO: &str = "need a hand? reach a human. we're happy to help.";
pub const HELP_OPEN: &str = "get help";
pub const HELP_EMAIL: &str = "support@solstone.app";
pub const LOGS_TITLE: &str = "recent logs";
pub const LOGS_INTRO: &str = "read the solstone app's recent logs before saving a copy for a support report. nothing is sent from here.";
pub const LOGS_READ: &str = "read recent logs";
pub const LOGS_READING: &str = "reading logs…";
pub const LOGS_READY: &str = "read the logs below, then choose whether to save a copy.";
pub const LOGS_PARTIAL: &str =
    "available logs are shown below. check the notes for gaps before saving a copy.";
pub const LOGS_EMPTY: &str = "no recent logs are available yet. try again after using the app.";
pub const LOGS_MISSING: &str = "no log files are available yet. try again after using the app.";
pub const LOGS_UNREADABLE: &str = "couldn't read the log files. try again.";
pub const LOGS_READ_FAILED: &str = "couldn't read the logs. try again.";
pub const LOGS_SAVE: &str = "save a copy…";
pub const LOGS_SAVE_HELP: &str = "saving writes exactly the preview above.";
pub const LOGS_SAVING: &str = "saving copy…";
pub const LOGS_SAVED: &str = "copy saved.";
pub const LOGS_SAVE_FAILED: &str = "couldn't save the copy. try saving to another folder.";
pub const LOGS_FILENAME: &str = "solstone-logs.txt";
pub const ACTION_START: &str = "start the solstone app";
pub const ACTION_STARTING: &str = "starting…";
pub const STATUS_START_DISCLOSURE: &str = "this starts the solstone app now and at sign-in. opening this window alone doesn't start intake.";
pub const STATUS_START_WAITING: &str = "waiting for the solstone app to answer. if your desktop asks what to share, make your choice there.";
pub const STATUS_START_UNAVAILABLE: &str =
    "the solstone app hasn't answered yet. check for a desktop sharing prompt, then check status.";
pub const STATUS_START_FAILED: &str =
    "couldn't start the solstone app. check the details, then try again.";
pub const LAUNCH_FAILURE_TITLE: &str = "couldn't open the solstone window";
pub const LAUNCH_RUNTIME_UNAVAILABLE_TEMPLATE: &str = "the window needs GTK 4.6 or newer. couldn't load its desktop libraries. open {path} for details.";
pub const LAUNCH_FAILED_TEMPLATE: &str = "the window couldn't start. open {path} for details.";
pub const LAUNCH_FAILED_WITHOUT_DIAGNOSTIC: &str =
    "the window couldn't start. try opening it again.";
pub const LAUNCH_RETAINED_DIAGNOSTIC_TEMPLATE: &str =
    "couldn't open the solstone window. open {path} for details.";

pub const NOT_YET: &str = "not yet";

pub enum PluralKey {
    Attention,
    OmittedBytes,
}

pub fn plural_copy(key: PluralKey, count: usize, formatted_count: &str) -> String {
    let template = match (key, count == 1) {
        (PluralKey::Attention, true) => "{count} needs attention",
        (PluralKey::Attention, false) => "{count} need attention",
        (PluralKey::OmittedBytes, true) => "\n[omitted {count} byte]\n",
        (PluralKey::OmittedBytes, false) => "\n[omitted {count} bytes]\n",
    };
    template.replace("{count}", formatted_count)
}

pub enum ValueKey {
    Waiting,
    LastDelivery,
    Version,
}
pub fn value_copy(key: ValueKey, value: &str) -> String {
    let template = match key {
        ValueKey::Waiting => "waiting to sync: {value}",
        ValueKey::LastDelivery => "last delivery: {value}",
        ValueKey::Version => "app version: {value}",
    };
    template.replace("{value}", value)
}

pub fn journal_mark_speech(tint1: &str, tint2: &str, word1: &str, word2: &str) -> String {
    format!("{tint1}, {tint2} · {word1}·{word2}")
}

pub fn journal_mark_label(word1: &str, word2: &str) -> String {
    format!("{word1}·{word2}")
}

pub fn launch_runtime_unavailable(path: &str) -> String {
    format!(
        "the window needs GTK 4.6 or newer. couldn't load its desktop libraries. open {path} for details."
    )
}

pub fn launch_failed(path: &str) -> String {
    format!("the window couldn't start. open {path} for details.")
}

pub fn launch_retained_diagnostic(path: &str) -> String {
    format!("couldn't open the solstone window. open {path} for details.")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CopyKey {
    WindowTitle,
    NavStatus,
    NavJournal,
    NavSettings,
    NavHelp,
    ActionOpenJournal,
    StatusOn,
    StatusOnDetail,
    StatusScreenWaiting,
    StatusScreenWaitingDetail,
    StatusPaused,
    StatusPausedDetail,
    StatusStarting,
    StatusOff,
    StatusOffDetail,
    StatusAttention,
    StatusUnavailable,
    StatusUnavailableDetail,
    StatusChecking,
    StatusIntakeLabel,
    StatusScreenLabel,
    StatusAudioLabel,
    StatusJournalLabel,
    StatusWaitingLabel,
    StatusAttentionLabel,
    StatusLastDelivery,
    StatusVersion,
    StatusDetails,
    ActionPause,
    ActionPausing,
    ActionResume,
    ActionResuming,
    ActionRetry,
    ActionCheckStatus,
    ActionViewJournal,
    StatusPauseFailed,
    StatusResumeFailed,
    StatusPauseUnavailable,
    StatusResumeUnavailable,
    BrowserOpenFailed,
    JournalConnected,
    JournalOffline,
    JournalOfflineDetail,
    JournalNotPaired,
    JournalPairTitle,
    JournalPairHelp,
    JournalLinkLabel,
    JournalLinkPlaceholder,
    ActionPair,
    ActionPairing,
    PairingEmpty,
    PairingBadLink,
    PairingExpired,
    PairingFailed,
    PairingHeld,
    PairingHeldDetail,
    PairingReplacementHeldDetail,
    PairingQuestion,
    PairingQuestionDetail,
    PairingConfirm,
    PairingReject,
    PairingCancel,
    PairingConfirming,
    PairingSuccess,
    PairingRejectedTitle,
    PairingRejectedDetail,
    PairingReplacementRejectedTitle,
    PairingReplacementRejectedDetail,
    PairingCancelled,
    PairingReplacementCancelled,
    PairingUnavailableTitle,
    PairingUnavailableDetail,
    PairingReplacementUnavailableDetail,
    PairingMarkUnavailable,
    PairingNewLink,
    PairingStale,
    PairingSaveFailed,
    PairingOutcomeUnavailable,
    PairingConfirmedStartFailed,
    PairingConfirmedUnavailable,
    PairingCheckStatus,
    PairingConfirmAction,
    JournalMarkLabel,
    SettingsHeading,
    SettingsFrameRate,
    SettingsFrameRateHelp,
    SettingsCursor,
    SettingsStartPaused,
    SettingsSegment,
    SettingsSegmentHelp,
    SettingsRestartHelp,
    SettingsSave,
    SettingsSaving,
    SettingsSaved,
    SettingsRestartNeeded,
    SettingsRestartDetail,
    SettingsRestart,
    SettingsRestarting,
    SettingsRestarted,
    SettingsRestartFailed,
    SettingsRestartRecoveryFailed,
    SettingsSaveFailed,
    SettingsSaveIncomplete,
    SettingsSaveUnavailable,
    SettingsLoadFailed,
    SettingsFrameInvalid,
    SettingsSegmentInvalid,
    SettingsFile,
    SettingsConflict,
    SettingsReload,
    ActivityWriteFailed,
    SettingsActivitySource,
    SettingsActivityThroughPause,
    SettingsActivityThroughPauseHelp,
    SettingsActivityTitles,
    SettingsActivityUrls,
    SettingsActivityOnly,
    SettingsActivityOnlyHelp,
    SettingsActivityPrivateWindowsNote,
    SettingsActivityExcludeApps,
    SettingsActivityExcludeTitles,
    SettingsActivityRoute,
    SettingsActivityExplain,
    HelpHeading,
    HelpIntro,
    HelpOpen,
    HelpEmail,
    LogsTitle,
    LogsIntro,
    LogsRead,
    LogsReading,
    LogsReady,
    LogsPartial,
    LogsEmpty,
    LogsMissing,
    LogsUnreadable,
    LogsReadFailed,
    LogsSave,
    LogsSaveHelp,
    LogsSaving,
    LogsSaved,
    LogsSaveFailed,
    LogsFilename,
    LogsGap,
    MarkGenericFirst,
    MarkGenericSecond,
    MarkGenericSpeech,
    MarkUnavailableFirst,
    MarkUnavailableSecond,
    MarkUnavailableSpeech,
    HelpLinkFailed,
    ActionStart,
    ActionStarting,
    StatusStartDisclosure,
    StatusStartWaiting,
    StatusStartUnavailable,
    StatusStartFailed,
    LaunchFailureTitle,
    LaunchFailedWithoutDiagnostic,
}

impl CopyKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WindowTitle => WINDOW_TITLE,
            Self::NavStatus => NAV_STATUS,
            Self::NavJournal => NAV_JOURNAL,
            Self::NavSettings => NAV_SETTINGS,
            Self::NavHelp => NAV_HELP,
            Self::ActionOpenJournal => ACTION_OPEN_JOURNAL,
            Self::StatusOn => STATUS_ON,
            Self::StatusOnDetail => STATUS_ON_DETAIL,
            Self::StatusScreenWaiting => STATUS_SCREEN_WAITING,
            Self::StatusScreenWaitingDetail => STATUS_SCREEN_WAITING_DETAIL,
            Self::StatusPaused => STATUS_PAUSED,
            Self::StatusPausedDetail => STATUS_PAUSED_DETAIL,
            Self::StatusStarting => STATUS_STARTING,
            Self::StatusOff => STATUS_OFF,
            Self::StatusOffDetail => STATUS_OFF_DETAIL,
            Self::StatusAttention => STATUS_ATTENTION,
            Self::StatusUnavailable => STATUS_UNAVAILABLE,
            Self::StatusUnavailableDetail => STATUS_UNAVAILABLE_DETAIL,
            Self::StatusChecking => STATUS_CHECKING,
            Self::StatusIntakeLabel => STATUS_INTAKE_LABEL,
            Self::StatusScreenLabel => STATUS_SCREEN_LABEL,
            Self::StatusAudioLabel => STATUS_AUDIO_LABEL,
            Self::StatusJournalLabel => STATUS_JOURNAL_LABEL,
            Self::StatusWaitingLabel => STATUS_WAITING_LABEL,
            Self::StatusAttentionLabel => STATUS_ATTENTION_LABEL,
            Self::StatusLastDelivery => STATUS_LAST_DELIVERY,
            Self::StatusVersion => STATUS_VERSION,
            Self::StatusDetails => STATUS_DETAILS,
            Self::ActionPause => ACTION_PAUSE,
            Self::ActionPausing => ACTION_PAUSING,
            Self::ActionResume => ACTION_RESUME,
            Self::ActionResuming => ACTION_RESUMING,
            Self::ActionRetry => ACTION_RETRY,
            Self::ActionCheckStatus => ACTION_CHECK_STATUS,
            Self::ActionViewJournal => ACTION_VIEW_JOURNAL,
            Self::StatusPauseFailed => STATUS_PAUSE_FAILED,
            Self::StatusResumeFailed => STATUS_RESUME_FAILED,
            Self::StatusPauseUnavailable => {
                "couldn't check whether intake paused. check status before trying again."
            }
            Self::StatusResumeUnavailable => {
                "couldn't check whether intake resumed. check status before trying again."
            }
            Self::BrowserOpenFailed => BROWSER_OPEN_FAILED,
            Self::JournalConnected => JOURNAL_CONNECTED,
            Self::JournalOffline => JOURNAL_OFFLINE,
            Self::JournalOfflineDetail => JOURNAL_OFFLINE_DETAIL,
            Self::JournalNotPaired => JOURNAL_NOT_PAIRED,
            Self::JournalPairTitle => JOURNAL_PAIR_TITLE,
            Self::JournalPairHelp => JOURNAL_PAIR_HELP,
            Self::JournalLinkLabel => JOURNAL_LINK_LABEL,
            Self::JournalLinkPlaceholder => JOURNAL_LINK_PLACEHOLDER,
            Self::ActionPair => ACTION_PAIR,
            Self::ActionPairing => ACTION_PAIRING,
            Self::PairingEmpty => PAIRING_EMPTY,
            Self::PairingBadLink => PAIRING_BAD_LINK,
            Self::PairingExpired => PAIRING_EXPIRED,
            Self::PairingFailed => PAIRING_FAILED,
            Self::PairingHeld => PAIRING_HELD,
            Self::PairingHeldDetail => PAIRING_HELD_DETAIL,
            Self::PairingReplacementHeldDetail => PAIRING_REPLACEMENT_HELD_DETAIL,
            Self::PairingQuestion => PAIRING_QUESTION,
            Self::PairingQuestionDetail => PAIRING_QUESTION_DETAIL,
            Self::PairingConfirm => PAIRING_CONFIRM,
            Self::PairingReject => PAIRING_REJECT,
            Self::PairingCancel => PAIRING_CANCEL,
            Self::PairingConfirming => PAIRING_CONFIRMING,
            Self::PairingSuccess => PAIRING_SUCCESS,
            Self::PairingRejectedTitle => PAIRING_REJECTED_TITLE,
            Self::PairingRejectedDetail => PAIRING_REJECTED_DETAIL,
            Self::PairingReplacementRejectedTitle => PAIRING_REPLACEMENT_REJECTED_TITLE,
            Self::PairingReplacementRejectedDetail => PAIRING_REPLACEMENT_REJECTED_DETAIL,
            Self::PairingCancelled => PAIRING_CANCELLED,
            Self::PairingReplacementCancelled => PAIRING_REPLACEMENT_CANCELLED,
            Self::PairingUnavailableTitle => PAIRING_UNAVAILABLE_TITLE,
            Self::PairingUnavailableDetail => PAIRING_UNAVAILABLE_DETAIL,
            Self::PairingReplacementUnavailableDetail => PAIRING_REPLACEMENT_UNAVAILABLE_DETAIL,
            Self::PairingMarkUnavailable => PAIRING_MARK_UNAVAILABLE,
            Self::PairingNewLink => PAIRING_NEW_LINK,
            Self::PairingStale => PAIRING_STALE,
            Self::PairingSaveFailed => PAIRING_SAVE_FAILED,
            Self::PairingOutcomeUnavailable => PAIRING_OUTCOME_UNAVAILABLE,
            Self::PairingConfirmedStartFailed => PAIRING_CONFIRMED_START_FAILED,
            Self::PairingConfirmedUnavailable => PAIRING_CONFIRMED_UNAVAILABLE,
            Self::PairingCheckStatus => PAIRING_CHECK_STATUS,
            Self::PairingConfirmAction => PAIRING_CONFIRM_ACTION,
            Self::JournalMarkLabel => JOURNAL_MARK_LABEL,
            Self::SettingsHeading => SETTINGS_HEADING,
            Self::SettingsFrameRate => SETTINGS_FRAME_RATE,
            Self::SettingsFrameRateHelp => SETTINGS_FRAME_RATE_HELP,
            Self::SettingsCursor => SETTINGS_CURSOR,
            Self::SettingsStartPaused => SETTINGS_START_PAUSED,
            Self::SettingsSegment => SETTINGS_SEGMENT,
            Self::SettingsSegmentHelp => SETTINGS_SEGMENT_HELP,
            Self::SettingsRestartHelp => SETTINGS_RESTART_HELP,
            Self::SettingsSave => SETTINGS_SAVE,
            Self::SettingsSaving => SETTINGS_SAVING,
            Self::SettingsSaved => SETTINGS_SAVED,
            Self::SettingsRestartNeeded => SETTINGS_RESTART_NEEDED,
            Self::SettingsRestartDetail => SETTINGS_RESTART_DETAIL,
            Self::SettingsRestart => SETTINGS_RESTART,
            Self::SettingsRestarting => SETTINGS_RESTARTING,
            Self::SettingsRestarted => SETTINGS_RESTARTED,
            Self::SettingsRestartFailed => SETTINGS_RESTART_FAILED,
            Self::SettingsRestartRecoveryFailed => SETTINGS_RESTART_RECOVERY_FAILED,
            Self::SettingsSaveFailed => SETTINGS_SAVE_FAILED,
            Self::SettingsSaveIncomplete => SETTINGS_SAVE_INCOMPLETE,
            Self::SettingsSaveUnavailable => SETTINGS_SAVE_UNAVAILABLE,
            Self::SettingsLoadFailed => SETTINGS_LOAD_FAILED,
            Self::SettingsFrameInvalid => SETTINGS_FRAME_INVALID,
            Self::SettingsSegmentInvalid => SETTINGS_SEGMENT_INVALID,
            Self::SettingsFile => SETTINGS_FILE,
            Self::SettingsConflict => SETTINGS_CONFLICT,
            Self::SettingsReload => SETTINGS_RELOAD,
            Self::ActivityWriteFailed => ACTIVITY_WRITE_FAILED,
            Self::SettingsActivitySource => SETTINGS_ACTIVITY_SOURCE,
            Self::SettingsActivityThroughPause => SETTINGS_ACTIVITY_THROUGH_PAUSE,
            Self::SettingsActivityThroughPauseHelp => SETTINGS_ACTIVITY_THROUGH_PAUSE_HELP,
            Self::SettingsActivityTitles => SETTINGS_ACTIVITY_TITLES,
            Self::SettingsActivityUrls => SETTINGS_ACTIVITY_URLS,
            Self::SettingsActivityOnly => SETTINGS_ACTIVITY_ONLY,
            Self::SettingsActivityOnlyHelp => SETTINGS_ACTIVITY_ONLY_HELP,
            Self::SettingsActivityPrivateWindowsNote => SETTINGS_ACTIVITY_PRIVATE_WINDOWS_NOTE,
            Self::SettingsActivityExcludeApps => SETTINGS_ACTIVITY_EXCLUDE_APPS,
            Self::SettingsActivityExcludeTitles => SETTINGS_ACTIVITY_EXCLUDE_TITLES,
            Self::SettingsActivityRoute => SETTINGS_ACTIVITY_ROUTE,
            Self::SettingsActivityExplain => SETTINGS_ACTIVITY_EXPLAIN,
            Self::HelpHeading => HELP_HEADING,
            Self::HelpIntro => HELP_INTRO,
            Self::HelpOpen => HELP_OPEN,
            Self::HelpEmail => HELP_EMAIL,
            Self::LogsTitle => LOGS_TITLE,
            Self::LogsIntro => LOGS_INTRO,
            Self::LogsRead => LOGS_READ,
            Self::LogsReading => LOGS_READING,
            Self::LogsReady => LOGS_READY,
            Self::LogsPartial => LOGS_PARTIAL,
            Self::LogsEmpty => LOGS_EMPTY,
            Self::LogsMissing => LOGS_MISSING,
            Self::LogsUnreadable => LOGS_UNREADABLE,
            Self::LogsReadFailed => LOGS_READ_FAILED,
            Self::LogsSave => LOGS_SAVE,
            Self::LogsSaveHelp => LOGS_SAVE_HELP,
            Self::LogsSaving => LOGS_SAVING,
            Self::LogsSaved => LOGS_SAVED,
            Self::LogsSaveFailed => LOGS_SAVE_FAILED,
            Self::LogsFilename => LOGS_FILENAME,
            Self::LogsGap => "\n[the log preview was limited; more log text may be available]\n",
            Self::MarkGenericFirst => "your",
            Self::MarkGenericSecond => "journal",
            Self::MarkGenericSpeech => "your journal, not set up yet",
            Self::MarkUnavailableFirst => "mark",
            Self::MarkUnavailableSecond => "unavailable",
            Self::MarkUnavailableSpeech => "your journal's mark, unavailable right now",
            Self::HelpLinkFailed => {
                "couldn't open that link. visit support.solstone.app or email support@solstone.app."
            }
            Self::ActionStart => ACTION_START,
            Self::ActionStarting => ACTION_STARTING,
            Self::StatusStartDisclosure => STATUS_START_DISCLOSURE,
            Self::StatusStartWaiting => STATUS_START_WAITING,
            Self::StatusStartUnavailable => STATUS_START_UNAVAILABLE,
            Self::StatusStartFailed => STATUS_START_FAILED,
            Self::LaunchFailureTitle => LAUNCH_FAILURE_TITLE,
            Self::LaunchFailedWithoutDiagnostic => LAUNCH_FAILED_WITHOUT_DIAGNOSTIC,
        }
    }
}
