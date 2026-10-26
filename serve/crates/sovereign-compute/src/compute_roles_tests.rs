// SPDX-License-Identifier: AGPL-3.0-or-later
//! The generate and embed compute-child roles are retired with the embedded
//! engine (ENGINE_SWAP_PLAN G2).

use super::*;
use sovereign_contracts::setup_config::ComputeSlotConfig;

fn slot(name: &str, role: &str) -> ComputeSlotConfig {
    ComputeSlotConfig {
        name: name.into(),
        role: role.into(),
        model: "/m/some.gguf".into(),
        context_size: None,
        n_gpu_layers: None,
        warm: true,
        capture_embed: false,
    }
}

fn compute(slots: &[(&str, &str)]) -> ComputeSection {
    ComputeSection {
        enabled: true,
        slot: slots.iter().map(|(n, r)| slot(n, r)).collect(),
        ..Default::default()
    }
}

/// THE FAILING INPUT: a `[compute]` pool naming the two roles whose engine
/// is gone. Before the router, each spawned an embedded child; now there is
/// nothing to host them, so the boot refuses them by name.
#[test]
fn a_generate_or_embed_pool_is_refused_by_name() {
    let section = compute(&[("talker", "generate"), ("vectors", "embed")]);
    let refused = retired_slots(&section);
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(refused[0].starts_with("talker: "), "{refused:?}");
    assert!(
        refused[0].contains("router") && refused[0].contains("generate"),
        "the refusal names the role and the router: {refused:?}"
    );
    assert!(refused[1].starts_with("vectors: "), "{refused:?}");
    assert!(
        refused[1].contains("embed_path") && refused[1].contains("embed"),
        "an embed refusal names the key that replaces it: {refused:?}"
    );
}

/// A served kind or the mock still runs, so the retirement is the two named
/// roles and not `[compute]` itself.
#[test]
fn a_role_the_router_does_not_own_is_left_alone() {
    let section = compute(&[("crash-slot", "mock")]);
    assert!(retired_slots(&section).is_empty());
}

/// The child's own parse refuses a retired role with the startup check's
/// words, so a hand-typed `--compute-child --role generate` says why too.
#[test]
fn the_child_refusal_is_the_startup_refusal() {
    let why = retired("generate").expect("generate is retired");
    assert_eq!(why, retired_role("generate"));
    assert!(why.contains("embedded engine"), "{why}");
    assert!(retired("mock").is_none());
    assert!(retired("rerank").is_some_and(|why| why.contains("router")));
}

/// A disabled `[compute]` spawns nothing, so nothing is retired and a
/// leftover `enabled = false` pool is not a startup failure.
#[test]
fn a_disabled_section_retires_nothing() {
    let mut section = compute(&[("talker", "generate")]);
    section.enabled = false;
    assert!(retired_slots(&section).is_empty());
}
