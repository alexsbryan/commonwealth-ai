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
//! # The altitude is the bar
//!
//! The reading is taken through `CorpusEngine::installed_indexes()`,
//! `usable_indexes()` and `open_index_for_corpus()` + a search — never through
//! `CorpusIndex::open` on the canonical path, which bypasses both gates and is
//! exactly what made B2's first reading wrong (`df2ffecb8`). The
//! `CorpusIndex::open` probe is kept BESIDE it, not instead of it, because
//! "the peer's chunks never arrived" and "the chunks arrived and nothing can
//! route to them" are different defects that one reading conflates.
//!
//! An unasked question is reported as `None`, never as `false` (ARCH §18.3):
//! when the corpus is missing from `usable_indexes()` the search never runs,
//! and that is a different fact from a search that ran and missed.
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

use commonwealth_core::ids::HandoffId;
use commonwealth_core::knowledge::{CompleteOutcome, HandoffPhase, IngestionHandoff, WorkUnit};
use commonwealth_core::oicp::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
use commonwealth_knowledge::{ShardManager, WorkQueueManager};
use corpus_engine::{Corpus, CorpusIndex};

use super::merge_participants_coverage::{embedding, fixture, Fixture, CORPUS};

/// The row written into the local donor's partition dir by the fixture.
const LOCAL_ONLY_TERM: &str = "alpha";
/// The row that exists ONLY inside the tarball the peer serves.
const PEER_ONLY_TERM: &str = "bravo";

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
    let mut handoff = IngestionHandoff::new_queue(CORPUS, "test-recipe", embed_model(), f.local);
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

    let manager = ShardManager::new(
        Arc::clone(&f.engine),
        f.index_dir.clone(),
        Arc::clone(&f.mesh_store),
    )
    .with_work_queue(queue);

    (handoff_id, manager)
}

/// What a USER — or a peer reading `hosted_corpora` — can see.
///
/// * `installed` is `installed_indexes()`, the list
///   `sovereign_mesh::capabilities` hands to `build_hosted_corpora`. A corpus
///   missing from it is advertised to nobody.
/// * `usable` is `usable_indexes()`, corpus-engine's own single decider for
///   "can I search it".
/// * The two `Option<bool>`s are `None` when the corpus never reached
///   `usable_indexes()` and the search therefore never ran.
#[derive(Debug)]
struct InstalledProbe {
    installed: Vec<String>,
    usable: Vec<String>,
    dirs_on_disk: Vec<String>,
    local_term_reachable: Option<bool>,
    peer_term_reachable: Option<bool>,
}

async fn probe_installed(f: &Fixture) -> InstalledProbe {
    let ids = |rows: Vec<corpus_engine::IndexInfo>| -> Vec<String> {
        rows.into_iter().map(|i| i.corpus_id).collect()
    };
    let installed = ids(f
        .engine
        .installed_indexes()
        .await
        .expect("installed_indexes must not fail on a temp dir this test owns"));
    let usable = ids(f
        .engine
        .usable_indexes()
        .await
        .expect("usable_indexes must not fail on a temp dir this test owns"));

    let mut dirs_on_disk: Vec<String> = std::fs::read_dir(&f.index_dir)
        .expect("index dir")
        .flatten()
        .filter(|e| Corpus::meta_in(e.path()).exists())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    dirs_on_disk.sort();

    let (local_term_reachable, peer_term_reachable) = if usable.iter().any(|c| c == CORPUS) {
        let index = f
            .engine
            .open_index_for_corpus(CORPUS)
            .await
            .expect("a corpus usable_indexes() listed must open");
        (
            Some(term_reachable(&index, LOCAL_ONLY_TERM).await),
            Some(term_reachable(&index, PEER_ONLY_TERM).await),
        )
    } else {
        (None, None)
    };

    InstalledProbe {
        installed,
        usable,
        dirs_on_disk,
        local_term_reachable,
        peer_term_reachable,
    }
}

/// One spelling of "reachable", so it cannot come to mean two different things
/// at the two altitudes below.
async fn term_reachable(index: &CorpusIndex, term: &str) -> bool {
    index
        .search(&embedding(1.0), term, 10)
        .await
        .map(|hits| hits.iter().any(|h| h.content.contains(term)))
        .unwrap_or(false)
}

/// The CORROBORATING reading: what the canonical directory holds, opened by
/// path. It deliberately answers a different question from [`InstalledProbe`]
/// — "are the bytes there" rather than "can anyone reach them" — and printing
/// both is what tells a missing pull apart from a missing finalize.
#[derive(Debug)]
struct CanonicalProbe {
    exists: bool,
    chunk_count: u64,
    local_term_on_disk: bool,
    peer_term_on_disk: bool,
}

async fn probe_canonical(f: &Fixture) -> CanonicalProbe {
    let path = Corpus::named(&f.index_dir, CORPUS)
        .expect("non-empty corpus id")
        .root();
    let Ok(index) = CorpusIndex::open(&path).await else {
        return CanonicalProbe {
            exists: false,
            chunk_count: 0,
            local_term_on_disk: false,
            peer_term_on_disk: false,
        };
    };
    CanonicalProbe {
        exists: true,
        chunk_count: index.info().await.expect("canonical info").chunk_count,
        local_term_on_disk: term_reachable(&index, LOCAL_ONLY_TERM).await,
        peer_term_on_disk: term_reachable(&index, PEER_ONLY_TERM).await,
    }
}

/// **B8.** A queue-mode merge, driven through `coordinate_merge`, yields a
/// corpus in `installed_indexes()` and `usable_indexes()` that answers a query
/// for a term only the remote donor contributed.
#[tokio::test]
async fn a_queue_mode_merge_lands_a_corpus_a_user_can_reach() {
    let f = fixture().await;
    let (handoff_id, manager) = queue_mode_handoff(&f).await;

    let outcome = manager
        .coordinate_merge(handoff_id, f.local, &f.peer_urls)
        .await;

    // Fixture check first, so a broken harness cannot be misread as the bar
    // failing. `Ok(None)` here would mean "not the merge leader" or "no
    // resolvable participants" — neither is the subject.
    let merged = outcome
        .as_ref()
        .expect("the queue-mode merge itself must not error")
        .as_ref()
        .expect("this node IS the merge leader for the seeded handoff");

    let seen = probe_installed(&f).await;
    let disk = probe_canonical(&f).await;

    assert_eq!(
        seen.peer_term_reachable,
        Some(true),
        "a queue-mode merge did not produce a corpus a user can reach.\n\
         \n\
         `peer-only term (installed)` is `None` when the corpus never reached \
         `usable_indexes()` — the search was never asked, because nothing \
         routes to a canonical that `installed_indexes()` skips on \
         `is_ingestion_complete`. That is also the list `hosted_corpora` \
         gossip is built from, so such a corpus is advertised to no peer \
         either. The disk rows below say whether the bytes are there.\n\
         \n\
         merge returned              : chunks={} corpus={}\n\
         installed_indexes()         : {:?}\n\
         usable_indexes()            : {:?}\n\
         canonical dirs on disk      : {:?}\n\
         local-only term (installed) : {:?}\n\
         peer-only term (installed)  : {:?}\n\
         --- through CorpusIndex::open, which bypasses both gates ---\n\
         canonical exists            : {}\n\
         canonical chunk_count       : {} (expected 2: 1 local + 1 peer)\n\
         local-only term (on disk)   : {}\n\
         peer-only term (on disk)    : {}\n\
         \n\
         See `quality/campaigns/cw-lift-5g-part2-prereg.md` B8.",
        merged.chunk_count,
        merged.corpus_id,
        seen.installed,
        seen.usable,
        seen.dirs_on_disk,
        seen.local_term_reachable,
        seen.peer_term_reachable,
        disk.exists,
        disk.chunk_count,
        disk.local_term_on_disk,
        disk.peer_term_on_disk,
    );
    assert_eq!(
        seen.local_term_reachable,
        Some(true),
        "the LOCAL donor's chunk is unreachable through `usable_indexes()`, \
         which is a different and worse defect than the one this bar is \
         about. installed: {:?} | usable: {:?}",
        seen.installed,
        seen.usable,
    );
    assert!(
        seen.installed.iter().any(|c| c == CORPUS),
        "the canonical is absent from `installed_indexes()`, so \
         `build_hosted_corpora` advertises it to no peer on the mesh. \
         installed: {:?} | dirs on disk: {:?}",
        seen.installed,
        seen.dirs_on_disk,
    );
    assert!(
        seen.usable.iter().any(|c| c == CORPUS),
        "the canonical is absent from `usable_indexes()`, so no local query \
         routes to it. usable: {:?} | installed: {:?}",
        seen.usable,
        seen.installed,
    );
    assert_eq!(
        disk.chunk_count, 2,
        "both donors' rows must land: 1 each. A reachable peer term with the \
         wrong count is a partial merge wearing a passing search.",
    );
    assert_eq!(
        merged.chunk_count, 2,
        "the merge must report the canonical it actually built",
    );
}
