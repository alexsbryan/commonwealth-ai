// SPDX-License-Identifier: AGPL-3.0-or-later
//! Supervised background-task spawning (DAEMON_RESILIENCE.md P0.4).
//!
//! Before this module the daemon's background loops (notes pollers,
//! RPC worker discovery, log rotation, the memory watch itself) were
//! fire-and-forget `tokio::spawn`s: a panic killed the task silently
//! and permanently — tokio parks the panic in a `JoinHandle` nobody
//! held. Observed failure shape: a panicked memory-watch task silently
//! disarms the OOM defense; a panicked ingest poller silently stops
//! note convergence until the next daemon restart.
//!
//! [`spawn_supervised`] is the antidote for tasks that must survive
//! their own bugs: run the task, and on *panic* rebuild + restart it
//! with exponential backoff, up to a ceiling — the same posture as
//! `sovereign-mesh`'s `supervised_task` (project watchers) and the
//! daemon's `WatcherSupervisor`, minus the heartbeat (these tasks are
//! either loops that only die by panicking, or idempotent one-shots).
//! On ceiling breach the task parks with a loud ERROR naming exactly
//! what is degraded until the next daemon restart. Clean completion is
//! respected — one-shot tasks that finish are NOT restarted.
//!
//! The process-global panic hook (`crate::panic_hook`) still fires for
//! each caught panic, so every restart leaves a crash record; this
//! module adds the *recovery*, not the forensics.

use std::future::Future;
use std::time::Duration;

const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// Panics tolerated before the task parks as degraded. Matches
/// `sovereign-mesh::supervised_task::MAX_AUTO_RESTARTS`.
const MAX_RESTARTS: u32 = 5;

/// Spawn `make()`'s future and keep it alive across panics.
///
/// `make` is called once per (re)start so the closure re-clones its
/// captured `Arc`s into a fresh future. Loop-internal state resets on
/// restart — every task supervised this way is either stateless per
/// iteration or explicitly idempotent (documented at its site).
pub(crate) fn spawn_supervised<F, Fut>(name: &'static str, make: F) -> tokio::task::JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = INITIAL_BACKOFF;
        let mut restarts: u32 = 0;
        loop {
            // Inner spawn so a panic is captured as a `JoinError`
            // instead of unwinding this supervisor.
            let outcome = tokio::spawn(make()).await;
            match outcome {
                Ok(()) => {
                    // Clean completion — one-shot task done, or a loop
                    // that chose to exit (e.g. memory-watch after its
                    // hard-limit self-SIGTERM). Not ours to restart.
                    tracing::debug!(task = name, "supervised background task completed");
                    return;
                }
                Err(e) if e.is_panic() => {
                    restarts += 1;
                    let msg = panic_payload_str(e.into_panic());
                    if restarts > MAX_RESTARTS {
                        tracing::error!(
                            task = name,
                            restarts,
                            panic = %msg,
                            "supervised background task exceeded restart ceiling — \
                             DEGRADED until daemon restart"
                        );
                        return;
                    }
                    tracing::error!(
                        task = name,
                        restarts,
                        backoff_secs = backoff.as_secs(),
                        panic = %msg,
                        "supervised background task panicked — restarting after backoff"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(_) => {
                    // Cancelled — runtime shutting down.
                    return;
                }
            }
        }
    })
}

fn panic_payload_str(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn restarts_on_panic_then_completes() {
        let attempts = Arc::new(AtomicU32::new(0));
        let a = Arc::clone(&attempts);
        let handle = spawn_supervised("test_task", move || {
            let a = Arc::clone(&a);
            async move {
                if a.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("first attempt dies");
                }
            }
        });
        // First attempt panics, second (after 2s backoff) completes.
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("supervisor finishes")
            .expect("supervisor itself never panics");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn clean_completion_is_not_restarted() {
        let attempts = Arc::new(AtomicU32::new(0));
        let a = Arc::clone(&attempts);
        let handle = spawn_supervised("one_shot", move || {
            let a = Arc::clone(&a);
            async move {
                a.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("finishes promptly")
            .expect("no panic");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
