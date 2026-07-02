// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus_store` — the workflow **store** step: write `(chunk, embedding)` pairs
//! into a searchable corpus index.
//!
//! This is the third stage of an ingest expressed as a workflow
//! (`chunk → embed → store`). It reuses the corpus engine's lowest-level write
//! primitive (`CorpusIndex::insert_batch`) — the exact call the bespoke
//! `ingest()` pipeline makes — so a workflow-authored ingest lands in the same
//! LanceDB schema `sovereign corpus`/retrieval reads back.
//!
//! **The zip needs no new Runner primitive.** `store` is a normal (non-`for_each`)
//! tool that receives *both whole collections* as params — `chunks` =
//! `{chunk.output}`, `embeddings` = `{embed.output}` (each a JSON string via
//! templating) — and pairs them by position internally.
//!
//! **Idempotent per `source_doc_id`.** `insert_batch` always appends, so a
//! workflow re-run would duplicate. Before inserting we drop the prior rows for
//! this document (`delete_chunks_by_source_doc`), so re-ingesting a document
//! replaces its chunks rather than doubling them — the right semantic for the
//! per-item ingest model. Effect is `Write` (a real side effect, never cached).

use async_trait::async_trait;

use corpus_engine::{CorpusIndex, InsertChunk};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct CorpusStoreTool;

#[async_trait]
impl Tool for CorpusStoreTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "corpus_store".to_string(),
            name: "corpus_store".to_string(),
            description: "Write (chunk, embedding) pairs into a searchable corpus index. \
                          Pairs the `chunks` and `embeddings` collections by position; \
                          idempotent per `source_doc_id`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "corpus": { "type": "string", "description": "Corpus id (directory under the index dir)" },
                    "chunks": { "type": "string", "description": "JSON array of {text, index} (or strings) — e.g. {chunk.output}" },
                    "embeddings": { "type": "string", "description": "JSON array of embedding vectors — e.g. {embed.output}" },
                    "source_doc_id": { "type": "string", "description": "Document key; its prior rows are replaced (idempotency). Default: the corpus id." },
                    "title": { "type": "string", "description": "Title stored on each chunk" },
                    "embedding_model": { "type": "string", "description": "Embed model name recorded in corpus metadata (informational)" },
                    "index_dir": { "type": "string", "description": "Index root. Default: ~/.sovereign/indexes" },
                    "build_indexes": { "type": "boolean", "description": "Build vector+FTS indices after write (default true)" }
                },
                "required": ["corpus", "chunks", "embeddings"]
            }),
            examples: vec![],
            // A real external side effect (writes the LanceDB table), so the
            // content cache must never skip it. The per-doc overwrite makes
            // re-execution idempotent regardless.
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::Persistent,
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from);
        let source_doc_id = params
            .get("source_doc_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&corpus)
            .to_string();
        let model = params
            .get("embedding_model")
            .and_then(|v| v.as_str())
            .unwrap_or("workflow-embed")
            .to_string();
        let do_build = params
            .get("build_indexes")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // The two collections arrive either as a JSON *string* (templating
        // stringified the upstream artifact) or as an already-spliced JSON
        // *array* (a lone `{chunk.output}` ref — the workflow value-splices a
        // whole step output). Accept both, then zip by position.
        let chunk_vals: Vec<serde_json::Value> =
            serde_json::from_value(collection_param(params, "chunks")?)
                .map_err(|e| Error::Execution(format!("corpus_store: `chunks` shape: {e}")))?;
        let embeddings: Vec<Vec<f32>> = {
            let raw: Vec<Vec<f64>> =
                serde_json::from_value(collection_param(params, "embeddings")?).map_err(|e| {
                    Error::Execution(format!("corpus_store: `embeddings` shape: {e}"))
                })?;
            raw.into_iter()
                .map(|v| v.into_iter().map(|x| x as f32).collect())
                .collect()
        };
        if chunk_vals.len() != embeddings.len() {
            return Err(Error::Execution(format!(
                "corpus_store: `chunks` ({}) and `embeddings` ({}) length mismatch",
                chunk_vals.len(),
                embeddings.len()
            )));
        }
        if chunk_vals.is_empty() {
            return Ok(StepOutput::Text(format!(
                "corpus_store: nothing to store for `{corpus}`"
            )));
        }
        let dim = embeddings[0].len();

        let index_dir = match params.get("index_dir").and_then(|v| v.as_str()) {
            Some(d) => std::path::PathBuf::from(d),
            None => default_index_dir(),
        };
        let corpus_path = index_dir.join(&corpus);

        // Serialize concurrent writes to the SAME corpus. The notebook workflow
        // runs items (files) concurrently, so several `corpus_store` steps can
        // target one corpus at once — and concurrent open + insert + commit on one
        // LanceDB table races (silent lost writes or a commit-conflict failure,
        // observed as "1 of 2 files stored" at --concurrency 4). A per-corpus async
        // lock makes the open→insert→finalize critical section serial, while the
        // expensive extract/chunk/embed stages keep running concurrently.
        let lock = corpus_write_lock(&corpus_path);
        let _guard = lock.lock().await;

        let index = if corpus_path.exists() {
            CorpusIndex::open(&corpus_path)
                .await
                .map_err(|e| Error::Execution(format!("corpus_store: open `{corpus}`: {e}")))?
        } else {
            CorpusIndex::create_with_sharing(
                &corpus_path,
                &corpus,
                &corpus,
                &model,
                dim,
                false,       // mesh_sharing: a workflow corpus stays local by default
                Some(false), // query_sharing: explicit local-only
                "workflow",
            )
            .await
            .map_err(|e| Error::Execution(format!("corpus_store: create `{corpus}`: {e}")))?
        };

        // Idempotency: drop the prior rows for every document id we're about to
        // write, then insert fresh, so a re-run replaces rather than duplicates. A
        // batch may carry MANY ids — e.g. a movie catalog, one id per film, via a
        // per-chunk `source_doc_id` — so delete each distinct one (first run finds
        // none; cheap).
        let doc_ids: std::collections::HashSet<String> = chunk_vals
            .iter()
            .map(|cv| chunk_field(cv, "source_doc_id").unwrap_or_else(|| source_doc_id.clone()))
            .collect();
        for id in &doc_ids {
            index
                .delete_chunks_by_source_doc(id)
                .await
                .map_err(|e| Error::Execution(format!("corpus_store: clear `{id}`: {e}")))?;
        }

        let pairs: Vec<(InsertChunk, Vec<f32>)> = chunk_vals
            .iter()
            .zip(embeddings)
            .map(|(cv, emb)| {
                (
                    InsertChunk {
                        content: chunk_text_field(cv),
                        // Per-chunk `title` / `source_doc_id` when the chunk object
                        // carries them (a heterogeneous batch — one corpus_store call
                        // holding many distinct documents); else the call-level params.
                        title: chunk_field(cv, "title").or_else(|| title.clone()),
                        url: None,
                        metadata: None,
                        content_hash: None,
                        source_doc_id: Some(
                            chunk_field(cv, "source_doc_id")
                                .unwrap_or_else(|| source_doc_id.clone()),
                        ),
                        source_file: None,
                        code: Default::default(),
                        unit_id: None,
                    },
                    emb,
                )
            })
            .collect();
        let n = pairs.len();
        index
            .insert_batch(&pairs)
            .await
            .map_err(|e| Error::Execution(format!("corpus_store: insert: {e}")))?;

        // Build vector + FTS indices. For a small corpus (<10k rows) search falls
        // back to a flat scan, so a build hiccup there is non-fatal — the chunks
        // are written and already searchable. Log it (glassbox), don't fail.
        if do_build {
            if let Err(e) = index.build_indexes(true, true, None).await {
                tracing::warn!(
                    target: "workflow",
                    corpus = %corpus, error = %e,
                    "corpus_store: index build failed — chunks stored, searchable via flat scan"
                );
            }
        }

        // Finalize: flip the corpus out of `ingestion_in_progress` (the real
        // ingest's `mark_ingestion_complete`, engine/ingest.rs). Without this a
        // workflow-built corpus stays invisible to listing AND retrieval —
        // `installed_indexes()` filters out in-progress corpora — so the notebook
        // would build but never be queryable. Idempotent.
        index
            .mark_ingestion_complete()
            .map_err(|e| Error::Execution(format!("corpus_store: finalize `{corpus}`: {e}")))?;

        Ok(StepOutput::Text(format!(
            "stored {n} chunks into corpus `{corpus}` (doc `{source_doc_id}`, dim {dim})"
        )))
    }
}

fn str_param(params: &serde_json::Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Execution(format!("corpus_store: missing required `{key}`")))
}

/// A collection param as a `serde_json::Value`, accepting either a JSON *string*
/// (templating stringified it) or an already-structured *value* (the workflow
/// value-spliced a lone `{step.output}` ref). The caller `from_value`s it into
/// the concrete shape it needs — so `chunks = "{chunk.output}"` works regardless
/// of which form templating delivers.
fn collection_param(params: &serde_json::Value, key: &str) -> Result<serde_json::Value> {
    match params.get(key) {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s)
            .map_err(|e| Error::Execution(format!("corpus_store: parse `{key}`: {e}"))),
        Some(other) => Ok(other.clone()),
        None => Err(Error::Execution(format!(
            "corpus_store: missing required `{key}`"
        ))),
    }
}

/// An optional string field on a chunk element (object only). Lets a batch carry
/// per-chunk `title` / `source_doc_id` so one `corpus_store` call can hold many
/// distinct documents (a catalog), each correctly keyed and titled.
fn chunk_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.as_object()
        .and_then(|o| o.get(key))
        .and_then(|x| x.as_str())
        .map(String::from)
}

/// A chunk element → its text: an object's `text` field, or a bare string.
fn chunk_text_field(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(_) => v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Process-global per-corpus write lock (keyed by index path). The workflow runs
/// items concurrently; this serializes `corpus_store`'s write critical section so
/// concurrent per-file stores to one corpus don't race the LanceDB table. Scoped
/// to this process (the workflow runner) — the only writer of a workflow corpus.
fn corpus_write_lock(path: &std::path::Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("corpus_store: lock registry poisoned");
    guard
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// `~/.sovereign/indexes` — the canonical corpus root `corpus list`/retrieval
/// read, derived from the same home-dir resolution as the setup config.
fn default_index_dir() -> std::path::PathBuf {
    sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("indexes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    /// Definition of done: store → search returns the chunk; re-store is
    /// idempotent (overwrites, doesn't duplicate). CI-safe — temp index dir,
    /// deterministic embeddings, FTS flat-scan search (no daemon/weights).
    #[tokio::test]
    async fn store_then_search_finds_the_chunk_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("indexes");

        let chunks = serde_json::json!([
            { "text": "Mr Verloc kept a shabby shop in Soho", "index": 0 },
            { "text": "The Assistant Commissioner left Scotland Yard at dusk", "index": 1 },
            { "text": "Winnie Verloc guarded her brother Stevie above all", "index": 2 }
        ])
        .to_string();
        // Deterministic 8-dim vectors (semantics irrelevant — search proves via FTS).
        let embeddings = serde_json::json!([
            [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            [0.2, 0.1, 0.0, 0.9, 0.3, 0.3, 0.1, 0.2],
            [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2]
        ])
        .to_string();
        let params = serde_json::json!({
            "corpus": "conrad",
            "chunks": chunks,
            "embeddings": embeddings,
            "source_doc_id": "secret-agent",
            "title": "The Secret Agent",
            "index_dir": index_dir.to_string_lossy(),
            // Skip index build on this tiny corpus — flat scan proves searchability.
            "build_indexes": false
        });

        let out = CorpusStoreTool.execute(&params, &ctx()).await.unwrap();
        match out {
            StepOutput::Text(t) => assert!(t.contains("stored 3"), "{t}"),
            o => panic!("unexpected output: {o:?}"),
        }

        // Finalized: the corpus is marked ingestion-complete, so listing +
        // retrieval surface it (an in-progress corpus is filtered out).
        assert!(
            CorpusIndex::is_ingestion_complete(&index_dir.join("conrad")),
            "store must finalize the corpus so it's listed + queryable"
        );

        // Searchable: a vector flat-scan returns the stored chunk. Query == the
        // "Stevie" chunk's vector, empty text → vector-only mode; a corpus under
        // 10k rows is searched by brute-force scan, so no built index is needed.
        let index = CorpusIndex::open(&index_dir.join("conrad")).await.unwrap();
        let stevie_vec = vec![0.9f32, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2];
        let hits = index.search(&stevie_vec, "", 5).await.unwrap();
        assert!(
            hits.iter().any(|h| h.content.contains("Stevie")),
            "vector search must return the stored chunk; got {:?}",
            hits.iter().map(|h| &h.content).collect::<Vec<_>>()
        );

        // Idempotent: re-storing the same document replaces, not duplicates —
        // the row count stays 3, not 6.
        CorpusStoreTool.execute(&params, &ctx()).await.unwrap();
        let index2 = CorpusIndex::open(&index_dir.join("conrad")).await.unwrap();
        assert_eq!(
            index2.chunk_count().await.unwrap(),
            3,
            "re-store must overwrite the doc, not duplicate its rows"
        );
    }
}
