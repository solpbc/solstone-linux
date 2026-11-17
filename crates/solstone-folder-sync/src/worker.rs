// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Runtime plumbing for a folder source. A source owns its durable state,
//! commands, cadence, pause rules, upload preparation and reply interpretation.

use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    sync::{Notify, mpsc},
    task::JoinError,
    time::Instant,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    Supported,
    NotListed,
    Unknown,
}

/// The transport binds requests to the owner's current, confirmed pairing.
/// The worker does not persist an association in the source state.
pub trait Journal: Send + Sync + 'static {
    type Upload: Sync;
    type Reply;

    fn capability(&self) -> BoxFuture<'_, Capability>;
    fn pairing_unchanged(&self) -> BoxFuture<'_, bool>;
    fn deliver<'a>(&'a self, upload: &'a Self::Upload) -> BoxFuture<'a, Self::Reply>;
}

impl<J: Journal + ?Sized> Journal for Arc<J> {
    type Upload = J::Upload;
    type Reply = J::Reply;

    fn capability(&self) -> BoxFuture<'_, Capability> {
        (**self).capability()
    }

    fn pairing_unchanged(&self) -> BoxFuture<'_, bool> {
        (**self).pairing_unchanged()
    }

    fn deliver<'a>(&'a self, upload: &'a Self::Upload) -> BoxFuture<'a, Self::Reply> {
        (**self).deliver(upload)
    }
}

/// A worker's next check, independent of the source's cadence and durable state.
pub struct CheckSchedule(std::time::Instant);

impl Default for CheckSchedule {
    fn default() -> Self {
        Self(std::time::Instant::now())
    }
}

impl CheckSchedule {
    pub fn due(&self) -> bool {
        self.remaining().is_zero()
    }

    pub fn remaining(&self) -> Duration {
        self.0.saturating_duration_since(std::time::Instant::now())
    }

    pub fn defer(&mut self, delay: Duration) {
        self.0 = std::time::Instant::now() + delay;
    }
}

#[derive(Default)]
pub struct CapabilityCache {
    listed: Option<bool>,
    checked: Option<Instant>,
}

impl CapabilityCache {
    pub fn listed(&self) -> Option<bool> {
        self.listed
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub async fn check(&mut self, journal: &(impl Journal + ?Sized), retry: Duration) -> bool {
        let stale = match self.checked {
            None => true,
            Some(_) if !journal.pairing_unchanged().await => true,
            Some(checked) => {
                let ttl = if self.listed == Some(true) {
                    Duration::from_secs(5 * 60)
                } else {
                    retry
                };
                checked.elapsed() >= ttl
            }
        };
        if stale {
            self.listed = match journal.capability().await {
                Capability::Supported => Some(true),
                Capability::NotListed => Some(false),
                Capability::Unknown => None,
            };
            self.checked = Some(Instant::now());
        }
        self.listed == Some(true)
    }
}

/// Send within the pairing that passed the capability check. Sources that can
/// interrupt a send supply their cancellation signal. The caller owns recovery
/// of the staged material when a send doesn't produce a reply.
pub async fn deliver<J: Journal + ?Sized>(
    journal: &J,
    upload: &J::Upload,
    cancel: Option<&Notify>,
) -> Option<J::Reply> {
    tokio::select! {
        biased;
        () = async {
            match cancel {
                Some(cancel) => cancel.notified().await,
                None => pending().await,
            }
        } => None,
        reply = async {
            if !journal.pairing_unchanged().await {
                return None;
            }
            let reply = journal.deliver(upload).await;
            journal.pairing_unchanged().await.then_some(reply)
        } => reply,
    }
}

/// A re-pair requests first setup even when the worker is busy. An ordinary
/// wake (for example, a pause change) skips the timer without requesting setup.
#[derive(Default)]
pub struct Wake {
    restart: AtomicBool,
    notify: Notify,
}

impl Wake {
    pub fn restart(&self) {
        self.restart.store(true, Ordering::Release);
        self.nudge();
    }

    pub fn nudge(&self) {
        self.notify.notify_one();
    }

    pub fn take_restart(&self) -> bool {
        self.restart.swap(false, Ordering::AcqRel)
    }

    pub fn restart_pending(&self) -> bool {
        self.restart.load(Ordering::Acquire)
    }
}

pub enum Event<C> {
    Command(C),
    Wake,
    Closed,
}

pub async fn wait<C>(commands: &mut mpsc::Receiver<C>, wake: &Wake, delay: Duration) -> Event<C> {
    tokio::select! {
        () = wake.notify.notified() => Event::Wake,
        command = commands.recv() => match command {
            Some(command) => Event::Command(command),
            None => Event::Closed,
        },
        () = tokio::time::sleep(delay) => Event::Wake,
    }
}

/// Each source runs one blocking state operation at a time. A panicking
/// operation leaves the state unavailable, rather than reusing a partial one.
pub async fn with_state<S: Send + 'static, R: Send + 'static>(
    state: &mut Option<S>,
    call: impl FnOnce(&mut S) -> R + Send + 'static,
) -> Result<Option<R>, JoinError> {
    let Some(mut owned) = state.take() else {
        return Ok(None);
    };
    let (owned, result) = tokio::task::spawn_blocking(move || {
        let result = call(&mut owned);
        (owned, result)
    })
    .await?;
    *state = Some(owned);
    Ok(Some(result))
}
