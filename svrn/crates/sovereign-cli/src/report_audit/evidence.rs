// SPDX-License-Identifier: AGPL-3.0-or-later
//! The evidence universe for a report audit: what this session actually did.
//!
//! The grounding gate asks "is this claim supported by the retrieved
//! corpus". This module answers the same question for a different corpus —
//! the session's own tool calls. A claim in a report to the operator is
//! grounded when something the agent RAN says so, and ungrounded when
//! nothing in the transcript touches it.
//!
//! # Why not `session_cmd::extract_spine`
//!
//! [`crate::session_cmd::Spine`] walks the same JSONL and keeps COUNTS:
//! `tool_calls: name → count`, `edits: path → count`. It drops every tool
//! input except `Edit.file_path`, and drops tool RESULTS entirely — correct
//! for distillation, which hands a model a narrative, and useless here,
//! where the result text IS the evidence. Two readers of one file format,
//! because they answer two different questions (ARCH §10.6 is one decider
//! per DECISION, not one parser per format).
//!
//! # Custody
//!
//! Every chunk is `custody_known: true`. The transcript is this session's
//! own record of its own actions — the one corpus whose provenance is not
//! in question. The field is carried anyway because [`AuditChunk`] is the
//! shape `deep_research::audit::assess_claim` consumes, so the model pass
//! can read this pool without a second type.

use sovereign_core::deep_research::audit::AuditChunk;
use std::collections::BTreeSet;
use std::path::Path;

/// Per-chunk content cap. Only the model pass reads `AuditChunk.content`;
/// the mechanical checks probe [`Evidence::corpus`], which is uncapped.
const CHUNK_CONTENT_CAP: usize = 4_000;

/// Strip heredoc BODIES from a shell command, leaving the invocations.
///
/// # The defect this exists for (found 2026-09-12, by the tool on itself)
///
/// `Command::line` is the whole Bash string, and in this harness that
/// routinely includes a heredoc carrying a file's entire contents. The gate
/// check asks "did a test command run in this session" by looking for
/// `sovereign-test.sh` in the command text — and a `cat > x.rs <<'RS'`
/// whose body merely MENTIONS `sovereign-test.sh` answered yes. Writing a
/// file that names a gate counted as running it, which grounded "all tests
/// pass" on a `mkdir`.
///
/// Bodies are dropped, invocations kept: the wrapper form this repo uses
/// (`with-cargo-lock.sh ./scripts/sovereign-test.sh`) puts the real command
/// in argument position, so command-position matching would reject the
/// legitimate case while this does not.
#[must_use]
pub fn strip_heredoc_bodies(cmd: &str) -> String {
    let mut out = String::new();
    let mut terminator: Option<String> = None;
    for line in cmd.lines() {
        if let Some(tag) = &terminator {
            if line.trim_end() == tag.as_str() {
                terminator = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(pos) = line.find("<<") {
            let rest = &line[pos + 2..];
            let rest = rest.strip_prefix('-').unwrap_or(rest);
            let tag: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let quoted: String = rest
                .trim_start()
                .trim_start_matches(['\'', '"'])
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let tag = if tag.is_empty() { quoted } else { tag };
            if !tag.is_empty() {
                terminator = Some(tag);
            }
        }
    }
    out
}

/// One shell command the session ran, with what it returned.
#[derive(Debug, Clone)]
pub struct Command {
    /// The command as issued, heredoc bodies and all.
    pub line: String,
    /// The command with heredoc bodies removed — what to match when the
    /// question is "was this program RUN", not "was this text present".
    pub invocation: String,
    /// The harness's own `is_error` on the tool result. Not an exit code —
    /// the transcript does not carry one — but it is the harness's verdict
    /// on whether the call failed, and it is never absent.
    pub is_error: bool,
    pub stdout: String,
    pub stderr: String,
}

/// What the session can be shown to have done.
#[derive(Debug, Default, Clone)]
pub struct Evidence {
    /// One per tool call, in order. The shape the model pass consumes.
    pub chunks: Vec<AuditChunk>,
    /// Shell commands, in order.
    pub commands: Vec<Command>,
    /// Paths named in a file-tool input (`Read`, `Edit`, `Write`, …).
    pub files_touched: BTreeSet<String>,
    /// Every tool call's command line and output, concatenated. The
    /// mechanical checks probe this; it is deliberately uncapped, because a
    /// truncated corpus turns "absent from evidence" into a false finding
    /// and a gate with false positives teaches people to bypass it.
    pub corpus: String,
    /// The operator's own turns. A figure the operator supplied is not the
    /// agent's to ground.
    pub operator_text: String,
    /// Assistant text blocks, in order. The last one is the report.
    pub assistant_texts: Vec<String>,
}

impl Evidence {
    /// Did any tool call mention this literal? Case-sensitive: paths and
    /// symbols are, and lowercasing would match `Error` against `error`.
    #[must_use]
    pub fn mentions(&self, needle: &str) -> bool {
        !needle.is_empty() && self.corpus.contains(needle)
    }

    /// Did the OPERATOR supply this literal? Their figures and paths are
    /// given, not claimed.
    #[must_use]
    pub fn operator_supplied(&self, needle: &str) -> bool {
        !needle.is_empty() && self.operator_text.contains(needle)
    }

    /// The commands whose INVOCATION matches any of `patterns`.
    ///
    /// Invocation, never `line`: see [`strip_heredoc_bodies`].
    #[must_use]
    pub fn commands_matching(&self, patterns: &[&str]) -> Vec<&Command> {
        self.commands
            .iter()
            .filter(|c| patterns.iter().any(|p| c.invocation.contains(p)))
            .collect()
    }
}

fn truncate_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    s.chars().take(cap).collect::<String>() + "\n[…truncated]"
}

/// Pull a tool result's text out of the two places the harness puts it: the
/// `tool_result` block's `content` (string or array-of-blocks) and the
/// record-level `toolUseResult` object (`stdout` / `stderr`).
fn result_text(block: &serde_json::Value) -> String {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// File-tool inputs that name a path.
fn input_path(name: &str, input: &serde_json::Value) -> Option<String> {
    let key = match name {
        "Read" | "Edit" | "Write" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

/// Parse one harness transcript into its evidence pool.
///
/// Returns an empty pool rather than an error for a transcript with no
/// assistant activity: "nothing to check" and "could not read" are
/// different answers and the caller distinguishes them by `Err`.
pub fn extract(path: &Path) -> Result<Evidence, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read transcript {}: {e}", path.display()))?;
    Ok(from_jsonl(&text, &session_id_of(path)))
}

fn session_id_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// The parse itself, over transcript text. Separated from [`extract`] so
/// the fixture bank drives it without touching a filesystem.
#[must_use]
pub fn from_jsonl(text: &str, session_id: &str) -> Evidence {
    let mut ev = Evidence::default();
    // tool_use_id → (tool name, command line) for the Bash calls still
    // awaiting their result. The result arrives in a LATER record.
    let mut pending: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut corpus = String::new();

    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = rec.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let content = rec.get("message").and_then(|m| m.get("content"));
        let Some(serde_json::Value::Array(blocks)) = content else {
            continue;
        };

        match kind {
            "assistant" => {
                for b in blocks {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    ev.assistant_texts.push(t.trim().to_string());
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let id = b.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            let input = b.get("input").cloned().unwrap_or(serde_json::Value::Null);
                            if let Some(p) = input_path(name, &input) {
                                ev.files_touched.insert(p);
                            }
                            let line = if name == "Bash" {
                                input
                                    .get("command")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string()
                            } else {
                                // Non-Bash calls carry their input verbatim so
                                // a path or pattern they named is in the corpus.
                                serde_json::to_string(&input).unwrap_or_default()
                            };
                            corpus.push_str(&line);
                            corpus.push('\n');
                            pending.insert(id.to_string(), (name.to_string(), line));
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                // A user record is either operator prose or tool results —
                // never both.
                let is_results = blocks
                    .iter()
                    .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
                if !is_results {
                    let joined = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let stripped = crate::session_cmd::strip_reminders(&joined);
                    if crate::session_cmd::keep_user_turn(&stripped) {
                        ev.operator_text.push_str(&stripped);
                        ev.operator_text.push('\n');
                    }
                    continue;
                }
                let tur = rec.get("toolUseResult");
                let stdout = tur
                    .and_then(|t| t.get("stdout"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let stderr = tur
                    .and_then(|t| t.get("stderr"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                for b in blocks {
                    if b.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let id = b.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
                    let is_error = b
                        .get("is_error")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let (name, cmdline) = pending
                        .remove(id)
                        .unwrap_or_else(|| ("?".to_string(), String::new()));
                    let body = result_text(b);
                    let out = if stdout.is_empty() {
                        body.clone()
                    } else {
                        stdout.clone()
                    };

                    corpus.push_str(&out);
                    corpus.push('\n');
                    corpus.push_str(&stderr);
                    corpus.push('\n');

                    if name == "Bash" {
                        ev.commands.push(Command {
                            invocation: strip_heredoc_bodies(&cmdline),
                            line: cmdline.clone(),
                            is_error,
                            stdout: out.clone(),
                            stderr: stderr.clone(),
                        });
                    }
                    let idx = ev.chunks.len();
                    ev.chunks.push(AuditChunk {
                        id: format!("tool-{idx}-{name}"),
                        content: truncate_chars(
                            &format!("$ {cmdline}\n{out}\n{stderr}"),
                            CHUNK_CONTENT_CAP,
                        ),
                        custody_known: true,
                        source_url: format!("session://{session_id}#tool-{idx}"),
                    });
                }
            }
            _ => {}
        }
    }

    // A Bash call whose result never arrived (the session ended mid-call)
    // is still a command the agent issued — but with NO result, so it can
    // never support a claim about what it returned. Recorded as errored.
    for (_, (name, line)) in pending {
        if name == "Bash" {
            ev.commands.push(Command {
                invocation: strip_heredoc_bodies(&line),
                line,
                is_error: true,
                stdout: String::new(),
                stderr: "[no result recorded]".to_string(),
            });
        }
    }

    ev.corpus = corpus;
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"fix the parser"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test -p foo"}}]}}"#,
        "\n",
        r#"{"type":"user","toolUseResult":{"stdout":"test result: ok. 12 passed","stderr":""},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"test result: ok. 12 passed"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/repo/src/parse.rs"}}]}}"#,
        "\n",
        r#"{"type":"user","toolUseResult":{"stdout":"","stderr":""},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","is_error":false,"content":"ok"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done: 12 tests pass."}]}}"#,
    );

    #[test]
    fn pairs_commands_with_the_results_that_arrive_later() {
        let ev = from_jsonl(T, "sess");
        assert_eq!(ev.commands.len(), 1, "one Bash call");
        assert_eq!(ev.commands[0].line, "cargo test -p foo");
        assert!(ev.commands[0].stdout.contains("12 passed"));
        assert!(!ev.commands[0].is_error);
    }

    #[test]
    fn file_tool_inputs_land_in_files_touched() {
        let ev = from_jsonl(T, "sess");
        assert!(ev.files_touched.contains("/repo/src/parse.rs"));
    }

    #[test]
    fn corpus_holds_both_the_command_and_its_output() {
        let ev = from_jsonl(T, "sess");
        assert!(ev.mentions("cargo test -p foo"));
        assert!(ev.mentions("12 passed"));
        assert!(!ev.mentions("cargo clippy"));
    }

    #[test]
    fn operator_turns_are_separated_from_tool_results() {
        let ev = from_jsonl(T, "sess");
        assert!(ev.operator_text.contains("fix the parser"));
        assert!(!ev.operator_text.contains("12 passed"));
    }

    #[test]
    fn last_assistant_text_is_the_report() {
        let ev = from_jsonl(T, "sess");
        assert_eq!(ev.assistant_texts.last().unwrap(), "Done: 12 tests pass.");
    }

    #[test]
    fn a_call_with_no_result_is_errored_never_a_clean_run() {
        let one = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"x","name":"Bash","input":{"command":"./scripts/sovereign-test.sh"}}]}}"#;
        let ev = from_jsonl(one, "sess");
        assert_eq!(ev.commands.len(), 1);
        assert!(
            ev.commands[0].is_error,
            "a command whose result never arrived cannot support a claim about what it returned"
        );
    }

    #[test]
    fn writing_a_file_that_names_a_gate_is_not_running_that_gate() {
        let cmd = "cat > notes.rs <<'RS'\n// run ./scripts/sovereign-test.sh first\nRS\necho done";
        let stripped = strip_heredoc_bodies(cmd);
        assert!(!stripped.contains("sovereign-test.sh"), "body dropped");
        assert!(
            stripped.contains("echo done"),
            "commands after the body kept"
        );
    }

    #[test]
    fn a_gate_invoked_through_a_wrapper_survives_stripping() {
        let cmd = "./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human";
        assert!(strip_heredoc_bodies(cmd).contains("sovereign-test.sh"));
    }

    #[test]
    fn commands_matching_reads_the_invocation_not_the_body() {
        // NOTE: the JSON must stay on ONE line with escaped newlines —
        // `from_jsonl` iterates `lines()`, so a fixture broken across
        // physical lines parses as nothing and the test passes for the
        // wrong reason. It did exactly that on 2026-09-12.
        let t = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"h","name":"Bash","input":{"command":"cat > a.rs <<'RS'\ncargo test is mentioned here\nRS"}}]}}"#,
            "\n",
            r#"{"type":"user","toolUseResult":{"stdout":"","stderr":""},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"h","is_error":false,"content":"ok"}]}}"#,
        );
        let ev = from_jsonl(t, "sess");
        assert_eq!(ev.commands.len(), 1, "the fixture must actually parse");
        assert!(
            ev.commands_matching(&["cargo test"]).is_empty(),
            "a heredoc that mentions a gate did not run it"
        );
        assert!(
            ev.mentions("cargo test"),
            "but the text is still in the corpus"
        );
    }

    #[test]
    fn chunks_carry_known_custody_and_a_session_address() {
        let ev = from_jsonl(T, "sess");
        assert!(ev.chunks.iter().all(|c| c.custody_known));
        assert!(ev.chunks[0].source_url.starts_with("session://sess#tool-"));
    }
}
