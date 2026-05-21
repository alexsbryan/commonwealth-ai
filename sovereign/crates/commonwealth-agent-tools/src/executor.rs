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
    AgentDoneArgs, CargoSmokeArgs, InspectIntent, Primitive, WriteFileArgs,
};
use crate::result::{ToolError, ToolResult};

/// Bound execution context. Every primitive is workdir-relative;
/// nothing escapes the directory by design.
#[derive(Debug, Clone)]
pub struct ExecCtx {
    pub workdir: PathBuf,
    /// Wall-clock cap for any subprocess this primitive may spawn.
    /// `inspect_workdir` and `write_file` ignore this; `cargo_build`
    /// and `cargo_smoke` honor it.
    pub subprocess_wall_cap: Duration,
}

impl ExecCtx {
    pub fn new(workdir: PathBuf) -> Self {
        Self {
            workdir,
            subprocess_wall_cap: Duration::from_secs(120),
        }
    }

    pub fn with_subprocess_wall_cap(mut self, dur: Duration) -> Self {
        self.subprocess_wall_cap = dur;
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
        Primitive::CargoBuild => exec_cargo_build(ctx).await,
        Primitive::CargoSmoke(args) => exec_cargo_smoke(ctx, args).await,
        Primitive::AgentDone(args) => exec_agent_done(args).await,
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
            let bytes = tokio::fs::read(&abs).await.map_err(|e| ToolError::Filesystem {
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
            let mut rd = tokio::fs::read_dir(&abs).await.map_err(|e| ToolError::Filesystem {
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
                a.get("name").and_then(|v| v.as_str()).unwrap_or("").cmp(
                    b.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                )
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
        if p.file_name().is_some_and(|n| n.to_string_lossy().contains(needle)) {
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

async fn exec_write_file(ctx: &ExecCtx, args: &WriteFileArgs) -> Result<ToolResult, ToolError> {
    let abs = resolve_workdir_path(&ctx.workdir, &args.path)?;
    if let Some(parent) = abs.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let bytes = args.content.as_bytes();
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

// ── cargo_build ───────────────────────────────────────────────────

async fn exec_cargo_build(ctx: &ExecCtx) -> Result<ToolResult, ToolError> {
    let (status_ok, stdout_tail) = run_subprocess(
        &ctx.workdir,
        &["cargo", "build"],
        ctx.subprocess_wall_cap,
        "cargo_build",
    )
    .await?;
    Ok(ToolResult::ok(json!({
        "ok": status_ok,
        "stdout_tail": stdout_tail,
    })))
}

// ── cargo_smoke ───────────────────────────────────────────────────

async fn exec_cargo_smoke(
    ctx: &ExecCtx,
    args: &CargoSmokeArgs,
) -> Result<ToolResult, ToolError> {
    let mut argv: Vec<&str> = vec!["cargo", "test", "--quiet", "--test", "integration"];
    if let Some(filter) = args.filter.as_deref() {
        argv.push(filter);
    }
    let (status_ok, stdout_tail) =
        run_subprocess(&ctx.workdir, &argv, ctx.subprocess_wall_cap, "cargo_smoke").await?;
    // Parse libtest output into structured pass/fail counts.
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

/// Spawn a subprocess in `workdir`, wait for it with the given wall
/// cap, return (status_ok, captured_stdout_tail). Wall-cap fires
/// SIGKILL via `kill_on_drop(true)`. Output is capped at 16 KiB
/// tail to keep the tool result small (the model doesn't need 50
/// KB of cargo output to know "build failed").
async fn run_subprocess(
    workdir: &Path,
    argv: &[&str],
    wall_cap: Duration,
    primitive: &'static str,
) -> Result<(bool, String), ToolError> {
    use std::process::Stdio;
    use tokio::process::Command;

    let mut cmd = Command::new(argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    cmd.current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| ToolError::Subprocess {
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
        Err(_) => Err(ToolError::Timeout {
            primitive,
            secs: wall_cap.as_secs(),
        }),
    }
}

fn cap_tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        let cut = s.len() - limit;
        format!(
            "... (truncated {cut} leading bytes) ...\n{}",
            &s[cut..]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
