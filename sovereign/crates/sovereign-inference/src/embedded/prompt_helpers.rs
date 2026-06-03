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
use crate::llama::cpp::mtp::MtpSession;
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaChatMessage, LlamaModel};
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::{LlamaContextExt, LlamaModelExt};

use sovereign_core::error::Error;
use sovereign_core::model_family::{EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

// ─── Shared helpers ────────────────────────────────────────────

/// Resolve the actual `max_tokens` budget for a generation, given the
/// caller's request, the prompt length, and the loaded context window.
///
/// Three behaviours we want to preserve, in order of importance:
///   1. **Never exceed the context window.** llama.cpp's KV cache
///      asserts on overflow and crashes the daemon. The decode loop
///      treats this cap as the upper bound for `n_generated`.
///   2. **Never reject a request that could partially succeed.** If a
///      caller asks for max_tokens=4096 on a 32k context but their
///      prompt is 30k tokens, we'd rather emit ~2k tokens than 503
///      with "Prompt too long" — opencode and aider don't recover
///      from that gracefully and the user just sees a dead chat.
///   3. **Default generously when the caller omits max_tokens.** A
///      1024-token cap (the previous default) clipped most coding
///      replies mid-thought. We hand back the entire remaining
///      context window in that case; the EOG sampler still terminates
///      naturally for short answers.
///
/// Errors only when the prompt itself doesn't fit. That's a real
/// "your input is too big" — no clamping can recover.
/// Read the `general.architecture` metadata field from a loaded gguf.
/// Returns the raw arch string ("qwen3", "qwen3_moe", "mamba",
/// "deltanet", "gemma3", etc.) or an empty string when the metadata
/// read fails. The caller treats empty as "unknown" and falls back
/// to the name-pattern heuristic.
///
/// Cheap: a single `llama_model_meta_val_str(model, "general.architecture", buf, 256)`
/// call. Done once at slot construction; the result lives on
/// `SlotContext.arch` for the lifetime of the slot.
pub(crate) fn read_gguf_arch(model: &LlamaModel) -> String {
    match model.meta_val_str("general.architecture", 256) {
        Ok(s) => {
            tracing::info!(arch = %s, "gguf arch: read from general.architecture");
            s
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "gguf arch read failed — falling back to name-pattern heuristic"
            );
            String::new()
        }
    }
}

/// Is a gguf architecture string known to have recurrent layers
/// (Mamba / Gated DeltaNet / RWKV / hybrid SSM+MoE)?
///
/// Source of truth for the `prefix_cache_safe` gate when the slot
/// carries arch metadata. Empty string is "unknown" — caller falls
/// back to `ModelQuirks::has_recurrent_layers` declared per-family
/// in `model_family.rs`.
pub(crate) fn is_recurrent_arch(arch: &str) -> bool {
    if arch.is_empty() {
        return false;
    }
    let lower = arch.to_lowercase();
    // Qwen MoE families (Qwen3-MoE, Qwen3.5-MoE, Qwen3.6-MoE) carry
    // Gated DeltaNet layers in the gguf attention stack. Same
    // recurrent-state hazard as Mamba — partial-keep prefix-cache
    // returns -1 on tail decode. Observed gguf arch values include
    // `qwen3moe`, `qwen35moe`, `qwen3_moe`, `qwen36moe`. The shape
    // is "qwen<version>moe", so we substring-match `moe` after a
    // qwen prefix.
    if lower.starts_with("qwen") && lower.contains("moe") {
        return true;
    }
    // Explicit recurrent architectures published in gguf metadata.
    // Substring match catches version suffixes (mamba2, rwkv6, etc.).
    for marker in ["mamba", "rwkv", "deltanet", "ssm"] {
        if lower.contains(marker) {
            return true;
        }
    }
    false
}


pub(crate) fn clamp_max_tokens(
    requested: Option<usize>,
    prompt_tokens: usize,
    n_ctx: usize,
) -> Result<usize> {
    if prompt_tokens >= n_ctx {
        return Err(Error::Inference(format!(
            "Prompt too long: {prompt_tokens} tokens already meets or exceeds \
             the context window of {n_ctx}. Shorten the conversation."
        )));
    }
    let headroom = n_ctx - prompt_tokens;
    let resolved = match requested {
        Some(asked) if asked > headroom => {
            tracing::warn!(
                requested = asked,
                clamped_to = headroom,
                prompt_tokens,
                n_ctx,
                "max_tokens exceeded context headroom; clamping to fit instead of rejecting"
            );
            headroom
        }
        Some(asked) => asked,
        // No explicit cap — give the model the whole remaining
        // window. Generation still stops at EOG for short replies.
        None => headroom,
    };
    Ok(resolved)
}

pub(crate) fn format_prompt(
    model: &LlamaModel,
    model_id: &str,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> Result<String> {
    // Compute the rendered prompt via the inner tier dispatch, then
    // append `request.assistant_prefix` if present. The prefix lands
    // *after* the chat template's generation-position marker
    // (`<|turn>model\n`, `<|im_start|>assistant\n`, `<start_of_turn>model\n`,
    // …) and *before* the model's first generated token — letting
    // upstream nudges (frontdoor's read-attractor, failure-recovery)
    // commit the model to a known-good response prefix structurally
    // rather than via instruction. Family-agnostic: every chat
    // template the inner dispatch handles ends at the generation
    // marker, so the append point is consistent.
    let rendered = format_prompt_inner(model, model_id, request, quirks)?;
    Ok(append_assistant_prefix(rendered, request.assistant_prefix.as_deref()))
}

/// Append a non-empty `assistant_prefix` to the rendered prompt.
/// Empty / None prefixes pass through unchanged so the historical
/// behaviour (no prefill) is preserved for callers that don't set
/// the field.
fn append_assistant_prefix(mut prompt: String, prefix: Option<&str>) -> String {
    if let Some(p) = prefix {
        if !p.is_empty() {
            // The rendered prompt always ends at the generation
            // marker with no trailing whitespace contract — append
            // directly. If a future template renders trailing
            // whitespace, the prefix still composes; the model's
            // tokenizer absorbs the whitespace into the prefix's
            // first token.
            prompt.push_str(p);
            tracing::debug!(
                prefix_len = p.len(),
                "format_prompt: assistant_prefix appended"
            );
        }
    }
    prompt
}

fn format_prompt_inner(
    model: &LlamaModel,
    model_id: &str,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> Result<String> {
    // Inject thinking-mode token into the system message based on family quirks.
    // `think_budget == Some(0)` signals the caller wants thinking suppressed.
    // For SystemPromptToken families (Qwen3, Qwen3.5, SmolLM3): append /think or /no_think.
    // For AlwaysOn (Phi-4-reasoning): thinking cannot be disabled — don't inject.
    // For None (Gemma3, Llama3, Phi-4): no thinking tokens exist — don't inject.
    let base_system = request.system_message.as_deref().unwrap_or("");
    let system_with_thinking = match &quirks.thinking {
        ThinkingControl::SystemPromptToken { enable, disable } => {
            let token = if request.think_budget == Some(0) { disable } else { enable };
            if base_system.is_empty() {
                token.clone()
            } else {
                format!("{base_system}\n{token}")
            }
        }
        // AlwaysOn: thinking is structural — injecting /no_think degrades output.
        // None: family has no thinking tokens — nothing to inject.
        ThinkingControl::AlwaysOn | ThinkingControl::None => base_system.to_string(),
    };

    // Tool-use prompt augmentation. Qwen3.5's chat template expects tool
    // schemas to appear in the system prompt inside a `<tools>...</tools>`
    // block of newline-separated JSON objects. The model then emits tool
    // calls as `<tool_call>{"name": "...", "arguments": {...}}</tool_call>`
    // which `parse_tool_calls_from_text` below decodes.
    //
    // We append to the existing system prompt (rather than replacing or
    // injecting a separate message) so the thinking-token augmentation
    // above still lands. Tools without a system prompt get a minimal
    // "You may call one of the following tools" lead-in so the model
    // doesn't try to emit tool calls in free chat by default.
    let system_with_tools = if let Some(tools) = request.tools.as_ref().filter(|t| !t.is_empty()) {
        let mut block = String::from(
            "\n\n# Tools\n\nYou may call one or more of the following tools by emitting a \
             `<tool_call>{\"name\": ..., \"arguments\": {...}}</tool_call>` block. One call per \
             block. After a block, stop — the runtime will execute the tool and feed the result \
             back to you in the next turn.\n\n<tools>\n"
        );
        for t in tools {
            let entry = serde_json::json!({
                "name": t.name,
                "description": t.description.clone().unwrap_or_default(),
                "parameters": t.parameters,
            });
            block.push_str(&entry.to_string());
            block.push('\n');
        }
        block.push_str("</tools>");
        if system_with_thinking.is_empty() {
            block.trim_start().to_string()
        } else {
            format!("{system_with_thinking}{block}")
        }
    } else {
        system_with_thinking
    };
    let system_with_thinking = system_with_tools;

    // Three-tier prompt-building strategy. Each tier is tried in
    // order; the first one that succeeds wins.
    //
    // 1. **Basic `apply_chat_template`** — calls llama.cpp's
    //    built-in `llama_chat_apply_template`. Fast, no JSON
    //    serialization, but its template parser only supports a
    //    limited Jinja-like subset (no macros, no complex control
    //    flow). Works for Qwen3 / Llama3 / base Phi-4 / etc.
    //
    // 2. **`apply_chat_template_oaicompat` with `use_jinja=true`**
    //    — calls llama.cpp's full minja-based Jinja2 path (the
    //    same one llama-server's `--jinja` flag uses). Handles
    //    templates that use macros or complex control flow.
    //    Required for Gemma 3/4 (their gguf templates start with
    //    `{%- macro format_parameters() -%}`).
    //
    // 3. **Plain-text concat** (last resort) — `{system}\n\n{user}`
    //    with no role markers. The model has no turn boundaries
    //    so it'll role-play multi-turn ("User: ... Assistant: ...")
    //    until `max_tokens`. We loud-warn so operators see this is
    //    happening — it was a silent fallback up until 2026-04-26
    //    (gemma-4-31B atlas bench debugging session).
    // Retrieve the chat template stored in the model's gguf metadata
    // (`tokenizer.chat_template`). Pre-2026-05-17 this returned a
    // `LlamaChatTemplate` struct; post-migration to llama-cpp-4 0.2.x
    // it returns an `Option<String>`. `None` ⇒ no chat-template
    // metadata in the gguf, so plain-text concat is the only path.
    let Some(template) = crate::llama::chat_template(model) else {
        tracing::warn!(
            model_id = %model_id,
            "chat_template lookup returned None — model gguf has no \
             tokenizer.chat_template metadata. Falling back to plain-text concat; \
             model output may include hallucinated role markers and may not stop \
             at the right place."
        );
        return Ok(plain_text_prompt(&system_with_thinking, &request.prompt));
    };

    // **Migration note (2026-05-17 llama-cpp-2 → llama-cpp-4):** the
    // `openai`-shaped `apply_chat_template_oaicompat` path (with its
    // `OpenAIChatTemplateParams` and `use_jinja=true` knob) was
    // retired upstream. The remaining path is `apply_chat_template`,
    // which uses llama.cpp's built-in template parser. Two
    // consequences for callers:
    //
    //   1. Templates that need full Jinja2 (Gemma 3/4 macros, e.g.)
    //      will fail at `apply_chat_template` and fall through to
    //      plain-text concat (loud warn). The MTP bench target
    //      (Qwen3.6-A3B) uses a template the built-in parser
    //      accepts, so this regression is acceptable on this branch.
    //
    //   2. Per-request `enable_thinking` is now spliced into the
    //      system message (`/no_think` / `/think`) rather than
    //      passed as a separate flag, because the binding's chat
    //      surface no longer exposes thinking control. Most callers
    //      pass `None` / `false`; the few that pin `true` (e.g. the
    //      witness path) get a `<think>` wrapper from the prefix.
    //
    //   3. `template_needs_jinja` is retained as documentation of
    //      which templates were Gemma-shaped; calling it just routes
    //      to the same `apply_chat_template` call now, but tracing
    //      the boolean preserves the diagnostic surface.
    let needs_jinja = template_needs_jinja(&template);
    if needs_jinja {
        tracing::debug!(
            model_id = %model_id,
            "chat template needs full Jinja2 (macros/sets/includes detected); \
             routing through the Rust-side minijinja renderer instead of \
             llama.cpp's limited-subset parser."
        );
        match apply_chat_template_minijinja(
            &template,
            &system_with_thinking,
            &request.prompt,
            request.enable_thinking.unwrap_or(false),
        ) {
            Ok(rendered) => return Ok(rendered),
            Err(e) => {
                tracing::warn!(
                    model_id = %model_id,
                    error = %e,
                    template_head = %template_head_for_log(&template),
                    "minijinja render failed — falling back to llama.cpp \
                     apply_chat_template, then plain-text concat."
                );
            }
        }
    }
    if request.enable_thinking.unwrap_or(false) {
        tracing::warn!(
            model_id = %model_id,
            "request.enable_thinking=true requested, but the llama-cpp-4 binding \
             dropped chat-template thinking control. Output may not be wrapped in \
             <think>...</think>; downstream strip_thinking_tags will pass through. \
             TODO: splice /think hint into system message."
        );
    }

    let mut messages = Vec::new();
    if !system_with_thinking.is_empty() {
        messages.push(
            LlamaChatMessage::new("system".to_string(), system_with_thinking.clone())
                .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
        );
    }
    messages.push(
        LlamaChatMessage::new("user".to_string(), request.prompt.clone())
            .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
    );
    match model.apply_chat_template(Some(&template), &messages, true) {
        Ok(formatted) => return Ok(formatted),
        Err(e) => {
            tracing::warn!(
                model_id = %model_id,
                error = ?e,
                template_head = %template_head_for_log(&template),
                jinja_required = needs_jinja,
                "apply_chat_template failed — model's gguf template likely needs \
                 a Jinja construct the built-in parser doesn't support, and the \
                 0.2.x binding dropped the oaicompat Jinja2 path. Falling back to \
                 plain-text concat; model output may include hallucinated role markers."
            );
        }
    }

    // Final fallback: plain-text concat.
    Ok(plain_text_prompt(&system_with_thinking, &request.prompt))
}

/// Render a `tokenizer.chat_template` (Jinja2 source) using the
/// Rust-side `minijinja` engine. Handles macros, `set` blocks, and
/// other constructs the llama.cpp built-in parser rejects.
///
/// Returns the formatted prompt the tokenizer feeds the model.
/// `add_generation_prompt=true` matches what llama.cpp's
/// `apply_chat_template` does — appends the assistant's turn-start
/// marker so the model picks up where the template left off.
///
/// `enable_thinking` toggles the `/think` vs `/no_think` hint that
/// Qwen3-family templates honour as a Jinja variable. Models that
/// ignore the flag are unaffected.
fn apply_chat_template_minijinja(
    template: &str,
    system: &str,
    user: &str,
    enable_thinking: bool,
) -> Result<String> {
    use minijinja::{context, Environment, Value};

    // Build the messages list the template iterates over.
    let mut messages: Vec<Value> = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(Value::from_serialize(&serde_json::json!({
            "role": "system",
            "content": system,
        })));
    }
    messages.push(Value::from_serialize(&serde_json::json!({
        "role": "user",
        "content": user,
    })));

    let mut env = Environment::new();
    // Match Hugging Face's `Jinja2 Templates` behaviour: keep the
    // raise_exception filter available — some templates call it
    // (`{{ raise_exception("…") }}`) to halt on bad input.
    env.add_function(
        "raise_exception",
        |msg: String| -> std::result::Result<String, minijinja::Error> {
            Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                msg,
            ))
        },
    );
    // Python-compat method shim. HF templates routinely call
    // `.get(key)`, `.get(key, default)`, `.split(sep)`,
    // `.startswith(prefix)`, `.endswith(suffix)`, `.upper()`,
    // `.lower()`, `.strip()` — methods that exist on Python's
    // `dict`/`str`/`list` but aren't in stock minijinja. Without
    // this shim, Gemma 4's template (which calls
    // `message.get('reasoning')`, `message.get('tool_calls')`,
    // `value['type'] | upper`, `part.split('<|channel>')`, …) fails
    // at the first unknown method and we fall through to plain-text
    // concat. The pycompat surface in `minijinja-contrib` would
    // also do this, but pulling in another workspace dep for a
    // half-dozen methods is excessive — handle them inline.
    env.set_unknown_method_callback(|_state, value, method, args| {
        use minijinja::value::{from_args, ValueKind};
        use minijinja::{Error, ErrorKind, Value};
        match method {
            "get" => {
                // dict.get(key) or dict.get(key, default)
                if value.kind() != ValueKind::Map {
                    return Err(Error::from(ErrorKind::UnknownMethod));
                }
                let (key, default): (Value, Option<Value>) = from_args(args)?;
                let key_str: String = key
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| key.to_string());
                match value.get_attr(&key_str) {
                    Ok(v) if !v.is_undefined() => Ok(v),
                    _ => Ok(default.unwrap_or(Value::from(())))
                }
            }
            "split" => {
                // str.split(sep) — sep is required in HF templates we've
                // seen (no zero-arg whitespace split path needed yet).
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "split on non-string")
                })?;
                let (sep,): (String,) = from_args(args)?;
                let parts: Vec<Value> =
                    s.split(&sep).map(|p| Value::from(p.to_string())).collect();
                Ok(Value::from(parts))
            }
            "startswith" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "startswith on non-string")
                })?;
                let (prefix,): (String,) = from_args(args)?;
                Ok(Value::from(s.starts_with(&prefix)))
            }
            "endswith" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "endswith on non-string")
                })?;
                let (suffix,): (String,) = from_args(args)?;
                Ok(Value::from(s.ends_with(&suffix)))
            }
            "upper" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "upper on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.to_uppercase()))
            }
            "lower" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "lower on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.to_lowercase()))
            }
            "strip" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "strip on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.trim().to_string()))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    });
    env.add_template("chat", template).map_err(|e| {
        Error::Inference(format!("minijinja: compile chat template: {e}"))
    })?;
    let tmpl = env
        .get_template("chat")
        .map_err(|e| Error::Inference(format!("minijinja: load chat template: {e}")))?;

    // Variables every reasonable chat template touches. Most
    // templates don't use every key — providing them all is safe
    // (Jinja2 is forgiving of unused globals). Templates that
    // reference unknown vars produce empty strings, which is the
    // same behaviour as the llama.cpp path.
    let ctx = context! {
        messages => messages,
        add_generation_prompt => true,
        enable_thinking => enable_thinking,
        bos_token => "",
        eos_token => "",
        // Qwen3-family hint surfaced via a context variable on some
        // template revisions. Most templates inspect the system
        // message text instead.
        thinking_mode => if enable_thinking { "think" } else { "no_think" },
    };
    tmpl.render(ctx)
        .map_err(|e| Error::Inference(format!("minijinja: render chat template: {e}")))
}

/// Cheap heuristic for whether a chat template requires the full
/// Jinja2 (minja) renderer rather than llama.cpp's built-in
/// limited-subset parser. The built-in parser handles plain
/// `{{ var }}` substitution, simple `{% if %} ... {% endif %}`
/// blocks, and `{% for x in xs %}` loops — but trips on macros
/// (`{%- macro ... -%}`), `{% set ... %}` blocks, and a few other
/// constructs that the recent generation of model templates use.
///
/// We don't try to be exhaustive — any false negative just
/// degrades back to the existing fall-through behaviour (tier-1
/// returns FfiError, tier 2 takes over). The win here is a clean
/// fast path for known-Jinja templates so we don't log a
/// debug-level noise line on every chat call.
fn template_needs_jinja(template: &str) -> bool {
    template.contains("{%- macro")
        || template.contains("{% macro")
        || template.contains("{%- set")
        || template.contains("{% set")
        || template.contains("{%- include")
        || template.contains("{% include")
}

/// Final-fallback prompt when no chat-template path works. Just
/// concatenates system + user with a blank line. The model has no
/// turn boundaries; expect it to role-play multi-turn output.
fn plain_text_prompt(system: &str, user: &str) -> String {
    if system.is_empty() {
        user.to_string()
    } else {
        format!("{system}\n\n{user}")
    }
}

/// Serialize a (system, user) pair into the OpenAI-compatible
/// messages JSON shape that `apply_chat_template_oaicompat`
/// expects. System is omitted when empty so templates that don't
/// support a system role (e.g. Gemma) don't get a stray empty
/// turn.
fn build_oai_messages_json(system: &str, user: &str) -> Result<String> {
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user}));
    serde_json::to_string(&messages)
        .map_err(|e| Error::Inference(format!("Failed to serialize chat messages: {e}")))
}

/// First ~80 chars of a chat template, with newlines escaped, for
/// log output. Lets operators identify which template format is
/// hitting the fallback path without dumping a full multi-KB
/// macro definition into the daemon log.
fn template_head_for_log(template: &str) -> String {
    let head: String = template.chars().take(80).collect();
    head.replace('\n', "\\n")
}

/// Token budget for `<think>` blocks. After this many tokens inside a thinking
/// block the generation loop injects `</think>` into the KV cache and stream,
/// forcing the model to transition to its answer rather than spiral endlessly.
pub(crate) const THINK_BUDGET: usize = 512;

/// Build the sampler chain.
///
/// Technique rationale:
/// - **DRY** ("Don't Repeat Yourself"): penalises repeated *token sequences*
///   rather than individual tokens. This is the primary fix for the
///   "completely totally wholly fully …" cascade — those are all different
///   tokens, so standard `penalties` misses them, but DRY catches the
///   recurring suffix pattern they form.
/// - **min_p**: replaces top_p. When the model is confident (high-probability
///   next token) min_p becomes more selective, preventing runaway picks from
///   a collapsed distribution. When uncertain it relaxes, preserving diversity.
/// - **penalties**: kept as a backstop for single-token repetition that DRY's
///   `allowed_length = 2` intentionally ignores.
/// Sampler chain plus optional schema-driven logit mask.
///
/// History: the per-token loop used to call `LlamaSampler::sample`
/// directly, which runs the entire chain inside llama.cpp. Grammar
/// enforcement was provided either by `LlamaSampler::llguidance`
/// (silent fallthrough on every BYOM model) or `LlamaSampler::grammar`
/// (crashes the daemon process at `GGML_ASSERT(!stacks.empty())`,
/// `llama-grammar.cpp:940`, reproducible across Vulkan AND ROCm AND
/// single-slot daemon configurations). See
/// `memory/project_grammar_alpha_blocker.md`.
///
/// `ConstrainedSampler` replaces both: it owns a `LlamaSampler` chain
/// (without any grammar sampler), and optionally owns a
/// `JsonConstraint` that masks token logits in pure Rust before the
/// chain runs. No call into `llama-grammar.cpp` ever fires.

/// Resolve the jump-forward enable gate from an env-getter. Returns
/// `true` (jump-forward active) by default; only an explicit
/// `SOVEREIGN_JUMP_FWD_DISABLE` value of `"1"` or `"true"`
/// (case-insensitive) turns it off. Generic over the env-getter so
/// tests can pin behaviour without mutating process-global env (which
/// races against parallel test execution).
pub(crate) fn jump_fwd_enabled_from_env<F>(env_get: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    match env_get("SOVEREIGN_JUMP_FWD_DISABLE") {
        Some(v) => !(v == "1" || v.eq_ignore_ascii_case("true")),
        None => true,
    }
}

/// Same shape as `jump_fwd_enabled_from_env` but for the Tier 2 gate.
/// Independent of the master gate so an operator can keep Tier 1 on
/// while disabling Tier 2 (e.g. to A/B the marginal Tier 2 lift on a
/// running daemon).
pub(crate) fn jump_fwd_t2_enabled_from_env<F>(env_get: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    match env_get("SOVEREIGN_JUMP_FWD_T2_DISABLE") {
        Some(v) => !(v == "1" || v.eq_ignore_ascii_case("true")),
        None => true,
    }
}

#[cfg(test)]
mod chat_template_minijinja_tests {
    use super::apply_chat_template_minijinja;

    /// Smoke test on a Qwen3-shape `{% for %}` template — the
    /// built-in parser handles this too, so the minijinja path
    /// must produce equivalent output (not byte-equivalent — we
    /// only check the role markers + body land in the right slots).
    #[test]
    fn renders_simple_for_loop_template() {
        let template = "\
{%- for m in messages -%}\
<|im_start|>{{ m.role }}\n{{ m.content }}<|im_end|>\n\
{%- endfor -%}\
{%- if add_generation_prompt -%}<|im_start|>assistant\n{%- endif -%}";
        let out = apply_chat_template_minijinja(
            template,
            "you are a helpful assistant",
            "hello",
            false,
        )
        .expect("renders cleanly");
        assert!(out.contains("<|im_start|>system"));
        assert!(out.contains("you are a helpful assistant"));
        assert!(out.contains("<|im_start|>user"));
        assert!(out.contains("hello"));
        assert!(out.contains("<|im_start|>assistant"));
    }

    /// Pinned regression: templates that declare a Jinja `{% macro %}`
    /// were the exact construct llama.cpp's built-in parser
    /// rejected (Gemma 3/4, Qwen3.5-9B-vOP). minijinja must handle
    /// them.
    #[test]
    fn renders_template_with_macro() {
        let template = "\
{%- macro render(m) -%}\
[{{ m.role }}] {{ m.content }}\n\
{%- endmacro -%}\
{%- for m in messages -%}{{ render(m) }}{%- endfor -%}\
{%- if add_generation_prompt -%}[assistant]\n{%- endif -%}";
        let out = apply_chat_template_minijinja(template, "sys", "ask", false).unwrap();
        assert!(out.contains("[system] sys"));
        assert!(out.contains("[user] ask"));
        // Whitespace control (`{%- -%}`) strips the trailing `\n`
        // inside the {%- if -%} block — match real Jinja2 behaviour.
        assert!(out.trim_end().ends_with("[assistant]"));
    }

    /// Templates that use `{% set %}` blocks were the other
    /// construct the built-in parser rejected.
    #[test]
    fn renders_template_with_set_block() {
        let template = "\
{%- set bos = '<bos>' -%}\
{{ bos }}\
{%- for m in messages -%}<{{ m.role }}>{{ m.content }}</{{ m.role }}>{%- endfor -%}";
        let out = apply_chat_template_minijinja(template, "", "q", false).unwrap();
        assert!(out.starts_with("<bos>"));
        assert!(out.contains("<user>q</user>"));
    }

    /// `enable_thinking` is exposed to the template so Qwen3-family
    /// templates can branch on it. Verify the variable is wired.
    #[test]
    fn renders_template_branching_on_enable_thinking() {
        let template = "\
{%- if enable_thinking -%}/think{%- else -%}/no_think{%- endif -%}";
        assert_eq!(
            apply_chat_template_minijinja(template, "", "", true).unwrap(),
            "/think"
        );
        assert_eq!(
            apply_chat_template_minijinja(template, "", "", false).unwrap(),
            "/no_think"
        );
    }

    /// `raise_exception` is the canonical Hugging Face escape hatch
    /// for templates that want to halt on bad input. Forward it as a
    /// minijinja error so the caller falls through to the
    /// llama.cpp built-in tier rather than producing a malformed
    /// prompt.
    #[test]
    fn raise_exception_is_propagated_as_error() {
        let template = "{{ raise_exception('nope') }}";
        let err = apply_chat_template_minijinja(template, "", "", false).unwrap_err();
        assert!(format!("{err}").contains("nope"), "error chain: {err}");
    }

    /// Empty system message is dropped (template iterates only the
    /// user message). Pinned because some real templates assume
    /// every message has non-empty content.
    #[test]
    fn empty_system_dropped_from_messages() {
        let template = "\
{%- for m in messages -%}{{ m.role }}\n{%- endfor -%}";
        let out = apply_chat_template_minijinja(template, "", "q", false).unwrap();
        // Only user role present; no leading "system". `{%- endfor -%}`
        // strips the trailing newline, so the body is just "user".
        assert_eq!(out, "user");
    }
}

#[cfg(test)]
mod jump_fwd_env_tests {
    use super::*;

    #[test]
    fn jump_fwd_default_is_on() {
        assert!(jump_fwd_enabled_from_env(|_| None));
        assert!(jump_fwd_t2_enabled_from_env(|_| None));
    }

    #[test]
    fn jump_fwd_disabled_only_on_truthy() {
        // Explicit truthy values disable.
        assert!(!jump_fwd_enabled_from_env(|_| Some("1".to_string())));
        assert!(!jump_fwd_enabled_from_env(|_| Some("true".to_string())));
        assert!(!jump_fwd_enabled_from_env(|_| Some("TRUE".to_string())));
        // Other values keep jump-forward on — "0", garbage, empty all
        // mean "no, you're not disabling me".
        assert!(jump_fwd_enabled_from_env(|_| Some("0".to_string())));
        assert!(jump_fwd_enabled_from_env(|_| Some("false".to_string())));
        assert!(jump_fwd_enabled_from_env(|_| Some("".to_string())));
        assert!(jump_fwd_enabled_from_env(|_| Some("yes".to_string())));
    }

    #[test]
    fn jump_fwd_t2_disable_independent_of_master_gate() {
        // The two gates read different env vars; disabling one must
        // not affect the other.
        let only_t2_disabled =
            |k: &str| if k == "SOVEREIGN_JUMP_FWD_T2_DISABLE" {
                Some("1".to_string())
            } else {
                None
            };
        assert!(jump_fwd_enabled_from_env(only_t2_disabled));
        assert!(!jump_fwd_t2_enabled_from_env(only_t2_disabled));
    }
}

