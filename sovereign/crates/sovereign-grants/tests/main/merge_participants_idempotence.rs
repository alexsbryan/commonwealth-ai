// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B4 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: the merge under
//! at-least-once delivery.**
//!
//! `IngestExecutor` declares [`Idempotency::Idempotent`] for `ingest:v1`
//! (`sovereign-mesh/src/ingest_executor.rs`), and its module docs say why in
//! plain words: "the merge step dedupes: `merge_shards` keys on
//! `content_hash` and on `(unit_id, source_doc_id)`
//! (`corpus-engine/src/sharding.rs`)". Until this file that was an assertion
//! in English prose rather than in a test (ARCH §7.2): the dedupe has unit
//! coverage inside `corpus-engine`, but nothing had run the MERGE twice and
//! looked at what came out.
//!
//! **What running it twice actually does, measured 2026-09-09.** The bar
//! holds and the stated mechanism does not run. A second
//! `merge_participants` against an already-merged corpus returns
//! `Err(Database("Table 'chunks' already exists"))` and leaves the canonical
//! exactly as it was. It never reaches the dedupe, because `merge_shards`
//! builds its output with `CorpusIndex::create` → `create_empty_table`, which
//! refuses a directory that already holds a `chunks` table.
//!
//! That makes `sharding.rs`'s own comment on the single-shard fast path —
//! "callers that actually do want to fold a partition into an existing
//! canonical fall through to the full merge below (which dedupes via
//! `content_hash`)" — false: the fall-through errors before it can dedupe.
//! `CorpusIndex::create_or_resume` documents this exact LanceDB failure and
//! works around it; `merge_shards` does not. **Not fixed here** — it is
//! corpus-engine's decision whether folding into a live canonical is a
//! capability or a refusal, and the safe behaviour is the one in place.
//!
//! So the two readings below are split accordingly:
//!
//! * [`a_second_merge_of_the_same_handoff_leaves_the_canonical_untouched`] is
//!   B4's bar as pre-registered — the second run must not change the chunk
//!   count — asserted on the CORPUS, so it holds whether the second run
//!   dedupes or refuses.
//! * [`the_merge_dedupes_a_row_two_donors_both_contributed`] witnesses the
//!   dedupe itself, in the one place it is reachable: a single merge whose
//!   inputs overlap. That is the mechanism `Idempotency::Idempotent` cites,
//!   and it is real.
//!
//! **Driven at `ShardManager::merge_participants` and not through
//! `merge_from_fold_coverage`, on purpose.** Going through the caller would
//! measure its `AlreadyHasCanonical` short-circuit, which is a weaker claim:
//! it says the second run declined, not that a second run would have been
//! safe. The bar is the second one.
//!
//! What this file does NOT check
//! -----------------------------
//! * **The fold.** No journal, no `WorkProjection`, no leader decision. The
//!   handoff here is an id. The fold's half of the collector is B2/B3, in
//!   `sovereign-mesh/tests/main/fold_ingest_cross_node_merge_e2e.rs`.
//! * **`(unit_id, source_doc_id)`, the SECONDARY dedupe key.** These rows
//!   carry a populated `content_hash`, so the primary key is the one under
//!   test — which matches the production shape the docs describe ("under the
//!   pull-based queue path every chunk has a populated hash").
//! * **Concurrent merges.** Two runs, strictly in sequence. Two nodes racing
//!   one output directory is B3's subject and is prevented upstream, at the
//!   leader decision, rather than here.
//! * **Whether the refusal leaves the re-pulled shard directory behind.**
//!   `merge_participants` cleans up shard dirs only after a successful merge,
//!   so an errored second run leaves the peer's freshly-pulled partition on
//!   disk. Harmless for the corpus and not this bar's subject, but it is why
//!   the caller's `AlreadyHasCanonical` short-circuit is load-bearing rather
//!   than merely an optimisation.
//!
//! The fixture is `merge_participants_coverage`'s, reused rather than
//! re-derived (ARCH §19): a local partition on disk, a peer whose partition
//! arrives only over a real socket as a real tarball, and a third participant
//! that is silent only because it was given no address.

use commonwealth_core::ids::HandoffId;
use sovereign_grants::shard_manager::MergePlan;
use corpus_engine::index::{InsertChunk, InsertCodeMeta};
use corpus_engine::{Corpus, CorpusIndex};

use super::merge_participants_coverage::{
    build_partition, embedding, fixture, plan, serve_tarball, tar_contents_of, Fixture, CORPUS,
};

/// The canonical's chunk count and whether each named term comes back from a
/// search against it.
///
/// The count alone cannot say a merge was harmless: 2 rows can be the right
/// number and the wrong rows. Same reasoning B2 states for its own bar — the
/// query is what proves the data is reachable.
async fn canonical_state(f: &Fixture, terms: &[&str]) -> (u64, Vec<bool>) {
    let path = Corpus::named(&f.index_dir, CORPUS)
        .expect("non-empty corpus id")
        .root();
    // A canonical that will not open is `0` rows and nothing reachable, not a
    // panic in a helper: "the second delivery deleted the corpus" is a result
    // this bar has to be able to REPORT, and it is the shape the watched red
    // below actually produces.
    let Ok(index) = CorpusIndex::open(&path).await else {
        return (0, terms.iter().map(|_| false).collect());
    };
    let count = index.info().await.expect("info").chunk_count;
    let hits = index
        .search(&embedding(1.0), &terms.join(" "), 16)
        .await
        .expect("search the canonical");
    let found = terms
        .iter()
        .map(|t| hits.iter().any(|h| h.content.contains(t)))
        .collect();
    (count, found)
}

/// Stage a partition directory holding two rows, and serve it as a tarball
/// over its own socket. Returns the base URL a `MergePlan` can name.
///
/// `build_partition` (one row) is the fixture's; this adds the second row
/// through the same public `insert_batch` rather than forking the helper.
async fn serve_two_row_partition(
    tmp: &std::path::Path,
    name: &str,
    first: (&str, &str),
    second: (&str, &str),
) -> String {
    let dir = tmp.join(name);
    build_partition(&dir, first.0, first.1).await;
    CorpusIndex::open(&dir)
        .await
        .expect("reopen the staged partition")
        .insert_batch(&[(
            InsertChunk {
                content: second.0.into(),
                title: Some(second.0.into()),
                url: None,
                metadata: None,
                content_hash: Some(second.1.into()),
                source_doc_id: None,
                source_file: None,
                code: InsertCodeMeta::default(),
                unit_id: None,
            },
            embedding(1.0),
        )])
        .await
        .expect("insert the second row");
    serve_tarball(tar_contents_of(&dir, &tmp.join(format!("{name}.tar")))).await
}

/// **B4 — THE BAR.** Merge the same participant set twice. The corpus after
/// the second run is the corpus after the first: same chunk count, same rows
/// reachable.
///
/// The second run is not a no-op, and knowing that is what makes the bar mean
/// something. After run one the local partition directory is gone (merged
/// shard dirs are cleaned up), so `merge_participants` resolves this node's
/// shard through its documented `original_path` fallback — the canonical
/// itself — and pulls the peer's shard down the socket a second time. It is
/// then asked to write its output over the very directory it just named as an
/// input.
///
/// The assertion is on the CORPUS and not on the return value, deliberately.
/// Two mechanisms satisfy this bar — dedupe the repeat rows, or refuse the
/// write — and the pre-registration named the first while the tree does the
/// second (see the module docs). Asserting the corpus keeps the bar honest
/// across both, and keeps this test from blocking a corpus-engine change that
/// makes folding into a live canonical work.
///
/// Failing input, named and watched (ARCH §18.1): make
/// `CorpusIndex::create_with_sharing` clear an existing directory before
/// creating the table — the obvious way to "fix" the error this run returns.
/// Run two then rebuilds the canonical from a canonical it has already
/// deleted, `alpha` is gone, and this test goes red on the row check rather
/// than the count.
#[tokio::test]
async fn a_second_merge_of_the_same_handoff_leaves_the_canonical_untouched() {
    let f = fixture().await;
    let participants = [f.local, f.reachable_peer];

    let first = f
        .manager
        .merge_participants(plan(&f, &participants, Some(2)))
        .await
        .expect("the first merge must succeed")
        .expect("merge_participants returns the merged index");
    let (count_1, found_1) = canonical_state(&f, &["alpha", "bravo"]).await;
    assert_eq!(
        first.chunk_count, 2,
        "alpha from the local shard + bravo from the peer's"
    );
    assert_eq!(count_1, 2);
    assert!(
        found_1.iter().all(|&b| b),
        "control: the first merge must land BOTH rows, or 'unchanged' below \
         would be unchanged-and-wrong. alpha/bravo reachable: {found_1:?}",
    );

    // ── The second delivery ──
    let second = f
        .manager
        .merge_participants(plan(&f, &participants, Some(2)))
        .await;
    let (count_2, found_2) = canonical_state(&f, &["alpha", "bravo"]).await;

    assert_eq!(
        count_2, count_1,
        "the second delivery changed the canonical's chunk count: {count_1} → \
         {count_2}. At-least-once is what the fold delivers — `auto_ingest` \
         re-reads the same terminal handoff every tick — so a second merge \
         must be harmless. Second run returned: {second:?}. See \
         `quality/campaigns/cw-lift-5g-part2-prereg.md` B4.",
    );
    assert!(
        found_2.iter().all(|&b| b),
        "the second delivery dropped a row. A count that stayed at {count_1} \
         while a row went missing is the same defect wearing a passing number \
         — on run two the LOCAL shard IS the canonical, so a clobber before \
         the read shows up exactly here. alpha/bravo reachable: {found_2:?}. \
         Second run returned: {second:?}",
    );

    // How the bar holds, pinned so the next reader is not told a mechanism
    // that does not run. An `Err` here is the measured behaviour; an `Ok`
    // would mean a corpus-engine change made the fold-into-live-canonical
    // path work, which the assertions above already cover.
    if let Err(e) = &second {
        assert!(
            e.to_string().contains("already exists"),
            "a second merge that fails must fail VISIBLY and for the reason \
             this file documents, not collapse into some other shape \
             (ARCH §18.3). Got: {e}",
        );
    }
}

/// **The dedupe itself, witnessed where it is reachable.** Two donors both
/// contribute a row under the same `content_hash`; the canonical holds it
/// once.
///
/// This is the mechanism `Idempotency::Idempotent` is declared on the
/// strength of, and the bar above cannot see it — that path errors before the
/// dedupe runs (module docs). Four input rows across three shards, three rows
/// out.
///
/// It doubles as the bar's negative control. Without it, "the second merge
/// changed nothing" would pass just as happily against a fixture whose merge
/// cannot combine shards at all (ARCH §18.1).
///
/// Failing input, named and watched (ARCH §18.1): delete the
/// `seen_hashes.contains(h)` early-return from `merge_shards`
/// (`corpus-engine/src/sharding.rs`) and the canonical comes back with 4
/// chunks, `bravo` twice.
#[tokio::test]
async fn the_merge_dedupes_a_row_two_donors_both_contributed() {
    let f = fixture().await;

    // A second reachable donor. `silent_peer` is silent only because the
    // fixture gives it no address; handed one it is an ordinary peer, so no
    // new identity is minted for this reading. Its partition carries `bravo`
    // — the row `reachable_peer` also contributes, under the SAME hash — plus
    // `delta`, which nothing else has.
    let staging = tempfile::tempdir().expect("tempdir");
    let second_url = serve_two_row_partition(
        staging.path(),
        "peer-2",
        ("bravo", "h-bravo"),
        ("delta", "h-delta"),
    )
    .await;

    let mut urls = f.peer_urls.clone();
    urls.push((f.silent_peer, second_url));
    let participants = [f.local, f.reachable_peer, f.silent_peer];

    let info = f
        .manager
        .merge_participants(MergePlan {
            handoff_id: HandoffId::from_u128(0xC0FFEE),
            corpus_id: CORPUS,
            local_node_id: f.local,
            participants: &participants,
            peer_shard_base_urls: &urls,
            ephemeral: false,
            expected_partitions: Some(3),
        })
        .await
        .expect("all three shards resolve, so the coverage bar is met")
        .expect("merge_participants returns the merged index");

    let (count, found) = canonical_state(&f, &["alpha", "bravo", "delta"]).await;
    assert_eq!(
        info.chunk_count, 3,
        "four input rows across three shards, and `bravo` arrives twice under \
         one content_hash — the canonical must hold three. Got {}. This is the \
         dedupe `IngestExecutor::Idempotency::Idempotent` is declared on.",
        info.chunk_count,
    );
    assert_eq!(count, 3, "the canonical must hold what the merge reported");
    assert!(
        found.iter().all(|&b| b),
        "dedupe must drop the REPEAT, never a distinct row: \
         alpha/bravo/delta reachable: {found:?}",
    );
}
