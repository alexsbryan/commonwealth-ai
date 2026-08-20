// SPDX-License-Identifier: AGPL-3.0-or-later
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

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_watchers::TestResultStore;
use corpus_engine_watchers::WatcherHeartbeat;

use super::watcher_health::{
    apply_liveness, assess, read_legacy, watcher_json, WatcherHealthInputs,
};

pub struct TestStatusTool {
    store: Arc<TestResultStore>,
    /// Optional handle to the watcher for live "Running" status.
    /// If None, status is derived entirely from the store.
    running_flag: Option<Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>>,
    /// The command the watcher runs, e.g. "cargo test --workspace". Passed
    /// through to the response so agents can confirm scope coverage.
    watched_scope: Option<String>,
    /// Legacy one-shot liveness bool (pre-heartbeat callers, e.g.
    /// `project serve`). Superseded by `heartbeat`; preferred only when
    /// no heartbeat is wired.
    watcher_active: Option<Arc<AtomicBool>>,
    /// Shared coordinator heartbeat — the authoritative liveness signal.
    /// Lets the tool detect a watcher that died after starting, which the
    /// one-shot bool never could. See [`super::watcher_health`].
    heartbeat: Option<Arc<WatcherHeartbeat>>,
}

impl TestStatusTool {
    pub fn new(store: Arc<TestResultStore>) -> Self {
        Self {
            store,
            running_flag: None,
            watched_scope: None,
            watcher_active: None,
            heartbeat: None,
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

    /// Attach the coordinator heartbeat. Preferred over
    /// [`with_watcher_active`](Self::with_watcher_active) — it can tell a
    /// live watcher from one that started and then died.
    pub fn with_heartbeat(mut self, heartbeat: Arc<WatcherHeartbeat>) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }
}

#[async_trait]
impl Tool for TestStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("test_status").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("test_status")
            .permissions
            .clone()
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
        let legacy_active = read_legacy(&self.watcher_active);
        let configured = self.watched_scope.is_some();

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
                "failures": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": reason.is_live(),
                "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
                "previous_run": previous_run,
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "test_status".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
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
                "failures": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": reason.is_live(),
                "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
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

        // Cross-check liveness. A completed run is only trustworthy if a
        // watcher is actually live to have produced it against the current
        // tree; otherwise demote to `watcher_down` so the caller falls
        // back instead of reading a possibly-ancient run as `fresh_*`.
        let reason = assess(&WatcherHealthInputs {
            heartbeat: self.heartbeat.as_ref(),
            legacy_active,
            configured,
            run_in_progress: false,
            last_run_age_secs: Some(age_seconds),
        });
        let status = apply_liveness(raw_status, reason);

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
            "watcher_active": reason.is_live(),
            "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
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

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_watchers::{TestResultKind, TestResultStore};
    use sovereign_core::types::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "test-status-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
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
        let store = Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap());

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
        let store = Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap());

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

    /// THE regression for this session: a completed failing run sitting
    /// in the store behind a watcher that isn't live (configured, but the
    /// heartbeat never stamped — coordinator never started or died) must
    /// report `watcher_down`, NOT `fresh_failing`. "fresh" behind a dead
    /// watcher is the lie that sent the agent hunting for a compile error
    /// that didn't exist.
    #[tokio::test]
    async fn dead_watcher_demotes_completed_run_to_watcher_down() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap());
        // A completed failing run, like the 3.4-day-old run 3048.
        let r1 = store.begin_run().await.unwrap();
        store
            .record_result(r1, TestResultKind::Fail, "demo::boom", Some("boom"))
            .await
            .unwrap();
        store.finish_run(r1, 101).await.unwrap();

        // Configured (scope set) but the heartbeat never stamped == the
        // watcher coordinator is not alive.
        let hb = WatcherHeartbeat::new();
        let tool = TestStatusTool::new(Arc::clone(&store))
            .with_watched_scope("cargo test --workspace".into())
            .with_heartbeat(hb);

        let v = match tool.execute(&json!({}), &ctx()).await.unwrap() {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };

        assert_eq!(
            v["status"], "watcher_down",
            "a failing run behind a dead watcher must not read fresh_failing"
        );
        assert_eq!(v["watcher"]["live"], false);
        assert_eq!(v["watcher"]["reason"], "watcher_dead");
        assert_eq!(v["watcher_active"], false);
        assert!(v["watcher"]["hint"].is_string());
        // The underlying summary is still available for inspection.
        assert_eq!(v["summary"]["fail_count"], 1);
    }

    /// Same store, but a LIVE heartbeat → the run is trustworthy and the
    /// status is the honest `fresh_failing` (not demoted).
    #[tokio::test]
    async fn live_watcher_keeps_fresh_failing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap());
        let r1 = store.begin_run().await.unwrap();
        store
            .record_result(r1, TestResultKind::Fail, "demo::boom", Some("boom"))
            .await
            .unwrap();
        store.finish_run(r1, 101).await.unwrap();

        let hb = WatcherHeartbeat::new();
        hb.stamp(); // live
        let tool = TestStatusTool::new(Arc::clone(&store))
            .with_watched_scope("cargo test --workspace".into())
            .with_heartbeat(hb);

        let v = match tool.execute(&json!({}), &ctx()).await.unwrap() {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(v["status"], "fresh_failing");
        assert_eq!(v["watcher"]["live"], true);
        assert_eq!(v["watcher"]["reason"], "live");
    }

    /// No runner configured (no scope) → never_run carries an explicit
    /// `not_configured` reason with an actionable hint, instead of a bare
    /// `watcher_active: false`.
    #[tokio::test]
    async fn not_configured_reports_reason_on_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap());
        // Heartbeat wired (daemon mode) but no scope == this tool has no
        // test_runner configured.
        let hb = WatcherHeartbeat::new();
        hb.stamp();
        let tool = TestStatusTool::new(Arc::clone(&store)).with_heartbeat(hb);

        let v = match tool.execute(&json!({}), &ctx()).await.unwrap() {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(v["status"], "never_run");
        assert_eq!(v["watcher"]["reason"], "not_configured");
        assert_eq!(v["watcher"]["configured"], false);
        assert_eq!(v["watcher"]["live"], false);
        assert!(v["watcher"]["hint"].is_string());
    }

    /// A passing previous run should NOT flag as compile failure
    /// even when the current run is in progress.
    #[tokio::test]
    async fn running_branch_passing_prior_does_not_flag_compile_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap());

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
