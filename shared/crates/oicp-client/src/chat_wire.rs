// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat-completions fields that more than one host spells differently:
//! structured output and thinking. Written here, once, so a mesh hop to a
//! peer daemon and enrich's dispatch to a vendor say the same thing the same
//! way ([`crate::RemoteApiProvider::build_request`] is their one builder).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How a host is asked for output that matches a JSON Schema. A property of
/// the HOST, not of the request: the request carries the schema, and the
/// provider spells it the way its host accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuredOutputMode {
    /// `response_format: {type: "json_schema", json_schema: {...}}`. The host
    /// enforces the schema (a Commonwealth daemon, OpenAI).
    #[default]
    JsonSchema,
    /// `response_format: {type: "json_object"}`: valid JSON, schema not
    /// enforced. The OICP-compliant spelling for a host that advertises
    /// `constraint:json_object` and not `constraint:json_schema`.
    JsonObject,
    /// The schema as the one function's `parameters`, `tool_choice: "auto"`:
    /// the model may answer in text instead.
    ToolUseAuto,
    /// The schema as the one function's `parameters`, the call forced. On a
    /// host with no schema-enforcing `response_format` this is the strongest
    /// adherence there is: DeepSeek's chat API (2026-10-03) answers
    /// `json_schema` with 400, and under `json_object` 15 of 20 wessex-hoard
    /// Phase 1 chapters dropped the required `questions_raised`.
    ToolUseForced,
}

impl StructuredOutputMode {
    /// True when the answer comes back as a function call's arguments rather
    /// than as the message content.
    pub fn via_tool(self) -> bool {
        matches!(self, Self::ToolUseAuto | Self::ToolUseForced)
    }
}

/// Write `schema` onto `body` in `mode`'s spelling, named after the schema's
/// own `title` (JSON Schema's annotation for exactly that, which a grammar
/// ignores) folded to the alphabet OpenAI accepts for a schema or function
/// name. Untitled, it is `structured`.
pub(crate) fn write_structured_output(
    body: &mut Value,
    schema: &Value,
    mode: StructuredOutputMode,
) {
    let title = schema.get("title").and_then(Value::as_str);
    let name = openai_function_name(title.unwrap_or("structured"));
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
