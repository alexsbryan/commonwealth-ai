// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading-surface HTTP — `/internal/corpus/{corpus}/chunks/...`
//! and `/internal/corpus/{corpus}/atoms/...`.
//!
//! Backs the desktop's glass-box reading experience: when the user
//! clicks a citation, the desktop fetches the cited chunk + its
//! immediate textual neighbors here; when the user clicks a typed
//! term in the reading surface (an "atom"), it fetches that atom's
//! card + a list of where else the atom appears.
//!
//! All routes are loopback-only. Two layers of enforcement, mirroring
//! `admin_http`:
//! 1. Router-level [`crate::loopback_guard::loopback_only`] middleware.
//! 2. Per-handler `enforce_localhost`.
//!
//! v1 scope (per the glass-box reading-surface plan):
//! - `GET /internal/corpus/{corpus}/chunks/{chunk_id}` — single chunk
//! - `GET /internal/corpus/{corpus}/chunks/{chunk_id}/neighbors?radius=N`
//!   — center chunk plus up to N prev/next within the same source_doc
//! - Atom routes ship in PR3/PR4 once the AtomSpan detector lands.
//!
//! Section-bounded reading (layer-2) is intentionally deferred — see
//! ENRICHMENT_V2 §"Layer-2 section reading deferred" — and shows up
//! here as "neighbors are id-ordered within source_doc_id" rather than
//! "neighbors are bounded by section."

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use corpus_engine::atlas_traversal::{detect_atom_spans, AtomSpan as DetectorAtomSpan};
use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_cross_corpus_edges, read_atlas_edges, AtomEnvelope, AtomId,
    CrossCorpusEdge, Edge,
};
use corpus_engine::EnrichmentChunkRow;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

// ─── Response shapes ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChunkRecord {
    pub chunk_id: u64,
    pub corpus_id: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_doc_id: Option<String>,
    /// Section identifier extracted from chunk metadata when the
    /// `sectioned` chunker was used. The AtomSpan detector joins
    /// atom evidence (`section_id`-anchored) to chunk text via
    /// this field; non-sectioned chunks have `None` and the atom
    /// layer no-ops gracefully on the frontend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    /// Atom mentions located inside this chunk's `content`. Empty
    /// when no atlas exists for the corpus, when the chunk has no
    /// section_id, or when no atom is anchored at this section.
    /// Each span carries byte offsets into `content`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub atom_spans: Vec<AtomSpan>,
    /// Raw extractor metadata (parsed JSON object). Empty object
    /// when the chunk has no metadata. Surfaced verbatim so the
    /// desktop can display extractor-specific fields without the
    /// HTTP layer needing to know about every extractor's shape.
    pub metadata: serde_json::Value,
    /// Populated only when `corpus_id == "conversation-history"`.
    /// The reading surface reads this to render conversation chunks
    /// as role-tagged segments instead of book paragraphs and to
    /// expose a "View conversation" jump back to the chat. `None`
    /// for every other corpus (book / SEP / Wikipedia / catalog).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationChunkMeta>,
}

/// Conversation-shaped metadata derived from a
/// `conversation-history` corpus chunk. The chunk's content is the
/// recipe-built `[role] message\n\n[role] message…` string, so the
/// segments here are produced by parsing that delimiter — no
/// schema change in the underlying corpus, just a frontend-friendly
/// view of the same bytes.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationChunkMeta {
    /// The owning conversation's id. Equal to `source_doc_id` on
    /// the chunk; surfaced explicitly so the desktop can wire the
    /// "View conversation" button without re-deriving from
    /// `source_doc_id` (which is "untyped" — could mean different
    /// things for different corpora).
    pub conversation_id: String,
    /// User-or-system-set conversation title, when available.
    /// Resolved at request time from the `conversations` table via
    /// the daemon's `StateStore`. `None` means the conversation has
    /// no title yet (auto-titling pending) or the lookup failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Last-modified epoch seconds, sourced from the same store
    /// lookup. `None` when the lookup is unavailable. The desktop
    /// uses this to render a "Last updated <date>" line in the
    /// breadcrumb.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Role-tagged segments parsed from the chunk content. Empty
    /// when the chunk's content doesn't carry the recipe's
    /// `[role] …\n\n[role] …` format (defensive — degrades to
    /// raw-text rendering on the frontend).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<ConversationSegment>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConversationSegment {
    /// Either `"user"`, `"assistant"`, or `"system"`. The recipe
    /// writes whatever the messages table holds in `role`; we don't
    /// reinterpret beyond preserving the raw value.
    pub role: String,
    pub content: String,
}

/// Wire shape mirrors `corpus_engine::atlas_traversal::AtomSpan`.
#[derive(Debug, Clone, Serialize)]
pub struct AtomSpan {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub span_start: usize,
    pub span_end: usize,
    pub surface_form: String,
}

impl From<DetectorAtomSpan> for AtomSpan {
    fn from(s: DetectorAtomSpan) -> Self {
        Self {
            atom_id: s.atom_id,
            atom_type: s.atom_type,
            span_start: s.span_start,
            span_end: s.span_end,
            surface_form: s.surface_form,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NeighborWindowResponse {
    pub center: ChunkRecord,
    pub prev: Vec<ChunkRecord>,
    pub next: Vec<ChunkRecord>,
    /// Outbound link to the canonical document, when present
    /// (mirrors `center.url`). The desktop surfaces this as the
    /// "Read the full source" footer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outbound_url: Option<String>,
    /// How neighbors were resolved. Currently always
    /// `"id_within_source_doc"`; future section-anchored ordering
    /// will set a different discriminator.
    pub ordering: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct NeighborQuery {
    /// Maximum number of prev / next chunks to include on each
    /// side. Clamped to `[0, 5]` — the reading surface only ever
    /// shows the immediately-adjacent paragraphs in v1.
    #[serde(default = "default_radius")]
    pub radius: usize,
}

fn default_radius() -> usize {
    1
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

// ─── Atom card / elsewhere shapes ──────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AtomCard {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub corpus_id: String,
    pub canonical_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub description: String,
    pub salience: Option<f32>,
    /// Enrichment depth — `"Extracted"` / `"Structural"` /
    /// `"StructuralClassified"`. Drives language calibration in
    /// the brief assembler; surfaced here so the desktop can
    /// calibrate its own framing.
    pub enrichment_depth: String,
    /// One-hop edges from this atom — for an entity, the relations
    /// + states + claims + events that mention it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedAtom>,
    /// Cross-corpus bridges this atom participates in. Empty when
    /// no cross-corpus edges exist for this corpus.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_corpus: Vec<CrossCorpusLink>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedAtom {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub canonical_name: String,
    pub edge_type: &'static str,
    /// `"source"` if this atom is the edge source, `"target"` if
    /// the target. Used by the desktop to phrase the relationship
    /// in the right direction.
    pub role: &'static str,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrossCorpusLink {
    pub peer_corpus_id: String,
    pub peer_atom_id: String,
    pub peer_canonical_name: String,
    pub edge_type: &'static str,
    pub signal: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AtomElsewhere {
    pub atom_id: String,
    pub corpus_id: String,
    /// Sections in this corpus where the atom appears, with a
    /// resolved chunk_id when one could be located. Sorted by
    /// section_id.
    pub same_corpus: Vec<SectionRef>,
    pub cross_corpus: Vec<CrossCorpusLink>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionRef {
    pub section_id: String,
    /// First chunk id in this section, when resolvable. `None`
    /// means the section_id is in atom evidence but no chunk in
    /// the index carries it (legacy ingest, partial reshard).
    /// The desktop should grey out the row in this case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u64>,
    /// Short preview text from the atom evidence's
    /// passage_preview, when populated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

// ─── Router ────────────────────────────────────────────────────

pub fn reading_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route(
            "/internal/corpus/{corpus}/chunks/{chunk_id}",
            get(get_chunk),
        )
        .route(
            "/internal/corpus/{corpus}/chunks/{chunk_id}/neighbors",
            get(get_neighbors),
        )
        .route(
            "/internal/corpus/{corpus}/atoms/{atom_id}",
            get(get_atom_card),
        )
        .route(
            "/internal/corpus/{corpus}/atoms/{atom_id}/elsewhere",
            get(get_atom_elsewhere),
        )
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

// ─── Handlers ──────────────────────────────────────────────────

async fn get_chunk(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, chunk_id)): Path<(String, u64)>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let engine = match daemon.corpus_engine().await {
        Some(e) => e,
        None => return service_unavailable("corpus engine not initialised"),
    };
    let index = match engine.open_index_for_corpus(&corpus).await {
        Ok(i) => i,
        Err(e) => return not_found(&format!("corpus open: {e}")),
    };
    let mut rows = match index.chunks_by_ids(&[chunk_id]).await {
        Ok(r) => r,
        Err(e) => return internal_error(&format!("chunks_by_ids: {e}")),
    };
    let Some(row) = rows.pop() else {
        return not_found("chunk not found");
    };
    let atlas_atoms = load_atlas_atoms(&engine, &corpus).await;
    let conv = maybe_resolve_conversation_meta(&daemon, &corpus, &row).await;
    let record = chunk_record_from_row_with_conv(&corpus, &row, atlas_atoms.as_deref(), conv);
    (StatusCode::OK, Json(record)).into_response()
}

async fn get_neighbors(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, chunk_id)): Path<(String, u64)>,
    Query(NeighborQuery { radius }): Query<NeighborQuery>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let radius = radius.min(5);
    let engine = match daemon.corpus_engine().await {
        Some(e) => e,
        None => return service_unavailable("corpus engine not initialised"),
    };
    let index = match engine.open_index_for_corpus(&corpus).await {
        Ok(i) => i,
        Err(e) => return not_found(&format!("corpus open: {e}")),
    };
    let window = match index.neighbors(chunk_id, radius).await {
        Ok(Some(w)) => w,
        Ok(None) => return not_found("chunk not found"),
        Err(e) => return internal_error(&format!("neighbors: {e}")),
    };

    // Load atlas atoms once and reuse across all three chunks in
    // the window. atoms.json is small (hundreds of atoms on BK);
    // re-reading per chunk would just multiply IO without benefit.
    let atlas_atoms = load_atlas_atoms(&engine, &corpus).await;
    let atoms_ref = atlas_atoms.as_deref();

    // Conversation augmentation: resolve once per chunk in the
    // window. The store lookup is keyed on `source_doc_id` so
    // adjacent paragraphs in the same conversation incur the same
    // (cheap) SQLite hit; we don't bother memoising across the
    // three-chunk radius.
    let center_conv = maybe_resolve_conversation_meta(&daemon, &corpus, &window.center).await;
    let center = chunk_record_from_row_with_conv(&corpus, &window.center, atoms_ref, center_conv);
    let outbound_url = center.url.clone();
    let mut prev_records = Vec::with_capacity(window.prev.len());
    for r in &window.prev {
        let conv = maybe_resolve_conversation_meta(&daemon, &corpus, r).await;
        prev_records.push(chunk_record_from_row_with_conv(&corpus, r, atoms_ref, conv));
    }
    let prev = prev_records;
    let mut next_records = Vec::with_capacity(window.next.len());
    for r in &window.next {
        let conv = maybe_resolve_conversation_meta(&daemon, &corpus, r).await;
        next_records.push(chunk_record_from_row_with_conv(&corpus, r, atoms_ref, conv));
    }
    let next = next_records;

    let response = NeighborWindowResponse {
        center,
        prev,
        next,
        outbound_url,
        ordering: window.ordering,
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn get_atom_card(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, atom_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let engine = match daemon.corpus_engine().await {
        Some(e) => e,
        None => return service_unavailable("corpus engine not initialised"),
    };
    let Some((atlas_dir, _index_path)) = atlas_dir_for_corpus(&engine, &corpus).await else {
        return not_found("corpus not installed or atlas missing");
    };

    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(file) => file.atoms,
        Err(e) => return internal_error(&format!("read atoms: {e}")),
    };
    let target_id = AtomId::from_raw(atom_id.clone());
    let Some(atom) = atoms.iter().find(|a| *a.id() == target_id) else {
        return not_found("atom not found");
    };

    let edges = read_atlas_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross_edges = read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();

    let card = build_atom_card(&corpus, atom, &atoms, &edges, &cross_edges);
    (StatusCode::OK, Json(card)).into_response()
}

async fn get_atom_elsewhere(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, atom_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let engine = match daemon.corpus_engine().await {
        Some(e) => e,
        None => return service_unavailable("corpus engine not initialised"),
    };
    let index = match engine.open_index_for_corpus(&corpus).await {
        Ok(i) => i,
        Err(e) => return not_found(&format!("corpus open: {e}")),
    };
    let Some((atlas_dir, _)) = atlas_dir_for_corpus(&engine, &corpus).await else {
        return not_found("corpus not installed or atlas missing");
    };

    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(file) => file.atoms,
        Err(e) => return internal_error(&format!("read atoms: {e}")),
    };
    let target_id = AtomId::from_raw(atom_id.clone());
    let Some(atom) = atoms.iter().find(|a| *a.id() == target_id) else {
        return not_found("atom not found");
    };

    let evidence = atom_evidence_section_refs(atom);
    let unique_sections: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        evidence
            .iter()
            .map(|(s, _)| s.clone())
            .filter(|s| seen.insert(s.clone()))
            .collect()
    };

    let section_to_chunk = match index.resolve_sections_to_chunks(&unique_sections).await {
        Ok(m) => m,
        Err(e) => return internal_error(&format!("resolve_sections: {e}")),
    };

    let mut same_corpus: Vec<SectionRef> = Vec::new();
    let mut seen_sections = std::collections::HashSet::new();
    for (section_id, preview) in &evidence {
        if !seen_sections.insert(section_id.clone()) {
            continue;
        }
        same_corpus.push(SectionRef {
            section_id: section_id.clone(),
            chunk_id: section_to_chunk.get(section_id).copied(),
            preview: preview.clone(),
        });
    }
    same_corpus.sort_by(|a, b| a.section_id.cmp(&b.section_id));

    let cross_edges = read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross_corpus = cross_corpus_links_for_atom(&target_id, &cross_edges);

    let response = AtomElsewhere {
        atom_id: target_id.as_str().to_string(),
        corpus_id: corpus,
        same_corpus,
        cross_corpus,
    };
    (StatusCode::OK, Json(response)).into_response()
}

// ─── Helpers ───────────────────────────────────────────────────

/// Load this corpus's atlas atoms.json once per request. Returns
/// `None` when the corpus has no atlas (no atom layer to surface)
/// or when reading the atoms file fails — both degrade silently to
/// "no atom spans" rather than failing the whole reading-surface
/// fetch.
async fn load_atlas_atoms(
    engine: &Arc<corpus_engine::CorpusEngine>,
    corpus_id: &str,
) -> Option<Vec<AtomEnvelope>> {
    let (atlas_dir, _) = atlas_dir_for_corpus(engine, corpus_id).await?;
    match read_atlas_atoms(&atlas_dir) {
        Ok(file) => Some(file.atoms),
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                atlas_dir = ?atlas_dir,
                error = %e,
                "reading_http: atlas atoms.json read failed; atom layer disabled for this chunk",
            );
            None
        }
    }
}

/// Resolve `(atlas_dir, index_dir)` for a corpus, when both exist.
async fn atlas_dir_for_corpus(
    engine: &Arc<corpus_engine::CorpusEngine>,
    corpus_id: &str,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let installed = engine.installed_indexes().await.ok()?;
    let entry = installed.iter().find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = entry.path.join("atlas");
    if !atlas_dir.exists() {
        return None;
    }
    Some((atlas_dir, entry.path.clone()))
}

/// Convert an `AtomEnvelope` to the wire shape: render atom-type
/// label + extract surface fields. Mirrors the
/// `atlas_traversal::spans::atom_type_label` mapping; kept inline
/// here to avoid leaking the spans module's pub seam.
fn build_atom_card(
    corpus_id: &str,
    atom: &AtomEnvelope,
    all_atoms: &[AtomEnvelope],
    edges: &[Edge],
    cross_edges: &[CrossCorpusEdge],
) -> AtomCard {
    let atom_type = atom_type_label(atom);
    let (canonical_name, aliases, description, salience) = atom_surface_fields(atom);

    let target_id = atom.id();
    let related: Vec<RelatedAtom> = edges
        .iter()
        .filter(|e| e.source == *target_id || e.target == *target_id)
        .filter_map(|e| {
            let (other_id, role) = if e.source == *target_id {
                (&e.target, "source")
            } else {
                (&e.source, "target")
            };
            let other = all_atoms.iter().find(|a| *a.id() == *other_id)?;
            let (other_name, _, _, _) = atom_surface_fields(other);
            Some(RelatedAtom {
                atom_id: other_id.as_str().to_string(),
                atom_type: atom_type_label(other),
                canonical_name: other_name,
                edge_type: edge_type_label(e.edge_type),
                role,
                confidence: e.confidence,
            })
        })
        .collect();

    let cross_corpus = cross_corpus_links_for_atom(target_id, cross_edges);

    AtomCard {
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

use crate::reading_formatters::{
    atom_evidence_section_refs, atom_surface_fields, atom_type_label, edge_type_label,
};

fn cross_corpus_links_for_atom(
    atom_id: &AtomId,
    edges: &[CrossCorpusEdge],
) -> Vec<CrossCorpusLink> {
    edges
        .iter()
        .filter(|e| e.edge.source == *atom_id || e.edge.target == *atom_id)
        .map(|e| CrossCorpusLink {
            peer_corpus_id: e.peer.corpus_id.clone(),
            peer_atom_id: e.peer.atom_id.as_str().to_string(),
            peer_canonical_name: e.peer.canonical_name.clone(),
            edge_type: edge_type_label(e.edge.edge_type),
            signal: e.trace.signal.clone(),
            confidence: e.trace.confidence,
        })
        .collect()
}

pub(crate) fn chunk_record_from_row_with_conv(
    corpus_id: &str,
    row: &EnrichmentChunkRow,
    atoms: Option<&[AtomEnvelope]>,
    conversation: Option<ConversationChunkMeta>,
) -> ChunkRecord {
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

    let atom_spans = match (atoms, section_id.as_deref()) {
        (Some(atoms), Some(_)) => detect_atom_spans(&row.content, section_id.as_deref(), atoms)
            .into_iter()
            .map(AtomSpan::from)
            .collect(),
        _ => Vec::new(),
    };

    ChunkRecord {
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

/// The `conversation-history` corpus_id is special-cased on the
/// reading surface. Centralised here so handlers don't repeat the
/// magic string and a future view-id rename has one site to update.
pub(crate) const CONVERSATION_HISTORY_CORPUS_ID: &str = "conversation-history";

/// Parse the recipe's `[role] msg\n\n[role] msg…` chunk content
/// into role-tagged segments. Returns an empty vector when the
/// content doesn't carry the leading `[role]` marker, so the
/// frontend can fall back to plain rendering on legacy / non-
/// conversation chunks misclassified as conversation.
pub(crate) fn parse_conversation_segments(content: &str) -> Vec<ConversationSegment> {
    // Recipe writes the marker at the start of every line that
    // begins a message. Splitting on `\n\n[` (after stripping the
    // initial `[`) recovers the per-message blocks robustly even
    // when a message body contains lone `[` characters.
    if !content.starts_with('[') {
        return Vec::new();
    }
    let mut segments: Vec<ConversationSegment> = Vec::new();
    // Walk the content message-by-message. Each message begins at
    // `[` and runs until the next `\n\n[` (or end-of-string).
    let mut idx = 0usize;
    while idx < content.len() {
        if !content[idx..].starts_with('[') {
            break;
        }
        // Find the closing `]` of the role tag.
        let role_close = match content[idx + 1..].find(']') {
            Some(rel) => idx + 1 + rel,
            None => break, // malformed — bail
        };
        let role = content[idx + 1..role_close].to_string();
        // Body starts after `] ` (the recipe inserts a space) but
        // we accept `]` alone too.
        let body_start = if content[role_close + 1..].starts_with(' ') {
            role_close + 2
        } else {
            role_close + 1
        };
        // Body ends at `\n\n[` or end of content.
        let body_end = match content[body_start..].find("\n\n[") {
            Some(rel) => body_start + rel,
            None => content.len(),
        };
        let body = content[body_start..body_end].to_string();
        if !role.is_empty() {
            segments.push(ConversationSegment {
                role,
                content: body,
            });
        }
        // Advance past the trailing `\n\n` separator.
        idx = if body_end == content.len() {
            content.len()
        } else {
            body_end + 2 // skip "\n\n", land on `[`
        };
    }
    segments
}

/// Look up conversation metadata for a chunk, when applicable.
/// Returns `None` for non-conversation corpora and for conversation
/// chunks whose `source_doc_id` couldn't be resolved (deleted
/// conversation, store unavailable). Errors are swallowed because
/// the reading surface should still render the chunk text even when
/// the augmentation fails — a partial card beats a failed request.
pub(crate) async fn maybe_resolve_conversation_meta(
    daemon: &EmbeddedDaemon,
    corpus_id: &str,
    row: &EnrichmentChunkRow,
) -> Option<ConversationChunkMeta> {
    if corpus_id != CONVERSATION_HISTORY_CORPUS_ID {
        return None;
    }
    let conversation_id = row.source_doc_id.clone()?;
    let segments = parse_conversation_segments(&row.content);
    let store = daemon.state_store().await;
    let (title, updated_at) = match store {
        Some(s) => match s.get_conversation(&conversation_id).await {
            Ok(c) => (c.title.clone(), Some(c.updated_at)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };
    Some(ConversationChunkMeta {
        conversation_id,
        title,
        updated_at,
        segments,
    })
}

fn not_found(msg: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn internal_error(msg: &str) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn service_unavailable(msg: &str) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_radius_is_one() {
        assert_eq!(default_radius(), 1);
    }

    #[test]
    fn chunk_record_extracts_section_id_from_metadata() {
        let row = EnrichmentChunkRow {
            id: 7,
            content: "passage".into(),
            title: Some("BK Ch 1".into()),
            url: Some("https://example.org/bk".into()),
            metadata_raw: Some(r#"{"section_id":"sec_0001","other":"x"}"#.into()),
            source_doc_id: Some("brothers_karamazov".into()),
        };
        let record = chunk_record_from_row_with_conv("brothers_karamazov", &row, None, None);
        assert_eq!(record.chunk_id, 7);
        assert_eq!(record.section_id.as_deref(), Some("sec_0001"));
        assert_eq!(record.source_doc_id.as_deref(), Some("brothers_karamazov"));
        assert!(record.atom_spans.is_empty(), "no atoms passed");
    }

    #[test]
    fn chunk_record_handles_missing_metadata() {
        let row = EnrichmentChunkRow {
            id: 1,
            content: "x".into(),
            title: None,
            url: None,
            metadata_raw: None,
            source_doc_id: None,
        };
        let record = chunk_record_from_row_with_conv("any", &row, None, None);
        assert!(record.section_id.is_none());
        assert_eq!(record.metadata, serde_json::json!({}));
        assert!(record.atom_spans.is_empty());
        assert!(
            record.conversation.is_none(),
            "non-conversation corpora must not populate conversation meta"
        );
    }

    #[test]
    fn parse_conversation_segments_handles_two_message_chunk() {
        let content = "[user] How does Schrödinger frame negative entropy?\n\n\
                       [assistant] He argues life sustains order by feeding on it.";
        let segs = parse_conversation_segments(content);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].role, "user");
        assert_eq!(
            segs[0].content,
            "How does Schrödinger frame negative entropy?"
        );
        assert_eq!(segs[1].role, "assistant");
        assert_eq!(
            segs[1].content,
            "He argues life sustains order by feeding on it."
        );
    }

    #[test]
    fn parse_conversation_segments_handles_three_messages_with_inline_brackets() {
        // Body containing a lone `[` must NOT be treated as a new
        // role tag — the splitter only re-arms after `\n\n[`.
        let content = "[user] What about [bracket] inside?\n\n\
                       [assistant] Brackets like [foo] in prose stay attached.\n\n\
                       [user] Got it.";
        let segs = parse_conversation_segments(content);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].content, "What about [bracket] inside?");
        assert_eq!(
            segs[1].content,
            "Brackets like [foo] in prose stay attached."
        );
        assert_eq!(segs[2].content, "Got it.");
    }

    #[test]
    fn parse_conversation_segments_returns_empty_on_non_role_content() {
        // A chunk that doesn't start with `[role]` (e.g. mid-section
        // chunk from a different corpus or legacy ingest) returns
        // empty so the frontend renders the raw content.
        let content = "Plain prose paragraph without role markers.";
        assert!(parse_conversation_segments(content).is_empty());
    }

    #[test]
    fn parse_conversation_segments_handles_empty_string() {
        assert!(parse_conversation_segments("").is_empty());
    }

    #[test]
    fn parse_conversation_segments_handles_malformed_role_tag() {
        // `[user` (missing close bracket) is malformed — return what
        // we got rather than panic.
        let content = "[user no close bracket";
        let segs = parse_conversation_segments(content);
        assert!(
            segs.is_empty(),
            "malformed role tag should not produce a segment"
        );
    }
}
