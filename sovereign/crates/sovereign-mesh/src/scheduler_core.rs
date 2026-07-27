// SPDX-License-Identifier: AGPL-3.0-or-later
//! The routing decision, extracted as a pure function.
//!
//! `SCHEDULER_QUALITY.md` §5 rests on one observation: **the
//! scheduling decision is a pure function, and the expensive part —
//! generating tokens — is exactly the part that does not affect it.**
//! This module is that observation made structural. Everything here
//! is total, synchronous and free of I/O: given a snapshot of what a
//! decider believed about itself and its peers, [`rank`] returns the
//! ranked peer list and the [`RoutingDecision`] record that explains
//! it.
//!
//! Two callers, one body:
//!
//!   - **Production** — `MeshInferenceProvider::select_peers_ranked`
//!     gathers the snapshot (manifest fetches, quarantine state, the
//!     gossiped endpoint fields) and calls [`rank`].
//!   - **Tier 1** — `crate::mesh_sim` builds the same snapshot from a
//!     simulated fleet and calls [`rank`]. Arm 0 of the sim is
//!     therefore *the production decision*, not a transcription of
//!     it, which is what makes "F1/F3/F5 reproduce" a falsifiable
//!     claim rather than a restatement of the model that predicted
//!     them.
//!
//! The split point is deliberate: everything above the line is
//! *gathering* (async, fallible, environment-shaped) and everything
//! below it is *deciding* (pure, total, seeded). A signal that is
//! stale, absent or mis-attributed enters here as a value — the core
//! never reaches out to refresh it, which is precisely why staleness
//! (F1) is observable in simulation at all.

use sovereign_core::oicp::{
    BenchmarkResult, InferenceRequirements, NodeLocality, NodeObservations, ProviderManifest,
};

use crate::decision_log::{
    self, CandidateInputs, CandidateKind, CandidateRecord, DecisionBuilder, ExclusionReason,
    LoadSource, RoutingDecision, ScoreRecord, Verdict,
};
use crate::oicp_select::{
    adjust_for_observations, candidates_equal, classify_rtt_ms, pick_better,
    score_manifest_for_request, ModelCandidate,
};
use crate::predicted_time::{
    self, LoadDebt, LocalOption, PredictInputs, Prediction, RequestShape, Unpredictable,
};

/// The name the local node is recorded under in a decision record's
/// candidate set. Peers are recorded under their mesh name, which is
/// never this.
pub(crate) const LOCAL_CANDIDATE_NAME: &str = "local";

/// Model id of the stand-in used when no loaded local model can serve
/// the request at all. Named rather than anonymous so a record or a
/// log line reading `<local-insufficient>` explains itself.
pub(crate) const LOCAL_INSUFFICIENT_MODEL_ID: &str = "<local-insufficient>";

/// Which objective maps the feasible set to a ranked list.
///
/// `SCHEDULER_QUALITY.md` §4.1 replaces the *ranking* half of the
/// decision and nothing else, so this is where the two candidates meet.
/// The **feasibility** half is identical either way — same hard gates,
/// same quarantine check, same claim matching, same candidate records —
/// and passing the objective in rather than branching at the call site
/// is what keeps that true. One scoring body, one record shape, one
/// [`DecisionBuilder::finish_at`], so a decision record describes what
/// the decider actually did under either objective instead of
/// describing the product's opinion of a choice the product did not
/// make.
///
/// What a record still cannot say is *which* objective produced its
/// verdict. [`crate::decision_replay`] therefore assumes
/// [`Product`](RankObjective::Product); see
/// [`crate::predicted_time`] for why closing that gap belongs to the
/// §4.1 landing rather than to the arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RankObjective {
    /// Production today, and arm 0 of the Tier-1 sim: the product of
    /// dimensionless multipliers, filtered by strictly-beats-local.
    #[default]
    Product,
    /// §4.1: predicted time-to-answer, filtered by
    /// strictly-faster-than-local. Introduces no constant — see
    /// [`crate::predicted_time`].
    PredictedTime,
}

// ── The ranking half of the decision ────────────────────────────
// Split out from [`rank`] so a *replayed* decision can re-run exactly
// this policy over the scores a record carries
// (`crate::decision_replay`). Same reason `rank` itself exists: two
// callers, one body. A second copy of the strictly-beats-local filter
// would make policy-agreement a measurement of the copy.

/// The comparison stand-in for "local cannot serve this request".
/// Negative infinity, so any peer that scores at all strictly beats
/// it.
pub(crate) fn local_sentinel() -> ModelCandidate {
    ModelCandidate {
        score: f32::NEG_INFINITY,
        size_gb: None,
        model_id: LOCAL_INSUFFICIENT_MODEL_ID.into(),
        claim_affinity: 0.0,
    }
}

/// Whether `cand` **strictly** beats `local` under the selection
/// policy — the predicate that decides whether a network hop is taken
/// at all.
///
/// "Strictly" carries two separate requirements, and both matter:
/// `pick_better` must return `cand` (local wins ties — no round trip,
/// no attribution churn), *and* the two must not be the same pick
/// (`candidates_equal`), so a peer advertising the identical model at
/// the identical score never trips a hop for zero delta.
pub(crate) fn beats_local(local: &ModelCandidate, cand: &ModelCandidate) -> bool {
    let winner = pick_better(local.clone(), cand.clone());
    winner.model_id == cand.model_id
        && winner.score == cand.score
        && winner.size_gb == cand.size_gb
        && !candidates_equal(local, cand)
}

/// Keep the candidates that strictly beat `local`, ordered best-first
/// per [`pick_better`] (score desc, then size asc), so the cascade
/// tries the strongest peer first.
///
/// Generic over the caller's tag so neither caller has to name the
/// other's peer representation: production carries an index into its
/// view slice, replay carries the peer's recorded name.
///
/// **Order-sensitive.** `pick_better` keeps the incumbent on a full
/// tie, so two indistinguishable candidates rank in the order they
/// were passed. Production passes them in scoring order and the
/// decision record stores them in that same order — which is what
/// makes replay reproduce a tie the same way round.
pub(crate) fn winners_over_local<T>(
    local: &ModelCandidate,
    scored: Vec<(T, ModelCandidate)>,
) -> Vec<(T, ModelCandidate)> {
    let mut winners: Vec<(T, ModelCandidate)> = scored
        .into_iter()
        .filter(|(_, cand)| beats_local(local, cand))
        .collect();
    winners.sort_by(|(_, a), (_, b)| {
        let w = pick_better(a.clone(), b.clone());
        if w.model_id == a.model_id && w.score == a.score && w.size_gb == a.size_gb {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    winners
}

// ── Feedback ────────────────────────────────────────────────────
// The other half of the loop: how a decision's *outcome* changes the
// inputs of the next decision. Pure, so the simulator's beliefs age
// by the same rules production's do — the alternative is two
// implementations of one EMA, which drift silently and take the
// calibration contract with them.

/// A request was just dispatched to this node: one more in flight,
/// one more sample toward the cold-start ramp.
pub(crate) fn observe_dispatch(obs: &mut NodeObservations) {
    obs.in_flight = obs.in_flight.saturating_add(1);
    obs.samples = obs.samples.saturating_add(1);
}

/// A dispatched request completed. Drifts the failure rate toward
/// zero on every success.
pub(crate) fn observe_success(obs: &mut NodeObservations) {
    obs.in_flight = obs.in_flight.saturating_sub(1);
    obs.recent_failure_rate = (obs.recent_failure_rate * 0.9).max(0.0);
}

/// A dispatched request failed. Rolling-window failure rate: EMA
/// toward 1.0 with alpha 0.1 — ten consecutive failures settle near
/// 0.65.
pub(crate) fn observe_failure(obs: &mut NodeObservations) {
    obs.in_flight = obs.in_flight.saturating_sub(1);
    obs.recent_failure_rate = (obs.recent_failure_rate * 0.9 + 0.1).min(1.0);
}

/// What the decider knows about *itself* at decision time.
///
/// The local side has no staleness by construction — the asymmetry
/// between this struct and [`PeerCandidateView`] (which carries three
/// separate age fields) *is* finding F1, stated in the type system.
pub(crate) struct LocalCandidateView<'a> {
    pub manifest: &'a ProviderManifest,
    pub observations: &'a NodeObservations,
    pub benchmark: Option<&'a BenchmarkResult>,
}

/// A peer manifest as the decider currently holds it, with the two
/// facts that make it more than a manifest: how long the round trip
/// took (which sets the locality bonus) and how old the copy is.
pub(crate) struct PeerManifestView {
    pub manifest: ProviderManifest,
    pub rtt_ms: u32,
    /// Seconds since the manifest was fetched. `0` for a live fetch.
    pub age_secs: u64,
    pub from_cache: bool,
}

/// Everything the decider believes about one peer, at one instant.
///
/// Assembled by the caller from whatever sources it has (gossip,
/// local observation, a manifest cache); consumed here without
/// further lookup. `manifest == None` means the manifest could not be
/// obtained — the peer is recorded as excluded rather than silently
/// dropped, because "the hub lost" and "the hub was never considered"
/// are different failures.
pub(crate) struct PeerCandidateView {
    pub name: String,
    pub node_id_hex: String,
    /// From the decider's own `PeerHealthTracker`. Quarantined peers
    /// are excluded before the manifest is even consulted.
    pub quarantined: bool,
    /// A pinned worker pod (`PeerInferenceEndpoint::transport`) has
    /// no users of its own, so its claim affinity is normalised.
    pub pinned_transport: bool,
    /// The peer's self-reported in-flight count as gossiped. `None`
    /// when the peer has never gossiped one.
    pub gossiped_in_flight: Option<u32>,
    /// The peer's gossiped `inference_availability`.
    pub availability: Option<f32>,
    /// Unix seconds the gossip record carrying the above was last
    /// seen. `0` = never; the pair (value, age) is what distinguishes
    /// "the peer is idle" from "the peer was idle a round ago".
    pub gossip_last_seen_unix: u64,
    pub benchmark: Option<BenchmarkResult>,
    /// The decider's *own* observations of this peer — dispatch
    /// counts, failure rate, latency EWMAs.
    pub observations: NodeObservations,
    pub manifest: Option<PeerManifestView>,
}

/// The complete snapshot a single decision is taken against.
pub(crate) struct RankInputs<'a> {
    /// Decision-time clock, in unix seconds. Passed rather than read
    /// so a simulated decider can run on virtual time.
    pub now_unix: u64,
    pub oicp_request_id: &'a str,
    pub req: &'a InferenceRequirements,
    /// SLOT_POLICY §6 — the request elicits a calibrated one-pass
    /// distribution, so only peers advertising `x:forced_choice` may
    /// serve it.
    pub needs_forced_choice: bool,
    /// Which objective ranks the feasible set. Production passes
    /// [`RankObjective::Product`]; the Tier-1 sim passes whatever its
    /// arm calls for.
    pub objective: RankObjective,
    pub local: LocalCandidateView<'a>,
    pub peers: &'a [PeerCandidateView],
}

/// The decision, plus the record that explains it.
///
/// `ranked` holds indices into [`RankInputs::peers`], best-first, so
/// the caller can re-pair them with whatever peer representation it
/// owns without this module having to name it.
pub(crate) struct RankResult {
    pub ranked: Vec<(usize, ModelCandidate)>,
    pub decision: RoutingDecision,
}

/// Score every candidate, keep the peers that strictly beat local,
/// rank them best-first, and record the whole thing.
///
/// Pure: no I/O, no clock read, no interior mutability. Given the
/// same `rec` seed and the same inputs it returns the same decision,
/// which is the property Tier-1 replay depends on.
pub(crate) fn rank(mut rec: DecisionBuilder, inputs: RankInputs<'_>) -> RankResult {
    let RankInputs {
        now_unix,
        oicp_request_id,
        req,
        needs_forced_choice,
        objective,
        local,
        peers,
    } = inputs;

    // Local is always a candidate. `None` means no loaded model's
    // claims can serve the request — any peer that CAN then wins
    // automatically.
    let local_cand = score_manifest_for_request(local.manifest, req).map(|c| {
        // Local availability is `None` (neutral): the local node's
        // business is already captured by `observations.in_flight`;
        // the gossiped availability signal exists to protect busy
        // PEERS.
        let (cand, breakdown) = adjust_for_observations(
            c,
            local.observations,
            NodeLocality::Local,
            local.benchmark,
            None,
        );
        // P2: the local candidate's load signal has no staleness —
        // that asymmetry between self and peers IS finding F1, so
        // the record states it explicitly (`LoadSource::Local`,
        // `gossip_age_secs: None`) rather than leaving it implied.
        let mut candidate_inputs = CandidateInputs::from_observations(local.observations, LoadSource::Local)
            .with_benchmark(local.benchmark, now_unix);
        // §4.1 reads model-load debt, so the record has to carry it —
        // an objective may not consume a signal a capture cannot replay.
        if let Some(debt) = LoadDebt::from_manifest(local.manifest, &cand.model_id) {
            candidate_inputs.model_loaded = Some(debt.model_loaded);
            candidate_inputs.estimated_load_ms = Some(debt.estimated_load_ms);
        }
        rec.push_candidate(CandidateRecord {
            kind: CandidateKind::Local,
            name: LOCAL_CANDIDATE_NAME.to_string(),
            node_id: None,
            model_id: cand.model_id.clone(),
            size_gb: cand.size_gb,
            locality: decision_log::locality_label(NodeLocality::Local).to_string(),
            rank: None,
            selected: false,
            score: ScoreRecord::from(&breakdown),
            inputs: candidate_inputs,
        });
        cand
    });
    tracing::info!(
        local_models = local.manifest.models.len(),
        local_scores = local_cand.is_some(),
        local_score = local_cand.as_ref().map(|c| c.score).unwrap_or(f32::NEG_INFINITY),
        local_pick = local_cand.as_ref().map(|c| c.model_id.as_str()).unwrap_or("<none>"),
        local_size_gb = ?local_cand.as_ref().and_then(|c| c.size_gb),
        req_hint = %req.effective_hint(),
        req_latency = ?req.effective_latency_class(),
        "mesh-inference: scoring local"
    );

    // §4.1 inputs, gathered alongside the product's. Both objectives
    // read the *same* beliefs — the gossiped in-flight count the
    // scorer used, the manifest probe's RTT, the advertised benchmark
    // — so a difference between the two arms is a difference of
    // objective and never of information.
    let shape = RequestShape::from_requirements(req);
    let local_timing = PredictInputs {
        in_flight: local.observations.in_flight,
        // No wire cost to oneself; the asymmetry that is F1 in the
        // load signal is *also* an asymmetry in the wire, and this is
        // the honest half of it.
        rtt_ms: 0,
        pp_tok_s: local.benchmark.map(|b| b.pp_tok_s),
        tg_tok_s: local.benchmark.map(|b| b.tg_tok_s),
        // Local pays a load too — a node whose own model is evicted is
        // not free just because it is local, and pretending otherwise
        // would bias every comparison toward staying home.
        pending_load_ms: local_cand
            .as_ref()
            .and_then(|c| LoadDebt::from_manifest(local.manifest, &c.model_id))
            .map(|d| d.pending_ms())
            .unwrap_or(0),
    };

    let mut scored: Vec<(usize, ModelCandidate)> = Vec::new();
    // Index-parallel to `scored`: pushed in the same place, so the two
    // cannot drift apart without the push sites disagreeing visibly.
    let mut timing: Vec<PredictInputs> = Vec::new();
    for (idx, peer) in peers.iter().enumerate() {
        // Drop quarantined peers from the candidate set. They
        // re-enter automatically once their cooldown expires.
        if peer.quarantined {
            tracing::debug!(
                peer = %peer.name,
                "mesh-inference: skipping quarantined peer in scoring"
            );
            rec.exclude(
                &peer.name,
                Some(peer.node_id_hex.clone()),
                ExclusionReason::Quarantined,
            );
            continue;
        }
        let Some(manifest_view) = peer.manifest.as_ref() else {
            rec.exclude(
                &peer.name,
                Some(peer.node_id_hex.clone()),
                ExclusionReason::ManifestUnavailable,
            );
            continue;
        };
        let PeerManifestView {
            manifest,
            rtt_ms,
            age_secs: manifest_age_secs,
            from_cache: manifest_from_cache,
        } = manifest_view;
        if needs_forced_choice
            && !manifest
                .features
                .iter()
                .any(|f| f.as_str() == sovereign_core::oicp::features::X_FORCED_CHOICE)
        {
            tracing::debug!(
                oicp_request_id = %oicp_request_id,
                peer = %peer.name,
                "mesh-inference: excluding peer — forced_choice sentinel \
                 but manifest does not advertise x:forced_choice"
            );
            rec.exclude(
                &peer.name,
                Some(peer.node_id_hex.clone()),
                ExclusionReason::NoForcedChoice,
            );
            continue;
        }
        let mut raw = match score_manifest_for_request(manifest, req) {
            Some(c) => c,
            None => {
                rec.exclude(
                    &peer.name,
                    Some(peer.node_id_hex.clone()),
                    ExclusionReason::NoClaimMatch,
                );
                continue;
            }
        };
        // Pinned worker pods have no "users" beyond the owner — the
        // mesh's local-affinity bias (which scales peer scores by
        // their willingness to serve outside requests) doesn't apply.
        // Normalising claim_affinity to 1.0 makes the
        // `effective_affinity / claim_affinity` ratio inside
        // `adjust_for_observations` collapse to a neutral multiplier
        // so a pinned pod isn't penalised for failing to advertise
        // mesh-affinity it has no concept of.
        // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md hard part 3.
        if peer.pinned_transport {
            raw.claim_affinity = 1.0;
        }
        // Apply operational adjustments. Locality is derived from the
        // manifest-fetch RTT (see PR-F) — same round trip, no extra
        // probe — so LAN deployments actually see their locality
        // bonus instead of every peer defaulting to `Far`.
        let mut obs = peer.observations.clone();
        // Cluster-wide load-awareness override. The decider's local
        // observation of a peer only counts requests *it* dispatched
        // — it is structurally blind to traffic the peer served from
        // its own local user. When the peer gossips its self-reported
        // count, prefer it: it captures total load. Without this
        // override a busy peer with no locally-originated traffic
        // looks phantom-idle and wins routing it can't serve in time.
        //
        // Sample-floor heuristic: keep the local sample count (used
        // elsewhere in scoring). Only the in-flight number is swapped
        // — gossiped samples are not yet plumbed and would muddle the
        // cold-start ramp.
        //
        // See `sovereign/docs/MESH_LOAD_AWARENESS.md`.
        let self_observed_in_flight = obs.in_flight;
        let mut in_flight_source = LoadSource::SelfObserved;
        if let Some(gossiped) = peer.gossiped_in_flight {
            tracing::debug!(
                peer = %peer.name,
                self_observed = obs.in_flight,
                gossiped,
                "mesh-inference: applying gossiped in-flight override"
            );
            obs.in_flight = gossiped;
            in_flight_source = LoadSource::Gossip;
        }
        let (cand, breakdown) = adjust_for_observations(
            raw,
            &obs,
            classify_rtt_ms(*rtt_ms),
            peer.benchmark.as_ref(),
            // Gossiped availability — ADOPTED 2026-06-10 (the signal
            // was previously dropped on the floor; a peer advertising
            // 0.2 was scored as if idle). `None` for peers that
            // haven't gossiped one keeps them neutral.
            peer.availability,
        );
        tracing::info!(
            peer = %peer.name,
            peer_pick = %cand.model_id,
            peer_score = cand.score,
            peer_size_gb = ?cand.size_gb,
            "mesh-inference: scored peer"
        );
        // P2: every signal that fed this score, stamped with how old
        // it was when the scorer read it. The gossip age is the
        // measurement F1 has been missing — the load number and the
        // load number's age are different facts, and only the pair
        // can distinguish "the peer is idle" from "the peer was idle
        // a full anti-entropy round ago."
        let mut candidate_inputs = CandidateInputs::from_observations(&obs, in_flight_source)
            .with_benchmark(peer.benchmark.as_ref(), now_unix);
        candidate_inputs.self_observed_in_flight = Some(self_observed_in_flight);
        candidate_inputs.gossiped_in_flight = peer.gossiped_in_flight;
        candidate_inputs.availability = peer.availability;
        candidate_inputs.gossip_age_secs = (peer.gossip_last_seen_unix > 0)
            .then(|| now_unix.saturating_sub(peer.gossip_last_seen_unix));
        candidate_inputs.manifest_age_secs = Some(*manifest_age_secs);
        candidate_inputs.manifest_from_cache = Some(*manifest_from_cache);
        candidate_inputs.rtt_ms = Some(*rtt_ms);
        let load_debt = LoadDebt::from_manifest(manifest, &cand.model_id);
        if let Some(debt) = load_debt {
            candidate_inputs.model_loaded = Some(debt.model_loaded);
            candidate_inputs.estimated_load_ms = Some(debt.estimated_load_ms);
        }
        rec.push_candidate(CandidateRecord {
            kind: CandidateKind::Peer,
            name: peer.name.clone(),
            node_id: Some(peer.node_id_hex.clone()),
            model_id: cand.model_id.clone(),
            size_gb: cand.size_gb,
            locality: decision_log::locality_label(classify_rtt_ms(*rtt_ms)).to_string(),
            rank: None,
            selected: false,
            score: ScoreRecord::from(&breakdown),
            inputs: candidate_inputs,
        });
        scored.push((idx, cand));
        timing.push(PredictInputs {
            // `obs.in_flight` post-override: the number the scorer
            // actually read, gossip staleness included.
            in_flight: obs.in_flight,
            rtt_ms: *rtt_ms,
            pp_tok_s: peer.benchmark.as_ref().map(|b| b.pp_tok_s),
            tg_tok_s: peer.benchmark.as_ref().map(|b| b.tg_tok_s),
            pending_load_ms: load_debt.map(|d| d.pending_ms()).unwrap_or(0),
        });
    }

    // Keep only peers that beat local — ranked best-first. The cascade
    // tries them in order; local is the final fallback step. `scored`
    // is in scoring order, which is also the order the candidate
    // records were pushed; both filters below are order-sensitive on
    // exact ties and document why.
    let winners = match objective {
        // Same tie-break as everywhere else: local wins ties (no
        // round-trip cost, no attribution churn).
        RankObjective::Product => {
            let local_for_cmp = local_cand.clone().unwrap_or_else(local_sentinel);
            winners_over_local(&local_for_cmp, scored)
        }
        // §4.1. The `ModelCandidate` rides along as part of the tag so
        // the winning peers come back paired with the pick the scorer
        // made for them — the *feasibility* answer is still the
        // product path's, and only the ordering is new.
        RankObjective::PredictedTime => {
            let local_option = match local_cand.as_ref() {
                Some(_) => LocalOption::from(predicted_time::predict(&local_timing, shape)),
                // Local cannot serve this at all — the time-objective
                // twin of `local_sentinel`.
                None => LocalOption::Infeasible,
            };
            let predicted: Vec<((usize, ModelCandidate), Result<Prediction, Unpredictable>)> =
                scored
                    .into_iter()
                    .zip(timing)
                    .map(|(tagged, t)| {
                        let p = predicted_time::predict(&t, shape);
                        (tagged, p)
                    })
                    .collect();
            let ranked = predicted_time::faster_than_local(local_option, predicted);
            tracing::info!(
                oicp_request_id = %oicp_request_id,
                local_predicted_ms = ?match local_option {
                    LocalOption::Predicted(p) => Some(p.total_ms),
                    _ => None,
                },
                local_unpredictable = ?match local_option {
                    LocalOption::Unpredictable(u) => Some(u.label()),
                    _ => None,
                },
                best_peer_predicted_ms = ?ranked.first().map(|(_, p)| p.total_ms),
                faster_than_local = ranked.len(),
                "mesh-inference: ranked by predicted time-to-answer (§4.1)"
            );
            ranked.into_iter().map(|(tagged, _)| tagged).collect()
        }
    };
    match winners.first() {
        Some((idx, cand)) => tracing::info!(
            peer = %peers[*idx].name,
            peer_pick = %cand.model_id,
            ranked = winners.len(),
            "mesh-inference: peer(s) selected by OICP (ranked, best-first)"
        ),
        None => tracing::debug!("mesh-inference: no peer strictly beats local, staying local"),
    }

    let ranked_names: Vec<String> = winners
        .iter()
        .map(|(idx, _)| peers[*idx].name.clone())
        .collect();
    let verdict = if ranked_names.is_empty() {
        Verdict::StayLocal
    } else {
        Verdict::Peers {
            ranked: ranked_names.clone(),
        }
    };
    RankResult {
        ranked: winners,
        decision: rec.finish_at(verdict, &ranked_names, now_unix.saturating_mul(1000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_log::{DecisionPath, RequestFacts};
    use sovereign_core::oicp::{
        CapabilityClaim, CapabilityHint, LatencyClass, ModelStatus, ProviderModel, ShardingPrivacy,
        OICP_VERSION,
    };

    fn manifest(model_id: &str, size_gb: f32, affinity: f32) -> ProviderManifest {
        ProviderManifest {
            oicp_version: OICP_VERSION.into(),
            provider: None,
            models: vec![ProviderModel {
                id: model_id.into(),
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
                    CapabilityHint::general(),
                    LatencyClass::Extended,
                    32_768,
                    4_000,
                    affinity,
                )],
                fingerprint: None,
            }],
            knowledge: None,
            federation: None,
            features: vec![],
        }
    }

    fn requirements() -> InferenceRequirements {
        InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Extended)
            .with_sharding(ShardingPrivacy::MeshAllowed)
    }

    fn facts() -> RequestFacts {
        RequestFacts {
            capability_hint: "general".into(),
            latency_class: "standard".into(),
            sharding: "mesh_allowed".into(),
            context_tokens: None,
            max_output_tokens: None,
            preferred_speed: "slow".into(),
            explicit_model_id: None,
        }
    }

    fn builder() -> DecisionBuilder {
        DecisionBuilder::new("req-1", DecisionPath::RankedOicp, facts())
    }

    fn peer(name: &str, affinity: f32, in_flight: Option<u32>) -> PeerCandidateView {
        PeerCandidateView {
            name: name.into(),
            node_id_hex: format!("{name}-hex"),
            quarantined: false,
            pinned_transport: false,
            gossiped_in_flight: in_flight,
            availability: None,
            gossip_last_seen_unix: 900,
            benchmark: None,
            observations: NodeObservations {
                samples: 100,
                ..Default::default()
            },
            manifest: Some(PeerManifestView {
                manifest: manifest("peer-model", 20.0, affinity),
                rtt_ms: 10,
                age_secs: 0,
                from_cache: false,
            }),
        }
    }

    /// The local view used by most tests: a weak local model, so any
    /// competent peer strictly beats it.
    fn weak_local() -> (ProviderManifest, NodeObservations) {
        (
            manifest("local-model", 4.0, 0.30),
            NodeObservations {
                samples: 100,
                ..Default::default()
            },
        )
    }

    fn run(peers: &[PeerCandidateView]) -> RankResult {
        let (m, obs) = weak_local();
        let req = requirements();
        rank(
            builder(),
            RankInputs {
                now_unix: 1000,
                oicp_request_id: "req-1",
                req: &req,
                needs_forced_choice: false,
                objective: RankObjective::Product,
                local: LocalCandidateView {
                    manifest: &m,
                    observations: &obs,
                    benchmark: None,
                },
                peers,
            },
        )
    }

    #[test]
    fn a_strong_peer_beats_a_weak_local_and_is_ranked_first() {
        let peers = vec![peer("hub", 0.95, Some(0))];
        let out = run(&peers);
        assert_eq!(out.ranked.len(), 1);
        assert_eq!(out.ranked[0].0, 0);
        assert!(matches!(out.decision.verdict, Verdict::Peers { .. }));
        // The local candidate is recorded even though it lost.
        assert!(out
            .decision
            .candidates
            .iter()
            .any(|c| c.name == LOCAL_CANDIDATE_NAME));
        let hub = out
            .decision
            .candidates
            .iter()
            .find(|c| c.name == "hub")
            .expect("hub recorded");
        assert_eq!(hub.rank, Some(0));
        assert!(hub.selected);
    }

    #[test]
    fn a_quarantined_peer_is_excluded_before_its_manifest_is_read() {
        let mut peers = vec![peer("hub", 0.95, Some(0))];
        peers[0].quarantined = true;
        let out = run(&peers);
        assert!(out.ranked.is_empty());
        assert!(matches!(out.decision.verdict, Verdict::StayLocal));
        assert_eq!(out.decision.excluded.len(), 1);
        assert!(matches!(
            out.decision.excluded[0].reason,
            ExclusionReason::Quarantined
        ));
        // Excluded before scoring: no candidate record for it.
        assert!(!out.decision.candidates.iter().any(|c| c.name == "hub"));
    }

    #[test]
    fn an_unavailable_manifest_is_recorded_as_an_exclusion_not_a_silent_drop() {
        let mut peers = vec![peer("hub", 0.95, Some(0))];
        peers[0].manifest = None;
        let out = run(&peers);
        assert!(out.ranked.is_empty());
        assert!(matches!(
            out.decision.excluded[0].reason,
            ExclusionReason::ManifestUnavailable
        ));
    }

    #[test]
    fn the_gossiped_in_flight_count_overrides_the_self_observed_one_and_says_so() {
        let mut peers = vec![peer("hub", 0.95, Some(7))];
        peers[0].observations.in_flight = 1;
        let out = run(&peers);
        let hub = out
            .decision
            .candidates
            .iter()
            .find(|c| c.name == "hub")
            .expect("hub recorded");
        assert_eq!(hub.inputs.in_flight, 7);
        assert_eq!(hub.inputs.self_observed_in_flight, Some(1));
        assert_eq!(hub.inputs.in_flight_source, LoadSource::Gossip);
        // now_unix 1000 - last_seen 900: the age F1 is about.
        assert_eq!(hub.inputs.gossip_age_secs, Some(100));
    }

    #[test]
    fn peers_are_ranked_best_first_and_a_loaded_peer_falls_behind_an_idle_one() {
        let peers = vec![peer("busy", 0.95, Some(12)), peer("idle", 0.95, Some(0))];
        let out = run(&peers);
        assert_eq!(out.ranked.len(), 2);
        assert_eq!(peers[out.ranked[0].0].name, "idle");
        assert_eq!(peers[out.ranked[1].0].name, "busy");
        assert!(out.ranked[0].1.score > out.ranked[1].1.score);
    }

    #[test]
    fn an_equal_peer_does_not_beat_local_so_no_hop_is_taken() {
        let m = manifest("same-model", 20.0, 0.30);
        let obs = NodeObservations {
            samples: 100,
            ..Default::default()
        };
        let req = requirements();
        let peers = vec![PeerCandidateView {
            manifest: Some(PeerManifestView {
                manifest: manifest("same-model", 20.0, 0.30),
                // Same locality class as local so the bonus matches.
                rtt_ms: 0,
                age_secs: 0,
                from_cache: false,
            }),
            ..peer("twin", 0.30, Some(0))
        }];
        let out = rank(
            builder(),
            RankInputs {
                now_unix: 1000,
                oicp_request_id: "req-1",
                req: &req,
                needs_forced_choice: false,
                objective: RankObjective::Product,
                local: LocalCandidateView {
                    manifest: &m,
                    observations: &obs,
                    benchmark: None,
                },
                peers: &peers,
            },
        );
        assert!(out.ranked.is_empty(), "an identical peer must not win");
        assert!(matches!(out.decision.verdict, Verdict::StayLocal));
    }

    #[test]
    fn the_forced_choice_sentinel_excludes_peers_that_do_not_advertise_it() {
        let (m, obs) = weak_local();
        let req = requirements();
        let peers = vec![peer("hub", 0.95, Some(0))];
        let out = rank(
            builder(),
            RankInputs {
                now_unix: 1000,
                oicp_request_id: "req-1",
                req: &req,
                needs_forced_choice: true,
                objective: RankObjective::Product,
                local: LocalCandidateView {
                    manifest: &m,
                    observations: &obs,
                    benchmark: None,
                },
                peers: &peers,
            },
        );
        assert!(out.ranked.is_empty());
        assert!(matches!(
            out.decision.excluded[0].reason,
            ExclusionReason::NoForcedChoice
        ));
    }

    fn benchmark(model_id: &str, size_gb: f32, pp: f32, tg: f32) -> BenchmarkResult {
        BenchmarkResult {
            baseline_model_id: model_id.into(),
            baseline_size_gb: size_gb,
            pp_tok_s: pp,
            tg_tok_s: tg,
            measured_at: 0,
        }
    }

    /// A request with a size. `requirements()` leaves both token counts
    /// unset, which is feasible for the product (the gates simply do
    /// not bind) but leaves the predicted-time objective with no job to
    /// predict.
    fn sized_requirements() -> InferenceRequirements {
        requirements()
            .with_context_tokens(4_000)
            .with_max_output_tokens(500)
    }

    /// **§4.1 wiring, and the property the product cannot express.**
    ///
    /// One set of inputs, two objectives, opposite verdicts. The peer is
    /// the capability winner (0.95 against local's 0.30) and has twelve
    /// requests already queued; the local model is small, idle and fast.
    ///
    /// The product ranks the hub first anyway: a queue enters as a
    /// dimensionless multiplier and cannot outweigh a claim advantage.
    /// The predicted time declines it, because 12 queued jobs × 22s of
    /// service is a quantity in the same unit as local's ~7.5s — which
    /// is the entire §4.1 thesis in one comparison.
    ///
    /// If this test ever reports the same verdict twice, the objective
    /// is not wired and `PredictedTime` fell through to the product.
    #[test]
    fn the_predicted_time_objective_declines_a_hop_the_product_takes() {
        let peers = vec![PeerCandidateView {
            benchmark: Some(benchmark("peer-model", 21.0, 2_000.0, 25.0)),
            ..peer("hub", 0.95, Some(12))
        }];
        let (m, obs) = weak_local();
        let local_bench = benchmark("local-model", 4.0, 1_200.0, 120.0);
        let req = sized_requirements();
        let decide = |objective: RankObjective| {
            rank(
                builder(),
                RankInputs {
                    now_unix: 1000,
                    oicp_request_id: "req-1",
                    req: &req,
                    needs_forced_choice: false,
                    objective,
                    local: LocalCandidateView {
                        manifest: &m,
                        observations: &obs,
                        benchmark: Some(&local_bench),
                    },
                    peers: &peers,
                },
            )
        };

        let product = decide(RankObjective::Product);
        assert_eq!(
            product.ranked.len(),
            1,
            "the product ranks a backed-up capability winner first"
        );
        assert!(matches!(product.decision.verdict, Verdict::Peers { .. }));

        let predicted = decide(RankObjective::PredictedTime);
        assert!(
            predicted.ranked.is_empty(),
            "a hop into a 12-deep queue costs more than it buys and must be declined"
        );
        assert!(matches!(predicted.decision.verdict, Verdict::StayLocal));

        // The feasibility half is untouched: same candidates, same
        // recorded scores. Only the ranking differs, which is exactly
        // the scope §4.1 claims.
        let scores = |r: &RankResult| -> Vec<(String, f32)> {
            r.decision
                .candidates
                .iter()
                .map(|c| (c.name.clone(), c.score.final_score))
                .collect()
        };
        assert_eq!(scores(&product), scores(&predicted));
    }

    /// The other half of the wiring check, and the one that stops the
    /// arm from being trivially conservative: when the hop *does* pay,
    /// predicted time takes it. An objective that only ever stays local
    /// would pass the test above for the wrong reason.
    #[test]
    fn the_predicted_time_objective_still_offloads_when_the_hop_pays() {
        let peers = vec![PeerCandidateView {
            benchmark: Some(benchmark("peer-model", 21.0, 2_000.0, 25.0)),
            ..peer("hub", 0.95, Some(0))
        }];
        let (m, obs) = weak_local();
        // A local model so slow that even 22s on the hub beats it.
        let local_bench = benchmark("local-model", 4.0, 100.0, 5.0);
        let req = sized_requirements();
        let out = rank(
            builder(),
            RankInputs {
                now_unix: 1000,
                oicp_request_id: "req-1",
                req: &req,
                needs_forced_choice: false,
                objective: RankObjective::PredictedTime,
                local: LocalCandidateView {
                    manifest: &m,
                    observations: &obs,
                    benchmark: Some(&local_bench),
                },
                peers: &peers,
            },
        );
        assert_eq!(out.ranked.len(), 1, "an idle hub beating a crawling local");
        assert!(matches!(out.decision.verdict, Verdict::Peers { .. }));
    }

    /// A request with no token counts has no size, so no candidate has
    /// a predicted time — including local. Rule 1 of
    /// `predicted_time::faster_than_local`: no comparison, no hop.
    #[test]
    fn a_request_with_no_token_shape_stays_local_under_predicted_time() {
        let peers = vec![PeerCandidateView {
            benchmark: Some(benchmark("peer-model", 21.0, 2_000.0, 25.0)),
            ..peer("hub", 0.95, Some(0))
        }];
        let (m, obs) = weak_local();
        let local_bench = benchmark("local-model", 4.0, 100.0, 5.0);
        // `requirements()`, not `sized_requirements()`.
        let req = requirements();
        let out = rank(
            builder(),
            RankInputs {
                now_unix: 1000,
                oicp_request_id: "req-1",
                req: &req,
                needs_forced_choice: false,
                objective: RankObjective::PredictedTime,
                local: LocalCandidateView {
                    manifest: &m,
                    observations: &obs,
                    benchmark: Some(&local_bench),
                },
                peers: &peers,
            },
        );
        assert!(out.ranked.is_empty());
        assert!(matches!(out.decision.verdict, Verdict::StayLocal));
    }

    /// The property Tier-1 replay depends on: same inputs, same
    /// decision. Not merely the same winner — the same record.
    #[test]
    fn the_core_is_deterministic_over_identical_inputs() {
        let peers = vec![peer("a", 0.95, Some(3)), peer("b", 0.80, Some(0))];
        let first = run(&peers);
        let second = run(&peers);
        assert_eq!(first.decision.verdict, second.decision.verdict);
        assert_eq!(first.decision.ts_unix_ms, second.decision.ts_unix_ms);
        let names = |r: &RankResult| -> Vec<String> {
            r.ranked.iter().map(|(i, _)| peers[*i].name.clone()).collect()
        };
        assert_eq!(names(&first), names(&second));
        assert_eq!(
            first
                .decision
                .candidates
                .iter()
                .map(|c| (c.name.clone(), c.score.final_score))
                .collect::<Vec<_>>(),
            second
                .decision
                .candidates
                .iter()
                .map(|c| (c.name.clone(), c.score.final_score))
                .collect::<Vec<_>>()
        );
    }
}
