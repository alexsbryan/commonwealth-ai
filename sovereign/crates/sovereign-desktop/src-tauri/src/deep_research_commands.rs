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
/// renders. A clock that cannot be read is `0`, never a fabricated time.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
        let done_poll = Arc::clone(&done);
        let app_poll = app_runner.clone();
        let channel_poll = channel_runner.clone();
        let poll = tokio::spawn(async move {
            let mut last: Option<DrLiveSnapshot> = None;
            let mut last_change_unix = started_at_unix;
            while !done_poll.load(Ordering::Relaxed) {
                let snapshot = poller.snapshot();
                let stage = snapshot
                    .as_ref()
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

// ── Live snapshot ──────────────────────────────────────────────────────────

/// Everything the live view shows, read from the run dir (the single state
/// source). `None` before the charter exists (the loop writes it first).
#[derive(Debug, Clone, PartialEq)]
struct DrLiveSnapshot {
    round: Option<u32>,
    max_rounds: Option<u32>,
    stage: String,
    gaps: Vec<DrGap>,
    budget: DrBudget,
    consent: Option<DrConsent>,
}

/// Polls the run dir. `snapshot()` re-reads the artifacts on every call
/// (cheap — a handful of small JSON files); the caller decides whether the
/// snapshot CHANGED before emitting, so the channel stays lean.
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
        // The charter (with the consent grant) is the first artifact; until
        // it exists there is no run state to show.
        let charter_raw = std::fs::read(dir.join("charter.json")).ok()?;
        let charter: Charter = serde_json::from_slice(&charter_raw).ok()?;

        // Round + stage, derived from which artifacts exist: the newest
        // gap-list-<round>.json names the current round; verdict-set.json
        // means the writing is being checked; report.md means done.
        let mut round: Option<u32> = None;
        let mut gaps: Vec<DrGap> = Vec::new();
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
                            .map(|g| DrGap {
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

        // Budget ledger — written from birth.
        let mut budget = DrBudget::default();
        if let Ok(raw) = std::fs::read(dir.join("budget-ledger.json")) {
            if let Ok(ledger) = serde_json::from_slice::<BudgetLedger>(&raw) {
                budget = DrBudget {
                    spent: ledger.spent.into_iter().collect(),
                    remaining: ledger.remaining.into_iter().collect(),
                };
            }
        }

        // Consent-grant status from the charter (live, not close-time).
        let max_rounds = charter.charter.max_rounds;
        let consent = charter.charter.consent.map(|c| DrConsent {
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

/// Emit a `Live` event only when the run dir actually moved. Returns
/// whether it did, which is what the heartbeat's quiet clock counts from —
/// so "nothing has changed for 4 minutes" is a measured fact rather than
/// an inference from an absent event.
fn emit_if_changed(
    app: &AppHandle,
    channel: &str,
    snapshot: Option<DrLiveSnapshot>,
    last: &mut Option<DrLiveSnapshot>,
) -> bool {
    if snapshot.is_none() || *last == snapshot {
        return false;
    }
    *last = snapshot.clone();
    if let Some(s) = snapshot {
        let _ = app.emit(
            channel,
            DeepResearchRunEvent::Live {
                round: s.round,
                max_rounds: s.max_rounds,
                stage: s.stage,
                gaps: s.gaps,
                budget: s.budget,
                consent: s.consent,
            },
        );
    }
    true
}

// ── Past runs ──────────────────────────────────────────────────────────────

/// One prior run on the shelf — read from its run dir's charter (live facts)
/// and manifest (close facts). A run without a manifest is `interrupted`
/// (the resume affordance's raw material).
#[derive(Debug, Serialize, Clone)]
pub struct DrRunSummary {
    pub run_id: String,
    pub question: Option<String>,
    pub created_at_unix: Option<i64>,
    /// The manifest's close-time state, or `None` when there is no
    /// manifest. ABSENCE IS REPORTED, NEVER DEFAULTED (§18.3): this field
    /// used to read `interrupted` whenever the manifest was missing, which
    /// made a run that was actively turning indistinguishable from one that
    /// had died — and put a Resume button next to it. Read it WITH `live`:
    /// live is "running", absent-and-not-live is genuinely interrupted.
    pub terminal_state: Option<String>,
    /// Is this process driving the run right now? From the live-run
    /// registry — the one decider.
    pub live: bool,
    pub rounds: usize,
    pub report_present: bool,
    pub consent: Option<DrConsent>,
}

/// List prior runs under the base, newest first (dr-<unix> sorts
/// chronologically).
#[tauri::command]
pub async fn dr_list_runs() -> Result<Vec<DrRunSummary>, String> {
    let base = runs_base();
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
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
            out.push(DrRunSummary {
                run_id,
                question: charter.as_ref().map(|c| c.question.clone()),
                created_at_unix: charter.as_ref().map(|c| c.created_at_unix),
                terminal_state: manifest.as_ref().map(|m| m.terminal_state.clone()),
                live,
                rounds: manifest.as_ref().map(|m| m.rounds.len()).unwrap_or(0),
                report_present: dir.join("report.md").is_file(),
                consent: charter.and_then(|c| c.charter.consent).map(|c| DrConsent {
                    release_floor: c.release_floor.as_str().to_string(),
                    granted_at_unix: c.granted_at_unix,
                }),
            });
        }
    }
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    Ok(out)
}

/// One run this process is driving right now, with everything a view that
/// holds no handle needs to re-attach: the channel to listen on and when
/// this leg started.
#[derive(Debug, Serialize, Clone)]
pub struct DrActiveRun {
    pub run_id: String,
    pub channel: String,
    pub question: Option<String>,
    pub started_at_unix: i64,
}

/// Quit anyway, with research still running. Called only after the operator
/// has been told what is in flight and said to go ahead — the close handler
/// refuses on its own until then. The run dir keeps every artifact written
/// so far, so the run comes back as resumable rather than lost.
#[tauri::command]
pub async fn dr_quit_anyway(app: AppHandle) {
    tracing::info!(
        live_run = ?first_live_run_id(),
        "deep-research: operator chose to quit with a run in flight"
    );
    app.exit(0);
}

/// The runs this process is driving. A view that was unmounted when the
/// run began — or a webview that reloaded and lost its listener — recovers
/// the live run from here, instead of showing an empty composer while work
/// is in flight.
#[tauri::command]
pub async fn dr_active_runs() -> Vec<DrActiveRun> {
    let entries: Vec<(String, String, i64, PathBuf)> = {
        let guard = live_runs().lock().expect("live runs mutex");
        guard
            .iter()
            .map(|(id, r)| {
                (
                    id.clone(),
                    r.channel.clone(),
                    r.started_at_unix,
                    r.run_dir.clone(),
                )
            })
            .collect()
    };
    let mut out: Vec<DrActiveRun> = entries
        .into_iter()
        .map(|(run_id, channel, started_at_unix, run_dir)| DrActiveRun {
            run_id,
            channel,
            // The question is the charter's, read at call time: a resumed
            // leg was started with no question text of its own.
            question: std::fs::read(run_dir.join("charter.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok())
                .map(|c| c.question),
            started_at_unix,
        })
        .collect();
    out.sort_by(|a, b| b.started_at_unix.cmp(&a.started_at_unix));
    out
}

// ── The checked report ─────────────────────────────────────────────────────

/// The checked report + its verdict dimensions, rendered from the verb's
/// artifacts — never re-invented. `report_md` is the verb's own report.md;
/// the dimensions come from verdict-set.json (corroboration), manifest.json
/// (residue, reframe, alignment, not-covered), and the constitution check
/// over the evidence windows (the (g) position property).
#[derive(Debug, Serialize, Clone)]
pub struct DrReport {
    pub run_id: String,
    pub question: String,
    pub terminal_state: String,
    pub report_md: String,
    /// Verdict-set claims with their corroboration records (the gate's own
    /// accounting — origins, floor, pass).
    pub claims: Vec<DrFinalClaim>,
    /// Open questions (could-not-judge) from the manifest.
    pub not_covered: Vec<String>,
    /// The epistemic residue — every searched-but-absent query.
    pub residue: Vec<DrResidueRow>,
    pub reframe: Option<DrReframe>,
    pub alignment: Option<DrAlignment>,
    pub budget: DrBudget,
    pub rounds: Vec<DrRoundRow>,
    pub consent: Option<DrConsent>,
    /// The (g) constitution position: zero untraced figures in [passed].
    /// `violations` names each offending claim; `unresolved` counts claims
    /// whose evidence ids did not resolve to window chunks (reported, never
    /// defaulted).
    pub constitution: DrConstitution,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrFinalClaim {
    pub id: String,
    pub text: String,
    pub verdict: String,
    pub status: String,
    pub citations: Vec<DrCitation>,
    pub corroboration: Option<DrCorroboration>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrCitation {
    pub evidence_id: String,
    pub url: String,
    pub chunk_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrCorroboration {
    pub origins: Vec<String>,
    pub support_chunks: usize,
    pub floor: usize,
    pub passes_floor: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrResidueRow {
    pub query: String,
    pub round: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrReframe {
    pub round: u32,
    pub original_question: String,
    pub reframed_question: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrAlignment {
    pub round: u32,
    pub original_question: String,
    pub redirected_question: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrRoundRow {
    pub round: u32,
    pub gaps_before: usize,
    pub gaps_after: usize,
    pub fetched: usize,
    pub search_calls: u32,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct DrConstitution {
    /// Number of [passed] claims checked.
    pub passed_claims: usize,
    /// Every untraced-figure violation: "claim c1 [passed] carries untraced
    /// figures: 2024". Empty = the position property holds.
    pub violations: Vec<String>,
    /// [passed] claims whose evidence ids resolved to no window chunk — the
    /// check could not run on them; reported, never defaulted.
    pub unresolved: usize,
}

/// Open the checked report for a completed run (the report view's data —
/// the verb's artifacts are the only source).
#[tauri::command]
pub async fn dr_open_report(run_id: String) -> Result<DrReport, String> {
    let dir = runs_base().join(&run_id);
    if !dir.is_dir() {
        return Err(format!("no run {run_id} under {}", runs_base().display()));
    }
    build_report(&dir)
        .ok_or_else(|| format!("run {run_id} has no report.md — it did not reach a report"))
}

/// Assemble the DrReport from a run dir's artifacts.
fn build_report(run_dir: &Path) -> Option<DrReport> {
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
                .map(|c| DrFinalClaim {
                    id: c.id.clone(),
                    text: c.text.clone(),
                    verdict: c.verdict.as_str().to_string(),
                    status: c.status.clone(),
                    citations: c
                        .citations
                        .iter()
                        .map(|ct| DrCitation {
                            evidence_id: ct.evidence_id.clone(),
                            url: ct.url.clone(),
                            chunk_id: ct.chunk_id.clone(),
                        })
                        .collect(),
                    corroboration: c.corroboration.as_ref().map(|cor| DrCorroboration {
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

    Some(DrReport {
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
                    .map(|r| DrResidueRow {
                        query: r.query.clone(),
                        round: r.round,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        reframe: manifest
            .as_ref()
            .and_then(|m| m.reframe.as_ref())
            .map(|r| DrReframe {
                round: r.round,
                original_question: r.original_question.clone(),
                reframed_question: r.reframed_question.clone(),
                reason: r.reason.clone(),
            }),
        alignment: manifest
            .as_ref()
            .and_then(|m| m.alignment.as_ref())
            .map(|a| DrAlignment {
                round: a.round,
                original_question: a.original_question.clone(),
                redirected_question: a.redirected_question.clone(),
                reason: a.reason.clone(),
            }),
        budget: manifest
            .as_ref()
            .map(|m| DrBudget {
                spent: m.budget.spent.clone().into_iter().collect(),
                remaining: m.budget.remaining.clone().into_iter().collect(),
            })
            .unwrap_or_default(),
        rounds: manifest
            .as_ref()
            .map(|m| {
                m.rounds
                    .iter()
                    .map(|r| DrRoundRow {
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
            .map(|c| DrConsent {
                release_floor: c.release_floor.as_str().to_string(),
                granted_at_unix: c.granted_at_unix,
            }),
        constitution,
    })
}

/// The (g) position property, over the verb's own artifacts: every figure
/// token in a [passed] claim must appear in the claim's evidence chunks.
/// Uses the loop's own decider (`containment::missing_claim_figures`) — one
/// figure parser, one implementation. Claims whose evidence ids resolve to
/// no window chunk are counted `unresolved` — reported, never defaulted.
fn constitution_check(run_dir: &Path, verdict_set: Option<&VerdictSet>) -> DrConstitution {
    let mut out = DrConstitution::default();
    let Some(vs) = verdict_set else {
        // No verdict set — the run never reached the claim gate; the
        // position property is vacuous and the report view shows no claims.
        return out;
    };
    // All window chunks, keyed by id (a chunk id is unique per run — the
    // window's dedup convention).
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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::deep_research::icd::{
        BudgetAllowance, CharterValues, ContainmentConfig, CorroborationRecord, CustodyPolicy,
        EmptyWindow, EvidenceWindow as Ew, FinalClaim, Gap, TriageConfig, UrlConstraintPolicy,
        WindowChunk,
    };
    use sovereign_core::egress::ConsentGrant;
    use sovereign_core::types::Custody;

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
        let dir = std::env::temp_dir().join(format!("dr-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        fixture_charter(&dir, "When did Apollo 11 land?");
        fixture_gap_list(
            &dir,
            1,
            vec![Gap {
                id: "g1".to_string(),
                text: "the landing date needs a second origin".to_string(),
                actionable_query: "Apollo 11 landing date".to_string(),
                from_claim_id: Some("c1".to_string()),
                corroboration: None,
            }],
        );
        fixture_budget(&dir);

        let snap = RunDirPoller::new(dir.clone()).snapshot().unwrap();
        assert_eq!(snap.round, Some(1));
        assert_eq!(snap.stage, "rounding");
        assert_eq!(snap.gaps.len(), 1);
        assert_eq!(snap.gaps[0].id, "g1");
        assert_eq!(snap.budget.spent.get("web"), Some(&2));
        assert_eq!(snap.budget.remaining.get("web"), Some(&2));
        let consent = snap.consent.unwrap();
        assert_eq!(consent.release_floor, "public-web");
        assert_eq!(consent.granted_at_unix, 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_is_none_before_the_charter_lands() {
        let dir = std::env::temp_dir().join(format!("dr-presnap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let snap = RunDirPoller::new(dir.clone()).snapshot();
        assert!(snap.is_none(), "no charter — no run state to show");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_consent_means_default_deny_is_reported() {
        let dir = std::env::temp_dir().join(format!("dr-noconsent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_json(
            &dir,
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
        let snap = RunDirPoller::new(dir.clone()).snapshot().unwrap();
        assert!(snap.consent.is_none(), "default-deny must read as no grant");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_advances_with_the_artifacts() {
        let dir = std::env::temp_dir().join(format!("dr-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        fixture_charter(&dir, "Q");
        fixture_budget(&dir);

        let poller = RunDirPoller::new(dir.clone());
        let snap = poller.snapshot().unwrap();
        assert_eq!(snap.stage, "planning", "charter only — planning");

        fixture_gap_list(&dir, 1, Vec::new());
        let snap = poller.snapshot().unwrap();
        assert_eq!(snap.stage, "rounding", "gap-list-1 — rounding");

        std::fs::write(
            dir.join("verdict-set.json"),
            serde_json::to_vec(&VerdictSet {
                icd: "verdict-set".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                claims: Vec::new(),
                empty_rounds: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
        let snap = poller.snapshot().unwrap();
        assert_eq!(
            snap.stage, "checking",
            "verdict-set — the writing is checked"
        );

        std::fs::write(dir.join("report.md"), "# Report").unwrap();
        let snap = poller.snapshot().unwrap();
        assert_eq!(snap.stage, "done", "report.md — done");
        assert!(poller.report_md().is_some());
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn constitution_holds_when_every_passed_figure_is_traced() {
        let dir = std::env::temp_dir().join(format!("dr-const-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        window(
            &dir,
            1,
            vec![WindowChunk {
                id: "c1".to_string(),
                locator: "estate:x:1".to_string(),
                source_url: "https://example.com/a".to_string(),
                custody: "personal".to_string(),
                provenance_class: "primary".to_string(),
                content: "Apollo 11 landed on July 20, 1969.".to_string(),
                ingested_into: None,
                tags: Vec::new(),
            }],
        );
        let mut vs = passed_claim_set();
        vs.claims.push(FinalClaim {
            id: "c1".to_string(),
            text: "Apollo 11 landed on July 20, 1969.".to_string(),
            verdict: Verdict::Passed,
            status: "passed".to_string(),
            evidence_ids: vec!["c1".to_string()],
            citations: Vec::new(),
            flag: None,
            corroboration: Some(CorroborationRecord {
                origins: vec!["https://example.com/a".to_string()],
                support_chunks: 1,
                floor: 2,
                passes_floor: false,
            }),
        });
        write_json(&dir, "verdict-set.json", &vs);
        let check = constitution_check(&dir, Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert!(check.violations.is_empty(), "{:?}", check.violations);
        assert_eq!(check.unresolved, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn constitution_names_an_untraced_figure_in_a_passed_claim() {
        let dir = std::env::temp_dir().join(format!("dr-const-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The claim carries "2024" which the evidence never mentions.
        window(
            &dir,
            1,
            vec![WindowChunk {
                id: "c1".to_string(),
                locator: "estate:x:1".to_string(),
                source_url: "https://example.com/a".to_string(),
                custody: "personal".to_string(),
                provenance_class: "primary".to_string(),
                content: "The bridge opened in 1930.".to_string(),
                ingested_into: None,
                tags: Vec::new(),
            }],
        );
        let mut vs = passed_claim_set();
        vs.claims.push(FinalClaim {
            id: "c1".to_string(),
            text: "The bridge opened in 1930 and was restored in 2024.".to_string(),
            verdict: Verdict::Passed,
            status: "passed".to_string(),
            evidence_ids: vec!["c1".to_string()],
            citations: Vec::new(),
            flag: None,
            corroboration: None,
        });
        write_json(&dir, "verdict-set.json", &vs);
        let check = constitution_check(&dir, Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert_eq!(check.violations.len(), 1, "{:?}", check.violations);
        assert!(
            check.violations[0].contains("2024"),
            "{}",
            check.violations[0]
        );
        assert_eq!(check.unresolved, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresolved_evidence_is_reported_not_defaulted() {
        let dir = std::env::temp_dir().join(format!("dr-const-unres-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No evidence windows at all — the claim's ids resolve nowhere.
        let mut vs = passed_claim_set();
        vs.claims.push(FinalClaim {
            id: "c1".to_string(),
            text: "Something passed.".to_string(),
            verdict: Verdict::Passed,
            status: "passed".to_string(),
            evidence_ids: vec!["missing".to_string()],
            citations: Vec::new(),
            flag: None,
            corroboration: None,
        });
        write_json(&dir, "verdict-set.json", &vs);
        let check = constitution_check(&dir, Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert!(check.violations.is_empty());
        assert_eq!(check.unresolved, 1, "unresolvable evidence is counted");
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn demo_backend_override_is_absent_unless_the_demo_var_is_set() {
        // The env mutation could race a parallel test that reads the var —
        // none does (it is demo-only), but the lock keeps the intent loud.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
        assert!(
            demo_backend_override().is_none(),
            "unset = the live port; real flows must not gain a mock backend"
        );
        unsafe {
            std::env::set_var(
                "SOVEREIGN_DEMO_DR_FLAGS",
                "--backend mock --mock-deck /tmp/deep-research-deck",
            )
        };
        assert_eq!(
            demo_backend_override(),
            Some((
                "mock".to_string(),
                Some(PathBuf::from("/tmp/deep-research-deck"))
            )),
            "the demo pass-through lands in typed launch options, not an argv"
        );
        unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
    }

    /// THE SHIPPING TEST, structurally (§7: make it structural, not
    /// remembered). Deep research used to run by spawning `svrn
    /// deep-research`, found by probing PATH — so a desktop-only install,
    /// which has no CLI on PATH, got zero runs, and an install that DID
    /// have one could bind a different version than the app was built
    /// against. Neither failure is visible in a unit test of behaviour;
    /// both are visible here.
    ///
    /// Watched red against the pre-lift file at HEAD: it spawned a child
    /// process twice and probed the CLI-path override.
    ///
    /// The scan is scoped to the PRODUCTION half of the file — everything
    /// above `#[cfg(test)]`. This test necessarily spells the forbidden
    /// tokens, and an instrument that trips on its own prose measures
    /// nothing (the same trap as note 8714cf3c, where a render gate
    /// matched the sentence describing it).
    #[test]
    fn the_driver_starts_no_subprocess_and_probes_no_path() {
        let src = include_str!("deep_research_commands.rs");
        let body = src
            .split_once("#[cfg(test)]")
            .expect("this file has a test module")
            .0;
        for forbidden in [
            "Command::new",
            "SOVEREIGN_CLI_PATH",
            "sovereign-cli",
            ".local/bin/sovereign",
        ] {
            assert!(
                !body.contains(forbidden),
                "the deep-research driver must not reach for a CLI binary, but it names \
                 `{forbidden}` — a desktop-only install has none, and a PATH hit can be a \
                 different version than this build"
            );
        }
    }

    /// A token the closed set does not name is IGNORED rather than
    /// forwarded: with no second process to parse it, passing it on
    /// would mean pretending it did something.
    #[test]
    fn demo_backend_override_ignores_tokens_it_does_not_name() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(
                "SOVEREIGN_DEMO_DR_FLAGS",
                "--backend mock --nonsense 7 --mock-deck /tmp/d",
            )
        };
        assert_eq!(
            demo_backend_override(),
            Some(("mock".to_string(), Some(PathBuf::from("/tmp/d"))))
        );
        unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
    }
}
