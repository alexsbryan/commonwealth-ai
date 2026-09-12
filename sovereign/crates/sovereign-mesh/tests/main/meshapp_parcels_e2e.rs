// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three SF-LVT parcel reads the desktop stops folding in-process
//! (thin-desktop order, 2026-09-11): `GET /internal/meshapp/{corpus}/
//! parcels`, `…/parcels/search` and `…/parcel-analytics`.
//!
//! Against a REAL daemon over a real index with a real `atlas/` on disk,
//! for `meshapp_surface_e2e`'s reason: the fault these routes exist to
//! prevent is the desktop and the daemon reading the same files through
//! two code paths and disagreeing. Its own fixture corpus rather than the
//! governance one, because that fixture's graph tests pin "two Entity
//! atoms, two nodes" and parcels are Entity atoms.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::meshapp_http::meshapp_router;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;
const CORPUS: &str = "sf-parcels";

/// Three parcel atoms and one non-parcel entity. Land/improvement values
/// chosen so the folds have something to distinguish: 0001001 is
/// land-rich (share 0.8, HighLandShare), 0002002 is improvement-heavy
/// (share 0.2), 0003003 is near-vacant (improvement/land = 0.05,
/// Underused AND HighLandShare).
const ATOMS_JSON: &str = r#"{
  "schema_version": "2.3",
  "atoms": [
    {"atom_type":"Entity","data":{
      "id":"entity-p1","canonical_name":"0001001","entity_type":"parcel",
      "first_appearance":{"chunk_id":"sec_00001"},
      "description":"parcel","salience":0.5,"enrichment_depth":"extracted",
      "attributes":{"assessed_land_value":800000.0,"assessed_improvement_value":200000.0,
                    "property_location":"100 MAIN ST"}}},
    {"atom_type":"Entity","data":{
      "id":"entity-p2","canonical_name":"0002002","entity_type":"parcel",
      "first_appearance":{"chunk_id":"sec_00001"},
      "description":"parcel","salience":0.5,"enrichment_depth":"extracted",
      "attributes":{"assessed_land_value":200000.0,"assessed_improvement_value":800000.0,
                    "property_location":"200 MARKET ST"}}},
    {"atom_type":"Entity","data":{
      "id":"entity-p3","canonical_name":"0003003","entity_type":"parcel",
      "first_appearance":{"chunk_id":"sec_00001"},
      "description":"parcel","salience":0.5,"enrichment_depth":"extracted",
      "attributes":{"assessed_land_value":1000000.0,"assessed_improvement_value":50000.0,
                    "property_location":"300 MAIN ST"}}},
    {"atom_type":"Entity","data":{
      "id":"entity-city","canonical_name":"San Francisco","entity_type":"place",
      "first_appearance":{"chunk_id":"sec_00001"},
      "description":"the city","salience":0.9,"enrichment_depth":"extracted"}}
  ]
}"#;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly (`meshapp_surface_e2e`'s allow).
#[allow(clippy::unwrap_used)]
async fn build_parcel_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let path = indexes.join(CORPUS);
    let index = common::fixture_index(&indexes, CORPUS).await;
    index.mark_ingestion_complete().unwrap();
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

/// `parcels?ids=` answers by atom id OR parcel number, carrying the
/// attributes the per-parcel calculator chips back to; a non-parcel
/// entity asked for by name is NOT a parcel and is not invented into one.
#[tokio::test]
async fn parcels_resolve_by_atom_id_or_parcel_number() {
    let (daemon, _tmp) = build_parcel_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/parcels?ids=entity-p1,0002002")).await;
    assert_eq!(status, 200, "{body:#?}");
    let rows = body.as_array().expect("an array of parcels");
    let numbers: Vec<&str> = rows
        .iter()
        .map(|r| r["parcel_number"].as_str().unwrap())
        .collect();
    assert_eq!(
        numbers,
        vec!["0001001", "0002002"],
        "atom id and parcel number both resolve"
    );
    assert_eq!(
        rows[0]["attributes"]["assessed_land_value"], 800000.0,
        "the attributes cross verbatim — the calculator reads them",
    );
    assert_eq!(rows[0]["atom_id"], "entity-p1");

    // An empty ask is an empty answer, not an error; every parcel is
    // answered only for the ids asked.
    let (status, body) = get(&addr, &format!("{CORPUS}/parcels?ids=")).await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

/// Search matches the parcel number exactly (case-folded) or the
/// address as a substring; a blank query is `[]`; `limit` caps and is
/// clamped rather than refused.
#[tokio::test]
async fn parcel_search_matches_number_or_address_and_clamps() {
    let (daemon, _tmp) = build_parcel_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/parcels/search?q=main%20st")).await;
    assert_eq!(status, 200, "{body:#?}");
    let hits = body.as_array().unwrap();
    assert_eq!(hits.len(), 2, "MAIN ST is on two parcels: {body:#?}");

    let (_, body) = get(&addr, &format!("{CORPUS}/parcels/search?q=0002002")).await;
    assert_eq!(body.as_array().unwrap().len(), 1, "exact number match");
    assert_eq!(body[0]["parcel_number"], "0002002");

    let (status, body) = get(&addr, &format!("{CORPUS}/parcels/search?q=%20%20")).await;
    assert_eq!(status, 200, "a blank query is a successful empty answer");
    assert_eq!(body.as_array().unwrap().len(), 0);

    let (status, body) = get(&addr, &format!("{CORPUS}/parcels/search?q=main&limit=1")).await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 1, "limit caps the page");
    let (status, _) = get(
        &addr,
        &format!("{CORPUS}/parcels/search?q=main&limit=100000"),
    )
    .await;
    assert_eq!(status, 200, "an over-large limit is clamped, not refused");
}

/// The revenue-neutral aggregate is corpus-engine's fold, and the
/// derivation names the numbers it was folded from — asserted against
/// the fixture's own sums, not against the route's echo.
#[tokio::test]
async fn parcel_analytics_folds_the_fixture_and_names_its_derivation() {
    let (daemon, _tmp) = build_parcel_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    let (status, body) = get(
        &addr,
        &format!("{CORPUS}/parcel-analytics?business_tax_target=100000"),
    )
    .await;
    assert_eq!(status, 200, "{body:#?}");
    assert_eq!(body["corpus_id"], CORPUS);
    assert_eq!(
        body["parcel_count"], 3,
        "the `place` entity is not a parcel"
    );
    assert_eq!(body["land_value_total"], 2_000_000.0);
    assert_eq!(body["improvement_value_total"], 1_050_000.0);
    assert_eq!(body["business_tax_target"], 100_000.0);
    assert_eq!(body["neutral_rate"], 0.05, "100k / 2M");
    assert_eq!(
        body["high_land_share_count"], 2,
        "0001001 (0.8) and 0003003 (0.95) are land-rich; 0002002 (0.2) is not",
    );
    assert_eq!(body["underused_count"], 1, "only 0003003 is near-vacant");
    let derivation = body["derivation"].as_array().unwrap();
    assert_eq!(derivation.len(), 4);
    assert!(
        derivation[0]
            .as_str()
            .unwrap()
            .contains("over 3 parcel atoms (sf-parcels) = $2,000,000.00"),
        "the trace names the fold's inputs: {derivation:#?}",
    );

    // Absent target: the SF default applies (the route's, not the pane's).
    let (status, body) = get(&addr, &format!("{CORPUS}/parcel-analytics")).await;
    assert_eq!(status, 200);
    assert_eq!(body["business_tax_target"], 1_400_000_000.0);
}

/// A corpus with no atlas, and one with an atlas but no `parcel` atoms,
/// are both 404s that NAME the corpus — never an empty aggregate.
#[tokio::test]
async fn parcel_routes_404_naming_the_corpus_when_there_is_nothing_to_fold() {
    let (daemon, _tmp) = build_parcel_daemon().await;
    let addr = spawn_router(meshapp_router(Arc::clone(&daemon))).await;

    for path in ["parcels?ids=x", "parcels/search?q=x", "parcel-analytics"] {
        let (status, body) = get(&addr, &format!("no-such-corpus/{path}")).await;
        assert_eq!(
            status, 404,
            "/{path} on a missing corpus must 404: {body:#?}"
        );
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("no-such-corpus"),
            "/{path}: the body must NAME the corpus: {body:#?}",
        );
    }
}
