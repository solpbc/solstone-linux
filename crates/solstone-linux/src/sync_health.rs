// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorType {
    Auth,
    Client,
    Transient,
    Incompatible,
}
