//! `wikipedia_fetch` agent-callable tool.
//!
//! Phase W4 — when retrieval surfaces a catalog hit on the
//! `wikipedia-catalog` corpus and no strong full-text answer is
//! available, the agent calls this tool to fetch the article from
//! Wikipedia, ingest it into the corpus mesh under
//! `parent_corpus_id = "wikipedia"`, and return a small summary the
//! agent can quote / cite. Subsequent queries about the same topic
//! land directly on the freshly-ingested chunks (Tier-0).
//!
//! ## Why a tool, not a runtime auto-fire
//!
//! The agentic surface gives the model the affordance to decide:
//! "low-confidence retrieval + strong catalog hit = fetch". The
//! KnowledgeSearch tool's catalog-hit formatting in `knowledge.rs`
//! already nudges this; the system prompt makes it the default.
//! A blind runtime hook would either over-fetch (every miss → 30s
//! latency) or under-fetch (require a confidence threshold the
//! retrieval scoring doesn't currently produce reliably). The
//! tool path keeps the policy in the model where it's adjustable.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine::CorpusEngine;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use crate::catalog_ingest::{
    run_catalog_ingest, CatalogIngestEvent, CatalogIngestRequest,
};

/// Default catalog corpus id paired with this tool. Operators with a
/// custom catalog setup can wrap [`run_catalog_ingest`] directly
/// instead.
pub const WIKIPEDIA_CATALOG_CORPUS_ID: &str = "wikipedia-catalog";

pub struct WikipediaFetchTool {
    engine: Arc<CorpusEngine>,
    /// Where per-article corpora land. Defaults to the engine's
    /// configured indexes dir; surfaced for tests.
    #[allow(dead_code)]
    indexes_dir: PathBuf,
}

impl WikipediaFetchTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        let indexes_dir = engine.index_dir().to_path_buf();
        Self {
            engine,
            indexes_dir,
        }
    }
}

#[async_trait]
impl Tool for WikipediaFetchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "wikipedia_fetch".to_string(),
            name: "Wikipedia: Fetch Article".to_string(),
            description: concat!(
                "Fetch a specific Wikipedia article on demand and ingest it into the user's ",
                "Wikipedia corpus. Use this when the catalog (`wikipedia-catalog`) surfaces a ",
                "title that matches the user's question but the full article hasn't been ",
                "indexed yet — the next retrieval will return the article's content directly. ",
                "Takes 15-60 seconds per article (download + chunk + embed); always inform the ",
                "user before calling. The fetched article carries `parent_corpus_id = wikipedia` ",
                "so it surfaces under the user's existing Wikipedia corpus and is shared across ",
                "the mesh like any other Wikipedia content."
            )
            .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "The exact article title from a catalog hit (e.g. \"Roman Empire\", \"Albert Einstein\"). Case-sensitive; pass the title verbatim from the CatalogHit metadata."
                    },
                    "enrich": {
                        "type": "boolean",
                        "description": "If true, also build the per-article atlas (~1-2 minutes additional). Default false — atlas grounding still works without per-article enrichment because the article's chunks land in the wikipedia corpus's atlas grounding bag.",
                        "default": false
                    },
                    "expand_links": {
                        "type": "boolean",
                        "description": "If true, queue a one-hop link-expansion (\"minesweeper\") after the primary fetch — top-N articles linked from this one are pre-loaded into the same shared corpus. Default true. Set false for batch / seed flows (e.g. Tier-1.5 placeholder seeding) where the caller wants exactly the requested article and no automatic neighbours.",
                        "default": true
                    }
                },
                "required": ["title"]
            }),
            examples: vec![],
            // External effect: hits the Wikipedia REST API and writes
            // a new corpus to disk. Network + filesystem.
            effect: Effect::ReadWrite,
            // Repeated calls with the same title are idempotent —
            // run_catalog_ingest's per-work corpus check
            // short-circuits when `wikipedia-catalog-<title>` is
            // already installed.
            idempotency: Idempotency::Idempotent,
            // 15-60s round trip. Don't block the chat thread; the
            // chat-loop should expose this as a "fetching…" status.
            latency: Latency::Slow,
            // Persists across sessions — the fetched corpus stays
            // installed, gets atlas-built by the post-install hook,
            // and is mesh-shared.
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "new_corpus_id": {"type": "string"},
                    "chunks_created": {"type": "integer"},
                    "title": {"type": "string"}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::FileWrite, Permission::Network]
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'title' parameter".into()))?
            .to_string();
        let enrich = params
            .get("enrich")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let expand_links = params
            .get("expand_links")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // The Action API URL substitutes spaces with underscores in
        // titles. We pass the raw title to run_catalog_ingest as the
        // work_id; the catalog config's download_url_template handles
        // the substitution at request-time. Wikipedia's API accepts
        // both encoded ("Albert%20Einstein") and underscored
        // ("Albert_Einstein") forms; underscores are simpler.
        let work_id = title.replace(' ', "_");

        let req = CatalogIngestRequest {
            catalog_corpus_id: WIKIPEDIA_CATALOG_CORPUS_ID.to_string(),
            work_id: work_id.clone(),
            enrich,
            // No progress callback in the tool path — the synthesis
            // layer already wraps tool calls with a generic
            // "running…" surface. Detailed per-event progress is
            // the desktop's job.
            progress: None,
            cancel: None,
            // Top-level user fetch: trigger one-hop link-expansion if
            // the catalog config opts in AND the caller didn't
            // override `expand_links: false` (the recursively-queued
            // expansion calls also pass false to bound depth at one).
            expand_links,
        };

        // run_catalog_ingest takes Arc<CorpusEngine> + the request;
        // it handles resolution + recipe load + ingest + (optional)
        // enrich + atlas summary in one call. Returns the new
        // corpus id on success; we re-shape into a tool-output
        // string so the agent can quote it back to the user.
        let new_corpus_id =
            run_catalog_ingest(Arc::clone(&self.engine), req).await.map_err(|e| {
                Error::Execution(format!("wikipedia_fetch: {e}"))
            })?;

        Ok(StepOutput::Text(format!(
            "Fetched \"{title}\" from Wikipedia and appended to the shared `{new_corpus_id}` \
             corpus. The next retrieval over Wikipedia will include this article's chunks \
             directly. A one-hop link-expansion has been queued in the background — articles \
             linked from this one will be ingested into the same corpus over the next minute \
             or so, so follow-up questions about the surrounding topic should land instantly."
        )))
    }
}
