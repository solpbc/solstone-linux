// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyValue {
    String(String),
    I32(i32),
    U32(u32),
}

impl std::fmt::Display for PropertyValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
    let property = |name: &str, value| PropertySpec {
        name: name.into(),
        value,
    };
    let element = |factory: &str, properties, caps| ElementSpec {
        factory: factory.into(),
        properties,
        caps,
    };
    PipelineDescription {
        elements: vec![
            element(
                "pipewiresrc",
                vec![
                    property("fd", PropertyValue::I32(pipewire_fd)),
                    // `path` is deprecated; target-object is a String on pipewiresrc.
                    property("target-object", PropertyValue::String(node_id.to_string())),
                ],
                None,
            ),
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
                    property("cq-level", PropertyValue::U32(4)),
                    property("max-quantizer", PropertyValue::U32(15)),
                    property("keyframe-max-dist", PropertyValue::U32(30)),
                    property("static-threshold", PropertyValue::U32(100)),
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
        ],
    }
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
    fn canonical_pipeline_matches_python_except_target_object() {
        let description =
            pipeline_description(7, 42, 1, Path::new("/tmp/unknown_monitor-0_screen.webm"));
        assert_eq!(
            render_gst_launch(&description),
            "pipewiresrc fd=7 target-object=42 ! videorate ! video/x-raw,framerate=1/1 ! videoconvert ! vp8enc end-usage=cq cq-level=4 max-quantizer=15 keyframe-max-dist=30 static-threshold=100 ! webmmux ! filesink location=/tmp/unknown_monitor-0_screen.webm"
        );
    }

    #[test]
    fn target_object_is_string_and_path_is_absent() {
        let description = pipeline_description(7, 42, 10, Path::new("out.webm"));
        let source = &description.elements[0];
        assert_eq!(
            source.properties[1].value,
            PropertyValue::String("42".into())
        );
        assert!(
            source
                .properties
                .iter()
                .all(|property| property.name != "path")
        );
        assert_eq!(
            description.elements[2].caps.as_deref(),
            Some("video/x-raw,framerate=10/1")
        );
    }
}
