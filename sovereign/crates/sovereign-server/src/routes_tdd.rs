// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP transport for the unified TDD solver.
//!
//! Single endpoint — `POST /v1/solve` — accepts a wire-shaped
//! `Trial` (workdir path + force + model + prompt + test_command +
//! polarity + optional config) and returns a `TrialResult`. The
//! pre-collapse per-phase endpoints (`/v1/solve/tdd_red`, etc.)
//! were retired 2026-05-24 when the four solvers collapsed into
//! one. Per-task convenience endpoints (split_file etc.) can be
//! added as thin wrappers in a follow-up — they all dispatch to
//! the same `run_trial` call.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;

use serde::Serialize;
use sovereign_tdd::tasks::bdd::{bdd_cycle, BddCycleArgs, ReviewMode};
use sovereign_tdd::{
    run_trial, ChatBackend, DirtyWorkdir, Polarity, Trial, TrialConfig, TrialResult, Workdir,
};

/// Extension state inserted by `main.rs`. Just the backend; the
/// loop is a free function so there's no per-request state to
/// hold.
#[derive(Clone)]
pub struct TddState(pub Arc<dyn ChatBackend>);

pub fn tdd_router() -> Router {
    Router::new()
        .route("/v1/solve", post(solve))
        .route("/v1/cycle/bdd", post(cycle_bdd))
}

#[derive(Debug, Deserialize)]
pub struct TrialWire {
    pub workdir: PathBuf,
    #[serde(default)]
    pub force: bool,
    pub model: String,
    pub prompt: String,
    pub test_command: String,
    pub polarity: PolarityWire,
    #[serde(default)]
    pub config: Option<ConfigWire>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolarityWire {
    MaximizePassing,
    GenerateOneFailing {
        #[serde(default)]
        test_name_hint: Option<String>,
    },
}

impl From<PolarityWire> for Polarity {
    fn from(w: PolarityWire) -> Self {
        match w {
            PolarityWire::MaximizePassing => Polarity::MaximizePassing,
            PolarityWire::GenerateOneFailing { test_name_hint } => {
                Polarity::GenerateOneFailing { test_name_hint }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ConfigWire {
    pub candidates_per_round: Option<usize>,
    pub rounds_per_trial: Option<usize>,
    pub max_stall_rounds: Option<u32>,
    pub emit_max_tokens: Option<u32>,
    pub candidate_test_timeout_seconds: Option<u64>,
    pub temp_ladder_default: Option<Vec<f32>>,
    pub temp_ladder_wide: Option<Vec<f32>>,
}

fn build_config(wire: Option<ConfigWire>) -> TrialConfig {
    let mut c = TrialConfig::default();
    let Some(w) = wire else { return c };
    if let Some(n) = w.candidates_per_round {
        c.candidates_per_round = n;
    }
    if let Some(n) = w.rounds_per_trial {
        c.rounds_per_trial = n;
    }
    if let Some(n) = w.max_stall_rounds {
        c.max_stall_rounds = n;
    }
    if let Some(n) = w.emit_max_tokens {
        c.emit_max_tokens = n;
    }
    if let Some(s) = w.candidate_test_timeout_seconds {
        c.candidate_test_timeout = Duration::from_secs(s);
    }
    if let Some(v) = w.temp_ladder_default {
        c.temp_ladder_default = v;
    }
    if let Some(v) = w.temp_ladder_wide {
        c.temp_ladder_wide = v;
    }
    c
}

async fn solve(
    Extension(state): Extension<TddState>,
    Json(req): Json<TrialWire>,
) -> impl IntoResponse {
    let workdir = match Workdir::check_safe(req.workdir.clone(), req.force) {
        Ok(w) => w,
        Err(e) => {
            let (kind, path) = dirty_payload(&e);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "dirty_workdir",
                    "kind": kind,
                    "path": path,
                    "message": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    let trial = Trial {
        workdir,
        model: req.model,
        prompt: req.prompt,
        test_command: req.test_command,
        polarity: req.polarity.into(),
        config: build_config(req.config),
        syntax_validator: None,
    };
    let result = run_trial(trial, Arc::clone(&state.0)).await;
    (StatusCode::OK, Json(result)).into_response()
}

// ── BDD cycle endpoint ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BddCycleWire {
    pub workdir: PathBuf,
    #[serde(default)]
    pub force: bool,
    pub model: String,
    /// Natural-language description of the behavior the model
    /// should generate a failing test for, then drive to passing.
    pub intent: String,
    #[serde(default)]
    pub test_file_hint: Option<String>,
    #[serde(default)]
    pub task_hint: Option<String>,
    #[serde(default)]
    pub test_command: Option<String>,
    #[serde(default)]
    pub config: Option<ConfigWire>,
    /// "auto" (default) or "pause_after_synthesis".
    #[serde(default)]
    pub review_mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BddCycleResponseWire {
    pub synthesis: TrialResult,
    /// Set when the green stage ran (auto mode + synthesis Reached).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green: Option<TrialResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_test_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_test_content: Option<String>,
}

async fn cycle_bdd(
    Extension(state): Extension<TddState>,
    Json(req): Json<BddCycleWire>,
) -> impl IntoResponse {
    let workdir = match Workdir::check_safe(req.workdir.clone(), req.force) {
        Ok(w) => w,
        Err(e) => {
            let (kind, path) = dirty_payload(&e);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "dirty_workdir",
                    "kind": kind,
                    "path": path,
                    "message": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    let review_mode = match req.review_mode.as_deref() {
        Some("pause_after_synthesis") => ReviewMode::PauseAfterSynthesis,
        Some("auto") | None => ReviewMode::Auto,
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "unknown_review_mode",
                    "value": other,
                    "valid": ["auto", "pause_after_synthesis"],
                })),
            )
                .into_response();
        }
    };
    let args = BddCycleArgs {
        workdir,
        model: req.model,
        intent: req.intent,
        test_file_hint: req.test_file_hint,
        task_hint: req.task_hint,
        test_command: req.test_command,
        config: Some(build_config(req.config)),
        review_mode,
    };
    let r = bdd_cycle(args, Arc::clone(&state.0)).await;
    let wire = BddCycleResponseWire {
        synthesis: r.synthesis,
        green: r.green,
        generated_test_path: r.generated_test_path,
        generated_test_content: r.generated_test_content,
    };
    (StatusCode::OK, Json(wire)).into_response()
}

fn dirty_payload(e: &DirtyWorkdir) -> (&'static str, PathBuf) {
    match e {
        DirtyWorkdir::SystemPath { path } => ("system_path", path.clone()),
        DirtyWorkdir::UncommittedChanges { path } => ("uncommitted_changes", path.clone()),
        DirtyWorkdir::NotAGitRepo { path } => ("not_a_git_repo", path.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sovereign_tdd::DeterministicChatBackend;
    use std::process::Command;
    use tower::ServiceExt;

    fn fresh_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .arg("init")
            .arg("--initial-branch=main")
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "user.email", "t@t.t"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "user.name", "t"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "--allow-empty", "-m", "init"])
            .output();
        tmp
    }

    fn build_app() -> Router {
        let backend: Arc<dyn ChatBackend> =
            Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()));
        tdd_router().layer(Extension(TddState(backend)))
    }

    #[tokio::test]
    async fn dirty_workdir_returns_422_with_kind() {
        let app = build_app();
        let tmp = fresh_repo();
        std::fs::write(tmp.path().join("wip.txt"), "x").unwrap();
        let body = serde_json::json!({
            "workdir": tmp.path(),
            "force": false,
            "model": "x",
            "prompt": "anything",
            "test_command": "pytest -q",
            "polarity": { "kind": "maximize_passing" }
        });
        let req = Request::builder()
            .uri("/v1/solve")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(v["error"], "dirty_workdir");
        assert_eq!(v["kind"], "uncommitted_changes");
    }

    #[tokio::test]
    async fn solve_returns_trial_result_envelope() {
        let app = build_app();
        let tmp = fresh_repo();
        let body = serde_json::json!({
            "workdir": tmp.path(),
            "model": "test",
            "prompt": "make failing tests pass",
            "test_command": "pytest -q",
            "polarity": { "kind": "maximize_passing" }
        });
        let req = Request::builder()
            .uri("/v1/solve")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        // TrialResult shape: status object, tests_before/after, rounds, etc.
        assert!(v.get("status").is_some(), "missing status field: {v}");
        assert!(v.get("tests_before").is_some());
        assert!(v.get("rounds").is_some());
    }

    #[tokio::test]
    async fn cycle_bdd_returns_synthesis_envelope() {
        let app = build_app();
        let tmp = fresh_repo();
        let body = serde_json::json!({
            "workdir": tmp.path(),
            "model": "test",
            "intent": "the cache evicts on size limit",
            "test_command": "pytest -q",
            "review_mode": "pause_after_synthesis"
        });
        let req = Request::builder()
            .uri("/v1/cycle/bdd")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        // BddCycleResponseWire shape: synthesis is always present;
        // green is optional (None when synthesis didn't Reach or pause mode).
        assert!(v.get("synthesis").is_some(), "missing synthesis field: {v}");
        // PauseAfterSynthesis → green absent.
        assert!(v.get("green").is_none(), "pause mode must not run green");
    }

    #[tokio::test]
    async fn cycle_bdd_rejects_unknown_review_mode() {
        let app = build_app();
        let tmp = fresh_repo();
        let body = serde_json::json!({
            "workdir": tmp.path(),
            "model": "test",
            "intent": "x",
            "test_command": "pytest -q",
            "review_mode": "frobnicate"
        });
        let req = Request::builder()
            .uri("/v1/cycle/bdd")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn solve_supports_generate_one_failing_polarity() {
        let app = build_app();
        let tmp = fresh_repo();
        let body = serde_json::json!({
            "workdir": tmp.path(),
            "model": "test",
            "prompt": "write a failing test",
            "test_command": "pytest -q",
            "polarity": { "kind": "generate_one_failing", "test_name_hint": "test_x" }
        });
        let req = Request::builder()
            .uri("/v1/solve")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
