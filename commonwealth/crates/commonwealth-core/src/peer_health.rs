//! Per-peer health tracking with automatic quarantine + recovery.
//!
//! When a peer fails repeatedly, we don't want the scheduler to keep
//! routing real work to it. Each failed attempt costs the wall-clock
//! time the inference would have taken — for Phase 1 enrichment that
//! can be 1-2 minutes per attempt — and stalls the queue. The
//! `PeerHealthTracker` watches consecutive failures per peer, marks a
//! peer "quarantined" after a small threshold, and skips it during
//! routing for a cooldown window. After the cooldown elapses, the
//! peer is tried again on the next request — success resets the
//! counter, failure re-quarantines with a longer cooldown.
//!
//! Design choices:
//!
//! - **Per-peer, not per-(peer, model)**. A peer that's hung on one
//!   model is usually hung on its slot scheduler generally; we'd
//!   rather be conservative and skip it for everything than serve
//!   degraded responses for "different" models that share the same
//!   broken slot machinery. Operators with a peer that's good at
//!   model A but bad at model B can configure model affinities at a
//!   different layer.
//!
//! - **Consecutive failures, not failure rate**. A peer with one
//!   bad day shouldn't be permanently scored down by an EWMA. The
//!   reset-on-success policy means a peer that recovers is fully
//!   trusted again on the next successful attempt.
//!
//! - **Linear backoff to cap, then steady**. First quarantine 60s,
//!   each re-quarantine adds another 60s up to a 10-minute cap.
//!   Long enough that a flapping peer doesn't churn the scheduler;
//!   short enough that genuine recovery surfaces quickly.
//!
//! - **Tracing on transitions**. `peer-health: quarantining peer`
//!   and `peer-health: peer recovered` are the two log lines the
//!   operator sees; both at INFO so they show up in normal daemon
//!   output without DEBUG flags.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many consecutive failures triggers quarantine. Three is a
/// balance: one or two could be a transient blip (network jitter,
/// peer mid-restart); four would let too many requests waste time
/// on a clearly-broken peer.
const FAILURE_THRESHOLD: u32 = 3;

/// First-quarantine duration. Subsequent re-quarantines add another
/// `INITIAL_COOLDOWN` per occurrence up to `MAX_COOLDOWN`.
const INITIAL_COOLDOWN: Duration = Duration::from_secs(60);

/// Cap on quarantine duration. Even a peer that fails every retry
/// gets re-tried at this cadence so genuine recovery isn't
/// indefinitely masked.
const MAX_COOLDOWN: Duration = Duration::from_secs(600);

/// Recent-event ring buffer capacity per peer. The "soft health"
/// signal — distinct from the hard quarantine threshold — is the
/// success fraction over the last `RECENT_EVENT_WINDOW` events.
/// 20 is small enough that one isolated failure doesn't dominate
/// the score (1/20 = 5% drop, recoverable in a few requests), and
/// large enough that real degradation (>30% failure rate)
/// surfaces within seconds.
///
/// The empirical anchor is the 2026-05-11 SEP fanout incident:
/// a thrashing Q4 pod had ~6 successes per 100 attempts in a
/// 5-minute window. The hard quarantine took until the 3rd
/// CONSECUTIVE failure to engage, but the success rate was
/// continuously visible from the first half-dozen events —
/// soft health would have pushed routing away from the peer
/// almost immediately, where quarantine cycled in and out and
/// the load balancer kept stampeding back to the bad peer.
const RECENT_EVENT_WINDOW: usize = 20;

/// Soft-health floor. Returned by `health_weight` even when every
/// recent event is a failure — keeps the peer marginally routable
/// so it has SOME chance to demonstrate recovery. Quarantine takes
/// over the hard "skip" when this many failures in a row land.
const HEALTH_WEIGHT_FLOOR: f32 = 0.05;

#[derive(Debug, Default, Clone)]
struct PeerHealth {
    /// Failures since the last success. Reset to 0 on
    /// `record_success`. Crosses `FAILURE_THRESHOLD` to enter
    /// quarantine.
    consecutive_failures: u32,
    /// Number of times this peer has been quarantined since the
    /// process started. Drives the linear backoff: cooldown =
    /// `INITIAL_COOLDOWN * quarantine_count` capped at `MAX_COOLDOWN`.
    quarantine_count: u32,
    /// If `Some(t)` and `t > Instant::now()`, this peer is currently
    /// quarantined. `None` once the cooldown elapses; the next
    /// attempt is allowed and either resets (success) or re-arms
    /// quarantine with a longer cooldown (failure).
    quarantined_until: Option<Instant>,
    /// Ring buffer of recent outcomes (true = success, false =
    /// failure), capped at `RECENT_EVENT_WINDOW`. Drives the
    /// `health_weight` soft signal used by the load balancer to
    /// bias routing AWAY from struggling peers before they cross
    /// the hard quarantine threshold.
    ///
    /// Pushed on every `record_success`/`record_failure`. Cleared
    /// when a peer transitions out of quarantine via success —
    /// the new event series starts fresh so a sustained-recovery
    /// peer regains full weight quickly.
    recent_events: VecDeque<bool>,
}

impl PeerHealth {
    fn is_quarantined_at(&self, now: Instant) -> bool {
        match self.quarantined_until {
            Some(deadline) => deadline > now,
            None => false,
        }
    }

    /// Compute success rate over the recent-event ring buffer.
    /// Returns 1.0 when the buffer is empty (no data → optimistic;
    /// a never-tried peer gets a full chance to prove itself).
    fn recent_success_rate(&self) -> f32 {
        if self.recent_events.is_empty() {
            return 1.0;
        }
        let successes = self.recent_events.iter().filter(|&&ok| ok).count();
        successes as f32 / self.recent_events.len() as f32
    }

    /// Push an outcome onto the ring buffer, evicting the oldest
    /// entry if at capacity. Constant-time amortised.
    fn push_event(&mut self, success: bool) {
        if self.recent_events.len() >= RECENT_EVENT_WINDOW {
            self.recent_events.pop_front();
        }
        self.recent_events.push_back(success);
    }
}

/// Shared health-state for the routing layer. Cheap to clone — the
/// underlying lock is shared.
#[derive(Debug, Default)]
pub struct PeerHealthTracker {
    peers: Mutex<HashMap<String, PeerHealth>>,
}

impl PeerHealthTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// True iff this peer is currently quarantined. Used by
    /// `locate_named_model` and `select_peer` to drop the peer from
    /// the candidate set before issuing the request.
    pub fn is_quarantined(&self, peer_name: &str) -> bool {
        let now = Instant::now();
        let map = self.peers.lock().unwrap();
        map.get(peer_name)
            .map(|h| h.is_quarantined_at(now))
            .unwrap_or(false)
    }

    /// Soft-health weight in `[HEALTH_WEIGHT_FLOOR, 1.0]`. This is
    /// the success rate over the last `RECENT_EVENT_WINDOW` events,
    /// floored so a struggling peer stays marginally routable
    /// (quarantine handles the hard "skip" case separately).
    ///
    /// **Use case**: the load balancer multiplies `peer_inflight`
    /// by `1.0 / health_weight` when comparing against `local_inflight`.
    /// A peer at 30% success rate appears to the balancer as having
    /// 3.3× its actual in-flight count, pushing new requests toward
    /// the healthier peer (or local) without removing the struggling
    /// peer from the candidate set entirely.
    ///
    /// **Why a floor**: a peer that's recovered after a brief outage
    /// shouldn't stay banned by its own historical failures. The
    /// `HEALTH_WEIGHT_FLOOR` (5%) gives every advertised peer at
    /// least a small dispatch probability per round, letting it
    /// re-prove itself organically as `recent_events` cycles fresh
    /// successes through.
    ///
    /// **Empty buffer = 1.0** — a never-tried peer is optimistically
    /// treated as fully healthy. The first failure pushes weight to
    /// ~0% (1/1 = 0% success), the next success to 50%, etc., so
    /// the signal converges in handfuls of events.
    pub fn health_weight(&self, peer_name: &str) -> f32 {
        let map = self.peers.lock().unwrap();
        map.get(peer_name)
            .map(|h| h.recent_success_rate().max(HEALTH_WEIGHT_FLOOR))
            .unwrap_or(1.0)
    }

    /// Mark a successful interaction with the peer. Resets the
    /// consecutive-failure counter and clears any quarantine flag —
    /// success is full vindication. The `quarantine_count` is
    /// preserved so a peer that has flapped multiple times still
    /// gets a longer next-cooldown if it fails again.
    pub fn record_success(&self, peer_name: &str) {
        let mut map = self.peers.lock().unwrap();
        let entry = map.entry(peer_name.to_string()).or_default();
        let was_quarantined = entry.quarantined_until.is_some();
        entry.consecutive_failures = 0;
        entry.quarantined_until = None;
        // Clear the soft-health ring buffer on recovery-from-quarantine
        // so the peer's effective weight jumps back to 1.0 immediately
        // after the first post-cooldown success. Without the wipe, a
        // peer with 19 failures in its buffer would need 19 more
        // successes to fully recover — too slow for fanout workloads
        // where the bad-peer window was brief.
        if was_quarantined {
            entry.recent_events.clear();
            tracing::info!(
                peer = %peer_name,
                "peer-health: peer recovered after success"
            );
        }
        entry.push_event(true);
    }

    /// Mark a failed interaction with the peer. Returns `true` iff
    /// this failure triggered (or extended) quarantine — useful for
    /// operator-visible logging at the call site.
    pub fn record_failure(&self, peer_name: &str) -> bool {
        let now = Instant::now();
        let mut map = self.peers.lock().unwrap();
        let entry = map.entry(peer_name.to_string()).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.push_event(false);

        if entry.consecutive_failures < FAILURE_THRESHOLD {
            return false;
        }

        // Already quarantined — don't compound. Failures during an
        // active quarantine window are expected (in-flight calls
        // draining after a peer restart, concurrent extracts all
        // discovering the same outage at once). The existing
        // deadline already covers this incident; bumping
        // `quarantine_count` and extending the cooldown for every
        // additional failure turns a 60 s "peer is briefly down"
        // event into a 4–10 min ban after a burst of 6+ concurrent
        // failures — and a fanout client retrying through the
        // cooldown then keeps re-extending it. See the 2026-05-11
        // SEP fanout incident where a model-slot reload on a single
        // pod cascaded into the whole fanout queue failing
        // instantly: the cache still had the right manifest, but
        // `is_quarantined` rejected the peer for ~4 min instead of
        // the expected 60 s.
        if entry.quarantined_until.is_some_and(|d| d > now) {
            return false;
        }

        // Transitioning INTO quarantine (either first time, or
        // re-arming after a previous cooldown expired). Bump count
        // and set deadline; backoff is linear in `quarantine_count`
        // — first time 60 s, second 120 s, etc., capped at
        // `MAX_COOLDOWN`.
        entry.quarantine_count = entry.quarantine_count.saturating_add(1);
        let cooldown = std::cmp::min(
            INITIAL_COOLDOWN.saturating_mul(entry.quarantine_count),
            MAX_COOLDOWN,
        );
        let deadline = now + cooldown;
        entry.quarantined_until = Some(deadline);

        tracing::warn!(
            peer = %peer_name,
            consecutive_failures = entry.consecutive_failures,
            quarantine_count = entry.quarantine_count,
            cooldown_secs = cooldown.as_secs(),
            "peer-health: quarantining peer"
        );
        true
    }

    /// Snapshot for diagnostics / admin endpoints. Returns
    /// `(peer_name, is_quarantined, consecutive_failures, seconds_until_recovery)`
    /// rows for every peer the tracker has observed.
    pub fn snapshot(&self) -> Vec<(String, bool, u32, u64)> {
        let now = Instant::now();
        let map = self.peers.lock().unwrap();
        map.iter()
            .map(|(name, h)| {
                let q = h.is_quarantined_at(now);
                let secs_until = h
                    .quarantined_until
                    .filter(|d| *d > now)
                    .map(|d| (d - now).as_secs())
                    .unwrap_or(0);
                (name.clone(), q, h.consecutive_failures, secs_until)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_quarantine_below_threshold() {
        let h = PeerHealthTracker::new();
        h.record_failure("mac");
        h.record_failure("mac");
        assert!(!h.is_quarantined("mac"));
    }

    #[test]
    fn quarantine_triggers_at_threshold() {
        let h = PeerHealthTracker::new();
        for _ in 0..FAILURE_THRESHOLD {
            h.record_failure("mac");
        }
        assert!(h.is_quarantined("mac"));
    }

    #[test]
    fn success_resets_counter_and_clears_quarantine() {
        let h = PeerHealthTracker::new();
        for _ in 0..FAILURE_THRESHOLD {
            h.record_failure("mac");
        }
        assert!(h.is_quarantined("mac"));
        h.record_success("mac");
        assert!(!h.is_quarantined("mac"));
        // After a single success the counter is fully reset, so the
        // next two failures alone don't re-quarantine.
        h.record_failure("mac");
        h.record_failure("mac");
        assert!(!h.is_quarantined("mac"));
    }

    #[test]
    fn returns_true_on_quarantine_trigger() {
        let h = PeerHealthTracker::new();
        assert!(!h.record_failure("mac"));
        assert!(!h.record_failure("mac"));
        assert!(h.record_failure("mac"), "third failure triggers quarantine");
    }

    #[test]
    fn unrelated_peers_dont_share_state() {
        let h = PeerHealthTracker::new();
        for _ in 0..FAILURE_THRESHOLD {
            h.record_failure("mac");
        }
        assert!(h.is_quarantined("mac"));
        assert!(!h.is_quarantined("laptop"));
    }

    #[test]
    fn burst_of_failures_during_quarantine_does_not_compound_cooldown() {
        // Regression for the 2026-05-11 SEP fanout incident: a single
        // peer outage that triggered 6 concurrent failures used to
        // re-quarantine 4 times (3rd failure starts cooldown, 4th-6th
        // each bumped `quarantine_count`), turning a 60 s cooldown
        // into 240 s and a fanout retry storm into 10-minute+ ban.
        let h = PeerHealthTracker::new();
        // 6 concurrent failures (simulates one in-flight burst all
        // discovering the same listener reset at once).
        for _ in 0..6 {
            h.record_failure("pod");
        }
        // After the burst we expect ONE quarantine, not four.
        let snap = h.snapshot();
        let pod = snap.iter().find(|r| r.0 == "pod").expect("pod recorded");
        // seconds_until_recovery should be close to INITIAL_COOLDOWN,
        // not 4× it. Tolerate up to a few seconds of test runtime
        // skew; the real test is that it isn't multiples larger.
        assert!(
            pod.3 <= INITIAL_COOLDOWN.as_secs(),
            "cooldown extended past 60 s on first burst (got {}s)",
            pod.3
        );
        assert!(pod.1, "peer should be quarantined");
    }

    #[test]
    fn record_failure_during_quarantine_returns_false() {
        // Per the docstring, `record_failure` returns true *iff* this
        // call triggered (or extended) quarantine. A redundant failure
        // during an existing quarantine window neither triggers nor
        // extends, so it must return false.
        let h = PeerHealthTracker::new();
        for _ in 0..FAILURE_THRESHOLD {
            h.record_failure("pod");
        }
        assert!(h.is_quarantined("pod"));
        // Additional failures while in cooldown return false.
        assert!(!h.record_failure("pod"));
        assert!(!h.record_failure("pod"));
        assert!(!h.record_failure("pod"));
    }

    #[test]
    fn snapshot_reports_all_peers() {
        let h = PeerHealthTracker::new();
        h.record_failure("mac");
        h.record_success("laptop");
        let snap = h.snapshot();
        let names: Vec<_> = snap.iter().map(|r| r.0.as_str()).collect();
        assert!(names.contains(&"mac"));
        assert!(names.contains(&"laptop"));
    }

    #[test]
    fn health_weight_optimistic_for_unknown_peer() {
        // A peer the tracker has never seen returns 1.0 so the
        // load balancer is willing to send it work and observe
        // outcomes. Anything lower would penalise new peers
        // before they had a chance to prove themselves.
        let h = PeerHealthTracker::new();
        assert_eq!(h.health_weight("brand-new-pod"), 1.0);
    }

    #[test]
    fn health_weight_tracks_recent_success_rate() {
        let h = PeerHealthTracker::new();
        // 4 successes, 1 failure → 80% weight. Note: float
        // comparison via abs() because 4/5 = 0.8 is exact in
        // binary but the floor.max() path may round.
        for _ in 0..4 {
            h.record_success("pod");
        }
        h.record_failure("pod");
        let w = h.health_weight("pod");
        assert!((w - 0.8).abs() < 1e-6, "expected ~0.8, got {w}");
    }

    #[test]
    fn health_weight_floors_at_minimum_not_zero() {
        // Even after a stream of failures, weight doesn't go to 0 —
        // the floor keeps the peer marginally routable so it has
        // a chance to recover. Quarantine handles the hard skip.
        let h = PeerHealthTracker::new();
        for _ in 0..RECENT_EVENT_WINDOW {
            h.record_failure("pod");
        }
        // Recent success rate is 0/20 = 0%, but the floor lifts it.
        let w = h.health_weight("pod");
        assert!(
            (HEALTH_WEIGHT_FLOOR..0.10).contains(&w),
            "expected floor (~5%), got {w}"
        );
    }

    #[test]
    fn health_weight_thrash_regression_5pct_under_load() {
        // Empirical anchor: the 2026-05-11 SEP fanout incident had
        // ~5% success rate on the Q4 pod (1 success / 20 attempts)
        // while it was reload-thrashing. After the burst, the soft
        // signal should be at the floor — telling the load balancer
        // "treat this peer as effectively 20× more loaded than its
        // raw in-flight count suggests."
        let h = PeerHealthTracker::new();
        h.record_success("pod"); // the one slug that made it through
        for _ in 0..19 {
            h.record_failure("pod");
        }
        let w = h.health_weight("pod");
        // 1/20 = 0.05 = exactly the floor; allow tiny float slack.
        assert!(
            (w - HEALTH_WEIGHT_FLOOR).abs() < 1e-6,
            "thrashing-peer weight should sit at floor ~{HEALTH_WEIGHT_FLOOR}, got {w}"
        );
    }

    #[test]
    fn ring_buffer_caps_at_window_size() {
        // The buffer is meant to be a SLIDING window — old events
        // age out. Push 50 events; only the last 20 should be in
        // the score.
        let h = PeerHealthTracker::new();
        for _ in 0..30 {
            h.record_failure("pod"); // 30 old failures
        }
        for _ in 0..RECENT_EVENT_WINDOW {
            h.record_success("pod"); // 20 fresh successes
        }
        // The 30 failures should have aged out completely. The
        // last 20 events are all successes → weight = 1.0.
        // record_success also clears consecutive_failures so the
        // peer isn't quarantined, but record_success doesn't wipe
        // the buffer unless the peer was actually quarantined —
        // and 3 consecutive failures DID trigger quarantine in
        // this sequence. Once the 20 successes land, weight is
        // 1.0 either way (buffer cleared on quarantine-recovery
        // OR full of fresh successes).
        assert_eq!(h.health_weight("pod"), 1.0);
    }

    #[test]
    fn recovery_after_quarantine_clears_history() {
        // A peer that quarantines, then recovers via a single
        // success, should have its weight jump back to 1.0
        // immediately — not stay anchored at 0% by the prior
        // failure history. This is the "fast recovery" property
        // that lets a transient outage not penalise the peer
        // for the next 20 requests.
        let h = PeerHealthTracker::new();
        for _ in 0..FAILURE_THRESHOLD {
            h.record_failure("pod");
        }
        assert!(h.is_quarantined("pod"));
        h.record_success("pod");
        assert!(!h.is_quarantined("pod"));
        // The single success after quarantine clears the buffer
        // and pushes 1 success, so weight = 1.0.
        assert_eq!(h.health_weight("pod"), 1.0);
    }
}
