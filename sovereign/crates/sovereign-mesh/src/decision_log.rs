// SPDX-License-Identifier: AGPL-3.0-or-later
//! Routing decision records — Phase 0 (P1 + P2) of
//! [`docs/specs/SCHEDULER_QUALITY.md`](../../../docs/specs/SCHEDULER_QUALITY.md).
//!
//! # Why this exists
//!
//! The OICP scheduler ranks candidates with a product of six
//! dimensionless multipliers. Today the only trace of a routing
//! decision is a scatter of `tracing::debug!` lines: one per
//! candidate, no shared identity, and nothing at all joining the
//! decision to what actually happened. You cannot answer "why did
//! this request go to the hub, and was that right in hindsight?"
//! from logs — which is exactly the glassbox obligation the spec
//! §7 acceptance bar states.
//!
//! This module makes a routing decision a **first-class record**:
//!
//! - one [`RoutingDecision`] per decision point, carrying the whole
//!   candidate set (each with its full `ScoreBreakdown` **and the
//!   provenance of every input that produced it** — P2), the peers
//!   that never made it into scoring and why, and the verdict;
//! - one [`RoutingOutcome`] per completed request, carrying
//!   served-by / TTFT / total / tokens / shed, joined to its
//!   decision by `decision_id`.
//!
//! The join is the point. SCHEDULER_QUALITY §5 defines Tier-1
//! calibration as **decision-agreement between the simulator and
//! real hardware** — a sim run is admissible evidence only if it
//! routes the way the mesh actually routed on a replayed episode.
//! Without a joinable decision→outcome record there is no
//! calibration contract, only a promise of one.
//!
//! # What it deliberately does not do
//!
//! Nothing here changes a routing decision. Phase 0 is instrumentation
//! only, by design: the simulator is the baseline machine, and moving
//! production before the baseline exists destroys the only baseline
//! we get (spec §6). Every emission site is an observer.
//!
//! # Where the records go
//!
//! [`DecisionSink`] is the seam. Production installs
//! [`TracingDecisionSink`], which always emits a summary line at
//! `tracing` target `mesh.decision` and — when
//! `SOVEREIGN_DECISION_LOG` names a path — appends the full record
//! as one JSON object per line. That JSONL file is the P4
//! trace-replay fixture substrate; see [`crate::decision_trace`].
//!
//! Tests install [`CaptureDecisionSink`] and assert on the records
//! directly, which is why the sink is a constructor-injected field
//! on `MeshInferenceProvider` rather than a process-global.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sovereign_core::oicp::{BenchmarkResult, NodeLocality, NodeObservations, ScoreBreakdown};

/// Schema tag stamped on every record. Bump on any
/// backwards-incompatible change to the shapes below — the replay
/// loader (`decision_trace`) refuses a trace it does not understand
/// rather than silently mis-reading old fields.
pub const DECISION_LOG_SCHEMA: &str = "oicp-decision/v1";

/// Env var naming the JSONL sink path. Unset (the default) means
/// records exist only as `tracing` events.
pub const DECISION_LOG_ENV: &str = "SOVEREIGN_DECISION_LOG";

/// `tracing` target every record is emitted under. Registered in the
/// daemon's `DAEMON_TRACING_FILTER` — a custom target is dark unless
/// it is listed there.
pub const DECISION_TRACE_TARGET: &str = "mesh.decision";

// ---------------------------------------------------------------
// Record model
// ---------------------------------------------------------------

/// One line of the decision log. Tagged so a JSONL stream can carry
/// both halves of the join interleaved, which is what the daemon
/// naturally produces (decisions and outcomes are minutes apart).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DecisionEvent {
    Decision(Box<RoutingDecision>),
    Outcome(Box<RoutingOutcome>),
    /// P3 — periodic observation-state export, interleaved into the
    /// same stream.
    ///
    /// Riding in the record stream rather than sitting behind its own
    /// route is what makes a capture **self-contained**: one env var
    /// (`SOVEREIGN_DECISION_LOG`) produces a file that already holds
    /// both the episodes and the fleet they ran against, so replay
    /// needs no second collection step that could be forgotten or
    /// taken at the wrong moment. A snapshot fetched an hour after
    /// the episodes would describe a different mesh.
    Snapshot(Box<FleetSnapshot>),
}

impl DecisionEvent {
    /// The join key shared by a decision and its outcome. Empty for
    /// a snapshot, which belongs to no single request.
    pub fn decision_id(&self) -> &str {
        match self {
            DecisionEvent::Decision(d) => &d.decision_id,
            DecisionEvent::Outcome(o) => &o.decision_id,
            DecisionEvent::Snapshot(_) => "",
        }
    }
}

// ---------------------------------------------------------------
// P3 — observation-state export
// ---------------------------------------------------------------

/// Everything one node observed about one peer at snapshot time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerObservationRecord {
    pub name: String,
    pub node_id: Option<String>,
    /// This node's observations of the peer — latency and throughput
    /// EWMAs, sample count, failure rate.
    pub observations: NodeObservations,
    /// The peer's gossiped benchmark, when it publishes one.
    pub benchmark: Option<BenchmarkResult>,
    /// The peer's gossiped self-reported in-flight count.
    pub gossiped_in_flight: Option<u32>,
    /// The peer's gossiped `inference_availability`.
    pub inference_availability: Option<f32>,
    /// Age of that gossip record in seconds at snapshot time. The
    /// distribution this field accumulates over a capture is F1's
    /// measurement and the simulator's most load-bearing parameter.
    pub gossip_age_secs: Option<u64>,
    pub quarantined: bool,
    pub consecutive_failures: u32,
    /// Seconds until quarantine lifts; `0` when not quarantined.
    pub cooldown_remaining_secs: u64,
}

/// The local node's side of the same state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalObservationRecord {
    pub observations: NodeObservations,
    pub benchmark: Option<BenchmarkResult>,
    /// Model ids this node currently advertises.
    pub advertised_models: Vec<String>,
    /// Total local in-flight — the number this node gossips.
    pub in_flight_published: u32,
}

/// A point-in-time export of the whole observation state a scheduler
/// decides from.
///
/// This is what lets a simulator's service-time model be **fit from
/// the real mesh** instead of hand-tuned. Spec §3 reports "p95
/// improves 2.5× under two-choices sampling" from a probe whose fleet
/// — 25 tok/s hub, 30 tok/s desktops, 45 tok/s laptops — the author
/// chose. Until those numbers come from a real mesh that result is a
/// property of the chosen constants, not evidence about the mesh.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetSnapshot {
    pub schema: String,
    pub captured_at_unix: u64,
    pub local: LocalObservationRecord,
    pub peers: Vec<PeerObservationRecord>,
}

impl FleetSnapshot {
    /// Median gossip age across peers that reported one, in seconds.
    ///
    /// The single number that says whether F1 is live on this mesh:
    /// compare it against the service time of a knowledge turn
    /// (10–20s). A feedback controller whose delay exceeds its
    /// process time constant oscillates, and that comparison is the
    /// whole diagnosis.
    pub fn median_gossip_age_secs(&self) -> Option<u64> {
        let mut ages: Vec<u64> = self
            .peers
            .iter()
            .filter_map(|p| p.gossip_age_secs)
            .collect();
        if ages.is_empty() {
            return None;
        }
        ages.sort_unstable();
        Some(ages[ages.len() / 2])
    }

    /// Observed decode throughput spread across the fleet, as
    /// `(min, max)` tok/s over peers whose EWMA has warmed up.
    ///
    /// F3 says `throughput_factor` clamps to 1.0 for everything above
    /// 20 tok/s. A spread wholly above that threshold is F3 measured
    /// rather than argued: the term meant to discriminate a
    /// heterogeneous fleet is constant across it.
    pub fn observed_tg_spread(&self) -> Option<(f64, f64)> {
        let rates: Vec<f64> = self
            .peers
            .iter()
            .map(|p| p.observations.tg_tok_s_ewma)
            .filter(|r| *r > 0.0)
            .collect();
        if rates.is_empty() {
            return None;
        }
        let min = rates.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = rates.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Some((min, max))
    }
}

/// One routing decision: the whole candidate set, the inputs each
/// candidate was scored on, who was excluded before scoring, and
/// the verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingDecision {
    pub schema: String,
    /// Join key to [`RoutingOutcome::decision_id`]. Generated here
    /// because `oicp_request_id` is caller-supplied and may be empty
    /// or repeated; this one is always present and always unique.
    pub decision_id: String,
    /// The caller-declared OICP tag (e.g. the workload resolver's
    /// `wl-<class>-<id>`), joinable against the serving node's
    /// `slot_selected` event. Empty string when the caller sent none.
    pub oicp_request_id: String,
    pub ts_unix_ms: u64,
    /// Which routing surface produced this record.
    pub path: DecisionPath,
    pub request: RequestFacts,
    /// Every candidate that was actually scored, local first.
    pub candidates: Vec<CandidateRecord>,
    /// Peers dropped before scoring, with the reason. A candidate
    /// set that silently omits peers is unreadable in hindsight —
    /// "the hub wasn't chosen" and "the hub was never considered"
    /// are different failures with different fixes.
    pub excluded: Vec<ExcludedCandidate>,
    pub verdict: Verdict,
}

/// The routing surface a decision came from. Named-model dispatch
/// and OICP ranked selection are different code paths with different
/// failure modes; a scoreboard that pools them measures neither.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPath {
    /// `select_peers_ranked` — score everything, rank peers that
    /// strictly beat local.
    RankedOicp,
    /// `locate_named_model` — an explicit `model_id` (or a configured
    /// shared-model primary) names the target.
    NamedModel,
    /// A **soft** named target (a configured shared-model primary)
    /// resolved to nobody, so selection fell THROUGH to the ranked
    /// scorer instead of collapsing to this node's own model. The
    /// candidates on this record are real — the scorer ran — which is
    /// why replay admits it alongside [`Self::RankedOicp`]. It is kept
    /// distinct so a scoreboard can answer "how often is the shared
    /// cluster unavailable?" without that traffic disappearing into
    /// the ordinary ranked population. Pairs with the `NamedModel`
    /// record emitted for the same `oicp_request_id`, which is what
    /// names the model that went missing.
    NamedFallthrough,
}

/// The request-side facts that constrained this decision. Everything
/// here is a scheduler *input*; a replayed episode reconstructs the
/// arrival stream from these fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestFacts {
    pub capability_hint: String,
    pub latency_class: String,
    pub sharding: String,
    pub context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub preferred_speed: String,
    /// Set on the named path; `None` on the ranked path.
    pub explicit_model_id: Option<String>,
}

/// Whether a candidate was this node or a peer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Local,
    Peer,
}

/// One scored candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CandidateRecord {
    pub kind: CandidateKind,
    /// `"local"` or the peer's mesh name.
    pub name: String,
    pub node_id: Option<String>,
    pub model_id: String,
    pub size_gb: Option<f32>,
    pub locality: String,
    /// Position in the final best-first ranking, `0` = first choice.
    /// `None` for candidates that did not make the ranked set (local,
    /// and peers that failed to strictly beat local).
    pub rank: Option<u32>,
    /// Whether this candidate was the first the cascade would try.
    pub selected: bool,
    pub score: ScoreRecord,
    pub inputs: CandidateInputs,
    /// Capability band this candidate fell in, `0` = the most capable
    /// models visible in this decision (`crate::tier`). `None` means
    /// the candidate advertised no size and could not be banded — which
    /// a binding [`TierFloor`](crate::tier::TierFloor) treats as
    /// failing the floor.
    ///
    /// Recorded rather than left to be recomputed because the tier
    /// floor *consumes* it: §4.1's own rule is that an objective may
    /// not read a signal the record does not carry, or a capture stops
    /// being replayable for that term. Bands are a property of the
    /// candidate *set*, so this is stamped after every candidate is
    /// pushed — see [`DecisionBuilder::assign_tier_bands`].
    ///
    /// `serde(default)` so captures written before the tier floor
    /// existed deserialise unchanged and replay reproduces their old
    /// verdicts exactly.
    #[serde(default)]
    pub tier_band: Option<u32>,
}

/// A serialisable mirror of `ScoreBreakdown`. Mirrored rather than
/// serialised in place because `ScoreBreakdown::throughput_source` is
/// a `&'static str` (not `Deserialize`) and because the log format
/// must be able to outlive a refactor of the scorer's internals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreRecord {
    pub claim_score: f32,
    pub observation_mult: f32,
    pub load_penalty: f32,
    pub locality_bonus: f32,
    pub cold_start_weight: f32,
    pub throughput_factor: f32,
    pub throughput_source: String,
    pub availability: f32,
    pub final_score: f32,
}

impl From<&ScoreBreakdown> for ScoreRecord {
    fn from(b: &ScoreBreakdown) -> Self {
        Self {
            claim_score: b.claim_score,
            observation_mult: b.observation_mult,
            load_penalty: b.load_penalty,
            locality_bonus: b.locality_bonus,
            cold_start_weight: b.cold_start_weight,
            throughput_factor: b.throughput_factor,
            throughput_source: b.throughput_source.to_string(),
            availability: b.availability,
            final_score: b.final_score,
        }
    }
}

/// Where a load signal came from. F1 (spec §2) is that a decider sees
/// its own load exactly and everyone else's 10–30s late; you cannot
/// measure that without recording *which* view of load was used.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoadSource {
    /// This node's own observations — always fresh.
    Local,
    /// The peer's gossiped self-reported count. Authoritative for
    /// total load, stale by up to a full anti-entropy round.
    Gossip,
    /// Only this node's view of what it dispatched to the peer.
    /// Structurally blind to the peer's locally-originated traffic.
    SelfObserved,
}

/// P2 — input provenance and staleness stamping.
///
/// Every scorer input, recorded next to *how old it was* at the
/// moment the scorer read it. F1's dead time is a hypothesis until
/// this is measured in production, and it is the simulator's most
/// load-bearing parameter: guessing the staleness distribution wrong
/// invalidates every latency number the sim produces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CandidateInputs {
    /// The in-flight count the scorer actually used.
    pub in_flight: u32,
    pub in_flight_source: LoadSource,
    /// This node's own view, before any gossip override.
    pub self_observed_in_flight: Option<u32>,
    /// The peer's gossiped self-report, when present.
    pub gossiped_in_flight: Option<u32>,
    /// Gossiped `inference_availability`, as received (unclamped —
    /// the clamped value the scorer used is in
    /// [`ScoreRecord::availability`]).
    pub availability: Option<f32>,
    /// Age, in seconds, of the gossip record the two signals above
    /// came from — `now - MemberRecord.last_seen`. This is the
    /// staleness measurement F1 turns on. `None` for the local
    /// candidate and for peers whose record carried no timestamp.
    pub gossip_age_secs: Option<u64>,
    /// Age of the cached OICP manifest the claims were read from.
    /// `0` means it was fetched during this decision.
    pub manifest_age_secs: Option<u64>,
    /// Whether the manifest came from cache (`true`) or a live fetch.
    pub manifest_from_cache: Option<bool>,
    /// Manifest-probe round-trip time; also the locality signal.
    pub rtt_ms: Option<u32>,
    pub samples: u32,
    pub recent_failure_rate: f32,
    pub p50_latency_ms: u32,
    pub p95_latency_ms: u32,
    pub ttft_ewma_ms: f64,
    pub tg_tok_s_ewma: f64,
    /// Gossiped benchmark, the throughput-estimate path's input.
    pub bench_pp_tok_s: Option<f32>,
    pub bench_tg_tok_s: Option<f32>,
    pub bench_baseline_size_gb: Option<f32>,
    /// Age of that benchmark in seconds. A benchmark measured before
    /// a hardware change is a silent mis-input to `throughput_factor`.
    pub bench_age_secs: Option<u64>,
    /// `ModelStatus::loaded` for the model the scorer picked, and the
    /// load time it advertises. Recorded because §4.1's predicted-time
    /// objective consumes them: **an objective may not read a signal the
    /// record does not carry**, or the decision stops being replayable
    /// from a capture. `None` on records written before these existed,
    /// which `PredictInputs::from_candidate` reads as "resident".
    #[serde(default)]
    pub model_loaded: Option<bool>,
    #[serde(default)]
    pub estimated_load_ms: Option<u32>,
}

impl CandidateInputs {
    /// Build from the observation snapshot the scorer was handed.
    /// The caller supplies the provenance fields it alone knows.
    pub fn from_observations(obs: &NodeObservations, source: LoadSource) -> Self {
        Self {
            in_flight: obs.in_flight,
            in_flight_source: source,
            self_observed_in_flight: None,
            gossiped_in_flight: None,
            availability: None,
            gossip_age_secs: None,
            manifest_age_secs: None,
            manifest_from_cache: None,
            rtt_ms: None,
            samples: obs.samples,
            recent_failure_rate: obs.recent_failure_rate,
            p50_latency_ms: obs.p50_latency_ms,
            p95_latency_ms: obs.p95_latency_ms,
            ttft_ewma_ms: obs.ttft_ewma_ms,
            tg_tok_s_ewma: obs.tg_tok_s_ewma,
            model_loaded: None,
            estimated_load_ms: None,
            bench_pp_tok_s: None,
            bench_tg_tok_s: None,
            bench_baseline_size_gb: None,
            bench_age_secs: None,
        }
    }

    /// Fold in the gossiped benchmark and its age.
    pub fn with_benchmark(mut self, bench: Option<&BenchmarkResult>, now_unix: u64) -> Self {
        if let Some(b) = bench {
            self.bench_pp_tok_s = Some(b.pp_tok_s);
            self.bench_tg_tok_s = Some(b.tg_tok_s);
            self.bench_baseline_size_gb = Some(b.baseline_size_gb);
            self.bench_age_secs = Some(now_unix.saturating_sub(b.measured_at));
        }
        self
    }
}

/// A peer that never entered scoring, and why. The reasons are a
/// closed set so the scoreboard can count them: an excluded-peer
/// distribution that shifts is a routing regression even when every
/// score stayed identical.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// `PeerHealthTracker` has the peer in cooldown.
    Quarantined,
    /// The manifest fetch failed or timed out across every address.
    ManifestUnavailable,
    /// The manifest parsed but no claim could serve this request.
    NoClaimMatch,
    /// Forced-choice sentinel, and the peer does not advertise
    /// `x:forced_choice`.
    NoForcedChoice,
    /// The peer refused a recent hop with `yielded_to_local` and the
    /// `retry_after_secs` it asked for has not elapsed. Self-clearing
    /// on that deadline, and — unlike [`Self::Quarantined`] — it books
    /// nothing against `PeerHealthTracker`: a refusal to serve is not
    /// a fault (see `book_peer_failure`).
    YieldedToLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExcludedCandidate {
    pub name: String,
    pub node_id: Option<String>,
    pub reason: ExclusionReason,
}

/// What the decision concluded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// A pre-scoring gate short-circuited: no candidate was scored at
    /// all. `gate` is the same string the existing glassbox debug
    /// lines name (`no_routing_signal`, `operator_disabled`,
    /// `not_offload_eligible`).
    Gated { gate: String },
    /// Everything was scored and no peer strictly beat local.
    StayLocal,
    /// Peers that strictly beat local, best-first. The cascade tries
    /// them in this order, then falls back to local.
    Peers { ranked: Vec<String> },
    /// Named-model dispatch resolved to the local node.
    NamedLocal { model_id: String },
    /// Named-model dispatch resolved to a peer.
    NamedPeer { peer: String, model_id: String },
    /// Named-model dispatch found nobody advertising the name.
    NamedUnknown { model_id: String },
}

/// Where a request was ultimately served.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServedBy {
    /// Served locally by the named-model path.
    Local { model_id: String },
    /// Served locally after the peer cascade was exhausted or empty.
    LocalFallback { model_id: String },
    Peer {
        name: String,
        node_id: Option<String>,
        model_id: String,
    },
    /// Every cascade step failed.
    Failed,
}

/// One cascade step that was tried and did not serve.
///
/// The terminal outcome's `attempt_index` says *how many* failovers
/// happened; this says *who* and *why*. The distinction matters
/// because the two costs are different: a peer that shed (congestion,
/// self-clearing) and a peer that broke (failure, quarantine-worthy)
/// are the same channel in the code today — spec F4 — and the
/// scoreboard cannot separate them unless the record does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailoverAttempt {
    pub peer: String,
    pub error: String,
    /// Classified by [`looks_shed`]. Read-only: nothing routes on it.
    pub shed: bool,
    /// Set when [`parse_yield_refusal`] recognised this refusal as the
    /// peer yielding to its own local user, to the seconds it asked us
    /// to wait. Unlike `shed`, something DOES route on this one: the
    /// peer is excluded from candidacy for that window
    /// ([`ExclusionReason::YieldedToLocal`]), so the next turn does not
    /// re-dial into the same refusal. `None` on every other failure and
    /// on records written before 2026-08-14.
    #[serde(default)]
    pub yield_retry_after_secs: Option<u64>,
}

/// The completion half of the join: what the decision actually cost.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingOutcome {
    pub schema: String,
    pub decision_id: String,
    pub oicp_request_id: String,
    pub ts_unix_ms: u64,
    pub served_by: ServedBy,
    /// Index of the cascade step that served: `0` = the decision's
    /// first choice. Anything above `0` is a failover, which is the
    /// waste metric in spec §5's scoreboard.
    pub attempt_index: u32,
    /// Time to first token, milliseconds. `None` when the request
    /// produced no tokens.
    pub ttft_ms: Option<f64>,
    /// Wall time from dispatch to stream end, milliseconds.
    pub total_ms: Option<f64>,
    /// Data frames yielded — the same coarse token proxy
    /// `ThroughputObservedStream` folds into the EWMAs.
    pub output_tokens: Option<u64>,
    /// Whether the serving side shed this request (503 / admission
    /// rejection) rather than failing. Congestion and failure share
    /// one channel today (spec F4); recording them apart here is
    /// what lets the scoreboard separate them before the code does.
    pub shed: bool,
    pub error: Option<String>,
    /// Cascade steps tried before this one, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failovers: Vec<FailoverAttempt>,
}

// ---------------------------------------------------------------
// Sink
// ---------------------------------------------------------------

/// Where decision records go. Injected into `MeshInferenceProvider`
/// so tests can capture records without racing on a process-global.
///
/// `Debug` is a supertrait so the contexts that carry a sink around
/// (notably [`OutcomeContext`], which rides on the stream wrapper)
/// stay printable — an instrumentation seam that cannot itself be
/// inspected is a poor glassbox.
pub trait DecisionSink: Send + Sync + std::fmt::Debug {
    fn record(&self, event: DecisionEvent);
}

/// Discards everything. Used where instrumentation would be pure
/// overhead (short-lived provider instances in unrelated tests).
#[derive(Debug, Default)]
pub struct NullDecisionSink;

impl DecisionSink for NullDecisionSink {
    fn record(&self, _event: DecisionEvent) {}
}

/// The production sink.
///
/// Always emits to `tracing` under [`DECISION_TRACE_TARGET`]: a
/// human-readable summary at `info`, the full JSON record at `debug`.
/// When [`DECISION_LOG_ENV`] names a path, also appends the full
/// record as one JSON object per line, flushed per record — a daemon
/// that runs for weeks must not hold the last decisions in a buffer.
pub struct TracingDecisionSink {
    file: Option<Mutex<std::io::BufWriter<std::fs::File>>>,
}

impl std::fmt::Debug for TracingDecisionSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TracingDecisionSink")
            .field("jsonl", &self.file.is_some())
            .finish()
    }
}

impl TracingDecisionSink {
    /// Build from the environment. A configured path that cannot be
    /// opened is a warning, not a failure — instrumentation must
    /// never take the daemon down.
    pub fn from_env() -> Self {
        let Some(path) = std::env::var_os(DECISION_LOG_ENV) else {
            return Self { file: None };
        };
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => {
                tracing::info!(
                    target: DECISION_TRACE_TARGET,
                    path = %path.display(),
                    schema = DECISION_LOG_SCHEMA,
                    "decision log: appending routing decision records"
                );
                Self {
                    file: Some(Mutex::new(std::io::BufWriter::new(f))),
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: DECISION_TRACE_TARGET,
                    path = %path.display(),
                    error = %e,
                    "decision log: could not open sink path — records stay in tracing only"
                );
                Self { file: None }
            }
        }
    }

    /// Build a sink that writes JSONL to an explicit path, ignoring
    /// the environment. The trace-capture CLI verb uses this.
    pub fn to_path(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Some(Mutex::new(std::io::BufWriter::new(f))),
        })
    }
}

impl DecisionSink for TracingDecisionSink {
    fn record(&self, event: DecisionEvent) {
        match &event {
            DecisionEvent::Decision(d) => {
                let winner = d
                    .candidates
                    .iter()
                    .find(|c| c.selected)
                    .map(|c| c.name.as_str())
                    .unwrap_or("<none>");
                // A gated decision scored nothing — it is policy, not
                // scheduling — and it used to sit at `debug` so the
                // `info` line stayed one-per-decision that actually
                // chose between candidates.
                //
                // That reasoning cost a whole measurement. The deployed
                // daemon filters `mesh.decision=info`
                // (`sovereign-cli-daemon`'s `DAEMON_TRACING_FILTER`), so
                // "why did this turn stay home?" was the ONE routing
                // question the operator's log could not answer. §9.1.1 of
                // MESH_SCALE_100_USERS_1000_CORPORA.md ran 100 turns at a
                // census-verified 2-node mesh, saw zero peer dispatches,
                // and had to record the firing gate as UNKNOWN — every
                // one of those turns emitted a `Gated` record naming it,
                // at a level nothing was listening to. The record was
                // there; the operator was not allowed to see it.
                //
                // Cost of the promotion, measured on this host's own
                // decision log (49,167 records, 2026-08-06 → 08-13):
                // 2,245 gated decisions, ~320/day, ~1 line per 4.5
                // minutes. That is not a stream. `gate` is lifted out of
                // the label into its own field so it is greppable
                // without parsing, and `path` rides along because
                // "gated on the ranked path" and "gated on the named
                // fallthrough" are different stories.
                if let Verdict::Gated { gate } = &d.verdict {
                    tracing::info!(
                        target: DECISION_TRACE_TARGET,
                        decision_id = %d.decision_id,
                        oicp_request_id = %d.oicp_request_id,
                        path = ?d.path,
                        gate = %gate,
                        verdict = %verdict_label(&d.verdict),
                        hint = %d.request.capability_hint,
                        latency = %d.request.latency_class,
                        sharding = %d.request.sharding,
                        "routing decision (gated) — stayed local before scoring"
                    );
                } else {
                    tracing::info!(
                        target: DECISION_TRACE_TARGET,
                        decision_id = %d.decision_id,
                        oicp_request_id = %d.oicp_request_id,
                        path = ?d.path,
                        verdict = %verdict_label(&d.verdict),
                        winner = %winner,
                        scored = d.candidates.len(),
                        excluded = d.excluded.len(),
                        hint = %d.request.capability_hint,
                        latency = %d.request.latency_class,
                        "routing decision"
                    );
                }
            }
            DecisionEvent::Outcome(o) => {
                tracing::info!(
                    target: DECISION_TRACE_TARGET,
                    decision_id = %o.decision_id,
                    oicp_request_id = %o.oicp_request_id,
                    served_by = %served_by_label(&o.served_by),
                    attempt = o.attempt_index,
                    ttft_ms = ?o.ttft_ms,
                    total_ms = ?o.total_ms,
                    tokens = ?o.output_tokens,
                    shed = o.shed,
                    "routing outcome"
                );
            }
            DecisionEvent::Snapshot(s) => {
                // The two headline diagnostics, in the log where an
                // operator will actually see them: is gossip lag
                // comparable to service time (F1), and does the
                // throughput term discriminate this fleet (F3)?
                tracing::info!(
                    target: DECISION_TRACE_TARGET,
                    peers = s.peers.len(),
                    median_gossip_age_secs = ?s.median_gossip_age_secs(),
                    observed_tg_spread = ?s.observed_tg_spread(),
                    quarantined = s.peers.iter().filter(|p| p.quarantined).count(),
                    "fleet observation snapshot"
                );
            }
        }

        // The full record is expensive to render and only wanted when
        // someone is actually collecting. Serialise once and reuse for
        // both the debug event and the file.
        let want_json = self.file.is_some()
            || tracing::enabled!(target: DECISION_TRACE_TARGET, tracing::Level::DEBUG);
        if !want_json {
            return;
        }
        let line = match serde_json::to_string(&event) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    target: DECISION_TRACE_TARGET,
                    error = %e,
                    "decision log: record failed to serialise"
                );
                return;
            }
        };
        tracing::debug!(target: DECISION_TRACE_TARGET, record = %line, "routing record");
        if let Some(file) = &self.file {
            if let Ok(mut w) = file.lock() {
                if let Err(e) = writeln!(w, "{line}").and_then(|()| w.flush()) {
                    tracing::warn!(
                        target: DECISION_TRACE_TARGET,
                        error = %e,
                        "decision log: write failed"
                    );
                }
            }
        }
    }
}

fn verdict_label(v: &Verdict) -> String {
    match v {
        Verdict::Gated { gate } => format!("gated:{gate}"),
        Verdict::StayLocal => "stay_local".into(),
        Verdict::Peers { ranked } => format!("peers:{}", ranked.len()),
        Verdict::NamedLocal { .. } => "named_local".into(),
        Verdict::NamedPeer { peer, .. } => format!("named_peer:{peer}"),
        Verdict::NamedUnknown { .. } => "named_unknown".into(),
    }
}

fn served_by_label(s: &ServedBy) -> String {
    match s {
        ServedBy::Local { model_id } => format!("local:{model_id}"),
        ServedBy::LocalFallback { model_id } => format!("local_fallback:{model_id}"),
        ServedBy::Peer { name, .. } => format!("peer:{name}"),
        ServedBy::Failed => "failed".into(),
    }
}

/// In-memory sink for tests: records are appended in emission order
/// and readable through [`CaptureDecisionSink::events`].
#[derive(Debug, Default)]
pub struct CaptureDecisionSink {
    events: Mutex<Vec<DecisionEvent>>,
}

impl CaptureDecisionSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of everything recorded so far.
    pub fn events(&self) -> Vec<DecisionEvent> {
        self.events.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Only the decision half.
    pub fn decisions(&self) -> Vec<RoutingDecision> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                DecisionEvent::Decision(d) => Some(*d),
                _ => None,
            })
            .collect()
    }

    /// Only the outcome half.
    pub fn outcomes(&self) -> Vec<RoutingOutcome> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                DecisionEvent::Outcome(o) => Some(*o),
                _ => None,
            })
            .collect()
    }
}

impl DecisionSink for CaptureDecisionSink {
    fn record(&self, event: DecisionEvent) {
        if let Ok(mut v) = self.events.lock() {
            v.push(event);
        }
    }
}

// ---------------------------------------------------------------
// Helpers shared by the emission sites
// ---------------------------------------------------------------

/// Monotonic within a process, so a decision id sorts by issue order
/// even when two decisions land in the same millisecond.
static DECISION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Mint a decision id. Prefixed with the process-local sequence so a
/// tailed log reads in order; suffixed with a short random tail so
/// ids from different nodes never collide when traces are merged.
pub fn new_decision_id() -> String {
    let seq = DECISION_SEQ.fetch_add(1, Ordering::Relaxed);
    let tail = uuid::Uuid::new_v4().simple().to_string();
    format!("d{seq:08x}-{}", &tail[..12])
}

pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Stable string form of a locality bucket, for the record.
pub fn locality_label(l: NodeLocality) -> &'static str {
    match l {
        NodeLocality::Local => "local",
        NodeLocality::Near => "near",
        NodeLocality::Far => "far",
    }
}

/// Inverse of [`locality_label`]. `None` for a label this build does
/// not know.
///
/// Replay ([`crate::decision_replay`]) needs the bucket back as a
/// value to re-run the scorer, and the record stores it as a string
/// precisely so the log format can outlive a refactor of the enum.
/// Returning `Option` rather than defaulting to `Far` is the point:
/// an unreadable label is a *gap in the record*, and silently
/// substituting the neutral bucket would hide it behind a
/// locality_bonus of 1.0 that looks perfectly plausible.
pub fn locality_from_label(label: &str) -> Option<NodeLocality> {
    match label {
        "local" => Some(NodeLocality::Local),
        "near" => Some(NodeLocality::Near),
        "far" => Some(NodeLocality::Far),
        _ => None,
    }
}

/// A decision in progress. The emission sites build one of these as
/// they score, then `emit` it once — so a decision record can never
/// be half-written, and the scoring loop stays readable.
pub struct DecisionBuilder {
    decision_id: String,
    oicp_request_id: String,
    path: DecisionPath,
    request: RequestFacts,
    candidates: Vec<CandidateRecord>,
    excluded: Vec<ExcludedCandidate>,
}

impl DecisionBuilder {
    pub fn new(oicp_request_id: &str, path: DecisionPath, request: RequestFacts) -> Self {
        Self {
            decision_id: new_decision_id(),
            oicp_request_id: oicp_request_id.to_string(),
            path,
            request,
            candidates: Vec::new(),
            excluded: Vec::new(),
        }
    }

    pub fn decision_id(&self) -> &str {
        &self.decision_id
    }

    /// Record a scored candidate. Returns its index, so a caller that
    /// needs to come back and stamp a set-wide property (the tier
    /// band) can address it without re-deriving the push order.
    pub fn push_candidate(&mut self, c: CandidateRecord) -> usize {
        self.candidates.push(c);
        self.candidates.len() - 1
    }

    /// Partition the candidates pushed so far into capability bands and
    /// stamp each record's `tier_band`, returning the bands
    /// index-parallel to [`Self::push_candidate`]'s indices.
    ///
    /// Bands are computed from the builder's own records rather than
    /// from a list the caller assembles in parallel, so the persisted
    /// band is *by construction* the same partition the filter used.
    /// The alternative — caller-built sizes plus a caller-built stamp —
    /// is two orderings that can drift, and a drift here would be
    /// invisible: the record would explain a decision the decider did
    /// not take.
    pub fn assign_tier_bands(&mut self) -> Vec<Option<u32>> {
        let sizes: Vec<Option<f32>> = self.candidates.iter().map(|c| c.size_gb).collect();
        let bands = crate::tier::bands(&sizes);
        for (c, band) in self.candidates.iter_mut().zip(&bands) {
            c.tier_band = *band;
        }
        bands
    }

    pub fn exclude(&mut self, name: &str, node_id: Option<String>, reason: ExclusionReason) {
        self.excluded.push(ExcludedCandidate {
            name: name.to_string(),
            node_id,
            reason,
        });
    }

    /// Mark the winners in ranked order and finish. `ranked` names the
    /// candidates in cascade order; the first is `selected`.
    pub fn finish(self, verdict: Verdict, ranked: &[String]) -> RoutingDecision {
        let now = now_unix_ms();
        self.finish_at(verdict, ranked, now)
    }

    /// [`Self::finish`] with the timestamp supplied rather than read
    /// from the wall clock.
    ///
    /// Production calls `finish`. A *simulated* decider
    /// (`crate::mesh_sim`) runs on virtual time, and a record stamped
    /// with the host's wall clock would be unorderable against the
    /// episode it belongs to — the scoreboard reads `ts_unix_ms` to
    /// build per-window herding and fairness series, so the sim's
    /// records have to carry sim time. Same reason the core takes
    /// `now_unix` rather than reading a clock: a decision is a pure
    /// function of its inputs, and time is one of them.
    pub fn finish_at(
        mut self,
        verdict: Verdict,
        ranked: &[String],
        ts_unix_ms: u64,
    ) -> RoutingDecision {
        for (i, name) in ranked.iter().enumerate() {
            if let Some(c) = self.candidates.iter_mut().find(|c| &c.name == name) {
                c.rank = Some(i as u32);
                c.selected = i == 0;
            }
        }
        RoutingDecision {
            schema: DECISION_LOG_SCHEMA.to_string(),
            decision_id: self.decision_id,
            oicp_request_id: self.oicp_request_id,
            ts_unix_ms,
            path: self.path,
            request: self.request,
            candidates: self.candidates,
            excluded: self.excluded,
            verdict,
        }
    }
}

/// Emit a decision through a sink.
pub fn emit_decision(sink: &Arc<dyn DecisionSink>, decision: RoutingDecision) {
    sink.record(DecisionEvent::Decision(Box::new(decision)));
}

/// Emit an outcome through a sink.
pub fn emit_outcome(sink: &Arc<dyn DecisionSink>, outcome: RoutingOutcome) {
    sink.record(DecisionEvent::Outcome(Box::new(outcome)));
}

/// Everything the completion half of the join needs, carried from the
/// decision site to wherever the request finishes. Cheap to clone —
/// it rides along on the stream wrapper.
#[derive(Debug, Clone)]
pub struct OutcomeContext {
    pub sink: Arc<dyn DecisionSink>,
    pub decision_id: String,
    pub oicp_request_id: String,
    pub served_by: ServedBy,
    pub attempt_index: u32,
    pub failovers: Vec<FailoverAttempt>,
}

impl OutcomeContext {
    /// Emit the terminal record for a request that produced timings.
    pub fn complete(
        &self,
        ttft_ms: Option<f64>,
        total_ms: Option<f64>,
        output_tokens: Option<u64>,
    ) {
        emit_outcome(
            &self.sink,
            RoutingOutcome {
                schema: DECISION_LOG_SCHEMA.to_string(),
                decision_id: self.decision_id.clone(),
                oicp_request_id: self.oicp_request_id.clone(),
                ts_unix_ms: now_unix_ms(),
                served_by: self.served_by.clone(),
                attempt_index: self.attempt_index,
                ttft_ms,
                total_ms,
                output_tokens,
                shed: false,
                error: None,
                failovers: self.failovers.clone(),
            },
        );
    }

    /// Emit the terminal record for a request that never produced a
    /// stream. `shed` distinguishes an admission rejection from a
    /// transport failure — the two are one channel in the code today
    /// (spec F4) and the scoreboard needs them apart.
    pub fn failed(&self, error: String, shed: bool) {
        emit_outcome(
            &self.sink,
            RoutingOutcome {
                schema: DECISION_LOG_SCHEMA.to_string(),
                decision_id: self.decision_id.clone(),
                oicp_request_id: self.oicp_request_id.clone(),
                ts_unix_ms: now_unix_ms(),
                served_by: ServedBy::Failed,
                attempt_index: self.attempt_index,
                ttft_ms: None,
                total_ms: None,
                output_tokens: None,
                shed,
                error: Some(error),
                failovers: self.failovers.clone(),
            },
        );
    }
}

/// Best-effort classification of a cascade error as congestion
/// (the peer shed) versus failure (the peer broke).
///
/// This is a **read-only** classifier: F4's fix — routing congestion
/// away from `PeerHealthTracker` — is a Phase 2 behavioural change
/// and deliberately does not land here. Recording the distinction now
/// is what lets the Phase 2 arm be measured against a baseline in
/// which the two were conflated.
pub fn looks_shed(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("503")
        || e.contains("service unavailable")
        || e.contains("too many requests")
        || e.contains("429")
        || e.contains("retry-after")
}

/// The default backoff applied to a `yielded_to_local` refusal whose
/// body carried no `retry_after_secs`. Deliberately short: guessing
/// too long benches a peer whose user stepped away, and the gossiped
/// availability signal (which is authoritative) arrives within a
/// round anyway.
pub const YIELD_REFUSAL_DEFAULT_BACKOFF_SECS: u64 = 5;

/// Is this cascade error the peer saying "my own user is at the
/// keyboard"? Returns the seconds it asked us to wait.
///
/// THE one asker for that question — the scheduler's backoff, the
/// decision record and the glassbox line all read this, so "what
/// counts as a yield refusal" has a single definition.
///
/// Narrower than [`looks_shed`] on purpose. A shed is any refusal
/// (paused contribution, `max_peer_inflight`, yield); only the yield
/// variety is *predictably* going to repeat for a known window, which
/// is what makes a backoff safe rather than a guess. Requiring the
/// `yielded_to_local` token means a congestion shed keeps its existing
/// behaviour exactly — it stays a candidate and the load penalty does
/// the work.
///
/// The wire shape is `commonwealth-api`'s `AdmissionRejection`,
/// serialised by its `IntoResponse`:
/// `503 {"error":"local user active","reason":"yielded_to_local",
/// "retry_after_secs":14}`. Parsed out of the error TEXT because that
/// is what the cascade has: `oicp-client` surfaces the remote body as
/// an excerpt on a failed hop, and threading a typed rejection back
/// through the streaming and non-streaming transports is a wider
/// change than this behaviour needs. The token is required, so a
/// truncated excerpt fails closed to "not a yield refusal" and the
/// peer simply stays a candidate.
pub fn parse_yield_refusal(error: &str) -> Option<u64> {
    let e = error.to_ascii_lowercase();
    if !e.contains("yielded_to_local") {
        return None;
    }
    Some(
        parse_retry_after_secs(&e)
            .filter(|s| *s > 0)
            .unwrap_or(YIELD_REFUSAL_DEFAULT_BACKOFF_SECS),
    )
}

/// Pull the integer that follows `retry_after_secs` in an error body,
/// tolerating the JSON punctuation around it. Returns `None` when the
/// key is absent or the value is not a bare integer.
fn parse_retry_after_secs(lowercased: &str) -> Option<u64> {
    let idx = lowercased.find("retry_after_secs")?;
    let rest = &lowercased[idx + "retry_after_secs".len()..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit() && *c != ',' && *c != '}')
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod yield_refusal_tests {
    use super::*;

    /// The bytes `commonwealth-api`'s admission layer actually emits.
    /// Kept identical to the `shedding_chat_handler` fixture in
    /// `tests/chat_completion_e2e.rs` — if the wire shape changes, both
    /// should change together and this is the cheaper one to notice.
    const YIELD_BODY: &str = r#"HTTP 503: {"error":"peer is serving its own user","reason":"yielded_to_local","retry_after_secs":34}"#;

    #[test]
    fn recognises_the_real_wire_shape_and_its_window() {
        assert_eq!(parse_yield_refusal(YIELD_BODY), Some(34));
    }

    #[test]
    fn a_yield_refusal_is_still_a_shed() {
        // The two classifiers must agree that this is congestion, not a
        // fault — the backoff is additional to the health exemption, not
        // a replacement for it.
        assert!(looks_shed(YIELD_BODY));
    }

    #[test]
    fn an_ordinary_congestion_shed_is_not_a_yield_refusal() {
        // `max_peer_inflight` and paused-contribution refusals keep
        // their existing behaviour: still candidates, load penalty does
        // the work. Only the yield variety predicts its own repeat.
        assert_eq!(parse_yield_refusal("HTTP 503: too many requests"), None);
        assert_eq!(
            parse_yield_refusal(r#"503 {"reason":"ceiling_exceeded","retry_after_secs":3}"#),
            None
        );
        assert_eq!(parse_yield_refusal("500 slot panicked"), None);
    }

    #[test]
    fn a_yield_refusal_without_a_window_gets_the_default() {
        assert_eq!(
            parse_yield_refusal(r#"503 {"reason":"yielded_to_local"}"#),
            Some(YIELD_REFUSAL_DEFAULT_BACKOFF_SECS)
        );
    }

    /// Fails CLOSED: a body truncated before the token reads as "not a
    /// yield refusal", so the peer stays a candidate. The cost of
    /// guessing wrong in that direction is one refused hop; guessing
    /// wrong the other way benches a healthy peer on no evidence.
    #[test]
    fn a_truncated_excerpt_is_not_a_yield_refusal() {
        assert_eq!(
            parse_yield_refusal(r#"HTTP 503: {"error":"peer is s"#),
            None
        );
    }

    #[test]
    fn a_zero_window_falls_back_to_the_default() {
        // 0 would mean "excluded for no time at all", i.e. a no-op that
        // silently re-dials. Treat it as absent.
        assert_eq!(
            parse_yield_refusal(r#"503 {"reason":"yielded_to_local","retry_after_secs":0}"#),
            Some(YIELD_REFUSAL_DEFAULT_BACKOFF_SECS)
        );
    }

    #[test]
    fn a_failover_record_written_before_this_field_still_deserialises() {
        // Old JSONL in the operator's decision log must keep replaying.
        let old = r#"{"peer":"hub","error":"503","shed":true}"#;
        let a: FailoverAttempt = serde_json::from_str(old).expect("old record must still parse");
        assert_eq!(a.yield_retry_after_secs, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> RequestFacts {
        RequestFacts {
            capability_hint: "general".into(),
            latency_class: "normal".into(),
            sharding: "mesh_allowed".into(),
            context_tokens: Some(1500),
            max_output_tokens: Some(250),
            preferred_speed: "slow".into(),
            explicit_model_id: None,
        }
    }

    fn candidate(name: &str, score: f32) -> CandidateRecord {
        CandidateRecord {
            kind: if name == "local" {
                CandidateKind::Local
            } else {
                CandidateKind::Peer
            },
            name: name.into(),
            node_id: None,
            model_id: "m".into(),
            size_gb: Some(9.0),
            locality: "far".into(),
            rank: None,
            selected: false,
            score: ScoreRecord {
                claim_score: score,
                observation_mult: 1.0,
                load_penalty: 1.0,
                locality_bonus: 1.0,
                cold_start_weight: 1.0,
                throughput_factor: 1.0,
                throughput_source: "neutral".into(),
                availability: 1.0,
                final_score: score,
            },
            inputs: CandidateInputs::from_observations(
                &NodeObservations::default(),
                LoadSource::Local,
            ),
            tier_band: None,
        }
    }

    #[test]
    fn decision_ids_are_unique_and_ordered() {
        let a = new_decision_id();
        let b = new_decision_id();
        assert_ne!(a, b);
        // The sequence prefix sorts by issue order.
        assert!(a < b, "{a} should sort before {b}");
    }

    #[test]
    fn finish_marks_rank_and_single_selection() {
        let mut b = DecisionBuilder::new("req-1", DecisionPath::RankedOicp, facts());
        b.push_candidate(candidate("local", 0.5));
        b.push_candidate(candidate("hub", 0.9));
        b.push_candidate(candidate("laptop", 0.7));
        let ranked = vec!["hub".to_string(), "laptop".to_string()];
        let d = b.finish(
            Verdict::Peers {
                ranked: ranked.clone(),
            },
            &ranked,
        );

        let hub = d.candidates.iter().find(|c| c.name == "hub").unwrap();
        let laptop = d.candidates.iter().find(|c| c.name == "laptop").unwrap();
        let local = d.candidates.iter().find(|c| c.name == "local").unwrap();
        assert_eq!(hub.rank, Some(0));
        assert!(hub.selected);
        assert_eq!(laptop.rank, Some(1));
        assert!(!laptop.selected);
        assert_eq!(local.rank, None);
        assert!(!local.selected);
        assert_eq!(d.candidates.iter().filter(|c| c.selected).count(), 1);
    }

    #[test]
    fn gated_decision_has_no_candidates_but_names_the_gate() {
        let b = DecisionBuilder::new("req-2", DecisionPath::RankedOicp, facts());
        let d = b.finish(
            Verdict::Gated {
                gate: "not_offload_eligible".into(),
            },
            &[],
        );
        assert!(d.candidates.is_empty());
        assert!(matches!(d.verdict, Verdict::Gated { .. }));
    }

    #[test]
    fn events_round_trip_through_json() {
        let mut b = DecisionBuilder::new("req-3", DecisionPath::RankedOicp, facts());
        b.push_candidate(candidate("local", 0.5));
        b.exclude("sick-peer", None, ExclusionReason::Quarantined);
        let d = b.finish(Verdict::StayLocal, &[]);
        let ev = DecisionEvent::Decision(Box::new(d));
        let line = serde_json::to_string(&ev).unwrap();
        let back: DecisionEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(ev, back);

        let out = DecisionEvent::Outcome(Box::new(RoutingOutcome {
            schema: DECISION_LOG_SCHEMA.into(),
            decision_id: "d0-x".into(),
            oicp_request_id: "req-3".into(),
            ts_unix_ms: 1,
            served_by: ServedBy::LocalFallback {
                model_id: "m".into(),
            },
            attempt_index: 2,
            ttft_ms: Some(120.0),
            total_ms: Some(4000.0),
            output_tokens: Some(250),
            shed: false,
            error: None,
            failovers: Vec::new(),
        }));
        let line = serde_json::to_string(&out).unwrap();
        assert_eq!(out, serde_json::from_str::<DecisionEvent>(&line).unwrap());
    }

    #[test]
    fn capture_sink_separates_halves() {
        let sink: Arc<dyn DecisionSink> = Arc::new(CaptureDecisionSink::new());
        let b = DecisionBuilder::new("req-4", DecisionPath::NamedModel, facts());
        let id = b.decision_id().to_string();
        emit_decision(&sink, b.finish(Verdict::StayLocal, &[]));

        let ctx = OutcomeContext {
            sink: Arc::clone(&sink),
            decision_id: id.clone(),
            oicp_request_id: "req-4".into(),
            served_by: ServedBy::Local {
                model_id: "m".into(),
            },
            attempt_index: 0,
            failovers: Vec::new(),
        };
        ctx.complete(Some(90.0), Some(1000.0), Some(42));

        // Downcast-free assertion: hold the concrete type too.
        let concrete = Arc::new(CaptureDecisionSink::new());
        let dyn_sink: Arc<dyn DecisionSink> = concrete.clone();
        emit_decision(
            &dyn_sink,
            DecisionBuilder::new("req-5", DecisionPath::RankedOicp, facts())
                .finish(Verdict::StayLocal, &[]),
        );
        assert_eq!(concrete.decisions().len(), 1);
        assert_eq!(concrete.outcomes().len(), 0);
    }

    #[test]
    fn decision_and_outcome_join_on_decision_id() {
        let concrete = Arc::new(CaptureDecisionSink::new());
        let sink: Arc<dyn DecisionSink> = concrete.clone();
        let b = DecisionBuilder::new("req-6", DecisionPath::RankedOicp, facts());
        let id = b.decision_id().to_string();
        emit_decision(&sink, b.finish(Verdict::StayLocal, &[]));
        OutcomeContext {
            sink: Arc::clone(&sink),
            decision_id: id.clone(),
            oicp_request_id: "req-6".into(),
            served_by: ServedBy::Failed,
            attempt_index: 1,
            failovers: vec![FailoverAttempt {
                peer: "hub".into(),
                error: "503 Service Unavailable".into(),
                shed: true,
                yield_retry_after_secs: None,
            }],
        }
        .failed("503 Service Unavailable".into(), true);

        let events = concrete.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].decision_id(), events[1].decision_id());
        assert_eq!(events[0].decision_id(), id);
    }

    #[test]
    fn shed_classifier_separates_congestion_from_transport_failure() {
        assert!(looks_shed("HTTP 503 Service Unavailable"));
        assert!(looks_shed("429 Too Many Requests"));
        assert!(!looks_shed("connection refused"));
        assert!(!looks_shed("dns error: no such host"));
    }

    #[test]
    fn benchmark_age_is_stamped_from_measurement_time() {
        let bench = BenchmarkResult {
            baseline_model_id: "b".into(),
            baseline_size_gb: 4.0,
            pp_tok_s: 400.0,
            tg_tok_s: 30.0,
            measured_at: 1_000,
        };
        let inputs = CandidateInputs::from_observations(
            &NodeObservations::default(),
            LoadSource::SelfObserved,
        )
        .with_benchmark(Some(&bench), 1_600);
        assert_eq!(inputs.bench_age_secs, Some(600));
        assert_eq!(inputs.bench_tg_tok_s, Some(30.0));
    }

    #[test]
    fn jsonl_sink_appends_one_line_per_record() {
        let dir = std::env::temp_dir().join(format!("decision-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.jsonl");
        let sink = TracingDecisionSink::to_path(&path).unwrap();
        for i in 0..3 {
            sink.record(DecisionEvent::Decision(Box::new(
                DecisionBuilder::new(&format!("r{i}"), DecisionPath::RankedOicp, facts())
                    .finish(Verdict::StayLocal, &[]),
            )));
        }
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in lines {
            let ev: DecisionEvent = serde_json::from_str(l).unwrap();
            assert!(matches!(ev, DecisionEvent::Decision(_)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
