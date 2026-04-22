//! v0.3 §7 observation-adjusted routing scenarios. These exercise
//! the scheduler's second-pass scoring (effective_affinity,
//! load_penalty, locality_bonus, cold_start_weight) rather than the
//! first-pass claim ranking — which is covered in
//! `oicp_v03_scenarios.rs`.

use commonwealth_core::capabilities::{
    AvailableResources, HardwareProfile, NodeCapabilities,
};
use commonwealth_inference::oicp::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass,
    ModelStatus, NodeLocality, NodeObservations, ProviderManifest,
    ProviderModel, COLD_START_SAMPLES,
};
use commonwealth_inference::scheduler::oicp_select::{
    pick_slot_for_oicp, BackendCandidate,
};

fn idle_capable() -> NodeCapabilities {
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
        inference_availability: 1.0,
        inference_capable: true,
        loaded_models: vec![],
        embed_model: None,
    }
}

fn manifest(claim: CapabilityClaim) -> ProviderManifest {
    ProviderManifest::new(vec![ProviderModel {
        id: "test".into(),
        base_model: None,
        quantization: None,
        context_tokens: claim.max_context,
        status: ModelStatus {
            available: true,
            loaded: true,
            estimated_tokens_per_sec: None,
            estimated_ttft_ms: None,
            estimated_load_time_sec: None,
        },
        size_gb: None,
        claims: vec![claim],
    }])
}

fn req(hint: CapabilityHint, lc: LatencyClass) -> InferenceRequirements {
    InferenceRequirements::new()
        .with_hint(hint)
        .with_latency_class(lc)
        .with_context_tokens(4_000)
        .with_max_output_tokens(1_000)
}

// -------------------------------------------------------------
// Thundering herd
// -------------------------------------------------------------

#[test]
fn thundering_herd_shifts_traffic_to_idle_peer() {
    // Node A is the reigning code specialist at 0.95 affinity with
    // 20 concurrent in-flight requests. Node B is a general peer at
    // 0.85 affinity with zero load. Without load penalty, A would
    // always win (0.95 > 0.85 × 0.5 fallback = 0.425). With load
    // penalty at 20 in-flight (~0.5 multiplier), A's effective
    // score drops and B takes the next request.
    let a = manifest(CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.95,
    ));
    let b = manifest(CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let caps = idle_capable();
    let a_obs = NodeObservations {
        in_flight: 25,
        samples: 100,
        recent_failure_rate: 0.0,
        ..Default::default()
    };
    let b_obs = NodeObservations {
        in_flight: 0,
        samples: 100,
        recent_failure_rate: 0.0,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&a)
            .with_node_capabilities(&caps)
            .with_observations(&a_obs),
        BackendCandidate::new(&b)
            .with_node_capabilities(&caps)
            .with_observations(&b_obs),
    ];
    let r = req(CapabilityHint::code(), LatencyClass::Normal);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(1),
        "heavy in-flight load on A must divert the next request to B \
         even though A has the higher claimed affinity"
    );
}

#[test]
fn low_load_keeps_traffic_on_specialist() {
    // Inverse of the herd scenario: when A's load is modest (3
    // in-flight), the load penalty is gentle (~0.87) and A's
    // 0.95 affinity still beats B's 0.80.
    let a = manifest(CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.95,
    ));
    let b = manifest(CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let caps = idle_capable();
    let a_obs = NodeObservations {
        in_flight: 3,
        samples: 100,
        ..Default::default()
    };
    let b_obs = NodeObservations {
        in_flight: 0,
        samples: 100,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&a)
            .with_node_capabilities(&caps)
            .with_observations(&a_obs),
        BackendCandidate::new(&b)
            .with_node_capabilities(&caps)
            .with_observations(&b_obs),
    ];
    let r = req(CapabilityHint::code(), LatencyClass::Normal);
    assert_eq!(pick_slot_for_oicp(&candidates, &r), Some(0));
}

// -------------------------------------------------------------
// Observed failures degrade effective affinity
// -------------------------------------------------------------

#[test]
fn failing_node_loses_to_reliable_peer() {
    // Node A advertises stronger affinity but has been failing 40%
    // of recent requests. With 50+ samples the observation fully
    // applies, effectively dropping A's affinity to 0.95 × 0.6 =
    // 0.57 — below B's 0.85.
    let a = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.95,
    ));
    let b = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.85,
    ));
    let caps = idle_capable();
    let a_obs = NodeObservations {
        in_flight: 0,
        samples: 100,
        recent_failure_rate: 0.4,
        ..Default::default()
    };
    let b_obs = NodeObservations {
        in_flight: 0,
        samples: 100,
        recent_failure_rate: 0.0,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&a)
            .with_node_capabilities(&caps)
            .with_observations(&a_obs),
        BackendCandidate::new(&b)
            .with_node_capabilities(&caps)
            .with_observations(&b_obs),
    ];
    let r = req(CapabilityHint::general(), LatencyClass::Normal);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(1),
        "40% failure rate on A must override its higher claimed affinity"
    );
}

// -------------------------------------------------------------
// Cold-start ramp
// -------------------------------------------------------------

#[test]
fn cold_start_deprioritizes_new_peer_vs_proven_peer() {
    // Both peers advertise equal affinity. Peer A has a long track
    // record (100 samples, no failures); peer B just joined (0
    // samples). Cold-start weight penalizes B by ~0.3 at zero
    // samples, so A wins the first request.
    let a = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let b = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let caps = idle_capable();
    let a_obs = NodeObservations {
        samples: 100,
        ..Default::default()
    };
    let b_obs = NodeObservations {
        samples: 0,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&a)
            .with_node_capabilities(&caps)
            .with_observations(&a_obs),
        BackendCandidate::new(&b)
            .with_node_capabilities(&caps)
            .with_observations(&b_obs),
    ];
    let r = req(CapabilityHint::general(), LatencyClass::Normal);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(0),
        "new peer's cold-start weight must defer to the proven peer"
    );
}

#[test]
fn cold_start_fully_ramped_after_threshold_samples() {
    // After COLD_START_SAMPLES observations, both peers sit at
    // cold_start_weight = 1.0 and ranking falls back to the raw
    // score — so the specialist (higher affinity) wins.
    let a = manifest(CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.95,
    ));
    let b = manifest(CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let caps = idle_capable();
    let a_obs = NodeObservations {
        samples: COLD_START_SAMPLES,
        ..Default::default()
    };
    let b_obs = NodeObservations {
        samples: COLD_START_SAMPLES,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&a)
            .with_node_capabilities(&caps)
            .with_observations(&a_obs),
        BackendCandidate::new(&b)
            .with_node_capabilities(&caps)
            .with_observations(&b_obs),
    ];
    let r = req(CapabilityHint::code(), LatencyClass::Normal);
    assert_eq!(pick_slot_for_oicp(&candidates, &r), Some(0));
}

// -------------------------------------------------------------
// Locality
// -------------------------------------------------------------

#[test]
fn local_node_wins_over_remote_with_higher_affinity() {
    // §6 spec example: a local 0.7-affinity node beats a remote
    // 0.8-affinity node because locality_bonus(Local) = 1.15.
    //   local_score = 0.7 × 1.15 = 0.805
    //   far_score   = 0.8 × 1.00 = 0.800
    let local = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.70,
    ));
    let remote = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let caps = idle_capable();
    // Both have observation history so cold-start doesn't skew the
    // outcome.
    let obs = NodeObservations {
        samples: 100,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&local)
            .with_node_capabilities(&caps)
            .with_observations(&obs)
            .with_locality(NodeLocality::Local),
        BackendCandidate::new(&remote)
            .with_node_capabilities(&caps)
            .with_observations(&obs)
            .with_locality(NodeLocality::Far),
    ];
    let r = req(CapabilityHint::general(), LatencyClass::Normal);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(0),
        "locality bonus must let a local 0.70 node beat a remote 0.80 node"
    );
}

#[test]
fn near_lan_peer_beats_far_internet_peer_at_equal_affinity() {
    let near_manifest = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let far_manifest = manifest(CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        32_000,
        4_000,
        0.80,
    ));
    let caps = idle_capable();
    let obs = NodeObservations {
        samples: 100,
        ..Default::default()
    };
    let candidates = vec![
        BackendCandidate::new(&near_manifest)
            .with_node_capabilities(&caps)
            .with_observations(&obs)
            .with_locality(NodeLocality::Near),
        BackendCandidate::new(&far_manifest)
            .with_node_capabilities(&caps)
            .with_observations(&obs)
            .with_locality(NodeLocality::Far),
    ];
    let r = req(CapabilityHint::general(), LatencyClass::Normal);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(0),
        "LAN peer must beat WAN peer at equal affinity"
    );
}
