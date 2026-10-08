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
//! Reasoning is a turn that starts with `<think>` — the engine starts the
//! turn with the tag when its prompt left the block open — and ends at
//! `</think>` or the first `<tool_call>` (llama.cpp's `thinking_end_tags`
//! for this format). [`TurnStream`] streams the same parse.

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

/// Parse `text`, one assistant turn as the engine returns it. The turn
/// starts where the prompt left off: when the prompt opened a `<think>`
/// block, the engine starts the text with that tag, so reasoning is exactly
/// a turn that starts with `<think>` (llama.cpp's `try_parse_reasoning`,
/// with `thinking_forced_open` folded into the text). It ends at
/// `</think>` or at the first `<tool_call>`, whichever comes first
/// (`thinking_end_tags` for this format). `tools` are the request's own
/// tools, whose schemas decide which XML parameters are strings.
pub fn parse_assistant_turn(text: &str, tools: &[ToolSchema]) -> AssistantTurn {
    parse(text, tools, false)
}

/// One parser for both modes. `partial` is a turn still being generated:
/// a marker that may still be completing is held back, an unterminated
/// call is not parsed yet, and whitespace that may yet be trimmed is not
/// emitted. Every field it returns is therefore a prefix of the field the
/// finished turn parses to (the property [`TurnStream`] is built on).
fn parse(text: &str, tools: &[ToolSchema], partial: bool) -> AssistantTurn {
    let text = if partial {
        &text[..stable_len(text)]
    } else {
        text
    };
    let mut turn = AssistantTurn::default();
    let mut rest = text;
    if let Some(body) = text.trim_start().strip_prefix(THINK_OPEN) {
        let close = body.find(THINK_CLOSE);
        let call = body.find(CALL_OPEN);
        let (end, resume) = match (close, call) {
            (Some(c), Some(k)) if k < c => (k, k),
            (Some(c), _) => (c, c + THINK_CLOSE.len()),
            (None, Some(k)) => (k, k),
            (None, None) => (body.len(), body.len()),
        };
        turn.reasoning_content = non_empty(body[..end].trim());
        rest = &body[resume..];
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
            // Still being written: nothing to parse yet.
            None if partial => break,
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

/// `text` without a tail that a marker may still be completing: the
/// longest suffix that is a proper prefix of any marker. A `<` in code
/// waits one more piece at most.
fn stable_len(text: &str) -> usize {
    let mut cut = text.len();
    for marker in [THINK_OPEN, THINK_CLOSE, CALL_OPEN] {
        for n in (1..marker.len()).rev() {
            if text.ends_with(&marker[..n]) {
                cut = cut.min(text.len() - n);
                break;
            }
        }
    }
    cut
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// One piece of a turn being streamed, in the order a client receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnDelta {
    /// More reasoning text.
    Reasoning(String),
    /// More visible reply.
    Content(String),
    /// A call whose block has closed, complete.
    ToolCall(ParsedToolCall),
}

/// The streaming face of [`parse_assistant_turn`], as llama-server streams:
/// after every piece the whole text is parsed again (as a turn still being
/// generated) and what grew since the last piece is sent. Because a partial
/// parse only ever extends, the deltas of any split of a text add up to the
/// finished turn's parse; [`TurnStream::finish`] sends the rest and returns
/// that parse.
#[derive(Debug, Clone)]
pub struct TurnStream {
    tools: Vec<ToolSchema>,
    text: String,
    sent: AssistantTurn,
    /// Pieces after which a field no longer extended what had been sent.
    /// Zero by construction; counted so a breach is visible, not silent.
    diverged: usize,
}

impl TurnStream {
    /// A stream for a turn answering a request with these `tools`.
    pub fn new(tools: Vec<ToolSchema>) -> Self {
        Self {
            tools,
            text: String::new(),
            sent: AssistantTurn::default(),
            diverged: 0,
        }
    }

    /// Take one generated piece; return what it made sendable.
    pub fn push(&mut self, piece: &str) -> Vec<TurnDelta> {
        self.text.push_str(piece);
        let now = parse(&self.text, &self.tools, true);
        self.advance(now)
    }

    /// The turn is complete: send what was held back, and return the
    /// finished parse (its `unparsed` included) and the divergence count.
    pub fn finish(mut self) -> TurnEnd {
        let turn = parse(&self.text, &self.tools, false);
        let deltas = self.advance(turn.clone());
        TurnEnd {
            deltas,
            turn,
            diverged: self.diverged,
        }
    }

    /// The text taken so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    fn advance(&mut self, now: AssistantTurn) -> Vec<TurnDelta> {
        let mut out = Vec::new();
        let sent_r = self.sent.reasoning_content.as_deref().unwrap_or("");
        let now_r = now.reasoning_content.as_deref().unwrap_or("");
        match grown(sent_r, now_r) {
            Some(d) if !d.is_empty() => {
                out.push(TurnDelta::Reasoning(d.to_string()));
                self.sent.reasoning_content = now.reasoning_content.clone();
            }
            Some(_) => {}
            None => self.diverged += 1,
        }
        match grown(&self.sent.content, &now.content) {
            Some(d) if !d.is_empty() => {
                out.push(TurnDelta::Content(d.to_string()));
                self.sent.content = now.content.clone();
            }
            Some(_) => {}
            None => self.diverged += 1,
        }
        let n = self.sent.tool_calls.len();
        if now.tool_calls.len() >= n && now.tool_calls[..n] == self.sent.tool_calls[..] {
            for call in &now.tool_calls[n..] {
                out.push(TurnDelta::ToolCall(call.clone()));
                self.sent.tool_calls.push(call.clone());
            }
        } else {
            self.diverged += 1;
        }
        out
    }
}

/// What [`TurnStream::finish`] returns.
#[derive(Debug, Clone)]
pub struct TurnEnd {
    /// The last deltas: what was held back until the turn ended.
    pub deltas: Vec<TurnDelta>,
    /// The finished parse, the same [`parse_assistant_turn`] returns.
    pub turn: AssistantTurn,
    /// Pieces after which a field did not extend what had been sent; the
    /// stream then differs from `turn`. Zero by construction.
    pub diverged: usize,
}

/// What `now` adds to `sent`, or `None` when it does not extend it —
/// including when it is shorter, since the wire cannot take text back.
fn grown<'a>(sent: &str, now: &'a str) -> Option<&'a str> {
    now.strip_prefix(sent)
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
        let turn = parse_assistant_turn(text, &read_tool());
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
        let turn = parse_assistant_turn(text, &read_tool());
        assert_eq!(turn.tool_calls[0].arguments, r#"{"filePath":"123"}"#);
    }

    #[test]
    fn several_calls_in_one_turn_all_parse_in_order() {
        let call = |p: &str| {
            format!("<tool_call>\n<function=read>\n<parameter=filePath>\n{p}\n</parameter>\n</function>\n</tool_call>")
        };
        let text = format!("Reading both.\n\n{}\n{}", call("a.py"), call("b.py"));
        let turn = parse_assistant_turn(&text, &read_tool());
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
        let text = "<think>\nPlan: read the file first.\n</think>\n\nOn it.\n<tool_call>\n<function=read>\n<parameter=filePath>\nx\n</parameter>\n</function>\n</tool_call>";
        let turn = parse_assistant_turn(text, &read_tool());
        assert_eq!(
            turn.reasoning_content.as_deref(),
            Some("Plan: read the file first.")
        );
        assert_eq!(turn.content, "On it.");
        assert_eq!(turn.tool_calls.len(), 1);
    }

    /// In a think block, a call ends the reasoning without a `</think>`
    /// (llama.cpp's thinking_end_tags for this format). Without the block —
    /// a prompt that opened none — the same words are the reply.
    #[test]
    fn reasoning_ends_at_a_call_inside_a_think_block() {
        let rest = "I should read it.\n<tool_call>\n<function=read>\n<parameter=filePath>\nx\n</parameter>\n</function>\n</tool_call>";
        let on = parse_assistant_turn(&format!("<think>\n{rest}"), &read_tool());
        assert_eq!(on.reasoning_content.as_deref(), Some("I should read it."));
        assert_eq!(on.content, "");
        assert_eq!(on.tool_calls.len(), 1);
        let off = parse_assistant_turn(rest, &read_tool());
        assert_eq!(off.reasoning_content, None);
        assert_eq!(off.content, "I should read it.");
    }

    /// The defect the adapter's old `unwrap_or(true)` made: a model whose
    /// prompt opened no think block, with no `enable_thinking` from the
    /// client, had its whole reply taken as reasoning and returned empty.
    /// Reasoning is now what the text says it is.
    #[test]
    fn a_turn_without_a_think_block_is_all_content() {
        let turn = parse_assistant_turn("Paris is the capital of France.", &read_tool());
        assert_eq!(turn.reasoning_content, None);
        assert_eq!(turn.content, "Paris is the capital of France.");
    }

    /// llama.cpp reads reasoning only from a turn that opens with the tag;
    /// a stray close in a reply is text.
    #[test]
    fn a_stray_think_close_in_a_reply_is_content() {
        let turn = parse_assistant_turn("Use </think> to end it.", &read_tool());
        assert_eq!(turn.reasoning_content, None);
        assert_eq!(turn.content, "Use </think> to end it.");
    }

    #[test]
    fn hermes_json_calls_still_parse() {
        let text = r#"<tool_call>{"name": "read", "arguments": {"filePath": "x"}}</tool_call>"#;
        let turn = parse_assistant_turn(text, &read_tool());
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "read");
    }

    /// The daemon's own malformed call from 2026-10-06 is reported, not
    /// dropped and not turned into a call.
    #[test]
    fn a_malformed_body_is_reported_unparsed() {
        let text = "<tool_call>\n{\"read\":\"filePath\":\"/tmp/x\"}\n</tool_call>";
        let turn = parse_assistant_turn(text, &read_tool());
        assert!(turn.tool_calls.is_empty());
        assert_eq!(turn.unparsed.len(), 1);
    }

    #[test]
    fn plain_text_is_content() {
        let turn = parse_assistant_turn("All twelve tests pass.", &read_tool());
        assert_eq!(turn.content, "All twelve tests pass.");
        assert!(turn.tool_calls.is_empty() && turn.unparsed.is_empty());
    }

    // ── Streaming: one parser, two modes ─────────────────────────────

    fn call(path: &str) -> String {
        format!("<tool_call>\n<function=read>\n<parameter=filePath>\n{path}\n</parameter>\n</function>\n</tool_call>")
    }

    fn samples() -> Vec<String> {
        vec![
            format!(
                "<think>\nPlan: read the file first.\n</think>\n\nOn it.\n{}",
                call("x")
            ),
            format!("<think>\nI should read it.\n{}", call("x")),
            format!("Reading both.\n\n{}\n{}", call("a.py"), call("b.py")),
            "x < y, a <b> tag, <t and </thin, then </think> literally.".into(),
            "<think>\nhmm\n\n</think>\n\nAnswer with trailing space   \n\n".into(),
            r#"<tool_call>{"name": "read", "arguments": {"filePath": "x"}}</tool_call>"#.into(),
            "Doing it.\n<tool_call>\n<function=read>\n<parameter=filePath>\nx\n</parameter>\n"
                .into(),
            "<think>\nÉtude — naïve 日本語 ✓\n</think>\nRéponse ✓ 日本".into(),
            "<think>\n".into(),
            "  <think>\nleading space\n</think>ok".into(),
            String::new(),
        ]
    }

    /// Split `text` into pieces of `sizes` chars, cycling.
    fn split(text: &str, sizes: &[usize]) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let (mut out, mut i, mut k) = (Vec::new(), 0, 0);
        while i < chars.len() {
            let n = sizes[k % sizes.len()].max(1);
            out.push(chars[i..(i + n).min(chars.len())].iter().collect());
            i += n;
            k += 1;
        }
        out
    }

    fn stream(pieces: &[String]) -> (Vec<TurnDelta>, AssistantTurn, usize) {
        let mut s = TurnStream::new(read_tool());
        let mut deltas: Vec<TurnDelta> = pieces.iter().flat_map(|p| s.push(p)).collect();
        let end = s.finish();
        deltas.extend(end.deltas);
        (deltas, end.turn, end.diverged)
    }

    /// P2-4 (note d25c3a96): however a reply is split into pieces, its
    /// stream adds up to exactly what the non-streaming parse returns.
    #[test]
    fn every_split_of_a_turn_streams_to_its_parse() {
        let mut lcg: u64 = 0x2545_f491_4f6c_dd1d;
        let mut random = Vec::new();
        for _ in 0..64 {
            lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            random.push((lcg >> 33) as usize % 9 + 1);
        }
        let mut splits: Vec<Vec<usize>> = (1..=7).map(|n| vec![n]).collect();
        splits.extend([vec![13], vec![29], vec![1, 4, 2, 8, 3], random]);
        for text in samples() {
            let want = parse_assistant_turn(&text, &read_tool());
            for sizes in &splits {
                let (deltas, done, diverged) = stream(&split(&text, sizes));
                let (mut r, mut c, mut calls) = (String::new(), String::new(), Vec::new());
                for d in deltas {
                    match d {
                        TurnDelta::Reasoning(x) => r.push_str(&x),
                        TurnDelta::Content(x) => c.push_str(&x),
                        TurnDelta::ToolCall(x) => calls.push(x),
                    }
                }
                let ctx = format!("text {text:?} split {sizes:?}");
                assert_eq!(diverged, 0, "{ctx}");
                assert_eq!(done, want, "{ctx}");
                assert_eq!(
                    Some(r).filter(|r| !r.is_empty()),
                    want.reasoning_content,
                    "{ctx}"
                );
                assert_eq!(c, want.content, "{ctx}");
                assert_eq!(calls, want.tool_calls, "{ctx}");
            }
        }
    }

    /// Live, not buffered: reasoning and reply leave as they are written.
    #[test]
    fn reasoning_and_content_stream_before_the_turn_ends() {
        let mut s = TurnStream::new(read_tool());
        assert!(s.push("<think>\n").is_empty());
        assert_eq!(s.push("Look at"), [TurnDelta::Reasoning("Look at".into())]);
        assert_eq!(
            s.push(" it.\n</think>\n\nH"),
            [
                TurnDelta::Reasoning(" it.".into()),
                TurnDelta::Content("H".into())
            ]
        );
        assert_eq!(s.push("ere."), [TurnDelta::Content("ere.".into())]);
    }

    /// Markup is never sent as text: a marker still being written waits,
    /// and a call is sent whole once its block closes.
    #[test]
    fn a_call_is_sent_whole_when_its_block_closes() {
        let mut s = TurnStream::new(read_tool());
        assert_eq!(s.push("Hello <tool_"), [TurnDelta::Content("Hello".into())]);
        assert!(s
            .push("call>\n<function=read>\n<parameter=filePath>\nx\n</parameter>\n</function>\n")
            .is_empty());
        let out = s.push("</tool_call>");
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(matches!(&out[0], TurnDelta::ToolCall(c) if c.arguments == r#"{"filePath":"x"}"#));
    }
}
