// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.3 reference-scenario integration tests (§11 of
//! `commonwealth/docs/oicp-v0.3.md`). Each test drives the
//! commonwealth-inference scheduler (`pick_slot_for_oicp`) through
//! a distinct deployment shape to prove that specialization-aware
//! routing actually achieves the acceptance criteria.
//!
//! The scheduler is pure: it takes a slice of `BackendCandidate`
//! and an `InferenceRequirements`, returns `Option<usize>`. The
//! tests synthesize the candidates directly — no HTTP, no gossip,
//! no mesh bring-up. That keeps the acceptance check to ~20ms per
//! scenario.

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_inference::oicp::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
    ProviderManifest, ProviderModel,
};
use commonwealth_inference::scheduler::oicp_select::{pick_slot_for_oicp, BackendCandidate};

fn node_caps(availability: f32) -> NodeCapabilities {
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

fn req(hint: CapabilityHint, lc: LatencyClass, ctx: u32, out: u32) -> InferenceRequirements {
    InferenceRequirements::new()
        .with_hint(hint)
        .with_latency_class(lc)
        .with_context_tokens(ctx)
        .with_max_output_tokens(out)
}

// -------------------------------------------------------------
// §6.1 — The newsroom writer
// -------------------------------------------------------------
//
// 16 GB MacBook Air with a small-fast 8K-context model. Part of a
// collective including a Mac Studio with a 64K-context larger model.
// Routine work (fast latency, short context) must stay local. Heavy
// work (normal latency, 16K+ context) must route to the Mac Studio.

#[test]
fn newsroom_writer_short_fast_request_stays_local() {
    let local = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Fast,
        8_000,
        1_000,
        0.7,
    )]);
    let peer = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        64_000,
        4_000,
        0.85,
    )]);
    let idle = node_caps(1.0);
    let candidates = vec![
        BackendCandidate::new(&local).with_node_capabilities(&idle),
        BackendCandidate::new(&peer).with_node_capabilities(&idle),
    ];
    // Fast drafting / reword / classification: fast latency, short
    // input, small output.
    let r = req(CapabilityHint::general(), LatencyClass::Fast, 2_000, 200);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(0),
        "quick local request should stay local — local claim is an \
         exact match on both hint and latency while the peer's \
         Normal-latency claim is only adjacent"
    );
}

#[test]
fn newsroom_writer_long_normal_request_routes_to_peer() {
    let local = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Fast,
        8_000,
        1_000,
        0.7,
    )]);
    let peer = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        64_000,
        4_000,
        0.85,
    )]);
    let idle = node_caps(1.0);
    let candidates = vec![
        BackendCandidate::new(&local).with_node_capabilities(&idle),
        BackendCandidate::new(&peer).with_node_capabilities(&idle),
    ];
    // Substantive research synthesis: normal latency, 16K context,
    // 2K output. Hard context gate eliminates the 8K local claim.
    let r = req(
        CapabilityHint::general(),
        LatencyClass::Normal,
        16_000,
        2_000,
    );
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(1),
        "16K context exceeds local claim's 8K max_context — must \
         route to peer via hard gate"
    );
}

// -------------------------------------------------------------
// §6.2 — The coder collective
// -------------------------------------------------------------
//
// Five engineers. Node A: Qwen Coder 32B (code, normal, 0.95).
// Node B: DeepSeek R1 Distill (general, extended, 0.85). Node C:
// Llama 3.3 70B (general, normal, 0.85). Nodes D/E: laptop fast
// general (fast, 0.7). Four requests of different shapes route to
// four different nodes.

fn build_coder_collective() -> (
    ProviderManifest,
    ProviderManifest,
    ProviderManifest,
    ProviderManifest,
) {
    // Node A: Qwen Coder 32B specialist. Advertises the code claim
    // at 0.95 and a general claim at 0.80 — honest self-report: the
    // model handles general work fine, just not as well as Llama
    // 3.3 70B (0.85). This mirrors the realistic advertiser pattern
    // that sovereign-mesh's build_self_manifest would produce once
    // a collective node runs multiple slots.
    let a = manifest_with_claims(vec![
        CapabilityClaim::new(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.95,
        ),
        CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.80,
        ),
    ]);
    let b = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Extended,
        64_000,
        8_000,
        0.85,
    )]);
    let c = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        64_000,
        4_000,
        0.85,
    )]);
    let d = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Fast,
        8_000,
        1_000,
        0.7,
    )]);
    (a, b, c, d)
}

#[test]
fn coder_collective_code_request_routes_to_specialist() {
    let (a, b, c, d) = build_coder_collective();
    let idle = node_caps(1.0);
    let candidates = vec![
        BackendCandidate::new(&a).with_node_capabilities(&idle),
        BackendCandidate::new(&b).with_node_capabilities(&idle),
        BackendCandidate::new(&c).with_node_capabilities(&idle),
        BackendCandidate::new(&d).with_node_capabilities(&idle),
    ];
    // Coding request: code hint, normal latency, medium context.
    // Node A is the only claim that exact-matches the hint.
    let r = req(CapabilityHint::code(), LatencyClass::Normal, 16_000, 2_000);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(0),
        "code-hinted request must route to Qwen Coder specialist"
    );
}

#[test]
fn coder_collective_architecture_planning_routes_to_reasoning_node() {
    let (a, b, c, d) = build_coder_collective();
    let idle = node_caps(1.0);
    let candidates = vec![
        BackendCandidate::new(&a).with_node_capabilities(&idle),
        BackendCandidate::new(&b).with_node_capabilities(&idle),
        BackendCandidate::new(&c).with_node_capabilities(&idle),
        BackendCandidate::new(&d).with_node_capabilities(&idle),
    ];
    // Architecture planning: general hint, extended latency, large
    // context. Node B is the only Extended-latency general claim.
    let r = req(
        CapabilityHint::general(),
        LatencyClass::Extended,
        48_000,
        4_000,
    );
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(1),
        "extended-latency general request must route to reasoning node"
    );
}

#[test]
fn coder_collective_design_doc_routes_to_large_general_node() {
    let (a, b, c, d) = build_coder_collective();
    let idle = node_caps(1.0);
    let candidates = vec![
        BackendCandidate::new(&a).with_node_capabilities(&idle),
        BackendCandidate::new(&b).with_node_capabilities(&idle),
        BackendCandidate::new(&c).with_node_capabilities(&idle),
        BackendCandidate::new(&d).with_node_capabilities(&idle),
    ];
    // Design doc: general hint, normal latency, medium context.
    // Node A's code claim scores high (1.0 × 1.0 × 0.95 = 0.95)
    // against a general request (general matches any), but A's
    // honest general claim at 0.80 is what ranks; Node C at 0.85
    // affinity exactly-matches hint + latency and wins with 0.85.
    // Node D can't fit 16K context via its 8K gate. Node B's
    // Extended latency is one class off Normal → 0.8 penalty.
    //
    // This is exactly the desired semantic: even a code specialist
    // that also advertises a general claim shouldn't out-rank the
    // dedicated general-purpose node for general work.
    let r = req(
        CapabilityHint::general(),
        LatencyClass::Normal,
        16_000,
        2_000,
    );
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(2),
        "general request at normal latency must route to the \
         dedicated general node (Llama 70B), not the code \
         specialist's secondary general claim"
    );
}

#[test]
fn coder_collective_fast_classification_stays_on_laptop() {
    let (a, b, c, d) = build_coder_collective();
    let idle = node_caps(1.0);
    let candidates = vec![
        BackendCandidate::new(&a).with_node_capabilities(&idle),
        BackendCandidate::new(&b).with_node_capabilities(&idle),
        BackendCandidate::new(&c).with_node_capabilities(&idle),
        BackendCandidate::new(&d).with_node_capabilities(&idle),
    ];
    // Fast classification: general hint, fast latency, tiny.
    // Only Node D exact-matches Fast latency.
    let r = req(CapabilityHint::general(), LatencyClass::Fast, 1_500, 100);
    assert_eq!(
        pick_slot_for_oicp(&candidates, &r),
        Some(3),
        "fast general request must route to the laptop with the fast claim"
    );
}

// -------------------------------------------------------------
// §6.3 — The capable solo user
// -------------------------------------------------------------
//
// Single node with two claims on one machine. No peers. Every
// request routes to the single local node. The scheduler just has
// to not crash on a one-candidate list.

#[test]
fn solo_user_all_requests_route_to_sole_candidate() {
    let local = manifest_with_claims(vec![
        CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Fast,
            4_000,
            500,
            0.8,
        ),
        CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            2_000,
            0.7,
        ),
    ]);
    let idle = node_caps(1.0);
    let candidates = vec![BackendCandidate::new(&local).with_node_capabilities(&idle)];

    // Fast, small.
    let fast_req = req(CapabilityHint::general(), LatencyClass::Fast, 1_000, 200);
    assert_eq!(pick_slot_for_oicp(&candidates, &fast_req), Some(0));

    // Normal, medium.
    let normal_req = req(
        CapabilityHint::general(),
        LatencyClass::Normal,
        12_000,
        1_500,
    );
    assert_eq!(pick_slot_for_oicp(&candidates, &normal_req), Some(0));

    // Code request — no specialist available → general fallback,
    // still routes to the sole candidate.
    let code_req = req(CapabilityHint::code(), LatencyClass::Normal, 8_000, 1_000);
    assert_eq!(pick_slot_for_oicp(&candidates, &code_req), Some(0));
}

#[test]
fn solo_user_oversized_request_returns_none() {
    // Request exceeds every advertised claim's hard gate.
    let local = manifest_with_claims(vec![CapabilityClaim::new(
        CapabilityHint::general(),
        LatencyClass::Normal,
        16_000,
        2_000,
        0.7,
    )]);
    let idle = node_caps(1.0);
    let candidates = vec![BackendCandidate::new(&local).with_node_capabilities(&idle)];
    let oversized = req(
        CapabilityHint::general(),
        LatencyClass::Normal,
        64_000,
        4_000,
    );
    assert_eq!(
        pick_slot_for_oicp(&candidates, &oversized),
        None,
        "solo user routing an oversized request must not silently \
         route to a claim that can't fit — the caller needs to see \
         the failure and surface it to the user"
    );
}
