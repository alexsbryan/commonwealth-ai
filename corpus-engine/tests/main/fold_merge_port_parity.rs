// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of sovereign-daemon's fold/pull e2es
//! (pb-ingest-dial-daemon-tests-merge, phase-b-52).
//!
//! `fold_ingest_cross_node_merge_e2e`, `fold_ingest_abandoned_unit_e2e` and
//! `fold_ingest_coverage_refusal_e2e` drive `IngestPortDouble` and assert
//! what the daemon hands ingest: which partitions, into which canonical,
//! then the finalize, or no merge at all. What ingest does with them is
//! proven here, through `impl PartitionMergePort for CorpusEngine`, over the
//! same partition fixture those files write (two donors, two chunks each,
//! 4-dim vectors of 0.25, a term only each donor carries).
//!
//! The defect the cross-node file was minted over is kept here: its first
//! reading probed the canonical with `CorpusIndex::open` by path and passed
//! over a corpus `installed_indexes()` could not see (`df2ffecb8`). So the
//! bar is read through `installed_indexes()` / `usable_indexes()` and the
//! engine's by-id open, and the by-path reading is asserted beside it as the
//! control that it alone would have passed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_index::corpus::Corpus;
use corpus_index::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use corpus_index::ingest_port::merge::PartitionMergePort;
use corpus_index::types::EmbedFn;

const EMBED_DIM: usize = 4;
const LEADER_ONLY_TERM: &str = "quokka";
const PEER_ONLY_TERM: &str = "narwhal";
/// The two donors' `NodeId` displays, as the daemon's fixture names them.
const LEADER: &str = "node-1100000000000000";
const PEER: &str = "node-2200000000000000";

/// Never called: the merge moves vectors that already exist.
fn unused_embed_fn() -> EmbedFn {
    Arc::new(|_: &str| Box::pin(async { Ok(vec![0.25f32; EMBED_DIM]) }))
}

fn corpus(index_dir: &Path, id: &str) -> Corpus {
    Corpus::named(index_dir, id).expect("non-empty corpus id")
}

/// One donor's finished slice at its partition dir, as the daemon's
/// `write_donor_partition` writes it.
async fn donor_partition(index_dir: &Path, node: &str, id: &str, unit_id: u32, term: &str) {
    let index = CorpusIndex::create(
        &corpus(index_dir, id).partition(node),
        id,
        "cw-lift 5g cross-node",
        "test-embed",
        EMBED_DIM,
        true,
        "MIT",
    )
    .await
    .expect("create partition index");
    let rows: Vec<_> = (0..2)
        .map(|i| {
            (
                InsertChunk {
                    content: format!("unit {unit_id} chunk {i}: the {term} is here"),
                    title: Some(format!("{term}-{i}")),
                    url: None,
                    metadata: None,
                    content_hash: Some(format!("{term}-{unit_id}-{i}")),
                    source_doc_id: Some(format!("{term}-{unit_id}-{i}")),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: Some(unit_id),
                },
                vec![0.25_f32; EMBED_DIM],
            )
        })
        .collect();
    index.insert_batch(&rows).await.expect("insert_batch");
    index
        .mark_ingestion_complete()
        .expect("mark the slice finished");
}

async fn term_reachable(index: &CorpusIndex, term: &str) -> bool {
    index
        .search(&[0.25_f32; EMBED_DIM], term, 10)
        .await
        .expect("search the canonical")
        .iter()
        .any(|h| h.content.contains(term))
}

/// Whether `id` is in `installed_indexes()` and in `usable_indexes()`.
async fn listed(engine: &CorpusEngine, id: &str) -> (bool, bool) {
    let has = |rows: Vec<corpus_index::types::IndexInfo>| rows.iter().any(|i| i.corpus_id == id);
    (
        has(engine.installed_indexes().await.expect("installed_indexes")),
        has(engine.usable_indexes().await.expect("usable_indexes")),
    )
}

/// Each term's reachability through the engine's by-id open, the surface a
/// user reaches; `None` when `usable_indexes()` does not list the corpus.
async fn reachable_where_a_user_reaches(
    engine: &CorpusEngine,
    id: &str,
    terms: &[&str],
) -> Option<Vec<bool>> {
    if !listed(engine, id).await.1 {
        return None;
    }
    let index = engine
        .open_index_for_corpus(id)
        .await
        .expect("a corpus usable_indexes() listed must open");
    let mut found = Vec::new();
    for term in terms {
        found.push(term_reachable(&index, term).await);
    }
    Some(found)
}

struct Node {
    _tmp: tempfile::TempDir,
    index_dir: PathBuf,
    engine: Arc<CorpusEngine>,
}

fn node() -> Node {
    let tmp = tempfile::tempdir().expect("tempdir");
    let index_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&index_dir).expect("index dir");
    let engine = Arc::new(CorpusEngine::new(
        tmp.path().join("recipes"),
        index_dir.clone(),
        unused_embed_fn(),
    ));
    Node {
        _tmp: tmp,
        index_dir,
        engine,
    }
}

/// **B2 and B5's merge clause, engine half.** The two partitions the daemon
/// hands ingest (the leader's from disk, the peer's as its pull lands it),
/// merged then finalized, are a corpus a user reaches with both donors'
/// terms; merged alone they are rows only a by-path open finds.
///
/// Failing input, named: skip `mark_ingestion_complete` in the engine's
/// `finalize_canonical` (`sharding.rs`); `installed_indexes()` drops the
/// canonical and the finalized reading goes red while the by-path one stays
/// green, the `df2ffecb8` shape.
#[tokio::test]
async fn a_fold_merge_then_finalize_lands_both_donors_where_a_user_reaches() {
    const CORPUS: &str = "cw-lift-5g-two-nodes";
    let n = node();
    donor_partition(&n.index_dir, LEADER, CORPUS, 0, LEADER_ONLY_TERM).await;
    donor_partition(&n.index_dir, PEER, CORPUS, 1, PEER_ONLY_TERM).await;
    let c = corpus(&n.index_dir, CORPUS);
    let port: &dyn PartitionMergePort = &*n.engine;

    let info = port
        .merge_partitions(&[c.partition(LEADER), c.partition(PEER)], &c.root())
        .await
        .expect("the merge");
    assert_eq!(info.chunk_count, 4, "2 chunks from each donor");

    let by_path = CorpusIndex::open(&c.root()).await.expect("open by path");
    assert!(
        term_reachable(&by_path, LEADER_ONLY_TERM).await
            && term_reachable(&by_path, PEER_ONLY_TERM).await,
        "control: the merged rows are on disk, which is all a by-path probe sees",
    );
    assert_eq!(
        listed(&n.engine, CORPUS).await,
        (false, false),
        "control: merged but not finalized, the canonical is in neither list — \
         the state a by-path probe passed over",
    );

    port.finalize_canonical(&by_path, CORPUS)
        .await
        .expect("the finalize");

    assert_eq!(
        listed(&n.engine, CORPUS).await,
        (true, true),
        "finalized, the canonical is in `installed_indexes()` (what \
         `hosted_corpora` gossip is built from) and `usable_indexes()`",
    );
    assert_eq!(
        reachable_where_a_user_reaches(&n.engine, CORPUS, &[LEADER_ONLY_TERM, PEER_ONLY_TERM])
            .await,
        Some(vec![true, true]),
        "both donors' terms answer a search through the engine's by-id open",
    );
}

/// **The one-node control and B7's premise, engine half.** The disk-derived
/// merge `try_recover_stranded_partitions` hands ingest merges every
/// partition under the index dir into a reachable canonical: both donors'
/// slices when both are local, and ONLY the leader's when the peer's never
/// arrived. The second is the partial canonical B7 exists to refuse; ingest
/// builds it without complaint, so the refusal must happen before the port.
#[tokio::test]
async fn the_disk_merge_lands_every_local_partition_and_only_those() {
    const WHOLE: &str = "cw-lift-5g-one-node";
    const PARTIAL: &str = "cw-lift-5g-unstamped";
    let n = node();
    donor_partition(&n.index_dir, LEADER, WHOLE, 0, LEADER_ONLY_TERM).await;
    donor_partition(&n.index_dir, PEER, WHOLE, 1, PEER_ONLY_TERM).await;
    donor_partition(&n.index_dir, LEADER, PARTIAL, 0, LEADER_ONLY_TERM).await;
    let port: &dyn PartitionMergePort = &*n.engine;

    let whole = port
        .merge_partitions_into_canonical(&n.index_dir, WHOLE, None)
        .await
        .expect("the whole merge");
    assert_eq!(
        whole.chunks_merged, 4,
        "2 chunks from each of the two units"
    );
    assert_eq!(
        reachable_where_a_user_reaches(&n.engine, WHOLE, &[LEADER_ONLY_TERM, PEER_ONLY_TERM]).await,
        Some(vec![true, true]),
    );

    let partial = port
        .merge_partitions_into_canonical(&n.index_dir, PARTIAL, None)
        .await
        .expect("the partial merge");
    assert_eq!(partial.chunks_merged, 2, "this node's slice only");
    assert_eq!(
        reachable_where_a_user_reaches(&n.engine, PARTIAL, &[LEADER_ONLY_TERM, PEER_ONLY_TERM])
            .await,
        Some(vec![true, false]),
        "half the corpus, installed and searchable: the hazard B7 names",
    );
}
