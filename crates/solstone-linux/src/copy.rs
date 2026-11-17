// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_i18n::{message, text};

pub fn journal_link_invalid_title() -> &'static str {
    text!("linux.copy.journallinkinvalidtitle")
}
pub fn journal_link_invalid_body() -> &'static str {
    text!("linux.copy.journallinkinvalidbody")
}

pub fn obsidian_heading() -> &'static str {
    text!("linux.copy.obsidian")
}
pub fn obsidian_explain() -> &'static str {
    text!("linux.copy.solstoneapptakesnotesone")
}
pub fn obsidian_found_label() -> &'static str {
    text!("linux.copy.vaultsthiscomputer")
}
pub fn obsidian_none_found() -> &'static str {
    text!("linux.copy.solstoneappdidntfind")
}
pub fn obsidian_use_this() -> &'static str {
    text!("linux.copy.usethisvault")
}
pub fn obsidian_choose_folder() -> &'static str {
    text!("linux.copy.choosefolder")
}
pub fn obsidian_chooser_title() -> &'static str {
    text!("linux.copy.chooseobsidianvault")
}
pub fn obsidian_chooser_accept() -> &'static str {
    text!("linux.copy.choose")
}
pub fn obsidian_cancel() -> &'static str {
    text!("linux.pair.cancel")
}
pub fn obsidian_refused_not_a_vault() -> &'static str {
    text!("linux.copy.thatfolderisntobsidian")
}
pub fn obsidian_refused_name() -> &'static str {
    text!("linux.copy.thatfoldersnamecan")
}
pub fn obsidian_warning_heading() -> &'static str {
    text!("linux.copy.vaultwiththisnameis")
}
pub fn obsidian_continue() -> &'static str {
    text!("linux.pair.continue")
}
pub fn obsidian_state_checking() -> &'static str {
    text!("linux.copy.waitingreachjournal")
}
pub fn obsidian_state_update() -> &'static str {
    text!("linux.copy.waitingjournalupdate")
}
pub fn obsidian_detail_update() -> &'static str {
    text!("linux.copy.journalneedsupdatebeforeit")
}
pub fn obsidian_vault_label() -> &'static str {
    text!("linux.copy.vault")
}
pub fn obsidian_last_checked() -> &'static str {
    text!("linux.copy.lastchecked")
}
pub fn obsidian_not_on_device() -> &'static str {
    text!("linux.copy.notthisdevice")
}
pub fn obsidian_too_large() -> &'static str {
    text!("linux.copy.toolargebring")
}
pub fn obsidian_attention_vault() -> &'static str {
    text!("linux.copy.solstoneappcantread")
}
pub fn obsidian_attention_part() -> &'static str {
    text!("linux.copy.partthisvaultcant")
}
pub fn obsidian_attention_file() -> &'static str {
    text!("linux.copy.somenotescouldntgo")
}
pub fn obsidian_attention_storage() -> &'static str {
    text!("linux.copy.solstoneappcouldntsave")
}
pub fn obsidian_turn_off() -> &'static str {
    text!("linux.copy.turnoff")
}
pub fn obsidian_turn_off_note() -> &'static str {
    text!("linux.copy.turningthisoffstopsbringing")
}

/// Shown before the first send when the journal already has this vault's name.
pub fn obsidian_warning_detail(vault: &str, device: Option<&str>, date: Option<&str>) -> String {
    match (device, date) {
        (None, None) => message!("linux.copy.obsidianwarning", "vault" => vault),
        (Some(device), None) => {
            message!("linux.copy.obsidianwarningdevice", "vault" => vault, "device" => device)
        }
        (None, Some(date)) => {
            message!("linux.copy.obsidianwarningdate", "vault" => vault, "date" => date)
        }
        (Some(device), Some(date)) => {
            message!("linux.copy.obsidianwarningdevicedate", "vault" => vault, "device" => device, "date" => date)
        }
    }
}

/// How much is waiting on this computer.
pub fn obsidian_waiting(count: &str) -> String {
    message!("linux.copy.countwaitingsync", "count" => count)
}

/// First-sync progress.
pub fn obsidian_progress(sent: &str, found: &str) -> String {
    message!("linux.copy.sentfoundnotesjournal", "sent" => sent, "found" => found)
}

/// The screenshots source's owner strings, each named for its id in the locked
/// screenshots copy. Used as they stand.
pub mod screenshots {
    use super::{message, text};

    /// S1, D11's label.
    pub fn s1() -> &'static str {
        text!("linux.copy.s1")
    }
    /// S4: `from {folder}`, the tile while on.
    pub fn s4(folder: &str) -> String {
        message!("linux.copy.from", "folder" => folder)
    }
    /// S5: the tile while what the owner chose is still going in.
    pub fn s5(done: &str, total: &str) -> String {
        message!("linux.copy.ofinyourjournal", "done" => done, "total" => total)
    }
    /// S6: the page, same moment.
    pub fn s6(done: &str, total: &str) -> String {
        message!("linux.copy.ofscreenshotsinyourjournal", "done" => done, "total" => total)
    }
    /// S7: the how-far-back heading.
    pub fn s7() -> &'static str {
        text!("linux.copy.s7")
    }
    /// S8: the five choices, in calendar's order; the first is preselected.
    pub fn s8() -> [&'static str; 5] {
        [
            text!("linux.copy.screenshotslookback76"),
            text!("linux.copy.screenshotslookback77"),
            text!("linux.copy.screenshotslookback78"),
            text!("linux.copy.screenshotslookback79"),
            text!("linux.copy.screenshotslookback80"),
        ]
    }
    /// S9: under the five choices.
    pub fn s9() -> &'static str {
        text!("linux.copy.s9")
    }
    /// S10: the confirm button.
    pub fn s10(count: u64, formatted: &str) -> String {
        if count == 0 {
            return s10_today().to_owned();
        }
        solstone_i18n::cardinal(
            "linux.screenshots.bringcount",
            count.min(i64::MAX as u64) as i64,
            &[("formatted", solstone_i18n::Value::String(formatted))],
        )
    }
    /// S10, for `nothing from before today` or a count of zero.
    pub fn s10_today() -> &'static str {
        text!("linux.copy.s10today")
    }
    /// S10 while counting: the locked state word.
    pub fn s10_counting() -> &'static str {
        text!("linux.copy.s10counting")
    }
    /// S11: the tile at `ready to set up`, before the confirm.
    pub fn s11() -> &'static str {
        text!("linux.copy.s11")
    }
    /// S12: the count failed, and its button.
    pub fn s12() -> &'static str {
        text!("linux.copy.s12")
    }
    pub fn s12_action() -> &'static str {
        text!("linux.copy.tryagain")
    }
    /// S14, on a computer: `the folder`.
    pub fn s14() -> &'static str {
        text!("linux.copy.s14")
    }
    /// S15.
    pub fn s15() -> &'static str {
        text!("linux.copy.s15")
    }
    /// S16 under a computer's whole pause: its second sentence alone.
    pub fn s16_computer() -> &'static str {
        text!("linux.copy.s16computer")
    }
    /// S17: under the five choices when the source is turned back on.
    pub fn s17() -> &'static str {
        text!("linux.copy.s17")
    }
    /// S18.
    pub fn s18() -> &'static str {
        text!("linux.copy.lastchecked")
    }
    /// S19, shown only while above zero.
    pub fn s19() -> &'static str {
        text!("linux.copy.s19")
    }
    /// S20: the `so far` total.
    pub fn s20(count: u64, formatted: &str) -> String {
        solstone_i18n::cardinal(
            "linux.screenshots.total",
            count.min(i64::MAX as u64) as i64,
            &[("formatted", solstone_i18n::Value::String(formatted))],
        )
    }
    /// S21 on linux: `this computer`.
    pub fn s21() -> &'static str {
        text!("linux.copy.s21")
    }
    /// S22.
    pub fn s22() -> &'static str {
        text!("linux.copy.s22")
    }
    /// S23: the facts block, never collapsed.
    pub fn s23() -> &'static str {
        text!("linux.copy.s23")
    }
    /// D1 on linux: `this computer`.
    pub fn d1() -> &'static str {
        text!("linux.copy.d1")
    }
    /// D3: a folder the owner chose, by its own name.
    pub fn d3(folder: &str) -> String {
        message!("linux.copy.otherfilesinstaywheretheyare", "folder" => folder)
    }
    /// D8, and its button.
    pub fn d8() -> &'static str {
        text!("linux.copy.d8")
    }
    pub fn choose_folder() -> &'static str {
        text!("linux.copy.choosefolder.v4cba918b")
    }
    /// D8's second sentence without its period: the folder dialog's title.
    pub fn d8_picker_title() -> &'static str {
        text!("linux.copy.d8pickertitle")
    }
    /// D9: the folder is gone, by its own name.
    pub fn d9(folder: &str) -> String {
        message!("linux.copy.thefolderisntonthiscomputer", "folder" => folder)
    }
    /// D10, in the collapsed detail.
    pub fn d10() -> &'static str {
        text!("linux.copy.d10")
    }
    /// D12: screenshots that reached the final state.
    pub fn d12(count: u64, formatted: &str) -> String {
        solstone_i18n::cardinal(
            "linux.screenshots.refusedcount",
            count.min(i64::MAX as u64) as i64,
            &[("formatted", solstone_i18n::Value::String(formatted))],
        )
    }
    /// D13: screenshots in the folder that can't be read, and S12's button.
    pub fn d13(count: u64, formatted: &str) -> String {
        solstone_i18n::cardinal(
            "linux.screenshots.unreadablecount",
            count.min(i64::MAX as u64) as i64,
            &[("formatted", solstone_i18n::Value::String(formatted))],
        )
    }
    /// D15: the disclosure that holds D10.
    pub fn d15() -> &'static str {
        text!("linux.copy.d15")
    }
}

pub fn window_title() -> &'static str {
    text!("linux.metadata.name")
}
pub fn nav_status() -> &'static str {
    text!("linux.shared.copy.status")
}
pub fn nav_journal() -> &'static str {
    text!("linux.copy.journal")
}
pub fn nav_settings() -> &'static str {
    text!("linux.copy.settings")
}
pub fn nav_help() -> &'static str {
    text!("linux.copy.help")
}
pub fn action_open_journal() -> &'static str {
    text!("linux.copy.openyourjournal")
}
pub fn status_on() -> &'static str {
    text!("linux.copy.on")
}
pub fn status_on_detail() -> &'static str {
    text!("linux.copy.intakeisrunning")
}
pub fn status_screen_waiting() -> &'static str {
    text!("linux.copy.screenintakeiswaiting")
}
pub fn status_screen_waiting_detail() -> &'static str {
    text!("linux.copy.audiointakecancontinue")
}
pub fn status_paused() -> &'static str {
    text!("linux.copy.paused")
}
pub fn status_paused_detail() -> &'static str {
    text!("linux.copy.intakeispauseduntilyouresume")
}
pub fn status_paused_activity_on_detail() -> &'static str {
    text!("linux.copy.screenandaudioarepauseduntilyouresumedesktop")
}
pub fn status_starting() -> &'static str {
    text!("linux.copy.starting")
}
pub fn status_off() -> &'static str {
    text!("linux.copy.off")
}
pub fn status_off_detail() -> &'static str {
    text!("linux.copy.intakeisntrunning")
}
pub fn status_attention() -> &'static str {
    text!("linux.copy.needsattention")
}
pub fn status_unavailable() -> &'static str {
    text!("linux.copy.statusunavailable")
}
pub fn status_unavailable_detail() -> &'static str {
    text!("linux.copy.couldntreadthesolstoneappsstatuscheckwhethe")
}
pub fn status_checking() -> &'static str {
    text!("linux.copy.checkingstatus")
}
pub fn status_intake_label() -> &'static str {
    text!("linux.copy.intake")
}
pub fn status_screen_label() -> &'static str {
    text!("linux.copy.screen")
}
pub fn status_audio_label() -> &'static str {
    text!("linux.copy.audio")
}
pub fn status_journal_label() -> &'static str {
    text!("linux.copy.journal")
}
pub fn status_waiting_label() -> &'static str {
    text!("linux.copy.waitingtosync")
}
pub fn status_attention_label() -> &'static str {
    text!("linux.copy.needattention")
}
pub fn status_last_delivery() -> &'static str {
    text!("linux.copy.lastdelivery")
}
pub fn status_version() -> &'static str {
    text!("linux.copy.appversion")
}
pub fn status_details() -> &'static str {
    text!("linux.copy.healthdetails")
}
pub fn action_pause() -> &'static str {
    text!("linux.copy.pause")
}
pub fn action_pausing() -> &'static str {
    text!("linux.copy.pausing")
}
pub fn action_resume() -> &'static str {
    text!("linux.copy.resume")
}
pub fn action_resuming() -> &'static str {
    text!("linux.copy.resuming")
}
pub fn action_retry() -> &'static str {
    text!("linux.copy.tryagain")
}
pub fn action_check_status() -> &'static str {
    text!("linux.copy.checkstatus")
}
pub fn action_view_journal() -> &'static str {
    text!("linux.copy.journaldetails")
}
pub fn status_pause_failed() -> &'static str {
    text!("linux.copy.couldntpauseintaketryagain")
}
pub fn status_resume_failed() -> &'static str {
    text!("linux.copy.couldntresumeintaketryagain")
}
pub fn browser_open_failed() -> &'static str {
    text!("linux.copy.couldntopenyourbrowsertryagainoropenyourjo")
}
pub fn journal_connected() -> &'static str {
    text!("linux.shared.copy.connected")
}
pub fn journal_syncing() -> &'static str {
    text!("linux.copy.syncingjournal")
}
pub fn journal_offline() -> &'static str {
    text!("linux.copy.notreachingyourjournalrightnow")
}
pub fn journal_offline_detail() -> &'static str {
    text!("linux.copy.anythingwaitingtosyncstaysonthiscomputerthe")
}
pub fn journal_attention_detail() -> &'static str {
    text!("linux.copy.anythingwaitingsyncstaysthis")
}
pub fn journal_not_paired() -> &'static str {
    text!("linux.copy.notpaired")
}
pub fn local_journal_found() -> &'static str {
    text!("linux.copy.foundjournalthiscomputer")
}
pub fn local_journal_connect() -> &'static str {
    text!("linux.copy.connectjournal")
}
pub fn local_journal_connecting() -> &'static str {
    text!("linux.copy.connecting")
}
pub fn local_journal_connected_to() -> &'static str {
    text!("linux.copy.connected")
}
pub fn local_journal_connect_failed() -> &'static str {
    text!("linux.copy.couldntverifyjournalso")
}
pub fn journal_pair_title() -> &'static str {
    text!("linux.copy.pairadevice")
}
pub fn journal_pair_help() -> &'static str {
    text!("linux.copy.openthenetworkappinyourjournalandgetapairi")
}
pub fn journal_link_label() -> &'static str {
    text!("linux.copy.pairinglink")
}
pub fn journal_link_placeholder() -> &'static str {
    text!("linux.copy.pastepairinglink")
}
pub fn action_pair() -> &'static str {
    text!("linux.copy.pairadevice")
}
pub fn action_pairing() -> &'static str {
    text!("linux.copy.pairing")
}
pub fn pairing_empty() -> &'static str {
    text!("linux.copy.pasteapairinglinkfromyourjournal")
}
pub fn pairing_bad_link() -> &'static str {
    text!("linux.copy.thatpairinglinkisntonethesolstoneappcanrea")
}
pub fn pairing_expired() -> &'static str {
    text!("linux.copy.thepairingwindowclosedgetanewpairinglinkfro")
}
pub fn pairing_failed() -> &'static str {
    text!("linux.copy.pairingdidntgothroughgetanewpairinglinkfro")
}
pub fn pairing_held() -> &'static str {
    text!("linux.copy.waitingforyoutoconfirmyourjournalsmark")
}
pub fn pairing_held_detail() -> &'static str {
    text!("linux.copy.nothingwaitinggoesintoyourjournaluntilyoudo")
}
pub fn pairing_held_run() -> &'static str {
    text!("linux.pair.whenyourereadyrunsolstonelinuxconfirm")
}
pub fn pairing_replacement_held_detail() -> &'static str {
    text!("linux.copy.yourcurrentpairingstaysinuseuntilyouconfirm")
}
pub fn pairing_question() -> &'static str {
    text!("linux.copy.doesthismatchyourjournal")
}
pub fn pairing_question_detail() -> &'static str {
    text!("linux.shared.copy.yourjournalshowsthissamemarkinitsnetworkapp")
}
pub fn pairing_confirm() -> &'static str {
    text!("linux.copy.yesthisismyjournal")
}
pub fn pairing_reject() -> &'static str {
    text!("linux.copy.thatdoesntmatch")
}
pub fn pairing_cancel() -> &'static str {
    text!("linux.copy.cancelpairing")
}
pub fn pairing_confirming() -> &'static str {
    text!("linux.copy.confirming")
}
pub fn pairing_success() -> &'static str {
    text!("linux.copy.thesolstoneappcannowconnecttoyourjournal")
}
pub fn pairing_rejected_title() -> &'static str {
    text!("linux.copy.notpaired")
}
pub fn pairing_rejected_detail() -> &'static str {
    text!("linux.copy.nothingthiscomputerhaskept")
}
pub fn pairing_replacement_rejected_title() -> &'static str {
    text!("linux.copy.newpairingcancelled")
}
pub fn pairing_replacement_rejected_detail() -> &'static str {
    text!("linux.copy.yousaidthismarkdoesntmatchtheoneyourjourna2")
}
pub fn pairing_cancelled() -> &'static str {
    text!("linux.copy.pairingcancelledgetafreshpairinglinkfromyour")
}
pub fn pairing_replacement_cancelled() -> &'static str {
    text!("linux.copy.newpairingcancelledyourcurrentpairingstaysin")
}
pub fn pairing_unavailable_title() -> &'static str {
    text!("linux.copy.couldntverify")
}
pub fn pairing_unavailable_detail() -> &'static str {
    text!("linux.copy.couldntworkoutyourjournalsmarksotheresnot")
}
pub fn pairing_replacement_unavailable_detail() -> &'static str {
    text!("linux.copy.couldntworkoutthismarksotheresnothingtoco")
}
pub fn pairing_mark_unavailable() -> &'static str {
    text!("linux.copy.markunavailable")
}
pub fn pairing_new_link() -> &'static str {
    text!("linux.copy.getafreshlink")
}
pub fn pairing_stale() -> &'static str {
    text!("linux.copy.thepairingchangedwhileyouwerecheckingitsmark")
}
pub fn pairing_save_failed() -> &'static str {
    text!("linux.copy.couldntsaveyouranswertryagain")
}
pub fn pairing_outcome_unavailable() -> &'static str {
    text!("linux.copy.couldntcheckwhetherpairingfinishedcheckstatus")
}
pub fn pairing_confirmed_start_failed() -> &'static str {
    text!("linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap")
}
pub fn pairing_confirmed_unavailable() -> &'static str {
    text!("linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap2")
}
pub fn pairing_check_status() -> &'static str {
    text!("linux.copy.checkstatus")
}
pub fn pairing_confirm_action() -> &'static str {
    text!("linux.copy.confirmyourjournalsmark")
}
pub fn journal_mark_heading_label() -> &'static str {
    text!("linux.copy.mark")
}
pub fn journal_mark_heading() -> &'static str {
    text!("linux.copy.thisisjournal")
}
pub fn unknown_journal_seen_through_relay() -> &'static str {
    text!("linux.health.unknownjournalseenthroughtherelay")
}
pub fn unknown_journal_view_details() -> &'static str {
    text!("linux.copy.viewdetails")
}
pub fn unknown_journal_heading() -> &'static str {
    text!("linux.copy.somethingansweredinsteadjournal")
}
pub fn unknown_journal_body_relay() -> &'static str {
    text!("linux.copy.somethingansweredthiscomputerthrough")
}
pub fn unknown_journal_what_answered() -> &'static str {
    text!("linux.copy.whatanswered")
}
pub fn unknown_journal_your_journal() -> &'static str {
    text!("linux.copy.journal3")
}
pub fn unknown_journal_claimed() -> &'static str {
    text!("linux.health.markclaimed")
}
pub fn unknown_journal_no_identity() -> &'static str {
    text!("linux.health.noidentitypresented")
}
pub fn unknown_journal_pair_again() -> &'static str {
    text!("linux.copy.pairagain")
}
pub fn settings_heading() -> &'static str {
    text!("linux.copy.settings")
}
pub fn settings_cursor() -> &'static str {
    text!("linux.copy.drawcursor")
}
pub fn settings_start_paused() -> &'static str {
    text!("linux.copy.startpaused")
}
pub fn settings_segment() -> &'static str {
    text!("linux.copy.segmentinterval")
}
pub fn settings_segment_help() -> &'static str {
    text!("linux.copy.seconds")
}
pub fn settings_restart_help() -> &'static str {
    text!("linux.copy.thesetakeeffectthenexttimethesolstoneappsta")
}
pub fn settings_save() -> &'static str {
    text!("linux.copy.savesettings")
}
pub fn settings_saving() -> &'static str {
    text!("linux.copy.saving")
}
pub fn settings_saved() -> &'static str {
    text!("linux.copy.settingssaved")
}
pub fn settings_restart_needed() -> &'static str {
    text!("linux.copy.restartneeded")
}
pub fn settings_restart_detail() -> &'static str {
    text!("linux.copy.yoursettingsaresavedrestartthesolstoneappto")
}
pub fn settings_restart() -> &'static str {
    text!("linux.copy.restartthesolstoneapp")
}
pub fn settings_restarting() -> &'static str {
    text!("linux.copy.restarting")
}
pub fn settings_restarted() -> &'static str {
    text!("linux.copy.thesolstoneapprestartedwithyoursavedsettings")
}
pub fn settings_restart_failed() -> &'static str {
    text!("linux.copy.yoursettingsaresavedbutthesolstoneappcouldn")
}
pub fn settings_restart_recovery_failed() -> &'static str {
    text!("linux.copy.couldntrestartthesolstoneappcheckthedetails")
}
pub fn settings_save_failed() -> &'static str {
    text!("linux.copy.couldntsaveyoursettingscheckthedetailsthent")
}
pub fn settings_save_incomplete() -> &'static str {
    text!("linux.copy.couldntsaveallyoursettingsyoureditsarestill")
}
pub fn settings_save_unavailable() -> &'static str {
    text!("linux.copy.couldntcheckwhetherallyoursettingsweresaved")
}
pub fn settings_load_failed() -> &'static str {
    text!("linux.copy.couldntreadyoursettingstryagainbeforemaking")
}
pub fn settings_segment_invalid() -> &'static str {
    text!("linux.copy.enterawholenumbergreaterthan0")
}
pub fn settings_file() -> &'static str {
    text!("linux.copy.configfile")
}
pub fn settings_conflict() -> &'static str {
    text!("linux.copy.yoursettingschangedoutsidethiswindowreadthem")
}
pub fn settings_reload() -> &'static str {
    text!("linux.copy.readsettingsagain")
}
pub fn activity_write_failed() -> &'static str {
    text!("linux.copy.couldntsaveactivityintaketryagain")
}
pub fn settings_activity_source() -> &'static str {
    text!("linux.copy.desktopactivity")
}
pub fn settings_activity_through_pause() -> &'static str {
    text!("linux.copy.keepdesktopactivityonwhilepaused")
}
pub fn settings_activity_through_pause_help() -> &'static str {
    text!("linux.copy.screenandaudiostillpausedesktopactivitykeeps")
}
pub fn settings_activity_titles() -> &'static str {
    text!("linux.copy.windowtitles")
}
pub fn settings_activity_urls() -> &'static str {
    text!("linux.copy.webaddressesturnsondesktopaccessibility")
}
pub fn settings_terminal() -> &'static str {
    text!("linux.copy.terminal")
}
pub fn settings_terminal_help() -> &'static str {
    text!("linux.copy.takestmuxsessionsallit")
}
pub fn settings_move_tmux_title() -> &'static str {
    text!("linux.copy.movesolstonetmux")
}
pub fn settings_move_tmux_body() -> &'static str {
    text!("linux.copy.thismovessolstonetmuxinto")
}
pub fn settings_move_tmux_confirm() -> &'static str {
    text!("linux.copy.moveit")
}
pub fn settings_move_tmux_cancel() -> &'static str {
    text!("linux.panel.notnow")
}
pub fn settings_desktop_capture() -> &'static str {
    text!("linux.copy.screenaudiodesktopactivity")
}
pub fn settings_desktop_capture_help() -> &'static str {
    text!("linux.copy.offmeansnonethreegoes")
}
pub fn settings_activity_only() -> &'static str {
    text!("linux.copy.activityonly")
}
pub fn settings_activity_only_help() -> &'static str {
    text!("linux.copy.settingsactivityonlyhelp")
}
pub fn settings_activity_private_windows_note() -> &'static str {
    text!("linux.copy.linuxdoesnotclassifyprivatewindows")
}
pub fn settings_activity_exclude_apps() -> &'static str {
    text!("linux.copy.appsleftoutofdesktopactivity")
}
pub fn settings_activity_exclude_titles() -> &'static str {
    text!("linux.copy.windowtitlesleftoutofdesktopactivity")
}
pub fn settings_activity_route() -> &'static str {
    text!("linux.copy.activityroute")
}
pub fn settings_activity_explain() -> &'static str {
    text!("linux.copy.desktopactivityisonwheneverthesolstoneapptak")
}
pub fn help_heading() -> &'static str {
    text!("linux.copy.gethelp")
}
pub fn help_intro() -> &'static str {
    text!("linux.copy.needahandreachahumanwerehappytohelp")
}
pub fn help_open() -> &'static str {
    text!("linux.copy.gethelp")
}
pub fn help_report() -> &'static str {
    text!("linux.copy.helpreport")
}
pub const HELP_EMAIL: &str = "support@solstone.app";

/// The help page's actions, in the order they are shown.
pub fn help_action_keys() -> [CopyKey; 3] {
    [CopyKey::HelpOpen, CopyKey::HelpReport, CopyKey::HelpEmail]
}
pub fn logs_title() -> &'static str {
    text!("linux.copy.recentlogs")
}
pub fn logs_intro() -> &'static str {
    text!("linux.copy.readthesolstoneappsrecentlogsbeforesavinga")
}
pub fn logs_read() -> &'static str {
    text!("linux.copy.readrecentlogs")
}
pub fn logs_reading() -> &'static str {
    text!("linux.copy.readinglogs")
}
pub fn logs_ready() -> &'static str {
    text!("linux.copy.readthelogsbelowthenchoosewhethertosaveaco")
}
pub fn logs_partial() -> &'static str {
    text!("linux.copy.availablelogsareshownbelowcheckthenotesforg")
}
pub fn logs_empty() -> &'static str {
    text!("linux.copy.norecentlogsareavailableyettryagainafterusi")
}
pub fn logs_missing() -> &'static str {
    text!("linux.copy.nologfilesareavailableyettryagainafterusing")
}
pub fn logs_unreadable() -> &'static str {
    text!("linux.copy.couldntreadthelogfilestryagain")
}
pub fn logs_read_failed() -> &'static str {
    text!("linux.copy.couldntreadthelogstryagain")
}
pub fn logs_save() -> &'static str {
    text!("linux.copy.saveacopy")
}
pub fn logs_save_help() -> &'static str {
    text!("linux.copy.savingwritesexactlythepreviewabove")
}
pub fn logs_saving() -> &'static str {
    text!("linux.copy.savingcopy")
}
pub fn logs_saved() -> &'static str {
    text!("linux.copy.copysaved")
}
pub fn logs_save_failed() -> &'static str {
    text!("linux.copy.couldntsavethecopytrysavingtoanotherfolder")
}
pub const LOGS_FILENAME: &str = "solstone-logs.txt";
pub fn about_heading() -> &'static str {
    text!("linux.copy.aboutheading")
}
pub fn about_made_by() -> &'static str {
    text!("linux.copy.aboutmadeby")
}
pub fn quit() -> &'static str {
    text!("linux.copy.quit")
}
pub fn quit_failed() -> &'static str {
    text!("linux.copy.quitfailed")
}
pub fn updates_journal_version() -> &'static str {
    text!("linux.copy.updatesjournalversion")
}
pub fn updates_how() -> &'static str {
    text!("linux.copy.updateshow")
}
pub fn action_start() -> &'static str {
    text!("linux.copy.startthesolstoneapp")
}
pub fn action_starting() -> &'static str {
    text!("linux.copy.starting")
}
pub fn status_start_disclosure() -> &'static str {
    text!("linux.copy.thisstartsthesolstoneappnowandatsigninopen")
}
pub fn status_start_waiting() -> &'static str {
    text!("linux.copy.waitingforthesolstoneapptoanswerifyourdeskt")
}
pub fn status_start_unavailable() -> &'static str {
    text!("linux.copy.thesolstoneapphasntansweredyetcheckforades")
}
pub fn status_start_failed() -> &'static str {
    text!("linux.copy.couldntstartthesolstoneappcheckthedetailsth")
}
pub fn launch_failure_title() -> &'static str {
    text!("linux.copy.couldntopenthesolstonewindow")
}
pub fn launch_failed_without_diagnostic() -> &'static str {
    text!("linux.copy.thewindowcouldntstarttryopeningitagain")
}

pub fn not_yet() -> &'static str {
    text!("linux.copy.notyet")
}

pub enum PluralKey {
    Attention,
    OmittedBytes,
}

pub fn plural_copy(key: PluralKey, count: usize, formatted_count: &str) -> String {
    let id = match key {
        PluralKey::Attention => "linux.copy.attentioncount",
        PluralKey::OmittedBytes => "linux.copy.omittedbytes",
    };
    solstone_i18n::cardinal(
        id,
        count.min(i64::MAX as usize) as i64,
        &[("formatted", solstone_i18n::Value::String(formatted_count))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_offers_a_problem_report_between_get_help_and_email() {
        assert_eq!(
            help_action_keys(),
            [CopyKey::HelpOpen, CopyKey::HelpReport, CopyKey::HelpEmail]
        );
    }
}

pub enum ValueKey {
    Waiting,
    LastDelivery,
    Version,
}
pub fn value_copy(key: ValueKey, value: &str) -> String {
    let id = match key {
        ValueKey::Waiting => "linux.copy.waitingvalue",
        ValueKey::LastDelivery => "linux.copy.lastdeliveryvalue",
        ValueKey::Version => "linux.copy.versionvalue",
    };
    solstone_i18n::render(
        solstone_i18n::locale(),
        id,
        &[("value", solstone_i18n::Value::String(value))],
    )
}

pub fn journal_mark_speech(tint1: &str, tint2: &str, word1: &str, word2: &str) -> String {
    format!("{tint1}, {tint2} · {word1}·{word2}")
}

pub fn journal_mark_label(word1: &str, word2: &str) -> String {
    format!("{word1}·{word2}")
}

pub fn unknown_journal_seen_at(address: &str) -> String {
    message!("linux.health.unknownjournalseenatx", "address" => address)
}

pub fn unknown_journal_body_direct(address: &str) -> String {
    message!("linux.copy.somethingansweredthiscomputerat", "address" => address)
}

pub fn launch_runtime_unavailable(path: &str) -> String {
    message!("linux.copy.thewindowneedsgtk46ornewercouldntloadits", "path" => path)
}

pub fn launch_failed(path: &str) -> String {
    message!("linux.copy.thewindowcouldntstartopenxfordetails", "path" => path)
}

pub fn launch_retained_diagnostic(path: &str) -> String {
    message!("linux.copy.couldntopenthesolstonewindowopenxfordetails", "path" => path)
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
    StatusPausedActivityOnDetail,
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
    JournalSyncing,
    JournalOffline,
    JournalOfflineDetail,
    JournalAttentionDetail,
    JournalNotPaired,
    JournalPairTitle,
    JournalPairHelp,
    LocalJournalFound,
    LocalJournalConnect,
    LocalJournalConnecting,
    LocalJournalConnectedTo,
    LocalJournalConnectFailed,
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
    PairingHeldRun,
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
    JournalMarkHeading,
    SettingsHeading,
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
    SettingsTerminal,
    SettingsTerminalHelp,
    SettingsMoveTmuxTitle,
    SettingsMoveTmuxBody,
    SettingsMoveTmuxConfirm,
    SettingsMoveTmuxCancel,
    SettingsDesktopCapture,
    SettingsDesktopCaptureHelp,
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
    HelpReport,
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
    MarkNotPresentedFirst,
    MarkNotPresentedSecond,
    UnknownJournalSeenThroughRelay,
    UnknownJournalViewDetails,
    UnknownJournalHeading,
    UnknownJournalBodyRelay,
    UnknownJournalWhatAnswered,
    UnknownJournalYourJournal,
    UnknownJournalClaimed,
    UnknownJournalNoIdentity,
    UnknownJournalPairAgain,
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
            Self::WindowTitle => window_title(),
            Self::NavStatus => nav_status(),
            Self::NavJournal => nav_journal(),
            Self::NavSettings => nav_settings(),
            Self::NavHelp => nav_help(),
            Self::ActionOpenJournal => action_open_journal(),
            Self::StatusOn => status_on(),
            Self::StatusOnDetail => status_on_detail(),
            Self::StatusScreenWaiting => status_screen_waiting(),
            Self::StatusScreenWaitingDetail => status_screen_waiting_detail(),
            Self::StatusPaused => status_paused(),
            Self::StatusPausedDetail => status_paused_detail(),
            Self::StatusPausedActivityOnDetail => status_paused_activity_on_detail(),
            Self::StatusStarting => status_starting(),
            Self::StatusOff => status_off(),
            Self::StatusOffDetail => status_off_detail(),
            Self::StatusAttention => status_attention(),
            Self::StatusUnavailable => status_unavailable(),
            Self::StatusUnavailableDetail => status_unavailable_detail(),
            Self::StatusChecking => status_checking(),
            Self::StatusIntakeLabel => status_intake_label(),
            Self::StatusScreenLabel => status_screen_label(),
            Self::StatusAudioLabel => status_audio_label(),
            Self::StatusJournalLabel => status_journal_label(),
            Self::StatusWaitingLabel => status_waiting_label(),
            Self::StatusAttentionLabel => status_attention_label(),
            Self::StatusLastDelivery => status_last_delivery(),
            Self::StatusVersion => status_version(),
            Self::StatusDetails => status_details(),
            Self::ActionPause => action_pause(),
            Self::ActionPausing => action_pausing(),
            Self::ActionResume => action_resume(),
            Self::ActionResuming => action_resuming(),
            Self::ActionRetry => action_retry(),
            Self::ActionCheckStatus => action_check_status(),
            Self::ActionViewJournal => action_view_journal(),
            Self::StatusPauseFailed => status_pause_failed(),
            Self::StatusResumeFailed => status_resume_failed(),
            Self::StatusPauseUnavailable => {
                text!("linux.copy.couldntcheckwhetherintakepausedcheckstatusbe")
            }
            Self::StatusResumeUnavailable => {
                text!("linux.copy.couldntcheckwhetherintakeresumedcheckstatusb")
            }
            Self::BrowserOpenFailed => browser_open_failed(),
            Self::JournalConnected => journal_connected(),
            Self::JournalSyncing => journal_syncing(),
            Self::JournalOffline => journal_offline(),
            Self::JournalOfflineDetail => journal_offline_detail(),
            Self::JournalAttentionDetail => journal_attention_detail(),
            Self::JournalNotPaired => journal_not_paired(),
            Self::JournalPairTitle => journal_pair_title(),
            Self::JournalPairHelp => journal_pair_help(),
            Self::LocalJournalFound => local_journal_found(),
            Self::LocalJournalConnect => local_journal_connect(),
            Self::LocalJournalConnecting => local_journal_connecting(),
            Self::LocalJournalConnectedTo => local_journal_connected_to(),
            Self::LocalJournalConnectFailed => local_journal_connect_failed(),
            Self::JournalLinkLabel => journal_link_label(),
            Self::JournalLinkPlaceholder => journal_link_placeholder(),
            Self::ActionPair => action_pair(),
            Self::ActionPairing => action_pairing(),
            Self::PairingEmpty => pairing_empty(),
            Self::PairingBadLink => pairing_bad_link(),
            Self::PairingExpired => pairing_expired(),
            Self::PairingFailed => pairing_failed(),
            Self::PairingHeld => pairing_held(),
            Self::PairingHeldDetail => pairing_held_detail(),
            Self::PairingHeldRun => pairing_held_run(),
            Self::PairingReplacementHeldDetail => pairing_replacement_held_detail(),
            Self::PairingQuestion => pairing_question(),
            Self::PairingQuestionDetail => pairing_question_detail(),
            Self::PairingConfirm => pairing_confirm(),
            Self::PairingReject => pairing_reject(),
            Self::PairingCancel => pairing_cancel(),
            Self::PairingConfirming => pairing_confirming(),
            Self::PairingSuccess => pairing_success(),
            Self::PairingRejectedTitle => pairing_rejected_title(),
            Self::PairingRejectedDetail => pairing_rejected_detail(),
            Self::PairingReplacementRejectedTitle => pairing_replacement_rejected_title(),
            Self::PairingReplacementRejectedDetail => pairing_replacement_rejected_detail(),
            Self::PairingCancelled => pairing_cancelled(),
            Self::PairingReplacementCancelled => pairing_replacement_cancelled(),
            Self::PairingUnavailableTitle => pairing_unavailable_title(),
            Self::PairingUnavailableDetail => pairing_unavailable_detail(),
            Self::PairingReplacementUnavailableDetail => pairing_replacement_unavailable_detail(),
            Self::PairingMarkUnavailable => pairing_mark_unavailable(),
            Self::PairingNewLink => pairing_new_link(),
            Self::PairingStale => pairing_stale(),
            Self::PairingSaveFailed => pairing_save_failed(),
            Self::PairingOutcomeUnavailable => pairing_outcome_unavailable(),
            Self::PairingConfirmedStartFailed => pairing_confirmed_start_failed(),
            Self::PairingConfirmedUnavailable => pairing_confirmed_unavailable(),
            Self::PairingCheckStatus => pairing_check_status(),
            Self::PairingConfirmAction => pairing_confirm_action(),
            Self::JournalMarkLabel => journal_mark_heading_label(),
            Self::JournalMarkHeading => journal_mark_heading(),
            Self::SettingsHeading => settings_heading(),
            Self::SettingsCursor => settings_cursor(),
            Self::SettingsStartPaused => settings_start_paused(),
            Self::SettingsSegment => settings_segment(),
            Self::SettingsSegmentHelp => settings_segment_help(),
            Self::SettingsRestartHelp => settings_restart_help(),
            Self::SettingsSave => settings_save(),
            Self::SettingsSaving => settings_saving(),
            Self::SettingsSaved => settings_saved(),
            Self::SettingsRestartNeeded => settings_restart_needed(),
            Self::SettingsRestartDetail => settings_restart_detail(),
            Self::SettingsRestart => settings_restart(),
            Self::SettingsRestarting => settings_restarting(),
            Self::SettingsRestarted => settings_restarted(),
            Self::SettingsRestartFailed => settings_restart_failed(),
            Self::SettingsRestartRecoveryFailed => settings_restart_recovery_failed(),
            Self::SettingsSaveFailed => settings_save_failed(),
            Self::SettingsSaveIncomplete => settings_save_incomplete(),
            Self::SettingsSaveUnavailable => settings_save_unavailable(),
            Self::SettingsLoadFailed => settings_load_failed(),
            Self::SettingsSegmentInvalid => settings_segment_invalid(),
            Self::SettingsFile => settings_file(),
            Self::SettingsConflict => settings_conflict(),
            Self::SettingsReload => settings_reload(),
            Self::ActivityWriteFailed => activity_write_failed(),
            Self::SettingsActivitySource => settings_activity_source(),
            Self::SettingsActivityThroughPause => settings_activity_through_pause(),
            Self::SettingsActivityThroughPauseHelp => settings_activity_through_pause_help(),
            Self::SettingsActivityTitles => settings_activity_titles(),
            Self::SettingsActivityUrls => settings_activity_urls(),
            Self::SettingsTerminal => settings_terminal(),
            Self::SettingsTerminalHelp => settings_terminal_help(),
            Self::SettingsMoveTmuxTitle => settings_move_tmux_title(),
            Self::SettingsMoveTmuxBody => settings_move_tmux_body(),
            Self::SettingsMoveTmuxConfirm => settings_move_tmux_confirm(),
            Self::SettingsMoveTmuxCancel => settings_move_tmux_cancel(),
            Self::SettingsDesktopCapture => settings_desktop_capture(),
            Self::SettingsDesktopCaptureHelp => settings_desktop_capture_help(),
            Self::SettingsActivityOnly => settings_activity_only(),
            Self::SettingsActivityOnlyHelp => settings_activity_only_help(),
            Self::SettingsActivityPrivateWindowsNote => settings_activity_private_windows_note(),
            Self::SettingsActivityExcludeApps => settings_activity_exclude_apps(),
            Self::SettingsActivityExcludeTitles => settings_activity_exclude_titles(),
            Self::SettingsActivityRoute => settings_activity_route(),
            Self::SettingsActivityExplain => settings_activity_explain(),
            Self::HelpHeading => help_heading(),
            Self::HelpIntro => help_intro(),
            Self::HelpOpen => help_open(),
            Self::HelpReport => help_report(),
            Self::HelpEmail => HELP_EMAIL,
            Self::LogsTitle => logs_title(),
            Self::LogsIntro => logs_intro(),
            Self::LogsRead => logs_read(),
            Self::LogsReading => logs_reading(),
            Self::LogsReady => logs_ready(),
            Self::LogsPartial => logs_partial(),
            Self::LogsEmpty => logs_empty(),
            Self::LogsMissing => logs_missing(),
            Self::LogsUnreadable => logs_unreadable(),
            Self::LogsReadFailed => logs_read_failed(),
            Self::LogsSave => logs_save(),
            Self::LogsSaveHelp => logs_save_help(),
            Self::LogsSaving => logs_saving(),
            Self::LogsSaved => logs_saved(),
            Self::LogsSaveFailed => logs_save_failed(),
            Self::LogsFilename => LOGS_FILENAME,
            Self::LogsGap => {
                text!("linux.copy.thelogpreviewwaslimitedmorelogtextmaybeava")
            }
            Self::MarkGenericFirst => text!("linux.copy.your"),
            Self::MarkGenericSecond => text!("linux.copy.journal"),
            Self::MarkGenericSpeech => text!("linux.copy.yourjournalnotsetupyet"),
            Self::MarkUnavailableFirst => text!("linux.copy.mark"),
            Self::MarkUnavailableSecond => text!("linux.copy.unavailable"),
            Self::MarkUnavailableSpeech => {
                text!("linux.copy.yourjournalsmarkunavailablerightnow")
            }
            Self::MarkNotPresentedFirst => text!("linux.copy.not"),
            Self::MarkNotPresentedSecond => text!("linux.copy.presented"),
            Self::UnknownJournalSeenThroughRelay => unknown_journal_seen_through_relay(),
            Self::UnknownJournalViewDetails => unknown_journal_view_details(),
            Self::UnknownJournalHeading => unknown_journal_heading(),
            Self::UnknownJournalBodyRelay => unknown_journal_body_relay(),
            Self::UnknownJournalWhatAnswered => unknown_journal_what_answered(),
            Self::UnknownJournalYourJournal => unknown_journal_your_journal(),
            Self::UnknownJournalClaimed => unknown_journal_claimed(),
            Self::UnknownJournalNoIdentity => unknown_journal_no_identity(),
            Self::UnknownJournalPairAgain => unknown_journal_pair_again(),
            Self::HelpLinkFailed => {
                text!("linux.copy.couldntopenthatlinkvisitsupportsolstoneappo")
            }
            Self::ActionStart => action_start(),
            Self::ActionStarting => action_starting(),
            Self::StatusStartDisclosure => status_start_disclosure(),
            Self::StatusStartWaiting => status_start_waiting(),
            Self::StatusStartUnavailable => status_start_unavailable(),
            Self::StatusStartFailed => status_start_failed(),
            Self::LaunchFailureTitle => launch_failure_title(),
            Self::LaunchFailedWithoutDiagnostic => launch_failed_without_diagnostic(),
        }
    }
}
pub fn journal_setup_action() -> &'static str {
    text!("linux.copy.journalsetupaction")
}
pub fn journal_setup_preparing() -> &'static str {
    text!("linux.copy.journalsetuppreparing")
}
pub fn journal_setup_in_browser() -> &'static str {
    text!("linux.copy.journalsetupinbrowser")
}
pub fn journal_setup_not_installed() -> &'static str {
    text!("linux.copy.journalsetupnotinstalled")
}
pub fn journal_setup_install() -> &'static str {
    text!("linux.copy.journalsetupinstall")
}
pub fn journal_setup_failed() -> &'static str {
    text!("linux.copy.journalsetupfailed")
}
pub fn journal_setup_open_failed() -> &'static str {
    text!("linux.copy.journalsetupopenfailed")
}
