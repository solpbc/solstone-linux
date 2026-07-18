// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::path::Path;

use crate::positions::BoxGeometry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyValue {
    Bool(bool),
    String(String),
    I32(i32),
    U32(u32),
}

impl std::fmt::Display for PropertyValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(value) => value.fmt(formatter),
            Self::String(value) => formatter.write_str(value),
            Self::I32(value) => value.fmt(formatter),
            Self::U32(value) => value.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertySpec {
    pub name: String,
    pub value: PropertyValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementSpec {
    pub factory: String,
    pub properties: Vec<PropertySpec>,
    pub caps: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineDescription {
    pub elements: Vec<ElementSpec>,
}

pub fn pipeline_description(
    pipewire_fd: i32,
    node_id: u32,
    framerate: u8,
    output: &Path,
) -> PipelineDescription {
    let mut elements = vec![element(
        "pipewiresrc",
        vec![
            property("fd", PropertyValue::I32(pipewire_fd)),
            // Empirical (KDE Plasma Wayland, GStreamer 1.26 + PipeWire 1.x, live
            // portal run): `target-object={node_id}` fails to reach Playing on a
            // portal-restricted remote -- target-object matches the object *serial*,
            // while the portal hands out the *node id*. The deprecated `path`
            // property matches node ids and works; it is also what the shipping
            // Python observer uses. Keep `path` until pipewiresrc grows a
            // node-id-typed replacement.
            property("path", PropertyValue::String(node_id.to_string())),
        ],
        None,
    )];
    // pipewiresrc exposes no DMA-BUF-disabling property. Plain video/x-raw has
    // empty caps features (memory:SystemMemory), excluding the
    // video/x-raw(memory:DMABuf) branch and its EGL/GL dependency for 1fps
    // multi-monitor Wayland capture.
    elements.push(element("capsfilter", vec![], Some("video/x-raw".into())));
    elements.extend(encoder_tail(framerate, output));
    PipelineDescription { elements }
}

pub fn ximagesrc_pipeline_description(
    display: &str,
    bounds: &BoxGeometry,
    framerate: u8,
    draw_cursor: bool,
    output: &Path,
) -> PipelineDescription {
    let mut elements = vec![element(
        "ximagesrc",
        vec![
            property("display-name", PropertyValue::String(display.into())),
            property("startx", PropertyValue::I32(bounds.x1)),
            property("starty", PropertyValue::I32(bounds.y1)),
            property("endx", PropertyValue::I32(bounds.x2 - 1)),
            property("endy", PropertyValue::I32(bounds.y2 - 1)),
            // Damage-event capture can produce stuttering or partial frames;
            // full-frame capture is required for reliable observer segments.
            property("use-damage", PropertyValue::Bool(false)),
            property("show-pointer", PropertyValue::Bool(draw_cursor)),
        ],
        None,
    )];
    elements.extend(encoder_tail(framerate, output));
    PipelineDescription { elements }
}

fn property(name: &str, value: PropertyValue) -> PropertySpec {
    PropertySpec {
        name: name.into(),
        value,
    }
}

fn element(factory: &str, properties: Vec<PropertySpec>, caps: Option<String>) -> ElementSpec {
    ElementSpec {
        factory: factory.into(),
        properties,
        caps,
    }
}

fn encoder_tail(framerate: u8, output: &Path) -> Vec<ElementSpec> {
    vec![
        element("videorate", vec![], None),
        element(
            "capsfilter",
            vec![],
            Some(format!("video/x-raw,framerate={framerate}/1")),
        ),
        element("videoconvert", vec![], None),
        element(
            "vp8enc",
            vec![
                property("end-usage", PropertyValue::String("cq".into())),
                property("cq-level", PropertyValue::I32(4)),
                property("max-quantizer", PropertyValue::I32(15)),
                property("keyframe-max-dist", PropertyValue::I32(30)),
                property("static-threshold", PropertyValue::I32(100)),
            ],
            None,
        ),
        element("webmmux", vec![], None),
        element(
            "filesink",
            vec![property(
                "location",
                PropertyValue::String(output.to_string_lossy().into_owned()),
            )],
            None,
        ),
    ]
}

pub fn render_gst_launch(description: &PipelineDescription) -> String {
    description
        .elements
        .iter()
        .map(|element| {
            if let Some(caps) = &element.caps {
                return caps.clone();
            }
            let mut rendered = element.factory.clone();
            for property in &element.properties {
                rendered.push(' ');
                rendered.push_str(&property.name);
                rendered.push('=');
                rendered.push_str(&property.value.to_string());
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(" ! ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_portal_pipeline_forces_shm_and_matches_encoder_tail() {
        let description =
            pipeline_description(7, 42, 1, Path::new("/tmp/unknown_monitor-0_screen.webm"));
        assert_eq!(
            render_gst_launch(&description),
            "pipewiresrc fd=7 path=42 ! video/x-raw ! videorate ! video/x-raw,framerate=1/1 ! videoconvert ! vp8enc end-usage=cq cq-level=4 max-quantizer=15 keyframe-max-dist=30 static-threshold=100 ! webmmux ! filesink location=/tmp/unknown_monitor-0_screen.webm"
        );
    }

    #[test]
    fn path_carries_node_id_and_target_object_is_absent() {
        // Empirical contract from a live KDE Plasma portal run: the portal hands
        // out node ids; pipewiresrc `target-object` matches object serials and
        // fails to reach Playing, while the deprecated `path` matches node ids.
        let description = pipeline_description(7, 42, 10, Path::new("out.webm"));
        let source = &description.elements[0];
        assert_eq!(source.properties[1].name, "path");
        assert_eq!(
            source.properties[1].value,
            PropertyValue::String("42".into())
        );
        assert!(
            source
                .properties
                .iter()
                .all(|property| property.name != "target-object")
        );
        let source_caps = description.elements[1].caps.as_deref().unwrap();
        assert_eq!(source_caps, "video/x-raw");
        assert!(!source_caps.contains("memory:"));
        assert!(!source_caps.contains("DMABuf"));
        assert_eq!(
            description.elements[3].caps.as_deref(),
            Some("video/x-raw,framerate=10/1")
        );
    }

    #[test]
    fn ximagesrc_bounds_are_inclusive_and_cursor_is_configurable() {
        // tests/test_screencast.py::TestX11Screencaster::test_pipeline_coordinates
        let first = ximagesrc_pipeline_description(
            ":0",
            &BoxGeometry {
                x1: 0,
                y1: 0,
                x2: 1920,
                y2: 1080,
            },
            1,
            true,
            Path::new("left.webm"),
        );
        assert_eq!(
            render_gst_launch(&first),
            "ximagesrc display-name=:0 startx=0 starty=0 endx=1919 endy=1079 use-damage=false show-pointer=true ! videorate ! video/x-raw,framerate=1/1 ! videoconvert ! vp8enc end-usage=cq cq-level=4 max-quantizer=15 keyframe-max-dist=30 static-threshold=100 ! webmmux ! filesink location=left.webm"
        );
        let second = ximagesrc_pipeline_description(
            ":1",
            &BoxGeometry {
                x1: 1920,
                y1: 0,
                x2: 3840,
                y2: 1080,
            },
            10,
            false,
            Path::new("right.webm"),
        );
        assert_eq!(
            render_gst_launch(&second),
            "ximagesrc display-name=:1 startx=1920 starty=0 endx=3839 endy=1079 use-damage=false show-pointer=false ! videorate ! video/x-raw,framerate=10/1 ! videoconvert ! vp8enc end-usage=cq cq-level=4 max-quantizer=15 keyframe-max-dist=30 static-threshold=100 ! webmmux ! filesink location=right.webm"
        );
    }
}
