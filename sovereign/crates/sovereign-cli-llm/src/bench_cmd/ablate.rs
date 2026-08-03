//! `svrn bench enrichment-ablate` — retrieval-utility A/B lane (T1 P0.4).
//!
//! Answers "which enrichment knobs actually improve answers?" by running the
//! SAME bank through the production retrieval pipeline (`eval run
//! --prod-pipeline --isolate`) under a declared knob matrix, one knob toggled
//! at a time against a baseline arm:
//!
//! | arm             | change vs baseline                                  |
//! |-----------------|-----------------------------------------------------|
//! | baseline        | shipped defaults (env force-cleared for isolation)  |
//! | raptor_off      | `SOVEREIGN_RAPTOR_GROUNDING=0`                      |
//! | conv_ppr_off    | `SOVEREIGN_CONV_PPR_WEIGHT=0`                       |
//! | with_atlas      | `--with-atlas <ids>` (only when `--atlas` given)    |
//!
//! Every rep is a SUBPROCESS so the knob env vars are read fresh by the
//! in-process production pipeline (several are cached at construction time —
//! in-process toggling would silently measure a half-applied config).
//!
//! The deliverable includes the honest negative: a knob whose |Δ mean fact
//! ratio| does not clear both the rep spread and the SP2 band (0.02) is
//! reported as NOT SEPARABLE by that bank — that finding routes to T2's
//! golden-authoring decision (P3.1), it is not padded into a win.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Minimum |Δ| that can count as separation even when rep spread is
/// tiny — the SP2 parity band on the summarize banks (1 fact / 8 q ×
/// 5 facts ≈ 0.025, rounded to a floor both banks share).
const SEPARATION_FLOOR: f64 = 0.02;

/// The knob env vars the matrix owns. Force-cleared from every arm's
/// environment (then selectively set) so an operator's ambient shell
/// exports cannot contaminate the baseline.
const MATRIX_ENV: &[&str] = &[
    "SOVEREIGN_RAPTOR_GROUNDING",
    "SOVEREIGN_CONV_PPR_WEIGHT",
    "SOVEREIGN_PREFIX_STATE",
];

/// Which pipeline an arm drives.
///
/// Most knobs are retrieval-side and are measured through
/// `--prod-pipeline` (no synthesis — fastest, deterministic, scores the
/// evidence pool). A knob consumed only during ANSWERING needs
/// `--synth`, which runs routing → retrieval → synthesis and therefore
/// runs the grounding gate. Picking the wrong one is silent: the arm
/// completes and reports a delta of ~0 because the knob's consumer
/// never ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum PipelineMode {
    ProdPipeline,
    Synth,
}

impl PipelineMode {
    fn flag(self) -> &'static str {
        match self {
            Self::ProdPipeline => "--prod-pipeline",
            Self::Synth => "--synth",
        }
    }
}

const HELP: Help = Help {
    command: "svrn bench enrichment-ablate",
    summary: "Retrieval-utility knob ablation over the production pipeline (T1 P0.4).",
    sections: &[
        HelpSection::Usage(
            "svrn bench enrichment-ablate <bank.toml> [<bank2.toml> ...] \
             [--reps N] [--limit N] [--atlas <ids>] [--prefix-state] \
             [--runs-dir <dir>] [--output <json>]",
        ),
        HelpSection::Notes(
            "Runs each bank through `eval run --prod-pipeline --isolate` under the \
             declared knob matrix (baseline / raptor_off / conv_ppr_off / \
             doc_cluster_on / with_atlas when --atlas is given), --reps subprocess \
             reps per arm (default 3). --limit is eval run's retrieval pool size \
             per question (default 30 — the SP2 bench register; NOT a question \
             cap, and starving it changes scores). Prints one joined table — \
             mean fact ratio per (bank, arm), Δ vs baseline, and a SEPARABLE / not \
             separable verdict per knob — and writes the full JSON artifact. A knob \
             the banks cannot separate is reported as exactly that (the honest \
             negative feeds T2's golden-authoring decision). Machine-heavy: budget \
             ~1 min per rep per bank. Wall-clock is reported per arm alongside the \
             fact ratio: SEPARABLE is a QUALITY verdict, and a knob can be \
             quality-neutral while being a large latency win or loss. \
             --prefix-state adds the caller-directed prefix-state pin arms \
             (DEFAULTS_LEDGER, DARK). Those are the expensive shape and are \
             opt-in for that reason: they run --synth (the pin's only consumer is \
             the grounding gate, which runs during ANSWERING, so --prod-pipeline \
             would never exercise it) and they are DAEMON-SIDE (the kill switch is \
             read in the engine, so setting it on the eval subprocess is a no-op). \
             Each such arm bounces a foreground daemon carrying its env, and the \
             run ABORTS if the ON arm shows no pin activity or the OFF arm shows \
             any — either means the arms did not differ and no delta is \
             attributable. Restores the service-managed daemon when done.",
        ),
    ],
};

pub async fn cmd_ablate(rest: &[String]) -> i32 {
    if help::wants_help(rest) {
        help::print(&HELP);
        return 0;
    }
    run(rest).await
}

struct ArmSpec {
    name: &'static str,
    env: Vec<(&'static str, String)>,
    extra_args: Vec<String>,
    /// The knob is read by the DAEMON process, not by the `eval run`
    /// client. Setting it on the subprocess is then a NO-OP and the arm
    /// silently duplicates its baseline — the worst failure an A/B can
    /// have, because it reports "no effect" for a knob that never
    /// changed. Such an arm must bounce the daemon with this env
    /// instead; see [`bounce_daemon`].
    daemon_side: bool,
    mode: PipelineMode,
    /// Arm whose mean this one's delta is measured against. Arms in
    /// different [`PipelineMode`]s are NOT comparable (one scores an
    /// evidence pool, the other scores answers), so a synth arm names a
    /// synth baseline rather than borrowing the prod-pipeline one.
    compare_to: &'static str,
}

impl ArmSpec {
    fn retrieval(name: &'static str, env: Vec<(&'static str, String)>) -> Self {
        Self {
            name,
            env,
            extra_args: vec![],
            daemon_side: false,
            mode: PipelineMode::ProdPipeline,
            compare_to: "baseline",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RepResult {
    fact_ratio: f64,
    source_ratio: f64,
    questions: usize,
    output_path: String,
    /// Wall-clock for this rep's `eval run`. A LATENCY knob's headline
    /// is time, not fact ratio — reporting only quality would score a
    /// pure-latency change as "not separable" and read as no effect.
    /// Recorded for every arm so a quality knob also shows what it cost.
    #[serde(default)]
    wall_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ArmSummary {
    reps: Vec<RepResult>,
    mean_fact: f64,
    min_fact: f64,
    max_fact: f64,
    mean_source: f64,
    mean_wall: f64,
    min_wall: f64,
    max_wall: f64,
}

#[derive(Debug, Serialize)]
struct KnobVerdict {
    arm: String,
    delta_vs_baseline: f64,
    baseline_spread: f64,
    arm_spread: f64,
    separable: bool,
    /// Latency effect. A knob can be quality-neutral and still worth
    /// shipping (or rejecting) on time alone — reporting only the fact
    /// ratio would score a pure-latency change as "not separable",
    /// which reads as "no effect" and is wrong.
    wall_delta_secs: f64,
    wall_speedup: f64,
    /// Which arm the deltas above are relative to.
    compared_to: String,
}

#[derive(Debug, Serialize)]
struct Artifact {
    generated_by: String,
    reps: usize,
    limit: usize,
    atlas: Option<String>,
    /// bank stem → arm name → summary
    results: BTreeMap<String, BTreeMap<String, ArmSummary>>,
    /// bank stem → verdict per non-baseline arm
    verdicts: BTreeMap<String, Vec<KnobVerdict>>,
}

fn summarize(reps: &[RepResult]) -> ArmSummary {
    let n = reps.len().max(1) as f64;
    let mean_fact = reps.iter().map(|r| r.fact_ratio).sum::<f64>() / n;
    let mean_source = reps.iter().map(|r| r.source_ratio).sum::<f64>() / n;
    let min_fact = reps.iter().map(|r| r.fact_ratio).fold(f64::MAX, f64::min);
    let max_fact = reps.iter().map(|r| r.fact_ratio).fold(f64::MIN, f64::max);
    let mean_wall = reps.iter().map(|r| r.wall_secs).sum::<f64>() / n;
    let min_wall = reps.iter().map(|r| r.wall_secs).fold(f64::MAX, f64::min);
    let max_wall = reps.iter().map(|r| r.wall_secs).fold(f64::MIN, f64::max);
    ArmSummary {
        reps: reps.to_vec(),
        mean_fact,
        min_fact: if reps.is_empty() { 0.0 } else { min_fact },
        max_fact: if reps.is_empty() { 0.0 } else { max_fact },
        mean_source,
        mean_wall,
        min_wall: if reps.is_empty() { 0.0 } else { min_wall },
        max_wall: if reps.is_empty() { 0.0 } else { max_wall },
    }
}

// ── Daemon-side arms ────────────────────────────────────────────────
//
// A knob read by the daemon cannot be toggled by setting env on the
// `eval run` subprocess. These helpers run a FOREGROUND daemon
// (`svrn daemon run` — the same entrypoint the service manager invokes)
// with the arm's environment, so the arm is genuinely different from
// its baseline.
//
// Three facts make this fiddlier than "stop, start", each of which cost
// a debugging round on 2026-08-03:
//
//   1. `svrn daemon start` delegates to the service manager, whose
//      plist carries only PATH — env handed to that subprocess never
//      reaches the daemon.
//   2. The macOS plist sets RunAtLoad + KeepAlive{SuccessfulExit:false},
//      so `daemon stop` alone lets launchd bring it right back.
//      `launchctl bootout` is what actually removes it.
//   3. The listener dies tens of seconds before the process does (an
//      18GB model unload is not instant), and `~/.svrnmesh/daemon.lock`
//      is held until the process exits. Waiting on the port therefore
//      races: the new daemon exits 1 with "another daemon already holds
//      the run lock". Wait on the LOCK.

struct ForegroundDaemon {
    child: std::process::Child,
}

impl Drop for ForegroundDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn daemon_lock_path() -> PathBuf {
    dirs_home().join(".svrnmesh").join("daemon.lock")
}

/// The DISPATCHER binary, which is not the one we are running.
///
/// `current_exe()` here is `sovereign-cli-llm`, which owns `eval` and
/// `bench` but NOT `daemon` — that verb belongs to
/// `sovereign-cli-daemon`, reached by exec from the `sovereign-cli`
/// dispatcher. Handing `daemon run` to our own exe fails with
/// "unknown subcommand 'daemon'" (observed 2026-08-03). Look for the
/// dispatcher beside us first, then the deployed symlink.
fn dispatcher_exe(current: &std::path::Path) -> Result<PathBuf, String> {
    if let Some(dir) = current.parent() {
        for name in ["sovereign-cli", "svrn"] {
            let c = dir.join(name);
            if c.exists() {
                return Ok(c);
            }
        }
    }
    for c in [
        dirs_home().join(".local/bin/sovereign"),
        dirs_home().join(".local/bin/svrn"),
    ] {
        if c.exists() {
            return Ok(c);
        }
    }
    Err("cannot find the `sovereign-cli` dispatcher (needed for `daemon` — this \
         binary does not own that verb). Build it: \
         cargo build -p sovereign-cli --features dev-tools"
        .to_string())
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// PIDs holding the run lock. Empty means a new daemon may start.
fn lock_holders() -> Vec<i32> {
    let lock = daemon_lock_path();
    if !lock.exists() {
        return Vec::new();
    }
    std::process::Command::new("lsof")
        .arg("-t")
        .arg(&lock)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

async fn daemon_reachable() -> bool {
    // :9741 has no /healthz (it 404s) — /v1/models is the liveness probe.
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reqwest::get("http://127.0.0.1:9741/v1/models"),
        )
        .await,
        Ok(Ok(r)) if r.status().is_success()
    )
}

/// Stop whatever daemon is running and wait until the run lock is free.
async fn quiesce_daemon(dispatcher: &std::path::Path) -> Result<(), String> {
    let _ = std::process::Command::new(dispatcher)
        .args(["daemon", "stop"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl")
            .arg("bootout")
            .arg(format!("gui/{uid}/com.svrnmesh.daemon"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    for _ in 0..180 {
        if lock_holders().is_empty() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(format!(
        "run lock still held after 180s by pid(s) {:?}",
        lock_holders()
    ))
}

/// Restore the service-managed daemon the operator normally runs.
fn restore_service_daemon(dispatcher: &std::path::Path) {
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc::getuid() };
        let plist = dirs_home().join("Library/LaunchAgents/com.svrnmesh.daemon.plist");
        if plist.exists() {
            let _ = std::process::Command::new("launchctl")
                .arg("bootstrap")
                .arg(format!("gui/{uid}"))
                .arg(&plist)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            return;
        }
    }
    let _ = std::process::Command::new(dispatcher)
        .args(["daemon", "start"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Bring up a foreground daemon carrying `env`, logging to `log_path`.
async fn bounce_daemon(
    dispatcher: &std::path::Path,
    env: &[(&'static str, String)],
    log_path: &std::path::Path,
) -> Result<ForegroundDaemon, String> {
    quiesce_daemon(dispatcher).await?;
    let log = std::fs::File::create(log_path)
        .map_err(|e| format!("create {}: {e}", log_path.display()))?;
    let errlog = log
        .try_clone()
        .map_err(|e| format!("clone log handle: {e}"))?;
    let mut cmd = std::process::Command::new(dispatcher);
    cmd.args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(errlog);
    // Force-clear the matrix so an operator's ambient shell cannot
    // contaminate a daemon-side arm, then set this arm's knobs.
    for k in MATRIX_ENV {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn daemon run: {e}"))?;
    let mut fg = ForegroundDaemon { child };
    for _ in 0..200 {
        if let Ok(Some(status)) = fg.child.try_wait() {
            return Err(format!(
                "daemon exited {status} during startup — see {}",
                log_path.display()
            ));
        }
        if daemon_reachable().await {
            return Ok(fg);
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    Err(format!(
        "daemon not reachable within 600s — see {}",
        log_path.display()
    ))
}

/// Count of pin engagements in a daemon log — the proof that a
/// prefix-state arm actually differed from its baseline. Literal,
/// case-sensitive strings from `model_slot.rs`; the 2026-07-21 soak
/// reported in exactly these terms ("76 LEARNED / 253 HIT / 0 WARN").
fn pin_activity(log_path: &std::path::Path) -> (usize, usize) {
    let raw = std::fs::read_to_string(log_path).unwrap_or_default();
    (
        raw.matches("prefix_state: LEARNED").count(),
        raw.matches("prefix_state: HIT").count(),
    )
}

fn parse_eval_output(path: &std::path::Path) -> Result<RepResult, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| format!("{}: no results array", path.display()))?;
    if results.is_empty() {
        return Err(format!("{}: zero questions scored", path.display()));
    }
    let ratio_of = |q: &serde_json::Value, key: &str| -> f64 {
        q.get(key)
            .and_then(|s| s.get("ratio"))
            .and_then(|r| r.as_f64())
            .unwrap_or(0.0)
    };
    let n = results.len() as f64;
    Ok(RepResult {
        fact_ratio: results.iter().map(|q| ratio_of(q, "fact_score")).sum::<f64>() / n,
        source_ratio: results.iter().map(|q| ratio_of(q, "source_score")).sum::<f64>() / n,
        questions: results.len(),
        output_path: path.display().to_string(),
        wall_secs: 0.0, // filled by the caller, which owns the clock
    })
}

async fn run(rest: &[String]) -> i32 {
    let mut banks: Vec<PathBuf> = Vec::new();
    let mut reps: usize = 3;
    let mut limit: usize = 30;
    let mut atlas: Option<String> = None;
    let mut prefix_state = false;
    let mut runs_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--reps" => match val!("--reps").parse() {
                Ok(v) if v > 0 => reps = v,
                _ => {
                    eprintln!("error: --reps must be a positive integer");
                    return 2;
                }
            },
            "--limit" => match val!("--limit").parse() {
                Ok(v) if v > 0 => limit = v,
                _ => {
                    eprintln!("error: --limit must be a positive integer");
                    return 2;
                }
            },
            "--atlas" => atlas = Some(val!("--atlas")),
            "--prefix-state" => prefix_state = true,
            "--runs-dir" => runs_dir = Some(PathBuf::from(val!("--runs-dir"))),
            "--output" => output = Some(PathBuf::from(val!("--output"))),
            "--help" | "-h" => {
                help::print(&HELP);
                return 0;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
            bank => banks.push(PathBuf::from(bank)),
        }
        i += 1;
    }
    if banks.is_empty() {
        eprintln!("error: at least one <bank.toml> is required");
        help::print(&HELP);
        return 2;
    }
    for b in &banks {
        if !b.exists() {
            eprintln!("error: bank not found: {}", b.display());
            return 2;
        }
    }

    let mut arms: Vec<ArmSpec> = vec![
        ArmSpec::retrieval("baseline", vec![]),
        ArmSpec::retrieval("raptor_off", vec![("SOVEREIGN_RAPTOR_GROUNDING", "0".into())]),
        ArmSpec::retrieval("conv_ppr_off", vec![("SOVEREIGN_CONV_PPR_WEIGHT", "0".into())]),
    ];
    if let Some(ids) = &atlas {
        arms.push(ArmSpec {
            name: "with_atlas",
            env: vec![],
            extra_args: vec!["--with-atlas".into(), ids.clone()],
            daemon_side: false,
            mode: PipelineMode::ProdPipeline,
            compare_to: "baseline",
        });
    }
    if prefix_state {
        // The caller-directed prefix-state pin (DEFAULTS_LEDGER, DARK).
        //
        // Opt-in because it is the expensive shape: SYNTH (its only
        // consumer is the grounding gate, which runs during answering)
        // and DAEMON-SIDE (the kill switch is read in the engine), so
        // each arm bounces the daemon and each rep runs full synthesis.
        //
        // Its own paired baseline: a synth arm cannot be measured
        // against the prod-pipeline baseline above, and an explicit
        // `=0` arm proves the OFF side rather than assuming the
        // ambient default is off.
        arms.push(ArmSpec {
            name: "prefix_state_off",
            env: vec![("SOVEREIGN_PREFIX_STATE", "0".into())],
            extra_args: vec![],
            daemon_side: true,
            mode: PipelineMode::Synth,
            compare_to: "prefix_state_off",
        });
        arms.push(ArmSpec {
            name: "prefix_state_on",
            env: vec![("SOVEREIGN_PREFIX_STATE", "1".into())],
            extra_args: vec![],
            daemon_side: true,
            mode: PipelineMode::Synth,
            compare_to: "prefix_state_off",
        });
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: current_exe: {e}");
            return 1;
        }
    };
    // `eval` is ours; `daemon` is not. Resolve the dispatcher up front
    // and only when a daemon-side arm is actually in the matrix, so a
    // plain retrieval ablation never needs it present.
    let dispatcher = if arms.iter().any(|a| a.daemon_side) {
        match dispatcher_exe(&exe) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        None
    };
    let runs_dir = runs_dir.unwrap_or_else(|| PathBuf::from("target/ci-bench/enrichment-ablate"));
    if let Err(e) = std::fs::create_dir_all(&runs_dir) {
        eprintln!("error: create {}: {e}", runs_dir.display());
        return 1;
    }

    let total = banks.len() * arms.len() * reps;
    eprintln!(
        "enrichment-ablate: {} bank(s) × {} arm(s) × {reps} rep(s) = {total} eval runs \
         (~1 min each — this is the machine-heavy lane)",
        banks.len(),
        arms.len(),
    );

    let mut results: BTreeMap<String, BTreeMap<String, ArmSummary>> = BTreeMap::new();
    let mut failures = 0usize;
    let mut done = 0usize;
    for bank in &banks {
        let stem = bank
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| bank.display().to_string());
        for arm in &arms {
            let mut rep_results: Vec<RepResult> = Vec::new();
            // Bounce ONCE per arm, not per rep — a daemon restart costs
            // a cold model load (57-95s for an 18GB primary), and the
            // knob is process-scoped, so per-rep bouncing would buy
            // nothing and dominate the measurement.
            let mut _daemon_guard: Option<ForegroundDaemon> = None;
            if arm.daemon_side {
                let dlog = runs_dir.join(format!("{stem}-{}-daemon.log", arm.name));
                eprintln!(
                    "  [daemon-side arm {}] bouncing daemon with {:?}",
                    arm.name, arm.env
                );
                let disp = dispatcher.as_deref().expect("daemon-side arm implies dispatcher resolved above");
                match bounce_daemon(disp, &arm.env, &dlog).await {
                    Ok(g) => _daemon_guard = Some(g),
                    Err(e) => {
                        // Abort rather than fall through to the ambient
                        // daemon: that would silently run this arm as a
                        // duplicate of its baseline and report "no
                        // effect" for a knob that never changed.
                        eprintln!("error: {} — cannot run arm {}", e, arm.name);
                        if let Some(d) = dispatcher.as_deref() { restore_service_daemon(d); }
                        return 1;
                    }
                }
            }
            for r in 1..=reps {
                let out_json = runs_dir.join(format!("{stem}-{}-r{r}.json", arm.name));
                let log_path = runs_dir.join(format!("{stem}-{}-r{r}.log", arm.name));
                let mut cmd = tokio::process::Command::new(&exe);
                cmd.arg("eval")
                    .arg("run")
                    .arg("--bank")
                    .arg(bank)
                    .arg(arm.mode.flag())
                    .arg("--isolate")
                    .arg("--limit")
                    .arg(limit.to_string())
                    .arg("--format")
                    .arg("json")
                    .arg("--output")
                    .arg(&out_json);
                for a in &arm.extra_args {
                    cmd.arg(a);
                }
                for k in MATRIX_ENV {
                    cmd.env_remove(k);
                }
                for (k, v) in &arm.env {
                    cmd.env(k, v);
                }
                done += 1;
                eprintln!("  [{done}/{total}] {stem} {} r{r} …", arm.name);
                let t0 = std::time::Instant::now();
                match cmd.output().await {
                    Ok(out) => {
                        let _ = std::fs::write(
                            &log_path,
                            [&out.stdout[..], &out.stderr[..]].concat(),
                        );
                        if !out.status.success() {
                            eprintln!(
                                "    FAIL (exit {:?}) — log: {}",
                                out.status.code(),
                                log_path.display()
                            );
                            failures += 1;
                            continue;
                        }
                    }
                    Err(e) => {
                        eprintln!("    FAIL spawn: {e}");
                        failures += 1;
                        continue;
                    }
                }
                let wall = t0.elapsed().as_secs_f64();
                match parse_eval_output(&out_json) {
                    Ok(mut rep) => {
                        rep.wall_secs = wall;
                        eprintln!("        {:.1}s", wall);
                        rep_results.push(rep);
                    }
                    Err(e) => {
                        eprintln!("    FAIL parse: {e}");
                        failures += 1;
                    }
                }
            }
            // A daemon-side arm that shows no pin activity did not
            // differ from its baseline — report that rather than a
            // delta nobody can attribute (ARCH_PRINCIPLES §18.3).
            if arm.daemon_side && arm.name.starts_with("prefix_state") {
                let dlog = runs_dir.join(format!("{stem}-{}-daemon.log", arm.name));
                let (learned, hit) = pin_activity(&dlog);
                eprintln!(
                    "  [{}] pin activity: LEARNED={learned} HIT={hit}",
                    arm.name
                );
                let wants_pin = arm.env.iter().any(|(k, v)| {
                    *k == "SOVEREIGN_PREFIX_STATE" && (v == "1" || v == "true" || v == "on")
                });
                if wants_pin && learned == 0 && hit == 0 {
                    eprintln!(
                        "error: arm `{}` asked for the pin and the daemon never pinned \
                         anything — the arms are identical and any delta is noise. \
                         Check {} (arch veto? gate not running?).",
                        arm.name,
                        dlog.display()
                    );
                    if let Some(d) = dispatcher.as_deref() { restore_service_daemon(d); }
                    return 1;
                }
                if !wants_pin && (learned > 0 || hit > 0) {
                    eprintln!(
                        "error: arm `{}` disabled the pin and the daemon pinned anyway — \
                         the env did not reach the daemon; results are not attributable.",
                        arm.name
                    );
                    if let Some(d) = dispatcher.as_deref() { restore_service_daemon(d); }
                    return 1;
                }
            }
            if rep_results.is_empty() {
                eprintln!(
                    "error: arm `{}` on bank `{stem}` produced zero successful reps — \
                     the joined table would silently misreport this knob; aborting",
                    arm.name
                );
                return 1;
            }
            results
                .entry(stem.clone())
                .or_default()
                .insert(arm.name.to_string(), summarize(&rep_results));
        }
    }

    // Give the operator their normal daemon back before reporting. The
    // last arm's foreground daemon has dropped by here; without this the
    // box would be left with NO daemon at all, which is a worse outcome
    // than the ablation failing.
    if let Some(d) = dispatcher.as_deref() {
        eprintln!("  restoring the service-managed daemon");
        restore_service_daemon(d);
    }

    // Verdicts: each arm against the arm it NAMES as its comparison
    // point. Most name "baseline"; a synth arm names a synth baseline,
    // because a fact ratio over an evidence pool and one over generated
    // answers are different quantities and differencing them is
    // meaningless.
    let compare_of: BTreeMap<&str, &str> =
        arms.iter().map(|a| (a.name, a.compare_to)).collect();
    let mut verdicts: BTreeMap<String, Vec<KnobVerdict>> = BTreeMap::new();
    for (stem, by_arm) in &results {
        let mut rows = Vec::new();
        for (arm_name, s) in by_arm {
            let base_name = compare_of
                .get(arm_name.as_str())
                .copied()
                .unwrap_or("baseline");
            if base_name == arm_name {
                continue; // this arm IS the comparison point
            }
            let Some(base) = by_arm.get(base_name) else {
                continue;
            };
            let base_spread = base.max_fact - base.min_fact;
            let delta = s.mean_fact - base.mean_fact;
            let arm_spread = s.max_fact - s.min_fact;
            let separable = delta.abs() > SEPARATION_FLOOR.max(base_spread).max(arm_spread);
            rows.push(KnobVerdict {
                arm: arm_name.clone(),
                delta_vs_baseline: delta,
                baseline_spread: base_spread,
                arm_spread,
                separable,
                wall_delta_secs: s.mean_wall - base.mean_wall,
                wall_speedup: if s.mean_wall > 0.0 {
                    base.mean_wall / s.mean_wall
                } else {
                    0.0
                },
                compared_to: base_name.to_string(),
            });
        }
        verdicts.insert(stem.clone(), rows);
    }

    println!();
    println!("  Enrichment knob ablation — quality (mean fact ratio) and latency");
    println!("  ──────────────────────────────────────────────────────────────────────────");
    println!("  bank                    arm              fact    Δ vs base   wall     Δ wall   verdict");
    for (stem, by_arm) in &results {
        for (arm_name, s) in by_arm {
            let v = verdicts
                .get(stem)
                .and_then(|rows| rows.iter().find(|r| &r.arm == arm_name));
            let (delta_str, wall_delta_str, verdict_str) = match v {
                None => ("       —".to_string(), "      —".to_string(), String::new()),
                Some(r) => (
                    format!("{:+.4}", r.delta_vs_baseline),
                    format!("{:+.0}s", r.wall_delta_secs),
                    if r.separable {
                        "SEPARABLE".to_string()
                    } else {
                        "not separable".to_string()
                    },
                ),
            };
            println!(
                "  {stem:<22}  {arm_name:<15}  {:.4}  {delta_str}   {:>6.0}s  {wall_delta_str}   {verdict_str}",
                s.mean_fact, s.mean_wall
            );
        }
    }
    // Latency is reported separately because the SEPARABLE verdict above
    // is a QUALITY verdict. A knob can be quality-neutral (not separable)
    // and still be the largest latency win available — collapsing both
    // into one verdict would hide exactly that case.
    for (stem, rows) in &verdicts {
        for r in rows {
            if r.wall_speedup > 0.0 && (r.wall_speedup >= 1.10 || r.wall_speedup <= 0.91) {
                println!(
                    "  latency: {stem} {} is {:.2}x vs {} ({:+.0}s per rep)",
                    r.arm, r.wall_speedup, r.compared_to, r.wall_delta_secs
                );
            }
        }
    }
    if failures > 0 {
        println!("  ({failures} failed rep(s) excluded — see logs in {})", runs_dir.display());
    }

    let artifact = Artifact {
        generated_by: "svrn bench enrichment-ablate".to_string(),
        reps,
        limit,
        atlas,
        results,
        verdicts,
    };
    let out_path =
        output.unwrap_or_else(|| PathBuf::from("target/ci-bench/enrichment-ablate.json"));
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&out_path, serde_json::to_string_pretty(&artifact).unwrap()) {
        Ok(_) => println!("\n  ✓ wrote {}", out_path.display()),
        Err(e) => {
            eprintln!("error: writing {}: {e}", out_path.display());
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(fact: f64) -> RepResult {
        rep_at(fact, 0.0)
    }

    fn rep_at(fact: f64, wall_secs: f64) -> RepResult {
        RepResult {
            fact_ratio: fact,
            source_ratio: 1.0,
            questions: 8,
            output_path: String::new(),
            wall_secs,
        }
    }

    #[test]
    fn summarize_reports_wall_clock_alongside_quality() {
        // A latency knob's headline is time. Before wall_secs existed,
        // a pure-latency change scored "not separable" on fact ratio
        // and read as no effect.
        let s = summarize(&[rep_at(0.62, 100.0), rep_at(0.62, 200.0), rep_at(0.62, 300.0)]);
        assert!((s.mean_wall - 200.0).abs() < 1e-9);
        assert!((s.min_wall - 100.0).abs() < 1e-9);
        assert!((s.max_wall - 300.0).abs() < 1e-9);
    }

    #[test]
    fn synth_arms_compare_to_their_own_baseline_not_the_prod_pipeline_one() {
        // A fact ratio over an evidence pool and one over generated
        // answers are different quantities; differencing them is
        // meaningless. Each arm names its comparison point.
        let arms = [
            ArmSpec::retrieval("baseline", vec![]),
            ArmSpec {
                name: "prefix_state_off",
                env: vec![("SOVEREIGN_PREFIX_STATE", "0".into())],
                extra_args: vec![],
                daemon_side: true,
                mode: PipelineMode::Synth,
                compare_to: "prefix_state_off",
            },
            ArmSpec {
                name: "prefix_state_on",
                env: vec![("SOVEREIGN_PREFIX_STATE", "1".into())],
                extra_args: vec![],
                daemon_side: true,
                mode: PipelineMode::Synth,
                compare_to: "prefix_state_off",
            },
        ];
        let compare_of: BTreeMap<&str, &str> =
            arms.iter().map(|a| (a.name, a.compare_to)).collect();
        assert_eq!(compare_of["prefix_state_on"], "prefix_state_off");
        // Self-referential arms are the comparison point and yield no verdict row.
        assert_eq!(compare_of["prefix_state_off"], "prefix_state_off");
        assert_eq!(compare_of["baseline"], "baseline");
    }

    #[test]
    fn pipeline_mode_picks_the_right_eval_flag() {
        // Picking the wrong one is SILENT: --prod-pipeline runs no
        // synthesis, so a gate-consumed knob's consumer never runs and
        // the arm reports ~0 delta.
        assert_eq!(PipelineMode::ProdPipeline.flag(), "--prod-pipeline");
        assert_eq!(PipelineMode::Synth.flag(), "--synth");
    }

    #[test]
    fn prefix_state_is_force_cleared_from_every_arm() {
        // An operator's ambient SOVEREIGN_PREFIX_STATE would otherwise
        // contaminate the OFF arm and silently make both arms identical.
        assert!(MATRIX_ENV.contains(&"SOVEREIGN_PREFIX_STATE"));
    }

    #[test]
    fn summarize_reports_mean_and_spread() {
        let s = summarize(&[rep(0.60), rep(0.62), rep(0.64)]);
        assert!((s.mean_fact - 0.62).abs() < 1e-9);
        assert!((s.min_fact - 0.60).abs() < 1e-9);
        assert!((s.max_fact - 0.64).abs() < 1e-9);
    }

    #[test]
    fn separation_requires_clearing_floor_and_spread() {
        // Δ = 0.015 < floor 0.02 → not separable even with zero spread.
        let delta: f64 = 0.015;
        assert!(delta.abs() <= SEPARATION_FLOOR.max(0.0).max(0.0));
        // Δ = 0.05 with spreads 0.01 → separable.
        let delta2: f64 = 0.05;
        assert!(delta2.abs() > SEPARATION_FLOOR.max(0.01).max(0.01));
        // Δ = 0.05 but baseline spread 0.06 (noisier than the effect) → not separable.
        assert!(delta2.abs() <= SEPARATION_FLOOR.max(0.06).max(0.01));
    }
}
