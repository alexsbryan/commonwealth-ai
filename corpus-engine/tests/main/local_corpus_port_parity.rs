// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of sovereign-tools' local-corpus e2es (phase-b-47).
//!
//! svrn's watched_folder_e2e, knowledge_view_e2e, obsidian_live_sync_e2e
//! and local_corpus_e2e drive `IngestPortDouble`; what ingest itself does
//! with the recipes and updates svrn hands it is proven here, through
//! `impl LocalCorpusPort for CorpusEngine`, over the same fixture folders
//! as svrn stages them (one JSONL row per file: root-relative `id`, `title`,
//! `content`) and the same recipe shapes (`recipe_toml`'s and a knowledge
//! view's). The composed path is pb-ingest-dial-daemon's watched-folder
//! PROOF.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::index::field_skeleton::write_field_skeleton;
use corpus_engine::CorpusEngine;
use corpus_engine_atlas_reader::field_model::{load_field_skeleton, LEGACY_ARTIFACT};
use corpus_index::ingest_port::{
    CustomAcquirerFn, DocFetchFn, IngestPluginPort, LocalCorpusPort, WatchedUpdate,
};
use corpus_index::source::CorpusReadPort;
use corpus_index::types::EmbedFn;
use understanding_vocab::skeleton::{CanonicalQuestion, FieldModelStats, FieldSkeleton};

/// Byte histogram over 32 dims, unit length: distinct texts get distinct
/// vectors, so FTS and vector search both run. `delay` holds each embed.
fn embed_fn(delay: Duration) -> EmbedFn {
    Arc::new(move |text: &str| {
        let mut v = vec![0f32; 32];
        for b in text.as_bytes() {
            v[(*b as usize) % 32] += 1.0;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        for x in &mut v {
            *x /= norm;
        }
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(v)
        })
    })
}

/// The engine through the port svrn holds, never `CorpusEngine`'s
/// same-named inherent methods.
fn port(engine: &CorpusEngine) -> &dyn LocalCorpusPort {
    engine
}

/// The engine as svrn's daemon builds it for local corpora: recipe
/// overrides and indexes under one data dir, where svrn writes recipes.
fn engine_at(dir: &Path, delay: Duration) -> CorpusEngine {
    CorpusEngine::new(dir.join("recipes"), dir.join("indexes"), embed_fn(delay))
        .with_embedding_model("test-mock")
}

/// Stage `rows` as svrn's extract stage does and write `recipe_toml`'s
/// shape over them to `<dir>/recipes/<id>.toml`; returns the recipe path.
fn stage_folder(dir: &Path, id: &str, rows: &[(&str, &str)]) -> PathBuf {
    let staged = dir.join("staging").join(format!("{id}.jsonl"));
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    let lines: Vec<String> = rows
        .iter()
        .map(|(doc_id, content)| {
            serde_json::json!({ "id": doc_id, "title": doc_id, "content": content }).to_string()
        })
        .collect();
    std::fs::write(&staged, lines.join("\n") + "\n").unwrap();
    let recipe = format!(
        r#"[corpus]
id = "{id}"
name = "Fixture folder"
description = "Local corpus (watched): Fixture folder"
license = "local"
mesh_sharing = false
scope = "local"
grantable = true
schema_version = 1

[acquire]
type = "local_file"
path = "{staged}"

[extract]
type = "jsonl"
content_field = "content"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 1500
overlap_chars = 150

[index]
fts = true
vector = true

[retrieval]
personal_scope = true

[display]
category = "watched_folder"
icon = "folder"

[enrichment]
enabled = true
type = "tiered"
"#,
        staged = staged.display()
    );
    let path = dir.join("recipes").join(format!("{id}.toml"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, recipe).unwrap();
    path
}

const FOLDER: &[(&str, &str)] = &[
    (
        "alpha.txt",
        "Alpha document. The FOIA response covers budget allocations for 2023.",
    ),
    (
        "beta.txt",
        "Beta document discusses climate policy and carbon pricing mechanisms.",
    ),
    (
        "notes/gamma.md",
        "Gamma document contains meeting minutes and action items.",
    ),
];

async fn doc_rows(engine: &CorpusEngine, id: &str, doc: &str) -> Vec<(Option<String>, bool)> {
    let index = port(&engine)
        .open_index_for_corpus(id)
        .await
        .expect("open index");
    index
        .chunks_by_source_doc_ids(&[doc.to_string()])
        .await
        .expect("rows by doc id")
        .into_iter()
        .map(|r| (r.source_doc_id, r.title.is_some()))
        .collect()
}

/// local_corpus_e2e's register → ingest → search (and the vault variant),
/// watched_folder_e2e's initial ingest, obsidian's root-relative doc ids:
/// a staged folder recipe ingests into a meta-stamped index whose chunks
/// carry svrn's ids and answer a search.
#[tokio::test]
async fn a_staged_folder_recipe_ingests_to_a_searchable_index() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_at(dir.path(), Duration::ZERO);
    let recipe = stage_folder(dir.path(), "watched-fixture", FOLDER);

    let ingested = port(&engine)
        .ingest_recipe_path(&recipe, None)
        .await
        .expect("ingest");
    assert_eq!(ingested.corpus_id, "watched-fixture");
    assert!(ingested.chunks_created >= 3, "{}", ingested.chunks_created);
    assert!(dir
        .path()
        .join("indexes/watched-fixture/_corpus_meta.json")
        .is_file());

    for (doc, _) in FOLDER {
        let rows = doc_rows(&engine, "watched-fixture", doc).await;
        assert!(!rows.is_empty(), "{doc} indexes under its root-relative id");
    }

    let query = "climate policy carbon";
    let embedding = port(&engine).embed(query).await.expect("embed");
    let index = port(&engine)
        .open_index_for_corpus("watched-fixture")
        .await
        .unwrap();
    let hits = index.search(&embedding, query, 5).await.expect("search");
    let top = hits.first().expect("a hit").content.to_ascii_lowercase();
    assert!(top.contains("climate") || top.contains("carbon"), "{top}");
}

/// watched_folder_e2e's empty-folder case: `ensure_empty_index` leaves a
/// meta-stamped, empty index, and a later watched update adds to it.
#[tokio::test]
async fn an_empty_index_is_sweepable_and_takes_a_late_add() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine_at(dir.path(), Duration::ZERO));
    let recipe = stage_folder(dir.path(), "watched-empty", &[]);

    port(&engine)
        .ensure_empty_index(&recipe)
        .await
        .expect("empty index");
    assert!(dir
        .path()
        .join("indexes/watched-empty/_corpus_meta.json")
        .is_file());
    let index = port(&engine)
        .open_index_for_corpus("watched-empty")
        .await
        .unwrap();
    assert_eq!(index.chunk_count().await.unwrap(), 0);

    apply(
        &engine,
        "watched-empty",
        &[("late.md", "arrived after registration")],
        &[],
        &[],
    )
    .await;
    assert!(!doc_rows(&engine, "watched-empty", "late.md")
        .await
        .is_empty());
}

async fn apply(
    engine: &Arc<CorpusEngine>,
    id: &str,
    added: &[(&str, &str)],
    updated: &[(&str, &str)],
    deleted: &[&str],
) {
    let texts: HashMap<String, String> = added
        .iter()
        .chain(updated)
        .map(|(d, t)| (d.to_string(), t.to_string()))
        .collect();
    let entries = texts
        .keys()
        .map(|d| (d.clone(), format!("hash-of-{}", texts[d].len())))
        .collect();
    let update = WatchedUpdate {
        corpus_id: id.to_string(),
        version: "sweep".into(),
        entries,
        new_documents: added.iter().map(|(d, _)| d.to_string()).collect(),
        updated_documents: updated.iter().map(|(d, _)| d.to_string()).collect(),
        deleted_documents: deleted.iter().map(|d| d.to_string()).collect(),
    };
    let texts = Arc::new(texts);
    let fetch: DocFetchFn = Arc::new(move |doc: &str| {
        let text = texts.get(doc).cloned();
        let doc = doc.to_string();
        Box::pin(async move {
            text.ok_or_else(|| corpus_index::Error::Recipe(format!("no text for {doc}")))
        })
    });
    Arc::clone(engine)
        .apply_watched_update(&update, fetch, Box::new(|_, _, _| {}))
        .await
        .expect("apply watched update");
}

/// obsidian_live_sync_e2e's write-back round trip, the engine half (and
/// the delta application behind watched_folder_e2e's add / modify / remove
/// sweeps): repeated updates of one document replace its chunks, never
/// duplicating them, and each replacement keeps the doc id and a title;
/// an add lands and a delete leaves nothing. The update resolves the
/// recipe svrn wrote under the engine's recipes dir.
#[tokio::test]
async fn a_watched_update_replaces_a_doc_and_keeps_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine_at(dir.path(), Duration::ZERO));
    let recipe = stage_folder(dir.path(), "obsidian-fixture", FOLDER);
    port(&engine)
        .ingest_recipe_path(&recipe, None)
        .await
        .unwrap();

    let before = doc_rows(&engine, "obsidian-fixture", "notes/gamma.md")
        .await
        .len();
    for i in 0..2 {
        let text = format!(
            "Gamma minutes, edit number {i}: {}",
            "x".repeat(16 * (i + 1))
        );
        apply(
            &engine,
            "obsidian-fixture",
            &[],
            &[("notes/gamma.md", &text)],
            &[],
        )
        .await;
        let rows = doc_rows(&engine, "obsidian-fixture", "notes/gamma.md").await;
        assert_eq!(rows.len(), before, "edit {i} duplicated chunks: {rows:?}");
        for (doc_id, has_title) in rows {
            assert_eq!(doc_id.as_deref(), Some("notes/gamma.md"));
            assert!(has_title, "a delta-produced chunk keeps a title");
        }
    }

    apply(
        &engine,
        "obsidian-fixture",
        &[("delta.md", "Brand new note.")],
        &[],
        &["alpha.txt"],
    )
    .await;
    assert!(!doc_rows(&engine, "obsidian-fixture", "delta.md")
        .await
        .is_empty());
    assert!(doc_rows(&engine, "obsidian-fixture", "alpha.txt")
        .await
        .is_empty());
}

/// local_corpus_e2e's remove-while-ingesting, the engine half: an ingest
/// is registered while it runs, a cancel stops it, and the wipe after it
/// leaves no index dir for the corpus (canonical or partition).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remove_after_cancel_leaves_no_index_dir() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine_at(dir.path(), Duration::from_millis(200)));
    let recipe = stage_folder(dir.path(), "watched-race", FOLDER);

    let ingest = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move { port(&engine).ingest_recipe_path(&recipe, None).await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !port(&engine).ingest_in_flight("watched-race") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the ingest never registered"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(port(&engine).cancel_corpus_ingest("watched-race"));
    let _ = ingest.await.expect("ingest task must not panic");
    assert!(!port(&engine).ingest_in_flight("watched-race"));

    port(&engine)
        .remove_corpus_everything("watched-race")
        .expect("remove");
    let leftovers: Vec<String> = std::fs::read_dir(dir.path().join("indexes"))
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with("watched-race"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// knowledge_view_e2e's view ingest, the engine half: a view recipe of the
/// personal-knowledge shape ingests the rows its registered custom acquirer
/// writes (svrn's is the sqlite acquirer, registered by kind).
#[tokio::test]
async fn a_view_recipe_ingests_through_its_registered_acquirer() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_at(dir.path(), Duration::ZERO);
    let acquirer: CustomAcquirerFn = Arc::new(|_params, download_dir| {
        Box::pin(async move {
            std::fs::create_dir_all(&download_dir)?;
            let out = download_dir.join("rows.jsonl");
            let rows = [
                (
                    "m1",
                    "I keep coming back to the question of meaningful work.",
                ),
                ("m2", "My work matters when it serves others."),
            ];
            let lines: Vec<String> = rows
                .iter()
                .map(|(id, c)| serde_json::json!({ "id": id, "content": c }).to_string())
                .collect();
            std::fs::write(&out, lines.join("\n") + "\n")?;
            Ok(out)
        })
    });
    port(&engine).register_acquirer("sqlite", acquirer);

    let recipe = dir.path().join("personal-knowledge.toml");
    std::fs::write(
        &recipe,
        r#"[corpus]
id = "personal-knowledge"
name = "Personal knowledge"
description = "Memories."
license = "local-only"
mesh_sharing = false
scope = "local"
query_sharing = false
grantable = false
schema_version = 1
kind = "knowledge"

[acquire]
type = "custom"
kind = "sqlite"

[acquire.params]
db_path = "/fixture/sovereign.db"
query = "SELECT id, content FROM memories"
content_column = "content"
id_column = "id"

[extract]
type = "jsonl"
content_field = "content"

[chunk]
type = "passthrough"

[index]
fts = true
vector = true

[enrichment]
enabled = true
type = "field_model"
domain = "personal"
prompt_version = "v1"
"#,
    )
    .unwrap();

    let ingested = port(&engine)
        .ingest_recipe_path(&recipe, None)
        .await
        .expect("view ingest");
    assert_eq!(ingested.corpus_id, "personal-knowledge");
    assert_eq!(ingested.chunks_created, 2, "one passthrough chunk per row");
}

/// knowledge_view_e2e's planted skeleton, the engine half: ingest's
/// skeleton writer and the leaf reader the splice uses agree on the file
/// and its content, so a skeleton svrn's test writes as JSON under
/// `LEGACY_ARTIFACT` is the one ingest writes.
#[test]
fn the_engine_skeleton_writer_and_the_leaf_reader_agree() {
    let dir = tempfile::tempdir().unwrap();
    let skeleton = FieldSkeleton {
        schema_version: 1,
        corpus_id: "personal-knowledge".into(),
        generated_at: "2026-04-20T00:00:00Z".into(),
        extraction_method: "fixture".into(),
        prompt_version: "v1".into(),
        domain_id: "personal".into(),
        canonical_questions: vec![CanonicalQuestion {
            id: "q1".into(),
            question: "What does meaningful work look like for me?".into(),
            status: "contested".into(),
            question_type: "normative".into(),
            primary_entries: vec![],
            positions: vec![],
            fault_lines: vec![],
        }],
        open_questions: vec![],
        field_stats: FieldModelStats::default(),
    };
    write_field_skeleton(dir.path(), &skeleton).expect("engine writes");
    assert!(dir.path().join(LEGACY_ARTIFACT).is_file());
    let read = load_field_skeleton(dir.path())
        .expect("leaf reads")
        .expect("present");
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        serde_json::to_value(&skeleton).unwrap()
    );
    let as_svrn_writes = serde_json::to_string_pretty(&skeleton).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join(LEGACY_ARTIFACT)).unwrap(),
        as_svrn_writes
    );
}
