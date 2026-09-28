// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FsIndexSource`] — the installed indexes under one directory, read from
//! the filesystem: list them (deduped by `corpus_id`, the canonical path
//! preferred), open one through a handle cache, and refuse a width the loaded
//! model cannot compare. The [`IndexSource`] a program uses when it reads
//! indexes and links no engine; `corpus_engine::CorpusEngine` holds one and
//! delegates to it, so there is one listing and one cache (phase-b
//! pb-corpus-mcp-reads, FIVE_PROGRAMS §12 decision 5).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::index::CorpusIndex;
use crate::source::IndexSource;
use crate::types::IndexInfo;
use crate::Result;

/// Whether an `open_index` call may admit its handle to the query-path
/// cache. A closed two-valued set, so an enum rather than a `bool` —
/// `open_index_inner(path, false)` at a call site says nothing about
/// what `false` means (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheOnOpen {
    /// Query path: pay the open once, keep the handle resident.
    Yes,
    /// Background walker: read through the cache, never populate it.
    No,
}

/// The read half of an index directory: listing, dedupe, the two caches
/// and the geometry gate. Every path under it is caller-supplied.
pub struct FsIndexSource {
    /// The directory every corpus index lives under.
    index_dir: PathBuf,
    /// Dimensionality the loaded embedding model produces, or 0 for
    /// "not yet known".
    ///
    /// GEOMETRY, NOT NAME, IS THE COMPATIBILITY CONTRACT (clause ST-8).
    /// Vectors of differing width cannot be compared at all; a model
    /// re-quantised or re-spelled at the same width can. Measured on this
    /// host 2026-09-03, 45 installed corpora: the model STRING has three
    /// spellings of one model (`qwen-embedding-0.6b`,
    /// `qwen3-embedding-0.6b`, `Qwen3-Embedding-0.6B-Q8_0`) — three false
    /// alarms — while the two genuinely incompatible corpora (`oicp-types`
    /// and `atos-experiment-oicp-types`, 768-dim against a 1024-dim model)
    /// record the SAME string as the compatible ones. The name check had
    /// zero true positives and could not see the only real conflict.
    ///
    /// An `AtomicUsize` set after construction rather than a builder arg,
    /// because the width is only knowable from a live `embed("probe")` —
    /// which every host runs long after the engine is built. 0 leaves the
    /// gate INACTIVE and says so at `debug`, rather than defaulting a
    /// verdict it has no data for (ARCH §18.3).
    expected_embedding_dimensions: std::sync::atomic::AtomicUsize,
    /// Cache of opened read indexes, keyed by index path. The value pairs
    /// the open `CorpusIndex` (cheap to clone — shared LanceDB handles)
    /// with the mtime of the index's `chunks.lance/_versions` dir at open
    /// time. Retrieval calls `open_index` per corpus per query; without a
    /// cache each call re-`connect`s LanceDB and reloads the table (~5s
    /// for the 1.9M-chunk wikipedia corpus). On a hit at the same on-disk
    /// version we hand back a clone, skipping both the open and the
    /// `info()` chunk-count/dir-size walk. The mtime key self-invalidates
    /// on any write (a commit adds a new `_versions/<n>.manifest`), so a
    /// re-indexed corpus is re-opened without threading invalidation
    /// through every mutation path.
    index_cache: std::sync::Mutex<HashMap<PathBuf, (std::time::SystemTime, CorpusIndex)>>,
    /// Cache of computed `IndexInfo`, keyed by index path + the same
    /// `chunks.lance/_versions` mtime freshness signal as `index_cache`
    /// (falling back to `_corpus_meta.json` mtime so every valid index
    /// caches). `installed_indexes()` runs every gossip tick (~10s) and
    /// walks EVERY installed corpus; without this, a node hosting ~1,800
    /// indexes (e.g. the per-article SEP atlases) re-`open`s + `info()`s
    /// all of them every tick — a btrfs metadata storm that thrashes even
    /// a 128 GB box. Unlike `index_cache` this holds only the small
    /// `IndexInfo` (no LanceDB handle), so caching all ~1,800 is cheap on
    /// memory. Self-invalidates per-index on any committed write.
    index_info_cache: std::sync::Mutex<HashMap<PathBuf, (std::time::SystemTime, IndexInfo)>>,
    /// Optional host-level allow-list of corpus ids to surface from
    /// `installed_indexes`. `None` (the default) lists every installed
    /// corpus. When `Some`, only listed ids are enumerated — and since
    /// every retrieval path and the corpora-list endpoint enumerate via
    /// `installed_indexes`, this scopes both search and the listing to a
    /// chosen set. Set by `sovereign-server` from `[retrieval] corpora`
    /// so a machine full of experiment/partial corpora doesn't search
    /// (or pay to open) the ones the operator doesn't care about.
    corpus_allow_list: Option<std::collections::HashSet<String>>,
    expected_embedding_model: String,
}

impl FsIndexSource {
    /// A source over `index_dir`, listing every corpus, with no model armed.
    pub fn new(index_dir: PathBuf) -> Self {
        Self {
            index_dir,
            expected_embedding_model: String::new(),
            expected_embedding_dimensions: std::sync::atomic::AtomicUsize::new(0),
            index_cache: std::sync::Mutex::new(HashMap::new()),
            index_info_cache: std::sync::Mutex::new(HashMap::new()),
            corpus_allow_list: None,
        }
    }

    /// The model name the indexes are expected to record. Advisory: a
    /// mismatch at the same width warns (see `open_index_inner`).
    pub fn with_embedding_model(mut self, model: &str) -> Self {
        self.expected_embedding_model = model.to_string();
        self
    }

    /// The expected model name; empty when none was declared.
    pub fn expected_embedding_model(&self) -> &str {
        &self.expected_embedding_model
    }

    /// Restrict `installed_indexes` to these corpus ids. An empty list is
    /// "no restriction", so an accidentally-empty config cannot silently
    /// disable all retrieval.
    pub fn with_corpus_allow_list(mut self, ids: Vec<String>) -> Self {
        self.corpus_allow_list = if ids.is_empty() {
            None
        } else {
            Some(ids.into_iter().collect())
        };
        self
    }

    /// Arm the geometry gate with the loaded model's width. `&self` because
    /// the width comes from a live embed probe long after construction.
    pub fn set_expected_embedding_dimensions(&self, dims: usize) {
        self.expected_embedding_dimensions
            .store(dims, std::sync::atomic::Ordering::Relaxed);
    }

    /// The armed width, or 0 when no probe has reported one yet.
    pub fn expected_embedding_dimensions(&self) -> usize {
        self.expected_embedding_dimensions
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The directory every corpus index lives under.
    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// Open the index for `corpus_id` under [`Self::index_dir`], through the
    /// query-path cache.
    pub async fn open_index_for_corpus(&self, corpus_id: &str) -> Result<CorpusIndex> {
        self.open_index(&self.index_dir.join(corpus_id)).await
    }

    /// List all indexes present in the index directory.
    /// Each index is a subdirectory containing LanceDB data.
    ///
    /// **Uniqueness invariant:** at most one entry per `corpus_id`. The
    /// `(corpus_id, chunk_id)` pair is the system's citation handle —
    /// retrieval emits it on every `ScoredChunk` and the reading-surface
    /// HTTP layer dereferences it via [`open_index_for_corpus`], which
    /// always opens `<index_dir>/<corpus_id>`. If two on-disk
    /// directories advertise the same `corpus_id` (typically a stale
    /// per-peer `<corpus>-partition-<peer>/` shard left over after the
    /// canonical merge ran), search would pull chunks from the
    /// partition while the reading desk re-resolves the same chunk id
    /// against the canonical, silently misrouting citations to whatever
    /// chunk happens to live at that row id in the canonical index.
    /// We dedupe by `corpus_id` here, preferring the directory whose
    /// basename equals the `corpus_id` (the canonical that the reading
    /// path will open) and `warn!`-logging the dropped paths so the
    /// operator can clean them up. Rename a stale partition to
    /// `<name>.retired` (or any name containing `.`) to take it
    /// out-of-band reversibly.
    pub async fn installed_indexes(&self) -> Result<Vec<IndexInfo>> {
        let mut indexes = Vec::new();
        if !self.index_dir.is_dir() {
            return Ok(indexes);
        }

        for entry in std::fs::read_dir(&self.index_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip internal directories.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('_') {
                continue;
            }
            if Self::is_out_of_band_index_name(name) {
                continue;
            }
            // Host allow-list (server `[retrieval] corpora`): skip corpora
            // not on the list BEFORE paying the open + info() cost. The
            // directory name equals the corpus_id for canonical installs;
            // partition/shard dirs (name != corpus_id) are skipped too,
            // which is what we want — the canonical dir carries the data.
            if let Some(allow) = &self.corpus_allow_list {
                if !allow.contains(name) {
                    continue;
                }
            }
            // IndexInfo cache check FIRST — before the validity gates — so a
            // hit is stat-only. Freshness key: the `chunks.lance/_versions`
            // mtime (bumped by every committed write — same signal
            // `open_index` uses), falling back to the `_corpus_meta.json`
            // mtime so even legacy-layout indexes cache. Cheap stats only,
            // no file reads. A hit returns the IndexInfo WITHOUT the
            // `CorpusIndex::open` AND without the `_corpus_meta.json` read
            // that `is_ingestion_complete` does below — so a gossip tick
            // (build_local_capabilities, ~10s) over ~1,800 unchanged indexes
            // is stat-only instead of a LanceDB open + JSON parse per index.
            // The mtime self-invalidates on any write — including an
            // ingestion-in-progress meta rewrite — so the validity gates
            // re-run whenever the index actually changes.
            //
            // BOTH mtimes, not the first one that exists. `_versions` alone
            // meant an edit to `_corpus_meta.json` on an index with a
            // chunks.lance was INVISIBLE until the next committed write —
            // and the flags that live only in that file include
            // `query_sharing`, the per-corpus dial that decides whether mesh
            // peers may run federated searches against this copy
            // (`capabilities.rs`, `build_hosted_corpora`). So flipping a
            // corpus to `query_sharing = false` left it advertised, and
            // hosted, for as long as nobody wrote a chunk. A privacy control
            // that silently does not take effect is worse than none (ARCH
            // §18.1 — a guard nobody has watched deny is not a guard).
            //
            // `max` rather than a fallback chain: either write must
            // invalidate, and the cache stays stat-only either way.
            let mtime_of =
                |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let version_mtime = match (
                mtime_of(path.join("chunks.lance").join("_versions")),
                mtime_of(crate::corpus::Corpus::meta_in(&path)),
            ) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            if let Some(mtime) = version_mtime {
                if let Ok(cache) = self.index_info_cache.lock() {
                    if let Some((cached_mtime, info)) = cache.get(&path) {
                        if *cached_mtime == mtime {
                            indexes.push(info.clone());
                            continue;
                        }
                    }
                }
            }
            // Cache miss (new / changed / unusual layout): run the full
            // validity gates, then open + info() and populate the cache.
            // _corpus_meta.json identifies a valid index dir.
            if !crate::corpus::Corpus::meta_in(&path).exists() {
                continue;
            }
            // Skip indexes where ingestion was interrupted (process killed
            // mid-embed). Trace-level: `installed_indexes` is called from
            // several startup paths and at debug produces 5–6 duplicate
            // lines per partial corpus per launch.
            if !CorpusIndex::is_ingestion_complete(&path) {
                tracing::trace!(
                    corpus = name,
                    "Skipping partial index — ingestion was not completed"
                );
                continue;
            }
            match CorpusIndex::open(&path).await {
                Ok(idx) => match idx.info().await {
                    Ok(info) => {
                        if let Some(mtime) = version_mtime {
                            if let Ok(mut cache) = self.index_info_cache.lock() {
                                cache.insert(path.clone(), (mtime, info.clone()));
                            }
                        }
                        indexes.push(info);
                    }
                    Err(e) => {
                        eprintln!("Skipping {}: {e}", path.display());
                    }
                },
                Err(e) => {
                    eprintln!("Skipping {}: {e}", path.display());
                }
            }
        }

        Ok(self.dedupe_by_corpus_id(indexes))
    }

    /// The indexes a CONSUMER may actually search, as opposed to the ones a
    /// writer is not currently touching.
    ///
    /// [`Self::installed_indexes`] answers "is a writer active right now" and
    /// is deliberately gated on `is_ingestion_complete`
    /// (`index::create::is_ingest_finished` states that split). That is the
    /// right question for the resume paths. It is the WRONG question for
    /// retrieval, and the difference is not cosmetic: an ingest that committed
    /// its chunks and died before `build_indexes()` leaves
    /// `ingestion_in_progress: false` beside `indexes_built: false`, so it is
    /// "installed" and unsearchable at the same time.
    ///
    /// Measured 2026-08-30 on this host: 41 corpora carry a meta, 41 pass the
    /// writer predicate, 38 pass this one. The three in the gap
    /// (`e2e-notebook`, `wikipedia-newsworthy`, a folder-governance corpus)
    /// all sit at `committed_iter_pos: 0` with a `chunks.lance` present —
    /// exactly the state that made `corpus status` print `ready` for seven
    /// unsearchable corpora and sent a chaos-soak triage down the wrong path
    /// twice.
    ///
    /// The truth was already on every row: `IndexInfo::indexes_built`. The
    /// defect was that ~84 call sites each decided for themselves what
    /// "installed" meant and only one retrieval leg
    /// (`runtime::retrieval::corpus_search::corpus_unavailability`) checked.
    /// One question, one decider (ARCH §10.6) — ask this one when the question
    /// is "can I search it", and `installed_indexes` only when it is "is a
    /// writer active".
    pub async fn usable_indexes(&self) -> Result<Vec<IndexInfo>> {
        let all = self.installed_indexes().await?;
        let total = all.len();
        let usable: Vec<IndexInfo> = all.into_iter().filter(|i| i.indexes_built).collect();
        if usable.len() != total {
            // Absence is reported, never defaulted (ARCH §18.3). A caller that
            // silently searched fewer corpora than it listed is the bug this
            // accessor exists to prevent, so the drop is always visible.
            tracing::debug!(
                target: "corpus.usable_indexes",
                listed = total,
                usable = usable.len(),
                dropped = total - usable.len(),
                "corpora listed as installed but not searchable (indexes_built=false)"
            );
        }
        Ok(usable)
    }

    /// Collapse `IndexInfo`s with duplicate `corpus_id`s to one entry
    /// each, preferring the directory whose basename equals the
    /// `corpus_id` (i.e., the canonical path that
    /// `open_index_for_corpus` opens). If neither candidate is
    /// canonically named, the lexicographically smaller path wins so
    /// the choice is stable across calls. Every drop emits a `warn!`
    /// naming both paths.
    fn dedupe_by_corpus_id(&self, indexes: Vec<IndexInfo>) -> Vec<IndexInfo> {
        use std::collections::HashMap;

        let mut by_id: HashMap<String, IndexInfo> = HashMap::new();
        for info in indexes {
            let canonical_path = self.index_dir.join(&info.corpus_id);
            match by_id.remove(&info.corpus_id) {
                None => {
                    by_id.insert(info.corpus_id.clone(), info);
                }
                Some(existing) => {
                    let new_is_canonical = info.path == canonical_path;
                    let existing_is_canonical = existing.path == canonical_path;
                    let (kept, dropped) = match (new_is_canonical, existing_is_canonical) {
                        (true, false) => (info, existing),
                        (false, true) => (existing, info),
                        _ => {
                            // Neither (or both — impossible) is canonical.
                            // Pick the lexicographically smaller path so
                            // the kept index is deterministic.
                            if info.path <= existing.path {
                                (info, existing)
                            } else {
                                (existing, info)
                            }
                        }
                    };
                    // Latch: this dedup runs on a hot path (recomputed on
                    // every capability / storage-advertise tick, ~15s), so a
                    // *persistent* collision would re-emit this WARN forever and
                    // bury the log — measured 4214 identical lines in 21h
                    // (2026-07-18). The collision is a steady state, not an
                    // event: warn once per unique (corpus_id, kept, dropped)
                    // triple per process. A genuinely new collision still warns;
                    // an operator who already saw this one is not re-spammed.
                    static WARNED_COLLISIONS: std::sync::OnceLock<
                        std::sync::Mutex<std::collections::HashSet<(String, String, String)>>,
                    > = std::sync::OnceLock::new();
                    let first_seen = WARNED_COLLISIONS
                        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
                        .lock()
                        .map(|mut seen| {
                            seen.insert((
                                kept.corpus_id.clone(),
                                kept.path.display().to_string(),
                                dropped.path.display().to_string(),
                            ))
                        })
                        // Poisoned mutex → fail open (warn) rather than swallow.
                        .unwrap_or(true);
                    if first_seen {
                        tracing::warn!(
                            corpus_id = %kept.corpus_id,
                            kept = %kept.path.display(),
                            dropped = %dropped.path.display(),
                            "installed_indexes: corpus_id collision — multiple physical indexes \
                             advertise the same corpus_id. The dropped one is invisible to search \
                             until you rename it out-of-band (any name containing '.', e.g. \
                             '<name>.retired') or remove it. Without dedup, retrieval and the \
                             reading desk could disagree on which chunk a (corpus_id, chunk_id) \
                             pair resolves to. (Logged once per unique collision per process.)"
                        );
                    }
                    by_id.insert(kept.corpus_id.clone(), kept);
                }
            }
        }

        let mut out: Vec<IndexInfo> = by_id.into_values().collect();
        // Stable order — tests + log diffs need this. read_dir order
        // is filesystem-defined; sort by corpus_id for a deterministic
        // surface.
        out.sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
        out
    }

    /// Out-of-band directories under `<index_dir>/` are user-managed
    /// snapshots, manual backups, or staging artifacts that happen to
    /// share the indexes parent. We never treat them as live corpora.
    ///
    /// Convention: corpus IDs in the wild are alphanumeric + hyphens
    /// only (`wikipedia-simple`, `sep-compatibilism`,
    /// `sovereign-recipes`). A `.` in a directory name is the
    /// unambiguous opt-out marker — anything matching gets quietly
    /// skipped by both `installed_indexes()` and
    /// `in_progress_ingestions()`. That stops auto-resume from
    /// repeatedly trying to fetch a registry recipe for the rogue
    /// directory and spamming WARN lines.
    ///
    /// Existing internal directories already use a `_` prefix to
    /// signal "skip this" (`_downloads`, `_corpus_meta.json`); the
    /// `.`-rule is a parallel out-of-band marker for user-facing
    /// artifacts where leading-underscore would feel hidden.
    pub fn is_out_of_band_index_name(name: &str) -> bool {
        name.contains('.')
    }

    /// Open an index for search, caching the open handle. Validates the
    /// embedding model on first open.
    ///
    /// Retrieval calls this once per corpus per query. Re-opening LanceDB
    /// each time dominates chat latency on large corpora (the 1.9M-chunk
    /// wikipedia index takes ~5s to `connect` + load). We cache the open
    /// `CorpusIndex` keyed by path, validated against the LanceDB commit
    /// marker mtime so any write transparently invalidates the entry.
    pub async fn open_index(&self, path: &Path) -> Result<CorpusIndex> {
        self.open_index_inner(path, CacheOnOpen::Yes).await
    }

    /// Open an index WITHOUT admitting it to the query-path cache.
    ///
    /// For background walkers that visit every installed corpus on a
    /// timer. `open_index`'s cache is a query-path accelerator with no
    /// eviction: whatever it holds is resident for the life of the
    /// process. A sweep that opens all N installed indexes through it
    /// therefore makes every LanceDB handle on the box permanently
    /// resident after one tick, whether or not anyone ever queries that
    /// corpus — at 1000 corpora that is the difference between "the
    /// hot set is resident" and "everything is resident"
    /// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.4 item 7).
    ///
    /// A cache HIT is still served from the cache: reusing a handle the
    /// query path already paid for is free and changes nothing about
    /// residency. Only the INSERT is suppressed. That asymmetry is the
    /// whole fix — the sweep may benefit from the query path's cache,
    /// but it may not populate it.
    ///
    /// Note also why the LRU proposed in §5 was rejected in favour of
    /// this: an hourly all-corpora scan is a textbook LRU-flusher, and
    /// would evict the hot set every tick.
    pub async fn open_index_transient(&self, path: &Path) -> Result<CorpusIndex> {
        self.open_index_inner(path, CacheOnOpen::No).await
    }

    /// How many index handles the query-path cache is currently holding
    /// resident. The observability surface for the residency question
    /// above — and what the sweep's regression test asserts on.
    pub fn index_cache_len(&self) -> usize {
        self.index_cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Clause ST-8's fail-closed half: may this engine read this corpus's
    /// vectors at all?
    ///
    /// Operator direction 2026-09-03, resolving the standing conflict the old
    /// check carried: "we don't want to fail loudly when a model is usable —
    /// the same model with a different file name or quant should still work,
    /// but clearly incompatible from a functional standpoint is what we want
    /// to flag."
    ///
    /// Width is that functional line. Vectors of different width cannot be
    /// compared, so an open against one is not a degraded search but a
    /// meaningless one — and until now NOTHING refused it: `open_index` only
    /// warned, and `validate_corpus_readiness`, which does return `Err`, has a
    /// single caller that catches the `Err` and warns
    /// (`sovereign-desktop/src-tauri/src/state.rs`). The clause's "MUST fail
    /// loudly" was enforced on no path at all.
    ///
    /// CALLED ON THE CACHE-HIT PATH TOO, and that is not incidental. The width
    /// is armed by a live embed probe that runs LONG AFTER boot, so at least
    /// one open of every corpus happens while the gate is still inactive. A
    /// check on the slow path alone would let every already-resident handle
    /// bypass it for the life of the process — which is this repo's
    /// characteristic bug wearing the gate's own clothes. Caught by the test
    /// below opening once unarmed before arming.
    fn geometry_verdict(&self, index: &CorpusIndex) -> Result<()> {
        let expected_dims = self.expected_embedding_dimensions();
        let recorded_dims = index.embedding_dim();
        if expected_dims != 0 && recorded_dims != 0 && expected_dims != recorded_dims {
            return Err(crate::Error::Database(format!(
                "Corpus '{}' was built with {} embedding dimensions but the \
                 loaded model produces {}. Its vectors cannot be compared with \
                 this model's, so searching it would return meaningless \
                 results rather than degraded ones. To fix: rebuild the corpus \
                 in Settings → Knowledge → Rebuild.",
                index.corpus_id(),
                recorded_dims,
                expected_dims,
            )));
        }
        if expected_dims == 0 {
            tracing::debug!(
                corpus_id = index.corpus_id(),
                recorded_dims,
                "open_index: geometry gate INACTIVE — no embed probe has \
                 reported a width to this engine yet"
            );
        }
        Ok(())
    }

    async fn open_index_inner(&self, path: &Path, cache: CacheOnOpen) -> Result<CorpusIndex> {
        // Cheap freshness key: the mtime of the table's `_versions` dir,
        // which gains a new `<n>.manifest` on every committed write. `None`
        // (unexpected layout) → skip the cache and always re-open.
        let version_mtime = std::fs::metadata(path.join("chunks.lance").join("_versions"))
            .and_then(|m| m.modified())
            .ok();

        // Fast path: a cached open at the same on-disk version. Hand back a
        // clone (shared LanceDB handles) — no re-`connect`, no `info()`
        // chunk-count/dir-size walk.
        if let Some(mtime) = version_mtime {
            if let Ok(cache) = self.index_cache.lock() {
                if let Some((cached_mtime, index)) = cache.get(path) {
                    if *cached_mtime == mtime {
                        let hit = index.clone();
                        drop(cache);
                        // Gate the HIT as well — see `geometry_verdict`.
                        self.geometry_verdict(&hit)?;
                        return Ok(hit);
                    }
                }
            }
        }

        // Slow path: open, validate the embedding model, then cache. The model
        // name comes off the handle's meta, NOT `info()`: `info()` also walks
        // the whole index directory for a byte total, which on a 113 GB
        // wikipedia index cost seconds on every re-open — and a background
        // reindexer committing one document every few seconds forced a
        // re-open on nearly every claim search (measured 2026-09-02, issue #57).
        let mut index = CorpusIndex::open(path).await?;

        // A re-open of a path we already hold means the on-disk version moved
        // under us (a committed external write). The search gate that handle
        // computed — IVF built, FTS built, rows above the flat-scan threshold
        // — is not what an append changes, so the new handle inherits it and
        // `GATE_CACHE_TTL` bounds the belief. Without this every such
        // re-open paid `count_rows` + `list_indices` cold: 4.4-9.7 s on the
        // 2.0M-row table while the reindexer was writing to it.
        if let Ok(cache) = self.index_cache.lock() {
            if let Some((_, prev)) = cache.get(path) {
                index.share_gate_cache_from(prev);
            }
        }

        self.geometry_verdict(&index)?;

        // The NAME, by contrast, is advisory. It is not a reliable identity:
        // one model appears here under three spellings, and a re-quantised
        // artefact of the same model is fully usable. Resolving a stem to its
        // base model + quant is `sovereign_core::models_manifest::
        // attribution_for_file`, which corpus-engine cannot reach without
        // inverting the layer map — so this stays a warning, and the residual
        // gap (a DIFFERENT base model at the same width) is named in clause
        // ST-8 rather than pretended away.
        if index.embedding_model() != self.expected_embedding_model {
            tracing::warn!(
                "Corpus '{}' records embedding model '{}' but this engine expects \
                 '{}'. The widths agree, so this is most likely the same model \
                 under a different file name or quantisation and search is \
                 fine. Re-install only if results look wrong.",
                index.corpus_id(),
                index.embedding_model(),
                self.expected_embedding_model,
            );
        }

        // Cache only when we have a freshness key to validate against —
        // and only when the CALLER is a query-path caller. See
        // `open_index_transient`.
        match cache {
            CacheOnOpen::Yes => {
                if let Some(mtime) = version_mtime {
                    if let Ok(mut cache) = self.index_cache.lock() {
                        cache.insert(path.to_path_buf(), (mtime, index.clone()));
                    }
                }
            }
            CacheOnOpen::No => {
                tracing::debug!(
                    index_path = %path.display(),
                    resident_handles = self.index_cache_len(),
                    "open_index: transient open — handle NOT admitted to the query cache"
                );
            }
        }

        Ok(index)
    }
}

#[async_trait]
impl IndexSource for FsIndexSource {
    async fn usable_indexes(&self) -> Result<Vec<IndexInfo>> {
        FsIndexSource::usable_indexes(self).await
    }

    async fn open_index(&self, path: &Path) -> Result<CorpusIndex> {
        FsIndexSource::open_index(self, path).await
    }
}

#[cfg(test)]
mod tests;
