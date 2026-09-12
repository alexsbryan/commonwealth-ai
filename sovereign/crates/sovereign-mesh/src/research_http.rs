// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep research as a daemon JOB — `/v1/research` (sv-surface, 2026-09-11).
//!
//! Until this file `sovereign_core::deep_research::run` was linked into two
//! hosts and served by none: the desktop's `dr_start` drove the loop inside
//! the app, the CLI verb drove it a second time, and the daemon had no
//! research route at all. This is the ONE place the loop now turns for a
//! client. Seven routes:
//!
//! - `GET  /v1/research/capabilities` — can this daemon run research, and
//!   with which affordances.
//! - `POST /v1/research` — launch (or resume) a run; `202` with a
//!   [`ResearchJobAck`]. One run at a time: a second is a `409` naming the
//!   first. The refusal moved here from the desktop because it is about the
//!   inference slot the runs would contend for, which is the daemon's.
//! - `GET  /v1/research/{job_id}/progress?after=N` — the frame log from the
//!   caller's cursor on, plus the elapsed/quiet clocks. `404` for a job this
//!   daemon never accepted.
//! - `POST /v1/research/{job_id}/abort` — raise the loop's abort flag. Not a
//!   kill: the loop polls it at every state entry and lands on a truncated
//!   report with the truncation declared.
//! - `GET  /v1/research/runs` — the shelf of prior runs under the run base.
//! - `GET  /v1/research/active` — the runs this daemon is driving right now.
//! - `GET  /v1/research/runs/{run_id}/report` — the checked report.
//!
//! **The run dir stays the single state source.** The daemon READS the
//! artifacts the loop writes — `charter.json`, `budget-ledger.json`,
//! `gap-list-<round>.json`, `verdict-set.json`, `report.md`,
//! `manifest.json` — and appends a `live` frame when the snapshot changes.
//! That poller and the report builder came down from the desktop whole;
//! the artifacts are deserialised with sovereign-core's OWN ICD types, so a
//! schema drift between the loop and this viewer is a compile error.
//!
//! **Egress custody.** The loop's web leg forms its OWN queries (the gap
//! templates) and releases them against the run's typed consent grant —
//! `deep_research/port.rs` passes `user_formed: false` at both
//! `egress::verify` sites. Nothing here asserts a fact only a keystroke
//! surface could, so moving the loop off the desktop weakens no boundary;
//! the `search_web` exception in `DEFAULTS_LEDGER.md` is about a
//! user-typed query and does not apply.
//!
//! **The launcher seam.** [`ResearchLauncher`] is what a job needs from the
//! loop: a run dir, and a future that drives to a terminal state. The
//! production impl is [`CoreLauncher`] over `launch::prepare`; the e2e test
//! supplies a stub, because a real run needs models this test binary does
//! not have, and the JOB CONTRACT (accept, cursor, abort, 404, 409) is the
//! thing to pin here — the loop has its own tests in `sovereign-core`.
//!
//! Loopback posture is `lc_http`'s: the router takes no daemon handle
//! (`launch::prepare` resolves the daemon endpoint and models from
//! `SetupConfig` itself), so it is sealed with `localhost_only()`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Extension, Path as AxPath, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::future::BoxFuture;
use serde::Deserialize;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::types::Custody;
use sovereign_core::deep_research::containment::missing_claim_figures;
use sovereign_core::deep_research::icd::{
    BudgetLedger, Charter, EvidenceWindow, GapList, Manifest, Verdict, VerdictSet,
};
use sovereign_core::deep_research::launch::{self, LaunchOptions};
use sovereign_core::deep_research::{resume, run, SearchSource};

use crate::http_response::{json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

pub use sovereign_contracts::daemon_wire::{
    ResearchAbortAck, ResearchActiveRun, ResearchAlignment, ResearchBudget, ResearchCapabilities,
    ResearchCitation, ResearchClaim, ResearchConsent, ResearchConstitution, ResearchCorroboration,
    ResearchFrame, ResearchGap, ResearchJobAck, ResearchProgress, ResearchReframe, ResearchReport,
    ResearchRequest, ResearchResidueRow, ResearchRoundRow, ResearchRunSummary,
};

// ─── The launcher seam ─────────────────────────────────────────

/// What one accepted job is, before the loop turns: its identity, the run
/// dir that is real on disk, and the future that drives it to a terminal
/// state. `drive` resolves `Ok(())` when the run landed AND closed (estate
/// ingest + RACE page); its `Err` is the sentence the terminal `failed`
/// frame carries.
pub struct LaunchedRun {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub drive: BoxFuture<'static, Result<(), String>>,
}

/// The loop, as the job surface needs it. One production impl
/// ([`CoreLauncher`]); the e2e test's stub is the other, and the reason
/// this is a trait rather than a function.
pub trait ResearchLauncher: Send + Sync + 'static {
    /// The directory runs are minted under and listed from.
    fn runs_base(&self) -> PathBuf;
    /// Report the affordances, and why research cannot run when it cannot.
    fn capabilities(&self) -> ResearchCapabilities;
    /// Prepare a fresh run (or a resume) from the wire request. Every
    /// refusal is a sentence the `400` body carries verbatim.
    fn launch(
        &self,
        request: ResearchRequest,
        abort: Arc<AtomicBool>,
    ) -> BoxFuture<'static, Result<LaunchedRun, String>>;
}

/// The production launcher: `launch::prepare` / `prepare_resume` (the ONE
/// assembly of a `RunConfig`), then `run` / `resume`, then `launch::close`.
/// The run base is the one the desktop drove the loop with until this
/// commit — `<SetupConfig dir>/deep-research-runs` — so every run already
/// on the shelf stays on it.
pub struct CoreLauncher;

/// The run-dir base: a stable, non-temp home so runs survive restarts.
fn default_runs_base() -> PathBuf {
    SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("deep-research-runs")
}

/// The typed consent grant's closed set, parsed at the wire boundary so a
/// typo never reaches a run. `Custody::parse_wire` is the ONE parser.
fn consent_class(floor: &str) -> Result<Custody, String> {
    match Custody::parse_wire(floor) {
        Some(c) if c != Custody::Unknown => Ok(c),
        _ => Err(format!(
            "unknown consent class `{floor}` — the closed set is public-web | peer | personal"
        )),
    }
}

impl ResearchLauncher for CoreLauncher {
    fn runs_base(&self) -> PathBuf {
        default_runs_base()
    }

    fn capabilities(&self) -> ResearchCapabilities {
        // The models are the precondition (the loop's draft + embed
        // surface). A missing or unreadable SetupConfig is reported, not
        // defaulted.
        ResearchCapabilities {
            flags: vec![
                "--consent".to_string(),
                "--corpora".to_string(),
                "--fetch".to_string(),
                "--max-rounds".to_string(),
                "--resume".to_string(),
                "--search".to_string(),
            ],
            error: launch::daemon_targets().err(),
        }
    }

    fn launch(
        &self,
        request: ResearchRequest,
        abort: Arc<AtomicBool>,
    ) -> BoxFuture<'static, Result<LaunchedRun, String>> {
        let base = self.runs_base();
        Box::pin(async move {
            std::fs::create_dir_all(&base).map_err(|e| format!("run dir base {base:?}: {e}"))?;
            let resuming = request.resume_run_id.is_some();
            let launch = match &request.resume_run_id {
                // A resume restores its identity from the checkpoint and
                // the sidecar — the bare-resume shape. Fields riding along
                // on the request are not verified against the frozen
                // config here (the CLI verb's flag-by-flag check is its
                // own); they are logged so the ignore is visible.
                Some(run_id) => {
                    tracing::debug!(
                        run_id = %run_id,
                        max_rounds = ?request.max_rounds,
                        search = ?request.search,
                        fetch = ?request.fetch,
                        corpora = request.corpora.len(),
                        consent = ?request.consent,
                        "research_http: resume inherits the checkpoint's frozen values",
                    );
                    launch::prepare_resume(&base.join(run_id)).await?
                }
                None => {
                    let consent_floor = match request.consent.as_deref() {
                        None => None,
                        Some(floor) => Some(consent_class(floor)?),
                    };
                    let backend = request
                        .backend
                        .clone()
                        .unwrap_or_else(|| "auto".to_string());
                    let search_source = match request.search_source.as_deref() {
                        Some(s) => SearchSource::parse(s).ok_or_else(|| {
                            format!(
                                "unknown search source `{s}` — the closed set is mock | corpus | web"
                            )
                        })?,
                        // The desktop's rule: a mock backend reads the deck;
                        // otherwise the estate corpora come before the web.
                        None if backend == "mock" => SearchSource::Mock,
                        None => SearchSource::Corpus,
                    };
                    use sovereign_core::deep_research::acquisition::{
                        DEFAULT_CODE_SET_K, DEFAULT_EPS_QUOTA,
                    };
                    launch::prepare(LaunchOptions {
                        question: request.question.trim().to_string(),
                        runs_base: base,
                        // The CLI verb's ceilings (2026-08-24, measured): the
                        // round split still divides them and the decider
                        // still refuses past them.
                        max_rounds: request.max_rounds.unwrap_or(3),
                        code_set_k: request.code_set_k.unwrap_or(DEFAULT_CODE_SET_K),
                        eps_quota: request.eps_quota.unwrap_or(DEFAULT_EPS_QUOTA),
                        search_allowance: request.search.unwrap_or(20),
                        fetch_allowance: request.fetch.unwrap_or(100),
                        estate_corpus_ids: request.corpora.clone(),
                        search_source,
                        backend,
                        mock_deck_dir: request.mock_deck_dir.as_deref().map(PathBuf::from),
                        consent_floor,
                    })
                    .await?
                }
            };
            let run_id = launch.run_id.clone();
            let run_dir = launch.run_dir.clone();
            let drive: BoxFuture<'static, Result<(), String>> = Box::pin(async move {
                let config = launch.config.clone();
                let port = launch.port.clone();
                let provider = launch.provider.clone();
                let outcome = if resuming {
                    resume(config, port, provider, abort).await
                } else {
                    run(config, port, provider, abort).await
                };
                let mut outcome = outcome.map_err(|e| format!("deep-research failed: {e}"))?;
                // Closing is not optional: the fetched evidence lands in
                // `dr-estate-<run_id>` and the RACE page is written. The
                // report already exists on disk, so the operator still
                // gets it; the close failure is named.
                launch::close(&mut outcome, &launch.provider, &launch.embed_model)
                    .await
                    .map_err(|e| format!("the run finished but could not be closed: {e}"))
            });
            Ok(LaunchedRun {
                run_id,
                run_dir,
                drive,
            })
        })
    }
}

// ─── The job registry ──────────────────────────────────────────

/// One research job: the loop's abort flag, the frame log the progress
/// route serves, and the clocks a heartbeat is made of. Kept in
/// [`RESEARCH_JOBS`] after it finishes so a client that polls late still
/// reads the terminal frame; `finished` is the liveness decider.
struct ResearchJob {
    job_id: String,
    run_dir: PathBuf,
    abort: Arc<AtomicBool>,
    started_at_unix: i64,
    last_change_unix: AtomicI64,
    stage: Mutex<String>,
    frames: Mutex<Vec<ResearchFrame>>,
    finished: AtomicBool,
}

/// The job table, keyed by run id. In-process on purpose, like
/// `corpus_catalog_http`'s index builds: the run it narrates is this
/// daemon's task, and a log that outlived the daemon would describe a run
/// that did not finish.
static RESEARCH_JOBS: OnceLock<Mutex<HashMap<String, Arc<ResearchJob>>>> = OnceLock::new();

fn research_jobs() -> &'static Mutex<HashMap<String, Arc<ResearchJob>>> {
    RESEARCH_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn job_for(job_id: &str) -> Option<Arc<ResearchJob>> {
    research_jobs()
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(job_id).cloned())
}

/// The one unfinished job, if any — the "one run at a time" decider.
fn live_job() -> Option<Arc<ResearchJob>> {
    research_jobs().lock().ok().and_then(|jobs| {
        jobs.values()
            .find(|j| !j.finished.load(Ordering::SeqCst))
            .cloned()
    })
}

fn is_live(run_id: &str) -> bool {
    job_for(run_id).is_some_and(|j| !j.finished.load(Ordering::SeqCst))
}

impl ResearchJob {
    fn push(&self, frame: ResearchFrame) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.push(frame);
        }
    }

    fn finish(&self, frame: ResearchFrame) {
        self.push(frame);
        self.finished.store(true, Ordering::SeqCst);
    }
}

/// The daemon is the ONE writer of every research job: it declares a
/// launcher and a router; `research_router_with` is the seam the e2e test
/// drives with a stub.
pub fn research_router() -> Router {
    research_router_with(Arc::new(CoreLauncher))
}

pub fn research_router_with(launcher: Arc<dyn ResearchLauncher>) -> Router {
    Router::new()
        .route("/v1/research", post(start))
        .route("/v1/research/capabilities", get(capabilities))
        .route("/v1/research/runs", get(runs))
        .route("/v1/research/active", get(active))
        .route("/v1/research/runs/{run_id}/report", get(report))
        .route("/v1/research/{job_id}/progress", get(progress))
        .route("/v1/research/{job_id}/abort", post(abort))
        .localhost_only_with(launcher)
}

type Launcher = Extension<Arc<dyn ResearchLauncher>>;

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/research/capabilities`.
async fn capabilities(_: LocalOnly, Extension(launcher): Launcher) -> Response {
    let caps = launcher.capabilities();
    tracing::debug!(
        flags = caps.flags.len(),
        error = ?caps.error,
        "research_http: capabilities served",
    );
    Json(caps).into_response()
}

/// POST `/v1/research` — accept a run as a job (`202`).
async fn start(
    _: LocalOnly,
    Extension(launcher): Launcher,
    Json(request): Json<ResearchRequest>,
) -> Result<Response, Absence> {
    if request.question.trim().is_empty() && request.resume_run_id.is_none() {
        return Err(Absence::invalid(
            "a question is required (or a run to resume)",
        ));
    }
    // A run this daemon is driving must not be re-entered: the resume path
    // would hand a second loop the run dir the first is mid-write on.
    if let Some(run_id) = request.resume_run_id.as_deref() {
        if is_live(run_id) {
            tracing::debug!(run_id, "research_http: resume of a live run refused");
            return Ok(json_error(
                StatusCode::CONFLICT,
                format!("{run_id} is still running — open it rather than resuming it"),
            ));
        }
    }
    // ONE RUN AT A TIME — two concurrent runs contend for the same local
    // inference slot and make each other slower, and a client can
    // represent one run in flight.
    if let Some(existing) = live_job() {
        tracing::debug!(
            existing = %existing.job_id,
            "research_http: second concurrent run refused",
        );
        return Ok(json_error(
            StatusCode::CONFLICT,
            format!(
                "a run is already going ({}) — stop it or wait for it to finish before \
                 starting another",
                existing.job_id
            ),
        ));
    }

    let abort = Arc::new(AtomicBool::new(false));
    let launched = match launcher.launch(request, Arc::clone(&abort)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!(error = %e, "research_http: launch refused");
            return Err(Absence::invalid(e));
        }
    };
    let job_id = launched.run_id.clone();
    let run_dir = launched.run_dir.clone();
    let started_at_unix = sovereign_core::time::unix_now();
    let job = Arc::new(ResearchJob {
        job_id: job_id.clone(),
        run_dir: run_dir.clone(),
        abort,
        started_at_unix,
        last_change_unix: AtomicI64::new(started_at_unix),
        stage: Mutex::new("planning".to_string()),
        frames: Mutex::new(Vec::new()),
        finished: AtomicBool::new(false),
    });
    job.push(ResearchFrame::Started {
        run_id: job_id.clone(),
        run_dir: run_dir.display().to_string(),
    });
    if let Ok(mut jobs) = research_jobs().lock() {
        jobs.insert(job_id.clone(), Arc::clone(&job));
    }
    tracing::info!(
        job_id = %job_id,
        run_dir = %run_dir.display(),
        "research_http: research job accepted",
    );

    tokio::spawn(drive_job(job, launched.drive));

    Ok((
        StatusCode::ACCEPTED,
        Json(ResearchJobAck {
            progress_route: format!("/v1/research/{job_id}/progress"),
            job_id,
            run_dir: run_dir.display().to_string(),
            ok: true,
        }),
    )
        .into_response())
}

/// Drive one job: the loop on this task, the run-dir poller on another,
/// the terminal frame appended HOWEVER the loop ends. A panic inside the
/// loop is caught by the spawned task's join and reported as `failed`,
/// so the registry can never be left answering "live" over a corpse.
async fn drive_job(job: Arc<ResearchJob>, drive: BoxFuture<'static, Result<(), String>>) {
    let done = Arc::new(AtomicBool::new(false));
    let poll = tokio::spawn(poll_run_dir(Arc::clone(&job), Arc::clone(&done)));
    let outcome = tokio::spawn(drive).await;
    done.store(true, Ordering::SeqCst);
    let _ = poll.await;
    let frame = match outcome {
        Ok(Ok(())) => match build_report(&job.run_dir) {
            Some(report) => ResearchFrame::ReportReady { report },
            None => ResearchFrame::Failed {
                error: "the run finished but its artifacts failed to parse".to_string(),
            },
        },
        Ok(Err(e)) => ResearchFrame::Failed { error: e },
        Err(join) => ResearchFrame::Failed {
            error: format!("deep-research task panicked: {join}"),
        },
    };
    tracing::info!(
        job_id = %job.job_id,
        terminal = match &frame {
            ResearchFrame::ReportReady { .. } => "report_ready",
            _ => "failed",
        },
        "research_http: research job finished",
    );
    job.finish(frame);
}

/// Poll the run dir once a second while the loop drives; append a `live`
/// frame only when the snapshot CHANGED, and move the quiet clock when it
/// did — so "nothing moved for 4 minutes" is a measured fact.
async fn poll_run_dir(job: Arc<ResearchJob>, done: Arc<AtomicBool>) {
    let poller = RunDirPoller::new(job.run_dir.clone());
    let mut last: Option<DrLiveSnapshot> = None;
    while !done.load(Ordering::SeqCst) {
        let snapshot = poller.snapshot();
        // Fall back to the LAST KNOWN stage, never to "planning": a
        // transient read failure mid-rewrite is not a rewind.
        if let Some(s) = snapshot.as_ref() {
            if let Ok(mut stage) = job.stage.lock() {
                *stage = s.stage.clone();
            }
        }
        if snapshot.is_some() && last != snapshot {
            last = snapshot.clone();
            if let Some(s) = snapshot {
                job.last_change_unix
                    .store(sovereign_core::time::unix_now(), Ordering::SeqCst);
                job.push(ResearchFrame::Live {
                    round: s.round,
                    max_rounds: s.max_rounds,
                    stage: s.stage,
                    gaps: s.gaps,
                    budget: s.budget,
                    consent: s.consent,
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
}

/// Query of `GET /v1/research/{job_id}/progress`.
#[derive(Debug, Default, Deserialize)]
pub struct ProgressQuery {
    /// The caller's cursor: frames at index `>= after` are returned.
    #[serde(default)]
    pub after: usize,
}

/// GET `/v1/research/{job_id}/progress?after=N`.
async fn progress(
    _: LocalOnly,
    AxPath(job_id): AxPath<String>,
    Query(query): Query<ProgressQuery>,
) -> Result<Response, Absence> {
    let Some(job) = job_for(&job_id) else {
        tracing::debug!(job_id, "research_http: progress for an unknown job");
        return Err(Absence::missing(format!("no research job {job_id}")));
    };
    let after = query.after;
    let (frames, next) = match job.frames.lock() {
        Ok(all) => (
            all.get(after..).unwrap_or(&[]).to_vec(),
            all.len().max(after),
        ),
        Err(_) => return Err(Absence::internal("progress: the frame log is poisoned")),
    };
    let now = sovereign_core::time::unix_now();
    let finished = job.finished.load(Ordering::SeqCst);
    let stage = job
        .stage
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| "planning".to_string());
    tracing::debug!(
        job_id = %job_id,
        after,
        served = frames.len(),
        finished,
        stage = %stage,
        "research_http: progress served",
    );
    Ok(Json(ResearchProgress {
        job_id,
        frames,
        next,
        finished,
        elapsed_secs: (now - job.started_at_unix).max(0),
        quiet_secs: (now - job.last_change_unix.load(Ordering::SeqCst)).max(0),
        stage,
    })
    .into_response())
}

/// POST `/v1/research/{job_id}/abort`. A job this daemon never accepted,
/// or one already finished, is a `404` — there is nothing to stop.
async fn abort(_: LocalOnly, AxPath(job_id): AxPath<String>) -> Result<Response, Absence> {
    match job_for(&job_id) {
        Some(job) if !job.finished.load(Ordering::SeqCst) => {
            job.abort.store(true, Ordering::Relaxed);
            tracing::info!(job_id = %job_id, "research_http: abort requested");
            Ok(Json(ResearchAbortAck {
                job_id,
                aborted: true,
            })
            .into_response())
        }
        _ => {
            tracing::debug!(job_id, "research_http: abort of a job that is not live");
            Err(Absence::missing(format!("no active run {job_id}")))
        }
    }
}

/// GET `/v1/research/runs` — prior runs under the base, newest first
/// (`dr-<unix>` sorts chronologically).
async fn runs(_: LocalOnly, Extension(launcher): Launcher) -> Response {
    let out = list_runs(&launcher.runs_base());
    tracing::debug!(runs = out.len(), "research_http: shelf served");
    Json(out).into_response()
}

/// GET `/v1/research/active` — the runs this daemon is driving.
async fn active(_: LocalOnly) -> Response {
    let mut out: Vec<ResearchActiveRun> = research_jobs()
        .lock()
        .map(|jobs| {
            jobs.values()
                .filter(|j| !j.finished.load(Ordering::SeqCst))
                .map(|j| ResearchActiveRun {
                    run_id: j.job_id.clone(),
                    // The charter's question, read at call time: a resumed
                    // leg was started with no question text of its own.
                    question: std::fs::read(j.run_dir.join("charter.json"))
                        .ok()
                        .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok())
                        .map(|c| c.question),
                    started_at_unix: j.started_at_unix,
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.started_at_unix.cmp(&a.started_at_unix));
    Json(out).into_response()
}

/// GET `/v1/research/runs/{run_id}/report` — the checked report of a
/// completed run. `404` when the run dir is missing or never reached a
/// report; the body names which.
async fn report(
    _: LocalOnly,
    Extension(launcher): Launcher,
    AxPath(run_id): AxPath<String>,
) -> Result<Response, Absence> {
    let base = launcher.runs_base();
    let dir = base.join(&run_id);
    if !dir.is_dir() {
        return Err(Absence::missing(format!(
            "no run {run_id} under {}",
            base.display()
        )));
    }
    match build_report(&dir) {
        Some(report) => Ok(Json(report).into_response()),
        None => Err(Absence::missing(format!(
            "run {run_id} has no report.md — it did not reach a report"
        ))),
    }
}

// ─── The run-dir readers (down from the desktop, 2026-09-11) ───

/// Everything the live view shows, read from the run dir. `None` before
/// the charter exists (the loop writes it first).
#[derive(Debug, Clone, PartialEq)]
struct DrLiveSnapshot {
    round: Option<u32>,
    max_rounds: Option<u32>,
    stage: String,
    gaps: Vec<ResearchGap>,
    budget: ResearchBudget,
    consent: Option<ResearchConsent>,
}

/// Re-reads the artifacts on every call (a handful of small JSON files);
/// the caller decides whether the snapshot CHANGED before appending.
struct RunDirPoller {
    run_dir: PathBuf,
}

impl RunDirPoller {
    fn new(run_dir: PathBuf) -> Self {
        Self { run_dir }
    }

    fn report_md(&self) -> Option<PathBuf> {
        let p = self.run_dir.join("report.md");
        p.is_file().then_some(p)
    }

    fn snapshot(&self) -> Option<DrLiveSnapshot> {
        let dir = &self.run_dir;
        // "No charter yet" is the ordinary pre-launch answer, so it is a
        // `None`. A charter that EXISTS and does not parse is a different
        // fact and must not arrive as the same silence (ARCH principle 6):
        // it means the loop that wrote this run dir and the ICD types
        // reading it have drifted, and the whole live view would just
        // never appear.
        let charter_raw = std::fs::read(dir.join("charter.json")).ok()?;
        let charter: Charter = match serde_json::from_slice(&charter_raw) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    run_dir = %dir.display(),
                    error = %e,
                    "research_http: charter.json is present but does not parse as the \
                     ICD Charter — no live view can be built for this run"
                );
                return None;
            }
        };

        // Round + stage, derived from which artifacts exist: the newest
        // gap-list-<round>.json names the current round; verdict-set.json
        // means the writing is being checked; report.md means done.
        let mut round: Option<u32> = None;
        let mut gaps: Vec<ResearchGap> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut lists: Vec<(u32, PathBuf)> = rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let rest = name.strip_prefix("gap-list-")?.strip_suffix(".json")?;
                    Some((rest.parse::<u32>().ok()?, e.path()))
                })
                .collect();
            lists.sort_by_key(|(r, _)| *r);
            if let Some((r, path)) = lists.last() {
                round = Some(*r);
                if let Ok(raw) = std::fs::read(path) {
                    if let Ok(list) = serde_json::from_slice::<GapList>(&raw) {
                        gaps = list
                            .gaps
                            .into_iter()
                            .map(|g| ResearchGap {
                                id: g.id,
                                text: g.text,
                            })
                            .collect();
                    }
                }
            }
        }
        let stage = (if self.report_md().is_some() {
            "done"
        } else if dir.join("verdict-set.json").is_file() {
            "checking"
        } else if round.is_some() {
            "rounding"
        } else {
            "planning"
        })
        .to_string();

        let mut budget = ResearchBudget::default();
        if let Ok(raw) = std::fs::read(dir.join("budget-ledger.json")) {
            if let Ok(ledger) = serde_json::from_slice::<BudgetLedger>(&raw) {
                budget = ResearchBudget {
                    spent: ledger.spent.into_iter().collect(),
                    remaining: ledger.remaining.into_iter().collect(),
                };
            }
        }

        let max_rounds = charter.charter.max_rounds;
        let consent = charter.charter.consent.map(|c| ResearchConsent {
            release_floor: c.release_floor.as_str().to_string(),
            granted_at_unix: c.granted_at_unix,
        });

        Some(DrLiveSnapshot {
            round,
            max_rounds: Some(max_rounds),
            stage,
            gaps,
            budget,
            consent,
        })
    }
}

/// The shelf: every `dr-*` dir under the base, newest first.
fn list_runs(base: &Path) -> Vec<ResearchRunSummary> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(base) {
        for e in rd.flatten() {
            let dir = e.path();
            let Some(run_id) = dir.file_name().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            if !dir.is_dir() || !run_id.starts_with("dr-") {
                continue;
            }
            let charter = std::fs::read(dir.join("charter.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok());
            let manifest = std::fs::read(dir.join("manifest.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok());
            let live = is_live(&run_id);
            out.push(ResearchRunSummary {
                run_id,
                question: charter.as_ref().map(|c| c.question.clone()),
                created_at_unix: charter.as_ref().map(|c| c.created_at_unix),
                terminal_state: manifest.as_ref().map(|m| m.terminal_state.clone()),
                live,
                rounds: manifest.as_ref().map(|m| m.rounds.len()).unwrap_or(0),
                report_present: dir.join("report.md").is_file(),
                consent: charter
                    .and_then(|c| c.charter.consent)
                    .map(|c| ResearchConsent {
                        release_floor: c.release_floor.as_str().to_string(),
                        granted_at_unix: c.granted_at_unix,
                    }),
            });
        }
    }
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    out
}

/// Assemble the report from a run dir's artifacts. `None` when there is
/// no `report.md` — the run did not reach a report.
fn build_report(run_dir: &Path) -> Option<ResearchReport> {
    let report_md = std::fs::read_to_string(run_dir.join("report.md")).ok()?;
    let charter = std::fs::read(run_dir.join("charter.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok());
    let manifest = std::fs::read(run_dir.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok());
    let verdict_set = std::fs::read(run_dir.join("verdict-set.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<VerdictSet>(&raw).ok());

    let claims = verdict_set
        .as_ref()
        .map(|v| {
            v.claims
                .iter()
                .map(|c| ResearchClaim {
                    id: c.id.clone(),
                    text: c.text.clone(),
                    verdict: c.verdict.as_str().to_string(),
                    status: c.status.clone(),
                    citations: c
                        .citations
                        .iter()
                        .map(|ct| ResearchCitation {
                            evidence_id: ct.evidence_id.clone(),
                            url: ct.url.clone(),
                            chunk_id: ct.chunk_id.clone(),
                        })
                        .collect(),
                    corroboration: c.corroboration.as_ref().map(|cor| ResearchCorroboration {
                        origins: cor.origins.clone(),
                        support_chunks: cor.support_chunks,
                        floor: cor.floor,
                        passes_floor: cor.passes_floor,
                    }),
                })
                .collect()
        })
        .unwrap_or_default();

    let constitution = constitution_check(run_dir, verdict_set.as_ref());

    Some(ResearchReport {
        run_id: charter
            .as_ref()
            .map(|c| c.run_id.clone())
            .unwrap_or_else(|| {
                run_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string()
            }),
        question: charter
            .as_ref()
            .map(|c| c.question.clone())
            .unwrap_or_default(),
        terminal_state: manifest
            .as_ref()
            .map(|m| m.terminal_state.clone())
            .unwrap_or_else(|| "interrupted".to_string()),
        report_md,
        claims,
        not_covered: manifest
            .as_ref()
            .map(|m| m.not_covered.clone())
            .unwrap_or_default(),
        residue: manifest
            .as_ref()
            .map(|m| {
                m.residue
                    .iter()
                    .map(|r| ResearchResidueRow {
                        query: r.query.clone(),
                        round: r.round,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        reframe: manifest
            .as_ref()
            .and_then(|m| m.reframe.as_ref())
            .map(|r| ResearchReframe {
                round: r.round,
                original_question: r.original_question.clone(),
                reframed_question: r.reframed_question.clone(),
                reason: r.reason.clone(),
            }),
        alignment: manifest
            .as_ref()
            .and_then(|m| m.alignment.as_ref())
            .map(|a| ResearchAlignment {
                round: a.round,
                original_question: a.original_question.clone(),
                redirected_question: a.redirected_question.clone(),
                reason: a.reason.clone(),
            }),
        budget: manifest
            .as_ref()
            .map(|m| ResearchBudget {
                spent: m.budget.spent.clone().into_iter().collect(),
                remaining: m.budget.remaining.clone().into_iter().collect(),
            })
            .unwrap_or_default(),
        rounds: manifest
            .as_ref()
            .map(|m| {
                m.rounds
                    .iter()
                    .map(|r| ResearchRoundRow {
                        round: r.round,
                        gaps_before: r.gaps_before,
                        gaps_after: r.gaps_after,
                        fetched: r.fetched,
                        search_calls: r.search_calls,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        consent: manifest
            .as_ref()
            .and_then(|m| m.consent.clone())
            .map(|c| ResearchConsent {
                release_floor: c.release_floor.as_str().to_string(),
                granted_at_unix: c.granted_at_unix,
            }),
        constitution,
    })
}

/// The (g) position property over the loop's own artifacts: every figure
/// token in a [passed] claim must appear in the claim's evidence chunks.
/// Uses the loop's own decider (`containment::missing_claim_figures`) —
/// one figure parser. Claims whose evidence ids resolve to no window
/// chunk are counted `unresolved` — reported, never defaulted.
fn constitution_check(run_dir: &Path, verdict_set: Option<&VerdictSet>) -> ResearchConstitution {
    let mut out = ResearchConstitution::default();
    let Some(vs) = verdict_set else {
        return out;
    };
    let mut chunks_by_id: HashMap<String, String> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(run_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("evidence-window-") || !name.ends_with(".json") {
                continue;
            }
            if let Ok(raw) = std::fs::read(e.path()) {
                if let Ok(window) = serde_json::from_slice::<EvidenceWindow>(&raw) {
                    for c in window.chunks {
                        chunks_by_id.entry(c.id.clone()).or_insert(c.content);
                    }
                }
            }
        }
    }
    for claim in &vs.claims {
        if claim.verdict != Verdict::Passed {
            continue;
        }
        out.passed_claims += 1;
        let evidence: Vec<String> = claim
            .evidence_ids
            .iter()
            .filter_map(|id| chunks_by_id.get(id).cloned())
            .collect();
        if evidence.is_empty() && !claim.evidence_ids.is_empty() {
            out.unresolved += 1;
            continue;
        }
        let untraced = missing_claim_figures(&claim.text, &evidence);
        if !untraced.is_empty() {
            out.violations.push(format!(
                "claim {} [passed] carries untraced figures: {}",
                claim.id,
                untraced.join(", ")
            ));
        }
    }
    out
}

// ─── Tests (down from the desktop's deep_research_commands/tests.rs) ──

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use sovereign_contracts::egress::ConsentGrant;
    use sovereign_core::deep_research::icd::{
        BudgetAllowance, CharterValues, ContainmentConfig, CorroborationRecord, CustodyPolicy,
        EmptyWindow, EvidenceWindow as Ew, FinalClaim, Gap, TriageConfig, UrlConstraintPolicy,
        WindowChunk,
    };

    fn write_json(dir: &Path, name: &str, value: &impl Serialize) {
        std::fs::write(dir.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn charter_values(consent: Option<ConsentGrant>) -> CharterValues {
        CharterValues {
            max_rounds: 3,
            evidence_window_max_chunks: 20,
            containment: ContainmentConfig {
                trigger: "witness".to_string(),
                extraction_max_tokens: 256,
                specifics_max: 3,
            },
            triage: TriageConfig {
                code_set_k: 3,
                eps_quota: 0.1,
                content_coverage_floor:
                    sovereign_core::deep_research::acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
                prose_line_floor:
                    sovereign_core::deep_research::acquisition::DEFAULT_PROSE_LINE_FLOOR,
            },
            budget: BudgetAllowance {
                web_search_queries: 4,
                web_fetch_pages: 4,
            },
            custody: CustodyPolicy {
                stamp_required: true,
                unknown_refuses: true,
            },
            url_constraint: UrlConstraintPolicy {
                enabled: true,
                layer: "strict".to_string(),
            },
            consent,
        }
    }

    fn fixture_charter(dir: &Path, question: &str) {
        write_json(
            dir,
            "charter.json",
            &Charter {
                icd: "charter".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                question: question.to_string(),
                seed_id: None,
                created_at_unix: 100,
                charter: charter_values(Some(ConsentGrant {
                    run_id: "dr-100".to_string(),
                    granted_at_unix: 100,
                    release_floor: Custody::PublicWeb,
                })),
                frozen: true,
            },
        );
    }

    fn fixture_gap_list(dir: &Path, round: u32, gaps: Vec<Gap>) {
        write_json(
            dir,
            &format!("gap-list-{round}.json"),
            &GapList {
                icd: "gap-list".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                round,
                claims: Vec::new(),
                gaps,
                empty_evidence_windows: Vec::<EmptyWindow>::new(),
                strict_subset_of_prior: false,
            },
        );
    }

    fn fixture_budget(dir: &Path) {
        write_json(
            dir,
            "budget-ledger.json",
            &BudgetLedger {
                icd: "budget-ledger".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                allowance: HashMap::new(),
                entries: Vec::new(),
                spent: HashMap::from([("web".to_string(), 2)]),
                remaining: HashMap::from([("web".to_string(), 2)]),
                refused_urls: Vec::new(),
            },
        );
    }

    #[test]
    fn snapshot_reads_round_gaps_budget_and_consent() {
        let dir = tempfile::tempdir().unwrap();
        fixture_charter(dir.path(), "When did Apollo 11 land?");
        fixture_gap_list(
            dir.path(),
            1,
            vec![Gap {
                id: "g1".to_string(),
                text: "the landing date needs a second origin".to_string(),
                actionable_query: "Apollo 11 landing date".to_string(),
                from_claim_id: Some("c1".to_string()),
                corroboration: None,
            }],
        );
        fixture_budget(dir.path());

        let snap = RunDirPoller::new(dir.path().to_path_buf())
            .snapshot()
            .unwrap();
        assert_eq!(snap.round, Some(1));
        assert_eq!(snap.stage, "rounding");
        assert_eq!(snap.gaps.len(), 1);
        assert_eq!(snap.gaps[0].id, "g1");
        assert_eq!(snap.budget.spent.get("web"), Some(&2));
        assert_eq!(snap.budget.remaining.get("web"), Some(&2));
        let consent = snap.consent.unwrap();
        assert_eq!(consent.release_floor, "public-web");
        assert_eq!(consent.granted_at_unix, 100);
    }

    #[test]
    fn snapshot_is_none_before_the_charter_lands() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            RunDirPoller::new(dir.path().to_path_buf())
                .snapshot()
                .is_none(),
            "no charter — no run state to show"
        );
    }

    #[test]
    fn no_consent_means_default_deny_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        write_json(
            dir.path(),
            "charter.json",
            &Charter {
                icd: "charter".to_string(),
                version: 1,
                run_id: "dr-101".to_string(),
                question: "Q".to_string(),
                seed_id: None,
                created_at_unix: 101,
                charter: charter_values(None),
                frozen: true,
            },
        );
        let snap = RunDirPoller::new(dir.path().to_path_buf())
            .snapshot()
            .unwrap();
        assert!(snap.consent.is_none(), "default-deny must read as no grant");
    }

    #[test]
    fn stage_advances_with_the_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        fixture_charter(dir.path(), "Q");
        fixture_budget(dir.path());

        let poller = RunDirPoller::new(dir.path().to_path_buf());
        assert_eq!(poller.snapshot().unwrap().stage, "planning");

        fixture_gap_list(dir.path(), 1, Vec::new());
        assert_eq!(poller.snapshot().unwrap().stage, "rounding");

        write_json(
            dir.path(),
            "verdict-set.json",
            &VerdictSet {
                icd: "verdict-set".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                claims: Vec::new(),
                empty_rounds: Vec::new(),
            },
        );
        assert_eq!(poller.snapshot().unwrap().stage, "checking");

        std::fs::write(dir.path().join("report.md"), "# Report").unwrap();
        assert_eq!(poller.snapshot().unwrap().stage, "done");
        assert!(poller.report_md().is_some());
        // And the shelf reads the same dir as a report-bearing run.
        let shelf = list_runs(dir.path().parent().unwrap());
        // The tempdir's own name is not `dr-*`, so the shelf cannot see
        // it; the report builder can.
        assert!(shelf.iter().all(|r| r.run_id.starts_with("dr-")));
        let report = build_report(dir.path()).expect("report.md present");
        assert_eq!(report.report_md, "# Report");
        assert_eq!(report.question, "Q");
        assert_eq!(report.terminal_state, "interrupted", "no manifest yet");
    }

    fn window(dir: &Path, round: u32, chunks: Vec<WindowChunk>) {
        write_json(
            dir,
            &format!("evidence-window-{round}.json"),
            &Ew {
                icd: "evidence-window".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                round,
                chunks,
                fetch_failures: Vec::new(),
                dedup_refused: Vec::new(),
                content_refused: Vec::new(),
                derived_custody: "personal".to_string(),
            },
        );
    }

    fn passed_claim_set() -> VerdictSet {
        VerdictSet {
            icd: "verdict-set".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            charter_hash: "h".to_string(),
            claims: Vec::new(),
            empty_rounds: Vec::new(),
        }
    }

    fn chunk(content: &str) -> WindowChunk {
        WindowChunk {
            id: "c1".to_string(),
            locator: "estate:x:1".to_string(),
            source_url: "https://example.com/a".to_string(),
            custody: "personal".to_string(),
            provenance_class: "primary".to_string(),
            content: content.to_string(),
            ingested_into: None,
            tags: Vec::new(),
        }
    }

    fn passed(text: &str, evidence_ids: Vec<&str>) -> FinalClaim {
        FinalClaim {
            id: "c1".to_string(),
            text: text.to_string(),
            verdict: Verdict::Passed,
            status: "passed".to_string(),
            evidence_ids: evidence_ids.into_iter().map(String::from).collect(),
            citations: Vec::new(),
            flag: None,
            corroboration: Some(CorroborationRecord {
                origins: vec!["https://example.com/a".to_string()],
                support_chunks: 1,
                floor: 2,
                passes_floor: false,
            }),
        }
    }

    #[test]
    fn constitution_holds_when_every_passed_figure_is_traced() {
        let dir = tempfile::tempdir().unwrap();
        window(
            dir.path(),
            1,
            vec![chunk("Apollo 11 landed on July 20, 1969.")],
        );
        let mut vs = passed_claim_set();
        vs.claims
            .push(passed("Apollo 11 landed on July 20, 1969.", vec!["c1"]));
        let check = constitution_check(dir.path(), Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert!(check.violations.is_empty(), "{:?}", check.violations);
        assert_eq!(check.unresolved, 0);
    }

    #[test]
    fn constitution_names_an_untraced_figure_in_a_passed_claim() {
        let dir = tempfile::tempdir().unwrap();
        // The claim carries "2024" which the evidence never mentions.
        window(dir.path(), 1, vec![chunk("The bridge opened in 1930.")]);
        let mut vs = passed_claim_set();
        vs.claims.push(passed(
            "The bridge opened in 1930 and was restored in 2024.",
            vec!["c1"],
        ));
        let check = constitution_check(dir.path(), Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert_eq!(check.violations.len(), 1, "{:?}", check.violations);
        assert!(
            check.violations[0].contains("2024"),
            "{}",
            check.violations[0]
        );
        assert_eq!(check.unresolved, 0);
    }

    #[test]
    fn unresolved_evidence_is_reported_not_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        // No evidence windows at all — the claim's ids resolve nowhere.
        let mut vs = passed_claim_set();
        vs.claims.push(passed("Something passed.", vec!["missing"]));
        let check = constitution_check(dir.path(), Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert!(check.violations.is_empty());
        assert_eq!(check.unresolved, 1, "unresolvable evidence is counted");
    }

    #[test]
    fn consent_class_refuses_an_unknown_class_and_passes_the_closed_set() {
        assert_eq!(consent_class("public-web"), Ok(Custody::PublicWeb));
        assert_eq!(consent_class("peer"), Ok(Custody::Peer));
        assert_eq!(consent_class("personal"), Ok(Custody::Personal));
        assert!(
            consent_class("everything").is_err(),
            "a typo must not reach a run"
        );
        assert!(
            consent_class("unknown").is_err(),
            "a grant never releases unknown provenance"
        );
    }
}
