// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B7 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: a partial canonical
//! is never advertised as complete.**
//!
//! B7 was minted after B1 ran, because B1's red found a hazard wider than the
//! rung: `auto_recover`'s older coverage guard arms only when a partition meta
//! stamps `total_shards`, and `corpus-engine/src/engine/ingest.rs:718` stamps
//! that for `ExtractorConfig::WikipediaJsonl` and nothing else. A fold unit
//! slices ANY recipe across donors, so every recipe is multi-shard the moment
//! the fold is used, and `df2ffecb8` is what puts the fold on the main road.
//!
//! Three things are unmeasured and each is a reading here.
//!
//! 1. **The guard is armed on the FOLD path specifically.**
//!    [`a_two_donor_fold_missing_its_peer_refuses_and_writes_no_canonical`].
//!    `c001e3634` already pins `merge_participants`' refusal at the unit
//!    level; what is new is that a real two-donor fold whose peer is
//!    unreachable produces `PartitionsUnreachable` and leaves NO canonical on
//!    disk — with a paired positive in the same test showing the same
//!    scenario merging a 1-of-2 canonical the moment the bar is dropped.
//!
//! 2. **The `continue` in `auto_ingest`'s arm is load-bearing.**
//!    [`the_folds_refusal_is_final_and_the_disk_path_never_runs`]. Driven
//!    through the real `spawn_auto_collaborate_loop`, because the claim is
//!    about the ORDER of that loop's arms and nothing smaller can see it. Its
//!    control is the same disk with no fold to speak for it, which does fall
//!    through and does publish the partial canonical.
//!
//! 3. **The `total_shards` premise.**
//!    [`the_older_disk_guard_is_dark_without_a_total_shards_stamp`]. The
//!    premise B7 rests on, measured rather than cited: a partition written by
//!    anything but the Wikipedia extractor carries no `total_shards`, and the
//!    older guard therefore never arms — while the SAME call on the SAME disk
//!    does refuse once the field is stamped.
//!
//! What this file does NOT check
//! -----------------------------
//! * Two machines. Same caveat as B2 and B5.
//! * The ingest pipeline: no recipe, no acquirer, no embedder, so the
//!   `total_shards` stamp is applied by hand in reading 3 rather than by
//!   running `ExtractorConfig::WikipediaJsonl`. What the reading pins is the
//!   guard's response to the field, plus a `grep`-level citation for who
//!   writes it.
//! * Concurrency: reading 2 drives ONE node's loop. Two nodes ticking at once
//!   is B3's subject (the leader decision) and is not re-measured here.
//! * Whether the refusal is retried usefully. `auto_ingest` logs "retrying
//!   next tick" and this file watches one tick.
//!
//! The `NodeId` fixture hazard the sibling files document applies verbatim.

use std::sync::{Arc, Mutex};

use sovereign_api::auto_recover::{
    merge_from_fold_coverage, try_recover_stranded_partitions, RecoveryOutcome,
};
use sovereign_api::state::AppState;
use commonwealth_rail::{RingRail, SigningKey};
use commonwealth_work::WORK_NAMESPACE;
use corpus_engine::index::CorpusIndex;
use corpus_engine::Corpus;
use sovereign_mesh::ingest_executor::fold_coverage_for;
use tempfile::TempDir;

use crate::common::corpus_at;
use crate::fold_ingest_cross_node_merge_e2e::{
    actor, key, leader_node, node_state, peer_node, probe_canonical, ring, terminal_handoff,
    terminal_handoff_ops, write_donor_partition, LEADER_ONLY_TERM,
};

/// The instant the in-memory readings ask the fold about. Same as the sibling
/// file's, and for the same reason: past every act in the fixture.
const NOW_MS: u64 = 400_000;

/// An address nothing is listening on. Bound and released, so the port is real
/// and unused rather than guessed — a guessed port that happened to be live
/// would turn this file's refusals into transfers of somebody else's bytes.
async fn dead_peer_addr() -> std::net::SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = l.local_addr().expect("local addr");
    drop(l);
    addr
}

/// Two index dirs, the leader's slice on disk and the peer's slice
/// deliberately NOT reachable — the peer is a mesh member at a dead address.
///
/// Only the leader's partition is written. The peer's slice exists in the
/// journal and nowhere this node can reach, which is exactly the shape the
/// coverage guard is for: the fold says two, the disk can supply one.
async fn leader_alone(corpus: &str) -> (TempDir, std::path::PathBuf, AppState) {
    let home = TempDir::new().expect("leader tempdir");
    let dir = home.path().join("indexes");
    std::fs::create_dir_all(&dir).expect("index dir");
    write_donor_partition(&dir, leader_node(), corpus, 0, LEADER_ONLY_TERM).await;
    let addr = dead_peer_addr().await;
    let state = node_state(leader_node(), &dir, &[(peer_node(), &addr.to_string())]);
    (home, dir, state)
}

// ─────────────────────────────────────────────────────────────────
// B7.1 — the guard is armed on the fold path
// ─────────────────────────────────────────────────────────────────

/// **B7 (reading 1).** A two-donor fold whose peer cannot be reached refuses,
/// says which coverage it lacks, and leaves the canonical directory absent.
///
/// The refusal is what stops the 17/38 case `auto_ingest.rs:263-276` records:
/// "Producing a partial canonical ourselves and then re-advertising it on
/// gossip pollutes the mesh's canonical-sync convergence — every peer ends up
/// with a different 'complete' canonical and they fight forever."
///
/// Failing input, named and watched: hand the merge `expected = 1` instead of
/// `coverage.expected`. That is the second half of this test and it is not
/// decoration — a refusal that never merges anything is indistinguishable from
/// a merge path that is simply broken, and `PartitionsUnreachable` would look
/// identical either way. With the bar dropped, the SAME disk and the SAME
/// unreachable peer produce a canonical holding half the corpus, `Recovered`,
/// and the peer's term unreachable: B1's reading, on purpose.
///
/// The leader's own partition surviving the refusal is asserted too. It is not
/// cosmetic: `merge_participants` deletes every resolved shard dir after a
/// successful merge, so a refusal that ran the cleanup anyway would destroy
/// the half of the corpus that DOES exist while reporting only that coverage
/// was short.
#[tokio::test]
async fn a_two_donor_fold_missing_its_peer_refuses_and_writes_no_canonical() {
    const CORPUS: &str = "cw-lift-5g-refusal";
    let (projection, _handoff) = terminal_handoff(CORPUS);
    let (_home, dir, state) = leader_alone(CORPUS).await;

    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");
    assert_eq!(
        coverage.expected, 2,
        "precondition: the fold names TWO donors, and only one of them has a \
         partition this node can reach",
    );

    let outcome = merge_from_fold_coverage(
        &state,
        CORPUS,
        coverage.handoff_id,
        &coverage.nodes,
        coverage.expected,
    )
    .await;

    let probe = probe_canonical(&dir, CORPUS).await;
    assert!(
        !probe.canonical_exists,
        "THE BAR: no canonical may exist after a coverage refusal. One does, \
         holding {} chunk(s). A canonical directory is TERMINAL to every \
         reader — `merge_from_fold_coverage` short-circuits on \
         `AlreadyHasCanonical` and `corpora_with_stranded_partitions` drops \
         the corpus — so a partial one written here is the corpus, for good.\n\
         merge outcome: {outcome:?}",
        probe.chunk_count,
    );
    assert!(
        matches!(
            outcome,
            RecoveryOutcome::PartitionsUnreachable {
                covered: 1,
                expected: 2
            }
        ),
        "the caller must be told WHICH coverage is missing, not merely that \
         something failed (ARCH §18.3). Got {outcome:?}",
    );
    assert!(
        corpus_at(&dir, CORPUS)
            .partition(&leader_node().to_string())
            .exists(),
        "the refusal must leave this node's own partition on disk — a cleanup \
         that ran anyway would delete the half of the corpus that does exist",
    );

    // ── The paired positive: drop the bar, and the same disk merges half ──
    let (_home2, dir2, state2) = leader_alone(CORPUS).await;
    let dropped = merge_from_fold_coverage(
        &state2,
        CORPUS,
        coverage.handoff_id,
        &coverage.nodes,
        1, // the bar the fold did NOT ask for
    )
    .await;
    let probe2 = probe_canonical(&dir2, CORPUS).await;
    assert!(
        probe2.canonical_exists && probe2.leader_term_reachable && !probe2.peer_term_reachable,
        "control: with `expected = 1` this scenario MUST produce the partial \
         canonical, or the refusal above proves only that nothing here can \
         merge at all. outcome: {dropped:?} | canonical: {} | chunks: {} | \
         leader term: {} | peer term: {}",
        probe2.canonical_exists,
        probe2.chunk_count,
        probe2.leader_term_reachable,
        probe2.peer_term_reachable,
    );
    assert!(
        matches!(dropped, RecoveryOutcome::Recovered { chunks: 2, .. }),
        "control: {dropped:?} — half the corpus, reported as a success",
    );
}

// ─────────────────────────────────────────────────────────────────
// B7.3 — the premise: the older guard is dark for every non-Wikipedia recipe
// ─────────────────────────────────────────────────────────────────

/// **B7 (reading 3).** `try_recover_stranded_partitions` — the disk-derived
/// path the fold arm falls through to — merges a 1-of-2 corpus into a
/// canonical without complaint, because its coverage guard arms only on a
/// `total_shards` stamp that no fold-sliced recipe carries. Stamp the field by
/// hand on the same disk and the same call refuses.
///
/// The premise B7 rests on, measured instead of recalled (ARCH §11.1). At HEAD
/// the only production caller of `CorpusIndex::set_total_shards` outside
/// `sharding.rs`'s merge-replay is `corpus-engine/src/engine/ingest.rs:718`,
/// inside `if let ExtractorConfig::WikipediaJsonl { .. } = recipe.extract`.
///
/// The two halves are the failing input and its control, in one test because
/// separating them invites reading either as the whole story: without the
/// stamp there is no protection, with it there is, and the stamp is what the
/// fold path now supplies by another route (`expected_partitions`).
///
/// What this does NOT show: that the FOLD path is affected. It is not — the
/// fold-driven merge arms `expected_partitions` from the handoff's own donor
/// count and never consults `total_shards`. That is the useful finding: for a
/// fold-driven merge the gap is closed by the new guard, and the gap is left
/// open for every merge that still comes off local disk, which is where this
/// reading's first half lands.
#[tokio::test]
async fn the_older_disk_guard_is_dark_without_a_total_shards_stamp() {
    const DARK: &str = "cw-lift-5g-unstamped";
    const ARMED: &str = "cw-lift-5g-stamped";

    let home = TempDir::new().expect("tempdir");
    let dir = home.path().join("indexes");
    std::fs::create_dir_all(&dir).expect("index dir");

    // Half one: a partition as a fold unit writes it — no `total_shards`.
    write_donor_partition(&dir, leader_node(), DARK, 0, LEADER_ONLY_TERM).await;
    let dark_meta = std::fs::read_to_string(Corpus::meta_in(
        corpus_at(&dir, DARK).partition(&leader_node().to_string()),
    ))
    .expect("the partition meta");
    let dark_json: serde_json::Value = serde_json::from_str(&dark_meta).expect("meta is json");
    assert!(
        dark_json.get("total_shards").is_none() || dark_json["total_shards"].is_null(),
        "precondition: a partition written outside the Wikipedia extractor \
         carries no `total_shards`. Meta: {dark_meta}",
    );

    let outcome = try_recover_stranded_partitions(&dir, DARK).await;
    let probe = probe_canonical(&dir, DARK).await;
    assert!(
        probe.canonical_exists && probe.leader_term_reachable && !probe.peer_term_reachable,
        "the disk path must publish a canonical holding ONLY this node's slice \
         — that is the hazard B7 names, and if it did not happen the premise \
         would be wrong. outcome: {outcome:?} | canonical: {} | chunks: {}",
        probe.canonical_exists,
        probe.chunk_count,
    );
    assert!(
        !matches!(outcome, RecoveryOutcome::IncompleteCoverage { .. }),
        "the older guard must NOT have armed here; got {outcome:?}",
    );

    // Half two: the SAME shape with the field stamped. The guard is real.
    write_donor_partition(&dir, leader_node(), ARMED, 0, LEADER_ONLY_TERM).await;
    let armed_partition = corpus_at(&dir, ARMED).partition(&leader_node().to_string());
    CorpusIndex::open(&armed_partition)
        .await
        .expect("open the partition")
        .set_total_shards(2)
        .expect("stamp total_shards");

    let armed_outcome = try_recover_stranded_partitions(&dir, ARMED).await;
    assert!(
        matches!(
            armed_outcome,
            RecoveryOutcome::IncompleteCoverage { total: 2, .. }
        ),
        "control: the guard the first half proved dark must fire once the \
         field it reads is present, or the first half is about a broken guard \
         rather than an unarmed one. Got {armed_outcome:?}",
    );
    assert!(
        !probe_canonical(&dir, ARMED).await.canonical_exists,
        "control: the armed guard must leave no canonical behind either",
    );
}

// ─────────────────────────────────────────────────────────────────
// B7.2 — the `continue` is load-bearing
// ─────────────────────────────────────────────────────────────────

/// A `MakeWriter` over a shared buffer. Same shape as the sibling file's
/// capture and `tests/main/injection_order.rs`'s.
#[derive(Clone)]
struct BufWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for BufWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("capture buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
    type Writer = BufWriter;
    fn make_writer(&self) -> BufWriter {
        self.clone()
    }
}

/// Put the fixture's journal on a REAL `RingRail` under this node's key, so
/// `work_donor::fold_now` — which the tick loop calls, and which reads the
/// rail rather than being handed a projection — folds the same handoff every
/// other reading in this campaign uses.
///
/// The ops are `terminal_handoff_ops`' verbatim, ingested the way a peer's
/// ops arrive (`RingJournal::ingest_all`, no re-signing), and the roster is
/// the fixture ring written to the namespace's `roster.json`. The signer is
/// `key(1)` because `fold_coverage_for` compares the handoff's submitter
/// against `rail.signer().actor()`: this node has to BE the leader for the
/// arm under test to be reached at all.
fn install_fold(state: &AppState, rail_dir: &std::path::Path, corpus: &str) {
    let signer: SigningKey = key(1);
    let rail = Arc::new(RingRail::new(rail_dir, Arc::new(signer)));
    let journal = rail.journal(WORK_NAMESPACE).expect("the work journal");
    journal.set_roster(&ring()).expect("write the roster");
    let (ops, _handoff) = terminal_handoff_ops(corpus);
    let appended = journal.ingest_all(&ops).expect("ingest the fixture ops");
    assert_eq!(
        appended,
        ops.len(),
        "every fixture op must land on the journal, or the loop folds a \
         different handoff than the one this test is about",
    );
    state.install_ring_rail(rail);
}

/// Poll `f` until it is true or `budget` elapses. Returns whether it became
/// true. The loop under test sleeps 10s before its first tick and then ticks
/// every 30s, so a fixed sleep would either be flaky or slower than it needs
/// to be.
async fn within<F: FnMut() -> bool>(budget: std::time::Duration, mut f: F) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if f() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// **B7 (reading 2) — THE `continue`.**
///
/// `df2ffecb8`'s commit body: "THE `continue` IN THAT ARM IS LOAD-BEARING.
/// When the fold has an answer it is authoritative INCLUDING WHEN IT IS A
/// REFUSAL. Falling through to the disk-derived path after a coverage refusal
/// would merge the local half anyway, which is the bug verbatim." Nothing
/// measured it, because the claim is about the ORDER of the arms in
/// `auto_collaborate_loop` and no unit-level call can see an order.
///
/// So this drives the real loop. One node, one corpus, one partition on disk
/// and a peer at a dead address; the loop is spawned exactly as the daemon
/// spawns it. The bar is that after the arm has run — witnessed by its own
/// REFUSED warning, not by a wall-clock guess — there is still no canonical.
///
/// The control is the same disk and the same loop with NO rail installed, so
/// `fold_now` returns `None`, the arm is never entered, and the fall-through
/// does what the fold refusal exists to prevent: a 1-of-2 canonical, on disk,
/// which gossip then advertises. Without it, "no canonical appeared" would
/// also be what a loop that never reached the stranded scan looks like.
///
/// Failing input, named: delete the `continue` at the end of the fold arm in
/// `auto_ingest.rs`. The refusal is logged exactly as before and then
/// `try_recover_stranded_partitions` runs anyway, so the first half of this
/// test finds the control's canonical and goes red on a corpus that the merge
/// had just refused to build.
///
/// Not checked here: the peer-canonical pull arm between the two (it needs a
/// gossip advertisement this fixture has none of), and any tick after the
/// first.
#[tokio::test]
async fn the_folds_refusal_is_final_and_the_disk_path_never_runs() {
    const CORPUS: &str = "cw-lift-5g-tick-refusal";

    // A port nothing serves, for the loop's own `corpus_collaborate` POST.
    let daemon_port = dead_peer_addr().await.port();

    let (_home, dir, state) = leader_alone(CORPUS).await;
    let rail_home = TempDir::new().expect("rail tempdir");
    install_fold(&state, rail_home.path(), CORPUS);

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(BufWriter(Arc::clone(&buf)))
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let _loop_handle = sovereign_mesh::auto_ingest::spawn_auto_collaborate_loop(state, daemon_port);

    let captured = || String::from_utf8_lossy(&buf.lock().expect("capture").clone()).into_owned();
    let saw_refusal = within(std::time::Duration::from_secs(60), || {
        captured().contains("REFUSED")
    })
    .await;

    assert!(
        saw_refusal,
        "the fold arm never ran, so this test measured nothing about the \
         `continue`. Captured:\n{}",
        captured(),
    );

    // The arm has run and refused. Give the fall-through every chance to
    // happen anyway before claiming it did not.
    let leaked = within(std::time::Duration::from_secs(5), || {
        corpus_at(&dir, CORPUS).is_installed()
    })
    .await;
    let probe = probe_canonical(&dir, CORPUS).await;
    assert!(
        !leaked && !probe.canonical_exists,
        "THE BAR: a coverage refusal must be FINAL. A canonical exists after \
         the fold refused to build one, which means the tick fell through to \
         the disk-derived path and merged the local half — the original bug, \
         verbatim.\n\
         canonical chunk_count      : {}\n\
         leader-only term reachable : {}\n\
         peer-only term reachable   : {}\n\
         \n\
         Captured:\n{}",
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
        captured(),
    );
}

/// **The control for the reading above, and the hazard in one line.** The same
/// disk and the same loop with no fold to speak for the corpus: the tick falls
/// through to `try_recover_stranded_partitions` and publishes a canonical
/// holding one donor's slice out of two.
///
/// Kept separate rather than folded into the test above because it needs its
/// own `set_default` guard and its own loop, and because "the control failed"
/// and "the bar failed" must not arrive as one verdict.
#[tokio::test]
async fn without_a_fold_the_same_tick_publishes_the_partial_canonical() {
    const CORPUS: &str = "cw-lift-5g-tick-control";

    let daemon_port = dead_peer_addr().await.port();
    let (_home, dir, state) = leader_alone(CORPUS).await;
    // No `install_fold`: `work_donor::fold_now` finds no rail and returns
    // `None`, which is every corpus ingested the legacy way.
    assert!(
        state.ring_rail().is_none(),
        "the control's premise: this node has no `work` journal to fold",
    );

    let _loop_handle = sovereign_mesh::auto_ingest::spawn_auto_collaborate_loop(state, daemon_port);

    let appeared = within(std::time::Duration::from_secs(60), || {
        corpus_at(&dir, CORPUS).is_installed()
    })
    .await;
    let probe = probe_canonical(&dir, CORPUS).await;

    assert!(
        appeared && probe.canonical_exists,
        "the control must reach the disk-derived path and merge — otherwise \
         the bar above is about a loop that never got as far as the stranded \
         scan, and proves nothing. canonical: {} | chunks: {}",
        probe.canonical_exists,
        probe.chunk_count,
    );
    assert!(
        probe.leader_term_reachable && !probe.peer_term_reachable,
        "and what it publishes is HALF the corpus: this node's slice only, \
         with the other donor's unreachable. chunks: {} | leader term: {} | \
         peer term: {}",
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
    );
}
