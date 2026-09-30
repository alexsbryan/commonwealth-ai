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
//! units, the partition writer, the two-node `AppState` pair, the merge port
//! double and the slices it records — is `fold_ingest_cross_node_merge_e2e`'s,
//! shared through the one test binary rather than copied. Only the act
//! sequence that abandons a unit and the corpus-clause reading are new. As
//! there, these readings assert what the daemon hands ingest's
//! `PartitionMergePort`; the engine half is corpus-engine's
//! `fold_merge_port_parity` (pb-ingest-dial-daemon-tests-merge).
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

use kernel_types::HandoffId;
use oicp_types::work_queue::{HandoffPhase, MAX_UNIT_ATTEMPTS};
use oicp_types::work::projection::{WorkProjection, WorkUnitStatus};
use oicp_types::work::{Submission, WorkAct};
use oicp_types::JobKind;
use sovereign_daemon::ingest_executor::{fold_coverage_for, FoldCoverage, INGEST_KIND};
use sovereign_daemon::server::internal_router;
use sovereign_grants::auto_recover::{merge_from_fold_coverage, RecoveryOutcome};
use tempfile::TempDir;

use crate::common;
use crate::fold_ingest_cross_node_merge_e2e::{
    actor, completion, ingest_unit, leader_node, node_port, node_state, peer_node, sign, unit_ref,
    work_rails, write_donor_partition, Slice, LEADER_ONLY_TERM, LEADER_SLICE, PEER_ONLY_TERM,
    PEER_SLICE,
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
async fn abandoned_handoff(corpus: &str) -> (WorkProjection, HandoffId, String) {
    let rails = work_rails().await;
    let handoff = HandoffId::from_u128(5_000_005);
    let a = ingest_unit(&rails, corpus, 0, 0, 100).await;
    let b = ingest_unit(&rails, corpus, 1, 100, 200).await;
    let c = ingest_unit(&rails, corpus, 2, 200, 300).await;

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

    rails.ingest(&ops).await;
    let projection = rails.projection().await;

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
            oicp_types::work_queue::LEASE_MS,
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
#[tokio::test]
async fn a_unit_whose_attempts_are_spent_is_named_in_the_coverage() {
    const CORPUS: &str = "cw-lift-5g-abandoned";
    let (projection, handoff, abandoned_hash) = abandoned_handoff(CORPUS).await;

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

/// **B5 (merge clause), the daemon's half.** With one slice abandoned, the
/// merge of the two that exist still runs: ingest is handed both — over the
/// real wire, the same way B2's reading does — and then the finalize.
///
/// This is the half of the bar that says a corpus is not held hostage by a
/// unit that will never arrive. `expected` is 2 and two shards resolve, so the
/// coverage guard in `merge_participants` passes: the abandoned unit is not a
/// missing PARTITION, it is work that never happened, and conflating the two
/// would make `PartitionsUnreachable` fire forever on a handoff no retry can
/// fix. That the two slices ingest is handed land in one reachable canonical
/// is the engine half, in corpus-engine's `fold_merge_port_parity`.
///
/// Failing input, named and watched: make `fold_coverage_for` count the
/// abandoned unit into `expected` (`expected: actors.len() + abandoned.len()`).
/// `merge_participants` then wants 3 shards, resolves 2, and refuses with
/// `PartitionsUnreachable { covered: 2, expected: 3 }` — ingest is handed
/// nothing, every tick, forever.
#[tokio::test]
async fn the_merge_proceeds_with_the_slices_that_exist() {
    const CORPUS: &str = "cw-lift-5g-abandoned-merge";
    let (projection, _handoff, _abandoned) = abandoned_handoff(CORPUS).await;

    let leader_home = TempDir::new().expect("leader tempdir");
    let peer_home = TempDir::new().expect("peer tempdir");
    let leader_dir = leader_home.path().join("indexes");
    let peer_dir = peer_home.path().join("indexes");
    std::fs::create_dir_all(&leader_dir).expect("leader index dir");
    std::fs::create_dir_all(&peer_dir).expect("peer index dir");
    two_donors_on_disk(&leader_dir, &peer_dir, CORPUS).await;

    let peer = node_port(&peer_dir);
    let peer_state = node_state(peer_node(), &peer, &[]);
    let peer_addr = common::spawn_router(internal_router(peer_state)).await;
    let leader = node_port(&leader_dir);
    let leader_state = node_state(
        leader_node(),
        &leader,
        &[(peer_node(), &peer_addr.to_string())],
    );

    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");
    assert!(
        coverage.is_partial(),
        "precondition: this handoff is partial"
    );

    let node = sovereign_daemon::routes_internal::fold_recovery(&leader_state).await;
    let outcome = merge_from_fold_coverage(
        node,
        CORPUS,
        coverage.handoff_id,
        &coverage.nodes,
        coverage.expected,
    )
    .await;

    let mut slices: Vec<Slice> = leader.merges().into_iter().flat_map(|(s, _)| s).collect();
    slices.sort_by_key(|s| s.peer_term);
    assert_eq!(
        slices,
        vec![LEADER_SLICE, PEER_SLICE],
        "both slices that DO exist must reach ingest — an abandoned unit must \
         not cost the corpus the work that was actually done.\n\
         merge outcome : {outcome:?}\n\
         port acts     : {:?}",
        leader.merge_acts(),
    );
    assert_eq!(
        leader.merge_acts(),
        vec!["merge_partitions", "finalize_canonical"]
    );
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { chunks: 4, .. }),
        "the merge must report the canonical it built; got {outcome:?}",
    );
}

// ─────────────────────────────────────────────────────────────────
// B5, the corpus clause — MEASURED AND NOT MET
// ─────────────────────────────────────────────────────────────────

/// **B5 (corpus clause) — THE BAR, AND IT IS NOT MET.**
///
/// Two corpora on one node, merged the same way from the same fold. One came
/// from a handoff whose third slice will never exist; the other from a handoff
/// that delivered everything it promised. Nothing the daemon hands ingest
/// tells them apart, so nothing ingest writes down can either.
///
/// The bar's words are "the corpus must record that it is partial". Today the
/// only record is a `tracing::warn!` in `auto_ingest`'s arm — process-local,
/// gone on the next restart, and invisible to every peer. Ingest records what
/// its merge port is handed, and the port carries slices, an output dir and a
/// corpus id; the fold's `abandoned` reaches none of them. Measured on the
/// engine before this split (cw-lift 5g part 2): the two canonicals'
/// `_corpus_meta.json` agreed field for field, their `IndexInfo` (the fields
/// `build_hosted_corpora` copies into a `CorpusShardInfo`) agreed, and so did
/// their `canonical_fingerprint`, so a peer comparing fingerprints reads a
/// 2-of-3 corpus and a 2-of-2 corpus as the same corpus.
///
/// The chunk counts are equal too, and that is the sharpest form of the
/// defect: a THREE-unit handoff that delivered two slices is byte-identical to
/// a TWO-unit handoff that delivered both of its own. No peer knows what the
/// count should have been.
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
    let peer = node_port(&peer_dir);
    let peer_state = node_state(peer_node(), &peer, &[]);
    let peer_addr = common::spawn_router(internal_router(peer_state)).await;
    let leader = node_port(&leader_dir);
    let leader_state = node_state(
        leader_node(),
        &leader,
        &[(peer_node(), &peer_addr.to_string())],
    );

    // The partial corpus: three units, one abandoned.
    let (partial_proj, _h, _u) = abandoned_handoff(PARTIAL).await;
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
    let (whole_proj, _h2) = crate::fold_ingest_cross_node_merge_e2e::terminal_handoff(WHOLE).await;
    let whole_cov = fold_coverage_for(&whole_proj, &actor(1), WHOLE, 400_000)
        .expect("the submitter leads the whole handoff");
    assert!(!whole_cov.is_partial(), "control: nothing abandoned here");
    merge_one(&leader_state, WHOLE, &whole_cov).await;

    // What ingest was handed for each, the corpus id aside: the slices of its
    // merge. The finalize carries the id alone.
    let merges = leader.merges();
    assert_eq!(merges.len(), 2, "one merge per corpus: {merges:?}");
    assert_eq!(
        leader.finalized(),
        vec![PARTIAL.to_string(), WHOLE.to_string()]
    );
    let (partial_handed, whole_handed) = (&merges[0].0, &merges[1].0);

    assert_ne!(
        partial_handed, whole_handed,
        "B5's corpus clause: the corpus must RECORD that it is partial, and \
         ingest records only what its port is handed. The daemon handed it the \
         same slices for a corpus with a slice that will never exist as for one \
         that delivered everything — so the fact survives only as a WARN in one \
         process's log.\n\
         \n\
         The fold knew: abandoned={:?}.\n\
         \n\
         See `quality/campaigns/cw-lift-5g-part2-prereg.md` B5.",
        partial_cov.abandoned,
    );
}

/// Merge one corpus from a coverage, asserting the merge itself succeeded —
/// a precondition for the reading, not the reading's own bar.
async fn merge_one(state: &sovereign_daemon::state::AppState, corpus: &str, cov: &FoldCoverage) {
    let node = sovereign_daemon::routes_internal::fold_recovery(state).await;
    let outcome =
        merge_from_fold_coverage(node, corpus, cov.handoff_id, &cov.nodes, cov.expected).await;
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "precondition for the reading: {corpus} must have a canonical. Got \
         {outcome:?}",
    );
}
