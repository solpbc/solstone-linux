// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use rustix::event::{PollFd, PollFlags, poll};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

use super::{BackendOutcome, IDLE_THRESHOLD, IdleEdge, WaylandIdleOps};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdleEdges {
    idle: bool,
}

impl IdleEdges {
    pub fn idled(&mut self) {
        self.idle = true;
    }
    pub fn resumed(&mut self) {
        self.idle = false;
    }
    pub fn current(self) -> bool {
        self.idle
    }
}

#[derive(Default)]
struct WaylandState {
    edges: IdleEdges,
    notifier: Option<ext_idle_notifier_v1::ExtIdleNotifierV1>,
    notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    seat: Option<wl_seat::WlSeat>,
}

struct Connected {
    connection: Connection,
    queue: EventQueue<WaylandState>,
    state: WaylandState,
}

pub struct NativeWaylandIdle {
    connected: Option<Connected>,
}

impl Default for NativeWaylandIdle {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeWaylandIdle {
    pub fn new() -> Self {
        Self {
            connected: connect().ok(),
        }
    }

    pub fn poll(&mut self) -> BackendOutcome<bool> {
        let Some(connected) = &mut self.connected else {
            return BackendOutcome::Absent;
        };
        if let Err(error) = dispatch_available(connected) {
            self.connected = None;
            return BackendOutcome::Broken(error);
        }
        BackendOutcome::Available(connected.state.edges.current())
    }
}

impl WaylandIdleOps for NativeWaylandIdle {
    fn bind(&mut self) -> BackendOutcome<()> {
        if self.connected.is_some() {
            BackendOutcome::Available(())
        } else {
            BackendOutcome::Absent
        }
    }

    fn drain_edges(&mut self) -> Vec<IdleEdge> {
        let before = self
            .connected
            .as_ref()
            .map(|value| value.state.edges.current());
        let after = match self.poll() {
            BackendOutcome::Available(value) => Some(value),
            _ => None,
        };
        match (before, after) {
            (Some(false), Some(true)) => vec![IdleEdge::Idled],
            (Some(true), Some(false)) => vec![IdleEdge::Resumed],
            _ => vec![],
        }
    }
}

fn connect() -> Result<Connected, String> {
    let connection = Connection::connect_to_env().map_err(|error| error.to_string())?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut state = WaylandState::default();
    queue
        .roundtrip(&mut state)
        .map_err(|error| error.to_string())?;
    let notifier = state
        .notifier
        .as_ref()
        .ok_or_else(|| "ext_idle_notifier_v1 is not advertised".to_owned())?;
    let notification = notifier.get_idle_notification(
        IDLE_THRESHOLD.as_millis() as u32,
        state
            .seat
            .as_ref()
            .ok_or_else(|| "Wayland compositor advertised no seat".to_owned())?,
        &handle,
        (),
    );
    state.notification = Some(notification);
    connection.flush().map_err(|error| error.to_string())?;
    Ok(Connected {
        connection,
        queue,
        state,
    })
}

fn dispatch_available(connected: &mut Connected) -> Result<(), String> {
    connected
        .queue
        .dispatch_pending(&mut connected.state)
        .map_err(|error| error.to_string())?;
    connected
        .connection
        .flush()
        .map_err(|error| error.to_string())?;
    let Some(guard) = connected.queue.prepare_read() else {
        return Ok(());
    };
    let fd = guard.connection_fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let zero = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if poll(&mut fds, Some(&zero)).map_err(|error| error.to_string())? > 0 {
        guard.read().map_err(|error| error.to_string())?;
        connected
            .queue
            .dispatch_pending(&mut connected.state)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "ext_idle_notifier_v1" => {
                state.notifier = Some(registry.bind(name, version.min(2), handle, ()));
            }
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(1), handle, ())),
            _ => {}
        }
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.edges.idled(),
            ext_idle_notification_v1::Event::Resumed => state.edges.resumed(),
            _ => {}
        }
    }
}

delegate_noop!(WaylandState: ignore ext_idle_notifier_v1::ExtIdleNotifierV1);
delegate_noop!(WaylandState: ignore wl_seat::WlSeat);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_bridge_to_poll_answer() {
        // No 1:1 Python ancestor: ext-idle-notify edges maintain a poll-shaped answer.
        let mut state = IdleEdges::default();
        assert!(!state.current());
        state.idled();
        assert!(state.current());
        state.resumed();
        assert!(!state.current());
    }
}
