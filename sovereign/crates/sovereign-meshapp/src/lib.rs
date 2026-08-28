// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mesh-app explorer ops — the read-only projections a sandboxed explorer
//! drives, factored out of the desktop so both the Tauri host AND the
//! `sovereign meshapp dev` CLI server can serve them.
//!
//! Everything here is **pure** (path-in, DTO-out) and Tauri-free. The host
//! wraps each function in a permission-gated [`tauri::command`]; the dev server
//! exposes them over HTTP. The split keeps one source of truth for the
//! backend-agnostic graph contract — an explorer reads either a deterministic
//! `investigation/` graph (UAP) or an `atlas/` enrichment (Enron) through the
//! same DTOs ([`GraphNodeDto`] / [`EdgeDto`] / [`NodeDetailDto`]).

pub mod wrapped;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use corpus_engine::enrichment::atlas::{AtomEnvelope, AtomId, ChunkRef};
use corpus_engine::enrichment::investigation::graph::{
    read_outputs as read_investigation_graph, Entity as InvEntity,
    ExtractionExcerpt as InvEvidence, PatternFinding, PatternKind, Relationship as InvRelationship,
    INVESTIGATION_DIRNAME,
};
use corpus_engine::index::CorpusIndex;

// ─── DTOs (the bundle contract) ──────────────────────────────────────

/// A degree-ranked node. `degree` = incident relationships; `alias_count` =
/// surface forms the coalesce phase folded in.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNodeDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
    pub degree: usize,
    pub alias_count: usize,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// One relationship incident to a node, resolved to its other endpoint and
/// carrying its cited evidence — the glassbox edge.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeDto {
    pub relationship_type: String,
    /// `"out"` — this node is the source; `"in"` — this node is the target.
    pub direction: String,
    pub other_id: String,
    pub other_name: String,
    pub other_type: String,
    pub excerpt: String,
    pub source_chunk: String,
    pub confidence: f32,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// A node's full detail: attributes, folded aliases, every incident cited edge.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDetailDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
    pub aliases: Vec<String>,
    pub edges: Vec<EdgeDto>,
}

/// A deterministic pattern finding (e.g. a sighting hotspot).
#[derive(Debug, Clone, Serialize)]
pub struct FindingDto {
    pub pattern_name: String,
    pub pattern_kind: String,
    pub entities: Vec<FindingEntityDto>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingEntityDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
}

/// One cross-origin identity merge: a canonical entity + the surface forms
/// folded into it + the signals that fired (the glassbox reason).
#[derive(Debug, Clone, Serialize)]
pub struct ReconciliationMergeDto {
    pub canonical_id: String,
    pub canonical_name: String,
    pub surface_forms: Vec<String>,
    pub signals_fired: Vec<String>,
    pub source_count: usize,
}

/// One undirected edge of a [`SubgraphDto`].
#[derive(Debug, Clone, Serialize)]
pub struct SubEdgeDto {
    pub source: String,
    pub target: String,
    pub relationship_type: String,
}

/// Top-degree nodes + the edges induced among them, for a node-link map.
#[derive(Debug, Clone, Serialize)]
pub struct SubgraphDto {
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<SubEdgeDto>,
}

/// Headline scale/provenance counts for a banner.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CorpusStatsDto {
    pub atoms: usize,
    pub entities: usize,
    pub events: usize,
    pub states: usize,
    pub relations: usize,
    pub claims: usize,
    pub questions: usize,
    pub edges: usize,
    pub reconciled_merges: usize,
    pub documents: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineBucketDto {
    /// `YYYY-MM`.
    pub ym: String,
    pub count: usize,
    /// A capped sample of chunk ids in this month, for click-to-drill.
    pub chunk_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineDto {
    pub buckets: Vec<TimelineBucketDto>,
    pub dated: usize,
    pub total: usize,
}

/// Full source-chunk text behind a cited edge.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkDto {
    pub chunk_id: String,
    pub content: String,
    pub title: Option<String>,
}

/// One chunk inside a [`FeedDocDto`] — carries the raw-metadata-derived
/// `outbound_links` (wikilink target titles for newsworthy; empty for
/// corpora whose extractor doesn't stamp links).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedChunkDto {
    pub chunk_id: String,
    pub content: String,
    pub title: Option<String>,
    pub outbound_links: Vec<String>,
}

/// One source document in a [`document_feed`] response — for the
/// newsworthy corpus, one portal day (`source_doc_id = "YYYY-MM-DD"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedDocDto {
    pub source_doc_id: String,
    pub chunks: Vec<FeedChunkDto>,
}

/// [`document_feed`] response: documents newest-first by
/// `source_doc_id` (dates sort correctly lexicographically).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentFeedDto {
    pub corpus_id: String,
    pub docs: Vec<FeedDocDto>,
}

/// A claim atom projected for the explorer's "arguments" view — the entity
/// graph ops don't surface claims, so this carries the proposition, its
/// discourse + epistemic framing, who it's attributed to (entity name,
/// resolved), and its first cited evidence.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimDto {
    pub id: String,
    pub content: String,
    pub discourse_act: String,
    pub epistemic_status: String,
    pub quotable_excerpt: Option<String>,
    pub attributed_to: Option<String>,
    pub source_chunk: String,
    pub excerpt: String,
}

/// A question atom projected for the explorer — the inquiry, its type +
/// resolution status, how many claims address it, and where it's raised.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionDto {
    pub id: String,
    pub content: String,
    pub question_type: String,
    pub resolution_status: String,
    pub addressed_by: usize,
    pub source_chunk: String,
}

// ─── The dispatched graph ────────────────────────────────────────────

/// A corpus's entity graph: entities + relationships + findings, projected
/// from whichever backend the index carries. The projection functions below
/// take it by reference.
pub struct Graph {
    pub entities: Vec<InvEntity>,
    pub rels: Vec<InvRelationship>,
    pub findings: Vec<PatternFinding>,
}

/// Load a corpus's graph, dispatching on what the index carries: a
/// deterministic `investigation/` graph (UAP) or an `atlas/` enrichment
/// (Enron). Both project into the same shapes.
pub fn load_graph(index_path: &Path) -> Result<Graph, String> {
    if index_path.join(INVESTIGATION_DIRNAME).is_dir() {
        let (entities, rels, findings) = read_investigation_graph(index_path)
            .map_err(|e| format!("read investigation graph: {e}"))?;
        return Ok(Graph {
            entities,
            rels,
            findings,
        });
    }
    if index_path.join("atlas").is_dir() {
        let (entities, rels, findings) = load_atlas_as_investigation(index_path)?;
        return Ok(Graph {
            entities,
            rels,
            findings,
        });
    }
    Err("corpus has neither an investigation graph nor an atlas to explore".to_string())
}

// ─── Atlas → graph adapter ───────────────────────────────────────────

/// Cap on participants paired into edges from a single Relation/Event atom.
const MAX_ATLAS_EDGE_PARTICIPANTS: usize = 8;

struct MergeRecord {
    surface_forms: Vec<String>,
    signals_fired: Vec<String>,
    source_count: usize,
}

#[derive(Deserialize)]
struct ReconFile {
    #[serde(default)]
    merged_entities: Vec<MergedEntityRow>,
}

#[derive(Deserialize)]
struct MergedEntityRow {
    canonical_id: String,
    #[serde(default)]
    canonical_name: String,
    #[serde(default)]
    surface_forms: Vec<(String, serde_json::Value)>,
    #[serde(default)]
    signals_fired: Vec<String>,
    #[serde(default)]
    source_atom_ids: Vec<String>,
}

/// Adapt an `atlas/` enrichment into the investigation-graph shapes: Entity
/// atoms → nodes (reconciliation reason stamped into `attributes`), Relation +
/// Event atoms → cited edges over their entity participants (with `sec_NNNNN`
/// evidence resolved to numeric chunk ids via `chapters.json`). Findings →
/// empty (the atlas identity story is [`reconciliation`], not findings).
fn load_atlas_as_investigation(
    index_path: &Path,
) -> Result<(Vec<InvEntity>, Vec<InvRelationship>, Vec<PatternFinding>), String> {
    let atlas_dir = index_path.join("atlas");
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?;
    let sec_to_chunk = read_chapter_chunk_map(index_path)?;
    let recon = read_reconciliation_index(&atlas_dir);

    let mut entities = Vec::new();
    let mut entity_ids: HashSet<String> = HashSet::new();
    for env in &file.atoms {
        if let AtomEnvelope::Entity(e) = env {
            let id = e.id.as_str().to_string();
            entity_ids.insert(id.clone());
            let mut attributes = serde_json::Map::new();
            if !e.description.is_empty() {
                attributes.insert("description".into(), e.description.clone().into());
            }
            attributes.insert("salience".into(), (e.salience as f64).into());
            if let Some(m) = recon.get(&id) {
                attributes.insert(
                    "reconciliation".into(),
                    serde_json::json!({
                        "surface_forms": m.surface_forms,
                        "signals_fired": m.signals_fired,
                        "source_count": m.source_count,
                    }),
                );
            }
            entities.push(InvEntity {
                id,
                canonical_name: e.canonical_name.clone(),
                entity_type: e.entity_type.as_str_repr().to_string(),
                attributes,
                aliases: e.aliases.clone(),
            });
        }
    }

    let mut rels = Vec::new();
    for env in &file.atoms {
        match env {
            AtomEnvelope::Relation(r) => {
                let participants = entity_participants(&r.participants, &entity_ids);
                if participants.len() < 2 {
                    continue;
                }
                let (excerpt, chunk) = first_evidence(r.evidence.first(), &sec_to_chunk);
                let rtype = if r.label.trim().is_empty() {
                    r.relation_type.as_str_repr().to_string()
                } else {
                    r.label.clone()
                };
                push_pairwise_edges(
                    &mut rels,
                    r.id.as_str(),
                    &participants,
                    &rtype,
                    &excerpt,
                    &chunk,
                    &serde_json::Map::new(),
                );
            }
            AtomEnvelope::Event(ev) => {
                let participants = entity_participants(&ev.participants, &entity_ids);
                if participants.len() < 2 {
                    continue;
                }
                let (excerpt, chunk) = first_evidence(ev.evidence.first(), &sec_to_chunk);
                let mut attrs = serde_json::Map::new();
                if !ev.description.is_empty() {
                    attrs.insert("description".into(), ev.description.clone().into());
                }
                push_pairwise_edges(
                    &mut rels,
                    ev.id.as_str(),
                    &participants,
                    ev.event_type.as_str_repr(),
                    &excerpt,
                    &chunk,
                    &attrs,
                );
            }
            _ => {}
        }
    }

    Ok((entities, rels, Vec::new()))
}

/// Participant ids that are real entities (drop dangling refs), capped so one
/// big multi-party atom can't dominate the degree distribution.
fn entity_participants<'a>(
    participants: &'a [AtomId],
    entity_ids: &HashSet<String>,
) -> Vec<&'a str> {
    participants
        .iter()
        .map(|p| p.as_str())
        .filter(|p| entity_ids.contains(*p))
        .take(MAX_ATLAS_EDGE_PARTICIPANTS)
        .collect()
}

/// Resolve an atom's first evidence ref into `(excerpt, source_chunk)`, where
/// `source_chunk` is the numeric `chunks.lance` row id the section maps to (via
/// `chapters.json`), falling back to the raw section id when unmapped.
fn first_evidence(
    evidence: Option<&ChunkRef>,
    sec_to_chunk: &HashMap<String, String>,
) -> (String, String) {
    match evidence {
        Some(cr) => {
            let excerpt = cr.passage_preview.clone().unwrap_or_default();
            let chunk = sec_to_chunk
                .get(&cr.chunk_id)
                .cloned()
                .unwrap_or_else(|| cr.chunk_id.clone());
            (excerpt, chunk)
        }
        None => (String::new(), String::new()),
    }
}

/// Emit one undirected edge per participant pair (`n choose 2`), each carrying
/// the same cited evidence.
fn push_pairwise_edges(
    out: &mut Vec<InvRelationship>,
    atom_id: &str,
    participants: &[&str],
    relationship_type: &str,
    excerpt: &str,
    chunk_id: &str,
    attributes: &serde_json::Map<String, serde_json::Value>,
) {
    let mut k = 0usize;
    for i in 0..participants.len() {
        for j in (i + 1)..participants.len() {
            out.push(InvRelationship {
                id: format!("{atom_id}#{k}"),
                from_entity_id: participants[i].to_string(),
                to_entity_id: participants[j].to_string(),
                relationship_type: relationship_type.to_string(),
                attributes: attributes.clone(),
                evidence: InvEvidence {
                    chunk_id: chunk_id.to_string(),
                    excerpt: excerpt.to_string(),
                },
                confidence: 1.0,
            });
            k += 1;
        }
    }
}

// ─── Projections over a `Graph` ──────────────────────────────────────

/// Incident-relationship count per entity id (graph degree).
fn degree_map(rels: &[InvRelationship]) -> HashMap<&str, usize> {
    let mut deg: HashMap<&str, usize> = HashMap::new();
    for r in rels {
        *deg.entry(r.from_entity_id.as_str()).or_default() += 1;
        *deg.entry(r.to_entity_id.as_str()).or_default() += 1;
    }
    deg
}

fn pattern_kind_str(kind: &PatternKind) -> &'static str {
    match kind {
        PatternKind::CircularFlow => "circular_flow",
        PatternKind::RoleOverlap => "role_overlap",
        PatternKind::Threshold => "threshold",
        PatternKind::CustomSql => "custom_sql",
    }
}

fn to_graph_node(e: &InvEntity, deg: &HashMap<&str, usize>) -> GraphNodeDto {
    GraphNodeDto {
        degree: deg.get(e.id.as_str()).copied().unwrap_or(0),
        alias_count: e.aliases.len(),
        id: e.id.clone(),
        canonical_name: e.canonical_name.clone(),
        entity_type: e.entity_type.clone(),
        attributes: e.attributes.clone(),
    }
}

/// Degree-ranked entities (optionally one type), highest-degree first.
pub fn graph_nodes(g: &Graph, node_type: Option<&str>, limit: usize) -> Vec<GraphNodeDto> {
    let deg = degree_map(&g.rels);
    let mut nodes: Vec<GraphNodeDto> = g
        .entities
        .iter()
        .filter(|e| node_type.is_none_or(|t| e.entity_type.eq_ignore_ascii_case(t)))
        .map(|e| to_graph_node(e, &deg))
        .collect();
    nodes.sort_by(|a, b| {
        b.degree
            .cmp(&a.degree)
            .then_with(|| b.alias_count.cmp(&a.alias_count))
    });
    nodes.truncate(limit);
    nodes
}

/// One entity's full detail + every incident edge, each resolved to its other
/// endpoint and quoting its evidence excerpt + source chunk.
pub fn node_detail(g: &Graph, id: &str) -> Result<NodeDetailDto, String> {
    let by_id: HashMap<&str, &InvEntity> = g.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let me = by_id.get(id).ok_or_else(|| format!("no entity `{id}`"))?;
    let mut edges = Vec::new();
    for r in &g.rels {
        let (direction, other_id) = if r.from_entity_id == id {
            ("out", r.to_entity_id.as_str())
        } else if r.to_entity_id == id {
            ("in", r.from_entity_id.as_str())
        } else {
            continue;
        };
        let (other_name, other_type) = by_id
            .get(other_id)
            .map(|e| (e.canonical_name.clone(), e.entity_type.clone()))
            .unwrap_or_else(|| (other_id.to_string(), String::new()));
        edges.push(EdgeDto {
            relationship_type: r.relationship_type.clone(),
            direction: direction.to_string(),
            other_id: other_id.to_string(),
            other_name,
            other_type,
            excerpt: r.evidence.excerpt.clone(),
            source_chunk: r.evidence.chunk_id.clone(),
            confidence: r.confidence,
            attributes: r.attributes.clone(),
        });
    }
    Ok(NodeDetailDto {
        id: me.id.clone(),
        canonical_name: me.canonical_name.clone(),
        entity_type: me.entity_type.clone(),
        attributes: me.attributes.clone(),
        aliases: me.aliases.clone(),
        edges,
    })
}

/// Deterministic pattern findings (optionally one pattern), each resolved to
/// its participating entities' names.
pub fn findings(g: &Graph, pattern: Option<&str>) -> Vec<FindingDto> {
    let by_id: HashMap<&str, &InvEntity> = g.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    g.findings
        .iter()
        .filter(|f| pattern.is_none_or(|p| f.pattern_name.as_str() == p))
        .map(|f| FindingDto {
            entities: f
                .entity_ids
                .iter()
                .map(|eid| {
                    let e = by_id.get(eid.as_str());
                    FindingEntityDto {
                        id: eid.clone(),
                        canonical_name: e
                            .map(|x| x.canonical_name.clone())
                            .unwrap_or_else(|| eid.clone()),
                        entity_type: e.map(|x| x.entity_type.clone()).unwrap_or_default(),
                    }
                })
                .collect(),
            pattern_kind: pattern_kind_str(&f.pattern_type).to_string(),
            pattern_name: f.pattern_name.clone(),
            attributes: f.attributes.clone(),
        })
        .collect()
}

/// Case-folded substring over canonical name, aliases, and string attribute
/// values. Degree-ranked.
pub fn search_entities(
    g: &Graph,
    query: &str,
    node_type: Option<&str>,
    limit: usize,
) -> Vec<GraphNodeDto> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let deg = degree_map(&g.rels);
    let mut out: Vec<GraphNodeDto> = g
        .entities
        .iter()
        .filter(|e| node_type.is_none_or(|t| e.entity_type.eq_ignore_ascii_case(t)))
        .filter(|e| {
            e.canonical_name.to_lowercase().contains(&q)
                || e.aliases.iter().any(|a| a.to_lowercase().contains(&q))
                || e.attributes
                    .values()
                    .any(|v| v.as_str().is_some_and(|s| s.to_lowercase().contains(&q)))
        })
        .map(|e| to_graph_node(e, &deg))
        .collect();
    out.sort_by(|a, b| b.degree.cmp(&a.degree));
    out.truncate(limit);
    out
}

/// Top-`limit` nodes by degree (optionally one type) + the de-duplicated,
/// capped edges induced among them.
pub fn subgraph(g: &Graph, node_type: Option<&str>, limit: usize) -> SubgraphDto {
    let mut nodes = graph_nodes(g, node_type, limit);
    nodes.truncate(limit);
    let keep: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut edges = Vec::new();
    for r in &g.rels {
        let (a, b) = (r.from_entity_id.as_str(), r.to_entity_id.as_str());
        if a == b || !keep.contains(a) || !keep.contains(b) {
            continue;
        }
        let key = if a <= b { (a, b) } else { (b, a) };
        if seen.insert(key) {
            edges.push(SubEdgeDto {
                source: r.from_entity_id.clone(),
                target: r.to_entity_id.clone(),
                relationship_type: r.relationship_type.clone(),
            });
            if edges.len() >= 400 {
                break;
            }
        }
    }
    SubgraphDto { nodes, edges }
}

// ─── Claims + Questions (atlas atoms the entity graph doesn't surface) ─

/// Map entity AtomId → canonical name, for resolving a claim's attribution to
/// a human-readable name.
fn entity_name_map(atoms: &[AtomEnvelope]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for env in atoms {
        if let AtomEnvelope::Entity(e) = env {
            m.insert(e.id.as_str().to_string(), e.canonical_name.clone());
        }
    }
    m
}

/// Render an atom enum to a display string. Plain string enums (DiscourseAct,
/// EpistemicStatus, QuestionType) serialise to `"argue"`; tagged unions
/// (ResolutionStatus) serialise to `{"kind":"resolved", ...}` — pull the kind.
fn enum_label<T: Serialize>(v: &T, fallback: &str) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(serde_json::Value::Object(m)) => m
            .get("kind")
            .and_then(|k| k.as_str())
            .map(String::from)
            .unwrap_or_else(|| fallback.to_string()),
        _ => fallback.to_string(),
    }
}

/// Claim atoms → cited DTOs (empty for non-atlas corpora). Reuses the same
/// `atoms.json` read + `sec_NNNNN → chunk` resolution as the graph adapter.
pub fn load_claims(index_path: &Path, limit: usize) -> Result<Vec<ClaimDto>, String> {
    let atlas_dir = index_path.join("atlas");
    if !atlas_dir.is_dir() {
        return Ok(Vec::new());
    }
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?;
    let sec_to_chunk = read_chapter_chunk_map(index_path)?;
    let names = entity_name_map(&file.atoms);
    let mut out = Vec::new();
    for env in &file.atoms {
        if let AtomEnvelope::Claim(c) = env {
            let (excerpt, source_chunk) = first_evidence(c.evidence.first(), &sec_to_chunk);
            out.push(ClaimDto {
                id: c.id.as_str().to_string(),
                content: c.content.clone(),
                discourse_act: enum_label(&c.discourse_act, "claim"),
                epistemic_status: enum_label(&c.epistemic_status, "stated"),
                quotable_excerpt: c.quotable_excerpt.clone(),
                attributed_to: c
                    .attributed_to
                    .as_ref()
                    .and_then(|id| names.get(id.as_str()).cloned()),
                source_chunk,
                excerpt,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// Question atoms → DTOs (empty for non-atlas corpora).
pub fn load_questions(index_path: &Path, limit: usize) -> Result<Vec<QuestionDto>, String> {
    let atlas_dir = index_path.join("atlas");
    if !atlas_dir.is_dir() {
        return Ok(Vec::new());
    }
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?;
    let sec_to_chunk = read_chapter_chunk_map(index_path)?;
    let mut out = Vec::new();
    for env in &file.atoms {
        if let AtomEnvelope::Question(q) = env {
            let (_, source_chunk) = first_evidence(q.raised_at.first(), &sec_to_chunk);
            out.push(QuestionDto {
                id: q.id.as_str().to_string(),
                content: q.content.clone(),
                question_type: enum_label(&q.question_type, "question"),
                resolution_status: enum_label(&q.resolution_status, "open"),
                addressed_by: q.addressed_by.len(),
                source_chunk,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

// ─── File-based reads (chapters / reconciliation / stats) ────────────

/// One `chapters.json` row — an ingested document and the chunk rows it made.
#[derive(Deserialize)]
struct ChapterRow {
    id: String,
    #[serde(default)]
    chunk_ids: Vec<u64>,
}

fn read_chapters(index_path: &Path) -> Result<Vec<ChapterRow>, String> {
    #[derive(Deserialize)]
    struct ChaptersFile {
        #[serde(default)]
        chapters: Vec<ChapterRow>,
    }
    let path = index_path.join("chapters.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let file: ChaptersFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(file.chapters)
}

/// `sec_NNNNN` → first numeric `chunks.lance` row id (as a string).
fn read_chapter_chunk_map(index_path: &Path) -> Result<HashMap<String, String>, String> {
    let chapters = read_chapters(index_path)?;
    let mut map = HashMap::with_capacity(chapters.len());
    for c in chapters {
        if let Some(first) = c.chunk_ids.first() {
            map.insert(c.id, first.to_string());
        }
    }
    Ok(map)
}

fn read_reconciliation_rows(atlas_dir: &Path) -> Vec<MergedEntityRow> {
    let path = atlas_dir.join("reconciliation.json");
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<ReconFile>(&bytes)
        .map(|f| f.merged_entities)
        .unwrap_or_default()
}

fn read_reconciliation_index(atlas_dir: &Path) -> HashMap<String, MergeRecord> {
    read_reconciliation_rows(atlas_dir)
        .into_iter()
        .map(|r| {
            let source_count = r.source_atom_ids.len();
            (
                r.canonical_id,
                MergeRecord {
                    surface_forms: r.surface_forms.into_iter().map(|(name, _)| name).collect(),
                    signals_fired: r.signals_fired,
                    source_count,
                },
            )
        })
        .collect()
}

/// The reconciliation merges as DTOs, richest (most surface forms) first.
pub fn reconciliation(index_path: &Path) -> Vec<ReconciliationMergeDto> {
    let mut out: Vec<ReconciliationMergeDto> = read_reconciliation_rows(&index_path.join("atlas"))
        .into_iter()
        .map(|r| {
            let source_count = r.source_atom_ids.len();
            ReconciliationMergeDto {
                canonical_id: r.canonical_id,
                canonical_name: r.canonical_name,
                surface_forms: r.surface_forms.into_iter().map(|(name, _)| name).collect(),
                signals_fired: r.signals_fired,
                source_count,
            }
        })
        .collect();
    out.sort_by(|a, b| b.surface_forms.len().cmp(&a.surface_forms.len()));
    out
}

/// Headline counts for a scale/provenance banner. Every field defaults to 0
/// for a corpus missing the atlas.
pub fn corpus_stats(index_path: &Path) -> CorpusStatsDto {
    let atlas = index_path.join("atlas");
    let mut s = CorpusStatsDto::default();
    if let Ok(bytes) = std::fs::read(atlas.join("_summary.json")) {
        #[derive(Deserialize, Default)]
        struct Summary {
            #[serde(default)]
            atom_count: usize,
            #[serde(default)]
            atom_counts: HashMap<String, usize>,
        }
        if let Ok(sm) = serde_json::from_slice::<Summary>(&bytes) {
            let c = |k: &str| sm.atom_counts.get(k).copied().unwrap_or(0);
            s.atoms = sm.atom_count;
            s.entities = c("Entity");
            s.events = c("Event");
            s.states = c("State");
            s.relations = c("Relation");
            s.claims = c("Claim");
            s.questions = c("Question");
        }
    }
    // Count edges.json without deserializing the Edge schema — robust to drift.
    s.edges = std::fs::read(atlas.join("edges.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("edges").and_then(|a| a.as_array()).map(|a| a.len()))
        .unwrap_or(0);
    s.reconciled_merges = read_reconciliation_rows(&atlas).len();
    s.documents = read_chapters(index_path).map(|c| c.len()).unwrap_or(0);
    s
}

// ─── Async reads (timeline / chunk) ──────────────────────────────────

/// Bucket the corpus's documents by month, parsed from the `Date:` header every
/// email chunk carries. Empty when the corpus has no `chapters.json`.
pub async fn timeline(index_path: &Path) -> Result<TimelineDto, String> {
    let chapters = read_chapters(index_path)?;
    let first_ids: Vec<u64> = chapters
        .iter()
        .filter_map(|c| c.chunk_ids.first().copied())
        .collect();
    let total = first_ids.len();
    if first_ids.is_empty() {
        return Ok(TimelineDto {
            buckets: Vec::new(),
            dated: 0,
            total: 0,
        });
    }
    let index = CorpusIndex::open(index_path)
        .await
        .map_err(|e| format!("open index: {e}"))?;
    let chunks = index
        .get_chunks(&first_ids)
        .await
        .map_err(|e| format!("read chunks: {e}"))?;

    let mut by_ym: std::collections::BTreeMap<String, (usize, Vec<u64>)> =
        std::collections::BTreeMap::new();
    let mut dated = 0;
    for c in &chunks {
        if let Some(ym) = parse_email_year_month(&c.content) {
            let e = by_ym.entry(ym).or_default();
            e.0 += 1;
            if e.1.len() < 40 {
                e.1.push(c.id);
            }
            dated += 1;
        }
    }
    let buckets = by_ym
        .into_iter()
        .map(|(ym, (count, chunk_ids))| TimelineBucketDto {
            ym,
            count,
            chunk_ids,
        })
        .collect();
    Ok(TimelineDto {
        buckets,
        dated,
        total,
    })
}

/// One chunk's full text by its numeric id.
pub async fn read_chunk(index_path: &Path, chunk_id: u64) -> Result<ChunkDto, String> {
    let index = CorpusIndex::open(index_path)
        .await
        .map_err(|e| format!("open index: {e}"))?;
    let chunks = index
        .get_chunks(&[chunk_id])
        .await
        .map_err(|e| format!("read chunk {chunk_id}: {e}"))?;
    let c = chunks
        .into_iter()
        .next()
        .ok_or_else(|| format!("no chunk {chunk_id}"))?;
    Ok(ChunkDto {
        chunk_id: chunk_id.to_string(),
        content: c.content,
        title: c.title,
    })
}

/// Document-keyed feed over a corpus: the latest `limit_docs` source
/// documents (by `source_doc_id`, descending — date-keyed corpora like
/// `wikipedia-newsworthy` therefore come newest-first), each with its
/// chunks in id order and the `outbound_links` parsed from raw chunk
/// metadata. This is the read primitive behind feed-shaped mesh apps
/// (the "Today" current-events app; a future inbox app groups by
/// thread the same way).
pub async fn document_feed(
    index_path: &Path,
    limit_docs: usize,
) -> Result<DocumentFeedDto, String> {
    let index = CorpusIndex::open(index_path)
        .await
        .map_err(|e| format!("open index: {e}"))?;

    let by_doc = index
        .group_chunks_by_source_doc()
        .await
        .map_err(|e| format!("group by source doc: {e}"))?;
    let mut doc_ids: Vec<String> = by_doc.keys().cloned().collect();
    doc_ids.sort();
    doc_ids.reverse();
    doc_ids.truncate(limit_docs.max(1));

    // Raw metadata is a separate projection (content is not in it, and
    // `get_chunks` drops metadata) — join the two by chunk id.
    let metadata_by_id: HashMap<u64, String> = index
        .all_chunks_with_raw_metadata()
        .await
        .map_err(|e| format!("read chunk metadata: {e}"))?
        .into_iter()
        .filter_map(|c| c.metadata_raw.map(|m| (c.id, m)))
        .collect();

    let mut docs = Vec::with_capacity(doc_ids.len());
    for doc_id in doc_ids {
        let mut ids = by_doc.get(&doc_id).cloned().unwrap_or_default();
        ids.sort_unstable();
        let chunks = index
            .get_chunks(&ids)
            .await
            .map_err(|e| format!("read chunks for {doc_id}: {e}"))?;
        let feed_chunks = chunks
            .into_iter()
            .map(|c| FeedChunkDto {
                outbound_links: metadata_by_id
                    .get(&c.id)
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                    .and_then(|v| {
                        v.get("outbound_links").and_then(|l| l.as_array()).map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                    })
                    .unwrap_or_default(),
                chunk_id: c.id.to_string(),
                content: c.content,
                title: c.title,
            })
            .collect();
        docs.push(FeedDocDto {
            source_doc_id: doc_id,
            chunks: feed_chunks,
        });
    }

    let corpus_id = index_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(DocumentFeedDto { corpus_id, docs })
}

/// Pull `YYYY-MM` from the `Date:` line of an email chunk's RFC5322 preamble.
fn parse_email_year_month(content: &str) -> Option<String> {
    let head = &content[..content.len().min(600)];
    for line in head.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Date:") {
            if let Some(ym) = year_month_from_rfc5322(rest) {
                return Some(ym);
            }
        }
    }
    None
}

fn year_month_from_rfc5322(s: &str) -> Option<String> {
    let mut month: Option<u32> = None;
    let mut year: Option<i32> = None;
    for tok in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        if month.is_none() {
            if let Some(m) = month_num(tok) {
                month = Some(m);
                continue;
            }
        }
        if year.is_none() && tok.len() == 4 {
            if let Ok(y) = tok.parse::<i32>() {
                if (1990..=2010).contains(&y) {
                    year = Some(y);
                }
            }
        }
    }
    match (year, month) {
        (Some(y), Some(m)) => Some(format!("{y:04}-{m:02}")),
        _ => None,
    }
}

fn month_num(tok: &str) -> Option<u32> {
    let t = tok.to_ascii_lowercase();
    Some(match &t[..t.len().min(3)] {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
