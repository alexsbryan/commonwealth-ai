//! Tauri commands for the desktop's Atlas Inspector surface.
//!
//! Read-only browsing today (Phase 1): list corpora that have an
//! atlas, list/filter atoms within one corpus (Step 3), inspect a
//! single atom (Step 4). Phase 2 will grow curation-edit commands
//! here — overlay reads ride on `sovereign_tools::atlas_view`'s
//! existing methods, so this module stays the only Tauri surface for
//! atlas inspection.
//!
//! These commands live outside `commands.rs` deliberately:
//! `commands.rs` is already the workspace's largest file (§3.3 in
//! sovereign/ARCH_PRINCIPLES.md), and atlas inspection is a distinct
//! concern from the "reading from a citation" flow that owns the
//! `read_*` commands.

use std::sync::Arc;

use sovereign_tools::atlas_view::{
    AtlasCorpusSummary, AtomDetail, AtomFilter, AtomListPage, FileAtlasReader, PageCursor,
};
use tauri::State;

use crate::state::AppState;

/// List every installed corpus that has an atlas on disk. Drives the
/// desktop's `/atlas` index route — one row per corpus, with
/// per-atom-type counts so the type tabs can show badges before the
/// user clicks in.
#[tauri::command]
pub async fn atlas_list_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<AtlasCorpusSummary>, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    reader
        .list_corpora()
        .await
        .map_err(|e| format!("atlas_list_corpora: {e}"))
}

/// Browse atoms within one corpus — filterable by type, searchable
/// by display name, paginated. Drives the desktop's per-corpus
/// inspector view.
///
/// Heavy first call (atoms.json deserialisation) is cached
/// in-process; subsequent filter/search changes are served from the
/// cached vec. Mtime + size key on atoms.json invalidates the cache
/// automatically when extraction reruns.
#[tauri::command]
pub async fn atlas_list_atoms(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    filter: Option<AtomFilter>,
    page: Option<PageCursor>,
) -> Result<AtomListPage, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    reader
        .list_atoms(
            &corpus_id,
            filter.unwrap_or_default(),
            page.unwrap_or_default(),
        )
        .await
        .map_err(|e| format!("atlas_list_atoms: {e}"))
}

/// Full inspector record for one atom — full type-specific shape +
/// one-hop related atoms + cross-corpus bridges + evidence
/// excerpts. Drives the desktop's `AtomDetail.svelte`.
///
/// After `FileAtlasReader` produces the detail, this command
/// resolves the section ids on every evidence excerpt to numeric
/// chunk ids via `index.resolve_sections_to_chunks` — the same path
/// `read_get_atom_elsewhere` uses. The frontend then renders
/// evidence rows as clickable, opening the ReadingSurface centered
/// on the chunk.
///
/// Returns `Ok(None)` when the atom id isn't present in the corpus's
/// atoms.json (stale UI link, or extraction renumbered atom_ids
/// since the last list_atoms call).
#[tauri::command]
pub async fn atlas_get_atom_detail(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<AtomDetail>, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    let mut detail = match reader
        .get_atom_detail(&corpus_id, &atom_id)
        .await
        .map_err(|e| format!("atlas_get_atom_detail: {e}"))?
    {
        Some(d) => d,
        None => return Ok(None),
    };

    // Resolve section_id → numeric chunk_id so evidence rows can
    // deep-link to the ReadingSurface. Best-effort: a resolution
    // failure on one section logs a warning but doesn't fail the
    // whole detail — the row just stays non-clickable.
    let unique_sections: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        detail
            .evidence_excerpts
            .iter()
            .filter_map(|e| {
                if seen.insert(e.section_id.clone()) {
                    Some(e.section_id.clone())
                } else {
                    None
                }
            })
            .collect()
    };
    if !unique_sections.is_empty() {
        match engine.open_index_for_corpus(&corpus_id).await {
            Ok(index) => match index.resolve_sections_to_chunks(&unique_sections).await {
                Ok(section_to_chunk) => {
                    for excerpt in &mut detail.evidence_excerpts {
                        excerpt.chunk_id = section_to_chunk.get(&excerpt.section_id).copied();
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        corpus_id = %corpus_id,
                        error = %e,
                        "atlas_get_atom_detail: resolve_sections_to_chunks failed; evidence rows non-clickable",
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    corpus_id = %corpus_id,
                    error = %e,
                    "atlas_get_atom_detail: open_index_for_corpus failed; evidence rows non-clickable",
                );
            }
        }
    }
    Ok(Some(detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{
        AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity,
    };
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn write_atoms(atlas_dir: &std::path::Path, atoms: Vec<AtomEnvelope>) {
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
            concept_kind: None,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
        })
    }

    // The Tauri command itself can't be unit-tested without a full
    // AppState (which owns a corpus engine + runtime). The
    // command's two ingredients ARE separately testable:
    //
    //   - FileAtlasReader::list_corpora — covered by 7 tests in
    //     sovereign_tools::atlas_view::reader::tests.
    //   - The DTO serialisation — covered by
    //     `atlas_corpus_summary_serialises_cleanly` in the same
    //     module.
    //
    // This test pins the *wire-level* end-to-end behaviour without
    // a Tauri runtime: build a FileAtlasReader against a tempdir
    // fixture (mimicking what the command does internally) and
    // verify the JSON the desktop receives.

    #[tokio::test]
    async fn atlas_list_corpora_returns_serialisable_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(
            &tmp.path().join("wikipedia").join("atlas"),
            vec![sample_entity(1, "Earth"), sample_entity(2, "Mars")],
        );
        write_atoms(
            &tmp.path().join("sep-mind").join("atlas"),
            vec![sample_entity(1, "Consciousness")],
        );
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let summaries = reader.list_corpora().await.expect("list_corpora succeeds");
        // What the Tauri command Ok-arm returns:
        let wire = serde_json::to_value(&summaries).unwrap();
        assert!(wire.is_array());
        let arr = wire.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // Sorted alphabetically — sep-mind first.
        assert_eq!(arr[0]["corpus_id"], "sep-mind");
        assert_eq!(arr[0]["total_atoms"], 1);
        assert_eq!(arr[1]["corpus_id"], "wikipedia");
        assert_eq!(arr[1]["total_atoms"], 2);
    }
}
