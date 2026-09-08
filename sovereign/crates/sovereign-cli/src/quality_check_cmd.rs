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

use kernel_types::quality::{
    Enforcement, Instrument, Precondition, Registry, RunsIn, Trigger, VenueAction, VerdictSource,
};
use kernel_types::{honesty_footer, render_rows, Judgement, Reason};
use sovereign_cli_shared::lane_verdict;

/// How often the runner checks on a lane subprocess.
const POLL: Duration = Duration::from_millis(250);

// ─── The registry (ONE table, ARCH §10.6) ───────────────────────────

/// Where the instrument registry lives, relative to the repo root.
///
/// It used to be `quality/check-lanes.toml`, a SECOND table answering "what
/// runs here" beside `quality/instruments.toml` — two schemas whose fields
/// collided on `kind`, `enforcement`, cost and `baseline` with different
/// meanings on each side. They merged on 2026-09-07 and this constant is what
/// is left: one file, one parser (`kernel_types::quality::Registry`), read by
/// this runner, `xtask instrument-gate`, `svrn quality map` and `svrn
/// posture`.
const REGISTRY: &str = "quality/instruments.toml";

/// The venue `svrn quality check` runs when no `--trigger` is given.
const DEFAULT_VENUE: RunsIn = RunsIn::Check;

/// Probe one [`Precondition`] against this host.
///
/// The predicates live here rather than in `kernel_types` deliberately: that
/// crate parses text and validates closed sets and knows nothing about a
/// checkout, a daemon or a load average. The SPELLING is shared; the probing
/// is the runner's.
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
        // The shell harnesses' spelling, which the lane runner lacked until
        // the merge. `/run/.containerenv` names the container on this repo's
        // toolbox hosts; its ABSENCE means the host side, which is a real
        // answer and not an error.
        Precondition::Container(name) => std::fs::read_to_string("/run/.containerenv")
            .map(|t| t.contains(name.as_str()))
            .unwrap_or(false),
        // A host that reports no load average cannot be shown quiet, and
        // assuming it is would make the guard a rubber stamp on exactly the
        // platform it cannot see.
        Precondition::HostQuiet(max) => check_host_quiet(*max),
    };
    tracing::debug!(precondition = ?p, ok, "quality check: precondition");
    ok
}

/// How an unmet precondition reads in a could-not-judge reason.
///
/// `host-quiet` is the one that carries evidence rather than restating the
/// rule: it names the load it SAW. Reached only for an UNMET precondition, so
/// a `None` reason there means the load fell back under the bound between the
/// check and the render — say THAT, because "the host is quiet" inside an
/// unmet-precondition reason would read as a contradiction and hide a real
/// race (ARCH §18.3).
fn describe_precondition(p: &Precondition) -> String {
    match p {
        Precondition::PortListening(port) => format!("nothing is listening on 127.0.0.1:{port}"),
        Precondition::SlotDecodes(s) => {
            format!("slot `{s}` did not decode a token — is the model resident?")
        }
        Precondition::CorpusInstalled(c) => format!("corpus `{c}` is not installed"),
        Precondition::Binary(b) => format!("binary `{b}` is not on this host"),
        Precondition::Container(c) => {
            format!("this shell is not inside the `{c}` container — the native toolchain is there")
        }
        // ONE reader, in `sovereign_cli_shared::host_load` — the chat-ask
        // lane judges its `per-stage ceilings` row against the same number in
        // another process (ARCH §10.6).
        Precondition::HostQuiet(max) => sovereign_cli_shared::host_load::host_quiet(*max)
            .reason()
            .unwrap_or_else(|| {
                format!(
                    "the 1-minute load average fell back under {max:.1} between the check and \
                     this message; it did not run"
                )
            }),
    }
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
    // Sorted and DEDUPED: one subset spans several banks, so `retrieval-prod-v1`
    // appears once per bank row. The fingerprint wants the SET of subsets this
    // stack runs, and a bank count leaking into it would move the fingerprint
    // every time a subset gained a bank without changing which items run.
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// A digest of what `smoke.toml`'s rows actually SELECT — every
/// (subset_id, bank, mode/ids) triple, canonicalised and sorted.
///
/// The subset IDs alone are not enough, and the gap is the exact trap the
/// ci-bench README records for its cap-specific baselines. Cut
/// `chaos-monkey-v1` from six probes to four and every id in
/// [`smoke_subset_ids`] is unchanged, every bank hash is unchanged (the bank
/// file was not touched), so the fingerprint is unchanged — and last week's
/// six-probe baseline is then compared against a four-probe run as if they
/// were the same measurement. Renaming the subset on every edit would work
/// and is a thing to remember; this is the same rule made structural
/// (ARCH §10).
///
/// Comments and key ORDER are deliberately not in the digest: only what is
/// selected. A reworded comment must not orphan a baseline.
fn smoke_selection_digest(repo: &Path) -> Result<String, String> {
    let path = repo.join("sovereign/bench/smoke.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("absent".to_string()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    // A malformed row REFUSES the run rather than digesting to a
    // placeholder. Two different unreadable files must not hash the same,
    // and "the fingerprint could not read this row" is not a stack anything
    // should be compared against (ARCH §18.3) — the same rule
    // `smoke_subset_ids` applies to a file that will not parse at all.
    let mut rows: Vec<String> = Vec::new();
    // A file with NO `subset` key declares no subsets, which is a real state
    // and the one `smoke_subset_ids` answers `Ok(vec![])` for. A `subset`
    // key that is not an array is a MALFORMED file, and reading it as "none
    // declared" is the substitution this function exists to avoid.
    let empty: Vec<toml::Value> = Vec::new();
    let subsets = match doc.get("subset") {
        None => &empty,
        Some(v) => v
            .as_array()
            .ok_or_else(|| format!("{}: `subset` must be an array of tables", path.display()))?,
    };
    for r in subsets {
        let id = r
            .get("subset_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{}: a [[subset]] row has no subset_id", path.display()))?;
        let bank = r
            .get("bank")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{}: subset `{id}` has a row with no bank", path.display()))?;
        let what = match (r.get("mode").and_then(|v| v.as_str()), r.get("ids")) {
            (Some(m), None) => format!("mode={m}"),
            (None, Some(ids)) => {
                let arr = ids.as_array().ok_or_else(|| {
                    format!("{}: `ids` for {bank} must be an array", path.display())
                })?;
                let mut v: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
                if v.len() != arr.len() {
                    return Err(format!(
                        "{}: `ids` for {bank} holds a non-string",
                        path.display()
                    ));
                }
                v.sort_unstable();
                format!("ids={}", v.join(","))
            }
            _ => {
                return Err(format!(
                    "{}: the row for {bank} in subset `{id}` must declare EITHER \
                     mode = \"full\" OR an `ids` list, not both and not neither",
                    path.display()
                ))
            }
        };
        rows.push(format!("{id}|{bank}|{what}"));
    }
    rows.sort();
    Ok(short_hash(rows.join("\n").as_bytes()))
}

async fn compute_fingerprint(
    repo: &Path,
    lanes: &[&Instrument],
    base: &str,
) -> Result<Fingerprint, String> {
    let (primary, fast) = resolve_slot_stems(base).await;
    let embed = sovereign_cli_shared::models::configured_embed_model_name();
    let smoke_subsets = smoke_subset_ids(repo)?;
    let mut banks = BTreeMap::new();
    for l in lanes {
        if let Some(bank) = l.bank.as_deref() {
            let hash = std::fs::read(repo.join(bank))
                .map(|b| short_hash(&b))
                .unwrap_or_else(|_| "absent".to_string());
            banks.insert(l.id.clone(), hash);
        }
    }
    let mut canonical = format!("primary={primary}\nfast={fast}\nembed={embed}\n");
    canonical.push_str(&format!("smoke={}\n", smoke_subsets.join(",")));
    canonical.push_str(&format!("smoke-sel={}\n", smoke_selection_digest(repo)?));
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

/// The `HostQuiet` predicate, split out so a test can drive it against a
/// bound no host beats and one no host exceeds — the two ends that show it
/// reads the machine rather than returning a constant (ARCH §18.1).
fn check_host_quiet(max_load: f64) -> bool {
    sovereign_cli_shared::host_load::host_quiet(max_load).is_quiet()
}

// ─── Running one instrument ─────────────────────────────────────────

/// What happened to one instrument, beyond its verdict.
struct InstrumentRun {
    judgement: Judgement,
    secs: u64,
    /// `None` when it never started.
    exit_code: Option<i32>,
    /// The tail of what it printed, kept only for a red. A gate in this repo
    /// ends its output with its own fix command, so printing the tail of a
    /// failure is the whole of what the shell harnesses used to reconstruct
    /// by grepping for `✗|FAIL|^error` — a pattern no gate promised to keep.
    tail: String,
}

/// Resolve `argv[0]`. `svrn`/`sovereign` mean THIS dispatcher — never
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

/// The last `n` non-empty lines of a captured stream.
fn tail_of(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// One instrument in flight.
struct InFlight<'a> {
    inst: &'a Instrument,
    child: std::process::Child,
    started: Instant,
    cap_secs: u64,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

/// Spawn one instrument. `Err` is a judgement it earned by not starting.
#[allow(clippy::too_many_arguments)]
fn spawn_instrument<'a>(
    inst: &'a Instrument,
    argv: &[String],
    repo: &Path,
    out_dir: &Path,
    fingerprint: &Fingerprint,
    mint: bool,
    cap_secs: u64,
) -> Result<InFlight<'a>, InstrumentRun> {
    let t0 = Instant::now();
    let stdout_path = out_dir.join(format!("lane-{}.out", inst.id));
    let stderr_path = out_dir.join(format!("lane-{}.err", inst.id));
    let (Ok(so), Ok(se)) = (
        std::fs::File::create(&stdout_path),
        std::fs::File::create(&stderr_path),
    ) else {
        return Err(InstrumentRun {
            judgement: Judgement::could_not_judge(
                inst.id.clone(),
                Reason::new(format!("cannot create the log under {}", out_dir.display()))
                    .expect("a path is never a placeholder"),
            ),
            secs: 0,
            exit_code: None,
            tail: String::new(),
        });
    };
    let program = resolve_program(&argv[0]);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&argv[1..])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(so))
        .stderr(Stdio::from(se))
        // The lane protocol, as environment. Env rather than appended flags
        // because a command is DATA and some instruments wrap a verb that
        // never heard of this runner.
        .env("SOVEREIGN_QUALITY_FINGERPRINT", &fingerprint.hex)
        .env("SOVEREIGN_QUALITY_OUT_DIR", out_dir)
        .env("SOVEREIGN_QUALITY_BUDGET_SECS", cap_secs.to_string())
        .env(
            "SOVEREIGN_QUALITY_BASELINE_DIR",
            baseline_dir(inst).map(|d| repo.join(d)).unwrap_or_default(),
        );
    if mint {
        cmd.env("SOVEREIGN_QUALITY_MINT", "1");
    }
    tracing::debug!(id = %inst.id, ?argv, cap_secs, "quality check: start");
    match cmd.spawn() {
        Ok(child) => Ok(InFlight {
            inst,
            child,
            started: t0,
            cap_secs,
            stdout_path,
            stderr_path,
        }),
        Err(e) => Err(InstrumentRun {
            judgement: Judgement::never_ran(
                inst.id.clone(),
                Reason::new(format!("cannot run `{}`: {e}", argv.join(" ")))
                    .expect("a command line is never a placeholder"),
            ),
            secs: t0.elapsed().as_secs(),
            exit_code: None,
            tail: String::new(),
        }),
    }
}

/// Read the verdict of a finished child.
///
/// TWO verdict sources, because this repo has two conventions and neither can
/// express the other. Every gate and script says pass/fail with an exit code;
/// the eight check lanes SAY a [`Judgement`] on their last stdout line,
/// because an exit code cannot express could-not-judge and `bench all` exits
/// 1 for regressed, stale AND missing-baseline.
fn finish(f: InFlight<'_>, status: std::process::ExitStatus) -> InstrumentRun {
    let secs = f.started.elapsed().as_secs();
    let code = status.code();
    let captured = match std::fs::read_to_string(&f.stdout_path) {
        Ok(c) => c,
        // An unreadable log is not silence. Defaulting to `""` here would
        // report "it printed nothing" about something that may have said
        // everything (ARCH §18.3).
        Err(e) => {
            return InstrumentRun {
                judgement: Judgement::could_not_judge(
                    f.inst.id.clone(),
                    Reason::new(format!("cannot read {}: {e}", f.stdout_path.display()))
                        .expect("a path is never a placeholder"),
                ),
                secs,
                exit_code: code,
                tail: String::new(),
            };
        }
    };
    let stderr = std::fs::read_to_string(&f.stderr_path).unwrap_or_default();
    tracing::debug!(id = %f.inst.id, ?code, secs, "quality check: exit");

    let judgement = match f.inst.verdict {
        VerdictSource::JudgementLine => match lane_verdict::from_stdout(&captured) {
            Ok(j) => {
                // It names its own subject; the TABLE is keyed on the id, so
                // one that answers about something else would put an
                // unrelated row in the operator's report.
                if j.subject() == f.inst.id {
                    j
                } else {
                    Judgement::could_not_judge(
                        f.inst.id.clone(),
                        Reason::new(format!(
                            "the verdict line names subject `{}`, not `{}`",
                            j.subject(),
                            f.inst.id
                        ))
                        .expect("never a placeholder"),
                    )
                }
            }
            Err(e) => Judgement::never_ran(
                f.inst.id.clone(),
                Reason::new(format!(
                    "{e} (exit {}) — see {}",
                    code.map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    f.stdout_path.display()
                ))
                .expect("never a placeholder"),
            ),
        },
        VerdictSource::ExitCode => match code {
            Some(0) => Judgement::passed(
                f.inst.id.clone(),
                Reason::new(format!("exit 0 in {secs}s")).expect("never a placeholder"),
            ),
            // Declared, never inferred — `concept-gate` exits 3 and 4 when
            // the graph cannot judge the commit in front of it, and a runner
            // guessing a range would get some other gate wrong.
            Some(c) if f.inst.could_not_judge_exits.contains(&c) => Judgement::could_not_judge(
                f.inst.id.clone(),
                Reason::new(format!(
                    "exit {c} — declared as could-not-judge; see {}",
                    f.stdout_path.display()
                ))
                .expect("never a placeholder"),
            ),
            Some(c) => Judgement::failed(
                f.inst.id.clone(),
                Reason::new(format!("exit {c} — see {}", f.stdout_path.display()))
                    .expect("never a placeholder"),
            ),
            // Killed by a signal. Not a failure it EARNED, and not a pass.
            None => Judgement::could_not_judge(
                f.inst.id.clone(),
                Reason::new(format!(
                    "killed by a signal — see {}",
                    f.stdout_path.display()
                ))
                .expect("never a placeholder"),
            ),
        },
    };
    // stdout first, then stderr: the ratchets print their findings and their
    // fix command to whichever one they chose, and the runner does not get to
    // decide which is the real output.
    let joined = format!("{captured}\n{stderr}");
    InstrumentRun {
        judgement: judgement.as_of(SystemTime::now()),
        secs,
        exit_code: code,
        tail: tail_of(&joined, 12),
    }
}

// ─── The command ────────────────────────────────────────────────────

struct Args {
    /// `--lane <id>` — narrow the trigger's selection to these ids.
    ids: Vec<String>,
    /// `--trigger <venue>`. `None` means [`DEFAULT_VENUE`].
    trigger: Option<String>,
    /// `--budget-secs <n>` overrides the trigger's declared budget.
    budget_secs: Option<u64>,
    mint: bool,
    table: Option<PathBuf>,
    /// `--dry-run` prints the selection and runs nothing.
    dry_run: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        ids: Vec::new(),
        trigger: None,
        budget_secs: None,
        mint: false,
        table: None,
        dry_run: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lane" | "--id" => {
                let v = args.get(i + 1).ok_or("--lane needs an instrument id")?;
                out.ids.push(v.clone());
                i += 1;
            }
            "--trigger" => {
                let v = args.get(i + 1).ok_or("--trigger needs a venue")?;
                out.trigger = Some(v.clone());
                i += 1;
            }
            "--budget-secs" => {
                let v = args.get(i + 1).ok_or("--budget-secs needs a number")?;
                out.budget_secs = Some(
                    v.parse()
                        .map_err(|_| format!("--budget-secs: `{v}` is not a number"))?,
                );
                i += 1;
            }
            "--mint" => out.mint = true,
            "--dry-run" => out.dry_run = true,
            "--lane-table" | "--registry" => {
                let v = args.get(i + 1).ok_or("--registry needs a path")?;
                out.table = Some(PathBuf::from(v));
                i += 1;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
        i += 1;
    }
    Ok(out)
}

/// Which lanes have a baseline captured against THIS stack.
fn comparable_baselines(repo: &Path, lanes: &[&Instrument], fp: &Fingerprint) -> usize {
    lanes
        .iter()
        .filter(|l| {
            baseline_dir(l).is_some_and(|d| repo.join(d).join(&fp.hex).join("latest.json").exists())
        })
        .count()
}

/// The per-fingerprint baseline directory, when the instrument declares one.
///
/// `check-lanes.toml`'s `baseline_dir` merged into `baseline` as a fourth
/// kind rather than surviving as a second field: one question, one field
/// (ARCH §10.6). A `count` or `metrics` baseline is a different currency and
/// is NOT a fingerprint directory, which is why this matches on the kind
/// rather than reading `path` wherever it finds one.
fn baseline_dir(i: &Instrument) -> Option<&str> {
    match i.baseline.kind {
        kernel_types::quality::BaselineKind::Fingerprint => i.baseline.path.as_deref(),
        _ => None,
    }
}

/// The changed paths a venue supplied, from `SOVEREIGN_CHANGED_PATHS`
/// (colon-separated — the spelling `scripts/sovereign-lint.sh` already reads).
///
/// `None` and "an empty set" are different answers and the difference gates a
/// whole selection: unset means the venue offered no change set and every
/// instrument is selected, while an empty value means it offered one and it
/// was empty. Collapsing them would silently run nothing (ARCH §18.3).
fn changed_paths() -> Option<Vec<String>> {
    let raw = std::env::var("SOVEREIGN_CHANGED_PATHS").ok()?;
    Some(
        raw.split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// Whether an instrument's `when_changed` matches anything the venue changed.
///
/// A malformed pattern SELECTS the instrument rather than dropping it. The
/// safe direction is running something unnecessary; the unsafe one is a gate
/// silently vanishing because a regex had a typo.
fn selected_by_change(inst: &Instrument, changed: Option<&Vec<String>>) -> bool {
    let (Some(pat), Some(changed)) = (inst.when_changed.as_deref(), changed) else {
        return true;
    };
    match regex::Regex::new(pat) {
        Ok(re) => changed.iter().any(|p| re.is_match(p)),
        Err(e) => {
            eprintln!("warning: {}: when_changed `{pat}` will not compile ({e}) — selecting it rather than dropping it", inst.id);
            true
        }
    }
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
            println!("Usage: svrn quality check [--trigger <venue>] [--lane <id>]... [--dry-run]");
            println!("       svrn quality lane <id>");
            println!();
            println!("  check   Run one VENUE's selection from quality/instruments.toml and");
            println!("          write the table to target/quality-check/<stamp>/summary.json");
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
        eprintln!("error: `svrn quality check` reads {REGISTRY} — run it from a source checkout");
        return 2;
    };
    let table_path = parsed.table.clone().unwrap_or_else(|| repo.join(REGISTRY));
    let text = match std::fs::read_to_string(&table_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", table_path.display());
            return 2;
        }
    };
    // A malformed row REFUSES the whole file rather than being skipped: one
    // silently dropped from a check is one whose absence reads as a pass.
    let registry = match Registry::parse(&text) {
        Ok(r) => r,
        Err(errs) => {
            eprintln!("error: {} is not valid:", table_path.display());
            for e in &errs {
                eprintln!("  ✗ {e}");
            }
            return 2;
        }
    };

    // ── Selection ───────────────────────────────────────────────────
    let venue_word = parsed
        .trigger
        .clone()
        .unwrap_or_else(|| DEFAULT_VENUE.label());
    let Some(venue) = RunsIn::parse(&venue_word) else {
        eprintln!("error: `{venue_word}` is not a venue. See `runs_in` in {REGISTRY}.");
        return 2;
    };
    let Some(trigger) = registry.trigger(&venue).cloned() else {
        eprintln!(
            "error: {REGISTRY} declares no [[trigger]] for `{venue_word}`. A venue with no run \
             policy has no budget, no concurrency and no failure disposition — it is not \
             runnable, and guessing one here would invent the very thing the table exists to \
             declare."
        );
        return 2;
    };
    let in_venue = registry.for_venue(&venue);
    for want in &parsed.ids {
        if !in_venue.iter().any(|l| &l.id == want) {
            eprintln!(
                "error: no instrument `{want}` runs in `{venue_word}`. Declared there: {}",
                in_venue
                    .iter()
                    .map(|l| l.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return 2;
        }
    }
    let changed = changed_paths();
    let mut deselected: Vec<&str> = Vec::new();
    let lanes: Vec<&Instrument> = in_venue
        .iter()
        .copied()
        .filter(|l| {
            if !parsed.ids.is_empty() && !parsed.ids.contains(&l.id) {
                return false;
            }
            if selected_by_change(l, changed.as_ref()) {
                true
            } else {
                deselected.push(&l.id);
                false
            }
        })
        .collect();

    let budget_secs = parsed.budget_secs.unwrap_or(trigger.budget_secs);

    if parsed.dry_run {
        print_selection(&venue_word, &trigger, &lanes, &deselected, budget_secs);
        return 0;
    }
    if lanes.is_empty() {
        // Same claim as `sovereign-test.sh`'s exit 4: nothing ran, so nothing
        // was verified, and that is never a pass.
        eprintln!(
            "nothing selected for `{venue_word}` — verified nothing.{}",
            if deselected.is_empty() {
                String::new()
            } else {
                format!(
                    " {} instrument(s) were deselected by `when_changed`: {}",
                    deselected.len(),
                    deselected.join(", ")
                )
            }
        );
        return 4;
    }

    let base = sovereign_cli_shared::urls::daemon_base_url();
    let fingerprint = match compute_fingerprint(&repo, &lanes, &base).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    // Only a venue whose instruments compare against a per-stack baseline
    // needs the fingerprint printed first; for a ratchet venue it is noise
    // about models nothing here read.
    let comparable = comparable_baselines(&repo, &lanes, &fingerprint);
    if lanes.iter().any(|l| baseline_dir(l).is_some()) {
        print!("{}", fingerprint.render());
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
    }
    if !deselected.is_empty() {
        // NAMED, never silent. An instrument left out because nothing it
        // gates changed is SELECTION, and the reader gets to see it.
        println!(
            "not selected — no changed path matches their `when_changed`: {}",
            deselected.join(", ")
        );
        println!();
    }

    let stamp = stamp_now();
    let out_dir = repo.join("target/quality-check").join(&stamp);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: cannot create {}: {e}", out_dir.display());
        return 2;
    }

    // ── Prepare, once, before anything runs ─────────────────────────
    //
    // A failed prepare step does NOT abort: its substitution is simply not
    // applied, every instrument runs its declared command, and the venue is
    // SLOW rather than blind.
    let mut applied: Vec<usize> = Vec::new();
    for (i, p) in trigger.prepare.iter().enumerate() {
        let t0 = Instant::now();
        let ok = std::process::Command::new(resolve_program(&p.command[0]))
            .args(&p.command[1..])
            .current_dir(&repo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        tracing::debug!(step = ?p.command, ok, secs = t0.elapsed().as_secs(), "quality check: prepare");
        if ok {
            applied.push(i);
        } else {
            eprintln!(
                "warning: prepare step `{}` failed — its instruments will run their declared \
                 command instead (slower, not weaker)",
                p.command.join(" ")
            );
        }
    }

    let started = Instant::now();
    let budget = Duration::from_secs(budget_secs);
    let mut results: BTreeMap<String, InstrumentRun> = BTreeMap::new();

    let queue = run_order(&lanes, trigger.concurrency);

    let mut flight: Vec<InFlight> = Vec::new();
    let mut next = 0usize;
    loop {
        // Start whatever fits.
        while flight.len() < trigger.concurrency && next < queue.len() {
            let inst = queue[next];
            next += 1;
            let remaining = budget.saturating_sub(started.elapsed()).as_secs();
            if remaining <= trigger.min_runway_secs {
                println!(
                    "── SKIP(budget)  [{}] {}",
                    inst.enforcement.label(),
                    inst.id
                );
                results.insert(
                    inst.id.clone(),
                    InstrumentRun {
                        judgement: Judgement::could_not_judge(
                            inst.id.clone(),
                            Reason::new(format!(
                                "out of budget — {remaining}s left of {budget_secs}s, and this \
                                 wants ~{}s",
                                inst.reservation_secs()
                            ))
                            .expect("never a placeholder"),
                        ),
                        secs: 0,
                        exit_code: None,
                        tail: String::new(),
                    },
                );
                continue;
            }
            // Preconditions. An unmet one is could-not-judge NAMING it, and
            // it does not run — an unmet precondition tells you nothing about
            // the code under test.
            let mut unmet: Vec<String> = Vec::new();
            for p in &inst.preconditions {
                if !check_precondition(p, &base).await {
                    unmet.push(describe_precondition(p));
                }
            }
            if !unmet.is_empty() {
                println!(
                    "── SKIP(precondition)  [{}] {} — {}",
                    inst.enforcement.label(),
                    inst.id,
                    unmet.join("; ")
                );
                results.insert(
                    inst.id.clone(),
                    InstrumentRun {
                        judgement: Judgement::could_not_judge(
                            inst.id.clone(),
                            Reason::new(format!("precondition unmet: {}", unmet.join("; ")))
                                .expect("never a placeholder"),
                        ),
                        secs: 0,
                        exit_code: None,
                        tail: String::new(),
                    },
                );
                continue;
            }
            let argv = trigger.rewrite(&inst.argv(), &applied);
            println!(
                "── RUN   [{}] {}   (budget left {remaining}s, est {}s)",
                inst.enforcement.label(),
                inst.id,
                inst.reservation_secs()
            );
            match spawn_instrument(
                inst,
                &argv,
                &repo,
                &out_dir,
                &fingerprint,
                parsed.mint,
                remaining,
            ) {
                Ok(f) => flight.push(f),
                Err(run) => {
                    report_one(inst, &run);
                    results.insert(inst.id.clone(), run);
                }
            }
        }
        if flight.is_empty() {
            if next >= queue.len() {
                break;
            }
            continue;
        }
        // Poll what is running.
        let mut done: Option<usize> = None;
        for (i, f) in flight.iter_mut().enumerate() {
            match f.child.try_wait() {
                Ok(Some(_)) => {
                    done = Some(i);
                    break;
                }
                Ok(None) => {
                    if f.started.elapsed() >= Duration::from_secs(f.cap_secs) {
                        let _ = f.child.kill();
                        let _ = f.child.wait();
                        done = Some(i);
                        break;
                    }
                }
                Err(_) => {
                    done = Some(i);
                    break;
                }
            }
        }
        match done {
            None => tokio::time::sleep(POLL).await,
            Some(i) => {
                let mut f = flight.remove(i);
                let status = match f.child.try_wait() {
                    Ok(Some(s)) => s,
                    _ => match f.child.wait() {
                        Ok(s) => s,
                        Err(e) => {
                            let run = InstrumentRun {
                                judgement: Judgement::could_not_judge(
                                    f.inst.id.clone(),
                                    Reason::new(format!("cannot wait on the process: {e}"))
                                        .expect("never a placeholder"),
                                ),
                                secs: f.started.elapsed().as_secs(),
                                exit_code: None,
                                tail: String::new(),
                            };
                            report_one(f.inst, &run);
                            results.insert(f.inst.id.clone(), run);
                            continue;
                        }
                    },
                };
                let inst = f.inst;
                let run = finish(f, status);
                report_one(inst, &run);
                results.insert(inst.id.clone(), run);
            }
        }
    }

    // ── The table ───────────────────────────────────────────────────
    let total_secs = started.elapsed().as_secs();
    let rows: Vec<Judgement> = lanes
        .iter()
        .filter_map(|l| results.get(&l.id).map(|r| r.judgement.clone()))
        .collect();
    println!();
    print!("{}", render_rows(&rows));
    if let Some(footer) = honesty_footer(&rows) {
        println!();
        println!("  {footer}");
    }
    println!();
    println!(
        "  total {total_secs}s of a {budget_secs}s budget · logs + summary: {}",
        out_dir.display()
    );

    let summary_path = out_dir.join("summary.json");
    if let Err(e) = write_summary(
        &summary_path,
        &stamp,
        &venue_word,
        &fingerprint,
        &lanes,
        &results,
        total_secs,
        budget_secs,
        comparable,
    ) {
        // The durable table is the reason this command exists. Losing it is
        // not a footnote.
        eprintln!("error: cannot write {}: {e}", summary_path.display());
        return 2;
    }

    // ── The exit code ───────────────────────────────────────────────
    //
    // Two questions, two fields. `enforcement` is the INSTRUMENT's: may this
    // row fail a run. `on_fail` / `on_could_not_judge` are the VENUE's: and
    // does the venue then stop. `pre-commit` runs real checks and still exits
    // 0; `pre-push` blocks on a gate that said no and only WARNS on one that
    // could not run on this host.
    let mut blocking: Vec<&str> = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    for l in &lanes {
        let Some(r) = results.get(&l.id) else {
            continue;
        };
        if l.enforcement != Enforcement::Hard {
            continue;
        }
        let action = match r.judgement.verdict() {
            kernel_types::Verdict::Passed => continue,
            kernel_types::Verdict::CouldNotJudge => trigger.on_could_not_judge,
            _ => trigger.on_fail,
        };
        match action {
            VenueAction::Block => blocking.push(&l.id),
            VenueAction::Report => reported.push(&l.id),
        }
    }
    if !reported.is_empty() {
        println!();
        println!(
            "  {} hard instrument(s) not passed, REPORTED not blocking here: {}",
            reported.len(),
            reported.join(", ")
        );
    }
    if !blocking.is_empty() {
        println!();
        println!("  {} blocking: {}", blocking.len(), blocking.join(", "));
        return 1;
    }
    0
}

/// The order instruments start in.
///
/// ONE SLOT: the order the author declared. That is not a default, it is the
/// answer — with a single slot the sequence decides which instruments get
/// squeezed when the budget runs out, and `quality/instruments.toml` lists the
/// eight check lanes cheapest-question-first on purpose. Sorting them by cost
/// there would spend a 30-minute budget on the 450 s chaos lane and report
/// SKIP(budget) for the seven that would have answered.
///
/// MORE THAN ONE SLOT: longest reservation first. Declared order with two
/// slots would start the 22 s compile last and leave a slot idle for most of
/// the run; longest-first is what makes `concurrency = 2` behave the way
/// `pre-push.sh`'s hand-written launch-at-:380 / wait-at-:565 did.
///
/// The `--dry-run` render prints this order, which is how the wrong one was
/// caught: the first version sorted unconditionally and put `chaos-monkey`
/// ahead of the seven cheaper lanes in a venue that has one slot.
fn run_order<'a>(lanes: &[&'a Instrument], concurrency: usize) -> Vec<&'a Instrument> {
    let mut out = lanes.to_vec();
    if concurrency > 1 {
        out.sort_by_key(|l| std::cmp::Reverse(l.reservation_secs()));
    }
    out
}

/// Print one instrument's outcome, with the tail of its output on a red.
///
/// The tail rather than a grep for `✗|FAIL|^error`: every gate in this repo
/// ends its output with its own fix command, and matching on wording no gate
/// promised to keep is the defect `scripts/lib/ci-bench-verdict.sh` is 130
/// lines of.
fn report_one(inst: &Instrument, run: &InstrumentRun) {
    println!(
        "── {}  [{}] {}   ({}s)",
        run.judgement.verdict(),
        inst.enforcement.label(),
        inst.id,
        run.secs
    );
    if run.judgement.verdict() != kernel_types::Verdict::Passed && !run.tail.is_empty() {
        for line in run.tail.lines() {
            println!("     {line}");
        }
    }
}

/// The `--dry-run` render: what WOULD run, in the order it would run, with
/// the budget arithmetic visible.
///
/// This is the cheapest instrument for the selection itself (ARCH §18.1). A
/// selection that silently picks the wrong set produces a green venue that
/// checked nothing, and the only way to watch that fail is to print it.
fn print_selection(
    venue: &str,
    trigger: &Trigger,
    lanes: &[&Instrument],
    deselected: &[&str],
    budget_secs: u64,
) {
    println!(
        "trigger {venue}: budget {budget_secs}s · concurrency {} · min runway {}s · \
         on_fail {} · on_could_not_judge {}",
        trigger.concurrency,
        trigger.min_runway_secs,
        trigger.on_fail.label(),
        trigger.on_could_not_judge.label()
    );
    for p in &trigger.prepare {
        print!("  prepare: {}", p.command.join(" "));
        match &p.substitute {
            Some((from, to)) => {
                println!("   → rewrites `{}` to `{}`", from.join(" "), to.join(" "))
            }
            None => println!(),
        }
    }
    println!();
    // The SAME decider the run uses, never a second sort that could disagree
    // with it (ARCH §10.6). A dry run that prints an order the real run does
    // not take is worse than no dry run.
    let order = run_order(lanes, trigger.concurrency);
    let reserved: u64 = lanes.iter().map(|l| l.reservation_secs()).sum();
    println!(
        "{} selected, {}s reserved of {budget_secs}s ({} slot(s), so the wall floor is ~{}s)",
        lanes.len(),
        reserved,
        trigger.concurrency,
        reserved / trigger.concurrency.max(1) as u64
    );
    println!();
    println!(
        "  {:<28} {:<9} {:<10} {:>6}  {}",
        "id", "enforce", "claim", "est", "command"
    );
    for l in order {
        println!(
            "  {:<28} {:<9} {:<10} {:>5}s  {}",
            l.id,
            l.enforcement.label(),
            l.claim.label(),
            l.reservation_secs(),
            l.command
        );
    }
    if !deselected.is_empty() {
        println!();
        println!(
            "  not selected — no changed path matches their `when_changed`: {}",
            deselected.join(", ")
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn write_summary(
    path: &Path,
    stamp: &str,
    venue: &str,
    fp: &Fingerprint,
    lanes: &[&Instrument],
    results: &BTreeMap<String, InstrumentRun>,
    total_secs: u64,
    budget_secs: u64,
    comparable: usize,
) -> std::io::Result<()> {
    let lane_rows: Vec<serde_json::Value> = lanes
        .iter()
        .filter_map(|l| results.get(&l.id).map(|r| (l, r)))
        .map(|(l, r)| {
            serde_json::json!({
                "id": l.id,
                // `kind` is the SHAPE and `claim` is what sort of claim the
                // verdict is. Both are written because a reader needs both:
                // the merged registry stopped one field answering two
                // questions, and the summary must not put it back.
                "kind": l.kind.label(),
                "claim": l.claim.label(),
                "enforcement": l.enforcement.label(),
                "verdict": r.judgement.verdict().as_str(),
                "reason": r.judgement.reason().as_str(),
                "secs": r.secs,
                "est_secs": l.reservation_secs(),
                "exit_code": r.exit_code,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": "quality-check/v1",
        "stamp": stamp,
        "trigger": venue,
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
    summary: "Run one VENUE's selection from quality/instruments.toml — four verdicts, persisted.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn quality check [--trigger <venue>] [--lane <id>]... [--budget-secs <n>] [--mint] [--dry-run]",
        ),
        crate::util::help::HelpSection::Flags(&[
            (
                "--trigger <venue>",
                "Which venue's selection to run: check (default), prepush, precommit, smoke:<n>, ci:<job>, run-if-stale. The venue must have a [[trigger]] row.",
            ),
            (
                "--lane <id>",
                "Narrow the selection to this instrument (repeatable). An id that does not run in the venue is refused, never ignored.",
            ),
            (
                "--dry-run",
                "Print the selection, the run order and the budget arithmetic; run nothing.",
            ),
            (
                "--budget-secs <n>",
                "Override the trigger's declared wall budget. An instrument with less runway than the trigger's min is could-not-judge, not a pass.",
            ),
            (
                "--mint",
                "Permit lanes to write a baseline for this stack fingerprint. Without it a first run writes none.",
            ),
            (
                "--registry <path>",
                "Read a different registry (default quality/instruments.toml).",
            ),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("svrn quality check", "the eight check lanes, 30-minute budget"),
            ("svrn quality check --lane chat-ask", "the focus lane alone"),
            (
                "svrn quality check --trigger prepush --dry-run",
                "what the push gate would run, in order, against its 60s budget",
            ),
        ]),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry with one ratchet and one lane, and a trigger for each — the
    /// smallest thing that exercises BOTH verdict sources and both venues.
    const TABLE: &str = r#"
censused_surfaces = [".github/workflows"]

[[instrument]]
id = "docs-gate"
kind = "gate"
claim = "invariant"
command = "cargo xtask docs-gate"
cost_secs = 2.2
enforcement = "hard"
fidelity = "F0"
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["prepush"]
when_changed = "\\.rs$"
doc = "ARCH §1.1"

[[instrument]]
id = "chat-ask"
kind = "bench"
claim = "judged"
command = "svrn quality lane chat-ask"
argv = ["svrn", "quality", "lane", "chat-ask"]
cost_secs = 251.0
est_secs = 300
enforcement = "hard"
fidelity = "F3"
verdict = "judgement-line"
preconditions = ["port-listening:9741", "slot-decodes:primary"]
baseline = { kind = "fingerprint", path = "sovereign/bench/quality-check/baselines/chat-ask" }
bank = "sovereign/bench/quality-check/chat-ask.toml"
negative_control = "none"
runs_in = ["check"]
doc = "the focus lane"

[[trigger]]
id = "prepush"
budget_secs = 60
concurrency = 2
min_runway_secs = 1
on_fail = "block"
on_could_not_judge = "report"
[[trigger.prepare]]
command = ["cargo", "build", "--quiet", "-p", "xtask"]
substitute_from = ["cargo", "xtask"]
substitute_to = ["target/debug/xtask"]

[[trigger]]
id = "check"
budget_secs = 1800
concurrency = 1
min_runway_secs = 60
on_fail = "block"
on_could_not_judge = "block"
"#;

    fn reg() -> Registry {
        Registry::parse(TABLE).expect("parses")
    }

    /// ONE table now answers "what runs here". Before 2026-09-07 the eight
    /// lanes lived in `quality/check-lanes.toml` and everything else in
    /// `quality/instruments.toml`, with `kind`, `enforcement`, cost and
    /// `baseline` colliding on four names with different meanings (ARCH
    /// §10.6). This is the merged shape: two venues, disjoint selections,
    /// out of one file.
    #[test]
    fn one_registry_answers_both_venues_and_the_selections_are_disjoint() {
        let r = reg();
        let prepush = r.for_venue(&RunsIn::Prepush);
        let check = r.for_venue(&RunsIn::Check);
        assert_eq!(prepush.len(), 1);
        assert_eq!(check.len(), 1);
        assert_eq!(prepush[0].id, "docs-gate");
        assert_eq!(check[0].id, "chat-ask");
        // Both axes survived the merge on the lane row.
        assert_eq!(check[0].kind.label(), "bench");
        assert_eq!(check[0].claim.label(), "judged");
        // And the lane's baseline_dir is now a baseline KIND.
        assert_eq!(
            baseline_dir(check[0]),
            Some("sovereign/bench/quality-check/baselines/chat-ask")
        );
        assert_eq!(baseline_dir(prepush[0]), None);
    }

    /// The prepare step's whole value is the argv rewrite it buys: without
    /// it, eleven `cargo xtask` invocations queue on the cargo lock and the
    /// venue's declared concurrency "silently becomes a queue"
    /// (`pre-push.sh`:96, :367).
    #[test]
    fn the_hoisted_prepare_rewrites_the_gate_argv_and_only_that() {
        let r = reg();
        let t = r.trigger(&RunsIn::Prepush).expect("declared");
        let gate = r.get("docs-gate").unwrap();
        assert_eq!(
            t.rewrite(&gate.argv(), &[0]),
            vec!["target/debug/xtask", "docs-gate"]
        );
        // The build failed: the declared command runs. Slow, never wrong.
        assert_eq!(
            t.rewrite(&gate.argv(), &[]),
            vec!["cargo", "xtask", "docs-gate"]
        );
        // A lane is not a `cargo xtask` and is untouched.
        let lane = r.get("chat-ask").unwrap();
        assert_eq!(t.rewrite(&lane.argv(), &[0]), lane.argv());
    }

    /// `when_changed` is SELECTION, and an unset change set selects
    /// everything. Collapsing "the venue offered no change set" with "it
    /// offered an empty one" would silently run nothing (ARCH §18.3).
    #[test]
    fn when_changed_selects_and_an_absent_change_set_selects_everything() {
        let r = reg();
        let gate = r.get("docs-gate").unwrap();
        assert!(selected_by_change(gate, None));
        assert!(selected_by_change(
            gate,
            Some(&vec!["src/main.rs".to_string()])
        ));
        assert!(!selected_by_change(
            gate,
            Some(&vec!["README.md".to_string()])
        ));
        assert!(!selected_by_change(gate, Some(&Vec::new())));
        // An instrument with no pattern is always selected.
        let lane = r.get("chat-ask").unwrap();
        assert!(selected_by_change(
            lane,
            Some(&vec!["README.md".to_string()])
        ));
    }

    /// A pattern that will not compile SELECTS rather than drops. The safe
    /// direction is running something unnecessary; the unsafe one is a gate
    /// silently vanishing because a regex had a typo.
    #[test]
    fn a_malformed_when_changed_selects_rather_than_dropping_the_gate() {
        let broken = TABLE.replace(
            r#"when_changed = "\\.rs$""#,
            r#"when_changed = "(unclosed""#,
        );
        let r = Registry::parse(&broken).expect("parses — the pattern is only a string here");
        let gate = r.get("docs-gate").unwrap();
        assert!(selected_by_change(
            gate,
            Some(&vec!["README.md".to_string()])
        ));
    }

    /// The reservation rule is what lets one budget line serve a 0.04 s
    /// ratchet and a 450 s lane. Watched on both ends.
    #[test]
    fn a_reservation_comes_from_the_est_or_from_the_measurement() {
        let r = reg();
        assert_eq!(r.get("chat-ask").unwrap().reservation_secs(), 300);
        // 2.2 s measured, no est: reserves 3, never 2.
        assert_eq!(r.get("docs-gate").unwrap().reservation_secs(), 3);
    }

    /// The order a venue starts things in, watched BOTH ways — and the wrong
    /// way is the one the `--dry-run` render caught. Sorting unconditionally
    /// put the 450 s chaos lane ahead of seven cheaper ones in a venue with a
    /// single slot, which spends a 30-minute budget on one answer and reports
    /// SKIP(budget) for the rest.
    #[test]
    fn one_slot_keeps_the_declared_order_and_two_slots_sort_longest_first() {
        let r = reg();
        let all: Vec<&Instrument> = r.instruments.iter().collect();
        // Declared order is docs-gate (3 s) then chat-ask (300 s).
        let serial: Vec<&str> = run_order(&all, 1).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(serial, vec!["docs-gate", "chat-ask"]);
        let parallel: Vec<&str> = run_order(&all, 2).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(parallel, vec!["chat-ask", "docs-gate"]);
    }

    #[test]
    fn args_parse_and_reject() {
        let a = parse_args(&[
            "--trigger".into(),
            "prepush".into(),
            "--lane".into(),
            "chat-ask".into(),
            "--budget-secs".into(),
            "600".into(),
            "--mint".into(),
            "--dry-run".into(),
        ])
        .unwrap();
        assert_eq!(a.trigger.as_deref(), Some("prepush"));
        assert_eq!(a.ids, vec!["chat-ask"]);
        assert_eq!(a.budget_secs, Some(600));
        assert!(a.mint);
        assert!(a.dry_run);
        // No trigger means the check lanes — the behaviour before this flag
        // existed, kept.
        assert_eq!(parse_args(&[]).unwrap().trigger, None);
        assert_eq!(DEFAULT_VENUE.label(), "check");
        assert!(parse_args(&["--lane".into()]).is_err());
        assert!(parse_args(&["--trigger".into()]).is_err());
        assert!(parse_args(&["--budget-secs".into(), "soon".into()]).is_err());
        assert!(parse_args(&["--wat".into()]).is_err());
        // The default is NOT mint. A run that writes a baseline it was not
        // asked to write is the defect this command was built against.
        assert!(!parse_args(&[]).unwrap().mint);
    }

    /// `host-quiet` parses its bound, and the could-not-judge reason NAMES
    /// the load it saw rather than restating the rule.
    ///
    /// The row this guards is the reason run 1 called a 17.8 tok/s decode a
    /// FAILURE against a 50 tok/s bar at load 32. The bar was right and the
    /// machine was busy (note d596639c); a wall-clock verdict on a contended
    /// host is could-not-judge (ARCH §18.3).
    #[test]
    fn host_quiet_reason_names_the_load_and_the_predicate_reads_the_machine() {
        let described = describe_precondition(&Precondition::HostQuiet(0.001));
        assert!(
            described.contains("load average") || described.contains("quiet"),
            "the reason must speak about load: {described}"
        );
        // A bound no real host beats is unmet; one no host exceeds is met.
        // Together these show the predicate actually reads the machine
        // rather than answering a constant.
        assert!(!check_host_quiet(0.0000001));
        assert!(check_host_quiet(1.0e9));
    }

    /// `container:` is the spelling the shell harnesses had and the lane
    /// runner did not. It now has exactly one probe, like the other five.
    #[test]
    fn the_container_precondition_describes_the_toolbox_it_wants() {
        let d = describe_precondition(&Precondition::Container("sovereign-vulkan".into()));
        assert!(d.contains("sovereign-vulkan"), "{d}");
        assert!(d.contains("container"), "{d}");
    }

    /// `svrn` in a command means THIS dispatcher. A PATH lookup here would
    /// silently run whichever build an operator's symlink points at — on this
    /// host, one in a different checkout.
    #[test]
    fn svrn_in_a_command_resolves_to_the_running_dispatcher() {
        let me = std::env::current_exe().unwrap();
        for spelling in ["svrn", "sovereign", "sovereign-cli"] {
            assert_eq!(resolve_program(spelling), me, "{spelling}");
        }
        assert_eq!(resolve_program("python3"), PathBuf::from("python3"));
    }

    #[test]
    fn the_tail_is_the_last_lines_that_said_something() {
        assert_eq!(tail_of("a\n\nb\nc\n", 2), "b\nc");
        assert_eq!(tail_of("only\n", 12), "only");
        assert_eq!(tail_of("\n\n", 12), "");
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
            "[[subset]]\nsubset_id = \"z1\"\n[[subset]]\nsubset_id = \"a1\"\n\
             [[subset]]\nsubset_id = \"z1\"\n",
        )
        .unwrap();
        // Sorted, and the repeat of `z1` (one subset, two banks) counts once:
        // the fingerprint wants which subsets ran, not how many banks each
        // spans.
        assert_eq!(
            smoke_subset_ids(tmp.path()),
            Ok(vec!["a1".into(), "z1".into()])
        );
    }

    /// The subset IDs cannot see a cut. Cutting `chaos-monkey-v1` from six
    /// probes to four leaves every id and every bank hash unchanged, so
    /// without this digest the fingerprint would match a baseline captured
    /// on the six.
    #[test]
    fn changing_which_ids_a_subset_selects_moves_the_fingerprint() {
        let write = |dir: &Path, body: &str| {
            let bench = dir.join("sovereign/bench");
            std::fs::create_dir_all(&bench).unwrap();
            std::fs::create_dir_all(dir.join("quality")).unwrap();
            std::fs::write(bench.join("smoke.toml"), body).unwrap();
        };
        let six = tempfile::tempdir().unwrap();
        let four = tempfile::tempdir().unwrap();
        let reordered = tempfile::tempdir().unwrap();
        let commented = tempfile::tempdir().unwrap();
        write(
            six.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"a\",\"b\",\"e\"]\n",
        );
        write(
            four.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"a\",\"b\"]\n",
        );
        write(
            reordered.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"e\",\"b\",\"a\"]\n",
        );
        write(
            commented.path(),
            "# a reworded comment\n[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"a\",\"b\",\"e\"]\n",
        );
        // Same subset ids on both sides — the digest is the only thing that
        // separates them.
        assert_eq!(smoke_subset_ids(six.path()), smoke_subset_ids(four.path()));
        let d6 = smoke_selection_digest(six.path()).unwrap();
        assert_ne!(d6, smoke_selection_digest(four.path()).unwrap());
        // Order and comments are NOT the selection: they must not orphan a
        // baseline.
        assert_eq!(d6, smoke_selection_digest(reordered.path()).unwrap());
        assert_eq!(d6, smoke_selection_digest(commented.path()).unwrap());
        // Absent is a named answer, not a hash of nothing.
        let none = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(none.path().join("sovereign/bench")).unwrap();
        assert_eq!(
            smoke_selection_digest(none.path()),
            Ok("absent".to_string())
        );
        // A malformed row REFUSES rather than digesting to a placeholder:
        // two unreadable files must not hash the same.
        let bad = tempfile::tempdir().unwrap();
        write(
            bad.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\n",
        );
        let err = smoke_selection_digest(bad.path()).unwrap_err();
        assert!(err.contains("must declare EITHER"), "{err}");
        write(
            bad.path(),
            "[[subset]]\nbank = \"b/c/d.toml\"\nmode = \"full\"\n",
        );
        assert!(smoke_selection_digest(bad.path())
            .unwrap_err()
            .contains("no subset_id"));
    }

    /// THE REGISTRY THIS REPO ACTUALLY SHIPS parses, every declared trigger
    /// selects something, and the venues the shell harnesses delegate to are
    /// all present. A schema that only parses its own fixtures is a schema
    /// nobody has watched read the real file.
    #[test]
    fn the_shipped_registry_parses_and_every_trigger_selects_something() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../quality/instruments.toml");
        let text = std::fs::read_to_string(&path).expect("quality/instruments.toml");
        let r = match Registry::parse(&text) {
            Ok(r) => r,
            Err(e) => panic!("{}: {e:#?}", path.display()),
        };
        assert!(!r.triggers.is_empty(), "no [[trigger]] rows");
        for t in &r.triggers {
            let sel = r.for_venue(&t.id);
            assert!(
                !sel.is_empty(),
                "trigger `{}` selects nothing — it would report green having run \
                 nothing (ARCH §18.1)",
                t.id.label()
            );
        }
        // The eight lanes of `svrn quality check`, out of the one table.
        let check = r.for_venue(&RunsIn::Check);
        assert_eq!(check.len(), 8, "the check venue must hold its eight lanes");
        for l in &check {
            assert_eq!(
                l.verdict,
                VerdictSource::JudgementLine,
                "lane `{}` must SAY its verdict — an exit code cannot express \
                 could-not-judge",
                l.id
            );
        }
        // `runs_in = []` is legal in the schema and is the finding this
        // registry exists to surface. This rung closed the last nine.
        assert!(
            r.nowhere().is_empty(),
            "instruments that run nowhere: {:?}",
            r.nowhere().iter().map(|i| &i.id).collect::<Vec<_>>()
        );
    }
}
