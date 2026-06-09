// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rust executors for the canonical primitives. One async fn per
//! `PrimitiveKind`. Each takes `&ExecCtx` (the bound workdir) +
//! parsed args and returns `Result<ToolResult, ToolError>`.
//!
//! Per ARCH §9, every executor emits a `tracing::info!` event with
//! the canonical primitive id, the args fingerprint, and the
//! outcome. The events are what makes "what did we run" auditable
//! after the fact.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::primitive::{
    AgentDoneArgs, AgentPlanArgs, HandoffToEvaluatorArgs, HandoffToImplementerArgs, InspectIntent,
    PatchFileArgs, Primitive, ReplaceFunctionArgs, SmokeArgs, WriteFileArgs,
};
use crate::result::{ToolError, ToolResult};
use crate::syntax::DynSyntaxValidator;

/// Bound execution context. Every primitive is workdir-relative;
/// nothing escapes the directory by design.
///
/// `build_cmd` and `verify_cmd` carry the per-problem language-
/// specific commands. Rust problems use `cargo build` / `cargo test
/// --test integration`; Go uses `go build ./...` / `go test ./...`;
/// Python's build is a no-op string and the executor returns
/// success immediately. The primitive holds the verb; the problem
/// config holds the command.
#[derive(Clone)]
pub struct ExecCtx {
    pub workdir: PathBuf,
    /// Wall-clock cap for subprocess primitives (`build`, `smoke`).
    /// `inspect_workdir` / `write_file` / handoffs ignore this.
    pub subprocess_wall_cap: Duration,
    /// Shell command for the `build` primitive. Empty string means
    /// "no-op build" (Python, etc.).
    pub build_cmd: String,
    /// Shell command for the `smoke` primitive. The bench reads
    /// this from `problem.witness.verify_cmd`.
    pub verify_cmd: String,
    /// Optional pre-build syntax validator. When set, `exec_build`
    /// walks the workdir, parses every source file matching the
    /// validator's language extensions, and short-circuits the
    /// subprocess invocation if any parse-level error is found.
    /// Closes "model emits broken syntax → wastes 5-30s `cargo
    /// build` cycle on something a static parser catches in <50ms"
    /// faster-feedback class. Language-agnostic: bench wires a
    /// language-appropriate impl per problem.
    pub syntax_validator: Option<DynSyntaxValidator>,
}

impl std::fmt::Debug for ExecCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecCtx")
            .field("workdir", &self.workdir)
            .field("subprocess_wall_cap", &self.subprocess_wall_cap)
            .field("build_cmd", &self.build_cmd)
            .field("verify_cmd", &self.verify_cmd)
            .field(
                "syntax_validator",
                &self.syntax_validator.as_ref().map(|_| "<set>"),
            )
            .finish()
    }
}

impl ExecCtx {
    pub fn new(workdir: PathBuf) -> Self {
        Self {
            workdir,
            subprocess_wall_cap: Duration::from_secs(120),
            // Default to Rust commands; per-language overrides
            // arrive via builders below.
            build_cmd: "cargo build 2>&1".to_string(),
            verify_cmd: "cargo test --quiet --test integration 2>&1".to_string(),
            syntax_validator: None,
        }
    }

    pub fn with_subprocess_wall_cap(mut self, dur: Duration) -> Self {
        self.subprocess_wall_cap = dur;
        self
    }

    pub fn with_build_cmd(mut self, cmd: impl Into<String>) -> Self {
        self.build_cmd = cmd.into();
        self
    }

    pub fn with_verify_cmd(mut self, cmd: impl Into<String>) -> Self {
        self.verify_cmd = cmd.into();
        self
    }

    pub fn with_syntax_validator(mut self, v: DynSyntaxValidator) -> Self {
        self.syntax_validator = Some(v);
        self
    }
}

/// Dispatch a parsed `Primitive` to its executor.
pub async fn execute(ctx: &ExecCtx, prim: &Primitive) -> Result<ToolResult, ToolError> {
    let id = prim.kind().id();
    info!(primitive = id, "commonwealth_agent_tools::executor: invoke");
    let result = match prim {
        Primitive::InspectWorkdir(intent) => exec_inspect(ctx, intent).await,
        Primitive::WriteFile(args) => exec_write_file(ctx, args).await,
        Primitive::PatchFile(args) => exec_patch_file(ctx, args).await,
        Primitive::ReplaceFunction(args) => exec_replace_function(ctx, args).await,
        Primitive::Build => exec_build(ctx).await,
        Primitive::Smoke(args) => exec_smoke(ctx, args).await,
        Primitive::AgentDone(args) => exec_agent_done(args).await,
        Primitive::AgentPlan(args) => exec_agent_plan(args).await,
        Primitive::HandoffToEvaluator(args) => exec_handoff_to_evaluator(args).await,
        Primitive::HandoffToImplementer(args) => exec_handoff_to_implementer(args).await,
    };
    match &result {
        Ok(r) => info!(
            primitive = id,
            ok = r.ok,
            "commonwealth_agent_tools::executor: ran"
        ),
        Err(e) => info!(
            primitive = id,
            error = %e,
            "commonwealth_agent_tools::executor: failed"
        ),
    }
    result
}

// ── inspect_workdir ────────────────────────────────────────────────

async fn exec_inspect(ctx: &ExecCtx, intent: &InspectIntent) -> Result<ToolResult, ToolError> {
    match intent {
        InspectIntent::File { path } => {
            let abs = resolve_workdir_path(&ctx.workdir, path)?;
            let bytes = tokio::fs::read(&abs)
                .await
                .map_err(|e| ToolError::Filesystem {
                    primitive: "inspect_workdir",
                    reason: format!("read {}: {e}", path),
                })?;
            let content = String::from_utf8_lossy(&bytes).into_owned();
            Ok(ToolResult::ok(json!({
                "intent": "file",
                "path": path,
                "bytes": bytes.len(),
                "content": content,
            })))
        }
        InspectIntent::Dir { path } => {
            let abs = resolve_workdir_path(&ctx.workdir, path)?;
            let mut entries: Vec<serde_json::Value> = Vec::new();
            let mut rd = tokio::fs::read_dir(&abs)
                .await
                .map_err(|e| ToolError::Filesystem {
                    primitive: "inspect_workdir",
                    reason: format!("readdir {}: {e}", path),
                })?;
            while let Some(entry) = rd.next_entry().await.map_err(|e| ToolError::Filesystem {
                primitive: "inspect_workdir",
                reason: format!("readdir-iter {}: {e}", path),
            })? {
                let name = entry.file_name().to_string_lossy().into_owned();
                let ft = entry.file_type().await.ok();
                let kind = match ft {
                    Some(t) if t.is_dir() => "dir",
                    Some(t) if t.is_file() => "file",
                    Some(t) if t.is_symlink() => "symlink",
                    _ => "other",
                };
                entries.push(json!({"name": name, "kind": kind}));
            }
            entries.sort_by(|a, b| {
                a.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
            });
            Ok(ToolResult::ok(json!({
                "intent": "dir",
                "path": path,
                "entries": entries,
            })))
        }
        InspectIntent::FindByName { root, pattern } => {
            let abs = resolve_workdir_path(&ctx.workdir, root)?;
            let mut matches: Vec<String> = Vec::new();
            walk_collect_paths(&abs, pattern, &mut matches);
            let rel: Vec<String> = matches
                .into_iter()
                .filter_map(|p| {
                    PathBuf::from(&p)
                        .strip_prefix(&ctx.workdir)
                        .ok()
                        .map(|r| r.to_string_lossy().into_owned())
                })
                .collect();
            Ok(ToolResult::ok(json!({
                "intent": "find_by_name",
                "root": root,
                "pattern": pattern,
                "matches": rel,
            })))
        }
        InspectIntent::GrepContents { root, pattern } => {
            let abs = resolve_workdir_path(&ctx.workdir, root)?;
            let mut hits: Vec<serde_json::Value> = Vec::new();
            walk_grep_contents(&abs, pattern, &ctx.workdir, &mut hits);
            Ok(ToolResult::ok(json!({
                "intent": "grep_contents",
                "root": root,
                "pattern": pattern,
                "matches": hits,
            })))
        }
    }
}

fn walk_collect_paths(dir: &Path, needle: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.file_name()
            .is_some_and(|n| n.to_string_lossy().contains(needle))
        {
            out.push(p.to_string_lossy().into_owned());
        }
        if p.is_dir() && !p.file_name().is_some_and(|n| n == "target" || n == ".git") {
            walk_collect_paths(&p, needle, out);
        }
    }
}

fn walk_grep_contents(
    dir: &Path,
    needle: &str,
    workdir_root: &Path,
    out: &mut Vec<serde_json::Value>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() && !p.file_name().is_some_and(|n| n == "target" || n == ".git") {
            walk_grep_contents(&p, needle, workdir_root, out);
            continue;
        }
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        if bytes.iter().take(4096).any(|b| *b == 0) {
            continue; // skip binaries
        }
        let s = String::from_utf8_lossy(&bytes);
        let rel = p
            .strip_prefix(workdir_root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
        for (ln_zero_idx, line) in s.lines().enumerate() {
            if line.contains(needle) {
                out.push(json!({
                    "path": rel,
                    "line": ln_zero_idx + 1,
                    "text": line.chars().take(400).collect::<String>(),
                }));
            }
        }
    }
}

// ── write_file ────────────────────────────────────────────────────

/// File-size threshold above which `write_file` is rejected for
/// EXISTING files in favor of `patch_file`. Files at or below this
/// fit comfortably in a single chat-completion response without
/// hitting the token-level corruption regime observed on
/// 4.2-mini-evaluator-python (2026-05-23). Net-new files (path
/// doesn't exist yet) are unaffected — initial author is the
/// legitimate use case for write_file on any size.
pub const LARGE_FILE_REWRITE_THRESHOLD_LINES: usize = 150;

async fn exec_write_file(ctx: &ExecCtx, args: &WriteFileArgs) -> Result<ToolResult, ToolError> {
    let abs = resolve_workdir_path(&ctx.workdir, &args.path)?;

    // Structural rejection: existing large files must be edited via
    // patch_file, not rewritten via write_file. Closes the
    // "5000-token Python rewrite accumulates token-level corruption"
    // class. New files (path doesn't exist yet) bypass this check
    // because there's no "small patch" alternative for initial author.
    if let Ok(existing) = tokio::fs::read_to_string(&abs).await {
        let existing_lines = existing.lines().count();
        if existing_lines > LARGE_FILE_REWRITE_THRESHOLD_LINES {
            tracing::info!(
                path = %args.path,
                existing_lines,
                threshold = LARGE_FILE_REWRITE_THRESHOLD_LINES,
                "commonwealth_agent_tools::executor: write_file rejected — large existing file, use patch_file"
            );
            return Err(ToolError::WriteFileTooLarge {
                path: args.path.clone(),
                existing_lines,
                threshold: LARGE_FILE_REWRITE_THRESHOLD_LINES,
            });
        }
    }

    // Pre-write syntax check. When a SyntaxValidator is bound and the
    // target path's extension is one the validator handles, parse
    // `args.content` BEFORE the write lands on disk. Rejects the
    // observed-on-3.2-lights-out-python (2026-05-23) class of
    // "prose-in-source / token-level typos / drifting indentation"
    // at the write boundary so the Implementer can re-emit
    // immediately — the broken bytes never reach disk, so the next
    // Evaluator build cycle doesn't waste a round-trip discovering
    // them.
    //
    // Symmetric with exec_build's existing post-write validator
    // call: both use the same `SyntaxValidator` instance; the pre-
    // write check operates on the candidate content string while
    // the pre-build check walks the workdir. Together they form a
    // belt-and-suspenders gate against syntactically-invalid
    // workdir state.
    let recovered =
        syntax_gate_with_gutter_recovery(ctx, "write_file", args.path.as_str(), &args.content, &[])?;

    if let Some(parent) = abs.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let content_to_write: &str = recovered.as_deref().unwrap_or(&args.content);
    let bytes = content_to_write.as_bytes();
    tokio::fs::write(&abs, bytes)
        .await
        .map_err(|e| ToolError::Filesystem {
            primitive: "write_file",
            reason: format!("write {}: {e}", args.path),
        })?;
    Ok(ToolResult::ok(json!({
        "wrote": args.path,
        "bytes": bytes.len(),
    })))
}

// ── patch_file ────────────────────────────────────────────────────

async fn exec_patch_file(ctx: &ExecCtx, args: &PatchFileArgs) -> Result<ToolResult, ToolError> {
    let abs = resolve_workdir_path(&ctx.workdir, &args.path)?;

    // Structural redirect to replace_function when the patch range
    // exactly matches a function/class's bounds. Same gate-style
    // pattern as write_file → patch_file for large files: the
    // model uses the wrong tool, the executor rejects with a
    // pointer at the right one. The model has no way to ignore
    // the rejection. Closes the "model has replace_function
    // available but defaults to patch_file from habit" class
    // observed on 4.2 v-replfn 2026-05-23.
    if let Ok(existing) = tokio::fs::read_to_string(&abs).await {
        if let Some(fn_name) =
            function_at_range(&existing, args.start_line as usize, args.end_line as usize)
        {
            return Err(ToolError::InvalidArguments {
                primitive: "patch_file",
                reason: format!(
                    "this patch range matches the bounds of function/class `{fn_name}` exactly. Use `replace_function(path=\"{}\", name=\"{fn_name}\", new_body=...)` instead — it's a smaller output surface (no line ranges to count) and matches how the model reasons about whole-function rewrites.",
                    args.path
                ),
            });
        }
    }

    // Reject unified-diff-shaped `new_content`. The tool name
    // "patch_file" + the line-numbered file anchor at position 0
    // jointly cued the model to emit diff-style content (with
    // `+/-` line prefixes and `N | ` line-number columns copied
    // verbatim from the anchor display). Observed 4.2 2026-05-23:
    // 6 consecutive rejected patches all in diff format. Reject
    // structurally with a help message clarifying that new_content
    // is raw replacement text. The check is heuristic: at least
    // two leading lines (after optional whitespace) start with
    // `+`, `-`, or `<digits>:`/`<digits> |` — strong indicator the
    // model is emitting a diff rather than raw code.
    if looks_like_diff(&args.new_content) {
        tracing::info!(
            path = %args.path,
            "executor: patch_file new_content looks like a unified diff or anchor-copy; rejecting"
        );
        return Err(ToolError::InvalidArguments {
            primitive: "patch_file",
            reason: "new_content looks like a unified diff or copy of the line-numbered file anchor (contains `+/-` line prefixes or `N: ` / `N | ` line-number columns). The new_content field must be raw replacement source code — exactly what should appear in the file. Strip any diff markers and line-number prefixes. Example: to replace lines 5-7 with two lines of code, new_content should be \"x = 1\\ny = 2\" (just the code, no `+` markers, no `5:` prefixes).".to_string(),
        });
    }

    let existing = tokio::fs::read_to_string(&abs)
        .await
        .map_err(|e| ToolError::Filesystem {
            primitive: "patch_file",
            reason: format!("read {}: {e}", args.path),
        })?;

    let lines: Vec<&str> = existing.lines().collect();
    let total = lines.len() as u32;
    if args.start_line < 1 || args.start_line > total {
        return Err(ToolError::InvalidArguments {
            primitive: "patch_file",
            reason: format!(
                "start_line {} out of range [1, {}] (file has {} lines)",
                args.start_line, total, total
            ),
        });
    }
    if args.end_line < args.start_line || args.end_line > total {
        return Err(ToolError::InvalidArguments {
            primitive: "patch_file",
            reason: format!(
                "end_line {} must be in [start_line={}, {}]",
                args.end_line, args.start_line, total
            ),
        });
    }

    // Assemble: prefix [0, start-1) + replacement + suffix [end, total).
    //
    // No auto-reindentation: experimentally validated 2026-05-24
    // (dynamic-loop ablation), letting model-emitted patches land
    // verbatim is correct even when their indent is "off." The
    // pre-write syntax check rejects mismatches, the rejection
    // becomes the model's next-turn feedback, and the model
    // typically recovers in one round. Auto-shifting the indent
    // bypassed that recovery loop AND let algorithmically-broken
    // patches (that previously bounced for incidental indent
    // issues) land on disk.
    let trailing_newline = existing.ends_with('\n');
    let prefix = &lines[..(args.start_line as usize - 1)];
    let suffix = &lines[args.end_line as usize..];

    let mut out_lines: Vec<&str> = Vec::with_capacity(prefix.len() + 64 + suffix.len());
    out_lines.extend_from_slice(prefix);
    // Empty new_content means pure deletion. A trailing \n in
    // new_content produces a trailing empty entry from split, which
    // we drop to avoid double-blank lines.
    let new_lines: Vec<&str> = if args.new_content.is_empty() {
        Vec::new()
    } else {
        let mut v: Vec<&str> = args.new_content.split('\n').collect();
        if let Some(last) = v.last() {
            if last.is_empty() {
                v.pop();
            }
        }
        v
    };
    out_lines.extend(new_lines.iter().copied());
    out_lines.extend_from_slice(suffix);

    let mut result = out_lines.join("\n");
    if trailing_newline {
        result.push('\n');
    }

    // Build a boundary-deduped recovery candidate: the dominant patch
    // defect is the model including a leading/trailing CONTEXT line that
    // this strict line-range splice then DUPLICATES against the unchanged
    // prefix/suffix (off-by-one on the range, or diff-style context).
    // Trim that overlap and offer the re-spliced result as a candidate —
    // adopted only if it parses, so a legitimate boundary-repeat is never
    // silently shortened.
    let deduped: Vec<String> = match dedup_patch_boundary(prefix, &new_lines, suffix) {
        Some(trimmed) => {
            let mut dl: Vec<&str> = Vec::with_capacity(prefix.len() + trimmed.len() + suffix.len());
            dl.extend_from_slice(prefix);
            dl.extend(trimmed.iter().copied());
            dl.extend_from_slice(suffix);
            let mut s = dl.join("\n");
            if trailing_newline {
                s.push('\n');
            }
            vec![s]
        }
        None => Vec::new(),
    };
    // Pre-write syntax check on the FULL post-patch content. Symmetric
    // with exec_write_file: broken content never lands on disk.
    let result = match syntax_gate_with_gutter_recovery(
        ctx,
        "patch_file",
        args.path.as_str(),
        &result,
        &deduped,
    )? {
        Some(cleaned) => cleaned,
        None => result,
    };

    tokio::fs::write(&abs, result.as_bytes())
        .await
        .map_err(|e| ToolError::Filesystem {
            primitive: "patch_file",
            reason: format!("write {}: {e}", args.path),
        })?;

    let lines_replaced = args.end_line - args.start_line + 1;
    let lines_inserted = new_lines.len() as u32;
    Ok(ToolResult::ok(json!({
        "patched": args.path,
        "lines_replaced": lines_replaced,
        "lines_inserted": lines_inserted,
        "bytes": result.len(),
    })))
}

// ── replace_function ──────────────────────────────────────────────

/// If a function/class in `content` has bounds that match the
/// 1-indexed inclusive (start_line, end_line) range, return its
/// name. Used by exec_patch_file to redirect whole-function patches
/// to replace_function. "Matches" is exact: start_line equals the
/// `def`/`class` line (or its decorator-prefix start) AND end_line
/// equals the function's last body line.
pub fn function_at_range(content: &str, start_line: usize, end_line: usize) -> Option<String> {
    // The patch_file API uses 1-indexed inclusive line numbers;
    // find_function_bounds returns 0-indexed half-open.
    // Convert: 0-indexed start = start_line - 1; 0-indexed end
    // (half-open) = end_line.
    if start_line == 0 || end_line < start_line {
        return None;
    }
    let pstart = start_line - 1;
    let pend = end_line;
    let lines: Vec<&str> = content.lines().collect();
    let patterns = ["def ", "async def ", "class "];
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let mut name: Option<String> = None;
        for p in &patterns {
            if let Some(after) = trimmed.strip_prefix(p) {
                let n: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !n.is_empty() {
                    name = Some(n);
                }
                break;
            }
        }
        let Some(fn_name) = name else { continue };
        // Walk back over decorators.
        let mut start = i;
        while start > 0 {
            let prev = lines[start - 1];
            let prev_t = prev.trim_start();
            if !prev_t.starts_with('@') {
                break;
            }
            let prev_indent = prev.len() - prev_t.len();
            if prev_indent != indent {
                break;
            }
            start -= 1;
        }
        // Walk forward over indented body.
        let mut end = i + 1;
        while end < lines.len() {
            let l = lines[end];
            let lt = l.trim_start();
            if lt.is_empty() || lt.starts_with('#') {
                end += 1;
                continue;
            }
            let li = l.len() - lt.len();
            if li <= indent {
                break;
            }
            end += 1;
        }
        // Back off trailing blank/comment-only lines so bounds
        // align with intuitive "last body line." A user patching
        // a function naturally specifies start..last-real-line.
        let mut tight_end = end;
        while tight_end > i + 1 {
            let lt = lines[tight_end - 1].trim_start();
            if lt.is_empty() || lt.starts_with('#') {
                tight_end -= 1;
            } else {
                break;
            }
        }
        if start == pstart && tight_end == pend {
            return Some(fn_name);
        }
    }
    None
}

/// Find the line range (start..end, half-open, 0-indexed) of the
/// definition of `name` in `content`. Scans for `def NAME(`,
/// `async def NAME(`, `class NAME(`, or `class NAME:` at any
/// indent, then walks forward including indented body until the
/// next non-empty, non-comment line at the SAME OR LESS indent.
/// Decorator lines (`@foo`) immediately preceding the def at the
/// same indent are included in the range.
///
/// Returns None if no matching definition is found.
pub fn find_function_bounds(content: &str, name: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let patterns = [
        format!("def {}(", name),
        format!("async def {}(", name),
        format!("class {}(", name),
        format!("class {}:", name),
    ];
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if !patterns.iter().any(|p| trimmed.starts_with(p)) {
            continue;
        }
        // Walk back over @decorator lines at the same indent.
        let mut start = i;
        while start > 0 {
            let prev = lines[start - 1];
            let prev_trim = prev.trim_start();
            if !prev_trim.starts_with('@') {
                break;
            }
            let prev_indent = prev.len() - prev_trim.len();
            if prev_indent != indent {
                break;
            }
            start -= 1;
        }
        // Walk forward over indented body until a non-empty,
        // non-comment line at SAME OR LESS indent terminates it.
        let mut end = i + 1;
        while end < lines.len() {
            let l = lines[end];
            let lt = l.trim_start();
            if lt.is_empty() || lt.starts_with('#') {
                end += 1;
                continue;
            }
            let li = l.len() - lt.len();
            if li <= indent {
                break;
            }
            end += 1;
        }
        return Some((start, end));
    }
    None
}

async fn exec_replace_function(
    ctx: &ExecCtx,
    args: &ReplaceFunctionArgs,
) -> Result<ToolResult, ToolError> {
    let abs = resolve_workdir_path(&ctx.workdir, &args.path)?;
    let existing = tokio::fs::read_to_string(&abs)
        .await
        .map_err(|e| ToolError::Filesystem {
            primitive: "replace_function",
            reason: format!("read {}: {e}", args.path),
        })?;

    let bounds = find_function_bounds(&existing, &args.name);
    let Some((start, end)) = bounds else {
        return Err(ToolError::InvalidArguments {
            primitive: "replace_function",
            reason: format!(
                "no function or class named `{}` found in {}",
                args.name, args.path
            ),
        });
    };

    let lines: Vec<&str> = existing.lines().collect();
    let trailing_newline = existing.ends_with('\n');

    // No auto-reindentation: see comment in exec_patch_file. Model
    // is responsible for matching the existing indent; pre-write
    // syntax check rejects mismatches so the model can recover on
    // its next turn.
    let mut new_lines: Vec<&str> = Vec::with_capacity(lines.len());
    new_lines.extend_from_slice(&lines[..start]);
    let body_lines: Vec<&str> = if args.new_body.is_empty() {
        Vec::new()
    } else {
        let mut v: Vec<&str> = args.new_body.split('\n').collect();
        if v.last().map(|s| s.is_empty()).unwrap_or(false) {
            v.pop();
        }
        v
    };
    new_lines.extend(body_lines.iter().copied());
    new_lines.extend_from_slice(&lines[end..]);
    let mut result = new_lines.join("\n");
    if trailing_newline {
        result.push('\n');
    }

    // Pre-write syntax check on the full post-replace content.
    let result = match syntax_gate_with_gutter_recovery(
        ctx,
        "replace_function",
        args.path.as_str(),
        &result,
        &[],
    )? {
        Some(cleaned) => cleaned,
        None => result,
    };

    tokio::fs::write(&abs, result.as_bytes())
        .await
        .map_err(|e| ToolError::Filesystem {
            primitive: "replace_function",
            reason: format!("write {}: {e}", args.path),
        })?;

    let lines_replaced = (end - start) as u32;
    let lines_inserted = body_lines.len() as u32;
    Ok(ToolResult::ok(json!({
        "replaced": args.name,
        "path": args.path,
        "lines_replaced": lines_replaced,
        "lines_inserted": lines_inserted,
        "bytes": result.len(),
    })))
}

// ── build ─────────────────────────────────────────────────────────

async fn exec_build(ctx: &ExecCtx) -> Result<ToolResult, ToolError> {
    // No-op build (e.g. Python) — return success immediately.
    if ctx.build_cmd.trim().is_empty() {
        return Ok(ToolResult::ok(json!({
            "ok": true,
            "note": "no-op build (interpreted language)",
            "stdout_tail": "",
        })));
    }
    // Pre-build syntax check (language-agnostic; one impl per
    // language plugged in via `with_syntax_validator`). When a
    // validator is bound and any source file fails to parse,
    // short-circuit the subprocess invocation and return a
    // cargo-shape error envelope. This gives the model the same
    // feedback texture as a real `cargo build` failure (caret,
    // line:col, file path) but in <50ms instead of 5-30s — and
    // catches a class of model failure (placeholder-comment
    // abandonments, missing-brace half-writes) that would otherwise
    // burn the full build cycle's tokens + wall.
    if let Some(validator) = ctx.syntax_validator.as_ref() {
        let errors = validator.check_workdir(&ctx.workdir);
        if !errors.is_empty() {
            let stdout_tail = validator.render_errors(&errors);
            tracing::info!(
                error_count = errors.len(),
                "commonwealth_agent_tools::executor: pre-build syntax check rejected workdir"
            );
            return Ok(ToolResult::ok(json!({
                "ok": false,
                "stdout_tail": stdout_tail,
                "pre_build_syntax_check": true,
            })));
        }
    }
    let (status_ok, stdout_tail) = run_shell(
        &ctx.workdir,
        &ctx.build_cmd,
        ctx.subprocess_wall_cap,
        "build",
    )
    .await?;
    Ok(ToolResult::ok(json!({
        "ok": status_ok,
        "stdout_tail": stdout_tail,
    })))
}

// ── smoke ─────────────────────────────────────────────────────────

async fn exec_smoke(ctx: &ExecCtx, args: &SmokeArgs) -> Result<ToolResult, ToolError> {
    if ctx.verify_cmd.trim().is_empty() {
        return Err(ToolError::Subprocess {
            primitive: "smoke",
            reason: "ExecCtx.verify_cmd is empty — bench problem config missing verify_cmd".into(),
        });
    }
    // Append filter as a single positional argument when supplied.
    // The per-language test runner interprets it according to its
    // convention (cargo: test name substring; pytest: -k expression;
    // etc.). The native runner is language-agnostic; the problem
    // config decides what the filter means.
    let cmd = match args.filter.as_deref() {
        Some(f) if !f.is_empty() => format!("{} {}", ctx.verify_cmd, shell_escape(f)),
        _ => ctx.verify_cmd.clone(),
    };
    let (status_ok, stdout_tail) =
        run_shell(&ctx.workdir, &cmd, ctx.subprocess_wall_cap, "smoke").await?;
    // Parse libtest output into structured pass/fail counts when
    // possible. Non-libtest output (go test json, vitest, pytest)
    // parses to all-zeros; the model still gets stdout_tail.
    let parsed = parse_libtest_summary(&stdout_tail);
    Ok(ToolResult::ok(json!({
        "ok": status_ok,
        "passed": parsed.passed,
        "failed": parsed.failed,
        "total": parsed.total,
        "failed_names": parsed.failed_names,
        "stdout_tail": stdout_tail,
    })))
}

#[derive(Debug, Default)]
struct LibtestSummary {
    passed: u32,
    failed: u32,
    total: u32,
    failed_names: Vec<String>,
}

/// Tiny libtest output parser. Lifted from the bench's existing
/// `witness/test_result_parser.rs` shape so canonical and bench
/// reporting agree on what "pass" means. We don't take a dep on the
/// bench crate (clean dependency direction); we re-implement the
/// few lines we need.
fn parse_libtest_summary(stdout: &str) -> LibtestSummary {
    let mut s = LibtestSummary::default();
    for line in stdout.lines() {
        let trimmed = line.trim();
        // "test foo::bar ... FAILED"
        if let Some(rest) = trimmed.strip_prefix("test ") {
            if let Some(name) = rest
                .strip_suffix(" ... FAILED")
                .or_else(|| rest.strip_suffix(" ... failed"))
            {
                s.failed_names.push(name.trim().to_string());
            }
        }
        // "test result: ok. 3 passed; 0 failed; ..."
        // "test result: FAILED. 1 passed; 2 failed; ..."
        // Tokenize on any non-alphanumeric boundary; scan adjacent
        // (number, label) pairs.
        if let Some(rest) = trimmed.strip_prefix("test result: ") {
            let toks: Vec<&str> = rest
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .collect();
            for window in toks.windows(2) {
                let (n, label) = (window[0], window[1]);
                if let Ok(v) = n.parse::<u32>() {
                    match label {
                        "passed" => s.passed = v,
                        "failed" => s.failed = v,
                        _ => {}
                    }
                }
            }
        }
    }
    s.total = s.passed + s.failed;
    s
}

// ── agent_done ────────────────────────────────────────────────────

async fn exec_agent_done(args: &AgentDoneArgs) -> Result<ToolResult, ToolError> {
    Ok(ToolResult::ok(json!({
        "done": true,
        "reason": args.reason,
    })))
}

// ── role-transition virtual primitives ────────────────────────────
//
// These don't execute work; they thread payload into the
// RoleDossier downstream. Returning a structured payload lets the
// role-aware runner (and the bench's telemetry) record exactly
// what was emitted without re-parsing.

async fn exec_agent_plan(args: &AgentPlanArgs) -> Result<ToolResult, ToolError> {
    Ok(ToolResult::ok(json!({
        "kind": "plan",
        "plan": args.plan,
        "files_to_create": args.files_to_create,
    })))
}

async fn exec_handoff_to_evaluator(args: &HandoffToEvaluatorArgs) -> Result<ToolResult, ToolError> {
    Ok(ToolResult::ok(json!({
        "kind": "handoff",
        "to": "evaluator",
        "what_you_changed": args.what_you_changed,
    })))
}

async fn exec_handoff_to_implementer(
    args: &HandoffToImplementerArgs,
) -> Result<ToolResult, ToolError> {
    Ok(ToolResult::ok(json!({
        "kind": "handoff",
        "to": "implementer",
        "diagnosis": args.diagnosis,
    })))
}

// ── shared helpers ────────────────────────────────────────────────

/// Reject paths that escape the workdir (no `..` traversal,
/// absolute paths are clamped). This is a structural invariant per
/// ARCH §7.1 — the canonical layer cannot be coerced into touching
/// files outside its workdir.
fn resolve_workdir_path(workdir: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(ToolError::WorkdirAccess(format!(
            "absolute path not allowed: {rel}"
        )));
    }
    // Reject `..` components rather than canonicalize — this stays
    // safe whether the path exists yet or not.
    for comp in candidate.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(ToolError::WorkdirAccess(format!(
                "parent-dir traversal not allowed: {rel}"
            )));
        }
    }
    Ok(workdir.join(candidate))
}

/// Spawn a shell command in `workdir` (`sh -c <cmd>`), wait with
/// wall-cap, return (status_ok, stdout+stderr tail). Wall-cap
/// fires SIGKILL via `kill_on_drop(true)`. Tail capped at 16 KiB
/// so the model doesn't drown in cargo output.
///
/// Shell form (vs argv form) means the bound build/verify commands
/// can include redirections + pipes (`2>&1`, `| head`) without
/// the executor parsing shell grammar. The per-problem commands
/// are operator-trusted; the workdir is sandboxed to the bench's
/// tempdir.
async fn run_shell(
    workdir: &Path,
    cmd: &str,
    wall_cap: Duration,
    primitive: &'static str,
) -> Result<(bool, String), ToolError> {
    use std::process::Stdio;
    use tokio::process::Command;

    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Put the shell into its OWN process group so we can kill the
    // whole group (sh + its children like pytest) on timeout. Without
    // this, kill_on_drop only kills the direct child (sh); grandchild
    // pytest is reparented to init and keeps spinning. Observed 4.2
    // 2026-05-23: pytest at 100% CPU for 30 min after bench's sh kill
    // because pytest was the grandchild.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|e| ToolError::Subprocess {
        primitive,
        reason: format!("spawn: {e}"),
    })?;

    // Wait with timeout.
    let wait_future = async {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let status = child.wait().await.map_err(|e| ToolError::Subprocess {
            primitive,
            reason: format!("wait: {e}"),
        })?;
        let mut out = String::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_string(&mut out).await;
        }
        if let Some(mut s) = stderr {
            let _ = s.read_to_string(&mut out).await;
        }
        Ok::<_, ToolError>((status.success(), out))
    };

    match tokio::time::timeout(wall_cap, wait_future).await {
        Ok(Ok((status_ok, combined))) => {
            let tail = cap_tail(&combined, 16 * 1024);
            Ok((status_ok, tail))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            // Timeout fired. kill_on_drop SIGKILLs the direct sh
            // child as the Child handle drops, but grandchildren
            // (pytest spawned BY sh) would be reparented to init and
            // keep running. Shell out to `kill -KILL -- -PGID` to
            // kill the whole process group cleanly. Best-effort — sh
            // may have already exited and reaped, in which case PGID
            // refers to nothing and kill returns ESRCH.
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                let pgid_arg = format!("-{pid}");
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", "--", &pgid_arg])
                    .status();
                tracing::warn!(
                    primitive,
                    pgid = pid,
                    wall_cap_secs = wall_cap.as_secs(),
                    "executor: subprocess timeout — killed process group"
                );
            }
            Err(ToolError::Timeout {
                primitive,
                secs: wall_cap.as_secs(),
            })
        }
    }
}

/// Minimal shell-escape for filter args appended to the bound
/// verify_cmd. Wraps in single quotes; replaces internal single
/// quotes with `'\''`. Sufficient for test-name filters; not
/// general-purpose.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Heuristic: does this look like a unified-diff or a copy of the
/// line-numbered file anchor rather than raw source?
///
/// Triggers when at least two non-empty lines in the first 20 begin
/// (after optional whitespace) with `+`, `-`, `| `, or `<digit>:`/`<digit> |`.
/// Tuned to be conservative — a single isolated `-` (negation) or
/// `+` (addition operator at start) won't trigger.
fn looks_like_diff(new_content: &str) -> bool {
    let mut diffish = 0;
    let mut sampled = 0;
    for line in new_content.lines().take(20) {
        if line.trim().is_empty() {
            continue;
        }
        sampled += 1;
        let t = line.trim_start();
        // Pipe-column anchor copy: `| ...` (with or without leading digits).
        if t.starts_with("| ") {
            diffish += 1;
            continue;
        }
        // Numbered-line prefix: `\d+:` or `\d+ |`.
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let after = &t[digits.len()..];
            if after.starts_with(": ") || after.starts_with(" | ") {
                diffish += 1;
                continue;
            }
        }
        // Unified-diff line markers. A leading `+` or `-` followed
        // by ANY identifier or whitespace is diffish — `-x = 1` and
        // `- x = 1` and `+ x = 1` and `+x = 1` are all diff-shaped.
        // Excluded: `+/-` followed by an operator (e.g. `+= 1`,
        // `-= 1`, `-1` digit literal at file-start is unusual but
        // not pursued here — those would not normally appear at
        // line-start in well-formed code).
        let bs = t.as_bytes();
        if (bs.first() == Some(&b'+') || bs.first() == Some(&b'-'))
            && bs.len() >= 2
            && (bs[1].is_ascii_alphabetic() || bs[1] == b'_' || bs[1] == b' ')
        {
            diffish += 1;
        }
    }
    // At least 2 diffish lines OR >50% of sampled lines.
    diffish >= 2 || (sampled >= 2 && diffish * 2 > sampled)
}

/// Strip a single leading line-number "gutter" from one source line, if
/// present. The line-numbered file view (`runners/native.rs` renders
/// source as `<pad><n>: <code>`); a model that echoes that format into a
/// `write_file` / `patch_file` content field emits lines like
/// `   25:     #[test] fn ...`, where the leading `25: ` is not valid
/// source and trips the pre-write syntax check. Returns `Some(rest)`
/// when the line begins (after optional indent) with `<digits>` then
/// `:` or `|` then an optional single space; `None` otherwise. Only the
/// gutter is removed — the code after it (including its own indentation)
/// is preserved verbatim.
fn strip_one_line_gutter(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None; // no leading number → not a gutter
    }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b':') | Some(b'|') => i += 1,
        _ => return None, // number not followed by `:`/`|` → not a gutter
    }
    if bytes.get(i) == Some(&b' ') {
        i += 1; // consume the single separator space
    }
    Some(&line[i..])
}

/// Strip echoed line-number gutters from every line of `content`,
/// returning `Some(cleaned)` if at least one line carried a gutter, else
/// `None`. Operates on `split('\n')` + LF re-join (the bench writes LF
/// source), preserving blank lines.
fn strip_echoed_line_number_gutters(content: &str) -> Option<String> {
    let mut any = false;
    let mut out = String::with_capacity(content.len());
    for (idx, line) in content.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        match strip_one_line_gutter(line) {
            Some(rest) => {
                any = true;
                out.push_str(rest);
            }
            None => out.push_str(line),
        }
    }
    if any {
        Some(out)
    } else {
        None
    }
}

/// Pre-write syntax gate with line-number-gutter recovery.
///
/// Runs the bound `SyntaxValidator` against `content`. On a clean parse
/// (or when no validator is bound / the path isn't handled), returns
/// `Ok(None)` — the caller writes the original content. On a parse
/// failure, makes ONE recovery attempt: strip echoed line-number gutters
/// (a model copying the line-numbered file view into its write) and
/// re-check. If that resolves every error, returns `Ok(Some(cleaned))`
/// and logs the recovery; otherwise the ORIGINAL errors surface as
/// `SyntaxRejected`.
///
/// The recovery is strictly safe: it only rewrites content that ALREADY
/// fails the check, and only adopts the stripped form when it parses
/// cleanly. Valid content (e.g. a Python dict whose integer key sits on
/// its own `25:` line) never enters the failure branch, so it is never
/// touched. Closes the "echo the line-numbered anchor into the write"
/// class observed 2026-06-03 (Qwen3.5-122B Implementer sticky-looping on
/// `SyntaxRejected:write_file`).
/// Repair a model "escaped-whitespace" artifact: a model emitting a
/// multi-line source file as one JSON string sometimes double-escapes its
/// newlines/tabs, so the decoded content carries the LITERAL two-char
/// sequences `\n` / `\t` where real whitespace belongs (observed
/// 2026-06-03: a 122B write landed `i += 1\n  continue\n...` as one
/// physical line → "unexpected character after line continuation
/// character"). Replace those literal escapes with the real char; returns
/// `Some` only when a replacement was made. Applied ONLY as a
/// post-failure candidate and adopted ONLY when the result parses clean,
/// so a genuine string literal containing a real `\n` escape is never
/// corrupted (that candidate would not parse and is discarded).
fn repair_escaped_whitespace(content: &str) -> Option<String> {
    if !content.contains("\\n") && !content.contains("\\t") {
        return None;
    }
    let repaired = content.replace("\\n", "\n").replace("\\t", "\t");
    if repaired == content {
        None
    } else {
        Some(repaired)
    }
}

/// Trim leading/trailing lines of a `patch_file` replacement that
/// DUPLICATE the unchanged lines just outside the spliced range. The
/// strict line-range splice does no context matching, so when the model
/// includes a line of surrounding context (diff convention) or is off by
/// one on `start_line`/`end_line`, that boundary line is duplicated
/// against the prefix/suffix and corrupts the file (observed 2026-06-03
/// on 5.1-minilang: a trailing `if op == "**":` context line duplicated
/// the next statement → "expected an indented block"). Returns the
/// trimmed replacement when a boundary overlap was found, else `None`.
/// Applied only as a post-failure recovery candidate, adopted only when
/// the re-spliced result parses clean — so a legitimate replacement that
/// genuinely repeats a boundary line is never silently shortened.
fn dedup_patch_boundary<'a>(
    prefix: &[&'a str],
    new_lines: &[&'a str],
    suffix: &[&'a str],
) -> Option<Vec<&'a str>> {
    // Largest trailing overlap: tail of new_lines == head of suffix.
    let max_tail = new_lines.len().min(suffix.len());
    let mut tail = 0;
    for k in (1..=max_tail).rev() {
        if new_lines[new_lines.len() - k..] == suffix[..k] {
            tail = k;
            break;
        }
    }
    // Largest leading overlap among the remaining lines: head of new_lines
    // == tail of prefix.
    let remaining = new_lines.len() - tail;
    let max_head = remaining.min(prefix.len());
    let mut head = 0;
    for j in (1..=max_head).rev() {
        if new_lines[..j] == prefix[prefix.len() - j..] {
            head = j;
            break;
        }
    }
    if head == 0 && tail == 0 {
        return None;
    }
    Some(new_lines[head..new_lines.len() - tail].to_vec())
}

fn syntax_gate_with_gutter_recovery(
    ctx: &ExecCtx,
    primitive: &'static str,
    path: &str,
    content: &str,
    extra_candidates: &[String],
) -> Result<Option<String>, ToolError> {
    let Some(validator) = ctx.syntax_validator.as_ref() else {
        return Ok(None);
    };
    let handled = validator
        .language_extensions()
        .iter()
        .any(|ext| path.ends_with(ext));
    if !handled {
        return Ok(None);
    }
    let errors = validator.check_file(std::path::Path::new(path), content);
    if errors.is_empty() {
        return Ok(None);
    }
    // Recovery: the content may carry a boundary-level formatting
    // artifact that the syntax check correctly rejects but that the model
    // then re-emits verbatim, sticky-looping. Two are known (2026-06-03,
    // 5.1-minilang):
    //   - line-number gutters echoed from the file view (`25: code`)
    //   - double-escaped whitespace: a multi-line file emitted as one
    //     physical line with literal `\n` / `\t` (`i += 1\n  continue\n`)
    // Try each conservative repair and adopt the FIRST that parses clean.
    // Strictly safe: only content that ALREADY failed is rewritten, and
    // only a candidate that parses is adopted — valid source (e.g. a
    // string literal with a real `\n` escape) never enters this branch,
    // and if it did the repaired form wouldn't parse and isn't adopted.
    //
    // Caller-supplied candidates (e.g. a `patch_file` boundary-dedup) are
    // tried FIRST — they are the most targeted repair for their primitive.
    for cand in extra_candidates {
        if validator
            .check_file(std::path::Path::new(path), cand)
            .is_empty()
        {
            tracing::info!(
                path = %path,
                primitive,
                repair = "caller-candidate",
                language = validator.language_id(),
                "commonwealth_agent_tools::executor: recovered write — a caller-supplied repair candidate parsed clean"
            );
            return Ok(Some(cand.clone()));
        }
    }
    for (label, candidate) in [
        ("escaped-whitespace", repair_escaped_whitespace(content)),
        ("line-number gutters", strip_echoed_line_number_gutters(content)),
    ] {
        if let Some(candidate) = candidate {
            if validator
                .check_file(std::path::Path::new(path), &candidate)
                .is_empty()
            {
                tracing::info!(
                    path = %path,
                    primitive,
                    repair = label,
                    language = validator.language_id(),
                    "commonwealth_agent_tools::executor: recovered write — repaired a model formatting artifact the syntax check rejected"
                );
                return Ok(Some(candidate));
            }
        }
    }
    let rendered = validator.render_errors(&errors);
    let language = validator.language_id();
    tracing::info!(
        path = %path,
        language,
        error_count = errors.len(),
        primitive,
        "commonwealth_agent_tools::executor: pre-write syntax check rejected"
    );
    Err(ToolError::SyntaxRejected {
        primitive,
        language: language.to_string(),
        rendered_errors: rendered,
    })
}

fn cap_tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    // Walk forward from `s.len() - limit` to the next char boundary
    // so we never slice mid-codepoint (em-dashes in pytest diff
    // output, etc.).
    let mut cut = s.len() - limit;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    format!("... (truncated {cut} leading bytes) ...\n{}", &s[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_gutter_removes_echoed_line_number() {
        // The failure shape observed 2026-06-03: a `25:` gutter copied
        // from the line-numbered file view into a write_file content.
        assert_eq!(strip_one_line_gutter("   25: pub fn f() {}"), Some("pub fn f() {}"));
        assert_eq!(strip_one_line_gutter("1: x"), Some("x"));
        assert_eq!(strip_one_line_gutter("  10 | code"), Some("code"));
    }

    #[test]
    fn strip_gutter_leaves_real_code_untouched() {
        assert_eq!(strip_one_line_gutter("    let x = 1;"), None);
        assert_eq!(strip_one_line_gutter("pub fn reverse() {}"), None);
        // digit then space+operator is arithmetic, not a gutter
        assert_eq!(strip_one_line_gutter("5 + 3 == 8"), None);
    }

    #[test]
    fn strip_echoed_gutters_only_when_present() {
        // Clean block → None (no change, no allocation churn).
        assert!(strip_echoed_line_number_gutters("fn a() {}\nfn b() {}").is_none());
        // One echoed gutter line → repaired, the rest preserved.
        let got = strip_echoed_line_number_gutters("fn a() {}\n   25: fn b() {}").unwrap();
        assert_eq!(got, "fn a() {}\nfn b() {}");
    }

    #[test]
    fn repair_escaped_whitespace_unescapes_literal_newlines() {
        // The 5.1-minilang trial-0 bomb: a body emitted as one physical
        // line with literal `\n` (double-escaped). Un-escaping → newlines.
        assert_eq!(
            repair_escaped_whitespace("def f():\\n    return 1\\n").unwrap(),
            "def f():\n    return 1\n"
        );
        assert_eq!(repair_escaped_whitespace("a\\tb").unwrap(), "a\tb");
        // Clean content (real newlines, no literal escapes) → None.
        assert!(repair_escaped_whitespace("def f():\n    return 1\n").is_none());
    }

    #[test]
    fn dedup_patch_boundary_trims_trailing_context_dup() {
        // The 5.1-minilang bomb: the replacement's trailing line duplicates
        // the suffix's first line (off-by-one on end_line / diff-style
        // context). It must be trimmed so the splice doesn't duplicate it.
        let prefix = ["        lv = a;"];
        let new_lines = [
            "        if op == \"/\":",
            "            return lv // rv",
            "        if op == \"**\":",
        ];
        let suffix = ["        if op == \"**\":", "            return lv ** rv"];
        let got = dedup_patch_boundary(&prefix, &new_lines, &suffix).unwrap();
        assert_eq!(
            got,
            vec!["        if op == \"/\":", "            return lv // rv"]
        );
    }

    #[test]
    fn dedup_patch_boundary_trims_leading_context_dup() {
        let prefix = ["def f():", "    x = 1"];
        let new_lines = ["    x = 1", "    return x * 3"];
        let suffix = ["    return x * 2"];
        let got = dedup_patch_boundary(&prefix, &new_lines, &suffix).unwrap();
        assert_eq!(got, vec!["    return x * 3"]);
    }

    #[test]
    fn dedup_patch_boundary_none_when_no_overlap() {
        let prefix = ["a"];
        let new_lines = ["x", "y"];
        let suffix = ["b"];
        assert!(dedup_patch_boundary(&prefix, &new_lines, &suffix).is_none());
    }

    #[test]
    fn resolve_workdir_path_rejects_absolute() {
        let wd = std::path::PathBuf::from("/tmp/workdir");
        assert!(resolve_workdir_path(&wd, "/etc/passwd").is_err());
    }

    #[test]
    fn resolve_workdir_path_rejects_parent_traversal() {
        let wd = std::path::PathBuf::from("/tmp/workdir");
        assert!(resolve_workdir_path(&wd, "../outside").is_err());
        assert!(resolve_workdir_path(&wd, "src/../../outside").is_err());
    }

    #[test]
    fn resolve_workdir_path_accepts_relative() {
        let wd = std::path::PathBuf::from("/tmp/workdir");
        let r = resolve_workdir_path(&wd, "src/lib.rs").unwrap();
        assert_eq!(r, std::path::PathBuf::from("/tmp/workdir/src/lib.rs"));
    }

    #[test]
    fn cap_tail_preserves_short_strings() {
        assert_eq!(cap_tail("hello", 100), "hello");
    }

    #[test]
    fn cap_tail_truncates_leading() {
        let s = "x".repeat(50);
        let capped = cap_tail(&s, 10);
        assert!(capped.starts_with("... (truncated"));
        assert!(capped.ends_with(&"x".repeat(10)));
    }

    #[test]
    fn parse_libtest_summary_extracts_pass_fail() {
        let out = "running 3 tests\n\
                   test foo ... ok\n\
                   test bar ... FAILED\n\
                   test baz ... ok\n\
                   \n\
                   test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured\n";
        let s = parse_libtest_summary(out);
        assert_eq!(s.passed, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.total, 3);
        assert_eq!(s.failed_names, vec!["bar".to_string()]);
    }

    #[tokio::test]
    async fn write_file_then_inspect_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());

        let write = Primitive::WriteFile(WriteFileArgs {
            path: "src/lib.rs".into(),
            content: "pub fn x() -> u8 { 1 }\n".into(),
        });
        let r = execute(&ctx, &write).await.unwrap();
        assert!(r.ok);

        let inspect = Primitive::InspectWorkdir(InspectIntent::File {
            path: "src/lib.rs".into(),
        });
        let r2 = execute(&ctx, &inspect).await.unwrap();
        let content = r2.payload.get("content").and_then(|v| v.as_str()).unwrap();
        assert!(content.contains("pub fn x()"));
    }

    #[tokio::test]
    async fn agent_done_returns_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let p = Primitive::AgentDone(AgentDoneArgs {
            reason: "all green".into(),
        });
        let r = execute(&ctx, &p).await.unwrap();
        assert_eq!(
            r.payload.get("reason").and_then(|v| v.as_str()),
            Some("all green")
        );
        assert_eq!(r.payload.get("done").and_then(|v| v.as_bool()), Some(true));
    }

    #[tokio::test]
    async fn write_file_pre_check_accepts_valid_rust() {
        // Pre-write check accepts well-formed code and writes to disk.
        use crate::syntax::RustSyntaxValidator;
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf())
            .with_syntax_validator(Arc::new(RustSyntaxValidator::new()));
        let write = Primitive::WriteFile(WriteFileArgs {
            path: "src/lib.rs".into(),
            content: "pub fn x() -> u8 { 1 }\n".into(),
        });
        let r = execute(&ctx, &write).await.unwrap();
        assert!(r.ok);
        assert!(tmp.path().join("src/lib.rs").is_file());
    }

    #[tokio::test]
    async fn write_file_pre_check_rejects_broken_rust_without_disk_touch() {
        // The §-pre-write invariant: broken content must NOT land on
        // disk. If a future PR softens this (e.g. writes the file
        // anyway "in case the next turn cleans it up"), the build
        // cycle is back to discovering syntax defects via cargo and
        // the prose-in-source class re-opens.
        use crate::syntax::RustSyntaxValidator;
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf())
            .with_syntax_validator(Arc::new(RustSyntaxValidator::new()));
        let write = Primitive::WriteFile(WriteFileArgs {
            path: "src/lib.rs".into(),
            // Missing closing brace — syn rejects.
            content: "pub fn x() -> u8 { 1 ".into(),
        });
        let err = execute(&ctx, &write).await.unwrap_err();
        assert!(matches!(err, ToolError::SyntaxRejected { .. }));
        // Disk must be untouched.
        assert!(!tmp.path().join("src/lib.rs").exists());
    }

    #[tokio::test]
    async fn patch_file_replaces_line_range() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.txt".into(),
            start_line: 2,
            end_line: 3,
            new_content: "BETA\nGAMMA".into(),
        });
        let r = execute(&ctx, &patch).await.unwrap();
        assert!(r.ok);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "alpha\nBETA\nGAMMA\ndelta\n");
        assert_eq!(
            r.payload.get("lines_replaced").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            r.payload.get("lines_inserted").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn patch_file_empty_new_content_deletes_range() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.txt".into(),
            start_line: 2,
            end_line: 3,
            new_content: String::new(),
        });
        let r = execute(&ctx, &patch).await.unwrap();
        assert!(r.ok);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "alpha\ndelta\n");
    }

    #[tokio::test]
    async fn patch_file_out_of_range_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        // file has 2 lines; patching at line 10 is invalid.
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.txt".into(),
            start_line: 10,
            end_line: 10,
            new_content: "X".into(),
        });
        let err = execute(&ctx, &patch).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        // file unchanged.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
    }

    #[test]
    fn looks_like_diff_detects_unified_diff_markers() {
        // The exact 4.2 2026-05-23 failure shape: model emits
        // diff-style content with `+/-` markers and `| N | ` line
        // columns copied from the anchor display.
        let diff = "| 54 | def tokenize(source):\n   |     result = []\n-     i = 0\n+     i = 0\n";
        assert!(looks_like_diff(diff), "diff-like content must trip");
    }

    #[test]
    fn looks_like_diff_detects_numbered_line_prefix() {
        // Model copying anchor's `N: content` format into the patch.
        let anchored = "54: def tokenize(source):\n55:     result = []\n";
        assert!(looks_like_diff(anchored), "numbered-prefix copy must trip");
    }

    #[test]
    fn looks_like_diff_does_not_trip_on_raw_python() {
        let raw = "def tokenize(source):\n    result = []\n    i = 0\n    while i < len(source):\n        result.append(source[i])\n        i += 1\n    return result\n";
        assert!(!looks_like_diff(raw), "raw Python must not trip");
    }

    #[test]
    fn looks_like_diff_does_not_trip_on_single_negation() {
        // `-x` (unary minus) at line start is legitimate Python.
        let neg = "def f(x):\n    return -x\n";
        assert!(!looks_like_diff(neg));
    }

    #[tokio::test]
    async fn patch_file_rejects_diff_format_new_content() {
        // Closes the 4.2-observed class: model emits unified-diff
        // content instead of raw replacement. Pre-existing-content-
        // check rejection short-circuits the file read so this
        // works without an existing file in the workdir.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.py"), "x = 1\n").unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.py".into(),
            start_line: 1,
            end_line: 1,
            new_content: "| 1 | x = 2\n-x = 1\n+x = 2".into(),
        });
        let err = execute(&ctx, &patch).await.unwrap_err();
        match err {
            ToolError::InvalidArguments { primitive, reason } => {
                assert_eq!(primitive, "patch_file");
                assert!(reason.contains("unified diff"));
                assert!(reason.contains("raw replacement"));
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
        // File must be unchanged.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.py")).unwrap(),
            "x = 1\n"
        );
    }

    #[tokio::test]
    async fn patch_file_redirects_whole_function_to_replace_function() {
        // Closes class: model habitually uses patch_file even when
        // the patch range is exactly a function's bounds. The
        // structural redirect names replace_function in the error
        // help so the model uses the right tool next attempt.
        let tmp = tempfile::tempdir().unwrap();
        let src = "def keep():\n    pass\n\ndef target():\n    return 1\n    return 2\n\ndef other():\n    pass\n";
        std::fs::write(tmp.path().join("a.py"), src).unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        // target() lives at lines 4-6 (1-indexed inclusive).
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.py".into(),
            start_line: 4,
            end_line: 6,
            new_content: "def target():\n    return 42".into(),
        });
        let err = execute(&ctx, &patch).await.unwrap_err();
        match err {
            ToolError::InvalidArguments { primitive, reason } => {
                assert_eq!(primitive, "patch_file");
                assert!(reason.contains("target"));
                assert!(reason.contains("replace_function"));
            }
            other => panic!("expected InvalidArguments redirect, got {other:?}"),
        }
        // File unchanged.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.py")).unwrap(),
            src
        );
    }

    #[tokio::test]
    async fn patch_file_partial_function_patch_not_redirected() {
        // A patch that's INSIDE a function (subset of its bounds)
        // is legitimate patch_file usage — should NOT be redirected.
        let tmp = tempfile::tempdir().unwrap();
        let src = "def foo():\n    x = 1\n    y = 2\n    return x + y\n";
        std::fs::write(tmp.path().join("a.py"), src).unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        // Just patch line 2.
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.py".into(),
            start_line: 2,
            end_line: 2,
            new_content: "    x = 100".into(),
        });
        let r = execute(&ctx, &patch).await.unwrap();
        assert!(r.ok);
    }

    #[tokio::test]
    async fn patch_file_pre_check_rejects_broken_result() {
        // Replace a body line with English prose — full post-patch
        // content should fail Python's parser and the write must not
        // land. Symmetric with write_file's pre-write check.
        use crate::syntax::PythonSyntaxValidator;
        use std::sync::Arc;
        // Skip when python3 absent.
        if std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf())
            .with_syntax_validator(Arc::new(PythonSyntaxValidator::new()));
        let path = tmp.path().join("a.py");
        let original = "def solve():\n    return 0\n";
        std::fs::write(&path, original).unwrap();
        let patch = Primitive::PatchFile(PatchFileArgs {
            path: "a.py".into(),
            start_line: 2,
            end_line: 2,
            new_content: "    let me redo Gaussian elimination more carefully.".into(),
        });
        let err = execute(&ctx, &patch).await.unwrap_err();
        assert!(matches!(err, ToolError::SyntaxRejected { .. }));
        // Original file must be intact.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn write_file_rejected_for_large_existing_file() {
        // §4.2-derived invariant: existing files above the threshold
        // must be edited via patch_file. The 4.2 smoke established
        // that the model accumulates token-level corruption (escape
        // confusion, language drift, lost whitespace) when emitting
        // 5000+ tokens of structured Python in one shot. Forcing
        // patch_file structurally removes the failing tool.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.py");
        let big_content = "x = 1\n".repeat(LARGE_FILE_REWRITE_THRESHOLD_LINES + 50);
        std::fs::write(&path, &big_content).unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let write = Primitive::WriteFile(WriteFileArgs {
            path: "big.py".into(),
            content: "x = 2\n".repeat(50),
        });
        let err = execute(&ctx, &write).await.unwrap_err();
        match err {
            ToolError::WriteFileTooLarge {
                path: p,
                existing_lines,
                threshold,
            } => {
                assert_eq!(p, "big.py");
                assert!(existing_lines > threshold);
                assert_eq!(threshold, LARGE_FILE_REWRITE_THRESHOLD_LINES);
            }
            other => panic!("expected WriteFileTooLarge, got {other:?}"),
        }
        // File must be unchanged.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), big_content);
    }

    #[tokio::test]
    async fn write_file_allowed_for_new_file_of_any_size() {
        // Net-new files have no patch_file alternative; the gate
        // must not fire on initial author. Pins this so a future PR
        // tightening the threshold doesn't accidentally block
        // FromScratch tier where the agent's whole job is authoring
        // a large file from nothing.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let large_initial = "pub fn x() {}\n".repeat(LARGE_FILE_REWRITE_THRESHOLD_LINES + 50);
        let write = Primitive::WriteFile(WriteFileArgs {
            path: "fresh.rs".into(),
            content: large_initial.clone(),
        });
        let r = execute(&ctx, &write).await.unwrap();
        assert!(r.ok);
        let landed = std::fs::read_to_string(tmp.path().join("fresh.rs")).unwrap();
        assert_eq!(landed, large_initial);
    }

    #[tokio::test]
    async fn write_file_allowed_for_existing_small_file() {
        // Existing-but-small files (≤ threshold) can still be
        // rewritten via write_file — patch_file isn't mandated, just
        // preferred. A 50-line module rewrite is well within the
        // model's reliable-generation range.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("small.py");
        std::fs::write(&path, "x = 1\n".repeat(50)).unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf());
        let write = Primitive::WriteFile(WriteFileArgs {
            path: "small.py".into(),
            content: "y = 2\n".repeat(60),
        });
        let r = execute(&ctx, &write).await.unwrap();
        assert!(r.ok);
    }

    #[tokio::test]
    async fn write_file_pre_check_skips_unhandled_extensions() {
        // Validator handles `.rs`; a `.md` write must pass through.
        use crate::syntax::RustSyntaxValidator;
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ExecCtx::new(tmp.path().to_path_buf())
            .with_syntax_validator(Arc::new(RustSyntaxValidator::new()));
        let write = Primitive::WriteFile(WriteFileArgs {
            path: "README.md".into(),
            content: "# Title\n\nIntentionally not valid Rust { ( ;".into(),
        });
        let r = execute(&ctx, &write).await.unwrap();
        assert!(r.ok);
        assert!(tmp.path().join("README.md").is_file());
    }
}
