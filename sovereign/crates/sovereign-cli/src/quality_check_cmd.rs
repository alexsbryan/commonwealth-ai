// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn quality check` — the curated 30-minute read on whether the resident
//! stack is BROKEN. Not whether it drifted; that is the nightly's job.
//!
//! # Why a second runner when `sovereign-ci-bench.sh` exists
//!
//! Three measured reasons, none of them "the old one is ugly":
//!
//! 1. **ci-bench keeps nothing.** `target/ci-bench` is empty: the lane table
//!    is echoed to a terminal and discarded, so "was this lane slower last
//!    week" has no answer on this host. Every run here writes
//!    `target/quality-check/<stamp>/summary.json` with per-lane seconds.
//! 2. **It reconstructs verdicts by grepping lane prose.**
//!    `scripts/lib/ci-bench-verdict.sh` is 130 lines of `grep -qE` against
//!    wording no lane promised to keep — including the daemon's own
//!    unreachability strings. Here the lane SAYS its verdict, as a
//!    [`Judgement`] on its last stdout line
//!    (`sovereign_cli_shared::lane_verdict`).
//! 3. **It is `--quick` in name only.** The lean tier is this command; the
//!    script keeps the full nightly.
//!
//! # The four verdicts are the whole design (ARCH §18.1, §18.2)
//!
//! - **passed** — the lane ran and every assertion held.
//! - **failed** — the lane ran and something did not.
//! - **could-not-judge** — a precondition was missing, the budget ran out
//!   before the lane could start, or the lane itself could not reach a
//!   verdict. Never a pass; a HARD lane goes red on it, because "suddenly
//!   nothing to judge" on a gated surface is a regression signal.
//! - **never-ran** — the lane produced no verdict line at all. The exit code
//!   is the reason. This is the one an exit-code-only runner cannot express,
//!   and it is the one a crashed lane earns.
//!
//! # What this command will not do
//!
//! It will not write a baseline. A run whose stack has no baseline for its
//! [`Fingerprint`] is `could-not-judge (first-run)` on its BASELINE-DERIVED
//! rows and writes nothing; `--mint` is the only door. Absolute rows —
//! pre-registered ceilings, a gate outcome, a usefulness bar — need no
//! baseline and are judged on the run in front of them, which is why the
//! `chat-ask` lane can fail on its very first run instead of reporting
//! first-run and teaching nobody anything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use kernel_types::{honesty_footer, render_rows, Judgement, Reason};
use sovereign_cli_shared::lane_verdict;

/// Default wall budget. The operator's number: "should take no longer than
/// 30 minutes".
const DEFAULT_BUDGET_SECS: u64 = 1800;

/// A lane needs at least this much runway to be worth starting. Verbatim
/// from `sovereign-ci-bench.sh::run_lane` — below it the lane would be
/// killed mid-flight and report a TIMEOUT that says nothing about the code.
const MIN_LANE_RUNWAY_SECS: u64 = 60;

/// How often the runner checks on a lane subprocess.
const POLL: Duration = Duration::from_millis(250);

// ─── The lane table (config as data, ARCH §6) ───────────────────────

/// Where the lane table lives, relative to the repo root.
const LANE_TABLE: &str = "quality/check-lanes.toml";

/// How a lane's verdict is allowed to affect the run's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enforcement {
    /// Not passed → the run fails.
    Hard,
    /// Recorded in the table; never breaks the build.
    Soft,
    /// Recorded and trended; never breaks the build. Same effect as `Soft`
    /// on the exit code and a DIFFERENT claim to a reader: soft means "we
    /// chose not to gate on this", tracked means "there is no band yet".
    Tracked,
}

impl Enforcement {
    fn parse(s: &str) -> Option<Enforcement> {
        match s {
            "hard" => Some(Enforcement::Hard),
            "soft" => Some(Enforcement::Soft),
            "tracked" => Some(Enforcement::Tracked),
            _ => None,
        }
    }
    const fn as_str(self) -> &'static str {
        match self {
            Enforcement::Hard => "hard",
            Enforcement::Soft => "soft",
            Enforcement::Tracked => "tracked",
        }
    }
}

/// What a lane asserts about. Carried into `summary.json` so a reader can
/// tell a lane that checks an INVARIANT (a ceiling, a route, an absence)
/// from one whose verdict came out of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneKind {
    Invariant,
    Judged,
}

impl LaneKind {
    fn parse(s: &str) -> Option<LaneKind> {
        match s {
            "invariant" => Some(LaneKind::Invariant),
            "judged" => Some(LaneKind::Judged),
            _ => None,
        }
    }
    const fn as_str(self) -> &'static str {
        match self {
            LaneKind::Invariant => "invariant",
            LaneKind::Judged => "judged",
        }
    }
}

/// A CLOSED set of things a lane can require before it is worth running
/// (ARCH §2 — closed sets are enums). An open `Vec<String>` of shell
/// snippets is how a precondition plane becomes a second, untested
/// orchestrator; this one is five variants and each has exactly one probe.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Precondition {
    /// Something accepts TCP on this loopback port.
    PortListening(u16),
    /// The named slot alias completes one token RIGHT NOW.
    ///
    /// A real decode, not `/v1/models`'s `loaded: true`. The models route
    /// echoes what the daemon believes about itself, and a guard that
    /// asserts on a field its subject supplies is not a guard (ARCH §18.1) —
    /// a wedged slot advertises `loaded` exactly as a healthy one does.
    SlotDecodes(String),
    /// The corpus is on disk with an index. Probed by the store's own
    /// accessor (`sovereign_enrichment_catalog::corpus_state`).
    CorpusInstalled(String),
    /// An executable of this name is reachable — beside the running
    /// dispatcher first, then on PATH.
    Binary(String),
}

impl Precondition {
    /// `port-listening:9741`, `slot-decodes:primary`,
    /// `corpus-installed:sep`, `binary:sovereign-cli-llm`.
    fn parse(s: &str) -> Result<Precondition, String> {
        let (head, arg) = s
            .split_once(':')
            .ok_or_else(|| format!("`{s}` is not `<kind>:<arg>`"))?;
        let arg = arg.trim();
        if arg.is_empty() {
            return Err(format!("`{s}` has an empty argument"));
        }
        match head.trim() {
            "port-listening" => arg
                .parse::<u16>()
                .map(Precondition::PortListening)
                .map_err(|_| format!("`{arg}` is not a port")),
            "slot-decodes" => Ok(Precondition::SlotDecodes(arg.to_string())),
            "corpus-installed" => Ok(Precondition::CorpusInstalled(arg.to_string())),
            "binary" => Ok(Precondition::Binary(arg.to_string())),
            other => Err(format!(
                "`{other}` is not a precondition kind (port-listening, \
                 slot-decodes, corpus-installed, binary)"
            )),
        }
    }

    /// How this precondition reads in a could-not-judge reason.
    fn describe(&self) -> String {
        match self {
            Precondition::PortListening(p) => format!("nothing is listening on 127.0.0.1:{p}"),
            Precondition::SlotDecodes(s) => {
                format!("slot `{s}` did not decode a token — is the model resident?")
            }
            Precondition::CorpusInstalled(c) => format!("corpus `{c}` is not installed"),
            Precondition::Binary(b) => format!("binary `{b}` is not on this host"),
        }
    }
}

/// One declared lane.
#[derive(Debug, Clone)]
struct LaneSpec {
    id: String,
    kind: LaneKind,
    enforcement: Enforcement,
    est_secs: u64,
    command: Vec<String>,
    preconditions: Vec<Precondition>,
    /// Directory holding this lane's per-fingerprint baselines, repo-relative.
    /// `None` for a lane that compares against nothing.
    baseline_dir: Option<PathBuf>,
    /// The lane's bank, repo-relative. Hashed into the run fingerprint —
    /// change the questions and last week's numbers stop being comparable.
    bank: Option<PathBuf>,
}

/// Parse the lane table. A malformed entry REFUSES the whole file rather
/// than being skipped: a lane silently dropped from a check is a lane whose
/// absence reads as a pass.
fn parse_lane_table(text: &str) -> Result<Vec<LaneSpec>, String> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| format!("{LANE_TABLE}: {e}"))?;
    let lanes = doc
        .get("lane")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{LANE_TABLE}: no [[lane]] entries"))?;
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (i, l) in lanes.iter().enumerate() {
        let at = |k: &str| format!("{LANE_TABLE}: [[lane]] #{}: {k}", i + 1);
        let id = l
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| at("no `id`"))?
            .to_string();
        if seen.contains(&id) {
            // One decider, one name (ARCH §10.6). Two lanes of one id means
            // one of them silently loses its row in the table.
            return Err(format!("{LANE_TABLE}: lane id `{id}` is declared twice"));
        }
        seen.push(id.clone());
        let kind = l
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(LaneKind::parse)
            .ok_or_else(|| at("`kind` must be `invariant` or `judged`"))?;
        let enforcement = l
            .get("enforcement")
            .and_then(|v| v.as_str())
            .and_then(Enforcement::parse)
            .ok_or_else(|| at("`enforcement` must be `hard`, `soft` or `tracked`"))?;
        let est_secs = l
            .get("est_secs")
            .and_then(toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| at("`est_secs` must be a non-negative integer"))?;
        let command: Vec<String> = l
            .get("command")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|c: &Vec<String>| !c.is_empty())
            .ok_or_else(|| at("`command` must be a non-empty array of strings"))?;
        let mut preconditions = Vec::new();
        if let Some(arr) = l.get("preconditions").and_then(|v| v.as_array()) {
            for p in arr {
                let s = p
                    .as_str()
                    .ok_or_else(|| at("`preconditions` entries must be strings"))?;
                preconditions.push(
                    Precondition::parse(s).map_err(|e| format!("{}: {e}", at("preconditions")))?,
                );
            }
        }
        out.push(LaneSpec {
            id,
            kind,
            enforcement,
            est_secs,
            command,
            preconditions,
            baseline_dir: l
                .get("baseline_dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from),
            bank: l.get("bank").and_then(|v| v.as_str()).map(PathBuf::from),
        });
    }
    Ok(out)
}

// ─── The stack fingerprint ──────────────────────────────────────────

/// What a number from this run may be compared against.
///
/// A latency in milliseconds means nothing without the model that produced
/// it and the bank that asked for it. `LaneBaseline::diff` already refuses
/// to compare across model stems (INCOMPARABLE); this is the same rule for
/// the whole run, computed ONCE and printed FIRST so a reader never has to
/// wonder which stack a table describes.
#[derive(Debug, Clone)]
struct Fingerprint {
    hex: String,
    primary: String,
    fast: String,
    embed: String,
    smoke_subsets: Vec<String>,
    banks: BTreeMap<String, String>,
}

impl Fingerprint {
    fn render(&self) -> String {
        let mut s = format!("stack fingerprint: {}\n", self.hex);
        s.push_str(&format!("  primary  {}\n", self.primary));
        s.push_str(&format!("  fast     {}\n", self.fast));
        s.push_str(&format!("  embed    {}\n", self.embed));
        s.push_str(&format!(
            "  smoke    {}\n",
            if self.smoke_subsets.is_empty() {
                "none declared".to_string()
            } else {
                self.smoke_subsets.join(", ")
            }
        ));
        for (lane, hash) in &self.banks {
            s.push_str(&format!("  bank     {lane}={hash}\n"));
        }
        s
    }
}

/// Short content hash. `sha2` is already a dependency of this crate; the
/// digest is truncated to 12 hex chars because it names a directory a human
/// reads, not a security boundary.
fn short_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())[..12].to_string()
}

/// The two slot aliases every lane's numbers hang from. `/v1/models` lists
/// each alias with `owned_by: "alias→<stem>"`; the stem is what a baseline
/// is keyed on, because two hosts running "primary" are not running the
/// same model.
async fn resolve_slot_stems(base: &str) -> (String, String) {
    let unknown = || "unresolved".to_string();
    let Ok(resp) = reqwest::Client::new()
        .get(format!("{base}/v1/models"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    else {
        return (unknown(), unknown());
    };
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return (unknown(), unknown());
    };
    // `unresolved` is a NAMED absence, and it is load-bearing: it goes into
    // the fingerprint, so a run against an unreachable daemon computes a
    // DIFFERENT fingerprint than a resolved one and cannot silently be
    // compared against a real baseline. It also prints, on the first line.
    let stem_of = |alias: &str| -> String {
        body.get("data")
            .and_then(|d| d.as_array())
            .and_then(|rows| {
                rows.iter()
                    .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(alias))
            })
            .and_then(|r| r.get("owned_by").and_then(|v| v.as_str()))
            // `alias→<stem>`; a non-alias row owns itself.
            .map(|o| o.split('→').next_back().unwrap_or(o).to_string())
            .unwrap_or_else(unknown)
    };
    (stem_of("primary"), stem_of("fast"))
}

/// Subset ids declared in `sovereign/bench/smoke.toml`, sorted.
///
/// In the fingerprint because a baseline captured against a 6-probe subset
/// is not comparable to one captured against 12 — the ci-bench README
/// records the same trap for its cap-specific baselines, where a moved cap
/// false-fired every lane.
///
/// ABSENT and MALFORMED are two answers, not one. A `smoke.toml` that fails
/// to parse would otherwise read as "none declared" — the same fingerprint a
/// host with no subsets at all computes — and every lane would then compare
/// against a baseline captured under subsets it is no longer running
/// (ARCH §18.3). Absent is `Ok(vec![])`; malformed is an `Err` that refuses
/// the run.
fn smoke_subset_ids(repo: &Path) -> Result<Vec<String>, String> {
    let path = repo.join("sovereign/bench/smoke.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut ids: Vec<String> = doc
        .get("subset")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("subset_id").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}

async fn compute_fingerprint(
    repo: &Path,
    lanes: &[LaneSpec],
    base: &str,
) -> Result<Fingerprint, String> {
    let (primary, fast) = resolve_slot_stems(base).await;
    let embed = sovereign_cli_shared::models::configured_embed_model_name();
    let smoke_subsets = smoke_subset_ids(repo)?;
    let mut banks = BTreeMap::new();
    for l in lanes {
        if let Some(bank) = &l.bank {
            let hash = std::fs::read(repo.join(bank))
                .map(|b| short_hash(&b))
                .unwrap_or_else(|_| "absent".to_string());
            banks.insert(l.id.clone(), hash);
        }
    }
    let mut canonical = format!("primary={primary}\nfast={fast}\nembed={embed}\n");
    canonical.push_str(&format!("smoke={}\n", smoke_subsets.join(",")));
    for (lane, hash) in &banks {
        canonical.push_str(&format!("bank:{lane}={hash}\n"));
    }
    tracing::debug!(canonical = %canonical, "quality check: fingerprint inputs");
    Ok(Fingerprint {
        hex: short_hash(canonical.as_bytes()),
        primary,
        fast,
        embed,
        smoke_subsets,
        banks,
    })
}

// ─── Preconditions ──────────────────────────────────────────────────

/// Locate an executable: beside the running dispatcher first (co-built
/// target artifacts are what a developer actually means), then PATH. Same
/// discovery order as `llm_bin::locate`.
fn locate_binary(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            if let Some(dir) = real.parent() {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    which::which(name).ok()
}

/// One token out of the named slot. The probe IS a decode.
async fn slot_decodes(base: &str, slot: &str) -> bool {
    let body = serde_json::json!({
        "model": slot,
        "messages": [{"role": "user", "content": "ok"}],
        "max_tokens": 1,
        "temperature": 0,
        "stream": false,
    });
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
    else {
        return false;
    };
    resp.status().is_success()
}

async fn check_precondition(p: &Precondition, base: &str) -> bool {
    let ok = match p {
        Precondition::PortListening(port) => std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], *port)),
            Duration::from_secs(2),
        )
        .is_ok(),
        Precondition::SlotDecodes(slot) => slot_decodes(base, slot).await,
        Precondition::CorpusInstalled(id) => {
            use sovereign_enrichment_catalog::corpus_state::{inspect_corpus_state, CorpusState};
            inspect_corpus_state(id) != CorpusState::Unindexed
        }
        Precondition::Binary(name) => locate_binary(name).is_some(),
    };
    tracing::debug!(precondition = ?p, ok, "quality check: precondition");
    ok
}

// ─── Running one lane ───────────────────────────────────────────────

/// What happened to one lane, beyond its verdict.
struct LaneRun {
    judgement: Judgement,
    secs: u64,
    /// `None` when the lane never started.
    exit_code: Option<i32>,
}

/// Resolve `command[0]`. `svrn`/`sovereign` mean THIS dispatcher — never
/// whatever an operator's PATH happens to hold, which on this host is a
/// symlink into someone else's `target/debug`.
fn resolve_program(program: &str) -> PathBuf {
    if matches!(program, "svrn" | "sovereign" | "sovereign-cli") {
        if let Ok(exe) = std::env::current_exe() {
            return exe;
        }
    }
    PathBuf::from(program)
}

#[allow(clippy::too_many_arguments)]
async fn run_lane(
    lane: &LaneSpec,
    repo: &Path,
    out_dir: &Path,
    fingerprint: &Fingerprint,
    mint: bool,
    cap_secs: u64,
) -> LaneRun {
    let t0 = Instant::now();
    let stdout_path = out_dir.join(format!("lane-{}.out", lane.id));
    let stderr_path = out_dir.join(format!("lane-{}.err", lane.id));
    let (Ok(so), Ok(se)) = (
        std::fs::File::create(&stdout_path),
        std::fs::File::create(&stderr_path),
    ) else {
        return LaneRun {
            judgement: Judgement::could_not_judge(
                lane.id.clone(),
                Reason::new(format!(
                    "cannot create the lane log under {}",
                    out_dir.display()
                ))
                .expect("a path is never a placeholder"),
            ),
            secs: 0,
            exit_code: None,
        };
    };

    let program = resolve_program(&lane.command[0]);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&lane.command[1..])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(so))
        .stderr(Stdio::from(se))
        // The lane protocol, as environment. Env rather than appended flags
        // because a lane command is DATA and some lanes wrap a verb that
        // never heard of this runner.
        .env("SOVEREIGN_QUALITY_FINGERPRINT", &fingerprint.hex)
        .env("SOVEREIGN_QUALITY_OUT_DIR", out_dir)
        .env("SOVEREIGN_QUALITY_BUDGET_SECS", cap_secs.to_string())
        .env(
            "SOVEREIGN_QUALITY_BASELINE_DIR",
            lane.baseline_dir
                .as_ref()
                .map(|d| repo.join(d))
                .unwrap_or_default(),
        );
    if mint {
        cmd.env("SOVEREIGN_QUALITY_MINT", "1");
    }
    tracing::debug!(lane = %lane.id, cmd = ?lane.command, cap_secs, "quality check: lane start");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return LaneRun {
                judgement: Judgement::never_ran(
                    lane.id.clone(),
                    Reason::new(format!("cannot run `{}`: {e}", lane.command.join(" ")))
                        .expect("a command line is never a placeholder"),
                ),
                secs: t0.elapsed().as_secs(),
                exit_code: None,
            };
        }
    };

    // Three outcomes, three reasons. A wait that ERRORED is not a lane that
    // ran out of time, and reporting it as one sends the reader to a budget
    // that was never the problem (ARCH §18.3).
    enum Waited {
        Exited(std::process::ExitStatus),
        Capped,
        WaitFailed(String),
    }
    let cap = Duration::from_secs(cap_secs);
    let waited = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Waited::Exited(s),
            Ok(None) => {
                if t0.elapsed() >= cap {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Waited::Capped;
                }
                tokio::time::sleep(POLL).await;
            }
            Err(e) => break Waited::WaitFailed(e.to_string()),
        }
    };
    let secs = t0.elapsed().as_secs();

    let status = match waited {
        Waited::Exited(s) => s,
        Waited::Capped => {
            return LaneRun {
                judgement: Judgement::could_not_judge(
                    lane.id.clone(),
                    Reason::new(format!(
                        "killed at its {cap_secs}s cap — it reached no verdict, \
                         which is not a pass"
                    ))
                    .expect("never a placeholder"),
                ),
                secs,
                exit_code: None,
            };
        }
        Waited::WaitFailed(e) => {
            return LaneRun {
                judgement: Judgement::could_not_judge(
                    lane.id.clone(),
                    Reason::new(format!("cannot wait on the lane process: {e}"))
                        .expect("never a placeholder"),
                ),
                secs,
                exit_code: None,
            };
        }
    };
    let code = status.code();
    let captured = match std::fs::read_to_string(&stdout_path) {
        Ok(c) => c,
        // An unreadable log is not silence. Defaulting to `""` here would
        // report "the lane printed nothing" about a lane that may have said
        // everything (ARCH §18.3).
        Err(e) => {
            return LaneRun {
                judgement: Judgement::could_not_judge(
                    lane.id.clone(),
                    Reason::new(format!("cannot read {}: {e}", stdout_path.display()))
                        .expect("a path is never a placeholder"),
                ),
                secs,
                exit_code: code,
            };
        }
    };
    tracing::debug!(lane = %lane.id, ?code, secs, "quality check: lane exit");

    let judgement = match lane_verdict::from_stdout(&captured) {
        Ok(j) => {
            // The lane names its own subject; the TABLE is keyed on the lane
            // id, so a lane that answers about something else would put an
            // unrelated row in the operator's report.
            if j.subject() == lane.id {
                j
            } else {
                Judgement::could_not_judge(
                    lane.id.clone(),
                    Reason::new(format!(
                        "the lane's verdict line names subject `{}`, not `{}`",
                        j.subject(),
                        lane.id
                    ))
                    .expect("never a placeholder"),
                )
            }
        }
        Err(e) => Judgement::never_ran(
            lane.id.clone(),
            Reason::new(format!(
                "{e} (exit {}) — see {}",
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                stdout_path.display()
            ))
            .expect("never a placeholder"),
        ),
    };
    LaneRun {
        judgement: judgement.as_of(SystemTime::now()),
        secs,
        exit_code: code,
    }
}

// ─── The command ────────────────────────────────────────────────────

struct Args {
    lanes: Vec<String>,
    budget_secs: u64,
    mint: bool,
    lane_table: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        lanes: Vec::new(),
        budget_secs: DEFAULT_BUDGET_SECS,
        mint: false,
        lane_table: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lane" => {
                let v = args.get(i + 1).ok_or("--lane needs a lane id")?;
                out.lanes.push(v.clone());
                i += 1;
            }
            "--budget-secs" => {
                let v = args.get(i + 1).ok_or("--budget-secs needs a number")?;
                out.budget_secs = v
                    .parse()
                    .map_err(|_| format!("--budget-secs: `{v}` is not a number"))?;
                i += 1;
            }
            "--mint" => out.mint = true,
            "--lane-table" => {
                let v = args.get(i + 1).ok_or("--lane-table needs a path")?;
                out.lane_table = Some(PathBuf::from(v));
                i += 1;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
        i += 1;
    }
    Ok(out)
}

/// Which lanes have a baseline captured against THIS stack.
fn comparable_baselines(repo: &Path, lanes: &[LaneSpec], fp: &Fingerprint) -> usize {
    lanes
        .iter()
        .filter(|l| {
            l.baseline_dir
                .as_ref()
                .is_some_and(|d| repo.join(d).join(&fp.hex).join("latest.json").exists())
        })
        .count()
}

/// `svrn quality <subcommand>` — the verb's own router.
///
/// It lives beside the runner rather than in `main.rs` because `main.rs` is
/// a DISPATCHER: 1,500 lines of verb table and exec hops, sitting on
/// ARCH §3.1's slack. A subcommand split belongs with the subcommand.
///
/// `exec_lane` is passed in rather than named here: the LANES live in
/// `sovereign-cli-llm` (each drives inference, ingests a corpus or runs a
/// judge) and this crate reaches that sibling by exec, which is the
/// dispatcher's business, not the runner's.
pub async fn run_verb(args: &[String], exec_lane: impl Fn(&str, &[String]) -> i32) -> i32 {
    match args.first().map(String::as_str) {
        Some("check") => run(&args[1..]).await,
        Some("lane") => exec_lane("quality-lane", &args[1..]),
        // An unknown subcommand is REFUSED, never defaulted to `check`
        // (ARCH §18.3): running a 30-minute suite because someone typo'd a
        // lane name is not a courtesy.
        Some(other) if other != "--help" && other != "-h" => {
            eprintln!("svrn quality: unknown subcommand `{other}`. Try: svrn quality check");
            2
        }
        _ => {
            println!("Usage: svrn quality check [--lane <id>]... [--budget-secs 1800] [--mint]");
            println!("       svrn quality lane <id>");
            println!();
            println!("  check   Run the curated breakage lanes and write the table to");
            println!("          target/quality-check/<stamp>/summary.json");
            println!("  lane    Run ONE lane directly, printing its own rows. The");
            println!("          runner above drives the same command per lane.");
            0
        }
    }
}

pub async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        crate::util::help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let Some(repo) = crate::posture_cmd::find_repo_root() else {
        eprintln!("error: `svrn quality check` reads {LANE_TABLE} — run it from a source checkout");
        return 2;
    };
    let table_path = parsed
        .lane_table
        .clone()
        .unwrap_or_else(|| repo.join(LANE_TABLE));
    let text = match std::fs::read_to_string(&table_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", table_path.display());
            return 2;
        }
    };
    let all_lanes = match parse_lane_table(&text) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    // A `--lane` that names nothing is a typo, and running the other seven
    // lanes as though the operator had asked for them is the silent
    // substitution ARCH §18.3 forbids.
    for want in &parsed.lanes {
        if !all_lanes.iter().any(|l| &l.id == want) {
            eprintln!(
                "error: no lane `{want}` in {}. Declared: {}",
                table_path.display(),
                all_lanes
                    .iter()
                    .map(|l| l.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return 2;
        }
    }
    let lanes: Vec<LaneSpec> = all_lanes
        .into_iter()
        .filter(|l| parsed.lanes.is_empty() || parsed.lanes.contains(&l.id))
        .collect();

    let base = sovereign_cli_shared::urls::daemon_base_url();
    let fingerprint = match compute_fingerprint(&repo, &lanes, &base).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    print!("{}", fingerprint.render());
    let comparable = comparable_baselines(&repo, &lanes, &fingerprint);
    println!(
        "{comparable} of {} lanes have a comparable baseline for this stack{}",
        lanes.len(),
        if parsed.mint {
            " · --mint: lanes may write one"
        } else {
            ""
        }
    );
    println!();

    let stamp = stamp_now();
    let out_dir = repo.join("target/quality-check").join(&stamp);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: cannot create {}: {e}", out_dir.display());
        return 2;
    }

    let started = Instant::now();
    let budget = Duration::from_secs(parsed.budget_secs);
    let mut rows: Vec<Judgement> = Vec::new();
    let mut secs_by_lane: Vec<(String, u64, Option<i32>)> = Vec::new();
    let mut ran_any = false;

    for lane in &lanes {
        // 1. Budget. Verbatim rule from ci-bench: under a minute of runway
        //    is not a lane, it is a lane that will be killed.
        let remaining = budget.saturating_sub(started.elapsed()).as_secs();
        if remaining <= MIN_LANE_RUNWAY_SECS {
            let j = Judgement::could_not_judge(
                lane.id.clone(),
                Reason::new(format!(
                    "out of budget — {remaining}s left of {}s, and this lane wants ~{}s",
                    parsed.budget_secs, lane.est_secs
                ))
                .expect("never a placeholder"),
            );
            println!(
                "── SKIP(budget)  [{}] {}",
                lane.enforcement.as_str(),
                lane.id
            );
            rows.push(j);
            secs_by_lane.push((lane.id.clone(), 0, None));
            continue;
        }

        // 2. Preconditions. A failed one is could-not-judge NAMING it, and
        //    the lane does not run — an unmet precondition tells you nothing
        //    about the code under test.
        let mut unmet: Vec<String> = Vec::new();
        for p in &lane.preconditions {
            if !check_precondition(p, &base).await {
                unmet.push(p.describe());
            }
        }
        if !unmet.is_empty() {
            let j = Judgement::could_not_judge(
                lane.id.clone(),
                Reason::new(format!("precondition unmet: {}", unmet.join("; ")))
                    .expect("never a placeholder"),
            );
            println!(
                "── SKIP(precondition)  [{}] {} — {}",
                lane.enforcement.as_str(),
                lane.id,
                unmet.join("; ")
            );
            rows.push(j);
            secs_by_lane.push((lane.id.clone(), 0, None));
            continue;
        }

        println!(
            "── RUN   [{}] {}   (budget left {remaining}s, est {}s)",
            lane.enforcement.as_str(),
            lane.id,
            lane.est_secs
        );
        let run = run_lane(lane, &repo, &out_dir, &fingerprint, parsed.mint, remaining).await;
        ran_any = true;
        println!(
            "── {}  [{}] {}   ({}s)",
            run.judgement.verdict(),
            lane.enforcement.as_str(),
            lane.id,
            run.secs
        );
        rows.push(run.judgement);
        secs_by_lane.push((lane.id.clone(), run.secs, run.exit_code));
    }

    let total_secs = started.elapsed().as_secs();
    println!();
    print!("{}", render_rows(&rows));
    if let Some(footer) = honesty_footer(&rows) {
        println!();
        println!("  {footer}");
    }
    println!();
    println!(
        "  total {total_secs}s of a {}s budget · lane logs + summary: {}",
        parsed.budget_secs,
        out_dir.display()
    );

    let summary_path = out_dir.join("summary.json");
    if let Err(e) = write_summary(
        &summary_path,
        &stamp,
        &fingerprint,
        &lanes,
        &rows,
        &secs_by_lane,
        total_secs,
        parsed.budget_secs,
        comparable,
    ) {
        // The durable table is the reason this command exists. Losing it is
        // not a footnote.
        eprintln!("error: cannot write {}: {e}", summary_path.display());
        return 2;
    }

    if !ran_any {
        // Same claim as `sovereign-test.sh`'s exit 4: nothing ran, so
        // nothing was verified, and that is never a pass.
        eprintln!("nothing ran — every lane was skipped. Verified nothing.");
        return 4;
    }
    let hard_red = lanes.iter().zip(rows.iter()).any(|(l, j)| {
        l.enforcement == Enforcement::Hard && j.verdict() != kernel_types::Verdict::Passed
    });
    i32::from(hard_red)
}

#[allow(clippy::too_many_arguments)]
fn write_summary(
    path: &Path,
    stamp: &str,
    fp: &Fingerprint,
    lanes: &[LaneSpec],
    rows: &[Judgement],
    secs: &[(String, u64, Option<i32>)],
    total_secs: u64,
    budget_secs: u64,
    comparable: usize,
) -> std::io::Result<()> {
    let lane_rows: Vec<serde_json::Value> = lanes
        .iter()
        .zip(rows.iter())
        .zip(secs.iter())
        .map(|((l, j), (_, s, code))| {
            serde_json::json!({
                "id": l.id,
                "kind": l.kind.as_str(),
                "enforcement": l.enforcement.as_str(),
                "verdict": j.verdict().as_str(),
                "reason": j.reason().as_str(),
                "secs": s,
                "est_secs": l.est_secs,
                "exit_code": code,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": "quality-check/v1",
        "stamp": stamp,
        "fingerprint": {
            "hex": fp.hex,
            "primary": fp.primary,
            "fast": fp.fast,
            "embed": fp.embed,
            "smoke_subsets": fp.smoke_subsets,
            "banks": fp.banks,
        },
        "budget_secs": budget_secs,
        "total_secs": total_secs,
        "lanes_with_comparable_baseline": comparable,
        "lanes": lane_rows,
    });
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&doc)?))
}

/// `YYYYmmdd-HHMMSS` in local time — the run directory a human names when
/// asking a colleague to look at a table.
fn stamp_now() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn quality check",
    summary: "The curated 30-minute breakage check — one lane table, four verdicts, persisted.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn quality check [--lane <id>]... [--budget-secs 1800] [--mint]",
        ),
        crate::util::help::HelpSection::Flags(&[
            (
                "--lane <id>",
                "Run only this lane (repeatable). An unknown id is refused, never ignored.",
            ),
            (
                "--budget-secs <n>",
                "Total wall budget (default 1800). A lane with under 60s of runway is could-not-judge, not a pass.",
            ),
            (
                "--mint",
                "Permit lanes to write a baseline for this stack fingerprint. Without it a first run writes none.",
            ),
            (
                "--lane-table <path>",
                "Read a different lane table (default quality/check-lanes.toml).",
            ),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("svrn quality check", "every declared lane, 30-minute budget"),
            ("svrn quality check --lane chat-ask", "the focus lane alone"),
        ]),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
[[lane]]
id = "chat-ask"
kind = "judged"
enforcement = "hard"
est_secs = 300
command = ["svrn", "quality", "lane", "chat-ask"]
preconditions = ["port-listening:9741", "slot-decodes:primary"]
baseline_dir = "sovereign/bench/quality-check/baselines/chat-ask"
bank = "sovereign/bench/quality-check/chat-ask.toml"
"#;

    #[test]
    fn the_lane_table_is_data_and_it_parses() {
        let lanes = parse_lane_table(TABLE).expect("parses");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].id, "chat-ask");
        assert_eq!(lanes[0].enforcement, Enforcement::Hard);
        assert_eq!(lanes[0].kind, LaneKind::Judged);
        assert_eq!(
            lanes[0].preconditions,
            vec![
                Precondition::PortListening(9741),
                Precondition::SlotDecodes("primary".into()),
            ]
        );
    }

    /// A malformed lane REFUSES the file. The alternative — skipping it —
    /// makes a lane's absence indistinguishable from its passing, which is
    /// the failure this whole command exists to prevent.
    #[test]
    fn a_malformed_lane_refuses_the_table_rather_than_vanishing() {
        for bad in [
            "[[lane]]\nid = \"x\"\nkind = \"judged\"\nenforcement = \"nope\"\nest_secs = 1\ncommand = [\"true\"]\n",
            "[[lane]]\nid = \"x\"\nkind = \"guess\"\nenforcement = \"hard\"\nest_secs = 1\ncommand = [\"true\"]\n",
            "[[lane]]\nid = \"x\"\nkind = \"judged\"\nenforcement = \"hard\"\nest_secs = 1\ncommand = []\n",
            "[[lane]]\nid = \"x\"\nkind = \"judged\"\nenforcement = \"hard\"\nest_secs = 1\ncommand = [\"true\"]\npreconditions = [\"whatever:1\"]\n",
        ] {
            assert!(parse_lane_table(bad).is_err(), "{bad}");
        }
        assert!(
            parse_lane_table("").is_err(),
            "an empty table verifies nothing"
        );
    }

    /// Two lanes of one id means one of them silently loses its row.
    #[test]
    fn a_duplicate_lane_id_is_refused() {
        let doubled = format!("{TABLE}{TABLE}");
        let err = parse_lane_table(&doubled).unwrap_err();
        assert!(err.contains("declared twice"), "{err}");
    }

    #[test]
    fn every_precondition_spelling_round_trips_and_junk_is_refused() {
        assert_eq!(
            Precondition::parse("port-listening:9741"),
            Ok(Precondition::PortListening(9741))
        );
        assert_eq!(
            Precondition::parse("corpus-installed:sep"),
            Ok(Precondition::CorpusInstalled("sep".into()))
        );
        assert_eq!(
            Precondition::parse("binary:sovereign-cli-llm"),
            Ok(Precondition::Binary("sovereign-cli-llm".into()))
        );
        for junk in [
            "",
            "port-listening",
            "port-listening:",
            "port-listening:no",
            "sudo:rm",
        ] {
            assert!(Precondition::parse(junk).is_err(), "{junk}");
        }
    }

    /// `svrn` in a lane command means THIS dispatcher. A PATH lookup here
    /// would silently run whichever build an operator's symlink points at —
    /// on this host, one in a different checkout.
    #[test]
    fn svrn_in_a_lane_command_resolves_to_the_running_dispatcher() {
        let me = std::env::current_exe().unwrap();
        for spelling in ["svrn", "sovereign", "sovereign-cli"] {
            assert_eq!(resolve_program(spelling), me, "{spelling}");
        }
        assert_eq!(resolve_program("python3"), PathBuf::from("python3"));
    }

    /// ABSENT and MALFORMED are two answers. Collapsing them makes a
    /// mis-edited `smoke.toml` compute the same fingerprint as a host with
    /// no subsets — and every lane then compares against a baseline captured
    /// under subsets it is no longer running.
    #[test]
    fn a_malformed_smoke_file_refuses_the_run_and_an_absent_one_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let bench = tmp.path().join("sovereign/bench");
        std::fs::create_dir_all(&bench).unwrap();
        // Absent: legitimately no subsets.
        assert_eq!(smoke_subset_ids(tmp.path()), Ok(Vec::new()));
        // Malformed: refused, naming the file.
        std::fs::write(bench.join("smoke.toml"), "[[subset\nnope").unwrap();
        let err = smoke_subset_ids(tmp.path()).unwrap_err();
        assert!(err.contains("smoke.toml"), "{err}");
        // Well-formed: sorted ids.
        std::fs::write(
            bench.join("smoke.toml"),
            "[[subset]]\nsubset_id = \"z1\"\n[[subset]]\nsubset_id = \"a1\"\n",
        )
        .unwrap();
        assert_eq!(
            smoke_subset_ids(tmp.path()),
            Ok(vec!["a1".into(), "z1".into()])
        );
    }

    #[test]
    fn args_parse_and_reject() {
        let a = parse_args(&[
            "--lane".into(),
            "chat-ask".into(),
            "--lane".into(),
            "throughput".into(),
            "--budget-secs".into(),
            "600".into(),
            "--mint".into(),
        ])
        .unwrap();
        assert_eq!(a.lanes, vec!["chat-ask", "throughput"]);
        assert_eq!(a.budget_secs, 600);
        assert!(a.mint);
        assert_eq!(parse_args(&[]).unwrap().budget_secs, DEFAULT_BUDGET_SECS);
        assert!(parse_args(&["--lane".into()]).is_err());
        assert!(parse_args(&["--budget-secs".into(), "soon".into()]).is_err());
        assert!(parse_args(&["--wat".into()]).is_err());
        // The default is NOT mint. A run that writes a baseline it was not
        // asked to write is the defect the order names.
        assert!(!parse_args(&[]).unwrap().mint);
    }
}
