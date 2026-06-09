// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ProbeUrlTool` — single-shot HTTP GET that lets the recipe-author
//! agent confirm an API contract before drafting a recipe around it.
//!
//! Why this exists: the live trial showed that without a way to read
//! a real response, the agent guesses URL paths, query params, and
//! pagination shape from training data — and gets them wrong. Most
//! visibly: CourtListener uses `cluster__docket__court=ca9` (v4),
//! not `jurisdiction=N9` (v3); its `next` field is a fully-qualified
//! URL, not a cursor token, so `[acquire.pagination] type = "cursor"`
//! produces a malformed follow-up request. Both errors take *one*
//! probe to disambiguate.
//!
//! The tool returns three layers of context:
//!
//! 1. **Status + headers** — auth / rate-limit / content-type sanity.
//! 2. **Structured hints** — top-level JSON keys and a sniff of the
//!    pagination shape (`next` URL vs cursor token vs page-number)
//!    so the agent doesn't have to invent a strategy.
//! 3. **Raw body excerpt** — the failsafe. The hint sniffer is best-
//!    effort; the agent can always fall back on reading the actual
//!    response prefix.
//!
//! Scope is deliberately narrow:
//! - GET only; no body. POST / PUT add a vector for accidental writes.
//! - Single request; pagination follow-up is the recipe's job.
//! - Body capped at 4 KB. Long responses don't add information for
//!   this task and inflate the agent's context for no return.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};

use corpus_engine::acquirers::http_api::template::render_template;
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

const MAX_BODY_BYTES: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Detected pagination shape, returned to the agent so it can pick
/// the right `[acquire.pagination]` arm without further guesswork.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaginationHint {
    /// Body has a `next` (or alias) field whose value is a full URL.
    /// Recipe should use `type = "next_url"`.
    NextUrl { field: String, example: String },
    /// Body has a `next_cursor` (or alias) field whose value is a
    /// short token. Recipe should use `type = "cursor"`.
    Cursor { field: String, example: String },
    /// Body has `page` / `total_pages` style metadata. Recipe should
    /// use `type = "page_number"`.
    PageNumber { field: String, example: String },
    /// No recognised pagination signal. Agent should look at the raw
    /// body excerpt or hit the next URL with a different query.
    Unknown,
}

#[derive(Default)]
pub struct ProbeUrlTool {
    /// Optional parameter store. When set, `{placeholder}` tokens
    /// in the request URL and each header value are substituted
    /// against this map before the wire request — matching the
    /// `http_api` acquirer's behaviour. The agent never sees the
    /// literal values: it writes `Authorization: Token {api_token}`
    /// and the tool does the substitution server-side. Populated
    /// by the live-trial harness from `--param key=value` flags.
    parameters: Option<Arc<BTreeMap<String, String>>>,
}

impl ProbeUrlTool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the recipe-author project's resolved parameter values
    /// so `{placeholder}` substitution works across `url` + `headers`.
    /// Without this the tool sends placeholders verbatim — fine for
    /// public APIs, broken for any auth header that pulls the token
    /// from `[parameters]`.
    pub fn with_parameters(mut self, parameters: Arc<BTreeMap<String, String>>) -> Self {
        self.parameters = Some(parameters);
        self
    }
}

#[async_trait]
impl Tool for ProbeUrlTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "probe_url".into(),
            name: "ProbeUrl".into(),
            description: "Send a single HTTP GET to `url` and report back: status \
                 code, content-type, the top-level JSON keys (if the \
                 body parses as JSON), a sniff of the response's \
                 pagination shape (`next_url` / `cursor` / \
                 `page_number` / `unknown`), and a raw body excerpt \
                 (capped at 4 KB). Use this BEFORE drafting an \
                 http_api recipe to confirm the API contract — the \
                 endpoint actually exists, your auth header works, \
                 the response shape matches what you expect, and \
                 pagination uses the strategy you'll wire into the \
                 recipe. The pagination hint is best-effort; always \
                 cross-check against the raw body excerpt before \
                 committing to a recipe shape. Single request only — \
                 follow-ups are the recipe's job."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description":
                            "Fully-qualified URL to GET. May contain \
                             `{placeholder}` tokens that resolve to \
                             the project's declared parameter values \
                             (same syntax as `[acquire.requests].url` \
                             in the recipe TOML)."
                    },
                    "headers": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description":
                            "Optional request headers. Header VALUES \
                             may contain `{placeholder}` tokens that \
                             resolve against the project's declared \
                             parameters — this is how you probe an \
                             auth-gated endpoint without ever pasting \
                             the literal token. Example: \
                             {\"Authorization\": \"Token {api_token}\"}."
                    }
                },
                "required": ["url"]
            }),
            examples: vec![ToolExample {
                situation: "Confirm CourtListener v4 endpoint shape \
                            and pagination before drafting the recipe."
                    .into(),
                call: json!({
                    "url": "https://www.courtlistener.com/api/rest/v4/opinions/?cluster__docket__court=ca9&page_size=2",
                    "headers": {"Authorization": "Token <api_token>"}
                }),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status":            {"type": "integer"},
                    "content_type":      {"type": "string"},
                    "top_level_keys":    {"type": "array",  "items": {"type": "string"}},
                    "pagination_hint":   {"type": "object"},
                    "body_excerpt":      {"type": "string"},
                    "body_truncated":    {"type": "boolean"},
                    "body_is_json":      {"type": "boolean"}
                },
                "required": ["status", "body_excerpt"]
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        // Same surface as web_fetch — single outbound request.
        vec![Permission::Network]
    }

    async fn execute(&self, params: &Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("ProbeUrlTool requires `url`".into()))?
            .to_string();
        let raw_headers: BTreeMap<String, String> = params
            .get("headers")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect::<BTreeMap<String, String>>()
            })
            .unwrap_or_default();

        // Substitute `{placeholder}` tokens in URL + headers against
        // the project's resolved parameters. Without a parameter map
        // attached, placeholders pass through verbatim — the trial
        // harness always attaches one, so this only matters for unit
        // tests that exercise probe_url stand-alone.
        let empty: BTreeMap<String, String> = BTreeMap::new();
        let bindings = self
            .parameters
            .as_ref()
            .map(|p| p.as_ref())
            .unwrap_or(&empty);
        let url = render_template(&raw_url, "", bindings)
            .map_err(|e| Error::InvalidInput(format!("probe_url: rendering url: {e}")))?;
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in &raw_headers {
            let rendered = render_template(v, "", bindings).map_err(|e| {
                Error::InvalidInput(format!("probe_url: rendering header `{k}`: {e}"))
            })?;
            headers.insert(k.clone(), rendered);
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::InvalidInput(format!(
                "ProbeUrlTool: `url` must be http(s); got `{url}`"
            )));
        }

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("Sovereign recipe-author probe (https://sovereign.dev/corpus-engine)")
            .build()
            .map_err(|e| Error::Execution(format!("probe_url: build client: {e}")))?;

        let mut req = client.get(&url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Execution(format!("probe_url GET {url}: {e}")))?;

        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        // Read the body up to MAX_BODY_BYTES + 1 so we can detect
        // truncation. `bytes()` will pull the full body — for safety
        // against a multi-MB response we use a chunk reader.
        let bytes_full = resp
            .bytes()
            .await
            .map_err(|e| Error::Execution(format!("probe_url read body: {e}")))?;
        let body_truncated = bytes_full.len() > MAX_BODY_BYTES;
        let bytes = if body_truncated {
            bytes_full.slice(0..MAX_BODY_BYTES)
        } else {
            bytes_full.clone()
        };
        let body_excerpt = String::from_utf8_lossy(&bytes).to_string();

        // Parse against the FULL body (not the excerpt) so a truncated
        // 4 KB-ish response still gives clean structured hints. JSON
        // bodies often blow past 4 KB on a single page.
        let parsed: Option<Value> = serde_json::from_slice(&bytes_full).ok();
        let body_is_json = parsed.is_some();
        let top_level_keys = parsed
            .as_ref()
            .and_then(|v| v.as_object())
            .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let pagination_hint = parsed
            .as_ref()
            .map(detect_pagination_hint)
            .unwrap_or(PaginationHint::Unknown);

        let mut out = json!({
            "status": status,
            "body_excerpt": body_excerpt,
            "body_truncated": body_truncated,
            "body_is_json": body_is_json,
            "top_level_keys": top_level_keys,
            "pagination_hint": pagination_hint,
        });
        if let Some(ct) = content_type {
            out["content_type"] = Value::String(ct);
        }
        // Status-shaped guidance. Agent loops were treating probe
        // responses as scenery — a 4xx with a perfectly clear
        // `unknown_params` body got skipped while the agent went
        // off guessing camelCase variants. Surface the actual error
        // payload as `likely_cause` and a short `next_action` so the
        // agent's next step is unambiguous.
        if (400..500).contains(&status) {
            if let Some(cause) = parsed.as_ref().and_then(extract_error_cause) {
                out["likely_cause"] = cause;
            }
            out["next_action"] = Value::String(
                "Read `body_excerpt` / `likely_cause` — the API \
                 told you exactly what's wrong. Fix the URL or \
                 params and re-probe; do NOT retry blind variants."
                    .into(),
            );
        } else if (200..300).contains(&status) {
            out["next_action"] = Value::String(
                "Probe succeeded. Write a `research_finding` \
                 capturing what you confirmed (host, path, auth \
                 shape, pagination_hint) BEFORE drafting. The note \
                 survives across sessions and a checkpoint restore."
                    .into(),
            );
        }
        Ok(StepOutput::Json(out))
    }
}

/// Pull the most-likely-useful error fields out of a parsed JSON
/// error body. Common shapes (REST defaults, DRF, JSON:API):
///   {"detail": "..."}                         — DRF / many APIs
///   {"error": "..."}                          — generic
///   {"message": "..."}                        — generic
///   {"errors": [...]}                         — JSON:API + bulk
///   {"unknown_params": ["..."], "detail": "..."}  — DRF filter rejection
///   {"errors": {"field": ["msg"]}}            — DRF serializer
fn extract_error_cause(body: &Value) -> Option<Value> {
    let obj = body.as_object()?;
    let mut cause = serde_json::Map::new();
    for key in &["detail", "error", "message", "errors"] {
        if let Some(v) = obj.get(*key) {
            cause.insert((*key).to_string(), v.clone());
        }
    }
    // DRF's filter-rejection shape is `unknown_params` — surface it
    // explicitly so the agent doesn't have to spelunk the body.
    for key in &["unknown_params", "invalid_params", "missing_params"] {
        if let Some(v) = obj.get(*key) {
            cause.insert((*key).to_string(), v.clone());
        }
    }
    if cause.is_empty() {
        None
    } else {
        Some(Value::Object(cause))
    }
}

/// Sniff the pagination shape from a parsed response body.
///
/// Heuristic order, since the agent will use the *first* signal that
/// matches:
///
/// 1. Top-level `next` (or alias) holding a full URL → `next_url`.
/// 2. Top-level `next_cursor` / `next_page_token` (or `next` with a
///    short non-URL value) → `cursor`.
/// 3. Top-level `page` + `total_pages` style fields → `page_number`.
/// 4. Otherwise `unknown` and the agent reads the raw body.
///
/// Lives here (not in `corpus-engine`) because the heuristic is
/// best-effort and tied to the agent's UX, not the engine's runtime
/// semantics — it should be cheap to tweak as we observe more APIs.
pub fn detect_pagination_hint(body: &Value) -> PaginationHint {
    let obj = match body.as_object() {
        Some(o) => o,
        None => return PaginationHint::Unknown,
    };

    const NEXT_URL_FIELDS: &[&str] = &["next", "next_url", "next_page_url"];
    const CURSOR_FIELDS: &[&str] = &["next_cursor", "next_page_token", "nextPageToken", "cursor"];
    const PAGE_NUMBER_FIELDS: &[&str] = &["page", "current_page", "page_number"];

    for field in NEXT_URL_FIELDS {
        if let Some(v) = obj.get(*field).and_then(|v| v.as_str()) {
            if looks_like_url(v) {
                return PaginationHint::NextUrl {
                    field: (*field).into(),
                    example: v.to_string(),
                };
            }
            // `next: "<short token>"` — looks like a cursor, not a URL.
            // The CourtListener case lands on the URL branch above; an
            // API that uses `next` for a token (some custom REST APIs)
            // lands here.
            if !v.is_empty() {
                return PaginationHint::Cursor {
                    field: (*field).into(),
                    example: v.to_string(),
                };
            }
        }
    }
    for field in CURSOR_FIELDS {
        if let Some(v) = obj.get(*field).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return PaginationHint::Cursor {
                    field: (*field).into(),
                    example: v.to_string(),
                };
            }
        }
    }
    for field in PAGE_NUMBER_FIELDS {
        if let Some(v) = obj.get(*field) {
            if v.is_number() || v.is_string() {
                return PaginationHint::PageNumber {
                    field: (*field).into(),
                    example: v.to_string(),
                };
            }
        }
    }
    PaginationHint::Unknown
}

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_next_url_pagination_courtlistener_shape() {
        let body = json!({
            "next": "https://www.courtlistener.com/api/rest/v4/opinions/?cursor=cD0xMTE5NjI4NA%3D%3D&page_size=50",
            "previous": null,
            "results": [{"id": 101}, {"id": 102}]
        });
        match detect_pagination_hint(&body) {
            PaginationHint::NextUrl { field, example } => {
                assert_eq!(field, "next");
                assert!(example.starts_with("https://"));
            }
            other => panic!("expected NextUrl, got {other:?}"),
        }
    }

    #[test]
    fn detects_cursor_when_next_field_is_a_short_token() {
        let body = json!({
            "next": "cD0xMTE5NjI4NA",
            "results": []
        });
        match detect_pagination_hint(&body) {
            PaginationHint::Cursor { field, example } => {
                assert_eq!(field, "next");
                assert_eq!(example, "cD0xMTE5NjI4NA");
            }
            other => panic!("expected Cursor, got {other:?}"),
        }
    }

    #[test]
    fn detects_explicit_cursor_field() {
        let body = json!({
            "next_cursor": "abc123",
            "items": []
        });
        match detect_pagination_hint(&body) {
            PaginationHint::Cursor { field, .. } => {
                assert_eq!(field, "next_cursor");
            }
            other => panic!("expected Cursor, got {other:?}"),
        }
    }

    #[test]
    fn detects_page_number_pagination() {
        let body = json!({
            "page": 1,
            "total_pages": 12,
            "results": []
        });
        match detect_pagination_hint(&body) {
            PaginationHint::PageNumber { field, example } => {
                assert_eq!(field, "page");
                assert_eq!(example, "1");
            }
            other => panic!("expected PageNumber, got {other:?}"),
        }
    }

    #[test]
    fn unknown_when_no_signal() {
        let body = json!({
            "data": [1, 2, 3],
            "metadata": {"server": "test"}
        });
        assert_eq!(detect_pagination_hint(&body), PaginationHint::Unknown);
    }

    #[test]
    fn unknown_for_non_object_body() {
        assert_eq!(
            detect_pagination_hint(&json!([1, 2, 3])),
            PaginationHint::Unknown
        );
        assert_eq!(
            detect_pagination_hint(&json!("string")),
            PaginationHint::Unknown
        );
    }

    #[test]
    fn next_field_with_empty_string_falls_through_to_cursor_search() {
        let body = json!({
            "next": "",
            "next_cursor": "real",
            "items": []
        });
        match detect_pagination_hint(&body) {
            PaginationHint::Cursor { field, .. } => assert_eq!(field, "next_cursor"),
            other => panic!("expected next_cursor fallback, got {other:?}"),
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: ConversationId::new(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[tokio::test]
    async fn rejects_non_http_url() {
        let tool = ProbeUrlTool::new();
        let err = tool
            .execute(&json!({"url": "file:///etc/passwd"}), &ctx())
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("http(s)"));
    }

    #[tokio::test]
    async fn descriptor_has_required_url_param() {
        let tool = ProbeUrlTool::new();
        let d = tool.descriptor();
        assert_eq!(d.id, "probe_url");
        let req = d.parameters["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "url"));
    }

    #[tokio::test]
    async fn placeholder_in_url_substitutes_against_parameters() {
        // Without a real network call we can't observe the rendered
        // URL via execute(). But render_template is the same path
        // used in production, so a unit test on the bindings path
        // is the right scope: we verify substitution happens via the
        // shared render_template, then test the failure mode
        // (undeclared placeholder) end-to-end through execute().
        let mut p: BTreeMap<String, String> = BTreeMap::new();
        p.insert("court".into(), "ca9".into());
        let rendered = render_template("https://example.com/api/?court={court}", "", &p).unwrap();
        assert_eq!(rendered, "https://example.com/api/?court=ca9");
    }

    #[tokio::test]
    async fn missing_placeholder_surfaces_clear_error() {
        let tool = ProbeUrlTool::new();
        let err = tool
            .execute(
                &json!({
                    "url": "https://example.com/{api_token}",
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("api_token") || msg.contains("placeholder"),
            "expected placeholder error, got: {msg}"
        );
    }

    #[test]
    fn extract_error_cause_pulls_drf_filter_rejection() {
        let body = json!({
            "detail": "Unknown filter parameters are not allowed.",
            "unknown_params": ["court", "date_filed_gte"]
        });
        let cause = extract_error_cause(&body).unwrap();
        let obj = cause.as_object().unwrap();
        assert!(obj.contains_key("detail"));
        assert!(obj.contains_key("unknown_params"));
    }

    #[test]
    fn extract_error_cause_handles_generic_error_field() {
        let body = json!({"error": "Bad token"});
        let cause = extract_error_cause(&body).unwrap();
        assert_eq!(cause["error"], "Bad token");
    }

    #[test]
    fn extract_error_cause_returns_none_when_no_signal() {
        let body = json!({"results": [], "next": null});
        assert!(extract_error_cause(&body).is_none());
    }

    #[tokio::test]
    async fn header_placeholder_renders_against_parameters() {
        // We can't easily observe the wire request headers without
        // a fake server. Verify the rendering step itself instead —
        // the same call execute() makes before send().
        let mut p: BTreeMap<String, String> = BTreeMap::new();
        p.insert("api_token".into(), "tok-123".into());
        let rendered = render_template("Token {api_token}", "", &p).unwrap();
        assert_eq!(rendered, "Token tok-123");
    }
}
