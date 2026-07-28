// SPDX-License-Identifier: AGPL-3.0-or-later
//! Routing metrics beyond exact-match accuracy.
//!
//! ## Why accuracy alone stopped being informative
//!
//! The five routing banks score 96/96 as of 2026-07-16. A metric that
//! reports 100% on every run cannot tell you whether the router got
//! better, got worse in a way the bank cannot see, or got slower.
//! Three things it hides, all recoverable from data the run ALREADY
//! collects:
//!
//! 1. **Which layer decided.** `RoutingResult::coarse_intent` is
//!    `"EMBED_ROUTER"` when the embedding classifier owned the
//!    decision and a coarse LLM label otherwise. On the 2026-07-16
//!    baselines that is 11/27 for cells_v1 and 1/9 for
//!    future_timeline — so most of the "100%" is being bought with a
//!    ~1.2-2.4s LLM call per question. Accuracy scores those runs
//!    identically to one where the embed router owned everything at
//!    ~50ms. [`RoutingMetrics::embed_coverage`] is the number that
//!    separates them, and it is the one a threshold fit can move.
//!
//! 2. **Which intents are weak.** A single accuracy figure averages a
//!    34-exemplar intent with a 4-exemplar one. Per-intent
//!    precision/recall says which class is actually carrying the
//!    errors.
//!
//! 3. **What it confuses with what.** "3 misroutes" is a list;
//!    `expected → actual` counts are a diagnosis.
//!
//! ## What is deliberately NOT here
//!
//! An abstention rate. The full cascade always returns an intent — a
//! Pass 1 parse failure defaults to `KnowledgeQuery` — so abstention
//! is not observable at this layer. It is a property of the individual
//! embedding gates, and it is measured where it exists: by
//! `sovereign router fit` against a calibration bank whose cases can
//! say "this must not be committed".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::runner::RoutingResult;

/// Per-layer attribution: who decided, how often, how well, how fast.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayerStat {
    pub decided: usize,
    pub correct: usize,
    /// Mean wall time for the questions this layer decided. The gap
    /// between the embed layer and the LLM layers is the entire
    /// argument for raising coverage.
    pub mean_latency_ms: u64,
}

/// Per-intent precision / recall over the unambiguous rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntentStat {
    /// Rows whose EXPECTED intent is this one.
    pub support: usize,
    /// Predicted this intent and was right.
    pub true_positive: usize,
    /// Predicted this intent when the truth was something else.
    pub false_positive: usize,
    /// Truth was this intent but something else was predicted.
    pub false_negative: usize,
}

impl IntentStat {
    pub fn precision(&self) -> f64 {
        let p = self.true_positive + self.false_positive;
        if p == 0 {
            return 1.0;
        }
        self.true_positive as f64 / p as f64
    }
    pub fn recall(&self) -> f64 {
        let t = self.true_positive + self.false_negative;
        if t == 0 {
            return 1.0;
        }
        self.true_positive as f64 / t as f64
    }
    pub fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 {
            return 0.0;
        }
        2.0 * p * r / (p + r)
    }
}

/// One `expected → actual` confusion and how often it happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Confusion {
    pub expected: String,
    pub actual: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingMetrics {
    pub total: usize,
    pub correct: usize,
    /// Keyed by `coarse_intent` (`"EMBED_ROUTER"`, `"LOOKUP"`, …);
    /// rows with no coarse label land under `"unattributed"`.
    pub layers: BTreeMap<String, LayerStat>,
    /// Keyed by intent label. Computed only over rows with a single
    /// expected intent — see `permissive_rows`.
    pub per_intent: BTreeMap<String, IntentStat>,
    /// Rows whose expected intent was a permissive set (`"a|b"`, from
    /// `ExpectedIntent::AnyOf`). Counted in `total`/`correct` but
    /// excluded from `per_intent`, because "either is fine" has no
    /// well-defined precision.
    pub permissive_rows: usize,
    pub confusions: Vec<Confusion>,
}

/// The layer label used when a decision carries no `coarse_intent`.
pub const UNATTRIBUTED: &str = "unattributed";
/// The layer label the router stamps when the embed classifier owned
/// the decision (see `router.rs` pre-check -1).
pub const EMBED_LAYER: &str = "EMBED_ROUTER";

impl RoutingMetrics {
    pub fn from_results(results: &[RoutingResult]) -> Self {
        let mut m = RoutingMetrics {
            total: results.len(),
            ..Default::default()
        };

        let mut latency_sums: BTreeMap<String, u64> = BTreeMap::new();
        let mut confusion_counts: BTreeMap<(String, String), usize> = BTreeMap::new();

        for r in results {
            if r.correct {
                m.correct += 1;
            }

            let layer = r
                .coarse_intent
                .clone()
                .unwrap_or_else(|| UNATTRIBUTED.to_string());
            let e = m.layers.entry(layer.clone()).or_default();
            e.decided += 1;
            if r.correct {
                e.correct += 1;
            }
            *latency_sums.entry(layer).or_default() += r.latency_ms;

            // Permissive expectations ("knowledge_query|deep_query")
            // are scoreable but not attributable to one class.
            if r.expected.contains('|') {
                m.permissive_rows += 1;
            } else {
                let truth = r.expected.clone();
                let pred = r.actual_intent.clone();
                m.per_intent.entry(truth.clone()).or_default().support += 1;
                if truth == pred {
                    m.per_intent.entry(truth).or_default().true_positive += 1;
                } else {
                    m.per_intent.entry(truth.clone()).or_default().false_negative += 1;
                    m.per_intent.entry(pred.clone()).or_default().false_positive += 1;
                    *confusion_counts.entry((truth, pred)).or_default() += 1;
                }
            }
        }

        for (layer, sum) in latency_sums {
            if let Some(stat) = m.layers.get_mut(&layer) {
                if stat.decided > 0 {
                    stat.mean_latency_ms = sum / stat.decided as u64;
                }
            }
        }

        m.confusions = confusion_counts
            .into_iter()
            .map(|((expected, actual), count)| Confusion {
                expected,
                actual,
                count,
            })
            .collect();
        // Most frequent confusion first — that is the one to fix.
        m.confusions.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.expected.cmp(&b.expected))
        });
        m
    }

    pub fn accuracy(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.correct as f64 / self.total as f64
    }

    /// Fraction of decisions the EMBED router owned — the share that
    /// never woke the LLM classifier.
    pub fn embed_coverage(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.layers
            .get(EMBED_LAYER)
            .map(|s| s.decided as f64 / self.total as f64)
            .unwrap_or(0.0)
    }

    /// Mean latency over every row, whichever layer decided it.
    pub fn mean_latency_ms(&self) -> u64 {
        let decided: usize = self.layers.values().map(|s| s.decided).sum();
        if decided == 0 {
            return 0;
        }
        let total: u64 = self
            .layers
            .values()
            .map(|s| s.mean_latency_ms * s.decided as u64)
            .sum();
        total / decided as u64
    }

    /// One-line summary for the bench rollup.
    pub fn headline(&self) -> String {
        format!(
            "routing {}/{} · embed-layer {:.0}% · mean {}ms",
            self.correct,
            self.total,
            self.embed_coverage() * 100.0,
            self.mean_latency_ms()
        )
    }

    /// Multi-line glassbox block for `--human` output.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "  accuracy       {}/{} ({:.1}%)\n",
            self.correct,
            self.total,
            self.accuracy() * 100.0
        ));
        out.push_str("  layer attribution (who decided, and how fast)\n");
        for (layer, s) in &self.layers {
            out.push_str(&format!(
                "    {:<16} {:>3}/{:<3} correct · {:>5}ms mean\n",
                layer, s.correct, s.decided, s.mean_latency_ms
            ));
        }
        out.push_str(&format!(
            "    embed-layer coverage {:.1}%\n",
            self.embed_coverage() * 100.0
        ));

        if !self.per_intent.is_empty() {
            out.push_str("  per-intent (support · precision · recall · F1)\n");
            for (intent, s) in &self.per_intent {
                out.push_str(&format!(
                    "    {:<20} {:>3} · {:>5.1}% · {:>5.1}% · {:.2}\n",
                    intent,
                    s.support,
                    s.precision() * 100.0,
                    s.recall() * 100.0,
                    s.f1()
                ));
            }
            if self.permissive_rows > 0 {
                out.push_str(&format!(
                    "    ({} row(s) with permissive expectations excluded)\n",
                    self.permissive_rows
                ));
            }
        }

        if !self.confusions.is_empty() {
            out.push_str("  confusions (expected → actual)\n");
            for c in &self.confusions {
                out.push_str(&format!(
                    "    {:<20} → {:<20} x{}\n",
                    c.expected, c.actual, c.count
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: &str,
        expected: &str,
        actual: &str,
        coarse: Option<&str>,
        latency_ms: u64,
    ) -> RoutingResult {
        RoutingResult {
            question_id: id.into(),
            category: "test".into(),
            question: "q".into(),
            expected: expected.into(),
            actual_intent: actual.into(),
            coarse_intent: coarse.map(String::from),
            confidence: 1.0,
            rationale: None,
            correct: expected.split('|').any(|e| e == actual),
            latency_ms,
        }
    }

    /// The shape of a real 2026-07-16 baseline: perfect accuracy, but
    /// most decisions bought with an LLM call. Accuracy cannot see the
    /// difference; coverage and mean latency can.
    fn mixed_bank() -> Vec<RoutingResult> {
        vec![
            row("a", "knowledge_query", "knowledge_query", Some(EMBED_LAYER), 50),
            row("b", "knowledge_query", "knowledge_query", Some(EMBED_LAYER), 60),
            row("c", "deep_query", "deep_query", Some("REASONING"), 1800),
            row("d", "deep_query", "deep_query", Some("REASONING"), 2200),
            row("e", "comparison_query", "comparison_query", Some("COMPARISON"), 1500),
        ]
    }

    #[test]
    fn accuracy_hides_what_coverage_reveals() {
        let m = RoutingMetrics::from_results(&mixed_bank());
        assert_eq!(m.correct, 5);
        assert!((m.accuracy() - 1.0).abs() < 1e-9, "a perfect bank");
        // ...but only 2 of 5 were owned by the embed layer.
        assert!((m.embed_coverage() - 0.4).abs() < 1e-9);
        assert_eq!(m.layers[EMBED_LAYER].decided, 2);
        assert_eq!(m.layers[EMBED_LAYER].mean_latency_ms, 55);
        assert_eq!(m.layers["REASONING"].mean_latency_ms, 2000);
    }

    #[test]
    fn mean_latency_is_weighted_by_decisions() {
        let m = RoutingMetrics::from_results(&mixed_bank());
        // (50+60+1800+2200+1500)/5 = 1122
        assert_eq!(m.mean_latency_ms(), 1122);
    }

    #[test]
    fn per_intent_separates_a_weak_class_from_a_strong_one() {
        let rows = vec![
            row("a", "knowledge_query", "knowledge_query", None, 1),
            row("b", "knowledge_query", "knowledge_query", None, 1),
            row("c", "knowledge_query", "knowledge_query", None, 1),
            // complex_task is the sparse class and it misses both times.
            row("d", "complex_task", "knowledge_query", None, 1),
            row("e", "complex_task", "deep_query", None, 1),
        ];
        let m = RoutingMetrics::from_results(&rows);
        assert_eq!(m.correct, 3);

        let k = &m.per_intent["knowledge_query"];
        assert_eq!(k.support, 3);
        assert_eq!(k.true_positive, 3);
        assert_eq!(k.false_positive, 1, "stole one complex_task row");
        assert!((k.recall() - 1.0).abs() < 1e-9);
        assert!((k.precision() - 0.75).abs() < 1e-9);

        let c = &m.per_intent["complex_task"];
        assert_eq!(c.support, 2);
        assert_eq!(c.true_positive, 0);
        assert_eq!(c.false_negative, 2);
        assert!((c.recall() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn confusions_are_ranked_most_frequent_first() {
        let rows = vec![
            row("a", "expressive_query", "knowledge_query", None, 1),
            row("b", "expressive_query", "knowledge_query", None, 1),
            row("c", "expressive_query", "knowledge_query", None, 1),
            row("d", "code_query", "knowledge_query", None, 1),
        ];
        let m = RoutingMetrics::from_results(&rows);
        assert_eq!(m.confusions.len(), 2);
        assert_eq!(m.confusions[0].expected, "expressive_query");
        assert_eq!(m.confusions[0].actual, "knowledge_query");
        assert_eq!(m.confusions[0].count, 3);
        assert_eq!(m.confusions[1].count, 1);
    }

    /// `ExpectedIntent::AnyOf` rows ("either route is defensible") are
    /// scoreable but have no well-defined precision, so they must not
    /// silently pollute per-intent stats.
    #[test]
    fn permissive_rows_are_scored_but_excluded_from_per_intent() {
        let rows = vec![
            row("a", "knowledge_query|deep_query", "deep_query", None, 1),
            row("b", "knowledge_query", "knowledge_query", None, 1),
        ];
        let m = RoutingMetrics::from_results(&rows);
        assert_eq!(m.total, 2);
        assert_eq!(m.correct, 2, "the permissive row still counts as correct");
        assert_eq!(m.permissive_rows, 1);
        assert_eq!(m.per_intent.len(), 1, "only the unambiguous row");
        assert!(m.per_intent.contains_key("knowledge_query"));
    }

    #[test]
    fn rows_without_a_coarse_label_land_under_unattributed() {
        let m = RoutingMetrics::from_results(&[row("a", "simple_query", "simple_query", None, 5)]);
        assert_eq!(m.layers[UNATTRIBUTED].decided, 1);
        assert!((m.embed_coverage() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn empty_results_do_not_divide_by_zero() {
        let m = RoutingMetrics::from_results(&[]);
        assert_eq!(m.total, 0);
        assert!((m.accuracy() - 0.0).abs() < 1e-9);
        assert!((m.embed_coverage() - 0.0).abs() < 1e-9);
        assert_eq!(m.mean_latency_ms(), 0);
    }

    #[test]
    fn headline_names_the_three_numbers_that_matter() {
        let h = RoutingMetrics::from_results(&mixed_bank()).headline();
        assert!(h.contains("5/5"), "got: {h}");
        assert!(h.contains("embed-layer 40%"), "got: {h}");
        assert!(h.contains("1122ms"), "got: {h}");
    }
}
