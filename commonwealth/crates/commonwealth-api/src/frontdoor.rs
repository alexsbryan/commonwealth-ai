//! Frontdoor — harness-protocol → model-native language normalizer.
//!
//! Coding harnesses (codex, opencode, the OpenAI agents SDK, …) speak
//! a verbose dialect tuned for frontier models that can filter noise
//! from signal. A local 35B-A3B-class model can't do that filtering
//! cheaply; it needs the task pre-situated and the tool catalog
//! shrunken to its training prior. The frontdoor pass simulates a
//! "noise filter machine" so the executing model speaks calm,
//! directed language.
//!
//! Two halves:
//!
//! 1. **Catalog filter** (deterministic). Drop tools the executing
//!    model can't usefully dispatch (codex's agent-management,
//!    plugin, and harness-state tools), keep the universal coding
//!    primitives. Run every turn — codex sends the catalog every
//!    request.
//!
//! 2. **Fast-slot distiller** (one inference per unique-instructions
//!    session, cached). Take codex's verbose system prompt + initial
//!    user task and re-emit it as a minimal directive the executing
//!    model can metabolize in one read. Cache by SHA-256 of the
//!    original instructions so subsequent turns of the same session
//!    pay the distiller cost once.
//!
//! Gated behind `SOVEREIGN_FRONTDOOR=1` env. Default off until the
//! frontdoor surface is baselined against the bare-sandbox results
//! that motivated it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::openai_types::{ChatCompletionRequest, ChatCompletionResponse, ChatMessage};
use crate::responses_types::{
    MessageContent, MessageItem, ResponsesContentPart, ResponsesInput, ResponsesInputItem,
    ResponsesRequest,
};
use crate::routes_inference::chat_completions;
use crate::state::AppState;

/// Env var that enables the frontdoor on a daemon. Read per call so
/// flipping it at runtime is enough; no daemon restart needed.
pub fn is_enabled() -> bool {
    std::env::var("SOVEREIGN_FRONTDOOR")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Tool-name allowlist applied to codex's function tool catalog. Anything
/// not in this list is dropped before the model ever sees it.
///
/// Rationale per tool — codex 0.130 ships these by default:
///   - `exec_command` KEEP: only shell path codex registers; covers
///     cargo / git / curl / cat / printf — the executing model already
///     pattern-matches against shell idioms from training.
///   - `web_search` KEEP: occasional value; cheap.
///   - `write_stdin` DROP: only useful inside an interactive process;
///     no agent-driven file-write surface.
///   - `update_plan` DROP: harness bookkeeping — encourages the model
///     to emit plan-shaped non-tool text instead of doing the work.
///   - `request_user_input` DROP: hands-off automation context; the
///     model has nobody to ask. Encourages pause-loops.
///   - `view_image` DROP: model has no vision modality.
///   - `spawn_agent`, `send_input`, `resume_agent`, `wait_agent`,
///     `close_agent` DROP: codex's agent-management toolkit; a local
///     model recursing on agents would never converge.
///
/// Synthetic `write_file` and `read_file` are appended elsewhere
/// (`routes_responses::synthetic_file_tools`); they don't appear here
/// because they're added after the keeplist filter.
pub const CODEX_TOOL_KEEPLIST: &[&str] = &["exec_command", "web_search"];

/// Returns true when `name` is allowed through the catalog filter.
pub fn tool_keeplist_contains(name: &str) -> bool {
    CODEX_TOOL_KEEPLIST.contains(&name)
}

/// Apply all frontdoor passes to `req` in-place. Safe to call when
/// disabled — caller is expected to gate via `is_enabled()` first,
/// but every internal step short-circuits on missing inputs anyway.
pub async fn apply(state: &AppState, headers: &HeaderMap, req: &mut ResponsesRequest) {
    // Half 1: distiller (instructions rewriting). Cached by hash of
    // the original `instructions` + first user message.
    apply_distiller(state, headers, req).await;
    // Half 2: history compression. As codex's conversation history
    // accumulates (every previous tool_call + tool_result is
    // resent every turn), the model loses coherence. When the input
    // array exceeds the trigger length, fold the oldest items into a
    // single summary user-message and keep only the most recent N
    // items verbatim. Cached by hash of the compressed range.
    apply_history_compression(state, headers, req).await;
    // Half 3: catalog filter is applied during request translation,
    // not here — `routes_responses::translate_request` consults
    // `tool_keeplist_contains` directly. Centralising the policy in
    // one place keeps the translation-time path tight and lets tests
    // exercise the filter without spinning up an AppState.
}

/// Distilled directive produced by the fast-slot pass. Wire shape is
/// JSON object emitted by the distiller model.
#[derive(Debug, Clone, serde::Deserialize)]
struct DistilledDirective {
    #[serde(default)]
    task: String,
    #[serde(default)]
    constraints: String,
    #[serde(default)]
    done_when: String,
    #[serde(default)]
    files_to_touch: Vec<String>,
}

impl DistilledDirective {
    fn render(&self) -> String {
        let mut out = String::new();
        if !self.task.is_empty() {
            out.push_str("## Task\n\n");
            out.push_str(self.task.trim());
            out.push_str("\n\n");
        }
        if !self.constraints.is_empty() {
            out.push_str("## Constraints\n\n");
            out.push_str(self.constraints.trim());
            out.push_str("\n\n");
        }
        if !self.done_when.is_empty() {
            out.push_str("## Done when\n\n");
            out.push_str(self.done_when.trim());
            out.push_str("\n\n");
        }
        if !self.files_to_touch.is_empty() {
            out.push_str("## Files likely involved\n\n");
            for f in &self.files_to_touch {
                out.push_str(&format!("- `{}`\n", f));
            }
            out.push('\n');
        }
        out
    }
}

/// Cache key: SHA-256 of the original `instructions` + first user
/// message text. Multi-turn conversations of the same session pay the
/// distiller cost once.
static DISTILLER_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn distiller_cache() -> &'static Mutex<HashMap<String, String>> {
    DISTILLER_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const FRONTDOOR_DISTILLER_SYSTEM: &str = r#"You are a request normalizer. A coding harness (codex / opencode / similar) sends a verbose system prompt + initial user task that's tuned for a frontier model. Your job: distill it for a smaller local execution agent.

The execution agent's tool catalog is FIXED at:
- write_file(path, content) — create or replace a file
- read_file(path) — read a file
- exec_command(cmd) — run a shell command (cargo, git, cat, etc.)
- web_search(query) — search the web

Output exactly ONE JSON object with this shape, no prose around it, no markdown fences:

{
  "task": "<one-paragraph plain-prose description of the user's actual intent>",
  "constraints": "<one paragraph of load-bearing constraints worth highlighting, or empty>",
  "done_when": "<unambiguous completion criterion the agent can verify>",
  "files_to_touch": ["<absolute path the agent likely needs to create or edit>", ...]
}

Hard rules:
- DO NOT echo the original harness system prompt.
- DO NOT include tool catalog reminders — the agent already knows its tools.
- DO NOT add style advice or meta commentary.
- DO NOT mention codex, opencode, plugins, marketplaces, or anything about the harness.
- The output prose IS the complete context the agent will see — strip everything except the actual ask."#;

/// Run the distiller pass and overwrite `req.instructions` with the
/// distilled directive. Cached by SHA of the original instructions +
/// first user input.
async fn apply_distiller(state: &AppState, headers: &HeaderMap, req: &mut ResponsesRequest) {
    // Build the cache key from the original system + initial user
    // text. Multi-turn re-prompts of the same session land on the
    // same key.
    let original_blob = canonical_source_blob(req);
    if original_blob.is_empty() {
        debug!("frontdoor: nothing to distill (empty source)");
        return;
    }
    let key = sha256_hex(&original_blob);

    // Cache hit?
    if let Some(cached) = distiller_cache().lock().ok().and_then(|m| m.get(&key).cloned()) {
        debug!(
            cache_key = %&key[..12],
            "frontdoor: distiller cache hit"
        );
        req.instructions = Some(cached);
        return;
    }

    // Cache miss — call the primary slot.
    //
    // We deliberately reuse `primary` (not `fast`) for the
    // distillation. Rationale:
    //   - Qwen3.5-2B at the fast slot locked up emitting whitespace
    //     when given the structured-output task.
    //   - Primary is already loaded for the executing agent — no
    //     extra VRAM cost.
    //   - Capability differences vs a frontier model are absorbed by
    //     the cache: one inference per unique-instructions session.
    //   - `enable_thinking=false` suppresses the model's chain-of-
    //     thought so we don't burn tokens on a `<think>` block we'd
    //     strip anyway. `max_tokens=800` caps the directive size at
    //     something the executing agent can consume in one read.
    let started = std::time::Instant::now();
    let chat_req = ChatCompletionRequest {
        model: Some("primary".to_string()),
        messages: vec![
            ChatMessage::new("system", FRONTDOOR_DISTILLER_SYSTEM),
            ChatMessage::new("user", &original_blob),
        ],
        temperature: Some(0.0),
        max_tokens: Some(800),
        stream: Some(false),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools: None,
        tool_choice: None,
        // No json_object grammar — see distiller module doc.
        response_format: None,
        oicp: None,
        // Suppress chain-of-thought. The distillation task is bounded
        // enough that thinking doesn't help; it just spends tokens we
        // strip via `strip_think_block` downstream.
        chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
        think_budget: Some(0),
        tool_profile: None,
    };

    let response = chat_completions(State(state.clone()), headers.clone(), Json(chat_req)).await;
    let status = response.status();
    let body = match axum::body::to_bytes(response.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "frontdoor: distiller body read failed; passing through");
            return;
        }
    };
    if !status.is_success() {
        warn!(
            status = %status,
            body = %String::from_utf8_lossy(&body),
            "frontdoor: distiller inner-call failed; passing through"
        );
        return;
    }
    let chat: ChatCompletionResponse = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "frontdoor: distiller response JSON parse failed");
            return;
        }
    };
    let content = chat
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();

    let directive = match parse_directive(&content) {
        Some(d) => d,
        None => {
            warn!(raw = %content.chars().take(240).collect::<String>(),
                  "frontdoor: distiller output not parseable; passing through");
            return;
        }
    };

    let rendered = directive.render();
    if rendered.trim().is_empty() {
        warn!("frontdoor: distiller produced empty directive; passing through");
        return;
    }

    if let Ok(mut cache) = distiller_cache().lock() {
        cache.insert(key.clone(), rendered.clone());
    }

    info!(
        cache_key = %&key[..12],
        elapsed_ms = %started.elapsed().as_millis(),
        rendered_bytes = rendered.len(),
        "frontdoor: distilled and cached"
    );
    req.instructions = Some(rendered);
}

/// Reduce a request to its identity-defining text: the original
/// `instructions` plus the first user message text (or the bare
/// string input). Returns "" when both are absent.
fn canonical_source_blob(req: &ResponsesRequest) -> String {
    let mut out = String::new();
    if let Some(instr) = req.instructions.as_deref() {
        out.push_str("# Harness instructions\n\n");
        out.push_str(instr.trim());
        out.push_str("\n\n");
    }
    match &req.input {
        ResponsesInput::Text(s) => {
            if !s.trim().is_empty() {
                out.push_str("# User task\n\n");
                out.push_str(s.trim());
                out.push('\n');
            }
        }
        ResponsesInput::Items(items) => {
            // Find the FIRST user message and emit its text. Later
            // items are conversation history — not part of the task
            // identity.
            for item in items {
                if let ResponsesInputItem::Message(m) = item {
                    if m.role != "user" {
                        continue;
                    }
                    let text = match &m.content {
                        MessageContent::Text(s) => s.clone(),
                        MessageContent::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| match p {
                                ResponsesContentPart::InputText { text } => Some(text.clone()),
                                ResponsesContentPart::OutputText { text } => Some(text.clone()),
                                ResponsesContentPart::Other => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    if !text.trim().is_empty() {
                        out.push_str("# User task\n\n");
                        out.push_str(text.trim());
                        out.push('\n');
                        break;
                    }
                }
            }
        }
    }
    out
}

// ─── History compression ────────────────────────────────────────────

/// Compress the older portion of the conversation history into a
/// single summary user-message. Triggers when `input.items.len()`
/// exceeds `HISTORY_COMPRESS_TRIGGER`; keeps the last
/// `HISTORY_KEEP_RECENT` items verbatim and folds the rest into one
/// `# Conversation so far\n<summary>` message at the front.
///
/// Why: codex resends the entire conversation every turn. By turn 10
/// the request includes the original task + 9 prior assistant
/// tool_calls + 9 tool_results. The model loses coherence on
/// long contexts even when the catalog is filtered and the system
/// prompt is distilled — observed 2026-05-12 v4 smoke as path
/// corruption and concatenated tool_calls in late-turn emits.
const HISTORY_COMPRESS_TRIGGER: usize = 8;
const HISTORY_KEEP_RECENT: usize = 4;

static HISTORY_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn history_cache() -> &'static Mutex<HashMap<String, String>> {
    HISTORY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const FRONTDOOR_HISTORY_SYSTEM: &str = r#"You compress a conversation history for a small execution agent. You are given the older turns of an agent's session: the user's task, the assistant's tool calls (with arguments), and the tool results. Your job: emit a single paragraph that captures everything the agent needs to know to continue the work.

What MUST appear in your output:
- What the user asked for, in one sentence
- Which files have been created or modified, with their current state (one bullet per file)
- Which shell commands ran, with outcomes (success / failed-with-error)
- What the agent learned (lib path, dep name, error patterns)
- The single open sub-task the agent should attack next

What MUST NOT appear:
- Verbatim file contents (cite the file name + one-line description instead)
- Verbose error messages (extract just the actionable error class)
- Repeated framing
- Tool catalog reminders
- Apologies, hedging, meta-commentary

Output: plain prose. No markdown headings (the agent already has framing). No JSON. Concise — aim for under 300 words."#;

async fn apply_history_compression(
    state: &AppState,
    headers: &HeaderMap,
    req: &mut ResponsesRequest,
) {
    let items = match &mut req.input {
        ResponsesInput::Items(v) => v,
        ResponsesInput::Text(_) => return,
    };
    if items.len() <= HISTORY_COMPRESS_TRIGGER {
        return;
    }
    let split_at = items.len() - HISTORY_KEEP_RECENT;
    if split_at == 0 {
        return;
    }
    let to_compress: Vec<ResponsesInputItem> = items.drain(..split_at).collect();

    let history_blob = render_items_for_distill(&to_compress);
    if history_blob.trim().is_empty() {
        // Restore — nothing useful to compress.
        for (i, item) in to_compress.into_iter().enumerate() {
            items.insert(i, item);
        }
        return;
    }
    let key = sha256_hex(&history_blob);

    let summary = if let Some(cached) =
        history_cache().lock().ok().and_then(|m| m.get(&key).cloned())
    {
        debug!(cache_key = %&key[..12], "frontdoor: history cache hit");
        cached
    } else {
        let started = std::time::Instant::now();
        let chat_req = ChatCompletionRequest {
            model: Some("primary".to_string()),
            messages: vec![
                ChatMessage::new("system", FRONTDOOR_HISTORY_SYSTEM),
                ChatMessage::new("user", &history_blob),
            ],
            temperature: Some(0.0),
            max_tokens: Some(1200),
            stream: Some(false),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            oicp: None,
            chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
            think_budget: Some(0),
            tool_profile: None,
        };
        let response =
            chat_completions(State(state.clone()), headers.clone(), Json(chat_req)).await;
        let status = response.status();
        let body = match axum::body::to_bytes(response.into_body(), usize::MAX).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "frontdoor: history compressor body read failed; restoring");
                for (i, item) in to_compress.into_iter().enumerate() {
                    items.insert(i, item);
                }
                return;
            }
        };
        if !status.is_success() {
            warn!(
                status = %status,
                "frontdoor: history compressor inner-call failed; restoring"
            );
            for (i, item) in to_compress.into_iter().enumerate() {
                items.insert(i, item);
            }
            return;
        }
        let chat: ChatCompletionResponse = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "frontdoor: history compressor response not JSON; restoring");
                for (i, item) in to_compress.into_iter().enumerate() {
                    items.insert(i, item);
                }
                return;
            }
        };
        let raw = chat
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();
        let summary_text = strip_think_block(&raw).trim().to_string();
        if summary_text.is_empty() {
            warn!("frontdoor: history compressor produced empty summary; restoring");
            for (i, item) in to_compress.into_iter().enumerate() {
                items.insert(i, item);
            }
            return;
        }
        if let Ok(mut cache) = history_cache().lock() {
            cache.insert(key.clone(), summary_text.clone());
        }
        info!(
            cache_key = %&key[..12],
            elapsed_ms = %started.elapsed().as_millis(),
            compressed_turns = to_compress.len(),
            summary_bytes = summary_text.len(),
            "frontdoor: history compressed and cached"
        );
        summary_text
    };

    // Prepend a synthetic user message with the summary.
    let summary_item = ResponsesInputItem::Message(MessageItem {
        role: "user".to_string(),
        content: MessageContent::Text(format!(
            "# Conversation so far (compressed by frontdoor)\n\n{}\n\nContinue the work from here.",
            summary
        )),
    });
    items.insert(0, summary_item);
}

/// Render an item list as plain prose for the history compressor's
/// input. Walks message / function_call / function_call_output items
/// and emits per-turn blocks.
fn render_items_for_distill(items: &[ResponsesInputItem]) -> String {
    use crate::responses_types::{FunctionCallItem, FunctionCallOutputItem};
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        out.push_str(&format!("## Turn {}\n\n", i + 1));
        match item {
            ResponsesInputItem::Message(m) => {
                out.push_str(&format!("[{}]\n", m.role));
                let text = match &m.content {
                    MessageContent::Text(s) => s.clone(),
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .filter_map(|p| match p {
                            ResponsesContentPart::InputText { text } => Some(text.clone()),
                            ResponsesContentPart::OutputText { text } => Some(text.clone()),
                            ResponsesContentPart::Other => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                out.push_str(text.trim());
                out.push_str("\n\n");
            }
            ResponsesInputItem::FunctionCall(c) => {
                let c: &FunctionCallItem = c;
                out.push_str(&format!(
                    "[assistant tool_call] name={} call_id={}\nargs={}\n\n",
                    c.name,
                    c.call_id,
                    truncate_for_history(&c.arguments, 600)
                ));
            }
            ResponsesInputItem::FunctionCallOutput(o) => {
                let o: &FunctionCallOutputItem = o;
                out.push_str(&format!(
                    "[tool result] call_id={}\n{}\n\n",
                    o.call_id,
                    truncate_for_history(&o.output, 600)
                ));
            }
        }
    }
    out
}

fn truncate_for_history(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated {} chars]", &s[..end], s.len() - end)
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

/// Extract the first JSON object from the distiller's response.
/// Tolerates leading `<think>` blocks and stray prose; finds the
/// first `{` and walks balanced.
fn parse_directive(raw: &str) -> Option<DistilledDirective> {
    let stripped = strip_think_block(raw);
    let start = stripped.find('{')?;
    let bytes = stripped.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = None;
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
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    serde_json::from_str::<DistilledDirective>(&stripped[start..end]).ok()
}

fn strip_think_block(s: &str) -> String {
    // Crude but sufficient: drop any `<think>...</think>` chunks.
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find("<think>") {
        out.push_str(&rest[..open]);
        rest = &rest[open + "<think>".len()..];
        match rest.find("</think>") {
            Some(close) => rest = &rest[close + "</think>".len()..],
            None => return out, // never closed; discard everything after the open
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeplist_keeps_exec_command_and_web_search() {
        assert!(tool_keeplist_contains("exec_command"));
        assert!(tool_keeplist_contains("web_search"));
    }

    #[test]
    fn keeplist_drops_agent_management_tools() {
        for n in [
            "spawn_agent",
            "resume_agent",
            "wait_agent",
            "close_agent",
            "send_input",
            "view_image",
            "update_plan",
            "request_user_input",
            "write_stdin",
        ] {
            assert!(!tool_keeplist_contains(n), "expected {n} to be dropped");
        }
    }

    // Single combined test: env-var reads are global state and the
    // tests would race in parallel.
    #[test]
    fn is_enabled_env_var_semantics() {
        // Snapshot prior value so we don't leak into other test cases
        // that might query the same env var.
        let prior = std::env::var("SOVEREIGN_FRONTDOOR").ok();

        std::env::remove_var("SOVEREIGN_FRONTDOOR");
        assert!(!is_enabled(), "should default off when unset");

        std::env::set_var("SOVEREIGN_FRONTDOOR", "0");
        assert!(!is_enabled(), "0 should be falsy");

        std::env::set_var("SOVEREIGN_FRONTDOOR", "1");
        assert!(is_enabled(), "1 should be truthy");

        std::env::set_var("SOVEREIGN_FRONTDOOR", "TRUE");
        assert!(is_enabled(), "TRUE should be truthy");

        // Restore.
        match prior {
            Some(v) => std::env::set_var("SOVEREIGN_FRONTDOOR", v),
            None => std::env::remove_var("SOVEREIGN_FRONTDOOR"),
        }
    }

    #[test]
    fn parse_directive_round_trips_well_formed_object() {
        let raw = r#"{
            "task": "implement Capability enum",
            "constraints": "use serde",
            "done_when": "cargo test passes",
            "files_to_touch": ["/abs/lib.rs", "/abs/tests/cap.rs"]
        }"#;
        let d = parse_directive(raw).unwrap();
        assert_eq!(d.task, "implement Capability enum");
        assert_eq!(d.files_to_touch.len(), 2);
    }

    #[test]
    fn parse_directive_tolerates_leading_think_block() {
        let raw = "<think>let me think...</think>\n{\"task\":\"x\",\"constraints\":\"\",\"done_when\":\"y\",\"files_to_touch\":[]}";
        let d = parse_directive(raw).unwrap();
        assert_eq!(d.task, "x");
    }

    #[test]
    fn parse_directive_tolerates_prose_after_json() {
        let raw = r#"{"task":"x","constraints":"","done_when":"y","files_to_touch":[]}

That's my answer."#;
        let d = parse_directive(raw).unwrap();
        assert_eq!(d.task, "x");
    }

    #[test]
    fn parse_directive_returns_none_on_garbage() {
        assert!(parse_directive("not even close").is_none());
        assert!(parse_directive("{not json").is_none());
    }

    #[test]
    fn render_omits_empty_sections() {
        let d = DistilledDirective {
            task: "x".into(),
            constraints: "".into(),
            done_when: "y".into(),
            files_to_touch: vec![],
        };
        let r = d.render();
        assert!(r.contains("## Task"));
        assert!(r.contains("## Done when"));
        assert!(!r.contains("## Constraints"));
        assert!(!r.contains("## Files"));
    }

    #[test]
    fn canonical_source_blob_strips_history_keeps_first_user_message() {
        use crate::responses_types::*;
        let req = ResponsesRequest {
            model: None,
            input: ResponsesInput::Items(vec![
                ResponsesInputItem::Message(MessageItem {
                    role: "user".into(),
                    content: MessageContent::Text("real task".into()),
                }),
                ResponsesInputItem::Message(MessageItem {
                    role: "assistant".into(),
                    content: MessageContent::Text("ack".into()),
                }),
                ResponsesInputItem::Message(MessageItem {
                    role: "user".into(),
                    content: MessageContent::Text("follow-up question".into()),
                }),
            ]),
            instructions: Some("be terse".into()),
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            previous_response_id: None,
            store: None,
            parallel_tool_calls: None,
            reasoning: None,
            metadata: None,
        };
        let blob = canonical_source_blob(&req);
        assert!(blob.contains("be terse"));
        assert!(blob.contains("real task"));
        // History (assistant + later user messages) is NOT in the
        // blob — distiller cache key is identity of the original ask.
        assert!(!blob.contains("ack"));
        assert!(!blob.contains("follow-up"));
    }

    #[test]
    fn render_items_for_distill_walks_messages_and_tool_calls() {
        use crate::responses_types::{FunctionCallItem, FunctionCallOutputItem, MessageContent};
        let items = vec![
            ResponsesInputItem::Message(MessageItem {
                role: "user".into(),
                content: MessageContent::Text("write a file".into()),
            }),
            ResponsesInputItem::FunctionCall(FunctionCallItem {
                call_id: "call_1".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"/x","content":"hi"}"#.into(),
                id: None,
            }),
            ResponsesInputItem::FunctionCallOutput(FunctionCallOutputItem {
                call_id: "call_1".into(),
                output: "ok".into(),
            }),
        ];
        let rendered = render_items_for_distill(&items);
        assert!(rendered.contains("Turn 1"));
        assert!(rendered.contains("write a file"));
        assert!(rendered.contains("tool_call] name=write_file"));
        assert!(rendered.contains("tool result] call_id=call_1"));
    }

    #[test]
    fn truncate_for_history_appends_count_marker() {
        let big = "x".repeat(800);
        let t = truncate_for_history(&big, 200);
        assert!(t.starts_with(&"x".repeat(200)));
        assert!(t.contains("[truncated 600 chars]"));
    }

    #[test]
    fn truncate_for_history_short_string_unchanged() {
        assert_eq!(truncate_for_history("short", 100), "short");
    }

    #[test]
    fn same_request_produces_same_cache_key() {
        use crate::responses_types::*;
        let mk = || ResponsesRequest {
            model: None,
            input: ResponsesInput::Text("hello".into()),
            instructions: Some("be brief".into()),
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            previous_response_id: None,
            store: None,
            parallel_tool_calls: None,
            reasoning: None,
            metadata: None,
        };
        let a = sha256_hex(&canonical_source_blob(&mk()));
        let b = sha256_hex(&canonical_source_blob(&mk()));
        assert_eq!(a, b);
    }
}
