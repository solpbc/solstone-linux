// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::time::{Duration, Instant};

use rustix::{
    event::{PollFd, PollFlags, poll},
    time::Timespec,
};

use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{wl_callback, wl_output, wl_registry},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

use crate::positions::{BoxGeometry, Monitor, assign_monitor_positions};

pub const WAYLAND_ENUMERATION_TIMEOUT: Duration = Duration::from_secs(5);
pub const SUPPORTED_WL_OUTPUT_VERSION: u32 = 4;
pub const SUPPORTED_XDG_OUTPUT_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundVersions {
    pub wl_output: u32,
    pub xdg_output: Option<u32>,
}

pub fn bound_versions(wl_output: u32, xdg_output: Option<u32>) -> BoundVersions {
    BoundVersions {
        wl_output: wl_output.min(SUPPORTED_WL_OUTPUT_VERSION),
        xdg_output: xdg_output.map(|version| version.min(SUPPORTED_XDG_OUTPUT_VERSION)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumeratedOutput {
    pub index: usize,
    pub versions: BoundVersions,
    pub wl_name: Option<String>,
    pub xdg_name: Option<String>,
    pub logical_position: Option<(i32, i32)>,
    pub logical_size: Option<(i32, i32)>,
}

pub fn outputs_to_monitors(outputs: &[EnumeratedOutput]) -> Result<Vec<Monitor>, String> {
    let mut monitors = Vec::with_capacity(outputs.len());
    for output in outputs {
        let (x, y) = output
            .logical_position
            .ok_or_else(|| format!("Wayland output {} has no logical position", output.index))?;
        let (width, height) = output
            .logical_size
            .filter(|(width, height)| *width > 0 && *height > 0)
            .ok_or_else(|| format!("Wayland output {} has no usable logical size", output.index))?;
        let id = output
            .wl_name
            .as_deref()
            .filter(|name| output.versions.wl_output >= 4 && !name.trim().is_empty())
            .or_else(|| {
                output.xdg_name.as_deref().filter(|name| {
                    output
                        .versions
                        .xdg_output
                        .is_some_and(|version| version >= 2)
                        && !name.trim().is_empty()
                })
            })
            .map(str::to_owned)
            .unwrap_or_else(|| format!("monitor-{}", output.index));
        monitors.push(Monitor {
            id,
            bounds: BoxGeometry {
                x1: x,
                y1: y,
                x2: x + width,
                y2: y + height,
            },
            position: None,
        });
    }
    Ok(assign_monitor_positions(&monitors))
}

#[derive(Default)]
pub struct NativeWaylandGeometry;

impl NativeWaylandGeometry {
    pub fn monitors(&mut self) -> Result<Vec<Monitor>, String> {
        let outputs = enumerate_native_outputs(WAYLAND_ENUMERATION_TIMEOUT)?;
        let outputs: Vec<_> = outputs.into_iter().map(|output| output.output).collect();
        outputs_to_monitors(&outputs)
    }
}

struct NativeOutput {
    proxy: wl_output::WlOutput,
    output: EnumeratedOutput,
    complete: bool,
}

#[derive(Default)]
struct NativeState {
    outputs: Vec<NativeOutput>,
    manager: Option<(zxdg_output_manager_v1::ZxdgOutputManagerV1, u32)>,
    registry_done: bool,
}

fn enumerate_native_outputs(timeout: Duration) -> Result<Vec<NativeOutput>, String> {
    let connection = Connection::connect_to_env().map_err(|error| error.to_string())?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    connection.display().sync(&handle, ());
    let mut state = NativeState::default();
    let deadline = Instant::now() + timeout;
    loop {
        queue
            .dispatch_pending(&mut state)
            .map_err(|error| error.to_string())?;
        if state.registry_done && state.manager.is_none() {
            return Err("Wayland compositor lacks xdg-output logical geometry support".into());
        }
        if state.registry_done
            && !state.outputs.is_empty()
            && state.outputs.iter().all(|output| output.complete)
        {
            return Ok(state.outputs);
        }
        if state.registry_done && state.outputs.is_empty() {
            return Err("Wayland compositor advertised no outputs".into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "Wayland output enumeration timed out".to_owned())?;
        queue.flush().map_err(|error| error.to_string())?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let connection_fd = guard.connection_fd();
        let mut fds = [PollFd::new(&connection_fd, PollFlags::IN)];
        let poll_timeout = Timespec::try_from(remaining)
            .map_err(|_| "Wayland output enumeration timeout is out of range".to_owned())?;
        if poll(&mut fds, Some(&poll_timeout)).map_err(|error| error.to_string())? == 0 {
            return Err("Wayland output enumeration timed out".into());
        }
        guard.read().map_err(|error| error.to_string())?;
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for NativeState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.registry_done = true;
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for NativeState {
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
            "wl_output" => {
                let index = state.outputs.len();
                let versions = bound_versions(version, state.manager.as_ref().map(|(_, v)| *v));
                let proxy = registry.bind::<wl_output::WlOutput, _, _>(
                    name,
                    versions.wl_output,
                    handle,
                    index,
                );
                state.outputs.push(NativeOutput {
                    proxy: proxy.clone(),
                    output: EnumeratedOutput {
                        index,
                        versions,
                        wl_name: None,
                        xdg_name: None,
                        logical_position: None,
                        logical_size: None,
                    },
                    complete: false,
                });
                if let Some((manager, manager_version)) = &state.manager {
                    let Some(xdg_version) =
                        bound_versions(version, Some(*manager_version)).xdg_output
                    else {
                        return;
                    };
                    state.outputs[index].output.versions.xdg_output = Some(xdg_version);
                    manager.get_xdg_output(&proxy, handle, index);
                }
            }
            "zxdg_output_manager_v1" => {
                let Some(manager_version) = bound_versions(1, Some(version)).xdg_output else {
                    return;
                };
                let manager = registry.bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, _, _>(
                    name,
                    manager_version,
                    handle,
                    (),
                );
                for (index, output) in state.outputs.iter_mut().enumerate() {
                    output.output.versions.xdg_output = Some(manager_version);
                    manager.get_xdg_output(&output.proxy, handle, index);
                }
                state.manager = Some((manager, manager_version));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for NativeState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => output.output.wl_name = Some(name),
            wl_output::Event::Done => {
                output.complete =
                    output.output.logical_position.is_some() && output.output.logical_size.is_some()
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for NativeState {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                output.output.logical_position = Some((x, y));
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                output.output.logical_size = Some((width, height));
            }
            zxdg_output_v1::Event::Name { name } => output.output.xdg_name = Some(name),
            zxdg_output_v1::Event::Done => output.complete = true,
            _ => {}
        }
    }
}

delegate_noop!(NativeState: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);

#[cfg(test)]
mod tests {
    use super::*;

    fn output(
        index: usize,
        versions: BoundVersions,
        wl_name: Option<&str>,
        xdg_name: Option<&str>,
        position: (i32, i32),
        size: (i32, i32),
    ) -> EnumeratedOutput {
        EnumeratedOutput {
            index,
            versions,
            wl_name: wl_name.map(str::to_owned),
            xdg_name: xdg_name.map(str::to_owned),
            logical_position: Some(position),
            logical_size: Some(size),
        }
    }

    #[test]
    fn never_binds_above_supported_or_advertised_version() {
        assert_eq!(
            bound_versions(1, Some(1)),
            BoundVersions {
                wl_output: 1,
                xdg_output: Some(1)
            }
        );
        assert_eq!(
            bound_versions(99, Some(99)),
            BoundVersions {
                wl_output: 4,
                xdg_output: Some(3)
            }
        );
        assert_eq!(
            bound_versions(3, None),
            BoundVersions {
                wl_output: 3,
                xdg_output: None
            }
        );
    }

    #[test]
    fn name_fallback_obeys_protocol_versions() {
        let monitors = outputs_to_monitors(&[
            output(
                0,
                bound_versions(4, Some(3)),
                Some("DP-1"),
                Some("old-DP-1"),
                (0, 0),
                (1920, 1080),
            ),
            output(
                1,
                bound_versions(3, Some(2)),
                Some("ignored"),
                Some("DP-2"),
                (1920, 0),
                (2560, 1440),
            ),
            output(
                2,
                bound_versions(1, Some(1)),
                None,
                Some("too-old"),
                (4480, 0),
                (800, 600),
            ),
        ])
        .unwrap();
        assert_eq!(
            monitors
                .iter()
                .map(|monitor| monitor.id.as_str())
                .collect::<Vec<_>>(),
            ["DP-1", "DP-2", "monitor-2"]
        );
    }

    #[test]
    fn observed_layout_flows_to_position_labels() {
        // crates/solstone-linux/src/matching.rs::standard
        let monitors = outputs_to_monitors(&[
            output(
                0,
                bound_versions(4, Some(3)),
                Some("DP-1"),
                None,
                (0, 0),
                (1920, 1080),
            ),
            output(
                1,
                bound_versions(4, Some(3)),
                Some("DP-2"),
                None,
                (1920, 0),
                (2560, 1440),
            ),
        ])
        .unwrap();
        assert_eq!(
            monitors[0].bounds,
            BoxGeometry {
                x1: 0,
                y1: 0,
                x2: 1920,
                y2: 1080
            }
        );
        assert_eq!(
            monitors[1].bounds,
            BoxGeometry {
                x1: 1920,
                y1: 0,
                x2: 4480,
                y2: 1440
            }
        );
        assert_eq!(monitors[0].position.as_deref(), Some("left"));
        assert_eq!(monitors[1].position.as_deref(), Some("right"));
    }

    #[test]
    fn incomplete_enumeration_is_an_error_and_timeout_is_bounded() {
        let incomplete = output(
            0,
            bound_versions(4, Some(3)),
            Some("DP-1"),
            None,
            (0, 0),
            (0, 0),
        );
        assert!(
            outputs_to_monitors(&[incomplete])
                .unwrap_err()
                .contains("usable logical size")
        );
        assert!(WAYLAND_ENUMERATION_TIMEOUT > Duration::ZERO);
        assert!(WAYLAND_ENUMERATION_TIMEOUT <= Duration::from_secs(30));
    }
}
