// SPDX-License-Identifier: AGPL-3.0-or-later
//! One assistant turn of a conversation, parsed out of the model's raw text
//! the way llama-server parses it: reasoning, content, and tool calls in the
//! model's own format.
//!
//! Two call formats live inside `<tool_call>…</tool_call>`, told apart by
//! their first character, so the parser needs no template:
//!
//! - the Qwen3-Coder / Qwen3.5 XML form the GGUF templates of this family
//!   teach — `<function=NAME>\n<parameter=K>\nVALUE\n</parameter>\n…
//!   </function>` — parsed to llama.cpp's grammar
//!   (common/chat.cpp `common_chat_params_init_qwen3_coder`): a parameter
//!   whose schema is a string keeps its raw text, any other is JSON;
//! - Hermes JSON, `{"name": …, "arguments": …}`, handed to the existing
//!   lenient parser in [`crate::tool_calls`] so there is one of it.
//!
//! Reasoning ends at `</think>`, or, when the prompt left a think block
//! open, at the first `<tool_call>` (llama.cpp's `thinking_end_tags` for
//! this format).

use serde_json::{Map, Value};

use crate::completion::ToolSchema;
use crate::tool_calls::{parse_tool_calls_with_errors, ParsedToolCall};

/// A parsed assistant turn.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssistantTurn {
    /// The thinking text, when the turn had any.
    pub reasoning_content: Option<String>,
    /// The visible reply, without reasoning or call markup.
    pub content: String,
    /// The calls, in the order the model made them.
    pub tool_calls: Vec<ParsedToolCall>,
    /// Bodies of `<tool_call>` blocks that did not parse — reported, never
    /// dropped silently.
    pub unparsed: Vec<String>,
}

const CALL_OPEN: &str = "<tool_call>";
const CALL_CLOSE: &str = "</tool_call>";
const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Parse `text`, the model's output after the prompt. `thinking_open` is
/// whether the prompt left a `<think>` block open (thinking on), which is
/// what lets reasoning end at a call instead of at `</think>`. `tools` are
/// the request's own tools, whose schemas decide which XML parameters are
/// strings.
pub fn parse_assistant_turn(
    text: &str,
    thinking_open: bool,
    tools: &[ToolSchema],
) -> AssistantTurn {
    let mut turn = AssistantTurn::default();
    let mut rest = text;
    if let Some(end) = rest.find(THINK_CLOSE) {
        let reasoning = rest[..end]
            .trim_start()
            .strip_prefix(THINK_OPEN)
            .unwrap_or(&rest[..end]);
        turn.reasoning_content = non_empty(reasoning.trim());
        rest = &rest[end + THINK_CLOSE.len()..];
    } else if thinking_open {
        let end = rest.find(CALL_OPEN).unwrap_or(rest.len());
        turn.reasoning_content = non_empty(rest[..end].trim());
        rest = &rest[end..];
    }
    let first_call = rest.find(CALL_OPEN).unwrap_or(rest.len());
    turn.content = rest[..first_call].trim().to_string();
    let mut cursor = first_call;
    while let Some(rel) = rest[cursor..].find(CALL_OPEN) {
        let body_start = cursor + rel + CALL_OPEN.len();
        let (body, next) = match rest[body_start..].find(CALL_CLOSE) {
            Some(rel_end) => (
                &rest[body_start..body_start + rel_end],
                body_start + rel_end + CALL_CLOSE.len(),
            ),
            None => (&rest[body_start..], rest.len()),
        };
        cursor = next;
        let trimmed = body.trim();
        if trimmed.starts_with("<function=") {
            match parse_xml_call(trimmed, tools) {
                Some(call) => turn.tool_calls.push(call),
                None => turn.unparsed.push(trimmed.to_string()),
            }
        } else {
            let (calls, errors) =
                parse_tool_calls_with_errors(&format!("{CALL_OPEN}{body}{CALL_CLOSE}"));
            turn.tool_calls.extend(calls);
            turn.unparsed.extend(errors);
        }
    }
    turn
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// `<function=NAME>` then `<parameter=K>VALUE</parameter>` pairs, up to
/// `</function>` (or the end of the block, leniently).
fn parse_xml_call(body: &str, tools: &[ToolSchema]) -> Option<ParsedToolCall> {
    let after = body.strip_prefix("<function=")?;
    let name_end = after.find('>')?;
    let name = after[..name_end].trim();
    if name.is_empty() {
        return None;
    }
    let mut args = after[name_end + 1..].trim_start_matches('\n');
    if let Some(close) = args.find("</function>") {
        args = &args[..close];
    }
    let schema = tools
        .iter()
        .find(|t| t.name == name)
        .and_then(|t| t.parameters.get("properties"));
    let mut out = Map::new();
    let mut cursor = args;
    while let Some(start) = cursor.find("<parameter=") {
        let after_open = &cursor[start + "<parameter=".len()..];
        let key_end = after_open.find('>')?;
        let key = after_open[..key_end].trim().to_string();
        let value_area = &after_open[key_end + 1..];
        let value_end = value_area.find("</parameter>")?;
        let raw = value_area[..value_end]
            .strip_prefix('\n')
            .unwrap_or(&value_area[..value_end]);
        let raw = raw.strip_suffix('\n').unwrap_or(raw);
        let is_string = schema
            .and_then(|p| p.get(&key))
            .is_some_and(resolves_to_string);
        let value = if is_string {
            Value::String(raw.to_string())
        } else {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        };
        out.insert(key, value);
        cursor = &value_area[value_end + "</parameter>".len()..];
    }
    Some(ParsedToolCall {
        name: name.to_string(),
        arguments: Value::Object(out).to_string(),
    })
}

/// A parameter schema that only admits strings: `"type": "string"`, or a
/// type list / `anyOf` whose every branch is a string.
fn resolves_to_string(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(t)) => t == "string",
        Some(Value::Array(ts)) => !ts.is_empty() && ts.iter().all(|t| t == "string"),
        _ => schema
            .get("anyOf")
            .or_else(|| schema.get("oneOf"))
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                !branches.is_empty() && branches.iter().all(resolves_to_string)
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_tool() -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "read".into(),
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["filePath"],
                "properties": {
                    "filePath": {"type": "string"},
                    "offset": {"type": "number"},
                    "limit": {"type": "number"}
                }
            }),
        }]
    }

    #[test]
    fn a_native_xml_call_parses_with_typed_arguments() {
        let text = "<tool_call>\n<function=read>\n<parameter=filePath>\n/tmp/x/lights_out.py\n</parameter>\n<parameter=offset>\n10\n</parameter>\n</function>\n</tool_call>";
        let turn = parse_assistant_turn(text, false, &read_tool());
        assert_eq!(turn.unparsed, Vec::<String>::new());
        assert_eq!(turn.content, "");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "read");
        assert_eq!(
            turn.tool_calls[0].arguments,
            r#"{"filePath":"/tmp/x/lights_out.py","offset":10}"#
        );
    }

    /// A string parameter whose text looks like JSON stays the text.
    #[test]
    fn a_string_parameter_keeps_its_raw_text() {
        let text = "<tool_call>\n<function=read>\n<parameter=filePath>\n123\n</parameter>\n</function>\n</tool_call>";
        let turn = parse_assistant_turn(text, false, &read_tool());
        assert_eq!(turn.tool_calls[0].arguments, r#"{"filePath":"123"}"#);
    }

    #[test]
    fn several_calls_in_one_turn_all_parse_in_order() {
        let call = |p: &str| {
            format!("<tool_call>\n<function=read>\n<parameter=filePath>\n{p}\n</parameter>\n</function>\n</tool_call>")
        };
        let text = format!("Reading both.\n\n{}\n{}", call("a.py"), call("b.py"));
        let turn = parse_assistant_turn(&text, false, &read_tool());
        assert_eq!(turn.content, "Reading both.");
        let paths: Vec<&str> = turn
            .tool_calls
            .iter()
            .map(|c| c.arguments.as_str())
            .collect();
        assert_eq!(paths, [r#"{"filePath":"a.py"}"#, r#"{"filePath":"b.py"}"#]);
    }

    #[test]
    fn reasoning_ends_at_the_think_close() {
        let text = "Plan: read the file first.\n</think>\n\nOn it.\n<tool_call>\n<function=read>\n<parameter=filePath>\nx\n</parameter>\n</function>\n</tool_call>";
        let turn = parse_assistant_turn(text, true, &read_tool());
        assert_eq!(
            turn.reasoning_content.as_deref(),
            Some("Plan: read the file first.")
        );
        assert_eq!(turn.content, "On it.");
        assert_eq!(turn.tool_calls.len(), 1);
    }

    /// With the think block open, a call can end the reasoning without a
    /// `</think>` (llama.cpp's thinking_end_tags for this format).
    #[test]
    fn reasoning_ends_at_a_call_when_thinking_is_open() {
        let text = "I should read it.\n<tool_call>\n<function=read>\n<parameter=filePath>\nx\n</parameter>\n</function>\n</tool_call>";
        let on = parse_assistant_turn(text, true, &read_tool());
        assert_eq!(on.reasoning_content.as_deref(), Some("I should read it."));
        assert_eq!(on.content, "");
        let off = parse_assistant_turn(text, false, &read_tool());
        assert_eq!(off.reasoning_content, None);
        assert_eq!(off.content, "I should read it.");
    }

    #[test]
    fn hermes_json_calls_still_parse() {
        let text = r#"<tool_call>{"name": "read", "arguments": {"filePath": "x"}}</tool_call>"#;
        let turn = parse_assistant_turn(text, false, &read_tool());
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "read");
    }

    /// The daemon's own malformed call from 2026-10-06 is reported, not
    /// dropped and not turned into a call.
    #[test]
    fn a_malformed_body_is_reported_unparsed() {
        let text = "<tool_call>\n{\"read\":\"filePath\":\"/tmp/x\"}\n</tool_call>";
        let turn = parse_assistant_turn(text, false, &read_tool());
        assert!(turn.tool_calls.is_empty());
        assert_eq!(turn.unparsed.len(), 1);
    }

    #[test]
    fn plain_text_is_content() {
        let turn = parse_assistant_turn("All twelve tests pass.", false, &read_tool());
        assert_eq!(turn.content, "All twelve tests pass.");
        assert!(turn.tool_calls.is_empty() && turn.unparsed.is_empty());
    }
}
