// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two enrichment-store reads the desktop stops doing in-process
//! (thin-desktop order, 2026-09-11, `enrich_http`): the enriched-corpus
//! inventory and the starter questions mined from a corpus's atlas.
//!
//! Against a REAL daemon whose data root carries a real `enrichment/`
//! store and a real index with an `atlas/` — the fault these routes exist
//! to prevent is the desktop reading ITS data root while the corpus lives
//! on the daemon's.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::enrich_http::enrich_router;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;
const CORPUS: &str = "essays";

/// Five Question atoms: one too short, one too long, three in the length
/// window across two sections with three question types — enough for the
/// ranker's tier order and section diversification to show.
const ATOMS_JSON: &str = r#"{
  "schema_version": "2.3",
  "atoms": [
    {"atom_type":"Question","data":{
      "id":"question-0001","content":"Why?",
      "question_type":"thematic","raised_at":[{"chunk_id":"sec_00001"}],
      "resolution_status":{"kind":"open"},"enrichment_depth":"extracted"}},
    {"atom_type":"Question","data":{
      "id":"question-0002","content":"What does the author mean by faction, and how does it differ from a party",
      "question_type":"interpretive","raised_at":[{"chunk_id":"sec_00001"}],
      "resolution_status":{"kind":"open"},"enrichment_depth":"extracted"}},
    {"atom_type":"Question","data":{
      "id":"question-0003","content":"Is a large republic more stable than a small one?",
      "question_type":"thematic","raised_at":[{"chunk_id":"sec_00002"}],
      "resolution_status":{"kind":"open"},"enrichment_depth":"extracted"}},
    {"atom_type":"Question","data":{
      "id":"question-0004","content":"Which year was the tenth essay first printed in New York?",
      "question_type":"factual","raised_at":[{"chunk_id":"sec_00001"}],
      "resolution_status":{"kind":"open"},"enrichment_depth":"extracted"}},
    {"atom_type":"Entity","data":{
      "id":"entity-0001","canonical_name":"Publius","entity_type":"person",
      "first_appearance":{"chunk_id":"sec_00001"},
      "description":"the pen name","salience":0.9,"enrichment_depth":"extracted"}}
  ]
}"#;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly (`meshapp_surface_e2e`'s allow).
#[allow(clippy::unwrap_used)]
async fn build_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let path = indexes.join(CORPUS);
    let index = common::fixture_index(&indexes, CORPUS).await;
    index.mark_ingestion_complete().unwrap();
    let atlas_dir = path.join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    std::fs::write(atlas_dir.join("atoms.json"), ATOMS_JSON).unwrap();
    // A second installed corpus with NO atlas, for the 404 arm.
    let bare = common::fixture_index(&indexes, "bare").await;
    bare.mark_ingestion_complete().unwrap();

    // The enrichment store under the DAEMON's data root: one loadable
    // workspace and one directory with no config (mid-`enrich init`).
    let enrichment = tmp.path().join("enrichment");
    // Written as the JSON `svrn enrich init` writes — the required keys of
    // `sovereign_enrichment_catalog::EnrichConfig`, defaults for the rest —
    // so the route is exercised through the SAME loader the CLI uses.
    let cfg = serde_json::json!({
        "schema_version": sovereign_enrichment_catalog::CONFIG_SCHEMA_VERSION,
        "corpus_id": CORPUS,
        "pipeline_id": "literary_atlas",
        "source_path": "/tmp/essays.txt",
        "chapter_regex": "^## ",
        "chat_model": "qwen3-8b",
        "embed_model": "qwen3-embedding-0.6b",
        "created_at": "2026-09-11T00:00:00Z",
    });
    std::fs::create_dir_all(enrichment.join(CORPUS)).unwrap();
    std::fs::write(
        enrichment.join(CORPUS).join("config.json"),
        serde_json::to_vec_pretty(&cfg).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(enrichment.join("half-made")).unwrap();

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
        .get(format!("http://{addr}/internal/corpus/{path}"))
        .send()
        .await
        .expect("enrich_router reachable");
    let status = resp.status().as_u16();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// The inventory reads the DAEMON's `enrichment/` tree: the loadable
/// workspace is listed with the fields the corpus list shows, the
/// config-less directory is not.
#[tokio::test]
async fn enriched_lists_the_daemons_store_and_skips_the_config_less_dir() {
    let (daemon, _tmp) = build_daemon().await;
    let addr = spawn_router(enrich_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, "enriched").await;
    assert_eq!(status, 200, "{body:#?}");
    let rows = body.as_array().expect("an array");
    assert_eq!(
        rows.len(),
        1,
        "one loadable config, one bare dir: {body:#?}"
    );
    assert_eq!(rows[0]["corpus_id"], CORPUS);
    assert_eq!(rows[0]["pipeline_id"], "literary_atlas");
    assert_eq!(rows[0]["source_path"], "/tmp/essays.txt");
    assert!(rows[0]["created_at"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
}

/// Starter questions come out ranked (thematic before interpretive before
/// factual), one per section first, length-gated, normalised to `?`, and
/// capped by `limit`.
#[tokio::test]
async fn starter_questions_are_ranked_diversified_and_capped() {
    let (daemon, _tmp) = build_daemon().await;
    let addr = spawn_router(enrich_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/starter-questions")).await;
    assert_eq!(status, 200, "{body:#?}");
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 3, "\"Why?\" fails the length gate: {body:#?}");
    // Thematic first; the two thematic/interpretive picks cover BOTH
    // sections before the second sec_00001 question is admitted.
    assert_eq!(rows[0]["question_type"], "thematic");
    assert_eq!(rows[0]["source_section"], "sec_00002");
    assert_eq!(rows[1]["question_type"], "interpretive");
    assert_eq!(rows[1]["source_section"], "sec_00001");
    assert_eq!(rows[2]["question_type"], "factual");
    assert!(
        rows[1]["text"].as_str().unwrap().ends_with('?'),
        "trailing punctuation is normalised to a question mark: {:?}",
        rows[1]["text"]
    );

    let (status, body) = get(&addr, &format!("{CORPUS}/starter-questions?limit=1")).await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 1, "limit caps");
    let (status, _) = get(&addr, &format!("{CORPUS}/starter-questions?limit=100000")).await;
    assert_eq!(status, 200, "an over-large limit is clamped, not refused");
}

/// No atlas and not installed are 404s that NAME the corpus — the desktop
/// turns the first into its excerpt-starter branch, and neither may read
/// as "the host is broken" or as an empty success.
#[tokio::test]
async fn starter_questions_404_name_the_corpus_for_no_atlas_and_not_installed() {
    let (daemon, _tmp) = build_daemon().await;
    let addr = spawn_router(enrich_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, "bare/starter-questions").await;
    assert_eq!(status, 404, "{body:#?}");
    assert!(body["error"].as_str().unwrap().contains("bare"));
    assert!(body["error"].as_str().unwrap().contains("no atlas"));

    let (status, body) = get(&addr, "no-such-corpus/starter-questions").await;
    assert_eq!(status, 404, "{body:#?}");
    assert!(body["error"].as_str().unwrap().contains("no-such-corpus"));
}
