// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-author streaming agent-loop handler.
//!
//! The recipe-author workspace is a long-lived tool-using loop: the
//! agent reads the project, drafts the recipe, validates, tests, and
//! checkpoints — every meaningful turn is a sequence of tool calls,
//! not a single synthesis pass. The generic `handle_complex_task`
//! planner dispatches a step DAG instead, which (a) hands the planner
//! the wrong catalog when the pre-classification narrow misses the
//! mode tag, and (b) emits opaque step plans (`cat recipe.toml`)
//! that the desktop has no UI to gate.
//!
//! This handler ports the algorithm proven in
//! `sovereign-cli-llm::recipe_agent_live_trial`. The trial harness
//! posts to the daemon's `/v1/chat/completions`; here we call
//! `InferenceProvider::complete` directly with `tools: Some(...)` so
//! the embedded chat-template path injects a `<tools>...</tools>` block
//! into the system prompt. The model replies with
//! `<tool_call>{"name":...,"arguments":{...}}</tool_call>` blocks
//! interleaved with prose; we parse them out, dispatch via
//! [`ToolRegistry`], append results back into the running transcript,
//! and complete again until the model emits a tool-call-free turn or
//! we hit the iteration cap.
//!
//! Streaming: the embedded `complete` path is non-streaming for
//! tool-using turns (tool calls can't be parsed from an in-flight
//! token stream). We emit the final assistant text as a single chunk
//! via [`StreamHandle`]; per-iteration progress is observable via
//! tracing logs (`recipe_author_loop:*`).

use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::error::{Error, Result};

use super::super::*;

/// Cap iterations per turn so a runaway loop can't burn the slot.
/// 12 matches the live-trial harness default — leaves headroom for
/// browse → read → write → validate → write → validate → test →
/// write → validate → test → checkpoint.
const MAX_TOOL_ITERATIONS: usize = 12;

/// Per-iteration `max_tokens`. Generous enough for the model to emit
/// a multi-block tool envelope plus the prose explanation it owes
/// the partner.
const ITERATION_MAX_TOKENS: usize = 4096;

#[derive(Debug, Clone)]
struct ParsedToolCall {
    name: String,
    arguments: JsonValue,
}

impl Runtime {
    /// Streaming dispatch for a turn in a recipe-author workspace.
    /// Runs the agent loop in a spawned task and returns a
    /// [`StreamHandle`] whose single chunk is the final assistant
    /// text. The assistant `Message` is persisted post-emit so the
    /// next turn sees the prior reply in `context.conversation`.
    pub(crate) async fn handle_recipe_author_turn_stream(
        &self,
        skill_id: &str,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<StreamHandle> {
        // Generic over authoring skills (recipe-author, workflow-author): the loop
        // is driven entirely by the named skill's `[prompts] synthesis` + the
        // registered tool descriptors handed in. `skill_id` selects the prompt.
        let base_prompt = self
            .skills
            .skill_by_id(skill_id)
            .and_then(|s| s.prompts.synthesis.clone())
            .ok_or_else(|| {
                Error::NotImplemented(format!(
                    "{skill_id} skill not loaded or `[prompts] synthesis` missing. \
                     Check that the daemon can resolve modes/{skill_id}/skill.toml \
                     (set SOVEREIGN_MODES_DIR or run from the workspace root)."
                ))
            })?;
        // Agent-loop-only addendum. The skill.toml prompt is shared
        // with non-loop callers; the `done` virtual tool is a
        // handler-side concept (we intercept it, never dispatch),
        // so the instruction lives here next to the inject_done_variant
        // schema augmentation rather than in the canonical prompt.
        let system_prompt = format!(
            "{base_prompt}\n\n\
             ──── Termination ──────────────────────────────────────────\n\
             When the partner's ask is fully addressed for THIS turn — \
             you've drafted/fixed/tested/checkpointed what they asked \
             for, OR you need their answer before continuing — call the \
             `done` tool. `arguments.reason` is your one- to three-\n\
             sentence partner-facing reply (what you did, what's open, \
             what you need from them). The `done` call ends the turn; \
             nothing else fires after it.\n\
             \n\
             Do NOT re-validate or re-test something that already passed \
             this turn just to fill iterations. After two tool calls \
             whose results converge (passing validation, no new \
             information), call `done`.\n"
        );

        if tool_descriptors.is_empty() {
            return Err(Error::NotImplemented(
                "recipe-author dispatch reached with an empty tool catalog. \
                 The narrowed recipe-author tools (recipe_write_structured, \
                 recipe_validate, recipe_test, etc.) are not registered on this \
                 runtime — check sovereign-desktop tool wiring."
                    .into(),
            ));
        }

        // Project the narrowed tool descriptors into the OpenAI
        // ToolSchema shape the embedded chat-template path consumes.
        let tool_schemas: Vec<ToolSchema> = tool_descriptors
            .iter()
            .map(|d| ToolSchema {
                name: d.id.clone(),
                description: Some(d.description.clone()),
                parameters: d.parameters.clone(),
            })
            .collect();

        // Lark alternation grammar for the per-iteration completion.
        // Built once outside the loop — the schema is stable for the
        // turn since the catalog doesn't change mid-turn.
        //
        // Why: pre-llguidance runs (2026-05-23 smoke) had the model
        // looping on `recipe_write_structured` calls because each
        // attempt emitted JSON whose TOML conversion was malformed —
        // 4-6 iterations of "write malformed → write again" with no
        // recipe_validate ever firing. Grammar-constrained sampling
        // closes the malformation class structurally: every
        // `<tool_call>` body is a JSON object that satisfies one of
        // the per-tool schemas, so `recipe_write_structured` receives
        // a recipe object that round-trips to valid TOML on the
        // first try. The grammar also carries a `plain_text` branch
        // so the model can still emit a partner-facing summary when
        // no tool call is appropriate.
        //
        // Pattern lifted from sovereign-agent-bench's proven path
        // (`SOVEREIGN_ALTERNATION_GRAMMAR=1` route in
        // `sovereign-mesh::inference_adapter`); we replicate the
        // schema builder + Lark string locally because sovereign-core
        // can't take a sovereign-inference dep (cycle).
        let envelope_schema = build_envelope_schema(&tool_schemas).map(inject_done_variant);
        let lark_grammar_string = envelope_schema.as_ref().map(|schema| {
            let schema_json = serde_json::to_string(schema).unwrap_or_default();
            build_tool_alternation_grammar(&schema_json)
        });

        tracing::info!(
            conversation_id,
            tools = tool_schemas.len(),
            grammar_enabled = lark_grammar_string.is_some(),
            grammar_chars = lark_grammar_string.as_ref().map(|s| s.len()).unwrap_or(0),
            "recipe_author_loop: dispatch begin"
        );

        let message_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<String>>();

        let inference = Arc::clone(&self.inference);
        let tools = Arc::clone(&self.tools);
        let store = Arc::clone(&self.store);
        let conv_id_owned = conversation_id.to_string();
        let msg_id_owned = message_id.clone();
        let prior_messages: Vec<Message> = context.conversation.messages.clone();
        let message_owned = message.to_string();
        let lark_grammar_for_loop = lark_grammar_string;

        tokio::spawn(async move {
            let loop_start = std::time::Instant::now();
            let mut transcript = render_transcript(&prior_messages, &message_owned);
            let mut final_text = String::new();
            let mut tool_calls_total = 0usize;
            let mut last_model_id: Option<String> = None;
            let mut error_for_user: Option<String> = None;
            let mut completed_cleanly = false;
            // Surface artifacts back to the partner when the loop
            // doesn't terminate cleanly: the last recipe path the
            // agent wrote and the last validation result. The
            // fallback message includes both so the partner has a
            // concrete next step (open the path, edit the TOML) even
            // when the agent didn't summarise.
            let mut last_recipe_path: Option<String> = None;
            let mut last_validation_summary: Option<String> = None;

            for iter in 0..MAX_TOOL_ITERATIONS {
                // Tool-choice flips to "required" when the alternation
                // grammar carries an envelope schema: the grammar
                // itself permits a plain-text exit branch, so the
                // sampler never gets stuck in a forced-tool-call loop,
                // and the daemon's adapter path treats "required" as
                // the signal to install schema-aware constraints
                // (consistent with the agent-bench convention).
                let tool_choice_value = if lark_grammar_for_loop.is_some() {
                    serde_json::json!("required")
                } else {
                    serde_json::json!("auto")
                };
                let request = CompletionRequest {
                    prompt: transcript.clone(),
                    system_message: Some(system_prompt.clone()),
                    preferred_speed: Speed::Slow,
                    max_tokens: Some(ITERATION_MAX_TOKENS),
                    temperature: Some(0.0),
                    think_budget: Some(0),
                    structured_output: None,
                    top_k: None,
                    top_p: None,
                    oicp: None,
                    tools: Some(tool_schemas.clone()),
                    tool_choice: Some(tool_choice_value),
                    model_id: None,
                    enable_thinking: Some(false),
                    sampling_mode: Some(SamplingMode::Instruct),
                    assistant_prefix: None,
                    cmd_prefix: None,
                    url_allowlist: None,
                    evidence_id_allowlist: None,
                    lark_grammar: lark_grammar_for_loop.clone(),
                };

                let response = match inference.complete(&request).await {
                    Ok(r) => r,
                    Err(e) => {
                        error_for_user = Some(format!(
                            "Recipe Author hit an inference error on iteration {iter}: {e}"
                        ));
                        break;
                    }
                };
                last_model_id = Some(response.model_id.clone());

                let (visible_text, tool_calls) = parse_assistant_text(&response.text);

                tracing::debug!(
                    conversation_id = %conv_id_owned,
                    iteration = iter,
                    visible_chars = visible_text.len(),
                    tool_calls = tool_calls.len(),
                    model_id = %response.model_id,
                    latency_ms = response.latency_ms,
                    "recipe_author_loop: iteration response"
                );

                if tool_calls.is_empty() {
                    final_text = visible_text;
                    completed_cleanly = true;
                    break;
                }

                // Stitch the model's visible text + the tool envelope
                // back into the running transcript so the next
                // iteration's prompt grounds in what just happened.
                // This is the same shape the live-trial harness uses
                // when re-posting to /v1/chat/completions; here we
                // bake it into the prompt string because we're not
                // round-tripping through the OICP message list.
                if !visible_text.trim().is_empty() {
                    transcript.push(' ');
                    transcript.push_str(visible_text.trim());
                    transcript.push('\n');
                }

                let mut done_signal = false;
                for call in &tool_calls {
                    // `done` is a virtual tool — never dispatched. Pull
                    // the partner-facing `reason` out as the final
                    // assistant text and short-circuit the loop. This
                    // is the model's structured termination signal,
                    // installed via `inject_done_variant` on the
                    // envelope schema so llguidance lets the model
                    // pick it whenever no more tool work is needed.
                    if call.name == "done" {
                        let reason = call
                            .arguments
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        tracing::info!(
                            conversation_id = %conv_id_owned,
                            iteration = iter,
                            reason_chars = reason.len(),
                            "recipe_author_loop: done virtual tool"
                        );
                        final_text = if reason.is_empty() {
                            visible_text.clone()
                        } else if visible_text.trim().is_empty() {
                            reason
                        } else {
                            format!("{}\n\n{}", visible_text.trim(), reason)
                        };
                        completed_cleanly = true;
                        done_signal = true;
                        break;
                    }

                    let ctx = ToolContext {
                        conversation_id: conv_id_owned.clone(),
                        task_id: None,
                        working_directory: None,
                        in_reasoning_loop: true,
                        agent_session_token: None,
                        turn_index: iter,
                    };
                    let tool_call_start = std::time::Instant::now();
                    let exec_result = match tools.get(&call.name) {
                        Ok(tool) => tool.execute(&call.arguments, &ctx).await,
                        Err(e) => Err(e),
                    };
                    let result_str = match &exec_result {
                        Ok(out) => format_step_output(out),
                        Err(e) => serde_json::json!({
                            "error": e.to_string(),
                        })
                        .to_string(),
                    };

                    // Track artifacts so a stuck loop's fallback
                    // message can surface concrete next steps (recipe
                    // path + validation status) instead of just "I
                    // gave up". Inspecting StepOutput::Json keeps this
                    // transparent without bolting tool-specific
                    // signalling onto the Tool trait.
                    if let Ok(StepOutput::Json(ref v)) = exec_result {
                        if matches!(
                            call.name.as_str(),
                            "recipe_write" | "recipe_write_structured"
                        ) {
                            if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                                last_recipe_path = Some(p.to_string());
                            }
                        }
                        if call.name == "recipe_validate" {
                            let passed = v.get("passed").and_then(|p| p.as_bool()).unwrap_or(false);
                            let err_count = v
                                .get("errors")
                                .and_then(|e| e.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            last_validation_summary = Some(if passed {
                                "validation PASSED".to_string()
                            } else {
                                format!("validation FAILED ({err_count} error(s))")
                            });
                        }
                    }

                    tool_calls_total += 1;
                    tracing::info!(
                        conversation_id = %conv_id_owned,
                        iteration = iter,
                        tool = %call.name,
                        result_chars = result_str.len(),
                        latency_ms = tool_call_start.elapsed().as_millis() as u64,
                        "recipe_author_loop: tool executed"
                    );

                    let args_json =
                        serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());
                    transcript.push_str(&format!(
                        "<tool_call>{{\"name\":\"{}\",\"arguments\":{args_json}}}</tool_call>\n\
                         <tool_result>{result_str}</tool_result>\n\n[Agent]:",
                        call.name,
                    ));
                }
                if done_signal {
                    break;
                }
            }

            let final_content = if let Some(err_text) = error_for_user {
                err_text
            } else if !completed_cleanly {
                // Loop hit the iteration cap without a `done` signal.
                // Surface artifacts so the partner has a real exit
                // path even when the agent didn't summarise — open
                // the recipe TOML, see the validation status, edit
                // directly. This is the "no broken-box" guarantee:
                // we never leave the partner with a dead-end response.
                let mut msg = format!(
                    "I made {tool_calls_total} tool calls but didn't reach a clean \
                     stop this turn (iteration cap = {MAX_TOOL_ITERATIONS}). Here's \
                     what's on disk so you can take it from here:\n\n"
                );
                match (&last_recipe_path, &last_validation_summary) {
                    (Some(path), Some(val)) => {
                        msg.push_str(&format!("- Recipe: `{path}` ({val})\n"));
                    }
                    (Some(path), None) => {
                        msg.push_str(&format!("- Recipe: `{path}` (not validated this turn)\n"));
                    }
                    (None, Some(val)) => {
                        msg.push_str(&format!("- Last action: {val}\n"));
                    }
                    (None, None) => {
                        msg.push_str(
                            "- No recipe was written this turn — try a more \
                             focused ask like \"draft the recipe from the charter\".\n",
                        );
                    }
                }
                msg.push_str(
                    "\nThe recipe TOML is yours to edit directly. Tell me what \
                     to fix or paste your edits and I'll re-validate.",
                );
                if !final_text.trim().is_empty() {
                    msg.push_str("\n\n---\nAgent's last words this turn:\n");
                    msg.push_str(final_text.trim());
                }
                msg
            } else if final_text.trim().is_empty() {
                "(The agent finished cleanly without a partner-facing reply this turn.)".to_string()
            } else {
                final_text
            };

            tracing::info!(
                conversation_id = %conv_id_owned,
                tool_calls = tool_calls_total,
                completed_cleanly,
                final_chars = final_content.len(),
                total_latency_ms = loop_start.elapsed().as_millis() as u64,
                "recipe_author_loop: turn end"
            );

            // Emit the final chunk before persistence so the UI shows
            // the reply as soon as it's ready; persistence races behind.
            if tx.send(Ok(final_content.clone())).is_err() {
                tracing::debug!(
                    conversation_id = %conv_id_owned,
                    "recipe_author_loop: consumer dropped before final emit"
                );
                return;
            }

            let assistant_msg = Message {
                id: msg_id_owned,
                conversation_id: conv_id_owned,
                role: Role::Assistant,
                content: final_content,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "RecipeAuthor",
                    "tool_calls": tool_calls_total,
                    "completed_cleanly": completed_cleanly,
                    "model": last_model_id,
                })),
                version: now(),
            };
            if let Err(e) = store.save_message(&assistant_msg).await {
                tracing::warn!(error = %e, "recipe_author_loop: persist failed");
            }
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(StreamHandle {
            message_id,
            stream: Box::pin(stream),
        })
    }

    /// Non-streaming entry — drains the streaming handler into a
    /// single [`Response`]. The non-streaming `handle_message`
    /// dispatch path uses this so an OICP caller (mesh peer, CLI,
    /// future MCP route) gets the same shape as a chat-style caller.
    pub(crate) async fn handle_recipe_author_turn(
        &self,
        skill_id: &str,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        use futures::StreamExt;

        let handle = self
            .handle_recipe_author_turn_stream(
                skill_id,
                message,
                conversation_id,
                context,
                tool_descriptors,
            )
            .await?;
        let StreamHandle {
            message_id,
            mut stream,
        } = handle;

        let mut text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => text.push_str(&chunk),
                Err(e) => return Err(e),
            }
        }

        // The streaming variant persisted the assistant message inside
        // its spawned pump. We can't reach back into the Store for
        // exactly that message_id race-free here, so build a fresh
        // Message carrying the same id + text for the response shape.
        // Metadata stays minimal — the streaming-side persistence is
        // the authoritative record consumers should read for
        // model/tool counts.
        let assistant_msg = Message {
            id: message_id,
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: text,
            created_at: now(),
            metadata: Some(serde_json::json!({"intent": "RecipeAuthor"})),
            version: now(),
        };
        Ok(Response {
            message: assistant_msg,
            task: None,
            metrics: None,
        })
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// Render prior conversation as a single prompt-injectable transcript.
/// The recipe-author skill prompt expects partner/agent framing; using
/// `[Partner]` / `[Agent]` labels (rather than User/Assistant) keeps the
/// in-loop transcript consistent with the situated-context prelude the
/// desktop already injects on the partner side.
fn render_transcript(prior: &[Message], current_partner_message: &str) -> String {
    let mut out = String::with_capacity(2048);
    for m in prior {
        match m.role {
            Role::User => {
                out.push_str("[Partner]: ");
                out.push_str(&m.content);
                out.push_str("\n\n");
            }
            Role::Assistant => {
                out.push_str("[Agent]: ");
                out.push_str(&m.content);
                out.push_str("\n\n");
            }
            Role::System => {
                // System rolls into the skill prompt; skipped here so
                // it doesn't double-render in the transcript.
            }
        }
    }
    out.push_str("[Partner]: ");
    out.push_str(current_partner_message);
    out.push_str("\n\n[Agent]:");
    out
}

/// Strip `<think>...</think>` blocks and extract any
/// `<tool_call>{...}</tool_call>` envelopes from an assistant turn.
/// Returns `(visible_text, parsed_calls)`. Visible text has the tool
/// envelopes removed so the next iteration's transcript carries the
/// model's prose explanation without the JSON.
fn parse_assistant_text(text: &str) -> (String, Vec<ParsedToolCall>) {
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
fn format_step_output(out: &StepOutput) -> String {
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

/// Build a JSON-Schema envelope describing the per-tool oneOf
/// shape llguidance constrains the sampler against. Mirrors
/// `sovereign_mesh::inference_adapter::tool_envelope_schema_for`,
/// replicated here because sovereign-core can't depend on
/// sovereign-mesh (cycle).
///
/// Returns `None` when there are no tools OR when any tool's
/// parameters schema isn't an object (the JSON-Schema compiler
/// requires that for property nesting). Better to install no
/// constraint than a partial one — the loop falls back to
/// unconstrained sampling and the existing parser's leniency.
fn build_envelope_schema(tools: &[ToolSchema]) -> Option<serde_json::Value> {
    if tools.is_empty() {
        return None;
    }
    let mut variants: Vec<serde_json::Value> = Vec::with_capacity(tools.len());
    for t in tools {
        if !t.parameters.is_object() {
            return None;
        }
        let mut props = serde_json::Map::new();
        props.insert(
            "name".to_string(),
            serde_json::json!({ "type": "string", "enum": [&t.name] }),
        );
        props.insert("arguments".to_string(), t.parameters.clone());
        variants.push(serde_json::json!({
            "type": "object",
            "properties": props,
            "required": ["name", "arguments"],
            "additionalProperties": false,
        }));
    }
    Some(serde_json::json!({ "oneOf": variants }))
}

/// Append a virtual `done` tool variant to the envelope schema so the
/// model has a structured way to say "I'm finished — here's why."
/// The handler intercepts `name == "done"` and treats `arguments.reason`
/// as the partner-facing final text instead of dispatching to a real
/// tool. Pattern mirrored from `sovereign-mesh::inference_adapter::
/// inject_done_tool` (the agent-bench / pi alternation-grammar
/// adoption — same convention).
fn inject_done_variant(mut envelope: serde_json::Value) -> serde_json::Value {
    let done_variant = serde_json::json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "enum": ["done"]},
            "arguments": {
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description":
                            "One- to three-sentence summary of what \
                             you accomplished and what remains open. \
                             The partner sees this verbatim as your \
                             reply."
                    },
                },
                "required": ["reason"],
                "additionalProperties": false,
            },
        },
        "required": ["name", "arguments"],
        "additionalProperties": false,
    });
    if let Some(variants) = envelope.get_mut("oneOf").and_then(|v| v.as_array_mut()) {
        variants.push(done_variant);
    } else if let Some(variants) = envelope.get_mut("anyOf").and_then(|v| v.as_array_mut()) {
        variants.push(done_variant);
    }
    envelope
}

/// Lark alternation grammar `start: think_block? body` where body
/// is either a `<tool_call>{...}</tool_call>` envelope or plain
/// text. Lifted verbatim from
/// `sovereign_inference::llguidance_constraint::build_tool_alternation_grammar`
/// — replicated here for the same dep-cycle reason.
///
/// The literal `<tool_call>` / `</tool_call>` markers in the grammar
/// agree with the Qwen chat template's tool-call wrapper, so the
/// embedded path's marker-stop and llguidance's grammar end-condition
/// land on the same boundary. The `plain_text` branch's first-byte
/// guard (`[^{<]`) prevents the model from ambiguously starting a
/// tool envelope it can't finish.
fn build_tool_alternation_grammar(envelope_schema_json: &str) -> String {
    format!(
        "start: think_block? body\n\
         think_block: /<think>([^<]|<[^\\/])*<\\/think>\\s*/\n\
         body: tool_envelope | plain_text\n\
         tool_envelope: \"<tool_call>\" /\\s*/ %json {envelope_schema_json} /\\s*<\\/tool_call>/\n\
         plain_text: /[^{{<](.|\\n)*|<[^t](.|\\n)*|<t[^ho](.|\\n)*|<th[^i](.|\\n)*|<tho(.|\\n)*|<to[^o](.|\\n)*|<too[^l](.|\\n)*|<tool[^_](.|\\n)*|<tool_[^c](.|\\n)*/\n"
    )
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

    #[test]
    fn envelope_schema_is_none_for_empty_tools() {
        assert!(build_envelope_schema(&[]).is_none());
    }

    #[test]
    fn envelope_schema_skips_when_parameters_not_object() {
        let tools = vec![ToolSchema {
            name: "broken".into(),
            description: None,
            parameters: serde_json::json!("not an object"),
        }];
        assert!(build_envelope_schema(&tools).is_none());
    }

    #[test]
    fn envelope_schema_emits_one_of_per_tool() {
        let tools = vec![
            ToolSchema {
                name: "recipe_read".into(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
            ToolSchema {
                name: "recipe_validate".into(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
        ];
        let schema = build_envelope_schema(&tools).expect("schema built");
        let variants = schema["oneOf"].as_array().expect("oneOf array");
        assert_eq!(variants.len(), 2);
        // Each variant pins `name` to a single tool id.
        let names: Vec<&str> = variants
            .iter()
            .filter_map(|v| v["properties"]["name"]["enum"][0].as_str())
            .collect();
        assert_eq!(names, vec!["recipe_read", "recipe_validate"]);
    }

    #[test]
    fn inject_done_appends_done_variant_to_one_of() {
        let envelope = serde_json::json!({
            "oneOf": [
                {"type": "object", "properties": {"name": {"const": "recipe_read"}}}
            ]
        });
        let injected = inject_done_variant(envelope);
        let variants = injected["oneOf"].as_array().expect("oneOf array");
        assert_eq!(variants.len(), 2);
        let done = &variants[1];
        assert_eq!(done["properties"]["name"]["enum"][0], "done");
        assert!(done["properties"]["arguments"]["properties"]["reason"].is_object());
        assert_eq!(done["properties"]["arguments"]["required"][0], "reason");
    }

    #[test]
    fn inject_done_appends_to_any_of_when_present() {
        let envelope = serde_json::json!({
            "anyOf": [{"type": "object"}]
        });
        let injected = inject_done_variant(envelope);
        let variants = injected["anyOf"].as_array().expect("anyOf array");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1]["properties"]["name"]["enum"][0], "done");
    }

    #[test]
    fn inject_done_noop_when_neither_one_of_nor_any_of() {
        // Defensive: if the envelope shape doesn't carry an alternation,
        // we leave it alone rather than synthesising a new structure
        // that the upstream caller didn't ask for. The handler then
        // operates without a done-tool escape, falling back to the
        // plain_text branch + iteration cap.
        let envelope = serde_json::json!({"type": "object"});
        let injected = inject_done_variant(envelope.clone());
        assert_eq!(injected, envelope);
    }

    #[test]
    fn alternation_grammar_wraps_schema_with_tool_call_markers() {
        let schema = r#"{"type":"object"}"#;
        let grammar = build_tool_alternation_grammar(schema);
        // Top-level alternation.
        assert!(grammar.contains("body: tool_envelope | plain_text"));
        // Literal <tool_call> opener on the envelope branch.
        assert!(grammar.contains("\"<tool_call>\""));
        // Closing marker uses Lark regex escape `<\/tool_call>` (the
        // forward-slash escape keeps Lark's parser happy inside the
        // /…/ regex literal).
        assert!(grammar.contains(r"<\/tool_call>"));
        // %json embedded with the schema body.
        assert!(grammar.contains(r#"%json {"type":"object"}"#));
    }
}
