// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wall-clock helpers — the canonical "seconds/millis since the Unix epoch"
//! functions for sovereign-side crates that do NOT depend on `sovereign-core`.
//!
//! This is a Tier-0 leaf (zero dependencies) so light app crates —
//! `sovereign-pipeline`, `sovereign-eval`, `sovereign-meshapp` — can share one
//! implementation without pulling the heavy `sovereign-core` (which carries an
//! identical `sovereign_core::time`). Crates already on `sovereign-core` use
//! that twin; the corpus-engine subtree uses `corpus_engine_yield::time`; the
//! commonwealth subtree uses `commonwealth_core::clock`. Each dependency island
//! duplicates this trivial logic exactly once, because the islands cannot import
//! across each other's boundaries without dependency cycles.
//!
//! Each returns 0 if the system clock is before the Unix epoch. Pick by type:
//!   - [`unix_now`]      — seconds, `i64`
//!   - [`unix_now_u64`]  — seconds, `u64`
//!   - [`unix_millis`]   — milliseconds, `u64`

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch as `i64`. 0 if the clock is pre-epoch.
#[inline]
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Seconds since the Unix epoch as `u64`. 0 if the clock is pre-epoch.
#[inline]
pub fn unix_now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Milliseconds since the Unix epoch as `u64`. 0 if the clock is pre-epoch.
#[inline]
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
