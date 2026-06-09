// SPDX-License-Identifier: AGPL-3.0-or-later
//! Proves that a live `EmbeddedDaemon` with a real `CorpusEngine`
//! publishes its `hosted_corpora` to its own `MemberRecord` on the
//! first gossip round. This is the mechanism by which the Founder's
//! SEP corpus becomes visible to the Joiner — without it, gossip
//! converges the membership but every peer advertises
//! `hosted_corpora: vec![]`, defeating the whole knowledge fan-out.
use std::sync::Arc;

use commonwealth_api::state::AppState;
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_mesh::gossip;
use std::collections::HashMap;
use std::time::Duration;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) }))
}

async fn make_engine_with_sep(dir: &std::path::Path) -> Arc<CorpusEngine> {
    let indexes = dir.join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let idx_path = indexes.join("sep");
    let index = CorpusIndex::create(
        &idx_path,
        "sep",
        "Stanford Encyclopedia of Philosophy",
        "qwen3-embedding-0.6b",
        8,
        true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: "Compatibilism is the thesis that free will is \
                          compatible with determinism."
                    .into(),
                title: Some("Compatibilism".into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some("compat".into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; 8],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
    Arc::new(
        CorpusEngine::new(dir.join("recipes"), indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    )
}

fn empty_node_capabilities() -> NodeCapabilities {
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
        inference_capable: false,
        loaded_models: vec![],

        embed_model: None,
        benchmark: None,
        current_in_flight: None,
    }
}

#[tokio::test]
async fn gossip_round_publishes_live_hosted_corpora() {
    // Build a real CorpusEngine containing a single "sep" chunk.
    let dir = tempfile::tempdir().unwrap();
    let engine = make_engine_with_sep(dir.path()).await;

    // Build an AppState whose member record starts with the empty
    // capabilities every constructor in Commonwealth hardcodes —
    // that's the "bug" we're proving the fix addresses.
    let self_id = NodeId::from_u128(1);
    let self_record = MemberRecord {
        node_id: self_id,
        name: "Host".into(),
        invited_by: self_id,
        joined_at: 0,
        last_seen: 100,
        status: NodeStatus::Online,
        capabilities: empty_node_capabilities(),
        addresses: vec!["127.0.0.1:9742".parse().unwrap()],
    };
    let mut members = HashMap::new();
    members.insert(self_id, self_record);
    let mesh = Mesh {
        id: MeshId::from_u128(42),
        name: "Test".into(),
        join_key_hash: [1u8; 32],
        members,
        peers: vec![],
    };

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state = AppState::new_with_platform_and_engine(
        self_id,
        mesh,
        mesh_store,
        app_registry,
        Some(Arc::clone(&engine)),
    );

    // Sanity pre-condition: before the round, `hosted_corpora` is empty
    // (matching every constructor in the Commonwealth tree today).
    {
        let m = state.inner.mesh.read().await;
        assert!(m
            .members
            .get(&self_id)
            .unwrap()
            .capabilities
            .hosted_corpora
            .is_empty());
    }

    // One round. No peers, so there's nothing to gossip WITH, but
    // the "refresh self capabilities" path runs regardless — it
    // doesn't depend on a peer being reachable.
    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("gossip round must succeed even with no peers");

    let m = state.inner.mesh.read().await;
    let caps = &m.members.get(&self_id).unwrap().capabilities;
    assert_eq!(
        caps.hosted_corpora.len(),
        1,
        "gossip should have republished live hosted_corpora"
    );
    assert_eq!(
        caps.hosted_corpora[0].corpus_id, "sep",
        "the SEP corpus must appear by id"
    );
    // And the hardware profile should have been replaced with
    // live-detected values — specifically system_ram_gb > 0 on any
    // real machine. Zeros here would indicate the publisher silently
    // failed.
    assert!(
        caps.hardware.system_ram_gb > 0,
        "hardware should have been detected: {:?}",
        caps.hardware
    );
}
