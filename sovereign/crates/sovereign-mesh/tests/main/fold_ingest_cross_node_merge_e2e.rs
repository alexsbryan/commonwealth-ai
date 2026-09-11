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
//! (`sovereign_api::auto_recover::try_recover_stranded_partitions`, which
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
//! * [`sovereign_mesh::ingest_executor::fold_coverage_for`] — a pure read.
//!   Given a folded journal it answers "who worked on this corpus, where do
//!   their partitions live, and do I lead this handoff?". Measured by
//!   [`the_fold_names_both_verified_donors_and_where_to_find_them`] (B2, first
//!   half) and [`only_the_submitter_reads_a_merge_out_of_the_fold`] (B3). No
//!   corpus, no disk, no clock beyond the `now_ms` it is handed.
//!
//! * [`sovereign_api::auto_recover::merge_from_fold_coverage`] → the
//!   `ShardManager::merge_participants` the fold's answer is handed to.
//!   Measured by
//!   [`two_donors_on_two_nodes_land_both_slices_in_the_canonical`] (B2, second
//!   half), which is the same scenario as B1's red with the collector wired.
//!
//! The bar is a QUERY, not a chunk count. A count can be right for the wrong
//! reason — two chunks merged twice is four. What is asserted is that a term
//! appearing ONLY in the remote donor's slice comes back from a search against
//! the merged canonical — **reached through `CorpusEngine::usable_indexes()`**,
//! with the corpus present in `installed_indexes()`, which is the list
//! `hosted_corpora` gossip is built from.
//!
//! That altitude is not incidental. The first reading of B2 probed with
//! `CorpusIndex::open` on the canonical path, which bypasses both of those
//! gates, and passed over a corpus that `installed_indexes()` could not see at
//! all. Both probes are read now and they print side by side, because
//! "the peer's chunks never arrived" and "the chunks arrived and nothing can
//! route to them" are different defects that a single reading conflates.
//!
//! What this file drives for real
//! ------------------------------
//! * The real fold. Ed25519-signed ops through `commonwealth_rail::admit` and
//!   `WorkProjection::fold`, not a hand-built one — two units, two DIFFERENT
//!   lessees, one handoff — over real `ingest:v1` units sealed by
//!   `commonwealth_work::seal::seal` over real `IngestPayload` bodies.
//! * The real partition layout: `CorpusEngine::partition_path`, the same call
//!   `IngestExecutor::run` makes to choose where a slice lands
//!   (`ingest_executor.rs`).
//! * **The real wire.** The peer donor's partition is served by the peer's own
//!   `sovereign_api::server::internal_router` on a real loopback socket,
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
use std::sync::Arc;

use sovereign_api::auto_recover::{merge_from_fold_coverage, RecoveryOutcome};
use sovereign_api::server::internal_router;
use sovereign_api::state::AppState;
use sovereign_meshapp_registry::AppRegistry;
use commonwealth_core::ids::{HandoffId, MeshId};
use commonwealth_core::knowledge::{HandoffPhase, WorkUnit};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    actor_of, admit, body_json, sign_ring_op, Ed25519Verifier, Op, Person, RailAct, Roster,
    SignedOp, SigningKey,
};
use commonwealth_state::MeshStore;
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::{ActorKey, Completion, Submission, UnitRef, WorkAct, WORK_NAMESPACE};
use corpus_engine::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use corpus_engine::{CorpusEngine, EmbedFn};
use kernel_types::judgement::Reason;
use kernel_types::{ComputeAttribution, Judgement, NodeId, Server};
use oicp_types::{JobKind, JobRequirements, JobUnit};
use serde_json::json;
use sovereign_mesh::ingest_executor::{fold_coverage_for, IngestPayload, INGEST_KIND};
use tempfile::TempDir;

use crate::common;
use crate::common::corpus_at;

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

/// One real `ingest:v1` unit — sealed by the plane's own sealer over a real
/// [`IngestPayload`], so the unit hash is the one production would compute.
pub(crate) fn ingest_unit(corpus: &str, unit_id: u32, start: u64, end: u64) -> JobUnit {
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
/// in memory, and `fold_ingest_coverage_refusal_e2e` writes it onto a real
/// `RingRail` so `auto_ingest`'s own tick folds it. One spelling (ARCH §10.6).
pub(crate) fn terminal_handoff_ops(corpus: &str) -> (Vec<Op<SignedOp>>, HandoffId) {
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
pub(crate) fn terminal_handoff(corpus: &str) -> (WorkProjection, HandoffId) {
    let (ops, handoff) = terminal_handoff_ops(corpus);
    let a = ingest_unit(corpus, 0, 0, 100);
    let b = ingest_unit(corpus, 1, 100, 200);

    let projection =
        WorkProjection::fold(&admit(&ops, &[], &ring(), WORK_NAMESPACE, &Ed25519Verifier));

    let h = projection
        .handoffs
        .get(&handoff)
        .expect("the submission was admitted");

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

fn embed_fn() -> EmbedFn {
    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.25_f32; EMBED_DIM]) }))
}

/// A `CorpusEngine` rooted at `index_dir` and told it is `node` — the identity
/// `partition_path` and `index_serve` both name the partition dir from.
pub(crate) fn engine_at(index_dir: &std::path::Path, node: NodeId) -> Arc<CorpusEngine> {
    let recipes = index_dir.join("..").join("recipes");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    Arc::new(
        CorpusEngine::new(recipes, index_dir.to_path_buf(), embed_fn())
            .with_self_node_id(node.to_string()),
    )
}

/// Write one donor's finished slice into the partition directory the real
/// [`sovereign_mesh::ingest_executor::IngestExecutor`] would have chosen —
/// `CorpusEngine::partition_path`, called here rather than re-spelled, so a
/// change to the layout breaks this test rather than silently detaching it.
pub(crate) async fn write_donor_partition(
    index_dir: &std::path::Path,
    node: NodeId,
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
    // refuses to merge a partition still flagged in-progress, so a simulated
    // donor that skipped it would fail this test for a setup reason rather
    // than for the gap.
    index
        .mark_ingestion_complete()
        .expect("mark the slice finished");
}

/// An `AppState` for one node: its own id, its own index dir, and a mesh
/// holding whatever `others` are reachable from it.
///
/// The mesh is what `peer_control_urls` reads to turn a `NodeId` from the fold
/// into a base URL, so a donor that is not a member here is unreachable —
/// which is the production behaviour, not a shortcut.
pub(crate) fn node_state(
    self_id: NodeId,
    index_dir: &std::path::Path,
    others: &[(NodeId, &str)],
) -> AppState {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        common::member(self_id, "self", "127.0.0.1:9742".parse().expect("addr")),
    );
    for (id, addr) in others {
        members.insert(
            *id,
            common::member(*id, "donor", addr.parse().expect("peer addr")),
        );
    }
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "cw-lift 5g part 2".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    AppState::new_with_platform_and_engine(
        self_id,
        mesh,
        Arc::new(MeshStore::in_memory().expect("in-memory mesh store")),
        Arc::new(AppRegistry::new()),
        Some(engine_at(index_dir, self_id)),
    )
}

/// Does a search of `index` return a chunk carrying `term`?
///
/// One spelling, used by both probes below, so that "reachable" cannot come
/// to mean two slightly different things at two altitudes.
pub(crate) async fn term_reachable(index: &CorpusIndex, term: &str) -> bool {
    index
        .search(&[0.25_f32; EMBED_DIM], term, 10)
        .await
        .map(|hits| hits.iter().any(|h| h.content.contains(term)))
        .unwrap_or(false)
}

/// What the leader's canonical corpus DIRECTORY actually holds after the
/// merge — opened by path, with `CorpusIndex::open`.
///
/// This answers "are the bytes on disk and are they retrievable from a handle
/// that already exists". It deliberately does NOT answer "can anyone reach
/// this corpus", which is [`InstalledProbe`]'s question; the two disagreed for
/// the whole of `df2ffecb8`, and telling them apart is the point of having
/// both.
#[derive(Debug)]
pub(crate) struct CanonicalProbe {
    pub(crate) canonical_exists: bool,
    pub(crate) chunk_count: u64,
    pub(crate) leader_term_reachable: bool,
    pub(crate) peer_term_reachable: bool,
}

pub(crate) async fn probe_canonical(index_dir: &std::path::Path, corpus: &str) -> CanonicalProbe {
    let canonical = corpus_at(index_dir, corpus).root();
    let Ok(index) = CorpusIndex::open(&canonical).await else {
        return CanonicalProbe {
            canonical_exists: false,
            chunk_count: 0,
            leader_term_reachable: false,
            peer_term_reachable: false,
        };
    };
    let info = index.info().await.expect("canonical info");
    CanonicalProbe {
        canonical_exists: true,
        chunk_count: info.chunk_count,
        leader_term_reachable: term_reachable(&index, LEADER_ONLY_TERM).await,
        peer_term_reachable: term_reachable(&index, PEER_ONLY_TERM).await,
    }
}

/// The canonical as THE REST OF THE SYSTEM sees it, which is the altitude a
/// user's query and a peer's gossip actually arrive at.
///
/// `CorpusIndex::open` on a path bypasses both gates a merged corpus has to
/// pass, and a bar asserted through it proves the bytes landed, not that the
/// corpus works. Measured on `df2ffecb8`, which passed B2 through that probe:
/// two canonical directories on disk, `installed_indexes()` → 0 rows,
/// `hosted_corpora` → `[]`.
///
/// The two accessors here are those gates, and they are the ones the product
/// goes through:
///
/// * `CorpusEngine::installed_indexes()` gates on `is_ingestion_complete`
///   (`engine/mod.rs`). It is the list
///   `sovereign_mesh::capabilities::build_local_capabilities` walks and hands
///   straight to `build_hosted_corpora` (`capabilities.rs:103` → `:122`), so a
///   corpus missing from it is advertised to NO peer.
/// * `CorpusEngine::usable_indexes()` gates additionally on `indexes_built`
///   and is corpus-engine's own single decider for "can I search it".
///
/// `hosted_corpora` is asserted through its input rather than by calling
/// `build_local_capabilities`: `build_hosted_corpora` is private, and the
/// public entry point detects hardware and probes GPU VRAM — a lot of machine
/// to drag into a merge test for a list it copies out of `installed_indexes()`
/// unchanged apart from the `query_sharing` filter.
#[derive(Debug)]
pub(crate) struct InstalledProbe {
    /// `corpus_id`s from `installed_indexes()` — the gossip term.
    pub(crate) installed: Vec<String>,
    /// `corpus_id`s from `usable_indexes()` — the searchable term.
    pub(crate) usable: Vec<String>,
    /// Every directory under the index dir carrying a corpus meta, so a
    /// reading of "zero rows" can be told apart from "nothing was written".
    pub(crate) dirs_on_disk: Vec<String>,
    /// `Some` only when the corpus reached `usable_indexes()` and the search
    /// therefore RAN, through the engine's own by-id accessor. `None` means
    /// the question was never asked — a different reading from asked-and-
    /// missed, and not defaulted into one (ARCH §18.3).
    pub(crate) leader_term_reachable: Option<bool>,
    pub(crate) peer_term_reachable: Option<bool>,
}

pub(crate) async fn probe_installed(index_dir: &std::path::Path, corpus: &str) -> InstalledProbe {
    let engine = engine_at(index_dir, leader_node());
    let ids = |rows: Vec<corpus_engine::IndexInfo>| -> Vec<String> {
        rows.into_iter().map(|i| i.corpus_id).collect()
    };
    let installed = ids(engine
        .installed_indexes()
        .await
        .expect("installed_indexes must not fail on a temp dir this test owns"));
    let usable = ids(engine
        .usable_indexes()
        .await
        .expect("usable_indexes must not fail on a temp dir this test owns"));

    let mut dirs_on_disk: Vec<String> = std::fs::read_dir(index_dir)
        .expect("index dir")
        .flatten()
        .filter(|e| corpus_engine::Corpus::meta_in(e.path()).exists())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    dirs_on_disk.sort();

    let (leader_term_reachable, peer_term_reachable) = if usable.iter().any(|c| c == corpus) {
        // Through the engine's by-id accessor, not a hand-built path:
        // "the surface a user reaches" is the whole claim.
        let index = engine
            .open_index_for_corpus(corpus)
            .await
            .expect("a corpus usable_indexes() listed must open");
        (
            Some(term_reachable(&index, LEADER_ONLY_TERM).await),
            Some(term_reachable(&index, PEER_ONLY_TERM).await),
        )
    } else {
        (None, None)
    };

    InstalledProbe {
        installed,
        usable,
        dirs_on_disk,
        leader_term_reachable,
        peer_term_reachable,
    }
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
#[test]
fn the_fold_names_both_verified_donors_and_where_to_find_them() {
    const CORPUS: &str = "cw-lift-5g-coverage";
    let (projection, handoff) = terminal_handoff(CORPUS);

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
#[test]
fn only_the_submitter_reads_a_merge_out_of_the_fold() {
    use std::io::Write;
    use std::sync::Mutex;

    const CORPUS: &str = "cw-lift-5g-leader";
    let (projection, _handoff) = terminal_handoff(CORPUS);

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

/// **B2 (second half) — THE BAR.** Two donors, two nodes, one corpus, one
/// handoff. The leader's canonical corpus contains BOTH donors' chunks, and
/// the term that only the REMOTE donor's slice carries is reachable by search
/// **through `usable_indexes()`** — the accessor a user's query goes through —
/// with the corpus present in `installed_indexes()`, which is what
/// `hosted_corpora` gossip advertises from.
///
/// This is B1's scenario with the collector wired. B1 measured
/// `Recovered { chunks: 2 }` here with `peer-only term reachable : false`.
///
/// # Why the altitude, and why this assertion changed
///
/// Until 2026-09-09 this test probed with `CorpusIndex::open` on the canonical
/// path, which bypasses both gates a merged corpus must pass. It went green on
/// `df2ffecb8` over a canonical that `installed_indexes()` returned zero rows
/// for and that `hosted_corpora` gossip advertised as `[]` — the merge wrote
/// the chunks and never called `build_indexes` / `mark_indexes_built` /
/// `mark_ingestion_complete` / the fingerprint stamp. What that green proved
/// was that the bytes were on disk, which is not the bar this
/// pre-registration set. The bar says a query only the remote donor's slice
/// can satisfy must be answerable, "because a count can be right for the wrong
/// reason" — and a query nothing routes to is the same failure one level up.
///
/// Failing input, named and watched (ARCH §18.1): two of them, and they print
/// differently, which is the reason [`InstalledProbe`] and [`CanonicalProbe`]
/// are both read here.
///
/// 1. *The participant set.* Truncate `coverage.nodes` to the local node
///    before the merge — the shape of the original defect, where participants
///    come from local disk instead of the fold. The merge resolves one shard
///    and the canonical comes back with 2 chunks and `narwhal` unreachable.
/// 2. *The finalize.* Drop `corpus_engine::finalize_canonical` from
///    `merge_from_fold_coverage`. Both donors' chunks are on disk and
///    retrievable through `CorpusIndex::open`, and `peer-only term
///    (installed)` is `None`: `installed_indexes()` and `usable_indexes()`
///    both return zero rows beside two canonical directories.
///
/// The count assertion is deliberately kept BELOW the query assertion: the
/// query is the bar and the count is corroboration. A right count with an
/// unreachable term would be a merge that wrote rows nothing can retrieve.
#[tokio::test]
async fn two_donors_on_two_nodes_land_both_slices_in_the_canonical() {
    const CORPUS: &str = "cw-lift-5g-two-nodes";

    let (projection, _handoff) = terminal_handoff(CORPUS);

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
    let peer_state = node_state(peer_node(), &peer_dir, &[]);
    let peer_addr = common::spawn_router(internal_router(peer_state)).await;

    // The leader knows the peer only as a mesh member with an address, which
    // is what `peer_control_urls` resolves through `PeerTransport::endpoints`.
    let leader_state = node_state(
        leader_node(),
        &leader_dir,
        &[(peer_node(), &peer_addr.to_string())],
    );

    // ── The collector: the fold's answer, handed to the merge ──
    let coverage = fold_coverage_for(&projection, &actor(1), CORPUS, NOW_MS)
        .expect("the submitter leads a terminal ingest:v1 handoff for this corpus");

    let outcome = merge_from_fold_coverage(
        &leader_state,
        CORPUS,
        coverage.handoff_id,
        &coverage.nodes,
        coverage.expected,
    )
    .await;

    // The BAR is read at the altitude a user reaches: the corpus has to be
    // in `usable_indexes()` for the search to run at all, and in
    // `installed_indexes()` for any peer to be told it exists.
    let seen = probe_installed(&leader_dir, CORPUS).await;
    // The disk-level reading is kept as CORROBORATION and as the thing that
    // tells the two failure shapes apart: bytes-missing versus bytes-present-
    // and-unreachable print differently below.
    let probe = probe_canonical(&leader_dir, CORPUS).await;

    assert_eq!(
        seen.peer_term_reachable,
        Some(true),
        "the peer donor's slice is not reachable through the surface a user \
         reaches.\n\
         \n\
         `peer-only term (installed)` is `None` when the corpus never reached \
         `usable_indexes()` — the canonical exists on disk and NOTHING can see \
         it: not local search, and not `hosted_corpora` gossip, which is built \
         from `installed_indexes()` (`capabilities.rs:103` → `:122`). That is a \
         different failure from the peer's chunks never arriving, and the \
         disk-level rows below say which one this is.\n\
         \n\
         The peer donor's two chunks (searchable by `{PEER_ONLY_TERM}`) were \
         written to its own `{}/` on a different index dir and served from a \
         different socket. The fold named both donors; the merge was supposed \
         to pull the second AND finish the canonical.\n\
         \n\
         merge outcome               : {outcome:?}\n\
         fold coverage               : expected={} nodes={:?}\n\
         installed_indexes()         : {:?}\n\
         usable_indexes()            : {:?}\n\
         canonical dirs on disk      : {:?}\n\
         leader-only term (installed): {:?}\n\
         peer-only term (installed)  : {:?}\n\
         --- through CorpusIndex::open, which bypasses both gates ---\n\
         canonical exists            : {}\n\
         canonical chunk_count       : {} (expected 4: 2 local + 2 peer)\n\
         leader-only term (on disk)  : {}\n\
         peer-only term (on disk)    : {}\n\
         \n\
         See `quality/campaigns/cw-lift-5g-part2-prereg.md` B2.",
        corpus_at("", CORPUS)
            .partition(&peer_node().to_string())
            .display(),
        coverage.expected,
        coverage.nodes,
        seen.installed,
        seen.usable,
        seen.dirs_on_disk,
        seen.leader_term_reachable,
        seen.peer_term_reachable,
        probe.canonical_exists,
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
    );
    assert_eq!(
        seen.leader_term_reachable,
        Some(true),
        "the LOCAL donor's chunks are unreachable through `usable_indexes()`, \
         which is a different defect than the one this bar is about and a worse \
         one. outcome: {outcome:?} | installed: {:?} | usable: {:?}",
        seen.installed,
        seen.usable,
    );
    assert!(
        seen.installed.iter().any(|c| c == CORPUS),
        "the canonical is not in `installed_indexes()`, so \
         `build_hosted_corpora` advertises it to no peer on the mesh \
         (`capabilities.rs:103` → `:122`). installed: {:?} | dirs on disk: {:?} \
         | outcome: {outcome:?}",
        seen.installed,
        seen.dirs_on_disk,
    );
    assert_eq!(
        probe.chunk_count, 4,
        "both donors' slices must land whole: 2 chunks each. Got {} — a \
         reachable peer term with the wrong count is a partial merge, which is \
         the same defect wearing a passing search. outcome: {outcome:?}",
        probe.chunk_count,
    );

    match outcome {
        RecoveryOutcome::Recovered {
            chunks,
            shards_covered,
        } => {
            assert_eq!(chunks, 4, "the reported chunk count must be the real one");
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

/// **THE NEGATIVE CONTROL, retained from B1.** The same scenario at n=1 —
/// part 1's proof shape: two units, one machine, both partition directories
/// under one index dir, merged by the DISK-derived path this rung does not
/// change (`try_recover_stranded_partitions`).
///
/// It is kept because it controls for the fixture, not for the collector: if
/// two partitions written by `write_donor_partition` cannot merge into a
/// searchable canonical at all, then B2's green above would be about a
/// different corpus shape than the one the reds were measured on. It also
/// pins that the legacy path still works, which the collector's `continue`
/// arm still falls through to for every corpus the fold cannot speak for.
#[tokio::test]
async fn two_donors_on_one_node_do_land_both_slices_in_the_canonical() {
    const CORPUS: &str = "cw-lift-5g-one-node";

    let home = TempDir::new().expect("tempdir");
    let index_dir = home.path().join("indexes");
    std::fs::create_dir_all(&index_dir).expect("index dir");

    // The ONLY difference from the reading above: both partitions are under
    // one index dir, which is what a single machine running two units produces.
    write_donor_partition(&index_dir, leader_node(), CORPUS, 0, LEADER_ONLY_TERM).await;
    write_donor_partition(&index_dir, peer_node(), CORPUS, 1, PEER_ONLY_TERM).await;

    let outcome = format!(
        "{:?}",
        sovereign_api::auto_recover::try_recover_stranded_partitions(&index_dir, CORPUS).await
    );
    let probe = probe_canonical(&index_dir, CORPUS).await;

    assert!(
        probe.canonical_exists && probe.leader_term_reachable && probe.peer_term_reachable,
        "the negative control failed, so B2's green proves LESS than it looks — \
         this fixture cannot merge two partitions into a searchable canonical \
         even when both are on one node. merge outcome: {outcome} | canonical: \
         {} | chunks: {} | leader term: {} | peer term: {}",
        probe.canonical_exists,
        probe.chunk_count,
        probe.leader_term_reachable,
        probe.peer_term_reachable,
    );
    assert_eq!(probe.chunk_count, 4, "2 chunks from each of the two units");
}
