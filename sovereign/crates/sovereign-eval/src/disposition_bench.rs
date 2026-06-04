//! Gold-label loader + split discipline for the UAP disposition bench —
//! the classification analog of `entity_resolution_bench`. Reuses the
//! proven `Split` + `PeekBudget` primitives verbatim (re-exported, not
//! redefined) so the train/test/holdout discipline is identical across
//! verticals.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

// Reuse the proven split + peek-budget primitives — do NOT redefine.
pub use crate::entity_resolution_bench::{PeekBudget, Split};
use crate::disposition_score::Labeling;

fn default_label_source() -> String {
    "FIXTURE".to_string()
}

/// One frozen gold label (UFO.md `GOLD_LABEL`). Holdout records use the
/// `<sealed>` placeholder discipline from `GroundTruthEntity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldLabel {
    pub case_id: String,
    /// A `Disposition::as_str()` token (or `<sealed>` for sealed holdout).
    pub official_category: String,
    pub split: Split,
    #[serde(default = "default_label_source")]
    pub label_source: String,
    #[serde(default)]
    pub frozen_at: i64,
}

impl GoldLabel {
    pub fn is_sealed(&self) -> bool {
        self.official_category == "<sealed>"
    }
}

/// The fixture record shape (`cases.jsonl`) — only the fields the bench
/// needs; serde ignores the rest.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureCase {
    pub case_id: String,
    pub date: String,
    pub disposition: String,
    pub narrative: String,
    #[serde(default)]
    pub shape: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
}

pub struct GoldLabels {
    pub entries: Vec<GoldLabel>,
}

impl GoldLabels {
    /// Load the committed gold-label JSONL (one record per line).
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut entries = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<GoldLabel>(line) {
                Ok(e) => entries.push(e),
                Err(err) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("gold_labels line {}: {err}", i + 1),
                    ));
                }
            }
        }
        Ok(Self { entries })
    }

    pub fn by_split(&self, split: Split) -> Vec<&GoldLabel> {
        self.entries.iter().filter(|e| e.split == split).collect()
    }

    /// `case_id` → category for a split, excluding sealed entries. This
    /// is the scorer's `gold` argument.
    pub fn as_gold_labeling(&self, split: Split) -> Labeling {
        let mut out = Labeling::new();
        for e in &self.entries {
            if e.split != split || e.is_sealed() {
                continue;
            }
            out.insert(e.case_id.clone(), e.official_category.clone());
        }
        out
    }

    /// Merge unsealed holdout entries from a private store (verbatim
    /// analog of `BenchGroundTruth::merge_unsealed_holdout`). Existing
    /// entries with the same `case_id` are replaced.
    pub fn merge_unsealed_holdout(&mut self, holdout_path: &Path) -> std::io::Result<usize> {
        if !holdout_path.exists() {
            return Ok(0);
        }
        let other = Self::load(holdout_path)?;
        let n = other.entries.len();
        for incoming in other.entries {
            if let Some(existing) = self
                .entries
                .iter_mut()
                .find(|e| e.case_id == incoming.case_id)
            {
                *existing = incoming;
            } else {
                self.entries.push(incoming);
            }
        }
        Ok(n)
    }

    /// Author gold rows from fixture-style records whose `disposition`
    /// field already carries the official category. The local-fixture
    /// substitute for the "finding-aid index → GOLD_LABEL" tool.
    /// `split_fn` assigns the split deterministically by case_id.
    pub fn author_from_fixture(
        records: &[FixtureCase],
        label_source: &str,
        frozen_at: i64,
        split_fn: impl Fn(&str) -> Split,
    ) -> GoldLabels {
        let entries = records
            .iter()
            .map(|c| GoldLabel {
                case_id: c.case_id.clone(),
                official_category: c.disposition.clone(),
                split: split_fn(&c.case_id),
                label_source: label_source.to_string(),
                frozen_at,
            })
            .collect();
        GoldLabels { entries }
    }
}

/// Load fixture cases from a `cases.jsonl` file.
pub fn load_fixture_cases(path: &Path) -> std::io::Result<Vec<FixtureCase>> {
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let c: FixtureCase = serde_json::from_str(line).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("cases line {}: {err}", i + 1),
            )
        })?;
        out.push(c);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(dir: &Path, name: &str, lines: &[String]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        path
    }

    #[test]
    fn load_and_gold_labeling_excludes_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![
            r#"{"case_id":"c1","official_category":"AIRCRAFT","split":"train"}"#.to_string(),
            r#"{"case_id":"c2","official_category":"BIRD","split":"test"}"#.to_string(),
            r#"{"case_id":"h1","official_category":"<sealed>","split":"holdout"}"#.to_string(),
        ];
        let p = write_lines(dir.path(), "gold.jsonl", &lines);
        let gold = GoldLabels::load(&p).unwrap();
        assert_eq!(gold.entries.len(), 3);
        let train = gold.as_gold_labeling(Split::Train);
        assert_eq!(train.get("c1"), Some(&"AIRCRAFT".to_string()));
        // Sealed holdout produces no labeling.
        assert!(gold.as_gold_labeling(Split::Holdout).is_empty());
    }

    #[test]
    fn author_from_fixture_uses_disposition_as_category() {
        let cases = vec![
            FixtureCase {
                case_id: "c1".into(),
                date: "1952-08-14".into(),
                disposition: "ASTRONOMICAL".into(),
                narrative: "x".into(),
                shape: Some("DISC".into()),
                location: None,
            },
            FixtureCase {
                case_id: "c2".into(),
                date: "2019-05-24".into(),
                disposition: "SATELLITE".into(),
                narrative: "y".into(),
                shape: None,
                location: None,
            },
        ];
        let gold = GoldLabels::author_from_fixture(&cases, "FIXTURE", 0, |id| {
            if id == "c2" {
                Split::Test
            } else {
                Split::Train
            }
        });
        let train = gold.as_gold_labeling(Split::Train);
        assert_eq!(train.get("c1"), Some(&"ASTRONOMICAL".to_string()));
        let test = gold.as_gold_labeling(Split::Test);
        assert_eq!(test.get("c2"), Some(&"SATELLITE".to_string()));
    }

    #[test]
    fn merge_unsealed_holdout_replaces_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let pub_lines =
            vec![r#"{"case_id":"h1","official_category":"<sealed>","split":"holdout"}"#.to_string()];
        let p = write_lines(dir.path(), "gold.jsonl", &pub_lines);
        let mut gold = GoldLabels::load(&p).unwrap();
        let secret =
            vec![r#"{"case_id":"h1","official_category":"UNIDENTIFIED","split":"holdout"}"#.to_string()];
        let ps = write_lines(dir.path(), "holdout.jsonl", &secret);
        let added = gold.merge_unsealed_holdout(&ps).unwrap();
        assert_eq!(added, 1);
        assert_eq!(
            gold.as_gold_labeling(Split::Holdout).get("h1"),
            Some(&"UNIDENTIFIED".to_string())
        );
    }

    #[test]
    fn split_reexport_is_the_same_type() {
        // The re-export is the canonical Split — holdout gating intact.
        assert!(Split::Holdout.requires_unseal());
        assert!(!Split::Train.requires_unseal());
    }
}
