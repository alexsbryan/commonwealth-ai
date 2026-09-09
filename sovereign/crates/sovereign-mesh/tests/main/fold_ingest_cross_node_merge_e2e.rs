// SPDX-License-Identifier: AGPL-3.0-or-later
//! **B1 of `quality/campaigns/cw-lift-5g-part2-prereg.md`: the cross-node
//! merge gap, watched red.**
//!
//! cw-lift 5g part 1 (`1e5418f4b`) put corpus ingest onto the
//! `commonwealth-work` fold as kind `ingest:v1`
//! (`sovereign-mesh/src/ingest_executor.rs`). Part 1's proof was single-node:
//! two units, one machine, one partition directory. This file is the n=2
//! reading part 1 could not take.
//!
//! What is claimed to be broken, stated precisely
//! ----------------------------------------------
//! It is NOT that the fold cannot express "the work is done". It can, and this
//! test drives it: every act names a handoff (`WorkAct::handoff`,
//! `commonwealth-work/src/act.rs:136-146`), `WorkHandoff::phase_at` returns
//! `HandoffPhase::Complete` once `queued == 0 && leased == 0`
//! (`projection.rs:306-317`), and each terminal `WorkUnitStatus::Complete`
//! names its `lessee: ActorKey` (`projection.rs:170-181`). The union of those
//! lessees IS the participating-peer set, on a durable replicated journal.
//!
//! The gap is that **nothing reads it for ingest.** `IngestPayload`
//! (`ingest_executor.rs:141-153`) carries no `HandoffId`, so no
//! `IngestionHandoff` blob is ever written to `MeshStore` for
//! `ShardManager::coordinate_merge`'s `load_handoff` to find
//! (`commonwealth-knowledge/src/shard_manager.rs:183`), and
//! `coordinate_merge` — the only code path in the workspace that pulls a
//! peer's shard tarball — has exactly two production call sites, both in
//! `commonwealth-api/src/routes_internal/corpus_queue.rs` (`:210`, `:550`),
//! neither reachable from the fold. So on the fold path the only merge a
//! donor node can reach is the local-only one: `auto_ingest`'s proactive
//! stranded-partition sweep calling
//! `commonwealth_api::auto_recover::try_recover_stranded_partitions`
//! (`sovereign-mesh/src/auto_ingest.rs:297`), which merges every
//! `<corpus>-partition-*/` directory **on this node** and fetches nothing.
//!
//! The predicted failure is this workspace's characteristic one: exit 0,
//! wrong answer. Both units `Complete`, the handoff terminal, every signal
//! green — and the canonical corpus holding only the local donor's chunks.
//!
//! What this file drives for real
//! ------------------------------
//! * The real fold. Ed25519-signed ops through `commonwealth_rail::admit` and
//!   `WorkProjection::fold`, not a hand-built `WorkProjection`. Two units,
//!   two DIFFERENT lessees, one handoff.
//! * Real `ingest:v1` units: `commonwealth_work::seal::seal` over real
//!   `sovereign_mesh::ingest_executor::IngestPayload` bodies.
//! * The real partition layout: `CorpusEngine::partition_path`, the same call
//!   `IngestExecutor::run` makes to choose where a slice lands
//!   (`ingest_executor.rs:245`).
//! * The real merge decider: `auto_recover::try_recover_stranded_partitions`.
//!
//! What this file does NOT check — said here rather than discovered later
//! ---------------------------------------------------------------------
//! * **The network half.** `coordinate_merge` pulls tarballs over HTTP via
//!   `fetch_remote_shard`. This is an in-process simulation of two nodes as
//!   two index directories; it proves nothing about that transfer. A real
//!   two-machine run is a separate, later reading.
//! * **The ingest pipeline.** No recipe, no acquirer, no embedder. Partitions
//!   are written directly through `CorpusIndex` at the path the executor
//!   computes. What is under test is the merge, not the extract.
//! * **The `total_shards` coverage gate.** `auto_recover` refuses with
//!   `IncompleteCoverage` when a partition meta stamps `total_shards` and the
//!   local union does not cover it — but only `ExtractorConfig::WikipediaJsonl`
//!   stamps that field (`corpus-engine/src/engine/ingest.rs:718-737`), so for
//!   every other recipe the gate is dark. These partitions do not stamp it,
//!   matching the common case. On a Wikipedia recipe the same gap surfaces as
//!   a permanent stall instead of a wrong canonical; both are "the corpus is
//!   missing the peer donor's chunks".
//!
//! `#[ignore]`d, with the reason naming cw-lift 5g part 2. That is a
//! **temporary marker, not a verdict**: a suite made green by hiding a known
//! red is no greener than a zero-test run. Remove the attribute when the
//! collector lands; until then this test is owed, not passing.

use std::collections::BTreeMap;
use std::sync::Arc;

use commonwealth_core::ids::HandoffId;
use commonwealth_core::knowledge::{HandoffPhase, WorkUnit};
use commonwealth_rail::{
    actor_of, admit, body_json, sign_ring_op, Ed25519Verifier, Op, Person, RailAct, Roster,
    SignedOp, SigningKey,
};
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::{ActorKey, Completion, Submission, UnitRef, WorkAct, WORK_NAMESPACE};
use corpus_engine::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use corpus_engine::{CorpusEngine, EmbedFn};
use kernel_types::judgement::Reason;
use kernel_types::{ComputeAttribution, Judgement, NodeId, Server};
use oicp_types::{JobKind, JobRequirements, JobUnit};
use serde_json::json;
use sovereign_mesh::ingest_executor::{IngestPayload, INGEST_KIND};
use tempfile::TempDir;

/// The `<corpus>-partition-<node>` suffix each simulated donor writes under.
/// Real values are node-id hex; the shape is all that is load-bearing here.
const LEADER_NODE: &str = "node-leader";
const PEER_NODE: &str = "node-peer";

const EMBED_DIM: usize = 4;

/// A term that appears ONLY in the leader donor's slice, and one that appears
/// ONLY in the peer donor's. The whole verdict turns on whether the second is
/// reachable in the leader's canonical index.
const LEADER_ONLY_TERM: &str = "quokka";
const PEER_ONLY_TERM: &str = "narwhal";

// ─────────────────────────────────────────────────────────────────
// The fold — real signed ops, real admission, real projection
// ─────────────────────────────────────────────────────────────────

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn actor(seed: u8) -> ActorKey {
    ActorKey::parse(actor_of(&key(seed))).expect("actor_of emits canonical hex")
}

/// Two donors in one ring. `leader` is also the submitter — which is what the
/// pre-registration names as the merge leader (`WorkHandoff.submitter`,
/// `projection.rs:274`), so the leader's node is where a collector would run.
fn ring() -> Roster {
    let mut m = BTreeMap::new();
    m.insert(Person::from("leader"), vec![actor_of(&key(1))]);
    m.insert(Person::from("peer"), vec![actor_of(&key(2))]);
    Roster::new(m)
}

fn sign(seed: u8, ts: i64, seq: u64, act: &WorkAct) -> Op<SignedOp> {
    let k = key(seed);
    let inner = RailAct::Record {
        payload: commonwealth_work::to_payload(act).expect("a well-formed work act"),
    };
    let sig = sign_ring_op(&k, WORK_NAMESPACE, ts, seq, &body_json(&inner));
    Op::new(
        SignedOp {
            seq,
            sig,
            act: inner,
        },
        ts,
        actor_of(&k),
    )
}

fn provenance(node: u128, name: &str) -> ComputeAttribution {
    ComputeAttribution {
        repo_rev: "cw-lift-5g".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        toolchain: "rustc 1.90.0".into(),
        host: Server::Peer {
            node: NodeId::from_u128(node),
            name: name.into(),
        },
    }
}

/// One real `ingest:v1` unit — sealed by the plane's own sealer over a real
/// [`IngestPayload`], so the unit hash is the one production would compute.
fn ingest_unit(corpus: &str, unit_id: u32, start: u64, end: u64) -> JobUnit {
    let payload = serde_json::to_value(IngestPayload::slice(
        corpus,
        corpus,
        unit_id,
        WorkUnit::JsonlRange { start, end },
    ))
    .expect("an IngestPayload encodes");
    commonwealth_work::seal::seal(
        JobKind::parse(INGEST_KIND).expect("`ingest:v1` parses"),
        payload,
        JobRequirements::any(),
        None,
    )
    .expect("seal")
}

fn unit_ref(handoff: HandoffId, unit: &JobUnit) -> UnitRef {
    UnitRef {
        handoff,
        unit_hash: unit.unit_hash.clone(),
    }
}

fn completion(
    handoff: HandoffId,
    unit: &JobUnit,
    corpus: &str,
    partition: &str,
    node: u128,
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
            "partition_path": partition,
        }),
        provenance: provenance(node, name),
    })
}

/// The scenario every reading in this file shares: one handoff, two
/// `ingest:v1` units for one corpus, leased and completed by two DIFFERENT
/// actors, folded to terminal.
///
/// Returns the projection and the handoff id. Asserts on the way out that the
/// fold really did reach `Complete` with two distinct lessees — if that ever
/// stops holding, the disk assertions below would be answering a question
/// nobody asked.
fn terminal_handoff(corpus: &str) -> (WorkProjection, HandoffId, [JobUnit; 2]) {
    let handoff = HandoffId::from_u128(5_000_002); // stable, arbitrary
    let a = ingest_unit(corpus, 0, 0, 100);
    let b = ingest_unit(corpus, 1, 100, 200);

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
            &completion(handoff, &a, corpus, LEADER_NODE, 1, "leader"),
        ),
        sign(
            2,
            310,
            1,
            &completion(handoff, &b, corpus, PEER_NODE, 2, "peer"),
        ),
    ];

    let projection =
        WorkProjection::fold(&admit(&ops, &[], &ring(), WORK_NAMESPACE, &Ed25519Verifier));

    let now_ms = 400_000u64;
    let h = projection
        .handoffs
        .get(&handoff)
        .expect("the submission was admitted");

    assert_eq!(
        h.phase_at(now_ms),
        HandoffPhase::Complete,
        "the scenario requires a TERMINAL handoff — every signal green — before \
         anything is asked of the corpus. Got {:?}",
        h.phase_at(now_ms),
    );

    let lessees: Vec<ActorKey> = [&a, &b]
        .iter()
        .map(|u| match h.units[&u.unit_hash].status_at(now_ms) {
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

    (projection, handoff, [a, b])
}

// ─────────────────────────────────────────────────────────────────
// The disk — two nodes are two index dirs
// ─────────────────────────────────────────────────────────────────

fn engine_at(index_dir: &std::path::Path, node: &str) -> Arc<CorpusEngine> {
    let recipes = index_dir.join("..").join("recipes");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    let embed: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.25_f32; EMBED_DIM]) }));
    Arc::new(CorpusEngine::new(recipes, index_dir.to_path_buf(), embed).with_self_node_id(node))
}

/// Write one donor's finished slice into the partition directory the real
/// [`sovereign_mesh::ingest_executor::IngestExecutor`] would have chosen —
/// `CorpusEngine::partition_path`, called here rather than re-spelled, so a
/// change to the layout breaks this test rather than silently detaching it.
async fn write_donor_partition(
    index_dir: &std::path::Path,
    node: &str,
    corpus: &str,
    unit_id: u32,
    term: &str,
) {
    let engine = engine_at(index_dir, node);
    let path = engine.partition_path(corpus);

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
                },
                vec![0.25_f32; EMBED_DIM],
            )
        })
        .collect();
    index.insert_batch(&rows).await.expect("insert_batch");

    // The engine clears this when the full pipeline finishes; `auto_recover`
    // refuses to merge a partition still flagged in-progress
    // (`auto_recover.rs:207-218`), so a simulated donor that skipped it would
    // fail this test for a setup reason rather than for the gap.
    index
        .mark_ingestion_complete()
        .expect("mark the slice finished");
}

/// What the leader's canonical corpus actually holds, after whatever merge the
/// fold path could reach has run.
#[derive(Debug)]
struct CanonicalProbe {
    outcome: String,
    canonical_exists: bool,
    chunk_count: u64,
    leader_term_reachable: bool,
    peer_term_reachable: bool,
}

/// Run the ONLY merge a donor node can reach on the fold path, then read the
/// canonical back.
///
/// `try_recover_stranded_partitions` is not chosen for convenience: it is the
/// single merge call in `auto_ingest`'s sweep (`auto_ingest.rs:297`), and the
/// fold path reaches no other. `coordinate_merge` is unreachable — see the
/// module docs.
async fn merge_as_the_fold_path_can_and_probe(
    index_dir: &std::path::Path,
    corpus: &str,
) -> CanonicalProbe {
    let outcome = format!(
        "{:?}",
        commonwealth_api::auto_recover::try_recover_stranded_partitions(index_dir, corpus).await
    );

    let canonical = index_dir.join(corpus);
    let Ok(index) = CorpusIndex::open(&canonical).await else {
        return CanonicalProbe {
            outcome,
            canonical_exists: false,
            chunk_count: 0,
            leader_term_reachable: false,
            peer_term_reachable: false,
        };
    };
    let info = index.info().await.expect("canonical info");
    let reachable = |term: &'static str| {
        let index = &index;
        async move {
            index
                .search(&[0.25_f32; EMBED_DIM], term, 10)
                .await
                .map(|hits| hits.iter().any(|h| h.content.contains(term)))
                .unwrap_or(false)
        }
    };
    CanonicalProbe {
        outcome,
        canonical_exists: true,
        chunk_count: info.chunk_count,
        leader_term_reachable: reachable(LEADER_ONLY_TERM).await,
        peer_term_reachable: reachable(PEER_ONLY_TERM).await,
    }
}

// ─────────────────────────────────────────────────────────────────
// The reading
// ─────────────────────────────────────────────────────────────────

/// **THE GAP.** Two donors, one corpus, one handoff, both units `Complete` —
/// and the leader's canonical corpus is missing the peer donor's chunks.
///
/// Failing input, named (ARCH §18.1): the current tree. Nothing turns a
/// terminal handoff and its two lessees into a shard collection, so the peer's
/// partition stays on the peer's disk and the merge the leader CAN reach —
/// `auto_recover`, which walks `<corpus>-partition-*/` under one index dir —
/// never sees it.
///
/// The negative control below is what makes this failure mean something: it
/// runs the same harness with both partitions on ONE node and must PASS. Read
/// them as a pair or read neither.
#[tokio::test]
#[ignore = "cw-lift 5g part 2: the fold-path shard collector does not exist yet — \
            this is a WATCHED RED, not a passing test. Remove the ignore with the fix."]
async fn two_donors_on_two_nodes_leave_the_canonical_missing_the_peers_chunks() {
    // A corpus id unique to this test: `auto_recover` keeps a process-global
    // 5-minute per-corpus cooldown (`auto_recover.rs:70-75`), so sharing an id
    // with the control below would make whichever ran second return
    // `InCooldown` and prove nothing.
    const CORPUS: &str = "cw-lift-5g-two-nodes";

    let (_projection, _handoff, _units) = terminal_handoff(CORPUS);

    // Two nodes are two index directories. Nothing in the fold path copies
    // between them.
    let leader_home = TempDir::new().expect("leader tempdir");
    let peer_home = TempDir::new().expect("peer tempdir");
    let leader_dir = leader_home.path().join("indexes");
    let peer_dir = peer_home.path().join("indexes");
    std::fs::create_dir_all(&leader_dir).expect("leader index dir");
    std::fs::create_dir_all(&peer_dir).expect("peer index dir");

    write_donor_partition(&leader_dir, LEADER_NODE, CORPUS, 0, LEADER_ONLY_TERM).await;
    write_donor_partition(&peer_dir, PEER_NODE, CORPUS, 1, PEER_ONLY_TERM).await;

    let probe = merge_as_the_fold_path_can_and_probe(&leader_dir, CORPUS).await;

    assert!(
        probe.peer_term_reachable,
        "the canonical corpus is missing the peer donor's chunks.\n\
         \n\
         The handoff is TERMINAL and both units are `Complete`, reported by two \
         different lessees — every signal on the work plane is green. The peer \
         donor's two chunks (searchable by `{PEER_ONLY_TERM}`) were written to its \
         own `{CORPUS}-partition-{PEER_NODE}/` and nothing on the fold path \
         fetched them.\n\
         \n\
         merge outcome on the leader : {}\n\
         canonical exists            : {}\n\
         canonical chunk_count       : {} (expected 4: 2 local + 2 peer)\n\
         leader-only term reachable  : {}\n\
         peer-only term reachable    : {}\n\
         \n\
         `leader-only term reachable = true` says the harness DID merge and \
         missed the peer; `false` with no canonical says the only merge the fold \
         path can reach declined outright. Both are this gap. See \
         `quality/campaigns/cw-lift-5g-part2-prereg.md` B1.",
        probe.outcome,
        probe.canonical_exists,
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
    );

    assert_eq!(
        probe.chunk_count, 4,
        "both donors' slices must land: 2 chunks each. Got {} — a reachable \
         peer term with the wrong count is a partial merge, which is the same \
         defect wearing a passing search.",
        probe.chunk_count,
    );
}

/// **THE NEGATIVE CONTROL.** The same scenario at n=1 — part 1's proof shape:
/// two units, one machine, one partition directory per unit but both under one
/// index dir. This MUST pass.
///
/// Without it, the red above could be a harness that never merges anything at
/// all and nobody would know (ARCH §18.1: a check with no failing input you
/// can name, inverted — a red with no green you can name).
#[tokio::test]
#[ignore = "cw-lift 5g part 2: paired negative control for the watched red above; \
            run it with the same filter so the pair is read together."]
async fn two_donors_on_one_node_do_land_both_slices_in_the_canonical() {
    const CORPUS: &str = "cw-lift-5g-one-node";

    let (_projection, _handoff, _units) = terminal_handoff(CORPUS);

    let home = TempDir::new().expect("tempdir");
    let index_dir = home.path().join("indexes");
    std::fs::create_dir_all(&index_dir).expect("index dir");

    // The ONLY difference from the reading above: both partitions are under
    // one index dir, which is what a single machine running two units produces.
    write_donor_partition(&index_dir, LEADER_NODE, CORPUS, 0, LEADER_ONLY_TERM).await;
    write_donor_partition(&index_dir, PEER_NODE, CORPUS, 1, PEER_ONLY_TERM).await;

    let probe = merge_as_the_fold_path_can_and_probe(&index_dir, CORPUS).await;

    assert!(
        probe.canonical_exists && probe.leader_term_reachable && probe.peer_term_reachable,
        "the negative control failed, so the red above proves NOTHING — this \
         harness does not merge even when both partitions are on one node. \
         merge outcome: {} | canonical: {} | chunks: {} | leader term: {} | \
         peer term: {}",
        probe.outcome,
        probe.canonical_exists,
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
    );
    assert_eq!(probe.chunk_count, 4, "2 chunks from each of the two units");
}
