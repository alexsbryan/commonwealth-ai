// SPDX-License-Identifier: AGPL-3.0-or-later
//! Semantic code search over installed code corpora.
//!
//! Trust contract: results are **approximate**. Every response is
//! prefixed with the literal string `*Approximate results for*` — the
//! label is compile-time enforced so there is no code path that returns
//! results without it.
//!
//! Ranking comes from `Table::nearest_to` when an embedding is available,
//! falling back to `Table::full_text_search` when no inference provider
//! is wired or embedding fails. Either way results are labelled
//! approximate — the label is about epistemic honesty, not about which
//! backend produced the ranking.

use std::sync::Arc;

use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;

use super::{escape_sql, extract_code_rows_pub, CodeRow};

/// Semantic search over installed code corpora.
/// A code corpus whose chunk index is at least this many days old gets
/// an "aging" posture note appended to results (see execute()).
const CHUNK_STALE_DAYS: u64 = 7;

pub struct CodeSearchTool {
    engine: Arc<CorpusEngine>,
    inference: Option<Arc<dyn InferenceProvider>>,
}

impl CodeSearchTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self {
            engine,
            inference: None,
        }
    }

    /// Wire in an inference provider for query embedding. Optional —
    /// without it, the tool falls back to FTS-only search.
    pub fn with_inference(mut self, inference: Arc<dyn InferenceProvider>) -> Self {
        self.inference = Some(inference);
        self
    }
}

/// Mandatory prefix on every response. Tests assert this is present.
const APPROXIMATE_HEADER: &str = "*Approximate results for*";

fn format_approximate_response(query: &str, rows: &[CodeRow]) -> String {
    let body = if rows.is_empty() {
        "No semantically similar code found. If you know the exact name, \
         `symbol_lookup` is always correct. Otherwise try different terms."
            .to_string()
    } else {
        rows.iter()
            .map(|r| {
                format!(
                    "```{lang}\n\
                     // {file}:{start}-{end}  [{kind}]  ({corpus})\n\
                     {content}\n\
                     ```",
                    lang = r.language,
                    file = r.file_path,
                    start = r.line_start + 1,
                    end = r.line_end + 1,
                    kind = r.symbol_kind,
                    corpus = r.corpus_id,
                    content = r.content,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    format!(
        "{APPROXIMATE_HEADER} `{query}`\n\n\
         Based on semantically similar code in the local corpus:\n\n\
         {body}"
    )
}

#[async_trait]
impl Tool for CodeSearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "code_search".to_string(),
            name: "Code Search".to_string(),
            description: "Semantic search over the indexed codebase. \
                          PREFER THIS OVER READING FILES when you need to understand \
                          how something is done, find implementations of a pattern, \
                          or locate relevant code before making a change. Returns the \
                          3-5 most relevant chunks — typically 30-50 tokens each — \
                          versus reading an entire file which may cost 200-500 tokens \
                          and contain mostly irrelevant content. Use read_file only \
                          when you need a complete, authoritative view of a specific \
                          file you have already located."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Description of what you're looking for"
                    },
                    "language": {
                        "type": "string",
                        "description": "Optional language filter: rust, typescript, javascript, go, python",
                        "default": ""
                    }
                },
                "required": ["query"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You need to find how a pattern is implemented before writing something similar. Don't read random files — search for the pattern semantically and get the 3-5 most relevant chunks.".into(),
                    call: serde_json::json!({ "query": "streaming SSE response handler" }),
                },
                ToolExample {
                    situation: "You're about to write a Python/shell script to grep for examples of a pattern across the codebase. This does it in one call and ranks results by relevance.".into(),
                    call: serde_json::json!({ "query": "retry logic with exponential backoff" }),
                },
                ToolExample {
                    situation: "You know the concept but not the exact symbol name. Use this to find it, then follow up with symbol_lookup for the precise definition.".into(),
                    call: serde_json::json!({ "query": "tool permission validation before execute", "language": "rust" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Fenced code blocks ranked by relevance; same format \
                                as symbol_lookup (`// file:start-end [kind] (corpus)`). \
                                Lower relevance than symbol_lookup — results are \
                                approximate."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("query")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("code_search requires 'query'".into()))?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'query'".to_string()))?;
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        // Embed the query if we have inference. Empty vector is the
        // sentinel that triggers the FTS-only path below.
        let embedding: Vec<f32> = match &self.inference {
            Some(inf) => inf.embed(query).await.unwrap_or_default(),
            None => Vec::new(),
        };

        let base_filter = match language {
            Some(lang) => {
                let lang_lit = escape_sql(lang);
                format!("symbol_name IS NOT NULL AND language = '{lang_lit}'")
            }
            None => "symbol_name IS NOT NULL".to_string(),
        };

        let indexes = self
            .engine
            .installed_indexes()
            .await
            .map_err(|e| Error::Tool {
                tool_id: "code_search".to_string(),
                message: format!("enumerate corpora: {e}"),
            })?;

        let mut rows: Vec<CodeRow> = Vec::new();
        // Track index health so an empty result is never mistaken for "the
        // code doesn't exist" when the real cause is a missing/stale chunk
        // index (the silent-degradation trap — a skipped corpus used to
        // vanish into a `tracing::debug!`).
        let code_corpora = indexes
            .iter()
            .filter(|i| super::has_code_graph(i))
            .count();
        let mut opened_ok = 0usize;
        // Chunk-index age per code corpus: `last_updated` is stamped by
        // `sovereign code index`, so a corpus nobody re-indexes keeps
        // opening fine while silently omitting everything newer than the
        // stamp (observed: a workspace chunk index 28 days behind the
        // live SCIP graph). Surface it past CHUNK_STALE_DAYS.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut aging_corpora: Vec<(String, u64)> = Vec::new();
        for info in &indexes {
            if !super::has_code_graph(info) {
                continue;
            }
            let age_days = now_secs.saturating_sub(info.last_updated) / 86_400;
            if info.last_updated > 0 && age_days >= CHUNK_STALE_DAYS {
                aging_corpora.push((info.corpus_id.clone(), age_days));
            }
            let Ok(index) = self.engine.open_index(&info.path).await else {
                continue;
            };
            opened_ok += 1;
            let table = index.table();

            // Vector-first if we have an embedding, else FTS.
            let batches_result = if !embedding.is_empty() {
                match table.query().nearest_to(embedding.clone()) {
                    Ok(q) => q.only_if(base_filter.clone()).limit(16).execute().await,
                    Err(e) => {
                        tracing::debug!(corpus = %info.corpus_id, "vector query build failed: {e}");
                        continue;
                    }
                }
            } else {
                table
                    .query()
                    .full_text_search(FullTextSearchQuery::new(query.to_string()))
                    .only_if(base_filter.clone())
                    .limit(16)
                    .execute()
                    .await
            };

            let batches = match batches_result {
                Ok(s) => match s.try_collect::<Vec<_>>().await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!(corpus = %info.corpus_id, "collect failed: {e}");
                        continue;
                    }
                },
                Err(e) => {
                    tracing::debug!(corpus = %info.corpus_id, "query execute failed: {e}");
                    continue;
                }
            };

            for batch in &batches {
                extract_code_rows_pub(batch, &info.corpus_id, &mut rows);
            }
        }

        // Dedupe by (file_path, symbol_name, line_start) — split chunks
        // of the same symbol surface once. Stable sort preserves the
        // relative order from the underlying ranked results.
        rows.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then_with(|| a.symbol_name.cmp(&b.symbol_name))
                .then_with(|| a.line_start.cmp(&b.line_start))
        });
        rows.dedup_by(|a, b| {
            a.file_path == b.file_path
                && a.symbol_name == b.symbol_name
                && a.line_start == b.line_start
        });
        rows.truncate(8);

        let mut text = format_approximate_response(query, &rows);

        // Index-health note (glassbox) — reuses the code-corpora tally from
        // the search loop above rather than re-enumerating. Three distinct
        // states so an empty result is never read as "the code isn't there":
        //   - no code corpora installed  → build the index
        //   - corpora present but some/all failed to open → DEGRADED (stale /
        //     missing chunk index); the symbol may exist but not appear here
        //   - corpora all opened, still empty → a genuine no-match (no note)
        if code_corpora == 0 {
            text.push_str(
                "\n\n---\nIndex: absent | 0 code corpora\n\
                 Code index not built. Run `sovereign code index <path>` \
                 to enable semantic code search.",
            );
        } else if opened_ok < code_corpora {
            let failed = code_corpora - opened_ok;
            text.push_str(&format!(
                "\n\n---\nIndex: DEGRADED | {failed}/{code_corpora} code corpora unreadable\n\
                 The chunk index for {failed} of {code_corpora} code corpora is missing or \
                 stale, so these results are INCOMPLETE — a symbol you searched for may EXIST \
                 but not appear here. Do not conclude the code is absent. Rebuild with \
                 `sovereign project refresh`. For an exact name, `symbols` reads the SCIP graph \
                 directly and is unaffected by this."
            ));
        } else if !aging_corpora.is_empty() {
            aging_corpora.sort_by(|a, b| b.1.cmp(&a.1));
            let worst = aging_corpora
                .iter()
                .take(3)
                .map(|(id, days)| format!("{id} ({days}d)"))
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!(
                "\n\n---\nIndex: aging | {n} of {code_corpora} code corpora stale: {worst}\n\
                 These chunk indexes predate recent edits — results omit anything newer than \
                 the last index run. For an exact name, `symbols` reads the live SCIP graph \
                 and is unaffected. Refresh with \
                 `sovereign code index <repo-root> --corpus-id=<id>`.",
                n = aging_corpora.len()
            ));
        }

        Ok(StepOutput::Text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approximate_header_present_for_empty_results() {
        let body = format_approximate_response("test", &[]);
        assert!(body.starts_with(APPROXIMATE_HEADER));
        assert!(body.contains("symbol_lookup"));
    }

    #[test]
    fn approximate_header_present_for_results() {
        let row = CodeRow {
            symbol_name: "foo".into(),
            symbol_kind: "function".into(),
            file_path: "src/lib.rs".into(),
            line_start: 0,
            line_end: 5,
            language: "rust".into(),
            mtime: 0,
            content: "fn foo() {}".into(),
            corpus_id: "test".into(),
        };
        let body = format_approximate_response("how to foo", &[row]);
        assert!(body.starts_with(APPROXIMATE_HEADER));
        assert!(body.contains("`how to foo`"));
        assert!(body.contains("fn foo()"));
    }
}
