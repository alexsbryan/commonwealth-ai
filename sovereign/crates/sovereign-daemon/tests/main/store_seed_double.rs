// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store seed is the seam a backing enters `AppState` through
//! (five-programs fp-97): an AppState built by `AppState::new_with_seeds` over
//! the recording double routes a served knowledge query's contribution write
//! to the double, not to any `MeshStore`.
use std::sync::Arc;

use commonwealth_core::ids::NodeId;
use corpus_engine::CorpusEngine;
use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::types::EmbedFn;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::{fabric, node, serving, store::StoreSeed, AppState};

use crate::common;
use crate::common::ledger_double::RecordingLedger;
use crate::common::{solo_mesh, spawn_router};

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

#[tokio::test]
async fn served_knowledge_query_records_through_the_store_seed() {
    let self_id = NodeId::from_u128(0x5EED_5EED);
    let requester = NodeId::from_u128(0xBBBB_0001);
    let requester_key = [0x5b; 32];

    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    let index = CorpusIndex::create(
        &indexes.join("sep"),
        "sep",
        "Test Corpus",
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: "Some content the search will return.".into(),
                title: Some("sep".into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some("sep".into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; EMBED_DIM],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let double = Arc::new(RecordingLedger::new(self_id));
    let state = AppState::new_with_seeds(
        self_id,
        solo_mesh(self_id, "store-seed"),
        Some(engine),
        None,
        fabric::FabricSeed::default(),
        serving::ServingSeed::default(),
        node::NodeSeed::default(),
        StoreSeed {
            kv: double.clone(),
            contributions: double.clone(),
            activity: double.clone(),
            peer_preferences: double.clone(),
            inference: double.clone(),
            processed_shards: double.clone(),
        },
    );
    common::name_member_with_key(&state, requester, "requester", requester_key).await;
    let addr = spawn_router(internal_router(state)).await;

    let status = common::acceptor_stamp(
        reqwest::Client::new().post(format!("http://{addr}/internal/knowledge/search")),
        "requester",
        requester,
        requester_key,
    )
    .json(&serde_json::json!({
        "query_embedding": vec![0.0_f32; EMBED_DIM],
        "query_text": "content",
        "corpora": ["sep"],
        "limit": 10,
    }))
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(status, reqwest::StatusCode::OK);

    let recorded: Vec<_> = double
        .calls()
        .into_iter()
        .filter(|c| c.method == "contributions.record")
        .collect();
    assert_eq!(
        recorded.len(),
        1,
        "one served corpus, one record: {recorded:?}"
    );
    assert!(
        recorded[0].args.contains("KnowledgeQueryServed")
            && recorded[0].args.contains(&format!("{requester:?}"))
            && recorded[0].args.contains("\"sep\""),
        "the record names the kind, the requester and the corpus: {:?}",
        recorded[0].args
    );
}
