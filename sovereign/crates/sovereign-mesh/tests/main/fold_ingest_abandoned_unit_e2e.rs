// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B5 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: a donor that never
//! reports is named, not silently dropped.**
//!
//! The bar has two halves and they do not both hold. The merge half does: a
//! handoff carrying a terminal `Failed` unit still merges what exists, and the
//! fold names the slice that will never arrive. The corpus half does not. The
//! only place that fact is recorded is a `WARN` in `auto_ingest`'s tick — the
//! canonical's own `_corpus_meta.json` is identical, field for field, to one
//! built from a handoff that delivered everything, and so is every `IndexInfo`
//! field `build_hosted_corpora` copies onto the wire.
//!
//! Each half is a separate reading below, and the second is `#[ignore]`d with
//! B5's number on it — the same marker `50238f364` used for B1's red, and for
//! the same reason: an unmet bar is not made met by leaving its measurement
//! out of the suite.
//!
//! The fixture, and why the abandoned unit is not hand-built
//! ---------------------------------------------------------
//! `WorkUnitStatus::Failed { outcome: None }` — attempts spent, nobody ever
//! reported — is DERIVED, not written: `ProjectedUnit::status_at`
//! (`commonwealth-work/src/projection.rs:237`) reads a lapsed lease back as
//! terminal once `MAX_UNIT_ATTEMPTS` of them have gone by. So the third unit
//! here is leased three times by real signed `Lease` acts, each after the
//! previous lease's `LEASE_MS` window closed, and never reported at all. A
//! hand-written status would prove that `fold_coverage_for` matches on an enum
//! variant; this proves that the journal a mesh actually accumulates produces
//! that variant.
//!
//! What this file reuses rather than rebuilds (ARCH §19)
//! ----------------------------------------------------
//! Every fixture primitive — the ring, the signer, the sealed `ingest:v1`
//! units, the partition writer, the two-node `AppState` pair, the canonical
//! probe — is `fold_ingest_cross_node_merge_e2e`'s, shared through the one
//! test binary rather than copied. Only the act sequence that abandons a unit
//! and the gossip reading are new.
//!
//! What this file does NOT check
//! -----------------------------
//! * Two machines. Same caveat as B2: two index dirs, two `AppState`s and two
//!   sockets in one process.
//! * The ingest pipeline. No recipe, no acquirer, no embedder.
//! * `auto_ingest`'s tick loop — the arm that emits the partial WARN is driven
//!   in `fold_ingest_coverage_refusal_e2e`, for B7's `continue`, not here.
//! * Whether a peer, on receiving the gossip below, would DO anything
//!   different if the advertisement did carry a completeness signal. That is
//!   the fix's bar, not this measurement's.
//!
//! The `NodeId` fixture hazard the sibling file documents applies verbatim:
//! `Display` is the first EIGHT of sixteen bytes and partition directories are
//! named from it, so the ids differ in their HIGH bytes.

use commonwealth_api::auto_recover::{merge_from_fold_coverage, RecoveryOutcome};
use commonwealth_api::server::internal_router;
use commonwealth_core::ids::HandoffId;
use commonwealth_core::knowledge::{HandoffPhase, MAX_UNIT_ATTEMPTS};
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::{Submission, WorkAct};
use corpus_engine::index::CorpusIndex;
use oicp_types::JobKind;
use sovereign_mesh::ingest_executor::{fold_coverage_for, FoldCoverage, INGEST_KIND};
use tempfile::TempDir;

use crate::common;
use crate::common::corpus_at;
use crate::fold_ingest_cross_node_merge_e2e::{
    actor, completion, ingest_unit, leader_node, node_state, peer_node, probe_canonical, ring,
    sign, unit_ref, write_donor_partition, LEADER_ONLY_TERM, PEER_ONLY_TERM,
};

/// Well past the last lease's expiry — see [`abandoned_handoff`]'s timeline.
/// The sibling file's `NOW_MS` is 400_000 and would read the third unit as
/// still leased, which is why this file carries its own.
const NOW_MS: u64 = 2_000_000;

/// The scenario: one handoff, THREE `ingest:v1` units for one corpus. Two are
/// completed by two different donors. The third is leased
/// [`MAX_UNIT_ATTEMPTS`] times and never reported — nobody ingested that
/// slice and nobody will.
///
/// The lease timeline is the load-bearing part. `LEASE_MS` is 300_000, so each
/// re-lease is taken after the previous window closed; a lease inside a live
/// one is a `LostLease` and does NOT increment `attempts`
/// (`projection.rs::lease`).
///
///     ts=100    submit (3 units)
///     ts=200    leader leases unit a           ts=300  leader completes a
///     ts=210    peer   leases unit b           ts=310  peer completes b
///     ts=400    peer   leases unit c   → attempts 1, expires 700_000
///     ts=800    leader leases unit c   → attempts 2, expires 1_100_000
///     ts=1200   peer   leases unit c   → attempts 3, expires 1_500_000
///     NOW_MS = 2_000_000                → Failed { outcome: None }
///
/// Returns the projection, the handoff and the abandoned unit's hash, and
/// asserts on the way out that the scenario really is the one described —
/// three units, one of them terminal-`Failed`-without-a-verdict, and the
/// handoff nevertheless `Complete`. If any of that stops holding, everything
/// below is answering a question nobody asked.
fn abandoned_handoff(corpus: &str) -> (WorkProjection, HandoffId, String) {
    let handoff = HandoffId::from_u128(5_000_005);
    let a = ingest_unit(corpus, 0, 0, 100);
    let b = ingest_unit(corpus, 1, 100, 200);
    let c = ingest_unit(corpus, 2, 200, 300);

    let submit = WorkAct::Submit(Submission::new(
        handoff,
        JobKind::parse(INGEST_KIND).expect("kind"),
        vec![a.clone(), b.clone(), c.clone()],
        None,
        None,
    ));

    let ops = vec![
        sign(1, 100, 0, &submit),
        sign(1, 200, 1, &WorkAct::Lease(unit_ref(handoff, &a))),
        sign(2, 210, 0, &WorkAct::Lease(unit_ref(handoff, &b))),
        sign(
            1,
            300,
            2,
            &completion(handoff, &a, corpus, leader_node(), "leader"),
        ),
        sign(
            2,
            310,
            1,
            &completion(handoff, &b, corpus, peer_node(), "peer"),
        ),
        // The three lapses. No `Complete` and no `Fail` ever follows.
        sign(2, 400, 2, &WorkAct::Lease(unit_ref(handoff, &c))),
        sign(1, 800, 3, &WorkAct::Lease(unit_ref(handoff, &c))),
        sign(2, 1200, 3, &WorkAct::Lease(unit_ref(handoff, &c))),
    ];

    let projection = WorkProjection::fold(&commonwealth_rail::admit(
        &ops,
        &[],
        &ring(),
        commonwealth_work::WORK_NAMESPACE,
        &commonwealth_rail::Ed25519Verifier,
    ));

    let h = projection
        .handoffs
        .get(&handoff)
        .expect("the submission was admitted");

    // The precondition the bar names, asserted rather than assumed: terminal
    // `Failed` with `outcome: None`, reached by spending attempts and not by
    // anybody reporting a verdict.
    match h.units[&c.unit_hash].status_at(NOW_MS) {
        WorkUnitStatus::Failed {
            attempts, outcome, ..
        } => {
            assert_eq!(
                attempts, MAX_UNIT_ATTEMPTS,
                "the third unit must be terminal because its ATTEMPTS are spent",
            );
            assert!(
                outcome.is_none(),
                "`outcome: None` is the bar's shape — nobody ever reported. A \
                 `Some` here would be the different fact `WorkUnitStatus::Failed` \
                 exists to keep apart (projection.rs:180). Got {outcome:?}",
            );
        }
        other => panic!(
            "the third unit must be terminal `Failed`; got {other:?}. Check the \
             lease timeline in this function's docs against LEASE_MS \
             ({}) and MAX_UNIT_ATTEMPTS ({MAX_UNIT_ATTEMPTS}).",
            commonwealth_core::knowledge::LEASE_MS,
        ),
    }
    assert_eq!(
        h.phase_at(NOW_MS),
        HandoffPhase::Complete,
        "the handoff is terminal — every unit settled, one of them badly. \
         A merge is reached only from here.",
    );

    (projection, handoff, c.unit_hash.clone())
}

/// Both donors' finished slices, written where the executor would put them,
/// and the leader `AppState` that can reach the peer's socket. Shared by the
/// two readings below because they need the same disk.
async fn two_donors_on_disk(
    leader_dir: &std::path::Path,
    peer_dir: &std::path::Path,
    corpus: &str,
) {
    write_donor_partition(leader_dir, leader_node(), corpus, 0, LEADER_ONLY_TERM).await;
    write_donor_partition(peer_dir, peer_node(), corpus, 1, PEER_ONLY_TERM).await;
}

// ─────────────────────────────────────────────────────────────────
// B5, first half — the fold names the slice nobody ingested
// ─────────────────────────────────────────────────────────────────

/// **B5 (first half).** The unit whose attempts are spent is REPORTED, by
/// unit hash, and the coverage says the corpus is partial. The two donors that
/// did finish still count, so the merge below has a participant set.
///
/// Absence is reported, never defaulted (ARCH §18.3). The failure this guards
/// is a coverage read that simply skips what it cannot use: `expected` would
/// still be 2, `nodes` would still be both donors, the merge would still run,
/// and nothing anywhere would say a third of the corpus does not exist.
///
/// Failing input, named and watched: delete the
/// `WorkUnitStatus::Failed { .. } => abandoned.push(unit_hash.clone())` arm in
/// `fold_coverage_for` — the `_ => {}` catch-all below it swallows the unit
/// and this goes red on `abandoned` being empty.
///
/// The paired positive is the sibling file's
/// `the_fold_names_both_verified_donors_and_where_to_find_them`, which asserts
/// `abandoned` is EMPTY on a handoff where nothing failed. Without it, a
/// `fold_coverage_for` that reported every unit as abandoned would pass here.
#[test]
fn a_unit_whose_attempts_are_spent_is_named_in_the_coverage() {
    const CORPUS: &str = "cw-lift-5g-abandoned";
    let (projection, handoff, abandoned_hash) = abandoned_handoff(CORPUS);

    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");

    assert_eq!(coverage.handoff_id, handoff);
    assert_eq!(
        coverage.abandoned,
        vec![abandoned_hash],
        "the slice nobody will ever ingest must be named BY UNIT, not counted \
         — `which slice is missing` is the question an operator has",
    );
    assert!(
        coverage.is_partial(),
        "a coverage carrying an abandoned unit is partial by definition",
    );

    // The merge still has a participant set: the two donors that did finish.
    // A coverage that refused outright would strand two thirds of a corpus
    // over a third that will never arrive.
    assert_eq!(
        coverage.expected, 2,
        "two distinct VERIFIED lessees completed units; the third unit has no \
         lessee at all and must not inflate the denominator",
    );
    let mut nodes = coverage.nodes.clone();
    nodes.sort();
    let mut want = vec![leader_node(), peer_node()];
    want.sort();
    assert_eq!(nodes, want);
}

// ─────────────────────────────────────────────────────────────────
// B5, second half of the merge clause — it proceeds with what exists
// ─────────────────────────────────────────────────────────────────

/// **B5 (merge clause).** With one slice abandoned, the merge of the two that
/// exist still runs and still lands both — over the real wire, the same way
/// B2's reading does.
///
/// This is the half of the bar that says a corpus is not held hostage by a
/// unit that will never arrive. `expected` is 2 and two shards resolve, so the
/// coverage guard in `merge_participants` passes: the abandoned unit is not a
/// missing PARTITION, it is work that never happened, and conflating the two
/// would make `PartitionsUnreachable` fire forever on a handoff no retry can
/// fix.
///
/// Failing input, named and watched: make `fold_coverage_for` count the
/// abandoned unit into `expected` (`expected: actors.len() + abandoned.len()`).
/// `merge_participants` then wants 3 shards, resolves 2, and refuses with
/// `PartitionsUnreachable { covered: 2, expected: 3 }` — no canonical at all,
/// every tick, forever.
#[tokio::test]
async fn the_merge_proceeds_with_the_slices_that_exist() {
    const CORPUS: &str = "cw-lift-5g-abandoned-merge";
    let (projection, _handoff, _abandoned) = abandoned_handoff(CORPUS);

    let leader_home = TempDir::new().expect("leader tempdir");
    let peer_home = TempDir::new().expect("peer tempdir");
    let leader_dir = leader_home.path().join("indexes");
    let peer_dir = peer_home.path().join("indexes");
    std::fs::create_dir_all(&leader_dir).expect("leader index dir");
    std::fs::create_dir_all(&peer_dir).expect("peer index dir");
    two_donors_on_disk(&leader_dir, &peer_dir, CORPUS).await;

    let peer_state = node_state(peer_node(), &peer_dir, &[]);
    let peer_addr = common::spawn_router(internal_router(peer_state)).await;
    let leader_state = node_state(
        leader_node(),
        &leader_dir,
        &[(peer_node(), &peer_addr.to_string())],
    );

    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");
    assert!(
        coverage.is_partial(),
        "precondition: this handoff is partial"
    );

    let outcome = merge_from_fold_coverage(
        &leader_state,
        CORPUS,
        coverage.handoff_id,
        &coverage.nodes,
        coverage.expected,
    )
    .await;

    let probe = probe_canonical(&leader_dir, CORPUS).await;
    assert!(
        probe.leader_term_reachable && probe.peer_term_reachable,
        "both slices that DO exist must land — an abandoned unit must not \
         cost the corpus the work that was actually done.\n\
         merge outcome              : {outcome:?}\n\
         canonical exists           : {}\n\
         canonical chunk_count      : {}\n\
         leader-only term reachable : {}\n\
         peer-only term reachable   : {}",
        probe.canonical_exists,
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
    );
    assert_eq!(probe.chunk_count, 4, "2 chunks from each of the two donors");
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { chunks: 4, .. }),
        "the merge must report the canonical it built; got {outcome:?}",
    );
}

// ─────────────────────────────────────────────────────────────────
// B5, the corpus clause — MEASURED AND NOT MET
// ─────────────────────────────────────────────────────────────────

/// The canonical's own on-disk record, with the fields that differ for
/// uninteresting reasons blanked: which corpus it is, what it is called, and
/// when it was written. What is left is everything the corpus says about
/// ITSELF.
fn corpus_record(index_dir: &std::path::Path, corpus: &str) -> serde_json::Value {
    let raw = std::fs::read_to_string(corpus_at(index_dir, corpus).meta_path())
        .unwrap_or_else(|e| panic!("{corpus} has no canonical meta: {e}"));
    let mut v: serde_json::Value = serde_json::from_str(&raw).expect("meta is json");
    for k in ["corpus_id", "corpus_name", "created_at", "last_updated"] {
        v[k] = serde_json::Value::Null;
    }
    v
}

/// The `IndexInfo` fields `build_hosted_corpora`
/// (`sovereign-mesh/src/capabilities.rs:285-310`) copies onto the wire as a
/// [`CorpusShardInfo`]. Read straight off the canonical rather than through
/// `CorpusEngine::installed_indexes`, because that walk drops this canonical
/// today — see the test's docs.
async fn advertisable(index_dir: &std::path::Path, corpus: &str) -> String {
    let info = CorpusIndex::open(&corpus_at(index_dir, corpus).root())
        .await
        .expect("open the canonical")
        .info()
        .await
        .expect("canonical info");
    format!(
        "query_sharing={} is_shard={} chunk_range={:?} chunk_count={} \
         total_shards={:?} processed_shards={:?}",
        info.query_sharing,
        info.is_shard,
        info.chunk_range,
        info.chunk_count,
        info.total_shards,
        info.processed_shards,
    )
}

/// **B5 (corpus clause) — THE BAR, AND IT IS NOT MET.**
///
/// Two corpora on one node, merged the same way from the same fold. One came
/// from a handoff whose third slice will never exist; the other from a handoff
/// that delivered everything it promised. Nothing the corpus writes down tells
/// them apart, and nothing gossip could carry does either.
///
/// The chunk counts are equal too, and that is the sharpest form of the
/// defect: a THREE-unit handoff that delivered two slices is byte-identical to
/// a TWO-unit handoff that delivered both of its own. A reading that compared
/// a 1-of-2 corpus against a 2-of-2 one would find different chunk counts and
/// could be talked into calling that a signal; it is not one, because no peer
/// knows what the count should have been.
///
/// The bar's words are "the corpus must record that it is partial". Today the
/// only record is a `tracing::warn!` in `auto_ingest`'s arm — process-local,
/// gone on the next restart, and invisible to every peer. `CorpusShardInfo`
/// carries `total_shards` / `processed_shards`, which is the shape a
/// completeness signal would take, and `build_hosted_corpora` reads both off
/// `IndexInfo` — which comes from the meta, which is stamped only by
/// `ExtractorConfig::WikipediaJsonl` (`corpus-engine/src/engine/ingest.rs:718`).
/// So for every fold-sliced recipe both are `None`/`[]` on the whole corpus
/// and on the partial one alike.
///
/// A SEPARATE DEFECT FOUND WHILE MEASURING THIS, NOT FIXED HERE, AND THE
/// REASON THE ADVERTISEMENT IS READ OFF THE INDEX RATHER THAN OFF A GOSSIP
/// ROUND. `merge_participants` builds its output through
/// `CorpusEngine::merge_partitions` → `sharding::merge_shards`, which creates
/// the canonical with `CorpusIndex::create` and never clears
/// `ingestion_in_progress` — unlike `merge_partitions_into_canonical`, which
/// does (`sharding.rs:1424`) and is what the DISK path uses. So a canonical
/// built by the fold path carries `ingestion_in_progress: true,
/// indexes_built: false`, and `CorpusEngine::installed_indexes` skips it: it
/// is advertised to nobody and `usable_indexes` will not search it either.
/// Measured, not inferred — `installed_indexes()` returned zero rows on the
/// disk this test builds. That is a bigger problem than the one this bar is
/// about and it belongs to whoever owns the merge's post-conditions.
///
/// IGNORED, not deleted, and not softened into a test of the current
/// behaviour. Asserting the indistinguishability would pass forever and go red
/// the day somebody fixes it — the exact inversion §18 is about. This is the
/// watched red for an OPEN bar, marked the way `50238f364` marked B1's.
/// Recorded as a miss in `quality/campaigns/cw-lift-5g-part2-prereg.md`.
#[tokio::test]
#[ignore = "B5's corpus clause is MEASURED AND NOT MET (cw-lift 5g part 2): a \
            canonical missing a slice nobody will ever ingest records nothing \
            that says so, and carries nothing gossip could. The ignore marks an \
            open bar, not a passing one — remove it with the fix, not before."]
async fn a_corpus_missing_an_abandoned_slice_records_nothing_that_says_so() {
    const PARTIAL: &str = "cw-lift-5g-record-partial";
    const WHOLE: &str = "cw-lift-5g-record-whole";

    let leader_home = TempDir::new().expect("leader tempdir");
    let peer_home = TempDir::new().expect("peer tempdir");
    let leader_dir = leader_home.path().join("indexes");
    let peer_dir = peer_home.path().join("indexes");
    std::fs::create_dir_all(&leader_dir).expect("leader index dir");
    std::fs::create_dir_all(&peer_dir).expect("peer index dir");

    two_donors_on_disk(&leader_dir, &peer_dir, PARTIAL).await;
    two_donors_on_disk(&leader_dir, &peer_dir, WHOLE).await;

    // One peer, one socket, both partitions — the peer serves whichever corpus
    // the leader asks for.
    let peer_state = node_state(peer_node(), &peer_dir, &[]);
    let peer_addr = common::spawn_router(internal_router(peer_state)).await;
    let leader_state = node_state(
        leader_node(),
        &leader_dir,
        &[(peer_node(), &peer_addr.to_string())],
    );

    // The partial corpus: three units, one abandoned.
    let (partial_proj, _h, _u) = abandoned_handoff(PARTIAL);
    let partial_cov = fold_coverage_for(&partial_proj, &actor(1), PARTIAL, NOW_MS)
        .expect("the submitter leads the partial handoff");
    assert!(
        partial_cov.is_partial(),
        "precondition: the fold KNOWS this corpus is partial — which is what \
         makes the silence below a loss of information rather than an absence \
         of it",
    );
    merge_one(&leader_state, PARTIAL, &partial_cov).await;

    // The whole corpus: two units, both delivered. Same shape on disk.
    let (whole_proj, _h2) = crate::fold_ingest_cross_node_merge_e2e::terminal_handoff(WHOLE);
    let whole_cov = fold_coverage_for(&whole_proj, &actor(1), WHOLE, 400_000)
        .expect("the submitter leads the whole handoff");
    assert!(!whole_cov.is_partial(), "control: nothing abandoned here");
    merge_one(&leader_state, WHOLE, &whole_cov).await;

    let partial_record = corpus_record(&leader_dir, PARTIAL);
    let whole_record = corpus_record(&leader_dir, WHOLE);
    // Computed before the assertion rather than after it, so the wire half is
    // exercised on every run and reported in the same breath. `IndexInfo` is
    // derived from the meta, so a meta that says nothing cannot produce a wire
    // record that does — but stating both is what makes the claim checkable
    // rather than inferred.
    let partial_ad = advertisable(&leader_dir, PARTIAL).await;
    let whole_ad = advertisable(&leader_dir, WHOLE).await;

    assert_ne!(
        partial_record,
        whole_record,
        "B5's corpus clause: the corpus must RECORD that it is partial. Its \
         `_corpus_meta.json` is identical, field for field, to one built from a \
         handoff that delivered everything — so the fact that a slice will \
         never exist survives only as a WARN in one process's log.\n\
         \n\
         The fold knew: abandoned={:?}.\n\
         \n\
         record (both):\n{}\n\
         \n\
         and nothing gossip could carry tells them apart either — these are \
         exactly the `IndexInfo` fields `build_hosted_corpora` copies into a \
         `CorpusShardInfo`:\n\
           partial : {partial_ad}\n\
           whole   : {whole_ad}\n\
         \n\
         See `quality/campaigns/cw-lift-5g-part2-prereg.md` B5.",
        partial_cov.abandoned,
        serde_json::to_string_pretty(&partial_record).expect("render"),
    );
}

/// Merge one corpus from a coverage, asserting the merge itself succeeded —
/// a precondition for the gossip reading, not the reading's own bar.
async fn merge_one(state: &commonwealth_api::state::AppState, corpus: &str, cov: &FoldCoverage) {
    let outcome =
        merge_from_fold_coverage(state, corpus, cov.handoff_id, &cov.nodes, cov.expected).await;
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "precondition for the gossip reading: {corpus} must have a canonical \
         to advertise. Got {outcome:?}",
    );
}
