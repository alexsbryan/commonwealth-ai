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
//! See `/Users/alexsbryan/.claude/plans/binary-scribbling-babbage.md`
//! for the broader architecture.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore};
use tokio::sync::RwLock;

use corpus_engine::{CorpusEngine, CorpusSpec, ScoredChunk};

use super::config::{recipe_toml, LocalCorpusConfig};
use super::extract_stage::{self, default_staging_path};
use super::pre_scanner::{PreScanResult, PreScanner};
use super::progress::{ExcerptChunk, LocalCorpusProgress, RuntimeFailure};

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
    corpora: RwLock<HashMap<String, LocalCorpusConfig>>,
    /// Cache of the most recent `LabeledClusterResult` per corpus, so
    /// `build_preview` can be called separately from `cluster` without
    /// re-running the LLM labelling pass. Cleared on app restart —
    /// the user re-clicks "Organize" each session.
    cluster_results: RwLock<HashMap<String, super::clusterer::LabeledClusterResult>>,
}

impl LocalCorpusManager {
    /// Construct + load persisted corpora. `data_dir` is the Sovereign
    /// data directory (typically `~/.sovereign`). Sidecars live under
    /// `{data_dir}/local-corpora/` and staging under
    /// `{data_dir}/local-corpus-staging/`. `snapshot_root` is the
    /// default root for vault snapshots (used only by Obsidian flows,
    /// M5).
    pub async fn init(
        engine: Arc<CorpusEngine>,
        store: Arc<dyn StateStore>,
        inference: Option<Arc<dyn InferenceProvider>>,
        data_dir: PathBuf,
        snapshot_root: PathBuf,
    ) -> Result<Self> {
        let corpora_dir = config_dir(&data_dir);
        std::fs::create_dir_all(&corpora_dir)
            .map_err(|e| Error::Execution(format!("create corpora dir: {e}")))?;

        let corpora = load_persisted_configs(&corpora_dir)?;

        Ok(Self {
            engine,
            store,
            inference,
            data_dir,
            snapshot_root,
            corpora: RwLock::new(corpora),
            cluster_results: RwLock::new(HashMap::new()),
        })
    }

    /// The default snapshot root Obsidian write-back should use when a
    /// new vault is registered. `obsidian_vault(path, snapshot_root)`
    /// in config.rs is the canonical factory to call.
    pub fn snapshot_root(&self) -> &Path {
        &self.snapshot_root
    }

    /// Persist a new corpus config. Idempotent: registering the same
    /// ID overwrites (re-canonicalising the root path, for example).
    pub async fn register(&self, config: LocalCorpusConfig) -> Result<String> {
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
    pub async fn remove(&self, id: &str) -> Result<()> {
        self.engine
            .remove_corpus_everything(id)
            .map_err(|e| Error::Execution(format!("remove index: {e}")))?;
        let _ = self.store.delete_corpus_state(id).await; // best-effort
        let path = config_file(&self.data_dir, id);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| Error::Execution(format!("remove config: {e}")))?;
        }
        self.corpora.write().await.remove(id);
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
    ///   3. Render a Recipe TOML for `corpus-engine` with the staged
    ///      JSONL as the source.
    ///   4. Delegate to `CorpusEngine::ingest`, bridging its
    ///      `IngestProgress` into our `LocalCorpusProgress::Ingesting`.
    ///   5. Return `IngestStats` — per-file runtime failures preserved.
    pub async fn ingest(
        &self,
        id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestStats> {
        let config = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("local corpus '{id}' not registered")))?;

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
        let stage_result = stage_result.map_err(|e| Error::Execution(format!("stage io: {e}")))?;

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

        // 3. Write recipe TOML to a temp file.
        let recipe = recipe_toml(&config, &staging);
        let recipe_path = recipe_path_for(&self.data_dir, &config.id);
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

        // 6. Compose IngestStats.
        let stats = IngestStats {
            corpus_id: ingest_result.corpus_id.clone(),
            files_indexed: readable_files.len() - stage_result.failures.len(),
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
        let cfg = self.get(id).await.ok_or_else(|| {
            Error::NotFound(format!("local corpus '{id}' not registered"))
        })?;
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
        let clusterer = super::clusterer::Clusterer::new(
            Arc::clone(&self.engine),
            inference_fn,
        );
        let result = clusterer.run(id, config, on_progress).await?;
        // Cache for subsequent `get_preview` calls so the UI doesn't
        // have to hand the whole result blob back through Tauri.
        self.cluster_results
            .write()
            .await
            .insert(id.to_string(), result.clone());
        Ok(result)
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
        let cfg = self.get(id).await.ok_or_else(|| {
            Error::NotFound(format!("local corpus '{id}' not registered"))
        })?;
        Ok(super::git::check_git_repo(&cfg.root_path))
    }

    /// Write tags (and optional index notes) for a previously-computed
    /// preview. Takes a snapshot first; rolls back nothing on per-file
    /// failure (the user can trigger rollback explicitly from the UI).
    pub async fn write_tags(
        &self,
        id: &str,
        git_commit: bool,
    ) -> Result<super::writeback::WriteBackResult> {
        let cfg = self.get(id).await.ok_or_else(|| {
            Error::NotFound(format!("local corpus '{id}' not registered"))
        })?;
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
        let wb = super::writeback::WriteBack::new(
            wb_cfg,
            cfg.root_path.clone(),
            cfg.id.clone(),
        );
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
    pub async fn list_snapshots(
        &self,
        id: &str,
    ) -> Result<Vec<super::writeback::SnapshotMeta>> {
        let cfg = self.get(id).await.ok_or_else(|| {
            Error::NotFound(format!("local corpus '{id}' not registered"))
        })?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!(
                "corpus '{id}' is not configured for write-back"
            ))
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
        let cfg = self.get(id).await.ok_or_else(|| {
            Error::NotFound(format!("local corpus '{id}' not registered"))
        })?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!(
                "corpus '{id}' is not configured for write-back"
            ))
        })?;
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path, cfg.id);
        let snapshot = wb.load_snapshot(snapshot_path)?;
        wb.rollback(&snapshot).await
    }

    /// Remove every sovereign/* tag and every sovereign_* key from
    /// every note in the vault; delete the generated index-note
    /// directory. Does NOT touch snapshots.
    pub async fn clean(&self, id: &str) -> Result<super::writeback::CleanResult> {
        let cfg = self.get(id).await.ok_or_else(|| {
            Error::NotFound(format!("local corpus '{id}' not registered"))
        })?;
        let wb_cfg = cfg.write_back.clone().ok_or_else(|| {
            Error::Execution(format!(
                "corpus '{id}' is not configured for write-back"
            ))
        })?;
        let wb = super::writeback::WriteBack::new(wb_cfg, cfg.root_path, cfg.id);
        wb.clean().await
    }

    /// Search this corpus. Embeds `query` with the engine's model, opens
    /// the corpus index, and returns the top `limit` ScoredChunks.
    ///
    /// Unknown corpus id returns `NotFound`, not an empty result,
    /// because silent empties are a debugging nightmare.
    pub async fn search(
        &self,
        id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
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

    fn engine_index_dir(&self) -> PathBuf {
        // The engine does not expose its index_dir publicly yet, so we
        // derive it from data_dir by convention. `AppState` passes the
        // same data_dir both to CorpusEngine and to us.
        self.data_dir.join("indexes")
    }
}

// ─── Progress bridge ─────────────────────────────────────────────────

fn ingest_progress_to_local(
    p: corpus_engine::progress::IngestProgress,
) -> LocalCorpusProgress {
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

fn recipe_path_for(data_dir: &Path, corpus_id: &str) -> PathBuf {
    data_dir
        .join("local-corpus-recipes")
        .join(format!("{corpus_id}.toml"))
}

fn persist_config(dir: &Path, config: &LocalCorpusConfig) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Execution(format!("create dir: {e}")))?;
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
                tracing::warn!(
                    "skip unparseable local corpus sidecar {:?}: {e}",
                    path
                );
            }
        }
    }
    Ok(out)
}

fn noop_progress() -> ProgressCallback {
    Arc::new(|_| {})
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
        let p = recipe_path_for(Path::new("/tmp/data"), "folder-abc123");
        assert_eq!(
            p,
            PathBuf::from("/tmp/data/local-corpus-recipes/folder-abc123.toml")
        );
    }
}
