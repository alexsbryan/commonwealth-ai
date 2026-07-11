// SPDX-License-Identifier: AGPL-3.0-or-later
//! Extension-hint usage registry (v0.3 §4.3): passive governance
//! observation of `x:*` hints on the wire.

use std::collections::HashMap;

use crate::capability::CapabilityHint;

// -----------------------------------------------------------------
// v0.3 §4.3 — Extension hint usage registry
//
// A passive observer that records which extension hints (`x:*`)
// appear on the wire. The registry is a governance input, not a
// routing input: the scheduler ignores it completely; a separate
// promotion process (v0.3 §4.3) reads the counts + first-seen /
// last-seen timestamps to decide which extensions have accumulated
// enough "measurable use over a meaningful time window" to merit
// promotion to the standardized set.
//
// Standardized hints (`general`, `code`) are ignored — they're
// already in the canonical set and governance has nothing to
// decide. Unknown-bare hints (no `x:` prefix, not standardized)
// are also skipped: those are most likely typos or
// future-standardized strings a newer peer knows about, neither of
// which are governance-track signals.
// -----------------------------------------------------------------

/// Aggregate statistics for a single observed extension hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStats {
    /// The hint as it appeared on the wire, including the `x:`
    /// prefix. Preserved verbatim so governance output shows the
    /// exact string the community uses.
    pub hint: String,
    /// Count of requests that asked for this hint. High values
    /// indicate consumer demand.
    pub requests_seen: u64,
    /// Count of advertised claims carrying this hint (across all
    /// peers observed by this scheduler). High values indicate
    /// provider adoption.
    pub advertisements_seen: u64,
    /// Unix timestamp (seconds since epoch) when this hint was
    /// first observed by this scheduler. `None` → not yet seen.
    pub first_seen_unix: u64,
    /// Unix timestamp (seconds since epoch) of the most recent
    /// observation. Combined with `first_seen_unix` it gives the
    /// "durability" signal the promotion process needs.
    pub last_seen_unix: u64,
}

/// Passive registry that accumulates [`ExtensionStats`] for every
/// extension hint observed on the wire. Owned by each scheduler
/// (not global): observations are local just like the per-node
/// tracker. Nothing in the scheduler consults this registry —
/// callers expose it via a diagnostic readout for operators and
/// the governance tooling.
///
/// Not thread-safe on its own; wrap in `RwLock` when shared.
#[derive(Debug, Default, Clone)]
pub struct ExtensionRegistry {
    entries: HashMap<String, ExtensionStats>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Observe an extension hint appearing on an outgoing request.
    /// Standardized and unknown-bare hints are silently ignored.
    pub fn observe_request(&mut self, hint: &CapabilityHint, now_unix: u64) {
        self.record(hint, now_unix, |stats| {
            stats.requests_seen = stats.requests_seen.saturating_add(1);
        });
    }

    /// Observe an extension hint appearing on an advertised claim
    /// (i.e., fetched in a peer's `ProviderManifest`).
    pub fn observe_advertisement(&mut self, hint: &CapabilityHint, now_unix: u64) {
        self.record(hint, now_unix, |stats| {
            stats.advertisements_seen = stats.advertisements_seen.saturating_add(1);
        });
    }

    fn record<F: FnOnce(&mut ExtensionStats)>(
        &mut self,
        hint: &CapabilityHint,
        now_unix: u64,
        bump: F,
    ) {
        if !hint.is_extension() {
            return;
        }
        let entry = self
            .entries
            .entry(hint.as_str().to_string())
            .or_insert_with(|| ExtensionStats {
                hint: hint.as_str().to_string(),
                requests_seen: 0,
                advertisements_seen: 0,
                first_seen_unix: now_unix,
                last_seen_unix: now_unix,
            });
        entry.last_seen_unix = now_unix;
        bump(entry);
    }

    /// Snapshot of every tracked hint. Ordering is insertion-order;
    /// callers that want canonical ordering should sort on the
    /// fields they care about (e.g., `requests_seen + advertisements_seen`
    /// for a popularity ranking).
    pub fn stats(&self) -> impl Iterator<Item = &ExtensionStats> {
        self.entries.values()
    }

    /// Look up a single hint by its wire form (with `x:` prefix).
    pub fn get(&self, hint: &str) -> Option<&ExtensionStats> {
        self.entries.get(hint)
    }

    /// Number of distinct extension hints the registry has seen.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ───── v0.3 §4.3 — Extension registry ──────────────────

    #[test]
    fn extension_registry_records_extension_on_first_observation() {
        let mut reg = ExtensionRegistry::new();
        let hint = CapabilityHint::extension("prose").unwrap();
        reg.observe_request(&hint, 1_000);
        let stats = reg.get("x:prose").expect("must be recorded");
        assert_eq!(stats.requests_seen, 1);
        assert_eq!(stats.advertisements_seen, 0);
        assert_eq!(stats.first_seen_unix, 1_000);
        assert_eq!(stats.last_seen_unix, 1_000);
    }

    #[test]
    fn extension_registry_accumulates_counts_and_updates_last_seen() {
        let mut reg = ExtensionRegistry::new();
        let hint = CapabilityHint::extension("prose").unwrap();
        reg.observe_request(&hint, 1_000);
        reg.observe_advertisement(&hint, 1_500);
        reg.observe_request(&hint, 2_000);
        let stats = reg.get("x:prose").unwrap();
        assert_eq!(stats.requests_seen, 2);
        assert_eq!(stats.advertisements_seen, 1);
        // first_seen stays pinned at the earliest observation; last_seen
        // advances monotonically.
        assert_eq!(stats.first_seen_unix, 1_000);
        assert_eq!(stats.last_seen_unix, 2_000);
    }

    #[test]
    fn extension_registry_ignores_standardized_hints() {
        let mut reg = ExtensionRegistry::new();
        reg.observe_request(&CapabilityHint::general(), 1_000);
        reg.observe_request(&CapabilityHint::code(), 2_000);
        reg.observe_advertisement(&CapabilityHint::code(), 3_000);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn extension_registry_ignores_unknown_bare_hints() {
        // A bare unrecognised string (e.g., "math" before any
        // hypothetical future promotion) is forward-compatibility
        // data, not a governance signal — skip it.
        let mut reg = ExtensionRegistry::new();
        let future = CapabilityHint::parse("math").unwrap();
        assert!(future.is_unknown_bare());
        reg.observe_request(&future, 1_000);
        reg.observe_advertisement(&future, 2_000);
        assert!(reg.is_empty());
    }

    #[test]
    fn extension_registry_tracks_multiple_hints_independently() {
        let mut reg = ExtensionRegistry::new();
        let prose = CapabilityHint::extension("prose").unwrap();
        let biomed = CapabilityHint::extension("biomed").unwrap();
        reg.observe_request(&prose, 1_000);
        reg.observe_advertisement(&biomed, 1_500);
        reg.observe_request(&prose, 2_000);
        assert_eq!(reg.len(), 2);
        let prose_stats = reg.get("x:prose").unwrap();
        assert_eq!(prose_stats.requests_seen, 2);
        assert_eq!(prose_stats.advertisements_seen, 0);
        let biomed_stats = reg.get("x:biomed").unwrap();
        assert_eq!(biomed_stats.requests_seen, 0);
        assert_eq!(biomed_stats.advertisements_seen, 1);
    }
}
