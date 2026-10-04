// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat-completions fields that more than one host spells differently:
//! structured output and thinking. Written here, once, so a mesh hop to a
//! peer daemon and enrich's dispatch to a vendor say the same thing the same
//! way ([`crate::RemoteApiProvider::build_request`] is their one builder).

use serde_json::{json, Value};
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::CompletionRequest;
use std::sync::atomic::Ordering;

use crate::{error_excerpt, ChatMessage, RemoteApiProvider};

pub use sovereign_contracts::setup_config::StructuredOutputMode;

/// Write `schema` onto `body` in `mode`'s spelling, named after the schema's
/// own `title` (JSON Schema's annotation for exactly that, which a grammar
/// ignores) folded to the alphabet OpenAI accepts for a schema or function
/// name. Untitled, it is `structured`.
pub(crate) fn write_structured_output(
    body: &mut Value,
    schema: &Value,
    mode: StructuredOutputMode,
) {
    let name = schema_function_name(schema);
    match mode {
        StructuredOutputMode::JsonSchema => {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {"name": name, "schema": schema, "strict": true},
            });
        }
        StructuredOutputMode::JsonObject => {
            body["response_format"] = json!({"type": "json_object"});
        }
        StructuredOutputMode::ToolUseAuto | StructuredOutputMode::ToolUseForced => {
            let function = json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": "Return the structured result.",
                    "parameters": schema,
                }
            });
            // Beside any tools the caller already offered, never instead of them.
            match body.get_mut("tools").and_then(Value::as_array_mut) {
                Some(tools) => tools.push(function),
                None => body["tools"] = json!([function]),
            }
            body["tool_choice"] = if mode == StructuredOutputMode::ToolUseForced {
                // A forced call and thinking do not ride together: DeepSeek
                // answers 400 and Anthropic refuses the pair. Every spelling
                // off, whatever the request asked for.
                write_thinking(body, Some(0), Some(false));
                json!({"type": "function", "function": {"name": name}})
            } else {
                json!("auto")
            };
        }
    }
}

/// Put every spelling of "think this much" on the body that some
/// OpenAI-compatible host reads. There are three, and a host reads ONE of
/// them; the other two are unknown fields it drops on the floor:
///
/// - `think_budget`: the Commonwealth daemon's native field
///   (`resolve_think_budget`). `0` makes the daemon inject `/no_think` for
///   SystemPromptToken families (Qwen3 / Qwen3.5 / SmolLM3).
/// - `thinking: {type: disabled|enabled}`: DeepSeek V3.1+ / V4.
/// - `chat_template_kwargs: {enable_thinking: bool}`: llama-server, vLLM and
///   SGLang hand the map to the Jinja chat template, and the Qwen3 templates
///   read exactly this key. It is the ONLY one of the three a bare
///   llama-server understands, and it is written from `enable_thinking`, not
///   derived from the budget: a positive budget says nothing to a template
///   that ships thinking off.
///
/// The third was missing on enrich's path until 2026-09-08, and the
/// bare-endpoint acceptance (`svrn/crates/corpus-mcp/acceptance.sh`, a
/// Qwen3.6-35B behind a plain llama-server) paid for it on every phase: all
/// 20 wessex-hoard chapters failed Phase 1 as `think_truncated`, phases 3 and
/// 6 returned 0 of 4 and 0 of 164, and the run spent 201,596 completion tokens
/// where the daemon spent 17,940.
pub(crate) fn write_thinking(
    body: &mut Value,
    think_budget: Option<usize>,
    enable_thinking: Option<bool>,
) {
    if let Some(tb) = think_budget {
        body["think_budget"] = json!(tb);
        body["thinking"] = if tb == 0 {
            json!({"type": "disabled"})
        } else {
            json!({"type": "enabled", "budget_tokens": tb})
        };
    }
    if let Some(enable) = enable_thinking {
        body["chat_template_kwargs"] = json!({ "enable_thinking": enable });
    }
}

/// `schema` with `title` set to `label`, so a host that takes the schema as a
/// function call names the function after it. No label, no change.
pub fn titled_schema(schema: &Value, label: Option<&str>) -> Value {
    let mut out = schema.clone();
    if let (Some(label), Some(obj)) = (label, out.as_object_mut()) {
        obj.insert("title".into(), json!(label));
    }
    out
}

/// An OpenAI function or schema name must match `^[a-zA-Z0-9_-]{1,64}$`; a
/// schema label is free text, so fold it rather than let the host 400 on it.
pub fn openai_function_name(name: &str) -> String {
    let folded: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if folded.is_empty() {
        "emit_response".to_string()
    } else {
        folded
    }
}

/// The name a schema goes by on the wire: its `title`, folded.
fn schema_function_name(schema: &Value) -> String {
    let title = schema.get("title").and_then(Value::as_str);
    openai_function_name(title.unwrap_or("structured"))
}

impl RemoteApiProvider {
    /// The host's spelling now: as configured, or a forced function call once
    /// the host has refused `json_schema` and answered that instead.
    pub(crate) fn effective_structured_output_mode(&self) -> StructuredOutputMode {
        if self.json_schema_refused.load(Ordering::Relaxed) {
            StructuredOutputMode::ToolUseForced
        } else {
            self.structured_output_mode
        }
    }

    /// POST one chat completion in this host's spelling, and say which
    /// spelling answered.
    ///
    /// A host that rejects `json_schema` (DeepSeek's chat API answers 400
    /// "unavailable now") is asked again, once, with the schema as a forced
    /// function call. Only a retry that ANSWERS flips the provider for the rest
    /// of its life: a 400 for another reason (an oversized prompt) fails the
    /// retry too, and both refusals are reported. Streaming does not retry; it
    /// follows the flip once a completion has learned it.
    ///
    /// Only a LAST-RESORT endpoint is asked again (`wait_out_sheds`, the same
    /// persist-or-report decision as a shed). A peer's refusal is a routing
    /// signal and goes back to the router at once, unchanged.
    pub(crate) async fn send_chat(
        &self,
        admitted: &crate::outbound::Admitted<'_>,
        url: &str,
        request: &CompletionRequest,
    ) -> Result<(reqwest::Response, StructuredOutputMode)> {
        let what = "Remote API request";
        let mode = self.effective_structured_output_mode();
        let body = self.build_request_in(request, mode);
        // The whole body, so a run can be replayed by hand (§9.1). Debug: it
        // carries the full prompt and any schema.
        tracing::debug!(target: "oicp_client", %url, %body, "chat request body");
        let send = |b: &Value| admitted.post(url).json(b);
        let refusal = match self.send_honouring_shed_raw(|| send(&body), what).await? {
            Ok(response) => return Ok((response, mode)),
            Err(refusal) => refusal,
        };
        let schema_refused = self.wait_out_sheds
            && request.structured_output.is_some()
            && mode == StructuredOutputMode::JsonSchema
            && refusal.status == reqwest::StatusCode::BAD_REQUEST;
        if !schema_refused {
            return Err(refusal.into_error(what));
        }
        let forced = StructuredOutputMode::ToolUseForced;
        let retry = self.build_request_in(request, forced);
        tracing::info!(
            target: "oicp_client",
            %url,
            refusal = %error_excerpt(&refusal.body),
            "host refused a json_schema request (400); asking once more with the schema as a forced function call"
        );
        tracing::debug!(target: "oicp_client", %url, body = %retry, "chat request body");
        match self.send_honouring_shed_raw(|| send(&retry), what).await? {
            Ok(response) => {
                self.json_schema_refused.store(true, Ordering::Relaxed);
                tracing::warn!(
                    target: "oicp_client",
                    %url,
                    "json_schema refused, forced function call answered: this provider sends schemas as a function call from now on"
                );
                Ok((response, forced))
            }
            Err(second) => Err(Error::Inference(format!(
                "{what} returned {}: {}; asked again with the schema as a forced function call, it returned {}: {}",
                refusal.status,
                error_excerpt(&refusal.body),
                second.status,
                error_excerpt(&second.body)
            ))),
        }
    }
}

impl ChatMessage {
    /// The assistant turn as the caller's answer. A schema that rode a
    /// function call comes back as that call's arguments, the JSON the caller
    /// asked for whichever spelling the host needed; anything else is
    /// [`ChatMessage::as_text`].
    pub(crate) fn answer(&self, request: &CompletionRequest, mode: StructuredOutputMode) -> String {
        let Some(schema) = request
            .structured_output
            .as_ref()
            .filter(|_| mode.via_tool())
        else {
            return self.as_text();
        };
        let name = schema_function_name(schema);
        let call = self.tool_calls.iter().find(|c| c.function.name == name);
        if let Some(args) = call.and_then(|c| c.function.arguments.clone()) {
            return args;
        }
        // `tool_choice: auto` lets the model answer in text. Use it, and say
        // so: the schema was offered, not enforced.
        tracing::warn!(
            target: "oicp_client",
            function = %name,
            ?mode,
            "schema offered as a function call but the model answered in text; schema not enforced on this call"
        );
        self.as_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]})
    }

    /// THE FAILING INPUT for the third spelling: the body a bare llama-server
    /// received before 2026-09-08 carried `think_budget` and `thinking`, neither
    /// of which it reads, and no `chat_template_kwargs`, so a Qwen3 template
    /// thought through the whole output budget under a JSON grammar and returned
    /// an empty `content`.
    #[test]
    fn a_zero_budget_is_spelled_for_llama_server_too() {
        let mut body = json!({});
        write_thinking(&mut body, Some(0), Some(false));
        assert_eq!(body["think_budget"], json!(0));
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert_eq!(
            body["chat_template_kwargs"],
            json!({"enable_thinking": false}),
            "the one spelling llama-server / vLLM / SGLang read"
        );
    }

    /// A positive budget opts in where a host has a budget notion and says
    /// nothing to the chat template: the template's default stands.
    #[test]
    fn a_positive_budget_leaves_the_template_default_alone() {
        let mut body = json!({});
        write_thinking(&mut body, Some(2048), None);
        assert_eq!(body["think_budget"], json!(2048));
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 2048})
        );
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "no forced enable_thinking on a model whose template ships it off"
        );
    }

    #[test]
    fn json_schema_is_strict_and_named_in_the_openai_alphabet() {
        let mut body = json!({});
        write_structured_output(
            &mut body,
            &titled_schema(&schema(), Some("phase 1 (atlas)")),
            StructuredOutputMode::JsonSchema,
        );
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["title"], "phase 1 (atlas)",
            "the label travels as the schema's own annotation"
        );
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "phase_1__atlas_"
        );
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn json_object_drops_the_schema_from_the_wire() {
        let mut body = json!({});
        write_structured_output(&mut body, &schema(), StructuredOutputMode::JsonObject);
        assert_eq!(body["response_format"], json!({"type": "json_object"}));
    }

    #[test]
    fn a_forced_tool_carries_the_schema_and_forces_the_call() {
        let mut body = json!({});
        write_structured_output(
            &mut body,
            &titled_schema(&schema(), Some("s")),
            StructuredOutputMode::ToolUseForced,
        );
        assert!(body.get("response_format").is_none());
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["required"],
            json!(["q"])
        );
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "function": {"name": "s"}})
        );
    }

    /// THE FAILING INPUT: DeepSeek answers a forced `tool_choice` in thinking
    /// mode with 400 ("Thinking mode does not support this tool_choice"), and
    /// Anthropic's API refuses the same pair. A schema forced as a function
    /// therefore goes out with thinking off in every spelling, whatever the
    /// request asked for.
    #[test]
    fn a_forced_function_turns_thinking_off() {
        let mut body = json!({});
        write_thinking(&mut body, Some(2048), Some(true));
        write_structured_output(&mut body, &schema(), StructuredOutputMode::ToolUseForced);
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert_eq!(body["think_budget"], json!(0));
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            json!(false)
        );
    }

    /// A caller's own tool catalog survives: the schema function is added beside
    /// it, not written over it.
    #[test]
    fn a_tool_mode_appends_to_the_callers_tools() {
        let mut body = json!({"tools": [{"type": "function", "function": {"name": "lookup"}}]});
        write_structured_output(
            &mut body,
            &titled_schema(&schema(), Some("s")),
            StructuredOutputMode::ToolUseAuto,
        );
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["lookup", "s"]);
        assert_eq!(body["tool_choice"], "auto");
    }

    /// A host that answers every chat request 400 and counts them.
    async fn refusing_host() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let chat = move || {
            counter.fetch_add(1, SeqCst);
            async {
                let refusal =
                    r#"{"error":{"message":"This response_format type is unavailable now"}}"#;
                (axum::http::StatusCode::BAD_REQUEST, refusal)
            }
        };
        let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}/v1"), hits)
    }

    /// DeepSeek's chat API in miniature: 400 to a `json_schema` request, a
    /// forced function call answered.
    async fn schema_refusing_host() -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let chat = move |axum::Json(body): axum::Json<Value>| {
            log.lock().unwrap().push(body.clone());
            async move {
                if body.get("response_format").is_some() {
                    let refusal = json!({"error": {"message": "This response_format type is unavailable now"}});
                    return (axum::http::StatusCode::BAD_REQUEST, axum::Json(refusal));
                }
                let name = body["tool_choice"]["function"]["name"].clone();
                let call = json!({"id": "c1", "type": "function",
                    "function": {"name": name, "arguments": "{\"q\":\"why?\"}"}});
                let reply = json!({"choices": [{"message": {"content": null, "tool_calls": [call]},
                    "finish_reason": "tool_calls"}]});
                (axum::http::StatusCode::OK, axum::Json(reply))
            }
        };
        let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}/v1"), seen)
    }

    /// THE FAILING INPUT: a vendor on the default `json_schema` that refuses
    /// it. Before 2026-10-03 the only way through was a per-provider
    /// `structured_output_mode = "tool-use-forced"`. Now the refusal is asked
    /// once more as a forced function call, the answer is the call's
    /// arguments, and the provider remembers: the next call goes that way.
    #[tokio::test]
    async fn a_refused_json_schema_is_asked_again_as_a_function_and_remembered() {
        use sovereign_contracts::traits::InferenceProvider;
        let (url, seen) = schema_refusing_host().await;
        let request = CompletionRequest {
            prompt: "p".into(),
            model_id: Some("m".into()),
            structured_output: Some(titled_schema(&schema(), Some("phase 1 (atlas)"))),
            ..Default::default()
        };
        let host = RemoteApiProvider::new(&url, None, "m", 0).waiting_out_sheds();
        assert_eq!(
            host.complete(&request).await.unwrap().text,
            r#"{"q":"why?"}"#
        );
        assert_eq!(
            host.complete(&request).await.unwrap().text,
            r#"{"q":"why?"}"#
        );

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            3,
            "one refusal, one retry, one call straight to the function"
        );
        assert_eq!(seen[0]["response_format"]["type"], "json_schema");
        for (i, body) in seen.iter().enumerate().skip(1) {
            assert!(body.get("response_format").is_none(), "body {i}: {body}");
            assert_eq!(body["tool_choice"]["function"]["name"], "phase_1__atlas_");
        }
    }

    /// A peer's refusal is a routing signal: reported at once, never re-asked.
    /// Only a last-resort endpoint gets the forced-function retry.
    #[tokio::test]
    async fn only_a_last_resort_host_is_asked_again() {
        use sovereign_contracts::traits::InferenceProvider;
        use std::sync::atomic::Ordering::SeqCst;
        let (url, hits) = refusing_host().await;
        let request = CompletionRequest {
            prompt: "p".into(),
            model_id: Some("m".into()),
            structured_output: Some(schema()),
            ..Default::default()
        };
        let peer = RemoteApiProvider::new(&url, None, "m", 0);
        assert!(peer.complete(&request).await.is_err());
        assert_eq!(hits.load(SeqCst), 1, "a peer is not re-asked");

        let last_resort = RemoteApiProvider::new(&url, None, "m", 0).waiting_out_sheds();
        let err = last_resort
            .complete(&request)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(hits.load(SeqCst), 3, "the last resort is asked once more");
        assert!(
            err.contains("forced function call"),
            "both refusals named: {err}"
        );
    }

    #[test]
    fn an_untitled_schema_is_named_structured() {
        let mut body = json!({});
        write_structured_output(&mut body, &schema(), StructuredOutputMode::JsonSchema);
        assert_eq!(body["response_format"]["json_schema"]["name"], "structured");
    }

    #[test]
    fn an_unfoldable_name_still_names_a_function() {
        assert_eq!(openai_function_name(""), "emit_response");
        assert_eq!(openai_function_name(&"x".repeat(80)).len(), 64);
    }

    fn provider() -> crate::RemoteApiProvider {
        crate::RemoteApiProvider::new("http://127.0.0.1:9/v1", None, "m", 0)
    }

    fn named_request() -> sovereign_contracts::types::CompletionRequest {
        sovereign_contracts::types::CompletionRequest {
            prompt: "p".into(),
            model_id: Some("m".into()),
            ..Default::default()
        }
    }

    /// THE FAILING INPUT: enrich's calls to its own daemon, sent as forwards,
    /// would carry `forward_budget: 0` (an unstated budget is one hop, and a
    /// forward spends it), so the daemon could never hand them to a peer.
    #[test]
    fn an_originating_request_spends_no_hop_and_synthesizes_no_envelope() {
        let forwarded = provider().build_request(&named_request());
        assert_eq!(
            forwarded["oicp"]["forward_budget"], 0,
            "a forward spends the hop"
        );

        let origin = provider().originating();
        assert!(origin.build_request(&named_request()).get("oicp").is_none());

        let mut composed = named_request();
        composed.oicp = Some(
            sovereign_contracts::oicp::InferenceRequirements::new()
                .with_latency_class(sovereign_contracts::oicp::LatencyClass::Fast)
                .with_max_output_tokens(512),
        );
        let body = origin.build_request(&composed);
        assert_eq!(body["oicp"]["latency_class"], "fast");
        assert_eq!(body["oicp"]["max_output_tokens"], 512);
        assert!(
            body["oicp"].get("forward_budget").is_none(),
            "the caller's envelope crosses verbatim: {body}"
        );
    }

    #[test]
    fn extra_params_land_last_and_can_override() {
        let p = provider().with_extra_params(Some(json!({
            "provider": {"require_parameters": true},
            "temperature": 0.9,
        })));
        let mut req = named_request();
        req.temperature = Some(0.2);
        let body = p.build_request(&req);
        assert_eq!(body["provider"], json!({"require_parameters": true}));
        assert_eq!(body["temperature"], 0.9);
    }

    #[test]
    fn the_hosts_mode_spells_the_requests_schema() {
        let p = provider().with_structured_output_mode(StructuredOutputMode::ToolUseForced);
        let mut req = named_request();
        req.structured_output = Some(titled_schema(&schema(), Some("phase 1 (atlas)")));
        let body = p.build_request(&req);
        assert!(body.get("response_format").is_none(), "{body}");
        assert_eq!(body["tools"][0]["function"]["name"], "phase_1__atlas_");
        assert_eq!(body["tool_choice"]["function"]["name"], "phase_1__atlas_");
    }
}
