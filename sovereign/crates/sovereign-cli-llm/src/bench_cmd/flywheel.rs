// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel run …` — the Fidelity-Flywheel loop's READ side.
//!
//! Drives an autonomously-generated probe set (I1 corpus self-supervision)
//! through the SAME live chat path the chaos bench uses (`run_live`, sealed to
//! one corpus), classifies each answer with the shared forced-choice judges,
//! verifies it against the probe's witness with the pure
//! `DeterministicVerifier`, scores the two red-lines (reusing the chaos scorer
//! of record), and captures every failure as a durable regression case.
//!
//! This is generator-agnostic by construction: it asks a
//! [`sovereign_eval::flywheel::Generator`] for probes and treats them
//! uniformly, so I2–I5 reuse this orchestrator unchanged — they only change
//! which generator is selected.
//!
//! The WRITE side (proposing + gating a scaffolding change) is
//! `bench_cmd::promote`; this command measures, captures, and reports.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_eval::chaos_monkey::{score, AgentAction, CalibrationReport, Gates, QuestionType};
use sovereign_eval::flywheel::generators::corpus::{AbsentSource, CorpusGenerator};
use sovereign_eval::flywheel::{
    by_id, generator_ids, validate_fairness, DeterministicVerifier, Observation, Probe,
    RegressionBank, RegressionCase, Verdict,
};
use sovereign_inference::remote::RemoteApiProvider;

use crate::bench_cmd::live_runner::{classify_abstain, classify_caveat, run_live};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench flywheel",
    summary: "Fidelity-Flywheel read side: generate probes from a corpus, run them through the live chat path, verify groundedness/abstention, capture failures as regression cases.",
    sections: &[
        HelpSection::Usage(
            "svrn bench flywheel run --corpus <id> [--mine-path <dir>] [--absent-bank <bank.toml>] [--withheld-path <dir>] [--n N] [--seed N] [--judge-model <stem>] [--out <jsonl>] [--regressions <jsonl>] [--no-capture]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Generate I1 probes (Present mined from --mine-path's atlas/atoms.json; Absent from --absent-bank or --withheld-path), run each through the live path sealed to --corpus, verify + score the two red-lines, capture failures.",
            ),
            (
                "calibration-set",
                "OFFLINE: mine (question, chunks, answerable?) pairs from one or more corpora's atlases and run the contamination pass against the dev/test banks. No daemon, no model, no RNG — the same corpora and --pool yield byte-identical output. This is NATIVE_GROUNDING §7.1's calibration role: the only data H1/H2 thresholds may be fitted on. Refuses to mine a dev/test bank corpus, and exits non-zero when the contamination pass finds a shared 13-word span.",
            ),
        ]),
        HelpSection::Notes(
            "Present probes need an ENRICHED corpus root (--mine-path with atlas/atoms.json); a corpus with no enrichment yields no Present probes. Absent probes come from a curated bank (--absent-bank) or a withheld, enriched-but-unindexed slice (--withheld-path). The verifier is pure and reuses the chaos two-red-line scorer; failures are captured to sovereign/bench/flywheel/regressions/<corpus>.jsonl (fairness-validated, deduped).",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_flywheel(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    match args[0].as_str() {
        "run" => run(&args[1..]).await,
        "calibration-set" => calibration_set(&args[1..]),
        "redteam" => super::redteam::cmd_redteam(&args[1..]).await,
        other => {
            eprintln!("error: unknown flywheel subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

/// `calibration-set` — mine the §7.1 calibration role and prove it clean.
///
/// Offline by construction: it reads corpus atlases off disk and the bank TOMLs
/// out of the repo. Nothing here builds a provider or touches the daemon, which
/// is why it is `fn` — the "no live model" property is structural.
///
/// Exit codes: `0` clean, `1` contaminated or I/O failure, `2` usage.
fn calibration_set(rest: &[String]) -> i32 {
    use sovereign_eval::flywheel::calibration as cal;

    let mut corpora: Vec<String> = Vec::new();
    // The one accessor for this path (`~/.svrnmesh|.sovereign/indexes`), not a
    // re-derivation and not a new env knob — ARCH §10.6, one accessor per path.
    let mut index_root = sovereign_cli_shared::dirs::sovereign_indexes();
    let mut banks: Vec<PathBuf> = Vec::new();
    let mut out = PathBuf::from("sovereign/bench/calibration/native_grounding_calibration.jsonl");
    let mut limit = 5_000usize;
    let mut pool = 8usize;

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
            "--corpus" => corpora.push(val!("--corpus")),
            "--index-root" => index_root = PathBuf::from(val!("--index-root")),
            "--bank" => banks.push(PathBuf::from(val!("--bank"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--limit" => match val!("--limit").parse() {
                Ok(v) => limit = v,
                Err(_) => {
                    eprintln!("error: --limit must be a usize");
                    return 2;
                }
            },
            "--pool" => match val!("--pool").parse() {
                Ok(v) => pool = v,
                Err(_) => {
                    eprintln!("error: --pool must be a usize");
                    return 2;
                }
            },
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    if corpora.is_empty() {
        eprintln!("error: at least one --corpus <id> is required (repeatable)");
        return 2;
    }
    if banks.is_empty() {
        eprintln!(
            "error: at least one --bank <bank.toml> is required — a contamination pass with \
             nothing to check against would call every set clean"
        );
        return 2;
    }

    let mut all = Vec::new();
    let mut reports = Vec::new();
    for id in &corpora {
        let root = index_root.join(id);
        match cal::mine_calibration_pairs(id, &root, limit, pool) {
            Ok((pairs, rep)) => {
                eprintln!(
                    "[calibration] {id}: claims={} answerable={} absent={} dropped_leaky={} witness_absent={}",
                    rep.claims_mined,
                    rep.pairs_answerable,
                    rep.pairs_absent,
                    rep.absent_dropped_leaky,
                    rep.answerable_witness_absent
                );
                all.extend(pairs);
                reports.push(rep);
            }
            Err(e) => {
                eprintln!("[calibration] {id}: {e}");
                return 1;
            }
        }
    }
    if all.is_empty() {
        eprintln!("error: mined 0 pairs across {} corpus(es)", corpora.len());
        return 1;
    }

    let contamination = match cal::contamination_pass(&all, &banks) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: contamination pass: {e}");
            return 1;
        }
    };
    for (bank, n) in &contamination.banks_indexed {
        eprintln!(
            "[contamination] indexed {n} {}-gram(s) from {bank}",
            contamination.shingle_n
        );
    }

    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = String::new();
    for p in &all {
        match serde_json::to_string(p) {
            Ok(s) => {
                body.push_str(&s);
                body.push('\n');
            }
            Err(e) => {
                eprintln!("error: could not serialize pair {}: {e}", p.id);
                return 1;
            }
        }
    }
    if let Err(e) = std::fs::write(&out, body) {
        eprintln!("error: could not write {out:?}: {e}");
        return 1;
    }
    let report_path = out.with_extension("contamination.json");
    let doc = serde_json::json!({
        "corpora": reports,
        "pool_size": pool,
        "limit": limit,
        "contamination": contamination,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&report_path, s + "\n") {
                eprintln!("error: could not write {report_path:?}: {e}");
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: could not serialize report: {e}");
            return 1;
        }
    }
    eprintln!(
        "[out] {} pair(s) → {out:?}\n[out] contamination report → {report_path:?}",
        all.len()
    );
    if contamination.clean {
        eprintln!(
            "[contamination] CLEAN — no calibration pair shares a 13-word span with any bank"
        );
        0
    } else {
        eprintln!(
            "[contamination] CONTAMINATED — {} pair(s) share a verbatim span with a dev/test bank; \
             thresholds fitted on this set would be unfalsifiable",
            contamination.collisions.len()
        );
        1
    }
}

struct Args {
    generator: String,
    corpus: String,
    mine_path: Option<PathBuf>,
    absent_bank: Option<PathBuf>,
    withheld_path: Option<PathBuf>,
    n: usize,
    seed: u64,
    judge_model: String,
    base_url: String,
    out: PathBuf,
    regressions: Option<PathBuf>,
    capture: bool,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut generator = "i1_corpus".to_string();
    let mut corpus: Option<String> = None;
    let mut mine_path: Option<PathBuf> = None;
    let mut absent_bank: Option<PathBuf> = None;
    let mut withheld_path: Option<PathBuf> = None;
    let mut n = 12usize;
    let mut seed = 0u64;
    let mut judge_model = "fast".to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut out = PathBuf::from("target/flywheel/results.jsonl");
    let mut regressions: Option<PathBuf> = None;
    let mut capture = true;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--generator" => generator = val!("--generator"),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--mine-path" => mine_path = Some(PathBuf::from(val!("--mine-path"))),
            "--absent-bank" => absent_bank = Some(PathBuf::from(val!("--absent-bank"))),
            "--withheld-path" => withheld_path = Some(PathBuf::from(val!("--withheld-path"))),
            "--n" => n = val!("--n").parse().map_err(|_| "--n must be a usize")?,
            "--seed" => seed = val!("--seed").parse().map_err(|_| "--seed must be a u64")?,
            "--judge-model" => judge_model = val!("--judge-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--out" => out = PathBuf::from(val!("--out")),
            "--regressions" => regressions = Some(PathBuf::from(val!("--regressions"))),
            "--no-capture" => capture = false,
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    Ok(Args {
        generator,
        corpus: corpus.ok_or("--corpus is required (the corpus id to seal retrieval to)")?,
        mine_path,
        absent_bank,
        withheld_path,
        n,
        seed,
        judge_model,
        base_url,
        out,
        regressions,
        capture,
    })
}

async fn run(rest: &[String]) -> i32 {
    let (mut globals, rest) = match parse_globals(rest) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if globals.temperature.is_none() {
        globals.temperature = Some(0.0);
    }
    let args = match parse_args(&rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    // ── Build the probe set ──
    // The generic registry produces a default-configured generator; I1's
    // absent source is set from the flags (the registry default is None).
    let probes = if args.generator == "i1_corpus" {
        let absent = if let Some(w) = args.withheld_path.clone() {
            AbsentSource::HeldOutSlice { withheld: w }
        } else if let Some(b) = args.absent_bank.clone() {
            AbsentSource::CuratedBank(b)
        } else {
            AbsentSource::None
        };
        let generator = CorpusGenerator { absent };
        use sovereign_eval::flywheel::Generator as _;
        generator.generate(args.n, args.seed, args.mine_path.as_deref())
    } else {
        match by_id(&args.generator) {
            Some(g) => g.generate(args.n, args.seed, args.mine_path.as_deref()),
            None => {
                eprintln!(
                    "error: unknown --generator `{}`. Registered: {}",
                    args.generator,
                    generator_ids().join(", ")
                );
                return 2;
            }
        }
    };

    if probes.is_empty() {
        eprintln!(
            "error: generator `{}` produced no probes.\n  \
             For I1 Present probes, pass --mine-path <enriched-corpus-root> (needs atlas/atoms.json).\n  \
             For Absent probes, pass --absent-bank <bank.toml> or --withheld-path <dir>.",
            args.generator
        );
        return 1;
    }
    // Defense-in-depth: the fairness contract is enforced at generation, but
    // re-check here so an unfair probe can never reach the model (or capture).
    if let Some(bad) = probes.iter().find_map(|p| validate_fairness(p).err()) {
        eprintln!("error: generator emitted an unfair probe: {bad}");
        return 1;
    }
    let n_answerable = probes.iter().filter(|p| p.qtype.is_answerable()).count();
    let n_absent = probes.len() - n_answerable;
    eprintln!(
        "[flywheel] generator={} corpus={} probes={} (answerable={}, absent={})",
        args.generator,
        args.corpus,
        probes.len(),
        n_answerable,
        n_absent,
    );

    // ── Live session + judge ──
    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not build chat session: {e}");
            return 1;
        }
    };
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let judge: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &args.judge_model,
        PROVIDER_CTX,
    ));
    let model_id = globals
        .chat_model
        .clone()
        .unwrap_or_else(|| "primary".to_string());

    // ── Run + verify each probe ──
    let verifier = DeterministicVerifier;
    let mut verdicts: Vec<Verdict> = Vec::with_capacity(probes.len());
    for (pi, probe) in probes.iter().enumerate() {
        let verdict = run_and_verify(
            &session,
            judge.as_ref(),
            &args.judge_model,
            &args.corpus,
            &model_id,
            &verifier,
            probe,
        )
        .await;
        eprintln!(
            "  [{:>2}/{}] {:<20} act={:<9} {}",
            pi + 1,
            probes.len(),
            probe.qtype.label(),
            format!("{:?}", verdict.row.agent_action),
            match &verdict.failure {
                None => "PASS".to_string(),
                Some(f) => format!("FAIL {f:?}"),
            },
        );
        verdicts.push(verdict);
    }

    // ── Score + glassbox ──
    let rows: Vec<_> = verdicts.iter().map(|v| v.row.clone()).collect();
    if let Err(e) = write_jsonl(&args.out, &verdicts) {
        eprintln!("error: could not write {:?}: {e}", args.out);
        return 1;
    }
    let report = score(&rows);
    let gates = Gates::default();
    let verdict = report.verdict(&gates);
    print_summary(&report, &verdicts);

    // ── Capture failures as regression cases ──
    if args.capture {
        let path = args.regressions.clone().unwrap_or_else(|| {
            PathBuf::from(format!(
                "sovereign/bench/flywheel/regressions/{}.jsonl",
                args.corpus
            ))
        });
        let captured_at = chrono::Utc::now().to_rfc3339();
        let source_run = format!("flywheel:{}:seed{}", args.corpus, args.seed);
        let mut newly = 0usize;
        for (probe, v) in probes.iter().zip(&verdicts) {
            let Some(failure) = v.failure else { continue };
            let case = RegressionCase {
                id: format!("{}-{}", source_run, probe.id),
                probe: probe.clone(),
                failure,
                determinism: v.determinism,
                captured_answer_excerpt: v.row.answer_excerpt.clone(),
                captured_chunks: Vec::new(),
                corpus: args.corpus.clone(),
                model_id: model_id.clone(),
                captured_at: captured_at.clone(),
                source_run: source_run.clone(),
            };
            match RegressionBank::capture(&path, &case) {
                Ok(true) => newly += 1,
                Ok(false) => {}
                Err(e) => eprintln!("  [capture] skipped {}: {e}", probe.id),
            }
        }
        eprintln!("[capture] {newly} new regression case(s) → {path:?}");
    }

    eprintln!("[out] wrote {} verdicts → {:?}", verdicts.len(), args.out);
    if verdict.overall_pass {
        0
    } else {
        1
    }
}

/// One probe → live answer → observation (judge classification) → verdict.
async fn run_and_verify(
    session: &crate::chat_cmd::bootstrap::ChatSession,
    judge: &dyn InferenceProvider,
    judge_model: &str,
    corpus: &str,
    model_id: &str,
    verifier: &DeterministicVerifier,
    probe: &Probe,
) -> Verdict {
    let live = run_live(session, corpus, &probe.query).await;
    let visible = live.visible;
    let chunks = live.retrieved_chunk_texts;

    let action = match classify_abstain(judge, judge_model, &visible).await {
        Some(true) => AgentAction::Abstained,
        Some(false) => AgentAction::Answered,
        None => {
            if visible.trim().len() < 24 {
                AgentAction::Abstained
            } else {
                AgentAction::Answered
            }
        }
    };

    // Provenance caveat — only for out-of-domain answers (mirrors chaos).
    let caveat_present =
        if probe.qtype == QuestionType::AbsentOutOfDomain && action == AgentAction::Answered {
            Some(
                classify_caveat(judge, judge_model, &visible)
                    .await
                    .unwrap_or(false),
            )
        } else {
            None
        };

    let obs = Observation {
        action,
        answer: &visible,
        chunks: &chunks,
        caveat_present,
    };
    verifier.verify(probe, &obs, model_id, corpus)
}

fn write_jsonl(path: &std::path::Path, verdicts: &[Verdict]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    use std::io::Write as _;
    for v in verdicts {
        let line = serde_json::to_string(v)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// The calibration report itself — the numbers the run exists to produce,
/// so every line here is stdout payload.
fn print_summary(report: &CalibrationReport, verdicts: &[Verdict]) {
    let c = &report.counts;
    println!("\n── fidelity flywheel: grounded calibration (I1) ──");
    println!(
        "  competence-when-present : {:.2}   [correct {}/{}, timid {}]",
        report.competence, c.answerable_correct, c.answerable, c.answerable_abstained,
    );
    println!(
        "  honesty-when-absent     : {:.2}   [honest {}/{}, HALLUCINATED {}]",
        report.honesty, c.absent_honest, c.absent, c.absent_hallucinated,
    );
    // Failure-class tally (the taxonomy glassbox).
    let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for v in verdicts {
        if let Some(f) = v.failure {
            *tally.entry(format!("{f:?}")).or_default() += 1;
        }
    }
    if tally.is_empty() {
        println!("  failures: none");
    } else {
        let parts: Vec<String> = tally.iter().map(|(k, n)| format!("{k}={n}")).collect();
        println!("  failures by class: {}", parts.join(" "));
    }
}
