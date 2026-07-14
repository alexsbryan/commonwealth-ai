// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wall-clock helpers — the canonical "seconds/millis since the Unix epoch"
//! functions for the corpus-engine dependency subtree.
//!
//! This is the corpus-engine-side twin of `sovereign_core::time`. The two exist
//! because `sovereign-core` depends on `corpus-engine`, so the corpus-engine
//! crates cannot import `sovereign_core` (that would be a dependency cycle).
//! Rather than let ~12 private `fn unix_now()` / `now_secs()` / `now_unix()`
//! copies proliferate across `corpus-engine`, `-notes`, `-atos`, and
//! `-watchers`, they all share this one — this crate is the lowest leaf every
//! one of them can (or already does) depend on. So the workspace duplicates this
//! trivial logic exactly once per dependency island, not once per file.
//!
//! Each returns 0 if the system clock is before the Unix epoch (an
//! impossible-in-practice case the old copies handled inconsistently). Pick by
//! the type your caller needs:
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
