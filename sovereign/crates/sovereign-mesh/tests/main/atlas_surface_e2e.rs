// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two atlas-read surfaces the desktop stops carrying privately
//! (sv-surface D3 + D4).
//!
//! Both routers are exercised against a REAL daemon over a real
//! corpus with a real `atlas/atoms.json` on disk, because the fault
//! these routes exist to prevent is not a wiring fault — it is the
//! desktop and the daemon reading the same atlas through two
//! different code paths and disagreeing. A test with a stubbed
//! reader would pass on the day they diverged.
//!
//! # Red-watch (2026-09-10, run, not asserted)
//!
//! The routes were taken back OUT of `reading_router` and
//! `atlas_router` — handlers, DTOs and this file left intact, so the
//! suite still built — and the seven cases were run against the
//! routerless daemon. All seven failed, `pass: 0 fail: 7`:
//!
//! ```text
//! corpus_atoms_serves_the_whole_atlas          :187  left: 404  right: 200
//!                                              "GET /atoms must serve the atlas"
//! atlas_corpora_lists_the_installed_atlas      :326  left: 404  right: 200
//! atlas_atom_detail_404s_on_an_unknown_atom    :413  left: 404  right: 200
//!                                              "a real atom must resolve"
//! atlas_members_is_empty_for_an_ordinary_corpus:445  left: 404  right: 200
//!                                              "'not a collection' is an answer"
//! corpus_atoms_pages_without_silently_truncating :241  decode EOF (404, empty body)
//! atlas_atoms_browse_filters_by_type             :363  decode EOF (404, empty body)
//! corpus_atoms_reports_a_missing_atlas…          :302  decode EOF (404, empty body)
//! ```
//!
//! One of those deserves a caveat rather than a tick.
//! `corpus_atoms_reports_a_missing_atlas_rather_than_an_empty_page`
//! asserts a 404, and a routerless daemon answers 404 too — so its
//! STATUS assertion passed under sabotage and would have passed
//! against a route that never existed. What made it red is the line
//! after: the body must parse and must name `atlas`. That is the
//! assertion doing the work here, and the status line alone is not a
//! gate (ARCH §18.1).
//!
//! Routes restored, re-run green — the numbers are on the rung's
//! report.
//!
//! # The fixture
//!
//! A one-chunk corpus plus a four-atom atlas modelled on
//! `sovereign-desktop/tests/e2e/real/fixtures/governance-atlas`
//! (schema 2.3, Claim atoms) with one Entity added so the per-type
//! counts on `list_corpora` have more than one bucket to fill. It is
//! written inline rather than read from the desktop crate's fixture
//! tree: a test in this crate reaching across into another crate's
//! fixtures is a coupling that breaks the first time either moves.

use std::sync::Arc;

use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::atlas_http::atlas_router;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::reading_http::reading_router;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;
const CORPUS: &str = "governance";

/// Four atoms: three Claims (the governance fixture's shape) and one
/// Entity, so `list_corpora`'s per-type counts are not a single
/// bucket and `list_atoms`'s type filter has something to exclude.
const ATOMS_JSON: &str = r#"{
  "schema_version": "2.3",
  "atoms": [
    {
      "atom_type": "Entity",
      "data": {
        "id": "entity-0001",
        "canonical_name": "The House Charter",
        "entity_type": "document",
        "first_appearance": { "chunk_id": "sec_charter_ii" },
        "description": "The founding agreement the quiet-hours claims cite.",
        "salience": 1.0,
        "enrichment_depth": "extracted"
      }
    },
    {
      "atom_type": "Claim",
      "data": {
        "id": "claim-179b296698ec4911",
        "content": "Quiet hours begin at 11 PM every night.",
        "discourse_act": "enact",
        "epistemic_status": "confident",
        "scope": "contextual",
        "evidence": [ { "chunk_id": "sec_charter_ii" } ],
        "attributed_to": "entity-0001",
        "claim_kind": "requires",
        "enrichment_depth": "extracted"
      }
    },
    {
      "atom_type": "Claim",
      "data": {
        "id": "claim-7d2475b7cdf5223c",
        "content": "Quiet hours begin at 10 PM on weeknights.",
        "discourse_act": "enact",
        "epistemic_status": "confident",
        "scope": "contextual",
        "evidence": [ { "chunk_id": "sec_2026_02_10" } ],
        "attributed_to": "entity-0001",
        "claim_kind": "requires",
        "enrichment_depth": "extracted"
      }
    },
    {
      "atom_type": "Claim",
      "data": {
        "id": "claim-5ba6f4db902ff89f",
        "content": "Guests may stay up to two nights.",
        "discourse_act": "enact",
        "epistemic_status": "confident",
        "scope": "contextual",
        "evidence": [ { "chunk_id": "sec_charter_ii" } ],
        "attributed_to": "entity-0001",
        "claim_kind": "requires",
        "enrichment_depth": "extracted"
      }
    }
  ]
}"#;

const ATOM_COUNT: usize = 4;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// A daemon whose `CorpusEngine` has one installed corpus carrying one
/// chunk and the four-atom atlas above.
/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly. `clippy.toml`'s
/// `allow-unwrap-in-tests` covers `#[test]` bodies only, so a helper
/// like this one still counts against the crate's panic ratchet
/// (`quality/baselines/clippy_counts.tsv`) — hence the scoped allow
/// rather than eight `?`s that would only turn a broken fixture into a
/// quieter failure.
#[allow(clippy::unwrap_used)]
async fn build_atlas_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir) {
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
        .insert_batch(&[(
            InsertChunk {
                content: "Quiet hours begin at 11 PM every night.".into(),
                title: Some("Governance".into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(CORPUS.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; EMBED_DIM],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();

    // The atlas the routes read. Written where `installed_indexes()`
    // + `atlas_dir_for_corpus` look for it: `<index>/atlas/atoms.json`.
    let atlas_dir = path.join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    std::fs::write(atlas_dir.join("atoms.json"), ATOMS_JSON).unwrap();

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

// ── D3: the one MeshApp primitive, served ─────────────────────────

/// The whole atlas comes back in one page, in `AtomEnvelope` shape.
///
/// Asserts the ATOM CONTENT, not just the count: the three meshapp
/// folds this route retires (`read_corpus`, `search_parcels`,
/// `parcel_analytics`) read type-specific fields off these envelopes,
/// so a route that returned four well-formed but EMPTY objects would
/// satisfy a count check and break all three.
#[tokio::test]
async fn corpus_atoms_serves_the_whole_atlas() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/corpus/{CORPUS}/atoms"))
        .send()
        .await
        .expect("reading_router reachable");
    assert_eq!(resp.status(), 200, "GET /atoms must serve the atlas");

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["corpus_id"], CORPUS);
    assert_eq!(body["schema_version"], "2.3");
    assert_eq!(body["total"], ATOM_COUNT);
    assert_eq!(body["offset"], 0);
    assert!(
        body["next_offset"].is_null(),
        "a complete read must say so with an explicit null, not an \
         absent key: {body:#?}",
    );

    let atoms = body["atoms"].as_array().expect("atoms array");
    assert_eq!(atoms.len(), ATOM_COUNT);
    // The envelope tag + the type-specific body both survive the wire.
    assert_eq!(atoms[0]["atom_type"], "Entity");
    assert_eq!(atoms[0]["data"]["canonical_name"], "The House Charter");
    assert_eq!(atoms[1]["atom_type"], "Claim");
    assert_eq!(
        atoms[1]["data"]["content"], "Quiet hours begin at 11 PM every night.",
        "the claim body must cross verbatim — the meshapp folds read it",
    );

    // And it round-trips into the type the desktop actually holds, which
    // is what makes the repoint a call-site change: this is the same
    // `Vec<AtomEnvelope>` `load_atoms` returns today.
    let envelopes: Vec<corpus_engine::enrichment::atlas::AtomEnvelope> =
        serde_json::from_value(body["atoms"].clone())
            .expect("the page deserialises as AtomEnvelope — the desktop's own type");
    assert_eq!(envelopes.len(), ATOM_COUNT);
}

/// A clipped page says so, and says where to resume — it never returns
/// a short vec that reads like the end of the atlas.
///
/// Red before the route: 404 on the first request. The guarded
/// property is §18.3's — the absence of the remaining atoms is
/// REPORTED (`total` = 4, `next_offset` = 2), never defaulted away.
#[tokio::test]
async fn corpus_atoms_pages_without_silently_truncating() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let first: serde_json::Value = http
        .get(format!(
            "http://{addr}/internal/corpus/{CORPUS}/atoms?offset=0&limit=2"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["atoms"].as_array().unwrap().len(), 2);
    assert_eq!(
        first["total"], ATOM_COUNT,
        "total is the ATLAS, not the page"
    );
    assert_eq!(first["limit"], 2);
    assert_eq!(
        first["next_offset"], 2,
        "a clipped read must name where to resume: {first:#?}",
    );

    let second: serde_json::Value = http
        .get(format!(
            "http://{addr}/internal/corpus/{CORPUS}/atoms?offset=2&limit=2"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["atoms"].as_array().unwrap().len(), 2);
    assert!(
        second["next_offset"].is_null(),
        "the last page must end the read: {second:#?}",
    );

    // The two pages together are the whole atlas, in order, no repeats.
    let ids: Vec<&str> = first["atoms"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["atoms"].as_array().unwrap())
        .map(|a| a["data"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), ATOM_COUNT);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        ATOM_COUNT,
        "paging must not repeat an atom: {ids:?}",
    );
}

/// An atlas the daemon does not have is an ERROR with a reason, not an
/// empty page. The desktop's `load_atoms` propagates this same failure;
/// a route that answered `{"atoms": []}` would turn "corpus missing"
/// into "corpus has no atoms" (§18.3).
#[tokio::test]
async fn corpus_atoms_reports_a_missing_atlas_rather_than_an_empty_page() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(reading_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "http://{addr}/internal/corpus/no-such-corpus/atoms"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("atlas"),
        "the refusal must name what was missing: {body:#?}",
    );
}

// ── D4: the atlas-browse surface, served ──────────────────────────

/// `GET /internal/atlas/corpora` finds the installed atlas and counts
/// its atoms by type.
///
/// Red before `atlas_router` existed: pointed at `reading_router`, the
/// request answered 404 and `assert_eq!(resp.status(), 200)` failed.
#[tokio::test]
async fn atlas_corpora_lists_the_installed_atlas() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/atlas/corpora"))
        .send()
        .await
        .expect("atlas_router reachable");
    assert_eq!(resp.status(), 200);

    let rows: Vec<sovereign_tools::atlas_view::AtlasCorpusSummary> = resp.json().await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.corpus_id == CORPUS)
        .unwrap_or_else(|| panic!("the installed atlas must be listed: {rows:#?}"));
    assert_eq!(
        row.total_atoms as usize, ATOM_COUNT,
        "the count must be the atlas's, read through the daemon's own engine",
    );

    // Deserialising into `sovereign_tools::atlas_view::AtlasCorpusSummary`
    // IS the parity assertion: the desktop command returns this exact
    // type, so a body that would not parse here would not repoint there.
}

/// The atom browse serves the same page `atlas_list_atoms` returns, and
/// the type filter still filters.
///
/// Red before the router: 404. The filter half matters because the
/// route accepts the filter as a POST body — a handler that dropped the
/// body would answer all four atoms and still look healthy.
#[tokio::test]
async fn atlas_atoms_browse_filters_by_type() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let all: sovereign_tools::atlas_view::AtomListPage = http
        .post(format!("http://{addr}/internal/atlas/{CORPUS}/atoms"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("atlas_router reachable")
        .json()
        .await
        .unwrap();
    assert_eq!(
        all.items.len(),
        ATOM_COUNT,
        "an empty filter matches everything"
    );

    let claims: sovereign_tools::atlas_view::AtomListPage = http
        .post(format!("http://{addr}/internal/atlas/{CORPUS}/atoms"))
        .json(&serde_json::json!({ "filter": { "atom_type": "Claim" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        claims.items.len(),
        3,
        "the POST body's filter must reach the reader: {claims:#?}",
    );
    assert!(
        claims
            .items
            .iter()
            .all(|a| a.atom_type == corpus_engine::enrichment::atlas::AtomType::Claim),
        "every returned atom must satisfy the filter",
    );
}

/// One atom's full inspector record crosses the wire; an id that is not
/// in the atlas is a 404, not a 500 and not an empty record.
///
/// Red before the router: both requests answered 404, so the FIRST
/// assertion (`status == 200` on the known atom) failed. That ordering
/// matters — a test whose only assertion was the 404 case would have
/// passed against a router that did not exist.
#[tokio::test]
async fn atlas_atom_detail_404s_on_an_unknown_atom() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let found = http
        .get(format!(
            "http://{addr}/internal/atlas/{CORPUS}/atoms/claim-179b296698ec4911"
        ))
        .send()
        .await
        .expect("atlas_router reachable");
    assert_eq!(found.status(), 200, "a real atom must resolve");
    let detail: sovereign_tools::atlas_view::AtomDetail = found.json().await.unwrap();
    assert_eq!(detail.corpus_id, CORPUS);
    assert_eq!(detail.atom_id.as_str(), "claim-179b296698ec4911");

    let missing = http
        .get(format!(
            "http://{addr}/internal/atlas/{CORPUS}/atoms/claim-does-not-exist"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing.status(),
        404,
        "a stale UI link is 'absent', which is a different fact from 'broken'",
    );
}

/// `members` answers an EMPTY list for an ordinary corpus — the
/// frontend branches on that to pick which Explore surface to render,
/// so it must never become a 404.
#[tokio::test]
async fn atlas_members_is_empty_for_an_ordinary_corpus() {
    let (daemon, _tmp) = build_atlas_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/atlas/{CORPUS}/members"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "'not a collection' is an answer, not an error"
    );
    let rows: Vec<sovereign_tools::atlas_view::AtlasMemberSummary> = resp.json().await.unwrap();
    assert!(rows.is_empty(), "{rows:#?}");
}
