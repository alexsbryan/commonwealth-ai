// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP v0.3 backend selection with observation-adjusted scoring.
//!
//! `pick_slot_for_oicp` chooses the best peer from a candidate list
//! by scoring each model's published claims via
//! [`oicp::score_claim_for_request`], then folding in (per v0.3 §7):
//!
//! - **Observed affinity** — claimed affinity blended with the
//!   scheduler's own observed failure rate
//!   ([`oicp::effective_affinity`]).
//! - **Locality** — same-machine / same-LAN / cross-internet
//!   multiplier ([`oicp::locality_bonus`]).
//! - **Load penalty** — hyperbolic taper on in-flight count
//!   ([`oicp::load_penalty`]) so popular specialists divert excess
//!   traffic to the second choice.
//! - **Cold-start ramp** — new nodes get reduced weight until they
//!   accumulate observation samples
//!   ([`oicp::cold_start_weight`]).
//! - **Inference availability** — gossiped ActivityReporter signal
//!   ([0.2, 1.0] clamp) captured before per-turn observations
//!   converge.

use commonwealth_core::capabilities::NodeCapabilities;

use crate::oicp::{
    self, cold_start_weight, effective_affinity, load_penalty, locality_bonus, throughput_factor,
    throughput_factor_source, InferenceRequirements, NodeLocality, NodeObservations,
    ProviderManifest,
};

/// A candidate backend entry for OICP-based selection.
pub struct BackendCandidate<'a> {
    /// The peer's advertised OICP provider manifest.
    pub manifest: &'a ProviderManifest,
    /// The peer's gossip capabilities, carrying inference_availability
    /// and the `inference_capable` hard gate. None for backends that
    /// don't participate in the activity-aware mesh — treated as
    /// always-capable, fully-idle.
    pub node_capabilities: Option<&'a NodeCapabilities>,
    /// Per-node observations accumulated by this scheduler. None →
    /// treated as zero-sample (claim-only, no load penalty).
    pub observations: Option<&'a NodeObservations>,
    /// Where this peer sits relative to the scheduler. Defaults to
    /// `Far` per [`NodeLocality::default`]; set to `Local` / `Near`
    /// when the scheduler knows the topology.
    pub locality: NodeLocality,
}

impl<'a> BackendCandidate<'a> {
    /// Minimal candidate: just the manifest, no gossip, no
    /// observations, default `Far` locality. Builder methods layer
    /// on additional state.
    pub fn new(manifest: &'a ProviderManifest) -> Self {
        Self {
            manifest,
            node_capabilities: None,
            observations: None,
            locality: NodeLocality::default(),
        }
    }

    pub fn with_node_capabilities(mut self, caps: &'a NodeCapabilities) -> Self {
        self.node_capabilities = Some(caps);
        self
    }

    pub fn with_observations(mut self, obs: &'a NodeObservations) -> Self {
        self.observations = Some(obs);
        self
    }

    pub fn with_locality(mut self, locality: NodeLocality) -> Self {
        self.locality = locality;
        self
    }
}

/// Score the best (model, claim) pair in a manifest against a
/// request via v0.3 claim-based ranking. Returns the raw claim
/// score (protocol-level), the claim's self-reported affinity, and
/// the size in GB of the winning model — the caller folds in
/// observation / load / locality / throughput adjustments.
///
/// `model_size_gb` is `None` when the manifest doesn't advertise it
/// (older peers, manifests assembled without size data); the
/// scheduler falls through to the no-benchmark branch of
/// [`oicp::throughput_factor`] in that case.
///
/// Returns `None` when no claim in the manifest can serve the
/// request.
fn best_score_for_manifest(
    manifest: &ProviderManifest,
    req: &InferenceRequirements,
) -> Option<(f32, f32, Option<f32>)> {
    let mut best: Option<(f32, f32, Option<f32>)> = None;
    for model in manifest.models.iter().filter(|m| m.status.available) {
        for claim in &model.claims {
            let Some(score) = oicp::score_claim_for_request(claim, req) else {
                continue;
            };
            let claim_affinity = claim.effective_affinity();
            let candidate = (score, claim_affinity, model.size_gb);
            best = Some(match best {
                Some((best_score, _, _)) if best_score >= score => best.unwrap(),
                _ => candidate,
            });
        }
    }
    best
}

/// Select the best backend for an OICP inference request.
///
/// Full scoring, per (node, claim) pair:
///
/// ```text
/// claim_score
///   × (effective_affinity / claimed_affinity)   // observed-health adjustment
///   × load_penalty                               // in-flight taper
///   × locality_bonus                             // LAN > WAN preference
///   × cold_start_weight                          // new-node ramp
///   × throughput_factor                          // [0.3, 1.0], from observed tg_tok/s or benchmark estimate
///   × inference_availability (clamped 0.2–1.0)   // gossip signal
/// ```
///
/// A node with `inference_capable: false` is eliminated before
/// scoring; a `None` `node_capabilities` is treated as capable and
/// idle so non-mesh backends still route.
///
/// `throughput_factor` slots after `cold_start_weight` because it
/// returns 1.0 (neutral) when neither observations nor a benchmark
/// are present — a peer with no data is identical to the
/// pre-throughput composition. New peers only become subject to
/// throughput scoring once they advertise a benchmark or accumulate
/// observation samples.
///
/// Returns the index into `candidates` of the selected entry, or
/// `None` if no candidate can serve the `requirements`.
pub fn pick_slot_for_oicp<'a>(
    candidates: &'a [BackendCandidate<'a>],
    requirements: &InferenceRequirements,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        // Hard gate: `inference_capable: false` is non-routable.
        // `None` = non-mesh backend → treat as capable.
        .filter(|(_, c)| {
            c.node_capabilities
                .map(|nc| nc.inference_capable)
                .unwrap_or(true)
        })
        .filter_map(|(idx, c)| {
            let (claim_score, claim_affinity, model_size_gb) =
                best_score_for_manifest(c.manifest, requirements)?;

            // Observed-health multiplier: effective / claimed.
            // Protect against zero-affinity claims (unlikely but
            // possible) by treating them as 1.0 — observation
            // changes nothing when there's nothing to adjust.
            let default_obs = NodeObservations::default();
            let obs = c.observations.unwrap_or(&default_obs);
            let observation_mult = if claim_affinity > 0.0 {
                effective_affinity(claim_affinity, obs) / claim_affinity
            } else {
                1.0
            };

            let load = load_penalty(obs);
            let locality = locality_bonus(c.locality);
            let cold_start = cold_start_weight(obs.samples);

            let baseline_benchmark = c.node_capabilities.and_then(|nc| nc.benchmark.as_ref());
            let candidate_size = model_size_gb.unwrap_or(0.0);
            let throughput = throughput_factor(obs, candidate_size, baseline_benchmark);
            tracing::debug!(
                idx,
                factor = throughput,
                source = throughput_factor_source(obs, baseline_benchmark),
                obs_samples = obs.samples,
                obs_tg_tok_s = obs.tg_tok_s_ewma,
                candidate_size_gb = candidate_size,
                "oicp_select: throughput_factor"
            );

            let availability = c
                .node_capabilities
                .map(|nc| nc.inference_availability)
                .unwrap_or(1.0_f32)
                .clamp(0.20, 1.0);

            let weighted = claim_score
                * observation_mult
                * load
                * locality
                * cold_start
                * throughput
                * availability;
            Some((idx, weighted))
        })
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oicp::{
        CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
        ProviderManifest, ProviderModel,
    };
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};

    fn make_caps(availability: f32) -> NodeCapabilities {
        NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 0,
                cpu_cores: 0,
                total_storage_gb: 0,
                free_storage_gb: 0,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 0,
            inference_availability: availability,
            inference_capable: true,
            loaded_models: vec![],
            embed_model: None,
            benchmark: None,
            current_in_flight: None,
        }
    }

    fn make_caps_with_capability(availability: f32, inference_capable: bool) -> NodeCapabilities {
        NodeCapabilities {
            inference_capable,
            ..make_caps(availability)
        }
    }

    fn manifest_with_claims(claims: Vec<CapabilityClaim>) -> ProviderManifest {
        ProviderManifest::new(vec![ProviderModel {
            id: "test".into(),
            base_model: None,
            quantization: None,
            context_tokens: claims.iter().map(|c| c.max_context).max().unwrap_or(0),
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
            claims,
        }])
    }

    fn v03_req(
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

    fn simple_general_request() -> InferenceRequirements {
        v03_req(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_000,
            1_000,
        )
    }

    fn simple_general_manifest(affinity: f32) -> ProviderManifest {
        manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            32_000,
            4_000,
            affinity,
        )])
    }

    #[test]
    fn availability_weighted_prefers_idle_over_hot_when_claims_equal() {
        let manifest = simple_general_manifest(0.8);
        let hot_caps = make_caps(0.20);
        let idle_caps = make_caps(1.00);
        let candidates = vec![
            BackendCandidate::new(&manifest).with_node_capabilities(&hot_caps),
            BackendCandidate::new(&manifest).with_node_capabilities(&idle_caps),
        ];
        let selected = pick_slot_for_oicp(&candidates, &simple_general_request());
        assert_eq!(selected, Some(1), "idle backend must win when claims match");
    }

    #[test]
    fn no_claim_satisfies_returns_none() {
        // Request asks for `code`; only a `general` claim exists.
        // Per hint_match_score, general→code is a 0.5 fallback — so
        // the node IS still reachable. To get None, request an
        // extension hint no claim covers.
        let manifest = simple_general_manifest(0.9);
        let caps = make_caps(1.0);
        let candidates = vec![BackendCandidate::new(&manifest).with_node_capabilities(&caps)];
        let req = v03_req(
            CapabilityHint::extension("biomed").unwrap(),
            LatencyClass::Normal,
            1_000,
            500,
        );
        // General claim against x:biomed request = 0.5 fallback, so still routes.
        assert!(pick_slot_for_oicp(&candidates, &req).is_some());

        // But if request exceeds the claim's context capacity, the hard
        // gate eliminates it — no routable candidate.
        let oversized = v03_req(
            CapabilityHint::general(),
            LatencyClass::Normal,
            100_000,
            1_000,
        );
        assert_eq!(pick_slot_for_oicp(&candidates, &oversized), None);
    }

    #[test]
    fn none_node_capabilities_treated_as_fully_idle() {
        let manifest = simple_general_manifest(0.8);
        let hot_caps = make_caps(0.20);
        let candidates = vec![
            BackendCandidate::new(&manifest).with_node_capabilities(&hot_caps),
            BackendCandidate::new(&manifest),
        ];
        let selected = pick_slot_for_oicp(&candidates, &simple_general_request());
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn empty_candidate_list_returns_none() {
        let selected = pick_slot_for_oicp(&[], &simple_general_request());
        assert_eq!(selected, None);
    }

    #[test]
    fn ghost_node_excluded_from_routing() {
        let manifest = simple_general_manifest(0.9);
        let ghost_caps = make_caps_with_capability(1.0, false);
        let capable_caps = make_caps_with_capability(1.0, true);
        let candidates = vec![
            BackendCandidate::new(&manifest).with_node_capabilities(&ghost_caps),
            BackendCandidate::new(&manifest).with_node_capabilities(&capable_caps),
        ];
        let selected = pick_slot_for_oicp(&candidates, &simple_general_request());
        assert_eq!(selected, Some(1), "ghost node must be excluded");
    }

    #[test]
    fn only_ghost_node_returns_none() {
        let manifest = simple_general_manifest(0.9);
        let ghost_caps = make_caps_with_capability(1.0, false);
        let candidates = vec![BackendCandidate::new(&manifest).with_node_capabilities(&ghost_caps)];
        assert_eq!(
            pick_slot_for_oicp(&candidates, &simple_general_request()),
            None,
            "single ghost node has no routing candidate"
        );
    }

    #[test]
    fn capable_node_with_low_availability_still_routes() {
        let manifest = simple_general_manifest(0.7);
        let low_avail = make_caps_with_capability(0.20, true);
        let candidates = vec![BackendCandidate::new(&manifest).with_node_capabilities(&low_avail)];
        let selected = pick_slot_for_oicp(&candidates, &simple_general_request());
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn coder_collective_routes_code_request_to_code_specialist() {
        // The spec's §6.2 scenario. A code request should win on the
        // Qwen Coder node over the 70B general peer, even though the
        // generalist has slightly lower but still-high affinity on
        // general work.
        let qwen_coder = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.95,
        )]);
        let llama_70b = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            64_000,
            4_000,
            0.85,
        )]);
        let idle = make_caps(1.0);
        let candidates = vec![
            BackendCandidate::new(&llama_70b).with_node_capabilities(&idle),
            BackendCandidate::new(&qwen_coder).with_node_capabilities(&idle),
        ];
        let req = v03_req(CapabilityHint::code(), LatencyClass::Normal, 16_000, 2_000);
        assert_eq!(
            pick_slot_for_oicp(&candidates, &req),
            Some(1),
            "code request must route to the code specialist, not the 70B general peer"
        );
    }

    #[test]
    fn hard_gate_eliminates_node_with_insufficient_context() {
        // Writer's local 8K model vs peer's 32K model, request needs
        // 16K context. Local claim is eliminated by the hard gate;
        // peer wins even if it has lower affinity.
        let local_small = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Fast,
            8_000, // not enough for 16K request
            1_000,
            0.9,
        )]);
        let peer_large = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            64_000,
            4_000,
            0.75,
        )]);
        let idle = make_caps(1.0);
        let candidates = vec![
            BackendCandidate::new(&local_small).with_node_capabilities(&idle),
            BackendCandidate::new(&peer_large).with_node_capabilities(&idle),
        ];
        let req = v03_req(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            2_000,
        );
        assert_eq!(
            pick_slot_for_oicp(&candidates, &req),
            Some(1),
            "hard context gate must eliminate the small local claim"
        );
    }

    #[test]
    fn multi_claim_model_picks_best_claim_per_request() {
        // A single 9B general model advertising both a fast-latency
        // short-context claim and a normal-latency full-context
        // claim. A fast short-context request should match the fast
        // claim; a normal long-context request should match the
        // other. The node still wins either way, but via different
        // claims — this is the unit-of-scheduling contract.
        let dual = manifest_with_claims(vec![
            CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Fast,
                4_000,
                500,
                0.85,
            ),
            CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Normal,
                16_000,
                2_000,
                0.65,
            ),
        ]);
        let idle = make_caps(1.0);
        let candidates = vec![BackendCandidate::new(&dual).with_node_capabilities(&idle)];
        let fast_req = v03_req(CapabilityHint::general(), LatencyClass::Fast, 2_000, 200);
        assert_eq!(pick_slot_for_oicp(&candidates, &fast_req), Some(0));
        let normal_req = v03_req(
            CapabilityHint::general(),
            LatencyClass::Normal,
            12_000,
            1_500,
        );
        assert_eq!(pick_slot_for_oicp(&candidates, &normal_req), Some(0));
    }

    #[test]
    fn hint_mismatch_falls_back_to_general_when_available() {
        // Request `code`. One peer offers an `x:prose` claim
        // (specific, wrong) and one peer offers `general` (fallback).
        // The general-serving peer must win: wrong specialization
        // scores 0 and is eliminated; general fallback wins at 0.5.
        let prose_peer = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::extension("prose").unwrap(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.9,
        )]);
        let general_peer = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.7,
        )]);
        let idle = make_caps(1.0);
        let candidates = vec![
            BackendCandidate::new(&prose_peer).with_node_capabilities(&idle),
            BackendCandidate::new(&general_peer).with_node_capabilities(&idle),
        ];
        let req = v03_req(CapabilityHint::code(), LatencyClass::Normal, 16_000, 2_000);
        assert_eq!(
            pick_slot_for_oicp(&candidates, &req),
            Some(1),
            "wrong specialization must be eliminated; general fallback wins"
        );
    }
}
