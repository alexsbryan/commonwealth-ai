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

/// Env var that enables the legacy "full frontdoor" reshape. Retained
/// as a backwards-compat alias — `SOVEREIGN_FRONTDOOR=1` now maps to
/// the `Opencode` harness profile (the original reshape design).
/// `SOVEREIGN_HARNESS` overrides this when set.
pub fn is_enabled() -> bool {
    std::env::var("SOVEREIGN_FRONTDOOR")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Which agentic-harness contract this /v1/responses request is
/// speaking. Each profile picks a different set of passes — codex's
/// apply_patch-trained contract resists the full reshape we built
/// for opencode, while bare drivers (curl scripts, ATOS sandbox)
/// don't need any of it. See `passes_for` for the per-profile pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// codex CLI (`codex_cli_rs/*` UA). System prompt teaches
    /// apply_patch via `exec_command` heredoc; tool catalog excludes
    /// apply_patch as a function. Touching the prompt or catalog
    /// breaks the contract. Keep only the coherence baseline.
    Codex,
    /// opencode CLI (`opencode/*` UA) — the original frontdoor
    /// target. Verbose system prompt, missing apply_patch teaching,
    /// benefits from distillation + synthetic write_file injection +
    /// grammar lock.
    Opencode,
    /// Unknown harness — apply the conservative middle ground:
    /// coherence baseline + grammar lock when tool_choice="required",
    /// but DO NOT reshape the prompt or inject synthetic tools.
    Generic,
    /// Bare driver (curl smoke, ATOS sandbox loop) — nothing applies.
    Bare,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Generic => "generic",
            Harness::Bare => "bare",
        }
    }

    /// Whether the distiller should run on this harness's request.
    ///
    /// Codex DISABLED 2026-05-13 (post-Inv #17 v3 bench). Even with
    /// (1) JSON-Schema-coerced output shape and (2) mechanical
    /// `scrub_paths` post-processing, the distiller fabricated whole
    /// sub-narratives — observed a directive claiming "the agent has
    /// been stuck trying to remove macOS xattr" when the original
    /// task was just to implement a crate. Path-scrub bounds ONE
    /// leak class; semantic narrative fabrication is wider. The
    /// distiller using the same primary slot as the agent inherits
    /// the agent's failure modes. Bound the distiller to harnesses
    /// where it's been validated end-to-end (Opencode).
    pub fn runs_distiller(self) -> bool {
        matches!(self, Harness::Opencode)
    }

    /// Whether the tool catalog should be filtered to
    /// `CODEX_TOOL_KEEPLIST` + synthetic tools. Codex profile
    /// participates as of Inv #17 — drops 9/11 codex internal
    /// tools (`spawn_agent`, `update_plan`, `view_image`, …) that
    /// add noise without value for a local coding agent.
    pub fn runs_catalog_filter(self) -> bool {
        matches!(self, Harness::Codex | Harness::Opencode)
    }

    /// Whether synthetic `write_file*` / `read_file` should be
    /// injected into the catalog. Opencode-only; deliberately NOT
    /// extended to Codex even under Inv #17 because the 2026-05-13
    /// `write_stage_minimal` bench proved the model emits perfect
    /// `apply_patch` heredocs via the natural `exec_command` tool
    /// (10/10 turns at greedy content T). Synthetic file tools
    /// would fight codex 0.130's apply_patch training prior with
    /// no observed benefit.
    pub fn runs_synthetic_tools(self) -> bool {
        matches!(self, Harness::Opencode)
    }

    /// Whether `tool_choice` should be promoted to `"required"` to
    /// engage the inference adapter's tool-envelope grammar
    /// (`{"name": string, "arguments": object}`).
    ///
    /// Codex is included as of 2026-05-13 (Investment 3): the v15
    /// "passthrough sans grammar" experiment in §4.8b is unwound.
    /// Empirically the 35B-A3B primary slot can NOT keep the JSON
    /// envelope shape without enforcement — codex smoke 2026-05-13
    /// observed the model emitting `<tool_call>{"name":"X","cmd":"…"}`
    /// (args fields flattened to root) and codex CLI rejecting
    /// `missing field \`cmd\`` on every turn. Envelope grammar is
    /// orthogonal to codex's apply_patch teaching: `arguments.cmd`
    /// remains a free string, so heredoc bodies pass through as
    /// JSON-string-encoded shell commands (one escape layer, not the
    /// triple-nest §4.8b feared).
    pub fn runs_grammar_lock(self) -> bool {
        matches!(self, Harness::Codex | Harness::Opencode | Harness::Generic)
    }

    /// Whether history compression / telemetry baseline should run.
    /// Every harness except `Bare` benefits.
    pub fn runs_coherence_baseline(self) -> bool {
        !matches!(self, Harness::Bare)
    }
}

/// Resolve the active harness from (in priority order):
/// 1. `SOVEREIGN_HARNESS` env var (explicit override)
/// 2. `User-Agent` header
/// 3. Legacy `SOVEREIGN_FRONTDOOR=1` → Opencode
/// 4. Default → Generic
pub fn detect_harness(headers: &HeaderMap) -> Harness {
    if let Ok(forced) = std::env::var("SOVEREIGN_HARNESS") {
        match forced.to_ascii_lowercase().as_str() {
            "codex" => return Harness::Codex,
            "opencode" => return Harness::Opencode,
            "bare" => return Harness::Bare,
            "generic" => return Harness::Generic,
            _ => {} // fall through to UA / legacy detection
        }
    }
    if let Some(ua) = headers.get("user-agent").and_then(|v| v.to_str().ok()) {
        let ua_lower = ua.to_ascii_lowercase();
        if ua_lower.contains("codex_cli") || ua_lower.contains("codex-cli") {
            return Harness::Codex;
        }
        if ua_lower.contains("opencode") {
            return Harness::Opencode;
        }
    }
    if is_enabled() {
        return Harness::Opencode;
    }
    Harness::Generic
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

// ─── Heredoc body diagnostics (Codex profile post-mortem) ───────────
//
// The Codex profile passes `exec_command(apply_patch <<'EOF' ...)`
// heredocs through untouched. The JSON-args layer parses fine on
// every observed turn (166/166 records as of 2026-05-13), but the
// model's escape coherence still collapses when the *body* of the
// heredoc embeds Rust strings containing JSON — three escape layers
// nested. The function-call args bytes / args_parsed_ok flags don't
// distinguish "the args JSON is malformed" from "the args JSON is
// fine but the Rust source inside the heredoc body has literal
// `\"` sequences that break compilation".
//
// `extract_heredoc_diagnostics` parses the `cmd` field of an
// `exec_command` call, isolates the heredoc body, and reports
// structural markers plus escape-coherence smells. Per-fc result
// rides on the terminal telemetry record so a `jq` post-mortem can
// answer "which turn's body went sideways and why" without holding
// the full args payload in the log.

/// Per-function-call heredoc-body diagnostics for `exec_command`
/// invocations that wrap an `apply_patch <<DELIM ... DELIM` heredoc.
/// `None` when the call is not a heredoc-write shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HeredocDiagnostics {
    /// Heredoc delimiter token as written, without surrounding
    /// quotes (`EOF`, `PATCH`, …).
    pub delimiter: String,
    /// Whether the opening `<<` used a single- or double-quoted
    /// delimiter — when true, the shell suppresses ALL backslash
    /// interpretation in the body, which is the contract codex's
    /// apply_patch teaching relies on.
    pub quoted_delimiter: bool,
    /// Bytes of the heredoc body (excluding the opening line and
    /// the closing delimiter line).
    pub body_bytes: usize,
    /// Whether the body contains `*** Begin Patch`.
    pub begin_patch: bool,
    /// Whether the body contains `*** End Patch`.
    pub end_patch: bool,
    /// Count of `*** Add File:` markers in the body.
    pub add_files: usize,
    /// Count of `*** Update File:` markers in the body.
    pub update_files: usize,
    /// Count of `*** Delete File:` markers in the body.
    pub delete_files: usize,
    /// Count of `\"` sequences inside the body. Inside a quoted
    /// heredoc these become literal `\"` bytes in the written file
    /// — almost always a triple-nested escape coherence collapse
    /// (the model leaked JSON-string escaping into Rust source).
    pub escape_quote_count: usize,
    /// Count of `\\` sequences inside the body. Same rationale.
    pub escape_backslash_count: usize,
    /// Whether the body's closing delimiter line was found. False
    /// means the heredoc is unterminated within the captured args
    /// — typical of token-truncation on the inner model.
    pub closed: bool,
}

/// Parse the heredoc body out of an `exec_command` JSON args string
/// and return structural + escape-coherence diagnostics. Returns
/// `None` when the args don't decode, the `cmd` field is missing,
/// or the command shape isn't `apply_patch <<...`.
pub fn extract_heredoc_diagnostics(arguments_json: &str) -> Option<HeredocDiagnostics> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    let cmd = v.get("cmd").and_then(|x| x.as_str())?;
    let apply_at = cmd.find("apply_patch")?;
    let after_apply = &cmd[apply_at + "apply_patch".len()..];
    let lt = after_apply.find("<<")?;
    let mut tail = &after_apply[lt + 2..];
    // Bash allows `<<-` to strip leading tabs. Skip the dash.
    if let Some(stripped) = tail.strip_prefix('-') {
        tail = stripped;
    }
    tail = tail.trim_start_matches(' ');
    let (delim, quoted, body_start) = parse_heredoc_delimiter(tail)?;
    let body_region = &tail[body_start..];
    // Body starts after the newline that ends the opening line.
    let body_region = body_region.strip_prefix('\n').unwrap_or(body_region);
    let close_marker = format!("\n{}", delim);
    let (body, closed) = match body_region.find(&close_marker) {
        Some(i) => (&body_region[..i], true),
        None => (body_region, false),
    };
    Some(HeredocDiagnostics {
        delimiter: delim.to_string(),
        quoted_delimiter: quoted,
        body_bytes: body.len(),
        begin_patch: body.contains("*** Begin Patch"),
        end_patch: body.contains("*** End Patch"),
        add_files: count_substring(body, "*** Add File:"),
        update_files: count_substring(body, "*** Update File:"),
        delete_files: count_substring(body, "*** Delete File:"),
        escape_quote_count: count_substring(body, "\\\""),
        escape_backslash_count: count_substring(body, "\\\\"),
        closed,
    })
}

fn parse_heredoc_delimiter(tail: &str) -> Option<(&str, bool, usize)> {
    let bytes = tail.as_bytes();
    let first = *bytes.first()?;
    let (quote, start) = if first == b'\'' || first == b'"' {
        (Some(first), 1)
    } else {
        (None, 0)
    };
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        match quote {
            Some(q) if b == q => break,
            None if b.is_ascii_whitespace() || b == b';' || b == b'&' || b == b'|' => break,
            _ => end += 1,
        }
    }
    if end == start {
        return None;
    }
    let delim = &tail[start..end];
    let after = if quote.is_some() { end + 1 } else { end };
    Some((delim, quote.is_some(), after))
}

fn count_substring(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

// ─── Harness brief — operator-supplied narrow-framing directive ─────
//
// `apply_brief_from_env_path` is a generic prepend-from-file pass:
// the env var named by the caller MAY point at a UTF-8 file whose
// content is prepended to `req.instructions`. The brief content
// lives in the operator's filesystem, not in this binary — that
// way a contract drift in any upstream harness (codex 0.130's
// apply_patch teaching today, codex 0.131's whatever tomorrow,
// opencode's tool prior, the next harness we adapt) is fixable by
// editing one file, no rebuild required.
//
// Background: the 2026-05-13 v15 smoke (§4.8b in the system map)
// closed the codex harness-integration question — the Codex profile
// landed 7 heredoc writes / 1076 bytes of real code. The remaining
// gap was the model's escape coherence on triple-nested
// heredoc-Rust-JSON bodies. The original §4.8b note flagged
// "narrower task framing" as the follow-up lever; this pass is the
// generalised wiring for that lever. Operators A/B with the
// heredoc-body telemetry's `escape_quote_count` as the witness.
//
// Wired in `routes_responses::responses` per harness profile:
//   - Codex profile reads `SOVEREIGN_CODEX_BRIEF` (path to brief)
// Additional harness profiles can call `apply_brief_from_env_path`
// with their own env var name as the contract surfaces grow.

/// Prepend the contents of the file at the path in environment
/// variable `env_var` onto `req.instructions`. No-op when:
///   - the env var is unset or empty
///   - the file does not exist or cannot be read
///   - the file's content is whitespace-only
///   - the brief is already at the head of `instructions`
///     (idempotent across repeated calls within one session)
///
/// The brief text itself lives in the operator's filesystem so it
/// can evolve with upstream harness contracts without a rebuild.
/// File reads are best-effort: failures log a `warn!` and pass the
/// request through unchanged. The path is re-read every call so
/// operators can edit the file mid-session and see the change on
/// the next turn — caching would defeat that ergonomic.
pub fn apply_brief_from_env_path(req: &mut ResponsesRequest, env_var: &str) {
    let path = match std::env::var(env_var) {
        Ok(p) if !p.trim().is_empty() => p,
        _ => return,
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                env_var = %env_var,
                path = %path,
                error = %e,
                "frontdoor: brief file unreadable; passing through"
            );
            return;
        }
    };
    let brief = raw.trim_end_matches('\n').to_string();
    if brief.trim().is_empty() {
        debug!(
            env_var = %env_var,
            path = %path,
            "frontdoor: brief file is whitespace-only; no-op"
        );
        return;
    }
    let new = match req.instructions.as_deref() {
        Some(prev) if prev.starts_with(&brief) => return,
        Some(prev) if prev.trim().is_empty() => brief.clone(),
        Some(prev) => format!("{}\n\n{}", brief, prev),
        None => brief.clone(),
    };
    info!(
        env_var = %env_var,
        path = %path,
        added_bytes = brief.len(),
        "frontdoor: brief prepended to instructions"
    );
    req.instructions = Some(new);
}

/// Codex-profile convenience wrapper. Reads from
/// `SOVEREIGN_CODEX_BRIEF` (path to a UTF-8 file) and prepends
/// the file content to `req.instructions`. Identical contract to
/// `apply_brief_from_env_path`; provided so the call site in
/// `responses()` reads intentionally without a magic-string env
/// name leaking into the route handler.
pub fn apply_codex_brief(req: &mut ResponsesRequest) {
    apply_brief_from_env_path(req, "SOVEREIGN_CODEX_BRIEF");
}

/// Apply ALL frontdoor passes to `req` in-place. Reshapes codex's
/// verbose harness contract into the local-model-native dialect.
/// Gated by `is_enabled()` — see module docs for the full rationale.
///
/// As of 2026-05-13 (post-v14 review): the full pass is known to
/// FIGHT codex's training contract (apply_patch teaching, free-text
/// finalization). Use `apply_baseline` for non-frontdoor sessions
/// to keep the coherence aids (history compression) without the
/// re-shaping. Behind the gate the full pass is still useful for
/// non-codex harnesses (opencode, bare-sandbox-style drivers) where
/// the verbose shaping is what's required.
pub async fn apply(
    state: &AppState,
    headers: &HeaderMap,
    req: &mut ResponsesRequest,
    harness: Harness,
) {
    // Half 1: distiller (instructions rewriting). Cached by hash of
    // the original `instructions` + first user message.
    apply_distiller(state, headers, req, harness).await;
    // Half 2: history compression — see apply_baseline for details.
    apply_history_compression(state, headers, req).await;
    // Half 3: catalog filter is applied during request translation,
    // not here — `routes_responses::translate_request` consults
    // `tool_keeplist_contains` directly. Centralising the policy in
    // one place keeps the translation-time path tight and lets tests
    // exercise the filter without spinning up an AppState.
}

/// Apply ONLY the coherence-preserving passes. Always safe to call:
/// no harness-shape assumptions, no prompt surgery, no tool catalog
/// changes. Today this is just history compression; pure observability
/// (telemetry) lives in the route handler, not here.
///
/// Rationale: the bigger frontdoor passes (distiller, catalog filter,
/// synthetic tools, grammar lock) interfere with codex's training
/// contract. History compression is orthogonal — it prevents MoE
/// context-drift on any agentic harness regardless of contract shape.
pub async fn apply_baseline(
    state: &AppState,
    headers: &HeaderMap,
    req: &mut ResponsesRequest,
) {
    apply_history_compression(state, headers, req).await;
}

/// Distilled directive produced by the fast-slot pass. Wire shape is
/// a grammar-coerced JSON object (3 string fields, schema-enforced
/// via `JsonConstraint`).
///
/// **Path scrubbing.** The distiller model reliably hallucinates
/// absolute paths even with explicit no-invent rules in its system
/// prompt — observed 2026-05-13 with three typos in a single
/// directive (`tos-experiment-oicp-types`, `alexsbryan.dev`,
/// `oicp_core`). Hope-prompting doesn't work; the model has the
/// same failure modes as the agent that consumes its output. So
/// the directive's string fields go through `scrub_paths` before
/// render, which mechanically replaces any absolute path with the
/// `<path>` placeholder. The agent's directive is structurally
/// path-free; the agent discovers paths via the tools it already
/// has.
#[derive(Debug, Clone, serde::Deserialize)]
struct DistilledDirective {
    #[serde(default)]
    task: String,
    #[serde(default)]
    constraints: String,
    #[serde(default)]
    done_when: String,
}

/// Mechanically replace any absolute path in `s` with `<path>`.
///
/// Matches Unix-style absolute paths: `/` followed by one or more
/// path components (each `/`-separated, component chars are
/// alphanumeric / `_` / `-` / `.`). Multi-character placeholders
/// like `<path>` are picked deliberately over erasure so the
/// resulting prose still scans as English (`under <path>` reads
/// cleanly; bare deletions can produce ungrammatical fragments
/// that confuse the consumer agent).
///
/// This is intentionally conservative: it strips ALL absolute
/// paths, even ones that happen to be correct. Correct paths
/// in the directive would still be model-fabricated (the
/// distiller is summarizing a prompt; it has no ground truth);
/// trusting any of them risks the typo class. The agent's tools
/// (`ls`, `find`, `pwd`, …) are the ground-truth source.
pub fn scrub_paths(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() && is_path_char(bytes[i + 1]) {
            // Walk a path: alternating components separated by `/`.
            let start = i;
            let mut j = i + 1;
            let mut components = 0usize;
            loop {
                let comp_start = j;
                while j < bytes.len() && is_path_char(bytes[j]) {
                    j += 1;
                }
                if j == comp_start {
                    break;
                }
                components += 1;
                if j < bytes.len() && bytes[j] == b'/' && j + 1 < bytes.len() && is_path_char(bytes[j + 1]) {
                    j += 1;
                    continue;
                }
                break;
            }
            // Require at least 2 components so we don't swallow
            // single-slash uses like `/dev/null`-quotes or `cmd/arg`
            // by accident. Two+ components = real path-like shape.
            if components >= 2 {
                out.push_str("<path>");
                i = j;
                continue;
            }
            // Not enough components — copy the leading `/` and
            // continue scanning from i+1.
            out.push('/');
            i = start + 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_path_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

/// Canonicalize an `apply_patch <<HEREDOC ... HEREDOC` shell command
/// emitted by a model.
///
/// Why: under load (long contexts, ambiguous user-message echoes of
/// the protocol), models sometimes emit malformed heredocs:
///   * `*** Begin Patch ***` (extra trailing `***`)
///   * `Add File: <path>` (missing leading `*** `)
///   * `*** End Patch EOF` (closing tag inlined with last marker)
/// The protocol is well-defined and deterministic; we don't need an
/// LLM to fix this. Parse the structural intent, re-emit in canonical
/// form. Idempotent on already-canonical input.
///
/// Returns `Some(canonical)` when the input looks like an apply_patch
/// heredoc and we successfully extract at least one file operation.
/// Returns `None` for non-apply_patch commands (passthrough — caller
/// must leave the original `cmd` untouched).
pub fn canonicalize_apply_patch_heredoc(cmd: &str) -> Option<String> {
    let trimmed_start = cmd.trim_start();
    let leading_ws_len = cmd.len() - trimmed_start.len();
    let leading = &cmd[..leading_ws_len];

    // Opener: `apply_patch <<TAG` with optional quoting around TAG.
    // Capture tag chars so we can use it as the closer marker.
    let after_apply = trimmed_start.strip_prefix("apply_patch")?;
    let after_lt = after_apply.trim_start().strip_prefix("<<")?;
    let (tag, after_tag) = parse_heredoc_tag(after_lt.trim_start())?;
    if tag.is_empty() {
        return None;
    }

    // Body: everything up to and including the terminating `End Patch`
    // marker (canonical) — we treat `End Patch` as the structural end,
    // not the literal `\nTAG\n` closer, since malformed emissions often
    // inline the closer (`*** End Patch EOF`).
    //
    // Fallback: if `End Patch` is entirely missing (gym 008 / real
    // codex smoke 2026-05-13: model emits the heredoc body then just
    // `EOF` with no End Patch marker), treat the heredoc closer
    // (`\nTAG\n` or `\nTAG` at end-of-string) as the body boundary.
    // Both repairs land in the canonical re-emission.
    let body_input = after_tag.trim_start_matches(|c: char| c == '\n' || c == '\r');
    let pre_end: &str = match find_end_patch_marker(body_input) {
        Some(end_idx) => &body_input[..end_idx],
        None => {
            // Strip the trailing heredoc closer if present so it
            // doesn't end up in the body. Match `\n<tag>` followed
            // by end-of-string or whitespace.
            let mut body = body_input;
            let needle_with_nl = format!("\n{tag}");
            if let Some(idx) = body.rfind(&needle_with_nl) {
                let tail = &body[idx + needle_with_nl.len()..];
                if tail.trim().is_empty() {
                    body = &body[..idx];
                }
            }
            body
        }
    };

    let canonical_inner = canonicalize_patch_body_lines(pre_end)?;
    let mut out = String::new();
    out.push_str(leading);
    out.push_str("apply_patch <<'");
    out.push_str(&tag);
    out.push_str("'\n*** Begin Patch\n");
    out.push_str(&canonical_inner);
    out.push_str("*** End Patch\n");
    out.push_str(&tag);
    out.push('\n');
    Some(out)
}

/// Parse the heredoc tag after `<<`. Accepts `EOF`, `'EOF'`, `"EOF"`.
/// Returns the bare tag string + the slice after the tag (incl. its
/// closing quote if any).
fn parse_heredoc_tag(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (quote, rest_start) = match bytes[0] {
        b'\'' | b'"' => (Some(bytes[0]), 1),
        _ => (None, 0),
    };
    let mut i = rest_start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == rest_start {
        return None;
    }
    let tag = &s[rest_start..i];
    let after = if let Some(q) = quote {
        if bytes.get(i) != Some(&q) {
            return None;
        }
        &s[i + 1..]
    } else {
        &s[i..]
    };
    Some((tag.to_string(), after))
}

/// Locate the first `End Patch` marker in `s`. Robust to leading
/// `*** ` and trailing tokens on the same line (e.g. `*** End Patch EOF`).
/// Returns the byte offset where the marker line starts.
fn find_end_patch_marker(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let needle = b"End Patch";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Found the substring. Walk backwards to the start of the
            // line — we want the marker to begin at line-start
            // (optionally after `*** ` prefix).
            let mut line_start = i;
            while line_start > 0 && bytes[line_start - 1] != b'\n' {
                line_start -= 1;
            }
            return Some(line_start);
        }
        i += 1;
    }
    None
}

/// Walk lines, identify each one's structural role (Begin Patch /
/// Add File / Update File / Delete File / Move to / body line),
/// re-emit in canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchSection {
    /// Outside any file operation.
    None,
    /// Inside an `Add File:` body — every line is an addition. Lines
    /// lacking a `+` prefix are repaired to have one.
    AddFile,
    /// Inside an `Update File:` body — lines may be `+`, `-`, ` `, or
    /// `@@`. Unprefixed lines default to `+` (an addition) since
    /// that's the most common malformation pattern from observed
    /// smokes (gym 008, 2026-05-13).
    UpdateFile,
}

fn canonicalize_patch_body_lines(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut saw_op = false;
    let mut section = PatchSection::None;
    for raw_line in body.lines() {
        let cleaned = strip_triple_star(raw_line);
        if cleaned.eq_ignore_ascii_case("Begin Patch") {
            continue; // Begin Patch emitted by caller
        }
        if let Some(path) = strip_op_prefix(&cleaned, "Add File:") {
            out.push_str("*** Add File: ");
            out.push_str(path.trim());
            out.push('\n');
            saw_op = true;
            section = PatchSection::AddFile;
            continue;
        }
        if let Some(path) = strip_op_prefix(&cleaned, "Update File:") {
            out.push_str("*** Update File: ");
            out.push_str(path.trim());
            out.push('\n');
            saw_op = true;
            section = PatchSection::UpdateFile;
            continue;
        }
        if let Some(path) = strip_op_prefix(&cleaned, "Delete File:") {
            out.push_str("*** Delete File: ");
            out.push_str(path.trim());
            out.push('\n');
            saw_op = true;
            section = PatchSection::None;
            continue;
        }
        if let Some(target) = strip_op_prefix(&cleaned, "Move to:") {
            out.push_str("*** Move to: ");
            out.push_str(target.trim());
            out.push('\n');
            continue;
        }
        // Body line. Preserve verbatim when it already has a valid
        // prefix (`+`, `-`, ` `, `@@`). Otherwise — and this is the
        // gym 008 repair — synthesise the prefix based on the
        // current section. Models trained on the apply_patch
        // protocol sometimes lose the `+` discipline mid-body
        // (especially for TOML / config files whose syntax visually
        // resembles a hunk header).
        let line_trimmed_end = raw_line.trim_end_matches(['\r', '\n']);
        if line_trimmed_end.trim().is_empty() && cleaned.is_empty() {
            continue;
        }
        // Prefix rules differ by section:
        //   AddFile  — every body line is an addition. The ONLY
        //              acceptable line-start is `+`; a leading space
        //              is content indentation, not a hunk prefix.
        //              Codex's apply_patch parser rejects `<space>foo`
        //              inside an Add File section ("not a valid hunk
        //              header"). Repair: always prepend `+`.
        //   UpdateFile — `+/-/space/@@` are all legal (context lines,
        //              additions, deletions, hunk headers).
        let first_char = line_trimmed_end.chars().next();
        let needs_prefix = match section {
            PatchSection::AddFile => !matches!(first_char, Some('+')),
            PatchSection::UpdateFile => match first_char {
                Some('+') | Some('-') | Some(' ') => false,
                Some('@') if line_trimmed_end.starts_with("@@") => false,
                _ => true,
            },
            PatchSection::None => false,
        };
        if needs_prefix && section != PatchSection::None {
            out.push('+');
        }
        out.push_str(line_trimmed_end);
        out.push('\n');
    }
    if !saw_op {
        return None;
    }
    Some(repair_per_file_content(&out))
}

/// Post-process the canonicalized patch body: walk file sections,
/// apply per-file-type content repairs. Currently handles `.toml`
/// files (Cargo.toml shape).
///
/// Why mechanical instead of better prompting: per the user's
/// philosophy (2026-05-13), the model will never be 100% on
/// formatting. Detect well-known config-file malformations and
/// repair them post-emission. Same pattern as the heredoc structural
/// canonicalizer, applied at the file-content level.
fn repair_per_file_content(canonical_body: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut current_path: Option<String> = None;
    let mut buffer: Vec<String> = Vec::new();
    for line in canonical_body.lines() {
        // Section boundary: `*** Add File:` / `*** Update File:` /
        // `*** Delete File:` / `*** End Patch` / `*** Begin Patch`.
        if line.starts_with("*** Add File:") {
            // Flush previous section, then enter new one.
            flush_section(&mut out, current_path.as_deref(), &buffer);
            buffer.clear();
            let path = line.trim_start_matches("*** Add File:").trim().to_string();
            current_path = Some(path);
            out.push(line.to_string());
            continue;
        }
        if line.starts_with("*** Update File:")
            || line.starts_with("*** Delete File:")
            || line.starts_with("*** Move to:")
            || line.starts_with("*** End Patch")
            || line.starts_with("*** Begin Patch")
        {
            flush_section(&mut out, current_path.as_deref(), &buffer);
            buffer.clear();
            current_path = None;
            out.push(line.to_string());
            continue;
        }
        // Inside a section — buffer until next boundary.
        if current_path.is_some() {
            buffer.push(line.to_string());
        } else {
            out.push(line.to_string());
        }
    }
    // Trailing section (shouldn't happen if End Patch is present,
    // but cover the path for safety).
    if !buffer.is_empty() {
        flush_section(&mut out, current_path.as_deref(), &buffer);
    }
    let mut joined = out.join("\n");
    if canonical_body.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn flush_section(out: &mut Vec<String>, path: Option<&str>, buffer: &[String]) {
    let Some(path) = path else {
        // No active section (or it was non-AddFile) — emit buffer verbatim.
        out.extend(buffer.iter().cloned());
        return;
    };
    if path.ends_with(".toml") || path.ends_with("/Cargo.toml") {
        let repaired = repair_toml_body(buffer);
        out.extend(repaired);
    } else {
        out.extend(buffer.iter().cloned());
    }
}

/// Repair a TOML body that was emitted in JSON-ish shape.
///
/// Three passes:
/// 1. Strip wrapper `+{` / `+}` lines that enclosed the whole body.
/// 2. Detect multi-line inline-table opens (`+    dependencies = {`)
///    and convert to TOML section headers (`+[dependencies]`). TOML
///    inline tables MUST be single-line; multi-line inline tables
///    are invalid syntax. Drop the matching close `+}` after.
/// 3. Strip trailing commas on key=value lines (TOML doesn't use
///    trailing commas — JSON habit slipping through).
/// 4. Prepend `[package]` if the body starts with key=value lines
///    without any preceding section header.
fn repair_toml_body(buffer: &[String]) -> Vec<String> {
    // Pass 1 — strip outer wrapper braces.
    let kept: Vec<String> = buffer
        .iter()
        .filter(|line| {
            let stripped = line.trim_start_matches('+').trim();
            stripped != "{" && stripped != "}"
        })
        .cloned()
        .collect();
    // Pass 2 — multi-line inline-table → section header. A line that
    // matches `+<indent><name> = {` with NO closing `}` on the same
    // line is a malformed multi-line inline-table open. Rewrite to
    // `+[<name>]` and remember to drop the corresponding close.
    let mut pass2: Vec<String> = Vec::new();
    let mut depth: i32 = 0;
    for line in &kept {
        let inner = line.strip_prefix('+').unwrap_or(line);
        let trimmed = inner.trim();
        if depth > 0 {
            // Inside a malformed multi-line table — emit lines as-is
            // until the matching close brace.
            let open_count = inner.matches('{').count() as i32;
            let close_count = inner.matches('}').count() as i32;
            depth += open_count - close_count;
            if depth <= 0 {
                depth = 0;
                if trimmed == "}" {
                    continue; // drop standalone close
                }
                // Drop trailing `}` from a real content line, if any.
                let cleaned_line = if trimmed.ends_with('}') {
                    let without_close = inner.trim_end().trim_end_matches('}').trim_end();
                    format!("+{without_close}")
                } else {
                    line.clone()
                };
                pass2.push(cleaned_line);
                continue;
            }
            pass2.push(line.clone());
            continue;
        }
        // Look for `<name> = {` with no closing `}` on same line.
        if let Some(name) = parse_inline_table_open(trimmed) {
            pass2.push(format!("+[{}]", name));
            depth = 1;
            continue;
        }
        pass2.push(line.clone());
    }
    // Pass 3 — strip trailing commas on key=value lines.
    let stripped_commas: Vec<String> = pass2
        .iter()
        .map(|line| {
            let inner = line.strip_prefix('+').unwrap_or(line);
            let inner_trim_end = inner.trim_end();
            if let Some(rest) = inner_trim_end.strip_suffix(',') {
                if rest.contains('=') && !rest.trim_start().starts_with('[') {
                    return format!("+{rest}");
                }
            }
            line.to_string()
        })
        .collect();
    // Pass 4 — ensure a `[package]` header before the first key=value.
    let mut out: Vec<String> = Vec::new();
    let mut header_seen = false;
    let mut header_injected = false;
    for line in stripped_commas {
        let inner = line.strip_prefix('+').unwrap_or(&line).trim_start();
        if inner.starts_with('[') {
            header_seen = true;
        }
        let is_kv =
            !inner.is_empty() && inner.contains('=') && !inner.starts_with('[');
        if !header_seen && !header_injected && is_kv {
            out.push("+[package]".to_string());
            header_injected = true;
        }
        out.push(line);
    }
    out
}

/// If `s` looks like `<name> = {` with no matching `}` on the same
/// line, return `<name>`. Used to detect malformed multi-line inline-
/// table opens that should be section headers.
fn parse_inline_table_open(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let (name_part, rest) = trimmed.split_once('=')?;
    let name = name_part.trim();
    if name.is_empty() {
        return None;
    }
    // Name must look like a TOML key — alphanum + `_-.`.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }
    let rest = rest.trim();
    if rest != "{" {
        return None; // would be a closed inline table if `{ ... }` on same line
    }
    Some(name.to_string())
}

/// Strip leading and trailing `***` runs (with adjacent whitespace).
/// Returns the inner content.
fn strip_triple_star(line: &str) -> String {
    let mut s = line.trim();
    while let Some(rest) = s.strip_prefix("***") {
        s = rest.trim_start();
    }
    while let Some(rest) = s.strip_suffix("***") {
        s = rest.trim_end();
    }
    s.to_string()
}

/// Case-insensitive prefix match; returns the suffix after the prefix.
fn strip_op_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() {
        return None;
    }
    let head = &s[..prefix.len()];
    if head.eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

const FAILURE_NUDGE_PREFIX: &str = "[failure-recovery note from runtime]";
const READ_ATTRACTOR_NUDGE_PREFIX: &str = "[read-attractor nudge from runtime]";

/// Returns true if any of the last few messages is a runtime nudge
/// from `apply_failure_nudge_chat`, `apply_anti_repetition_chat`, or
/// `apply_read_attractor_nudge_chat`. Used by every nudge to gate
/// re-injection across both user-role and system-role variants.
fn has_recent_runtime_nudge(messages: &[crate::openai_types::ChatMessage]) -> bool {
    for msg in messages.iter().rev() {
        if !matches!(msg.role.as_str(), "user" | "system") {
            continue;
        }
        if msg.content.starts_with(REPETITION_NOTE_PREFIX)
            || msg.content.starts_with(FAILURE_NUDGE_PREFIX)
            || msg.content.starts_with(READ_ATTRACTOR_NUDGE_PREFIX)
        {
            return true;
        }
        // We only check until we hit the first user message (the
        // "current turn" boundary); going further back is a perf
        // concern on long histories.
        if msg.role == "user" {
            return false;
        }
    }
    false
}

/// Mode classification of an `exec_command` shell call. Used by
/// `apply_read_attractor_nudge_chat` to decide whether the model is
/// stuck in an exploration-only mode and needs an explicit push
/// toward a write/build action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmdMode {
    /// Pure read: `cat`, `ls`, `rg`, `find`, `head`, `tail`, `grep`,
    /// `file`, `stat`, `less`, `more`, `pwd`, `wc`, `du`. These don't
    /// mutate state and produce information for the model to attend to.
    Read,
    /// Anything else — `apply_patch`, `cargo`, `git`, `mv`, `rm`,
    /// `mkdir`, `python`, `node`, `tsc`, custom scripts. Treated as
    /// "made progress".
    Action,
}

/// Coarse classification of a shell command by its head token.
/// Skips leading `cd <path>` stanzas and `VAR=value` env assignments
/// across all `; | & &&` segments so `cd /foo && cat bar.md`
/// classifies as a read of `cat`, not an action on `cd`.
fn classify_cmd(cmd: &str) -> CmdMode {
    let mut head: String = String::new();
    for token in cmd
        .split(|c: char| c == ';' || c == '|' || c == '&' || c == '\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let words: Vec<&str> = token.split_whitespace().collect();
        let mut i = 0;
        while i < words.len() {
            let w = words[i];
            // Skip `cd <path>` (consumes 2 words).
            if w == "cd" && i + 1 < words.len() {
                i += 2;
                continue;
            }
            // Skip `VAR=value` env assignments (single word).
            if let Some((var, _)) = w.split_once('=') {
                if !var.is_empty()
                    && var
                        .chars()
                        .next()
                        .map_or(false, |c| c.is_ascii_uppercase() || c == '_')
                {
                    i += 1;
                    continue;
                }
            }
            head = w.trim_start_matches('!').to_string();
            break;
        }
        if !head.is_empty() {
            break;
        }
    }
    match head.as_str() {
        "cat" | "ls" | "rg" | "find" | "head" | "tail" | "grep" | "file" | "stat"
        | "less" | "more" | "pwd" | "wc" | "du" | "awk" | "sed" | "tree" | "which" => {
            CmdMode::Read
        }
        _ => CmdMode::Action,
    }
}

/// Detects when the model is stuck in exploration-only mode (≥3
/// read-shaped exec_command calls in history, ZERO action calls)
/// and appends a synthetic user message naming `apply_patch` as the
/// expected next emission shape.
///
/// Why a separate mechanism from anti-rep / failure-nudge: this
/// fires when reads SUCCEED (exit 0) but the model keeps reading
/// instead of pivoting to action. Gym fixture 006: 4 successful
/// reads in history, explicit user "stop reading, write src/lib.rs",
/// model still emits `ls -la ./oicp-core/src/`. Anti-rep doesn't
/// catch (distinct read commands). Failure-nudge doesn't catch
/// (no failed call at tail).
///
/// Idempotent: bails when the last user message is already a
/// runtime nudge.
pub fn apply_read_attractor_nudge_chat(
    req: &mut crate::openai_types::ChatCompletionRequest,
) {
    if req.messages.is_empty() {
        return;
    }
    let messages = &mut req.messages;
    // Idempotency: skip if any of our runtime nudges already sits in
    // the tail. Read-attractor uses system-role; the others are
    // user-role. Walk back through both kinds.
    if has_recent_runtime_nudge(messages) {
        return;
    }

    let mut reads = 0usize;
    let mut actions = 0usize;
    for msg in messages.iter() {
        if msg.role != "assistant" {
            continue;
        }
        // Prefer proper tool_calls; fall back to envelope-shadow
        // content. Same call shape duality as anti-rep / failure-nudge.
        let cmd: Option<String> = if let Some(tcs) = &msg.tool_calls {
            tcs.first()
                .and_then(|tc| {
                    if tc.function.name == "exec_command" {
                        Some(tc.function.arguments.as_str())
                    } else {
                        None
                    }
                })
                .and_then(|args| extract_exec_command_cmd(args))
        } else {
            let trimmed = msg.content.trim_start();
            if trimmed.starts_with('{') && trimmed.contains("\"name\"") {
                serde_json::from_str::<serde_json::Value>(&msg.content)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/arguments/cmd")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                    })
            } else {
                None
            }
        };
        let Some(cmd_str) = cmd else { continue };
        match classify_cmd(&cmd_str) {
            CmdMode::Read => reads += 1,
            CmdMode::Action => actions += 1,
        }
    }
    if reads < 3 || actions > 0 {
        return;
    }
    let note = format!(
        "{} The agent has run {} read-only shell commands in this session and \
         ZERO write/build/test commands. The next emission from the model MUST \
         be a write action via `apply_patch <<'EOF'\\n*** Begin Patch\\n\
         *** Add File: <path>\\n+<content>\\n*** End Patch\\nEOF`. Emit \
         exactly ONE exec_command tool_call now whose `cmd` begins with \
         `apply_patch <<'EOF'`. Do NOT emit cat/ls/rg/find/head/tail/grep. \
         Do NOT explain. The next tool_call MUST be apply_patch.",
        READ_ATTRACTOR_NUDGE_PREFIX, reads
    );
    info!(
        reads,
        actions,
        "frontdoor: read-attractor nudge injected (chat)"
    );
    // System-role injection at the tail. Empirical (gym 006,
    // 2026-05-13):
    //   * user-role at tail:        0/5
    //   * system-role at tail:      2/5 ← winner so far
    //   * system-tail + user-prepend: 0/5 (regression — the prepend
    //     diluted the original user directive)
    // Keep the simplest variant that moved the needle.
    // Trim the read history. The system-role nudge alone moved gym
    // 006 from 0% → 20% but capped there: the in-context evidence
    // of "this is a reading session" (3+ assistant read calls + 3+
    // tool results with spec content) keeps the model in
    // exploration mode. Deleting prior reads kills the attractor's
    // evidence base.
    //
    // What we delete: every read-classified assistant tool_call /
    // envelope-shadow pair AND the immediately-following tool
    // result. Plus — and this is the gym 007 fix, captured from
    // a real codex smoke 2026-05-13 — the frontdoor-generated
    // compressed-history user message, when present. Its
    // LLM-written narrative ("Shell commands executed: cat on the
    // spec file succeeded three times", "No actual source files
    // have been written yet") self-reinforces the read pattern,
    // and the trim of raw tool_calls is moot if the summary
    // re-injects the same evidence in prose form.
    //
    // The model retains: codex system prompt, the trailing user
    // pivot directive (if any), the appended system-role nudge.
    // Risk: model loses the spec content it had read. The nudge
    // tells it to write a placeholder regardless; iteration is
    // expected. If the spec is load-bearing the model can re-issue
    // a read in the next turn.
    let read_indices = collect_read_turn_indices(messages);
    for idx in read_indices.into_iter().rev() {
        messages.remove(idx);
    }
    // Replace codex's 20K explore-first system prompt with a focused
    // write-mandate. The codex CLI's system prompt is HEAVILY trained
    // to encourage exploration before action ("Add a preamble for
    // every non-trivial action", "explore the codebase", etc.). When
    // the model is already locked in a read loop, that bias is the
    // proximate cause — every nudge we layer on top is being out-
    // voted by the 20K-token directive at the top of the prompt.
    // Replacing it per-turn isolates the model from the bias; codex
    // resends the original next turn, so the override is scoped to
    // exactly when we need it.
    for msg in messages.iter_mut() {
        if msg.role != "system" {
            continue;
        }
        if msg
            .content
            .starts_with("You are a coding agent running in the Codex CLI")
        {
            msg.content = "You are a code-writing assistant. Your only legal next \
                 action is a single `exec_command` tool call whose `cmd` begins \
                 with `apply_patch <<'EOF'`. Do NOT emit any other shell command \
                 (no cat, ls, rg, find, pwd, head, tail, grep). Commit to writing \
                 the deliverable now."
                .to_string();
            break;
        }
    }
    // The compressed-history user message: keep the task framing
    // (extracted from Block 1's first sentence), drop the rest.
    // Deleting the whole message strips the task signal — model
    // then emits `pwd && ls` to orient itself, defeating the
    // pivot. Rewriting preserves "what to do" while removing the
    // "how I've been doing it" recap that reinforces reading.
    for msg in messages.iter_mut() {
        if msg.role != "user" {
            continue;
        }
        if !msg
            .content
            .starts_with("# Conversation so far (compressed by frontdoor)")
        {
            continue;
        }
        let task_seed = extract_task_seed_from_compressed_history(&msg.content);
        msg.content = format!(
            "## Task\n\n{}\n\nYou have gathered enough context. The NEXT emission \
             MUST be a write action via `apply_patch <<'EOF'\\n*** Begin Patch\\n\
             *** Add File: <path>\\n+<content>\\n*** End Patch\\nEOF`. Do NOT \
             read another file.",
            task_seed
        );
    }

    messages.push(crate::openai_types::ChatMessage {
        role: "system".to_string(),
        content: note,
        tool_call_id: None,
        tool_calls: None,
    });
}

/// Pull the task seed (what the user originally asked for) out of a
/// frontdoor compressed-history user message.
///
/// The compressed history's Block 1 opens with "The user wants me to
/// ..." — the rest of the document is the model's running narrative
/// of HOW it's been working on the task. We want only the WHAT.
///
/// Returns either the first sentence after "The user wants me to" or
/// a generic fallback when the format doesn't match.
fn extract_task_seed_from_compressed_history(content: &str) -> String {
    const ANCHOR: &str = "The user wants me to ";
    if let Some(start) = content.find(ANCHOR) {
        let rest = &content[start + ANCHOR.len()..];
        // Take up to the first sentence boundary (period + space or
        // newline). Cap at 600 bytes so very long opening sentences
        // don't slip the read-recap back in.
        let mut end_byte = rest.len();
        for boundary in [". ", ".\n", "\n\n"] {
            if let Some(idx) = rest.find(boundary) {
                end_byte = end_byte.min(idx + 1); // include the period
            }
        }
        let mut snippet = &rest[..end_byte];
        if snippet.len() > 600 {
            let mut cap = 600;
            while cap > 0 && !snippet.is_char_boundary(cap) {
                cap -= 1;
            }
            snippet = &snippet[..cap];
        }
        return format!("The user wants you to {}", snippet.trim());
    }
    "The user wants you to complete the implementation task.".to_string()
}

/// Collect indices of all messages that participate in read-classified
/// assistant turns: the assistant tool_call message, the adjacent
/// envelope-shadow assistant message (if any), and the immediately
/// following tool result. Returns indices in ascending order.
fn collect_read_turn_indices(messages: &[crate::openai_types::ChatMessage]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        if msg.role != "assistant" {
            i += 1;
            continue;
        }
        // Pull the cmd from tool_calls OR envelope-shadow content.
        let cmd: Option<String> = if let Some(tcs) = &msg.tool_calls {
            tcs.first()
                .and_then(|tc| {
                    if tc.function.name == "exec_command" {
                        Some(tc.function.arguments.as_str())
                    } else {
                        None
                    }
                })
                .and_then(extract_exec_command_cmd)
        } else {
            let trimmed = msg.content.trim_start();
            if trimmed.starts_with('{') && trimmed.contains("\"name\"") {
                serde_json::from_str::<serde_json::Value>(&msg.content)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/arguments/cmd")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                    })
            } else {
                None
            }
        };
        let Some(cmd_str) = cmd else {
            i += 1;
            continue;
        };
        if classify_cmd(&cmd_str) != CmdMode::Read {
            i += 1;
            continue;
        }
        // Mark this assistant message.
        out.push(i);
        // Mark the envelope-shadow sibling if present (next msg,
        // role=assistant, no tool_calls, content starts with `{`).
        if i + 1 < messages.len() {
            let next = &messages[i + 1];
            if next.role == "assistant" && next.tool_calls.is_none() {
                let t = next.content.trim_start();
                if t.starts_with('{') && t.contains("\"name\"") {
                    out.push(i + 1);
                    i += 1;
                }
            }
        }
        // Mark the immediately following tool result (if any).
        if i + 1 < messages.len() && messages[i + 1].role == "tool" {
            out.push(i + 1);
            i += 1;
        }
        i += 1;
    }
    out.sort_unstable();
    out.dedup();
    out
}


/// Detects a failed last `exec_command` in the chat history and
/// appends a synthetic user note instructing the model NOT to repeat
/// the same command.
///
/// Why a separate mechanism from `apply_anti_repetition_chat`:
/// anti-rep fires only after the model has already wasted ≥3 turns
/// on the same dead-end. The single-turn failure case — model emits
/// a command, gets an error, immediately repeats verbatim — is
/// where the attractor is strongest (gym fixture 002: one failed
/// `rg 'oicp-v0.2' --files`, next turn 5/5 emits the exact same
/// string). Surgically nudging at the FIRST failure breaks that
/// attractor before it amplifies into a multi-turn loop.
///
/// Trigger: tail of `messages` is a `tool` result whose content
/// indicates non-zero exit (`Process exited with code N` where
/// N != 0). The preceding assistant message must contain the
/// matching `tool_calls[0]`. Inject a user note referencing both.
///
/// Idempotent: bails when the last user message already starts with
/// `FAILURE_NUDGE_PREFIX`.
pub fn apply_failure_nudge_chat(req: &mut crate::openai_types::ChatCompletionRequest) {
    let messages = &mut req.messages;
    if messages.len() < 2 {
        return;
    }
    // Idempotent: skip if any of our runtime nudges is already in
    // the recent tail (user OR system role).
    if has_recent_runtime_nudge(messages) {
        return;
    }
    // Tail must be a tool result.
    let last = messages.last().expect("len >= 2");
    if last.role != "tool" {
        return;
    }
    let Some(exit_code) = parse_non_zero_exit_code(&last.content) else {
        return;
    };
    // Find the preceding assistant message with `tool_calls` (the
    // call that produced this result). Walk back from the second-to-
    // last message; ignore envelope-shadow assistant messages.
    // Owned strings so we can drop the immutable borrow before
    // mutating the tool result.
    let mut failed_call: Option<(String, String)> = None;
    for msg in messages.iter().rev().skip(1) {
        match msg.role.as_str() {
            "assistant" => {
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    if let Some(tc) = tcs.first() {
                        failed_call = Some((
                            tc.function.name.clone(),
                            tc.function.arguments.clone(),
                        ));
                    }
                    break;
                }
                // No tool_calls — could be an envelope shadow; keep walking.
                let trimmed = msg.content.trim_start();
                if trimmed.starts_with('{') && trimmed.contains("\"name\"") {
                    continue;
                }
                // Real text-only reply — give up; we can't attribute
                // the tool result to a specific call.
                break;
            }
            "tool" => continue, // chained tool results; keep walking
            _ => break,
        }
    }
    let Some((name, args)) = failed_call else { return };
    let args_preview = if args.len() > 200 {
        let mut end = 200;
        while end > 0 && !args.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &args[..end])
    } else {
        args.clone()
    };
    // Extract the command string from the JSON args so we can quote
    // it verbatim in the nudge. Fall back to the full args blob if
    // the parse fails — gym fixture 002 shows the model's attention
    // collapses to the exact `cmd` string, so quoting it explicitly
    // is what breaks the echo attractor.
    let cmd_quote = extract_exec_command_cmd(&args).unwrap_or_else(|| args_preview.clone());

    // Three-pronged intervention. Crucially: the banner and the
    // user nudge do NOT echo the failed cmd verbatim. The model's
    // primary failure mode is token-level echo of the last emitted
    // cmd. If we ever name the cmd in our nudge text, the model can
    // reconstruct it character-by-character from our own nudge.
    // Empirically: even with redaction in the assistant message,
    // an echo of the cmd inside the banner restores the attractor.
    //
    //   1. Prepend a hard-signal banner to the failing tool result.
    //      Generic — refers to "your previous command" without
    //      quoting it.
    //   2. Append the user-message nudge as a backup signal.
    //   3. Redact the cmd from the prior assistant tool_calls
    //      (and any envelope-shadow) so the only place the verbatim
    //      cmd still appears is in the tool result body's error
    //      message — which is data the model has to actively
    //      paraphrase, not lazy-echo.
    //
    // The tool-result rewrite is idempotent: if the result already
    // starts with our banner prefix, skip.
    let _ = cmd_quote; // retained for tracing only; not emitted to model
    let banner = format!(
        "[FAILURE — exit code {}] Your previous shell command failed. Do NOT \
         emit the same command again — it WILL fail identically. Pick a \
         genuinely different command.\n\n",
        exit_code
    );
    if let Some(last_tool_msg) = messages.last_mut() {
        if last_tool_msg.role == "tool" && !last_tool_msg.content.starts_with("[FAILURE")
        {
            let mut new_content = banner.clone();
            new_content.push_str(&last_tool_msg.content);
            last_tool_msg.content = new_content;
        }
    }

    // Delete the failed assistant call and any envelope-shadow
    // siblings. The model's strongest attractor is verbatim echo of
    // the last assistant message. Redaction (replacing the cmd with
    // a placeholder) just shifted the echo target — the model
    // happily emitted "[redacted: ...]" as the new cmd. Removing
    // the messages entirely leaves the model nothing to copy.
    //
    // The tool result is retained (with banner) as the only signal
    // that a failure occurred. The tool_call_id chain is broken
    // post-edit but the inference layer doesn't validate it —
    // messages are flattened into a single prompt before sampling.
    delete_failed_call_from_history(messages, &cmd_quote);
    // Nudge stays generic about the failed cmd (no echo) but
    // explicit about ALTERNATIVES. Without concrete alternatives,
    // the model reconstructs the failed cmd token-by-token from
    // adjacent context (the error message body, the system prompt's
    // tool examples, the prior cat commands' shape). Naming
    // alternatives pushes the next emission toward a different
    // attractor.
    let _ = args_preview; // retained for tracing only
    let note = format!(
        "{} Your last `{}` call failed with exit code {}. Do NOT retry the \
         same command. Pick a structurally different action:\n\
         • If you were searching for content, switch to listing files: \
         `ls`, `find . -type f -name '*.ext'`.\n\
         • If you were reading a file by name, try `cat <relative_path>` \
         with a DIFFERENT filename.\n\
         • If you've gathered enough information already, COMMIT to the \
         next concrete step (often: write code with `apply_patch`).",
        FAILURE_NUDGE_PREFIX, name, exit_code
    );
    info!(
        tool_name = %name,
        exit_code,
        cmd = %cmd_quote,
        "frontdoor: failure-recovery note injected (chat)"
    );
    messages.push(crate::openai_types::ChatMessage {
        role: "user".to_string(),
        content: note,
        tool_call_id: None,
        tool_calls: None,
    });
}

/// Parse the `cmd` field out of a JSON-encoded exec_command args
/// string. Returns `None` for malformed args or missing `cmd`.
fn extract_exec_command_cmd(args_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args_json).ok()?;
    v.get("cmd").and_then(|c| c.as_str()).map(|s| s.to_string())
}

/// Delete the most recent assistant turn that matches `failed_cmd`,
/// including any envelope-shadow sibling messages emitted for the
/// same call. The tool result (last message) is preserved — it
/// carries the failure banner and the actual error text, which is
/// the signal we want the model to attend to.
///
/// Why deletion not redaction: empirically (gym 2026-05-13), the
/// model emits whatever placeholder we put in the prior assistant
/// `cmd` field as its next emission, because the model's strongest
/// signal is "echo the last assistant turn". Removing the messages
/// instead leaves no echo target.
///
/// Iteration is reverse-from-end, skipping the trailing tool
/// result. Stops at the first non-tool, non-failed-assistant message
/// (so we only touch the most recent failure).
fn delete_failed_call_from_history(
    messages: &mut Vec<crate::openai_types::ChatMessage>,
    failed_cmd: &str,
) {
    // Collect indices to delete. We iterate from the second-to-last
    // message backwards (last message is the tool result, retained).
    let mut indices_to_delete: Vec<usize> = Vec::new();
    let mut i = messages.len();
    while i > 0 {
        i -= 1;
        // Skip trailing tool results entirely.
        if i == messages.len() - 1 && messages[i].role == "tool" {
            continue;
        }
        let msg = &messages[i];
        match msg.role.as_str() {
            "tool" => continue, // chained tool results
            "assistant" => {
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    if let Some(tc) = tcs.first() {
                        // Is this the failed call?
                        if let Ok(parsed) =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        {
                            if parsed.get("cmd").and_then(|v| v.as_str()) == Some(failed_cmd) {
                                indices_to_delete.push(i);
                                // Found the call. Keep walking up the
                                // history to catch the matching
                                // envelope-shadow (it's emitted as a
                                // separate adjacent assistant message
                                // in the captured shape).
                                continue;
                            }
                        }
                    }
                    // Different assistant call — stop.
                    break;
                }
                // Envelope-shadow assistant: keep walking. If the
                // shadow IS for the failed call, delete it. Either
                // way, continue searching — the actual tool_calls
                // message may be just beyond.
                let trimmed = msg.content.trim_start();
                if trimmed.starts_with('{') && trimmed.contains("\"name\"") {
                    if let Ok(env) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                        if env.pointer("/arguments/cmd").and_then(|v| v.as_str())
                            == Some(failed_cmd)
                        {
                            indices_to_delete.push(i);
                        }
                    }
                    continue;
                }
                // Real text-only assistant reply — stop.
                break;
            }
            _ => break, // user/system — stop
        }
    }
    // Also catch the envelope-shadow that may sit BETWEEN the deleted
    // tool_call and the tool result. Re-walk: if any message we
    // didn't already mark is an envelope shadow of the failed call,
    // mark it too.
    for (idx, msg) in messages.iter().enumerate() {
        if indices_to_delete.contains(&idx) || msg.role != "assistant" {
            continue;
        }
        if msg.tool_calls.is_some() {
            continue;
        }
        let trimmed = msg.content.trim_start();
        if !(trimmed.starts_with('{') && trimmed.contains("\"name\"")) {
            continue;
        }
        if let Ok(env) = serde_json::from_str::<serde_json::Value>(&msg.content) {
            if env.pointer("/arguments/cmd").and_then(|v| v.as_str()) == Some(failed_cmd) {
                indices_to_delete.push(idx);
            }
        }
    }
    indices_to_delete.sort_unstable();
    indices_to_delete.dedup();
    for idx in indices_to_delete.into_iter().rev() {
        messages.remove(idx);
    }
}

/// Parse a `Process exited with code N` line out of a tool result
/// body. Returns `Some(N)` when N != 0, `None` otherwise (including
/// when the line is absent or the code is zero).
fn parse_non_zero_exit_code(body: &str) -> Option<i32> {
    for line in body.lines() {
        let Some(suffix) = line.trim().strip_prefix("Process exited with code ") else {
            continue;
        };
        let code: i32 = suffix.trim().parse().ok()?;
        return if code != 0 { Some(code) } else { None };
    }
    None
}

/// Chat-completions-side variant of `apply_anti_repetition`.
///
/// Walks `messages` from the tail, counting consecutive assistant
/// `tool_calls[0]` items with identical `(name, arguments)`. Tool
/// messages between assistant turns don't break the run (they are
/// the expected request→result pairs). If the run is at least
/// `REPETITION_THRESHOLD`, appends a synthetic `user` message naming
/// the repeated call and instructing the model to switch strategy.
///
/// Why mirror `apply_anti_repetition` here: that function operates on
/// `ResponsesRequest::input` items and only runs on the
/// `/v1/responses` ingress. Direct `/v1/chat/completions` callers
/// (gym fixtures, peer mesh, opencode without the Responses adapter)
/// don't benefit. The Codex-style loop bias is a property of the
/// inference layer, not the entry point — so the nudge should apply
/// at both ingresses.
///
/// Idempotent: when the tail message is already our synthetic note,
/// returns without re-injecting.
pub fn apply_anti_repetition_chat(req: &mut crate::openai_types::ChatCompletionRequest) {
    let messages = &mut req.messages;
    if messages.is_empty() {
        return;
    }
    // Idempotent: any prior runtime nudge (anti-rep, failure-recovery,
    // read-attractor) makes this turn the model's responsibility —
    // don't stack a second nudge on top.
    if has_recent_runtime_nudge(messages) {
        return;
    }

    let mut last_call: Option<(&str, &str)> = None;
    let mut run_len: usize = 0;
    for msg in messages.iter().rev() {
        match msg.role.as_str() {
            "assistant" => {
                let Some(tcs) = msg.tool_calls.as_ref() else {
                    // Assistant message with no `tool_calls`. Two cases:
                    //   (a) envelope-as-content "shadow" — the model
                    //       emitted a JSON envelope as content, then
                    //       the response-translator emitted the same
                    //       call as a separate assistant message with
                    //       `tool_calls`. We see both shapes in the
                    //       input history. Treat as transparent.
                    //   (b) genuine text-only assistant reply — the
                    //       model said something instead of calling
                    //       a tool. That's a real strategy change;
                    //       break the run.
                    let trimmed = msg.content.trim_start();
                    if trimmed.starts_with('{') && trimmed.contains("\"name\"") {
                        continue;
                    }
                    break;
                };
                let Some(tc) = tcs.first() else { break };
                let name = tc.function.name.as_str();
                let args = tc.function.arguments.as_str();
                match last_call {
                    None => {
                        last_call = Some((name, args));
                        run_len = 1;
                    }
                    Some((p_name, p_args)) => {
                        if p_name == name && p_args == args {
                            run_len += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            "tool" => {
                // Tool result between assistant calls — expected, skip.
            }
            _ => {
                // user / system message ends the run.
                break;
            }
        }
    }
    if run_len < REPETITION_THRESHOLD {
        return;
    }
    let Some((name, args)) = last_call else { return };
    let args_preview = if args.len() > 200 {
        let mut end = 200;
        while end > 0 && !args.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &args[..end])
    } else {
        args.to_string()
    };
    let note = format!(
        "{} You have just emitted `{}({})` {} times in a row with the same result. \
         The approach is failing. Do NOT repeat this command. Try a fundamentally \
         different strategy — a different tool, a different argument shape, or \
         abandon the sub-goal and proceed to the next step.",
        REPETITION_NOTE_PREFIX, name, args_preview, run_len
    );
    info!(
        tool_name = %name,
        run_len,
        "frontdoor: anti-repetition note injected (chat)"
    );
    messages.push(crate::openai_types::ChatMessage {
        role: "user".to_string(),
        content: note,
        tool_call_id: None,
        tool_calls: None,
    });
}

/// Extract absolute-path-shaped substrings from `s`. A path is a
/// `/`-rooted sequence of at least 2 components made of
/// `[A-Za-z0-9_.-]`. Returns owned strings deduplicated.
pub fn extract_absolute_paths(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() && is_path_char(bytes[i + 1]) {
            let start = i;
            let mut j = i + 1;
            let mut components = 0usize;
            loop {
                let comp_start = j;
                while j < bytes.len() && is_path_char(bytes[j]) {
                    j += 1;
                }
                if j == comp_start {
                    break;
                }
                components += 1;
                if j < bytes.len()
                    && bytes[j] == b'/'
                    && j + 1 < bytes.len()
                    && is_path_char(bytes[j + 1])
                {
                    j += 1;
                    continue;
                }
                break;
            }
            if components >= 2 {
                if let Ok(slice) = std::str::from_utf8(&bytes[start..j]) {
                    out.push(slice.to_string());
                }
                i = j;
                continue;
            }
            i = start + 1;
            continue;
        }
        i += 1;
    }
    out.sort();
    out.dedup();
    out
}

/// Collect every absolute path that appears anywhere in `messages` —
/// content bodies AND inside `tool_calls[].function.arguments` JSON.
/// Returns a deduplicated set. Kept for diagnostic / test use; the
/// canonicalizer itself uses frequency-weighted components instead
/// (see `gather_context_components`).
pub fn gather_context_paths(
    messages: &[crate::openai_types::ChatMessage],
) -> std::collections::HashSet<String> {
    let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages {
        for p in extract_absolute_paths(&msg.content) {
            set.insert(p);
        }
        if let Some(tcs) = &msg.tool_calls {
            for tc in tcs {
                for p in extract_absolute_paths(&tc.function.arguments) {
                    set.insert(p);
                }
            }
        }
    }
    set
}

/// Levenshtein edit distance (small-string variant). Capped at
/// `cap` — returns `cap + 1` when the true distance exceeds it, to
/// keep us out of pathological O(mn) on long mismatched strings.
fn levenshtein_capped(a: &str, b: &str, cap: usize) -> usize {
    let (a, b) = if a.len() < b.len() { (a, b) } else { (b, a) };
    let m = a.chars().count();
    let n = b.chars().count();
    if n - m > cap {
        return cap + 1;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for j in 1..=n {
        curr[0] = j;
        let mut row_min = curr[0];
        for i in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            curr[i] = (prev[i] + 1)
                .min(curr[i - 1] + 1)
                .min(prev[i - 1] + cost);
            if curr[i] < row_min {
                row_min = curr[i];
            }
        }
        if row_min > cap {
            return cap + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Split a `/`-rooted path into its components, dropping the empty
/// leading element from the initial `/`.
fn path_components(path: &str) -> Vec<&str> {
    path.split('/').filter(|c| !c.is_empty()).collect()
}

/// Frequency-weighted multiset of all PATH COMPONENTS that appear
/// across the entire request — both the unique `context_paths` and
/// the raw text bodies. The canonicalizer prefers the most frequent
/// variant when picking among similar components.
///
/// Why frequency, not just presence: real session histories often
/// contain a typo path AT LEAST ONCE (gym 003: a compressed history
/// block summary recites the typo'd path the model emitted in a
/// prior turn). Plain set-membership treats the typo as "already
/// canonical" and skips the rewrite. Frequency weighting recovers
/// the right canonical: the typo appears once; the correct form
/// appears 3+ times across content + tool_calls; we prefer the
/// frequent one.
fn context_path_components(
    messages: &[crate::openai_types::ChatMessage],
) -> std::collections::HashMap<String, usize> {
    let mut counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut add_from = |s: &str, counts: &mut std::collections::HashMap<String, usize>| {
        for p in extract_absolute_paths(s) {
            for c in path_components(&p) {
                *counts.entry(c.to_string()).or_insert(0) += 1;
            }
        }
    };
    for msg in messages {
        add_from(&msg.content, &mut counts);
        if let Some(tcs) = &msg.tool_calls {
            for tc in tcs {
                add_from(&tc.function.arguments, &mut counts);
            }
        }
    }
    counts
}

/// Find the best canonical replacement for `component` in
/// `context_components` (frequency-weighted).
///
/// Decision:
///   * If a SIMILAR component (Lev ≤ cap, len ≥ 5) has at least
///     `FREQ_RATIO_FOR_REWRITE` × the emitted component's frequency,
///     prefer it as canonical.
///   * Otherwise leave the component alone.
/// This means a typo that appears once is overruled by the correct
/// form appearing 3+ times. A unique component (frequency 1) with no
/// other candidates is left as-is — we assume it's intentional.
pub fn find_canonical_component<'a>(
    component: &str,
    context_components: &'a std::collections::HashMap<String, usize>,
) -> Option<&'a String> {
    const FREQ_RATIO_FOR_REWRITE: usize = 2;
    if component.len() < 5 {
        return None;
    }
    let own_freq = context_components.get(component).copied().unwrap_or(0);
    let lev_cap = (component.len() / 10).max(2);
    let mut best: Option<(&String, usize, usize)> = None; // (component, freq, distance)
    for (ctx, &ctx_freq) in context_components.iter() {
        if ctx == component {
            continue; // skip self
        }
        if ctx.len() < 5 {
            continue;
        }
        let d = levenshtein_capped(component, ctx, lev_cap);
        if d == 0 || d > lev_cap {
            continue;
        }
        // Prefer higher frequency, then lower distance.
        match best {
            None => best = Some((ctx, ctx_freq, d)),
            Some((_, bf, bd)) => {
                if ctx_freq > bf || (ctx_freq == bf && d < bd) {
                    best = Some((ctx, ctx_freq, d));
                }
            }
        }
    }
    let Some((canonical, canonical_freq, _)) = best else {
        return None;
    };
    // If the emitted component is well-attested in context (high
    // frequency itself), don't rewrite — it's intentional.
    if own_freq > 0 && canonical_freq < own_freq.saturating_mul(FREQ_RATIO_FOR_REWRITE) {
        return None;
    }
    // For an emitted component that doesn't appear in context at all,
    // any similar canonical wins regardless of ratio.
    Some(canonical)
}

/// Walk `cmd`; for each absolute path, split into components, and
/// for each component NOT in `context_components` but similar to one
/// that IS, rewrite it. Returns `Some(rewritten)` when any rewrite
/// happened, `None` otherwise.
pub fn canonicalize_paths_in_cmd(
    cmd: &str,
    context_components: &std::collections::HashMap<String, usize>,
) -> Option<String> {
    if context_components.is_empty() {
        return None;
    }
    let paths = extract_absolute_paths(cmd);
    if paths.is_empty() {
        return None;
    }
    let mut out = cmd.to_string();
    let mut any_fix = false;
    for emitted_path in paths {
        let comps = path_components(&emitted_path);
        let mut new_comps: Vec<String> = Vec::with_capacity(comps.len());
        let mut rewrote_this_path = false;
        for c in &comps {
            match find_canonical_component(c, context_components) {
                Some(canonical) => {
                    new_comps.push(canonical.clone());
                    rewrote_this_path = true;
                }
                None => new_comps.push((*c).to_string()),
            }
        }
        if !rewrote_this_path {
            continue;
        }
        let canonical_path = format!("/{}", new_comps.join("/"));
        if out.contains(&emitted_path) {
            out = out.replace(&emitted_path, &canonical_path);
            any_fix = true;
        }
    }
    if any_fix {
        Some(out)
    } else {
        None
    }
}

/// Post-emission path canonicalization on chat response tool_calls.
/// For each `exec_command` call, scan the `cmd` field for absolute
/// paths that don't exist in `context_paths`; rewrite to a similar
/// canonical path when one exists.
///
/// Why: the model's BPE tokenizer can drop or add a leading character
/// on long path tokens (gym 003: `/atos-experiment-oicp-types` emits
/// as `/tos-experiment-oicp-types` under greedy because `/tos` is a
/// single token while `/atos` is multi-token at lower joint
/// probability). The role-conditional Content/greedy sampler DOES
/// engage (confirmed via SOVEREIGN_TRACE_SAMPLER_ROLES, 2026-05-13)
/// but the tokenizer's BPE merges are the bottleneck — sampler-level
/// fix isn't possible without logit_bias support. Post-emission
/// canonicalization is the cheapest correct fix.
///
/// Returns the count of rewrites performed.
pub fn canonicalize_chat_response_paths(
    tool_calls: &mut [crate::openai_types::ToolCall],
    context_components: &std::collections::HashMap<String, usize>,
) -> usize {
    let mut fixed = 0usize;
    for tc in tool_calls.iter_mut() {
        if tc.function.name != "exec_command" {
            continue;
        }
        let parsed: serde_json::Value =
            match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(_) => continue,
            };
        let cmd = match parsed.get("cmd").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        if let Some(rewritten) = canonicalize_paths_in_cmd(cmd, context_components) {
            info!(
                cmd_first_60 = %&cmd.chars().take(60).collect::<String>(),
                rewritten_first_60 = %&rewritten.chars().take(60).collect::<String>(),
                "path-canonicalizer rewrote tool_call cmd"
            );
            let mut new_obj = parsed.clone();
            new_obj["cmd"] = serde_json::Value::String(rewritten);
            if let Ok(reser) = serde_json::to_string(&new_obj) {
                tc.function.arguments = reser;
                fixed += 1;
            }
        }
    }
    fixed
}

/// Convenience: build the frequency-weighted component map from a
/// request's messages. Call once pre-generation; pass the result
/// into `canonicalize_chat_response_paths` post-generation.
pub fn gather_context_components(
    messages: &[crate::openai_types::ChatMessage],
) -> std::collections::HashMap<String, usize> {
    context_path_components(messages)
}

/// Scalar canonicalizer used by both the non-streaming chat response
/// path and the SSE streaming-response translator in
/// `routes_responses`. Takes the function `name` and serialized
/// `arguments` (JSON object containing `cmd`); if the call is an
/// `exec_command` whose `cmd` is an apply_patch heredoc that needs
/// repair, returns the rewritten arguments JSON. Returns `None` for
/// any case where no rewrite applies (different tool, non-JSON args,
/// no apply_patch heredoc, already canonical).
pub fn canonicalize_exec_command_arguments(name: &str, arguments: &str) -> Option<String> {
    if name != "exec_command" {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let cmd = parsed.get("cmd").and_then(|v| v.as_str())?;
    let canonical = canonicalize_apply_patch_heredoc(cmd)?;
    if canonical == cmd {
        return None;
    }
    let mut new_obj = parsed.clone();
    new_obj["cmd"] = serde_json::Value::String(canonical);
    serde_json::to_string(&new_obj).ok()
}

/// Walk the `tool_calls` in a chat-completions message; for any
/// `exec_command` call whose `cmd` is an apply_patch heredoc, replace
/// it with the canonical form. Returns the number of canonicalizations
/// performed (0 = no-op).
pub fn canonicalize_chat_response_tool_calls(
    tool_calls: &mut [crate::openai_types::ToolCall],
) -> usize {
    let mut fixed = 0usize;
    for tc in tool_calls.iter_mut() {
        if let Some(rewritten) =
            canonicalize_exec_command_arguments(&tc.function.name, &tc.function.arguments)
        {
            tc.function.arguments = rewritten;
            fixed += 1;
        }
    }
    fixed
}

/// Which tool policy block to render in the distilled directive.
/// Matches the `runs_synthetic_tools` decision: Opencode profile
/// gets synthetic `write_file*` injection so its policy points
/// agents at those tools; Codex profile keeps codex's apply_patch
/// training prior and gets a shell-heredoc policy instead.
#[derive(Debug, Clone, Copy)]
enum ToolPolicyShape {
    SyntheticFileTools,
    ApplyPatchHeredoc,
}

impl DistilledDirective {
    fn render(&self, policy: ToolPolicyShape) -> String {
        let mut out = String::new();
        // Path-scrub every model-emitted string before it lands in
        // the agent's prompt. See `scrub_paths` doc.
        let task = scrub_paths(self.task.trim());
        let constraints = scrub_paths(self.constraints.trim());
        let done_when = scrub_paths(self.done_when.trim());
        if !task.is_empty() {
            out.push_str("## Task\n\n");
            out.push_str(&task);
            out.push_str("\n\n");
        }
        if !constraints.is_empty() {
            out.push_str("## Constraints\n\n");
            out.push_str(&constraints);
            out.push_str("\n\n");
        }
        if !done_when.is_empty() {
            out.push_str("## Done when\n\n");
            out.push_str(&done_when);
            out.push_str("\n\n");
        }
        // Harness-conditional tool usage policy. The previous version
        // baked a single "use write_file" block into every render,
        // which actively misled Codex profile sessions (where
        // synthetic tools aren't injected — the model called for
        // tools it didn't have and reverted to read-loops). Each
        // profile gets the block that matches its actual catalog.
        out.push_str("## Tool usage policy\n\n");
        match policy {
            ToolPolicyShape::SyntheticFileTools => {
                out.push_str(
                    "- To create or replace a file (any `.rs`, `.toml`, `.md`, `.txt`, `.json`): call `write_file(path, content)`. For content over 350 bytes call `write_file_begin(path)` then a series of `write_file_chunk(path, chunk)` (150-250 bytes each) then `write_file_end(path)`. NEVER use a shell heredoc, `cat > file <<EOF`, `echo > file`, or `printf > file` — those paths break under the grammar and lose content.\n",
                );
                out.push_str(
                    "- To read a file: call `read_file(path)`. Do NOT use `cat`, `head`, `tail`, `less`, `ls`, or `find` via `exec_command` — `read_file` is faster and avoids path-corruption typos.\n",
                );
                out.push_str(
                    "- Use `exec_command` ONLY for build/test verification: `cargo test`, `cargo build`, `cargo check`, `cargo run`.\n",
                );
            }
            ToolPolicyShape::ApplyPatchHeredoc => {
                out.push_str(
                    "- To create or replace a file, emit ONE `exec_command` call with `cmd` set to an `apply_patch` shell heredoc:\n",
                );
                out.push_str(
                    "  `apply_patch <<'EOF'\\n*** Begin Patch\\n*** Add File: <path>\\n+<line1>\\n+<line2>\\n*** End Patch\\nEOF`\n",
                );
                out.push_str(
                    "  The single-quoted heredoc passes content verbatim — do NOT double-escape backslashes inside the body. One file per call.\n",
                );
                out.push_str(
                    "- To read or list files, use `exec_command` with `cat`, `ls`, `find`, or `rg`.\n",
                );
                out.push_str(
                    "- To verify your work, use `exec_command` with `cargo check`, `cargo build`, or `cargo test`.\n",
                );
            }
        }
        out.push_str(
            "- Discover paths empirically — when the workdir or file location is unclear, list directories with `ls` or `find` BEFORE assuming a path. Do not invent or guess directory names.\n\n",
        );
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

Output exactly ONE JSON object with this shape, no prose around it, no markdown fences:

{
  "task": "<one-paragraph plain-prose description of the user's actual intent>",
  "constraints": "<one paragraph of load-bearing constraints worth highlighting, or empty>",
  "done_when": "<unambiguous completion criterion the agent can verify>"
}

Hard rules:
- DO NOT echo the original harness system prompt.
- DO NOT include tool catalog reminders — the agent already knows its tools.
- DO NOT add style advice or meta commentary.
- DO NOT mention codex, opencode, plugins, marketplaces, or anything about the harness.
- DO NOT invent file paths, directory names, or workdir locations. The agent will discover paths empirically via its own tools — your job is to capture INTENT, not file inventory.
- The output prose IS the complete context the agent will see — strip everything except the actual ask."#;

/// Run the distiller pass and overwrite `req.instructions` with the
/// distilled directive. Cached by SHA of the original instructions +
/// first user input.
pub(crate) async fn apply_distiller(
    state: &AppState,
    headers: &HeaderMap,
    req: &mut ResponsesRequest,
    harness: Harness,
) {
    let policy = if harness.runs_synthetic_tools() {
        ToolPolicyShape::SyntheticFileTools
    } else {
        ToolPolicyShape::ApplyPatchHeredoc
    };
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
    // Grammar-coerced output: schema enforces the 3 string fields.
    // Model can no longer emit free-form prose; this slot's job is
    // to fill structured fields. Paired with `scrub_paths` post-
    // processing — together they bound the distiller's hallucination
    // blast radius to "wrong words inside well-shaped strings,"
    // structurally never "wrong shape" and never "leaked paths."
    let directive_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "task": {"type": "string"},
            "constraints": {"type": "string"},
            "done_when": {"type": "string"}
        },
        "required": ["task", "done_when"],
        "additionalProperties": false
    });
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
        response_format: Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "distilled_directive",
                "schema": directive_schema,
                "strict": true
            }
        })),
        oicp: None,
        chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
        think_budget: Some(0),
        tool_profile: None,
    sampling_mode: None,
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

    let rendered = directive.render(policy);
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

// ─── Anti-repetition injection ──────────────────────────────────────
//
// In-context loop break (Investment #15, 2026-05-13). Codex resends
// the entire conversation every turn. When the model emits the same
// failing command N consecutive times, each turn's input contains
// all prior identical emissions — reinforcing the loop via in-context
// learning. Temperature alone can't reliably escape (bench: T=0.7
// breaks the rg-loop attractor 8/10 turns, but a second identical
// attractor can lock in just as hard).
//
// Detection: walk the input items from the end. Count consecutive
// FunctionCall items with identical (name, arguments). When the run
// length ≥ REPETITION_THRESHOLD, prepend a synthetic user message
// alerting the model to the loop and explicitly disallowing one more
// repeat. Empirical: the model emits clean tool envelopes ~10/10
// turns (Inv 3 grammar), so it CAN read this nudge and adapt — it's
// the in-context bias that's wrong, not capability.
//
// The injection is idempotent: when the latest item is the synthetic
// note itself we don't re-inject. Cleared automatically as the
// model's next emission breaks the run length.

const REPETITION_THRESHOLD: usize = 3;
const REPETITION_NOTE_PREFIX: &str = "[anti-repetition note from runtime]";

/// Inspect the input items for a run of identical assistant tool
/// calls at the tail. If the run ≥ `REPETITION_THRESHOLD`, prepend
/// a synthetic user message before the most recent items naming the
/// repeated emission and instructing the model to switch strategy.
pub fn apply_anti_repetition(req: &mut ResponsesRequest) {
    use crate::responses_types::FunctionCallItem;
    let items = match &mut req.input {
        ResponsesInput::Items(v) => v,
        ResponsesInput::Text(_) => return,
    };
    // Walk from the end, counting consecutive FunctionCall items with
    // matching (name, arguments). FunctionCallOutput items between
    // them are expected (each tool_call has its result); they don't
    // break the run.
    let mut last_call: Option<&FunctionCallItem> = None;
    let mut run_len: usize = 0;
    for item in items.iter().rev() {
        match item {
            ResponsesInputItem::FunctionCall(c) => {
                match last_call {
                    None => {
                        last_call = Some(c);
                        run_len = 1;
                    }
                    Some(prev) => {
                        if prev.name == c.name && prev.arguments == c.arguments {
                            run_len += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            ResponsesInputItem::FunctionCallOutput(_) => {
                // Tool result between calls — keep walking; doesn't
                // affect the run length.
            }
            ResponsesInputItem::Message(m) => {
                // Reaching a user/assistant message ends the run.
                // Also: if it's our own anti-repetition note, the
                // runtime already nudged — don't re-inject.
                if let MessageContent::Text(t) = &m.content {
                    if t.starts_with(REPETITION_NOTE_PREFIX) {
                        return;
                    }
                }
                break;
            }
        }
    }
    if run_len < REPETITION_THRESHOLD {
        return;
    }
    let Some(call) = last_call else { return };
    let args_preview = if call.arguments.len() > 200 {
        let mut end = 200;
        while end > 0 && !call.arguments.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &call.arguments[..end])
    } else {
        call.arguments.clone()
    };
    let note = format!(
        "{} You have just emitted `{}({})` {} times in a row with the same result. \
         The approach is failing. Do NOT repeat this command. Try a fundamentally \
         different strategy — a different tool, a different argument shape, or \
         abandon the sub-goal and proceed to the next step.",
        REPETITION_NOTE_PREFIX, call.name, args_preview, run_len
    );
    info!(
        tool_name = %call.name,
        run_len,
        "frontdoor: anti-repetition note injected"
    );
    items.push(ResponsesInputItem::Message(MessageItem {
        role: "user".to_string(),
        content: MessageContent::Text(note),
    }));
}

// ─── History compression (block-based) ──────────────────────────────
//
// Codex resends the entire conversation every turn. By turn 10 the
// request includes the original task + 9 prior assistant tool_calls
// + 9 tool_results, and the model loses coherence past ~20-30K
// tokens of working context (the arxiv-named MoE working-context
// ceiling). So we summarise older turns before forwarding to the
// inference slot.
//
// **Block-based caching** (Investment #12, 2026-05-13). The original
// 2026-05-12 design summarised a variable-length suffix of older
// turns: range `[0, items.len() - HISTORY_KEEP_RECENT)`. Every new
// turn the range grew by 1, the SHA-256 cache key was fresh, and we
// paid for re-summarisation every single turn. Empirically: 11-turn
// codex smoke 2026-05-13 fired 24 re-summarisations, each consuming
// ~13s of primary-slot wall-clock and stripping prior tool results
// the model needed to avoid command loops.
//
// New strategy: partition the compressible prefix into fixed-size
// **blocks** of `HISTORY_BLOCK_SIZE` items, summarise each block
// independently, and cache by SHA-256 of the block's contents. A
// block's identity is stable once closed — turn N+1 doesn't change
// turn N's block hash. Cache hits become real.
//
//   conversation:  [b0 b1 b2 b3 b4 b5 b6 b7][b8 b9 b10 …][partial …][recent x4]
//                  ←── Block 1 (closed) ───→←── Block 2 ──→  ← kept verbatim ──
//
// Each closed block contributes one summary paragraph. The synthetic
// `# Conversation so far` user-message stitches them together in
// order. Items past the last closed-block boundary stay verbatim,
// joining the recent-window. As the conversation grows, new blocks
// close one at a time; only the newest closed block requires a
// fresh summarisation call.

/// Items per closed block. Tuned against the codex resend pattern:
/// 8 items ≈ 4 turns of (user prompt + tool_call + tool_result), a
/// natural "phase" granularity for tool-using sessions.
const HISTORY_BLOCK_SIZE: usize = 8;
/// Items kept verbatim at the conversation tail (always the most
/// recent — the model's working memory for the current sub-task).
const HISTORY_KEEP_RECENT: usize = 4;
/// Byte-size backup trigger: when the conversation has too few
/// items to form a block but one of them carries a multi-KB tool
/// result, we still compress the prefix. Matches the
/// "agentic success under 20-30K tokens" working-context ceiling
/// for MoE coherence.
const HISTORY_COMPRESS_BYTES: usize = 20_480;

#[cfg(test)]
fn items_byte_size(items: &[ResponsesInputItem]) -> usize {
    items.iter().map(item_byte_size).sum()
}

fn item_byte_size(item: &ResponsesInputItem) -> usize {
    match item {
        ResponsesInputItem::Message(m) => match &m.content {
            MessageContent::Text(s) => s.len(),
            MessageContent::Parts(ps) => ps.iter().map(part_byte_size).sum(),
        },
        ResponsesInputItem::FunctionCall(c) => c.name.len() + c.arguments.len(),
        ResponsesInputItem::FunctionCallOutput(o) => o.output.len(),
    }
}

fn part_byte_size(p: &ResponsesContentPart) -> usize {
    match p {
        ResponsesContentPart::InputText { text } | ResponsesContentPart::OutputText { text } => {
            text.len()
        }
        ResponsesContentPart::Other => 0,
    }
}

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

/// Determine how the current items list partitions into closed
/// blocks (eligible for cached summarisation) and a verbatim tail.
/// Returns `None` when the conversation is short enough that no
/// compression is warranted.
///
/// Algorithm:
/// 1. Anchor recent window: last `HISTORY_KEEP_RECENT` items always
///    kept verbatim.
/// 2. Remaining `compressible_count` items split into closed blocks
///    of `HISTORY_BLOCK_SIZE`; any leftover partial-block items
///    spill into the verbatim tail (so block boundaries are stable
///    integer multiples of BLOCK_SIZE from the start).
/// 3. If `closed_blocks == 0` AND total bytes exceed the byte
///    backup trigger, force a single best-effort block over the
///    eligible prefix so we don't OOM-context on huge tool results
///    in short conversations.
fn plan_blocks(items: &[ResponsesInputItem]) -> Option<BlockPlan> {
    if items.len() <= HISTORY_KEEP_RECENT {
        return None;
    }
    let compressible_count = items.len() - HISTORY_KEEP_RECENT;
    let closed_blocks = compressible_count / HISTORY_BLOCK_SIZE;
    if closed_blocks > 0 {
        let block_items = closed_blocks * HISTORY_BLOCK_SIZE;
        return Some(BlockPlan {
            closed_blocks,
            block_items_end: block_items,
            triggered_by: BlockTrigger::ItemCount,
        });
    }
    // No full block yet. Fall back to byte trigger: if any of the
    // compressible items is fat enough that the total exceeds the
    // working-context ceiling, summarise the eligible prefix as a
    // single best-effort "byte-backup" block.
    let bytes_in_compressible: usize = items[..compressible_count]
        .iter()
        .map(item_byte_size)
        .sum();
    if bytes_in_compressible > HISTORY_COMPRESS_BYTES {
        return Some(BlockPlan {
            closed_blocks: 1,
            block_items_end: compressible_count,
            triggered_by: BlockTrigger::ByteBackup,
        });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockTrigger {
    ItemCount,
    ByteBackup,
}

#[derive(Debug, Clone)]
struct BlockPlan {
    closed_blocks: usize,
    /// `items[..block_items_end]` are eligible for compression.
    /// Items past this index stay verbatim.
    block_items_end: usize,
    triggered_by: BlockTrigger,
}

/// Summarise the conversation's older closed blocks into one
/// synthetic `# Conversation so far` user-message at the head of
/// `req.input`. See module-level docs for the block-based caching
/// strategy.
async fn apply_history_compression(
    state: &AppState,
    headers: &HeaderMap,
    req: &mut ResponsesRequest,
) {
    let items = match &mut req.input {
        ResponsesInput::Items(v) => v,
        ResponsesInput::Text(_) => return,
    };
    let Some(plan) = plan_blocks(items) else {
        return;
    };
    info!(
        item_count = items.len(),
        closed_blocks = plan.closed_blocks,
        block_items_end = plan.block_items_end,
        block_size = HISTORY_BLOCK_SIZE,
        keep_recent = HISTORY_KEEP_RECENT,
        triggered_by = ?plan.triggered_by,
        "frontdoor: history compression planned"
    );

    // Summarise each block. We could parallelise, but a fresh block
    // summarisation is on the hot path (the request is waiting); a
    // serial walk gives predictable latency and keeps the primary
    // slot contended one call at a time.
    let mut block_summaries: Vec<String> = Vec::with_capacity(plan.closed_blocks);
    let mut all_blocks_resolved = true;
    let block_slice_end = plan.block_items_end;

    // Variable-size blocks under ByteBackup: single block spans the
    // whole eligible prefix. Item-count blocks are fixed BLOCK_SIZE.
    let block_size = match plan.triggered_by {
        BlockTrigger::ItemCount => HISTORY_BLOCK_SIZE,
        BlockTrigger::ByteBackup => block_slice_end,
    };

    for block_idx in 0..plan.closed_blocks {
        let block_start = block_idx * block_size;
        let block_end = ((block_idx + 1) * block_size).min(block_slice_end);
        let block_items = &items[block_start..block_end];
        let block_blob = render_items_for_distill(block_items);
        if block_blob.trim().is_empty() {
            // Empty block (rare — Other-only parts). Skip without
            // recording a summary; sister blocks still proceed.
            continue;
        }
        let key = sha256_hex(&block_blob);
        let cached = history_cache().lock().ok().and_then(|m| m.get(&key).cloned());
        let summary = match cached {
            Some(s) => {
                debug!(
                    block_idx,
                    cache_key = %&key[..12],
                    "frontdoor: history block cache hit"
                );
                s
            }
            None => match summarise_block(state, headers, &block_blob, &key, block_idx).await {
                Some(s) => s,
                None => {
                    // Inner call failed — abandon compression rather
                    // than emit a partial summary that misrepresents
                    // the conversation. The model gets the full
                    // verbatim history this turn (slow but correct).
                    all_blocks_resolved = false;
                    break;
                }
            },
        };
        block_summaries.push(summary);
    }

    if !all_blocks_resolved {
        warn!("frontdoor: at least one block summary failed; keeping full history");
        return;
    }
    if block_summaries.is_empty() {
        return;
    }

    let synthetic = render_block_summaries(&block_summaries);
    let summary_item = ResponsesInputItem::Message(MessageItem {
        role: "user".to_string(),
        content: MessageContent::Text(synthetic),
    });
    // Replace the compressed prefix with the single synthetic
    // summary item; items past `block_items_end` stay verbatim.
    items.drain(..plan.block_items_end);
    items.insert(0, summary_item);
}

/// Run the summariser on one block and write the result into the
/// cache. Returns `None` on inner-call failure so the caller can
/// abort compression cleanly.
async fn summarise_block(
    state: &AppState,
    headers: &HeaderMap,
    block_blob: &str,
    key: &str,
    block_idx: usize,
) -> Option<String> {
    let started = std::time::Instant::now();
    let chat_req = ChatCompletionRequest {
        model: Some("primary".to_string()),
        messages: vec![
            ChatMessage::new("system", FRONTDOOR_HISTORY_SYSTEM),
            ChatMessage::new("user", block_blob),
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
    sampling_mode: None,
    };
    let response = chat_completions(State(state.clone()), headers.clone(), Json(chat_req)).await;
    let status = response.status();
    let body = match axum::body::to_bytes(response.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "frontdoor: block summariser body read failed");
            return None;
        }
    };
    if !status.is_success() {
        warn!(status = %status, "frontdoor: block summariser inner-call failed");
        return None;
    }
    let chat: ChatCompletionResponse = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "frontdoor: block summariser response not JSON");
            return None;
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
        warn!("frontdoor: block summariser produced empty output");
        return None;
    }
    if let Ok(mut cache) = history_cache().lock() {
        cache.insert(key.to_string(), summary_text.clone());
    }
    info!(
        block_idx,
        cache_key = %&key[..12],
        elapsed_ms = %started.elapsed().as_millis(),
        summary_bytes = summary_text.len(),
        "frontdoor: history block summarised and cached"
    );
    Some(summary_text)
}

/// Stitch one or more block summaries into the synthetic message
/// body that replaces the compressed prefix.
fn render_block_summaries(summaries: &[String]) -> String {
    let mut out = String::new();
    out.push_str("# Conversation so far (compressed by frontdoor)\n\n");
    if summaries.len() == 1 {
        out.push_str(summaries[0].trim());
    } else {
        for (i, s) in summaries.iter().enumerate() {
            out.push_str(&format!("## Block {} of {}\n\n", i + 1, summaries.len()));
            out.push_str(s.trim());
            out.push_str("\n\n");
        }
    }
    out.push_str("\n\nContinue the work from here.");
    out
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
    fn harness_per_profile_pass_pipeline_is_what_we_expect() {
        // Codex (final Inv #17 outcome, 2026-05-13): catalog filter +
        // grammar lock ON. Distiller and synthetic tools OFF. The
        // distiller iterations (v1..v3) bench-failed: even with
        // JSON-Schema coercion + scrub_paths, the same-slot distiller
        // fabricates wholesale narratives ("agent is stuck on xattr"
        // observed in a session where xattr was never mentioned).
        // Bench-validated path is: catalog filter drops 9/11 codex
        // tools, grammar locks envelope shape, role sampler keeps
        // content bytes correct, model writes apply_patch heredocs
        // via natural exec_command.
        assert!(!Harness::Codex.runs_distiller());
        assert!(Harness::Codex.runs_catalog_filter());
        assert!(!Harness::Codex.runs_synthetic_tools());
        assert!(Harness::Codex.runs_grammar_lock());
        assert!(Harness::Codex.runs_coherence_baseline());

        // Opencode: full reshape (original frontdoor target).
        assert!(Harness::Opencode.runs_distiller());
        assert!(Harness::Opencode.runs_catalog_filter());
        assert!(Harness::Opencode.runs_synthetic_tools());
        assert!(Harness::Opencode.runs_grammar_lock());
        assert!(Harness::Opencode.runs_coherence_baseline());

        // Generic: middle ground — grammar lock for tool-shape
        // discipline, no prompt or catalog surgery.
        assert!(!Harness::Generic.runs_distiller());
        assert!(!Harness::Generic.runs_catalog_filter());
        assert!(!Harness::Generic.runs_synthetic_tools());
        assert!(Harness::Generic.runs_grammar_lock());
        assert!(Harness::Generic.runs_coherence_baseline());

        // Bare: zero interference.
        assert!(!Harness::Bare.runs_distiller());
        assert!(!Harness::Bare.runs_catalog_filter());
        assert!(!Harness::Bare.runs_synthetic_tools());
        assert!(!Harness::Bare.runs_grammar_lock());
        assert!(!Harness::Bare.runs_coherence_baseline());
    }

    #[test]
    fn detect_harness_reads_user_agent_first() {
        use axum::http::HeaderValue;
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("codex_cli_rs/0.130.0"));
        assert_eq!(detect_harness(&h), Harness::Codex);

        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("opencode/2.1.0"));
        assert_eq!(detect_harness(&h), Harness::Opencode);

        // Empty UA + no env override = Generic (Bare requires explicit opt-in).
        let h = HeaderMap::new();
        let prior = std::env::var("SOVEREIGN_HARNESS").ok();
        let prior_fd = std::env::var("SOVEREIGN_FRONTDOOR").ok();
        std::env::remove_var("SOVEREIGN_HARNESS");
        std::env::remove_var("SOVEREIGN_FRONTDOOR");
        assert_eq!(detect_harness(&h), Harness::Generic);

        // SOVEREIGN_HARNESS env wins over UA.
        std::env::set_var("SOVEREIGN_HARNESS", "bare");
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("codex_cli_rs/0.130.0"));
        assert_eq!(detect_harness(&h), Harness::Bare);

        // Legacy SOVEREIGN_FRONTDOOR=1 maps to Opencode when no
        // explicit harness override and no UA hint.
        std::env::remove_var("SOVEREIGN_HARNESS");
        std::env::set_var("SOVEREIGN_FRONTDOOR", "1");
        let h = HeaderMap::new();
        assert_eq!(detect_harness(&h), Harness::Opencode);

        // Restore prior env values.
        match prior {
            Some(v) => std::env::set_var("SOVEREIGN_HARNESS", v),
            None => std::env::remove_var("SOVEREIGN_HARNESS"),
        }
        match prior_fd {
            Some(v) => std::env::set_var("SOVEREIGN_FRONTDOOR", v),
            None => std::env::remove_var("SOVEREIGN_FRONTDOOR"),
        }
    }

    #[test]
    fn items_byte_size_counts_text_args_and_outputs() {
        use crate::responses_types::*;
        let items = vec![
            ResponsesInputItem::Message(MessageItem {
                role: "user".into(),
                content: MessageContent::Text("abc".into()),
            }),
            ResponsesInputItem::Message(MessageItem {
                role: "assistant".into(),
                content: MessageContent::Parts(vec![
                    ResponsesContentPart::InputText { text: "wxyz".into() },
                    ResponsesContentPart::OutputText { text: "123".into() },
                    ResponsesContentPart::Other,
                ]),
            }),
            ResponsesInputItem::FunctionCall(FunctionCallItem {
                call_id: "c1".into(),
                name: "exec_command".into(),
                arguments: "{\"cmd\":\"ls\"}".into(),
                id: None,
            }),
            ResponsesInputItem::FunctionCallOutput(FunctionCallOutputItem {
                call_id: "c1".into(),
                output: "ok".into(),
            }),
        ];
        // Sum: 3 + (4+3+0) + (12+12) + 2 = 36
        assert_eq!(items_byte_size(&items), 3 + 7 + 24 + 2);
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
    fn scrub_paths_replaces_multi_component_absolute_paths() {
        let s = "Create files under /Users/alex/dev/foo/bar.md after reading /etc/passwd.";
        let out = scrub_paths(s);
        assert!(out.contains("<path>"));
        assert!(!out.contains("/Users/alex"));
        assert!(!out.contains("/etc/passwd"));
        // English structure preserved.
        assert!(out.contains("Create files under <path>"));
    }

    #[test]
    fn scrub_paths_leaves_single_component_alone() {
        // `/dev/null` would be stripped (two components), but a
        // lone slash inside a sentence like `with `cat foo`/`bar`
        // shouldn't be aggressively scrubbed when there's no
        // multi-component path. Conservative: requires 2+ components.
        let s = "Use cargo build / cargo test to verify.";
        let out = scrub_paths(s);
        // No path-shape match here — slash isn't followed by a path component
        // because spaces interrupt.
        assert!(out.contains("/"), "single-slash uses with spaces should not be scrubbed");
    }

    #[test]
    fn scrub_paths_strips_typo_paths_too() {
        // The whole point: even mis-typed paths get scrubbed.
        let s = "Workdir is /Users/alexsbryan.dev/tos-experiment-oicp_types — bad path.";
        let out = scrub_paths(s);
        assert!(out.contains("<path>"));
        assert!(!out.contains("alexsbryan.dev"));
    }

    #[test]
    fn canonicalize_apply_patch_fixes_005_malformed_emission() {
        // This is the actual emission captured from gym fixture 005.
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch ***\n\
                   Add File: src/lib.rs\n\
                   +pub fn answer() -> u32 {\n\
                   +    42\n\
                   +}\n\
                   *** End Patch EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        // The three pass-yaml predicates for fixture 005:
        assert!(canonical.contains("apply_patch"), "missing apply_patch opener");
        assert!(
            canonical.contains("*** Add File: src/lib.rs"),
            "missing canonical Add File marker:\n{canonical}"
        );
        assert!(canonical.contains("pub fn answer"), "body content lost");
        // Plus the structural invariants:
        assert!(canonical.contains("*** Begin Patch\n"));
        assert!(canonical.contains("*** End Patch\n"));
        assert!(!canonical.contains("*** Begin Patch ***"));
        assert!(!canonical.contains("*** End Patch EOF"));
        assert!(canonical.ends_with("EOF\n"));
    }

    #[test]
    fn canonicalize_apply_patch_injects_missing_end_patch_marker() {
        // Real codex emission (gym 008 / smoke 2026-05-13): model
        // forgets the `*** End Patch` line before the EOF closer.
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch\n\
                   *** Add File: a.rs\n\
                   +pub fn x() {}\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        assert!(canonical.contains("*** Begin Patch\n"));
        assert!(canonical.contains("*** End Patch\n"));
        assert!(canonical.contains("+pub fn x()"));
        assert!(canonical.ends_with("EOF\n"));
    }

    #[test]
    fn canonicalize_apply_patch_adds_plus_prefix_to_body_lines() {
        // Real codex emission: TOML body lines missing `+` prefix.
        // The canonicalizer prepends `+` to any non-prefixed body
        // line inside an Add File section.
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch\n\
                   *** Add File: Cargo.toml\n\
                   +[package]\n\
                   +name = \"x\"\n\
                   edition = \"2021\"\n\
                   description = \"foo\"\n\
                   +[dependencies]\n\
                   serde = \"1\"\n\
                   *** End Patch\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        assert!(canonical.contains("+edition = \"2021\""), "edition gained +");
        assert!(canonical.contains("+description = \"foo\""), "description gained +");
        assert!(canonical.contains("+serde = \"1\""), "serde gained +");
        // Lines already prefixed are not double-prefixed.
        assert!(!canonical.contains("++"));
    }

    #[test]
    fn canonicalize_apply_patch_repairs_both_missing_end_and_prefixes() {
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch\n\
                   *** Add File: oicp-types/Cargo.toml\n\
                   +[package]\n\
                   +name = \"oicp-types\"\n\
                   +version = \"0.1.0\"\n\
                   edition = \"2021\"\n\
                   description = \"OICP types\"\n\
                   license = \"MIT OR Apache-2.0\"\n\
                   +[dependencies]\n\
                   +serde = { version = \"1\" }\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        // Structural markers present
        assert!(canonical.contains("*** Begin Patch\n"));
        assert!(canonical.contains("*** End Patch\n"));
        // Repair landed on every unprefixed line
        assert!(canonical.contains("+edition"));
        assert!(canonical.contains("+description"));
        assert!(canonical.contains("+license"));
        // Closer present, body comes BEFORE End Patch
        let end_idx = canonical.find("*** End Patch").expect("End Patch present");
        let pre_end = &canonical[..end_idx];
        assert!(pre_end.contains("+license"));
    }

    #[test]
    fn canonicalize_apply_patch_addfile_space_prefix_repaired() {
        // Real codex emission (smoke v4, 2026-05-13): inside an
        // Add File section, body lines emitted with leading
        // whitespace (indentation) were getting through as
        // context lines. Codex's parser rejects context lines
        // in an Add File section. Repair: always force `+`.
        let bad = "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: src/lib.rs\n+pub struct Foo {\n    pub bar: bool,\n    pub baz: f64,\n+}\n*** End Patch\nEOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        // Content preserved with `+` prepended; indentation lives
        // after the `+`.
        assert!(
            canonical.contains("+    pub bar: bool,"),
            "expected `+    pub bar: bool,`; got:\n{canonical}"
        );
        assert!(canonical.contains("+    pub baz: f64,"));
    }

    #[test]
    fn canonicalize_apply_patch_updatefile_space_prefix_kept() {
        // For Update File sections, space-prefix is a context line
        // (codex apply_patch convention) and must be preserved.
        let good = "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: src/lib.rs\n@@ pub fn foo()\n let x = 1;\n-let y = 2;\n+let y = 3;\n*** End Patch\nEOF";
        let canonical = canonicalize_apply_patch_heredoc(good).expect("should canonicalize");
        // Space-prefixed context line preserved verbatim.
        assert!(canonical.contains("\n let x = 1;\n"), "got:\n{canonical}");
        assert!(canonical.contains("\n-let y = 2;\n"));
        assert!(canonical.contains("\n+let y = 3;\n"));
    }

    #[test]
    fn canonicalize_apply_patch_repairs_json_shape_cargo_toml() {
        // Real codex emission (smoke v5, 2026-05-13): model wraps
        // Cargo.toml body in JSON-style braces with trailing commas.
        // Canonicalizer strips the wrappers, commas, and inserts the
        // missing [package] header.
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch\n\
                   *** Add File: oicp-types/Cargo.toml\n\
                   +{\n\
                   +name = \"oicp-types\",\n\
                   +version = \"0.1.0\",\n\
                   +edition = \"2021\",\n\
                   +\n\
                   +[dependencies]\n\
                   +serde = { version = \"1\" }\n\
                   +}\n\
                   *** End Patch\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        // Wrapper `+{` and `+}` lines removed.
        assert!(!canonical.contains("+{\n"), "wrapper open-brace not stripped:\n{canonical}");
        assert!(!canonical.contains("+}\n"));
        // Trailing commas stripped.
        assert!(canonical.contains("+name = \"oicp-types\"\n"));
        assert!(canonical.contains("+version = \"0.1.0\"\n"));
        assert!(canonical.contains("+edition = \"2021\"\n"));
        assert!(!canonical.contains("\","));
        // [package] header injected.
        assert!(canonical.contains("+[package]\n"));
        // [dependencies] preserved.
        assert!(canonical.contains("+[dependencies]\n"));
        // Structural envelope intact.
        assert!(canonical.contains("*** Add File: oicp-types/Cargo.toml\n"));
        assert!(canonical.contains("*** End Patch\n"));
    }

    #[test]
    fn canonicalize_apply_patch_repairs_multiline_inline_table() {
        // Real codex emission (smoke v5, 2026-05-13): model uses
        // multi-line inline-table syntax for `dependencies`, which
        // is invalid TOML. Canonicalizer must rewrite to a section
        // header and drop the wrapping `{`/`}`.
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch\n\
                   *** Add File: oicp-types/Cargo.toml\n\
                   +[package]\n\
                   +name = \"oicp-types\"\n\
                   +version = \"0.1.0\"\n\
                   +\n\
                   +dependencies = {\n\
                   +    serde = { workspace = true }\n\
                   +    thiserror = { workspace = true }\n\
                   +}\n\
                   *** End Patch\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        // Multi-line inline-table replaced with section header.
        assert!(
            canonical.contains("+[dependencies]\n"),
            "[dependencies] section header missing:\n{canonical}"
        );
        // Original `+dependencies = {` gone.
        assert!(
            !canonical.contains("+dependencies = {"),
            "malformed multi-line inline-table still present:\n{canonical}"
        );
        // Inline-tables for individual entries preserved (valid TOML).
        assert!(canonical.contains("+    serde = { workspace = true }"));
        assert!(canonical.contains("+    thiserror = { workspace = true }"));
    }

    #[test]
    fn canonicalize_apply_patch_preserves_single_line_inline_table() {
        // A single-line inline table is valid TOML and must NOT be
        // rewritten as a section header.
        let good = "apply_patch <<'EOF'\n\
                    *** Begin Patch\n\
                    *** Add File: Cargo.toml\n\
                    +[package]\n\
                    +name = \"x\"\n\
                    +deps = { serde = \"1\", tokio = \"1\" }\n\
                    *** End Patch\n\
                    EOF";
        let canonical = canonicalize_apply_patch_heredoc(good).expect("should canonicalize");
        // Single-line inline table kept verbatim.
        assert!(canonical.contains("+deps = { serde = \"1\", tokio = \"1\" }"));
        // No spurious `[deps]` section header.
        assert!(!canonical.contains("[deps]"));
    }

    #[test]
    fn canonicalize_apply_patch_toml_repair_idempotent_when_already_clean() {
        // Already-canonical Cargo.toml passes through unchanged.
        let good = "apply_patch <<'EOF'\n\
                    *** Begin Patch\n\
                    *** Add File: Cargo.toml\n\
                    +[package]\n\
                    +name = \"x\"\n\
                    +version = \"0.1.0\"\n\
                    *** End Patch\n\
                    EOF";
        let canonical = canonicalize_apply_patch_heredoc(good).expect("should canonicalize");
        // No double-injection of [package].
        let header_count = canonical.matches("+[package]").count();
        assert_eq!(header_count, 1, "[package] should appear exactly once:\n{canonical}");
    }

    #[test]
    fn canonicalize_apply_patch_toml_repair_skips_non_toml() {
        // A .rs file with key=value-looking content should NOT get
        // [package] injected.
        let bad = "apply_patch <<'EOF'\n\
                   *** Begin Patch\n\
                   *** Add File: src/lib.rs\n\
                   +pub const NAME: &str = \"x\";\n\
                   +pub const VERSION: u32 = 1;\n\
                   *** End Patch\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        assert!(!canonical.contains("[package]"));
    }

    #[test]
    fn canonicalize_apply_patch_idempotent_on_canonical_input() {
        let good = "apply_patch <<'EOF'\n\
                    *** Begin Patch\n\
                    *** Add File: src/lib.rs\n\
                    +pub fn answer() -> u32 {\n\
                    +    42\n\
                    +}\n\
                    *** End Patch\n\
                    EOF";
        let first = canonicalize_apply_patch_heredoc(good).expect("should accept canonical");
        let second = canonicalize_apply_patch_heredoc(&first).expect("idempotent");
        assert_eq!(first, second, "canonicalizer must be idempotent");
    }

    #[test]
    fn canonicalize_apply_patch_passes_through_non_patch_cmds() {
        // Anything that isn't an apply_patch heredoc must return None
        // so the caller leaves the original cmd untouched.
        assert!(canonicalize_apply_patch_heredoc("ls -la").is_none());
        assert!(canonicalize_apply_patch_heredoc("cargo test").is_none());
        // Heredoc but not apply_patch: pass through.
        assert!(canonicalize_apply_patch_heredoc("cat <<'EOF'\nhello\nEOF").is_none());
    }

    #[test]
    fn canonicalize_apply_patch_handles_unquoted_tag() {
        // Bash heredocs allow unquoted tags. We accept them but always
        // re-emit quoted (`'EOF'`) since that's the safe canonical form
        // (suppresses interpolation of `$VAR` in body content).
        let bad = "apply_patch <<EOF\n\
                   *** Begin Patch\n\
                   *** Add File: a.rs\n\
                   +x\n\
                   *** End Patch\n\
                   EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        assert!(canonical.contains("apply_patch <<'EOF'\n"));
        assert!(canonical.contains("*** Add File: a.rs"));
    }

    #[test]
    fn canonicalize_apply_patch_rejects_empty_patch() {
        // A patch with no file operations isn't actionable — return
        // None so the caller's downstream error path can surface the
        // empty patch rather than silently emitting one.
        let empty = "apply_patch <<'EOF'\n*** Begin Patch\n*** End Patch\nEOF";
        assert!(canonicalize_apply_patch_heredoc(empty).is_none());
    }

    #[test]
    fn canonicalize_apply_patch_preserves_update_file_hunks() {
        let bad = "apply_patch <<'EOF'\n\
                   Begin Patch\n\
                   Update File: src/lib.rs\n\
                   @@ pub fn answer() -> u32 {\n\
                   -    42\n\
                   +    43\n\
                   End Patch EOF";
        let canonical = canonicalize_apply_patch_heredoc(bad).expect("should canonicalize");
        assert!(canonical.contains("*** Update File: src/lib.rs"));
        // Hunk header preserved verbatim — `@@` lines are user data.
        assert!(canonical.contains("@@ pub fn answer"));
        assert!(canonical.contains("-    42"));
        assert!(canonical.contains("+    43"));
    }

    #[test]
    fn canonicalize_chat_response_rewrites_exec_command_args() {
        use crate::openai_types::{FunctionCall, ToolCall};
        let mut tcs = vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "exec_command".into(),
                arguments: r#"{"cmd":"apply_patch <<'EOF'\n*** Begin Patch ***\nAdd File: src/lib.rs\n+pub fn answer() -> u32 {\n+    42\n+}\n*** End Patch EOF"}"#.into(),
            },
        }];
        let fixed = canonicalize_chat_response_tool_calls(&mut tcs);
        assert_eq!(fixed, 1, "expected exactly one canonicalization");
        let new_args: serde_json::Value = serde_json::from_str(&tcs[0].function.arguments).unwrap();
        let new_cmd = new_args["cmd"].as_str().unwrap();
        assert!(new_cmd.contains("*** Add File: src/lib.rs"));
        assert!(!new_cmd.contains("*** Begin Patch ***"));
    }

    fn chat_req_defaults() -> crate::openai_types::ChatCompletionRequest {
        crate::openai_types::ChatCompletionRequest {
            model: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            oicp: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        sampling_mode: None,
        }
    }

    fn make_assistant_tool_call(name: &str, args: &str) -> crate::openai_types::ChatMessage {
        use crate::openai_types::{ChatMessage, FunctionCall, ToolCall};
        ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: format!("call_{}", name),
                kind: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.into(),
                },
            }]),
        }
    }
    fn make_tool_result(call_id: &str, output: &str) -> crate::openai_types::ChatMessage {
        crate::openai_types::ChatMessage {
            role: "tool".to_string(),
            content: output.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls: None,
        }
    }
    fn make_user(content: &str) -> crate::openai_types::ChatMessage {
        crate::openai_types::ChatMessage {
            role: "user".to_string(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn anti_repetition_chat_fires_on_three_identical_tool_calls() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"xattr -d com.apple.provenance x"}"#),
                make_tool_result("call_exec_command", "Operation not permitted"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"xattr -d com.apple.provenance x"}"#),
                make_tool_result("call_exec_command", "Operation not permitted"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"xattr -d com.apple.provenance x"}"#),
                make_tool_result("call_exec_command", "Operation not permitted"),
            ],
            ..chat_req_defaults()
        };
        apply_anti_repetition_chat(&mut req);
        let last = req.messages.last().expect("note appended");
        assert_eq!(last.role, "user");
        assert!(last.content.starts_with(REPETITION_NOTE_PREFIX));
        assert!(last.content.contains("xattr -d com.apple.provenance"));
        assert!(last.content.contains("3 times in a row"));
    }

    #[test]
    fn anti_repetition_chat_no_fire_when_below_threshold() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("call_exec_command", "ok"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("call_exec_command", "ok"),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_anti_repetition_chat(&mut req);
        assert_eq!(req.messages.len(), before, "must not inject below threshold");
    }

    #[test]
    fn anti_repetition_chat_idempotent_when_note_already_present() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("call_exec_command", "ok"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("call_exec_command", "ok"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("call_exec_command", "ok"),
                make_user(&format!("{} prior nudge text", REPETITION_NOTE_PREFIX)),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_anti_repetition_chat(&mut req);
        assert_eq!(req.messages.len(), before, "must not re-inject");
    }

    #[test]
    fn anti_repetition_chat_treats_envelope_shadow_as_transparent() {
        // Fixture 004's actual shape: each xattr emission appears as
        // BOTH a proper tool_calls assistant message AND a separate
        // envelope-as-content assistant message. The envelope shadow
        // must not break the run.
        use crate::openai_types::ChatCompletionRequest;
        let envelope_shadow = crate::openai_types::ChatMessage {
            role: "assistant".to_string(),
            content: "{\n\"name\": \"exec_command\",\n\"arguments\": {\"cmd\": \"xattr -d X\"}\n}".into(),
            tool_call_id: None,
            tool_calls: None,
        };
        let xattr_call = make_assistant_tool_call(
            "exec_command",
            r#"{"cmd":"xattr -d com.apple.provenance x"}"#,
        );
        let result = make_tool_result("c", "Operation not permitted");
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                xattr_call.clone(),
                envelope_shadow.clone(),
                result.clone(),
                xattr_call.clone(),
                envelope_shadow.clone(),
                result.clone(),
                xattr_call.clone(),
                envelope_shadow.clone(),
                result.clone(),
            ],
            ..chat_req_defaults()
        };
        apply_anti_repetition_chat(&mut req);
        let last = req.messages.last().unwrap();
        assert_eq!(last.role, "user");
        assert!(last.content.starts_with(REPETITION_NOTE_PREFIX));
        assert!(last.content.contains("3 times in a row"));
    }

    #[test]
    fn anti_repetition_chat_text_only_assistant_breaks_run() {
        // A real text-only assistant reply (not an envelope shadow) =
        // model genuinely changed strategy; the run should break.
        use crate::openai_types::ChatCompletionRequest;
        let text_reply = crate::openai_types::ChatMessage {
            role: "assistant".to_string(),
            content: "Let me try a different approach.".into(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "err"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "err"),
                text_reply,
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "err"),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_anti_repetition_chat(&mut req);
        assert_eq!(req.messages.len(), before, "text reply breaks the run");
    }

    #[test]
    fn failure_nudge_fires_on_non_zero_exit_tail() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call(
                    "exec_command",
                    r#"{"cmd":"rg 'oicp-v0.2' --files"}"#,
                ),
                make_tool_result(
                    "c",
                    "Chunk ID: abc\nProcess exited with code 2\nOutput:\nrg: oicp-v0.2: No such file",
                ),
            ],
            ..chat_req_defaults()
        };
        apply_failure_nudge_chat(&mut req);
        let last = req.messages.last().unwrap();
        assert_eq!(last.role, "user");
        assert!(last.content.starts_with(FAILURE_NUDGE_PREFIX));
        // The nudge does NOT echo the cmd verbatim — that's what
        // creates the echo attractor in the first place.
        assert!(
            !last.content.contains("oicp-v0.2"),
            "nudge must not echo the failed cmd verbatim:\n{}",
            last.content
        );
        assert!(last.content.contains("exit code 2"));
        // Tool result kept; banner prepended.
        let tool_msg = &req.messages[req.messages.len() - 2];
        assert_eq!(tool_msg.role, "tool");
        assert!(tool_msg.content.starts_with("[FAILURE — exit code 2]"));
        // Banner must NOT echo the cmd verbatim.
        let banner_part = tool_msg.content.split("\n\n").next().unwrap();
        assert!(
            !banner_part.contains("oicp-v0.2"),
            "banner must not echo the failed cmd:\n{}",
            banner_part
        );
        // Original output preserved after the banner.
        assert!(tool_msg.content.contains("No such file"));
        // The failed assistant call has been REMOVED (not redacted).
        // What was: [user, assistant(failed), tool, nudge]
        // Should now be: [user, tool(with banner), nudge]
        assert_eq!(req.messages.len(), 3, "failed call should be removed");
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[1].role, "tool");
        assert_eq!(req.messages[2].role, "user");
        // The failed cmd string must not appear anywhere in the
        // model-visible context except inside the tool result's
        // error body (where it's data, not an echo target).
        for (i, msg) in req.messages.iter().enumerate() {
            if i == 1 {
                continue; // tool result body legitimately includes the err
            }
            assert!(
                !msg.content.contains("rg 'oicp-v0.2' --files"),
                "msg {i} ({}) still contains the failed cmd:\n{}",
                msg.role,
                msg.content
            );
        }
    }

    #[test]
    fn failure_nudge_deletes_envelope_shadow_too() {
        use crate::openai_types::ChatCompletionRequest;
        let envelope = crate::openai_types::ChatMessage {
            role: "assistant".to_string(),
            content: "{\"name\":\"exec_command\",\"arguments\":{\"cmd\":\"rg 'x' --files\"}}".into(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"rg 'x' --files"}"#),
                envelope,
                make_tool_result("c", "Process exited with code 1\nerr"),
            ],
            ..chat_req_defaults()
        };
        apply_failure_nudge_chat(&mut req);
        // Both the call and the envelope shadow should be gone.
        for msg in &req.messages {
            assert!(!msg.content.contains("rg 'x' --files"));
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    assert!(!tc.function.arguments.contains("rg 'x' --files"));
                }
            }
        }
        // Sequence: [user, tool(with banner), nudge]
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[1].role, "tool");
        assert!(req.messages[1].content.starts_with("[FAILURE"));
    }

    #[test]
    fn failure_nudge_no_fire_on_zero_exit_tail() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("c", "Process exited with code 0\nOutput:\nfoo bar"),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_failure_nudge_chat(&mut req);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn failure_nudge_idempotent() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "Process exited with code 1\nerr"),
                make_user(&format!("{} prior nudge", FAILURE_NUDGE_PREFIX)),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_failure_nudge_chat(&mut req);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn failure_nudge_skips_envelope_shadow_to_find_real_call() {
        use crate::openai_types::ChatCompletionRequest;
        let envelope = crate::openai_types::ChatMessage {
            role: "assistant".to_string(),
            content: "{\n\"name\": \"exec_command\",\n\"arguments\": {\"cmd\": \"rg\"}\n}".into(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"rg 'x' --files"}"#),
                envelope, // shadow — skipped
                make_tool_result("c", "Process exited with code 2\nerr"),
            ],
            ..chat_req_defaults()
        };
        apply_failure_nudge_chat(&mut req);
        let last = req.messages.last().unwrap();
        assert!(last.content.starts_with(FAILURE_NUDGE_PREFIX));
        // Real-call's tool_calls are deleted, not redacted —
        // the assistant message is removed entirely.
        for msg in &req.messages {
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    assert!(
                        !tc.function.arguments.contains("rg 'x' --files"),
                        "failed cmd should be gone, found in tool_calls"
                    );
                }
            }
            assert!(
                !msg.content.contains("rg 'x' --files"),
                "failed cmd should be gone, found in content"
            );
        }
    }

    #[test]
    fn anti_repetition_chat_bails_when_failure_nudge_already_present() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "err"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "err"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"x"}"#),
                make_tool_result("c", "err"),
                make_user(&format!("{} prior failure note", FAILURE_NUDGE_PREFIX)),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_anti_repetition_chat(&mut req);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn anti_repetition_chat_breaks_run_on_different_args() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("c", "ok"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"ls"}"#),
                make_tool_result("c", "ok"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"pwd"}"#),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_anti_repetition_chat(&mut req);
        assert_eq!(req.messages.len(), before, "different args break the run");
    }

    #[test]
    fn classify_cmd_recognises_reads_and_actions() {
        assert_eq!(classify_cmd("cat foo.txt"), CmdMode::Read);
        assert_eq!(classify_cmd("ls -la"), CmdMode::Read);
        assert_eq!(classify_cmd("rg 'pattern' --files"), CmdMode::Read);
        assert_eq!(classify_cmd("find . -name '*.rs'"), CmdMode::Read);
        // `cd` is transparent — should fall through to next token.
        assert_eq!(classify_cmd("cd /tmp && cat x"), CmdMode::Read);
        assert_eq!(classify_cmd("cd /tmp && cargo check"), CmdMode::Action);
        // Env var assignment is transparent.
        assert_eq!(classify_cmd("RUST_LOG=debug cargo test"), CmdMode::Action);
        // Action commands.
        assert_eq!(classify_cmd("apply_patch <<'EOF'\n..."), CmdMode::Action);
        assert_eq!(classify_cmd("cargo build"), CmdMode::Action);
        assert_eq!(classify_cmd("mv a b"), CmdMode::Action);
        assert_eq!(classify_cmd("echo hi"), CmdMode::Action);
    }

    #[test]
    fn read_attractor_fires_on_three_reads_zero_actions() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c.md"}"#),
                make_tool_result("c", "..."),
                make_user("now write"),
            ],
            ..chat_req_defaults()
        };
        apply_read_attractor_nudge_chat(&mut req);
        let last = req.messages.last().unwrap();
        // System-role at tail.
        assert_eq!(last.role, "system");
        assert!(last.content.starts_with(READ_ATTRACTOR_NUDGE_PREFIX));
        assert!(last.content.contains("apply_patch"));
        assert!(last.content.contains("Add File"));
        // Original last user message untouched.
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .unwrap();
        assert_eq!(last_user.content, "now write");
    }

    #[test]
    fn read_attractor_no_fire_when_any_action_seen() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cargo build"}"#),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_read_attractor_nudge_chat(&mut req);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn read_attractor_no_fire_below_threshold() {
        use crate::openai_types::ChatCompletionRequest;
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b.md"}"#),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_read_attractor_nudge_chat(&mut req);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn read_attractor_replaces_codex_system_prompt() {
        use crate::openai_types::ChatCompletionRequest;
        let codex_system = crate::openai_types::ChatMessage {
            role: "system".to_string(),
            content: "You are a coding agent running in the Codex CLI, a terminal-\
                      based coding assistant. You should explore the codebase \
                      thoroughly before taking action..."
                .to_string(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                codex_system,
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c"}"#),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        apply_read_attractor_nudge_chat(&mut req);
        // Codex's explore-first system prompt should be replaced.
        let first_sys = req.messages.iter().find(|m| m.role == "system").unwrap();
        assert!(
            !first_sys.content.starts_with("You are a coding agent running in the Codex CLI"),
            "codex system prompt should be replaced when read-attractor fires"
        );
        assert!(first_sys.content.contains("apply_patch"));
        assert!(first_sys.content.contains("only legal next action"));
    }

    #[test]
    fn read_attractor_leaves_non_codex_system_prompts_alone() {
        use crate::openai_types::ChatCompletionRequest;
        let custom_system = crate::openai_types::ChatMessage {
            role: "system".to_string(),
            content: "You are a helpful assistant. Be concise.".to_string(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                custom_system.clone(),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c"}"#),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        apply_read_attractor_nudge_chat(&mut req);
        // Non-codex system prompt survives.
        let kept = req
            .messages
            .iter()
            .find(|m| m.role == "system" && m.content.contains("helpful assistant"));
        assert!(kept.is_some());
    }

    #[test]
    fn read_attractor_rewrites_compressed_history_user_msg() {
        use crate::openai_types::ChatCompletionRequest;
        let compressed = crate::openai_types::ChatMessage {
            role: "user".to_string(),
            content:
                "# Conversation so far (compressed by frontdoor)\n\n## Block 1 of 3\n\n\
                 The user wants me to implement the oicp-types crate per spec. The \
                 agent has executed `cat` on the spec file three times. Now reading \
                 ARCHITECTURE.md for orientation."
                    .to_string(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                compressed,
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b.md"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c.md"}"#),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        apply_read_attractor_nudge_chat(&mut req);
        // No message starts with the frontdoor's compression banner.
        for msg in &req.messages {
            assert!(
                !msg.content.starts_with("# Conversation so far (compressed by frontdoor)"),
                "compressed-history header should be gone after rewrite"
            );
        }
        // The rewritten message preserves the task seed and adds
        // pivot directive.
        let user_with_task = req
            .messages
            .iter()
            .find(|m| m.role == "user" && m.content.contains("apply_patch"))
            .expect("rewritten user msg should exist");
        assert!(user_with_task.content.contains("implement the oicp-types"));
        // Read-recap text is gone.
        assert!(
            !user_with_task.content.contains("executed `cat`"),
            "read-pattern recap should be stripped"
        );
        // Trailing system nudge present.
        let last = req.messages.last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.starts_with(READ_ATTRACTOR_NUDGE_PREFIX));
    }

    #[test]
    fn extract_task_seed_pulls_first_sentence_after_anchor() {
        let s = "# Conversation so far (compressed by frontdoor)\n\n## Block 1 of 5\n\n\
                 The user wants me to implement the oicp-types crate. I have been \
                 reading the spec.";
        let seed = extract_task_seed_from_compressed_history(s);
        assert!(seed.contains("implement the oicp-types crate"));
        assert!(!seed.contains("I have been reading"));
    }

    #[test]
    fn extract_task_seed_fallback_when_anchor_missing() {
        let s = "Some random text without the anchor phrase.";
        let seed = extract_task_seed_from_compressed_history(s);
        assert!(seed.contains("complete the implementation task"));
    }

    #[test]
    fn read_attractor_leaves_non_frontdoor_user_msgs_alone() {
        use crate::openai_types::ChatCompletionRequest;
        // A user message that mentions "compressed" but isn't ours
        // must NOT be deleted.
        let user_real = crate::openai_types::ChatMessage {
            role: "user".to_string(),
            content: "I have compressed the file. Now what?".to_string(),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                user_real.clone(),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c"}"#),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        apply_read_attractor_nudge_chat(&mut req);
        // Non-frontdoor user msg survives.
        let kept_user_count = req
            .messages
            .iter()
            .filter(|m| m.role == "user" && m.content.contains("I have compressed"))
            .count();
        assert_eq!(kept_user_count, 1);
    }

    #[test]
    fn read_attractor_counts_envelope_shadows_too() {
        use crate::openai_types::ChatCompletionRequest;
        let env = |c: &str| crate::openai_types::ChatMessage {
            role: "assistant".to_string(),
            content: format!(
                "{{\"name\":\"exec_command\",\"arguments\":{{\"cmd\":\"{c}\"}}}}"
            ),
            tool_call_id: None,
            tool_calls: None,
        };
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                env("cat a.md"),
                make_tool_result("c", "..."),
                env("cat b.md"),
                make_tool_result("c", "..."),
                env("cat c.md"),
                make_tool_result("c", "..."),
            ],
            ..chat_req_defaults()
        };
        apply_read_attractor_nudge_chat(&mut req);
        let last = req.messages.last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.starts_with(READ_ATTRACTOR_NUDGE_PREFIX));
    }

    #[test]
    fn read_attractor_idempotent() {
        use crate::openai_types::ChatCompletionRequest;
        // System-role nudge already present at tail.
        let mut req = ChatCompletionRequest {
            messages: vec![
                make_user("task"),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat a"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat b"}"#),
                make_tool_result("c", "..."),
                make_assistant_tool_call("exec_command", r#"{"cmd":"cat c"}"#),
                make_tool_result("c", "..."),
                crate::openai_types::ChatMessage {
                    role: "system".to_string(),
                    content: format!("{} prior nudge", READ_ATTRACTOR_NUDGE_PREFIX),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ],
            ..chat_req_defaults()
        };
        let before = req.messages.len();
        apply_read_attractor_nudge_chat(&mut req);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn extract_absolute_paths_finds_multi_component_paths() {
        let s = "Read /Users/alex/dev/foo.md after running cmd /etc/passwd; ignore ./relative.";
        let paths = extract_absolute_paths(s);
        assert!(paths.contains(&"/Users/alex/dev/foo.md".to_string()));
        assert!(paths.contains(&"/etc/passwd".to_string()));
        // Relative path with `./` is not absolute — skipped.
        assert!(!paths.iter().any(|p| p.contains("relative")));
    }

    fn make_components(entries: &[(&str, usize)]) -> std::collections::HashMap<String, usize> {
        entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn find_canonical_component_picks_higher_frequency_similar() {
        let ctx = make_components(&[
            ("atos-experiment-oicp-types", 3),
            ("Users", 4),
        ]);
        // Drift drops the leading 'a'. Typo absent from context →
        // any similar component wins. Found: atos-experiment.
        let canonical = find_canonical_component("tos-experiment-oicp-types", &ctx);
        assert_eq!(
            canonical.map(|s| s.as_str()),
            Some("atos-experiment-oicp-types")
        );
    }

    #[test]
    fn find_canonical_component_rewrites_when_typo_also_appears_but_less_frequent() {
        // Real gym 003 case: a compressed history block recites the
        // typo'd path once, while the correct path appears 3x. We
        // need the canonical (frequent) form to win.
        let ctx = make_components(&[
            ("atos-experiment-oicp-types", 3),
            ("tos-experiment-oicp-types", 1),
        ]);
        let canonical = find_canonical_component("tos-experiment-oicp-types", &ctx);
        assert_eq!(
            canonical.map(|s| s.as_str()),
            Some("atos-experiment-oicp-types")
        );
    }

    #[test]
    fn find_canonical_component_no_rewrite_when_typo_well_attested() {
        // If the "typo" actually appears as often as or more than the
        // alternative, treat it as intentional — don't rewrite.
        let ctx = make_components(&[
            ("atos-experiment-oicp-types", 2),
            ("tos-experiment-oicp-types", 2),
        ]);
        assert!(find_canonical_component("tos-experiment-oicp-types", &ctx).is_none());
    }

    #[test]
    fn find_canonical_component_skips_short_components() {
        let ctx = make_components(&[("Users", 1), ("dev", 1)]);
        // `tmp` is too short — risk of false matches. Skip.
        assert!(find_canonical_component("tmp", &ctx).is_none());
    }

    #[test]
    fn find_canonical_component_no_op_when_only_self_in_context() {
        let ctx = make_components(&[("commonwealth-ai", 5)]);
        assert!(find_canonical_component("commonwealth-ai", &ctx).is_none());
    }

    #[test]
    fn canonicalize_paths_in_cmd_rewrites_typo() {
        let ctx = make_components(&[
            ("Users", 3),
            ("alexsbryan", 3),
            ("dev", 3),
            ("atos-experiment-oicp-types", 3),
            ("oicp-v0.3.md", 2),
        ]);
        let bad =
            "cat /Users/alexsbryan/dev/tos-experiment-oicp-types/oicp-v0.3.md";
        let fixed = canonicalize_paths_in_cmd(bad, &ctx).expect("should rewrite");
        assert!(fixed.contains("atos-experiment-oicp-types"));
        assert!(!fixed.contains("/tos-experiment-oicp-types"));
    }

    #[test]
    fn canonicalize_paths_in_cmd_noop_when_no_typo() {
        let ctx = make_components(&[("etc", 1), ("passwd", 1)]);
        assert!(canonicalize_paths_in_cmd("cat /etc/passwd", &ctx).is_none());
    }

    #[test]
    fn canonicalize_paths_in_cmd_noop_when_no_similar_context_path() {
        let ctx = make_components(&[
            ("usr", 1),
            ("local", 1),
            ("bin", 1),
            ("python", 1),
        ]);
        assert!(canonicalize_paths_in_cmd("ls /tmp/foo/bar", &ctx).is_none());
    }

    #[test]
    fn canonicalize_chat_response_paths_end_to_end() {
        use crate::openai_types::{FunctionCall, ToolCall};
        let ctx = make_components(&[
            ("Users", 3),
            ("alex", 3),
            ("atos-experiment-oicp-types", 3),
            ("foo.md", 1),
        ]);
        let mut tcs = vec![ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "exec_command".into(),
                arguments: r#"{"cmd":"cat /Users/alex/tos-experiment-oicp-types/foo.md"}"#
                    .into(),
            },
        }];
        let fixed = canonicalize_chat_response_paths(&mut tcs, &ctx);
        assert_eq!(fixed, 1);
        let parsed: serde_json::Value =
            serde_json::from_str(&tcs[0].function.arguments).unwrap();
        let cmd = parsed["cmd"].as_str().unwrap();
        assert!(cmd.contains("atos-experiment-oicp-types"));
        assert!(!cmd.contains("/tos-experiment-oicp-types"));
    }

    #[test]
    fn gather_context_paths_pulls_from_content_and_tool_calls() {
        let mut req_msgs = vec![
            make_user("Spec at /Users/alex/foo/bar.md"),
            make_assistant_tool_call(
                "exec_command",
                r#"{"cmd":"cat /Users/alex/baz/qux.md"}"#,
            ),
        ];
        // Add a content with a path too.
        req_msgs.push(crate::openai_types::ChatMessage {
            role: "tool".to_string(),
            content: "result from /opt/data/file".to_string(),
            tool_call_id: None,
            tool_calls: None,
        });
        let paths = gather_context_paths(&req_msgs);
        assert!(paths.contains("/Users/alex/foo/bar.md"));
        assert!(paths.contains("/Users/alex/baz/qux.md"));
        assert!(paths.contains("/opt/data/file"));
    }

    #[test]
    fn canonicalize_chat_response_skips_non_exec_command() {
        use crate::openai_types::{FunctionCall, ToolCall};
        let mut tcs = vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"apply_patch"}"#.into(),
            },
        }];
        let fixed = canonicalize_chat_response_tool_calls(&mut tcs);
        assert_eq!(fixed, 0);
        assert_eq!(tcs[0].function.arguments, r#"{"query":"apply_patch"}"#);
    }

    #[test]
    fn scrub_paths_handles_multiple_paths_per_string() {
        let s = "Read /a/b then write to /c/d and verify /e/f exists.";
        let out = scrub_paths(s);
        assert_eq!(out.matches("<path>").count(), 3);
    }

    #[test]
    fn parse_directive_round_trips_well_formed_object() {
        let raw = r#"{
            "task": "implement Capability enum",
            "constraints": "use serde",
            "done_when": "cargo test passes"
        }"#;
        let d = parse_directive(raw).unwrap();
        assert_eq!(d.task, "implement Capability enum");
        assert_eq!(d.constraints, "use serde");
        assert_eq!(d.done_when, "cargo test passes");
    }

    #[test]
    fn parse_directive_tolerates_unknown_extra_fields() {
        // Inv #17 dropped `files_to_touch`. Older distiller emissions
        // (or models that hallucinate the old field) must still parse
        // — serde's `#[serde(default)]` ignores unknown JSON keys by
        // default for this struct.
        let raw = r#"{"task":"x","constraints":"","done_when":"y","files_to_touch":["/abs/lib.rs"]}"#;
        let d = parse_directive(raw).unwrap();
        assert_eq!(d.task, "x");
    }

    #[test]
    fn parse_directive_tolerates_leading_think_block() {
        let raw = "<think>let me think...</think>\n{\"task\":\"x\",\"constraints\":\"\",\"done_when\":\"y\"}";
        let d = parse_directive(raw).unwrap();
        assert_eq!(d.task, "x");
    }

    #[test]
    fn parse_directive_tolerates_prose_after_json() {
        let raw = r#"{"task":"x","constraints":"","done_when":"y"}

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
        };
        let r = d.render(ToolPolicyShape::ApplyPatchHeredoc);
        assert!(r.contains("## Task"));
        assert!(r.contains("## Done when"));
        assert!(!r.contains("## Constraints"));
        // Tool usage policy block always renders.
        assert!(r.contains("## Tool usage policy"));
    }

    #[test]
    fn render_codex_policy_uses_apply_patch_heredoc() {
        let d = DistilledDirective {
            task: "x".into(),
            constraints: "".into(),
            done_when: "y".into(),
        };
        let r = d.render(ToolPolicyShape::ApplyPatchHeredoc);
        assert!(r.contains("apply_patch"));
        assert!(r.contains("*** Begin Patch"));
        assert!(!r.contains("write_file(path, content)"));
    }

    #[test]
    fn render_opencode_policy_uses_synthetic_tools() {
        let d = DistilledDirective {
            task: "x".into(),
            constraints: "".into(),
            done_when: "y".into(),
        };
        let r = d.render(ToolPolicyShape::SyntheticFileTools);
        assert!(r.contains("write_file(path, content)"));
        assert!(!r.contains("apply_patch"));
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

    fn mk_msg(role: &str, content: &str) -> ResponsesInputItem {
        ResponsesInputItem::Message(MessageItem {
            role: role.to_string(),
            content: MessageContent::Text(content.to_string()),
        })
    }

    #[test]
    fn plan_blocks_none_when_under_keep_recent() {
        let items: Vec<ResponsesInputItem> =
            (0..HISTORY_KEEP_RECENT).map(|i| mk_msg("user", &format!("t{i}"))).collect();
        assert!(plan_blocks(&items).is_none());
    }

    #[test]
    fn plan_blocks_none_when_no_full_block_and_under_byte_trigger() {
        // KEEP_RECENT + a few extras but small bodies → no compression
        let items: Vec<ResponsesInputItem> = (0..HISTORY_KEEP_RECENT + 3)
            .map(|i| mk_msg("user", &format!("t{i}")))
            .collect();
        assert!(plan_blocks(&items).is_none());
    }

    #[test]
    fn plan_blocks_closes_one_block_at_boundary() {
        // KEEP_RECENT + BLOCK_SIZE items → exactly 1 closed block
        let n = HISTORY_KEEP_RECENT + HISTORY_BLOCK_SIZE;
        let items: Vec<ResponsesInputItem> =
            (0..n).map(|i| mk_msg("user", &format!("t{i}"))).collect();
        let plan = plan_blocks(&items).expect("should plan a block");
        assert_eq!(plan.closed_blocks, 1);
        assert_eq!(plan.block_items_end, HISTORY_BLOCK_SIZE);
        assert_eq!(plan.triggered_by, BlockTrigger::ItemCount);
    }

    #[test]
    fn plan_blocks_partial_block_stays_verbatim() {
        // 1 closed block + 3 extras + KEEP_RECENT recent items.
        let n = HISTORY_KEEP_RECENT + HISTORY_BLOCK_SIZE + 3;
        let items: Vec<ResponsesInputItem> =
            (0..n).map(|i| mk_msg("user", &format!("t{i}"))).collect();
        let plan = plan_blocks(&items).expect("should plan a block");
        assert_eq!(plan.closed_blocks, 1, "only complete blocks compress");
        // Partial-block items stay verbatim along with the recent
        // window — only the first BLOCK_SIZE items get compressed.
        assert_eq!(plan.block_items_end, HISTORY_BLOCK_SIZE);
    }

    #[test]
    fn plan_blocks_closes_multiple_blocks() {
        // 3 full blocks + recent window.
        let n = HISTORY_KEEP_RECENT + 3 * HISTORY_BLOCK_SIZE;
        let items: Vec<ResponsesInputItem> =
            (0..n).map(|i| mk_msg("user", &format!("t{i}"))).collect();
        let plan = plan_blocks(&items).expect("should plan blocks");
        assert_eq!(plan.closed_blocks, 3);
        assert_eq!(plan.block_items_end, 3 * HISTORY_BLOCK_SIZE);
    }

    #[test]
    fn plan_blocks_byte_backup_triggers_on_short_but_heavy_conversation() {
        // Short conversation (no full block) but one heavy tool result
        // pushes the eligible prefix over HISTORY_COMPRESS_BYTES.
        let mut items: Vec<ResponsesInputItem> = (0..HISTORY_KEEP_RECENT)
            .map(|i| mk_msg("user", &format!("t{i}")))
            .collect();
        items.insert(0, mk_msg("user", &"X".repeat(HISTORY_COMPRESS_BYTES + 1)));
        items.insert(1, mk_msg("assistant", "ok"));
        items.insert(2, mk_msg("user", "follow"));
        let plan = plan_blocks(&items).expect("byte backup should fire");
        assert_eq!(plan.triggered_by, BlockTrigger::ByteBackup);
        assert_eq!(plan.closed_blocks, 1);
        // Eligible prefix = all items minus KEEP_RECENT
        assert_eq!(
            plan.block_items_end,
            items.len() - HISTORY_KEEP_RECENT
        );
    }

    #[test]
    fn render_block_summaries_single_block_no_section_headers() {
        let out = render_block_summaries(&["alpha summary".to_string()]);
        assert!(out.contains("# Conversation so far"));
        assert!(out.contains("alpha summary"));
        assert!(!out.contains("Block 1 of 1"));
    }

    #[test]
    fn render_block_summaries_multi_block_labels_each() {
        let out = render_block_summaries(&[
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ]);
        assert!(out.contains("## Block 1 of 3"));
        assert!(out.contains("## Block 2 of 3"));
        assert!(out.contains("## Block 3 of 3"));
        // Ordering preserved.
        let a = out.find("alpha").unwrap();
        let b = out.find("beta").unwrap();
        let g = out.find("gamma").unwrap();
        assert!(a < b && b < g);
    }

    fn mk_fn_call(name: &str, args: &str) -> ResponsesInputItem {
        use crate::responses_types::FunctionCallItem;
        ResponsesInputItem::FunctionCall(FunctionCallItem {
            call_id: format!("call_{}", name),
            name: name.to_string(),
            arguments: args.to_string(),
            id: None,
        })
    }
    fn mk_fn_out(call_id: &str, output: &str) -> ResponsesInputItem {
        use crate::responses_types::FunctionCallOutputItem;
        ResponsesInputItem::FunctionCallOutput(FunctionCallOutputItem {
            call_id: call_id.to_string(),
            output: output.to_string(),
        })
    }
    fn mk_req_with(items: Vec<ResponsesInputItem>) -> ResponsesRequest {
        ResponsesRequest {
            model: None,
            input: ResponsesInput::Items(items),
            instructions: None,
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
        }
    }

    #[test]
    fn anti_repetition_no_op_under_threshold() {
        let mut req = mk_req_with(vec![
            mk_msg("user", "go"),
            mk_fn_call("exec_command", r#"{"cmd":"ls"}"#),
            mk_fn_out("call_exec_command", "a\nb"),
            mk_fn_call("exec_command", r#"{"cmd":"ls"}"#),
            mk_fn_out("call_exec_command", "a\nb"),
        ]);
        let before_len = match &req.input {
            ResponsesInput::Items(v) => v.len(),
            _ => 0,
        };
        apply_anti_repetition(&mut req);
        let after_len = match &req.input {
            ResponsesInput::Items(v) => v.len(),
            _ => 0,
        };
        assert_eq!(before_len, after_len, "2 < 3 threshold, no-op");
    }

    #[test]
    fn anti_repetition_fires_at_threshold() {
        let mut req = mk_req_with(vec![
            mk_msg("user", "go"),
            mk_fn_call("exec_command", r#"{"cmd":"rg 'oicp-v0.2' --files"}"#),
            mk_fn_out("call_exec_command", "error"),
            mk_fn_call("exec_command", r#"{"cmd":"rg 'oicp-v0.2' --files"}"#),
            mk_fn_out("call_exec_command", "error"),
            mk_fn_call("exec_command", r#"{"cmd":"rg 'oicp-v0.2' --files"}"#),
            mk_fn_out("call_exec_command", "error"),
        ]);
        apply_anti_repetition(&mut req);
        let items = match &req.input {
            ResponsesInput::Items(v) => v,
            _ => panic!("expected items"),
        };
        let last = items.last().expect("at least one item");
        let last_text = match last {
            ResponsesInputItem::Message(m) => match &m.content {
                MessageContent::Text(t) => t.clone(),
                _ => panic!("text content"),
            },
            _ => panic!("expected message"),
        };
        assert!(
            last_text.starts_with("[anti-repetition note from runtime]"),
            "synthetic nudge appended"
        );
        assert!(last_text.contains("3 times"), "run length surfaced");
        assert!(last_text.contains("exec_command"), "tool name surfaced");
        assert!(last_text.contains("rg 'oicp-v0.2'"), "args surfaced");
    }

    #[test]
    fn anti_repetition_idempotent_when_note_already_present() {
        let mut req = mk_req_with(vec![
            mk_fn_call("exec_command", r#"{"cmd":"x"}"#),
            mk_fn_out("call_exec_command", ""),
            mk_fn_call("exec_command", r#"{"cmd":"x"}"#),
            mk_fn_out("call_exec_command", ""),
            mk_fn_call("exec_command", r#"{"cmd":"x"}"#),
            mk_fn_out("call_exec_command", ""),
        ]);
        apply_anti_repetition(&mut req);
        let n1 = match &req.input {
            ResponsesInput::Items(v) => v.len(),
            _ => 0,
        };
        apply_anti_repetition(&mut req);
        let n2 = match &req.input {
            ResponsesInput::Items(v) => v.len(),
            _ => 0,
        };
        assert_eq!(n1, n2, "second call should be no-op");
    }

    #[test]
    fn anti_repetition_breaks_run_at_different_args() {
        let mut req = mk_req_with(vec![
            mk_fn_call("exec_command", r#"{"cmd":"a"}"#),
            mk_fn_out("c", ""),
            mk_fn_call("exec_command", r#"{"cmd":"b"}"#),
            mk_fn_out("c", ""),
            mk_fn_call("exec_command", r#"{"cmd":"b"}"#),
            mk_fn_out("c", ""),
            mk_fn_call("exec_command", r#"{"cmd":"b"}"#),
            mk_fn_out("c", ""),
        ]);
        apply_anti_repetition(&mut req);
        let items = match &req.input {
            ResponsesInput::Items(v) => v,
            _ => panic!("items"),
        };
        // 3 identical "b" calls at the tail → note fires for "b"
        let last_text = match items.last().unwrap() {
            ResponsesInputItem::Message(m) => match &m.content {
                MessageContent::Text(t) => t.clone(),
                _ => panic!(),
            },
            _ => panic!("expected note"),
        };
        assert!(last_text.contains(r#"{\"cmd\":\"b\"}"#)
            || last_text.contains(r#"{"cmd":"b"}"#));
    }

    #[test]
    fn heredoc_diagnostics_none_for_non_heredoc_exec() {
        let args = serde_json::json!({"cmd": "cargo test --package oicp-types"}).to_string();
        assert!(extract_heredoc_diagnostics(&args).is_none());
    }

    #[test]
    fn heredoc_diagnostics_none_for_malformed_args() {
        assert!(extract_heredoc_diagnostics("not json").is_none());
        assert!(extract_heredoc_diagnostics("{\"no_cmd\":1}").is_none());
    }

    #[test]
    fn heredoc_diagnostics_extracts_apply_patch_shape() {
        let cmd = "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: /tmp/foo.rs\n+pub fn x() {}\n*** End Patch\nEOF\n";
        let args = serde_json::json!({"cmd": cmd}).to_string();
        let d = extract_heredoc_diagnostics(&args).expect("heredoc detected");
        assert_eq!(d.delimiter, "EOF");
        assert!(d.quoted_delimiter);
        assert!(d.begin_patch);
        assert!(d.end_patch);
        assert_eq!(d.add_files, 1);
        assert_eq!(d.update_files, 0);
        assert_eq!(d.delete_files, 0);
        assert_eq!(d.escape_quote_count, 0);
        assert_eq!(d.escape_backslash_count, 0);
        assert!(d.closed);
    }

    #[test]
    fn heredoc_diagnostics_counts_escape_coherence_smells() {
        let cmd = "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: /tmp/a.rs\n+let s = \\\"hello\\\";\n+let p = \\\\;\n*** End Patch\nEOF\n";
        let args = serde_json::json!({"cmd": cmd}).to_string();
        let d = extract_heredoc_diagnostics(&args).expect("heredoc detected");
        assert_eq!(d.escape_quote_count, 2, "two `\\\"` sequences in body");
        assert_eq!(d.escape_backslash_count, 1, "one `\\\\` sequence in body");
    }

    #[test]
    fn heredoc_diagnostics_unterminated_marked_not_closed() {
        let cmd = "apply_patch <<EOF\n*** Begin Patch\n*** Add File: /tmp/a.rs\n+x";
        let args = serde_json::json!({"cmd": cmd}).to_string();
        let d = extract_heredoc_diagnostics(&args).expect("heredoc detected");
        assert!(!d.closed);
        assert!(!d.quoted_delimiter);
        assert_eq!(d.delimiter, "EOF");
    }

    #[test]
    fn heredoc_diagnostics_handles_double_quoted_delimiter() {
        let cmd = "apply_patch <<\"PATCH\"\n*** Begin Patch\n*** Add File: /tmp/a.rs\n+ok\n*** End Patch\nPATCH\n";
        let args = serde_json::json!({"cmd": cmd}).to_string();
        let d = extract_heredoc_diagnostics(&args).expect("heredoc detected");
        assert_eq!(d.delimiter, "PATCH");
        assert!(d.quoted_delimiter);
        assert!(d.closed);
    }

    // Single combined test: the env var read inside
    // apply_brief_from_env_path is process-global state; splitting
    // cases would race under cargo test's default parallelism
    // (same pattern as `is_enabled_env_var_semantics` above). The
    // env-var name is per-test-unique so cases inside this test do
    // not race against the production `SOVEREIGN_CODEX_BRIEF` name
    // either.
    #[test]
    fn apply_brief_from_env_path_semantics() {
        let env_name = "SOVEREIGN_BRIEF_TEST_VAR";
        let prior = std::env::var(env_name).ok();
        let mk_req = |instr: Option<&str>| ResponsesRequest {
            model: None,
            input: ResponsesInput::Text("hi".into()),
            instructions: instr.map(|s| s.to_string()),
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

        // Unset env: no-op.
        std::env::remove_var(env_name);
        let mut req = mk_req(Some("original"));
        apply_brief_from_env_path(&mut req, env_name);
        assert_eq!(req.instructions.as_deref(), Some("original"));

        // Env set but path does not exist: no-op + warn (request
        // passes through unchanged).
        std::env::set_var(env_name, "/tmp/__nonexistent_brief_file__xyzzy.md");
        let mut req = mk_req(Some("kept"));
        apply_brief_from_env_path(&mut req, env_name);
        assert_eq!(req.instructions.as_deref(), Some("kept"));

        // Env set to a real file with content: brief prepends.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "sovereign_brief_test_{}.md",
            std::process::id()
        ));
        std::fs::write(&path, "BRIEF CONTENT\n- rule one\n- rule two\n").unwrap();
        std::env::set_var(env_name, path.to_str().unwrap());

        let mut req = mk_req(Some("be terse"));
        apply_brief_from_env_path(&mut req, env_name);
        let after_first = req.instructions.clone().expect("instructions set");
        assert!(after_first.starts_with("BRIEF CONTENT"));
        assert!(after_first.contains("be terse"));
        assert!(after_first.contains("rule one"));

        // Idempotent: second call leaves request unchanged.
        apply_brief_from_env_path(&mut req, env_name);
        assert_eq!(req.instructions.as_deref(), Some(after_first.as_str()));

        // No prior instructions: brief is the entire instructions
        // value (trailing newline trimmed).
        let mut req = mk_req(None);
        apply_brief_from_env_path(&mut req, env_name);
        assert_eq!(
            req.instructions.as_deref(),
            Some("BRIEF CONTENT\n- rule one\n- rule two")
        );

        // Whitespace-only brief: treated as no-op.
        std::fs::write(&path, "   \n\n\t\n").unwrap();
        let mut req = mk_req(Some("untouched"));
        apply_brief_from_env_path(&mut req, env_name);
        assert_eq!(req.instructions.as_deref(), Some("untouched"));

        // Cleanup + restore.
        let _ = std::fs::remove_file(&path);
        match prior {
            Some(v) => std::env::set_var(env_name, v),
            None => std::env::remove_var(env_name),
        }
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
