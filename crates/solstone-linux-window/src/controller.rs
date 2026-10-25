// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_linux::dbus_client::ControlClientError;
use solstone_linux::window_control::{
    ControlModel, MarkRenderDocument, SessionView, SettingsModel, decode_control_str,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlight<T> {
    pub id: u64,
    pub bus_owner: String,
    pub payload: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntakeCommand {
    Pause(i32),
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingCommand {
    Begin(String),
    Answer {
        attempt_id: String,
        pairing_id: String,
        accept: bool,
    },
    Cancel(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PairingPresentation {
    pub bus_owner: String,
    pub attempt_id: String,
    pub pairing_id: String,
    pub mark: Option<MarkRenderDocument>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDraft {
    pub framerate: Option<i64>,
    pub framerate_dirty: bool,
    pub cursor: Option<bool>,
    pub cursor_dirty: bool,
    pub start_paused: Option<bool>,
    pub start_paused_dirty: bool,
    pub segment: Option<i64>,
    pub segment_dirty: bool,
    pub activity_source: Option<bool>,
    pub activity_source_dirty: bool,
    pub activity_titles: Option<bool>,
    pub activity_titles_dirty: bool,
    pub activity_urls: Option<bool>,
    pub activity_urls_dirty: bool,
    pub activity_exclude_apps: Option<Vec<String>>,
    pub activity_exclude_apps_dirty: bool,
    pub activity_exclude_titles: Option<Vec<String>>,
    pub activity_exclude_titles_dirty: bool,
    pub activity_through_pause: Option<bool>,
    pub activity_through_pause_dirty: bool,
    pub activity_only: Option<bool>,
    pub activity_only_dirty: bool,
    pub in_flight: Option<InFlight<SettingsModel>>,
    pub saved_verified: bool,
    pub write_failed: bool,
    pub waiting_confirming_read: bool,
    pub requested_writes: Vec<(&'static str, String)>,
    pub confirm_after: u64,
    pub write_uncertain: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsWriteOutcome {
    Complete,
    Rejected,
    Unknown,
}

impl SettingsDraft {
    pub fn is_dirty(&self) -> bool {
        self.framerate_dirty
            || self.cursor_dirty
            || self.start_paused_dirty
            || self.segment_dirty
            || self.activity_source_dirty
            || self.activity_titles_dirty
            || self.activity_urls_dirty
            || self.activity_exclude_apps_dirty
            || self.activity_exclude_titles_dirty
            || self.activity_through_pause_dirty
            || self.activity_only_dirty
    }

    pub fn changed_writes(&self) -> Vec<(&'static str, String)> {
        let mut writes = Vec::new();
        if self.framerate_dirty
            && let Some(val) = self.framerate
        {
            writes.push(("capture_framerate", val.to_string()));
        }
        if self.cursor_dirty
            && let Some(val) = self.cursor
        {
            writes.push(("draw_cursor", val.to_string()));
        }
        if self.start_paused_dirty
            && let Some(val) = self.start_paused
        {
            writes.push(("start_paused", val.to_string()));
        }
        if self.segment_dirty
            && let Some(val) = self.segment
        {
            writes.push(("segment_interval", val.to_string()));
        }
        // One atomic policy write carrying only the activity choices the owner
        // changed. Untouched choices stay unsaved, so they keep the app's defaults.
        let mut policy = serde_json::Map::new();
        if self.activity_source_dirty
            && let Some(val) = self.activity_source
        {
            policy.insert("activity_source".into(), val.into());
        }
        if self.activity_titles_dirty
            && let Some(val) = self.activity_titles
        {
            policy.insert("activity_titles".into(), val.into());
        }
        if self.activity_urls_dirty
            && let Some(val) = self.activity_urls
        {
            policy.insert("activity_urls".into(), val.into());
        }
        if self.activity_exclude_apps_dirty
            && let Some(ref val) = self.activity_exclude_apps
        {
            policy.insert("activity_exclude_apps".into(), val.clone().into());
        }
        if self.activity_exclude_titles_dirty
            && let Some(ref val) = self.activity_exclude_titles
        {
            policy.insert("activity_exclude_titles".into(), val.clone().into());
        }
        if self.activity_through_pause_dirty
            && let Some(val) = self.activity_through_pause
        {
            policy.insert("activity_through_pause".into(), val.into());
        }
        if !policy.is_empty() {
            writes.push((
                "activity_policy",
                serde_json::Value::Object(policy).to_string(),
            ));
        }
        if self.activity_only_dirty
            && let Some(val) = self.activity_only
        {
            writes.push(("activity_only", val.to_string()));
        }
        writes
    }

    pub fn matches_writes(settings: &SettingsModel, writes: &[(&'static str, String)]) -> bool {
        writes.iter().all(|(field, expected)| match *field {
            "capture_framerate" => settings.capture_framerate.to_string() == *expected,
            "draw_cursor" => settings.draw_cursor.to_string() == *expected,
            "start_paused" => settings.start_paused.to_string() == *expected,
            "segment_interval" => settings.segment_interval.to_string() == *expected,
            "activity_only" => settings.activity_only.to_string() == *expected,
            "activity_policy" => {
                let Ok(serde_json::Value::Object(val)) =
                    serde_json::from_str::<serde_json::Value>(expected)
                else {
                    return false;
                };
                val.iter().all(|(key, item)| match key.as_str() {
                    "activity_source" => item.as_bool() == Some(settings.activity_source),
                    "activity_titles" => item.as_bool() == Some(settings.activity_titles),
                    "activity_urls" => item.as_bool() == Some(settings.activity_urls),
                    "activity_through_pause" => {
                        item.as_bool() == Some(settings.activity_through_pause)
                    }
                    "activity_exclude_apps" => {
                        serde_json::from_value::<Vec<String>>(item.clone()).ok()
                            == Some(settings.activity_exclude_apps.clone())
                    }
                    "activity_exclude_titles" => {
                        serde_json::from_value::<Vec<String>>(item.clone()).ok()
                            == Some(settings.activity_exclude_titles.clone())
                    }
                    _ => false,
                })
            }
            _ => false,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogSlot {
    pub in_flight: Option<InFlight<()>>,
    pub frozen: Option<crate::logs::FrozenLog>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum StartActionKind {
    Start,
}

#[allow(dead_code)]
pub struct Controller {
    pub refreshing: bool,
    next_id: u64,
    last_status_id: u64,
    pub service_presence: solstone_linux::service::ServicePresence,
    pub service_owner_pid: Option<u32>,
    pub older_daemon: bool,
    pub start_deadline: Option<std::time::Instant>,
    pub restart_expected: Option<(String, u32, Option<SettingsModel>, std::time::Instant)>,
    pub restart_verified: bool,
    pub browser_error: Option<solstone_linux::copy::CopyKey>,
    pub browser_request: Option<InFlight<()>>,
    pub bus_owner: Option<String>,
    pub document: ControlModel,
    pub intake_command: Option<InFlight<IntakeCommand>>,
    pub intake_error: Option<String>,
    pub intake_error_key: Option<solstone_linux::copy::CopyKey>,
    intake_deadline: Option<std::time::Instant>,
    intake_confirm_after: u64,
    intake_unverified: Option<InFlight<IntakeCommand>>,
    pub pairing: Option<InFlight<PairingCommand>>,
    pub pairing_attempt_outcome: Option<String>,
    pub pairing_error: Option<String>,
    pub last_answer_mark: Option<MarkRenderDocument>,
    pub settings_draft: SettingsDraft,
    pub logs: LogSlot,
    pub start: Option<InFlight<StartActionKind>>,
    pub start_error: Option<String>,
    pub service_diagnostic: Option<String>,
    pub restart: Option<InFlight<()>>,
    pub restart_error: Option<String>,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl Controller {
    pub fn new() -> Self {
        Self {
            refreshing: false,
            next_id: 1,
            last_status_id: 0,
            service_presence: solstone_linux::service::ServicePresence::Uncertain,
            service_owner_pid: None,
            older_daemon: false,
            start_deadline: None,
            restart_expected: None,
            restart_verified: false,
            browser_error: None,
            browser_request: None,
            bus_owner: None,
            document: ControlModel::unknown(),
            intake_command: None,
            intake_error: None,
            intake_error_key: None,
            intake_deadline: None,
            intake_confirm_after: 0,
            intake_unverified: None,
            pairing: None,
            pairing_attempt_outcome: None,
            pairing_error: None,
            last_answer_mark: None,
            settings_draft: SettingsDraft::default(),
            logs: LogSlot::default(),
            start: None,
            start_error: None,
            service_diagnostic: None,
            restart: None,
            restart_error: None,
        }
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn set_bus_owner(&mut self, owner: Option<String>) {
        if self.bus_owner != owner {
            self.bus_owner = owner;
            self.service_owner_pid = None;
            self.older_daemon = false;
            self.restart_verified = false;
            self.browser_error = None;
            self.browser_request = None;
            self.document = ControlModel::unknown();
            self.last_status_id = 0;
            self.settings_draft.saved_verified = false;
            if self.settings_draft.in_flight.is_some()
                || self.settings_draft.waiting_confirming_read
            {
                self.settings_draft.in_flight = None;
                self.settings_draft.waiting_confirming_read = true;
                self.settings_draft.confirm_after = 0;
                self.settings_draft.write_uncertain = true;
                self.settings_draft.write_failed = false;
            }
            if self.pairing.take().is_some() {
                self.pairing_attempt_outcome = Some("outcome-unknown".into());
            }
            self.make_intake_unverified();
        }
    }

    pub fn start_intake_command(&mut self, bus_owner: &str, command: IntakeCommand) -> u64 {
        let id = self.alloc_id();
        self.intake_command = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: command,
        });
        self.intake_error = None;
        self.intake_error_key = None;
        self.intake_unverified = None;
        self.intake_confirm_after = u64::MAX;
        self.intake_deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        id
    }

    fn make_intake_unverified(&mut self) {
        if let Some(command) = self.intake_command.take() {
            self.intake_confirm_after = self.alloc_id();
            self.intake_error_key = Some(match command.payload {
                IntakeCommand::Pause(_) => solstone_linux::copy::CopyKey::StatusPauseUnavailable,
                IntakeCommand::Resume => solstone_linux::copy::CopyKey::StatusResumeUnavailable,
            });
            self.intake_error = Some("outcome-unknown".into());
            self.intake_unverified = Some(command);
        }
        self.intake_deadline = None;
    }

    pub fn expire_intake(&mut self, now: std::time::Instant) {
        if self.intake_deadline.is_some_and(|deadline| now >= deadline) {
            self.make_intake_unverified();
        }
    }

    pub fn start_pairing(&mut self, bus_owner: &str, command: PairingCommand) -> u64 {
        if !matches!(command, PairingCommand::Answer { accept: true, .. }) {
            self.last_answer_mark = None;
        }
        let id = self.alloc_id();
        self.pairing = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: command,
        });
        self.pairing_error = None;
        self.pairing_attempt_outcome = None;
        id
    }

    pub fn pairing_presentation(&self) -> Option<PairingPresentation> {
        if self.pairing.is_some()
            || self.pairing_attempt_outcome.as_deref() == Some("outcome-unknown")
            || self.document.view == SessionView::Unknown
        {
            return None;
        }
        let attempt = self.document.attempt.as_ref()?;
        if attempt.state != "pending" {
            return None;
        }
        Some(PairingPresentation {
            bus_owner: self.bus_owner.clone()?,
            attempt_id: attempt.id.clone(),
            pairing_id: attempt.pairing_id.clone(),
            mark: self.document.pending_mark.clone(),
        })
    }

    pub fn answer_presented(
        &mut self,
        shown: &PairingPresentation,
        accept: bool,
    ) -> Result<(u64, PairingCommand), solstone_linux::copy::CopyKey> {
        if self.pairing_presentation().as_ref() != Some(shown) {
            return Err(solstone_linux::copy::CopyKey::PairingStale);
        }
        if accept && shown.mark.is_none() {
            return Err(solstone_linux::copy::CopyKey::PairingUnavailableTitle);
        }
        let command = PairingCommand::Answer {
            attempt_id: shown.attempt_id.clone(),
            pairing_id: shown.pairing_id.clone(),
            accept,
        };
        self.last_answer_mark = if accept { shown.mark.clone() } else { None };
        let id = self.start_pairing(&shown.bus_owner, command.clone());
        Ok((id, command))
    }

    pub fn cancel_presented(
        &mut self,
        shown: &PairingPresentation,
    ) -> Result<(u64, PairingCommand), solstone_linux::copy::CopyKey> {
        if self.pairing_presentation().as_ref() != Some(shown) {
            return Err(solstone_linux::copy::CopyKey::PairingStale);
        }
        let command = PairingCommand::Cancel(shown.attempt_id.clone());
        let id = self.start_pairing(&shown.bus_owner, command.clone());
        Ok((id, command))
    }

    pub fn start_settings_save(&mut self, bus_owner: &str, settings: SettingsModel) -> u64 {
        self.restart_verified = false;
        let id = self.alloc_id();
        self.settings_draft.requested_writes = self.settings_draft.changed_writes();
        self.settings_draft.in_flight = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: settings,
        });
        self.settings_draft.saved_verified = false;
        self.settings_draft.write_failed = false;
        self.settings_draft.waiting_confirming_read = true;
        self.settings_draft.write_uncertain = false;
        self.settings_draft.confirm_after = u64::MAX;
        id
    }

    pub fn start_logs_request(&mut self, bus_owner: &str) -> u64 {
        let id = self.alloc_id();
        self.logs.in_flight = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: (),
        });
        id
    }

    pub fn start_service_start(&mut self, bus_owner: &str) -> u64 {
        let id = self.alloc_id();
        self.start = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: StartActionKind::Start,
        });
        self.start_error = None;
        self.service_diagnostic = None;
        id
    }

    pub fn start_service_restart(&mut self, bus_owner: &str) -> u64 {
        let id = self.alloc_id();
        self.restart = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: (),
        });
        self.restart_error = None;
        self.service_diagnostic = None;
        id
    }

    pub fn observe_service_reply(
        &mut self,
        id: u64,
        owner: &str,
        pid: Option<u32>,
        service_pid: Option<u32>,
        reply: Result<String, ControlClientError>,
    ) {
        self.set_bus_owner(Some(owner.to_string()));
        self.service_owner_pid = pid.filter(|pid| *pid != 0 && Some(*pid) == service_pid);
        self.apply_status_reply(id, owner, reply);
        if self.document.view != solstone_linux::window_control::SessionView::Unknown {
            self.start = None;
            self.start_error = None;
            self.start_deadline = None;
            if let Some((old_owner, old_pid, wanted, _)) = &self.restart_expected
                && owner != old_owner
                && pid.is_some_and(|pid| pid != *old_pid)
                && self.service_owner_pid == pid
                && pid.is_some()
                && self.document.saved_available
                && wanted
                    .as_ref()
                    .is_none_or(|wanted| self.document.settings == *wanted)
                && self.document.effective.as_ref() == Some(&self.document.settings)
            {
                self.restart_verified = true;
                self.restart = None;
                self.restart_expected = None;
                self.restart_error = None;
            }
        }
    }

    pub fn apply_status_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if Some(reply_owner) != self.bus_owner.as_deref() || id < self.last_status_id {
            return;
        }
        self.last_status_id = id;

        let older_daemon = matches!(reply, Err(ControlClientError::UnknownMethod));
        let model = match reply {
            Ok(json_str) => match decode_control_str(&json_str) {
                Ok(m) if m.view != SessionView::Unknown => m,
                _ => ControlModel::unknown(),
            },
            Err(_) => ControlModel::unknown(),
        };

        self.older_daemon = older_daemon;
        self.document = model;
        if self.document.view == SessionView::Unknown
            || !self.document.saved_available
            || self.document.effective.as_ref() != Some(&self.document.settings)
        {
            self.restart_verified = false;
        }
        if self.document.view == SessionView::Unknown {
            return;
        }

        if self.document.fault.is_some() || self.document.attempt.is_some() {
            self.pairing_attempt_outcome = None;
        }
        if self.last_answer_mark.is_some()
            && self.document.confirmed_mark == self.last_answer_mark
            && self.document.durable == "confirmed"
            && self.document.owner == "running"
            && self.document.attempt.is_none()
            && self.document.fault.is_none()
        {
            self.pairing_attempt_outcome = Some("confirmed".into());
            self.last_answer_mark = None;
        }

        // Dirty settings fields stay dirty until a fresh read shows that field's requested value
        if self.document.saved_available {
            if self.settings_draft.saved_verified
                && !SettingsDraft::matches_writes(
                    &self.document.settings,
                    &self.settings_draft.requested_writes,
                )
            {
                self.settings_draft.saved_verified = false;
            }
            if self.settings_draft.framerate_dirty
                && let Some(val) = self.settings_draft.framerate
                && val == self.document.settings.capture_framerate
            {
                self.settings_draft.framerate_dirty = false;
            }
            if self.settings_draft.cursor_dirty
                && let Some(val) = self.settings_draft.cursor
                && val == self.document.settings.draw_cursor
            {
                self.settings_draft.cursor_dirty = false;
            }
            if self.settings_draft.start_paused_dirty
                && let Some(val) = self.settings_draft.start_paused
                && val == self.document.settings.start_paused
            {
                self.settings_draft.start_paused_dirty = false;
            }
            if self.settings_draft.segment_dirty
                && let Some(val) = self.settings_draft.segment
                && val == self.document.settings.segment_interval
            {
                self.settings_draft.segment_dirty = false;
            }
            if self.settings_draft.activity_source_dirty
                && let Some(val) = self.settings_draft.activity_source
                && val == self.document.settings.activity_source
            {
                self.settings_draft.activity_source_dirty = false;
            }
            if self.settings_draft.activity_titles_dirty
                && let Some(val) = self.settings_draft.activity_titles
                && val == self.document.settings.activity_titles
            {
                self.settings_draft.activity_titles_dirty = false;
            }
            if self.settings_draft.activity_urls_dirty
                && let Some(val) = self.settings_draft.activity_urls
                && val == self.document.settings.activity_urls
            {
                self.settings_draft.activity_urls_dirty = false;
            }
            if self.settings_draft.activity_exclude_apps_dirty
                && let Some(ref val) = self.settings_draft.activity_exclude_apps
                && val == &self.document.settings.activity_exclude_apps
            {
                self.settings_draft.activity_exclude_apps_dirty = false;
            }
            if self.settings_draft.activity_exclude_titles_dirty
                && let Some(ref val) = self.settings_draft.activity_exclude_titles
                && val == &self.document.settings.activity_exclude_titles
            {
                self.settings_draft.activity_exclude_titles_dirty = false;
            }
            if self.settings_draft.activity_through_pause_dirty
                && let Some(val) = self.settings_draft.activity_through_pause
                && val == self.document.settings.activity_through_pause
            {
                self.settings_draft.activity_through_pause_dirty = false;
            }
            if self.settings_draft.activity_only_dirty
                && let Some(val) = self.settings_draft.activity_only
                && val == self.document.settings.activity_only
            {
                self.settings_draft.activity_only_dirty = false;
            }
        }

        if self.settings_draft.waiting_confirming_read
            && self.document.saved_available
            && self.settings_draft.in_flight.is_none()
            && id > self.settings_draft.confirm_after
        {
            if SettingsDraft::matches_writes(
                &self.document.settings,
                &self.settings_draft.requested_writes,
            ) {
                self.settings_draft.saved_verified = !self.settings_draft.is_dirty();
                self.settings_draft.waiting_confirming_read = false;
                self.settings_draft.write_failed = false;
                self.settings_draft.write_uncertain = false;
            } else if !self.settings_draft.write_uncertain {
                self.settings_draft.saved_verified = false;
                self.settings_draft.waiting_confirming_read = false;
                self.settings_draft.write_failed = true;
            }
        }

        // Completion of Pause/Resume: later document whose pause facts match the request
        if let Some(in_flight) = self
            .intake_command
            .as_ref()
            .or(self.intake_unverified.as_ref())
            .cloned()
            && in_flight.id <= id
            && id > self.intake_confirm_after
        {
            match in_flight.payload {
                IntakeCommand::Pause(_) => {
                    if self.document.capture == "paused" {
                        self.intake_command = None;
                        self.intake_unverified = None;
                        self.intake_deadline = None;
                        self.intake_error = None;
                        self.intake_error_key = None;
                    }
                }
                IntakeCommand::Resume => {
                    if matches!(
                        self.document.capture.as_str(),
                        "recording" | "idle" | "waiting" | "stopped"
                    ) && self.document.pause_bounded.is_none()
                    {
                        self.intake_command = None;
                        self.intake_unverified = None;
                        self.intake_deadline = None;
                        self.intake_error = None;
                        self.intake_error_key = None;
                    }
                }
            }
        }
    }

    pub fn apply_intake_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if self.bus_owner.as_deref() != Some(reply_owner)
            || !self
                .intake_command
                .as_ref()
                .is_some_and(|request| request.id == id && request.bus_owner == reply_owner)
        {
            return;
        }
        match reply {
            Ok(_) => self.intake_confirm_after = self.alloc_id(),
            Err(error) => {
                self.make_intake_unverified();
                self.intake_error = Some(error.to_string());
            }
        }
    }

    pub fn apply_pairing_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if self.bus_owner.as_deref() != Some(reply_owner) {
            return;
        }
        if let Some(in_flight) = &self.pairing
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match reply {
                Ok(payload) => {
                    self.pairing = None;
                    self.pairing_error = None;
                    if id < self.last_status_id {
                        return;
                    }
                    self.last_status_id = id;
                    match decode_control_str(&payload) {
                        Ok(model) if model.view != SessionView::Unknown => self.document = model,
                        _ => {
                            self.document = ControlModel::unknown();
                            self.pairing_attempt_outcome = Some("outcome-unknown".into());
                        }
                    }
                }
                Err(e) => {
                    self.pairing_error = Some(e.to_string());
                    self.pairing_attempt_outcome = Some("outcome-unknown".into());
                    self.pairing = None;
                }
            }
        }
    }

    pub fn apply_settings_write_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: SettingsWriteOutcome,
    ) {
        if self.bus_owner.as_deref() != Some(reply_owner) {
            return;
        }
        let matches = self
            .settings_draft
            .in_flight
            .as_ref()
            .is_some_and(|request| request.id == id && request.bus_owner == reply_owner);
        if matches {
            self.settings_draft.confirm_after = self.alloc_id();
            self.settings_draft.in_flight = None;
            self.settings_draft.waiting_confirming_read = true;
            self.settings_draft.write_failed = false;
            self.settings_draft.write_uncertain = reply == SettingsWriteOutcome::Unknown;
        }
    }

    pub fn apply_logs_reply(&mut self, id: u64, reply_owner: &str, frozen: crate::logs::FrozenLog) {
        if let Some(in_flight) = &self.logs.in_flight
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            self.logs.frozen = Some(frozen);
            self.logs.in_flight = None;
        }
    }

    pub fn apply_start_reply(&mut self, id: u64, reply_owner: &str, res: Result<(), String>) {
        if let Some(in_flight) = &self.start
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match res {
                Ok(()) => {
                    self.start = None;
                    self.start_error = None;
                }
                Err(e) => {
                    self.start_error = Some(e);
                    self.start = None;
                }
            }
        }
    }

    pub fn apply_restart_reply(&mut self, id: u64, reply_owner: &str, res: Result<(), String>) {
        if let Some(in_flight) = &self.restart
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match res {
                Ok(()) => {
                    self.restart = None;
                    self.restart_error = None;
                }
                Err(e) => {
                    self.restart_error = Some(e);
                    self.restart = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_reply_does_not_overwrite_settings_draft_or_logs() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;
        controller.logs.frozen = Some(crate::logs::FrozenLog {
            content: "pre-existing log".into(),
            outcome: crate::logs::LogOutcome::Complete,
        });

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": "running",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));

        // Settings draft is preserved and still dirty because 5 != 7
        assert_eq!(controller.settings_draft.framerate, Some(7));
        assert!(controller.settings_draft.framerate_dirty);

        // Logs are untouched
        assert_eq!(
            controller.logs.frozen.as_ref().unwrap().content,
            "pre-existing log"
        );
    }

    #[test]
    fn test_clean_available_status_read_does_not_set_saved_verified() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": "running",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));
        assert!(!controller.settings_draft.saved_verified);
    }

    #[test]
    fn test_dirty_fields_clear_when_read_matches_requested() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": "running",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 7,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));
        assert!(!controller.settings_draft.framerate_dirty);
        assert!(!controller.settings_draft.saved_verified);
    }

    fn begin_frame_save(controller: &mut Controller) -> u64 {
        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;
        let owner = controller.bus_owner.clone().unwrap();
        controller.start_settings_save(
            &owner,
            SettingsModel {
                capture_framerate: 7,
                draw_cursor: true,
                start_paused: false,
                segment_interval: 300,
                activity_source: false,
                activity_titles: false,
                activity_urls: false,
                activity_exclude_apps: Vec::new(),
                activity_exclude_titles: Vec::new(),
                activity_through_pause: false,
                activity_only: false,
            },
        )
    }

    fn saved_frame_reply() -> String {
        CURRENT_RESPONSE.replace("\"capture_framerate\":1", "\"capture_framerate\":7")
    }

    fn fresh_reply(controller: &mut Controller, reply: Result<String, ControlClientError>) {
        let id = controller.alloc_id();
        controller.apply_status_reply(id, ":1.100", reply);
    }

    #[test]
    fn saved_requires_a_read_started_after_write_completion() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        let save_id = begin_frame_save(&mut controller);
        let older_read_id = controller.alloc_id();
        controller.apply_settings_write_reply(save_id, ":1.100", SettingsWriteOutcome::Complete);
        controller.apply_status_reply(older_read_id, ":1.100", Ok(saved_frame_reply()));
        assert!(!controller.settings_draft.saved_verified);
        assert!(controller.settings_draft.waiting_confirming_read);
        fresh_reply(&mut controller, Ok(saved_frame_reply()));
        assert!(controller.settings_draft.saved_verified);
        assert!(!controller.settings_draft.waiting_confirming_read);
    }

    #[test]
    fn failed_followup_read_does_not_report_save_success_or_definite_failure() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        let id = begin_frame_save(&mut controller);
        controller.apply_settings_write_reply(id, ":1.100", SettingsWriteOutcome::Complete);
        fresh_reply(&mut controller, Err(ControlClientError::Timeout));
        assert!(!controller.settings_draft.saved_verified);
        assert!(!controller.settings_draft.write_failed);
        assert!(controller.settings_draft.waiting_confirming_read);
        assert!(controller.settings_draft.framerate_dirty);
        fresh_reply(&mut controller, Ok(saved_frame_reply()));
        assert!(controller.settings_draft.saved_verified);
    }

    #[test]
    fn lost_reply_settles_when_a_fresh_read_establishes_every_requested_write() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        let id = begin_frame_save(&mut controller);
        controller.apply_settings_write_reply(id, ":1.100", SettingsWriteOutcome::Unknown);
        assert!(!controller.settings_draft.write_failed);
        assert!(controller.settings_draft.write_uncertain);
        fresh_reply(&mut controller, Ok(saved_frame_reply()));
        assert!(controller.settings_draft.saved_verified);
        assert!(!controller.settings_draft.write_uncertain);
    }

    #[test]
    fn partial_save_keeps_outstanding_edits_and_a_lost_reply_stays_unknown() {
        for outcome in [
            SettingsWriteOutcome::Rejected,
            SettingsWriteOutcome::Unknown,
        ] {
            let mut controller = Controller::new();
            controller.set_bus_owner(Some(":1.100".into()));
            controller.settings_draft.cursor = Some(false);
            controller.settings_draft.cursor_dirty = true;
            let id = begin_frame_save(&mut controller);
            controller.apply_settings_write_reply(id, ":1.100", outcome);
            fresh_reply(&mut controller, Ok(saved_frame_reply()));
            assert!(!controller.settings_draft.framerate_dirty);
            assert!(controller.settings_draft.cursor_dirty);
            assert!(!controller.settings_draft.saved_verified);
            assert_eq!(
                controller.settings_draft.write_failed,
                outcome == SettingsWriteOutcome::Rejected
            );
            assert_eq!(
                controller.settings_draft.write_uncertain,
                outcome == SettingsWriteOutcome::Unknown
            );
        }
    }

    #[test]
    fn edits_made_after_save_are_preserved_without_a_whole_form_saved_claim() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        let id = begin_frame_save(&mut controller);
        controller.settings_draft.framerate = Some(8);
        controller.settings_draft.framerate_dirty = true;
        controller.apply_settings_write_reply(id, ":1.100", SettingsWriteOutcome::Complete);
        fresh_reply(&mut controller, Ok(saved_frame_reply()));
        assert_eq!(controller.settings_draft.framerate, Some(8));
        assert!(controller.settings_draft.framerate_dirty);
        assert!(!controller.settings_draft.saved_verified);
        assert!(!controller.settings_draft.waiting_confirming_read);
    }

    fn sample_baseline() -> SettingsModel {
        SettingsModel {
            capture_framerate: 1,
            draw_cursor: true,
            start_paused: false,
            segment_interval: 300,
            activity_source: false,
            activity_titles: false,
            activity_urls: false,
            activity_exclude_apps: Vec::new(),
            activity_exclude_titles: Vec::new(),
            activity_through_pause: false,
            activity_only: false,
        }
    }

    #[test]
    fn test_changed_writes_contains_only_dirty_fields() {
        let mut draft = SettingsDraft::default();
        assert!(draft.changed_writes().is_empty());

        draft.framerate = Some(8);
        assert!(draft.changed_writes().is_empty()); // not dirty yet

        draft.framerate_dirty = true;
        assert_eq!(
            draft.changed_writes(),
            vec![("capture_framerate", "8".to_string())]
        );

        draft.cursor = Some(false);
        draft.cursor_dirty = true;
        draft.start_paused = Some(true);
        draft.start_paused_dirty = true;
        draft.segment = Some(15);
        draft.segment_dirty = true;

        assert_eq!(
            draft.changed_writes(),
            vec![
                ("capture_framerate", "8".to_string()),
                ("draw_cursor", "false".to_string()),
                ("start_paused", "true".to_string()),
                ("segment_interval", "15".to_string()),
            ]
        );
    }

    #[test]
    fn test_changed_writes_activity_policy_carries_only_changed_choices() {
        let draft = SettingsDraft {
            activity_titles: Some(false),
            activity_titles_dirty: true,
            activity_through_pause: Some(true),
            activity_through_pause_dirty: true,
            // Shown but untouched: not sent, so it keeps the app's default.
            activity_source: Some(true),
            ..SettingsDraft::default()
        };

        let writes = draft.changed_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "activity_policy");
        let val: serde_json::Value = serde_json::from_str(&writes[0].1).unwrap();
        assert_eq!(
            val,
            serde_json::json!({"activity_titles": false, "activity_through_pause": true})
        );

        let mut saved = sample_baseline();
        assert!(!SettingsDraft::matches_writes(&saved, &writes));
        saved.activity_through_pause = true;
        assert!(SettingsDraft::matches_writes(&saved, &writes));
    }

    #[test]
    fn save_applies_exclusions_before_content_reads() {
        use std::cell::Cell;

        use solstone_linux::activity::{ActivityFacts, BoundBackends};
        use solstone_linux::activity_prefs::{ActivityPolicy, ActivityPrefs};
        use solstone_linux::activity_source::{
            ActivitySampler, ForegroundProvider, ForegroundRead, ForegroundWindow, PollInput,
        };
        use solstone_linux::config::Config;
        use solstone_linux::observer::ActivityState;

        struct Sentinel {
            app: &'static str,
            title: &'static str,
            identity_reads: Cell<usize>,
            title_reads: Cell<usize>,
            url_reads: Cell<usize>,
        }

        impl ForegroundProvider for Sentinel {
            fn read_foreground(
                &mut self,
                want_title: bool,
                _facts: &ActivityFacts,
            ) -> ForegroundRead {
                let title = if want_title {
                    self.title_reads.set(self.title_reads.get() + 1);
                    Some(self.title.to_string())
                } else {
                    self.identity_reads.set(self.identity_reads.get() + 1);
                    None
                };
                ForegroundRead::Window(ForegroundWindow {
                    app: self.app.into(),
                    app_id: None,
                    title,
                    pid: Some(4),
                    window_id: 9,
                    window_token: String::new(),
                })
            }

            fn read_committed_url(&mut self, _pid: u32) -> Option<String> {
                self.url_reads.set(self.url_reads.get() + 1);
                Some("https://user:secret@sentinel.example/path?q=1#frag".into())
            }
        }

        fn poll(sampler: &mut ActivitySampler, prefs: &ActivityPrefs, provider: &mut Sentinel) {
            sampler.poll(
                &PollInput {
                    now_ms: 1_000,
                    prefs: prefs.snapshot(),
                    activity: ActivityState::default(),
                    bound: BoundBackends::default(),
                    paused: false,
                    route: "gnome-shell",
                },
                provider,
                &ActivityFacts::default(),
                &mut || true,
            );
        }

        fn withheld(blob: &str) {
            assert!(!blob.contains("SENTINEL_APP"));
            assert!(!blob.contains("SENTINEL_TITLE"));
            assert!(!blob.contains("sentinel.example"));
            assert!(!blob.contains("secret"));
            assert!(!blob.contains("Notes"));
        }

        // What the app applies: its current choices with the sent ones on top.
        fn applied(base: &Config, sent: &str) -> ActivityPolicy {
            let mut policy = serde_json::to_value(ActivityPolicy::from_config(base)).unwrap();
            let sent: serde_json::Value = serde_json::from_str(sent).unwrap();
            for (key, value) in sent.as_object().unwrap() {
                policy[key] = value.clone();
            }
            serde_json::from_value(policy).unwrap()
        }

        let off = Config {
            activity_source: false,
            ..Config::default()
        };
        let draft = SettingsDraft {
            activity_source: Some(true),
            activity_source_dirty: true,
            activity_titles: Some(true),
            activity_titles_dirty: true,
            activity_urls: Some(true),
            activity_urls_dirty: true,
            activity_exclude_apps: Some(vec!["SENTINEL_APP".into()]),
            activity_exclude_apps_dirty: true,
            activity_exclude_titles: Some(vec!["SENTINEL_TITLE".into()]),
            activity_exclude_titles_dirty: true,
            ..SettingsDraft::default()
        };
        let writes = draft.changed_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "activity_policy");

        let policy = applied(&off, &writes[0].1);
        assert!(policy.activity_source);
        assert!(policy.activity_titles);
        assert!(policy.activity_urls);
        assert_eq!(
            policy.activity_exclude_apps,
            vec!["SENTINEL_APP".to_string()]
        );
        assert_eq!(
            policy.activity_exclude_titles,
            vec!["SENTINEL_TITLE".to_string()]
        );

        let prefs = ActivityPrefs::new(&off);
        let mut sampler = ActivitySampler::new();
        let mut provider = Sentinel {
            app: "SENTINEL_APP",
            title: "SENTINEL_TITLE",
            identity_reads: Cell::new(0),
            title_reads: Cell::new(0),
            url_reads: Cell::new(0),
        };
        poll(&mut sampler, &prefs, &mut provider);
        sampler.seal(2_000);
        assert_eq!(provider.identity_reads.get(), 0);
        assert_eq!(provider.title_reads.get(), 0);
        assert_eq!(provider.url_reads.get(), 0);
        withheld(&serde_json::to_string(&sampler.take_pending()).unwrap());

        prefs.apply_policy(policy);
        poll(&mut sampler, &prefs, &mut provider);
        sampler.seal(2_000);
        assert_eq!(provider.identity_reads.get(), 1);
        assert_eq!(provider.title_reads.get(), 0);
        assert_eq!(provider.url_reads.get(), 0);
        withheld(&serde_json::to_string(&sampler.take_pending()).unwrap());

        let title_draft = SettingsDraft {
            activity_source: Some(true),
            activity_source_dirty: true,
            activity_titles: Some(true),
            activity_titles_dirty: true,
            activity_urls: Some(true),
            activity_urls_dirty: true,
            activity_exclude_titles: Some(vec!["SENTINEL_TITLE".into()]),
            activity_exclude_titles_dirty: true,
            ..SettingsDraft::default()
        };
        let title_writes = title_draft.changed_writes();
        assert_eq!(title_writes.len(), 1);
        assert_eq!(title_writes[0].0, "activity_policy");
        let title_policy = applied(&Config::default(), &title_writes[0].1);
        assert!(title_policy.activity_exclude_apps.is_empty());
        let title_prefs = ActivityPrefs::new(&Config::default());
        title_prefs.apply_policy(title_policy);
        let mut title_sampler = ActivitySampler::new();
        let mut title_provider = Sentinel {
            app: "Notes",
            title: "SENTINEL_TITLE",
            identity_reads: Cell::new(0),
            title_reads: Cell::new(0),
            url_reads: Cell::new(0),
        };
        poll(&mut title_sampler, &title_prefs, &mut title_provider);
        title_sampler.seal(2_000);
        assert_eq!(title_provider.identity_reads.get(), 1);
        assert_eq!(title_provider.title_reads.get(), 1);
        assert_eq!(title_provider.url_reads.get(), 0);
        withheld(&serde_json::to_string(&title_sampler.take_pending()).unwrap());
    }

    #[test]
    fn test_superseded_or_mismatched_owner_is_ignored() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        let id1 = controller.start_intake_command(":1.100", IntakeCommand::Pause(10));
        let id2 = controller.start_intake_command(":1.100", IntakeCommand::Resume);

        // Reply for old id1 is ignored
        controller.apply_intake_reply(id1, ":1.100", Err(ControlClientError::Timeout));
        assert!(controller.intake_error.is_none());
        assert_eq!(controller.intake_command.as_ref().unwrap().id, id2);

        // Reply for wrong owner is ignored
        controller.apply_intake_reply(id2, ":1.999", Err(ControlClientError::NoOwner));
        assert!(controller.intake_error.is_none());
    }

    #[test]
    fn test_pause_ok_does_not_complete_until_subsequent_status_matches() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        let id = controller.start_intake_command(":1.100", IntakeCommand::Pause(60));
        controller.apply_intake_reply(id, ":1.100", Ok("ok".into()));

        // In-flight command is NOT completed yet
        assert!(controller.intake_command.is_some());

        // Status still says recording
        let rec_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": "running",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            }
        }"#;
        controller.apply_status_reply(id, ":1.100", Ok(rec_json.into()));
        assert!(controller.intake_command.is_some());

        // Now status says paused
        let pau_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": "running",
            "health": "connected",
            "capture": "paused",
            "pause_bounded": true,
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            }
        }"#;
        let read_id = controller.alloc_id();
        controller.apply_status_reply(read_id, ":1.100", Ok(pau_json.into()));
        assert!(controller.intake_command.is_none());
    }

    #[test]
    fn test_transport_error_decodes_to_unknown() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.apply_status_reply(1, ":1.100", Err(ControlClientError::Timeout));
        assert_eq!(controller.document.view, SessionView::Unknown);
        assert!(!controller.document.saved_available);
    }

    #[test]
    fn intake_read_started_before_dispatch_completion_cannot_enable_the_next_change() {
        let mut c = Controller::new();
        c.set_bus_owner(Some(":1.100".into()));
        let id = c.start_intake_command(":1.100", IntakeCommand::Pause(0));
        let early_read = c.alloc_id();
        let paused = CURRENT_RESPONSE.replace("recording", "paused");
        c.apply_status_reply(early_read, ":1.100", Ok(paused.clone()));
        assert!(c.intake_command.is_some());
        c.apply_intake_reply(id, ":1.100", Ok("ok".into()));
        c.apply_status_reply(early_read, ":1.100", Ok(paused.clone()));
        assert!(c.intake_command.is_some());
        let fresh_read = c.alloc_id();
        c.apply_status_reply(fresh_read, ":1.100", Ok(paused));
        assert!(c.intake_command.is_none());
    }

    #[test]
    fn acknowledged_intake_change_has_a_bound_and_only_fresh_matching_state_clears_uncertainty() {
        for command in [IntakeCommand::Pause(0), IntakeCommand::Resume] {
            let mut c = Controller::new();
            c.set_bus_owner(Some(":1.100".into()));
            c.document = decode_control_str(CURRENT_RESPONSE).unwrap();
            let id = c.start_intake_command(":1.100", command.clone());
            c.apply_intake_reply(id, ":1.100", Ok("ok".into()));
            assert!(c.intake_command.is_some());
            c.expire_intake(c.intake_deadline.unwrap());
            assert!(c.intake_command.is_none());
            assert!(c.intake_error_key.is_some());
            let id = c.alloc_id();
            c.apply_status_reply(id, ":1.100", Err(ControlClientError::Timeout));
            assert!(c.intake_error_key.is_some());
            let reply = match command {
                IntakeCommand::Pause(_) => CURRENT_RESPONSE.replace("recording", "paused"),
                IntakeCommand::Resume => CURRENT_RESPONSE.into(),
            };
            let id = c.alloc_id();
            c.apply_status_reply(id, ":1.100", Ok(reply));
            assert!(c.intake_error_key.is_none());
        }
    }

    #[test]
    fn save_reconciliation_survives_owner_replacement_without_erasing_partial_edits() {
        for complete in [false, true] {
            let mut c = Controller::new();
            c.set_bus_owner(Some(":1.old".into()));
            c.document = decode_control_str(CURRENT_RESPONSE).unwrap();
            c.settings_draft.framerate = Some(7);
            c.settings_draft.framerate_dirty = true;
            c.settings_draft.cursor = Some(false);
            c.settings_draft.cursor_dirty = true;
            let wanted = SettingsModel {
                capture_framerate: 7,
                draw_cursor: false,
                ..c.document.settings.clone()
            };
            let write_id = c.start_settings_save(":1.old", wanted);
            c.set_bus_owner(Some(":1.new".into()));
            c.apply_settings_write_reply(write_id, ":1.old", SettingsWriteOutcome::Complete);
            assert!(c.settings_draft.waiting_confirming_read);
            let mut reply = response_with_effective(7);
            if complete {
                reply = reply.replace("\"draw_cursor\":true", "\"draw_cursor\":false");
            }
            let id = c.alloc_id();
            c.apply_status_reply(id, ":1.new", Ok(reply));
            assert_eq!(c.settings_draft.saved_verified, complete);
            assert!(!c.settings_draft.framerate_dirty);
            assert_eq!(c.settings_draft.cursor_dirty, !complete);
            assert_eq!(c.settings_draft.write_uncertain, !complete);
        }
    }

    #[test]
    fn new_save_and_changed_instance_facts_invalidate_previous_restart_success() {
        let mut c = Controller::new();
        c.set_bus_owner(Some(":1.old".into()));
        c.document = decode_control_str(&response_with_effective(1)).unwrap();
        c.restart_verified = true;
        begin_frame_save(&mut c);
        assert!(!c.restart_verified);
        c.restart_verified = true;
        let mismatch = response_with_effective(7).replace(
            "\"effective\":{\"capture_framerate\":7",
            "\"effective\":{\"capture_framerate\":1",
        );
        c.apply_status_reply(100, ":1.old", Ok(mismatch));
        assert!(!c.restart_verified);
        c.restart_verified = true;
        c.set_bus_owner(None);
        assert!(!c.restart_verified);
    }

    #[test]
    fn confirming_one_mark_cannot_hide_a_later_replacement_ceremony() {
        let mark = r##"{"icon1":{"name":"circle","svg":"<circle cx='12' cy='12' r='8'/>","color":{"name":"amber","hex":"#f59e0b"},"rot":0},"icon2":{"name":"circle","svg":"<circle cx='12' cy='12' r='8'/>","color":{"name":"magenta","hex":"#c026d3"},"rot":45},"words":["afoot","unfixed"]}"##;
        let pending = format!(
            "{},\"attempt\":{{\"id\":\"a\",\"pairing_id\":\"pair-a\",\"state\":\"pending\",\"mark\":\"afoot·unfixed\"}},\"pending_mark\":{mark}}}",
            CURRENT_RESPONSE.strip_suffix('}').unwrap()
        );
        let confirmed = format!(
            "{},\"confirmed_mark\":{mark}}}",
            CURRENT_RESPONSE.strip_suffix('}').unwrap()
        );
        let mut c = Controller::new();
        c.set_bus_owner(Some(":1.100".into()));
        c.apply_status_reply(1, ":1.100", Ok(pending));
        let shown = c.pairing_presentation().unwrap();
        let (id, _) = c.answer_presented(&shown, true).unwrap();
        c.apply_pairing_reply(id, ":1.100", Ok(confirmed.clone()));
        let id = c.alloc_id();
        c.apply_status_reply(id, ":1.100", Ok(confirmed.clone()));
        assert_eq!(c.pairing_attempt_outcome.as_deref(), Some("confirmed"));
        let id = c.start_pairing(":1.100", PairingCommand::Begin("new link".into()));
        let replacement_mark = mark.replace("afoot", "verse");
        let replacement = format!(
            "{},\"attempt\":{{\"id\":\"b\",\"pairing_id\":\"pair-b\",\"state\":\"pending\",\"mark\":\"verse·unfixed\"}},\"pending_mark\":{replacement_mark}}}",
            confirmed.strip_suffix('}').unwrap()
        );
        c.apply_pairing_reply(id, ":1.100", Ok(replacement.clone()));
        let id = c.alloc_id();
        c.apply_status_reply(id, ":1.100", Ok(replacement));
        assert_eq!(c.pairing_presentation().unwrap().attempt_id, "b");
        assert!(
            crate::copy_map::map_pairing(&c.document, c.pairing_attempt_outcome.as_deref())
                .confirm_action
                .is_some()
        );
    }

    fn response_with_effective(frame: i64) -> String {
        CURRENT_RESPONSE.replace("\"saved_available\":true}",
            "\"saved_available\":true,\"effective\":{\"capture_framerate\":1,\"draw_cursor\":true,\"start_paused\":false,\"segment_interval\":300}}")
            .replace("\"capture_framerate\":1", &format!("\"capture_framerate\":{frame}"))
    }

    #[test]
    fn restart_waits_for_a_new_owned_instance_and_verified_saved_effective_values() {
        let wanted = decode_control_str(&response_with_effective(7))
            .unwrap()
            .settings;
        for (owner, pid, service_pid, reply) in [
            (":1.old", Some(40), Some(40), Ok(response_with_effective(7))),
            (":1.new", Some(40), Some(40), Ok(response_with_effective(7))),
            (":1.old", Some(41), Some(41), Ok(response_with_effective(7))),
            (":1.new", Some(41), Some(99), Ok(response_with_effective(7))),
            (":1.new", Some(41), Some(41), Ok(response_with_effective(8))),
            (
                ":1.new",
                Some(41),
                Some(41),
                Ok(response_with_effective(7)
                    .replace("\"saved_available\":true", "\"saved_available\":false")),
            ),
            (
                ":1.new",
                Some(41),
                Some(41),
                Err(ControlClientError::Timeout),
            ),
        ] {
            let mut c = Controller::new();
            c.set_bus_owner(Some(":1.old".into()));
            c.start_service_restart(":1.old");
            c.restart_expected = Some((
                ":1.old".into(),
                40,
                Some(wanted.clone()),
                std::time::Instant::now(),
            ));
            c.observe_service_reply(10, owner, pid, service_pid, reply);
            assert!(!c.restart_verified);
            assert!(c.restart.is_some());
            c.observe_service_reply(
                11,
                ":1.new",
                Some(41),
                Some(41),
                Ok(response_with_effective(7)),
            );
            assert!(c.restart_verified);
            assert!(c.restart.is_none());
        }
    }

    #[test]
    fn older_daemon_recovery_uses_fresh_settings_without_pre_restart_defaults() {
        let mut c = Controller::new();
        c.observe_service_reply(
            1,
            ":1.old",
            Some(40),
            Some(40),
            Err(ControlClientError::UnknownMethod),
        );
        assert!(c.older_daemon);
        assert!(!c.document.saved_available);
        c.start_service_restart(":1.old");
        c.restart_expected = Some((":1.old".into(), 40, None, std::time::Instant::now()));
        c.observe_service_reply(
            2,
            ":1.old",
            Some(40),
            Some(40),
            Ok(response_with_effective(9)),
        );
        assert!(!c.restart_verified);
        c.observe_service_reply(
            3,
            ":1.new",
            Some(41),
            Some(41),
            Ok(response_with_effective(9)),
        );
        assert!(c.restart_verified);
    }

    #[test]
    fn start_acknowledgement_is_not_a_status_confirmation() {
        let mut c = Controller::new();
        c.start_service_start("");
        c.observe_service_reply(
            2,
            ":1.new",
            Some(41),
            Some(41),
            Err(ControlClientError::Timeout),
        );
        assert!(c.start.is_some());
        c.observe_service_reply(3, ":1.new", Some(41), Some(41), Ok(CURRENT_RESPONSE.into()));
        assert!(c.start.is_none());
    }

    #[test]
    fn unsupported_pairing_reply_invalidates_the_previous_presentation() {
        let mut c = Controller::new();
        c.set_bus_owner(Some(":1.old".into()));
        c.document = decode_control_str(CURRENT_RESPONSE).unwrap();
        let id = c.start_pairing(":1.old", PairingCommand::Begin("pending".into()));
        c.apply_pairing_reply(
            id,
            ":1.old",
            Ok(CURRENT_RESPONSE.replace("\"v\":1", "\"v\":99")),
        );
        assert_eq!(c.document.view, SessionView::Unknown);
        assert!(c.pairing_presentation().is_none());
    }

    const CURRENT_RESPONSE: &str = r#"{"v":1,"durable":"confirmed","owner":"running","health":"connected","capture":"recording","settings":{"capture_framerate":1,"draw_cursor":true,"start_paused":false,"segment_interval":300},"saved_available":true}"#;

    #[test]
    fn failed_reads_do_not_consume_drafts_or_finish_resume() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        controller.settings_draft.framerate = Some(1);
        controller.settings_draft.framerate_dirty = true;
        controller.start_intake_command(":1.100", IntakeCommand::Resume);
        controller.apply_status_reply(2, ":1.100", Err(ControlClientError::Timeout));
        assert!(controller.settings_draft.framerate_dirty);
        assert!(controller.intake_command.is_some());
        assert!(!controller.settings_draft.saved_verified);

        let unreadable =
            CURRENT_RESPONSE.replace("\"saved_available\":true", "\"saved_available\":false");
        controller.apply_status_reply(3, ":1.100", Ok(unreadable));
        assert!(controller.settings_draft.framerate_dirty);
    }

    #[test]
    fn owner_loss_invalidates_status_and_fences_old_mutation_replies() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        controller.apply_status_reply(1, ":1.100", Ok(CURRENT_RESPONSE.into()));
        assert_eq!(controller.document.view, SessionView::Confirmed);
        let pairing_id =
            controller.start_pairing(":1.100", PairingCommand::Begin("synthetic".into()));
        let save_id =
            controller.start_settings_save(":1.100", controller.document.settings.clone());
        controller.set_bus_owner(None);
        assert_eq!(controller.document.view, SessionView::Unknown);
        controller.set_bus_owner(Some(":1.200".into()));
        controller.apply_pairing_reply(pairing_id, ":1.100", Ok(CURRENT_RESPONSE.into()));
        controller.apply_settings_write_reply(save_id, ":1.100", SettingsWriteOutcome::Complete);
        assert_eq!(controller.document.view, SessionView::Unknown);
        assert!(!controller.settings_draft.saved_verified);
    }

    #[test]
    fn older_status_cannot_replace_newer_success_or_unavailability() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        controller.apply_status_reply(20, ":1.100", Ok(CURRENT_RESPONSE.into()));
        controller.apply_status_reply(19, ":1.100", Err(ControlClientError::Timeout));
        assert_eq!(controller.document.view, SessionView::Confirmed);
        controller.apply_status_reply(21, ":1.100", Err(ControlClientError::Timeout));
        controller.apply_status_reply(20, ":1.100", Ok(CURRENT_RESPONSE.into()));
        assert_eq!(controller.document.view, SessionView::Unknown);
    }

    #[test]
    fn answer_is_bound_to_the_displayed_attempt_and_mark() {
        use solstone_linux::copy::CopyKey;
        use solstone_linux::window_control::{AttemptModel, MarkColorDocument, MarkIconDocument};
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));
        controller.document = decode_control_str(CURRENT_RESPONSE).unwrap();
        controller.document.view = SessionView::Pending;
        controller.document.durable = "unpaired".into();
        controller.document.owner = "absent".into();
        controller.document.health = "not-reported".into();
        controller.document.attempt = Some(AttemptModel {
            id: "candidate-a".into(),
            pairing_id: "ceremony-a".into(),
            state: "pending".into(),
            mark: Some("afoot·unfixed".into()),
        });
        let icon = MarkIconDocument {
            name: "circle".into(),
            svg: "<circle cx='12' cy='12' r='8'/>".into(),
            color: MarkColorDocument {
                name: "amber".into(),
                hex: "#f59e0b".into(),
            },
            rot: 0,
        };
        controller.document.pending_mark = Some(MarkRenderDocument {
            icon1: icon.clone(),
            icon2: icon,
            words: ["afoot".into(), "unfixed".into()],
        });
        let shown = controller.pairing_presentation().unwrap();
        controller.document.attempt.as_mut().unwrap().id = "candidate-b".into();
        controller.document.attempt.as_mut().unwrap().pairing_id = "ceremony-b".into();
        controller.document.pending_mark.as_mut().unwrap().words[0] = "verse".into();
        assert_eq!(
            controller.answer_presented(&shown, true),
            Err(CopyKey::PairingStale)
        );
        assert!(controller.pairing.is_none());

        let current = controller.pairing_presentation().unwrap();
        let (_, command) = controller.answer_presented(&current, true).unwrap();
        assert_eq!(
            command,
            PairingCommand::Answer {
                attempt_id: current.attempt_id.clone(),
                pairing_id: current.pairing_id.clone(),
                accept: true,
            }
        );
        assert_eq!(
            controller.answer_presented(&current, true),
            Err(CopyKey::PairingStale)
        );
        controller.set_bus_owner(None);
        assert_eq!(
            controller.answer_presented(&current, true),
            Err(CopyKey::PairingStale)
        );
    }
}
