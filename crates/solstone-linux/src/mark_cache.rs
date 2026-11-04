// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! A mark is one argon2id run at 64 MiB, and a jid's mark never changes, so this
//! process derives each jid's once. The window reads its state every two seconds,
//! and a read has to stay cheap.

use spl_core::mark::{MarkRenderSpec, mark_from_jid};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

/// A peer can present a new CA on every dial, so what it can make this keep is bounded.
const MAX_MARKS: usize = 64;

#[derive(Default)]
struct Cache {
    marks: HashMap<String, MarkRenderSpec>,
    #[cfg(test)]
    derivations: HashMap<String, usize>,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

/// The mark for `jid`, or `None` if `jid` can't give one. A failure isn't kept, so it
/// is tried again next time.
pub(crate) fn render_spec(jid: &str) -> Option<MarkRenderSpec> {
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mark) = cache.marks.get(jid) {
        return Some(mark.clone());
    }
    #[cfg(test)]
    {
        *cache.derivations.entry(jid.to_owned()).or_default() += 1;
    }
    let mark = mark_from_jid(jid).ok()?.to_render_spec();
    // Tests count derivations across the whole process, so they keep everything.
    if !cfg!(test) && cache.marks.len() >= MAX_MARKS {
        cache.marks.clear();
    }
    cache.marks.insert(jid.to_owned(), mark.clone());
    Some(mark)
}

/// How many times this process has run the derivation for `jid`.
#[cfg(test)]
pub(crate) fn derivations(jid: &str) -> usize {
    CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .derivations
        .get(jid)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_read_does_not_derive() {
        let jid = "d0b9cf9d-6318-894d-8688-cf3060a24f49";
        let first = render_spec(jid).unwrap();
        assert_eq!(derivations(jid), 1);
        assert_eq!(render_spec(jid), Some(first));
        assert_eq!(derivations(jid), 1);
    }

    #[test]
    fn a_failure_is_not_kept() {
        let not_a_jid = "not-a-jid";
        assert_eq!(render_spec(not_a_jid), None);
        assert_eq!(render_spec(not_a_jid), None);
        assert_eq!(derivations(not_a_jid), 2);
    }
}
