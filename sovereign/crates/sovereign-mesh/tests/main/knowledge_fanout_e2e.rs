// SPDX-License-Identifier: AGPL-3.0-or-later
//! Two-daemon knowledge fan-out test.
//!
//! Exercises the "ask my mesh about X" path end-to-end:
//!
//! 1. Daemon A (the founder) installs a corpus and advertises it
//!    via `hosted_corpora` on its `MemberRecord`.
//! 2. Daemon B (the joiner) has no corpora locally but knows about
//!    A through its mesh state.
//! 3. A client POSTs `/v1/knowledge/search` to B.
//! 4. B's handler at `routes_knowledge::knowledge_search`:
//!    - Inspects local corpora (empty).
//!    - Walks `mesh.members[*].capabilities.hosted_corpora` and
//!      finds A.
//!    - POSTs `/internal/knowledge/search` to A's internal port
//!      with the request.
//!    - Merges A's response into the union, returns to the client.
//!
//! This is the "user asks Sovereign a philosophy question and the
//! SEP corpus that lives on the Founder's machine actually answers"
//! path. Pre-fix coverage was zero at the integration layer — it
//! spans `corpus-engine` (corpus open/search), `commonwealth-api`
//! (client + internal routes), and `sovereign-mesh` (peer endpoint
//! plumbing).
//!
//! Failure modes guarded:
//! - **Mesh discovery slip.** If B's `peer_offerings` loop doesn't
//!   pick up A's `hosted_corpora`, no fan-out happens and B's
//!   response is empty.
//! - **Address-format slip.** Gossiped addresses carry the
//!   internal port; B must construct `http://<ip>:<internal>/internal/knowledge/search`
//!   verbatim. A regression that rewrites to the client port
//!   (a `:9741`-style assumption) would 404 against the bare
//!   `internal_router`.
//! - **`is_queryable` gate slip.** Only `Online`/`Busy` members
//!   should be queried; an `Offline` peer must drop out of the
//!   fan-out plan even though gossip still surfaces them.
//!
//! - **Ledger emission via X-Node-Id stamping.** The fan-out
//!   POST must include `X-Node-Id: <self_hex>` so the peer-side
//!   `routes_internal::knowledge_search` can attribute the
//!   emitted `KnowledgeQueryServed` event to the requester
//!   (§10's intra-mesh accounting). Pre-fix the header was never
//!   set and the peer's ledger silently stayed empty for every
//!   fan-out request. The third test below pins this end-to-end.
use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::knowledge::CorpusShardInfo;
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_api::server::{client_router, internal_router};
use sovereign_api::state::AppState;
use sovereign_meshapp_registry::registry::AppRegistry;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

async fn install_corpus(indexes_dir: &std::path::Path, id: &str, name: &str, chunk_content: &str) {
    let path = indexes_dir.join(id);
    let index = CorpusIndex::create(
        &path,
        id,
        name,
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: chunk_content.into(),
                title: Some(name.into()),
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

/// `NodeCapabilities` advertising the given hosted corpora. Used to
/// construct B's view of A: B sees A as a member whose `hosted_corpora`
/// includes the corpus we want B to fan out to.
fn caps_with_hosted(corpora: &[&str]) -> NodeCapabilities {
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
        hosted_corpora: corpora
            .iter()
            .map(|id| CorpusShardInfo {
                corpus_id: id.to_string(),
                chunk_range: None,
                is_replica: false,
                last_updated: 0,
                chunk_count: 1,
                canonical_fingerprint: None,
                total_shards: None,
                processed_shards: vec![],
                atlas_atom_count: 0,
                atlas_tier2_count: 0,
                atlas_fingerprint: None,
            })
            .collect(),
        reported_at: 0,
        inference_availability: 0.0,
        inference_capable: false,
        loaded_models: vec![],
        origins: Vec::new(),
        media_allow: Vec::new(),
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: None,
    }
}

#[tokio::test]
async fn joiner_fans_out_to_peer_when_corpus_not_local() {
    // === Daemon A (the Founder) ===
    // Has a real CorpusEngine with the "sep" corpus.
    let tmp_a = tempfile::tempdir().unwrap();
    let indexes_a = tmp_a.path().join("indexes");
    std::fs::create_dir_all(&indexes_a).unwrap();
    install_corpus(
        &indexes_a,
        "sep",
        "Stanford Encyclopedia of Philosophy",
        "Compatibilism: free will is compatible with determinism.",
    )
    .await;
    let recipes_a = tmp_a.path().join("recipes");
    std::fs::create_dir_all(&recipes_a).unwrap();
    let engine_a = Arc::new(
        CorpusEngine::new(recipes_a, indexes_a, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let id_a = NodeId::from_u128(0xAAAA_AAAA_AAAA_AAAA);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry_a = Arc::new(AppRegistry::new());
    // A's mesh contains only A — that's fine, only B needs to know
    // about A for fan-out to work.
    let mut members_a = HashMap::new();
    members_a.insert(
        id_a,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_a,
            name: "Founder".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&["sep"]),
            addresses: vec!["127.0.0.1:9742".parse().unwrap()],
        },
    );
    let mesh_a = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "fanout-test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: members_a,
        peers: vec![],
    };
    let state_a = AppState::new_with_platform_and_engine(
        id_a,
        mesh_a,
        store_a,
        app_registry_a,
        Some(Arc::clone(&engine_a)),
    );
    // Spawn A's internal router. The ephemeral port is what B's
    // fan-out will dial — we'll plug it into B's MemberRecord
    // below.
    let addr_a = spawn_router(internal_router(state_a.clone())).await;

    // === Daemon B (the Joiner) ===
    // No CorpusEngine attached. B's mesh state DOES contain A as
    // a member with `hosted_corpora=["sep"]` so the fan-out
    // discovery loop picks A up.
    let id_b = NodeId::from_u128(0xBBBB_BBBB_BBBB_BBBB);
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry_b = Arc::new(AppRegistry::new());
    let mut members_b = HashMap::new();
    // Self record.
    members_b.insert(
        id_b,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_b,
            name: "Joiner".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&[]),
            addresses: vec!["127.0.0.1:0".parse().unwrap()],
        },
    );
    // A's record — with the ACTUAL bound address as the only
    // gossiped address. This is what `fanout_one_peer` will try.
    members_b.insert(
        id_a,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_a,
            name: "Founder".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&["sep"]),
            addresses: vec![addr_a],
        },
    );
    let mesh_b = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "fanout-test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: members_b,
        peers: vec![],
    };
    let state_b =
        AppState::new_with_platform_and_engine(id_b, mesh_b, store_b, app_registry_b, None);
    let addr_b = spawn_router(client_router(state_b.clone())).await;

    // === Client request ===
    // Joiner-side `/v1/knowledge/search` with the "sep" corpus.
    // B doesn't host it; the fan-out should route to A.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr_b}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "compatibilism",
            "corpora": ["sep"],
            "limit": 10,
        }))
        .send()
        .await
        .expect("/v1/knowledge/search reachable on Joiner");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "fan-out target was reachable; response should be 200"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"]
        .as_array()
        .expect("response carries a results array");

    assert!(
        !results.is_empty(),
        "fan-out must surface at least one chunk from A's corpus; \
         empty results would mean either (a) the joiner didn't see \
         A as a peer offering 'sep', (b) the fan-out URL was wrong, \
         or (c) A's internal_router rejected the request. Got body: {body}"
    );
    let returned_corpus_ids: Vec<&str> = results
        .iter()
        .filter_map(|r| r["corpus_id"].as_str())
        .collect();
    assert!(
        returned_corpus_ids.iter().all(|c| *c == "sep"),
        "every result must be attributed to 'sep' — the only fan-out target; \
         got corpus_ids: {returned_corpus_ids:?}"
    );

    let searched: Vec<&str> = body["corpora_searched"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        searched.contains(&"sep"),
        "corpora_searched must include 'sep' (the peer-served target); got: {searched:?}"
    );

    // Sanity content check — make sure we got A's actual content
    // back, not some empty-row artefact.
    let first_content = results[0]["content"]
        .as_str()
        .expect("result rows have a content field");
    assert!(
        first_content.contains("Compatibilism") || first_content.contains("compatibilism"),
        "first result should carry A's installed chunk content; got: {first_content:?}"
    );
}

#[tokio::test]
async fn offline_peer_is_excluded_from_fan_out_plan() {
    // The `is_queryable` gate in `routes_knowledge` drops any
    // member whose status isn't `Online` or `Busy`. A regression
    // that flipped the gate would burn a 3-second timeout per
    // round on offline peers — UI latency death.
    //
    // We stand up A's internal router but mark A as `Offline` in
    // B's mesh view. The fan-out planner should skip A entirely,
    // so B's response carries no results.
    let tmp_a = tempfile::tempdir().unwrap();
    let indexes_a = tmp_a.path().join("indexes");
    std::fs::create_dir_all(&indexes_a).unwrap();
    install_corpus(&indexes_a, "sep", "SEP", "Some content.").await;
    let recipes_a = tmp_a.path().join("recipes");
    std::fs::create_dir_all(&recipes_a).unwrap();
    let engine_a = Arc::new(
        CorpusEngine::new(recipes_a, indexes_a, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let id_a = NodeId::from_u128(0xA0A0_A0A0_A0A0_A0A0);
    let state_a = AppState::new_with_platform_and_engine(
        id_a,
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "offline-test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        },
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        Some(engine_a),
    );
    let addr_a = spawn_router(internal_router(state_a)).await;

    // B sees A as Offline.
    let id_b = NodeId::from_u128(0xB0B0_B0B0_B0B0_B0B0);
    let mut members_b = HashMap::new();
    members_b.insert(
        id_b,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_b,
            name: "Joiner".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&[]),
            addresses: vec!["127.0.0.1:0".parse().unwrap()],
        },
    );
    members_b.insert(
        id_a,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_a,
            name: "Sleeper".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Offline, // ← the gate
            capabilities: caps_with_hosted(&["sep"]),
            addresses: vec![addr_a],
        },
    );
    let state_b = AppState::new_with_platform_and_engine(
        id_b,
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "offline-test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: members_b,
            peers: vec![],
        },
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
    );
    let addr_b = spawn_router(client_router(state_b)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr_b}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "anything",
            "corpora": ["sep"],
            "limit": 10,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert!(
        results.is_empty(),
        "Offline peer must be skipped — got {} result(s) from a peer the fan-out \
         planner should have dropped: {body}",
        results.len()
    );
}

#[tokio::test]
async fn fan_out_stamps_x_node_id_so_peer_emits_ledger() {
    // The full §10 contract: B fans out to A, A serves the
    // request, A's `ContributionEmitter` records exactly one
    // `KnowledgeQueryServed` event stamped with `for_node = id_b`
    // and `corpus_id = "sep"`.
    //
    // Pre-fix `fanout_one_peer` did NOT stamp `X-Node-Id`, so the
    // peer-side handler at `routes_internal::knowledge_search`
    // saw `parse_x_node_id` → None and skipped emission. This
    // test fails on the pre-fix code path and passes on the
    // post-fix path; it's the regression target for the §10
    // intra-mesh-accounting promise.
    let tmp_a = tempfile::tempdir().unwrap();
    let indexes_a = tmp_a.path().join("indexes");
    std::fs::create_dir_all(&indexes_a).unwrap();
    install_corpus(
        &indexes_a,
        "sep",
        "Stanford Encyclopedia of Philosophy",
        "Some philosophical content.",
    )
    .await;
    let recipes_a = tmp_a.path().join("recipes");
    std::fs::create_dir_all(&recipes_a).unwrap();
    let engine_a = Arc::new(
        CorpusEngine::new(recipes_a, indexes_a, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let id_a = NodeId::from_u128(0xA1_A1_A1_A1_A1_A1_A1_A1);
    let id_b = NodeId::from_u128(0xB2_B2_B2_B2_B2_B2_B2_B2);

    // A's mesh: solo. The ledger emitter is on A's AppState; we
    // keep a handle to it for the assertion.
    let state_a = AppState::new_with_platform_and_engine(
        id_a,
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "ledger-stamp-test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        },
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        Some(Arc::clone(&engine_a)),
    );
    let addr_a = spawn_router(internal_router(state_a.clone())).await;

    // B's mesh: knows A as an Online peer hosting "sep".
    let mut members_b = HashMap::new();
    members_b.insert(
        id_b,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_b,
            name: "Joiner".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&[]),
            addresses: vec!["127.0.0.1:0".parse().unwrap()],
        },
    );
    members_b.insert(
        id_a,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_a,
            name: "Founder".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&["sep"]),
            addresses: vec![addr_a],
        },
    );
    let state_b = AppState::new_with_platform_and_engine(
        id_b,
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "ledger-stamp-test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: members_b,
            peers: vec![],
        },
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
    );
    let addr_b = spawn_router(client_router(state_b)).await;

    // Pre-condition: A's ledger has no KnowledgeQueryServed.
    let pre_events = state_a
        .inner
        .contribution_emitter
        .events()
        .expect("emitter.events() ok");
    assert!(
        pre_events
            .iter()
            .all(|e| !matches!(e.kind, LedgerEventKind::KnowledgeQueryServed { .. })),
        "ledger should be clean before the fan-out fires"
    );

    // Fire the fan-out from B.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr_b}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "philosophy",
            "corpora": ["sep"],
            "limit": 5,
        }))
        .send()
        .await
        .expect("/v1/knowledge/search reachable on Joiner");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // A's emitter should now show exactly one KnowledgeQueryServed
    // event stamped with `for_node = id_b` and `corpus_id = "sep"`.
    let post_events = state_a
        .inner
        .contribution_emitter
        .events()
        .expect("emitter.events() ok");
    let served: Vec<(NodeId, String, u32)> = post_events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::KnowledgeQueryServed {
                for_node,
                corpus_id,
                chunks_returned,
            } => Some((*for_node, corpus_id.clone(), *chunks_returned)),
            _ => None,
        })
        .collect();

    assert_eq!(
        served.len(),
        1,
        "fan-out must trigger exactly one KnowledgeQueryServed on A's ledger; \
         a count of 0 means `X-Node-Id` was not stamped on the outbound \
         fan-out and the peer-side emission gate short-circuited. Got events: \
         {post_events:?}"
    );
    let (for_node, corpus_id, chunks) = &served[0];
    assert_eq!(
        for_node, &id_b,
        "for_node must be the joiner's (requester's) NodeId, not A's own — \
         a regression that stamped `self_id` from A's perspective would put \
         A here, polluting B's lookup. Got: {for_node:?}, expected: {id_b:?}"
    );
    assert_eq!(corpus_id, "sep", "corpus_id must be the served corpus");
    assert_eq!(*chunks, 1, "one chunk served");
}

/// Routes every message to `KnowledgeQuery`, the intent a question about a
/// corpus reaches in production (the ring-room leg-1 log: `→ KnowledgeQuery`).
struct KnowledgeRouter;

#[async_trait::async_trait]
impl sovereign_core::traits::Router for KnowledgeRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &sovereign_core::types::ConversationContext,
        _tools: &[sovereign_core::types::ToolDescriptor],
    ) -> sovereign_core::error::Result<sovereign_core::types::RouterClassification> {
        Ok(sovereign_core::types::RouterClassification {
            primary: sovereign_core::types::IntentCandidate {
                intent: sovereign_core::types::Intent::KnowledgeQuery,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: None,
            coarse_intent: None,
            self_assessment: None,
            timing: None,
            scope: None,
        })
    }
}

/// ring-room D13: a CHAT turn served by the asker's daemon reaches the corpus
/// a peer hosts, and the answer's sources name that peer. The route above was
/// already green; the turn was not, because the daemon's `Runtime` had no mesh
/// seam, so `step_main_retrieval_mesh` skipped the fan-out (ring-room leg 1:
/// `mesh fan-out skipped — this Runtime has no mesh knowledge source`).
///
/// Asserted on `provenance.sources[].from_peer`, which is written from the
/// same `peer_attribution` the released citation's `member` is
/// (rr-1-citation-member-on-released covers that projection at the gate).
/// And the streaming ledger's per-claim holding names the member too
/// (rr-1-pool-members-every-ledger): `SOVEREIGN_LONGFORM_CHARS=0` forces the
/// per-claim gate, the mode whose holdings read null when streaming.rs left
/// the pool's members on a default.
#[tokio::test]
async fn a_daemon_served_chat_turn_names_the_peer_whose_corpus_answered() {
    std::env::set_var("SOVEREIGN_LONGFORM_CHARS", "0");
    const FACT: &str =
        "The Larkspur Lane cooperative keeps its bees in four hives painted teal, ochre, plum and slate.";
    // === Host (Bo): the corpus lives only here, behind its internal router ===
    let tmp_host = tempfile::tempdir().unwrap();
    let indexes_host = tmp_host.path().join("indexes");
    std::fs::create_dir_all(&indexes_host).unwrap();
    install_corpus(&indexes_host, "larkspur", "note-0", FACT).await;
    let engine_host = Arc::new(
        CorpusEngine::new(
            tmp_host.path().join("recipes"),
            indexes_host,
            mock_embed_fn(),
        )
        .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let id_host = NodeId::from_u128(0xB0B0_B0B0_B0B0_B0B0);
    let mut host_self = common::member(id_host, "Bo", "127.0.0.1:9742".parse().unwrap());
    host_self.capabilities = caps_with_hosted(&["larkspur"]);
    let mut mesh_host = common::solo_mesh(id_host, "room");
    mesh_host.members.insert(id_host, host_self);
    let addr_host = spawn_router(internal_router(AppState::new_with_platform_and_engine(
        id_host,
        mesh_host,
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        Some(engine_host),
    )))
    .await;

    // === Asker: nothing installed; its client router knows Bo hosts it ===
    let id_ask = NodeId::from_u128(0xA5A5_A5A5_A5A5_A5A5);
    let mut mesh_ask = common::solo_mesh(id_ask, "room");
    let mut bo = common::member(id_host, "Bo", addr_host);
    bo.capabilities = caps_with_hosted(&["larkspur"]);
    mesh_ask.members.insert(id_host, bo);
    let addr_ask = spawn_router(client_router(AppState::new_with_platform_and_engine(
        id_ask,
        mesh_ask,
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
    )))
    .await;

    // The asker's daemon Runtime, with the seam the daemon gives it.
    let tmp_ask = tempfile::tempdir().unwrap();
    let engine_ask = Arc::new(CorpusEngine::new(
        tmp_ask.path().join("recipes"),
        tmp_ask.path().join("indexes"),
        mock_embed_fn(),
    ));
    let provider: Arc<dyn sovereign_core::traits::InferenceProvider> = Arc::new(
        common::TestProvider::new()
            .with_embed_marker(|_| vec![0.0_f32; EMBED_DIM])
            .with_complete_text("teal, ochre, plum and slate")
            .with_stream_chunks(vec!["The hives are teal, ochre, plum and slate.".into()]),
    );
    // One store for the Runtime that writes the answer and the daemon that
    // projects it onto the `Complete` frame.
    let store: Arc<dyn sovereign_core::traits::StateStore> =
        Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let mut runtime = sovereign_core::runtime::Runtime::new(sovereign_core::RuntimeParts::new(
        Arc::clone(&provider),
        Box::new(KnowledgeRouter),
        Box::new(sovereign_core::stubs::NoOpPlanner),
        Arc::new(sovereign_core::ToolRegistry::new()),
        Arc::clone(&store),
        Arc::new(sovereign_core::SkillRegistry::new()),
        Arc::new(sovereign_core::executor::AutoApprovalChannel),
        sovereign_core::types::InferenceConfig::default(),
        sovereign_core::runtime::lane::LaneSources::none(),
    ));
    runtime.corpus_engine = Some(Arc::clone(&engine_ask));
    runtime.mesh_knowledge =
        sovereign_mesh::knowledge_client::daemon_knowledge_source(&format!("http://{addr_ask}"));
    let daemon = sovereign_mesh::EmbeddedDaemon::new(
        tmp_ask.path().to_path_buf(),
        sovereign_core::setup_config::SetupConfig::unconfigured(),
        common::desktop_services(common::DesktopParts {
            provider,
            store,
            runtime: Arc::new(runtime),
            ..common::DesktopParts::new(engine_ask)
        }),
    );
    let addr_turn = spawn_router(sovereign_mesh::turn_http::turn_router(daemon)).await;

    // === The chat ask, over the route `svrn chat ask` drives ===
    let conv: serde_json::Value = reqwest::Client::new()
        .post(format!("http://{addr_turn}/v1/conversations"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("turn route reachable")
        .json()
        .await
        .unwrap();
    let conv = conv["id"].as_str().expect("conversation id").to_string();
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr_turn}/v1/conversations/{conv}/stream"
    ))
    .await
    .expect("websocket upgrade");
    use futures::{SinkExt, StreamExt};
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&sovereign_contracts::types::TurnRequest::Message {
            content: "What colours are the four hives of the Larkspur Lane cooperative painted?"
                .into(),
            mode: sovereign_contracts::types::TurnMode::Grounded,
            intent: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let complete = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: sovereign_contracts::types::TurnFrame = serde_json::from_str(&t).unwrap();
            match frame {
                sovereign_contracts::types::TurnFrame::Complete { .. } => return frame,
                sovereign_contracts::types::TurnFrame::StreamError { .. } => {
                    panic!("the turn failed: {frame:?}")
                }
                _ => {}
            }
        }
        panic!("the socket closed before a Complete frame")
    })
    .await
    .expect("the turn completed within 60s");
    std::env::remove_var("SOVEREIGN_LONGFORM_CHARS");
    let sovereign_contracts::types::TurnFrame::Complete {
        provenance,
        epistemic_state,
        ..
    } = complete
    else {
        unreachable!()
    };
    let sources = provenance
        .expect("a grounded Complete carries provenance — absent is not the same as no sources")
        .sources;
    assert!(
        sources
            .iter()
            .any(|s| s.origin == "larkspur" && s.from_peer.as_deref() == Some("Bo")),
        "the answer's sources must name Bo as the machine serving `larkspur`; the asker \
         installs nothing, so a source list without it means the turn never fanned out. \
         Got: {sources:?}"
    );
    let members: Vec<Option<String>> = epistemic_state
        .expect("the turn assembled a ledger")
        .holdings
        .iter()
        .filter_map(|h| match &h.provenance {
            sovereign_contracts::types::Provenance::Corpus { member, .. } => Some(member.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !members.is_empty() && members.iter().all(|m| m.as_deref() == Some("Bo")),
        "every per-claim corpus holding over an all-Bo pool must name Bo. Got: {members:?}"
    );
}

fn online_member(
    id: NodeId,
    name: &str,
    hosted: &[&str],
    addr: std::net::SocketAddr,
) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen: 0,
        status: NodeStatus::Online,
        capabilities: caps_with_hosted(hosted),
        addresses: vec![addr],
    }
}

/// The ring-room 4B collision (seat A23): a's gate judge offloaded to Bo took
/// Bo's single peer-inference slot and marked Bo's foreground busy, and Bo then
/// refused a's corpus read with `503 CeilingExceeded` / `YieldedToLocal` — the
/// fan-out recorded the corpus unavailable and a answered with no pool. A
/// corpus read is not an inference: it is admitted under its own ceiling.
#[tokio::test]
async fn corpus_read_is_served_while_the_inference_slot_is_held() {
    let id_a = NodeId::from_u128(0xAAAA_AAAA_AAAA_AAAA);
    let id_b = NodeId::from_u128(0xBBBB_BBBB_BBBB_BBBB);

    // === Bo: hosts the corpus, one peer-inference slot, busy with a's judge ===
    let tmp_b = tempfile::tempdir().unwrap();
    let indexes_b = tmp_b.path().join("indexes");
    std::fs::create_dir_all(&indexes_b).unwrap();
    install_corpus(&indexes_b, "room", "Room", "Bo keeps the room notes.").await;
    let recipes_b = tmp_b.path().join("recipes");
    std::fs::create_dir_all(&recipes_b).unwrap();
    let engine_b = Arc::new(
        CorpusEngine::new(recipes_b, indexes_b, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let mesh_b = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "ring-room".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::from([(
            id_b,
            online_member(id_b, "Bo", &["room"], "127.0.0.1:9742".parse().unwrap()),
        )]),
        peers: vec![],
    };
    let state_b = AppState::new_with_platform_and_engine(
        id_b,
        mesh_b,
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        Some(engine_b),
    );
    // Exactly the collision run's state: the default `max_peer_inflight = 1`,
    // that one slot held by a's offloaded judge, and the judge's turn marking
    // Bo's foreground active inside the yield window.
    state_b.set_contribution_max_peer_inflight(1);
    state_b.set_yield_window_secs(60);
    let _judge = state_b
        .admit_peer_request(id_a, sovereign_api::admission::PeerWork::Inference)
        .expect("a's offloaded judge takes Bo's one inference slot");
    state_b.bump_foreground_active();
    let addr_b = spawn_router(internal_router(state_b.clone())).await;

    // === a: no corpus locally, fans out to Bo ===
    let mesh_a = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "ring-room".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::from([
            (
                id_a,
                online_member(id_a, "a", &[], "127.0.0.1:0".parse().unwrap()),
            ),
            (id_b, online_member(id_b, "Bo", &["room"], addr_b)),
        ]),
        peers: vec![],
    };
    let state_a = AppState::new_with_platform_and_engine(
        id_a,
        mesh_a,
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
    );
    let addr_a = spawn_router(client_router(state_a)).await;

    let body: serde_json::Value = reqwest::Client::new()
        .post(format!("http://{addr_a}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "room notes",
            "corpora": ["room"],
            "limit": 10,
        }))
        .send()
        .await
        .expect("/v1/knowledge/search reachable on a")
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["corpora_searched"],
        serde_json::json!(["room"]),
        "Bo's corpus read must be answered while Bo's inference slot is held — \
         a 503 here blinds a to Bo's corpora. Got: {body}"
    );
    assert!(
        !body["results"].as_array().unwrap().is_empty(),
        "Bo's chunk must come back. Got: {body}"
    );
}
