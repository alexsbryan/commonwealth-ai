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
//! forever" (`sovereign-daemon/src/auto_ingest.rs`). A subset merge is a
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
//!
//! # Split at the port (pb-grants-merge, phase-b-47)
//!
//! Grants decides who participates and pulls the peers' partitions; the
//! merge and the finalize are ingest's, reached through
//! `PartitionMergePort`. So the manager here drives `IngestPortDouble`, and
//! these tests assert what grants HANDS the port: which partition dirs,
//! holding what, into which canonical, then the finalize. What ingest does
//! with those two partitions (one canonical, two rows, reachable) is proven
//! on `impl PartitionMergePort for CorpusEngine` over the same fixtures, in
//! corpus-engine's `partition_merge_port_parity`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use corpus_index::index::{InsertChunk, InsertCodeMeta};
use corpus_index::ingest_port::double::IngestPortDouble;
use corpus_index::{corpus::Corpus, index::CorpusIndex};
use kernel_types::{HandoffId, NodeId};
use sovereign_contracts::peer::SoloReplicatedKv;
use sovereign_grants::shard_manager::MergePlan;
use sovereign_grants::ShardManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const EMBED_DIM: usize = 8;
pub(crate) const CORPUS: &str = "coverage";

pub(crate) fn embedding(seed: f32) -> Vec<f32> {
    (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
}

pub(crate) async fn build_partition(path: &Path, content: &str, hash: &str) {
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
        text_sha256: None,
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
pub(crate) fn tar_contents_of(dir: &Path, tar_path: &Path) -> Vec<u8> {
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
pub(crate) async fn serve_tarball(tar: Vec<u8>) -> String {
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

/// One `merge_partitions` call the port received: each input dir with the
/// chunk count it held AT CALL TIME (the manager deletes merged shard dirs
/// afterwards), and the output dir.
#[derive(Debug, Clone)]
pub(crate) struct MergeCall {
    pub(crate) inputs: Vec<(PathBuf, u64)>,
    pub(crate) output: PathBuf,
}

/// What grants handed ingest's port, in order.
#[derive(Default)]
pub(crate) struct PortLog {
    pub(crate) merges: Mutex<Vec<MergeCall>>,
    pub(crate) finalized: Mutex<Vec<String>>,
}

/// Ingest's merge port as a double, rooted at `index_dir`.
///
/// Its merge records what each input held, then creates the canonical at
/// the output with the leaf's own `CorpusIndex::create`, empty, because
/// `merge_participants` opens it before the finalize. It merges no rows:
/// what ingest writes is proven on the engine. With `fail_from_merge =
/// Some(n)`, call `n` and every later one answer the error ingest gives for
/// a canonical whose table already exists.
pub(crate) fn merge_port(
    index_dir: &Path,
    log: Arc<PortLog>,
    fail_from_merge: Option<usize>,
) -> IngestPortDouble {
    let merges = Arc::clone(&log);
    let finalized = log;
    IngestPortDouble::new()
        .with_index_dir(index_dir)
        .on_merge_partitions(move |inputs, output| {
            let log = Arc::clone(&merges);
            Box::pin(async move {
                let mut seen = Vec::new();
                for dir in &inputs {
                    let rows = CorpusIndex::open(dir).await?.info().await?.chunk_count;
                    seen.push((dir.clone(), rows));
                }
                let nth = {
                    let mut calls = log.merges.lock().expect("merges lock");
                    calls.push(MergeCall {
                        inputs: seen,
                        output: output.clone(),
                    });
                    calls.len()
                };
                if fail_from_merge.is_some_and(|n| nth >= n) {
                    return Err(corpus_index::Error::Database(
                        "Table 'chunks' already exists".into(),
                    ));
                }
                let canonical = CorpusIndex::create(
                    &output,
                    CORPUS,
                    "Coverage Corpus",
                    "test-model",
                    EMBED_DIM,
                    true,
                    "MIT",
                )
                .await?;
                canonical.info().await
            })
        })
        .on_finalize_canonical(move |corpus_id| {
            finalized
                .finalized
                .lock()
                .expect("finalized lock")
                .push(corpus_id.to_string());
            Ok(())
        })
}

pub(crate) struct Fixture {
    _tmp: tempfile::TempDir,
    pub(crate) index_dir: PathBuf,
    /// Everything the manager handed ingest's port.
    pub(crate) log: Arc<PortLog>,
    /// The double the manager holds, for its call order.
    pub(crate) port: Arc<IngestPortDouble>,
    /// The gossip store the manager loads handoffs from. Exposed so a caller
    /// can seed a handoff blob and drive `coordinate_merge`.
    pub(crate) mesh_store: Arc<SoloReplicatedKv>,
    pub(crate) manager: ShardManager,
    pub(crate) local: NodeId,
    pub(crate) reachable_peer: NodeId,
    pub(crate) silent_peer: NodeId,
    pub(crate) peer_urls: Vec<(NodeId, String)>,
}

impl Fixture {
    /// Where `node`'s partition dir sits: this node's own on disk, a peer's
    /// once the pull lands it.
    pub(crate) fn partition_of(&self, node: NodeId) -> PathBuf {
        Corpus::named(&self.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .partition(&node.to_string())
    }

    pub(crate) fn canonical(&self) -> PathBuf {
        Corpus::named(&self.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .root()
    }

    pub(crate) fn merges(&self) -> Vec<MergeCall> {
        self.log.merges.lock().expect("merges lock").clone()
    }

    pub(crate) fn finalized(&self) -> Vec<String> {
        self.log.finalized.lock().expect("finalized lock").clone()
    }

    /// The port's merge-family calls, in order (its `index_dir` reads left
    /// out: they are paths, not acts).
    pub(crate) fn port_acts(&self) -> Vec<&'static str> {
        self.port
            .calls()
            .into_iter()
            .filter(|c| *c != "index_dir")
            .collect()
    }
}

/// Three participants, two of whom can be resolved:
///
/// * `local` — its partition dir is on disk.
/// * `reachable_peer` — served by a live socket, so the pull lands.
/// * `silent_peer` — no entry in `peer_shard_base_urls`, so it is
///   skipped exactly as an unaddressable peer is in production.
pub(crate) async fn fixture() -> Fixture {
    fixture_failing_from(None).await
}

/// [`fixture`], with the port's merge failing from call `n` on.
pub(crate) async fn fixture_failing_from(n: Option<usize>) -> Fixture {
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

    let log = Arc::new(PortLog::default());
    let port = Arc::new(merge_port(&index_dir, Arc::clone(&log), n));
    let mesh_store = Arc::new(SoloReplicatedKv::new());
    let manager = ShardManager::new(port.clone(), mesh_store.clone());

    Fixture {
        _tmp: tmp,
        index_dir,
        log,
        port,
        mesh_store,
        manager,
        local,
        reachable_peer,
        silent_peer,
        peer_urls: vec![(reachable_peer, url)],
    }
}

pub(crate) fn plan<'a>(
    f: &'a Fixture,
    participants: &'a [NodeId],
    expected: Option<usize>,
) -> MergePlan<'a> {
    MergePlan {
        mesh_proof: None,
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
        corpus_index::Error::IncompleteCoverage {
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

    // The load-bearing half: nothing was handed to ingest to merge, so
    // nothing that looks complete can have been produced.
    assert!(
        f.port_acts().is_empty(),
        "a refused merge must reach neither the merge nor the finalize; \
         the port saw {:?}",
        f.port_acts(),
    );
    assert!(
        !f.canonical().exists(),
        "a refused merge must leave no canonical behind",
    );
    // The resolved shards stay where they are: the refusal returns before
    // the cleanup that follows a merge.
    assert!(f.partition_of(f.local).exists());
    assert!(f.partition_of(f.reachable_peer).exists());
}

#[tokio::test]
async fn merges_the_same_two_shards_when_no_bar_is_set() {
    // Negative control for the test above. Same fixture, same two
    // resolvable shards, same unaddressable third participant — only
    // `expected_partitions` differs. Without this, the guard test would
    // pass just as happily against a harness that can never merge.
    let f = fixture().await;
    let participants = [f.local, f.reachable_peer, f.silent_peer];

    f.manager
        .merge_participants(plan(&f, &participants, None))
        .await
        .expect("unbarred merge must succeed")
        .expect("merge_participants returns the merged index");

    let merges = f.merges();
    assert_eq!(merges.len(), 1, "one merge per plan: {merges:?}");
    assert_eq!(
        merges[0].inputs,
        vec![
            (f.partition_of(f.local), 1),
            (f.partition_of(f.reachable_peer), 1),
        ],
        "alpha's partition from disk + bravo's, pulled over the wire into \
         its partition dir, one row each",
    );
    assert_eq!(merges[0].output, f.canonical());
    assert_eq!(
        f.port_acts(),
        vec!["merge_partitions", "finalize_canonical"],
        "the merge ends in the finalize that makes the canonical reachable",
    );
    assert_eq!(f.finalized(), vec![CORPUS.to_string()]);
    // Shard-dir cleanup is part of what moved out of `coordinate_merge`.
    assert!(
        !f.partition_of(f.local).exists() && !f.partition_of(f.reachable_peer).exists(),
        "merged shard dirs are cleaned up",
    );
}
