// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool-call extraction from free-form model output.
//!
//! OpenAI-compatible models emit tool calls as `<tool_call>{...}</tool_call>`
//! blocks inside ordinary assistant text. This module owns the lenient parser
//! the serving host and the embedded engine both need: it recovers a call
//! whose closing tag a quantized model dropped, and repairs the two
//! malformed-JSON shapes observed in the field (raw control characters inside
//! string values, orphan `]` brackets).
//!
//! Moved here from `sovereign_inference::embedded::grammar` (domains
//! `REVIEW-build-serving-drop-inference`): the parser is pure — `serde_json`
//! and `std`, no llama.cpp — and the serving host must reach it without
//! linking the inference stack. `sovereign-inference` re-exports every item at
//! the old `sovereign_inference::embedded::*` path, so no caller changed.

/// A single tool call extracted from model output. The adapter maps
/// this into `oicp_types::openai_types::ToolCall` (with a
/// generated id) before emitting the chat-completion response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCall {
    pub name: String,
    /// Raw JSON string for the tool arguments. Kept as a string so the
    /// client sees exactly what the model produced — if the model emits
    /// partial JSON we don't silently coerce it.
    pub arguments: String,
}

/// Extract Qwen3.5-style tool calls from free-form model output.
///
/// Expected markup:
///
/// ```text
/// <tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call>
/// ```
///
/// Multiple blocks per response are supported. Whitespace inside a block
/// is ignored. A block whose body is not valid JSON, or whose JSON lacks
/// a top-level `"name"` field, is **skipped** — not treated as an error
/// — and logged at `warn` so the adapter can tag an `atos_tool_events`
/// row with `phase='parse_error'` and the raw payload. Returning an
/// empty vec on all-malformed output is intentional: a tool-less
/// response is a valid answer.
///
/// Observability: the parser stays pure (no I/O); the caller is
/// responsible for logging. This keeps the function unit-testable
/// without a tracing subscriber.
/// Find the byte length of a balanced JSON object starting at the
/// first `{` in `s`. String-aware (won't trip on `}` inside `"..."`
/// values). Returns None when the input doesn't contain a complete
/// `{...}` (depth never returns to zero) — which is also the
/// "model truncated mid-emission" signal we use to abandon parsing.
///
/// Used by the lenient tool-call extractor when `</tool_call>` is
/// missing. Some quantized models reliably emit `<tool_call>{...}`
/// with a valid JSON object but never close the XML tag; falling
/// back to brace-balancing recovers the call instead of dropping it.
fn find_balanced_json_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Escape unescaped control characters (newline, CR, tab) that appear
/// inside JSON string values. Intentionally narrow: only operates
/// inside `"..."` runs and only on the three control chars known to
/// trip serde — the rest of the body is passed through byte-for-byte.
///
/// Why this exists: some models (Qwen3-Coder-30B observed 2026-05-08)
/// emit a syntactically-correct JSON envelope around their tool call
/// but fail to escape literal `\n`/`\r`/`\t` inside the `content`
/// string value. The result balances braces and parses up to the
/// invalid-control-in-string error, so the daemon was rejecting the
/// whole call. This pre-pass converts those raw bytes to their
/// `\n`/`\r`/`\t` escape forms so `serde_json::from_str` accepts the
/// body. Already-escaped sequences (preceded by an unescaped `\`)
/// pass through untouched.
///
/// Returns the original string if nothing needed escaping (no
/// allocation), otherwise a normalized copy.
pub fn escape_unescaped_control_chars_in_string_values(body: &str) -> std::borrow::Cow<'_, str> {
    let needs_fix = {
        let mut in_string = false;
        let mut escape = false;
        let mut hit = false;
        for c in body.chars() {
            if in_string {
                if escape {
                    escape = false;
                } else {
                    match c {
                        '"' => in_string = false,
                        '\\' => escape = true,
                        '\n' | '\r' | '\t' => {
                            hit = true;
                            break;
                        }
                        _ => {}
                    }
                }
            } else if c == '"' {
                in_string = true;
            }
        }
        hit
    };
    if !needs_fix {
        return std::borrow::Cow::Borrowed(body);
    }

    let mut out = String::with_capacity(body.len() + 16);
    let mut in_string = false;
    let mut escape = false;
    for c in body.chars() {
        if in_string {
            if escape {
                out.push(c);
                escape = false;
                continue;
            }
            match c {
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\\' => {
                    escape = true;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

pub fn parse_tool_calls_from_text(text: &str) -> Vec<ParsedToolCall> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = text[cursor..].find("<tool_call>") {
        let start = cursor + rel_start + "<tool_call>".len();
        // Find the matching closer. Use the literal `</tool_call>`
        // marker — we intentionally do NOT allow nested `<tool_call>`
        // blocks (Qwen3.5 never emits them).
        //
        // Lenient mode: some quantized models emit `<tool_call>` then
        // the JSON body but forget the closing `</tool_call>` tag
        // (FINAL-Bench at Q6_K_M shows this consistently). When the
        // closer is missing, fall back to brace-balancing inside the
        // remaining text — if we find a complete JSON object, use
        // its end as the implicit closer and continue scanning.
        let (body, advance) = match text[start..].find("</tool_call>") {
            Some(rel_end) => (
                text[start..start + rel_end].trim().to_string(),
                start + rel_end + "</tool_call>".len(),
            ),
            None => match find_balanced_json_end(&text[start..]) {
                Some(json_len) => {
                    let body = text[start..start + json_len].trim().to_string();
                    (body, start + json_len)
                }
                None => break, // can't recover; stop scanning
            },
        };
        cursor = advance;

        // Parse the body. Accept either:
        //   {"name": "...", "arguments": {...}}
        //   {"name": "...", "arguments": "<string>"}
        // If serde rejects the raw body (commonly: raw newlines inside
        // a string value), retry on a control-char-normalized copy.
        let parsed = serde_json::from_str::<serde_json::Value>(&body)
            .or_else(|_| {
                let fixed = escape_unescaped_control_chars_in_string_values(&body);
                serde_json::from_str::<serde_json::Value>(&fixed)
            })
            .or_else(|_| {
                let stripped = strip_orphan_close_brackets(&body);
                let fixed = escape_unescaped_control_chars_in_string_values(&stripped);
                serde_json::from_str::<serde_json::Value>(&fixed)
            });
        match parsed {
            Ok(obj) => {
                let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                    continue; // malformed — skip (caller may log via diagnostic API below)
                };
                let args_str = match obj.get("arguments") {
                    Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                out.push(ParsedToolCall {
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
            Err(_) => {
                // malformed JSON — skip; caller logs via tracing::warn!
                continue;
            }
        }
    }
    out
}

/// Same as [`parse_tool_calls_from_text`] but returns the raw bodies
/// of any blocks that failed to parse, so the caller can attribute
/// parse-error telemetry (feeds `atos_tool_events.phase='parse_error'`).
pub fn parse_tool_calls_with_errors(text: &str) -> (Vec<ParsedToolCall>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = text[cursor..].find("<tool_call>") {
        let start = cursor + rel_start + "<tool_call>".len();
        // Same lenient closer behaviour as `parse_tool_calls_from_text`:
        // accept missing `</tool_call>` when the body parses as a
        // balanced JSON object. Models with damaged closing-tag
        // emission (FINAL-Bench Q6_K_M) emit valid call payloads
        // surrounded by half-broken markup.
        let (body, advance) = match text[start..].find("</tool_call>") {
            Some(rel_end) => (
                text[start..start + rel_end].trim().to_string(),
                start + rel_end + "</tool_call>".len(),
            ),
            None => match find_balanced_json_end(&text[start..]) {
                Some(json_len) => {
                    let body = text[start..start + json_len].trim().to_string();
                    (body, start + json_len)
                }
                None => {
                    errors.push(text[start..].to_string());
                    break;
                }
            },
        };
        cursor = advance;
        let body = body.as_str();

        // Same retry-on-normalized-body pattern as the non-with-errors
        // variant (Qwen3-Coder emits raw \n inside a content string).
        // Plus a third retry that strips orphan `]` chars Qwen3.5-9B
        // observed emitting mid-envelope (2026-05-21): model duplicated
        // a key after a runaway content-string and inserted `}]}` at the
        // tail, breaking serde.
        let parsed = serde_json::from_str::<serde_json::Value>(body)
            .or_else(|_| {
                let fixed = escape_unescaped_control_chars_in_string_values(body);
                serde_json::from_str::<serde_json::Value>(&fixed)
            })
            .or_else(|_| {
                let stripped = strip_orphan_close_brackets(body);
                let fixed = escape_unescaped_control_chars_in_string_values(&stripped);
                serde_json::from_str::<serde_json::Value>(&fixed)
            });
        match parsed {
            Ok(obj) => {
                let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                    errors.push(body.to_string());
                    continue;
                };
                let args_str = match obj.get("arguments") {
                    Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                out.push(ParsedToolCall {
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
            Err(_) => errors.push(body.to_string()),
        }
    }
    (out, errors)
}

/// Walk a JSON candidate and drop any `]` that doesn't match an open
/// `[`. Used as a last-ditch repair when a tool-call body has been
/// damaged by mid-stream prose drift (model wrote `}]}` where it
/// meant `}}`). Mirror-image `[` are NOT dropped — that would create
/// new orphan `]` later in the stream and the repair has to stay
/// idempotent under retry.
///
/// String contents pass through verbatim (we don't want to touch
/// `]` inside JSON strings). Escape sequences within strings are
/// honoured so a `\"` doesn't prematurely close the string.
pub fn strip_orphan_close_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bracket_depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = false;
            continue;
        }
        if in_str {
            if c == '\\' {
                esc = true;
                out.push(c);
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            out.push(c);
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '[' => {
                bracket_depth += 1;
                out.push(c);
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    out.push(c);
                }
                // else: orphan close, skip
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod parse_tool_calls_tests {
    //! Lock the parser's behaviour against the two real-world model
    //! emission failure modes:
    //!   1. **Closed tags** — happy path, both `<tool_call>` and
    //!      `</tool_call>` present (Qwen3.5 baseline).
    //!   2. **Missing closing tag** — quantized models (FINAL-Bench
    //!      Q6_K_M observed) emit `<tool_call>{...JSON...}` and
    //!      stop without `</tool_call>`. The lenient brace-balancer
    //!      recovers the call instead of dropping it.
    use super::{parse_tool_calls_from_text, parse_tool_calls_with_errors};

    #[test]
    fn closed_tag_extracts_one_call() {
        let text =
            r#"prelude <tool_call>{"name":"write","arguments":{"path":"a.rs"}}</tool_call> tail"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("a.rs"));
    }

    #[test]
    fn missing_closing_tag_with_balanced_json_recovers() {
        // FINAL-Bench Q6_K_M reliably truncates the closing tag.
        // We accept the body when the JSON balances cleanly.
        let text = r#"<tool_call>{"name":"write","arguments":{"filePath":"Cargo.toml","content":"[package]\nname = \"x\"\n"}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1, "lenient mode should recover the call");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("Cargo.toml"));
    }

    #[test]
    fn missing_closing_tag_with_truncated_json_returns_no_calls() {
        // Body never balances — model truncated mid-string. Drop it
        // rather than emit a corrupt call. `_with_errors` should also
        // surface the leftover so telemetry catches it.
        let text = r#"<tool_call>{"name":"write","arguments":{"content":"unterminated string"#;
        let calls = parse_tool_calls_from_text(text);
        assert!(calls.is_empty());
        let (out, errors) = parse_tool_calls_with_errors(text);
        assert!(out.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn brace_inside_string_does_not_close_early() {
        // A string value containing `}` shouldn't close the JSON.
        let text = r#"<tool_call>{"name":"write","arguments":{"content":"loop { x += 1; }"}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn multiple_calls_extract_in_order_even_with_mixed_closers() {
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call>some text<tool_call>{"name":"b","arguments":{"x":1}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn orphan_close_bracket_inside_envelope_recovers_via_repair() {
        // Qwen3.5-9B-HighIQ observed 2026-05-21 (run r): model
        // emitted a runaway `content` string ending with `","path":"src/lib.rs"}]}`.
        // The trailing `]` is orphan — no matching `[` open — and
        // serde rejects the body. The repair pass strips the orphan
        // close bracket so the call survives.
        let body = r#"{"name":"write","arguments":{"path":"src/lib.rs","content":"// short","path":"src/lib.rs"}]}"#;
        let text = format!("<tool_call>{}</tool_call>", body);
        let calls = parse_tool_calls_from_text(&text);
        assert_eq!(
            calls.len(),
            1,
            "orphan-bracket repair should rescue the call"
        );
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("src/lib.rs"));

        let (out, errors) = parse_tool_calls_with_errors(&text);
        assert_eq!(out.len(), 1);
        assert!(errors.is_empty());
    }

    #[test]
    fn strip_orphan_close_brackets_idempotent_on_valid_input() {
        let s = r#"{"a":1,"b":[2,3],"c":"x]y"}"#;
        assert_eq!(super::strip_orphan_close_brackets(s), s);
    }

    #[test]
    fn strip_orphan_close_brackets_drops_lone_close() {
        let s = r#"{"a":1}]}"#;
        // Only the `]` is orphan; the outer braces balance. The
        // surrounding `}` after `]` is also at depth 0 here, but the
        // repair only touches brackets.
        let repaired = super::strip_orphan_close_brackets(s);
        assert!(!repaired.contains(']'));
        assert!(repaired.starts_with('{'));
    }

    #[test]
    fn strip_orphan_close_brackets_leaves_bracketed_inside_strings_alone() {
        // `]` inside a JSON string should NOT be stripped — it isn't
        // structural. Repair only touches characters at depth 0 of
        // string nesting.
        let s = r#"{"a":"x]y","b":"]"}"#;
        assert_eq!(super::strip_orphan_close_brackets(s), s);
    }

    #[test]
    fn raw_newlines_inside_string_value_recover_via_normalization() {
        // Qwen3-Coder-30B observed 2026-05-08: balanced JSON envelope
        // but raw `\n` (0x0A) bytes inside the `content` string value
        // instead of the `\\n` escape. Without normalization this
        // fails serde with "control character in string". The pre-pass
        // converts the raw bytes to escapes and re-parses.
        let body = "{\"name\":\"write\",\"arguments\":{\"path\":\"a.rs\",\"content\":\"fn main() {\nprintln!(\\\"hi\\\");\n}\"}}";
        let text = format!("<tool_call>{}</tool_call>", body);
        let calls = parse_tool_calls_from_text(&text);
        assert_eq!(calls.len(), 1, "normalization should recover the call");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("println"));
    }

    #[test]
    fn already_escaped_sequences_are_preserved_through_normalization() {
        use super::escape_unescaped_control_chars_in_string_values;
        // `\\n` (literal backslash + n) must stay `\\n` after the
        // normalize pass — it's an already-correct escape sequence.
        let input = r#"{"x":"a\nb"}"#;
        let out = escape_unescaped_control_chars_in_string_values(input);
        assert_eq!(out.as_ref(), input, "no raw control chars present");
    }

    #[test]
    fn normalization_skips_control_chars_outside_strings() {
        use super::escape_unescaped_control_chars_in_string_values;
        // A newline between fields (outside any string value) must
        // be left alone. JSON allows it as whitespace; serde already
        // accepts it.
        let input = "{\"x\":\"a\",\n\"y\":\"b\"}";
        let out = escape_unescaped_control_chars_in_string_values(input);
        assert_eq!(out.as_ref(), input);
    }
}

#[cfg(test)]
mod tool_call_parser_tests {
    use super::{parse_tool_calls_from_text, parse_tool_calls_with_errors};

    #[test]
    fn happy_path_single_call() {
        let text = r#"Sure, I'll check the weather.
<tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        // Object arguments serialize with no spaces — lock the shape.
        assert_eq!(calls[0].arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn multiple_calls_in_one_response() {
        let text = r#"<tool_call>{"name":"a","arguments":{"x":1}}</tool_call>
then <tool_call>{"name":"b","arguments":{"y":"hi"}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn string_arguments_preserved_as_is() {
        // Some models (including older Qwen variants) stringify the
        // arguments object. We round-trip that form verbatim.
        let text = r#"<tool_call>{"name":"f","arguments":"{\"x\":1}"}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, r#"{"x":1}"#);
    }

    #[test]
    fn missing_arguments_defaults_to_empty_object() {
        let text = r#"<tool_call>{"name":"noargs"}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noargs");
        assert_eq!(calls[0].arguments, "{}");
    }

    #[test]
    fn malformed_json_is_skipped_silently() {
        let text = r#"<tool_call>{"name": "truncated", "arguments": {</tool_call>
and <tool_call>{"name":"ok","arguments":{}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ok");
    }

    #[test]
    fn unterminated_tag_stops_scanning() {
        // No closing </tool_call> → parser stops rather than munching
        // arbitrary text as JSON. Anything before the open tag is
        // already ignored.
        let text = r#"preamble <tool_call>{"name":"never_closed""#;
        assert!(parse_tool_calls_from_text(text).is_empty());
    }

    #[test]
    fn no_tool_call_tags_returns_empty() {
        let text = "plain reply, no tools here";
        assert!(parse_tool_calls_from_text(text).is_empty());
    }

    #[test]
    fn error_variant_reports_malformed_bodies() {
        let text = r#"<tool_call>{ not json }</tool_call>
<tool_call>{"name":"ok","arguments":{}}</tool_call>
<tool_call>{"missing_name":true}</tool_call>"#;
        let (calls, errors) = parse_tool_calls_with_errors(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ok");
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("not json"));
        assert!(errors[1].contains("missing_name"));
    }

    #[test]
    fn whitespace_inside_block_tolerated() {
        let text = "<tool_call>\n  {\"name\": \"spaced\", \"arguments\": {}}  \n</tool_call>";
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spaced");
    }
}
