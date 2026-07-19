//! Acquisition resolver — maps a knowledge gap to concrete, catalog-
//! grounded places the user could fetch the missing knowledge.
//!
//! Design: `sovereign/docs/EPISTEMIC_STATE.md` §4.3 / D4. Two
//! commitments are structural:
//!
//! - **I4 — routes come only from the catalog.** The resolver ranks a
//!   fixed candidate set (installable registry recipes + the product's
//!   connector affordances + web/paste); no model ever invents a
//!   route. A unit test pins this.
//! - **No new model calls on the answer path.** Matching uses the
//!   embed slot only, on GAP turns only, and catalog-description
//!   embeddings are computed lazily once then cached on disk keyed by
//!   the embed-model id (the router-embed-cache validity idea, scoped
//!   down: no committed bake in I1).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::traits::InferenceProvider;
use crate::types::{AcquisitionRoute, GapCoverage};

/// One candidate acquisition target.
#[derive(Debug, Clone)]
pub(crate) struct CatalogEntry {
    /// The route this entry resolves to when ranked in.
    pub route: AcquisitionRoute,
    /// Text embedded for matching (name + description).
    pub match_text: String,
    /// Whether this entry acquires a NEW topical source (recipes,
    /// connectors) vs. deepens/verifies an existing topic (web,
    /// provide-document). Drives the GapCoverage bias.
    pub acquires_topic: bool,
}

/// The resolver's candidate set: installable catalog recipes (minus
/// already-installed ones, minus hidden entries) + the standing
/// connector affordances.
pub(crate) struct AcquisitionCatalog {
    pub entries: Vec<CatalogEntry>,
}

impl AcquisitionCatalog {
    /// Build from the recipe registry + installed-corpus diff. Pure
    /// data assembly — no I/O beyond the engine's already-loaded
    /// registry snapshot and index listing.
    pub(crate) fn build(
        registry_entries: &[(String, String, String, String)],
        installed: &std::collections::HashSet<String>,
    ) -> Self {
        let mut entries: Vec<CatalogEntry> = Vec::new();
        for (id, name, description, catalog_status) in registry_entries {
            if catalog_status == "hidden" || installed.contains(id) {
                continue;
            }
            entries.push(CatalogEntry {
                route: AcquisitionRoute::InstallRecipe {
                    recipe_id: id.clone(),
                    name: name.clone(),
                },
                match_text: format!("{name}. {description}"),
                acquires_topic: true,
            });
        }
        // Connector affordances — the Library Add-sheet's standing
        // capabilities, described for embedding-match purposes.
        entries.push(CatalogEntry {
            route: AcquisitionRoute::ConnectFolder,
            match_text: "Your own files: connect a local folder of documents, notes, PDFs, \
                         reports, or papers you already have on this machine."
                .into(),
            acquires_topic: true,
        });
        entries.push(CatalogEntry {
            route: AcquisitionRoute::ConnectVault,
            match_text: "Your personal notes: connect an Obsidian vault of personal notes, \
                         journals, and knowledge base."
                .into(),
            acquires_topic: true,
        });
        entries.push(CatalogEntry {
            route: AcquisitionRoute::ImportConversations,
            match_text: "Your past AI conversations: import Claude or ChatGPT conversation \
                         exports as a searchable source."
                .into(),
            acquires_topic: true,
        });
        Self { entries }
    }
}

/// Rank routes for one gap. `gap_embedding` is the gap statement's
/// embedding; `entry_embeddings` parallel `catalog.entries`. Returns
/// the top routes above a floor, biased by the coverage verdict:
/// `TopicUncovered` prefers topic-acquiring routes (a recipe or
/// connector could supply the whole topic); `ClaimUncovered` prefers
/// web/document routes (the topic exists locally — the specific claim
/// needs a deeper or fresher source). Web and provide-document are
/// synthesized here (they need the gap text, not a catalog row).
pub(crate) fn resolve_routes(
    gap_statement: &str,
    coverage: GapCoverage,
    catalog: &AcquisitionCatalog,
    gap_embedding: &[f32],
    entry_embeddings: &[Vec<f32>],
) -> Vec<AcquisitionRoute> {
    const ROUTE_FLOOR: f32 = 0.35;
    const MAX_ROUTES: usize = 2;
    let mut scored: Vec<(f32, &CatalogEntry)> = catalog
        .entries
        .iter()
        .zip(entry_embeddings.iter())
        .filter_map(|(e, emb)| {
            let sim = cosine(gap_embedding, emb)?;
            (sim >= ROUTE_FLOOR).then_some((sim, e))
        })
        .collect();
    // Coverage bias: a multiplicative nudge, not a hard filter — a
    // very strong recipe match should survive a ClaimUncovered bias.
    for (sim, e) in scored.iter_mut() {
        let aligned = match coverage {
            GapCoverage::TopicUncovered => e.acquires_topic,
            GapCoverage::ClaimUncovered => !e.acquires_topic,
        };
        if !aligned {
            *sim *= 0.75;
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    // On ClaimUncovered the synthesized web-search conjecture is
    // guaranteed a slot — the topic exists locally, so "check a
    // fresher/deeper source" must never be squeezed out by two
    // mediocre catalog matches (caught by the unit test 2026-07-18).
    let catalog_cap = match coverage {
        GapCoverage::ClaimUncovered => MAX_ROUTES - 1,
        GapCoverage::TopicUncovered => MAX_ROUTES,
    };
    let mut routes: Vec<AcquisitionRoute> = scored
        .into_iter()
        .take(catalog_cap)
        .map(|(_, e)| e.route.clone())
        .collect();
    if matches!(coverage, GapCoverage::ClaimUncovered)
        && !routes
            .iter()
            .any(|r| matches!(r, AcquisitionRoute::WebSearch { .. }))
    {
        routes.push(AcquisitionRoute::WebSearch {
            queries: vec![gap_statement.chars().take(120).collect()],
        });
    }
    // Nothing ranked at all → web search is the honest fallback.
    if routes.is_empty() {
        routes.push(AcquisitionRoute::WebSearch {
            queries: vec![gap_statement.chars().take(120).collect()],
        });
    }
    routes.truncate(MAX_ROUTES);
    routes
}

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

// ─── Lazy per-model embedding cache ──────────────────────────

/// On-disk cache of catalog-entry embeddings, keyed by embed model id
/// and entry text hash — first gap turn on a machine pays the one
/// batch embed (~30 texts), every later turn reads disk.
#[derive(Serialize, Deserialize, Default)]
struct CatalogEmbedCache {
    schema_version: u32,
    /// The embed model these vectors came from; a mismatch discards
    /// the cache (the router-embed-cache model-swap guard, simplified).
    embed_model: String,
    /// sha-like key (length + first/last chars) → embedding. Keyed by
    /// entry match_text so recipe description edits invalidate rows.
    entries: std::collections::HashMap<String, Vec<f32>>,
}

fn cache_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".sovereign").join("catalog-embed-cache.json"))
}

fn entry_key(text: &str) -> String {
    // Cheap content key: len + djb2 hash (stable, no new deps).
    let mut h: u64 = 5381;
    for b in text.bytes() {
        h = h.wrapping_mul(33) ^ (b as u64);
    }
    format!("{}:{h:x}", text.len())
}

/// `SOVEREIGN_ACQUISITION_ROUTES=0|false|off|no` disables route
/// resolution (gaps ship without conjectures; the card degrades to
/// its pre-routes layout).
pub(crate) fn acquisition_routes_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_ACQUISITION_ROUTES")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Everything route resolution needs beyond the inference handle.
pub struct RouteContext {
    /// Engine handle for the recipe catalog + installed-corpus diff.
    /// `None` = connectors-only catalog (still useful).
    pub engine: Option<Arc<corpus_engine::CorpusEngine>>,
    /// The turn's coverage-probe verdict, when one ran. `None`
    /// defaults to `ClaimUncovered` — the less dramatic claim.
    pub coverage: Option<GapCoverage>,
}

/// Resolve acquisition routes for one gap: build the catalog (recipes
/// minus installed, plus connectors), embed lazily, rank against the
/// gap text. Every failure path returns `[]` — the card ships without
/// routes rather than the turn paying for the resolver (invariant I5).
pub async fn routes_for_gap(
    inference: &dyn InferenceProvider,
    ctx: &RouteContext,
    gap_text: &str,
) -> Vec<AcquisitionRoute> {
    if !acquisition_routes_enabled() || gap_text.trim().is_empty() {
        return Vec::new();
    }
    let started = std::time::Instant::now();
    let mut registry_rows: Vec<(String, String, String, String)> = Vec::new();
    let mut installed: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(engine) = &ctx.engine {
        for e in engine.registry().list_entries() {
            registry_rows.push((
                e.id.clone(),
                e.name.clone(),
                e.description.clone(),
                e.catalog_status.clone().unwrap_or_default(),
            ));
        }
        if let Ok(infos) = engine.installed_indexes().await {
            installed.extend(infos.into_iter().map(|i| i.corpus_id));
        }
    }
    let catalog = AcquisitionCatalog::build(&registry_rows, &installed);
    let Some(entry_embeddings) = catalog_embeddings(inference, &catalog).await else {
        return Vec::new();
    };
    let gap_embedding = match inference.embed(gap_text).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "epistemic.ledger", error = %e, "gap embed failed — no routes");
            return Vec::new();
        }
    };
    let coverage = ctx.coverage.unwrap_or(GapCoverage::ClaimUncovered);
    let routes = resolve_routes(gap_text, coverage, &catalog, &gap_embedding, &entry_embeddings);
    tracing::info!(
        target: "epistemic.ledger",
        routes = ?routes,
        coverage = ?coverage,
        catalog_entries = catalog.entries.len(),
        resolve_ms = started.elapsed().as_millis() as u64,
        "acquisition routes resolved"
    );
    routes
}

/// Embed the catalog entries, reading/writing the disk cache. Returns
/// one embedding per entry (order-parallel). Missing entries are
/// embedded via `inference.embed` and persisted best-effort. Any
/// failure returns `None` — the caller ships the gap without routes
/// (never blocks the turn; invariant I5).
pub(crate) async fn catalog_embeddings(
    inference: &dyn InferenceProvider,
    catalog: &AcquisitionCatalog,
) -> Option<Vec<Vec<f32>>> {
    let model_id = inference.embed_model_id();
    let path = cache_path();
    let mut cache: CatalogEmbedCache = match &path {
        Some(p) => std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .filter(|c: &CatalogEmbedCache| c.embed_model == model_id && c.schema_version == 1)
            .unwrap_or_default(),
        None => CatalogEmbedCache::default(),
    };
    cache.schema_version = 1;
    cache.embed_model = model_id;
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(catalog.entries.len());
    let mut wrote = false;
    for e in &catalog.entries {
        let key = entry_key(&e.match_text);
        if let Some(v) = cache.entries.get(&key) {
            out.push(v.clone());
            continue;
        }
        match inference.embed(&e.match_text).await {
            Ok(v) => {
                cache.entries.insert(key, v.clone());
                out.push(v);
                wrote = true;
            }
            Err(err) => {
                tracing::debug!(target: "epistemic.ledger", error = %err, "catalog embed failed — gap ships without routes");
                return None;
            }
        }
    }
    if wrote {
        if let Some(p) = path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_string(&cache) {
                let _ = std::fs::write(&p, json);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> AcquisitionCatalog {
        AcquisitionCatalog::build(
            &[
                (
                    "sep".into(),
                    "Stanford Encyclopedia of Philosophy".into(),
                    "Comprehensive philosophical reference covering 1,800+ entries.".into(),
                    "featured".into(),
                ),
                (
                    "wikipedia".into(),
                    "Wikipedia".into(),
                    "General encyclopedia, vital articles.".into(),
                    "featured".into(),
                ),
                (
                    "hidden-one".into(),
                    "Hidden".into(),
                    "Should never appear.".into(),
                    "hidden".into(),
                ),
            ],
            &["wikipedia".to_string()].into_iter().collect(),
        )
    }

    #[test]
    fn catalog_skips_hidden_and_installed() {
        let c = catalog();
        let recipe_ids: Vec<&str> = c
            .entries
            .iter()
            .filter_map(|e| match &e.route {
                AcquisitionRoute::InstallRecipe { recipe_id, .. } => Some(recipe_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(recipe_ids, vec!["sep"]);
        // Connectors always present.
        assert!(c
            .entries
            .iter()
            .any(|e| matches!(e.route, AcquisitionRoute::ConnectVault)));
    }

    /// Invariant I4: every resolved route is a catalog entry or one of
    /// the two synthesized fallbacks (web / provide-document) — the
    /// resolver structurally cannot emit a route outside that set.
    #[test]
    fn routes_come_only_from_the_catalog() {
        let c = catalog();
        // Orthonormal fake embeddings: gap aligned with entry 0.
        let dims = c.entries.len();
        let entry_embs: Vec<Vec<f32>> = (0..dims)
            .map(|i| (0..dims).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
            .collect();
        let mut gap = vec![0.0; dims];
        gap[0] = 1.0;
        let routes = resolve_routes(
            "philosophy of mind",
            GapCoverage::TopicUncovered,
            &c,
            &gap,
            &entry_embs,
        );
        assert!(!routes.is_empty());
        for r in &routes {
            let in_catalog = c.entries.iter().any(|e| &e.route == r);
            let synthesized = matches!(
                r,
                AcquisitionRoute::WebSearch { .. } | AcquisitionRoute::ProvideDocument { .. }
            );
            assert!(in_catalog || synthesized, "route outside catalog: {r:?}");
        }
    }

    #[test]
    fn claim_uncovered_always_carries_web_search() {
        let c = catalog();
        let dims = c.entries.len() + 1;
        // Entries live on axes 0..n; the gap on the extra axis —
        // truly orthogonal (cosine is scale-invariant, so "small
        // values" are NOT weak alignment; orthogonality is).
        let entry_embs: Vec<Vec<f32>> = (0..c.entries.len())
            .map(|i| (0..dims).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
            .collect();
        let mut gap = vec![0.0; dims];
        gap[dims - 1] = 1.0;
        let routes = resolve_routes(
            "what did the filing say",
            GapCoverage::ClaimUncovered,
            &c,
            &gap,
            &entry_embs,
        );
        assert!(routes
            .iter()
            .any(|r| matches!(r, AcquisitionRoute::WebSearch { .. })));

        // And even when catalog entries DO rank (gap aligned with one),
        // ClaimUncovered still reserves the web-search slot.
        let mut aligned = vec![0.0; dims];
        aligned[0] = 1.0;
        let routes = resolve_routes(
            "what did the filing say",
            GapCoverage::ClaimUncovered,
            &c,
            &aligned,
            &entry_embs,
        );
        assert!(routes
            .iter()
            .any(|r| matches!(r, AcquisitionRoute::WebSearch { .. })));
    }
}
