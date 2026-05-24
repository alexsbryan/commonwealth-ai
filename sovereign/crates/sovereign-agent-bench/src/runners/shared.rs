//! Shared infrastructure for the search-not-agent and bare-metal
//! runners. Both runners emit a JSON action + raw code block, apply
//! it to a workdir, and run tests; the differences (sample-and-pick
//! vs single-shot) live in their respective modules.
//!
//! Design notes (2026-05-24 session learnings):
//!
//! 1. **No defensive parsing.** The model's emitted code lands
//!    verbatim. Pre-write syntax check rejects malformed; downstream
//!    handles recovery (search retries via diversity, bare-metal
//!    reports the rejection).
//!
//! 2. **Full directory snapshots.** Per-candidate workdir copies
//!    (not just source files) so tests directories and any model-
//!    created scaffolding survive the snapshot round-trip.
//!
//! 3. **Tests as the only judge.** Test-pass count is the canonical
//!    fitness function. No LLM-eval, no rubric scoring, no role
//!    diagnosis — those live downstream in the witness layer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use commonwealth_agent_tools::executor::{execute, ExecCtx};
use commonwealth_agent_tools::{
    PatchFileArgs, Primitive, ReplaceFunctionArgs, ToolError, WriteFileArgs,
};
use regex::Regex;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::problem::WitnessLanguage;
use crate::witness::test_result_parser::{parse_test_output, TestParseResult};

// ── edit action schema ───────────────────────────────────────────

/// Discriminated edit shape the model chooses per turn.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EditAction {
    /// Rewrite an entire named function or class.
    RewriteFunction { name: String },
    /// Replace lines `start..=end` (1-indexed inclusive).
    PatchLines { start: u32, end: u32 },
    /// Insert before `line` (1-indexed); existing content at and
    /// below `line` is preserved.
    InsertBefore { line: u32 },
    /// Replace the entire file contents.
    WriteFile,
}

/// Parsed model output: an EditAction plus the python/rust/etc.
/// source block to apply with it.
#[derive(Debug, Clone)]
pub struct ParsedResponse {
    pub action: EditAction,
    pub body: String,
}

/// Extract the JSON action header and the source code block from the
/// model's chat response. Returns None when either is missing or the
/// JSON doesn't conform to EditAction. Tolerates the model emitting
/// the JSON inline (without a fence) and tolerates missing closing
/// fences on the source block (truncation-friendly).
pub fn parse_response(content: &str) -> Option<ParsedResponse> {
    let action = parse_action_json(content)?;
    let body = parse_source_block(content)?;
    Some(ParsedResponse { action, body })
}

fn parse_action_json(content: &str) -> Option<EditAction> {
    // Prefer ```json fenced block.
    let fenced = Regex::new(r"(?s)```json\s*\n(\{[^`]*?\})\s*\n```").unwrap();
    if let Some(c) = fenced.captures(content) {
        if let Ok(v) = serde_json::from_str::<EditAction>(&c[1]) {
            return Some(v);
        }
    }
    // Inline {"action": ...} object.
    let inline = Regex::new(r#"(\{[^{}]*?"action"\s*:\s*"[^"]+?"[^{}]*?\})"#).unwrap();
    if let Some(c) = inline.captures(content) {
        if let Ok(v) = serde_json::from_str::<EditAction>(&c[1]) {
            return Some(v);
        }
    }
    None
}

fn parse_source_block(content: &str) -> Option<String> {
    // Closed code fence (any language tag, prefer specific languages).
    let closed = Regex::new(r"(?s)```(?:python|py|rust|rs|go|ts|tsx|js)\s*\n(.*?)```").unwrap();
    if let Some(c) = closed.captures(content) {
        return Some(c[1].to_string());
    }
    // Any closed code fence (skip the json one — handled by caller).
    let any_closed = Regex::new(r"(?s)```(\w*)\s*\n(.*?)```").unwrap();
    let mut last_non_json: Option<String> = None;
    for cap in any_closed.captures_iter(content) {
        if !cap[1].eq_ignore_ascii_case("json") {
            last_non_json = Some(cap[2].to_string());
        }
    }
    if let Some(b) = last_non_json {
        return Some(b);
    }
    // Truncation-friendly: opening fence without close.
    let open_only = Regex::new(r"(?s)```(?:python|py|rust|rs)\s*\n(.*)").unwrap();
    if let Some(c) = open_only.captures(content) {
        return Some(c[1].to_string());
    }
    None
}

// ── edit application ─────────────────────────────────────────────

/// Convert a parsed response into a Primitive and dispatch via the
/// shared executor. `source_file` is the agent's target file
/// (e.g. `"evaluator.py"`) — most actions need a path argument.
pub async fn apply_edit(
    ctx: &ExecCtx,
    source_file: &str,
    response: &ParsedResponse,
) -> Result<(), ToolError> {
    let primitive = match &response.action {
        EditAction::RewriteFunction { name } => Primitive::ReplaceFunction(ReplaceFunctionArgs {
            path: source_file.to_string(),
            name: name.clone(),
            new_body: response.body.clone(),
        }),
        EditAction::PatchLines { start, end } => Primitive::PatchFile(PatchFileArgs {
            path: source_file.to_string(),
            start_line: *start,
            end_line: *end,
            new_content: response.body.clone(),
        }),
        EditAction::InsertBefore { line } => {
            // No InsertBefore primitive exists; emulate by patching
            // the (line, line-1) empty range. PatchFile with
            // start=line, end=line-1 would be invalid; instead patch
            // lines (line..=line) and prepend our body to the
            // existing line content. We read the file directly here
            // and use WriteFile semantics for the relevant lines.
            //
            // Simpler: PatchFile with start=line, end=line - 1 is
            // rejected by the executor's range validation, so we
            // implement insertion via patch_lines(line, line-1) NO —
            // we do an inline file rewrite: read existing, splice in
            // body before line N, write_file the whole thing.
            //
            // To avoid the WriteFile-too-large gate, we use patch_lines
            // covering JUST line N replaced with (body + original
            // content of line N). That's a 1-line replace producing
            // body.lines + 1 lines of output.
            let abs = ctx.workdir.join(source_file);
            let existing = tokio::fs::read_to_string(&abs)
                .await
                .map_err(|e| ToolError::Filesystem {
                    primitive: "insert_before",
                    reason: format!("read {source_file}: {e}"),
                })?;
            let lines: Vec<&str> = existing.lines().collect();
            let line_idx = (*line as usize).saturating_sub(1);
            if line_idx > lines.len() {
                return Err(ToolError::InvalidArguments {
                    primitive: "insert_before",
                    reason: format!(
                        "line {line} out of range for {source_file} ({} lines)",
                        lines.len()
                    ),
                });
            }
            // The patch replaces just `line` with body + line's original content.
            let existing_at_line = lines.get(line_idx).copied().unwrap_or("");
            let new_content = if line_idx >= lines.len() {
                response.body.clone()
            } else {
                format!("{}\n{existing_at_line}", response.body.trim_end_matches('\n'))
            };
            Primitive::PatchFile(PatchFileArgs {
                path: source_file.to_string(),
                start_line: *line,
                end_line: *line,
                new_content,
            })
        }
        EditAction::WriteFile => Primitive::WriteFile(WriteFileArgs {
            path: source_file.to_string(),
            content: response.body.clone(),
        }),
    };
    execute(ctx, &primitive).await.map(|_| ())
}

// ── workdir snapshot / restore ───────────────────────────────────

/// Copy `src` to `dst` recursively. Used to snapshot a workdir per
/// candidate so test runs against one candidate don't pollute the
/// state for the others. Existing content at `dst` is removed first.
/// Skips heavy build-output directories (target, node_modules, etc.).
pub fn snapshot_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_dir_filtered(src, dst)
}

fn copy_dir_filtered(src: &Path, dst: &Path) -> std::io::Result<()> {
    const SKIP: &[&str] = &["target", "node_modules", ".git", "__pycache__", ".pytest_cache"];
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if SKIP.iter().any(|s| *s == name_str) {
            continue;
        }
        let s = entry.path();
        let d = dst.join(&name);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_filtered(&s, &d)?;
        } else if ft.is_file() {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

// ── test execution ───────────────────────────────────────────────

/// Run the bench's verify command in `workdir` with a per-candidate
/// timeout and return parsed test results. Wraps subprocess
/// management and exit-code/timeout signal capture.
pub async fn run_tests(
    workdir: &Path,
    verify_cmd: &str,
    language: WitnessLanguage,
    timeout: Duration,
) -> TestRunResult {
    if verify_cmd.trim().is_empty() {
        return TestRunResult::empty("verify_cmd is empty");
    }
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(verify_cmd)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return TestRunResult::empty(&format!("spawn failed: {e}")),
    };
    let pid = child.id();
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return TestRunResult::empty(&format!("wait failed: {e}")),
        Err(_) => {
            #[cfg(unix)]
            if let Some(p) = pid {
                let pgid = format!("-{p}");
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", "--", &pgid])
                    .status();
            }
            return TestRunResult::empty(&format!("timeout after {}s", timeout.as_secs()));
        }
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        combined.push_str("\n---stderr---\n");
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let parsed = parse_test_output(language, &combined);
    TestRunResult {
        parsed,
        tail: tail(&combined, 1500),
    }
}

#[derive(Debug, Clone)]
pub struct TestRunResult {
    pub parsed: TestParseResult,
    /// Last ~1.5 KB of combined stdout/stderr for the prompt.
    pub tail: String,
}

impl TestRunResult {
    fn empty(reason: &str) -> Self {
        Self {
            parsed: TestParseResult {
                passed: 0,
                failed: 0,
                total: 0,
                failed_names: vec![],
            },
            tail: reason.to_string(),
        }
    }
}

// ── source file discovery ─────────────────────────────────────────

/// Find the primary source file in the agent's workdir. Returns the
/// first .py / .rs / .ts file at the workdir root that isn't named
/// "test_*". Returns None when no candidate is present.
pub fn discover_source_file(workdir: &Path) -> Option<String> {
    let exts = [".py", ".rs", ".ts", ".tsx", ".go"];
    let entries = std::fs::read_dir(workdir).ok()?;
    let mut hits: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            !n.starts_with("test_")
                && exts.iter().any(|ext| n.ends_with(ext))
        })
        .collect();
    hits.sort();
    hits.into_iter()
        .next()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
}

// ── prompt rendering ──────────────────────────────────────────────

/// Render a file's contents prefixed with right-aligned line numbers.
/// Mirrors the format the v2/v3 isolation probes used — the model
/// references line numbers in its JSON action.
pub fn render_with_line_numbers(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let width = lines.len().to_string().len();
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>w$}: {l}", i + 1, w = width))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── small utilities ──────────────────────────────────────────────

fn tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk back to a char boundary so multi-byte chars don't split.
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("... (truncated)\n{}", &s[start..])
}

// ── HTTP transport ───────────────────────────────────────────────

/// POST a chat-completion request body to the provider URL and parse
/// the response. Returns the raw JSON value on success or an error
/// string suitable for logging.
pub async fn post_chat_completion(
    http: &reqwest::Client,
    provider_url: &str,
    body: &Value,
) -> Result<Value, String> {
    let url = format!("{provider_url}/chat/completions");
    let resp = http
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "daemon {status}: {}",
            text.chars().take(500).collect::<String>()
        ));
    }
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "parse: {e} (body: {})",
            text.chars().take(500).collect::<String>()
        )
    })
}

/// Build a minimal chat-completion request body. Used by both
/// search and bare-metal — they layer different message structures
/// but share the same HTTP envelope.
pub fn chat_body(
    model: &str,
    messages: Vec<Value>,
    temperature: Option<f32>,
    max_tokens: u32,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": false,
    });
    if let Some(t) = temperature {
        body["temperature"] = json!(t);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_extracts_rewrite_function_action() {
        let content = r#"
Some prose.

```json
{"action": "rewrite_function", "name": "tokenize"}
```

```python
def tokenize(source):
    return []
```
"#;
        let r = parse_response(content).expect("parse should succeed");
        match r.action {
            EditAction::RewriteFunction { name } => assert_eq!(name, "tokenize"),
            other => panic!("expected RewriteFunction, got {other:?}"),
        }
        assert!(r.body.contains("def tokenize"));
    }

    #[test]
    fn parse_response_extracts_patch_lines_action() {
        let content = r#"
```json
{"action": "patch_lines", "start": 12, "end": 18}
```

```python
    return 42
```
"#;
        let r = parse_response(content).expect("parse should succeed");
        match r.action {
            EditAction::PatchLines { start, end } => {
                assert_eq!(start, 12);
                assert_eq!(end, 18);
            }
            other => panic!("expected PatchLines, got {other:?}"),
        }
    }

    #[test]
    fn parse_response_extracts_insert_before_action() {
        let content = r#"
```json
{"action": "insert_before", "line": 89}
```

```python
if c == "<" and i + 1 < n and source[i + 1] == "=":
    tokens.append(Token("LE", "<=", i))
    i += 2
    continue
```
"#;
        let r = parse_response(content).expect("parse should succeed");
        match r.action {
            EditAction::InsertBefore { line } => assert_eq!(line, 89),
            other => panic!("expected InsertBefore, got {other:?}"),
        }
    }

    #[test]
    fn parse_response_handles_unterminated_code_block() {
        // Truncated mid-emission: opening fence, no close. The probe
        // v3 measurement on Bug 1 showed this happens at low
        // temperature — the parser must still extract what it can so
        // the search loop can score the candidate (it'll fail syntax
        // check downstream, which is the correct signal).
        let content = r#"
```json
{"action": "patch_lines", "start": 89, "end": 100}
```

```python
if c == "<"
    "#;
        let r = parse_response(content).expect("should still parse");
        assert!(r.body.contains("if c ==")); // truncated but extracted
    }

    #[test]
    fn parse_response_returns_none_for_missing_action() {
        let content = "no action here, just prose";
        assert!(parse_response(content).is_none());
    }

    #[test]
    fn render_with_line_numbers_pads_to_widest_index() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.py");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let out = render_with_line_numbers(&path);
        // 3 lines → width=1, no padding
        assert!(out.starts_with("1: a"));
        assert!(out.contains("\n2: b"));
        assert!(out.contains("\n3: c"));
    }

    #[test]
    fn discover_source_file_finds_python_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("evaluator.py"), "pass\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::write(tmp.path().join("tests/test_x.py"), "pass\n").unwrap();
        let f = discover_source_file(tmp.path()).expect("should find file");
        assert_eq!(f, "evaluator.py");
    }

    #[test]
    fn discover_source_file_skips_test_files_at_root() {
        // Some scaffolds have test_*.py at the root; those are tests
        // not the primary source.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("test_x.py"), "pass\n").unwrap();
        std::fs::write(tmp.path().join("config_applier.py"), "pass\n").unwrap();
        let f = discover_source_file(tmp.path()).expect("should find file");
        assert_eq!(f, "config_applier.py");
    }

    #[test]
    fn snapshot_dir_round_trip_preserves_content() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.py"), "x = 1\n").unwrap();
        std::fs::create_dir_all(src.path().join("tests")).unwrap();
        std::fs::write(src.path().join("tests/test_a.py"), "def test_a(): pass\n").unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("snap");
        snapshot_dir(src.path(), &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("a.py")).unwrap(), "x = 1\n");
        assert_eq!(
            std::fs::read_to_string(dst.join("tests/test_a.py")).unwrap(),
            "def test_a(): pass\n"
        );
    }

    #[test]
    fn snapshot_dir_skips_build_dirs() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("target/debug")).unwrap();
        std::fs::write(src.path().join("target/debug/big_artifact"), "junk").unwrap();
        std::fs::write(src.path().join("a.py"), "x = 1\n").unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("snap");
        snapshot_dir(src.path(), &dst).unwrap();
        assert!(dst.join("a.py").exists());
        assert!(!dst.join("target").exists(), "target/ should be skipped");
    }
}
