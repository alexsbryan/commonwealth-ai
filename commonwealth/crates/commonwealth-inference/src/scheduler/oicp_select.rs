//! Availability-weighted OICP backend selection.
//!
//! `pick_slot_for_oicp` chooses the best peer from a candidate list by
//! multiplying each peer's OICP capability score by their
//! `inference_availability` weight from gossip. A peer with a great model
//! but currently coding (availability=0.20) scores lower than an idle peer
//! with a slightly weaker model.

use commonwealth_core::capabilities::NodeCapabilities;

use crate::oicp::{self, InferenceRequirements, ProviderManifest};

/// A candidate backend entry for OICP-based selection.
pub struct BackendCandidate<'a> {
    /// The peer's advertised OICP provider manifest.
    pub manifest: &'a ProviderManifest,
    /// The peer's gossip capabilities, carrying inference_availability.
    /// None for backends that don't participate in the activity-aware mesh.
    pub node_capabilities: Option<&'a NodeCapabilities>,
}

/// Select the best backend for an OICP inference request.
///
/// Scoring: `capability_score × inference_availability`. The availability
/// weight is clamped to `[0.20, 1.0]` so even the busiest peer is still
/// reachable for requests that find no better option.
///
/// Returns the index into `candidates` of the selected entry, or `None`
/// if no candidate satisfies the `requirements`.
pub fn pick_slot_for_oicp<'a>(
    candidates: &'a [BackendCandidate<'a>],
    requirements: &InferenceRequirements,
) -> Option<usize> {
    let required = requirements.required();
    let preferred = requirements.preferred();

    candidates
        .iter()
        .enumerate()
        // Hard gate: a node that has explicitly declared inference_capable: false
        // is structurally unable to serve — exclude it before scoring.
        // None node_capabilities = legacy pre-mesh node = do not exclude
        // (preserves backward-compatibility with non-mesh backends).
        .filter(|(_, c)| {
            c.node_capabilities
                .map(|nc| nc.inference_capable)
                .unwrap_or(true)
        })
        .filter_map(|(idx, c)| {
            let cap_score = c
                .manifest
                .models
                .iter()
                .filter(|m| m.status.available)
                .filter(|m| oicp::satisfies_required(&m.capabilities, required))
                .map(|m| oicp::score_preferred(&m.capabilities, preferred))
                .fold(f32::NEG_INFINITY, f32::max);

            if cap_score == f32::NEG_INFINITY {
                return None;
            }

            let availability = c
                .node_capabilities
                .map(|nc| nc.inference_availability)
                .unwrap_or(1.0_f32)
                .clamp(0.20, 1.0);

            let weighted = cap_score * availability;
            Some((idx, weighted))
        })
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::{
        AvailableResources, HardwareProfile, NodeCapabilities,
    };
    use crate::oicp::{
        Capability, CapabilityProfile, CapabilityRequirements, InferenceRequirements,
        ModelStatus, ProviderModel, ProviderManifest,
    };

    fn make_manifest(cap_score: u8) -> ProviderManifest {
        let mut caps = CapabilityProfile::default();
        caps.insert(Capability::Code, cap_score);
        ProviderManifest::new(vec![ProviderModel {
            id: "test".into(),
            base_model: None,
            quantization: None,
            capabilities: caps,
            context_tokens: 4096,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
        }])
    }

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

            embed_model: None,        }
    }

    fn reqs_requiring_code(min_level: u8) -> InferenceRequirements {
        let mut required = CapabilityProfile::default();
        required.insert(Capability::Code, min_level);
        let mut reqs = InferenceRequirements::new();
        reqs.capabilities = Some(CapabilityRequirements {
            required,
            preferred: CapabilityProfile::default(),
        });
        reqs
    }

    #[test]
    fn prefers_idle_over_hot_when_capability_equal() {
        let hot_caps = make_caps(0.20);
        let idle_caps = make_caps(1.00);
        let manifest = make_manifest(4);

        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&hot_caps) },
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&idle_caps) },
        ];

        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(selected, Some(1), "should pick the idle backend (index 1)");
    }

    #[test]
    fn no_match_returns_none() {
        let caps = make_caps(1.0);
        let manifest = make_manifest(2); // level 2

        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&caps) },
        ];

        // Require level 4 — manifest only has level 2, so satisfies_required returns false.
        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(4));
        assert_eq!(selected, None);
    }

    #[test]
    fn none_node_capabilities_treated_as_fully_idle() {
        // A backend without node_capabilities (e.g. pre-mesh node) should
        // default to availability=1.0 and win over a hot peer.
        let hot_caps = make_caps(0.20);
        let manifest = make_manifest(4);

        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&hot_caps) },
            BackendCandidate { manifest: &manifest, node_capabilities: None }, // defaults to 1.0
        ];

        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(selected, Some(1), "backend without node_capabilities should win over hot peer");
    }

    fn make_manifest_with_analysis(code_score: u8, analysis_score: u8) -> ProviderManifest {
        let mut caps = CapabilityProfile::default();
        caps.insert(Capability::Code, code_score);
        caps.insert(Capability::Analysis, analysis_score);
        ProviderManifest::new(vec![ProviderModel {
            id: "test".into(),
            base_model: None,
            quantization: None,
            capabilities: caps,
            context_tokens: 4096,
            status: ModelStatus { available: true, loaded: true, estimated_tokens_per_sec: None,
                estimated_ttft_ms: None, estimated_load_time_sec: None },
            size_gb: None,
        }])
    }

    fn reqs_preferring(min_code: u8, preferred_code: u8, preferred_analysis: u8) -> InferenceRequirements {
        let mut required = CapabilityProfile::default();
        required.insert(Capability::Code, min_code);
        let mut preferred = CapabilityProfile::default();
        preferred.insert(Capability::Code, preferred_code);
        preferred.insert(Capability::Analysis, preferred_analysis);
        let mut reqs = InferenceRequirements::new();
        reqs.capabilities = Some(CapabilityRequirements { required, preferred });
        reqs
    }

    #[test]
    fn hot_node_beats_idle_when_capability_gap_is_large_enough() {
        // score_preferred({Code:10,Analysis:10}, preferred={Code:10,Analysis:10}) = 1.0
        // → hot weighted = 1.0 × 0.20 = 0.20
        //
        // score_preferred({Code:1,Analysis:1}, preferred={Code:10,Analysis:10}) = 0.1
        // → idle weighted = 0.1 × 1.0 = 0.10
        //
        // 0.20 > 0.10 → hot wins.
        let hot_caps = make_caps(0.20);
        let idle_caps = make_caps(1.00);
        let strong = make_manifest_with_analysis(10, 10);
        let weak = make_manifest_with_analysis(1, 1);

        let candidates = vec![
            BackendCandidate { manifest: &strong, node_capabilities: Some(&hot_caps) },
            BackendCandidate { manifest: &weak, node_capabilities: Some(&idle_caps) },
        ];

        let selected = pick_slot_for_oicp(&candidates, &reqs_preferring(1, 10, 10));
        assert_eq!(
            selected, Some(0),
            "hot node with 10× better preferred-capability score must beat the idle peer's 5× availability bonus"
        );
    }

    #[test]
    fn equal_scores_with_empty_preferred_tiebreak_to_last_candidate() {
        // When preferred profile is empty, score_preferred returns 0.0 for all
        // models that pass the required check.  Both weighted scores are equal,
        // so max_by (stable: picks last) returns the last candidate.
        let hot_caps = make_caps(0.20);
        let idle_caps = make_caps(1.00);
        let manifest_a = make_manifest(8);
        let manifest_b = make_manifest(4);

        let candidates = vec![
            BackendCandidate { manifest: &manifest_a, node_capabilities: Some(&hot_caps) },
            BackendCandidate { manifest: &manifest_b, node_capabilities: Some(&idle_caps) },
        ];

        // With empty preferred, both score 0.0 — idle (last) wins via stable max_by.
        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(
            selected, Some(1),
            "with empty preferred profile both scores are 0.0; idle (last) wins as tiebreaker"
        );
    }

    #[test]
    fn empty_candidate_list_returns_none() {
        let selected = pick_slot_for_oicp(&[], &reqs_requiring_code(1));
        assert_eq!(selected, None);
    }

    fn make_caps_with_capability(availability: f32, inference_capable: bool) -> NodeCapabilities {
        NodeCapabilities {
            inference_capable,
            ..make_caps(availability)
        }
    }

    #[test]
    fn ghost_node_excluded_from_routing() {
        // A node with inference_capable: false must never be routed to,
        // even if it would otherwise have the highest capability score.
        let ghost_caps = make_caps_with_capability(1.0, false);
        let capable_caps = make_caps_with_capability(1.0, true);
        let manifest = make_manifest(10);

        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&ghost_caps) },
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&capable_caps) },
        ];

        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(selected, Some(1), "ghost node (inference_capable: false) must be excluded");
    }

    #[test]
    fn only_ghost_node_returns_none() {
        let ghost_caps = make_caps_with_capability(1.0, false);
        let manifest = make_manifest(10);

        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&ghost_caps) },
        ];

        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(selected, None, "single ghost node must result in no routing candidate");
    }

    #[test]
    fn none_node_capabilities_not_excluded_by_hard_gate() {
        // A backend without node_capabilities (legacy non-mesh node) must
        // still be routed to — None is treated as capable (backward compat).
        let manifest = make_manifest(4);
        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: None },
        ];
        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(selected, Some(0), "None node_capabilities must not trigger ghost-node exclusion");
    }

    #[test]
    fn capable_node_with_low_availability_still_routes() {
        // inference_capable: true with low availability must still appear as a candidate.
        let low_avail_caps = make_caps_with_capability(0.20, true);
        let manifest = make_manifest(4);

        let candidates = vec![
            BackendCandidate { manifest: &manifest, node_capabilities: Some(&low_avail_caps) },
        ];

        let selected = pick_slot_for_oicp(&candidates, &reqs_requiring_code(1));
        assert_eq!(selected, Some(0), "capable node with low availability must still be routable");
    }
}
