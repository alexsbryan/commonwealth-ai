use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use corpus_engine::recipe::CatalogConfig;
use corpus_engine::types::CorpusKind;
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::*;

use crate::catalog::{
    partition_hits_by_kind, CatalogHit, CatalogResolutionContext,
};

/// Search over ingested documents using vector similarity or text search.
pub struct KnowledgeTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
}

impl KnowledgeTool {
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            store,
            inference,
            corpus_engine: None,
        }
    }

    /// Set an optional corpus engine for searching local corpus indexes.
    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
    }
}

#[async_trait]
impl Tool for KnowledgeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "knowledge".to_string(),
            name: "Knowledge".to_string(),
            description: "Search your ingested documents for relevant information".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                },
                "required": ["query"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Prose synthesis over local knowledge corpora, with \
                                inline citations to source chunks."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![] // No special permissions needed to search own documents.
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Knowledge tool requires a 'query' string parameter".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'query' parameter".to_string()))?;

        // Try vector search via embeddings first.
        let embedding = self.inference.embed(query).await.ok();

        // ── Search user documents via StateStore ──────────────
        let store_chunks: Vec<(String, String, f32)> = if let Some(ref emb) = embedding {
            self.store
                .search_documents_scored(emb, query, 5)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|sc| (sc.chunk.source, sc.chunk.content, sc.score))
                .collect()
        } else {
            Vec::new()
        };

        // ── Search corpus-engine indexes ─────────────────────
        //
        // Catalog corpora hold one chunk per work (metadata only)
        // and pair with an on-demand content recipe. We partition
        // hits by `IndexInfo.kind`: full-text hits flow into the
        // ranked result list as before; catalog hits are surfaced
        // as a separate "I know of these — want me to read one?"
        // section so the runtime never confabulates plot details
        // from metadata.
        let (corpus_chunks, catalog_hits): (
            Vec<(String, String, f32)>,
            Vec<CatalogHit>,
        ) = if let (Some(ref engine), Some(ref emb)) =
            (&self.corpus_engine, &embedding)
        {
            let mut full_text: Vec<(String, String, f32)> = Vec::new();
            let mut catalog: Vec<CatalogHit> = Vec::new();

            let indexes = engine.installed_indexes().await.unwrap_or_default();
            // Build the `corpus_id → CorpusKind` map once.
            let mut kinds: HashMap<String, CorpusKind> = HashMap::new();
            for info in &indexes {
                kinds.insert(info.corpus_id.clone(), info.kind);
            }
            // Resolve each catalog corpus's `[catalog]` block
            // through the engine's recipe registry. Best-effort —
            // a missing CatalogConfig drops the hit back into the
            // full-text stream rather than dropping it outright
            // (see `partition_hits_by_kind`).
            let mut catalog_configs: HashMap<String, CatalogConfig> = HashMap::new();
            for info in &indexes {
                if info.kind == CorpusKind::Catalog {
                    if let Ok(recipe) =
                        engine.registry().fetch_recipe(&info.corpus_id).await
                    {
                        if let Some(cat) = recipe.catalog {
                            catalog_configs.insert(info.corpus_id.clone(), cat);
                        }
                    }
                }
            }
            let ctx = CatalogResolutionContext::from_indexes(
                &indexes,
                catalog_configs,
            );

            for info in &indexes {
                let idx = match engine.open_index(&info.path).await {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                let scored = match idx.search(emb, query, 5).await {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let (ft, cat) = partition_hits_by_kind(scored, &kinds, &ctx);
                for sc in ft {
                    let source = sc
                        .title
                        .clone()
                        .unwrap_or_else(|| sc.corpus_id.clone());
                    full_text.push((source, sc.content, sc.score));
                }
                catalog.extend(cat);
            }
            (full_text, catalog)
        } else {
            (Vec::new(), Vec::new())
        };

        // ── Merge, sort by score, truncate ───────────────────
        let mut all: Vec<(String, String, f32)> = store_chunks;
        all.extend(corpus_chunks);

        if all.is_empty() && catalog_hits.is_empty() {
            // Fall back to FTS message search.
            let messages = self.store.search_messages(query).await.unwrap_or_default();
            if messages.is_empty() {
                return Ok(StepOutput::Text(
                    "No relevant documents or messages found.".to_string(),
                ));
            }
            let results: Vec<String> = messages
                .iter()
                .take(5)
                .map(|m| format!("[{}] {}", m.role_str(), &m.content[..m.content.len().min(500)]))
                .collect();
            return Ok(StepOutput::Text(results.join("\n\n")));
        }

        // Sort descending by score.
        all.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(10);

        // Format full-text results.
        let mut sections: Vec<String> = Vec::new();
        if !all.is_empty() {
            let full_text_block: Vec<String> = all
                .iter()
                .map(|(source, content, _score)| {
                    format!("[{}] {}", source, &content[..content.len().min(500)])
                })
                .collect();
            sections.push(full_text_block.join("\n\n"));
        }

        if !catalog_hits.is_empty() {
            sections.push(format_catalog_hits(&catalog_hits));
        }

        Ok(StepOutput::Text(sections.join("\n\n")))
    }
}

/// Format a flat list of catalog hits as a "we know of these but
/// haven't read them" section. The runtime's synthesis prompt
/// (see `runtime.rs::KNOWLEDGE_SYNTHESIS_SYSTEM`) carries the
/// matching guidance — invent nothing beyond the metadata, end
/// with an explicit ingest offer.
fn format_catalog_hits(hits: &[CatalogHit]) -> String {
    let mut out = String::from(
        "CATALOG-AWARE SOURCES (metadata only, full text not yet ingested):",
    );
    for (i, h) in hits.iter().take(5).enumerate() {
        let mut line = format!("\n  [C{}] {}", i + 1, h.title);
        if let Some(a) = &h.authors {
            line.push_str(&format!(" — {a}"));
        }
        if let Some(y) = &h.year {
            line.push_str(&format!(" ({y})"));
        }
        if let Some(s) = &h.subjects {
            line.push_str(&format!(". Subjects: {s}"));
        }
        if let Some(corpus_id) = &h.already_ingested_corpus_id {
            line.push_str(&format!(
                ". ALREADY INGESTED → {corpus_id} (search the per-work corpus directly)"
            ));
        } else if let Some(mins) = h.estimated_ingest_minutes {
            line.push_str(&format!(". Ingest estimate: ~{mins} min"));
        }
        out.push_str(&line);
    }
    out
}
