// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::Config, observer::StateSnapshot, private_link::OpenJournalAccess,
    sync_health::SyncHealth, tray_model,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub const OBSERVER_BUS_NAME: &str = "org.solpbc.solstone.Observer1";
pub const ALREADY_RUNNING_MESSAGE: &str = "Another solstone app process is already running (owns org.solpbc.solstone.Observer1). Check: systemctl --user status solstone-linux";
pub(crate) const OPEN_JOURNAL_REMEDIATION: &str =
    "Could not open your journal. Wait for the solstone app to reconnect, then try again.";

pub trait BusNameRequester {
    fn request_name(
        &self,
        name: &str,
        flag: zbus::fdo::RequestNameFlags,
    ) -> Result<zbus::fdo::RequestNameReply, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentSignal {
    StatusChanged(String),
    SyncProgressChanged(String),
}
pub struct SignalState {
    last_status: String,
    last_sync: String,
}
impl SignalState {
    pub fn new(snapshot: &StateSnapshot, health: &SyncHealth, progress: &str) -> Self {
        let last_status = tray_model::status_name(tray_model::status(snapshot)).to_owned();
        Self {
            last_status,
            last_sync: format!("{}:{}", health.dbus, progress),
        }
    }
    pub fn snapshot_changed(&mut self, s: &StateSnapshot) -> Option<ComponentSignal> {
        let value = tray_model::status_name(tray_model::status(s)).to_owned();
        if value == self.last_status {
            return None;
        }
        self.last_status = value.clone();
        Some(ComponentSignal::StatusChanged(value))
    }
    pub fn sync_changed(&mut self, health: &SyncHealth, progress: &str) -> Option<ComponentSignal> {
        let value = format!("{}:{}", health.dbus, progress);
        if value == self.last_sync {
            return None;
        }
        self.last_sync = value.clone();
        Some(ComponentSignal::SyncProgressChanged(value))
    }
}

#[derive(Clone)]
pub struct DesktopComponent {
    pub config: Config,
    disabled: Arc<AtomicBool>,
    open_journal: OpenJournalAccess,
}
impl DesktopComponent {
    pub fn new(config: Config) -> Self {
        Self::with_open_journal(config, OpenJournalAccess::default())
    }
    pub(crate) fn with_open_journal(config: Config, open_journal: OpenJournalAccess) -> Self {
        Self {
            config,
            disabled: Arc::new(AtomicBool::new(false)),
            open_journal,
        }
    }
    pub fn disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }
    /// Acquire this singleton before capture, registration, export, recovery, or any other side
    /// effect.
    pub fn acquire_singleton(
        &self,
        bus: &impl BusNameRequester,
        mut log: impl FnMut(&str),
    ) -> bool {
        match bus.request_name(
            std::env::var("SOLSTONE_LINUX_BUS_NAME")
                .unwrap_or_else(|_| OBSERVER_BUS_NAME.to_owned())
                .as_str(),
            zbus::fdo::RequestNameFlags::DoNotQueue,
        ) {
            Ok(
                zbus::fdo::RequestNameReply::PrimaryOwner
                | zbus::fdo::RequestNameReply::AlreadyOwner,
            ) => true,
            Ok(_) => {
                log(ALREADY_RUNNING_MESSAGE);
                false
            }
            Err(error) => {
                tracing::error!(%error, "Failed to acquire Observer1 bus name");
                false
            }
        }
    }
    pub fn setup(
        &self,
        mut register: impl FnMut() -> bool,
        mut wait: impl FnMut(std::time::Duration),
    ) -> bool {
        for attempt in 0..3 {
            if register() {
                return true;
            }
            if attempt < 2 {
                tracing::info!("SNI watcher retry {}/2...", attempt + 1);
                wait(std::time::Duration::from_secs(1));
            }
        }
        tracing::info!("No StatusNotifierWatcher available");
        false
    }
    pub async fn watch_until_lost(
        &self,
        mut receiver: tokio::sync::watch::Receiver<StateSnapshot>,
        mut render: impl FnMut(&StateSnapshot) -> Result<(), String>,
    ) {
        let mut cadence = tokio::time::interval(std::time::Duration::from_secs(1));
        cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut dirty = true;
        loop {
            tokio::select! {
                changed=receiver.changed()=>match changed { Ok(())=>{receiver.borrow_and_update();dirty=true},Err(_)=>{tracing::warn!("observer snapshot subscription lost; disabling tray");self.disabled.store(true,Ordering::Release);return}},
                _=cadence.tick()=>{if dirty || receiver.has_changed().unwrap_or(false){let snapshot=receiver.borrow_and_update().clone();if let Err(error)=render(&snapshot){tracing::warn!(%error,"tray model recompute failed; retaining last known layout");}dirty=false}else{let snapshot=receiver.borrow().clone();if let Err(error)=render(&snapshot){tracing::warn!(%error,"tray countdown recompute failed; retaining last known layout");}}}
            }
        }
    }
    pub fn command_url<'a>(&'a self, command: &'a crate::tray::TrayCommand) -> Option<&'a str> {
        match command {
            crate::tray::TrayCommand::OpenUrl(url) => Some(url),
            _ => None,
        }
    }
    pub fn perform_desktop_command(&self, command: crate::tray::TrayCommand) -> Result<(), String> {
        match command {
            crate::tray::TrayCommand::OpenJournal => self
                .open_journal
                .open()
                .map_err(|_| OPEN_JOURNAL_REMEDIATION.into()),
            crate::tray::TrayCommand::OpenUrl(url) => {
                open::that_detached(url).map_err(|e| e.to_string())
            }
            crate::tray::TrayCommand::OpenConfig => {
                open::that_detached(self.config.config_path()).map_err(|e| e.to_string())
            }
            crate::tray::TrayCommand::CopyInstructions => {
                let text = crate::clipboard::agent_instructions(
                    &self.config.config_path().display().to_string(),
                    &self.config.captures_dir().display().to_string(),
                );
                let session_type = std::env::var_os("XDG_SESSION_TYPE");
                let display = std::env::var_os("WAYLAND_DISPLAY");
                let wayland =
                    crate::clipboard::is_wayland(session_type.as_deref(), display.as_deref());
                crate::clipboard::copy(&text, wayland)
                    .map_err(|e| e.to_string())
                    .and_then(|ok| {
                        if ok {
                            Ok(())
                        } else {
                            Err("clipboard command exited unsuccessfully".into())
                        }
                    })
            }
            crate::tray::TrayCommand::Pause(_)
            | crate::tray::TrayCommand::PauseIndefinite
            | crate::tray::TrayCommand::Resume => {
                Err("observer command must be routed by the run-loop owner".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::Mode;
    fn snap() -> StateSnapshot {
        StateSnapshot {
            mode: Mode::Idle,
            paused: false,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
        }
    }
    #[test]
    fn component_uses_config() {
        let c = Config {
            base_dir: "/tmp/solstone-test".into(),
            ..Default::default()
        };
        let d = DesktopComponent::new(c.clone());
        assert_eq!(d.config, c);
        assert_eq!(
            d.config.captures_dir(),
            std::path::PathBuf::from("/tmp/solstone-test/captures")
        );
        assert_eq!(d.config.config_path(), c.config_path());
    }
    #[test]
    fn setup_retries_three_times() {
        let d = DesktopComponent::new(Config::default());
        let mut calls = 0;
        let mut waits = 0;
        assert!(!d.setup(
            || {
                calls += 1;
                false
            },
            |duration| {
                assert_eq!(duration, std::time::Duration::from_secs(1));
                waits += 1
            }
        ));
        assert_eq!((calls, waits), (3, 2));
    }
    #[test]
    fn public_links_remain_distinct_from_open_journal() {
        let component = DesktopComponent::new(Config::default());
        let commands = [
            (
                crate::tray::TrayCommand::OpenUrl("https://solstone.app/observers"),
                "https://solstone.app/observers",
            ),
            (
                crate::tray::TrayCommand::OpenUrl("https://github.com/solpbc/solstone-linux"),
                "https://github.com/solpbc/solstone-linux",
            ),
            (
                crate::tray::TrayCommand::OpenUrl("https://solpbc.org/privacy"),
                "https://solpbc.org/privacy",
            ),
        ];
        for (command, expected) in &commands {
            assert_eq!(component.command_url(command), Some(*expected));
        }
        assert_eq!(
            component.command_url(&crate::tray::TrayCommand::OpenJournal),
            None
        );
    }
    #[test]
    fn unavailable_open_journal_has_one_owner_visible_remediation() {
        assert_eq!(
            DesktopComponent::new(Config::default())
                .perform_desktop_command(crate::tray::TrayCommand::OpenJournal),
            Err(
                "Could not open your journal. Wait for the solstone app to reconnect, then try again."
                    .to_owned()
            )
        );
    }
    #[test]
    fn no_transition_means_no_signal() {
        let health = crate::sync_health::derive_health(&Default::default(), 0.0, 600.0);
        let s = snap();
        let mut state = SignalState::new(&s, &health, "");
        assert_eq!(state.snapshot_changed(&s), None)
    }
    #[test]
    fn sync_payload_is_composite_and_deduped() {
        let h = crate::sync_health::derive_health(
            &crate::sync_health::SyncFacts::default(),
            0.0,
            600.0,
        );
        let mut s = SignalState::new(&snap(), &h, "");
        assert_eq!(
            s.sync_changed(&h, "3/10"),
            Some(ComponentSignal::SyncProgressChanged(
                "not-reported:3/10".into()
            ))
        );
        assert_eq!(s.sync_changed(&h, "3/10"), None)
    }
    #[test]
    fn pause_emits_paused_status() {
        let health = crate::sync_health::derive_health(&Default::default(), 0.0, 600.0);
        let initial = snap();
        let mut state = SignalState::new(&initial, &health, "");
        let mut paused = initial;
        paused.paused = true;
        assert_eq!(
            state.snapshot_changed(&paused),
            Some(ComponentSignal::StatusChanged("paused".into()))
        );
    }
    #[test]
    fn resume_emits_status_for_current_mode() {
        let health = crate::sync_health::derive_health(&Default::default(), 0.0, 600.0);
        for (mode, expected) in [(Mode::Screencast, "recording"), (Mode::Idle, "idle")] {
            let mut paused = snap();
            paused.mode = mode;
            paused.paused = true;
            let mut state = SignalState::new(&paused, &health, "");
            let mut resumed = paused;
            resumed.paused = false;
            assert_eq!(
                state.snapshot_changed(&resumed),
                Some(ComponentSignal::StatusChanged(expected.into()))
            );
        }
    }
    #[test]
    fn auto_resume_emits_current_mode_status() {
        let health = crate::sync_health::derive_health(&Default::default(), 0.0, 600.0);
        let mut paused = snap();
        paused.mode = Mode::Screencast;
        paused.paused = true;
        paused.pause_until = Some(1.0);
        let mut state = SignalState::new(&paused, &health, "");
        paused.paused = false;
        paused.pause_until = None;
        assert_eq!(
            state.snapshot_changed(&paused),
            Some(ComponentSignal::StatusChanged("recording".into()))
        );
    }
    #[test]
    fn mode_boundary_emits_new_status() {
        let health = crate::sync_health::derive_health(&Default::default(), 0.0, 600.0);
        let initial = snap();
        let mut state = SignalState::new(&initial, &health, "");
        let mut recording = initial;
        recording.mode = Mode::Screencast;
        assert_eq!(
            state.snapshot_changed(&recording),
            Some(ComponentSignal::StatusChanged("recording".into()))
        );
    }
    #[test]
    fn progress_change_emits_syncing_composite() {
        let health = crate::sync_health::derive_health(
            &crate::sync_health::SyncFacts {
                in_progress: true,
                progress: "30s until probe".into(),
                link: Some(crate::private_link::LinkFactState {
                    observer_registered: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            0.0,
            600.0,
        );
        let initial = snap();
        let mut state = SignalState::new(&initial, &health, "");
        assert_eq!(
            state.sync_changed(&health, "30s until probe"),
            Some(ComponentSignal::SyncProgressChanged(
                "syncing:30s until probe".into()
            ))
        );
    }
    #[tokio::test]
    async fn sender_drop_disables_without_panic() {
        let (sink, rx) = crate::observer::WatchStateSink::channel(snap());
        let d = DesktopComponent::new(Config::default());
        drop(sink);
        d.watch_until_lost(rx, |_| Ok(())).await;
        assert!(d.disabled())
    }
    #[tokio::test]
    async fn recompute_failure_keeps_task_alive_until_terminal_loss() {
        let (mut sink, rx) = crate::observer::WatchStateSink::channel(snap());
        let d = DesktopComponent::new(Config::default());
        let disabled = d.disabled.clone();
        let task =
            tokio::spawn(async move { d.watch_until_lost(rx, |_| Err("boom".into())).await });
        crate::observer::StateSink::publish(&mut sink, snap());
        tokio::task::yield_now().await;
        assert!(!disabled.load(Ordering::Acquire));
        drop(sink);
        assert!(task.await.is_ok())
    }
    #[tokio::test(start_paused = true)]
    async fn burst_has_a_trailing_render_at_one_hz() {
        let (mut sink, rx) = crate::observer::WatchStateSink::channel(snap());
        let d = DesktopComponent::new(Config::default());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let task = tokio::spawn(async move {
            d.watch_until_lost(rx, move |_| {
                seen.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
            .await
        });
        tokio::task::yield_now().await;
        for _ in 0..5 {
            crate::observer::StateSink::publish(&mut sink, snap());
        }
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::Acquire), 1);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::Acquire), 2);
        drop(sink);
        assert!(task.await.is_ok())
    }
    #[test]
    fn lifecycle_setup_precedes_recovery() {
        let config = Config::default();
        let order = std::cell::RefCell::new(Vec::new());
        let code = crate::observer::lifecycle(
            &config,
            || {
                order.borrow_mut().push("setup");
                true
            },
            |_, _| order.borrow_mut().push("recovery"),
            || Ok(()),
            || Ok(()),
            || false,
        );
        assert_eq!(code, 0);
        assert_eq!(*order.borrow(), ["setup", "recovery"]);
    }
    struct NameBus {
        reply: zbus::fdo::RequestNameReply,
        requested: std::cell::RefCell<Vec<(String, zbus::fdo::RequestNameFlags)>>,
    }
    impl BusNameRequester for NameBus {
        fn request_name(
            &self,
            name: &str,
            flag: zbus::fdo::RequestNameFlags,
        ) -> Result<zbus::fdo::RequestNameReply, String> {
            self.requested.borrow_mut().push((name.to_owned(), flag));
            Ok(match self.reply {
                zbus::fdo::RequestNameReply::PrimaryOwner => {
                    zbus::fdo::RequestNameReply::PrimaryOwner
                }
                zbus::fdo::RequestNameReply::InQueue => zbus::fdo::RequestNameReply::InQueue,
                zbus::fdo::RequestNameReply::Exists => zbus::fdo::RequestNameReply::Exists,
                zbus::fdo::RequestNameReply::AlreadyOwner => {
                    zbus::fdo::RequestNameReply::AlreadyOwner
                }
            })
        }
    }
    #[test]
    fn singleton_name_taken_logs_and_lifecycle_exits_one_before_recovery() {
        let bus = NameBus {
            reply: zbus::fdo::RequestNameReply::Exists,
            requested: Default::default(),
        };
        let component = DesktopComponent::new(Config::default());
        let messages = std::cell::RefCell::new(Vec::new());
        let recovered = std::cell::Cell::new(false);
        let config = Config::default();
        let code = crate::observer::lifecycle(
            &config,
            || {
                component.acquire_singleton(&bus, |message| {
                    messages.borrow_mut().push(message.to_owned())
                })
            },
            |_, _| recovered.set(true),
            || Ok(()),
            || Ok(()),
            || false,
        );
        assert_eq!(code, 1);
        assert!(!recovered.get());
        assert_eq!(*messages.borrow(), [ALREADY_RUNNING_MESSAGE]);
        assert_eq!(
            *bus.requested.borrow(),
            [(
                OBSERVER_BUS_NAME.to_owned(),
                zbus::fdo::RequestNameFlags::DoNotQueue
            )]
        );
    }
    #[test]
    fn singleton_accepts_primary_and_already_owner() {
        for reply in [
            zbus::fdo::RequestNameReply::PrimaryOwner,
            zbus::fdo::RequestNameReply::AlreadyOwner,
        ] {
            let bus = NameBus {
                reply,
                requested: Default::default(),
            };
            assert!(DesktopComponent::new(Config::default()).acquire_singleton(&bus, |_| {}));
        }
    }
}
