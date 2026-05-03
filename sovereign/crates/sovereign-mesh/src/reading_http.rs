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
    CrossCorpusEdge, Edge, EdgeType,
};
use corpus_engine::EnrichmentChunkRow;

use crate::daemon::EmbeddedDaemon;

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
            "/internal/corpus/:corpus/chunks/:chunk_id",
            get(get_chunk),
        )
        .route(
            "/internal/corpus/:corpus/chunks/:chunk_id/neighbors",
            get(get_neighbors),
        )
        .route(
            "/internal/corpus/:corpus/atoms/:atom_id",
            get(get_atom_card),
        )
        .route(
            "/internal/corpus/:corpus/atoms/:atom_id/elsewhere",
            get(get_atom_elsewhere),
        )
        .layer(axum::middleware::from_fn(crate::loopback_guard::loopback_only))
        .layer(Extension(daemon))
}

fn enforce_localhost(addr: &SocketAddr) -> Result<(), axum::response::Response> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(ErrorBody { error: "local-only".into() }),
        )
            .into_response())
    }
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
    let record = chunk_record_from_row(&corpus, &row, atlas_atoms.as_deref());
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

    let center = chunk_record_from_row(&corpus, &window.center, atoms_ref);
    let outbound_url = center.url.clone();
    let prev = window
        .prev
        .iter()
        .map(|r| chunk_record_from_row(&corpus, r, atoms_ref))
        .collect();
    let next = window
        .next
        .iter()
        .map(|r| chunk_record_from_row(&corpus, r, atoms_ref))
        .collect();

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

    let edges = read_atlas_edges(&atlas_dir).map(|f| f.edges).unwrap_or_default();
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

fn atom_type_label(atom: &AtomEnvelope) -> &'static str {
    match atom {
        AtomEnvelope::Entity(_) => "entity",
        AtomEnvelope::Event(_) => "event",
        AtomEnvelope::State(_) => "state",
        AtomEnvelope::Relation(_) => "relation",
        AtomEnvelope::Claim(_) => "claim",
        AtomEnvelope::Question(_) => "question",
        AtomEnvelope::Configuration(_) => "configuration",
    }
}

fn edge_type_label(t: EdgeType) -> &'static str {
    match t {
        EdgeType::Transition => "transition",
        EdgeType::Causes => "causes",
        EdgeType::Grounds => "grounds",
        EdgeType::Tension => "tension",
        EdgeType::Involves => "involves",
        EdgeType::Composes => "composes",
        EdgeType::Configures => "configures",
        EdgeType::Grounding => "grounding",
        EdgeType::Framing => "framing",
        EdgeType::Provenance => "provenance",
    }
}

/// Pull the human-readable fields for any atom type. Not every
/// type has every field — for atoms without a clean canonical name
/// we synthesize from the most descriptive available text so the
/// panel still shows something sensible.
fn atom_surface_fields(
    atom: &AtomEnvelope,
) -> (String, Vec<String>, String, Option<f32>) {
    match atom {
        AtomEnvelope::Entity(e) => (
            e.canonical_name.clone(),
            e.aliases.clone(),
            e.description.clone(),
            Some(e.salience),
        ),
        AtomEnvelope::Event(e) => (
            truncate(&e.description, 80),
            Vec::new(),
            e.description.clone(),
            None,
        ),
        AtomEnvelope::State(s) => (
            s.label.clone(),
            Vec::new(),
            format!("State of {}: {}", s.entity_id.as_str(), s.label),
            s.confidence,
        ),
        AtomEnvelope::Relation(r) => (
            r.label.clone(),
            Vec::new(),
            r.label.clone(),
            None,
        ),
        AtomEnvelope::Claim(c) => (
            truncate(&c.content, 80),
            Vec::new(),
            c.content.clone(),
            c.confidence,
        ),
        AtomEnvelope::Question(q) => (
            truncate(&q.content, 80),
            Vec::new(),
            q.content.clone(),
            None,
        ),
        AtomEnvelope::Configuration(c) => (
            c.label.clone(),
            Vec::new(),
            c.description.clone(),
            Some(c.confidence),
        ),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let trimmed: String = s.chars().take(max_chars).collect();
    if trimmed.chars().count() < s.chars().count() {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

/// Extract every `(section_id, optional_preview)` pair from an
/// atom's evidence (or `first_appearance` for entities, or
/// `section_position` for events). Order preserves the order
/// evidence was written.
fn atom_evidence_section_refs(atom: &AtomEnvelope) -> Vec<(String, Option<String>)> {
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
    }
}

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

pub(crate) fn chunk_record_from_row(
    corpus_id: &str,
    row: &EnrichmentChunkRow,
    atoms: Option<&[AtomEnvelope]>,
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
    }
}

fn not_found(msg: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody { error: msg.to_string() }),
    )
        .into_response()
}

fn internal_error(msg: &str) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody { error: msg.to_string() }),
    )
        .into_response()
}

fn service_unavailable(msg: &str) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody { error: msg.to_string() }),
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
        let record = chunk_record_from_row("brothers_karamazov", &row, None);
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
        let record = chunk_record_from_row("any", &row, None);
        assert!(record.section_id.is_none());
        assert_eq!(record.metadata, serde_json::json!({}));
        assert!(record.atom_spans.is_empty());
    }
}
