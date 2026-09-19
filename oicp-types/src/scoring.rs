// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reference scoring (v0.3 §6/§7): the protocol-level claim scorer,
//! the operational-state model, and the composed single-source-of-truth
//! routing scorer.

use serde::{Deserialize, Serialize};

use crate::capability::{CapabilityClaim, CapabilityHint, LatencyClass};
use crate::manifest::ProviderManifest;
use crate::requirements::InferenceRequirements;

// -----------------------------------------------------------------
// v0.3 §6 — Reference scoring function
// -----------------------------------------------------------------

/// Hint-match score when a request asks for a specific hint but only
/// a `general` claim is available. Decisively worse than an exact
/// match (1.0) yet noticeably better than a wrong specialization
/// (0.0) so the scheduler prefers any node with the requested
/// specialty over a general fallback, but still routes work
/// somewhere if no specialist is reachable.
pub const HINT_GENERAL_FALLBACK_SCORE: f32 = 0.5;

/// Latency-match score when claim and request classes are one class
/// apart (fast↔normal or normal↔extended). Latency mismatch is a
/// soft deprioritization per §5 — a node advertising fast work can
/// still serve normal work, just with a weaker fit.
///
/// The values 0.8 / 0.5 are NOT derived from spec §5 (which mandates
/// only "soft deprioritization") — they are this reference
/// implementation's choices, sized so one class of mismatch loses to
/// any same-class claim within ~0.25 affinity, and two classes lose
/// to anything plausible. Pinned by tests for scheduler interop;
/// change them only with a routing A/B in hand.
pub const LATENCY_ADJACENT_SCORE: f32 = 0.8;

/// Latency-match score when claim and request classes are two apart
/// (fast↔extended). The widest soft deprioritization. Same
/// non-normative-but-pinned status as [`LATENCY_ADJACENT_SCORE`].
pub const LATENCY_TWO_CLASS_SCORE: f32 = 0.5;

/// Score how well a claim's `hint` covers a request for `req_hint`.
///
/// - Exact match (same standardized hint, or same extension hint) →
///   `1.0`.
/// - Request specific (e.g., `code`, `x:prose`), claim `general` →
///   [`HINT_GENERAL_FALLBACK_SCORE`] (0.5) — the documented spec
///   §4.2 fallback: "falling back to general when no node advertises
///   the requested hint."
/// - Every other non-match → `0.0`. In particular, a request for
///   `general` against a specific-hint claim (code, x:prose, …) is
///   **not** a free 1.0. The spec §4.1 requirement "every node
///   serving inference must support general as a minimum" is an
///   obligation on the **advertiser**: a node that wants to serve
///   general work must publish a general claim. Scoring a code-
///   specialist claim at 1.0 for a general request would subvert
///   that obligation and let a specialist silently absorb every
///   general-hinted request on the mesh.
pub fn hint_match_score(claim_hint: &CapabilityHint, req_hint: &CapabilityHint) -> f32 {
    if claim_hint == req_hint {
        return 1.0;
    }
    // Request asks for a specific hint; claim offers general —
    // documented fallback path (§4.2).
    if claim_hint.as_str() == CapabilityHint::GENERAL
        && req_hint.as_str() != CapabilityHint::GENERAL
    {
        return HINT_GENERAL_FALLBACK_SCORE;
    }
    // All other mismatches (request general vs specific claim; two
    // different specifics) are zero score → eliminated from ranking
    // by the scheduler.
    0.0
}

/// Score how well a claim's `latency_class` covers a request for
/// `req_class`.
///
/// - Exact match → `1.0`.
/// - Adjacent class → [`LATENCY_ADJACENT_SCORE`] (0.8).
/// - Two-class gap → [`LATENCY_TWO_CLASS_SCORE`] (0.5).
pub fn latency_match_score(claim_class: LatencyClass, req_class: LatencyClass) -> f32 {
    fn rank(c: LatencyClass) -> i32 {
        match c {
            LatencyClass::Fast => 0,
            LatencyClass::Normal => 1,
            LatencyClass::Extended => 2,
        }
    }
    match rank(claim_class).abs_diff(rank(req_class)) {
        0 => 1.0,
        1 => LATENCY_ADJACENT_SCORE,
        _ => LATENCY_TWO_CLASS_SCORE,
    }
}

// -----------------------------------------------------------------
// v0.3 §7 — Operational state (non-normative)
//
// The spec explicitly leaves observation, load, and locality
// modelling to each scheduler (§7 "operational concerns are local").
// These types + helpers are the shared reference model so Sovereign
// + Commonwealth + mesh-peer schedulers all rank (node, claim) pairs
// with the same second-pass scoring math. Nothing here is on the
// wire.
// -----------------------------------------------------------------

/// Sample-count threshold above which observed-performance fully
/// replaces claimed affinity in [`effective_affinity`]. Below this
/// the claim still dominates; at this value and above the observed
/// health score fully applies.
pub const CONFIDENCE_SAMPLES: u32 = 50;

/// Sample threshold for cold-start ramping in [`cold_start_weight`].
/// A brand-new node starts at [`COLD_START_MIN_WEIGHT`] and ramps
/// linearly to `1.0` over this many observed samples.
pub const COLD_START_SAMPLES: u32 = 20;

/// Minimum routing weight a brand-new node gets before any
/// observations exist. Non-trivially below `1.0` so new peers
/// don't absorb a burst before they've proven reliable, but high
/// enough that a peer with a strictly-better advertised affinity
/// can still win the first request — otherwise the scheduler
/// would never actually ROUTE to new peers and cold-start would
/// become a trap. `0.7` corresponds to "new peer gets 70% of the
/// weight it would at full ramp", roughly the same deprioritization
/// a real-world load balancer uses for fresh backends.
pub const COLD_START_MIN_WEIGHT: f32 = 0.7;

/// Load-penalty coefficient: `load_penalty = 1 / (1 + in_flight * C)`.
/// At the default 0.05, 5 in-flight requests drop the penalty to
/// ~0.8; 20 in-flight drops to ~0.5 — enough to divert the next
/// burst to a second-choice node without starving the popular one.
pub const LOAD_COEFFICIENT: f32 = 0.05;

/// Locality bonus: same-machine local serving.
pub const LOCALITY_LOCAL_BONUS: f32 = 1.15;

/// Locality bonus: same-LAN peer.
pub const LOCALITY_NEAR_BONUS: f32 = 1.05;

/// Locality bonus: cross-internet peer (no bonus).
pub const LOCALITY_FAR_BONUS: f32 = 1.0;

/// Reference token-generation rate that maps to a throughput factor of
/// `1.0` in [`throughput_factor`]. Anything at or above this rate is
/// treated as fully responsive; lower rates scale linearly down toward
/// the floor. 20 tok/s is the "good for interactive use" inflection
/// point — below it conversation feels sluggish to a human.
pub const THROUGHPUT_REFERENCE_TG_TOK_S: f32 = 20.0;

/// Floor for [`throughput_factor`]: a node observed at very low
/// throughput is still routable as a last resort. Without a floor a
/// 3 tok/s peer would score `0.15×` and effectively never receive
/// traffic, even when it is the only candidate that satisfies the
/// hard gates. The floor preserves reachability while still tilting
/// routing decisively toward faster peers.
pub const THROUGHPUT_FLOOR: f32 = 0.3;

/// Where a node sits relative to the scheduler making the routing
/// decision. Derived from the scheduler's network topology — not
/// advertised by the peer. Protocol-independent: every scheduler
/// resolves its own `(peer → locality)` map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NodeLocality {
    /// Same process or machine. No network hop.
    Local,
    /// Same LAN. Single-digit-ms hop.
    Near,
    /// Cross-internet. Tens of ms hop, up to hundreds for relayed
    /// paths. Default for unknown peers.
    #[default]
    Far,
}

/// Per-node operational observations recorded by the scheduler.
///
/// Updated as requests complete: `in_flight` increments on dispatch
/// and decrements on completion; latency and failure metrics roll
/// over a recent window (typical: last 50 requests). `samples` is
/// the total observation count — gates cold-start ramping and
/// observation-vs-claim confidence blending.
///
/// Observations are **local** to each scheduler per §7 — they are
/// never advertised between nodes.
///
/// `Serialize`/`Deserialize` do **not** contradict that: nothing
/// gossips this type. The derives exist so a node can *export* its
/// own observation state for offline analysis and simulator
/// calibration (`SCHEDULER_QUALITY.md` P3), which is a diagnostic
/// read, not an advertisement.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NodeObservations {
    /// Currently outstanding requests on this node.
    pub in_flight: u32,
    /// Median observed latency over the recent window, in ms.
    pub p50_latency_ms: u32,
    /// 95th-percentile observed latency in ms — catches slow-path
    /// behaviour the p50 hides.
    pub p95_latency_ms: u32,
    /// Fraction of recent requests that failed (0.0 = clean,
    /// 1.0 = every recent request failed). The scheduler uses this
    /// as the primary "observed health" signal.
    pub recent_failure_rate: f32,
    /// Total observed requests this scheduler has recorded for the
    /// node. Used by [`effective_affinity`] to weight claim vs.
    /// observation, and by [`cold_start_weight`] to ramp new
    /// peers in gradually.
    pub samples: u32,
    /// EWMA (α=0.3) of time-to-first-token in milliseconds. Captures
    /// dispatch + first-token latency, the human-perceived "did it
    /// hear me" signal. Not directly used in throughput scoring but
    /// surfaced to operators in diagnostics and the desktop members
    /// panel. Zero until at least one streaming request has completed.
    pub ttft_ewma_ms: f64,
    /// EWMA (α=0.3) of observed token-generation rate in tokens per
    /// second. Source of truth for [`throughput_factor`] when at
    /// least [`THROUGHPUT_OBSERVATION_THRESHOLD`] samples have
    /// accumulated; below the threshold the scheduler falls back to
    /// the benchmark estimate. Zero before any streaming request has
    /// completed.
    pub tg_tok_s_ewma: f64,
}

/// Sample-count threshold above which observed token-generation rate
/// becomes the source of truth for [`throughput_factor`]. Below this
/// the benchmark estimate is used (or neutral 1.0 if neither is
/// present). Same magnitude as [`COLD_START_SAMPLES`] so a peer that
/// has earned full cold-start weight has also earned its observed
/// throughput signal.
pub const THROUGHPUT_OBSERVATION_THRESHOLD: u32 = 5;

/// Smoothing factor for the throughput / TTFT EWMAs.
/// Matches the latency-probe α at
/// `commonwealth-discovery::latency_probe`. Surfaces thermal
/// throttling within ~3–4 requests; lower α values would make the
/// signal sluggish, higher would make it jittery.
pub const THROUGHPUT_EWMA_ALPHA: f64 = 0.3;

/// EWMA update for the throughput-observation fields on [`NodeObservations`].
///
/// α follows [`THROUGHPUT_EWMA_ALPHA`] so this stays in lock-step with the
/// latency probe and other observation paths. A zero field means "never
/// observed", so the first sample seeds it rather than blending against zero;
/// a `None` leaves its field untouched.
///
/// Lives here, on the leaf that owns `NodeObservations`, because both the
/// serving host's stream observer and the Tier-1 mesh simulator fold
/// observations through the same arithmetic (domains
/// `REVIEW-build-mesh-sim-decouple`); one implementation, two callers
/// (ARCH principle 8).
pub fn apply_throughput_observation(
    obs: &mut NodeObservations,
    ttft_ms: Option<f64>,
    tg_tok_s: Option<f64>,
) {
    let alpha = THROUGHPUT_EWMA_ALPHA;
    if let Some(ttft) = ttft_ms {
        obs.ttft_ewma_ms = if obs.ttft_ewma_ms == 0.0 {
            ttft
        } else {
            alpha * ttft + (1.0 - alpha) * obs.ttft_ewma_ms
        };
    }
    if let Some(tg) = tg_tok_s {
        obs.tg_tok_s_ewma = if obs.tg_tok_s_ewma == 0.0 {
            tg
        } else {
            alpha * tg + (1.0 - alpha) * obs.tg_tok_s_ewma
        };
    }
}

/// Blend a claim's self-reported `affinity` with observed node
/// health.
///
/// - Zero samples → return `claimed` verbatim (trust the advertiser).
/// - Above [`CONFIDENCE_SAMPLES`] → claimed × observed health.
/// - In between: linear ramp weighted by sample count.
///
/// "Observed health" here is `1.0 - recent_failure_rate` — a node
/// with 20% recent failures has health 0.8. Latency-based health is
/// applied separately as part of the load-penalty path so the two
/// factors compound multiplicatively, not additively.
pub fn effective_affinity(claimed: f32, obs: &NodeObservations) -> f32 {
    let claim = if claimed.is_nan() {
        0.0
    } else {
        claimed.clamp(0.0, 1.0)
    };
    if obs.samples == 0 {
        return claim;
    }
    let obs_weight = (obs.samples as f32 / CONFIDENCE_SAMPLES as f32).min(1.0);
    let failure = obs.recent_failure_rate.clamp(0.0, 1.0);
    // Interpolation: claim → claim × (1 - failure) as weight → 1.0.
    claim * (1.0 - obs_weight * failure)
}

/// Multiplicative load penalty applied to a node's score. In
/// `(0.0, 1.0]` — `1.0` at zero in-flight, decreasing with load.
///
/// The curve is hyperbolic (`1 / (1 + k * n)`) rather than linear so
/// the first few in-flight requests barely penalize but the tail
/// diverges past `~1/k`. At `LOAD_COEFFICIENT = 0.05`, 10 in-flight
/// ≈ 0.67 and 20 in-flight ≈ 0.50 — enough to divert a second burst
/// without starving the popular node entirely.
pub fn load_penalty(obs: &NodeObservations) -> f32 {
    let k = LOAD_COEFFICIENT;
    let n = obs.in_flight as f32;
    1.0 / (1.0 + k * n)
}

/// Locality bonus in `[1.0, 1.15]`. Multiplicative — applied to the
/// ranked score so a local 0.7-affinity node can out-rank a remote
/// 0.8-affinity node (0.7 × 1.15 = 0.805 > 0.8 × 1.0).
pub fn locality_bonus(locality: NodeLocality) -> f32 {
    match locality {
        NodeLocality::Local => LOCALITY_LOCAL_BONUS,
        NodeLocality::Near => LOCALITY_NEAR_BONUS,
        NodeLocality::Far => LOCALITY_FAR_BONUS,
    }
}

/// Cold-start ramp weight in `[COLD_START_MIN_WEIGHT, 1.0]`. A node
/// with zero samples starts at [`COLD_START_MIN_WEIGHT`] and ramps
/// linearly to `1.0` over [`COLD_START_SAMPLES`] observations — so
/// new peers still receive routable traffic (otherwise they'd never
/// accumulate history) but don't win a burst until they've proven
/// reliable.
pub fn cold_start_weight(samples: u32) -> f32 {
    if samples >= COLD_START_SAMPLES {
        return 1.0;
    }
    let progress = samples as f32 / COLD_START_SAMPLES as f32;
    COLD_START_MIN_WEIGHT + (1.0 - COLD_START_MIN_WEIGHT) * progress
}

/// A node's measured baseline-model throughput. Recorded once at
/// daemon launch (and re-recorded when [`HardwareProfile`] changes)
/// and gossiped via [`NodeCapabilities.benchmark`]. Lets remote
/// schedulers estimate how a *different* model on the same hardware
/// would perform without running it themselves.
///
/// Wire-tolerant: every field has a serde default so an older peer's
/// `NodeCapabilities` payload (sans benchmark) deserializes cleanly
/// and the resulting `Option<BenchmarkResult>` reads as `None`.
///
/// Surfaced to `tracing=debug` via the `bench: completed` event in
/// the daemon startup path so an operator can verify the benchmark
/// ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkResult {
    /// File-stem of the model that was benchmarked (e.g.
    /// `"bonsai-8b-q1_0"`). Schedulers use this as an opaque token
    /// for cache-invalidation only — they do not parse it.
    pub baseline_model_id: String,
    /// On-disk size in GB of the benchmarked model. The same number
    /// `ProviderModel` advertises for the same model. Schedulers
    /// scale `tg_tok_s` by `baseline_size_gb / candidate_size_gb`
    /// when estimating throughput for a *different* model on this
    /// hardware.
    pub baseline_size_gb: f32,
    /// Prompt-processing throughput in tokens per second over a
    /// standardized prompt.
    pub pp_tok_s: f32,
    /// Token-generation throughput in tokens per second over a
    /// standardized prompt.
    pub tg_tok_s: f32,
    /// Unix seconds the benchmark was measured. Operators use this
    /// to spot a stale benchmark after hardware changes; schedulers
    /// don't gate on it.
    pub measured_at: u64,
}

/// Map an observed token-generation rate (or a benchmark-derived
/// estimate) to a routing multiplier in
/// `[THROUGHPUT_FLOOR, 1.0]`.
///
/// Source-of-truth ordering (spec §3.3):
///
/// 1. **Observed**: at least [`THROUGHPUT_OBSERVATION_THRESHOLD`]
///    samples accumulated → use `obs.tg_tok_s_ewma`.
/// 2. **Benchmark estimate**: the node has a [`BenchmarkResult`] →
///    scale baseline `tg_tok_s` by `baseline_size_gb /
///    candidate_size_gb` (smaller models on the same hardware run
///    faster; larger models run slower).
/// 3. **Neutral**: neither signal exists → return `1.0`.
///
/// Returning `1.0` for a zero-data peer is intentional — slotting
/// the multiplier at the end of the composition chain means a peer
/// with no benchmark and no observations behaves identically to the
/// pre-throughput scoring world. This keeps the change wire-tolerant
/// AND behaviour-tolerant: older peers and brand-new peers don't
/// suddenly drop in score.
pub fn throughput_factor(
    obs: &NodeObservations,
    candidate_size_gb: f32,
    baseline_benchmark: Option<&BenchmarkResult>,
) -> f32 {
    let observed_tg_tok_s =
        if obs.samples >= THROUGHPUT_OBSERVATION_THRESHOLD && obs.tg_tok_s_ewma > 0.0 {
            Some(obs.tg_tok_s_ewma as f32)
        } else {
            None
        };

    let estimated_tg_tok_s = match (observed_tg_tok_s, baseline_benchmark) {
        (Some(rate), _) => rate,
        (None, Some(bench)) => {
            // Smaller models on the same hardware run faster. We
            // scale linearly with model-size ratio, which is the
            // simplest defensible heuristic without running an
            // actual benchmark for the candidate. Real-world scaling
            // is sub-linear (memory bandwidth dominates) but linear
            // is good enough for *ranking*: it preserves order across
            // candidate sizes.
            let ratio = if candidate_size_gb > 0.0 {
                bench.baseline_size_gb / candidate_size_gb
            } else {
                1.0
            };
            (bench.tg_tok_s * ratio).max(0.0)
        }
        (None, None) => return 1.0,
    };

    (estimated_tg_tok_s / THROUGHPUT_REFERENCE_TG_TOK_S).clamp(THROUGHPUT_FLOOR, 1.0)
}

/// String label for a [`throughput_factor`] decision — `"observed"`,
/// `"benchmark_estimate"`, or `"neutral"`. Pure helper for the
/// `oicp_select: throughput_factor` glassbox tracing event so
/// operators see *why* a given factor was chosen, not just the
/// number.
pub fn throughput_factor_source(
    obs: &NodeObservations,
    baseline_benchmark: Option<&BenchmarkResult>,
) -> &'static str {
    if obs.samples >= THROUGHPUT_OBSERVATION_THRESHOLD && obs.tg_tok_s_ewma > 0.0 {
        "observed"
    } else if baseline_benchmark.is_some() {
        "benchmark_estimate"
    } else {
        "neutral"
    }
}

// -----------------------------------------------------------------
// v0.3 §6/§7 — the composed scorer (single source of truth)
//
// 2026-06-10 rationalization: the product below used to be
// implemented three times (sovereign-mesh `adjust_for_observations`,
// sovereign-inference `selector.rs` inline, and a dead commonwealth
// scheduler copy) and had already diverged about the availability
// term. It lives HERE, once, next to its factor helpers; consumers
// log the returned [`ScoreBreakdown`] so every routing decision is
// reconstructible from a single trace event.

/// Score-floor below which score-ties are considered "the same".
/// Floating-point noise in the claim scorer (division-by-max-level
/// produces 1/3, 2/3, 1.0 type values) shouldn't cause spurious
/// decisions where a 5.5 GB model beats a 16.5 GB model by a
/// rounding blip.
pub const SCORING_EPSILON: f32 = 1e-3;

/// A scored model pick from a single manifest: the claim score
/// (protocol-level) alongside the claim's self-reported affinity so
/// operational adjustments can compute the observed-health
/// multiplier, plus the tie-break inputs (`size_gb`, `model_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredClaim {
    pub score: f32,
    pub size_gb: Option<f32>,
    pub model_id: String,
    /// Self-reported affinity of the claim this score came from.
    pub claim_affinity: f32,
}

/// Compare two [`ScoredClaim`]s under the selection policy and
/// return the winner:
///
/// 1. Strictly higher `score` wins.
/// 2. Scores tied (within [`SCORING_EPSILON`]): smaller known
///    `size_gb` wins.
/// 3. Known size always beats unknown size on a score tie — an
///    annotated manifest entry represents curated data we trust
///    over a silent BYOM default.
/// 4. Full tie: incumbent (`cur`) wins for stability. Callers use
///    this to encode "local wins ties" and "earlier peer wins
///    duplicate-score ties".
pub fn pick_better(cur: ScoredClaim, new: ScoredClaim) -> ScoredClaim {
    if new.score > cur.score + SCORING_EPSILON {
        return new;
    }
    if cur.score > new.score + SCORING_EPSILON {
        return cur;
    }
    match (cur.size_gb, new.size_gb) {
        (Some(c), Some(n)) if n < c => new,
        (None, Some(_)) => new,
        _ => cur,
    }
}

/// Rank each (model, claim) pair in `manifest` against the request
/// and return the best [`ScoredClaim`] via v0.3 claim-based scoring.
/// Returns `None` when no claim can serve the request. Tie-break per
/// [`pick_better`]. Models advertising `status.available == false`
/// are skipped — they exist in the manifest for inventory, not for
/// routing. (Unification note, 2026-06-10: of the pre-SSOT copies,
/// sovereign-inference filtered availability and sovereign-mesh
/// didn't; the filter is the correct semantics and now applies to
/// both.)
pub fn best_claim_for_request(
    manifest: &ProviderManifest,
    req: &InferenceRequirements,
) -> Option<ScoredClaim> {
    let mut best: Option<ScoredClaim> = None;
    for model in manifest.models.iter().filter(|m| m.status.available) {
        for claim in &model.claims {
            let Some(score) = score_claim_for_request(claim, req) else {
                continue;
            };
            let cand = ScoredClaim {
                score,
                size_gb: model.size_gb,
                model_id: model.id.clone(),
                claim_affinity: claim.effective_affinity(),
            };
            best = Some(match best {
                None => cand,
                Some(cur) => pick_better(cur, cand),
            });
        }
    }
    best
}

/// Every factor of one composed scoring decision — the glassbox
/// artifact. Consumers emit this whole struct in ONE tracing event
/// per candidate, which is what makes "why did peer A beat peer B"
/// answerable from logs alone.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreBreakdown {
    pub claim_score: f32,
    /// `effective_affinity(claimed, obs) / claimed` — observed
    /// failure rate eroding the self-reported affinity.
    pub observation_mult: f32,
    pub load_penalty: f32,
    pub locality_bonus: f32,
    pub cold_start_weight: f32,
    pub throughput_factor: f32,
    /// Why that throughput factor: "observed" | "benchmark_estimate"
    /// | "neutral".
    pub throughput_source: &'static str,
    /// Gossiped `inference_availability`, clamped to `[0.2, 1.0]`;
    /// `1.0` when the caller had no signal (`None`).
    pub availability: f32,
    /// The product of everything above — the routing score.
    pub final_score: f32,
}

/// THE composed v0.3 operational scorer. `claim_score` comes from
/// [`score_claim_for_request`] / [`best_claim_for_request`];
/// `availability` is the gossiped `inference_availability` when the
/// caller has one (peers), `None` otherwise (e.g. scoring the local
/// node, whose business is already captured by `obs.in_flight`).
pub fn score_with_adjustments(
    claim_score: f32,
    claim_affinity: f32,
    obs: &NodeObservations,
    locality: NodeLocality,
    candidate_size_gb: f32,
    baseline_benchmark: Option<&BenchmarkResult>,
    availability: Option<f32>,
) -> ScoreBreakdown {
    let observation_mult = if claim_affinity > 0.0 {
        effective_affinity(claim_affinity, obs) / claim_affinity
    } else {
        1.0
    };
    let load = load_penalty(obs);
    let loc = locality_bonus(locality);
    let cold = cold_start_weight(obs.samples);
    let throughput = throughput_factor(obs, candidate_size_gb, baseline_benchmark);
    let avail = availability.map(|a| a.clamp(0.2, 1.0)).unwrap_or(1.0);
    let final_score = claim_score * observation_mult * load * loc * cold * throughput * avail;
    ScoreBreakdown {
        claim_score,
        observation_mult,
        load_penalty: load,
        locality_bonus: loc,
        cold_start_weight: cold,
        throughput_factor: throughput,
        throughput_source: throughput_factor_source(obs, baseline_benchmark),
        availability: avail,
        final_score,
    }
}

/// Score a candidate claim against a request (§6).
///
/// Applies the protocol-level portion of the full scoring function:
///
/// ```text
/// hint_match × context_fits × output_fits × latency_match × affinity
/// ```
///
/// Returns `None` when the claim fails a hard feasibility gate
/// (context or output capacity exceeded) or fails the hint gate
/// (wrong specialization). Returns `Some(score)` in `[0.0, 1.0]`
/// otherwise.
///
/// Schedulers apply their own locality bonus, load penalty, and
/// observation-adjusted affinity *outside* this function — see
/// [`effective_affinity`], [`load_penalty`], [`locality_bonus`],
/// [`cold_start_weight`].
pub fn score_claim_for_request(
    claim: &CapabilityClaim,
    req: &InferenceRequirements,
) -> Option<f32> {
    // Hard gates first per §6.
    if let Some(context) = req.context_tokens {
        if claim.max_context < context {
            return None;
        }
    }
    if let Some(output) = req.max_output_tokens {
        if claim.max_output < output {
            return None;
        }
    }

    let hint = hint_match_score(&claim.hint, &req.effective_hint());
    if hint == 0.0 {
        return None;
    }

    let latency = latency_match_score(claim.latency_class, req.effective_latency_class());

    Some(hint * latency * claim.effective_affinity())
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests;
