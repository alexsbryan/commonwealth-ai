// SPDX-License-Identifier: AGPL-3.0-or-later
//! A per-peer affinity preference: the multiplier an operator privately
//! applies to one peer, clamped to `(0.0, 1.0]` at construction.
//!
//! Moved here from `commonwealth_state::peer_preferences` by pb-mesh-exit-core
//! (FIVE_PROGRAMS §12 3a rung 2): it crosses cw-rails' ledger doors
//! (commonwealth-rails ledger.rs) and the daemon's
//! `/internal/peer-preference/*` routes, so two programs speak it. A wire leaf
//! reads no clock, so the constructor takes `set_at`;
//! `commonwealth_state::peer_preferences::peer_preference` is the
//! clock-reading wrapper its callers use.

use serde::{Deserialize, Serialize};

/// A single peer preference. Constructed via [`PeerPreference::new`]
/// which enforces the `(0.0, 1.0]` clamp; direct field
/// construction is impossible because the type is a struct with
/// private invariants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerPreference {
    multiplier: f64,
    reason: Option<String>,
    set_at: u64,
}

impl PeerPreference {
    /// Construct a preference. `multiplier` must lie in
    /// `(0.0, 1.0]` — values outside this range, NaN, and
    /// non-finite f64s are all rejected with `Err`. The error path
    /// is deliberately the *only* way to fail to set a preference;
    /// callers don't have to defensively re-validate elsewhere. `set_at` is
    /// the caller's wall clock, Unix seconds; the `Err` is the refusal's
    /// sentence.
    pub fn new(multiplier: f64, reason: Option<String>, set_at: u64) -> Result<Self, String> {
        if !multiplier.is_finite() {
            return Err(format!(
                "peer-preference multiplier must be finite, got {multiplier}"
            ));
        }
        if multiplier <= 0.0 || multiplier > 1.0 {
            return Err(format!(
                "peer-preference multiplier must be in (0.0, 1.0], got {multiplier}"
            ));
        }
        Ok(Self {
            multiplier,
            reason,
            set_at,
        })
    }

    pub fn multiplier(&self) -> f64 {
        self.multiplier
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn set_at(&self) -> u64 {
        self.set_at
    }
}
