//! End-to-end test for the Phase 6 canonical-sync surface.
//!
//! Wires a real `commonwealth_api::internal_router` over an
//! ephemeral localhost port with a real `CorpusEngine` holding a
//! synthetic canonical, then drives `canonical_pull` from a
//! different node's index dir. Confirms:
//!
//!   1. `GET /internal/corpus/canonical/{id}` streams a tar+zstd
//!      with the `X-Canonical-Fingerprint` header populated.
//!   2. The pull side unpacks into a temp dir, recomputes the
//!      fingerprint, and atomically renames into the final
//!      canonical path.
//!   3. The destination canonical's content_hashes match the
//!      source's byte-for-byte (verified by recomputing the
//!      fingerprint after pull).
//!   4. A pull whose `expected_fingerprint` arg disagrees with the
//!      peer's advertisement is rejected before any rename.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use commonwealth_app::AppRegistry;
use corpus_engine::index::{CorpusIndex, EmbeddedChunk, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_mesh::canonical_pull::{pull_canonical_from_peer, PullError};
use tempfile::tempdir;

/// Build a tiny canonical with three chunks carrying explicit
/// content_hashes. Returns the index_dir (parent of canonical) and
/// the stamped fingerprint.
async fn create_synthetic_canonical(
    index_dir: &Path,
    corpus_id: &str,
) -> String {
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
            code: corpus_engine::index::InsertCodeMeta::default(),
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
        let _ = axum::serve(listener, router).await;
    });
    // Brief beat so axum starts accepting before the test issues
    // its first request.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// Build an AppState whose `corpus_engine` is wired to `index_dir`.
async fn app_state_with_engine(index_dir: &Path) -> AppState {
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "Canonical Sync Test".into(),
        join_key_hash: [0u8; 32],
        members: Default::default(),
        peers: vec![],
    };
    let zero_embed: EmbedFn = Arc::new(|_t: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) })
    });
    let engine = Arc::new(
        CorpusEngine::new(index_dir.to_path_buf(), index_dir.to_path_buf(), zero_embed)
            .with_embedding_model("test-embed"),
    );
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    AppState::new_with_platform_and_engine(
        NodeId::from_u128(1),
        mesh,
        mesh_store,
        Arc::new(AppRegistry::new()),
        Some(engine),
    )
}

#[tokio::test]
async fn canonical_pull_round_trip_via_internal_router() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();

    // Build the source canonical and stamp its fingerprint.
    let expected_fp =
        create_synthetic_canonical(&server_index_dir, "wiki-mini").await;
    assert!(!expected_fp.is_empty(), "fingerprint must be non-empty");

    // Bind internal_router on an ephemeral port pointing at this
    // engine.
    let state = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;
    let peer_url = format!("http://127.0.0.1:{}", addr.port());

    // Pull-side temp dir for the receiving node's local index dir.
    let client_dir = tempdir().unwrap();
    let client_index_dir = client_dir.path().to_path_buf();

    let report = pull_canonical_from_peer(
        &peer_url,
        "wiki-mini",
        &client_index_dir,
        Some(&expected_fp),
    )
    .await
    .expect("pull should succeed");

    assert_eq!(report.fingerprint, expected_fp);
    assert!(report.bytes_uncompressed > 0);
    assert_eq!(report.canonical_path, client_index_dir.join("wiki-mini"));
    assert!(report.canonical_path.join("_corpus_meta.json").is_file());

    // The pulled canonical must reproduce the same fingerprint
    // when reopened — proves the on-disk state is byte-faithful.
    let pulled = CorpusIndex::open(&report.canonical_path).await.unwrap();
    let recomputed = pulled.compute_canonical_fingerprint().await.unwrap();
    assert_eq!(
        recomputed, expected_fp,
        "pulled canonical's fingerprint must match the source's"
    );
}

#[tokio::test]
async fn canonical_pull_rejects_wrong_expected_fingerprint() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();
    let _ = create_synthetic_canonical(&server_index_dir, "wiki-mini").await;

    let state = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;
    let peer_url = format!("http://127.0.0.1:{}", addr.port());

    let client_dir = tempdir().unwrap();
    let client_index_dir = client_dir.path().to_path_buf();

    // Caller passes a wrong fingerprint — the pull must fail with
    // `FingerprintMismatch` BEFORE any rename, and the destination
    // must remain absent.
    let r = pull_canonical_from_peer(
        &peer_url,
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

#[tokio::test]
async fn canonical_pull_returns_404_when_corpus_absent() {
    let server_dir = tempdir().unwrap();
    let server_index_dir = server_dir.path().to_path_buf();
    // Note: NO canonical written.

    let state = app_state_with_engine(&server_index_dir).await;
    let addr = spawn_router(state).await;
    let peer_url = format!("http://127.0.0.1:{}", addr.port());

    let client_dir = tempdir().unwrap();
    let client_index_dir = client_dir.path().to_path_buf();

    let r = pull_canonical_from_peer(
        &peer_url,
        "missing-corpus",
        &client_index_dir,
        None,
    )
    .await;
    match r {
        Err(PullError::PeerHttpError { status, .. }) => {
            assert_eq!(status, 404, "expected 404 for missing canonical");
        }
        other => panic!("expected PeerHttpError(404), got {other:?}"),
    }
}
