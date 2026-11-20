// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! A private, pairing-scoped connection to the installed GTK journal window.
use std::{
    io::{self, Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

pub(crate) struct JournalViewer {
    process: Child,
    channel: UnixStream,
}

impl JournalViewer {
    fn start(program: &Path) -> io::Result<Self> {
        let (channel, peer) = UnixStream::pair()?;
        let timeout = Some(Duration::from_secs(8));
        channel.set_read_timeout(timeout)?;
        channel.set_write_timeout(timeout)?;
        let input = OwnedFd::from(peer.try_clone()?);
        let output = OwnedFd::from(peer);
        let process = Command::new(program)
            .env("SOLSTONE_JOURNAL_LANGUAGE", solstone_i18n::locale())
            .env(
                "SOLSTONE_JOURNAL_NEW_NAME",
                solstone_i18n::text!("linux.window.choosenewdownloadname"),
            )
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { process, channel })
    }

    pub(crate) fn open(slot: &mut Option<Self>, url: &str) -> io::Result<()> {
        if let Some(viewer) = slot
            && viewer.process.try_wait()?.is_some()
        {
            *slot = None;
        }
        if slot.is_none() {
            let executable = crate::service::current_executable()?;
            let parent = executable.parent().ok_or(io::ErrorKind::InvalidInput)?;
            *slot = Some(Self::start(&parent.join("solstone-linux-journal"))?);
        }
        let viewer = slot.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
        let result = viewer.navigate(url);
        if result.is_err() {
            *slot = None;
        }
        result
    }

    fn navigate(&mut self, url: &str) -> io::Result<()> {
        // A fixed-size reply acknowledges the committed destination, not merely spawn.
        writeln!(self.channel, "{url}")?;
        let mut reply = [0];
        self.channel.read_exact(&mut reply)?;
        if reply[0] == b'+' {
            Ok(())
        } else {
            Err(io::ErrorKind::ConnectionAborted.into())
        }
    }
}

impl Drop for JournalViewer {
    fn drop(&mut self) {
        let _ = self.channel.shutdown(std::net::Shutdown::Both);
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
