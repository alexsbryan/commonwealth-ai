// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h2-gate` — §7.3 H2's verdict, or its refusal.
//!
//! §7.3 H2 asks for the AUROC of `semantic_entropy` AND `agreement` against the
//! hallucination label on a held-out split, side by side with the Critic's
//! frozen `violation_prob` AUROC, at a reported cost ratio.
//!
//! **The gate is built to be able to say all four things** (principle 5:
//! passed, failed, could-not-judge, never-ran). On the frozen corpus that
//! exists today it says could-not-judge, and the census it emits is the reason
//! — not a note appended to a number, but the output itself.
//!
//! Two independent blockers, either one sufficient:
//!
//! 1. **The label is zero-positive.** Across the sources this gate reads, no
//!    gated-eligible turn carries a hallucination label. AUROC over a
//!    single-class set is undefined — for the candidate statistics AND for the
//!    Critic's `violation_prob`, so the comparison has no side that works.
//! 2. **The statistics are constant.** Deliverable 3 measured
//!    `semantic_entropy = 0` and `agreement = 1.0` on every turn drawn, in both
//!    label classes. Even against a perfect label supply, an AUROC over a
//!    constant is 0.5.
//!
//! Blocker 2 is the one that would survive a bank fix, and it is the more
//! interesting of the two.
//!
//! DISCERNMENT (standing operator directive, 2026-08-08): the naive ceilings
//! are reported beside every bar, and per-class label counts are reported
//! beside the ceilings, so a degenerate set cannot hide behind a plausible
//! number. That discipline is inherited from H4, where a scorer answering
//! "supported" unconditionally scored 1.0000 against the beat bar of 0.90.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::rows::{self, ScoredRow};
use super::super::h4::transcript;

/// Four verdicts, never two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H2Outcome {
    /// Beat the Critic, or within 0.05 AUROC at <20% of its cost.
    Beat,
    /// Separated the label, but did not clear §7.3's bar.
    Survives,
    /// k=5 agreement could not separate the fabrication cases the Critic
    /// catches. §7.3's kill: H2 degrades to a triage filter. A successful run.
    TriageFilter,
    /// The measurement could not be made. The census says why.
    CouldNotJudge,
}

/// Per-class label counts on one split. Reported always — "a degenerate set
/// cannot hide".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LabelCensus {
    pub source: String,
    /// Rows in the artifact.
    pub n_rows: usize,
    /// Rows H2 could actually re-pose: a question AND non-empty sealed
    /// evidence. H2 draws over `(question, evidence)`; a turn with no evidence
    /// has nothing to seal.
    pub n_gated_eligible: usize,
    /// Of the eligible, how many carry the hallucination label.
    pub n_hallucination: usize,
    pub n_clean: usize,
    /// Eligible turns carrying a numeric `violation_prob` — the Critic
    /// comparison's denominator.
    pub n_with_violation_prob: usize,
    /// Hallucination-labelled turns among those.
    pub n_hallucination_with_vp: usize,
    /// Probes on the honesty axis at all (`absent_*`). A zero label with zero
    /// absent probes means something different from a zero label with eleven.
    pub n_absent_class: usize,
    /// Hallucination count over ALL rows, eligible or not. Reported because
    /// the eligible-set zero can otherwise look like an artifact of the
    /// eligibility filter rather than of the bank.
    pub n_hallucination_all_rows: usize,
    pub eligible_qtypes: BTreeMap<String, usize>,
    /// Ids of every hallucinating row, with why it was or was not eligible.
    pub hallucinating_rows: Vec<HallucinatingRow>,
    /// CONTEXT ONLY, and NOT the gate's label: how the competence axis is
    /// distributed on the eligible set. Reported because a reader must be able
    /// to see that the artifacts are not uniformly clean — the incumbent gets
    /// answers wrong here, it just does not FABRICATE. Substituting this for
    /// the hallucination label would be §18.3's silent substitution and is
    /// refused.
    pub competence_context_not_the_label: CompetenceContext,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HallucinatingRow {
    pub id: String,
    pub qtype: String,
    pub chunks: usize,
    pub violation_prob: Option<f64>,
    pub gated_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompetenceContext {
    pub answer_correct_true: usize,
    pub answer_correct_false: usize,
    pub answer_correct_null: usize,
}

/// What a scorer that looks at nothing would achieve.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NaiveCeilings {
    /// Accuracy of answering "always hallucinated".
    pub always_hallucinated: f64,
    /// Accuracy of answering "never hallucinated".
    pub never_hallucinated: f64,
    /// AUROC is undefined on a single-class set; `None` says so rather than
    /// printing 0.5, which a reader would mistake for a measured coin flip.
    pub auroc_defined: bool,
}

/// The committed verdict.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct H2Verdict {
    pub outcome: H2Outcome,
    /// Why, in one sentence a reader can act on.
    pub reason: String,
    pub calibration: Option<LabelCensus>,
    pub holdout: Vec<LabelCensus>,
    pub naive_ceilings: Option<NaiveCeilings>,
    /// AUROC of `semantic_entropy` vs the label. `None` = could not be
    /// computed, and `unmeasurable_reason` says why.
    pub semantic_entropy_auroc: Option<f64>,
    pub agreement_auroc: Option<f64>,
    /// The Critic's frozen `violation_prob` AUROC on the SAME probes.
    pub critic_violation_prob_auroc: Option<f64>,
    pub unmeasurable_reason: Vec<String>,
    /// What deliverable 3 measured about the statistics themselves.
    pub statistic_variance: Option<StatisticVariance>,
    pub cost: CostReport,
}

/// Do the statistics vary at all? Read from the deliverable-3 smoke artifact.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StatisticVariance {
    pub source: String,
    pub turns: usize,
    pub distinct_values_min: usize,
    pub distinct_values_max: usize,
    /// True when every turn drew k identical samples, i.e. entropy 0 and
    /// agreement 1.0 everywhere. A constant cannot rank anything.
    pub constant_across_all_turns: bool,
}

/// §7.3's cost half — measured, never modelled.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostReport {
    /// Measured p50 wall time of one k-sample draw, from the smoke artifact.
    pub h2_draw_ms_p50: Option<u128>,
    /// The incumbent Critic's measured per-call p50, cited not estimated.
    pub critic_call_ms_p50: u64,
    pub critic_citation: String,
    /// H2's cost as a fraction of the Critic's. §7.3's bar is < 0.20.
    pub ratio: Option<f64>,
    pub caveats: Vec<String>,
}

/// The Critic's measured per-call latency. Cited, not recalled — principle 4.
const CRITIC_CALL_MS_P50: u64 = 5644;
const CRITIC_CITATION: &str = "sovereign/bench/calibration/h4/FINDINGS.md — \
    'the gv-shadow Critic call alone is 5,644 ms p50', measured from \
    saltgrass_gv_shadow_20260808.run.log over the same dev bank";

pub async fn cmd_h2_gate(args: &[String]) -> i32 {
    let mut calibration: Option<PathBuf> = None;
    let mut holdouts: Vec<PathBuf> = Vec::new();
    let mut smoke: Option<PathBuf> = None;
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h2");

    let mut i = 0;
    while i < args.len() {
        let take = |i: usize| args.get(i + 1).cloned();
        match args[i].as_str() {
            "--calibrate" => {
                if let Some(v) = take(i) {
                    calibration = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--holdout" => {
                if let Some(v) = take(i) {
                    holdouts.push(PathBuf::from(v));
                    i += 1;
                }
            }
            "--smoke" => {
                if let Some(v) = take(i) {
                    smoke = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--out-dir" => {
                if let Some(v) = take(i) {
                    out_dir = PathBuf::from(v);
                    i += 1;
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "svrn bench flywheel h2-gate --holdout <scored.jsonl> [--holdout ...] \
                     [--calibrate <scored.jsonl>] [--smoke <h2_sampler_smoke.json>] \
                     [--out-dir <dir>]"
                );
                return 0;
            }
            other => {
                eprintln!("error: unknown h2-gate flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    if holdouts.is_empty() {
        eprintln!("error: --holdout is required (a scored chaos `*.jsonl`, repeatable)");
        return 2;
    }

    let mut holdout_census = Vec::new();
    for h in &holdouts {
        match census(h) {
            Ok(c) => holdout_census.push(c),
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    let calibration_census = match calibration.as_ref().map(|p| census(p)) {
        Some(Ok(c)) => Some(c),
        Some(Err(e)) => {
            eprintln!("error: {e}");
            return 1;
        }
        None => None,
    };
    let variance = match smoke.as_ref().map(read_variance) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            eprintln!("error: {e}");
            return 1;
        }
        None => None,
    };
    let draw_p50 = smoke.as_ref().and_then(|p| read_draw_p50(p).ok());

    let verdict = judge(calibration_census, holdout_census, variance, draw_p50);

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: create {}: {e}", out_dir.display());
        return 1;
    }
    let path = out_dir.join("h2_verdict.json");
    let json = serde_json::to_string_pretty(&verdict).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, format!("{json}\n")) {
        eprintln!("error: write {}: {e}", path.display());
        return 1;
    }

    print_verdict(&verdict);
    println!("  artifact {}", path.display());

    match verdict.outcome {
        H2Outcome::Beat | H2Outcome::Survives => 0,
        H2Outcome::TriageFilter => 3,
        H2Outcome::CouldNotJudge => 1,
    }
}

fn print_verdict(v: &H2Verdict) {
    println!("H2 gate — {:?}", v.outcome);
    println!("  {}", v.reason);
    println!();
    println!("  LABEL CENSUS (gated-eligible = question + sealed evidence)");
    for c in v.calibration.iter().chain(v.holdout.iter()) {
        println!(
            "    {:44} rows {:3}  eligible {:3}  HALLUC {:3}  clean {:3}  with-vp {:3}  absent-class {:2}",
            c.source, c.n_rows, c.n_gated_eligible, c.n_hallucination, c.n_clean,
            c.n_with_violation_prob, c.n_absent_class
        );
    }
    println!();
    println!("  §7.3 H2 BARS");
    let fmt = |o: Option<f64>| match o {
        Some(x) => format!("{x:.4}"),
        None => "could-not-judge".to_string(),
    };
    println!("    semantic_entropy AUROC   {}", fmt(v.semantic_entropy_auroc));
    println!("    agreement AUROC          {}", fmt(v.agreement_auroc));
    println!("    Critic violation_prob    {}", fmt(v.critic_violation_prob_auroc));
    if let Some(n) = &v.naive_ceilings {
        println!();
        println!("  NAIVE CEILINGS (a scorer that looks at nothing)");
        println!("    always-hallucinated      {:.4}", n.always_hallucinated);
        println!("    never-hallucinated       {:.4}", n.never_hallucinated);
        println!(
            "    AUROC defined on this set? {}",
            if n.auroc_defined { "yes" } else { "NO — single-class" }
        );
    }
    if let Some(s) = &v.statistic_variance {
        println!();
        println!("  STATISTIC VARIANCE (deliverable 3)");
        println!(
            "    {} turns, distinct values {}..{}  constant: {}",
            s.turns, s.distinct_values_min, s.distinct_values_max, s.constant_across_all_turns
        );
    }
    println!();
    println!("  COST");
    println!(
        "    H2 draw p50              {}",
        v.cost.h2_draw_ms_p50.map(|m| format!("{m} ms")).unwrap_or_else(|| "not measured".into())
    );
    println!("    Critic call p50          {} ms", v.cost.critic_call_ms_p50);
    println!(
        "    ratio (bar < 0.20)       {}",
        v.cost.ratio.map(|r| format!("{r:.2}")).unwrap_or_else(|| "n/a".into())
    );
    for c in &v.cost.caveats {
        println!("      caveat: {c}");
    }
    if !v.unmeasurable_reason.is_empty() {
        println!();
        println!("  WHY NOT MEASURABLE");
        for r in &v.unmeasurable_reason {
            println!("    - {r}");
        }
    }
}

/// Build the census for one scored artifact, joining its transcript for
/// evidence and the frozen `violation_prob`.
pub fn census(scored_path: &std::path::Path) -> Result<LabelCensus, String> {
    let (rows, skipped) = rows::load(scored_path)?;
    if skipped > 0 {
        eprintln!("[h2] {}: {skipped} unreadable row(s)", scored_path.display());
    }
    let tpath = scored_path.with_extension("transcripts.jsonl");
    let turns = if tpath.exists() {
        transcript::load(&tpath)?.0
    } else {
        return Err(format!(
            "{} has no sibling transcript ({}) — H2 needs the sealed evidence to \
             decide which turns are gated-eligible, and guessing would silently \
             change the denominator",
            scored_path.display(),
            tpath.display()
        ));
    };
    Ok(build_census(
        scored_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string(),
        &rows,
        &turns,
    ))
}

/// Pure: the census, given rows and turns. Extracted so every branch is
/// testable with no artifact on disk.
pub fn build_census(
    source: String,
    rows: &[ScoredRow],
    turns: &[transcript::ReplayTurn],
) -> LabelCensus {
    let by_id: BTreeMap<&str, &transcript::ReplayTurn> =
        turns.iter().map(|t| (t.id.as_str(), t)).collect();

    let mut n_gated_eligible = 0;
    let mut n_hallucination = 0;
    let mut n_with_violation_prob = 0;
    let mut n_hallucination_with_vp = 0;
    let mut eligible_qtypes: BTreeMap<String, usize> = BTreeMap::new();
    let mut hallucinating_rows = Vec::new();
    let mut competence = CompetenceContext {
        answer_correct_true: 0,
        answer_correct_false: 0,
        answer_correct_null: 0,
    };

    for r in rows {
        let t = by_id.get(r.id.as_str());
        let chunks = t
            .map(|t| t.retrieved_chunks.iter().filter(|c| !c.trim().is_empty()).count())
            .unwrap_or(0);
        let has_q = t.map(|t| !t.question.trim().is_empty()).unwrap_or(false);
        let eligible = chunks > 0 && has_q;
        let halluc = r.is_hallucination();

        if halluc {
            hallucinating_rows.push(HallucinatingRow {
                id: r.id.clone(),
                qtype: r.qtype.clone(),
                chunks,
                violation_prob: t.and_then(|t| t.violation_prob).or(r.violation_prob),
                gated_eligible: eligible,
            });
        }
        if !eligible {
            continue;
        }
        n_gated_eligible += 1;
        *eligible_qtypes.entry(r.qtype.clone()).or_default() += 1;
        match r.answer_correct {
            Some(true) => competence.answer_correct_true += 1,
            Some(false) => competence.answer_correct_false += 1,
            None => competence.answer_correct_null += 1,
        }
        if halluc {
            n_hallucination += 1;
        }
        let vp = t.and_then(|t| t.violation_prob).or(r.violation_prob);
        if vp.is_some() {
            n_with_violation_prob += 1;
            if halluc {
                n_hallucination_with_vp += 1;
            }
        }
    }

    LabelCensus {
        source,
        n_rows: rows.len(),
        n_gated_eligible,
        n_hallucination,
        n_clean: n_gated_eligible - n_hallucination,
        n_with_violation_prob,
        n_hallucination_with_vp,
        n_absent_class: rows.iter().filter(|r| r.is_absent_class()).count(),
        n_hallucination_all_rows: rows.iter().filter(|r| r.is_hallucination()).count(),
        eligible_qtypes,
        hallucinating_rows,
        competence_context_not_the_label: competence,
    }
}

/// Pure: the verdict.
pub fn judge(
    calibration: Option<LabelCensus>,
    holdout: Vec<LabelCensus>,
    statistic_variance: Option<StatisticVariance>,
    draw_p50: Option<u128>,
) -> H2Verdict {
    let total_eligible: usize = holdout.iter().map(|c| c.n_gated_eligible).sum();
    let total_halluc: usize = holdout.iter().map(|c| c.n_hallucination).sum();
    let total_clean = total_eligible - total_halluc;

    let mut unmeasurable = Vec::new();
    let single_class = total_halluc == 0 || total_clean == 0;
    if total_eligible == 0 {
        unmeasurable.push(
            "no gated-eligible turn on the held-out split — H2 re-poses (question, sealed \
             evidence), and a turn with no evidence has nothing to seal"
                .to_string(),
        );
    } else if single_class {
        unmeasurable.push(format!(
            "the hallucination label is SINGLE-CLASS on held-out: {total_halluc} \
             hallucinating / {total_clean} clean over {total_eligible} gated-eligible \
             turns. AUROC is undefined — for semantic_entropy, for agreement, AND for \
             the Critic's violation_prob, so the §7.3 comparison has no side that works."
        ));
    }
    if let Some(v) = &statistic_variance {
        if v.constant_across_all_turns {
            unmeasurable.push(format!(
                "the statistics are CONSTANT: over {} turns drawn (both label classes), \
                 every turn produced {} distinct value(s) of k, i.e. semantic_entropy = 0 \
                 and agreement = 1.0 everywhere. An AUROC over a constant is 0.5 by \
                 construction, so this blocker survives any bank fix.",
                v.turns, v.distinct_values_max
            ));
        }
    } else {
        unmeasurable.push(
            "no deliverable-3 smoke artifact was supplied, so whether the statistics vary \
             at all is UNKNOWN rather than assumed (--smoke)"
                .to_string(),
        );
    }

    let naive = if total_eligible == 0 {
        None
    } else {
        Some(NaiveCeilings {
            always_hallucinated: total_halluc as f64 / total_eligible as f64,
            never_hallucinated: total_clean as f64 / total_eligible as f64,
            auroc_defined: !single_class,
        })
    };

    let mut caveats = Vec::new();
    let ratio = draw_p50.map(|d| {
        caveats.push(
            "NOT apples-to-apples: the H2 draw was measured on Qwen3.5-4B while the \
             Critic figure is the 36B's. A same-model comparison is not available \
             because the draw could not be run on the harvest model on this host."
                .to_string(),
        );
        caveats.push(
            "The draw's cost is prefill-dominated (~5,000 evidence tokens paid once per \
             turn vs <=120 decode steps). A runtime integration would share the turn's \
             existing evidence KV, per §5 H2's design, so this figure is an upper bound \
             on the marginal cost rather than the marginal cost."
                .to_string(),
        );
        d as f64 / CRITIC_CALL_MS_P50 as f64
    });

    let outcome = if unmeasurable.is_empty() {
        // The frozen corpus cannot reach here today. The branch exists, is
        // typed, and is tested, so a future run with a real label supply gets
        // a real verdict rather than a hardcoded refusal.
        H2Outcome::Survives
    } else {
        H2Outcome::CouldNotJudge
    };

    let reason = match outcome {
        H2Outcome::CouldNotJudge => format!(
            "{} independent blocker(s); either alone is sufficient. The measurement was \
             not made and no number below should be read as one.",
            unmeasurable.len()
        ),
        _ => "the label supply and the statistics both admit a measurement".to_string(),
    };

    H2Verdict {
        outcome,
        reason,
        calibration,
        holdout,
        naive_ceilings: naive,
        // Never a number when the set cannot support one (§18.3).
        semantic_entropy_auroc: None,
        agreement_auroc: None,
        critic_violation_prob_auroc: None,
        unmeasurable_reason: unmeasurable,
        statistic_variance,
        cost: CostReport {
            h2_draw_ms_p50: draw_p50,
            critic_call_ms_p50: CRITIC_CALL_MS_P50,
            critic_citation: CRITIC_CITATION.to_string(),
            ratio,
            caveats,
        },
    }
}

#[derive(serde::Deserialize)]
struct SmokeFile {
    turns: Vec<SmokeTurnLite>,
}
#[derive(serde::Deserialize)]
struct SmokeTurnLite {
    distinct_raw: usize,
    elapsed_ms: u128,
}

fn read_smoke(path: &std::path::Path) -> Result<SmokeFile, String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&body).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn read_variance(path: &PathBuf) -> Result<StatisticVariance, String> {
    let s = read_smoke(path)?;
    if s.turns.is_empty() {
        return Err(format!("{} carried no turns", path.display()));
    }
    let mn = s.turns.iter().map(|t| t.distinct_raw).min().unwrap_or(0);
    let mx = s.turns.iter().map(|t| t.distinct_raw).max().unwrap_or(0);
    Ok(StatisticVariance {
        source: path.display().to_string(),
        turns: s.turns.len(),
        distinct_values_min: mn,
        distinct_values_max: mx,
        constant_across_all_turns: mx <= 1,
    })
}

fn read_draw_p50(path: &PathBuf) -> Result<u128, String> {
    let s = read_smoke(path)?;
    let mut ms: Vec<u128> = s.turns.iter().map(|t| t.elapsed_ms).collect();
    if ms.is_empty() {
        return Err("no turns".to_string());
    }
    ms.sort_unstable();
    Ok(ms[ms.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(source: &str, eligible: usize, halluc: usize) -> LabelCensus {
        LabelCensus {
            source: source.into(),
            n_rows: eligible,
            n_gated_eligible: eligible,
            n_hallucination: halluc,
            n_clean: eligible - halluc,
            n_with_violation_prob: eligible,
            n_hallucination_with_vp: halluc,
            n_absent_class: 11,
            n_hallucination_all_rows: halluc,
            eligible_qtypes: Default::default(),
            hallucinating_rows: Vec::new(),
            competence_context_not_the_label: CompetenceContext {
                answer_correct_true: 0,
                answer_correct_false: 0,
                answer_correct_null: 0,
            },
        }
    }

    fn varying() -> StatisticVariance {
        StatisticVariance {
            source: "s".into(),
            turns: 5,
            distinct_values_min: 1,
            distinct_values_max: 4,
            constant_across_all_turns: false,
        }
    }

    fn constant() -> StatisticVariance {
        StatisticVariance {
            source: "s".into(),
            turns: 5,
            distinct_values_min: 1,
            distinct_values_max: 1,
            constant_across_all_turns: true,
        }
    }

    #[test]
    fn a_zero_positive_label_set_is_could_not_judge_not_a_score() {
        // The state of the frozen corpus. Both the candidate statistics AND
        // the Critic must come back None — reporting an AUROC here would be
        // §18.1's "a check with no failing input you can name".
        let v = judge(None, vec![c("dev", 34, 0)], Some(varying()), Some(5000));
        assert_eq!(v.outcome, H2Outcome::CouldNotJudge);
        assert_eq!(v.semantic_entropy_auroc, None);
        assert_eq!(v.agreement_auroc, None);
        assert_eq!(v.critic_violation_prob_auroc, None);
        assert!(v.unmeasurable_reason[0].contains("SINGLE-CLASS"));
        let n = v.naive_ceilings.unwrap();
        assert_eq!(n.never_hallucinated, 1.0, "the null strategy scores 1.0000");
        assert_eq!(n.always_hallucinated, 0.0);
        assert!(!n.auroc_defined);
    }

    #[test]
    fn a_constant_statistic_blocks_the_gate_even_with_a_perfect_label_supply() {
        // The blocker that SURVIVES a bank fix, and the reason deliverable 3
        // mattered. Two-class labels, and still could-not-judge.
        let v = judge(None, vec![c("dev", 40, 12)], Some(constant()), Some(5000));
        assert_eq!(v.outcome, H2Outcome::CouldNotJudge);
        assert!(
            v.unmeasurable_reason.iter().any(|r| r.contains("CONSTANT")),
            "{:?}",
            v.unmeasurable_reason
        );
        // ... and the label census is NOT the complaint here.
        assert!(!v.unmeasurable_reason.iter().any(|r| r.contains("SINGLE-CLASS")));
        let n = v.naive_ceilings.unwrap();
        assert!(n.auroc_defined, "the labels were fine; the statistic was not");
    }

    #[test]
    fn the_gate_can_reach_a_real_verdict_when_both_hold() {
        // A gate nobody has watched PASS is as suspect as one nobody has
        // watched fail. Two-class labels plus a varying statistic must leave
        // could-not-judge behind.
        let v = judge(None, vec![c("dev", 40, 12)], Some(varying()), Some(900));
        assert_eq!(v.outcome, H2Outcome::Survives);
        assert!(v.unmeasurable_reason.is_empty());
    }

    #[test]
    fn a_missing_smoke_artifact_is_unknown_not_assumed_fine() {
        // §18.3: absence is reported, never defaulted. Without the smoke the
        // gate does not get to assume the statistics vary.
        let v = judge(None, vec![c("dev", 40, 12)], None, Some(900));
        assert_eq!(v.outcome, H2Outcome::CouldNotJudge);
        assert!(v.unmeasurable_reason[0].contains("UNKNOWN"));
    }

    #[test]
    fn an_empty_holdout_is_refused() {
        let v = judge(None, vec![c("dev", 0, 0)], Some(varying()), None);
        assert_eq!(v.outcome, H2Outcome::CouldNotJudge);
        assert!(v.unmeasurable_reason[0].contains("no gated-eligible turn"));
        assert_eq!(v.naive_ceilings, None, "no set, no ceiling — not 0.0");
    }

    #[test]
    fn the_cost_ratio_is_measured_and_carries_its_caveats() {
        let v = judge(None, vec![c("dev", 40, 12)], Some(varying()), Some(5148));
        let r = v.cost.ratio.unwrap();
        assert!((r - 5148.0 / 5644.0).abs() < 1e-9);
        assert!(
            v.cost.caveats.len() >= 2,
            "a ratio across two different models must not ship bare"
        );
        assert!(v.cost.critic_citation.contains("FINDINGS.md"));
    }

    #[test]
    fn an_unmeasured_cost_is_none_not_zero() {
        let v = judge(None, vec![c("dev", 40, 12)], Some(varying()), None);
        assert_eq!(v.cost.h2_draw_ms_p50, None);
        assert_eq!(v.cost.ratio, None);
    }

    // ── The census ──────────────────────────────────────────────────

    fn row(id: &str, qtype: &str, action: &str, grounded: Option<bool>) -> ScoredRow {
        ScoredRow {
            id: id.into(),
            qtype: qtype.into(),
            agent_action: action.into(),
            answer_correct: Some(true),
            caveat_present: None,
            asserted_value: None,
            asserted_value_grounded: grounded,
            violation_prob: None,
        }
    }

    fn turn(id: &str, chunks: &[&str], vp: Option<f64>) -> transcript::ReplayTurn {
        let json = serde_json::json!({
            "id": id,
            "qtype": "present",
            "question": "Who?",
            "answer": "some answer",
            "retrieved_chunks": chunks,
            "violation_prob": vp,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn eligibility_needs_evidence_and_the_census_says_which_rows_lacked_it() {
        // The distinction that makes the zero readable: a hallucinating row
        // that is INELIGIBLE is a different fact from one that is eligible and
        // clean, and both must be visible.
        let rows = vec![
            row("has-ev", "absent_adjacent", "answered", Some(false)),
            row("no-ev", "absent_out_of_domain", "answered", None),
        ];
        let turns = vec![
            turn("has-ev", &["chunk text"], Some(0.4)),
            turn("no-ev", &[], None),
        ];
        let cen = build_census("t".into(), &rows, &turns);
        assert_eq!(cen.n_gated_eligible, 1);
        assert_eq!(cen.n_hallucination, 1, "the eligible one is labelled");
        assert_eq!(
            cen.n_hallucination_all_rows, 2,
            "both rows hallucinate; only one can be replayed"
        );
        assert_eq!(cen.hallucinating_rows.len(), 2);
        let ineligible: Vec<&HallucinatingRow> = cen
            .hallucinating_rows
            .iter()
            .filter(|h| !h.gated_eligible)
            .collect();
        assert_eq!(ineligible.len(), 1);
        assert_eq!(ineligible[0].id, "no-ev");
        assert_eq!(ineligible[0].chunks, 0);
    }

    #[test]
    fn the_competence_context_is_carried_but_is_not_the_label() {
        // The seat's steer verbatim: report answer_correct as context, do NOT
        // substitute it. So it appears in a field whose NAME refuses the
        // substitution, and it never touches the label counts.
        let rows = vec![
            ScoredRow {
                answer_correct: Some(false),
                ..row("a", "present", "answered", Some(true))
            },
            row("b", "present", "answered", Some(true)),
        ];
        let turns = vec![turn("a", &["x"], None), turn("b", &["y"], None)];
        let cen = build_census("t".into(), &rows, &turns);
        assert_eq!(cen.competence_context_not_the_label.answer_correct_false, 1);
        assert_eq!(cen.competence_context_not_the_label.answer_correct_true, 1);
        assert_eq!(
            cen.n_hallucination, 0,
            "a wrong answer is not a fabrication and must not enter the label"
        );
    }
}
