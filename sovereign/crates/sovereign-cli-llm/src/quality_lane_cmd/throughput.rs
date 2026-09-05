// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lane `throughput` — the engine's own numbers, wrapped rather than rewritten.
//!
//! # What this lane is
//!
//! `scripts/throughput_probe.py` has measured TTFT, decode t/s, inter-token
//! latency, prefill t/s and greedy fidelity from a real SSE stream since the
//! heterogeneous-distribution experiment. It is not re-implemented here: this
//! lane runs it, reads its one JSON line, and turns the numbers into rows
//! against the bank's PRE-REGISTERED bars (ARCH §19).
//!
//! Its only caller until today was `scripts/desktop-smoke.sh::perf_probe`,
//! which compared each run against a gitignored baseline directory that does
//! not exist on this host — so in the whole repo the probe had never compared
//! anything to anything. That compare block is deleted in the same commit as
//! this file; phase 1 calls `svrn quality check --lane throughput` instead.
//!
//! # The rows
//!
//! | row | kind | what a failure means |
//! |---|---|---|
//! | probe:`<arm>` | HARD when the bank declares bars for the running stem | the slot got slower than a number written down before the run |
//! | probe:`<arm>` | TRACKED when the bank says `bars_deferred` | recorded; the operator has not set this arm's numbers |
//! | prompt size:`<arm>` | HARD | the long arm's prompt no longer costs the tokens the bank declared — the tokenizer moved, so the arm is not the arm the baseline was captured against |
//! | greedy fidelity | TRACKED | temperature-0 decode stopped being reproducible across trials |
//! | e2e | HARD on catastrophe | a plain turn through the runtime errored or came back empty |
//! | baseline | TRACKED | a metric moved against this stack's last run |
//!
//! An arm with NEITHER a bars table for the stem the daemon is serving NOR a
//! declared `bars_deferred` is **could-not-judge naming the stem**. Running a
//! different model is not evidence that this one got faster, and a deferral
//! an operator DECLARED is a different answer from a stem row that is simply
//! missing (ARCH §18.3).

use std::path::{Path, PathBuf};
use std::time::Instant;

use kernel_types::Judgement;
use sovereign_contracts::types::TurnMode;
use sovereign_turn_client::{TurnClient, TurnObserver};

use super::{reason, LaneCtx, LaneReport};
use crate::bench_cmd::lane_baseline::{self, Direction, LaneBaseline, LaneMetric};

const LANE: &str = "throughput";
const BANK: &str = "sovereign/bench/quality-check/throughput.toml";

// ─── The bank ───────────────────────────────────────────────────────

#[cfg_attr(test, derive(Debug))]
struct ThroughputBank {
    probe: PathBuf,
    long_seed: String,
    long_tail: String,
    arms: Vec<ThroughputArm>,
    e2e_question: String,
    e2e_runs: usize,
}

/// `Default` is the probe's own built-in prompt; `Long` is the salted filler
/// the bank sizes in CHARS.
///
/// Chars, not tokens, because chars are what the lane can produce exactly and
/// tokens are what the tokenizer decides. The arm then asserts the resulting
/// `prompt_tokens` against a declared band, which is the same measurement read
/// as a check on the tokenizer rather than as a hope about it.
#[cfg_attr(test, derive(Debug))]
enum ArmPrompt {
    Default,
    Long {
        chars: usize,
        tokens_min: u64,
        tokens_max: u64,
    },
}

#[cfg_attr(test, derive(Debug))]
struct ThroughputArm {
    id: String,
    slot: String,
    prompt: ArmPrompt,
    trials: usize,
    warmup: usize,
    max_tokens: u32,
    /// `bars[<model stem>]` — a HARD table for that stem, or nothing.
    bars: toml::value::Table,
    /// Declared by an operator: this arm records and does not gate.
    bars_deferred: bool,
}

/// Parse the bank. Every field is required unless the doc above says
/// otherwise: a bank that half-parses drops an assertion, and a dropped
/// assertion reads exactly like a passing one.
///
/// `Throughput`-prefixed for the same reason `ChatAskBank` is — `Bank` and
/// `Arm` are both already taken in this crate by concepts that are not this
/// one.
fn parse_bank(text: &str) -> Result<ThroughputBank, String> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| format!("{BANK}: {e}"))?;
    let want = |t: &toml::Value, k: &str| -> Result<toml::Value, String> {
        t.get(k)
            .cloned()
            .ok_or_else(|| format!("{BANK}: missing `{k}`"))
    };
    let s_of = |t: &toml::Value, k: &str| -> Result<String, String> {
        want(t, k)?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{BANK}: `{k}` must be a string"))
    };
    let u_of = |t: &toml::Value, k: &str| -> Result<u64, String> {
        want(t, k)?
            .as_integer()
            .and_then(|v| u64::try_from(v).ok())
            .ok_or_else(|| format!("{BANK}: `{k}` must be a non-negative integer"))
    };
    // Widened, never clamped. A `trials` that does not fit a `usize` is a
    // malformed bank, and `unwrap_or(0)` would turn it into an arm that runs
    // zero trials and reports nothing — a refusal read as a measurement
    // (ARCH §18.3, and the same claim `sovereign-test.sh`'s zero-test exit 4
    // makes).
    let n_of = |t: &toml::Value, k: &str| -> Result<usize, String> {
        usize::try_from(u_of(t, k)?)
            .map_err(|_| format!("{BANK}: `{k}` does not fit this platform's usize"))
    };

    let bank_t = want(&doc, "bank")?;
    let long_t = want(&doc, "long")?;
    let e2e_t = want(&doc, "e2e")?;

    let arms_v = want(&doc, "arm")?;
    let arms_a = arms_v
        .as_array()
        .ok_or_else(|| format!("{BANK}: `arm` must be an array of tables"))?;
    if arms_a.is_empty() {
        return Err(format!("{BANK}: declares no arms"));
    }
    let mut arms = Vec::new();
    for a in arms_a {
        let id = s_of(a, "id")?;
        let prompt = match s_of(a, "prompt")?.as_str() {
            "default" => ArmPrompt::Default,
            "long" => ArmPrompt::Long {
                chars: n_of(a, "chars")?,
                tokens_min: u_of(a, "prompt_tokens_min")?,
                tokens_max: u_of(a, "prompt_tokens_max")?,
            },
            // Refused, never defaulted to the cheap arm (ARCH §18.3).
            other => {
                return Err(format!(
                    "{BANK}: arm `{id}` declares prompt `{other}`; known: default, long"
                ))
            }
        };
        arms.push(ThroughputArm {
            slot: s_of(a, "slot")?,
            prompt,
            trials: n_of(a, "trials")?,
            warmup: n_of(a, "warmup")?,
            max_tokens: u32::try_from(u_of(a, "max_tokens")?)
                .map_err(|_| format!("{BANK}: arm `{id}` max_tokens is out of range"))?,
            bars: a
                .get("bars")
                .and_then(|v| v.as_table())
                .cloned()
                .unwrap_or_default(),
            bars_deferred: a
                .get("bars_deferred")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false),
            id,
        });
    }

    Ok(ThroughputBank {
        probe: PathBuf::from(s_of(&bank_t, "probe")?),
        long_seed: s_of(&long_t, "seed")?,
        long_tail: s_of(&long_t, "tail")?,
        arms,
        e2e_question: s_of(&e2e_t, "question")?,
        e2e_runs: n_of(&e2e_t, "runs")?,
    })
}

// ─── The probe ──────────────────────────────────────────────────────

/// What one `throughput_probe.py --json` invocation reported. Every field is
/// `Option` because the probe itself reports `null` for a metric it could not
/// compute (a non-streaming fallback has no TTFT), and a missing number is
/// reported as missing rather than substituted (ARCH §18.3).
struct ProbeResult {
    decode_tps: Option<f64>,
    ttft_ms: Option<f64>,
    prefill_tps: Option<f64>,
    itl_p95_ms: Option<f64>,
    prompt_tokens: Option<u64>,
    greedy_identical: Option<bool>,
}

fn f64_at(v: &serde_json::Value, k: &str) -> Option<f64> {
    v.get(k)?.as_f64()
}

/// Build the salted long prompt.
///
/// The salt goes FIRST, in the first bytes, because the daemon's cache is a
/// PREFIX cache: a salt anywhere else leaves the cached prefix intact and the
/// arm measures the cache. Measured on this host 2026-09-04 — the same 5,917-
/// token prompt cost 44,727 ms cold and 375 ms repeated.
fn long_prompt(bank: &ThroughputBank, chars: usize, salt: u128) -> String {
    let mut s = format!("Run {salt}.\n");
    while s.len() < chars {
        s.push_str(&bank.long_seed);
    }
    s.truncate(chars);
    s.push_str(&bank.long_tail);
    s
}

/// Run the probe for one arm. `Err` names what went wrong; it is never a
/// zeroed `ProbeResult`.
fn run_probe(
    repo: &Path,
    bank: &ThroughputBank,
    arm: &ThroughputArm,
    base_url: &str,
) -> Result<ProbeResult, String> {
    let probe = repo.join(&bank.probe);
    if !probe.is_file() {
        return Err(format!("{} is not on disk", probe.display()));
    }
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&probe)
        .arg("--url")
        .arg(base_url)
        .arg("--model")
        .arg(&arm.slot)
        .arg("--max-tokens")
        .arg(arm.max_tokens.to_string())
        .arg("--trials")
        .arg(arm.trials.to_string())
        .arg("--warmup")
        .arg(arm.warmup.to_string())
        .arg("--label")
        .arg(&arm.id)
        .arg("--json")
        .current_dir(repo);

    // The long prompt is staged as a file rather than an argv string: 43 KB
    // of prompt on a command line is a portability trap, and `--prompt-file`
    // is a door the probe already has.
    let staged = match &arm.prompt {
        ArmPrompt::Default => None,
        ArmPrompt::Long { chars, .. } => {
            let salt = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = tempfile::tempdir().map_err(|e| format!("cannot stage the prompt: {e}"))?;
            let path = dir.path().join("long.txt");
            std::fs::write(&path, long_prompt(bank, *chars, salt))
                .map_err(|e| format!("cannot write the staged prompt: {e}"))?;
            cmd.arg("--prompt-file").arg(&path);
            Some(dir)
        }
    };

    tracing::debug!(lane = LANE, arm = %arm.id, slot = %arm.slot, "throughput: probe start");
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run `python3 {}`: {e}", probe.display()))?;
    drop(staged);
    if !out.status.success() {
        let tail: String = String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(format!(
            "the probe exited {}: {tail}",
            out.status.code().unwrap_or(-1)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "the probe printed no JSON line".to_string())?;
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("the probe's last line is not JSON: {e}"))?;
    Ok(ProbeResult {
        decode_tps: f64_at(&v, "decode_tps_median"),
        ttft_ms: f64_at(&v, "ttft_ms_median"),
        prefill_tps: f64_at(&v, "prefill_tps"),
        itl_p95_ms: f64_at(&v, "itl_p95_ms"),
        prompt_tokens: v.get("prompt_tokens").and_then(serde_json::Value::as_u64),
        greedy_identical: v
            .get("greedy_identical")
            .and_then(serde_json::Value::as_bool),
    })
}

// ─── The bars ───────────────────────────────────────────────────────

/// The three answers an arm's bars can give. `Deferred` is a DECLARED
/// absence and `NoStemRow` is an undeclared one; collapsing them would let a
/// model swap read as an operator's decision.
enum Bars<'a> {
    Declared(&'a toml::value::Table),
    Deferred,
    NoStemRow,
}

fn bars_for<'a>(arm: &'a ThroughputArm, stem: Option<&str>) -> Bars<'a> {
    if let Some(t) = stem
        .and_then(|s| arm.bars.get(s))
        .and_then(toml::Value::as_table)
    {
        return Bars::Declared(t);
    }
    if arm.bars_deferred {
        return Bars::Deferred;
    }
    Bars::NoStemRow
}

fn bar_f64(t: &toml::value::Table, k: &str) -> Option<f64> {
    t.get(k)
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
}

/// The measured numbers, as one sentence. Absent metrics say `n/a` — the
/// probe's own answer for "could not compute", carried through rather than
/// zeroed.
fn describe(p: &ProbeResult) -> String {
    let f = |v: Option<f64>, unit: &str| match v {
        Some(x) if unit == "ms" => format!("{x:.0} {unit}"),
        Some(x) => format!("{x:.1} {unit}"),
        None => format!("n/a {unit}"),
    };
    format!(
        "TTFT {} · decode {} · prefill {} · ITL p95 {}",
        f(p.ttft_ms, "ms"),
        f(p.decode_tps, "tok/s"),
        f(p.prefill_tps, "tok/s"),
        f(p.itl_p95_ms, "ms"),
    )
}

/// One arm's row against its bars.
fn arm_row(report: &mut LaneReport, arm: &ThroughputArm, stem: Option<&str>, p: &ProbeResult) {
    let subject = format!("probe:{}", arm.id);
    let numbers = describe(p);
    match bars_for(arm, stem) {
        Bars::NoStemRow => report.cannot_judge(
            &subject,
            format!(
                "no bars for model stem `{}` in {BANK}, and none deferred — {numbers}",
                stem.unwrap_or("unresolved")
            ),
        ),
        Bars::Deferred => report.passed(
            &subject,
            format!("tracked, bars deferred to the operator — {numbers}"),
        ),
        Bars::Declared(t) => {
            let mut blown = Vec::new();
            let mut unmeasured = Vec::new();
            if let Some(max) = bar_f64(t, "ttft_ms_max") {
                match p.ttft_ms {
                    Some(v) if v > max => blown.push(format!("TTFT {v:.0} ms > {max:.0}")),
                    Some(_) => {}
                    None => unmeasured.push("ttft_ms"),
                }
            }
            if let Some(min) = bar_f64(t, "decode_tps_min") {
                match p.decode_tps {
                    Some(v) if v < min => {
                        blown.push(format!("decode {v:.1} tok/s < {min:.1}"));
                    }
                    Some(_) => {}
                    None => unmeasured.push("decode_tps"),
                }
            }
            if let Some(min) = bar_f64(t, "prefill_tps_min") {
                match p.prefill_tps {
                    Some(v) if v < min => {
                        blown.push(format!("prefill {v:.1} tok/s < {min:.1}"));
                    }
                    Some(_) => {}
                    None => unmeasured.push("prefill_tps"),
                }
            }
            // A bar whose metric the probe could not compute was not met —
            // it was not judged, and saying otherwise is the flattering
            // direction (ARCH §18.3).
            if !unmeasured.is_empty() {
                report.cannot_judge(
                    &subject,
                    format!(
                        "the probe reported no {} on `{}`, so its bar was not judged — {numbers}",
                        unmeasured.join(", "),
                        stem.unwrap_or("unresolved")
                    ),
                );
            } else if blown.is_empty() {
                report.passed(
                    &subject,
                    format!("within the bars for `{}` — {numbers}", stem.unwrap_or("?")),
                );
            } else {
                report.failed(&subject, format!("{} — {numbers}", blown.join("; ")));
            }
        }
    }
}

/// The long arms' prompt-size row: the tokenizer did not move under us.
fn prompt_size_row(report: &mut LaneReport, arm: &ThroughputArm, p: &ProbeResult) {
    let ArmPrompt::Long {
        tokens_min,
        tokens_max,
        ..
    } = arm.prompt
    else {
        return;
    };
    let subject = format!("prompt size:{}", arm.id);
    match p.prompt_tokens {
        None => report.cannot_judge(
            &subject,
            format!("the probe reported no prompt_tokens; the declared band is {tokens_min}-{tokens_max}"),
        ),
        Some(n) if n < tokens_min || n > tokens_max => report.failed(
            &subject,
            format!("the staged prompt cost {n} tokens, outside the declared {tokens_min}-{tokens_max}"),
        ),
        Some(n) => report.passed(
            &subject,
            format!("{n} tokens, inside the declared {tokens_min}-{tokens_max}"),
        ),
    }
}

// ─── The lane ───────────────────────────────────────────────────────

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: svrn quality lane throughput");
        println!();
        println!("Runs scripts/throughput_probe.py for every arm {BANK} declares,");
        println!("asserts each against that arm's pre-registered bars, drives two");
        println!("end-to-end turns through the runtime, and compares every metric");
        println!("against this stack's baseline.");
        return 0;
    }
    let ctx = LaneCtx::from_env();
    let mut report = LaneReport::new(LANE);

    let Some(repo) = find_repo_root() else {
        report.cannot_judge(
            "bank",
            "the lane reads a repo-relative bank; run it from a source checkout".into(),
        );
        return report.finish();
    };
    let bank_path = repo.join(BANK);
    let bank = match std::fs::read_to_string(&bank_path)
        .map_err(|e| format!("{}: {e}", bank_path.display()))
        .and_then(|t| parse_bank(&t))
    {
        Ok(b) => b,
        Err(e) => {
            report.cannot_judge("bank", e);
            return report.finish();
        }
    };

    let base = sovereign_cli_shared::urls::daemon_base_url();
    let mut metrics: Vec<(String, LaneMetric)> = Vec::new();
    let mut fidelity: Vec<(String, bool)> = Vec::new();

    for arm in &bank.arms {
        let stem = slot_stem(&arm.slot);
        let t0 = Instant::now();
        match run_probe(&repo, &bank, arm, &base) {
            Err(e) => {
                report.cannot_judge(&format!("probe:{}", arm.id), e);
            }
            Ok(p) => {
                eprintln!(
                    "  [{LANE}] {} — {} ({} s)",
                    arm.id,
                    describe(&p),
                    t0.elapsed().as_secs()
                );
                arm_row(&mut report, arm, stem.as_deref(), &p);
                prompt_size_row(&mut report, arm, &p);
                if let Some(v) = p.ttft_ms {
                    metrics.push((
                        format!("{}.ttft_ms", arm.id),
                        LaneMetric::lower_is_better(v, 0.25),
                    ));
                }
                if let Some(v) = p.decode_tps {
                    metrics.push((
                        format!("{}.decode_tps", arm.id),
                        LaneMetric::higher_is_better(v, 0.10),
                    ));
                }
                if let Some(v) = p.prefill_tps {
                    metrics.push((
                        format!("{}.prefill_tps", arm.id),
                        LaneMetric::higher_is_better(v, 0.25),
                    ));
                }
                if let Some(v) = p.greedy_identical {
                    fidelity.push((arm.id.clone(), v));
                }
            }
        }
    }

    fidelity_row(&mut report, &fidelity);
    e2e_row(&mut report, &bank, &base, &mut metrics).await;
    baseline_row(&mut report, &ctx, metrics);

    report.finish()
}

/// Greedy fidelity across an arm's trials. TRACKED, not gated: this host
/// decodes with MTP, and whether speculative acceptance is bit-reproducible
/// run to run has not been measured over a week here. It is recorded so the
/// promotion to HARD can be made on evidence rather than on a guess — the
/// same rule `pre-push.sh` applies to its two advisory ratchets.
fn fidelity_row(report: &mut LaneReport, fidelity: &[(String, bool)]) {
    if fidelity.is_empty() {
        report.cannot_judge(
            "greedy fidelity",
            "no arm reported greedy_identical, so reproducibility was not observed".into(),
        );
        return;
    }
    let diverged: Vec<&str> = fidelity
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(id, _)| id.as_str())
        .collect();
    // A single-trial arm cannot diverge from itself; it is reported as such
    // rather than counted as evidence of reproducibility.
    if diverged.is_empty() {
        report.passed(
            "greedy fidelity",
            format!(
                "identical across trials on {} arm(s) at temperature 0",
                fidelity.len()
            ),
        );
    } else {
        report.passed(
            "greedy fidelity",
            format!(
                "tracked: temperature-0 decode DIVERGED across trials on {}",
                diverged.join(", ")
            ),
        );
    }
}

/// Two plain turns through the runtime, with no corpus NAMED.
///
/// The probe measures the ENGINE. This measures what a user waits for: the
/// router, the policy, the synthesis and every gate between them. An errored
/// or empty turn is a catastrophe and fails; the latency is TRACKED, because
/// the order pre-registered no bar for it.
async fn e2e_row(
    report: &mut LaneReport,
    bank: &ThroughputBank,
    base: &str,
    metrics: &mut Vec<(String, LaneMetric)>,
) {
    if bank.e2e_runs == 0 {
        report.cannot_judge("e2e", "the bank declares no end-to-end runs".into());
        return;
    }
    let client = TurnClient::new(base);
    let mut latencies: Vec<f64> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for i in 0..bank.e2e_runs {
        match e2e_once(&client, &bank.e2e_question).await {
            Ok((ms, chars)) => {
                eprintln!("  [{LANE}] e2e run {}/{} — {ms} ms", i + 1, bank.e2e_runs);
                if chars == 0 {
                    failures.push(format!("run {} came back empty", i + 1));
                } else {
                    latencies.push(ms as f64);
                }
            }
            Err(e) => failures.push(format!("run {}: {e}", i + 1)),
        }
    }
    if !failures.is_empty() {
        report.failed(
            "e2e",
            format!(
                "{} of {} plain turns did not complete: {}",
                failures.len(),
                bank.e2e_runs,
                failures.join("; ")
            ),
        );
        return;
    }
    let Some(med) = median(latencies.clone()) else {
        report.cannot_judge("e2e", "no turn reported a latency".into());
        return;
    };
    metrics.push((
        "e2e.total_latency_ms".to_string(),
        LaneMetric::lower_is_better(med, 0.30),
    ));
    report.passed(
        "e2e",
        format!(
            "{} plain turns completed; median total_latency_ms {med:.0} (tracked, no bar)",
            latencies.len()
        ),
    );
}

/// One plain turn. Returns `(total_latency_ms, visible chars)`.
///
/// `total_latency_ms` is the runtime's OWN number off the provenance block,
/// not this process's wall clock — the wall clock also times the HTTP hop and
/// this lane is about the engine, not about the loopback.
async fn e2e_once(client: &TurnClient, question: &str) -> Result<(u64, usize), String> {
    // No allow-list. The daemon REFUSES an empty one ("enabled_corpora names
    // no corpus — an empty allow-list would search nothing"), so "no corpus"
    // here means what it means for `chat ask`: no corpus was NAMED, and the
    // question is a plain one that no installed corpus answers. This arm is
    // about the runtime's own path — router, policy, synthesis, gate — not
    // about retrieval hitting or missing.
    let convo = client
        .create_conversation(None, None)
        .await
        .map_err(|e| format!("create_conversation: {e}"))?;
    let t0 = Instant::now();
    let mut observer = TurnObserver::default();
    let outcome = client
        .run_turn(&convo.id, question, TurnMode::Grounded, None, &mut observer)
        .await
        .map_err(|e| format!("run_turn: {e}"))?;
    let wall = t0.elapsed().as_millis() as u64;
    let ms = outcome
        .provenance
        .as_ref()
        .and_then(|p| p.total_ms)
        .filter(|v| *v > 0)
        .unwrap_or(wall);
    Ok((ms, outcome.text.trim().len()))
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(xs[xs.len() / 2])
}

// ─── The baseline ───────────────────────────────────────────────────

/// TRACKED movement against `<baseline_dir>/<fingerprint>/latest.json`, as
/// `LaneBaseline`/`LaneMetric` — the shape every other bench lane already
/// stores, with its direction and tolerance per metric (ARCH §19).
///
/// A run whose stack has no baseline writes NOTHING unless `--mint`. That is
/// the runner's rule, and a lane that quietly minted its own would make the
/// first run of a changed stack look like a comparison.
fn baseline_row(report: &mut LaneReport, ctx: &LaneCtx, metrics: Vec<(String, LaneMetric)>) {
    if metrics.is_empty() {
        report.cannot_judge(
            "baseline",
            "no metric was measured, so nothing can be compared".into(),
        );
        return;
    }
    let n = metrics.len();
    let mut current = LaneBaseline::new(LANE, lane_baseline_now());
    current.attribute(slot_stem("primary").as_deref());
    for (k, m) in metrics {
        current = current.with(k, m);
    }

    let (Some(fp), Some(dir)) = (ctx.fingerprint.as_deref(), ctx.baseline_dir.as_deref()) else {
        report.cannot_judge(
            "baseline",
            format!(
                "no fingerprint or baseline dir for this run ({n} metrics measured) — \
                 run under `svrn quality check` to compare"
            ),
        );
        return;
    };
    let path = dir.join(fp).join("latest.json");
    if !path.exists() {
        if ctx.mint {
            let wrote =
                std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).and_then(|()| {
                    std::fs::write(
                        &path,
                        format!("{}\n", serde_json::to_string_pretty(&current)?),
                    )
                });
            match wrote {
                Ok(()) => report.cannot_judge(
                    "baseline",
                    format!(
                        "first run for stack `{fp}` — minted {} with {n} metrics (--mint). \
                         Nothing was compared",
                        path.display()
                    ),
                ),
                Err(e) => report.cannot_judge(
                    "baseline",
                    format!("first run for stack `{fp}`, and --mint could not write it: {e}"),
                ),
            }
        } else {
            report.cannot_judge(
                "baseline",
                format!(
                    "first run for stack `{fp}` — no baseline at {}, and this run wrote none \
                     (pass --mint to set one)",
                    path.display()
                ),
            );
        }
        return;
    }
    let prev = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<LaneBaseline>(&t).map_err(|e| e.to_string()))
    {
        Ok(b) => b,
        Err(e) => {
            report.cannot_judge(
                "baseline",
                format!("the baseline at {} is unreadable: {e}", path.display()),
            );
            return;
        }
    };
    let d = lane_baseline::diff(Some(&prev), &current);
    if let Some((was, now)) = &d.model_mismatch {
        // INCOMPARABLE is `lane_baseline`'s own answer for this and it is
        // the right one: a number from another model is not this model's
        // number.
        report.cannot_judge(
            "baseline",
            format!("the baseline was captured on `{was}` and this run served `{now}`"),
        );
        return;
    }
    let moved: Vec<String> = d
        .regressions()
        .map(|m| {
            let arrow = match m.direction {
                Direction::HigherIsBetter => "fell",
                _ => "rose",
            };
            format!("{} {arrow} {:.1} → {:.1}", m.name, m.baseline, m.current)
        })
        .collect();
    // TRACKED, always passed: the nightly is where drift is judged, and this
    // lane is about breakage. The numbers are in the reason either way.
    report.push(Judgement::passed(
        "baseline",
        reason(if moved.is_empty() {
            format!(
                "{} metrics within tolerance of {}",
                d.deltas.len(),
                path.display()
            )
        } else {
            format!(
                "tracked movement vs {}: {}",
                path.display(),
                moved.join("; ")
            )
        }),
    ));
}

/// RFC-3339, the same stamp `bench_cmd::report` writes into every other
/// lane baseline — provenance only, never compared.
fn lane_baseline_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The GGUF stem a slot alias resolves to on THIS host, from the setup
/// config — the same door `chat_ask::model_stem` uses. `None` rather than a
/// guess: an unresolved stem makes an arm could-not-judge, which is the
/// honest verdict when nobody can say which model produced the number.
fn slot_stem(slot: &str) -> Option<String> {
    let cfg = sovereign_contracts::setup_config::SetupConfig::load().ok()?;
    let models = cfg.models.as_ref()?;
    match slot {
        "fast" => models.fast_stem(),
        _ => models.primary_stem(),
    }
}

/// Walk up to the enclosing checkout. Same shape as `chat_ask`'s.
fn find_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("quality").is_dir() && dir.join("sovereign").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bank_text() -> String {
        std::fs::read_to_string(
            find_repo_root()
                .expect("tests run inside the checkout")
                .join(BANK),
        )
        .expect("the shipped bank is on disk")
    }

    #[test]
    fn the_shipped_bank_parses_and_declares_four_arms() {
        let b = parse_bank(&bank_text()).expect("the shipped bank parses");
        let ids: Vec<&str> = b.arms.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["primary/short", "fast/short", "primary/long", "fast/long"]
        );
        assert_eq!(b.probe, PathBuf::from("scripts/throughput_probe.py"));
        assert_eq!(b.e2e_runs, 2);
    }

    #[test]
    fn the_pre_registered_bars_are_the_orders_numbers() {
        let b = parse_bank(&bank_text()).unwrap();
        let short = b.arms.iter().find(|a| a.id == "primary/short").unwrap();
        let t = match bars_for(short, Some("Qwen3.6-35B-A3B-UD-MTP-IQ4_NL")) {
            Bars::Declared(t) => t,
            _ => panic!("the primary short arm declares bars for the 35B"),
        };
        assert_eq!(bar_f64(t, "ttft_ms_max"), Some(1000.0));
        assert_eq!(bar_f64(t, "decode_tps_min"), Some(40.0));
    }

    /// The distinction the whole `Bars` enum exists for: an operator's
    /// DECLARED deferral and a missing stem row are two answers, and only
    /// one of them is allowed to pass.
    #[test]
    fn a_deferred_arm_and_an_unknown_stem_are_two_different_answers() {
        let b = parse_bank(&bank_text()).unwrap();
        let short = b.arms.iter().find(|a| a.id == "primary/short").unwrap();
        let fast = b.arms.iter().find(|a| a.id == "fast/short").unwrap();
        assert!(matches!(
            bars_for(short, Some("some-other-model-nobody-measured")),
            Bars::NoStemRow
        ));
        assert!(matches!(bars_for(short, None), Bars::NoStemRow));
        assert!(matches!(
            bars_for(fast, Some("some-other-model-nobody-measured")),
            Bars::Deferred
        ));
    }

    #[test]
    fn an_unknown_stem_is_could_not_judge_and_a_blown_bar_is_a_failure() {
        let b = parse_bank(&bank_text()).unwrap();
        let short = b.arms.iter().find(|a| a.id == "primary/short").unwrap();
        let fine = ProbeResult {
            decode_tps: Some(50.0),
            ttft_ms: Some(430.0),
            prefill_tps: Some(153.0),
            itl_p95_ms: Some(20.0),
            prompt_tokens: Some(60),
            greedy_identical: Some(true),
        };
        let slow = ProbeResult {
            decode_tps: Some(12.0),
            ttft_ms: Some(9_000.0),
            ..fine
        };

        let mut r = LaneReport::new(LANE);
        arm_row(&mut r, short, Some("Qwen3.6-35B-A3B-UD-MTP-IQ4_NL"), &fine);
        arm_row(&mut r, short, Some("Qwen3.6-35B-A3B-UD-MTP-IQ4_NL"), &slow);
        arm_row(&mut r, short, Some("nobody-measured-this"), &fine);
        let verdicts: Vec<_> = r.rows_for_test().iter().map(Judgement::verdict).collect();
        assert_eq!(
            verdicts,
            vec![
                kernel_types::Verdict::Passed,
                kernel_types::Verdict::Failed,
                kernel_types::Verdict::CouldNotJudge,
            ]
        );
    }

    /// A bar whose metric the probe could not compute was NOT met — it was
    /// not judged. The non-streaming fallback path reports no TTFT at all,
    /// and reading that as "within the bar" is the flattering direction.
    #[test]
    fn a_bar_whose_metric_is_missing_is_could_not_judge_not_a_pass() {
        let b = parse_bank(&bank_text()).unwrap();
        let short = b.arms.iter().find(|a| a.id == "primary/short").unwrap();
        let no_ttft = ProbeResult {
            decode_tps: Some(50.0),
            ttft_ms: None,
            prefill_tps: None,
            itl_p95_ms: None,
            prompt_tokens: None,
            greedy_identical: Some(true),
        };
        let mut r = LaneReport::new(LANE);
        arm_row(
            &mut r,
            short,
            Some("Qwen3.6-35B-A3B-UD-MTP-IQ4_NL"),
            &no_ttft,
        );
        assert_eq!(
            r.rows_for_test()[0].verdict(),
            kernel_types::Verdict::CouldNotJudge
        );
    }

    /// The salt is in the FIRST bytes or the arm measures the prefix cache
    /// instead of the prefill — 44,727 ms cold vs 375 ms repeated, measured
    /// on this host.
    #[test]
    fn the_long_prompt_is_salted_at_its_head_and_sized_in_chars() {
        let b = parse_bank(&bank_text()).unwrap();
        let a = long_prompt(&b, 4000, 111);
        let c = long_prompt(&b, 4000, 222);
        assert!(a.starts_with("Run 111.\n"), "{}", &a[..20]);
        assert_ne!(a.as_bytes()[..40], c.as_bytes()[..40]);
        assert_eq!(a.len(), 4000 + b.long_tail.len());
        assert!(a.ends_with(&b.long_tail));
    }

    #[test]
    fn a_long_arm_outside_its_declared_token_band_fails() {
        let b = parse_bank(&bank_text()).unwrap();
        let long = b.arms.iter().find(|a| a.id == "primary/long").unwrap();
        let mk = |n: Option<u64>| ProbeResult {
            decode_tps: Some(40.0),
            ttft_ms: Some(40_000.0),
            prefill_tps: Some(130.0),
            itl_p95_ms: Some(20.0),
            prompt_tokens: n,
            greedy_identical: Some(true),
        };
        let mut r = LaneReport::new(LANE);
        prompt_size_row(&mut r, long, &mk(Some(8_010)));
        prompt_size_row(&mut r, long, &mk(Some(3_000)));
        prompt_size_row(&mut r, long, &mk(None));
        let verdicts: Vec<_> = r.rows_for_test().iter().map(Judgement::verdict).collect();
        assert_eq!(
            verdicts,
            vec![
                kernel_types::Verdict::Passed,
                kernel_types::Verdict::Failed,
                kernel_types::Verdict::CouldNotJudge,
            ]
        );
    }

    #[test]
    fn a_bank_missing_a_required_field_is_refused() {
        let full = bank_text();
        for missing in ["probe =", "seed =", "question =", "id = \"primary/short\""] {
            let cut = full.replace(missing, "cut_out =");
            assert!(
                parse_bank(&cut).is_err(),
                "a bank without `{missing}` must be refused, not half-parsed"
            );
        }
    }

    #[test]
    fn an_unknown_prompt_kind_is_refused_not_defaulted() {
        let bad = bank_text().replace("prompt = \"default\"", "prompt = \"whatever\"");
        let err = parse_bank(&bad).expect_err("an unknown prompt kind is refused");
        assert!(err.contains("whatever"), "{err}");
    }
}
