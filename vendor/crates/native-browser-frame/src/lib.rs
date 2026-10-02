// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod constants;
pub mod frame;
pub mod codec;
pub mod predicate;
pub mod register;

pub use constants::*;
pub use frame::{Assembler, Chunk, Direction, FrameError, OutFrame, Step};
pub use codec::{build_recipe, build_reply, canonical_stringify, decode, encode, DecodeError, DecodeOutcome};
pub use predicate::*;
pub use register::{render_registration, RegisterError, RegistrationRender};

