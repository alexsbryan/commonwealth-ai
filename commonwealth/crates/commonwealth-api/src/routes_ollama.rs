// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ollama-native `/api/*` compatibility shim.
//!
//! A thin translation layer that lets Ollama-native clients (Open WebUI's
//! Ollama mode, several IDE plugins, Raycast, Enchanted, …) talk to the
//! daemon without changes. Every handler here is a *translator*: it reshapes
//! the Ollama wire format and delegates the actual work to the existing
//! OpenAI-shaped handlers in [`crate::routes_inference`] /
//! [`crate::routes_status`]. There is no new inference, routing, or
//! model-management logic — only request/response shape.
//!
//! ## What's covered
//! - `GET  /api/version`            — version probe (clients use this to detect a server)
//! - `GET  /api/tags`               — list models (reuses `list_models`, liveness-filtered)
//! - `GET  /api/ps`                 — loaded models (reuses `/status`)
//! - `POST /api/show`               — minimal model details
//! - `POST /api/chat`               — chat (reuses `chat_completions`)
//! - `POST /api/generate`           — single-prompt completion (reuses `chat_completions`)
//! - `POST /api/embed`              — batch embeddings (reuses `embeddings`)
//! - `POST /api/embeddings`         — legacy single embedding (reuses `embeddings`)
//!
//! ## Streaming (v1 limitation, documented on purpose)
//! Ollama streams newline-delimited JSON (NDJSON); our inference handler
//! streams Server-Sent Events. Rather than ship a subtly-wrong SSE→NDJSON
//! re-framer, v1 drives the inner handler **non-streaming** and frames the
//! complete answer as Ollama NDJSON: one content frame (`done:false`) then a
//! terminal frame (`done:true`). Clients work correctly; they just receive
//! the answer in one piece rather than token-by-token. True incremental
//! streaming is a tracked follow-up.
//!
//! ## Trust posture (read before exposing this beyond loopback)
//! These routes inherit the `:9741` port's posture: the `client_auth` bearer
//! layer wraps them together with `/v1/*` — loopback callers are exempt, any
//! non-loopback caller must present `Authorization: Bearer <client_token>`,
//! and the layer fails closed when no token can be resolved (see
//! [`crate::client_auth`]). Auth is one shared client token guarding the
//! whole surface, not per-user tenancy. Adding `/api/*` widened no
//! privilege — it routes to the same handlers `/v1/*` already exposes. A
//! permissive CORS layer was deliberately NOT added here: it would silently
//! broaden browser-origin reachability, and "honest disclosure over silent
//! exposure" is the rule. Opt-in CORS for browser clients is a tracked
//! follow-up. The consolidated surface-by-surface posture lives in
//! `docs/THREAT_MODEL.md` at the repo root.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::openai_types::{
    ChatCompletionRequest, ChatCompletionResponse, EmbeddingInput, EmbeddingRequest,
    EmbeddingResponse, ModelListResponse,
};
use crate::routes_inference;
use crate::routes_status;
use crate::state::AppState;

// ─── Small helpers ─────────────────────────────────────────────

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// RFC3339 timestamp for Ollama's `created_at`. Informational for clients,
/// but we emit a real one (glassbox) rather than a placeholder.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Read an axum `Response`'s body to bytes. Reused by every handler that
/// delegates to an inner handler and reshapes its JSON.
async fn body_bytes(resp: Response) -> Result<(StatusCode, bytes::Bytes), Response> {
    let status = resp.status();
    match axum::body::to_bytes(resp.into_body(), usize::MAX).await {
        Ok(b) => Ok((status, b)),
        Err(e) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read inner response body: {e}"),
        )),
    }
}

// ─── Request shapes (only the fields we translate) ─────────────
// serde ignores unknown fields by default, so clients can send the full
// Ollama payload; we pick out what maps cleanly and drop the rest
// (e.g. `images` / `suffix` — vision + fill-in-middle aren't supported).

#[derive(Deserialize)]
pub(crate) struct OllamaOptions {
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    top_k: Option<u32>,
    /// Ollama's max-tokens knob. `-1` means "unbounded" — we drop it then so
    /// the daemon's own default budget applies.
    #[serde(default)]
    num_predict: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct OllamaChatMessage {
    role: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
pub(crate) struct OllamaChatRequest {
    model: String,
    #[serde(default)]
    messages: Vec<OllamaChatMessage>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    options: Option<OllamaOptions>,
    #[serde(default)]
    format: Option<Value>,
    #[serde(default)]
    tools: Option<Value>,
}

#[derive(Deserialize)]
pub(crate) struct OllamaGenerateRequest {
    model: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    options: Option<OllamaOptions>,
    #[serde(default)]
    format: Option<Value>,
}

#[derive(Deserialize)]
pub(crate) struct OllamaShowRequest {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct OllamaEmbeddingsRequest {
    model: String,
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct OllamaEmbedRequest {
    model: String,
    /// String or array of strings.
    input: Value,
}

// ─── Translation: Ollama request → OpenAI ChatCompletionRequest ─

/// Build a `ChatCompletionRequest` from translated parts. We assemble a JSON
/// value and deserialize so serde's field defaults fill everything we don't
/// set — far less brittle than a 20-field struct literal that breaks every
/// time the OpenAI request type grows a field. `stream` is always `false`
/// here: this shim frames the Ollama stream itself (see module docs).
fn build_openai_request(
    model: &str,
    messages: Vec<Value>,
    options: Option<&OllamaOptions>,
    format: Option<&Value>,
    tools: Option<&Value>,
) -> Result<ChatCompletionRequest, String> {
    let mut body = serde_json::Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), json!(messages));
    body.insert("stream".into(), json!(false));

    if let Some(o) = options {
        if let Some(t) = o.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(p) = o.top_p {
            body.insert("top_p".into(), json!(p));
        }
        if let Some(k) = o.top_k {
            body.insert("top_k".into(), json!(k));
        }
        if let Some(n) = o.num_predict {
            if n > 0 {
                body.insert("max_tokens".into(), json!(n));
            }
        }
    }

    // Ollama `format`: the string "json" → OpenAI `json_object`; an object is
    // a JSON Schema → OpenAI `json_schema`. The daemon's in-house constraint
    // sampler enforces whichever lands on `response_format`.
    if let Some(fmt) = format {
        let rf = match fmt {
            Value::String(s) if s == "json" => Some(json!({ "type": "json_object" })),
            Value::Object(_) => Some(json!({
                "type": "json_schema",
                "json_schema": { "name": "response", "schema": fmt }
            })),
            _ => None,
        };
        if let Some(rf) = rf {
            body.insert("response_format".into(), rf);
        }
    }

    if let Some(t) = tools {
        body.insert("tools".into(), t.clone());
    }

    serde_json::from_value(Value::Object(body)).map_err(|e| format!("translate request: {e}"))
}

/// Drive the inner `chat_completions` handler (non-streaming) and frame its
/// complete answer as Ollama output. `generate_mode` puts the text in
/// `response` (for `/api/generate`) vs `message` (for `/api/chat`); `stream`
/// chooses NDJSON-with-a-terminal-frame vs a single JSON object.
async fn run_and_frame(
    state: AppState,
    headers: HeaderMap,
    oai: ChatCompletionRequest,
    model: String,
    want_stream: bool,
    generate_mode: bool,
) -> Response {
    let inner = routes_inference::chat_completions(State(state), headers, Json(oai)).await;
    let (status, raw) = match body_bytes(inner).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    if !status.is_success() {
        // Forward the daemon's error (503 no-model-loaded, 400 local_only, …)
        // under the same status, reshaped to Ollama's `{ "error": … }`.
        let msg = String::from_utf8_lossy(&raw).to_string();
        return (status, Json(json!({ "error": msg }))).into_response();
    }
    let cc: ChatCompletionResponse = match serde_json::from_slice(&raw) {
        Ok(c) => c,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("parse inner response: {e}"),
            )
        }
    };

    let content = cc
        .choices
        .first()
        .map(|c| c.message.content.clone())
        .unwrap_or_default();
    let done_reason = cc
        .choices
        .first()
        .and_then(|c| c.finish_reason.clone())
        .unwrap_or_else(|| "stop".to_string());
    let (prompt_tokens, eval_count) = cc
        .usage
        .map(|u| (u.prompt_tokens, u.completion_tokens))
        .unwrap_or((0, 0));
    let created = now_rfc3339();

    let frame = |text: &str, done: bool| -> Value {
        let mut o = serde_json::Map::new();
        o.insert("model".into(), json!(model));
        o.insert("created_at".into(), json!(created));
        if generate_mode {
            o.insert("response".into(), json!(text));
        } else {
            o.insert(
                "message".into(),
                json!({ "role": "assistant", "content": text }),
            );
        }
        o.insert("done".into(), json!(done));
        if done {
            o.insert("done_reason".into(), json!(done_reason));
            o.insert("prompt_eval_count".into(), json!(prompt_tokens));
            o.insert("eval_count".into(), json!(eval_count));
        }
        Value::Object(o)
    };

    if want_stream {
        // NDJSON: one content frame, then a terminal frame. Non-incremental
        // in v1 (see module docs) but a valid Ollama stream.
        let body = format!("{}\n{}\n", frame(&content, false), frame("", true));
        (
            [(header::CONTENT_TYPE, "application/x-ndjson")],
            body,
        )
            .into_response()
    } else {
        Json(frame(&content, true)).into_response()
    }
}

// ─── Handlers ──────────────────────────────────────────────────

/// `GET /api/version` — clients probe this to confirm an Ollama-shaped server.
pub(crate) async fn version() -> Response {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") })).into_response()
}

/// `GET /api/tags` — list installed/advertised models. Reuses the
/// liveness-filtered `/v1/models` output.
pub(crate) async fn tags(State(state): State<AppState>) -> Response {
    let resp = routes_inference::list_models(State(state)).await.into_response();
    let (status, raw) = match body_bytes(resp).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    if !status.is_success() {
        return (status, raw).into_response();
    }
    let list: ModelListResponse = match serde_json::from_slice(&raw) {
        Ok(l) => l,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("parse models: {e}")),
    };
    let now = now_rfc3339();
    let models: Vec<Value> = list
        .data
        .into_iter()
        .map(|m| {
            json!({
                "name": m.id,
                "model": m.id,
                "modified_at": now,
                "size": 0,
                "digest": "",
                "details": {
                    "family": m.owned_by,
                    "format": "gguf",
                    "parameter_size": "",
                    "quantization_level": ""
                }
            })
        })
        .collect();
    Json(json!({ "models": models })).into_response()
}

/// `GET /api/ps` — currently-loaded models. Reuses `/status`.
pub(crate) async fn ps(State(state): State<AppState>) -> Response {
    let Json(s) = routes_status::status(State(state)).await;
    let models: Vec<Value> = s
        .inference
        .loaded_models
        .into_iter()
        .filter(|m| m.loaded)
        .map(|m| {
            json!({
                "name": m.model,
                "model": m.model,
                "size": 0,
                "digest": "",
                "details": { "family": "", "format": "gguf" },
                "expires_at": "",
                "size_vram": 0
            })
        })
        .collect();
    Json(json!({ "models": models })).into_response()
}

/// `POST /api/show` — minimal model metadata. We don't track per-model
/// Modelfile/template details, so we return a valid-shaped stub with the
/// honest capability we expose (`completion`).
pub(crate) async fn show(State(state): State<AppState>, Json(req): Json<OllamaShowRequest>) -> Response {
    let want = req.model.or(req.name).unwrap_or_default();
    if want.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "missing 'model'");
    }
    // Confirm the model is known (so a typo 404s rather than silently
    // returning a stub for a non-existent model).
    let resp = routes_inference::list_models(State(state)).await.into_response();
    if let Ok((status, raw)) = body_bytes(resp).await {
        if status.is_success() {
            if let Ok(list) = serde_json::from_slice::<ModelListResponse>(&raw) {
                if !list.data.iter().any(|m| m.id == want) {
                    return err(StatusCode::NOT_FOUND, format!("model '{want}' not found"));
                }
            }
        }
    }
    Json(json!({
        "details": {
            "family": "",
            "format": "gguf",
            "parameter_size": "",
            "quantization_level": ""
        },
        "model_info": {},
        "capabilities": ["completion"]
    }))
    .into_response()
}

/// `POST /api/chat` — multi-turn chat.
pub(crate) async fn chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OllamaChatRequest>,
) -> Response {
    // Ollama defaults `stream` to true.
    let want_stream = req.stream.unwrap_or(true);
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    if messages.is_empty() {
        return err(StatusCode::BAD_REQUEST, "no messages");
    }
    let oai = match build_openai_request(
        &req.model,
        messages,
        req.options.as_ref(),
        req.format.as_ref(),
        req.tools.as_ref(),
    ) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    run_and_frame(state, headers, oai, req.model, want_stream, false).await
}

/// `POST /api/generate` — single-prompt completion. The optional `system`
/// becomes a system message; the response text lands in `response`.
pub(crate) async fn generate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OllamaGenerateRequest>,
) -> Response {
    let want_stream = req.stream.unwrap_or(true);
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = req.system.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    messages.push(json!({ "role": "user", "content": req.prompt.unwrap_or_default() }));
    let oai = match build_openai_request(
        &req.model,
        messages,
        req.options.as_ref(),
        req.format.as_ref(),
        None,
    ) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    run_and_frame(state, headers, oai, req.model, want_stream, true).await
}

/// Shared embedding delegation: build an OpenAI `EmbeddingRequest`, call the
/// inner handler, return the parsed `EmbeddingResponse` (or a forwarded error).
async fn run_embeddings(
    state: AppState,
    headers: HeaderMap,
    model: String,
    input: EmbeddingInput,
) -> Result<EmbeddingResponse, Response> {
    let er = EmbeddingRequest {
        model,
        input,
        encoding_format: None,
    };
    let inner = routes_inference::embeddings(State(state), headers, Json(er)).await;
    let (status, raw) = body_bytes(inner).await?;
    if !status.is_success() {
        let msg = String::from_utf8_lossy(&raw).to_string();
        return Err((status, Json(json!({ "error": msg }))).into_response());
    }
    serde_json::from_slice(&raw).map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse embeddings: {e}"),
        )
    })
}

/// `POST /api/embed` — batch embeddings. `input` is a string or array of
/// strings; response is `{ model, embeddings: [[…], …] }`.
pub(crate) async fn embed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OllamaEmbedRequest>,
) -> Response {
    let input = match req.input {
        Value::String(s) => EmbeddingInput::Single(s),
        Value::Array(a) => {
            EmbeddingInput::Batch(a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
        }
        _ => return err(StatusCode::BAD_REQUEST, "'input' must be a string or array of strings"),
    };
    let model = req.model.clone();
    match run_embeddings(state, headers, req.model, input).await {
        Ok(resp) => {
            let embeddings: Vec<Vec<f32>> = resp.data.into_iter().map(|d| d.embedding).collect();
            Json(json!({ "model": model, "embeddings": embeddings })).into_response()
        }
        Err(resp) => resp,
    }
}

/// `POST /api/embeddings` — legacy single-input embedding. Response is the
/// legacy `{ "embedding": [...] }` shape.
pub(crate) async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OllamaEmbeddingsRequest>,
) -> Response {
    let input = EmbeddingInput::Single(req.prompt.unwrap_or_default());
    match run_embeddings(state, headers, req.model, input).await {
        Ok(resp) => {
            let embedding = resp.data.into_iter().next().map(|d| d.embedding).unwrap_or_default();
            Json(json!({ "embedding": embedding })).into_response()
        }
        Err(resp) => resp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_app_state;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn router() -> axum::Router {
        crate::server::mock_router(test_app_state())
    }

    #[tokio::test]
    async fn version_reports_a_version() {
        let resp = router()
            .oneshot(Request::get("/api/version").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["version"].as_str().is_some_and(|v| !v.is_empty()));
    }

    #[tokio::test]
    async fn tags_returns_models_array() {
        let resp = router()
            .oneshot(Request::get("/api/tags").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        // Empty test state → empty (but present) models array.
        assert!(json["models"].is_array());
    }

    #[tokio::test]
    async fn ps_returns_models_array() {
        let resp = router()
            .oneshot(Request::get("/api/ps").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["models"].is_array());
    }

    #[tokio::test]
    async fn chat_forwards_no_model_loaded_as_503() {
        // No model loaded in the test state → inner handler 503s → the shim
        // forwards the status (reshaped to Ollama's error envelope).
        let body = json!({ "model": "x", "messages": [{"role":"user","content":"hi"}] });
        let resp = router()
            .oneshot(
                Request::post("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn chat_rejects_empty_messages() {
        let body = json!({ "model": "x", "messages": [] });
        let resp = router()
            .oneshot(
                Request::post("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_request_translates_options_and_format() {
        let opts = OllamaOptions {
            temperature: Some(0.5),
            top_p: Some(0.9),
            top_k: Some(40),
            num_predict: Some(256),
        };
        let req = build_openai_request(
            "m",
            vec![json!({"role":"user","content":"hi"})],
            Some(&opts),
            Some(&json!("json")),
            None,
        )
        .expect("translate");
        assert_eq!(req.model.as_deref(), Some("m"));
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.max_tokens, Some(256));
        // stream is always forced false — the shim frames Ollama output itself.
        assert_eq!(req.stream, Some(false));
    }

    #[test]
    fn build_request_drops_unbounded_num_predict() {
        let opts = OllamaOptions {
            temperature: None,
            top_p: None,
            top_k: None,
            num_predict: Some(-1),
        };
        let req = build_openai_request("m", vec![json!({"role":"user","content":"x"})], Some(&opts), None, None)
            .expect("translate");
        assert_eq!(req.max_tokens, None, "-1 (unbounded) must not set a max_tokens cap");
    }
}
