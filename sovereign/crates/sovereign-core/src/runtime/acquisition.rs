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
    /// Whether the entry's description states what the source CONTAINS
    /// (recipes: yes) vs. points at the user's own material the catalog
    /// knows nothing about (connectors: no). Content-bearing entries
    /// outrank connectors at any similarity: a connector's match is
    /// generic-form similarity, not topical evidence — measured
    /// 2026-07-20, `import_conversations` ranked top-1 on 6/6 gap turns
    /// across unrelated topics inside a 0.35-0.45 similarity mush.
    pub content_bearing: bool,
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
                content_bearing: true,
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
            content_bearing: false,
        });
        entries.push(CatalogEntry {
            route: AcquisitionRoute::ConnectVault,
            match_text: "Your personal notes: connect an Obsidian vault of personal notes, \
                         journals, and knowledge base."
                .into(),
            acquires_topic: true,
            content_bearing: false,
        });
        entries.push(CatalogEntry {
            route: AcquisitionRoute::ImportConversations,
            match_text: "Your past AI conversations: import Claude or ChatGPT conversation \
                         exports as a searchable source."
                .into(),
            acquires_topic: true,
            content_bearing: false,
        });
        Self { entries }
    }
}

/// Does the turn's coverage verdict place the authority for this gap
/// INSIDE the corpora the user already consulted?
///
/// `ClaimUncovered` is not "we found nothing" — it is the probe's
/// positive finding that an ENABLED corpus has a region within
/// `epistemic::coverage_near_sim` of the question and the claim is
/// still absent from it. The source the user chose is on-topic and
/// does not say. Nothing the resolver can name is a better authority
/// on that subject than the source already open, so the honest
/// conjecture is none at all (ARCH §18.3: absence is reported, never
/// defaulted).
///
/// **This is the same discriminator `gk_rescue::rescue_precondition_met`
/// already uses, applied to the same question.** That guard asks "can a
/// source outside the enabled corpus settle this?" of parametric
/// memory, and answers no on `ClaimUncovered` — its own doc names the
/// failure family this reproduces: "labelled-but-confident IN-WORLD
/// fabrications ('Winnie's former lover was Eddie Henderson'). Those
/// turns probe `ClaimUncovered` (in-topic, ~0.71 nearest-sim) and are
/// structurally NEVER rescued here." An acquisition route is the same
/// assertion in a different costume — "some source out there holds
/// this" — and it was the one path still making it. One decider, two
/// consumers (ARCH §10.6).
///
/// `None` is NOT this case. It means no probe verdict reached the
/// resolver (probe disabled, no engine, or a legacy call site), so no
/// authority judgement was made and none may be asserted: those turns
/// keep the pre-existing behaviour rather than going silently mute.
fn authority_already_installed(coverage: Option<GapCoverage>) -> bool {
    matches!(coverage, Some(GapCoverage::ClaimUncovered))
}

/// Rank routes for one gap. `gap_embedding` is the gap statement's
/// embedding; `entry_embeddings` parallel `catalog.entries`. Returns
/// the top routes above a floor, biased by the coverage verdict:
/// `TopicUncovered` prefers topic-acquiring routes (a recipe or
/// connector could supply the whole topic). A probe-confirmed
/// `ClaimUncovered` returns NOTHING — see
/// [`authority_already_installed`]. Web and provide-document are
/// synthesized here (they need the gap text, not a catalog row).
///
/// `coverage` is `None` when no probe verdict reached this turn; the
/// ranking then falls back to the `ClaimUncovered` bias as it always
/// has, but the silence above is never asserted on a guess.
pub(crate) fn resolve_routes(
    gap_statement: &str,
    coverage: Option<GapCoverage>,
    catalog: &AcquisitionCatalog,
    gap_embedding: &[f32],
    entry_embeddings: &[Vec<f32>],
) -> Vec<AcquisitionRoute> {
    const ROUTE_FLOOR: f32 = 0.35;
    const MAX_ROUTES: usize = 2;
    // The bias/reservation half still needs a concrete verdict; the
    // less dramatic claim is the historical default for an absent one.
    let bias = coverage.unwrap_or(GapCoverage::ClaimUncovered);
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
        let aligned = match bias {
            GapCoverage::TopicUncovered => e.acquires_topic,
            GapCoverage::ClaimUncovered => !e.acquires_topic,
        };
        if !aligned {
            *sim *= 0.75;
        }
    }
    // Tiered rank: content-bearing entries (recipes — descriptions that
    // state what the source contains) before connectors (pointers to the
    // user's own material, about which the catalog has no content
    // evidence), by similarity within each tier. A topic conjecture must
    // be groundable in the entry's description; connector matches inside
    // the observed 0.35-0.45 mush are form-similarity, not evidence
    // (2026-07-20 slate receipts: import_conversations top-1 on 6/6 gap
    // turns across unrelated topics). Connectors still rank when no
    // recipe cleared the floor.
    scored.sort_by(|a, b| {
        b.1.content_bearing
            .cmp(&a.1.content_bearing)
            .then(b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    // Glassbox: the full ranked slate, so a mis-ranked top-1 is
    // diagnosable from logs without re-instrumenting (the 2026-07-20
    // import_conversations-attractor investigation). Gap turns only,
    // ~8 short strings — cheap enough to build unconditionally.
    let slate: Vec<String> = scored
        .iter()
        .take(8)
        .map(|(s, e)| format!("{s:.3}·{}", route_label(&e.route)))
        .collect();
    tracing::debug!(
        target: "epistemic.ledger",
        gap = %gap_statement.chars().take(60).collect::<String>(),
        coverage = ?coverage,
        slate = %slate.join(" | "),
        "acquisition ranking slate (post-bias)"
    );
    // The corpus the user consulted IS the authority and does not hold
    // the claim: no route is honest. Decided AFTER the slate so the
    // withheld candidates stay readable — this branch trades away real
    // conjectures (a second broad source, a fresher web page) and that
    // cost has to be visible at `info`, not inferred from an absence.
    if authority_already_installed(coverage) {
        tracing::info!(
            target: "epistemic.ledger",
            gap = %gap_statement.chars().take(60).collect::<String>(),
            withheld = %slate.join(" | "),
            "acquisition declined — an enabled corpus is the authority here and lacks the claim; no source would settle it"
        );
        return Vec::new();
    }
    // Under the ClaimUncovered BIAS the synthesized web-search
    // conjecture is guaranteed a slot, so "check a fresher/deeper
    // source" is never squeezed out by two mediocre catalog matches
    // (caught by the unit test 2026-07-18). Since 2026-09-09 that bias
    // is reachable only via `coverage: None` — a probe-CONFIRMED
    // ClaimUncovered returned above — so this is the no-verdict path,
    // where the resolver may not assert that nothing would help.
    let catalog_cap = match bias {
        GapCoverage::ClaimUncovered => MAX_ROUTES - 1,
        GapCoverage::TopicUncovered => MAX_ROUTES,
    };
    let mut routes: Vec<AcquisitionRoute> = scored
        .into_iter()
        .take(catalog_cap)
        .map(|(_, e)| e.route.clone())
        .collect();
    if matches!(bias, GapCoverage::ClaimUncovered)
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

/// Short display label for the ranking-slate glassbox trace.
fn route_label(r: &AcquisitionRoute) -> String {
    match r {
        AcquisitionRoute::InstallRecipe { recipe_id, .. } => format!("recipe:{recipe_id}"),
        AcquisitionRoute::ConnectFolder => "connect_folder".into(),
        AcquisitionRoute::ConnectVault => "connect_vault".into(),
        AcquisitionRoute::ImportConversations => "import_conversations".into(),
        AcquisitionRoute::WebSearch { .. } => "web_search".into(),
        AcquisitionRoute::ProvideDocument { .. } => "provide_document".into(),
    }
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
    Some(sovereign_contracts::rebrand::svrnmesh_root().join("catalog-embed-cache.json"))
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
    /// The turn's coverage-probe verdict, when one ran. **Pass the
    /// probe's own `Option`, never a defaulted stand-in** — `Gap::
    /// coverage` is already `probe.unwrap_or(ClaimUncovered)`, and
    /// feeding that back in would report "no source can settle this"
    /// (see [`authority_already_installed`]) on turns where the probe
    /// never ran. `None` means exactly that: no verdict, so no
    /// authority claim, so the pre-probe ranking behaviour.
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
        if let Ok(infos) = engine.usable_indexes().await {
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
    let coverage = ctx.coverage;
    let routes = resolve_routes(
        gap_text,
        coverage,
        &catalog,
        &gap_embedding,
        &entry_embeddings,
    );
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
            Some(GapCoverage::TopicUncovered),
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

    /// The 2026-07-20 attractor fix: a connector that out-similarities
    /// every recipe inside the mush must NOT take the top slot when any
    /// recipe cleared the floor — content-bearing entries claim the
    /// topic conjecture; connectors are the fallback tier.
    #[test]
    fn content_bearing_recipes_outrank_connectors() {
        let c = catalog();
        // Give the CONNECTOR entries the highest raw similarity and the
        // one live recipe (sep) a floor-clearing but lower similarity —
        // the measured import_conversations shape.
        let dims = c.entries.len() + 1;
        let mut gap = vec![0.0; dims];
        gap[dims - 1] = 1.0;
        let entry_embs: Vec<Vec<f32>> = c
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                // Cosine against `gap` = the last-axis component.
                let sim: f32 = if e.content_bearing { 0.40 } else { 0.45 };
                let mut v = vec![0.0; dims];
                v[i] = (1.0 - sim * sim).sqrt();
                v[dims - 1] = sim;
                v
            })
            .collect();
        let routes = resolve_routes(
            "capital of Australia",
            Some(GapCoverage::TopicUncovered),
            &c,
            &gap,
            &entry_embs,
        );
        assert!(
            matches!(routes[0], AcquisitionRoute::InstallRecipe { .. }),
            "a floor-clearing recipe must take the top slot over higher-sim connectors: {routes:?}"
        );

        // And when NO recipe clears the floor, connectors still rank —
        // the tier is a preference, not an exclusion.
        let entry_embs_low: Vec<Vec<f32>> = c
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let sim: f32 = if e.content_bearing { 0.10 } else { 0.45 };
                let mut v = vec![0.0; dims];
                v[i] = (1.0 - sim * sim).sqrt();
                v[dims - 1] = sim;
                v
            })
            .collect();
        let routes = resolve_routes(
            "what did I tell you about my project",
            Some(GapCoverage::TopicUncovered),
            &c,
            &gap,
            &entry_embs_low,
        );
        assert!(
            routes
                .iter()
                .any(|r| !matches!(r, AcquisitionRoute::InstallRecipe { .. })),
            "connectors must still rank when no recipe clears the floor: {routes:?}"
        );
    }

    /// The reserved web-search slot survives EXACTLY where the
    /// authority judgement was never made: no probe verdict reached the
    /// resolver (`coverage: None` — the legacy `system_message` /
    /// collaboration call sites, or a disabled probe). Was
    /// `claim_uncovered_always_carries_web_search`; the reservation it
    /// pins is unchanged, its precondition is now "no verdict" rather
    /// than "ClaimUncovered", because a probe-confirmed ClaimUncovered
    /// resolves nothing at all (see the test below).
    #[test]
    fn web_search_is_reserved_when_no_probe_verdict_reached_the_resolver() {
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
        let routes = resolve_routes("what did the filing say", None, &c, &gap, &entry_embs);
        assert!(routes
            .iter()
            .any(|r| matches!(r, AcquisitionRoute::WebSearch { .. })));

        // And even when catalog entries DO rank (gap aligned with one),
        // the no-verdict path still reserves the web-search slot.
        let mut aligned = vec![0.0; dims];
        aligned[0] = 1.0;
        let routes = resolve_routes("what did the filing say", None, &c, &aligned, &entry_embs);
        assert!(routes
            .iter()
            .any(|r| matches!(r, AcquisitionRoute::WebSearch { .. })));
    }

    /// Entry embeddings giving each catalog entry an exact cosine
    /// against a gap vector that is 1.0 on the last (shared) axis.
    /// `sim_of(entry) -> f32` names the similarity per entry.
    fn embeddings_with_sims(
        c: &AcquisitionCatalog,
        sim_of: impl Fn(&CatalogEntry) -> f32,
    ) -> (Vec<f32>, Vec<Vec<f32>>) {
        let dims = c.entries.len() + 1;
        let mut gap = vec![0.0; dims];
        gap[dims - 1] = 1.0;
        let embs = c
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let sim = sim_of(e);
                let mut v = vec![0.0; dims];
                v[i] = (1.0 - sim * sim).sqrt();
                v[dims - 1] = sim;
                v
            })
            .collect();
        (gap, embs)
    }

    /// RED-LINE 4, the unknowable half. The chaos smoke run on
    /// 2026-09-09 resolved, for `absent-embassy-country`:
    ///
    /// ```text
    /// acquisition routes resolved routes=[InstallRecipe { recipe_id:
    ///   "federal-register-presidential", .. }, WebSearch { queries:
    ///   ["Which specific country's embassy employs Mr Vladimir?"] }]
    ///   coverage=ClaimUncovered
    /// ```
    ///
    /// Conrad never names the country; no source holds it. The US
    /// Federal Register cleared the 0.35 floor on mush-level similarity
    /// (measured over the live catalog the same night: every labeled
    /// probe's top-1 sits at 0.33-0.51 whatever its class, so the
    /// catalog carries NO signal separating the two — the discriminator
    /// is the coverage probe), and the web-search slot was reserved
    /// unconditionally. Both halves are gone: a probe-confirmed
    /// `ClaimUncovered` means the enabled corpus is the authority and
    /// lacks the claim, so nothing is resolved.
    #[test]
    fn claim_uncovered_over_the_authority_resolves_no_routes() {
        let c = catalog();
        // The observed shape: one recipe clears the floor at mush level
        // (0.47 raw, exactly where `federal-register-presidential`
        // landed), connectors just under it.
        let (gap, embs) = embeddings_with_sims(&c, |e| if e.content_bearing { 0.47 } else { 0.34 });

        // Watched to fail before the fix: this returned
        // [InstallRecipe { "sep" }, WebSearch { .. }].
        let routes = resolve_routes(
            "Which specific country's embassy employs Mr Vladimir?",
            Some(GapCoverage::ClaimUncovered),
            &c,
            &gap,
            &embs,
        );
        assert!(
            routes.is_empty(),
            "an enabled corpus is the authority and lacks the claim — no route is honest: {routes:?}"
        );

        // The scorer reads `gaps[0].routes[0]`; an empty set is what it
        // scores as the `unknowable` match. Pin the property it reads,
        // not just the length.
        assert!(routes.first().is_none());
    }

    /// RED-LINE 4, the satisfiable half — the direction the fix must
    /// NOT move. `ood-australia-capital` ("What is the capital of
    /// Australia?", `acquisition_class = "install_recipe"`) probes
    /// off-topic against the sealed Conrad corpus (0.2015..0.3323 over
    /// the five OOD probes, `epistemic::coverage_near_sim`), so its
    /// verdict is `TopicUncovered` and a floor-clearing recipe must
    /// still be conjectured — at the same mush-level similarity that
    /// the ClaimUncovered test above rejects. The coverage verdict is
    /// the whole difference between the two.
    #[test]
    fn topic_uncovered_still_conjectures_the_recipe() {
        let c = catalog();
        let (gap, embs) = embeddings_with_sims(&c, |e| if e.content_bearing { 0.47 } else { 0.34 });
        let routes = resolve_routes(
            "What is the capital of Australia?",
            Some(GapCoverage::TopicUncovered),
            &c,
            &gap,
            &embs,
        );
        assert!(
            matches!(
                routes.first(),
                Some(AcquisitionRoute::InstallRecipe { recipe_id, .. }) if recipe_id == "sep"
            ),
            "an uncovered topic must still name a source to install: {routes:?}"
        );
    }
}
