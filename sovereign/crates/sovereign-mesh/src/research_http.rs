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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::job_registry::JobRegistry;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};
use crate::research_run_dir::{build_report, list_runs, DrLiveSnapshot, RunDirPoller};

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
static RESEARCH_JOBS: JobRegistry<ResearchJob> = JobRegistry::new("research");

fn job_for(job_id: &str) -> Option<Arc<ResearchJob>> {
    RESEARCH_JOBS.get(job_id).ok().flatten()
}

/// The one unfinished job, if any — the "one run at a time" decider.
fn live_job() -> Option<Arc<ResearchJob>> {
    RESEARCH_JOBS
        .find(|j| !j.finished.load(Ordering::SeqCst))
        .ok()
        .flatten()
}

pub(crate) fn is_live(run_id: &str) -> bool {
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
    RESEARCH_JOBS.insert(job_id.clone(), Arc::clone(&job));
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
    let mut out: Vec<ResearchActiveRun> = RESEARCH_JOBS
        .snapshot()
        .map(|jobs| {
            jobs.iter()
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

#[cfg(test)]
mod tests {
    use super::*;

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
