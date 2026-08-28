// SPDX-License-Identifier: AGPL-3.0-or-later
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

use std::path::Path;
use std::time::Duration;

use commonwealth_agent_tools::executor::{execute, ExecCtx};
use commonwealth_agent_tools::{
    PatchFileArgs, Primitive, ReplaceFunctionArgs, ToolError, WriteFileArgs,
};
use serde_json::{json, Value};
use tokio::process::Command;

/// The edit-action schema, response parser, workdir snapshotter,
/// source-file discovery and prompt renderer are `commonwealth_tdd`'s.
/// Re-exported here because both runners import them through
/// `runners::shared` — this module is now the bench's adapter over
/// the TDD machine's primitives rather than a second copy of them.
pub use commonwealth_tdd::shared::{discover_source_file, render_with_line_numbers, snapshot_dir};
pub use commonwealth_tdd::{EditAction, ParsedResponse, TestRunResult};

/// Parse a model turn into an edit action + source block, declining
/// `commonwealth_tdd`'s header-inference fallback.
///
/// The PARSING is tdd's. The ACCEPTANCE POLICY is the bench's, and
/// they differ on purpose. tdd will infer an edit shape from a bare
/// source block with no JSON action header, justified by ITS
/// monotonic fitness gate ("worst case equals the parse-fail this
/// replaces") — but the bench SCORES candidates instead of landing
/// only strict improvements, so that argument does not transfer.
/// Accepting inferred candidates here would make `agent-coding-gate`
/// — a HARD tail gate (`scripts/sovereign-ci-bench.sh:55`) — more
/// permissive across all 15 problems in every language, mechanically
/// raising scores against the five committed baselines in
/// `sovereign/bench/agent-coding/baselines/ci/` without any model
/// improvement. That is a change to the veto deciding whether a
/// candidate counts as an attempt at all: ARCH_PRINCIPLES §18.6, a
/// scoring decision to be taken on its own evidence and with a
/// re-baseline — not a ride-along on a deduplication commit.
///
/// This filter is therefore load-bearing, not redundant: deleting it
/// silently changes bench scores. Adopting the fallback deliberately
/// is filed as `bench-adopt-header-inference-fallback`.
pub fn parse_response(content: &str) -> Option<ParsedResponse> {
    let parsed = commonwealth_tdd::shared::parse_response(content)?;
    if parsed.inferred {
        return None;
    }
    Some(parsed)
}

use crate::problem::WitnessLanguage;
use crate::witness::test_result_parser::parse_test_output;

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
            let existing =
                tokio::fs::read_to_string(&abs)
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
                format!(
                    "{}\n{existing_at_line}",
                    response.body.trim_end_matches('\n')
                )
            };
            Primitive::PatchFile(PatchFileArgs {
                path: source_file.to_string(),
                start_line: *line,
                end_line: *line,
                new_content,
            })
        }
        // tdd's `WriteFile` carries an optional `path`; the bench
        // always routes to its discovered `source_file`, which is
        // what it did before adoption (its unit variant made serde
        // drop the key). Bench problems are single-file.
        EditAction::WriteFile { .. } => Primitive::WriteFile(WriteFileArgs {
            path: source_file.to_string(),
            content: response.body.clone(),
        }),
    };
    execute(ctx, &primitive).await.map(|_| ())
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

    /// The bench declines tdd's header-inference fallback. Without
    /// this filter the input below parses as an inferred
    /// `RewriteFunction` and the candidate is ACCEPTED, which makes
    /// the HARD `agent-coding-gate` more permissive across every
    /// problem and invalidates the five committed CI baselines with
    /// no model improvement (ARCH_PRINCIPLES §18.6). Named input, so
    /// the filter is a gate rather than a comment.
    #[test]
    fn parse_response_declines_tdd_header_inference() {
        let content = "```python\ndef tokenize(s):\n    return s.split()\n```";
        // tdd infers an action from the bare block ...
        let inferred = commonwealth_tdd::shared::parse_response(content)
            .expect("tdd infers an action from a bare source block");
        assert!(
            inferred.inferred,
            "guard is meaningless if tdd stops inferring"
        );
        // ... and the bench declines it, preserving HEAD's semantics.
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

    /// Tool configs are infrastructure, not source. They sort FIRST
    /// (fewest path components), so without an explicit filter a
    /// webapp's `playwright.config.ts` becomes the file the prompt
    /// points the model at — and candidates "fix" the test runner
    /// instead of the app (live receipts, job 09777dfe, 2026-07-07).
    #[test]
    fn discover_source_file_skips_tool_configs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("playwright.config.ts"),
            "export default {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/app.ts"), "export const x = 1;\n").unwrap();
        let f = discover_source_file(tmp.path()).expect("should find file");
        assert_eq!(f, "src/app.ts");
    }

    #[test]
    fn discover_source_file_finds_rust_src_lib() {
        // Rust scaffolds keep source under src/. Discovery must
        // recurse to find lib.rs and return the workdir-relative
        // path (not just the bare filename).
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let f = discover_source_file(tmp.path()).expect("should find lib.rs");
        assert_eq!(f, "src/lib.rs");
    }

    #[test]
    fn discover_source_file_skips_tests_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::write(tmp.path().join("tests/test_x.py"), "pass\n").unwrap();
        std::fs::write(tmp.path().join("evaluator.py"), "pass\n").unwrap();
        let f = discover_source_file(tmp.path()).expect("should skip tests/");
        assert_eq!(f, "evaluator.py");
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
        assert_eq!(
            std::fs::read_to_string(dst.join("a.py")).unwrap(),
            "x = 1\n"
        );
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
