// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared primitives for text-protocol agentic tool loops.
//!
//! Both the recipe-author workspace loop ([`runtime::handlers::recipe_author`])
//! and the delegate context-firewall worker ([`executor::Executor::execute_delegate`])
//! drive a model that emits `<tool_call>{"name":..,"arguments":{..}}</tool_call>`
//! envelopes interleaved with prose, expose the scoped tools via
//! [`CompletionRequest::tools`](crate::types::CompletionRequest), and feed each
//! tool's result back into the next iteration's transcript. These are the pieces
//! the two loops share — one parser, one schema projection, one result
//! formatter — extracted so they can't drift. (This is distinct from the
//! search-shaped `<tool_call>{"tool","query"}` loop in `executor.rs`, which
//! discards rich params and so can't drive an actuator.)

use serde_json::Value as JsonValue;

use crate::types::{StepOutput, ToolDescriptor, ToolSchema};

/// A rich tool call parsed from assistant text: a tool `name` plus its full
/// `arguments` object (unlike the search loop's `{query}` shorthand).
#[derive(Debug, Clone)]
pub(crate) struct ParsedToolCall {
    pub name: String,
    pub arguments: JsonValue,
}

/// Strip `<think>...</think>` blocks and extract any
/// `<tool_call>{...}</tool_call>` envelopes from an assistant turn.
/// Returns `(visible_text, parsed_calls)`. Visible text has the tool
/// envelopes removed so the next iteration's transcript carries the
/// model's prose explanation without the JSON.
pub(crate) fn parse_assistant_text(text: &str) -> (String, Vec<ParsedToolCall>) {
    let stripped = strip_think_block(text);
    let mut calls = Vec::new();
    let mut clean = String::with_capacity(stripped.len());
    let mut cursor = 0usize;
    while let Some(start_rel) = stripped[cursor..].find("<tool_call>") {
        let start = cursor + start_rel;
        clean.push_str(&stripped[cursor..start]);
        let inner_start = start + "<tool_call>".len();
        match stripped[inner_start..].find("</tool_call>") {
            Some(end_rel) => {
                let body = &stripped[inner_start..inner_start + end_rel];
                if let Some(parsed) = parse_tool_call_body(body) {
                    calls.push(parsed);
                }
                cursor = inner_start + end_rel + "</tool_call>".len();
            }
            // No closing tag. A grammar that satisfies on the tool envelope's
            // final `}` lets the model stop before emitting `</tool_call>` — seen
            // on daemon-routed authoring turns, where the envelope JSON is complete
            // but the wrapper isn't. Recover by extracting the balanced JSON object
            // right after the opener rather than discarding a valid tool call.
            None => {
                let rest = &stripped[inner_start..];
                if let Some(obj_len) = balanced_json_len(rest) {
                    if let Some(parsed) = parse_tool_call_body(&rest[..obj_len]) {
                        calls.push(parsed);
                    }
                    cursor = inner_start + obj_len;
                } else {
                    break;
                }
            }
        }
    }
    clean.push_str(&stripped[cursor..]);
    (clean.trim().to_string(), calls)
}

/// Parse a single `<tool_call>` body. Tolerates `arguments` arriving as
/// either a JSON object (canonical) or a JSON-encoded string (some
/// model variants escape the inner object).
fn parse_tool_call_body(body: &str) -> Option<ParsedToolCall> {
    let v: JsonValue = serde_json::from_str(body.trim()).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let raw_args = v
        .get("arguments")
        .or_else(|| v.get("parameters"))
        .cloned()
        .unwrap_or(JsonValue::Object(Default::default()));
    let arguments = if let Some(s) = raw_args.as_str() {
        serde_json::from_str(s).unwrap_or(JsonValue::Object(Default::default()))
    } else {
        raw_args
    };
    Some(ParsedToolCall { name, arguments })
}

/// Byte length of the leading balanced JSON object in `s` — from the first `{`
/// (after optional whitespace) through its matching `}`, honouring string literals
/// and escapes so braces inside string values don't miscount. `None` when `s`
/// doesn't start with an object or it never closes. ASCII-only scan: UTF-8
/// continuation bytes (≥0x80) never collide with the `{ } " \` it watches for, so a
/// byte index is a safe char boundary (it always lands right after an ASCII `}`).
fn balanced_json_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn strip_think_block(content: &str) -> String {
    if let Some(start) = content.find("<think>") {
        if let Some(end_rel) = content[start..].find("</think>") {
            let end = start + end_rel + "</think>".len();
            let mut s = String::with_capacity(content.len());
            s.push_str(&content[..start]);
            s.push_str(content[end..].trim_start());
            return s;
        }
    }
    content.to_string()
}

/// Render a tool's `StepOutput` as the JSON the agent's next turn
/// will see. Matches the shape the live-trial harness sends back over
/// the OICP wire — JSON values pass through verbatim, text is wrapped
/// in `{"text": ...}` so the parser-side doesn't have to special-case.
pub(crate) fn format_step_output(out: &StepOutput) -> String {
    match out {
        StepOutput::Json(v) => v.to_string(),
        StepOutput::Text(t) => serde_json::json!({ "text": t }).to_string(),
        StepOutput::ReasonWithToolsResult {
            text,
            iterations,
            capped,
            ..
        } => serde_json::json!({
            "text": text,
            "iterations": iterations,
            "capped": capped,
        })
        .to_string(),
        other => serde_json::json!({ "non_json_output": format!("{other:?}") }).to_string(),
    }
}

/// Project tool descriptors into the OpenAI `ToolSchema` shape the embedded
/// chat-template path (`CompletionRequest.tools`) consumes — the array a loop
/// passes so the model sees the scoped tools' names + parameter schemas.
pub(crate) fn tool_schemas_for(descriptors: &[ToolDescriptor]) -> Vec<ToolSchema> {
    descriptors
        .iter()
        .map(|d| ToolSchema {
            name: d.id.clone(),
            description: Some(d.description.clone()),
            parameters: d.parameters.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_tool_call() {
        let text = r#"Let me check the recipe.
<tool_call>{"name":"recipe_validate","arguments":{"path":"foo"}}</tool_call>"#;
        let (visible, calls) = parse_assistant_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "recipe_validate");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "foo"}));
        assert_eq!(visible, "Let me check the recipe.");
    }

    #[test]
    fn parses_tool_call_missing_closing_tag() {
        // Daemon-routed authoring turns: the grammar satisfies on the envelope's
        // final `}` and the model stops before `</tool_call>`. The envelope JSON is
        // complete and valid — recover it instead of dropping a real tool call.
        // (A nested object + a brace inside a string value exercise brace-matching.)
        let text = r#"<tool_call>{"name":"workflow_write_structured","arguments":{"path":"folder-summaries","workflow":{"step":[{"id":"s","prompt":"use { braces } in text"}]}}}"#;
        let (visible, calls) = parse_assistant_text(text);
        assert_eq!(calls.len(), 1, "missing-closing-tag call must still parse");
        assert_eq!(calls[0].name, "workflow_write_structured");
        assert_eq!(
            calls[0].arguments["path"],
            serde_json::json!("folder-summaries")
        );
        // The envelope is stripped from the visible text just like the tagged case.
        assert_eq!(visible, "");
    }

    #[test]
    fn balanced_json_len_handles_nesting_and_strings() {
        assert_eq!(balanced_json_len("{}"), Some(2));
        assert_eq!(balanced_json_len(r#"{"a":{"b":1}}rest"#), Some(13));
        // Braces inside a string value must not miscount the close.
        assert_eq!(balanced_json_len(r#"{"k":"a{b}c"}"#), Some(13));
        // An escaped quote inside a string doesn't end the string early.
        assert_eq!(balanced_json_len(r#"{"k":"a\"}"}xx"#), Some(12));
        // Leading whitespace is counted in the returned length (offset from start).
        assert_eq!(balanced_json_len("  {\"x\":1} "), Some(9));
        assert_eq!(balanced_json_len("not json"), None);
        assert_eq!(balanced_json_len("{unclosed"), None);
    }

    #[test]
    fn parses_string_encoded_arguments() {
        let text =
            r#"<tool_call>{"name":"recipe_read","arguments":"{\"path\":\"foo\"}"}</tool_call>"#;
        let (_, calls) = parse_assistant_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "foo"}));
    }

    #[test]
    fn strips_think_block_then_parses() {
        let text = r#"<think>I should validate first.</think>
<tool_call>{"name":"recipe_validate","arguments":{"path":"a"}}</tool_call>"#;
        let (visible, calls) = parse_assistant_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(visible, "");
    }

    #[test]
    fn no_tool_call_returns_text_only() {
        let text = "The recipe looks correct now.";
        let (visible, calls) = parse_assistant_text(text);
        assert!(calls.is_empty());
        assert_eq!(visible, "The recipe looks correct now.");
    }

    #[test]
    fn multiple_tool_calls_in_one_response() {
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call> and then <tool_call>{"name":"b","arguments":{"k":1}}</tool_call>"#;
        let (visible, calls) = parse_assistant_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
        assert_eq!(visible, "and then");
    }

    #[test]
    fn unterminated_tool_call_treats_as_text() {
        let text = "<tool_call>{\"name\":\"a\"";
        let (visible, calls) = parse_assistant_text(text);
        assert!(calls.is_empty());
        // Visible text after the broken opener is preserved as-is so
        // the operator can see what the model emitted.
        assert!(visible.starts_with("<tool_call>"));
    }
}
