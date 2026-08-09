// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h2b-gate` — stages 2 and 3: equivalence, then the bar.
//!
//! Stage 2 loads the **reranker** (never the generator) and turns a frozen arms
//! file into scored pairs: `evidence_dependence = 1 − equiv(value_A, value_B)`,
//! where `equiv` is the committed meaning clusterer at its committed floor.
//! Stage 3 loads nothing and applies the kill bar the order fixed **before any
//! run**, which is what makes it a bar rather than an argument.
//!
//! # The bar, verbatim
//!
//! > H2b earns Phase-2 standing iff, on non-leaked held-out pairs, EITHER the
//! > combined score beats margin-alone AUROC by >=0.02, OR `evidence_dependence`
//! > alone reaches >=0.85 AUROC with Pearson correlation to the margin <0.5.
//!
//! Both clauses are computed always; the artifact records both numbers whichever
//! one fires, because "cleared the second clause" and "cleared the second clause
//! and missed the first by 0.001" are different results.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sovereign_core::runtime::native_grounding::meaning_cluster::{self, MergeRung};
use sovereign_core::runtime::native_grounding::sentence_sweep::SentenceScorer;
use sovereign_eval::flywheel::operating_curve::{self as oc, ScoredPair};

use super::super::calibrate::FloorCalibration;
use super::arms::read_arms;
use super::{
    pearson, ArmOutcome, PairArms, PairScore, Split, COMBINE_WEIGHTS, KILL_BAR_COMBINED_DELTA,
    KILL_BAR_ORTHOGONAL_AUROC, KILL_BAR_ORTHOGONAL_PEARSON, STOP_ARM_B_REFUSAL_RATE,
    STOP_LEAK_RATE,
};

/// P2's bar, from the order: mean `evidence_dependence` on answerable must
/// exceed absent by at least this much, or *"the mechanism is dead regardless of
/// AUROC"*.
const P2_SEPARATION_BAR: f64 = 0.15;

/// Four verdicts, never two (principle 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H2bOutcome {
    /// The combined score beat margin-alone by ≥ the bar.
    EarnsPhase2Combined,
    /// `evidence_dependence` alone cleared the AUROC floor while staying
    /// uncorrelated with the margin — an orthogonal signal worth its own seat.
    EarnsPhase2Orthogonal,
    /// Neither clause. H2b dies, the escalation tier stays permanent, and §9's
    /// H2-dependent deletes move to the H4/H1 column. **A successful run.**
    Killed,
    /// The measurement could not be made. The census says why.
    CouldNotJudge,
}

/// P1 — the leak rate, over answerable pairs only (an absent pair has no gold).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct P1Leak {
    pub n_answerable: usize,
    pub n_parametric_known: usize,
    pub leak_rate: f64,
    /// The honest band the order predicted, recorded so the prediction can be
    /// scored rather than remembered.
    pub predicted_band: String,
    pub within_predicted_band: bool,
    /// Arm P's outcome mix — how often the model even attempted a value with no
    /// evidence at all. A leak rate near zero means something different when arm
    /// P refused everywhere than when it answered and was wrong.
    pub arm_p_value: usize,
    pub arm_p_refusal: usize,
    pub arm_p_garbage: usize,
}

/// P2 — separation of mean dependence, on non-leaked held-out pairs.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct P2Separation {
    pub n_answerable: usize,
    pub n_absent: usize,
    pub mean_dependence_answerable: f64,
    pub mean_dependence_absent: f64,
    pub delta: f64,
    pub bar: f64,
    pub clears_bar: bool,
}

/// P3 — the AUROCs, and the two clauses of the kill bar.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct P3Gate {
    pub n_holdout_scored: usize,
    pub dependence_auroc: Option<f64>,
    pub margin_auroc: Option<f64>,
    pub combined_auroc: Option<f64>,
    /// The one fitted parameter, chosen on CALIBRATION and applied unchanged.
    pub combine_weight: Option<f32>,
    pub combine_weight_calibration_auroc: Option<f64>,
    pub combined_minus_margin: Option<f64>,
    pub pearson_dependence_margin: Option<f64>,
    pub clause_a_combined_beats_margin: bool,
    pub clause_b_orthogonal: bool,
}

/// Per-class counts and the naive ceilings — the standing discernment
/// directive. A set whose "always answerable" scorer already scores 0.9 cannot
/// demonstrate discernment, and a reader must see that beside the AUROC.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NaiveCeilings {
    pub set: String,
    pub n: usize,
    pub n_answerable: usize,
    pub n_absent: usize,
    /// Balanced accuracy of "call everything answerable" — 0.5 on any set with
    /// both classes, reported because its *accuracy* is what a careless reader
    /// compares against.
    pub always_answerable_accuracy: f64,
    pub never_answerable_accuracy: f64,
}

/// The census. Everything that could make a number mean less than it looks.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Census {
    pub n_rows: usize,
    pub n_calibration: usize,
    pub n_holdout: usize,
    pub n_answerable: usize,
    pub n_absent: usize,
    /// Pairs whose `evidence_dependence` is `None` because an arm was garbage.
    pub n_uncomparable: usize,
    /// Pairs excluded from the primary gate as `parametric_known`.
    pub n_excluded_leaked: usize,
    /// Pairs with no joined H1 margin — they can score dependence but not the
    /// combined signal.
    pub n_missing_rerank_margin: usize,
    pub arm_a_value: usize,
    pub arm_a_refusal: usize,
    pub arm_a_garbage: usize,
    pub arm_b_value: usize,
    pub arm_b_refusal: usize,
    pub arm_b_garbage: usize,
    /// Arm B's refusal rate over ABSENT pairs — the order's stop condition.
    pub arm_b_refusal_rate_absent: f64,
    /// How many merges each rung of the clusterer decided.
    pub equiv_by_rung: std::collections::BTreeMap<String, usize>,
    /// Determinism: arm A re-decoded on a sample of pairs.
    pub n_repeat_checked: usize,
    pub n_repeat_unstable: usize,
}

/// The verdict artifact.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct H2bVerdict {
    pub criterion: String,
    pub census: Census,
    pub p1: P1Leak,
    pub p2: Option<P2Separation>,
    pub p3: P3Gate,
    pub naive_ceilings: Vec<NaiveCeilings>,
    pub outcome: H2bOutcome,
    pub notes: Vec<String>,
}

pub async fn cmd_h2b_gate(args: &[String]) -> i32 {
    let mut arms_path: Option<PathBuf> = None;
    let mut from_scores: Option<PathBuf> = None;
    let mut h1_scores = PathBuf::from("sovereign/bench/calibration/h1/h1_scores.jsonl");
    let mut floor_curve =
        PathBuf::from("sovereign/bench/calibration/h2/h2_cluster_floor.curve.json");
    let mut floor_override: Option<f32> = None;
    let mut rerank_model: Option<PathBuf> = None;
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h2b");

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match args.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < args.len() {
        match args[i].as_str() {
            "--arms" => arms_path = Some(PathBuf::from(val!("--arms"))),
            "--from-scores" => from_scores = Some(PathBuf::from(val!("--from-scores"))),
            "--h1-scores" => h1_scores = PathBuf::from(val!("--h1-scores")),
            "--floor-curve" => floor_curve = PathBuf::from(val!("--floor-curve")),
            "--floor" => match val!("--floor").parse() {
                Ok(v) => floor_override = Some(v),
                Err(_) => {
                    eprintln!("error: --floor must be an f32");
                    return 2;
                }
            },
            "--rerank-model" => rerank_model = Some(PathBuf::from(val!("--rerank-model"))),
            "--out-dir" => out_dir = PathBuf::from(val!("--out-dir")),
            "--help" | "-h" => {
                print_help();
                return 0;
            }
            other => {
                eprintln!("error: unknown h2b-gate flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: create {out_dir:?}: {e}");
        return 1;
    }

    // ── stage 2 or stage 3 ──────────────────────────────────────────
    let scores: Vec<PairScore> = match (&from_scores, &arms_path) {
        (Some(_), Some(_)) => {
            eprintln!(
                "error: --from-scores and --arms are two different stages. Passing both would \
                 leave which one produced the verdict ambiguous."
            );
            return 2;
        }
        (Some(p), None) => match read_scores(p) {
            Ok(s) => {
                eprintln!("[h2b] replaying {} frozen score(s) from {p:?} — no model loaded", s.len());
                s
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        },
        (None, Some(p)) => {
            let arms = match read_arms(p) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let margins = match read_h1_margins(&h1_scores) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "error: {e}\n  H2b's kill bar is stated against H1's committed margin on \
                         the SAME pairs. Without it there is no bar, and scoring dependence alone \
                         would produce a number with nothing to beat."
                    );
                    return 1;
                }
            };
            let floor = match floor_override {
                Some(f) => {
                    eprintln!("[h2b] clustering floor {f} — OVERRIDDEN on the command line, not the committed one");
                    f
                }
                None => match read_floor(&floor_curve) {
                    Ok(f) => {
                        eprintln!("[h2b] clustering floor {f:.4} from {floor_curve:?} (the committed calibration)");
                        f
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 1;
                    }
                },
            };
            let rpath = match super::super::super::h4::scorer::resolve_rerank_path(rerank_model) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let scorer = match super::super::super::h4::scorer::load(&rpath) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let s = match score_arms(&arms, &margins, &scorer, floor).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let sp = out_dir.join("h2b_scores.jsonl");
            if let Err(e) = write_scores(&sp, &s) {
                eprintln!("error: {e}");
                return 1;
            }
            eprintln!("[out] scores → {sp:?}");
            s
        }
        (None, None) => {
            eprintln!("error: pass --arms <h2b_arms.jsonl> (stage 2) or --from-scores <h2b_scores.jsonl> (stage 3)");
            return 2;
        }
    };

    // The determinism seam only carries what the arms file recorded; the
    // repeat-check counts live there, so they are re-read from the arms file
    // when it is the input and carried in the scores otherwise.
    let repeat = arms_path
        .as_ref()
        .and_then(|p| read_arms(p).ok())
        .map(|a| {
            (
                a.iter().filter(|r| r.repeat_stable.is_some()).count(),
                a.iter().filter(|r| r.repeat_stable == Some(false)).count(),
            )
        })
        .unwrap_or((0, 0));

    let v = verdict(&scores, repeat);
    let vp = out_dir.join("h2b_verdict.json");
    match serde_json::to_string_pretty(&v) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&vp, s + "\n") {
                eprintln!("error: write {vp:?}: {e}");
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serialize verdict: {e}");
            return 1;
        }
    }
    report(&v);
    eprintln!("[out] verdict → {vp:?}");

    match v.outcome {
        H2bOutcome::EarnsPhase2Combined | H2bOutcome::EarnsPhase2Orthogonal => 0,
        H2bOutcome::Killed => 3,
        H2bOutcome::CouldNotJudge => 1,
    }
}

// ───────────────────────────── stage 2 ─────────────────────────────

/// Turn frozen arms into scored pairs.
///
/// `equiv` is [`meaning_cluster::cluster_values`] over the **two** arm values at
/// the committed floor — the same decider, at the same threshold, that H2
/// calibrated (principle 8). Two values land in one cluster or they do not; the
/// dependence is `1 − that`.
pub async fn score_arms(
    arms: &[PairArms],
    margins: &HashMap<String, f32>,
    scorer: &dyn SentenceScorer,
    floor: f32,
) -> Result<Vec<PairScore>, String> {
    let mut out = Vec::with_capacity(arms.len());
    let started = std::time::Instant::now();
    for (n, a) in arms.iter().enumerate() {
        let (dependence, rung) = if a.arm_a.outcome.is_comparable() && a.arm_b.outcome.is_comparable()
        {
            let values = vec![a.arm_a.value.clone(), a.arm_b.value.clone()];
            let r = meaning_cluster::cluster_values(&values, scorer, None, floor)
                .await
                .map_err(|e| format!("cluster {}: {e}", a.id))?;
            let equivalent = r.clusters.len() == 1;
            let rung = r.merges.first().map(|m| rung_name(m.rung).to_string());
            (Some(if equivalent { 0.0 } else { 1.0 }), rung)
        } else {
            // §18.3: a garbage arm leaves this pair WITHOUT a dependence, not
            // with a default one. It still appears in the artifact, and the
            // census counts it.
            (None, None)
        };
        out.push(PairScore {
            id: a.id.clone(),
            corpus_id: a.corpus_id.clone(),
            family: a.family.clone(),
            answerable: a.answerable,
            evidence_dependence: dependence,
            equiv_rung: rung,
            value_margin: a.arm_a.mean_token_margin,
            rerank_margin: margins.get(&a.id).copied(),
            parametric_known: a.parametric_known,
            arm_a_outcome: a.arm_a.outcome,
            arm_b_outcome: a.arm_b.outcome,
            arm_p_outcome: a.arm_p.outcome,
            split: super::split_of(&a.corpus_id),
        });
        if (n + 1) % 200 == 0 || n + 1 == arms.len() {
            eprintln!(
                "[h2b] equiv {}/{}  elapsed {:.0}s",
                n + 1,
                arms.len(),
                started.elapsed().as_secs_f64()
            );
        }
    }
    Ok(out)
}

fn rung_name(r: MergeRung) -> &'static str {
    match r {
        MergeRung::DetKernel => "det_kernel",
        MergeRung::BidirectionalEntailment => "bidirectional_entailment",
        MergeRung::EntailmentCosineTiebreak => "entailment_cosine_tiebreak",
        MergeRung::NoValue => "no_value",
    }
}

// ───────────────────────────── stage 3 ─────────────────────────────

/// The verdict. **Pure**, so every branch — including all four outcomes — is
/// reachable in a test with no model, which is what makes this a gate rather
/// than a print statement (§18.1).
pub fn verdict(scores: &[PairScore], repeat: (usize, usize)) -> H2bVerdict {
    let mut notes: Vec<String> = Vec::new();
    let census = census(scores, repeat);

    // ── P1: the leak rate, over answerable pairs only ───────────────
    let answerable: Vec<&PairScore> = scores.iter().filter(|s| s.answerable).collect();
    let n_leaked = answerable.iter().filter(|s| s.parametric_known).count();
    let leak_rate = if answerable.is_empty() {
        0.0
    } else {
        n_leaked as f64 / answerable.len() as f64
    };
    let p1 = P1Leak {
        n_answerable: answerable.len(),
        n_parametric_known: n_leaked,
        leak_rate,
        predicted_band: "10-40% on a 36B over encyclopedic philosophy; lower on the 4B".into(),
        within_predicted_band: (0.10..=0.40).contains(&leak_rate),
        arm_p_value: scores.iter().filter(|s| s.arm_p_outcome == ArmOutcome::Value).count(),
        arm_p_refusal: scores.iter().filter(|s| s.arm_p_outcome == ArmOutcome::Refusal).count(),
        arm_p_garbage: scores.iter().filter(|s| s.arm_p_outcome == ArmOutcome::Garbage).count(),
    };

    // ── the primary population: held-out, non-leaked, comparable ────
    let holdout: Vec<&PairScore> = scores
        .iter()
        .filter(|s| {
            s.split == Split::Holdout && !s.parametric_known && s.evidence_dependence.is_some()
        })
        .collect();
    let calibration: Vec<&PairScore> = scores
        .iter()
        .filter(|s| {
            s.split == Split::Calibration && !s.parametric_known && s.evidence_dependence.is_some()
        })
        .collect();

    // ── P2: separation of the means ─────────────────────────────────
    let dep = |s: &PairScore| s.evidence_dependence.unwrap_or(0.0) as f64;
    let ho_ans: Vec<&&PairScore> = holdout.iter().filter(|s| s.answerable).collect();
    let ho_abs: Vec<&&PairScore> = holdout.iter().filter(|s| !s.answerable).collect();
    let p2 = if ho_ans.is_empty() || ho_abs.is_empty() {
        None
    } else {
        let ma = ho_ans.iter().map(|s| dep(s)).sum::<f64>() / ho_ans.len() as f64;
        let mb = ho_abs.iter().map(|s| dep(s)).sum::<f64>() / ho_abs.len() as f64;
        Some(P2Separation {
            n_answerable: ho_ans.len(),
            n_absent: ho_abs.len(),
            mean_dependence_answerable: ma,
            mean_dependence_absent: mb,
            delta: ma - mb,
            bar: P2_SEPARATION_BAR,
            clears_bar: (ma - mb) >= P2_SEPARATION_BAR,
        })
    };

    // ── P3: the AUROCs and the two clauses ──────────────────────────
    let dep_curve = curve("evidence_dependence", &holdout, |s| dep(s) as f32);
    let with_margin: Vec<&&PairScore> = holdout.iter().filter(|s| s.rerank_margin.is_some()).collect();
    let cal_with_margin: Vec<&&PairScore> =
        calibration.iter().filter(|s| s.rerank_margin.is_some()).collect();
    let margin_curve = curve2("rerank_margin", &with_margin, |s| s.rerank_margin.unwrap_or(0.0));

    // The one fitted parameter, chosen on CALIBRATION.
    let mut best_w: Option<(f32, f64)> = None;
    for &w in COMBINE_WEIGHTS {
        if let Some(c) = curve2("combined", &cal_with_margin, |s| {
            s.rerank_margin.unwrap_or(0.0) + w * dep(s) as f32
        }) {
            match best_w {
                Some((_, a)) if a >= c.auroc => {}
                _ => best_w = Some((w, c.auroc)),
            }
        }
    }
    let combined_curve = best_w.and_then(|(w, _)| {
        curve2("combined", &with_margin, |s| {
            s.rerank_margin.unwrap_or(0.0) + w * dep(s) as f32
        })
    });

    let xs: Vec<f64> = with_margin.iter().map(|s| dep(s)).collect();
    let ys: Vec<f64> = with_margin
        .iter()
        .map(|s| f64::from(s.rerank_margin.unwrap_or(0.0)))
        .collect();
    let r = pearson(&xs, &ys);

    let dependence_auroc = dep_curve.as_ref().map(|c| c.auroc);
    let margin_auroc = margin_curve.as_ref().map(|c| c.auroc);
    let combined_auroc = combined_curve.as_ref().map(|c| c.auroc);
    let combined_minus_margin = match (combined_auroc, margin_auroc) {
        (Some(c), Some(m)) => Some(c - m),
        _ => None,
    };
    let clause_a = combined_minus_margin.is_some_and(|d| d >= KILL_BAR_COMBINED_DELTA);
    // Both halves of clause B must be MEASURED. An undefined correlation is not
    // a small one: `pearson` returns None when dependence is constant, and a
    // constant dependence is exactly the degenerate case that would otherwise
    // walk through the orthogonality clause.
    let clause_b = dependence_auroc.is_some_and(|a| a >= KILL_BAR_ORTHOGONAL_AUROC)
        && r.is_some_and(|r| r.abs() < KILL_BAR_ORTHOGONAL_PEARSON);

    let p3 = P3Gate {
        n_holdout_scored: holdout.len(),
        dependence_auroc,
        margin_auroc,
        combined_auroc,
        combine_weight: best_w.map(|(w, _)| w),
        combine_weight_calibration_auroc: best_w.map(|(_, a)| a),
        combined_minus_margin,
        pearson_dependence_margin: r,
        clause_a_combined_beats_margin: clause_a,
        clause_b_orthogonal: clause_b,
    };

    // ── the outcome ─────────────────────────────────────────────────
    let outcome = if holdout.is_empty() {
        notes.push(
            "no held-out pair survived the exclusions (leaked, or an arm typed garbage) — \
             there is nothing to apply the bar to"
                .into(),
        );
        H2bOutcome::CouldNotJudge
    } else if leak_rate > STOP_LEAK_RATE {
        notes.push(format!(
            "leak rate {:.1}% exceeds the order's {:.0}% stop condition: the non-leaked \
             remainder is too small to split held-out, and the finding IS the leak rate. \
             The order's named fallback is fictional-corpus pair-mining.",
            leak_rate * 100.0,
            STOP_LEAK_RATE * 100.0
        ));
        H2bOutcome::CouldNotJudge
    } else if census.arm_b_refusal_rate_absent > STOP_ARM_B_REFUSAL_RATE {
        notes.push(format!(
            "arm B refused on {:.1}% of absent pairs, past the order's {:.0}% stop condition: \
             H2b's statistic has collapsed to refusal-detection. That names the model's \
             abstention-trained behaviour under an empty EVIDENCE block; it does not measure \
             evidence dependence.",
            census.arm_b_refusal_rate_absent * 100.0,
            STOP_ARM_B_REFUSAL_RATE * 100.0
        ));
        H2bOutcome::CouldNotJudge
    } else if dependence_auroc.is_none() {
        notes.push(
            "the held-out split is single-class after exclusions — AUROC is undefined, and \
             reporting 0.5 would look like a measured coin flip"
                .into(),
        );
        H2bOutcome::CouldNotJudge
    } else if margin_auroc.is_none() {
        notes.push(
            "no held-out pair carries a joined H1 rerank margin — the bar H2b is measured \
             against is absent, so clause A cannot be evaluated at all"
                .into(),
        );
        H2bOutcome::CouldNotJudge
    } else if clause_a {
        H2bOutcome::EarnsPhase2Combined
    } else if clause_b {
        H2bOutcome::EarnsPhase2Orthogonal
    } else {
        H2bOutcome::Killed
    };

    if census.n_uncomparable > 0 {
        notes.push(format!(
            "{} pair(s) have NO evidence_dependence: an arm decoded past its budget without \
             terminating, so the clusterer was never asked. They are excluded from every \
             AUROC and counted here rather than folded to 0.",
            census.n_uncomparable
        ));
    }
    if census.n_repeat_checked == 0 {
        notes.push(
            "arm A's determinism was NOT re-checked on this run (--repeat-every 0). Greedy \
             decoding should be byte-stable; an instrument whose determinism is assumed is \
             half-validated (§18.4)."
                .into(),
        );
    } else if census.n_repeat_unstable > 0 {
        notes.push(format!(
            "{} of {} re-decoded pairs did NOT reproduce byte-for-byte under a greedy chain. \
             No H2b artifact from this host is replayable until that is understood.",
            census.n_repeat_unstable, census.n_repeat_checked
        ));
    }
    if let Some(p) = &p2 {
        if !p.clears_bar {
            notes.push(format!(
                "P2 FAILED: mean dependence separates by {:+.4}, under the order's {:.2} bar. \
                 The order's words: below that, the mechanism is dead regardless of AUROC.",
                p.delta, p.bar
            ));
        }
    }

    H2bVerdict {
        criterion: format!(
            "H2b earns Phase-2 standing iff, on non-leaked held-out pairs, EITHER combined \
             AUROC - margin AUROC >= {KILL_BAR_COMBINED_DELTA:.2}, OR evidence_dependence \
             AUROC >= {KILL_BAR_ORTHOGONAL_AUROC:.2} with |pearson(dependence, margin)| < \
             {KILL_BAR_ORTHOGONAL_PEARSON:.1}. Fixed before the run."
        ),
        naive_ceilings: vec![
            ceilings("holdout (non-leaked, comparable)", &holdout),
            ceilings("calibration (non-leaked, comparable)", &calibration),
        ],
        census,
        p1,
        p2,
        p3,
        outcome,
        notes,
    }
}

fn census(scores: &[PairScore], repeat: (usize, usize)) -> Census {
    let count = |f: &dyn Fn(&PairScore) -> bool| scores.iter().filter(|s| f(s)).count();
    let absent: Vec<&PairScore> = scores.iter().filter(|s| !s.answerable).collect();
    let arm_b_refusal_rate_absent = if absent.is_empty() {
        0.0
    } else {
        absent
            .iter()
            .filter(|s| s.arm_b_outcome == ArmOutcome::Refusal)
            .count() as f64
            / absent.len() as f64
    };
    let mut equiv_by_rung = std::collections::BTreeMap::new();
    for s in scores {
        if let Some(r) = &s.equiv_rung {
            *equiv_by_rung.entry(r.clone()).or_insert(0usize) += 1;
        }
    }
    Census {
        n_rows: scores.len(),
        n_calibration: count(&|s| s.split == Split::Calibration),
        n_holdout: count(&|s| s.split == Split::Holdout),
        n_answerable: count(&|s| s.answerable),
        n_absent: absent.len(),
        n_uncomparable: count(&|s| s.evidence_dependence.is_none()),
        n_excluded_leaked: count(&|s| s.parametric_known),
        n_missing_rerank_margin: count(&|s| s.rerank_margin.is_none()),
        arm_a_value: count(&|s| s.arm_a_outcome == ArmOutcome::Value),
        arm_a_refusal: count(&|s| s.arm_a_outcome == ArmOutcome::Refusal),
        arm_a_garbage: count(&|s| s.arm_a_outcome == ArmOutcome::Garbage),
        arm_b_value: count(&|s| s.arm_b_outcome == ArmOutcome::Value),
        arm_b_refusal: count(&|s| s.arm_b_outcome == ArmOutcome::Refusal),
        arm_b_garbage: count(&|s| s.arm_b_outcome == ArmOutcome::Garbage),
        arm_b_refusal_rate_absent,
        equiv_by_rung,
        n_repeat_checked: repeat.0,
        n_repeat_unstable: repeat.1,
    }
}

fn ceilings(set: &str, rows: &[&PairScore]) -> NaiveCeilings {
    let n = rows.len();
    let a = rows.iter().filter(|s| s.answerable).count();
    NaiveCeilings {
        set: set.to_string(),
        n,
        n_answerable: a,
        n_absent: n - a,
        always_answerable_accuracy: if n == 0 { 0.0 } else { a as f64 / n as f64 },
        never_answerable_accuracy: if n == 0 { 0.0 } else { (n - a) as f64 / n as f64 },
    }
}

fn curve(
    signal: &str,
    rows: &[&PairScore],
    f: impl Fn(&PairScore) -> f32,
) -> Option<oc::OperatingCurve> {
    let scored: Vec<ScoredPair> = rows
        .iter()
        .map(|s| ScoredPair {
            id: s.id.clone(),
            corpus_id: s.corpus_id.clone(),
            answerable: s.answerable,
            score: f(s),
        })
        .collect();
    oc::build(signal, &scored).ok()
}

fn curve2(
    signal: &str,
    rows: &[&&PairScore],
    f: impl Fn(&PairScore) -> f32,
) -> Option<oc::OperatingCurve> {
    let flat: Vec<&PairScore> = rows.iter().map(|s| **s).collect();
    curve(signal, &flat, f)
}

// ───────────────────────────── plumbing ─────────────────────────────

/// The committed clustering floor, read from H2's calibration artifact.
///
/// Read rather than hard-coded: 4.0757 is a *calibration result*, and a
/// duplicate of it in this file would be principle 8's smell — two places that
/// could disagree about one threshold. `--floor` overrides for experiments and
/// says so on stdout when it does.
pub fn read_floor(path: &Path) -> Result<f32, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "read the committed clustering floor {path:?}: {e}. H2b's equivalence decision IS \
             that floor; there is no default to fall back to (§18.3)."
        )
    })?;
    let c: FloorCalibration = serde_json::from_str(&text)
        .map_err(|e| format!("parse {path:?} as a floor calibration: {e}"))?;
    Ok(c.floor as f32)
}

/// H1's per-pair rerank margin, joined by pair id.
pub fn read_h1_margins(path: &Path) -> Result<HashMap<String, f32>, String> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        rerank_margin: f32,
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut out = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let r: Row = serde_json::from_str(line)
            .map_err(|e| format!("{path:?} line {}: {e}", n + 1))?;
        out.insert(r.id, r.rerank_margin);
    }
    if out.is_empty() {
        return Err(format!("{path:?} holds 0 H1 margins"));
    }
    Ok(out)
}

pub fn read_scores(path: &Path) -> Result<Vec<PairScore>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str::<PairScore>(line)
                .map_err(|e| format!("{path:?} line {}: {e}", n + 1))?,
        );
    }
    if out.is_empty() {
        return Err(format!("{path:?} holds 0 scores — there is nothing to verdict"));
    }
    Ok(out)
}

fn write_scores(path: &Path, scores: &[PairScore]) -> Result<(), String> {
    let mut body = String::new();
    for s in scores {
        body.push_str(&serde_json::to_string(s).map_err(|e| format!("serialize {}: {e}", s.id))?);
        body.push('\n');
    }
    std::fs::write(path, body).map_err(|e| format!("write {path:?}: {e}"))
}

fn report(v: &H2bVerdict) {
    let c = &v.census;
    eprintln!("\n════════ H2b GATE ════════");
    eprintln!("  {}", v.criterion);
    eprintln!(
        "\n── census ── {} rows: {} calibration / {} holdout; {} answerable / {} absent",
        c.n_rows, c.n_calibration, c.n_holdout, c.n_answerable, c.n_absent
    );
    eprintln!(
        "  arm A  value {:>5}  refusal {:>5}  garbage {:>5}",
        c.arm_a_value, c.arm_a_refusal, c.arm_a_garbage
    );
    eprintln!(
        "  arm B  value {:>5}  refusal {:>5}  garbage {:>5}   (refusal rate on ABSENT: {:.3})",
        c.arm_b_value, c.arm_b_refusal, c.arm_b_garbage, c.arm_b_refusal_rate_absent
    );
    eprintln!(
        "  excluded: {} leaked, {} uncomparable; {} without an H1 margin",
        c.n_excluded_leaked, c.n_uncomparable, c.n_missing_rerank_margin
    );
    eprintln!(
        "  determinism: {} re-decoded, {} unstable",
        c.n_repeat_checked, c.n_repeat_unstable
    );
    eprintln!(
        "\n── P1 leak ── {}/{} answerable = {:.3}  (predicted {}, within band: {})",
        v.p1.n_parametric_known,
        v.p1.n_answerable,
        v.p1.leak_rate,
        v.p1.predicted_band,
        v.p1.within_predicted_band
    );
    eprintln!(
        "  arm P: value {} / refusal {} / garbage {}",
        v.p1.arm_p_value, v.p1.arm_p_refusal, v.p1.arm_p_garbage
    );
    match &v.p2 {
        Some(p) => eprintln!(
            "\n── P2 separation ── answerable {:.4} (n={})  absent {:.4} (n={})  delta {:+.4}  bar {:.2}  {}",
            p.mean_dependence_answerable,
            p.n_answerable,
            p.mean_dependence_absent,
            p.n_absent,
            p.delta,
            p.bar,
            if p.clears_bar { "CLEARS" } else { "FAILS" }
        ),
        None => eprintln!("\n── P2 separation ── could not be computed (a class is empty)"),
    }
    let f = |o: Option<f64>| o.map_or("     n/a".to_string(), |x| format!("{x:8.4}"));
    eprintln!("\n── P3 ── over {} held-out non-leaked pairs", v.p3.n_holdout_scored);
    eprintln!("  evidence_dependence AUROC : {}", f(v.p3.dependence_auroc));
    eprintln!("  rerank_margin       AUROC : {}", f(v.p3.margin_auroc));
    eprintln!(
        "  combined            AUROC : {}   (w = {}, fitted on calibration @ {})",
        f(v.p3.combined_auroc),
        v.p3.combine_weight.map_or("n/a".into(), |w| format!("{w}")),
        f(v.p3.combine_weight_calibration_auroc)
    );
    eprintln!("  combined - margin         : {}", f(v.p3.combined_minus_margin));
    eprintln!("  pearson(dep, margin)      : {}", f(v.p3.pearson_dependence_margin));
    eprintln!(
        "  clause A (>= {:.2}): {}    clause B (AUROC >= {:.2} AND |r| < {:.1}): {}",
        KILL_BAR_COMBINED_DELTA,
        v.p3.clause_a_combined_beats_margin,
        KILL_BAR_ORTHOGONAL_AUROC,
        KILL_BAR_ORTHOGONAL_PEARSON,
        v.p3.clause_b_orthogonal
    );
    for n in &v.naive_ceilings {
        eprintln!(
            "\n── naive ceiling ── {}: n={} ({} answerable / {} absent) — always-answerable {:.4}, never {:.4}",
            n.set, n.n, n.n_answerable, n.n_absent, n.always_answerable_accuracy, n.never_answerable_accuracy
        );
    }
    for n in &v.notes {
        eprintln!("\n  note: {n}");
    }
    eprintln!("\n  VERDICT: {:?}", v.outcome);
}

fn print_help() {
    eprintln!(
        "svrn bench flywheel h2b-gate — H2b stages 2 and 3.\n\
         \n\
         Stage 2 (--arms): loads the RERANKER, computes evidence_dependence = 1 - equiv(A, B)\n\
         with the committed clusterer at its committed floor, joins H1's margin by pair id,\n\
         and writes h2b_scores.jsonl.\n\
         Stage 3 (--from-scores): loads NOTHING and applies the kill bar.\n\
         \n\
         Flags:\n\
         \x20 --arms <jsonl>         stage 2 input (h2b-arms' output)\n\
         \x20 --from-scores <jsonl>  stage 3 input — the determinism seam\n\
         \x20 --h1-scores <jsonl>    H1's committed per-pair margins (the bar)\n\
         \x20 --floor-curve <json>   H2's committed clustering floor artifact\n\
         \x20 --floor <f32>          override the floor (says so when it does)\n\
         \x20 --rerank-model <gguf>  default $SOVEREIGN_RERANK_MODEL_PATH (default-inert)\n\
         \x20 --out-dir <dir>        default sovereign/bench/calibration/h2b\n\
         \n\
         Exit: 0 = H2b earns Phase-2 standing, 3 = H2b killed (a successful run either way),\n\
         \x20     1 = could not judge, 2 = usage."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(
        id: &str,
        corpus: &str,
        answerable: bool,
        dep: Option<f32>,
        margin: Option<f32>,
        leaked: bool,
        split: Split,
    ) -> PairScore {
        PairScore {
            id: id.into(),
            corpus_id: corpus.into(),
            family: "sep".into(),
            answerable,
            evidence_dependence: dep,
            equiv_rung: None,
            value_margin: Some(3.0),
            rerank_margin: margin,
            parametric_known: leaked,
            arm_a_outcome: ArmOutcome::Value,
            arm_b_outcome: ArmOutcome::Value,
            arm_p_outcome: ArmOutcome::Value,
            split,
        }
    }

    /// Give half the ABSENT pairs an arm-B refusal.
    ///
    /// **Not cosmetic, and the reason is worth the paragraph.** The first
    /// version of these fixtures had arm B refuse on *every* absent pair, which
    /// is a 100% refusal rate — past the order's 80% stop condition — so five
    /// tests below came back `CouldNotJudge` and could not reach the branch
    /// each was written for. The stop condition was right and the fixtures were
    /// wrong. That is the direction one wants to discover it in, and it is also
    /// the evidence that the stop condition is a live branch rather than a
    /// decoration: it fired unprompted, on the first run, against its author.
    ///
    /// 50% is safely under the bar and is what a model that sometimes guesses
    /// and sometimes abstains would produce.
    fn with_realistic_arm_b(rows: &mut [PairScore]) {
        let mut n = 0usize;
        for r in rows.iter_mut() {
            if !r.answerable {
                if n % 2 == 0 {
                    r.arm_b_outcome = ArmOutcome::Refusal;
                }
                n += 1;
            }
        }
    }

    /// A held-out set where dependence separates PERFECTLY and the margin is
    /// pure noise — so the combined score must beat margin-alone by a mile.
    fn set_where_dependence_wins() -> Vec<PairScore> {
        let mut v = Vec::new();
        for i in 0..40 {
            let ans = i % 2 == 0;
            let dep = if ans { 1.0 } else { 0.0 };
            // A margin that is deliberately anti-correlated with nothing: the
            // same value in both classes, so margin AUROC ≈ 0.5.
            let margin = (i % 5) as f32;
            for split in [Split::Calibration, Split::Holdout] {
                let corpus = if split == Split::Holdout { "H" } else { "C" };
                v.push(s(
                    &format!("{corpus}{i}"),
                    corpus,
                    ans,
                    Some(dep),
                    Some(margin),
                    false,
                    split,
                ));
            }
        }
        with_realistic_arm_b(&mut v);
        v
    }

    #[test]
    fn the_gate_reaches_a_real_verdict_when_dependence_carries_the_signal() {
        // A gate nobody has watched PASS is as suspect as one nobody has
        // watched fail. This is the pass.
        let v = verdict(&set_where_dependence_wins(), (10, 0));
        assert_eq!(v.outcome, H2bOutcome::EarnsPhase2Combined, "{v:#?}");
        assert!(v.p3.clause_a_combined_beats_margin);
        assert!(v.p3.combined_minus_margin.unwrap() >= KILL_BAR_COMBINED_DELTA);
        assert!(v.p2.as_ref().unwrap().clears_bar);
    }

    #[test]
    fn the_gate_is_allowed_to_kill_and_this_is_the_input_that_kills_it() {
        // Dependence is pure noise: it flips on half of each class. The
        // combined score cannot beat the margin (w = 0 is in the grid), the
        // dependence AUROC lands near 0.5, and the verdict must be Killed —
        // not CouldNotJudge, which would let a dead mechanism hide behind an
        // instrument complaint.
        let mut v = Vec::new();
        for i in 0..40 {
            let ans = i % 2 == 0;
            let dep = if i % 4 < 2 { 1.0 } else { 0.0 };
            let margin = if ans { 5.0 } else { -5.0 };
            for split in [Split::Calibration, Split::Holdout] {
                let corpus = if split == Split::Holdout { "H" } else { "C" };
                v.push(s(&format!("{corpus}{i}"), corpus, ans, Some(dep), Some(margin), false, split));
            }
        }
        with_realistic_arm_b(&mut v);
        let r = verdict(&v, (10, 0));
        assert_eq!(r.outcome, H2bOutcome::Killed, "{r:#?}");
        assert!(!r.p3.clause_a_combined_beats_margin);
        assert!(!r.p3.clause_b_orthogonal);
        assert!(!r.p2.as_ref().unwrap().clears_bar);
        assert!(r.notes.iter().any(|n| n.contains("P2 FAILED")));
    }

    #[test]
    fn the_orthogonality_clause_fires_only_when_both_halves_are_measured() {
        // Dependence separates perfectly (AUROC 1.0) AND the margin is
        // uncorrelated with it — but the margin is ALSO perfect, so clause A
        // cannot fire (a perfect margin cannot be beaten). Clause B is the
        // only route to a pass here, which is exactly what it is for.
        let mut v = Vec::new();
        for i in 0..40 {
            let ans = i % 2 == 0;
            let dep = if ans { 1.0 } else { 0.0 };
            // Margin correlates with the LABEL but, within each class, varies
            // independently of dependence — dependence is constant per class,
            // so pearson over the pooled set is high. Force a low correlation
            // by giving the margin the label-perfect ordering with a large
            // spread that swamps the binary term.
            let margin = if ans { 100.0 } else { -100.0 };
            for split in [Split::Calibration, Split::Holdout] {
                let corpus = if split == Split::Holdout { "H" } else { "C" };
                v.push(s(&format!("{corpus}{i}"), corpus, ans, Some(dep), Some(margin), false, split));
            }
        }
        with_realistic_arm_b(&mut v);
        let r = verdict(&v, (10, 0));
        // Both signals are perfect and perfectly correlated: pearson = 1.0, so
        // clause B must NOT fire. Two perfect copies of one signal is not an
        // orthogonal second seat, and this is the assertion that says so.
        assert_eq!(r.p3.dependence_auroc, Some(1.0));
        assert!(r.p3.pearson_dependence_margin.unwrap() > 0.9);
        assert!(!r.p3.clause_b_orthogonal, "correlated duplicates must not pass clause B");
        assert_eq!(r.outcome, H2bOutcome::Killed);
    }

    #[test]
    fn a_leak_rate_past_the_stop_condition_is_could_not_judge_not_a_kill() {
        // The order's stop condition. A 70%-leaked set has too small a
        // remainder to split, and calling that a kill would blame the
        // mechanism for a property of the corpus.
        let mut v = set_where_dependence_wins();
        let n_ans = v.iter().filter(|x| x.answerable).count();
        let mut flagged = 0;
        for x in v.iter_mut() {
            if x.answerable && flagged < (n_ans * 7) / 10 {
                x.parametric_known = true;
                flagged += 1;
            }
        }
        let r = verdict(&v, (10, 0));
        assert_eq!(r.outcome, H2bOutcome::CouldNotJudge);
        assert!(r.p1.leak_rate > STOP_LEAK_RATE);
        assert!(r.notes.iter().any(|n| n.contains("stop condition")));
    }

    #[test]
    fn arm_b_refusing_on_almost_every_absent_pair_is_reported_as_the_collapse_it_is() {
        let mut v = set_where_dependence_wins();
        for x in v.iter_mut() {
            if !x.answerable {
                x.arm_b_outcome = ArmOutcome::Refusal;
            }
        }
        // 100% > the 80% stop condition.
        let r = verdict(&v, (10, 0));
        assert_eq!(r.outcome, H2bOutcome::CouldNotJudge);
        assert!(r.notes.iter().any(|n| n.contains("refusal-detection")));
    }

    #[test]
    fn a_garbage_arm_leaves_the_pair_without_a_dependence_rather_than_with_a_zero() {
        let mut v = set_where_dependence_wins();
        // Break the first ten holdout ANSWERABLE pairs: they were the ones
        // carrying dependence = 1.0. Folding them to 0.0 would degrade the
        // separation silently; excluding them must instead be counted.
        let mut broken = 0;
        for x in v.iter_mut() {
            if x.split == Split::Holdout && x.answerable && broken < 10 {
                x.evidence_dependence = None;
                x.arm_b_outcome = ArmOutcome::Garbage;
                broken += 1;
            }
        }
        let r = verdict(&v, (10, 0));
        assert_eq!(r.census.n_uncomparable, 10);
        assert_eq!(r.p3.n_holdout_scored, 30, "the ten must be OUT of the AUROC");
        assert!(r.notes.iter().any(|n| n.contains("NO evidence_dependence")));
    }

    #[test]
    fn an_empty_holdout_is_could_not_judge() {
        let v: Vec<PairScore> = set_where_dependence_wins()
            .into_iter()
            .filter(|x| x.split == Split::Calibration)
            .collect();
        let r = verdict(&v, (10, 0));
        assert_eq!(r.outcome, H2bOutcome::CouldNotJudge);
        assert!(r.notes.iter().any(|n| n.contains("nothing to apply the bar to")));
    }

    #[test]
    fn a_holdout_without_h1_margins_cannot_evaluate_the_bar() {
        // §18.3: the bar's absence is REPORTED. Scoring dependence alone and
        // calling it a pass would compare it against nothing.
        let mut v = set_where_dependence_wins();
        for x in v.iter_mut() {
            x.rerank_margin = None;
        }
        let r = verdict(&v, (10, 0));
        assert_eq!(r.outcome, H2bOutcome::CouldNotJudge);
        assert!(r.notes.iter().any(|n| n.contains("bar H2b is measured against is absent")));
    }

    #[test]
    fn an_unchecked_or_unstable_determinism_pin_is_said_out_loud() {
        let v = set_where_dependence_wins();
        let never = verdict(&v, (0, 0));
        assert!(never.notes.iter().any(|n| n.contains("half-validated")));
        let broken = verdict(&v, (10, 3));
        assert!(broken.notes.iter().any(|n| n.contains("did NOT reproduce")));
    }

    // ── stage 2: the equivalence fold ──────────────────────────────
    //
    // These exist because the pure-verdict tests above cannot reach
    // `score_arms`. Sabotaging the garbage rule THERE (returning `Some(0.0)`
    // instead of `None`) left every test above green — a §18.1 check with no
    // failing input anyone could name. These are that input.

    /// A scorer that refuses every merge. Rung (b) can then never fire, so each
    /// case below is decided by the rung it is written to test rather than by
    /// an accident of the margin.
    struct NeverMerges;
    #[async_trait::async_trait]
    impl SentenceScorer for NeverMerges {
        async fn score(&self, _q: &str, docs: &[String]) -> Result<Vec<f32>, String> {
            Ok(vec![-100.0; docs.len()])
        }
    }

    fn arm(raw: &str, finished_eog: bool) -> super::super::ArmRecord {
        let value = sovereign_inference::k_sample::clean_value(raw);
        super::super::ArmRecord {
            outcome: ArmOutcome::classify(&value, finished_eog),
            raw: raw.into(),
            value,
            finished_eog,
            prompt_tokens: 10,
            decoded_tokens: 3,
            mean_token_margin: Some(2.5),
            elapsed_ms: 1,
        }
    }

    fn arms_row(id: &str, a: &str, a_eog: bool, b: &str, b_eog: bool) -> PairArms {
        PairArms {
            id: id.into(),
            corpus_id: "sep-x".into(),
            family: "sep".into(),
            answerable: true,
            question: "q?".into(),
            n_chunks: 3,
            evidence_index: Some(0),
            arm_a: arm(a, a_eog),
            arm_b: arm(b, b_eog),
            arm_p: arm("NONE", true),
            repeat_stable: Some(true),
            parametric_known: false,
        }
    }

    #[tokio::test]
    async fn the_equivalence_fold_reads_each_arm_pairing_the_way_the_order_specifies() {
        let rows = vec![
            // Same value: rung (a), the deterministic kernel. Dependence 0.
            arms_row("same", "Karl Yundt", true, "Karl Yundt", true),
            // Different values, and the scorer refuses to merge them.
            arms_row("flip", "Karl Yundt", true, "Ossipon", true),
            // A value against a decline: different meanings, no instrument
            // asked, dependence 1.
            arms_row("declined", "Karl Yundt", true, "NONE", true),
            // Two declines: ONE cluster. k unanimous "the evidence does not
            // say" is agreement, so dependence 0.
            arms_row("both_declined", "NONE", true, "NONE", true),
        ];
        let margins: HashMap<String, f32> =
            [("same".to_string(), 1.5f32)].into_iter().collect();
        let out = score_arms(&rows, &margins, &NeverMerges, 4.0757).await.unwrap();
        let dep = |id: &str| {
            out.iter().find(|s| s.id == id).unwrap().evidence_dependence
        };
        assert_eq!(dep("same"), Some(0.0));
        assert_eq!(dep("flip"), Some(1.0));
        assert_eq!(dep("declined"), Some(1.0));
        assert_eq!(dep("both_declined"), Some(0.0));
        // The H1 margin is JOINED, and its absence is `None` rather than 0.0 —
        // a zero margin is a real and quite different score.
        assert_eq!(out[0].rerank_margin, Some(1.5));
        assert_eq!(out[1].rerank_margin, None);
    }

    #[tokio::test]
    async fn a_garbage_arm_is_never_handed_to_the_clusterer_and_gets_no_dependence() {
        // THE failing input the pure tests could not supply. Fold this to
        // `Some(0.0)` and a truncated stream reads as "the value did not
        // change under ablation" — the most confident possible reading of a
        // stream that never finished a sentence.
        let rows = vec![
            arms_row("a_truncated", "The evidence indicates that the", false, "Ossipon", true),
            arms_row("b_truncated", "Karl Yundt", true, "The passage states that", false),
        ];
        let out = score_arms(&rows, &HashMap::new(), &NeverMerges, 4.0757).await.unwrap();
        for s in &out {
            assert_eq!(
                s.evidence_dependence, None,
                "{} must have NO dependence, not a defaulted one",
                s.id
            );
            assert_eq!(s.equiv_rung, None, "the clusterer must not have been asked");
        }
        assert_eq!(out[0].arm_a_outcome, ArmOutcome::Garbage);
        assert_eq!(out[1].arm_b_outcome, ArmOutcome::Garbage);
        // And it reaches the verdict as an exclusion rather than as a score.
        assert_eq!(verdict(&out, (2, 0)).census.n_uncomparable, 2);
    }

    #[test]
    fn the_naive_ceilings_are_always_reported() {
        let v = verdict(&set_where_dependence_wins(), (10, 0));
        assert_eq!(v.naive_ceilings.len(), 2);
        let ho = &v.naive_ceilings[0];
        assert_eq!(ho.n, ho.n_answerable + ho.n_absent);
        assert!((ho.always_answerable_accuracy - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_combination_weight_is_fitted_on_calibration_and_never_on_holdout() {
        // The leakage guard. Make the two splits disagree about which weight is
        // best: calibration says dependence helps, holdout says it hurts. The
        // reported combined AUROC must be the one the CALIBRATION weight
        // produces on holdout — a lower number — not the best holdout number.
        let mut v = Vec::new();
        for i in 0..40 {
            let ans = i % 2 == 0;
            // calibration: dependence perfectly separates
            v.push(s(&format!("C{i}"), "C", ans, Some(if ans { 1.0 } else { 0.0 }), Some(0.0), false, Split::Calibration));
            // holdout: dependence is INVERTED
            v.push(s(&format!("H{i}"), "H", ans, Some(if ans { 0.0 } else { 1.0 }), Some(0.0), false, Split::Holdout));
        }
        with_realistic_arm_b(&mut v);
        let r = verdict(&v, (10, 0));
        let w = r.p3.combine_weight.expect("a weight must be chosen");
        assert!(w > 0.0, "calibration says dependence helps, so w > 0");
        assert!(
            r.p3.combined_auroc.unwrap() < 0.5,
            "the calibration weight applied to an inverted holdout must SCORE BADLY \
             ({:?}) — a number at or above 0.5 would mean the weight was re-fitted here",
            r.p3.combined_auroc
        );
        assert_eq!(r.outcome, H2bOutcome::Killed);
    }
}
