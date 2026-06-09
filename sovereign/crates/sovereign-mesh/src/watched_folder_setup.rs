// SPDX-License-Identifier: AGPL-3.0-or-later
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

use sovereign_tools::local_corpus::config::{ReconcileKind, SyncMode};
use sovereign_tools::local_corpus::watched::events::EventSink;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::watched::scheduler::{
    ScheduleCancel, Scheduler, SchedulerConfig,
};
use sovereign_tools::local_corpus::watched::worker::Worker;
use sovereign_tools::local_corpus::LocalCorpusManager;

/// Default sweep cadence for obsidian vaults registered with the
/// reconciliation worker. Matches the watched-folder
/// `WatchedFolderConfig::default().sweep_interval_secs` (120s) so the
/// two corpus types behave identically under the daemon's dispatch
/// loop. The scheduler also enforces a hard 60s floor; tuning lower
/// has no effect.
const OBSIDIAN_DEFAULT_SWEEP_INTERVAL_SECS: u64 = 120;

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

        // Auto-resume: every persisted reconcilable corpus (watched
        // folder OR obsidian vault) gets re-registered so the
        // scheduler picks it up on the next tick. Idempotent.
        // Threading sync_mode through for watched folders is what
        // restores Manual-mode behaviour after a daemon restart —
        // without it, every Manual corpus would revert to Continuous
        // on the very next tick. Obsidian vaults always register as
        // Continuous on a fixed cadence; per-vault sweep tuning is
        // deferred until a tuning need surfaces.
        let reconcilable = manager.list_reconcilable().await;
        let mut watched_count = 0_usize;
        let mut obsidian_count = 0_usize;
        for cfg in &reconcilable {
            match cfg.source_type.reconcile_kind() {
                Some(ReconcileKind::WatchedFolder) => {
                    if let Some(wf) = cfg.source_type.watched_config() {
                        registry
                            .register_with_mode(
                                cfg.id.clone(),
                                wf.sweep_interval_secs,
                                wf.sync_mode,
                            )
                            .await;
                        watched_count += 1;
                    }
                }
                Some(ReconcileKind::ObsidianVault) => {
                    registry
                        .register_with_mode(
                            cfg.id.clone(),
                            OBSIDIAN_DEFAULT_SWEEP_INTERVAL_SECS,
                            SyncMode::Continuous,
                        )
                        .await;
                    obsidian_count += 1;
                }
                None => {}
            }
        }
        if watched_count > 0 || obsidian_count > 0 {
            tracing::info!(
                watched = watched_count,
                obsidian = obsidian_count,
                "watched_folder:resumed_corpora"
            );
        }

        // Sink fans every worker event into the manager so the
        // auto-rebuild watchdog can debounce tiered rebuilds against
        // `SweepCompleted` events (Move 8 — folder-ingest v1 §3.6).
        // The watchdog short-circuits non-`SweepCompleted` variants,
        // so this is cheap on the dispatcher hot path.
        //
        // HTTP /watch/status still reads from the per-corpus state
        // file when the user asks; a future Tauri-event bridge can
        // chain a second sink in front of this one to fan events
        // onto the desktop progress drawer without disturbing the
        // watchdog wiring.
        let sink_manager = Arc::clone(&manager);
        let sink: EventSink = Arc::new(move |event| {
            let m = Arc::clone(&sink_manager);
            tokio::spawn(async move {
                m.on_sweep_event(&event).await;
            });
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
        crate::watched_folder_runtime::install(Arc::clone(&manager), Arc::clone(&registry));
        crate::watched_folder_runtime::set_cancel(cancel_tx);

        // Mount the watched-folder HTTP routes on the daemon's
        // loopback-only listener. Reads the singleton internally,
        // so no Arc threading.
        daemon
            .install_corpus_watch_http_router(crate::corpus_watch_http::corpus_watch_router())
            .await;

        let handle = Scheduler::spawn(registry, worker, cancel_token, scheduler_cfg);
        Self {
            _scheduler_handle: handle,
        }
    }
}
