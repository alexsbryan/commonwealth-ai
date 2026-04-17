//! Panic-boundary supervisor for watcher tasks.
//!
//! Every long-running watcher the daemon spawns (reindexer, test
//! runner, lint runner, config reload loop) goes through
//! [`supervise`]. The supervisor:
//!
//! 1. Runs the watcher body on its own tokio task with
//!    [`catch_unwind`](futures::FutureExt::catch_unwind) so a panic
//!    cannot propagate into the caller.
//! 2. Records the outcome in a [`ProjectState`] so MCP tools / HTTP
//!    endpoints can surface it ("watcher crashed 3 times, last
//!    reason: …").
//! 3. Restarts crashed watchers with exponential backoff, up to
//!    [`MAX_AUTO_RESTARTS`]. Past that, the watcher parks in
//!    [`WatcherStatus::Disabled`] until an operator runs
//!    `sovereign project watch restart`.
//!
//! **Isolation guarantee.** A `test_runner` that panics takes out
//! its own supervised task and nothing else — not the SCIP
//! rebuilder, not `/v1/chat/completions`, not the mesh. This is
//! load-bearing: user-authored scripts are routinely broken during
//! iteration.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::projects::{ProjectState, WatcherKind, WatcherStatus};

/// Crash count past which the supervisor stops auto-restarting.
/// Five gives room for the typical "I just changed my script and
/// it panics; next save will fix it" iteration without pinning a
/// tight crash loop for minutes.
pub const MAX_AUTO_RESTARTS: usize = 5;

/// Starting backoff after the first crash. Doubles each failure up
/// to [`MAX_BACKOFF`].
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Handle for a supervised task. Dropping it does not stop the
/// watcher; call [`abort`](Self::abort) for that. The supervisor
/// records [`WatcherStatus::Aborted`] when cancelled so tools
/// report "watcher stopped" rather than "watcher crashed".
///
/// Cancellation uses an explicit `oneshot` shutdown channel rather
/// than `JoinHandle::abort()` so the supervisor has a chance to
/// record `Aborted` cleanly. Using `abort()` tears the task down
/// mid-statement and the status never updates.
pub struct SupervisedTask {
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    handle: JoinHandle<()>,
}

impl SupervisedTask {
    /// Ask the supervised task to stop at its next `await` point.
    /// Idempotent. The supervisor writes [`WatcherStatus::Aborted`]
    /// once it observes the signal; `join()` then returns.
    pub fn abort(&self) {
        if let Ok(mut slot) = self.shutdown.lock() {
            if let Some(tx) = slot.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Await the supervisor's completion. Mostly useful in tests
    /// and in the project-unregister path; production callers
    /// usually keep the handle alive for the lifetime of the
    /// project registration.
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.handle.await
    }
}

/// Spawn `build()` under the supervisor. `build` is a factory so
/// the supervisor can call it again after each crash to produce a
/// fresh future (you cannot re-poll a future that already panicked).
///
/// Contract:
/// - `build` is expected to run until the task's natural end (e.g.
///   `shutdown_rx` fires) or panic. A clean return is treated as
///   "watcher decided it's done", transitions to `Idle`, and the
///   supervisor exits. This is how config-reload swaps work: the
///   old watcher returns cleanly after receiving a signal, new one
///   is supervised fresh.
pub fn supervise<F, Fut>(
    kind: WatcherKind,
    state: Arc<ProjectState>,
    build: F,
) -> SupervisedTask
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    supervise_with_backoff(kind, state, build, INITIAL_BACKOFF, MAX_BACKOFF)
}

/// Variant of [`supervise`] that lets tests (and future operator
/// knobs) shorten the restart backoff. Production code should use
/// [`supervise`] so the behaviour is consistent.
pub fn supervise_with_backoff<F, Fut>(
    kind: WatcherKind,
    state: Arc<ProjectState>,
    build: F,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> SupervisedTask
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let name = kind.as_str();
        let mut crashes = 0usize;
        let mut backoff = initial_backoff;

        loop {
            state.set(kind, WatcherStatus::Active).await;

            // Race the watcher body against shutdown. `catch_unwind`
            // contains panics inside the body; `select!` gives us a
            // cooperative cancellation point so we can write the
            // `Aborted` status before returning.
            let body = AssertUnwindSafe(build()).catch_unwind();
            tokio::pin!(body);

            let outcome: BodyOutcome = tokio::select! {
                biased;
                _ = &mut shutdown_rx => BodyOutcome::Aborted,
                result = &mut body => match result {
                    Ok(()) => BodyOutcome::Clean,
                    Err(panic) => BodyOutcome::Panicked(panic_message(&panic)),
                },
            };

            match outcome {
                BodyOutcome::Clean => {
                    tracing::info!(watcher = name, "watcher exited cleanly");
                    state.set(kind, WatcherStatus::Idle).await;
                    return;
                }
                BodyOutcome::Aborted => {
                    tracing::info!(watcher = name, "watcher aborted via shutdown");
                    state.set(kind, WatcherStatus::Aborted).await;
                    return;
                }
                BodyOutcome::Panicked(reason) => {
                    crashes += 1;
                    tracing::warn!(
                        watcher = name,
                        crash_count = crashes,
                        reason = %reason,
                        "watcher panicked"
                    );
                    state
                        .set(
                            kind,
                            WatcherStatus::Crashed {
                                reason,
                                count: crashes,
                            },
                        )
                        .await;
                }
            }

            if crashes >= MAX_AUTO_RESTARTS {
                tracing::error!(
                    watcher = name,
                    crashes = crashes,
                    "watcher disabled after repeated crashes"
                );
                state
                    .set(
                        kind,
                        WatcherStatus::Disabled {
                            reason: format!(
                                "crashed {crashes} times; run `sovereign project watch restart` to retry"
                            ),
                        },
                    )
                    .await;
                return;
            }

            // Race backoff sleep against shutdown too, so aborting
            // during the backoff window transitions to Aborted
            // rather than running one more retry.
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    state.set(kind, WatcherStatus::Aborted).await;
                    return;
                }
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(max_backoff);
        }
    });

    SupervisedTask {
        shutdown: Mutex::new(Some(shutdown_tx)),
        handle,
    }
}

enum BodyOutcome {
    Clean,
    Aborted,
    Panicked(String),
}

/// Best-effort extraction of a panic payload's message. `panic!()`
/// wraps the formatted string in `&'static str` or `String`; other
/// payloads show up as `<non-string panic payload>`.
fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A panicking task is caught and surfaced as `Crashed`, then
    /// retried; the second attempt returning cleanly moves the
    /// state to `Idle`.
    #[tokio::test]
    async fn panic_once_then_recover() {
        let state = ProjectState::new("test");
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let task = supervise(WatcherKind::Scip, Arc::clone(&state), move || {
            let attempts = Arc::clone(&attempts_clone);
            async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    panic!("first attempt blows up");
                }
                // Clean return on the second attempt.
            }
        });

        task.join().await.ok();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(state.status(WatcherKind::Scip).await, WatcherStatus::Idle);
    }

    /// After MAX_AUTO_RESTARTS crashes in a row, the supervisor
    /// stops retrying and parks the watcher in `Disabled`. The
    /// mesh / inference subsystems should never see this case.
    #[tokio::test]
    async fn repeated_crashes_trip_auto_disable() {
        let state = ProjectState::new("test");
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = Arc::clone(&attempts);

        // Short backoffs so the test completes in milliseconds.
        let task = supervise_with_backoff(
            WatcherKind::Test,
            Arc::clone(&state),
            move || {
                let attempts = Arc::clone(&attempts_clone);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    panic!("always");
                }
            },
            Duration::from_millis(5),
            Duration::from_millis(20),
        );

        task.join().await.ok();
        assert_eq!(attempts.load(Ordering::SeqCst), MAX_AUTO_RESTARTS);
        match state.status(WatcherKind::Test).await {
            WatcherStatus::Disabled { reason } => {
                assert!(reason.contains("crashed"));
                assert!(reason.contains("restart"));
            }
            other => panic!("expected Disabled, got {other:?}"),
        }
    }

    /// Aborting a healthy watcher moves it cleanly to `Aborted` —
    /// the supervisor distinguishes cancellation from crash so
    /// tools don't falsely blame the user's script.
    #[tokio::test]
    async fn abort_records_aborted_not_crashed() {
        let state = ProjectState::new("test");
        let task = supervise(WatcherKind::Lint, Arc::clone(&state), || async {
            // Run forever until aborted.
            futures::future::pending::<()>().await;
        });

        // Give the task a moment to enter the body and set Active.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(state.status(WatcherKind::Lint).await, WatcherStatus::Active);

        task.abort();
        // Wait for supervisor to observe cancellation + record state.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if matches!(
                state.status(WatcherKind::Lint).await,
                WatcherStatus::Aborted
            ) {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "expected Aborted, got {:?}",
                    state.status(WatcherKind::Lint).await
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Clean return on first run transitions to `Idle` without a
    /// crash loop. This is the config-reload path: the old watcher
    /// sees a shutdown signal, returns cleanly, supervisor exits.
    #[tokio::test]
    async fn clean_exit_leaves_idle() {
        let state = ProjectState::new("test");
        let task = supervise(WatcherKind::Config, Arc::clone(&state), || async { /* return */ });

        task.join().await.ok();
        assert_eq!(
            state.status(WatcherKind::Config).await,
            WatcherStatus::Idle
        );
    }
}
