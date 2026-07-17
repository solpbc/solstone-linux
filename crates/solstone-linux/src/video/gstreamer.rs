// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    error::Error,
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use gst::prelude::*;
use gstreamer as gst;

use crate::pipeline::{PipelineDescription, PropertyValue};

type AnyError = Box<dyn Error + Send + Sync>;

pub trait CapturePipeline: Send {
    fn start(&mut self) -> Result<(), String>;
    fn is_healthy(&self) -> bool;
    fn send_eos(&mut self) -> bool;
    fn poll_terminal(&mut self) -> Option<Result<(), String>>;
    fn force_stop(&mut self);
}

pub trait PipelineFactory: Send {
    fn build(
        &mut self,
        description: &PipelineDescription,
    ) -> Result<Box<dyn CapturePipeline>, String>;
}

pub fn stop_pipelines(pipelines: &mut [&mut Box<dyn CapturePipeline>], timeout: Duration) {
    let mut awaiting = Vec::new();
    for (index, pipeline) in pipelines.iter_mut().enumerate() {
        if pipeline.as_mut().send_eos() {
            awaiting.push(index);
        } else {
            pipeline.as_mut().force_stop();
        }
    }
    let deadline = Instant::now() + timeout;
    while !awaiting.is_empty() && Instant::now() < deadline {
        awaiting.retain(|index| {
            let pipeline = &mut pipelines[*index];
            match pipeline.as_mut().poll_terminal() {
                None => true,
                Some(_) => {
                    pipeline.as_mut().force_stop();
                    false
                }
            }
        });
        if !awaiting.is_empty() {
            thread::sleep(Duration::from_millis(10));
        }
    }
    for index in awaiting {
        pipelines[index].as_mut().force_stop();
    }
}

#[derive(Default)]
pub struct GstreamerPipelineFactory;

impl PipelineFactory for GstreamerPipelineFactory {
    fn build(
        &mut self,
        description: &PipelineDescription,
    ) -> Result<Box<dyn CapturePipeline>, String> {
        build_pipeline(description)
            .map(|pipeline| Box::new(GstreamerPipeline { pipeline }) as Box<dyn CapturePipeline>)
            .map_err(|error| error.to_string())
    }
}

struct GstreamerPipeline {
    pipeline: gst::Pipeline,
}

impl CapturePipeline for GstreamerPipeline {
    fn start(&mut self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn is_healthy(&self) -> bool {
        let Some(bus) = self.pipeline.bus() else {
            return false;
        };
        bus.pop_filtered(&[gst::MessageType::Error]).is_none()
    }

    fn send_eos(&mut self) -> bool {
        self.pipeline.send_event(gst::event::Eos::new())
    }

    fn poll_terminal(&mut self) -> Option<Result<(), String>> {
        let message = self
            .pipeline
            .bus()?
            .pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])?;
        match message.view() {
            gst::MessageView::Eos(_) => Some(Ok(())),
            gst::MessageView::Error(error) => {
                Some(Err(format!("{} debug={:?}", error.error(), error.debug())))
            }
            _ => None,
        }
    }

    fn force_stop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

pub fn build_pipeline(description: &PipelineDescription) -> Result<gst::Pipeline, AnyError> {
    let pipeline = gst::Pipeline::new();
    let mut elements = Vec::with_capacity(description.elements.len());
    for spec in &description.elements {
        let element = gst::ElementFactory::make(&spec.factory)
            .build()
            .map_err(|error| {
                format!(
                    "missing or unloadable GStreamer element '{}': {error}",
                    spec.factory
                )
            })?;
        for property in &spec.properties {
            let value =
                checked_property_value(&element, &spec.factory, &property.name, &property.value)?;
            element.set_property_from_value(&property.name, &value);
        }
        if let Some(caps) = &spec.caps {
            let caps = gst::Caps::from_str(caps)?;
            set_checked_value(&element, &spec.factory, "caps", caps.to_value())?;
        }
        pipeline.add(&element)?;
        elements.push(element);
    }
    for pair in elements.windows(2) {
        pair[0].link(&pair[1])?;
    }
    Ok(pipeline)
}

fn checked_property_value(
    element: &gst::Element,
    element_name: &str,
    property_name: &str,
    property_value: &PropertyValue,
) -> Result<gst::glib::Value, AnyError> {
    let property = element.find_property(property_name).ok_or_else(|| {
        format!("GStreamer element '{element_name}' has no property '{property_name}'")
    })?;
    if !property.flags().contains(gst::glib::ParamFlags::WRITABLE)
        || property
            .flags()
            .contains(gst::glib::ParamFlags::CONSTRUCT_ONLY)
    {
        return Err(format!(
            "GStreamer element '{element_name}' property '{property_name}' is not writable"
        )
        .into());
    }

    let expected_type = property.value_type();
    let value = match property_value {
        PropertyValue::Bool(value) if expected_type == bool::static_type() => value.to_value(),
        PropertyValue::Bool(_) => {
            return Err(incompatible_property_type(element_name, property_name, expected_type).into());
        }
        PropertyValue::String(value) if expected_type == gst::glib::Type::STRING => value.to_value(),
        PropertyValue::String(value) if expected_type.is_a(gst::glib::Type::ENUM) => {
            gst::glib::EnumClass::with_type(expected_type)
                .and_then(|class| class.to_value_by_nick(value))
                .ok_or_else(|| {
                    format!(
                        "GStreamer element '{element_name}' property '{property_name}' does not accept value '{value}'"
                    )
                })?
        }
        PropertyValue::String(_) => {
            return Err(incompatible_property_type(element_name, property_name, expected_type).into());
        }
        PropertyValue::I32(value) => {
            let range = property
                .downcast_ref::<gst::glib::ParamSpecInt>()
                .ok_or_else(|| incompatible_property_type(element_name, property_name, expected_type))?;
            if *value < range.minimum() || *value > range.maximum() {
                return Err(format!(
                    "GStreamer element '{element_name}' property '{property_name}' rejects value '{value}' outside {}..={}",
                    range.minimum(), range.maximum()
                )
                .into());
            }
            value.to_value()
        }
        PropertyValue::U32(value) => {
            let range = property
                .downcast_ref::<gst::glib::ParamSpecUInt>()
                .ok_or_else(|| incompatible_property_type(element_name, property_name, expected_type))?;
            if *value < range.minimum() || *value > range.maximum() {
                return Err(format!(
                    "GStreamer element '{element_name}' property '{property_name}' rejects value '{value}' outside {}..={}",
                    range.minimum(), range.maximum()
                )
                .into());
            }
            value.to_value()
        }
    };
    ensure_property_type(element_name, property_name, expected_type, &value)?;
    Ok(value)
}

fn set_checked_value(
    element: &gst::Element,
    element_name: &str,
    property_name: &str,
    value: gst::glib::Value,
) -> Result<(), AnyError> {
    let property = element.find_property(property_name).ok_or_else(|| {
        format!("GStreamer element '{element_name}' has no property '{property_name}'")
    })?;
    if !property.flags().contains(gst::glib::ParamFlags::WRITABLE)
        || property
            .flags()
            .contains(gst::glib::ParamFlags::CONSTRUCT_ONLY)
    {
        return Err(format!(
            "GStreamer element '{element_name}' property '{property_name}' is not writable"
        )
        .into());
    }
    ensure_property_type(element_name, property_name, property.value_type(), &value)?;
    element.set_property_from_value(property_name, &value);
    Ok(())
}

fn incompatible_property_type(
    element_name: &str,
    property_name: &str,
    expected_type: gst::glib::Type,
) -> String {
    format!(
        "GStreamer element '{element_name}' property '{property_name}' has incompatible type '{expected_type}'"
    )
}

fn ensure_property_type(
    element_name: &str,
    property_name: &str,
    expected_type: gst::glib::Type,
    value: &gst::glib::Value,
) -> Result<(), AnyError> {
    if value.type_() != expected_type {
        return Err(format!(
            "GStreamer element '{element_name}' property '{property_name}' expects type '{expected_type}', got '{}'",
            value.type_()
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePipeline;

    impl CapturePipeline for FakePipeline {
        fn start(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn is_healthy(&self) -> bool {
            true
        }
        fn send_eos(&mut self) -> bool {
            true
        }
        fn poll_terminal(&mut self) -> Option<Result<(), String>> {
            Some(Ok(()))
        }
        fn force_stop(&mut self) {}
    }

    #[test]
    fn pipeline_seam_needs_no_gstreamer_runtime() {
        let mut pipeline: Box<dyn CapturePipeline> = Box::new(FakePipeline);
        assert!(pipeline.start().is_ok());
        assert!(pipeline.is_healthy());
        assert!(pipeline.send_eos());
        assert_eq!(pipeline.poll_terminal(), Some(Ok(())));
    }
}
