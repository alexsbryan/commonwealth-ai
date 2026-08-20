// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn recipe-agent live-trial` — scripted, daemon-driven trial.
//!
//! Drives the recipe-author agent loop end-to-end against the running
//! svrn daemon's `/v1/chat/completions`. Reads partner messages
//! from a script file, runs an OpenAI-style tool-call loop with the
//! recipe-author skill's system prompt and tool definitions, then
//! validates the generated recipe + runs an initial fetch.
//!
//! ## Production integration
//!
//! The harness deliberately reuses every production-side primitive
//! that the M2 desktop workspace will eventually drive:
//!
//! - **Daemon endpoint** — `POST /v1/chat/completions` is the same
//!   surface the desktop chat will call.
//! - **Skill manifest** — reads `recipe-author/skill.toml` directly
//!   (parses `[prompts] synthesis = "..."`); no copy of the prompt.
//! - **Tool implementations** — the same `RecipeReadTool /
//!   RecipeWriteTool / RecipeValidateTool / RecipeTestTool /
//!   RegistryBrowseTool / DecisionLogTool / CheckpointTool /
//!   CapabilityRequestTool / WebFetchTool / WebSearchTool` registered
//!   in `sovereign-cli/src/main.rs`.
//! - **Persistence** — `NoteStore` + `RecipeProjectStore` at the user's
//!   real `~/.svrnmesh/{notes,features}.db`. Capability requests
//!   land in the user's real maintainer inbox.
//! - **Project model** — `RecipeProject` provisioned via
//!   `provision_recipe_project`; sidecar dir at
//!   `~/.svrnmesh/recipe-projects/<feature_id>/`.
//! - **Situated-context renderer** — same
//!   `recipe_author::situated_context::render` the M2 desktop will
//!   call.
//!
//! The one place the harness diverges from the production runtime:
//! it does NOT go through `sovereign_core::runtime::Runtime`.
//! Runtime classifies every turn into an `Intent` (SimpleQuery /
//! DeepQuery / KnowledgeQuery / MetalingualQuery / …) and routes to
//! a synthesis path tuned for chat-style retrieval. The recipe-
//! author agent is a different shape — a long-lived tool-using loop
//! — and Runtime's classifier doesn't pass tool schemas through to
//! that synthesis path, so the model never sees the tools it should
//! be calling. Going direct to the daemon's OpenAI-compatible
//! surface gives the model the tool list explicitly. M2's desktop
//! workspace will face the same routing question; the resolution
//! (skill-aware bypass in Runtime, or a parallel agent-loop
//! entrypoint) lands there. This harness exercises the right shape
//! today so the M2 work has a working ground-truth comparison.
//!
//! ## Intended uses
//!
//! - Regression test: pin a partner-message script and assert the
//!   resulting recipe passes validation + extracts ≥ 1 doc.
//! - Prompt iteration: tweak the recipe-author skill manifest and
//!   re-run the same script.
//! - Smoke test before a real partner session: verify the daemon +
//!   tools + skill + situated-context renderer all wire together.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use corpus_engine_notes::NoteStore;
use sovereign_contracts::recipe::notes::{NoteScope, RecipeNotes, ScopeFilter};
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::{ConversationId, StepOutput, ToolContext};
use sovereign_core::ToolRegistry;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_store::recipe_project_store::RecipeProjectStore;
use sovereign_tools::recipe_author::{
    situated_context, CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool,
    RecipeProject, RecipeReadTool, RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool,
    RecipeWriteTool, RegistryBrowseTool, ResearchFindingTool,
};
use sovereign_tools::recipe_notes_adapter::NoteStoreRecipeNotes;
use sovereign_tools::recipe_tester_adapter::CorpusEngineRecipeTester;

// ─── OpenAI-style wire types ────────────────────────────────────
//
// Defined locally rather than pulled in via `commonwealth-api` so
// `sovereign-cli` doesn't acquire a heavy commonwealth dependency.
// Field set tracks what the daemon's `/v1/chat/completions` route
// requires + emits today; missing fields land via `serde(default)`
// so this stays forward-compat with daemon-side additions.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolDefinition {
    #[serde(rename = "type")]
    kind: String,
    function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolFunction {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    model: Option<String>,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    /// Daemon-supplied stop reason. Read for diagnostics — kept on
    /// the type so the deserializer doesn't reject an upstream that
    /// promotes it from optional to required.
    #[serde(default)]
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

// ─── Args ────────────────────────────────────────────────────────

#[derive(Debug)]
struct Args {
    charter_path: PathBuf,
    script_path: PathBuf,
    feature_id: Option<String>,
    title: Option<String>,
    daemon_base: String,
    skills_dir: PathBuf,
    sample_size: u64,
    chat_model: Option<String>,
    /// Install-time parameter values forwarded to the post-trial
    /// `recipe_test` call. Repeatable: `--param api_token=<TOKEN>
    /// --param court=ca9`. Lets the partner inject auth secrets and
    /// other runtime values without baking them into the recipe.
    params: Vec<(String, String)>,
    /// Maximum tool-call iterations per partner turn before bailing.
    /// 12 leaves plenty of headroom for browse → read → write → validate
    /// → write → validate → test → write → validate → test → checkpoint.
    max_tool_iters: usize,
    /// Drop assistant `<think>...</think>` blocks before logging /
    /// re-sending. Reasoning models like Qwen3 emit these and they
    /// crowd the trial log; the daemon strips them on the next turn
    /// anyway, but stripping client-side keeps the printed transcript
    /// missing values fall back to skipping the network-side fetch
    /// entirely. Useful when running the trial against rate-limited
    /// upstream APIs (CourtListener etc.) for plumbing-only smoke
    /// tests where the agent loop + validation matter but a real
    /// pull does not.
    strip_think: bool,
    /// Skip the post-trial fetch (the `RecipeTestTool` call that
    /// happens AFTER all script turns drain). When `true`, the
    /// harness reports the in-script outcomes and exits without
    /// hitting the recipe's upstream API.
    no_fetch: bool,
    /// Drive the REAL recipe-author Runtime loop over the daemon's
    /// conversation API (`POST /v1/conversations {"skill_id":
    /// "recipe-author"}` + `/v1/conversations/:id/messages`) instead of
    /// the client-side raw-completions loop. The daemon's Runtime owns
    /// the tool loop + grammar (the same `handle_recipe_author_turn`
    /// the desktop uses); this harness only feeds partner turns and
    /// reads the project state back. Requires a daemon built with
    /// recipe-author conversation support (skill_id + registered tools).
    via_runtime: bool,
}

fn parse_args(argv: &[String]) -> std::result::Result<Args, String> {
    let mut charter: Option<PathBuf> = None;
    let mut script: Option<PathBuf> = None;
    let mut feature_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut daemon_base = "http://localhost:9741".to_string();
    let mut skills_dir: Option<PathBuf> = None;
    let mut sample_size: u64 = 50;
    let mut chat_model: Option<String> = None;
    let mut max_tool_iters: usize = 20;
    let mut strip_think = true;
    let mut no_fetch = false;
    let mut via_runtime = false;
    let mut params: Vec<(String, String)> = Vec::new();

    let mut iter = argv.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--charter" => charter = iter.next().map(PathBuf::from),
            "--script" => script = iter.next().map(PathBuf::from),
            "--feature-id" => feature_id = iter.next().cloned(),
            "--title" => title = iter.next().cloned(),
            "--daemon" => {
                if let Some(v) = iter.next() {
                    daemon_base = v.clone();
                }
            }
            "--skills-dir" => skills_dir = iter.next().map(PathBuf::from),
            "--sample-size" => {
                sample_size = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--sample-size requires an integer".to_string())?;
            }
            "--chat-model" => chat_model = iter.next().cloned(),
            "--max-tool-iters" => {
                max_tool_iters = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--max-tool-iters requires an integer".to_string())?;
            }
            "--keep-think" => strip_think = false,
            "--no-fetch" => no_fetch = true,
            "--via-runtime" => via_runtime = true,
            "--param" => {
                let kv = iter
                    .next()
                    .ok_or_else(|| "--param requires KEY=VALUE".to_string())?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| format!("--param expects KEY=VALUE, got `{kv}`"))?;
                params.push((k.to_string(), v.to_string()));
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let charter_path = charter.ok_or("missing --charter <FILE>")?;
    let script_path = script.ok_or("missing --script <FILE>")?;
    let skills_dir = skills_dir.unwrap_or_else(|| {
        // Prefer the new `modes/` directory; fall back to legacy
        // `skills/` so checkouts mid-migration still resolve.
        let cwd = std::env::current_dir().unwrap_or_default();
        for sub in ["modes", "skills"] {
            let candidate = cwd.join("sovereign").join(sub);
            if candidate.exists() {
                return candidate;
            }
            let candidate2 = cwd.join(sub);
            if candidate2.exists() {
                return candidate2;
            }
        }
        // User-overlay path (~/.svrnmesh/skills) keeps its name
        // for back-compat with existing custom skills on disk.
        sovereign_contracts::rebrand::svrnmesh_root().join("skills")
    });
    Ok(Args {
        charter_path,
        script_path,
        feature_id,
        title,
        daemon_base,
        skills_dir,
        sample_size,
        chat_model,
        max_tool_iters,
        strip_think,
        no_fetch,
        via_runtime,
        params,
    })
}

fn print_help() {
    eprintln!(
        "Usage:\n  svrn recipe-agent live-trial \\\n    \
         --charter <FILE> --script <FILE> \\\n    \
         [--feature-id <ID>] [--title <T>] \\\n    \
         [--daemon <URL>] [--skills-dir <D>] \\\n    \
         [--sample-size <N>] [--chat-model <ID>] \\\n    \
         [--max-tool-iters <N>] [--keep-think] [--via-runtime] \\\n    \
         [--param KEY=VALUE ...]\n\n\
         --via-runtime drives the real recipe-author Runtime loop over the\n  \
         daemon's conversation API (the desktop-equivalent path) instead of\n  \
         the client-side completions loop.\n\n\
         Drive the recipe-author agent loop end-to-end against the \
         daemon's\n  /v1/chat/completions, then validate the generated \
         recipe and run\n  an initial fetch with --sample-size docs (default 50).\n\n\
         Script file format: one partner message per blank-line-separated \
         block.\n  Lines starting with # are comments and skipped.\n\n\
         --param: install-time parameter values forwarded to the\n  \
         post-trial recipe_test call. Use this for auth tokens and\n  \
         other secrets. The recipe should declare them in its\n  \
         [parameters] section and reference them in [acquire].headers\n  \
         via `{{name}}` placeholders. Example:\n  \
         --param api_token=<COURTLISTENER_TOKEN>\n"
    );
}

// ─── Script file ─────────────────────────────────────────────────

fn parse_script(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.trim().is_empty() {
            if !cur.trim().is_empty() {
                blocks.push(cur.trim().to_string());
                cur.clear();
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    if !cur.trim().is_empty() {
        blocks.push(cur.trim().to_string());
    }
    blocks
}

// ─── Daemon probe + model resolve ────────────────────────────────

async fn probe_daemon(base: &str) -> std::result::Result<(), String> {
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("http client build: {e}"))?;
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!(
            "daemon at {base} returned {} from /v1/models. \
             Is it really a svrn daemon?",
            r.status()
        )),
        Err(_) => Err(format!(
            "daemon unreachable at {base}. \
             Start it with `svrn daemon run`, or pass --daemon <URL>."
        )),
    }
}

async fn resolve_chat_model(
    base: &str,
    explicit: Option<&str>,
) -> std::result::Result<String, String> {
    if let Some(c) = explicit {
        return Ok(c.to_string());
    }
    if let Ok(cfg) = sovereign_core::setup_config::SetupConfig::load() {
        if let Some(stem) = cfg
            .models
            .primary
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from)
        {
            return Ok(stem);
        }
    }
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /v1/models: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse /v1/models: {e}"))?;
    let data = resp
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "/v1/models has no .data array".to_string())?;
    data.iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
        .find(|id| !id.to_lowercase().contains("embed"))
        .map(String::from)
        .ok_or_else(|| "no chat model on /v1/models".to_string())
}

// ─── Skill manifest loader (system prompt) ──────────────────────

fn load_recipe_author_system_prompt(
    skills_dir: &std::path::Path,
) -> std::result::Result<String, String> {
    let path = skills_dir.join("recipe-author").join("skill.toml");
    if !path.exists() {
        return Err(format!(
            "recipe-author skill not found at {}",
            path.display()
        ));
    }
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let toml: toml::Value =
        toml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let prompt = toml
        .get("prompts")
        .and_then(|p| p.get("synthesis"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| {
            format!(
                "{} is missing [prompts] synthesis = \"...\"",
                path.display()
            )
        })?;
    Ok(prompt.trim().to_string())
}

// ─── Tool descriptors → OpenAI tool defs ─────────────────────────

fn registry_to_tool_defs(registry: &ToolRegistry) -> Vec<ToolDefinition> {
    registry
        .descriptors()
        .into_iter()
        .map(|d| ToolDefinition {
            kind: "function".to_string(),
            function: ToolFunction {
                name: d.id,
                description: Some(d.description),
                parameters: d.parameters,
            },
        })
        .collect()
}

// ─── Tool execution ─────────────────────────────────────────────

async fn execute_tool_call(
    registry: &ToolRegistry,
    call: &ToolCall,
    ctx: &ToolContext,
    project: &RecipeProject,
) -> ChatMessage {
    let args: serde_json::Value = match serde_json::from_str(&call.function.arguments) {
        Ok(v) => v,
        Err(e) => {
            return ChatMessage {
                role: "tool".into(),
                content: serde_json::json!({
                    "error": format!("malformed tool arguments JSON: {e}"),
                    "raw": call.function.arguments,
                })
                .to_string(),
                tool_call_id: Some(call.id.clone()),
                tool_calls: None,
            };
        }
    };
    let tool = match registry.get(&call.function.name) {
        Ok(t) => t,
        Err(_) => {
            return ChatMessage {
                role: "tool".into(),
                content: serde_json::json!({
                    "error": format!(
                        "unknown tool `{}`. The trial harness only \
                         registers the recipe-author + web-research \
                         tool set; pick one of those.",
                        call.function.name
                    )
                })
                .to_string(),
                tool_call_id: Some(call.id.clone()),
                tool_calls: None,
            };
        }
    };
    let outcome = tool.execute(&args, ctx).await;
    let content = match &outcome {
        Ok(StepOutput::Json(v)) => v.to_string(),
        Ok(StepOutput::Text(t)) => serde_json::json!({ "text": t }).to_string(),
        Ok(other) => serde_json::json!({
            "non_json_output": format!("{other:?}")
        })
        .to_string(),
        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
    };
    // Surface a peek of every tool result alongside the existing
    // "→ tool: foo({…})" line so it's possible to tell from the
    // transcript whether the agent's narration matches what the
    // tool actually returned. Cap at 400 chars to keep the log
    // legible — tool results that matter to debugging (probe_url,
    // recipe_test) fit comfortably under that.
    let preview: String = content.chars().take(400).collect();
    eprintln!(
        "    ← result: {}{}",
        preview.replace('\n', " "),
        if content.len() > 400 { "…" } else { "" }
    );

    // Side effect: when a recipe-writing tool succeeds, derive the
    // recipe_id from the path argument and stamp it onto the project
    // summary. The agent never has to remember to call a separate
    // "register recipe" tool; the act of writing the TOML *is* the
    // registration. This keeps `RecipeProject::read_summary().recipe_id`
    // consistent with the on-disk recipe path the agent just produced.
    //
    // Both `recipe_write` (raw TOML) and `recipe_write_structured`
    // (JSON-then-converted) are recipe-writing tools and both should
    // trigger the summary update. `recipe_write_structured` also
    // returns a non-error JSON when the validator finds problems
    // (the file is on disk but malformed) — we still record the id
    // because the agent's NEXT recipe_write call may overwrite a
    // valid version.
    let is_recipe_write = matches!(
        call.function.name.as_str(),
        "recipe_write" | "recipe_write_structured"
    );
    if is_recipe_write {
        if let Ok(StepOutput::Json(_)) = &outcome {
            if let Some(rid) = derive_recipe_id_from_args(&args) {
                if let Ok(mut summary) = project.read_summary() {
                    if summary.recipe_id.as_deref() != Some(rid.as_str()) {
                        summary.recipe_id = Some(rid);
                        summary.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        let _ = project.write_summary(&summary);
                    }
                }
            }
        }
    }

    ChatMessage {
        role: "tool".into(),
        content,
        tool_call_id: Some(call.id.clone()),
        tool_calls: None,
    }
}

/// Derive a recipe id from a `recipe_write`'s `path` argument. The
/// agent passes either a bare id (`"foo"`) or a relative path
/// (`"foo/recipe.toml"` or `"foo"`). We strip any `recipe.toml`
/// suffix and any leading `~/.svrnmesh/recipes/` prefix to land
/// on the canonical id. Returns `None` for malformed paths so the
/// summary update fails closed rather than corrupting state.
fn derive_recipe_id_from_args(args: &serde_json::Value) -> Option<String> {
    let raw = args.get("path").and_then(|v| v.as_str())?;
    let trimmed = raw.trim_end_matches('/');
    // Strip trailing `/recipe.toml` if present.
    let without_file = trimmed
        .strip_suffix("/recipe.toml")
        .unwrap_or(trimmed)
        .strip_suffix(".toml")
        .map(|s| s.trim_end_matches('/'))
        .unwrap_or(trimmed.strip_suffix("/recipe.toml").unwrap_or(trimmed));
    // Strip any path prefix the agent might have included.
    let id = without_file
        .rsplit('/')
        .find(|seg| !seg.is_empty())?
        .to_string();
    if id.is_empty() || id.contains("..") {
        return None;
    }
    Some(id)
}

fn strip_think_block(content: &str) -> String {
    let lower = content;
    if let Some(start) = lower.find("<think>") {
        if let Some(end_rel) = lower[start..].find("</think>") {
            let end = start + end_rel + "</think>".len();
            let mut s = String::with_capacity(content.len());
            s.push_str(&content[..start]);
            s.push_str(content[end..].trim_start());
            return s.trim_start().to_string();
        }
    }
    content.to_string()
}

/// Stable signature for a tool call used by the per-turn loop
/// detector. We collapse whitespace + lowercase the arguments JSON
/// so cosmetically-different repeats (different whitespace, case)
/// still bucket together. This is *not* a content hash — semantic
/// duplicates (same intent, slightly different wording) won't
/// match. The intent is to catch the most common loop mode: the
/// model retrying the literal same tool call after getting nothing
/// useful from the prior result.
fn call_signature(call: &ToolCall) -> String {
    let args_compact: String = call
        .function
        .arguments
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    format!("{}::{}", call.function.name, args_compact.to_lowercase())
}

/// Build a tool-result message that breaks the model out of a
/// per-tool repetition loop. Two trip wires:
/// - `kind = "exact_args"` — same call hash repeated > threshold.
/// - `kind = "same_tool"` — same tool name repeated regardless of
///   args. Catches the "barely-permuted query" failure mode where
///   the agent burns 7 web_searches changing one word each time.
fn synthesize_loop_break_message(call: &ToolCall, count: usize, kind: &'static str) -> ChatMessage {
    let hint = if call.function.name == "web_search" {
        "You've called web_search many times this turn and it isn't \
         converging — DDG is rate-limiting or the docs aren't where \
         you're looking. Switch tactics NOW: \
         (1) ASK THE PARTNER. If you don't know the canonical \
         endpoint or the correct field names, the partner can \
         answer in one sentence — surfacing the question is better \
         than another search. \
         (2) probe_url with a corrected guess (e.g. swap the \
         rejected field names from a 4xx body for the API's \
         convention) and READ the body. \
         (3) draft with reasonable defaults via \
         recipe_write_structured and let recipe_test surface the \
         real errors. \
         Do NOT call web_search again."
            .to_string()
    } else {
        format!(
            "You've called this tool {count} times this turn ({kind}). \
             Switch tactics: ASK THE PARTNER (a clean question beats \
             more retries), try a different tool, or proceed with the \
             info you have. Repeated calls won't converge."
        )
    };
    let body = serde_json::json!({
        "loop_detected": true,
        "tool": call.function.name,
        "repeat_count": count,
        "kind": kind,
        "hint": hint,
    });
    ChatMessage {
        role: "tool".into(),
        content: body.to_string(),
        tool_call_id: Some(call.id.clone()),
        tool_calls: None,
    }
}

/// Salvage `<tool_call>...</tool_call>` blocks the model emitted as
/// plain text but the daemon's tool-call parser didn't pick up.
///
/// Why this is needed: the daemon (`sovereign-mesh::inference_adapter`)
/// is supposed to extract every `<tool_call>...</tool_call>` JSON
/// block from a model's raw output and surface it as a structured
/// `tool_calls[]` field on the response, then strip the block from
/// the visible content. In practice the parser sometimes misses a
/// block — JSON-with-bad-escapes inside a `content`-as-a-string
/// arguments field, leading whitespace differences, or the model
/// using a slightly off tag shape. The block survives in the
/// content and the `tool_calls` field comes back empty.
///
/// We treat this defensively: if the model emitted no structured
/// `tool_calls` AND the content carries `<tool_call>{...}</tool_call>`
/// blocks, parse them out and synthesise `ToolCall` entries. The
/// content the agent ends up seeing in subsequent turns has the
/// blocks removed (matching what the daemon would have produced if
/// its parser had succeeded).
///
/// Returns `(content_with_blocks_stripped, recovered_calls)`. An
/// empty recovered_calls vec means nothing was salvaged and the
/// content is returned unchanged.
fn salvage_text_tool_calls(content: &str) -> (String, Vec<ToolCall>) {
    let mut recovered = Vec::new();
    let mut clean = String::with_capacity(content.len());
    let mut cursor = 0usize;
    while let Some(start_rel) = content[cursor..].find("<tool_call>") {
        let start = cursor + start_rel;
        clean.push_str(&content[cursor..start]);
        let inner_start = start + "<tool_call>".len();

        // Find the inner-blob end. Two strategies:
        //
        // (a) An explicit `</tool_call>` closer between this
        //     `<tool_call>` and the next `<tool_call>` (or EOF).
        // (b) **No closer** — the model chained multiple
        //     `<tool_call>` blocks without closing them. Use the
        //     next `<tool_call>` as a soft delimiter so each block
        //     gets parsed independently.
        //
        // The "no closer" path matters because real models
        // (Hermes-style 35B builds in particular) frequently emit
        // 2–3 chained tool calls in one assistant message and only
        // close the last one — or close none at all.
        let next_open_rel = content[inner_start..].find("<tool_call>");
        let close_rel = content[inner_start..].find("</tool_call>");
        let (inner_end, advance_past_close): (usize, usize) = match (close_rel, next_open_rel) {
            (Some(c), Some(n)) if c < n => {
                (inner_start + c, inner_start + c + "</tool_call>".len())
            }
            (Some(c), None) => (inner_start + c, inner_start + c + "</tool_call>".len()),
            (_, Some(n)) => {
                // No closer before the next opener — stop the
                // blob at the next opener, leaving the next
                // iteration to consume it.
                (inner_start + n, inner_start + n)
            }
            (None, None) => {
                // No closer, no next opener — try to parse
                // everything to EOF. If parse fails the caller
                // sees the block in clean content.
                (content.len(), content.len())
            }
        };
        let inner = content[inner_start..inner_end].trim();
        if let Some(call) = parse_tool_call_blob(inner, recovered.len()) {
            recovered.push(call);
        } else {
            // Could not parse — keep the block visible so the agent
            // can see (and possibly correct) what it emitted.
            clean.push_str(&content[start..advance_past_close.min(content.len())]);
        }
        cursor = advance_past_close;
        if cursor >= content.len() {
            break;
        }
    }
    if cursor < content.len() {
        clean.push_str(&content[cursor..]);
    }
    (clean.trim().to_string(), recovered)
}

/// Parse one inner `<tool_call>` payload into a structured
/// [`ToolCall`]. The expected shape is `{"name": "<id>",
/// "arguments": <object-or-string>}`. Tolerates `arguments` being
/// either a JSON object (the model dropped the OpenAI-style string
/// wrapping) or a JSON-string-of-an-object.
///
/// Resilient to four common model failure modes observed in real
/// trials:
///
/// - **Truncated trailing braces.** Model stops sampling one `}`
///   short of balanced.
/// - **Trailing noise after the JSON object.** Model continues
///   emitting unrelated fields (e.g. `,"feature_id":"..."`)
///   *after* the proper close. We slice at the first balanced
///   close and ignore the rest.
/// - **Trailing JSON noise** (e.g. `</think>` markers). Parse
///   forward from the first `{`.
/// - **Multiple chained tool calls** in the same block. The
///   outer salvage loop handles N distinct blocks; the per-blob
///   parser just takes the first complete object.
///
/// Returns `None` for unrecoverable payloads — the caller leaves
/// the block in content for the agent to see.
fn parse_tool_call_blob(blob: &str, ordinal: usize) -> Option<ToolCall> {
    let trimmed = blob.trim();
    let first_brace = trimmed.find('{')?;
    let candidate = &trimmed[first_brace..];

    // Pull just the first balanced JSON object out of `candidate`.
    // Handles "trailing noise" (extra fields after the proper close)
    // and multi-blob blocks.
    let balanced = first_balanced_object(candidate);
    let to_parse = balanced.as_deref().unwrap_or(candidate);

    // Three-tier recovery: literal parse → trailing-`}` recovery →
    // unescaped-arguments rewrite. The third rung catches a Qwen-
    // family failure mode where the model emits the tool's
    // `arguments` field as raw JSON (`"arguments":{...}` -shaped
    // payload but written as `"arguments":"{...}"` with the inner
    // quotes left unescaped). Without recovery the outer object
    // never parses and a perfectly intentional tool call is dropped
    // — exactly what cost us the second `research_finding` call in
    // the b5jlf1b0w trial.
    let parsed: serde_json::Value = if let Ok(v) = serde_json::from_str(to_parse) {
        v
    } else if let Some(v) = parse_with_brace_recovery(to_parse) {
        v
    } else {
        // Try the rewrite; fall through to brace-recovery on its
        // output too, since the rewrite often leaves the outer wrapper
        // one `}` short of balanced (the model truncated before
        // closing the wrapper).
        let rewritten = recover_unescaped_arguments_object(to_parse)?;
        match serde_json::from_str(&rewritten) {
            Ok(v) => v,
            Err(_) => parse_with_brace_recovery(&rewritten)?,
        }
    };
    let name = parsed.get("name").and_then(|n| n.as_str())?.to_string();
    let arguments = match parsed.get("arguments") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    };
    Some(ToolCall {
        id: format!("salvaged-{ordinal}-{}", name),
        kind: "function".to_string(),
        function: FunctionCall { name, arguments },
    })
}

/// Return the first balanced `{...}` JSON object in `s`, or
/// `None` if `s` doesn't open with `{` or never balances. Walks
/// the bytes tracking string boundaries (so braces inside JSON
/// strings don't count) and the `\` escape state. Stops at the
/// first `}` that makes depth = 0 and returns everything up to
/// and including that close.
///
/// Cheap and self-contained: we don't need a full JSON parser to
/// decide where a balanced close lands, and a real parser would
/// reject the trailing noise that motivates this helper.
fn first_balanced_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[..=i].to_string());
                }
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Recover the Qwen-style `"arguments":"{...}"` emission where the
/// inner JSON was meant to be a stringified object but was written
/// with unescaped inner quotes. Rewrite to `"arguments":{...}`
/// (raw object) which the upstream `parse_tool_call_blob` handler
/// already accepts via the `Some(other) => other.to_string()` arm.
///
/// The model often also drops the closing `"` and the wrapper's
/// closing `}`. We strip the post-object `"` if present and let
/// `parse_with_brace_recovery` add the missing wrapper close.
///
/// Returns `None` if the anchor isn't present or the balanced inner
/// object can't be found — i.e. nothing to recover.
fn recover_unescaped_arguments_object(text: &str) -> Option<String> {
    // Anchor on the exact literal that signals the failure mode.
    // Tolerate optional whitespace between `:` and `"` since some
    // emissions have `"arguments": "{` and others have no space.
    let anchor_variants = ["\"arguments\":\"{", "\"arguments\": \"{"];
    let (anchor_pos, anchor_len) = anchor_variants
        .iter()
        .find_map(|a| text.find(a).map(|p| (p, a.len())))?;
    // The `{` is the last byte of the anchor.
    let object_start = anchor_pos + anchor_len - 1;
    let balanced = first_balanced_object(&text[object_start..])?;
    let after_obj = object_start + balanced.len();
    // Skip the trailing `"` the model meant to close the arguments
    // string with, if present. Without this we'd produce
    // `"arguments":{...}"` which is itself malformed.
    let tail_start = if text.as_bytes().get(after_obj) == Some(&b'"') {
        after_obj + 1
    } else {
        after_obj
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..anchor_pos]);
    out.push_str("\"arguments\":");
    out.push_str(&balanced);
    out.push_str(&text[tail_start..]);
    Some(out)
}

/// Try to parse `candidate` as JSON, appending up to 4 trailing
/// closing braces if the original parse fails with an "unfinished
/// JSON term" / unbalanced-braces error. Stops as soon as parsing
/// succeeds. Returns `None` if no amount of trailing-`}` recovery
/// produces valid JSON — that's a genuinely-malformed payload, not
/// a mid-emission truncation.
fn parse_with_brace_recovery(candidate: &str) -> Option<serde_json::Value> {
    const MAX_APPEND: usize = 4;
    let mut buf = candidate.to_string();
    for _ in 0..MAX_APPEND {
        buf.push('}');
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&buf) {
            return Some(v);
        }
    }
    None
}

// ─── Tool-call loop for one partner turn ────────────────────────

struct TurnOutcome {
    final_content: String,
    tool_calls: usize,
    iters: usize,
    elapsed_secs: f32,
}

async fn run_one_turn(
    http: &reqwest::Client,
    base: &str,
    chat_model: &str,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    registry: &ToolRegistry,
    ctx: &ToolContext,
    project: &RecipeProject,
    max_iters: usize,
    strip_think: bool,
) -> std::result::Result<TurnOutcome, String> {
    let started = std::time::Instant::now();
    let mut tool_calls = 0usize;
    let mut iters = 0usize;
    // Per-turn loop detector with two trip wires:
    // - `EXACT_ARGS_THRESHOLD` (3) for the literal-same-call case.
    // - `SAME_TOOL_THRESHOLD` (5) for the case where the agent
    //   permutes one word per call to dodge the exact-args check.
    //   Real failure mode this caught: 7 web_search calls each with
    //   a slightly different phrasing, all returning 0 results
    //   because DDG is rate-limited.
    const EXACT_ARGS_THRESHOLD: usize = 3;
    const SAME_TOOL_THRESHOLD: usize = 5;
    let mut tool_signature_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut tool_name_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    loop {
        if iters >= max_iters {
            return Err(format!(
                "tool-call loop exceeded {max_iters} iterations without \
                 a final assistant message"
            ));
        }
        iters += 1;
        let req = ChatCompletionRequest {
            model: Some(chat_model.to_string()),
            messages: messages.clone(),
            temperature: Some(0.3),
            // 8192 tokens per response. The natural budget is the
            // sum of "diagnose the result + emit the next tool call".
            // 4096 broke down on the bw9pkay71 trial: agent received
            // a recipe_write_structured validation error, narrated 30
            // lines of analysis weighing PDF vs HTML vs JSON
            // alternatives, and ran out of generation tokens before
            // emitting the corrective tool call. 8192 leaves room
            // for both a complex think AND a tool call in the same
            // assistant message.
            max_tokens: Some(8192),
            stream: Some(false),
            tools: Some(tools.to_vec()),
            tool_choice: Some(serde_json::json!("auto")),
        };
        let url = format!("{base}/v1/chat/completions");
        let resp: ChatCompletionResponse = http
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?
            .error_for_status()
            .map_err(|e| format!("daemon returned non-success: {e}"))?
            .json()
            .await
            .map_err(|e| format!("parse chat completion: {e}"))?;
        let ChatChoice { message, .. } = resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| "chat completion has no choices".to_string())?;
        let raw_content = if strip_think {
            strip_think_block(&message.content)
        } else {
            message.content.clone()
        };
        // Daemon-supplied structured tool calls take priority. If the
        // daemon parser missed a `<tool_call>` block in the raw
        // content, recover it client-side so the loop can continue.
        let mut calls = message.tool_calls.clone().unwrap_or_default();
        let (content_for_log, salvaged) = if calls.is_empty() {
            salvage_text_tool_calls(&raw_content)
        } else {
            (raw_content.clone(), Vec::new())
        };
        if !salvaged.is_empty() {
            eprintln!(
                "  (salvaged {} tool call(s) from text-format <tool_call> blocks)",
                salvaged.len()
            );
            calls.extend(salvaged);
        }
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: content_for_log.clone(),
            tool_call_id: None,
            tool_calls: if calls.is_empty() {
                None
            } else {
                Some(calls.clone())
            },
        });
        if calls.is_empty() {
            return Ok(TurnOutcome {
                final_content: content_for_log,
                tool_calls,
                iters,
                elapsed_secs: started.elapsed().as_secs_f32(),
            });
        }
        for call in &calls {
            tool_calls += 1;
            eprintln!(
                "  → tool: {}({}…)",
                call.function.name,
                &call.function.arguments[..call.function.arguments.len().min(120)]
                    .replace('\n', " ")
            );
            // Loop-detection: two trip wires per turn.
            // - Exact-args repeat: caught by `tool_signature_counts`
            //   on `(name, normalised_args)` — fires at >3.
            // - Same-tool spam: caught by `tool_name_counts` on
            //   `name` only — fires at >5. Catches the case where
            //   the agent permutes one query word per call.
            // Either trip emits a synthesised tool result that
            // nudges the agent to ask the partner / switch tools.
            // `done` is the virtual termination tool injected into
            // the envelope schema by the daemon adapter when
            // SOVEREIGN_ALTERNATION_GRAMMAR is on. Treat it as
            // turn-end here: append a synthesised tool-result so the
            // OpenAI message history stays well-formed, then break
            // out of the iteration loop. Mirrors the equivalent
            // special-case in `sovereign-core::runtime::handlers::
            // recipe_author` and in sovereign-agent-bench's native
            // runner.
            if call.function.name == "done" {
                let reason = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                    .ok()
                    .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
                    .unwrap_or_default();
                eprintln!(
                    "  → done: {}",
                    if reason.is_empty() {
                        "(no reason given)"
                    } else {
                        reason.as_str()
                    }
                );
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: serde_json::json!({
                        "done": true,
                        "reason": reason,
                    })
                    .to_string(),
                    tool_call_id: Some(call.id.clone()),
                    tool_calls: None,
                });
                return Ok(TurnOutcome {
                    final_content: if reason.is_empty() {
                        "(done — no reason supplied)".to_string()
                    } else {
                        reason
                    },
                    tool_calls,
                    iters,
                    elapsed_secs: started.elapsed().as_secs_f32(),
                });
            }

            let signature = call_signature(call);
            let sig_count = tool_signature_counts.entry(signature).or_insert(0);
            *sig_count += 1;
            let name_count = tool_name_counts
                .entry(call.function.name.clone())
                .or_insert(0);
            *name_count += 1;
            let tool_msg = if *sig_count > EXACT_ARGS_THRESHOLD {
                eprintln!(
                    "  (loop detector: {} exact-args repeat {} — \
                     short-circuiting)",
                    call.function.name, *sig_count
                );
                synthesize_loop_break_message(call, *sig_count, "exact_args")
            } else if *name_count > SAME_TOOL_THRESHOLD {
                eprintln!(
                    "  (loop detector: {} same-tool repeat {} — \
                     short-circuiting)",
                    call.function.name, *name_count
                );
                synthesize_loop_break_message(call, *name_count, "same_tool")
            } else {
                execute_tool_call(registry, call, ctx, project).await
            };
            messages.push(tool_msg);
        }
    }
}

// ─── Runtime-path drivers (--via-runtime) ───────────────────────
//
// Hit the daemon's Runtime-backed conversation API so the daemon owns
// the recipe-author agent loop (the desktop-equivalent path), rather
// than re-running the loop client-side against /v1/chat/completions.

/// Create a conversation tagged `skill_id = "recipe-author"` so the
/// daemon routes its messages into the recipe-author agent loop.
async fn create_runtime_conversation(
    http: &reqwest::Client,
    base: &str,
) -> std::result::Result<String, String> {
    let url = format!("{base}/v1/conversations");
    let resp: serde_json::Value = http
        .post(&url)
        .json(&serde_json::json!({ "skill_id": "recipe-author" }))
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?
        .error_for_status()
        .map_err(|e| {
            format!(
                "daemon returned non-success creating conversation ({e}). \
                 Is the daemon built with recipe-author conversation support?"
            )
        })?
        .json()
        .await
        .map_err(|e| format!("parse create-conversation response: {e}"))?;
    resp.get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "create-conversation response missing `id`".to_string())
}

/// Post one partner turn and return the agent's final reply. A single
/// call blocks for the daemon's whole server-side tool loop.
async fn send_runtime_message(
    http: &reqwest::Client,
    base: &str,
    conversation_id: &str,
    content: &str,
) -> std::result::Result<String, String> {
    let url = format!("{base}/v1/conversations/{conversation_id}/messages");
    let resp: serde_json::Value = http
        .post(&url)
        .json(&serde_json::json!({ "content": content }))
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("daemon returned non-success: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse message response: {e}"))?;
    Ok(resp
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

// ─── Trial entry ─────────────────────────────────────────────────

pub async fn run_live_trial(argv: &[String]) -> i32 {
    if argv.first().map(String::as_str) == Some("--help")
        || argv.first().map(String::as_str) == Some("-h")
    {
        print_help();
        return 0;
    }
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("recipe-agent live-trial: {e}");
            print_help();
            return 1;
        }
    };

    let charter = match std::fs::read_to_string(&args.charter_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "live-trial: failed to read charter {}: {e}",
                args.charter_path.display()
            );
            return 1;
        }
    };
    let script_text = match std::fs::read_to_string(&args.script_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "live-trial: failed to read script {}: {e}",
                args.script_path.display()
            );
            return 1;
        }
    };
    let messages_in = parse_script(&script_text);
    if messages_in.is_empty() {
        eprintln!(
            "live-trial: script {} has no messages",
            args.script_path.display()
        );
        return 1;
    }

    if let Err(e) = probe_daemon(&args.daemon_base).await {
        eprintln!("live-trial: {e}");
        return 2;
    }

    eprintln!("Daemon: {}", args.daemon_base);
    let chat_model = match resolve_chat_model(&args.daemon_base, args.chat_model.as_deref()).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("live-trial: {e}");
            return 2;
        }
    };
    eprintln!("Chat model: {chat_model}");

    // Stores. We touch the user's real ~/.svrnmesh/{notes,features}.db
    // on purpose — a live trial against the running daemon is a real
    // session. To sandbox, point HOME at a tempdir before invoking.
    let dotsovereign = sovereign_contracts::rebrand::svrnmesh_root();
    let notes: Arc<dyn RecipeNotes> = match NoteStore::open(&dotsovereign.join("notes.db")) {
        Ok(s) => Arc::new(NoteStoreRecipeNotes::new(Arc::new(s))),
        Err(e) => {
            eprintln!("live-trial: notes store: {e}");
            return 2;
        }
    };
    let features = match RecipeProjectStore::open(&dotsovereign.join("features.db")) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("live-trial: feature store: {e}");
            return 2;
        }
    };

    let project = match args.feature_id.as_deref() {
        Some(fid) => {
            match RecipeProject::load(fid, Arc::clone(&notes), Arc::clone(&features)).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("live-trial: load project {fid}: {e}");
                    return 2;
                }
            }
        }
        None => {
            let title = args
                .title
                .clone()
                .or_else(|| {
                    args.charter_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "live-trial project".to_string());
            match RecipeProject::new(&title, &charter, Arc::clone(&notes), Arc::clone(&features))
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("live-trial: provision project: {e}");
                    return 2;
                }
            }
        }
    };
    eprintln!(
        "Project:    {} (feature_id={})",
        project.title(),
        project.feature_id()
    );
    eprintln!("Project dir: {}", project.project_dir().display());

    // System prompt from skill manifest.
    let system_prompt = match load_recipe_author_system_prompt(&args.skills_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("live-trial: {e}");
            return 2;
        }
    };
    eprintln!(
        "Skill prompt: {} chars from {}",
        system_prompt.len(),
        args.skills_dir
            .join("recipe-author")
            .join("skill.toml")
            .display()
    );

    // Tool registry — focused on recipe-author + web research only.
    let recipes_dir = dotsovereign.join("recipes");
    let _ = std::fs::create_dir_all(&recipes_dir);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(RecipeReadTool::new()));
    registry.register(Box::new(RecipeWriteTool::new()));
    registry.register(Box::new(RecipeWriteStructuredTool::new(Arc::new(
        CorpusEngineRecipeTester::new(),
    ))));
    registry.register(Box::new(RecipeValidateTool::new(Arc::new(
        CorpusEngineRecipeTester::new(),
    ))));
    registry.register(Box::new(RecipeTestTool::new(Arc::new(
        CorpusEngineRecipeTester::new(),
    ))));
    registry.register(Box::new(RegistryBrowseTool));
    registry.register(Box::new(DecisionLogTool::with_notes(Arc::clone(&notes))));
    registry.register(Box::new(CheckpointTool::with_stores(
        Arc::clone(&notes),
        Arc::clone(&features),
    )));
    registry.register(Box::new(CapabilityRequestTool::with_stores(
        Arc::clone(&notes),
        Arc::clone(&features),
    )));
    // probe_url + research_finding close the API-shape loop the
    // earlier trial revealed: probe_url lets the agent confirm an
    // endpoint contract before drafting; research_finding gives the
    // v7 NoteStore `research_finding` kind a real writer so web
    // findings survive across sessions and checkpoint restores.
    //
    // Forward the trial's `--param key=value` flags into probe_url
    // so the agent can probe auth-gated endpoints by writing
    // `Authorization: Token {api_token}` without ever pasting the
    // literal token. Same surface as the http_api acquirer's
    // `[acquire].headers` interpolation.
    let probe_params: std::collections::BTreeMap<String, String> =
        args.params.iter().cloned().collect();
    registry.register(Box::new(
        ProbeUrlTool::new().with_parameters(Arc::new(probe_params)),
    ));
    registry.register(Box::new(ResearchFindingTool::with_notes(Arc::clone(
        &notes,
    ))));
    registry.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    // WebSearchTool runs a query → extract → synthesise pipeline that
    // needs an inference provider for the synthesis step. We back it
    // with a `RemoteApiProvider` pointing at the same daemon we're
    // driving the agent against — keeps the live trial honest about
    // what the production agent loop would see.
    let inference_for_search: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        &format!("{}/v1", args.daemon_base),
        None,
        &chat_model,
        8192,
    ));
    registry.register(Box::new(sovereign_tools::web::WebSearchTool::with_backend(
        inference_for_search,
        // Client built by the egress boundary (order deep-research-t2a):
        // tools-base is contract-only and must not construct an
        // egress-capable HTTP client itself.
        sovereign_core::egress::search_client().expect("egress boundary search client build"),
        sovereign_tools::web::search::SearchBackend::DuckDuckGo,
    )));
    let tool_defs = registry_to_tool_defs(&registry);
    eprintln!("Tools:      {} registered", tool_defs.len());

    // Conversation. The system prompt anchors the recipe-author
    // contract; the situated context lives in the first user message
    // (regenerated each turn so decisions made earlier this session
    // are visible). The tool-call loop appends assistant + tool
    // messages between partner turns.
    let conversation_id: ConversationId = uuid::Uuid::new_v4().to_string();
    let ctx = ToolContext {
        conversation_id: conversation_id.clone(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
        turn_index: 0,
        ..Default::default()
    };
    let http = match reqwest::Client::builder()
        // A single --via-runtime /messages POST blocks for the daemon's
        // entire server-side tool loop (up to ~12 iterations × a 35B), so
        // the budget is generous; the client-side path's per-completion
        // calls finish well under this.
        .timeout(Duration::from_secs(600))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("live-trial: http client: {e}");
            return 2;
        }
    };
    let mut messages: Vec<ChatMessage> = vec![ChatMessage::new("system", system_prompt)];

    eprintln!(
        "\nDriving {} partner turn(s) on conversation {}\n",
        messages_in.len(),
        conversation_id
    );

    let mut total_tool_calls = 0usize;
    if args.via_runtime {
        // Drive the REAL recipe-author Runtime loop over the daemon's
        // conversation API. The daemon owns the tool loop + grammar
        // (handle_recipe_author_turn); we feed partner turns and let the
        // shared ~/.svrnmesh stores carry the recipe + decisions back to
        // the post-trial assertions below.
        let conv_id = match create_runtime_conversation(&http, &args.daemon_base).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("live-trial: create runtime conversation: {e}");
                return 3;
            }
        };
        eprintln!("Runtime conversation: {conv_id} (skill_id=recipe-author)\n");
        for (i, partner_msg) in messages_in.iter().enumerate() {
            eprintln!("──── Turn {} ─────────────────────────────────", i + 1);
            eprintln!("Partner: {partner_msg}\n");
            let situated = match situated_context::render(&project).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("live-trial: situated render failed: {e}");
                    return 2;
                }
            };
            let content = format!(
                "Project state for this turn:\n\n{situated}\n\n\
                 ---\n\nPartner says: {partner_msg}"
            );
            let started = std::time::Instant::now();
            match send_runtime_message(&http, &args.daemon_base, &conv_id, &content).await {
                Ok(reply) => eprintln!(
                    "\nAgent ({:.1}s, via daemon Runtime):\n{reply}\n",
                    started.elapsed().as_secs_f32()
                ),
                Err(e) => {
                    eprintln!("live-trial: turn {} failed: {e}\n", i + 1);
                    return 3;
                }
            }
        }
    } else {
        for (i, partner_msg) in messages_in.iter().enumerate() {
            eprintln!("──── Turn {} ─────────────────────────────────", i + 1);
            eprintln!("Partner: {partner_msg}\n");
            let situated = match situated_context::render(&project).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("live-trial: situated render failed: {e}");
                    return 2;
                }
            };
            // Combine situated context + partner message into a single
            // user turn. NO bracketed framing — the router-free path
            // doesn't need cues, and unframed prose stays out of the
            // model's "this is meta-discussion" bucket.
            let user_text = format!(
                "Project state for this turn:\n\n{situated}\n\n\
                 ---\n\nPartner says: {partner_msg}"
            );
            messages.push(ChatMessage::new("user", user_text));
            match run_one_turn(
                &http,
                &args.daemon_base,
                &chat_model,
                &mut messages,
                &tool_defs,
                &registry,
                &ctx,
                &project,
                args.max_tool_iters,
                args.strip_think,
            )
            .await
            {
                Ok(out) => {
                    eprintln!(
                        "\nAgent ({:.1}s, {} tool call(s) over {} iter(s)):\n{}\n",
                        out.elapsed_secs, out.tool_calls, out.iters, out.final_content
                    );
                    total_tool_calls += out.tool_calls;
                }
                Err(e) => {
                    eprintln!("live-trial: turn {} failed: {e}\n", i + 1);
                    return 3;
                }
            }
        }
    }

    // ─── Post-trial assertions ────────────────────────────────
    eprintln!("──── Post-trial summary ─────────────────────────");
    let summary = match project.read_summary() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("live-trial: read summary: {e}");
            return 4;
        }
    };
    let scope = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(project.feature_id().to_string()),
    };
    let decisions = notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &["decision".to_string()],
            1000,
            false,
            &scope,
        )
        .await
        .unwrap_or_default();
    let checkpoints = project.list_checkpoints().unwrap_or_default();
    let cap_requests = notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &["capability_request".to_string()],
            100,
            false,
            &scope,
        )
        .await
        .unwrap_or_default();

    eprintln!("Recipe id:           {:?}", summary.recipe_id);
    eprintln!("Decisions logged:    {}", decisions.len());
    eprintln!("Checkpoints:         {}", checkpoints.len());
    eprintln!("Capability requests: {}", cap_requests.len());
    eprintln!("Total tool calls:    {total_tool_calls}");

    let mut overall_pass = true;
    if let Some(recipe_id) = summary.recipe_id.as_deref() {
        eprintln!("\nValidating {} …", recipe_id);
        let validate_tool = RecipeValidateTool::with_recipes_dir(
            Arc::new(CorpusEngineRecipeTester::new()),
            recipes_dir.clone(),
        );
        match validate_tool
            .execute(&serde_json::json!({"path": recipe_id}), &ctx)
            .await
        {
            Ok(StepOutput::Json(v)) => {
                let passed = v.get("passed").and_then(|p| p.as_bool()).unwrap_or(false);
                if passed {
                    eprintln!("  validate: PASS");
                } else {
                    let errs = v
                        .get("errors")
                        .and_then(|e| e.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    eprintln!("  validate: FAIL ({errs} error[s])");
                    overall_pass = false;
                }
            }
            Ok(other) => {
                eprintln!("  validate: unexpected output {other:?}");
                overall_pass = false;
            }
            Err(e) => {
                eprintln!("  validate: error {e}");
                overall_pass = false;
            }
        }

        if args.no_fetch {
            eprintln!(
                "\nSkipping post-trial fetch — --no-fetch is set. \
                 (recipe validated; upstream API was not contacted)"
            );
            // Skip the recipe_test invocation entirely so rate-limited
            // upstreams (CourtListener etc.) don't get hit during
            // wiring smoke tests. Falls through to the trial-summary
            // block below.
        } else {
            eprintln!(
                "\nFetching initial sample (sample_size={}, params={}) …",
                args.sample_size,
                args.params.len()
            );
            let test_tool = RecipeTestTool::with_recipes_dir(
                Arc::new(CorpusEngineRecipeTester::new()),
                recipes_dir.clone(),
            );
            // Forward `--param k=v` flags through to `recipe_test` so
            // recipes that declare an install-time parameter (auth tokens,
            // jurisdiction filters, etc.) get the partner's value at fetch
            // time without ever entering the recipe file.
            let mut params_json = serde_json::Map::new();
            for (k, v) in &args.params {
                params_json.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
            let mut test_args = serde_json::Map::new();
            test_args.insert(
                "path".into(),
                serde_json::Value::String(recipe_id.to_string()),
            );
            test_args.insert(
                "sample_size".into(),
                serde_json::Value::from(args.sample_size),
            );
            if !params_json.is_empty() {
                test_args.insert("params".into(), serde_json::Value::Object(params_json));
            }
            match test_tool
                .execute(&serde_json::Value::Object(test_args), &ctx)
                .await
            {
                Ok(StepOutput::Json(v)) => {
                    let attempted = v
                        .get("extraction")
                        .and_then(|e| e.get("records_attempted"))
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0);
                    let succeeded = v
                        .get("extraction")
                        .and_then(|e| e.get("records_succeeded"))
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0);
                    let rate = v
                        .get("extraction")
                        .and_then(|e| e.get("extraction_rate"))
                        .and_then(|n| n.as_f64())
                        .unwrap_or(0.0);
                    eprintln!("  test: attempted={attempted} succeeded={succeeded} rate={rate:.2}");
                    if succeeded == 0 {
                        eprintln!("  test: FAIL — zero docs extracted");
                        overall_pass = false;
                    } else {
                        eprintln!("  test: PASS");
                    }
                }
                Ok(other) => {
                    eprintln!("  test: unexpected output {other:?}");
                    overall_pass = false;
                }
                Err(e) => {
                    eprintln!("  test: error {e}");
                    overall_pass = false;
                }
            }
        } // end --no-fetch else
    } else {
        eprintln!("\n(no recipe drafted — agent didn't reach recipe_write)");
        overall_pass = false;
    }

    if decisions.is_empty() {
        eprintln!(
            "\nWarning: no decisions logged. The agent should be calling \
             DecisionLog on every non-trivial choice."
        );
    }
    if checkpoints.is_empty() {
        eprintln!(
            "Warning: no checkpoints created. The agent should have \
             checkpointed at project creation at minimum."
        );
    }

    if overall_pass {
        eprintln!("\nLive trial: PASS");
        0
    } else {
        eprintln!("\nLive trial: FAIL");
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_script_skips_comments_and_blank_lines() {
        let text =
            "# header\n\nFirst message.\n\n# mid comment\nSecond,\nstill second.\n\nThird.\n";
        let blocks = parse_script(text);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], "First message.");
        assert_eq!(blocks[1], "Second,\nstill second.");
        assert_eq!(blocks[2], "Third.");
    }

    #[test]
    fn parse_script_handles_empty_input() {
        assert!(parse_script("").is_empty());
        assert!(parse_script("# only a comment\n").is_empty());
        assert!(parse_script("\n\n\n").is_empty());
    }

    #[test]
    fn parse_script_trims_trailing_whitespace() {
        let blocks = parse_script("Just one line.   \n\n");
        assert_eq!(blocks, vec!["Just one line."]);
    }

    #[test]
    fn strip_think_block_removes_paired_tags() {
        let s = "<think>plan</think>\n\nFinal answer here.";
        assert_eq!(strip_think_block(s), "Final answer here.");
    }

    #[test]
    fn strip_think_block_passes_through_when_unpaired() {
        // Unmatched <think> with no closing tag — preserve as-is so
        // we don't silently drop content.
        let s = "<think>still thinking";
        assert_eq!(strip_think_block(s), s);
    }

    #[test]
    fn salvage_recovers_truncated_one_brace_short() {
        // Real-world failure mode: model stops sampling one `}` short
        // of balanced. Salvage should recover via brace-balancing.
        let content = r#"<tool_call>{"name":"recipe_write_structured","arguments":{"path":"demo","recipe":{"corpus":{"id":"demo","name":"demo"},"acquire":{"type":"bulk_download","url":"https://x"},"extract":{"type":"html"},"chunk":{"type":"paragraph"}}}</tool_call>"#;
        let (_clean, calls) = salvage_text_tool_calls(content);
        assert_eq!(calls.len(), 1, "expected one recovered call");
        assert_eq!(calls[0].function.name, "recipe_write_structured");
    }

    #[test]
    fn salvage_returns_none_for_genuinely_broken_json() {
        // Random bytes inside a tool_call block — neither valid JSON
        // nor recoverable by brace-balancing.
        let content = "<tool_call>not json at all { missing quotes</tool_call>";
        let (clean, calls) = salvage_text_tool_calls(content);
        assert!(calls.is_empty(), "expected no recovered calls");
        // Block is preserved in clean content.
        assert!(clean.contains("<tool_call>"));
    }

    #[test]
    fn salvage_strips_trailing_noise_after_balanced_close() {
        // Real-world failure mode: model emits a balanced JSON object
        // and then tacks an extra `feature_id` field on after the
        // proper close — invalid JSON but the meaningful tool call is
        // up front.
        let content = r#"<tool_call>{"name":"checkpoint","arguments":{"feature_id":"abc","name":"test","trigger":"partner_request"}},"feature_id":"abc"}</tool_call>"#;
        let (_clean, calls) = salvage_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "checkpoint");
    }

    #[test]
    fn salvage_recovers_multiple_chained_unclosed_blocks() {
        // Real-world failure mode: model emits 2+ `<tool_call>` blocks
        // in one assistant message without `</tool_call>` between them.
        // The outer salvage loop should treat each `<tool_call>` as a
        // soft boundary.
        let content = r#"<tool_call>{"name":"recipe_validate","arguments":{"path":"x"}}<tool_call>{"name":"checkpoint","arguments":{"feature_id":"y","name":"z","trigger":"partner_request"}}"#;
        let (_clean, calls) = salvage_text_tool_calls(content);
        assert_eq!(calls.len(), 2, "expected two recovered calls");
        assert_eq!(calls[0].function.name, "recipe_validate");
        assert_eq!(calls[1].function.name, "checkpoint");
    }

    #[test]
    fn salvage_recovers_unescaped_arguments_object_qwen_emit() {
        // Real-world failure mode (b5jlf1b0w trial Turn 3): Qwen
        // emits the tool's `arguments` field as raw JSON (`{...}`)
        // but writes it inside an outer string literal without
        // escaping the inner quotes. The result is malformed at
        // the outer level — `serde_json::from_str` rejects it on
        // the first inner `"` after `"arguments":"`. Salvage's
        // third-tier recovery rewrites `"arguments":"{...}"` to
        // `"arguments":{...}` and re-parses.
        let content = r#"<tool_call>{"name":"research_finding","arguments":"{"feature_id": "abc", "claim": "v4 uses cluster__docket__court", "source_url": "https://example.com", "confidence": "high", "scope": "api_contract"}
</tool_call>"#;
        let (_clean, calls) = salvage_text_tool_calls(content);
        assert_eq!(calls.len(), 1, "expected one recovered call");
        assert_eq!(calls[0].function.name, "research_finding");
        // Arguments should round-trip back to a valid JSON object
        // string with the original fields intact.
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["feature_id"], "abc");
        assert_eq!(args["confidence"], "high");
    }

    #[test]
    fn first_balanced_object_handles_strings_with_braces() {
        // Brace inside a JSON string must not affect depth tracking.
        let s = r#"{"url":"https://x?a={b}&c=1","tail":true}EXTRA"#;
        let bal = first_balanced_object(s).unwrap();
        assert_eq!(bal, r#"{"url":"https://x?a={b}&c=1","tail":true}"#);
    }
}
