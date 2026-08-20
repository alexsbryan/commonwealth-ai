// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

// ─── Reading Surface ─────────────────────────────────────────────────────────
//
// Backs the desktop's glass-box reading UI. Frontend calls
// `read_get_chunk_neighbors(corpus, chunkId, radius)` after the user
// clicks a citation; the response shape mirrors the HTTP routes in
// `sovereign-mesh::reading_http` so the same UI works against either
// the in-process daemon (this code path) or a remote daemon (HTTP).

#[derive(Serialize)]
pub struct ChunkRecordDto {
    pub chunk_id: u64,
    pub corpus_id: String,
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub source_doc_id: Option<String>,
    pub section_id: Option<String>,
    /// Atom mentions located in `content` — byte offsets into the
    /// chunk's text. Empty when the corpus has no atlas, when the
    /// chunk wasn't produced by a sectioned chunker, or when no
    /// atom is anchored at this section.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub atom_spans: Vec<AtomSpanDto>,
    pub metadata: serde_json::Value,
    /// Populated when `corpus_id == "conversation-history"`. The
    /// reading surface uses presence of this field to pick the
    /// conversation-shaped renderer over the default book renderer.
    /// Mirrors `ConversationChunkMeta` in the HTTP layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationChunkMetaDto>,
}

#[derive(Serialize)]
pub struct ConversationChunkMetaDto {
    pub conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<ConversationSegmentDto>,
}

#[derive(Serialize)]
pub struct ConversationSegmentDto {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct AtomSpanDto {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub span_start: usize,
    pub span_end: usize,
    pub surface_form: String,
}

impl From<corpus_engine::atlas_traversal::AtomSpan> for AtomSpanDto {
    fn from(s: corpus_engine::atlas_traversal::AtomSpan) -> Self {
        Self {
            atom_id: s.atom_id,
            atom_type: s.atom_type,
            span_start: s.span_start,
            span_end: s.span_end,
            surface_form: s.surface_form,
        }
    }
}

#[derive(Serialize)]
pub struct NeighborWindowDto {
    pub center: ChunkRecordDto,
    pub prev: Vec<ChunkRecordDto>,
    pub next: Vec<ChunkRecordDto>,
    pub outbound_url: Option<String>,
    pub ordering: &'static str,
}

fn chunk_record_dto_from_row(
    corpus_id: &str,
    row: &corpus_engine::EnrichmentChunkRow,
    atoms: Option<&[corpus_engine::enrichment::atlas::AtomEnvelope]>,
    conversation: Option<ConversationChunkMetaDto>,
) -> ChunkRecordDto {
    let metadata: serde_json::Value = row
        .metadata_raw
        .as_deref()
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let section_id = metadata
        .as_object()
        .and_then(|obj| obj.get("section_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let atom_spans: Vec<AtomSpanDto> = match (atoms, section_id.as_deref()) {
        (Some(atoms), Some(_)) => corpus_engine::atlas_traversal::detect_atom_spans(
            &row.content,
            section_id.as_deref(),
            atoms,
        )
        .into_iter()
        .map(AtomSpanDto::from)
        .collect(),
        _ => Vec::new(),
    };

    ChunkRecordDto {
        chunk_id: row.id,
        corpus_id: corpus_id.to_string(),
        content: row.content.clone(),
        title: row.title.clone(),
        url: row.url.clone(),
        source_doc_id: row.source_doc_id.clone(),
        section_id,
        atom_spans,
        metadata,
        conversation,
    }
}

const CONVERSATION_HISTORY_CORPUS_ID: &str = "conversation-history";

/// Same role-marker parser as the HTTP layer. Lives here in
/// duplicate (small, no shared crate available between mesh-http
/// and src-tauri) to keep the in-process Tauri path independent of
/// the HTTP path. Both shapes are wire-compatible.
fn parse_conversation_segments_dto(content: &str) -> Vec<ConversationSegmentDto> {
    if !content.starts_with('[') {
        return Vec::new();
    }
    let mut segments: Vec<ConversationSegmentDto> = Vec::new();
    let mut idx = 0usize;
    while idx < content.len() {
        if !content[idx..].starts_with('[') {
            break;
        }
        let role_close = match content[idx + 1..].find(']') {
            Some(rel) => idx + 1 + rel,
            None => break,
        };
        let role = content[idx + 1..role_close].to_string();
        let body_start = if content[role_close + 1..].starts_with(' ') {
            role_close + 2
        } else {
            role_close + 1
        };
        let body_end = match content[body_start..].find("\n\n[") {
            Some(rel) => body_start + rel,
            None => content.len(),
        };
        let body = content[body_start..body_end].to_string();
        if !role.is_empty() {
            segments.push(ConversationSegmentDto {
                role,
                content: body,
            });
        }
        idx = if body_end == content.len() {
            content.len()
        } else {
            body_end + 2
        };
    }
    segments
}

/// Resolve conversation metadata for a chunk via the SQLite store.
/// Returns `None` for non-conversation corpora and for conversation
/// chunks whose `source_doc_id` (= conversation_id) couldn't be
/// looked up. Errors are swallowed so the chunk still renders.
async fn maybe_resolve_conversation_meta_for_commands(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
    row: &corpus_engine::EnrichmentChunkRow,
) -> Option<ConversationChunkMetaDto> {
    if corpus_id != CONVERSATION_HISTORY_CORPUS_ID {
        return None;
    }
    let conversation_id = row.source_doc_id.clone()?;
    let segments = parse_conversation_segments_dto(&row.content);
    let store_arc = state.store.read().await.clone();
    let (title, updated_at) = match store_arc {
        Some(s) => match s.get_conversation(&conversation_id).await {
            Ok(c) => (c.title, Some(c.updated_at)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };
    Some(ConversationChunkMetaDto {
        conversation_id,
        title,
        updated_at,
        segments,
    })
}

/// Load atlas atoms for the corpus from `atlas/atoms.json` next to
/// the index. Returns `None` when no atlas is present (corpus
/// hasn't been enriched) or when the file is unreadable — the atom
/// layer no-ops gracefully rather than failing the chunk fetch.
async fn load_atlas_atoms_for_commands(
    engine: &Arc<corpus_engine::CorpusEngine>,
    corpus_id: &str,
) -> Option<Vec<corpus_engine::enrichment::atlas::AtomEnvelope>> {
    let installed = engine.installed_indexes().await.ok()?;
    let entry = installed.iter().find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = entry.path.join("atlas");
    if !atlas_dir.exists() {
        return None;
    }
    match corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir) {
        Ok(file) => Some(file.atoms),
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                ?atlas_dir,
                error = %e,
                "read_get_chunk_neighbors: atlas read failed; atom layer disabled",
            );
            None
        }
    }
}

/// In Attach mode the desktop owns no local corpus indexes — the CLI daemon
/// holds them. Route the reading surface to the daemon's loopback `reading_http`
/// routes (`/internal/corpus/...`, merged into the client router on
/// `client_port`). Those DTOs are byte-compatible with this module's `*Dto`
/// shapes (see this file's header: "the same UI works against either the
/// in-process daemon or a remote daemon (HTTP)"), so the daemon's JSON is
/// returned verbatim. A 404 (chunk/atom/corpus absent, or an older daemon
/// without the route) maps to `Ok(None)`, matching the in-process path.
async fn daemon_reading_get(
    base_url: &str,
    path: &str,
) -> Result<Option<serde_json::Value>, String> {
    let url = format!("{base_url}{path}");
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("daemon reading GET {url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("daemon reading {url} → {status}: {body}"));
    }
    resp.json::<serde_json::Value>()
        .await
        .map(Some)
        .map_err(|e| format!("daemon reading decode {url}: {e}"))
}

#[tauri::command]
pub async fn read_get_chunk(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
) -> Result<Option<serde_json::Value>, String> {
    if state.is_attach_mode() {
        return daemon_reading_get(
            &state.client_base_url(),
            &format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}"),
        )
        .await;
    }
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open index '{corpus_id}': {e}"))?;
    let mut rows = index
        .chunks_by_ids(&[chunk_id])
        .await
        .map_err(|e| format!("chunks_by_ids: {e}"))?;
    let atoms = load_atlas_atoms_for_commands(&engine, &corpus_id).await;
    let row_opt = rows.pop();
    let dto = match row_opt {
        Some(row) => {
            let conv = maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, &row).await;
            Some(chunk_record_dto_from_row(
                &corpus_id,
                &row,
                atoms.as_deref(),
                conv,
            ))
        }
        None => None,
    };
    dto.map(serde_json::to_value)
        .transpose()
        .map_err(|e| format!("serialize chunk: {e}"))
}

#[tauri::command]
pub async fn read_get_chunk_neighbors(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
    radius: Option<usize>,
) -> Result<Option<serde_json::Value>, String> {
    let radius = radius.unwrap_or(1).min(5);
    if state.is_attach_mode() {
        return daemon_reading_get(
            &state.client_base_url(),
            &format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}/neighbors?radius={radius}"),
        )
        .await;
    }
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open index '{corpus_id}': {e}"))?;
    let window = match index
        .neighbors(chunk_id, radius)
        .await
        .map_err(|e| format!("neighbors: {e}"))?
    {
        Some(w) => w,
        None => return Ok(None),
    };

    // Load the atlas once and reuse across all three chunks in
    // the window. atoms.json is small; per-chunk re-reads would
    // multiply IO without benefit.
    let atoms = load_atlas_atoms_for_commands(&engine, &corpus_id).await;
    let atoms_ref = atoms.as_deref();

    // Conversation augmentation per chunk. Cheap (one SQLite hit
    // per neighbor), and adjacent chunks tend to share a
    // conversation_id so the get_conversation cache hits hot.
    let center_conv =
        maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, &window.center).await;
    let center = chunk_record_dto_from_row(&corpus_id, &window.center, atoms_ref, center_conv);
    let outbound_url = center.url.clone();
    let mut prev: Vec<ChunkRecordDto> = Vec::with_capacity(window.prev.len());
    for r in &window.prev {
        let conv = maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, r).await;
        prev.push(chunk_record_dto_from_row(&corpus_id, r, atoms_ref, conv));
    }
    let mut next: Vec<ChunkRecordDto> = Vec::with_capacity(window.next.len());
    for r in &window.next {
        let conv = maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, r).await;
        next.push(chunk_record_dto_from_row(&corpus_id, r, atoms_ref, conv));
    }

    let dto = NeighborWindowDto {
        center,
        prev,
        next,
        outbound_url,
        ordering: window.ordering,
    };
    Ok(Some(
        serde_json::to_value(dto).map_err(|e| format!("serialize neighbors: {e}"))?,
    ))
}

// ─── Atom Panel ──────────────────────────────────────────────────────────────
//
// Two endpoints back the desktop's atom panel: `read_get_atom_card`
// returns the atom card (canonical_name, description, salience,
// one-hop relations, cross-corpus bridges) and
// `read_get_atom_elsewhere` returns the section list + cross-corpus
// links so the user can jump to other places the atom appears. The
// section→chunk projection happens here via
// `index.resolve_sections_to_chunks` so the desktop receives ready-
// to-click chunk_ids.

#[derive(Serialize)]
pub struct AtomCardDto {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub corpus_id: String,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub salience: Option<f32>,
    pub enrichment_depth: String,
    pub related: Vec<RelatedAtomDto>,
    pub cross_corpus: Vec<CrossCorpusLinkDto>,
}

#[derive(Serialize)]
pub struct RelatedAtomDto {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub canonical_name: String,
    pub edge_type: &'static str,
    pub role: &'static str,
    pub confidence: f32,
}

#[derive(Serialize)]
pub struct CrossCorpusLinkDto {
    pub peer_corpus_id: String,
    pub peer_atom_id: String,
    pub peer_canonical_name: String,
    pub edge_type: &'static str,
    pub signal: String,
    pub confidence: f32,
}

#[derive(Serialize)]
pub struct AtomElsewhereDto {
    pub atom_id: String,
    pub corpus_id: String,
    pub same_corpus: Vec<SectionRefDto>,
    pub cross_corpus: Vec<CrossCorpusLinkDto>,
}

#[derive(Serialize)]
pub struct SectionRefDto {
    pub section_id: String,
    pub chunk_id: Option<u64>,
    pub preview: Option<String>,
}

async fn atlas_dir_for_atom_commands(
    engine: &Arc<corpus_engine::CorpusEngine>,
    corpus_id: &str,
) -> Option<std::path::PathBuf> {
    let installed = engine.installed_indexes().await.ok()?;
    let entry = installed.iter().find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = entry.path.join("atlas");
    if atlas_dir.exists() {
        Some(atlas_dir)
    } else {
        None
    }
}

#[tauri::command]
pub async fn read_get_atom_card(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<serde_json::Value>, String> {
    if state.is_attach_mode() {
        return daemon_reading_get(
            &state.client_base_url(),
            &format!("/internal/corpus/{corpus_id}/atoms/{atom_id}"),
        )
        .await;
    }
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let Some(atlas_dir) = atlas_dir_for_atom_commands(&engine, &corpus_id).await else {
        return Ok(None);
    };
    let atoms = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?
        .atoms;
    let target = corpus_engine::enrichment::atlas::AtomId::from_raw(atom_id.clone());
    let Some(atom) = atoms.iter().find(|a| *a.id() == target) else {
        return Ok(None);
    };
    let edges = corpus_engine::enrichment::atlas::read_atlas_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross = corpus_engine::enrichment::atlas::read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    Ok(Some(
        serde_json::to_value(build_atom_card_dto(
            &corpus_id, atom, &atoms, &edges, &cross,
        ))
        .map_err(|e| format!("serialize atom card: {e}"))?,
    ))
}

#[tauri::command]
pub async fn read_get_atom_elsewhere(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<serde_json::Value>, String> {
    if state.is_attach_mode() {
        return daemon_reading_get(
            &state.client_base_url(),
            &format!("/internal/corpus/{corpus_id}/atoms/{atom_id}/elsewhere"),
        )
        .await;
    }
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let Some(atlas_dir) = atlas_dir_for_atom_commands(&engine, &corpus_id).await else {
        return Ok(None);
    };
    let atoms = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?
        .atoms;
    let target = corpus_engine::enrichment::atlas::AtomId::from_raw(atom_id.clone());
    let Some(atom) = atoms.iter().find(|a| *a.id() == target) else {
        return Ok(None);
    };

    let evidence = atom_evidence_section_refs_dto(atom);
    let unique_sections: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        evidence
            .iter()
            .map(|(s, _)| s.clone())
            .filter(|s| seen.insert(s.clone()))
            .collect()
    };

    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open index '{corpus_id}': {e}"))?;
    let section_to_chunk = index
        .resolve_sections_to_chunks(&unique_sections)
        .await
        .map_err(|e| format!("resolve_sections: {e}"))?;

    let mut same_corpus: Vec<SectionRefDto> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (section_id, preview) in &evidence {
        if !seen.insert(section_id.clone()) {
            continue;
        }
        same_corpus.push(SectionRefDto {
            section_id: section_id.clone(),
            chunk_id: section_to_chunk.get(section_id).copied(),
            preview: preview.clone(),
        });
    }
    same_corpus.sort_by(|a, b| a.section_id.cmp(&b.section_id));

    let cross = corpus_engine::enrichment::atlas::read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross_corpus = cross_corpus_links_dto(&target, &cross);

    let dto = AtomElsewhereDto {
        atom_id: target.as_str().to_string(),
        corpus_id,
        same_corpus,
        cross_corpus,
    };
    Ok(Some(
        serde_json::to_value(dto).map_err(|e| format!("serialize atom elsewhere: {e}"))?,
    ))
}

fn build_atom_card_dto(
    corpus_id: &str,
    atom: &corpus_engine::enrichment::atlas::AtomEnvelope,
    all_atoms: &[corpus_engine::enrichment::atlas::AtomEnvelope],
    edges: &[corpus_engine::enrichment::atlas::Edge],
    cross_edges: &[corpus_engine::enrichment::atlas::CrossCorpusEdge],
) -> AtomCardDto {
    let (atom_type, canonical_name, aliases, description, salience) = atom_surface_dto(atom);
    let target_id = atom.id();
    let related: Vec<RelatedAtomDto> = edges
        .iter()
        .filter(|e| e.source == *target_id || e.target == *target_id)
        .filter_map(|e| {
            let (other_id, role) = if e.source == *target_id {
                (&e.target, "source")
            } else {
                (&e.source, "target")
            };
            let other = all_atoms.iter().find(|a| *a.id() == *other_id)?;
            let (other_type, other_name, _, _, _) = atom_surface_dto(other);
            Some(RelatedAtomDto {
                atom_id: other_id.as_str().to_string(),
                atom_type: other_type,
                canonical_name: other_name,
                edge_type: e.edge_type.label(),
                role,
                confidence: e.confidence,
            })
        })
        .collect();
    let cross_corpus = cross_corpus_links_dto(target_id, cross_edges);
    AtomCardDto {
        atom_id: target_id.as_str().to_string(),
        atom_type,
        corpus_id: corpus_id.to_string(),
        canonical_name,
        aliases,
        description,
        salience,
        enrichment_depth: format!("{:?}", atom.enrichment_depth()),
        related,
        cross_corpus,
    }
}

fn atom_surface_dto(
    atom: &corpus_engine::enrichment::atlas::AtomEnvelope,
) -> (&'static str, String, Vec<String>, String, Option<f32>) {
    use corpus_engine::enrichment::atlas::AtomEnvelope;
    match atom {
        AtomEnvelope::Entity(e) => (
            "entity",
            e.canonical_name.clone(),
            e.aliases.clone(),
            e.description.clone(),
            Some(e.salience),
        ),
        AtomEnvelope::Event(e) => (
            "event",
            truncate_dto(&e.description, 80),
            Vec::new(),
            e.description.clone(),
            None,
        ),
        AtomEnvelope::State(s) => (
            "state",
            s.label.clone(),
            Vec::new(),
            format!("State of {}: {}", s.entity_id.as_str(), s.label),
            s.confidence,
        ),
        AtomEnvelope::Relation(r) => (
            "relation",
            r.label.clone(),
            Vec::new(),
            r.label.clone(),
            None,
        ),
        AtomEnvelope::Claim(c) => (
            "claim",
            truncate_dto(&c.content, 80),
            Vec::new(),
            c.content.clone(),
            c.confidence,
        ),
        AtomEnvelope::Question(q) => (
            "question",
            truncate_dto(&q.content, 80),
            Vec::new(),
            q.content.clone(),
            None,
        ),
        AtomEnvelope::Configuration(c) => (
            "configuration",
            c.label.clone(),
            Vec::new(),
            c.description.clone(),
            Some(c.confidence),
        ),
        AtomEnvelope::ArgumentReconstruction(a) => (
            "argument_reconstruction",
            a.name.clone(),
            Vec::new(),
            a.conclusion.clone(),
            None,
        ),
        AtomEnvelope::Position(p) => (
            "position",
            p.canonical_name.clone(),
            Vec::new(),
            p.content.clone(),
            Some(p.salience),
        ),
        AtomEnvelope::Opposition(o) => (
            "opposition",
            o.canonical_label.clone(),
            Vec::new(),
            if o.framing.is_empty() {
                format!("{} vs {}", o.left_label, o.right_label)
            } else {
                o.framing.clone()
            },
            Some(o.salience),
        ),
        AtomEnvelope::Asset(a) => (
            "asset",
            if a.original_filename.is_empty() {
                format!("{} asset", a.asset_kind)
            } else {
                a.original_filename.clone()
            },
            Vec::new(),
            format!(
                "{} ({} bytes); sha256:{}",
                a.asset_kind,
                a.size,
                &a.sha256[..16.min(a.sha256.len())]
            ),
            None,
        ),
    }
}

fn truncate_dto(s: &str, max_chars: usize) -> String {
    let trimmed: String = s.chars().take(max_chars).collect();
    if trimmed.chars().count() < s.chars().count() {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

fn atom_evidence_section_refs_dto(
    atom: &corpus_engine::enrichment::atlas::AtomEnvelope,
) -> Vec<(String, Option<String>)> {
    use corpus_engine::enrichment::atlas::AtomEnvelope;
    match atom {
        AtomEnvelope::Entity(e) => vec![(
            e.first_appearance.chunk_id.clone(),
            e.first_appearance.passage_preview.clone(),
        )],
        AtomEnvelope::Event(e) => {
            let mut out = vec![(e.section_position.section_id.clone(), None)];
            for c in &e.evidence {
                out.push((c.chunk_id.clone(), c.passage_preview.clone()));
            }
            out
        }
        AtomEnvelope::State(s) => s
            .evidence
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Relation(r) => r
            .evidence
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Claim(c) => c
            .evidence
            .iter()
            .map(|cr| (cr.chunk_id.clone(), cr.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Question(q) => q
            .raised_at
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Configuration(c) => c
            .evidence
            .iter()
            .map(|cr| (cr.chunk_id.clone(), cr.passage_preview.clone()))
            .collect(),
        AtomEnvelope::ArgumentReconstruction(a) => {
            let mut out = vec![(a.section_position.section_id.clone(), None)];
            for c in &a.evidence {
                out.push((c.chunk_id.clone(), c.passage_preview.clone()));
            }
            out
        }
        AtomEnvelope::Position(_) | AtomEnvelope::Opposition(_) => {
            unreachable!("typed atoms wired in Gap B Stage 4")
        }
        AtomEnvelope::Asset(_) => Vec::new(),
    }
}

fn cross_corpus_links_dto(
    atom_id: &corpus_engine::enrichment::atlas::AtomId,
    edges: &[corpus_engine::enrichment::atlas::CrossCorpusEdge],
) -> Vec<CrossCorpusLinkDto> {
    edges
        .iter()
        .filter(|e| e.edge.source == *atom_id || e.edge.target == *atom_id)
        .map(|e| CrossCorpusLinkDto {
            peer_corpus_id: e.peer.corpus_id.clone(),
            peer_atom_id: e.peer.atom_id.as_str().to_string(),
            peer_canonical_name: e.peer.canonical_name.clone(),
            edge_type: e.edge.edge_type.label(),
            signal: e.trace.signal.clone(),
            confidence: e.trace.confidence,
        })
        .collect()
}
