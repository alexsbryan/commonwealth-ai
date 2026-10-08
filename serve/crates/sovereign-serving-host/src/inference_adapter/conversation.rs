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

use futures::{Stream, StreamExt};
use oicp_types::chat_turn::{parse_assistant_turn, AssistantTurn, TurnDelta, TurnStream};
use oicp_types::openai_types::{self as wire, ChatCompletionRequest, FunctionCall, ToolCall};
use sovereign_contracts::types::{CompletionRequest, PromptShape};

use super::{
    translate_finish_reason, translate_stream_frame, translate_stream_usage, CoreStreamFrame,
};

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

/// Whether `req` was made a conversation by [`into_conversation`].
pub(crate) fn is_conversation(req: &CompletionRequest) -> bool {
    matches!(req.prompt_shape, Some(PromptShape::Conversation { .. }))
}

/// The reply as llama-server returns it: reasoning apart, content without
/// call markup, calls parsed in the model's own format. The engine starts
/// the text with the `<think>` its prompt left open, so the text alone says
/// whether the turn opens in reasoning.
pub(crate) fn parse_reply(text: &str, req: &CompletionRequest) -> AssistantTurn {
    parse_assistant_turn(text, req.tools.as_deref().unwrap_or(&[]))
}

/// A conversation's turn as llama-server streams it: reasoning as
/// `reasoning_content` deltas, the reply as `content` deltas, each call
/// whole once its block closes, then `tool_calls` as the finish reason when
/// any call went out. Every piece is parsed by [`TurnStream`], the
/// streaming face of [`parse_reply`], so a streamed turn is the turn the
/// non-streaming path returns.
pub(crate) fn stream_turn(
    inner: impl Stream<Item = CoreStreamFrame> + Send + 'static,
    req: &CompletionRequest,
) -> impl Stream<Item = wire::StreamFrame> + Send + 'static {
    let mut state = TurnFrames {
        turn: Some(TurnStream::new(req.tools.clone().unwrap_or_default())),
        calls: 0,
        stamp: sovereign_time::unix_millis(),
    };
    inner.flat_map(move |frame| futures::stream::iter(state.on(frame)))
}

struct TurnFrames {
    /// `None` once the turn has finished.
    turn: Option<TurnStream>,
    calls: usize,
    stamp: u64,
}

impl TurnFrames {
    fn on(&mut self, frame: CoreStreamFrame) -> Vec<wire::StreamFrame> {
        match frame {
            CoreStreamFrame::Token(piece) => match self.turn.as_mut() {
                Some(turn) => {
                    let deltas = turn.push(&piece);
                    self.frames(deltas)
                }
                None => {
                    tracing::warn!(
                        "inference_adapter:conversation_stream token after finish dropped"
                    );
                    Vec::new()
                }
            },
            CoreStreamFrame::Finish { reason, usage } => {
                let Some(turn) = self.turn.take() else {
                    return Vec::new();
                };
                let end = turn.finish();
                for raw in &end.turn.unparsed {
                    tracing::warn!(payload = %raw, "inference adapter: tool_call parse failed");
                }
                if end.diverged > 0 {
                    tracing::warn!(
                        diverged = end.diverged,
                        "inference_adapter:conversation_stream sent text the finished parse does not contain"
                    );
                }
                let mut out = self.frames(end.deltas);
                tracing::debug!(
                    tool_calls = self.calls,
                    parse_errors = end.turn.unparsed.len(),
                    reasoning_chars = end.turn.reasoning_content.as_deref().map_or(0, str::len),
                    content_chars = end.turn.content.len(),
                    "sovereign inference adapter: conversation stream served"
                );
                // As the non-streaming path decides: calls outrank the
                // engine's reason, which is otherwise reported as observed.
                let reason = if self.calls > 0 {
                    wire::FinishReason::ToolCalls
                } else {
                    translate_finish_reason(reason)
                };
                out.push(wire::StreamFrame::Finish {
                    reason,
                    usage: usage.map(translate_stream_usage),
                });
                out
            }
            error @ CoreStreamFrame::Error(_) => vec![translate_stream_frame(error)],
        }
    }

    fn frames(&mut self, deltas: Vec<TurnDelta>) -> Vec<wire::StreamFrame> {
        deltas
            .into_iter()
            .map(|d| match d {
                TurnDelta::Reasoning(text) => wire::StreamFrame::Reasoning(text),
                TurnDelta::Content(text) => wire::StreamFrame::Token(text),
                TurnDelta::ToolCall(call) => {
                    let id = format!("call_{}_{}", self.stamp, self.calls);
                    self.calls += 1;
                    wire::StreamFrame::ToolCalls(vec![ToolCall {
                        id,
                        kind: "function".into(),
                        function: FunctionCall {
                            name: call.name,
                            arguments: call.arguments,
                        },
                    }])
                }
            })
            .collect()
    }
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

    fn streamed(text: &str, tools: bool) -> Vec<wire::StreamFrame> {
        let r = request(if tools {
            serde_json::json!({"tools": [{"type": "function", "function": {"name": "read", "parameters": {"type": "object", "properties": {"filePath": {"type": "string"}}}}}]})
        } else {
            serde_json::json!({})
        });
        let mut req = CompletionRequest::new("flattened");
        into_conversation(&mut req, &r);
        if tools {
            req.tools = Some(vec![sovereign_contracts::types::ToolSchema {
                name: "read".into(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {"filePath": {"type": "string"}}}),
            }]);
        }
        // Pieces of a few chars, the way a decode loop hands them over.
        let chars: Vec<char> = text.chars().collect();
        let mut frames: Vec<CoreStreamFrame> = chars
            .chunks(3)
            .map(|c| CoreStreamFrame::Token(c.iter().collect()))
            .collect();
        frames.push(CoreStreamFrame::Finish {
            reason: sovereign_contracts::types::FinishReason::Stop,
            usage: None,
        });
        futures::executor::block_on(stream_turn(futures::stream::iter(frames), &req).collect())
    }

    /// Phase 2 (note d25c3a96): a thinking turn with a call streams its
    /// reasoning and reply as they are written, the call whole, and
    /// finishes `tool_calls`, as llama-server streams it.
    #[test]
    fn a_conversation_streams_reasoning_reply_and_calls_apart() {
        let text = "<think>\nRead it first.\n</think>\n\nOn it.\n<tool_call>\n<function=read>\n<parameter=filePath>\nx.py\n</parameter>\n</function>\n</tool_call>";
        let frames = streamed(text, true);
        let (mut reasoning, mut content, mut calls) = (String::new(), String::new(), Vec::new());
        let mut first_reasoning_at = None;
        for (i, f) in frames.iter().enumerate() {
            match f {
                wire::StreamFrame::Reasoning(r) => {
                    first_reasoning_at.get_or_insert(i);
                    reasoning.push_str(r)
                }
                wire::StreamFrame::Token(c) => content.push_str(c),
                wire::StreamFrame::ToolCalls(c) => calls.extend(c.iter().cloned()),
                _ => {}
            }
        }
        assert_eq!(reasoning, "Read it first.");
        assert_eq!(content, "On it.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments, r#"{"filePath":"x.py"}"#);
        assert!(
            frames.len() > 4,
            "streamed in pieces, not one blob: {frames:?}"
        );
        assert!(matches!(
            frames.last(),
            Some(wire::StreamFrame::Finish {
                reason: wire::FinishReason::ToolCalls,
                ..
            })
        ));
    }

    /// P2-5 at the adapter: a turn whose prompt opened no think block is
    /// the reply, and finishes as the engine said.
    #[test]
    fn a_turn_without_a_think_block_streams_as_content() {
        let frames = streamed("Paris is the capital of France.", false);
        let content: String = frames
            .iter()
            .filter_map(|f| match f {
                wire::StreamFrame::Token(c) => Some(c.as_str()),
                wire::StreamFrame::Reasoning(r) => {
                    panic!("reasoning {r:?} from a turn with no think block")
                }
                _ => None,
            })
            .collect();
        assert_eq!(content, "Paris is the capital of France.");
        assert!(matches!(
            frames.last(),
            Some(wire::StreamFrame::Finish {
                reason: wire::FinishReason::Stop,
                ..
            })
        ));
    }
}
