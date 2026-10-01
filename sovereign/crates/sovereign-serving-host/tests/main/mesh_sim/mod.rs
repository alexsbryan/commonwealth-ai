// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tier 1 — `mesh-sim`: a seeded, deterministic mesh that runs the
//! **real** routing decision at thousands of scenarios per second.
//!
//! `SCHEDULER_QUALITY.md` §5, Phase 1 S0. The unlock is that the
//! scheduling decision is a pure function ([`sovereign_scheduler::scheduler_core`])
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

mod arm;
pub mod rng;
pub mod scenario;
pub mod scoreboard;
mod sim_drive;
mod sim_views;

pub use arm::{Arm, ALL_ARMS};

use std::collections::{BinaryHeap, HashMap, VecDeque};

use oicp_types::{
    apply_throughput_observation, BenchmarkResult, InferenceRequirements, NodeObservations,
    ProviderManifest,
};
use sovereign_scheduler::peer_health::PeerHealthTracker;

use sovereign_scheduler::decision_log::{
    DecisionBuilder, DecisionEvent, DecisionPath, RequestFacts, RoutingOutcome, ServedBy, Verdict,
    DECISION_LOG_SCHEMA,
};
use sovereign_scheduler::oicp_select::offload_eligible;
use sovereign_scheduler::predicted_time;
use sovereign_scheduler::scheduler_core::{
    self, LocalCandidateView, RankInputs, RankObjective, RankResult, VenueManifestView, VenueView,
};
use sovereign_scheduler::tier::TierFloor;

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
    /// Size in GB of the model a shipped probe would actually
    /// benchmark. `None` — the default — means each node advertises a
    /// rate card measured on the model it *serves*, which is what
    /// [`scenario::NodeSpec::benchmark`] builds and what every F10
    /// number recorded before this knob existed assumed.
    ///
    /// **This knob exists because production could not have that rate
    /// card, and the difference is a mechanism rather than a
    /// perturbation.** `run_baseline_benchmark`
    /// (`sovereign-inference/src/benchmark.rs`, deleted 2026-07-28)
    /// probed the `Speed::Fast` slot — a ~4B model — and stamped that
    /// model's size into `baseline_size_gb`. `throughput_factor` then
    /// extrapolates
    /// to whatever the peer is being scored for by scaling linearly on
    /// the size ratio (`scoring.rs:384`). When the two models are the
    /// same, as they are in this sim, the ratio is 1.0 and the
    /// extrapolation is inert. When they differ by 8× — a 2.5 GB probe
    /// standing in for a 21 GB hub — it is the dominant term in the
    /// score, and no arm recorded before this knob has ever run it.
    ///
    /// Paired with [`probe_sublinearity`](SimConfig::probe_sublinearity),
    /// which is what decides whether the extrapolation is honest.
    pub probe_baseline_size_gb: Option<f32>,
    /// How far from linear real throughput-versus-size scaling is, as
    /// the exponent β in `rate ∝ size^-β`.
    ///
    /// The extrapolation in `throughput_factor` assumes **β = 1**:
    /// halve the model, double the rate. Its own doc comment concedes
    /// the assumption is wrong ("real-world scaling is sub-linear —
    /// memory bandwidth dominates") and argues it is good enough for
    /// ranking because it preserves order across candidate sizes. That
    /// argument holds when every candidate is extrapolated from the
    /// same baseline; it does not hold here, because each node probes
    /// *its own* fast slot and the baselines differ.
    ///
    /// At β = 1 this knob is a no-op by construction, which is the
    /// control every reading of it needs. Below 1 a probe on a small
    /// model over-states how fast that hardware is per GB, so the
    /// extrapolated estimate for a large model comes out **low** — and
    /// the term is clamped to `[FLOOR, 1.0]`, so a systematic
    /// under-estimate can only push a candidate down. 0.7 is the
    /// default when the knob is armed: llama.cpp decode is roughly
    /// bandwidth-bound, which puts real β well below 1 without being
    /// close to 0.
    pub probe_sublinearity: f32,
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
    /// Maximum multiplicative error between the `size_gb` a node
    /// **advertises** on its manifest and its true weight. `0.0` = a
    /// perfectly honest fleet.
    ///
    /// The tier floor (§4.1.1) reads advertised size, and advertised
    /// size is the one input to a *quality* gate that a peer states
    /// about itself. This knob is the same instrument
    /// [`SimConfig::advertised_rate_error`] is for the rate card, and
    /// exists for the same reason: the flattering assumption has to be
    /// priced, not assumed away. Unlike the rate card it moves only
    /// candidate *banding*, never a score or a service time — so a
    /// change here is unambiguously the floor mis-classifying somebody.
    ///
    /// Two-sided and multiplicative-symmetric, so a node is as likely
    /// to under-sell itself into a lower band as to over-sell itself
    /// into the top one. The second is the adversarial direction: it is
    /// how a 4B talks its way into serving synthesis.
    pub advertised_size_error: f32,
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
            advertised_size_error: 0.0,
            model_load_sec_per_gb: 0.0,
            // Off: each node advertises a rate card for the model it
            // serves. See the field docs — this is the *sim's*
            // property, not production's.
            probe_baseline_size_gb: None,
            probe_sublinearity: 1.0,
        }
    }
}

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
    ServiceDone {
        node: usize,
        job_seq: u64,
    },
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
    /// §4.2 step 1: what a peer told us about itself on the last
    /// response it returned to *us*. Absent for a peer we have never
    /// dispatched to — which is the whole difference between this
    /// mechanism and [`Arm::FreshSignals`]'s oracle.
    ///
    /// Same [`Belief`] shape as `gossip` on purpose: the two are rival
    /// readings of one quantity, and `build_peer_views` picks between
    /// them by measurement time. Populated only under
    /// [`Arm::response_backpressure`]; every other arm leaves it empty
    /// and reproduces byte-for-byte.
    backpressure: HashMap<usize, Belief>,
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
    /// What §4.2 step 2's sampler actually did, as opposed to what its
    /// arm is named. Zeroed on every arm that does not sample.
    pub sampler: SamplerTrace,
    /// How far §4.2 step 1's piggybacked load reading reached.
    /// `peer_views_with_signal` is populated on **every** arm (it is
    /// just "did the decider have a load number"); the
    /// `*_from_response` halves are zero on arms without the mechanism.
    pub backpressure: BackpressureTrace,
}

/// How often the within-noise sampler had a choice, and how often it
/// used it.
///
/// Without this the arm is uninterpretable: `predicted-time+tier-floor`
/// and `predicted-time+tier-floor+within-noise` can land on nearly the
/// same mean either because the sampler almost never fires *or* because
/// it fires constantly and its picks are a wash. Those are opposite
/// findings and the latency column cannot tell them apart — the same
/// scoreboard-denominator trap §6 keeps collecting instances of.
///
/// It is also the operator-facing number: "how often did my scheduler
/// have two options it could not tell apart?" is answerable from a
/// production capture the moment the objective ships, because
/// `tie_band` rides on the ranking rather than on the simulation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SamplerTrace {
    /// Decisions that reached the sampler with at least one ranked peer.
    pub decisions: u64,
    /// …of those, how many had a band of two or more — the sampler's
    /// opportunity rate.
    pub band_at_least_two: u64,
    /// …of those, how many ended somewhere other than the argmax. The
    /// difference between this and `band_at_least_two` is the draw
    /// landing on the leader anyway, which is not a policy no-op: it is
    /// the sampler declining, and it happens at a rate the band size
    /// sets.
    pub moved_off_argmax: u64,
    /// Sum of band sizes over `decisions`, so a mean is derivable
    /// without keeping the histogram.
    pub band_total: u64,
}

/// Which channel carried the load signal a decision consumed.
///
/// The arm's name says a mechanism is *available*; this says whether it
/// was *used*, which is a different claim and the one the latency table
/// cannot make on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoadSource {
    /// No load reading at all — a peer this decider has never heard
    /// from. Distinct from a reading of zero, and scored differently.
    #[default]
    None,
    /// The gossiped counter: the only channel before §4.2 step 1.
    Gossip,
    /// Piggybacked on a response this decider received from that peer.
    Response,
}

/// How far §4.2 step 1's mechanism actually reached.
///
/// A response can only carry news about the peer that sent it, so
/// [`Arm::ResponseBackpressure`]'s coverage is an *outcome* of the
/// routing policy rather than a property of the arm — a decider that
/// stays local learns nothing from anyone. Without these counters a
/// null result is unreadable: "piggybacking does not help" and
/// "piggybacking never fired" produce the same latency column and are
/// opposite findings.
///
/// `dispatch_*` is the load-bearing pair. `peer_views_*` is coverage
/// across the whole candidate set, which is always the weaker number
/// and would flatter a null result if quoted alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackpressureTrace {
    /// Peer views built carrying any load signal at all.
    pub peer_views_with_signal: u64,
    /// …of those, how many read a response-carried signal.
    pub peer_views_from_response: u64,
    /// Dispatches to a peer (not stay-local) that consumed a signal.
    pub dispatches_with_signal: u64,
    /// …of those, how many were decided on a response-carried signal.
    /// This is the number that says whether the mechanism was present
    /// at the moment it could change an outcome.
    pub dispatches_from_response: u64,
}

impl BackpressureTrace {
    /// Share of peer-dispatches whose load signal came off a response.
    /// `None` when nothing was dispatched with a signal, which reads
    /// differently from 0.0 and must not be collapsed into it.
    pub fn dispatch_coverage(&self) -> Option<f64> {
        (self.dispatches_with_signal > 0)
            .then(|| self.dispatches_from_response as f64 / self.dispatches_with_signal as f64)
    }

    /// Share of scored peer views whose load signal came off a
    /// response.
    pub fn view_coverage(&self) -> Option<f64> {
        (self.peer_views_with_signal > 0)
            .then(|| self.peer_views_from_response as f64 / self.peer_views_with_signal as f64)
    }
}

impl SamplerTrace {
    /// Mean band width across every decision that reached the sampler.
    /// `None` when it never ran, which reads differently from 1.0 and
    /// must not be collapsed into it.
    pub fn mean_band(&self) -> Option<f64> {
        (self.decisions > 0).then(|| self.band_total as f64 / self.decisions as f64)
    }
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
    sampler: SamplerTrace,
    backpressure: BackpressureTrace,
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
        sampler: sim.sampler,
        backpressure: sim.backpressure,
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

/// Mirrors `InferenceRouter::request_facts` — same `{:?}`
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
mod tests;
