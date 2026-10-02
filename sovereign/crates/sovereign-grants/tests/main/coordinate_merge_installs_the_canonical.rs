// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B8 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: a merge produces a
//! corpus someone can reach — for BOTH callers, not just the one that
//! remembered.**
//!
//! `2dc1bf160` gave the fold-side caller its finalize by calling
//! `corpus_engine::finalize_canonical` after `ShardManager::merge_participants`
//! returned. The QUEUE-mode caller — `ShardManager::coordinate_merge`, reached
//! from `commonwealth-api`'s `routes_internal/corpus_queue.rs:210` and `:550` —
//! shares that merge and got nothing: `:210` logs and returns, `:550` goes
//! straight to `verify_merge_sample`. Neither finalizes, so a queue-mode merge
//! wrote chunks that `installed_indexes()` skips on `is_ingestion_complete`
//! (`corpus-engine/src/engine/mod.rs`, "Skipping partial index"), which means
//! `usable_indexes()` never sees them and `hosted_corpora` gossip — built from
//! `installed_indexes()` in `sovereign-mesh/src/capabilities.rs` — advertises
//! nothing.
//!
//! **This file is `coordinate_merge`'s first test.** Established at
//! `c001e3634` and re-checked here: before this file, every mention of
//! `coordinate_merge` under a `tests/` path was a prose reference in a module
//! comment. The state machine that decides queue-mode participation, leadership
//! and the merge had no executable claim on it at all, which is the reason the
//! gap `2dc1bf160` found on one caller could sit unnoticed on the other.
//!
//! # Split at the port (pb-grants-merge, phase-b-47)
//!
//! B8 has two halves, and since the merge is ingest's they live in two
//! crates. This file holds grants' half: queue-mode `coordinate_merge`
//! resolves both donors, pulls the peer's partition, and hands ingest's
//! `PartitionMergePort` both partitions and THEN the finalize, so the
//! post-condition is structural at the caller that used to skip it. The
//! other half — a merge plus that finalize yields a corpus in
//! `installed_indexes()` and `usable_indexes()` that answers a search for a
//! term only the peer contributed, taken at that altitude and never through
//! `CorpusIndex::open` alone (`df2ffecb8`) — runs on
//! `impl PartitionMergePort for CorpusEngine` over the same two partitions,
//! in corpus-engine's `partition_merge_port_parity`.
//!
//! # What this file drives for real
//!
//! * The real queue. A `WorkQueueManager` is registered with two units, one
//!   leased and completed by each donor, so `participating_peers` is populated
//!   the way `corpus_complete_unit` populates it in production — not
//!   hand-stuffed, and not through `coordinate_merge`'s gossip FALLBACK.
//! * The real gossip load. The handoff is serialized into the `MeshStore`
//!   under `handoff:<id>`, which is where `load_handoff` reads it from.
//! * The real wire. The peer's partition arrives as a tarball over a loopback
//!   socket through `fetch_remote_shard`'s `GET /internal/index/serve` →
//!   `tar xf`, from `merge_participants_coverage`'s fixture.
//!
//! # What this file does NOT check
//!
//! * **The legacy static-partition mode.** `handoff.partitions` is empty here
//!   by construction, which is what makes it queue mode. The legacy branch —
//!   the peer-status poll and the lowest-`NodeId` leader rule — is untested
//!   before this file and still is.
//! * **Two machines.** One process, one index dir plus a staging dir; the peer
//!   is a socket. Clocks, loss and partial transfers are out of scope, as in
//!   B2.
//! * **The `Ok(None)` non-leader arm**, and the gossip fallback used when the
//!   live queue snapshot is gone. Both are `coordinate_merge` behaviours this
//!   bar does not speak to.
//!
//! The fixture is `merge_participants_coverage`'s, reused rather than
//! re-derived (ARCH §19) — including its `NodeId` HIGH-byte hazard note, which
//! applies here verbatim because the partition directory names are the same.

use std::sync::Arc;

use kernel_types::HandoffId;
use oicp_types::work_queue::{CompleteOutcome, HandoffPhase, IngestionHandoff, WorkUnit};
use oicp_types::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
use sovereign_contracts::peer::ReplicatedKv;
use sovereign_grants::{ShardManager, WorkQueueManager};

use super::merge_participants_coverage::{fixture, Fixture, CORPUS};

/// Both ends of a collaborative ingest must agree on this exactly; the value
/// itself is inert here because nothing embeds.
fn embed_model() -> EmbedModelInfo {
    EmbedModelInfo {
        model_id: "test-model".into(),
        dimensions: 8,
        pooling: PoolingStrategy::Mean,
        normalization: NormalizationStrategy::Application,
        query_instruction_prefix: String::new(),
    }
}

/// Put the fixture into the state `corpus_complete_unit` hands
/// `coordinate_merge`: a queue-mode handoff in gossip, and a live queue whose
/// units are all terminal with both donors recorded as participants.
///
/// Returns the handoff id and a manager with the queue attached — the
/// PREFERRED participant source, so this bar is not measured through the
/// coordinator-restart gossip fallback.
async fn queue_mode_handoff(f: &Fixture) -> (HandoffId, ShardManager) {
    let handoff_id = HandoffId::from_u128(0xB8);

    // Queue mode is `partitions.is_empty() && phase != legacy`
    // (`IngestionHandoff::is_queue_mode`). `new_queue` gives exactly that; the
    // id is overridden so the gossip key and the queue key are the same one.
    let mut handoff = IngestionHandoff::new_queue(
        CORPUS,
        "test-recipe",
        embed_model(),
        f.local,
        corpus_engine_yield::time::unix_millis(),
    );
    handoff.handoff_id = handoff_id;
    handoff.phase = HandoffPhase::Merging;
    assert!(
        handoff.is_queue_mode(),
        "the fixture must exercise the QUEUE-mode branch of coordinate_merge",
    );
    f.mesh_store
        .set(
            "corpus-engine",
            &format!("handoff:{handoff_id}"),
            bytes::Bytes::from(serde_json::to_vec(&handoff).expect("a handoff serializes")),
            f.local,
        )
        .expect("seed the handoff into gossip");

    // Two units, one per donor, leased and completed through the queue's own
    // API. `next_unit` is what inserts a peer into `participating_peers`
    // (`work_queue.rs`), so going through it is what makes this the real
    // participant set rather than a hand-built one.
    let queue = Arc::new(WorkQueueManager::new());
    queue
        .register(
            handoff_id,
            CORPUS,
            "test-recipe",
            embed_model(),
            vec![
                WorkUnit::JsonlRange { start: 0, end: 1 },
                WorkUnit::JsonlRange { start: 1, end: 2 },
            ],
            f.local,
            None,
        )
        .await;
    for donor in [f.reachable_peer, f.local] {
        let leased = queue
            .next_unit(&handoff_id, donor)
            .await
            .expect("the queue leases to an unrestricted donor")
            .expect("two units, two donors — neither lease is empty");
        queue
            .complete_unit(
                &handoff_id,
                donor,
                leased.unit_id,
                CompleteOutcome::Complete,
                None,
            )
            .await
            .expect("a live lease completes");
    }
    let snapshot = queue
        .snapshot(&handoff_id)
        .await
        .expect("the queue we just registered");
    assert!(
        snapshot.all_terminal(),
        "coordinate_merge is only triggered once every unit is terminal",
    );
    assert!(
        snapshot.participating_peers.contains(&f.reachable_peer),
        "the peer donor must be in the participant set the merge reads",
    );

    let manager = ShardManager::new(f.port.clone(), f.mesh_store.clone()).with_work_queue(queue);

    (handoff_id, manager)
}

/// **B8, grants' half.** A queue-mode merge, driven through
/// `coordinate_merge`, hands ingest both donors' partitions — the local one
/// from disk, the peer's as it arrived over the wire — into the canonical,
/// and then asks ingest to finalize it. The finalize is what
/// `installed_indexes()`, `usable_indexes()` and `hosted_corpora` gossip gate
/// on; that the pair lands a reachable corpus is the engine half.
///
/// Failing input, named: drop the finalize from `merge_participants` (the
/// state before cw-lift 5g B8) and `port_acts` stops at `merge_partitions`.
#[tokio::test]
async fn a_queue_mode_merge_hands_ingest_both_partitions_then_the_finalize() {
    let f = fixture().await;
    let (handoff_id, manager) = queue_mode_handoff(&f).await;

    let outcome = manager
        .coordinate_merge(handoff_id, f.local, &f.peer_urls, None)
        .await;

    // Fixture check first, so a broken harness cannot be misread as the bar
    // failing. `Ok(None)` here would mean "not the merge leader" or "no
    // resolvable participants" — neither is the subject.
    outcome
        .as_ref()
        .expect("the queue-mode merge itself must not error")
        .as_ref()
        .expect("this node IS the merge leader for the seeded handoff");

    let merges = f.merges();
    assert_eq!(merges.len(), 1, "one merge for the handoff: {merges:?}");
    let mut inputs = merges[0].inputs.clone();
    inputs.sort();
    let mut want = vec![
        (f.partition_of(f.local), 1),
        (f.partition_of(f.reachable_peer), 1),
    ];
    want.sort();
    assert_eq!(
        inputs, want,
        "both donors' partitions, one row each: `alpha` from this node's \
         disk, `bravo` from the peer's tarball. The participant set came from \
         the live queue, not from gossip.",
    );
    assert_eq!(merges[0].output, f.canonical());
    assert_eq!(
        f.port_acts(),
        vec!["merge_partitions", "finalize_canonical"],
        "a queue-mode merge must end in the finalize, or it writes chunks \
         that no surface can see — the gap `coordinate_merge` had until B8",
    );
    assert_eq!(f.finalized(), vec![CORPUS.to_string()]);
}
