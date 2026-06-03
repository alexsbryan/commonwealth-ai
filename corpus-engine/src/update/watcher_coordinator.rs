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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

// ─── WatcherHeartbeat ───────────────────────────────────────────────────────────

/// Shared liveness beacon for the watcher coordinator.
///
/// ## Why a heartbeat and not a bool
///
/// The daemon historically tracked watcher liveness with a one-shot
/// `AtomicBool` set to `true` once `coordinator.start()` returned and
/// never cleared. That cannot detect a watcher that *died after
/// starting* — if the coordinator loop task panics or the underlying
/// `notify` thread dies, the bool stays `true` forever and
/// `lint_status`/`test_status` keep asserting the watcher is live while
/// nothing is actually watching. The agent then trusts increasingly
/// stale data with no signal that anything is wrong.
///
/// A heartbeat inverts the failure mode: the coordinator loop stamps
/// [`stamp`](Self::stamp) on every iteration (≤ `debounce/2`, so
/// sub-second in practice). Readers treat the watcher as live iff the
/// last stamp is recent. When the loop stops — for *any* reason — the
/// stamp freezes and liveness decays to `false` on its own. Death is
/// the default, not something we have to remember to signal.
///
/// ## Cross-process visibility
///
/// The status tools that report liveness (`lint_status` / `test_status`)
/// run in TWO processes: the daemon (which owns the live in-memory
/// heartbeat) and the `sovereign` CLI (a separate process that reads the
/// daemon's SQLite stores). An in-memory `AtomicU64` is invisible to the
/// CLI. So a writer heartbeat optionally mirrors each stamp to a small
/// **sidecar file** (`~/.sovereign/watcher-heartbeat`); the CLI builds a
/// [`reader`](Self::reader) over that path and gets the same liveness the
/// daemon sees. Writes are throttled to once per [`SIDECAR_WRITE_THROTTLE_SECS`]
/// so a sub-second loop doesn't churn the inode.
#[derive(Debug)]
pub struct WatcherHeartbeat {
    /// Unix seconds of the last loop iteration (writer modes). `0` ==
    /// never ticked (coordinator not started, or loop hasn't run yet).
    last_tick_unix: AtomicU64,
    /// Throttle bookkeeping: unix seconds of the last sidecar write.
    last_write_unix: AtomicU64,
    /// When set, each `stamp` also mirrors the timestamp to this file so
    /// other processes can read liveness. Writer mode.
    sidecar_write: Option<std::path::PathBuf>,
    /// When set, `last_tick_unix`/`age_secs`/`is_live` read the timestamp
    /// from this file rather than the in-memory atomic. Reader mode —
    /// `stamp` is a no-op. Used by the CLI process.
    file_read: Option<std::path::PathBuf>,
}

/// Minimum gap between sidecar file writes. The loop stamps sub-second;
/// 3s keeps the file fresh for a 30–60s liveness window without inode
/// churn.
pub const SIDECAR_WRITE_THROTTLE_SECS: u64 = 3;

impl Default for WatcherHeartbeat {
    fn default() -> Self {
        Self {
            last_tick_unix: AtomicU64::new(0),
            last_write_unix: AtomicU64::new(0),
            sidecar_write: None,
            file_read: None,
        }
    }
}

impl WatcherHeartbeat {
    /// A fresh in-memory heartbeat that has never ticked. Wrapped in
    /// `Arc` because it is shared between the coordinator loop (writer)
    /// and any in-process readers (the daemon's status tools).
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A writer heartbeat that also mirrors every stamp to `path`, so a
    /// different process can read liveness via [`reader`](Self::reader).
    pub fn with_sidecar(path: std::path::PathBuf) -> Arc<Self> {
        Arc::new(Self {
            sidecar_write: Some(path),
            ..Self::default()
        })
    }

    /// A read-only heartbeat backed by a sidecar `path` a writer in
    /// another process maintains. `stamp` is a no-op; `age_secs` /
    /// `is_live` reflect the file's mtime-of-content (the stamped unix
    /// seconds). A missing/empty/unparseable file reads as never-ticked.
    pub fn reader(path: std::path::PathBuf) -> Arc<Self> {
        Arc::new(Self {
            file_read: Some(path),
            ..Self::default()
        })
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Record that the coordinator loop is alive *now*. Called once per
    /// loop iteration. No-op for a reader heartbeat. For a sidecar writer,
    /// throttles the file mirror to [`SIDECAR_WRITE_THROTTLE_SECS`].
    pub fn stamp(&self) {
        if self.file_read.is_some() {
            return; // reader: never writes
        }
        let now = Self::now_unix();
        self.last_tick_unix.store(now, Ordering::Release);

        if let Some(ref path) = self.sidecar_write {
            let last_write = self.last_write_unix.load(Ordering::Acquire);
            if now.saturating_sub(last_write) >= SIDECAR_WRITE_THROTTLE_SECS {
                self.last_write_unix.store(now, Ordering::Release);
                write_heartbeat_file(path, now);
            }
        }
    }

    /// Unix seconds of the last stamp, or `None` if never ticked. Reads
    /// the sidecar file in reader mode, the atomic otherwise.
    pub fn last_tick_unix(&self) -> Option<u64> {
        if let Some(ref path) = self.file_read {
            return read_heartbeat_file(path);
        }
        match self.last_tick_unix.load(Ordering::Acquire) {
            0 => None,
            t => Some(t),
        }
    }

    /// Seconds since the last stamp, or `None` if never stamped. Uses
    /// saturating subtraction so a clock skew can't underflow into a
    /// huge "looks live" value.
    pub fn age_secs(&self) -> Option<u64> {
        self.last_tick_unix()
            .map(|t| Self::now_unix().saturating_sub(t))
    }

    /// True iff the loop stamped within `window_secs`. A never-stamped
    /// heartbeat is never live.
    pub fn is_live(&self, window_secs: u64) -> bool {
        matches!(self.age_secs(), Some(age) if age <= window_secs)
    }
}

/// Atomically write `unix_secs` to the heartbeat sidecar (temp + rename
/// so a concurrent reader never sees a torn write). Best-effort: a
/// failure just means readers see a slightly older stamp until the next
/// successful write — the watcher itself is unaffected.
fn write_heartbeat_file(path: &std::path::Path, unix_secs: u64) {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, unix_secs.to_string().as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Read the unix-seconds stamp from the sidecar. `None` on any of
/// missing / empty / unparseable — all of which mean "no live writer."
fn read_heartbeat_file(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&t| t > 0)
}

/// True iff the path has an extension that maps to a tracked source-file
/// language. Inlined here so `watcher_coordinator` (which lives in the
/// `stores` feature) doesn't pull `extractors::code` — that module is
/// the full tree-sitter extractor and stays under `treesitter`.
///
/// The extension list mirrors `extractors::code::all_languages()`'s
/// `extensions` arrays; a test in that module pins the two in sync.
fn is_source_file(path: &std::path::Path) -> bool {
    const TRACKED_EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "py",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TRACKED_EXTS.contains(&e))
        .unwrap_or(false)
}

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

// ─── ActivityCallback ─────────────────────────────────────────────────────────

/// Notified by [`WatcherCoordinator`] when filesystem activity is detected.
///
/// Implemented by `sovereign-server`'s `ActivityReporter` — this trait lives
/// in `corpus-engine` so the dependency direction is correct (corpus-engine
/// cannot depend on sovereign-server).
#[async_trait]
pub trait ActivityCallback: Send + Sync + 'static {
    /// Called when the debounce window fires and paths are dispatched to watchers.
    async fn on_files_changed(&self);
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
    activity: Option<Arc<dyn ActivityCallback>>,
    heartbeat: Option<Arc<WatcherHeartbeat>>,
}

impl WatcherCoordinator {
    /// Create a new coordinator. `debounce_ms` is the idle window (in
    /// milliseconds) after the last event on a path before it is flushed to
    /// plugins. 800ms matches the existing `CodeWatcher` default.
    pub fn new(debounce_ms: u64) -> Self {
        Self {
            watchers: Vec::new(),
            debounce_ms,
            activity: None,
            heartbeat: None,
        }
    }

    /// Attach an activity reporter. Called before [`start`] — the callback is
    /// notified whenever files are flushed to watchers.
    pub fn with_activity(mut self, cb: Arc<dyn ActivityCallback>) -> Self {
        self.activity = Some(cb);
        self
    }

    /// Attach a [`WatcherHeartbeat`] the loop will stamp on every
    /// iteration. The daemon shares this same `Arc` with the status
    /// tools so they can tell whether the loop is still alive — see the
    /// `WatcherHeartbeat` docs for why this replaces the old one-shot
    /// `watcher_active` bool.
    pub fn with_heartbeat(mut self, heartbeat: Arc<WatcherHeartbeat>) -> Self {
        self.heartbeat = Some(heartbeat);
        self
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
        let activity = self.activity;
        let heartbeat = self.heartbeat;
        // Stamp once up-front so `is_live` reads true the instant
        // `start()` returns, before the loop's first tick lands.
        if let Some(ref hb) = heartbeat {
            hb.stamp();
        }
        let handle_heartbeat = heartbeat.clone();

        let task = tokio::spawn(async move {
            run_coordinator_loop(rx, watchers, roots, debounce, activity, heartbeat).await;
        });

        tracing::info!(
            debounce_ms = self.debounce_ms,
            "WatcherCoordinator started"
        );

        Ok(CoordinatorHandle {
            _watcher: watcher,
            task,
            heartbeat: handle_heartbeat,
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
    heartbeat: Option<Arc<WatcherHeartbeat>>,
}

impl CoordinatorHandle {
    /// Abort the background task explicitly. Also called by `Drop`.
    pub fn abort(&self) {
        self.task.abort();
    }

    /// True iff the coordinator loop task is still running. Cheap,
    /// non-blocking poll of the tokio `JoinHandle`. A supervisor uses
    /// this to detect a panicked loop and restart the coordinator.
    pub fn is_alive(&self) -> bool {
        !self.task.is_finished()
    }

    /// The shared heartbeat, if one was attached via
    /// [`WatcherCoordinator::with_heartbeat`]. Lets a supervisor check
    /// liveness by stamp-age in addition to the coarse `is_alive` task
    /// poll (catches a loop that's alive-but-wedged).
    pub fn heartbeat(&self) -> Option<&Arc<WatcherHeartbeat>> {
        self.heartbeat.as_ref()
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
    activity: Option<Arc<dyn ActivityCallback>>,
    heartbeat: Option<Arc<WatcherHeartbeat>>,
) {
    // Map: absolute path → (last_event_at, is_delete).
    let mut pending: std::collections::HashMap<PathBuf, (Instant, bool)> =
        std::collections::HashMap::new();

    // `tick_interval` is `debounce/2`, so even a fully idle workspace
    // wakes this loop at least that often via the timer arm. Guard
    // against a zero-debounce config producing a zero interval (which
    // would busy-spin) by flooring at 100ms.
    let tick_interval = (debounce / 2).max(Duration::from_millis(100));

    loop {
        // Heartbeat: stamp at the top of every iteration. Because the
        // timer arm below fires every `tick_interval`, this stamp is
        // refreshed sub-second on an idle workspace. If this task ever
        // panics or returns, the stamp freezes and readers see the
        // watcher decay to not-live on their own — no explicit
        // death-signal needed. See `WatcherHeartbeat`.
        if let Some(ref hb) = heartbeat {
            hb.stamp();
        }

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
                        flush_coordinator(&mut pending, &watchers, Duration::ZERO, activity.as_deref()).await;
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(tick_interval) => {
                flush_coordinator(&mut pending, &watchers, debounce, activity.as_deref()).await;
            }
        }
    }
}

/// Collect source-file paths from an event that live under any watched root.
///
/// **Event kind filtering is load-bearing.** Without it, `cargo check`'s own
/// reads of source files fire `EventKind::Access` (Open / Read / CloseNoWrite)
/// on every `.rs` it inspects — which the watcher used to interpret as "the
/// file changed", retriggering itself in an infinite loop. We accept only:
///
/// - `Create`             — new file appeared
/// - `Modify(Data | Name)` — content or rename change
/// - `Remove`             — deletion
///
/// and drop `Access(*)`, `Modify(Metadata | Other)`, `Any`, and `Other`.
/// Pure mtime/atime touches (e.g. cargo's fingerprint-stamp `touch` of a
/// dependency input) fall under `Modify::Metadata` and are ignored — if a
/// rebuild output really *changed* the file, the data write also fires
/// `Modify::Data` separately.
///
/// Paths inside `target/`, `.git/`, `node_modules/`, or any hidden directory
/// component are also excluded — build artifacts and VCS internals trigger
/// constant events during compilation regardless of kind.
fn interesting_coordinator_paths(event: &Event, roots: &[PathBuf]) -> Vec<PathBuf> {
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    let is_mutating = matches!(
        event.kind,
        EventKind::Create(CreateKind::File | CreateKind::Any)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_))
            | EventKind::Remove(RemoveKind::File | RemoveKind::Any)
    );
    if !is_mutating {
        return Vec::new();
    }

    let mut out = Vec::new();
    for path in &event.paths {
        let is_delete = matches!(event.kind, EventKind::Remove(_));
        let keep = is_delete || is_source_file(path);
        if !keep {
            continue;
        }
        // Skip build artifacts and VCS / dependency caches.
        let in_ignored_dir = path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            matches!(s.as_ref(), "target" | ".git" | "node_modules")
                || (s.starts_with('.') && s.len() > 1)
        });
        if in_ignored_dir {
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
    activity: Option<&dyn ActivityCallback>,
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

    // Notify activity reporter that files changed.
    if let Some(cb) = activity {
        cb.on_files_changed().await;
    }

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
        let paths = [PathBuf::from("/tmp/foo.rs")];
        let watchers = vec![Arc::clone(&wa), Arc::clone(&wb)];
        let mut pending: std::collections::HashMap<PathBuf, (Instant, bool)> =
            [(paths[0].clone(), (Instant::now() - Duration::from_secs(10), false))]
                .into();

        flush_coordinator(&mut pending, &watchers, Duration::from_millis(800), None).await;

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

        flush_coordinator(&mut pending, &[w], Duration::from_millis(800), None).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0, "should not have been called");
        assert_eq!(pending.len(), 1, "pending should not be drained");
    }

    #[test]
    fn heartbeat_never_stamped_is_not_live() {
        let hb = WatcherHeartbeat::new();
        assert_eq!(hb.last_tick_unix(), None);
        assert_eq!(hb.age_secs(), None);
        // A never-stamped heartbeat must never read live, no matter how
        // generous the window — this is the "watcher never started" case.
        assert!(!hb.is_live(0));
        assert!(!hb.is_live(86_400));
    }

    #[test]
    fn heartbeat_live_after_stamp() {
        let hb = WatcherHeartbeat::new();
        hb.stamp();
        assert!(hb.last_tick_unix().is_some());
        assert!(matches!(hb.age_secs(), Some(a) if a <= 1));
        assert!(hb.is_live(30), "freshly stamped heartbeat must be live");
    }

    /// `start()` stamps the heartbeat synchronously before returning, and
    /// the spawned loop keeps it warm. A live coordinator therefore reads
    /// `is_live` immediately and `is_alive` (task running) is true.
    #[tokio::test]
    async fn started_coordinator_is_live_via_heartbeat() {
        let tmp = std::env::temp_dir().join(format!(
            "wc_hb_live_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let hb = WatcherHeartbeat::new();
        let coordinator = WatcherCoordinator::new(200).with_heartbeat(Arc::clone(&hb));
        let handle = coordinator.start(vec![tmp.clone()]).await.unwrap();

        assert!(hb.is_live(30), "heartbeat must be live right after start");
        assert!(handle.is_alive(), "loop task must be running");
        assert!(handle.heartbeat().is_some());

        // Let the timer arm tick at least once and confirm the stamp
        // advances (loop is actually running, not just the pre-stamp).
        let first = hb.last_tick_unix();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(hb.last_tick_unix() >= first);

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// When the loop task is aborted (the failure we previously couldn't
    /// detect), `is_alive` flips to false. The heartbeat stops advancing,
    /// so a liveness window eventually lapses too.
    #[tokio::test]
    async fn aborted_loop_reports_not_alive() {
        let tmp = std::env::temp_dir().join(format!(
            "wc_hb_abort_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let hb = WatcherHeartbeat::new();
        let coordinator = WatcherCoordinator::new(200).with_heartbeat(Arc::clone(&hb));
        let handle = coordinator.start(vec![tmp.clone()]).await.unwrap();
        assert!(handle.is_alive());

        handle.abort();
        // Give tokio a moment to mark the JoinHandle finished.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_alive(), "aborted loop must report not-alive");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn sidecar_writer_to_reader_roundtrip() {
        let path = std::env::temp_dir().join(format!("wc_hb_sidecar_{}", std::process::id()));
        std::fs::remove_file(&path).ok();

        let reader = WatcherHeartbeat::reader(path.clone());
        // No file yet → reader is never-ticked / not live.
        assert_eq!(reader.last_tick_unix(), None);
        assert!(!reader.is_live(86_400));

        // Writer stamps → mirrors to the file (first stamp always writes:
        // last_write starts at 0, so the throttle gap is satisfied).
        let writer = WatcherHeartbeat::with_sidecar(path.clone());
        writer.stamp();
        assert!(path.exists(), "sidecar file must be written on first stamp");

        // A fresh reader over the same path now sees a live heartbeat.
        let reader2 = WatcherHeartbeat::reader(path.clone());
        assert!(reader2.last_tick_unix().is_some());
        assert!(reader2.is_live(30), "reader must see the writer's stamp as live");
        assert!(matches!(reader2.age_secs(), Some(a) if a <= 1));

        // `reader` mode never writes, even on stamp.
        let before = std::fs::read_to_string(&path).unwrap();
        reader2.stamp();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reader_treats_garbage_file_as_never_ticked() {
        let path = std::env::temp_dir().join(format!("wc_hb_garbage_{}", std::process::id()));
        std::fs::write(&path, b"not-a-number").unwrap();
        let reader = WatcherHeartbeat::reader(path.clone());
        assert_eq!(reader.last_tick_unix(), None);
        assert!(!reader.is_live(86_400));
        std::fs::remove_file(&path).ok();
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
