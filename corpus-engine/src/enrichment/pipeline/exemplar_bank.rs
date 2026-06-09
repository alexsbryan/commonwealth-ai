// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exemplar bank — data-driven prompt shaping for the v2 pipeline.
//!
//! Each phase gets a JSON file at
//! `~/.sovereign/enrichment/<corpus>/exemplars/phase<N>.json` containing
//! a list of `Exemplar`s: positive examples that model the desired
//! output shape, corrected examples that teach the difference between a
//! bad model output and the developer's rewrite, and negative examples
//! that name anti-patterns the model should avoid.
//!
//! The developer's creative work lives in these files. The runner
//! loads the bank at run time, embeds the selector text for each
//! exemplar, and picks the top-K nearest neighbours of the current
//! input before composing the prompt. In-memory cosine is sufficient
//! at current bank sizes (10-50); a LanceDB-backed replacement is
//! noted in the architecture roadmap once banks approach 200+.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::types::PipelinePhase;
use crate::error::{Error, Result};
use crate::types::EmbedFn;

/// What role this exemplar plays in the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExemplarKind {
    /// An output the developer would want the model to produce.
    Positive,
    /// The model produced `model_output`; the developer overrode with
    /// `corrected_output`. Teaches the diff.
    Corrected,
    /// The model produced `model_output`; the developer rejected it
    /// entirely. Teaches what not to do.
    Negative,
}

/// One entry in a bank. Phase-specific schemas are enforced by the
/// corresponding `Pipeline::parse_*` method at load time; the bank
/// itself is generic over `serde_json::Value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exemplar {
    pub id: String,
    pub kind: ExemplarKind,

    /// Phase-specific input description — e.g. the chapter text for
    /// phase 1, the cluster + chapter excerpts for phase 3.
    pub input: serde_json::Value,

    /// The "good" output the model should imitate.
    ///
    /// - `Positive`: the developer-authored target.
    /// - `Corrected`: the developer's corrected rewrite (what the
    ///   model *should* have produced).
    /// - `Negative`: unused (left as `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,

    /// The model's actual output when this exemplar was captured.
    ///
    /// Populated for `Corrected` and `Negative`. The prompt composer
    /// uses this to show the model its own mistake alongside the
    /// correction or rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_output: Option<serde_json::Value>,

    /// Duplicate of `output` kept separately for `Corrected` exemplars
    /// so downstream code can distinguish "authored from scratch" from
    /// "rewrote the model's output". Either field can carry the
    /// target — the prompt composer prefers `corrected_output` when
    /// both are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_output: Option<serde_json::Value>,

    /// The single most important field — why this exemplar matters.
    /// Surfaced in the prompt so the model sees the developer's
    /// reasoning, not just the shape.
    pub rationale: String,

    /// Text embedded for similarity selection. Falls back to a JSON
    /// dump of `input` when absent, but callers are encouraged to set
    /// something focused (e.g. the chapter's opening line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_text: Option<String>,

    /// RFC3339 timestamp. Set by `append`; left empty on user-authored
    /// bank files until the next save.
    #[serde(default)]
    pub created_at: String,

    /// Optional facet tag for atlas-pipeline exemplars. Phase 3
    /// naming runs one banking pass per facet
    /// (`question`/`claim`/`entity_state`/…); the tag lets a single
    /// bank file carry exemplars from multiple facets while still
    /// driving facet-specific selection via `select_top_k_facet`.
    ///
    /// `None` means "not facet-tagged" — the entry participates in
    /// unfiltered `select_top_k` calls but is excluded from any
    /// facet-filtered call. Banks loaded from the per-facet
    /// directory layout (`exemplars/<pipeline>/<phase>/<facet>.json`)
    /// land with the facet auto-stamped at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<String>,
}

impl Exemplar {
    /// The text that gets embedded for similarity selection.
    pub fn selector(&self) -> String {
        if let Some(s) = &self.selector_text {
            return s.clone();
        }
        // Fallback: stringify the input JSON. Good enough when the
        // input is short; banks with long inputs should set
        // `selector_text` explicitly to something focused.
        serde_json::to_string(&self.input).unwrap_or_default()
    }
}

/// On-disk envelope. Keeps room for forward-compat metadata without
/// versioning every exemplar individually.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BankFile {
    pub phase: PipelinePhase,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub exemplars: Vec<Exemplar>,
}

const BANK_SCHEMA_VERSION: u32 = 1;
fn default_schema_version() -> u32 {
    BANK_SCHEMA_VERSION
}

/// One loaded exemplar plus its precomputed embedding.
#[derive(Debug, Clone)]
struct LoadedExemplar {
    exemplar: Exemplar,
    embedding: Vec<f32>,
}

/// An exemplar bank for a single phase.
///
/// Two useful states:
/// - `open()` — loads the JSON but skips embedding. Use when you only
///   need counts/lints/roundtrip.
/// - `load_embedded()` — loads JSON and embeds every selector. Use
///   before calling `select_top_k()` for real prompt composition.
#[derive(Debug)]
pub struct ExemplarBank {
    phase: PipelinePhase,
    path: PathBuf,
    raw: Vec<Exemplar>,
    /// `None` until `load_embedded()` has run or `reembed` has been
    /// called after an `append`.
    loaded: Option<Vec<LoadedExemplar>>,
}

impl ExemplarBank {
    /// Open an existing bank without embedding. Returns an empty bank
    /// if the file does not exist.
    pub fn open(path: impl AsRef<Path>, phase: PipelinePhase) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                phase,
                path,
                raw: Vec::new(),
                loaded: None,
            });
        }
        let raw = fs::read_to_string(&path).map_err(Error::Io)?;
        let file: BankFile = serde_json::from_str(&raw).map_err(|e| {
            Error::Serialization(format!(
                "exemplar bank at {} is malformed: {}",
                path.display(),
                e
            ))
        })?;
        if file.phase != phase {
            return Err(Error::Serialization(format!(
                "exemplar bank at {} declares phase {:?} but was opened as {:?}",
                path.display(),
                file.phase,
                phase
            )));
        }
        if file.schema_version > BANK_SCHEMA_VERSION {
            return Err(Error::Serialization(format!(
                "exemplar bank at {} has schema_version {} but this binary supports {}",
                path.display(),
                file.schema_version,
                BANK_SCHEMA_VERSION
            )));
        }
        Ok(Self {
            phase,
            path,
            raw: file.exemplars,
            loaded: None,
        })
    }

    /// Open and embed every exemplar's selector text.
    pub async fn load_embedded(
        path: impl AsRef<Path>,
        phase: PipelinePhase,
        embed: &EmbedFn,
    ) -> Result<Self> {
        let mut bank = Self::open(path, phase)?;
        bank.reembed(embed).await?;
        Ok(bank)
    }

    /// Recompute embeddings for every exemplar. Idempotent.
    pub async fn reembed(&mut self, embed: &EmbedFn) -> Result<()> {
        let mut loaded = Vec::with_capacity(self.raw.len());
        for exemplar in &self.raw {
            let embedding = embed(&exemplar.selector()).await?;
            loaded.push(LoadedExemplar {
                exemplar: exemplar.clone(),
                embedding,
            });
        }
        self.loaded = Some(loaded);
        Ok(())
    }

    pub fn phase(&self) -> PipelinePhase {
        self.phase
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Exemplar> {
        self.raw.iter()
    }

    /// `(positive, corrected, negative)` counts.
    pub fn counts_by_kind(&self) -> (usize, usize, usize) {
        let mut p = 0;
        let mut c = 0;
        let mut n = 0;
        for e in &self.raw {
            match e.kind {
                ExemplarKind::Positive => p += 1,
                ExemplarKind::Corrected => c += 1,
                ExemplarKind::Negative => n += 1,
            }
        }
        (p, c, n)
    }

    /// Append an exemplar. The caller is responsible for calling
    /// `save()` to persist and `reembed()` before the next selection.
    pub fn append(&mut self, mut exemplar: Exemplar) {
        if exemplar.created_at.is_empty() {
            exemplar.created_at = now_rfc3339();
        }
        self.raw.push(exemplar);
        self.loaded = None; // stale after mutation
    }

    /// Write the bank to disk atomically (tmp → rename).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let file = BankFile {
            phase: self.phase,
            schema_version: BANK_SCHEMA_VERSION,
            exemplars: self.raw.clone(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| Error::Serialization(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(Error::Io)?;
        fs::rename(&tmp, &self.path).map_err(Error::Io)?;
        Ok(())
    }

    /// Pick the top-K exemplars whose selector embedding is nearest
    /// to `query_embedding` by cosine similarity. Returns fewer
    /// entries when the bank is smaller than K.
    ///
    /// Returns an empty vec if `reembed` / `load_embedded` has not
    /// been called.
    pub fn select_top_k(&self, query_embedding: &[f32], k: usize) -> Vec<&Exemplar> {
        self.select_top_k_facet(query_embedding, k, None)
    }

    /// Facet-filtered variant of `select_top_k`. When `facet_filter`
    /// is `Some(s)`, only exemplars with that exact `facet` value
    /// participate in scoring. `None` matches every exemplar,
    /// reproducing the legacy `select_top_k` behaviour.
    ///
    /// The atlas Phase 3 naming loop uses this to fetch
    /// facet-specific examples even when the bank on disk mixes
    /// facets — a convenience for hand-edited banks where
    /// splitting by facet file isn't worth the ceremony.
    pub fn select_top_k_facet(
        &self,
        query_embedding: &[f32],
        k: usize,
        facet_filter: Option<&str>,
    ) -> Vec<&Exemplar> {
        let Some(loaded) = &self.loaded else {
            return Vec::new();
        };
        if loaded.is_empty() || k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(f32, &Exemplar)> = loaded
            .iter()
            .filter(|l| match facet_filter {
                None => true,
                Some(f) => l.exemplar.facet.as_deref() == Some(f),
            })
            .map(|l| {
                (
                    cosine_similarity(query_embedding, &l.embedding),
                    &l.exemplar,
                )
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored.into_iter().map(|(_, e)| e).collect()
    }

    /// Lint pass — checks referential integrity of every entry. Used
    /// by `sovereign enrich <id> exemplars` so the developer catches
    /// malformed hand-edits before a run.
    pub fn lint(&self) -> Vec<ExemplarLint> {
        let mut issues = Vec::new();
        let mut ids: HashMap<&str, usize> = HashMap::new();
        for (i, e) in self.raw.iter().enumerate() {
            if e.id.trim().is_empty() {
                issues.push(ExemplarLint {
                    index: i,
                    id: e.id.clone(),
                    reason: "empty id".into(),
                });
            }
            if let Some(prev) = ids.insert(e.id.as_str(), i) {
                issues.push(ExemplarLint {
                    index: i,
                    id: e.id.clone(),
                    reason: format!("duplicate id (first seen at index {prev})"),
                });
            }
            if e.rationale.trim().is_empty() {
                issues.push(ExemplarLint {
                    index: i,
                    id: e.id.clone(),
                    reason: "empty rationale — rationale is the most important field".into(),
                });
            }
            match e.kind {
                ExemplarKind::Positive => {
                    if e.output.is_none() {
                        issues.push(ExemplarLint {
                            index: i,
                            id: e.id.clone(),
                            reason: "positive exemplar missing `output`".into(),
                        });
                    }
                }
                ExemplarKind::Corrected => {
                    if e.corrected_output.is_none() && e.output.is_none() {
                        issues.push(ExemplarLint {
                            index: i,
                            id: e.id.clone(),
                            reason: "corrected exemplar needs `corrected_output` (or `output`)"
                                .into(),
                        });
                    }
                    if e.model_output.is_none() {
                        issues.push(ExemplarLint {
                            index: i,
                            id: e.id.clone(),
                            reason:
                                "corrected exemplar needs `model_output` (what the model did wrong)"
                                    .into(),
                        });
                    }
                }
                ExemplarKind::Negative => {
                    if e.model_output.is_none() {
                        issues.push(ExemplarLint {
                            index: i,
                            id: e.id.clone(),
                            reason: "negative exemplar needs `model_output` (what the model did)"
                                .into(),
                        });
                    }
                }
            }
        }
        issues
    }
}

/// A linter finding on one exemplar.
#[derive(Debug, Clone)]
pub struct ExemplarLint {
    pub index: usize,
    pub id: String,
    pub reason: String,
}

// ── Similarity helpers ────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn fake_embed(deterministic: fn(&str) -> Vec<f32>) -> EmbedFn {
        Arc::new(move |s: &str| {
            let v = deterministic(s);
            Box::pin(async move { Ok(v) })
        })
    }

    fn positive(id: &str, selector: &str) -> Exemplar {
        Exemplar {
            id: id.into(),
            kind: ExemplarKind::Positive,
            input: serde_json::json!({"text": selector}),
            output: Some(serde_json::json!({"ok": true})),
            model_output: None,
            corrected_output: None,
            rationale: "Good because it is short.".into(),
            selector_text: Some(selector.into()),
            created_at: String::new(),
            facet: None,
        }
    }

    #[test]
    fn open_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        assert!(bank.is_empty());
        assert_eq!(bank.phase(), PipelinePhase::Questions);
    }

    #[test]
    fn save_and_open_roundtrip_preserves_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("phase1.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(positive("ex_001", "chapter one"));
        bank.append(Exemplar {
            id: "ex_002".into(),
            kind: ExemplarKind::Corrected,
            input: serde_json::json!({"text": "c"}),
            output: None,
            model_output: Some(serde_json::json!({"bad": true})),
            corrected_output: Some(serde_json::json!({"ok": true})),
            rationale: "model said X, should have said Y".into(),
            selector_text: Some("c".into()),
            created_at: String::new(),
            facet: None,
        });
        bank.save().unwrap();

        let reopened = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        assert_eq!(reopened.len(), 2);
        let (pos, cor, neg) = reopened.counts_by_kind();
        assert_eq!((pos, cor, neg), (1, 1, 0));
        let second = reopened.iter().nth(1).unwrap();
        assert!(second.model_output.is_some());
        assert!(second.corrected_output.is_some());
    }

    #[test]
    fn phase_mismatch_rejects_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(positive("a", "x"));
        bank.save().unwrap();

        let err = ExemplarBank::open(&path, PipelinePhase::Positions).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("declares phase"), "{msg}");
    }

    #[tokio::test]
    async fn select_top_k_returns_k_nearest_by_cosine() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(positive("near", "aligned"));
        bank.append(positive("far", "orthogonal"));
        bank.append(positive("medium", "partial"));

        // Deterministic "embed": map selector text to hand-picked vectors.
        let embed: EmbedFn = fake_embed(|s| match s {
            "aligned" => vec![1.0, 0.0, 0.0],
            "partial" => vec![0.7, 0.7, 0.0],
            "orthogonal" => vec![0.0, 1.0, 0.0],
            _ => vec![0.0, 0.0, 1.0],
        });
        bank.reembed(&embed).await.unwrap();

        let query = vec![1.0_f32, 0.0, 0.0];
        let picked = bank.select_top_k(&query, 2);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].id, "near");
        assert_eq!(picked[1].id, "medium");
    }

    #[tokio::test]
    async fn select_top_k_without_load_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(positive("a", "x"));
        // Did not call reembed.
        let picked = bank.select_top_k(&[1.0, 0.0, 0.0], 3);
        assert!(picked.is_empty());
    }

    #[test]
    fn append_stamps_created_at() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        let e = positive("a", "x");
        assert!(e.created_at.is_empty());
        bank.append(e);
        let stamped = bank.iter().next().unwrap();
        assert!(!stamped.created_at.is_empty());
    }

    #[tokio::test]
    async fn select_top_k_facet_filters_by_facet_tag() {
        // Two facet-tagged exemplars in the same bank, plus one
        // untagged. Facet-filtered selection picks only the
        // matching tag — the untagged exemplar is excluded even
        // when it would otherwise score highest.
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Concerns).unwrap();
        bank.append(Exemplar {
            facet: Some("claim".into()),
            ..positive("c1", "claim about love")
        });
        bank.append(Exemplar {
            facet: Some("question".into()),
            ..positive("q1", "question about love")
        });
        bank.append(positive("u1", "untagged about love"));
        bank.save().unwrap();

        let mut bank = ExemplarBank::open(&path, PipelinePhase::Concerns).unwrap();
        let embed = fake_embed(|_| vec![1.0_f32, 0.0, 0.0]);
        bank.reembed(&embed).await.unwrap();

        let query: Vec<f32> = vec![1.0, 0.0, 0.0];

        // Unfiltered select still returns all entries — backward
        // compatible with callers that don't care about facet.
        let all = bank.select_top_k(&query, 5);
        assert_eq!(all.len(), 3);

        // Facet-filtered select returns only the matching tag.
        let claim_only = bank.select_top_k_facet(&query, 5, Some("claim"));
        assert_eq!(claim_only.len(), 1);
        assert_eq!(claim_only[0].id, "c1");

        let question_only = bank.select_top_k_facet(&query, 5, Some("question"));
        assert_eq!(question_only.len(), 1);
        assert_eq!(question_only[0].id, "q1");

        // Asking for a facet with no tagged entries returns empty.
        let state_only = bank.select_top_k_facet(&query, 5, Some("entity_state"));
        assert!(state_only.is_empty());
    }

    #[test]
    fn exemplar_serde_roundtrip_preserves_facet_tag() {
        let e = Exemplar {
            facet: Some("entity_state".into()),
            ..positive("a", "x")
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"facet\":\"entity_state\""));
        let back: Exemplar = serde_json::from_str(&json).unwrap();
        assert_eq!(back.facet.as_deref(), Some("entity_state"));

        // Absent facet omits the field entirely — old bank files
        // stay valid wire format.
        let e_none = positive("b", "y");
        let json_none = serde_json::to_string(&e_none).unwrap();
        assert!(!json_none.contains("facet"));
    }

    #[test]
    fn lint_flags_empty_rationale() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(Exemplar {
            rationale: "".into(),
            ..positive("a", "x")
        });
        let lints = bank.lint();
        assert!(lints.iter().any(|l| l.reason.contains("rationale")));
    }

    #[test]
    fn lint_flags_duplicate_ids() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(positive("same", "x"));
        bank.append(positive("same", "y"));
        let lints = bank.lint();
        assert!(lints.iter().any(|l| l.reason.contains("duplicate id")));
    }

    #[test]
    fn lint_flags_corrected_without_model_output() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bank.json");
        let mut bank = ExemplarBank::open(&path, PipelinePhase::Questions).unwrap();
        bank.append(Exemplar {
            id: "c1".into(),
            kind: ExemplarKind::Corrected,
            input: serde_json::json!({}),
            output: None,
            model_output: None,
            corrected_output: Some(serde_json::json!({"fixed": true})),
            rationale: "why".into(),
            selector_text: None,
            created_at: String::new(),
            facet: None,
        });
        let lints = bank.lint();
        assert!(lints.iter().any(|l| l.reason.contains("model_output")));
    }

    #[test]
    fn selector_falls_back_to_input_json() {
        let e = Exemplar {
            id: "x".into(),
            kind: ExemplarKind::Positive,
            input: serde_json::json!({"k": "v"}),
            output: Some(serde_json::json!({})),
            model_output: None,
            corrected_output: None,
            rationale: "r".into(),
            selector_text: None,
            created_at: String::new(),
            facet: None,
        };
        let s = e.selector();
        assert!(s.contains("\"k\""));
    }

    #[test]
    fn cosine_handles_zero_vectors() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }
}
