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

use crate::bench_cmd::live_runner::{caveat_credit, classify_abstain, classify_caveat, run_live};
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
        HelpSection::Subcommands(&[(
            "run",
            "Generate I1 probes (Present mined from --mine-path's atlas/atoms.json; Absent from --absent-bank or --withheld-path), run each through the live path sealed to --corpus, verify + score the two red-lines, capture failures.",
        )]),
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
        "redteam" => super::redteam::cmd_redteam(&args[1..]).await,
        other => {
            eprintln!("error: unknown flywheel subcommand `{other}`");
            help::print(&HELP);
            2
        }
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
    let mut base_url = sovereign_core::setup_config::client_daemon_base();
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
    verdict.overall.exit_code()
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
            Some(caveat_credit(
                classify_caveat(judge, judge_model, &visible).await,
            ))
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
