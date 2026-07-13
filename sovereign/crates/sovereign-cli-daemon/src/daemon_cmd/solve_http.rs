// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-hosted SOLVE surface — give the daemon a coding goal,
//! get a green tree back. Spec: `docs/specs/SOLVE_UX.md`.
//!
//! ```text
//! POST   /v1/solve/jobs            → 202 {job_id, detected}
//! GET    /v1/solve/jobs/{id}       → state + rounds + result
//! GET    /v1/solve/jobs/{id}/events → SSE round/done events
//! DELETE /v1/solve/jobs/{id}       → cancel
//! ```
//!
//! The surface is a thin job host over
//! [`commonwealth_tdd::tasks::solve`] — it adds queuing, live round
//! events, and cancellation, and deliberately NO solver behavior.
//! The backend is the daemon's own `/v1/chat/completions` over
//! loopback, so the solver runs against whatever model the daemon
//! is already serving.
//!
//! Everything is in-memory: the job table dies with the daemon,
//! events are ring-buffered per job. Limits: one running job per
//! workdir, [`MAX_RUNNING_JOBS`] global.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use commonwealth_tdd::tasks::framework::{detect_framework, has_playwright_config, Framework};
use commonwealth_tdd::tasks::solve::{
    solve, SolveArgs, SolveOutcome, SolveRoundObserver, SolveVerb,
};
use commonwealth_tdd::{
    ChatBackend, DirtyWorkdir, ReqwestChatBackend, RoundSummary, TrialResult, TrialStatus, Workdir,
};

/// Global cap on concurrently RUNNING jobs. The solver fans out
/// parallel candidates against one local model — two trials already
/// saturate it; more just queue on the model slot and stretch every
/// candidate's wall clock toward its timeout.
pub const MAX_RUNNING_JOBS: usize = 2;
/// Completed/cancelled jobs kept for status queries before eviction.
const FINISHED_JOBS_KEPT: usize = 32;
/// Per-job event ring capacity. Rounds are few (≤ ~15 across all
/// stages) — the cap is protective, not expected to be hit.
const EVENT_RING_CAP: usize = 256;
const EVENT_CHANNEL_CAP: usize = 64;

/// Model alias the daemon serves when the caller doesn't pick one.
const DEFAULT_MODEL: &str = "commonwealth/primary";

// ── wire types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitWire {
    pub workdir: PathBuf,
    /// Plain-language coding goal. With `workdir`, the only
    /// required field.
    pub goal: String,
    /// `fix` / `pin` / `split` — only when the default inference
    /// isn't what you meant.
    #[serde(default)]
    pub verb: Option<String>,
    /// Required with `verb: "split"`.
    #[serde(default)]
    pub max_lines: Option<usize>,
    #[serde(default)]
    pub test_command: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Acknowledge solving on a dirty tree.
    #[serde(default)]
    pub force: bool,
}

/// What submit-time detection found. File-marker based — cheap
/// enough to answer in the 202.
#[derive(Debug, Clone, Serialize)]
pub struct Detected {
    pub framework: &'static str,
    pub test_command: String,
    pub model: String,
    /// Set to "playwright" when a unit framework is the default but
    /// a Playwright config is also present — the caller steers to
    /// the e2e suite explicitly (`--suite e2e` / `test_command`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub also_detected: Option<&'static str>,
}

/// One SSE / ring event. `seq` is per-job monotonic so a client can
/// stitch the replayed ring and the live tail without duplicates.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SolveEvent {
    Round {
        seq: u64,
        /// fix | pin | green | split
        stage: &'static str,
        round: u32,
        /// Winning candidate's `shape@temp`, absent on a stall round.
        winner: Option<String>,
        /// One `shape@temp=outcome` label per candidate — what each
        /// candidate tried and where it landed.
        candidates: Vec<String>,
        passing_after: u32,
        failed_after: u32,
    },
    Done {
        seq: u64,
        /// reached | improved | stalled | exhausted | no_baseline |
        /// errored | cancelled
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Which path the dispatch took, when the run got that far.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<&'static str>,
        rounds: u32,
        tests_passed: u32,
        tests_failed: u32,
    },
}

impl SolveEvent {
    fn seq(&self) -> u64 {
        match self {
            SolveEvent::Round { seq, .. } | SolveEvent::Done { seq, .. } => *seq,
        }
    }
    fn is_done(&self) -> bool {
        matches!(self, SolveEvent::Done { .. })
    }
    fn name(&self) -> &'static str {
        match self {
            SolveEvent::Round { .. } => "round",
            SolveEvent::Done { .. } => "done",
        }
    }
}

/// Final record kept on the job once the run ends.
#[derive(Debug, Clone, Serialize)]
pub struct SolveDone {
    /// fix | pin_then_green | pin | split
    pub path: &'static str,
    pub result: TrialResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<TrialResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_test_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_test_content: Option<String>,
}

#[derive(Debug, Clone)]
enum JobState {
    Running,
    Done(Box<SolveDone>),
    Cancelled,
}

impl JobState {
    fn label(&self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Done(_) => "done",
            JobState::Cancelled => "cancelled",
        }
    }
}

// ── job ─────────────────────────────────────────────────────────────

pub struct SolveJob {
    pub id: String,
    pub workdir: PathBuf,
    pub goal: String,
    pub detected: Detected,
    pub created_at_unix: u64,
    state: Mutex<JobState>,
    events: Mutex<VecDeque<SolveEvent>>,
    next_seq: AtomicU64,
    tx: broadcast::Sender<SolveEvent>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SolveJob {
    fn new(id: String, workdir: PathBuf, goal: String, detected: Detected) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAP);
        Self {
            id,
            workdir,
            goal,
            detected,
            created_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            state: Mutex::new(JobState::Running),
            events: Mutex::new(VecDeque::new()),
            next_seq: AtomicU64::new(1),
            tx,
            handle: Mutex::new(None),
        }
    }

    fn is_running(&self) -> bool {
        matches!(*self.state.lock().unwrap(), JobState::Running)
    }

    /// Append to the ring and fan out to live SSE subscribers. Sync
    /// and cheap — safe to call from the solver's round observer.
    fn push_event(&self, build: impl FnOnce(u64) -> SolveEvent) {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let ev = build(seq);
        {
            let mut ring = self.events.lock().unwrap();
            if ring.len() >= EVENT_RING_CAP {
                ring.pop_front();
            }
            ring.push_back(ev.clone());
        }
        let _ = self.tx.send(ev);
    }

    fn push_round(&self, stage: &'static str, summary: &RoundSummary) {
        let (winner, candidates, round, passing, failed) = (
            summary.winner.clone(),
            summary.candidates.clone(),
            summary.round,
            summary.passing_after,
            summary.failed_after,
        );
        self.push_event(move |seq| SolveEvent::Round {
            seq,
            stage,
            round,
            winner,
            candidates,
            passing_after: passing,
            failed_after: failed,
        });
    }

    fn finish(&self, done: SolveDone) {
        let mut state = self.state.lock().unwrap();
        if !matches!(*state, JobState::Running) {
            return; // cancel won the race
        }
        let (status, reason) = status_label(&done.result.status);
        let (rounds, passed, failed) = (
            done.result.rounds,
            done.result.tests_after.passed,
            done.result.tests_after.failed,
        );
        let path = done.path;
        *state = JobState::Done(Box::new(done));
        drop(state);
        self.push_event(move |seq| SolveEvent::Done {
            seq,
            status,
            reason,
            path: Some(path),
            rounds,
            tests_passed: passed,
            tests_failed: failed,
        });
    }

    fn cancel(&self) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            if !matches!(*state, JobState::Running) {
                return false;
            }
            *state = JobState::Cancelled;
        }
        if let Some(handle) = self.handle.lock().unwrap().take() {
            handle.abort();
        }
        self.push_event(|seq| SolveEvent::Done {
            seq,
            status: "cancelled",
            reason: None,
            path: None,
            rounds: 0,
            tests_passed: 0,
            tests_failed: 0,
        });
        true
    }

    pub(super) fn status_json(&self) -> serde_json::Value {
        let state = self.state.lock().unwrap().clone();
        let rounds: Vec<SolveEvent> = self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !e.is_done())
            .cloned()
            .collect();
        let mut v = serde_json::json!({
            "job_id": self.id,
            "workdir": self.workdir,
            "goal": self.goal,
            "detected": self.detected,
            "state": state.label(),
            "rounds": rounds,
        });
        if let JobState::Done(done) = state {
            v["result"] = serde_json::to_value(&*done).unwrap_or_default();
        }
        v
    }
}

fn status_label(s: &TrialStatus) -> (&'static str, Option<String>) {
    match s {
        TrialStatus::Reached => ("reached", None),
        TrialStatus::Improved => ("improved", None),
        TrialStatus::Stalled {
            rounds_without_improvement,
        } => (
            "stalled",
            Some(format!(
                "{rounds_without_improvement} rounds without improvement"
            )),
        ),
        TrialStatus::Exhausted { rounds } => {
            ("exhausted", Some(format!("round budget spent ({rounds})")))
        }
        TrialStatus::NoBaseline { reason } => ("no_baseline", Some(reason.clone())),
        TrialStatus::Errored { reason } => ("errored", Some(reason.clone())),
    }
}

fn framework_label(f: Framework) -> &'static str {
    match f {
        Framework::Pytest => "pytest",
        Framework::Cargo => "cargo",
        Framework::Vitest => "vitest",
        Framework::Jest => "jest",
        Framework::GoTest => "go-test",
        Framework::Playwright => "playwright",
    }
}

// ── job table ───────────────────────────────────────────────────────

pub struct SolveJobs {
    jobs: Mutex<HashMap<String, Arc<SolveJob>>>,
    /// Base URL of the daemon's own OpenAI-compatible surface,
    /// e.g. `http://127.0.0.1:9741/v1`.
    backend_url: String,
}

/// Submit-time refusals, mapped to HTTP statuses by the handler and
/// to error strings by the MCP tools.
pub enum SubmitError {
    /// §7.1 gate refusal — dirty tree, system path, not a git repo.
    DirtyWorkdir(DirtyWorkdir),
    /// The workdir doesn't resolve on disk.
    BadWorkdir(String),
    /// A running job already owns this workdir.
    WorkdirBusy { job_id: String },
    /// MAX_RUNNING_JOBS reached.
    Capacity { running: usize },
    /// Unknown verb, or `split` without `max_lines`.
    BadRequest(String),
}

impl SolveJobs {
    pub fn new(client_port: u16) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            backend_url: format!("http://127.0.0.1:{client_port}/v1"),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<SolveJob>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    /// Vet the workdir, run submit-time detection, enforce limits,
    /// and spawn the runner. Returns the job (whose `detected` is
    /// the 202 payload) or a refusal.
    pub fn submit(&self, req: SubmitWire) -> Result<Arc<SolveJob>, SubmitError> {
        let canonical = std::fs::canonicalize(&req.workdir)
            .map_err(|e| SubmitError::BadWorkdir(format!("{}: {e}", req.workdir.display())))?;
        let vetted =
            Workdir::check_safe(canonical.clone(), req.force).map_err(SubmitError::DirtyWorkdir)?;
        let verb = parse_verb(req.verb.as_deref(), req.max_lines)?;

        let framework = detect_framework(&canonical);
        let detected = Detected {
            framework: framework_label(framework),
            test_command: req
                .test_command
                .clone()
                .unwrap_or_else(|| framework.default_test_command().to_string()),
            model: req.model.clone().unwrap_or_else(|| DEFAULT_MODEL.into()),
            also_detected: (framework != Framework::Playwright
                && has_playwright_config(&canonical))
            .then_some("playwright"),
        };

        let job = {
            let mut jobs = self.jobs.lock().unwrap();
            let running: Vec<&Arc<SolveJob>> = jobs.values().filter(|j| j.is_running()).collect();
            if let Some(owner) = running.iter().find(|j| j.workdir == canonical) {
                return Err(SubmitError::WorkdirBusy {
                    job_id: owner.id.clone(),
                });
            }
            if running.len() >= MAX_RUNNING_JOBS {
                return Err(SubmitError::Capacity {
                    running: running.len(),
                });
            }
            drop(running);
            evict_finished(&mut jobs);
            let job = Arc::new(SolveJob::new(
                uuid::Uuid::new_v4().to_string(),
                canonical,
                req.goal.clone(),
                detected,
            ));
            jobs.insert(job.id.clone(), Arc::clone(&job));
            job
        };

        let backend: Arc<dyn ChatBackend> =
            Arc::new(ReqwestChatBackend::new(self.backend_url.clone()));
        let runner_job = Arc::clone(&job);
        let args = SolveArgs {
            workdir: vetted,
            model: job.detected.model.clone(),
            goal: req.goal,
            verb,
            test_command: Some(job.detected.test_command.clone()),
            config: None,
        };
        let handle = tokio::spawn(async move {
            let observer_job = Arc::clone(&runner_job);
            let observer: SolveRoundObserver = Arc::new(move |stage, summary: &RoundSummary| {
                observer_job.push_round(stage.as_str(), summary);
            });
            let SolveOutcome {
                path,
                synthesis,
                result,
                generated_test_path,
                generated_test_content,
            } = solve(args, backend, Some(observer)).await;
            runner_job.finish(SolveDone {
                path: path.as_str(),
                result,
                synthesis,
                generated_test_path,
                generated_test_content,
            });
        });
        *job.handle.lock().unwrap() = Some(handle);
        tracing::info!(job_id = %job.id, workdir = %job.workdir.display(), "solve: job started");
        Ok(job)
    }

    pub fn cancel(&self, id: &str) -> Option<bool> {
        let job = self.get(id)?;
        Some(job.cancel())
    }
}

fn parse_verb(
    verb: Option<&str>,
    max_lines: Option<usize>,
) -> Result<Option<SolveVerb>, SubmitError> {
    match verb {
        None => Ok(None),
        Some("fix") => Ok(Some(SolveVerb::Fix)),
        Some("pin") => Ok(Some(SolveVerb::Pin)),
        Some("split") => {
            let max_lines = max_lines.ok_or_else(|| {
                SubmitError::BadRequest("verb \"split\" requires max_lines".into())
            })?;
            Ok(Some(SolveVerb::Split { max_lines }))
        }
        Some(other) => Err(SubmitError::BadRequest(format!(
            "unknown verb {other:?} — valid: fix, pin, split"
        ))),
    }
}

/// Keep the table bounded: running jobs always stay; the most
/// recent [`FINISHED_JOBS_KEPT`] finished jobs stay for status
/// queries; older finished jobs go.
fn evict_finished(jobs: &mut HashMap<String, Arc<SolveJob>>) {
    let mut finished: Vec<(u64, String)> = jobs
        .values()
        .filter(|j| !j.is_running())
        .map(|j| (j.created_at_unix, j.id.clone()))
        .collect();
    if finished.len() <= FINISHED_JOBS_KEPT {
        return;
    }
    finished.sort(); // oldest first
    for (_, id) in finished
        .into_iter()
        .take(jobs.len().saturating_sub(FINISHED_JOBS_KEPT))
    {
        jobs.remove(&id);
    }
}

impl SubmitError {
    fn into_response(self) -> Response {
        let (code, body) = self.payload();
        (code, Json(body)).into_response()
    }

    pub fn payload(&self) -> (StatusCode, serde_json::Value) {
        match self {
            SubmitError::DirtyWorkdir(e) => {
                let (kind, path) = match e {
                    DirtyWorkdir::SystemPath { path } => ("system_path", path),
                    DirtyWorkdir::UncommittedChanges { path } => ("uncommitted_changes", path),
                    DirtyWorkdir::NotAGitRepo { path } => ("not_a_git_repo", path),
                };
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    serde_json::json!({
                        "error": "dirty_workdir",
                        "kind": kind,
                        "path": path,
                        "message": e.to_string(),
                    }),
                )
            }
            SubmitError::BadWorkdir(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                serde_json::json!({ "error": "bad_workdir", "message": msg }),
            ),
            SubmitError::WorkdirBusy { job_id } => (
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": "workdir_busy",
                    "job_id": job_id,
                    "message": "a running solve job already owns this workdir",
                }),
            ),
            SubmitError::Capacity { running } => (
                StatusCode::TOO_MANY_REQUESTS,
                serde_json::json!({
                    "error": "at_capacity",
                    "running": running,
                    "message": format!("{running} jobs running (max {MAX_RUNNING_JOBS}) — retry when one finishes"),
                }),
            ),
            SubmitError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                serde_json::json!({ "error": "bad_request", "message": msg }),
            ),
        }
    }
}

// ── router ──────────────────────────────────────────────────────────

pub fn solve_router(jobs: Arc<SolveJobs>) -> Router {
    Router::new()
        .route("/v1/solve/jobs", post(submit))
        .route("/v1/solve/jobs/{id}", get(status).delete(cancel))
        .route("/v1/solve/jobs/{id}/events", get(events))
        // The solver executes the workdir's test command — this
        // surface must never be reachable off-box.
        .layer(axum::middleware::from_fn(
            sovereign_mesh::loopback_guard::loopback_only,
        ))
        .layer(Extension(jobs))
}

async fn submit(
    Extension(jobs): Extension<Arc<SolveJobs>>,
    Json(req): Json<SubmitWire>,
) -> Response {
    match jobs.submit(req) {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "job_id": job.id,
                "detected": job.detected,
            })),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

async fn status(Extension(jobs): Extension<Arc<SolveJobs>>, Path(id): Path<String>) -> Response {
    match jobs.get(&id) {
        Some(job) => (StatusCode::OK, Json(job.status_json())).into_response(),
        None => not_found(&id),
    }
}

async fn cancel(Extension(jobs): Extension<Arc<SolveJobs>>, Path(id): Path<String>) -> Response {
    match jobs.cancel(&id) {
        Some(true) => (
            StatusCode::OK,
            Json(serde_json::json!({ "job_id": id, "state": "cancelled" })),
        )
            .into_response(),
        Some(false) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "not_running",
                "message": "job already finished",
            })),
        )
            .into_response(),
        None => not_found(&id),
    }
}

/// SSE: replay the ring, then the live tail, ending after `done`.
/// Subscribe-then-snapshot plus seq dedup closes the race between
/// the two.
async fn events(Extension(jobs): Extension<Arc<SolveJobs>>, Path(id): Path<String>) -> Response {
    let Some(job) = jobs.get(&id) else {
        return not_found(&id);
    };
    let rx = job.tx.subscribe();
    let replay: VecDeque<SolveEvent> = job.events.lock().unwrap().clone();

    struct SseState {
        replay: VecDeque<SolveEvent>,
        rx: broadcast::Receiver<SolveEvent>,
        last_seq: u64,
        finished: bool,
    }
    let state = SseState {
        replay,
        rx,
        last_seq: 0,
        finished: false,
    };
    let stream = futures::stream::unfold(state, |mut st| async move {
        if st.finished {
            return None;
        }
        if let Some(ev) = st.replay.pop_front() {
            st.last_seq = ev.seq();
            st.finished = ev.is_done();
            return Some((Ok::<Event, std::convert::Infallible>(sse_event(&ev)), st));
        }
        loop {
            match st.rx.recv().await {
                Ok(ev) => {
                    if ev.seq() <= st.last_seq {
                        continue; // already replayed from the ring
                    }
                    st.last_seq = ev.seq();
                    st.finished = ev.is_done();
                    return Some((Ok(sse_event(&ev)), st));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_event(ev: &SolveEvent) -> Event {
    Event::default()
        .event(ev.name())
        .data(serde_json::to_string(ev).unwrap_or_else(|_| "{}".into()))
}

fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "no_such_job", "job_id": id })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::net::SocketAddr;
    use std::process::Command;
    use tower::ServiceExt;

    fn fresh_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "t@t.t"],
            vec!["config", "user.name", "t"],
            vec!["commit", "--allow-empty", "-m", "init"],
        ] {
            let _ = Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(&args)
                .output();
        }
        tmp
    }

    /// Router with ConnectInfo pre-injected as loopback, mirroring
    /// what `into_make_service_with_connect_info` provides live.
    fn app(jobs: Arc<SolveJobs>) -> Router {
        let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
        solve_router(jobs).layer(Extension(axum::extract::ConnectInfo(loopback)))
    }

    fn submit_body(workdir: &std::path::Path) -> String {
        serde_json::json!({
            "workdir": workdir,
            "goal": "add an is_palindrome function to utils.py",
        })
        .to_string()
    }

    async fn post_submit(app: &Router, body: String) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .uri("/v1/solve/jobs")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn submit_returns_202_with_detected() {
        let jobs = Arc::new(SolveJobs::new(1)); // port 1: backend unreachable, job just errors in background
        let app = app(Arc::clone(&jobs));
        let repo = fresh_repo();
        std::fs::write(repo.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        // Commit it — a dirty tree is (correctly) refused at submit.
        for args in [vec!["add", "-A"], vec!["commit", "-m", "manifest"]] {
            let _ = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(&args)
                .output();
        }
        let (status, v) = post_submit(&app, submit_body(repo.path())).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        assert!(v["job_id"].as_str().is_some());
        assert_eq!(v["detected"]["framework"], "cargo");
        assert_eq!(v["detected"]["test_command"], "cargo test --quiet");
        assert_eq!(v["detected"]["model"], DEFAULT_MODEL);
        // Cleanly stop the background runner before the tempdir drops.
        let id = v["job_id"].as_str().unwrap();
        jobs.cancel(id);
    }

    #[tokio::test]
    async fn dirty_workdir_is_refused_422() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(jobs);
        let repo = fresh_repo();
        std::fs::write(repo.path().join("wip.txt"), "x").unwrap();
        let (status, v) = post_submit(&app, submit_body(repo.path())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
        assert_eq!(v["error"], "dirty_workdir");
        assert_eq!(v["kind"], "uncommitted_changes");
    }

    #[tokio::test]
    async fn one_job_per_workdir_conflicts_409() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(Arc::clone(&jobs));
        let repo = fresh_repo();
        let (s1, v1) = post_submit(&app, submit_body(repo.path())).await;
        assert_eq!(s1, StatusCode::ACCEPTED);
        let (s2, v2) = post_submit(&app, submit_body(repo.path())).await;
        assert_eq!(s2, StatusCode::CONFLICT, "{v2}");
        assert_eq!(v2["error"], "workdir_busy");
        assert_eq!(v2["job_id"], v1["job_id"]);
        jobs.cancel(v1["job_id"].as_str().unwrap());
    }

    #[tokio::test]
    async fn global_capacity_enforced_429() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(Arc::clone(&jobs));
        let repos: Vec<_> = (0..3).map(|_| fresh_repo()).collect();
        let mut ids = Vec::new();
        for repo in repos.iter().take(MAX_RUNNING_JOBS) {
            let (s, v) = post_submit(&app, submit_body(repo.path())).await;
            assert_eq!(s, StatusCode::ACCEPTED);
            ids.push(v["job_id"].as_str().unwrap().to_string());
        }
        let (s, v) = post_submit(&app, submit_body(repos[MAX_RUNNING_JOBS].path())).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
        assert_eq!(v["error"], "at_capacity");
        for id in ids {
            jobs.cancel(&id);
        }
    }

    #[tokio::test]
    async fn cancel_flips_state_and_emits_done_event() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(Arc::clone(&jobs));
        let repo = fresh_repo();
        let (_, v) = post_submit(&app, submit_body(repo.path())).await;
        let id = v["job_id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .uri(format!("/v1/solve/jobs/{id}"))
            .method("DELETE")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let job = jobs.get(&id).unwrap();
        assert_eq!(job.state.lock().unwrap().label(), "cancelled");
        // Block-scoped: the ring guard must not span the second-cancel
        // await below (clippy::await_holding_lock is scope-based).
        {
            let ring = job.events.lock().unwrap();
            assert!(
                ring.iter().any(|e| e.is_done()),
                "cancel must emit a done event"
            );
        }

        // Second cancel: already finished.
        let req = Request::builder()
            .uri(format!("/v1/solve/jobs/{id}"))
            .method("DELETE")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn status_reports_state_and_rounds() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(Arc::clone(&jobs));
        let repo = fresh_repo();
        let (_, v) = post_submit(&app, submit_body(repo.path())).await;
        let id = v["job_id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .uri(format!("/v1/solve/jobs/{id}"))
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["job_id"], id);
        assert!(v["rounds"].is_array());
        assert!(v["detected"]["framework"].as_str().is_some());
        jobs.cancel(&id);
    }

    #[tokio::test]
    async fn unknown_job_404s() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(jobs);
        let req = Request::builder()
            .uri("/v1/solve/jobs/nope")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn split_verb_requires_max_lines() {
        let jobs = Arc::new(SolveJobs::new(1));
        let app = app(jobs);
        let repo = fresh_repo();
        let body = serde_json::json!({
            "workdir": repo.path(),
            "goal": "split the big file",
            "verb": "split",
        })
        .to_string();
        let (status, v) = post_submit(&app, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    }

    #[tokio::test]
    async fn non_loopback_callers_are_rejected() {
        let jobs = Arc::new(SolveJobs::new(1));
        let remote: SocketAddr = "10.0.0.7:1234".parse().unwrap();
        let app = solve_router(jobs).layer(Extension(axum::extract::ConnectInfo(remote)));
        let req = Request::builder()
            .uri("/v1/solve/jobs/nope")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
