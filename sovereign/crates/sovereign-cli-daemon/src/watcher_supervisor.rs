//! Self-healing supervisor for the lint/test watcher coordinator.
//!
//! ## Why this exists
//!
//! The daemon used to start the [`WatcherCoordinator`] once and hold its
//! handle for the process lifetime. If the coordinator's loop task
//! panicked, or the OS `notify` watcher died, nothing noticed — the
//! daemon's one-shot `watcher_active` bool stayed `true` and the
//! `lint_status`/`test_status` tools kept serving an increasingly stale
//! result while asserting the watcher was live. The watcher "silently
//! went stale."
//!
//! The supervisor closes that gap from the other side of the
//! [`WatcherHeartbeat`]: it owns the coordinator handle, polls liveness
//! on an interval (the loop task finished, OR the heartbeat stopped
//! advancing), and rebuilds + restarts the coordinator when it finds it
//! dead. The same heartbeat the tools read for honesty, the supervisor
//! reads for recovery — one signal, two consumers.
//!
//! Restart is bounded by exponential backoff so a persistently-failing
//! start (e.g. a broken watch root) can't become a hot loop; between
//! attempts the heartbeat stays stale, so the tools honestly report
//! `watcher_dead` and callers fall back to a direct `cargo` run.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::{
    BackgroundWatcher, CoordinatorHandle, WatcherCoordinator, WatcherHeartbeat,
};
use tokio::task::JoinHandle;

/// How often the monitor re-checks coordinator liveness.
const CHECK_INTERVAL: Duration = Duration::from_secs(15);

/// The monitor treats the coordinator as dead when the heartbeat hasn't
/// advanced within this window. Deliberately wider than the status
/// tools' 30s liveness window so the supervisor only acts on a
/// genuinely-stuck loop, never a merely-slow one — and so the tools flip
/// to `watcher_dead` *before* the supervisor restarts, never after.
const RESTART_STALE_WINDOW_SECS: u64 = 60;

/// Backoff bounds for restart attempts.
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(120);

/// Owns everything needed to (re)build a coordinator so the monitor can
/// restart it without the daemon re-deriving the inputs.
pub struct WatcherSupervisor {
    watchers: Vec<Arc<dyn BackgroundWatcher>>,
    watch_paths: Vec<PathBuf>,
    debounce_ms: u64,
    heartbeat: Arc<WatcherHeartbeat>,
}

impl WatcherSupervisor {
    pub fn new(
        watchers: Vec<Arc<dyn BackgroundWatcher>>,
        watch_paths: Vec<PathBuf>,
        debounce_ms: u64,
        heartbeat: Arc<WatcherHeartbeat>,
    ) -> Self {
        Self {
            watchers,
            watch_paths,
            debounce_ms,
            heartbeat,
        }
    }

    /// Build and start one coordinator instance, sharing the supervisor's
    /// heartbeat so both this monitor and the status tools observe the
    /// same liveness.
    async fn start_once(&self) -> corpus_engine::error::Result<CoordinatorHandle> {
        let mut coordinator = WatcherCoordinator::new(self.debounce_ms)
            .with_heartbeat(Arc::clone(&self.heartbeat));
        for w in &self.watchers {
            coordinator.register(Arc::clone(w));
        }
        coordinator.start(self.watch_paths.clone()).await
    }

    /// True iff `handle` is both running and stamping its heartbeat. A
    /// finished task is the fast signal for a panicked loop; a frozen
    /// heartbeat catches a loop that's alive-but-wedged.
    fn is_healthy(&self, handle: &CoordinatorHandle) -> bool {
        handle.is_alive() && self.heartbeat.is_live(RESTART_STALE_WINDOW_SECS)
    }

    /// Start the coordinator and spawn the monitor task. The returned
    /// [`JoinHandle`] must be held for the daemon's lifetime: dropping it
    /// aborts the monitor, which drops the live `CoordinatorHandle` and
    /// shuts the watcher down. Returns `None` only if there is nothing to
    /// watch (no registered watchers / no paths).
    pub fn spawn(self) -> Option<JoinHandle<()>> {
        if self.watchers.is_empty() || self.watch_paths.is_empty() {
            return None;
        }
        Some(tokio::spawn(async move {
            self.run().await;
        }))
    }

    async fn run(self) {
        // `None` means "no live coordinator right now" — the loop below
        // (re)starts it. This unifies cold start and restart-after-death.
        let mut current: Option<CoordinatorHandle> = None;
        let mut backoff = INITIAL_BACKOFF;

        loop {
            let needs_start = match &current {
                None => true,
                Some(h) => !self.is_healthy(h),
            };

            if needs_start {
                if current.is_some() {
                    tracing::error!(
                        "watcher coordinator is not healthy (task finished or heartbeat \
                         stale > {RESTART_STALE_WINDOW_SECS}s) — restarting"
                    );
                    // Drop the dead/wedged handle: its Drop aborts the old
                    // task and releases the notify watcher before we
                    // build a fresh one.
                    current = None;
                }
                match self.start_once().await {
                    Ok(handle) => {
                        tracing::info!(
                            watchers = self.watchers.len(),
                            "watcher coordinator (re)started by supervisor"
                        );
                        current = Some(handle);
                        backoff = INITIAL_BACKOFF;
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            backoff_secs = backoff.as_secs(),
                            "watcher coordinator start failed; retrying after backoff \
                             (status tools will report watcher_dead meanwhile)"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                }
            }

            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use corpus_engine::WatcherStatus;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NoopWatcher {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BackgroundWatcher for NoopWatcher {
        fn id(&self) -> &'static str {
            "noop"
        }
        fn description(&self) -> &'static str {
            "test no-op"
        }
        async fn on_files_changed(&self, _paths: Vec<PathBuf>) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        async fn current_status(&self) -> WatcherStatus {
            WatcherStatus::NeverRun
        }
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wsup_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// No watchers / no paths → nothing to supervise, `spawn` returns None
    /// rather than spinning a monitor that can never start anything.
    #[test]
    fn spawn_returns_none_when_empty() {
        let hb = WatcherHeartbeat::new();
        let sup = WatcherSupervisor::new(vec![], vec![tmp_dir("empty")], 200, hb);
        assert!(sup.spawn().is_none());

        let hb2 = WatcherHeartbeat::new();
        let w: Arc<dyn BackgroundWatcher> = Arc::new(NoopWatcher {
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let sup2 = WatcherSupervisor::new(vec![w], vec![], 200, hb2);
        assert!(sup2.spawn().is_none());
    }

    /// The supervisor starts the coordinator (heartbeat goes live) and the
    /// monitor keeps running. Holding the returned handle keeps it alive.
    #[tokio::test]
    async fn supervisor_starts_and_keeps_heartbeat_live() {
        let dir = tmp_dir("start");
        let hb = WatcherHeartbeat::new();
        let w: Arc<dyn BackgroundWatcher> = Arc::new(NoopWatcher {
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let sup = WatcherSupervisor::new(
            vec![w],
            vec![dir.clone()],
            200,
            Arc::clone(&hb),
        );
        let monitor = sup.spawn().expect("supervisor should spawn");

        // Give the monitor a beat to run start_once.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(hb.is_live(30), "heartbeat must be live once supervisor starts");

        monitor.abort();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `is_healthy` is false for a never-stamped heartbeat even if the
    /// task is alive — the wedged-loop case the old bool couldn't catch.
    #[tokio::test]
    async fn is_healthy_false_when_heartbeat_stale() {
        let dir = tmp_dir("health");
        // A supervisor whose heartbeat we control independently of any
        // coordinator: start a coordinator with a DIFFERENT heartbeat so
        // the supervisor's own heartbeat never advances.
        let sup_hb = WatcherHeartbeat::new(); // never stamped
        let coord_hb = WatcherHeartbeat::new();
        let w: Arc<dyn BackgroundWatcher> = Arc::new(NoopWatcher {
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let sup = WatcherSupervisor::new(vec![w], vec![dir.clone()], 200, sup_hb);
        // Build a live handle bound to coord_hb (alive task, but the
        // supervisor's heartbeat is stale).
        let handle = WatcherCoordinator::new(200)
            .with_heartbeat(Arc::clone(&coord_hb))
            .start(vec![dir.clone()])
            .await
            .unwrap();
        assert!(handle.is_alive());
        assert!(
            !sup.is_healthy(&handle),
            "stale supervisor heartbeat must read unhealthy even with a live task"
        );
        let _ = Path::new(""); // silence unused import in some cfgs
        std::fs::remove_dir_all(&dir).ok();
    }
}
