// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase D1 — atlas readiness summary.
//!
//! Single source of truth for "what's the atlas state of every
//! installed corpus on this node?". Consumed by:
//!
//!   - `sovereign corpus status` CLI (`mesh_cmd::cmd_corpus_status`).
//!   - `/internal/atlas/status` HTTP endpoint, which the desktop's
//!     Knowledge-readiness panel polls.
//!   - Future: `sovereign atlas status` standalone CLI command for
//!     scriptable queries.
//!
//! All readers use [`compute_atlas_status`] to ensure the CLI table,
//! the gossip-advertised fields, and the desktop panel never drift.

use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{read_or_compute_atlas_summary, AtlasSummary};
use serde::{Deserialize, Serialize};

/// One corpus's atlas readiness snapshot. Serializes cleanly so the
/// HTTP endpoint and the desktop panel share the same wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasStatusRow {
    pub corpus_id: String,
    /// `None` when the corpus has no `atlas/atoms.json` yet (fresh
    /// install before the post-install structural pass completes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas: Option<AtlasSummary>,
    /// True when `atlas/atoms.embeddings.bin` exists. The cache is
    /// what makes runtime atlas-grounded retrieval cheap; missing
    /// means the next chat turn will skip atlas grounding for this
    /// corpus until the cache rebuilds.
    pub embed_cache_present: bool,
    /// Cumulative tokens spent in the corpus's `<corpus>-tier2`
    /// workspace's most recent extract run (Phase D2). `None` when
    /// no `_tokens.json` sidecar exists yet — Tier-2 hasn't run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier2_tokens: Option<TokenSummary>,
    /// True when a `<corpus>-tier2` workspace exists with the auto-
    /// managed sentinel — i.e. the post-install hook spawned a
    /// background extractor that may still be running. Lets the
    /// desktop show a "tier-2 enrichment in progress" indicator.
    pub tier2_in_progress: bool,
    /// `<corpus>-tier2/runs/_phase1_checkpoint.jsonl` chapter count
    /// vs. `<indexes>/<corpus>-tier2/chapters.json` total. `None`
    /// when no workspace exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier2_progress: Option<Tier2Progress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSummary {
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier2Progress {
    pub chapters_done: usize,
    pub chapters_total: usize,
}

/// Walk every directory under `indexes_dir` and return one
/// [`AtlasStatusRow`] per installed corpus. Cheap — uses cached
/// atlas summaries (`<atlas_dir>/_summary.json`) so wiki-scale
/// atoms.json files don't get reparsed on each call.
///
/// `enrichment_dir` is typically `<data_dir>/enrichment` and is the
/// sibling of `indexes_dir`. Used to find Tier-2 workspaces and
/// their token sidecars.
pub fn compute_atlas_status(indexes_dir: &Path, enrichment_dir: &Path) -> Vec<AtlasStatusRow> {
    let mut rows = Vec::new();
    let entries = match std::fs::read_dir(indexes_dir) {
        Ok(rd) => rd,
        Err(_) => return rows,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        // Skip `<corpus>-tier2` workspace mirror dirs that live
        // under indexes/ (chapters.json is created there by `enrich
        // init`); they're not atlases themselves.
        if name.ends_with("-tier2") {
            continue;
        }
        rows.push(compute_one(name, &path, enrichment_dir));
    }
    rows.sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
    rows
}

fn compute_one(corpus_id: &str, corpus_dir: &Path, enrichment_dir: &Path) -> AtlasStatusRow {
    let atlas_dir = corpus_dir.join("atlas");
    let atlas = read_or_compute_atlas_summary(&atlas_dir).ok().flatten();
    let embed_cache_present = atlas_dir.join("atoms.embeddings.bin").exists();

    let workspace_dir = enrichment_dir.join(format!("{corpus_id}-tier2"));
    let tier2_in_progress = workspace_dir.join(".auto_managed").exists();
    let tier2_tokens = read_token_summary(&workspace_dir.join("_tokens.json"));
    let tier2_progress = read_tier2_progress(corpus_id, &workspace_dir, corpus_dir.parent());

    AtlasStatusRow {
        corpus_id: corpus_id.to_string(),
        atlas,
        embed_cache_present,
        tier2_tokens,
        tier2_in_progress,
        tier2_progress,
    }
}

fn read_token_summary(path: &Path) -> Option<TokenSummary> {
    let raw = std::fs::read_to_string(path).ok()?;
    #[derive(Deserialize)]
    struct WireRecord {
        schema_version: u32,
        calls: u64,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        started_at_ms: u64,
        updated_at_ms: u64,
    }
    let r: WireRecord = serde_json::from_str(&raw).ok()?;
    if r.schema_version != 1 {
        return None;
    }
    Some(TokenSummary {
        calls: r.calls,
        prompt_tokens: r.prompt_tokens,
        completion_tokens: r.completion_tokens,
        total_tokens: r.total_tokens,
        started_at_ms: r.started_at_ms,
        updated_at_ms: r.updated_at_ms,
    })
}

fn read_tier2_progress(
    corpus_id: &str,
    workspace_dir: &Path,
    indexes_dir: Option<&Path>,
) -> Option<Tier2Progress> {
    let chapters_manifest = indexes_dir?
        .join(format!("{corpus_id}-tier2"))
        .join("chapters.json");
    let (done, total) =
        crate::atlas_postinstall::checkpoint_progress(workspace_dir, &chapters_manifest)?;
    Some(Tier2Progress {
        chapters_done: done,
        chapters_total: total,
    })
}

/// Look up just one corpus's status. Convenience wrapper for the
/// CLI's `sovereign atlas status <corpus>` mode.
pub fn status_for_corpus(
    indexes_dir: &Path,
    enrichment_dir: &Path,
    corpus_id: &str,
) -> Option<AtlasStatusRow> {
    let corpus_dir = indexes_dir.join(corpus_id);
    if !corpus_dir.is_dir() {
        return None;
    }
    Some(compute_one(corpus_id, &corpus_dir, enrichment_dir))
}

/// Default data-dir layout helper — convenient for callers that
/// want to compute paths from a single root.
pub fn default_paths(data_dir: PathBuf) -> (PathBuf, PathBuf) {
    (data_dir.join("indexes"), data_dir.join("enrichment"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_indexes_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = compute_atlas_status(tmp.path(), tmp.path());
        assert!(rows.is_empty());
    }

    #[test]
    fn skips_tier2_workspace_mirrors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("wikipedia")).unwrap();
        std::fs::create_dir_all(tmp.path().join("wikipedia-tier2")).unwrap();
        let rows = compute_atlas_status(tmp.path(), tmp.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].corpus_id, "wikipedia");
    }

    #[test]
    fn records_atlas_summary_when_present() {
        use corpus_engine::enrichment::atlas::atoms::{
            AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity,
        };
        use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wikipedia").join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let atoms = AtomsFile::new(vec![AtomEnvelope::Entity(Entity {
            id: AtomId::entity(1),
            canonical_name: "Earth".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })]);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&atoms).unwrap(),
        )
        .unwrap();

        let rows = compute_atlas_status(tmp.path(), tmp.path());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].atlas.is_some());
        assert_eq!(rows[0].atlas.as_ref().unwrap().tier2_count, 1);
        assert!(!rows[0].embed_cache_present);
    }
}
