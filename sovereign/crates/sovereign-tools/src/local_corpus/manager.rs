// SPDX-License-Identifier: AGPL-3.0-or-later
//! `LocalCorpusManager` — single entry point for both the Folder Drop
//! and Obsidian Vault flows.
//!
//! Construction: `init(engine, store, inference, data_dir, snapshot_root)`.
//! On construction we load every persisted `LocalCorpusConfig` from
//! `{data_dir}/local-corpora/*.json` so corpora survive relaunches
//! without any StateStore changes. The engine and the store remain
//! authoritative for their own scopes (index on disk, corpus rows in
//! the DB); we only own the "which local corpora does the user have?"
//! list and the pre-scan/extraction glue in front of the engine.
//!
//! See `/Users/user/.claude/plans/binary-scribbling-babbage.md`
//! for the broader architecture.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, SensitiveCorpusOracle, StateStore};
use tokio::sync::RwLock;

use corpus_engine::{CorpusEngine, CorpusSpec, ScoredChunk};

use super::config::{recipe_toml, LocalCorpusConfig};
use super::extract_stage::{self, default_staging_path};
use super::ocr::{OcrCtx, PageProgress, PageProgressCallback};
use super::pre_scanner::{PreScanResult, PreScanner};
use super::progress::{ExcerptChunk, LocalCorpusProgress, RuntimeFailure};

/// Predicate gating the auto-rebuild watchdog (Move 8 — folder-ingest
/// v1 §3.6). Returns `true` only when the corpus has a finished
/// tiered build sitting on disk: a fresh rebuild then makes sense
/// because new chunks from the just-completed sweep are not yet
/// reflected in `chunk_entities` / RAPTOR / motifs.
///
/// Returns `false` for every other shape:
///
/// - `None` — no live status; enrichment never ran for this corpus.
/// - `Off` — user disabled enrichment; do not silently re-enable.
/// - `Building*` / `Tiered { Pending | Indexing | BuildingSkeleton |
///    PartiallyReady | Failed }` — a build is in flight or has not
///    yet reached usable state; preempting it with a fresh sweep
///    would discard partial work.
/// - `Complete` / `Failed` (legacy) — pre-tiered status; the legacy
///    subprocess path is not auto-rebuild safe.
pub(crate) fn should_fire_auto_rebuild(
    status: Option<&super::watched::state::EnrichmentRuntimeStatus>,
) -> bool {
    use super::watched::state::EnrichmentRuntimeStatus;
    use sovereign_core::types::AssetState;
    matches!(
        status,
        Some(EnrichmentRuntimeStatus::Tiered {
            state: AssetState::Ready | AssetState::MultiHopReady,
            ..
        })
    )
}

// ─── Public result types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestStats {
    pub corpus_id: String,
    pub files_indexed: usize,
    pub chunks_written: u64,
    /// Files the pre-scan approved but that failed during the
    /// staging/extraction step. Named individually on the completion
    /// screen per spec §9.
    pub runtime_failures: Vec<RuntimeFailure>,
    /// Top 3 excerpts for the completion screen. Populated by M2
    /// (the excerpt scorer) — empty in M1.
    pub excerpt_chunks: Vec<ExcerptChunk>,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteJob {
    pub corpus_id: String,
    pub display_name: String,
    pub files_done: usize,
    pub files_total: usize,
}

/// A watched-folder corpus the user should know about — not in
/// `Idle` status. Surfaced by `LocalCorpusManager::watched_incomplete_jobs`
/// to the desktop's ResumePrompt and the CLI's `corpus watch-list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedIncompleteJob {
    pub corpus_id: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub status: super::watched::status::WatchedFolderStatus,
    pub tombstones: usize,
    pub failed_files: usize,
}

/// One registered corpus plus its current disk-level summary, shown
/// in the settings list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusSummary {
    pub config: LocalCorpusConfig,
    /// Number of source files listed as `Complete` in the manifest.
    pub files_done: usize,
    /// Total files in the manifest (0 if the ingest never started).
    pub files_total: usize,
    /// True if an ingest is currently running for this corpus (best
    /// effort — based on the engine's canonical meta flag).
    pub in_progress: bool,
}

/// Thread-safe progress callback — matches the shape the UI layer
/// already uses for public corpora.
pub type ProgressCallback = Arc<dyn Fn(LocalCorpusProgress) + Send + Sync>;

// ─── Manager ─────────────────────────────────────────────────────────

pub struct LocalCorpusManager {
    engine: Arc<CorpusEngine>,
    store: Arc<dyn StateStore>,
    #[allow(dead_code)]
    inference: Option<Arc<dyn InferenceProvider>>,
    data_dir: PathBuf,
    snapshot_root: PathBuf,
    /// Directory the manager writes generated recipe TOMLs to —
    /// `<recipes_dir>/<corpus_id>.toml`. Must match the
    /// `CorpusEngine`'s `overrides_dir` so `fetch_recipe(corpus_id)`
    /// resolves at apply time. Defaulted by [`init`] to
    /// `{data_dir}/local-corpus-recipes/`; production daemons should
    /// use [`init_with_recipes_dir`] to point this at the engine's
    /// own recipes_dir.
    recipes_dir: PathBuf,
    corpora: RwLock<HashMap<String, LocalCorpusConfig>>,
    /// Cache of the most recent `LabeledClusterResult` per corpus, so
    /// `build_preview` can be called separately from `cluster` without
    /// re-running the LLM labelling pass. Cleared on app restart —
    /// the user re-clicks "Organize" each session.
    cluster_results: RwLock<HashMap<String, super::clusterer::LabeledClusterResult>>,
    /// Optional OCR runtime config (Tesseract sidecar path, daemon
    /// URL, DPI, …). The desktop layer sets this at boot via
    /// `set_ocr_ctx`; the CLI / tests run without it. When `None`,
    /// any corpus with `ocr_pdfs = true` falls back to surfacing the
    /// scanned PDFs as runtime failures rather than silently
    /// dropping them.
    ocr_ctx: RwLock<Option<OcrCtx>>,
    /// Folder-ingest v1 §3.3 — per-folder enrichment driver.
    /// Owns the per-corpus build queue + global concurrency cap.
    /// Defaults are installed at daemon boot via
    /// `set_enrichment_defaults`; before that, `enable_enrichment`
    /// returns an error.
    enrichment_driver: Arc<super::watched::enrich::EnrichmentDriver>,
    /// Live enrichment progress per corpus. The driver's progress
    /// callback writes here on every parsed `EnrichProgress` event
    /// so the HTTP `/details` route can render the current phase
    /// without polling the on-disk state file (which we only
    /// rewrite on completion / cancellation / disable, to avoid
    /// fsync churn). A `std::sync::RwLock` is the right primitive
    /// because the writers are sync callbacks fired from the
    /// subprocess stdout reader; tokio's `RwLock` would require a
    /// blocking lock in those paths.
    enrichment_progress:
        Arc<std::sync::RwLock<HashMap<String, super::watched::state::EnrichmentRuntimeStatus>>>,
    /// Folder-ingest v1 §3.6 — debounced auto-rebuild watchdog.
    ///
    /// The watched-folder worker emits `SweepCompleted` after every
    /// reconciliation pass. When the pass actually changed something
    /// (added/modified/removed > 0), the tiered enrichment built off
    /// the prior snapshot has gone stale — new chunks lack
    /// `chunk_entities` / RAPTOR cluster membership / motif coverage
    /// and PPR rerank under-weights them until the next manual
    /// rebuild.
    ///
    /// Rather than rebuild after every sweep (folders with bursty
    /// edits would thrash the GliNER + RAPTOR pipeline), we debounce
    /// per corpus: each qualifying `SweepCompleted` resets a sleep
    /// task. When the sleep expires without a fresh sweep, the
    /// watchdog fires `rebuild_enrichment` — but only when the
    /// corpus's enrichment status is `Tiered { state: Ready |
    /// MultiHopReady }`, so we never preempt an in-flight build.
    ///
    /// The map keys on `corpus_id` → handle of the pending sleep
    /// task. Replacing a key aborts the prior task before storing
    /// the new one (effective debounce reset).
    auto_rebuild_tasks: Arc<tokio::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Debounce delay for `on_sweep_event`. Defaults to 5 minutes per
    /// the plan; tests override via `set_auto_rebuild_debounce` to
    /// keep them fast.
    auto_rebuild_debounce: Arc<std::sync::RwLock<std::time::Duration>>,
    /// Corpus ids with a [`Self::ingest`] call currently in flight,
    /// keyed to a count (concurrent ingests of one corpus are legal —
    /// they share the engine-side cancellation flag). [`Self::remove`]
    /// awaits this reaching zero (alongside the engine's own
    /// cancellation registry) before wiping: the engine's
    /// `remove_corpus_everything` contract requires the in-flight
    /// writer to exit first, otherwise it recreates index files after
    /// the delete — or the wipe itself fails under the writer's feet
    /// (observed as a 500 from the watched-folder DELETE route when a
    /// register's detached initial ingest was still building).
    ///
    /// `std::sync::RwLock`: the guard increments/decrements in sync
    /// Drop code, and critical sections are a single map op.
    active_ingests: Arc<std::sync::RwLock<HashMap<String, usize>>>,
}

/// RAII ticket marking one in-flight `ingest()` for `corpus_id` in
/// [`LocalCorpusManager::active_ingests`]. Created before the config
/// precondition check so `remove()` can never observe "no ticket" for
/// an ingest call that later writes.
struct IngestTicket {
    map: Arc<std::sync::RwLock<HashMap<String, usize>>>,
    corpus_id: String,
}

impl IngestTicket {
    fn acquire(map: &Arc<std::sync::RwLock<HashMap<String, usize>>>, corpus_id: &str) -> Self {
        *map.write()
            .expect("active_ingests poisoned")
            .entry(corpus_id.to_string())
            .or_insert(0) += 1;
        Self {
            map: map.clone(),
            corpus_id: corpus_id.to_string(),
        }
    }
}

impl Drop for IngestTicket {
    fn drop(&mut self) {
        let mut map = self.map.write().expect("active_ingests poisoned");
        if let Some(count) = map.get_mut(&self.corpus_id) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.corpus_id);
            }
        }
    }
}

impl LocalCorpusManager {
    /// Construct + load persisted corpora. `data_dir` is the Sovereign
    /// data directory (typically `~/.sovereign`). Sidecars live under
    /// `{data_dir}/local-corpora/` and staging under
    /// `{data_dir}/local-corpus-staging/`. `snapshot_root` is the
    /// default root for vault snapshots (used only by Obsidian flows,
    /// M5).
    ///
    /// Defaults the recipe-output directory to
    /// `{data_dir}/local-corpus-recipes/`. The daemon should prefer
    /// [`init_with_recipes_dir`] so the manager writes recipes into
    /// the same path the engine reads from — otherwise sweeps error
    /// with `No registry entry for corpus '…'`.
    pub async fn init(
        engine: Arc<CorpusEngine>,
        store: Arc<dyn StateStore>,
        inference: Option<Arc<dyn InferenceProvider>>,
        data_dir: PathBuf,
        snapshot_root: PathBuf,
    ) -> Result<Self> {
        let default_recipes_dir = data_dir.join("local-corpus-recipes");
        Self::init_with_recipes_dir(
            engine,
            store,
            inference,
            data_dir,
            snapshot_root,
            default_recipes_dir,
        )
        .await
    }

    /// Same as [`init`] but with an explicit `recipes_dir`. The
    /// daemon's `CorpusEngine` is constructed with one overrides
    /// directory; the manager must write its generated recipe TOMLs
    /// into the same directory, otherwise the engine's
    /// `fetch_recipe(corpus_id)` returns `No registry entry for
    /// corpus '<id>'` and every reconciliation sweep errors at apply
    /// time. Threading the directory through here lets the daemon
    /// keep the two in sync without colocating production-layout
    /// constants in two crates.
    pub async fn init_with_recipes_dir(
        engine: Arc<CorpusEngine>,
        store: Arc<dyn StateStore>,
        inference: Option<Arc<dyn InferenceProvider>>,
        data_dir: PathBuf,
        snapshot_root: PathBuf,
        recipes_dir: PathBuf,
    ) -> Result<Self> {
        let corpora_dir = config_dir(&data_dir);
        std::fs::create_dir_all(&corpora_dir)
            .map_err(|e| Error::Execution(format!("create corpora dir: {e}")))?;

        let corpora = load_persisted_configs(&corpora_dir)?;

        // Backfill index metadata onto corpora registered before the
        // fields existed (2026-06-10 obsidian audit). Newly registered
        // corpora get both stamps at ingest from the generated recipe;
        // this covers the already-installed ones. Pure meta I/O —
        // no-op once stamped, never clobbers an existing value.
        //
        // - `personal_scope=true`: every local corpus is the user's
        //   own files, so the runtime's personal-scope retrieval
        //   filter must retain it (the old prefix-only match silently
        //   dropped `watched-<hash>` ids).
        // - `display` (category/icon): `is_tiered_category` gates the
        //   tiered retrieval surface (RAPTOR briefings + entity-PPR
        //   rerank) on the category, so an unstamped vault was
        //   silently exempt from entity-aware retrieval.
        let index_root = data_dir.join("indexes");
        for (corpus_id, cfg) in corpora.iter() {
            let dir = index_root.join(corpus_id);
            if !dir.join("_corpus_meta.json").exists() {
                continue; // registered but never ingested
            }
            match corpus_engine::index::backfill_personal_scope(&dir, true) {
                Ok(true) => tracing::info!(
                    corpus = %corpus_id,
                    "backfilled personal_scope=true onto local corpus meta"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "personal_scope backfill failed — personal-scope \
                     retrieval may drop this corpus"
                ),
            }
            if let Some(display) = cfg.source_type.display_meta() {
                let category = display.category.clone();
                match corpus_engine::index::backfill_display(&dir, display) {
                    Ok(true) => tracing::info!(
                        corpus = %corpus_id,
                        category = category.as_deref().unwrap_or(""),
                        "backfilled display category onto local corpus meta"
                    ),
                    Ok(false) => {}
                    Err(e) => tracing::warn!(
                        corpus = %corpus_id,
                        error = %e,
                        "display backfill failed — corpus may miss the \
                         tiered retrieval surface (entity-PPR, briefings)"
                    ),
                }
            }
        }

        Ok(Self {
            engine,
            store,
            inference,
            data_dir,
            snapshot_root,
            recipes_dir,
            corpora: RwLock::new(corpora),
            cluster_results: RwLock::new(HashMap::new()),
            ocr_ctx: RwLock::new(None),
            enrichment_driver: Arc::new(super::watched::enrich::EnrichmentDriver::new()),
            enrichment_progress: Arc::new(std::sync::RwLock::new(HashMap::new())),
            auto_rebuild_tasks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            auto_rebuild_debounce: Arc::new(std::sync::RwLock::new(
                std::time::Duration::from_secs(300),
            )),
            active_ingests: Arc::new(std::sync::RwLock::new(HashMap::new())),
        })
    }

    /// Install an OCR runtime context. Called once at desktop boot
    /// after the Tauri layer has resolved the bundled tesseract /
    /// pdfium paths. Subsequent calls overwrite. Idempotent.
    pub async fn set_ocr_ctx(&self, ctx: OcrCtx) {
        *self.ocr_ctx.write().await = Some(ctx);
    }

    /// Folder-ingest v1 §3.3: install the daemon-side enrichment
    /// defaults (chat_model, embed_model, base_url, optional CLI
    /// path). Called once at daemon boot — without this, the
    /// `enable_enrichment` path returns an error pointing the
    /// operator at daemon setup.
    pub async fn set_enrichment_defaults(
        &self,
        defaults: super::watched::enrich::EnrichmentDefaults,
    ) {
        self.enrichment_driver.set_defaults(defaults).await;
    }

    /// Install in-process tiered-enrichment deps (FolderTieredProvider
    /// + optional GliNER extractor). Once set, `enable_enrichment`
    /// routes through `start_tiered_build` instead of the legacy
    /// subprocess. Idempotent.
    pub async fn set_tiered_deps(&self, deps: super::watched::enrich::TieredDeps) {
        self.enrichment_driver.set_tiered_deps(deps).await;
    }

    /// Override the debounce window used by `on_sweep_event`. Production
    /// default is 5 minutes (set in `init_with_recipes_dir`); tests
    /// shorten this so they don't have to wait. No-op on a poisoned
    /// lock — debounce timing is a best-effort heuristic, not a
    /// correctness lever.
    pub fn set_auto_rebuild_debounce(&self, dur: std::time::Duration) {
        if let Ok(mut guard) = self.auto_rebuild_debounce.write() {
            *guard = dur;
        }
    }

    /// Folder-ingest v1 §3.6 — handle one worker event for the
    /// auto-rebuild watchdog. Called by the daemon's event sink for
    /// every `WatchedFolderEvent`. Only `SweepCompleted` with a non-
    /// empty diff drives behaviour; every other variant short-circuits
    /// so the sink stays cheap.
    ///
    /// Behaviour on a qualifying `SweepCompleted`:
    ///
    /// 1. Cancel + drop any prior pending rebuild task for this
    ///    corpus (debounce reset — bursty edits collapse into one
    ///    rebuild at the tail of the burst).
    /// 2. Spawn a new tokio task that sleeps `auto_rebuild_debounce`.
    /// 3. On expiry, gate-check the corpus's enrichment status:
    ///    `Tiered { Ready | MultiHopReady }` → fire
    ///    `rebuild_enrichment`. Any other state (Off, in-flight
    ///    Building / PartiallyReady, Failed, legacy Complete /
    ///    Building) → skip silently. Rationale: rebuilding requires
    ///    enrichment to be configured *and* finished; we never
    ///    preempt an in-flight build.
    ///
    /// Errors during `rebuild_enrichment` are logged at `warn!` and
    /// dropped — the user can manually rebuild from the UI if a
    /// transient daemon condition (no inference, paused corpus)
    /// blocked the auto-pass. Calls are best-effort by design.
    pub async fn on_sweep_event(
        self: &Arc<Self>,
        event: &super::watched::events::WatchedFolderEvent,
    ) {
        use super::watched::events::WatchedFolderEvent;
        let (corpus_id, applied) = match event {
            WatchedFolderEvent::SweepCompleted {
                corpus_id, applied, ..
            } => (corpus_id.clone(), applied.clone()),
            _ => return,
        };
        let changed = applied.added + applied.modified + applied.removed;
        if changed == 0 {
            return;
        }
        let debounce = self
            .auto_rebuild_debounce
            .read()
            .map(|g| *g)
            .unwrap_or_else(|_| std::time::Duration::from_secs(300));
        let me = Arc::clone(self);
        let corpus_for_task = corpus_id.clone();
        let mut tasks = self.auto_rebuild_tasks.lock().await;
        if let Some(prior) = tasks.remove(&corpus_id) {
            prior.abort();
        }
        let handle = tokio::spawn(async move {
            tokio::time::sleep(debounce).await;
            me.fire_auto_rebuild(&corpus_for_task).await;
        });
        tasks.insert(corpus_id, handle);
    }

    /// Inner: debounce expired — gate-check status, fire rebuild if
    /// safe. Extracted so the gate logic is unit-testable without
    /// driving real time.
    async fn fire_auto_rebuild(self: &Arc<Self>, corpus_id: &str) {
        let status = self.enrichment_progress(corpus_id);
        let should_fire = should_fire_auto_rebuild(status.as_ref());
        if !should_fire {
            tracing::debug!(
                corpus_id = %corpus_id,
                ?status,
                "auto_rebuild:skipped — status not Ready/MultiHopReady"
            );
            // Clear the slot so a later sweep can re-arm.
            let mut tasks = self.auto_rebuild_tasks.lock().await;
            tasks.remove(corpus_id);
            return;
        }
        tracing::info!(
            corpus_id = %corpus_id,
            "auto_rebuild:firing"
        );
        match self.rebuild_enrichment(corpus_id).await {
            Ok(job_id) => {
                tracing::info!(
                    corpus_id = %corpus_id,
                    job_id = %job_id,
                    "auto_rebuild:dispatched"
                );
            }
            Err(e) => {
                tracing::warn!(
                    corpus_id = %corpus_id,
                    error = %e,
                    "auto_rebuild:failed — user can retry via manual rebuild"
                );
            }
        }
        let mut tasks = self.auto_rebuild_tasks.lock().await;
        tasks.remove(corpus_id);
    }

    /// Snapshot of the live enrichment progress for one corpus.
    /// `None` = no live status (the build is finished, not started,
    /// or the corpus's enrichment is disabled). The HTTP
    /// `/details` route consults this so progress renders without
    /// polling the on-disk state file every tick.
    pub fn enrichment_progress(
        &self,
        corpus_id: &str,
    ) -> Option<super::watched::state::EnrichmentRuntimeStatus> {
        self.enrichment_progress
            .read()
            .ok()?
            .get(corpus_id)
            .cloned()
    }

    /// Folder-ingest v1 §3.3: enable atlas enrichment on a watched
    /// corpus, kicking off a background subprocess build through
    /// the existing `sovereign-cli enrich build` orchestrator.
    /// Returns the assigned `job_id` once the build is queued.
    ///
    /// Errors when:
    /// - The corpus isn't a watched-folder corpus.
    /// - `pipeline_id` isn't one of the known atlas pipelines
    ///   (`philosophy_atlas`, `referential_atlas`, `literary_atlas`).
    /// - A build is already in flight for this corpus.
    /// - Enrichment defaults haven't been installed (daemon boot
    ///   incomplete).
    /// - The synthesised `EnrichConfig` can't be persisted.
    pub async fn enable_enrichment(&self, corpus_id: &str, pipeline_id: &str) -> Result<String> {
        // Validate the pipeline_id matches one of the atlas
        // pipelines the registry recognises. Reject `literary` (the
        // legacy non-atlas variant) explicitly — `enrich build`
        // would error at start with the same message, but we want
        // the failure mode to land at request time so the UI can
        // surface a clean error before the build queue is
        // consumed.
        const ATLAS_PIPELINES: &[&str] =
            &["philosophy_atlas", "referential_atlas", "literary_atlas"];
        if !ATLAS_PIPELINES.contains(&pipeline_id) {
            return Err(Error::Execution(format!(
                "pipeline '{pipeline_id}' is not a recognised atlas pipeline; \
                 valid choices: philosophy_atlas, referential_atlas, literary_atlas"
            )));
        }

        let cfg = self.require_watched(corpus_id).await?;
        let source_path = cfg.root_path.clone();

        // Mutate the in-memory + persisted config to record the
        // user's choice. The watched-folder side stamps the
        // pipeline_id so a subsequent `Rebuild` doesn't have to
        // re-prompt.
        {
            let mut corpora = self.corpora.write().await;
            let entry = corpora
                .get_mut(corpus_id)
                .ok_or_else(|| Error::Execution(format!("corpus '{corpus_id}' not registered")))?;
            if let super::config::LocalCorpusSourceType::WatchedFolder(w) = &mut entry.source_type {
                w.enrichment = super::config::WatchedEnrichmentConfig::On {
                    pipeline_id: pipeline_id.to_string(),
                    last_built_at_unix: 0,
                    last_built_doc_count: 0,
                };
            }
            persist_config(&config_dir(&self.data_dir), entry)?;
        }

        // Stamp the source type's display metadata on the index's
        // _corpus_meta.json so the runtime retrieval gates
        // (`is_tiered_category` in conv_briefing) route this corpus
        // through the tiered (RAPTOR + chunk_entities + PPR) path.
        // The category comes from the `display_meta()` SSOT — before
        // 2026-06-10 this hardcoded "watched_folder", mislabeling
        // Obsidian vaults (same tiered family, so PPR worked, but the
        // briefing label and Atlas rail grouping read this string).
        // Best-effort: a stamp failure logs + continues — the build
        // still produces useful enrichment data and the user can
        // re-trigger the stamp later by re-enabling.
        let source_display = {
            let corpora = self.corpora.read().await;
            corpora
                .get(corpus_id)
                .and_then(|c| c.source_type.display_meta())
        };
        if let Some(display) = source_display {
            match self.engine.open_index_for_corpus(corpus_id).await {
                Ok(index) => {
                    let category = display.category.clone();
                    if let Err(e) = index.set_display(Some(display)) {
                        tracing::warn!(
                            corpus_id = %corpus_id,
                            category = category.as_deref().unwrap_or(""),
                            "enable_enrichment: set_display failed: {e}"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        corpus_id = %corpus_id,
                        "enable_enrichment: open_index_for_corpus failed; \
                         display category not stamped: {e}"
                    );
                }
            }
        }

        // Stamp Building into both the live progress map and the
        // on-disk state mirror so the UI immediately shows the
        // running state. The progress callback below will keep
        // both updated as events arrive.
        let started_at_unix = now_unix();
        self.set_enrichment_runtime_status(
            corpus_id,
            super::watched::state::EnrichmentRuntimeStatus::Building {
                phase: "starting".into(),
                current: 0,
                total: 0,
                started_at_unix,
            },
        )?;

        // Tiered fork: when the in-process tiered driver is wired,
        // route through it instead of the subprocess. The legacy
        // path stays as a fallback for daemons without tiered deps
        // installed (e.g. the gliner-ner feature off, or
        // FolderTieredProvider not constructed). pipeline_id is
        // accepted by the validator above but unused in the tiered
        // path — tiered enrichment is universal across recipe
        // variants.
        if self.enrichment_driver.is_tiered_ready().await {
            let progress_map = Arc::clone(&self.enrichment_progress);
            let corpus_id_for_cb = corpus_id.to_string();
            let state_dir = self.engine_index_dir().join(corpus_id);
            let on_state: Arc<dyn Fn(sovereign_core::types::AssetState) + Send + Sync> =
                Arc::new(move |state| {
                    use super::watched::state::EnrichmentRuntimeStatus;
                    use sovereign_core::types::AssetState;
                    // Tiered variant carries the AssetState directly
                    // so the UI renders the same T1 / T2 / T3
                    // milestones as attached-doc ingest. Ready stamps
                    // `built_at_unix`; Failed bubbles through Failed
                    // variant separately so callers that key off
                    // "did this fail?" stay simple.
                    let status = match &state {
                        AssetState::Failed { reason } => EnrichmentRuntimeStatus::Failed {
                            failed_at_unix: now_unix(),
                            reason: reason.clone(),
                        },
                        AssetState::Ready => EnrichmentRuntimeStatus::Tiered {
                            state: state.clone(),
                            started_at_unix,
                            built_at_unix: Some(now_unix()),
                            doc_count: 0,
                        },
                        _ => EnrichmentRuntimeStatus::Tiered {
                            state: state.clone(),
                            started_at_unix,
                            built_at_unix: None,
                            doc_count: 0,
                        },
                    };
                    if let Ok(mut guard) = progress_map.write() {
                        guard.insert(corpus_id_for_cb.clone(), status.clone());
                    }
                    // Best-effort persist to the state.json mirror so
                    // a UI fetch after the daemon restarts mid-build
                    // can read the last known phase.
                    let state_dir = state_dir.clone();
                    let corpus_id = corpus_id_for_cb.clone();
                    tokio::spawn(async move {
                        use super::watched::state::WatchedFolderState;
                        match WatchedFolderState::load(&state_dir) {
                            Ok(Some(mut s)) => {
                                s.enrichment_status = status;
                                s.last_updated_unix = now_unix();
                                if let Err(e) = s.save(&state_dir) {
                                    tracing::warn!(
                                        corpus_id = %corpus_id,
                                        "tiered_state_persist: save failed: {e}"
                                    );
                                }
                            }
                            Ok(None) => {
                                tracing::warn!(
                                    corpus_id = %corpus_id,
                                    "tiered_state_persist: state file missing — skipping persist"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    corpus_id = %corpus_id,
                                    "tiered_state_persist: load failed: {e}"
                                );
                            }
                        }
                    });
                });

            // Lance index lives at `<engine_index_dir>/<corpus_id>`.
            let index_path = self.engine_index_dir().join(corpus_id);
            return self
                .enrichment_driver
                .start_tiered_build(corpus_id, &index_path, on_state)
                .await;
        }

        // Legacy subprocess path. Stays in place until the tiered
        // path has been validated across watched-folder + obsidian-
        // vault corpora end-to-end.
        let progress_map = Arc::clone(&self.enrichment_progress);
        let corpus_id_for_cb = corpus_id.to_string();
        let progress_cb: crate::enrich::EnrichProgressFn = Arc::new(move |evt| {
            // Project EnrichProgress onto our runtime mirror.
            // Only Building→Building transitions go here; the
            // terminal Complete / Aborted events flow through a
            // separate watcher path inside the manager (added
            // alongside the `forget` call below).
            if let Some(status) = project_enrich_progress(&evt, started_at_unix) {
                if let Ok(mut guard) = progress_map.write() {
                    guard.insert(corpus_id_for_cb.clone(), status);
                }
            }
        });

        let job_id = self
            .enrichment_driver
            .start_build(corpus_id, &source_path, pipeline_id, progress_cb)
            .await?;
        Ok(job_id)
    }

    /// Folder-ingest v1 §3.3: disable enrichment on a watched
    /// corpus. Cancels any in-flight build, tears down the atlas
    /// directory cleanly via `corpus_engine::atlas_teardown`, and
    /// resets the config + state to `Off`.
    pub async fn disable_enrichment(&self, corpus_id: &str) -> Result<()> {
        let _ = self.require_watched(corpus_id).await?;

        // Signal cancellation if there's a running build. The
        // subprocess will tear down on its next stdout poll.
        self.enrichment_driver.cancel(corpus_id).await;
        let _ = self.enrichment_driver.forget(corpus_id).await;

        // Atlas teardown — atomic rename + remove of the
        // `~/.sovereign/indexes/<corpus>/atlas/` directory. Idempotent
        // on missing dirs, so disable-after-failed-build is safe.
        let index_dir = self.engine_index_dir();
        if let Err(e) = corpus_engine::atlas_teardown(&index_dir, corpus_id) {
            tracing::warn!(
                corpus_id = %corpus_id,
                "disable_enrichment: atlas_teardown failed: {e}"
            );
        }

        // Tiered teardown — purge the SQLite-side rows the in-process
        // tiered driver wrote: conv_raptor_nodes, conv_motifs,
        // conv_skeletons, chunk_entities, chunk_entity_progress. The
        // legacy subprocess path doesn't write these (it writes to the
        // atlas dir torn down above) so this is a no-op for legacy-
        // only corpora. Best-effort: a failure here leaves stale rows
        // that the next enable will overwrite, never a correctness
        // issue.
        let db_path = self.data_dir.join("sovereign.db");
        match sovereign_store::sqlite::SqliteStateStore::open(&db_path) {
            Ok(store) => {
                if let Err(e) = store.delete_tiered_for_corpus(corpus_id).await {
                    tracing::warn!(
                        corpus_id = %corpus_id,
                        "disable_enrichment: tiered teardown failed: {e}"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    db_path = %db_path.display(),
                    corpus_id = %corpus_id,
                    "disable_enrichment: cannot open state store for tiered teardown: {e}"
                );
            }
        }

        // Persist Off on the config side.
        {
            let mut corpora = self.corpora.write().await;
            if let Some(entry) = corpora.get_mut(corpus_id) {
                if let super::config::LocalCorpusSourceType::WatchedFolder(w) =
                    &mut entry.source_type
                {
                    w.enrichment = super::config::WatchedEnrichmentConfig::Off;
                }
                persist_config(&config_dir(&self.data_dir), entry)?;
            }
        }

        // Clear the live progress mirror + state file.
        if let Ok(mut guard) = self.enrichment_progress.write() {
            guard.remove(corpus_id);
        }
        self.set_enrichment_runtime_status(
            corpus_id,
            super::watched::state::EnrichmentRuntimeStatus::Off,
        )?;
        Ok(())
    }

    /// Folder-ingest v1 §3.3: rebuild the atlas using the
    /// previously-configured pipeline. Errors when the corpus
    /// isn't currently in `On` state — the user must enable
    /// first to pick a pipeline.
    pub async fn rebuild_enrichment(&self, corpus_id: &str) -> Result<String> {
        let cfg = self.require_watched(corpus_id).await?;
        let pipeline_id = match cfg
            .source_type
            .watched_config()
            .map(|w| w.enrichment.clone())
        {
            Some(super::config::WatchedEnrichmentConfig::On { pipeline_id, .. }) => pipeline_id,
            _ => {
                return Err(Error::Execution(
                    "rebuild_enrichment: corpus has no enrichment configured; \
                     call enable_enrichment first to pick a pipeline"
                        .into(),
                ));
            }
        };
        // Same path as enable: the atlas dir is overwritten in
        // place by the orchestrator's writer, so we don't tear it
        // down first. (If the user wants a clean slate, they can
        // disable then enable — same effect, more steps.)
        self.enable_enrichment(corpus_id, &pipeline_id).await
    }

    fn set_enrichment_runtime_status(
        &self,
        corpus_id: &str,
        status: super::watched::state::EnrichmentRuntimeStatus,
    ) -> Result<()> {
        use super::watched::state::WatchedFolderState;
        let state_dir = self.engine_index_dir().join(corpus_id);
        let mut state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));
        state.enrichment_status = status;
        state.last_updated_unix = now_unix();
        state.save(&state_dir)?;
        Ok(())
    }

    /// Whether an OCR context is installed. The desktop's
    /// `lc_ocr_available` command surfaces this to the UI so the
    /// "Read them with OCR" button only renders when we can actually
    /// honour the click.
    pub async fn ocr_available(&self) -> bool {
        self.ocr_ctx.read().await.is_some()
    }

    /// Clone the installed OCR context (if any). The watched-folder
    /// worker calls this once per sweep so a per-file OCR fallback
    /// can run when `cfg.ocr_pdfs` is true. Returns `None` when no
    /// `OcrCtx` has been installed — a watched corpus with
    /// `with_ocr: true` configured but no runtime context surfaces
    /// scanned PDFs as `failed_files` instead of OCR'ing.
    pub async fn ocr_ctx_clone(&self) -> Option<OcrCtx> {
        self.ocr_ctx.read().await.clone()
    }

    /// The default snapshot root Obsidian write-back should use when a
    /// new vault is registered. `obsidian_vault(path, snapshot_root)`
    /// in config.rs is the canonical factory to call.
    pub fn snapshot_root(&self) -> &Path {
        &self.snapshot_root
    }

    /// Persist a new corpus config. Idempotent: registering the same
    /// ID overwrites (re-canonicalising the root path, for example).
    pub async fn register(&self, mut config: LocalCorpusConfig) -> Result<String> {
        // Path identity guard: a folder already registered under a
        // DIFFERENT id (e.g. a pre-2026-06-11 `<kind>-<hex>` id, from
        // before ids gained readable slugs) keeps its original id —
        // the id is the citation handle and the key of every sidecar;
        // minting a second id for the same path would orphan the
        // existing corpus and double-ingest the folder.
        if let Some(existing_id) = self
            .corpora
            .read()
            .await
            .values()
            .find(|c| c.root_path == config.root_path && c.id != config.id)
            .map(|c| c.id.clone())
        {
            tracing::info!(
                path = %config.root_path.display(),
                minted = %config.id,
                existing = %existing_id,
                "register: path already registered — reusing existing corpus id"
            );
            config.id = existing_id;
        }
        let id = config.id.clone();
        persist_config(&config_dir(&self.data_dir), &config)?;
        self.corpora.write().await.insert(id.clone(), config);
        Ok(id)
    }

    /// Return a snapshot of all registered corpora.
    pub async fn list(&self) -> Vec<LocalCorpusConfig> {
        self.corpora.read().await.values().cloned().collect()
    }

    /// Signal a running ingest to stop cooperatively. Returns `true`
    /// when a flag was found and flipped; `false` when no ingest is
    /// registered for this corpus (never started, or already
    /// finished). Caller should not await this — `CorpusEngine`'s
    /// ingest loop polls the flag at checkpoint boundaries.
    pub fn cancel(&self, corpus_id: &str) -> bool {
        self.engine.cancel_corpus_ingest(corpus_id)
    }

    /// Get one config by id, returning `None` if unknown.
    pub async fn get(&self, id: &str) -> Option<LocalCorpusConfig> {
        self.corpora.read().await.get(id).cloned()
    }

    /// Drop a corpus: removes the engine index, deletes persisted
    /// config, deletes the corresponding row in StateStore (if any).
    /// Idempotent.
    ///
    /// Honors the contract documented on
    /// `CorpusEngine::remove_corpus_everything`: an in-flight ingest is
    /// cancelled and awaited BEFORE the wipe. Ordering:
    ///
    /// 1. Unlist the config first, so an ingest that hasn't reached its
    ///    registration check yet (e.g. the detached initial-ingest task
    ///    the watched-folder register route spawns) fails with NotFound
    ///    instead of recreating the index after the wipe.
    /// 2. Fire cancellation and poll both in-flight signals — the
    ///    manager-level [`IngestTicket`] (covers staging, which runs
    ///    before the engine pipeline registers its flag) and the
    ///    engine's cancellation registry (covers pipeline work from any
    ///    entry point, e.g. sweep workers). Cancel is re-fired each
    ///    poll because the engine registers its flag only once the
    ///    pipeline starts.
    /// 3. Wipe.
    pub async fn remove(&self, id: &str) -> Result<()> {
        self.corpora.write().await.remove(id);

        const REMOVE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
        let deadline = tokio::time::Instant::now() + REMOVE_WAIT;
        loop {
            let manager_busy = self
                .active_ingests
                .read()
                .expect("active_ingests poisoned")
                .contains_key(id);
            let engine_busy = self.engine.cancel_registry().get(id).is_some();
            if !manager_busy && !engine_busy {
                break;
            }
            self.engine.cancel_corpus_ingest(id);
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Execution(format!(
                    "remove {id}: in-flight ingest did not stop within \
                     {REMOVE_WAIT:?} after cancellation — retry the remove"
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        self.engine
            .remove_corpus_everything(id)
            .map_err(|e| Error::Execution(format!("remove index: {e}")))?;
        let _ = self.store.delete_corpus_state(id).await; // best-effort
        let path = config_file(&self.data_dir, id);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| Error::Execution(format!("remove config: {e}")))?;
        }
        Ok(())
    }

    /// Run a pre-scan for the corpus with the given id. Blocking — the
    /// PDF classifier is CPU-bound, so we hop onto `spawn_blocking`
    /// internally.
    pub async fn pre_scan(
        &self,
        id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<PreScanResult> {
        let config = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;

        let progress = progress.unwrap_or_else(noop_progress);
        tokio::task::spawn_blocking(move || {
            let scanner = PreScanner::new(&config);
            scanner.run_blocking(|done, total| {
                progress(LocalCorpusProgress::Scanning { done, total });
            })
        })
        .await
        .map_err(|e| Error::Execution(format!("pre_scan task: {e}")))
    }

    /// Ingest the given corpus.
    ///
    /// Flow:
    ///   1. Pre-scan to find readable files (respecting the caller's
    ///      most recent decisions about scanned/protected PDFs).
    ///   2. Stage the text of each readable file into a JSONL file.
    ///   3. (Optional) When `with_ocr` is requested, append OCR'd
    ///      content for scanned PDFs to the same JSONL using the
    ///      installed `OcrCtx`. Page-level progress is bridged onto
    ///      the same `progress` channel via `OcrPage` events.
    ///   4. Render a Recipe TOML for `corpus-engine` with the staged
    ///      JSONL as the source.
    ///   5. Delegate to `CorpusEngine::ingest`, bridging its
    ///      `IngestProgress` into our `LocalCorpusProgress::Ingesting`.
    ///   6. Return `IngestStats` — per-file runtime failures preserved.
    ///
    /// `with_ocr` is the one-shot user decision from the desktop's
    /// pre-scan panel. `Some(true)` flips `config.ocr_pdfs` to true
    /// and persists; `Some(false)` flips it off; `None` leaves the
    /// stored flag alone (matches the CLI default and preserves the
    /// per-corpus opt-in across re-ingest).
    pub async fn ingest(
        &self,
        id: &str,
        with_ocr: Option<bool>,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestStats> {
        // Ticket BEFORE the registration check: `remove()` unlists the
        // config and then awaits ticket absence, so an ingest must
        // never hold a config without holding a ticket.
        let _ticket = IngestTicket::acquire(&self.active_ingests, id);
        let mut config = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;

        if let Some(flag) = with_ocr {
            if config.ocr_pdfs != flag {
                config.ocr_pdfs = flag;
                // Persist the updated flag so subsequent ingests pick
                // up the same decision without re-prompting.
                persist_config(&config_dir(&self.data_dir), &config)?;
                self.corpora
                    .write()
                    .await
                    .insert(config.id.clone(), config.clone());
            }
        }

        let progress = progress.unwrap_or_else(noop_progress);

        // 1. Pre-scan.
        let scan = {
            let scan_cb = progress.clone();
            let cfg = config.clone();
            tokio::task::spawn_blocking(move || {
                PreScanner::new(&cfg).run_blocking(|done, total| {
                    scan_cb(LocalCorpusProgress::Scanning { done, total });
                })
            })
            .await
            .map_err(|e| Error::Execution(format!("pre_scan task: {e}")))?
        };
        if scan.readable.is_empty() {
            // Nothing to index. Emit a Complete event with an empty
            // IngestStats so the UI can react, and return.
            let stats = IngestStats {
                corpus_id: id.into(),
                files_indexed: 0,
                chunks_written: 0,
                runtime_failures: Vec::new(),
                excerpt_chunks: Vec::new(),
                duration_secs: 0,
            };
            progress(LocalCorpusProgress::Complete {
                result: crate::local_corpus::progress::CompletionResult::Ingest(stats.clone()),
            });
            return Ok(stats);
        }

        // 2. Stage to JSONL.
        let staging = default_staging_path(&self.data_dir, &config.id);
        let (stage_result, readable_files) = {
            let stage_cb = progress.clone();
            let cfg = config.clone();
            let readable = scan.readable.clone();
            let staging_for_task = staging.clone();
            tokio::task::spawn_blocking(move || {
                let res = extract_stage::stage_blocking(
                    &cfg,
                    &readable,
                    &staging_for_task,
                    |done, total, current| {
                        stage_cb(LocalCorpusProgress::Staging {
                            done,
                            total,
                            current_file: current.to_string(),
                        });
                    },
                );
                (res, readable)
            })
            .await
            .map_err(|e| Error::Execution(format!("stage task: {e}")))?
        };
        let mut stage_result =
            stage_result.map_err(|e| Error::Execution(format!("stage io: {e}")))?;

        // Optional OCR pass: append extracted text from scanned PDFs
        // to the same JSONL. Failures from this pass merge into the
        // existing runtime_failures vector so the completion screen
        // shows them alongside born-digital extraction failures.
        if config.ocr_pdfs && !scan.scanned_pdfs.is_empty() {
            let ocr_ctx = self.ocr_ctx.read().await.clone();
            match ocr_ctx {
                Some(ctx) => {
                    let page_cb: PageProgressCallback = {
                        let progress = progress.clone();
                        Arc::new(move |pp: PageProgress| {
                            progress(LocalCorpusProgress::OcrPage {
                                file: pp.file_display_name,
                                page: pp.current_page,
                                total_pages: pp.total_pages,
                                file_idx: pp.file_idx,
                                file_total: pp.file_total,
                            });
                        })
                    };
                    let ocr_result = extract_stage::append_ocr_to_staging(
                        &config,
                        &scan.scanned_pdfs,
                        &staging,
                        &ctx,
                        Some(page_cb),
                    )
                    .await
                    .map_err(|e| Error::Execution(format!("ocr append io: {e}")))?;
                    stage_result.staged += ocr_result.staged;
                    stage_result.failures.extend(ocr_result.failures);
                }
                None => {
                    // Caller asked for OCR but the runtime context
                    // isn't installed (CLI without sidecar bundle, or
                    // misconfigured desktop). Surface every scanned
                    // PDF as a runtime failure so the user sees them
                    // named individually on the completion screen.
                    for meta in &scan.scanned_pdfs {
                        stage_result.failures.push(RuntimeFailure {
                            file: meta.clone(),
                            reason: "OCR requested but engine is not installed".into(),
                        });
                    }
                }
            }
        }

        if stage_result.staged == 0 {
            // Everything failed during extraction. Report to the UI
            // and return — no point invoking the engine.
            let stats = IngestStats {
                corpus_id: id.into(),
                files_indexed: 0,
                chunks_written: 0,
                runtime_failures: stage_result.failures,
                excerpt_chunks: Vec::new(),
                duration_secs: 0,
            };
            progress(LocalCorpusProgress::Complete {
                result: crate::local_corpus::progress::CompletionResult::Ingest(stats.clone()),
            });
            return Ok(stats);
        }

        // 3. Write recipe TOML to a temp file. Output goes into
        // `self.recipes_dir`, which the daemon configures to match
        // `CorpusEngine`'s `overrides_dir` — without that alignment
        // the sweep's first `apply_update` call errors with `No
        // registry entry for corpus '<id>'`.
        let recipe = recipe_toml(&config, &staging);
        let recipe_path = recipe_path_for(&self.recipes_dir, &config.id);
        if let Some(parent) = recipe_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Execution(format!("create recipe dir: {e}")))?;
        }
        std::fs::write(&recipe_path, &recipe)
            .map_err(|e| Error::Execution(format!("write recipe: {e}")))?;

        // 4. Delegate to engine.
        let engine = Arc::clone(&self.engine);
        let started = std::time::Instant::now();
        let ingest_cb: Option<corpus_engine::ProgressCallback> = Some({
            let progress = progress.clone();
            Box::new(move |p| {
                progress(ingest_progress_to_local(p));
            })
        });
        let ingest_result = engine
            .ingest(&CorpusSpec::RecipePath(recipe_path.clone()), ingest_cb)
            .await
            .map_err(|e| Error::Execution(format!("engine ingest: {e}")))?;

        // 5. Pick excerpts for the completion screen. Spec §5.4:
        //    search the freshly-built index for a generic seed query,
        //    pick the 3 best-fitting chunks (length + source diversity).
        //    Failure here is non-fatal — we still return IngestStats
        //    without excerpts rather than failing the whole ingest.
        let excerpt_chunks = match self
            .search(&ingest_result.corpus_id, super::excerpt::SEED_QUERY, 20)
            .await
        {
            Ok(candidates) => super::excerpt::select_excerpts(&candidates),
            Err(e) => {
                tracing::warn!(
                    corpus_id = %ingest_result.corpus_id,
                    "excerpt selection failed after successful ingest: {e}"
                );
                Vec::new()
            }
        };

        // 6. Compose IngestStats. `stage_result.staged` covers both
        // born-digital files and any OCR-recovered scanned PDFs.
        let _ = readable_files; // retained for future per-source stats
        let stats = IngestStats {
            corpus_id: ingest_result.corpus_id.clone(),
            files_indexed: stage_result.staged,
            chunks_written: ingest_result.chunks_created,
            runtime_failures: stage_result.failures,
            excerpt_chunks,
            duration_secs: started.elapsed().as_secs(),
        };
        progress(LocalCorpusProgress::Complete {
            result: crate::local_corpus::progress::CompletionResult::Ingest(stats.clone()),
        });
        Ok(stats)
    }

    /// Run the clustering + labelling pipeline on an ingested vault.
    /// Returns the structured result; does NOT persist yet (M5 writes).
    /// Caller passes a progress callback to bridge into the UI.
    pub async fn cluster(
        &self,
        id: &str,
        config: &super::clusterer::ClusterConfig,
        on_progress: ProgressCallback,
    ) -> Result<super::clusterer::LabeledClusterResult> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        if cfg.enrichment.is_none() {
            return Err(Error::Execution(format!(
                "corpus '{id}' does not support clustering \
                 (enrichment config missing — was it registered as a folder?)"
            )));
        }
        let inference = self.inference.as_ref().cloned().ok_or_else(|| {
            Error::Execution(
                "clustering requires an inference provider; none is configured".to_string(),
            )
        })?;
        let inference_fn = crate::corpus::inference_to_inference_fn(inference);
        let clusterer = super::clusterer::Clusterer::new(Arc::clone(&self.engine), inference_fn);
        let result = clusterer.run(id, config, on_progress).await?;
        // Cache for subsequent `get_preview` calls so the UI doesn't
        // have to hand the whole result blob back through Tauri.
        self.cluster_results
            .write()
            .await
            .insert(id.to_string(), result.clone());
        Ok(result)
    }

    /// Seed the cluster-result cache directly, bypassing the
    /// inference-backed `cluster()` pipeline. Test/bench seam (the
    /// live-sync e2e exercises the real write-back path with a
    /// hand-built cluster over real ingested chunk ids); also the
    /// hook a future "restore persisted cluster result" feature
    /// would use. Mirrors `set_auto_rebuild_debounce`'s pattern of
    /// public test-visible knobs.
    pub async fn seed_cluster_result(
        &self,
        id: &str,
        result: super::clusterer::LabeledClusterResult,
    ) {
        self.cluster_results
            .write()
            .await
            .insert(id.to_string(), result);
    }

    /// Build the UI-facing `VaultPreview` from the most recent
    /// cached clustering result. Returns `NotFound` if the user
    /// hasn't run `cluster` yet.
    pub async fn get_preview(
        &self,
        id: &str,
        config: &super::clusterer::ClusterConfig,
    ) -> Result<super::preview::VaultPreview> {
        let cache = self.cluster_results.read().await;
        let result = cache.get(id).ok_or_else(|| {
            Error::NotFound(format!(
                "no clustering run on record for '{id}' — run lc_cluster first"
            ))
        })?;
        super::preview::build_preview(Arc::clone(&self.engine), id, config, result).await
    }

    // ─── M5: Write-back, snapshots, rollback, clean ─────────────

    /// Detect whether the vault is a git repository. Returns `None`
    /// when the vault isn't a repo OR when `git` isn't installed.
    pub async fn check_git(&self, id: &str) -> Result<Option<super::git::GitStatus>> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        Ok(super::git::check_git_repo(&cfg.root_path))
    }

    /// Best-effort writeback refresh used by the live-sync
    /// reconciliation worker. Returns `Ok(None)` when there is no
    /// cached cluster result for the corpus — the user hasn't built a
    /// Map of Content yet, so there are no tags to refresh. Returns
    /// `Ok(Some(result))` when writeback executed against the cached
    /// preview.
    ///
    /// Idempotent against unchanged input: `WriteBack::execute` skips
    /// the per-note write when the merged frontmatter is byte-identical
    /// to disk (post Phase A2), so calling this every sweep with no
    /// new cluster work doesn't churn mtimes.
    ///
    /// Caller (the worker) is responsible for:
    ///   - Debouncing (e.g., 5-minute window via
    ///     `WatchedFolderState.last_writeback_unix`).
    ///   - Patching `WatchedFolderState.entries` with the returned
    ///     `WriteBackResult.touched_user_notes` so the very-next walker
    ///     sweep's fast-path doesn't re-detect this writeback's mtime
    ///     bumps as user edits.
    pub async fn refresh_writeback_if_clustered(
        &self,
        id: &str,
    ) -> Result<Option<super::writeback::WriteBackResult>> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        let Some(wb_cfg) = cfg.write_back.clone() else {
            return Ok(None);
        };
        // No cached cluster → no preview to write back → benign skip.
        // The caller treats this as "writeback not applicable yet,"
        // not a failure.
        let has_clusters = self.cluster_results.read().await.contains_key(id);
        if !has_clusters {
            return Ok(None);
        }
        let cluster_cfg = super::clusterer::ClusterConfig::default();
        let preview = self.get_preview(id, &cluster_cfg).await?;
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path.clone(), cfg.id.clone());
        let version = (wb.list_snapshots().map(|s| s.len()).unwrap_or(0) as u32) + 1;
        let result = wb.execute(&preview, version, None).await?;
        Ok(Some(result))
    }

    /// Write tags (and optional index notes) for a previously-computed
    /// preview. Takes a snapshot first; rolls back nothing on per-file
    /// failure (the user can trigger rollback explicitly from the UI).
    pub async fn write_tags(
        &self,
        id: &str,
        git_commit: bool,
    ) -> Result<super::writeback::WriteBackResult> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!(
                "corpus '{id}' is not configured for write-back (is it a vault?)"
            ))
        })?;
        let cluster_cfg = super::clusterer::ClusterConfig::default();
        let preview = self.get_preview(id, &cluster_cfg).await?;
        // Version monotonically increments per successful write. We
        // derive from existing snapshot count so the number reflects
        // "nth Sovereign pass on this vault".
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path.clone(), cfg.id.clone());
        let version = (wb.list_snapshots().map(|s| s.len()).unwrap_or(0) as u32) + 1;

        let git_hash = if git_commit {
            match super::git::git_commit_before_write(
                &cfg.root_path,
                &format!("Pre-Sovereign-tag snapshot (version {version})"),
            ) {
                Ok(sha) => Some(sha),
                Err(e) => {
                    tracing::warn!("pre-write git commit failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        wb.execute(&preview, version, git_hash).await
    }

    /// List every persisted snapshot for a vault, newest first.
    pub async fn list_snapshots(&self, id: &str) -> Result<Vec<super::writeback::SnapshotMeta>> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!("corpus '{id}' is not configured for write-back"))
        })?;
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path, cfg.id);
        wb.list_snapshots()
    }

    /// Restore the vault to the state captured in the given snapshot.
    pub async fn rollback(
        &self,
        id: &str,
        snapshot_path: &std::path::Path,
    ) -> Result<super::writeback::RollbackResult> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!("corpus '{id}' is not configured for write-back"))
        })?;
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path, cfg.id);
        let snapshot = wb.load_snapshot(snapshot_path)?;
        wb.rollback(&snapshot).await
    }

    /// Remove every sovereign/* tag and every sovereign_* key from
    /// every note in the vault; delete the generated index-note
    /// directory. Does NOT touch snapshots.
    pub async fn clean(&self, id: &str) -> Result<super::writeback::CleanResult> {
        let cfg = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!("corpus '{id}' is not configured for write-back"))
        })?;
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path, cfg.id);
        wb.clean().await
    }

    /// Search this corpus. Embeds `query` with the engine's model, opens
    /// the corpus index, and returns the top `limit` ScoredChunks.
    ///
    /// Unknown corpus id returns `NotFound`, not an empty result,
    /// because silent empties are a debugging nightmare.
    pub async fn search(&self, id: &str, query: &str, limit: usize) -> Result<Vec<ScoredChunk>> {
        if self.get(id).await.is_none() {
            return Err(Error::NotFound(format!(
                "local corpus '{id}' not registered"
            )));
        }
        let embedding = self
            .engine
            .embed(query)
            .await
            .map_err(|e| Error::Execution(format!("embed query: {e}")))?;
        let index = self
            .engine
            .open_index_for_corpus(id)
            .await
            .map_err(|e| Error::Execution(format!("open index '{id}': {e}")))?;
        index
            .search(&embedding, query, limit)
            .await
            .map_err(|e| Error::Execution(format!("search '{id}': {e}")))
    }

    /// Return any registered corpus whose source-file manifest shows
    /// non-`Complete` entries — surfaced on relaunch via the
    /// ResumePrompt.
    pub async fn incomplete_jobs(&self) -> Vec<IncompleteJob> {
        use corpus_engine::progress::SourceFileManifest;
        let mut out = Vec::new();
        for config in self.corpora.read().await.values() {
            let status = self.engine.corpus_disk_status(&config.id);
            // Paths the engine writes manifests to: canonical, or the
            // partition-of-self. `corpus_disk_status` doesn't expose
            // the manifest directly, so re-read from the expected
            // locations.
            for candidate in [
                self.engine_index_dir().join(&config.id),
                self.engine_index_dir()
                    .join(format!("{}-partition-local", config.id)),
            ] {
                let Ok(Some(manifest)) = SourceFileManifest::load(&candidate) else {
                    continue;
                };
                let total = manifest.files.len();
                let done = manifest
                    .files
                    .iter()
                    .filter(|f| {
                        matches!(
                            f.status,
                            corpus_engine::progress::SourceFileStatus::Complete { .. }
                        )
                    })
                    .count();
                if total > 0 && done < total {
                    out.push(IncompleteJob {
                        corpus_id: config.id.clone(),
                        display_name: config.display_name.clone(),
                        files_done: done,
                        files_total: total,
                    });
                }
                let _ = status.canonical_in_progress; // quiet "unused" until we surface.
                break; // One manifest per corpus is enough.
            }
        }
        out
    }

    /// Public accessor for the engine's per-corpus index directory
    /// root. The watched-folder worker needs this to locate its
    /// `_watched_folder_state.json` sidecar inside each corpus dir.
    pub fn index_dir_root(&self) -> PathBuf {
        self.engine_index_dir()
    }

    fn engine_index_dir(&self) -> PathBuf {
        // The engine does not expose its index_dir publicly yet, so we
        // derive it from data_dir by convention. `AppState` passes the
        // same data_dir both to CorpusEngine and to us.
        self.data_dir.join("indexes")
    }

    // ─── Watched-folder shims ────────────────────────────────────────

    /// Snapshot of every registered `WatchedFolder` corpus. Used by
    /// the daemon at startup (auto-resume) and by the
    /// `corpus watch-list` CLI.
    pub async fn list_watched(&self) -> Vec<LocalCorpusConfig> {
        self.corpora
            .read()
            .await
            .values()
            .filter(|c| c.source_type.is_watched())
            .cloned()
            .collect()
    }

    /// Snapshot of every corpus the reconciliation worker should
    /// sweep. Covers `WatchedFolder` *and* `ObsidianVault` — both
    /// surface user edits that must reflect into the index. Used by
    /// `watched_folder_setup::WatchedSubsystem::install` to seed the
    /// scheduler on daemon startup. `DocumentFolder` corpora are
    /// one-shot and excluded.
    pub async fn list_reconcilable(&self) -> Vec<LocalCorpusConfig> {
        self.corpora
            .read()
            .await
            .values()
            .filter(|c| c.source_type.should_reconcile())
            .cloned()
            .collect()
    }

    /// Transition a watched-folder corpus into `PausedManual`. The
    /// scheduler skips paused corpora on its next tick. Idempotent —
    /// already-paused corpora keep their original `since_unix`.
    pub async fn pause_watched(&self, corpus_id: &str, reason: String) -> Result<()> {
        use super::watched::state::WatchedFolderState;
        use super::watched::status::WatchedFolderStatus;
        let _cfg = self.require_watched(corpus_id).await?;
        let state_dir = self.engine_index_dir().join(corpus_id);
        let mut state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));
        if !matches!(state.status, WatchedFolderStatus::PausedManual { .. }) {
            state.status = WatchedFolderStatus::PausedManual {
                since_unix: now_unix(),
                reason,
            };
            state.last_updated_unix = now_unix();
            state.save(&state_dir)?;
        }
        Ok(())
    }

    /// Resume a paused watched-folder corpus. The next scheduler
    /// tick picks it up; we do not force an immediate sweep here so
    /// the cadence behaviour is uniform with normal operation.
    pub async fn resume_watched(&self, corpus_id: &str) -> Result<()> {
        use super::watched::state::WatchedFolderState;
        use super::watched::status::WatchedFolderStatus;
        let _cfg = self.require_watched(corpus_id).await?;
        let state_dir = self.engine_index_dir().join(corpus_id);
        let mut state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));
        if state.is_paused() || matches!(state.status, WatchedFolderStatus::Errored { .. }) {
            // Last sweep numbers no longer reflect reality after a
            // pause window — reset to a fresh Idle so the UI reflects
            // "we're starting from a clean state, sweep due imminently".
            state.status = WatchedFolderStatus::Idle {
                last_sweep_unix: 0,
                live_docs: state.entries.len(),
                tombstones: state.tombstones.len(),
            };
            state.last_updated_unix = now_unix();
            state.save(&state_dir)?;
        }
        Ok(())
    }

    /// Folder-ingest v1 §3.1: layer an additional root onto an
    /// existing watched-folder corpus. The path is canonicalised
    /// before persistence; duplicates (same canonical path already
    /// in `additional_roots` OR equal to the primary `root_path`)
    /// are rejected. Persists the updated config; the next
    /// scheduler tick walks the new root automatically.
    pub async fn add_watched_root(&self, corpus_id: &str, path: PathBuf) -> Result<()> {
        use super::config::{LocalCorpusSourceType, RootSpec};
        let canonical = std::fs::canonicalize(&path)
            .map_err(|e| Error::Execution(format!("canonicalize {}: {e}", path.display())))?;
        if !canonical.is_dir() {
            return Err(Error::Execution(format!(
                "additional root '{}' is not a directory",
                canonical.display()
            )));
        }
        let mut corpora = self.corpora.write().await;
        let cfg = corpora
            .get_mut(corpus_id)
            .ok_or_else(|| Error::Execution(format!("corpus '{corpus_id}' not registered")))?;
        if cfg.root_path == canonical {
            return Err(Error::Execution(
                "root matches the corpus's primary root_path".into(),
            ));
        }
        let watched = match &mut cfg.source_type {
            LocalCorpusSourceType::WatchedFolder(w) => w,
            _ => {
                return Err(Error::Execution(format!(
                    "corpus '{corpus_id}' is not a watched folder"
                )));
            }
        };
        if watched.additional_roots.iter().any(|r| r.path == canonical) {
            return Err(Error::Execution(
                "root already attached to this corpus".into(),
            ));
        }
        watched.additional_roots.push(RootSpec {
            path: canonical,
            added_at_unix: now_unix(),
        });
        let cfg_clone = cfg.clone();
        drop(corpora);
        // Persist outside the write lock so a slow disk write
        // doesn't block other manager operations on this corpus.
        persist_config(&config_dir(&self.data_dir), &cfg_clone)?;
        Ok(())
    }

    /// Folder-ingest v1 §3.1: detach an additional root by index.
    /// `idx` is 0-based into `WatchedFolderConfig.additional_roots`
    /// (i.e. the array position the UI displays — NOT the
    /// `source_root_index` that is `idx + 1`). Out-of-range
    /// indices return `Err`.
    ///
    /// The next sweep walks the surviving roots; entries whose
    /// `source_root_index` matched the removed root no longer
    /// surface in the snapshot, so the diff naturally classifies
    /// them as deletions and the existing tombstone semantics
    /// apply. The deletion guard still gates catastrophic removal
    /// — if the removed root contributed many docs, the user gets
    /// a `confirm-deletion` prompt before the chunks evaporate.
    pub async fn remove_watched_root(&self, corpus_id: &str, idx: usize) -> Result<()> {
        use super::config::LocalCorpusSourceType;
        let mut corpora = self.corpora.write().await;
        let cfg = corpora
            .get_mut(corpus_id)
            .ok_or_else(|| Error::Execution(format!("corpus '{corpus_id}' not registered")))?;
        let watched = match &mut cfg.source_type {
            LocalCorpusSourceType::WatchedFolder(w) => w,
            _ => {
                return Err(Error::Execution(format!(
                    "corpus '{corpus_id}' is not a watched folder"
                )));
            }
        };
        if idx >= watched.additional_roots.len() {
            return Err(Error::Execution(format!(
                "additional_roots index {idx} out of range (len = {})",
                watched.additional_roots.len()
            )));
        }
        watched.additional_roots.remove(idx);
        let cfg_clone = cfg.clone();
        drop(corpora);
        persist_config(&config_dir(&self.data_dir), &cfg_clone)?;
        Ok(())
    }

    /// Folder-ingest v1 §3.7: per-document inspection summary used
    /// by the desktop's document-inspector panel. Returns
    /// `(chunk_count, first_chunk_preview)` for a `doc_id` (the
    /// relative-path key from `WatchedFolderState.entries`). The
    /// preview is truncated to `preview_chars` so the wire payload
    /// stays small even on large corpora.
    ///
    /// `Ok((0, None))` is the right answer for files that exist
    /// in `state.entries` but haven't been chunked yet (initial
    /// sweep mid-flight) or for files that failed extraction.
    pub async fn watched_doc_summary(
        &self,
        corpus_id: &str,
        doc_id: &str,
        preview_chars: usize,
    ) -> Result<(usize, Option<String>)> {
        let _cfg = self.require_watched(corpus_id).await?;
        let info = self
            .engine
            .installed_indexes()
            .await
            .map_err(|e| Error::Execution(format!("installed_indexes: {e}")))?
            .into_iter()
            .find(|i| i.corpus_id == corpus_id)
            .ok_or_else(|| {
                Error::Execution(format!(
                    "corpus '{corpus_id}' has no LanceDB index yet — \
                     either the initial sweep hasn't run or it's mid-build"
                ))
            })?;
        let idx = self
            .engine
            .open_index(&info.path)
            .await
            .map_err(|e| Error::Execution(format!("open_index: {e}")))?;
        idx.doc_summary(doc_id, preview_chars)
            .await
            .map_err(|e| Error::Execution(format!("doc_summary: {e}")))
    }

    /// Mark a Manual-mode watched corpus as ready to sweep on the
    /// next tick by flipping `state.manual_sync_pending`. Caller
    /// (typically the HTTP `/sync-now/{id}` route) must also flip
    /// the in-memory mirror on `WatchedFolderRegistry` so the
    /// scheduler picks it up without waiting for state-file polling.
    /// Defence in depth: either layer alone keeps Manual cadence
    /// honest; flipping both prevents a daemon restart between the
    /// two writes from re-dispatching.
    ///
    /// Returns `Err` if the corpus isn't a watched-folder corpus or
    /// isn't in `SyncMode::Manual` — calling `/sync-now` on a
    /// Continuous corpus is a 409 at the HTTP layer because the
    /// scheduler ignores the flag in that mode and the request
    /// would silently no-op.
    pub async fn request_manual_sync(&self, corpus_id: &str) -> Result<()> {
        use super::config::SyncMode;
        use super::watched::state::WatchedFolderState;
        let cfg = self.require_watched(corpus_id).await?;
        let watched_cfg = match &cfg.source_type {
            super::config::LocalCorpusSourceType::WatchedFolder(w) => w,
            _ => unreachable!("require_watched returned a non-watched corpus"),
        };
        if watched_cfg.sync_mode != SyncMode::Manual {
            return Err(Error::Execution(format!(
                "corpus '{corpus_id}' is in Continuous sync mode; \
                 sync-now only applies to Manual-mode corpora"
            )));
        }
        let state_dir = self.engine_index_dir().join(corpus_id);
        let mut state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));
        state.manual_sync_pending = true;
        state.last_updated_unix = now_unix();
        state.save(&state_dir)?;
        Ok(())
    }

    /// Acknowledge a `PausedAwaitingConfirmation` state. Per plan Q3,
    /// this clears the pause flag; the next sweep re-walks fresh and
    /// applies whatever the current diff is — which is safer than
    /// replaying a stale diff (the user may have restored some files
    /// in the meantime).
    pub async fn confirm_pending_deletion(&self, corpus_id: &str) -> Result<()> {
        use super::watched::state::WatchedFolderState;
        use super::watched::status::WatchedFolderStatus;
        let _cfg = self.require_watched(corpus_id).await?;
        let state_dir = self.engine_index_dir().join(corpus_id);
        let mut state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));
        if matches!(
            state.status,
            WatchedFolderStatus::PausedAwaitingConfirmation { .. }
        ) {
            state.status = WatchedFolderStatus::Idle {
                last_sweep_unix: 0, // 0 makes the corpus due immediately on next tick
                live_docs: state.entries.len(),
                tombstones: state.tombstones.len(),
            };
            // One-shot bypass — the worker consumes this on the next
            // sweep so the guard doesn't immediately re-trip on the
            // same diff. Subsequent sweeps re-evaluate the guard
            // normally.
            state.bypass_guard_next_sweep = true;
            state.last_updated_unix = now_unix();
            state.save(&state_dir)?;
        } else {
            return Err(Error::Execution(format!(
                "corpus '{corpus_id}' is not in PausedAwaitingConfirmation state"
            )));
        }
        Ok(())
    }

    /// Read the current `WatchedFolderStatus` for a corpus. Returns a
    /// fresh `Idle { live_docs: 0 }` when the state file doesn't
    /// exist yet (first sweep hasn't run).
    pub async fn watched_status(
        &self,
        corpus_id: &str,
    ) -> Result<super::watched::status::WatchedFolderStatus> {
        use super::watched::state::WatchedFolderState;
        let _cfg = self.require_watched(corpus_id).await?;
        let state_dir = self.engine_index_dir().join(corpus_id);
        let state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));
        Ok(state.status)
    }

    /// Read the full `WatchedFolderState` for a corpus. The richer
    /// surface includes `skipped_by_extension` + `failed_files` for
    /// `corpus watch-status --skipped --failures`. Returns a fresh
    /// state when the file doesn't exist.
    pub async fn watched_state(
        &self,
        corpus_id: &str,
    ) -> Result<super::watched::state::WatchedFolderState> {
        use super::watched::state::WatchedFolderState;
        let _cfg = self.require_watched(corpus_id).await?;
        let state_dir = self.engine_index_dir().join(corpus_id);
        Ok(WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id)))
    }

    /// Watched-folder corpora that are NOT in `Idle` status (i.e.,
    /// the user should know about them). Used by the desktop's
    /// ResumePrompt at startup to surface guard-tripped, paused,
    /// errored, or mid-sweep corpora. Returns a snapshot — the next
    /// scheduler tick may resolve `Sweeping` entries on its own.
    pub async fn watched_incomplete_jobs(&self) -> Vec<WatchedIncompleteJob> {
        use super::watched::state::WatchedFolderState;
        use super::watched::status::WatchedFolderStatus;
        let mut out = Vec::new();
        for cfg in self.corpora.read().await.values() {
            if !cfg.source_type.is_watched() {
                continue;
            }
            let state_dir = self.engine_index_dir().join(&cfg.id);
            let state = match WatchedFolderState::load(&state_dir) {
                Ok(Some(s)) => s,
                _ => continue, // No state yet → nothing to surface.
            };
            if matches!(state.status, WatchedFolderStatus::Idle { .. }) {
                continue;
            }
            out.push(WatchedIncompleteJob {
                corpus_id: cfg.id.clone(),
                display_name: cfg.display_name.clone(),
                root_path: cfg.root_path.clone(),
                status: state.status,
                tombstones: state.tombstones.len(),
                failed_files: state.failed_files.len(),
            });
        }
        out
    }

    async fn require_watched(&self, corpus_id: &str) -> Result<LocalCorpusConfig> {
        let cfg = self
            .get(corpus_id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{corpus_id}' not registered")))?;
        if !cfg.source_type.is_watched() {
            return Err(Error::Execution(format!(
                "corpus '{corpus_id}' is not a watched folder"
            )));
        }
        Ok(cfg)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Progress bridge ─────────────────────────────────────────────────

fn ingest_progress_to_local(p: corpus_engine::progress::IngestProgress) -> LocalCorpusProgress {
    use corpus_engine::progress::IngestProgress::*;
    match p {
        Downloading {
            bytes_downloaded,
            bytes_total,
            ..
        } => LocalCorpusProgress::Ingesting {
            done: bytes_downloaded,
            total: bytes_total.unwrap_or(0),
            phase_label: "Downloading".into(),
            current_file: None,
        },
        Extracting {
            documents_processed,
        } => LocalCorpusProgress::Ingesting {
            done: documents_processed,
            total: 0,
            phase_label: "Reading your documents".into(),
            current_file: None,
        },
        Chunking { chunks_created } => LocalCorpusProgress::Ingesting {
            done: chunks_created,
            total: 0,
            phase_label: "Chunking".into(),
            current_file: None,
        },
        Embedding {
            chunks_embedded,
            total,
            ..
        } => LocalCorpusProgress::Ingesting {
            done: chunks_embedded,
            total,
            phase_label: "Building the index".into(),
            current_file: None,
        },
        Indexing {
            chunks_indexed,
            total,
        } => LocalCorpusProgress::Ingesting {
            done: chunks_indexed,
            total,
            phase_label: "Writing index".into(),
            current_file: None,
        },
        OptimizingIndex { current_chunks } => LocalCorpusProgress::Ingesting {
            done: current_chunks,
            total: current_chunks,
            phase_label: "Optimizing search index".into(),
            current_file: None,
        },
        Enriching {
            detail, fraction, ..
        } => {
            // Post-embed enrichment phases (entity extraction,
            // clustering, atlas build). `detail` carries the
            // human-readable phase label the daemon already emits to
            // stderr; we surface it verbatim so the UI's local-corpus
            // surface tells the same story as the daemon log.
            //
            // Done/total are pulled from the optional fraction when
            // the underlying phase reports one. Otherwise we set
            // total=0 so the UI falls back to spinner-mode rather
            // than rendering a 0% progress bar.
            let (done, total) = match fraction {
                Some(f) => ((f * 100.0).round() as u64, 100u64),
                None => (0, 0),
            };
            LocalCorpusProgress::Ingesting {
                done,
                total,
                phase_label: detail,
                current_file: None,
            }
        }
        Complete {
            total_chunks,
            duration_secs,
        } => LocalCorpusProgress::Ingesting {
            done: total_chunks,
            total: total_chunks,
            phase_label: format!("Done in {duration_secs}s"),
            current_file: None,
        },
    }
}

// ─── Persistence helpers ─────────────────────────────────────────────

fn config_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("local-corpora")
}

fn config_file(data_dir: &Path, corpus_id: &str) -> PathBuf {
    config_dir(data_dir).join(format!("{corpus_id}.json"))
}

/// Resolve the recipe-TOML path for a corpus inside the manager's
/// configured `recipes_dir`. The directory is the manager's
/// per-instance setting (see [`LocalCorpusManager::init_with_recipes_dir`])
/// — it is NOT derived from `data_dir` anymore so the daemon can
/// align it with the engine's `overrides_dir`.
fn recipe_path_for(recipes_dir: &Path, corpus_id: &str) -> PathBuf {
    recipes_dir.join(format!("{corpus_id}.toml"))
}

fn persist_config(dir: &Path, config: &LocalCorpusConfig) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Execution(format!("create dir: {e}")))?;
    let path = dir.join(format!("{}.json", config.id));
    let raw = serde_json::to_string_pretty(config)
        .map_err(|e| Error::Execution(format!("serialize config: {e}")))?;
    // Write to a temp file and rename so partial writes never leave a
    // corrupt sidecar behind. Uses `persist` from `tempfile`.
    let dir_owned = dir.to_path_buf();
    let temp = tempfile::NamedTempFile::new_in(&dir_owned)
        .map_err(|e| Error::Execution(format!("temp file: {e}")))?;
    std::fs::write(temp.path(), raw.as_bytes())
        .map_err(|e| Error::Execution(format!("write config: {e}")))?;
    temp.persist(&path)
        .map_err(|e| Error::Execution(format!("rename config: {e}")))?;
    Ok(())
}

fn load_persisted_configs(dir: &Path) -> Result<HashMap<String, LocalCorpusConfig>> {
    let mut out = HashMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(out), // fresh install — nothing persisted yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("skip unreadable local corpus sidecar {:?}: {e}", path);
                continue;
            }
        };
        match serde_json::from_str::<LocalCorpusConfig>(&raw) {
            Ok(cfg) => {
                out.insert(cfg.id.clone(), cfg);
            }
            Err(e) => {
                tracing::warn!("skip unparseable local corpus sidecar {:?}: {e}", path);
            }
        }
    }
    Ok(out)
}

fn noop_progress() -> ProgressCallback {
    Arc::new(|_| {})
}

/// Folder-ingest v1 §3.3: map an `EnrichProgress` event onto the
/// `EnrichmentRuntimeStatus` enum the watched-folder state file
/// stores. Returns `None` for events that don't change the
/// runtime status (e.g. step-level fine-grained events the UI
/// already shows via the chapter counter).
///
/// The mapping is deliberately coarse: the live status is what
/// the UI's progress bar reads, and a one-line "phase X of Y"
/// summary is enough. Operators who want every event subscribe to
/// the SSE channel directly.
fn project_enrich_progress(
    evt: &corpus_engine::enrichment::pipeline::EnrichProgress,
    started_at_unix: u64,
) -> Option<super::watched::state::EnrichmentRuntimeStatus> {
    use super::watched::state::EnrichmentRuntimeStatus;
    use corpus_engine::enrichment::pipeline::EnrichProgress as EP;
    match evt {
        // BuildStart fires once at the very top; we already
        // stamped Building when the build was queued, so this
        // is informational. Re-stamp anyway for resilience.
        EP::BuildStart { steps, .. } => Some(EnrichmentRuntimeStatus::Building {
            phase: "starting".into(),
            current: 0,
            total: steps.len(),
            started_at_unix,
        }),
        EP::StepStart {
            step,
            ordinal,
            total,
            ..
        } => Some(EnrichmentRuntimeStatus::Building {
            phase: format!("{step:?}"),
            current: *ordinal,
            total: *total,
            started_at_unix,
        }),
        EP::ChapterProgress {
            chapter_id,
            index,
            total,
            ..
        } => Some(EnrichmentRuntimeStatus::Building {
            phase: format!("phase1: {chapter_id}"),
            current: *index,
            total: *total,
            started_at_unix,
        }),
        // Terminal events are handled separately by the manager's
        // completion watcher path so we can stamp Complete /
        // Failed with the right `built_at_unix` and tear down the
        // in-flight slot. They're returned as `None` here on
        // purpose — the live-progress map only carries Building.
        _ => None,
    }
}

// ─── SensitiveCorpusOracle impl ─────────────────────────────────────
//
// Folder-ingest v1 §3.4: a watched-folder corpus marked sensitive
// must be excluded from the agent's ambient situated-context
// assembly. The runtime asks this oracle on every retrieval; we
// answer from the in-memory `corpora` map (the single source of
// truth for `WatchedFolderConfig`). Per ARCH §7.4 (defence in
// depth), the on-disk state file also mirrors the flag so a
// concurrent state-file inspector can verify the same answer.
#[async_trait]
impl SensitiveCorpusOracle for LocalCorpusManager {
    async fn sensitive_corpus_ids(&self) -> std::collections::HashSet<String> {
        let corpora = self.corpora.read().await;
        corpora
            .iter()
            .filter_map(|(id, cfg)| {
                let watched = cfg.source_type.watched_config()?;
                if watched.sensitive {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

// ─── FolderMetadataOracle impl ──────────────────────────────────────
//
// Folder-ingest v1 §6.3: any folder corpus that contributes
// retrieval should carry its display name + the "what I don't have"
// gaps (failed_files / skipped_by_extension) back through the
// runtime, so the model says "your case-files folder" and so the
// chat surface can surface a coverage chip.
//
// Snapshot-style API (sync to async over an in-memory map +
// per-corpus state-file read). The runtime calls this once per
// knowledge-query plan; the cost is bounded by the number of
// installed watched-folder corpora (typically <10), and each state
// read is a small JSON.
#[async_trait]
impl sovereign_core::traits::FolderMetadataOracle for LocalCorpusManager {
    async fn folder_metadata(
        &self,
    ) -> std::collections::HashMap<String, sovereign_core::traits::FolderMetadata> {
        use super::watched::state::WatchedFolderState;
        use sovereign_core::traits::FolderMetadata;
        let mut out: std::collections::HashMap<String, FolderMetadata> =
            std::collections::HashMap::new();
        let corpora = self.corpora.read().await;
        for (id, cfg) in corpora.iter() {
            if !cfg.source_type.is_watched() {
                continue;
            }
            let state_dir = self.engine_index_dir().join(id);
            let (failed_count, skipped_count, top_skipped) =
                match WatchedFolderState::load(&state_dir) {
                    Ok(Some(state)) => {
                        let failed = state.failed_files.len();
                        let skipped: usize = state.skipped_by_extension.values().sum();
                        let mut by_count: Vec<(String, usize)> = state
                            .skipped_by_extension
                            .iter()
                            .map(|(k, v)| (k.clone(), *v))
                            .collect();
                        by_count.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                        let top: Vec<String> =
                            by_count.into_iter().take(2).map(|(ext, _)| ext).collect();
                        (failed, skipped, top)
                    }
                    _ => (0, 0, Vec::new()),
                };
            out.insert(
                id.clone(),
                FolderMetadata {
                    display_name: cfg.display_name.clone(),
                    failed_count,
                    skipped_count,
                    top_skipped_extensions: top_skipped,
                },
            );
        }
        out
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_config(root: &Path) -> LocalCorpusConfig {
        LocalCorpusConfig::document_folder(root.to_path_buf(), "Test".into())
    }

    #[test]
    fn persist_and_load_round_trip() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let corpora_dir = config_dir(data_dir);
        std::fs::create_dir_all(&corpora_dir).unwrap();

        let cfg = sample_config(dir.path());
        persist_config(&corpora_dir, &cfg).unwrap();

        let loaded = load_persisted_configs(&corpora_dir).unwrap();
        assert_eq!(loaded.len(), 1);
        let got = loaded.get(&cfg.id).unwrap();
        assert_eq!(got.display_name, "Test");
    }

    #[test]
    fn missing_sidecar_dir_is_not_an_error() {
        let dir = tempdir().unwrap();
        let result = load_persisted_configs(&dir.path().join("does-not-exist"));
        let configs = result.unwrap();
        assert!(configs.is_empty());
    }

    #[test]
    fn malformed_sidecar_is_skipped_not_fatal() {
        let dir = tempdir().unwrap();
        let corpora_dir = config_dir(dir.path());
        std::fs::create_dir_all(&corpora_dir).unwrap();
        std::fs::write(corpora_dir.join("bogus.json"), "{not valid json").unwrap();
        let cfg = sample_config(dir.path());
        persist_config(&corpora_dir, &cfg).unwrap();
        let loaded = load_persisted_configs(&corpora_dir).unwrap();
        // The bogus file is skipped; the good one survives.
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key(&cfg.id));
    }

    #[test]
    fn recipe_path_layout_includes_corpus_id() {
        // recipe_path_for now takes the recipes_dir directly — the
        // caller (manager) supplies whatever directory the daemon
        // aligned with `CorpusEngine`'s overrides_dir. The function
        // is a flat join + ".toml" suffix.
        let p = recipe_path_for(Path::new("/tmp/data/recipes"), "folder-abc123");
        assert_eq!(p, PathBuf::from("/tmp/data/recipes/folder-abc123.toml"));
    }

    // ── auto-rebuild watchdog gate (Move 8) ─────────────────────────

    #[test]
    fn auto_rebuild_skips_when_no_status() {
        assert!(!should_fire_auto_rebuild(None));
    }

    #[test]
    fn auto_rebuild_skips_when_off() {
        use super::super::watched::state::EnrichmentRuntimeStatus;
        assert!(!should_fire_auto_rebuild(Some(
            &EnrichmentRuntimeStatus::Off
        )));
    }

    #[test]
    fn auto_rebuild_skips_when_building_in_flight() {
        use super::super::watched::state::EnrichmentRuntimeStatus;
        use sovereign_core::types::AssetState;
        let status = EnrichmentRuntimeStatus::Tiered {
            state: AssetState::PartiallyReady,
            started_at_unix: 0,
            built_at_unix: None,
            doc_count: 0,
        };
        assert!(!should_fire_auto_rebuild(Some(&status)));
    }

    #[test]
    fn auto_rebuild_skips_when_failed() {
        use super::super::watched::state::EnrichmentRuntimeStatus;
        let status = EnrichmentRuntimeStatus::Failed {
            failed_at_unix: 0,
            reason: "test".into(),
        };
        assert!(!should_fire_auto_rebuild(Some(&status)));
    }

    #[test]
    fn auto_rebuild_skips_when_legacy_complete() {
        // Legacy `Complete` is the subprocess-path terminal state;
        // it lacks tiered artifacts so an auto-rebuild here would
        // route through the wrong code path.
        use super::super::watched::state::EnrichmentRuntimeStatus;
        let status = EnrichmentRuntimeStatus::Complete {
            built_at_unix: 0,
            doc_count: 5,
        };
        assert!(!should_fire_auto_rebuild(Some(&status)));
    }

    #[test]
    fn auto_rebuild_fires_when_tiered_ready() {
        use super::super::watched::state::EnrichmentRuntimeStatus;
        use sovereign_core::types::AssetState;
        let status = EnrichmentRuntimeStatus::Tiered {
            state: AssetState::Ready,
            started_at_unix: 0,
            built_at_unix: Some(100),
            doc_count: 12,
        };
        assert!(should_fire_auto_rebuild(Some(&status)));
    }

    #[test]
    fn auto_rebuild_fires_when_tiered_multi_hop_ready() {
        // MultiHopReady = T2 done, T3 in flight. A sweep landing
        // here still benefits from a fresh rebuild because the T2
        // entity index does not yet cover the new chunks. T3 will
        // re-run as part of the rebuild — accepted cost.
        use super::super::watched::state::EnrichmentRuntimeStatus;
        use sovereign_core::types::AssetState;
        let status = EnrichmentRuntimeStatus::Tiered {
            state: AssetState::MultiHopReady,
            started_at_unix: 0,
            built_at_unix: None,
            doc_count: 12,
        };
        assert!(should_fire_auto_rebuild(Some(&status)));
    }
}
