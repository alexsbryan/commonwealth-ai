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
//!   - [`system_now`]    — the raw `SystemTime`, for APIs that take one

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

/// The current wall-clock instant, for an API whose parameter IS a
/// `SystemTime` — `kernel_types::Judgement::as_of` is the one this was minted
/// for. Added 2026-09-07: without it the decider had no door for that shape,
/// so four call sites read the clock by hand and `clock-gate` was red on
/// `main`. A decider that cannot answer a legitimate question is why people
/// go around it (ARCH §10.6).
#[inline]
#[must_use]
pub fn system_now() -> SystemTime {
    SystemTime::now()
}

/// Milliseconds since the Unix epoch as `u64`. 0 if the clock is pre-epoch.
#[inline]
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_agree_on_the_same_instant() {
        // A recent lower bound: 2025-01-01T00:00:00Z. The clock is well past it,
        // so every helper must exceed it (in its own unit) — cheap sanity that
        // none returns the pre-epoch 0 fallback and the units are right.
        const Y2025_SECS: i64 = 1_735_689_600;
        assert!(unix_now() > Y2025_SECS);
        assert!(unix_now_u64() > Y2025_SECS as u64);
        assert!(unix_millis() > Y2025_SECS as u64 * 1000);
        // Seconds views agree; millis is ~1000× the seconds view.
        assert_eq!(unix_now() as u64, unix_now_u64());
        assert!(unix_millis() / 1000 >= unix_now_u64() - 1);
    }
}
