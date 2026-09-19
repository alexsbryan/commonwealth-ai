// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP-driven selection primitives shared across the mesh crate.
//!
//! Both sides of a mesh chat completion need the same scoring +
//! tie-break policy:
//!
//!   - **Joiner side** (`peer_inference`): score our own manifest
//!     and every reachable peer's manifest; pick the best model.
//!     Only cross the wire when a peer is strictly better than
//!     local under the (score, size_gb) policy.
//!
//!   - **Peer side** (`inference_adapter`): a chat completion
//!     request has arrived carrying an OICP envelope. Our own
//!     daemon has multiple slots loaded (Fast = 9B, Slow = 27B,
//!     say). We must pick the slot whose capabilities best match
//!     the request — otherwise the OICP work the Joiner did is
//!     wasted the moment we default to `Speed::Slow`. That pick is
//!     host code and lives in `sovereign_serving_host::slot_select`
//!     (domains row REVIEW-build-sched-split-pick-slot); the hint
//!     matching it reads is the oicp-types SSOT re-exported here.
//!
//! Keeping the primitives in one place means the two sides can't
//! drift out of agreement about what "best" means.
use oicp_types::{
    score_with_adjustments, BenchmarkResult, CapabilityHint, InferenceRequirements, LatencyClass,
    NodeLocality, NodeObservations, ScoreBreakdown, ShardingPrivacy,
    THROUGHPUT_OBSERVATION_THRESHOLD, THROUGHPUT_REFERENCE_TG_TOK_S,
};

// The scoring primitives are the oicp-types SSOT (2026-06-10
// rationalization — this module used to carry its own copies, one of
// three divergent implementations). Re-exported under the historical
// local names so call sites and tests read unchanged.
pub use oicp_types::{
    best_claim_for_request as score_manifest_for_request, pick_better,
    ScoredClaim as ModelCandidate, SCORING_EPSILON as SCORE_TIE_EPSILON,
};

/// Used by the Joiner-side selector to detect "peer pick is
/// identical to local pick" so a zero-delta routing decision
/// doesn't trip a network hop (e.g. both sides advertise the
/// same Qwen3.5-9B).
pub(crate) fn candidates_equal(a: &ModelCandidate, b: &ModelCandidate) -> bool {
    (a.score - b.score).abs() <= SCORE_TIE_EPSILON
        && a.size_gb == b.size_gb
        && a.model_id == b.model_id
}

/// SLOT_POLICY §5 — the offload gate. A request may cross the network
/// to a peer slot iff BOTH conditions hold:
///
///   1. its privacy posture permits sharding (`MeshAllowed`), and
///   2. its latency class tolerates a network hop (anything but
///      `Fast`).
///
/// Latency-`Fast` work (routing, titling, compression, the memory
/// housekeepers) normally stays home: the peer round-trip dominates the
/// inference, so a hop is a net loss even when a peer would score
/// higher on capability. That clause is an empirical claim about the
/// deciding node's hardware, and a node that has measured itself can
/// falsify it — see [`offload_verdict_with_local`]. `LocalOnly` work
/// never offloads by definition; that is the privacy contract and no
/// measurement reopens it.
///
/// Callers: `shared_primary_id` ("does the configured shared model
/// apply?"), which only ever asks about a request that HAS an envelope.
/// `select_peers_ranked` asks the same question through
/// [`offload_verdict_opt`], because it must also answer it for a request
/// with no envelope at all — see that function for why absence is not a
/// refusal. Both bottom out in [`offload_verdict`], so they cannot drift
/// out of agreement about what "offloadable" means.
pub fn offload_eligible(req: &InferenceRequirements) -> bool {
    offload_verdict(req).is_eligible()
}

/// Why a request may not be handed to a peer — or that it may.
///
/// [`offload_eligible`] is this, collapsed to a bool. The verdict exists
/// because the three ways to be ineligible are operationally different and a
/// bare `false` cannot tell an operator which one fired: a `LocalOnly`
/// request staying home is the privacy contract working, while a request
/// stopped by an exhausted budget means *some other node already forwarded
/// it*. Reporting both as "not offload eligible" is the silent-substitution
/// shape ARCH_PRINCIPLES §18.3 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadVerdict {
    /// Every gate open.
    Eligible,
    /// §3.1 privacy contract: this work never leaves the node.
    LocalOnlyPrivacy,
    /// SLOT_POLICY §5: a hop costs more than `Fast` work is worth.
    FastLatency,
    /// SLOT_POLICY §5's `Fast` gate, **stood down on this node's own
    /// measurement**. Eligible — see [`OffloadVerdict::is_eligible`].
    ///
    /// A separate variant rather than a plain `Eligible` for the same
    /// reason the three refusals are separate (principle 6): "nothing
    /// was in the way" and "the standing rule was set aside because
    /// this machine measured itself too slow to obey it" are different
    /// facts, and an operator reading why a routing call left a phone
    /// needs the second one by name.
    FastLatencyYielded,
    /// The request has already been forwarded as far as it may go.
    ForwardBudgetExhausted,
}

impl OffloadVerdict {
    /// Stable gate name for the decision log and `tracing`.
    ///
    /// Every name here is load-bearing downstream: the decision log, its
    /// replay, and their fixtures key on the string. `fast_latency_yielded`
    /// is the only one added since the budget gate, and it is added rather
    /// than folded into `eligible` because principle 1 wants the line to
    /// say *which floor decided*.
    pub fn gate(self) -> &'static str {
        match self {
            OffloadVerdict::Eligible => "eligible",
            OffloadVerdict::FastLatencyYielded => "fast_latency_yielded",
            // Unchanged from before the budget gate existed: the decision
            // log, its replay, and their fixtures all key on this string.
            OffloadVerdict::LocalOnlyPrivacy | OffloadVerdict::FastLatency => {
                "not_offload_eligible"
            }
            OffloadVerdict::ForwardBudgetExhausted => "forward_budget_exhausted",
        }
    }

    /// May this request cross to a peer?
    ///
    /// The one place the eligible set is spelled, so a new eligible verdict
    /// cannot be added without every caller seeing it (principle 8). Callers
    /// compared against `OffloadVerdict::Eligible` directly until
    /// `FastLatencyYielded` existed; a bare `==` would silently have read the
    /// yield as a refusal.
    pub fn is_eligible(self) -> bool {
        matches!(
            self,
            OffloadVerdict::Eligible | OffloadVerdict::FastLatencyYielded
        )
    }
}

/// Is this node measurably too slow to serve `Fast`-class work itself?
///
/// `None` when the node has not measured itself yet, and that is reported
/// rather than defaulted (principle 6): an unmeasured node keeps SLOT_POLICY
/// §5's standing rule, it does not get a guessed rate.
///
/// **Every input here already exists and is already maintained.** The rate is
/// `NodeObservations::tg_tok_s_ewma`, which the serving host folds in on every
/// local streaming completion (`throughput_tracking::ThroughputTarget::Local`,
/// wired at `peer_inference/provider_impl.rs:390` and `:530`) and which
/// `throughput_factor` already treats as its source of truth. The sample gate
/// is [`THROUGHPUT_OBSERVATION_THRESHOLD`], the same count that function uses
/// to decide the EWMA is trustworthy. The floor is
/// [`THROUGHPUT_REFERENCE_TG_TOK_S`], whose own doc defines it as the "good
/// for interactive use" inflection point, "below it conversation feels
/// sluggish to a human" — which is the question `LatencyClass::Fast` asks and
/// is why no new constant is minted here (principles 8, 11).
///
/// A **rate**, deliberately, and not the measured time-to-first-token: TTFT
/// mixes job sizes, so a GPU node prefilling one long synthesis would read as
/// slow, while tokens-per-second is a property of the machine and its model.
///
/// What this is NOT: a prefill measurement. No node on this mesh advertises a
/// `BenchmarkResult` and reviving the probe that used to produce one is a
/// measured quality regression — canon `dc3c9856`, `SCHEDULER_QUALITY.md`
/// §4.5 / F10, −56% latency bought with capability. The decode rate is the
/// speed signal this fleet actually collects.
fn local_is_sub_interactive(obs: &NodeObservations) -> Option<bool> {
    if obs.samples < THROUGHPUT_OBSERVATION_THRESHOLD || obs.tg_tok_s_ewma <= 0.0 {
        return None;
    }
    Some(obs.tg_tok_s_ewma < f64::from(THROUGHPUT_REFERENCE_TG_TOK_S))
}

/// The same gate, for a request whose envelope may be **absent**.
///
/// `None` is `Eligible`, and that is a deliberate reading of OICP §3.1,
/// not a loophole. §3.1 makes an absent *privacy field* default to
/// `LocalOnly` — privacy is not something a client has to remember to
/// request. It says nothing about an absent *envelope*, and the two are
/// different facts: a client that sent `{"privacy": {}}` has stated a
/// posture, while a plain OpenAI client that sent no `oicp` key at all
/// has stated nothing. Treating "stated nothing" as "asked for
/// local_only" is the same misattribution class as B1.
///
/// This is not a new rule. `peer_inference::resolve_named_dispatch` has
/// applied exactly it on the NAMED path since 2026-08-06 — both its
/// budget and its privacy predicates are `Option::is_none_or`, with the
/// rationale written out at the `privacy_permits_peer` binding — and
/// that path has served turns on a peer in this fleet's own decision log.
/// The ranked path answered the same question the other way, at
/// `has_routing_signal`, whose "there's nothing to match against the peer
/// manifests" was simply false: `effective_hint()` and
/// `effective_latency_class()` exist to give §8 defaults precisely so an
/// envelope-less request IS scoreable.
///
/// Two implementations of one policy, disagreeing (§10.6) — and the
/// disagreement is the §9.1.1 red: 100 turns at a census-verified 2-node
/// mesh, every one of them envelope-less, every one gated
/// `no_routing_signal` before a peer was ever scored. Written here rather
/// than at either call site so they cannot diverge again;
/// `both_routing_surfaces_agree_an_absent_envelope_permits_a_peer` pins it.
pub fn offload_verdict_opt(req: Option<&InferenceRequirements>) -> OffloadVerdict {
    offload_verdict_opt_with_local(req, None)
}

/// [`offload_verdict_opt`], told what the deciding node measured about itself.
/// The production ranked path calls this one; every other caller keeps the
/// two-argument form and so keeps today's behaviour exactly.
///
/// An ABSENT envelope is `Eligible` before the latency gate is ever reached,
/// so a slow node changes nothing for plain OpenAI clients — they could
/// already cross. The yield reaches only requests that explicitly asked for
/// `Fast`, which in this repo means the three `Workload` bundles that declare
/// that class in `sovereign-contracts::slot_policy` — `Route`, `Housekeep` and
/// `EnrichBulk`. (`Judge` and `Synthesize` are `Normal` and were never gated
/// here.) Of those three, only `Route` threads a posture today, so only
/// `Route` can reach this gate at all; the other two are still `LocalOnly` by
/// `Workload::request` and are refused one check earlier.
pub fn offload_verdict_opt_with_local(
    req: Option<&InferenceRequirements>,
    local: Option<&NodeObservations>,
) -> OffloadVerdict {
    match req {
        None => OffloadVerdict::Eligible,
        Some(req) => offload_verdict_with_local(req, local),
    }
}

/// The single decider behind [`offload_eligible`], [`offload_verdict_opt`],
/// and every gate name.
///
/// Order matters only for which reason is reported first; the gates are
/// independent and any one of them closes the request. Privacy is checked
/// first because it is the contract a reader is most likely to be auditing.
pub fn offload_verdict(req: &InferenceRequirements) -> OffloadVerdict {
    offload_verdict_with_local(req, None)
}

/// The single decider, told what the deciding node has measured about
/// **itself**. [`offload_verdict`] is this with nothing measured.
///
/// Order matters only for which reason is reported first; the gates are
/// independent and any one of them closes the request. Privacy is checked
/// first because it is the contract a reader is most likely to be auditing,
/// and because no measurement may reopen it — `local` is read only by the
/// latency gate, never by the privacy one.
///
/// ## Why the `Fast` gate has a measured escape and the others do not
///
/// SLOT_POLICY §5 keeps `Fast` work home on the ground that "the peer
/// round-trip dominates the inference, so a hop is a net loss". That is not a
/// policy, it is a **prediction about hardware**, and it is false on a machine
/// slow enough. Measured on `ring-doc-a`, a CPU-only podman node running
/// Qwen3.5-2B.Q6_K (`target/ring-room-demo/a/daemon.err`, bring-up 04:23Z
/// 2026-09-19): one `Workload::Route` classify — 1,288 prompt tokens for a
/// one-letter answer — took **38.4 s** gated `not_offload_eligible`, while the
/// knowledge fan-out to a peer on the same host completed in **37 ms**. The
/// rule cost three orders of magnitude more than the hop it was avoiding.
///
/// So the gate now asks its own premise instead of assuming it. A node that
/// has measured itself below the interactive reference has falsified "a hop is
/// a net loss" for its own work, and the gate stands down — reported as
/// [`OffloadVerdict::FastLatencyYielded`], never as a bare `Eligible`.
///
/// **Standing down is not offloading.** The verdict only decides whether the
/// scorer is allowed to *look* at peers; `scheduler_core::rank` still ranks
/// local against every peer and `pick_better` still keeps the work home when
/// no peer wins. A slow node with no peer, or with only slower peers, behaves
/// exactly as before — which is what bounds this change's blast radius.
///
/// The privacy and forward-budget gates take no measured escape and must not
/// grow one: neither is a claim about speed.
pub fn offload_verdict_with_local(
    req: &InferenceRequirements,
    local: Option<&NodeObservations>,
) -> OffloadVerdict {
    if req.sharding() != ShardingPrivacy::MeshAllowed {
        return OffloadVerdict::LocalOnlyPrivacy;
    }
    if req.effective_latency_class() == LatencyClass::Fast {
        // `None` (never measured) and `Some(false)` (measured fast enough)
        // both keep the standing rule. Only a node that measured itself slow
        // stands it down.
        return match local.and_then(local_is_sub_interactive) {
            Some(true) => OffloadVerdict::FastLatencyYielded,
            _ => OffloadVerdict::FastLatency,
        };
    }
    if !req.may_forward() {
        return OffloadVerdict::ForwardBudgetExhausted;
    }
    OffloadVerdict::Eligible
}

/// RTT threshold below which a peer is classified as
/// `NodeLocality::Local` — sub-5ms is same-host (loopback, Unix
/// socket equivalents). Rare in the mesh (peers are normally
/// separate machines) but handled for completeness.
pub(crate) const LOCAL_RTT_MS_THRESHOLD: u32 = 5;

/// RTT threshold below which a peer is classified as
/// `NodeLocality::Near` — typical for same-LAN (ethernet/WiFi with
/// a shared subnet) and direct Tailscale/WireGuard WAN links
/// between nearby endpoints. 25ms comfortably covers reasonable
/// LAN deployments without grabbing every lucky cross-internet
/// peer.
pub(crate) const NEAR_RTT_MS_THRESHOLD: u32 = 25;

/// Classify a measured round-trip time into a
/// [`NodeLocality`] bucket. Pure function; the async HTTP probe
/// that produces the `rtt_ms` value lives in the mesh host's
/// `InferenceRouter::get_peer_manifest`.
pub fn classify_rtt_ms(rtt_ms: u32) -> NodeLocality {
    if rtt_ms < LOCAL_RTT_MS_THRESHOLD {
        NodeLocality::Local
    } else if rtt_ms < NEAR_RTT_MS_THRESHOLD {
        NodeLocality::Near
    } else {
        NodeLocality::Far
    }
}

/// Fold v0.3 §7 operational adjustments (observation, load,
/// locality, cold-start, throughput, availability) into a
/// claim-scored candidate via the oicp-types SSOT scorer. The
/// returned candidate has `score` rescaled; all other fields are
/// preserved so downstream tie-breaks (`size_gb`, `model_id`) still
/// work. The full [`ScoreBreakdown`] rides along for the caller's
/// glassbox event — emit it, don't drop it.
///
/// `baseline_benchmark` is the peer's gossiped [`BenchmarkResult`]
/// (or `None` for older peers). `availability` is the peer's
/// gossiped `inference_availability` — pass `None` when scoring the
/// local node (its business is already captured by
/// `obs.in_flight`), `Some(...)` for peers. Adopting the gossiped
/// signal on the Joiner side is the one disclosed behavior change
/// of the 2026-06-10 rationalization: a peer advertising 0.2
/// availability used to be scored as if idle.
pub fn adjust_for_observations(
    cand: ModelCandidate,
    obs: &NodeObservations,
    locality: NodeLocality,
    baseline_benchmark: Option<&BenchmarkResult>,
    availability: Option<f32>,
) -> (ModelCandidate, ScoreBreakdown) {
    let breakdown = score_with_adjustments(
        cand.score,
        cand.claim_affinity,
        obs,
        locality,
        cand.size_gb.unwrap_or(0.0),
        baseline_benchmark,
        availability,
    );
    (
        ModelCandidate {
            score: breakdown.final_score,
            ..cand
        },
        breakdown,
    )
}

#[cfg(test)]
#[path = "oicp_select/tests.rs"]
mod tests;
