//! MeshApp bridge — the permission-gated Tauri commands a sandboxed
//! mesh-app webview reaches through `window.meshApp.*`.
//!
//! Every command's FIRST act is [`crate::meshapp::authorize`] against the
//! CALLING webview's label (Tauri injects the `WebviewWindow`; the label
//! is host-assigned at window creation and unspoofable from inside the
//! sandbox). Only after the grant check does a command touch host state.
//!
//! The numeric ops (`read_corpus`, `parcel_analytics`) are deterministic
//! and read-only — folds over typed parcel atoms, no inference — so the
//! SF-LVT "no confabulated numbers" guarantee carries onto the desktop
//! surface: a model never originates a figure here either.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::meshapp::{app_id_from_label, authorize, resolve_grant, MeshAppPermissions, Permission};
use crate::state::AppState;

use corpus_engine::enrichment::atlas::analysis::{compute_aggregates, flags, FlagKind};
use corpus_engine::enrichment::atlas::{AtomEnvelope, AtomId, ChunkRef};
use corpus_engine::index::CorpusIndex;
use corpus_engine::enrichment::investigation::graph::{
    read_outputs as read_investigation_graph, Entity as InvEntity, Evidence as InvEvidence,
    PatternFinding, PatternKind, Relationship as InvRelationship, INVESTIGATION_DIRNAME,
};
use corpus_engine::enrichment::pipeline::atlas::EntityType;

/// Default SF business-tax take (~$1.4B) the flat land levy must replace.
const DEFAULT_BUSINESS_TAX_TARGET: f64 = 1_400_000_000.0;
const DEFAULT_ENTITY_TYPE: &str = "parcel";
/// SF's effective secured property-tax rate (the 1% Prop-13 base + voter-
/// approved add-ons). A labeled estimate — used to derive the revenue-neutral
/// land-only ("swap") rate, which is the only coherent per-parcel comparison:
/// today's tax falls on land + improvements; a land-value tax shifts the same
/// revenue onto land alone, producing real winners (improvement-heavy parcels)
/// and losers (land-rich / underused parcels).
const DEFAULT_PROPERTY_TAX_RATE: f64 = 0.0118;

/// One parcel atom for the webview, carrying its provenance handle so the
/// per-parcel calculator can chip every number back to its source.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelDto {
    pub atom_id: String,
    pub parcel_number: String,
    pub source_chunk: Option<String>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// The deterministic city-wide aggregate + its derivation. Scalars only
/// (NOT the ~208k `atom_ids`): the macro model multiplies
/// `land_value_total` by the slider rate in JS, and provenance is
/// "computed over `parcel_count` parcel atoms in `corpus_id`", surfaced
/// via `derivation`.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelAnalyticsDto {
    pub corpus_id: String,
    pub parcel_count: usize,
    pub land_value_total: f64,
    pub improvement_value_total: f64,
    pub business_tax_target: f64,
    pub neutral_rate: f64,
    /// Revenue-neutral property-tax → land-only "swap" scenario — the coherent
    /// per-parcel basis. `property_tax_revenue_est` = (land + improvements) ×
    /// `property_tax_rate`; `property_tax_swap_rate` = that ÷ land base.
    pub property_tax_rate: f64,
    pub property_tax_revenue_est: f64,
    pub property_tax_swap_rate: f64,
    pub high_land_share_count: usize,
    pub underused_count: usize,
    pub derivation: Vec<String>,
}

/// Load a corpus's atlas atoms, propagating errors (the bridge surfaces a
/// reason rather than silently returning empty). Mirrors the read path in
/// `commands::reading`.
async fn load_atoms(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
) -> Result<Vec<AtomEnvelope>, String> {
    let engine = state
        .corpus_engine
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "corpus engine not initialized".to_string())?;
    let installed = engine
        .installed_indexes()
        .await
        .map_err(|e| format!("installed_indexes: {e}"))?;
    let entry = installed
        .iter()
        .find(|i| i.corpus_id == corpus_id)
        .ok_or_else(|| format!("corpus `{corpus_id}` is not installed"))?;
    let atlas_dir = entry.path.join("atlas");
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms for `{corpus_id}`: {e}"))?;
    Ok(file.atoms)
}

/// `window.meshApp.capabilities()` — ungated. Returns the permission
/// subset the calling app was granted (all-false when not installed), so
/// the UI can hide affordances it isn't allowed to use.
#[tauri::command]
pub async fn meshapp_capabilities(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
) -> Result<MeshAppPermissions, String> {
    let app_id = app_id_from_label(webview.label())
        .ok_or_else(|| "caller is not a mesh-app window".to_string())?;
    let installs = state.config.read().await.meshapp_installs.clone();
    Ok(resolve_grant(&installs, &app_id)
        .map(|i| i.granted)
        .unwrap_or_default())
}

/// `window.meshApp.readCorpus(corpusId, ids)` — gated on `mesh_store_read`.
/// Returns the requested parcel atoms with provenance. Each id matches by
/// EITHER the atom id (content-hash) OR the parcel number (canonical
/// name) — so a UI that knows only a human parcel number (e.g. a blklot)
/// can look it up without deriving the host-side hash.
#[tauri::command]
pub async fn meshapp_read_corpus(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_ids: Vec<String>,
) -> Result<Vec<ParcelDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let want: HashSet<&str> = atom_ids.iter().map(String::as_str).collect();
    let atoms = load_atoms(&state, &corpus_id).await?;
    let out = atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e)
                if want.contains(e.id.as_str())
                    || want.contains(e.canonical_name.as_str()) =>
            {
                Some(ParcelDto {
                    atom_id: e.id.as_str().to_string(),
                    parcel_number: e.canonical_name.clone(),
                    source_chunk: e.provenance.source_chunk_id.clone(),
                    attributes: e.attributes.clone(),
                })
            }
            _ => None,
        })
        .collect();
    Ok(out)
}

/// `window.meshApp.searchParcels(corpusId, query, limit?)` — gated on
/// `mesh_store_read`. Substring/number search over parcel atoms so a UI
/// (a homeowner) can find their parcel by street name or number without
/// knowing the atom-id. Matches the parcel number (exact, case-folded) OR
/// `property_location` (substring, case-folded); capped at `limit` (≤100).
#[tauri::command]
pub async fn meshapp_search_parcels(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<ParcelDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let q = query.trim().to_uppercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let cap = limit.unwrap_or(25).min(100);
    let atoms = load_atoms(&state, &corpus_id).await?;
    let mut out: Vec<ParcelDto> = atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => {
                let num_match = e.canonical_name.to_uppercase() == q;
                let addr_match = e
                    .attributes
                    .get("property_location")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_uppercase().contains(&q))
                    .unwrap_or(false);
                if num_match || addr_match {
                    Some(ParcelDto {
                        atom_id: e.id.as_str().to_string(),
                        parcel_number: e.canonical_name.clone(),
                        source_chunk: e.provenance.source_chunk_id.clone(),
                        attributes: e.attributes.clone(),
                    })
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    out.truncate(cap);
    Ok(out)
}

/// `window.meshApp.parcelAnalytics(corpusId, businessTaxTarget?)` — gated
/// on `mesh_store_read` (it reads corpus atoms). Deterministic: folds the
/// parcel atoms into the revenue-neutral land-levy aggregate via
/// corpus-engine's pure lib. No inference; the macro model's headline
/// figures are computed here, never originated by a model.
#[tauri::command]
pub async fn meshapp_parcel_analytics(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    business_tax_target: Option<f64>,
) -> Result<ParcelAnalyticsDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let target = business_tax_target.unwrap_or(DEFAULT_BUSINESS_TAX_TARGET);
    let atoms = load_atoms(&state, &corpus_id).await?;
    let parcels: Vec<_> = atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => match &e.entity_type {
                EntityType::Other(t) if t.as_str() == DEFAULT_ENTITY_TYPE => Some(e),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if parcels.is_empty() {
        return Err(format!(
            "corpus `{corpus_id}` has no `{DEFAULT_ENTITY_TYPE}` atoms"
        ));
    }

    let agg = compute_aggregates(&parcels, &corpus_id, target, DEFAULT_PROPERTY_TAX_RATE);
    let fs = flags(&parcels);
    let high = fs.iter().filter(|f| f.kind == FlagKind::HighLandShare).count();
    let under = fs.iter().filter(|f| f.kind == FlagKind::Underused).count();

    // The revenue-neutral property-tax → land-only swap is computed by the lib
    // (single source — the chat `parcel_analytics` tool reads the same fields).
    // Bind locals so the derivation/DTO below render the lib's values.
    let roll = agg.land_value_total + agg.improvement_value_total;
    let property_tax_rate = agg.property_tax_rate;
    let property_tax_revenue_est = agg.property_tax_revenue_est;
    let property_tax_swap_rate = agg.property_tax_swap_rate;

    let n = fmt_int(agg.parcel_count as f64);
    let derivation = vec![
        format!(
            "land_value_total = Σ assessed_land_value over {n} parcel atoms ({corpus_id}) = {}",
            fmt_usd(agg.land_value_total)
        ),
        format!(
            "neutral_rate = business_tax_target ÷ land_value_total = {} ÷ {} = {}",
            fmt_usd(agg.business_tax_target),
            fmt_usd(agg.land_value_total),
            fmt_pct(agg.neutral_rate)
        ),
        format!(
            "property_tax_revenue_est = (Σland + Σimprovement) × property_tax_rate = {} × {} = {}",
            fmt_usd(roll),
            fmt_pct(property_tax_rate),
            fmt_usd(property_tax_revenue_est)
        ),
        format!(
            "property_tax_swap_rate = property_tax_revenue_est ÷ land_value_total = {} ÷ {} = {}",
            fmt_usd(property_tax_revenue_est),
            fmt_usd(agg.land_value_total),
            fmt_pct(property_tax_swap_rate)
        ),
    ];

    Ok(ParcelAnalyticsDto {
        corpus_id: agg.corpus_id,
        parcel_count: agg.parcel_count,
        land_value_total: agg.land_value_total,
        improvement_value_total: agg.improvement_value_total,
        business_tax_target: agg.business_tax_target,
        neutral_rate: agg.neutral_rate,
        property_tax_rate,
        property_tax_revenue_est,
        property_tax_swap_rate,
        high_land_share_count: high,
        underused_count: under,
        derivation,
    })
}

// ─── Investigation-graph primitives (reusable: UAP now, Enron next) ───
// Read a corpus's typed investigation graph — entities + relationships +
// pattern findings — and shape it for an explorer bundle. Like the parcel
// ops these are deterministic, read-only, and gated on `mesh_store_read`;
// every edge carries its verbatim evidence excerpt + source chunk, so the
// "every link cites its source" guarantee is structural, not a prompt
// promise. The DTOs are the reusable contract a bundle codes against; a
// future Enron explorer reads its atlas+reconciliation artifacts into the
// same shapes.

/// A degree-ranked node from the investigation graph. `degree` = incident
/// relationships; `alias_count` = surface forms the coalesce phase folded
/// into this entity (the identity-grade dedup).
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

/// A node's full detail: attributes, the folded aliases, and every incident
/// edge with cited evidence.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDetailDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
    pub aliases: Vec<String>,
    pub edges: Vec<EdgeDto>,
}

/// A deterministic pattern finding (e.g. a sighting hotspot), resolved to
/// its participating entities.
#[derive(Debug, Clone, Serialize)]
pub struct FindingDto {
    pub pattern_name: String,
    /// Detector family: `threshold` | `role_overlap` | `circular_flow` | `custom_sql`.
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

/// Resolve an installed corpus's on-disk index directory, or a reason.
/// Shared by the graph/atlas/reconciliation readers below.
async fn resolve_index_path(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
) -> Result<PathBuf, String> {
    let engine = state
        .corpus_engine
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "corpus engine not initialized".to_string())?;
    let installed = engine
        .installed_indexes()
        .await
        .map_err(|e| format!("installed_indexes: {e}"))?;
    installed
        .iter()
        .find(|i| i.corpus_id == corpus_id)
        .map(|i| i.path.clone())
        .ok_or_else(|| format!("corpus `{corpus_id}` is not installed"))
}

/// Load a corpus's entity graph as (entities, relationships, findings),
/// dispatching on what the index carries: a deterministic `investigation/`
/// graph (UAP Blue Book) or an `atlas/` enrichment (Enron). Both project
/// into the SAME shapes, so the four graph ops below are source-agnostic —
/// a bundle codes against one DTO contract regardless of backend. Surfaces a
/// reason on error rather than returning empty.
async fn load_investigation(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
) -> Result<(Vec<InvEntity>, Vec<InvRelationship>, Vec<PatternFinding>), String> {
    let path = resolve_index_path(state, corpus_id).await?;
    if path.join(INVESTIGATION_DIRNAME).is_dir() {
        return read_investigation_graph(&path)
            .map_err(|e| format!("read investigation graph for `{corpus_id}`: {e}"));
    }
    if path.join("atlas").is_dir() {
        return load_atlas_as_investigation(&path)
            .map_err(|e| format!("read atlas graph for `{corpus_id}`: {e}"));
    }
    Err(format!(
        "corpus `{corpus_id}` has neither an investigation graph nor an atlas to explore"
    ))
}

// ─── Atlas → investigation-graph adapter ─────────────────────────────
// An `atlas/` enrichment carries a richer, typed atom set (Entity / Event /
// State / Relation / Claim / Question). To let the same graph ops drive an
// atlas-backed explorer, we project the slice the ops need — entities and the
// entity-to-entity edges with their cited evidence — into the investigation
// shapes. The atlas owns the reconciliation story separately (see
// [`meshapp_reconciliation`]); pattern findings have no atlas analogue.

/// Cap on participants paired into edges from a single Relation/Event atom.
/// Most carry exactly two; a large multi-party event would otherwise emit
/// O(n²) edges and over-inflate a node's degree. Pairing the first few
/// preserves the centrality signal without the blow-up.
const MAX_ATLAS_EDGE_PARTICIPANTS: usize = 8;

/// canonical_id → the cross-origin reconciliation reason, stamped onto the
/// canonical entity's `attributes.reconciliation` so a drill-down shows WHY a
/// merge happened (surface forms folded + signals that fired).
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
    /// `[[surface_form, {signal_kind: …}], …]` — we keep only the form name.
    #[serde(default)]
    surface_forms: Vec<(String, serde_json::Value)>,
    #[serde(default)]
    signals_fired: Vec<String>,
    #[serde(default)]
    source_atom_ids: Vec<String>,
}

/// Adapt an `atlas/` enrichment (atoms + chapters + reconciliation) into the
/// investigation-graph shapes the graph ops consume:
/// - Entity atoms → nodes. Coalesce `aliases` carry through; the cross-origin
///   reconciliation reason lands in `attributes.reconciliation`.
/// - Relation + Event atoms → edges between their entity participants, each
///   quoting its `passage_preview` and resolving its `sec_NNNNN` source to the
///   numeric `chunks.lance` row id (via `chapters.json`) so `read_chunk`
///   dereferences the source email unchanged.
/// - Findings → empty: the atlas identity story is served by
///   [`meshapp_reconciliation`], not pattern findings.
fn load_atlas_as_investigation(
    index_path: &Path,
) -> Result<(Vec<InvEntity>, Vec<InvRelationship>, Vec<PatternFinding>), String> {
    let atlas_dir = index_path.join("atlas");
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?;
    let sec_to_chunk = read_chapter_chunk_map(index_path)?;
    let recon = read_reconciliation_index(&atlas_dir);

    // Entities — and the id set that gates which participants become edges.
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

    // Edges — Relation + Event atoms, pairwise over their entity participants.
    // Both extractions are inlined per-arm: `AtomId`/`ChunkRef` are not
    // imported here, so we touch them only through `.as_str()` and public
    // field access, never by type name.
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

/// The participant ids that are real entities (drop dangling refs), capped so
/// one big multi-party atom can't dominate the degree distribution.
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
/// `source_chunk` is the numeric `chunks.lance` row id the section maps to
/// (via `chapters.json`) — falling back to the raw section id when unmapped,
/// so the excerpt still renders even if the full chunk can't be fetched.
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
/// the same cited evidence. Edge ids are `<atom_id>#<k>` so a multi-pair atom
/// stays addressable.
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

/// `sec_NNNNN` → first numeric `chunks.lance` row id (as a string), read from
/// `<index>/chapters.json`. The atlas stamps atom evidence with the section
/// id; this resolves it to the chunk id `read_chunk` dereferences. Absent
/// file → empty map (resolution falls back to the raw section id).
fn read_chapter_chunk_map(index_path: &Path) -> Result<HashMap<String, String>, String> {
    #[derive(Deserialize)]
    struct ChaptersFile {
        #[serde(default)]
        chapters: Vec<ChapterRow>,
    }
    #[derive(Deserialize)]
    struct ChapterRow {
        id: String,
        #[serde(default)]
        chunk_ids: Vec<u64>,
    }
    let path = index_path.join("chapters.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let file: ChaptersFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let mut map = HashMap::with_capacity(file.chapters.len());
    for c in file.chapters {
        if let Some(first) = c.chunk_ids.first() {
            map.insert(c.id, first.to_string());
        }
    }
    Ok(map)
}

/// Parse `atlas/reconciliation.json` into its merge rows. Missing file or
/// parse error → empty (reconciliation is an optional enrichment pass).
fn read_reconciliation_rows(atlas_dir: &Path) -> Vec<MergedEntityRow> {
    let path = atlas_dir.join("reconciliation.json");
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<ReconFile>(&bytes)
        .map(|f| f.merged_entities)
        .unwrap_or_default()
}

/// canonical_id → [`MergeRecord`], for stamping entity attributes.
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
fn reconciliation_merges(atlas_dir: &Path) -> Vec<ReconciliationMergeDto> {
    let mut out: Vec<ReconciliationMergeDto> = read_reconciliation_rows(atlas_dir)
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

fn to_graph_node(e: InvEntity, deg: &HashMap<&str, usize>) -> GraphNodeDto {
    GraphNodeDto {
        degree: deg.get(e.id.as_str()).copied().unwrap_or(0),
        alias_count: e.aliases.len(),
        id: e.id,
        canonical_name: e.canonical_name,
        entity_type: e.entity_type,
        attributes: e.attributes,
    }
}

/// `window.meshApp.graph(corpusId, nodeType?, limit?)` — gated on
/// `mesh_store_read`. Degree-ranked entities (optionally one type),
/// highest-degree first. Powers UAP installation hotspots, Enron
/// counterparty centrality.
#[tauri::command]
pub async fn meshapp_graph(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    node_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<GraphNodeDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let (entities, rels, _findings) = load_investigation(&state, &corpus_id).await?;
    let deg = degree_map(&rels);
    let want = node_type.as_deref();
    let mut nodes: Vec<GraphNodeDto> = entities
        .into_iter()
        .filter(|e| want.map_or(true, |t| e.entity_type.eq_ignore_ascii_case(t)))
        .map(|e| to_graph_node(e, &deg))
        .collect();
    nodes.sort_by(|a, b| {
        b.degree
            .cmp(&a.degree)
            .then_with(|| b.alias_count.cmp(&a.alias_count))
    });
    nodes.truncate(limit.unwrap_or(50).min(500));
    Ok(nodes)
}

/// `window.meshApp.node(corpusId, id)` — gated on `mesh_store_read`. One
/// entity's full detail + every incident edge, each resolved to its other
/// endpoint and quoting its evidence excerpt + source chunk.
#[tauri::command]
pub async fn meshapp_node(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    id: String,
) -> Result<NodeDetailDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let (entities, rels, _findings) = load_investigation(&state, &corpus_id).await?;
    let by_id: HashMap<&str, &InvEntity> = entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let me = by_id
        .get(id.as_str())
        .ok_or_else(|| format!("no entity `{id}` in `{corpus_id}`"))?;

    let mut edges = Vec::new();
    for r in &rels {
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

/// `window.meshApp.findings(corpusId, pattern?)` — gated on
/// `mesh_store_read`. Deterministic pattern findings (optionally one
/// pattern), each resolved to its participating entities' names.
#[tauri::command]
pub async fn meshapp_findings(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    pattern: Option<String>,
) -> Result<Vec<FindingDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let (entities, _rels, findings) = load_investigation(&state, &corpus_id).await?;
    let by_id: HashMap<&str, &InvEntity> = entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let want = pattern.as_deref();
    let out = findings
        .into_iter()
        .filter(|f| want.map_or(true, |p| f.pattern_name.as_str() == p))
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
            pattern_name: f.pattern_name,
            attributes: f.attributes,
        })
        .collect();
    Ok(out)
}

/// `window.meshApp.searchEntities(corpusId, query, nodeType?, limit?)` —
/// gated on `mesh_store_read`. Case-folded substring over an entity's
/// canonical name, aliases, and string attribute values (find a case by
/// place, a person by alias). Degree-ranked.
#[tauri::command]
pub async fn meshapp_search_entities(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    query: String,
    node_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<GraphNodeDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let (entities, rels, _findings) = load_investigation(&state, &corpus_id).await?;
    let deg = degree_map(&rels);
    let want = node_type.as_deref();
    let mut out: Vec<GraphNodeDto> = entities
        .into_iter()
        .filter(|e| want.map_or(true, |t| e.entity_type.eq_ignore_ascii_case(t)))
        .filter(|e| {
            e.canonical_name.to_lowercase().contains(&q)
                || e.aliases.iter().any(|a| a.to_lowercase().contains(&q))
                || e
                    .attributes
                    .values()
                    .any(|v| v.as_str().is_some_and(|s| s.to_lowercase().contains(&q)))
        })
        .map(|e| to_graph_node(e, &deg))
        .collect();
    out.sort_by(|a, b| b.degree.cmp(&a.degree));
    out.truncate(limit.unwrap_or(25).min(100));
    Ok(out)
}

/// One cross-origin identity merge from the atlas reconciliation pass: a
/// canonical entity, the surface forms folded into it, and the signals that
/// fired (the glassbox reason). This is a SEPARATE mechanism from an entity's
/// coalesce `aliases` — reconciliation merges atoms that were extracted
/// independently (e.g. across mailboxes), and every merge carries why.
#[derive(Debug, Clone, Serialize)]
pub struct ReconciliationMergeDto {
    pub canonical_id: String,
    pub canonical_name: String,
    pub surface_forms: Vec<String>,
    pub signals_fired: Vec<String>,
    pub source_count: usize,
}

/// `window.meshApp.reconciliation(corpusId)` — gated on `mesh_store_read`.
/// The atlas cross-origin identity merges, richest (most surface forms)
/// first. Each carries the surface forms it folded and the signals that fired
/// — "every merge carries its reason." Returns `[]` for a corpus with no
/// `atlas/reconciliation.json` (e.g. an investigation-graph corpus), so a
/// bundle can probe-and-degrade rather than error. Powers the Enron
/// explorer's reconciled-identities view.
#[tauri::command]
pub async fn meshapp_reconciliation(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Vec<ReconciliationMergeDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let path = resolve_index_path(&state, &corpus_id).await?;
    Ok(reconciliation_merges(&path.join("atlas")))
}

/// Full source-chunk text behind a cited edge. An edge's `excerpt` is the
/// short fragment the extractor tagged as evidence; this returns the WHOLE
/// chunk (e.g. an OCR'd Form-10073 card narrative) so a bundle can expand a
/// citation into the actual document.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkDto {
    pub chunk_id: String,
    pub content: String,
    pub title: Option<String>,
}

/// `window.meshApp.readChunk(corpusId, chunkId)` — gated on
/// `mesh_store_read`. Reads one chunk's full text from the corpus index by
/// its (numeric) id — the same id an edge carries in `source_chunk`.
#[tauri::command]
pub async fn meshapp_read_chunk(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: String,
) -> Result<ChunkDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let id: u64 = chunk_id
        .trim()
        .parse()
        .map_err(|_| format!("chunk id `{chunk_id}` is not a numeric id"))?;
    let engine = state
        .corpus_engine
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "corpus engine not initialized".to_string())?;
    let installed = engine
        .installed_indexes()
        .await
        .map_err(|e| format!("installed_indexes: {e}"))?;
    let entry = installed
        .iter()
        .find(|i| i.corpus_id == corpus_id)
        .ok_or_else(|| format!("corpus `{corpus_id}` is not installed"))?;
    let index = CorpusIndex::open(&entry.path)
        .await
        .map_err(|e| format!("open index `{corpus_id}`: {e}"))?;
    let chunks = index
        .get_chunks(&[id])
        .await
        .map_err(|e| format!("read chunk {id} from `{corpus_id}`: {e}"))?;
    let c = chunks
        .into_iter()
        .next()
        .ok_or_else(|| format!("no chunk {id} in `{corpus_id}`"))?;
    Ok(ChunkDto {
        chunk_id,
        content: c.content,
        title: c.title,
    })
}

// ─── Host-side install management ────────────────────────────────────
// These are called from the MAIN (host) window's UI, not the sandbox
// bridge. They mutate the grant store, so each guards against being
// called FROM a mesh-app window — otherwise (since Tauri v2 lets any
// webview invoke any app command) a hostile bundle could grant itself
// permissions. The check: the caller's label must NOT be a meshapp-*
// window. (Trusted-first-party model; this is belt-and-suspenders.)

/// `meshapp_list_installs()` — installed mesh apps + their granted
/// permission subsets, for the host's manage-apps UI.
#[tauri::command]
pub async fn meshapp_list_installs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::meshapp::MeshAppInstall>, String> {
    Ok(state.config.read().await.meshapp_installs.clone())
}

/// `meshapp_record_install(appId, name, granted)` — record (or replace)
/// an install with the GRANTED permission subset from the consent sheet.
/// Persist-first so the grant survives a restart; the granted set, not
/// the manifest's request, is what the bridge enforces.
#[tauri::command]
pub async fn meshapp_record_install(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    app_id: String,
    name: String,
    granted: MeshAppPermissions,
) -> Result<crate::meshapp::MeshAppInstall, String> {
    if app_id_from_label(webview.label()).is_some() {
        return Err("install management is host-only".into());
    }
    let recorded_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let install = crate::meshapp::MeshAppInstall {
        app_id: app_id.clone(),
        name,
        granted,
        trust: crate::meshapp::MeshAppTrust::Unsigned,
        recorded_at_unix,
    };
    let mut cfg = state.config.write().await;
    cfg.meshapp_installs.retain(|i| i.app_id != app_id);
    cfg.meshapp_installs.push(install.clone());
    cfg.save()
        .map_err(|e| format!("save desktop config: {e}"))?;
    Ok(install)
}

/// `meshapp_uninstall(appId)` — remove an install, revoking every grant.
#[tauri::command]
pub async fn meshapp_uninstall(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    app_id: String,
) -> Result<(), String> {
    if app_id_from_label(webview.label()).is_some() {
        return Err("install management is host-only".into());
    }
    let mut cfg = state.config.write().await;
    let before = cfg.meshapp_installs.len();
    cfg.meshapp_installs.retain(|i| i.app_id != app_id);
    if cfg.meshapp_installs.len() != before {
        cfg.save()
            .map_err(|e| format!("save desktop config: {e}"))?;
    }
    Ok(())
}

// ─── Window creation + sandbox ───────────────────────────────────────

/// The `window.meshApp` shim injected into every mesh-app window before
/// its own scripts run. The bundle calls these instead of touching
/// `invoke` directly. (Trusted-first-party model: a hostile bundle could
/// still reach `window.__TAURI__` since Tauri v2 doesn't gate app
/// commands per-window — tauri#9227 — so true isolation for untrusted
/// apps is the deferred no-IPC bridge milestone. For first-party apps
/// this shim is the clean, intended surface.)
// Embedded from a shared `.js` file so the Playwright wiring test injects
// the EXACT same source (single source of truth) — the mocked-`meshApp`
// specs don't exercise this shim→IPC path, which is where the
// `withGlobalTauri`-off bug hid. See `meshapp_shim.js` for the rationale.
const MESHAPP_SHIM: &str = include_str!("../meshapp_shim.js");

/// Strict CSP for a mesh-app window: scripts/styles from the bundle only
/// (no inline/eval scripts), NO external network egress — `connect-src`
/// is limited to the Tauri IPC scheme so `window.meshApp` still works but
/// the bundle cannot `fetch`/WebSocket anywhere. The only path to the
/// host is the gated bridge.
const MESHAPP_CSP: &str = "default-src 'self'; script-src 'self'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
     connect-src ipc: http://ipc.localhost; object-src 'none'; \
     base-uri 'self'; form-action 'none'";

/// `meshapp_open(appId, entry?)` — host command (main-window UI) that
/// opens the sandboxed window for an INSTALLED app. The window label is
/// `meshapp-<appId>`, which the bridge resolves the calling app from and
/// which `capabilities/meshapp.json` scopes to. Loads the bundled assets
/// at `meshapp/<appId>/<entry>`, injects the `window.meshApp` shim, and
/// clamps the window to the strict CSP. Async per Tauri's
/// WebviewWindowBuilder guidance (sync commands can deadlock on Windows).
#[tauri::command]
pub async fn meshapp_open(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    app_id: String,
    entry: Option<String>,
) -> Result<(), String> {
    // Only open INSTALLED apps — the consent/grant must exist first, so a
    // window never loads for an app with no recorded permissions.
    let installed = state
        .config
        .read()
        .await
        .meshapp_installs
        .iter()
        .any(|i| i.app_id == app_id);
    if !installed {
        return Err(format!(
            "app `{app_id}` is not installed — record install consent first"
        ));
    }

    let label = format!("{}{app_id}", crate::meshapp::MESHAPP_LABEL_PREFIX);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(());
    }

    let entry = entry.unwrap_or_else(|| "index.html".to_string());
    let url = format!("meshapp/{app_id}/{entry}");
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(format!("Mesh App — {app_id}"))
        .inner_size(1024.0, 760.0)
        .initialization_script(MESHAPP_SHIM)
        .on_web_resource_request(|_req, res| {
            res.headers_mut().insert(
                tauri::http::header::CONTENT_SECURITY_POLICY,
                tauri::http::HeaderValue::from_static(MESHAPP_CSP),
            );
        })
        .build()
        .map_err(|e| format!("open mesh-app window `{label}`: {e}"))?;
    Ok(())
}

/// `$174,097,946,887.00` — full-precision, comma-grouped USD for the
/// derivation trace (matches the chat/tool surface so the two agree).
fn fmt_usd(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let dollars = (cents / 100) as f64;
    format!("${}.{:02}", fmt_int(dollars), (cents % 100).abs())
}

fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

fn fmt_int(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but real-shaped atlas: two entities, one Relation and one
    /// Event edge (the Event references a dangling `entity-ghost` to prove
    /// non-entity participants are dropped), a `chapters.json` that maps the
    /// two sections to non-trivial chunk ids, and one reconciliation merge.
    fn write_fixture(dir: &Path) {
        let atlas = dir.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(
            atlas.join("atoms.json"),
            r#"{
              "schema_version": "2.3",
              "atoms": [
                {"atom_type":"Entity","data":{
                  "id":"entity-aaa","canonical_name":"El Paso","entity_type":"institution",
                  "first_appearance":{"chunk_id":"sec_00002","passage_preview":"El Paso Corp."},
                  "description":"Energy company.","salience":0.5,"enrichment_depth":"extracted",
                  "aliases":["El Paso Corp.","PGET"]}},
                {"atom_type":"Entity","data":{
                  "id":"entity-bbb","canonical_name":"Kenneth Lay","entity_type":"person",
                  "first_appearance":{"chunk_id":"sec_00001","passage_preview":"Ken Lay"},
                  "description":"Chairman.","salience":0.9,"enrichment_depth":"extracted"}},
                {"atom_type":"Relation","data":{
                  "id":"relation-xyz","label":"counterparty_of",
                  "participants":["entity-aaa","entity-bbb"],"relation_type":"association",
                  "evidence":[{"chunk_id":"sec_00002","passage_preview":"El Paso and Lay discussed terms"}],
                  "section_range":{"start":"sec_00002","end":"sec_00002"},"enrichment_depth":"extracted"}},
                {"atom_type":"Event","data":{
                  "id":"event-pqr","description":"Lay emailed El Paso","event_type":"unspecified",
                  "participants":["entity-bbb","entity-aaa","entity-ghost"],
                  "evidence":[{"chunk_id":"sec_00001","passage_preview":"Date: Thu, 26 Jul 2001"}],
                  "section_position":{"section_id":"sec_00001"},"enrichment_depth":"extracted"}}
              ]
            }"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapters.json"),
            r#"{"corpus_id":"t","schema_version":"1.0","chapters":[
                {"id":"sec_00001","title":"Email A","chapter":1,"chunk_ids":[100]},
                {"id":"sec_00002","title":"Email B","chapter":2,"chunk_ids":[200,201]}
            ]}"#,
        )
        .unwrap();
        std::fs::write(
            atlas.join("reconciliation.json"),
            r#"{"schema_version":1,"corpus":"t","merged_entities":[
                {"canonical_id":"entity-aaa","canonical_name":"El Paso",
                 "surface_forms":[["El Paso",{"signal_kind":"llm_batch"}],
                                  ["El Paso Corp.",{"signal_kind":"llm_batch"}]],
                 "signals_fired":["name_similarity"],
                 "source_atom_ids":["entity-aaa","entity-zzz"]}
            ]}"#,
        )
        .unwrap();
    }

    #[test]
    fn atlas_entities_map_with_type_aliases_and_reconciliation() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        let (entities, _rels, findings) = load_atlas_as_investigation(tmp.path()).unwrap();

        assert_eq!(entities.len(), 2);
        assert!(findings.is_empty(), "atlas has no pattern findings");

        let el_paso = entities.iter().find(|e| e.id == "entity-aaa").unwrap();
        assert_eq!(el_paso.entity_type, "institution");
        assert_eq!(el_paso.aliases, vec!["El Paso Corp.", "PGET"]);
        assert_eq!(
            el_paso.attributes.get("description").unwrap().as_str(),
            Some("Energy company.")
        );
        // Cross-origin reconciliation reason is stamped onto the canonical.
        let recon = el_paso.attributes.get("reconciliation").unwrap();
        assert_eq!(recon["surface_forms"].as_array().unwrap().len(), 2);
        assert_eq!(recon["signals_fired"][0], "name_similarity");
        assert_eq!(recon["source_count"], 2);

        // An entity NOT in the merge log carries no reconciliation key.
        let lay = entities.iter().find(|e| e.id == "entity-bbb").unwrap();
        assert!(lay.attributes.get("reconciliation").is_none());
    }

    #[test]
    fn atlas_edges_resolve_sec_to_chunk_and_drop_dangling_participants() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        let (_entities, rels, _findings) = load_atlas_as_investigation(tmp.path()).unwrap();

        // One Relation pair + one Event pair (ghost participant dropped) = 2.
        assert_eq!(rels.len(), 2);

        let rel = rels
            .iter()
            .find(|r| r.relationship_type == "counterparty_of")
            .unwrap();
        let pair: HashSet<&str> = [rel.from_entity_id.as_str(), rel.to_entity_id.as_str()]
            .into_iter()
            .collect();
        assert_eq!(pair, HashSet::from(["entity-aaa", "entity-bbb"]));
        // sec_00002 → first chunk id (200), the form `read_chunk` parses.
        assert_eq!(rel.evidence.chunk_id, "200");
        assert_eq!(rel.evidence.excerpt, "El Paso and Lay discussed terms");

        let ev = rels
            .iter()
            .find(|r| r.relationship_type == "unspecified")
            .unwrap();
        // The Event's LLM description rides along in attributes for the label.
        assert_eq!(
            ev.attributes.get("description").unwrap().as_str(),
            Some("Lay emailed El Paso")
        );
        assert_eq!(ev.evidence.chunk_id, "100"); // sec_00001 → 100
        let ev_pair: HashSet<&str> = [ev.from_entity_id.as_str(), ev.to_entity_id.as_str()]
            .into_iter()
            .collect();
        assert_eq!(ev_pair, HashSet::from(["entity-aaa", "entity-bbb"]));
    }

    #[test]
    fn reconciliation_merges_read_sorted_with_reasons() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        let merges = reconciliation_merges(&tmp.path().join("atlas"));
        assert_eq!(merges.len(), 1);
        let m = &merges[0];
        assert_eq!(m.canonical_name, "El Paso");
        assert_eq!(m.surface_forms, vec!["El Paso", "El Paso Corp."]);
        assert_eq!(m.signals_fired, vec!["name_similarity"]);
        assert_eq!(m.source_count, 2);
    }

    #[test]
    fn missing_sidecars_degrade_gracefully() {
        // Atlas with atoms only — no chapters.json, no reconciliation.json.
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        std::fs::remove_file(tmp.path().join("chapters.json")).unwrap();
        std::fs::remove_file(tmp.path().join("atlas").join("reconciliation.json")).unwrap();

        let (entities, rels, _f) = load_atlas_as_investigation(tmp.path()).unwrap();
        // Entities still map; no reconciliation key now.
        assert_eq!(entities.len(), 2);
        assert!(entities
            .iter()
            .all(|e| e.attributes.get("reconciliation").is_none()));
        // Edges still emit; chunk id falls back to the raw section id.
        let rel = rels
            .iter()
            .find(|r| r.relationship_type == "counterparty_of")
            .unwrap();
        assert_eq!(rel.evidence.chunk_id, "sec_00002");
        // The reconciliation op returns empty rather than erroring.
        assert!(reconciliation_merges(&tmp.path().join("atlas")).is_empty());
    }
}
