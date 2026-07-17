// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::positions::Monitor;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalStream {
    pub index: usize,
    pub node_id: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchedStream {
    pub index: usize,
    pub node_id: u32,
    pub connector: String,
    pub position_label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

struct WorkItem {
    stream: MatchedStream,
    matched: bool,
}

pub fn match_streams_to_monitors(
    streams: &[PortalStream],
    monitors: &[Monitor],
) -> Vec<MatchedStream> {
    let all_zero_position = streams
        .iter()
        .all(|stream| stream.position.unwrap_or((0, 0)) == (0, 0));
    let mut used = vec![false; monitors.len()];
    let mut work = Vec::with_capacity(streams.len());

    for stream in streams {
        let (sx, sy) = stream.position.unwrap_or((0, 0));
        let (sw, sh) = stream.size.unwrap_or((0, 0));
        let mut best: Option<(usize, i32)> = None;
        if !all_zero_position {
            for (index, monitor) in monitors.iter().enumerate() {
                if used[index] {
                    continue;
                }
                if (sx - monitor.bounds.x1).abs() < 10 && (sy - monitor.bounds.y1).abs() < 10 {
                    let overlap = sw.min(monitor.bounds.width()) * sh.min(monitor.bounds.height());
                    if overlap > best.map_or(0, |(_, overlap)| overlap) {
                        best = Some((index, overlap));
                    }
                }
            }
        }

        let item = if let Some((index, _)) = best {
            used[index] = true;
            WorkItem {
                stream: from_monitor(stream, &monitors[index]),
                matched: true,
            }
        } else {
            WorkItem {
                stream: MatchedStream {
                    index: stream.index,
                    node_id: stream.node_id,
                    connector: format!("monitor-{}", stream.index),
                    position_label: "unknown".into(),
                    x: sx,
                    y: sy,
                    width: sw,
                    height: sh,
                },
                matched: false,
            }
        };
        work.push(item);
    }

    for item in work.iter_mut().filter(|item| !item.matched) {
        let matched_index = monitors.iter().enumerate().position(|(index, monitor)| {
            !used[index]
                && (item.stream.width - monitor.bounds.width()).abs() <= 2
                && (item.stream.height - monitor.bounds.height()).abs() <= 2
        });
        if let Some(index) = matched_index {
            used[index] = true;
            let portal = PortalStream {
                index: item.stream.index,
                node_id: item.stream.node_id,
                position: Some((item.stream.x, item.stream.y)),
                size: Some((item.stream.width, item.stream.height)),
            };
            item.stream = from_monitor(&portal, &monitors[index]);
            item.matched = true;
        }
    }
    work.into_iter().map(|item| item.stream).collect()
}

fn from_monitor(stream: &PortalStream, monitor: &Monitor) -> MatchedStream {
    MatchedStream {
        index: stream.index,
        node_id: stream.node_id,
        connector: monitor.id.clone(),
        position_label: monitor.position.clone().unwrap_or_else(|| "unknown".into()),
        x: monitor.bounds.x1,
        y: monitor.bounds.y1,
        width: monitor.bounds.width(),
        height: monitor.bounds.height(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::BoxGeometry;

    fn stream(index: usize, pos: Option<(i32, i32)>, size: Option<(i32, i32)>) -> PortalStream {
        PortalStream {
            index,
            node_id: 10 + index as u32,
            position: pos,
            size,
        }
    }
    fn monitor(id: &str, b: [i32; 4], p: &str) -> Monitor {
        Monitor {
            id: id.into(),
            bounds: BoxGeometry {
                x1: b[0],
                y1: b[1],
                x2: b[2],
                y2: b[3],
            },
            position: Some(p.into()),
        }
    }
    fn standard() -> Vec<Monitor> {
        vec![
            monitor("DP-1", [0, 0, 1920, 1080], "left"),
            monitor("DP-2", [1920, 0, 4480, 1440], "right"),
        ]
    }
    fn assert_result(s: &MatchedStream, c: &str, p: &str, g: [i32; 4]) {
        assert_eq!(
            (
                s.connector.as_str(),
                s.position_label.as_str(),
                s.x,
                s.y,
                s.width,
                s.height
            ),
            (c, p, g[0], g[1], g[2], g[3])
        );
    }

    #[test]
    fn position_based_matching() {
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((1920, 1080))),
                stream(1, Some((1920, 0)), Some((2560, 1440))),
            ],
            &standard(),
        );
        assert_result(&r[0], "DP-1", "left", [0, 0, 1920, 1080]);
        assert_result(&r[1], "DP-2", "right", [1920, 0, 2560, 1440]);
    }
    #[test]
    fn size_based_fallback_when_no_position() {
        let m = [
            monitor("DP-1", [20, 0, 1940, 1080], "left"),
            monitor("DP-2", [1940, 0, 4500, 1440], "right"),
        ];
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((1920, 1080))),
                stream(1, Some((0, 0)), Some((2560, 1440))),
            ],
            &m,
        );
        assert_result(&r[0], "DP-1", "left", [20, 0, 1920, 1080]);
        assert_result(&r[1], "DP-2", "right", [1940, 0, 2560, 1440]);
    }
    #[test]
    fn position_match_skipped_when_all_zero() {
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((2560, 1440))),
                stream(1, Some((0, 0)), Some((1920, 1080))),
            ],
            &standard(),
        );
        assert_eq!(r[0].connector, "DP-2");
        assert_eq!(r[1].connector, "DP-1");
    }
    #[test]
    fn ambiguous_size_assigns_in_order() {
        let m = [
            monitor("DP-1", [20, 0, 1940, 1080], "left"),
            monitor("DP-2", [1940, 0, 3860, 1080], "right"),
        ];
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((1920, 1080))),
                stream(1, Some((0, 0)), Some((1920, 1080))),
            ],
            &m,
        );
        assert_eq!([&r[0].connector, &r[1].connector], ["DP-1", "DP-2"]);
    }
    #[test]
    fn no_monitors_falls_back_to_monitor_index() {
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((1920, 1080))),
                stream(1, Some((1920, 0)), Some((2560, 1440))),
            ],
            &[],
        );
        assert_result(&r[0], "monitor-0", "unknown", [0, 0, 1920, 1080]);
        assert_result(&r[1], "monitor-1", "unknown", [1920, 0, 2560, 1440]);
    }
    #[test]
    fn all_zero_streams_use_size_fallback() {
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((1920, 1080))),
                stream(1, Some((0, 0)), Some((2560, 1440))),
            ],
            &standard(),
        );
        assert_eq!([&r[0].connector, &r[1].connector], ["DP-1", "DP-2"]);
    }
    #[test]
    fn position_overlap_prefers_larger_overlap() {
        let m = [
            monitor("small", [0, 0, 100, 100], "left"),
            monitor("large", [1, 1, 201, 201], "right"),
        ];
        let r = match_streams_to_monitors(&[stream(0, Some((1, 1)), Some((200, 200)))], &m);
        assert_eq!(r[0].connector, "large");
    }
    #[test]
    fn equal_position_overlap_keeps_first_monitor() {
        let m = [
            monitor("first", [0, 0, 100, 100], "left"),
            monitor("second", [1, 1, 101, 101], "right"),
        ];
        let r = match_streams_to_monitors(&[stream(0, Some((2, 2)), Some((100, 100)))], &m);
        assert_eq!(r[0].connector, "first");
    }
    #[test]
    fn tolerances_have_expected_boundaries() {
        let m = [monitor("m", [10, 0, 110, 100], "center")];
        assert_eq!(
            match_streams_to_monitors(&[stream(0, Some((1, 0)), Some((50, 50)))], &m)[0].connector,
            "m"
        );
        assert_eq!(
            match_streams_to_monitors(&[stream(0, Some((0, 0)), Some((98, 102)))], &m)[0].connector,
            "m"
        );
        assert_eq!(
            match_streams_to_monitors(&[stream(0, Some((0, 0)), Some((97, 100)))], &m)[0].connector,
            "monitor-0"
        );
    }
    #[test]
    fn missing_properties_default_to_zero() {
        let r = match_streams_to_monitors(&[stream(0, None, None)], &[]);
        assert_result(&r[0], "monitor-0", "unknown", [0, 0, 0, 0]);
    }
    #[test]
    fn connector_with_monitor_prefix_remains_matched() {
        let m = [
            monitor("monitor-real", [0, 0, 100, 100], "center"),
            monitor("other", [200, 0, 300, 100], "right"),
        ];
        let r = match_streams_to_monitors(
            &[
                stream(0, Some((0, 0)), Some((100, 100))),
                stream(1, Some((200, 0)), Some((100, 100))),
            ],
            &m,
        );
        assert_eq!(
            [&r[0].connector, &r[1].connector],
            ["monitor-real", "other"]
        );
    }
}
