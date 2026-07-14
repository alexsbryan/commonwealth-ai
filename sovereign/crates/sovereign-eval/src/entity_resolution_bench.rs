// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entity-resolution bench loader + split discipline (Phase 3 of the
//! architecture-over-Enron push).
//!
//! Reads `ground_truth_entities.jsonl` and applies the
//! train/test/holdout discipline. The runner-side enforcement lives in
//! `sovereign-cli-llm/src/bench_cmd`; this module owns the
//! data-model and the peek-budget primitive so they're reusable across
//! future verticals (Firm Inbox ground-truth, sales-intel ground-truth,
//! …) with the same shape.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::entity_resolution_score::Clustering;

/// Three-way split. `Train` runs freely; `Test` runs once per tuned
/// policy; `Holdout` refuses to run without `--unseal-holdout` and
/// burns a peek-budget counter when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Train,
    Test,
    Holdout,
}

impl Split {
    pub fn as_str(&self) -> &'static str {
        match self {
            Split::Train => "train",
            Split::Test => "test",
            Split::Holdout => "holdout",
        }
    }

    /// True when the split must be unsealed via an explicit CLI flag.
    pub fn requires_unseal(&self) -> bool {
        matches!(self, Split::Holdout)
    }
}

/// One ground-truth entity record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundTruthEntity {
    pub canonical_id: String,
    pub entity_type: String,
    pub canonical_name: String,
    pub surface_forms: Vec<String>,
    pub split: Split,
    /// Authoring timestamp in unix seconds — records when the entry
    /// landed so the holdout-leakage audit can spot post-hoc edits.
    #[serde(default)]
    pub authored_ts: i64,
}

impl GroundTruthEntity {
    /// Sealed entries carry placeholder `<sealed>` strings in the
    /// canonical name + surface forms. The runner refuses to score
    /// such entries without unseal.
    pub fn is_sealed(&self) -> bool {
        self.canonical_name == "<sealed>" || self.surface_forms.iter().all(|s| s == "<sealed>")
    }
}

/// Loader that reads the JSONL ground-truth file + optionally the
/// canonical unsealed-holdout store at
/// `~/.sovereign/bench/<bench>/holdout.jsonl`.
pub struct BenchGroundTruth {
    pub entries: Vec<GroundTruthEntity>,
}

impl BenchGroundTruth {
    /// Load the public (committed) ground-truth file.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut entries = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<GroundTruthEntity>(line) {
                Ok(e) => entries.push(e),
                Err(err) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("ground_truth line {}: {err}", i + 1),
                    ));
                }
            }
        }
        Ok(Self { entries })
    }

    /// Merge in unsealed holdout entries from the canonical store.
    /// Existing entries with the same `canonical_id` are replaced.
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
                .find(|e| e.canonical_id == incoming.canonical_id)
            {
                *existing = incoming;
            } else {
                self.entries.push(incoming);
            }
        }
        Ok(n)
    }

    /// Filter to entries in a given split.
    pub fn by_split(&self, split: Split) -> Vec<&GroundTruthEntity> {
        self.entries.iter().filter(|e| e.split == split).collect()
    }

    /// Build a [`Clustering`] keyed by *surface form* → canonical id.
    /// The reconciler's predicted clustering uses the same key shape so
    /// the two are alignable by [`crate::entity_resolution_score::score`].
    pub fn as_gold_clustering(&self, split: Split) -> Clustering {
        let mut out = Clustering::new();
        for e in &self.entries {
            if e.split != split || e.is_sealed() {
                continue;
            }
            for sf in &e.surface_forms {
                out.insert(sf.clone(), e.canonical_id.clone());
            }
        }
        out
    }
}

// ── Peek budget ──────────────────────────────────────────────

/// On-disk record of how many times the holdout has been unsealed.
/// Lives at
/// `baselines/<bench-id>/peek_budget.json` so it's checked in alongside
/// the baseline numbers and visible to a release-time auditor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeekBudget {
    pub holdout_peeks: u32,
    /// Per-peek records — when, why, what changed since the prior
    /// peek. Append-only.
    #[serde(default)]
    pub log: Vec<PeekEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeekEntry {
    pub ts_unix: i64,
    pub reason: String,
    pub commit_hash: Option<String>,
}

impl PeekBudget {
    /// Read the peek budget from disk. Returns a fresh zero-budget
    /// when the file does not yet exist.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Persist atomically (write-temp + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp: PathBuf = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Burn one peek + append the audit entry. Returns the new count.
    pub fn burn(&mut self, reason: impl Into<String>, commit_hash: Option<String>) -> u32 {
        self.holdout_peeks = self.holdout_peeks.saturating_add(1);
        self.log.push(PeekEntry {
            ts_unix: now_secs(),
            reason: reason.into(),
            commit_hash,
        });
        self.holdout_peeks
    }
}

use sovereign_time::unix_now as now_secs;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(dir: &Path, contents: &[GroundTruthEntity]) -> PathBuf {
        let path = dir.join("gt.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for e in contents {
            let line = serde_json::to_string(e).unwrap();
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    fn ent(id: &str, split: Split, surfaces: &[&str], sealed: bool) -> GroundTruthEntity {
        GroundTruthEntity {
            canonical_id: id.to_string(),
            entity_type: "person".into(),
            canonical_name: if sealed {
                "<sealed>".into()
            } else {
                id.to_string()
            },
            surface_forms: surfaces.iter().map(|s| s.to_string()).collect(),
            split,
            authored_ts: 1_700_000_000,
        }
    }

    #[test]
    fn load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let entries = vec![
            ent("a", Split::Train, &["a1", "a2"], false),
            ent("b", Split::Test, &["b1"], false),
            ent("h", Split::Holdout, &["<sealed>"], true),
        ];
        let p = write_jsonl(dir.path(), &entries);
        let loaded = BenchGroundTruth::load(&p).unwrap();
        assert_eq!(loaded.entries.len(), 3);
        assert!(loaded.entries[2].is_sealed());
    }

    #[test]
    fn as_gold_clustering_excludes_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let entries = vec![
            ent("a", Split::Train, &["a1", "a2"], false),
            ent("b", Split::Train, &["b1"], false),
            ent("c", Split::Holdout, &["<sealed>"], true),
        ];
        let p = write_jsonl(dir.path(), &entries);
        let loaded = BenchGroundTruth::load(&p).unwrap();
        let gold = loaded.as_gold_clustering(Split::Train);
        assert_eq!(gold.len(), 3);
        assert_eq!(gold["a1"], "a");
        assert_eq!(gold["a2"], "a");
        assert_eq!(gold["b1"], "b");
        let holdout_gold = loaded.as_gold_clustering(Split::Holdout);
        assert!(
            holdout_gold.is_empty(),
            "sealed holdout entries must not produce ground-truth pairs"
        );
    }

    #[test]
    fn merge_unsealed_holdout_replaces_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let public = vec![
            ent("a", Split::Train, &["a1"], false),
            ent("h", Split::Holdout, &["<sealed>"], true),
        ];
        let p_pub = write_jsonl(dir.path(), &public);
        let mut loaded = BenchGroundTruth::load(&p_pub).unwrap();
        // Unseal the holdout from a private file.
        let secret = vec![ent("h", Split::Holdout, &["secret1", "secret2"], false)];
        let p_secret = dir.path().join("holdout.jsonl");
        let mut f = fs::File::create(&p_secret).unwrap();
        for e in &secret {
            writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
        let added = loaded.merge_unsealed_holdout(&p_secret).unwrap();
        assert_eq!(added, 1);
        let h = loaded
            .entries
            .iter()
            .find(|e| e.canonical_id == "h")
            .unwrap();
        assert!(!h.is_sealed());
        assert_eq!(h.surface_forms.len(), 2);
    }

    #[test]
    fn peek_budget_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peek_budget.json");
        let mut budget = PeekBudget::load(&path).unwrap();
        assert_eq!(budget.holdout_peeks, 0);
        budget.burn(
            "tuned reconciler over train; want holdout sanity",
            Some("abc123".into()),
        );
        budget.save(&path).unwrap();
        let read = PeekBudget::load(&path).unwrap();
        assert_eq!(read.holdout_peeks, 1);
        assert_eq!(read.log.len(), 1);
        assert!(read.log[0].reason.contains("sanity"));
    }

    #[test]
    fn split_requires_unseal_only_for_holdout() {
        assert!(!Split::Train.requires_unseal());
        assert!(!Split::Test.requires_unseal());
        assert!(Split::Holdout.requires_unseal());
    }
}
