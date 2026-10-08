// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of sovereign-grants' merge tests (pb-grants-merge,
//! phase-b-47).
//!
//! Grants' `merge_participants_coverage`, `coordinate_merge_installs_the_canonical`
//! and `merge_participants_idempotence` drive `IngestPortDouble` and assert
//! what grants hands ingest: which partitions, into which canonical, then
//! the finalize. What ingest does with them is proven here, through
//! `impl PartitionMergePort for CorpusEngine`, over the same partition
//! fixtures (one row each, `alpha` local and `bravo` from the peer, 8-dim
//! vectors, content hashes `h-<term>`):
//!
//! * B8: a merge plus the finalize yields a corpus in `installed_indexes()`
//!   and `usable_indexes()` that answers a search for both donors' terms —
//!   read at that altitude, never through `CorpusIndex::open` alone, which
//!   bypasses both gates (`df2ffecb8`). Before the finalize it is in
//!   neither, which is the gap B8 closed.
//! * B4: a second merge written over the canonical leaves its rows as they
//!   were (measured 2026-09-09: ingest refuses, "already exists"), and one
//!   merge whose donors overlap on a `content_hash` keeps the row once — the
//!   dedupe `IngestExecutor`'s `Idempotency::Idempotent` is declared on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_index::corpus::Corpus;
use corpus_index::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use corpus_index::ingest_port::merge::PartitionMergePort;
use corpus_index::types::EmbedFn;

const EMBED_DIM: usize = 8;
const CORPUS: &str = "coverage";

fn embedding(seed: f32) -> Vec<f32> {
    (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
}

/// Never called: the merge moves vectors that already exist.
fn unused_embed_fn() -> EmbedFn {
    Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0f32; EMBED_DIM]) }))
}

/// One partition holding `rows` (content, content_hash), as grants' fixture
/// builds it.
/// Rows are `(document, content, content_hash)`.
async fn build_partition(path: &Path, rows: &[(&str, &str, &str)]) {
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
    let batch: Vec<_> = rows
        .iter()
        .map(|(doc, content, hash)| {
            (
                InsertChunk {
                    content: (*content).into(),
                    title: Some((*content).into()),
                    url: None,
                    metadata: None,
                    content_hash: Some((*hash).into()),
                    source_doc_id: Some((*doc).into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                embedding(1.0),
            )
        })
        .collect();
    index.insert_batch(&batch).await.expect("insert_batch");
}

struct Fixture {
    _tmp: tempfile::TempDir,
    index_dir: PathBuf,
    engine: Arc<CorpusEngine>,
}

impl Fixture {
    fn partition(&self, node: &str) -> PathBuf {
        Corpus::named(&self.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .partition(node)
    }

    fn canonical(&self) -> PathBuf {
        Corpus::named(&self.index_dir, CORPUS)
            .expect("non-empty corpus id")
            .root()
    }

    fn port(&self) -> &dyn PartitionMergePort {
        &*self.engine
    }
}

async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let index_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&index_dir).expect("mkdir index_dir");
    let engine = Arc::new(CorpusEngine::new(
        tmp.path().join("recipes"),
        index_dir.clone(),
        unused_embed_fn(),
    ));
    Fixture {
        _tmp: tmp,
        index_dir,
        engine,
    }
}

/// The two partitions grants hands ingest: `alpha` from this node's disk,
/// `bravo` as the peer's pull lands it.
async fn two_donors(f: &Fixture) -> Vec<PathBuf> {
    let local = f.partition("local");
    let peer = f.partition("peer");
    build_partition(&local, &[("doc-a", "alpha", "h-alpha")]).await;
    build_partition(&peer, &[("doc-b", "bravo", "h-bravo")]).await;
    vec![local, peer]
}

/// The canonical's chunk count and whether each term comes back from a
/// search, opened by path. `(0, all false)` when it will not open.
async fn on_disk(f: &Fixture, terms: &[&str]) -> (u64, Vec<bool>) {
    let Ok(index) = CorpusIndex::open(&f.canonical()).await else {
        return (0, terms.iter().map(|_| false).collect());
    };
    let count = index.info().await.expect("info").chunk_count;
    (count, reachable(&index, terms).await)
}

async fn reachable(index: &CorpusIndex, terms: &[&str]) -> Vec<bool> {
    let hits = index
        .search(&embedding(1.0), &terms.join(" "), 16)
        .await
        .expect("search the canonical");
    terms
        .iter()
        .map(|t| hits.iter().any(|h| h.content.contains(t)))
        .collect()
}

/// Is `CORPUS` in the engine's `installed_indexes()` and `usable_indexes()`?
async fn listed(f: &Fixture) -> (bool, bool) {
    let has =
        |rows: Vec<corpus_index::types::IndexInfo>| rows.iter().any(|i| i.corpus_id == CORPUS);
    (
        has(f
            .engine
            .installed_indexes()
            .await
            .expect("installed_indexes")),
        has(f.engine.usable_indexes().await.expect("usable_indexes")),
    )
}

/// **B8, engine half.** The merge alone writes rows no surface lists; the
/// finalize makes the corpus one a user can reach, both donors' terms
/// included.
#[tokio::test]
async fn merge_then_finalize_lands_a_corpus_a_user_can_reach() {
    let f = fixture().await;
    let donors = two_donors(&f).await;

    let info = f
        .port()
        .merge_partitions(&donors, &f.canonical())
        .await
        .expect("the merge");
    assert_eq!(info.chunk_count, 2, "alpha + bravo");
    assert_eq!(
        listed(&f).await,
        (false, false),
        "control: merged but not finalized, the canonical is in neither list \
         — the state `coordinate_merge` left before B8",
    );

    let canonical = CorpusIndex::open(&f.canonical())
        .await
        .expect("open the canonical");
    f.port()
        .finalize_canonical(&canonical, CORPUS)
        .await
        .expect("the finalize");

    assert_eq!(
        listed(&f).await,
        (true, true),
        "finalized, the canonical is in `installed_indexes()` (what \
         `hosted_corpora` gossip is built from) and `usable_indexes()`",
    );
    let index = f
        .engine
        .open_index_for_corpus(CORPUS)
        .await
        .expect("a corpus usable_indexes() listed must open");
    assert_eq!(
        reachable(&index, &["alpha", "bravo"]).await,
        vec![true, true],
        "both donors' rows answer a search through the engine",
    );
    assert_eq!(on_disk(&f, &["alpha", "bravo"]).await.0, 2);
}

/// **B4, engine half.** A second merge written over the canonical — this
/// node's shard resolved as the canonical itself, the peer's re-pulled, as
/// grants hands them on a second delivery — leaves the canonical's rows as
/// they were.
///
/// Asserted on the CORPUS, because two mechanisms satisfy the bar (dedupe
/// the repeat rows, or refuse the write) and the tree does the second. The
/// error, when there is one, must be ingest's named one.
///
/// Failing input, named: make `CorpusIndex::create_with_sharing` clear an
/// existing directory before creating the table; the second merge then
/// rebuilds the canonical from a canonical it already deleted and `alpha`
/// is gone.
#[tokio::test]
async fn a_second_merge_over_the_canonical_leaves_its_rows() {
    let f = fixture().await;
    let donors = two_donors(&f).await;
    f.port()
        .merge_partitions(&donors, &f.canonical())
        .await
        .expect("the first merge");
    let canonical = CorpusIndex::open(&f.canonical()).await.expect("open");
    f.port()
        .finalize_canonical(&canonical, CORPUS)
        .await
        .expect("the finalize");
    let (count_1, found_1) = on_disk(&f, &["alpha", "bravo"]).await;
    assert_eq!((count_1, found_1.clone()), (2, vec![true, true]), "control");

    let second = f
        .port()
        .merge_partitions(&[f.canonical(), donors[1].clone()], &f.canonical())
        .await;
    let (count_2, found_2) = on_disk(&f, &["alpha", "bravo"]).await;

    assert_eq!(
        count_2, count_1,
        "the second merge changed the canonical's chunk count. Returned: {second:?}",
    );
    assert_eq!(
        found_2, found_1,
        "the second merge dropped a row. Returned: {second:?}",
    );
    if let Err(e) = &second {
        assert!(
            e.to_string().contains("already exists"),
            "a second merge that fails must fail for the reason grants' \
             idempotence test documents (ARCH §18.3). Got: {e}",
        );
    }
}

/// **The dedupe itself.** Two donors both contribute document `doc-b`'s
/// `bravo` under one `content_hash`; the canonical holds it once. Four input
/// rows across three shards, three out.
///
/// Failing input, named: make the `seen_keys.insert(key)` check in
/// `merge_shards` (`ingest/crates/corpus-engine/src/sharding.rs`) always keep the row and
/// the canonical comes back with 4 chunks, `bravo` twice.
#[tokio::test]
async fn the_merge_dedupes_a_row_two_donors_both_contributed() {
    let f = fixture().await;
    let mut donors = two_donors(&f).await;
    let second_peer = f.partition("peer-2");
    build_partition(
        &second_peer,
        &[("doc-b", "bravo", "h-bravo"), ("doc-d", "delta", "h-delta")],
    )
    .await;
    donors.push(second_peer);

    let info = f
        .port()
        .merge_partitions(&donors, &f.canonical())
        .await
        .expect("the merge");

    assert_eq!(
        info.chunk_count, 3,
        "four rows in, `bravo` twice under one hash"
    );
    let (count, found) = on_disk(&f, &["alpha", "bravo", "delta"]).await;
    assert_eq!(count, 3, "the canonical holds what the merge reported");
    assert_eq!(
        found,
        vec![true, true, true],
        "dedupe drops the REPEAT, never a distinct row",
    );
}

/// The same text in two documents is two rows. Two threads' timelines each
/// hold an event reading `closed`: one `content_hash`, two documents.
///
/// Failing input, named: key `merge_shards`' dedupe on `content_hash` alone
/// (`corpus_index::ChunkKey` without its document) and the canonical comes
/// back with 1 chunk, the second thread's event gone.
#[tokio::test]
async fn the_merge_keeps_one_text_in_two_documents() {
    let f = fixture().await;
    let (local, peer) = (f.partition("local"), f.partition("peer"));
    build_partition(&local, &[("issues/1#event-1", "closed", "h-closed")]).await;
    build_partition(&peer, &[("issues/2#event-2", "closed", "h-closed")]).await;

    let info = f
        .port()
        .merge_partitions(&[local, peer], &f.canonical())
        .await
        .expect("the merge");

    assert_eq!(info.chunk_count, 2, "one text, two documents, two rows");
}
