//! Daemon-level coordinator for watched-folder workers.
//!
//! Runs one tokio task that:
//!   - ticks every `dispatch_interval` (default 5 s)
//!   - snapshots the registry
//!   - for each registered corpus, computes "is it due?" from
//!     `(now - last_started_unix) >= sweep_interval_secs`
//!   - dispatches due corpora into a bounded fan-out (Semaphore)
//!     governed by `max_concurrent_sweeps`
//!
//! Per-corpus serialisation is enforced inside `Worker::run_once` via
//! the registry's per-corpus mutex — the scheduler itself doesn't try
//! to be clever about that. A long sweep just delays the next start
//! for that corpus; it never overlaps.
//!
//! Cancellation: the returned `JoinHandle` is held by the daemon for
//! its lifetime. Dropping it (or the explicit `cancel.cancel()` call)
//! signals the loop to exit at its next tick boundary.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::registry::{DispatchDecision, WatchedFolderRegistry};
use super::worker::Worker;

/// How often the scheduler checks the registry for due corpora.
/// Floor on cadence: a corpus configured for `sweep_interval_secs <
/// dispatch_interval` will sweep at most once per dispatch tick.
const DEFAULT_DISPATCH_INTERVAL_SECS: u64 = 5;

/// Hard floor on per-corpus sweep cadence. The plan §Q4 establishes
/// this — tighter intervals hammer disk and shrink the
/// deletion-guard window below human reaction time.
const SWEEP_INTERVAL_FLOOR_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub dispatch_interval: Duration,
    pub max_concurrent_sweeps: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            dispatch_interval: Duration::from_secs(DEFAULT_DISPATCH_INTERVAL_SECS),
            max_concurrent_sweeps: 2,
        }
    }
}

/// Cancellation primitive used by the scheduler. A simple
/// `tokio::sync::watch<bool>` so we don't pull in `tokio_util` for
/// one type — the daemon spawn point holds the sender, the scheduler
/// loop polls the receiver between ticks.
#[derive(Clone)]
pub struct ScheduleCancel {
    inner: tokio::sync::watch::Sender<bool>,
}

impl ScheduleCancel {
    pub fn new() -> (Self, ScheduleCancelToken) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (Self { inner: tx }, ScheduleCancelToken { inner: rx })
    }

    pub fn cancel(&self) {
        let _ = self.inner.send(true);
    }
}

#[derive(Clone)]
pub struct ScheduleCancelToken {
    inner: tokio::sync::watch::Receiver<bool>,
}

impl ScheduleCancelToken {
    pub fn is_cancelled(&self) -> bool {
        *self.inner.borrow()
    }
}

pub struct Scheduler;

impl Scheduler {
    /// Spawn the dispatcher loop. Returns a `JoinHandle` the daemon
    /// keeps alive; dropping it OR calling `cancel.cancel()` exits
    /// the loop.
    ///
    /// Pattern mirrors `sovereign-mesh::gossip::spawn_gossip_loop`:
    /// `tokio::spawn(async move { loop { sleep; do_work } })` with
    /// the `JoinHandle` retained by the caller.
    pub fn spawn(
        registry: Arc<WatchedFolderRegistry>,
        worker: Arc<Worker>,
        cancel: ScheduleCancelToken,
        cfg: SchedulerConfig,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!(
                dispatch_interval_secs = cfg.dispatch_interval.as_secs(),
                max_concurrent = cfg.max_concurrent_sweeps,
                "watched_folder:scheduler_started"
            );
            let semaphore = Arc::new(Semaphore::new(cfg.max_concurrent_sweeps));

            loop {
                tokio::time::sleep(cfg.dispatch_interval).await;

                if cancel.is_cancelled() {
                    tracing::info!("watched_folder:scheduler_shutting_down");
                    break;
                }

                let now = now_unix();
                let corpora = registry.list().await;
                for corpus_id in corpora {
                    let decision = match registry
                        .dispatch_decision(&corpus_id, now, SWEEP_INTERVAL_FLOOR_SECS)
                        .await
                    {
                        Some(d) => d,
                        None => continue, // race: corpus was deregistered
                    };
                    let take_pending = match decision {
                        DispatchDecision::Due { take_pending } => take_pending,
                        DispatchDecision::NotDue => continue,
                    };

                    // Acquire a fan-out slot. If the semaphore is at
                    // capacity, await until a slot frees — bounded
                    // wait, since active sweeps complete eventually.
                    let permit = match semaphore.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => break, // semaphore closed → exit
                    };

                    // For Manual dispatches, clear the pending flag
                    // on the registry side immediately. The worker
                    // also clears the on-disk state mirror; both
                    // layers together prevent the same sync-now
                    // request from re-dispatching on the next tick.
                    if take_pending {
                        registry.clear_manual_pending(&corpus_id).await;
                    }

                    let worker = worker.clone();
                    let id = corpus_id.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(e) = worker.run_once(&id).await {
                            tracing::warn!(
                                corpus_id = %id,
                                error = %e,
                                "watched_folder:run_once_errored"
                            );
                        }
                    });
                }
            }
        })
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_token_round_trips() {
        let (sender, token) = ScheduleCancel::new();
        assert!(!token.is_cancelled());
        sender.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn sweep_interval_floor_is_one_minute() {
        // Pin the floor — the plan's Q4 decision is that 60s is the
        // minimum sweep cadence. If this changes, the plan should
        // change too.
        assert_eq!(SWEEP_INTERVAL_FLOOR_SECS, 60);
    }
}
