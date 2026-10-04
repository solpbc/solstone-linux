// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Host mode: the process a browser starts for native messaging. It holds no journal
//! credential and opens no journal session. It relays whole frames between the
//! extension (stdin/stdout) and the running app's same-user endpoint, and writes
//! nothing but frames to stdout.

use super::argv::HostInvocation;
use native_browser_frame::{Assembler, Chunk, Direction, Step};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::Path,
    sync::LazyLock,
    thread,
};

/// What the extension is told when the app is not running.
pub static UNAVAILABLE_HELLO_ACK: LazyLock<Vec<u8>> = LazyLock::new(|| {
    format!(
        r#"{{"type":"hello_ack","capture":"unavailable","delivery":"unknown","freshness_ms":0,"destination_generation":null,"period_id":null,"custody":{{"full":false,"stale":false}},"version":"{}"}}"#,
        native_browser_frame::BUNDLE_VERSION
    )
    .into_bytes()
});
const ARGV_REJECTED: &[u8] = br#"{"code":"argv_rejected"}"#;

/// Exit code for a refused native-messaging launch.
pub const EXIT_REFUSED: i32 = 1;

pub fn refuse(output: &mut dyn Write) -> i32 {
    let _ = write_frame(output, ARGV_REJECTED);
    EXIT_REFUSED
}

pub fn local_hello(invocation: HostInvocation) -> Vec<u8> {
    format!(
        r#"{{"brand":"{}","mode":"{}","type":"local_hello"}}"#,
        invocation.brand.as_str(),
        invocation.channel.as_str()
    )
    .into_bytes()
}

pub fn run(invocation: HostInvocation) -> i32 {
    let mut stdout = io::stdout().lock();
    let endpoint = match super::production_endpoint_path() {
        Ok(endpoint) => endpoint,
        Err(_) => {
            let _ = write_frame(&mut stdout, &UNAVAILABLE_HELLO_ACK);
            return 0;
        }
    };
    let socket = match connect(&endpoint, rustix::process::geteuid().as_raw()) {
        Ok(socket) => socket,
        Err(Connect::Unavailable) => {
            let _ = write_frame(&mut stdout, &UNAVAILABLE_HELLO_ACK);
            return 0;
        }
        Err(Connect::Untrusted) => return 0,
    };
    drop(stdout);
    relay(invocation, socket, io::stdin(), io::stdout())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Connect {
    /// Nothing is listening: the app is not running.
    Unavailable,
    /// Something is there that this user does not own outright. Say nothing.
    Untrusted,
}

/// Connect only to an endpoint in a directory this user owns with mode 0700, held by a
/// process of the same user.
pub fn connect(endpoint: &Path, uid: u32) -> Result<UnixStream, Connect> {
    let directory = endpoint.parent().ok_or(Connect::Untrusted)?;
    match fs::symlink_metadata(directory) {
        Ok(meta) if meta.is_dir() && meta.uid() == uid && meta.mode() & 0o777 == 0o700 => {}
        Ok(_) => return Err(Connect::Untrusted),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Connect::Unavailable);
        }
        Err(_) => return Err(Connect::Untrusted),
    }
    match fs::symlink_metadata(endpoint) {
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == uid => {}
        Ok(_) => return Err(Connect::Untrusted),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Connect::Unavailable);
        }
        Err(_) => return Err(Connect::Untrusted),
    }
    let socket = match UnixStream::connect(endpoint) {
        Ok(socket) => socket,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Connect::Unavailable);
        }
        Err(_) => return Err(Connect::Untrusted),
    };
    match rustix::net::sockopt::socket_peercred(&socket) {
        Ok(credentials) if credentials.uid.as_raw() == uid => Ok(socket),
        _ => Err(Connect::Untrusted),
    }
}

/// Relay frames both ways until either side ends. Returns the process exit code.
pub fn relay<R, W>(invocation: HostInvocation, socket: UnixStream, input: R, output: W) -> i32
where
    R: Read + Send + 'static,
    W: Write,
{
    let Ok(mut to_app) = socket.try_clone() else {
        return 0;
    };
    if write_frame(&mut to_app, &local_hello(invocation)).is_err() {
        return 0;
    }
    let upstream = thread::spawn(move || {
        let _ = pump(input, &mut to_app, Direction::ExtensionToHost, |_| false);
        // The extension went away: the app sees end of input and ends the session.
        let _ = to_app.shutdown(std::net::Shutdown::Write);
    });
    let mut output = output;
    let _ = pump(socket, &mut output, Direction::HostToExtension, |body| {
        matches!(message_type(body).as_deref(), Some("bye" | "unsupported"))
    });
    let _ = output.flush();
    // Either the app ended the session or the extension did; there is nothing left to
    // relay, so the input pump is not waited for.
    drop(upstream);
    0
}

/// Copy whole frames from `input` to `output`, stopping at end of input, a framing
/// error, a write failure, or after a frame `stop_after` matches.
fn pump<R: Read, W: Write + ?Sized>(
    mut input: R,
    output: &mut W,
    direction: Direction,
    stop_after: impl Fn(&[u8]) -> bool,
) -> io::Result<()> {
    let mut assembler = Assembler::new(direction);
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = match input.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let chunk = if count == 0 {
            Chunk::Eof
        } else {
            Chunk::Data(&buffer[..count])
        };
        let steps = assembler.feed(chunk).map_err(io::Error::other)?;
        for step in steps {
            match step {
                Step::Message(body) => {
                    write_frame(output, &body)?;
                    if stop_after(&body) {
                        return Ok(());
                    }
                }
                Step::CleanEof => return Ok(()),
                Step::EmptyRead | Step::NeedMore => {}
            }
        }
    }
}

pub fn write_frame<W: Write + ?Sized>(output: &mut W, body: &[u8]) -> io::Result<()> {
    let length = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&length.to_ne_bytes());
    frame.extend_from_slice(body);
    output.write_all(&frame)?;
    output.flush()
}

fn message_type(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("type")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::argv::{BrandHint, Channel};
    use native_browser_frame::{DecodeOutcome, decode};
    use std::os::unix::fs::PermissionsExt;

    fn invocation() -> HostInvocation {
        HostInvocation {
            brand: BrandHint::Firefox,
            channel: Channel::Production,
        }
    }

    fn read_frame(input: &mut impl Read) -> Vec<u8> {
        let mut length = [0_u8; 4];
        input.read_exact(&mut length).unwrap();
        let mut body = vec![0_u8; u32::from_ne_bytes(length) as usize];
        input.read_exact(&mut body).unwrap();
        body
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, body).unwrap();
        out
    }

    #[test]
    fn the_unavailable_answer_is_a_valid_hello_ack() {
        assert!(matches!(
            decode(&UNAVAILABLE_HELLO_ACK, Direction::HostToExtension),
            DecodeOutcome::Accept(_)
        ));
    }

    #[test]
    fn local_hello_names_the_launch() {
        assert_eq!(
            local_hello(invocation()),
            br#"{"brand":"firefox","mode":"production","type":"local_hello"}"#.to_vec()
        );
    }

    #[test]
    fn connect_reports_a_missing_app_and_refuses_a_loose_directory() {
        let temp = tempfile::tempdir().unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let directory = temp.path().join("solstone-linux");
        let endpoint = directory.join("browser-host.sock");
        assert_eq!(connect(&endpoint, uid).unwrap_err(), Connect::Unavailable);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(connect(&endpoint, uid).unwrap_err(), Connect::Untrusted);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(connect(&endpoint, uid).unwrap_err(), Connect::Unavailable);
        let listener = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        assert!(connect(&endpoint, uid).is_ok());
        assert_eq!(
            connect(&endpoint, uid.wrapping_add(1)).unwrap_err(),
            Connect::Untrusted
        );
        drop(listener);
        assert_eq!(connect(&endpoint, uid).unwrap_err(), Connect::Unavailable);
    }

    #[test]
    fn relay_forwards_whole_frames_both_ways_and_stops_after_bye() {
        let (host_side, mut app_side) = UnixStream::pair().unwrap();
        let hello =
            br#"{"type":"hello","protocol":1,"version":"0.2.0","brand":"firefox","inst":"i"}"#;
        // The extension's frame arrives split across reads.
        let input = io::Cursor::new(frame(hello));
        let app = thread::spawn(move || {
            assert_eq!(read_frame(&mut app_side), local_hello(invocation()));
            assert_eq!(read_frame(&mut app_side), hello.to_vec());
            app_side
                .write_all(&frame(br#"{"type":"bye","reason":"shutdown"}"#))
                .unwrap();
            app_side
                .write_all(&frame(br#"{"type":"state","after":"bye"}"#))
                .unwrap();
            app_side
        });
        let mut output = Vec::new();
        assert_eq!(relay(invocation(), host_side, input, &mut output), 0);
        let _app_side = app.join().unwrap();
        let mut cursor = io::Cursor::new(output);
        assert_eq!(
            read_frame(&mut cursor),
            br#"{"type":"bye","reason":"shutdown"}"#.to_vec()
        );
        assert_eq!(cursor.position() as usize, cursor.get_ref().len());
    }

    #[test]
    fn relay_refuses_an_oversized_frame_from_the_app() {
        let (host_side, mut app_side) = UnixStream::pair().unwrap();
        let app = thread::spawn(move || {
            let _ = read_frame(&mut app_side);
            app_side.write_all(&(70_000_u32).to_ne_bytes()).unwrap();
            app_side
        });
        let mut output = Vec::new();
        relay(invocation(), host_side, io::empty(), &mut output);
        let _app_side = app.join().unwrap();
        assert!(output.is_empty());
    }
}
