// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atlas_http`'s six conversation-tiered routes, end to end
//! (sv-surface D4 remainder).
//!
//! The wire form of the desktop's six `atlas_*` conv commands. Every
//! case here drives a REAL `SqliteStateStore` — the same concrete
//! store `state.rs:1549` upcasts into `lane_sources.conv_tiered` in
//! production — so what is proven is the path the handler takes, not
//! only the shape it returns. A stub reader would satisfy every
//! assertion below and prove nothing about the wiring.
//!
//! # Red-watch (2026-09-10, run, not asserted)
//!
//! The six routes were taken back OUT of `atlas_router` — handlers,
//! helpers and this file left in place, so the suite still built —
//! and every case re-run against the routerless daemon. ALL SIX
//! failed, `pass: 0 fail: 6`:
//!
//! ```text
//! conv_corpora_names_each_corpus_…             :274  left: 404  right: 200
//! conversations_page_serves_titles_…           :304  left: 404  right: 200
//! conv_detail_serves_the_raptor_tree_…         :361  left: 404  right: 200
//! conv_entities_rank_by_salience               :399  left: 404  right: 200
//! entity_aggregate_and_chunk_progress_…        :423  left: 404  right: 200
//! absence_has_three_answers_…                  :466  "the 404 must NAME
//!                                                     the conversation … Null"
//! ```
//!
//! The last one is the case worth reading twice, and the run says
//! something the file did not predict. Its STATUS line (an unknown
//! conversation is a 404) passed under sabotage — a daemon with no
//! route 404s too — so it never reached the 503 loop that was written
//! as its gate. What went red one line later is the BODY: an empty 404
//! cannot name `nope-1234`. Both halves are recorded because a reader
//! who only sees "FAILED" would credit the wrong assertion (ARCH
//! §18.1).

use std::sync::Arc;

use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::conv_tiered::{
    ChunkEntityProgressRow, ChunkEntityRow, ConvRaptorNodeRow, ConvSkeletonRow, ConvTieredReader,
};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::atlas_http::atlas_router;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_store::sqlite::SqliteStateStore;

use crate::common;
use crate::common::spawn_router;

const CORPUS: &str = "governance";
const EMBED_DIM: usize = 8;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

fn node(conv: &str, id: &str, level: i64, entities: &str, coherence: f64) -> ConvRaptorNodeRow {
    ConvRaptorNodeRow {
        node_id: id.into(),
        corpus_id: CORPUS.into(),
        conv_uuid: conv.into(),
        level,
        summary: format!("summary of {id}"),
        summary_embedding: vec![0.0; EMBED_DIM],
        centroid_embedding: vec![0.0; EMBED_DIM],
        children_node_ids_json: "[]".into(),
        direct_member_chunk_ids_json: Some("[100,200]".into()),
        evidence_chunk_ids_json: "[100,200,201]".into(),
        quote_spans_json: "[]".into(),
        primary_entities_json: entities.into(),
        cluster_coherence: coherence,
        created_at: 10,
        prompt_version: "v1".into(),
        summarizer_model: "test".into(),
    }
}

/// A serving daemon whose `Runtime` reads the SAME `SqliteStateStore`
/// this function seeded, and whose `CorpusEngine` has the corpus
/// installed under a display name — so the corpora route has both
/// halves to compose.
///
/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly (`meshapp_surface_e2e`'s scoped
/// allow, same reason).
#[allow(clippy::unwrap_used)]
async fn build_conv_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir) {
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
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();

    let store = Arc::new(SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap());

    // conv-1: two RAPTOR levels, a correction, real entities.
    store
        .save_conv_skeleton(&ConvSkeletonRow {
            corpus_id: CORPUS.into(),
            conv_uuid: "conv-1".into(),
            state: "ready".into(),
            skeleton_json: None,
            overview: Some("Quiet hours policy thread".into()),
            segments_json: None,
            chunk_count: 3,
            updated_at: 2_000,
        })
        .await
        .unwrap();
    store
        .save_conv_raptor_nodes(
            CORPUS,
            "conv-1",
            &[
                node("conv-1", "n-root", 1, r#"["Ken Lay","El Paso"]"#, 0.8),
                node("conv-1", "n-leaf", 0, r#"["Ken Lay"]"#, 0.5),
            ],
        )
        .await
        .unwrap();
    store
        .upsert_summary_correction(
            CORPUS,
            "conv-1",
            Some("say who approved it"),
            Some("the old summary"),
            "pending",
            42,
        )
        .await
        .unwrap();

    // conv-2: the "Tiny" shape — one synthetic node, no entities, no
    // overview, so the title falls back.
    store
        .save_conv_skeleton(&ConvSkeletonRow {
            corpus_id: CORPUS.into(),
            conv_uuid: "conv-2".into(),
            state: "pending".into(),
            skeleton_json: None,
            overview: None,
            segments_json: None,
            chunk_count: 1,
            updated_at: 1_000,
        })
        .await
        .unwrap();
    store
        .save_conv_raptor_nodes(CORPUS, "conv-2", &[node("conv-2", "n-tiny", 0, "[]", 1.0)])
        .await
        .unwrap();

    store
        .save_chunk_entities_for_conv(
            CORPUS,
            "conv-1",
            &[
                ChunkEntityRow {
                    corpus_id: CORPUS.into(),
                    chunk_id: 100,
                    text: "Ken Lay".into(),
                    label: "Person".into(),
                    char_start: 0,
                    char_end: 7,
                    score: 0.9,
                    conv_uuid: Some("conv-1".into()),
                    extracted_at: 11,
                },
                ChunkEntityRow {
                    corpus_id: CORPUS.into(),
                    chunk_id: 100,
                    text: "El Paso".into(),
                    label: "Organization".into(),
                    char_start: 9,
                    char_end: 16,
                    score: 0.8,
                    conv_uuid: Some("conv-1".into()),
                    extracted_at: 11,
                },
            ],
        )
        .await
        .unwrap();
    store
        .upsert_chunk_entity_progress(&ChunkEntityProgressRow {
            corpus_id: CORPUS.into(),
            chunks_processed: 7,
            chunks_total: 9,
            mentions_extracted: 2,
            last_chunk_id: Some(100),
            started_at: 1,
            updated_at: 2,
            finished_at: None,
            state: "running".into(),
            model_id: Some("gliner-small".into()),
            threshold: Some(0.5),
            labels_json: Some(r#"["Person"]"#.into()),
            error_msg: None,
        })
        .await
        .unwrap();

    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes, indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let conv: Arc<dyn ConvTieredReader> = Arc::clone(&store) as Arc<dyn ConvTieredReader>;
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_conv_reader(
            engine,
            store as Arc<dyn sovereign_core::traits::StateStore>,
            conv,
        ),
    );
    (daemon, tmp)
}

/// `GET` one conv path and hand back status + parsed body.
async fn get(addr: &std::net::SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/internal/atlas/conv/{path}"))
        .send()
        .await
        .expect("atlas_router reachable");
    let status = resp.status().as_u16();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// The corpora row composes two sources: the store's state buckets and
/// the engine's display name. Asserts BOTH — a route that answered the
/// buckets with `display_name == corpus_id` would satisfy a count.
#[tokio::test]
async fn conv_corpora_names_each_corpus_with_its_state_buckets_and_display_name() {
    let (daemon, _tmp) = build_conv_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, "corpora").await;
    assert_eq!(status, 200, "corpora: {body:#?}");
    let rows = body.as_array().expect("an array of corpora");
    assert_eq!(rows.len(), 1, "one seeded corpus: {body:#?}");
    let row = &rows[0];
    assert_eq!(row["corpus_id"], CORPUS);
    assert_eq!(
        row["display_name"], "Governance",
        "the display name is the ENGINE's, not the id: {row:#?}"
    );
    assert_eq!(row["conv_count"], 2);
    assert_eq!(
        row["state_counts"]["ready"], 1,
        "the state buckets are the STORE's: {row:#?}"
    );
    assert_eq!(row["state_counts"]["pending"], 1);
    assert_eq!(
        row["last_updated_unix"], 2_000,
        "the newest conversation's stamp: {row:#?}"
    );
}

/// The list row carries the title fallback, the salience-ranked chip
/// budget, the Tiny flag and the paging terminator — every field the
/// pane branches on.
#[tokio::test]
async fn conversations_page_serves_titles_entities_and_the_tiny_flag() {
    let (daemon, _tmp) = build_conv_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/conversations")).await;
    assert_eq!(status, 200, "conversations: {body:#?}");
    assert_eq!(body["total_matching"], 2);
    assert!(
        body["next_offset"].is_null(),
        "both rows fit one page, so next_offset is an EXPLICIT null, not an \
         absent key — an absent key is indistinguishable from an old host: {body:#?}"
    );
    let rows = body["conversations"].as_array().expect("an array");
    assert_eq!(rows.len(), 2);
    // updated_at DESC: conv-1 (2000) before conv-2 (1000).
    assert_eq!(rows[0]["conv_uuid"], "conv-1");
    assert_eq!(rows[0]["title"], "Quiet hours policy thread");
    assert_eq!(rows[0]["state"], "ready");
    assert_eq!(rows[0]["chunk_count"], 3);
    assert_eq!(
        rows[0]["top_entities"][0], "Ken Lay",
        "salience 1.3 over two nodes outranks El Paso's 0.8: {:#?}",
        rows[0]
    );
    assert_eq!(rows[0]["top_entities"][1], "El Paso");
    assert_eq!(rows[0]["is_tiny"], false);

    assert_eq!(rows[1]["conv_uuid"], "conv-2");
    assert_eq!(
        rows[1]["title"], "(untitled conversation)",
        "a NULL overview renders as the fallback, not as an empty string: {:#?}",
        rows[1]
    );
    assert_eq!(
        rows[1]["is_tiny"], true,
        "one synthetic node, perfect coherence, no entities: {:#?}",
        rows[1]
    );

    // The filter is a substring on `overview`, trimmed; a blank one is
    // no filter at all.
    let (_, filtered) = get(&addr, &format!("{CORPUS}/conversations?filter=quiet")).await;
    assert_eq!(
        filtered["total_matching"], 1,
        "the overview filter is case-insensitive LIKE: {filtered:#?}"
    );
    let (_, blank) = get(&addr, &format!("{CORPUS}/conversations?filter=%20%20")).await;
    assert_eq!(
        blank["total_matching"], 2,
        "a whitespace-only filter is None, not a search for spaces: {blank:#?}"
    );
}

/// The detail view is the RAPTOR tree plus the active correction. The
/// correction is the field the desktop swallowed on error; here it is
/// a real value from the store.
#[tokio::test]
async fn conv_detail_serves_the_raptor_tree_and_the_active_correction() {
    let (daemon, _tmp) = build_conv_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/conversations/conv-1")).await;
    assert_eq!(status, 200, "detail: {body:#?}");
    assert_eq!(body["conv_uuid"], "conv-1");
    assert_eq!(body["title"], "Quiet hours policy thread");
    assert_eq!(body["max_level"], 1, "the tree's tallest level: {body:#?}");
    let nodes = body["raptor_nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 2);
    // level DESC, so the root comes first.
    assert_eq!(nodes[0]["node_id"], "n-root");
    assert_eq!(nodes[0]["summary"], "summary of n-root");
    assert_eq!(nodes[0]["primary_entities"][0], "Ken Lay");
    assert_eq!(
        nodes[0]["direct_member_chunk_ids"][1], 200,
        "the JSON column is PARSED into a list, not echoed as a string: {:#?}",
        nodes[0]
    );
    assert_eq!(
        nodes[0]["evidence_chunk_count"], 3,
        "the count, not the ids: {:#?}",
        nodes[0]
    );
    assert_eq!(nodes[0]["is_synthetic_tiny"], false);
    assert_eq!(
        body["correction"]["correction_hint"], "say who approved it",
        "the active correction rides the detail — a missing badge would \
         read as 'never revised': {body:#?}"
    );
    assert_eq!(body["correction"]["status"], "pending");
    assert_eq!(body["correction"]["created_at"], 42);
}

/// The chip row is ranked by salience with a name tiebreak — the same
/// ranking the list row's `top_entities` uses, from one implementation.
#[tokio::test]
async fn conv_entities_rank_by_salience() {
    let (daemon, _tmp) = build_conv_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/conversations/conv-1/entities")).await;
    assert_eq!(status, 200, "entities: {body:#?}");
    let chips = body.as_array().expect("chips");
    assert_eq!(chips.len(), 2);
    assert_eq!(chips[0]["name"], "Ken Lay");
    assert_eq!(
        chips[0]["occurrence_count"], 2,
        "Ken Lay is named by both nodes: {body:#?}"
    );
    assert!(
        (chips[0]["salience"].as_f64().unwrap_or(0.0) - 1.3).abs() < 1e-4,
        "salience is the SUM of cluster_coherence (0.8 + 0.5): {body:#?}"
    );
    assert_eq!(chips[1]["name"], "El Paso");
    assert_eq!(chips[1]["occurrence_count"], 1);
}

/// The entity drawer and the extraction progress bar, both straight
/// from the store's own aggregate.
#[tokio::test]
async fn entity_aggregate_and_chunk_progress_serve_the_stores_rows() {
    let (daemon, _tmp) = build_conv_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let (status, body) = get(
        &addr,
        &format!("{CORPUS}/entities/aggregate?text=ken%20lay"),
    )
    .await;
    assert_eq!(status, 200, "aggregate: {body:#?}");
    assert_eq!(
        body["text"], "Ken Lay",
        "the seed is matched case-insensitively and answered in its \
         canonical form: {body:#?}"
    );
    assert_eq!(body["mention_count"], 1);
    assert_eq!(body["labels"][0]["label"], "Person");
    assert_eq!(
        body["co_occurring"][0]["text"], "El Paso",
        "the co-occurrence drawer reads the same chunk: {body:#?}"
    );
    assert_eq!(body["top_convs"][0]["conv_uuid"], "conv-1");

    let (status, body) = get(&addr, &format!("{CORPUS}/chunk-entity-progress")).await;
    assert_eq!(status, 200, "progress: {body:#?}");
    assert_eq!(body["chunks_processed"], 7);
    assert_eq!(body["chunks_total"], 9);
    assert_eq!(body["state"], "running");
    assert_eq!(body["model_id"], "gliner-small");

    // A corpus that never ran extraction answers an explicit null BODY
    // on a 200 — a fact about the corpus, not an absent resource.
    let (status, body) = get(&addr, "no-such-corpus/chunk-entity-progress").await;
    assert_eq!(status, 200, "never-extracted: {body:#?}");
    assert!(
        body.is_null(),
        "never extracted is an explicit null, never an invented zero row: {body:#?}"
    );
}

/// Three absences, three answers, none of them an empty page (§18.3).
///
/// The 404 half is NOT a gate on its own — a routerless daemon 404s too
/// — and it is here for its BODY, which must name the id. The 503 half
/// is the gate: no absent route can produce it.
#[tokio::test]
async fn absence_has_three_answers_and_a_readerless_daemon_says_so() {
    let (daemon, _tmp) = build_conv_daemon().await;
    let addr = spawn_router(atlas_router(Arc::clone(&daemon))).await;

    let (status, body) = get(&addr, &format!("{CORPUS}/conversations/nope-1234")).await;
    assert_eq!(status, 404);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("nope-1234"),
        "the 404 must NAME the conversation — the status alone is not a \
         gate, a routerless daemon 404s too: {body:#?}"
    );

    // A daemon commissioned WITHOUT a conv reader (the mesh-admin and
    // headless shapes) answers 503 naming the wiring, on every route.
    let tmp = tempfile::tempdir().expect("tempdir");
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).expect("indexes dir");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    let engine = Arc::new(CorpusEngine::new(recipes, indexes, mock_embed_fn()));
    let bare = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_engine(engine),
    );
    let bare_addr = spawn_router(atlas_router(Arc::clone(&bare))).await;

    for path in [
        "corpora".to_string(),
        format!("{CORPUS}/conversations"),
        format!("{CORPUS}/conversations/conv-1"),
        format!("{CORPUS}/conversations/conv-1/entities"),
        format!("{CORPUS}/entities/aggregate?text=x"),
        format!("{CORPUS}/chunk-entity-progress"),
    ] {
        let (status, body) = get(&bare_addr, &path).await;
        assert_eq!(
            status, 503,
            "/{path} on a readerless daemon must be 503, not an empty page: {body:#?}"
        );
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("conv_tiered"),
            "/{path}: the 503 must name the wiring that is missing: {body:#?}"
        );
    }
}
