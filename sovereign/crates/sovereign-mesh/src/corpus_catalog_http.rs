// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus catalogue and the notebook shelf — the rest of
//! `/internal/corpus` (sv-surface D9a).
//!
//! Six routes over the same `CorpusEngine` `reading_http` serves: catalog
//! (`builtin_corpora` ∪ `installed_indexes`), the five-source notebook shelf
//! fold, diagnose, per-corpus health, coverage card, retry-enrichment. Their
//! own file because they answer a different question, and `reading_http` is
//! already within sight of the §3.1 split line. Serving the SHELF rather than
//! its inputs makes ~120 lines of naming and sort judgement one
//! implementation, on the process that holds all five sources natively.
//!
//! **The param is `{corpus}`, not `{id}`** — `matchit` panics at merge time on
//! two parameter names in one slot. A wire fact, not a style choice.
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! Deliberately NOT decided here:
//! - **`"installing"`** — a corpus mid-ingest is not in `installed_indexes()`
//!   at all; "what is in flight" is `GET /internal/corpus/status`'s answer.
//! - **`tiers`** — [`tiers_for`] is an eleven-arm `match` on corpus id (§2.1),
//!   carried down verbatim rather than improved in transit (§10.2); it belongs
//!   in `registry_snapshot.toml`, and one copy is the precondition.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use corpus_engine::CorpusEngine;
use sovereign_tools::atlas_view::FileAtlasReader;
use sovereign_tools::local_corpus::config::{LocalCorpusConfig, LocalCorpusSourceType};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

pub use sovereign_contracts::daemon_wire::{IndexBuildProgress, IndexBuildState, IngestJobAck};

// ─── The wire projections ──────────────────────────────────────

/// One row of the catalogue picker — a built-in recipe, or an
/// installed index with no catalogue entry behind it.
///
/// Field-for-field the desktop's `CorpusEntry`, which is
/// `Serialize`-only up there (a Tauri return) and has to parse back
/// down here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_compressed_gb: f64,
    pub size_indexed_gb: f64,
    pub license: String,
    pub tiers: Vec<String>,
    /// `"installed"` or `"not_installed"`. The third state the
    /// desktop renders (`"installing"`) is the CALLER's overlay — see
    /// the module header.
    pub status: String,
    /// Chunk count when installed; `None` otherwise.
    pub chunks_count: Option<u64>,
    /// Whether the recipe asked for the epistemic enrichment phase.
    pub enrichment_enabled: bool,
    pub indexed_at: Option<u64>,
    pub embedding_model: Option<String>,
    pub embedding_dimensions: Option<usize>,
    /// Whether IVF-PQ vector search is live for this corpus. On-disk
    /// meta first, the store's flag as the fallback — the two-source
    /// rule the desktop documented and this carries down whole.
    pub vector_index_ready: bool,
    /// Installed but its index never finished building: it answers
    /// ~nothing at query time and the badge should say "needs
    /// rebuild", not present it as healthy.
    pub needs_rebuild: bool,
    pub registry_url: Option<String>,
    pub schema_version: Option<u32>,
    pub parent_corpus_id: Option<String>,
    pub catalog_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogResponse {
    pub corpora: Vec<CatalogEntry>,
}

/// One row on the Library shelf — the desktop's `NotebookSummary`,
/// assembled where its five sources live.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookRow {
    /// Corpus id — the citation handle, structurally unique.
    pub id: String,
    /// The local-corpus display name, else the catalogue name, else
    /// the on-disk index name, else the id.
    pub name: String,
    /// `"folder"` | `"obsidian"` | `"watched"` | `"catalog"` |
    /// `"installed"`.
    pub source_kind: String,
    pub doc_count: u64,
    /// An `atoms.json` atlas with at least one atom, a collection
    /// parent of one, or conv-tiered enrichment.
    pub explorable: bool,
    pub updated_unix: Option<u64>,
    /// `"local"` | `"mesh"` | `"public"`.
    pub scope: String,
    /// Open (unadjudicated) conflicts for a governance corpus.
    /// `None` for an ordinary corpus — which is what gates the
    /// Conflicts tab off; `Some(0)` still shows it.
    pub open_conflicts: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookListResponse {
    pub notebooks: Vec<NotebookRow>,
}

/// Enrichment health for one installed corpus, loaded on demand —
/// opening every index on every catalogue read is what this shape
/// exists to avoid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusHealth {
    pub corpus_id: String,
    pub claims_count: u64,
    pub relationships_count: u64,
    pub has_article_profiles: bool,
    /// Chunks whose enrichment parse failed and can be retried
    /// without re-running inference.
    pub parse_failure_count: u64,
}

/// `POST /{corpus}/retry-enrichment`'s body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryEnrichmentResponse {
    /// Questions recovered by the repair parser this run.
    pub salvaged: u64,
    /// Failures the repair parser still cannot read. Reported beside
    /// `salvaged` because "12 salvaged" and "12 salvaged, 400 still
    /// broken" ask for different next actions (§18.3); the command
    /// this replaces dropped it on the floor.
    pub still_failed: u64,
}

/// `GET /internal/corpus/diagnose`'s body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnoseResponse {
    /// `CorpusEngine::diagnose_indexes`'s report, verbatim.
    pub report: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The catalogue router. Mounted unconditionally on serving daemons;
/// a commission with no `CorpusEngine` answers a named 503, which is
/// a different fact from an unmounted route's 404 (§18.3).
pub fn corpus_catalog_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/corpus/catalog", get(catalog))
        .route("/internal/corpus/notebooks", get(notebooks))
        .route("/internal/corpus/diagnose", get(diagnose))
        .route("/internal/corpus/{corpus}/health", get(health))
        .route(
            "/internal/corpus/{corpus}/coverage-card",
            get(coverage_card),
        )
        .route(
            "/internal/corpus/{corpus}/retry-enrichment",
            post(retry_enrichment),
        )
        .route("/internal/corpus/{corpus}/index/build", post(index_build))
        .route(
            "/internal/corpus/{corpus}/index/progress",
            get(index_progress),
        )
        .localhost_only_with(daemon)
}

// ─── The index build job ───────────────────────────────────────

/// One build's live state, held per corpus in [`INDEX_BUILDS`]. The
/// progress route reads it; the spawned build writes it. In-process on
/// purpose, like `lc_http`'s cluster jobs: the index it narrates is this
/// daemon's, and a log that outlived the daemon would describe a build
/// that may not have finished.
struct IndexBuild {
    job_id: String,
    pct: AtomicU64,
    outcome: Mutex<Option<Result<(), String>>>,
}

static INDEX_BUILDS: OnceLock<Mutex<HashMap<String, Arc<IndexBuild>>>> = OnceLock::new();

fn index_builds() -> &'static Mutex<HashMap<String, Arc<IndexBuild>>> {
    INDEX_BUILDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn index_build_for(corpus_id: &str) -> Option<Arc<IndexBuild>> {
    index_builds()
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(corpus_id).cloned())
}

impl IndexBuild {
    fn progress(&self, corpus_id: &str) -> IndexBuildProgress {
        let outcome = self.outcome.lock().ok().and_then(|o| o.clone());
        let (state, error) = match outcome {
            None => (IndexBuildState::Building, None),
            Some(Ok(())) => (IndexBuildState::Complete, None),
            Some(Err(e)) => (IndexBuildState::Error, Some(e)),
        };
        IndexBuildProgress {
            corpus_id: corpus_id.to_string(),
            job_id: self.job_id.clone(),
            state,
            pct: self.pct.load(Ordering::SeqCst),
            error,
        }
    }
}

/// POST `/internal/corpus/{corpus}/index/build` — build the vector + FTS
/// indexes of an installed corpus as a JOB; answers `IngestJobAck` with
/// `202 Accepted`. Wire form of the desktop's `build_corpus_index`, which
/// opened the index with its own `CorpusEngine` until 2026-09-11.
///
/// Both flags are passed `true` so the builder respects the recipe's own
/// enable flags; forcing FTS off here marked it built without building
/// it (the desktop's comment at the old site records that corruption).
/// On success the state store's `vector_index_ready` flips, which is what
/// `GET /internal/corpus/catalog` reports.
///
/// A second build on a corpus still building is refused by name, not
/// queued: two concurrent writers on one LanceDB index is the failure a
/// refusal is cheaper than.
async fn index_build(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus_id): Path<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let indexes = engine
        .installed_indexes()
        .await
        .map_err(|e| Absence::unavailable(format!("installed_indexes: {e}")))?;
    let Some(info) = indexes.iter().find(|i| i.corpus_id == corpus_id) else {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            &format!("no installed corpus `{corpus_id}`"),
        ));
    };
    if let Some(live) = index_build_for(&corpus_id) {
        if live.outcome.lock().ok().is_some_and(|o| o.is_none()) {
            return Ok(json_error(
                StatusCode::CONFLICT,
                &format!("`{corpus_id}` is already building (job {})", live.job_id),
            ));
        }
    }

    let job_id = format!("index-build-{}", uuid::Uuid::new_v4());
    let job = Arc::new(IndexBuild {
        job_id: job_id.clone(),
        pct: AtomicU64::new(0),
        outcome: Mutex::new(None),
    });
    if let Ok(mut jobs) = index_builds().lock() {
        jobs.insert(corpus_id.clone(), Arc::clone(&job));
    }
    tracing::info!(
        corpus_id = %corpus_id,
        job_id = %job_id,
        path = %info.path.display(),
        "corpus_catalog_http: index build accepted",
    );

    let store = daemon.state_store().map(Arc::clone);
    let path = info.path.clone();
    let spawn_corpus = corpus_id.clone();
    tokio::spawn(async move {
        let result: Result<(), String> = async {
            let idx = engine
                .open_index(&path)
                .await
                .map_err(|e| format!("open_index: {e}"))?;
            let pct = Arc::clone(&job);
            let on_progress: Box<dyn Fn(u64, u64) + Send + Sync> = Box::new(move |done, total| {
                let p = if total > 0 { done * 100 / total } else { 0 };
                pct.pct.store(p, Ordering::SeqCst);
            });
            idx.build_indexes(true, true, Some(&*on_progress))
                .await
                .map_err(|e| format!("build_indexes: {e}"))?;
            if let Some(store) = store {
                if let Err(e) = store.set_vector_index_ready(&spawn_corpus, true).await {
                    // The index IS built; the catalog flag is the part that
                    // failed. Reported as the build's error rather than
                    // swallowed, because a catalog that says "not ready"
                    // over a ready index is the operator's next mystery.
                    return Err(format!("set_vector_index_ready: {e}"));
                }
            }
            Ok(())
        }
        .await;
        match &result {
            Ok(()) => {
                job.pct.store(100, Ordering::SeqCst);
                tracing::info!(
                    corpus_id = %spawn_corpus,
                    job_id = %job.job_id,
                    "corpus_catalog_http: index build complete",
                );
            }
            Err(e) => tracing::warn!(
                corpus_id = %spawn_corpus,
                job_id = %job.job_id,
                error = %e,
                "corpus_catalog_http: index build failed",
            ),
        }
        if let Ok(mut o) = job.outcome.lock() {
            *o = Some(result);
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(IngestJobAck {
            progress_route: format!("/internal/corpus/{corpus_id}/index/progress"),
            corpus_id,
            job_id,
            ok: true,
        }),
    )
        .into_response())
}

/// GET `/internal/corpus/{corpus}/index/progress` — where the build
/// stands. `Idle` for a corpus nobody asked to build in this daemon's
/// lifetime (a 200, not a 404: the corpus exists, the build does not).
async fn index_progress(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus_id): Path<String>,
) -> Result<Response, Absence> {
    let progress = match index_build_for(&corpus_id) {
        Some(job) => job.progress(&corpus_id),
        None => {
            let engine = engine_for(&daemon)?;
            let known = engine
                .installed_indexes()
                .await
                .map_err(|e| Absence::unavailable(format!("installed_indexes: {e}")))?
                .iter()
                .any(|i| i.corpus_id == corpus_id);
            if !known {
                return Ok(json_error(
                    StatusCode::NOT_FOUND,
                    &format!("no installed corpus `{corpus_id}`"),
                ));
            }
            IndexBuildProgress {
                corpus_id: corpus_id.clone(),
                job_id: String::new(),
                state: IndexBuildState::Idle,
                pct: 0,
                error: None,
            }
        }
    };
    tracing::debug!(
        corpus_id = %corpus_id,
        state = ?progress.state,
        pct = progress.pct,
        "corpus_catalog_http: index progress served",
    );
    Ok(Json(progress).into_response())
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/internal/corpus/catalog` — the built-in catalogue UNIONED
/// with every installed index the catalogue does not name.
///
/// The union is the contract, not an optimisation: emitting only
/// built-ins made a recipe-installed, CLI-acquired or snapshot-restored
/// corpus report as MISSING while fully present on disk, and the
/// mesh-app "Get data" flow then re-staged it and polled forever for a
/// row that could never appear. Those rows carry
/// `catalog_status: "hidden"` so they satisfy an installed-status check
/// without crowding the picker's "Coming soon" rail.
async fn catalog(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let builtins = engine.builtin_corpora();
    // Non-fatal, deliberately: the picker still renders so the user
    // can choose what to INSTALL when the indexes dir is unreadable.
    // The catalogue half of the answer does not depend on it.
    let installed = engine.installed_indexes().await.unwrap_or_default();
    let store = daemon.state_store().map(Arc::clone);

    let mut corpora = Vec::new();
    for b in &builtins {
        let registry_entry = engine.registry().find_entry(&b.id);
        let info = installed
            .iter()
            .find(|i| i.corpus_id == b.id && !i.is_shard);
        // Two historical sources for "is vector search live", easy to
        // drift: the on-disk meta the ingest writes, and the store flag
        // only the explicit build command sets. A plain ingest builds
        // the index and never writes the flag, so the meta wins and the
        // flag is the fallback for installs that predate the field.
        let vector_index_ready = match info {
            Some(i) if i.vector_index_built => true,
            Some(_) => match &store {
                Some(s) => s.get_vector_index_ready(&b.id).await.unwrap_or(false),
                None => false,
            },
            None => false,
        };
        corpora.push(CatalogEntry {
            id: b.id.clone(),
            name: b.name.clone(),
            description: b.description.clone(),
            size_compressed_gb: b.size_compressed_gb,
            size_indexed_gb: b.size_indexed_gb,
            license: b.license.clone(),
            tiers: tiers_for(&b.id),
            status: if info.is_some() {
                "installed"
            } else {
                "not_installed"
            }
            .to_string(),
            chunks_count: info.map(|i| i.chunk_count),
            enrichment_enabled: registry_entry
                .map(|e| e.enrichment_enabled)
                .unwrap_or(false),
            indexed_at: info.map(|i| i.created_at),
            embedding_model: info.map(|i| i.embedding_model.clone()),
            embedding_dimensions: info.map(|i| i.embedding_dimensions),
            vector_index_ready,
            needs_rebuild: info.is_some_and(|i| !i.indexes_built),
            registry_url: registry_entry.map(|e| e.toml_url.clone()),
            schema_version: Some(1),
            parent_corpus_id: b.parent_corpus_id.clone(),
            catalog_status: b.catalog_status.clone(),
        });
    }

    let builtin_ids: HashSet<&str> = builtins.iter().map(|b| b.id.as_str()).collect();
    for info in &installed {
        if info.is_shard || builtin_ids.contains(info.corpus_id.as_str()) {
            continue;
        }
        corpora.push(CatalogEntry {
            id: info.corpus_id.clone(),
            name: if info.corpus_name.is_empty() {
                info.corpus_id.clone()
            } else {
                info.corpus_name.clone()
            },
            description: String::new(),
            size_compressed_gb: 0.0,
            size_indexed_gb: info.index_size_bytes as f64 / 1e9,
            license: String::new(),
            tiers: tiers_for(&info.corpus_id),
            status: "installed".to_string(),
            chunks_count: Some(info.chunk_count),
            // "the recipe asked for enrichment" is the best available
            // answer to the badge's question for a corpus with no
            // catalogue entry. The wire name stays `enrichment_enabled`
            // (the frontend and its e2e specs read it); only the
            // engine-side field was renamed `enrichment_requested`.
            enrichment_enabled: info.enrichment_requested,
            indexed_at: Some(info.created_at),
            embedding_model: Some(info.embedding_model.clone()),
            embedding_dimensions: Some(info.embedding_dimensions),
            vector_index_ready: info.vector_index_built,
            needs_rebuild: !info.indexes_built,
            registry_url: None,
            schema_version: Some(1),
            parent_corpus_id: info.parent_corpus_id.clone(),
            catalog_status: Some("hidden".to_string()),
        });
    }

    tracing::debug!(
        builtins = builtins.len(),
        installed_on_disk = installed.len(),
        rows = corpora.len(),
        "corpus_catalog_http: catalogue ∪ installed served"
    );
    Ok(Json(CatalogResponse { corpora }).into_response())
}

/// GET `/internal/corpus/notebooks` — the unified Library shelf.
///
/// Five sources, one row type. Shards and layer children never appear:
/// a shard is a storage internal and a layer folds under its parent,
/// exactly as the picker hides it.
///
/// The local-corpus registry read is the one that is NOT best-effort.
/// A missing registry used to produce an empty map that reads exactly
/// like "no local corpora are registered", so every vault lost its
/// source-kind, its display name and its scope and rendered as a
/// catalogue row with no error anywhere (§18.3). It is a 503 here: a
/// notebook list that cannot say which rows are yours is not a
/// notebook list.
async fn notebooks(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let manager = match crate::watched_folder_runtime::manager() {
        Some(m) => m,
        None => {
            return Ok(json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "local-corpus runtime not installed on this daemon, so the shelf cannot \
                 say which notebooks are yours",
            ))
        }
    };

    let installed = engine.installed_indexes().await.unwrap_or_default();
    let builtins = engine.builtin_corpora();
    let builtin_names: HashMap<&str, &str> = builtins
        .iter()
        .map(|b| (b.id.as_str(), b.name.as_str()))
        .collect();
    let local_configs: HashMap<String, LocalCorpusConfig> = manager
        .list()
        .await
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();

    // Explorable — the union of three best-effort things: a corpus
    // whose own atlas holds at least one atom; a COLLECTION parent
    // whose map lives in `<id>-<slug>` member indexes (SEP: one atlas
    // per entry); a corpus with conv-tiered enrichment.
    //
    // The ATOM COUNT is the gate, not the presence of an `atlas/`
    // dir: `sep/atlas/atoms.json` is a 44-byte `{"atoms":[]}`, and
    // counting that as explorable shipped the Explore tab straight
    // into "No atoms match the current filter" with nothing to match.
    let mut explorable: HashSet<String> = HashSet::new();
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    if let Ok(atom_corpora) = reader.list_corpora().await {
        let notebook_ids: HashSet<&str> = installed
            .iter()
            .filter(|i| !i.is_shard && i.parent_corpus_id.is_none())
            .map(|i| i.corpus_id.as_str())
            .collect();
        for c in &atom_corpora {
            if c.total_atoms == 0 {
                continue;
            }
            explorable.insert(c.corpus_id.clone());
            if let Some(parent) = collection_parent_of(&c.corpus_id, &notebook_ids) {
                explorable.insert(parent.to_string());
            }
        }
    }
    if let Some(runtime) = daemon.runtime() {
        if let Some(conv) = runtime.lane_sources.conv_tiered.as_ref() {
            if let Ok(buckets) = conv.list_conv_corpora_with_state_buckets().await {
                explorable.extend(buckets.into_iter().map(|(corpus_id, ..)| corpus_id));
            }
        }
    }

    // Open-conflict counts for governance corpora. Cheap gate: stat
    // the oplog per corpus, and only for the (typically one) corpus
    // that has it load the view. Off the async executor; a read
    // failure omits the count rather than failing the shelf.
    let open_conflicts: HashMap<String, u32> = {
        let index_dir = engine.index_dir().to_path_buf();
        let ids: Vec<String> = installed
            .iter()
            .filter(|i| !i.is_shard && i.parent_corpus_id.is_none())
            .map(|i| i.corpus_id.clone())
            .collect();
        tokio::task::spawn_blocking(move || {
            let mut counts = HashMap::new();
            for id in ids {
                let atlas = index_dir.join(&id).join("atlas");
                if !atlas.join("governance_oplog.jsonl").exists() {
                    continue;
                }
                if let Ok(view) = corpus_engine::enrichment::GovernanceView::from_atlas_dir(&atlas)
                {
                    counts.insert(id, view.open_tensions().count() as u32);
                }
            }
            counts
        })
        .await
        .unwrap_or_default()
    };

    let mut notebooks = Vec::new();
    for info in &installed {
        if info.is_shard || info.parent_corpus_id.is_some() {
            continue;
        }
        let local = local_configs.get(&info.corpus_id);
        let source_kind = if let Some(cfg) = local {
            match &cfg.source_type {
                LocalCorpusSourceType::ObsidianVault { .. } => "obsidian",
                LocalCorpusSourceType::WatchedFolder(_) => "watched",
                LocalCorpusSourceType::DocumentFolder => "folder",
            }
        } else if builtin_names.contains_key(info.corpus_id.as_str()) {
            "catalog"
        } else {
            // Recipe-installed, CLI-acquired, snapshot-restored,
            // mesh-app or conversation import — a real notebook with
            // no local-folder config behind it.
            "installed"
        };
        let name = if let Some(cfg) = local {
            cfg.display_name.clone()
        } else if let Some(n) = builtin_names.get(info.corpus_id.as_str()) {
            n.to_string()
        } else if !info.corpus_name.is_empty() {
            info.corpus_name.clone()
        } else {
            info.corpus_id.clone()
        };
        notebooks.push(NotebookRow {
            id: info.corpus_id.clone(),
            name,
            source_kind: source_kind.to_string(),
            doc_count: info.chunk_count,
            explorable: explorable.contains(&info.corpus_id),
            updated_unix: Some(info.created_at),
            scope: local
                .map(|c| c.scope.as_recipe_str().to_string())
                .unwrap_or_else(|| "local".to_string()),
            open_conflicts: open_conflicts.get(&info.corpus_id).copied(),
        });
    }

    // Most-recently-indexed first, then alphabetical — a stable,
    // scannable order that floats fresh ingests to the top.
    notebooks.sort_by(|a, b| {
        b.updated_unix
            .cmp(&a.updated_unix)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    tracing::debug!(
        installed_on_disk = installed.len(),
        notebooks = notebooks.len(),
        local_configs = local_configs.len(),
        explorable = explorable.len(),
        "corpus_catalog_http: unified Library shelf served"
    );
    Ok(Json(NotebookListResponse { notebooks }).into_response())
}

/// GET `/internal/corpus/diagnose` — the engine's own indexes-dir
/// report, verbatim.
async fn diagnose(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let report = engine.diagnose_indexes().await;
    tracing::debug!(
        bytes = report.len(),
        "corpus_catalog_http: diagnosis served"
    );
    Ok(Json(DiagnoseResponse { report }).into_response())
}

/// GET `/internal/corpus/{corpus}/health` — enrichment health for one
/// installed corpus.
///
/// 404 for a corpus whose index will not open. The command it replaces
/// answered `Ok(None)`, which the detail panel rendered as "no
/// enrichment data" — the same shape as an installed corpus that has
/// simply never been enriched. Two different facts (§18.3).
async fn health(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let index = match engine.open_index_for_corpus(&corpus).await {
        Ok(i) => i,
        Err(e) => {
            return Ok(json_error(
                StatusCode::NOT_FOUND,
                &format!("no index for corpus '{corpus}' opened on this daemon: {e}"),
            ))
        }
    };
    let failures_path = index.path().join("_skeleton_failures.ndjson");
    let parse_failure_count = if failures_path.exists() {
        std::fs::read_to_string(&failures_path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0)
    } else {
        0
    };
    // One load, two answers. The command loaded the skeleton TWICE —
    // once to test presence, once to count — which is two chances for
    // the disk to answer differently within one response.
    let skeleton = index.load_field_skeleton().ok().flatten();
    let has_article_profiles = skeleton.is_some();
    let claims_count = skeleton
        .map(|s| s.canonical_questions.len() as u64)
        .unwrap_or(0);
    tracing::debug!(
        %corpus,
        claims_count,
        parse_failure_count,
        "corpus_catalog_http: corpus health served"
    );
    Ok(Json(CorpusHealth {
        corpus_id: corpus,
        claims_count,
        // No relationships table is read today; `0` is what the
        // command reported and this does not invent a number for it.
        relationships_count: 0,
        has_article_profiles,
        parse_failure_count,
    })
    .into_response())
}

/// GET `/internal/corpus/{corpus}/coverage-card` — the typed
/// authoritative-store card, for a corpus whose recipe declares one.
///
/// `card: null` is a 200, not a 404: "this corpus declares no typed
/// store" is an ANSWER the detail pane branches on to show nothing,
/// and a 404 would collapse it into "no such corpus" (§18.3).
///
/// Discovery and content both come from `sec_facts` — the SAME
/// `authoritative_store` accessor the `sec_facts` tool resolves
/// through and the SAME `coverage_card` derivation — so the card
/// cannot advertise a corpus or a period the tool would refuse.
async fn coverage_card(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    use corpus_engine::enrichment::atlas::analysis::sec_facts::{
        authoritative_store, coverage_card as derive_card,
    };
    let engine = engine_for(&daemon)?;
    let card = authoritative_store(engine.index_dir(), engine.recipes_dir(), &corpus)
        .map(|store| derive_card(&store));
    tracing::debug!(
        target: "sec_facts",
        %corpus,
        declared = card.is_some(),
        "corpus_catalog_http: coverage card served"
    );
    Ok(Json(CoverageCardResponse { card }).into_response())
}

/// `GET /{corpus}/coverage-card`'s body. An object, not a bare
/// `Option`, so `null` arrives under a named key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageCardResponse {
    pub card: Option<corpus_engine::enrichment::atlas::analysis::sec_facts::CoverageCard>,
}

/// POST `/internal/corpus/{corpus}/retry-enrichment` — re-parse stored
/// skeleton extraction failures with the repair parser.
///
/// No inference: only the saved raw responses are re-processed, and
/// salvaged questions merge into the existing `field_skeleton.json`.
async fn retry_enrichment(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let index = match engine.open_index_for_corpus(&corpus).await {
        Ok(i) => i,
        Err(e) => {
            return Ok(json_error(
                StatusCode::NOT_FOUND,
                &format!("no index for corpus '{corpus}' opened on this daemon: {e}"),
            ))
        }
    };
    Ok(match corpus_engine::reprocess_skeleton_failures(&index) {
        Ok((salvaged, still_failed)) => {
            tracing::info!(
                %corpus,
                salvaged,
                still_failed,
                "corpus_catalog_http: skeleton failure reprocessing complete"
            );
            Json(RetryEnrichmentResponse {
                salvaged: salvaged as u64,
                still_failed: still_failed as u64,
            })
            .into_response()
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("reprocessing failures for '{corpus}': {e}"),
        ),
    })
}

// ─── Helpers ───────────────────────────────────────────────────

/// Which installed notebook, if any, owns `corpus_id` as a MEMBER
/// atlas — the collection relationship behind SEP's `sep-<slug>`
/// per-article maps.
///
/// Walks the id's hyphen boundaries and takes the LONGEST prefix that
/// names an installed notebook, so `sep-logic-modal` resolves to `sep`
/// and never to a shorter accidental match.
fn collection_parent_of<'a>(corpus_id: &'a str, notebook_ids: &HashSet<&str>) -> Option<&'a str> {
    corpus_id
        .match_indices('-')
        .rev()
        .map(|(idx, _)| &corpus_id[..idx])
        .find(|prefix| notebook_ids.contains(prefix))
}

/// Which setup-wizard tiers offer this corpus.
///
/// The §2.1 smell, carried down verbatim — see the module header for
/// why it moved as-is and where it belongs.
fn tiers_for(corpus_id: &str) -> Vec<String> {
    match corpus_id {
        // Wikipedia Core ships in every tier — its scoped 100K + Vital
        // Articles is the baseline general-knowledge corpus.
        "wikipedia" | "wikipedia-simple" => vec![
            "essential".into(),
            "research".into(),
            "technical".into(),
            "full".into(),
        ],
        "sep" | "openalex" | "crs_reports" => vec!["research".into(), "full".into()],
        "stackexchange" => vec!["technical".into(), "full".into()],
        "gutenberg" => vec!["full".into()],
        _ => Vec::new(),
    }
}

/// The daemon's own `CorpusEngine`. One lookup site, so no handler can
/// read a different index dir than the one an ingest writes to.
fn engine_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<CorpusEngine>, Absence> {
    daemon.corpus_engine().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable(
            "this daemon holds no CorpusEngine (it was commissioned to serve nothing)",
        )
    })
}
