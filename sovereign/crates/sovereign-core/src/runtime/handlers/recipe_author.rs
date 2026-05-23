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
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<StreamHandle> {
        let system_prompt = self
            .skills
            .skill_by_id("recipe-author")
            .and_then(|s| s.prompts.synthesis.clone())
            .ok_or_else(|| {
                Error::NotImplemented(
                    "recipe-author skill not loaded or `[prompts] synthesis` missing. \
                     Check that the daemon can resolve modes/recipe-author/skill.toml \
                     (set SOVEREIGN_MODES_DIR or run from the workspace root)."
                        .into(),
                )
            })?;

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

        tracing::info!(
            conversation_id,
            tools = tool_schemas.len(),
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

        tokio::spawn(async move {
            let loop_start = std::time::Instant::now();
            let mut transcript = render_transcript(&prior_messages, &message_owned);
            let mut final_text = String::new();
            let mut tool_calls_total = 0usize;
            let mut last_model_id: Option<String> = None;
            let mut error_for_user: Option<String> = None;
            let mut completed_cleanly = false;

            for iter in 0..MAX_TOOL_ITERATIONS {
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
                    tool_choice: Some(serde_json::json!("auto")),
                    model_id: None,
                    enable_thinking: Some(false),
                    sampling_mode: Some(SamplingMode::Instruct),
                    assistant_prefix: None,
                    cmd_prefix: None,
                    url_allowlist: None,
                    evidence_id_allowlist: None,
                    lark_grammar: None,
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

                for call in &tool_calls {
                    let ctx = ToolContext {
                        conversation_id: conv_id_owned.clone(),
                        task_id: None,
                        working_directory: None,
                        in_reasoning_loop: true,
                        agent_session_token: None,
                        turn_index: iter,
                    };
                    let tool_call_start = std::time::Instant::now();
                    let result_str = match tools.get(&call.name) {
                        Ok(tool) => match tool.execute(&call.arguments, &ctx).await {
                            Ok(out) => format_step_output(&out),
                            Err(e) => serde_json::json!({
                                "error": e.to_string(),
                            })
                            .to_string(),
                        },
                        Err(_) => serde_json::json!({
                            "error": format!(
                                "unknown tool `{}` — pick one from the recipe-author catalog",
                                call.name
                            ),
                        })
                        .to_string(),
                    };

                    tool_calls_total += 1;
                    tracing::info!(
                        conversation_id = %conv_id_owned,
                        iteration = iter,
                        tool = %call.name,
                        result_chars = result_str.len(),
                        latency_ms = tool_call_start.elapsed().as_millis() as u64,
                        "recipe_author_loop: tool executed"
                    );

                    let args_json = serde_json::to_string(&call.arguments)
                        .unwrap_or_else(|_| "{}".to_string());
                    transcript.push_str(&format!(
                        "<tool_call>{{\"name\":\"{}\",\"arguments\":{args_json}}}</tool_call>\n\
                         <tool_result>{result_str}</tool_result>\n\n[Agent]:",
                        call.name,
                    ));
                }
            }

            let final_content = if let Some(err_text) = error_for_user {
                err_text
            } else if !completed_cleanly && final_text.trim().is_empty() {
                format!(
                    "I made {tool_calls_total} tool calls without reaching a final \
                     answer this turn (iteration cap = {MAX_TOOL_ITERATIONS}). Try \
                     splitting the request, or ask me to summarize what I tried."
                )
            } else if final_text.trim().is_empty() {
                "(The agent finished without emitting a partner-facing reply this turn.)"
                    .to_string()
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
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        use futures::StreamExt;

        let handle = self
            .handle_recipe_author_turn_stream(message, conversation_id, context, tool_descriptors)
            .await?;
        let StreamHandle { message_id, mut stream } = handle;

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
            None => break,
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
    fn parses_string_encoded_arguments() {
        let text = r#"<tool_call>{"name":"recipe_read","arguments":"{\"path\":\"foo\"}"}</tool_call>"#;
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
