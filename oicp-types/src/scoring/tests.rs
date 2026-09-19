use super::*;
use crate::manifest::{ModelStatus, ProviderModel};
use crate::version::OICP_VERSION;

// ───── Scoring ──────────────────────────────────────────

fn claim(hint: CapabilityHint, lc: LatencyClass, ctx: u32, out: u32, aff: f32) -> CapabilityClaim {
    CapabilityClaim::new(hint, lc, ctx, out, aff)
}

fn req_with(hint: CapabilityHint, lc: LatencyClass, ctx: u32, out: u32) -> InferenceRequirements {
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
    let local_score =
        0.72_f32 * locality_bonus(NodeLocality::Local) * throughput_factor(&local_obs, 8.0, None);
    let remote_score =
        0.78_f32 * locality_bonus(NodeLocality::Far) * throughput_factor(&remote_obs, 8.0, None);
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
    let specialist = score_with_adjustments(1.0, 1.0, &light, NodeLocality::Far, 8.0, None, None);
    let generalist =
        score_with_adjustments(0.5, 0.85, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
    assert!(specialist.final_score > generalist.final_score);
}

#[test]
fn failing_node_loses_to_reliable_peer() {
    let mut flaky = quiet_obs();
    flaky.recent_failure_rate = 0.5; // past ramp ⇒ halves affinity
    assert!(score(&quiet_obs(), NodeLocality::Far, None) > score(&flaky, NodeLocality::Far, None));
}

#[test]
fn cold_start_deprioritizes_new_peer_vs_proven_peer() {
    let newcomer = NodeObservations::default(); // samples 0 ⇒ 0.7×
    assert!(
        score(&quiet_obs(), NodeLocality::Far, None) > score(&newcomer, NodeLocality::Far, None)
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

fn manifest_model(id: &str, size_gb: f32, hint: CapabilityHint, affinity: f32) -> ProviderModel {
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
        forward_budget: None,
    };
    let best = best_claim_for_request(&manifest, &req).unwrap();
    // Specialist at exact-hint 0.95 beats generalist's 0.5-fallback path.
    assert_eq!(best.model_id, "coder");
}

// ───── throughput observation EWMA ────────────────────

#[test]
fn ewma_seed_takes_first_value_when_zero() {
    let mut obs = NodeObservations::default();
    apply_throughput_observation(&mut obs, Some(120.0), Some(15.0));
    assert!((obs.ttft_ewma_ms - 120.0).abs() < 1e-9);
    assert!((obs.tg_tok_s_ewma - 15.0).abs() < 1e-9);
}

#[test]
fn ewma_blends_subsequent_samples_at_alpha() {
    let mut obs = NodeObservations::default();
    apply_throughput_observation(&mut obs, Some(100.0), Some(20.0));
    apply_throughput_observation(&mut obs, Some(200.0), Some(10.0));
    // alpha=0.3; 0.3*200 + 0.7*100 = 130
    assert!((obs.ttft_ewma_ms - 130.0).abs() < 1e-9);
    // 0.3*10 + 0.7*20 = 17
    assert!((obs.tg_tok_s_ewma - 17.0).abs() < 1e-9);
}

#[test]
fn ewma_ignores_none_inputs() {
    let mut obs = NodeObservations::default();
    apply_throughput_observation(&mut obs, Some(100.0), None);
    assert_eq!(obs.tg_tok_s_ewma, 0.0);
    apply_throughput_observation(&mut obs, None, Some(15.0));
    assert!((obs.ttft_ewma_ms - 100.0).abs() < 1e-9);
    assert!((obs.tg_tok_s_ewma - 15.0).abs() < 1e-9);
}
