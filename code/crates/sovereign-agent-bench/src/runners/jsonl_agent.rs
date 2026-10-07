// SPDX-License-Identifier: AGPL-3.0-or-later
//! Supervision for a coding agent driven as a subprocess that streams JSONL
//! on stdout (pi, opencode).
//!
//! The harness's half of every such run is the same: spawn, read stdout line
//! by line, enforce the output-token budget, the wall cap, the no-progress
//! and write-thrash detectors and the `done` intercept, kill on the first
//! that fires, keep a capped stderr tail. Only reading the agent's events is
//! agent-specific, and that is a [`JsonlDialect`]: one line in, a finished
//! model turn out. Two agents under one supervisor is what makes a pi run and
//! an opencode run comparable: they are killed by the same rules.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::runner::{AgentRunContext, AgentRunError, ExitReason, TokenCounts, ToolCallRecord};
use crate::runners::shared_detectors::{ThrashSignal, ThrashTracker, SAME_PATH_WRITE_THRESHOLD};

/// Stderr tail cap.
pub(crate) const STDERR_TAIL_CAP_BYTES: usize = 32 * 1024;

/// How long to wait for SIGTERM to be honoured before we escalate to
/// SIGKILL (via the platform `kill_on_drop` path).
const SIGTERM_GRACE: Duration = Duration::from_secs(5);

/// How many consecutive tool calls without a workdir state change
/// trigger the no-progress kill. Tuned for the documented failure
/// mode (model loops `read` on an empty directory under
/// `SOVEREIGN_FORCE_TOOL_CALLS=1`). 8 is generous enough to ride
/// through a legitimate "read several files before writing" pattern
/// but cuts off the 48-read loop we observed in run `n`.
pub(crate) const NO_PROGRESS_TOOL_CALLS_THRESHOLD: u32 = 8;

/// Workdir polling interval — every N tool calls observed we
/// recompute the workdir hash. Cheaper than per-call polling.
const NO_PROGRESS_CHECK_EVERY: u32 = 1;

/// What a tool call means to the detectors. Agents name their tools
/// differently (pi `write`, opencode `write` and `edit`); the dialect maps
/// each name onto the role the supervisor keys on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolRole {
    /// Changes a file: counts toward write-thrash on its path.
    Write,
    /// Runs a command: the agent verified, write-thrash resets.
    Verify,
    /// The agent declared itself finished: the run ends cleanly.
    Done,
    /// Neutral to the detectors.
    Other,
}

/// One tool call inside a finished model turn.
#[derive(Debug, Clone)]
pub(crate) struct TurnTool {
    pub role: ToolRole,
    /// The file a `Write` targets, when the call names one.
    pub path: Option<String>,
    /// Telemetry, with the canonical kind the agent's adapter gave it.
    pub record: ToolCallRecord,
}

/// One finished model turn, as the dialect read it off stdout.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentTurn {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tools: Vec<TurnTool>,
    pub text: String,
}

/// An agent's JSONL event schema.
pub(crate) trait JsonlDialect: Send + 'static {
    /// One stdout line in; a turn out when this line finishes one. A
    /// dialect whose agent spreads a turn over several events keeps the
    /// partial turn itself.
    fn feed(&mut self, line: &str) -> Option<AgentTurn>;
}

/// What the supervisor hands back for the runner to wrap in its artifact.
pub(crate) struct Supervised {
    pub exit_reason: ExitReason,
    pub tokens: TokenCounts,
    pub tool_calls: Vec<ToolCallRecord>,
    pub final_text: String,
    pub raw_lines: Vec<String>,
    pub stderr_tail: String,
}

/// Why the budget kill fired (used internally to classify the artifact's
/// `ExitReason`).
#[derive(Debug, Clone)]
enum KillReason {
    Tokens {
        cap: u64,
        observed: u64,
    },
    Wall {
        cap_seconds: u64,
    },
    NoProgress {
        consecutive: u32,
    },
    WriteThrash {
        consecutive_writes: u32,
    },
    /// Model emitted the `done` virtual tool. Pi-agent-core has no
    /// max-iteration heuristic and won't terminate on `done` by
    /// itself (per `invariant_pi_done_heuristic` — it exits only
    /// when the assistant turn contains NO tool calls). So we
    /// intercept here: first `done` ends the run cleanly via
    /// SIGTERM. The witness still scores whatever is in the
    /// workdir, which is the model's last `write`.
    ModelDone,
}

enum SelectOutcome {
    Natural(std::io::Result<std::process::ExitStatus>),
    Kill(KillReason),
    NoKill,
}

/// Spawn `cmd` (args, cwd and env already set) and supervise it to exit.
/// `agent` names the agent in crash messages.
pub(crate) async fn supervise(
    agent: &'static str,
    mut cmd: Command,
    ctx: &AgentRunContext,
    dialect: Box<dyn JsonlDialect>,
) -> Result<Supervised, AgentRunError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => AgentRunError::BinaryNotFound(program),
        _ => AgentRunError::SpawnFailed(e.to_string()),
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentRunError::Internal("child has no stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentRunError::Internal("child has no stderr".into()))?;

    // Channel for the reader task to push parsed turns back.
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel::<AgentTurn>();
    // Channel for raw stdout lines — captured verbatim so the
    // operator can reverse-engineer the agent's event schema
    // when our parser misses something.
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<String>();
    // Oneshot for budget kill.
    let (kill_tx, kill_rx) = oneshot::channel::<KillReason>();

    let token_budget = ctx.token_budget;
    let problem_id = ctx.problem_id.clone();
    let workdir = ctx.workdir.path().to_path_buf();

    // Reader task — dialect + budget watcher + no-progress detector.
    // Owns the kill sender; uses Option::take to fire once.
    let reader = tokio::spawn(async move {
        let mut dialect = dialect;
        let mut kill_tx_opt: Option<oneshot::Sender<KillReason>> = Some(kill_tx);
        let mut lines = BufReader::new(stdout).lines();
        let mut cumulative_out: u64 = 0;
        let mut last_workdir_hash: u64 = hash_workdir(&workdir);
        let mut consecutive_no_progress_calls: u32 = 0;
        let mut calls_since_check: u32 = 0;
        // Same-path consecutive-write counter for the write-thrash
        // detector. Resets on a verify (a command ran) or on a write to
        // a different path (multi-file scaffolding).
        let mut thrash = ThrashTracker::new();
        while let Ok(Some(line)) = lines.next_line().await {
            // Push raw line first so artifact capture wins even
            // if the parser panics on a malformed payload.
            let _ = raw_tx.send(line.clone());
            let Some(turn) = dialect.feed(&line) else {
                continue;
            };
            cumulative_out = cumulative_out.saturating_add(turn.output_tokens);
            let turn_tools = turn.tools.len();
            tracing::debug!(
                agent,
                problem = %problem_id,
                tokens_out = turn.output_tokens,
                cumulative_out,
                turn_tools,
                "agent_bench: assistant turn"
            );
            if cumulative_out > token_budget {
                if let Some(tx) = kill_tx_opt.take() {
                    tracing::warn!(
                        agent,
                        problem = %problem_id,
                        cap = token_budget,
                        observed = cumulative_out,
                        "agent_bench: budget_exceeded"
                    );
                    let _ = tx.send(KillReason::Tokens {
                        cap: token_budget,
                        observed: cumulative_out,
                    });
                }
            }
            // No-progress detector: every tool-bearing turn,
            // recompute the workdir hash. If unchanged, increment
            // a counter; when the counter hits the threshold,
            // SIGTERM. Resets to zero whenever the workdir
            // actually changes.
            if turn_tools > 0 {
                calls_since_check = calls_since_check.saturating_add(turn_tools as u32);
                if calls_since_check >= NO_PROGRESS_CHECK_EVERY {
                    calls_since_check = 0;
                    let current_hash = hash_workdir(&workdir);
                    if current_hash == last_workdir_hash {
                        consecutive_no_progress_calls =
                            consecutive_no_progress_calls.saturating_add(turn_tools as u32);
                        tracing::debug!(
                            agent,
                            problem = %problem_id,
                            consecutive = consecutive_no_progress_calls,
                            threshold = NO_PROGRESS_TOOL_CALLS_THRESHOLD,
                            "agent_bench: no-progress increment"
                        );
                        if consecutive_no_progress_calls >= NO_PROGRESS_TOOL_CALLS_THRESHOLD {
                            if let Some(tx) = kill_tx_opt.take() {
                                tracing::warn!(
                                    agent,
                                    problem = %problem_id,
                                    consecutive = consecutive_no_progress_calls,
                                    threshold = NO_PROGRESS_TOOL_CALLS_THRESHOLD,
                                    "agent_bench: no_progress kill"
                                );
                                let _ = tx.send(KillReason::NoProgress {
                                    consecutive: consecutive_no_progress_calls,
                                });
                            }
                        }
                    } else {
                        tracing::debug!(
                            agent,
                            problem = %problem_id,
                            "agent_bench: workdir changed — resetting no-progress"
                        );
                        consecutive_no_progress_calls = 0;
                        last_workdir_hash = current_hash;
                    }
                }
            }

            // Write-thrash detector. Counts consecutive writes to the
            // *same path* without an interleaving verify. A verify resets
            // the counter; a write to a different path resets and starts
            // tracking the new path (multi-file scaffolding under
            // tier=FromScratch is healthy, not thrash). Other tools are
            // neutral. When the counter crosses threshold, SIGTERM with a
            // distinct exit reason so the operator can tell write-thrash
            // from token cap or no-progress kills.
            for tool in &turn.tools {
                match tool.role {
                    ToolRole::Write => {
                        let signal = thrash.observe_write(tool.path.as_deref());
                        tracing::debug!(
                            agent,
                            problem = %problem_id,
                            same_path_writes = thrash.same_path_writes(),
                            threshold = SAME_PATH_WRITE_THRESHOLD,
                            path = ?thrash.last_write_path(),
                            "agent_bench: write-thrash increment"
                        );
                        if let ThrashSignal::Kill { same_path_writes } = signal {
                            if let Some(tx) = kill_tx_opt.take() {
                                tracing::warn!(
                                    agent,
                                    problem = %problem_id,
                                    same_path_writes,
                                    threshold = SAME_PATH_WRITE_THRESHOLD,
                                    path = ?thrash.last_write_path(),
                                    "agent_bench: write_thrash kill"
                                );
                                let _ = tx.send(KillReason::WriteThrash {
                                    consecutive_writes: same_path_writes,
                                });
                            }
                            break;
                        }
                    }
                    ToolRole::Verify => {
                        if thrash.same_path_writes() > 0 {
                            tracing::debug!(
                                agent,
                                problem = %problem_id,
                                "agent_bench: write-thrash reset (verify observed)"
                            );
                        }
                        thrash.observe_verify();
                    }
                    ToolRole::Done => {
                        if let Some(tx) = kill_tx_opt.take() {
                            tracing::info!(
                                agent,
                                problem = %problem_id,
                                "agent_bench: model emitted `done` — terminating run"
                            );
                            let _ = tx.send(KillReason::ModelDone);
                        }
                        break;
                    }
                    ToolRole::Other => {}
                }
            }
            let _ = turn_tx.send(turn);
        }
    });

    // Stderr drain — capped tail.
    let stderr_drain = tokio::spawn(async move {
        let mut tail = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > STDERR_TAIL_CAP_BYTES {
                let cut = tail.len() - STDERR_TAIL_CAP_BYTES;
                tail.drain(..cut);
            }
        }
        tail
    });

    // Wall-clock cap. We construct `child.wait()` inside the
    // select so the future is dropped after select returns,
    // releasing the &mut borrow on `child`. The Kill and NoKill
    // branches then call `child.wait()` again with a fresh future.
    let wall_cap = Duration::from_secs(ctx.wall_seconds_cap);
    let outcome = tokio::select! {
        // Bias: natural exit wins ties. Without this, the
        // reader-task dropping its kill_tx on stdout close races
        // wait_future and fires the kill_rx Err arm first → a
        // false-positive Crashed classification.
        biased;
        status = child.wait() => SelectOutcome::Natural(status),
        _ = tokio::time::sleep(wall_cap) => SelectOutcome::Kill(KillReason::Wall {
            cap_seconds: ctx.wall_seconds_cap,
        }),
        kr = kill_rx => match kr {
            Ok(reason) => SelectOutcome::Kill(reason),
            Err(_) => SelectOutcome::NoKill,
        },
    };

    let exit_reason = match outcome {
        SelectOutcome::Natural(status) => classify_status(agent, status),
        SelectOutcome::Kill(reason) => {
            let _ = child.start_kill();
            let _ = timeout(SIGTERM_GRACE, child.wait()).await;
            match reason {
                KillReason::Tokens { cap, observed } => {
                    ExitReason::TokensExceeded { cap, observed }
                }
                KillReason::Wall { cap_seconds } => ExitReason::Timeout { cap_seconds },
                KillReason::NoProgress { consecutive } => ExitReason::NoProgress {
                    consecutive_tool_calls: consecutive,
                    threshold: NO_PROGRESS_TOOL_CALLS_THRESHOLD,
                },
                KillReason::WriteThrash { consecutive_writes } => ExitReason::WriteThrash {
                    consecutive_writes,
                    threshold: SAME_PATH_WRITE_THRESHOLD,
                },
                KillReason::ModelDone => ExitReason::Completed,
            }
        }
        SelectOutcome::NoKill => {
            // Reader closed without sending a kill. Wait for the
            // natural exit and classify it.
            classify_status(agent, child.wait().await)
        }
    };

    // Wait for the reader to finish draining stdout (the child
    // process is already gone by this point; the close should
    // arrive promptly).
    let _ = reader.await;
    let stderr_tail = stderr_drain.await.unwrap_or_default();

    let mut raw_lines: Vec<String> = Vec::new();
    while let Ok(line) = raw_rx.try_recv() {
        raw_lines.push(line);
    }

    // Drain turns into totals, numbered tool calls and the final text.
    let mut tokens = TokenCounts::default();
    let mut tool_calls: Vec<ToolCallRecord> = Vec::new();
    let mut final_text = String::new();
    let mut turn_no: u32 = 0;
    while let Ok(turn) = turn_rx.try_recv() {
        tokens.input = tokens.input.saturating_add(turn.input_tokens);
        tokens.output = tokens.output.saturating_add(turn.output_tokens);
        turn_no = turn_no.saturating_add(1);
        for tool in turn.tools {
            let mut rec = tool.record;
            rec.turn = turn_no;
            tool_calls.push(rec);
        }
        if !turn.text.is_empty() {
            final_text = turn.text;
        }
    }

    // Stamp stderr into Crashed reason when applicable.
    let exit_reason = match exit_reason {
        ExitReason::Crashed { stderr_tail: prior } if prior.is_empty() => ExitReason::Crashed {
            stderr_tail: cap_tail(&stderr_tail),
        },
        other => other,
    };

    Ok(Supervised {
        exit_reason,
        tokens,
        tool_calls,
        final_text,
        raw_lines,
        stderr_tail: cap_tail(&stderr_tail),
    })
}

fn classify_status(agent: &str, status: std::io::Result<std::process::ExitStatus>) -> ExitReason {
    match status {
        Ok(s) if s.success() => ExitReason::Completed,
        Ok(s) => ExitReason::Crashed {
            stderr_tail: format!("{agent} exited with status {s}"),
        },
        Err(e) => ExitReason::Crashed {
            stderr_tail: format!("wait err: {e}"),
        },
    }
}

/// A tool record for telemetry, before the supervisor numbers its turn.
pub(crate) fn tool_record(
    name: &str,
    input: &Value,
    canonical_kind: Option<sovereign_agent_tools::PrimitiveKind>,
) -> ToolCallRecord {
    ToolCallRecord {
        turn: 0,
        tool: name.to_string(),
        args_preview: serde_json::to_string(input)
            .unwrap_or_default()
            .chars()
            .take(256)
            .collect::<String>(),
        ok: true,
        canonical_kind,
    }
}

/// Render the workdir as a short tree the model can read. Empty dirs
/// surface as "(empty)" so the agent knows it must write before it
/// can read anything useful.
pub(crate) fn describe_workdir(root: &std::path::Path) -> String {
    let mut entries: Vec<String> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let it = match std::fs::read_dir(&dir) {
            Ok(it) => it,
            Err(_) => continue,
        };
        let mut local: Vec<_> = it.flatten().collect();
        local.sort_by_key(|e| e.file_name());
        for entry in local {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if matches!(name, "target" | "node_modules" | ".git" | "__pycache__") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                entries.push(format!("  {rel}/"));
                stack.push(path);
            } else if meta.is_file() {
                entries.push(format!("  {rel}  ({} bytes)", meta.len()));
            }
        }
    }
    if entries.is_empty() {
        "(empty — the workdir contains no files. You must create Cargo.toml and src/lib.rs via the `write` tool before any `read`/`bash` call will find them.)".into()
    } else {
        entries.join("\n")
    }
}

/// Quick rolling hash of every regular file under `dir`. Used by the
/// no-progress detector to tell "workdir state changed since last
/// tool call" from "model is looping with nothing to read." Walks the
/// tree depth-first, hashes `(relative_path, size, mtime, first
/// 4 KiB of contents)`. Robust to permission errors (skipped silently)
/// and to symlinks (treated as their immediate target if it resolves).
fn hash_workdir(root: &std::path::Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let it = match std::fs::read_dir(&dir) {
            Ok(it) => it,
            Err(_) => continue,
        };
        let mut entries: Vec<_> = it.flatten().collect();
        // Sort for deterministic order — without this two equivalent
        // workdirs can hash differently across runs.
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // Skip noise that ate cycles in earlier runs.
            if matches!(name, "target" | "node_modules" | ".git" | "__pycache__") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if meta.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                rel.hash(&mut h);
                meta.len().hash(&mut h);
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        dur.as_secs().hash(&mut h);
                        dur.subsec_nanos().hash(&mut h);
                    }
                }
                // Sample first 4 KiB so a same-size in-place edit is
                // detected. Full file hash would be exact but the
                // detector runs per tool call — cheap is the right
                // tradeoff.
                if let Ok(prefix) = read_prefix(&path, 4096) {
                    prefix.hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

fn read_prefix(path: &std::path::Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; limit];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

pub(crate) fn cap_tail(s: &str) -> String {
    if s.len() <= STDERR_TAIL_CAP_BYTES {
        s.to_string()
    } else {
        let cut = s.len() - STDERR_TAIL_CAP_BYTES;
        format!("... (truncated {cut} leading bytes) ...\n{}", &s[cut..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_tail_short_string_passes_through() {
        let s = "hello";
        assert_eq!(cap_tail(s), "hello");
    }

    #[test]
    fn cap_tail_long_string_truncates_prefix() {
        let s = "x".repeat(STDERR_TAIL_CAP_BYTES + 64);
        let cut = cap_tail(&s);
        assert!(cut.starts_with("... (truncated"));
        assert!(cut.len() < s.len());
    }
}
