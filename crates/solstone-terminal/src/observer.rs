// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use time::{OffsetDateTime, UtcOffset};

use crate::clock::{Clock, Zone, ZoneSource, local_date_and_time};
use crate::command::CommandRunner;
use crate::indicator::{IndicatorError, IndicatorIo, IndicatorOwnership, ServerIndicator};
use crate::model::{CaptureResult, PollOutcome};
use crate::name::DerivedName;
use crate::paths::ensure_private_directory;
use crate::segment::{SegmentClose, SegmentError, SegmentState};
use crate::storage::FaultPlan;
use crate::tmux::{TmuxAdapter, WarningSink};

pub const SYNCING_LINGER: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncActivity {
    Idle,
    Working,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownEvent {
    SigInt,
    SigTerm,
    Injected,
}

#[derive(Debug)]
pub struct ObserverOperationError(pub String);

impl fmt::Display for ObserverOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for ObserverOperationError {}

pub trait CaptureProvider: Send + Sync {
    fn poll<'a>(
        &'a self,
        wall_now: OffsetDateTime,
        capture_interval: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<PollOutcome, ObserverOperationError>> + Send + 'a>>;
}

impl<R, W> CaptureProvider for TmuxAdapter<R, W>
where
    R: CommandRunner + 'static,
    W: WarningSink + 'static,
{
    fn poll<'a>(
        &'a self,
        wall_now: OffsetDateTime,
        capture_interval: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<PollOutcome, ObserverOperationError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.poll_context(wall_now, capture_interval)
                .await
                .map_err(|error| ObserverOperationError(error.to_string()))
        })
    }
}

pub trait SegmentLifecycle: Send {
    fn process_poll(
        &mut self,
        captures: &[CaptureResult],
        activity_jsonl: &[u8],
        wall_now: OffsetDateTime,
        monotonic_now: Duration,
        segment_interval: Duration,
    ) -> Result<(), ObserverOperationError>;

    fn shutdown(&mut self, monotonic_now: Duration)
    -> Result<SegmentClose, ObserverOperationError>;
}

#[derive(Clone, Default)]
pub struct SyncWake {
    sink: Option<Arc<dyn Fn(SegmentClose) + Send + Sync>>,
}

impl SyncWake {
    pub fn new(sink: impl Fn(SegmentClose) + Send + Sync + 'static) -> Self {
        Self {
            sink: Some(Arc::new(sink)),
        }
    }

    pub fn segment_closed(&self, close: &SegmentClose) {
        if let Some(sink) = &self.sink {
            sink(close.clone());
        }
    }
}

pub type LayoutFn = Arc<dyn Fn(&str) -> PathBuf + Send + Sync>;

/// How a segment warning about the local time zone begins; every other segment
/// warning is an append rollback.
pub(crate) const ZONE_LOAD_WARNING: &str = "could not load the local time zone";

#[allow(clippy::too_many_arguments)]
fn open_segment(
    layout: &LayoutFn,
    wall_now: OffsetDateTime,
    monotonic_now: Duration,
    segment_interval: Duration,
    source: &mut dyn ZoneSource,
    last_zone: &mut Option<Zone>,
    zone_failure_warned: &mut bool,
    warnings: &dyn WarningSink,
    faults: FaultPlan,
) -> Result<SegmentState, SegmentError> {
    let (zone, tz) = match source.read() {
        Ok(zone) => {
            *last_zone = Some(zone.clone());
            *zone_failure_warned = false;
            let tz = zone.iana_name();
            (zone, tz)
        }
        Err(cause) => {
            if !*zone_failure_warned {
                warnings.warn(&format!(
                    "{ZONE_LOAD_WARNING} ({cause}); set TZ or repair /etc/localtime"
                ));
                *zone_failure_warned = true;
            }
            if let Some(prev) = last_zone.as_ref() {
                let tz = prev.iana_name();
                (prev.clone(), tz)
            } else {
                (Zone::utc(), Some("UTC".to_owned()))
            }
        }
    };
    let local_offset = zone.offset_at(wall_now);
    let local = wall_now.to_offset(local_offset);
    let mut cur_date = local.date();
    let mut cur_h = local.hour();
    let mut cur_m = local.minute();
    let mut cur_s = local.second();

    let natural_date_str = format!(
        "{:04}{:02}{:02}",
        cur_date.year(),
        u8::from(cur_date.month()),
        cur_date.day()
    );
    let natural_stream_dir = layout(&natural_date_str);
    ensure_private_directory(&natural_stream_dir).map_err(SegmentError::Path)?;

    let natural_stem = format!(
        "{:02}{:02}{:02}",
        local.hour(),
        local.minute(),
        local.second()
    );

    let mut bumped = false;
    let (chosen_stream_dir, chosen_stem) = loop {
        let stem_str = format!("{:02}{:02}{:02}", cur_h, cur_m, cur_s);
        let date_str = format!(
            "{:04}{:02}{:02}",
            cur_date.year(),
            u8::from(cur_date.month()),
            cur_date.day()
        );
        let stream_dir = layout(&date_str);
        if let Err(error) = ensure_private_directory(&stream_dir) {
            if bumped {
                break (natural_stream_dir, natural_stem);
            }
            return Err(SegmentError::Path(error));
        }

        let entries = match std::fs::read_dir(&stream_dir) {
            Ok(entries) => entries,
            Err(_) => {
                break (natural_stream_dir, natural_stem);
            }
        };

        let mut is_taken = false;
        let mut scan_error = false;
        for entry in entries {
            let Ok(entry) = entry else {
                scan_error = true;
                break;
            };
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if name_str.starts_with(&stem_str) {
                is_taken = true;
                break;
            }
        }

        if scan_error {
            break (natural_stream_dir, natural_stem);
        }

        if !is_taken {
            break (stream_dir, stem_str);
        }

        bumped = true;
        if cur_s < 59 {
            cur_s += 1;
        } else {
            cur_s = 0;
            if cur_m < 59 {
                cur_m += 1;
            } else {
                cur_m = 0;
                if cur_h < 23 {
                    cur_h += 1;
                } else {
                    cur_h = 0;
                    cur_date = cur_date.next_day().expect("calendar date next day");
                }
            }
        }
    };

    SegmentState::create_with_faults(
        &chosen_stream_dir,
        &chosen_stem,
        wall_now,
        monotonic_now,
        local_offset,
        tz.as_deref(),
        segment_interval,
        faults,
    )
}

pub struct SegmentManager {
    segment: SegmentState,
    layout: LayoutFn,
    sync_wake: SyncWake,
    source: Box<dyn ZoneSource>,
    warnings: Arc<dyn WarningSink>,
    last_zone: Option<Zone>,
    zone_failure_warned: bool,
}

impl SegmentManager {
    pub fn start(
        data_root: PathBuf,
        stream: DerivedName,
        clock: &dyn Clock,
        segment_interval: Duration,
        sync_wake: SyncWake,
        source: Box<dyn ZoneSource>,
        warnings: Arc<dyn WarningSink>,
    ) -> Result<Self, SegmentError> {
        let layout: LayoutFn =
            Arc::new(move |day: &str| data_root.join("captures").join(day).join(stream.as_str()));
        Self::start_with_layout(layout, clock, segment_interval, sync_wake, source, warnings)
    }

    pub fn start_with_layout(
        layout: LayoutFn,
        clock: &dyn Clock,
        segment_interval: Duration,
        sync_wake: SyncWake,
        mut source: Box<dyn ZoneSource>,
        warnings: Arc<dyn WarningSink>,
    ) -> Result<Self, SegmentError> {
        let wall_now = clock.wall_now();
        let monotonic_now = clock.monotonic_now();
        let mut last_zone = None;
        let mut zone_failure_warned = false;
        let segment = open_segment(
            &layout,
            wall_now,
            monotonic_now,
            segment_interval,
            &mut *source,
            &mut last_zone,
            &mut zone_failure_warned,
            &*warnings,
            FaultPlan::default(),
        )?;
        Ok(Self {
            segment,
            layout,
            sync_wake,
            source,
            warnings,
            last_zone,
            zone_failure_warned,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        segment: SegmentState,
        data_root: PathBuf,
        stream: DerivedName,
        sync_wake: SyncWake,
        source: Box<dyn ZoneSource>,
        warnings: Arc<dyn WarningSink>,
        last_zone: Option<Zone>,
        zone_failure_warned: bool,
    ) -> Self {
        let layout: LayoutFn =
            Arc::new(move |day: &str| data_root.join("captures").join(day).join(stream.as_str()));
        Self {
            segment,
            layout,
            sync_wake,
            source,
            warnings,
            last_zone,
            zone_failure_warned,
        }
    }

    pub fn new_with_layout(
        segment: SegmentState,
        layout: LayoutFn,
        sync_wake: SyncWake,
        source: Box<dyn ZoneSource>,
        warnings: Arc<dyn WarningSink>,
        last_zone: Option<Zone>,
        zone_failure_warned: bool,
    ) -> Self {
        Self {
            segment,
            layout,
            sync_wake,
            source,
            warnings,
            last_zone,
            zone_failure_warned,
        }
    }

    pub fn segment_mut(&mut self) -> &mut SegmentState {
        &mut self.segment
    }

    pub fn segment(&self) -> &SegmentState {
        &self.segment
    }
}

impl SegmentLifecycle for SegmentManager {
    fn process_poll(
        &mut self,
        captures: &[CaptureResult],
        activity_jsonl: &[u8],
        wall_now: OffsetDateTime,
        monotonic_now: Duration,
        segment_interval: Duration,
    ) -> Result<(), ObserverOperationError> {
        if self.segment.rotation_due(monotonic_now, segment_interval) {
            let close = self
                .segment
                .finalize(monotonic_now)
                .map_err(operation_error)?;
            self.sync_wake.segment_closed(&close);
            self.segment = open_segment(
                &self.layout,
                wall_now,
                monotonic_now,
                segment_interval,
                &mut *self.source,
                &mut self.last_zone,
                &mut self.zone_failure_warned,
                &*self.warnings,
                FaultPlan::default(),
            )
            .map_err(operation_error)?;
        }
        let timestamp = self.segment.frame_timestamp(wall_now);
        for capture in captures {
            if let Err(error) = self
                .segment
                .append_capture(capture, timestamp, monotonic_now)
            {
                if error.is_recoverable_append() {
                    self.warnings.warn(&format!(
                        "durable append for session {:?} rolled back and will retry: {error}",
                        capture.session
                    ));
                    continue;
                }
                return Err(operation_error(error));
            }
        }
        if !activity_jsonl.is_empty()
            && let Err(error) = self.segment.append_activity(activity_jsonl, monotonic_now)
        {
            if error.is_recoverable_append() {
                self.warnings.warn(&format!(
                    "durable activity append rolled back and will retry: {error}"
                ));
            } else {
                return Err(operation_error(error));
            }
        }
        Ok(())
    }

    fn shutdown(
        &mut self,
        monotonic_now: Duration,
    ) -> Result<SegmentClose, ObserverOperationError> {
        let close = self
            .segment
            .finalize(monotonic_now)
            .map_err(operation_error)?;
        Ok(close)
    }
}

pub fn stream_directory(
    data_root: &Path,
    stream: &DerivedName,
    wall_now: OffsetDateTime,
    local_offset: UtcOffset,
) -> Result<PathBuf, ObserverOperationError> {
    let (date, _) = local_date_and_time(wall_now, local_offset);
    stream
        .join_checked(&data_root.join("captures").join(date))
        .map_err(|error| ObserverOperationError(error.to_string()))
}

fn operation_error(error: SegmentError) -> ObserverOperationError {
    ObserverOperationError(error.to_string())
}

pub trait ShutdownIndicator: Send {
    fn set_activity<'a>(
        &'a mut self,
        _activity: SyncActivity,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn refresh_interval(&self) -> Option<Duration> {
        None
    }

    fn refresh<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn restore<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>>;
}

pub struct NoopShutdownIndicator;

impl ShutdownIndicator for NoopShutdownIndicator {
    fn restore<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

impl<I: IndicatorIo> ShutdownIndicator for IndicatorOwnership<I> {
    fn set_activity<'a>(
        &'a mut self,
        activity: SyncActivity,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async move {
            let value = match activity {
                SyncActivity::Idle => crate::indicator::OBSERVING_VALUE,
                SyncActivity::Working => crate::indicator::SYNCING_VALUE,
            };
            self.update_solstone(value.to_owned())
                .await
                .map(|_| ())
                .map_err(indicator_update_error)
        })
    }

    fn restore<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async move {
            IndicatorOwnership::restore(self)
                .await
                .map_err(|error| ObserverOperationError(error.to_string()))
        })
    }
}

impl<I: IndicatorIo + Clone> ShutdownIndicator for ServerIndicator<I> {
    fn set_activity<'a>(
        &'a mut self,
        activity: SyncActivity,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async move {
            self.set_working(activity == SyncActivity::Working)
                .await
                .map_err(indicator_update_error)
        })
    }

    fn refresh_interval(&self) -> Option<Duration> {
        Some(ServerIndicator::refresh_interval(self))
    }

    fn refresh<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async move {
            ServerIndicator::refresh(self)
                .await
                .map_err(indicator_update_error)
        })
    }

    fn restore<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObserverOperationError>> + Send + 'a>> {
        Box::pin(async move {
            ServerIndicator::restore(self)
                .await
                .map_err(|error| ObserverOperationError(error.to_string()))
        })
    }
}

fn indicator_update_error(error: IndicatorError) -> ObserverOperationError {
    ObserverOperationError(format!("indicator update failed: {error}"))
}

#[derive(Default, Debug)]
pub struct FailureWarning {
    failing: bool,
}

impl FailureWarning {
    pub fn record_failure<F>(&mut self, on_first_failure: F)
    where
        F: FnOnce(),
    {
        if !self.failing {
            on_first_failure();
            self.failing = true;
        }
    }

    pub fn record_success(&mut self) {
        self.failing = false;
    }

    pub fn record<E: fmt::Display>(&mut self, result: Result<(), E>, warnings: &dyn WarningSink) {
        match result {
            Ok(()) => self.failing = false,
            Err(error) => {
                if !self.failing {
                    warnings.warn(&error.to_string());
                }
                self.failing = true;
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ActivityLinger {
    shown: Option<SyncActivity>,
    idle_at: Option<std::time::Instant>,
}

impl ActivityLinger {
    pub fn observe(
        &mut self,
        activity: SyncActivity,
        now: std::time::Instant,
    ) -> Option<SyncActivity> {
        match activity {
            SyncActivity::Working => {
                self.idle_at = None;
                self.show(SyncActivity::Working)
            }
            SyncActivity::Idle if self.shown == Some(SyncActivity::Working) => {
                self.idle_at.get_or_insert(now + SYNCING_LINGER);
                None
            }
            SyncActivity::Idle => self.show(SyncActivity::Idle),
        }
    }

    pub fn ends_at(&self) -> Option<std::time::Instant> {
        self.idle_at
    }

    pub fn expire(&mut self, now: std::time::Instant) -> Option<SyncActivity> {
        match self.idle_at {
            Some(at) if now >= at => {
                self.idle_at = None;
                self.show(SyncActivity::Idle)
            }
            _ => None,
        }
    }

    fn show(&mut self, activity: SyncActivity) -> Option<SyncActivity> {
        (self.shown != Some(activity)).then(|| {
            self.shown = Some(activity);
            activity
        })
    }
}
