// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod extract;
pub mod search;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::{InferenceProvider, Tool};
use sovereign_contracts::types::*;

use self::extract::fetch_and_extract;
use self::search::{
    BudgetView, SearchBackend, SearchOrchestrator, SearchPrivacy, SearchResult, SelectInputs,
};

// ─── WebSearchTool ─────────────────────────────────────────────

/// Web search pipeline:
/// 1. Convert query to search keywords (Fast slot)
/// 2. Search execution → results
/// 3. Content extraction (top URLs) → clean text
/// 4. Synthesis (Primary slot) → cited answer
///
/// Two backend dispatch modes coexist for the Phase 0 → 6 migration
/// (see `sovereign/docs/PRODUCTION_SEARCH_INTEGRATION.md`):
///
/// - **Legacy**: a single concrete `SearchBackend` enum value. Set by
///   `new()` and `with_backend()`. The original API; eight call sites
///   still use it.
/// - **Orchestrated**: an `Arc<SearchOrchestrator>` that picks a
///   backend per call from a registry, filtering by privacy and
///   budget. Set by `with_orchestrator()`. New code (desktop's Phase 6
///   migration, future integrations) should reach for this path.
///
/// When both are somehow set (e.g. a future builder bug), the
/// orchestrator wins — it carries more invariants.
pub struct WebSearchTool {
    inference: Arc<dyn InferenceProvider>,
    client: reqwest::Client,
    /// Legacy direct-backend dispatch. None when the orchestrator
    /// path is in use.
    backend: Option<SearchBackend>,
    /// Trait+registry path. When `Some`, supersedes `backend`.
    orchestrator: Option<Arc<SearchOrchestrator>>,
}

impl WebSearchTool {
    /// Create with DuckDuckGo (free, zero-config default). Legacy
    /// path — new callers should prefer
    /// `with_orchestrator()`.
    pub fn new(inference: Arc<dyn InferenceProvider>) -> Self {
        Self::with_backend(inference, SearchBackend::DuckDuckGo)
    }

    /// Create with a specific search backend. Legacy path; kept for
    /// back-compat with the eight existing call sites that pass a
    /// `SearchBackend` enum value.
    pub fn with_backend(inference: Arc<dyn InferenceProvider>, backend: SearchBackend) -> Self {
        Self {
            inference,
            client: default_client(),
            backend: Some(backend),
            orchestrator: None,
        }
    }

    /// Create with the trait-based orchestrator. The orchestrator
    /// holds the registry and applies privacy + budget filtering on
    /// every selection. Per the Phase 6 migration, this is the
    /// constructor production code should reach for.
    pub fn with_orchestrator(
        inference: Arc<dyn InferenceProvider>,
        orchestrator: Arc<SearchOrchestrator>,
    ) -> Self {
        Self {
            inference,
            client: default_client(),
            backend: None,
            orchestrator: Some(orchestrator),
        }
    }

    /// Convert a natural language question into effective search keywords.
    /// A simple LLM call that strips conversational fluff and produces
    /// 1-2 clean keyword queries (not sub-queries that reference each other).
    pub(crate) async fn to_search_queries(&self, query: &str) -> Vec<String> {
        let request = CompletionRequest {
            prompt: format!(
                "Convert this into 1-2 concise search engine queries (keywords only, no full sentences). \
                 Each query must be independent and self-contained.\n\n\
                 Input: \"{query}\"\n\n\
                 Output one query per line, nothing else."
            ),
            system_message: Some(
                "You convert questions into short search engine queries. \
                 Output keywords only, one query per line."
                    .to_string(),
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(60),
            temperature: Some(0.0),
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
        evidence_id_allowlist: None,
        lark_grammar: None,
        prompt_shape: None,
        };

        match self.inference.complete(&request).await {
            Ok(response) => {
                let queries: Vec<String> = response
                    .text
                    .lines()
                    .map(|l| {
                        l.trim()
                            .trim_start_matches(|c: char| {
                                c == '-' || c == '*' || c.is_numeric() || c == '.' || c == ')'
                            })
                            .trim()
                            .to_string()
                    })
                    .filter(|l| !l.is_empty() && l.len() > 3 && l.len() < 200)
                    .take(2)
                    .collect();

                if queries.is_empty() {
                    vec![query.to_string()]
                } else {
                    queries
                }
            }
            Err(_) => vec![query.to_string()],
        }
    }

    /// Execute searches and collect results. Routes through
    /// `do_one_search` so both legacy and orchestrated paths share
    /// the dedup + error-handling envelope.
    pub(crate) async fn execute_searches(&self, queries: &[String]) -> Vec<SearchResult> {
        let mut all_results = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        for query in queries {
            let results = self.do_one_search(query, 5).await;
            for r in results {
                if seen_urls.insert(r.url.clone()) {
                    all_results.push(r);
                }
            }
        }

        all_results
    }

    /// Single-query dispatch. Orchestrator wins when present (it's
    /// the future-state path); legacy backend is the back-compat
    /// fallback. When neither is set (shouldn't happen — both
    /// constructors set one) returns empty.
    async fn do_one_search(&self, query: &str, max_results: usize) -> Vec<SearchResult> {
        if let Some(orch) = &self.orchestrator {
            let budget = BudgetView::new(); // Phase 6.5: thread real budget through
            let out = orch
                .search(
                    &self.client,
                    SelectInputs {
                        query,
                        max_results,
                        // Default to External — the orchestrator will
                        // narrow further based on its registry's
                        // privacy postures. Future: receive the
                        // request's OICP privacy and tighten this.
                        max_privacy: SearchPrivacy::External { provider: "any" },
                        budget: &budget,
                        // Empty prefer: registry order. Operator
                        // override comes through BackendsConfig in a
                        // follow-up wiring step (currently the
                        // orchestrator carries no operator config).
                        prefer: &[],
                    },
                )
                .await;
            return out.results;
        }
        if let Some(backend) = &self.backend {
            match search::search(&self.client, backend, query, max_results).await {
                Ok(results) => return results,
                Err(e) => {
                    eprintln!("  [web] Search failed for \"{query}\": {e}");
                    return Vec::new();
                }
            }
        }
        eprintln!(
            "  [web] WebSearchTool has neither backend nor orchestrator — \
             returning empty results"
        );
        Vec::new()
    }

    /// Fetch and extract content from top URLs.
    pub(crate) async fn extract_content(
        &self,
        results: &[SearchResult],
        max_pages: usize,
    ) -> Vec<(String, String, String)> {
        let mut extracted = Vec::new();

        for result in results.iter().take(max_pages) {
            match fetch_and_extract(&self.client, &result.url).await {
                Ok(text) => {
                    if text.len() > 100 {
                        extracted.push((result.title.clone(), result.url.clone(), text));
                    }
                }
                Err(_) => {
                    // Use snippet as fallback.
                    if !result.snippet.is_empty() {
                        extracted.push((
                            result.title.clone(),
                            result.url.clone(),
                            result.snippet.clone(),
                        ));
                    }
                }
            }
        }

        extracted
    }

    /// Synthesize a cited answer from extracted content.
    pub(crate) async fn synthesize(
        &self,
        query: &str,
        sources: &[(String, String, String)],
    ) -> Result<String> {
        if sources.is_empty() {
            return Ok(format!(
                "I searched the web for \"{query}\" but couldn't extract content from the results."
            ));
        }

        let context: String = sources
            .iter()
            .enumerate()
            .map(|(i, (title, url, content))| {
                let truncated = &content[..content.len().min(2000)];
                format!("[Source {}: {} ({})]\n{truncated}", i + 1, title, url)
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let request = CompletionRequest {
            prompt: format!(
                "Answer this question based on the web search results below. \
                 Cite sources by number [1], [2], etc.\n\n\
                 Question: {query}\n\n\
                 Search Results:\n{context}"
            ),
            system_message: Some(
                "You are a research assistant. Answer based on the provided sources. \
                 Always cite your sources with [N] notation. If sources conflict, present both views."
                    .to_string(),
            ),
            preferred_speed: Speed::Slow,
            max_tokens: Some(1024),
            temperature: Some(0.5),
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
        evidence_id_allowlist: None,
        lark_grammar: None,
        prompt_shape: None,
        };

        let response = self.inference.complete(&request).await?;

        // Append source list.
        let source_list: String = sources
            .iter()
            .enumerate()
            .map(|(i, (title, url, _))| format!("[{}] {} — {}", i + 1, title, url))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(format!("{}\n\nSources:\n{source_list}", response.text))
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "web_search".to_string(),
            name: "Web Search".to_string(),
            description: "Search the web and synthesize an answer with citations".to_string(),
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
                "description": "Synthesized answer text with inline citation markers \
                                and a Sources list."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Network]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Web search requires a 'query' string parameter".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'query' parameter".to_string()))?;

        eprintln!("[web] Searching: \"{query}\"");

        // 1. Convert to search keywords.
        let search_queries = self.to_search_queries(query).await;
        eprintln!("[web] Search queries: {}", search_queries.join(" | "));

        // 2. Execute searches.
        let results = self.execute_searches(&search_queries).await;
        eprintln!("[web] Found {} results", results.len());

        if results.is_empty() {
            // Last resort: try searching with the raw query directly.
            // Routes through `do_one_search` so the orchestrator
            // (when configured) gets the same fallback chain.
            eprintln!("[web] Retrying with raw query");
            let raw_results = self.do_one_search(query, 5).await;
            eprintln!("[web] Raw query found {} results", raw_results.len());

            if raw_results.is_empty() {
                return Ok(StepOutput::Text(format!(
                    "Web search returned no results for \"{query}\". \
                     The search provider may be temporarily unavailable. \
                     Try again or configure a different search backend (Brave or Tavily) in Settings."
                )));
            }

            let extracted = self.extract_content(&raw_results, 4).await;
            let answer = self.synthesize(query, &extracted).await?;
            return Ok(StepOutput::Text(answer));
        }

        // 3. Extract content from top pages.
        let extracted = self.extract_content(&results, 4).await;
        eprintln!("[web] Extracted content from {} pages", extracted.len());

        // 4. Synthesize answer.
        let answer = self.synthesize(query, &extracted).await?;

        Ok(StepOutput::Text(answer))
    }
}

// ─── WebFetchTool ──────────────────────────────────────────────

/// Fetch a single URL and extract its text content.
pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_default();

        Self { client }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "web_fetch".to_string(),
            name: "Web Fetch".to_string(),
            description: "Fetch a URL and extract its text content".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    }
                },
                "required": ["url"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::External,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Plain-text body of the fetched URL, HTML-stripped \
                                to main content."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Network]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let url = params.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::InvalidInput("Web fetch requires a 'url' parameter".to_string())
        })?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::InvalidInput(
                "URL must start with http:// or https://".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'url' parameter".to_string()))?;

        let text = fetch_and_extract(&self.client, url).await?;
        Ok(StepOutput::Text(text))
    }
}

/// Build the reqwest client both WebSearchTool constructors share.
/// 15s timeout matches the per-call budget the search backends
/// (DuckDuckGo's two endpoints, Tavily, Brave) expect; redirect
/// limit of 5 catches typical 30x chains without letting a
/// redirect loop hang the request.
fn default_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap_or_default()
}
