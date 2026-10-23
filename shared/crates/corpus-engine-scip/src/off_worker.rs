// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's synchronous bulk work, run off the async workers.
//!
//! Decoding a SCIP index and writing, checking or copying a graph's rows is
//! seconds of synchronous work on a large workspace (1.84M refs for this one).
//! Inline in an async fn it holds the worker polling it for its whole length,
//! and the reindexer's rebuild is woken by its exporter's exit, so it runs on
//! the worker that just left the runtime's I/O driver: until that poll returns
//! no worker polls the driver, and every accept, read and timer in the process
//! waits on it. Measured on the daemon 2026-10-16: stalls of up to 113 s at a
//! rebuild's tail, `/v1/models` and `/status` held and then released in one
//! burst. Every bulk step goes through [`run`]; `off_worker_tests` holds each.

use std::time::Instant;

/// Run `work` on tokio's blocking pool and wait for it without holding a
/// worker. A panic in `work` resumes on the caller, as it would have inline.
/// `Err` is the one outcome inline execution could not produce: the runtime
/// shut down before the pool ran `work`.
pub(crate) async fn run<T, F>(op: &'static str, work: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let started = Instant::now();
    match tokio::task::spawn_blocking(work).await {
        Ok(out) => {
            tracing::debug!(
                op,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "scip: bulk work ran on the blocking pool"
            );
            Ok(out)
        }
        Err(e) if e.is_panic() => {
            tracing::error!(
                op,
                "scip: bulk work panicked on the blocking pool; resuming it on the caller"
            );
            std::panic::resume_unwind(e.into_panic())
        }
        Err(e) => {
            tracing::error!(op, error = %e, "scip: bulk work never ran: the blocking pool shut down");
            Err(format!(
                "{op}: the blocking pool shut down before it ran: {e}"
            ))
        }
    }
}

#[cfg(test)]
#[path = "off_worker_tests.rs"]
mod tests;
