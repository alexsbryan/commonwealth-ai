// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep-research scene 1 driver (order deep-research-t3b) — a thin client
//! over the daemon's `/v1/research` job surface since 2026-09-11.
//!
//! The desktop is a CLIENT of the loop, not a host of it. Until this
//! commit `dr_start` linked the loop's `run`/`resume` out of `sovereign-core`
//! and drove it inside the app — a second in-process copy of a
//! long-running model loop beside the CLI verb's, with the daemon serving
//! no research route. The loop turns on the daemon now
//! (`sovereign-mesh/src/research_http.rs`), which also owns the run-dir
//! readers this module carried (`live.rs`, `report.rs`, `runs.rs` — moved
//! down whole). What is left here:
//!
//! - `dr_start` → `POST /v1/research`, then a poll loop that re-emits the
//!   daemon's frames on the job-scoped Tauri channel — the same `kind`-
//!   tagged payloads the Svelte store already reads, because the frame
//!   enum IS the contracts type (`ResearchFrame`), not a second spelling.
//!   The `heartbeat` is synthesised each tick from the progress answer's
//!   clocks, as the in-process poller synthesised it from its own.
//! - `dr_abort` → `POST /v1/research/{job}/abort`.
//! - `dr_capabilities`, `dr_list_runs`, `dr_active_runs`, `dr_open_report`
//!   → the matching GETs.
//!
//! The demo's `SOVEREIGN_DEMO_DR_FLAGS` override stays HERE: the demo's
//! global-setup sets it in the app's environment, so the app is the
//! process that can read it; it lands on the wire as `backend` +
//! `mock_deck_dir`, which the daemon's launcher already accepts for the
//! CLI's sake.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use sovereign_contracts::daemon_wire::{
    ResearchAbortAck, ResearchActiveRun, ResearchCapabilities, ResearchFrame, ResearchJobAck,
    ResearchProgress, ResearchReport, ResearchRequest, ResearchRunSummary,
};
use sovereign_turn_client::TurnClient;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

#[cfg(test)]
mod tests;

fn research_client(state: &AppState) -> TurnClient {
    TurnClient::new(state.client_base_url())
}

// ── Capabilities ───────────────────────────────────────────────────────────

/// What this install can do. `cli_path` is retained for the UI's shape and
/// is always `None`: deep research is neither a binary to find nor, now, a
/// loop linked into this app — it is the daemon's job surface. `flags`
/// and `error` are the daemon's answer verbatim.
#[derive(Debug, Serialize, Clone)]
pub struct DrCapabilities {
    pub cli_path: Option<String>,
    pub flags: Vec<String>,
    /// Why the feature is unavailable, when it is — the daemon's sentence
    /// (no models configured), or this client's (daemon unreachable).
    /// Absence is reported, never defaulted.
    pub error: Option<String>,
}

#[tauri::command]
pub async fn dr_capabilities(state: State<'_, Arc<AppState>>) -> Result<DrCapabilities, String> {
    Ok(
        match research_client(&state)
            .research_capabilities::<ResearchCapabilities>()
            .await
        {
            Ok(caps) => DrCapabilities {
                cli_path: None,
                flags: caps.flags,
                error: caps.error,
            },
            Err(e) => DrCapabilities {
                cli_path: None,
                flags: Vec::new(),
                error: Some(format!("dr_capabilities: {e}")),
            },
        },
    )
}

// ── Run lifecycle ──────────────────────────────────────────────────────────

fn progress_channel(job_id: &str) -> String {
    format!("deep-research://progress/{job_id}")
}

/// The launch surface — mirrors `WorkflowRunHandle`'s shape (job-scoped
/// channel; the UI listens for `ResearchFrame` events on it).
#[derive(Debug, Serialize, Clone)]
pub struct DrRunHandle {
    pub job_id: String,
    pub channel: String,
}

/// What the operator typed at the Ask entry: the question, the budget, and
/// the typed consent grant (default-deny — `consent: None` sends no class
/// and the daemon's web leg refuses non-public-web payloads).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DrStartOptions {
    pub max_rounds: Option<u32>,
    /// Estate corpus ids the run may consult first (a prior run's estate
    /// corpus is selectable here).
    pub corpora: Vec<String>,
    /// `"public-web"` | `"peer"` | `"personal"` — the typed release floor.
    /// Absent = default-deny.
    pub consent: Option<String>,
    pub search: Option<u32>,
    pub fetch: Option<u32>,
    /// t3a's resume surface.
    pub resume_run_id: Option<String>,
}

/// Demo-only backend override (order deep-research-t3b, evidence pass
/// (f)): `SOVEREIGN_DEMO_DR_FLAGS` carries `--backend mock --mock-deck
/// DIR` so the recorded pass films a deterministic deck run while the Ask
/// surface stays spec-faithful (question + budget + consent only). Unset
/// in every real flow — the demo's global-setup is the only writer.
///
/// It stays spelled as flags because the demo's global-setup and the
/// env-flag registry already name it that way; it lands in typed request
/// fields. Anything the closed set does not name is IGNORED, not passed
/// through: an unrecognised token has no meaning on the wire.
fn demo_backend_override() -> Option<(String, Option<PathBuf>)> {
    let raw = std::env::var("SOVEREIGN_DEMO_DR_FLAGS").ok()?;
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let mut backend: Option<String> = None;
    let mut deck: Option<PathBuf> = None;
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            "--backend" => {
                backend = toks.get(i + 1).map(|s| s.to_string());
                i += 2;
            }
            "--mock-deck" => {
                deck = toks.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            _ => i += 1,
        }
    }
    backend.map(|b| (b, deck))
}

/// The jobs this app is polling right now, keyed by run id. The daemon's
/// job table is the decider for "is the run alive" (`dr_list_runs`,
/// `dr_active_runs` and `dr_abort` all read it over the wire); this set
/// answers the one question that must be answered synchronously and
/// locally — the window's close handler asking [`has_live_run`].
static POLLING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn polling() -> &'static Mutex<HashSet<String>> {
    POLLING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The polling set, with a poisoned lock RECOVERED rather than panicked
/// on or skipped — ONE treatment for one lock (ARCH principle 8). This
/// file had three: `expect` at the read the close handler gates on,
/// `if let Ok(..)` at the cleanup, and `unwrap_or(0)` in a trace field.
/// The `if let Ok` was the one that mattered: `poll_job` documents that
/// it clears the set HOWEVER the loop ends, so the close handler can
/// never be left blocking quit over a run nobody is watching — and a
/// poisoned lock made it skip the clear and strand exactly that state.
/// A poison here means some other holder panicked; the set's contents
/// are a `HashSet<String>` that cannot be logically torn, so recovering
/// it is right and refusing to would trade a real bug for a worse one.
fn polling_set() -> std::sync::MutexGuard<'static, HashSet<String>> {
    polling()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The event the window's close handler emits when it refuses to quit
/// because research is in flight. The frontend owns the conversation that
/// follows; the backend only declines to disappear silently.
pub const QUIT_BLOCKED_EVENT: &str = "deep-research://quit-blocked";

/// Is this app attached to a run in flight? Read by the window's
/// `CloseRequested` handler. The run itself now survives the app — it is
/// the daemon's task — so what closing loses is the LISTENER, not the
/// run; the handler's refusal keeps the operator from losing sight of a
/// run they started, and `dr_active_runs` re-attaches after a relaunch.
pub fn has_live_run() -> bool {
    !polling_set().is_empty()
}

/// Consecutive progress-poll failures before the client gives up on the
/// daemon and reports the loss. Ten one-second ticks: long enough to ride
/// out a daemon restart's bind window, short enough that a dead daemon is
/// named within the heartbeat's own stale window on the Svelte side.
const POLL_FAILURES_TOLERATED: u32 = 10;

/// Start a deep-research run as a daemon job. Returns as soon as the
/// daemon acked — its `run_dir` is real on disk by then — and polls the
/// job's frame log on a background task, re-emitting every frame on the
/// job-scoped channel plus a `heartbeat` per tick.
///
/// The `job_id` is the RUN id (`dr-<unix>`), the daemon's own.
#[tauri::command]
pub async fn dr_start(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    question: String,
    options: DrStartOptions,
) -> Result<DrRunHandle, String> {
    // The demo's deterministic deck, when the demo set it; otherwise the
    // daemon's launcher decides the backend (`auto`) and search source.
    let (backend, mock_deck_dir) = match demo_backend_override() {
        Some((b, deck)) => (Some(b), deck.map(|d| d.display().to_string())),
        None => (None, None),
    };
    let request = ResearchRequest {
        question: question.trim().to_string(),
        max_rounds: options.max_rounds,
        corpora: options.corpora,
        consent: options.consent,
        search: options.search,
        fetch: options.fetch,
        resume_run_id: options.resume_run_id,
        backend,
        mock_deck_dir,
        ..Default::default()
    };
    let client = research_client(&state);
    // The daemon refuses an empty question, an unknown consent class, a
    // resume of a live run and a second concurrent run — each by name.
    // Its sentence is the operator's error, not a re-spelling of it.
    let ack: ResearchJobAck = client
        .research_start(&request)
        .await
        .map_err(|e| format!("dr_start: {e}"))?;
    let job_id = ack.job_id.clone();
    let channel = progress_channel(&job_id);
    polling_set().insert(job_id.clone());
    tracing::debug!(
        job_id = %job_id,
        run_dir = %ack.run_dir,
        "deep-research: job accepted by the daemon; polling",
    );

    tokio::spawn(poll_job(app, client, job_id.clone(), channel.clone()));

    Ok(DrRunHandle { job_id, channel })
}

/// Re-emit the daemon's frame log on the channel, one poll a second,
/// until the terminal frame — plus a `heartbeat` per tick, changed or
/// not, from the daemon's clocks. Clears the polling set HOWEVER the
/// loop ends (a lost daemon included), so the close handler can never be
/// left blocking quit over a run nobody is watching.
async fn poll_job(app: AppHandle, client: TurnClient, job_id: String, channel: String) {
    let mut after = 0usize;
    let mut failures = 0u32;
    loop {
        match client
            .research_progress::<ResearchProgress>(&job_id, after)
            .await
        {
            Ok(p) => {
                failures = 0;
                for frame in p.frames {
                    let _ = app.emit(&channel, frame);
                }
                after = p.next;
                if p.finished {
                    tracing::debug!(job_id = %job_id, "deep-research: terminal frame relayed");
                    break;
                }
                let _ = app.emit(
                    &channel,
                    ResearchFrame::Heartbeat {
                        elapsed_secs: p.elapsed_secs,
                        quiet_secs: p.quiet_secs,
                        stage: p.stage,
                    },
                );
            }
            Err(e) => {
                failures += 1;
                tracing::debug!(
                    job_id = %job_id,
                    failures,
                    error = %e,
                    "deep-research: progress poll failed",
                );
                if failures >= POLL_FAILURES_TOLERATED {
                    let _ = app.emit(
                        &channel,
                        ResearchFrame::Failed {
                            error: format!(
                                "lost the daemon while polling {job_id} ({failures} tries): {e} — \
                                 the run continues on the daemon; reopen it from the shelf"
                            ),
                        },
                    );
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    polling_set().remove(&job_id);
}

/// Ask a running loop to stop. Not a kill: the daemon raises the loop's
/// abort flag, the loop lands on a truncated report with the truncation
/// declared, and the resume affordance picks up from the last checkpoint.
#[tauri::command]
pub async fn dr_abort(state: State<'_, Arc<AppState>>, job_id: String) -> Result<(), String> {
    research_client(&state)
        .research_abort::<ResearchAbortAck>(&job_id)
        .await
        .map(|_| ())
        .map_err(|e| format!("dr_abort: {e}"))
}

// ── The shelf, the active census, the report ───────────────────────────────

/// List prior runs under the daemon's run base, newest first. `live` is
/// the daemon's job table's answer — the one decider.
#[tauri::command]
pub async fn dr_list_runs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ResearchRunSummary>, String> {
    research_client(&state)
        .research_runs()
        .await
        .map_err(|e| format!("dr_list_runs: {e}"))
}

/// One run the daemon is driving right now, with everything a view that
/// holds no handle needs to re-attach: the channel to listen on and when
/// this leg started. The channel is this app's naming, added to the
/// daemon's row.
#[derive(Debug, Serialize, Clone)]
pub struct DrActiveRun {
    pub run_id: String,
    pub channel: String,
    pub question: Option<String>,
    pub started_at_unix: i64,
}

/// The runs the daemon is driving. A view that was unmounted when the run
/// began — or a webview that reloaded and lost its listener — recovers the
/// live run from here. A run this app is NOT polling (started before a
/// relaunch, say) is re-attached: the poll loop starts from cursor 0, so
/// the store replays the log from `started`.
#[tauri::command]
pub async fn dr_active_runs(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<DrActiveRun>, String> {
    let client = research_client(&state);
    let rows: Vec<ResearchActiveRun> = client
        .research_active()
        .await
        .map_err(|e| format!("dr_active_runs: {e}"))?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let channel = progress_channel(&r.run_id);
        let attached = polling_set().insert(r.run_id.clone());
        if attached {
            tracing::debug!(run_id = %r.run_id, "deep-research: re-attaching to a daemon run");
            tokio::spawn(poll_job(
                app.clone(),
                research_client(&state),
                r.run_id.clone(),
                channel.clone(),
            ));
        }
        out.push(DrActiveRun {
            run_id: r.run_id,
            channel,
            question: r.question,
            started_at_unix: r.started_at_unix,
        });
    }
    Ok(out)
}

/// Quit anyway, with research still running. Called only after the
/// operator has been told what is in flight and said to go ahead — the
/// close handler refuses on its own until then. The run is the daemon's
/// and keeps going; `dr_active_runs` re-attaches on the next launch.
#[tauri::command]
pub async fn dr_quit_anyway(app: AppHandle) {
    tracing::info!(
        polling = polling_set().len(),
        "deep-research: operator chose to quit with a run in flight"
    );
    app.exit(0);
}

/// Open the checked report for a completed run — the daemon renders it
/// from the loop's artifacts, the only source.
#[tauri::command]
pub async fn dr_open_report(
    state: State<'_, Arc<AppState>>,
    run_id: String,
) -> Result<ResearchReport, String> {
    research_client(&state)
        .research_report::<ResearchReport>(&run_id)
        .await
        .map_err(|e| format!("dr_open_report: {e}"))?
        .ok_or_else(|| {
            format!("run {run_id}: no such run on the daemon's shelf, or it did not reach a report")
        })
}
