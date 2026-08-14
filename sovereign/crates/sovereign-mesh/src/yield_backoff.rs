//! Short-lived memory of peers that just refused with `yielded_to_local`.
//!
//! ## Why this exists
//!
//! A peer whose local user is at the keyboard refuses every peer hop with
//! `503 {"reason":"yielded_to_local","retry_after_secs":N}`. That policy is
//! correct — a node's own user comes first. The waste is on the *asking*
//! side: measured at N=2 on 2026-08-14, this node selected the peer on 421
//! of 672 dispatches and was refused all 421 times, paying a round-trip per
//! turn to be told the same thing (note 3234d770).
//!
//! The peer's gossiped `inference_availability` is the authoritative signal
//! and now carries the yield state (see
//! `AppState::recompute_local_availability`), but it arrives on a gossip
//! round — up to ~10s late, and only from peers running a build that
//! publishes it. This store is the same fact arriving immediately, from a
//! source that cannot be stale: the peer just said so, in a response we
//! hold.
//!
//! ## Why exclusion rather than a score discount
//!
//! The order that motivated this asked for the peer to be "scored down for
//! the `retry_after_secs` window". Scoring down cannot express it: the SSOT
//! scorer clamps availability to `[0.2, 1.0]`
//! (`oicp-types/src/scoring.rs:553`), so the strongest discount the score
//! path can apply is a 5× multiplier — a peer that is 5× better on every
//! other term still wins, and still gets refused. "Do not re-dial into the
//! same refusal" is an exclusion, so it is modelled as one:
//! [`ExclusionReason::YieldedToLocal`]. Excluding also skips the manifest
//! fetch, which is what makes the refusal cost *nothing* rather than a
//! cheaper something.
//!
//! [`ExclusionReason::YieldedToLocal`]: crate::decision_log::ExclusionReason::YieldedToLocal
//!
//! ## Why it is not quarantine
//!
//! `PeerHealthTracker` quarantines on consecutive *failures* and a
//! quarantine is punitive and cumulative. A refusal is neither — see
//! `PeerInferenceEngine::book_peer_failure`, which already exempts sheds
//! from health accounting for exactly this reason. This store books
//! nothing, expires on the deadline the peer itself named, and is cleared
//! outright by any successful turn.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Hard ceiling on how long one refusal may bench a peer, whatever it
/// asked for. A peer that reports a wild `retry_after_secs` (or a build
/// with a different unit) must not be able to remove itself from the mesh
/// for an unbounded time — the failure mode we are fixing is a peer being
/// selected forever, and its mirror image is a peer being skipped forever.
pub const MAX_BACKOFF_SECS: u64 = 60;

/// Cap on distinct peers held at once. Bounded memory, per the order: a
/// mesh this size has tens of peers, and the map is pruned on every write,
/// so this only ever bites under a bug or an adversarial peer set.
const MAX_ENTRIES: usize = 512;

/// Peers that recently answered `yielded_to_local`, and when their stated
/// retry window ends. Keyed by peer NAME — the same key
/// `PeerHealthTracker` and the observation maps use, so a peer is one
/// subject across all three.
#[derive(Debug, Default)]
pub struct YieldBackoff {
    until: Mutex<HashMap<String, Instant>>,
}

impl YieldBackoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `peer` refused because its local user is active, and
    /// asked us to wait `retry_after_secs`.
    ///
    /// Clamped to [`MAX_BACKOFF_SECS`]. Prunes expired entries on the way
    /// through, which is what keeps the map bounded without a sweeper task.
    pub fn record_refusal(&self, peer: &str, retry_after_secs: u64) {
        let secs = retry_after_secs.clamp(1, MAX_BACKOFF_SECS);
        let deadline = Instant::now() + Duration::from_secs(secs);
        let Ok(mut map) = self.until.lock() else {
            return;
        };
        let now = Instant::now();
        map.retain(|_, until| *until > now);
        // Only ever extend: two refusals in flight concurrently should
        // leave the LATER deadline standing, not whichever landed last.
        let entry = map.entry(peer.to_string()).or_insert(deadline);
        if deadline > *entry {
            *entry = deadline;
        }
        if map.len() > MAX_ENTRIES {
            // Pathological only. Drop the soonest-expiring entries first:
            // they are the ones closest to being irrelevant anyway.
            let mut by_deadline: Vec<(String, Instant)> =
                map.iter().map(|(k, v)| (k.clone(), *v)).collect();
            by_deadline.sort_by_key(|(_, until)| *until);
            for (name, _) in by_deadline.iter().take(map.len() - MAX_ENTRIES) {
                map.remove(name);
            }
        }
        tracing::info!(
            target: "mesh.decision",
            peer,
            retry_after_secs = secs,
            "peer YIELDED to its local user — excluded from candidacy until its \
             stated retry window elapses; the next turn will not re-dial it"
        );
    }

    /// Seconds remaining in `peer`'s yield window, or `None` when it is a
    /// normal candidate. `Some(_)` means "exclude, and do not fetch its
    /// manifest".
    pub fn secs_remaining(&self, peer: &str) -> Option<u64> {
        let map = self.until.lock().ok()?;
        let until = map.get(peer)?;
        let now = Instant::now();
        if *until <= now {
            return None;
        }
        // Round UP, so a live window never reports 0 and reads as clear.
        Some(
            until
                .saturating_duration_since(now)
                .as_secs()
                .saturating_add(1),
        )
    }

    /// Forget any backoff for `peer`. Called when a peer actually serves:
    /// its user evidently stepped away, and the deadline it named earlier
    /// is now known to be wrong.
    pub fn clear(&self, peer: &str) {
        if let Ok(mut map) = self.until.lock() {
            if map.remove(peer).is_some() {
                tracing::debug!(
                    target: "mesh.decision",
                    peer,
                    "yield backoff cleared — peer served a turn"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_store_excludes_nobody() {
        let b = YieldBackoff::new();
        assert_eq!(b.secs_remaining("mac"), None);
    }

    #[test]
    fn a_refusal_excludes_that_peer_only() {
        let b = YieldBackoff::new();
        b.record_refusal("mac", 14);
        assert!(b.secs_remaining("mac").is_some());
        assert_eq!(b.secs_remaining("fedora"), None);
    }

    #[test]
    fn an_elapsed_window_clears_itself() {
        let b = YieldBackoff::new();
        // Deadline in the past: 1s clamp floor, then rewind the clock by
        // writing the entry directly — Instant cannot be constructed, so
        // this is the honest way to test expiry without sleeping.
        b.record_refusal("mac", 1);
        {
            let mut map = b.until.lock().unwrap();
            let past = Instant::now() - Duration::from_secs(1);
            map.insert("mac".into(), past);
        }
        assert_eq!(
            b.secs_remaining("mac"),
            None,
            "an expired window must not keep excluding the peer"
        );
    }

    #[test]
    fn a_served_turn_clears_the_backoff() {
        let b = YieldBackoff::new();
        b.record_refusal("mac", 30);
        assert!(b.secs_remaining("mac").is_some());
        b.clear("mac");
        assert_eq!(b.secs_remaining("mac"), None);
    }

    #[test]
    fn backoff_is_capped_however_long_the_peer_asks() {
        let b = YieldBackoff::new();
        b.record_refusal("mac", 86_400);
        let secs = b.secs_remaining("mac").expect("still backed off");
        assert!(
            secs <= MAX_BACKOFF_SECS + 1,
            "a peer asked for a day and got {secs}s — the cap did not hold"
        );
    }

    #[test]
    fn concurrent_refusals_keep_the_later_deadline() {
        let b = YieldBackoff::new();
        b.record_refusal("mac", 30);
        b.record_refusal("mac", 2);
        let secs = b.secs_remaining("mac").expect("still backed off");
        assert!(
            secs > 2,
            "a shorter second refusal shortened the window to {secs}s"
        );
    }

    #[test]
    fn expired_entries_are_pruned_on_write() {
        let b = YieldBackoff::new();
        for i in 0..10 {
            b.record_refusal(&format!("peer{i}"), 1);
        }
        {
            let mut map = b.until.lock().unwrap();
            let past = Instant::now() - Duration::from_secs(1);
            for i in 0..10 {
                map.insert(format!("peer{i}"), past);
            }
        }
        b.record_refusal("fresh", 10);
        let map = b.until.lock().unwrap();
        assert_eq!(
            map.len(),
            1,
            "expired entries survived a write — the map is not bounded"
        );
    }
}
