// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end test for the Phase 6 canonical-sync surface.
//!
//! Wires a real `sovereign_daemon::internal_router` over an
//! ephemeral localhost port whose corpus handle reads a synthetic
//! canonical, then drives `canonical_pull` from a
//! different node's index dir. Confirms:
//!
//!   1. `GET /internal/corpus/canonical/{id}` streams exactly what
//!      ingest's `pack_canonical` writes for the canonical, headed by
//!      its `X-Canonical-Fingerprint` and chunk count.
//!   2. A pull whose `expected_fingerprint` arg disagrees with the
//!      peer's advertisement is rejected before any rename.
//!   3. A pull falls through an unreachable first URL to the peer.
//!
//! Split at the port (pb-ingest-dial-daemon-tests-merge, phase-b-52):
//! the pack is ingest's, reached through `IngestPort::pack_canonical`,
//! so the serving node holds `IngestPortDouble` and these readings
//! assert what the route asks of it and streams from it. That what the
//! port packs unpacks, the way the pull side unpacks it, into a
//! canonical recomputing the same fingerprint — this file's round trip
//! until then — is corpus-engine's `daemon_port_parity`.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use corpus_index::index::{CorpusIndex, EmbeddedChunk, InsertChunk};
use corpus_index::ingest_port::double::IngestPortDouble;
use corpus_index::types::EmbedFn;
use kernel_types::NodeId;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::{fabric, node, serving, AppState};
use sovereign_mesh::canonical_pull::{pull_canonical_from_peer, PullError};
use tempfile::tempdir;

use crate::common::ledger_double::RecordingLedger;

/// What the serving node's pack writes: not an archive. The route streams
/// it untouched, which is the claim; what a real pack holds is the engine's.
const PACKED: &[u8] = b"what ingest packed for wiki-mini";

/// Build a tiny canonical with three chunks carrying explicit
/// content_hashes. Returns the stamped fingerprint.
async fn create_synthetic_canonical(index_dir: &Path, corpus_id: &str) -> String {
    let canonical_path = index_dir.join(corpus_id);
    let idx = CorpusIndex::create(
        &canonical_path,
        corpus_id,
        "Canonical Sync Test",
        "test-embed",
        4,
        true,  // mesh_sharing
        "MIT", // license
    )
    .await
    .expect("create index");

    let mk = |hash: &str, content: &str, vec: [f32; 4]| EmbeddedChunk {
        insert: InsertChunk {
            content: content.into(),
            title: Some(format!("doc-{hash}")),
            url: None,
            metadata: None,
            content_hash: Some(hash.into()),
            source_doc_id: Some(hash.into()),
            source_file: None,
            code: corpus_index::index::InsertCodeMeta::default(),
            unit_id: None,
        },
        embedding: vec.to_vec(),
    };
    idx.insert_chunks(&[
        mk("hash-aaa", "content for AAA", [1.0, 0.0, 0.0, 0.0]),
        mk("hash-bbb", "content for BBB", [0.0, 1.0, 0.0, 0.0]),
        mk("hash-ccc", "content for CCC", [0.0, 0.0, 1.0, 0.0]),
    ])
    .await
    .expect("insert");

    idx.mark_ingestion_complete().expect("mark complete");
    idx.compute_and_stamp_fingerprint().await.expect("stamp")
}

/// Spawn the internal API router for `state` on `127.0.0.1:0`.
/// Returns the bound address. The server lives for the lifetime of
/// the test process (the JoinHandle is dropped intentionally —
/// tokio::test owns the runtime).
async fn spawn_router(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = internal_router(state);
    tokio::spawn(async move {
        // `into_make_service_with_connect_info` as `start_daemon`'s internal
        // listener does —
        // `internal_gate` refuses a hop with no peer address.
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    // Brief beat so axum starts accepting before the test issues
    // its first request.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// Each `pack_canonical` the serving node's port was asked for: the
/// canonical path and the compression level.
type Packs = Arc<Mutex<Vec<(PathBuf, i32)>>>;

/// Build an AppState whose corpus handle reads `index_dir`: ingest's port
/// double over the leaf's own reader (`common::reading_double`). The route
/// asks the port for `canonical_path`, which the engine answers by the
/// `Corpus` layout the double delegates to as well, and for
/// `pack_canonical`, which this double records and answers with
/// [`PACKED`].
async fn app_state_with_engine(index_dir: &Path) -> (AppState, Packs) {
    let zero_embed: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok::<Vec<f32>, corpus_index::Error>(vec![0.0; 4]) }));
    let packs: Packs = Arc::default();
    let seen = Arc::clone(&packs);
    let engine: Arc<IngestPortDouble> = Arc::new(
        crate::common::reading_double(index_dir.to_path_buf(), zero_embed).on_pack_canonical(
            move |path, mut writer, level| {
                seen.lock()
                    .expect("packs lock")
                    .push((path.to_path_buf(), level));
                writer.write_all(PACKED)?;
                Ok(PACKED.len() as u64)
            },
        ),
    );
    let self_id = NodeId::from_u128(1);
    let state = AppState::new_with_seeds(
        self_id,
        Some(engine),
        None,
        fabric::FabricSeed::default(),
        serving::ServingSeed::default(),
        node::NodeSeed::default(),
        Arc::new(RecordingLedger::new(self_id)).seed(),
    );
    (state, packs)
}

/// The route streams what ingest packed for the canonical — asked for by
/// the canonical's path at compression level 1 — headed by the fingerprint
/// and chunk count the canonical carries.
///
/// Failing input, named: stream the route's body from a path other than
/// `canonical_path` (say the partition dir); the recorded pack names it.
#[tokio::test]
async fn the_canonical_route_streams_what_ingest_packs_under_its_fingerprint() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();
    let expected_fp = create_synthetic_canonical(&server_index_dir, "wiki-mini").await;
    assert!(!expected_fp.is_empty(), "fingerprint must be non-empty");

    let (state, packs) = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;

    let resp = reqwest::get(format!(
        "http://127.0.0.1:{}/internal/corpus/canonical/wiki-mini",
        addr.port()
    ))
    .await
    .expect("the route answers");
    assert_eq!(resp.status(), 200);
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    assert_eq!(
        header("x-canonical-fingerprint").as_deref(),
        Some(expected_fp.as_str())
    );
    assert_eq!(header("x-canonical-chunk-count").as_deref(), Some("3"));
    let body = resp.bytes().await.expect("the body streams");
    assert_eq!(
        &body[..],
        PACKED,
        "the body is what ingest packed, untouched"
    );
    assert_eq!(
        *packs.lock().unwrap(),
        vec![(server_index_dir.join("wiki-mini"), 1)],
        "one pack, of the canonical, at the level the route documents",
    );
}

#[tokio::test]
async fn canonical_pull_rejects_wrong_expected_fingerprint() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();
    let _ = create_synthetic_canonical(&server_index_dir, "wiki-mini").await;

    let (state, _packs) = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;
    let peer_url = format!("http://127.0.0.1:{}", addr.port());

    let client_dir = tempdir().unwrap();
    let client_index_dir = client_dir.path().to_path_buf();

    // Caller passes a wrong fingerprint — the pull must fail with
    // `FingerprintMismatch` BEFORE any rename, and the destination
    // must remain absent.
    let candidates = vec![peer_url];
    let r = pull_canonical_from_peer(
        &candidates,
        "wiki-mini",
        &client_index_dir,
        Some("0".repeat(64).as_str()),
    )
    .await;
    match r {
        Err(PullError::FingerprintMismatch { .. }) => {}
        other => panic!("expected FingerprintMismatch, got {other:?}"),
    }
    assert!(
        !client_index_dir.join("wiki-mini").exists(),
        "destination must not exist after rejected pull"
    );
}

/// Verifies the address-fallthrough fix: when a peer publishes
/// multiple addresses and the FIRST one is unreachable (e.g. a
/// LAN IP that doesn't route from the puller's network), the
/// pull tries the next URL until one answers. This is the
/// regression test for the linux-peer case where Alex's MacBook
/// gossiped `[192.168.1.6, 100.64.0.2-tailscale, ipv6]` and
/// my first cut picked the LAN address that wasn't reachable
/// from the puller's network.
///
/// The peer is proven reached by its own advertisement: asked for a
/// fingerprint it does not hold, the pull comes back naming the one the
/// peer headed its stream with. An unreachable URL alone is a transport
/// error, never a mismatch.
#[tokio::test]
async fn canonical_pull_falls_through_on_unreachable_first_url() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();
    let peer_fp = create_synthetic_canonical(&server_index_dir, "wiki-mini").await;

    let (state, _packs) = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;
    let working_url = format!("http://127.0.0.1:{}", addr.port());

    // RFC 5737 reserved test-net address; nothing routable lives
    // here. The connect attempt should refuse / time out fast and
    // the pull should advance to the second (working) URL.
    let dead_url = "http://192.0.2.1:9742".to_string();

    let candidates = vec![dead_url, working_url];
    let r = pull_canonical_from_peer(
        &candidates,
        "wiki-mini",
        tempdir().unwrap().path(),
        Some("0".repeat(64).as_str()),
    )
    .await;

    match r {
        Err(PullError::FingerprintMismatch { actual, .. }) => assert_eq!(
            actual, peer_fp,
            "the mismatch must name the working peer's advertisement"
        ),
        other => panic!("expected the working URL's FingerprintMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn canonical_pull_returns_404_when_corpus_absent() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();
    // Note: NO canonical written.

    let (state, _packs) = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;
    let peer_url = format!("http://127.0.0.1:{}", addr.port());

    let client_dir = tempdir().unwrap();
    let client_index_dir = client_dir.path().to_path_buf();

    let candidates = vec![peer_url];
    let r = pull_canonical_from_peer(&candidates, "missing-corpus", &client_index_dir, None).await;
    match r {
        Err(PullError::PeerHttpError { status, .. }) => {
            assert_eq!(status, 404, "expected 404 for missing canonical");
        }
        other => panic!("expected PeerHttpError(404), got {other:?}"),
    }
}
