// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::upload::Inner;
use serde_json::{Map, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};

pub const SILENT_QUEUE_MAX: usize = 64;

type Fields = Map<String, Value>;

#[derive(Default)]
struct State {
    latest_status: Option<Fields>,
    silent: VecDeque<Fields>,
    inflight: usize,
    stopping: bool,
}

pub(crate) struct EventSender {
    state: Arc<Mutex<State>>,
    notify: Arc<Notify>,
    handle: Option<JoinHandle<()>>,
    silent_capacity: usize,
}

impl EventSender {
    pub(crate) fn with_capacity(inner: Arc<Inner>, silent_capacity: usize) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let notify = Arc::new(Notify::new());
        let worker_state = Arc::clone(&state);
        let worker_notify = Arc::clone(&notify);
        let handle = tokio::spawn(async move {
            run(inner, worker_state, worker_notify).await;
        });
        Self {
            state,
            notify,
            handle: Some(handle),
            silent_capacity,
        }
    }

    pub fn submit_status(&self, fields: Fields) {
        let mut state = self.state.lock().unwrap();
        if state.latest_status.is_some() {
            tracing::debug!("Superseding undelivered observe.status event");
        }
        state.latest_status = Some(fields);
        drop(state);
        self.notify.notify_one();
    }

    pub fn submit_stream_silent(&self, fields: Fields) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.silent.len() >= self.silent_capacity {
            tracing::warn!(
                connector = fields
                    .get("connector")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                position = fields
                    .get("position")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                "Dropping stream_silent event because queue is full"
            );
            return false;
        }
        state.silent.push_back(fields);
        drop(state);
        self.notify.notify_one();
        true
    }

    pub async fn stop(&mut self, timeout: Duration) -> usize {
        {
            let mut state = self.state.lock().unwrap();
            state.stopping = true;
        }
        self.notify.notify_one();

        let Some(handle) = self.handle.as_mut() else {
            return 0;
        };
        if tokio::time::timeout(timeout, handle).await.is_ok() {
            self.handle = None;
            return 0;
        }

        let state = self.state.lock().unwrap();
        let undelivered =
            state.inflight + state.silent.len() + usize::from(state.latest_status.is_some());
        tracing::warn!(
            timeout = timeout.as_secs_f64(),
            undelivered,
            "Event sender did not stop within timeout; event(s) may be undelivered"
        );
        undelivered
    }
}

async fn run(inner: Arc<Inner>, state: Arc<Mutex<State>>, notify: Arc<Notify>) {
    loop {
        let notified = notify.notified();
        let work = {
            let mut state = state.lock().unwrap();
            if state.stopping && state.latest_status.is_none() && state.silent.is_empty() {
                return;
            }
            if state.latest_status.is_none() && state.silent.is_empty() {
                None
            } else {
                let status = state.latest_status.take();
                let silent = state.silent.drain(..).collect::<Vec<_>>();
                state.inflight = silent.len() + usize::from(status.is_some());
                Some((silent, status))
            }
        };

        let Some((silent, status)) = work else {
            notified.await;
            continue;
        };
        for fields in silent {
            if !inner.relay_event("observe", "stream_silent", fields).await {
                tracing::debug!("Event relay failed: observe.stream_silent");
            }
        }
        if let Some(fields) = status
            && !inner.relay_event("observe", "status", fields).await
        {
            tracing::debug!("Event relay failed: observe.status");
        }
        state.lock().unwrap().inflight = 0;
    }
}
