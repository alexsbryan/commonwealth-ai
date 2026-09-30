// SPDX-License-Identifier: AGPL-3.0-or-later
//! Contribution records under concurrent load, seen at the daemon's
//! store port.
//!
//! The §10 ledger invariant says every recorded event carries
//! `node_id = self.self_node_id` regardless of which caller
//! triggered the emission. Under concurrent traffic from many
//! peers (a real Founder serving simultaneous fan-out requests
//! from a mesh of 5+ joiners), the daemon must:
//!
//! 1. Leave origin to the ledger port: `ContributionLedgerPort::record`
//!    takes only the kind, so the daemon cannot stamp an origin at
//!    all — the port stamps its own node id (five-programs fp-84).
//! 2. Issue **every** record without loss — one per served request.
//! 3. Carry the requester in the per-variant `for_node` payload.
//!
//! The AppState here is built over the recording double
//! (`common::ledger_double`), so each assertion reads the recorded
//! `contributions.record` calls. The store half — keys that cannot
//! collide, serde round-trip — is pinned beside the writer
//! (commonwealth-state's `contributions` tests). A regression that:
//!
//!   - stamped the requester somewhere other than `for_node`,
//!   - dropped records under contention,
//!   - or recorded the wrong corpus,
//!
//! would slip past every unit test but corrupt the dimensional
//! ledger in production. Caught here.
use std::sync::Arc;

use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::ingest_port::double::IngestPortDouble;
use corpus_index::types::EmbedFn;
use kernel_types::NodeId;
use oicp_types::contributions::LedgerEventKind;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::{fabric, node, serving, AppState};

use crate::common;
use crate::common::ledger_double::RecordingLedger;
use crate::common::{id_to_hex, solo_mesh, spawn_router};

const EMBED_DIM: usize = 8;
const N_REQUESTERS: u64 = 50;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// An AppState whose every store port is `double`.
fn state_over(
    double: &Arc<RecordingLedger>,
    self_id: NodeId,
    mesh_name: &str,
    engine: Arc<IngestPortDouble>,
) -> AppState {
    AppState::new_with_seeds(
        self_id,
        solo_mesh(self_id, mesh_name),
        Some(engine),
        None,
        fabric::FabricSeed::default(),
        serving::ServingSeed::default(),
        node::NodeSeed::default(),
        double.seed(),
    )
}

/// The recorded `contributions.record` arguments, in call order.
fn recorded(double: &RecordingLedger) -> Vec<String> {
    double
        .calls()
        .into_iter()
        .filter(|c| c.method == "contributions.record")
        .map(|c| c.args)
        .collect()
}

/// The record one served query on the one-chunk `sep` corpus makes.
fn served_record(for_node: NodeId) -> String {
    format!(
        "{:?}",
        LedgerEventKind::KnowledgeQueryServed {
            for_node,
            corpus_id: "sep".into(),
            chunks_returned: 1,
        }
    )
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

    // The port double + one corpus so every request returns
    // exactly one chunk → exactly one KnowledgeQueryServed event
    // per request. Cleaner accounting than "some events".
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install_corpus(&indexes, "sep").await;
    let engine = Arc::new(crate::common::reading_double(indexes, mock_embed_fn()));

    let double = Arc::new(RecordingLedger::new(self_id));
    let state = state_over(&double, self_id, "origin-test", engine);
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

    // Inspect the recorded ledger calls.
    let served: Vec<String> = recorded(&double);

    // Assertion 1: no records lost.
    assert_eq!(
        served.len() as u64,
        N_REQUESTERS,
        "{} concurrent requests must produce {} records; got {}. \
         A count below N means the emit path dropped records \
         under contention.",
        N_REQUESTERS,
        N_REQUESTERS,
        served.len()
    );

    // Assertions 2 and 3: every record is `KnowledgeQueryServed` on `sep`
    // with the requester as `for_node`, and every requester appears exactly
    // once. Origin is the port's own stamp — `record` takes no origin, so a
    // regression that flipped origin and `for_node` cannot compile. If two
    // records shared a for_node, one request would have been double-counted
    // or another lost — either way, accounting drift.
    let mut got = served;
    got.sort();
    let mut expected: Vec<String> = requesters.iter().copied().map(served_record).collect();
    expected.sort();
    assert_eq!(
        got, expected,
        "every requester must appear exactly once as for_node on a `sep` \
         record — duplicates or omissions mean the emit path lost track \
         under concurrency"
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
    let engine = Arc::new(crate::common::reading_double(indexes, mock_embed_fn()));

    let double = Arc::new(RecordingLedger::new(self_id));
    let state = state_over(&double, self_id, "origin-swap-test", engine);
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

    let served: Vec<String> = recorded(&double)
        .into_iter()
        .filter(|args| args.starts_with("KnowledgeQueryServed"))
        .collect();
    assert_eq!(
        served.len(),
        1,
        "a verified requester that happens to be us still produces one record"
    );

    // The record's origin is the port's own stamp (`record` takes none), so
    // it is self regardless of header content. for_node reflects the
    // VERIFIED requester — the attribution path works, and the origin is
    // independent of it.
    assert_eq!(
        served[0],
        served_record(self_id),
        "for_node is the verified requester; a regression that conflated \
         origin and for_node would lose this distinction"
    );
}
