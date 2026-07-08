// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former 9669-line `embedded.rs` (PR5b). One slot /
//! concern per file; re-exported flat through `embedded/mod.rs` so every
//! `crate::embedded::<Item>` path stays valid.
#![allow(unused_imports)]
use super::*;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use crate::llama::cpp::context::params::{LlamaContextParams, LlamaContextType};
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaChatMessage, LlamaModel};
use crate::llama::cpp::mtp::MtpSession;
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::{LlamaContextExt, LlamaModelExt};

use sovereign_core::error::Error;
use sovereign_core::model_family::{
    EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

// ─── JSON Schema → GBNF Grammar ─────────────────────────────

/// Convert a JSON schema to a GBNF (GGML BNF) grammar string.
///
/// A single tool call extracted from model output. The adapter maps
/// this into `commonwealth_api::openai_types::ToolCall` (with a
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

/// Handles the subset of JSON schema used by tool descriptors:
/// - `"type": "object"` with `"properties"` and `"required"`
/// - `"type": "string"` / `"integer"` / `"number"` / `"boolean"`
/// - Flat schemas only (no nested objects or arrays of objects)
///
/// The grammar constrains the token sampler so the model can only
/// produce valid JSON matching the schema. This eliminates malformed
/// JSON, missing required fields, and type errors.
pub fn json_schema_to_gbnf(schema: &serde_json::Value) -> String {
    // Primitive rules shared by all schemas.
    let mut rules = vec![
        r#"ws ::= [ \t\n]*"#.to_string(),
        r#"string ::= "\"" ([^"\\] | "\\" .)* "\""  "#.to_string(),
        r#"integer ::= "-"? [0-9]+"#.to_string(),
        r#"number ::= "-"? [0-9]+ ("." [0-9]+)?"#.to_string(),
        r#"boolean ::= "true" | "false""#.to_string(),
        r#"null ::= "null""#.to_string(),
    ];

    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let required: Vec<String> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            if props.is_empty() {
                // Any JSON object.
                rules.push(r#"root ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}""#.to_string());
                rules.push(r#"value ::= string | number | integer | boolean | null"#.to_string());
            } else {
                // Build a rule that produces exactly the required fields in order,
                // then optionally the non-required fields. This is simpler than
                // allowing arbitrary order and covers our tool calling needs.
                let mut field_parts = Vec::new();

                // Required fields first, in schema order.
                for key in &required {
                    if let Some(prop_schema) = props.get(key) {
                        let type_rule = prop_type_rule(prop_schema);
                        field_parts.push(format!(r#"ws "\"{}\"" ws ":" ws {}"#, key, type_rule));
                    }
                }

                // Optional fields.
                for (key, prop_schema) in &props {
                    if required.contains(key) {
                        continue;
                    }
                    let type_rule = prop_type_rule(prop_schema);
                    field_parts.push(format!(
                        r#"(ws "," ws "\"{}\"" ws ":" ws {})?"#,
                        key, type_rule
                    ));
                }

                if field_parts.is_empty() {
                    rules.push(r#"root ::= "{" ws "}""#.to_string());
                } else {
                    let fields_joined = field_parts.join(r#" ws "," "#);
                    rules.push(format!(r#"root ::= "{{" {} ws "}}""#, fields_joined));
                }
            }
        }
        _ => {
            // Fallback: allow any JSON value.
            rules.push(r#"root ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}""#.to_string());
            rules.push(r#"value ::= string | number | integer | boolean | null"#.to_string());
        }
    }

    rules.join("\n")
}

/// Get the GBNF rule name for a property's type.
fn prop_type_rule(schema: &serde_json::Value) -> &'static str {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("string") => "string",
        Some("integer") => "integer",
        Some("number") => "number",
        Some("boolean") => "boolean",
        _ => "string", // default to string for unknown types
    }
}

#[cfg(test)]
mod primary_siblings_env_tests {
    //! Pin `SOVEREIGN_PRIMARY_SIBLINGS` parsing — env access is split
    //! out so this test is pure (no process-env mutation that would
    //! race with other tests).
    use super::parse_primary_siblings;

    #[test]
    fn absent_returns_none() {
        assert!(parse_primary_siblings(None).is_none());
    }

    #[test]
    fn empty_returns_none() {
        assert!(parse_primary_siblings(Some("")).is_none());
    }

    #[test]
    fn zero_and_one_are_treated_as_disabled() {
        // 0 and 1 both mean "no parallel siblings" — caller stays on
        // the single-context lazy path. Anything else would be a
        // confusing footgun for operators who type "1".
        assert!(parse_primary_siblings(Some("0")).is_none());
        assert!(parse_primary_siblings(Some("1")).is_none());
    }

    #[test]
    fn n_two_and_above_returns_count() {
        let two = parse_primary_siblings(Some("2")).expect("N=2 should parse");
        assert_eq!(two.get(), 2);
        let four = parse_primary_siblings(Some("4")).expect("N=4 should parse");
        assert_eq!(four.get(), 4);
    }

    #[test]
    fn whitespace_is_trimmed() {
        let n = parse_primary_siblings(Some("  3  ")).expect("N=3 with padding");
        assert_eq!(n.get(), 3);
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_primary_siblings(Some("two")).is_none());
        assert!(parse_primary_siblings(Some("-1")).is_none());
        assert!(parse_primary_siblings(Some("3.5")).is_none());
    }
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
mod pick_slot_tests {
    //! PR-E2: the slot dispatch rules ultimately drive the hot-swap
    //! decision in `complete` / `complete_stream`. Lock them here
    //! with fast, GPU-free table tests so a regression in routing
    //! is caught by `cargo test` long before it reaches a live
    //! llama.cpp load.
    use super::{pick_slot, SlotTarget, FAST_SHORT_MAX_INPUT_CHARS};
    use sovereign_core::oicp::{CapabilityHint, InferenceRequirements};
    use sovereign_core::types::{CompletionRequest, Speed};

    fn req(speed: Speed, hint: Option<CapabilityHint>) -> CompletionRequest {
        let mut r = CompletionRequest::new("hi");
        r.preferred_speed = speed;
        if let Some(h) = hint {
            r.oicp = Some(InferenceRequirements::new().with_hint(h));
        }
        r
    }

    #[test]
    fn fast_speed_without_code_hint_goes_to_fast() {
        let r = req(Speed::Fast, None);
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Fast);
        assert_eq!(pick_slot(&r, false, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn fast_short_routing_picks_fast_short_for_small_max_tokens() {
        // Phase 1b/3/5/6 composers attach max_output_tokens ≤512.
        // When the FastShort companion is built, those calls route to
        // the continuous-batched path — that's where the 2.1× wall-
        // clock speedup lives.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(512);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
        // Smaller budgets pass too.
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
    }

    #[test]
    fn fast_short_not_selected_when_companion_absent() {
        // SOVEREIGN_FAST_SHORT_DISABLE=1 (or load failure) leaves
        // has_fast_short=false. Every short call falls through to
        // the original Fast slot — no regression vs pre-rework.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(256);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn fast_short_skipped_for_large_output_budget() {
        // Phase 1 chapter ingestion asks for the full output budget
        // (24576 via PHASE1_SEED_OUTPUT_BUDGET). It must route to Fast
        // (which keeps `n_seq_max=1` for the full per-call window),
        // not FastShort — whose 2048-per-seq slice would mid-decode
        // overflow on a long chapter.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(8192);
        assert_eq!(pick_slot(&r, true, false, true, None), SlotTarget::Fast);
    }

    #[test]
    fn fast_short_skipped_when_prompt_exceeds_per_seq_budget() {
        // 2026-05-19 desktop trace: a Fast+max_tokens=16 call from
        // (likely) the title/scope classifier carried a 9272-char
        // conversation-context prompt; pick_slot routed it to
        // FastShort whose per-seq KV slice is only 2048 tokens, and
        // decode failed with `NoKvCacheSlot (batch_n_tokens=2328,
        // n_requests=1)`. The fix gates FastShort on prompt size in
        // addition to output budget.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(16);
        r.prompt = "x".repeat(FAST_SHORT_MAX_INPUT_CHARS + 1);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Fast,
            "oversized prompt must fall through to Fast even when max_tokens is tiny"
        );
        // Right at the boundary — still allowed.
        r.prompt = "x".repeat(FAST_SHORT_MAX_INPUT_CHARS);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
        // system_message counts too — a long system + tiny user
        // prompt should also fall through.
        r.prompt = "x".repeat(100);
        r.system_message = Some("y".repeat(FAST_SHORT_MAX_INPUT_CHARS));
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Fast,
            "system+prompt combined must respect the FastShort input budget"
        );
    }

    #[test]
    fn fast_short_skipped_when_speed_is_not_fast() {
        // Slow-speed callers (chat synthesis on the primary) ignore
        // FastShort entirely even if their max_output is small.
        // FastShort is for the Fast-slot model only.
        let mut r = req(Speed::Slow, None);
        r.max_tokens = Some(256);
        assert_eq!(pick_slot(&r, true, false, true, None), SlotTarget::Primary);
    }

    #[test]
    fn slow_speed_without_code_hint_picks_primary_when_available() {
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Primary);
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Primary);
    }

    #[test]
    fn slow_speed_without_primary_falls_back_to_fast() {
        // Degraded config: user only configured a fast GGUF. Pre-E2
        // behaviour that must not regress — a Medium/Slow request
        // still runs instead of erroring out.
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, false, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn code_hint_with_code_slot_picks_code_even_on_fast_speed() {
        // Code specialist wins over Speed::Fast dispatch. The hint
        // semantics are "this work needs code reasoning"; Fast-slot
        // generals can't do that well.
        let r = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Code);
    }

    #[test]
    fn code_hint_without_code_slot_follows_speed_rules() {
        // Solo-user with no dedicated coder: fall back to whatever
        // speed-tier would normally serve this request. The peer-
        // mesh scheduler is responsible for routing the hint to a
        // better-matched peer; locally we do what we can.
        let r_fast = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(
            pick_slot(&r_fast, true, false, false, None),
            SlotTarget::Fast
        );

        let r_slow = req(Speed::Slow, Some(CapabilityHint::code()));
        assert_eq!(
            pick_slot(&r_slow, true, false, false, None),
            SlotTarget::Primary
        );
    }

    #[test]
    fn non_code_hint_does_not_pick_code_even_when_configured() {
        // The extension hint `x:prose` must not accidentally trip
        // the code dispatch — only the standardized `code` hint
        // should.
        let hint = CapabilityHint::extension("prose").unwrap();
        let r = req(Speed::Slow, Some(hint));
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Primary);
    }

    #[test]
    fn extras_match_wins_over_speed_routing() {
        // Operator-declared per-phase routing must override the
        // Speed-based heuristic. A Fast-speed request with
        // model_id matching an extras slot lands on the extras
        // slot, NOT on the fast slot.
        let r = req(Speed::Fast, None);
        assert_eq!(
            pick_slot(&r, true, false, false, Some("reasoning".into())),
            SlotTarget::Extra("reasoning".into())
        );
    }

    // ── Forced-choice sentinel backstop (SLOT_POLICY §6) ──────────

    fn forced_choice_req(speed: Speed, labels: &[&str]) -> CompletionRequest {
        let mut r = req(speed, None);
        // The sentinel shape: max_tokens=1, enum candidates, and the
        // `x_forced_choice` marker. max_tokens=1 + Fast would satisfy
        // the FastShort gate, which is exactly the trap the backstop
        // guards against.
        r.max_tokens = Some(1);
        r.structured_output = Some(serde_json::json!({
            "type": "string",
            "enum": labels,
            "x_forced_choice": true,
        }));
        r
    }

    #[test]
    fn forced_choice_sentinel_routes_to_primary_beating_fast_short() {
        // A Fast-labelled, max_tokens=1 sentinel would land on FastShort
        // under the ordinary gates — but calibrated logprobs need the
        // primary model, so the backstop (which precedes the FastShort
        // gate) sends it to Primary.
        let r = forced_choice_req(Speed::Fast, &["A", "B", "C"]);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Primary,
            "forced-choice sentinel must beat the FastShort gate to reach Primary"
        );
    }

    #[test]
    fn forced_choice_sentinel_without_primary_falls_through_to_fast_short() {
        // Primary-less host: the backstop is a no-op and the request
        // falls through to the best available slot (FastShort here)
        // rather than failing.
        let r = forced_choice_req(Speed::Fast, &["yes", "no"]);
        assert_eq!(
            pick_slot(&r, false, false, true, None),
            SlotTarget::FastShort,
            "with no primary, the sentinel falls through to the normal gates"
        );
    }

    #[test]
    fn forced_choice_sentinel_with_empty_enum_is_not_special() {
        // A malformed sentinel (empty candidate set) is NOT forced-choice
        // — `forced_choice_candidates` returns None — so it routes by the
        // ordinary gates (FastShort for a tiny Fast request), never
        // hijacking Primary.
        let r = forced_choice_req(Speed::Fast, &[]);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort,
            "empty-enum sentinel is not treated as forced-choice"
        );
    }

    #[test]
    fn extras_match_wins_over_code_hint() {
        // Even a code-hinted request defers to an explicit extras
        // routing — the operator's declared model recruitment is
        // higher-precedence than any heuristic.
        let r = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(
            pick_slot(&r, true, true, false, Some("bulk".into())),
            SlotTarget::Extra("bulk".into())
        );
    }

    #[test]
    fn no_extras_match_falls_through_to_speed_routing() {
        // Untagged request OR tagged request whose model_id missed
        // the extras lookup → existing Speed/code rules apply
        // unchanged. Locks the back-compat invariant.
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Primary);
    }
}

#[cfg(test)]
mod eviction_tests {
    //! Exercises the LRU eviction selection algorithm. Pure and
    //! lock-free — runs without loading any real llama.cpp model.
    use super::{pick_evictions, EvictionCandidate, EvictionPlan};

    fn cand(name: &str, last_used_ms: u64, size_mb: u64) -> EvictionCandidate {
        EvictionCandidate {
            slot_name: name.into(),
            last_used_ms,
            size_bytes: size_mb * 1024 * 1024,
        }
    }

    #[test]
    fn fits_returns_no_eviction_needed() {
        // current 5 GB + new 3 GB = 8 GB ≤ 12 GB budget → Fits.
        let c = vec![cand("warm", 1000, 5 * 1024)];
        let plan = pick_evictions(
            &c,
            5 * 1024 * 1024 * 1024,
            3 * 1024 * 1024 * 1024,
            12 * 1024 * 1024 * 1024,
        );
        assert_eq!(plan, EvictionPlan::Fits);
    }

    #[test]
    fn evicts_coldest_slot_first() {
        // Two candidates: "old" used at t=100, "fresh" used at t=999.
        // Need to free 1 MB. Algorithm picks the colder one ("old")
        // even though "fresh" alone would also free enough — the
        // ordering is the contract.
        let c = vec![cand("fresh", 999, 5), cand("old", 100, 5)];
        // current 10 MB + new 4 MB = 14, budget 12 → need to free 2 MB
        let plan = pick_evictions(&c, 10 * 1024 * 1024, 4 * 1024 * 1024, 12 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["old".to_string()]);
            }
            other => panic!("expected Evict, got {other:?}"),
        }
    }

    #[test]
    fn evicts_multiple_when_one_isnt_enough() {
        // Three cold slots, all 1 MB. Need to free 3 MB. Walks LRU
        // order ("a" → "b" → "c"). Stops once enough freed.
        let c = vec![cand("c", 300, 1), cand("b", 200, 1), cand("a", 100, 1)];
        // current 3 MB + new 5 MB = 8, budget 5 → need 3 MB
        let plan = pick_evictions(&c, 3 * 1024 * 1024, 5 * 1024 * 1024, 5 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(
                    names,
                    vec!["a".to_string(), "b".to_string(), "c".to_string()]
                );
            }
            other => panic!("expected Evict, got {other:?}"),
        }
    }

    #[test]
    fn stops_evicting_once_enough_freed() {
        // Multiple candidates available, but only the coldest's
        // capacity is needed. Stops early to preserve the warmer
        // slots.
        let c = vec![
            cand("a", 100, 10), // coldest, 10 MB — alone enough
            cand("b", 200, 10),
            cand("c", 300, 10),
        ];
        // current 30 MB + new 5 MB = 35, budget 30 → need 5 MB
        let plan = pick_evictions(&c, 30 * 1024 * 1024, 5 * 1024 * 1024, 30 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["a".to_string()]);
            }
            other => panic!("expected single eviction, got {other:?}"),
        }
    }

    #[test]
    fn insufficient_when_cold_capacity_too_small() {
        // Only 1 MB of cold inventory, but need to free 5 MB.
        // Algorithm reports the shortfall so the caller can
        // surface an error.
        let c = vec![cand("a", 100, 1)];
        // current 1 MB + new 10 MB = 11, budget 5 → need 6 MB but
        // cold total is only 1 MB.
        let plan = pick_evictions(&c, 1024 * 1024, 10 * 1024 * 1024, 5 * 1024 * 1024);
        match plan {
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                assert_eq!(need_to_free, 6 * 1024 * 1024);
                assert_eq!(cold_total, 1024 * 1024);
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn empty_candidates_with_overflow_returns_insufficient() {
        // Edge case: budget would be exceeded but no cold slots
        // available (everything is busy in-flight). Caller can't
        // load.
        let c = vec![];
        let plan = pick_evictions(&c, 10 * 1024 * 1024, 5 * 1024 * 1024, 8 * 1024 * 1024);
        match plan {
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                assert_eq!(need_to_free, 7 * 1024 * 1024);
                assert_eq!(cold_total, 0);
            }
            other => panic!("expected Insufficient on empty cold, got {other:?}"),
        }
    }

    #[test]
    fn ties_break_deterministically_by_input_order() {
        // Two candidates with the same last_used. The algorithm
        // sorts by last_used; for equal keys, sort_by_key is
        // stable, so input order wins. Lock the contract — a
        // future change to unstable sort would surface here.
        let c = vec![cand("first", 100, 1), cand("second", 100, 1)];
        // current 2 MB + new 1 MB = 3, budget 1 → need 2 MB.
        let plan = pick_evictions(&c, 2 * 1024 * 1024, 1024 * 1024, 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["first".to_string(), "second".to_string()]);
            }
            other => panic!("expected Evict on tie, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod clamp_tests {
    use super::clamp_max_tokens;

    #[test]
    fn defaults_to_remaining_context_when_unspecified() {
        // Caller didn't ask for a cap — give them the whole window.
        assert_eq!(clamp_max_tokens(None, 1000, 8192).unwrap(), 7192);
    }

    #[test]
    fn passes_through_when_request_fits() {
        assert_eq!(clamp_max_tokens(Some(2000), 1000, 8192).unwrap(), 2000);
    }

    #[test]
    fn clamps_when_request_exceeds_headroom() {
        // Coding-agent worst case: opencode asks for 4096 on a tight
        // window. Pre-fix: returned Err. Post-fix: emits as much as
        // fits.
        assert_eq!(clamp_max_tokens(Some(4096), 30000, 32768).unwrap(), 2768);
    }

    #[test]
    fn errs_only_when_prompt_alone_exhausts_context() {
        let err = clamp_max_tokens(Some(100), 8192, 8192).unwrap_err();
        // The wording is the user-visible error string; lock the
        // hint phrasing so it stays actionable.
        let msg = format!("{err}");
        assert!(msg.contains("Prompt too long"), "{msg}");
    }

    #[test]
    fn errs_when_prompt_is_strictly_oversized() {
        assert!(clamp_max_tokens(None, 9000, 8192).is_err());
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
