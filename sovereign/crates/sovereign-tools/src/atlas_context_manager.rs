// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AtlasContextManager` — daemon-side bag of pre-embedded atlas
//! Entity contexts, one per installed corpus that has an `atlas/`
//! dir.
//!
//! At daemon boot, [`AtlasContextManager::spawn_init`] walks every
//! installed corpus, loads its atoms (skipping placeholders +
//! short-description entries), and embeds the survivors via the
//! inference provider. The result is cached on disk
//! (`atlas/atoms.embeddings.bin` — see
//! `corpus-engine::enrichment::atlas::embeddings`) so subsequent
//! daemon starts skip the embed pass entirely on cache hit.
//!
//! Once loaded, the manager implements
//! [`sovereign_core::atlas_context::AtlasContextProvider`]: the
//! `Runtime` consults it inside `prepare_knowledge_query_plan` to
//! fuse atlas Entity matches into the chunk hit set as virtual
//! `ScoredChunk`s. Pre-loading makes the per-question cost a few
//! microseconds (50 cosines) rather than a multi-second embed cycle.
//!
//! ## Filter defaults
//!
//! The manager ships with `min_description_chars = 200` —
//! structural one-liners ("X is a Y born in Z.") run shorter than
//! that and would dilute retrieval; extracted / Tier-2 augmented
//! entities run hundreds-to-thousands of chars. Operators tuning
//! the filter can override via `AtlasContextManager::with_filter`.

use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use sovereign_core::atlas_context::{AtlasContext, AtlasContextProvider};
use sovereign_core::traits::InferenceProvider;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// The ONE `atoms.json` → embedded-bag loader (ontology-v1 P0.2). Lives in
/// `atlas_context_loader.rs` to keep this file under the §3.1 ceiling;
/// re-exported so `atlas_context_manager::load_atlas_context` is the name
/// every caller — CLI wrapper, daemon hook, eval harness — uses.
pub use crate::atlas_context_loader::{
    backfill_ann, load_atlas_context, BackfillOutcome, LoadAtlasError,
};

/// Filename of the per-corpus query-bump map. Lives alongside
/// `atoms.json` so it travels with the atlas (mesh transfer brings
/// it along) and the operator can inspect it without poking inside
/// the daemon.
pub const TRIAGE_BUMPS_FILE: &str = "triage_bumps.json";

/// Filter applied during atlas-context loading. Mirrors the shape
/// of the eval CLI's `AtlasLoadFilter` so the cache key derived
/// here is comparable to what the CLI writes / reads.
#[derive(Debug, Clone)]
pub struct AtlasContextFilter {
    pub min_description_chars: usize,
    pub depth_allowlist: Vec<String>,
    pub max_entries: Option<usize>,
    pub top_k: usize,
    /// Path 2 (Phase A) — when true, the loader also emits virtual
    /// entries for `Claim` atoms in addition to `Entity` atoms. Each
    /// claim becomes one `AtlasEntry` whose `canonical_name` is the
    /// article slug (so retrieval-time `score_sources` matching by
    /// title still credits the source) and whose `embed_text`
    /// encodes the discourse_act + epistemic_status + content as
    /// `[Claim: <act>] <content>`. Default `false` for backwards
    /// compatibility with the entity-only cache. Cache key
    /// invalidates automatically via `signature()`.
    pub include_claims: bool,
    /// Path 2 (Phase B) — when true, the loader also emits virtual
    /// entries for `Tension` edges in `edges.json`. Each tension fuses
    /// its `sub_question` with both endpoint atoms into one embed text;
    /// `canonical_name` is the article slug. Default `false`. Cache
    /// key invalidates automatically via `signature()`. This is the
    /// only Path 2 surface that can move the `dialectical_breadth`
    /// essay axis — the substance lives on the edge, not on either
    /// endpoint atom by itself.
    pub include_tensions: bool,
    /// Path 2 (Phase C) — when true, the loader also emits virtual
    /// entries for `Configuration` atoms (spec §2.7). Each
    /// configuration becomes one `AtlasEntry` with `canonical_name`
    /// set to the article slug and embed text
    /// `[Configuration: <label>] <description>`. Default `false`.
    /// Should lift `argument_depth` on essay-readiness — Configurations
    /// articulate the interpretive shape the article enacts as a whole.
    pub include_configurations: bool,
    /// DARK (ontology-v1 P5, default **OFF**) —
    /// `SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS`. Narrower than
    /// [`Self::include_claims`]: it admits only Claim atoms whose `claim_kind`
    /// names a type the corpus DECLARED, so an undeclared corpus admits
    /// nothing new however it is set. The declared claim is where a
    /// numismatics corpus keeps "who dated this coin to when, on what
    /// evidence" — content the entity bag cannot carry.
    ///
    /// Baked into [`Self::signature`], so a cache built with it off is
    /// correctly ignored when it flips on.
    pub include_declared_claim_types: bool,
}

impl Default for AtlasContextFilter {
    fn default() -> Self {
        // Defaults are tuned for Wikipedia/SEP-scale corpora where
        // Tier-2 extracted entities carry multi-sentence descriptions.
        // Small-corpus atom schemas (the `conversational` domain
        // produces ~0-150 char descriptions; arch-principles structural
        // atoms similarly short) would be filtered to zero here. Three
        // env knobs let the operator relax the filter at boot without
        // rebuilding:
        //   - SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS=<N> overrides the
        //     200-char floor. `0` admits every atom.
        //   - SOVEREIGN_ATLAS_INCLUDE_DEPTHS=<csv> overrides the
        //     `extracted`-only depth filter. `*` admits every depth.
        //   - SOVEREIGN_ATLAS_INCLUDE_CLAIMS=1|true surfaces Claim
        //     atoms as virtual chunks (default off).
        // The cache signature (`signature()`) bakes all three, so a cache
        // populated under one filter is correctly ignored under
        // another — no risk of cross-contaminating loaded atoms.
        // Floor on an atom's FULL embed signal (name + aliases + description),
        // not description alone — names are first-class grounding signal, so a
        // 10-char floor admits every real atom and drops only empty fragments.
        // (Was 200, which silently nuked name-rich/short-description atoms —
        // ~85% of SEP — and "filtered to zero" small-corpus schemas.)
        let min_chars = std::env::var("SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        let depth_allowlist = match std::env::var("SOVEREIGN_ATLAS_INCLUDE_DEPTHS") {
            Ok(v) if v.trim() == "*" => Vec::new(),
            Ok(v) => v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Err(_) => vec!["extracted".to_string()],
        };
        // Claim atoms as virtual chunks (Path 2 Phase A). Off by default:
        // wiki/SEP-scale atlases lean on Entity grounding, and claims
        // multiply the embed count. For a small narrative atlas (a single
        // novel) the Claims ARE the substance — the entity descriptions are
        // short and the discriminating content lives in the Claim atoms — so
        // a literary grounding run sets this on. Baked into `signature()`,
        // so a cache built with claims off is ignored when it flips on.
        let include_claims = std::env::var("SOVEREIGN_ATLAS_INCLUDE_CLAIMS")
            .ok()
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        // DARK: declared-type claims as virtual chunks (ontology-v1 P5).
        // Off by default; see the `DEFAULTS_LEDGER.md` row for the flip
        // conditions.
        let include_declared_claim_types = std::env::var("SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS")
            .ok()
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        Self {
            min_description_chars: min_chars,
            depth_allowlist,
            max_entries: None,
            top_k: 3,
            include_claims,
            include_tensions: false,
            include_configurations: false,
            include_declared_claim_types,
        }
    }
}

impl AtlasContextFilter {
    /// Stable signature used as the embeddings cache key. Must agree
    /// with `sovereign-cli::eval_cmd::runner::filter_signature` so a
    /// cache populated by either side is recognised by the other.
    pub fn signature(&self) -> String {
        let mut depths = self.depth_allowlist.clone();
        depths.sort();
        format!(
            "min_chars={};depth=[{}];max={};claims={};tensions={};configs={};declared_claims={}",
            self.min_description_chars,
            depths.join(","),
            self.max_entries
                .map(|n| n.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.include_claims,
            self.include_tensions,
            self.include_configurations,
            self.include_declared_claim_types,
        )
    }
}

/// Daemon-side lifecycle for atlas-grounded retrieval.
pub struct AtlasContextManager {
    indexes_dir: PathBuf,
    inference: Arc<dyn InferenceProvider>,
    embed_model: String,
    filter: AtlasContextFilter,
    contexts: Arc<RwLock<HashMap<String, Arc<AtlasContext>>>>,
    /// Structural graph layer per atlas (atom-by-id + edge adjacency).
    /// Used by [`sovereign_core::atlas_context::atlas_navigate_ann`] for graph
    /// BFS; without it the runtime falls back to bag-of-atoms cosine
    /// (`atlas_top_k_as_chunks`). Populated two ways: eagerly at init
    /// for corpora whose embedding context loaded (those are the only
    /// ids `atlas_navigate` can ever seed), and on-demand in
    /// [`AtlasContextProvider::graph`] for everything else (the atom-
    /// enumeration path pulls graphs for enabled corpora regardless of
    /// context state). Wikipedia-scale atlases run to GBs parsed —
    /// eager-loading all of them cost ~20G RSS for graphs no query
    /// could reach (observed 2026-06-10: contexts=0, graphs=1790).
    graphs: Arc<RwLock<HashMap<String, Arc<sovereign_core::atlas_context::AtlasGraph>>>>,
    /// Discovery map `corpus_id → atlas dir`, filled by the init scan
    /// without parsing anything. [`AtlasContextProvider::graph`] uses
    /// it to lazy-load on first request; a failed parse evicts the
    /// entry so corrupt atlases aren't re-parsed every turn. Sync
    /// `RwLock` — read from the sync provider trait on the hot path.
    graph_dirs: Arc<std::sync::RwLock<HashMap<String, PathBuf>>>,
    /// Per-corpus query-bump map, in-memory mirror of each atlas's
    /// `triage_bumps.json`. Loaded at init time, mutated on every
    /// `record_match`, persisted by [`flush_bumps`] (debounced via
    /// `bumps_dirty`). Sync `Mutex` rather than tokio `RwLock` —
    /// `record_match` is called from a sync trait method on the hot
    /// retrieval path, and the critical section is a single hashmap
    /// increment.
    bumps: Arc<Mutex<HashMap<String, BumpState>>>,
}

/// Per-corpus bump book-keeping. Counts are u64 because human-rate
/// queries over many years still fit comfortably; saturating add
/// guards against the pathological case anyway.
#[derive(Debug, Default, Clone)]
struct BumpState {
    counts: HashMap<String, u64>,
    /// Set true on every `record_match`; cleared by the flusher
    /// after a successful disk write.
    dirty: bool,
}

impl AtlasContextManager {
    pub fn new(
        indexes_dir: PathBuf,
        inference: Arc<dyn InferenceProvider>,
        embed_model: String,
    ) -> Self {
        Self {
            indexes_dir,
            inference,
            embed_model,
            filter: AtlasContextFilter::default(),
            contexts: Arc::new(RwLock::new(HashMap::new())),
            graphs: Arc::new(RwLock::new(HashMap::new())),
            graph_dirs: Arc::new(std::sync::RwLock::new(HashMap::new())),
            bumps: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Override the load filter (test surface; production uses defaults).
    pub fn with_filter(mut self, filter: AtlasContextFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Cache-only init. Walks installed corpora, loads any atlas
    /// whose embeddings are already cached on disk, and skips
    /// anything that would require a fresh embed pass. Use this on
    /// the per-process build path (CLI bootstrap, desktop launch)
    /// so first-query latency stays predictable — cold-start embed
    /// work belongs in the post-install hook / background scheduler
    /// (Track A4 / A5), not in front of the user's first message.
    pub async fn init_from_cache(&self) {
        self.init_internal(true).await
    }

    /// Walk every installed corpus, load + embed atlas Entity
    /// records (skipping placeholders + entries below the
    /// description-length threshold), persist embeddings to the
    /// cache, and stash the resulting `AtlasContext` in the manager.
    /// Idempotent: running twice with no atoms.json change is a
    /// pure cache replay.
    pub async fn init(&self) {
        self.init_internal(false).await
    }

    async fn init_internal(&self, _cache_only: bool) {
        // Walk the indexes dir directly. Atlases live at
        // `<indexes_dir>/<dir>/atlas/atoms.json` — `<dir>` may be a
        // proper installed corpus (with `_corpus_meta.json`) or an
        // atlas-only sibling produced by `enrich ingest
        // --strategy structure_first` against a different source
        // corpus. Both shapes contribute Entity grounding, and the
        // runtime fuses every loaded atlas into every query
        // regardless of which chunk corpus the question came from.
        let candidates = match std::fs::read_dir(&self.indexes_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    if !p.is_dir() {
                        return None;
                    }
                    let name = p.file_name()?.to_str()?.to_string();
                    if name.starts_with('.') || name.starts_with('_') {
                        return None;
                    }
                    let atlas_dir = p.join(ATLAS_DIRNAME);
                    if !atlas_dir.join("atoms.json").exists() {
                        return None;
                    }
                    Some((name, atlas_dir))
                })
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(
                    indexes_dir = %self.indexes_dir.display(),
                    error = %e,
                    "atlas-context: indexes dir read failed"
                );
                return;
            }
        };
        tracing::info!(
            candidate_count = candidates.len(),
            "atlas-context: scanning indexes dir for atlases"
        );
        for (corpus_id, atlas_dir) in candidates {
            // Hydrate any persisted bump map first — done before
            // the load attempt so the bump record survives even if
            // the embed cache is missing and load_one rejects.
            if let Some(state) = read_bump_state(&atlas_dir) {
                let mut guard = self.bumps.lock().expect("bumps mutex");
                guard
                    .entry(corpus_id.clone())
                    .or_default()
                    .counts
                    .extend(state.counts);
            }
            // Lazy boot: register the dir (so `graph()` + `ensure_loaded` can
            // find it) but do NOT build the bag here. Scoped grounding warms
            // only the query-relevant atlases on demand via `ensure_loaded`,
            // so boot no longer pays the O(N-corpora) eager-load cost (~15s at
            // SEP's 1778-atlas scale).
            if let Ok(mut dirs) = self.graph_dirs.write() {
                dirs.insert(corpus_id.clone(), atlas_dir.clone());
            }
        }
        let loaded = self.contexts.read().await.len();
        let graphs_loaded = self.graphs.read().await.len();
        let graphs_available = self.graph_dirs.read().map(|m| m.len()).unwrap_or_default();
        tracing::info!(
            contexts = loaded,
            graphs = graphs_loaded,
            graphs_available,
            "atlas-context: init complete (graphs without a loaded context parse on first use)"
        );
    }

    /// Load one corpus's atlas into the manager (ATLAS_STORAGE_V2 Phase B): load
    /// the v2 store, attach its ANN seed table, and derive the embedding bag from
    /// that table joined to the resident atoms — no re-embed, no
    /// `atoms.embeddings.bin`. Shared by the [`init_internal`] walk and
    /// [`warm_one`]. Returns whether a seed bag loaded.
    ///
    /// Only corpora with a persistent ANN table (`atoms_ann.lance`, written once
    /// at backfill/enrich) produce a seed bag — those atoms are the pool
    /// `atlas_navigate_ann` seeds from, so they're warmed eagerly. A corpus
    /// without a table (structural-only, or not yet backfilled) contributes no
    /// bag; its graph stays lazy (parsed on demand by [`graph`](Self::graph) for
    /// the atom-enumeration path) — eager-loading every store would be needless
    /// RSS. `cache_only` is moot now (loading only reads the table; there is no
    /// embed path), kept for caller-signature compatibility.
    async fn load_corpus(
        &self,
        corpus_id: &str,
        atlas_dir: &std::path::Path,
        cache_only: bool,
    ) -> bool {
        let _ = cache_only;
        // Record where this atlas lives so the lazy `graph()` path can parse it
        // on first request even when there's no seed bag.
        if let Ok(mut dirs) = self.graph_dirs.write() {
            dirs.insert(corpus_id.to_string(), atlas_dir.to_path_buf());
        }
        // ontology-v1 P0.3 — a silent `false` here was the whole failure:
        // a corpus with resolved atoms and no seed table simply never
        // grounded, and nothing said so. Warn — with the fix — when the v2
        // atom store is present (so the corpus COULD ground) and the table is
        // missing or older than `atoms.json`. Never embed here: `init()` walks
        // every installed atlas (1,770 SEP articles) at boot.
        use corpus_engine::enrichment::atlas::ann_store::{ann_table_is_fresh, ann_table_present};
        use corpus_engine::enrichment::atlas::store::ATOMS_LANCE_DIRNAME;
        if !ann_table_present(atlas_dir) {
            if atlas_dir.join(ATOMS_LANCE_DIRNAME).is_dir() {
                tracing::warn!(
                    corpus = corpus_id,
                    atlas = %atlas_dir.display(),
                    "atlas-context: atom store present but no ANN seed table — this corpus cannot ground; \
                     run `svrn atlas backfill-ann {corpus_id}` (or `svrn enrich build {corpus_id}`)"
                );
            } else {
                tracing::debug!(
                    corpus = corpus_id,
                    "atlas-context: no atom store and no ANN seed table; nothing to ground from"
                );
            }
            return false;
        }
        if !ann_table_is_fresh(atlas_dir) {
            tracing::warn!(
                corpus = corpus_id,
                atlas = %atlas_dir.display(),
                "atlas-context: ANN seed table is older than atoms.json — grounding seeds from a stale atom set; \
                 run `svrn atlas backfill-ann {corpus_id}`"
            );
        }
        let load_started = std::time::Instant::now();
        // Load the v2 store (atoms.lance + edges.csr). A corpus without one
        // (e.g. wikipedia — columnar WikipediaGraph, no atom store) is skipped.
        let graph =
            match sovereign_core::atlas_context::AtlasGraph::load_from_disk(corpus_id, atlas_dir) {
                Ok(g) => g,
                Err(e) => {
                    tracing::debug!(corpus = corpus_id, error = %e, "atlas-graph: load skipped");
                    return false;
                }
            };
        // Attach the ANN seed table on THIS long-lived runtime (the held
        // lancedb::Table is queried later by `atlas_navigate_ann`).
        let graph = sovereign_core::atlas_context::open_and_attach_ann_seed_table(
            corpus_id, atlas_dir, graph,
        )
        .await;
        // Derive the bag from the ANN table joined to the resident atoms.
        let context_loaded = match sovereign_core::atlas_context::build_atlas_context_from_ann(
            corpus_id,
            &graph,
            self.filter.top_k,
        )
        .await
        {
            Ok(ctx) => {
                let count = ctx.entries.len();
                self.contexts
                    .write()
                    .await
                    .insert(corpus_id.to_string(), Arc::new(ctx));
                tracing::info!(
                    corpus = corpus_id,
                    entries = count,
                    "atlas-context: bag built from ANN seed table"
                );
                true
            }
            Err(e) => {
                tracing::debug!(corpus = corpus_id, error = %e, "atlas-context: no ANN bag");
                false
            }
        };
        let atom_count = graph.atom_count();
        let edge_count = graph.edge_count();
        self.graphs
            .write()
            .await
            .insert(corpus_id.to_string(), Arc::new(graph));
        tracing::info!(
            corpus = corpus_id,
            atoms = atom_count,
            edges = edge_count,
            load_ms = load_started.elapsed().as_millis(),
            "atlas-graph: loaded"
        );
        context_loaded
    }

    /// Eagerly (re-)embed and load a SINGLE corpus's atlas, bypassing
    /// the cache-only policy of [`init_from_cache`]. For measurement
    /// harnesses (the chaos-monkey bench) that seal to one corpus and
    /// must deterministically ground against its atlas: the daemon's
    /// full [`spawn_init`] embeds EVERY installed corpus (incl. the
    /// 18 GB wikipedia) under the default filter, so it is neither scoped
    /// to nor guaranteed to cover the corpus under test. A cache hit
    /// replays instantly; a miss embeds only this corpus's filter-
    /// admitted atoms and persists the cache for reuse. Returns the
    /// number of context entries now loaded (0 if the atlas is absent or
    /// the filter admitted nothing — the caller should surface that as a
    /// "measuring base retrieval, not the atlas" warning).
    pub async fn warm_one(&self, corpus_id: &str) -> usize {
        let atlas_dir = self.indexes_dir.join(corpus_id).join(ATLAS_DIRNAME);
        if !atlas_dir.join("atoms.json").exists() {
            tracing::warn!(
                corpus = corpus_id,
                dir = %atlas_dir.display(),
                "atlas-context: warm_one found no atoms.json"
            );
            return 0;
        }
        self.load_corpus(corpus_id, &atlas_dir, false).await;
        self.contexts
            .read()
            .await
            .get(corpus_id)
            .map(|c| c.entries.len())
            .unwrap_or(0)
    }

    /// `spawn_init` mirrors `KnowledgeViewManager::spawn_init` —
    /// detaches loading onto a tokio task so daemon startup doesn't
    /// block on the embed pass (cold first run on a wiki-scale
    /// atlas can be tens of seconds; cached subsequent boots are
    /// near-instant).
    pub fn spawn_init(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("atlas-context: starting background init");
            self.init().await;
        })
    }

    /// Same shape as [`spawn_init`] but only loads atlases whose
    /// embeddings are already cached on disk. Use on per-process
    /// startup paths where cold-start embed work would block
    /// first-query latency.
    pub fn spawn_init_from_cache(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("atlas-context: starting cache-only init");
            self.init_from_cache().await;
        })
    }

    /// Spawn a background task that flushes dirty bump maps to disk
    /// every `interval_secs`. Returns the join handle so the daemon
    /// can drop it on shutdown (each next `record_match` would re-
    /// dirty the state and the next interval tick would catch up).
    ///
    /// The flush interval is intentionally generous (default 30 s in
    /// the daemon callsite): a dropped bump on graceful shutdown is
    /// acceptable lossy data. The signal is statistical (does this
    /// article get queried often?), not transactional.
    pub fn spawn_bump_flusher(self: Arc<Self>, interval_secs: u64) -> JoinHandle<()> {
        let dur = std::time::Duration::from_secs(interval_secs.max(1));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(dur);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                self.flush_bumps();
            }
        })
    }

    /// Flush every dirty corpus bump map to its atlas dir. Cheap and
    /// idempotent — non-dirty corpora are skipped, write failures are
    /// logged but don't unset the dirty bit (next tick retries).
    pub fn flush_bumps(&self) {
        let snapshots: Vec<(String, HashMap<String, u64>)> = {
            let mut guard = self.bumps.lock().expect("bumps mutex");
            let mut out = Vec::new();
            for (corpus_id, state) in guard.iter_mut() {
                if !state.dirty {
                    continue;
                }
                out.push((corpus_id.clone(), state.counts.clone()));
                state.dirty = false;
            }
            out
        };
        for (corpus_id, counts) in snapshots {
            let atlas_dir = self.indexes_dir.join(&corpus_id).join(ATLAS_DIRNAME);
            if let Err(e) = write_bump_state(&atlas_dir, &counts) {
                tracing::warn!(corpus = corpus_id, error = %e,
                    "atlas-context: bump flush failed; will retry next tick");
                // Re-mark dirty so the next tick retries.
                let mut guard = self.bumps.lock().expect("bumps mutex");
                if let Some(state) = guard.get_mut(&corpus_id) {
                    state.dirty = true;
                }
            }
        }
    }

    /// Snapshot of the current in-memory bump map for `corpus_id`.
    /// Used by the triage rebuilder to apply user-query priors on top
    /// of centrality + Vital Articles tier. Returns an empty map when
    /// no bumps have been recorded.
    pub fn bump_snapshot(&self, corpus_id: &str) -> HashMap<String, u64> {
        self.bumps
            .lock()
            .expect("bumps mutex")
            .get(corpus_id)
            .map(|s| s.counts.clone())
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl AtlasContextProvider for AtlasContextManager {
    fn get(&self, atlas_corpus_id: &str) -> Option<Arc<AtlasContext>> {
        // Best-effort: the lock is async, but provider callers are
        // synchronous. We use `try_read` — under contention a query
        // gets `None` (atlas grounding off for that turn) rather
        // than blocking the runtime. Init populates the map once
        // and then never writes to it, so contention is essentially
        // zero in practice.
        self.contexts
            .try_read()
            .ok()
            .and_then(|m| m.get(atlas_corpus_id).cloned())
    }

    fn loaded_corpus_ids(&self) -> Vec<String> {
        self.contexts
            .try_read()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn discoverable_corpus_ids(&self) -> Vec<String> {
        // Every registered atlas dir (the cheap path map filled at boot), not
        // just the bags warmed so far — the atom-enumeration path walks graphs,
        // which load lazily on first `graph()`.
        self.graph_dirs
            .read()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    async fn ensure_loaded(&self, ids: &[String]) {
        for id in ids {
            if self.contexts.read().await.contains_key(id) {
                continue; // bag already resident
            }
            let atlas_dir = self.indexes_dir.join(id).join(ATLAS_DIRNAME);
            if atlas_dir.join("atoms.json").exists() {
                // `load_corpus` is idempotent + ANN-gated (a fast no-op for a
                // corpus with no seed table), so warming the scoped set per
                // query is cheap after the first hit.
                self.load_corpus(id, &atlas_dir, false).await;
            }
        }
    }

    fn record_match(&self, atlas_corpus_id: &str, canonical_name: &str) {
        let mut guard = match self.bumps.lock() {
            Ok(g) => g,
            Err(_) => return, // Poisoned mutex — best-effort drop.
        };
        let state = guard.entry(atlas_corpus_id.to_string()).or_default();
        let entry = state.counts.entry(canonical_name.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        state.dirty = true;
    }

    fn graph(
        &self,
        atlas_corpus_id: &str,
    ) -> Option<Arc<sovereign_core::atlas_context::AtlasGraph>> {
        if let Some(g) = self
            .graphs
            .try_read()
            .ok()
            .and_then(|m| m.get(atlas_corpus_id).cloned())
        {
            return Some(g);
        }
        // Miss: parse on demand if the init scan discovered this
        // atlas. Synchronous parse on the caller's thread — the only
        // callers reaching ids without a warm graph are per-id pulls
        // (atom enumeration over a conversation's enabled corpora),
        // and the one-time cost is paid exactly when the graph is
        // actually needed instead of for all 1,700+ atlases at boot.
        let atlas_dir = self
            .graph_dirs
            .read()
            .ok()
            .and_then(|m| m.get(atlas_corpus_id).cloned())?;
        let load_started = std::time::Instant::now();
        // NB: the ANN seed table (ATLAS_STORAGE_V2 3b) is intentionally NOT
        // attached on this lazy path — it is sync (a `Provider` trait method),
        // and the ANN table must be opened on the long-lived async runtime that
        // queries it (see `attach_ann_seed_table`). Lazy graphs are the
        // atom-enumeration / atlas-only siblings, not the `atlas_navigate` seed
        // pool (that pool has embedding context and is eager-loaded WITH the ANN
        // in `load_corpus`). If a seed-pool corpus ever reaches here (a
        // pre-init-completion query), it loads without ANN and the retrieval
        // gate (`has_ann_seed_table` over the whole pool) falls back to the v1
        // cosine seed — correct, just not the ANN win until the eager warm.
        match sovereign_core::atlas_context::AtlasGraph::load_from_disk(atlas_corpus_id, &atlas_dir)
        {
            Ok(graph) => {
                let load_ms = load_started.elapsed().as_millis();
                let graph = Arc::new(graph);
                tracing::info!(
                    corpus = atlas_corpus_id,
                    atoms = graph.atom_count(),
                    load_ms,
                    "atlas-graph: lazy-loaded on first request"
                );
                if let Ok(mut m) = self.graphs.try_write() {
                    m.entry(atlas_corpus_id.to_string())
                        .or_insert_with(|| Arc::clone(&graph));
                }
                Some(graph)
            }
            Err(e) => {
                tracing::warn!(
                    corpus = atlas_corpus_id,
                    error = %e,
                    "atlas-graph: lazy load failed; evicting so it isn't re-parsed every turn"
                );
                if let Ok(mut dirs) = self.graph_dirs.write() {
                    dirs.remove(atlas_corpus_id);
                }
                None
            }
        }
    }
}

// Graph loader lives in `sovereign_core::atlas_context::AtlasGraph::load_from_disk`
// — single canonical implementation shared with the eval CLI. The persistent
// ANN seed table (ATLAS_STORAGE_V2 3b) is opened + attached by
// `sovereign_core::atlas_context::open_and_attach_ann_seed_table` (the single
// attach path shared with the eval), called from `load_corpus` on the daemon's
// long-lived async runtime.

/// Persisted shape of `triage_bumps.json`. `schema_version` lets a
/// future change to the bump weighting cleanly invalidate cached
/// counts without crashing the daemon.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BumpsFile {
    schema_version: u32,
    /// canonical_name → bump_count.
    bumps: HashMap<String, u64>,
}

const BUMPS_SCHEMA: u32 = 1;

fn read_bump_state(atlas_dir: &Path) -> Option<BumpState> {
    let path = atlas_dir.join(TRIAGE_BUMPS_FILE);
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: BumpsFile = serde_json::from_str(&raw).ok()?;
    if parsed.schema_version != BUMPS_SCHEMA {
        tracing::warn!(
            path = %path.display(),
            schema = parsed.schema_version,
            "atlas-context: bump file schema mismatch, ignoring"
        );
        return None;
    }
    Some(BumpState {
        counts: parsed.bumps,
        dirty: false,
    })
}

fn write_bump_state(atlas_dir: &Path, counts: &HashMap<String, u64>) -> std::io::Result<()> {
    std::fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join(TRIAGE_BUMPS_FILE);
    let tmp = atlas_dir.join(format!(".{TRIAGE_BUMPS_FILE}.tmp"));
    let body = BumpsFile {
        schema_version: BUMPS_SCHEMA,
        bumps: counts.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&body).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_signature_is_stable_across_depth_orderings() {
        let a = AtlasContextFilter {
            depth_allowlist: vec!["extracted".into(), "structural_classified".into()],
            ..Default::default()
        };
        let b = AtlasContextFilter {
            depth_allowlist: vec!["structural_classified".into(), "extracted".into()],
            ..Default::default()
        };
        assert_eq!(a.signature(), b.signature());
    }

    /// DARK (`SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS`). Off by default, and
    /// baked into the cache key — a bag built without declared claims must not
    /// be served when the knob flips on.
    #[test]
    fn declared_claim_types_are_dark_and_keyed_into_the_signature() {
        assert!(
            std::env::var("SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS").is_err(),
            "this test asserts the DEFAULT; unset SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS to run it"
        );
        let off = AtlasContextFilter::default();
        assert!(!off.include_declared_claim_types);
        let on = AtlasContextFilter {
            include_declared_claim_types: true,
            ..Default::default()
        };
        assert_ne!(off.signature(), on.signature());
    }

    #[test]
    fn filter_signature_changes_when_tensions_toggled() {
        let off = AtlasContextFilter::default();
        let on = AtlasContextFilter {
            include_tensions: true,
            ..Default::default()
        };
        assert_ne!(
            off.signature(),
            on.signature(),
            "embed cache must invalidate when --atlas-include tension changes"
        );
    }

    #[test]
    fn filter_signature_changes_with_min_chars() {
        let a = AtlasContextFilter {
            min_description_chars: 200,
            ..Default::default()
        };
        let b = AtlasContextFilter {
            min_description_chars: 0,
            ..Default::default()
        };
        assert_ne!(a.signature(), b.signature());
    }

    #[test]
    fn bump_file_roundtrips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("atlas");

        let mut counts = HashMap::new();
        counts.insert("Earth".to_string(), 5u64);
        counts.insert("Mars".to_string(), 1u64);
        write_bump_state(&atlas_dir, &counts).unwrap();

        let parsed = read_bump_state(&atlas_dir).expect("file should be readable");
        assert_eq!(parsed.counts.get("Earth"), Some(&5));
        assert_eq!(parsed.counts.get("Mars"), Some(&1));
        assert!(!parsed.dirty, "freshly-loaded state isn't dirty");
    }

    use futures::Stream;
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
    };
    use std::pin::Pin;

    /// Bare-minimum InferenceProvider. The lazy-graph tests run in
    /// `cache_only` mode with no embeddings cache on disk, so
    /// `load_one` bails before any inference call — every method
    /// panicking keeps that assumption loud.
    struct PanicInference;

    #[async_trait::async_trait]
    impl InferenceProvider for PanicInference {
        async fn complete(
            &self,
            _: &CompletionRequest,
        ) -> sovereign_core::Result<CompletionResponse> {
            unreachable!("graph lazy-load path must not call complete()")
        }

        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> sovereign_core::Result<
            Pin<Box<dyn Stream<Item = sovereign_core::Result<String>> + Send>>,
        > {
            unreachable!("graph lazy-load path must not stream")
        }

        async fn embed(&self, _: &str) -> sovereign_core::Result<Vec<f32>> {
            unreachable!("cache_only init must not embed")
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    /// `<indexes>/<corpus>/atlas/` with `atoms.json` plus the v2 store
    /// (`atoms.lance` + `edges.csr`) so `AtlasGraph::load_from_disk` can load it
    /// (ATLAS_STORAGE_V2 retired the `atoms.json` convert-on-load). A
    /// deliberately-corrupt `atoms_json` (one that doesn't parse) is left
    /// store-less on purpose — its `graph()` then Errs, which the lazy-load
    /// eviction test relies on. No ANN table is written, so the corpus has no
    /// seed bag (its graph stays lazy) — matching the "deferred graph" tests.
    fn write_atlas_fixture(indexes: &Path, corpus: &str, atoms_json: &str) {
        use corpus_engine::enrichment::atlas::{store, AtomsFile};
        let atlas = indexes.join(corpus).join(ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), atoms_json).unwrap();
        if let Ok(file) = serde_json::from_str::<AtomsFile>(atoms_json) {
            store::write_store_blocking(&atlas, corpus, &file.atoms, &[]).unwrap();
        }
    }

    fn manager_for(indexes: &Path) -> AtlasContextManager {
        AtlasContextManager::new(
            indexes.to_path_buf(),
            Arc::new(PanicInference),
            "test-embed".into(),
        )
    }

    const EMPTY_ATOMS: &str = r#"{"schema_version":"2","atoms":[]}"#;

    #[tokio::test]
    async fn init_defers_graphs_for_contextless_atlases() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_fixture(tmp.path(), "t1", EMPTY_ATOMS);
        // Filtered shapes: dot/underscore dirs and a dir without atlas/.
        write_atlas_fixture(tmp.path(), ".hidden", EMPTY_ATOMS);
        write_atlas_fixture(tmp.path(), "_scratch", EMPTY_ATOMS);
        std::fs::create_dir_all(tmp.path().join("no-atlas-here")).unwrap();

        let mgr = manager_for(tmp.path());
        mgr.init_from_cache().await;

        // No embed cache → no context → no graph parsed at init.
        assert_eq!(mgr.contexts.read().await.len(), 0);
        assert_eq!(mgr.graphs.read().await.len(), 0, "graphs must defer");
        // ...but discovery recorded exactly the one legitimate atlas.
        let dirs = mgr.graph_dirs.read().unwrap();
        assert_eq!(dirs.len(), 1);
        assert!(dirs.contains_key("t1"));
    }

    #[tokio::test]
    async fn graph_lazy_loads_on_first_request_and_memoizes() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_fixture(tmp.path(), "t1", EMPTY_ATOMS);
        let mgr = manager_for(tmp.path());
        mgr.init_from_cache().await;
        assert_eq!(mgr.graphs.read().await.len(), 0);

        let g = AtlasContextProvider::graph(&mgr, "t1");
        assert!(g.is_some(), "discovered atlas must lazy-load");
        assert_eq!(
            mgr.graphs.read().await.len(),
            1,
            "lazy load memoizes into the warm map"
        );
        // Unknown id: no discovery entry, no load.
        assert!(AtlasContextProvider::graph(&mgr, "nope").is_none());
    }

    #[tokio::test]
    async fn graph_lazy_load_failure_evicts_discovery_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_fixture(tmp.path(), "bad", "{ not json");
        let mgr = manager_for(tmp.path());
        mgr.init_from_cache().await;
        assert!(mgr.graph_dirs.read().unwrap().contains_key("bad"));

        assert!(AtlasContextProvider::graph(&mgr, "bad").is_none());
        assert!(
            !mgr.graph_dirs.read().unwrap().contains_key("bad"),
            "corrupt atlas must not be re-parsed on every turn"
        );
    }

    #[test]
    fn bump_file_with_unknown_schema_drops_quietly() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        // Write a future schema version we don't understand.
        let payload = serde_json::json!({
            "schema_version": 99,
            "bumps": {"Earth": 1},
        });
        std::fs::write(
            atlas_dir.join(TRIAGE_BUMPS_FILE),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        // Should be ignored, not panic — adaptive priors degrade
        // gracefully on unknown formats.
        assert!(read_bump_state(&atlas_dir).is_none());
    }
}
