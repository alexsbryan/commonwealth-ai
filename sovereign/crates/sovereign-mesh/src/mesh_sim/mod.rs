// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tier 1 — `mesh-sim`: a seeded, deterministic mesh that runs the
//! **real** routing decision at thousands of scenarios per second.
//!
//! `SCHEDULER_QUALITY.md` §5, Phase 1 S0. The unlock is that the
//! scheduling decision is a pure function ([`crate::scheduler_core`])
//! and the expensive part — generating tokens — is exactly the part
//! that does not affect it. So arm 0 here is not a model of the
//! scheduler; it *is* the scheduler, handed simulated beliefs.
//!
//! ## What is real and what is modelled
//!
//! | real (production code, called directly) | modelled (this module) |
//! |---|---|
//! | the OICP scorer (`oicp-types::score_with_adjustments`) | service time |
//! | claim matching, tie-breaks, the strictly-beats-local filter | gossip propagation delay |
//! | the offload gate (`oicp_select::offload_eligible`) | manifest-cache ageing |
//! | observation feedback (`scheduler_core::observe_*`, the throughput EWMA) | arrival process |
//! | the decision + outcome record vocabulary (`decision_log`) | queueing |
//!
//! The right-hand column is where a Tier-1 number can be wrong. That
//! is what the §5 calibration contract is for: the sim does not need
//! to predict latency, it needs to predict *decisions*.
//!
//! ## Three modelling assumptions worth naming
//!
//! 1. **Published load is total load.** A node gossips its whole
//!    in-flight count, including requests it is serving *for peers*.
//!    That is the documented intent (`MESH_LOAD_AWARENESS.md`,
//!    `AppState::current_local_in_flight`). Whether production
//!    achieves it on the inbound path is worth confirming from a P1
//!    capture — every bump site for the published counter currently
//!    lives in `peer_inference.rs`, the *outbound* path.
//! 2. **Gossip age is measured from receipt.** `gossip_last_seen_unix`
//!    is when the record arrived, not when its contents were true, so
//!    the recorded age *understates* real staleness by the
//!    propagation delay — in the sim as in production. [`ServedFact`]
//!    keeps both numbers so the gap is reportable rather than
//!    assumed.
//! 3. **Quarantine cooldowns run on wall time.** `PeerHealthTracker`
//!    holds `Instant`s internally. Arm 0 produces no failures so it
//!    never fires, but an F4 arm (Phase 2 step 4) will need a clock
//!    seam in that type before its cooldowns mean anything here.
//!
//! ## Determinism discipline
//!
//! No wall clock, no thread scheduling, no map-iteration in any
//! decision path. Environment randomness (gossip delay) and policy
//! randomness (two-choices sampling) draw from **separate** streams,
//! so switching arms cannot perturb the world the arms are compared
//! in. Two runs of the same (scenario, arm, seed) produce identical
//! record streams.

pub mod rng;
pub mod scenario;
pub mod scoreboard;

use std::collections::{BinaryHeap, HashMap, VecDeque};

use commonwealth_core::peer_health::PeerHealthTracker;
use sovereign_core::oicp::{
    BenchmarkResult, InferenceRequirements, NodeObservations, ProviderManifest,
};

use crate::decision_log::{
    DecisionBuilder, DecisionEvent, DecisionPath, RequestFacts, RoutingOutcome, ServedBy, Verdict,
    DECISION_LOG_SCHEMA,
};
use crate::oicp_select::offload_eligible;
use crate::scheduler_core::{
    self, LocalCandidateView, PeerCandidateView, PeerManifestView, RankInputs, RankObjective,
};
use crate::throughput_tracking::apply_throughput_observation;

use rng::Rng;
use scenario::{Arrival, RequestClass, Scenario};

/// Knobs that describe the *environment*, not the policy. Defaults
/// are the production constants, cited per field.
#[derive(Debug, Clone)]
pub struct SimConfig {
    /// Anti-entropy period. `gossip.rs:57` — 10s.
    pub gossip_interval_ms: u64,
    /// Extra anti-entropy rounds a value may take to reach a given
    /// peer. Full-mesh propagation at N=12 takes several rounds; this
    /// is what turns a 10s period into the 10–30s staleness F1 is
    /// about.
    pub gossip_max_extra_rounds: u64,
    /// Peer-manifest cache TTL. `peer_inference.rs:63` — 60s.
    pub manifest_ttl_ms: u64,
    /// Concurrent requests a node serves before queueing. One,
    /// because a slot decodes one sequence at a time.
    pub slots_per_node: usize,
    /// Maximum multiplicative error between the rate a node
    /// **advertises** in its `BenchmarkResult` and the rate it
    /// actually serves at. `0.0` = a perfect rate card.
    ///
    /// **This knob exists to price the harness's most flattering
    /// assumption, and [`Arm::PredictedTime`] should not be read
    /// without it.** In this sim a node's advertised benchmark is built
    /// from the same [`scenario::Hardware`] the service-time model
    /// consumes, so at `0.0` the rate card is *exact truth by
    /// construction*. A predicted-time decider then has **no model
    /// error at all** — its only error is the queue-count substitution
    /// — and its efficiency ratio reads better than any real fleet
    /// could deliver. That is a property of the harness, not a finding
    /// about the objective.
    ///
    /// The error is **per node and two-sided** (some over-advertise,
    /// some under-advertise), because a *uniform* bias would scale
    /// every candidate together and barely disturb the ranking —
    /// measuring nothing. It moves both objectives, since the product
    /// reads the same benchmark through `throughput_factor`, so the
    /// comparison stays fair.
    pub advertised_rate_error: f32,
    /// Seconds of model-load time per GB of weights. `0.0` = every node
    /// starts with its model already resident, which is what the sim
    /// assumed before this existed.
    ///
    /// Above zero, a node starts **cold**: its manifest advertises
    /// `loaded: false` plus an `estimated_load_time_sec` of
    /// `size_gb × this`, and the first request it serves pays that time
    /// before service begins. After that the slot is warm for
    /// everything behind it — which is why
    /// `predicted_time::predict` adds the load term rather than
    /// multiplying it by the queue.
    ///
    /// This exists because the objective was pricing a real cost at
    /// zero: paging in a 21GB model dwarfs every other addend, and
    /// nothing in the harness charged for it, so the arm could not have
    /// found the mistake on its own.
    ///
    /// **Known simplification:** `build_peer_views` clones the peer's
    /// *current* manifest while reporting a cache age, so residency is
    /// visible to a decider sooner than a real 60s-TTL cache would
    /// allow. That biases toward the decider being right about
    /// residency, so any load-related finding here is a lower bound on
    /// the real cost.
    pub model_load_sec_per_gb: f64,
}

/// What a node counts when it gossips its in-flight number.
///
/// Not a policy knob — a **model of production that is not yet
/// confirmed**, which is why it is an arm rather than a constant.
/// `MESH_LOAD_AWARENESS.md` and `AppState::current_local_in_flight`
/// document the intent as [`Total`](PublishedLoad::Total), but every
/// bump site for the published counter
/// (`peer_inference.rs::enter_local_total`) sits in the joiner-side
/// provider — the *outbound* path. Whether a request arriving from a
/// peer also passes through it decides which of these two variants
/// production actually implements, and that question is answerable
/// only with two daemons.
///
/// So the arm is a sensitivity test taken *first*: if arm 0's numbers
/// barely move between the two, the audit is not worth two daemons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishedLoad {
    /// Everything on this node's slot and queue, whoever originated
    /// it. The documented intent.
    Total,
    /// Only work this node originated itself — inbound peer requests
    /// are invisible to the counter. The failure mode this models is
    /// specific: a node saturated by peer traffic gossips near-zero
    /// load, reads as idle, and wins more of it.
    OutboundOnly,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            gossip_interval_ms: 10_000,
            gossip_max_extra_rounds: 2,
            manifest_ttl_ms: 60_000,
            slots_per_node: 1,
            // Perfect rate cards and pre-loaded models by default, so
            // every number recorded before these knobs existed still
            // reproduces exactly.
            advertised_rate_error: 0.0,
            model_load_sec_per_gb: 0.0,
        }
    }
}

/// A policy arm. Arm 0 is as-implemented; everything else is a
/// candidate change that must earn its landing here first
/// (`SCHEDULER_QUALITY.md` §6: "behavioural work goes INTO the sim as
/// arms, not into production first").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// Arm 0. The production decision on production's beliefs.
    AsImplemented,
    /// Arm 0 with the staleness removed: the gossiped in-flight count
    /// is the peer's *true* current count. Isolates F1 — the gap
    /// between this and arm 0 is the cost of dead time, and nothing
    /// else changes.
    FreshSignals,
    /// Arm 0, but the decider samples two of the eligible peers and
    /// takes the better rather than always taking the argmax.
    /// Isolates F5 — deterministic argmax over a shared signal is a
    /// herd generator.
    TwoChoices,
    /// Both, to show whether they compose or overlap.
    FreshTwoChoices,
    /// Arm 0 with every decider's peer observations pre-seeded at
    /// `COLD_START_SAMPLES` — the counterfactual in which every
    /// decider already has history with every peer. Prices F7.
    ///
    /// **Not a single-factor isolation, and the distinction matters.**
    /// `samples` is one number feeding three scorer terms, so seeding
    /// it lifts everything the scorer withholds from a stranger:
    ///
    ///   - `cold_start_weight` — 0.7 → 1.0. This is F7 proper.
    ///   - `throughput_factor`'s *source* — a peer past
    ///     `THROUGHPUT_OBSERVATION_THRESHOLD` (5) with a warmed EWMA
    ///     is scored on observed decode rate instead of the benchmark
    ///     estimate. Identical at `t = 0` (the EWMA is still zero) and
    ///     divergent as soon as anything completes.
    ///   - `effective_affinity`'s observation weight — inert here,
    ///     because no arm produces failures, so the blend is
    ///     `1 − w·0 = 1` at any sample count.
    ///
    /// That is not a defect of the arm: `samples` is *the* "do I know
    /// this peer" signal, and a peer that is never dispatched to keeps
    /// all of these penalties at once. The counterfactual worth
    /// pricing is therefore the whole stranger penalty, not one term
    /// of it. `tests/mesh_sim_scoreboard.rs` prints the throughput
    /// source mix per arm so the two effects stay separable in the
    /// report.
    WarmStart,
    /// [`WarmStart`](Arm::WarmStart) **and** fresh signals — the arm
    /// that tells you *why* warm-start hurts.
    ///
    /// Warm-start alone is much worse than arm 0, and the obvious
    /// explanation is F1: lifting the cold-start floor unlocks
    /// offloads, and a decider cannot see the queue it is offloading
    /// into. That is a mechanism, and a mechanism asserted is not a
    /// mechanism measured. This arm discriminates. If the damage is
    /// F1's, it disappears when the signal is fresh; if warm-start is
    /// still worse here, offloading is simply unprofitable in this
    /// fleet and F1 was the wrong culprit.
    FreshWarmStart,
    /// Arm 0 with [`PublishedLoad::OutboundOnly`] — the counterfactual
    /// in which the gossiped counter misses inbound peer work.
    /// Isolates the load-attribution question before it costs two
    /// daemons.
    OutboundOnlyLoad,
    /// **§4.1.** Rank on predicted time-to-answer
    /// (`queue + prefill + decode + rtt`) instead of on the product of
    /// dimensionless multipliers. The feasibility half is untouched —
    /// same hard gates, same candidate records, same scores recorded —
    /// so the delta against arm 0 is a delta of *objective* and of
    /// nothing else. See [`crate::predicted_time`].
    ///
    /// Read it against [`Oracle`](Arm::Oracle), which minimises the
    /// same quantity with perfect knowledge of every queue. The two
    /// gaps price different things, and that is the point of having
    /// this arm between them:
    ///
    ///   - `oracle − predicted` — the cost of **imperfect
    ///     information**. The only term the two disagree on is the
    ///     queue: the oracle knows `backlog_ms` exactly, a decider
    ///     knows a gossiped in-flight *count*.
    ///   - `predicted − arm 0` — the cost of a **wrong objective**,
    ///     holding information constant.
    ///
    /// Arm 0 and the oracle were both already here; this is the
    /// missing middle term, and it is the one that says which of the
    /// two problems to fix first.
    ///
    /// **Bound on what may be claimed from it.** A predicted time is
    /// far more sensitive to this module's hand-chosen service-time
    /// model than a ranking is — it consumes `pp_tok_s` / `tg_tok_s`
    /// directly, where the product objective flattens them through a
    /// clamp (F3). The qualitative result is robust: *does it decline
    /// the offloads `WarmStart` exposed?* The latency **magnitudes**
    /// are not quotable until S1 runs against a capture from real
    /// hardware.
    PredictedTime,
    /// [`PredictedTime`](Arm::PredictedTime) **under
    /// [`PublishedLoad::OutboundOnly`]** — the composition that was
    /// missing when the arm first landed, and it is the one that
    /// matters most.
    ///
    /// The two objectives consume `in_flight` very differently. The
    /// product passes it through `load_penalty`, a **bounded**
    /// multiplier: a mis-attributed count moves the score a little. The
    /// predicted time **multiplies it by a service time**, so a count
    /// that misses inbound peer work is a *first-order* error that
    /// scales with the queue. F2 should therefore hurt this objective
    /// more than it hurts arm 0, and a scheduler that is confidently
    /// wrong is worse than one that is vaguely wrong.
    ///
    /// If that holds, the two-daemon inbound-load audit is not just
    /// earned (it already was, at +126%..+584% for arm 0) but a
    /// **prerequisite** for landing §4.1 — because the objective's
    /// accuracy is exactly what it trades the product's fudge factors
    /// for.
    PredictedTimeOutboundOnly,
    /// Not a policy anyone could implement: assigns each request to
    /// whichever node would finish it soonest, with perfect knowledge
    /// of every queue. The denominator of the efficiency ratio.
    ///
    /// "Clairvoyant" in the online sense — it knows the present
    /// exactly but not the future, so it bounds what any
    /// current-state policy could achieve rather than being a global
    /// optimum.
    Oracle,
}

impl Arm {
    pub fn label(&self) -> &'static str {
        match self {
            Arm::AsImplemented => "as-implemented",
            Arm::FreshSignals => "fresh-signals",
            Arm::TwoChoices => "two-choices",
            Arm::FreshTwoChoices => "fresh+two-choices",
            Arm::WarmStart => "warm-start",
            Arm::FreshWarmStart => "fresh+warm-start",
            Arm::OutboundOnlyLoad => "outbound-only-load",
            Arm::PredictedTime => "predicted-time",
            Arm::PredictedTimeOutboundOnly => "predicted-time+outbound-only",
            Arm::Oracle => "oracle",
        }
    }

    /// Which ranking objective this arm hands to
    /// `scheduler_core::rank`. Everything but the predicted-time arms
    /// ranks the way production does today.
    pub(crate) fn objective(&self) -> RankObjective {
        match self {
            Arm::PredictedTime | Arm::PredictedTimeOutboundOnly => RankObjective::PredictedTime,
            _ => RankObjective::Product,
        }
    }

    fn fresh_signals(&self) -> bool {
        matches!(
            self,
            Arm::FreshSignals | Arm::FreshTwoChoices | Arm::FreshWarmStart
        )
    }

    fn two_choices(&self) -> bool {
        matches!(self, Arm::TwoChoices | Arm::FreshTwoChoices)
    }

    /// Whether deciders start already believing they have completed
    /// the cold-start ramp for every peer.
    pub fn warm_start(&self) -> bool {
        matches!(self, Arm::WarmStart | Arm::FreshWarmStart)
    }

    /// What a node counts when it gossips its load.
    pub fn published_load(&self) -> PublishedLoad {
        match self {
            Arm::OutboundOnlyLoad | Arm::PredictedTimeOutboundOnly => PublishedLoad::OutboundOnly,
            _ => PublishedLoad::Total,
        }
    }
}

/// Every arm worth reporting side by side. `PredictedTime` sits last
/// before `Oracle` because that is the order the §4.1 reading wants:
/// arm 0 → predicted → perfect information.
pub const ALL_ARMS: [Arm; 10] = [
    Arm::AsImplemented,
    Arm::FreshSignals,
    Arm::TwoChoices,
    Arm::FreshTwoChoices,
    Arm::WarmStart,
    Arm::FreshWarmStart,
    Arm::OutboundOnlyLoad,
    Arm::PredictedTime,
    Arm::PredictedTimeOutboundOnly,
    Arm::Oracle,
];

/// What one decider believes about one peer, as of the last gossip
/// record it received.
#[derive(Debug, Clone)]
struct Belief {
    in_flight: u32,
    availability: Option<f32>,
    /// When this node received the record — what
    /// `gossip_last_seen_unix` reports.
    received_at_ms: u64,
    /// When the value was actually true. Invisible to the scorer; the
    /// sim keeps it to measure how far the recorded age understates
    /// real staleness.
    measured_at_ms: u64,
}

/// A unit of work moving through the mesh.
#[derive(Debug, Clone)]
struct Job {
    seq: u64,
    origin: usize,
    server: usize,
    decision_id: String,
    oicp_request_id: String,
    context_tokens: u32,
    output_tokens: u32,
    class: RequestClass,
    /// When the user asked.
    arrived_ms: u64,
    /// When service actually began (after queueing).
    started_ms: Option<u64>,
    /// Round-trip cost of the hop; 0 when served locally.
    rtt_ms: u32,
    /// Model-load time this particular job paid, if it was the one that
    /// found the node cold. Tracked per job so `on_service_done` can
    /// attribute it to **TTFT** rather than to decode — charging it to
    /// decode would understate observed `tg_tok_s` and teach the
    /// throughput EWMA that a cold node is permanently slow, which is an
    /// artifact of the accounting and not of the hardware.
    load_paid_ms: u64,
    /// What this request would have cost had it stayed local, given
    /// the origin's true queue at decision time. Feeds the waste
    /// metric.
    local_alternative_ms: u64,
    model_id: String,
    facts: DispatchFacts,
}

/// What the decision knew, carried alongside the job so the
/// scoreboard can ask *why* a dispatch went where it did.
#[derive(Debug, Clone, Copy, Default)]
struct DispatchFacts {
    /// True vs recorded age of the load signal the decision used.
    true_signal_age_ms: Option<u64>,
    recorded_signal_age_ms: Option<u64>,
    /// How many peers strictly beat local — the set a sampling policy
    /// would have had to choose from. A singleton means two-choices
    /// has nothing to sample.
    eligible_peers: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Arrival(usize),
    ServiceDone { node: usize, job_seq: u64 },
    GossipTick,
    GossipDeliver {
        from: usize,
        to: usize,
        in_flight: u32,
        /// Availability in thousandths, so the event stays `Eq`.
        availability_milli: Option<u32>,
        measured_at_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scheduled {
    at_ms: u64,
    seq: u64,
    kind: EventKind,
}

impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reversed: `BinaryHeap` is a max-heap and we want earliest
        // first. Ties break on insertion order — never on address or
        // hash, because determinism depends on it.
        other
            .at_ms
            .cmp(&self.at_ms)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One node's truth and one node's beliefs, side by side. The gap
/// between the two halves is the whole subject of this simulator.
struct SimNode {
    name: String,
    node_id_hex: String,
    manifest: ProviderManifest,
    benchmark: Option<BenchmarkResult>,
    availability: Option<f32>,
    pp_tok_s: f32,
    tg_tok_s: f32,
    /// Time this node needs to page its model in. Zero when
    /// `model_load_sec_per_gb` is off.
    load_ms: u64,

    // ── truth ──
    running: Vec<Job>,
    queue: VecDeque<Job>,
    /// Whether the model is currently paged in. Flips once, on the
    /// first request served, and is mirrored into `manifest`'s
    /// `ModelStatus::loaded` so a decider can see it.
    resident: bool,

    // ── beliefs ──
    local_obs: NodeObservations,
    peer_obs: Vec<NodeObservations>,
    peer_health: PeerHealthTracker,
    /// `fetched_at_ms` per peer; absent = never fetched.
    manifest_fetched_ms: HashMap<usize, u64>,
    gossip: HashMap<usize, Belief>,
}

impl SimNode {
    /// True total load: everything on the slot and in the queue,
    /// whoever originated it. This is *truth*, and the fresh-signals
    /// arm reads it directly — only the gossiped number is allowed to
    /// be an approximation.
    fn in_flight(&self) -> u32 {
        (self.running.len() + self.queue.len()) as u32
    }

    /// The number this node puts on the wire, under a given
    /// attribution policy. `self_idx` is the node's own index, which
    /// is what "locally originated" is measured against.
    fn published_in_flight(&self, self_idx: usize, policy: PublishedLoad) -> u32 {
        match policy {
            PublishedLoad::Total => self.in_flight(),
            PublishedLoad::OutboundOnly => self
                .running
                .iter()
                .chain(self.queue.iter())
                .filter(|j| j.origin == self_idx)
                .count() as u32,
        }
    }

    /// Server-side time to first token: the prompt is processed
    /// before anything comes back.
    fn ttft_ms(&self, context_tokens: u32) -> u64 {
        (context_tokens as f64 / self.pp_tok_s as f64 * 1000.0) as u64
    }

    fn service_ms(&self, context_tokens: u32, output_tokens: u32) -> u64 {
        self.ttft_ms(context_tokens) + (output_tokens as f64 / self.tg_tok_s as f64 * 1000.0) as u64
    }

    /// Milliseconds of work already committed to this node: what is
    /// left of the running job plus every queued job in full.
    fn backlog_ms(&self, now_ms: u64) -> u64 {
        let running: u64 = self
            .running
            .iter()
            .map(|j| {
                let finish = j.started_ms.unwrap_or(now_ms)
                    + self.service_ms(j.context_tokens, j.output_tokens);
                finish.saturating_sub(now_ms)
            })
            .sum();
        let queued: u64 = self
            .queue
            .iter()
            .map(|j| self.service_ms(j.context_tokens, j.output_tokens))
            .sum();
        // A cold node owes its model load before anything above can
        // start. Once, not per job — same shape as the load term in
        // `predicted_time::predict`, so the oracle and the predictor
        // are minimising the same quantity.
        let load = if self.resident { 0 } else { self.load_ms };
        load + running + queued
    }
}

/// Everything one run produces.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub scenario: String,
    pub arm: Arm,
    pub seed: u64,
    /// Decisions and outcomes, in the same vocabulary a production
    /// capture uses — so every scoreboard metric computed from this
    /// field is also computable from a real trace.
    pub records: Vec<DecisionEvent>,
    /// Ground truth the record stream cannot carry.
    pub truth: Vec<ServedFact>,
    pub node_names: Vec<String>,
    /// `peer_samples[origin][peer]` at end of run — how much history
    /// each decider ever accumulated about each peer.
    ///
    /// `cold_start_weight`'s doc comment states the ramp exists "so
    /// new peers still receive routable traffic (otherwise they'd
    /// never accumulate history)". This matrix is how that claim gets
    /// checked rather than assumed: a column of zeros is a peer no
    /// decider ever tried.
    pub peer_samples: Vec<Vec<u32>>,
}

/// Per-request ground truth. Separate from the record stream on
/// purpose: everything here is knowable only inside a simulation, so
/// a metric that a production capture must also support may not
/// depend on it.
#[derive(Debug, Clone)]
pub struct ServedFact {
    pub decision_id: String,
    pub origin: usize,
    pub server: usize,
    pub class: RequestClass,
    pub total_ms: u64,
    pub ttft_ms: u64,
    /// How long this request sat behind others before service began.
    /// Separating queue wait from service time is what distinguishes
    /// "the peer was slower" from "the peer was busy".
    pub queue_wait_ms: u64,
    /// What staying local would have cost, measured against the
    /// origin's true queue at decision time.
    pub local_alternative_ms: u64,
    pub dispatched_at_ms: u64,
    /// True staleness of the load signal the decision used, in ms.
    /// `None` when the decision scored no gossiped peer.
    pub true_signal_age_ms: Option<u64>,
    /// Age the record claimed for that same signal.
    pub recorded_signal_age_ms: Option<u64>,
    /// Peers that strictly beat local at decision time.
    pub eligible_peers: usize,
}

struct Sim {
    cfg: SimConfig,
    arm: Arm,
    nodes: Vec<SimNode>,
    rtt_ms: Vec<Vec<u32>>,
    events: BinaryHeap<Scheduled>,
    now_ms: u64,
    seq: u64,
    /// Environment randomness (gossip propagation). Never consumed by
    /// a policy, so every arm sees the same world.
    world_rng: Rng,
    /// Policy randomness (two-choices sampling). Never consumed by
    /// the environment, so an arm that draws from it cannot shift the
    /// gossip schedule.
    policy_rng: Rng,
    records: Vec<DecisionEvent>,
    truth: Vec<ServedFact>,
}

/// Run one (scenario, arm, seed) to completion under the default
/// environment.
pub fn run(scenario: &Scenario, arm: Arm, seed: u64) -> RunReport {
    run_with(scenario, arm, seed, SimConfig::default())
}

pub fn run_with(scenario: &Scenario, arm: Arm, seed: u64, cfg: SimConfig) -> RunReport {
    let mut sim = Sim::new(scenario, arm, seed, cfg);
    sim.seed_events(scenario);
    sim.drive(scenario);
    let peer_samples = sim
        .nodes
        .iter()
        .map(|n| n.peer_obs.iter().map(|o| o.samples).collect())
        .collect();
    RunReport {
        scenario: scenario.name.clone(),
        arm,
        seed,
        records: sim.records,
        truth: sim.truth,
        node_names: scenario.nodes.iter().map(|n| n.name.clone()).collect(),
        peer_samples,
    }
}

impl Sim {
    fn new(scenario: &Scenario, arm: Arm, seed: u64, cfg: SimConfig) -> Self {
        let n = scenario.nodes.len();
        // A THIRD stream, and deliberately not `world_rng`: drawing
        // rate-card errors from the world stream at construction time
        // would shift every later gossip draw, so an
        // `advertised_rate_error: 0.0` run would stop reproducing the
        // runs recorded before this knob existed.
        let mut rate_rng = Rng::new(seed ^ 0xC0DE_FACE_5A17_3E11);
        let rate_error = cfg.advertised_rate_error.max(0.0);
        let load_per_gb = cfg.model_load_sec_per_gb.max(0.0);
        let nodes = scenario
            .nodes
            .iter()
            .enumerate()
            .map(|(i, spec)| SimNode {
                name: spec.name.clone(),
                // Deterministic stand-in for a real 16-byte node id.
                node_id_hex: format!("{:032x}", i as u128 + 1),
                // A cold node ADVERTISES that it is cold, and how long
                // it would take — which is the only reason a decider
                // can price the load at all.
                manifest: {
                    let mut m = spec.manifest();
                    if load_per_gb > 0.0 {
                        if let Some(model) = m.models.first_mut() {
                            model.status.loaded = false;
                            model.status.estimated_load_time_sec =
                                Some((spec.size_gb as f64 * load_per_gb).round() as u32);
                        }
                    }
                    m
                },
                load_ms: (spec.size_gb as f64 * load_per_gb * 1000.0) as u64,
                resident: load_per_gb <= 0.0,
                // What the node CLAIMS it can do, which is only the
                // truth when `advertised_rate_error` is zero. Two-sided
                // and multiplicative-symmetric: the factor lands in
                // `[1/(1+e), 1+e]`, so a node is as likely to
                // under-sell itself as to over-sell.
                benchmark: spec.benchmark().map(|mut b| {
                    if rate_error > 0.0 {
                        let u = rate_rng.next_f64() as f32;
                        let factor = (1.0 + rate_error).powf(2.0 * u - 1.0);
                        b.pp_tok_s *= factor;
                        b.tg_tok_s *= factor;
                    }
                    b
                }),
                availability: spec.availability,
                pp_tok_s: spec.hardware.pp_tok_s,
                tg_tok_s: spec.hardware.tg_tok_s,
                running: Vec::new(),
                queue: VecDeque::new(),
                // Production seeds local samples above the cold-start
                // threshold: a node always knows itself.
                local_obs: NodeObservations {
                    samples: sovereign_core::oicp::COLD_START_SAMPLES * 2,
                    ..Default::default()
                },
                // F7 arm: a warm-started decider begins already
                // believing it has finished the cold-start ramp for
                // every peer, so `cold_start_weight` is 1.0 from the
                // first decision instead of 0.7.
                peer_obs: vec![
                    NodeObservations {
                        samples: if arm.warm_start() {
                            sovereign_core::oicp::COLD_START_SAMPLES
                        } else {
                            0
                        },
                        ..Default::default()
                    };
                    n
                ],
                peer_health: PeerHealthTracker::new(),
                manifest_fetched_ms: HashMap::new(),
                gossip: HashMap::new(),
            })
            .collect();
        Self {
            cfg,
            arm,
            nodes,
            rtt_ms: scenario.rtt_ms.clone(),
            events: BinaryHeap::new(),
            now_ms: 0,
            seq: 0,
            world_rng: Rng::new(seed ^ 0xA5A5_5A5A_C3C3_3C3C),
            policy_rng: Rng::new(seed ^ 0x1357_9BDF_2468_ACE0),
            records: Vec::new(),
            truth: Vec::new(),
        }
    }

    fn push(&mut self, at_ms: u64, kind: EventKind) {
        self.seq += 1;
        let seq = self.seq;
        self.events.push(Scheduled { at_ms, seq, kind });
    }

    fn seed_events(&mut self, scenario: &Scenario) {
        for (idx, arrival) in scenario.arrivals.iter().enumerate() {
            self.push(arrival.at_ms, EventKind::Arrival(idx));
        }
        let mut t = self.cfg.gossip_interval_ms;
        while t <= scenario.duration_ms {
            self.push(t, EventKind::GossipTick);
            t += self.cfg.gossip_interval_ms;
        }
    }

    fn drive(&mut self, scenario: &Scenario) {
        while let Some(ev) = self.events.pop() {
            self.now_ms = ev.at_ms;
            match ev.kind {
                EventKind::Arrival(idx) => self.on_arrival(&scenario.arrivals[idx]),
                EventKind::ServiceDone { node, job_seq } => self.on_service_done(node, job_seq),
                EventKind::GossipTick => self.on_gossip_tick(),
                EventKind::GossipDeliver {
                    from,
                    to,
                    in_flight,
                    availability_milli,
                    measured_at_ms,
                } => {
                    let received_at_ms = self.now_ms;
                    self.nodes[to].gossip.insert(
                        from,
                        Belief {
                            in_flight,
                            availability: availability_milli.map(|m| m as f32 / 1000.0),
                            received_at_ms,
                            measured_at_ms,
                        },
                    );
                }
            }
        }
    }

    // ── gossip ──────────────────────────────────────────────────

    fn on_gossip_tick(&mut self) {
        let n = self.nodes.len();
        let now = self.now_ms;
        let policy = self.arm.published_load();
        for from in 0..n {
            let in_flight = self.nodes[from].published_in_flight(from, policy);
            let availability_milli = self.nodes[from].availability.map(|a| (a * 1000.0) as u32);
            for to in 0..n {
                if to == from {
                    continue;
                }
                // Anti-entropy: a value reaches a given peer after
                // between zero and `gossip_max_extra_rounds` further
                // rounds, plus the wire.
                let extra = self.world_rng.next_u64() % (self.cfg.gossip_max_extra_rounds + 1);
                let delay = extra * self.cfg.gossip_interval_ms + self.rtt_ms[from][to] as u64;
                self.push(
                    now + delay,
                    EventKind::GossipDeliver {
                        from,
                        to,
                        in_flight,
                        availability_milli,
                        measured_at_ms: now,
                    },
                );
            }
        }
    }

    // ── arrivals and decisions ──────────────────────────────────

    fn on_arrival(&mut self, arrival: &Arrival) {
        let origin = arrival.origin;
        // The OICP envelope carries the request's size in production —
        // `MeshInferenceProvider::request_facts` reads both counts
        // straight off it — and both are hard feasibility gates in the
        // scorer (`scoring.rs:590`). Setting them here is a fidelity
        // fix that stands on its own: without it the gates never bind
        // in the sim, and §4.1 has no job size to predict.
        //
        // It cannot move arm 0, and that is checked rather than
        // asserted in prose: the gates only ever *exclude*, every
        // simulated claim advertises 32768/4000, and no arrival exceeds
        // either — see `no_arrival_is_gated_out_by_its_own_size`.
        let req = arrival
            .class
            .requirements()
            .with_context_tokens(arrival.context_tokens)
            .with_max_output_tokens(arrival.output_tokens);
        let facts = request_facts(&req, arrival);
        self.seq += 1;
        let oicp_request_id = format!("sim-{origin}-{}", self.seq);
        let rec = DecisionBuilder::new(&oicp_request_id, DecisionPath::RankedOicp, facts);

        // The production gate, called directly — which is what makes
        // "LocalOnly never crossed the wire" a property of the code
        // under test rather than of the harness.
        if !offload_eligible(&req) {
            let decision = rec.finish_at(
                Verdict::Gated {
                    gate: "not_offload_eligible".into(),
                },
                &[],
                self.now_ms,
            );
            let decision_id = decision.decision_id.clone();
            self.records.push(DecisionEvent::Decision(Box::new(decision)));
            self.dispatch(
                origin,
                origin,
                arrival,
                decision_id,
                oicp_request_id,
                DispatchFacts::default(),
            );
            return;
        }

        if self.arm == Arm::Oracle {
            let server = self.oracle_pick(arrival);
            let ranked = if server == origin {
                Vec::new()
            } else {
                vec![self.nodes[server].name.clone()]
            };
            let verdict = if ranked.is_empty() {
                Verdict::StayLocal
            } else {
                Verdict::Peers {
                    ranked: ranked.clone(),
                }
            };
            let decision = rec.finish_at(verdict, &ranked, self.now_ms);
            let decision_id = decision.decision_id.clone();
            self.records.push(DecisionEvent::Decision(Box::new(decision)));
            self.dispatch(
                origin,
                server,
                arrival,
                decision_id,
                oicp_request_id,
                DispatchFacts::default(),
            );
            return;
        }

        let views = self.build_peer_views(origin);
        let local_obs = self.local_view_observations(origin);
        let result = {
            let node = &self.nodes[origin];
            scheduler_core::rank(
                rec,
                RankInputs {
                    now_unix: self.now_ms / 1000,
                    oicp_request_id: &oicp_request_id,
                    req: &req,
                    needs_forced_choice: false,
                    objective: self.arm.objective(),
                    local: LocalCandidateView {
                        manifest: &node.manifest,
                        observations: &local_obs,
                        benchmark: node.benchmark.as_ref(),
                    },
                    peers: &views,
                },
            )
        };

        // Which of the ranked candidates the decider dispatches to.
        // Arm 0 takes the argmax — that determinism is F5.
        let chosen_view = if result.ranked.is_empty() {
            None
        } else if self.arm.two_choices() && result.ranked.len() > 1 {
            let a = self.policy_rng.below(result.ranked.len());
            let b = self.policy_rng.below(result.ranked.len());
            // `ranked` is best-first, so the better of two samples is
            // the one with the smaller index.
            Some(result.ranked[a.min(b)].0)
        } else {
            Some(result.ranked[0].0)
        };

        let decision_id = result.decision.decision_id.clone();
        let eligible_peers = result.ranked.len();
        self.records
            .push(DecisionEvent::Decision(Box::new(result.decision)));

        let (server, ages) = match chosen_view {
            Some(view_idx) => {
                let peer = views_index_to_node(origin, view_idx);
                (peer, self.signal_ages(origin, peer))
            }
            None => (origin, (None, None)),
        };
        self.dispatch(
            origin,
            server,
            arrival,
            decision_id,
            oicp_request_id,
            DispatchFacts {
                true_signal_age_ms: ages.0,
                recorded_signal_age_ms: ages.1,
                eligible_peers,
            },
        );
    }

    /// The origin's view of itself. Load is exact — a node knows its
    /// own queue with zero delay, and that asymmetry is F1.
    fn local_view_observations(&self, origin: usize) -> NodeObservations {
        let node = &self.nodes[origin];
        NodeObservations {
            in_flight: node.in_flight(),
            ..node.local_obs.clone()
        }
    }

    /// Assemble what `origin` currently believes about every peer —
    /// the *gather* half of the production selector, with HTTP
    /// replaced by a cache-age model.
    fn build_peer_views(&mut self, origin: usize) -> Vec<PeerCandidateView> {
        let now = self.now_ms;
        let ttl = self.cfg.manifest_ttl_ms;
        let fresh = self.arm.fresh_signals();
        let n = self.nodes.len();
        let mut views = Vec::with_capacity(n.saturating_sub(1));
        for peer in 0..n {
            if peer == origin {
                continue;
            }
            let rtt = self.rtt_ms[origin][peer];
            let fetched = self.nodes[origin].manifest_fetched_ms.get(&peer).copied();
            let (age_ms, from_cache) = match fetched {
                Some(at) if now.saturating_sub(at) < ttl => (now - at, true),
                _ => {
                    self.nodes[origin].manifest_fetched_ms.insert(peer, now);
                    (0, false)
                }
            };
            let (gossiped_in_flight, availability, last_seen_unix) = if fresh {
                // The counterfactual: the same scorer, told the truth.
                (
                    Some(self.nodes[peer].in_flight()),
                    self.nodes[peer].availability,
                    now / 1000,
                )
            } else {
                match self.nodes[origin].gossip.get(&peer) {
                    Some(b) => (Some(b.in_flight), b.availability, b.received_at_ms / 1000),
                    // Never heard from: no gossiped load signal at
                    // all, which is what a cold peer looks like.
                    None => (None, None, 0),
                }
            };
            views.push(PeerCandidateView {
                name: self.nodes[peer].name.clone(),
                node_id_hex: self.nodes[peer].node_id_hex.clone(),
                quarantined: self.nodes[origin]
                    .peer_health
                    .is_quarantined(&self.nodes[peer].name),
                pinned_transport: false,
                gossiped_in_flight,
                availability,
                gossip_last_seen_unix: last_seen_unix,
                benchmark: self.nodes[peer].benchmark.clone(),
                observations: self.nodes[origin].peer_obs[peer].clone(),
                manifest: Some(PeerManifestView {
                    manifest: self.nodes[peer].manifest.clone(),
                    rtt_ms: rtt,
                    age_secs: age_ms / 1000,
                    from_cache,
                }),
            });
        }
        views
    }

    /// True vs recorded age of the load signal behind a dispatch.
    fn signal_ages(&self, origin: usize, peer: usize) -> (Option<u64>, Option<u64>) {
        match self.nodes[origin].gossip.get(&peer) {
            Some(b) => (
                Some(self.now_ms.saturating_sub(b.measured_at_ms)),
                Some(self.now_ms.saturating_sub(b.received_at_ms)),
            ),
            None => (None, None),
        }
    }

    /// Perfect-information greedy: whoever finishes it soonest.
    fn oracle_pick(&self, arrival: &Arrival) -> usize {
        let mut best = arrival.origin;
        let mut best_ms = u64::MAX;
        for candidate in 0..self.nodes.len() {
            let rtt = if candidate == arrival.origin {
                0
            } else {
                self.rtt_ms[arrival.origin][candidate]
            } as u64;
            let node = &self.nodes[candidate];
            let finish = node.backlog_ms(self.now_ms)
                + node.service_ms(arrival.context_tokens, arrival.output_tokens)
                + rtt;
            if finish < best_ms {
                best_ms = finish;
                best = candidate;
            }
        }
        best
    }

    // ── dispatch and service ────────────────────────────────────

    fn dispatch(
        &mut self,
        origin: usize,
        server: usize,
        arrival: &Arrival,
        decision_id: String,
        oicp_request_id: String,
        facts: DispatchFacts,
    ) {
        let rtt = if server == origin {
            0
        } else {
            self.rtt_ms[origin][server]
        };
        // Counterfactual: what staying local would have cost, given
        // the origin's true queue right now.
        let local_alternative_ms = {
            let node = &self.nodes[origin];
            node.backlog_ms(self.now_ms)
                + node.service_ms(arrival.context_tokens, arrival.output_tokens)
        };
        self.seq += 1;
        let job = Job {
            seq: self.seq,
            origin,
            server,
            decision_id,
            oicp_request_id,
            context_tokens: arrival.context_tokens,
            output_tokens: arrival.output_tokens,
            class: arrival.class,
            arrived_ms: self.now_ms,
            started_ms: None,
            rtt_ms: rtt,
            load_paid_ms: 0,
            local_alternative_ms,
            model_id: self.nodes[server]
                .manifest
                .models
                .first()
                .map(|m| m.id.clone())
                .unwrap_or_default(),
            facts,
        };
        // Belief bookkeeping, through the same helpers production
        // uses.
        if server == origin {
            scheduler_core::observe_dispatch(&mut self.nodes[origin].local_obs);
        } else {
            scheduler_core::observe_dispatch(&mut self.nodes[origin].peer_obs[server]);
        }
        self.nodes[server].queue.push_back(job);
        self.maybe_start(server);
    }

    fn maybe_start(&mut self, node_idx: usize) {
        while self.nodes[node_idx].running.len() < self.cfg.slots_per_node {
            let Some(mut job) = self.nodes[node_idx].queue.pop_front() else {
                return;
            };
            // The first request to a cold node pays the model load;
            // everything behind it inherits a warm slot. Flipping
            // `manifest`'s `loaded` here is what lets a decider stop
            // charging for it.
            let load = if self.nodes[node_idx].resident {
                0
            } else {
                let n = &mut self.nodes[node_idx];
                n.resident = true;
                if let Some(model) = n.manifest.models.first_mut() {
                    model.status.loaded = true;
                }
                n.load_ms
            };
            let service =
                load + self.nodes[node_idx].service_ms(job.context_tokens, job.output_tokens);
            job.load_paid_ms = load;
            job.started_ms = Some(self.now_ms);
            let job_seq = job.seq;
            self.nodes[node_idx].running.push(job);
            self.push(
                self.now_ms + service,
                EventKind::ServiceDone {
                    node: node_idx,
                    job_seq,
                },
            );
        }
    }

    fn on_service_done(&mut self, node_idx: usize, job_seq: u64) {
        let Some(pos) = self.nodes[node_idx]
            .running
            .iter()
            .position(|j| j.seq == job_seq)
        else {
            return;
        };
        let job = self.nodes[node_idx].running.remove(pos);
        let started = job.started_ms.unwrap_or(job.arrived_ms);
        // Load is pre-first-token time, so it belongs to TTFT.
        let server_ttft = self.nodes[node_idx].ttft_ms(job.context_tokens) + job.load_paid_ms;
        let queue_wait = started.saturating_sub(job.arrived_ms);
        let ttft_ms = queue_wait + server_ttft + job.rtt_ms as u64;
        let total_ms = self.now_ms.saturating_sub(job.arrived_ms) + job.rtt_ms as u64;

        // Feedback into the decider's beliefs — the same EWMA and
        // failure-rate arithmetic production runs, so a peer that
        // served slowly is scored lower next time in the sim exactly
        // as it would be in the field.
        let decode_ms = total_ms.saturating_sub(ttft_ms).max(1);
        let observed_tg = job.output_tokens as f64 / (decode_ms as f64 / 1000.0);
        let origin = job.origin;
        let server = job.server;
        if server == origin {
            scheduler_core::observe_success(&mut self.nodes[origin].local_obs);
            apply_throughput_observation(
                &mut self.nodes[origin].local_obs,
                Some(ttft_ms as f64),
                Some(observed_tg),
            );
        } else {
            scheduler_core::observe_success(&mut self.nodes[origin].peer_obs[server]);
            apply_throughput_observation(
                &mut self.nodes[origin].peer_obs[server],
                Some(ttft_ms as f64),
                Some(observed_tg),
            );
            let peer_name = self.nodes[server].name.clone();
            self.nodes[origin].peer_health.record_success(&peer_name);
        }

        let served_by = if server == origin {
            ServedBy::LocalFallback {
                model_id: job.model_id.clone(),
            }
        } else {
            ServedBy::Peer {
                name: self.nodes[server].name.clone(),
                node_id: Some(self.nodes[server].node_id_hex.clone()),
                model_id: job.model_id.clone(),
            }
        };
        self.records
            .push(DecisionEvent::Outcome(Box::new(RoutingOutcome {
                schema: DECISION_LOG_SCHEMA.to_string(),
                decision_id: job.decision_id.clone(),
                oicp_request_id: job.oicp_request_id.clone(),
                ts_unix_ms: self.now_ms,
                served_by,
                attempt_index: 0,
                ttft_ms: Some(ttft_ms as f64),
                total_ms: Some(total_ms as f64),
                output_tokens: Some(job.output_tokens as u64),
                shed: false,
                error: None,
                failovers: Vec::new(),
            })));
        self.truth.push(ServedFact {
            decision_id: job.decision_id,
            origin,
            server,
            class: job.class,
            total_ms,
            ttft_ms,
            queue_wait_ms: queue_wait,
            local_alternative_ms: job.local_alternative_ms,
            dispatched_at_ms: job.arrived_ms,
            true_signal_age_ms: job.facts.true_signal_age_ms,
            recorded_signal_age_ms: job.facts.recorded_signal_age_ms,
            eligible_peers: job.facts.eligible_peers,
        });

        self.maybe_start(node_idx);
    }
}

/// Views skip the origin, so view index `i` is node `i` when
/// `i < origin` and node `i + 1` otherwise.
fn views_index_to_node(origin: usize, view_idx: usize) -> usize {
    if view_idx < origin {
        view_idx
    } else {
        view_idx + 1
    }
}

/// Mirrors `MeshInferenceProvider::request_facts` — same `{:?}`
/// rendering, so a sim record and a production record describe a
/// request the same way and the two streams stay comparable.
fn request_facts(req: &InferenceRequirements, arrival: &Arrival) -> RequestFacts {
    RequestFacts {
        capability_hint: req.effective_hint().to_string(),
        latency_class: format!("{:?}", req.effective_latency_class()),
        sharding: format!("{:?}", req.sharding()),
        context_tokens: Some(arrival.context_tokens),
        max_output_tokens: Some(arrival.output_tokens),
        preferred_speed: "Slow".into(),
        explicit_model_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_log::DecisionEvent;

    fn outcomes(report: &RunReport) -> Vec<&RoutingOutcome> {
        report
            .records
            .iter()
            .filter_map(|e| match e {
                DecisionEvent::Outcome(o) => Some(&**o),
                _ => None,
            })
            .collect()
    }

    fn decisions(report: &RunReport) -> Vec<&crate::decision_log::RoutingDecision> {
        report
            .records
            .iter()
            .filter_map(|e| match e {
                DecisionEvent::Decision(d) => Some(&**d),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_decision_gets_exactly_one_joined_outcome() {
        let s = scenario::household_evening_12(1);
        let r = run(&s, Arm::AsImplemented, 1);
        let ds = decisions(&r);
        let os = outcomes(&r);
        assert_eq!(ds.len(), s.arrivals.len());
        assert_eq!(os.len(), ds.len(), "every request must complete");
        let ids: std::collections::HashSet<_> =
            ds.iter().map(|d| d.decision_id.clone()).collect();
        for o in &os {
            assert!(ids.contains(&o.decision_id), "outcome with no decision");
        }
    }

    #[test]
    fn a_run_is_reproducible_and_arms_share_one_world() {
        let s = scenario::household_evening_12(4);
        let a = run(&s, Arm::AsImplemented, 4);
        let b = run(&s, Arm::AsImplemented, 4);
        assert_eq!(a.truth.len(), b.truth.len());
        for (x, y) in a.truth.iter().zip(b.truth.iter()) {
            assert_eq!(x.total_ms, y.total_ms);
            assert_eq!(x.server, y.server);
        }
        // Switching the policy must not change the workload.
        let c = run(&s, Arm::TwoChoices, 4);
        assert_eq!(a.truth.len(), c.truth.len());
    }

    /// The hard invariants of §5, as assertions rather than scores.
    #[test]
    fn private_and_fast_requests_never_cross_the_wire() {
        let s = scenario::household_evening_12(9);
        for arm in ALL_ARMS {
            let r = run(&s, arm, 9);
            for fact in &r.truth {
                if matches!(fact.class, RequestClass::Private | RequestClass::Fast) {
                    assert_eq!(
                        fact.origin,
                        fact.server,
                        "{}: a {:?} request was offloaded",
                        arm.label(),
                        fact.class
                    );
                }
            }
        }
    }

    #[test]
    fn the_oracle_is_at_least_as_fast_as_the_implementation() {
        let s = scenario::household_evening_12(5);
        let mean = |r: &RunReport| -> f64 {
            r.truth.iter().map(|f| f.total_ms as f64).sum::<f64>() / r.truth.len() as f64
        };
        let arm0 = mean(&run(&s, Arm::AsImplemented, 5));
        let oracle = mean(&run(&s, Arm::Oracle, 5));
        assert!(
            oracle <= arm0,
            "oracle {oracle:.0}ms should not lose to arm 0 {arm0:.0}ms"
        );
    }

    #[test]
    fn gossip_makes_the_load_signal_stale_and_the_record_understates_it() {
        let s = scenario::household_evening_12(2);
        let r = run(&s, Arm::AsImplemented, 2);
        let aged: Vec<_> = r
            .truth
            .iter()
            .filter_map(|f| f.true_signal_age_ms.zip(f.recorded_signal_age_ms))
            .collect();
        assert!(!aged.is_empty(), "no peer-routed decision used a gossiped signal");
        assert!(
            aged.iter().any(|(t, _)| *t > 10_000),
            "no decision saw a load signal older than one gossip round"
        );
        assert!(
            aged.iter().all(|(t, rec)| rec <= t),
            "the recorded age can never exceed the true age"
        );
    }

    /// Populating the OICP envelope's token counts switched on two hard
    /// feasibility gates that had never bound in this sim. They must
    /// still not bind: every simulated claim advertises 32768 context /
    /// 4000 output, and no arrival exceeds either — so arm 0's candidate
    /// set is unchanged and the F7 pricing recorded against it stays
    /// comparable.
    ///
    /// If an arrival distribution or a claim ever changes so that the
    /// gates *do* bind, this fails loudly. That is the right outcome:
    /// silently shrinking the candidate set would move every number on
    /// the scoreboard for a reason nobody chose.
    #[test]
    fn no_arrival_is_gated_out_by_its_own_size() {
        use crate::decision_log::ExclusionReason;
        let s = scenario::household_evening_12(13);
        let r = run(&s, Arm::AsImplemented, 13);
        for d in decisions(&r) {
            for ex in &d.excluded {
                assert!(
                    !matches!(ex.reason, ExclusionReason::NoClaimMatch),
                    "peer `{}` was excluded for claim mismatch — the context/output \
                     gates now bind, so the candidate set has moved and every number \
                     derived from it needs re-baselining",
                    ex.name
                );
            }
        }
    }

    /// Wiring check for the §4.1 arm: it has to actually decide
    /// differently somewhere. If [`Arm::PredictedTime`] silently fell
    /// through to the product objective, the world is identical and the
    /// two runs would agree exactly — and every table printed from the
    /// arm would be arm 0 run twice under a different name.
    #[test]
    fn the_predicted_time_arm_decides_differently_from_arm_zero() {
        let s = scenario::household_evening_12(8);
        let offloads = |arm: Arm| -> usize {
            run(&s, arm, 8)
                .truth
                .iter()
                .filter(|f| f.origin != f.server)
                .count()
        };
        let base = offloads(Arm::AsImplemented);
        let predicted = offloads(Arm::PredictedTime);
        assert_ne!(
            base, predicted,
            "predicted-time took exactly as many offloads ({base}) as the product \
             objective — either the objective is not wired through RankInputs, or \
             the two genuinely coincide on this fleet and this check needs a sharper \
             discriminator"
        );
    }

    /// An arm may change *where* work runs and therefore the order it
    /// finishes in — but never what work arrived. Compared as a
    /// multiset of (arrival time, origin, class), because completion
    /// order is exactly the thing an arm is allowed to move.
    #[test]
    fn an_arm_changes_where_work_runs_never_what_work_arrived() {
        let s = scenario::household_evening_12(6);
        let workload = |r: &RunReport| {
            let mut w: Vec<(u64, usize, RequestClass)> = r
                .truth
                .iter()
                .map(|f| (f.dispatched_at_ms, f.origin, f.class))
                .collect();
            w.sort();
            w
        };
        let baseline = workload(&run(&s, Arm::AsImplemented, 6));
        for arm in ALL_ARMS {
            assert_eq!(
                workload(&run(&s, arm, 6)),
                baseline,
                "{} saw a different workload",
                arm.label()
            );
        }
    }
}
