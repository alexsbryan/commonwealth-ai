// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ContributionEmitter` origin-stamping under concurrent load.
//!
//! The §10 ledger invariant says every recorded event carries
//! `node_id = self.self_node_id` regardless of which caller
//! triggered the emission. Under concurrent traffic from many
//! peers (a real Founder serving simultaneous fan-out requests
//! from a mesh of 5+ joiners), the daemon's `ContributionEmitter`
//! must:
//!
//! 1. Stamp **every** event with the local node id as origin
//!    (the `LedgerEvent.node_id` field, NOT to be confused with
//!    the per-variant `for_node` / `from_node` payloads).
//! 2. Persist **every** event without loss — `unique_key` uses
//!    `(node_id, secs, nanos, atomic_seq)` so concurrent writes
//!    can't collide.
//! 3. Round-trip each event back through `events()` such that
//!    serde reads the same `node_id` and `for_node` it wrote.
//!
//! Pre-fix coverage was unit-level only (single-threaded
//! `record()` → `events()` calls). What's not pinned: many
//! concurrent emit calls coming through real HTTP routes, with
//! varied `for_node` values from distinct requesters. A regression
//! that:
//!
//!   - stamped `for_node` into `node_id` (the inverse of the
//!     fan-out X-Node-Id fix — different field, same risk),
//!   - dropped events under contention (race in `unique_key`
//!     suffix or in the MeshStore set path),
//!   - or serialised the wrong field on round-trip,
//!
//! would slip past every unit test but corrupt the dimensional
//! ledger in production. Caught here.
use std::sync::Arc;

use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::NodeId;
use commonwealth_state::MeshStore;
use corpus_engine::CorpusEngine;
use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::types::EmbedFn;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;
use sovereign_meshapp_registry::registry::AppRegistry;

use crate::common;
use crate::common::{id_to_hex, solo_mesh, spawn_router};

const EMBED_DIM: usize = 8;
const N_REQUESTERS: u64 = 50;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

async fn install_corpus(indexes_dir: &std::path::Path, id: &str) {
    let path = indexes_dir.join(id);
    let index = CorpusIndex::create(
        &path,
        id,
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
                title: Some(id.into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(id.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; EMBED_DIM],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_serves_stamp_origin_as_self_for_every_event() {
    // Multi-threaded runtime so concurrent reqwest calls can
    // genuinely race. Single-threaded would serialise them and
    // mask any race the daemon might have.

    let self_id = NodeId::from_u128(0xCAFE_BABE_CAFE_BABE);

    // Real CorpusEngine + one corpus so every request returns
    // exactly one chunk → exactly one KnowledgeQueryServed event
    // per request. Cleaner accounting than "some events".
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install_corpus(&indexes, "sep").await;
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state = AppState::new_with_platform_and_engine(
        self_id,
        solo_mesh(self_id, "origin-test"),
        mesh_store,
        app_registry,
        Some(engine),
    );
    let addr = spawn_router(internal_router(state.clone())).await;

    // Generate N distinct requester NodeIds. Using a deterministic
    // sequence so the post-assertion can match for_node values
    // back to the original set.
    let requesters: Vec<NodeId> = (1..=N_REQUESTERS)
        .map(|i| NodeId::from_u128(0xBBBB_0000_0000_0000 + i as u128))
        .collect();

    // Each requester is a MEMBER whose key this node's roster names. A typed
    // `X-Node-Id` is a claim, not an identity: the internal router strips it
    // and the caller is attributed to nobody, so a test that wants N distinct
    // `for_node` values has to present N distinct VERIFIED keys.
    for (i, requester) in requesters.iter().enumerate() {
        common::name_member_with_key(&state, *requester, "requester", requester_key(i)).await;
    }

    // Fire N concurrent requests. `tokio::join!` won't scale to 50;
    // spawn each into a task and join.
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/internal/knowledge/search");
    let mut handles = Vec::with_capacity(requesters.len());
    for (i, requester) in requesters.iter().enumerate() {
        let client = client.clone();
        let url = url.clone();
        let requester = *requester;
        let key = requester_key(i);
        handles.push(tokio::spawn(async move {
            common::acceptor_stamp(client.post(&url), "requester", requester, key)
                .json(&serde_json::json!({
                    "query_embedding": vec![0.0_f32; EMBED_DIM],
                    "query_text": "content",
                    "corpora": ["sep"],
                    "limit": 10,
                }))
                .send()
                .await
                .map(|r| r.status())
        }));
    }

    // Collect outcomes. Every request should have succeeded — the
    // route is local + the corpus is installed, no flakiness
    // source.
    let mut succeeded = 0usize;
    for h in handles {
        match h.await {
            Ok(Ok(status)) if status == reqwest::StatusCode::OK => succeeded += 1,
            Ok(Ok(status)) => panic!("unexpected status: {status}"),
            Ok(Err(e)) => panic!("request error: {e}"),
            Err(e) => panic!("join error: {e}"),
        }
    }
    assert_eq!(
        succeeded as u64, N_REQUESTERS,
        "all {N_REQUESTERS} concurrent requests must complete with 200"
    );

    // Inspect the ledger.
    let events = state
        .inner
        .fabric
        .contribution_emitter
        .events()
        .expect("emitter.events() reads");
    let served: Vec<(NodeId, NodeId, String)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::KnowledgeQueryServed {
                for_node,
                corpus_id,
                ..
            } => Some((e.node_id, *for_node, corpus_id.clone())),
            _ => None,
        })
        .collect();

    // Assertion 1: no events lost. The unique_key suffix should
    // prevent collisions even under sub-nanosecond contention.
    assert_eq!(
        served.len() as u64,
        N_REQUESTERS,
        "{} concurrent requests must produce {} events; got {}. \
         A count below N means events collided on the MeshStore key \
         and clobbered each other.",
        N_REQUESTERS,
        N_REQUESTERS,
        served.len()
    );

    // Assertion 2: origin is always self. THE invariant this test
    // exists for — `node_id` (origin) is `self`, `for_node` is the
    // requester. A regression that flipped them would corrupt every
    // peer's dimensional view of who served what.
    for (origin, for_node, corpus_id) in &served {
        assert_eq!(
            origin, &self_id,
            "every emitted event MUST stamp `node_id = self` as origin; \
             found origin={origin:?} on an event where for_node={for_node:?}. \
             If origin and for_node ever swap, peer aggregation rolls \
             credit to the wrong node."
        );
        assert_eq!(corpus_id, "sep", "corpus_id round-trips correctly");
    }

    // Assertion 3: every requester appears exactly once as for_node.
    // If two events shared a for_node, one request would have been
    // double-counted or another lost — either way, accounting drift.
    let mut for_nodes: Vec<NodeId> = served.iter().map(|(_, fn_, _)| *fn_).collect();
    for_nodes.sort_by_key(|n| n.as_bytes().to_vec());
    let mut expected: Vec<NodeId> = requesters.clone();
    expected.sort_by_key(|n| n.as_bytes().to_vec());
    assert_eq!(
        for_nodes, expected,
        "every requester must appear exactly once as for_node — duplicates \
         or omissions mean the emit path lost track under concurrency"
    );
}

/// One distinct verified key per requester, derived from its index — the
/// roster tells two requesters apart by key, so the keys must differ.
/// The key the header-swap test's self-member is verified by.
const SELF_KEY: [u8; 32] = [0xc3; 32];

fn requester_key(i: usize) -> [u8; 32] {
    let mut k = [0x5a; 32];
    k[0] = i as u8;
    k[1] = (i >> 8) as u8;
    k
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn origin_unaffected_by_a_requester_claiming_to_be_us() {
    // Tighter invariant: even when the VERIFIED requester is a member whose
    // node id is our own self_id (a daemon that did not realise it was
    // talking to itself), the recorded origin must still be self — the
    // requester drives `for_node`, never `node_id`. The aggregation path
    // (`current_contributions`) groups by `node_id` to attribute "who
    // served"; a requester that polluted origin would let an external caller
    // masquerade as a different serving node in our local view.
    let self_id = NodeId::from_u128(0xDEADBEEF_DEADBEEF);

    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install_corpus(&indexes, "sep").await;
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let state = AppState::new_with_platform_and_engine(
        self_id,
        solo_mesh(self_id, "origin-swap-test"),
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        Some(engine),
    );
    let addr = spawn_router(internal_router(state.clone())).await;

    // The requester is a verified member whose node id IS our own self_id.
    // The route treats it as "a peer that is us" — no check that it differs —
    // so the emission still fires, and origin stays self.
    common::name_member_with_key(&state, self_id, "self", SELF_KEY).await;
    let resp = common::acceptor_stamp(
        reqwest::Client::new().post(format!("http://{addr}/internal/knowledge/search")),
        "self",
        self_id,
        SELF_KEY,
    )
    .json(&serde_json::json!({
        "query_embedding": vec![0.0_f32; EMBED_DIM],
        "query_text": "content",
        "corpora": ["sep"],
        "limit": 10,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let events = state.inner.fabric.contribution_emitter.events().unwrap();
    let served: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, LedgerEventKind::KnowledgeQueryServed { .. }))
        .collect();
    assert_eq!(
        served.len(),
        1,
        "a verified requester that happens to be us still produces one event"
    );

    // The event's origin (`node_id`) is always self.
    assert_eq!(
        served[0].node_id, self_id,
        "origin MUST be self regardless of header content"
    );

    // for_node reflects the VERIFIED requester — the attribution path works,
    // and the origin is independent of it.
    if let LedgerEventKind::KnowledgeQueryServed { for_node, .. } = &served[0].kind {
        assert_eq!(
            for_node, &self_id,
            "for_node is the verified requester; a regression that conflated \
             origin and for_node would lose this distinction"
        );
    }
}
