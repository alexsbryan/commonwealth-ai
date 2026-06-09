// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disk-backed reader for atlas inspection.
//!
//! `FileAtlasReader` is the *only* path the desktop atlas-inspector
//! UI uses to reach atlas data. Phase 2 (curation overlay) will add
//! overlay-merging branches inside this same struct — no new trait,
//! no new caller-facing seam. See the module-level docs in
//! `atlas_view/mod.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::atoms::AtomType;
use corpus_engine::enrichment::atlas::{read_or_compute_atlas_summary, ATLAS_DIRNAME};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One row in the corpus picker.
///
/// `display_name` is the corpus_id today; a future change can hydrate
/// it from `IndexInfo.corpus_name` for friendlier rendering. The wire
/// shape exists now so we don't break the UI when that lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasCorpusSummary {
    pub corpus_id: String,
    pub display_name: String,
    pub total_atoms: u64,
    /// Per-type atom counts. `BTreeMap` so the JSON order is
    /// deterministic — easier diffing in tests and friendlier to UI
    /// snapshot comparison.
    pub atom_counts: BTreeMap<AtomType, u64>,
    /// `atoms.json` mtime in seconds since the Unix epoch. Closest
    /// proxy we have for "when was extraction last run for this
    /// corpus" without a separate provenance file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_extracted_unix: Option<u64>,
    /// Logical UI category from the recipe's `[display]` block —
    /// drives the Atlas View rail grouping (`category =
    /// "conversation"` collapses every conversation-source corpus
    /// under one "Conversations" header). `None` on legacy indexes
    /// that pre-date the `[display]` block — the frontend buckets
    /// those into an "Other" group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_category: Option<String>,
    /// Icon hint from the recipe's `[display]` block. Free-form
    /// string the frontend maps onto its icon set. `None` falls back
    /// to a generic glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_icon: Option<String>,
}

/// Forward-compat field on per-atom DTOs. Phase 1 always emits
/// [`CurationStatus::Generated`]; Phase 2 starts populating the other
/// variants. Putting the field through the wire today means the UI
/// can wire a `<CurationStatusBadge>` once and have it light up when
/// Phase 2 ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurationStatus {
    /// Came straight from extraction. No human has reviewed it.
    Generated,
}

/// Errors surfaced by [`FileAtlasReader`]. Per-corpus read failures
/// inside [`FileAtlasReader::list_corpora`] are logged via tracing
/// and skipped rather than raised — a single corrupt atlas should
/// not block the whole picker. Other entry points propagate.
#[derive(Debug, Error)]
pub enum AtlasViewError {
    #[error("indexes dir not readable: {0}")]
    IndexesDir(#[source] std::io::Error),
}

/// File-system-backed atlas reader.
///
/// Cheap to construct (holds a single path). Each method re-reads
/// the relevant `atoms.json` from disk; there is no in-process cache.
/// At the inspection rates this drives (a human clicking through a
/// list), the re-read cost is irrelevant and the simplicity is worth
/// more than the throughput.
#[derive(Debug, Clone)]
pub struct FileAtlasReader {
    indexes_dir: PathBuf,
}

impl FileAtlasReader {
    /// Build a reader rooted at the corpus indexes directory
    /// (typically `<data_dir>/indexes`, the same path
    /// `compute_atlas_status` walks).
    pub fn new(indexes_dir: PathBuf) -> Self {
        Self { indexes_dir }
    }

    /// Resolve the on-disk atlas directory for a given corpus.
    /// Returns `None` when the corpus dir doesn't exist or has no
    /// `atlas/` subdirectory yet.
    pub fn atlas_dir(&self, corpus_id: &str) -> Option<PathBuf> {
        let dir = self.indexes_dir.join(corpus_id).join(ATLAS_DIRNAME);
        if dir.is_dir() {
            Some(dir)
        } else {
            None
        }
    }

    /// Return one [`AtlasCorpusSummary`] per installed corpus that
    /// has an atlas on disk. Sorted by `corpus_id` for stable
    /// rendering. Corpora without an atlas (fresh installs, pure
    /// catalog entries) are omitted — the inspector has nothing to
    /// show for them.
    ///
    /// Per-corpus summaries are pulled from corpus-engine's cached
    /// `_summary.json` sidecar (`read_or_compute_atlas_summary`).
    /// First read of each atlas pays the deserialisation cost once;
    /// subsequent reads are an O(KB) cache hit. The N corpus reads
    /// fan out onto the blocking-task pool — for a fleet of
    /// installed atlases, picker latency is now `max(per-corpus)`
    /// instead of `sum(per-corpus)`.
    pub async fn list_corpora(&self) -> Result<Vec<AtlasCorpusSummary>, AtlasViewError> {
        let entries = match std::fs::read_dir(&self.indexes_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    indexes_dir = %self.indexes_dir.display(),
                    "atlas_view:list_corpora: indexes_dir absent, returning empty",
                );
                return Ok(Vec::new());
            }
            Err(e) => return Err(AtlasViewError::IndexesDir(e)),
        };

        // Collect candidate (corpus_id, atlas_dir) pairs synchronously
        // — the `read_dir` walk is cheap and serialising it sidesteps
        // ordering / Send concerns on the entries iterator.
        let mut candidates: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Same filtering as `compute_atlas_status` — internal /
            // tier-2 mirror dirs aren't corpora.
            if name.starts_with('.') || name.starts_with('_') {
                continue;
            }
            if name.ends_with("-tier2") {
                continue;
            }
            let atlas_dir = path.join(ATLAS_DIRNAME);
            if !atlas_dir.is_dir() {
                continue;
            }
            candidates.push((name.to_string(), atlas_dir));
        }

        // Fan out the per-corpus reads onto the blocking pool. Each
        // task either hits a fresh _summary.json (microseconds) or
        // pays the one-time atoms.json deserialisation cost — which
        // also writes the sidecar so subsequent calls hit the hot
        // path. Either way the reads proceed concurrently.
        let mut handles = Vec::with_capacity(candidates.len());
        for (corpus_id, atlas_dir) in candidates {
            let h = tokio::task::spawn_blocking(move || {
                let result = summarise_corpus(&corpus_id, &atlas_dir);
                (corpus_id, atlas_dir, result)
            });
            handles.push(h);
        }

        let mut summaries = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok((_, _, Ok(summary))) => summaries.push(summary),
                Ok((corpus_id, atlas_dir, Err(e))) => {
                    // Glassbox: a corrupt atoms.json on one corpus
                    // shouldn't take out the whole picker, but the
                    // operator needs to know it happened.
                    tracing::warn!(
                        corpus_id = %corpus_id,
                        atlas_dir = %atlas_dir.display(),
                        error = %e,
                        "atlas_view:list_corpora: skipping corpus, summary unreadable",
                    );
                }
                Err(join_err) => {
                    tracing::warn!(
                        error = %join_err,
                        "atlas_view:list_corpora: per-corpus task panicked or was cancelled",
                    );
                }
            }
        }

        summaries.sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
        tracing::debug!(
            corpus_count = summaries.len(),
            "atlas_view:list_corpora: enumerated installed atlases",
        );
        Ok(summaries)
    }
}

fn summarise_corpus(
    corpus_id: &str,
    atlas_dir: &Path,
) -> Result<AtlasCorpusSummary, std::io::Error> {
    let (display_category, display_icon) = read_display_meta(atlas_dir);

    // Hot path: a v2 `_summary.json` exists and matches the live
    // atoms.json cache key — returns in microseconds. Cold path:
    // cache miss recomputes once and writes the sidecar, so
    // subsequent calls hit the hot path.
    let summary = match read_or_compute_atlas_summary(atlas_dir)? {
        Some(s) => s,
        None => {
            // No atoms.json on disk yet. The walk already filtered
            // out corpora without an `atlas/` dir, so this is a
            // mid-bootstrap corpus (atlas dir exists but extraction
            // hasn't written atoms.json yet). Return a zero-atom row
            // so the picker shows "extraction not yet run" rather
            // than hiding the corpus entirely.
            return Ok(AtlasCorpusSummary {
                corpus_id: corpus_id.to_string(),
                display_name: corpus_id.to_string(),
                total_atoms: 0,
                atom_counts: BTreeMap::new(),
                last_extracted_unix: None,
                display_category,
                display_icon,
            });
        }
    };

    // Cache key carries atoms.json mtime in milliseconds; convert
    // to seconds for the wire (UI shows minute-resolution timestamps).
    let last_extracted_unix = if summary.atoms_mtime_ms > 0 {
        Some(summary.atoms_mtime_ms / 1000)
    } else {
        None
    };

    Ok(AtlasCorpusSummary {
        corpus_id: corpus_id.to_string(),
        // Phase 1: display_name == corpus_id. Hydrate from
        // `IndexInfo.corpus_name` in a later pass — the field is in
        // the wire shape so the desktop doesn't need to change then.
        display_name: corpus_id.to_string(),
        total_atoms: summary.atom_count,
        atom_counts: summary.atom_counts,
        last_extracted_unix,
        display_category,
        display_icon,
    })
}

/// Read the `[display]` block from `<index_dir>/_corpus_meta.json`
/// (the parent of the atlas directory). Returns
/// `(category, icon)` — either or both `None` on legacy indexes
/// pre-dating the field, malformed meta, or missing file.
///
/// Light-weight: parses just enough JSON to extract the two fields
/// rather than pulling in the full `IndexMeta` deserialiser (which
/// the corpus-engine crate keeps `pub(crate)`).
fn read_display_meta(atlas_dir: &Path) -> (Option<String>, Option<String>) {
    let Some(index_dir) = atlas_dir.parent() else {
        return (None, None);
    };
    let meta_path = index_dir.join("_corpus_meta.json");
    let Ok(raw) = std::fs::read(&meta_path) else {
        return (None, None);
    };
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default)]
        display: Option<DisplayProbe>,
    }
    #[derive(serde::Deserialize)]
    struct DisplayProbe {
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        icon: Option<String>,
    }
    match serde_json::from_slice::<Probe>(&raw) {
        Ok(p) => match p.display {
            Some(d) => (d.category, d.icon),
            None => (None, None),
        },
        Err(_) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{
        AtomEnvelope, AtomId, AtomsFile, ChunkRef, Claim, Entity,
    };
    use corpus_engine::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus,
    };
    use tempfile::TempDir;

    fn write_atoms(atlas_dir: &Path, atoms: Vec<AtomEnvelope>) {
        std::fs::create_dir_all(atlas_dir).unwrap();
        let file = AtomsFile::new(atoms);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    fn sample_entity(id: usize, name: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(id),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    fn sample_claim(id: usize, content: &str) -> AtomEnvelope {
        AtomEnvelope::Claim(Claim {
            id: AtomId::claim(id),
            content: content.into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
        })
    }

    fn make_reader() -> (TempDir, FileAtlasReader) {
        let tmp = tempfile::tempdir().unwrap();
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        (tmp, reader)
    }

    #[tokio::test]
    async fn list_corpora_returns_empty_when_indexes_dir_missing() {
        let reader = FileAtlasReader::new(PathBuf::from("/this/path/does/not/exist/xyz"));
        let summaries = reader.list_corpora().await.unwrap();
        assert!(summaries.is_empty());
    }

    #[tokio::test]
    async fn list_corpora_skips_corpora_without_atlas() {
        let (tmp, reader) = make_reader();
        // Two corpora — only one has an atlas.
        std::fs::create_dir_all(tmp.path().join("plain-corpus")).unwrap();
        write_atoms(
            &tmp.path().join("wikipedia").join("atlas"),
            vec![sample_entity(1, "Earth")],
        );
        let summaries = reader.list_corpora().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].corpus_id, "wikipedia");
    }

    #[tokio::test]
    async fn list_corpora_counts_atoms_by_type() {
        let (tmp, reader) = make_reader();
        write_atoms(
            &tmp.path().join("sep-epistemology").join("atlas"),
            vec![
                sample_entity(1, "Knowledge"),
                sample_entity(2, "Belief"),
                sample_claim(1, "Knowledge is justified true belief."),
            ],
        );
        let summaries = reader.list_corpora().await.unwrap();
        assert_eq!(summaries.len(), 1);
        let s = &summaries[0];
        assert_eq!(s.corpus_id, "sep-epistemology");
        assert_eq!(s.total_atoms, 3);
        assert_eq!(s.atom_counts.get(&AtomType::Entity).copied(), Some(2));
        assert_eq!(s.atom_counts.get(&AtomType::Claim).copied(), Some(1));
        // Untouched types are absent from the map (not zero) —
        // BTreeMap deserialises cleanly either way.
        assert!(!s.atom_counts.contains_key(&AtomType::Question));
    }

    #[tokio::test]
    async fn list_corpora_sorts_alphabetically() {
        let (tmp, reader) = make_reader();
        for name in ["zeta", "alpha", "mu"] {
            write_atoms(
                &tmp.path().join(name).join("atlas"),
                vec![sample_entity(1, name)],
            );
        }
        let summaries = reader.list_corpora().await.unwrap();
        let ids: Vec<&str> = summaries.iter().map(|s| s.corpus_id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
    }

    #[tokio::test]
    async fn list_corpora_skips_tier2_mirror_dirs() {
        let (tmp, reader) = make_reader();
        write_atoms(
            &tmp.path().join("sep-mind").join("atlas"),
            vec![sample_entity(1, "Mind")],
        );
        // Tier-2 workspace mirror — has an atlas-shaped dir but
        // shouldn't surface as a corpus.
        write_atoms(
            &tmp.path().join("sep-mind-tier2").join("atlas"),
            vec![sample_entity(1, "Mind")],
        );
        let summaries = reader.list_corpora().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].corpus_id, "sep-mind");
    }

    #[tokio::test]
    async fn list_corpora_skips_dotted_and_underscored_dirs() {
        let (tmp, reader) = make_reader();
        for name in [".scratch", "_tmp"] {
            write_atoms(
                &tmp.path().join(name).join("atlas"),
                vec![sample_entity(1, name)],
            );
        }
        write_atoms(
            &tmp.path().join("real-corpus").join("atlas"),
            vec![sample_entity(1, "Real")],
        );
        let summaries = reader.list_corpora().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].corpus_id, "real-corpus");
    }

    #[tokio::test]
    async fn list_corpora_skips_corpus_with_corrupt_atoms_json() {
        let (tmp, reader) = make_reader();
        // Good corpus.
        write_atoms(
            &tmp.path().join("good").join("atlas"),
            vec![sample_entity(1, "Good")],
        );
        // Corrupt corpus — atoms.json exists but isn't valid JSON.
        let bad_atlas = tmp.path().join("bad").join("atlas");
        std::fs::create_dir_all(&bad_atlas).unwrap();
        std::fs::write(bad_atlas.join("atoms.json"), b"{not json").unwrap();
        // Reader logs a warning and skips the bad one, doesn't fail.
        let summaries = reader.list_corpora().await.unwrap();
        let ids: Vec<&str> = summaries.iter().map(|s| s.corpus_id.as_str()).collect();
        assert_eq!(ids, vec!["good"]);
    }

    #[tokio::test]
    async fn list_corpora_uses_cached_summary_on_repeat_calls() {
        // Pins the perf win: after the first call writes _summary.json,
        // a follow-up call can read it even if atoms.json becomes
        // unreadable. Atoms.json is hidden between calls to prove the
        // cache is the data source, not the live file.
        let (tmp, reader) = make_reader();
        let atlas_dir = tmp.path().join("wikipedia").join("atlas");
        write_atoms(
            &atlas_dir,
            vec![sample_entity(1, "Earth"), sample_entity(2, "Mars")],
        );
        let first = reader.list_corpora().await.unwrap();
        assert_eq!(first[0].total_atoms, 2);
        assert!(atlas_dir.join("_summary.json").exists());

        // Make atoms.json unreadable by replacing it with garbage,
        // but keep its mtime + size identical so the cache key
        // still matches. The summary cache should still satisfy.
        let original = std::fs::metadata(atlas_dir.join("atoms.json")).unwrap();
        std::fs::write(
            atlas_dir.join("atoms.json"),
            vec![0u8; original.len() as usize],
        )
        .unwrap();
        filetime::set_file_mtime(
            atlas_dir.join("atoms.json"),
            filetime::FileTime::from_system_time(original.modified().unwrap()),
        )
        .unwrap();
        let second = reader.list_corpora().await.unwrap();
        assert_eq!(second[0].total_atoms, 2);
        assert_eq!(
            second[0].atom_counts.get(&AtomType::Entity).copied(),
            Some(2)
        );
    }

    #[test]
    fn atlas_dir_resolves_known_corpus() {
        let (tmp, reader) = make_reader();
        write_atoms(
            &tmp.path().join("wikipedia").join("atlas"),
            vec![sample_entity(1, "Earth")],
        );
        let dir = reader.atlas_dir("wikipedia").expect("atlas dir resolves");
        assert!(dir.ends_with(Path::new("wikipedia/atlas")));
    }

    #[test]
    fn atlas_dir_returns_none_for_corpus_without_atlas() {
        let (tmp, reader) = make_reader();
        std::fs::create_dir_all(tmp.path().join("plain")).unwrap();
        assert!(reader.atlas_dir("plain").is_none());
        assert!(reader.atlas_dir("nonexistent").is_none());
    }

    #[test]
    fn atlas_corpus_summary_serialises_cleanly() {
        // The Tauri layer relies on this DTO crossing the IPC
        // boundary. Pin the wire shape so a refactor doesn't
        // silently break the desktop.
        let summary = AtlasCorpusSummary {
            corpus_id: "wikipedia".into(),
            display_name: "wikipedia".into(),
            total_atoms: 3,
            atom_counts: BTreeMap::from([(AtomType::Entity, 2), (AtomType::Claim, 1)]),
            last_extracted_unix: Some(1_700_000_000),
            display_category: Some("reference".into()),
            display_icon: Some("book".into()),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"corpus_id\":\"wikipedia\""));
        assert!(json.contains("\"total_atoms\":3"));
        assert!(json.contains("\"display_category\":\"reference\""));
        let back: AtlasCorpusSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, summary);
    }

    #[test]
    fn display_block_in_corpus_meta_is_read_when_present() {
        let tmp = TempDir::new().unwrap();
        let atlas_dir = tmp.path().join("conversations-anthropic").join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        // Minimal `_corpus_meta.json` with the `[display]` block populated.
        std::fs::write(
            atlas_dir.parent().unwrap().join("_corpus_meta.json"),
            r#"{
                "corpus_id": "conversations-anthropic",
                "corpus_name": "Claude conversations",
                "embedding_model": "qwen-embedding-0.6b",
                "embedding_dimensions": 768,
                "mesh_sharing": false,
                "license": "private",
                "created_at": 0,
                "last_updated": 0,
                "display": { "category": "conversation", "icon": "chat-bubble" }
            }"#,
        )
        .unwrap();
        let (category, icon) = read_display_meta(&atlas_dir);
        assert_eq!(category.as_deref(), Some("conversation"));
        assert_eq!(icon.as_deref(), Some("chat-bubble"));
    }

    #[test]
    fn display_block_absent_returns_none_pair() {
        let tmp = TempDir::new().unwrap();
        let atlas_dir = tmp.path().join("legacy").join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        std::fs::write(
            atlas_dir.parent().unwrap().join("_corpus_meta.json"),
            r#"{"corpus_id":"legacy"}"#,
        )
        .unwrap();
        let (category, icon) = read_display_meta(&atlas_dir);
        assert!(category.is_none());
        assert!(icon.is_none());
    }
}
