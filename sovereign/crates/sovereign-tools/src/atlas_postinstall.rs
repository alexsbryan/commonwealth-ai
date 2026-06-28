// SPDX-License-Identifier: AGPL-3.0-or-later
//! Post-install atlas hooks — build the structural atlas (and, in
//! follow-up tracks, run triage + spawn Tier-2 extraction) the moment
//! a corpus install finishes. No CLI handholding required.
//!
//! Today this module ships the **structural-atlas** step: takes a
//! freshly-installed chunk corpus and runs the deterministic
//! `structure_first` strategy against it, writing
//! `<corpus>/atlas/{atoms,edges}.json`. The discovery walk in
//! [`crate::atlas_context_manager::AtlasContextManager`] picks the
//! result up automatically on the next process start; in-flight
//! processes will catch it on the next `init_from_cache` call.
//!
//! Idempotency: the hook checks for an existing
//! `<corpus>/atlas/atoms.json` and short-circuits if present. To
//! force a rebuild, delete the atlas dir.
//!
//! Triage candidate generation and Tier-2 background extraction
//! follow in Tracks A5 + B; the surface here is the chain anchor
//! they extend.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, vital_tier, AtlasIngestionConfig, AtlasIngestionRegistry,
    AtomEnvelope,
};
use corpus_engine::progress::IngestProgress;
use corpus_engine::{CorpusEngine, EmbedFn, ProgressCallback};

/// Outcome of a structural-atlas post-install run.
#[derive(Debug)]
pub enum StructuralAtlasOutcome {
    /// Atlas was built fresh and persisted.
    Built {
        atoms_path: PathBuf,
        edges_path: PathBuf,
        elapsed_secs: f64,
    },
    /// Atlas already existed at the target path; nothing rebuilt.
    AlreadyPresent { atoms_path: PathBuf },
    /// Build was attempted but failed (already logged with detail).
    Failed { reason: String },
}

/// Build the structural atlas for `corpus_id` and write it to
/// `<indexes_dir>/<corpus_id>/atlas/`. Source and target are the
/// same — the atlas lives alongside the chunk store, so the runtime
/// atlas-context discovery walk finds it without extra plumbing.
///
/// `recipes_dir` is the standard recipe-search root (typically
/// `<data_dir>/recipes`). It's not load-bearing for `structure_first`
/// today but the `CorpusEngine` constructor requires it.
pub async fn build_structural_atlas(
    corpus_id: &str,
    indexes_dir: PathBuf,
    recipes_dir: PathBuf,
) -> StructuralAtlasOutcome {
    build_structural_atlas_inner(corpus_id, indexes_dir, recipes_dir, false).await
}

/// Force a structural atlas rebuild, overwriting any existing
/// `atoms.json` / `edges.json` at the corpus's atlas dir. Used by
/// the newsworthy watcher's `on_chunks_committed` hook to refresh
/// the atlas after the watcher mutates the underlying chunks — the
/// install-time [`build_structural_atlas`] bails on AlreadyPresent
/// so it can't be reused for the post-write rebuild path.
///
/// Same machinery as `build_structural_atlas` otherwise — uses the
/// `structure_first` strategy, walks chunk metadata (no embedding),
/// writes atomically via `write_atomic_json` so an in-progress
/// rebuild can't half-corrupt the atlas. On a 1.85M-chunk wikipedia
/// corpus the rebuild takes ~30s-2min reading metadata; callers
/// should run this on a detached task rather than blocking a
/// request handler.
pub async fn rebuild_structural_atlas(
    corpus_id: &str,
    indexes_dir: PathBuf,
    recipes_dir: PathBuf,
) -> StructuralAtlasOutcome {
    build_structural_atlas_inner(corpus_id, indexes_dir, recipes_dir, true).await
}

async fn build_structural_atlas_inner(
    corpus_id: &str,
    indexes_dir: PathBuf,
    recipes_dir: PathBuf,
    force: bool,
) -> StructuralAtlasOutcome {
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    let atoms_path = atlas_dir.join("atoms.json");
    if !force && atoms_path.exists() {
        // The atlas JSON is already present (shipped/prebuilt corpus, or a
        // prior build that didn't go through `write_atlas_full`'s archive
        // sidecar). Ensure the zero-copy `atoms.rkyv` exists now, off the
        // query thread, so the first query mmaps it instead of paying the
        // convert-on-load parse (ATLAS_STORAGE.md Phase 1.5). Best-effort —
        // the reader self-heals via convert-on-load if this is skipped.
        if corpus_engine::enrichment::atlas::archive::archive_needs_build(&atlas_dir) {
            match corpus_engine::enrichment::atlas::archive::build_and_write_archive(
                &atlas_dir, corpus_id,
            ) {
                Ok(p) => tracing::info!(
                    corpus = corpus_id,
                    path = %p.display(),
                    "atlas archive built post-install"
                ),
                Err(e) => {
                    tracing::warn!(corpus = corpus_id, "atlas archive build skipped: {e}")
                }
            }
        }
        // ATLAS_STORAGE_V2 Stage 0 (dormant, gated): build the v2 store beside
        // the rkyv for an already-present atlas. No-op unless the env is set.
        if corpus_engine::enrichment::atlas::store::store_v2_enabled()
            && corpus_engine::enrichment::atlas::store::store_needs_build(&atlas_dir)
        {
            match corpus_engine::enrichment::atlas::store::build_and_write_store(
                &atlas_dir, corpus_id,
            )
            .await
            {
                Ok(p) => tracing::info!(
                    corpus = corpus_id,
                    path = %p.display(),
                    "atlas v2 store built post-install"
                ),
                Err(e) => {
                    tracing::warn!(corpus = corpus_id, "atlas v2 store build skipped: {e}")
                }
            }
        }
        return StructuralAtlasOutcome::AlreadyPresent { atoms_path };
    }

    let registry = AtlasIngestionRegistry::builtin();
    let strategy = match registry.get("structure_first") {
        Some(s) => s,
        None => {
            return StructuralAtlasOutcome::Failed {
                reason: "structure_first strategy not registered".into(),
            };
        }
    };

    // structure_first reads chunk metadata, never embeds — wire a
    // no-op EmbedFn so the engine constructor doesn't require a
    // model. Same pattern as the CLI's `enrich ingest` path.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = Arc::new(CorpusEngine::new(
        recipes_dir,
        indexes_dir.clone(),
        noop_embed.clone(),
    ));

    let cfg = AtlasIngestionConfig {
        strategy_id: "structure_first".into(),
        strategy_config: serde_json::json!({
            "source_corpus_id": corpus_id,
        }),
    };

    let started = std::time::Instant::now();
    let progress: Arc<ProgressCallback> = Arc::new(Box::new(move |ev: IngestProgress| {
        tracing::debug!(?ev, "structural_atlas: progress");
    }));

    let result = strategy
        .ingest(engine, noop_embed, None, cfg, progress)
        .await;
    let elapsed_secs = started.elapsed().as_secs_f64();

    let data = match result {
        Ok(d) => d,
        Err(e) => {
            return StructuralAtlasOutcome::Failed {
                reason: format!("strategy.ingest failed: {e}"),
            };
        }
    };

    if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
        return StructuralAtlasOutcome::Failed {
            reason: format!("create atlas dir {}: {e}", atlas_dir.display()),
        };
    }

    let edges_path = atlas_dir.join("edges.json");
    if let Err(e) = write_atomic_json(&atoms_path, &data.atoms) {
        return StructuralAtlasOutcome::Failed {
            reason: format!("write atoms.json: {e}"),
        };
    }
    if let Err(e) = write_atomic_json(&edges_path, &data.edges) {
        return StructuralAtlasOutcome::Failed {
            reason: format!("write edges.json: {e}"),
        };
    }
    StructuralAtlasOutcome::Built {
        atoms_path,
        edges_path,
        elapsed_secs,
    }
}

/// Default Tier-2 budget when no per-corpus override is on disk:
/// how many top-priority articles the triage step picks for the
/// deep-enrichment queue. Calibrated against the wikipedia-core eval
/// bank — at 1,000 articles the L1+L2+L3 vital sets all fit, with
/// enough headroom for top-L4 by centrality. Operators override via
/// `<corpus>/atlas/triage-config.json` or the
/// `sovereign atlas budget` CLI helper.
pub const DEFAULT_TIER2_BUDGET: usize = 1000;

/// Filename of the per-corpus Tier-2 budget override. Lives next to
/// `atoms.json` so it travels with the atlas and is readable by both
/// the post-install hook and an operator inspecting the on-disk
/// state.
pub const TRIAGE_CONFIG_FILE: &str = "triage-config.json";

/// Persisted shape of the triage override. `schema_version = 1` lets
/// future additions land without breaking existing files. All fields
/// are `Option` so partial overrides are fine — fields left absent
/// fall through to the constants defined alongside.
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct TriageConfig {
    pub schema_version: u32,
    /// Cap on `top_in_corpus_by_centrality` after seed + expansion
    /// picks merge. `None` (or absent file) → [`DEFAULT_TIER2_BUDGET`].
    pub budget_articles: Option<usize>,
    /// Fraction of `budget_articles` reserved for seed-expansion picks
    /// (1-hop outbound wikilinks from the seed set). `None` →
    /// [`DEFAULT_EXPANSION_FRACTION`]. Set to `0.0` to disable
    /// expansion and recover the old "pure tier+centrality" behaviour.
    /// Clamped to [0.0, 0.9] at read time — at least 10% of the
    /// budget always goes to seeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_fraction: Option<f64>,
    /// How many wikilink hops to walk outward from each seed when
    /// computing the expansion candidate pool. `None` →
    /// [`DEFAULT_EXPANSION_HOPS`]. Values >2 explode quickly on
    /// wiki-scale graphs; the loader caps at 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_hops: Option<u32>,
}

/// Default share of the Tier-2 budget reserved for expansion picks.
/// 30% leaves room for the full vital roster (L1+L2+L3 = ~1100
/// articles fit comfortably in the 70% seed cap of a 1000 budget)
/// while still capturing several hundred connective-tissue articles
/// (Einstein → Bohr, photoelectric effect, special relativity).
pub const DEFAULT_EXPANSION_FRACTION: f64 = 0.3;

/// Default expansion depth. 1-hop is the right tradeoff: the
/// connective tissue we want IS the articles vital ones link
/// directly to. 2-hop dilutes the signal (transitive neighbours
/// are usually only loosely related to the seed) and quadruples
/// the candidate pool size.
pub const DEFAULT_EXPANSION_HOPS: u32 = 1;

/// Read `<atlas_dir>/triage-config.json` and return the full
/// resolved config (with defaults filled in). Missing / malformed
/// file → defaults across the board.
pub fn read_triage_config(atlas_dir: &Path) -> ResolvedTriageConfig {
    let path = atlas_dir.join(TRIAGE_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).ok();
    let cfg: TriageConfig = raw
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    ResolvedTriageConfig {
        budget_articles: cfg.budget_articles.unwrap_or(DEFAULT_TIER2_BUDGET),
        expansion_fraction: cfg
            .expansion_fraction
            .unwrap_or(DEFAULT_EXPANSION_FRACTION)
            .clamp(0.0, 0.9),
        expansion_hops: cfg.expansion_hops.unwrap_or(DEFAULT_EXPANSION_HOPS).min(2),
    }
}

/// Resolved triage knobs after defaults + clamping. Returned by
/// [`read_triage_config`]; the post-install chain reads this once
/// per corpus install and threads it into the rest of the pipeline.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTriageConfig {
    pub budget_articles: usize,
    pub expansion_fraction: f64,
    pub expansion_hops: u32,
}

/// Read just the `budget_articles` field for back-compat with the
/// pre-expansion CLI. Prefer [`read_triage_config`] for new callers.
pub fn read_triage_budget(atlas_dir: &Path) -> Option<usize> {
    let path = atlas_dir.join(TRIAGE_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).ok()?;
    let cfg: TriageConfig = serde_json::from_str(&raw).ok()?;
    cfg.budget_articles
}

/// Persist a Tier-2 budget override (legacy entry point — preserves
/// any expansion knobs already on disk). New callers should use
/// [`write_triage_config`] which sets all three fields atomically.
pub fn write_triage_budget(atlas_dir: &Path, budget_articles: usize) -> std::io::Result<()> {
    // Read existing config so we don't trample expansion knobs.
    let path = atlas_dir.join(TRIAGE_CONFIG_FILE);
    let mut cfg: TriageConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    cfg.schema_version = 1;
    cfg.budget_articles = Some(budget_articles);
    let value = serde_json::to_value(&cfg).map_err(std::io::Error::other)?;
    write_atomic_json(&path, &value)
}

/// Persist the full triage config (budget + expansion knobs).
/// Atomic via sibling `.tmp` + rename. Use this for setting
/// expansion-related knobs to make sure all three fields land
/// in one consistent write.
pub fn write_triage_config(
    atlas_dir: &Path,
    budget_articles: Option<usize>,
    expansion_fraction: Option<f64>,
    expansion_hops: Option<u32>,
) -> std::io::Result<()> {
    let path = atlas_dir.join(TRIAGE_CONFIG_FILE);
    let mut cfg: TriageConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    cfg.schema_version = 1;
    if let Some(b) = budget_articles {
        cfg.budget_articles = Some(b);
    }
    if let Some(f) = expansion_fraction {
        cfg.expansion_fraction = Some(f);
    }
    if let Some(h) = expansion_hops {
        cfg.expansion_hops = Some(h);
    }
    let value = serde_json::to_value(&cfg).map_err(std::io::Error::other)?;
    write_atomic_json(&path, &value)
}

/// Resolve the effective Tier-2 budget for `corpus_id`: the per-
/// corpus override if set, else [`DEFAULT_TIER2_BUDGET`]. Used by
/// the post-install chain so the structural-atlas → triage step
/// honours operator overrides without each call site re-implementing
/// the lookup.
pub fn effective_tier2_budget(indexes_dir: &Path, corpus_id: &str) -> usize {
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    read_triage_budget(&atlas_dir).unwrap_or(DEFAULT_TIER2_BUDGET)
}

/// Outcome of a triage post-install run.
#[derive(Debug)]
pub enum TriageOutcome {
    /// Triage was computed and persisted.
    Built {
        path: PathBuf,
        in_corpus_picked: usize,
        elapsed_secs: f64,
    },
    /// Atlas wasn't there yet (build_structural_atlas failed earlier).
    NoAtlas,
    /// Triage failed (already logged with detail).
    Failed { reason: String },
}

/// Read the structural atlas at `<indexes_dir>/<corpus_id>/atlas/`,
/// pick a seed set by (Vital Articles tier × centrality × bumps),
/// expand 1-hop outbound through the wikilink graph to gather the
/// connective-tissue articles each seed actually points at, rank
/// the expansion candidates by hits-from-seeds + tier + centrality,
/// and persist the merged top-`budget` canonical names to
/// `<corpus>/triage-candidates.json`.
///
/// ## Why two-phase
///
/// Pure tier+centrality picks the 1000 most central articles. That
/// IS the vital roster for the head, but it leaves no slots for the
/// articles each vital one actually links to (Einstein → Bohr,
/// special relativity, photoelectric effect — none of which are
/// L1+L2). Two-phase keeps the head AND captures the directly-cited
/// neighbourhood, which is what makes Q&A about a vital topic
/// retrievable in depth.
///
/// ## Scoring
///
/// **Seeds**: pick top-`(1 - expansion_fraction) * budget` by
/// `(6 - tier) * BIG + centrality + bumps * BUMP_WEIGHT`. Tier
/// dominates so all of L1+L2+L3 in-corpus lands in the seed set
/// before any centrality-only off-list candidate.
///
/// **Expansion**: walk outbound wikilinks from every seed,
/// counting `hits_from_seeds[target]++` per (seed, target) edge.
/// Drop placeholders (off-corpus targets) and entities already in
/// the seed set. Rank survivors by
/// `hits_from_seeds * SEED_HIT_WEIGHT + (6 - tier) * SMALL_TIER_WEIGHT
/// + centrality + bumps * BUMP_WEIGHT`. Take top `expansion_cap`.
///
/// **Combined output**: seeds (in seed-score order) followed by
/// expansion picks. The downstream `enrich init --include-articles`
/// consumer just reads the array — order matters only for the
/// extract scheduler's first-N preference (seeds get processed
/// first, so even a partial run gets the highest-priority work).
///
/// ## Schema
///
/// Output matches `sovereign enrich init --include-articles <path>`:
/// `{ "schema_version": 1, "corpus_id": …, "top_in_corpus_by_centrality": [...] }`.
/// The schema_version stays 1 because the consumer only reads the
/// `top_in_corpus_by_centrality` array; new diagnostic fields land
/// alongside without bumping the contract.
pub async fn build_triage_candidates(
    corpus_id: &str,
    indexes_dir: PathBuf,
    budget: usize,
) -> TriageOutcome {
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    if !atlas_dir.join("atoms.json").exists() {
        return TriageOutcome::NoAtlas;
    }

    let started = std::time::Instant::now();
    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            return TriageOutcome::Failed {
                reason: format!("read atoms.json: {e}"),
            }
        }
    };
    let edges = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(e) => {
            return TriageOutcome::Failed {
                reason: format!("read edges.json: {e}"),
            }
        }
    };

    // Read expansion knobs alongside the budget. We pass `budget`
    // explicitly (the caller resolves it via `effective_tier2_budget`)
    // but read fraction + hops here so a single triage rebuild
    // honours operator overrides on those knobs without the caller
    // needing to thread them through.
    let cfg = read_triage_config(&atlas_dir);
    let expansion_fraction = cfg.expansion_fraction;
    let expansion_hops = cfg.expansion_hops;

    // Index entities, splitting placeholders from in-corpus.
    struct Ent {
        canonical_name: String,
        is_placeholder: bool,
    }
    let mut by_id: HashMap<String, Ent> = HashMap::with_capacity(atoms.atoms.len());
    for atom in &atoms.atoms {
        if let AtomEnvelope::Entity(e) = atom {
            by_id.insert(
                e.id.as_str().to_string(),
                Ent {
                    canonical_name: e.canonical_name.clone(),
                    is_placeholder: e.description.is_empty() && e.salience == 0.0,
                },
            );
        }
    }

    // Inbound + outbound degree per entity id.
    let mut inbound: HashMap<String, u32> = HashMap::with_capacity(by_id.len());
    let mut outbound: HashMap<String, u32> = HashMap::with_capacity(by_id.len());
    for edge in &edges.edges {
        *outbound
            .entry(edge.source.as_str().to_string())
            .or_insert(0) += 1;
        *inbound.entry(edge.target.as_str().to_string()).or_insert(0) += 1;
    }

    // Load adaptive query-bump map (Phase B2) if persisted alongside
    // the atlas. Missing → empty (no adaptive prior yet). Counts are
    // additive within tier — they don't promote across tier
    // boundaries.
    let bumps = read_triage_bumps(&atlas_dir);

    // Each user-query match is worth this many "centrality points."
    // 10 is calibrated against typical Wikipedia centrality (median
    // ~10, p99 ~10K): one query is one centrality unit, ten queries
    // promote a moderate-centrality article above its peers, 100
    // queries pull a long-tail article past most of its tier.
    const BUMP_WEIGHT: u64 = 10;

    // Tier weight is large enough that centrality + bumps + seed-
    // hits never cross a tier boundary in seed scoring. Using
    // u32::MAX + 1 leaves room for ~2^31 expansion points, far
    // beyond realistic centrality + hits-from-seeds totals.
    const TIER_WEIGHT: u64 = (u32::MAX as u64) + 1;

    // Each "this article is referenced by N seeds" hit is worth this
    // many centrality units in the expansion ranker. 1000 means a
    // candidate referenced by 10 seeds outranks a same-tier candidate
    // with only +10K centrality — sensible: hits-from-seeds is a
    // direct quality signal for "this is connective tissue", whereas
    // raw centrality on the long tail is noisy.
    const SEED_HIT_WEIGHT: u64 = 1_000;

    // ── PHASE 1: score every in-corpus entity (full ranking) ────
    struct Ranked {
        id: String,
        canonical_name: String,
        tier: u8, // 1..=5 for vital, 6 for off-list
        centrality: u32,
        bumps: u64,
    }
    let mut ranked: Vec<Ranked> = by_id
        .iter()
        .filter(|(_, e)| !e.is_placeholder)
        .map(|(id, e)| {
            let centrality =
                inbound.get(id).copied().unwrap_or(0) + outbound.get(id).copied().unwrap_or(0);
            let tier = vital_tier(&e.canonical_name).unwrap_or(6);
            let bumps = bump_count_for(&bumps, &e.canonical_name);
            Ranked {
                id: id.clone(),
                canonical_name: e.canonical_name.clone(),
                tier,
                centrality,
                bumps,
            }
        })
        .collect();
    let seed_score = |r: &Ranked| -> u64 {
        (6u64 - r.tier as u64) * TIER_WEIGHT
            + r.centrality as u64
            + r.bumps.saturating_mul(BUMP_WEIGHT)
    };
    ranked.sort_by(|a, b| {
        seed_score(b)
            .cmp(&seed_score(a))
            .then_with(|| a.canonical_name.cmp(&b.canonical_name))
    });

    // ── PHASE 2: pick seeds (top-K of full ranking) ─────────────
    let seed_cap = ((budget as f64) * (1.0 - expansion_fraction)).round() as usize;
    // If expansion is disabled (fraction=0), seed_cap == budget and
    // we recover the legacy pure-centrality behaviour.
    let seed_cap = seed_cap.min(budget);
    let seeds: Vec<Ranked> = ranked.drain(..seed_cap.min(ranked.len())).collect();
    let seed_ids: std::collections::HashSet<String> = seeds.iter().map(|r| r.id.clone()).collect();

    // ── PHASE 3: 1-hop (or 2-hop) outbound expansion from seeds ─
    // hits_from_seeds[target_id] = how many seeds reference it.
    // For 2-hop, a second pass treats every 1-hop hit as a quasi-
    // seed but at half weight, so direct neighbours always outrank
    // grandchildren.
    let mut hits_from_seeds: HashMap<String, u32> = HashMap::new();
    if expansion_fraction > 0.0 && !seeds.is_empty() {
        for edge in &edges.edges {
            let src = edge.source.as_str();
            let tgt = edge.target.as_str();
            if seed_ids.contains(src) && !seed_ids.contains(tgt) {
                *hits_from_seeds.entry(tgt.to_string()).or_insert(0) += 2;
            }
        }
        if expansion_hops >= 2 {
            // 2-hop: walk one more level from current 1-hop set.
            // Half-weight (each 1-hop hit contributes 1, vs 2 for
            // direct seed hits) so direct neighbours dominate.
            let one_hop_ids: std::collections::HashSet<String> =
                hits_from_seeds.keys().cloned().collect();
            for edge in &edges.edges {
                let src = edge.source.as_str();
                let tgt = edge.target.as_str();
                if one_hop_ids.contains(src)
                    && !seed_ids.contains(tgt)
                    && !one_hop_ids.contains(tgt)
                {
                    *hits_from_seeds.entry(tgt.to_string()).or_insert(0) += 1;
                }
            }
        }
    }

    // Score expansion candidates. Drop placeholders (off-corpus
    // wikilink targets — can't enrich) and any seeds that snuck
    // through. Score formula keeps tier as a strong influence so a
    // vital article touched by even one seed beats a centrality-
    // heavy off-list article touched by many.
    struct Expansion {
        id: String,
        canonical_name: String,
        tier: u8,
        centrality: u32,
        bumps: u64,
        hits_from_seeds: u32,
    }
    let mut expansions: Vec<Expansion> = hits_from_seeds
        .into_iter()
        .filter_map(|(id, hits)| {
            let ent = by_id.get(&id)?;
            if ent.is_placeholder {
                return None;
            }
            let centrality =
                inbound.get(&id).copied().unwrap_or(0) + outbound.get(&id).copied().unwrap_or(0);
            let tier = vital_tier(&ent.canonical_name).unwrap_or(6);
            let bumps = bump_count_for(&bumps, &ent.canonical_name);
            Some(Expansion {
                id,
                canonical_name: ent.canonical_name.clone(),
                tier,
                centrality,
                bumps,
                hits_from_seeds: hits,
            })
        })
        .collect();
    let expansion_score = |e: &Expansion| -> u64 {
        // Tier weight stays large so a single vital-article hit
        // outranks any off-list saturating with seeds. Within a
        // tier, hits_from_seeds dominates centrality.
        (6u64 - e.tier as u64) * TIER_WEIGHT
            + (e.hits_from_seeds as u64).saturating_mul(SEED_HIT_WEIGHT)
            + e.centrality as u64
            + e.bumps.saturating_mul(BUMP_WEIGHT)
    };
    expansions.sort_by(|a, b| {
        expansion_score(b)
            .cmp(&expansion_score(a))
            .then_with(|| a.canonical_name.cmp(&b.canonical_name))
    });
    let expansion_cap = budget.saturating_sub(seeds.len());
    expansions.truncate(expansion_cap);

    // ── Tier histograms + diagnostics for the persisted output ──
    let mut tier_counts = [0usize; 6];
    let mut bumped_picks = 0usize;
    for r in &seeds {
        tier_counts[(r.tier as usize) - 1] += 1;
        if r.bumps > 0 {
            bumped_picks += 1;
        }
    }
    let mut expansion_tier_counts = [0usize; 6];
    for e in &expansions {
        tier_counts[(e.tier as usize) - 1] += 1;
        expansion_tier_counts[(e.tier as usize) - 1] += 1;
        if e.bumps > 0 {
            bumped_picks += 1;
        }
    }

    // ── Final pick list = seeds (in seed order) + expansions ────
    let picked: Vec<String> = seeds
        .iter()
        .map(|r| r.canonical_name.clone())
        .chain(expansions.iter().map(|e| e.canonical_name.clone()))
        .collect();
    let n = picked.len();

    let payload = serde_json::json!({
        "schema_version": 1,
        "corpus_id": corpus_id,
        "budget": budget,
        "top_in_corpus_by_centrality": picked,
        // Diagnostic: per-tier counts (combined seeds + expansion).
        // Consumed by `corpus status` and surfaced in tracing logs
        // from the post-install hook.
        "tier_breakdown": {
            "l1": tier_counts[0],
            "l2": tier_counts[1],
            "l3": tier_counts[2],
            "l4": tier_counts[3],
            "l5": tier_counts[4],
            "off_list": tier_counts[5],
        },
        // Seed/expansion split — operator wants to confirm the
        // expansion phase actually picked a meaningful pool, not
        // that the seed cap absorbed the whole budget.
        "seed_count": seeds.len(),
        "expansion_count": expansions.len(),
        "expansion_fraction": expansion_fraction,
        "expansion_hops": expansion_hops,
        "expansion_tier_breakdown": {
            "l1": expansion_tier_counts[0],
            "l2": expansion_tier_counts[1],
            "l3": expansion_tier_counts[2],
            "l4": expansion_tier_counts[3],
            "l5": expansion_tier_counts[4],
            "off_list": expansion_tier_counts[5],
        },
        // How many of the picks got an adaptive bump from the user's
        // own query history (Phase B2). When > 0 the triage queue
        // has incorporated lived usage; when 0 the rebuild was
        // pre-bump or no queries had landed yet.
        "bumped_picks": bumped_picks,
    });
    let out_path = indexes_dir.join(corpus_id).join("triage-candidates.json");
    if let Err(e) = write_atomic_json(&out_path, &payload) {
        return TriageOutcome::Failed {
            reason: format!("write triage-candidates.json: {e}"),
        };
    }
    TriageOutcome::Built {
        path: out_path,
        in_corpus_picked: n,
        elapsed_secs: started.elapsed().as_secs_f64(),
    }
}

/// Read the persisted query-bump map at `<atlas_dir>/triage_bumps.json`.
/// Returns an empty map if the file is missing, malformed, or has a
/// future schema we don't recognise — adaptive priors are best-effort
/// telemetry, not a load-bearing contract.
fn read_triage_bumps(atlas_dir: &Path) -> HashMap<String, u64> {
    use corpus_engine::filters::normalize_title;
    let path = atlas_dir.join("triage_bumps.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    #[derive(serde::Deserialize)]
    struct File {
        schema_version: u32,
        bumps: HashMap<String, u64>,
    }
    let parsed: File = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(_) => return HashMap::new(),
    };
    if parsed.schema_version != 1 {
        return HashMap::new();
    }
    // Pre-normalise keys so the lookup at scoring time is a direct
    // hash hit. The atlas Entity canonical_name and the runtime-
    // recorded bump key both pass through `normalize_title`.
    parsed
        .bumps
        .into_iter()
        .map(|(k, v)| (normalize_title(&k), v))
        .collect()
}

fn bump_count_for(bumps: &HashMap<String, u64>, canonical_name: &str) -> u64 {
    if bumps.is_empty() {
        return 0;
    }
    let key = corpus_engine::filters::normalize_title(canonical_name);
    bumps.get(&key).copied().unwrap_or(0)
}

/// Suffix appended to the source corpus id to derive the Tier-2
/// workspace name. `wikipedia` → `wikipedia-tier2`. Stable so the
/// daemon's resume scan can find unfinished work on boot.
pub const TIER2_WORKSPACE_SUFFIX: &str = "-tier2";

/// Sentinel file dropped into a workspace dir the moment the
/// post-install hook creates it. The daemon's resume scan only
/// re-spawns workspaces with this marker — manual workspaces
/// (created by `sovereign enrich init`) are left to the operator.
pub const AUTO_MANAGED_MARKER: &str = ".auto_managed";

/// Outcome of a Tier-2 extraction launch.
#[derive(Debug)]
pub enum Tier2LaunchOutcome {
    /// `enrich init` ran successfully and `enrich extract --full
    /// --resume` was spawned in the background. The PID lets the
    /// caller surface "extracting" state to status endpoints.
    Spawned {
        workspace_id: String,
        log_path: PathBuf,
        pid: u32,
    },
    /// Already complete — every chapter in the workspace's checkpoint
    /// is success or skipped. No new process spawned. The
    /// `chapters_total` / `chapters_done` fields lets callers report
    /// final state.
    AlreadyComplete {
        workspace_id: String,
        chapters_done: usize,
        chapters_total: usize,
    },
    /// Phase C3 — local extraction was deliberately skipped because
    /// a mesh peer already has a deeper atlas. The operator (or a
    /// future automated pull) is expected to fetch the peer's
    /// canonical via the mesh sync surface; until that happens the
    /// local atlas stays at whatever depth the structural pass
    /// produced, and atlas grounding still works on those entries.
    DeferredToPeer {
        peer_name: String,
        peer_tier2_count: u64,
        local_tier2_count: u64,
    },
    /// Init failed (e.g. corpus not installed, triage missing).
    InitFailed { reason: String },
    /// `enrich init` ran but spawning `extract` failed.
    SpawnFailed { reason: String },
}

/// Launch — but don't await — the Tier-2 extraction pipeline against
/// `source_corpus_id`. Idempotent on workspace existence: if the
/// workspace's `chapters.json` is already present and the checkpoint
/// covers every chapter, returns `AlreadyComplete`. If the workspace
/// is partially done, the spawned `extract --resume` picks up
/// where it left off.
///
/// `peer_advice` (Phase C3) is `Some` when a mesh peer already has a
/// deeper atlas; in that case we short-circuit with `DeferredToPeer`
/// rather than burning local tokens + days of wall-clock to reproduce
/// the peer's work. Pulling the peer's atlas is a separate operator
/// step today (`sovereign mesh canonical-pull`); auto-pull is a
/// follow-up.
///
/// The subprocess inherits no stdin and writes stdout + stderr to
/// `<workspace>/extraction.log` so daemon logs stay clean. The
/// process is detached from the daemon's reaper — if the daemon
/// dies, the extract child becomes a zombie until reaped on next
/// daemon boot via the resume scan.
pub async fn launch_tier2_extraction(
    source_corpus_id: &str,
    triage_path: PathBuf,
    cli_binary: PathBuf,
    enrichment_dir: PathBuf,
    indexes_dir: PathBuf,
) -> Tier2LaunchOutcome {
    launch_tier2_extraction_with_advice(
        source_corpus_id,
        triage_path,
        cli_binary,
        enrichment_dir,
        indexes_dir,
        None,
    )
    .await
}

/// Variant that takes an optional [`PeerAtlasPullCandidate`]. Used
/// by the post-install hook (which has access to mesh state) to
/// short-circuit local extraction when a peer leads on Tier-2 by
/// at least [`crate::atlas_peer_advice::MIN_PEER_LEAD`].
pub async fn launch_tier2_extraction_with_advice(
    source_corpus_id: &str,
    triage_path: PathBuf,
    cli_binary: PathBuf,
    enrichment_dir: PathBuf,
    indexes_dir: PathBuf,
    peer_advice: Option<crate::atlas_peer_advice::PeerAtlasPullCandidate>,
) -> Tier2LaunchOutcome {
    if let Some(advice) = peer_advice {
        return Tier2LaunchOutcome::DeferredToPeer {
            peer_name: advice.peer_name,
            peer_tier2_count: advice.peer_tier2_count,
            local_tier2_count: advice.local_tier2_count,
        };
    }
    let workspace_id = format!("{source_corpus_id}{TIER2_WORKSPACE_SUFFIX}");
    let workspace_dir = enrichment_dir.join(&workspace_id);
    let chapters_manifest_path = indexes_dir.join(&workspace_id).join("chapters.json");

    // Already complete?
    if let Some((done, total)) = checkpoint_progress(&workspace_dir, &chapters_manifest_path) {
        if done == total && total > 0 {
            return Tier2LaunchOutcome::AlreadyComplete {
                workspace_id,
                chapters_done: done,
                chapters_total: total,
            };
        }
    }

    // Init the workspace if config.json is missing. Skip otherwise
    // — the extract --resume below picks up wherever the existing
    // checkpoint left off.
    let config_exists = workspace_dir.join("config.json").exists();
    if !config_exists {
        let init_status = tokio::process::Command::new(&cli_binary)
            .args([
                "enrich",
                "init",
                &workspace_id,
                "--from-corpus",
                source_corpus_id,
                "--include-articles",
                triage_path.to_str().unwrap_or("/dev/null"),
                "--pipeline",
                "referential_atlas",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await;
        match init_status {
            Ok(out) if out.status.success() => {
                // Drop the auto-managed sentinel so the daemon's
                // boot-time resume scan picks this workspace up but
                // skips manual workspaces operators created with
                // `sovereign enrich init` directly.
                let _ = std::fs::write(workspace_dir.join(AUTO_MANAGED_MARKER), "");
            }
            Ok(out) => {
                return Tier2LaunchOutcome::InitFailed {
                    reason: format!(
                        "enrich init exit {}: {}",
                        out.status,
                        String::from_utf8_lossy(&out.stderr)
                            .lines()
                            .last()
                            .unwrap_or("(no stderr)")
                    ),
                }
            }
            Err(e) => {
                return Tier2LaunchOutcome::InitFailed {
                    reason: format!("enrich init spawn failed: {e}"),
                }
            }
        }
    }

    // Spawn `extract --full --resume` and detach. Logs go to
    // <workspace>/extraction.log so they don't pollute daemon
    // stdout / stderr.
    let log_path = workspace_dir.join("extraction.log");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            return Tier2LaunchOutcome::SpawnFailed {
                reason: format!("open extraction.log: {e}"),
            }
        }
    };
    let stderr_file = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return Tier2LaunchOutcome::SpawnFailed {
                reason: format!("clone log handle: {e}"),
            }
        }
    };
    let spawn = std::process::Command::new(&cli_binary)
        .args(["enrich", "extract", &workspace_id, "--full", "--resume"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file))
        .env("RUST_LOG", "info")
        .spawn();
    match spawn {
        Ok(child) => Tier2LaunchOutcome::Spawned {
            workspace_id,
            log_path,
            pid: child.id(),
        },
        Err(e) => Tier2LaunchOutcome::SpawnFailed {
            reason: format!("enrich extract spawn: {e}"),
        },
    }
}

/// Read `<workspace>/runs/_phase1_checkpoint.jsonl` and return
/// `(distinct_chapters_processed, total_chapters)`. The chapters
/// manifest lives at `<indexes_dir>/<workspace_id>/chapters.json`
/// (per `enrich_cmd::paths::chapters_manifest_path`), so the caller
/// passes both paths. `None` when the chapters manifest is missing
/// or unparseable.
pub fn checkpoint_progress(
    workspace_dir: &Path,
    chapters_manifest_path: &Path,
) -> Option<(usize, usize)> {
    let chapters: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(chapters_manifest_path).ok()?).ok()?;
    let total = chapters.get("chapters")?.as_array()?.len();

    let ckpt_path = workspace_dir.join("runs").join("_phase1_checkpoint.jsonl");
    let ckpt_text = std::fs::read_to_string(&ckpt_path).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for line in ckpt_text.lines() {
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(id) = rec.get("chapter_id").and_then(|v| v.as_str()) {
                seen.insert(id.to_string());
            }
        }
    }
    Some((seen.len(), total))
}

/// On daemon boot, find every Tier-2 workspace under
/// `enrichment_dir` whose checkpoint is incomplete and re-spawn
/// `enrich extract --full --resume` for each. Idempotent — already-
/// complete workspaces are left alone, and spawning twice is safe
/// because `extract --resume` skips chapters already in the
/// checkpoint.
pub async fn resume_inflight_tier2(
    enrichment_dir: PathBuf,
    indexes_dir: PathBuf,
    cli_binary: PathBuf,
) -> Vec<Tier2LaunchOutcome> {
    let mut outcomes = Vec::new();
    let entries = match std::fs::read_dir(&enrichment_dir) {
        Ok(rd) => rd,
        Err(_) => return outcomes,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(TIER2_WORKSPACE_SUFFIX) {
            continue;
        }
        // Auto-managed sentinel — we only resume workspaces that
        // the post-install hook created. A manual workspace
        // (`sovereign enrich init <name>-tier2`) won't have this
        // marker and is left to the operator.
        if !path.join(AUTO_MANAGED_MARKER).exists() {
            continue;
        }
        let source_corpus_id = name.trim_end_matches(TIER2_WORKSPACE_SUFFIX);
        let chapters_manifest_path = indexes_dir.join(name).join("chapters.json");
        let Some((done, total)) = checkpoint_progress(&path, &chapters_manifest_path) else {
            continue;
        };
        if total == 0 || done >= total {
            continue;
        }
        // The triage file is keyed off the source corpus, not the
        // workspace. init won't actually run (config.json exists),
        // but pass a sensible path so debug logs aren't misleading.
        let triage = indexes_dir
            .join(source_corpus_id)
            .join("triage-candidates.json");
        outcomes.push(
            launch_tier2_extraction(
                source_corpus_id,
                triage,
                cli_binary.clone(),
                enrichment_dir.clone(),
                indexes_dir.clone(),
            )
            .await,
        );
    }
    outcomes
}

fn write_atomic_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("atlas-postinstall")
    ));
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::{
        atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity},
        edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile},
    };
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    /// Build a synthetic structural atlas under `<dir>/<corpus>/atlas/`
    /// containing one L1 entity (low centrality) and a flotilla of
    /// off-list entities with very high centrality. The L1 should
    /// outrank the noise once the tier prior is applied.
    fn write_synthetic_atlas(dir: &Path, corpus_id: &str) -> std::io::Result<()> {
        let atlas_dir = dir.join(corpus_id).join("atlas");
        std::fs::create_dir_all(&atlas_dir)?;

        // L1: "Earth" with salience 1.0 (so not classified as
        // placeholder) and modest centrality.
        let earth = Entity {
            id: AtomId::entity(1),
            canonical_name: "Earth".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "L1 vital article".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };
        // Off-list noise: 5 entities with massive centrality (each
        // sourcing 100 edges into earth or each other).
        let mut atoms: Vec<AtomEnvelope> = vec![AtomEnvelope::Entity(earth)];
        for i in 2..=6 {
            atoms.push(AtomEnvelope::Entity(Entity {
                id: AtomId::entity(i),
                canonical_name: format!("Random Page {i}"),
                aliases: Vec::new(),
                entity_type: EntityType::Concept,
                first_appearance: ChunkRef::new("sec_0002", None),
                description: "off-list noise".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Structural,
                affiliation: None,
                role: None,
                participants: Vec::new(),
                defining_quote: None,
                provenance: Default::default(),
                attributes: serde_json::Map::new(),
                concept_kind: None,
            }));
        }

        // Edges: Earth gets 5 inbound. Random Pages each get 100
        // outbound (to each other in a ring). Without the tier prior
        // a Random Page would land at top.
        let mut edges = Vec::new();
        let mut next_edge = 0u64;
        for i in 2..=6 {
            edges.push(Edge {
                id: EdgeId::new(next_edge as usize),
                edge_type: EdgeType::Involves,
                source: AtomId::entity(i),
                target: AtomId::entity(1),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            });
            next_edge += 1;
        }
        for i in 2..=6 {
            for _ in 0..100 {
                let target = (i % 5) + 2;
                edges.push(Edge {
                    id: EdgeId::new(next_edge as usize),
                    edge_type: EdgeType::Involves,
                    source: AtomId::entity(i),
                    target: AtomId::entity(target),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::WikilinkStructural,
                });
                next_edge += 1;
            }
        }

        let atoms_file = AtomsFile::new(atoms);
        let edges_file = EdgesFile::new(edges);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&atoms_file).unwrap(),
        )?;
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&edges_file).unwrap(),
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn vital_tier_prior_promotes_l1_above_high_centrality_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = "synthetic-vital";
        write_synthetic_atlas(tmp.path(), corpus).unwrap();
        // Disable expansion so this test exercises the seed-only
        // behaviour. Seed-expansion gets its own dedicated test
        // below; keeping this one focused on the tier prior.
        write_triage_config(
            &tmp.path().join(corpus).join("atlas"),
            None,
            Some(0.0),
            None,
        )
        .unwrap();

        let outcome = build_triage_candidates(corpus, tmp.path().to_path_buf(), 3).await;
        let path = match outcome {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("triage failed: {other:?}"),
        };

        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let picks = v["top_in_corpus_by_centrality"].as_array().unwrap();
        // Earth (L1) must be #1 even though Random Page entities have
        // 100× the centrality.
        assert_eq!(picks[0].as_str(), Some("Earth"));
        // Tier breakdown sanity: l1 == 1, off_list == 2 (budget 3,
        // one L1 + two off-list noise pages).
        assert_eq!(v["tier_breakdown"]["l1"].as_u64(), Some(1));
        assert_eq!(v["tier_breakdown"]["off_list"].as_u64(), Some(2));
        // No bumps file → bumped_picks = 0.
        assert_eq!(v["bumped_picks"].as_u64(), Some(0));
        // Expansion disabled.
        assert_eq!(v["seed_count"].as_u64(), Some(3));
        assert_eq!(v["expansion_count"].as_u64(), Some(0));
    }

    /// Seed-expansion: a small seed (one L1 vital article) plus a
    /// long-tail web of articles it links to should produce a triage
    /// list that's seeds + the highest-hit wikilink targets, even
    /// when those targets aren't themselves vital and have low
    /// centrality.
    #[tokio::test]
    async fn seed_expansion_picks_articles_linked_from_seeds() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = "synthetic-expansion";
        let atlas_dir = tmp.path().join(corpus).join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();

        // 1 L1 vital seed (Earth) + 4 off-list "neighbour" articles
        // that Earth links to. None of the neighbours have any
        // other inbound edges, so pure-centrality triage would never
        // rank them above random off-list candidates. Plus 5 random
        // off-list "noise" pages with high mutual centrality.
        let mut atoms: Vec<AtomEnvelope> = Vec::new();
        let mk = |i: usize, name: &str| {
            AtomEnvelope::Entity(Entity {
                id: AtomId::entity(i),
                canonical_name: name.into(),
                aliases: Vec::new(),
                entity_type: EntityType::Concept,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "x".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Structural,
                affiliation: None,
                role: None,
                participants: Vec::new(),
                defining_quote: None,
                provenance: Default::default(),
                attributes: serde_json::Map::new(),
                concept_kind: None,
            })
        };
        atoms.push(mk(1, "Earth")); // L1 seed
        for (i, name) in ["Neighbour A", "Neighbour B", "Neighbour C", "Neighbour D"]
            .iter()
            .enumerate()
        {
            atoms.push(mk(2 + i, name));
        }
        for (i, name) in ["Noise 1", "Noise 2", "Noise 3", "Noise 4", "Noise 5"]
            .iter()
            .enumerate()
        {
            atoms.push(mk(6 + i, name));
        }

        let mk_edge = |idx: usize, src: usize, tgt: usize| Edge {
            id: EdgeId::new(idx),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(src),
            target: AtomId::entity(tgt),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::WikilinkStructural,
        };
        let mut edges = Vec::new();
        let mut next = 0;
        // Earth → each neighbour (4 edges).
        for tgt in 2..=5 {
            edges.push(mk_edge(next, 1, tgt));
            next += 1;
        }
        // Noise pages link aggressively to each other (50 edges
        // each → high centrality even though Earth doesn't touch
        // them).
        for src in 6..=10 {
            for _ in 0..50 {
                let tgt = 6 + ((src - 5) % 5); // cycle within noise
                edges.push(mk_edge(next, src, tgt));
                next += 1;
            }
        }
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&AtomsFile::new(atoms)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&EdgesFile::new(edges)).unwrap(),
        )
        .unwrap();

        // Budget 2 with default 30% expansion → seed_cap=1
        // (round 1.4), expansion_cap=1. Earth (only vital) takes
        // the seed slot. Expansion must come from Earth's outbound
        // links — Neighbour A wins on alphabetical tie-break (each
        // neighbour has the same hits-from-seeds = 1).
        // We use budget=2 (not 5) deliberately: a larger budget
        // would let high-centrality noise pages claim seed slots,
        // which would in turn promote OTHER noise pages via the
        // expansion ranker. That edge case is real (mismatched
        // tier supply vs. seed cap) but tested separately below.
        let outcome = build_triage_candidates(corpus, tmp.path().to_path_buf(), 2).await;
        let path = match outcome {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("triage failed: {other:?}"),
        };
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let picks: Vec<&str> = v["top_in_corpus_by_centrality"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(picks[0], "Earth", "Earth should lead the seeds");
        assert_eq!(picks.len(), 2);
        let expansion_pick = picks[1];
        assert!(
            expansion_pick.starts_with("Neighbour"),
            "expected an expansion pick from Earth's outbound links, \
             got '{expansion_pick}' instead — noise pages should not \
             win expansion when seeds are vital-only"
        );
        assert_eq!(v["seed_count"].as_u64(), Some(1));
        assert_eq!(v["expansion_count"].as_u64(), Some(1));
        assert_eq!(v["expansion_tier_breakdown"]["off_list"].as_u64(), Some(1));
    }

    #[test]
    fn budget_override_roundtrips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("corpus").join("atlas");

        // No file → fall back to default.
        assert_eq!(
            effective_tier2_budget(tmp.path(), "corpus"),
            DEFAULT_TIER2_BUDGET
        );
        assert!(read_triage_budget(&atlas_dir).is_none());

        // Write override → the resolver picks it up.
        write_triage_budget(&atlas_dir, 5_000).unwrap();
        assert_eq!(read_triage_budget(&atlas_dir), Some(5_000));
        assert_eq!(effective_tier2_budget(tmp.path(), "corpus"), 5_000);

        // Nuking the file reverts to default.
        std::fs::remove_file(atlas_dir.join(TRIAGE_CONFIG_FILE)).unwrap();
        assert_eq!(
            effective_tier2_budget(tmp.path(), "corpus"),
            DEFAULT_TIER2_BUDGET
        );
    }

    /// Phase B2 end-to-end: a `triage_bumps.json` next to the atlas
    /// should reorder same-tier entries so heavily-bumped names rank
    /// above same-centrality unbumped ones.
    #[tokio::test]
    async fn query_bumps_reorder_same_tier_picks() {
        use std::collections::HashMap;
        let tmp = tempfile::tempdir().unwrap();
        let corpus = "synthetic-bumps";
        let atlas_dir = tmp.path().join(corpus).join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        // Disable expansion so this test stays focused on the bump
        // reordering rule. Expansion has its own test above.
        write_triage_config(&atlas_dir, None, Some(0.0), None).unwrap();

        // Two off-list entities with equal centrality. Without the
        // bump file, they tie-break alphabetically (Alpha first).
        // With Beta bumped 50 times, Beta should win.
        let mut atoms: Vec<AtomEnvelope> = Vec::new();
        for (i, name) in ["Alpha thing", "Beta thing"].iter().enumerate() {
            atoms.push(AtomEnvelope::Entity(Entity {
                id: AtomId::entity(i + 1),
                canonical_name: (*name).into(),
                aliases: Vec::new(),
                entity_type: EntityType::Concept,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "off-list".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Structural,
                affiliation: None,
                role: None,
                participants: Vec::new(),
                defining_quote: None,
                provenance: Default::default(),
                attributes: serde_json::Map::new(),
                concept_kind: None,
            }));
        }
        // Equal centrality: each gets one inbound from the other.
        let edges = vec![
            Edge {
                id: EdgeId::new(0),
                edge_type: EdgeType::Involves,
                source: AtomId::entity(1),
                target: AtomId::entity(2),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            },
            Edge {
                id: EdgeId::new(1),
                edge_type: EdgeType::Involves,
                source: AtomId::entity(2),
                target: AtomId::entity(1),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            },
        ];
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&AtomsFile::new(atoms)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&EdgesFile::new(edges)).unwrap(),
        )
        .unwrap();

        // Sanity: pre-bump rank places Alpha first (alphabetical
        // tie-break on equal score).
        let outcome = build_triage_candidates(corpus, tmp.path().to_path_buf(), 2).await;
        let path = match outcome {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("pre-bump triage failed: {other:?}"),
        };
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let picks = v["top_in_corpus_by_centrality"].as_array().unwrap();
        assert_eq!(picks[0].as_str(), Some("Alpha thing"));
        assert_eq!(v["bumped_picks"].as_u64(), Some(0));

        // Drop a bumps file giving Beta 50 hits.
        let mut bumps: HashMap<String, u64> = HashMap::new();
        bumps.insert("Beta thing".into(), 50);
        let bumps_payload = serde_json::json!({
            "schema_version": 1,
            "bumps": bumps,
        });
        std::fs::write(
            atlas_dir.join("triage_bumps.json"),
            serde_json::to_vec_pretty(&bumps_payload).unwrap(),
        )
        .unwrap();

        // Re-rank: Beta should now win, and bumped_picks should
        // reflect one bumped entry in the kept set.
        let outcome2 = build_triage_candidates(corpus, tmp.path().to_path_buf(), 2).await;
        let path2 = match outcome2 {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("post-bump triage failed: {other:?}"),
        };
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path2).unwrap()).unwrap();
        let picks2 = v2["top_in_corpus_by_centrality"].as_array().unwrap();
        assert_eq!(picks2[0].as_str(), Some("Beta thing"));
        assert_eq!(v2["bumped_picks"].as_u64(), Some(1));
    }
}
