//! Filesystem watcher for code corpora.
//!
//! Wraps the `notify` crate in a tokio-friendly shape: `notify`'s event
//! callback runs on its own thread, so we forward events through a
//! bounded `mpsc` channel into a tokio task that debounces and fans out
//! to [`CorpusEngine::reindex_file`].
//!
//! ## Why a debouncer?
//!
//! Editors routinely write a file two or three times in quick succession
//! (temporary → final, atomic rename, background save). Re-indexing on
//! every event would mean we embed the same symbols 2–3× per save. The
//! debouncer collects events per-path and flushes once the path has been
//! idle for `debounce`. 800ms matches the spec and comfortably covers
//! editors that `write → move` or `unlink → create`.
//!
//! ## Ownership
//!
//! [`CodeWatcher`] owns the notify watcher and the tokio task. Dropping
//! the returned [`WatcherHandle`] aborts the task and lets `notify` shut
//! down its own thread. No background work outlives the handle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
use crate::extractors::code::is_source_file;

/// Default idle window before a pending change is flushed. Rapid writes
/// to the same path within this window collapse into a single reindex.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(800);

/// Handle returned by [`CodeWatcher::start`]. Dropping this handle
/// aborts the background task and releases the underlying watcher.
pub struct WatcherHandle {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
}

impl WatcherHandle {
    /// Abort the background task explicitly. Called by `Drop` too, but
    /// exposed so callers can assert that the task is gone (useful in
    /// tests to avoid lingering filesystem watchers between runs).
    pub fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Configures and starts a filesystem watcher for a single code corpus.
pub struct CodeWatcher {
    engine: Arc<CorpusEngine>,
    corpus_id: String,
    root: PathBuf,
    debounce: Duration,
    #[cfg(feature = "treesitter")]
    scip_graph: Option<Arc<corpus_engine_scip::ScipGraph>>,
}

impl CodeWatcher {
    pub fn new(engine: Arc<CorpusEngine>, corpus_id: impl Into<String>, root: PathBuf) -> Self {
        Self {
            engine,
            corpus_id: corpus_id.into(),
            root,
            debounce: DEFAULT_DEBOUNCE,
            #[cfg(feature = "treesitter")]
            scip_graph: None,
        }
    }

    /// Override the debounce window. Use for tests that need a tight
    /// bound — in production the default is the right value.
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Attach a SCIP call graph so the watcher can mark files as stale
    /// when they're modified. Without this, the call graph has no
    /// per-file staleness tracking.
    #[cfg(feature = "treesitter")]
    pub fn with_scip_graph(mut self, graph: Arc<corpus_engine_scip::ScipGraph>) -> Self {
        self.scip_graph = Some(graph);
        self
    }

    /// Start watching. Returns a handle that aborts the task on drop.
    ///
    /// This function is `async` only because it needs a tokio runtime
    /// handle for the debounce task — it completes synchronously.
    pub async fn start(self) -> Result<WatcherHandle> {
        if !self.root.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("watch root does not exist: {}", self.root.display()),
            )));
        }
        // Canonicalize the root up-front. On macOS, FSEvents reports
        // paths with the `/private` prefix (because `/var` is a symlink
        // to `/private/var`). If we watch `/var/foo` but compare events
        // against that literal string, `starts_with` fails and every
        // event is silently dropped. Canonicalizing both sides fixes it.
        let root = self.root.canonicalize().unwrap_or(self.root.clone());

        // mpsc for the notify → tokio bridge. Bounded because notify's
        // thread shouldn't outrun the debouncer by much.
        let (tx, rx) = mpsc::channel::<Event>(256);

        let watcher_tx = tx;
        let mut watcher = notify::recommended_watcher(move |res| match res {
            Ok(event) => {
                // Fire-and-forget: if the channel is full, the event is
                // dropped. The debouncer treats missing events as a
                // no-op (the next event for the same path will still
                // trigger a reindex). Better to drop than block the
                // notify thread.
                let _ = watcher_tx.blocking_send(event);
            }
            Err(e) => {
                tracing::warn!("notify watcher error: {e}");
            }
        })
        .map_err(|e| Error::Io(std::io::Error::other(format!("notify: {e}"))))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| Error::Io(std::io::Error::other(format!("watch start: {e}"))))?;

        let engine = self.engine;
        let corpus_id = self.corpus_id;
        let debounce = self.debounce;
        #[cfg(feature = "treesitter")]
        let scip_graph = self.scip_graph;

        let task = tokio::spawn(async move {
            run_debouncer(
                rx, engine, corpus_id, root, debounce,
                #[cfg(feature = "treesitter")]
                scip_graph,
            ).await;
        });

        Ok(WatcherHandle {
            _watcher: watcher,
            task,
        })
    }
}

/// Inner debounce loop. Collects events keyed by path; whenever any
/// path's pending entry is older than `debounce`, flushes it.
async fn run_debouncer(
    mut rx: mpsc::Receiver<Event>,
    engine: Arc<CorpusEngine>,
    corpus_id: String,
    root: PathBuf,
    debounce: Duration,
    #[cfg(feature = "treesitter")]
    scip_graph: Option<Arc<corpus_engine_scip::ScipGraph>>,
) {
    // `pending` maps absolute path → (last_event_at, is_delete). We
    // track the last event time so we can flush only after idle; we
    // also track whether the last event was a removal so the reindex
    // call knows to delete rather than re-embed.
    let mut pending: HashMap<PathBuf, (Instant, bool)> = HashMap::new();

    // Tick interval has to be shorter than the debounce — otherwise
    // the check-for-idle path sleeps too long before flushing. Half the
    // debounce is a safe default.
    let tick_interval = debounce / 2;

    loop {
        // Wait for either a new event or the next tick, whichever comes
        // first. `tokio::select!` gives us that without spawning more
        // tasks.
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        tracing::trace!(
                            kind = ?event.kind,
                            paths = ?event.paths,
                            "watcher saw event"
                        );
                        for path in interesting_paths(&event, &root) {
                            let is_delete = matches!(event.kind, EventKind::Remove(_));
                            pending.insert(path, (Instant::now(), is_delete));
                        }
                    }
                    None => {
                        // Sender dropped → the `notify` thread has gone
                        // away. Flush whatever is pending and exit.
                        flush_ready(
                            &mut pending, &engine, &corpus_id, &root, Duration::ZERO,
                            #[cfg(feature = "treesitter")]
                            &scip_graph,
                        ).await;
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(tick_interval) => {
                flush_ready(
                    &mut pending, &engine, &corpus_id, &root, debounce,
                    #[cfg(feature = "treesitter")]
                    &scip_graph,
                ).await;
            }
        }
    }
}

/// Emit every path in an event that looks like a source file and lives
/// under the watched root. We skip paths that don't pass
/// `is_source_file` — notify fires on every file in the tree, including
/// `.git`/`target`/etc. that the extractor wouldn't touch anyway.
fn interesting_paths(event: &Event, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in &event.paths {
        let is_delete = matches!(event.kind, EventKind::Remove(_));
        // For creates/modifies we check is_source_file on the current
        // path — if the file no longer exists that's fine, reindex_file
        // handles the missing-file branch. For deletes we skip the
        // extension check: the file is gone so we can't classify it,
        // but we still want to attempt a delete in case it was a
        // previously-indexed source file.
        let keep = is_delete || is_source_file(path);
        if !keep {
            continue;
        }
        if path.starts_with(root) {
            out.push(path.clone());
        }
    }
    out
}

/// Drain paths whose last-event timestamp is older than `debounce`,
/// running `reindex_file` for each. Called on every tick; also called
/// with `Duration::ZERO` during shutdown to force-flush.
async fn flush_ready(
    pending: &mut HashMap<PathBuf, (Instant, bool)>,
    engine: &Arc<CorpusEngine>,
    corpus_id: &str,
    root: &Path,
    debounce: Duration,
    #[cfg(feature = "treesitter")]
    scip_graph: &Option<Arc<corpus_engine_scip::ScipGraph>>,
) {
    let now = Instant::now();
    let ready: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, (ts, _))| now.duration_since(*ts) >= debounce)
        .map(|(p, _)| p.clone())
        .collect();

    for path in ready {
        pending.remove(&path);
        match engine.reindex_file(corpus_id, &path, root).await {
            Ok(result) => {
                tracing::info!(
                    corpus = corpus_id,
                    path = %path.display(),
                    ?result,
                    "watcher reindexed"
                );
                // Mark the file stale in the call graph so queries for
                // symbols in this file show staleness notes.
                #[cfg(feature = "treesitter")]
                if let Some(ref graph) = scip_graph {
                    if let Ok(rel) = path.strip_prefix(root) {
                        graph.mark_file_stale(&rel.to_string_lossy()).await;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    path = %path.display(),
                    error = %e,
                    "watcher reindex failed"
                );
            }
        }
    }
}
