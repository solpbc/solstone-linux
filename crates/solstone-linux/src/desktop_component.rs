// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{config::Config, observer::StateSnapshot, sync_health::SyncHealth, tray_model};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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
        let last_status = match tray_model::status(snapshot) {
            tray_model::TrayStatus::Paused => "paused",
            tray_model::TrayStatus::Recording => "recording",
            tray_model::TrayStatus::Idle => "idle",
            tray_model::TrayStatus::Stopped => "stopped",
        }
        .to_owned();
        Self {
            last_status,
            last_sync: format!("{}:{}", health.dbus, progress),
        }
    }
    pub fn snapshot_changed(&mut self, s: &StateSnapshot) -> Option<ComponentSignal> {
        let value = match tray_model::status(s) {
            tray_model::TrayStatus::Paused => "paused",
            tray_model::TrayStatus::Recording => "recording",
            tray_model::TrayStatus::Idle => "idle",
            tray_model::TrayStatus::Stopped => "stopped",
        }
        .to_owned();
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

pub struct DesktopComponent {
    pub config: Config,
    disabled: Arc<AtomicBool>,
}
impl DesktopComponent {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            disabled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }
    /// Setup must be called from `observer::lifecycle`'s Setup closure, before recovery.
    pub fn setup(&self, mut register: impl FnMut() -> bool, mut wait: impl FnMut()) -> bool {
        for attempt in 0..3 {
            if register() {
                return true;
            }
            if attempt < 2 {
                tracing::info!("SNI watcher retry {}/2...", attempt + 1);
                wait();
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
    pub fn journal_url(&self) -> &str {
        if self.config.server_url.is_empty() {
            "https://solstone.app"
        } else {
            &self.config.server_url
        }
    }
    pub fn perform_desktop_command(&self, command: crate::tray::TrayCommand) -> Result<(), String> {
        match command {
            crate::tray::TrayCommand::OpenJournal => {
                open::that_detached(self.journal_url()).map_err(|e| e.to_string())
            }
            crate::tray::TrayCommand::OpenConfig => {
                open::that_detached(self.config.config_path()).map_err(|e| e.to_string())
            }
            crate::tray::TrayCommand::CopyInstructions => {
                let text = crate::clipboard::agent_instructions(
                    &self.config.config_path().display().to_string(),
                    &self.config.captures_dir().display().to_string(),
                );
                let wayland = std::env::var_os("XDG_SESSION_TYPE").is_some_and(|v| v == "wayland")
                    || std::env::var_os("WAYLAND_DISPLAY").is_some();
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
            server_url: "x".into(),
            ..Default::default()
        };
        let d = DesktopComponent::new(c.clone());
        assert_eq!(d.config, c)
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
            || waits += 1
        ));
        assert_eq!((calls, waits), (3, 2));
    }
    #[test]
    fn public_journal_fallback() {
        assert_eq!(
            DesktopComponent::new(Config::default()).journal_url(),
            "https://solstone.app"
        )
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
            Some(ComponentSignal::SyncProgressChanged("unknown:3/10".into()))
        );
        assert_eq!(s.sync_changed(&h, "3/10"), None)
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
}
