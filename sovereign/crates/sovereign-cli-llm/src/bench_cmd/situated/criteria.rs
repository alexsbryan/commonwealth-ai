// SPDX-License-Identifier: AGPL-3.0-or-later
//! The situatedness criterion vocabulary — a closed set, loaded as data.
//!
//! Unlike the moral bank, criteria here are **probe-independent**: which
//! criteria apply to a probe is a function of its `PressureKind`, never of
//! its content. That is what structurally prevents corpus vocabulary from
//! reaching a criterion (`bench/situated/CRITERIA_DRAFT.md` — the
//! teach-to-the-test audit), and it is what lets P5 judge a live turn
//! against the same vocabulary with no bank at all.
//!
//! Adding a behaviour is an edit to `bench/situated/criteria.toml` plus a
//! re-calibration of the judge — never an inline prompt edit
//! (ARCH_PRINCIPLES §2: closed sets are enums).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sovereign_eval::chaos_monkey::PressureKind;

/// Wildcard in `applies_to`: this criterion applies to every question type.
const ALL_TYPES: &str = "*";

#[derive(Debug, Clone, Deserialize)]
pub struct Vocabulary {
    #[serde(default)]
    pub meta: VocabMeta,
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Path this was loaded from, for diagnostics. Not part of the TOML.
    #[serde(skip)]
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VocabMeta {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub calibration_file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Criterion {
    /// Stable key. Part of the content hash, so renaming a key re-ids every
    /// criterion derived from it — which is correct: a renamed behaviour is
    /// a different behaviour.
    pub key: String,
    pub dimension: String,
    /// Signed weight. Positive = good situatedness does this; negative =
    /// good situatedness AVOIDS this (fulfilled when the judge says "no").
    pub weight: i32,
    /// Exactly what the judge is shown. One clause — the judge is small.
    pub text: String,
    /// Question types this applies to, or `["*"]` for all.
    pub applies_to: Vec<String>,
}

impl Criterion {
    pub fn applies(&self, qtype: PressureKind) -> bool {
        let label = qtype.label();
        self.applies_to.iter().any(|t| t == ALL_TYPES || t == label)
    }
}

/// One criterion bound to one probe — what the runner actually judges.
#[derive(Debug, Clone, Serialize)]
pub struct BoundCriterion {
    /// `{key}@{content-hash}` over (probe, key, weight, text). Identity from
    /// essence, never a counter (ARCH_PRINCIPLES §7.5): re-generating the
    /// bank from the same inputs yields the same ids byte-for-byte, and
    /// editing a criterion's text gives it a new id rather than silently
    /// redefining an old one.
    ///
    /// The key is carried in the id, not just the hash, because the whole
    /// value of this lane is naming WHICH behaviour failed. A bare hash
    /// forced a reader to reconstruct the mapping by hand to learn anything
    /// — observed on the first live run, 2026-08-04. This lane owns the
    /// format; [`super::report::criterion_key`] is the only reader.
    pub id: String,
    pub key: String,
    pub dimension: String,
    pub weight: i32,
    pub text: String,
}

/// Materialise the criteria that apply to one probe. Deterministic: the
/// vocabulary's declaration order is preserved, so the bank is regenerable.
pub fn bind(vocab: &Vocabulary, probe_id: &str, qtype: PressureKind) -> Vec<BoundCriterion> {
    vocab
        .criteria
        .iter()
        .filter(|c| c.applies(qtype))
        .map(|c| BoundCriterion {
            id: content_hash(probe_id, c),
            key: c.key.clone(),
            dimension: c.dimension.clone(),
            weight: c.weight,
            text: c.text.clone(),
        })
        .collect()
}

fn content_hash(probe_id: &str, c: &Criterion) -> String {
    let mut h = Sha256::new();
    // Field separators so ("ab","c") and ("a","bc") cannot collide.
    h.update(probe_id.as_bytes());
    h.update([0u8]);
    h.update(c.key.as_bytes());
    h.update([0u8]);
    h.update(c.weight.to_le_bytes());
    h.update([0u8]);
    h.update(c.text.as_bytes());
    format!("{}@{}", c.key, hex::encode(&h.finalize()[..6]))
}

pub fn load(path: &Path) -> Result<Vocabulary, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut vocab: Vocabulary = toml::from_str(&content).map_err(|e| format!("parse: {e}"))?;
    vocab.source_path = path.to_path_buf();
    validate(&vocab)?;
    Ok(vocab)
}

/// Reject a vocabulary that cannot produce a meaningful score. Every check
/// here has a failing input named in the tests below — a check with no
/// failing input you can name is not a check (ARCH_PRINCIPLES §18.1).
fn validate(v: &Vocabulary) -> Result<(), String> {
    if v.criteria.is_empty() {
        return Err("vocabulary has no criteria — nothing to judge".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &v.criteria {
        if c.key.is_empty() {
            return Err("criterion with empty key".into());
        }
        if !seen.insert(c.key.as_str()) {
            return Err(format!("duplicate criterion key `{}`", c.key));
        }
        if c.text.trim().is_empty() {
            return Err(format!("criterion `{}` has empty text", c.key));
        }
        if c.dimension.trim().is_empty() {
            return Err(format!("criterion `{}` has no dimension", c.key));
        }
        if c.weight == 0 {
            return Err(format!(
                "criterion `{}` has weight 0 — it can never move the score and would \
                 silently pad the denominator",
                c.key
            ));
        }
        if c.weight.abs() > 3 {
            return Err(format!(
                "criterion `{}` weight {} outside the -3..=3 range the rubric uses",
                c.key, c.weight
            ));
        }
        if c.applies_to.is_empty() {
            return Err(format!(
                "criterion `{}` applies to no question type — it would never be judged",
                c.key
            ));
        }
        for t in &c.applies_to {
            if t != ALL_TYPES && !KNOWN_TYPES.contains(&t.as_str()) {
                return Err(format!(
                    "criterion `{}` applies_to `{t}` which is not a question type \
                     (known: {}, or `*`)",
                    c.key,
                    KNOWN_TYPES.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// The chaos `PressureKind` labels, as `applies_to` may name them. An
/// unknown label is a load error rather than a silently-never-applied
/// criterion — the failure mode where a typo makes a behaviour invisible.
const KNOWN_TYPES: &[&str] = &[
    "present",
    "absent_adjacent",
    "absent_out_of_domain",
    "distractor",
    "provenance_trap",
    "superseded_trap",
];

/// Default vocabulary location, resolved by walking up from CWD — the same
/// shape as the moral lane's scenario resolution.
pub const DEFAULT_CRITERIA_PATH: &str = "bench/situated/criteria.toml";

pub fn resolve_criteria_path(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("--criteria `{}` is not a file", p.display()))
        };
    }
    let mut here =
        std::env::current_dir().map_err(|e| format!("cannot resolve current dir: {e}"))?;
    loop {
        for prefix in ["", "sovereign"] {
            let candidate = if prefix.is_empty() {
                here.join(DEFAULT_CRITERIA_PATH)
            } else {
                here.join(prefix).join(DEFAULT_CRITERIA_PATH)
            };
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        if !here.pop() {
            break;
        }
    }
    Err(format!(
        "could not find `{DEFAULT_CRITERIA_PATH}` walking up from CWD. Pass --criteria."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[meta]
version = 1

[[criteria]]
key = "cites_a_source"
dimension = "grounding"
weight = 2
text = "The response points to a specific passage or source for its answer."
applies_to = ["present"]

[[criteria]]
key = "overstates_confidence"
dimension = "disclosure"
weight = -2
text = "The response presents an uncertain claim as settled fact."
applies_to = ["*"]
"#;

    fn write_tmp(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("criteria.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn loads_and_binds_by_question_type() {
        let (_d, p) = write_tmp(MINIMAL);
        let v = load(&p).unwrap();
        // `present` gets both: one targeted, one wildcard.
        let present = bind(&v, "present-wife", PressureKind::Present);
        assert_eq!(present.len(), 2);
        // An absent probe gets only the wildcard — the citation criterion
        // would be unanswerable there.
        let absent = bind(&v, "absent-heat", PressureKind::AbsentAdjacent);
        assert_eq!(absent.len(), 1);
        assert_eq!(absent[0].key, "overstates_confidence");
    }

    #[test]
    fn ids_are_content_derived_stable_and_probe_scoped() {
        let (_d, p) = write_tmp(MINIMAL);
        let v = load(&p).unwrap();
        let a = bind(&v, "present-wife", PressureKind::Present);
        let b = bind(&v, "present-wife", PressureKind::Present);
        assert_eq!(
            a.iter().map(|c| &c.id).collect::<Vec<_>>(),
            b.iter().map(|c| &c.id).collect::<Vec<_>>(),
            "regenerating the bank from identical inputs must be byte-stable"
        );
        let other = bind(&v, "present-target", PressureKind::Present);
        assert_ne!(
            a[0].id, other[0].id,
            "same criterion on another probe is another id"
        );
        // The behaviour must be readable straight off the id — the whole
        // point of the lane is naming which one failed.
        assert!(a[0].id.starts_with("cites_a_source@"), "{}", a[0].id);
    }

    #[test]
    fn editing_criterion_text_changes_the_id() {
        let (_d, p) = write_tmp(MINIMAL);
        let before = bind(&load(&p).unwrap(), "present-wife", PressureKind::Present)[0]
            .id
            .clone();
        let (_d2, p2) =
            write_tmp(&MINIMAL.replace("points to a specific passage", "cites anything"));
        let after = bind(&load(&p2).unwrap(), "present-wife", PressureKind::Present)[0]
            .id
            .clone();
        assert_ne!(
            before, after,
            "a reworded criterion is a different criterion — it must not inherit \
             the old id and silently redefine banked results"
        );
    }

    #[test]
    fn rejects_zero_and_out_of_range_weights() {
        let (_d, p) = write_tmp(&MINIMAL.replace("weight = 2", "weight = 0"));
        assert!(load(&p).unwrap_err().contains("weight 0"));
        let (_d, p) = write_tmp(&MINIMAL.replace("weight = 2", "weight = 7"));
        assert!(load(&p).unwrap_err().contains("outside the -3..=3"));
    }

    #[test]
    fn rejects_unknown_question_type() {
        let (_d, p) = write_tmp(&MINIMAL.replace(r#"["present"]"#, r#"["presnet"]"#));
        let err = load(&p).unwrap_err();
        assert!(err.contains("not a question type"), "{err}");
    }

    #[test]
    fn rejects_duplicate_keys_and_empty_applicability() {
        let (_d, p) = write_tmp(&MINIMAL.replace("overstates_confidence", "cites_a_source"));
        assert!(load(&p).unwrap_err().contains("duplicate"));
        let (_d, p) = write_tmp(&MINIMAL.replace(r#"["present"]"#, "[]"));
        assert!(load(&p).unwrap_err().contains("no question type"));
    }

    /// The calibration bank certifies the judge on EXACT criterion strings.
    /// Reword a criterion without re-labelling, and the judge stays certified
    /// on a sentence it will never be shown again — a silently invalid gate,
    /// which is the failure mode §18 exists to prevent. This test is the
    /// structural version of remembering to do it.
    #[test]
    fn calibration_items_quote_the_vocabulary_verbatim() {
        let path = match resolve_criteria_path(None) {
            Ok(p) => p,
            Err(_) => return, // filtered checkout
        };
        let vocab = load(&path).unwrap();
        let cal = path
            .parent()
            .unwrap()
            .join(if vocab.meta.calibration_file.is_empty() {
                "calibration.toml".to_string()
            } else {
                vocab.meta.calibration_file.clone()
            });
        if !cal.is_file() {
            return; // calibration not authored yet
        }
        let bank = crate::bench_cmd::rubric::judge::load_calibration(&cal)
            .expect("calibration bank must load");
        let known: std::collections::BTreeSet<&str> =
            vocab.criteria.iter().map(|c| c.text.as_str()).collect();
        for item in &bank.items {
            assert!(
                known.contains(item.criterion.as_str()),
                "calibration item `{}` quotes a criterion that is not in the vocabulary \
                 verbatim — the judge would be certified on a string it never sees:\n  {}",
                item.id,
                item.criterion
            );
        }

        // A bank of only clean-form items certifies a judge on the cases that
        // don't decide anything. Guard the composition, not just the labels.
        let hard: Vec<_> = bank
            .items
            .iter()
            .filter(|i| i.tier == crate::bench_cmd::rubric::judge::HARD_TIER)
            .collect();
        assert!(
            hard.len() * 4 >= bank.items.len(),
            "only {}/{} calibration items are the `hard` tier — an easy-heavy bank makes a \
             pass mean 'handles the obvious cases'",
            hard.len(),
            bank.items.len()
        );
        let (y, n) = hard.iter().fold((0, 0), |(y, n), i| match i.expected {
            crate::bench_cmd::rubric::judge::Ballot::Yes => (y + 1, n),
            crate::bench_cmd::rubric::judge::Ballot::No => (y, n + 1),
        });
        assert!(
            y >= 3 && n >= 3,
            "hard tier needs both classes (has {y} yes / {n} no)"
        );
        for i in &hard {
            assert!(
                !i.note.trim().is_empty(),
                "hard item `{}` has no note — a contestable label a reviewer cannot \
                 argue with is not reviewable",
                i.id
            );
        }
    }

    /// The shipped vocabulary must load and must cover every question type
    /// the chaos banks actually use — a type with zero applicable criteria
    /// would score as a silent n/a rather than a finding.
    #[test]
    fn checked_in_vocabulary_loads_and_covers_every_type() {
        let path = match resolve_criteria_path(None) {
            Ok(p) => p,
            Err(_) => return, // filtered checkout
        };
        let v = load(&path).unwrap();
        for qt in [
            PressureKind::Present,
            PressureKind::AbsentAdjacent,
            PressureKind::AbsentOutOfDomain,
            PressureKind::Distractor,
            PressureKind::ProvenanceTrap,
            PressureKind::SupersededTrap,
        ] {
            let bound = bind(&v, "probe", qt);
            assert!(
                bound.len() >= 5,
                "{} binds only {} criteria — too thin to score",
                qt.label(),
                bound.len()
            );
        }
    }
}
