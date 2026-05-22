//! Daemon-side SCIP rebuild pipeline.
//!
//! Each registered project gets one [`ProjectHandle`], running a
//! single supervised task that reacts to four freshness signals
//! (FS change, git HEAD drift, lazy MCP nudge, explicit/startup)
//! and dispatches to a coalescing rebuild worker. Signals call
//! [`ProjectHandle::nudge`]; the worker observes them through the
//! [`tokio::sync::watch`] channel on the handle.
//!
//! The primitives the worker relies on:
//! - [`ScipGraph::try_rebuild_lock`] — cross-process flock so a
//!   stray `sovereign project refresh` can't race with the daemon.
//! - [`ScipGraph::open_with_integrity`] — post-rebuild open on the
//!   freshly-renamed DB; quarantines corruption and triggers the
//!   next rebuild if the schema drifts.
//! - [`corpus_engine::scip_export::export_all`] — the per-language
//!   exporter dispatch that already exists.
//!
//! The rebuild itself writes to `scip_graph.db.new` then renames
//! over `scip_graph.db`, so in-flight MCP calls holding an
//! `Arc<ScipGraph>` continue to see the old graph. After the
//! rename, the worker opens the new file fresh and swaps it into
//! the project's [`ScipGraphHandle`] (an `ArcSwap`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use corpus_engine::ScipGraph;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tokio::sync::{mpsc, watch, RwLock, Semaphore};

use tokio::sync::oneshot;

use crate::projects::{ProjectEntry, ProjectState, WatcherKind, WatcherStatus};

/// An `ArcSwap`-backed handle the daemon and MCP tools share to
/// access a project's SCIP graph. Matches the type alias used by
/// the existing tool crates (`sovereign_tools::ScipGraphHandle`).
pub type ScipGraphHandle = Arc<ArcSwap<ScipGraph>>;

/// Why a rebuild was enqueued. Surfaced in logs and persisted into
/// `scip_meta.last_trigger_reason` so `sovereign project watch
/// status` can tell the operator "last rebuild fired from FS event
/// at 15:02:17". Purely observability; the rebuild pipeline does
/// not branch on the reason.
#[derive(Debug, Clone)]
pub enum RebuildReason {
    Startup,
    FsChange,
    GitHead { old: String, new: String },
    Lazy,
    Explicit,
}

impl RebuildReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::FsChange => "fs_change",
            Self::GitHead { .. } => "git_poll",
            Self::Lazy => "lazy",
            Self::Explicit => "explicit",
        }
    }
}

/// One registered project's runtime state. The daemon holds one
/// per registered project; dropping it (or calling
/// [`abort`](Self::abort)) stops the worker loop and releases the
/// FS watcher.
///
/// Note on isolation: the Reindexer worker handles *our* code
/// (SCIP export, DB rename, git poll) — not user-authored scripts.
/// A panic inside it is a bug we want to see, not a fault to
/// auto-recover from, so we don't wrap it in a
/// [`crate::supervised_task::SupervisedTask`]. Test/lint runners
/// (the user-script surface) are supervised separately in a later
/// step.
pub struct ProjectHandle {
    pub entry: ProjectEntry,
    pub state: Arc<ProjectState>,
    pub graph: ScipGraphHandle,
    /// Most-recent rebuild request, observed by the worker loop
    /// via `watch::Receiver::changed`. Signal producers call
    /// [`nudge`](Self::nudge).
    pub rebuild_tx: watch::Sender<Option<RebuildRequest>>,
    shutdown: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl ProjectHandle {
    /// Ask the worker to perform a rebuild. Coalesced via the
    /// watch channel — multiple rapid nudges collapse to at most
    /// one extra rebuild beyond the one in flight.
    pub fn nudge(&self, reason: RebuildReason) {
        let req = RebuildRequest {
            reason,
            enqueued_at: Instant::now(),
        };
        // Mark dirty unconditionally. If a rebuild is already in
        // flight the worker reads the dirty bit after the current
        // cycle; otherwise the watch channel wakes it immediately.
        self.state.mark_dirty();
        let _ = self.rebuild_tx.send(Some(req));
    }

    /// Stop the worker cooperatively. Idempotent.
    pub fn abort(&self) {
        if let Ok(mut slot) = self.shutdown.lock() {
            if let Some(tx) = slot.take() {
                let _ = tx.send(());
            }
        }
    }
}

impl Drop for ProjectHandle {
    fn drop(&mut self) {
        // Best-effort cooperative shutdown. We also abort the task
        // handle as a safety net in case the shutdown oneshot was
        // already consumed.
        self.abort();
        self.worker.abort();
    }
}

/// One request in flight or parked in the coalescer.
#[derive(Debug, Clone)]
pub struct RebuildRequest {
    pub reason: RebuildReason,
    pub enqueued_at: Instant,
}

/// Top-level object owned by the daemon. Tracks every registered
/// project and the merged SCIP graph that tools query.
pub struct Reindexer {
    indexes_dir: PathBuf,
    projects: RwLock<HashMap<String, Arc<ProjectHandle>>>,
    merged: ScipGraphHandle,
    /// Phase 7.1 commit harvester. Set by the daemon via
    /// [`Reindexer::with_commit_harvester`] when a NoteStore is
    /// available. When `None`, the git-poll path skips harvesting
    /// — production daemons configure it; minimal test setups
    /// don't have to.
    commit_harvester: Option<Arc<corpus_engine::NoteStore>>,
    /// Global serializer for `rust-analyzer scip` invocations across
    /// every registered project. SCIP export is an
    /// O(workspace-size) cargo + rust-analyzer pass — running four
    /// of them in parallel (one per registered project) saturated
    /// 4 cores at daemon startup, blocked anything else trying to
    /// use the build cache, and kept the user staring at idle UIs.
    /// One permit means at most one project rebuilds at a time;
    /// the others queue on the semaphore. Cross-project parallelism
    /// gave zero throughput because they all hit the same cargo
    /// target dir anyway. Shared via Arc so it can be cloned into
    /// every `WorkerCtx` without leaking the Reindexer Arc.
    rebuild_permits: Arc<Semaphore>,
}

impl Reindexer {
    /// `indexes_dir` is the parent directory of per-project SCIP
    /// DBs (`~/.sovereign/indexes/` in production). `merged` is a
    /// pre-existing in-memory graph handle that the daemon hands
    /// to MCP tools; after each successful rebuild, the project's
    /// new graph is imported into this merged handle.
    pub fn new(indexes_dir: PathBuf, merged: ScipGraphHandle) -> Arc<Self> {
        Arc::new(Self {
            indexes_dir,
            projects: RwLock::new(HashMap::new()),
            merged,
            commit_harvester: None,
            rebuild_permits: Arc::new(Semaphore::new(1)),
        })
    }

    /// Configure the commit-message harvester (Phase 7.1). When
    /// set, the worker's git-HEAD poll harvests non-noisy commit
    /// messages between `old_head..new_head` and writes them as
    /// `source='committed'` notes via the supplied store.
    ///
    /// `Arc::get_mut` ordering: callers must invoke this BEFORE
    /// the Reindexer is shared (cloning it after this returns
    /// `None` from `get_mut`). The daemon's startup wires the
    /// harvester immediately after `Reindexer::new` and before
    /// handing the Arc to anything else.
    pub fn with_commit_harvester(
        self: &mut Arc<Self>,
        notes: Arc<corpus_engine::NoteStore>,
    ) {
        if let Some(inner) = Arc::get_mut(self) {
            inner.commit_harvester = Some(notes);
        } else {
            tracing::error!(
                "Reindexer::with_commit_harvester: Arc already shared; \
                 harvester not configured. Call this before sharing the \
                 Reindexer handle."
            );
        }
    }

    /// Register or update a project. Idempotent: re-registering
    /// the same `corpus_id` replaces the handle in place,
    /// aborting the old supervisor first.
    pub async fn register(&self, entry: ProjectEntry) -> Arc<ProjectHandle> {
        let corpus_id = entry.corpus_id.clone();
        let state = ProjectState::new(&corpus_id);

        // Open the per-project graph, migrating or quarantining as
        // needed. Any OpenError triggers the first rebuild via the
        // Startup signal below — we spawn the worker with a fresh
        // in-memory graph as a placeholder so tools don't race.
        let db_path = self
            .indexes_dir
            .join(&corpus_id)
            .join("scip_graph.db");
        let initial_graph = match ScipGraph::open_with_integrity(&db_path, &corpus_id) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "open_with_integrity failed on register; placeholder graph + rebuild scheduled"
                );
                ScipGraph::open_in_memory(&corpus_id)
                    .expect("in-memory fallback graph")
            }
        };
        let graph: ScipGraphHandle = Arc::new(ArcSwap::from_pointee(initial_graph));

        let (rebuild_tx, rebuild_rx) = watch::channel::<Option<RebuildRequest>>(None);

        // FS events land in this mpsc; the worker select!s over
        // rebuild_rx and fs_rx. Bounded at 256 to absorb typical
        // save storms without blocking the notify thread.
        let (fs_tx, fs_rx) = mpsc::channel::<Event>(256);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let ctx = WorkerCtx {
            entry: entry.clone(),
            state: Arc::clone(&state),
            graph: Arc::clone(&graph),
            merged: Arc::clone(&self.merged),
            rebuild_rx,
            fs_rx,
            fs_tx: fs_tx.clone(),
            indexes_dir: self.indexes_dir.clone(),
            shutdown_rx,
            commit_harvester: self.commit_harvester.clone(),
            rebuild_permits: Arc::clone(&self.rebuild_permits),
        };

        let worker = tokio::spawn(run_worker(ctx));

        // Kick the startup catch-up signal — but only if the on-disk
        // graph isn't already current. Cold-starting four corpora and
        // rebuilding all of them OOM'd the box once: the daemon
        // restart for a model swap fired Startup on every project
        // even though nothing had changed since the last rebuild
        // five minutes earlier. The git_poll loop already does the
        // right freshness check (HEAD drift + working-tree state);
        // mirror that decision here so the startup catch-up only
        // fires when there's something to catch up on.
        if needs_startup_rebuild(&entry, &graph.load()).await {
            let _ = rebuild_tx.send(Some(RebuildRequest {
                reason: RebuildReason::Startup,
                enqueued_at: Instant::now(),
            }));
        } else {
            tracing::info!(
                corpus = %corpus_id,
                "startup rebuild skipped — graph fresh at HEAD with clean working tree"
            );
        }

        let handle = Arc::new(ProjectHandle {
            entry,
            state,
            graph,
            rebuild_tx,
            shutdown: std::sync::Mutex::new(Some(shutdown_tx)),
            worker,
        });

        let mut projects = self.projects.write().await;
        if let Some(old) = projects.insert(corpus_id.clone(), Arc::clone(&handle)) {
            // Abort the previous worker so the new one owns the
            // project exclusively. The Drop impl also covers this,
            // but explicit intent beats relying on drop order.
            old.abort();
        }
        handle
    }

    /// Remove a project. Aborts its supervisor; the returned
    /// `ProjectHandle` (if any) is dropped by the caller, which
    /// releases FS watcher resources.
    pub async fn unregister(&self, corpus_id: &str) -> Option<Arc<ProjectHandle>> {
        let removed = self.projects.write().await.remove(corpus_id);
        if let Some(ref h) = removed {
            h.abort();
        }
        removed
    }

    pub async fn get(&self, corpus_id: &str) -> Option<Arc<ProjectHandle>> {
        self.projects.read().await.get(corpus_id).cloned()
    }

    pub async fn snapshot(&self) -> Vec<Arc<ProjectHandle>> {
        self.projects.read().await.values().cloned().collect()
    }

    /// Shared merged graph the MCP tool layer queries.
    pub fn merged_graph(&self) -> ScipGraphHandle {
        Arc::clone(&self.merged)
    }
}

// ─── Worker ──────────────────────────────────────────────────

struct WorkerCtx {
    entry: ProjectEntry,
    state: Arc<ProjectState>,
    graph: ScipGraphHandle,
    merged: ScipGraphHandle,
    rebuild_rx: watch::Receiver<Option<RebuildRequest>>,
    fs_rx: mpsc::Receiver<Event>,
    /// Held alive so FS watcher doesn't shut down when the outer
    /// scope drops the builder's copy. The worker itself doesn't
    /// send on this — the notify callback does.
    #[allow(dead_code)]
    fs_tx: mpsc::Sender<Event>,
    indexes_dir: PathBuf,
    shutdown_rx: oneshot::Receiver<()>,
    /// Phase 7.1: optional commit-message harvester. When set, the
    /// git-HEAD poll calls into [`crate::commit_harvest`] for the
    /// `old_head..new_head` range alongside the SCIP rebuild.
    commit_harvester: Option<Arc<corpus_engine::NoteStore>>,
    /// Cross-project rebuild serializer. See [`Reindexer::rebuild_permits`].
    rebuild_permits: Arc<Semaphore>,
}

/// Per-rebuild context. Separated from [`WorkerCtx`] because the
/// select! loop keeps a mutable `rebuild_rx` / `fs_rx` / shutdown
/// channel, and we don't want `execute_rebuild` to see any of
/// those — it's a pure transformation over (entry, graphs, state).
#[derive(Clone)]
struct RebuildCtx {
    entry: ProjectEntry,
    state: Arc<ProjectState>,
    graph: ScipGraphHandle,
    merged: ScipGraphHandle,
    indexes_dir: PathBuf,
    /// Acquired around the rust-analyzer scip subprocess inside
    /// `run_one_rebuild` so cross-project rebuilds serialize. See
    /// [`Reindexer::rebuild_permits`].
    rebuild_permits: Arc<Semaphore>,
}

async fn run_worker(ctx: WorkerCtx) {
    let WorkerCtx {
        entry,
        state,
        graph,
        merged,
        mut rebuild_rx,
        mut fs_rx,
        fs_tx: _fs_tx,
        indexes_dir,
        shutdown_rx,
        commit_harvester,
        rebuild_permits,
    } = ctx;

    state.set(WatcherKind::Scip, WatcherStatus::Idle).await;
    let mut shutdown = shutdown_rx;

    let rebuild_ctx = RebuildCtx {
        entry: entry.clone(),
        state: Arc::clone(&state),
        graph: Arc::clone(&graph),
        merged,
        indexes_dir,
        rebuild_permits,
    };

    // Filesystem watcher. Held for the lifetime of the task so
    // the notify backend is released when the worker exits.
    let _fs_watcher = match start_fs_watcher(&entry.root, _fs_tx) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(
                corpus = %entry.corpus_id,
                error = %e,
                "fs watcher failed to start; falling back to git-poll + lazy signals only"
            );
            None
        }
    };

    let debounce = Duration::from_millis(entry.watchers.scip_debounce_ms.max(50));
    let git_poll_secs = entry.watchers.git_poll_secs;
    let mut git_poll_ticker = if git_poll_secs > 0 {
        let mut t = tokio::time::interval(Duration::from_secs(git_poll_secs));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Some(t)
    } else {
        None
    };

    let mut pending_fs = false;
    let mut last_fs_event: Option<Instant> = None;

    loop {
        let debounce_sleep = match (pending_fs, last_fs_event) {
            (true, Some(when)) => {
                let elapsed = when.elapsed();
                if elapsed >= debounce {
                    Duration::from_millis(0)
                } else {
                    debounce - elapsed
                }
            }
            _ => Duration::from_secs(3600),
        };

        tokio::select! {
            biased;
            _ = &mut shutdown => {
                state.set(WatcherKind::Scip, WatcherStatus::Aborted).await;
                return;
            }
            changed = rebuild_rx.changed() => {
                if changed.is_err() {
                    return;
                }
                let snapshot = rebuild_rx.borrow_and_update().clone();
                if let Some(req) = snapshot {
                    run_one_rebuild(&rebuild_ctx, req).await;
                }
            }
            maybe_evt = fs_rx.recv() => {
                if maybe_evt.is_some() {
                    pending_fs = true;
                    last_fs_event = Some(Instant::now());
                }
                // `None` = watcher task dropped the sender;
                // git_poll + lazy + explicit keep the worker useful.
            }
            _ = async {
                if let Some(ref mut t) = git_poll_ticker {
                    t.tick().await;
                } else {
                    futures::future::pending::<()>().await;
                }
            } => {
                if let Some(new_head) = read_git_head(&entry.root) {
                    let old_head = rebuild_ctx
                        .graph
                        .load()
                        .last_indexed_head()
                        .await
                        .unwrap_or_default();
                    if old_head != new_head {
                        // Phase 7.1: harvest commit messages
                        // between old_head..new_head into
                        // `source='committed'` notes BEFORE the
                        // SCIP rebuild kicks off. Order doesn't
                        // matter for correctness — both are
                        // independent — but doing it first means
                        // the next `sovereign audit` after the
                        // rebuild completes already has the new
                        // committed-source rows.
                        if let Some(notes) = commit_harvester.as_deref() {
                            let session_id = format!(
                                "harvest-{}",
                                rebuild_ctx.entry.corpus_id
                            );
                            let wrote = crate::commit_harvest::harvest_between(
                                &entry.root,
                                &old_head,
                                &new_head,
                                notes,
                                &session_id,
                            )
                            .await;
                            if wrote > 0 {
                                tracing::info!(
                                    corpus_id = %rebuild_ctx.entry.corpus_id,
                                    new_notes = wrote,
                                    "commit_harvest: persisted committed-source notes"
                                );
                            }
                        }
                        let req = RebuildRequest {
                            reason: RebuildReason::GitHead {
                                old: old_head,
                                new: new_head,
                            },
                            enqueued_at: Instant::now(),
                        };
                        run_one_rebuild(&rebuild_ctx, req).await;
                    }
                }
            }
            _ = tokio::time::sleep(debounce_sleep) => {
                if pending_fs {
                    pending_fs = false;
                    last_fs_event = None;
                    let req = RebuildRequest {
                        reason: RebuildReason::FsChange,
                        enqueued_at: Instant::now(),
                    };
                    run_one_rebuild(&rebuild_ctx, req).await;
                }
            }
        }
    }
}

async fn run_one_rebuild(ctx: &RebuildCtx, req: RebuildRequest) {
    if !ctx.state.begin_rebuild() {
        ctx.state.mark_dirty();
        return;
    }

    ctx.state.set(WatcherKind::Scip, WatcherStatus::Active).await;
    let start = Instant::now();
    // Cross-project semaphore — at most one project rebuilds at a
    // time, monorepo-wide. SCIP export is a full cargo + rust-
    // analyzer pass; running four in parallel saturates the
    // machine and gives zero speed-up because they all share one
    // target dir. Permit acquire is async and cancellable; on
    // shutdown the worker tasks abort cleanly without leaking the
    // permit (the SemaphorePermit RAII guard drops with the
    // future). Errors here mean the semaphore was closed —
    // impossible in normal flow, but we degrade to "rebuild
    // without serialization" rather than skip the rebuild
    // entirely.
    let _permit = match ctx.rebuild_permits.acquire().await {
        Ok(p) => Some(p),
        Err(_) => {
            tracing::warn!(
                corpus = %ctx.entry.corpus_id,
                "rebuild_permits closed; falling back to unserialized rebuild"
            );
            None
        }
    };
    let outcome = execute_rebuild(ctx, &req).await;
    match &outcome {
        Ok(summary) => {
            tracing::info!(
                corpus = %ctx.entry.corpus_id,
                reason = %req.reason.as_str(),
                elapsed_ms = start.elapsed().as_millis() as u64,
                symbols = summary.symbols,
                refs = summary.refs,
                "scip rebuild complete"
            );
        }
        Err(e) => {
            tracing::warn!(
                corpus = %ctx.entry.corpus_id,
                reason = %req.reason.as_str(),
                error = %e,
                "scip rebuild failed"
            );
        }
    }
    ctx.state.set(WatcherKind::Scip, WatcherStatus::Idle).await;

    // Observe dirty bit — if more signals fired during the
    // rebuild, loop into one more pass. Bounded: any further
    // requests after this second pass arrive via rebuild_rx on
    // the next select! iteration.
    if ctx.state.end_rebuild() {
        let follow = RebuildRequest {
            reason: RebuildReason::Explicit,
            enqueued_at: Instant::now(),
        };
        Box::pin(run_one_rebuild(ctx, follow)).await;
    }
}

/// A thin summary returned by [`execute_rebuild`]. Not the same as
/// [`corpus_engine::scip_export::ExportSummary`] — we keep this
/// lean so the worker doesn't depend on the exporter's schema
/// during unit tests that stub the rebuild body.
#[derive(Debug, Serialize)]
pub struct RebuildSummary {
    pub symbols: usize,
    pub refs: usize,
    pub languages: Vec<String>,
    pub skipped: Vec<String>,
}

async fn execute_rebuild(
    ctx: &RebuildCtx,
    req: &RebuildRequest,
) -> Result<RebuildSummary, String> {
    let corpus_id = ctx.entry.corpus_id.clone();
    let live_path = ctx
        .indexes_dir
        .join(&corpus_id)
        .join("scip_graph.db");
    let db_dir = live_path
        .parent()
        .ok_or_else(|| "db path has no parent".to_string())?
        .to_path_buf();
    std::fs::create_dir_all(&db_dir).map_err(|e| format!("mkdir {}: {e}", db_dir.display()))?;

    // Cross-process flock. Holders keep the .rebuild.lock file;
    // we drop the guard at scope exit which releases the kernel
    // lock and lets a follow-up rebuild proceed.
    let _lock = match ScipGraph::try_rebuild_lock(&db_dir)
        .map_err(|e| format!("acquire rebuild lock: {e}"))?
    {
        Some(lock) => lock,
        None => {
            // Another writer holds the lock — coalesce by marking
            // dirty so our worker picks it up once they're done.
            ctx.state.mark_dirty();
            return Err("another writer holds the rebuild lock".into());
        }
    };

    // Build the new graph in a staging DB so the live file stays
    // intact for concurrent readers until the rename commits.
    let staging_path = live_path.with_file_name(format!(
        "{}.new",
        live_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("scip_graph.db")
    ));
    let _ = std::fs::remove_file(&staging_path);

    let new_graph = ScipGraph::open_with_integrity(&staging_path, &corpus_id)
        .map_err(|e| format!("open staging: {e}"))?;

    let tempdir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let export_out = tempdir.path().join("scip");

    let workspace_roots: Option<Vec<PathBuf>> = None; // auto-detect
    // The exporter wants a `&dyn Fn(ScipProgress)`; use a plain
    // function (Send + Sync by default) rather than a closure, so
    // the containing async body stays Send.
    fn silent_progress(_p: corpus_engine::scip_export::ScipProgress<'_>) {}
    let summary = corpus_engine::scip_export::export_all(
        &ctx.entry.root,
        &export_out,
        &new_graph,
        workspace_roots.as_deref(),
        &silent_progress,
    )
    .await
    .map_err(|e| format!("scip_export::export_all: {e}"))?;

    // Gather structured outcomes per language (Succeeded /
    // Skipped) for the status surface. We reconstruct from the
    // summary since `export_all` doesn't return a per-exporter
    // outcome list directly yet.
    let outcomes_json = serde_json::json!({
        "succeeded": summary.languages_exported,
        "skipped": summary
            .languages_skipped
            .iter()
            .map(|s| serde_json::json!({
                "language": s.language,
                "reason": s.reason,
            }))
            .collect::<Vec<_>>(),
    })
    .to_string();

    let head = read_git_head(&ctx.entry.root);
    new_graph
        .record_rebuild(
            req.reason.as_str(),
            head.as_deref(),
            Some(&outcomes_json),
        )
        .await;

    // Close the staging connection before rename so Windows and
    // some macOS filesystems don't complain about active fds. On
    // POSIX the rename works either way; explicit drop is cheap.
    drop(new_graph);

    std::fs::rename(&staging_path, &live_path)
        .map_err(|e| format!("rename {} → {}: {e}", staging_path.display(), live_path.display()))?;

    // Re-open from the renamed live path, swap into the handle.
    let live = ScipGraph::open_with_integrity(&live_path, &corpus_id)
        .map_err(|e| format!("open live after rename: {e}"))?;
    ctx.graph.store(Arc::new(live));
    ctx.state.mark_graph_updated();

    // Merge into the daemon-wide graph so tools querying the
    // merged handle see the refreshed symbols. Best-effort —
    // merge failure doesn't invalidate the per-project rebuild.
    if let Err(e) = ctx.merged.load().import_from_path(&live_path).await {
        tracing::warn!(
            corpus = %corpus_id,
            error = %e,
            "merged graph import failed after successful rebuild"
        );
    }

    Ok(RebuildSummary {
        symbols: summary.total_symbols,
        refs: summary.total_refs,
        languages: summary.languages_exported,
        skipped: summary
            .languages_skipped
            .into_iter()
            .map(|s| s.language)
            .collect(),
    })
}

// ─── Signal primitives ───────────────────────────────────────

fn start_fs_watcher(
    root: &Path,
    tx: mpsc::Sender<Event>,
) -> notify::Result<RecommendedWatcher> {
    let root = root.to_path_buf();
    let filter = build_ignore_filter(&root);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else {
            return;
        };
        // Only forward events we care about — Create/Modify/Remove
        // on source files. Access/Open/Close spam gets dropped at
        // the watcher seam so the worker's channel buffer lasts.
        if !is_source_event(&event) {
            return;
        }
        if event.paths.iter().all(|p| filter.is_ignored(p)) {
            return;
        }
        // Fire-and-forget: if the channel is full the rebuild is
        // already pending. Dropping the event is safe because the
        // debouncer re-fires on the next tick regardless of how
        // many we've seen.
        let _ = tx.try_send(event);
    })?;

    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

fn is_source_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

struct IgnoreFilter {
    matcher: Option<ignore::gitignore::Gitignore>,
}

impl IgnoreFilter {
    fn is_ignored(&self, path: &Path) -> bool {
        // Hard-exclude a small set of directories even without a
        // .gitignore. These are never source-relevant in any
        // language we support, and on macOS `node_modules/` alone
        // can easily push 100k events/sec during `npm install`.
        const HARD_EXCLUDE: &[&str] = &[
            ".git",
            "node_modules",
            "target",
            "dist",
            "build",
            ".cache",
            ".next",
            "__pycache__",
            ".venv",
            "venv",
        ];
        if path
            .components()
            .any(|c| HARD_EXCLUDE.contains(&c.as_os_str().to_string_lossy().as_ref()))
        {
            return true;
        }
        if let Some(m) = &self.matcher {
            if m.matched(path, path.is_dir()).is_ignore() {
                return true;
            }
        }
        // Only fire on known language extensions. Keeps the worker
        // from spinning on README edits that don't affect SCIP.
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        !matches!(
            ext.as_str(),
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "java"
        )
    }
}

fn build_ignore_filter(root: &Path) -> IgnoreFilter {
    let gitignore = root.join(".gitignore");
    let matcher = if gitignore.exists() {
        let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
        let _ = builder.add(&gitignore);
        builder.build().ok()
    } else {
        None
    };
    IgnoreFilter { matcher }
}

/// Decide whether the startup catch-up rebuild should fire for
/// `entry`. Returns `true` (must rebuild) whenever any freshness
/// signal is missing or stale; returns `false` only when we have
/// positive evidence the on-disk graph already reflects the current
/// source tree.
///
/// Conservative on purpose: any uncertainty (no git, no recorded
/// HEAD, can't run `git status`) returns `true` so a misconfigured
/// project still gets indexed once on startup. The win is the
/// common case — daemon restart for an inference-side change with
/// nothing touched in the source tree — which used to fan out N
/// full SCIP rebuilds and OOM the box.
async fn needs_startup_rebuild(
    entry: &ProjectEntry,
    graph: &ScipGraph,
) -> bool {
    // 1. The on-disk graph must have a recorded HEAD. Placeholder
    //    in-memory graphs (from a corrupt-DB recovery path) and
    //    legacy DBs without the `last_indexed_head` row return
    //    `None` here → always rebuild.
    let Some(indexed_head) = graph.last_indexed_head().await else {
        return true;
    };
    if indexed_head.is_empty() {
        return true;
    }
    // 2. Current HEAD must match what was indexed.
    let Some(current_head) = read_git_head(&entry.root) else {
        // No git — we have no cheap freshness primitive. Rebuild
        // to stay safe; non-git projects are rare on this daemon.
        return true;
    };
    if current_head != indexed_head {
        return true;
    }
    // 3. Working tree must be clean. The git_poll signal only
    //    catches HEAD drift, so without this check uncommitted
    //    edits made while the daemon was down would never trigger
    //    a startup rebuild. `git status --porcelain` is cheap (a
    //    single git invocation, no diff content) and reports both
    //    modified-tracked and untracked files.
    if working_tree_dirty(&entry.root) {
        return true;
    }
    false
}

/// `true` iff `git status --porcelain` produces any output.
/// Failure to spawn or non-zero exit returns `true` (treat as
/// dirty) so a broken git invocation doesn't silently skip a
/// rebuild we needed to do.
fn working_tree_dirty(root: &Path) -> bool {
    let out = std::process::Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(root)
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => true,
    }
}

/// Read the current `HEAD` SHA of the repository at `root`.
/// `None` when `root` isn't a git repository or `git` isn't on
/// PATH (in which case the git-poll signal becomes a no-op and
/// freshness falls back to FS + lazy).
pub fn read_git_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::WatcherToggles;

    fn sample_entry(id: &str, root: PathBuf) -> ProjectEntry {
        ProjectEntry {
            corpus_id: id.into(),
            root,
            registered_at: "2026-04-17T00:00:00Z".into(),
            watchers: WatcherToggles {
                scip_debounce_ms: 30,
                git_poll_secs: 0,
                ..WatcherToggles::default()
            },
        }
    }

    #[test]
    fn reason_string_mapping_is_stable() {
        assert_eq!(RebuildReason::Startup.as_str(), "startup");
        assert_eq!(RebuildReason::FsChange.as_str(), "fs_change");
        assert_eq!(
            RebuildReason::GitHead {
                old: "a".into(),
                new: "b".into()
            }
            .as_str(),
            "git_poll"
        );
        assert_eq!(RebuildReason::Lazy.as_str(), "lazy");
        assert_eq!(RebuildReason::Explicit.as_str(), "explicit");
    }

    #[test]
    fn ignore_filter_excludes_hard_excludes_and_non_source_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        let filter = build_ignore_filter(tmp.path());
        assert!(filter.is_ignored(&tmp.path().join("target/debug/foo.rs")));
        assert!(filter.is_ignored(&tmp.path().join("node_modules/x/index.js")));
        assert!(filter.is_ignored(&tmp.path().join("README.md")));
        assert!(filter.is_ignored(&tmp.path().join("docs/.git/HEAD")));
        assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
        assert!(!filter.is_ignored(&tmp.path().join("app/server.ts")));
    }

    #[test]
    fn ignore_filter_honours_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "secret.rs\n").unwrap();
        let filter = build_ignore_filter(tmp.path());
        assert!(filter.is_ignored(&tmp.path().join("secret.rs")));
        assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
    }

    #[test]
    fn read_git_head_returns_none_for_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_git_head(tmp.path()).is_none());
    }

    /// Initialise a git repo with one commit; return (entry, current_head).
    /// Used by the `needs_startup_rebuild` cases below.
    fn init_repo_with_commit(corpus_id: &str) -> (tempfile::TempDir, ProjectEntry, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("git invocation");
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(root.join("src.rs"), "fn main() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);
        let head = read_git_head(root).expect("HEAD after commit");
        let entry = sample_entry(corpus_id, root.to_path_buf());
        (tmp, entry, head)
    }

    #[tokio::test]
    async fn needs_startup_rebuild_true_when_graph_has_no_recorded_head() {
        let (_tmp, entry, _head) = init_repo_with_commit("no-head");
        // Fresh in-memory graph — never had `record_rebuild` called,
        // so `last_indexed_head()` returns None.
        let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
        assert!(needs_startup_rebuild(&entry, &graph).await);
    }

    #[tokio::test]
    async fn needs_startup_rebuild_true_when_root_is_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = sample_entry("no-git", tmp.path().to_path_buf());
        let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
        // Pretend the graph was indexed at some prior HEAD, but the
        // directory isn't a git repo — we can't verify freshness.
        graph
            .record_rebuild("startup", Some("deadbeef"), None)
            .await;
        assert!(needs_startup_rebuild(&entry, &graph).await);
    }

    #[tokio::test]
    async fn needs_startup_rebuild_true_when_head_drifted() {
        let (_tmp, entry, _head) = init_repo_with_commit("drift");
        let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
        graph
            .record_rebuild("startup", Some("0000000000000000000000000000000000000000"), None)
            .await;
        assert!(needs_startup_rebuild(&entry, &graph).await);
    }

    #[tokio::test]
    async fn needs_startup_rebuild_true_when_working_tree_dirty() {
        let (tmp, entry, head) = init_repo_with_commit("dirty");
        let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
        graph.record_rebuild("startup", Some(&head), None).await;
        // Touch a tracked file so `git status --porcelain` is non-empty.
        std::fs::write(tmp.path().join("src.rs"), "fn main() { let _ = (); }\n").unwrap();
        assert!(needs_startup_rebuild(&entry, &graph).await);
    }

    #[tokio::test]
    async fn needs_startup_rebuild_false_when_head_matches_and_tree_clean() {
        let (_tmp, entry, head) = init_repo_with_commit("fresh");
        let graph = ScipGraph::open_in_memory(&entry.corpus_id).unwrap();
        graph.record_rebuild("startup", Some(&head), None).await;
        assert!(
            !needs_startup_rebuild(&entry, &graph).await,
            "graph indexed at current HEAD with clean tree must not trigger a rebuild"
        );
    }

    #[tokio::test]
    async fn nudge_sets_dirty_flag_on_project_state() {
        // Verify via ProjectState directly — ProjectHandle's
        // worker task is hard to isolate in a unit test (it tries
        // to spawn a FS watcher + run exporters), and nudge() is
        // pure state manipulation.
        let state = ProjectState::new("test");
        state.mark_dirty(); // simulates what nudge() does internally
        assert!(state.end_rebuild(), "dirty bit should be observable");
    }

    #[tokio::test]
    async fn register_then_unregister_cleans_up_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path().join("indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        let merged = Arc::new(ArcSwap::from_pointee(
            ScipGraph::open_in_memory("merged").unwrap(),
        ));
        let reindexer = Reindexer::new(indexes.clone(), merged);

        let entry = sample_entry("probe", tmp.path().to_path_buf());
        let _h = reindexer.register(entry.clone()).await;

        assert!(reindexer.get("probe").await.is_some());

        reindexer.unregister("probe").await;
        assert!(reindexer.get("probe").await.is_none());
    }
}
