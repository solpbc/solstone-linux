// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use native_browser_frame::*;

#[test]
fn over_cap_prefix_does_not_retain_body() {
    let mut assembler = Assembler::new(Direction::ExtensionToHost);
    let over_cap_len = (EXTENSION_TO_HOST_MAX + 1) as u32;
    let prefix = over_cap_len.to_ne_bytes();

    let mut chunk = Vec::new();
    chunk.extend_from_slice(&prefix);
    chunk.extend_from_slice(b"extra bytes that should not be retained");

    let res = assembler.feed(Chunk::Data(&chunk));
    assert_eq!(res, Err(FrameError::OverCap));
    assert!(assembler.retained() <= 4);

    // Further feed fails
    let res2 = assembler.feed(Chunk::Data(b"more"));
    assert_eq!(res2, Err(FrameError::OverCap));

    // Reset clears failure
    assembler.reset();
    assert_eq!(assembler.retained(), 0);
}

#[test]
fn frame_empty_read_and_clean_eof() {
    let mut assembler = Assembler::new(Direction::HostToExtension);
    let res = assembler.feed(Chunk::Data(b"")).unwrap();
    assert_eq!(res, vec![Step::EmptyRead]);

    let res_eof = assembler.feed(Chunk::Eof).unwrap();
    assert_eq!(res_eof, vec![Step::CleanEof]);
}

#[test]
fn frame_truncated_prefix_and_body() {
    let mut assembler1 = Assembler::new(Direction::HostToExtension);
    assembler1.feed(Chunk::Data(&[1, 2])).unwrap();
    let res1 = assembler1.feed(Chunk::Eof);
    assert_eq!(res1, Err(FrameError::TruncatedPrefix));

    let mut assembler2 = Assembler::new(Direction::HostToExtension);
    let prefix = 10u32.to_ne_bytes();
    assembler2.feed(Chunk::Data(&prefix)).unwrap();
    assembler2.feed(Chunk::Data(b"123")).unwrap();
    let res2 = assembler2.feed(Chunk::Eof);
    assert_eq!(res2, Err(FrameError::TruncatedBody));
}

#[test]
fn frame_split_and_concatenated_messages() {
    let mut assembler = Assembler::new(Direction::ExtensionToHost);
    let msg1 = b"{\"type\":\"bye\",\"reason\":\"shutdown\"}";
    let msg2 = b"{\"type\":\"boundary\",\"destination_generation\":\"g1\",\"period_id\":\"p1\"}";

    let p1 = (msg1.len() as u32).to_ne_bytes();
    let p2 = (msg2.len() as u32).to_ne_bytes();

    let mut stream = Vec::new();
    stream.extend_from_slice(&p1);
    stream.extend_from_slice(msg1);
    stream.extend_from_slice(&p2);
    stream.extend_from_slice(msg2);

    // Feed byte by byte
    let mut collected = Vec::new();
    for byte in &stream {
        let steps = assembler.feed(Chunk::Data(&[*byte])).unwrap();
        for step in steps {
            if let Step::Message(m) = step {
                collected.push(m);
            }
        }
    }

    assert_eq!(collected.len(), 2);
    assert_eq!(collected[0], msg1);
    assert_eq!(collected[1], msg2);
}

#[test]
fn host_to_ext_over_cap_prefix_does_not_retain_body() {
    let mut assembler = Assembler::new(Direction::HostToExtension);
    let over_cap_len = (HOST_TO_EXTENSION_MAX + 1) as u32;
    let prefix = over_cap_len.to_ne_bytes();

    let mut chunk = Vec::new();
    chunk.extend_from_slice(&prefix);
    chunk.extend_from_slice(b"extra bytes that should not be retained");

    let res = assembler.feed(Chunk::Data(&chunk));
    assert_eq!(res, Err(FrameError::OverCap));
    assert!(assembler.retained() <= 4);

    let res2 = assembler.feed(Chunk::Data(b"more"));
    assert_eq!(res2, Err(FrameError::OverCap));

    assembler.reset();
    assert_eq!(assembler.retained(), 0);
}

#[test]
fn out_frame_short_writes() {
    let msg = b"{\"type\":\"bye\",\"reason\":\"shutdown\"}";
    let mut out = OutFrame::from_payload(Direction::HostToExtension, msg).unwrap();

    let expected_total = 4 + msg.len();
    assert_eq!(out.pending().len(), expected_total);

    // Short write 2 bytes
    let w1 = out.write_with(|pending| Ok::<usize, ()>(pending.len().min(2))).unwrap();
    assert_eq!(w1, 2);
    assert_eq!(out.pending().len(), expected_total - 2);

    // Empty write (0 bytes)
    let w0 = out.write_with(|_| Ok::<usize, ()>(0)).unwrap();
    assert_eq!(w0, 0);
    assert_eq!(out.pending().len(), expected_total - 2);

    // Write remainder
    let rem = out.pending().len();
    let w_rem = out.write_with(|pending| Ok::<usize, ()>(pending.len())).unwrap();
    assert_eq!(w_rem, rem);
    assert_eq!(out.pending().len(), 0);
}

#[test]
fn out_frame_write_with_clamped_to_pending() {
    let msg = b"{\"type\":\"bye\",\"reason\":\"shutdown\"}";
    let mut out = OutFrame::from_payload(Direction::HostToExtension, msg).unwrap();
    let expected_total = 4 + msg.len();

    // Closure returns 999999 (larger than pending)
    let written = out.write_with(|_| Ok::<usize, ()>(999999)).unwrap();
    assert_eq!(written, expected_total);
    assert_eq!(out.pending().len(), 0);
}

#[test]
fn frame_split_multibyte_and_escapes() {
    let mut assembler = Assembler::new(Direction::ExtensionToHost);
    let msg = "{\"type\":\"batch\",\"text\":\"Astral: \u{1F600}, Escape: \\u0001\"}".as_bytes();
    let prefix = (msg.len() as u32).to_ne_bytes();

    let mut full_stream = Vec::new();
    full_stream.extend_from_slice(&prefix);
    full_stream.extend_from_slice(msg);

    // Feed 3 bytes at a time, splitting multibyte astral emoji and escapes across chunk boundaries
    let mut collected = Vec::new();
    for chunk in full_stream.chunks(3) {
        let steps = assembler.feed(Chunk::Data(chunk)).unwrap();
        for step in steps {
            if let Step::Message(m) = step {
                collected.push(m);
            }
        }
    }

    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0], msg);
}

#[test]
fn assembler_isolation_and_reset() {
    let mut a1 = Assembler::new(Direction::ExtensionToHost);
    let a2 = Assembler::new(Direction::ExtensionToHost);

    // Feed partial prefix into a1
    a1.feed(Chunk::Data(&[0x10, 0x00])).unwrap();
    assert_eq!(a1.retained(), 2);
    assert_eq!(a2.retained(), 0);

    // Reset a1 clears its partial state
    a1.reset();
    assert_eq!(a1.retained(), 0);
}

#[test]
fn bad_utf8_payload_does_not_echo_bytes() {
    let invalid_utf8 = vec![0xff, 0xfe, 0xfd];
    let res = decode(&invalid_utf8, Direction::ExtensionToHost);
    match res {
        DecodeOutcome::Refuse(err) => {
            assert_eq!(err.code, "bad_utf8");
            let err_display = format!("{}", err);
            assert!(!err_display.contains("255"));
            assert!(!err_display.contains("0xff"));
        }
        other => panic!("expected Refuse(bad_utf8), got {:?}", other),
    }
}
