//! `sovereign bench promote …` — the Fidelity-Flywheel WRITE side: the
//! promotion controller that actually closes the loop.
//!
//! Propose a typed [`ScaffoldingParam`] change, evaluate it as two PAIRED arms
//! on a Dev split of an autonomously-generated probe set — a *baseline arm*
//! (current settings) and a *candidate arm* (settings + the proposed change) —
//! run back-to-back in ONE process so MoE/Metal float-drift is shared (the
//! arms' differential noise is below the per-arm noise the chaos tolerances
//! were calibrated for, making the gate strictly conservative). Then:
//!
//!   * diff candidate-vs-baseline with the reused [`super::lane_baseline`] gate,
//!   * Reject on any red-line regression, NoChange on sub-tolerance noise
//!     (the ≥3-item-collapse discipline that stops the loop thrashing),
//!   * on Accept, optionally confirm on the sacred Test split (burning a
//!     [`PeekBudget`] peek — the held-out pool overrides a Dev win),
//!   * on a confirmed Accept with `--apply`, write the candidate settings to a
//!     checked-in `candidate/rerank.toml` artifact + promote the Dev baseline.
//!
//! The write-back is decoupled from `atoms.json` by construction (see
//! [`ScaffoldingParam`]); reranking is applied in-process via the env vars
//! `build_session` already reads, so a candidate change needs only a new arm —
//! no daemon restart.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_eval::chaos_monkey::{score, AgentAction, CalibrationReport, QuestionType};
use sovereign_eval::entity_resolution_bench::PeekBudget;
use sovereign_eval::flywheel::generators::corpus::{AbsentSource, CorpusGenerator};
use sovereign_eval::flywheel::{DeterministicVerifier, Generator as _, Observation, Probe};
use sovereign_inference::remote::RemoteApiProvider;

use super::baselines::{baseline_dir, write_dated_and_update_latest_at};
use super::gate::chaos_lane_baseline;
use super::lane_baseline::{render_and_exit_code, LaneBaseline};
use super::live_runner::{classify_abstain, classify_caveat, run_live};
use super::scaffolding_param::{decide, AutoApplyPolicy, PromoteDecision, RerankSettings, ScaffoldingParam};
use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench promote",
    summary: "Fidelity-Flywheel write side: propose a retrieval scaffolding change, gate it on a held-out pool, apply it on a pass.",
    sections: &[
        HelpSection::Usage(
            "sovereign bench promote --param <key=value> --corpus <id> [--mine-path <dir>] [--absent-bank <bank.toml>] [--withheld-path <dir>] [--n N] [--seed N] [--bench-root <dir>] [--candidate-config <toml>] [--apply] [--unseal-test --reason \"…\"] [--update-baseline]",
        ),
        HelpSection::Subcommands(&[]),
        HelpSection::Notes(
            "Supported --param: rerank.enabled=<bool>, rerank.candidates_k=<usize> (atoms-decoupled, in-process, auto-applies on a pass behind --apply). Runs PAIRED baseline/candidate arms on the Dev split; --unseal-test confirms on the sacred Test split and burns a peek. The write-back never touches atoms.json (the verifier's oracle) — the ScaffoldingParam type has no enrichment variant.",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_promote(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    run(args).await
}

struct Args {
    param: ScaffoldingParam,
    corpus: String,
    mine_path: Option<PathBuf>,
    absent_bank: Option<PathBuf>,
    withheld_path: Option<PathBuf>,
    n: usize,
    seed: u64,
    bench_root: PathBuf,
    candidate_config: Option<PathBuf>,
    judge_model: String,
    base_url: String,
    apply: bool,
    unseal_test: bool,
    reason: Option<String>,
    update_baseline: bool,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut param: Option<ScaffoldingParam> = None;
    let mut corpus: Option<String> = None;
    let mut mine_path = None;
    let mut absent_bank = None;
    let mut withheld_path = None;
    let mut n = 12usize;
    let mut seed = 0u64;
    let mut bench_root = PathBuf::from("sovereign/bench");
    let mut candidate_config = None;
    let mut judge_model = "fast".to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut apply = false;
    let mut unseal_test = false;
    let mut reason = None;
    let mut update_baseline = false;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i).cloned().ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--param" => param = Some(ScaffoldingParam::parse(&val!("--param"))?),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--mine-path" => mine_path = Some(PathBuf::from(val!("--mine-path"))),
            "--absent-bank" => absent_bank = Some(PathBuf::from(val!("--absent-bank"))),
            "--withheld-path" => withheld_path = Some(PathBuf::from(val!("--withheld-path"))),
            "--n" => n = val!("--n").parse().map_err(|_| "--n must be a usize")?,
            "--seed" => seed = val!("--seed").parse().map_err(|_| "--seed must be a u64")?,
            "--bench-root" => bench_root = PathBuf::from(val!("--bench-root")),
            "--candidate-config" => candidate_config = Some(PathBuf::from(val!("--candidate-config"))),
            "--judge-model" => judge_model = val!("--judge-model"),
            "--base-url" => base_url = val!("--base-url"),
            "--apply" => apply = true,
            "--unseal-test" => unseal_test = true,
            "--reason" => reason = Some(val!("--reason")),
            "--update-baseline" => update_baseline = true,
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    Ok(Args {
        param: param.ok_or("--param is required (e.g. rerank.enabled=true)")?,
        corpus: corpus.ok_or("--corpus is required")?,
        mine_path,
        absent_bank,
        withheld_path,
        n,
        seed,
        bench_root,
        candidate_config,
        judge_model,
        base_url,
        apply,
        unseal_test,
        reason,
        update_baseline,
    })
}

async fn run(args_in: &[String]) -> i32 {
    let (mut globals, rest) = match parse_globals(args_in) {
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

    let candidate_config = args.candidate_config.clone().unwrap_or_else(|| {
        args.bench_root.join("flywheel/candidate").join(format!("{}-rerank.toml", args.corpus))
    });
    let current = match RerankSettings::load(&candidate_config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not load candidate config {candidate_config:?}: {e}");
            return 1;
        }
    };
    let candidate = args.param.apply(current);
    eprintln!(
        "[promote] param={}  current={current:?}  candidate={candidate:?}",
        args.param.id()
    );

    // ── Build the probe set + Dev/Test split ──
    let absent = if let Some(w) = args.withheld_path.clone() {
        AbsentSource::HeldOutSlice { withheld: w }
    } else if let Some(b) = args.absent_bank.clone() {
        AbsentSource::CuratedBank(b)
    } else {
        AbsentSource::None
    };
    let probes = CorpusGenerator { absent }.generate(args.n, args.seed, args.mine_path.as_deref());
    if probes.is_empty() {
        eprintln!(
            "error: no probes (pass --mine-path for Present, --absent-bank/--withheld-path for Absent)"
        );
        return 1;
    }
    let dev = pool_split(&probes, false);
    if dev.is_empty() {
        eprintln!("error: Dev split is empty (need ≥2 probes to split)");
        return 1;
    }
    eprintln!("[promote] probes={} → dev={} test={}", probes.len(), dev.len(), probes.len() - dev.len());

    // ── Live session + judge ──
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let judge: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&v1, None, &args.judge_model, PROVIDER_CTX));
    let model_id = globals.chat_model.clone().unwrap_or_else(|| "primary".to_string());

    // ── Seed-only mode: capture the current-settings Dev baseline + exit ──
    if args.update_baseline {
        let arm = match run_arm(&globals, &current, &args, &judge, &model_id, &dev).await {
            Ok(b) => b,
            Err(code) => return code,
        };
        let dir = baseline_dir(&args.bench_root, "flywheel", &args.corpus);
        match write_dated_and_update_latest_at(&dir, &arm) {
            Ok(p) => {
                eprintln!("[promote] captured Dev baseline ({} metrics) → {}", arm.metrics.len(), p.display());
                return 0;
            }
            Err(e) => {
                eprintln!("error: could not write baseline to {}: {e}", dir.display());
                return 1;
            }
        }
    }

    // ── Paired arms on Dev ──
    eprintln!("[promote] running BASELINE arm (current settings) on Dev …");
    let baseline_arm = match run_arm(&globals, &current, &args, &judge, &model_id, &dev).await {
        Ok(b) => b,
        Err(code) => return code,
    };
    eprintln!("[promote] running CANDIDATE arm (proposed settings) on Dev …");
    let candidate_arm = match run_arm(&globals, &candidate, &args, &judge, &model_id, &dev).await {
        Ok(b) => b,
        Err(code) => return code,
    };

    let (decision, d) = decide(&baseline_arm, &candidate_arm);
    render_and_exit_code(&d, &format!("flywheel:promote:{}", args.param.id()));
    eprintln!("[promote] Dev decision: {decision:?}");

    match decision {
        PromoteDecision::Reject => {
            eprintln!("[promote] REJECTED — a red line regressed past tolerance. Settings unchanged.");
            return 1;
        }
        PromoteDecision::NoChange => {
            eprintln!("[promote] NO CHANGE — movement within tolerance (noise). Settings unchanged.");
            return 0;
        }
        PromoteDecision::Accept => {}
    }

    // ── Sacred Test confirm (only on Accept + --unseal-test) ──
    if args.unseal_test {
        let test = pool_split(&probes, true);
        if test.is_empty() {
            eprintln!("[promote] WARNING: Test split empty — cannot confirm; treating as Dev-only accept.");
        } else {
            let reason = args.reason.clone().unwrap_or_else(|| "(no reason given)".to_string());
            let peek_path = baseline_dir(&args.bench_root, "flywheel", &args.corpus).join("peek_budget.json");
            let mut budget = match PeekBudget::load(&peek_path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("error: could not read peek budget {peek_path:?}: {e}");
                    return 1;
                }
            };
            let n = budget.burn(reason, git_commit_hash());
            if let Err(e) = budget.save(&peek_path) {
                eprintln!("error: could not persist peek budget: {e}");
                return 1;
            }
            eprintln!("[promote] [unseal] burned Test peek #{n} → {peek_path:?}");

            let base_test = match run_arm(&globals, &current, &args, &judge, &model_id, &test).await {
                Ok(b) => b,
                Err(code) => return code,
            };
            let cand_test = match run_arm(&globals, &candidate, &args, &judge, &model_id, &test).await {
                Ok(b) => b,
                Err(code) => return code,
            };
            let (test_decision, td) = decide(&base_test, &cand_test);
            render_and_exit_code(&td, &format!("flywheel:promote:test:{}", args.param.id()));
            if test_decision == PromoteDecision::Reject {
                eprintln!("[promote] REJECTED on the sacred Test split (overrides the Dev win). Settings unchanged.");
                return 1;
            }
            eprintln!("[promote] Test confirm: {test_decision:?} (held)");
        }
    }

    // ── Apply (Accept + --apply + AutoOnPass) ──
    let policy = args.param.auto_apply_policy();
    if args.apply && policy == AutoApplyPolicy::AutoOnPass {
        if let Err(e) = candidate.save(&candidate_config) {
            eprintln!("error: could not write candidate config {candidate_config:?}: {e}");
            return 1;
        }
        let dir = baseline_dir(&args.bench_root, "flywheel", &args.corpus);
        match write_dated_and_update_latest_at(&dir, &candidate_arm) {
            Ok(p) => eprintln!("[promote] ACCEPTED — applied {candidate:?} → {candidate_config:?}; Dev baseline → {}", p.display()),
            Err(e) => {
                eprintln!("error: could not promote baseline to {}: {e}", dir.display());
                return 1;
            }
        }
    } else {
        eprintln!(
            "[promote] ACCEPTED (proposal) — to apply:\n    sovereign bench promote --param {}={} --corpus {} --apply",
            args.param.id(),
            settings_value_for(&args.param, &candidate),
            args.corpus,
        );
        if policy == AutoApplyPolicy::ProposeOnly {
            eprintln!("    (this param class is propose-only; review before applying)");
        }
    }
    0
}

/// Run one arm: set this arm's rerank env, build a fresh in-process session,
/// run every probe through the live path + verifier, score the two red-lines,
/// and return the headline [`LaneBaseline`].
async fn run_arm(
    globals: &crate::chat_cmd::config::ChatGlobals,
    settings: &RerankSettings,
    args: &Args,
    judge: &Arc<dyn InferenceProvider>,
    model_id: &str,
    probes: &[Probe],
) -> Result<LaneBaseline, i32> {
    settings.set_env(&args.corpus);
    let session = match build_session(globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not build chat session: {e}");
            return Err(1);
        }
    };
    let verifier = DeterministicVerifier;
    let mut rows = Vec::with_capacity(probes.len());
    for probe in probes {
        let v = run_and_verify(&session, judge.as_ref(), &args.judge_model, &args.corpus, model_id, &verifier, probe).await;
        rows.push(v.row);
    }
    let report: CalibrationReport = score(&rows);
    Ok(chaos_lane_baseline(
        &report,
        Some(args.corpus.clone()),
        Some(model_id.to_string()),
        chrono::Utc::now().to_rfc3339(),
    ))
}

/// One probe → live answer → observation (judge classification) → verified row.
/// (Same shape as the flywheel read-side runner; kept local to avoid coupling
/// promote's gating loop to the read-side orchestrator's CLI surface.)
async fn run_and_verify(
    session: &ChatSession,
    judge: &dyn InferenceProvider,
    judge_model: &str,
    corpus: &str,
    model_id: &str,
    verifier: &DeterministicVerifier,
    probe: &Probe,
) -> sovereign_eval::flywheel::Verdict {
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
    let caveat_present = if probe.qtype == QuestionType::AbsentOutOfDomain && action == AgentAction::Answered {
        Some(classify_caveat(judge, judge_model, &visible).await.unwrap_or(false))
    } else {
        None
    };
    let obs = Observation { action, answer: &visible, chunks: &chunks, caveat_present };
    verifier.verify(probe, &obs, model_id, corpus)
}

/// Deterministic Dev/Test split: sort by probe id, then assign by sorted-index
/// parity (even → Dev, odd → Test). Stable across runs; the sacred Test split
/// the optimizer never tunes on.
fn pool_split(probes: &[Probe], want_test: bool) -> Vec<Probe> {
    let mut idx: Vec<usize> = (0..probes.len()).collect();
    idx.sort_by(|&a, &b| probes[a].id.cmp(&probes[b].id));
    idx.iter()
        .enumerate()
        .filter(|(rank, _)| (rank % 2 == 1) == want_test)
        .map(|(_, &i)| probes[i].clone())
        .collect()
}

fn settings_value_for(param: &ScaffoldingParam, s: &RerankSettings) -> String {
    match param.id() {
        "rerank.enabled" => s.enabled.to_string(),
        "rerank.candidates_k" => s.candidates_k.to_string(),
        _ => String::new(),
    }
}

fn git_commit_hash() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_eval::chaos_monkey::QuestionType;
    use sovereign_eval::flywheel::{AbsentKind, Oracle, ProbeSource};

    fn probe(id: &str, answerable: bool) -> Probe {
        Probe {
            id: id.into(),
            query: "q".into(),
            qtype: if answerable { QuestionType::Present } else { QuestionType::AbsentAdjacent },
            oracle: if answerable {
                Oracle::Witness { gold_keywords: vec!["x".into()], supporting_quote: None, distractor_quote: None }
            } else {
                Oracle::Absent { held_out_witness: None, kind: AbsentKind::Adjacent }
            },
            source: ProbeSource::I1Corpus,
            note: String::new(),
        }
    }

    #[test]
    fn pool_split_is_deterministic_and_disjoint() {
        let probes: Vec<Probe> = (0..6).map(|i| probe(&format!("p{i}"), i % 2 == 0)).collect();
        let dev = pool_split(&probes, false);
        let test = pool_split(&probes, true);
        assert_eq!(dev.len() + test.len(), probes.len());
        // Disjoint by id.
        for d in &dev {
            assert!(!test.iter().any(|t| t.id == d.id), "dev∩test must be empty");
        }
        // Stable across calls.
        assert_eq!(dev, pool_split(&probes, false));
    }
}
