// SPDX-License-Identifier: AGPL-3.0-or-later
//! Governance tension-detector bench (FR-9 Lane A) — pure scorer.
//!
//! Scores a governance corpus's detected `Tension` edges against an
//! exhaustively-labeled ground-truth manifest (`truth.json`): which
//! genuine conflicts were found (recall, per type and per split) and
//! what fraction of flagged pairs are real (precision). This is the
//! productionized form of the session's `score_tensions.py` prototype.
//!
//! Pure and deterministic: the scorer takes already-mapped
//! section-pairs (a [`PairKey`] per detected Tension edge) + the parsed
//! truth manifest and returns a [`DetectorReport`]. The IO that maps
//! `atoms.json` + `edges.json` + the chapter manifest into `PairKey`s
//! lives in the bench command (it needs corpus-engine atom types); this
//! module stays model-free and corpus-schema-free so it unit-tests with
//! no daemon (ARCH §12.4).
//!
//! Split discipline (entity-resolution-bench convention): recall is
//! scored *per split* so `test` can be held sacred while `train`/`dev`
//! are tuned. Precision is corpus-wide — a flagged pair that matches a
//! genuine tension in any split is a true positive — because "other"
//! false positives (flagged pairs absent from the manifest) have no
//! split to belong to.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A section's stable identity — Charter article numeral or Decision
/// date. Both are unique within the fixture and robust to the
/// enricher's internal section ordering.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum SectionKey {
    Article(String),
    Date(String),
    Other(String),
}

impl SectionKey {
    /// Derive the key from a chapter title, e.g.
    /// `"Maple House Charter, Article XI — Quiet Study Hours"` →
    /// `Article("XI")`, `"Decision — 2026-03-14 — Guest Policy"` →
    /// `Date("2026-03-14")`. Manual parse (no regex dep): an
    /// `Article <roman>` token, else the first `YYYY-MM-DD`.
    pub fn from_title(title: &str) -> Self {
        if let Some(idx) = title.find("Article ") {
            let rest = &title[idx + "Article ".len()..];
            let num: String = rest
                .chars()
                .take_while(|c| "IVXLCDM".contains(*c))
                .collect();
            if !num.is_empty() {
                return SectionKey::Article(num);
            }
        }
        let b = title.as_bytes();
        let mut i = 0;
        while i + 10 <= b.len() {
            let d = |j: usize| b[j].is_ascii_digit();
            if d(i)
                && d(i + 1)
                && d(i + 2)
                && d(i + 3)
                && b[i + 4] == b'-'
                && d(i + 5)
                && d(i + 6)
                && b[i + 7] == b'-'
                && d(i + 8)
                && d(i + 9)
            {
                return SectionKey::Date(title[i..i + 10].to_string());
            }
            i += 1;
        }
        SectionKey::Other(title.trim().to_string())
    }
}

/// An unordered pair of sections — the unit a tension connects.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct PairKey(pub SectionKey, pub SectionKey);

impl PairKey {
    pub fn new(a: SectionKey, b: SectionKey) -> Self {
        if a <= b {
            Self(a, b)
        } else {
            Self(b, a)
        }
    }
}

/// Train/dev/test pool. `test` is sacred — guarded by a `PeekBudget`
/// (reused from `entity_resolution_bench`) at the bench-command layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Train,
    Dev,
    Test,
}

// ── truth.json shape ────────────────────────────────────────

#[derive(Clone, Debug, Deserialize)]
struct SectionRef {
    #[serde(default)]
    article: Option<String>,
    #[serde(default)]
    date: Option<String>,
}

impl SectionRef {
    fn key(&self) -> SectionKey {
        if let Some(a) = &self.article {
            SectionKey::Article(a.clone())
        } else if let Some(d) = &self.date {
            SectionKey::Date(d.clone())
        } else {
            SectionKey::Other(String::new())
        }
    }
}

/// One planted tension — a genuine conflict between two sections.
#[derive(Clone, Debug, Deserialize)]
pub struct PlantedRow {
    pub id: String,
    #[serde(rename = "type")]
    pub ttype: String,
    pub split: Split,
    a: SectionRef,
    b: SectionRef,
    #[serde(default)]
    pub why: String,
}

impl PlantedRow {
    pub fn pair(&self) -> PairKey {
        PairKey::new(self.a.key(), self.b.key())
    }
}

/// One expected-non — a superficially-related pair that must NOT be
/// flagged (a compatible refinement, an additive rule, or a decoy).
#[derive(Clone, Debug, Deserialize)]
pub struct NonRow {
    pub id: String,
    pub split: Split,
    a: SectionRef,
    b: SectionRef,
    #[serde(default)]
    pub why: String,
}

impl NonRow {
    pub fn pair(&self) -> PairKey {
        PairKey::new(self.a.key(), self.b.key())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct GovernanceTruth {
    #[serde(default)]
    pub planted_tensions: Vec<PlantedRow>,
    #[serde(default)]
    pub expected_non_tensions: Vec<NonRow>,
}

impl GovernanceTruth {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

// ── report ──────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pr {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectorReport {
    /// Overall precision (corpus-wide) + recall (over `recall_splits`).
    pub overall: Pr,
    /// Recall per tension type (over `recall_splits`): type → (found, total).
    pub recall_by_type: BTreeMap<String, (usize, usize)>,
    /// Planted-tension ids found / missed (over `recall_splits`).
    pub planted_found: Vec<String>,
    pub planted_missed: Vec<String>,
    /// Expected-non ids the detector wrongly flagged (precision killers).
    pub flagged_decoys: Vec<String>,
    /// Flagged pairs that are neither planted nor a labeled decoy.
    pub flagged_other: Vec<PairKey>,
    pub n_detected_pairs: usize,
    pub n_detected_edges: usize,
}

/// Score detected Tension edges against the truth manifest.
///
/// `detected_edges` is one [`PairKey`] per detected Tension edge (the
/// scorer dedups to pairs and counts edges). `recall_splits` scopes
/// which planted tensions count toward recall (e.g. `&[Split::Test]`
/// for the gated metric); precision is always corpus-wide.
pub fn score_detector(
    truth: &GovernanceTruth,
    detected_edges: &[PairKey],
    recall_splits: &[Split],
) -> DetectorReport {
    let n_detected_edges = detected_edges.len();
    let detected_pairs: HashSet<PairKey> = detected_edges.iter().cloned().collect();

    // ── recall (scoped to recall_splits), overall + per type ──
    let in_scope = |s: &Split| recall_splits.contains(s);
    let mut planted_found = Vec::new();
    let mut planted_missed = Vec::new();
    let mut recall_by_type: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut n_planted_scope = 0usize;
    for p in &truth.planted_tensions {
        if !in_scope(&p.split) {
            continue;
        }
        n_planted_scope += 1;
        let entry = recall_by_type.entry(p.ttype.clone()).or_insert((0, 0));
        entry.1 += 1;
        if detected_pairs.contains(&p.pair()) {
            entry.0 += 1;
            planted_found.push(p.id.clone());
        } else {
            planted_missed.push(p.id.clone());
        }
    }
    let recall = if n_planted_scope == 0 {
        0.0
    } else {
        planted_found.len() as f64 / n_planted_scope as f64
    };

    // ── precision (corpus-wide): a flagged pair matching ANY genuine
    //    tension is a TP; otherwise FP (decoy or unlabeled "other") ──
    let all_planted: HashSet<PairKey> = truth.planted_tensions.iter().map(|p| p.pair()).collect();
    let decoys: HashMap<PairKey, String> = truth
        .expected_non_tensions
        .iter()
        .map(|n| (n.pair(), n.id.clone()))
        .collect();
    let mut tp = 0usize;
    let mut flagged_decoys = Vec::new();
    let mut flagged_other = Vec::new();
    for pk in &detected_pairs {
        if all_planted.contains(pk) {
            tp += 1;
        } else if let Some(id) = decoys.get(pk) {
            flagged_decoys.push(id.clone());
        } else {
            flagged_other.push(pk.clone());
        }
    }
    let n_detected_pairs = detected_pairs.len();
    let precision = if n_detected_pairs == 0 {
        0.0
    } else {
        tp as f64 / n_detected_pairs as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };

    flagged_decoys.sort();
    DetectorReport {
        overall: Pr {
            precision,
            recall,
            f1,
        },
        recall_by_type,
        planted_found,
        planted_missed,
        flagged_decoys,
        flagged_other,
        n_detected_pairs,
        n_detected_edges,
    }
}

pub const ALL_SPLITS: &[Split] = &[Split::Train, Split::Dev, Split::Test];

#[cfg(test)]
mod tests {
    use super::*;

    fn art(n: &str) -> SectionKey {
        SectionKey::Article(n.into())
    }
    fn date(d: &str) -> SectionKey {
        SectionKey::Date(d.into())
    }
    fn pk(a: SectionKey, b: SectionKey) -> PairKey {
        PairKey::new(a, b)
    }

    fn truth() -> GovernanceTruth {
        // 3 planted (2 test, 1 train) + 2 decoys.
        let p = |id: &str, ttype: &str, split: Split, a: SectionRef, b: SectionRef| PlantedRow {
            id: id.into(),
            ttype: ttype.into(),
            split,
            a,
            b,
            why: String::new(),
        };
        let aref = |s: &str| SectionRef {
            article: Some(s.into()),
            date: None,
        };
        let dref = |s: &str| SectionRef {
            article: None,
            date: Some(s.into()),
        };
        GovernanceTruth {
            planted_tensions: vec![
                p(
                    "T1",
                    "direct_contradiction",
                    Split::Test,
                    aref("I"),
                    dref("2026-03-14"),
                ),
                p(
                    "T2",
                    "direct_contradiction",
                    Split::Test,
                    aref("VI"),
                    dref("2026-06-03"),
                ),
                p(
                    "T3",
                    "scope_overlap",
                    Split::Train,
                    dref("2026-02-10"),
                    dref("2026-06-22"),
                ),
            ],
            expected_non_tensions: vec![
                NonRow {
                    id: "D1".into(),
                    split: Split::Test,
                    a: aref("I"),
                    b: dref("2026-03-28"),
                    why: String::new(),
                },
                NonRow {
                    id: "N1".into(),
                    split: Split::Test,
                    a: aref("II"),
                    b: dref("2026-02-24"),
                    why: String::new(),
                },
            ],
        }
    }

    #[test]
    fn from_title_parses_article_and_date() {
        assert_eq!(
            SectionKey::from_title("Maple House Charter, Article XI — Quiet Study Hours"),
            art("XI")
        );
        assert_eq!(
            SectionKey::from_title("Decision — 2026-03-14 — Guest Policy Revisited"),
            date("2026-03-14")
        );
        assert_eq!(SectionKey::from_title("Article I — Guests"), art("I"));
    }

    #[test]
    fn perfect_test_detection_scores_1() {
        let t = truth();
        // Detect exactly the two test-split planted tensions.
        let detected = vec![
            pk(art("I"), date("2026-03-14")),
            pk(art("VI"), date("2026-06-03")),
        ];
        let r = score_detector(&t, &detected, &[Split::Test]);
        assert_eq!(r.overall.recall, 1.0);
        assert_eq!(r.overall.precision, 1.0);
        assert_eq!(r.planted_missed.len(), 0);
    }

    #[test]
    fn missed_planted_lowers_recall() {
        let t = truth();
        let detected = vec![pk(art("I"), date("2026-03-14"))]; // only T1 of 2 test
        let r = score_detector(&t, &detected, &[Split::Test]);
        assert_eq!(r.overall.recall, 0.5);
        assert_eq!(r.planted_missed, vec!["T2".to_string()]);
    }

    #[test]
    fn flagged_decoy_lowers_precision_and_is_named() {
        let t = truth();
        let detected = vec![
            pk(art("I"), date("2026-03-14")),   // TP
            pk(art("I"), date("2026-03-28")),   // FP: decoy D1
            pk(art("III"), date("2026-09-09")), // FP: other (unlabeled)
        ];
        let r = score_detector(&t, &detected, &[Split::Test]);
        // 1 TP of 3 flagged.
        assert!((r.overall.precision - (1.0 / 3.0)).abs() < 1e-9);
        assert_eq!(r.flagged_decoys, vec!["D1".to_string()]);
        assert_eq!(r.flagged_other.len(), 1);
    }

    #[test]
    fn recall_is_split_scoped_precision_is_corpus_wide() {
        let t = truth();
        // Detect only the TRAIN planted tension.
        let detected = vec![pk(date("2026-02-10"), date("2026-06-22"))];
        // Scoring recall on TEST: the train hit doesn't count for recall…
        let r = score_detector(&t, &detected, &[Split::Test]);
        assert_eq!(r.overall.recall, 0.0);
        // …but it IS a real tension, so precision stays 1.0 (corpus-wide).
        assert_eq!(r.overall.precision, 1.0);
        // Scoring recall on TRAIN finds it.
        let r2 = score_detector(&t, &detected, &[Split::Train]);
        assert_eq!(r2.overall.recall, 1.0);
    }

    #[test]
    fn per_type_recall_breaks_down() {
        let t = truth();
        let detected = vec![pk(art("I"), date("2026-03-14"))]; // T1 only (direct_contradiction)
        let r = score_detector(&t, &detected, &[Split::Test]);
        // direct_contradiction: 1 of 2 found on test.
        assert_eq!(r.recall_by_type.get("direct_contradiction"), Some(&(1, 2)));
    }

    #[test]
    fn edge_dedup_distinct_from_pair_count() {
        let t = truth();
        // Two edges on the same section-pair → one detected pair.
        let detected = vec![
            pk(art("I"), date("2026-03-14")),
            pk(date("2026-03-14"), art("I")), // same unordered pair
        ];
        let r = score_detector(&t, &detected, &[Split::Test]);
        assert_eq!(r.n_detected_edges, 2);
        assert_eq!(r.n_detected_pairs, 1);
    }
}
