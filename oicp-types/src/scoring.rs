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
mod tests {
    use super::*;
    use crate::manifest::{ModelStatus, ProviderModel};
    use crate::version::OICP_VERSION;

    // ───── Scoring ──────────────────────────────────────────

    fn claim(
        hint: CapabilityHint,
        lc: LatencyClass,
        ctx: u32,
        out: u32,
        aff: f32,
    ) -> CapabilityClaim {
        CapabilityClaim::new(hint, lc, ctx, out, aff)
    }

    fn req_with(
        hint: CapabilityHint,
        lc: LatencyClass,
        ctx: u32,
        out: u32,
    ) -> InferenceRequirements {
        InferenceRequirements::new()
            .with_hint(hint)
            .with_latency_class(lc)
            .with_context_tokens(ctx)
            .with_max_output_tokens(out)
    }

    #[test]
    fn hint_match_exact_is_one() {
        assert_eq!(
            hint_match_score(&CapabilityHint::code(), &CapabilityHint::code()),
            1.0
        );
        assert_eq!(
            hint_match_score(&CapabilityHint::general(), &CapabilityHint::general()),
            1.0
        );
    }

    #[test]
    fn hint_match_general_request_against_specific_claim_is_zero() {
        assert_eq!(
            hint_match_score(&CapabilityHint::code(), &CapabilityHint::general()),
            0.0
        );
        assert_eq!(
            hint_match_score(
                &CapabilityHint::extension("biomed").unwrap(),
                &CapabilityHint::general()
            ),
            0.0
        );
    }

    #[test]
    fn hint_match_specific_request_with_general_claim_is_fallback() {
        assert_eq!(
            hint_match_score(&CapabilityHint::general(), &CapabilityHint::code()),
            HINT_GENERAL_FALLBACK_SCORE
        );
    }

    #[test]
    fn hint_match_specific_vs_different_specific_is_zero() {
        assert_eq!(
            hint_match_score(
                &CapabilityHint::code(),
                &CapabilityHint::extension("prose").unwrap()
            ),
            0.0
        );
    }

    #[test]
    fn latency_match_exact_adjacent_and_gap() {
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Fast),
            1.0
        );
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Normal),
            LATENCY_ADJACENT_SCORE
        );
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Extended),
            LATENCY_TWO_CLASS_SCORE
        );
    }

    #[test]
    fn score_hard_gate_eliminates_insufficient_context() {
        let c = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_000,
            2_000,
            0.9,
        );
        let over = req_with(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_001,
            1_000,
        );
        assert_eq!(score_claim_for_request(&c, &over), None);
    }

    #[test]
    fn score_wrong_specialization_returns_none() {
        let c = claim(
            CapabilityHint::extension("prose").unwrap(),
            LatencyClass::Normal,
            16_000,
            2_000,
            0.9,
        );
        let req = req_with(CapabilityHint::code(), LatencyClass::Normal, 4_000, 1_000);
        assert_eq!(score_claim_for_request(&c, &req), None);
    }

    #[test]
    fn score_full_formula_multiplies_hint_latency_affinity() {
        // code/fast claim against code/fast request: 1.0 × 1.0 × 0.9 = 0.9.
        let c = claim(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.9,
        );
        let req = req_with(CapabilityHint::code(), LatencyClass::Fast, 4_000, 500);
        let score = score_claim_for_request(&c, &req).expect("passes");
        // hint=1.0, latency=Fast vs Normal adjacent=0.8, affinity=0.9
        assert!((score - 0.72).abs() < 1e-6, "got {score}");
    }

    // ───── v0.3 §7 — observation helpers ───────────────────

    fn obs_with(in_flight: u32, failures: f32, samples: u32) -> NodeObservations {
        NodeObservations {
            in_flight,
            p50_latency_ms: 0,
            p95_latency_ms: 0,
            recent_failure_rate: failures,
            samples,
            ttft_ewma_ms: 0.0,
            tg_tok_s_ewma: 0.0,
        }
    }

    #[test]
    fn effective_affinity_trusts_claim_with_zero_samples() {
        let obs = obs_with(0, 0.8, 0); // 80% failure claim — ignored
        assert!(
            (effective_affinity(0.9, &obs) - 0.9).abs() < 1e-6,
            "zero-sample observations must not override the claim"
        );
    }

    #[test]
    fn effective_affinity_fully_applies_observation_past_threshold() {
        let obs = obs_with(0, 0.2, CONFIDENCE_SAMPLES);
        // claim 0.9, failure 0.2 → 0.9 × (1 - 1.0 × 0.2) = 0.72
        let eff = effective_affinity(0.9, &obs);
        assert!((eff - 0.72).abs() < 1e-6, "got {eff}");
    }

    #[test]
    fn effective_affinity_ramps_observation_weight() {
        // At half CONFIDENCE_SAMPLES the observation should weigh 50%.
        let obs = obs_with(0, 0.4, CONFIDENCE_SAMPLES / 2);
        // 0.8 × (1 - 0.5 × 0.4) = 0.8 × 0.8 = 0.64
        let eff = effective_affinity(0.8, &obs);
        assert!((eff - 0.64).abs() < 1e-6, "got {eff}");
    }

    #[test]
    fn effective_affinity_clamps_and_handles_nan() {
        assert_eq!(effective_affinity(1.5, &obs_with(0, 0.0, 0)), 1.0);
        assert_eq!(effective_affinity(-0.2, &obs_with(0, 0.0, 0)), 0.0);
        assert_eq!(effective_affinity(f32::NAN, &obs_with(0, 0.0, 0)), 0.0);
    }

    #[test]
    fn load_penalty_is_one_at_zero_in_flight() {
        assert_eq!(load_penalty(&obs_with(0, 0.0, 0)), 1.0);
    }

    #[test]
    fn load_penalty_decreases_monotonically() {
        let ten = load_penalty(&obs_with(10, 0.0, 0));
        let twenty = load_penalty(&obs_with(20, 0.0, 0));
        let fifty = load_penalty(&obs_with(50, 0.0, 0));
        assert!(ten > twenty);
        assert!(twenty > fifty);
        assert!(
            fifty > 0.0,
            "must never collapse to zero — that would eliminate the node entirely"
        );
    }

    #[test]
    fn load_penalty_curve_hits_documented_points() {
        // Check the spec comment's example points within 10%.
        let ten = load_penalty(&obs_with(10, 0.0, 0));
        assert!((ten - 0.667).abs() < 0.01, "got {ten}");
        let twenty = load_penalty(&obs_with(20, 0.0, 0));
        assert!((twenty - 0.5).abs() < 0.01, "got {twenty}");
    }

    #[test]
    fn locality_bonus_order() {
        assert!(locality_bonus(NodeLocality::Local) > locality_bonus(NodeLocality::Near));
        assert!(locality_bonus(NodeLocality::Near) > locality_bonus(NodeLocality::Far));
        assert_eq!(locality_bonus(NodeLocality::Far), 1.0);
    }

    #[test]
    fn locality_bonus_strength_matches_spec() {
        // A local 0.7-affinity node must beat a remote 0.8-affinity
        // node per the spec's worked example.
        let local = 0.7 * locality_bonus(NodeLocality::Local);
        let far = 0.8 * locality_bonus(NodeLocality::Far);
        assert!(local > far, "local {local} must beat far {far}");
    }

    #[test]
    fn cold_start_ramps_from_min_to_one() {
        assert_eq!(cold_start_weight(0), COLD_START_MIN_WEIGHT);
        assert_eq!(cold_start_weight(COLD_START_SAMPLES), 1.0);
        assert_eq!(cold_start_weight(COLD_START_SAMPLES + 1_000), 1.0);
        // Monotonic between 0 and the threshold.
        let mid = cold_start_weight(COLD_START_SAMPLES / 2);
        assert!(mid > COLD_START_MIN_WEIGHT && mid < 1.0, "got {mid}");
    }

    // ───── v0.3 §3 — throughput scoring ────────────────────

    fn obs_with_throughput(samples: u32, tg: f64) -> NodeObservations {
        NodeObservations {
            in_flight: 0,
            p50_latency_ms: 0,
            p95_latency_ms: 0,
            recent_failure_rate: 0.0,
            samples,
            ttft_ewma_ms: 0.0,
            tg_tok_s_ewma: tg,
        }
    }

    fn benchmark(baseline_size_gb: f32, tg: f32) -> BenchmarkResult {
        BenchmarkResult {
            baseline_model_id: "bonsai-8b-q1_0".into(),
            baseline_size_gb,
            pp_tok_s: 100.0,
            tg_tok_s: tg,
            measured_at: 1_700_000_000,
        }
    }

    #[test]
    fn throughput_factor_neutral_without_data() {
        let obs = obs_with_throughput(0, 0.0);
        assert_eq!(
            throughput_factor(&obs, 8.0, None),
            1.0,
            "no observations + no benchmark must be neutral 1.0"
        );
        assert_eq!(throughput_factor_source(&obs, None), "neutral");
    }

    #[test]
    fn throughput_factor_floor_at_low_observed_rate() {
        let obs = obs_with_throughput(100, 3.0);
        assert!(
            (throughput_factor(&obs, 8.0, None) - THROUGHPUT_FLOOR).abs() < 1e-6,
            "3 tok/s observed must clamp to floor"
        );
        assert_eq!(throughput_factor_source(&obs, None), "observed");
    }

    #[test]
    fn throughput_factor_one_at_or_above_reference() {
        let obs = obs_with_throughput(100, 25.0);
        assert_eq!(
            throughput_factor(&obs, 8.0, None),
            1.0,
            ">= reference rate must produce 1.0"
        );
    }

    #[test]
    fn throughput_factor_scales_linearly_in_band() {
        // 10 tok/s observed → 10/20 = 0.5
        let obs = obs_with_throughput(100, 10.0);
        let f = throughput_factor(&obs, 8.0, None);
        assert!((f - 0.5).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn throughput_factor_falls_back_to_benchmark_estimate_below_threshold() {
        // Below sample threshold → ignore observation, use benchmark.
        let obs = obs_with_throughput(2, 100.0); // huge observation but ignored
        let bench = benchmark(8.0, 20.0);
        // Same model size: ratio 1.0, estimated tg = 20 → factor 1.0.
        let f = throughput_factor(&obs, 8.0, Some(&bench));
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
        assert_eq!(
            throughput_factor_source(&obs, Some(&bench)),
            "benchmark_estimate"
        );
    }

    #[test]
    fn throughput_factor_extrapolates_by_size_ratio() {
        // Baseline 8GB at 20 tok/s. Candidate 16GB → expected ~10 tok/s.
        let bench = benchmark(8.0, 20.0);
        let obs = obs_with_throughput(0, 0.0);
        let f = throughput_factor(&obs, 16.0, Some(&bench));
        // 10/20 = 0.5
        assert!((f - 0.5).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn throughput_factor_observed_overrides_benchmark() {
        // Past threshold, observed wins even when benchmark exists.
        let obs = obs_with_throughput(100, 25.0); // saturates to 1.0
        let bench = benchmark(8.0, 5.0); // would estimate 0.3
        let f = throughput_factor(&obs, 8.0, Some(&bench));
        assert_eq!(f, 1.0);
    }

    #[test]
    fn throughput_factor_zero_size_is_safe() {
        // Defensive: a candidate with size_gb==0 must not divide-by-zero.
        let bench = benchmark(8.0, 20.0);
        let obs = obs_with_throughput(0, 0.0);
        let f = throughput_factor(&obs, 0.0, Some(&bench));
        // ratio defaults to 1.0; estimated rate = 20 → factor 1.0.
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn benchmark_result_is_serde_round_trip() {
        let b = benchmark(8.0, 17.5);
        let json = serde_json::to_string(&b).unwrap();
        let back: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn local_slow_peer_loses_to_remote_fast_peer_after_throughput() {
        // Spec §3.3 composition stability: a local 0.72-affinity peer
        // running at 3 tok/s must lose to a remote 0.78-affinity peer
        // running at 25 tok/s, even after the locality bonus is
        // applied. This pins that throughput_factor dominates the
        // composition when one peer is genuinely slow.
        let local_obs = obs_with_throughput(100, 3.0);
        let remote_obs = obs_with_throughput(100, 25.0);
        let local_score = 0.72_f32
            * locality_bonus(NodeLocality::Local)
            * throughput_factor(&local_obs, 8.0, None);
        let remote_score = 0.78_f32
            * locality_bonus(NodeLocality::Far)
            * throughput_factor(&remote_obs, 8.0, None);
        assert!(
            remote_score > local_score,
            "remote fast {remote_score} must beat local slow {local_score}"
        );
    }

    #[test]
    fn score_coder_collective_ranks_specialist_above_generalist() {
        let qwen_coder = claim(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.95,
        );
        let llama_70b = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            64_000,
            4_000,
            0.85,
        );
        let req = req_with(CapabilityHint::code(), LatencyClass::Normal, 16_000, 2_000);
        let a = score_claim_for_request(&qwen_coder, &req).unwrap();
        let b = score_claim_for_request(&llama_70b, &req).unwrap();
        assert!(a > b, "coder {a} must beat general {b}");
        assert!((a - 0.95).abs() < 1e-6);
        // general fallback: 0.5 × 1.0 × 0.85 = 0.425.
        assert!((b - 0.425).abs() < 1e-6);
    }

    // ── score_with_adjustments — the composed SSOT scorer ────────
    //
    // The first block pins the full product (mirrors the golden
    // vector in sovereign-mesh's oicp_select tests, which pinned the
    // pre-SSOT implementation). The scenario tests re-pin the nine
    // behavioral scenarios from the deleted
    // commonwealth-inference/tests/oicp_v03_observations.rs against
    // the SSOT fn directly.

    fn quiet_obs() -> NodeObservations {
        NodeObservations {
            samples: 100, // fully ramped, no cold-start penalty
            ..Default::default()
        }
    }

    fn score(obs: &NodeObservations, locality: NodeLocality, avail: Option<f32>) -> f32 {
        score_with_adjustments(0.8, 0.9, obs, locality, 8.0, None, avail).final_score
    }

    #[test]
    fn composed_product_all_factors_active_golden() {
        let obs = NodeObservations {
            in_flight: 10,
            samples: 10,
            recent_failure_rate: 0.1,
            tg_tok_s_ewma: 10.0,
            ..Default::default()
        };
        let b = score_with_adjustments(0.5, 0.95, &obs, NodeLocality::Near, 8.0, None, None);
        assert!((b.observation_mult - 0.98).abs() < 1e-6);
        assert!((b.load_penalty - 2.0 / 3.0).abs() < 1e-6);
        assert!((b.locality_bonus - 1.05).abs() < 1e-6);
        assert!((b.cold_start_weight - 0.85).abs() < 1e-6);
        assert!((b.throughput_factor - 0.5).abs() < 1e-6);
        assert_eq!(b.throughput_source, "observed");
        assert!((b.availability - 1.0).abs() < 1e-6, "None ⇒ neutral 1.0");
        let expected = 0.5_f32 * 0.98 * (2.0 / 3.0) * 1.05 * 0.85 * 0.5;
        assert!((b.final_score - expected).abs() < 1e-6);
    }

    #[test]
    fn availability_none_is_bit_identical_to_pre_adoption_product() {
        // The adoption contract: availability=None reproduces the old
        // (term-free) formula exactly — same product, no epsilon.
        let obs = NodeObservations {
            in_flight: 3,
            samples: 30,
            recent_failure_rate: 0.05,
            tg_tok_s_ewma: 18.0,
            ..Default::default()
        };
        let without = score_with_adjustments(0.7, 0.85, &obs, NodeLocality::Far, 4.0, None, None);
        let manual = 0.7
            * (effective_affinity(0.85, &obs) / 0.85)
            * load_penalty(&obs)
            * locality_bonus(NodeLocality::Far)
            * cold_start_weight(obs.samples)
            * throughput_factor(&obs, 4.0, None);
        assert_eq!(without.final_score.to_bits(), manual.to_bits());
    }

    #[test]
    fn availability_clamps_floor_and_ceiling() {
        let obs = quiet_obs();
        let floor = score_with_adjustments(0.8, 0.9, &obs, NodeLocality::Far, 8.0, None, Some(0.0));
        assert!(
            (floor.availability - 0.2).abs() < 1e-6,
            "floor 0.2 keeps a busy peer routable"
        );
        let ceil = score_with_adjustments(0.8, 0.9, &obs, NodeLocality::Far, 8.0, None, Some(2.0));
        assert!((ceil.availability - 1.0).abs() < 1e-6);
    }

    #[test]
    fn busy_peer_loses_to_idle_equal_peer_via_availability() {
        // The decided behavior change (2026-06-10): the gossiped
        // availability signal now affects routing. Equal peers,
        // availability 0.2 vs 1.0 — idle wins.
        let obs = quiet_obs();
        let busy = score(&obs, NodeLocality::Far, Some(0.2));
        let idle = score(&obs, NodeLocality::Far, Some(1.0));
        assert!(idle > busy * 4.9, "0.2 vs 1.0 is a 5× score gap");
    }

    // ── re-pinned oicp_v03_observations scenarios ────────────────

    #[test]
    fn thundering_herd_shifts_traffic_to_idle_peer() {
        let mut herd = quiet_obs();
        herd.in_flight = 20; // load_penalty 0.5
        let idle = quiet_obs();
        assert!(score(&idle, NodeLocality::Far, None) > score(&herd, NodeLocality::Far, None));
    }

    #[test]
    fn low_load_keeps_traffic_on_specialist() {
        // A specialist (higher claim score) under LIGHT load still
        // beats an idle generalist: 2 in-flight ⇒ penalty ~0.91.
        let mut light = quiet_obs();
        light.in_flight = 2;
        let specialist =
            score_with_adjustments(1.0, 1.0, &light, NodeLocality::Far, 8.0, None, None);
        let generalist =
            score_with_adjustments(0.5, 0.85, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
        assert!(specialist.final_score > generalist.final_score);
    }

    #[test]
    fn failing_node_loses_to_reliable_peer() {
        let mut flaky = quiet_obs();
        flaky.recent_failure_rate = 0.5; // past ramp ⇒ halves affinity
        assert!(
            score(&quiet_obs(), NodeLocality::Far, None) > score(&flaky, NodeLocality::Far, None)
        );
    }

    #[test]
    fn cold_start_deprioritizes_new_peer_vs_proven_peer() {
        let newcomer = NodeObservations::default(); // samples 0 ⇒ 0.7×
        assert!(
            score(&quiet_obs(), NodeLocality::Far, None)
                > score(&newcomer, NodeLocality::Far, None)
        );
    }

    #[test]
    fn cold_start_fully_ramped_after_threshold_samples() {
        let mut ramped = NodeObservations::default();
        ramped.samples = COLD_START_SAMPLES;
        let b = score_with_adjustments(0.8, 0.9, &ramped, NodeLocality::Far, 8.0, None, None);
        assert!((b.cold_start_weight - 1.0).abs() < 1e-6);
    }

    #[test]
    fn local_node_wins_over_remote_with_higher_affinity() {
        // Locality 1.15 vs 1.0 outweighs a modest claim-score edge:
        // 0.78·1.15 > 0.8·1.0.
        let local = score_with_adjustments(
            0.78,
            0.9,
            &quiet_obs(),
            NodeLocality::Local,
            8.0,
            None,
            None,
        );
        let remote =
            score_with_adjustments(0.8, 0.95, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
        assert!(local.final_score > remote.final_score);
    }

    #[test]
    fn near_lan_peer_beats_far_internet_peer_at_equal_affinity() {
        assert!(
            score(&quiet_obs(), NodeLocality::Near, None)
                > score(&quiet_obs(), NodeLocality::Far, None)
        );
    }

    #[test]
    fn slow_peer_loses_to_fast_peer_under_throughput_scoring() {
        let mut slow = quiet_obs();
        slow.tg_tok_s_ewma = 4.0; // 4/20 ⇒ clamps to floor 0.3
        let mut fast = quiet_obs();
        fast.tg_tok_s_ewma = 30.0; // ≥ reference ⇒ 1.0
        assert!(score(&fast, NodeLocality::Far, None) > score(&slow, NodeLocality::Far, None));
    }

    #[test]
    fn neutral_throughput_preserves_pre_throughput_routing_behavior() {
        // No observed throughput and no benchmark ⇒ factor 1.0 and
        // the decision reduces to the other factors.
        let b = score_with_adjustments(0.8, 0.9, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
        assert!((b.throughput_factor - 1.0).abs() < 1e-6);
        assert_eq!(b.throughput_source, "neutral");
    }

    // ── best_claim_for_request / pick_better ─────────────────────

    #[test]
    fn pick_better_smaller_size_wins_score_tie() {
        let big = ScoredClaim {
            score: 0.8,
            size_gb: Some(16.0),
            model_id: "big".into(),
            claim_affinity: 0.8,
        };
        let small = ScoredClaim {
            score: 0.8,
            size_gb: Some(5.0),
            model_id: "small".into(),
            claim_affinity: 0.8,
        };
        assert_eq!(pick_better(big, small).model_id, "small");
    }

    fn manifest_model(
        id: &str,
        size_gb: f32,
        hint: CapabilityHint,
        affinity: f32,
    ) -> ProviderModel {
        ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: Some(size_gb),
            claims: vec![CapabilityClaim::new(
                hint,
                LatencyClass::Normal,
                32_768,
                4_000,
                affinity,
            )],
            fingerprint: None,
        }
    }

    #[test]
    fn best_claim_for_request_picks_highest_scoring_model() {
        let manifest = ProviderManifest {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models: vec![
                manifest_model("generalist", 16.0, CapabilityHint::general(), 0.85),
                manifest_model("coder", 8.0, CapabilityHint::code(), 0.95),
            ],
            knowledge: None,
            federation: None,
            features: Vec::new(),
        };
        let req = InferenceRequirements {
            oicp_version: OICP_VERSION.to_string(),
            capability_hint: Some(CapabilityHint::code()),
            latency_class: Some(LatencyClass::Normal),
            context_tokens: Some(8_000),
            max_output_tokens: Some(1_000),
            privacy: None,
            request_id: None,
        };
        let best = best_claim_for_request(&manifest, &req).unwrap();
        // Specialist at exact-hint 0.95 beats generalist's 0.5-fallback path.
        assert_eq!(best.model_id, "coder");
    }
}
