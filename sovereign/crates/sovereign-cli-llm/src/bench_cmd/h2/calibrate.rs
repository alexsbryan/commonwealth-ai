// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h2-calibrate` — fit the clustering floor and commit its
//! curve.
//!
//! Deliverable 2's second half. §7.1: *"a threshold with no committed curve
//! fails review"*, and principle 2: every score ships with its curve. This is
//! the command that produces the one the clusterer reads.
//!
//! The signal is `min(margin(a→b), margin(b→a))` — the bidirectional-entailment
//! rule expressed as a single number, so a floor on the curve IS the floor
//! `cluster_values` applies. If the two were computed differently the curve
//! would describe a decision nobody makes (principle 8).
//!
//! `--from-scores` replays a frozen score file with no model loaded, so the
//! curve and the chosen floor are reproducible on a machine with no reranker.

use std::path::PathBuf;

use sovereign_core::runtime::native_grounding::meaning_cluster::entailment_query;
use sovereign_core::runtime::native_grounding::sentence_sweep::SentenceScorer;
use sovereign_eval::flywheel::operating_curve;

use super::pairs::{self, EquivalenceSet, ValuePair};

/// One pair after both directions were scored.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PairScore {
    pub id: String,
    pub same: bool,
    pub margin_ab: f32,
    pub margin_ba: f32,
    /// `min(ab, ba)` — the number the floor is compared against.
    pub signal: f32,
    pub det_settled: bool,
    pub value_a: String,
    pub value_b: String,
}

/// The committed calibration artifact.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FloorCalibration {
    /// The chosen floor: the curve's best-balanced-accuracy threshold. By
    /// rule, not by hand — the same selection rule H4's floor uses, so the two
    /// calibrations cannot differ in their taste.
    pub floor: f64,
    pub curve: operating_curve::OperatingCurve,
    pub n_same: usize,
    pub n_different: usize,
    pub n_same_nontrivial: usize,
    /// AUROC restricted to the positives rung (a) does NOT already settle. The
    /// headline AUROC is inflated by trivial positives; this is the number that
    /// says whether rung (b) earns its reranker calls.
    pub auroc_nontrivial_positives: Option<f64>,
    pub provenance: String,
    /// What this calibration is NOT. Travels with the artifact.
    pub caveat: String,
}

const CAVEAT: &str = "\
EQUIVALENCE-CALIBRATED, NOT OUTCOME-CALIBRATED. This floor was fitted to \
separate `same answer` from `different answer`, because the hallucination label \
is zero-positive on every frozen artifact (0 of 91 gated-eligible turns) and no \
outcome-calibrated floor can be fitted from existing data. A clusterer using \
this floor makes correct merge decisions to the extent this curve says; whether \
the resulting entropy predicts hallucination is UNMEASURED and is exactly what \
the H2 gate could not judge. Re-calibration against outcomes is expected as \
soon as a dev bank supplies fabrication positives.";

pub async fn cmd_h2_calibrate(args: &[String]) -> i32 {
    let mut sources: Vec<PathBuf> = Vec::new();
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h2");
    let mut rerank_model: Option<PathBuf> = None;
    let mut from_scores: Option<PathBuf> = None;
    let mut max_different = 900usize;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                if let Some(v) = args.get(i + 1) {
                    sources.push(PathBuf::from(v));
                    i += 1;
                }
            }
            "--out-dir" => {
                if let Some(v) = args.get(i + 1) {
                    out_dir = PathBuf::from(v);
                    i += 1;
                }
            }
            "--rerank-model" => {
                if let Some(v) = args.get(i + 1) {
                    rerank_model = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--from-scores" => {
                if let Some(v) = args.get(i + 1) {
                    from_scores = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--max-different" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                    max_different = v;
                    i += 1;
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "svrn bench flywheel h2-calibrate --source <scored.jsonl> [--source ...] \
                     [--rerank-model <gguf> | --from-scores <jsonl>] [--out-dir <dir>] \
                     [--max-different N]"
                );
                return 0;
            }
            other => {
                eprintln!("error: unknown h2-calibrate flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    if sources.is_empty() {
        eprintln!(
            "error: --source is required (a scored chaos `*.jsonl`, repeatable). The \
             equivalence labels come from `asserted_value` on those rows."
        );
        return 2;
    }

    let set = match pairs::build(&sources, max_different) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    eprintln!(
        "[h2] equivalence set: {} pairs — {} same ({} non-trivial) / {} different",
        set.pairs.len(),
        set.n_same,
        set.n_same_nontrivial(),
        set.n_different
    );

    let scores = match from_scores {
        Some(p) => match replay_scores(&p) {
            Ok(s) => {
                eprintln!("[h2] replayed {} pair scores — no model loaded", s.len());
                s
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        },
        None => {
            let path = match super::super::h4::scorer::resolve_rerank_path(rerank_model) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            let scorer = match super::super::h4::scorer::load(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            match score_all(&set, &scorer).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            }
        }
    };

    let calib = match fit(&set, &scores) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: create {}: {e}", out_dir.display());
        return 1;
    }
    let scores_path = out_dir.join("h2_pair_scores.jsonl");
    let mut buf = String::new();
    for s in &scores {
        buf.push_str(&serde_json::to_string(s).unwrap_or_default());
        buf.push('\n');
    }
    if let Err(e) = std::fs::write(&scores_path, buf) {
        eprintln!("error: write {}: {e}", scores_path.display());
        return 1;
    }
    let curve_path = out_dir.join("h2_cluster_floor.curve.json");
    let json = match serde_json::to_string_pretty(&calib) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: serialize calibration: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&curve_path, format!("{json}\n")) {
        eprintln!("error: write {}: {e}", curve_path.display());
        return 1;
    }

    println!("H2 clustering floor — calibrated on value equivalence");
    println!("  pairs                 {}", calib.curve.n_pairs);
    println!(
        "  same / different      {} / {}",
        calib.n_same, calib.n_different
    );
    println!("  non-trivial positives {}", calib.n_same_nontrivial);
    println!("  AUROC                 {:.4}", calib.curve.auroc);
    match calib.auroc_nontrivial_positives {
        Some(a) => println!("  AUROC (non-trivial +) {a:.4}"),
        None => println!("  AUROC (non-trivial +) could-not-judge — no non-trivial positives"),
    }
    println!("  best BAcc             {:.4}", calib.curve.best_balanced_accuracy);
    println!("  FLOOR                 {:.4}", calib.floor);
    println!("  curve                 {}", curve_path.display());
    println!("  scores                {}", scores_path.display());
    println!("\n{}", calib.caveat);
    0
}

fn replay_scores(path: &std::path::Path) -> Result<Vec<PairScore>, String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (n, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str::<PairScore>(line)
                .map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?,
        );
    }
    if out.is_empty() {
        return Err(format!("{} carried no scores", path.display()));
    }
    Ok(out)
}

async fn score_all(
    set: &EquivalenceSet,
    scorer: &dyn SentenceScorer,
) -> Result<Vec<PairScore>, String> {
    let mut out = Vec::with_capacity(set.pairs.len());
    let total = set.pairs.len();
    for (n, p) in set.pairs.iter().enumerate() {
        if n % 100 == 0 {
            eprintln!("[h2] scoring pair {n}/{total}");
        }
        out.push(score_one(p, scorer).await?);
    }
    Ok(out)
}

/// Score one pair in both directions. Extracted so the fit is testable against
/// a fake scorer with no model.
async fn score_one(p: &ValuePair, scorer: &dyn SentenceScorer) -> Result<PairScore, String> {
    let ab = *scorer
        .score(&entailment_query(&p.value_a), std::slice::from_ref(&p.value_b))
        .await?
        .first()
        .ok_or_else(|| format!("pair {}: scorer returned nothing for a->b", p.id))?;
    let ba = *scorer
        .score(&entailment_query(&p.value_b), std::slice::from_ref(&p.value_a))
        .await?
        .first()
        .ok_or_else(|| format!("pair {}: scorer returned nothing for b->a", p.id))?;
    Ok(PairScore {
        id: p.id.clone(),
        same: p.same,
        margin_ab: ab,
        margin_ba: ba,
        // THE signal, and the only place it is defined: the bidirectional rule
        // as one number. `cluster_values` merges iff both margins clear the
        // floor, which is exactly `min(ab, ba) >= floor`.
        signal: ab.min(ba),
        det_settled: p.det_settled,
        value_a: p.value_a.clone(),
        value_b: p.value_b.clone(),
    })
}

/// Build the curve and pick the floor.
pub fn fit(set: &EquivalenceSet, scores: &[PairScore]) -> Result<FloorCalibration, String> {
    let to_pairs = |rows: &[&PairScore]| -> Vec<operating_curve::ScoredPair> {
        rows.iter()
            .map(|s| operating_curve::ScoredPair {
                id: s.id.clone(),
                corpus_id: "h2-equivalence".to_string(),
                // The curve's `answerable` axis is "the positive class". Here
                // that is `same`, and `honesty_recall` reads as "different
                // pairs correctly NOT merged", `false_alarm` as "same pairs
                // wrongly split". Reusing the one curve builder rather than
                // writing a second one (principle 8); the vocabulary is H1's
                // and the mapping is stated here and in the artifact.
                answerable: s.same,
                score: s.signal,
            })
            .collect()
    };

    let all: Vec<&PairScore> = scores.iter().collect();
    let curve = operating_curve::build("h2_cluster_min_margin", &to_pairs(&all))?;

    // The same curve over the positives rung (a) does not already settle, kept
    // separate because the headline number is inflated by trivial positives.
    let nontrivial: Vec<&PairScore> = scores
        .iter()
        .filter(|s| !(s.same && s.det_settled))
        .collect();
    let auroc_nontrivial_positives =
        operating_curve::build("h2_cluster_min_margin_nontrivial", &to_pairs(&nontrivial))
            .ok()
            .map(|c| c.auroc);

    Ok(FloorCalibration {
        floor: curve.best_balanced_accuracy_threshold,
        n_same: set.n_same,
        n_different: set.n_different,
        n_same_nontrivial: set.n_same_nontrivial(),
        auroc_nontrivial_positives,
        provenance: set.provenance.clone(),
        caveat: CAVEAT.to_string(),
        curve,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    #[async_trait::async_trait]
    impl SentenceScorer for Fake {
        async fn score(&self, query: &str, docs: &[String]) -> Result<Vec<f32>, String> {
            // A perfectly informative scorer: high when the two strings share
            // their first word, low otherwise.
            let from = query
                .strip_prefix("Does this answer mean the same as: ")
                .unwrap_or(query);
            Ok(docs
                .iter()
                .map(|to| {
                    let f = from.split_whitespace().next().unwrap_or("");
                    let t = to.split_whitespace().next().unwrap_or("");
                    if f.eq_ignore_ascii_case(t) {
                        5.0
                    } else {
                        -5.0
                    }
                })
                .collect())
        }
    }

    fn pair(id: &str, a: &str, b: &str, same: bool) -> ValuePair {
        ValuePair {
            id: id.into(),
            value_a: a.into(),
            value_b: b.into(),
            same,
            probe_a: "pa".into(),
            probe_b: if same { "pa".into() } else { "pb".into() },
            source_a: "s".into(),
            source_b: "s".into(),
            det_settled: false,
        }
    }

    #[tokio::test]
    async fn the_signal_is_the_min_of_the_two_directions() {
        // The floor must be comparable against the SAME number
        // `cluster_values` compares — `min(ab, ba)`, because it merges only
        // when BOTH clear. Any other reduction (mean, max) would put the curve
        // and the clusterer on different decisions.
        struct Asym;
        #[async_trait::async_trait]
        impl SentenceScorer for Asym {
            async fn score(&self, query: &str, _d: &[String]) -> Result<Vec<f32>, String> {
                Ok(vec![if query.contains("alpha") { 9.0 } else { -3.0 }])
            }
        }
        let s = score_one(&pair("p", "alpha thing", "beta thing", true), &Asym)
            .await
            .unwrap();
        assert_eq!(s.margin_ab, 9.0);
        assert_eq!(s.margin_ba, -3.0);
        assert_eq!(s.signal, -3.0, "min, so one-way entailment cannot clear a floor");
    }

    #[tokio::test]
    async fn a_perfect_scorer_yields_a_perfect_curve_and_a_separating_floor() {
        // Instrument validation before the result (§18.4): if the fit cannot
        // recover a floor from a scorer that separates perfectly, no number it
        // produces on real margins means anything.
        let ps = vec![
            pair("s1", "alpha one", "alpha two", true),
            pair("s2", "gamma one", "gamma two", true),
            pair("d1", "alpha one", "beta two", false),
            pair("d2", "gamma one", "delta two", false),
        ];
        let mut scores = Vec::new();
        for p in &ps {
            scores.push(score_one(p, &Fake).await.unwrap());
        }
        let set = EquivalenceSet {
            pairs: ps,
            provenance: "t".into(),
            n_same: 2,
            n_different: 2,
            n_same_det_settled: 0,
            values_per_source: Default::default(),
        };
        let c = fit(&set, &scores).unwrap();
        assert_eq!(c.curve.auroc, 1.0);
        assert!(
            c.floor > -5.0 && c.floor <= 5.0,
            "the floor must land between the two classes, got {}",
            c.floor
        );
        assert_eq!(c.curve.best_balanced_accuracy, 1.0);
    }

    #[tokio::test]
    async fn a_constant_scorer_scores_half_not_one() {
        // The negative control. A scorer that returns one number for
        // everything must not look perfect — the curve's tie-corrected AUROC
        // is what guarantees that, and this is the input that proves it here
        // rather than in the abstract.
        struct Constant;
        #[async_trait::async_trait]
        impl SentenceScorer for Constant {
            async fn score(&self, _q: &str, d: &[String]) -> Result<Vec<f32>, String> {
                Ok(vec![1.0; d.len()])
            }
        }
        let ps = vec![
            pair("s1", "a b", "a c", true),
            pair("d1", "a b", "z c", false),
        ];
        let mut scores = Vec::new();
        for p in &ps {
            scores.push(score_one(p, &Constant).await.unwrap());
        }
        let set = EquivalenceSet {
            pairs: ps,
            provenance: "t".into(),
            n_same: 1,
            n_different: 1,
            n_same_det_settled: 0,
            values_per_source: Default::default(),
        };
        let c = fit(&set, &scores).unwrap();
        assert_eq!(c.curve.auroc, 0.5, "a constant signal is a coin flip");
    }

    #[tokio::test]
    async fn the_nontrivial_auroc_excludes_positives_rung_a_settles() {
        let mut ps = vec![
            pair("s-trivial", "alpha one", "alpha one", true),
            pair("s-hard", "gamma one", "gamma two", true),
            pair("d1", "alpha one", "beta two", false),
        ];
        ps[0].det_settled = true;
        let mut scores = Vec::new();
        for p in &ps {
            scores.push(score_one(p, &Fake).await.unwrap());
        }
        let set = EquivalenceSet {
            pairs: ps,
            provenance: "t".into(),
            n_same: 2,
            n_different: 1,
            n_same_det_settled: 1,
            values_per_source: Default::default(),
        };
        let c = fit(&set, &scores).unwrap();
        assert_eq!(c.n_same_nontrivial, 1);
        assert!(
            c.auroc_nontrivial_positives.is_some(),
            "with one non-trivial positive and one negative the restricted \
             curve is computable and must be reported"
        );
    }

    #[tokio::test]
    async fn a_set_with_no_nontrivial_positives_reports_that_rather_than_a_number() {
        // §18.3: absence is reported, never defaulted. A restricted curve that
        // cannot be built must come back None, not 0.5.
        let mut ps = vec![
            pair("s-trivial", "alpha one", "alpha one", true),
            pair("d1", "alpha one", "beta two", false),
        ];
        ps[0].det_settled = true;
        let mut scores = Vec::new();
        for p in &ps {
            scores.push(score_one(p, &Fake).await.unwrap());
        }
        let set = EquivalenceSet {
            pairs: ps,
            provenance: "t".into(),
            n_same: 1,
            n_different: 1,
            n_same_det_settled: 1,
            values_per_source: Default::default(),
        };
        let c = fit(&set, &scores).unwrap();
        assert_eq!(c.n_same_nontrivial, 0);
        assert_eq!(
            c.auroc_nontrivial_positives, None,
            "single-class restricted set must report could-not-judge"
        );
    }
}
