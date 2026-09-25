// SPDX-License-Identifier: AGPL-3.0-or-later
//! Each `LocalLedger` method answers what the commonwealth-state writer or
//! reader it wraps answers over the same store.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use commonwealth_core::activity::ActivityEventKind;
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::{ModelArchitecture, ModelInfo};
use commonwealth_core::oicp::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
use commonwealth_state::peer_preferences::PeerPreferenceStore;
use commonwealth_state::store_adapter::InferenceStateStore;
use commonwealth_state::{ActivityEmitter, ContributionEmitter, MeshStore};

use super::*;

const DAEMON: u128 = 0xDAE;

fn ledger() -> (MeshStore, LocalLedger) {
    let store = MeshStore::in_memory().unwrap();
    let ledger = LocalLedger::new(Arc::new(store.clone()), NodeId::from_u128(DAEMON));
    (store, ledger)
}

fn json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap()
}

fn model(id: u128) -> ModelInfo {
    ModelInfo {
        id: ModelId::from_u128(id),
        name: "m".into(),
        repo: "test/m".into(),
        file: "m.gguf".into(),
        size_bytes: 1,
        total_layers: 1,
        architecture: ModelArchitecture::Qwen,
        available_on: HashMap::new(),
        oicp_capabilities: Default::default(),
        quantization: "Q4_K_M".into(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    }
}

#[tokio::test]
async fn contributions_answer_what_the_emitter_answers() {
    let (store, ledger) = ledger();
    assert_eq!(
        ContributionLedgerPort::self_node_id(&ledger),
        NodeId::from_u128(DAEMON)
    );
    let kind = LedgerEventKind::InferenceServed {
        for_node: NodeId::from_u128(7),
        model_id: "m".into(),
        tokens_generated: 12,
        wall_seconds: 0.5,
    };
    ContributionLedgerPort::record(&ledger, kind.clone())
        .await
        .unwrap();

    let events = ContributionLedgerPort::events(&ledger).await.unwrap();
    let expected = ContributionEmitter::new(store.clone(), NodeId::from_u128(0))
        .events()
        .unwrap();
    assert_eq!(events, expected);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].node_id, NodeId::from_u128(DAEMON));
    assert_eq!(events[0].kind, kind);

    let current = ledger
        .current_contributions(&HashMap::new(), 30)
        .await
        .unwrap();
    assert_eq!(
        current,
        commonwealth_state::current_contributions(&store, &HashMap::new(), 30).unwrap()
    );
}

#[tokio::test]
async fn storage_snapshot_loop_emits_on_first_tick() {
    // Pinned cadence — first tick fires immediately when the
    // tokio interval is created, so a snapshot lands at boot
    // without waiting an hour. Pin this so a future tokio
    // change doesn't silently shift the boot-time snapshot
    // off the ledger.
    let (_store, ledger) = ledger();
    let port: Arc<dyn ContributionLedgerPort> = Arc::new(ledger.clone());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let walker = || async { vec![("wikipedia".to_string(), 12.5_f64)] };
    let handle = tokio::spawn(run_storage_snapshot_loop(
        port,
        walker,
        std::time::Duration::from_secs(3_600),
        shutdown_rx,
    ));
    // Real-time wait — first tick is immediate, so 50ms is
    // generous. Pinned at 3600s interval so the second tick is
    // an hour out (well after this test ends).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = shutdown_tx.send(true);
    let _ = handle.await;

    let events = ContributionLedgerPort::events(&ledger).await.unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            LedgerEventKind::StorageSnapshot { corpora }
                if corpora == &vec![("wikipedia".to_string(), 12.5)]
        )),
        "first tick must produce a StorageSnapshot, got {events:?}"
    );
}

#[tokio::test]
async fn storage_snapshot_loop_skips_emission_when_walker_returns_empty() {
    // Empty walker → no event. Lets a daemon without a corpus
    // engine wired up (or with no mesh-shared corpora) start
    // the loop unconditionally without polluting the ledger.
    let (_store, ledger) = ledger();
    let port: Arc<dyn ContributionLedgerPort> = Arc::new(ledger.clone());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let walker = || async { Vec::<(String, f64)>::new() };
    let handle = tokio::spawn(run_storage_snapshot_loop(
        port,
        walker,
        std::time::Duration::from_secs(3_600),
        shutdown_rx,
    ));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = shutdown_tx.send(true);
    let _ = handle.await;

    let events = ContributionLedgerPort::events(&ledger).await.unwrap();
    assert!(
        events.is_empty(),
        "no events expected when walker returns empty, got {events:?}"
    );
}

#[tokio::test]
async fn activity_answers_what_current_activity_answers() {
    let (store, ledger) = ledger();
    ActivityLedgerPort::record(
        &ledger,
        ActivityEventKind::LocalInferenceServed {
            model_id: "m".into(),
            prompt_tokens: 3,
            completion_tokens: 4,
            wall_seconds: 0.1,
        },
    )
    .await
    .unwrap();
    let summary = ledger.current_activity(7).await.unwrap();
    let expected = commonwealth_state::current_activity(&store, 7).unwrap();
    assert_eq!(summary, expected);
    assert_ne!(expected, Default::default(), "the write reached the store");

    let events = ActivityLedgerPort::events(&ledger).await.unwrap();
    let expected = ActivityEmitter::new(store, NodeId::from_u128(0))
        .events()
        .unwrap();
    assert_eq!(events, expected);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].node_id, NodeId::from_u128(DAEMON));
}

#[tokio::test]
async fn peer_preferences_answer_what_the_store_answers() {
    let (store, ledger) = ledger();
    let peer = NodeId::from_u128(9);
    let prefs = PeerPreferenceStore::new(store, NodeId::from_u128(DAEMON));
    prefs
        .set(
            &peer,
            PeerPreference::new(0.5, Some("slow".into())).unwrap(),
        )
        .unwrap();

    assert_eq!(
        PeerPreferencesPort::list(&ledger).await.unwrap(),
        prefs.list().unwrap()
    );
    assert_eq!(
        PeerPreferencesPort::get(&ledger, &peer).await.unwrap(),
        prefs.get(&peer).unwrap()
    );
    assert_eq!(
        PeerPreferencesPort::get(&ledger, &NodeId::from_u128(10))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn processed_shards_publish_under_the_key_and_join_the_union() {
    let (store, ledger) = ledger();
    assert!(ledger.publish("c", &[1, 3]).await.unwrap());
    assert!(
        !ledger.publish("c", &[1, 3]).await.unwrap(),
        "an unchanged value is not a change"
    );
    let key = commonwealth_state::processed_shards_key("c", NodeId::from_u128(DAEMON));
    let entry = store
        .get(commonwealth_state::PROCESSED_SHARDS_APP_ID, &key)
        .unwrap()
        .expect("published under processed_shards_key");
    assert_eq!(entry.origin, NodeId::from_u128(DAEMON));

    let union = ledger.union("c").await.unwrap();
    assert_eq!(
        union,
        commonwealth_state::union_processed_shards(&store, "c")
    );
    assert_eq!(union, BTreeSet::from([1, 3]));
}

#[tokio::test]
async fn inference_state_answers_what_the_store_adapter_answers() {
    let (store, ledger) = ledger();
    let reader = InferenceStateStore::new(Arc::new(store), NodeId::from_u128(DAEMON));

    // `InferencePlan` and `ModelInfo` carry no `PartialEq`; their wire
    // form is what the port's two implementations must agree on.
    assert_eq!(
        json(&ledger.get_plan().await.unwrap()),
        json(&reader.get_plan())
    );
    ledger.set_plan(&InferencePlan::default()).await.unwrap();
    assert_eq!(
        json(&ledger.get_plan().await.unwrap()),
        json(&reader.get_plan())
    );
    assert!(reader.get_plan().is_some());

    let m = model(1);
    ledger.set_model_info(&m).await.unwrap();
    assert_eq!(
        json(&ledger.get_model_info(m.id).await.unwrap()),
        json(&reader.get_model_info(m.id))
    );
    assert!(reader.get_model_info(m.id).is_some());
    assert_eq!(
        json(&ledger.list_models_with_origins().await.unwrap()),
        json(&reader.list_models_with_origins())
    );

    ledger.set_llama_address(m.id, "127.0.0.1:1").await.unwrap();
    assert_eq!(
        ledger.get_llama_address(m.id).await.unwrap(),
        reader.get_llama_address(m.id)
    );
    assert_eq!(
        reader.get_llama_address(m.id).as_deref(),
        Some("127.0.0.1:1")
    );

    assert!(ledger.remove_model_info(m.id).await.unwrap());
    assert!(reader.get_model_info(m.id).is_none());

    let embed = EmbedModelInfo {
        model_id: "e".into(),
        dimensions: 8,
        pooling: PoolingStrategy::Mean,
        normalization: NormalizationStrategy::Server,
        query_instruction_prefix: String::new(),
    };
    assert_eq!(ledger.get_local_embed_model().await.unwrap(), None);
    ledger.set_local_embed_model(&embed).await.unwrap();
    assert_eq!(
        ledger.get_local_embed_model().await.unwrap(),
        reader.get_local_embed_model()
    );
    assert_eq!(reader.get_local_embed_model(), Some(embed));
}
