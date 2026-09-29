// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B4 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: the merge under
//! at-least-once delivery — grants' half.**
//!
//! `IngestExecutor` declares `Idempotency::Idempotent` for `ingest:v1`
//! (`sovereign-daemon/src/ingest_executor.rs`) on the strength of the merge's
//! dedupe. Measured 2026-09-09: a second merge against an already-merged
//! corpus never reaches that dedupe. Ingest refuses to write over a canonical
//! whose `chunks` table exists (`Err(Database("Table 'chunks' already
//! exists"))`) and leaves it exactly as it was; the dedupe runs only inside
//! one merge whose inputs overlap.
//!
//! # Split at the port (pb-grants-merge, phase-b-47)
//!
//! Both of those readings are about what INGEST does with the partitions it
//! is handed, so they run on `impl PartitionMergePort for CorpusEngine` in
//! corpus-engine's `partition_merge_port_parity`: the second merge leaves the
//! canonical's rows untouched, and overlapping donors dedupe.
//!
//! What stays here is what GRANTS does on the second delivery, which the
//! engine cannot see. After run one this node's partition is gone (merged
//! shard dirs are cleaned up), so `merge_participants` resolves the local
//! shard through its documented `original_path` fallback — the canonical
//! itself — and pulls the peer's shard down the socket a second time. It then
//! hands ingest exactly that pair, into the same canonical, and returns
//! ingest's refusal unchanged, without the finalize and without cleaning up.
//!
//! **Driven at `ShardManager::merge_participants` and not through
//! `merge_from_fold_coverage`, on purpose.** Going through the caller would
//! measure its `AlreadyHasCanonical` short-circuit, which says the second
//! run declined, not what a second run does.
//!
//! What this file does NOT check: the fold (B2/B3, in
//! `sovereign-daemon/tests/main/fold_ingest_cross_node_merge_e2e.rs`), and
//! concurrent merges (prevented upstream, at the leader decision).
//!
//! The fixture is `merge_participants_coverage`'s, reused rather than
//! re-derived (ARCH §19): a local partition on disk, a peer whose partition
//! arrives only over a real socket as a real tarball.

use super::merge_participants_coverage::{fixture_failing_from, plan};

/// **B4, grants' half.** The second delivery of one handoff hands ingest the
/// canonical (as this node's shard) and the re-pulled peer shard, and ingest's
/// refusal comes back as it was given: no finalize, nothing cleaned up.
///
/// Failing input, named: drop the `original_path` fallback from
/// `merge_participants` and the second run resolves no local shard, so the
/// port's second merge sees one input, not two.
#[tokio::test]
async fn a_second_delivery_hands_ingest_the_canonical_and_returns_its_refusal() {
    // The port answers ingest's "already exists" from its second merge on.
    let f = fixture_failing_from(Some(2)).await;
    let participants = [f.local, f.reachable_peer];

    f.manager
        .merge_participants(plan(&f, &participants, Some(2)))
        .await
        .expect("the first merge must succeed")
        .expect("merge_participants returns the merged index");
    assert!(
        !f.partition_of(f.local).exists(),
        "control: run one cleaned up this node's partition, so run two can \
         only find its shard through the canonical",
    );

    // ── The second delivery ──
    let second = f
        .manager
        .merge_participants(plan(&f, &participants, Some(2)))
        .await;

    let merges = f.merges();
    assert_eq!(merges.len(), 2, "one merge per delivery: {merges:?}");
    let resolved: Vec<_> = merges[1]
        .inputs
        .iter()
        .map(|(dir, _)| dir.clone())
        .collect();
    assert_eq!(
        resolved,
        vec![f.canonical(), f.partition_of(f.reachable_peer)],
        "run two resolves this node's shard as the canonical itself and \
         re-pulls the peer's",
    );
    assert_eq!(merges[1].output, f.canonical());

    let err = second.expect_err("ingest refused the second write");
    assert!(
        err.to_string().contains("already exists"),
        "a second merge that fails must fail VISIBLY and for ingest's reason, \
         not collapse into some other shape (ARCH §18.3). Got: {err}",
    );
    assert_eq!(
        f.finalized(),
        vec![super::merge_participants_coverage::CORPUS.to_string()],
        "only run one finalized; a refused merge is not finalized",
    );
    assert!(
        f.partition_of(f.reachable_peer).exists(),
        "cleanup follows a successful merge only, so the re-pulled peer \
         partition stays — which is why the caller's `AlreadyHasCanonical` \
         short-circuit is load-bearing",
    );
}
