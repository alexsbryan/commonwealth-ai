// SPDX-License-Identifier: AGPL-3.0-or-later
//! Eligibility gate for mesh RPC inference workers — distribute only to
//! PROVEN-STABLE workers, never thrash on a flapping one.
//!
//! ## Why this exists
//!
//! The discovery loop ([`daemon_cmd`]) reloads the primary across the worker set
//! whenever that set changes. A worker whose in-process RPC server flaps
//! (crashes → the supervisor restarts it → its liveness-gated `/status`
//! advertisement toggles) makes the discovered set oscillate, and the host
//! redistributes on every change. A 2026-06-05 benchmark drove **11 reloads in
//! 27 min** this way — and worse, when a worker crashes *during* graph compute
//! the host process is `GGML_ABORT`ed (upstream ggml-rpc.cpp, uncatchable
//! in-process). So an unstable worker doesn't just churn the scheduler; it can
//! kill the host. The only robust defence is to **not distribute to it**.
//!
//! This gate sits between raw discovery and the worker provider. It tracks each
//! worker's presence history and exposes only workers that are **present, past a
//! settle window, and not quarantined**. A worker that flaps is quarantined with
//! linear backoff and excluded until it proves stable again.
//!
//! ## Design (mirrors `commonwealth-core::peer_health::PeerHealthTracker`)
//!
//! - **Settle before use.** A freshly-appeared worker is *Probationary* until it
//!   has been continuously advertised for `settle` (default 90 s). This keeps a
//!   worker that joins and immediately dies out of the distribution set.
//! - **Flap → quarantine, linear backoff.** Each present→absent transition is a
//!   flap; `flap_threshold` (default 3) flaps within `flap_window` (default
//!   10 min) quarantines the worker for `initial_cooldown × quarantine_count`,
//!   capped at `max_cooldown` (60 s → 600 s). Long enough that a flapping worker
//!   stops churning the host; short enough that genuine recovery surfaces.
//! - **Fail-safe default.** A worker is NOT eligible until proven stable —
//!   "don't distribute" is the safe default, matching the never-wedge ethos.
//! - **Glassbox.** Every state transition logs at INFO (`worker-eligibility:
//!   …`), so an operator sees *why* a worker isn't being used without DEBUG.
//! - **Pure + clock-injected.** `observe`/`eligible` take `now: Instant`, so the
//!   whole state machine is unit-testable with a simulated clock — no GPU, no
//!   network, no model files (ARCH §12.4).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Robust, operator-overridable defaults. Read once at construction. Mirrors the
/// `SOVEREIGN_RPC_*` env-knob convention used elsewhere in the RPC path.
#[derive(Debug, Clone)]
pub struct EligibilityConfig {
    /// Continuous presence required before a worker becomes eligible.
    pub settle: Duration,
    /// Flaps within `flap_window` that trigger quarantine.
    pub flap_threshold: u32,
    /// Sliding window over which flaps are counted.
    pub flap_window: Duration,
    /// First quarantine duration; each re-quarantine adds another, up to the cap.
    pub initial_cooldown: Duration,
    /// Cap on quarantine duration so genuine recovery isn't masked forever.
    pub max_cooldown: Duration,
}

impl Default for EligibilityConfig {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(90),
            flap_threshold: 3,
            flap_window: Duration::from_secs(600),
            initial_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
        }
    }
}

/// Stricter settle for shared-model anchors: holding a shard of the shared
/// model is a commitment, not casual help. An anchor must prove ~5 min of
/// stability before we re-plan the layer-split across it (a freshly-joined
/// anchor that's about to leave again would thrash the whole cluster).
pub const ANCHOR_SETTLE_SECS: u64 = 300;
/// Quarantine an anchor on its FIRST flap. A casual RPC worker that crashes is
/// merely unhelpful; a flapping *anchor* can `GGML_ABORT` the entire host
/// mid-decode, so one strike is enough to exclude it until it recovers.
pub const ANCHOR_FLAP_THRESHOLD: u32 = 1;

impl EligibilityConfig {
    /// The stricter eligibility profile for shared-model anchors. Same flap
    /// window + cooldown backoff as the casual-worker [`default`](Self::default),
    /// but a longer settle and a one-strike flap threshold (see the consts).
    pub fn anchor() -> Self {
        Self {
            settle: Duration::from_secs(ANCHOR_SETTLE_SECS),
            flap_threshold: ANCHOR_FLAP_THRESHOLD,
            ..Self::default()
        }
    }
}

impl EligibilityConfig {
    /// Override any field from the environment; otherwise the robust default.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            settle: env_secs("SOVEREIGN_RPC_WORKER_SETTLE_SECS", d.settle),
            flap_threshold: env_u32("SOVEREIGN_RPC_WORKER_FLAP_THRESHOLD", d.flap_threshold),
            flap_window: env_secs("SOVEREIGN_RPC_WORKER_FLAP_WINDOW_SECS", d.flap_window),
            initial_cooldown: env_secs("SOVEREIGN_RPC_WORKER_COOLDOWN_SECS", d.initial_cooldown),
            max_cooldown: env_secs("SOVEREIGN_RPC_WORKER_MAX_COOLDOWN_SECS", d.max_cooldown),
        }
    }
}

fn env_secs(key: &str, default: Duration) -> Duration {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}
fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

/// The observable state of a worker, derived from its history at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    /// Not advertised right now (and not quarantined).
    Absent,
    /// Present but still inside the settle window — not yet distributed to.
    Probationary,
    /// Present, settled, not quarantined — distributed to.
    Eligible,
    /// Flapped too often; excluded until the cooldown elapses.
    Quarantined,
}

#[derive(Debug, Default, Clone)]
struct WorkerState {
    /// Present in the most recent observation.
    present: bool,
    /// When the current continuous-presence run began (reset when it disappears).
    present_since: Option<Instant>,
    /// When it crossed the settle window into eligibility (None until then).
    eligible_since: Option<Instant>,
    /// Timestamps of present→absent transitions within the flap window.
    flaps: VecDeque<Instant>,
    /// How many times quarantined this process — drives the linear backoff.
    quarantine_count: u32,
    /// `Some(t)` while quarantined; excluded until `now >= t`.
    quarantined_until: Option<Instant>,
}

impl WorkerState {
    fn status(&self, now: Instant, cfg: &EligibilityConfig) -> WorkerStatus {
        let _ = cfg;
        if self.quarantined_until.is_some_and(|d| d > now) {
            return WorkerStatus::Quarantined;
        }
        if !self.present {
            return WorkerStatus::Absent;
        }
        if self.eligible_since.is_some() {
            WorkerStatus::Eligible
        } else {
            WorkerStatus::Probationary
        }
    }

    /// Seconds until the quarantine cooldown elapses (0 if not quarantined).
    fn quarantine_remaining(&self, now: Instant) -> u64 {
        match self.quarantined_until {
            Some(d) if d > now => d.duration_since(now).as_secs(),
            _ => 0,
        }
    }

    /// Record a present→absent transition and quarantine if it flaps too often.
    fn record_flap(&mut self, now: Instant, cfg: &EligibilityConfig) {
        // Evict flaps older than the window, then count this one.
        while let Some(&front) = self.flaps.front() {
            if now.duration_since(front) > cfg.flap_window {
                self.flaps.pop_front();
            } else {
                break;
            }
        }
        self.flaps.push_back(now);
        let already_quarantined = self.quarantined_until.is_some_and(|d| d > now);
        if self.flaps.len() as u32 >= cfg.flap_threshold && !already_quarantined {
            self.quarantine_count = self.quarantine_count.saturating_add(1);
            let cooldown = cfg
                .initial_cooldown
                .saturating_mul(self.quarantine_count)
                .min(cfg.max_cooldown);
            self.quarantined_until = Some(now + cooldown);
        }
    }
}

/// One worker's eligibility view — for `/status` and `sovereign mesh status`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerStatusView {
    pub endpoint: String,
    pub status: WorkerStatus,
    pub flaps_in_window: u32,
    pub quarantine_remaining_secs: u64,
}

/// Tracks per-worker eligibility. Cheap to share (`Arc`) — the lock is internal.
#[derive(Debug)]
pub struct WorkerEligibility {
    config: EligibilityConfig,
    workers: Mutex<HashMap<String, WorkerState>>,
}

impl Default for WorkerEligibility {
    fn default() -> Self {
        Self::new(EligibilityConfig::from_env())
    }
}

impl WorkerEligibility {
    pub fn new(config: EligibilityConfig) -> Self {
        Self {
            config,
            workers: Mutex::new(HashMap::new()),
        }
    }

    /// Fold one discovery observation (`present` = workers advertised this tick)
    /// into per-worker state, logging every status transition at INFO. Pure given
    /// `(state, present, now)`.
    pub fn observe(&self, present: &[String], now: Instant) {
        let present_set: HashSet<&str> = present.iter().map(String::as_str).collect();
        let mut map = self.workers.lock().unwrap_or_else(|e| e.into_inner());

        // Ensure every currently-present worker has an entry to update.
        for ep in present {
            map.entry(ep.clone()).or_default();
        }

        for (ep, st) in map.iter_mut() {
            let before = st.status(now, &self.config);
            let now_present = present_set.contains(ep.as_str());
            let was_present = st.present;

            match (was_present, now_present) {
                (false, true) => {
                    // Appeared. If the quarantine has elapsed, (re)enter probation;
                    // if still quarantined, stay quarantined (just mark present).
                    if !st.quarantined_until.is_some_and(|d| d > now) {
                        st.quarantined_until = None;
                        st.present_since = Some(now);
                        st.eligible_since = None;
                    }
                }
                (true, false) => {
                    // Disappeared — a flap. Reset the settle run; maybe quarantine.
                    st.present_since = None;
                    st.eligible_since = None;
                    st.record_flap(now, &self.config);
                }
                (true, true) => {
                    let not_quarantined = !st.quarantined_until.is_some_and(|d| d > now);
                    // Start the settle clock if we don't have one yet and we're not
                    // quarantined — e.g. the worker stayed present THROUGH a
                    // quarantine that has now expired; it re-enters probation from
                    // here and must settle afresh before becoming eligible again.
                    if st.present_since.is_none() && not_quarantined {
                        st.present_since = Some(now);
                    }
                    // Promote to eligible once settled.
                    let settled = st
                        .present_since
                        .is_some_and(|s| now.duration_since(s) >= self.config.settle);
                    if st.eligible_since.is_none() && settled && not_quarantined {
                        st.eligible_since = Some(now);
                    }
                }
                (false, false) => {}
            }
            st.present = now_present;

            let after = st.status(now, &self.config);
            if before != after {
                tracing::info!(
                    worker = %ep,
                    from = ?before,
                    to = ?after,
                    flaps = st.flaps.len(),
                    quarantine_count = st.quarantine_count,
                    cooldown_secs = st.quarantine_remaining(now),
                    "worker-eligibility: state change"
                );
            }
        }
    }

    /// The workers safe to distribute to right now — present, settled, not
    /// quarantined. Sorted, so the caller's set-change comparison is stable.
    pub fn eligible(&self, now: Instant) -> Vec<String> {
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<String> = map
            .iter()
            .filter(|(_, st)| st.status(now, &self.config) == WorkerStatus::Eligible)
            .map(|(ep, _)| ep.clone())
            .collect();
        out.sort();
        out
    }

    /// Per-worker views for observability surfaces. Sorted by endpoint.
    pub fn status_views(&self, now: Instant) -> Vec<WorkerStatusView> {
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<WorkerStatusView> = map
            .iter()
            .map(|(ep, st)| WorkerStatusView {
                endpoint: ep.clone(),
                status: st.status(now, &self.config),
                flaps_in_window: st.flaps.len() as u32,
                quarantine_remaining_secs: st.quarantine_remaining(now),
            })
            .collect();
        out.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
        out
    }
}

/// Process-wide tracker, installed once by the daemon's discovery loop so
/// observability surfaces (`/status`, `sovereign mesh status`) can read the same
/// eligibility state the loop acts on — one source of truth, like
/// `set_rpc_worker_provider`.
static GLOBAL: std::sync::OnceLock<std::sync::Arc<WorkerEligibility>> = std::sync::OnceLock::new();

/// Install the process-wide eligibility tracker (idempotent).
pub fn set_global(tracker: std::sync::Arc<WorkerEligibility>) {
    let _ = GLOBAL.set(tracker);
}

/// The process-wide eligibility tracker, if the daemon installed one (i.e. it's a
/// discovery host). `None` on a node not running RPC discovery.
pub fn global() -> Option<std::sync::Arc<WorkerEligibility>> {
    GLOBAL.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EligibilityConfig {
        // Small windows for fast deterministic tests.
        EligibilityConfig {
            settle: Duration::from_secs(90),
            flap_threshold: 3,
            flap_window: Duration::from_secs(600),
            initial_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
        }
    }
    fn w(s: &str) -> Vec<String> {
        if s.is_empty() {
            vec![]
        } else {
            vec![s.to_string()]
        }
    }

    #[test]
    fn anchor_profile_is_stricter_than_default() {
        let a = EligibilityConfig::anchor();
        let d = EligibilityConfig::default();
        assert_eq!(a.settle, Duration::from_secs(ANCHOR_SETTLE_SECS));
        assert!(a.settle > d.settle, "anchor settles slower");
        assert_eq!(a.flap_threshold, 1, "anchor quarantines on first flap");
        assert!(a.flap_threshold < d.flap_threshold);
        // Only settle + flap threshold tighten; the backoff knobs are unchanged.
        assert_eq!(a.flap_window, d.flap_window);
        assert_eq!(a.max_cooldown, d.max_cooldown);
    }

    #[test]
    fn stable_worker_eligible_after_settle() {
        let e = WorkerEligibility::new(cfg());
        let t = Instant::now();
        e.observe(&w("a:1"), t); // appears
        assert!(e.eligible(t).is_empty(), "not eligible before settle");
        e.observe(&w("a:1"), t + Duration::from_secs(60)); // still settling
        assert!(e.eligible(t + Duration::from_secs(60)).is_empty());
        let t2 = t + Duration::from_secs(95); // past 90s settle
        e.observe(&w("a:1"), t2);
        assert_eq!(e.eligible(t2), vec!["a:1".to_string()], "eligible after settle");
    }

    #[test]
    fn flapping_worker_never_eligible_and_gets_quarantined() {
        let e = WorkerEligibility::new(cfg());
        let mut t = Instant::now();
        // Flap 3 times, each cycle shorter than the settle window.
        for _ in 0..3 {
            e.observe(&w("a:1"), t);
            t += Duration::from_secs(20);
            e.observe(&w(""), t); // disappears → flap
            t += Duration::from_secs(20);
        }
        assert!(e.eligible(t).is_empty(), "a flapping worker is never eligible");
        // After 3 flaps it is quarantined; even continuous presence now can't make
        // it eligible until the cooldown elapses.
        let views = e.status_views(t);
        assert_eq!(views[0].status, WorkerStatus::Quarantined);
        assert!(views[0].quarantine_remaining_secs > 0);
    }

    #[test]
    fn quarantine_backoff_is_linear_and_capped() {
        let mut st = WorkerState::default();
        let c = cfg();
        let t = Instant::now();
        // Drive successive quarantines; each must add initial_cooldown, capped.
        let mut cooldowns = vec![];
        for round in 0..12 {
            // Force a fresh flap burst past threshold after the prior cooldown.
            let base = t + Duration::from_secs(round * 1000);
            st.quarantined_until = None;
            st.flaps.clear();
            for _ in 0..c.flap_threshold {
                st.record_flap(base, &c);
            }
            cooldowns.push(st.quarantine_remaining(base));
        }
        assert_eq!(cooldowns[0], 60);
        assert_eq!(cooldowns[1], 120);
        assert_eq!(cooldowns[2], 180);
        assert_eq!(*cooldowns.last().unwrap(), 600, "capped at max_cooldown");
        assert!(cooldowns.windows(2).all(|p| p[1] >= p[0]), "monotonic");
    }

    #[test]
    fn quarantined_excluded_until_cooldown_then_reprobation() {
        let e = WorkerEligibility::new(cfg());
        let mut t = Instant::now();
        for _ in 0..3 {
            e.observe(&w("a:1"), t);
            t += Duration::from_secs(10);
            e.observe(&w(""), t);
            t += Duration::from_secs(10);
        }
        // Quarantined (~60s). Reappearing during cooldown stays quarantined.
        e.observe(&w("a:1"), t + Duration::from_secs(5));
        assert!(e.eligible(t + Duration::from_secs(5)).is_empty());
        // After cooldown + reappearance → Probationary again (not instantly eligible).
        let after = t + Duration::from_secs(120);
        e.observe(&w("a:1"), after);
        assert_eq!(e.status_views(after)[0].status, WorkerStatus::Probationary);
        assert!(e.eligible(after).is_empty());
        // Settle from there → eligible again.
        let settled = after + Duration::from_secs(95);
        e.observe(&w("a:1"), settled);
        assert_eq!(e.eligible(settled), vec!["a:1".to_string()]);
    }

    #[test]
    fn eligible_set_stable_under_flap_does_not_thrash() {
        // The anti-thrash invariant: while a worker flaps, the eligible set the
        // discovery loop compares against must NOT oscillate (it stays empty,
        // then quarantined) — so no reload is triggered per flap.
        let e = WorkerEligibility::new(cfg());
        let mut t = Instant::now();
        let mut eligible_sets = vec![];
        for _ in 0..6 {
            e.observe(&w("a:1"), t);
            eligible_sets.push(e.eligible(t));
            t += Duration::from_secs(25);
            e.observe(&w(""), t);
            eligible_sets.push(e.eligible(t));
            t += Duration::from_secs(25);
        }
        assert!(
            eligible_sets.iter().all(|s| s.is_empty()),
            "a flapping worker never enters the eligible set, so the loop never reloads"
        );
    }
}
