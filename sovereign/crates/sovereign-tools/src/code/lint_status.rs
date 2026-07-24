// SPDX-License-Identifier: AGPL-3.0-or-later
//! `lint_status` — return the current state of the background lint runner.
//!
//! Groups errors by file and separates warnings from failures. Cheap —
//! reads from a local SQLite cache, never triggers a run.
//!
//! ## When to call
//!
//! - Before every commit.
//! - Constantly during active editing — lint is fast (seconds, not minutes)
//!   and this tool costs microseconds.
//! - After any structural change (new imports, type changes) to catch type
//!   errors before wasting time on test runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_watchers::WatcherHeartbeat;
use corpus_engine_watchers::{LintResult, LintResultStore, LintRunSummary};

use super::watcher_health::{
    apply_liveness, assess, read_legacy, watcher_json, WatcherHealthInputs,
};

pub struct LintStatusTool {
    store: Arc<LintResultStore>,
    /// The command the watcher runs, e.g. "cargo check --workspace". Passed
    /// through to the response so agents can confirm scope coverage.
    watched_scope: Option<String>,
    /// Legacy one-shot liveness bool. Superseded by `heartbeat`.
    watcher_active: Option<Arc<AtomicBool>>,
    /// Shared coordinator heartbeat — authoritative liveness signal. See
    /// [`super::watcher_health`].
    heartbeat: Option<Arc<WatcherHeartbeat>>,
    /// Workspace root, used to resolve relative paths in the `files` query
    /// param and to run `git diff` when `changed = true`. Optional because
    /// MCP-spawned daemons may not have a workspace configured; in that
    /// case per-file queries require absolute paths from the caller and
    /// `changed = true` becomes a no-op.
    workspace_root: Option<PathBuf>,
}

impl LintStatusTool {
    pub fn new(store: Arc<LintResultStore>) -> Self {
        Self {
            store,
            watched_scope: None,
            watcher_active: None,
            heartbeat: None,
            workspace_root: None,
        }
    }

    pub fn with_watched_scope(mut self, scope: String) -> Self {
        self.watched_scope = Some(scope);
        self
    }

    pub fn with_watcher_active(mut self, flag: Arc<AtomicBool>) -> Self {
        self.watcher_active = Some(flag);
        self
    }

    /// Attach the coordinator heartbeat. Preferred over
    /// [`with_watcher_active`](Self::with_watcher_active) — it detects a
    /// watcher that started and then died.
    pub fn with_heartbeat(mut self, heartbeat: Arc<WatcherHeartbeat>) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }

    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }
}

#[async_trait]
impl Tool for LintStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "lint_status".to_string(),
            name: "Lint Status".to_string(),
            description: "Return lint/type-check status from the background watcher. \
                          NEVER run `cargo check` or `cargo build` via Bash — the watcher \
                          holds the Cargo file lock continuously; running cargo check \
                          alongside it causes BOTH processes to stall indefinitely waiting \
                          for the lock. This call reads cached results in microseconds with \
                          zero contention. \
                          Response fields to trust the result: \
                          `age_seconds` — how old the result is (typically < 30s after an \
                          edit; if large, the watcher may have been idle); \
                          `watched_scope` — the exact command the watcher runs (e.g. \
                          'cargo check --workspace'), confirming which crates are covered; \
                          `watcher` — the liveness object `{live, reason, configured, \
                          heartbeat_age_secs, hint}`. Read it FIRST: when `live` is false \
                          the result below is orphaned (no watcher is running to keep it \
                          current), `reason` says why, `hint` says what to do. \
                          `watcher_active` mirrors `watcher.live` for back-compat. \
                          Status: 'fresh_passing' (clean, age_seconds shows recency), \
                          'fresh_failing' (errors in response), 'stale' (files changed \
                          since last run — watcher will rerun automatically on next save), \
                          'running' (in progress — check again in ~15s), \
                          'watcher_down' (a completed run exists but NO live watcher — do \
                          not trust it; fall back to scripts/sovereign-lint.sh per \
                          `watcher.hint`), 'never_run' (no run yet — see `watcher.reason`)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter freshness to a specific set of paths. The response gains a per-file `files[]` array with status (`fresh_passing | fresh_failing | stale | never_checked`), `checked_at_unix`, `mtime_unix`, and per-file error/warning counts. Top-level `errors[]` / `warnings[]` are filtered to these files too. Paths may be absolute or workspace-relative."
                    },
                    "changed": {
                        "type": "boolean",
                        "description": "Shortcut for `files`: auto-derive the list from `git diff --name-only HEAD` + untracked `.rs` files. The killer query for active editing: 'are MY files clean?' Mutually-exclusive with `files`; if both provided, `files` wins."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You've edited one or more files and want to know if the code compiles. Do NOT run `cargo check` or `cargo build` — that fights the background watcher for the Cargo file lock and blocks both processes. This reads the watcher's cached result instantly with no contention.".into(),
                    call: serde_json::json!({}),
                },
                ToolExample {
                    situation: "Active edit loop — you just touched a few files and want to know if THOSE files are clean. The workspace-wide check may still be running, but per-file freshness lands as soon as cargo finishes each crate.".into(),
                    call: serde_json::json!({ "changed": true }),
                },
                ToolExample {
                    situation: "Scripting / explicit query against a known file set (e.g. a pre-commit hook with a precomputed list).".into(),
                    call: serde_json::json!({ "files": ["corpus-engine/src/recipe.rs", "sovereign/crates/sovereign-cli/src/drift_cmd_orchestrator.rs"] }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "status":          { "type": "string", "enum": ["fresh_passing","fresh_failing","stale","running","watcher_down","never_run"] },
                    "age_seconds":     { "type": "integer" },
                    "pass_count":      { "type": "integer" },
                    "fail_count":      { "type": "integer" },
                    "warn_count":      { "type": "integer" },
                    "watcher_active":  { "type": "boolean" },
                    "watcher": {
                        "type": "object",
                        "description": "Watcher liveness. Read before trusting `status`. When `live` is false the run below is orphaned.",
                        "properties": {
                            "live":               { "type": "boolean" },
                            "reason":             { "type": "string", "enum": ["live","not_configured","watcher_dead","legacy_active","inferred_from_age","unknown"] },
                            "configured":         { "type": "boolean" },
                            "heartbeat_age_secs": { "type": ["integer","null"] },
                            "hint":               { "type": ["string","null"] }
                        }
                    },
                    "watched_scope":   { "type": "string" },
                    "errors":          { "type": "array" },
                    "warnings":        { "type": "array" },
                    "run_id":          { "type": "integer" },
                    "output_truncated":{ "type": "boolean" },
                    // `previous_run` is populated whenever `status` is
                    // `running` and a prior completed run exists. Lets
                    // callers see "in flight, but the last completed
                    // run failed with these errors" rather than polling
                    // `null` indefinitely on a watcher wedged against a
                    // stable compile error.
                    "previous_run":    {
                        "type": "object",
                        "properties": {
                            "status":           { "type": "string", "enum": ["fresh_passing","fresh_failing"] },
                            "run_id":           { "type": "integer" },
                            "pass_count":       { "type": "integer" },
                            "fail_count":       { "type": "integer" },
                            "warn_count":       { "type": "integer" },
                            "exit_code":        { "type": "integer" },
                            "age_seconds":      { "type": "integer" },
                            "looks_like_compile_failure": { "type": "boolean" },
                            "errors":           { "type": "array" }
                        }
                    },
                    "files": {
                        "type": "array",
                        "description": "Per-file freshness, populated when the call passes `files` or `changed`. Each entry: { path, status, checked_at_unix, mtime_unix, errors, warnings }. `status` uses the same vocabulary as the top-level workspace status: `fresh_passing | fresh_failing | stale | never_checked`.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path":            { "type": "string" },
                                "status":          { "type": "string", "enum": ["fresh_passing","fresh_failing","stale","never_checked"] },
                                "checked_at_unix": { "type": ["integer","null"] },
                                "mtime_unix":      { "type": ["integer","null"] },
                                "errors":          { "type": "integer" },
                                "warnings":        { "type": "integer" }
                            }
                        }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Signal: a one-liner when the lint watcher shows failures or
    /// stale state. Silent when the last run was clean. Read-only
    /// SQLite point lookup — no extra I/O beyond what `execute()`
    /// would do on the same call.
    async fn signal(&self) -> Option<String> {
        let summary = self.store.latest_run().await.ok().flatten()?;
        if summary.passed() {
            return None;
        }
        let age = summary
            .finished_at
            .elapsed()
            .ok()
            .map(|d| format!(" age {}s", d.as_secs()))
            .unwrap_or_default();
        // Find the file with the most recent failure to make the line
        // actionable (operators can go straight there).
        let top_file = self
            .store
            .latest_failures(1)
            .await
            .ok()
            .and_then(|rs| rs.into_iter().next())
            .map(|r| r.file);
        let where_ = top_file
            .map(|f| format!(" (first in {f})"))
            .unwrap_or_default();
        Some(format!("{} lint error(s){where_}{age}", summary.fail_count))
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let legacy_active = read_legacy(&self.watcher_active);
        let configured = self.watched_scope.is_some();

        // Resolve the per-file query (if any). `files` wins over
        // `changed`; both unset means workspace-only mode.
        let query_paths = self.resolve_query_paths(params);

        let is_running = self.store.run_in_progress().await.unwrap_or(false);

        if is_running {
            // Run in progress = watcher is doing real work right now.
            // Per-file freshness is still computable against the
            // last completed run (if any) — answer the kill query
            // "are my files clean as of the most recent finish?"
            // while the workspace check trundles on.
            //
            // The `previous_run` block additionally surfaces the prior
            // run's summary + errors so a watcher perpetually
            // re-failing on the same compile error is observable from
            // a single call instead of looking like an infinite
            // "running, summary: null". The interesting flag is
            // `looks_like_compile_failure`: exit_code != 0 AND zero
            // diagnostics emitted (pass+fail+warn == 0) — the signature
            // of `cargo check` aborting before producing any
            // file-shaped output.
            let prior = self.store.latest_run().await.ok().flatten();
            let files_block = if let (Some(paths), Some(run)) = (&query_paths, &prior) {
                Some(self.build_files_block(paths, run).await)
            } else {
                query_paths
                    .as_ref()
                    .map(|paths| paths.iter().map(|p| never_checked_entry(p)).collect())
            };
            let previous_run = build_previous_run(&self.store, prior.as_ref()).await;
            let reason = assess(&WatcherHealthInputs {
                heartbeat: self.heartbeat.as_ref(),
                legacy_active,
                configured,
                run_in_progress: true,
                last_run_age_secs: None,
            });
            return Ok(StepOutput::Json(json!({
                "status": "running",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": reason.is_live(),
                "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
                "files": files_block,
                "previous_run": previous_run,
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "lint_status".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
            let files_block = query_paths.as_ref().map(|paths| {
                paths
                    .iter()
                    .map(|p| never_checked_entry(p))
                    .collect::<Vec<_>>()
            });
            let reason = assess(&WatcherHealthInputs {
                heartbeat: self.heartbeat.as_ref(),
                legacy_active,
                configured,
                run_in_progress: false,
                last_run_age_secs: None,
            });
            return Ok(StepOutput::Json(json!({
                "status": "never_run",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": reason.is_live(),
                "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
                "files": files_block,
            })));
        };

        let age_seconds = SystemTime::now()
            .duration_since(run.finished_at)
            .unwrap_or_default()
            .as_secs();

        let stale = self
            .store
            .stale_files_since_last_run()
            .await
            .unwrap_or_default();

        let raw_status = if !stale.is_empty() {
            "stale"
        } else if run.passed() {
            "fresh_passing"
        } else {
            "fresh_failing"
        };

        // Demote to `watcher_down` when no live watcher could have
        // produced this against the current tree (see watcher_health).
        let reason = assess(&WatcherHealthInputs {
            heartbeat: self.heartbeat.as_ref(),
            legacy_active,
            configured,
            run_in_progress: false,
            last_run_age_secs: Some(age_seconds),
        });
        let status = apply_liveness(raw_status, reason);

        let raw_failures = self.store.latest_failures(50).await.unwrap_or_default();
        let raw_warnings = self.store.latest_warnings(50).await.unwrap_or_default();

        // Filter top-level diagnostics to the queried file set when
        // a filter is active. The unfiltered counts are still in
        // `summary` (workspace-level), so this is purely an
        // ergonomic narrowing of the per-finding lists.
        let (failures, warnings_raw) = if let Some(paths) = &query_paths {
            (
                filter_results_by_paths(&raw_failures, paths, self.workspace_root.as_deref()),
                filter_results_by_paths(&raw_warnings, paths, self.workspace_root.as_deref()),
            )
        } else {
            (raw_failures.clone(), raw_warnings.clone())
        };

        let errors: Vec<_> = failures
            .iter()
            .map(|f| {
                json!({
                    "file": f.file,
                    "output": f.output,
                    "output_truncated": f.output_truncated,
                    "line": f.line,
                    "col": f.col,
                    "run_id": f.run_id
                })
            })
            .collect();

        let warnings: Vec<_> = warnings_raw
            .iter()
            .map(|w| {
                json!({
                    "file": w.file,
                    "output": w.output,
                    "line": w.line,
                    "col": w.col
                })
            })
            .collect();

        let stale_paths: Vec<String> = stale
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        // Per-file freshness, populated only when the caller asked
        // for it. Reuses the same raw_* diagnostics so the per-file
        // counts agree with the (possibly filtered) top-level
        // arrays.
        let files_block: Option<Vec<serde_json::Value>> = query_paths.as_ref().map(|paths| {
            paths
                .iter()
                .map(|p| self.freshness_entry(p, &run, &stale, &raw_failures, &raw_warnings))
                .collect()
        });

        Ok(StepOutput::Json(json!({
            "status": status,
            "summary": {
                "run_id": run.run_id,
                "pass_count": run.pass_count,
                "fail_count": run.fail_count,
                "warn_count": run.warn_count,
                "elapsed_ms": run.elapsed_ms,
            },
            "errors": errors,
            "warnings": warnings,
            "stale_since": stale_paths,
            "age_seconds": age_seconds,
            "watched_scope": self.watched_scope,
            "watcher_active": reason.is_live(),
            "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
            "files": files_block,
        })))
    }
}

impl LintStatusTool {
    /// Parse the `files` / `changed` params and resolve to absolute
    /// paths. Returns `None` when neither param is set (workspace-only
    /// mode — existing callers see no behaviour change).
    fn resolve_query_paths(&self, params: &serde_json::Value) -> Option<Vec<PathBuf>> {
        let explicit: Option<Vec<String>> =
            params.get("files").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
        let want_changed = params
            .get("changed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let raw: Vec<String> = if let Some(paths) = explicit {
            // `files` wins over `changed` if both are supplied.
            paths
        } else if want_changed {
            self.git_changed_files().unwrap_or_default()
        } else {
            return None;
        };

        let resolved: Vec<PathBuf> = raw
            .into_iter()
            .map(|p| self.canonicalize_query_path(&p))
            .collect();
        Some(resolved)
    }

    /// Resolve a caller-supplied path. Absolute → as-is. Relative →
    /// joined to workspace_root if known, else cwd. Canonicalization
    /// is best-effort: if it fails (deleted file, broken symlink), we
    /// keep the joined path so the response can still report
    /// `never_checked` or `stale` honestly.
    fn canonicalize_query_path(&self, p: &str) -> PathBuf {
        let raw = PathBuf::from(p);
        let joined = if raw.is_absolute() {
            raw
        } else if let Some(root) = &self.workspace_root {
            root.join(&raw)
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&raw))
                .unwrap_or(raw)
        };
        std::fs::canonicalize(&joined).unwrap_or(joined)
    }

    /// `git diff --name-only HEAD` + untracked Rust files. Returns
    /// workspace-relative paths; canonicalization happens later.
    /// Silent on failure (no git, no HEAD, not a repo) — the caller
    /// gets an empty list and the response shape stays consistent.
    fn git_changed_files(&self) -> Option<Vec<String>> {
        let root = self.workspace_root.as_ref()?;
        let mut out: Vec<String> = Vec::new();
        let diff = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--name-only", "HEAD"])
            .output()
            .ok()?;
        if diff.status.success() {
            for line in String::from_utf8_lossy(&diff.stdout).lines() {
                if line.ends_with(".rs") {
                    out.push(line.to_string());
                }
            }
        }
        // Pick up new files that aren't yet tracked.
        let untracked = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "--others", "--exclude-standard"])
            .output()
            .ok()?;
        if untracked.status.success() {
            for line in String::from_utf8_lossy(&untracked.stdout).lines() {
                if line.ends_with(".rs") {
                    out.push(line.to_string());
                }
            }
        }
        Some(out)
    }

    /// Build the `files[]` array from an already-resolved list of
    /// query paths against a known prior run. Used in the
    /// `running` branch where we're answering "as of the last
    /// completed run, were these clean?"
    async fn build_files_block(
        &self,
        paths: &[PathBuf],
        run: &LintRunSummary,
    ) -> Vec<serde_json::Value> {
        let stale = self
            .store
            .stale_files_since_last_run()
            .await
            .unwrap_or_default();
        let failures = self.store.latest_failures(200).await.unwrap_or_default();
        let warnings = self.store.latest_warnings(200).await.unwrap_or_default();
        paths
            .iter()
            .map(|p| self.freshness_entry(p, run, &stale, &failures, &warnings))
            .collect()
    }

    /// Compute the per-file freshness JSON entry. The status enum
    /// mirrors the workspace status vocabulary so callers parse
    /// both levels with the same rule.
    fn freshness_entry(
        &self,
        path: &Path,
        run: &LintRunSummary,
        stale: &[PathBuf],
        failures: &[LintResult],
        warnings: &[LintResult],
    ) -> serde_json::Value {
        let checked_at_unix = run
            .finished_at
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64);
        let mtime_unix = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);

        let path_str = path.to_string_lossy();
        let is_stale_marked = stale.iter().any(|s| paths_match(s, path));
        let file_mtime_after_run = match (mtime_unix, checked_at_unix) {
            (Some(m), Some(c)) => m > c,
            _ => false,
        };

        let file_failures = failures
            .iter()
            .filter(|r| diag_matches_path(&r.file, path, self.workspace_root.as_deref()))
            .count();
        let file_warnings = warnings
            .iter()
            .filter(|r| diag_matches_path(&r.file, path, self.workspace_root.as_deref()))
            .count();

        let status = if is_stale_marked || file_mtime_after_run {
            "stale"
        } else if file_failures > 0 {
            "fresh_failing"
        } else {
            "fresh_passing"
        };

        json!({
            "path": path_str,
            "status": status,
            "checked_at_unix": checked_at_unix,
            "mtime_unix": mtime_unix,
            "errors": file_failures,
            "warnings": file_warnings,
        })
    }
}

/// Default per-file entry for "no run has happened" or "watcher
/// hasn't seen anything" cases. Reports `never_checked` plus
/// best-effort mtime so the caller still knows the file exists.
fn never_checked_entry(path: &Path) -> serde_json::Value {
    let mtime_unix = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    json!({
        "path": path.to_string_lossy(),
        "status": "never_checked",
        "checked_at_unix": serde_json::Value::Null,
        "mtime_unix": mtime_unix,
        "errors": 0,
        "warnings": 0,
    })
}

/// Filter a raw diagnostics vec down to the files in `query_paths`.
/// Cargo's `file` column may be workspace-relative or absolute; we
/// resolve both sides to the same shape before comparing.
fn filter_results_by_paths(
    raw: &[LintResult],
    query_paths: &[PathBuf],
    workspace_root: Option<&Path>,
) -> Vec<LintResult> {
    raw.iter()
        .filter(|r| {
            query_paths
                .iter()
                .any(|qp| diag_matches_path(&r.file, qp, workspace_root))
        })
        .cloned()
        .collect()
}

/// True iff a diagnostic's `file` column refers to `query_path`.
/// Handles three flavours cargo emits: absolute, workspace-relative,
/// and `crate-name/src/...` shapes. The query path is already
/// canonicalized to absolute; we normalize the diagnostic the same
/// way and compare. Suffix-match is the last-resort fallback for
/// odd cargo output forms.
fn diag_matches_path(diag_file: &str, query_path: &Path, workspace_root: Option<&Path>) -> bool {
    let diag = Path::new(diag_file);
    if diag.is_absolute() {
        if let (Ok(a), Ok(b)) = (
            std::fs::canonicalize(diag),
            std::fs::canonicalize(query_path),
        ) {
            return a == b;
        }
        return diag == query_path;
    }
    if let Some(root) = workspace_root {
        let joined = root.join(diag);
        if let (Ok(a), Ok(b)) = (
            std::fs::canonicalize(&joined),
            std::fs::canonicalize(query_path),
        ) {
            if a == b {
                return true;
            }
        }
    }
    // Suffix fallback — handles edge cases like cargo emitting
    // `corpus-engine/src/recipe.rs` without a leading
    // `commonwealth-ai/`.
    query_path.to_string_lossy().ends_with(diag_file)
}

/// True iff two paths refer to the same file. Used for the stale
/// list, which is stored as caller-supplied PathBufs (could be
/// either absolute or relative depending on who marked them).
fn paths_match(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// Build the `previous_run` payload returned alongside `status:
/// running`. Returns `Value::Null` when no prior run exists.
///
/// `looks_like_compile_failure` flips to true when the prior run
/// exited non-zero AND produced zero diagnostics
/// (pass+fail+warn = 0) — the signature of `cargo check` aborting
/// in the build phase before any file-shaped output. That's the
/// failure mode that previously showed up as a perpetual
/// `{status: running, summary: null}` because the watcher kept
/// re-launching against the same broken workspace.
async fn build_previous_run(
    store: &LintResultStore,
    prior: Option<&LintRunSummary>,
) -> serde_json::Value {
    let Some(run) = prior else {
        return serde_json::Value::Null;
    };
    let age_seconds = SystemTime::now()
        .duration_since(run.finished_at)
        .unwrap_or_default()
        .as_secs();
    let status = if run.passed() {
        "fresh_passing"
    } else {
        "fresh_failing"
    };
    let looks_like_compile_failure =
        run.exit_code != 0 && run.pass_count == 0 && run.fail_count == 0 && run.warn_count == 0;
    let errors = store
        .latest_failures(10)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            json!({
                "file": f.file,
                "output": f.output,
                "output_truncated": f.output_truncated,
                "line": f.line,
                "col": f.col,
                "run_id": f.run_id,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "status": status,
        "run_id": run.run_id,
        "pass_count": run.pass_count,
        "fail_count": run.fail_count,
        "warn_count": run.warn_count,
        "exit_code": run.exit_code,
        "age_seconds": age_seconds,
        "looks_like_compile_failure": looks_like_compile_failure,
        "errors": errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diag_matches_path_absolute() {
        let tmp = std::env::temp_dir().join("lint_status_match_abs.rs");
        std::fs::write(&tmp, b"").unwrap();
        let canonical = std::fs::canonicalize(&tmp).unwrap();
        assert!(diag_matches_path(
            canonical.to_str().unwrap(),
            &canonical,
            None
        ));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn diag_matches_path_workspace_relative() {
        // Stage a temp workspace with a real file so canonicalize() succeeds.
        let workspace = std::env::temp_dir().join("lint_status_ws_rel");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        let file = workspace.join("src").join("lib.rs");
        std::fs::write(&file, b"").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        // Cargo would emit a workspace-relative diagnostic.
        assert!(diag_matches_path(
            "src/lib.rs",
            &canonical,
            Some(&workspace)
        ));
        std::fs::remove_file(&file).ok();
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn diag_matches_path_suffix_fallback() {
        // Crate-name-prefixed diagnostic when the canonical workspace
        // join doesn't resolve. Suffix-match is the safety net.
        let query = PathBuf::from("/tmp/some/synthetic/path/corpus-engine/src/recipe.rs");
        assert!(diag_matches_path(
            "corpus-engine/src/recipe.rs",
            &query,
            None
        ));
    }

    #[test]
    fn never_checked_entry_reports_null_checked_at() {
        let entry = never_checked_entry(&PathBuf::from("/tmp/does-not-exist.rs"));
        assert_eq!(entry["status"], "never_checked");
        assert!(entry["checked_at_unix"].is_null());
        assert_eq!(entry["errors"], 0);
        assert_eq!(entry["warnings"], 0);
    }

    #[test]
    fn resolve_query_paths_prefers_explicit_files_over_changed() {
        let store = Arc::new(LintResultStore::open(std::path::Path::new(":memory:")).unwrap());
        let tool = LintStatusTool::new(store).with_workspace_root(std::env::temp_dir());
        let params = json!({
            "files": ["a.rs", "b.rs"],
            "changed": true,
        });
        let resolved = tool.resolve_query_paths(&params).unwrap();
        // Two paths in, two out — `files` was used, `changed` did
        // not append a git-derived list on top.
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn resolve_query_paths_returns_none_when_neither_set() {
        let store = Arc::new(LintResultStore::open(std::path::Path::new(":memory:")).unwrap());
        let tool = LintStatusTool::new(store);
        assert!(tool.resolve_query_paths(&json!({})).is_none());
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "lint-status-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    /// When the watcher is mid-run AND the last completed lint run was
    /// a compile failure (exit_code != 0, zero diagnostics emitted —
    /// `cargo check` aborted before any file output), the running-branch
    /// response must surface the previous run via `previous_run` with
    /// `looks_like_compile_failure: true`. Otherwise a watcher
    /// repeatedly retrying the same broken workspace looks identical
    /// (`{status: running, summary: null}`) to a freshly-kicked-off
    /// healthy run.
    #[tokio::test]
    async fn running_branch_surfaces_compile_failed_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LintResultStore::open(&dir.path().join("lint.db")).unwrap());

        // Run 1: compile-failure shape — non-zero exit, no diagnostics.
        let r1 = store.begin_run().await.unwrap();
        store.finish_run(r1, 101, 1234).await.unwrap();

        // Run 2: in-flight. Marks run_in_progress true in execute().
        let _r2 = store.begin_run().await.unwrap();

        let tool = LintStatusTool::new(Arc::clone(&store));
        let out = tool.execute(&json!({}), &ctx()).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };

        assert_eq!(v["status"], "running");
        assert!(v["summary"].is_null());

        let prev = &v["previous_run"];
        assert!(!prev.is_null(), "previous_run must be populated");
        assert_eq!(prev["status"], "fresh_failing");
        assert_eq!(prev["exit_code"], 101);
        assert_eq!(prev["pass_count"], 0);
        assert_eq!(prev["fail_count"], 0);
        assert_eq!(prev["warn_count"], 0);
        assert_eq!(
            prev["looks_like_compile_failure"], true,
            "non-zero exit with zero diagnostics is the compile-failure signature"
        );
    }

    /// No completed run yet → previous_run is JSON null. Lets callers
    /// distinguish "no history" from "history says everything's fine".
    #[tokio::test]
    async fn running_branch_with_no_prior_run_returns_null_previous() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LintResultStore::open(&dir.path().join("lint.db")).unwrap());
        let _r = store.begin_run().await.unwrap();

        let tool = LintStatusTool::new(Arc::clone(&store));
        let out = tool.execute(&json!({}), &ctx()).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(v["status"], "running");
        assert!(
            v["previous_run"].is_null(),
            "previous_run must be null when no completed run exists"
        );
    }
}
