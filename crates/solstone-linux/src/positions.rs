// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoxGeometry {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl BoxGeometry {
    pub fn width(&self) -> i32 {
        self.x2 - self.x1
    }

    pub fn height(&self) -> i32 {
        self.y2 - self.y1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub id: String,
    pub bounds: BoxGeometry,
    pub position: Option<String>,
}

pub fn assign_monitor_positions(monitors: &[Monitor]) -> Vec<Monitor> {
    if monitors.is_empty() {
        return Vec::new();
    }
    if monitors.len() == 1 {
        let mut result = monitors.to_vec();
        result[0].position = Some("center".into());
        return result;
    }

    const EPSILON: f64 = 1.0;
    monitors
        .iter()
        .map(|monitor| {
            let bounds = &monitor.bounds;
            let center_x = f64::from(bounds.x1 + bounds.x2) / 2.0;
            let center_y = f64::from(bounds.y1 + bounds.y2) / 2.0;
            let mut has_left = false;
            let mut has_right = false;
            let mut has_above = false;
            let mut has_below = false;

            for other in monitors {
                if std::ptr::eq(other, monitor) {
                    continue;
                }
                let other_x = f64::from(other.bounds.x1 + other.bounds.x2) / 2.0;
                let other_y = f64::from(other.bounds.y1 + other.bounds.y2) / 2.0;
                if other_x < center_x - EPSILON {
                    has_left = true;
                } else if other_x > center_x + EPSILON {
                    has_right = true;
                }

                let horizontal_overlap = bounds.x1 < other.bounds.x2 && bounds.x2 > other.bounds.x1;
                if horizontal_overlap {
                    if other_y < center_y - EPSILON {
                        has_above = true;
                    } else if other_y > center_y + EPSILON {
                        has_below = true;
                    }
                }
            }

            let horizontal = match (has_left, has_right) {
                (true, true) | (false, false) => "center",
                (true, false) => "right",
                (false, true) => "left",
            };
            let vertical = match (has_above, has_below) {
                (true, true) => Some("middle"),
                (true, false) => Some("bottom"),
                (false, true) => Some("top"),
                (false, false) => None,
            };
            let position = match (horizontal, vertical) {
                (horizontal, None) => horizontal.to_owned(),
                ("center", Some(vertical)) => vertical.to_owned(),
                (horizontal, Some(vertical)) => format!("{horizontal}-{vertical}"),
            };
            let mut labeled = monitor.clone();
            labeled.position = Some(position);
            labeled
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str, bounds: [i32; 4]) -> Monitor {
        Monitor {
            id: id.into(),
            bounds: BoxGeometry {
                x1: bounds[0],
                y1: bounds[1],
                x2: bounds[2],
                y2: bounds[3],
            },
            position: None,
        }
    }

    fn positions(monitors: &[Monitor]) -> Vec<String> {
        assign_monitor_positions(monitors)
            .into_iter()
            .map(|m| m.position.unwrap())
            .collect()
    }

    #[test]
    fn single_monitor_is_center() {
        assert_eq!(
            positions(&[monitor("DP-1", [0, 0, 1920, 1080])]),
            ["center"]
        );
    }
    #[test]
    fn two_horizontal_monitors() {
        assert_eq!(
            positions(&[
                monitor("DP-1", [0, 0, 1920, 1080]),
                monitor("DP-2", [1920, 0, 3840, 1080])
            ]),
            ["left", "right"]
        );
    }
    #[test]
    fn three_horizontal_monitors() {
        assert_eq!(
            positions(&[
                monitor("DP-1", [0, 0, 1920, 1080]),
                monitor("DP-2", [1920, 0, 3840, 1080]),
                monitor("DP-3", [3840, 0, 5760, 1080])
            ]),
            ["left", "center", "right"]
        );
    }
    #[test]
    fn stacked_vertical_monitors() {
        assert_eq!(
            positions(&[
                monitor("DP-1", [0, 0, 1920, 1080]),
                monitor("DP-2", [0, 1080, 1920, 2160])
            ]),
            ["top", "bottom"]
        );
    }
    #[test]
    fn empty_monitor_list() {
        assert!(assign_monitor_positions(&[]).is_empty());
    }
    #[test]
    fn offset_touching_monitors_have_no_vertical_label() {
        assert_eq!(
            positions(&[
                monitor("DP-1", [0, 0, 1920, 1080]),
                monitor("DP-2", [1920, 200, 3840, 1280])
            ]),
            ["left", "right"]
        );
    }
    #[test]
    fn exact_epsilon_is_centered() {
        assert_eq!(
            positions(&[monitor("a", [0, 0, 2, 2]), monitor("b", [1, 0, 3, 2])]),
            ["center", "center"]
        );
    }
    #[test]
    fn overlapping_offset_monitors_get_combined_labels() {
        assert_eq!(
            positions(&[
                monitor("a", [0, 0, 100, 100]),
                monitor("b", [50, 50, 150, 150])
            ]),
            ["left-top", "right-bottom"]
        );
    }
    #[test]
    fn caller_data_is_not_mutated() {
        let input = [monitor("a", [0, 0, 1, 1])];
        let before = input.clone();
        let _ = assign_monitor_positions(&input);
        assert_eq!(input, before);
    }
}
