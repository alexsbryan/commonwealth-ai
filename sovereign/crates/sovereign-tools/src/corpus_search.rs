// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus_search` — the workflow **read** step: rank a corpus by similarity to a
//! query vector. The mirror of [`crate::corpus_store::CorpusStoreTool`]: where
//! `store` writes `(chunk, embedding)` pairs, `search` reads them back, ranked.
//!
//! This closes the substrate's read side. Until now a workflow could *build* a
//! corpus (`tool:corpus_store`) but not *query* one — so the whole class of
//! read-side workflows (recommendation, retrieval-augmented steps, "find related",
//! dedup) had no step to stand on. `corpus_search` is that step.
//!
//! **Pre-embedded, like `store`.** The query arrives as a vector in the
//! `embedding` param (the upstream `embed:default` step's output) — symmetric with
//! `store`, which takes pre-embedded `embeddings`. No inference handle is needed at
//! execute time, so the tool is a bare unit struct and a daemon-free unit test
//! exercises it end to end. An optional `query` text adds lexical (FTS) recall on
//! top of vector similarity (the corpus engine's hybrid path).
//!
//! **Real scores.** It searches through `CorpusIndex::search` — the same call the
//! bespoke retrieval makes — which returns real cosine / RRF-hybrid scores in
//! `[0, 1]`. It deliberately does NOT use the `DocumentStore::search_documents_scored`
//! trait default, whose fallback impl returns `score: 0.0`. Effect is `Read`.

use async_trait::async_trait;

use corpus_engine::CorpusIndex;
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct CorpusSearchTool;

#[async_trait]
impl Tool for CorpusSearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "corpus_search".to_string(),
            name: "corpus_search".to_string(),
            description: "Rank a corpus by similarity to a query vector. Returns the top-k \
                          hits as a collection of {source_doc_id, title, score, text} — the \
                          read side of `corpus_store`. Pre-embed the query with `embed:default`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "corpus": { "type": "string", "description": "Corpus id (directory under the index dir)" },
                    "embedding": { "type": "string", "description": "Query vector — e.g. {seed_vec.output} from an embed: step" },
                    "query": { "type": "string", "description": "Optional query text for hybrid lexical (FTS) recall on top of vector similarity" },
                    "top_k": { "type": "string", "description": "How many hits to return (default 10)" },
                    "index_dir": { "type": "string", "description": "Index root. Default: ~/.sovereign/indexes" }
                },
                "required": ["corpus", "embedding"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "source_doc_id": { "type": "string" },
                        "title": { "type": "string" },
                        "score": { "type": "number" },
                        "text": { "type": "string" }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![] // Reading your own corpus needs no special permission.
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let query_text = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let top_k = parse_top_k(params).unwrap_or(10);
        let embedding = parse_embedding(params)?;

        let index_dir = match params.get("index_dir").and_then(|v| v.as_str()) {
            Some(d) => std::path::PathBuf::from(d),
            None => default_index_dir(),
        };
        let corpus_path = index_dir.join(&corpus);

        // A clear miss is more useful than a deep LanceDB error: point the author
        // at the build step. (`genome` composes with `notebook`, which builds.)
        if !corpus_path.exists() {
            return Err(Error::Execution(format!(
                "corpus_search: corpus `{corpus}` not found under {} — build it first, \
                 e.g. `sovereign workflow run notebook --folder … --corpus {corpus}`",
                index_dir.display()
            )));
        }

        let index = CorpusIndex::open(&corpus_path)
            .await
            .map_err(|e| Error::Execution(format!("corpus_search: open `{corpus}`: {e}")))?;

        // The retrieval read path: real cosine / RRF-hybrid scores in [0, 1].
        let hits = index
            .search(&embedding, query_text, top_k)
            .await
            .map_err(|e| Error::Execution(format!("corpus_search: search `{corpus}`: {e}")))?;

        // Ranked collection — a JSON array makes this `for_each`-able downstream
        // (`{element.title}`, `{element.score}`, …). `search` already returns hits
        // sorted by descending score.
        let out: Vec<serde_json::Value> = hits
            .into_iter()
            .map(|h| {
                serde_json::json!({
                    "source_doc_id": h.source_doc_id,
                    "title": h.title,
                    "score": h.score,
                    "text": h.content,
                })
            })
            .collect();

        Ok(StepOutput::Json(serde_json::Value::Array(out)))
    }
}

fn str_param(params: &serde_json::Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Execution(format!("corpus_search: missing required `{key}`")))
}

/// `top_k` may arrive as a JSON number *or* a string (`"10"`) — templating
/// stringifies params. Accept both; `None` lets the caller default.
fn parse_top_k(params: &serde_json::Value) -> Option<usize> {
    let v = params.get("top_k")?;
    v.as_u64()
        .map(|n| n as usize)
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<usize>().ok()))
}

/// The query vector from the `embedding` param. Accepts either a JSON *string*
/// (templating stringified the upstream artifact) or a structured array (a lone
/// `{seed_vec.output}` value-splice), and either a flat `[num, …]` (a single
/// `embed:` step's output) or a nested `[[num, …]]` (array-of-one) — taking the
/// first vector in the nested case. Symmetric with `corpus_store`'s acceptance of
/// both collection forms.
fn parse_embedding(params: &serde_json::Value) -> Result<Vec<f32>> {
    let raw = match params.get("embedding") {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s)
            .map_err(|e| Error::Execution(format!("corpus_search: parse `embedding`: {e}")))?,
        Some(other) => other.clone(),
        None => return Err(Error::Execution("corpus_search: missing required `embedding`".into())),
    };
    if let Ok(flat) = serde_json::from_value::<Vec<f64>>(raw.clone()) {
        if !flat.is_empty() {
            return Ok(flat.into_iter().map(|x| x as f32).collect());
        }
    }
    if let Ok(nested) = serde_json::from_value::<Vec<Vec<f64>>>(raw) {
        if let Some(first) = nested.into_iter().find(|v| !v.is_empty()) {
            return Ok(first.into_iter().map(|x| x as f32).collect());
        }
    }
    Err(Error::Execution(
        "corpus_search: `embedding` must be a non-empty vector of numbers".into(),
    ))
}

/// `~/.sovereign/indexes` — the canonical corpus root retrieval reads, derived
/// from the same home-dir resolution as the setup config (mirrors `corpus_store`).
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
    use crate::corpus_store::CorpusStoreTool;

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

    /// Definition of done: store a corpus, then `corpus_search` it by one of the
    /// stored vectors and get that chunk back, ranked first, with a REAL score
    /// (not the 0.0 default). CI-safe — temp index dir, deterministic vectors,
    /// vector flat-scan (no daemon/weights).
    #[tokio::test]
    async fn search_ranks_the_corpus_with_real_scores() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("indexes");

        // Build a tiny corpus via the write tool (the symmetric mirror).
        let chunks = serde_json::json!([
            { "text": "Mr Verloc kept a shabby shop in Soho", "index": 0 },
            { "text": "The Assistant Commissioner left Scotland Yard at dusk", "index": 1 },
            { "text": "Winnie Verloc guarded her brother Stevie above all", "index": 2 }
        ])
        .to_string();
        let embeddings = serde_json::json!([
            [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            [0.2, 0.1, 0.0, 0.9, 0.3, 0.3, 0.1, 0.2],
            [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2]
        ])
        .to_string();
        let store_params = serde_json::json!({
            "corpus": "conrad",
            "chunks": chunks,
            "embeddings": embeddings,
            "source_doc_id": "secret-agent",
            "title": "The Secret Agent",
            "index_dir": index_dir.to_string_lossy(),
            "build_indexes": false
        });
        CorpusStoreTool.execute(&store_params, &ctx()).await.unwrap();

        // Search by the "Stevie" vector — it must come back ranked first.
        let search_params = serde_json::json!({
            "corpus": "conrad",
            "embedding": [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2],
            "top_k": "5",
            "index_dir": index_dir.to_string_lossy()
        });
        let out = CorpusSearchTool
            .execute(&search_params, &ctx())
            .await
            .unwrap();
        let arr = match out {
            StepOutput::Json(serde_json::Value::Array(a)) => a,
            o => panic!("corpus_search must return a JSON array; got {o:?}"),
        };

        assert!(!arr.is_empty(), "search returned no hits");
        let top = &arr[0];
        assert!(
            top.get("text").and_then(|v| v.as_str()).unwrap_or("").contains("Stevie"),
            "the exact-match vector must rank first; got {arr:?}"
        );
        let score = top.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(
            score > 0.0,
            "score must be a real similarity, not the 0.0 trait default; got {score}"
        );
        // Hits arrive sorted by descending score.
        let scores: Vec<f64> = arr
            .iter()
            .filter_map(|h| h.get("score").and_then(|v| v.as_f64()))
            .collect();
        assert!(
            scores.windows(2).all(|w| w[0] >= w[1]),
            "hits must be ranked by descending score: {scores:?}"
        );
    }

    /// A clear, actionable error when the corpus was never built.
    #[tokio::test]
    async fn missing_corpus_points_at_the_build_step() {
        let dir = tempfile::tempdir().unwrap();
        let params = serde_json::json!({
            "corpus": "nope",
            "embedding": [0.1, 0.2, 0.3],
            "index_dir": dir.path().join("indexes").to_string_lossy()
        });
        let err = CorpusSearchTool.execute(&params, &ctx()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not found"), "{msg}");
        assert!(msg.contains("notebook"), "should point at the build step: {msg}");
    }
}
