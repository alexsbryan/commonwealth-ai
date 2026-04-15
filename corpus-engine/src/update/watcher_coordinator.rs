//! Plugin architecture for background file-watching computations.
//!
//! [`WatcherCoordinator`] owns the single OS-level filesystem watcher (via the
//! `notify` crate) and fans out debounced file-change events to every registered
//! [`BackgroundWatcher`] concurrently. Each plugin manages its own cancel-and-
//! restart semantics independently — the coordinator has no opinion about what
//! happens inside a plugin.
//!
//! ## Adding a new plugin
//!
//! 1. Implement [`BackgroundWatcher`] for your type.
//! 2. Register it with `coordinator.register(Arc::new(MyWatcher::new(...)))`.
//! 3. Register its MCP tools in `routes_mcp.rs`.
//!
//! That's it — two files changed, one new file written.
//!
//! ## Design rationale
//!
//! One OS watcher per process is intentional. On macOS, `kqueue`/`FSEvents`
//! file descriptors are a limited resource, and each `notify::recommended_watcher`
//! consumes one. Sharing a single watcher across all plugins avoids that
//! ceiling and reduces latency (one event path, not N).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};
use crate::extractors::code::is_source_file;

// ─── Status ───────────────────────────────────────────────────────────────────

/// Structured status returned by [`BackgroundWatcher::current_status`].
///
/// Used by [`WatcherCoordinator::status`] for health checks and by the server
/// startup summary. Individual MCP tools return richer domain-specific status
/// and do not use this enum directly.
#[derive(Debug, Clone)]
pub enum WatcherStatus {
    /// The watcher has never run (no recorded results in the store).
    NeverRun,
    /// A run is currently in progress.
    Running,
    /// The last run completed and no files have changed since.
    Fresh {
        /// Whether the last run passed (exit code 0 / zero failures).
        pass: bool,
        /// When the last run finished.
        last_run_at: SystemTime,
    },
    /// Files have changed since the last run; results are out of date.
    Stale {
        /// The paths that changed since the last completed run.
        stale_since: Vec<PathBuf>,
    },
    /// The watcher has no command configured — it will not react to events.
    Unconfigured,
}

impl WatcherStatus {
    /// One-line human-readable summary for startup logging.
    pub fn summary(&self) -> String {
        match self {
            WatcherStatus::NeverRun => "never run".into(),
            WatcherStatus::Running => "running".into(),
            WatcherStatus::Fresh { pass: true, .. } => "passing".into(),
            WatcherStatus::Fresh { pass: false, .. } => "failing (fresh)".into(),
            WatcherStatus::Stale { stale_since } => {
                format!("stale ({} files changed)", stale_since.len())
            }
            WatcherStatus::Unconfigured => "unconfigured".into(),
        }
    }
}

// ─── BackgroundWatcher trait ──────────────────────────────────────────────────

/// A background computation that re-runs on file changes and stores results
/// for agent queries.
///
/// Implementations are responsible for their own storage and cancel-and-restart
/// semantics. The coordinator calls [`on_files_changed`] and then immediately
/// returns — it does not wait for the run to complete.
///
/// ## Object safety
///
/// This trait is `dyn`-safe. It compiles as `Arc<dyn BackgroundWatcher>`.
#[async_trait]
pub trait BackgroundWatcher: Send + Sync + 'static {
    /// Unique stable identifier. Used as the MCP tool name prefix and SQLite
    /// DB filename. e.g. `"test"`, `"lint"`. Must be kebab-case, no spaces.
    fn id(&self) -> &'static str;

    /// Human-readable description for logs and status output.
    fn description(&self) -> &'static str;

    /// Called by [`WatcherCoordinator`] when the debounce window closes for
    /// one or more paths. Implementations should cancel any in-flight run and
    /// start a new one. This method must return quickly — spawn a task.
    async fn on_files_changed(&self, paths: Vec<PathBuf>);

    /// Returns a brief structured status for health checks. Used by
    /// [`WatcherCoordinator::status`] — not the MCP tools (those return
    /// richer domain-specific responses).
    async fn current_status(&self) -> WatcherStatus;
}

// ─── WatcherCoordinator ───────────────────────────────────────────────────────

/// Owns the single OS filesystem watcher and fans out debounced file-change
/// events to all registered [`BackgroundWatcher`] plugins simultaneously.
///
/// Created via [`WatcherCoordinator::new`], populated with [`register`], then
/// started with [`start`]. Dropping the returned [`CoordinatorHandle`] shuts
/// down the background task and the filesystem watcher.
pub struct WatcherCoordinator {
    watchers: Vec<Arc<dyn BackgroundWatcher>>,
    debounce_ms: u64,
}

impl WatcherCoordinator {
    /// Create a new coordinator. `debounce_ms` is the idle window (in
    /// milliseconds) after the last event on a path before it is flushed to
    /// plugins. 800ms matches the existing `CodeWatcher` default.
    pub fn new(debounce_ms: u64) -> Self {
        Self {
            watchers: Vec::new(),
            debounce_ms,
        }
    }

    /// Register a plugin. Plugins receive events in the order they are
    /// registered but execute concurrently — registration order has no
    /// priority semantics.
    pub fn register(&mut self, watcher: Arc<dyn BackgroundWatcher>) {
        tracing::info!(
            id = watcher.id(),
            description = watcher.description(),
            "watcher registered"
        );
        self.watchers.push(watcher);
    }

    /// Start the coordinator. Creates the OS watcher, begins watching all
    /// `watch_paths` recursively, and spawns the debounce task. Returns a
    /// [`CoordinatorHandle`] that shuts everything down on drop.
    pub async fn start(self, watch_paths: Vec<PathBuf>) -> Result<CoordinatorHandle> {
        if watch_paths.is_empty() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "WatcherCoordinator: watch_paths must not be empty",
            )));
        }

        // Canonicalize all paths to survive macOS /private prefix quirk.
        let canonical_paths: Vec<PathBuf> = watch_paths
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();

        let (tx, rx) = mpsc::channel::<Event>(512);
        let watcher_tx = tx;

        let mut watcher = notify::recommended_watcher(move |res| match res {
            Ok(event) => {
                let _ = watcher_tx.blocking_send(event);
            }
            Err(e) => {
                tracing::warn!("notify watcher error: {e}");
            }
        })
        .map_err(|e| Error::Io(std::io::Error::other(format!("notify init: {e}"))))?;

        for path in &canonical_paths {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|e| Error::Io(std::io::Error::other(format!("watch start: {e}"))))?;
        }

        let debounce = Duration::from_millis(self.debounce_ms);
        let watchers = self.watchers;
        let roots = canonical_paths;

        let task = tokio::spawn(async move {
            run_coordinator_loop(rx, watchers, roots, debounce).await;
        });

        tracing::info!(
            debounce_ms = self.debounce_ms,
            "WatcherCoordinator started"
        );

        Ok(CoordinatorHandle {
            _watcher: watcher,
            task,
        })
    }

    /// Returns the current status of all registered watchers. Safe to call
    /// before [`start`] — returns empty vec.
    pub fn registered_ids(&self) -> Vec<&'static str> {
        self.watchers.iter().map(|w| w.id()).collect()
    }
}

// ─── CoordinatorHandle ────────────────────────────────────────────────────────

/// Returned by [`WatcherCoordinator::start`]. Dropping this handle aborts the
/// background task and lets `notify` shut down its watcher thread.
pub struct CoordinatorHandle {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
}

impl CoordinatorHandle {
    /// Abort the background task explicitly. Also called by `Drop`.
    pub fn abort(&self) {
        self.task.abort();
    }

    /// Query the current status of all plugins. Calls each plugin's
    /// `current_status()` concurrently and returns results in registration
    /// order.
    pub async fn status(&self, watchers: &[Arc<dyn BackgroundWatcher>]) -> Vec<(String, WatcherStatus)> {
        let futs: Vec<_> = watchers
            .iter()
            .map(|w| {
                let w = Arc::clone(w);
                async move {
                    let id = w.id().to_string();
                    let status = w.current_status().await;
                    (id, status)
                }
            })
            .collect();
        futures::future::join_all(futs).await
    }
}

impl Drop for CoordinatorHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ─── Internal debounce loop ───────────────────────────────────────────────────

/// Main coordinator loop. Debounces events per path then fans out to all
/// registered watchers concurrently.
async fn run_coordinator_loop(
    mut rx: mpsc::Receiver<Event>,
    watchers: Vec<Arc<dyn BackgroundWatcher>>,
    roots: Vec<PathBuf>,
    debounce: Duration,
) {
    // Map: absolute path → (last_event_at, is_delete).
    let mut pending: std::collections::HashMap<PathBuf, (Instant, bool)> =
        std::collections::HashMap::new();

    let tick_interval = debounce / 2;

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        tracing::trace!(
                            kind = ?event.kind,
                            paths = ?event.paths,
                            "coordinator saw event"
                        );
                        for path in interesting_coordinator_paths(&event, &roots) {
                            let is_delete = matches!(event.kind, EventKind::Remove(_));
                            pending.insert(path, (Instant::now(), is_delete));
                        }
                    }
                    None => {
                        // Channel closed — flush remaining and exit.
                        flush_coordinator(&mut pending, &watchers, Duration::ZERO).await;
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(tick_interval) => {
                flush_coordinator(&mut pending, &watchers, debounce).await;
            }
        }
    }
}

/// Collect source-file paths from an event that live under any watched root.
fn interesting_coordinator_paths(event: &Event, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in &event.paths {
        let is_delete = matches!(event.kind, EventKind::Remove(_));
        let keep = is_delete || is_source_file(path);
        if !keep {
            continue;
        }
        let under_any_root = roots.iter().any(|r| path.starts_with(r));
        if under_any_root {
            out.push(path.clone());
        }
    }
    out
}

/// Flush all paths that have been idle for at least `debounce`. When
/// `debounce` is `ZERO` (shutdown flush), everything is flushed immediately.
async fn flush_coordinator(
    pending: &mut std::collections::HashMap<PathBuf, (Instant, bool)>,
    watchers: &[Arc<dyn BackgroundWatcher>],
    debounce: Duration,
) {
    let now = Instant::now();
    let ready: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, (ts, _))| now.duration_since(*ts) >= debounce)
        .map(|(p, _)| p.clone())
        .collect();

    if ready.is_empty() {
        return;
    }

    for path in &ready {
        pending.remove(path);
    }

    tracing::debug!(
        count = ready.len(),
        "coordinator flushing paths to {} watchers",
        watchers.len()
    );

    // Fan out to all watchers concurrently. Each watcher gets the same
    // path list and handles its own cancel-and-restart logic.
    let futs: Vec<_> = watchers
        .iter()
        .map(|w| {
            let w = Arc::clone(w);
            let paths = ready.clone();
            async move {
                w.on_files_changed(paths).await;
            }
        })
        .collect();

    futures::future::join_all(futs).await;
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingWatcher {
        id: &'static str,
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BackgroundWatcher for CountingWatcher {
        fn id(&self) -> &'static str {
            self.id
        }
        fn description(&self) -> &'static str {
            "test counter"
        }
        async fn on_files_changed(&self, _paths: Vec<PathBuf>) {
            self.call_count.fetch_add(1, Ordering::SeqCst);
        }
        async fn current_status(&self) -> WatcherStatus {
            WatcherStatus::NeverRun
        }
    }

    /// Compile-time: Arc<dyn BackgroundWatcher> must be constructable.
    #[test]
    fn trait_object_safety() {
        let counter = Arc::new(AtomicUsize::new(0));
        let watcher = Arc::new(CountingWatcher {
            id: "test",
            call_count: Arc::clone(&counter),
        });
        let _boxed: Arc<dyn BackgroundWatcher> = watcher;
    }

    /// WatcherStatus::NeverRun is the initial status for a fresh watcher.
    #[test]
    fn watcher_status_never_run_summary() {
        let s = WatcherStatus::NeverRun;
        assert_eq!(s.summary(), "never run");
    }

    /// Multiple watchers can be registered and all receive fan-out calls.
    #[tokio::test]
    async fn coordinator_fans_out_to_all_watchers() {
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));

        let wa: Arc<dyn BackgroundWatcher> = Arc::new(CountingWatcher {
            id: "watcher-a",
            call_count: Arc::clone(&counter_a),
        });
        let wb: Arc<dyn BackgroundWatcher> = Arc::new(CountingWatcher {
            id: "watcher-b",
            call_count: Arc::clone(&counter_b),
        });

        // Call flush_coordinator directly, bypassing the notify watcher.
        let paths = vec![PathBuf::from("/tmp/foo.rs")];
        let watchers = vec![Arc::clone(&wa), Arc::clone(&wb)];
        let mut pending: std::collections::HashMap<PathBuf, (Instant, bool)> =
            [(paths[0].clone(), (Instant::now() - Duration::from_secs(10), false))]
                .into();

        flush_coordinator(&mut pending, &watchers, Duration::from_millis(800)).await;

        assert_eq!(counter_a.load(Ordering::SeqCst), 1, "watcher-a not called");
        assert_eq!(counter_b.load(Ordering::SeqCst), 1, "watcher-b not called");
        assert!(pending.is_empty(), "pending should be drained");
    }

    /// A watcher with no events pending does not call on_files_changed.
    #[tokio::test]
    async fn coordinator_skips_fresh_paths() {
        let counter = Arc::new(AtomicUsize::new(0));
        let w: Arc<dyn BackgroundWatcher> = Arc::new(CountingWatcher {
            id: "w",
            call_count: Arc::clone(&counter),
        });

        // Path was seen "now" — not yet idle for 800ms.
        let mut pending: std::collections::HashMap<PathBuf, (Instant, bool)> =
            [(PathBuf::from("/tmp/bar.rs"), (Instant::now(), false))].into();

        flush_coordinator(&mut pending, &[w], Duration::from_millis(800)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0, "should not have been called");
        assert_eq!(pending.len(), 1, "pending should not be drained");
    }

    #[tokio::test]
    async fn coordinator_register_ids() {
        let mut coordinator = WatcherCoordinator::new(800);
        coordinator.register(Arc::new(CountingWatcher {
            id: "alpha",
            call_count: Arc::new(AtomicUsize::new(0)),
        }));
        coordinator.register(Arc::new(CountingWatcher {
            id: "beta",
            call_count: Arc::new(AtomicUsize::new(0)),
        }));
        let ids = coordinator.registered_ids();
        assert_eq!(ids, vec!["alpha", "beta"]);
    }
}
