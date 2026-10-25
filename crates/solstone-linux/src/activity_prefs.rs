// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ActivityPolicy {
    pub activity_source: bool,
    pub activity_titles: bool,
    pub activity_urls: bool,
    pub activity_exclude_apps: Vec<String>,
    pub activity_exclude_titles: Vec<String>,
    #[serde(default)]
    pub activity_through_pause: bool,
}

impl ActivityPolicy {
    pub fn from_config(config: &Config) -> Self {
        Self {
            activity_source: config.activity_source,
            activity_titles: config.activity_titles,
            activity_urls: config.activity_urls,
            activity_exclude_apps: config.activity_exclude_apps.clone(),
            activity_exclude_titles: config.activity_exclude_titles.clone(),
            activity_through_pause: config.activity_through_pause,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityPrefsSnapshot {
    pub source: bool,
    pub titles: bool,
    pub urls: bool,
    pub exclude_apps: Vec<String>,
    pub exclude_titles: Vec<String>,
    /// Desktop activity keeps going through the owner's pause.
    pub through_pause: bool,
    pub generation: u64,
    pub policy_boundary_ms: Option<u64>,
}

pub struct ActivityPrefs {
    state: RwLock<ActivityPrefsSnapshot>,
    generation: AtomicU64,
}

impl ActivityPrefs {
    pub fn new(config: &Config) -> Self {
        let initial_gen = 1;
        let snapshot = ActivityPrefsSnapshot {
            source: config.activity_source,
            titles: config.activity_titles,
            urls: config.activity_urls,
            exclude_apps: config.activity_exclude_apps.clone(),
            exclude_titles: config.activity_exclude_titles.clone(),
            through_pause: config.activity_through_pause,
            generation: initial_gen,
            policy_boundary_ms: None,
        };
        Self {
            state: RwLock::new(snapshot),
            generation: AtomicU64::new(initial_gen),
        }
    }

    pub fn snapshot(&self) -> ActivityPrefsSnapshot {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        lock.clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// The owner chose to keep desktop activity running through the main pause.
    /// It does nothing while the source itself is off.
    pub fn runs_through_pause(&self) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        lock.source && lock.through_pause
    }

    pub fn apply_policy(&self, policy: ActivityPolicy) -> u64 {
        self.apply_policy_with_clock(policy, crate::activity_helpers::current_unix_ms)
    }

    pub fn apply_policy_at(&self, policy: ActivityPolicy, now_ms: u64) -> u64 {
        self.apply_policy_with_clock(policy, || now_ms)
    }

    fn apply_policy_with_clock(&self, policy: ActivityPolicy, clock: impl FnOnce() -> u64) -> u64 {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        let now_ms = clock();
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        lock.source = policy.activity_source;
        lock.titles = policy.activity_titles;
        lock.urls = policy.activity_urls;
        lock.exclude_apps = policy.activity_exclude_apps;
        lock.exclude_titles = policy.activity_exclude_titles;
        lock.through_pause = policy.activity_through_pause;
        lock.generation = next;
        lock.policy_boundary_ms = Some(
            lock.policy_boundary_ms
                .map_or(now_ms, |old| old.min(now_ms)),
        );
        next
    }

    pub fn take_policy_snapshot(
        &self,
        clock: impl FnOnce() -> u64,
    ) -> (ActivityPrefsSnapshot, u64) {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        let now_ms = clock();
        let snapshot = lock.clone();
        lock.policy_boundary_ms = None;
        (snapshot, now_ms)
    }

    /// Runtime stop only: the owner's saved enable choice remains unchanged.
    pub fn stop_intake_at(&self, now_ms: u64) {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        lock.source = false;
        lock.policy_boundary_ms = Some(
            lock.policy_boundary_ms
                .map_or(now_ms, |old| old.min(now_ms)),
        );
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        lock.generation = next;
    }

    /// A segment boundary earlier than the policy change must leave it pending.
    pub fn take_policy_boundary_before(&self, now_ms: u64) -> Option<u64> {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        if lock.policy_boundary_ms.is_some_and(|at| at <= now_ms) {
            lock.policy_boundary_ms.take()
        } else {
            None
        }
    }

    pub fn is_app_excluded(&self, app: &str, app_id: Option<&str>) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        for excluded in &lock.exclude_apps {
            if excluded == app || app_id.is_some_and(|id| excluded == id) {
                return true;
            }
        }
        false
    }

    pub fn is_title_excluded(&self, title: &str) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        for excluded in &lock.exclude_titles {
            if excluded == title {
                return true;
            }
        }
        false
    }

    pub fn has_title_exclusions(&self) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        !lock.exclude_titles.is_empty()
    }

    /// Drops outstanding reads. The next sample starts a new interval.
    pub fn bump_generation(&self) -> u64 {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        lock.generation = next;
        next
    }
}
