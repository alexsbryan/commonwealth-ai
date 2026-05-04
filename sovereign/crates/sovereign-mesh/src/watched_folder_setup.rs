//! One-call setup for the watched-folder reconciliation subsystem.
//!
//! Both the standalone CLI daemon (`sovereign daemon`) and the
//! desktop's embedded daemon need exactly the same wiring:
//!
//!   1. Build a `WatchedFolderRegistry`
//!   2. Re-register every persisted `WatchedFolder` corpus from
//!      `LocalCorpusManager::list_watched`
//!   3. Park the manager + registry on the `watched_folder_runtime`
//!      singleton so the HTTP routes can reach them
//!   4. Mount the `corpus_watch_http` router on the daemon
//!   5. Spawn the dispatcher loop and stash the cancel sender
//!
//! Without this helper both call sites duplicate the same dozen
//! lines, and a future refactor of the wiring (new dependency, new
//! event sink, etc.) has to land in two places. The CLI daemon
//! already had the inline block — this module factors it out so the
//! desktop can adopt the same shape with one call.

use std::sync::Arc;

use sovereign_tools::local_corpus::watched::events::EventSink;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::watched::scheduler::{
    ScheduleCancel, Scheduler, SchedulerConfig,
};
use sovereign_tools::local_corpus::watched::worker::Worker;
use sovereign_tools::local_corpus::LocalCorpusManager;

use corpus_engine::CorpusEngine;

use crate::daemon::EmbeddedDaemon;

/// Handle to the spawned scheduler loop. The caller holds it for the
/// daemon's lifetime; dropping it (or calling `cancel()`) signals the
/// loop to exit at the next tick.
pub struct WatchedSubsystem {
    /// Scheduler dispatcher loop. Drop = abort.
    pub _scheduler_handle: tokio::task::JoinHandle<()>,
}

impl WatchedSubsystem {
    /// Wire up the full subsystem onto an `EmbeddedDaemon`. Returns
    /// `Some(WatchedSubsystem)` when wiring succeeded, or `None` if
    /// the manager was already populated by a prior call (the
    /// `OnceLock` semantics make double-installation a no-op).
    ///
    /// Pattern matches `daemon_cmd.rs::run_daemon`'s former inline
    /// block. Sink is a no-op by default — the worker emits
    /// structured tracing events itself, so the sink is purely an
    /// extension point for testing or a future progress drawer.
    pub async fn install(
        daemon: Arc<EmbeddedDaemon>,
        engine: Arc<CorpusEngine>,
        manager: Arc<LocalCorpusManager>,
        max_concurrent_sweeps: usize,
    ) -> Self {
        let registry = Arc::new(WatchedFolderRegistry::new());

        // Auto-resume: every persisted WatchedFolder corpus gets
        // re-registered so the scheduler picks it up on the next
        // tick. Idempotent. Threading sync_mode through here is
        // what restores Manual-mode behaviour after a daemon
        // restart — without it, every Manual corpus would revert
        // to Continuous on the very next tick.
        let watched = manager.list_watched().await;
        let mut count = 0_usize;
        for cfg in &watched {
            if let Some(wf) = cfg.source_type.watched_config() {
                registry
                    .register_with_mode(cfg.id.clone(), wf.sweep_interval_secs, wf.sync_mode)
                    .await;
                count += 1;
            }
        }
        if count > 0 {
            tracing::info!(count, "watched_folder:resumed_corpora");
        }

        let sink: EventSink = Arc::new(|_event| {
            // Worker emits structured tracing events alongside sink
            // calls. The sink itself is a no-op; HTTP /watch/status
            // reads state from the per-corpus state file when the
            // user asks. A future Tauri-event bridge would install a
            // sink here to fan events onto the desktop progress
            // drawer.
        });

        let worker = Arc::new(Worker::new(
            Arc::clone(&engine),
            Arc::clone(&manager),
            Arc::clone(&registry),
            sink,
            manager.index_dir_root(),
        ));

        let (cancel_tx, cancel_token) = ScheduleCancel::new();
        let scheduler_cfg = SchedulerConfig {
            max_concurrent_sweeps,
            ..Default::default()
        };

        // Park the manager + registry on the runtime singleton so
        // the corpus_watch HTTP router (and the Tauri commands that
        // proxy through it) can reach them. OnceLock semantics —
        // the second installer's `set` call returns `Err`, leaving
        // the first installer's handles in place. Production
        // expectation: install once.
        crate::watched_folder_runtime::install(
            Arc::clone(&manager),
            Arc::clone(&registry),
        );
        crate::watched_folder_runtime::set_cancel(cancel_tx);

        // Mount the watched-folder HTTP routes on the daemon's
        // loopback-only listener. Reads the singleton internally,
        // so no Arc threading.
        daemon
            .install_corpus_watch_http_router(
                crate::corpus_watch_http::corpus_watch_router(),
            )
            .await;

        let handle = Scheduler::spawn(registry, worker, cancel_token, scheduler_cfg);
        Self {
            _scheduler_handle: handle,
        }
    }
}
