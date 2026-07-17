// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use x11rb::{
    connection::Connection,
    protocol::{
        dpms::{self, DPMSMode},
        screensaver,
        xproto::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

use super::{BackendOutcome, DpmsPower, IDLE_THRESHOLD, PowerObservation, XActivityOps};

pub fn dpms_mode(mode: DPMSMode) -> bool {
    mode == DPMSMode::STANDBY || mode == DPMSMode::SUSPEND || mode == DPMSMode::OFF
}

pub struct NativeX11 {
    connection: Option<(RustConnection, usize)>,
}

impl Default for NativeX11 {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeX11 {
    pub fn new() -> Self {
        Self {
            connection: x11rb::connect(None).ok(),
        }
    }

    pub fn power(&self) -> BackendOutcome<PowerObservation> {
        match self.power_level() {
            BackendOutcome::Available(value) => BackendOutcome::Available(PowerObservation {
                power_save: dpms_mode(value),
                readable: true,
            }),
            BackendOutcome::Absent => BackendOutcome::Absent,
            BackendOutcome::Broken(error) => BackendOutcome::Broken(error),
        }
    }

    fn power_level(&self) -> BackendOutcome<DPMSMode> {
        let Some((connection, _)) = &self.connection else {
            return BackendOutcome::Absent;
        };
        match connection.query_extension(b"DPMS") {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) if !reply.present => return BackendOutcome::Absent,
                Ok(_) => {}
                Err(error) => return BackendOutcome::Broken(error.to_string()),
            },
            Err(error) => return BackendOutcome::Broken(error.to_string()),
        }
        match dpms::info(connection) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) if reply.state => BackendOutcome::Available(reply.power_level),
                Ok(_) => BackendOutcome::Available(DPMSMode::ON),
                Err(error) => BackendOutcome::Broken(error.to_string()),
            },
            Err(error) => BackendOutcome::Broken(error.to_string()),
        }
    }

    pub fn idle(&self) -> BackendOutcome<bool> {
        match self.idle_ms() {
            BackendOutcome::Available(ms) => {
                BackendOutcome::Available(Duration::from_millis(ms) >= IDLE_THRESHOLD)
            }
            BackendOutcome::Absent => BackendOutcome::Absent,
            BackendOutcome::Broken(error) => BackendOutcome::Broken(error),
        }
    }

    fn idle_ms(&self) -> BackendOutcome<u64> {
        let Some((connection, screen)) = &self.connection else {
            return BackendOutcome::Absent;
        };
        match connection.query_extension(b"MIT-SCREEN-SAVER") {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) if !reply.present => return BackendOutcome::Absent,
                Ok(_) => {}
                Err(error) => return BackendOutcome::Broken(error.to_string()),
            },
            Err(error) => return BackendOutcome::Broken(error.to_string()),
        }
        let root = connection.setup().roots[*screen].root;
        match screensaver::query_info(connection, root) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => BackendOutcome::Available(u64::from(reply.ms_since_user_input)),
                Err(error) => BackendOutcome::Broken(error.to_string()),
            },
            Err(error) => BackendOutcome::Broken(error.to_string()),
        }
    }
}

impl XActivityOps for NativeX11 {
    fn dpms_state(&mut self) -> BackendOutcome<DpmsPower> {
        match self.power_level() {
            BackendOutcome::Available(DPMSMode::ON) => BackendOutcome::Available(DpmsPower::On),
            BackendOutcome::Available(DPMSMode::STANDBY) => {
                BackendOutcome::Available(DpmsPower::Standby)
            }
            BackendOutcome::Available(DPMSMode::SUSPEND) => {
                BackendOutcome::Available(DpmsPower::Suspend)
            }
            BackendOutcome::Available(DPMSMode::OFF) => BackendOutcome::Available(DpmsPower::Off),
            BackendOutcome::Available(_) => {
                BackendOutcome::Broken("unknown DPMS power level".into())
            }
            BackendOutcome::Absent => BackendOutcome::Absent,
            BackendOutcome::Broken(error) => BackendOutcome::Broken(error),
        }
    }

    fn screensaver_idle_ms(&mut self) -> BackendOutcome<u64> {
        self.idle_ms()
    }
}

use std::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpms_behavior_mapping_supersedes_xset_parser() {
        // tests/test_activity.py::TestIsDpmsActive::test_monitor_on_returns_false
        assert!(!dpms_mode(DPMSMode::ON));
        // tests/test_activity.py::TestIsDpmsActive::test_monitor_standby_returns_true
        assert!(dpms_mode(DPMSMode::STANDBY));
        // No 1:1 Python ancestor: xset suite omitted Suspend; protocol behavior pins it true.
        assert!(dpms_mode(DPMSMode::SUSPEND));
        // tests/test_activity.py::TestIsDpmsActive::test_monitor_off_returns_true
        assert!(dpms_mode(DPMSMode::OFF));
        // tests/test_activity.py::TestIsDpmsActive::test_xset_missing_returns_false
        // tests/test_activity.py::TestIsDpmsActive::test_xset_nonzero_returns_false
        // tests/test_activity.py::TestIsDpmsActive::test_no_monitor_line_returns_false
        // NativeX11 maps absent extensions and query failures to non-power-save outcomes.
    }
}
