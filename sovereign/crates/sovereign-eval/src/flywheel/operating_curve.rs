// SPDX-License-Identifier: AGPL-3.0-or-later
//! The operating curve: what a score has to ship with before a threshold
//! cut from it is allowed into the runtime.
//!
//! `NATIVE_GROUNDING.md §3` principle 2 — "every score ships with its
//! curve. No raw threshold enters the runtime without a calibration
//! artifact (held-out AUROC, recall at bounded false-alarm budgets)
//! checked into the branch."  This module is that artifact's producer, and
//! it is deliberately pure: given the same `(score, label)` pairs it emits
//! byte-identical output, so the instrument can be validated (ARCH §18.4)
//! before any number computed with it is believed.
//!
//! **Orientation, stated once so nothing downstream has to guess.** Every
//! score here is *higher = more answerable* (both H1 candidates are:
//! the reranker's yes/no margin, and `top_cosine`). The DECISION the curve
//! describes runs the other way — you abstain when the score falls BELOW a
//! threshold — so the two rates are:
//!
//!   * **honesty-recall** — of the genuinely absent pairs, the fraction the
//!     threshold correctly abstains on. This is the thing H1 exists to buy.
//!   * **false-alarm** — of the genuinely answerable pairs, the fraction the
//!     threshold wrongly abstains on. This is what it costs.
//!
//! Reporting recall at *bounded* false-alarm budgets ({5%, 10%, 20%}, the
//! verifier-v0 convention) is the honest form: a router that abstains on
//! everything has perfect honesty-recall and is useless, and a single
//! headline number would hide that.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The false-alarm budgets §7.3 names, in percent. One definition, so the
/// harness, the report and the docs cannot drift (ARCH §10.6).
pub const FALSE_ALARM_BUDGETS_PCT: &[u32] = &[5, 10, 20];

/// One pair after scoring: what it scored, and what it actually was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredPair {
    pub id: String,
    pub corpus_id: String,
    /// The ground-truth label from the calibration set.
    pub answerable: bool,
    /// Higher = more answerable.
    pub score: f32,
}

/// One threshold and everything it decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatingPoint {
    /// Abstain when `score < threshold`.
    pub threshold: f64,
    /// Absent pairs correctly abstained on.
    pub honesty_recall: f64,
    /// Answerable pairs wrongly abstained on.
    pub false_alarm: f64,
    /// `(honesty_recall + (1 - false_alarm)) / 2`.
    pub balanced_accuracy: f64,
}

/// The committed artifact for one signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatingCurve {
    /// What was scored (`"rerank_margin"` / `"top_cosine"`).
    pub signal: String,
    pub n_pairs: usize,
    pub n_answerable: usize,
    pub n_absent: usize,
    /// Area under the ROC for answerable-vs-absent. Tie-corrected
    /// (Mann-Whitney U with mid-ranks), so a signal that returns one
    /// constant for everything scores 0.5 rather than 1.0.
    pub auroc: f64,
    /// Best honesty-recall achievable within each false-alarm budget,
    /// keyed by budget in percent.
    pub honesty_recall_at_false_alarm: BTreeMap<u32, RecallAtBudget>,
    /// Best balanced accuracy over all thresholds, and where.
    pub best_balanced_accuracy: f64,
    pub best_balanced_accuracy_threshold: f64,
    /// Every distinct operating point, low threshold first.
    pub points: Vec<OperatingPoint>,
}

/// What a budget actually bought, and at what threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallAtBudget {
    pub honesty_recall: f64,
    /// The realized false-alarm rate at the chosen threshold — always
    /// `<= budget`, and reported because it is usually strictly less
    /// (thresholds are discrete).
    pub false_alarm: f64,
    pub threshold: f64,
}

/// Build the curve for one signal.
///
/// # Errors
/// Refuses a set that cannot produce a curve: no pairs, or only one class
/// present. Both would otherwise yield a number (`0.5`, or `NaN`) that
/// reads like a measurement — ARCH §18.1, a check with no failing input it
/// can name is not a check.
pub fn build(signal: &str, scored: &[ScoredPair]) -> Result<OperatingCurve, String> {
    if scored.is_empty() {
        return Err(format!(
            "`{signal}`: 0 scored pairs — a curve over nothing is not a curve"
        ));
    }
    let n_answerable = scored.iter().filter(|s| s.answerable).count();
    let n_absent = scored.len() - n_answerable;
    if n_answerable == 0 || n_absent == 0 {
        return Err(format!(
            "`{signal}`: {n_answerable} answerable / {n_absent} absent — AUROC is undefined with \
             only one class present, and reporting 0.5 would look like a measurement"
        ));
    }

    let auroc = auroc_mid_rank(scored);

    // Candidate thresholds: every distinct score, plus one above the max so
    // the "abstain on everything" endpoint exists. Sorted ascending and
    // deduped on the bit pattern, so the curve is reproducible.
    let mut thresholds: Vec<f64> = scored.iter().map(|s| f64::from(s.score)).collect();
    thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    thresholds.dedup_by(|a, b| (*a).to_bits() == (*b).to_bits());
    let above_all = thresholds.last().copied().unwrap_or(0.0) + 1.0;
    thresholds.push(above_all);

    let mut points = Vec::with_capacity(thresholds.len());
    for &t in &thresholds {
        let mut abstained_absent = 0usize;
        let mut abstained_answerable = 0usize;
        for s in scored {
            if f64::from(s.score) < t {
                if s.answerable {
                    abstained_answerable += 1;
                } else {
                    abstained_absent += 1;
                }
            }
        }
        let honesty_recall = abstained_absent as f64 / n_absent as f64;
        let false_alarm = abstained_answerable as f64 / n_answerable as f64;
        points.push(OperatingPoint {
            threshold: t,
            honesty_recall,
            false_alarm,
            balanced_accuracy: (honesty_recall + (1.0 - false_alarm)) / 2.0,
        });
    }

    let mut honesty_recall_at_false_alarm = BTreeMap::new();
    for &budget in FALSE_ALARM_BUDGETS_PCT {
        let cap = f64::from(budget) / 100.0;
        // Best honesty-recall among points whose false-alarm fits the
        // budget. Ties on recall break toward the LOWER false-alarm, then
        // the lower threshold, so the pick is deterministic.
        let best = points
            .iter()
            .filter(|p| p.false_alarm <= cap + f64::EPSILON)
            .min_by(|a, b| {
                b.honesty_recall
                    .partial_cmp(&a.honesty_recall)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(
                        a.false_alarm
                            .partial_cmp(&b.false_alarm)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                    .then(
                        a.threshold
                            .partial_cmp(&b.threshold)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
            });
        // There is always at least one: the lowest threshold abstains on
        // nothing, so false_alarm = 0.
        if let Some(p) = best {
            honesty_recall_at_false_alarm.insert(
                budget,
                RecallAtBudget {
                    honesty_recall: p.honesty_recall,
                    false_alarm: p.false_alarm,
                    threshold: p.threshold,
                },
            );
        }
    }

    let best_point = points
        .iter()
        .min_by(|a, b| {
            b.balanced_accuracy
                .partial_cmp(&a.balanced_accuracy)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.threshold
                        .partial_cmp(&b.threshold)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        })
        .expect("thresholds is non-empty");

    Ok(OperatingCurve {
        signal: signal.to_string(),
        n_pairs: scored.len(),
        n_answerable,
        n_absent,
        auroc,
        honesty_recall_at_false_alarm,
        best_balanced_accuracy: best_point.balanced_accuracy,
        best_balanced_accuracy_threshold: best_point.threshold,
        points,
    })
}

// ─────────────────────────────── the H1 gate ───────────────────────────────

/// H1's kill margin, verbatim from `NATIVE_GROUNDING.md §7.3`:
///
/// > *Kill:* if the reranker margin AUROC < top_cosine + 0.05 on calibration
/// > data, H1 dies before any runtime integration.
pub const H1_KILL_DELTA: f64 = 0.05;

/// H1's *beat* bar, also §7.3: "Beat: `top_cosine` on the same pairs (the
/// incumbent signal), by >= 0.10 AUROC."
pub const H1_BEAT_DELTA: f64 = 0.10;

/// What the gate decided. Three outcomes, not two — "cleared the kill bar
/// but not the beat bar" is a real and different state, and collapsing it
/// into pass/fail is what ARCH §18.2 warns against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H1Outcome {
    /// margin AUROC >= top_cosine + 0.10 — the funded win.
    Beat,
    /// margin AUROC >= top_cosine + 0.05 but < + 0.10 — H1 lives, but did
    /// not clear the bar §7.3 set for calling it a win.
    Survives,
    /// margin AUROC < top_cosine + 0.05 — H1 dies here, and the fallback
    /// (train the 4B head via the verifier-v0 pipeline) is the
    /// recommendation.
    Killed,
}

/// The committed verdict artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct H1Verdict {
    pub outcome: H1Outcome,
    pub rerank_margin_auroc: f64,
    pub top_cosine_auroc: f64,
    /// `rerank_margin_auroc - top_cosine_auroc`.
    pub delta: f64,
    pub kill_threshold_delta: f64,
    pub beat_threshold_delta: f64,
    pub n_pairs: usize,
    /// Stated in the artifact so a reader never has to reconstruct which
    /// way the comparison ran.
    pub criterion: String,
}

/// Apply §7.3's H1 criterion to two curves over the SAME pairs.
///
/// # Errors
/// Refuses two curves built over different pair sets — the difference of
/// AUROCs from different sets is not a comparison, though it would look
/// exactly like one in the artifact.
pub fn h1_verdict(
    rerank_margin: &OperatingCurve,
    top_cosine: &OperatingCurve,
) -> Result<H1Verdict, String> {
    if rerank_margin.n_pairs != top_cosine.n_pairs
        || rerank_margin.n_answerable != top_cosine.n_answerable
    {
        return Err(format!(
            "the two curves are not over the same pairs (rerank {}/{} vs cosine {}/{}) — their \
             AUROC difference would not be a comparison",
            rerank_margin.n_pairs,
            rerank_margin.n_answerable,
            top_cosine.n_pairs,
            top_cosine.n_answerable
        ));
    }
    let delta = rerank_margin.auroc - top_cosine.auroc;
    let outcome = if delta >= H1_BEAT_DELTA {
        H1Outcome::Beat
    } else if delta >= H1_KILL_DELTA {
        H1Outcome::Survives
    } else {
        H1Outcome::Killed
    };
    Ok(H1Verdict {
        outcome,
        rerank_margin_auroc: rerank_margin.auroc,
        top_cosine_auroc: top_cosine.auroc,
        delta,
        kill_threshold_delta: H1_KILL_DELTA,
        beat_threshold_delta: H1_BEAT_DELTA,
        n_pairs: rerank_margin.n_pairs,
        criterion: format!(
            "NATIVE_GROUNDING.md §7.3 — kill if margin AUROC < top_cosine + {H1_KILL_DELTA}; \
             beat if margin AUROC >= top_cosine + {H1_BEAT_DELTA}"
        ),
    })
}

/// AUROC by the Mann-Whitney U identity, with mid-ranks for ties.
///
/// The tie handling is the load-bearing part: a scorer that returns the
/// same value for every pair — a genuine failure mode for a rerank slot
/// that silently failed to load its weights — must score 0.5, not 1.0.
fn auroc_mid_rank(scored: &[ScoredPair]) -> f64 {
    let mut idx: Vec<usize> = (0..scored.len()).collect();
    idx.sort_by(|&a, &b| {
        scored[a]
            .score
            .partial_cmp(&scored[b].score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Mid-rank each tie group (ranks are 1-based).
    let mut ranks = vec![0.0f64; scored.len()];
    let mut i = 0usize;
    while i < idx.len() {
        let mut j = i + 1;
        while j < idx.len()
            && scored[idx[j]].score.to_bits() == scored[idx[i]].score.to_bits()
        {
            j += 1;
        }
        // Ranks i+1 ..= j, averaged.
        let mid = ((i + 1) as f64 + j as f64) / 2.0;
        for &k in &idx[i..j] {
            ranks[k] = mid;
        }
        i = j;
    }

    let n_pos = scored.iter().filter(|s| s.answerable).count() as f64;
    let n_neg = scored.len() as f64 - n_pos;
    let rank_sum_pos: f64 = scored
        .iter()
        .zip(&ranks)
        .filter(|(s, _)| s.answerable)
        .map(|(_, r)| *r)
        .sum();
    (rank_sum_pos - n_pos * (n_pos + 1.0) / 2.0) / (n_pos * n_neg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, corpus: &str, answerable: bool, score: f32) -> ScoredPair {
        ScoredPair {
            id: id.into(),
            corpus_id: corpus.into(),
            answerable,
            score,
        }
    }

    /// A perfectly separating signal: every answerable outscores every
    /// absent.
    fn separable() -> Vec<ScoredPair> {
        vec![
            p("a1", "c", true, 0.9),
            p("a2", "c", true, 0.8),
            p("n1", "c", false, 0.2),
            p("n2", "c", false, 0.1),
        ]
    }

    #[test]
    fn a_perfect_separation_is_auroc_one_and_full_recall_at_zero_false_alarm() {
        let c = build("test", &separable()).unwrap();
        assert!((c.auroc - 1.0).abs() < 1e-12, "{}", c.auroc);
        let at5 = &c.honesty_recall_at_false_alarm[&5];
        assert!((at5.honesty_recall - 1.0).abs() < 1e-12);
        assert!((at5.false_alarm - 0.0).abs() < 1e-12);
        assert!((c.best_balanced_accuracy - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_perfectly_inverted_signal_is_auroc_zero() {
        let inverted: Vec<ScoredPair> = separable()
            .into_iter()
            .map(|s| ScoredPair {
                score: 1.0 - s.score,
                ..s
            })
            .collect();
        let c = build("test", &inverted).unwrap();
        assert!((c.auroc - 0.0).abs() < 1e-12, "{}", c.auroc);
    }

    #[test]
    fn a_constant_signal_is_auroc_one_half_not_one() {
        // The failure this exists for: a rerank slot that loaded but is
        // returning one value for every pair. Without mid-rank tie
        // handling this comes out as 1.0 and reads as a triumph.
        let flat: Vec<ScoredPair> = separable()
            .into_iter()
            .map(|s| ScoredPair { score: 0.5, ..s })
            .collect();
        let c = build("test", &flat).unwrap();
        assert!((c.auroc - 0.5).abs() < 1e-12, "constant signal scored {}", c.auroc);
    }

    #[test]
    fn honesty_recall_never_exceeds_its_false_alarm_budget() {
        // A hard, overlapping set — the budgets have to bind here.
        let mut v = Vec::new();
        for i in 0..50 {
            v.push(p(&format!("a{i}"), "c", true, 0.4 + (i as f32) * 0.01));
            v.push(p(&format!("n{i}"), "c", false, 0.2 + (i as f32) * 0.01));
        }
        let c = build("test", &v).unwrap();
        for (&budget, r) in &c.honesty_recall_at_false_alarm {
            assert!(
                r.false_alarm <= f64::from(budget) / 100.0 + 1e-9,
                "budget {budget}% realized false_alarm {}",
                r.false_alarm
            );
        }
        // And a tighter budget can never buy MORE recall than a looser one.
        let r5 = c.honesty_recall_at_false_alarm[&5].honesty_recall;
        let r10 = c.honesty_recall_at_false_alarm[&10].honesty_recall;
        let r20 = c.honesty_recall_at_false_alarm[&20].honesty_recall;
        assert!(r5 <= r10 + 1e-12 && r10 <= r20 + 1e-12, "{r5} {r10} {r20}");
    }

    #[test]
    fn one_class_only_is_refused_rather_than_scored_as_a_coin_flip() {
        let only_pos = vec![p("a1", "c", true, 0.9), p("a2", "c", true, 0.1)];
        let err = build("test", &only_pos).unwrap_err();
        assert!(err.contains("only one class"), "{err}");
        let err = build("test", &[]).unwrap_err();
        assert!(err.contains("0 scored pairs"), "{err}");
    }

    #[test]
    fn the_curve_is_byte_identical_across_repeats() {
        // The rescore-determinism pattern: the instrument is validated
        // before any result computed with it is believed (ARCH §18.4).
        let v = separable();
        let a = serde_json::to_string(&build("test", &v).unwrap()).unwrap();
        for _ in 0..3 {
            let b = serde_json::to_string(&build("test", &v).unwrap()).unwrap();
            assert_eq!(a, b, "the curve builder is not deterministic");
        }
    }

    #[test]
    fn input_order_does_not_change_the_curve() {
        // Scored pairs arrive in whatever order the harness produced them;
        // the artifact must not depend on it.
        let v = separable();
        let mut shuffled = v.clone();
        shuffled.reverse();
        shuffled.swap(0, 2);
        assert_eq!(
            serde_json::to_string(&build("t", &v).unwrap()).unwrap(),
            serde_json::to_string(&build("t", &shuffled).unwrap()).unwrap()
        );
    }

    /// Build a curve whose AUROC is a chosen value, by planting
    /// `concordant` of `n*n` answerable-beats-absent comparisons.
    fn curve_with_auroc(signal: &str, answerable: &[f32], absent: &[f32]) -> OperatingCurve {
        let mut v = Vec::new();
        for (i, s) in answerable.iter().enumerate() {
            v.push(p(&format!("a{i}"), "c", true, *s));
        }
        for (i, s) in absent.iter().enumerate() {
            v.push(p(&format!("n{i}"), "c", false, *s));
        }
        build(signal, &v).unwrap()
    }

    #[test]
    fn the_h1_gate_is_allowed_to_kill() {
        // The whole point of §7.3: this gate must be able to say no.
        // cosine separates perfectly (AUROC 1.0); margin is a coin flip.
        let cosine = curve_with_auroc("top_cosine", &[0.9, 0.8], &[0.2, 0.1]);
        let margin = curve_with_auroc("rerank_margin", &[0.9, 0.1], &[0.8, 0.2]);
        let v = h1_verdict(&margin, &cosine).unwrap();
        assert_eq!(v.outcome, H1Outcome::Killed);
        assert!(v.delta < H1_KILL_DELTA, "{}", v.delta);
    }

    #[test]
    fn the_h1_gate_separates_beat_from_merely_surviving() {
        // margin 1.0 vs cosine 0.5 → delta 0.50 → Beat.
        let beat = h1_verdict(
            &curve_with_auroc("rerank_margin", &[0.9, 0.8], &[0.2, 0.1]),
            &curve_with_auroc("top_cosine", &[0.9, 0.1], &[0.8, 0.2]),
        )
        .unwrap();
        assert_eq!(beat.outcome, H1Outcome::Beat);

        // margin 0.75 vs cosine 0.70 → delta 0.05 → exactly the kill bar,
        // which is `>=`, so it SURVIVES and does not beat. The boundary is
        // pinned because an off-by-one-comparison here silently flips a
        // kill into a pass.
        let margin = curve_with_auroc("rerank_margin", &[0.9, 0.6], &[0.7, 0.1]);
        let cosine = curve_with_auroc("top_cosine", &[1.0, 0.6], &[0.7, 0.1]);
        assert!((margin.auroc - 0.75).abs() < 1e-12, "{}", margin.auroc);
        assert!((cosine.auroc - 0.75).abs() < 1e-12, "{}", cosine.auroc);
        let tie = h1_verdict(&margin, &cosine).unwrap();
        assert_eq!(tie.outcome, H1Outcome::Killed, "delta 0 must kill");
    }

    #[test]
    fn the_h1_gate_refuses_curves_from_different_sets() {
        let a = curve_with_auroc("rerank_margin", &[0.9, 0.8], &[0.2, 0.1]);
        let b = curve_with_auroc("top_cosine", &[0.9, 0.8, 0.7], &[0.2, 0.1]);
        let err = h1_verdict(&a, &b).unwrap_err();
        assert!(err.contains("not over the same pairs"), "{err}");
    }

    #[test]
    fn a_frozen_fixture_reproduces_its_committed_numbers() {
        // Golden values, hand-checkable. Scores:
        //   answerable: 0.9, 0.6   absent: 0.7, 0.1
        // Pairs (answerable, absent): (0.9,0.7)✓ (0.9,0.1)✓ (0.6,0.7)✗
        //   (0.6,0.1)✓  → 3/4 concordant → AUROC 0.75.
        let v = vec![
            p("a1", "c", true, 0.9),
            p("a2", "c", true, 0.6),
            p("n1", "c", false, 0.7),
            p("n2", "c", false, 0.1),
        ];
        let c = build("frozen", &v).unwrap();
        assert!((c.auroc - 0.75).abs() < 1e-12, "{}", c.auroc);
        // At a 0% realized false-alarm the best threshold is just above
        // 0.1: it abstains on n2 only → honesty-recall 0.5.
        let at5 = &c.honesty_recall_at_false_alarm[&5];
        assert!((at5.honesty_recall - 0.5).abs() < 1e-12, "{at5:?}");
        assert!((at5.false_alarm - 0.0).abs() < 1e-12, "{at5:?}");
        assert_eq!(c.n_answerable, 2);
        assert_eq!(c.n_absent, 2);
    }
}
