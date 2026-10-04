// SPDX-License-Identifier: AGPL-3.0-or-later
//! THE tool-call protocol for this crate's text-protocol agentic loops.
//!
//! Every loop that lets a model call a tool drives the same envelope —
//! `<tool_call>{"name":..,"arguments":{..}}</tool_call>` interleaved with prose
//! — exposes the scoped tools via
//! [`CompletionRequest::tools`](crate::types::CompletionRequest), and feeds each
//! tool's result back into the next iteration's transcript. One parser, one
//! schema projection, one result formatter, one result count. Four loops share
//! them: [`recipe_author`](crate::runtime::handlers::recipe_author),
//! [`Executor::execute_delegate`](crate::executor::Executor),
//! `Executor::execute_reason_with_tools` and the attached-document turn.
//!
//! # The fork this replaced (2026-09-08)
//!
//! Until this module became the only protocol there was a second one: a
//! search-shaped `<tool_call>{"tool","query"}` envelope in `executor.rs` and a
//! third parser in `runtime/attached_doc_render.rs`. It discarded every
//! parameter but `query`, so it could not drive an actuator; and it rendered a
//! tool's `StepOutput::Json` by reading `.get("answer")` and substituting the
//! literal string `"No results."` when the key was absent. `answer` is emitted
//! by exactly one tool (`SearchTool`), so every OTHER tool that returned rows
//! delivered the string `"No results."` to the model and recorded zero hits in
//! the search log. Watched failing 2026-09-08: a `knowledge_lookup` call that
//! returned one evidence row put `No results.` in the model's next prompt
//! (`functional.rs::reason_with_tools_delivers_a_json_tools_evidence_to_the_model`).
//!
//! The lesson the header should carry: a renderer that reads ONE tool's key is
//! a schema guess wearing a formatter's clothes. [`format_step_output`] knows
//! no tool's schema, and that is why it cannot lose evidence.

use serde_json::Value as JsonValue;

use crate::types::{StepOutput, ToolDescriptor, ToolSchema};

/// A tool call parsed from assistant text: a tool `name` plus its full
/// `arguments` object. The arguments reach the tool intact — nothing here
/// projects them down to a single field.
#[derive(Debug, Clone)]
pub(crate) struct ParsedToolCall {
    pub name: String,
    pub arguments: JsonValue,
}

impl ParsedToolCall {
    /// The call's `query` argument when it has one, else the whole argument
    /// object rendered compactly.
    ///
    /// For the provenance log only (`SearchLogEntry::query`, the narration
    /// line a user reads). Never for dispatch: the tool is handed
    /// [`Self::arguments`] whole. A tool whose parameters are not a `query`
    /// therefore still logs something a reader can act on, instead of the
    /// empty string the search-shaped loop recorded for it.
    pub fn query_arg(&self) -> String {
        match self.arguments.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => self.arguments.to_string(),
        }
    }
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
    let v: JsonValue = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "tool_loop", error = %e, "tool_call body is not JSON");
            return None;
        }
    };
    let name = match v.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_string(),
        None => {
            // Reported, never defaulted (ARCH §18.3). The shape most likely to
            // land here is the retired search envelope `{"tool":..,"query":..}`
            // — a model still copying an old worked example. Guessing a `name`
            // from `tool` would resurrect the second protocol; naming the miss
            // makes it a prompt bug the next run can see.
            tracing::warn!(
                target: "tool_loop",
                body = %body.trim().chars().take(200).collect::<String>(),
                "tool_call envelope has no `name` field — call dropped"
            );
            return None;
        }
    };
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

/// How a loop wants a `StepOutput::Text` spliced into its transcript.
///
/// The ONE thing the four loops legitimately disagree about, so it is a
/// declared parameter rather than four renderers (ARCH principle 8). Every
/// other variant — and in particular `Json` — renders identically for all of
/// them, which is the property that stops a tool's evidence from depending on
/// which loop called it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextEnvelope {
    /// `{"text": …}` — the shape the live-trial harness sends back over the
    /// OICP wire, so a transcript of `Result: {…}` lines is uniformly JSON and
    /// the parser side never special-cases.
    Wire,
    /// The text verbatim. For loops that splice a result into prose the model
    /// reads directly — the retrieval loop's `[Search results for "q"]:` block
    /// and the attached-document turn, whose `[Source …]` labels the model is
    /// instructed to cite back and which JSON-escaping would mangle.
    Prose,
}

/// Render a tool's `StepOutput` as the text the agent's next turn will see.
///
/// **Knows no tool's schema, by construction.** A `Json` value passes through
/// whole, so a tool that returned rows delivers those rows whatever its keys
/// are. The predecessor of this function read `.get("answer")` and substituted
/// `"No results."` — see this module's header for what that cost.
pub(crate) fn format_step_output(out: &StepOutput, envelope: TextEnvelope) -> String {
    match out {
        StepOutput::Json(v) => v.to_string(),
        StepOutput::Text(t) => match envelope {
            TextEnvelope::Wire => serde_json::json!({ "text": t }).to_string(),
            TextEnvelope::Prose => t.clone(),
        },
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

/// How many results a tool handed back — the number a loop records as
/// `SearchLogEntry::result_count` and the UI shows as "hits returned".
///
/// Schema-free on purpose, and the same decider for every shape (ARCH §10.6).
/// The predecessor counted `"[Source"` occurrences in the rendered string,
/// which is a convention of the corpus-search tools' TEXT output and is absent
/// from every JSON envelope — so a `knowledge_lookup` call that returned two
/// evidence rows was recorded as zero hits, and `0` is documented to mean "a
/// dead-end search the model had to route around".
///
/// - `Text` keeps the `[Source` convention where it applies, and otherwise
///   counts non-empty text as one result.
/// - `Json` counts the members of the value's array fields — one row per
///   returned item for every envelope this codebase emits (`evidence`,
///   `results`, `sources`, `chunks`). An envelope that HAS a list is described
///   by it even when it is empty — a search that found nothing is zero hits,
///   not one because it echoed its query back. Only an envelope with NO list
///   falls back to "one result" (a figure-bearing tool like
///   `parcel_analytics`); `null` and `{}` are zero. It is a cardinality, not a
///   claim about relevance.
pub(crate) fn result_cardinality(out: &StepOutput) -> usize {
    match out {
        StepOutput::Text(t) => {
            let sources = t.matches("[Source").count();
            if sources > 0 {
                sources
            } else {
                usize::from(!t.trim().is_empty())
            }
        }
        StepOutput::Json(v) => json_cardinality(v),
        StepOutput::ReasonWithToolsResult { text, .. } => usize::from(!text.trim().is_empty()),
        StepOutput::Jump(_) | StepOutput::Skipped => 0,
    }
}

fn json_cardinality(v: &JsonValue) -> usize {
    match v {
        JsonValue::Null => 0,
        JsonValue::Array(a) => a.len(),
        JsonValue::Object(o) => {
            // An envelope that HAS a list is described by that list's length,
            // including when it is empty: `{"query":"q","evidence":[]}` is a
            // search that found nothing, and counting its `query` string as a
            // result would report a hit for a miss.
            let mut lists = o.values().filter_map(|x| x.as_array()).peekable();
            if lists.peek().is_some() {
                lists.map(|a| a.len()).sum()
            } else if o.values().any(|x| !x.is_null()) {
                // No list at all — a figure-bearing envelope like
                // `parcel_analytics`. One result, not zero.
                1
            } else {
                0
            }
        }
        _ => 1,
    }
}

/// The worked `<tool_call>` line a prompt shows, built from the tools that are
/// actually on offer.
///
/// `None` when nothing is offered — a prompt with no tools must not print an
/// example naming one, which is the exact defect this replaced: the retrieval
/// prompt hardcoded `{"tool":"search",…}` whatever `available_tools` held, so
/// a model offered only `knowledge_lookup` emitted `search` first and was told
/// `Tool 'search' not available.` (measured 6 of 9 replays, 2026-09-07).
///
/// Arguments come from the tool's own declaration, cheapest source first: a
/// manifest [`ToolExample`](crate::types::ToolExample), else its required
/// parameters, else an empty object. One decider — the manifest — so an
/// example cannot drift from the schema the tool validates against.
pub(crate) fn worked_example(descriptors: &[ToolDescriptor]) -> Option<String> {
    let d = descriptors.first()?;
    let args = d
        .examples
        .first()
        .map(|ex| ex.call.clone())
        .unwrap_or_else(|| example_args_from_schema(&d.parameters));
    Some(format!(
        "<tool_call>{}</tool_call>",
        serde_json::json!({ "name": d.id, "arguments": args })
    ))
}

/// Placeholder arguments for a tool with no declared example: one entry per
/// required parameter, valued by its declared type so the model copies the
/// right SHAPE rather than a literal it might repeat.
fn example_args_from_schema(schema: &JsonValue) -> JsonValue {
    let props = schema.get("properties").and_then(|p| p.as_object());
    let required: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut out = serde_json::Map::new();
    for key in required {
        let ty = props
            .and_then(|p| p.get(&key))
            .and_then(|s| s.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("string");
        let placeholder = match ty {
            "integer" | "number" => serde_json::json!(1),
            "boolean" => serde_json::json!(true),
            "array" => serde_json::json!([]),
            "object" => serde_json::json!({}),
            _ => serde_json::json!(format!("<{key}>")),
        };
        out.insert(key, placeholder);
    }
    JsonValue::Object(out)
}

/// A tool's own compact prose rendering of its result, when it published one.
///
/// The ONE place that knows a tool may pre-render itself, and it knows exactly
/// one key. Everything else — including deciding what to do when the key is
/// absent — belongs to the caller, which falls back to
/// [`format_step_output`] and therefore never loses the value.
///
/// # Why any key at all
///
/// `parcel_analytics` returns figures like `1477806471.0`. A model asked to
/// narrate from the raw envelope cannot retype those faithfully and corrupts
/// them into digit-salad, so the tool publishes a `summary` of compact,
/// pre-cited figures it CAN copy and the exact derivation is appended verbatim
/// downstream. That is a measured reason for one key on one shape.
///
/// It read `answer` first until 2026-09-08 — `SearchTool`'s key, and the same
/// guess the two retired tool-loop renderers were built on. Dropped: a
/// `search` step's full envelope carries the synthesized answer AND its
/// provenance (`search_method`, `sources`, result counts), which is strictly
/// more for the synthesizer and carries no long figures to mangle.
pub(crate) fn narratable_summary(out: &StepOutput) -> Option<&str> {
    match out {
        StepOutput::Json(v) => v
            .get("summary")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        _ => None,
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
    fn json_output_reaches_the_model_whole_whatever_its_keys() {
        // The named failing input (ARCH §18.1): an envelope with no `answer`
        // key. The retired renderer turned this into the literal string
        // "No results."; the shared one must carry every row through.
        let out = StepOutput::Json(serde_json::json!({
            "evidence": [{"id": "ev-0001", "content": "a passage"}],
            "by_kind_counts": {"corpus": 1},
        }));
        for envelope in [TextEnvelope::Wire, TextEnvelope::Prose] {
            let rendered = format_step_output(&out, envelope);
            assert!(rendered.contains("ev-0001"), "{envelope:?}: {rendered}");
            assert!(rendered.contains("a passage"), "{envelope:?}: {rendered}");
            assert!(!rendered.contains("No results"), "{envelope:?}: {rendered}");
        }
    }

    #[test]
    fn the_text_envelope_is_the_only_thing_the_loops_disagree_about() {
        let text = StepOutput::Text("[Source: sep] a passage".to_string());
        assert_eq!(
            format_step_output(&text, TextEnvelope::Prose),
            "[Source: sep] a passage",
            "prose loops splice the text verbatim so cite-back labels survive"
        );
        assert_eq!(
            format_step_output(&text, TextEnvelope::Wire),
            r#"{"text":"[Source: sep] a passage"}"#,
            "wire loops keep the OICP shape"
        );
        // Every other variant renders identically for both.
        let json = StepOutput::Json(serde_json::json!({"k": [1, 2]}));
        assert_eq!(
            format_step_output(&json, TextEnvelope::Wire),
            format_step_output(&json, TextEnvelope::Prose)
        );
    }

    #[test]
    fn result_cardinality_counts_rows_a_json_tool_returned() {
        // The failing input: two evidence rows counted as zero hits, which is
        // documented to mean "a dead-end search the model had to route around".
        let two = StepOutput::Json(serde_json::json!({
            "query": "q",
            "evidence": [{"id": "ev-0001"}, {"id": "ev-0002"}],
            "by_kind_counts": {"corpus": 2},
        }));
        assert_eq!(result_cardinality(&two), 2);

        // An empty envelope is honestly zero, not one.
        let none = StepOutput::Json(serde_json::json!({"query": "q", "evidence": []}));
        assert_eq!(result_cardinality(&none), 0);

        // A figure-bearing object with no list is one result, not zero.
        let figures = StepOutput::Json(serde_json::json!({"neutral_rate": 0.021}));
        assert_eq!(result_cardinality(&figures), 1);

        // The `[Source` convention still holds where it applies.
        assert_eq!(
            result_cardinality(&StepOutput::Text("[Source: a] x [Source: b] y".to_string())),
            2
        );
        // Prose with no labels is one result, and empty text is none.
        assert_eq!(
            result_cardinality(&StepOutput::Text("no matches".to_string())),
            1
        );
        assert_eq!(result_cardinality(&StepOutput::Text("  ".to_string())), 0);
        assert_eq!(result_cardinality(&StepOutput::Skipped), 0);
    }

    #[test]
    fn a_body_with_no_name_is_dropped_not_guessed() {
        // The retired search envelope. Reading `tool` as `name` here would be
        // the second protocol coming back in through the parser.
        let text = r#"<tool_call>{"tool":"search","query":"x"}</tool_call>"#;
        let (_, calls) = parse_assistant_text(text);
        assert!(calls.is_empty(), "search-shaped envelope must not parse");
    }

    #[test]
    fn the_worked_example_names_a_tool_that_is_actually_offered() {
        use crate::types::{Effect, Idempotency, Latency, Scope, ToolExample};
        let d = |id: &str, params: serde_json::Value, examples: Vec<ToolExample>| ToolDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            parameters: params,
            examples,
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: None,
        };

        // No tools offered — no example. The prompt must not name a tool the
        // step did not offer; that is the defect this replaced.
        assert_eq!(worked_example(&[]), None);

        // Required params with no declared example: shape comes from the schema.
        let from_schema = worked_example(&[d(
            "knowledge_lookup",
            serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {"query": {"type": "string"}},
            }),
            vec![],
        )])
        .expect("one tool offered yields one example");
        assert!(
            from_schema.contains(r#""name":"knowledge_lookup""#),
            "{from_schema}"
        );
        assert!(
            from_schema.contains(r#""query":"<query>""#),
            "{from_schema}"
        );
        assert!(!from_schema.contains("search"), "{from_schema}");

        // A declared example wins over the synthesised one.
        let from_manifest = worked_example(&[d(
            "parcel_analytics",
            serde_json::json!({"type": "object", "required": ["corpus"]}),
            vec![ToolExample {
                situation: "s".to_string(),
                call: serde_json::json!({"corpus": "sf-assessor-roll"}),
            }],
        )])
        .expect("example");
        assert!(
            from_manifest.contains("sf-assessor-roll"),
            "{from_manifest}"
        );
    }

    #[test]
    fn narratable_summary_is_one_key_and_absence_is_absence() {
        let with = StepOutput::Json(serde_json::json!({
            "summary": "Land value $1.48B across 12 parcels.",
            "neutral_rate": 0.0213,
        }));
        assert_eq!(
            narratable_summary(&with),
            Some("Land value $1.48B across 12 parcels.")
        );
        // The failing input: `answer` was read here until 2026-09-08, so a
        // search envelope short-circuited the fallback and its provenance was
        // dropped. It must now be absent, and the caller renders the whole
        // value instead.
        let search_shaped = StepOutput::Json(serde_json::json!({
            "answer": "Bergson argued …",
            "sources": [{"origin": "sep", "count": 3}],
        }));
        assert_eq!(narratable_summary(&search_shaped), None);
        // Present-but-empty is absent, not an empty narration.
        let blank = StepOutput::Json(serde_json::json!({"summary": "   "}));
        assert_eq!(narratable_summary(&blank), None);
        assert_eq!(narratable_summary(&StepOutput::Text("x".into())), None);
    }

    #[test]
    fn query_arg_falls_back_to_the_whole_argument_object() {
        let (_, calls) = parse_assistant_text(
            r#"<tool_call>{"name":"a","arguments":{"query":"birds"}}</tool_call>"#,
        );
        assert_eq!(calls[0].query_arg(), "birds");
        let (_, calls) = parse_assistant_text(
            r#"<tool_call>{"name":"b","arguments":{"path":"/x","depth":2}}</tool_call>"#,
        );
        // An actuator's call still logs something a reader can act on, rather
        // than the empty string the `{query}` shorthand recorded for it.
        assert!(
            calls[0].query_arg().contains("/x"),
            "{}",
            calls[0].query_arg()
        );
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
