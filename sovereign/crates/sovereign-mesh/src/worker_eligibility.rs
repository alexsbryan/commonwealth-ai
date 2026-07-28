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

use commonwealth_core::ids::NodeId;

/// One RPC worker as seen by a single discovery tick: a stable mesh identity
/// (`node_id`) plus the address the host should dial it at THIS tick.
///
/// The identity is what the eligibility tracker keys on; the endpoint is a
/// *mutable attribute* of that identity. So a worker whose address flips
/// (direct-ip ↔ iroh-bridge loopback) for the SAME node is NOT treated as a
/// departed-then-reappeared worker — which would read as a flap and force a full
/// re-settle, collapsing a live distribution to local-only (observed 2026-07-19
/// in the 122B e2e: one transient direct-ip probe miss flipped BeefyMac's
/// endpoint string and emptied the eligible set mid-inference).
///
/// Env-configured workers (`SOVEREIGN_RPC_WORKERS`) never reach this layer —
/// they're unioned in at the provider (`rpc_distribution`), so every worker the
/// tracker observes has a real mesh node_id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWorker {
    /// Stable mesh identity — the eligibility map key.
    pub node_id: NodeId,
    /// Address to dial this tick (`ip:port`); a mutable attribute of `node_id`.
    pub endpoint: String,
}

/// One discovery tick's FULL statement — including what it could not determine.
///
/// A bare `Vec<DiscoveredWorker>` erased the difference between "the peer
/// answered: no RPC worker" and "the peer never answered", and the eligibility
/// gate read both as absence. On 2026-07-28 a peer that was alive the whole time
/// — serving OUR 21GB model warm, which is exactly when its `/status` probe is
/// least likely to answer inside an 800 ms budget — went `Eligible → Absent` on
/// starved probes. Under the anchor profile (one strike) that quarantined it for
/// 60 s and forced a fresh 300 s settle, and the next tick retired a compute
/// child that had been serving for eight seconds.
///
/// THE CONTRACT, which is the whole point of this type:
/// - in `workers` → confirmed present, dial it;
/// - in `unconfirmed` → NO STATEMENT. Gossip still lists the peer; we simply
///   could not confirm. Hold the prior state.
/// - in NEITHER → confirmed ABSENT. Gossip dropped the peer, which is positive
///   evidence, so `kill -9` of a worker daemon still converges at today's speed
///   (the P0.4 acceptance criterion).
/// - `scanned == false` → the scan could not run at all; NOTHING in this tick is
///   evidence about ANY worker.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryOutcome {
    /// Workers confirmed dialable this tick.
    pub workers: Vec<DiscoveredWorker>,
    /// Peers we hold worker history for and polled, but could not confirm.
    pub unconfirmed: Vec<NodeId>,
    /// Gossip-online, dialable, anchor-capable peers considered this tick.
    /// Distinguishes "we polled nobody" from "we polled one and it went quiet" —
    /// the ambiguity that made the incident's `eligible=0 discovered=0` log line
    /// unreadable.
    pub polled: usize,
    /// Whether the scan ran at all.
    pub scanned: bool,
}

impl DiscoveryOutcome {
    /// An outcome with FULL evidence: every worker not listed is confirmed
    /// absent. This is what a caller that genuinely knows the complete set
    /// asserts — and what the legacy [`WorkerEligibility::observe`] shim builds.
    pub fn complete(workers: Vec<DiscoveredWorker>) -> Self {
        let polled = workers.len();
        Self {
            workers,
            unconfirmed: Vec::new(),
            polled,
            scanned: true,
        }
    }
}

/// What one tick says about one tracked worker. See [`DiscoveryOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// Confirmed dialable.
    Present,
    /// No statement either way — freeze this worker's state machine.
    Unconfirmed,
    /// Positive grounds to believe it is gone.
    Absent,
}

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
    /// How long a worker may stay UNCONFIRMED before its absence is believed.
    ///
    /// Bounds the benefit of the doubt: a peer whose probe we starved keeps its
    /// state, but a peer that is genuinely dead still leaves. `0` disables the
    /// hold entirely — every non-present tick is read as absence, which is
    /// exactly the pre-2026-07-28 behaviour.
    pub absence_grace: Duration,
}

impl Default for EligibilityConfig {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(90),
            flap_threshold: 3,
            flap_window: Duration::from_secs(600),
            initial_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
            // ~8 discovery ticks. Comfortably outlasts the multi-tick probe
            // starvation observed while a peer served a 21GB warm, and is well
            // under the 300s anchor settle it exists to protect — while still
            // bounded, so a dead-but-gossip-online worker leaves in ~2 min.
            absence_grace: Duration::from_secs(120),
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
    ///
    /// NOTE: this constructor is NOT the live path. The daemon applies the
    /// anchor profile by setting `SOVEREIGN_RPC_WORKER_SETTLE_SECS` /
    /// `..._FLAP_THRESHOLD` from the shared-model role (`bootstrap.rs`), which
    /// [`Self::from_env`] then reads on top of [`Self::default`]. The values
    /// agree today because both read the consts above; changing THIS function
    /// alone will not change daemon behaviour.
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
            absence_grace: env_secs("SOVEREIGN_RPC_WORKER_ABSENCE_GRACE_SECS", d.absence_grace),
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
    /// The address dialed for this node as of the most recent observation. The
    /// node_id is the map key (stable identity); this tracks the mutable
    /// address, so an address change updates in place and never disturbs
    /// presence/flap/settle state.
    endpoint: String,
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
    /// When the current UNCONFIRMED run began; `None` whenever we have a
    /// definite statement. Bounds the hold — see `EligibilityConfig::absence_grace`.
    unconfirmed_since: Option<Instant>,
    /// When this peer last carried a successful multi-gigabyte model transfer.
    /// Out-of-band liveness that is strictly stronger than an 800ms probe.
    warm_alive_at: Option<Instant>,
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
    /// Stable mesh identity (full hex) — so an operator can tell WHICH peer a
    /// row is, independent of whichever address it's currently dialed at.
    /// `serde(default)` keeps older `/v1/mesh/status` payloads deserializable.
    #[serde(default)]
    pub node_id: String,
    pub endpoint: String,
    pub status: WorkerStatus,
    pub flaps_in_window: u32,
    pub quarantine_remaining_secs: u64,
    /// How long this worker's state has been held on NO EVIDENCE (0 when we
    /// have a definite statement). Makes the withheld-judgement window visible
    /// to an operator — "Eligible, unconfirmed for 45s" — instead of it being a
    /// silent internal hold. `serde(default)` keeps older payloads readable.
    #[serde(default)]
    pub unconfirmed_for_secs: u64,
}

/// Classify what this tick says about one tracked worker.
///
/// Pure, and the hinge of the whole fix. Absence-from-the-slice degrades to a
/// real [`Evidence::Absent`] only when we have positive grounds AND the benefit
/// of the doubt has run out — a genuinely dead worker must still leave.
fn evidence_for(
    id: &NodeId,
    st: &WorkerState,
    present: &HashSet<NodeId>,
    unconfirmed: &HashSet<NodeId>,
    scanned: bool,
    now: Instant,
    cfg: &EligibilityConfig,
) -> Evidence {
    if present.contains(id) {
        return Evidence::Present;
    }
    // Grace disabled — every non-present tick is absence, i.e. exactly the
    // pre-2026-07-28 behaviour. Kept as a one-line escape for an operator who
    // wants the old semantics back.
    if cfg.absence_grace.is_zero() {
        return Evidence::Absent;
    }

    // Do we have a REASON to withhold judgement this tick?
    let no_statement = !scanned
        || unconfirmed.contains(id)
        || st
            .warm_alive_at
            .is_some_and(|t| now.duration_since(t) < cfg.absence_grace);
    if !no_statement {
        // Positive grounds: the scan ran, the peer was not merely unprobeable
        // (gossip dropped it), and nothing vouches for it.
        return Evidence::Absent;
    }

    // Withholding — but only for so long.
    let held_for = st.unconfirmed_since.map(|s| now.duration_since(s));
    if held_for.is_some_and(|d| d >= cfg.absence_grace) {
        Evidence::Absent
    } else {
        Evidence::Unconfirmed
    }
}

/// Tracks per-worker eligibility. Cheap to share (`Arc`) — the lock is internal.
#[derive(Debug)]
pub struct WorkerEligibility {
    config: EligibilityConfig,
    workers: Mutex<HashMap<NodeId, WorkerState>>,
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

    /// Fold one FULL-EVIDENCE observation: every worker not in `present` is
    /// treated as confirmed absent.
    ///
    /// Retained for callers that genuinely know the complete set (and for the
    /// unit tests that predate [`DiscoveryOutcome`]). Production discovery uses
    /// [`Self::observe_outcome`], because a probe that timed out is not the same
    /// statement as a peer reporting no worker.
    pub fn observe(&self, present: &[DiscoveredWorker], now: Instant) {
        self.observe_outcome(&DiscoveryOutcome::complete(present.to_vec()), now);
    }

    /// Fold one discovery outcome, honouring its unconfirmed set.
    ///
    /// Logs every status transition at INFO. Pure given `(state, outcome, now)`.
    pub fn observe_outcome(&self, outcome: &DiscoveryOutcome, now: Instant) {
        let present_ids: HashSet<NodeId> = outcome.workers.iter().map(|w| w.node_id).collect();
        let unconfirmed_ids: HashSet<NodeId> = outcome.unconfirmed.iter().copied().collect();
        let addresses: HashMap<NodeId, &str> = outcome
            .workers
            .iter()
            .map(|w| (w.node_id, w.endpoint.as_str()))
            .collect();
        let mut map = self.workers.lock().unwrap_or_else(|e| e.into_inner());

        // Ensure every currently-present worker has an entry to update.
        for w in &outcome.workers {
            map.entry(w.node_id).or_default();
        }

        for (id, st) in map.iter_mut() {
            let before = st.status(now, &self.config);
            let was_present = st.present;
            let evidence = evidence_for(
                id,
                st,
                &present_ids,
                &unconfirmed_ids,
                outcome.scanned,
                now,
                &self.config,
            );

            match evidence {
                // No statement. Freeze the whole state machine: no flap, no
                // re-settle, endpoint untouched. Because `present` is left as it
                // was, the disappear arm below fires EXACTLY ONCE when the grace
                // finally expires — one flap, not one per tick.
                Evidence::Unconfirmed => {
                    st.unconfirmed_since.get_or_insert(now);
                }
                Evidence::Present => {
                    st.unconfirmed_since = None;
                    // An address change for a present node is NOT a flap —
                    // update the endpoint in place and leave presence/settle
                    // state untouched.
                    if let Some(ep) = addresses.get(id) {
                        st.endpoint = (*ep).to_string();
                    }
                    if was_present {
                        let not_quarantined = st.quarantined_until.is_none_or(|d| d <= now);
                        // Start the settle clock if we don't have one yet and
                        // we're not quarantined — e.g. the worker stayed present
                        // THROUGH a quarantine that has now expired; it re-enters
                        // probation from here and must settle afresh.
                        if st.present_since.is_none() && not_quarantined {
                            st.present_since = Some(now);
                        }
                        let settled = st
                            .present_since
                            .is_some_and(|s| now.duration_since(s) >= self.config.settle);
                        if st.eligible_since.is_none() && settled && not_quarantined {
                            st.eligible_since = Some(now);
                        }
                    } else {
                        // Appeared. If the quarantine has elapsed, (re)enter
                        // probation; if still quarantined, stay quarantined
                        // (just mark present).
                        if st.quarantined_until.is_none_or(|d| d <= now) {
                            st.quarantined_until = None;
                            st.present_since = Some(now);
                            st.eligible_since = None;
                        }
                    }
                    st.present = true;
                }
                Evidence::Absent => {
                    st.unconfirmed_since = None;
                    if was_present {
                        // Disappeared — a flap. Reset the settle run; maybe
                        // quarantine.
                        st.present_since = None;
                        st.eligible_since = None;
                        st.record_flap(now, &self.config);
                    }
                    st.present = false;
                }
            }

            let after = st.status(now, &self.config);
            if before != after {
                tracing::info!(
                    worker = %id,
                    endpoint = %st.endpoint,
                    from = ?before,
                    to = ?after,
                    evidence = ?evidence,
                    flaps = st.flaps.len(),
                    quarantine_count = st.quarantine_count,
                    cooldown_secs = st.quarantine_remaining(now),
                    "worker-eligibility: state change"
                );
            }
        }
    }

    /// Positive out-of-band liveness: this peer just carried a successful
    /// multi-gigabyte model transfer, which is far stronger evidence than any
    /// 800 ms probe — and it arrives precisely when the probes are most likely
    /// to be starved, because the transfer is what starves them.
    ///
    /// Deliberately narrow, so that a warm can never mask a genuinely flapping
    /// worker:
    /// - it NEVER creates an entry — a warm may vouch for a worker discovery
    ///   already knows about, never invent one;
    /// - it does not clear a quarantine, retract a flap, or shorten a settle;
    /// - its only effect is to deny the NEXT tick the right to call a confirmed
    ///   absence, and even that is bounded by the same `absence_grace` as every
    ///   other hold. A peer that warms once and then goes quiet still degrades
    ///   on schedule.
    ///
    /// Routing it through [`Evidence`] rather than mutating presence directly is
    /// what makes that safe: a `note_alive` that set `present = true` would be
    /// re-flapped by the very next `observe`, oscillating between the two
    /// mutators.
    pub fn note_alive(&self, node: NodeId, endpoint: &str, now: Instant) {
        let mut map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let Some(st) = map.get_mut(&node) else {
            tracing::debug!(
                worker = %node,
                endpoint = %endpoint,
                "worker-eligibility: warm success for a worker discovery has never seen — ignored"
            );
            return;
        };
        if st.endpoint != endpoint {
            tracing::debug!(
                worker = %node,
                warm_endpoint = %endpoint,
                known_endpoint = %st.endpoint,
                "worker-eligibility: warm succeeded over a different address for a known worker"
            );
        }
        st.warm_alive_at = Some(now);
        tracing::info!(
            worker = %node,
            endpoint = %endpoint,
            status = ?st.status(now, &self.config),
            "worker-eligibility: warm transfer succeeded — treating this peer as alive"
        );
    }

    /// The workers safe to distribute to right now — present, settled, not
    /// quarantined. Sorted, so the caller's set-change comparison is stable.
    pub fn eligible(&self, now: Instant) -> Vec<String> {
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<String> = map
            .iter()
            .filter(|(_, st)| st.status(now, &self.config) == WorkerStatus::Eligible)
            .map(|(_, st)| st.endpoint.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Per-worker views for observability surfaces. Sorted by endpoint.
    pub fn status_views(&self, now: Instant) -> Vec<WorkerStatusView> {
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<WorkerStatusView> = map
            .iter()
            .map(|(id, st)| WorkerStatusView {
                node_id: id.to_hex(),
                endpoint: st.endpoint.clone(),
                status: st.status(now, &self.config),
                flaps_in_window: st.flaps.len() as u32,
                quarantine_remaining_secs: st.quarantine_remaining(now),
                unconfirmed_for_secs: st
                    .unconfirmed_since
                    .map(|s| now.duration_since(s).as_secs())
                    .unwrap_or(0),
            })
            .collect();
        out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
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
            // The pre-existing tests assert FULL-EVIDENCE semantics (every
            // worker missing from a tick is gone), so they run with the hold
            // disabled. The unconfirmed tests below build their own config.
            absence_grace: Duration::ZERO,
        }
    }

    /// As [`cfg`], but with the unconfirmed hold enabled.
    fn cfg_with_grace(grace_secs: u64) -> EligibilityConfig {
        EligibilityConfig {
            absence_grace: Duration::from_secs(grace_secs),
            ..cfg()
        }
    }

    /// A tick that polled `ids` and could confirm none of them.
    fn unconfirmed_tick(ids: &[u128]) -> DiscoveryOutcome {
        DiscoveryOutcome {
            workers: vec![],
            unconfirmed: ids.iter().map(|n| NodeId::from_u128(*n)).collect(),
            polled: ids.len(),
            scanned: true,
        }
    }

    /// One discovered worker with a caller-chosen node identity + address.
    fn dw(node: u128, ep: &str) -> DiscoveredWorker {
        DiscoveredWorker {
            node_id: NodeId::from_u128(node),
            endpoint: ep.to_string(),
        }
    }
    /// Single-worker observation helper: endpoint `s` under a fixed node id, so
    /// the same logical worker keeps its identity across ticks. `""` = absent.
    fn w(s: &str) -> Vec<DiscoveredWorker> {
        if s.is_empty() {
            vec![]
        } else {
            vec![dw(1, s)]
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
        assert_eq!(
            e.eligible(t2),
            vec!["a:1".to_string()],
            "eligible after settle"
        );
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
        assert!(
            e.eligible(t).is_empty(),
            "a flapping worker is never eligible"
        );
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

    // ── unconfirmed evidence (2026-07-28) ───────────────────────────────

    /// Settle a worker to Eligible under a grace-enabled config.
    fn settled(grace_secs: u64) -> (WorkerEligibility, Instant) {
        let e = WorkerEligibility::new(cfg_with_grace(grace_secs));
        let t0 = Instant::now();
        e.observe(&w("a:1"), t0);
        let t1 = t0 + Duration::from_secs(95);
        e.observe(&w("a:1"), t1);
        assert_eq!(e.eligible(t1), vec!["a:1".to_string()], "precondition");
        (e, t1)
    }

    /// THE INCIDENT, at the eligibility layer. A peer busy serving our own 21GB
    /// warm cannot answer an 800ms probe; that silence must not be read as
    /// departure, because under the anchor profile one strike quarantines it and
    /// forces a fresh 300s settle.
    #[test]
    fn an_unconfirmed_tick_does_not_flap_an_eligible_worker() {
        let (e, mut t) = settled(120);
        for _ in 0..3 {
            t += Duration::from_secs(15);
            e.observe_outcome(&unconfirmed_tick(&[1]), t);
        }
        assert_eq!(
            e.eligible(t),
            vec!["a:1".to_string()],
            "a worker we merely failed to reach must stay eligible"
        );
        let view = &e.status_views(t)[0];
        assert_eq!(view.flaps_in_window, 0, "no flap may be recorded");
        assert!(
            view.unconfirmed_for_secs > 0,
            "the hold must be visible to an operator"
        );
    }

    /// The benefit of the doubt is BOUNDED, and spends itself exactly once: a
    /// worker held through the grace produces ONE flap at expiry, not one per
    /// tick.
    #[test]
    fn unconfirmed_degrades_to_exactly_one_flap_after_the_grace() {
        let (e, mut t) = settled(120);
        for _ in 0..12 {
            t += Duration::from_secs(15);
            e.observe_outcome(&unconfirmed_tick(&[1]), t);
        }
        assert!(
            e.eligible(t).is_empty(),
            "a genuinely dead worker must still leave"
        );
        assert_eq!(
            e.status_views(t)[0].flaps_in_window,
            1,
            "expiry is ONE transition, not one per tick"
        );
    }

    /// Recovery inside the grace is free: the settle clock was never reset, so
    /// the worker is still eligible the moment it answers again. This is what
    /// turns the incident's 3 warm cycles into 1.
    #[test]
    fn a_recovery_inside_the_grace_does_not_re_settle() {
        let (e, mut t) = settled(120);
        for _ in 0..3 {
            t += Duration::from_secs(15);
            e.observe_outcome(&unconfirmed_tick(&[1]), t);
        }
        t += Duration::from_secs(15);
        e.observe(&w("a:1"), t);
        assert_eq!(
            e.eligible(t),
            vec!["a:1".to_string()],
            "no 300s re-settle after a transient probe starvation"
        );
        assert_eq!(e.status_views(t)[0].unconfirmed_for_secs, 0);
    }

    #[test]
    fn a_scan_that_did_not_run_is_not_evidence_of_absence() {
        let (e, mut t) = settled(120);
        t += Duration::from_secs(15);
        // scanned = false: the daemon wasn't running, or the HTTP client failed
        // to build. This says nothing about any worker.
        e.observe_outcome(&DiscoveryOutcome::default(), t);
        assert_eq!(e.eligible(t), vec!["a:1".to_string()]);
    }

    /// THE P0.4 REGRESSION GUARD. Gossip dropping a peer is POSITIVE evidence,
    /// so `kill -9` of a worker daemon must still converge at today's speed —
    /// the unconfirmed hold must not slow the acceptance path down.
    #[test]
    fn a_gossip_dropped_worker_is_confirmed_absent_immediately() {
        let (e, mut t) = settled(120);
        t += Duration::from_secs(15);
        // Scanned, and the peer is in NEITHER set — it is gone, not silent.
        e.observe_outcome(
            &DiscoveryOutcome {
                workers: vec![],
                unconfirmed: vec![],
                polled: 1,
                scanned: true,
            },
            t,
        );
        assert!(
            e.eligible(t).is_empty(),
            "confirmed absence must take effect on the first tick"
        );
        assert_eq!(e.status_views(t)[0].flaps_in_window, 1);
    }

    #[test]
    fn a_zero_grace_restores_the_pre_incident_behaviour() {
        let (e, mut t) = settled(0);
        t += Duration::from_secs(15);
        e.observe_outcome(&unconfirmed_tick(&[1]), t);
        assert!(
            e.eligible(t).is_empty(),
            "absence_grace = 0 must read every non-present tick as absence"
        );
    }

    // ── note_alive (warm-success liveness) ──────────────────────────────

    #[test]
    fn note_alive_suppresses_a_confirmed_absence_then_stops() {
        let (e, mut t) = settled(120);
        e.note_alive(NodeId::from_u128(1), "a:1", t);

        // Confirmed-absent ticks, but a multi-GB transfer just succeeded
        // through this peer — that outranks a failed probe.
        t += Duration::from_secs(15);
        e.observe_outcome(&DiscoveryOutcome::complete(vec![]), t);
        assert_eq!(
            e.eligible(t),
            vec!["a:1".to_string()],
            "a peer that just carried gigabytes is not dead"
        );

        // ...but the vouching is bounded by the same grace as everything else.
        for _ in 0..10 {
            t += Duration::from_secs(15);
            e.observe_outcome(&DiscoveryOutcome::complete(vec![]), t);
        }
        assert!(
            e.eligible(t).is_empty(),
            "one warm cannot vouch forever — the hold is bounded"
        );
    }

    #[test]
    fn note_alive_cannot_invent_a_worker() {
        let e = WorkerEligibility::new(cfg_with_grace(120));
        let t = Instant::now();
        e.note_alive(NodeId::from_u128(99), "ghost:1", t);
        assert!(
            e.status_views(t).is_empty(),
            "a warm may vouch for a known worker, never conjure one"
        );
    }

    #[test]
    fn note_alive_does_not_clear_a_quarantine() {
        // Flap a worker into quarantine (threshold 3), then warm it.
        let e = WorkerEligibility::new(cfg_with_grace(120));
        let mut t = Instant::now();
        for _ in 0..3 {
            e.observe(&w("a:1"), t);
            t += Duration::from_secs(5);
            e.observe(&w(""), t);
            t += Duration::from_secs(5);
        }
        assert_eq!(e.status_views(t)[0].status, WorkerStatus::Quarantined);

        e.note_alive(NodeId::from_u128(1), "a:1", t);
        assert_eq!(
            e.status_views(t)[0].status,
            WorkerStatus::Quarantined,
            "a warm must not resurrect a flapping anchor — that is the gate that \
             protects the host"
        );
    }

    #[test]
    fn address_change_for_same_node_is_not_a_flap() {
        // Regression (2026-07-19, 122B distributed e2e): a worker's endpoint
        // flipping (direct-ip → iroh-bridge loopback) for the SAME mesh node was
        // read as "old worker gone (flap), new worker appeared (re-settle from
        // zero)" because identity was the endpoint STRING. The eligible set
        // collapsed to empty and the host reloaded to local-only mid-inference.
        // Identity is the node; the address is a mutable attribute.
        let e = WorkerEligibility::new(cfg());
        let node = NodeId::from_u128(7);
        let direct = "100.104.36.28:50052";
        let bridge = "127.0.0.1:39119";
        let t0 = Instant::now();

        // Settle at the direct-ip address.
        e.observe(&[dw(7, direct)], t0);
        let settled = t0 + Duration::from_secs(95);
        e.observe(&[dw(7, direct)], settled);
        assert_eq!(e.eligible(settled), vec![direct.to_string()]);

        // The address flips to the bridge loopback — SAME node id.
        let t1 = settled + Duration::from_secs(15);
        e.observe(&[dw(7, bridge)], t1);

        // Still eligible (no flap, no re-settle) and now reports the NEW address,
        // so a genuine address change still triggers exactly one reload downstream.
        assert_eq!(
            e.eligible(t1),
            vec![bridge.to_string()],
            "same node under a new address stays eligible; endpoint tracks the address"
        );
        let views = e.status_views(t1);
        assert_eq!(views.len(), 1, "one worker tracked, not two");
        assert_eq!(views[0].node_id, node.to_hex());
        assert_eq!(views[0].status, WorkerStatus::Eligible);
        assert_eq!(
            views[0].flaps_in_window, 0,
            "an address change is not a flap"
        );
    }
}
