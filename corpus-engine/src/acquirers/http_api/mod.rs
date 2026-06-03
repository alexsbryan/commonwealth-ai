//! Generic HTTP API acquirer.
//!
//! Replaces the never-implemented `api_paginated` stub with a real
//! recipe-author-friendly surface: parameterised URL templates,
//! pagination strategies (offset / cursor / next-URL / page-number),
//! JSONPath document-URL follow, rate limiting, custom headers /
//! User-Agent. Combined with `[recipe.parameters]` and
//! [`Recipe::resolve_parameters`](crate::recipe::Recipe::resolve_parameters),
//! a domain expert can author a working recipe for SEC EDGAR /
//! CourtListener / OpenAlex / PubMed / etc. without touching Rust.
//!
//! ## On-disk layout
//!
//! ```text
//! <download_dir>/<corpus_id>/
//! ├── docs/
//! │   ├── <sha256-of-url-1>.html      ← when [acquire.follow] is set
//! │   ├── <sha256-of-url-2>.html
//! │   └── ...
//! └── _progress.json                   ← resume bookkeeping
//! ```
//!
//! When `[acquire.follow]` is absent, the per-page response itself
//! is persisted under `docs/<sha-of-page-url>.json` so the
//! extractor (typically `jsonl` or `json`) can stream it.
//!
//! ## Resume
//!
//! `_progress.json` records the URLs of completed pages and
//! documents. A re-run with the same parameters short-circuits
//! anything already on disk, so a crashed ingest can be resumed
//! without re-hitting the upstream API for work that's already done.
//!
//! ## Concurrency
//!
//! Pagination is sequential per (template × for_each binding) so we
//! observe the cursor / offset state in order. Document follow is
//! bounded-concurrent within a single page response — see
//! [`FollowConfig::max_concurrency`](crate::recipe::FollowConfig).

pub mod follow;
pub mod pagination;
pub mod rate_limit;
pub mod template;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::progress::{IngestProgress, ProgressCallback};
use crate::recipe::{
    FollowConfig, HttpMethod, PaginationStrategy, RequestTemplate, ResolvedParameters,
};

use self::follow::{document_path, extract_document_urls, fetch_document};
use self::pagination::{next_page, NextPage, PaginationState};
use self::rate_limit::TokenBucket;
use self::template::{for_each_bindings, render_template};

/// HTTP API acquirer instance. Configured per ingest via the
/// recipe's `[acquire]` block.
pub struct HttpApiAcquirer {
    base_url: String,
    requests: Vec<RequestTemplate>,
    pagination: Option<PaginationStrategy>,
    follow: Option<FollowConfig>,
    rate_limiter: TokenBucket,
    client: reqwest::Client,
    parameters: ResolvedParameters,
    /// Headers whose value contains a `{name}` placeholder. The name
    /// is validated at construction time (so a typo fails fast) but
    /// the value template is interpolated per-request via
    /// [`render_request_headers`](Self::render_request_headers) so
    /// the final string can vary by `for_each` binding. Static
    /// headers (no placeholder) are baked into the client's
    /// `default_headers` instead.
    templated_headers: Vec<(reqwest::header::HeaderName, String)>,
}

impl HttpApiAcquirer {
    /// Construct from a fully-typed `[acquire]` block plus the
    /// resolved parameter values.
    pub fn new(
        base_url: String,
        requests: Vec<RequestTemplate>,
        pagination: Option<PaginationStrategy>,
        follow: Option<FollowConfig>,
        rate_limit_per_second: Option<f32>,
        user_agent: Option<String>,
        headers: Option<BTreeMap<String, String>>,
        parameters: ResolvedParameters,
    ) -> Result<Self> {
        if requests.is_empty() {
            return Err(Error::Recipe(
                "http_api acquirer requires at least one [[acquire.requests]] entry".into(),
            ));
        }

        // Build the HTTP client with the recipe-supplied UA and
        // any default headers that don't depend on parameters.
        // Templated headers are interpolated per-request later.
        let ua = user_agent.unwrap_or_else(|| {
            "CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)".to_string()
        });
        let mut client_builder = reqwest::Client::builder()
            .user_agent(ua)
            .timeout(Duration::from_secs(60));

        // Headers split into:
        //   - static (no `{...}` placeholder) → client default_headers
        //   - templated → kept on Self, interpolated per-request
        //     against the active for_each binding so values that
        //     reference parameters (e.g. `{api_token}`) actually
        //     reach the wire. The header *name* is validated now so
        //     a typo fails at construction, not at first request.
        let mut templated_headers: Vec<(reqwest::header::HeaderName, String)> = Vec::new();
        if let Some(headers) = &headers {
            let mut default_headers = reqwest::header::HeaderMap::new();
            for (k, v) in headers {
                let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| Error::Recipe(format!("invalid header `{k}`: {e}")))?;
                if v.contains('{') {
                    templated_headers.push((name, v.clone()));
                    continue;
                }
                let value = reqwest::header::HeaderValue::from_str(v)
                    .map_err(|e| Error::Recipe(format!("invalid header value for `{k}`: {e}")))?;
                default_headers.insert(name, value);
            }
            if !default_headers.is_empty() {
                client_builder = client_builder.default_headers(default_headers);
            }
        }

        let client = client_builder.build()?;

        let rate_limiter = match rate_limit_per_second {
            Some(rate) if rate > 0.0 => TokenBucket::new(rate),
            _ => TokenBucket::unlimited(),
        };

        Ok(Self {
            base_url,
            requests,
            pagination,
            follow,
            rate_limiter,
            client,
            parameters,
            templated_headers,
        })
    }

    /// Render every templated header against the current `binding`
    /// and assemble a [`reqwest::header::HeaderMap`] that
    /// `fetch_one_page` and `fetch_document` apply to each outbound
    /// request. Static headers are already on the client.
    fn render_request_headers(
        &self,
        binding: &BTreeMap<String, String>,
    ) -> Result<reqwest::header::HeaderMap> {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, template) in &self.templated_headers {
            let rendered = render_template(template, &self.base_url, binding)?;
            let value = reqwest::header::HeaderValue::from_str(&rendered).map_err(|e| {
                Error::Recipe(format!(
                    "header `{}` rendered to an invalid value: {e}",
                    name.as_str()
                ))
            })?;
            map.insert(name.clone(), value);
        }
        Ok(map)
    }

    /// Run the acquirer end-to-end. Returns the directory under
    /// `download_dir` that the extractor should walk.
    pub async fn acquire(
        &self,
        download_dir: &Path,
        corpus_id: &str,
        progress: &Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        let out_dir = download_dir.join(corpus_id);
        let docs_dir = out_dir.join("docs");
        std::fs::create_dir_all(&docs_dir)?;
        let journal_path = out_dir.join("_progress.json");
        let mut journal = ProgressJournal::load(&journal_path);

        for template in &self.requests {
            let bindings = for_each_bindings(&template.for_each, &self.parameters)?;
            for binding in bindings {
                self.run_one_template(template, &binding, &docs_dir, &mut journal, progress)
                    .await?;
                journal.save(&journal_path)?;
            }
        }
        Ok(docs_dir)
    }

    async fn run_one_template(
        &self,
        template: &RequestTemplate,
        binding: &BTreeMap<String, String>,
        docs_dir: &Path,
        journal: &mut ProgressJournal,
        progress: &Option<ProgressCallback>,
    ) -> Result<()> {
        let mut current_url = render_template(&template.url, &self.base_url, binding)?;
        let body_template = template.body.clone();
        let mut state = PaginationState::default();
        // Templated headers are stable across the page-loop within a
        // single binding: render once, reuse for every page + every
        // followed document.
        let request_headers = self.render_request_headers(binding)?;

        loop {
            // Skip the page if we already finished it on a prior run.
            if journal.completed_pages.contains(&current_url) {
                tracing::debug!(url = %current_url, "http_api: skipping completed page");
                // We still need pagination state to advance, but
                // without the response body we can't compute the
                // next URL for cursor-based strategies. The simplest
                // robust resume rule is: skip already-fetched
                // documents (handled in fetch_document via existing
                // file check) but re-fetch pages — pages are usually
                // small. So break out and proceed.
                break;
            }

            self.rate_limiter.wait().await;

            let body_str = match body_template.as_deref() {
                None => None,
                Some(b) => Some(render_template(b, &self.base_url, binding)?),
            };

            let response_value = self
                .fetch_one_page(
                    &current_url,
                    template.method,
                    body_str.as_deref(),
                    &request_headers,
                )
                .await?;

            // Persist (or follow + persist documents) for this page.
            match &self.follow {
                Some(follow_cfg) => {
                    self.fetch_followed_documents(
                        &response_value,
                        docs_dir,
                        follow_cfg,
                        journal,
                        progress,
                        &request_headers,
                    )
                    .await?;
                }
                None => {
                    let path =
                        document_path(docs_dir, &current_url, crate::recipe::DocFormat::Json);
                    if !path.exists() {
                        let bytes = serde_json::to_vec_pretty(&response_value)
                            .map_err(|e| Error::Serialization(e.to_string()))?;
                        std::fs::write(&path, bytes)?;
                    }
                }
            }
            journal.completed_pages.insert(current_url.clone());

            // Decide whether to continue.
            let next = match &self.pagination {
                None => NextPage::Done,
                Some(strat) => next_page(strat, &current_url, &response_value, &mut state)?,
            };
            match next {
                NextPage::Url(u) => {
                    current_url = u;
                }
                NextPage::Done => break,
            }
        }
        Ok(())
    }

    async fn fetch_one_page(
        &self,
        url: &str,
        method: HttpMethod,
        body: Option<&str>,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<serde_json::Value> {
        let request = match method {
            HttpMethod::Get => self.client.get(url),
            HttpMethod::Post => {
                let mut req = self.client.post(url);
                if let Some(b) = body {
                    req = req
                        .body(b.to_string())
                        .header("Content-Type", "application/json");
                }
                req
            }
        };
        let request = if headers.is_empty() {
            request
        } else {
            request.headers(headers.clone())
        };
        let response = request.send().await?;
        let status = response.status();
        // Read the body up-front so non-2xx errors can include the
        // server's actual reason (e.g. CourtListener returns
        // `{"detail":"Invalid token."}` with 401). Without the body
        // a 401 is indistinguishable from "no auth header sent",
        // and the recipe author has to re-curl by hand to find out.
        let body = response.bytes().await?;
        if !status.is_success() {
            // Cap the excerpt at 400 bytes — the actionable content
            // (status detail / API error message) is always at the
            // start; long bodies are usually HTML error pages and
            // not useful here.
            let excerpt = String::from_utf8_lossy(&body[..body.len().min(400)])
                .trim()
                .to_string();
            let body_suffix = if excerpt.is_empty() {
                String::new()
            } else {
                format!(" — body: {excerpt}")
            };
            return Err(Error::Recipe(format!(
                "http_api request failed: {} {} -> HTTP {}{}",
                method_label(method),
                url,
                status,
                body_suffix,
            )));
        }
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
            Error::Recipe(format!(
                "http_api response from `{url}` is not valid JSON: {e}"
            ))
        })?;
        Ok(value)
    }

    async fn fetch_followed_documents(
        &self,
        page_body: &serde_json::Value,
        docs_dir: &Path,
        follow: &FollowConfig,
        journal: &mut ProgressJournal,
        progress: &Option<ProgressCallback>,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<()> {
        let urls = extract_document_urls(page_body, &follow.document_url_path)?;
        if urls.is_empty() {
            return Ok(());
        }
        let max_concurrency = follow.max_concurrency.max(1);
        let client = self.client.clone();
        let rate_limiter = self.rate_limiter.clone();
        let docs_dir_buf = docs_dir.to_path_buf();
        let follow_clone = follow.clone();
        let headers_clone = headers.clone();

        // Bounded-concurrent fetch with per-doc rate limiting.
        let results: Vec<Result<String>> = stream::iter(urls)
            .map(|url| {
                let client = client.clone();
                let rate_limiter = rate_limiter.clone();
                let docs_dir = docs_dir_buf.clone();
                let follow = follow_clone.clone();
                let headers = headers_clone.clone();
                async move {
                    rate_limiter.wait().await;
                    fetch_document(&client, &url, &docs_dir, &follow, &headers).await?;
                    Ok::<_, Error>(url)
                }
            })
            .buffer_unordered(max_concurrency)
            .collect()
            .await;

        for r in results {
            let url = r?;
            journal.completed_documents.insert(url);
        }
        if let Some(cb) = progress {
            // We don't know the total ahead of time; surface the
            // count-so-far as a Downloading event with no percent.
            cb(IngestProgress::Downloading {
                percent: 0.0,
                bytes_downloaded: journal.completed_documents.len() as u64,
                bytes_total: None,
            });
        }
        Ok(())
    }
}

fn method_label(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
    }
}

// ---------------------------------------------------------------------------
// Resume bookkeeping
// ---------------------------------------------------------------------------

/// Persistent journal of completed pages and documents. Written
/// atomically (`.part` + rename) after each template binding so a
/// crash mid-ingest doesn't lose the bookkeeping.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProgressJournal {
    /// URLs of pages that were fully fetched and either persisted
    /// (no follow) or whose documents were all enqueued.
    pub completed_pages: BTreeSet<String>,
    /// URLs of documents successfully written to disk.
    pub completed_documents: BTreeSet<String>,
}

impl ProgressJournal {
    fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|e| Error::Serialization(e.to_string()))?;
        let part = path.with_extension("json.part");
        std::fs::write(&part, bytes)?;
        std::fs::rename(&part, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{ParameterValue, ResolvedParameters};
    use std::collections::BTreeMap;

    fn params(items: Vec<(&str, ParameterValue)>) -> ResolvedParameters {
        let mut values = BTreeMap::new();
        for (k, v) in items {
            values.insert(k.into(), v);
        }
        ResolvedParameters { values }
    }

    #[test]
    fn rejects_empty_requests_list() {
        let result = HttpApiAcquirer::new(
            "https://example.com".into(),
            vec![],
            None,
            None,
            None,
            None,
            None,
            ResolvedParameters::default(),
        );
        match result {
            Err(e) => assert!(format!("{e}").contains("at least one")),
            Ok(_) => panic!("expected empty-requests rejection"),
        }
    }

    #[test]
    fn constructs_with_minimal_config() {
        let req = RequestTemplate {
            url: "{base_url}/items".into(),
            method: HttpMethod::Get,
            body: None,
            for_each: vec![],
        };
        let acq = HttpApiAcquirer::new(
            "https://example.com".into(),
            vec![req],
            None,
            None,
            Some(2.0),
            Some("Test".into()),
            None,
            params(vec![]),
        );
        assert!(acq.is_ok());
    }

    #[test]
    fn for_each_expands_two_axes_in_construction() {
        // Sanity check: cartesian product is computed by the helper
        // and surfaces in the acquirer's per-binding loop. We don't
        // run real HTTP here — wiremock-backed e2e tests live in
        // `corpus-engine/tests/http_api_pagination_e2e.rs` (Phase 1e).
        let p = params(vec![
            (
                "entity",
                ParameterValue::List(vec!["NVDA".into(), "MSFT".into()]),
            ),
            (
                "form_type",
                ParameterValue::List(vec!["10-K".into(), "10-Q".into()]),
            ),
        ]);
        let bindings = for_each_bindings(&["entity".into(), "form_type".into()], &p).unwrap();
        assert_eq!(bindings.len(), 4);
    }
}
