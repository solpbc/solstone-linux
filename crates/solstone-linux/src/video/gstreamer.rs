// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    error::Error,
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use gst::prelude::*;
use gstreamer as gst;

use crate::pipeline::{PipelineDescription, PropertyValue};
use crate::rotation::{self, RotationAction, RotationEvent, RotationState};

type AnyError = Box<dyn Error + Send + Sync>;

pub trait CapturePipeline: Send {
    fn start(&mut self) -> Result<(), String>;
    fn is_healthy(&self) -> bool;
    fn send_eos(&mut self) -> bool;
    fn poll_terminal(&mut self) -> Option<Result<(), String>>;
    fn state_label(&self) -> String;
    fn force_stop(&mut self);
}

pub trait PipelineFactory: Send {
    fn build(
        &mut self,
        description: &PipelineDescription,
    ) -> Result<Box<dyn CapturePipeline>, String>;
}

pub struct StoppingPipeline<'a> {
    pub identity: String,
    pub pipeline: &'a mut Box<dyn CapturePipeline>,
}

pub fn stop_pipelines(pipelines: &mut [StoppingPipeline<'_>], timeout: Duration) {
    // This drives only rotation.rs's Stop half. Membership in `awaiting` is the
    // persisted per-stream state; AwaitingEos(Stop) is reconstructed for each
    // terminal event. Rotate is unreachable under D2 because rotation is a
    // stop followed by a new start/session.
    let mut awaiting = Vec::new();
    for (index, record) in pipelines.iter_mut().enumerate() {
        let transition = rotation::transition(RotationState::Running, RotationEvent::StopRequested);
        if transition.actions.contains(&RotationAction::SendEos)
            && record.pipeline.as_mut().send_eos()
        {
            awaiting.push(index);
        } else {
            let transition = rotation::transition(
                transition.state,
                RotationEvent::Error("pipeline rejected EOS".into()),
            );
            execute_stop_actions(record, transition.actions);
        }
    }
    let deadline = Instant::now() + timeout;
    while !awaiting.is_empty() && Instant::now() < deadline {
        awaiting.retain(|index| {
            let record = &mut pipelines[*index];
            match record.pipeline.as_mut().poll_terminal() {
                None => true,
                Some(Ok(())) => {
                    let transition = rotation::transition(
                        RotationState::AwaitingEos(rotation::RotationIntent::Stop),
                        RotationEvent::EosReceived,
                    );
                    execute_stop_actions(record, transition.actions);
                    false
                }
                Some(Err(error)) => {
                    let transition = rotation::transition(
                        RotationState::AwaitingEos(rotation::RotationIntent::Stop),
                        RotationEvent::Error(error),
                    );
                    execute_stop_actions(record, transition.actions);
                    false
                }
            }
        });
        if !awaiting.is_empty() {
            thread::sleep(Duration::from_millis(10));
        }
    }
    for index in awaiting {
        let record = &mut pipelines[index];
        let transition = rotation::transition(
            RotationState::AwaitingEos(rotation::RotationIntent::Stop),
            RotationEvent::TimeoutElapsed {
                stream_identity: record.identity.clone(),
                last_pipeline_state: record.pipeline.state_label(),
            },
        );
        execute_stop_actions(record, transition.actions);
    }
}

fn execute_stop_actions(record: &mut StoppingPipeline<'_>, actions: Vec<RotationAction>) {
    for action in actions {
        match action {
            RotationAction::ForcePipelineNull => record.pipeline.force_stop(),
            RotationAction::ReportNotCleanlyFinalized {
                stream_identity,
                last_pipeline_state,
            } => {
                tracing::error!(stream = %stream_identity, %last_pipeline_state, "EOS timeout: stream not cleanly finalized");
            }
            RotationAction::ReportError(error) => {
                tracing::error!(stream = %record.identity, %error, "pipeline bus error while stopping");
            }
            RotationAction::StopCleanly => {}
            RotationAction::SendEos
            | RotationAction::FinalizeCleanlyAndRestart
            | RotationAction::RestartAfterUncleanFinalization => {}
        }
    }
}

pub struct GstreamerPipelineFactory {
    _private: (),
}

impl GstreamerPipelineFactory {
    pub fn new() -> Result<Self, String> {
        ensure_initialized()?;
        Ok(Self { _private: () })
    }
}

pub fn ensure_initialized() -> Result<(), String> {
    gst::init().map_err(|error| format!("failed to initialize GStreamer: {error}"))
}

impl PipelineFactory for GstreamerPipelineFactory {
    fn build(
        &mut self,
        description: &PipelineDescription,
    ) -> Result<Box<dyn CapturePipeline>, String> {
        build_pipeline(description)
            .map(|pipeline| {
                Box::new(GstreamerPipeline {
                    pipeline,
                    failed: AtomicBool::new(false),
                }) as Box<dyn CapturePipeline>
            })
            .map_err(|error| error.to_string())
    }
}

struct GstreamerPipeline {
    pipeline: gst::Pipeline,
    failed: AtomicBool,
}

impl CapturePipeline for GstreamerPipeline {
    fn start(&mut self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn is_healthy(&self) -> bool {
        if self.failed.load(Ordering::Acquire) {
            return false;
        }
        let Some(bus) = self.pipeline.bus() else {
            self.failed.store(true, Ordering::Release);
            return false;
        };
        if bus.pop_filtered(&[gst::MessageType::Error]).is_some() {
            self.failed.store(true, Ordering::Release);
            return false;
        }
        true
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

    fn state_label(&self) -> String {
        format!("{:?}", self.pipeline.current_state())
    }

    fn force_stop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let (_, current, pending) = self.pipeline.state(Some(gst::ClockTime::from_seconds(1)));
        if current != gst::State::Null {
            tracing::warn!(
                ?current,
                ?pending,
                "pipeline did not reach Null during forced stop"
            );
        }
        if let Some(bus) = self.pipeline.bus() {
            bus.set_flushing(true);
        }
    }
}

fn build_pipeline(description: &PipelineDescription) -> Result<gst::Pipeline, AnyError> {
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
            if let Some(range) = property.downcast_ref::<gst::glib::ParamSpecInt>() {
                if *value < range.minimum() || *value > range.maximum() {
                    return Err(format!(
                        "GStreamer element '{element_name}' property '{property_name}' rejects value '{value}' outside {}..={}",
                        range.minimum(), range.maximum()
                    )
                    .into());
                }
                value.to_value()
            } else if element_name == "ximagesrc"
                && matches!(property_name, "startx" | "starty" | "endx" | "endy")
            {
                let range = property
                    .downcast_ref::<gst::glib::ParamSpecUInt>()
                    .ok_or_else(|| {
                        incompatible_property_type(element_name, property_name, expected_type)
                    })?;
                let value = u32::try_from(*value).map_err(|_| {
                    format!(
                        "GStreamer element '{element_name}' property '{property_name}' rejects negative value '{value}'"
                    )
                })?;
                if value < range.minimum() || value > range.maximum() {
                    return Err(format!(
                        "GStreamer element '{element_name}' property '{property_name}' rejects value '{value}' outside {}..={}",
                        range.minimum(), range.maximum()
                    )
                    .into());
                }
                value.to_value()
            } else {
                return Err(
                    incompatible_property_type(element_name, property_name, expected_type).into(),
                );
            }
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
    use std::sync::{Arc, Mutex};

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
        fn state_label(&self) -> String {
            "Playing".into()
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

    #[test]
    fn signed_nonnegative_coordinate_accepts_ximagesrc_property_type() {
        ensure_initialized().unwrap();
        let element = gst::ElementFactory::make("ximagesrc").build().unwrap();
        let value =
            checked_property_value(&element, "ximagesrc", "startx", &PropertyValue::I32(1920))
                .unwrap();
        assert_eq!(
            value.type_(),
            element.find_property("startx").unwrap().value_type()
        );
    }

    struct StoppingFake {
        accept_eos: bool,
        terminal: Option<Result<(), String>>,
        forced: Arc<Mutex<bool>>,
    }

    impl CapturePipeline for StoppingFake {
        fn start(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn is_healthy(&self) -> bool {
            true
        }
        fn send_eos(&mut self) -> bool {
            self.accept_eos
        }
        fn poll_terminal(&mut self) -> Option<Result<(), String>> {
            self.terminal.clone()
        }
        fn state_label(&self) -> String {
            "Playing".into()
        }
        fn force_stop(&mut self) {
            *self.forced.lock().unwrap() = true;
        }
    }

    #[test]
    fn rejected_eos_forces_null_without_burning_timeout() {
        let forced = Arc::new(Mutex::new(false));
        let mut pipeline: Box<dyn CapturePipeline> = Box::new(StoppingFake {
            accept_eos: false,
            terminal: None,
            forced: forced.clone(),
        });
        let mut pipelines = [StoppingPipeline {
            identity: "DP-1".into(),
            pipeline: &mut pipeline,
        }];
        let started = Instant::now();
        stop_pipelines(&mut pipelines, Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(*forced.lock().unwrap());
    }

    #[test]
    fn all_streams_share_one_eos_deadline() {
        let first = Arc::new(Mutex::new(false));
        let second = Arc::new(Mutex::new(false));
        let mut a: Box<dyn CapturePipeline> = Box::new(StoppingFake {
            accept_eos: true,
            terminal: None,
            forced: first.clone(),
        });
        let mut b: Box<dyn CapturePipeline> = Box::new(StoppingFake {
            accept_eos: true,
            terminal: None,
            forced: second.clone(),
        });
        let mut pipelines = [
            StoppingPipeline {
                identity: "DP-1".into(),
                pipeline: &mut a,
            },
            StoppingPipeline {
                identity: "DP-2".into(),
                pipeline: &mut b,
            },
        ];
        let started = Instant::now();
        stop_pipelines(&mut pipelines, Duration::from_millis(60));
        assert!(started.elapsed() < Duration::from_millis(110));
        assert!(*first.lock().unwrap());
        assert!(*second.lock().unwrap());
    }
}
