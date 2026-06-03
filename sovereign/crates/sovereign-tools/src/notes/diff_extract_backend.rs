//! Production backend for [`DiffDecisionExtractor`].
//!
//! Calls the daemon's OpenAI-compatible `/v1/chat/completions`
//! endpoint with the focused-extraction prompt assembled by
//! [`super::diff_extract::build_prompt`] and parses the response
//! through [`super::diff_extract::parse_extractions`] (line-
//! delimited fallback) plus a JSON-schema-constrained variant for
//! daemons that have LLGuidance enabled.
//!
//! ## Dual output shapes
//!
//! Two parse paths supported:
//!
//! 1. **Schema-constrained (preferred).** The request carries
//!    `response_format = { type: "json_schema", json_schema: ... }`
//!    where the schema is a single object with a `decisions`
//!    array. The daemon's grammar-constrained sampler forces the
//!    model to produce exactly that shape, defeating the malformed-
//!    JSON drift Gemma-31B and Qwopus-27B exhibit on long
//!    structured outputs (see the
//!    `project_grammar_constrained_phase1` decision note for the
//!    Phase 1 atlas precedent).
//!
//! 2. **Free-form (fallback).** If the daemon doesn't support
//!    grammar constraints, the model produces line-delimited
//!    JSON per `build_prompt`'s instruction. The existing
//!    `parse_extractions` function handles that shape.
//!
//! The backend tries the schema-constrained path first; on parse
//! failure it falls back to the line-delimited parser. Either
//! way the output is `Vec<DecisionExtraction>`.
//!
//! ## Token budget
//!
//! - `max_tokens = 800` on the response — enough for ~6–8
//!   single-sentence decisions plus envelope, well under most
//!   models' completion budget.
//! - `temperature = 0.2` — deterministic enough that two runs on
//!   the same diff produce roughly the same extractions, but not
//!   so deterministic the model refuses to surface borderline
//!   cases.
//! - Diff input is already capped at `MAX_DIFF_INPUT_BYTES`
//!   inside `build_prompt`. The backend doesn't re-cap.
//!
//! ## Latency posture
//!
//! Audit-time invocation. A single end-of-week run on a
//! Qwen3.5-32B can take 20–60 seconds; the user is waiting on
//! `sovereign audit` and that's fine. Per-turn middleware
//! (`decision_extractor`) does NOT call this — that path uses
//! the lightweight `response_mine` regex.

use async_trait::async_trait;

use super::diff_extract::{
    parse_extractions, DecisionExtraction, DecisionExtractorBackend, ExtractionRequest,
};

/// Maximum tokens for the model's completion. ~800 fits roughly
/// 6–8 decisions with the schema's framing overhead. The cap
/// stops a runaway model from producing a 10K-token wall of text.
const MAX_COMPLETION_TOKENS: u32 = 800;

/// Temperature for the extraction call. Low enough for
/// reproducibility, high enough that the model surfaces
/// borderline decisions instead of refusing.
const TEMPERATURE: f32 = 0.2;

/// Default request timeout. Audit-time, but we still want a
/// hard ceiling so a stuck slot doesn't park `sovereign audit`
/// indefinitely. 120s comfortably covers a 32B-model invocation
/// at ~20–40 tok/s.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Minimal model-response envelope. We borrow only the fields
/// `parse_extractions` cares about so the backend doesn't pull
/// in `commonwealth-api`'s richer `ChatCompletionResponse` shape
/// (which depends on inference-side types we don't need here).
#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(serde::Deserialize)]
struct ResponseMessage {
    content: String,
}

/// Configuration for [`LocalLlmBackend`]. Decoupled from the
/// backend struct so callers can build a config from
/// `~/.sovereign/config.toml` (or test harnesses) and pass
/// it to [`LocalLlmBackend::new`].
#[derive(Debug, Clone)]
pub struct LocalLlmConfig {
    /// Base URL of the daemon, e.g. `http://127.0.0.1:9741`. The
    /// backend appends `/v1/chat/completions`. No trailing slash
    /// requirement — we trim before joining.
    pub daemon_url: String,
    /// Concrete model identifier the daemon's slot manager
    /// resolves. Typically `qwen-27b-coder` or whatever the user
    /// configured as primary.
    pub model_id: String,
    /// When `true`, the request carries a JSON-schema
    /// `response_format`. When `false`, free-form output is
    /// requested and the line-delimited parser handles the
    /// result. Default: `true` — daemons with grammar support
    /// produce dramatically cleaner output.
    pub use_schema_constraint: bool,
    /// Per-request timeout. `Duration::from_secs(120)` by default.
    pub timeout: std::time::Duration,
}

impl LocalLlmConfig {
    /// Convenience constructor with sensible defaults for the
    /// daemon's standard `:9741` mount.
    pub fn for_daemon(daemon_url: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            daemon_url: daemon_url.into(),
            model_id: model_id.into(),
            use_schema_constraint: true,
            timeout: std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }
}

/// Backend that calls a daemon-hosted chat completion endpoint.
/// Construct via [`LocalLlmBackend::new`] and pass to
/// [`super::diff_extract::DiffDecisionExtractor::new`].
pub struct LocalLlmBackend {
    config: LocalLlmConfig,
    client: reqwest::Client,
}

impl LocalLlmBackend {
    pub fn new(config: LocalLlmConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| format!("LocalLlmBackend: HTTP client build: {e}"))?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl DecisionExtractorBackend for LocalLlmBackend {
    async fn extract(
        &self,
        request: &ExtractionRequest,
    ) -> Result<Vec<DecisionExtraction>, String> {
        let prompt = super::diff_extract::build_prompt(request);

        // System message kept empty — the prompt itself is the
        // full instruction. Some local models (Qwen, Gemma) react
        // poorly to dual-prompt setups when the system slot is
        // tuned for a different persona.
        let mut body = serde_json::json!({
            "model": self.config.model_id,
            "temperature": TEMPERATURE,
            "max_tokens": MAX_COMPLETION_TOKENS,
            "messages": [
                { "role": "user", "content": prompt }
            ],
        });

        if self.config.use_schema_constraint {
            body["response_format"] = response_format_schema();
        }

        let url = format!(
            "{}/v1/chat/completions",
            self.config.daemon_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("LocalLlmBackend: POST {url}: {e}"))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("LocalLlmBackend: read body: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "LocalLlmBackend: {} returned {status}: {}",
                url,
                text.chars().take(200).collect::<String>()
            ));
        }

        let envelope: ChatResponse = serde_json::from_str(&text)
            .map_err(|e| format!("LocalLlmBackend: parse envelope: {e}"))?;
        let content = envelope
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| "LocalLlmBackend: empty choices in response".to_string())?;

        // Schema-constrained path: a single JSON object with a
        // `decisions` array. Free-form path: line-delimited JSON.
        // We try schema first, fall through to line-delimited so
        // a daemon that ignored `response_format` still works.
        if let Some(extractions) = parse_schema_envelope(&content) {
            return Ok(extractions);
        }
        Ok(parse_extractions(&content))
    }
}

/// `response_format` value for the schema-constrained path.
/// Pulled into a function so tests can assert the shape.
fn response_format_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "decision_extractions",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["decisions"],
                "properties": {
                    "decisions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "body"],
                            "properties": {
                                "kind": {
                                    "type": "string",
                                    "enum": ["decision", "deviation", "invariant"]
                                },
                                "body": { "type": "string" },
                                "supersedes_id": { "type": ["string", "null"] }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Parse the schema-constrained shape: a single JSON object whose
/// `decisions` field is an array of `{kind, body, supersedes_id?}`.
/// Returns `None` if the input doesn't look like that shape — the
/// caller falls back to line-delimited parsing.
fn parse_schema_envelope(raw: &str) -> Option<Vec<DecisionExtraction>> {
    let trimmed = raw.trim();
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let arr = value.get("decisions")?.as_array()?;
    let mut out = Vec::new();
    for item in arr {
        let kind = item.get("kind")?.as_str()?.to_string();
        let body = item.get("body")?.as_str()?.to_string();
        if body.trim().is_empty() {
            continue;
        }
        let supersedes = item
            .get("supersedes_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        out.push(DecisionExtraction {
            kind,
            body,
            supersedes,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::routing::post;
    use axum::{Json, Router};

    /// Minimal stub of `/v1/chat/completions`. The handler echoes
    /// whichever canned response the test installed via the
    /// `Arc<Mutex<...>>` extension.
    type CannedResponse = Arc<tokio::sync::Mutex<serde_json::Value>>;

    async fn chat_handler(
        axum::extract::Extension(canned): axum::extract::Extension<CannedResponse>,
        Json(_req): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        let value = canned.lock().await.clone();
        Json(value)
    }

    async fn spawn_stub(canned: serde_json::Value) -> (SocketAddr, CannedResponse) {
        let canned = Arc::new(tokio::sync::Mutex::new(canned));
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_handler))
            .layer(axum::Extension(Arc::clone(&canned)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        // Brief beat so axum starts accepting.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        (addr, canned)
    }

    fn req() -> ExtractionRequest {
        ExtractionRequest {
            diff_text: "+let x = 1;".into(),
            session_summary: None,
            existing_notes: Vec::new(),
        }
    }

    fn chat_envelope(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 0,
            "model": "stub",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }],
            "usage": null
        })
    }

    /// Schema-constrained happy path: daemon returns a single
    /// JSON object with a `decisions` array; backend parses it
    /// directly.
    #[tokio::test]
    async fn schema_envelope_round_trips() {
        let payload = serde_json::json!({
            "decisions": [
                {"kind": "decision", "body": "switch to async channels"},
                {"kind": "deviation", "body": "drops strict ordering",
                 "supersedes_id": "n42"}
            ]
        })
        .to_string();
        let (addr, _) = spawn_stub(chat_envelope(&payload)).await;

        let backend =
            LocalLlmBackend::new(LocalLlmConfig::for_daemon(format!("http://{addr}"), "stub"))
                .unwrap();
        let out = backend.extract(&req()).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, "decision");
        assert_eq!(out[0].body, "switch to async channels");
        assert_eq!(out[1].supersedes.as_deref(), Some("n42"));
    }

    /// Free-form (line-delimited) fallback: when the daemon
    /// returns NDJSON instead of a schema envelope, the backend
    /// falls through to `parse_extractions`.
    #[tokio::test]
    async fn line_delimited_fallback_when_no_schema_envelope() {
        let payload = "\
            {\"kind\":\"decision\",\"body\":\"first\"}\n\
            {\"kind\":\"invariant\",\"body\":\"second\"}\n";
        let (addr, _) = spawn_stub(chat_envelope(payload)).await;

        let cfg = LocalLlmConfig {
            use_schema_constraint: false,
            ..LocalLlmConfig::for_daemon(format!("http://{addr}"), "stub")
        };
        let backend = LocalLlmBackend::new(cfg).unwrap();
        let out = backend.extract(&req()).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].body, "first");
        assert_eq!(out[1].kind, "invariant");
    }

    /// Mid-shape fallback: backend requests schema-constrained
    /// output, but the daemon ignores `response_format` and emits
    /// NDJSON anyway. The schema-envelope parse fails, the
    /// backend falls through to the line-delimited parser, and
    /// the user still gets useful output.
    #[tokio::test]
    async fn schema_request_with_ndjson_response_falls_back() {
        let payload = "{\"kind\":\"decision\",\"body\":\"single\"}\n";
        let (addr, _) = spawn_stub(chat_envelope(payload)).await;

        let backend =
            LocalLlmBackend::new(LocalLlmConfig::for_daemon(format!("http://{addr}"), "stub"))
                .unwrap();
        let out = backend.extract(&req()).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "single");
    }

    /// Empty `decisions` array is a valid response (the model
    /// found nothing worth surfacing). Backend returns an empty
    /// Vec, NOT an error.
    #[tokio::test]
    async fn empty_decisions_array_yields_empty_vec() {
        let payload = serde_json::json!({ "decisions": [] }).to_string();
        let (addr, _) = spawn_stub(chat_envelope(&payload)).await;

        let backend =
            LocalLlmBackend::new(LocalLlmConfig::for_daemon(format!("http://{addr}"), "stub"))
                .unwrap();
        let out = backend.extract(&req()).await.unwrap();
        assert!(out.is_empty());
    }

    /// Non-2xx status from the daemon surfaces as an error string.
    /// The audit's `DiffDecisionExtractor::extract` swallows the
    /// error and returns an empty Vec, so the audit's "extracted"
    /// stream is best-effort even in this case.
    #[tokio::test]
    async fn http_error_surfaces_as_err() {
        // Build a tiny stub that always returns 500.
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "model unavailable",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let backend =
            LocalLlmBackend::new(LocalLlmConfig::for_daemon(format!("http://{addr}"), "stub"))
                .unwrap();
        let err = backend.extract(&req()).await.unwrap_err();
        assert!(
            err.contains("500"),
            "error string should mention status; got: {err}"
        );
    }

    /// Connect failure (port is bound but doesn't serve) surfaces
    /// as an Err string. The 250ms timeout in `extract` keeps the
    /// audit from hanging.
    #[tokio::test]
    async fn connect_failure_surfaces_as_err() {
        // RFC 5737 reserved address; nothing listens here.
        let cfg = LocalLlmConfig {
            timeout: std::time::Duration::from_millis(250),
            ..LocalLlmConfig::for_daemon("http://192.0.2.1:9999", "stub")
        };
        let backend = LocalLlmBackend::new(cfg).unwrap();
        let err = backend.extract(&req()).await.unwrap_err();
        assert!(
            err.contains("LocalLlmBackend: POST"),
            "error should be tagged with the URL prefix: {err}"
        );
    }

    /// Schema shape sanity: the `response_format` value matches the
    /// OpenAI-compatible json_schema layout. A regression in this
    /// shape would silently disable grammar constraints on the
    /// daemon side.
    #[test]
    fn response_format_schema_has_required_layout() {
        let schema = response_format_schema();
        assert_eq!(schema["type"], "json_schema");
        assert_eq!(schema["json_schema"]["name"], "decision_extractions");
        assert_eq!(schema["json_schema"]["strict"], true);
        let s = &schema["json_schema"]["schema"];
        assert_eq!(s["type"], "object");
        let item_props = &s["properties"]["decisions"]["items"]["properties"];
        let kind_enum = item_props["kind"]["enum"].as_array().unwrap();
        let kinds: Vec<&str> = kind_enum.iter().filter_map(|v| v.as_str()).collect();
        // Schema enum must match the kinds the parser admits.
        for k in &["decision", "deviation", "invariant"] {
            assert!(kinds.contains(k), "schema missing kind {k}: {kinds:?}");
        }
    }

    /// `parse_schema_envelope` rejects shapes that aren't a single
    /// JSON object with a `decisions` array, returning None so the
    /// caller can fall through.
    #[test]
    fn parse_schema_envelope_returns_none_on_unrelated_shapes() {
        assert!(parse_schema_envelope("").is_none());
        assert!(parse_schema_envelope("not json").is_none());
        assert!(parse_schema_envelope("[1,2,3]").is_none());
        assert!(parse_schema_envelope(r#"{"other_key": []}"#).is_none());
    }

    /// Empty body strings inside the array are skipped (matches
    /// `parse_extractions`'s posture).
    #[test]
    fn parse_schema_envelope_skips_empty_body() {
        let raw = r#"{"decisions":[{"kind":"decision","body":""},
                                     {"kind":"decision","body":"keep"}]}"#;
        let out = parse_schema_envelope(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "keep");
    }
}
