// SPDX-License-Identifier: AGPL-3.0-or-later
//! The thirteen MeshApp explorer reads the desktop stops carrying
//! privately (sv-surface D3, `meshapp_http`).
//!
//! Exercised against a REAL daemon over a real corpus index with a real
//! `atlas/` on disk, for `atlas_surface_e2e`'s reason: the fault these
//! routes exist to prevent is the desktop and the daemon reading the
//! SAME files through two code paths and disagreeing. A stubbed reader
//! would pass on the day they diverged.
//!
//! # Red-watch (2026-09-10, run, not asserted)
//!
//! All thirteen routes were taken back OUT of `meshapp_router` —
//! handlers, DTOs and this file left intact, so the suite still built —
//! and the cases re-run against the routerless daemon. Seven of eight
//! failed, `pass: 1 fail: 7`:
//!
//! ```text
//! graph_serves_the_atlas_entities_as_nodes        :202  left: 404  right: 200
//! node_detail_serves_incident_edges_…             :231  left: 404  right: 200
//!                                                       "a real entity must resolve"
//! entity_search_short_circuits_blank_…            :268  left: 404  right: 200
//!                                                       "a blank query is a successful empty answer"
//! subgraph_findings_and_reconciliation_…          :291  left: 404  right: 200
//! claims_and_questions_serve_the_atom_bodies      :324  left: 404  right: 200
//! stats_timeline_feed_and_chunk_read_…            :351  left: 404  right: 200
//! chunk_id_that_is_not_a_number_is_a_400_…        :396  left: 404  right: 400
//! ```
//!
//! The eighth needs its caveat in the file, not only in a log.
//! `uninstalled_corpus_is_a_404_naming_it_on_every_route` PASSED under
//! sabotage: a routerless daemon 404s too, and the loop's body check
//! reads `unwrap_or_default()`, so an empty 404 body satisfies it the
//! same way a named one does. That case guards the WORDING of a real
//! 404 and is not, on its own, evidence the route exists — the seven
//! above are (ARCH §18.1). The same caveat applies to the two 404
//! branches inside `node_detail_…` and `chunk_id_…`; both of those
//! tests went red on their 200/400 half, which is what makes them
//! gates.
//!
//! `wrapped_artifact_builds_over_the_daemons_corpus` was added after
//! that run and has not been watched red; it is the one case in this
//! file with no sabotage evidence, and it says so here rather than
//! borrowing the others'.
//!
//! # The fixture
//!
//! `sovereign-meshapp`'s own atlas shape — two Entities, one Relation
//! and one Event (the Event names a dangling participant, which the
//! adapter must drop) — plus a Claim and a Question so the two atom
//! routes have something to return, plus `chapters.json` so evidence
//! resolves to numeric chunk ids, plus two real chunks in the index
//! under two `source_doc_id`s so the document feed has documents to
//! group. Written inline rather than read from another crate's fixture
//! tree: reaching across breaks the first time either side moves.

use std::sync::Arc;

use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::meshapp_http::meshapp_router;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;
const CORPUS: &str = "governance";

const ATOMS_JSON: &str = r#"{
  "schema_version": "2.3",
  "atoms": [
    {"atom_type":"Entity","data":{
      "id":"entity-aaa","canonical_name":"El Paso","entity_type":"institution",
      "first_appearance":{"chunk_id":"sec_00002","passage_preview":"El Paso Corp."},
      "description":"Energy company.","salience":0.5,"enrichment_depth":"extracted",
      "aliases":["El Paso Corp.","PGET"]}},
    {"atom_type":"Entity","data":{
      "id":"entity-bbb","canonical_name":"Kenneth Lay","entity_type":"person",
      "first_appearance":{"chunk_id":"sec_00001","passage_preview":"Ken Lay"},
      "description":"Chairman.","salience":0.9,"enrichment_depth":"extracted"}},
    {"atom_type":"Relation","data":{
      "id":"relation-xyz","label":"counterparty_of",
      "participants":["entity-aaa","entity-bbb"],"relation_type":"association",
      "evidence":[{"chunk_id":"sec_00002","passage_preview":"El Paso and Lay discussed terms"}],
      "section_range":{"start":"sec_00002","end":"sec_00002"},"enrichment_depth":"extracted"}},
    {"atom_type":"Event","data":{
      "id":"event-pqr","description":"Lay emailed El Paso","event_type":"unspecified",
      "participants":["entity-bbb","entity-aaa","entity-ghost"],
      "evidence":[{"chunk_id":"sec_00001","passage_preview":"Date: Thu, 26 Jul 2001"}],
      "section_position":{"section_id":"sec_00001"},"enrichment_depth":"extracted"}},
    {"atom_type":"Claim","data":{
      "id":"claim-179b296698ec4911","content":"Quiet hours begin at 11 PM every night.",
      "discourse_act":"enact","epistemic_status":"confident","scope":"contextual",
      "evidence":[{"chunk_id":"sec_00002"}],"attributed_to":"entity-bbb",
      "claim_kind":"requires","enrichment_depth":"extracted"}},
    {"atom_type":"Question","data":{
      "id":"question-0001","content":"Who approved the quiet-hours change?",
      "question_type":"open","raised_at":[{"chunk_id":"sec_00001"}],
      "resolution_status":{"kind":"open"},"enrichment_depth":"extracted"}}
  ]
}"#;

const CHAPTERS_JSON: &str = r#"{"corpus_id":"governance","schema_version":"1.0","chapters":[
    {"id":"sec_00001","title":"Email A","chapter":1,"chunk_ids":[100]},
    {"id":"sec_00002","title":"Email B","chapter":2,"chunk_ids":[200,201]}
]}"#;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// A daemon whose `CorpusEngine` has one installed corpus carrying two
/// chunks under two source documents and the atlas above.
///
/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly. The scoped allow is
/// `atlas_surface_e2e`'s — `clippy.toml`'s `allow-unwrap-in-tests`
/// covers `#[test]` bodies only, and eight `?`s would turn a broken
/// fixture into a quieter failure.
#[allow(clippy::unwrap_used)]
async fn build_meshapp_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();

    let path = indexes.join(CORPUS);
    let index = CorpusIndex::create(
        &path,
        CORPUS,
        "Governance",
        "qwen3-embedding-0.6b",
        EMBED_DIM,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[
            (
                InsertChunk {
                    content: "Ken Lay emailed El Paso about the terms.".into(),
                    title: Some("Email A".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("doc-a".into()),
                    source_file: None,
                    code: Default::default(),
                    unit_id: None,
                },
                vec![0.0_f32; EMBED_DIM],
            ),
            (
                InsertChunk {
                    content: "Quiet hours begin at 11 PM every night.".into(),
                    title: Some("Email B".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("doc-b".into()),
                    source_file: None,
                    code: Default::default(),
                    unit_id: None,
                },
                vec![0.0_f32; EMBED_DIM],
            ),
        ])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();

    let atlas_dir = path.join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    std::fs::write(atlas_dir.join("atoms.json"), ATOMS_JSON).unwrap();
    std::fs::write(path.join("chapters.json"), CHAPTERS_JSON).unwrap();

    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_engine(engine),
    );
    (daemon, tmp)
}

/// `GET` one meshapp path and hand back status + parsed body.
async fn get(addr: &std::net::SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/meshapp/{path}"))
        .send()
        .await
        .expect("meshapp_router reachable");
    let status = resp.status().as_u16();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

// ── The four graph projections ────────────────────────────────────

/// `graph` returns the atlas entities as nodes, with the type and the
/// attributes the explorer renders — not four well-formed empties.
///
/// Asserts CONTENT, not a count: a route that answered two blank objects
/// would satisfy `len() == 2` and break every card in the pane.
#[tokio::test]
async fn graph_serves_the_atlas_entities_as_nodes() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/graph")).await;
    assert_eq!(
        status, 200,
        "GET /graph must serve the atlas graph: {body:#?}"
    );
    let nodes = body.as_array().expect("graph answers an array");
    assert_eq!(nodes.len(), 2, "two Entity atoms, two nodes: {body:#?}");

    let lay = nodes
        .iter()
        .find(|n| n["id"] == "entity-bbb")
        .expect("entity-bbb is in the graph");
    assert_eq!(lay["canonical_name"], "Kenneth Lay");
    assert_eq!(
        lay["entity_type"], "person",
        "the entity type must cross — the pane colours by it",
    );

    // The `limit` clamp is the ROUTE's now. An over-large ask is served
    // clamped, never refused (the desktop's `.min(500)`, moved).
    let (status, body) = get(&addr, &format!("{CORPUS}/graph?limit=100000")).await;
    assert_eq!(status, 200, "an over-large limit is clamped, not refused");
    assert_eq!(body.as_array().unwrap().len(), 2);
}

/// `nodes/{id}` returns one entity WITH its incident edges, each quoting
/// its evidence — and 404s on an id the graph does not carry.
#[tokio::test]
async fn node_detail_serves_incident_edges_and_404s_on_an_unknown_id() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/nodes/entity-bbb")).await;
    assert_eq!(status, 200, "a real entity must resolve");
    assert_eq!(body["canonical_name"], "Kenneth Lay");
    assert_eq!(body["entity_type"], "person");
    let edges = body["edges"].as_array().expect("edges array");
    assert!(
        !edges.is_empty(),
        "the Relation + Event atoms give this entity edges: {body:#?}",
    );

    // The dangling `entity-ghost` participant must NOT have become a node.
    let (_, graph) = get(&addr, &format!("{CORPUS}/graph")).await;
    assert!(
        !graph
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "entity-ghost"),
        "a participant that is not an Entity atom is dropped, not invented",
    );

    let (status, body) = get(&addr, &format!("{CORPUS}/nodes/entity-nope")).await;
    assert_eq!(status, 404, "an unknown entity id is an absence");
    assert!(
        body["error"].as_str().unwrap().contains("entity-nope"),
        "the 404 must NAME what was missing — the status line alone is \
         not a gate, a routerless daemon 404s too: {body:#?}",
    );
}

/// A blank search answers `[]` without touching the graph; a real one
/// discriminates.
#[tokio::test]
async fn entity_search_short_circuits_blank_and_discriminates_otherwise() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/entities?q=%20%20")).await;
    assert_eq!(status, 200, "a blank query is a successful empty answer");
    assert_eq!(body.as_array().unwrap().len(), 0);

    let (status, body) = get(&addr, &format!("{CORPUS}/entities?q=lay")).await;
    assert_eq!(status, 200);
    let hits = body.as_array().unwrap();
    assert_eq!(
        hits.len(),
        1,
        "case-folded substring matches Kenneth Lay and NOT El Paso — a \
         route that returned both would pass a non-empty check: {body:#?}",
    );
    assert_eq!(hits[0]["id"], "entity-bbb");
}

/// `subgraph`, `findings` and `reconciliation` each answer their own
/// shape, and an empty one where empty is the truth.
#[tokio::test]
async fn subgraph_findings_and_reconciliation_each_answer_their_own_shape() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/subgraph")).await;
    assert_eq!(status, 200);
    assert_eq!(
        body["nodes"].as_array().unwrap().len(),
        2,
        "the subgraph is nodes + induced edges, not a bare list: {body:#?}",
    );
    assert!(
        !body["edges"].as_array().unwrap().is_empty(),
        "induced edges must survive the wire: {body:#?}",
    );

    // An atlas carries no pattern findings — that is the adapter's
    // documented behaviour, and an EMPTY LIST is the right answer. A
    // 404 here would tell the pane the corpus is broken.
    let (status, body) = get(&addr, &format!("{CORPUS}/findings")).await;
    assert_eq!(status, 200, "'no findings' is an answer, not an absence");
    assert_eq!(body.as_array().unwrap().len(), 0);

    let (status, body) = get(&addr, &format!("{CORPUS}/reconciliation")).await;
    assert_eq!(status, 200);
    assert!(body.is_array(), "reconciliation answers a list: {body:#?}");
}

// ── The index-path projections ────────────────────────────────────

/// Claims and Questions come back as their own atom projections, with
/// the atom BODY intact — the pane renders the content, not the id.
#[tokio::test]
async fn claims_and_questions_serve_the_atom_bodies() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/claims")).await;
    assert_eq!(status, 200);
    let claims = body.as_array().unwrap();
    assert_eq!(claims.len(), 1, "one Claim atom in the fixture: {body:#?}");
    assert_eq!(
        claims[0]["content"], "Quiet hours begin at 11 PM every night.",
        "the claim body must cross verbatim",
    );

    let (status, body) = get(&addr, &format!("{CORPUS}/questions")).await;
    assert_eq!(status, 200);
    let questions = body.as_array().unwrap();
    assert_eq!(questions.len(), 1);
    assert_eq!(
        questions[0]["content"], "Who approved the quiet-hours change?",
        "the question body must cross verbatim",
    );
}

/// `stats`, `timeline`, `documents` and `chunks/{id}` all read the
/// daemon's index — and the feed's chunk id deep-links into the chunk
/// route, which is the actual UI flow.
#[tokio::test]
async fn stats_timeline_feed_and_chunk_read_the_daemons_index() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/stats")).await;
    assert_eq!(status, 200);
    assert!(
        body.is_object(),
        "stats answers one object, not a list: {body:#?}",
    );

    let (status, body) = get(&addr, &format!("{CORPUS}/timeline")).await;
    assert_eq!(
        status, 200,
        "timeline is a successful read even with no dates"
    );
    assert!(body.is_object(), "timeline answers one object: {body:#?}");

    let (status, feed) = get(&addr, &format!("{CORPUS}/documents")).await;
    assert_eq!(status, 200);
    let docs = feed["docs"].as_array().expect("the feed carries docs");
    assert_eq!(
        docs.len(),
        2,
        "two source_doc_ids, two documents: {feed:#?}",
    );

    // Deep-link: take a chunk id the feed handed out and read it back.
    let chunk_id = docs[0]["chunks"][0]["chunk_id"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| docs[0]["chunks"][0]["chunk_id"].as_u64())
        .expect("the feed names a numeric chunk id");
    let (status, body) = get(&addr, &format!("{CORPUS}/chunks/{chunk_id}")).await;
    assert_eq!(status, 200, "a chunk the feed just named must resolve");
    assert!(
        !body["content"].as_str().unwrap().is_empty(),
        "the chunk text must cross, not just its id: {body:#?}",
    );
}

/// A non-numeric chunk id is the caller's mistake (400); a numeric one
/// that is not in the index is an absence (404). Two different facts,
/// two different statuses, each naming itself.
#[tokio::test]
async fn chunk_id_that_is_not_a_number_is_a_400_and_a_missing_one_is_a_404() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/chunks/not-a-number")).await;
    assert_eq!(status, 400);
    assert!(
        body["error"].as_str().unwrap().contains("numeric"),
        "a malformed id must say so rather than read as 'chunk missing': {body:#?}",
    );

    let (status, body) = get(&addr, &format!("{CORPUS}/chunks/999999")).await;
    assert_eq!(status, 404);
    assert!(
        body["error"].as_str().unwrap().contains("999999"),
        "the 404 must name the chunk it could not find: {body:#?}",
    );
}

/// The Wrapped deck is the thirteenth route, and the only one that can
/// BUILD rather than read: the library serves its cached
/// `wrapped/all-time.json` when fresh and folds the corpus otherwise.
///
/// Asserted here as a real 200 over the fixture, because a route that
/// only ever returned the cache would look identical to one that works
/// until the first cold corpus.
#[tokio::test]
async fn wrapped_artifact_builds_over_the_daemons_corpus() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/wrapped")).await;
    assert_eq!(
        status, 200,
        "the Wrapped route must build over a cold corpus, not only serve \
         a cache somebody else wrote: {body:#?}",
    );
    assert!(
        body.is_object(),
        "the deck is one artifact object: {body:#?}",
    );

    // The GLiNER entity cards read the daemon's `sovereign.db`, which
    // this fixture does not create. Their ABSENCE is the library's
    // documented behaviour and must not be a failure — what would be a
    // failure is the route inventing a db path of its own.
    let (status, _) = get(&addr, "no-such-corpus/wrapped").await;
    assert_eq!(status, 404, "an uninstalled corpus has no deck to build");
}

/// A corpus the daemon does not have is a 404 WITH a reason on every
/// route — never an empty success, which would read to the pane as "this
/// corpus is empty" (ARCH §18.3).
#[tokio::test]
async fn uninstalled_corpus_is_a_404_naming_it_on_every_route() {
    let (daemon, _tmp) = build_meshapp_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    for path in [
        "graph",
        "findings",
        "entities?q=x",
        "claims",
        "questions",
        "reconciliation",
        "subgraph",
        "stats",
        "timeline",
        "documents",
    ] {
        let (status, body) = get(&addr, &format!("no-such-corpus/{path}")).await;
        assert_eq!(status, 404, "/{path} on a missing corpus must 404");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("no-such-corpus"),
            "/{path}: the body must NAME the corpus — the status alone is \
             not a gate, a routerless daemon 404s too: {body:#?}",
        );
    }
}
