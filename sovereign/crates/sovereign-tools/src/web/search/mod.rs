// SPDX-License-Identifier: AGPL-3.0-or-later
//! Web-search dispatch — both the legacy enum surface (`SearchBackend`,
//! `search()` — the eight existing call sites use these) and the new
//! trait-based abstraction (`backend_trait::WebSearchBackend`,
//! `WebSearchRegistry`) that Phase 2's orchestrator will consume.
//!
//! See `sovereign/docs/PRODUCTION_SEARCH_INTEGRATION.md` for the
//! full migration plan. Phase 0 ships the trait + registry alongside
//! the enum (additive — nothing breaks); subsequent phases migrate
//! call sites and retire the legacy surface.

pub mod assets;
pub mod backend_trait;
pub mod orchestrator;

pub use assets::{
    BackendsConfig, BudgetConfig, BudgetEntry, PrivacyConfig, SelectionConfig,
    DEFAULT_BACKENDS_TOML, SYSTEM_PROMPT, TOOL_DESCRIPTION,
};
pub use backend_trait::{
    BraveBackendImpl, DuckDuckGoBackendImpl, MockBackendImpl, SearchCost, SearchPrivacy,
    TavilyBackendImpl, WebSearchBackend, WebSearchRegistry,
};
pub use orchestrator::{BudgetView, OrchestratedSearch, SearchOrchestrator, SelectInputs};

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use sovereign_core::error::{Error, Result};

/// A search result from any backend.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Which search backend to use.
#[derive(Debug, Clone)]
pub enum SearchBackend {
    /// Free, zero-config. HTML scraping of DuckDuckGo.
    DuckDuckGo,

    /// Independent index, $5/1k queries. API key required.
    Brave { api_key: String },

    /// AI-native search, pre-extracted content. API key required.
    /// 1000 free queries/month.
    Tavily { api_key: String },

    /// Deterministic fixture-replay backend for the search-gym harness.
    /// Resolves a query to a pre-recorded response file under
    /// `<corpus_path>/<sha256(normalized_query)>.json`. A missing file
    /// is a loud error — the mock never silently falls through to a
    /// live provider. Production code paths cannot construct this
    /// variant without an on-disk corpus path, which they never have.
    Mock { corpus_path: PathBuf },
}

impl SearchBackend {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Brave { .. } => "Brave",
            Self::Tavily { .. } => "Tavily",
            Self::Mock { .. } => "Mock",
        }
    }
}

/// Execute a search using the configured backend.
pub async fn search(
    client: &reqwest::Client,
    backend: &SearchBackend,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    match backend {
        SearchBackend::DuckDuckGo => search_duckduckgo(client, query, max_results).await,
        SearchBackend::Brave { api_key } => search_brave(client, api_key, query, max_results).await,
        SearchBackend::Tavily { api_key } => {
            search_tavily(client, api_key, query, max_results).await
        }
        SearchBackend::Mock { corpus_path } => search_mock(corpus_path, query, max_results),
    }
}

// ─── Mock backend (gym fixtures) ───────────────────────────────

/// Normalize a query for fixture-resolution: lowercase, trim, collapse
/// internal whitespace. Punctuation is preserved — gym authors who
/// want trailing-`?` insensitivity should strip in the recipe.
fn normalize_query(query: &str) -> String {
    let lowered = query.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut prev_was_space = false;
    for ch in lowered.trim().chars() {
        if ch.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(ch);
            prev_was_space = false;
        }
    }
    out
}

/// SHA-256 of the normalized query, hex-encoded. Public so the gym
/// runner can compute fixture paths without re-implementing the
/// hashing rule.
pub fn mock_fixture_hash(query: &str) -> String {
    let normalized = normalize_query(query);
    let digest = Sha256::digest(normalized.as_bytes());
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[derive(Debug, Deserialize)]
struct MockResponse {
    /// Human-readable echo of the query this fixture was recorded for.
    /// Not used for matching — present so `grep` over the corpus is
    /// useful. The match is purely hash-based.
    #[serde(default)]
    #[allow(dead_code)]
    query: String,
    results: Vec<SearchResult>,
}

/// An `aliases.toml` entry. Lets fixture authors give files
/// human-readable names and bind multiple query phrasings to one
/// response — which beats hash-mining every variant the model might
/// emit. Schema is intentionally tiny so a typo'd field fails loudly
/// instead of being silently ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MockAliasEntry {
    file: String,
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MockAliasIndex {
    #[serde(default, rename = "entry")]
    entries: Vec<MockAliasEntry>,
}

/// Try to resolve `normalized_query` to a fixture file via the
/// `aliases.toml` index. Returns:
///   - `Ok(Some(filename))` on alias hit
///   - `Ok(None)` if the index doesn't exist OR no alias matches
///   - `Err(_)` if the index exists but is malformed (fail loud)
///
/// Per-call file read is fine: aliases.toml is small (~KB) and the
/// gym's search rate is bounded by replay count, not throughput.
fn lookup_alias(corpus_path: &std::path::Path, normalized_query: &str) -> Result<Option<String>> {
    let index_path = corpus_path.join("aliases.toml");
    let body = match std::fs::read_to_string(&index_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Execution(format!(
                "mock aliases index unreadable: file={} ({e})",
                index_path.display()
            )))
        }
    };
    let index: MockAliasIndex = toml::from_str(&body).map_err(|e| {
        Error::Execution(format!(
            "mock aliases index malformed: file={} ({e})",
            index_path.display()
        ))
    })?;

    // Substring containment, not exact-equal. The model routinely
    // adds qualifying suffixes ("...today", "...announcement",
    // "...success status") that strict equality would miss. Aliases
    // function as canonical topic phrasings; a query is considered a
    // match if any alias appears as a contiguous normalized substring
    // of the query. First-match-wins, so author-side specificity
    // controls ambiguity (don't list "stock" as an alias for one
    // company; do list "nvda stock price").
    for entry in &index.entries {
        if entry.aliases.iter().any(|a| {
            let a_norm = normalize_query(a);
            !a_norm.is_empty() && normalized_query.contains(&a_norm)
        }) {
            return Ok(Some(entry.file.clone()));
        }
    }
    Ok(None)
}

fn search_mock(
    corpus_path: &std::path::Path,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    let normalized = normalize_query(query);

    // Two-tier lookup: aliases.toml first (human-readable filenames,
    // multiple phrasings per file), then hash fallback for fixtures
    // authored before aliases or for one-off responses.
    let (path, lookup_mode) = match lookup_alias(corpus_path, &normalized)? {
        Some(filename) => (corpus_path.join(&filename), "alias"),
        None => {
            let hash = mock_fixture_hash(query);
            (corpus_path.join(format!("{hash}.json")), "hash")
        }
    };

    let body = std::fs::read_to_string(&path).map_err(|e| {
        // The error message names the mode that failed so the fixture
        // author knows whether they need to add an alias or record a
        // new file. Keep the query echo for grep-ability.
        Error::Execution(format!(
            "mock search fixture missing ({lookup_mode}): query={query:?} \
             expected_file={} ({e})",
            path.display()
        ))
    })?;

    let parsed: MockResponse = serde_json::from_str(&body).map_err(|e| {
        Error::Execution(format!(
            "mock search fixture malformed: file={} ({e})",
            path.display()
        ))
    })?;

    tracing::debug!(
        mode = %lookup_mode,
        file = %path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        results = parsed.results.len(),
        "web_search: mock cache_hit=true"
    );

    let mut results = parsed.results;
    results.truncate(max_results);
    Ok(results)
}

// ─── DuckDuckGo (free, zero-config) ───────────────────────────

const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

async fn search_duckduckgo(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    // DDG HTML endpoint — use POST like the actual form does.
    let response = client
        .post("https://html.duckduckgo.com/html/")
        .header("User-Agent", BROWSER_UA)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Origin", "https://html.duckduckgo.com")
        .header("Referer", "https://html.duckduckgo.com/")
        .body(format!("q={}&b=", urlencoded(query)))
        .send()
        .await
        .map_err(|e| Error::Execution(format!("DuckDuckGo search failed: {e}")))?;

    let status = response.status();
    let html = response
        .text()
        .await
        .map_err(|e| Error::Execution(format!("Failed to read DDG response: {e}")))?;

    eprintln!(
        "[web] DDG HTML response: status={}, body_len={}, has_result_a={}",
        status,
        html.len(),
        html.contains("result__a")
    );

    // If DDG returns a bot-detection page (status 202 or no results markers),
    // skip straight to the fallback API approach.
    if status.as_u16() == 202
        || (!html.contains("result__a") && !html.contains("result__url") && html.len() < 20000)
    {
        eprintln!("[web] DDG appears to be blocking automated requests, using API fallback");
        return search_duckduckgo_api(client, query, max_results).await;
    }

    let results = parse_ddg_results(&html, max_results);

    if !results.is_empty() {
        return Ok(results);
    }

    // Fallback: try the DuckDuckGo Lite endpoint (POST, different HTML structure).
    eprintln!("[web] DDG HTML returned no results, trying Lite endpoint");

    let lite_response = client
        .post("https://lite.duckduckgo.com/lite/")
        .header("User-Agent", BROWSER_UA)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Origin", "https://lite.duckduckgo.com")
        .header("Referer", "https://lite.duckduckgo.com/")
        .body(format!("q={}", urlencoded(query)))
        .send()
        .await
        .map_err(|e| Error::Execution(format!("DuckDuckGo Lite search failed: {e}")))?;

    let lite_status = lite_response.status();
    let lite_html = lite_response
        .text()
        .await
        .map_err(|e| Error::Execution(format!("Failed to read DDG Lite response: {e}")))?;

    eprintln!(
        "[web] DDG Lite response: status={}, body_len={}, has_result_link={}",
        lite_status,
        lite_html.len(),
        lite_html.contains("result-link")
    );

    let lite_results = parse_ddg_lite_results(&lite_html, max_results);
    Ok(lite_results)
}

fn parse_ddg_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    // DDG HTML routinely lists the same canonical URL twice (zero-click
    // info box + organic row). Dedup at the parser so callers don't see
    // duplicates leak into the URL allowlist or waste model context.
    let mut seen: HashSet<String> = HashSet::new();
    let mut pos = 0;

    while results.len() < max_results {
        let link_marker = "class=\"result__a\"";
        let link_start = match html[pos..].find(link_marker) {
            Some(i) => pos + i,
            None => break,
        };

        let href_start = match html[..link_start].rfind("href=\"") {
            Some(i) => i + 6,
            None => {
                pos = link_start + link_marker.len();
                continue;
            }
        };
        let href_end = match html[href_start..].find('"') {
            Some(i) => href_start + i,
            None => {
                pos = link_start + link_marker.len();
                continue;
            }
        };
        let raw_url = &html[href_start..href_end];
        let url = extract_ddg_url(raw_url);

        let title_start = match html[link_start..].find('>') {
            Some(i) => link_start + i + 1,
            None => {
                pos = link_start + link_marker.len();
                continue;
            }
        };
        let title_end = match html[title_start..].find("</a>") {
            Some(i) => title_start + i,
            None => {
                pos = link_start + link_marker.len();
                continue;
            }
        };
        let title = strip_html_tags(&html[title_start..title_end]);

        let snippet_marker = "class=\"result__snippet\"";
        let snippet = if let Some(snippet_start) = html[title_end..].find(snippet_marker) {
            let snippet_abs = title_end + snippet_start;
            if let Some(tag_end) = html[snippet_abs..].find('>') {
                let text_start = snippet_abs + tag_end + 1;
                if let Some(text_end) = html[text_start..].find("</") {
                    strip_html_tags(&html[text_start..text_start + text_end])
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !url.is_empty() && !title.is_empty() && seen.insert(url.clone()) {
            results.push(SearchResult {
                title: html_decode(&title),
                url,
                snippet: html_decode(&snippet),
            });
        }

        pos = title_end;
    }

    results
}

/// Parse results from DuckDuckGo Lite (table-based layout).
/// Each result is in a table row with class "result-link" for the link
/// and class "result-snippet" for the snippet text.
fn parse_ddg_lite_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pos = 0;

    // DDG Lite wraps each result link in: <a rel="nofollow" href="URL" class="result-link">TITLE</a>
    while results.len() < max_results {
        let marker = "class=\"result-link\"";
        let marker_pos = match html[pos..].find(marker) {
            Some(i) => pos + i,
            None => break,
        };

        // Find the href before the marker.
        let search_region_start = marker_pos.saturating_sub(200);
        let href_start = match html[search_region_start..marker_pos].rfind("href=\"") {
            Some(i) => search_region_start + i + 6,
            None => {
                pos = marker_pos + marker.len();
                continue;
            }
        };
        let href_end = match html[href_start..marker_pos].find('"') {
            Some(i) => href_start + i,
            None => {
                pos = marker_pos + marker.len();
                continue;
            }
        };
        let raw_url = &html[href_start..href_end];
        let url = extract_ddg_url(raw_url);

        // Title is between > and </a>.
        let title_start = match html[marker_pos..].find('>') {
            Some(i) => marker_pos + i + 1,
            None => {
                pos = marker_pos + marker.len();
                continue;
            }
        };
        let title_end = match html[title_start..].find("</a>") {
            Some(i) => title_start + i,
            None => {
                pos = marker_pos + marker.len();
                continue;
            }
        };
        let title = strip_html_tags(&html[title_start..title_end]);

        // Snippet: look for class="result-snippet" after the title.
        let snippet_marker = "class=\"result-snippet\"";
        let snippet = if let Some(s_pos) = html[title_end..].find(snippet_marker) {
            let s_abs = title_end + s_pos;
            if let Some(tag_end) = html[s_abs..].find('>') {
                let text_start = s_abs + tag_end + 1;
                if let Some(text_end) = html[text_start..].find("</td") {
                    strip_html_tags(&html[text_start..text_start + text_end])
                } else if let Some(text_end) = html[text_start..].find("</span") {
                    strip_html_tags(&html[text_start..text_start + text_end])
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !url.is_empty()
            && !title.is_empty()
            && url.starts_with("http")
            && seen.insert(url.clone())
        {
            results.push(SearchResult {
                title: html_decode(&title),
                url,
                snippet: html_decode(&snippet).trim().to_string(),
            });
        }

        pos = title_end;
    }

    results
}

/// Fallback: scrape Google search results when DDG blocks us.
async fn search_duckduckgo_api(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    // Use Google search as the actual fallback since DDG is blocking.
    let url = format!(
        "https://www.google.com/search?q={}&num={}&hl=en",
        urlencoded(query),
        max_results + 2,
    );

    let response = client
        .get(&url)
        .header("User-Agent", BROWSER_UA)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| Error::Execution(format!("Google search failed: {e}")))?;

    let html = response
        .text()
        .await
        .map_err(|e| Error::Execution(format!("Failed to read Google response: {e}")))?;

    let results = parse_google_results(&html, max_results);
    eprintln!("[web] Google fallback found {} results", results.len());
    Ok(results)
}

/// Parse results from Google search HTML.
/// Google wraps result links in <a href="/url?q=ACTUAL_URL&..."> tags
/// and titles in <h3> tags.
fn parse_google_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut pos = 0;

    while results.len() < max_results {
        // Find the next <a href="/url?q= which indicates a search result link.
        let marker = "/url?q=";
        let marker_pos = match html[pos..].find(marker) {
            Some(i) => pos + i,
            None => break,
        };

        // Extract the actual URL (up to & or ").
        let url_start = marker_pos + marker.len();
        let url_end = html[url_start..]
            .find(['&', '"'])
            .map(|i| url_start + i)
            .unwrap_or(url_start);

        let url = urldecoded(&html[url_start..url_end]);

        // Skip Google's own links and non-http URLs.
        if !url.starts_with("http") || url.contains("google.com") || url.contains("accounts.google")
        {
            pos = url_end;
            continue;
        }

        // Find an <h3> tag near this link for the title.
        let search_end = (marker_pos + 500).min(html.len());
        let search_start = marker_pos.saturating_sub(500);
        let region = &html[search_start..search_end];

        let title = if let Some(h3_start) = region.find("<h3") {
            let h3_region = &region[h3_start..];
            if let Some(tag_end) = h3_region.find('>') {
                let text_start = tag_end + 1;
                if let Some(text_end) = h3_region[text_start..].find("</h3>") {
                    strip_html_tags(&h3_region[text_start..text_start + text_end])
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Try to find a snippet (Google uses <span> blocks near the result).
        let snippet_region_end = (url_end + 2000).min(html.len());
        let snippet_region = &html[url_end..snippet_region_end];
        let snippet = extract_google_snippet(snippet_region);

        if !title.is_empty() {
            results.push(SearchResult {
                title: html_decode(&title),
                url,
                snippet: html_decode(&snippet),
            });
        }

        pos = url_end;
    }

    results
}

/// Extract a snippet from the HTML region after a Google result link.
fn extract_google_snippet(region: &str) -> String {
    // Google puts snippets in spans with various data- attributes.
    // Look for longer text content in <span> tags.
    let mut best = String::new();

    let mut search_pos = 0;
    while let Some(span_start) = region[search_pos..].find("<span") {
        let abs = search_pos + span_start;
        if let Some(tag_end) = region[abs..].find('>') {
            let text_start = abs + tag_end + 1;
            if let Some(text_end) = region[text_start..].find("</span>") {
                let text = strip_html_tags(&region[text_start..text_start + text_end]);
                let trimmed = text.trim();
                // Keep the longest span content as the snippet.
                if trimmed.len() > best.len() && trimmed.len() > 40 && trimmed.len() < 500 {
                    best = trimmed.to_string();
                }
                search_pos = text_start + text_end;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    best
}

fn extract_ddg_url(raw: &str) -> String {
    if let Some(uddg_start) = raw.find("uddg=") {
        let encoded = &raw[uddg_start + 5..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        return urldecoded(&encoded[..end]);
    }
    if raw.starts_with("http") {
        return raw.to_string();
    }
    if raw.starts_with("//") {
        return format!("https:{raw}");
    }
    raw.to_string()
}

// ─── Brave Search API ──────────────────────────────────────────

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWebResults>,
}

#[derive(Deserialize)]
struct BraveWebResults {
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: Option<String>,
}

async fn search_brave(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoded(query),
        max_results,
    );

    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Accept-Encoding", "gzip")
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|e| Error::Execution(format!("Brave search failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Execution(format!(
            "Brave API returned {status}: {body}"
        )));
    }

    let brave_response: BraveResponse = response
        .json()
        .await
        .map_err(|e| Error::Execution(format!("Failed to parse Brave response: {e}")))?;

    let results = brave_response
        .web
        .map(|w| {
            w.results
                .into_iter()
                .take(max_results)
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: r.description.unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(results)
}

// ─── Tavily Search API ─────────────────────────────────────────

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

async fn search_tavily(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    let body = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "max_results": max_results,
        "search_depth": "basic",
        "include_answer": false,
    });

    let response = client
        .post("https://api.tavily.com/search")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Execution(format!("Tavily search failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Execution(format!(
            "Tavily API returned {status}: {body}"
        )));
    }

    let tavily_response: TavilyResponse = response
        .json()
        .await
        .map_err(|e| Error::Execution(format!("Failed to parse Tavily response: {e}")))?;

    let results = tavily_response
        .results
        .into_iter()
        .take(max_results)
        .map(|r| SearchResult {
            title: r.title,
            url: r.url,
            // Tavily returns pre-extracted content — richer than snippets.
            snippet: r.content,
        })
        .collect();

    Ok(results)
}

// ─── Shared Helpers ────────────────────────────────────────────

fn urlencoded(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(ch),
            ' ' => result.push('+'),
            _ => {
                for byte in ch.to_string().as_bytes() {
                    result.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    result
}

fn urldecoded(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
            {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            result.push(b' ');
        } else {
            result.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}

fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_decode_roundtrip() {
        let original = "rust programming language";
        let encoded = urlencoded(original);
        assert_eq!(encoded, "rust+programming+language");
        let decoded = urldecoded(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn url_decode_percent() {
        assert_eq!(
            urldecoded("https%3A%2F%2Fexample.com"),
            "https://example.com"
        );
    }

    #[test]
    fn strip_tags() {
        assert_eq!(strip_html_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html_tags("no tags"), "no tags");
    }

    #[test]
    fn html_decode_entities() {
        assert_eq!(html_decode("A &amp; B"), "A & B");
        assert_eq!(html_decode("&lt;tag&gt;"), "<tag>");
    }

    #[test]
    fn extract_ddg_url_with_redirect() {
        let raw = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        assert_eq!(extract_ddg_url(raw), "https://example.com/page");
    }

    #[test]
    fn extract_ddg_url_direct() {
        assert_eq!(
            extract_ddg_url("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn backend_names() {
        assert_eq!(SearchBackend::DuckDuckGo.name(), "DuckDuckGo");
        assert_eq!(
            SearchBackend::Brave {
                api_key: "x".into()
            }
            .name(),
            "Brave"
        );
        assert_eq!(
            SearchBackend::Tavily {
                api_key: "x".into()
            }
            .name(),
            "Tavily"
        );
        assert_eq!(
            SearchBackend::Mock {
                corpus_path: std::path::PathBuf::from("/tmp/x")
            }
            .name(),
            "Mock"
        );
    }

    // ─── Mock-backend invariants ──────────────────────────────────
    //
    // Pin three structural properties:
    //   1. Query normalization is stable (the fixture-author can
    //      compute the hash off-line and trust it will match).
    //   2. A missing fixture is a loud failure (`Err`), never silent
    //      zero-results — the whole point of the mock is that the
    //      harness sees what the model asked for, even when the
    //      author hasn't recorded it.
    //   3. A present fixture returns the recorded results, truncated
    //      to `max_results`.

    #[test]
    fn mock_normalize_lowercases_and_collapses_whitespace() {
        assert_eq!(normalize_query("Hello  World"), "hello world");
        assert_eq!(
            normalize_query("  leading and trailing  "),
            "leading and trailing"
        );
        assert_eq!(normalize_query("Tabs\tand\nnewlines"), "tabs and newlines");
    }

    #[test]
    fn mock_hash_is_stable_across_whitespace_changes() {
        // Pin the hash so a future "improvement" to normalization
        // doesn't silently invalidate every recorded fixture.
        assert_eq!(
            mock_fixture_hash("hello world"),
            mock_fixture_hash("  HELLO   world  ")
        );
    }

    #[test]
    fn mock_missing_fixture_is_loud_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = search_mock(tmp.path(), "no fixture for this query", 10)
            .expect_err("missing fixture must error");
        let msg = format!("{err}");
        assert!(msg.contains("mock search fixture missing"), "msg={msg}");
        assert!(msg.contains("no fixture for this query"), "msg={msg}");
    }

    #[test]
    fn mock_present_fixture_returns_results_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let query = "spacex starship launch";
        let hash = mock_fixture_hash(query);
        let body = serde_json::json!({
            "query": query,
            "results": [
                {"title": "Result 1", "url": "https://example.com/1", "snippet": "..."},
                {"title": "Result 2", "url": "https://example.com/2", "snippet": "..."},
                {"title": "Result 3", "url": "https://example.com/3", "snippet": "..."},
            ]
        });
        std::fs::write(tmp.path().join(format!("{hash}.json")), body.to_string()).unwrap();

        let out = search_mock(tmp.path(), query, 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://example.com/1");
        assert_eq!(out[1].url, "https://example.com/2");
    }

    // ─── Alias-resolution invariants ─────────────────────────────
    //
    // `aliases.toml` lets one response file serve multiple query
    // phrasings. The lookup is two-tier — aliases first, hash
    // fallback — so existing hash-keyed fixtures keep working and
    // the new ergonomics are purely additive.

    fn write_alias_file(dir: &std::path::Path, filename: &str, results: Vec<(&str, &str, &str)>) {
        let arr: Vec<_> = results
            .into_iter()
            .map(|(t, u, s)| serde_json::json!({"title": t, "url": u, "snippet": s}))
            .collect();
        let body = serde_json::json!({"query": "fixture", "results": arr}).to_string();
        std::fs::write(dir.join(filename), body).unwrap();
    }

    #[test]
    fn mock_alias_resolves_multiple_phrasings_to_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_alias_file(
            tmp.path(),
            "spacex.json",
            vec![("Flight 14", "https://example.com/flight-14", "...")],
        );
        std::fs::write(
            tmp.path().join("aliases.toml"),
            r#"
[[entry]]
file = "spacex.json"
aliases = [
  "spacex starship test launch",
  "latest spacex starship flight",
  "spacex starship",
]
"#,
        )
        .unwrap();

        for q in &[
            "spacex starship test launch",
            "Latest SpaceX Starship Flight", // case + whitespace normalized
            "  spacex   starship  ",
        ] {
            let out = search_mock(tmp.path(), q, 10).unwrap();
            assert_eq!(out.len(), 1, "query={q:?}");
            assert_eq!(out[0].url, "https://example.com/flight-14");
        }
    }

    #[test]
    fn mock_alias_matches_via_substring_containment() {
        // The model routinely appends qualifying suffixes/prefixes.
        // Substring matching means an alias like "nvda stock price"
        // catches "nvda stock price today" and "current nvda stock
        // price", without the author having to enumerate every
        // variant.
        let tmp = tempfile::tempdir().unwrap();
        write_alias_file(
            tmp.path(),
            "nvda.json",
            vec![("NVDA quote", "https://example.com/nvda", "...")],
        );
        std::fs::write(
            tmp.path().join("aliases.toml"),
            r#"
[[entry]]
file = "nvda.json"
aliases = ["nvda stock price"]
"#,
        )
        .unwrap();

        for q in &[
            "NVDA stock price",                      // exact
            "NVDA stock price today",                // suffix
            "current NVDA stock price",              // prefix
            "what's the NVDA stock price right now", // both
        ] {
            let out = search_mock(tmp.path(), q, 10).unwrap();
            assert_eq!(out.len(), 1, "query={q:?}");
        }
    }

    #[test]
    fn mock_alias_does_not_overmatch_unrelated_queries() {
        // Substring-match is one-way: the alias must appear in the
        // query, not the reverse. A query that doesn't contain the
        // full alias falls through to the hash path (and errors if
        // no hash file).
        let tmp = tempfile::tempdir().unwrap();
        write_alias_file(
            tmp.path(),
            "nvda.json",
            vec![("NVDA quote", "https://example.com/nvda", "...")],
        );
        std::fs::write(
            tmp.path().join("aliases.toml"),
            r#"
[[entry]]
file = "nvda.json"
aliases = ["nvda stock price"]
"#,
        )
        .unwrap();

        // "nvda" alone is a substring of "what is nvda", but the
        // full alias "nvda stock price" is not — so this should
        // fall through to hash (and fail).
        let err = search_mock(tmp.path(), "what is nvda", 10).unwrap_err();
        assert!(format!("{err}").contains("mock search fixture missing (hash)"));
    }

    #[test]
    fn mock_alias_miss_falls_back_to_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let q = "uncovered query phrasing";
        let hash = mock_fixture_hash(q);
        write_alias_file(
            tmp.path(),
            &format!("{hash}.json"),
            vec![("Hash-keyed", "https://example.com/h", "...")],
        );
        // aliases.toml exists but doesn't cover this query.
        std::fs::write(
            tmp.path().join("aliases.toml"),
            r#"
[[entry]]
file = "something_else.json"
aliases = ["completely different"]
"#,
        )
        .unwrap();

        let out = search_mock(tmp.path(), q, 10).unwrap();
        assert_eq!(out[0].url, "https://example.com/h");
    }

    #[test]
    fn mock_missing_alias_toml_is_not_an_error() {
        // The aliases index is optional — a fixture corpus that
        // only uses hash-keyed files must still work.
        let tmp = tempfile::tempdir().unwrap();
        let q = "hash-only fixture";
        let hash = mock_fixture_hash(q);
        write_alias_file(
            tmp.path(),
            &format!("{hash}.json"),
            vec![("X", "https://example.com/x", "...")],
        );
        let out = search_mock(tmp.path(), q, 10).unwrap();
        assert_eq!(out[0].url, "https://example.com/x");
    }

    #[test]
    fn mock_malformed_aliases_toml_is_loud_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("aliases.toml"),
            "this is not valid toml [[[",
        )
        .unwrap();
        let err =
            search_mock(tmp.path(), "anything", 10).expect_err("malformed aliases must error");
        let msg = format!("{err}");
        assert!(msg.contains("aliases index malformed"), "msg={msg}");
    }

    #[test]
    fn mock_alias_typo_field_fails_loudly() {
        // `aliasses` instead of `aliases` — fixture-author typo
        // should surface, not silently match nothing.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("aliases.toml"),
            r#"
[[entry]]
file = "x.json"
aliasses = ["whatever"]
"#,
        )
        .unwrap();
        let err = search_mock(tmp.path(), "anything", 10).expect_err("unknown field must error");
        let msg = format!("{err}");
        assert!(msg.contains("aliases index malformed"), "msg={msg}");
    }
}
