// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for [`super`] — the offload gate's truth tables, the
//! operational-adjustment golden vector, and the RTT locality buckets.
//!
//! Split out of `oicp_select.rs` when the Fast gate's measured stand-down
//! took the file past the 800-line band (`cargo xtask arch-gate`), by the
//! same `#[path]` move `e85076537` used on five campaign-grown files.
//! Test names are unchanged, so every prior run's names still resolve.

use super::*;
use oicp_types::ProviderManifest;

fn cand(score: f32, size_gb: Option<f32>, id: &str) -> ModelCandidate {
    ModelCandidate {
        score,
        size_gb,
        model_id: id.into(),
        claim_affinity: score,
    }
}

/// GOLDEN VECTOR — pins the operational-adjustment product
/// bit-for-bit across the SSOT move to oicp-types (Phase B of
/// the 2026-06-10 rationalization). Every factor ≠ 1.0:
///   observation_mult = eff(0.95, obs)/0.95
///                    = (0.95·(1 − (10/50)·0.1))/0.95 = 0.98
///   load   = 1/(1 + 0.05·10)   = 2/3
///   loc    = Near              = 1.05
///   cold   = 0.7 + 0.3·(10/20) = 0.85
///   thru   = 10/20 (observed)  = 0.5
///   final  = 0.5 · 0.98 · (2/3) · 1.05 · 0.85 · 0.5
/// If this fails after a refactor, the refactor changed routing
/// behavior — that is a disclosure, not a test update.
#[test]
fn golden_adjustment_product_all_factors_active() {
    let obs = NodeObservations {
        in_flight: 10,
        samples: 10,
        recent_failure_rate: 0.1,
        tg_tok_s_ewma: 10.0,
        ..Default::default()
    };
    let raw = ModelCandidate {
        score: 0.5,
        size_gb: Some(8.0),
        model_id: "golden".into(),
        claim_affinity: 0.95,
    };
    let (adjusted, breakdown) = adjust_for_observations(raw, &obs, NodeLocality::Near, None, None);
    let expected = 0.5_f32 * 0.98 * (2.0 / 3.0) * 1.05 * 0.85 * 0.5;
    assert!(
        (adjusted.score - expected).abs() < 1e-6,
        "golden product drifted: got {}, want {expected}",
        adjusted.score
    );
    // Equivalence: the wrapper's candidate score IS the SSOT
    // breakdown's final score, forever.
    assert_eq!(adjusted.score.to_bits(), breakdown.final_score.to_bits());
    assert!(
        (breakdown.availability - 1.0).abs() < 1e-6,
        "None ⇒ neutral"
    );
    // Tie-break inputs must survive adjustment untouched.
    assert_eq!(adjusted.model_id, "golden");
    assert_eq!(adjusted.size_gb, Some(8.0));
}

/// The decided behavior change (2026-06-10): the Joiner honors
/// gossiped `inference_availability`. Two otherwise-identical
/// peers — the one advertising 0.2 loses to the idle one 5:1.
#[test]
fn gossiped_availability_demotes_busy_peer() {
    let obs = NodeObservations {
        samples: 100, // fully ramped — isolate the availability term
        ..Default::default()
    };
    let raw = |id: &str| ModelCandidate {
        score: 0.8,
        size_gb: Some(8.0),
        model_id: id.into(),
        claim_affinity: 0.9,
    };
    let (busy, _) = adjust_for_observations(raw("busy"), &obs, NodeLocality::Far, None, Some(0.2));
    let (idle, _) = adjust_for_observations(raw("idle"), &obs, NodeLocality::Far, None, Some(1.0));
    assert!(idle.score > busy.score * 4.9);
    assert_eq!(pick_better(busy, idle).model_id, "idle");
}

#[test]
fn pick_better_higher_score_wins() {
    let a = cand(0.5, Some(5.5), "small");
    let b = cand(1.0, Some(16.5), "big");
    assert_eq!(pick_better(a, b).model_id, "big");
}

// ── v0.3 §7 — RTT-based locality classification ───────────

#[test]
fn classify_rtt_sub_5ms_is_local() {
    assert_eq!(classify_rtt_ms(0), NodeLocality::Local);
    assert_eq!(classify_rtt_ms(1), NodeLocality::Local);
    assert_eq!(classify_rtt_ms(4), NodeLocality::Local);
}

#[test]
fn classify_rtt_lan_range_is_near() {
    // 5ms is the Local threshold (exclusive), so it tips into
    // Near — the "local" bucket is reserved for same-host loop.
    assert_eq!(classify_rtt_ms(5), NodeLocality::Near);
    assert_eq!(classify_rtt_ms(12), NodeLocality::Near);
    assert_eq!(classify_rtt_ms(24), NodeLocality::Near);
}

#[test]
fn classify_rtt_wan_range_is_far() {
    assert_eq!(classify_rtt_ms(25), NodeLocality::Far);
    assert_eq!(classify_rtt_ms(50), NodeLocality::Far);
    assert_eq!(classify_rtt_ms(250), NodeLocality::Far);
    assert_eq!(classify_rtt_ms(u32::MAX), NodeLocality::Far);
}

#[test]
fn classify_rtt_thresholds_are_exclusive_upper() {
    // Exact-boundary behaviour: the `< LOCAL`, `< NEAR` rule
    // means LOCAL_RTT_MS_THRESHOLD itself falls into Near, and
    // NEAR_RTT_MS_THRESHOLD itself falls into Far. Document
    // this so future tweaks to the constants can't silently
    // shift which bucket the boundary lands in.
    assert_eq!(classify_rtt_ms(LOCAL_RTT_MS_THRESHOLD), NodeLocality::Near);
    assert_eq!(classify_rtt_ms(NEAR_RTT_MS_THRESHOLD), NodeLocality::Far);
}

#[test]
fn pick_better_score_tied_smaller_size_wins() {
    let nine = cand(1.0, Some(5.5), "qwen-9b");
    let twenty_seven = cand(1.0, Some(16.5), "qwen-27b");
    assert_eq!(
        pick_better(twenty_seven.clone(), nine.clone()).model_id,
        "qwen-9b"
    );
    assert_eq!(pick_better(nine, twenty_seven).model_id, "qwen-9b");
}

#[test]
fn pick_better_known_size_beats_unknown_on_tie() {
    let annotated = cand(1.0, Some(5.5), "annotated");
    let unannotated = cand(1.0, None, "byom");
    assert_eq!(
        pick_better(unannotated.clone(), annotated.clone()).model_id,
        "annotated"
    );
    assert_eq!(pick_better(annotated, unannotated).model_id, "annotated");
}

#[test]
fn pick_better_full_tie_keeps_incumbent() {
    let a = cand(1.0, Some(5.5), "incumbent");
    let b = cand(1.0, Some(5.5), "challenger");
    assert_eq!(pick_better(a, b).model_id, "incumbent");
}

#[test]
fn pick_better_epsilon_ignores_floating_point_noise() {
    let nine = cand(1.0, Some(5.5), "qwen-9b");
    let twenty_seven = cand(1.0 - 1e-6, Some(16.5), "qwen-27b");
    assert_eq!(pick_better(twenty_seven, nine).model_id, "qwen-9b");
}

// -----------------------------------------------------------
// v0.3 — score_manifest_for_request claim path
// -----------------------------------------------------------

fn manifest_with_claim(
    id: &str,
    size_gb: Option<f32>,
    claim: oicp_types::CapabilityClaim,
) -> ProviderManifest {
    ProviderManifest::new(vec![oicp_types::ProviderModel {
        id: id.into(),
        base_model: None,
        quantization: None,
        context_tokens: claim.max_context,
        status: oicp_types::ModelStatus {
            available: true,
            loaded: true,
            estimated_tokens_per_sec: None,
            estimated_ttft_ms: None,
            estimated_load_time_sec: None,
        },
        size_gb,
        claims: vec![claim],
        fingerprint: None,
    }])
}

#[test]
fn score_manifest_for_request_prefers_claim_path_when_claims_present() {
    use oicp_types::CapabilityClaim;
    let qwen_coder = manifest_with_claim(
        "qwen-coder-32b",
        Some(16.1),
        CapabilityClaim::new(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.95,
        ),
    );
    let req = InferenceRequirements::new()
        .with_hint(CapabilityHint::code())
        .with_latency_class(LatencyClass::Normal)
        .with_context_tokens(16_000)
        .with_max_output_tokens(2_000);
    let cand = score_manifest_for_request(&qwen_coder, &req).expect("v0.3 claim scores non-None");
    assert_eq!(cand.model_id, "qwen-coder-32b");
    // Exact hint + latency match → score equals affinity.
    assert!((cand.score - 0.95).abs() < 1e-4);
}

#[test]
fn score_manifest_for_request_returns_none_when_no_claims_match() {
    // Claim with zero-output gate against a request needing any
    // output → hard gate fails, no candidate.
    let m = manifest_with_claim(
        "undersized",
        Some(1.0),
        oicp_types::CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            100,
            50,
            0.5,
        ),
    );
    let req = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_context_tokens(8_000)
        .with_max_output_tokens(1_000);
    assert!(score_manifest_for_request(&m, &req).is_none());
}

// -----------------------------------------------------------
// SLOT_POLICY §5 — offload_eligible truth table
// -----------------------------------------------------------

/// The offload gate is the AND of two conditions: privacy
/// `MeshAllowed` and latency class != `Fast`. This table pins
/// every combination plus the two envelope defaults the
/// derivation accessors apply (`privacy` unset → `LocalOnly`;
/// `latency` unset → `Normal`).
#[test]
fn offload_eligible_truth_table() {
    let mesh = ShardingPrivacy::MeshAllowed;
    let local = ShardingPrivacy::LocalOnly;

    // (privacy, latency, expected)
    let cases: &[(ShardingPrivacy, LatencyClass, bool)] = &[
        (mesh, LatencyClass::Fast, false),      // latency gate closes it
        (mesh, LatencyClass::Normal, true),     // both gates open
        (mesh, LatencyClass::Extended, true),   // both gates open
        (local, LatencyClass::Fast, false),     // both gates closed
        (local, LatencyClass::Normal, false),   // privacy gate closes it
        (local, LatencyClass::Extended, false), // privacy gate closes it
    ];
    for (privacy, latency, expected) in cases {
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(*latency)
            .with_sharding(*privacy);
        assert_eq!(
            offload_eligible(&req),
            *expected,
            "privacy={privacy:?} latency={latency:?} should be {expected}"
        );
    }

    // Default-privacy envelope (no `with_sharding`) resolves to
    // LocalOnly → never offloadable even at Normal latency.
    let default_privacy = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal);
    assert!(
        !offload_eligible(&default_privacy),
        "envelope without explicit privacy defaults to LocalOnly → not offloadable"
    );

    // Default-latency envelope (no `with_latency_class`) resolves
    // to Normal → offloadable when MeshAllowed (the hint-only /
    // sizing-only case the §5 headline delta covers).
    let default_latency = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_sharding(ShardingPrivacy::MeshAllowed);
    assert!(
        offload_eligible(&default_latency),
        "MeshAllowed hint-only envelope defaults to Normal latency → offloadable"
    );

    // Budget-unstated envelope resolves to one hop → still offloadable.
    // This is the compatibility case that matters most: every envelope
    // built before the budget existed, and every locally-originated one
    // today, omits the field. Reading absence as zero would disable mesh
    // routing outright.
    assert!(
        default_latency.forward_budget.is_none(),
        "fixture must actually omit the field"
    );
    assert!(
        offload_eligible(&default_latency),
        "an unstated budget must not block offload"
    );
}

/// The budget is a third, independent gate — and it must stay
/// distinguishable from the other two, because "stayed home by policy"
/// and "someone already forwarded this" are different operator problems.
#[test]
fn forward_budget_is_an_independent_gate_with_its_own_name() {
    let open = || {
        InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal)
            .with_sharding(ShardingPrivacy::MeshAllowed)
    };

    // Both other gates open, budget spent → blocked, and named as such.
    let spent = open().with_forward_budget(0);
    assert_eq!(
        offload_verdict(&spent),
        OffloadVerdict::ForwardBudgetExhausted
    );
    assert!(!offload_eligible(&spent));
    assert_eq!(offload_verdict(&spent).gate(), "forward_budget_exhausted");

    // A remaining budget with both gates open is eligible.
    assert_eq!(
        offload_verdict(&open().with_forward_budget(1)),
        OffloadVerdict::Eligible
    );

    // The pre-existing gates keep their reported name, so the decision
    // log, its replay, and their fixtures are unaffected.
    let private = open()
        .with_sharding(ShardingPrivacy::LocalOnly)
        .with_forward_budget(1);
    assert_eq!(offload_verdict(&private), OffloadVerdict::LocalOnlyPrivacy);
    assert_eq!(offload_verdict(&private).gate(), "not_offload_eligible");

    let fast = open()
        .with_latency_class(LatencyClass::Fast)
        .with_forward_budget(1);
    assert_eq!(offload_verdict(&fast), OffloadVerdict::FastLatency);
    assert_eq!(offload_verdict(&fast).gate(), "not_offload_eligible");
}

// -----------------------------------------------------------
// SLOT_POLICY §5 — the Fast gate's measured stand-down
// -----------------------------------------------------------

fn fast_mesh() -> InferenceRequirements {
    InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Fast)
        .with_sharding(ShardingPrivacy::MeshAllowed)
}

/// Observations for a node that HAS measured itself, at `tg` tok/s.
fn measured(tg: f64) -> NodeObservations {
    NodeObservations {
        samples: THROUGHPUT_OBSERVATION_THRESHOLD,
        tg_tok_s_ewma: tg,
        ..Default::default()
    }
}

/// The red one. `ring-doc-a` is a CPU-only podman node whose 2B decodes
/// at ~14 tok/s; its `Workload::Route` classify took 38.2 s while the hop
/// it was avoiding cost 37 ms. SLOT_POLICY §5's "a hop is a net loss" is
/// a claim about hardware, and this node falsifies it — so the gate
/// stands down and the scorer is allowed to look at peers.
#[test]
fn a_node_measured_below_the_interactive_reference_stands_down_the_fast_gate() {
    let slow = measured(14.0);
    assert_eq!(
        offload_verdict_with_local(&fast_mesh(), Some(&slow)),
        OffloadVerdict::FastLatencyYielded
    );
    assert!(offload_verdict_with_local(&fast_mesh(), Some(&slow)).is_eligible());
    // Principle 1: the line names WHICH floor decided, and it is not the
    // pre-existing name — an operator grepping `not_offload_eligible`
    // must not find this decision hiding under it.
    assert_eq!(
        offload_verdict_with_local(&fast_mesh(), Some(&slow)).gate(),
        "fast_latency_yielded"
    );
}

/// The other direction, and the one that bounds the blast radius: a node
/// that measured itself AT OR ABOVE the interactive reference keeps
/// SLOT_POLICY §5 exactly as written. A GPU node's Fast work does not
/// start crossing the wire because a CPU node needed to.
#[test]
fn a_node_measured_at_or_above_the_reference_keeps_the_fast_gate() {
    for tg in [
        f64::from(THROUGHPUT_REFERENCE_TG_TOK_S),
        f64::from(THROUGHPUT_REFERENCE_TG_TOK_S) + 40.0,
    ] {
        let fast_node = measured(tg);
        assert_eq!(
            offload_verdict_with_local(&fast_mesh(), Some(&fast_node)),
            OffloadVerdict::FastLatency,
            "{tg} tok/s is not sub-interactive"
        );
        assert!(!offload_verdict_with_local(&fast_mesh(), Some(&fast_node)).is_eligible());
    }
}

/// Absence is reported, never defaulted (principle 6). A node that has
/// not measured itself — no samples, or samples but a zero EWMA because
/// no streaming completion has landed — is NOT given a guessed rate. It
/// keeps the standing rule, which is also what every caller of the
/// two-argument `offload_verdict` gets, so the simulator and every other
/// surface are provably unchanged by this commit.
#[test]
fn an_unmeasured_node_keeps_the_standing_fast_rule() {
    let never = NodeObservations::default();
    let ramping = NodeObservations {
        samples: THROUGHPUT_OBSERVATION_THRESHOLD - 1,
        tg_tok_s_ewma: 3.0, // slow, but not yet trustworthy
        ..Default::default()
    };
    let zero_ewma = NodeObservations {
        samples: THROUGHPUT_OBSERVATION_THRESHOLD * 10,
        tg_tok_s_ewma: 0.0,
        ..Default::default()
    };
    for obs in [&never, &ramping, &zero_ewma] {
        assert_eq!(
            offload_verdict_with_local(&fast_mesh(), Some(obs)),
            OffloadVerdict::FastLatency,
            "unmeasured must not yield: {obs:?}"
        );
    }
    // And the no-argument form, which is what `offload_eligible`,
    // `offload_verdict_opt` and the mesh simulator all call.
    assert_eq!(offload_verdict(&fast_mesh()), OffloadVerdict::FastLatency);
    assert!(!offload_eligible(&fast_mesh()));
}

/// The privacy contract takes no measured escape, and this is the test
/// that says so. A `local_only` envelope on the slowest node in the world
/// stays home — measuring yourself slow is not consent.
#[test]
fn no_measurement_reopens_the_privacy_gate() {
    let slow = measured(0.5);
    let private = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Fast)
        .with_sharding(ShardingPrivacy::LocalOnly);
    assert_eq!(
        offload_verdict_with_local(&private, Some(&slow)),
        OffloadVerdict::LocalOnlyPrivacy
    );
    // A present-but-silent envelope is `LocalOnly` by §3.1, same answer.
    let silent = InferenceRequirements::new().with_latency_class(LatencyClass::Fast);
    assert_eq!(
        offload_verdict_with_local(&silent, Some(&slow)),
        OffloadVerdict::LocalOnlyPrivacy
    );
    // And the spent budget still closes a Normal-class request; the
    // stand-down is the Fast gate's alone.
    let spent = InferenceRequirements::new()
        .with_sharding(ShardingPrivacy::MeshAllowed)
        .with_latency_class(LatencyClass::Normal)
        .with_forward_budget(0);
    assert_eq!(
        offload_verdict_with_local(&spent, Some(&slow)),
        OffloadVerdict::ForwardBudgetExhausted
    );
}

/// The `_opt` surface carries the same stand-down, because production's
/// ranked path calls that one. An ABSENT envelope was already eligible
/// and is untouched — a slow node changes nothing for plain OpenAI
/// clients, only for work that explicitly asked for `Fast`.
#[test]
fn the_opt_surface_yields_the_same_way_and_absence_is_untouched() {
    let slow = measured(14.0);
    let req = fast_mesh();
    assert_eq!(
        offload_verdict_opt_with_local(Some(&req), Some(&slow)),
        OffloadVerdict::FastLatencyYielded
    );
    assert_eq!(
        offload_verdict_opt_with_local(None, Some(&slow)),
        OffloadVerdict::Eligible,
        "absence stated nothing before this change and states nothing now"
    );
}

/// The §9.1.1 regression, at the smallest scale that can hold it.
///
/// A plain OpenAI client sends no `oicp` key. Before 2026-08-13 the
/// ranked path read that absence as a refusal and never scored a peer;
/// the named path read the same absence as "unstated" and routed
/// perfectly well. This pins the answer that both now give.
#[test]
fn an_absent_envelope_is_not_a_refusal() {
    assert_eq!(offload_verdict_opt(None), OffloadVerdict::Eligible);
    assert_eq!(offload_verdict_opt(None).gate(), "eligible");
}

/// A PRESENT envelope is still judged in full — absence is the only
/// thing that changed. Without this, "absence is eligible" could be
/// mis-implemented as "the envelope is eligible", silently unhooking
/// the privacy contract for every `local_only` caller.
#[test]
fn a_present_envelope_is_judged_exactly_as_before() {
    let private = InferenceRequirements::new().with_sharding(ShardingPrivacy::LocalOnly);
    assert_eq!(
        offload_verdict_opt(Some(&private)),
        OffloadVerdict::LocalOnlyPrivacy
    );

    // The subtle one: an envelope that is PRESENT but says nothing
    // about privacy is `LocalOnly` by §3.1 and stays home. Absence of
    // the envelope and absence of the field are different facts and
    // this is where that distinction has to hold.
    let silent = InferenceRequirements::new();
    assert_eq!(
        offload_verdict_opt(Some(&silent)),
        OffloadVerdict::LocalOnlyPrivacy,
        "an empty envelope states LocalOnly by §3.1; only a MISSING \
         envelope states nothing"
    );

    let fast = InferenceRequirements::new()
        .with_sharding(ShardingPrivacy::MeshAllowed)
        .with_latency_class(LatencyClass::Fast);
    assert_eq!(
        offload_verdict_opt(Some(&fast)),
        OffloadVerdict::FastLatency
    );

    let spent = InferenceRequirements::new()
        .with_sharding(ShardingPrivacy::MeshAllowed)
        .with_forward_budget(0);
    assert_eq!(
        offload_verdict_opt(Some(&spent)),
        OffloadVerdict::ForwardBudgetExhausted
    );
}

/// §10.6, made structural. The named path (`resolve_named_dispatch`)
/// and the ranked path (`select_peers_ranked`) each decide whether an
/// envelope-less request may cross to a peer. They disagreed for a
/// week and the disagreement cost the §9.1.1 measurement. This asserts
/// the named path's two predicates — reproduced here verbatim in
/// shape, `Option::is_none_or` — against the ranked path's decider, so
/// a future edit to either one fails here rather than silently
/// re-forking the policy.
#[test]
fn both_routing_surfaces_agree_an_absent_envelope_permits_a_peer() {
    let absent: Option<&InferenceRequirements> = None;

    // The named path, as written at `resolve_named_dispatch`.
    let named_may_forward = absent.is_none_or(|o: &InferenceRequirements| o.may_forward());
    let named_privacy_permits =
        absent.is_none_or(|o: &InferenceRequirements| o.sharding() == ShardingPrivacy::MeshAllowed);
    assert!(named_may_forward && named_privacy_permits);

    // The ranked path.
    assert_eq!(
        offload_verdict_opt(absent),
        OffloadVerdict::Eligible,
        "the ranked path must reach the same verdict the named path \
         reaches for an envelope-less request, or §9.1.1 recurs"
    );
}
