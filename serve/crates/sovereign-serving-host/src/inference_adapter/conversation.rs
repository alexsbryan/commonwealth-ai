// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OpenAI chat path as llama-server serves it (note fb4d2489): an
//! external client's conversation reaches the engine whole, as
//! `PromptShape::Conversation`, the model's own template renders it, and the
//! reply is parsed in the model's own tool-call format
//! (`oicp_types::chat_turn`).
//!
//! Which requests take it is one decision, [`hermes_path_reason`]. The other
//! path — flatten into one user turn, the daemon's Hermes tool block and
//! envelope grammar — stays for the in-repo callers built on it: every
//! request a Commonwealth caller sends carries an OICP envelope
//! (`RemoteApiProvider::build_request_in` attaches one for every daemon or
//! peer far end), and the daemon's own extensions assume that envelope.

use oicp_types::chat_turn::{parse_assistant_turn, AssistantTurn};
use oicp_types::openai_types::ChatCompletionRequest;
use sovereign_contracts::types::{CompletionRequest, PromptShape};

/// Why `request` keeps the single-turn Hermes path, or `None` when it is
/// an external client's conversation for the model's own template.
pub(crate) fn hermes_path_reason(request: &ChatCompletionRequest) -> Option<&'static str> {
    let forced_tool_choice = match &request.tool_choice {
        Some(serde_json::Value::String(s)) => s == "required",
        Some(serde_json::Value::Object(_)) => true,
        _ => false,
    };
    if request.oicp.is_some() {
        Some("oicp envelope (in-repo caller)")
    } else if request.lark_grammar.is_some() {
        Some("lark_grammar")
    } else if request.assistant_prefix.is_some() {
        Some("assistant_prefix")
    } else if request.cmd_prefix.is_some() {
        Some("cmd_prefix")
    } else if request.sampling_mode.is_some() {
        Some("sampling_mode")
    } else if request.think_budget.is_some() {
        Some("think_budget")
    } else if request.stable_prefix_len.is_some() {
        Some("stable_prefix_len")
    } else if forced_tool_choice {
        // A forced call is enforced by the Hermes envelope grammar; the
        // model-native equivalent (a lazy grammar) does not exist yet.
        Some("tool_choice forces a call")
    } else if super::force_tool_calls_env() {
        Some("SOVEREIGN_FORCE_TOOL_CALLS")
    } else if super::alternation_grammar_enabled() {
        Some("SOVEREIGN_ALTERNATION_GRAMMAR")
    } else {
        None
    }
}

/// Make `req` a conversation: the client's messages go to the engine whole
/// and none of the Hermes path's settings apply. `req.prompt` keeps the
/// flattened text for token estimators and peers that predate the shape.
pub(crate) fn into_conversation(req: &mut CompletionRequest, request: &ChatCompletionRequest) {
    let messages = request
        .messages
        .iter()
        // A derived Serialize over strings and options cannot fail.
        .map(|m| serde_json::to_value(m).expect("a ChatMessage serializes to JSON"))
        .collect();
    req.prompt_shape = Some(PromptShape::Conversation { messages });
    req.think_budget = None;
}

/// The reply as llama-server returns it: reasoning apart, content without
/// call markup, calls parsed in the model's own format.
pub(crate) fn parse_reply(text: &str, req: &CompletionRequest) -> AssistantTurn {
    // Thinking the client did not set follows the template, which is on
    // for the thinking-capable templates this path serves; a `</think>` in
    // the text decides on its own either way.
    let thinking_open = req.enable_thinking.unwrap_or(true);
    let tools = req.tools.as_deref().unwrap_or(&[]);
    parse_assistant_turn(text, thinking_open, tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(extra: serde_json::Value) -> ChatCompletionRequest {
        let mut body = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(body).unwrap()
    }

    #[test]
    fn an_external_client_conversation_takes_the_template_path() {
        let r = request(serde_json::json!({
            "tools": [{"type": "function", "function": {"name": "read", "parameters": {}}}],
            "max_tokens": 16384
        }));
        assert_eq!(hermes_path_reason(&r), None);
    }

    #[test]
    fn in_repo_callers_keep_the_hermes_path() {
        for (extra, why) in [
            (
                serde_json::json!({"oicp": sovereign_contracts::oicp::InferenceRequirements::new()}),
                "oicp envelope (in-repo caller)",
            ),
            (
                serde_json::json!({"lark_grammar": "start: x"}),
                "lark_grammar",
            ),
            (serde_json::json!({"think_budget": 0}), "think_budget"),
            (
                serde_json::json!({"tool_choice": "required"}),
                "tool_choice forces a call",
            ),
        ] {
            assert_eq!(hermes_path_reason(&request(extra)), Some(why));
        }
    }

    #[test]
    fn a_conversation_carries_every_message_and_no_think_budget() {
        let r = request(serde_json::json!({
            "messages": [
                {"role": "system", "content": "s"},
                {"role": "user", "content": "u"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"filePath\":\"x\"}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "data"}
            ]
        }));
        let mut req = CompletionRequest::new("flattened");
        req.think_budget = Some(0);
        into_conversation(&mut req, &r);
        let Some(PromptShape::Conversation { messages }) = &req.prompt_shape else {
            panic!("not a conversation: {:?}", req.prompt_shape);
        };
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(req.think_budget, None);
        assert_eq!(req.prompt, "flattened");
    }
}
