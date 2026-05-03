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
    atoms_content_hash, read_atlas_atoms, read_atlas_embeddings, write_atlas_embeddings,
    AtomEnvelope, CachedAtlasEntry, ATLAS_DIRNAME,
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

/// Filter applied during atlas-context loading. Mirrors the shape
/// of the eval CLI's `AtlasLoadFilter` so the cache key derived
/// here is comparable to what the CLI writes / reads.
#[derive(Debug, Clone)]
pub struct AtlasContextFilter {
    pub min_description_chars: usize,
    pub depth_allowlist: Vec<String>,
    pub max_entries: Option<usize>,
    pub top_k: usize,
}

impl Default for AtlasContextFilter {
    fn default() -> Self {
        Self {
            min_description_chars: 200,
            // Only Tier-2 extracted entities by default. Structural
            // entities have one-line article-lead descriptions that
            // dilute retrieval; they're loaded if the operator
            // explicitly opts in via `with_filter`.
            depth_allowlist: vec!["extracted".to_string()],
            max_entries: None,
            top_k: 3,
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
            "min_chars={};depth=[{}];max={}",
            self.min_description_chars,
            depths.join(","),
            self.max_entries
                .map(|n| n.to_string())
                .unwrap_or_else(|| "none".to_string()),
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
        }
        let loaded = self.contexts.read().await.len();
        tracing::info!(loaded, "atlas-context: init complete");
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

        let mut payloads: Vec<(String, String)> = Vec::new();
        for atom in &atoms.atoms {
            let AtomEnvelope::Entity(e) = atom else {
                continue;
            };
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
}

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
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_signature_is_stable_across_depth_orderings() {
        let a = AtlasContextFilter {
            min_description_chars: 200,
            depth_allowlist: vec!["extracted".into(), "structural_classified".into()],
            max_entries: None,
            top_k: 3,
        };
        let b = AtlasContextFilter {
            min_description_chars: 200,
            depth_allowlist: vec!["structural_classified".into(), "extracted".into()],
            max_entries: None,
            top_k: 3,
        };
        assert_eq!(a.signature(), b.signature());
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
