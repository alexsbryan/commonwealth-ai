// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B2 and B3 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: the
//! fold-side collector closes the cross-node merge gap, and exactly one node
//! acts on it.**
//!
//! This file was born at `50238f364` as B1 — the *watched red*. Two donors on
//! two nodes, one handoff, both units `Complete`, and the leader's canonical
//! corpus holding half the data while `auto_recover` returned `Recovered`.
//! That reading is preserved verbatim in the pre-registration's
//! `## Measurements` section and is not re-run here: the path it measured
//! (`sovereign_grants::auto_recover::try_recover_stranded_partitions`, which
//! merges `<corpus>-partition-*/` **on this node** and fetches nothing) still
//! exists and still behaves that way. What changed at `df2ffecb8` is that
//! `auto_ingest` no longer reaches it for a corpus the `work` fold can speak
//! for.
//!
//! What the collector added, and what each half is measured by
//! -----------------------------------------------------------
//! The new path has two halves and a thin piece of glue, and a test that
//! drives only the glue proves little. So both halves are driven, and the
//! glue is driven too:
//!
//! * [`sovereign_daemon::ingest_executor::fold_coverage_for`] — a pure read.
//!   Given a folded journal it answers "who worked on this corpus, where do
//!   their partitions live, and do I lead this handoff?". Measured by
//!   [`the_fold_names_both_verified_donors_and_where_to_find_them`] (B2, first
//!   half) and [`only_the_submitter_reads_a_merge_out_of_the_fold`] (B3). No
//!   corpus, no disk, no clock beyond the `now_ms` it is handed.
//!
//! * [`sovereign_grants::auto_recover::merge_from_fold_coverage`] → the
//!   `ShardManager::merge_participants` the fold's answer is handed to.
//!   Measured by
//!   [`two_donors_on_two_nodes_land_both_slices_in_the_canonical`] (B2, second
//!   half), which is the same scenario as B1's red with the collector wired.
//!
//! The bar is a QUERY, not a chunk count. A count can be right for the wrong
//! reason — two chunks merged twice is four. What is asserted is that a term
//! appearing ONLY in the remote donor's slice comes back from a search against
//! the merged canonical — **reached through `usable_indexes()`**, with the
//! corpus present in `installed_indexes()`, which is the list
//! `hosted_corpora` gossip is built from.
//!
//! That altitude is not incidental. The first reading of B2 probed with
//! `CorpusIndex::open` on the canonical path, which bypasses both of those
//! gates, and passed over a corpus that `installed_indexes()` could not see at
//! all. "The peer's chunks never arrived" and "the chunks arrived and nothing
//! can route to them" are different defects that a single reading conflates.
//!
//! Split at the port (pb-ingest-dial-daemon-tests-merge, phase-b-52)
//! ---------------------------------------------------------------
//! The merge and the finalize are ingest's, reached through
//! `PartitionMergePort`, so each node here holds `IngestPortDouble` and these
//! readings assert what the daemon HANDS that port: which slices (read as
//! they arrived, the peer's over the wire), into which canonical, then the
//! finalize, as the merge family's entries in `calls()`. Nothing on this side
//! opens the canonical: whether it is on disk was never the daemon's
//! question. What ingest makes of those two slices, and the two altitudes
//! told apart (merged-only rows a by-path open finds, finalized rows
//! `installed_indexes()` and `usable_indexes()` list), is proven on
//! `impl PartitionMergePort for CorpusEngine` over the same fixture, in
//! corpus-engine's `fold_merge_port_parity`.
//!
//! What this file drives for real
//! ------------------------------
//! * The real fold. Ed25519-signed ops admitted and folded by a real cw-rails
//!   (`common::work_rails`, pb-work-donor: the daemon links no rail), not a
//!   hand-built projection — two units, two DIFFERENT lessees, one handoff —
//!   over real `ingest:v1` units sealed by cw-rails' seal door over real
//!   `IngestPayload` bodies.
//! * The real partition layout: `Corpus::partition`, which the engine's
//!   `partition_path` — the call `IngestExecutor::run` makes to choose where
//!   a slice lands — delegates to.
//! * **The real wire.** The peer donor's partition is served by the peer's own
//!   `sovereign_daemon::server::internal_router` on a real loopback socket,
//!   and reaches the leader through `ShardManager::fetch_remote_shard`'s
//!   `GET /internal/index/serve` → `tar xf`. B1 could not say this: it stubbed
//!   nothing because it pulled nothing.
//! * The real peer resolution: `peer_control_urls` → `PeerTransport::endpoints`
//!   over a `MemberRecord`, so the leader learns the peer's address the way
//!   production does rather than being handed a URL.
//!
//! What this file does NOT check — said here rather than discovered later
//! ---------------------------------------------------------------------
//! * **Two machines.** Two nodes are two index dirs, two `AppState`s and two
//!   sockets in one process. Cross-machine clocks, real network loss and
//!   partial transfers are not exercised — the pre-registration's own caveat.
//! * **The ingest pipeline.** No recipe, no acquirer, no embedder. Partitions
//!   are written through `CorpusIndex` at the path the executor computes: what
//!   is under test is the merge, not the extract.
//! * **`auto_ingest`'s tick loop.** This file calls the collector directly, so
//!   the ORDER of the arms — in particular the load-bearing `continue` that
//!   stops a coverage refusal falling through to the disk-derived merge — is
//!   B7's subject, in `fold_ingest_coverage_refusal_e2e`, not this file's.
//! * **Idempotence.** B4 is measured at the merge level in
//!   `commonwealth-knowledge/tests/main/merge_participants_idempotence.rs`,
//!   because `merge_from_fold_coverage` short-circuits on
//!   `AlreadyHasCanonical` and would answer a different question.
//! * **B5 and B7.** Their own files — `fold_ingest_abandoned_unit_e2e` and
//!   `fold_ingest_coverage_refusal_e2e`, which share this file's fixture.
//!
//! A hazard this file steps around on purpose
//! ------------------------------------------
//! `NodeId`'s `Display` is `node-<hex of the first EIGHT bytes>`, and every
//! partition directory name is built from it. `NodeId::from_u128(0x11)` and
//! `from_u128(0x22)` therefore print IDENTICALLY, and two fixture peers whose
//! ids share a 64-bit prefix collide on one partition directory — the merge
//! then sees fewer shards and says nothing. The ids below differ in their HIGH
//! bytes for that reason, as in `merge_participants_coverage.rs`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use commonwealth_rail_core::{
    actor_of, body_json, sign_ring_op, Op, Person, RailAct, Roster, SignedOp, SigningKey,
};
use corpus_index::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use corpus_index::ingest_port::double::IngestPortDouble;
use corpus_index::ingest_port::merge::PartitionMergeReport;
use kernel_types::judgement::Reason;
use kernel_types::ActorKey;
use kernel_types::HandoffId;
use kernel_types::{ComputeAttribution, Judgement, NodeId, Server};
use oicp_types::work::projection::{WorkProjection, WorkUnitStatus};
use oicp_types::work::{Completion, Submission, UnitRef, WorkAct};
use oicp_types::work_queue::{HandoffPhase, WorkUnit};
use oicp_types::{JobKind, JobUnit};
use serde_json::json;
use sovereign_daemon::ingest_executor::{fold_coverage_for, IngestPayload, INGEST_KIND};
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;
use sovereign_grants::auto_recover::{merge_from_fold_coverage, RecoveryOutcome};
use tempfile::TempDir;

use crate::common;
use crate::common::corpus_at;
use crate::common::ledger_double::RecordingLedger;
use crate::common::work_rails::{payload_of, WorkRails, WORK_NAMESPACE};

/// The two donor nodes. **The HIGH bytes must differ** — see the module docs.
pub(crate) fn leader_node() -> NodeId {
    NodeId::from_u128(0x11 << 120)
}
pub(crate) fn peer_node() -> NodeId {
    NodeId::from_u128(0x22 << 120)
}

const EMBED_DIM: usize = 4;

/// The instant every reading in this file asks the fold about. Well past the
/// last act's timestamp, so nothing is still leased.
const NOW_MS: u64 = 400_000;

/// A term that appears ONLY in the leader donor's slice, and one that appears
/// ONLY in the peer donor's. The whole verdict turns on whether the second is
/// reachable in the leader's canonical index.
pub(crate) const LEADER_ONLY_TERM: &str = "quokka";
pub(crate) const PEER_ONLY_TERM: &str = "narwhal";

// ─────────────────────────────────────────────────────────────────
// The fold — real signed ops, real admission, real projection
// ─────────────────────────────────────────────────────────────────

pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub(crate) fn actor(seed: u8) -> ActorKey {
    ActorKey::parse(actor_of(&key(seed))).expect("actor_of emits canonical hex")
}

/// Two donors in one ring. `leader` is also the submitter — which is what the
/// pre-registration names as the merge leader (`WorkHandoff.submitter`,
/// `projection.rs:274`), so the leader's node is where the collector runs.
pub(crate) fn ring() -> Roster {
    let mut m = BTreeMap::new();
    m.insert(Person::from("leader"), vec![actor_of(&key(1))]);
    m.insert(Person::from("peer"), vec![actor_of(&key(2))]);
    Roster::new(m)
}

pub(crate) fn sign(seed: u8, ts: i64, seq: u64, act: &WorkAct) -> Op<SignedOp> {
    let k = key(seed);
    let inner = RailAct::Record {
        payload: payload_of(act),
    };
    let sig = sign_ring_op(&k, WORK_NAMESPACE, ts, seq, &body_json(&inner, None, None));
    Op::new(
        SignedOp {
            seq,
            sig,
            act: inner,
            on_behalf_of: None,
            view: None,
        },
        ts,
        actor_of(&k),
    )
}

fn provenance(node: NodeId, name: &str) -> ComputeAttribution {
    ComputeAttribution {
        repo_rev: "cw-lift-5g".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        toolchain: "rustc 1.90.0".into(),
        host: Server::Peer {
            node,
            name: name.into(),
        },
    }
}

/// The cw-rails every reading here seals and folds through, its `work` ring
/// narrowed to [`ring`]'s two donors.
pub(crate) async fn work_rails() -> WorkRails {
    WorkRails::spawn(Some(&ring()), "").await
}

/// One real `ingest:v1` unit — sealed by the plane's own sealer (cw-rails'
/// seal door) over a real [`IngestPayload`], so the unit hash is the one
/// production would compute.
pub(crate) async fn ingest_unit(
    rails: &WorkRails,
    corpus: &str,
    unit_id: u32,
    start: u64,
    end: u64,
) -> JobUnit {
    let payload = serde_json::to_value(IngestPayload::slice(
        corpus,
        corpus,
        unit_id,
        WorkUnit::JsonlRange { start, end },
    ))
    .expect("an IngestPayload encodes");
    rails
        .seal(
            &JobKind::parse(INGEST_KIND).expect("`ingest:v1` parses"),
            vec![payload],
        )
        .await
        .remove(0)
}

pub(crate) fn unit_ref(handoff: HandoffId, unit: &JobUnit) -> UnitRef {
    UnitRef {
        handoff,
        unit_hash: unit.unit_hash.clone(),
    }
}

pub(crate) fn completion(
    handoff: HandoffId,
    unit: &JobUnit,
    corpus: &str,
    node: NodeId,
    name: &str,
) -> WorkAct {
    WorkAct::Complete(Completion {
        handoff,
        unit_hash: unit.unit_hash.clone(),
        outcome: Judgement::passed(
            "ingest slice",
            Reason::literal("the slice ran and its partition holds chunks"),
        ),
        result: json!({
            "corpus_id": corpus,
            // Where the donor says its slice landed. Nothing under test reads
            // it; spelled through `Corpus` anyway so the layout has one owner.
            "partition_path": corpus_at("", corpus).partition(&node.to_string()),
        }),
        provenance: provenance(node, name),
    })
}

/// The SIGNED OPS of the scenario below, before anything folds them. Split out
/// because two readings need the same journal in two shapes: this file folds it
/// in cw-rails, and `fold_ingest_coverage_refusal_e2e` writes it onto a real
/// `RingRail` so `auto_ingest`'s own tick folds it. One spelling (ARCH §10.6).
/// `rails` seals the units.
pub(crate) async fn terminal_handoff_ops(
    rails: &WorkRails,
    corpus: &str,
) -> (Vec<Op<SignedOp>>, HandoffId) {
    let handoff = HandoffId::from_u128(5_000_002); // stable, arbitrary
    let a = ingest_unit(rails, corpus, 0, 0, 100).await;
    let b = ingest_unit(rails, corpus, 1, 100, 200).await;

    let submit = WorkAct::Submit(Submission::new(
        handoff,
        JobKind::parse(INGEST_KIND).expect("kind"),
        vec![a.clone(), b.clone()],
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
    ];
    (ops, handoff)
}

/// The scenario every reading in this file shares: one handoff, two
/// `ingest:v1` units for one corpus, leased and completed by two DIFFERENT
/// actors, folded to terminal.
///
/// Asserts on the way out that the fold really did reach `Complete` with two
/// distinct lessees — if that ever stops holding, everything below would be
/// answering a question nobody asked.
pub(crate) async fn terminal_handoff(corpus: &str) -> (WorkProjection, HandoffId) {
    let rails = work_rails().await;
    let (ops, handoff) = terminal_handoff_ops(&rails, corpus).await;
    let a = ingest_unit(&rails, corpus, 0, 0, 100).await;
    let b = ingest_unit(&rails, corpus, 1, 100, 200).await;

    rails.ingest(&ops).await;
    let projection = rails.projection().await;

    let h = projection.handoffs.get(&handoff).unwrap_or_else(|| {
        panic!(
            "the submission was admitted; cw-rails' log:\n{}",
            rails.log()
        )
    });

    assert_eq!(
        h.phase_at(NOW_MS),
        HandoffPhase::Complete,
        "the scenario requires a TERMINAL handoff — every signal green — before \
         anything is asked of the corpus. Got {:?}",
        h.phase_at(NOW_MS),
    );

    let lessees: Vec<ActorKey> = [&a, &b]
        .iter()
        .map(|u| match h.units[&u.unit_hash].status_at(NOW_MS) {
            WorkUnitStatus::Complete { lessee, .. } => lessee,
            other => panic!("unit {} is {other:?}, not Complete", u.unit_hash),
        })
        .collect();
    assert_eq!(lessees, vec![actor(1), actor(2)]);
    assert_ne!(
        lessees[0], lessees[1],
        "the whole point of this reading is TWO donors — a scenario where one \
         actor ran both units is part 1's n=1 proof, not this one",
    );

    (projection, handoff)
}

// ─────────────────────────────────────────────────────────────────
// The disk and the wire — two nodes are two index dirs and two sockets
// ─────────────────────────────────────────────────────────────────

/// What one partition dir held when the daemon handed it to ingest: its rows,
/// and whether a search of it returns each donor's term. Read at call time,
/// because `merge_participants` deletes the shard dirs it merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Slice {
    pub(crate) chunks: u64,
    pub(crate) leader_term: bool,
    pub(crate) peer_term: bool,
}

pub(crate) const LEADER_SLICE: Slice = Slice {
    chunks: 2,
    leader_term: true,
    peer_term: false,
};
pub(crate) const PEER_SLICE: Slice = Slice {
    chunks: 2,
    leader_term: false,
    peer_term: true,
};

async fn slice_of(dir: &Path) -> corpus_index::Result<Slice> {
    let index = CorpusIndex::open(dir).await?;
    Ok(Slice {
        chunks: index.info().await?.chunk_count,
        leader_term: term_reachable(&index, LEADER_ONLY_TERM).await?,
        peer_term: term_reachable(&index, PEER_ONLY_TERM).await?,
    })
}

/// Everything the daemon handed one node's ingest port.
#[derive(Default)]
pub(crate) struct PortLog {
    /// Each `merge_partitions`: the slices it was handed, and its output.
    pub(crate) merges: Mutex<Vec<(Vec<Slice>, PathBuf)>>,
    /// Each `finalize_canonical`'s corpus id.
    pub(crate) finalized: Mutex<Vec<String>>,
    /// Each `merge_partitions_into_canonical`: the corpus id, and the slices
    /// its partition dirs held.
    pub(crate) disk_merges: Mutex<Vec<(String, Vec<Slice>)>>,
}

/// One node's corpus handle: ingest's port double rooted at the node's index
/// dir, and what the daemon handed it.
pub(crate) struct NodePort {
    pub(crate) port: Arc<IngestPortDouble>,
    pub(crate) log: Arc<PortLog>,
}

impl NodePort {
    /// The merge family's entries in the port's `calls()`, in order.
    pub(crate) fn merge_acts(&self) -> Vec<&'static str> {
        const MERGE_ACTS: [&str; 3] = [
            "merge_partitions",
            "finalize_canonical",
            "merge_partitions_into_canonical",
        ];
        self.port
            .calls()
            .into_iter()
            .filter(|c| MERGE_ACTS.contains(c))
            .collect()
    }

    pub(crate) fn merges(&self) -> Vec<(Vec<Slice>, PathBuf)> {
        self.log.merges.lock().expect("merges lock").clone()
    }

    pub(crate) fn finalized(&self) -> Vec<String> {
        self.log.finalized.lock().expect("finalized lock").clone()
    }

    pub(crate) fn disk_merges(&self) -> Vec<(String, Vec<Slice>)> {
        self.log
            .disk_merges
            .lock()
            .expect("disk merges lock")
            .clone()
    }
}

/// [`node_port_with`], programmed with nothing more.
pub(crate) fn node_port(index_dir: &Path) -> NodePort {
    node_port_with(index_dir, |port| port)
}

/// Ingest's merge port as a double rooted at `index_dir`, with `program`
/// chained on for whatever else a reading drives.
///
/// Its merge records the slices it was handed and creates the canonical at
/// the output with the leaf's own `CorpusIndex::create`, empty, because
/// `merge_participants` opens it before the finalize; it answers the sum of
/// the slices' rows, what ingest reports for disjoint slices. Its disk merge
/// records the partition dirs `<corpus>-partition-*` held and answers one
/// shard per dir. Neither writes a row: what ingest writes is proven on the
/// engine, in corpus-engine's `fold_merge_port_parity`.
pub(crate) fn node_port_with(
    index_dir: &Path,
    program: impl FnOnce(IngestPortDouble) -> IngestPortDouble,
) -> NodePort {
    let log = Arc::new(PortLog::default());
    let (merges, finalized, disk_merges) = (Arc::clone(&log), Arc::clone(&log), Arc::clone(&log));
    let port = IngestPortDouble::new()
        .with_index_dir(index_dir)
        .on_merge_partitions(move |inputs, output| {
            let log = Arc::clone(&merges);
            Box::pin(async move {
                let mut slices = Vec::new();
                for dir in &inputs {
                    slices.push(slice_of(dir).await?);
                }
                let chunks = slices.iter().map(|s| s.chunks).sum();
                log.merges
                    .lock()
                    .expect("merges lock")
                    .push((slices, output.clone()));
                let id = output
                    .file_name()
                    .and_then(|n| n.to_str())
                    .expect("the canonical dir is named for its corpus")
                    .to_string();
                let canonical = CorpusIndex::create(
                    &output,
                    &id,
                    "cw-lift 5g cross-node",
                    "test-embed",
                    EMBED_DIM,
                    true,
                    "MIT",
                )
                .await?;
                let mut info = canonical.info().await?;
                info.chunk_count = chunks;
                Ok(info)
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
        .on_merge_partitions_into_canonical(move |index_dir, corpus_id| {
            let log = Arc::clone(&disk_merges);
            Box::pin(async move {
                let corpus = corpus_at(&index_dir, &corpus_id);
                let prefix = corpus.partition_prefix();
                let mut dirs: Vec<PathBuf> = std::fs::read_dir(&index_dir)?
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                    .map(|e| e.path())
                    .collect();
                dirs.sort();
                let mut slices = Vec::new();
                for dir in &dirs {
                    slices.push(slice_of(dir).await?);
                }
                let chunks = slices.iter().map(|s| s.chunks).sum();
                log.disk_merges
                    .lock()
                    .expect("disk merges lock")
                    .push((corpus_id.clone(), slices));
                Ok(PartitionMergeReport {
                    shard_union: (0..dirs.len()).collect(),
                    partition_paths: dirs,
                    canonical_path: corpus.root(),
                    chunks_input: chunks,
                    chunks_merged: chunks,
                    total_shards: None,
                    embedding_model: "test-embed".into(),
                    embedding_dimensions: EMBED_DIM,
                })
            })
        });
    NodePort {
        port: Arc::new(program(port)),
        log,
    }
}

/// Write one donor's finished slice into the partition directory the real
/// [`sovereign_daemon::ingest_executor::IngestExecutor`] would have chosen —
/// `Corpus::partition`, which the engine's `partition_path` delegates to,
/// called here rather than re-spelled, so a change to the layout breaks this
/// test rather than silently detaching it.
pub(crate) async fn write_donor_partition(
    index_dir: &std::path::Path,
    node: NodeId,
    corpus: &str,
    unit_id: u32,
    term: &str,
) {
    let path = corpus_at(index_dir, corpus).partition(&node.to_string());

    let index = CorpusIndex::create(
        &path,
        corpus,
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
                    text_sha256: None,
                },
                vec![0.25_f32; EMBED_DIM],
            )
        })
        .collect();
    index.insert_batch(&rows).await.expect("insert_batch");

    // The engine clears this when the full pipeline finishes; `auto_recover`
    // refuses to merge a partition still flagged in-progress, so a simulated
    // donor that skipped it would fail this test for a setup reason rather
    // than for the gap.
    index
        .mark_ingestion_complete()
        .expect("mark the slice finished");
}

/// An `AppState` for one node: its own id, its own corpus handle (`node`'s
/// port, rooted at its own index dir), and a mesh holding whatever `others`
/// are reachable from it.
///
/// The mesh is what `peer_control_urls` reads to turn a `NodeId` from the fold
/// into a base URL, so a donor that is not a member here is unreachable —
/// which is the production behaviour, not a shortcut.
pub(crate) fn node_state(self_id: NodeId, node: &NodePort, others: &[(NodeId, &str)]) -> AppState {
    node_state_with_seed(
        self_id,
        node,
        others,
        sovereign_daemon::state::FabricSeed::default(),
    )
}

/// [`node_state`] with Fabric's construction seed — a rail is a construction
/// argument now, not a post-construction install (DC §4.2 "Construction is
/// staged, and parts are total").
pub(crate) fn node_state_with_seed(
    self_id: NodeId,
    node: &NodePort,
    others: &[(NodeId, &str)],
    seed: sovereign_daemon::state::FabricSeed,
) -> AppState {
    use sovereign_contracts::daemon_wire::mesh::MemberStatus;
    let mut rows = vec![common::peer_row(
        self_id,
        "self",
        MemberStatus::Online,
        common::empty_capabilities(),
        vec!["127.0.0.1:9742".parse().expect("addr")],
    )];
    for (id, addr) in others {
        rows.push(common::peer_row(
            *id,
            "donor",
            MemberStatus::Online,
            common::empty_capabilities(),
            vec![addr.parse().expect("peer addr")],
        ));
    }
    let seed = sovereign_daemon::state::FabricSeed {
        peer_transport: sovereign_daemon::double::address_transport(),
        membership: Some(common::roster("cw-lift 5g part 2", rows)),
        ..seed
    };
    AppState::new_with_seeds(
        self_id,
        Some(Arc::clone(&node.port) as _),
        None,
        seed,
        sovereign_daemon::state::serving::ServingSeed::default(),
        sovereign_daemon::state::NodeSeed::default(),
        Arc::new(RecordingLedger::new(self_id)).seed(),
    )
}

/// Does a search of `index` return a chunk carrying `term`? A failed search
/// is the port's error, never "not reachable".
async fn term_reachable(index: &CorpusIndex, term: &str) -> corpus_index::Result<bool> {
    Ok(index
        .search(&[0.25_f32; EMBED_DIM], term, 10)
        .await?
        .iter()
        .any(|h| h.content.contains(term)))
}

// ─────────────────────────────────────────────────────────────────
// B2, first half — the pure read
// ─────────────────────────────────────────────────────────────────

/// **B2 (first half).** The fold names both VERIFIED donors, both hosts to
/// pull from, and the handoff the merge is keyed on — with nothing abandoned.
///
/// No corpus and no disk: `fold_coverage_for` is a pure function of a folded
/// journal, this node's key, a corpus id and a clock reading. Everything the
/// merge below is handed comes from here, so if this is wrong the merge is
/// merging the wrong set and a green canonical would mean nothing.
///
/// Failing input, named and watched (ARCH §18.1): gate the collection loop on
/// `&lessee == self_key`, so only this node's own contributions count. That is
/// the local-donor-only defect moved down to the fold — the same shape the
/// disk-derived path has — and it takes `expected` from 2 to 1.
///
/// WHAT THIS DOES NOT DISTINGUISH, and it is the subtle one: in this fixture
/// each donor completes exactly one unit from exactly one host, so counting
/// distinct VERIFIED actors and counting distinct SELF-REPORTED hosts both give
/// 2. The rule `FoldCoverage` documents — `expected` counts actors, never hosts
/// — is therefore NOT witnessed here. Witnessing it needs a donor that names
/// two hosts for one lessee, which is B5's fixture shape, not this one.
#[tokio::test]
async fn the_fold_names_both_verified_donors_and_where_to_find_them() {
    const CORPUS: &str = "cw-lift-5g-coverage";
    let (projection, handoff) = terminal_handoff(CORPUS).await;

    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");

    assert_eq!(
        coverage.handoff_id, handoff,
        "the coverage must name the handoff the merge will be keyed on",
    );

    // `expected` counts distinct VERIFIED actors — the `lessee` admission
    // checked — never the self-reported `provenance.host`. Two donors, two
    // actors. Counting hosts would let one donor inflate coverage by naming
    // extra nodes (ARCH §18.1: a guard asserting on a field the subject
    // supplies).
    assert_eq!(
        coverage.expected, 2,
        "two distinct lessees completed this handoff",
    );

    let mut nodes = coverage.nodes.clone();
    nodes.sort();
    let mut want = vec![leader_node(), peer_node()];
    want.sort();
    assert_eq!(
        nodes, want,
        "both donors' hosts must be named, or the merge cannot know where to \
         pull the second partition from",
    );

    assert!(
        coverage.abandoned.is_empty() && !coverage.is_partial(),
        "no unit failed in this scenario, so nothing may be reported abandoned; \
         got {:?}",
        coverage.abandoned,
    );
}

// ─────────────────────────────────────────────────────────────────
// B3 — exactly one node merges
// ─────────────────────────────────────────────────────────────────

/// **B3.** Both donors fold the SAME journal. Only the submitter gets an
/// answer; the other declines, and says so at `debug`.
///
/// Two nodes racing one output directory is its own failure and a passing B2
/// does not imply it: B2 only ever asks the leader. This test asks the peer.
///
/// Failing input, named (ARCH §18.1): drop the `&handoff.submitter != self_key`
/// arm from `fold_coverage_for` and the second assertion goes red — the peer
/// gets the same coverage the leader does and both merge.
///
/// The paired positive is deliberate. `None` is also what a bug that never
/// matches anything returns, so a test asserting only `None` passes just as
/// happily against a function that always declines.
#[tokio::test]
async fn only_the_submitter_reads_a_merge_out_of_the_fold() {
    use std::io::Write;
    use std::sync::Mutex;

    const CORPUS: &str = "cw-lift-5g-leader";
    let (projection, _handoff) = terminal_handoff(CORPUS).await;

    // The positive control: the submitter DOES get an answer from this exact
    // projection, so a `None` below is about the actor and not about the fold.
    assert!(
        fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS).is_some(),
        "control: the submitter must lead this handoff, or the refusal below \
         proves nothing",
    );

    /// A `MakeWriter` over a shared buffer, so the decline event can be read
    /// back. Same shape as `tests/main/injection_order.rs`'s capture.
    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for BufWriter {
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

    let buf = Arc::new(Mutex::new(Vec::new()));
    let coverage = {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(BufWriter(Arc::clone(&buf)))
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        fold_coverage_for(&projection, &actor(2), CORPUS, NOW_MS)
    };

    assert!(
        coverage.is_none(),
        "the non-submitter must decline. It folded the SAME journal and reached \
         the same terminal handoff; if it also gets a coverage answer then two \
         nodes merge into one output directory. Got {coverage:?}",
    );

    // Pre-registered instrument (B3): the decision must be visible at debug,
    // naming the submitter it compared against — "nobody merged" and "two
    // nodes merged" are indistinguishable after the fact otherwise.
    let captured = String::from_utf8(buf.lock().expect("capture buffer").clone())
        .expect("tracing output is utf-8");
    assert!(
        captured.contains("not the submitter"),
        "the decline must be traced, not silent (ARCH §9.1). Captured:\n{captured}",
    );
    assert!(
        captured.contains(&actor(1).to_string()) && captured.contains(&actor(2).to_string()),
        "the trace must name BOTH the submitter it compared against and this \
         node's own key, or an operator cannot tell which node should have \
         acted. Captured:\n{captured}",
    );
}

// ─────────────────────────────────────────────────────────────────
// B2, second half — the merge, over a real socket
// ─────────────────────────────────────────────────────────────────

/// **B2 (second half) — THE BAR, the daemon's half.** Two donors, two nodes,
/// one corpus, one handoff. The leader hands ingest BOTH donors' slices —
/// its own from disk, the peer's as it arrived over the wire from the peer's
/// own `internal_router` — into the canonical, and THEN the finalize.
///
/// This is B1's scenario with the collector wired. B1 measured
/// `Recovered { chunks: 2 }` here with the peer-only term unreachable.
///
/// # Why the finalize is asserted, and where the altitude went
///
/// Until 2026-09-09 this bar was read with `CorpusIndex::open` on the
/// canonical path, and went green on `df2ffecb8` over a canonical that
/// `installed_indexes()` returned zero rows for: the merge wrote the chunks
/// and never finalized them. On this side that defect is the finalize
/// missing from the merge family's `calls()`; that a merge plus the finalize
/// is a corpus `installed_indexes()` and `usable_indexes()` list, with the
/// peer's term reachable through the engine's by-id open, is the engine
/// half (`fold_merge_port_parity`), read at that altitude and never by path.
/// Nothing here opens the canonical.
///
/// Failing inputs, named, and they print differently:
///
/// 1. *The participant set.* Truncate `coverage.nodes` to the local node
///    before the merge — the shape of the original defect, where
///    participants come from local disk instead of the fold. The port is
///    handed the leader's slice alone.
/// 2. *The finalize.* Drop `finalize_canonical` from `merge_participants`.
///    The port is handed both slices, and `merge_acts` stops at
///    `merge_partitions`.
#[tokio::test]
async fn two_donors_on_two_nodes_hand_ingest_both_slices_then_the_finalize() {
    const CORPUS: &str = "cw-lift-5g-two-nodes";

    let (projection, _handoff) = terminal_handoff(CORPUS).await;

    // Two nodes are two index directories. Nothing copies between them except
    // the HTTP pull below.
    let leader_home = TempDir::new().expect("leader tempdir");
    let peer_home = TempDir::new().expect("peer tempdir");
    let leader_dir = leader_home.path().join("indexes");
    let peer_dir = peer_home.path().join("indexes");
    std::fs::create_dir_all(&leader_dir).expect("leader index dir");
    std::fs::create_dir_all(&peer_dir).expect("peer index dir");

    write_donor_partition(&leader_dir, leader_node(), CORPUS, 0, LEADER_ONLY_TERM).await;
    write_donor_partition(&peer_dir, peer_node(), CORPUS, 1, PEER_ONLY_TERM).await;

    // The peer serves its own partition from its own `internal_router`, so
    // `GET /internal/index/serve` → `tar cf` → the wire → `tar xf` is inside
    // this test rather than stubbed around it.
    let peer = node_port(&peer_dir);
    let peer_state = node_state(peer_node(), &peer, &[]);
    let peer_addr = common::spawn_router(internal_router(peer_state)).await;

    // The leader knows the peer only as a mesh member with an address, which
    // is what `peer_control_urls` resolves through `PeerTransport::endpoints`.
    let leader = node_port(&leader_dir);
    let leader_state = node_state(
        leader_node(),
        &leader,
        &[(peer_node(), &peer_addr.to_string())],
    );

    // ── The collector: the fold's answer, handed to the merge ──
    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");

    let node = sovereign_daemon::routes_internal::fold_recovery(&leader_state).await;
    let outcome = merge_from_fold_coverage(
        node,
        CORPUS,
        coverage.handoff_id,
        &coverage.nodes,
        coverage.expected,
    )
    .await;

    let merges = leader.merges();
    let mut slices: Vec<Slice> = merges.iter().flat_map(|(s, _)| s.clone()).collect();
    slices.sort_by_key(|s| s.peer_term);
    assert_eq!(
        slices,
        vec![LEADER_SLICE, PEER_SLICE],
        "the leader must hand ingest BOTH donors' slices whole — its own, and \
         the peer's `{PEER_ONLY_TERM}` slice pulled from a different index dir \
         over a different socket. The fold named both donors.\n\
         merge outcome : {outcome:?}\n\
         fold coverage : expected={} nodes={:?}\n\
         merges        : {merges:?}\n\
         port acts     : {:?}\n\
         \n\
         See `quality/campaigns/cw-lift-5g-part2-prereg.md` B2.",
        coverage.expected,
        coverage.nodes,
        leader.merge_acts(),
    );
    assert_eq!(merges.len(), 1, "one merge for the handoff: {merges:?}");
    assert_eq!(merges[0].1, corpus_at(&leader_dir, CORPUS).root());
    assert_eq!(
        leader.merge_acts(),
        vec!["merge_partitions", "finalize_canonical"],
        "the merge must end in the finalize, or it writes chunks that \
         `installed_indexes()` skips and `hosted_corpora` gossip advertises to \
         no peer (`df2ffecb8`). outcome: {outcome:?}",
    );
    assert_eq!(leader.finalized(), vec![CORPUS.to_string()]);
    assert!(
        peer.merge_acts().is_empty(),
        "the peer only serves its slice; it merges nothing",
    );

    match outcome {
        RecoveryOutcome::Recovered {
            chunks,
            shards_covered,
        } => {
            assert_eq!(chunks, 4, "the outcome carries the count ingest reported");
            assert_eq!(
                shards_covered, 2,
                "both donors' partitions were covered by this merge",
            );
        }
        other => panic!(
            "the merge must report the canonical it actually built; got {other:?}. \
             A canonical holding both slices under a non-`Recovered` outcome would \
             mean the caller is told nothing happened while the corpus changed."
        ),
    }
}

/// **THE NEGATIVE CONTROL, retained from B1, the daemon's half.** The same
/// scenario at n=1 — part 1's proof shape: both partition directories under
/// one index dir, merged by the DISK-derived path this rung does not change
/// (`try_recover_stranded_partitions`).
///
/// It controls for the fixture, not for the collector: the disk path hands
/// ingest's disk merge this corpus while both slices sit under its
/// partition prefix. That the disk merge lands both in a reachable
/// canonical is the engine half, in `fold_merge_port_parity`. It also pins
/// that the legacy path still reaches ingest, which the collector's
/// `continue` arm still falls through to for every corpus the fold cannot
/// speak for.
#[tokio::test]
async fn two_donors_on_one_node_hand_the_disk_merge_both_slices() {
    const CORPUS: &str = "cw-lift-5g-one-node";

    let home = TempDir::new().expect("tempdir");
    let index_dir = home.path().join("indexes");
    std::fs::create_dir_all(&index_dir).expect("index dir");

    // The ONLY difference from the reading above: both partitions are under
    // one index dir, which is what a single machine running two units produces.
    write_donor_partition(&index_dir, leader_node(), CORPUS, 0, LEADER_ONLY_TERM).await;
    write_donor_partition(&index_dir, peer_node(), CORPUS, 1, PEER_ONLY_TERM).await;

    let node = node_port(&index_dir);
    let outcome = sovereign_grants::auto_recover::try_recover_stranded_partitions(
        &*node.port,
        &index_dir,
        CORPUS,
    )
    .await;

    assert_eq!(
        node.disk_merges(),
        vec![(CORPUS.to_string(), vec![LEADER_SLICE, PEER_SLICE])],
        "the negative control failed, so B2's green proves LESS than it looks — \
         the disk path did not hand ingest this fixture's two slices even when \
         both are on one node. merge outcome: {outcome:?}",
    );
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { chunks: 4, .. }),
        "2 chunks from each of the two units; got {outcome:?}",
    );
}
