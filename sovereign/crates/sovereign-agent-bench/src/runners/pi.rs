//! `PiRunner` — drives the `pi` coding agent against the local daemon.
//!
//! Surface (see `pi --help`):
//!   pi --provider commonwealth --model commonwealth/coder \
//!      --no-session -p --mode json \
//!      --offline --no-context-files \
//!      --tools read,edit,write,bash,find,grep,ls \
//!      "<prompt>"
//!
//! Pi streams JSONL events on stdout. We parse a small subset:
//!   - `message_end` carrying `usage.input_tokens` / `usage.output_tokens`
//!   - `tool_execution_start` / `tool_execution_end`
//!   - `turn_end`
//!
//! Other event types are skipped at `tracing::debug` (lenient: pi's
//! schema may grow; we don't want a new event type to crash the harness).
//!
//! Budget enforcement: cumulative `usage.output_tokens` past
//! `token_budget` → SIGTERM. Wall-clock cap fires independently.
//!
//! Env scrub: only `PATH`, `HOME`, `PI_PROVIDER_URL`, `LANG`.
//! No model credentials reach the child.

use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ExitReason, TokenCounts,
    ToolCallRecord,
};
use crate::sandbox::Sandbox;

/// Allowlisted pi tools. Structural invariant per ARCH §7.2 — not
/// configurable from TOML.
/// Pi tools exposed to the agent. `edit` is intentionally absent —
/// pi's edit requires `oldText` to match the file byte-for-byte
/// (whitespace + line endings included), which models struggle to
/// reproduce after a `read`. Forcing `write` (full-file replacement)
/// removes that brittleness. See `bench/agent-coding/problems/*/prompt.md`
/// — each prompt now nudges the model toward `write` + `bash` verify.
/// Tool names this runner passes to pi via `--tools`. Authoritative
/// source is `commonwealth_agent_tools::adapter::pi::Adapter::
/// pi_tool_allowlist()`; the equivalence is pinned by
/// `tool_allowlist_matches_canonical_adapter` below so a future PR
/// that adds a primitive to the canonical layer can't forget to
/// expose it on the pi runner.
pub const PI_TOOL_ALLOWLIST: &[&str] = &["read", "write", "bash", "find", "grep", "ls"];

/// Default daemon endpoint. The setup script writes a matching provider
/// in `~/.pi/agent/models.json`.
pub const DEFAULT_PI_PROVIDER_URL: &str = "http://localhost:9741/v1";

/// Stderr tail cap.
const STDERR_TAIL_CAP_BYTES: usize = 32 * 1024;

/// How long to wait for SIGTERM to be honoured before we escalate to
/// SIGKILL (via the platform `kill_on_drop` path).
const SIGTERM_GRACE: Duration = Duration::from_secs(5);

/// How many consecutive tool calls without a workdir state change
/// trigger the no-progress kill. Tuned for the documented failure
/// mode (model loops `read` on an empty directory under
/// `SOVEREIGN_FORCE_TOOL_CALLS=1`). 8 is generous enough to ride
/// through a legitimate "read several files before writing" pattern
/// but cuts off the 48-read loop we observed in run `n`.
const NO_PROGRESS_TOOL_CALLS_THRESHOLD: u32 = 8;

/// Workdir polling interval — every N tool calls observed we
/// recompute the workdir hash. Cheaper than per-call polling.
const NO_PROGRESS_CHECK_EVERY: u32 = 1;

// SAME_PATH_WRITE_THRESHOLD + ThrashTracker live in
// `runners::shared_detectors` so the native runner can reuse them
// (ARCH §10.3). Re-imported below.

use crate::runners::shared_detectors::{
    ThrashSignal, ThrashTracker, SAME_PATH_WRITE_THRESHOLD,
};

/// Why the budget kill fired (used internally to classify the artifact's
/// `ExitReason`).
#[derive(Debug, Clone)]
enum KillReason {
    Tokens { cap: u64, observed: u64 },
    Wall { cap_seconds: u64 },
    NoProgress { consecutive: u32 },
    WriteThrash { consecutive_writes: u32 },
    /// Model emitted the `done` virtual tool. Pi-agent-core has no
    /// max-iteration heuristic and won't terminate on `done` by
    /// itself (per `invariant_pi_done_heuristic` — it exits only
    /// when the assistant turn contains NO tool calls). So we
    /// intercept here: first `done` ends the run cleanly via
    /// SIGTERM. The witness still scores whatever is in the
    /// workdir, which is the model's last `write`.
    ModelDone,
}

pub struct PiRunner {
    /// Path to the `pi` binary. `None` means search PATH.
    binary: Option<String>,
    provider_url: String,
}

impl PiRunner {
    pub fn new() -> Self {
        Self {
            binary: None,
            provider_url: DEFAULT_PI_PROVIDER_URL.to_string(),
        }
    }

    pub fn with_binary(mut self, path: impl Into<String>) -> Self {
        self.binary = Some(path.into());
        self
    }

    pub fn with_provider_url(mut self, url: impl Into<String>) -> Self {
        self.provider_url = url.into();
        self
    }

    fn binary_path(&self) -> String {
        self.binary.clone().unwrap_or_else(|| "pi".to_string())
    }
}

impl Default for PiRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for PiRunner {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/coder")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        let start = Instant::now();

        let env = Sandbox::scrubbed_env(&[("PI_PROVIDER_URL", self.provider_url.as_str())]);
        let tools_arg = ctx.tool_allowlist.join(",");

        // Prefix the prompt with the actual workdir state. Without
        // this, agents reach for `read` to inspect the empty dir and
        // loop until the no-progress detector fires. Closes the
        // observed "read-only loop on empty workdir" failure mode
        // surfaced by run `p`.
        let workdir_state = describe_workdir(ctx.workdir.path());
        let final_prompt = format!(
            "## Workdir state (factual, current state of `.`)\n{workdir_state}\n\n---\n\n{}",
            ctx.prompt,
        );

        tracing::info!(
            problem = %ctx.problem_id,
            model = %ctx.model_handle,
            budget = ctx.token_budget,
            wall_cap = ctx.wall_seconds_cap,
            "agent_bench: pi.run starting"
        );

        let mut cmd = Command::new(self.binary_path());
        cmd.arg("--provider")
            .arg("commonwealth")
            .arg("--model")
            .arg(&ctx.model_handle)
            .arg("--no-session")
            .arg("--no-context-files")
            .arg("--offline")
            .arg("--print")
            .arg("--mode")
            .arg("json")
            .arg("--tools")
            .arg(&tools_arg)
            .arg(&final_prompt)
            .current_dir(ctx.workdir())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    AgentRunError::BinaryNotFound(self.binary_path())
                }
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

        // Channel for the reader task to push parsed events back.
        let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<ParsedEvent>();
        // Channel for raw stdout lines — captured verbatim so the
        // operator can reverse-engineer the agent's event schema
        // when our parser misses something.
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<String>();
        // Oneshot for budget kill.
        let (kill_tx, kill_rx) = oneshot::channel::<KillReason>();

        let token_budget = ctx.token_budget;
        let problem_id = ctx.problem_id.clone();
        let workdir_path_for_reader = ctx.workdir.path().to_path_buf();
        let pi_build_cmd = ctx.build_cmd.clone();
        let pi_verify_cmd = ctx.verify_cmd.clone();

        // Reader task — JSONL parser + budget watcher + no-progress
        // detector. Owns the kill sender; uses Option::take to fire
        // once.
        let reader = tokio::spawn(async move {
            let mut kill_tx_opt: Option<oneshot::Sender<KillReason>> = Some(kill_tx);
            let mut lines = BufReader::new(stdout).lines();
            let mut cumulative_out: u64 = 0;
            let mut last_workdir_hash: u64 = hash_workdir(&workdir_path_for_reader);
            let mut consecutive_no_progress_calls: u32 = 0;
            let mut calls_since_check: u32 = 0;
            // Same-path consecutive-write counter for the write-thrash
            // detector. Resets on `bash` (verification happened) or on
            // a write to a different path (multi-file scaffolding).
            let mut thrash = ThrashTracker::new();
            while let Ok(Some(line)) = lines.next_line().await {
                // Push raw line first so artifact capture wins even
                // if the parser panics on a malformed payload.
                let _ = raw_tx.send(line.clone());
                let parsed = parse_pi_line(&line);
                if let ParsedEvent::AssistantTurn {
                    output_tokens,
                    content,
                    ..
                } = &parsed
                {
                    cumulative_out = cumulative_out.saturating_add(*output_tokens);
                    let turn_tools = count_tool_blocks(content);
                    tracing::debug!(
                        problem = %problem_id,
                        tokens_out = output_tokens,
                        cumulative_out,
                        turn_tools,
                        "agent_bench: assistant turn"
                    );
                    if cumulative_out > token_budget {
                        if let Some(tx) = kill_tx_opt.take() {
                            tracing::warn!(
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
                        calls_since_check =
                            calls_since_check.saturating_add(turn_tools as u32);
                        if calls_since_check >= NO_PROGRESS_CHECK_EVERY {
                            calls_since_check = 0;
                            let current_hash = hash_workdir(&workdir_path_for_reader);
                            if current_hash == last_workdir_hash {
                                consecutive_no_progress_calls = consecutive_no_progress_calls
                                    .saturating_add(turn_tools as u32);
                                tracing::debug!(
                                    problem = %problem_id,
                                    consecutive = consecutive_no_progress_calls,
                                    threshold = NO_PROGRESS_TOOL_CALLS_THRESHOLD,
                                    "agent_bench: no-progress increment"
                                );
                                if consecutive_no_progress_calls
                                    >= NO_PROGRESS_TOOL_CALLS_THRESHOLD
                                {
                                    if let Some(tx) = kill_tx_opt.take() {
                                        tracing::warn!(
                                            problem = %problem_id,
                                            consecutive =
                                                consecutive_no_progress_calls,
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
                                    problem = %problem_id,
                                    "agent_bench: workdir changed — resetting no-progress"
                                );
                                consecutive_no_progress_calls = 0;
                                last_workdir_hash = current_hash;
                            }
                        }
                    }

                    // Write-thrash detector. Streaming inspection of
                    // tool-events in this turn. Counts consecutive
                    // writes to the *same path* without an
                    // interleaving `bash`. `bash` resets the counter
                    // (verification happened). A write to a different
                    // path resets and starts tracking the new path
                    // (multi-file scaffolding under tier=FromScratch
                    // is healthy, not thrash). Other tools are
                    // neutral. When the counter crosses threshold,
                    // SIGTERM with a distinct exit reason so the
                    // operator can tell write-thrash from token cap
                    // or no-progress kills.
                    for (name, path) in tools_in_turn(content) {
                        match name.as_str() {
                            "write" => {
                                let signal = thrash.observe_write(path.as_deref());
                                tracing::debug!(
                                    problem = %problem_id,
                                    same_path_writes = thrash.same_path_writes(),
                                    threshold = SAME_PATH_WRITE_THRESHOLD,
                                    path = ?thrash.last_write_path(),
                                    "agent_bench: write-thrash increment"
                                );
                                if let ThrashSignal::Kill { same_path_writes } = signal {
                                    if let Some(tx) = kill_tx_opt.take() {
                                        tracing::warn!(
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
                            "bash" => {
                                if thrash.same_path_writes() > 0 {
                                    tracing::debug!(
                                        problem = %problem_id,
                                        "agent_bench: write-thrash reset (bash observed)"
                                    );
                                }
                                thrash.observe_verify();
                            }
                            "done" => {
                                if let Some(tx) = kill_tx_opt.take() {
                                    tracing::info!(
                                        problem = %problem_id,
                                        "agent_bench: model emitted `done` — terminating run"
                                    );
                                    let _ = tx.send(KillReason::ModelDone);
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                let _ = evt_tx.send(parsed);
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
            SelectOutcome::Natural(status) => classify_status(status),
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
                    KillReason::WriteThrash { consecutive_writes } => {
                        ExitReason::WriteThrash {
                            consecutive_writes,
                            threshold: SAME_PATH_WRITE_THRESHOLD,
                        }
                    }
                    KillReason::ModelDone => ExitReason::Completed,
                }
            }
            SelectOutcome::NoKill => {
                // Reader closed without sending a kill. Wait for the
                // natural exit and classify it.
                classify_status(child.wait().await)
            }
        };

        // Wait for the reader to finish draining stdout (the child
        // process is already gone by this point; the close should
        // arrive promptly).
        let _ = reader.await;
        let stderr_tail = stderr_drain.await.unwrap_or_default();

        // Drain raw stdout lines into a Vec.
        let mut raw_lines: Vec<String> = Vec::new();
        while let Ok(line) = raw_rx.try_recv() {
            raw_lines.push(line);
        }

        // Drain events. Walk the per-turn assistant `content[]` to
        // extract tool calls + the model's final text.
        let mut tokens = TokenCounts::default();
        let mut tool_calls: Vec<ToolCallRecord> = Vec::new();
        let mut final_text = String::new();
        let mut turn: u32 = 0;
        while let Ok(evt) = evt_rx.try_recv() {
            match evt {
                ParsedEvent::AssistantTurn {
                    input_tokens,
                    output_tokens,
                    content,
                } => {
                    tokens.input = tokens.input.saturating_add(input_tokens);
                    tokens.output = tokens.output.saturating_add(output_tokens);
                    turn = turn.saturating_add(1);
                    let (turn_tools, turn_text) =
                        harvest_assistant_blocks(&content, &pi_build_cmd, &pi_verify_cmd);
                    for mut rec in turn_tools {
                        rec.turn = turn;
                        tool_calls.push(rec);
                    }
                    if !turn_text.is_empty() {
                        final_text = turn_text;
                    }
                }
                ParsedEvent::Unknown => {}
            }
        }

        // Stamp stderr into Crashed reason when applicable.
        let exit_reason = match exit_reason {
            ExitReason::Crashed { stderr_tail: prior } if prior.is_empty() => {
                ExitReason::Crashed {
                    stderr_tail: cap_tail(&stderr_tail),
                }
            }
            other => other,
        };

        let wall_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            problem = %ctx.problem_id,
            tokens_in = tokens.input,
            tokens_out = tokens.output,
            wall_ms,
            exit = exit_reason.id(),
            "agent_bench: pi.run complete"
        );

        Ok(AgentRunArtifact {
            workdir: ctx.workdir,
            tokens,
            wall_ms,
            exit_reason,
            tool_calls,
            stderr_tail: cap_tail(&stderr_tail),
            final_assistant_text: final_text,
            raw_stdout_lines: raw_lines,
            // Pi runner is subprocess-driven — request capture would
            // require parsing pi's internal HTTP traffic. Out of
            // scope; replay supports the native runner only.
            request_records: Vec::new(),
            // Pi has no role concept; role_model_map is ignored on
            // this path.
            role_model_map_used: None,
        })
    }
}

enum SelectOutcome {
    Natural(std::io::Result<std::process::ExitStatus>),
    Kill(KillReason),
    NoKill,
}

fn classify_status(status: std::io::Result<std::process::ExitStatus>) -> ExitReason {
    match status {
        Ok(s) if s.success() => ExitReason::Completed,
        Ok(s) => ExitReason::Crashed {
            stderr_tail: format!("pi exited with status {s}"),
        },
        Err(e) => ExitReason::Crashed {
            stderr_tail: format!("wait err: {e}"),
        },
    }
}

/// Render the workdir as a short tree the model can read. Empty dirs
/// surface as "(empty)" so the agent knows it must write before it
/// can read anything useful.
fn describe_workdir(root: &std::path::Path) -> String {
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

/// Extract tool-call names in the order they appear in an assistant
/// content array. Used by the write-thrash detector to inspect the
/// per-turn tool sequence without paying the cost of the full
/// `harvest_assistant_blocks` extraction.
/// Per-turn tool sequence with the `arguments.path` field surfaced for
/// `write` calls. Used by the write-thrash detector to identify
/// same-path consecutive rewrites — under tier=FromScratch a model
/// may legitimately write Cargo.toml then src/lib.rs before its
/// first bash, and that's healthy scaffolding. Rewriting the SAME
/// file without an intervening verify is the actual thrash mode.
fn tools_in_turn(content: &Value) -> Vec<(String, Option<String>)> {
    content
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let is_tool = matches!(
                        b.get("type").and_then(|x| x.as_str()),
                        Some("tool_use") | Some("toolCall")
                    );
                    if !is_tool {
                        return None;
                    }
                    let name = b
                        .get("name")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())?;
                    let path = b
                        .get("arguments")
                        .or_else(|| b.get("input"))
                        .and_then(|args| args.get("path"))
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string());
                    Some((name, path))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Count tool-use blocks in an assistant content array — used by the
/// no-progress detector's per-turn increment. Safe on null / non-array
/// content.
fn count_tool_blocks(content: &Value) -> usize {
    content
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|b| {
                    matches!(
                        b.get("type").and_then(|x| x.as_str()),
                        Some("tool_use") | Some("toolCall")
                    )
                })
                .count()
        })
        .unwrap_or(0)
}

fn cap_tail(s: &str) -> String {
    if s.len() <= STDERR_TAIL_CAP_BYTES {
        s.to_string()
    } else {
        let cut = s.len() - STDERR_TAIL_CAP_BYTES;
        format!(
            "... (truncated {cut} leading bytes) ...\n{}",
            &s[cut..]
        )
    }
}

/// Parsed-and-relevant subset of pi's JSONL events.
///
/// `AssistantTurn` bundles a single `message_end` event's usage +
/// content[]. Tools and text are extracted by `harvest_assistant_blocks`
/// at drain time.
#[derive(Debug)]
enum ParsedEvent {
    AssistantTurn {
        input_tokens: u64,
        output_tokens: u64,
        content: Value,
    },
    Unknown,
}

/// Pi (`@earendil-works/pi-coding-agent`, observed 2026-05-21)
/// emits a single event per JSONL line with `type` ∈
/// {session, agent_start, turn_start, message_start, message_end,
///  turn_end, agent_end, auto_retry_start, auto_retry_end, ...}.
///
/// Assistant tool calls live INSIDE `message_end.message.content[]`
/// as `{type:"tool_use", name, input}` blocks (alongside text blocks).
/// `message_end.message.usage` carries token accounting with
/// `{input, output, totalTokens, cacheRead, cacheWrite}` fields.
fn parse_pi_line(line: &str) -> ParsedEvent {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ParsedEvent::Unknown;
    }
    let v: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return ParsedEvent::Unknown,
    };
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match kind {
        "message_end" => {
            let msg = v.get("message").cloned().unwrap_or(Value::Null);
            let role = msg
                .get("role")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let usage = msg.get("usage").cloned().unwrap_or(Value::Null);
            let input_tokens = extract_token_count(&usage, &["input_tokens", "inputTokens", "input"]);
            let output_tokens = extract_token_count(&usage, &["output_tokens", "outputTokens", "output"]);
            // Only credit tokens to assistant messages — user-message
            // echoes also carry message_end events but with zero usage.
            // Defensive: zero-usage events fall through harmlessly anyway.
            ParsedEvent::AssistantTurn {
                input_tokens,
                output_tokens,
                content: if role == "assistant" {
                    msg.get("content").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
            }
        }
        _ => ParsedEvent::Unknown,
    }
}

fn extract_token_count(usage: &Value, keys: &[&str]) -> u64 {
    for k in keys {
        if let Some(n) = usage.get(*k).and_then(|x| x.as_u64()) {
            return n;
        }
    }
    0
}

/// Walk an assistant-message `content` array and emit per-block
/// observations (tool_use → ToolCallRecord, text → string).
fn harvest_assistant_blocks(
    content: &Value,
    pi_build_cmd: &str,
    pi_verify_cmd: &str,
) -> (Vec<ToolCallRecord>, String) {
    let mut tools: Vec<ToolCallRecord> = Vec::new();
    let mut text = String::new();
    if let Some(arr) = content.as_array() {
        for block in arr {
            let block_type = block.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match block_type {
                "tool_use" | "toolCall" => {
                    let name = block
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let input = block
                        .get("input")
                        .or_else(|| block.get("arguments"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args_preview = serde_json::to_string(&input)
                        .unwrap_or_default()
                        .chars()
                        .take(256)
                        .collect::<String>();
                    // Normalize through the pi adapter into a
                    // canonical primitive kind. Observer-only —
                    // pi already executed the tool; this just
                    // labels the telemetry for cross-agent
                    // comparison. `Unrecognized` / `Unknown`
                    // outcomes leave `canonical_kind = None`,
                    // which the failure-class aggregator surfaces.
                    let canonical_kind = {
                        use commonwealth_agent_tools::adapter::{AgentToolAdapter, pi as pi_adapter};
                        let adapter = pi_adapter::Adapter::default()
                            .with_problem_commands(pi_build_cmd, pi_verify_cmd);
                        adapter.translate(&name, &input).canonical_kind()
                    };
                    tools.push(ToolCallRecord {
                        turn: 0,
                        tool: name,
                        args_preview,
                        ok: true,
                        canonical_kind,
                    });
                }
                "text" => {
                    if let Some(s) = block.get("text").and_then(|x| x.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(s);
                    }
                }
                _ => {}
            }
        }
    }
    (tools, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_message_end_extracts_pi_real_shape() {
        // Pi's real usage shape: `input` / `output` (no _tokens suffix).
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":120,"output":35,"totalTokens":155,"cacheRead":0,"cacheWrite":0}}}"#;
        match parse_pi_line(line) {
            ParsedEvent::AssistantTurn {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 120);
                assert_eq!(output_tokens, 35);
            }
            _ => panic!("expected AssistantTurn"),
        }
    }

    #[test]
    fn parse_message_end_supports_legacy_alias() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":2}}}"#;
        match parse_pi_line(line) {
            ParsedEvent::AssistantTurn {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 7);
                assert_eq!(output_tokens, 2);
            }
            _ => panic!("expected AssistantTurn"),
        }
    }

    #[test]
    fn parse_user_message_yields_null_content() {
        // User-message echoes carry message_end too but role=user;
        // content must NOT leak to the assistant-side harvest.
        let line = r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        match parse_pi_line(line) {
            ParsedEvent::AssistantTurn { content, .. } => {
                assert!(content.is_null());
            }
            _ => panic!("expected AssistantTurn"),
        }
    }

    #[test]
    fn harvest_extracts_tool_use_and_text_blocks() {
        let content = serde_json::json!([
            {"type": "text", "text": "I'll write src/lib.rs."},
            {"type": "tool_use", "id": "abc", "name": "write", "input": {"path": "src/lib.rs", "content": "pub fn solve() {}"}},
            {"type": "text", "text": "Done."},
        ]);
        let (tools, text) = harvest_assistant_blocks(&content, "cargo build", "cargo test --quiet --test integration");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool, "write");
        assert!(tools[0].args_preview.contains("src/lib.rs"));
        assert!(text.contains("I'll write"));
        assert!(text.contains("Done."));
    }

    #[test]
    fn harvest_handles_empty_content() {
        let (tools, text) = harvest_assistant_blocks(&serde_json::json!([]), "cargo build", "cargo test");
        assert!(tools.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn harvest_handles_null_content() {
        let (tools, text) = harvest_assistant_blocks(&serde_json::Value::Null, "cargo build", "cargo test");
        assert!(tools.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn parse_unknown_kind_is_lenient() {
        let line = r#"{"type":"some_new_event","payload":{}}"#;
        assert!(matches!(parse_pi_line(line), ParsedEvent::Unknown));
    }

    #[test]
    fn parse_blank_line_is_unknown() {
        assert!(matches!(parse_pi_line(""), ParsedEvent::Unknown));
        assert!(matches!(parse_pi_line("   "), ParsedEvent::Unknown));
    }

    #[test]
    fn parse_garbage_does_not_panic() {
        assert!(matches!(parse_pi_line("not json"), ParsedEvent::Unknown));
    }

    #[test]
    fn tool_allowlist_is_canonical() {
        assert_eq!(
            PI_TOOL_ALLOWLIST,
            &["read", "write", "bash", "find", "grep", "ls"]
        );
    }

    #[test]
    fn tool_allowlist_matches_canonical_adapter() {
        // The canonical pi adapter is the source of truth for which
        // pi tools the bench exposes. If a future PR adds a tool to
        // the adapter (e.g. opens up `mv` for some new primitive)
        // without updating PI_TOOL_ALLOWLIST, this fails.
        let canonical =
            commonwealth_agent_tools::adapter::pi::Adapter::pi_tool_allowlist();
        assert_eq!(PI_TOOL_ALLOWLIST, canonical);
    }

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

    // tools_in_turn — kept here because the helper is pi-local;
    // ThrashTracker state-machine tests live in
    // `runners::shared_detectors::tests` since that's where the
    // tracker now lives.

    #[test]
    fn tools_in_turn_extracts_path_for_write() {
        let content = serde_json::json!([
            {"type": "toolCall", "name": "read", "arguments": {"path": "src/lib.rs"}},
            {"type": "toolCall", "name": "write", "arguments": {"path": "src/lib.rs", "content": "..."}},
            {"type": "tool_use", "name": "bash", "input": {"command": "cargo test"}},
        ]);
        let tools = tools_in_turn(&content);
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0], ("read".to_string(), Some("src/lib.rs".to_string())));
        assert_eq!(tools[1], ("write".to_string(), Some("src/lib.rs".to_string())));
        assert_eq!(tools[2], ("bash".to_string(), None));
    }
}
