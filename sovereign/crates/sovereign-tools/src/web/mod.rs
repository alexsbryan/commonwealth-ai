pub mod extract;
pub mod search;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::*;

use self::extract::fetch_and_extract;
use self::search::{SearchBackend, SearchResult};

// ─── WebSearchTool ─────────────────────────────────────────────

/// Multi-stage web search pipeline:
/// 1. Query decomposition (Fast slot) → 2-3 sub-queries
/// 2. Search execution (DuckDuckGo) → results per sub-query
/// 3. Content extraction (top URLs) → clean text
/// 4. Synthesis (Primary slot) → cited answer
pub struct WebSearchTool {
    inference: Arc<dyn InferenceProvider>,
    client: reqwest::Client,
    backend: SearchBackend,
}

impl WebSearchTool {
    /// Create with DuckDuckGo (free, zero-config default).
    pub fn new(inference: Arc<dyn InferenceProvider>) -> Self {
        Self::with_backend(inference, SearchBackend::DuckDuckGo)
    }

    /// Create with a specific search backend.
    pub fn with_backend(
        inference: Arc<dyn InferenceProvider>,
        backend: SearchBackend,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_default();

        Self {
            inference,
            client,
            backend,
        }
    }

    /// Stage 1: Decompose the user query into focused sub-queries.
    async fn decompose_query(&self, query: &str) -> Result<Vec<String>> {
        let request = CompletionRequest {
            prompt: format!(
                "Break this search query into 2-3 focused sub-queries that would find the most relevant information.\n\n\
                 Query: \"{query}\"\n\n\
                 Return each sub-query on its own line. No numbering, no bullets, just the queries."
            ),
            system_message: Some(
                "You generate search queries. Output 2-3 search queries, one per line. Nothing else.".to_string()
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(100),
            temperature: Some(0.3),
            structured_output: None,
        };

        let response = self.inference.complete(&request).await?;

        let queries: Vec<String> = response
            .text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && l.len() > 3)
            .take(3)
            .collect();

        if queries.is_empty() {
            // Fallback to the original query.
            Ok(vec![query.to_string()])
        } else {
            Ok(queries)
        }
    }

    /// Stage 2: Execute searches and collect results.
    async fn execute_searches(&self, queries: &[String]) -> Vec<SearchResult> {
        let mut all_results = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        for query in queries {
            match search::search(&self.client, &self.backend, query, 5).await {
                Ok(results) => {
                    for r in results {
                        if seen_urls.insert(r.url.clone()) {
                            all_results.push(r);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  [web] Search failed for \"{query}\": {e}");
                }
            }
        }

        all_results
    }

    /// Stage 3: Fetch and extract content from top URLs.
    async fn extract_content(&self, results: &[SearchResult], max_pages: usize) -> Vec<(String, String, String)> {
        let mut extracted = Vec::new();

        for result in results.iter().take(max_pages) {
            match fetch_and_extract(&self.client, &result.url).await {
                Ok(text) => {
                    if text.len() > 100 {
                        // Only keep pages with meaningful content.
                        extracted.push((
                            result.title.clone(),
                            result.url.clone(),
                            text,
                        ));
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

    /// Stage 4: Synthesize a cited answer from extracted content.
    async fn synthesize(
        &self,
        query: &str,
        sources: &[(String, String, String)],
    ) -> Result<String> {
        if sources.is_empty() {
            return Ok(format!(
                "I searched the web for \"{query}\" but couldn't find relevant results."
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

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'query' parameter".to_string()))?;

        eprintln!("[web] Searching: \"{query}\"");

        // 1. Decompose query.
        let sub_queries = self.decompose_query(query).await?;
        eprintln!("[web] Sub-queries: {}", sub_queries.join(", "));

        // 2. Execute searches.
        let results = self.execute_searches(&sub_queries).await;
        eprintln!("[web] Found {} results", results.len());

        if results.is_empty() {
            return Ok(StepOutput::Text(format!(
                "No web search results found for \"{query}\"."
            )));
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
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Network]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Web fetch requires a 'url' parameter".to_string()))?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::InvalidInput(
                "URL must start with http:// or https://".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'url' parameter".to_string()))?;

        let text = fetch_and_extract(&self.client, url).await?;
        Ok(StepOutput::Text(text))
    }
}
