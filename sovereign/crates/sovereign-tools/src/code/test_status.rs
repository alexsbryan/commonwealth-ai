//! `test_status` — return the current state of the background test runner.
//!
//! Reads from [`TestResultStore`] — never triggers a run itself. The watcher
//! runs automatically on file changes; this tool gives an agent an instant,
//! cheap snapshot without waiting for a run to complete.
//!
//! ## When to call
//!
//! - Before claiming a change is correct. If status is `fresh + passing`, the
//!   tests you care about have already run on the current file state.
//! - Before committing. `stale` status means files changed since the last run.
//! - Constantly during active editing — this call costs microseconds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::test_results::TestResultStore;

pub struct TestStatusTool {
    store: Arc<TestResultStore>,
    /// Optional handle to the watcher for live "Running" status.
    /// If None, status is derived entirely from the store.
    running_flag: Option<Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>>,
    /// The command the watcher runs, e.g. "cargo test --workspace". Passed
    /// through to the response so agents can confirm scope coverage.
    watched_scope: Option<String>,
    /// Shared with the watcher coordinator — true while the FS watcher is live.
    watcher_active: Option<Arc<AtomicBool>>,
}

impl TestStatusTool {
    pub fn new(store: Arc<TestResultStore>) -> Self {
        Self {
            store,
            running_flag: None,
            watched_scope: None,
            watcher_active: None,
        }
    }

    /// Attach the watcher's run_handle so we can report "Running" accurately.
    pub fn with_run_handle(
        mut self,
        handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ) -> Self {
        self.running_flag = Some(handle);
        self
    }

    pub fn with_watched_scope(mut self, scope: String) -> Self {
        self.watched_scope = Some(scope);
        self
    }

    pub fn with_watcher_active(mut self, flag: Arc<AtomicBool>) -> Self {
        self.watcher_active = Some(flag);
        self
    }
}

#[async_trait]
impl Tool for TestStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "test_status".to_string(),
            name: "Test Status".to_string(),
            description: "Return test suite status from the background watcher. \
                          NEVER run `cargo test` via Bash — the watcher holds the Cargo \
                          file lock continuously; running cargo test alongside it causes \
                          BOTH processes to stall indefinitely waiting for the lock. \
                          This call reads cached results in microseconds with zero contention. \
                          Response fields to trust the result: \
                          `age_seconds` — how old the result is (if 0-60s, the watcher \
                          ran on your current changes); \
                          `watched_scope` — the exact command the watcher runs (e.g. \
                          'cargo test --workspace'), confirming which crates are covered; \
                          `watcher_active` — true = watcher is live and will rerun on your \
                          next save automatically; false = watcher not running, only then \
                          fall back to Bash. \
                          Status: 'fresh_passing' (all pass — safe to proceed), \
                          'fresh_failing' (failures in response), 'stale' (files changed \
                          since last run — call run_tests then poll), 'running' (in \
                          progress — check again in 30-60s), 'never_run' (watcher not \
                          configured — fall back to Bash)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You've made a change and want to know if tests pass. Do NOT run `cargo test` — it contends with the background watcher for the Cargo file lock. This reads the watcher's result instantly. If status is 'stale', call run_tests to force a fresh run, then poll back here in ~30s.".into(),
                    call: serde_json::json!({}),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "status":          { "type": "string", "enum": ["fresh_passing","fresh_failing","stale","running","never_run"] },
                    "age_seconds":     { "type": "integer" },
                    "pass_count":      { "type": "integer" },
                    "fail_count":      { "type": "integer" },
                    "watcher_active":  { "type": "boolean" },
                    "watched_scope":   { "type": "string" },
                    "failures":        { "type": "array" },
                    "run_id":          { "type": "integer" },
                    "output_truncated":{ "type": "boolean" },
                    // `previous_run` is populated whenever `status` is
                    // `running` and a prior completed run exists. Lets
                    // callers see "in flight, but the last completed
                    // run failed with these errors" rather than polling
                    // `null` indefinitely on a watcher wedged against
                    // a stable compile error. Mirrors the prior_run
                    // surface on `lint_status`.
                    "previous_run":    {
                        "type": "object",
                        "properties": {
                            "status":           { "type": "string", "enum": ["fresh_passing","fresh_failing"] },
                            "run_id":           { "type": "integer" },
                            "pass_count":       { "type": "integer" },
                            "fail_count":       { "type": "integer" },
                            "exit_code":        { "type": "integer" },
                            "age_seconds":      { "type": "integer" },
                            "looks_like_compile_failure": { "type": "boolean" },
                            "failures":         { "type": "array" }
                        }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Signal: one-liner when the last test run failed. Silent on a
    /// clean run. SQLite point lookup — no extra work.
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
        Some(format!(
            "last test run: {} passed, {} failed{age}",
            summary.pass_count, summary.fail_count
        ))
    }

    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let explicit_active = self
            .watcher_active
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed));

        // Check running flag.
        let is_running = if let Some(ref flag) = self.running_flag {
            flag.lock()
                .await
                .as_ref()
                .map(|h| !h.is_finished())
                .unwrap_or(false)
        } else {
            self.store.run_in_progress().await.unwrap_or(false)
        };

        if is_running {
            // Surface the previous completed run so a watcher
            // perpetually re-failing on the same compile error is
            // observable from a single `test_status` call, instead of
            // showing `{status: running, summary: null}` indefinitely.
            // The interesting case is `looks_like_compile_failure`:
            // exit_code != 0 AND zero tests reported (pass+fail = 0),
            // which is the signature of `cargo test` failing in the
            // compile phase before any test executes. That's the
            // failure mode the in-flight watcher will keep hitting
            // until someone fixes the build.
            let previous_run = build_previous_run(&self.store).await;
            return Ok(StepOutput::Json(json!({
                "status": "running",
                "summary": null,
                "failures": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": derive_watcher_active(explicit_active, None, true),
                "previous_run": previous_run,
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "test_status".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
            return Ok(StepOutput::Json(json!({
                "status": "never_run",
                "summary": null,
                "failures": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": derive_watcher_active(explicit_active, None, false),
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

        let status = if !stale.is_empty() {
            "stale"
        } else if run.passed() {
            "fresh_passing"
        } else {
            "fresh_failing"
        };

        let failures = self
            .store
            .latest_failures(10)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "output": f.output,
                    "output_truncated": f.output_truncated,
                    "run_id": f.run_id
                })
            })
            .collect::<Vec<_>>();

        let stale_paths: Vec<String> = stale
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        Ok(StepOutput::Json(json!({
            "status": status,
            "summary": {
                "run_id": run.run_id,
                "pass_count": run.pass_count,
                "fail_count": run.fail_count,
                "exit_code": run.exit_code,
            },
            "failures": failures,
            "stale_since": stale_paths,
            "age_seconds": age_seconds,
            "watched_scope": self.watched_scope,
            "watcher_active": derive_watcher_active(explicit_active, Some(age_seconds), false),
        })))
    }
}

/// Build the `previous_run` payload returned alongside `status:
/// running`. Returns `Value::Null` when no prior run exists.
///
/// The block makes a watcher that's been wedged on a compile error
/// observable from one call: `looks_like_compile_failure` flips to
/// true when the prior run exited non-zero AND reported zero tests
/// (pass+fail = 0). That's the exact signature of `cargo test`
/// failing in the build phase before any test could execute — the
/// state that previously presented as "running, summary: null" for
/// hours because the watcher kept relaunching against the same
/// broken workspace.
async fn build_previous_run(store: &TestResultStore) -> serde_json::Value {
    let Some(run) = store.latest_run().await.ok().flatten() else {
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
        run.exit_code != 0 && run.pass_count == 0 && run.fail_count == 0;
    let failures = store
        .latest_failures(10)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            json!({
                "name": f.name,
                "output": f.output,
                "output_truncated": f.output_truncated,
                "run_id": f.run_id,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "status": status,
        "run_id": run.run_id,
        "pass_count": run.pass_count,
        "fail_count": run.fail_count,
        "exit_code": run.exit_code,
        "age_seconds": age_seconds,
        "looks_like_compile_failure": looks_like_compile_failure,
        "failures": failures,
    })
}

/// Same shape and rationale as the helper in `lint_status.rs`. See
/// that module's doc-comment on `derive_watcher_active` for the
/// daemon-vs-CLI fallback logic.
const WATCHER_FRESH_SECS: u64 = 600;

fn derive_watcher_active(
    explicit: Option<bool>,
    last_run_age_secs: Option<u64>,
    run_in_progress: bool,
) -> bool {
    if let Some(flag) = explicit {
        return flag;
    }
    if run_in_progress {
        return true;
    }
    matches!(last_run_age_secs, Some(age) if age < WATCHER_FRESH_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::test_results::{TestResultKind, TestResultStore};
    use sovereign_core::types::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "test-status-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    /// When the watcher is mid-run AND the last completed run was a
    /// compile failure (exit_code != 0, zero tests reported), the
    /// response must expose the previous run with
    /// `looks_like_compile_failure: true` so the caller can diagnose
    /// a wedged-on-compile state from a single call. Regression-pins
    /// the previously-observable behaviour of returning
    /// `{status: "running", summary: null}` indefinitely with no
    /// signal about WHY the watcher kept relaunching against the same
    /// broken workspace.
    #[tokio::test]
    async fn running_branch_surfaces_compile_failed_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            TestResultStore::open(&dir.path().join("test.db")).unwrap(),
        );

        // Run 1: completed, compile-failure shape (no test results,
        // non-zero exit).
        let r1 = store.begin_run().await.unwrap();
        store.finish_run(r1, 101).await.unwrap();

        // Run 2: in-flight (never finished). Triggers run_in_progress
        // in execute().
        let _r2 = store.begin_run().await.unwrap();

        let tool = TestStatusTool::new(Arc::clone(&store));
        let out = tool.execute(&json!({}), &ctx()).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };

        assert_eq!(v["status"], "running");
        assert!(v["summary"].is_null(), "summary stays null while running");

        let prev = &v["previous_run"];
        assert!(!prev.is_null(), "previous_run must be populated");
        assert_eq!(prev["status"], "fresh_failing");
        assert_eq!(prev["exit_code"], 101);
        assert_eq!(prev["pass_count"], 0);
        assert_eq!(prev["fail_count"], 0);
        assert_eq!(
            prev["looks_like_compile_failure"], true,
            "exit_code != 0 with zero tests is the compile-failure signature"
        );
    }

    /// `previous_run` is `null` (not omitted, not an empty object)
    /// when no prior completed run exists. Callers should be able to
    /// distinguish "no history" from "history says everything's fine."
    #[tokio::test]
    async fn running_branch_with_no_prior_run_returns_null_previous() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            TestResultStore::open(&dir.path().join("test.db")).unwrap(),
        );

        // Only an in-flight run; no completed history.
        let _r = store.begin_run().await.unwrap();

        let tool = TestStatusTool::new(Arc::clone(&store));
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

    /// A passing previous run should NOT flag as compile failure
    /// even when the current run is in progress.
    #[tokio::test]
    async fn running_branch_passing_prior_does_not_flag_compile_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            TestResultStore::open(&dir.path().join("test.db")).unwrap(),
        );

        // Completed run: one passing test, exit_code 0.
        let r1 = store.begin_run().await.unwrap();
        store
            .record_result(r1, TestResultKind::Pass, "demo::ok", None)
            .await
            .unwrap();
        store.finish_run(r1, 0).await.unwrap();

        // In-flight run.
        let _r2 = store.begin_run().await.unwrap();

        let tool = TestStatusTool::new(Arc::clone(&store));
        let out = tool.execute(&json!({}), &ctx()).await.unwrap();
        let v = match out {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };

        let prev = &v["previous_run"];
        assert_eq!(prev["status"], "fresh_passing");
        assert_eq!(prev["pass_count"], 1);
        assert_eq!(prev["exit_code"], 0);
        assert_eq!(
            prev["looks_like_compile_failure"], false,
            "exit 0 with passing tests must never look like a compile failure"
        );
    }
}
