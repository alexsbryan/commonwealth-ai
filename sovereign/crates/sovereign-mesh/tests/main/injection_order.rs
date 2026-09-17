// SPDX-License-Identifier: AGPL-3.0-or-later
//! The service-injection hazards that used to be *remembered* are now
//! structural.
//!
//! `AppState::with_local_inference` and `with_rpc_shard_warmer` mutated
//! `AppStateInner` through `Arc::get_mut`, which returns `None` the moment any
//! other code has cloned `app_state.inner` — the installer then became a
//! `tracing::error!` and a quiet return, NOT a panic or an error result, so the
//! daemon booted with no local inference and 503'd every chat turn.
//!
//! Both values are constructor arguments now (`ServingSeed::local_inference`,
//! `ServingSeed::rpc_shard_warmer`; DC §4.2 "Construction is staged, and parts
//! are total"), as is the mesh-mutation hook (`FabricSeed::mesh_mutation_hook`).
//! The hazard is gone structurally (ARCH 10): there is no installer to
//! re-order, so these tests assert the values are present the moment the state
//! exists rather than capturing a log line that a bad ordering would emit.
use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_api::state::{AppState, LocalInferenceService, ServingSeed};
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;
use sovereign_daemon::slot_manifest::CoreSlotManifest;
use sovereign_meshapp_registry::registry::AppRegistry;

use crate::common::TestProvider;

fn empty_mesh() -> Mesh {
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "injection-test".into(),
        invite_key_hash: [9u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    }
}

#[test]
fn local_inference_is_present_at_construction() {
    // The provider used to ride the `Arc::get_mut` installer; it is a
    // constructor argument now, so a future refactor cannot re-order a clone
    // ahead of it (ARCH 10 — structural, not remembered).
    let provider: Arc<dyn InferenceProvider> = Arc::new(TestProvider::new());
    let adapter: Arc<dyn LocalInferenceService> = Arc::new(SovereignInferenceAdapter::new(
        provider,
        Arc::new(CoreSlotManifest),
    ));
    let app_state = AppState::new_with_platform_and_engine_and_serving(
        NodeId::from_u128(0xDEAD_BEEF_CAFE_F00D),
        empty_mesh(),
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
    );

    assert!(
        app_state.inner.serving.local_inference.is_some(),
        "a provider passed at construction must be present"
    );
}

#[test]
fn mesh_mutation_hook_is_present_at_construction() {
    // Sister assertion — the second value that used to ride the same
    // `Arc::get_mut` contract. It is a constructor argument now, so a future
    // refactor cannot re-order a clone ahead of it: the hook is present the
    // moment the state exists (ARCH 10 — structural, not remembered).
    let hook: sovereign_api::state::MeshMutationHook =
        Arc::new(|_mesh: &Mesh, _self_id: NodeId| {
            // Body intentionally empty — the test isn't about firing the
            // hook, only about it surviving construction.
        });
    let app_state = AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        NodeId::from_u128(0xDEAD_BEEF_CAFE_F00D),
        empty_mesh(),
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
        None,
        sovereign_api::state::FabricSeed {
            mesh_mutation_hook: Some(hook),
            ..Default::default()
        },
    );

    assert!(
        app_state.inner.fabric.on_mesh_mutation.is_some(),
        "a hook passed at construction must be present"
    );
}
