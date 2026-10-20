// SPDX-License-Identifier: AGPL-3.0-or-later
//! How an install or expand attempt's terminal outcome is recorded: the
//! progress callback both spawn paths install, the retiring of the last
//! attempt's record, and the failure record.
//!
//! Moved whole from `corpus_ingest.rs`, which PV3g's `clear_last_attempt`
//! took past the arch gate's 800-line band. Behaviour-preserving; the names
//! are unchanged.

use crate::state::AppState;

// Install and expand run the same status/progress/poller pipeline, so
// they must agree on how a terminal outcome is recorded. These three
// helpers are that agreement in one place; duplicating them is how the
// expand path came to swallow its failures while install reported them.
// The governing invariant is documented on `IngestProgress::Failed`.

/// The progress callback both spawn paths install into the engine.
///
/// Latest-wins per corpus, with one exception: a terminal `Failed`
/// record is never overwritten. Each insert runs in its own spawned
/// task, so ordering against the failure write is NOT guaranteed —
/// and losing that race would park the corpus on a non-terminal phase
/// (say "embedding") with no task running to ever advance it, i.e. a
/// permanent fake spinner in place of the error. Safe across retries
/// because `clear_last_attempt` runs before a new attempt spawns.
pub(super) fn ingest_progress_callback(
    state: AppState,
    corpus_id: String,
) -> corpus_index::ingest_port::ProgressCallback {
    Box::new(move |p| {
        let state = state.clone();
        let corpus_id = corpus_id.clone();
        // The callback is synchronous but the map needs an async lock.
        // Spawn a short-lived task; it finishes essentially instantly.
        tokio::spawn(async move {
            let mut map = state.inner.ingest.corpus_progress.write().await;
            if matches!(
                map.get(&corpus_id),
                Some(sovereign_contracts::daemon_wire::IngestProgress::Failed { .. })
            ) {
                return;
            }
            map.insert(corpus_id, p);
        });
    })
}

/// Retire the last attempt's terminal record, `Failed` or `Complete`, and
/// its documents, so a new attempt starts clean.
///
/// The failure record is deliberately sticky — it has to outlive its
/// task to be reportable at all — which makes the retry responsible for
/// clearing it. Without this, the stale message would sit in the
/// snapshot beside live progress and a UI showing "Install failed" would
/// keep showing it straight through a successful reinstall. A `Complete`
/// goes too (platform row PV3g): kept, a re-install of a held corpus read
/// `complete` with no documents from its spawn to the engine's first
/// callback, where OICP v0.6 §2.9 lists it `pending` ("a new attempt starts
/// it again"), and a client read the library installed and empty.
///
/// In-flight phases cannot be present: the `active_ingests` guard already
/// returned.
pub(super) async fn clear_last_attempt(state: &AppState, corpus_id: &str) {
    // A new attempt's documents are its own: the last install's list goes
    // with it, so no poll mixes the two.
    if let Some(corpus) = state
        .inner
        .node
        .corpus_engine
        .as_ref()
        .and_then(|e| corpus_index::corpus::Corpus::named(e.index_dir(), corpus_id))
    {
        if let Err(e) = corpus.clear_install_documents() {
            tracing::warn!(corpus = %corpus_id, error = %e, "clear_last_attempt: the last install's documents record did not clear");
        }
    }
    let last = state
        .inner
        .ingest
        .corpus_progress
        .write()
        .await
        .remove(corpus_id);
    tracing::debug!(
        corpus = %corpus_id,
        retired = ?last,
        "clear_last_attempt: a new attempt starts its progress entry again"
    );
}

/// Record a terminal failure so `/internal/corpus/status` can report it.
///
/// Not merely a log line: `active_ingests` has already dropped this
/// corpus by the time we get here, and `corpus_status` builds its
/// candidate set from `active_ingests ∪ corpus_progress`. With no entry
/// the corpus vanishes from the response, and the Desktop poller reads
/// "present last tick, absent this tick" as SUCCESS — emitting
/// phase=complete / 100% / "Done" for an install that committed nothing.
pub(super) async fn record_failure(state: &AppState, corpus_id: &str, message: String) {
    state.inner.ingest.corpus_progress.write().await.insert(
        corpus_id.to_string(),
        sovereign_contracts::daemon_wire::IngestProgress::Failed { message },
    );
}
