// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep-research scene 1 driver (order deep-research-t3b).
//!
//! The desktop is a DRIVER over the loop, not an implementation of it: it
//! forwards the operator's question + budget + typed consent grant into
//! `sovereign_core::deep_research::launch`, and then READS the run-dir
//! artifacts the loop writes — `charter.json`, `budget-ledger.json`,
//! `gap-list-<round>.json`, `verdict-set.json`, `report.md`, `manifest.json` —
//! as the single live-state source. No loop logic, no instrument, no decider,
//! no second state source.
//!
//! It used to SPAWN `svrn deep-research` to get here, probing PATH for a
//! binary that a desktop-only install does not have — and because config
//! does not cross a process boundary, the operator's configured search
//! provider and every env-set knob stopped at the process edge. The
//! one-loop rule is now enforced the honest way: by calling the one
//! function. `launch::prepare` is the single assembly of a `RunConfig`,
//! so the CLI verb and this driver cannot drift apart.
//!
//! The artifacts are deserialized with sovereign-core's OWN ICD types
//! (`deep_research::icd`), so a schema drift between the verb and the viewer
//! is a compile error, not a silent mismatch. The report's constitution check
//! (zero untraced figures in [passed]) calls the loop's own decider
//! (`containment::missing_claim_figures`) — never a second figure parser.
//!
//! Aborting is a flag, not a signal: `run` polls the shared `AtomicBool`
//! at every state entry and lands on a truncated report with the
//! truncation declared, where killing a child process left the run dir
//! mid-write.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_core::deep_research::containment::missing_claim_figures;
use sovereign_core::deep_research::icd::{
    BudgetLedger, Charter, EvidenceWindow, GapList, Manifest, Verdict, VerdictSet,
};
use sovereign_core::deep_research::launch::{self, LaunchOptions};
use sovereign_core::deep_research::{resume, run, SearchSource};
use sovereign_core::types::Custody;
use tauri::{AppHandle, Emitter};

// ── Carved-out sections ────────────────────────────────────────────────────
//
// One driver, four files. The section banners this file used to carry are now
// module boundaries (ARCH §3.1): the driver kept the run lifecycle, and the
// three read-side surfaces moved out beside it.
mod live;
mod report;
mod runs;

#[cfg(test)]
mod tests;

use live::{emit_if_changed, DrLiveSnapshot, RunDirPoller};
use report::build_report;

// Glob, not a named list: `#[tauri::command]` also emits per-command items
// (`__cmd__<name>`, `__tauri_command_name_<name>`) that `generate_handler!`
// resolves beside the function. A named re-export leaves those behind and the
// handler macro then fails to resolve, so the whole public surface travels.
pub use report::*;
pub use runs::*;

/// The run-dir base the desktop drives the verb with (`--run-dir <base>`).
/// A stable, non-temp home so runs survive app restarts: the resume
/// affordance and the Library's estate linkage both key off it. The verb
/// mints `<base>/dr-<unix>` per run (the run id is the verb's own).
fn runs_base() -> PathBuf {
    SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("deep-research-runs")
}

// ── Capabilities ───────────────────────────────────────────────────────────

/// What this build can do. Deep research is compiled IN — there is no
/// binary to find and nothing to probe — so the capability question is
/// answered by the build, not by a filesystem search. `flags` keeps the
/// name and shape the UI already gates its affordances on.
#[derive(Debug, Serialize, Clone)]
pub struct DrCapabilities {
    /// Retained for the UI's shape. Always `None` now: deep research is
    /// not a separate binary any more.
    pub cli_path: Option<String>,
    /// The affordances this build supports. Named capabilities, not
    /// scraped `--help` tokens.
    pub flags: Vec<String>,
    /// Why the feature is unavailable, when it is. Absence is reported,
    /// never defaulted.
    pub error: Option<String>,
}

/// Report the in-process deep-research capability. Where this used to
/// probe `deep-research --help` on a CLI it had to find on PATH — and
/// returned "not installed" on any desktop-only install — the loop is
/// now linked into this binary, so the answer cannot be absent.
#[tauri::command]
pub async fn dr_capabilities() -> DrCapabilities {
    // The daemon is still required (the loop's embed + draft surface).
    // A missing or unreadable SetupConfig is reported, not defaulted.
    let error = launch::daemon_targets().err();
    DrCapabilities {
        cli_path: None,
        flags: vec![
            "--consent".to_string(),
            "--corpora".to_string(),
            "--fetch".to_string(),
            "--max-rounds".to_string(),
            "--resume".to_string(),
            "--search".to_string(),
        ],
        error,
    }
}

// ── Run lifecycle ──────────────────────────────────────────────────────────

fn progress_channel(job_id: &str) -> String {
    format!("deep-research://progress/{job_id}")
}

/// The launch surface — mirrors `WorkflowRunHandle`'s shape (job-scoped
/// channel; the UI listens for `DeepResearchRunEvent` events on it).
#[derive(Debug, Serialize, Clone)]
pub struct DrRunHandle {
    pub job_id: String,
    pub channel: String,
}

/// What the operator typed at the Ask entry: the question, the budget, and
/// the typed consent grant (default-deny — `consent: None` sends no
/// `--consent` flag and the verb's web leg refuses non-public-web payloads).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DrStartOptions {
    pub max_rounds: Option<u32>,
    /// Estate corpus ids for `--corpora` (the next ask's compounding handoff:
    /// a prior run's estate corpus is selectable here).
    pub corpora: Vec<String>,
    /// `"public-web"` | `"peer"` | `"personal"` — the typed release floor.
    /// Absent = default-deny (no `--consent` flag).
    pub consent: Option<String>,
    pub search: Option<u32>,
    pub fetch: Option<u32>,
    /// t3a's resume surface: `--resume <run-id>`. Only offered when the
    /// verb's help names `--resume`.
    pub resume_run_id: Option<String>,
}

/// The typed consent grant's closed set, parsed at the driver boundary so
/// a typo never reaches a run. `Custody::parse_wire` is the ONE parser —
/// the desktop does not carry a second spelling of the closed set. `None`
/// means default-deny.
fn consent_class(floor: &str) -> Result<Custody, String> {
    match Custody::parse_wire(floor) {
        Some(c) if c != Custody::Unknown => Ok(c),
        _ => Err(format!(
            "unknown consent class `{floor}` — the closed set is public-web | peer | personal"
        )),
    }
}

/// Demo-only backend override (order deep-research-t3b, evidence pass
/// (f)): `SOVEREIGN_DEMO_DR_FLAGS` carries `--backend mock --mock-deck
/// DIR` so the recorded pass films a deterministic deck run while the Ask
/// surface stays spec-faithful (question + budget + consent only). Unset
/// in every real flow — the demo's global-setup is the only writer.
///
/// It stays spelled as flags because the demo's global-setup and the
/// env-flag registry already name it that way, but it now lands in typed
/// `LaunchOptions` fields rather than a subprocess argv. Anything the
/// closed set does not name is IGNORED, not passed through: with no
/// second process to parse them, an unrecognised token has no meaning.
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

/// A live event from the driver, tagged on `kind` (mirrors the workflow run
/// event union). Everything except `failed` is derived by READING the run-dir
/// artifacts the verb wrote — the run-dir is the single state source.
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeepResearchRunEvent {
    /// The verb named its run dir (its stderr's "run dir" line — the verb's
    /// own naming). Polling begins from here.
    Started { run_id: String, run_dir: String },
    /// A changed snapshot of the run dir: current round, the gate's named
    /// gap list, the budget ledger, and the consent-grant status (from
    /// charter.json — live, not the close-time manifest).
    Live {
        round: Option<u32>,
        /// The charter's `max_rounds` — what the round number is OUT OF.
        /// `None` before the charter is readable; the view says "round 2"
        /// rather than inventing a denominator.
        max_rounds: Option<u32>,
        stage: String,
        gaps: Vec<DrGap>,
        budget: DrBudget,
        consent: Option<DrConsent>,
    },
    /// The run is still being driven. Emitted every poll tick regardless of
    /// whether anything changed, because `Live` is deliberately quiet and a
    /// round spends minutes inside one model call with no artifact moving —
    /// so silence on this channel is the NORMAL case, and a view holding
    /// only change events cannot tell a healthy run from a dead one. The
    /// tick says: still here, this long in, this long since anything moved.
    Heartbeat {
        elapsed_secs: i64,
        quiet_secs: i64,
        stage: String,
    },
    /// Terminal: the verb exited and `report.md` exists — the checked report
    /// with its verdict dimensions, read from the verb's artifacts.
    ReportReady { report: DrReport },
    /// Terminal: the verb could not run or exited without a report.
    Failed { error: String },
}

/// One named gap from `gap-list-<round>.json` — the gate's compass output.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct DrGap {
    pub id: String,
    pub text: String,
}

/// The budget ledger's spent/remaining, keyed by meter (from
/// `budget-ledger.json`).
#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct DrBudget {
    pub spent: BTreeMap<String, u32>,
    pub remaining: BTreeMap<String, u32>,
}

/// The run's typed consent grant as recorded in the charter (absent =
/// default-deny: the web leg refused non-public-web payloads).
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct DrConsent {
    pub release_floor: String,
    pub granted_at_unix: i64,
}

/// One run this process is driving right now.
struct LiveRun {
    /// Raised by `dr_abort`. The loop polls it at every state entry and
    /// lands on a truncated report with the truncation declared — where
    /// killing a child process left the run dir mid-write, with no record
    /// that it had been cut short.
    abort: Arc<AtomicBool>,
    /// When THIS leg started. A resumed run's charter still carries the
    /// original birth, which is not the elapsed the operator is watching.
    started_at_unix: i64,
    /// The job-scoped progress channel, so a webview that reloaded
    /// mid-run can re-attach to a run it no longer holds a handle for.
    channel: String,
    run_dir: PathBuf,
}

/// The live-run registry, keyed by run id. It is the ONE decider for "is
/// this run alive" (§8: one decider, one name) — the shelf's `live` flag,
/// `dr_active_runs`'s re-attach list, the resume guard and `dr_abort` all
/// read this same map, so a run cannot be alive for one surface and
/// finished for another.
///
/// It exists because the absence of a `manifest.json` used to be DEFAULTED
/// to `interrupted`: a run that was actively turning showed on the shelf as
/// interrupted, with a Resume button next to it, and pressing it re-entered
/// a run dir another task was mid-write on. The registry makes that state
/// nameable instead of guessed (§18.3 — absence is reported, never
/// defaulted).
static LIVE_RUNS: OnceLock<Mutex<HashMap<String, LiveRun>>> = OnceLock::new();

fn live_runs() -> &'static Mutex<HashMap<String, LiveRun>> {
    LIVE_RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clears a run from [`LIVE_RUNS`] and stops its poller HOWEVER the driving
/// task ends — including an unwind.
///
/// The happy path still does both explicitly, so the ordering around the
/// terminal event is unchanged; this is the backstop for the path that has no
/// code. `LIVE_RUNS` is the one decider for "is a run alive", and a panic
/// anywhere in the deep-research loop used to leave it permanently answering
/// YES: `dr_start` refuses every later run, the close handler keeps blocking
/// quit, `dr_abort` returns `Ok(())` on a corpse, and the Start button stays
/// disabled — with the panic itself swallowed by the dropped `JoinHandle`.
/// A registry that can only be wrong in the direction of "stuck forever" needs
/// its cleanup to be structural, not remembered (§7).
struct RunCleanup {
    job_id: String,
    done: Arc<AtomicBool>,
}

impl Drop for RunCleanup {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        // Never `expect` here: a panic inside Drop during an unwind aborts the
        // process, turning a recoverable run failure into a dead app.
        if let Ok(mut runs) = live_runs().lock() {
            runs.remove(&self.job_id);
        }
    }
}

/// The event the window's close handler emits when it refuses to quit
/// because research is in flight. The frontend owns the conversation that
/// follows; the backend only declines to disappear silently.
pub const QUIT_BLOCKED_EVENT: &str = "deep-research://quit-blocked";

/// Is any run in flight? Read by the window's `CloseRequested` handler:
/// closing the app kills the loop's task, and doing that without asking is
/// the one way left for a run to end that the operator did not choose.
pub fn has_live_run() -> bool {
    !live_runs().lock().expect("live runs mutex").is_empty()
}

/// The run this process is driving, if any. `dr_start` reads it to refuse a
/// second concurrent run; see the refusal there for why one at a time.
fn first_live_run_id() -> Option<String> {
    live_runs()
        .lock()
        .expect("live runs mutex")
        .keys()
        .next()
        .cloned()
}

/// Is this process driving that run right now? Every surface that needs
/// to distinguish "still working" from "died" asks through here.
fn is_live(run_id: &str) -> bool {
    live_runs()
        .lock()
        .expect("live runs mutex")
        .contains_key(run_id)
}

/// Seconds since the epoch, for the elapsed/quiet clocks the live view
/// renders. `sovereign_core::time` is the island's ONE decider (clock-gate,
/// ARCH §10.6) and already returns `0` for a clock it cannot read, so this is
/// a name for the call site, not a second implementation.
fn now_unix() -> i64 {
    sovereign_core::time::unix_now()
}

/// Start a deep-research run in-process. Returns as soon as the run dir
/// exists — `launch::prepare` mints it before the loop turns, so the
/// `Started` event carries a real path with no stderr to scrape and no
/// 60-second discovery timeout. The loop then proceeds on a background
/// task and progress lands on the job-scoped channel.
///
/// The `job_id` is the RUN id (`dr-<unix>`), which is what the run
/// actually is — not a process id, which was an address for a child that
/// no longer exists (§7.5: identity from essence, never an address).
#[tauri::command]
pub async fn dr_start(
    app: AppHandle,
    question: String,
    options: DrStartOptions,
) -> Result<DrRunHandle, String> {
    if question.trim().is_empty() && options.resume_run_id.is_none() {
        return Err("a question is required (or a run to resume)".to_string());
    }
    let base = runs_base();
    std::fs::create_dir_all(&base).map_err(|e| format!("run dir base {base:?}: {e}"))?;

    // A run this process is already driving must not be re-entered. The
    // resume path would hand a second loop the same run dir the first is
    // mid-write on — and the shelf used to OFFER exactly that, because a
    // live run with no manifest yet read as `interrupted`. The registry is
    // the decider; the refusal is named, not silent.
    if let Some(run_id) = options.resume_run_id.as_deref() {
        if is_live(run_id) {
            return Err(format!(
                "{run_id} is still running — open it rather than resuming it"
            ));
        }
    }

    // ONE RUN AT A TIME, and the refusal lives here rather than in the UI
    // that happens to know it (§7: make it structural, not remembered).
    // Two reasons, and the first is the load-bearing one: the desktop can
    // represent exactly one run in flight, so a second start would leave
    // the first with no surface, no listener, and no way to report that it
    // finished — the precise failure this whole change set exists to
    // remove. The second is that two concurrent runs contend for the same
    // local inference slot and make each other slower.
    if let Some(existing) = first_live_run_id() {
        return Err(format!(
            "a run is already going ({existing}) — stop it or wait for it to \
             finish before starting another"
        ));
    }

    // Resume restores its identity from the checkpoint; a fresh launch
    // assembles one. Either way `launch` is the ONE assembly — this
    // driver never builds a `RunConfig` of its own.
    let resuming = options.resume_run_id.is_some();
    let launch = match &options.resume_run_id {
        Some(run_id) => launch::prepare_resume(&base.join(run_id)).await?,
        None => {
            // The typed consent grant (default-deny): an absent class
            // grants nothing; an unknown one refuses before a run dir
            // exists.
            let consent_floor = match options.consent.as_deref() {
                None => None,
                Some(floor) => Some(consent_class(floor)?),
            };
            // The demo's deterministic deck, when the demo set it.
            let (backend, mock_deck_dir) =
                demo_backend_override().unwrap_or_else(|| ("auto".to_string(), None));
            let search_source = if backend == "mock" {
                SearchSource::Mock
            } else {
                SearchSource::Corpus
            };
            launch::prepare(LaunchOptions {
                question: question.trim().to_string(),
                runs_base: base,
                max_rounds: options.max_rounds.unwrap_or(2),
                code_set_k: 0,
                eps_quota: 0.0,
                search_allowance: options.search.unwrap_or(4),
                fetch_allowance: options.fetch.unwrap_or(4),
                estate_corpus_ids: options.corpora.clone(),
                search_source,
                backend,
                mock_deck_dir,
                consent_floor,
            })
            .await?
        }
    };

    let job_id = launch.run_id.clone();
    let run_dir = launch.run_dir.clone();
    let abort = Arc::new(AtomicBool::new(false));
    let channel = progress_channel(&job_id);
    let started_at_unix = now_unix();
    live_runs().lock().expect("live runs mutex").insert(
        job_id.clone(),
        LiveRun {
            abort: Arc::clone(&abort),
            started_at_unix,
            channel: channel.clone(),
            run_dir: run_dir.clone(),
        },
    );

    let app_runner = app.clone();
    let channel_runner = channel.clone();
    let run_dir_runner = run_dir.clone();
    let job_runner = job_id.clone();

    let _ = app.emit(
        &channel,
        DeepResearchRunEvent::Started {
            run_id: job_id.clone(),
            run_dir: run_dir.display().to_string(),
        },
    );

    tokio::spawn(async move {
        // Poll the run dir for live state on one task while the loop
        // drives on another. The run dir stays the single state source —
        // in-process changes who writes it, not what reads it.
        let poller = RunDirPoller::new(run_dir_runner.clone());
        let done = Arc::new(AtomicBool::new(false));
        // Held for the life of the task: if `run`/`resume` unwinds, this is
        // what still clears the registry and stops the poller.
        let _cleanup = RunCleanup {
            job_id: job_runner.clone(),
            done: Arc::clone(&done),
        };
        let done_poll = Arc::clone(&done);
        let app_poll = app_runner.clone();
        let channel_poll = channel_runner.clone();
        let poll = tokio::spawn(async move {
            let mut last: Option<DrLiveSnapshot> = None;
            let mut last_change_unix = started_at_unix;
            while !done_poll.load(Ordering::Relaxed) {
                let snapshot = poller.snapshot();
                // Fall back to the LAST KNOWN stage, not to "planning".
                // `snapshot()` returns `None` whenever `charter.json` is
                // briefly unreadable or mid-rewrite, and the store applies
                // `event.stage || a.stage` — so fabricating "planning" rewound
                // a round-3 run to "Planning the search" on a transient read
                // failure. Absence is not a state (§18.3); "planning" is only
                // honest before anything has ever been observed.
                let stage = snapshot
                    .as_ref()
                    .or(last.as_ref())
                    .map(|s| s.stage.clone())
                    .unwrap_or_else(|| "planning".to_string());
                if emit_if_changed(&app_poll, &channel_poll, snapshot, &mut last) {
                    last_change_unix = now_unix();
                }
                // Then the tick, changed or not. This is the difference
                // between "working" and "hung" on a surface whose other
                // events are silent by design.
                let now = now_unix();
                let _ = app_poll.emit(
                    &channel_poll,
                    DeepResearchRunEvent::Heartbeat {
                        elapsed_secs: (now - started_at_unix).max(0),
                        quiet_secs: (now - last_change_unix).max(0),
                        stage,
                    },
                );
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
        });

        let config = launch.config.clone();
        let port = launch.port.clone();
        let provider = launch.provider.clone();
        let outcome = if resuming {
            resume(config, port, provider, abort).await
        } else {
            run(config, port, provider, abort).await
        };
        done.store(true, Ordering::Relaxed);
        let _ = poll.await;
        live_runs()
            .lock()
            .expect("live runs mutex")
            .remove(&job_runner);

        let event = match outcome {
            Ok(mut outcome) => {
                // Closing is not optional: the fetched evidence lands in
                // `dr-estate-<run_id>` and the RACE page is written. A
                // failure here is reported, never swallowed — but the
                // report itself already exists, so the operator still
                // gets it, with the close failure named.
                let close_err = launch::close(&mut outcome, &launch.provider, &launch.embed_model)
                    .await
                    .err();
                match build_report(&run_dir_runner) {
                    Some(report) => match close_err {
                        None => DeepResearchRunEvent::ReportReady { report },
                        Some(e) => DeepResearchRunEvent::Failed {
                            error: format!("the run finished but could not be closed: {e}"),
                        },
                    },
                    None => DeepResearchRunEvent::Failed {
                        error: "the run finished but its artifacts failed to parse".to_string(),
                    },
                }
            }
            Err(e) => DeepResearchRunEvent::Failed {
                error: format!("deep-research failed: {e}"),
            },
        };
        let _ = app_runner.emit(&channel_runner, event);
    });

    Ok(DrRunHandle { job_id, channel })
}

/// Ask a running loop to stop. The flag is polled at every state entry, so
/// the run lands on a truncated report with the truncation DECLARED —
/// where killing a child process left whatever the run dir happened to
/// hold. The resume affordance picks up from the last checkpoint.
#[tauri::command]
pub async fn dr_abort(job_id: String) -> Result<(), String> {
    match live_runs().lock().expect("live runs mutex").get(&job_id) {
        Some(run) => {
            run.abort.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("no active run {job_id}")),
    }
}
