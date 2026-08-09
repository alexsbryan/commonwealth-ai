// SPDX-License-Identifier: AGPL-3.0-or-later
//! The value-equivalence calibration set — H2's clustering floor needs a
//! labelled set, and this builds one from frozen artifacts only.
//!
//! # Why equivalence and not outcome
//!
//! §7.3 H2 calibrates against the hallucination label. **That label is
//! zero-positive on every frozen artifact this repo has** — 0 positives over
//! 91 gated-eligible turns across the two sources the order names (the census
//! is in `sovereign/bench/calibration/h2/FINDINGS.md`). A floor cannot be
//! fitted against a constant.
//!
//! So the floor is calibrated against the thing it actually decides: **are
//! these two values the same answer?** That is a well-posed question with a
//! two-class label available from frozen data, and it is the question rung (b)
//! is asked at runtime. It is NOT the outcome the gate is about, and the
//! artifact says so — see [`EquivalenceSet::provenance`] and the FINDINGS
//! section this docs into. Re-calibration against outcomes is expected the
//! moment a bank supplies positives.
//!
//! # Where the labels come from, and what they assume
//!
//! Every pair is built from `asserted_value` on frozen chaos `*.jsonl` rows,
//! restricted to rows the chaos scorer already marked `answer_correct == true`
//! AND `asserted_value_grounded == true`. On that restriction:
//!
//! | Label | Construction | The assumption |
//! |---|---|---|
//! | **same** | two rows with the SAME probe id, from different runs | two answers that are both *correct* and both *grounded* for one question assert the same value |
//! | **different** | two rows with DIFFERENT probe ids | two correct answers to two different questions assert different values |
//!
//! Both assumptions are stated rather than hidden because both can be
//! strained. The negative one is strained when two probes share an answer
//! (rare in these banks, and [`build`] drops any negative pair whose values are
//! deterministically equivalent rather than mislabelling it). The positive one
//! is strained when a probe admits two true descriptions at different
//! granularity — `prov-crown` yields *"a knurled brass button"* and *"the
//! winding-crown"*, which are the same object described two ways. That pair is
//! kept: it is exactly the paraphrase rung (b) exists for, and dropping the
//! hard positives would fit a floor to the easy ones.
//!
//! # The trivial/non-trivial split matters and is reported
//!
//! Most same-probe pairs are byte-identical, because the pipeline is
//! effectively deterministic at temperature 0 (measured: 36/37 and 20/20
//! byte-identical answers across two harvests, `h4/FINDINGS.md`). Those are
//! settled by rung (a) for free and teach rung (b) nothing. [`EquivalenceSet`]
//! counts them separately so a floor fitted mostly on trivial positives cannot
//! be read as a measurement of paraphrase detection.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sovereign_core::runtime::native_grounding::meaning_cluster::det_equivalent;

/// One scored chaos row, as the pair builder needs it.
#[derive(Debug, Clone, Deserialize)]
struct ScoredRow {
    id: String,
    #[serde(default)]
    qtype: String,
    #[serde(default)]
    answer_correct: Option<bool>,
    #[serde(default)]
    asserted_value: Option<String>,
    #[serde(default)]
    asserted_value_grounded: Option<bool>,
}

/// One labelled value pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValuePair {
    /// Stable id: `<probe_a>|<probe_b>|<n>`, so a rebuilt set names its pairs
    /// the same way and a scores file can be replayed.
    pub id: String,
    pub value_a: String,
    pub value_b: String,
    /// The label: do these assert the same answer?
    pub same: bool,
    /// Probe the left value came from.
    pub probe_a: String,
    /// Probe the right value came from.
    pub probe_b: String,
    /// Artifact the left value came from.
    pub source_a: String,
    /// Artifact the right value came from.
    pub source_b: String,
    /// True when rung (a) — the deterministic kernel — already settles this
    /// pair. Reported, never dropped: a floor fitted only on pairs rung (a)
    /// cannot settle would be fitted on a biased slice, and a floor fitted
    /// only on pairs it CAN settle would measure nothing.
    pub det_settled: bool,
}

/// The built set plus everything needed to read it honestly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquivalenceSet {
    pub pairs: Vec<ValuePair>,
    /// One paragraph naming what the labels are and what they assume. Written
    /// into the committed artifact so the curve can never be quoted without it.
    pub provenance: String,
    pub n_same: usize,
    pub n_different: usize,
    /// Same-label pairs rung (a) already settles (byte-identical / case /
    /// honorific). Non-trivial positives are `n_same - n_same_det_settled`.
    pub n_same_det_settled: usize,
    /// Values found per source artifact, before pairing.
    pub values_per_source: BTreeMap<String, usize>,
}

impl EquivalenceSet {
    /// Positives rung (a) does NOT settle — the ones that actually exercise the
    /// reranker floor. A curve whose positive class is mostly trivial is
    /// reported as such rather than quoted as paraphrase performance.
    pub fn n_same_nontrivial(&self) -> usize {
        self.n_same - self.n_same_det_settled
    }
}

/// Load the `asserted_value`s from one scored chaos `*.jsonl`.
///
/// Malformed lines are counted and skipped, never swallowed — same contract as
/// `h4::transcript::load`.
fn load_values(path: &Path) -> Result<(Vec<(String, String, String)>, usize), String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let source = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(r) = serde_json::from_str::<ScoredRow>(line) else {
            skipped += 1;
            continue;
        };
        // The restriction that makes the labels defensible. A row that was
        // wrong, or whose value was not grounded, tells us nothing about what
        // the right value IS.
        if r.answer_correct != Some(true) || r.asserted_value_grounded != Some(true) {
            continue;
        }
        let Some(v) = r.asserted_value.as_ref().map(|s| s.trim()) else {
            continue;
        };
        if v.is_empty() {
            continue;
        }
        let _ = &r.qtype;
        out.push((r.id.clone(), v.to_string(), source.clone()));
    }
    Ok((out, skipped))
}

/// Build the equivalence set from a list of frozen scored artifacts.
///
/// `max_different` caps the negative class (they are quadratic in the probe
/// count and would otherwise swamp the positives); negatives are taken in
/// deterministic id order, never sampled, so two builds over the same inputs
/// are byte-identical.
pub fn build(paths: &[std::path::PathBuf], max_different: usize) -> Result<EquivalenceSet, String> {
    let mut by_probe: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut values_per_source: BTreeMap<String, usize> = BTreeMap::new();
    for p in paths {
        let (vals, skipped) = load_values(p)?;
        if skipped > 0 {
            eprintln!("[h2] {}: {skipped} unreadable line(s) skipped", p.display());
        }
        for (probe, value, source) in vals {
            *values_per_source.entry(source.clone()).or_default() += 1;
            by_probe.entry(probe).or_default().push((value, source));
        }
    }
    if by_probe.is_empty() {
        return Err(
            "no correct+grounded asserted values in the given artifacts — nothing to \
             calibrate equivalence on"
                .to_string(),
        );
    }

    let mut pairs: Vec<ValuePair> = Vec::new();

    // ── Positives: same probe, different rows ───────────────────────
    for (probe, vals) in &by_probe {
        for i in 0..vals.len() {
            for j in (i + 1)..vals.len() {
                let (va, sa) = &vals[i];
                let (vb, sb) = &vals[j];
                pairs.push(ValuePair {
                    id: format!("{probe}|{probe}|{}-{}", i, j),
                    det_settled: det_equivalent(va, vb),
                    value_a: va.clone(),
                    value_b: vb.clone(),
                    same: true,
                    probe_a: probe.clone(),
                    probe_b: probe.clone(),
                    source_a: sa.clone(),
                    source_b: sb.clone(),
                });
            }
        }
    }

    // ── Negatives: distinct probes, one representative each ─────────
    //
    // One representative per probe (the first value in id order) so the
    // negative class is not dominated by whichever probe happened to appear in
    // the most harvests.
    let reps: Vec<(&String, &(String, String))> = by_probe
        .iter()
        .filter_map(|(p, v)| v.first().map(|f| (p, f)))
        .collect();
    let mut n_diff = 0usize;
    'outer: for i in 0..reps.len() {
        for j in (i + 1)..reps.len() {
            if n_diff >= max_different {
                break 'outer;
            }
            let (pa, (va, sa)) = reps[i];
            let (pb, (vb, sb)) = reps[j];
            // A negative whose two values are deterministically equivalent is
            // not a negative — it is two probes that share an answer. Dropped
            // rather than mislabelled (§18.3: never silently substitute).
            if det_equivalent(va, vb) {
                continue;
            }
            pairs.push(ValuePair {
                id: format!("{pa}|{pb}|x"),
                value_a: va.clone(),
                value_b: vb.clone(),
                same: false,
                probe_a: pa.clone(),
                probe_b: pb.clone(),
                source_a: sa.clone(),
                source_b: sb.clone(),
                det_settled: false,
            });
            n_diff += 1;
        }
    }

    pairs.sort_by(|a, b| a.id.cmp(&b.id));
    let n_same = pairs.iter().filter(|p| p.same).count();
    let n_same_det_settled = pairs.iter().filter(|p| p.same && p.det_settled).count();
    let n_different = pairs.len() - n_same;

    if n_same == 0 || n_different == 0 {
        return Err(format!(
            "equivalence set is single-class ({n_same} same / {n_different} different) — \
             refusing to fit a floor on it"
        ));
    }

    Ok(EquivalenceSet {
        provenance: PROVENANCE.to_string(),
        pairs,
        n_same,
        n_different,
        n_same_det_settled,
        values_per_source,
    })
}

/// Travels with every committed artifact built from this set.
pub const PROVENANCE: &str = "\
Value-equivalence labels, NOT hallucination outcomes. Positives are pairs of \
`asserted_value`s from the SAME probe across frozen chaos runs, restricted to \
rows the chaos scorer marked answer_correct=true AND asserted_value_grounded=\
true; the assumption is that two correct, grounded answers to one question \
assert the same value. Negatives are values from DIFFERENT probes under the \
same restriction, dropping any pair the deterministic kernel finds equivalent. \
This floor is therefore calibrated to decide `are these the same answer?` — the \
question rung (b) is actually asked — and NOT to predict hallucination. It is \
built this way because the hallucination label is zero-positive on every frozen \
artifact (0 of 91 gated-eligible turns; see h2/FINDINGS.md), so no \
outcome-calibrated floor can be fitted from existing data. Re-calibrate against \
outcomes as soon as a bank supplies positives.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, rows: &[&str]) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        for r in rows {
            writeln!(f, "{r}").unwrap();
        }
        p
    }

    fn row(id: &str, correct: bool, grounded: bool, value: &str) -> String {
        format!(
            r#"{{"id":"{id}","qtype":"present","answer_correct":{correct},"asserted_value":"{value}","asserted_value_grounded":{grounded}}}"#
        )
    }

    #[test]
    fn positives_come_from_the_same_probe_and_negatives_from_different_ones() {
        let d = tempfile::tempdir().unwrap();
        let a = write(
            d.path(),
            "run_a.jsonl",
            &[
                &row("p-killer", true, true, "Severin Quenholt"),
                &row("p-place", true, true, "The Cold Lantern inn"),
            ],
        );
        let b = write(
            d.path(),
            "run_b.jsonl",
            &[
                &row("p-killer", true, true, "Quenholt the shipwright"),
                &row("p-place", true, true, "The Cold Lantern inn"),
            ],
        );
        let set = build(&[a, b], 100).unwrap();
        assert_eq!(set.n_same, 2, "one positive per probe across the two runs");
        assert!(set.n_different >= 1);
        let same: Vec<_> = set.pairs.iter().filter(|p| p.same).collect();
        assert!(same.iter().all(|p| p.probe_a == p.probe_b));
        let diff: Vec<_> = set.pairs.iter().filter(|p| !p.same).collect();
        assert!(diff.iter().all(|p| p.probe_a != p.probe_b));
    }

    #[test]
    fn a_wrong_or_ungrounded_row_contributes_no_label() {
        // The restriction is what makes the positive assumption defensible.
        // Watched to fail: drop it and this row would pair with the correct
        // one as a "same" label, teaching the floor that a wrong answer means
        // the same as a right one.
        let d = tempfile::tempdir().unwrap();
        let a = write(
            d.path(),
            "a.jsonl",
            &[
                &row("p1", true, true, "Severin Quenholt"),
                &row("p2", true, true, "the lock basin"),
            ],
        );
        let b = write(
            d.path(),
            "b.jsonl",
            &[
                &row("p1", false, true, "Lessa Pellow"),
                &row("p2", true, false, "somewhere else"),
            ],
        );
        // No positive survives the restriction, so the set is single-class
        // and the build REFUSES — the refusal names the counts, which is the
        // observable proof the restriction bit.
        let err = build(&[a, b], 100).expect_err(
            "with every candidate positive filtered out the set is single-class",
        );
        assert!(
            err.contains("0 same"),
            "the refusal must name the zero positive class: {err}"
        );
    }

    #[test]
    fn a_single_class_set_is_refused_not_fitted() {
        let d = tempfile::tempdir().unwrap();
        let a = write(
            d.path(),
            "a.jsonl",
            &[
                &row("p1", true, true, "Severin Quenholt"),
                &row("p2", true, true, "the lock basin"),
            ],
        );
        // One run only ⇒ no same-probe pairs ⇒ no positives.
        let err = build(&[a], 100).expect_err("a single-class set must refuse");
        assert!(err.contains("single-class"), "{err}");
    }

    #[test]
    fn a_negative_whose_values_are_det_equivalent_is_dropped_not_mislabelled() {
        // Two probes that happen to share an answer. Labelling that pair
        // "different" would teach the floor that identical values differ.
        let d = tempfile::tempdir().unwrap();
        let a = write(
            d.path(),
            "a.jsonl",
            &[
                &row("p1", true, true, "The Cold Lantern"),
                &row("p2", true, true, "the cold lantern"),
                &row("p3", true, true, "Severin Quenholt"),
            ],
        );
        let b = write(
            d.path(),
            "b.jsonl",
            &[&row("p1", true, true, "The Cold Lantern inn")],
        );
        let set = build(&[a, b], 100).unwrap();
        assert!(set.n_different > 0, "p3 supplies the surviving negatives");
        assert!(
            !set.pairs
                .iter()
                .any(|p| !p.same && det_equivalent(&p.value_a, &p.value_b)),
            "no negative may be deterministically equivalent"
        );
        // p1-vs-p2 is the pair that had to be dropped: same words, different
        // probes.
        assert!(
            !set.pairs
                .iter()
                .any(|p| !p.same && p.probe_a == "p1" && p.probe_b == "p2"),
            "the shared-answer pair must be dropped, not labelled `different`"
        );
    }

    #[test]
    fn the_trivial_positive_count_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let a = write(
            d.path(),
            "a.jsonl",
            &[
                &row("p1", true, true, "Severin Quenholt"),
                &row("p2", true, true, "the lock basin"),
            ],
        );
        let b = write(
            d.path(),
            "b.jsonl",
            &[
                // byte-identical up to case ⇒ rung (a) settles it
                &row("p1", true, true, "severin quenholt"),
                &row("p2", true, true, "the drowning pool"),
            ],
        );
        let set = build(&[a, b], 100).unwrap();
        assert_eq!(set.n_same, 2);
        assert_eq!(set.n_same_det_settled, 1, "p1 is trivial, p2 is not");
        assert_eq!(set.n_same_nontrivial(), 1);
    }

    #[test]
    fn the_build_is_byte_stable() {
        let d = tempfile::tempdir().unwrap();
        let a = write(
            d.path(),
            "a.jsonl",
            &[
                &row("p1", true, true, "alpha value"),
                &row("p2", true, true, "beta value"),
                &row("p3", true, true, "gamma value"),
            ],
        );
        let b = write(
            d.path(),
            "b.jsonl",
            &[&row("p1", true, true, "the alpha thing")],
        );
        let s1 = build(&[a.clone(), b.clone()], 100).unwrap();
        let s2 = build(&[a, b], 100).unwrap();
        assert_eq!(
            serde_json::to_string(&s1).unwrap(),
            serde_json::to_string(&s2).unwrap()
        );
    }

    #[test]
    fn the_negative_cap_bounds_the_class_deterministically() {
        let d = tempfile::tempdir().unwrap();
        // Distinct MULTI-CHARACTER words per probe: the presence kernel drops
        // tokens shorter than 2 chars, so "value number 0" and "value number 1"
        // are deterministically equivalent and would all be dropped.
        const W: [&str; 10] = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
            "hotel", "india", "juliett",
        ];
        let rows: Vec<String> = (0..10)
            .map(|i| row(&format!("p{i}"), true, true, &format!("{} marker", W[i])))
            .collect();
        let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
        let a = write(d.path(), "a.jsonl", &refs);
        let b = write(
            d.path(),
            "b.jsonl",
            &[&row("p0", true, true, "alpha marker restated")],
        );
        let set = build(&[a.clone(), b.clone()], 7).unwrap();
        assert_eq!(set.n_different, 7);
        let again = build(&[a, b], 7).unwrap();
        assert_eq!(set.pairs, again.pairs, "the cap must not sample randomly");
    }
}
