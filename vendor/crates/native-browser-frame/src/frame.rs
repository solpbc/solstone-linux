// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::constants::{EXTENSION_TO_HOST_MAX, HOST_TO_EXTENSION_MAX};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ExtensionToHost,
    HostToExtension,
}

impl Direction {
    pub fn socket_cap(self) -> usize {
        match self {
            Direction::ExtensionToHost => EXTENSION_TO_HOST_MAX,
            Direction::HostToExtension => HOST_TO_EXTENSION_MAX,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Message(Vec<u8>),
    EmptyRead,
    CleanEof,
    NeedMore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk<'a> {
    Data(&'a [u8]),
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    EmptyPayload,
    OverCap,
    TruncatedPrefix,
    TruncatedBody,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::EmptyPayload => write!(f, "empty_payload"),
            FrameError::OverCap => write!(f, "over_cap"),
            FrameError::TruncatedPrefix => write!(f, "truncated_prefix"),
            FrameError::TruncatedBody => write!(f, "truncated_body"),
        }
    }
}

impl std::error::Error for FrameError {}

pub struct Assembler {
    direction: Direction,
    buf: Vec<u8>,
    expected_len: Option<usize>,
    failed: Option<FrameError>,
}

impl Assembler {
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            buf: Vec::new(),
            expected_len: None,
            failed: None,
        }
    }

    pub fn retained(&self) -> usize {
        self.buf.len()
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.expected_len = None;
        self.failed = None;
    }

    pub fn feed(&mut self, chunk: Chunk) -> Result<Vec<Step>, FrameError> {
        if let Some(err) = self.failed {
            return Err(err);
        }

        match chunk {
            Chunk::Eof => {
                if self.buf.is_empty() && self.expected_len.is_none() {
                    Ok(vec![Step::CleanEof])
                } else if self.expected_len.is_none() {
                    Err(FrameError::TruncatedPrefix)
                } else {
                    Err(FrameError::TruncatedBody)
                }
            }
            Chunk::Data(data) => {
                if data.is_empty() {
                    return Ok(vec![Step::EmptyRead]);
                }

                let mut steps = Vec::new();
                let mut offset = 0;

                while offset < data.len() {
                    if let Some(err) = self.failed {
                        return Err(err);
                    }

                    // Reading 4-byte prefix
                    if self.expected_len.is_none() {
                        let needed = 4 - self.buf.len();
                        let available = data.len() - offset;
                        let take = available.min(needed);
                        self.buf.extend_from_slice(&data[offset..offset + take]);
                        offset += take;

                        if self.buf.len() == 4 {
                            let prefix_bytes: [u8; 4] = self.buf[0..4].try_into().unwrap();
                            let len = u32::from_ne_bytes(prefix_bytes) as usize;
                            if len == 0 {
                                self.failed = Some(FrameError::EmptyPayload);
                                return Err(FrameError::EmptyPayload);
                            }
                            if len > self.direction.socket_cap() {
                                self.failed = Some(FrameError::OverCap);
                                return Err(FrameError::OverCap);
                            }
                            self.expected_len = Some(len);
                            self.buf.clear();
                        }
                    }

                    // Reading payload body
                    if let Some(len) = self.expected_len {
                        let needed = len - self.buf.len();
                        let available = data.len() - offset;
                        let take = available.min(needed);
                        self.buf.extend_from_slice(&data[offset..offset + take]);
                        offset += take;

                        if self.buf.len() == len {
                            let msg = std::mem::take(&mut self.buf);
                            self.expected_len = None;
                            steps.push(Step::Message(msg));
                        }
                    }
                }

                if steps.is_empty() {
                    steps.push(Step::NeedMore);
                }

                Ok(steps)
            }
        }
    }
}

pub struct OutFrame {
    direction: Direction,
    buffer: Vec<u8>,
    cursor: usize,
}

impl OutFrame {
    pub fn from_payload(direction: Direction, payload: &[u8]) -> Result<Self, FrameError> {
        if payload.is_empty() {
            return Err(FrameError::EmptyPayload);
        }
        if payload.len() > direction.socket_cap() {
            return Err(FrameError::OverCap);
        }

        let len_prefix = (payload.len() as u32).to_ne_bytes();
        let mut buffer = Vec::with_capacity(4 + payload.len());
        buffer.extend_from_slice(&len_prefix);
        buffer.extend_from_slice(payload);

        Ok(Self {
            direction,
            buffer,
            cursor: 0,
        })
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn pending(&self) -> &[u8] {
        &self.buffer[self.cursor..]
    }

    pub fn write_with<F, E>(&mut self, mut write_fn: F) -> Result<usize, E>
    where
        F: FnMut(&[u8]) -> Result<usize, E>,
    {
        let pending = self.pending();
        if pending.is_empty() {
            return Ok(0);
        }
        let written = write_fn(pending)?.min(pending.len());
        self.cursor += written;
        Ok(written)
    }
}
