// SPDX-License-Identifier: AGPL-3.0-or-later
//! H2b — the evidence counterfactual (`NATIVE_GROUNDING.md` §5 H2, post-amendment).
//!
//! H2's original perturbation axis was sampling noise, and it was measured dead:
//! k=5 draws produced **1 distinct value out of 5 on every turn at every
//! coherent temperature** (`h2/FINDINGS.md` §2, commit `9900da95`). H2b keeps the
//! whole apparatus — the multi-seq decoder, the pinned seeds, the committed
//! meaning clusterer and its floor — and changes only what is perturbed: the
//! **evidence**.
//!
//! | Arm | Prompt | What it answers |
//! |---|---|---|
//! | **A** | value prompt WITH the sealed chunks | what the model says when it can read the evidence |
//! | **B** | the identical prompt MINUS the chunks | what it says when it cannot — the order's ablation |
//! | **P** | the question with the evidence frame removed | what it knows parametrically — the leak detector |
//!
//! Arm P is an **addition to the order's two arms, named rather than
//! substituted**, and the reason is in [`sovereign_inference::k_sample::build_parametric_prompt`]:
//! arm B still instructs *"If the EVIDENCE does not state it, reply with exactly
//! NONE"*, and an empty evidence block satisfies that antecedent on every pair.
//! A leak rate read off arm B could not come out non-zero for a reason that has
//! nothing to do with what the model knows, and P1 would be answered by the
//! prompt rather than by the measurement. Arm B remains the gate's statistic
//! exactly as ordered; arm P feeds `parametric_known` and nothing else.
//!
//! # The three stages, and the two determinism seams between them
//!
//! | Stage | Command | Model | Writes |
//! |---|---|---|---|
//! | 1 | `h2b-arms` | the **generator** | `h2b_arms.jsonl` — raw arms, typed outcomes, margins |
//! | 2 | `h2b-gate` | the **reranker** | `h2b_scores.jsonl` — + `evidence_dependence` |
//! | 3 | `h2b-gate --from-scores` | none | `h2b_verdict.json` |
//!
//! The generator and the reranker are never co-resident, which is what lets
//! stage 1 run a 36B on this host at all. Each seam replays: the same arms file
//! rescores identically, and the same scores file re-verdicts byte-for-byte.
//!
//! # What this module is
//!
//! [`mod`] here is the **pure** half — the typing rule, the statistics, the
//! split, the kill bar — so every branch of the verdict is testable with no
//! model, including the branches that refuse. [`arms`] and [`gate`] bind it to
//! models and a command line.

pub mod arms;
pub mod gate;

use serde::{Deserialize, Serialize};

/// Which question the three arms ask. **A closed set, so it is an enum**
/// (principle 9), and it is recorded on every row so no artifact can be read
/// without knowing which unit produced it.
///
/// The default is [`ValueUnit::Value`], which is §5 H2's own unit. The
/// alternative exists because the label supply and the unit turned out not to
/// compose: **100.0% of the H1 calibration set's 4,207 questions open "Is it
/// true that …?"** (measured), and a proposition has no specific value to
/// extract. §18.3 — the mismatch is named on the command line and in the
/// artifact, never worked around quietly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueUnit {
    /// "Reply with only the specific value the EVIDENCE gives." §5 H2's unit,
    /// `extract_answer_value`'s budget, built for *wh*- probes.
    #[default]
    Value,
    /// "Reply YES, NO, or NONE." The same counterfactual asked in the form a
    /// proposition set can answer.
    Verdict,
}

impl ValueUnit {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "value" => Some(ValueUnit::Value),
            "verdict" => Some(ValueUnit::Verdict),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ValueUnit::Value => "value",
            ValueUnit::Verdict => "verdict",
        }
    }
}

/// What one arm produced. **A closed set, so it is an enum** (principle 9), and
/// every member is decided structurally — no keyword list votes on which one a
/// stream is.
///
/// The order is explicit that arm B's type is *kept, not coerced*: a refusal
/// under ablation is signal, and folding it into "no value" alongside a
/// truncated stream would erase the difference between a model that declined and
/// a model that never finished a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmOutcome {
    /// The stream terminated and cleaned to a value.
    Value,
    /// The stream cleaned to no value — `clean_value`'s own decline kernel, the
    /// incumbent's (`value_presence.rs:130-146`). Declining is declining
    /// whether or not the stream also ran out of budget, so this is tested
    /// first.
    Refusal,
    /// The stream cleaned to a value but **never emitted an end-of-generation
    /// token** — it is the first `VALUE_TOKEN_BUDGET` tokens of something
    /// longer, not an answer. Letting it into the equivalence clusterer would
    /// compare a truncated sentence against a value as though both were values.
    Garbage,
}

impl ArmOutcome {
    /// Type one decoded stream. **The one implementation** (principle 8): the
    /// run calls it, the gate's replay does not need to (the type is frozen in
    /// the artifact), and the tests below pin every branch.
    pub fn classify(value: &Option<String>, finished_eog: bool) -> Self {
        match value {
            None => ArmOutcome::Refusal,
            Some(_) if !finished_eog => ArmOutcome::Garbage,
            Some(_) => ArmOutcome::Value,
        }
    }

    /// Is this an outcome the equivalence clusterer may be asked about?
    ///
    /// `Garbage` is not. The order says the clusterer *"decides
    /// value-equivalence only between actual values"*, and a pair with a
    /// garbage arm therefore has **no** `evidence_dependence` — not a zero and
    /// not a one. §18.3: the absence is reported and the pair leaves the
    /// primary gate with its own count.
    pub fn is_comparable(self) -> bool {
        matches!(self, ArmOutcome::Value | ArmOutcome::Refusal)
    }
}

/// One arm's decode, frozen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmRecord {
    /// The raw decoded string, before cleaning. Kept because a stream that
    /// cleaned to `None` looks identical to one that produced nothing, and a
    /// glassbox artifact must tell those apart.
    pub raw: String,
    /// `clean_value(raw)` — the incumbent's kernel.
    pub value: Option<String>,
    pub outcome: ArmOutcome,
    /// Did the stream emit an end-of-generation token? The decider behind
    /// [`ArmOutcome::Garbage`], carried separately so a reader can audit the
    /// typing rather than trust it.
    pub finished_eog: bool,
    /// Prompt tokens for this arm. Arm B's is shorter than arm A's by exactly
    /// the chunks, and arm P's is shorter than both — the cost claim in §5 H2 is
    /// checkable from this field rather than estimated.
    pub prompt_tokens: usize,
    pub decoded_tokens: usize,
    /// §5 H2's `value_margin` for this arm: the mean top-1/top-2 logit margin
    /// over its decoded positions. `None` when the arm decoded nothing.
    pub mean_token_margin: Option<f32>,
    pub elapsed_ms: u128,
}

/// The three arms of one calibration pair, frozen. Stage 1's row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairArms {
    pub id: String,
    pub corpus_id: String,
    /// `sep` or `literary`, by H1's rule.
    pub family: String,
    /// The ground-truth label: is the value evidence-determined?
    pub answerable: bool,
    pub question: String,
    /// Pool size arm A saw.
    pub n_chunks: usize,
    /// Index into the pool of the claim's own supporting passage — `Some` on
    /// every answerable pair, `None` on every absent one. This is what the leak
    /// detector withholds from arm P and then checks against.
    pub evidence_index: Option<usize>,
    pub arm_a: ArmRecord,
    pub arm_b: ArmRecord,
    pub arm_p: ArmRecord,
    /// Did arm A reproduce byte-for-byte on a second decode? `None` when the
    /// repeat was not run. Greedy decoding should make this trivially true;
    /// **that is exactly why it is checked** — an instrument whose determinism
    /// is assumed rather than measured is not an instrument (§18.4).
    pub repeat_stable: Option<bool>,
    /// **P1's flag, and it does not look at arm A.**
    ///
    /// True when arm P's value is literally present in the WITHHELD supporting
    /// passage, under the incumbent's own presence kernel. Defining the leak as
    /// "arm B agreed with arm A" would make `parametric_known` synonymous with
    /// `evidence_dependence == 0`, and excluding those pairs would drive mean
    /// dependence on answerable to 1.0 by construction — a separation
    /// manufactured by the exclusion rule rather than measured.
    pub parametric_known: bool,
    /// Which question the arms were asked. Recorded per row so an artifact can
    /// never be read without knowing which unit produced it, and defaulted on
    /// deserialize so a file written before the unit existed reads back as the
    /// unit it in fact used.
    #[serde(default)]
    pub unit: ValueUnit,
}

/// One pair, scored. Stage 2's row — everything the verdict reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairScore {
    pub id: String,
    pub corpus_id: String,
    pub family: String,
    pub answerable: bool,
    /// `1 - equiv(value_A, value_B)`: 1.0 when the value flipped under
    /// ablation, 0.0 when it did not. `None` when either arm was
    /// [`ArmOutcome::Garbage`] and the clusterer was therefore never asked.
    pub evidence_dependence: Option<f32>,
    /// Which rung of the clusterer decided the pair, for the pairs it decided.
    /// Glassbox: a run whose merges are all cosine tie-breaks is a run whose
    /// floor is mis-calibrated, and that must be visible rather than inferred.
    pub equiv_rung: Option<String>,
    /// §5 H2's `value_margin`: arm A's mean token margin.
    pub value_margin: Option<f32>,
    /// H1's `rerank_margin` for the same pair id — the bar H2b is measured
    /// against, joined rather than recomputed so the two gates cannot disagree
    /// about what the margin was.
    pub rerank_margin: Option<f32>,
    pub parametric_known: bool,
    #[serde(default)]
    pub unit: ValueUnit,
    pub arm_a_outcome: ArmOutcome,
    pub arm_b_outcome: ArmOutcome,
    pub arm_p_outcome: ArmOutcome,
    /// Did arm A reproduce byte-for-byte on a re-decode? Carried FORWARD from
    /// the arms file rather than re-read out of it, so `--from-scores` is an
    /// exact replay. It was not, at first: stage 3 reported "determinism NOT
    /// re-checked" and its verdict therefore differed from the stage-2 one by
    /// two lines. A seam that cannot reproduce the artifact it replays is not
    /// a seam, and this field is what makes it one.
    #[serde(default)]
    pub repeat_stable: Option<bool>,
    /// Which side of the split this pair landed on. Frozen in the artifact so a
    /// re-verdict cannot re-roll it.
    pub split: Split,
}

/// The two sides of the leakage-free split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    /// Where the combination weight is fitted.
    Calibration,
    /// Where the kill bar is applied. Nothing is fitted here.
    Holdout,
}

/// Assign a pair to a split **by corpus, never by pair**.
///
/// A claim's answerable and absent pairs share k−1 pool members, and
/// neighbouring claims in one article share passages outright. Splitting per
/// pair would put near-duplicate evidence on both sides and the held-out AUROC
/// would be reporting memorised passages. Grouping by `corpus_id` is the
/// cheapest structural fix and it costs nothing: the set spans hundreds of SEP
/// articles.
///
/// Deterministic and pure — FNV-1a over the corpus id, so the same set splits
/// the same way on any host, in any order, forever. Not a hash chosen for
/// quality: it is chosen for being fixed and reimplementable by a reader.
pub fn split_of(corpus_id: &str) -> Split {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in corpus_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if h % 2 == 0 {
        Split::Holdout
    } else {
        Split::Calibration
    }
}

/// H1's corpus-family rule, one implementation, imported rather than
/// re-derived — `h1_gate.rs:78` is private to its module, and a second copy
/// that disagreed would split the families differently in two artifacts about
/// the same pairs.
pub fn family_of(corpus_id: &str) -> &'static str {
    if corpus_id.starts_with("sep-") {
        "sep"
    } else {
        "literary"
    }
}

/// Pearson correlation. `None` when either series is constant — a correlation
/// with a zero-variance side is undefined, and reporting 0.0 would claim a
/// measured independence where nothing was measurable.
pub fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for (x, y) in xs.iter().zip(ys) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    if sxx <= 0.0 || syy <= 0.0 {
        return None;
    }
    Some(sxy / (sxx.sqrt() * syy.sqrt()))
}

/// The grid the combination weight is chosen from, and the only degree of
/// freedom the combined score has.
///
/// `combined = rerank_margin + w · evidence_dependence`. One parameter, fitted
/// on the calibration split and **applied unchanged** to the held-out split. A
/// richer combiner (logistic regression on both features) would fit better and
/// would also make the +0.02 kill bar a measure of the combiner's capacity
/// rather than of the signal — with one weight, a combined score that beats the
/// margin does so because dependence carried information the margin did not.
pub const COMBINE_WEIGHTS: &[f32] = &[
    0.0, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 15.0, 20.0,
];

/// §7.3-style bar, quantified in the order **before any run**: the combined
/// score must beat margin-alone by at least this much AUROC on held-out
/// non-leaked pairs.
pub const KILL_BAR_COMBINED_DELTA: f64 = 0.02;

/// The orthogonality clause's AUROC floor for `evidence_dependence` alone.
pub const KILL_BAR_ORTHOGONAL_AUROC: f64 = 0.85;

/// The orthogonality clause's correlation ceiling. Above this the signal is a
/// noisy restatement of the margin and does not earn its own seat.
pub const KILL_BAR_ORTHOGONAL_PEARSON: f64 = 0.5;

/// The order's stop condition on leakage: above this, the non-leaked remainder
/// is too small to split held-out and the finding is the leak rate itself.
pub const STOP_LEAK_RATE: f64 = 0.60;

/// The order's stop condition on abstention: arm B refusing on more than this
/// fraction of absent pairs collapses the statistic to refusal-detection.
pub const STOP_ARM_B_REFUSAL_RATE: f64 = 0.80;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminated_stream_with_a_value_is_a_value() {
        assert_eq!(
            ArmOutcome::classify(&Some("Karl Yundt".into()), true),
            ArmOutcome::Value
        );
    }

    #[test]
    fn a_declining_stream_is_a_refusal_even_when_it_was_truncated() {
        // Declining is declining. A stream that said "not stated in the ev…"
        // and ran out of budget still declined, and typing it `Garbage` would
        // discard the abstention signal the order says to keep.
        assert_eq!(ArmOutcome::classify(&None, true), ArmOutcome::Refusal);
        assert_eq!(ArmOutcome::classify(&None, false), ArmOutcome::Refusal);
    }

    #[test]
    fn a_value_that_never_terminated_is_garbage_not_a_value() {
        // Watched to fail: this is the input that separates the typing rule
        // from `clean_value` alone. Without the EOG check, a truncated sentence
        // enters the clusterer as an answer.
        assert_eq!(
            ArmOutcome::classify(&Some("The evidence indicates that the".into()), false),
            ArmOutcome::Garbage
        );
        assert!(!ArmOutcome::Garbage.is_comparable());
        assert!(ArmOutcome::Value.is_comparable());
        assert!(ArmOutcome::Refusal.is_comparable());
    }

    #[test]
    fn the_split_is_stable_and_groups_by_corpus() {
        // Determinism: the artifact freezes the split, but a re-mine must
        // reproduce it or two runs are not comparable.
        assert_eq!(split_of("sep-abduction"), split_of("sep-abduction"));
        // Group-wise: every pair from one article lands on one side, which is
        // the whole point — the alternative leaks shared passages across it.
        let a = split_of("sep-abduction");
        for suffix in ["claim-0001:present", "claim-0001:absent", "claim-0400:present"] {
            let _ = suffix; // the id is not consulted; only the corpus is
            assert_eq!(split_of("sep-abduction"), a);
        }
        // And it does actually split — a constant "splitter" would pass every
        // assertion above.
        let mut seen_h = false;
        let mut seen_c = false;
        for c in [
            "sep-abduction", "sep-kant", "sep-plato", "sep-hume", "sep-frege",
            "sep-quine", "sep-tarski", "brothers-karamazov-book-1",
        ] {
            match split_of(c) {
                Split::Holdout => seen_h = true,
                Split::Calibration => seen_c = true,
            }
        }
        assert!(seen_h && seen_c, "a splitter that never splits is not one");
    }

    #[test]
    fn pearson_refuses_a_constant_series_rather_than_calling_it_uncorrelated() {
        // §18.3, and it matters here specifically: `evidence_dependence` is
        // binary, so a run where every pair flipped has zero variance. Reporting
        // r = 0.0 there would read as "orthogonal to the margin" and could
        // hand H2b the orthogonality clause on a set that proved nothing.
        assert_eq!(pearson(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]), None);
        assert_eq!(pearson(&[1.0], &[1.0]), None);
        assert_eq!(pearson(&[1.0, 2.0], &[1.0, 2.0, 3.0]), None);
        let r = pearson(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]).unwrap();
        assert!((r - 1.0).abs() < 1e-9, "perfect positive, got {r}");
        let r = pearson(&[1.0, 2.0, 3.0], &[6.0, 4.0, 2.0]).unwrap();
        assert!((r + 1.0).abs() < 1e-9, "perfect negative, got {r}");
    }

    #[test]
    fn the_family_rule_matches_h1s() {
        assert_eq!(family_of("sep-abduction"), "sep");
        assert_eq!(family_of("brothers-karamazov-book-1"), "literary");
    }

    #[test]
    fn the_kill_bar_constants_are_the_orders_and_a_change_is_a_spec_change() {
        // The bar was written before the run so it could not be argued after
        // it. Pinning the numbers is how that stays true across edits.
        assert_eq!(KILL_BAR_COMBINED_DELTA, 0.02);
        assert_eq!(KILL_BAR_ORTHOGONAL_AUROC, 0.85);
        assert_eq!(KILL_BAR_ORTHOGONAL_PEARSON, 0.5);
        assert_eq!(STOP_LEAK_RATE, 0.60);
        assert_eq!(STOP_ARM_B_REFUSAL_RATE, 0.80);
        assert!(
            COMBINE_WEIGHTS.first() == Some(&0.0),
            "the grid must contain 0 — margin-alone has to be reachable, or the \
             combined score cannot be worse than the margin and the +0.02 bar is \
             measuring nothing"
        );
    }
}
