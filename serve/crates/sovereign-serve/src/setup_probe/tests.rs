// SPDX-License-Identifier: AGPL-3.0-or-later
//! The probe's plan answer (moved from cli-daemon's json_surface_tests with
//! `build_plan`, pb-distribution-setup).
use super::*;
use sovereign_contracts::daemon_wire::ProfileName;

fn hw(effective_gb: f32) -> HardwareProfile {
    HardwareProfile {
        system_ram_bytes: (effective_gb * 1_073_741_824.0) as u64,
        gpu_available: effective_gb > 0.0,
        gpu_name: Some("test".into()),
        gpu_memory_bytes: None,
        recommended_gpu_layers: 999,
        is_unified_memory: true,
    }
}

/// The plan a client reads is the plan the wizard would act on: it
/// serializes and parses back into the CONTRACTS types, with the tier,
/// the recommended row and both single-pick slots intact.
///
/// Watched fail: change `ProfileName`'s serde spelling away from
/// `as_str` and the profile assertion goes red; drop `Serialize` from
/// `SlotConfig` and it does not compile.
#[test]
fn the_plan_round_trips_through_the_contracts_types() {
    let plan = build_plan(hw(32.0));
    let json = serde_json::to_string(&plan).expect("plan serializes");
    let back: SetupPlan = serde_json::from_str(&json).expect("plan parses back");

    assert_eq!(back.profile, ProfileName::VeryHigh);
    assert_eq!(back.profile.as_str(), "very_high");
    assert!(
        json.contains(r#""profile":"very_high""#),
        "the wire spells the tier the way models.toml does: {json}"
    );
    assert!(!back.catalog.is_empty(), "a very_high tier has candidates");
    assert_eq!(
        back.catalog.iter().filter(|o| o.recommended).count(),
        1,
        "exactly one row is the pick"
    );
    let fast = back.fast.expect("very_high defines a fast slot");
    let embed = back.embed.expect("very_high defines an embed slot");
    assert!(fast.file.ends_with(".gguf"), "fast: {}", fast.file);
    assert!(embed.file.ends_with(".gguf"), "embed: {}", embed.file);
    assert_eq!(
        back.hardware.system_ram_bytes,
        plan.hardware.system_ram_bytes
    );
}

/// `--plan` is a READ. A cpu_only machine still gets a catalog rather
/// than a refusal, and nothing about the answer depends on a config
/// existing — which is the whole reason the flag exists.
#[test]
fn a_machine_with_no_gpu_still_has_a_plan() {
    let plan = build_plan(hw(0.0));
    assert_eq!(plan.profile, ProfileName::CpuOnly);
    assert!(!plan.catalog.is_empty());
}

/// A typo'd rung is refused before anything is fetched, echoing the input and
/// listing the ladder (it was `parse_args`' refusal until the ladder moved
/// behind the probe). Watched fail: fall back to the profile's rung on a miss
/// and this returns a plan for a model nobody asked for.
#[test]
fn an_unknown_rung_is_refused_naming_the_ladder() {
    let err = fim_plan(Some("q3_k_s".into()), hw(32.0)).unwrap_err();
    assert!(err.contains("q3_k_s"), "echoes the input: {err}");
    assert!(err.contains("q6_k"), "lists the rungs: {err}");
}

/// The fim answer carries a URL for every slot it names, so `svrn setup
/// --fim` fetches without a planner of its own.
#[test]
fn the_fim_plan_names_a_url_for_every_slot() {
    let plan = fim_plan(Some("q6_k".into()), hw(32.0)).expect("q6_k is on the ladder");
    assert_eq!(plan.rung, "q6_k");
    let named = std::iter::once(&plan.slot)
        .chain(plan.embed.iter())
        .chain(plan.next.iter().map(|(_, s)| s));
    for slot in named {
        assert!(
            plan.urls.contains_key(&slot.file),
            "{} has no url",
            slot.file
        );
    }
}

/// A question the probe does not know exits 2, never 0 with nothing printed.
#[test]
fn an_unknown_question_exits_two() {
    assert_eq!(run(&["weights".to_string()]), 2);
    assert_eq!(run(&[]), 2);
}
