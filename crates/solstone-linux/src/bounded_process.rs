// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io::{self, Read};
use std::os::fd::AsFd;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
    pub timed_out: bool,
}

fn nonblocking(fd: impl AsFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(&fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
    let mut truncated = false;
    let mut buffer = [0; 8192];
    // Bound work per turn even when a child writes continuously.
    for _ in 0..32 {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                let keep = n.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..keep]);
                truncated |= keep < n;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(truncated)
}

/// Capture bounded diagnostics without waiting for pipe EOF held by a descendant.
/// A missing deadline leaves lifetime control to the caller or the child's own deadlines.
pub fn run(command: &mut Command, deadline: Option<Duration>, limit: usize) -> io::Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing stderr"))?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let started = Instant::now();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut truncated = false;
        let mut timed_out = false;
        loop {
            truncated |= drain(&mut stdout, &mut out, limit)?;
            truncated |= drain(&mut stderr, &mut err, limit)?;
            if let Some(status) = child.try_wait()? {
                truncated |= drain(&mut stdout, &mut out, limit)?;
                truncated |= drain(&mut stderr, &mut err, limit)?;
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                    truncated,
                    timed_out,
                });
            }
            if !timed_out && deadline.is_some_and(|bound| started.elapsed() >= bound) {
                child.kill()?;
                timed_out = true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_output_deadline_and_descendant_pipe_lifetime() {
        let started = Instant::now();
        let out = run(
            Command::new("sh").args(["-c", "printf abcdef; printf failure >&2; sleep 5 & exit 7"]),
            Some(Duration::from_secs(1)),
            4,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(out.status.code(), Some(7));
        assert_eq!(out.stdout, b"abcd");
        assert_eq!(out.stderr, b"fail");
        assert!(out.truncated);
        let out = run(
            Command::new("sleep").arg("2"),
            Some(Duration::from_millis(20)),
            4,
        )
        .unwrap();
        assert!(out.timed_out);
        assert!(!out.status.success());
    }
}
