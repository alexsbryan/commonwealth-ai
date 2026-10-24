// SPDX-License-Identifier: AGPL-3.0-or-later
//! The router retires the generate and embed compute-child roles.

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

/// THE FAILING INPUT: a `[compute]` pool naming the two roles the router now
/// serves itself. Before this, the daemon spawned a second copy of each
/// model in an embedded child beside the router's own.
#[test]
fn a_generate_or_embed_pool_is_refused_by_name_on_the_router() {
    let section = compute(&[("talker", "generate"), ("vectors", "embed")]);
    let refused = retired_slots(&EngineKind::LlamaServer, &section);
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

/// A role the router does not own still runs, so the retirement is the two
/// named roles and not `[compute]` itself.
#[test]
fn a_role_the_router_does_not_own_is_left_alone() {
    let section = compute(&[("crash-slot", "mock"), ("ranker", "rerank")]);
    assert!(retired_slots(&EngineKind::LlamaServer, &section).is_empty());
}

/// The embedded engine is untouched until the default flips: the same pool
/// is admitted there.
#[test]
fn the_embedded_engine_still_admits_both_roles() {
    let section = compute(&[("talker", "generate"), ("vectors", "embed")]);
    assert!(retired_slots(&EngineKind::Llama, &section).is_empty());
    assert!(retired_slots(&EngineKind::Remote, &section).is_empty());
}

/// A disabled `[compute]` spawns nothing, so nothing is retired and a
/// leftover `enabled = false` pool is not a startup failure.
#[test]
fn a_disabled_section_retires_nothing() {
    let mut section = compute(&[("talker", "generate")]);
    section.enabled = false;
    assert!(retired_slots(&EngineKind::LlamaServer, &section).is_empty());
}
