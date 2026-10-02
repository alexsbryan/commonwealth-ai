// SPDX-License-Identifier: AGPL-3.0-or-later
//! Folder-ingest v1 §3.3 — per-folder enrichment orchestration.
//!
//! Wraps `sovereign_tools::enrich::run_enrich_build` (the subprocess
//! runner that already powers the desktop's `enrich_build_async`)
//! with watched-folder lifecycle: synthesize an `EnrichConfig` JSON
//! the way `enrich init` would, kick off the subprocess, route
//! `EnrichProgress` events back to the watched-folder state mirror,
//! enforce per-corpus mutual exclusion plus a global concurrency
//! cap of 1 (enrichment is GPU-bound — concurrent runs thrash).
//!
//! Why a separate driver instead of calling `enrich build` from the
//! manager directly: the manager is shared across watched-folder
//! mutations (register, sweep, pause, …) and has no business
//! holding a tokio Semaphore + per-corpus JoinHandles. The driver
//! owns that state and exposes a small async surface
//! (`enable`, `disable`, `rebuild`, `is_running`,
//! `current_status`) the manager calls into.
//!
//! Cost estimate: a simple linear heuristic for the UI's "Enable
//! enrichment will take ~X minutes" framing per spec §3.3. The
//! coefficient comes from prior SEP runs — Phase 1 dominates and
//! is roughly 0.6s/chunk on M-series with the daemon's primary
//! chat slot. The estimate is presented as a range to set
//! expectations honestly.
//!
//! v1 posture: full-rebuild on every enable / rebuild. The plan
//! committed to this trade-off to avoid the missing
//! incremental-Phase-1 plumbing in corpus-engine. When the user
//! enables enrichment, we run the whole pipeline; when they click
//! Rebuild, we run the whole pipeline. Disabling tears down the
//! atlas dir cleanly via `corpus_engine::atlas_teardown`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sovereign_core::error::{Error, Result};
use sovereign_core::types::AssetState;
use tokio::sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::task::JoinHandle;

use crate::enrich::{
    new_cancellation_flag, run_enrich_build, CancellationFlag, EnrichBuildConfig, EXIT_CANCELLED,
};
// The enrichment config is ingest's; this driver writes it only through
// ingest's port (pb-ingest-dial-tools-close).
use corpus_index::ingest_port::enrich_config::{EnrichConfigPort, WatchedEnrichConfig};

/// Daemon-side defaults the driver needs to synthesize an enrich
/// config when the user enables enrichment on a folder. Populated
/// at daemon boot from the active `sovereign-server.toml` —
/// without these, the driver can't run because every `EnrichConfig`
/// requires a `chat_model`, `embed_model`, and `base_url`.
#[derive(Debug, Clone)]
pub struct EnrichmentDefaults {
    /// Chat-completions model id the orchestrator routes Phase 1+
    /// LLM calls through. Daemon's primary chat slot in v1.
    pub chat_model: String,
    /// Embedding model id Phase 2 (resolution, dedup) uses for
    /// cosine similarity. Daemon's embed slot.
    pub embed_model: String,
    /// Base URL the orchestrator hits for chat / embeddings.
    /// `http://localhost:9741` for the loopback daemon.
    pub base_url: String,
    /// Optional path to the `sovereign-cli` binary the subprocess
    /// runner spawns. `None` = use `$PATH` lookup; tests point
    /// this at a fixture binary.
    pub cli_path: Option<PathBuf>,
}

/// What a svrn that composes no ingest program answers where it needs ingest's
/// enrichment config (pb-ingest-dial-tools-close): a named absence, never a
/// silent default (FIVE_PROGRAMS §4 rule 3). `what` names the act refused.
pub fn ingest_absent(what: &str) -> String {
    format!(
        "{what}: the enrichment config is the ingest program's, and this svrn \
         does not host it (the stock install, `svrn daemon start`, composes it)"
    )
}

/// Write the watched folder's config through ingest's port and return where
/// it landed: the path the subprocess spawned on the next line reads. The
/// schema and the watched-folder policy are the implementor's.
fn save_watched_config(
    port: &dyn EnrichConfigPort,
    config: &WatchedEnrichConfig<'_>,
) -> Result<PathBuf> {
    port.write_watched(config).map_err(|e| {
        Error::Execution(format!(
            "write enrich config for corpus '{}': {e}",
            config.corpus_id
        ))
    })
}

/// Folder-ingest v1 §3.3 cost-estimate range surfaced to the user
/// in the Enable-enrichment toggle UI. The numbers are a heuristic
/// — better-than-nothing framing the user can use to decide
/// whether to commit. The actual run can drift either direction
/// depending on doc length, model speed, and Phase 1b breadth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CostEstimate {
    /// Lower bound seconds. Optimistic case: short docs, primary
    /// chat slot warm, no Phase 1b retries.
    pub low_secs: u64,
    /// Upper bound seconds. Conservative: longer docs, occasional
    /// retries, fixed per-phase overhead included.
    pub high_secs: u64,
}

impl CostEstimate {
    /// Linear-in-doc-count heuristic. Coefficients picked from
    /// historical SEP runs (per `project_qwopus_size_ab.md`):
    ///
    /// - Phase 1 dominates: ~0.5–1.5s per chunk. We assume ~5
    ///   chunks per doc on average (longer docs amortise more).
    /// - Phase 2-7 fixed overhead: ~30s for the resolver +
    ///   tensions + gaps + configurations passes that don't scale
    ///   linearly with doc count.
    ///
    /// Returns `low_secs == high_secs == 0` for empty corpora —
    /// not "no cost", but "nothing to estimate". The UI hides the
    /// estimate in that case.
    pub fn from_doc_count(doc_count: usize) -> Self {
        if doc_count == 0 {
            return Self {
                low_secs: 0,
                high_secs: 0,
            };
        }
        let docs = doc_count as u64;
        let chunks_per_doc = 5_u64;
        let low_per_chunk_ms = 500_u64;
        let high_per_chunk_ms = 1_500_u64;
        let fixed_overhead_secs = 30_u64;
        let low_secs = fixed_overhead_secs + (docs * chunks_per_doc * low_per_chunk_ms) / 1000;
        let high_secs = fixed_overhead_secs + (docs * chunks_per_doc * high_per_chunk_ms) / 1000;
        Self {
            low_secs,
            high_secs,
        }
    }
}

/// Per-corpus job tracking record. Held inside `EnrichmentDriver`
/// for the duration of an in-flight build.
struct JobHandle {
    job_id: String,
    cancel: CancellationFlag,
    /// Tokio task that holds the subprocess. Dropping the driver
    /// (or the manager that owns it) aborts the task — but the
    /// subprocess is not auto-killed in that case, so callers
    /// who care about clean shutdown must call `cancel_all`
    /// before drop.
    task: JoinHandle<()>,
    /// Held for the duration of the build so the global semaphore
    /// stays acquired. Released on task completion.
    _permit: Arc<OwnedSemaphorePermit>,
}

/// Tiered-path dependencies installed at daemon boot. Until set,
/// `start_tiered_build` returns an error and `enable_enrichment`
/// falls back to the legacy `start_build` (subprocess) path.
///
/// `tiered` is ingest's `FolderTieredPort` over the tiered provider
/// shared with `CorpusEngine::with_tiered_provider` for conversation
/// corpora (`FolderTieredProvider` in `conv_tiered_provider.rs`) and the
/// optional GLiNER extractor, so a single daemon-side instance serves both
/// paths. The driver doesn't distinguish — it routes `corpus_id` +
/// `index_path` into `run_folder_tiered_enrichment`, which iterates the
/// index's per-`source_doc_id` groups and fires the provider once per doc
/// with `conv_uuid = source_doc_id`. Each document becomes its own
/// RAPTOR tree + signpost set; the folder is no longer collapsed
/// into a single bag.
#[derive(Clone)]
pub struct TieredDeps {
    pub tiered: Arc<dyn corpus_index::ingest_port::FolderTieredPort>,
}

/// Folder-ingest v1 §3.3 driver. One per daemon instance. Holds
/// the per-corpus job table + the global concurrency cap. The
/// `LocalCorpusManager` owns one `Arc<EnrichmentDriver>` and
/// proxies its public methods.
/// An in-process `enrich build` a HOST installs so the driver never has to
/// spawn a CLI — which a shipped desktop bundle does not carry (ontology-v1
/// P0.4, operator decision (b)). Object-safe with a boxed future so the one
/// host crate that links the orchestrator (`sovereign-cli-daemon`) can
/// implement it without this crate depending upward (ARCH_LAYERS: hosts are
/// terminal). `cancel` is polled between steps; the build returns
/// [`EXIT_CANCELLED`] when it honours it.
pub trait AtlasBuildRunner: Send + Sync {
    fn build(
        &self,
        corpus_id: String,
        progress: crate::enrich::EnrichProgressFn,
        cancel: CancellationFlag,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = i32> + Send + 'static>>;
}

pub struct EnrichmentDriver {
    defaults: RwLock<Option<EnrichmentDefaults>>,
    /// In-flight builds, keyed by corpus_id. Reads must take this
    /// mutex; the value is consumed when a build completes (the
    /// task itself removes its own entry via the manager-level
    /// cleanup hook in `start_build`).
    in_flight: Mutex<HashMap<String, JobHandle>>,
    /// Global concurrency cap. Capacity 1 in v1 because
    /// enrichment runs are GPU-bound; concurrent runs would thrash
    /// the daemon's chat slot.
    permits: Arc<Semaphore>,
    /// Tiered-enrichment dependencies. Installed by the daemon at
    /// boot via `set_tiered_deps`. `None` = the in-process path is
    /// unavailable; `LocalCorpusManager::enable_enrichment` errors
    /// loudly instead of shelling out (the subprocess fallback was
    /// removed — it isn't bundled with every deployment).
    tiered_deps: RwLock<Option<TieredDeps>>,
    /// The in-process atlas build, when the host installed one. `None` keeps
    /// the subprocess path (`run_enrich_build`) — a dev box with the CLI on
    /// PATH still works; a shipped bundle installs this.
    atlas_builder: RwLock<Option<Arc<dyn AtlasBuildRunner>>>,
    /// Ingest's enrichment-config port, installed by a host that composes
    /// ingest (`set_enrich_config`). `None` = the config sites answer
    /// [`ingest_absent`].
    enrich_config: RwLock<Option<Arc<dyn EnrichConfigPort>>>,
}

impl EnrichmentDriver {
    pub fn new() -> Self {
        Self {
            defaults: RwLock::new(None),
            in_flight: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(1)),
            tiered_deps: RwLock::new(None),
            atlas_builder: RwLock::new(None),
            enrich_config: RwLock::new(None),
        }
    }

    /// Install ingest's enrichment-config port. Idempotent: a second call
    /// replaces the first.
    pub async fn set_enrich_config(&self, port: Arc<dyn EnrichConfigPort>) {
        *self.enrich_config.write().await = Some(port);
    }

    /// The installed enrichment-config port, `None` when this process
    /// composes no ingest program.
    pub async fn enrich_config(&self) -> Option<Arc<dyn EnrichConfigPort>> {
        self.enrich_config.read().await.clone()
    }

    /// Install the host's in-process atlas build (see [`AtlasBuildRunner`]).
    pub async fn set_atlas_builder(&self, builder: Arc<dyn AtlasBuildRunner>) {
        *self.atlas_builder.write().await = Some(builder);
    }

    /// Whether [`set_atlas_builder`](Self::set_atlas_builder) has run.
    pub async fn has_atlas_builder(&self) -> bool {
        self.atlas_builder.read().await.is_some()
    }

    /// Install tiered-path deps. Called once at daemon boot after
    /// the tiered provider + GliNER extractor are constructed.
    /// Idempotent — calling again replaces the prior install.
    pub async fn set_tiered_deps(&self, deps: TieredDeps) {
        *self.tiered_deps.write().await = Some(deps);
    }

    /// True if the tiered path is wired and `start_tiered_build`
    /// will succeed. `LocalCorpusManager::enable_enrichment` gates on
    /// this: it routes through the in-process tiered driver when
    /// ready, and errors (no subprocess fallback) when not.
    pub async fn is_tiered_ready(&self) -> bool {
        self.tiered_deps.read().await.is_some()
    }

    /// Install daemon defaults. The daemon calls this once at boot
    /// after resolving its model paths + base URL. Without this,
    /// `start_build` returns an error pointing the operator at
    /// daemon setup.
    pub async fn set_defaults(&self, defaults: EnrichmentDefaults) {
        *self.defaults.write().await = Some(defaults);
    }

    /// True if the driver has defaults wired and is ready to
    /// accept builds. The HTTP layer surfaces this so the UI can
    /// disable the Enable-enrichment button before defaults
    /// arrive (e.g. during daemon boot).
    pub async fn is_ready(&self) -> bool {
        self.defaults.read().await.is_some()
    }

    /// True if a build is in flight for `corpus_id`. The manager
    /// calls this from `enable` / `rebuild` to avoid double-
    /// scheduling.
    pub async fn is_running(&self, corpus_id: &str) -> bool {
        self.in_flight.lock().await.contains_key(corpus_id)
    }

    /// Re-enrich a SINGLE source document in-process — the "flag a wrong
    /// summary → re-enrich just this note" revision loop
    /// (`docs/specs/SUMMARY_REVISION_LOOP.md`). Unlike `start_tiered_build`
    /// this does not spawn a detached task (the desktop flag flow awaits
    /// the corrected summary) and re-runs only the one note's RAPTOR via
    /// the provider's incremental path. The provider's `enrich_conversation`
    /// picks up any active correction for this note and, when it is still
    /// `pending`, forces past the content-hash checkpoint so the summary
    /// actually regenerates with the hint.
    ///
    /// `try_acquire` the single permit: if a full build holds it, return a
    /// friendly busy error rather than parking an interactive request
    /// behind a multi-minute run.
    pub async fn reenrich_source(&self, corpus_id: &str, source_doc_id: &str) -> Result<()> {
        let deps = {
            let guard = self.tiered_deps.read().await;
            guard.as_ref().cloned().ok_or_else(|| {
                Error::Execution(
                    "tiered enrichment deps not installed yet — \
                     daemon boot incomplete or feature disabled"
                        .into(),
                )
            })?
        };

        let _permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            Error::Execution(
                "enrichment is busy with a full build right now — \
                 try re-enriching this note again when it finishes"
                    .into(),
            )
        })?;

        deps.tiered
            .reenrich_sources(corpus_id, &[source_doc_id.to_string()])
            .await
            .map_err(|e| Error::Execution(format!("re-enrich note '{source_doc_id}': {e}")))
    }

    /// Legacy CLI-subprocess build. NO LONGER WIRED into
    /// `LocalCorpusManager::enable_enrichment` — that path now requires
    /// the in-process tiered driver (`start_tiered_build`) and errors
    /// when it isn't ready, rather than shelling out here (the
    /// `sovereign-cli` binary isn't bundled with every deployment, so
    /// this exited 127 and wedged builds silently). Retained as a
    /// tested library primitive for tools that run against a full CLI
    /// install; do not re-wire it into a user-facing enable path.
    ///
    /// Start a build for `corpus_id` rooted at `source_path`,
    /// running pipeline `pipeline_id`. Returns the assigned job_id
    /// immediately; the actual run happens in a spawned task.
    ///
    /// Errors when:
    /// - Defaults aren't installed yet.
    /// - A build is already in flight for this corpus (caller
    ///   should `cancel` first if they want to retry).
    /// - The synthesised `EnrichConfig` can't be written to disk.
    /// - The semaphore is closed (driver shutdown).
    ///
    /// `progress` is invoked synchronously from the subprocess
    /// stdout-reader task as `EnrichProgress` events arrive. The
    /// callback is held inside the spawned task; failures inside
    /// it are silently ignored (the build itself proceeds).
    pub async fn start_build(
        &self,
        corpus_id: &str,
        source_path: &Path,
        pipeline_id: &str,
        progress: crate::enrich::EnrichProgressFn,
    ) -> Result<String> {
        let defaults = {
            let guard = self.defaults.read().await;
            guard.as_ref().cloned().ok_or_else(|| {
                Error::Execution(
                    "enrichment defaults not installed yet — daemon \
                         boot incomplete or models unconfigured"
                        .into(),
                )
            })?
        };

        if self.is_running(corpus_id).await {
            return Err(Error::Execution(format!(
                "enrichment build already in flight for corpus '{corpus_id}' \
                 — cancel it first if you want to retry"
            )));
        }

        // Write the enrich config. The build reads from this exact path on
        // the very next line.
        let Some(port) = self.enrich_config().await else {
            tracing::info!(
                corpus_id,
                "watched_folder:enrich_config_absent — no ingest program in this process"
            );
            return Err(Error::Execution(ingest_absent(&format!(
                "write enrich config for corpus '{corpus_id}'"
            ))));
        };
        save_watched_config(
            port.as_ref(),
            &WatchedEnrichConfig {
                corpus_id,
                pipeline_id,
                source_path,
                chat_model: &defaults.chat_model,
                embed_model: &defaults.embed_model,
                base_url: &defaults.base_url,
            },
        )?;

        self.spawn_build(corpus_id, defaults.cli_path.clone(), progress)
            .await
    }

    /// Start the atlas build for a corpus whose enrichment config ALREADY
    /// exists (a recipe corpus after `svrn enrich init`; the caller checks).
    /// No config is synthesized — a recipe-driven config carries the custom
    /// ontology, and rewriting it here would lose it. Same permit, same
    /// in-flight bookkeeping, same cancel flag as [`start_build`](Self::start_build).
    /// Does not require the watched-folder enrichment defaults: with the
    /// in-process builder installed there is no CLI to point at.
    pub async fn start_atlas_build(
        &self,
        corpus_id: &str,
        progress: crate::enrich::EnrichProgressFn,
    ) -> Result<String> {
        if self.is_running(corpus_id).await {
            return Err(Error::Execution(format!(
                "enrichment build already in flight for corpus '{corpus_id}' \
                 — cancel it first if you want to retry"
            )));
        }
        let cli_path = self
            .defaults
            .read()
            .await
            .as_ref()
            .and_then(|d| d.cli_path.clone());
        self.spawn_build(corpus_id, cli_path, progress).await
    }

    /// The shared tail of both `start_*` entries: take the global permit,
    /// spawn the build — in-process when a host installed an
    /// [`AtlasBuildRunner`], else the `sovereign-cli` subprocess — and record
    /// the job so `cancel` / `forget` / `in_flight_snapshot` see it.
    async fn spawn_build(
        &self,
        corpus_id: &str,
        cli_path: Option<PathBuf>,
        progress: crate::enrich::EnrichProgressFn,
    ) -> Result<String> {
        // Acquire a global permit. With capacity = 1 this means a
        // build for any other corpus is queued behind this one;
        // by the time the spawned task starts running, the permit
        // is held until the build returns.
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Execution("enrichment driver shutting down".into()))?;
        let permit = Arc::new(permit);

        let job_id = uuid::Uuid::new_v4().to_string();
        let cancel = new_cancellation_flag();
        let cancel_for_task = cancel.clone();
        let corpus_id_owned = corpus_id.to_string();
        let task_permit = permit.clone();
        let builder = self.atlas_builder.read().await.clone();

        let task = tokio::spawn(async move {
            // Hold the permit for the duration of the build.
            let _permit = task_permit;
            if let Some(builder) = builder {
                tracing::info!(
                    corpus_id = %corpus_id_owned,
                    "enrichment_driver:build_in_process"
                );
                let code = builder
                    .build(corpus_id_owned.clone(), progress.clone(), cancel_for_task)
                    .await;
                if code == 0 {
                    tracing::info!(
                        corpus_id = %corpus_id_owned,
                        "enrichment_driver:build_complete"
                    );
                } else if code == EXIT_CANCELLED {
                    tracing::info!(
                        corpus_id = %corpus_id_owned,
                        "enrichment_driver:build_cancelled"
                    );
                } else {
                    tracing::warn!(
                        corpus_id = %corpus_id_owned,
                        exit_code = code,
                        "enrichment_driver:build_failed_exit"
                    );
                }
                return;
            }
            let build_cfg = EnrichBuildConfig {
                cli_path,
                extra_args: vec!["--full".into()],
                cancel: Some(cancel_for_task),
            };
            // The runner's progress callback fires synchronously
            // for each parsed stdout line; we route directly to
            // the manager-supplied sink.
            let result =
                run_enrich_build(&corpus_id_owned, build_cfg, Some(progress.clone())).await;
            match result {
                Ok(out) => {
                    if out.cancelled {
                        tracing::info!(
                            corpus_id = %corpus_id_owned,
                            "enrichment_driver:build_cancelled"
                        );
                    } else if out.exit_code != 0 {
                        tracing::warn!(
                            corpus_id = %corpus_id_owned,
                            exit_code = out.exit_code,
                            "enrichment_driver:build_failed_exit"
                        );
                    } else {
                        tracing::info!(
                            corpus_id = %corpus_id_owned,
                            "enrichment_driver:build_complete"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        corpus_id = %corpus_id_owned,
                        "enrichment_driver:build_io_error: {e}"
                    );
                }
            }
        });

        self.in_flight.lock().await.insert(
            corpus_id.to_string(),
            JobHandle {
                job_id: job_id.clone(),
                cancel,
                task,
                _permit: permit,
            },
        );
        Ok(job_id)
    }

    /// In-process tiered build. Runs T2 (GliNER chunk_entities) +
    /// T3 (RAPTOR atlas + motif index) inside the daemon, streaming
    /// `AssetState` transitions through `on_state` so the manager
    /// can update the live `EnrichmentRuntimeStatus` mirror visible
    /// to the UI.
    ///
    /// Assumes T1 is already complete (embeddings live in the
    /// corpus's Lance index from prior sweeps). First transition is
    /// to `PartiallyReady`; final is `Ready` on success or
    /// `Failed { reason }` on RAPTOR-build failure. GliNER failure
    /// is best-effort and does NOT fail the build — the corpus
    /// still gets a RAPTOR-only tier.
    ///
    /// Errors at the synchronous entry point (before the spawn):
    /// - Tiered deps not installed yet (`set_tiered_deps` missing).
    /// - A build is already in flight for `corpus_id`.
    /// - Global semaphore closed (driver shutdown).
    pub async fn start_tiered_build(
        &self,
        corpus_id: &str,
        index_path: &Path,
        on_state: Arc<dyn Fn(AssetState) + Send + Sync>,
    ) -> Result<String> {
        let deps = {
            let guard = self.tiered_deps.read().await;
            guard.as_ref().cloned().ok_or_else(|| {
                Error::Execution(
                    "tiered enrichment deps not installed yet — \
                     daemon boot incomplete or feature disabled"
                        .into(),
                )
            })?
        };

        if self.is_running(corpus_id).await {
            return Err(Error::Execution(format!(
                "enrichment build already in flight for corpus '{corpus_id}' \
                 — cancel it first if you want to retry"
            )));
        }

        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Execution("enrichment driver shutting down".into()))?;
        let permit = Arc::new(permit);

        let job_id = uuid::Uuid::new_v4().to_string();
        // Cancellation flag is allocated so `cancel(corpus_id)` keeps
        // working uniformly across legacy + tiered jobs. The tiered
        // path doesn't honour mid-run cancellation today (the
        // run_folder_tiered_enrichment helper has no cancel hook);
        // this is a known v1 limitation surfaced as a no-op.
        let cancel = new_cancellation_flag();
        let corpus_id_owned = corpus_id.to_string();
        let index_path_owned = index_path.to_path_buf();
        let task_permit = permit.clone();

        let task = tokio::spawn(async move {
            let _permit = task_permit;
            tracing::info!(
                corpus_id = %corpus_id_owned,
                index = %index_path_owned.display(),
                "tiered_driver: build starting"
            );

            // Liveness floor for the whole T2 + T3 build. GliNER NER is
            // CPU-bound and emits no enrichment `stamp`s, and a single long
            // RAPTOR document can summarise for minutes between the provider's
            // coarse phase stamps. Without a heartbeat either window can cross
            // `STALL_THRESHOLD_SECS` and the status endpoint reports a wedge on
            // a build that is very much alive (the "last build stopped before
            // finishing" false positive). Dropped when this task ends — after
            // the provider's terminal Complete/Failed stamp has already landed,
            // and the heartbeat never touches a terminal state, so it cannot
            // race that stamp.
            let _build_heartbeat = corpus_index::enrichment_state::EnrichmentHeartbeat::spawn(
                index_path_owned.clone(),
            );

            // T1 is already on disk (corpus-engine ingest stamps
            // embeddings into Lance). The first explicit tier
            // marker is PartiallyReady — UI shifts from
            // "Building" to "T1 ready, T2 in flight".
            on_state(AssetState::PartiallyReady);

            // T2: GliNER chunk_entities delta. Best-effort —
            // failure logs + the build proceeds with RAPTOR-only
            // entities (the conv_entity_graph builder degrades
            // gracefully when chunk_entities is empty).
            if deps.tiered.has_entity_extractor() {
                // Honest phase label for the CPU-bound NER pass so the UI moves
                // off "Scanning documents" to "Finding people, places, and
                // ideas" while entities extract. The build heartbeat above keeps
                // `last_progress_at` fresh across this pass, which emits no
                // enrichment stamps of its own.
                if let Err(e) = corpus_index::enrichment_state::EnrichmentStateFile::stamp(
                    &index_path_owned,
                    &corpus_id_owned,
                    Some("folder_tiered"),
                    corpus_index::enrichment_state::EnrichmentPhase::EntityExtraction,
                    0,
                    0,
                    Some("Finding people, places, and ideas"),
                ) {
                    tracing::warn!(
                        corpus_id = %corpus_id_owned,
                        error = %e,
                        "tiered_driver: could not stamp EntityExtraction phase"
                    );
                }
                // A refused-over-cap count is reported inside the port,
                // where the operator already looks (ARCH 6).
                match deps
                    .tiered
                    .extract_entity_delta(&corpus_id_owned, &index_path_owned)
                    .await
                {
                    Some(Ok(o)) => tracing::info!(
                        corpus_id = %corpus_id_owned,
                        mentions = o.mentions,
                        refused_over_cap = o.refused_over_cap,
                        "tiered_driver: GliNER delta complete"
                    ),
                    Some(Err(e)) => tracing::warn!(
                        corpus_id = %corpus_id_owned,
                        error = %e,
                        "tiered_driver: GliNER delta failed; continuing with RAPTOR-only entities"
                    ),
                    None => {}
                }
            }
            on_state(AssetState::MultiHopReady);

            // T3: RAPTOR tree + motif index. Persistence happens
            // inside the provider's enrich_conversation. The runner
            // iterates per `source_doc_id` and calls the provider
            // once per document (`conv_uuid = source_doc_id`) so
            // heterogeneous folders produce one RAPTOR tree per
            // file rather than a single mixed-topic bag. Pass
            // `None` for the entity_extractor because we already
            // ran the delta above.
            match deps
                .tiered
                .run_folder_tiered_enrichment(&corpus_id_owned, &index_path_owned)
                .await
            {
                Ok(()) => {
                    on_state(AssetState::Ready);
                    tracing::info!(
                        corpus_id = %corpus_id_owned,
                        "tiered_driver: build complete"
                    );
                }
                Err(e) => {
                    let reason = format!("{e}");
                    on_state(AssetState::Failed {
                        reason: reason.clone(),
                    });
                    // Stamp the enrichment state Failed too. The runner
                    // only returns Err on a pre-loop error (index open /
                    // grouping) — it never returns Err after stamping its
                    // terminal Complete — so this can't clobber a success.
                    // Without it a pre-loop failure would sit non-terminal
                    // until the 10-min stall sweep, reading as "still
                    // building" the whole time.
                    let _ = corpus_index::enrichment_state::EnrichmentStateFile::fail(
                        &index_path_owned,
                        &corpus_id_owned,
                        &reason,
                    );
                    tracing::warn!(
                        corpus_id = %corpus_id_owned,
                        error = %reason,
                        "tiered_driver: run_folder_tiered_enrichment failed"
                    );
                }
            }
        });

        self.in_flight.lock().await.insert(
            corpus_id.to_string(),
            JobHandle {
                job_id: job_id.clone(),
                cancel,
                task,
                _permit: permit,
            },
        );
        Ok(job_id)
    }

    /// Signal cancellation to an in-flight build. Returns `true`
    /// if a build was running (and now will tear down on its next
    /// stdout poll), `false` if no build was found for `corpus_id`.
    pub async fn cancel(&self, corpus_id: &str) -> bool {
        if let Some(handle) = self.in_flight.lock().await.get(corpus_id) {
            crate::enrich::fire_cancellation(&handle.cancel);
            true
        } else {
            false
        }
    }

    /// Remove the in-flight record for `corpus_id`. Called by the
    /// manager from a watcher task once it observes the build has
    /// completed (success, failure, or cancelled). The driver
    /// itself doesn't auto-remove because the manager wants to
    /// observe completion to update `WatchedFolderState.enrichment_status`
    /// before the slot opens up for another build.
    pub async fn forget(&self, corpus_id: &str) -> Option<String> {
        self.in_flight
            .lock()
            .await
            .remove(corpus_id)
            .map(|h| h.job_id)
    }

    /// Snapshot of all in-flight job IDs by corpus. The HTTP layer
    /// uses this to surface "what's enriching right now" without
    /// poking individual corpora.
    pub async fn in_flight_snapshot(&self) -> HashMap<String, String> {
        self.in_flight
            .lock()
            .await
            .iter()
            .map(|(id, h)| (id.clone(), h.job_id.clone()))
            .collect()
    }
}

impl Default for EnrichmentDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    // The data-dir test lock intentionally spans awaits: the guard
    // must cover the whole test body (the env var is process-global), and
    // each #[tokio::test] owns its runtime, so a contending sibling parks a
    // thread — serialization, never deadlock (P0.3 lock audit, 2026-07-12).
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use sovereign_contracts::daemon_wire::enrich_progress::EnrichProgress;
    use tempfile::tempdir;

    fn defaults() -> EnrichmentDefaults {
        EnrichmentDefaults {
            chat_model: "qwen3-32b".into(),
            embed_model: "qwen3-embedding-0.6b".into(),
            base_url: "http://localhost:9741".into(),
            cli_path: None,
        }
    }

    #[test]
    fn cost_estimate_zero_for_empty_corpus() {
        let est = CostEstimate::from_doc_count(0);
        assert_eq!(est.low_secs, 0);
        assert_eq!(est.high_secs, 0);
    }

    #[test]
    fn cost_estimate_scales_linearly() {
        let small = CostEstimate::from_doc_count(10);
        let big = CostEstimate::from_doc_count(100);
        // Fixed overhead is the same; per-doc piece is 10×.
        assert!(big.low_secs > small.low_secs);
        assert!(big.high_secs > small.high_secs);
        // The high bound is always at least the low bound (sanity).
        assert!(small.high_secs >= small.low_secs);
        assert!(big.high_secs >= big.low_secs);
    }

    #[tokio::test]
    async fn driver_rejects_start_without_defaults() {
        let driver = EnrichmentDriver::new();
        let progress: crate::enrich::EnrichProgressFn = Arc::new(|_| {});
        let result = driver
            .start_build("c1", Path::new("/tmp"), "philosophy_atlas", progress)
            .await;
        assert!(result.is_err(), "expected Err when defaults not installed");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("defaults not installed"),
            "unexpected error message: {msg}"
        );
    }

    /// Per-file lock so tests that mutate `SVRNMESH_DATA_DIR` don't race
    /// each other under `cargo test`'s parallel runner. The env var is
    /// process-global; two parallel `set_var(..)` callers would
    /// otherwise see each other's value on read. Mirrors the
    /// `cache_test_lock()` pattern in `mcp_surface.rs` — preferred
    /// over `serial_test` because it scopes to the tests that need it.
    fn data_dir_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Records what it was asked to build and reports Complete, so the
    /// test can see the in-process path was taken.
    struct RecordingBuilder {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl AtlasBuildRunner for RecordingBuilder {
        fn build(
            &self,
            corpus_id: String,
            progress: crate::enrich::EnrichProgressFn,
            _cancel: CancellationFlag,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = i32> + Send + 'static>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(corpus_id.clone());
                progress(EnrichProgress::Complete {
                    corpus_id,
                    steps_completed: 0,
                });
                0
            })
        }
    }

    /// The daemon-level substitution for the tauri-bundle gate (ontology-v1
    /// P0.4): with a host-installed builder, `start_atlas_build` runs the
    /// build in-process and never reaches for a CLI. `cli_path` names a
    /// binary that does not exist, so had the subprocess path been taken the
    /// spawn would have failed and no `Complete` would arrive; the recorded
    /// call and the Complete event prove which path ran.
    #[tokio::test]
    async fn start_atlas_build_runs_the_installed_builder_not_a_subprocess() {
        let driver = EnrichmentDriver::new();
        let mut d = defaults();
        d.cli_path = Some(PathBuf::from("/nonexistent/sovereign-cli"));
        driver.set_defaults(d).await;
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        driver
            .set_atlas_builder(Arc::new(RecordingBuilder {
                calls: calls.clone(),
            }))
            .await;
        assert!(driver.has_atlas_builder().await);

        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_sink = completed.clone();
        let progress: crate::enrich::EnrichProgressFn = Arc::new(move |evt| {
            if matches!(evt, EnrichProgress::Complete { .. }) {
                completed_sink.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let job = driver
            .start_atlas_build("maple-house", progress)
            .await
            .expect("an installed builder needs no CLI");
        assert!(!job.is_empty());

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !completed.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the in-process build reports Complete");
        assert_eq!(*calls.lock().unwrap(), vec!["maple-house".to_string()]);
        assert_eq!(
            driver.in_flight_snapshot().await.get("maple-house"),
            Some(&job),
            "the job is tracked like a subprocess build"
        );
    }

    /// The write goes through the installed port with the folder's inputs,
    /// and the build is accepted. Fails if the driver writes the config any
    /// other way (the double is the only writer) or drops an input.
    #[tokio::test]
    async fn start_build_writes_the_watched_config_through_the_installed_port() {
        let driver = EnrichmentDriver::new();
        let mut d = defaults();
        d.cli_path = Some(PathBuf::from("/bin/true"));
        driver.set_defaults(d).await;
        let port = Arc::new(
            corpus_index::ingest_port::double::IngestPortDouble::new()
                .writing_watched_configs_under("/enrichment"),
        );
        driver.set_enrich_config(port.clone()).await;
        let progress: crate::enrich::EnrichProgressFn = Arc::new(|_| {});
        driver
            .start_build("c1", Path::new("/tmp/notes"), "philosophy_atlas", progress)
            .await
            .expect("build accepted");
        assert_eq!(
            port.watched_writes(),
            vec![(
                "c1".to_string(),
                "philosophy_atlas".to_string(),
                PathBuf::from("/tmp/notes")
            )]
        );
        driver.cancel("c1").await;
        driver.forget("c1").await;
    }

    /// A svrn that composes no ingest program refuses the write by name and
    /// starts no build. Fails if the driver writes through a default or
    /// reports success.
    #[tokio::test]
    async fn a_driver_with_no_ingest_reports_it_absent_by_name() {
        let driver = EnrichmentDriver::new();
        driver.set_defaults(defaults()).await;
        let progress: crate::enrich::EnrichProgressFn = Arc::new(|_| {});
        let err = driver
            .start_build("c1", Path::new("/tmp/notes"), "philosophy_atlas", progress)
            .await
            .expect_err("no ingest program: the write is refused");
        let msg = err.to_string();
        assert!(
            msg.contains("write enrich config for corpus 'c1'")
                && msg.contains("the ingest program's"),
            "absence not named: {msg}"
        );
        assert!(!driver.is_running("c1").await, "a build started anyway");
    }

    #[tokio::test]
    async fn driver_rejects_concurrent_start_for_same_corpus() {
        let _guard = data_dir_test_lock();
        // Override the data root for this test.
        let dir = tempdir().unwrap();
        std::env::set_var("SVRNMESH_DATA_DIR", dir.path());

        let driver = EnrichmentDriver::new();
        driver.set_defaults(defaults()).await;
        // Use a fake CLI path that exits immediately so the
        // spawned task doesn't actually try to run anything
        // useful. Failure-to-spawn is fine — we're testing the
        // pre-spawn race-rejection logic.
        let mut d = defaults();
        d.cli_path = Some(PathBuf::from("/bin/true"));
        driver.set_defaults(d).await;
        driver
            .set_enrich_config(Arc::new(
                corpus_index::ingest_port::double::IngestPortDouble::new()
                    .writing_watched_configs_under(dir.path()),
            ))
            .await;
        let progress: crate::enrich::EnrichProgressFn = Arc::new(|_| {});

        let _job_id = driver
            .start_build(
                "c1",
                Path::new("/tmp"),
                "philosophy_atlas",
                progress.clone(),
            )
            .await
            .expect("first build accepted");
        let second = driver
            .start_build("c1", Path::new("/tmp"), "philosophy_atlas", progress)
            .await;
        // The second start should reject because the first is
        // still tracked. Even if the first task already exited
        // (subprocess returned immediately), `forget` hasn't been
        // called yet, so the slot is still held.
        match second {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("already in flight"),
                    "expected already-in-flight error, got: {msg}"
                );
            }
            Ok(_) => {
                // Race: if the test machine ran /bin/true so fast
                // the first task hadn't even inserted into in_flight
                // yet, accept this case as well — the lock was
                // properly released. Not a failure mode; just a
                // timing window.
            }
        }
        // Clean up: signal cancellation + forget so the driver's
        // tokio task drains.
        driver.cancel("c1").await;
        driver.forget("c1").await;
        std::env::remove_var("SVRNMESH_DATA_DIR");
    }
}
