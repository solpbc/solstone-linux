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

use super::{BackendOutcome, DpmsPower, XActivityOps};

fn map_dpms_mode(mode: DPMSMode) -> Result<DpmsPower, String> {
    match mode {
        DPMSMode::ON => Ok(DpmsPower::On),
        DPMSMode::STANDBY => Ok(DpmsPower::Standby),
        DPMSMode::SUSPEND => Ok(DpmsPower::Suspend),
        DPMSMode::OFF => Ok(DpmsPower::Off),
        _ => Err("unknown DPMS power level".into()),
    }
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
            BackendOutcome::Available(mode) => match map_dpms_mode(mode) {
                Ok(power) => BackendOutcome::Available(power),
                Err(error) => BackendOutcome::Broken(error),
            },
            BackendOutcome::Absent => BackendOutcome::Absent,
            BackendOutcome::Broken(error) => BackendOutcome::Broken(error),
        }
    }

    fn screensaver_idle_ms(&mut self) -> BackendOutcome<u64> {
        self.idle_ms()
    }

    fn dpms_available(&mut self) -> bool {
        self.extension_available(b"DPMS")
    }

    fn screensaver_available(&mut self) -> bool {
        self.extension_available(b"MIT-SCREEN-SAVER")
    }
}

impl NativeX11 {
    fn extension_available(&self, name: &[u8]) -> bool {
        let Some((connection, _)) = &self.connection else {
            return false;
        };
        connection
            .query_extension(name)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| reply.present)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpms_behavior_mapping_supersedes_xset_parser() {
        // tests/test_activity.py::TestIsDpmsActive::test_monitor_on_returns_false
        assert_eq!(map_dpms_mode(DPMSMode::ON), Ok(DpmsPower::On));
        // tests/test_activity.py::TestIsDpmsActive::test_monitor_standby_returns_true
        assert_eq!(map_dpms_mode(DPMSMode::STANDBY), Ok(DpmsPower::Standby));
        // No 1:1 Python ancestor: xset suite omitted Suspend; protocol behavior pins it true.
        assert_eq!(map_dpms_mode(DPMSMode::SUSPEND), Ok(DpmsPower::Suspend));
        // tests/test_activity.py::TestIsDpmsActive::test_monitor_off_returns_true
        assert_eq!(map_dpms_mode(DPMSMode::OFF), Ok(DpmsPower::Off));
    }
}
