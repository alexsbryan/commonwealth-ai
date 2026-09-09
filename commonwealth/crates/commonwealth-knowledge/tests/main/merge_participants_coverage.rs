// SPDX-License-Identifier: AGPL-3.0-or-later
//! The coverage bar on [`ShardManager::merge_participants`].
//!
//! `merge_participants` is the merge half of a collaborative ingest,
//! split out of `coordinate_merge` so the fold-side collector in
//! sovereign-mesh — which resolves participation completely differently
//! — shares ONE merge implementation (ARCH §10.6). This file witnesses
//! the one behaviour that is NEW rather than moved: `expected_partitions`.
//!
//! Why the bar exists. `auto_recover`'s coverage guard arms only when a
//! partition meta stamps `total_shards`, and `corpus-engine`'s ingest
//! stamps that for `ExtractorConfig::WikipediaJsonl` alone. With the
//! guard dark, linux-peer merged 17 of 38 wikipedia partitions into a
//! canonical that then advertised itself as complete on gossip — "every
//! peer ends up with a different 'complete' canonical and they fight
//! forever" (`sovereign-mesh/src/auto_ingest.rs`). A subset merge is a
//! refusal, not a result.
//!
//! Lives at the public-API layer because the second caller is in another
//! crate: if `merge_participants` or `MergePlan` stops being reachable
//! from outside `commonwealth-knowledge`, this file stops compiling.
//!
//! The pair is deliberate. `refuses_when_coverage_falls_short_of_the_bar`
//! is the guard; `merges_the_same_two_shards_when_no_bar_is_set` is its
//! negative control, because a guard test over a harness that never
//! merges anything passes for the wrong reason.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_knowledge::shard_manager::MergePlan;
use commonwealth_knowledge::ShardManager;
use commonwealth_state::MeshStore;
use corpus_engine::index::{InsertChunk, InsertCodeMeta};
use corpus_engine::{Corpus, CorpusEngine, CorpusIndex, EmbedFn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const EMBED_DIM: usize = 8;
const CORPUS: &str = "coverage";

/// Never called: `merge_participants` merges vectors that already exist.
/// Present only because `CorpusEngine::new` requires one.
fn unused_embed_fn() -> EmbedFn {
    Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0f32; EMBED_DIM]) }))
}

fn embedding(seed: f32) -> Vec<f32> {
    (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
}

async fn build_partition(path: &Path, content: &str, hash: &str) {
    let index = CorpusIndex::create(
        path,
        CORPUS,
        "Coverage Corpus",
        "test-model",
        EMBED_DIM,
        true,
        "MIT",
    )
    .await
    .expect("create index");
    let chunk = InsertChunk {
        content: content.into(),
        title: Some(content.into()),
        url: None,
        metadata: None,
        content_hash: Some(hash.into()),
        source_doc_id: None,
        source_file: None,
        code: InsertCodeMeta::default(),
        unit_id: None,
    };
    index
        .insert_batch(&[(chunk, embedding(1.0))])
        .await
        .expect("insert_batch");
}

/// Tar the CONTENTS of `dir` (`tar cf … -C dir .`), byte-identical to
/// what `routes_internal::index_serve` ships. The puller extracts
/// straight into its `<corpus>-partition-<peer>/` dest, so a top-level
/// wrapper entry would produce a nested dir that `merge_partitions`
/// does not recognise as a shard.
fn tar_contents_of(dir: &Path, tar_path: &Path) -> Vec<u8> {
    let status = std::process::Command::new("tar")
        .args([
            "cf",
            &tar_path.to_string_lossy(),
            "-C",
            &dir.to_string_lossy(),
            ".",
        ])
        .status()
        .expect("spawn tar");
    assert!(status.success(), "tar cf failed: {status}");
    let bytes = std::fs::read(tar_path).expect("read tar");
    let _ = std::fs::remove_file(tar_path);
    bytes
}

/// The smallest thing `fetch_remote_shard` will talk to: answer any
/// request with `200` + the tarball. Using a real socket rather than a
/// mocked HTTP client keeps the pull path — reqwest, `tar xf`, the dest
/// dir — inside the test rather than stubbed around it.
async fn serve_tarball(tar: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = tar.clone();
            tokio::spawn(async move {
                // Drain the request head; we do not care what it asked for.
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            head.extend_from_slice(&buf[..n]);
                            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    index_dir: PathBuf,
    manager: ShardManager,
    local: NodeId,
    reachable_peer: NodeId,
    silent_peer: NodeId,
    peer_urls: Vec<(NodeId, String)>,
}

/// Three participants, two of whom can be resolved:
///
/// * `local` — its partition dir is on disk.
/// * `reachable_peer` — served by a live socket, so the pull lands.
/// * `silent_peer` — no entry in `peer_shard_base_urls`, so it is
///   skipped exactly as an unaddressable peer is in production.
async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let index_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&index_dir).expect("mkdir index_dir");

    // The HIGH bytes must differ. Partition dir names are built from
    // `NodeId`'s `Display`, which is the hex of the first EIGHT bytes
    // only — `from_u128(0x11)` and `from_u128(0x22)` print identically
    // and would collide on one directory, quietly turning this
    // two-shard fixture into a one-shard one.
    let local = NodeId::from_u128(0x11 << 120);
    let reachable_peer = NodeId::from_u128(0x22 << 120);
    let silent_peer = NodeId::from_u128(0x33 << 120);

    let corpus = Corpus::named(&index_dir, CORPUS).expect("non-empty corpus id");
    build_partition(&corpus.partition(&local.to_string()), "alpha", "h-alpha").await;

    // The peer's partition lives outside index_dir and reaches the
    // manager only over the wire, the same way a real peer's does.
    let staging = tmp.path().join("peer-staging");
    build_partition(&staging, "bravo", "h-bravo").await;
    let tar = tar_contents_of(&staging, &tmp.path().join("peer.tar"));
    let url = serve_tarball(tar).await;

    let engine = Arc::new(CorpusEngine::new(
        tmp.path().join("recipes"),
        index_dir.clone(),
        unused_embed_fn(),
    ));
    let mesh_store = Arc::new(MeshStore::in_memory().expect("in-memory mesh store"));
    let manager = ShardManager::new(Arc::clone(&engine), index_dir.clone(), mesh_store);

    Fixture {
        _tmp: tmp,
        index_dir,
        manager,
        local,
        reachable_peer,
        silent_peer,
        peer_urls: vec![(reachable_peer, url)],
    }
}

fn plan<'a>(f: &'a Fixture, participants: &'a [NodeId], expected: Option<usize>) -> MergePlan<'a> {
    MergePlan {
        handoff_id: HandoffId::from_u128(0xC0FFEE),
        corpus_id: CORPUS,
        local_node_id: f.local,
        participants,
        peer_shard_base_urls: &f.peer_urls,
        ephemeral: false,
        expected_partitions: expected,
    }
}

#[tokio::test]
async fn refuses_when_coverage_falls_short_of_the_bar() {
    let f = fixture().await;
    let participants = [f.local, f.reachable_peer, f.silent_peer];

    let err = f
        .manager
        .merge_participants(plan(&f, &participants, Some(3)))
        .await
        .expect_err("2 of 3 partitions must be a refusal, not a canonical");

    match err {
        corpus_engine::Error::IncompleteCoverage {
            ref corpus,
            covered,
            expected,
        } => {
            assert_eq!(corpus, CORPUS);
            assert_eq!(covered, 2, "local + the reachable peer resolved");
            assert_eq!(expected, 3);
        }
        other => panic!("expected IncompleteCoverage; got {other:?}"),
    }

    // The load-bearing half: nothing that looks complete was produced.
    assert!(
        !Corpus::named(&f.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .is_installed(),
        "a refused merge must leave no canonical behind",
    );
}

#[tokio::test]
async fn merges_the_same_two_shards_when_no_bar_is_set() {
    // Negative control for the test above. Same fixture, same two
    // resolvable shards, same unaddressable third participant — only
    // `expected_partitions` differs. Without this, the guard test would
    // pass just as happily against a harness that can never merge.
    let f = fixture().await;
    let participants = [f.local, f.reachable_peer, f.silent_peer];

    let info = f
        .manager
        .merge_participants(plan(&f, &participants, None))
        .await
        .expect("unbarred merge must succeed")
        .expect("merge_participants returns the merged index");

    assert_eq!(
        info.chunk_count, 2,
        "alpha from local + bravo from the peer"
    );
    assert!(
        Corpus::named(&f.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .is_installed(),
        "the canonical must exist after an unbarred merge",
    );
    // Shard-dir cleanup is part of what moved out of `coordinate_merge`.
    assert!(
        !Corpus::named(&f.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .partition(&f.local.to_string())
            .exists(),
        "merged shard dirs are cleaned up",
    );
}
