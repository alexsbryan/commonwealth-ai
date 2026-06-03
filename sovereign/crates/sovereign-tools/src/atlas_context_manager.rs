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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use corpus_engine::enrichment::atlas::{
    atoms_content_hash, read_atlas_atoms, read_atlas_edges, read_atlas_embeddings,
    write_atlas_embeddings, AtomEnvelope, CachedAtlasEntry, EdgeType, ATLAS_DIRNAME,
};
use sovereign_core::atlas_context::{AtlasContext, AtlasContextProvider, AtlasEntry};
use sovereign_core::traits::InferenceProvider;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// Filename of the per-corpus query-bump map. Lives alongside
/// `atoms.json` so it travels with the atlas (mesh transfer brings
/// it along) and the operator can inspect it without poking inside
/// the daemon.
pub const TRIAGE_BUMPS_FILE: &str = "triage_bumps.json";

/// Same character cap as the eval CLI loader — see
/// `sovereign-cli::eval_cmd::runner::ATLAS_ENTRY_CHAR_LIMIT`.
/// Embed models cap context near ~8K tokens; entities with augmented
/// descriptions can run 18 KB chars, so 3000 chars (~750 tokens)
/// keeps headroom while still covering the strongest signals.
const ATLAS_ENTRY_CHAR_LIMIT: usize = 3000;

/// Render a tension-edge endpoint as a single line for the virtual
/// chunk's embed text. Mirrors the eval-CLI helper of the same name —
/// keep them in sync so the shared embed-cache produces identical
/// payloads on either side.
fn endpoint_text(atom: Option<&AtomEnvelope>, atom_id: &str) -> String {
    use AtomEnvelope::*;
    match atom {
        Some(Entity(e)) => format!("{}: {}", e.canonical_name, e.description),
        Some(Claim(c)) => {
            let act = serde_json::to_string(&c.discourse_act)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let status = serde_json::to_string(&c.epistemic_status)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            format!("[Claim: {act}, {status}] {}", c.content)
        }
        Some(Question(q)) => format!("Question: {}", q.content),
        Some(State(s)) => format!("State: {}", s.label),
        Some(Relation(r)) => format!("Relation: {}", r.label),
        Some(Event(ev)) => format!("Event: {}", ev.description),
        Some(Configuration(cfg)) => format!("{}: {}", cfg.label, cfg.description),
        Some(ArgumentReconstruction(a)) => format!("Argument: {}", a.name),
        Some(Position(p)) => format!("Position ({}): {}", p.stance, p.canonical_name),
        Some(Opposition(o)) => format!("Opposition: {}", o.canonical_label),
        Some(Asset(a)) => {
            let name = if a.original_filename.is_empty() {
                format!("asset:{}", &a.sha256[..12.min(a.sha256.len())])
            } else {
                a.original_filename.clone()
            };
            format!("Asset ({}): {}", a.asset_kind, name)
        }
        None => format!("{atom_id} (missing)"),
    }
}

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
}

impl Default for AtlasContextFilter {
    fn default() -> Self {
        // Defaults are tuned for Wikipedia/SEP-scale corpora where
        // Tier-2 extracted entities carry multi-sentence descriptions.
        // Small-corpus atom schemas (the `conversational` domain
        // produces ~0-150 char descriptions; arch-principles structural
        // atoms similarly short) would be filtered to zero here. Two
        // env knobs let the operator relax the filter at boot without
        // rebuilding:
        //   - SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS=<N> overrides the
        //     200-char floor. `0` admits every atom.
        //   - SOVEREIGN_ATLAS_INCLUDE_DEPTHS=<csv> overrides the
        //     `extracted`-only depth filter. `*` admits every depth.
        // The cache signature (`signature()`) bakes both, so a cache
        // populated under one filter is correctly ignored under
        // another — no risk of cross-contaminating loaded atoms.
        let min_chars = std::env::var("SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);
        let depth_allowlist = match std::env::var("SOVEREIGN_ATLAS_INCLUDE_DEPTHS") {
            Ok(v) if v.trim() == "*" => Vec::new(),
            Ok(v) => v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Err(_) => vec!["extracted".to_string()],
        };
        Self {
            min_description_chars: min_chars,
            depth_allowlist,
            max_entries: None,
            top_k: 3,
            include_claims: false,
            include_tensions: false,
            include_configurations: false,
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
            "min_chars={};depth=[{}];max={};claims={};tensions={};configs={}",
            self.min_description_chars,
            depths.join(","),
            self.max_entries
                .map(|n| n.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.include_claims,
            self.include_tensions,
            self.include_configurations,
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
    /// Used by [`crate::atlas_context::atlas_navigate`] for graph BFS;
    /// without it the runtime falls back to bag-of-atoms cosine
    /// (`atlas_top_k_as_chunks`). Loaded alongside `contexts` at init.
    graphs: Arc<RwLock<HashMap<String, Arc<sovereign_core::atlas_context::AtlasGraph>>>>,
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

    async fn init_internal(&self, cache_only: bool) {
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
            match self.load_one(&corpus_id, &atlas_dir, cache_only).await {
                Ok(ctx) => {
                    let count = ctx.entries.len();
                    self.contexts
                        .write()
                        .await
                        .insert(corpus_id.clone(), Arc::new(ctx));
                    tracing::info!(
                        corpus = corpus_id,
                        entries = count,
                        "atlas-context: loaded"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        corpus = corpus_id,
                        error = %e,
                        "atlas-context: load skipped"
                    );
                }
            }
            // Load the structural graph layer alongside. Independent
            // of embedding load — even if the cache-miss-deferred
            // path skipped the embeddings, the graph itself is cheap
            // to parse and we want it available for graph-walk
            // navigation regardless.
            match sovereign_core::atlas_context::AtlasGraph::load_from_disk(&corpus_id, &atlas_dir) {
                Ok(graph) => {
                    let atom_count = graph.atoms_by_id.len();
                    let edge_out_count: usize =
                        graph.edges_by_source.values().map(|v| v.len()).sum();
                    self.graphs
                        .write()
                        .await
                        .insert(corpus_id.clone(), Arc::new(graph));
                    tracing::info!(
                        corpus = corpus_id,
                        atoms = atom_count,
                        edges = edge_out_count,
                        "atlas-graph: loaded"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        corpus = corpus_id,
                        error = %e,
                        "atlas-graph: load skipped"
                    );
                }
            }
        }
        let loaded = self.contexts.read().await.len();
        let graphs_loaded = self.graphs.read().await.len();
        tracing::info!(
            contexts = loaded,
            graphs = graphs_loaded,
            "atlas-context: init complete"
        );
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

    async fn load_one(
        &self,
        corpus_id: &str,
        atlas_dir: &std::path::Path,
        cache_only: bool,
    ) -> Result<AtlasContext, String> {
        let filter_sig = self.filter.signature();
        let atoms_hash = atoms_content_hash(atlas_dir)
            .map_err(|e| format!("hash atoms.json: {e}"))?;

        // Try cache first.
        match read_atlas_embeddings(atlas_dir, &self.embed_model, &atoms_hash, &filter_sig) {
            Ok(Some(cached)) => {
                let entries = cached
                    .into_iter()
                    .map(|c| AtlasEntry {
                        canonical_name: c.canonical_name,
                        embed_text: c.embed_text,
                        embedding: c.embedding,
                    })
                    .collect();
                return Ok(AtlasContext {
                    atlas_corpus_id: corpus_id.to_string(),
                    entries,
                    top_k: self.filter.top_k,
                });
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    error = %e,
                    "atlas-context: cache read failed; re-embedding"
                );
            }
        }

        if cache_only {
            return Err("no embeddings cache (cache_only mode)".into());
        }

        let atoms = read_atlas_atoms(atlas_dir).map_err(|e| format!("read atoms.json: {e}"))?;

        // The article-slug used as `canonical_name` for non-Entity
        // atoms (Claims, Tensions, etc.). For per-article SEP atlases
        // the corpus_id is `sep-<slug>`; strip the prefix so
        // `score_sources` (rigid title match) credits the right slug
        // when a virtual chunk surfaces. For other atlases the prefix
        // strip is a no-op and the corpus_id itself flows through.
        let article_slug: String = corpus_id
            .strip_prefix("sep-")
            .unwrap_or(corpus_id)
            .to_string();

        let mut payloads: Vec<(String, String)> = Vec::new();
        for atom in &atoms.atoms {
            match atom {
                AtomEnvelope::Entity(e) => {
                    let is_placeholder = e.description.is_empty() && e.salience == 0.0;
                    if is_placeholder {
                        continue;
                    }
                    if e.description.len() < self.filter.min_description_chars {
                        continue;
                    }
                    if !self.filter.depth_allowlist.is_empty() {
                        let depth_label = serde_json::to_string(&e.enrichment_depth)
                            .unwrap_or_default()
                            .trim_matches('"')
                            .to_string();
                        if !self
                            .filter
                            .depth_allowlist
                            .iter()
                            .any(|d| d.eq_ignore_ascii_case(&depth_label))
                        {
                            continue;
                        }
                    }
                    if let Some(cap) = self.filter.max_entries {
                        if payloads.len() >= cap {
                            break;
                        }
                    }
                    let mut text = String::new();
                    text.push_str(&e.canonical_name);
                    text.push('\n');
                    if !e.aliases.is_empty() {
                        text.push_str(&e.aliases.join(", "));
                        text.push('\n');
                    }
                    text.push_str(&e.description);
                    if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                        text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                    }
                    payloads.push((e.canonical_name.clone(), text));
                }
                AtomEnvelope::Claim(c) if self.filter.include_claims => {
                    // Path 2 Phase A: surface Claim atoms as virtual
                    // chunks. `canonical_name = article_slug` so
                    // `score_sources` rigid title-match credits the
                    // article when a claim is in top-K. The embed
                    // text encodes discourse_act + epistemic_status
                    // alongside the proposition itself so cosine
                    // similarity reflects the substantive content,
                    // not the meta-tags.
                    if !self.filter.depth_allowlist.is_empty() {
                        let depth_label = serde_json::to_string(&c.enrichment_depth)
                            .unwrap_or_default()
                            .trim_matches('"')
                            .to_string();
                        if !self
                            .filter
                            .depth_allowlist
                            .iter()
                            .any(|d| d.eq_ignore_ascii_case(&depth_label))
                        {
                            continue;
                        }
                    }
                    if let Some(cap) = self.filter.max_entries {
                        if payloads.len() >= cap {
                            break;
                        }
                    }
                    let act = serde_json::to_string(&c.discourse_act)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string();
                    let status = serde_json::to_string(&c.epistemic_status)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string();
                    let mut text =
                        format!("[Claim: {act}, {status}] {content}", content = c.content);
                    if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                        text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                    }
                    payloads.push((article_slug.clone(), text));
                }
                AtomEnvelope::Configuration(cfg) if self.filter.include_configurations => {
                    if !self.filter.depth_allowlist.is_empty() {
                        let depth_label = serde_json::to_string(&cfg.enrichment_depth)
                            .unwrap_or_default()
                            .trim_matches('"')
                            .to_string();
                        if !self
                            .filter
                            .depth_allowlist
                            .iter()
                            .any(|d| d.eq_ignore_ascii_case(&depth_label))
                        {
                            continue;
                        }
                    }
                    if let Some(cap) = self.filter.max_entries {
                        if payloads.len() >= cap {
                            break;
                        }
                    }
                    let mut text =
                        format!("[Configuration: {}] {}", cfg.label, cfg.description);
                    if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                        text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                    }
                    payloads.push((article_slug.clone(), text));
                }
                AtomEnvelope::ArgumentReconstruction(a) => {
                    // Always include — these are the named-argument
                    // reconstructions Phase 1 extracted. Embed text
                    // is name + premises + conclusion so a question
                    // mentioning the argument name OR matching its
                    // content can seed the navigation onto this atom.
                    if !self.filter.depth_allowlist.is_empty() {
                        let depth_label = serde_json::to_string(&a.enrichment_depth)
                            .unwrap_or_default()
                            .trim_matches('"')
                            .to_string();
                        if !self
                            .filter
                            .depth_allowlist
                            .iter()
                            .any(|d| d.eq_ignore_ascii_case(&depth_label))
                        {
                            continue;
                        }
                    }
                    if let Some(cap) = self.filter.max_entries {
                        if payloads.len() >= cap {
                            break;
                        }
                    }
                    let mut text = String::with_capacity(256);
                    text.push_str("[Argument: ");
                    text.push_str(&a.name);
                    text.push_str("] ");
                    for p in &a.premises {
                        text.push_str(p);
                        text.push(' ');
                    }
                    text.push_str(&a.conclusion);
                    for o in &a.objections {
                        if !o.content.trim().is_empty() {
                            text.push(' ');
                            text.push_str(o.content.trim());
                        } else if !o.name.trim().is_empty() {
                            text.push(' ');
                            text.push_str(o.name.trim());
                        }
                    }
                    if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                        text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                    }
                    payloads.push((article_slug.clone(), text));
                }
                _ => continue,
            }
        }

        // Path 2 Phase B — surface Tension edges as virtual chunks.
        // Mirrors the eval-CLI loader; same embed-text shape so the
        // shared cache key is symmetric. Missing edges.json (older
        // atlases without Phase 6) is non-fatal — log and skip.
        if self.filter.include_tensions {
            let atoms_by_id: HashMap<&str, &AtomEnvelope> = atoms
                .atoms
                .iter()
                .map(|a| (a.id().as_str(), a))
                .collect();
            match read_atlas_edges(atlas_dir) {
                Ok(edges_file) => {
                    for edge in &edges_file.edges {
                        if edge.edge_type != EdgeType::Tension {
                            continue;
                        }
                        if let Some(cap) = self.filter.max_entries {
                            if payloads.len() >= cap {
                                break;
                            }
                        }
                        let src = atoms_by_id.get(edge.source.as_str()).copied();
                        let tgt = atoms_by_id.get(edge.target.as_str()).copied();
                        let sub = edge
                            .sub_question
                            .as_deref()
                            .unwrap_or("(no sub_question recorded)");
                        let mut text = format!("[Tension] {sub}\n");
                        text.push_str(&endpoint_text(src, edge.source.as_str()));
                        text.push_str("\n↔\n");
                        text.push_str(&endpoint_text(tgt, edge.target.as_str()));
                        if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                            text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                        }
                        payloads.push((article_slug.clone(), text));
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        corpus = corpus_id,
                        error = %e,
                        "atlas-context: include_tensions set but edges.json unreadable; skipping"
                    );
                }
            }
        }

        if payloads.is_empty() {
            return Err(format!(
                "filter excluded every entity (min_chars={}, depth={:?})",
                self.filter.min_description_chars, self.filter.depth_allowlist,
            ));
        }

        let t0 = Instant::now();
        let mut entries: Vec<AtlasEntry> = Vec::with_capacity(payloads.len());
        for (name, text) in payloads {
            match self.inference.embed_query(&text).await {
                Ok(v) => entries.push(AtlasEntry {
                    canonical_name: name,
                    embed_text: text,
                    embedding: v,
                }),
                Err(e) => {
                    tracing::warn!(corpus = corpus_id, entity = name, error = %e,
                        "atlas-context: entity embed failed (skipped)");
                }
            }
        }
        tracing::info!(
            corpus = corpus_id,
            entries = entries.len(),
            elapsed_s = t0.elapsed().as_secs_f32(),
            "atlas-context: embedded (cache MISS)"
        );

        // Persist for next boot.
        if !entries.is_empty() {
            let embed_dim = entries[0].embedding.len();
            let cached: Vec<CachedAtlasEntry> = entries
                .iter()
                .map(|e| CachedAtlasEntry {
                    canonical_name: e.canonical_name.clone(),
                    embed_text: e.embed_text.clone(),
                    embedding: e.embedding.clone(),
                })
                .collect();
            if let Err(e) = write_atlas_embeddings(
                atlas_dir,
                &self.embed_model,
                embed_dim,
                &atoms_hash,
                &filter_sig,
                &cached,
            ) {
                tracing::warn!(corpus = corpus_id, error = %e,
                    "atlas-context: cache write failed (non-fatal)");
            }
        }

        Ok(AtlasContext {
            atlas_corpus_id: corpus_id.to_string(),
            entries,
            top_k: self.filter.top_k,
        })
    }
}

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
        self.graphs
            .try_read()
            .ok()
            .and_then(|m| m.get(atlas_corpus_id).cloned())
    }
}

// Graph loader lives in `sovereign_core::atlas_context::AtlasGraph::load_from_disk`
// — single canonical implementation shared with the eval CLI.

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
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(std::io::Error::other)?;
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
