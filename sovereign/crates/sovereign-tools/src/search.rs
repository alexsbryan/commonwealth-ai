use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::*;

use crate::web::search::{SearchBackend, SearchOrchestrator};
use crate::web::WebSearchTool;

// ─── Thresholds ──────────────────────────────────────────────

const SCORE_SUFFICIENT: f32 = 0.85;
const SCORE_LOW: f32 = 0.3;
const LOCAL_RESULT_LIMIT: usize = 10;
const SYNTHESIS_SOURCE_LIMIT: usize = 5;
const WEB_EXTRACT_LIMIT: usize = 3;

// ─── SearchTool ──────────────────────────────────────────────

pub struct SearchTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    web: Option<WebSearchTool>,
}

impl SearchTool {
    /// Create a local-only search tool (no web fallback).
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            store,
            inference,
            web: None,
        }
    }

    /// Create a search tool with web fallback (legacy direct-
    /// backend constructor). Kept for the eight existing call
    /// sites; new code should prefer `with_orchestrator()`.
    pub fn with_web(
        store: Arc<dyn StateStore>,
        inference: Arc<dyn InferenceProvider>,
        backend: SearchBackend,
    ) -> Self {
        Self {
            web: Some(WebSearchTool::with_backend(
                Arc::clone(&inference),
                backend,
            )),
            store,
            inference,
        }
    }

    /// Create a search tool with web fallback routed through the
    /// trait-based orchestrator. Per the Phase 6 migration in
    /// `sovereign/docs/PRODUCTION_SEARCH_INTEGRATION.md`, production
    /// callers should reach for this constructor to pick up the
    /// orchestrator's privacy + budget + fallback chain.
    pub fn with_orchestrator(
        store: Arc<dyn StateStore>,
        inference: Arc<dyn InferenceProvider>,
        orchestrator: Arc<SearchOrchestrator>,
    ) -> Self {
        Self {
            web: Some(WebSearchTool::with_orchestrator(
                Arc::clone(&inference),
                orchestrator,
            )),
            store,
            inference,
        }
    }
}

/// Canonical model-facing description of the `search` tool. Loaded
/// from an asset file (data, not program — per ARCH_PRINCIPLES §6.2)
/// so the gym harness can pin alignment via the same file. Edit the
/// .md file when you want to change what the model sees; running
/// the search-gym after any edit catches regressions in tool-call
/// judiciousness or citation faithfulness before production users
/// see drift.
pub const SEARCH_TOOL_DESCRIPTION: &str =
    include_str!("../assets/search_tool_description.md");

/// Canonical system prompt for chats where the search tool is
/// enabled. Mirrors SEARCH_TOOL_DESCRIPTION's rules but framed as a
/// direct instruction to the model rather than a tool description.
/// Models anchor more heavily on the system message than on tool
/// metadata, so the same shape rules need to appear in both — kept
/// in lockstep via the gym's alignment test.
pub const SEARCH_SYSTEM_PROMPT: &str =
    include_str!("../assets/search_system_prompt.md");

#[async_trait]
impl Tool for SearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "search".to_string(),
            name: "Search".to_string(),
            description: SEARCH_TOOL_DESCRIPTION.trim().to_string(),
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
            latency: Latency::Slow,
            scope: Scope::External,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Synthesised answer with inline citation markers \
                                (`[1]`, `[2]`, ...) and a trailing `Sources:` list \
                                tying markers to URLs. Output is prose, not JSON."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn retry_config(&self) -> Option<RetryConfig> {
        Some(RetryConfig {
            max_retries: 3,
            backoff_ms: vec![1000, 3000, 10000],
        })
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Search requires a 'query' string parameter".to_string(),
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

        // Stage 1: Local corpus search.
        let scored = self.local_search(query).await;

        // Stage 2: Coverage assessment.
        let decision = assess_coverage(query, &scored);

        // Stage 3: Web fallback (if needed).
        let web_sources = match &decision {
            CoverageDecision::SupplementWithWeb { .. }
            | CoverageDecision::RequiresWeb { .. } => {
                self.web_search(query).await
            }
            CoverageDecision::Sufficient => Vec::new(),
        };

        // Stage 4: Determine search method used.
        let method = determine_method(&decision, &scored, &web_sources);

        // Stage 5: Synthesize answer.
        let answer = self
            .synthesize(query, &scored, &web_sources, &method)
            .await?;

        // Build structured provenance for upstream consumers.
        let mut source_map: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for sc in &scored {
            let origin = match &sc.chunk.source_type {
                SourceType::Corpus { corpus_id } => corpus_id.clone(),
                SourceType::WebSearch { .. } => "web".to_string(),
                SourceType::UserDocument => "user_document".to_string(),
            };
            *source_map.entry(origin).or_insert(0) += 1;
        }
        for _ in &web_sources {
            *source_map.entry("web".to_string()).or_insert(0) += 1;
        }

        let search_provenance = serde_json::json!({
            "answer": answer,
            "search_method": format!("{method:?}"),
            "sources": source_map.iter()
                .map(|(k, v)| serde_json::json!({"origin": k, "count": v}))
                .collect::<Vec<_>>(),
            "local_results": scored.len(),
            "web_results": web_sources.len(),
        });

        Ok(StepOutput::Json(search_provenance))
    }
}

impl SearchTool {
    async fn local_search(&self, query: &str) -> Vec<ScoredChunk> {
        let embedding = self.inference.embed(query).await.ok();
        let emb_slice = embedding.as_deref().unwrap_or(&[]);
        self.store
            .search_documents_scored(emb_slice, query, LOCAL_RESULT_LIMIT)
            .await
            .unwrap_or_default()
    }

    async fn web_search(&self, query: &str) -> Vec<(String, String, String)> {
        let web = match &self.web {
            Some(w) => w,
            None => return Vec::new(),
        };

        // Check budget.
        if !self.check_budget().await {
            return Vec::new();
        }

        let search_queries = web.to_search_queries(query).await;
        let results = web.execute_searches(&search_queries).await;
        if results.is_empty() {
            return Vec::new();
        }

        // Decrement budget.
        self.decrement_budget().await;

        web.extract_content(&results, WEB_EXTRACT_LIMIT).await
    }

    async fn check_budget(&self) -> bool {
        // If no budget record exists, web search is allowed (no limit configured).
        let budget = match self.store.get_search_budget("web").await {
            Ok(Some(b)) => b,
            _ => return true,
        };
        let now = now();
        // Reset if past reset date.
        if now > budget.reset_date {
            return true;
        }
        budget.used_this_month < budget.monthly_limit
    }

    async fn decrement_budget(&self) {
        if let Ok(Some(mut budget)) = self.store.get_search_budget("web").await {
            let now = now();
            if now > budget.reset_date {
                budget.used_this_month = 1;
                // Set next reset to ~30 days from now.
                budget.reset_date = now + 30 * 86400;
            } else {
                budget.used_this_month += 1;
            }
            let _ = self.store.update_search_budget(&budget).await;
        }
    }

    async fn synthesize(
        &self,
        query: &str,
        local: &[ScoredChunk],
        web: &[(String, String, String)],
        method: &SearchMethod,
    ) -> Result<String> {
        let has_local = !local.is_empty();
        let has_web = !web.is_empty();

        if !has_local && !has_web {
            let note = match method {
                SearchMethod::NoResults { reason } => {
                    format!("No results found for \"{query}\". {reason}")
                }
                _ => format!("No results found for \"{query}\"."),
            };
            return Ok(note);
        }

        // Build numbered source list.
        let mut sources = Vec::new();
        let mut context_parts = Vec::new();
        let mut source_idx = 1;

        for sc in local.iter().take(SYNTHESIS_SOURCE_LIMIT) {
            let origin = source_origin(&sc.chunk);
            let label = format_origin(&origin);
            sources.push(format!("[{source_idx}] {label}"));
            let truncated = &sc.chunk.content[..sc.chunk.content.len().min(2000)];
            context_parts.push(format!("[Source {source_idx}: {label}]\n{truncated}"));
            source_idx += 1;
        }

        for (title, url, content) in web {
            sources.push(format!("[{source_idx}] {title} — {url}"));
            let truncated = &content[..content.len().min(2000)];
            context_parts.push(format!(
                "[Source {source_idx}: {title} ({url})]\n{truncated}"
            ));
            source_idx += 1;
        }

        let context = context_parts.join("\n\n---\n\n");

        let method_note = match method {
            SearchMethod::LocalOnly => "Answered from local knowledge bases.",
            SearchMethod::LocalPlusWeb { .. } => {
                "Answered from local knowledge bases, supplemented with web results."
            }
            SearchMethod::WebOnly { .. } => "Answered from web search results.",
            SearchMethod::LocalOnlyIncomplete { .. } => {
                "Answered from local knowledge bases (web search unavailable)."
            }
            SearchMethod::NoResults { .. } => "",
        };

        let request = CompletionRequest {
            prompt: format!(
                "Answer this question based on the search results below. \
                 Cite sources by number [1], [2], etc.\n\n\
                 Question: {query}\n\n\
                 Search Results:\n{context}"
            ),
            system_message: Some(
                "You are a research assistant. Answer based on the provided sources. \
                 Always cite your sources with [N] notation. Be comprehensive but concise."
                    .to_string(),
            ),
            preferred_speed: Speed::Slow,
            max_tokens: Some(1024),
            temperature: Some(0.3),
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
                    model_id: None,
                    enable_thinking: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        };

        let response = self.inference.complete(&request).await?;

        let source_list = sources.join("\n");
        let mut answer = format!("{}\n\nSources:\n{source_list}", response.text);
        if !method_note.is_empty() {
            answer.push_str(&format!("\n\n_{method_note}_"));
        }
        Ok(answer)
    }
}

// ─── Coverage Assessment ─────────────────────────────────────

fn assess_coverage(query: &str, results: &[ScoredChunk]) -> CoverageDecision {
    if results.is_empty() {
        return CoverageDecision::RequiresWeb {
            reason: "No local results found".to_string(),
        };
    }

    let top_score = results[0].score;

    if top_score >= SCORE_SUFFICIENT {
        return CoverageDecision::Sufficient;
    }

    if needs_current_info(query) {
        return CoverageDecision::SupplementWithWeb {
            reason: "Query may require current information".to_string(),
        };
    }

    if top_score < SCORE_LOW {
        return CoverageDecision::RequiresWeb {
            reason: "Low relevance scores from local search".to_string(),
        };
    }

    // Gray zone: some relevant results but not highly confident.
    // Default to supplementing with web if available.
    CoverageDecision::SupplementWithWeb {
        reason: format!("Moderate local coverage (top score: {top_score:.2})"),
    }
}

fn determine_method(
    decision: &CoverageDecision,
    local: &[ScoredChunk],
    web: &[(String, String, String)],
) -> SearchMethod {
    let has_local = !local.is_empty();
    let has_web = !web.is_empty();

    match (has_local, has_web) {
        (true, false) => match decision {
            CoverageDecision::Sufficient => SearchMethod::LocalOnly,
            _ => SearchMethod::LocalOnlyIncomplete {
                reason: "Web search unavailable or budget exhausted".to_string(),
            },
        },
        (true, true) => SearchMethod::LocalPlusWeb {
            reason: match decision {
                CoverageDecision::SupplementWithWeb { reason } => reason.clone(),
                CoverageDecision::RequiresWeb { reason } => reason.clone(),
                _ => "Supplemented with web results".to_string(),
            },
        },
        (false, true) => SearchMethod::WebOnly {
            reason: "No local results found".to_string(),
        },
        (false, false) => SearchMethod::NoResults {
            reason: "No results from local search or web".to_string(),
        },
    }
}

// ─── Helpers ─────────────────────────────────────────────────

fn source_origin(chunk: &DocumentChunk) -> SourceOrigin {
    match &chunk.source_type {
        SourceType::Corpus { corpus_id } => SourceOrigin::Local {
            corpus: corpus_id.clone(),
            article_title: chunk.source.clone(),
        },
        SourceType::WebSearch { url } => SourceOrigin::Web {
            url: url.clone(),
            domain: url
                .split('/')
                .nth(2)
                .unwrap_or("unknown")
                .to_string(),
        },
        SourceType::UserDocument => SourceOrigin::UserDocument {
            filename: chunk.source.clone(),
        },
    }
}

fn format_origin(origin: &SourceOrigin) -> String {
    match origin {
        SourceOrigin::Local {
            corpus,
            article_title,
        } => format!("{corpus}: {article_title}"),
        SourceOrigin::Web { url, .. } => url.clone(),
        SourceOrigin::UserDocument { filename } => filename.clone(),
    }
}

/// Heuristic: does this query likely need current/real-time information?
fn needs_current_info(message: &str) -> bool {
    let lower = message.to_lowercase();

    let has_recent_year = (2024..=2030).any(|y| lower.contains(&y.to_string()));

    let temporal_keywords = [
        "latest",
        "recent",
        "current",
        "today",
        "yesterday",
        "this week",
        "this month",
        "this year",
        "right now",
        "breaking",
        "news",
        "price",
        "stock",
        "weather",
        "who won",
        "election",
    ];
    let has_temporal = temporal_keywords.iter().any(|kw| lower.contains(kw));

    let search_keywords = ["search for", "look up", "find out", "google"];
    let has_search_request = search_keywords.iter().any(|kw| lower.contains(kw));

    has_recent_year || has_temporal || has_search_request
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assess_no_results() {
        let decision = assess_coverage("test query", &[]);
        assert!(matches!(decision, CoverageDecision::RequiresWeb { .. }));
    }

    #[test]
    fn assess_high_score_sufficient() {
        let results = vec![ScoredChunk {
            chunk: DocumentChunk {
                id: "test".to_string(),
                source: "wiki".to_string(),
                content: "answer".to_string(),
                chunk_index: 0,
                embedding: None,
                created_at: 0,
                source_type: SourceType::Corpus {
                    corpus_id: "wikipedia".to_string(),
                },
                version: 0,
                deleted_at: None,
            },
            score: 0.92,
        }];
        let decision = assess_coverage("What is Rust?", &results);
        assert!(matches!(decision, CoverageDecision::Sufficient));
    }

    #[test]
    fn assess_temporal_query_supplements() {
        let results = vec![ScoredChunk {
            chunk: DocumentChunk {
                id: "test".to_string(),
                source: "wiki".to_string(),
                content: "answer".to_string(),
                chunk_index: 0,
                embedding: None,
                created_at: 0,
                source_type: SourceType::UserDocument,
                version: 0,
                deleted_at: None,
            },
            score: 0.6,
        }];
        let decision = assess_coverage("What is the latest news?", &results);
        assert!(matches!(
            decision,
            CoverageDecision::SupplementWithWeb { .. }
        ));
    }

    #[test]
    fn assess_low_score_requires_web() {
        let results = vec![ScoredChunk {
            chunk: DocumentChunk {
                id: "test".to_string(),
                source: "doc".to_string(),
                content: "unrelated".to_string(),
                chunk_index: 0,
                embedding: None,
                created_at: 0,
                source_type: SourceType::UserDocument,
                version: 0,
                deleted_at: None,
            },
            score: 0.1,
        }];
        let decision = assess_coverage("quantum chromodynamics", &results);
        assert!(matches!(decision, CoverageDecision::RequiresWeb { .. }));
    }

    #[test]
    fn determine_method_local_only() {
        let local = vec![ScoredChunk {
            chunk: DocumentChunk {
                id: "t".to_string(),
                source: "s".to_string(),
                content: "c".to_string(),
                chunk_index: 0,
                embedding: None,
                created_at: 0,
                source_type: SourceType::UserDocument,
                version: 0,
                deleted_at: None,
            },
            score: 0.9,
        }];
        let method = determine_method(&CoverageDecision::Sufficient, &local, &[]);
        assert!(matches!(method, SearchMethod::LocalOnly));
    }

    #[test]
    fn determine_method_no_results() {
        let method = determine_method(
            &CoverageDecision::RequiresWeb {
                reason: "test".to_string(),
            },
            &[],
            &[],
        );
        assert!(matches!(method, SearchMethod::NoResults { .. }));
    }

    #[test]
    fn needs_current_info_works() {
        assert!(needs_current_info("What happened in 2025?"));
        assert!(needs_current_info("latest news about AI"));
        assert!(!needs_current_info("What is recursion?"));
        assert!(!needs_current_info("Explain photosynthesis"));
    }

    #[test]
    fn search_tool_description_is_loaded_from_asset() {
        // Pins the load-bearing prompt assertions we validated in
        // the search-gym fixtures (Phase 2). If anyone changes the
        // asset, this test still passes — but the search-gym's
        // alignment test (in sovereign-cli) will catch drift between
        // the asset and the fixtures' tool descriptions.
        let desc = SEARCH_TOOL_DESCRIPTION;
        // Cost-awareness — proven to reduce reflexive search in the
        // multi-corpus archetype fixtures (06–10).
        assert!(
            desc.contains("budget") || desc.contains("monthly"),
            "description should signal cost awareness"
        );
        // Shape-level guidance for "when to search" — validated
        // against fixtures 01/02 (should search) and 03 (should
        // skip).
        assert!(desc.contains("current") || desc.contains("changes"));
        assert!(desc.contains("stable") || desc.contains("definitions"));
        // Verbatim-URL guidance — the URL-fabrication failure mode
        // surfaced by fixture 02 isolated runs.
        assert!(
            desc.contains("verbatim") || desc.contains("exact"),
            "description should require verbatim URL copy"
        );
    }

    #[test]
    fn source_origin_from_corpus() {
        let chunk = DocumentChunk {
            id: "test".to_string(),
            source: "epistemology".to_string(),
            content: "content".to_string(),
            chunk_index: 0,
            embedding: None,
            created_at: 0,
            source_type: SourceType::Corpus {
                corpus_id: "sep".to_string(),
            },
            version: 0,
            deleted_at: None,
        };
        let origin = source_origin(&chunk);
        assert!(matches!(origin, SourceOrigin::Local { corpus, .. } if corpus == "sep"));
    }
}
