// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SOLVE surface, code's (moved from the svrn daemon at phase-b
//! pb-meshapp-solve) — give it a coding goal, get a green tree back.
//! Spec: `docs/specs/SOLVE_UX.md`.
//!
//! ```text
//! POST   /v1/solve/jobs            → 202 {job_id, detected}
//! GET    /v1/solve/jobs/{id}       → state + rounds + result
//! GET    /v1/solve/jobs/{id}/events → SSE round/done events
//! DELETE /v1/solve/jobs/{id}       → cancel
//! ```
//!
//! The surface is a thin job host over
//! [`sovereign_tdd::tasks::solve`] — it adds queuing, live round
//! events, and cancellation, and deliberately NO solver behavior.
//! The backend is `/v1/chat/completions` at the base the host hands
//! [`SolveJobs::new`].
//!
//! Everything is in-memory: the job table dies with the host,
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

use sovereign_tdd::tasks::framework::{detect_framework, has_playwright_config, Framework};
use sovereign_tdd::tasks::solve::{solve, SolveArgs, SolveOutcome, SolveRoundObserver, SolveVerb};
use sovereign_tdd::{
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
    /// Land a Reached run: commit the promotion and record it on
    /// `refs/notes/bench` (AVO build order #2, mechanized — a Reached
    /// solve IS correctness + strict improvement held all the way).
    /// Default true; `false` leaves the tree dirty for review.
    #[serde(default = "default_true")]
    pub commit: bool,
}

fn default_true() -> bool {
    true
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
    /// Set when a Reached run landed (commit-on-reached). Absent on
    /// every other outcome, and absent on commit failure — which
    /// never changes the job's own verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitReceipt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitReceipt {
    /// Short sha of the landing commit.
    pub sha: String,
    /// Whether the job record also landed on `refs/notes/bench`.
    pub noted: bool,
}

/// Land a Reached run (AVO build order #2, mechanized): commit the
/// promotion in the workdir and write the job record as a git note on
/// that commit under `refs/notes/bench` — the same P_t store the
/// bench lanes use (`scripts/sovereign-ci-bench.sh::note_lane_score`;
/// lanes key under `lanes` on a shared commit, jobs key under `jobs`
/// on their OWN commit). A Reached solve earned the landing: the tree
/// was clean at submit and every promotion was held to strict
/// improvement against the checker. Non-Reached runs never land here —
/// their dirty tree stays for review, the way it always was. Failure
/// is reported, never fatal: a commit problem must not turn a Reached
/// into something it is not.
fn commit_reached(
    workdir: &std::path::Path,
    job_id: &str,
    goal: &str,
    rounds: u32,
    passed: u32,
    failed: u32,
    model: &str,
) -> Result<CommitReceipt, String> {
    fn git(workdir: &std::path::Path, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(args)
            .output()
            .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(format!(
                "git {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
    let goal_line: String = goal
        .lines()
        .next()
        .unwrap_or(goal)
        .chars()
        .take(72)
        .collect();
    let id8 = &job_id[..job_id.len().min(8)];
    let message = format!(
        "solve: {goal_line} (reached {passed}p/{failed}f in {rounds} rounds, {model}, job {id8})"
    );
    git(workdir, &["add", "-A"])?;
    // --allow-empty keeps a Reached-that-changed-nothing (already
    // green at baseline) from failing the landing; the note still
    // records the run.
    git(workdir, &["commit", "--allow-empty", "-m", &message])?;
    let sha = git(workdir, &["rev-parse", "--short", "HEAD"])?;
    let ts = sovereign_time::unix_now_u64();
    // Per-commit notes are the point: `git log --notes=bench` walks
    // one score per commit and the lineage IS the log — the note on
    // each landing commit carries that commit's job (the way a bench
    // commit's note carries that commit's lane scores). Nothing
    // merges forward; the prior commit keeps its own note.
    let note = serde_json::json!({
        "jobs": {
            job_id: {
                "status": "reached",
                "rounds": rounds,
                "tests_passed": passed,
                "tests_failed": failed,
                "model": model,
                "goal": goal.chars().take(200).collect::<String>(),
                "ts_unix": ts,
                "commit": sha,
            }
        }
    });
    let body = serde_json::to_string(&note).map_err(|e| format!("note serialize: {e}"))?;
    let noted = git(
        workdir,
        &["notes", "--ref=bench", "add", "-f", "-m", &body, "HEAD"],
    )
    .is_ok();
    Ok(CommitReceipt { sha, noted })
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
            created_at_unix: sovereign_time::unix_now_u64(),
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

    pub fn status_json(&self) -> serde_json::Value {
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
    /// The binary on disk was rebuilt after this daemon started — every
    /// verb below would execute stale code. Refused with the repair;
    /// `SOVEREIGN_ALLOW_STALE_SOLVE=1` opts out.
    StaleBinary { exe: String },
}

impl SolveJobs {
    /// `base` is the chat backend's host root (no `/v1`), e.g.
    /// `http://127.0.0.1:9741`.
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into();
        Self {
            jobs: Mutex::new(HashMap::new()),
            backend_url: format!("{}/v1", base.trim_end_matches('/')),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<SolveJob>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    /// Vet the workdir, run submit-time detection, enforce limits,
    /// and spawn the runner. Returns the job (whose `detected` is
    /// the 202 payload) or a refusal.
    pub fn submit(&self, req: SubmitWire) -> Result<Arc<SolveJob>, SubmitError> {
        // Before anything else: is THIS process the code the tree says it
        // is? A rebuild under a live daemon served six 2026-09-02 solve
        // attempts from a pre-dawn binary while every diagnostic on the
        // box pointed at the fresh one. Refuse, name the repair.
        if sovereign_contracts::run_identity::exe_rebuilt_since_start() {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|e| format!("<unreadable: {e}>"));
            return Err(SubmitError::StaleBinary { exe });
        }
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
        let commit = req.commit;
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
            let commit_enabled = commit;
            let SolveOutcome {
                path,
                synthesis,
                result,
                generated_test_path,
                generated_test_content,
            } = solve(args, backend, Some(observer)).await;
            let commit = if commit_enabled && matches!(result.status, TrialStatus::Reached) {
                match commit_reached(
                    &runner_job.workdir,
                    &runner_job.id,
                    &runner_job.goal,
                    result.rounds,
                    result.tests_after.passed,
                    result.tests_after.failed,
                    &runner_job.detected.model,
                ) {
                    Ok(receipt) => Some(receipt),
                    Err(e) => {
                        tracing::warn!(job_id = %runner_job.id, error = %e,
                            "solve: commit-on-reached failed (job verdict unchanged)");
                        None
                    }
                }
            } else {
                None
            };
            runner_job.finish(SolveDone {
                path: path.as_str(),
                result,
                synthesis,
                generated_test_path,
                generated_test_content,
                commit,
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
            SubmitError::StaleBinary { exe } => (
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": "stale_binary",
                    "exe": exe,
                    "message": "the daemon binary was rebuilt after this process started — \
                                its verbs run stale code. Repair: `sovereign daemon restart`. \
                                Opt out (deliberate archaeology only): SOVEREIGN_ALLOW_STALE_SOLVE=1",
                }),
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
            host_kit::shell::guard::loopback_only,
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
mod tests;
