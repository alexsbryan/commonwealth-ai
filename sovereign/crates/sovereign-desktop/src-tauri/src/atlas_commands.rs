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
    AtlasCorpusSummary, AtomDetail, AtomFilter, AtomListPage, ConvCorpusSummary, ConvDetailView,
    ConvEntityChip, ConvListPage, ConvRaptorNodeView, ConvSummary, FileAtlasReader, PageCursor,
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

// ── Conversation tiered-retrieval commands (spec CONV_TIERED_PORT.md
//    §"Retrieval surface — A1/A2") ──────────────────────────────────
//
// Conv corpora never wrote atoms.json — their tiered enrichment lives
// in the `conv_skeletons` / `conv_raptor_nodes` / `conv_motifs` SQLite
// sidecar tables. These commands read via the
// `Arc<SqliteStateStore>` stashed at desktop bootstrap. AtlasIndex
// calls BOTH atlas_list_corpora (atoms.json) and
// atlas_list_conv_corpora (these), then merges client-side.

/// Display-name + icon lookup for a conv corpus_id. Best-effort —
/// pulls from the corpus registry when reachable, falls back to the
/// corpus_id itself. Doesn't fail if recipe registry isn't loaded
/// (some test paths skip the engine bootstrap).
async fn conv_display_metadata(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
) -> (String, Option<String>, Option<String>) {
    let mut display_name = corpus_id.to_string();
    let mut category: Option<String> = None;
    let mut icon: Option<String> = None;
    if let Some(engine) = state.corpus_engine.read().await.as_ref() {
        if let Ok(infos) = engine.installed_indexes().await {
            if let Some(info) = infos.iter().find(|i| i.corpus_id == corpus_id) {
                if !info.corpus_name.is_empty() {
                    display_name = info.corpus_name.clone();
                }
                if let Some(d) = &info.display {
                    category = d.category.clone();
                    icon = d.icon.clone();
                }
            }
        }
    }
    (display_name, category, icon)
}

/// List every conv corpus with at least one row in `conv_skeletons`,
/// plus its state-bucket counts. Drives the desktop Atlas index
/// "Conversations" group.
#[tauri::command]
pub async fn atlas_list_conv_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ConvCorpusSummary>, String> {
    let store = match state.sqlite_store.read().await.as_ref() {
        Some(s) => Arc::clone(s),
        None => return Err("Sqlite store not initialised".into()),
    };
    let buckets = store
        .list_conv_corpora_with_state_buckets()
        .await
        .map_err(|e| format!("atlas_list_conv_corpora: {e}"))?;
    let mut out = Vec::with_capacity(buckets.len());
    for (corpus_id, total, max_ts, per_state) in buckets {
        let (display_name, category, icon) = conv_display_metadata(&state, &corpus_id).await;
        let mut state_counts = std::collections::BTreeMap::new();
        for (state_name, n) in per_state {
            state_counts.insert(state_name, n);
        }
        out.push(ConvCorpusSummary {
            corpus_id,
            display_name,
            conv_count: total,
            state_counts,
            last_updated_unix: if max_ts > 0 { Some(max_ts) } else { None },
            display_category: category,
            display_icon: icon,
        });
    }
    Ok(out)
}

/// Paginated list of conversations in one corpus, filterable by
/// substring on `overview`. Page size capped at 200 to match the
/// existing atoms-list pagination.
#[tauri::command]
pub async fn atlas_list_conversations(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    filter: Option<String>,
    offset: Option<u64>,
) -> Result<ConvListPage, String> {
    let store = match state.sqlite_store.read().await.as_ref() {
        Some(s) => Arc::clone(s),
        None => return Err("Sqlite store not initialised".into()),
    };
    let limit: u64 = 200;
    let offset = offset.unwrap_or(0);
    let filter_str = filter.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (rows, total) = store
        .list_conversations_paginated(&corpus_id, filter_str, offset, limit)
        .await
        .map_err(|e| format!("atlas_list_conversations: {e}"))?;
    let mut conversations = Vec::with_capacity(rows.len());
    for row in rows {
        let raptor = store
            .list_conv_raptor_nodes(&corpus_id, &row.conv_uuid)
            .await
            .unwrap_or_default();
        let chunk_count = row.chunk_count;
        let (top_entities, is_tiny) = summarize_entities(&raptor, 6);
        conversations.push(ConvSummary {
            conv_uuid: row.conv_uuid,
            title: row
                .overview
                .clone()
                .unwrap_or_else(|| "(untitled conversation)".to_string()),
            state: row.state,
            chunk_count,
            top_entities,
            updated_at: row.updated_at,
            is_tiny,
        });
    }
    let next_offset = if (offset + conversations.len() as u64) < total {
        Some(offset + conversations.len() as u64)
    } else {
        None
    };
    Ok(ConvListPage {
        conversations,
        total_matching: total,
        next_offset,
    })
}

/// Full conversation detail: skeleton + RAPTOR tree. Drives the
/// ConvDetail.svelte component (tree view).
#[tauri::command]
pub async fn atlas_get_conv_detail(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    conv_uuid: String,
) -> Result<Option<ConvDetailView>, String> {
    let store = match state.sqlite_store.read().await.as_ref() {
        Some(s) => Arc::clone(s),
        None => return Err("Sqlite store not initialised".into()),
    };
    let skeleton = match store
        .get_conv_skeleton(&corpus_id, &conv_uuid)
        .await
        .map_err(|e| format!("atlas_get_conv_detail.skeleton: {e}"))?
    {
        Some(s) => s,
        None => return Ok(None),
    };
    let nodes = store
        .list_conv_raptor_nodes(&corpus_id, &conv_uuid)
        .await
        .map_err(|e| format!("atlas_get_conv_detail.nodes: {e}"))?;
    let max_level = nodes.iter().map(|n| n.level as u8).max().unwrap_or(0);
    let raptor_nodes: Vec<ConvRaptorNodeView> = nodes
        .into_iter()
        .map(|n| {
            let primary_entities: Vec<String> =
                serde_json::from_str(&n.primary_entities_json).unwrap_or_default();
            let direct_member_chunk_ids: Vec<u64> = n
                .direct_member_chunk_ids_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let evidence_chunk_ids: Vec<u64> =
                serde_json::from_str(&n.evidence_chunk_ids_json).unwrap_or_default();
            let is_synthetic_tiny =
                primary_entities.is_empty() && (n.cluster_coherence - 1.0).abs() < 1e-6;
            ConvRaptorNodeView {
                node_id: n.node_id,
                level: n.level as u8,
                summary: n.summary,
                primary_entities,
                direct_member_chunk_ids,
                evidence_chunk_count: evidence_chunk_ids.len(),
                cluster_coherence: n.cluster_coherence,
                is_synthetic_tiny,
            }
        })
        .collect();
    Ok(Some(ConvDetailView {
        corpus_id,
        conv_uuid,
        title: skeleton
            .overview
            .clone()
            .unwrap_or_else(|| "(untitled conversation)".to_string()),
        state: skeleton.state,
        chunk_count: skeleton.chunk_count,
        updated_at: skeleton.updated_at,
        raptor_nodes,
        max_level,
    }))
}

/// GliNER model availability + path for the Settings → Imports
/// surface. Returns whether the configured model is installed +
/// the expected on-disk path so the UI can show "Install model"
/// vs "Re-download" affordances. Spec: Phase 1 model UX.
#[derive(serde::Serialize, Clone)]
pub struct GlinerModelStatus {
    pub installed: bool,
    pub model_id: String,
    pub expected_path: String,
    pub size_estimate_mb: u64,
}

#[tauri::command]
pub async fn atlas_check_gliner_model() -> Result<GlinerModelStatus, String> {
    let model_id = sovereign_tools::gliner_ner::DEFAULT_MODEL_ID.to_string();
    let installed = sovereign_tools::gliner_ner::probe_model_available(&model_id);
    let expected_path = sovereign_tools::gliner_ner::models_root()
        .join(&model_id)
        .display()
        .to_string();
    Ok(GlinerModelStatus {
        installed,
        model_id,
        expected_path,
        // Empirical: gliner_small-v2.1 = ~600MB (ONNX f32 + tokenizer).
        size_estimate_mb: 600,
    })
}

/// Kicks off a model download. Streams progress via Tauri events
/// on the channel `gliner-download-progress` (payload: `{ file,
/// downloaded, total }`). Returns when the download completes or
/// errors. Idempotent: skips files already present.
#[tauri::command]
pub async fn atlas_download_gliner_model(
    app: tauri::AppHandle,
    model_id: Option<String>,
) -> Result<(), String> {
    use tauri::Emitter;
    let model_id =
        model_id.unwrap_or_else(|| sovereign_tools::gliner_ner::DEFAULT_MODEL_ID.to_string());
    let app_for_cb = app.clone();
    let on_progress = move |file: &str, downloaded: u64, total: u64| {
        let _ = app_for_cb.emit(
            "gliner-download-progress",
            serde_json::json!({
                "file": file,
                "downloaded": downloaded,
                "total": total,
            }),
        );
    };
    sovereign_tools::gliner_ner::download_model(&model_id, on_progress)
        .await
        .map_err(|e| format!("atlas_download_gliner_model: {e}"))?;
    let _ = app.emit(
        "gliner-download-progress",
        serde_json::json!({ "file": "__complete__", "downloaded": 0u64, "total": 0u64 }),
    );
    Ok(())
}

/// Aggregate one entity's footprint inside a corpus. Powers the
/// Atlas-view entity drawer: click an `entity-chip`, the UI invokes
/// this to render mention/conv counts, label breakdown, top convs,
/// and co-occurring entities. Matches `text` case-insensitively so
/// the drawer collapses casing variance but splits homonyms by label.
#[tauri::command]
pub async fn atlas_get_entity_aggregate(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    text: String,
) -> Result<sovereign_core::conv_tiered::EntityAggregateRow, String> {
    let store = match state.sqlite_store.read().await.as_ref() {
        Some(s) => Arc::clone(s),
        None => return Err("Sqlite store not initialised".into()),
    };
    // Drawer hard-caps. Co-occurring 20 fits a single scroll-free
    // column; top-convs 10 covers a "where it appears" tail with
    // room for an explicit "view all" affordance later.
    store
        .aggregate_entity(&corpus_id, &text, 20, 10)
        .await
        .map_err(|e| format!("atlas_get_entity_aggregate: {e}"))
}

/// Per-corpus entity-extraction progress. Drives the AtlasIndex
/// "X% extracted" badge that appears alongside per-state enrichment
/// counts while extraction is running.
#[tauri::command]
pub async fn atlas_get_chunk_entity_progress(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<sovereign_core::conv_tiered::ChunkEntityProgressRow>, String> {
    let store = match state.sqlite_store.read().await.as_ref() {
        Some(s) => Arc::clone(s),
        None => return Err("Sqlite store not initialised".into()),
    };
    store
        .get_chunk_entity_progress(&corpus_id)
        .await
        .map_err(|e| format!("atlas_get_chunk_entity_progress: {e}"))
}

/// Top-N entity chips for one conversation (A2). Drives the entity
/// chip row above `ConversationChunkRenderer`'s message bubbles.
/// Tiny convs return an empty list — the UI suppresses the chip row.
#[tauri::command]
pub async fn atlas_get_conv_entities(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    conv_uuid: String,
) -> Result<Vec<ConvEntityChip>, String> {
    let store = match state.sqlite_store.read().await.as_ref() {
        Some(s) => Arc::clone(s),
        None => return Err("Sqlite store not initialised".into()),
    };
    let nodes = store
        .list_conv_raptor_nodes(&corpus_id, &conv_uuid)
        .await
        .map_err(|e| format!("atlas_get_conv_entities: {e}"))?;
    Ok(rank_entity_chips(&nodes, 12))
}

/// Salience-rank entities for both A1's `top_entities` and A2's
/// chip row. Returns the top-N entities sorted by salience desc,
/// salience = sum of `cluster_coherence` over nodes containing the
/// entity. Also returns whether the conv is "Tiny" (single
/// synthetic node, no LLM-extracted entities).
fn summarize_entities(
    nodes: &[sovereign_store::sqlite::ConvRaptorNodeRow],
    top_n: usize,
) -> (Vec<String>, bool) {
    let chips = rank_entity_chips(nodes, top_n);
    let is_tiny = nodes.len() == 1
        && (nodes[0].cluster_coherence - 1.0).abs() < 1e-6
        && nodes[0].primary_entities_json.trim() == "[]";
    (chips.into_iter().map(|c| c.name).collect(), is_tiny)
}

fn rank_entity_chips(
    nodes: &[sovereign_store::sqlite::ConvRaptorNodeRow],
    top_n: usize,
) -> Vec<ConvEntityChip> {
    use std::collections::HashMap;
    let mut acc: HashMap<String, (f32, u32)> = HashMap::new();
    for node in nodes {
        let entities: Vec<String> =
            serde_json::from_str(&node.primary_entities_json).unwrap_or_default();
        for ent in entities {
            let trimmed = ent.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry = acc.entry(trimmed.to_string()).or_insert((0.0, 0));
            entry.0 += node.cluster_coherence as f32;
            entry.1 += 1;
        }
    }
    let mut ranked: Vec<(String, f32, u32)> = acc
        .into_iter()
        .map(|(name, (sal, occ))| (name, sal, occ))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
        .into_iter()
        .take(top_n)
        .map(|(name, salience, occurrence_count)| ConvEntityChip {
            name,
            salience,
            occurrence_count,
        })
        .collect()
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
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            concept_kind: None,
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
