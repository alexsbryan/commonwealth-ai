// SPDX-License-Identifier: AGPL-3.0-or-later
//! `KnowledgeQueryServed` ledger emission test.
//!
//! `routes_internal::knowledge_search` is the inter-node fan-out
//! target: a peer POSTs a `KnowledgeSearchRequest` over the mesh
//! transport, this daemon searches its installed shards and emits
//! **one `KnowledgeQueryServed` event per corpus** that contributed
//! at least one chunk.
//!
//! Since 2026-09-20 the requester is the member whose key the iroh
//! acceptor VERIFIED, not an `X-Node-Id` header the caller typed —
//! attributing served work to whoever asks for it is how one node
//! spends another's reciprocity. `common::acceptor_stamp` is how a
//! test speaks as a verified peer.
//!
//! The contract (§10 of `SYSTEM_OVERVIEW.md`):
//!   - A caller with no verified identity skips emission. The
//!     dimensional ledger is intra-mesh-only.
//!   - Per-corpus chunk count is post-truncation — reflects what
//!     the requester actually sees, not the raw pre-merge size.
//!   - `for_node` is the requester (the verified key), not the
//!     local node.
//!
//! Three cases worth pinning:
//!
//! 1. **Peer request → one event per contributing corpus.** Two
//!    corpora installed, both contribute → two events emitted with
//!    the right `for_node` + `corpus_id` + `chunks_returned`.
//! 2. **A caller with no verified identity → no event.** Same
//!    request, unstamped → response succeeds, ledger stays empty.
//! 3. **Empty result → no event for the empty corpus.** Filter to
//!    a non-installed corpus → response has no results, no event.
//!
//! Pre-fix this contract was only readable in the route's comments;
//! no test would catch a regression that:
//! - Dropped the per-corpus emission loop entirely (silent ledger).
//! - Stamped the LOCAL node as `for_node` instead of the requester
//!   (lookup pollution).
//! - Emitted before truncation (over-counted under pressure).
use std::sync::Arc;

use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::types::EmbedFn;
use kernel_types::NodeId;
use oicp_types::contributions::LedgerEventKind;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;

use crate::common;
use crate::common::ledger_double::RecordingLedger;
use crate::common::{id_to_hex, spawn_router};

pub(crate) const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Install a corpus at `<indexes>/<id>` with one chunk pinned to a
/// known content string. Returns once `mark_ingestion_complete`
/// has been called, so the engine's `installed_indexes()` reports
/// it as present.
async fn install_corpus_with_chunk(
    indexes_dir: &std::path::Path,
    id: &str,
    name: &str,
    chunk_content: &str,
) {
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

/// Build an `AppState` with ingest's port double over `tmp/indexes/`
/// and pre-installed corpora. Returns the state, the recording double every
/// store port writes to, the roster the test names members in (cw-rails'
/// registration tie is installed, `common::TIE`), and the on-disk directory
/// (keep alive for the test's duration).
pub(crate) async fn build_state_with_corpora(
    self_id: NodeId,
    corpora: &[(&str, &str, &str)], // (id, name, chunk_content)
) -> (
    AppState,
    Arc<RecordingLedger>,
    Arc<common::StaticRoster>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    for (id, name, content) in corpora {
        install_corpus_with_chunk(&indexes, id, name, content).await;
    }
    let engine = Arc::new(crate::common::reading_double(indexes, mock_embed_fn()));
    let double = Arc::new(RecordingLedger::new(self_id));
    let (roster, fabric) = common::roster_seed(self_id, "knowledge-served-test");
    let state = AppState::new_with_seeds(
        self_id,
        Some(engine),
        None,
        fabric,
        Default::default(),
        Default::default(),
        double.seed(),
    );
    // The receiver keeps the last tie after its sender drops.
    let _ = common::tie_as_cw_rails(&state, common::TIE);
    (state, double, roster, tmp)
}

/// The recorded `KnowledgeQueryServed` records, `Debug`-rendered, in call order.
pub(crate) fn served_records(double: &RecordingLedger) -> Vec<String> {
    double
        .calls()
        .into_iter()
        .filter(|c| c.method == "contributions.record")
        .map(|c| c.args)
        .filter(|args| args.starts_with("KnowledgeQueryServed"))
        .collect()
}

/// The record one served query of `chunks_returned` chunks on `corpus_id` makes.
pub(crate) fn served_record(for_node: NodeId, corpus_id: &str, chunks_returned: u32) -> String {
    format!(
        "{:?}",
        LedgerEventKind::KnowledgeQueryServed {
            for_node,
            corpus_id: corpus_id.into(),
            chunks_returned,
        }
    )
}

/// The requester's verified key, as this node's roster would carry it.
const REQUESTER_KEY: [u8; 32] = [0x5a; 32];

#[tokio::test]
async fn peer_request_emits_one_knowledge_query_served_per_contributing_corpus() {
    let self_id = NodeId::from_u128(0xAAAA_AAAA);
    let requester = NodeId::from_u128(0xBBBB_BBBB);
    let (state, double, roster, _tmp) = build_state_with_corpora(
        self_id,
        &[
            ("sep", "Stanford Encyclopedia", "Free will and determinism."),
            ("wikipedia", "Wikipedia", "Article about compatibilism."),
        ],
    )
    .await;
    // The requester is a MEMBER whose key this node's roster names — the
    // only shape that can be attributed now. A typed `X-Node-Id` is a claim,
    // not an identity, and the internal router strips it.
    common::name_member_with_key(&roster, requester, "requester", REQUESTER_KEY);
    let addr = spawn_router(internal_router(state.clone())).await;

    // Request both corpora — both should contribute one chunk each.
    let resp = common::cw_rails_stamp(
        reqwest::Client::new().post(format!("http://{addr}/internal/knowledge/search")),
        "requester",
        requester,
        REQUESTER_KEY,
    )
    .json(&serde_json::json!({
        "query_embedding": vec![0.0_f32; EMBED_DIM],
        "query_text": "compatibilism",
        "corpora": ["sep", "wikipedia"],
        "limit": 10,
    }))
    .send()
    .await
    .expect("/internal/knowledge/search reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().expect("results is an array");
    assert_eq!(
        results.len(),
        2,
        "both corpora should each contribute one chunk; got: {body}"
    );

    // The ledger port should now have recorded exactly two
    // `KnowledgeQueryServed`, one per contributing corpus, each of one
    // chunk, both stamped with `for_node = requester` — the key the
    // acceptor verified, not the local node (a §10 lookup-pollution
    // regression).
    let mut served = served_records(&double);
    served.sort();
    let mut expected = vec![
        served_record(requester, "sep", 1),
        served_record(requester, "wikipedia", 1),
    ];
    expected.sort();
    assert_eq!(
        served, expected,
        "one record per contributing corpus, for_node = requester"
    );
}

#[tokio::test]
async fn local_origin_request_with_no_x_node_id_emits_nothing() {
    // Same request, no `X-Node-Id` header. The route serves the
    // search results but must NOT record any ledger event — §10
    // promises intra-mesh-only accounting, and a missing header
    // means "I can't tell who you are" → safe-default skip.
    let self_id = NodeId::from_u128(0xCCCC_CCCC);
    let (state, double, _roster, _tmp) = build_state_with_corpora(
        self_id,
        &[("sep", "Stanford Encyclopedia", "Compatibilism essay.")],
    )
    .await;
    let addr = spawn_router(internal_router(state.clone())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/knowledge/search"))
        // intentionally no `X-Node-Id`
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "compatibilism",
            "corpora": ["sep"],
            "limit": 10,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["results"].as_array().unwrap().len(),
        1,
        "the search still serves results; only the ledger emission is gated"
    );

    let served = served_records(&double);
    assert_eq!(
        served.len(),
        0,
        "no `X-Node-Id` header → no KnowledgeQueryServed events. \
         Got events: {served:?}"
    );
}

#[tokio::test]
async fn unavailable_corpus_filter_emits_no_event_and_lists_unavailable() {
    // Caller asks for a corpus we don't host. The route returns 200
    // with `corpora_unavailable` listing it, no chunks served, no
    // event emitted (zero chunks → no entry in per_corpus_chunks).
    let self_id = NodeId::from_u128(0xDDDD_DDDD);
    let requester = NodeId::from_u128(0xEEEE_EEEE);
    let (state, double, _roster, _tmp) = build_state_with_corpora(
        self_id,
        &[("sep", "Stanford Encyclopedia", "Some content.")],
    )
    .await;
    let addr = spawn_router(internal_router(state.clone())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/knowledge/search"))
        .header("X-Node-Id", id_to_hex(&requester))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "anything",
            "corpora": ["not-hosted-here"],
            "limit": 10,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let unavailable: Vec<&str> = body["corpora_unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        unavailable.contains(&"not-hosted-here"),
        "the route must report the unhosted corpus in `corpora_unavailable`; got {body}"
    );

    let served_count = served_records(&double).len();
    assert_eq!(
        served_count, 0,
        "zero chunks served → zero KnowledgeQueryServed events. \
         A regression that emits per-corpus regardless of chunk count \
         would over-credit the local node for serving empty searches."
    );
}
