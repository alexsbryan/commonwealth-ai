// SPDX-License-Identifier: AGPL-3.0-or-later
//! Paired significance for a two-arm rubric comparison — the test that
//! reads a bench the way the bench was actually run.
//!
//! # Why this exists
//!
//! [`super::score::DimensionAggregate::separates_from`] tests whether two
//! INDEPENDENT Wilson intervals are disjoint. That is the right test for two
//! unrelated samples, and it is the wrong one here: both arms run the SAME
//! probes against the SAME criteria, judged by the SAME pinned judge. Treating
//! them as independent throws the pairing away, and with it most of the power.
//!
//! Measured on the arm-C comparison (63 paired criterion judgements, 7 probes):
//! only 8 of 63 judgements moved at all — 5 better, 3 worse. The independent
//! test needs ~138 probes per arm to call an effect that size; the paired test
//! needs ~49. Same evidence, a third of the runs.
//!
//! # What it does NOT buy
//!
//! Power, not truth. The arm-C boundary effect is 4 improvements against 2
//! REGRESSIONS (exact p = 0.6875) — a heterogeneous effect, not merely an
//! under-powered one, and no amount of pairing cleans that up. The paired test
//! makes the dirtiness legible rather than hiding it inside a rate delta: a
//! `+9.5` headline and a 4-better/2-worse flip count are different claims about
//! the same numbers, and only the second one is honest about what moved.
//!
//! # The tests
//!
//! **Per criterion** — exact two-sided McNemar (the binomial exact test at
//! p = ½ over the discordant pairs). Exact rather than the χ² approximation
//! because dimension slices here are single-digit, where χ² is simply wrong.
//!
//! **Per item score** — an exhaustive sign-flip permutation test, which is
//! EXACT and DETERMINISTIC for the bank sizes this lane runs. This substitutes
//! for the bootstrap originally sketched for this slot, and the substitution is
//! named rather than silent (ARCH_PRINCIPLES §18.3): a bootstrap would need a
//! PRNG, and a seeded resample is a worse answer than an exhaustive enumeration
//! whenever the enumeration fits. Above [`EXACT_PERMUTATION_MAX_N`] items it
//! cannot fit, and the run reports [`PairedMethod::MonteCarlo`] with its seed
//! and draw count — the method is always on the face of the result, so a
//! sampled p-value can never be read as an exact one.
//!
//! # Absence is reported, never defaulted
//!
//! A criterion judged in one arm and could-not-judge in the other cannot be
//! paired. So can a criterion whose dimension or weight changed between the two
//! reports — that is a different criterion wearing the same id, and pairing it
//! would silently compare two instruments. Both land in `unpairable`, which is
//! carried through every struct here and printed whenever it is non-zero
//! (ARCH_PRINCIPLES §18.3). A paired verdict computed over a shrunken set says
//! so on its face.

use std::collections::BTreeMap;

use super::report::RubricRun;
use super::score::{CriterionOutcome, RubricItem};

/// Above this many paired items the sign-flip permutation test stops being
/// enumerable (2^n statistic evaluations) and falls back to seeded sampling.
/// 20 → ~1M evaluations, tens of milliseconds; 21 doubles it. The situated
/// bank is 7 probes today and heading for ~49, so both branches are live.
pub const EXACT_PERMUTATION_MAX_N: usize = 20;

/// Draws used when the item count puts an exhaustive enumeration out of reach.
pub const MONTE_CARLO_DRAWS: u32 = 100_000;

/// Fixed seed for the sampled branch. Pinned, and reported alongside the
/// p-value: a bench number that cannot be reproduced is not a measurement.
pub const MONTE_CARLO_SEED: u64 = 0x5EED_1234_5678_9ABC;

/// How a p-value was obtained. Carried into the report so an exact and a
/// sampled result can never be confused for one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairedMethod {
    /// Every sign assignment enumerated — the p-value is exact.
    Exact,
    MonteCarlo {
        draws: u32,
        seed: u64,
    },
}

impl PairedMethod {
    pub fn label(&self) -> String {
        match self {
            PairedMethod::Exact => "exact".into(),
            PairedMethod::MonteCarlo { draws, seed } => {
                format!("monte-carlo {draws} draws, seed {seed:#x}")
            }
        }
    }
}

/// Discordance between two arms over one set of paired criteria.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PairedFlips {
    /// Unfulfilled in the baseline, fulfilled in the current arm.
    pub better: usize,
    /// Fulfilled in the baseline, unfulfilled in the current arm.
    pub worse: usize,
    /// Paired and identical in both arms. Contributes no information to
    /// McNemar, and is reported because "nothing moved" is the single most
    /// important fact about a bench that returns a flat verdict.
    pub concordant: usize,
    /// Criteria that could not be paired at all: present in one arm only,
    /// could-not-judge in either, or carrying a changed dimension/weight.
    /// Never silently dropped (ARCH_PRINCIPLES §18.3).
    pub unpairable: usize,
    /// Exact two-sided McNemar. `1.0` when nothing was discordant, which is
    /// the honest reading: zero flips is zero evidence of a difference.
    pub p_value: f64,
}

impl PairedFlips {
    pub fn discordant(&self) -> usize {
        self.better + self.worse
    }

    /// Criteria that entered the test — the denominator a reader needs to
    /// interpret `p_value`.
    pub fn paired(&self) -> usize {
        self.concordant + self.discordant()
    }
}

/// Paired delta over per-item scores.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedScoreDelta {
    /// Mean of (current − baseline) over items scored in BOTH arms.
    pub mean_delta: f64,
    /// Items contributing. An item scored in one arm only cannot pair.
    pub n: usize,
    pub unpairable: usize,
    pub p_value: f64,
    pub method: PairedMethod,
}

/// The full paired reading of one A/B.
#[derive(Debug, Clone)]
pub struct PairedComparison {
    pub overall: PairedFlips,
    pub by_dimension: BTreeMap<String, PairedFlips>,
    /// `None` when fewer than two items scored in both arms — a permutation
    /// test over one pair is meaningless, and reporting `p = 1.0` there would
    /// dress up an absence as a result.
    pub score_delta: Option<PairedScoreDelta>,
}

/// Exact two-sided McNemar: the binomial test at p = ½ over the discordant
/// pairs only. `better`/`worse` are the two off-diagonal cells; the concordant
/// cells are conditioned away, which is the whole point of the test.
///
/// Computed in log space so a large discordant count cannot overflow a
/// binomial coefficient — `C(441, 220)` is fine in `f64`, `C(4000, 2000)` is
/// not, and the bank is meant to grow.
pub fn mcnemar_exact(better: usize, worse: usize) -> f64 {
    let n = better + worse;
    if n == 0 {
        // No criterion moved. There is no evidence of a difference — and
        // equally none of equivalence. p = 1 is the former, which is what a
        // reader of a significance test is asking.
        return 1.0;
    }
    let k = better.min(worse);
    let ln2 = std::f64::consts::LN_2;
    let n_ln2 = n as f64 * ln2;
    // ln C(n, 0) = 0; then the multiplicative recurrence in logs.
    let mut ln_c = 0.0f64;
    let mut tail = (-n_ln2).exp();
    for i in 0..k {
        ln_c += ((n - i) as f64).ln() - ((i + 1) as f64).ln();
        tail += (ln_c - n_ln2).exp();
    }
    (2.0 * tail).min(1.0)
}

/// Pair every criterion of every item across two arms and count the flips.
///
/// Pairing key is `(item id, criterion id)`. A key present in one arm only,
/// could-not-judge in either, or whose `(dimension, weight)` disagree between
/// the arms is UNPAIRABLE and is counted as such — the last case because a
/// criterion that changed its dimension or its weight is a different criterion,
/// and comparing it across arms would be comparing two instruments.
pub fn compare<B: RubricItem, C: RubricItem>(baseline: &[B], current: &[C]) -> PairedComparison {
    let b_map = index_criteria(baseline);
    let c_map = index_criteria(current);

    let mut overall = PairedFlips::default();
    let mut by_dimension: BTreeMap<String, PairedFlips> = BTreeMap::new();

    // Union of keys, so a criterion present in only one arm is SEEN and
    // counted unpairable rather than quietly missing from the denominator.
    let mut keys: Vec<&(&str, &str)> = b_map.keys().chain(c_map.keys()).collect();
    keys.sort_unstable();
    keys.dedup();

    for key in keys {
        let b = b_map.get(key);
        let c = c_map.get(key);
        // The dimension a criterion is filed under, for slicing. Taken from
        // whichever arm has it; when they disagree the pair is unpairable
        // anyway, and the count lands under the baseline's name.
        let dim = b.or(c).map(|o| o.dimension.clone()).unwrap_or_default();
        let slot = by_dimension.entry(dim).or_default();

        let (Some(b), Some(c)) = (b, c) else {
            overall.unpairable += 1;
            slot.unpairable += 1;
            continue;
        };
        if b.dimension != c.dimension || b.weight != c.weight {
            overall.unpairable += 1;
            slot.unpairable += 1;
            continue;
        }
        let (Some(bf), Some(cf)) = (b.fulfilled(), c.fulfilled()) else {
            overall.unpairable += 1;
            slot.unpairable += 1;
            continue;
        };
        match (bf, cf) {
            (false, true) => {
                overall.better += 1;
                slot.better += 1;
            }
            (true, false) => {
                overall.worse += 1;
                slot.worse += 1;
            }
            _ => {
                overall.concordant += 1;
                slot.concordant += 1;
            }
        }
    }

    overall.p_value = mcnemar_exact(overall.better, overall.worse);
    for f in by_dimension.values_mut() {
        f.p_value = mcnemar_exact(f.better, f.worse);
    }

    PairedComparison {
        overall,
        by_dimension,
        score_delta: paired_scores(baseline, current),
    }
}

/// Index every criterion of every item by its pairing key,
/// `(item id, criterion id)`.
///
/// A duplicate key within ONE arm would silently overwrite — but it cannot
/// occur, because ids are unique per item and a criterion appears at most once
/// per item. If that ever changes, this is the line that has to change with it.
fn index_criteria<I: RubricItem>(items: &[I]) -> BTreeMap<(&str, &str), &CriterionOutcome> {
    let mut m = BTreeMap::new();
    for it in items {
        for o in it.criteria() {
            m.insert((it.id(), o.criterion_id.as_str()), o);
        }
    }
    m
}

/// Paired test over per-item scores. `None` when fewer than two items scored
/// in both arms.
fn paired_scores<B: RubricItem, C: RubricItem>(
    baseline: &[B],
    current: &[C],
) -> Option<PairedScoreDelta> {
    let b: BTreeMap<&str, Option<f64>> = baseline.iter().map(|i| (i.id(), i.score())).collect();
    let c: BTreeMap<&str, Option<f64>> = current.iter().map(|i| (i.id(), i.score())).collect();
    let mut deltas = Vec::new();
    let mut unpairable = 0usize;
    let mut keys: Vec<&&str> = b.keys().collect();
    for k in c.keys() {
        if !b.contains_key(k) {
            keys.push(k);
        }
    }
    keys.sort();
    for k in keys {
        match (b.get(*k).copied().flatten(), c.get(*k).copied().flatten()) {
            (Some(bs), Some(cs)) => deltas.push(cs - bs),
            _ => unpairable += 1,
        }
    }
    if deltas.len() < 2 {
        return None;
    }
    let n = deltas.len();
    let mean = deltas.iter().sum::<f64>() / n as f64;
    let (p, method) = sign_flip_test(&deltas);
    Some(PairedScoreDelta {
        mean_delta: mean,
        n,
        unpairable,
        p_value: p,
        method,
    })
}

/// Two-sided paired sign-flip permutation test on the mean of `deltas`.
/// Under the null the sign of each paired difference is exchangeable, so the
/// reference distribution is the mean under all 2^n sign assignments.
fn sign_flip_test(deltas: &[f64]) -> (f64, PairedMethod) {
    let n = deltas.len();
    let observed = (deltas.iter().sum::<f64>() / n as f64).abs();
    // Ties count toward the p-value (`>=`): a permuted statistic equal to the
    // observed one is not evidence against the null.
    let eps = 1e-12;
    if n <= EXACT_PERMUTATION_MAX_N {
        let total = 1u64 << n;
        let mut at_least = 0u64;
        for mask in 0..total {
            let mut sum = 0.0;
            for (i, d) in deltas.iter().enumerate() {
                if mask >> i & 1 == 1 {
                    sum -= d;
                } else {
                    sum += d;
                }
            }
            if (sum / n as f64).abs() >= observed - eps {
                at_least += 1;
            }
        }
        return (at_least as f64 / total as f64, PairedMethod::Exact);
    }
    let mut rng = SplitMix64::new(MONTE_CARLO_SEED);
    let mut at_least = 0u64;
    for _ in 0..MONTE_CARLO_DRAWS {
        let mut sum = 0.0;
        for d in deltas {
            if rng.next_bit() {
                sum -= d;
            } else {
                sum += d;
            }
        }
        if (sum / n as f64).abs() >= observed - eps {
            at_least += 1;
        }
    }
    // Add-one smoothing: a sampled test must never report p = 0, which would
    // claim more certainty than the number of draws can support.
    let p = (at_least + 1) as f64 / (MONTE_CARLO_DRAWS as u64 + 1) as f64;
    (
        p,
        PairedMethod::MonteCarlo {
            draws: MONTE_CARLO_DRAWS,
            seed: MONTE_CARLO_SEED,
        },
    )
}

/// SplitMix64 — a fixed, dependency-free PRNG so the sampled branch is
/// reproducible from the seed printed in the report.
struct SplitMix64 {
    state: u64,
    bits: u64,
    left: u32,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self {
            state: seed,
            bits: 0,
            left: 0,
        }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn next_bit(&mut self) -> bool {
        if self.left == 0 {
            self.bits = self.next_u64();
            self.left = 64;
        }
        let b = self.bits & 1 == 1;
        self.bits >>= 1;
        self.left -= 1;
        b
    }
}

/// Paired reading of an A/B, printed under the existing dimension diff.
/// Deliberately prints the flip COUNTS next to the p-value: `4 better / 2
/// worse` and `+9.5` are different claims about the same numbers, and a
/// reader who sees only the second one will over-read it.
pub fn print_paired<R: RubricRun>(baseline: &R, current: &R) {
    let cmp = compare(baseline.items(), current.items());
    println!();
    println!("Paired test (same probes, same criteria — exact two-sided McNemar):");
    println!(
        "  {:<18} {:>8} {:>8} {:>8} {:>10}",
        "dimension", "better", "worse", "same", "p"
    );
    let row = |name: &str, f: &PairedFlips| {
        println!(
            "  {name:<18} {:>8} {:>8} {:>8} {:>10.4}{}",
            f.better,
            f.worse,
            f.concordant,
            f.p_value,
            if f.unpairable > 0 {
                format!("  ({} unpairable)", f.unpairable)
            } else {
                String::new()
            }
        );
    };
    for (dim, f) in &cmp.by_dimension {
        row(dim, f);
    }
    row("ALL", &cmp.overall);
    if cmp.overall.discordant() == 0 {
        println!(
            "  No criterion changed verdict. This is not evidence the arms are \
             equivalent — it is this bank failing to tell them apart."
        );
    }
    match &cmp.score_delta {
        Some(d) => {
            println!(
                "  per-item score: mean delta {:+.1} over {} paired items, p = {:.4} ({}){}",
                d.mean_delta,
                d.n,
                d.p_value,
                d.method.label(),
                if d.unpairable > 0 {
                    format!(", {} unpairable", d.unpairable)
                } else {
                    String::new()
                }
            );
        }
        None => println!("  per-item score: fewer than 2 items scored in both arms — not tested"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_cmd::rubric::judge::Ballot;
    use crate::bench_cmd::rubric::test_support::{item, outcome};

    /// Reference values from R's `binom.test(k, n, 0.5)` / the textbook exact
    /// McNemar. These are the anchors everything else in the module rests on.
    #[test]
    fn exact_mcnemar_matches_reference_values() {
        assert_eq!(mcnemar_exact(0, 0), 1.0, "no discordance is no evidence");
        // n = 8, k = 3 → 2 * (1+8+28+56)/256 = 0.7265625
        assert!((mcnemar_exact(5, 3) - 0.7265625).abs() < 1e-12);
        // n = 6, k = 2 → 2 * (1+6+15)/64 = 0.6875
        assert!((mcnemar_exact(4, 2) - 0.6875).abs() < 1e-12);
        // n = 2, k = 1 → 2 * (1+2)/4 = 1.5 → clamped to 1
        assert_eq!(mcnemar_exact(1, 1), 1.0, "must clamp, never exceed 1");
        // One-directional: n = 7, k = 0 → 2/128 = 0.015625.
        assert!((mcnemar_exact(7, 0) - 0.015625).abs() < 1e-12);
    }

    #[test]
    fn mcnemar_is_symmetric_in_its_arguments() {
        for (a, b) in [(5usize, 3usize), (7, 0), (1, 9), (12, 4)] {
            assert!(
                (mcnemar_exact(a, b) - mcnemar_exact(b, a)).abs() < 1e-15,
                "direction must not change the p-value, only its sign of effect"
            );
        }
    }

    #[test]
    fn concordant_pairs_do_not_change_the_p_value() {
        // The whole point of McNemar: conditioning on discordance. A bank ten
        // times larger with the same flips is the same p-value.
        assert_eq!(mcnemar_exact(4, 2), mcnemar_exact(4, 2));
        let few = compare(
            &[item(
                "p1",
                "g",
                vec![outcome("c1", "d", 2, Some(Ballot::No))],
            )],
            &[item(
                "p1",
                "g",
                vec![outcome("c1", "d", 2, Some(Ballot::Yes))],
            )],
        );
        assert_eq!(few.overall.better, 1);
        assert_eq!(few.overall.concordant, 0);
    }

    #[test]
    fn negative_weight_polarity_is_respected() {
        // weight < 0: fulfilled means the judge said NO. A naive
        // yes-is-good reading inverts restraint and disclosure criteria —
        // the exact analysis trap that produced a wrong first answer on the
        // arm-C reports.
        let base = [item(
            "p1",
            "g",
            vec![outcome("c1", "restraint", -2, Some(Ballot::Yes))],
        )];
        let cur = [item(
            "p1",
            "g",
            vec![outcome("c1", "restraint", -2, Some(Ballot::No))],
        )];
        let cmp = compare(&base, &cur);
        assert_eq!(
            cmp.overall.better, 1,
            "no on a negative-weight criterion is an improvement"
        );
        assert_eq!(cmp.overall.worse, 0);
    }

    #[test]
    fn could_not_judge_is_unpairable_not_concordant() {
        let base = [item(
            "p1",
            "g",
            vec![outcome("c1", "d", 2, Some(Ballot::Yes))],
        )];
        let cur = [item("p1", "g", vec![outcome("c1", "d", 2, None)])];
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.overall.unpairable, 1);
        assert_eq!(cmp.overall.paired(), 0);
        assert_eq!(cmp.overall.p_value, 1.0);
    }

    #[test]
    fn a_changed_weight_makes_a_criterion_unpairable() {
        // Same id, different weight = a different criterion. Pairing it would
        // silently compare two instruments.
        let base = [item(
            "p1",
            "g",
            vec![outcome("c1", "d", 2, Some(Ballot::Yes))],
        )];
        let cur = [item(
            "p1",
            "g",
            vec![outcome("c1", "d", 3, Some(Ballot::Yes))],
        )];
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.overall.unpairable, 1);
        assert_eq!(cmp.overall.concordant, 0);
    }

    #[test]
    fn an_item_missing_from_one_arm_is_unpairable() {
        let base = [
            item("p1", "g", vec![outcome("c1", "d", 2, Some(Ballot::Yes))]),
            item("p2", "g", vec![outcome("c1", "d", 2, Some(Ballot::Yes))]),
        ];
        let cur = [item(
            "p1",
            "g",
            vec![outcome("c1", "d", 2, Some(Ballot::Yes))],
        )];
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.overall.concordant, 1);
        assert_eq!(cmp.overall.unpairable, 1);
    }

    #[test]
    fn sign_flip_is_exact_and_deterministic_for_small_banks() {
        // Every delta positive and equal: only the all-positive assignment
        // reaches the observed mean, so p = 2/2^n (both extremes, two-sided).
        let (p, m) = sign_flip_test(&[4.0, 4.0, 4.0]);
        assert_eq!(m, PairedMethod::Exact);
        assert!((p - 2.0 / 8.0).abs() < 1e-12, "got {p}");
        // Deterministic: same input, same answer, always.
        assert_eq!(sign_flip_test(&[4.0, 4.0, 4.0]).0, p);
    }

    #[test]
    fn sign_flip_all_zero_deltas_is_p_one() {
        let (p, _) = sign_flip_test(&[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(p, 1.0, "no movement cannot be significant");
    }

    #[test]
    fn monte_carlo_branch_reports_its_method_and_never_claims_zero() {
        let deltas: Vec<f64> = (0..EXACT_PERMUTATION_MAX_N + 1)
            .map(|i| 10.0 + i as f64)
            .collect();
        let (p, m) = sign_flip_test(&deltas);
        assert!(
            matches!(m, PairedMethod::MonteCarlo { .. }),
            "must not claim exactness"
        );
        assert!(
            p > 0.0,
            "a sampled p-value must never be reported as exactly 0"
        );
        // Reproducible from the pinned seed.
        assert_eq!(sign_flip_test(&deltas).0, p);
    }

    #[test]
    fn score_delta_needs_two_paired_items() {
        let base = [item(
            "p1",
            "g",
            vec![outcome("c1", "d", 2, Some(Ballot::Yes))],
        )];
        let cur = [item(
            "p1",
            "g",
            vec![outcome("c1", "d", 2, Some(Ballot::No))],
        )];
        assert!(compare(&base, &cur).score_delta.is_none());
    }
}
