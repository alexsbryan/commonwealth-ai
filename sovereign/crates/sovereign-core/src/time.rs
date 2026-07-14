// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wall-clock helpers — the canonical "seconds/millis since the Unix epoch"
//! functions.
//!
//! These existed as ~40 copy-pasted private `fn unix_now()` / `now_secs()` /
//! `now_unix()` / `now_millis()` across the workspace, all computing the same
//! `SystemTime::now().duration_since(UNIX_EPOCH)` and differing only in cast and
//! error handling. Consolidated here so there is one implementation to reason
//! about. Each returns 0 if the system clock is before the Unix epoch (an
//! impossible-in-practice case the old copies handled inconsistently — some
//! `unwrap_or(0)`, some `.expect(...)`; 0 is the sane, non-panicking choice).
//!
//! Pick by the type your caller needs:
//!   - [`unix_now`]      — seconds, `i64` (timestamps stored as signed)
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
