// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! - [`corpus_engine_scip::scip_export::export_all`] — the per-language
//!   exporter dispatch that already exists.
//!
//! The rebuild itself writes to `scip_graph.db.new` then renames
//! over `scip_graph.db`, so in-flight MCP calls holding an
//! `Arc<ScipGraph>` continue to see the old graph. After the
//! rename, the worker opens the new file fresh and swaps it into
//! the project's [`ScipGraphHandle`] (an `ArcSwap`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use corpus_engine_scip::ScipGraph;
use futures::future::BoxFuture;
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
    commit_harvester: Option<Arc<corpus_engine_notes::NoteStore>>,
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
    /// DBs (`~/.svrnmesh/indexes/` in production). `merged` is a
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
    pub fn with_commit_harvester(self: &mut Arc<Self>, notes: Arc<corpus_engine_notes::NoteStore>) {
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
        let db_path = self.indexes_dir.join(&corpus_id).join("scip_graph.db");
        let initial_graph = match ScipGraph::open_with_integrity(&db_path, &corpus_id) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "open_with_integrity failed on register; placeholder graph + rebuild scheduled"
                );
                ScipGraph::open_in_memory(&corpus_id).expect("in-memory fallback graph")
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
    commit_harvester: Option<Arc<corpus_engine_notes::NoteStore>>,
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

/// How long the demoted full rust-analyzer export stays suppressed after it
/// last FINISHED, while FS saves keep arriving. The tree-sitter overlay keeps
/// symbol defs fresh in this window; only the precise cross-file edges wait,
/// and a commit refreshes precisely and immediately (git-HEAD path, ungated).
///
/// Two properties this constant must hold, both learned the hard way:
///
/// 1. It is measured from COMPLETION, not spawn (see [`RebuildRunGuard`]).
/// 2. It must EXCEED a real export, or the gate reopens before the exporter
///    has released and the machine never gets the pause back.
///
/// At 300s-from-spawn it satisfied neither: measured exports on this monorepo
/// run 257-498s, so the gate reopened mid-export and continuous editing pinned
/// rust-analyzer at a ~88-90% duty cycle holding ~14GB (measured 2026-08-16 —
/// starts every ~6min, each running ~5.3min, on a box whose swap was
/// exhausted). The doc comment claimed "at most once per five minutes of
/// active editing"; the constants delivered "continuously".
///
/// 900s from completion bounds the worst case at roughly one export per 20
/// minutes of unbroken editing (~25% duty cycle), which is what makes the
/// watcher affordable to leave on.
const FULL_REBUILD_COOLDOWN: Duration = Duration::from_secs(900);

/// How long the FS-save stream must stay quiet before a due full export
/// actually launches. The export is a multi-minute whole-workspace
/// rust-analyzer pass; starting it between two keystrokes contends with
/// the operator's own builds mid-flow. Gating on a short quiescence
/// window shifts it into the natural pauses. Commit-triggered and
/// startup/explicit rebuilds are NOT gated — a commit is the operator
/// saying "done here".
const FULL_REBUILD_QUIESCENCE: Duration = Duration::from_secs(30);

/// Upper bound on how long the quiescence gate may defer a due full
/// export during continuous editing. Without a cap, a long uninterrupted
/// editing session would starve precise cross-file edges indefinitely
/// (the tree-sitter overlay only keeps symbol *defs* fresh).
const FULL_REBUILD_MAX_DEFER: Duration = Duration::from_secs(600);

/// Spawn the heavy full rust-analyzer rebuild as a DETACHED task instead of
/// awaiting it inline in the select loop. This is what keeps the overlay (and
/// FS-event collection) responsive while a multi-minute export runs — the loop
/// must never block on rust-analyzer, or "fresh during work" breaks and every
/// rebuild is a blackout. `in_flight` guards against stacking concurrent
/// rebuilds for the same project: if one is already running, we coalesce by
/// marking the project dirty and returning — the overlay keeps symbol defs fresh
/// in the meantime, so a deferred full rebuild only delays precise cross-file
/// edges, never symbol existence. Returns whether a rebuild was actually
/// spawned (so the caller can stamp its cooldown only when one started).
/// Upper bound on how long one spawned rebuild (including its
/// follow-up passes) may run before the watchdog declares it wedged.
/// Generous — measured exports on this monorepo run ~5 min warm —
/// while still converting a hung task into a loud recorded failure
/// instead of an eternal Active status. The primary wedge (the
/// follow-up permit self-deadlock, see `run_one_rebuild_with`) is
/// fixed structurally; this is defense-in-depth for residual hangs
/// (e.g. a rust-analyzer subprocess that never exits).
const MAX_REBUILD_WALL: Duration = Duration::from_secs(45 * 60);

/// RAII guard for the rebuild slot. Clears `in_flight` and the
/// `ProjectState` rebuild claim on ANY exit path — panic, watchdog
/// abort, or normal completion. Without this a task that dies
/// without running `end_rebuild` leaves the project wedged forever:
/// every later signal coalesces into a silent no-op (the live wedge
/// of 2026-08-14: status "active" for hours, zero failures recorded).
struct RebuildRunGuard {
    in_flight: Arc<AtomicBool>,
    state: Arc<ProjectState>,
    finished: FinishClock,
}

impl Drop for RebuildRunGuard {
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::Release);
        self.state.force_clear_rebuild();
        // The cooldown clock starts HERE — when the exporter released
        // the machine — not when it was spawned. Stamping at spawn
        // with a cooldown shorter than the export let the gate reopen
        // mid-export; see `FULL_REBUILD_COOLDOWN`. Drop runs on every
        // exit path, so a panicked or watchdog-aborted export (which
        // still spiked the machine) also starts a full cooldown.
        stamp_finished(&self.finished);
    }
}

/// The cooldown clock for the full rust-analyzer export: when the
/// exporter last RELEASED the machine. Shared between the watch loop
/// (which reads it to decide whether an export is due) and the
/// detached rebuild task (which stamps it on every exit path).
///
/// A plain `std::sync::Mutex` because [`RebuildRunGuard::drop`] is
/// synchronous; it is held for a single store and never across an
/// await.
type FinishClock = Arc<std::sync::Mutex<Option<Instant>>>;

/// Stamp the cooldown clock at "now".
fn stamp_finished(clock: &FinishClock) {
    if let Ok(mut slot) = clock.lock() {
        *slot = Some(Instant::now());
    }
}

/// Is a full rust-analyzer export due, given when one last finished?
///
/// ONE implementation of this threshold — the watch loop asks in two
/// places (arming the quiescence gate, and re-checking after the gate
/// opens) and both must agree (§10.6).
fn full_export_due(last_finished: Option<Instant>, cooldown: Duration) -> bool {
    last_finished.is_none_or(|t| t.elapsed() >= cooldown)
}

/// The rebuild body as an injectable plain `fn` pointer so unit tests
/// can substitute a fake export (real `execute_rebuild` needs
/// rust-analyzer + a cargo workspace; the loop's permit/panic
/// semantics are what the wedge tests pin). A function item — not a
/// closure — so the pointer is `'static` and `Copy`: the watchdog
/// task can own it outright with no borrow that outlives the caller.
type RebuildBody =
    for<'a> fn(&'a RebuildCtx, &'a RebuildRequest) -> BoxFuture<'a, Result<RebuildSummary, String>>;

fn execute_rebuild_boxed<'a>(
    ctx: &'a RebuildCtx,
    req: &'a RebuildRequest,
) -> BoxFuture<'a, Result<RebuildSummary, String>> {
    Box::pin(execute_rebuild(ctx, req))
}

fn spawn_full_rebuild(
    ctx: &RebuildCtx,
    req: RebuildRequest,
    in_flight: &Arc<AtomicBool>,
    finished: &FinishClock,
) -> bool {
    spawn_full_rebuild_with(
        ctx,
        req,
        in_flight,
        finished,
        execute_rebuild_boxed,
        MAX_REBUILD_WALL,
    )
}

/// The body of [`spawn_full_rebuild`] with the rebuild body and the
/// watchdog wall-clock injectable (tests use a short wall).
fn spawn_full_rebuild_with(
    ctx: &RebuildCtx,
    req: RebuildRequest,
    in_flight: &Arc<AtomicBool>,
    finished: &FinishClock,
    body: RebuildBody,
    wall: Duration,
) -> bool {
    // Acquire the single-rebuild slot; if already taken, coalesce.
    if in_flight.swap(true, Ordering::AcqRel) {
        ctx.state.mark_dirty();
        return false;
    }
    let ctx = ctx.clone();
    let in_flight = Arc::clone(in_flight);
    let finished = Arc::clone(finished);
    tokio::spawn(async move {
        // Clears both slots on every exit path (see `RebuildRunGuard`).
        let _guard = RebuildRunGuard {
            in_flight: Arc::clone(&in_flight),
            state: Arc::clone(&ctx.state),
            finished: Arc::clone(&finished),
        };
        // Detach the body into its own task so a panic surfaces as a
        // JoinError we can name, and the watchdog can abort it. All
        // inputs are owned (`RebuildCtx` is Clone; `RebuildBody` is a
        // `'static` fn pointer), so the inner task needs no borrows.
        let inner = tokio::spawn(run_one_rebuild_with(ctx.clone(), req, body));
        match tokio::time::timeout(wall, inner).await {
            Ok(Ok(())) => {}
            Ok(Err(join_err)) => {
                // A panicked rebuild task. WITHOUT this branch the
                // project stayed "active" forever and every later
                // nudge coalesced into a silent no-op — the wedge.
                let reason = format!("scip rebuild task panicked: {join_err}");
                tracing::error!(
                    corpus = %ctx.entry.corpus_id,
                    error = %reason,
                    "WEDGE GUARD: rebuild task panicked; flags cleared so the next signal can retry"
                );
                ctx.state.record_rebuild_failure(&reason).await;
                ctx.graph.load().record_rebuild_failure(&reason).await;
                ctx.state
                    .set(
                        WatcherKind::Scip,
                        WatcherStatus::Crashed {
                            reason: reason.clone(),
                            count: ctx.state.rebuild_failure_count() as usize,
                        },
                    )
                    .await;
                append_watcher_log(&ctx, &format!("rebuild PANICKED: {join_err}"));
            }
            Err(_) => {
                // The watchdog: a rebuild that never completes within
                // `wall` is a wedge, not a slow export. Abort it, say
                // so in the daemon log (the order's wedge-detection
                // line), and clear the flags so the next git poll /
                // nudge retries rather than coalescing forever.
                let reason = format!(
                    "rebuild active for {:?} without completing (wedged); aborted by the watchdog",
                    wall
                );
                tracing::error!(
                    corpus = %ctx.entry.corpus_id,
                    error = %reason,
                    "WEDGE GUARD: rebuild exceeded the wall clock; aborted — flags cleared so the next signal can retry"
                );
                ctx.state.record_rebuild_failure(&reason).await;
                ctx.graph.load().record_rebuild_failure(&reason).await;
                ctx.state
                    .set(
                        WatcherKind::Scip,
                        WatcherStatus::Crashed {
                            reason: reason.clone(),
                            count: ctx.state.rebuild_failure_count() as usize,
                        },
                    )
                    .await;
                append_watcher_log(&ctx, &format!("rebuild WEDGED: exceeded {:?}", wall));
            }
        }
    });
    true
}

/// Append one line to the per-watcher log file the CLI's
/// `svrn project watch logs <id> scip` reads
/// (`~/.svrnmesh/logs/watch-{corpus}-scip.log`). Best-effort: a
/// missing or unwritable logs dir must never fail a rebuild. The
/// path derives from `indexes_dir`'s parent because production logs
/// live next to the indexes under `~/.svrnmesh/`.
fn append_watcher_log(ctx: &RebuildCtx, line: &str) {
    use std::io::Write;
    let Some(root) = ctx.indexes_dir.parent() else {
        return;
    };
    // `<root>/logs/watch-{corpus}-scip.log` — the same path the CLI's
    // `project watch logs <id> scip` reads
    // (`sovereign-cli/src/project_registry.rs::cmd_watch_logs`; it lived in
    // `cli-dev`'s `registry_watch.rs` until that unreachable fork was
    // deleted, nc-27 2026-08-21). The
    // `logs` segment is the contract; without it the daemon wrote
    // `<root>/watch-...` and the CLI's promise went unkept (watched
    // failing in the full suite 2026-08-14).
    let path = root
        .join("logs")
        .join(format!("watch-{}-scip.log", ctx.entry.corpus_id));
    if let Err(e) = std::fs::create_dir_all(path.parent().unwrap_or(root)) {
        tracing::debug!(error = %e, "watcher log dir create failed");
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Overlay merge (structural watcher hot path): re-index the changed *source*
/// files with tree-sitter and merge their symbol definitions into `graph` via
/// [`ScipGraph::replace_file_symbols`]. Embed-free, no rust-analyzer, no
/// blocking on the inference slots. Returns `(files_merged, symbols_merged)`.
///
/// - Non-source paths (no tree-sitter pack) are skipped — the full export owns
///   them.
/// - A changed source file that can't be read (deleted/renamed-away) is still
///   passed to `replace_file_symbols` with no symbols, so its stale defs are
///   dropped.
/// A merge failure is logged and swallowed: the overlay is best-effort freshness
/// and must never wedge the watcher or corrupt the graph (the primitive rolls
/// back on error).
async fn run_overlay_merge(
    merged: &ScipGraphHandle,
    corpus_id: &str,
    indexes_dir: &Path,
    root: &Path,
    changed: &[PathBuf],
) -> (usize, usize) {
    use corpus_engine::facts::{
        extract_facts_for_file, extract_symbol_defs, pack_for_extension, Facts,
    };

    let mut files: Vec<String> = Vec::new();
    let mut symbols = Vec::new();
    // Facts for the changed files — the SAME tree-sitter pass as the symbol
    // defs, so we read each file once and extract both.
    let mut facts = Facts::default();
    for rel in changed {
        // Only files a tree-sitter pack claims; others belong to the full export.
        let is_source = rel
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| pack_for_extension(e).is_some())
            .unwrap_or(false);
        if !is_source {
            continue;
        }
        let rel_str = rel.to_string_lossy().to_string();
        files.push(rel_str.clone());
        // Read from the working tree ONCE; a deleted file simply contributes no
        // records, so the per-file replaces drop its prior rows.
        if let Ok(src) = std::fs::read_to_string(root.join(rel)) {
            symbols.extend(extract_symbol_defs(&rel_str, &src));
            let f = extract_facts_for_file(&rel_str, &src);
            facts.fn_defs.extend(f.fn_defs);
            facts.ctor_fields.extend(f.ctor_fields);
            facts.str_lits.extend(f.str_lits);
        }
    }

    if files.is_empty() {
        return (0, 0);
    }
    let n_files = files.len();
    let n_syms = symbols.len();

    // 1) SCIP symbol defs → the MERGED in-memory graph the daemon's tools query,
    //    scoped to this project's corpus_id — visible to a live `symbols()` call
    //    with no full re-import or restart.
    let g = merged.load();
    if let Err(e) = g.replace_file_symbols_for(corpus_id, &files, symbols).await {
        tracing::warn!(
            error = %e,
            "scip overlay merge failed (graph preserved); full export will correct"
        );
        return (0, 0);
    }

    // 2) Facts → the on-disk per-corpus facts.db the `facts` tool reads. We only
    //    PATCH an existing store; if facts.db isn't built yet we skip (a full
    //    `code facts` run, or the tool's first-read migration of legacy
    //    facts.json, creates it) so the hot path never pays the one-time
    //    whole-repo JSON→SQLite migration inline. WAL makes these writes visible
    //    to the tool's next read immediately — facts go live-fresh per save.
    let facts_db = indexes_dir.join(corpus_id).join("facts.db");
    if facts_db.exists() {
        match corpus_engine::facts_store::FactStore::open(&facts_db) {
            Ok(store) => {
                if let Err(e) = store.replace_files(corpus_id, &files, &facts).await {
                    tracing::warn!(error = %e, "facts overlay merge failed (store preserved)");
                }
            }
            Err(e) => tracing::warn!(error = %e, "facts.db open failed for overlay"),
        }
    }

    (n_files, n_syms)
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
    let _fs_watcher = match start_fs_watcher(&entry.root, &entry.watchers.ignore_paths, _fs_tx) {
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
    // Last save seen, NOT cleared at the debounce flush — feeds the
    // quiescence gate for the demoted full export.
    let mut last_save: Option<Instant> = None;
    // Set when a full export comes due at a debounce flush; the export
    // launches once the save stream has been quiet for
    // FULL_REBUILD_QUIESCENCE, or unconditionally after
    // FULL_REBUILD_MAX_DEFER. Holds the instant it came due.
    let mut export_due_since: Option<Instant> = None;
    // Structural watcher — tree-sitter overlay hot path.
    // `changed_files` accumulates the workspace-relative paths touched since the
    // last debounce flush; on flush we re-parse just those with tree-sitter and
    // merge symbol defs (embed-free, no rust-analyzer) so `symbols()` is fresh
    // in milliseconds. The heavy whole-workspace rust-analyzer export is DEMOTED
    // off the per-save path: it runs on commit (git-HEAD, below) and at most
    // once per `FULL_REBUILD_COOLDOWN` of active editing — this is what removes
    // the per-save memory contention that had the watcher switched off.
    let mut changed_files: HashSet<PathBuf> = HashSet::new();
    // When the full export last RELEASED the machine (not when it was
    // spawned — see `FULL_REBUILD_COOLDOWN`). Stamped by the detached
    // rebuild task's guard on every exit path, read here to gate the
    // next one.
    let rebuild_finished: FinishClock = Arc::new(std::sync::Mutex::new(None));
    let last_finished = || rebuild_finished.lock().ok().and_then(|s| *s);
    // Guards the single detached rust-analyzer rebuild so the select loop never
    // blocks on it (see `spawn_full_rebuild`).
    let rebuild_in_flight = Arc::new(AtomicBool::new(false));

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
        // Second wake source: a quiescence-gated full export. Fires at
        // last_save + QUIESCENCE or export_due_since + MAX_DEFER,
        // whichever comes first.
        let export_sleep = match export_due_since {
            Some(due_at) => {
                let quiet_in = match last_save {
                    Some(s) => FULL_REBUILD_QUIESCENCE.saturating_sub(s.elapsed()),
                    None => Duration::from_millis(0),
                };
                let force_in = FULL_REBUILD_MAX_DEFER.saturating_sub(due_at.elapsed());
                quiet_in.min(force_in)
            }
            None => Duration::from_secs(3600),
        };
        let wake_sleep = debounce_sleep.min(export_sleep);

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
                    // Spawn (don't await) so startup/explicit rebuilds don't
                    // freeze the loop — the overlay must keep serving saves.
                    spawn_full_rebuild(&rebuild_ctx, req, &rebuild_in_flight, &rebuild_finished);
                }
            }
            maybe_evt = fs_rx.recv() => {
                if let Some(evt) = maybe_evt {
                    // Record which files changed (workspace-relative) for the
                    // overlay merge on the next debounce flush. Absolute paths
                    // outside the repo root are ignored defensively.
                    for p in &evt.paths {
                        if let Ok(rel) = p.strip_prefix(&entry.root) {
                            changed_files.insert(rel.to_path_buf());
                        }
                    }
                    pending_fs = true;
                    last_fs_event = Some(Instant::now());
                    last_save = Some(Instant::now());
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
                        // Spawn (don't await): a commit-triggered rebuild must
                        // not block the loop's overlay servicing either.
                        // A commit is the natural precise-refresh boundary and
                        // resets the FS-change cooldown: the graph is being
                        // brought fully current, so no FS-triggered full
                        // export is due until this one finishes and its
                        // cooldown elapses.
                        spawn_full_rebuild(
                            &rebuild_ctx,
                            req,
                            &rebuild_in_flight,
                            &rebuild_finished,
                        );
                    }
                }
            }
            _ = tokio::time::sleep(wake_sleep) => {
                // The timer serves two schedules; check each condition
                // explicitly rather than assuming which one fired.
                let debounce_elapsed = last_fs_event
                    .map(|t| t.elapsed() >= debounce)
                    .unwrap_or(false);
                if pending_fs && debounce_elapsed {
                    pending_fs = false;
                    last_fs_event = None;

                    // 1) Overlay merge — ALWAYS, and first. Cheap (tree-sitter,
                    //    embed-free, no rust-analyzer), so `symbols()` reflects
                    //    added/moved/removed functions within milliseconds of a
                    //    save. Never contends with inference.
                    let files: Vec<PathBuf> = changed_files.drain().collect();
                    let (merged_files, merged_syms) = run_overlay_merge(
                        &rebuild_ctx.merged,
                        &rebuild_ctx.entry.corpus_id,
                        &rebuild_ctx.indexes_dir,
                        &entry.root,
                        &files,
                    )
                    .await;
                    if merged_files > 0 {
                        tracing::debug!(
                            corpus_id = %rebuild_ctx.entry.corpus_id,
                            files = merged_files,
                            symbols = merged_syms,
                            "scip overlay: symbol defs refreshed (tree-sitter, embed-free)"
                        );
                        rebuild_ctx.state.mark_graph_updated();
                    }

                    // 2) Full rust-analyzer export — DEMOTED. Runs at most once
                    //    per FULL_REBUILD_COOLDOWN of active editing (the overlay
                    //    keeps defs fresh in between; precise cross-file edges and
                    //    qualified names lag one cooldown/commit). This is the
                    //    contention fix: whole-workspace rust-analyzer no longer
                    //    fires on every save.
                    // Never arm the gate while an export is already running:
                    // the spawn would only coalesce, and edits landing during
                    // a rebuild are picked up by its follow-up passes.
                    // ASK WHAT CHANGED, NOT JUST WHEN. The export is a
                    // whole-workspace rust-analyzer pass costing ~11 GiB and
                    // minutes; arming it on a cooldown alone meant editing a
                    // .py/.sh/.json/.md file scheduled a RUST export that
                    // could not produce a symbol the graph did not already
                    // have. On 2026-08-26 exactly that fired at 09:56 — from
                    // python and shell edits — beside a 47 GiB judge daemon,
                    // on a host whose measured wall is ~55 GiB. Skipping is
                    // safe: the live graph is untouched and the tree-sitter
                    // overlay above already refreshed symbol defs.
                    let changed_exts: HashSet<String> = files
                        .iter()
                        .filter_map(|p| p.extension().and_then(|e| e.to_str()))
                        .map(|e| e.to_ascii_lowercase())
                        .collect();
                    let learns =
                        corpus_engine_scip::scip_export::changed_extensions_have_exporter(
                            &changed_exts,
                        );
                    if !learns {
                        tracing::debug!(
                            corpus_id = %rebuild_ctx.entry.corpus_id,
                            extensions = ?changed_exts,
                            "full rust-analyzer export NOT armed — no changed file \
                             belongs to an installed SCIP exporter; the overlay \
                             already carries this change set"
                        );
                    }
                    let due = learns
                        && !rebuild_in_flight.load(Ordering::Acquire)
                        && full_export_due(last_finished(), FULL_REBUILD_COOLDOWN);
                    if due {
                        // Don't launch yet — arm the quiescence gate so the
                        // export starts in an editing pause, not between two
                        // keystrokes. Keep the earliest due-instant so
                        // MAX_DEFER caps total deferral, not per-save.
                        if export_due_since.is_none() {
                            export_due_since = Some(Instant::now());
                        }
                    } else {
                        tracing::debug!(
                            corpus_id = %rebuild_ctx.entry.corpus_id,
                            "full rust-analyzer export deferred (within cooldown); overlay keeps defs fresh"
                        );
                    }
                }

                // Quiescence gate for the demoted full export.
                if let Some(due_at) = export_due_since {
                    let quiet = last_save
                        .map(|t| t.elapsed() >= FULL_REBUILD_QUIESCENCE)
                        .unwrap_or(true);
                    let forced = due_at.elapsed() >= FULL_REBUILD_MAX_DEFER;
                    if quiet || forced {
                        export_due_since = None;
                        // Re-check the cooldown: a commit- or explicit-
                        // triggered rebuild may have refreshed the graph
                        // while we waited, making this export redundant.
                        let still_due = full_export_due(last_finished(), FULL_REBUILD_COOLDOWN);
                        if still_due {
                            if forced && !quiet {
                                tracing::debug!(
                                    corpus_id = %rebuild_ctx.entry.corpus_id,
                                    "full export quiescence deferral hit MAX_DEFER; launching despite active editing"
                                );
                            }
                            let req = RebuildRequest {
                                reason: RebuildReason::FsChange,
                                enqueued_at: Instant::now(),
                            };
                            // Spawned, not awaited: the loop stays live for the
                            // next save's overlay while rust-analyzer runs in
                            // the background. The cooldown clock is stamped by
                            // the rebuild task when it finishes, not here.
                            spawn_full_rebuild(
                                &rebuild_ctx,
                                req,
                                &rebuild_in_flight,
                                &rebuild_finished,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Upper bound on follow-up passes per spawned rebuild. Each pass
/// re-checks the dirty bit; a pass that completes with the head
/// unchanged exits the loop (the git poll compares old vs new every
/// cycle), so in practice the loop runs one or two passes. The cap
/// bounds a pathological case (commits landing continuously) —
/// further requests are picked up by the next select! iteration's
/// git poll / rebuild_rx, never lost, just deferred.
const MAX_FOLLOWUP_PASSES: usize = 4;

async fn run_one_rebuild(ctx: &RebuildCtx, req: RebuildRequest) {
    run_one_rebuild_with(ctx.clone(), req, execute_rebuild_boxed).await
}

/// One full rebuild cycle: claim the ProjectState slot, run the
/// rebuild body, then — if signals fired during the pass — run
/// follow-up passes. The follow-up passes run INSIDE the same permit
/// scope: they must never re-acquire it, because the outer call
/// holds the sole cross-project permit and a nested `acquire()`
/// blocks forever, permanently wedging the project (live incident
/// 2026-08-14: status "active" for hours, every later nudge
/// coalescing into a silent no-op — the wedge this order exists
/// to kill). The old code recursed into `run_one_rebuild`, which
/// re-acquired the permit; that is the wedge, structurally.
async fn run_one_rebuild_with(ctx: RebuildCtx, req: RebuildRequest, body: RebuildBody) {
    if !ctx.state.begin_rebuild() {
        ctx.state.mark_dirty();
        return;
    }

    ctx.state
        .set(WatcherKind::Scip, WatcherStatus::Active)
        .await;
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
    append_watcher_log(
        &ctx,
        &format!(
            "rebuild start (reason={}) at {}",
            req.reason.as_str(),
            chrono::Utc::now().to_rfc3339(),
        ),
    );

    let mut passes: usize = 0;
    let mut req = req;
    loop {
        let outcome = body(&ctx, &req).await;
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
                ctx.state.record_rebuild_success().await;
                append_watcher_log(
                    &ctx,
                    &format!(
                        "rebuild complete: {} symbols, {} refs in {}s",
                        summary.symbols,
                        summary.refs,
                        start.elapsed().as_secs(),
                    ),
                );
            }
            Err(e) if e == corpus_engine_scip::REBUILD_COALESCED => {
                // Benign coalescing, not a failure — the holder's rebuild
                // covers this request via the dirty bit.
                tracing::debug!(corpus = %ctx.entry.corpus_id, "scip rebuild coalesced");
                append_watcher_log(
                    &ctx,
                    "rebuild coalesced (another writer holds the rebuild lock)",
                );
            }
            Err(e) => {
                // Record the failure where it is VISIBLE: the in-memory count
                // feeds /v1/projects and `project watch status`; the live
                // graph's scip_meta survives a daemon restart and feeds the
                // daemon-free `project status` + doctor. A deterministic
                // failure repeats every poll cycle, so the log is throttled to
                // the first occurrence and every 10th — the count carries the
                // magnitude the suppressed lines would have.
                let n = ctx.state.record_rebuild_failure(e).await;
                ctx.graph.load().record_rebuild_failure(e).await;
                if n == 1 || n % 10 == 0 {
                    tracing::warn!(
                        corpus = %ctx.entry.corpus_id,
                        reason = %req.reason.as_str(),
                        error = %e,
                        consecutive_failures = n,
                        "scip rebuild failed"
                    );
                } else {
                    tracing::debug!(
                        corpus = %ctx.entry.corpus_id,
                        error = %e,
                        consecutive_failures = n,
                        "scip rebuild failed (throttled)"
                    );
                }
                append_watcher_log(
                    &ctx,
                    &format!("rebuild FAILED: {e} (consecutive failure {n})"),
                );
            }
        }
        passes += 1;
        if passes >= MAX_FOLLOWUP_PASSES {
            break;
        }
        // Observe the dirty bit. If more signals fired during this
        // pass, one more pass — under the SAME permit (see the
        // function doc: re-acquiring self-deadlocks). Further
        // requests after the cap arrive via the next select!
        // iteration's git poll / rebuild_rx.
        if !ctx.state.end_rebuild() {
            break;
        }
        req = RebuildRequest {
            reason: RebuildReason::Explicit,
            enqueued_at: Instant::now(),
        };
    }
    ctx.state.set(WatcherKind::Scip, WatcherStatus::Idle).await;
}

/// A thin summary returned by [`execute_rebuild`]. Not the same as
/// [`corpus_engine_scip::scip_export::ExportSummary`] — we keep this
/// lean so the worker doesn't depend on the exporter's schema
/// during unit tests that stub the rebuild body.
#[derive(Debug, Serialize)]
pub struct RebuildSummary {
    pub symbols: usize,
    pub refs: usize,
    pub languages: Vec<String>,
    pub skipped: Vec<String>,
}

async fn execute_rebuild(ctx: &RebuildCtx, req: &RebuildRequest) -> Result<RebuildSummary, String> {
    let corpus_id = ctx.entry.corpus_id.clone();
    let live_path = ctx.indexes_dir.join(&corpus_id).join("scip_graph.db");

    // The ONE writer protocol (flock → staging → wipe guard →
    // record → atomic rename) lives in corpus-engine-scip so the
    // CLI's `project refresh --local` path uses the SAME writer —
    // before the extraction it opened the live DB directly and
    // collided with this handle ("attempt to write a readonly
    // database", live 2026-08-14).
    fn silent_progress(_p: corpus_engine_scip::scip_export::ScipProgress<'_>) {}
    let live_symbols = ctx.graph.load().symbol_count().await;
    let outcome = ScipGraph::export_to_live(
        &ctx.entry.root,
        None, // auto-detect workspace roots
        &live_path,
        &corpus_id,
        req.reason.as_str(),
        live_symbols,
        &silent_progress,
    )
    .await;
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) if e == corpus_engine_scip::REBUILD_COALESCED => {
            // Another writer holds the lock — coalesce by marking
            // dirty so our worker picks it up once they're done.
            ctx.state.mark_dirty();
            return Err(e);
        }
        Err(e) => return Err(e),
    };

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
        symbols: outcome.summary.total_symbols,
        refs: outcome.summary.total_refs,
        languages: outcome.summary.languages_exported,
        skipped: outcome
            .summary
            .languages_skipped
            .into_iter()
            .map(|s| s.language)
            .collect(),
    })
}

// ─── Signal primitives ───────────────────────────────────────

fn start_fs_watcher(
    root: &Path,
    extra_ignores: &[String],
    tx: mpsc::Sender<Event>,
) -> notify::Result<RecommendedWatcher> {
    let root = root.to_path_buf();
    let filter = build_ignore_filter(&root, extra_ignores);

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
    /// User-configured extras from `WatcherToggles::ignore_paths`.
    /// Matched against any path component, same shape as
    /// `HARD_EXCLUDE`. Empty for projects registered before the
    /// field existed (serde default fills in `.sovereign`).
    extra_ignores: Vec<String>,
}

impl IgnoreFilter {
    fn is_ignored(&self, path: &Path) -> bool {
        // Hard-exclude path components that are never source-relevant in any
        // language we support. The list complements the per-project
        // `.gitignore` matcher below and the user-configurable
        // `WatcherToggles::ignore_paths` — anything matched here is dropped
        // at the watcher seam, before any event reaches the worker.
        //
        // Keep this list tight: only directory names that are universal noise
        // across ecosystems (build outputs, VCS state, dep caches). Anything
        // sovereign-specific or project-specific belongs in `ignore_paths`.
        // On macOS `node_modules/` alone can push 100k events/sec during
        // `npm install`, so this cheap component scan pays for itself.
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
        if path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            HARD_EXCLUDE.contains(&s.as_ref()) || self.extra_ignores.iter().any(|e| e == s.as_ref())
        }) {
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

fn build_ignore_filter(root: &Path, extra_ignores: &[String]) -> IgnoreFilter {
    let gitignore = root.join(".gitignore");
    let matcher = if gitignore.exists() {
        let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
        let _ = builder.add(&gitignore);
        builder.build().ok()
    } else {
        None
    };
    IgnoreFilter {
        matcher,
        extra_ignores: extra_ignores.to_vec(),
    }
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
async fn needs_startup_rebuild(entry: &ProjectEntry, graph: &ScipGraph) -> bool {
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

    // ── Structural watcher overlay (hot-path merge) ──

    fn mem_graph() -> ScipGraphHandle {
        Arc::new(ArcSwap::from_pointee(
            ScipGraph::open_in_memory("overlay-test").unwrap(),
        ))
    }

    #[tokio::test]
    async fn overlay_merge_refreshes_symbol_defs_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lib.rs"),
            "fn hello() {}\nfn world() {\n  let x=1;\n}\n",
        )
        .unwrap();
        let graph = mem_graph();

        let (files, syms) = run_overlay_merge(
            &graph,
            "overlay-test",
            tmp.path(),
            tmp.path(),
            &[PathBuf::from("lib.rs")],
        )
        .await;
        assert_eq!(files, 1);
        assert_eq!(syms, 2);

        // The end-to-end proof: symbols() finds functions that only exist on
        // disk, with NO rust-analyzer, purely via the tree-sitter overlay.
        let g = graph.load();
        assert!(!g
            .find_symbols_by_name("hello", None, 8)
            .await
            .unwrap()
            .is_empty());
        let world = g.find_symbols_by_name("world", None, 8).await.unwrap();
        assert_eq!(world.len(), 1);
        assert_eq!(world[0].file_path, "lib.rs");
    }

    #[tokio::test]
    async fn overlay_merge_skips_non_source_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "# not code\nfn nope() {}").unwrap();
        let graph = mem_graph();
        let (files, syms) = run_overlay_merge(
            &graph,
            "overlay-test",
            tmp.path(),
            tmp.path(),
            &[PathBuf::from("README.md"), PathBuf::from("Cargo.toml")],
        )
        .await;
        assert_eq!(
            files, 0,
            "non-source paths belong to the full export, not the overlay"
        );
        assert_eq!(syms, 0);
    }

    #[tokio::test]
    async fn overlay_merge_drops_defs_for_deleted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let graph = mem_graph();
        // Seed a def for gone.rs, then "delete" it (never write the file).
        std::fs::write(tmp.path().join("gone.rs"), "fn ghost() {}").unwrap();
        run_overlay_merge(
            &graph,
            "overlay-test",
            tmp.path(),
            tmp.path(),
            &[PathBuf::from("gone.rs")],
        )
        .await;
        assert!(!graph
            .load()
            .find_symbols_by_name("ghost", None, 8)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_file(tmp.path().join("gone.rs")).unwrap();
        let (files, syms) = run_overlay_merge(
            &graph,
            "overlay-test",
            tmp.path(),
            tmp.path(),
            &[PathBuf::from("gone.rs")],
        )
        .await;
        assert_eq!(
            files, 1,
            "deleted source file is still processed (to drop its rows)"
        );
        assert_eq!(syms, 0);
        assert!(
            graph
                .load()
                .find_symbols_by_name("ghost", None, 8)
                .await
                .unwrap()
                .is_empty(),
            "deleted file's defs must be gone"
        );
    }

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
        let filter = build_ignore_filter(tmp.path(), &[]);
        assert!(filter.is_ignored(&tmp.path().join("target/debug/foo.rs")));
        assert!(filter.is_ignored(&tmp.path().join("node_modules/x/index.js")));
        assert!(filter.is_ignored(&tmp.path().join("README.md")));
        assert!(filter.is_ignored(&tmp.path().join("docs/.git/HEAD")));
        // .sovereign is NOT in HARD_EXCLUDE — it's a deployment convention,
        // not universal noise. The project registry seeds it as a default
        // ignore_path so it's still filtered for newly-registered projects.
        assert!(!filter.is_ignored(&tmp.path().join(".sovereign/build.rs")));
        assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
        assert!(!filter.is_ignored(&tmp.path().join("app/server.ts")));
    }

    #[test]
    fn ignore_filter_honours_extra_ignores() {
        let tmp = tempfile::tempdir().unwrap();
        let extras = vec![".sovereign".to_string(), "my-cache".to_string()];
        let filter = build_ignore_filter(tmp.path(), &extras);
        // Project-local daemon state — SQLite WALs here would slip through
        // any `.gitignore` that wasn't loaded, hence the explicit ignore.
        assert!(filter.is_ignored(&tmp.path().join(".sovereign/notes.db-wal")));
        assert!(filter.is_ignored(&tmp.path().join(".sovereign/build.rs")));
        // A user-configured custom name applies the same way.
        assert!(filter.is_ignored(&tmp.path().join("my-cache/some.rs")));
        // Without the extras, a non-matching project shape isn't penalised.
        assert!(!filter.is_ignored(&tmp.path().join("src/main.rs")));
        let bare = build_ignore_filter(tmp.path(), &[]);
        assert!(!bare.is_ignored(&tmp.path().join("my-cache/some.rs")));
    }

    #[test]
    fn ignore_filter_honours_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "secret.rs\n").unwrap();
        let filter = build_ignore_filter(tmp.path(), &[]);
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
            .record_rebuild(
                "startup",
                Some("0000000000000000000000000000000000000000"),
                None,
            )
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

    // ── Rebuild wedge regression (order code-intel-reindexer-fix) ──
    //
    // The live wedge of 2026-08-14: the follow-up pass re-acquired
    // the sole cross-project rebuild permit the first pass still
    // held, hanging the task forever — status "active" for hours,
    // every later nudge coalescing into a silent no-op. These tests
    // pin the fixed invariants with injected fake rebuild bodies
    // (real `execute_rebuild` needs rust-analyzer + a cargo
    // workspace, which unit tests cannot run).

    fn test_rebuild_ctx(
        tmp: &tempfile::TempDir,
        id: &str,
    ) -> (RebuildCtx, Arc<ProjectState>, ScipGraphHandle) {
        let indexes = tmp.path().join("indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        let entry = sample_entry(id, tmp.path().to_path_buf());
        // `ProjectState::new` already returns an `Arc`.
        let state = ProjectState::new(id);
        let graph = mem_graph();
        let ctx = RebuildCtx {
            entry: entry.clone(),
            state: Arc::clone(&state),
            graph: Arc::clone(&graph),
            merged: mem_graph(),
            indexes_dir: indexes,
            rebuild_permits: Arc::new(Semaphore::new(1)),
        };
        (ctx, state, graph)
    }

    fn explicit_req() -> RebuildRequest {
        RebuildRequest {
            reason: RebuildReason::Explicit,
            enqueued_at: Instant::now(),
        }
    }

    fn body_ok<'a>(
        _c: &'a RebuildCtx,
        _r: &'a RebuildRequest,
    ) -> BoxFuture<'a, Result<RebuildSummary, String>> {
        Box::pin(async {
            Ok(RebuildSummary {
                symbols: 1,
                refs: 1,
                languages: vec!["rust".into()],
                skipped: vec![],
            })
        })
    }

    async fn wait_for_in_flight_clear(in_flight: &Arc<AtomicBool>) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while in_flight.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("in_flight must clear — the wedge guard");
    }

    /// THE wedge regression: the follow-up pass must run under the
    /// SAME permit the first pass holds. At HEAD the follow-up
    /// re-acquired the sole permit and hung forever (live incident
    /// 2026-08-14). The test wraps the call in a timeout because the
    /// old code deadlocked instead of returning.
    ///
    /// The incident sequence is reproduced directly: the dirty bit is
    /// SET while a rebuild is running (a signal arriving mid-pass),
    /// so the loop must run exactly one follow-up pass — observable
    /// as two "rebuild complete" lines in the per-watcher log.
    #[tokio::test]
    async fn followup_pass_runs_under_single_permit() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "wedge");
        // A signal arrives during the first pass.
        state.mark_dirty();
        let done = tokio::time::timeout(
            Duration::from_secs(10),
            run_one_rebuild_with(ctx, explicit_req(), body_ok),
        )
        .await;
        assert!(
            done.is_ok(),
            "follow-up pass self-deadlocked on the permit — the wedge"
        );
        let log = tmp.path().join("logs").join("watch-wedge-scip.log");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(
            text.matches("rebuild complete").count(),
            2,
            "exactly two passes (initial + follow-up) must have run; log:\n{text}"
        );
        assert!(
            !state.is_rebuild_in_flight(),
            "rebuild claim must be released after the loop"
        );
    }

    /// A rebuild body that never returns, for the watchdog test.
    /// A plain `fn` so the `RebuildBody` fn pointer needs no capture.
    fn body_hang<'a>(
        _c: &'a RebuildCtx,
        _r: &'a RebuildRequest,
    ) -> BoxFuture<'a, Result<RebuildSummary, String>> {
        Box::pin(async {
            futures::future::pending::<()>().await;
            unreachable!()
        })
    }

    /// A rebuild body that panics, for the panic test. A plain `fn`
    /// so the `RebuildBody` fn pointer needs no capture.
    fn body_panic<'a>(
        _c: &'a RebuildCtx,
        _r: &'a RebuildRequest,
    ) -> BoxFuture<'a, Result<RebuildSummary, String>> {
        Box::pin(async { panic!("boom") })
    }

    /// A panicked rebuild task must clear both slots (worker
    /// `in_flight` + the ProjectState claim) and record a visible
    /// failure. At HEAD the flags were only cleared by the task's
    /// own tail, which a panic skips — the project then wedged
    /// forever.
    #[tokio::test]
    async fn panic_in_rebuild_clears_flags_and_records_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "panic");
        let in_flight = Arc::new(AtomicBool::new(false));
        assert!(spawn_full_rebuild_with(
            &ctx,
            explicit_req(),
            &in_flight,
            &finish_slot(),
            body_panic,
            Duration::from_secs(30),
        ));
        wait_for_in_flight_clear(&in_flight).await;
        assert!(
            !state.is_rebuild_in_flight(),
            "ProjectState claim must clear after a panic"
        );
        assert!(
            state.rebuild_failure_count() >= 1,
            "panic must be recorded as a failure"
        );
        let snap = state.snapshot().await;
        assert!(
            matches!(
                snap.get(&WatcherKind::Scip),
                Some(WatcherStatus::Crashed { .. })
            ),
            "status must surface Crashed, got: {snap:?}"
        );
    }

    /// The watchdog: a rebuild that never completes within the wall
    /// clock is a WEDGE, not a slow export. It must be aborted,
    /// recorded as a named failure, and the slots cleared so the
    /// next signal retries instead of coalescing forever.
    #[tokio::test]
    async fn watchdog_aborts_a_hung_rebuild_and_clears_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "hung");
        let in_flight = Arc::new(AtomicBool::new(false));
        assert!(spawn_full_rebuild_with(
            &ctx,
            explicit_req(),
            &in_flight,
            &finish_slot(),
            body_hang,
            Duration::from_millis(300),
        ));
        wait_for_in_flight_clear(&in_flight).await;
        assert!(!state.is_rebuild_in_flight());
        assert!(state.rebuild_failure_count() >= 1);
        let err = state.last_rebuild_error().await;
        assert!(
            err.as_ref().is_some_and(|(e, _)| e.contains("wedged")),
            "watchdog failure must be named, got: {err:?}"
        );
    }

    /// The per-watcher log file the CLI's `project watch logs <id>
    /// scip` reads must actually be written. Before the fix nothing
    /// wrote it, so the CLI's promise ("the daemon writes per-watcher
    /// logs here once the first cycle runs") was a promise nothing
    /// kept.
    #[tokio::test]
    async fn rebuild_writes_the_per_watcher_log() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _state, _graph) = test_rebuild_ctx(&tmp, "logged");
        run_one_rebuild_with(ctx, explicit_req(), body_ok).await;
        let log = tmp.path().join("logs").join("watch-logged-scip.log");
        let text = std::fs::read_to_string(&log)
            .unwrap_or_else(|e| panic!("no per-watcher log at {}: {e}", log.display()));
        assert!(text.contains("rebuild start"), "log: {text}");
        assert!(text.contains("rebuild complete"), "log: {text}");
    }

    /// A signal while a rebuild is in flight coalesces into the
    /// dirty bit instead of stacking a second task.
    #[tokio::test]
    async fn second_spawn_coalesces_into_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, state, _graph) = test_rebuild_ctx(&tmp, "coalesce");
        let in_flight = Arc::new(AtomicBool::new(true)); // a rebuild is running
        assert!(!spawn_full_rebuild_with(
            &ctx,
            explicit_req(),
            &in_flight,
            &finish_slot(),
            body_ok,
            Duration::from_secs(30),
        ));
        assert!(
            state.end_rebuild(),
            "coalesced signal must set the dirty bit"
        );
    }

    // ─── Cooldown clock (the duty-cycle defect) ──────────────────

    fn finish_slot() -> FinishClock {
        Arc::new(std::sync::Mutex::new(None))
    }

    fn read_finish(clock: &FinishClock) -> Option<Instant> {
        clock.lock().ok().and_then(|s| *s)
    }

    /// A rebuild body slow enough that a completion stamp is
    /// distinguishable from the spawn instant.
    fn body_slow<'a>(
        _c: &'a RebuildCtx,
        _r: &'a RebuildRequest,
    ) -> BoxFuture<'a, Result<RebuildSummary, String>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(RebuildSummary {
                symbols: 1,
                refs: 1,
                languages: vec!["rust".into()],
                skipped: vec![],
            })
        })
    }

    /// THE duty-cycle defect: the cooldown gating the full
    /// rust-analyzer export was stamped when the export was SPAWNED,
    /// and the cooldown (300s) was shorter than a measured export on
    /// this monorepo (257-498s, watch-commonwealth-ai-scip.log
    /// 2026-08-14..16). The gate therefore reopened before the
    /// exporter had even finished, so continuous editing pinned
    /// rust-analyzer at a ~88-90% duty cycle holding ~14GB — measured
    /// live as export starts every ~6min each running ~5.3min. The
    /// clock must start when the exporter RELEASES the machine.
    #[tokio::test]
    async fn cooldown_clock_stamps_at_completion_not_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _state, _graph) = test_rebuild_ctx(&tmp, "cooldown");
        let in_flight = Arc::new(AtomicBool::new(false));
        let finished = finish_slot();

        let spawned_at = Instant::now();
        assert!(spawn_full_rebuild_with(
            &ctx,
            explicit_req(),
            &in_flight,
            &finished,
            body_slow,
            Duration::from_secs(30),
        ));
        wait_for_in_flight_clear(&in_flight).await;

        let stamp = read_finish(&finished).expect("completion must stamp the cooldown clock");
        assert!(
            stamp.duration_since(spawned_at) >= Duration::from_millis(150),
            "the cooldown clock must start when the export RELEASES, not when \
             it is spawned — otherwise the gate reopens mid-export"
        );
    }

    /// The stamp must land on EVERY exit path. A panicked or
    /// watchdog-aborted export still consumed the exporter slot and
    /// still spiked the machine, so the next one must still wait a
    /// full cooldown rather than launching immediately.
    #[tokio::test]
    async fn cooldown_clock_stamps_even_when_the_rebuild_panics() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _state, _graph) = test_rebuild_ctx(&tmp, "panic-stamp");
        let in_flight = Arc::new(AtomicBool::new(false));
        let finished = finish_slot();

        assert!(spawn_full_rebuild_with(
            &ctx,
            explicit_req(),
            &in_flight,
            &finished,
            body_panic,
            Duration::from_secs(30),
        ));
        wait_for_in_flight_clear(&in_flight).await;

        assert!(
            read_finish(&finished).is_some(),
            "a panicked export must still stamp the cooldown clock"
        );
    }

    #[test]
    fn full_export_due_when_none_has_ever_run() {
        assert!(full_export_due(None, FULL_REBUILD_COOLDOWN));
    }

    #[test]
    fn full_export_not_due_immediately_after_one_finished() {
        assert!(!full_export_due(
            Some(Instant::now()),
            FULL_REBUILD_COOLDOWN
        ));
    }

    /// The cooldown must exceed a real export, or the gate reopens
    /// before the exporter has released and the duty cycle climbs
    /// back toward 100% — the defect this constant was raised to fix.
    /// Slowest measured export on this monorepo: 498s.
    #[test]
    fn cooldown_exceeds_the_slowest_measured_export() {
        assert!(
            FULL_REBUILD_COOLDOWN > Duration::from_secs(498),
            "cooldown {FULL_REBUILD_COOLDOWN:?} must exceed the slowest \
             measured export (498s) or the gate reopens mid-export"
        );
    }
}
