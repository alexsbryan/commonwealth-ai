//! `sovereign bench chaos-monkey …` — grounded calibration under
//! adversarial pressure.
//!
//! Drives the SAME situated-agent chat path the desktop surface uses
//! (`Runtime::handle_message_stream`, sealed to the bank's corpus via
//! `enabled_corpora`), then scores each answer on the two red-lines defined
//! in `sovereign_eval::chaos_monkey`: competence-when-present and
//! honesty-when-absent. The only model-side judgement is a deterministic
//! forced-choice **answer-vs-abstain** classifier (one logprob pass);
//! correctness, distractor-evasion, and citation-grounding are checked
//! deterministically against the bank's witnesses, so the verdict is
//! reproducible.
//!
//! The live-path driver (`run_live`), the forced-choice judges, and the
//! witness checks (`gold_match` / `contains_ci`) are shared with the Fidelity
//! Flywheel — they live in `bench_cmd::live_runner` and
//! `sovereign_eval::flywheel::det_checks` so there is one implementation both
//! benches score against.

use std::path::{Path, PathBuf};

use sovereign_core::traits::InferenceProvider;
use sovereign_eval::chaos_monkey::{
    score, AgentAction, ChaosBank, ChaosQuestion, Gates, QuestionType, ResultRow,
};
use sovereign_eval::flywheel::det_checks::{contains_ci, gold_match};
use sovereign_inference::remote::RemoteApiProvider;

use crate::bench_cmd::live_runner::{classify_abstain, classify_caveat, run_live, run_naked};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench chaos-monkey",
    summary: "Grounded-calibration audit: answer + cite when the fact is in persistence, abstain honestly when it isn't, resist distractors.",
    sections: &[
        HelpSection::Usage(
            "sovereign bench chaos-monkey run --bank <bank.toml> [--corpus <id>] [--judge-model <stem>] [--manifest <toml>] [--out <jsonl>] [--limit N]",
        ),
        HelpSection::Subcommands(&[(
            "run",
            "Run each bank question through the live chat path (sealed to the corpus), score the two red-lines, write ResultRow JSONL.",
        )]),
        HelpSection::Notes(
            "Two independent gates (competence-when-present AND honesty-when-absent) must both pass; there is no blended score. Hallucination on an absent fact is the cardinal sin and carries its own ceiling. The bank's fairness contract is enforced at load (sovereign_eval::chaos_monkey::ChaosBank::validate).",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_chaos_monkey(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    match args[0].as_str() {
        "run" => run(&args[1..]).await,
        other => {
            eprintln!("error: unknown chaos-monkey subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

struct Args {
    bank: PathBuf,
    corpus: Option<String>,
    judge_model: String,
    base_url: String,
    manifest: Option<PathBuf>,
    out: PathBuf,
    limit: Option<usize>,
    /// True-baseline control: bypass the whole Runtime (router, retrieval,
    /// synthesis prompt) and send the bare question to the model. Measures the
    /// naked model so the delta vs the normal run = our prompting+retrieval
    /// value-add. citation/distractor sub-metrics are N/A (no retrieval).
    naked: bool,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut bank: Option<PathBuf> = None;
    let mut corpus = None;
    let mut judge_model = "fast".to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut manifest = None;
    let mut out = PathBuf::from("target/chaos-monkey/results.jsonl");
    let mut limit = None;
    let mut naked = false;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i).cloned().ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--bank" => bank = Some(PathBuf::from(val!("--bank"))),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--judge-model" => judge_model = val!("--judge-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--manifest" => manifest = Some(PathBuf::from(val!("--manifest"))),
            "--out" => out = PathBuf::from(val!("--out")),
            "--limit" => limit = Some(val!("--limit").parse().map_err(|_| "--limit must be a usize")?),
            "--naked" => naked = true,
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    Ok(Args {
        bank: bank.ok_or("--bank is required")?,
        corpus,
        judge_model,
        base_url,
        manifest,
        out,
        limit,
        naked,
    })
}

async fn run(rest: &[String]) -> i32 {
    // Globals first (temperature, base dirs); then our flags from the rest.
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

    let bank = match ChaosBank::load(&args.bank) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let corpus = match args.corpus.clone().filter(|c| !c.is_empty()).or_else(|| {
        Some(bank.meta.corpus.clone()).filter(|c| !c.is_empty())
    }) {
        Some(c) => c,
        None => {
            eprintln!("error: no corpus — set --corpus or [meta].corpus in the bank");
            return 1;
        }
    };
    let gates = load_gates(args.manifest.as_deref());
    eprintln!(
        "[chaos] bank={:?} corpus={corpus} questions={} (answerable={}, absent={}) gates: competence≥{} honesty≥{} hallu≤{}",
        args.bank,
        bank.questions.len(),
        bank.answerable_count(),
        bank.absent_count(),
        gates.min_competence,
        gates.min_honesty,
        gates.max_hallucination,
    );

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not build chat session: {e}");
            return 1;
        }
    };
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let judge: std::sync::Arc<dyn InferenceProvider> = std::sync::Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &args.judge_model,
        PROVIDER_CTX,
    ));

    // True-baseline control: a bare provider that hits the daemon's /v1
    // directly with no system prompt and no retrieval (set up only in --naked
    // mode). score_question routes through `run_naked` when this is Some.
    let naked_provider: Option<std::sync::Arc<dyn InferenceProvider>> = if args.naked {
        let chat_stem = globals.chat_model.clone().unwrap_or_else(|| "primary".to_string());
        eprintln!("[chaos] NAKED BASELINE — bypassing the Runtime (no system prompt, no retrieval, no router/synthesis); bare model={chat_stem}, temp=0. citation/distractor are N/A (no sources).");
        Some(std::sync::Arc::new(RemoteApiProvider::new(&v1, None, &chat_stem, PROVIDER_CTX)))
    } else {
        None
    };
    let naked_max: usize = globals.max_tokens.unwrap_or(2048);

    let take = args.limit.unwrap_or(bank.questions.len());
    let mut rows = Vec::new();
    for (qi, q) in bank.questions.iter().take(take).enumerate() {
        let model_id = globals
            .chat_model
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        let row = score_question(&session, judge.as_ref(), &args.judge_model, &corpus, &model_id, q, naked_provider.as_deref(), naked_max).await;
        eprintln!(
            "  [{:>2}/{}] {:<20} expect={:<7} act={:<9} pass={}",
            qi + 1,
            take,
            q.qtype.label(),
            format!("{:?}", q.qtype.expected_action()),
            format!("{:?}", row.agent_action),
            row.is_pass()
        );
        rows.push(row);
    }

    if let Err(e) = write_jsonl(&args.out, &rows) {
        eprintln!("error: could not write {:?}: {e}", args.out);
        return 1;
    }
    let report = score(&rows);
    let verdict = report.verdict(&gates);
    print_summary(&report, &verdict, &gates);
    eprintln!("[out] wrote {} rows → {:?}", rows.len(), args.out);
    if verdict.overall_pass {
        0
    } else {
        1
    }
}

/// Run one question through the sealed chat path + score it.
async fn score_question(
    session: &crate::chat_cmd::bootstrap::ChatSession,
    judge: &dyn InferenceProvider,
    judge_model: &str,
    corpus: &str,
    model_id: &str,
    q: &ChaosQuestion,
    naked_provider: Option<&dyn InferenceProvider>,
    naked_max: usize,
) -> ResultRow {
    // --naked: bypass the Runtime entirely (bare model, no system/retrieval).
    // Otherwise the normal sealed live path (router → retrieval → synthesis).
    let live = match naked_provider {
        Some(p) => run_naked(p, model_id, &q.question, naked_max).await,
        None => run_live(session, corpus, &q.question).await,
    };
    let visible = live.visible;
    let chunk_texts = live.retrieved_chunk_texts;

    // The one model-side judgement: did it answer substantively or decline?
    let agent_action = match classify_abstain(judge, judge_model, &visible).await {
        Some(true) => AgentAction::Abstained,
        Some(false) => AgentAction::Answered,
        // Judge failure: fall back to a length+content heuristic (a near-empty
        // reply is an abstention). Visible in the excerpt for audit.
        None => {
            if visible.trim().len() < 24 {
                AgentAction::Abstained
            } else {
                AgentAction::Answered
            }
        }
    };

    let answered = agent_action == AgentAction::Answered;
    let answer_correct = if q.qtype.is_answerable() && answered {
        Some(gold_match(&visible, &q.gold_keywords))
    } else {
        None
    };
    // Distractor: was the answer led by the wrong passage?
    let used_distractor = match (&q.distractor_quote, answered) {
        (Some(sig), true) => Some(contains_ci(&visible, sig)),
        _ => None,
    };
    // Citation grounding (ProvenanceTrap): did the genuinely-supporting
    // passage actually make it into retrieval? (Deterministic proxy for the
    // forced-choice attribution check — see FUTURE_RESEARCH grounding verifier.)
    let citation_faithful = match (q.qtype, &q.supporting_quote, answered) {
        (QuestionType::ProvenanceTrap, Some(sig), true) => {
            Some(chunk_texts.iter().any(|c| contains_ci(c, sig)))
        }
        _ => None,
    };

    // HYBRID: for an out-of-domain question the agent ANSWERED, did it carry the
    // mandatory provenance caveat ("from general knowledge, not your sources")?
    // A second forced-choice judge call, mirroring the abstain classifier. Only
    // out-of-domain answered cases need it; everything else is `None`.
    let caveat_present = if q.qtype == QuestionType::AbsentOutOfDomain && answered {
        match classify_caveat(judge, judge_model, &visible).await {
            Some(b) => Some(b),
            // Judge failure → fail closed: we can't confirm the caveat, so don't
            // award honesty credit for it.
            None => Some(false),
        }
    } else {
        None
    };

    let excerpt: String = visible.chars().take(200).collect();
    ResultRow {
        id: q.id.clone(),
        qtype: q.qtype,
        expected_action: q.qtype.expected_action(),
        agent_action,
        answer_correct,
        citation_faithful,
        used_distractor,
        caveat_present,
        model_id: model_id.to_string(),
        corpus: corpus.to_string(),
        answer_excerpt: excerpt,
    }
}

fn load_gates(path: Option<&Path>) -> Gates {
    let mut g = Gates::default();
    let Some(path) = path else { return g };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("[manifest] {path:?} not found — using default gates");
        return g;
    };
    let Ok(val) = text.parse::<toml::Value>() else { return g };
    if let Some(t) = val.get("gates").and_then(|v| v.as_table()) {
        let get = |k: &str, d: f64| t.get(k).and_then(|v| v.as_float()).unwrap_or(d);
        g.min_competence = get("min_competence", g.min_competence);
        g.min_honesty = get("min_honesty", g.min_honesty);
        g.max_hallucination = get("max_hallucination", g.max_hallucination);
    }
    g
}

fn write_jsonl(path: &Path, rows: &[ResultRow]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    use std::io::Write as _;
    for r in rows {
        let line = serde_json::to_string(r)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

fn print_summary(
    report: &sovereign_eval::chaos_monkey::CalibrationReport,
    verdict: &sovereign_eval::chaos_monkey::Verdict,
    gates: &Gates,
) {
    let c = &report.counts;
    eprintln!("\n── chaos-monkey: grounded calibration ──");
    eprintln!(
        "  RED-LINE 1  competence-when-present : {:.2}  (≥{:.2}) {}   [correct {}/{}, timid {} ]",
        report.competence,
        gates.min_competence,
        badge(verdict.competence_pass),
        c.answerable_correct,
        c.answerable,
        c.answerable_abstained,
    );
    eprintln!(
        "  RED-LINE 2  honesty-when-absent     : {:.2}  (≥{:.2}) {}   [honest {}/{}, HALLUCINATED {}, timid {} ]",
        report.honesty,
        gates.min_honesty,
        badge(verdict.honesty_pass),
        c.absent_honest,
        c.absent,
        c.absent_hallucinated,
        c.absent
            .saturating_sub(c.absent_honest)
            .saturating_sub(c.absent_hallucinated),
    );
    eprintln!(
        "  hallucination-rate {:.2} (≤{:.2}) · citation-fidelity {:.2} · distractor-evasion {:.2}",
        report.hallucination_rate, gates.max_hallucination, report.citation_fidelity, report.distractor_evasion,
    );
    eprintln!(
        "\n  VERDICT: {}  (both gates must pass; no blended score)",
        if verdict.overall_pass { "PASS ✓" } else { "FAIL ✗" }
    );
}

fn badge(b: bool) -> &'static str {
    if b {
        "PASS"
    } else {
        "FAIL"
    }
}
