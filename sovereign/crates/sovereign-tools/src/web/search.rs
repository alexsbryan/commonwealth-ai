use serde::Deserialize;

use sovereign_core::error::{Error, Result};

/// A search result from any backend.
#[derive(Debug, Clone)]
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
}

impl SearchBackend {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Brave { .. } => "Brave",
            Self::Tavily { .. } => "Tavily",
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
        SearchBackend::Brave { api_key } => {
            search_brave(client, api_key, query, max_results).await
        }
        SearchBackend::Tavily { api_key } => {
            search_tavily(client, api_key, query, max_results).await
        }
    }
}

// ─── DuckDuckGo (free, zero-config) ───────────────────────────

async fn search_duckduckgo(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoded(query)
    );

    let response = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (compatible; Sovereign/0.1)")
        .send()
        .await
        .map_err(|e| Error::Execution(format!("DuckDuckGo search failed: {e}")))?;

    let html = response
        .text()
        .await
        .map_err(|e| Error::Execution(format!("Failed to read DDG response: {e}")))?;

    Ok(parse_ddg_results(&html, max_results))
}

fn parse_ddg_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
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

        if !url.is_empty() && !title.is_empty() {
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
            if let Ok(byte) = u8::from_str_radix(
                &String::from_utf8_lossy(&bytes[i + 1..i + 3]),
                16,
            ) {
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
    }
}
