//! Synthetic file-I/O tool reshaping — the codex catalog-augmentation
//! layer extracted from `routes_responses.rs` (PR6).
//!
//! Codex ships a shell-only file-write path (`exec_command`); local
//! models hallucinate `write_file` / `read_file` primitives instead.
//! We inject those synthetic tools into the catalog the model sees, then
//! rewrite each emitted call into the equivalent `exec_command` shell
//! invocation before the SSE event reaches codex's router. This module
//! owns that injection (`synthetic_file_tools`) + rewrite
//! (`rewrite_synthetic_tool_call`) + the POSIX-quoting helpers.
//!
//! Behaviour is unchanged from the in-file version — this is a pure
//! relocation (PR6). The frontdoor-side path/heredoc canonicalizers
//! remain in `frontdoor.rs` (already shared via `frontdoor::`); folding
//! them in here is a follow-up to run under `scripts/harness-replay.sh`.

use crate::openai_types::{ToolDefinition, ToolFunction};
use crate::responses_types::ResponsesInputItem;

// ─── Synthetic file-I/O tools ───────────────────────────────────────
//
// Why this exists: codex 0.130 ships an 11-tool catalog whose only
// file-write path is `exec_command` (shell). Local 35B-A3B models
// pass 77/80 on the cognitive bank with a curated 13-tool menu that
// includes Read/Edit/Write/Grep, but on codex's catalog they
// hallucinate names (`write`, `read_file`) because their training
// prior expects those primitives. Two consequences:
//
//   1. The model's first emit picks a hallucinated tool name; codex's
//      router rejects with `unsupported call: <name>`.
//   2. When the model falls back to `exec_command` with a heredoc to
//      write a multi-KB plan, the JSON-escape of the inner shell
//      script breaks (`error=failed to parse function arguments:
//      invalid escape at line 1 column 23`).
//
// The fix is *catalog augmentation*. We inject `write_file(path,
// content)` and `read_file(path)` into the catalog the model sees.
// The model emits clean JSON envelopes (no shell escapes, no
// heredocs). The adapter then rewrites the tool_call into an
// equivalent `exec_command` call before the SSE event reaches codex's
// router, which dispatches against its real handler.
//
// The shell synthesis for `write_file` uses POSIX single-quote
// quoting on both path and content — newlines and special chars
// survive intact because everything inside `'...'` is literal in sh,
// and any literal `'` in the content is escaped via the standard
// `'\''` sequence.

pub(crate) const SYNTHETIC_TOOL_WRITE_FILE: &str = "write_file";
pub(crate) const SYNTHETIC_TOOL_READ_FILE: &str = "read_file";
pub(crate) const SYNTHETIC_TOOL_WRITE_FILE_BEGIN: &str = "write_file_begin";
pub(crate) const SYNTHETIC_TOOL_WRITE_FILE_CHUNK: &str = "write_file_chunk";
pub(crate) const SYNTHETIC_TOOL_WRITE_FILE_END: &str = "write_file_end";

/// True when the most recent assistant tool emission in `items` was
/// `write_file_begin` or `write_file_chunk` — i.e. the model is
/// mid-chunked-write and the next turn should produce another chunk
/// or `write_file_end`. Walks backwards from the end so prior
/// chunked-write sessions earlier in the conversation don't trigger.
///
/// Used by `translate_request` to filter the outbound tool catalog
/// down to `[write_file_chunk, write_file_end]` and promote
/// `tool_choice` to `"required"`, which engages the inference
/// adapter's tool-envelope grammar over the next emission. The
/// decoder physically cannot emit malformed JSON args under that
/// constraint, closing the over-escape / mid-string-control-char
/// failure class observed on multi-KB chunked writes.
pub(crate) fn in_chunked_write_state(items: &[ResponsesInputItem]) -> bool {
    for item in items.iter().rev() {
        if let ResponsesInputItem::FunctionCall(fc) = item {
            return matches!(
                fc.name.as_str(),
                SYNTHETIC_TOOL_WRITE_FILE_BEGIN | SYNTHETIC_TOOL_WRITE_FILE_CHUNK
            );
        }
    }
    false
}

pub(crate) fn synthetic_file_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolFunction {
                name: SYNTHETIC_TOOL_WRITE_FILE.to_string(),
                description: Some(
                    "Write SHORT text content (≤400 bytes) to a file on disk. Overwrites if \
                     the file exists. For LARGER content use the chunked write protocol: \
                     `write_file_begin(path)` → repeated `write_file_chunk(path, chunk)` (one \
                     150–250 byte chunk per call) → `write_file_end(path)`. Chunking keeps each \
                     tool envelope small and is far more reliable for files over ~400 bytes. \
                     \
                     Emit `arguments` as a JSON object literal (not a stringified JSON). \
                     Correct: `\"arguments\": {\"path\": \"/abs/x.rs\", \"content\": \"fn main(){}\"}`."
                        .to_string(),
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path of the file to write."
                        },
                        "content": {
                            "type": "string",
                            "description": "Exact file contents. Newlines and special characters are preserved verbatim."
                        }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            },
        },
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolFunction {
                name: SYNTHETIC_TOOL_READ_FILE.to_string(),
                description: Some(
                    "Read the entire contents of a file from disk. Returns the file's text."
                        .to_string(),
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path of the file to read."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
        },
        // Chunked-write protocol. Motivation: telemetry on v7 codex
        // smoke (2026-05-13) showed FINAL-Bench 35B-A3B-Opus losing
        // structural coherence (stray chars, nested types crossing
        // scope, over-closing braces) when emitting multi-KB Rust
        // source inside a single `<tool_call>` envelope. Breaking the
        // write into 150–250 byte chunks keeps each emit short, well
        // inside the model's reliable-emit window, and makes per-call
        // failures recoverable instead of nuking the whole file.
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolFunction {
                name: SYNTHETIC_TOOL_WRITE_FILE_BEGIN.to_string(),
                description: Some(
                    "Begin a chunked write. Creates (or truncates) the file at `path` to an \
                     empty state. Follow with one or more `write_file_chunk(path, chunk)` \
                     calls, then a single `write_file_end(path)` to commit. Use this when the \
                     content you intend to write is over ~400 bytes — short writes can use \
                     `write_file(path, content)` in one shot."
                        .to_string(),
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path of the file to (re)create."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
        },
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolFunction {
                name: SYNTHETIC_TOOL_WRITE_FILE_CHUNK.to_string(),
                description: Some(
                    "Append one chunk of text to a file previously opened with \
                     `write_file_begin(path)`. Recommended chunk size: 150–250 bytes (about \
                     8–15 lines of code) — smaller chunks emit more reliably than long ones. \
                     Call repeatedly until the entire content is on disk, then call \
                     `write_file_end(path)` to commit."
                        .to_string(),
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path of the file being assembled."
                        },
                        "chunk": {
                            "type": "string",
                            "description": "The text to append to the file. Newlines and \
                                            special characters are preserved verbatim."
                        }
                    },
                    "required": ["path", "chunk"],
                    "additionalProperties": false
                }),
            },
        },
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolFunction {
                name: SYNTHETIC_TOOL_WRITE_FILE_END.to_string(),
                description: Some(
                    "Commit a chunked write. Returns the final byte count of the file. Call \
                     once after all `write_file_chunk` calls have landed. (No-op apart from \
                     reporting size — chunks are flushed as they arrive.)"
                        .to_string(),
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path of the file being finalized."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
        },
    ]
}

/// If `name` matches a synthetic tool, returns the rewritten
/// `(name, arguments_json)` pair to forward to codex. The new name is
/// always `exec_command`; the arguments encode the file op as a shell
/// command using POSIX single-quote quoting.
///
/// Returns `None` ONLY when the tool name is not synthetic — caller
/// emits the original tool_call unchanged in that case. When `name`
/// IS synthetic but the args are malformed (parse fails, no `path`,
/// empty `path`), we still rewrite to a safe fallback so codex never
/// sees a `write_file` / `read_file` function-call name it can't
/// route. Letting the original name leak through caused `unsupported
/// call: write_file` errors in the 2026-05-12 codex smoke when the
/// model emitted a fourth, badly-shaped attempt after three good
/// ones.
pub(crate) fn rewrite_synthetic_tool_call(
    name: &str,
    arguments_json: &str,
) -> Option<(String, String)> {
    let is_synthetic = matches!(
        name,
        SYNTHETIC_TOOL_WRITE_FILE
            | SYNTHETIC_TOOL_READ_FILE
            | SYNTHETIC_TOOL_WRITE_FILE_BEGIN
            | SYNTHETIC_TOOL_WRITE_FILE_CHUNK
            | SYNTHETIC_TOOL_WRITE_FILE_END
    );
    if !is_synthetic {
        return None;
    }
    // Telemetry: every synthetic tool call gets a structured log line
    // with the model's emit shape so we can answer "what did the
    // model actually ask us to do?" without re-reading the codex
    // session log. Fields:
    //   - args_bytes: length of the JSON args string emitted by the
    //     model
    //   - args_parsed: whether the args parsed as a JSON object
    //   - raw_path / normalized_path: visible whitespace mangling, if any
    //   - content_bytes: size of write_file content
    //   - content_starts_with: first 80 chars of content (for sanity)
    //
    // Emitted at info level — every synthetic call is meaningful work
    // and the volume is bounded by the model's iteration count.
    let parsed_result: Result<serde_json::Value, _> = serde_json::from_str(arguments_json);
    let args_parsed = parsed_result.is_ok();
    let args: serde_json::Value = parsed_result.unwrap_or(serde_json::Value::Null);
    let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let path_owned = normalize_path_segments(raw_path);
    let path = path_owned.as_str();
    // `content` for write_file, `chunk` for write_file_chunk; absent
    // for the other shapes.
    let content = args
        .get("content")
        .or_else(|| args.get("chunk"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content_starts_with: String = content.chars().take(80).collect();
    tracing::info!(
        tool = %name,
        args_bytes = arguments_json.len(),
        args_parsed,
        raw_path = %raw_path,
        normalized_path = %path,
        path_was_mangled = (raw_path != path),
        content_bytes = content.len(),
        content_starts_with = %content_starts_with,
        "responses: synthetic tool call inbound"
    );

    let cmd = if path.is_empty() {
        // Malformed: emit a noisy shell error so the model can self-
        // correct on the next turn. Never let codex see the synthetic
        // name.
        tracing::warn!(
            tool = %name,
            args_bytes = arguments_json.len(),
            args_parsed,
            "responses: synthetic call had empty/missing path — emitting shell error"
        );
        format!(
            "echo 'rewrite_synthetic_tool_call: {} called with empty path' >&2; exit 64",
            name
        )
    } else {
        match name {
            SYNTHETIC_TOOL_WRITE_FILE => {
                // Hard cap: route anything over ~350 bytes through the
                // chunked protocol. The model's training prior biases
                // toward single-shot writes regardless of the tool
                // description's recommendation; without this guard the
                // local model fails reliably on multi-KB content. The
                // error message names the exact alternative tools so
                // the model can self-correct on the next turn.
                if content.len() > 350 {
                    tracing::warn!(
                        tool = %name,
                        content_bytes = content.len(),
                        path = %path,
                        "responses: synthetic write_file content over 350 bytes — routing to chunked-write nudge"
                    );
                    format!(
                        "echo 'write_file refused: content is {} bytes which exceeds the 350-byte single-shot limit. Use write_file_begin(path) then a series of write_file_chunk(path, chunk) calls (150-250 bytes each), then write_file_end(path).' >&2; exit 65",
                        content.len()
                    )
                } else {
                    // Short write — truncate-and-write in one shell
                    // statement. Parent dir created on demand.
                    let dir = parent_dir(path);
                    format!(
                        "mkdir -p {} && printf '%s' {} > {}",
                        shell_single_quote(&dir),
                        shell_single_quote(content),
                        shell_single_quote(path)
                    )
                }
            }
            SYNTHETIC_TOOL_READ_FILE => {
                format!("cat {}", shell_single_quote(path))
            }
            SYNTHETIC_TOOL_WRITE_FILE_BEGIN => {
                // Truncate the file to empty so subsequent
                // write_file_chunk calls append from a known state.
                // Create parent dir on demand.
                let dir = parent_dir(path);
                format!(
                    "mkdir -p {} && : > {} && wc -c < {}",
                    shell_single_quote(&dir),
                    shell_single_quote(path),
                    shell_single_quote(path)
                )
            }
            SYNTHETIC_TOOL_WRITE_FILE_CHUNK => {
                format!(
                    "printf '%s' {} >> {} && wc -c < {}",
                    shell_single_quote(content),
                    shell_single_quote(path),
                    shell_single_quote(path)
                )
            }
            SYNTHETIC_TOOL_WRITE_FILE_END => {
                format!("wc -c < {}", shell_single_quote(path))
            }
            _ => unreachable!("guarded by name check above"),
        }
    };
    tracing::info!(
        tool = %name,
        cmd_bytes = cmd.len(),
        path = %path,
        will_write_to = %path,
        "responses: synthetic call rewritten to exec_command"
    );
    let new_args = serde_json::json!({ "cmd": cmd });
    Some(("exec_command".to_string(), new_args.to_string()))
}

/// Parent directory of `path`. Returns `.` when the path has no
/// directory component (relative bare filename) or `/` when the path
/// is just `/<file>`.
pub(crate) fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

/// Normalize a filesystem path emitted by the model. Splits on `/`,
/// trims each segment of leading/trailing whitespace, drops empty
/// segments, rejoins with `/`. Preserves the leading `/` for
/// absolute paths.
///
/// Why: local models occasionally emit `/abs/ project-name /src/lib.rs`
/// — whitespace around segments — when markdown-emphasis tokens from
/// the prompt bleed into tool_call arguments. Without normalization
/// the shell writes to a sibling path that's invisible to the
/// operator and the agent thinks it succeeded.
pub(crate) fn normalize_path_segments(p: &str) -> String {
    let absolute = p.trim_start().starts_with('/');
    let mut out: Vec<&str> = p
        .split('/')
        .map(|seg| seg.trim())
        .filter(|seg| !seg.is_empty())
        .collect();
    let joined = out.join("/");
    if absolute {
        // Re-prepend leading slash that the split-and-rejoin stripped.
        let _ = &mut out;
        format!("/{}", joined)
    } else {
        joined
    }
}

/// POSIX sh single-quote quoting. Wraps `s` in `'...'` and replaces
/// any embedded `'` with `'\''` (close-quote, escaped-quote,
/// reopen-quote) — the standard idiom for embedding arbitrary bytes
/// inside a shell single-quoted string with no possibility of
/// interpolation.
pub(crate) fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}
