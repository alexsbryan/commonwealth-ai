// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep-research scene 1 driver (order deep-research-t3b).
//!
//! The desktop is a DRIVER over the CLI verb's contract (`svrn deep-research`,
//! order deep-research-t3a): it spawns the verb, forwards the operator's
//! question + budget + typed consent grant as verb flags, and then READS the
//! run-dir artifacts the verb writes — `charter.json`, `budget-ledger.json`,
//! `gap-list-<round>.json`, `verdict-set.json`, `report.md`, `manifest.json` —
//! as the single live-state source. No loop logic, no instrument, no decider,
//! no second state source: the verb remains the only implementation of the
//! loop (the one-loop rule is structural; the desktop is a driver, never an
//! implementation).
//!
//! The artifacts are deserialized with sovereign-core's OWN ICD types
//! (`deep_research::icd`), so a schema drift between the verb and the viewer
//! is a compile error, not a silent mismatch. The report's constitution check
//! (zero untraced figures in [passed]) calls the loop's own decider
//! (`containment::missing_claim_figures`) — never a second figure parser.
//!
//! Binary discovery probes in order (the supervisor's probe order, extended
//! for a CLI rather than a daemon): `SOVEREIGN_CLI_PATH` (dev/dogfood), then
//! `sovereign` / `sovereign-cli` on PATH, then `~/.local/bin/sovereign`.
//! Absence is reported, never defaulted — the Ask entry names the probes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use sovereign_core::deep_research::containment::missing_claim_figures;
use sovereign_core::deep_research::icd::{
    BudgetLedger, Charter, EvidenceWindow, GapList, Manifest, Verdict, VerdictSet,
};
use sovereign_contracts::setup_config::SetupConfig;
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

/// Resolve the CLI binary that owns the deep-research verb. Probes in order:
/// `SOVEREIGN_CLI_PATH` (dev/dogfood), `sovereign` / `sovereign-cli` on PATH,
/// then `~/.local/bin/sovereign`. `None` only when every probe missed — the
/// caller reports it loudly, naming the probes.
fn resolve_cli() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOVEREIGN_CLI_PATH") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for name in ["sovereign", "sovereign-cli"] {
            for dir in std::env::split_paths(&paths) {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cand = PathBuf::from(home).join(".local/bin/sovereign");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

// ── Capabilities ───────────────────────────────────────────────────────────

/// What the installed verb can do — the desktop gates its affordances on the
/// verb's own flag list, so t3a's additions (e.g. `--resume`) light up the
/// resume affordance the moment the verb grows them, with no desktop change.
#[derive(Debug, Serialize, Clone)]
pub struct DrCapabilities {
    /// The resolved CLI path, when one was found.
    pub cli_path: Option<String>,
    /// The flags the verb's `--help` names (`--consent`, `--resume`, …).
    pub flags: Vec<String>,
    /// Why the verb could not be probed, when it could not. Absence is
    /// reported, never defaulted.
    pub error: Option<String>,
}

/// Resolve the CLI and probe `deep-research --help` for its flag list.
#[tauri::command]
pub async fn dr_capabilities() -> DrCapabilities {
    let Some(cli) = resolve_cli() else {
        return DrCapabilities {
            cli_path: None,
            flags: Vec::new(),
            error: Some(
                "the deep-research CLI verb is not installed — probed SOVEREIGN_CLI_PATH, \
                 PATH (sovereign, sovereign-cli), ~/.local/bin/sovereign"
                    .to_string(),
            ),
        };
    };
    let probe = tokio::process::Command::new(&cli)
        .args(["deep-research", "--help"])
        .output()
        .await;
    match probe {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let flags: Vec<String> = text
                .split_whitespace()
                .filter(|tok| tok.starts_with("--") && tok.len() > 2)
                .map(|tok| tok.to_string())
                .collect();
            DrCapabilities {
                cli_path: Some(cli.display().to_string()),
                flags,
                error: None,
            }
        }
        Ok(out) => DrCapabilities {
            cli_path: Some(cli.display().to_string()),
            flags: Vec::new(),
            error: Some(format!(
                "deep-research --help exited {} — the verb may not be built into this CLI",
                out.status
            )),
        },
        Err(e) => DrCapabilities {
            cli_path: Some(cli.display().to_string()),
            flags: Vec::new(),
            error: Some(format!("deep-research --help probe failed: {e}")),
        },
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

/// The typed consent grant's closed set — the verb's own contract, checked
/// at the driver boundary so a typo never reaches the verb. `None` means
/// default-deny: no `--consent` flag.
fn consent_flag(floor: &str) -> Result<Option<&'static str>, String> {
    match floor {
        "public-web" => Ok(Some("public-web")),
        "peer" => Ok(Some("peer")),
        "personal" => Ok(Some("personal")),
        other => Err(format!(
            "unknown consent class `{other}` — the closed set is public-web | peer | personal"
        )),
    }
}

/// Demo-only verb-flag pass-through (order deep-research-t3b, evidence
/// pass (f)): `SOVEREIGN_DEMO_DR_FLAGS` holds verb flags to append at
/// spawn (space-separated; values must not contain spaces). Unset in
/// every real flow — the demo's global-setup is the only writer, so the
/// recorded pass films a deterministic deck run while the Ask surface
/// stays spec-faithful (question + budget + consent only).
fn demo_extra_args() -> Vec<String> {
    std::env::var("SOVEREIGN_DEMO_DR_FLAGS")
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// A live event from the driver, tagged on `kind` (mirrors the workflow run
/// event union). Everything except `failed` is derived by READING the run-dir
/// artifacts the verb wrote — the run-dir is the single state source.
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeepResearchRunEvent {
    /// The verb named its run dir (its stderr's "run dir" line — the verb's
    /// own naming). Polling begins from here.
    Started {
        run_id: String,
        run_dir: String,
    },
    /// A changed snapshot of the run dir: current round, the gate's named
    /// gap list, the budget ledger, and the consent-grant status (from
    /// charter.json — live, not the close-time manifest).
    Live {
        round: Option<u32>,
        stage: String,
        gaps: Vec<DrGap>,
        budget: DrBudget,
        consent: Option<DrConsent>,
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

/// One live run's child, kept so `dr_abort` can kill it. The poll loop and
/// the abort path share it through a tokio mutex (try_wait/kill need `&mut`).
static CHILDREN: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<tokio::process::Child>>>>>
    = OnceLock::new();

fn children() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<tokio::process::Child>>>> {
    CHILDREN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start a deep-research run by spawning the verb. Returns immediately; the
/// run proceeds on background tasks and progress lands on the job-scoped
/// channel.
#[tauri::command]
pub async fn dr_start(
    app: AppHandle,
    question: String,
    options: DrStartOptions,
) -> Result<DrRunHandle, String> {
    let cli = resolve_cli().ok_or_else(|| {
        "the deep-research CLI verb is not installed — probed SOVEREIGN_CLI_PATH, \
         PATH (sovereign, sovereign-cli), ~/.local/bin/sovereign"
            .to_string()
    })?;
    if question.trim().is_empty() && options.resume_run_id.is_none() {
        return Err("a question is required (or a run to resume)".to_string());
    }
    // The typed consent grant (default-deny): an untyped grant sends no
    // `--consent` flag; an unknown class is refused here before spawn.
    let consent = match options.consent.as_deref() {
        None => None,
        Some(floor) => consent_flag(floor)?,
    };

    let base = runs_base();
    std::fs::create_dir_all(&base).map_err(|e| format!("run dir base {base:?}: {e}"))?;

    let mut cmd = tokio::process::Command::new(&cli);
    cmd.arg("deep-research");
    if !question.trim().is_empty() {
        cmd.arg(question.trim());
    }
    cmd.arg("--run-dir").arg(&base);
    if let Some(n) = options.max_rounds {
        cmd.arg("--max-rounds").arg(n.to_string());
    }
    if let Some(n) = options.search {
        cmd.arg("--search").arg(n.to_string());
    }
    if let Some(n) = options.fetch {
        cmd.arg("--fetch").arg(n.to_string());
    }
    if !options.corpora.is_empty() {
        cmd.arg("--corpora").arg(options.corpora.join(","));
    }
    if let Some(floor) = consent {
        cmd.arg("--consent").arg(floor);
    }
    if let Some(run_id) = &options.resume_run_id {
        cmd.arg("--resume").arg(run_id);
    }
    // The demo film's pass-through (order deep-research-t3b, evidence pass
    // (f)): `SOVEREIGN_DEMO_DR_FLAGS` appends the verb's OWN flags — e.g.
    // `--backend mock --mock-deck DIR` — so the recorded run can be served
    // from a deterministic deck. The desktop stays a driver: every token
    // goes through the verb's closed-set parsing and run-dir verification.
    // Only tests/e2e/demo's global-setup sets it; every real flow leaves it
    // unset and the argument list is byte-identical to today's.
    cmd.args(demo_extra_args());
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {} deep-research: {e}", cli.display()))?;
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let job_id = child.id().expect("a spawned child has an id").to_string();
    let child_arc = Arc::new(tokio::sync::Mutex::new(child));
    children()
        .lock()
        .expect("children mutex")
        .insert(job_id.clone(), Arc::clone(&child_arc));
    let channel = progress_channel(&job_id);

    // The verb's stdout tail (its summary) — the failure path names it.
    let tail_buf: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        use tokio::io::AsyncBufReadExt;
        let buf = Arc::clone(&tail_buf);
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let mut b = buf.lock().unwrap();
                b.push(l);
                if b.len() > 40 {
                    b.remove(0);
                }
            }
        });
    }

    let app_runner = app.clone();
    let channel_runner = channel.clone();
    let base_runner = base.clone();
    let job_runner = job_id.clone();

    tokio::spawn(async move {
        // The verb's stderr names its run dir before the loop opens it — the
        // verb's own naming is the run-id discovery (fallback: a new dr-* dir
        // under the base, so a stderr wording change degrades to discovery
        // rather than a silent miss).
        let run_dir = match await_run_dir(stderr, &base_runner).await {
            Ok(d) => d,
            Err(e) => {
                let _ = app_runner.emit(&channel_runner, DeepResearchRunEvent::Failed { error: e });
                return;
            }
        };
        let run_id = run_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let _ = app_runner.emit(
            &channel_runner,
            DeepResearchRunEvent::Started {
                run_id,
                run_dir: run_dir.display().to_string(),
            },
        );

        // Poll the run dir for live state while the verb runs; on exit,
        // read the report (or fail with the stdout tail).
        let poller = RunDirPoller::new(run_dir.clone());
        let mut last: Option<DrLiveSnapshot> = None;
        let mut exited: Option<std::process::ExitStatus> = None;
        loop {
            if let Some(status) = exited {
                if poller.report_md().is_some() {
                    match build_report(&run_dir) {
                        Some(report) => {
                            let _ = app_runner.emit(
                                &channel_runner,
                                DeepResearchRunEvent::ReportReady { report },
                            );
                        }
                        None => {
                            let _ = app_runner.emit(
                                &channel_runner,
                                DeepResearchRunEvent::Failed {
                                    error: "report.md exists but its artifacts failed to parse"
                                        .to_string(),
                                },
                            );
                        }
                    }
                } else {
                    // Let the stdout drainer catch up before reading the tail.
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    let tail = tail_buf.lock().unwrap().join(" | ");
                    let _ = app_runner.emit(
                        &channel_runner,
                        DeepResearchRunEvent::Failed {
                            error: format!(
                                "deep-research exited {status} without a report — {tail}"
                            ),
                        },
                    );
                }
                let _ = children().lock().expect("children mutex").remove(&job_runner);
                break;
            }
            emit_if_changed(&app_runner, &channel_runner, poller.snapshot(), &mut last);
            let status = {
                let mut guard = child_arc.lock().await;
                match guard.try_wait() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = app_runner.emit(
                            &channel_runner,
                            DeepResearchRunEvent::Failed { error: format!("wait: {e}") },
                        );
                        return;
                    }
                }
            };
            if let Some(s) = status {
                exited = Some(s);
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    });

    Ok(DrRunHandle { job_id, channel })
}

/// Kill a running deep-research child (the run dir keeps its artifacts; the
/// resume affordance picks up from there once the verb's `--resume` lands).
#[tauri::command]
pub async fn dr_abort(job_id: String) -> Result<(), String> {
    let child = children().lock().expect("children mutex").remove(&job_id);
    match child {
        Some(c) => {
            let mut guard = c.lock().await;
            guard
                .kill()
                .await
                .map_err(|e| format!("kill run {job_id}: {e}"))
        }
        None => Err(format!("no active run {job_id}")),
    }
}

/// Read the verb's stderr until it names its run dir ("deep-research: run dir
/// PATH" — the verb's own naming, printed before the loop opens the dir).
/// Fallback: the first dr-* directory that appears under `base` after spawn
/// (covers a stderr wording change without a silent miss). Fails loudly after
/// a 60s timeout with the stderr tail.
async fn await_run_dir(stderr: tokio::process::ChildStderr, base: &Path) -> Result<PathBuf, String> {
    use tokio::io::AsyncBufReadExt;
    let mut known: std::collections::HashSet<PathBuf> = dir_children(base);
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let mut tail: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        tail.push(l.clone());
                        if tail.len() > 40 { tail.remove(0); }
                        if let Some(p) = l.strip_prefix("deep-research: run dir ") {
                            let named = PathBuf::from(p.trim());
                            if named.is_dir() { return Ok(named); }
                            // The verb names a dir it is about to create —
                            // accept it and wait for the dir to appear.
                            for _ in 0..50 {
                                if named.is_dir() { return Ok(named.clone()); }
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                            return Err(format!(
                                "run dir {named:?} never appeared — {}",
                                tail.join(" | ")
                            ));
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(format!("stderr read: {e} — {}", tail.join(" | "))),
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                let now = dir_children(base);
                for p in now.difference(&known) {
                    if p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|n| n.starts_with("dr-"))
                    {
                        return Ok(p.clone());
                    }
                }
                known = now;
                if tokio::time::Instant::now() > deadline {
                    return Err(format!(
                        "the verb never named its run dir (60s) — stderr tail: {}",
                        tail.join(" | ")
                    ));
                }
            }
        }
    }
    Err(format!(
        "verb exited before naming its run dir — {}",
        tail.join(" | ")
    ))
}

fn dir_children(base: &Path) -> std::collections::HashSet<PathBuf> {
    std::fs::read_dir(base)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

// ── Live snapshot ──────────────────────────────────────────────────────────

/// Everything the live view shows, read from the run dir (the single state
/// source). `None` before the charter exists (the loop writes it first).
#[derive(Debug, Clone, PartialEq)]
struct DrLiveSnapshot {
    round: Option<u32>,
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
                            .map(|g| DrGap { id: g.id, text: g.text })
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
        let consent = charter.charter.consent.map(|c| DrConsent {
            release_floor: c.release_floor.as_str().to_string(),
            granted_at_unix: c.granted_at_unix,
        });

        Some(DrLiveSnapshot {
            round,
            stage,
            gaps,
            budget,
            consent,
        })
    }
}

fn emit_if_changed(
    app: &AppHandle,
    channel: &str,
    snapshot: Option<DrLiveSnapshot>,
    last: &mut Option<DrLiveSnapshot>,
) {
    if snapshot.is_none() || *last == snapshot {
        return;
    }
    *last = snapshot.clone();
    if let Some(s) = snapshot {
        let _ = app.emit(
            channel,
            DeepResearchRunEvent::Live {
                round: s.round,
                stage: s.stage,
                gaps: s.gaps,
                budget: s.budget,
                consent: s.consent,
            },
        );
    }
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
    pub terminal_state: String,
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
            out.push(DrRunSummary {
                run_id,
                question: charter.as_ref().map(|c| c.question.clone()),
                created_at_unix: charter.as_ref().map(|c| c.created_at_unix),
                terminal_state: manifest
                    .as_ref()
                    .map(|m| m.terminal_state.clone())
                    .unwrap_or_else(|| "interrupted".to_string()),
                rounds: manifest.as_ref().map(|m| m.rounds.len()).unwrap_or(0),
                report_present: dir.join("report.md").is_file(),
                consent: charter
                    .and_then(|c| c.charter.consent)
                    .map(|c| DrConsent {
                        release_floor: c.release_floor.as_str().to_string(),
                        granted_at_unix: c.granted_at_unix,
                    }),
            });
        }
    }
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    Ok(out)
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
        question: charter.as_ref().map(|c| c.question.clone()).unwrap_or_default(),
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
            })
            .unwrap(),
        )
        .unwrap();
        let snap = poller.snapshot().unwrap();
        assert_eq!(snap.stage, "checking", "verdict-set — the writing is checked");

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
        assert!(check.violations[0].contains("2024"), "{}", check.violations[0]);
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
    fn consent_flag_refuses_an_unknown_class_and_passes_the_closed_set() {
        assert_eq!(consent_flag("public-web"), Ok(Some("public-web")));
        assert_eq!(consent_flag("peer"), Ok(Some("peer")));
        assert_eq!(consent_flag("personal"), Ok(Some("personal")));
        assert!(consent_flag("everything").is_err(), "a typo must not reach the verb");
    }

    #[test]
    fn demo_extra_args_are_verb_flags_only_when_the_demo_var_is_set() {
        // The env mutation could race a parallel test that reads the var —
        // none does (it is demo-only), but the lock keeps the intent loud.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
        assert!(
            demo_extra_args().is_empty(),
            "unset = byte-identical args; real flows must not gain flags"
        );
        unsafe {
            std::env::set_var(
                "SOVEREIGN_DEMO_DR_FLAGS",
                "--backend mock --mock-deck /tmp/deep-research-deck",
            )
        };
        assert_eq!(
            demo_extra_args(),
            vec!["--backend", "mock", "--mock-deck", "/tmp/deep-research-deck"],
            "the demo pass-through appends the verb's own flags, split on whitespace"
        );
        unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
    }
}
