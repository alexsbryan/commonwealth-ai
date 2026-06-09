// SPDX-License-Identifier: AGPL-3.0-or-later
//! Debounced Tier-3 enrichment runner.
//!
//! Writes to the state store fire `ViewEvent`s into an unbounded
//! mpsc channel. The background task in this module coalesces them
//! per-view: the first event starts a pending-window; subsequent
//! events bump a counter. When either the counter crosses
//! `DEBOUNCE_MAX_WRITES` or `DEBOUNCE_MAX_IDLE` elapses, the task
//! runs `FieldModelEngine::enrich` for that view and clears the
//! window.
//!
//! Separated from `manager.rs` so the timing policy and the
//! enrichment mechanics are each testable in isolation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use corpus_engine::engine::CorpusEngine;
use corpus_engine::enrichment::field_engine::FieldModelEngine;
use corpus_engine::recipe::Recipe;
use corpus_engine::types::InferenceFn;
use corpus_engine::EnrichmentProgress;
use tokio::sync::{mpsc, RwLock};

use super::view_kind::ViewKind;

/// How many pending writes can accumulate for a single view before
/// we force an enrichment run, regardless of how recent the last
/// write was. Chosen empirically: a burst of 20+ memory writes
/// usually means a session is wrapping up or a bulk import is
/// happening, either of which is worth re-enriching eagerly.
pub(crate) const DEBOUNCE_MAX_WRITES: usize = 20;

/// Longest we wait after the *first* pending write before running
/// enrichment, even if the write counter never reaches
/// `DEBOUNCE_MAX_WRITES`. Five minutes is a reasonable upper bound
/// for user-perceived staleness of the landscape digest.
pub(crate) const DEBOUNCE_MAX_IDLE: Duration = Duration::from_secs(300);

/// Per-view entry that the debouncer locks against ingest + enrich.
/// Defined here so the debouncer owns the types it needs; the
/// manager re-exports / embeds it as `ViewEntry`.
pub(crate) struct ViewEntry {
    pub(crate) recipe: Recipe,
    /// Serialises long-running ingest + enrichment for this view.
    /// Manager `ingest_view` grabs the same lock so the two paths
    /// can't race on the skeleton.json write.
    pub(crate) lock: Arc<tokio::sync::Mutex<()>>,
}

/// Write-side events that flow into the debouncer. Each maps to a
/// view id; the debouncer tracks a pending-write counter per view.
#[derive(Debug, Clone)]
pub(crate) enum ViewEvent {
    /// A memory was written → refresh the personal view.
    MemoryTouched,
    /// A conversation had new activity → refresh conversation view.
    ConversationTouched,
    /// A conversation was deleted → refresh the conversation view
    /// so the deleted conversation's chunks drop out of the index.
    ConversationDeleted,
    /// Explicit manual trigger (e.g. CLI command, startup check).
    Manual { view_id: String },
}

/// Start the background debouncer task. Returns immediately; the
/// task runs until the `rx` channel is closed (i.e. until all
/// `KnowledgeViewManager` clones are dropped).
pub(crate) fn spawn_debouncer(
    engine: Arc<CorpusEngine>,
    inference: InferenceFn,
    views: Arc<RwLock<HashMap<String, ViewEntry>>>,
    mut rx: mpsc::UnboundedReceiver<ViewEvent>,
) {
    tokio::spawn(async move {
        let mut state: HashMap<String, PendingView> = HashMap::new();

        loop {
            let wakeup = state
                .values()
                .map(|p| p.earliest_deadline())
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
            let sleep = tokio::time::sleep_until(wakeup.into());
            tokio::pin!(sleep);

            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => match event {
                            ViewEvent::MemoryTouched => {
                                note(&mut state, ViewKind::Personal.id());
                            }
                            ViewEvent::ConversationTouched |
                            ViewEvent::ConversationDeleted => {
                                note(&mut state, ViewKind::Conversational.id());
                            }
                            ViewEvent::Manual { view_id } => {
                                // Manual triggers bypass the debounce window.
                                run_enrichment(&engine, inference.clone(), &views, &view_id).await;
                                state.remove(&view_id);
                            }
                        },
                        None => break, // Manager dropped, channel closed.
                    }
                }
                _ = &mut sleep => {
                    // Fall through to the deadline sweep below.
                }
            }

            let now = Instant::now();
            let ready: Vec<String> = state
                .iter()
                .filter(|(_, p)| p.is_ready(now))
                .map(|(k, _)| k.clone())
                .collect();
            for view_id in ready {
                run_enrichment(&engine, inference.clone(), &views, &view_id).await;
                state.remove(&view_id);
            }
        }
    });
}

/// One view's in-progress debounce window.
struct PendingView {
    first_pending_at: Instant,
    pending_count: usize,
}

impl PendingView {
    fn earliest_deadline(&self) -> Instant {
        self.first_pending_at + DEBOUNCE_MAX_IDLE
    }

    fn is_ready(&self, now: Instant) -> bool {
        self.pending_count >= DEBOUNCE_MAX_WRITES
            || now.duration_since(self.first_pending_at) >= DEBOUNCE_MAX_IDLE
    }
}

/// Bump the pending counter for one view, starting the window on
/// the first write.
fn note(state: &mut HashMap<String, PendingView>, view_id: &str) {
    let entry = state.entry(view_id.to_string()).or_insert(PendingView {
        first_pending_at: Instant::now(),
        pending_count: 0,
    });
    entry.pending_count += 1;
}

/// Run one full enrichment pass for `view_id`. Resolves the recipe,
/// acquires the per-view lock, opens the index, builds a
/// `FieldModelEngine`, and invokes `enrich()`. Soft-fails: logs and
/// returns on any error so the debouncer loop keeps running.
async fn run_enrichment(
    engine: &Arc<CorpusEngine>,
    inference: InferenceFn,
    views: &Arc<RwLock<HashMap<String, ViewEntry>>>,
    view_id: &str,
) {
    let (recipe, lock) = {
        let guard = views.read().await;
        match guard.get(view_id) {
            Some(v) => (v.recipe.clone(), v.lock.clone()),
            None => {
                tracing::warn!(view_id, "unknown view in debouncer");
                return;
            }
        }
    };

    // Hold the per-view mutex across the entire enrichment. Prevents
    // two overlapping enrichment runs from racing on skeleton.json
    // write or the LanceDB checkpoint.
    let _guard = lock.lock().await;

    let index = match engine.open_index_for_corpus(view_id).await {
        Ok(idx) => idx,
        Err(e) => {
            tracing::debug!(
                view_id,
                error = %e,
                "skipping enrichment — index not available yet"
            );
            return;
        }
    };

    let embed = engine.embed_fn();
    let field_engine = match FieldModelEngine::from_recipe(&recipe, embed, inference) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(view_id, error = %e, "failed to construct FieldModelEngine");
            return;
        }
    };

    let progress = |p: EnrichmentProgress| {
        tracing::debug!(view_id, ?p, "enrichment progress");
    };
    match field_engine.enrich(&index, &progress).await {
        Ok(stats) => tracing::info!(view_id, ?stats, "enrichment complete"),
        Err(e) => tracing::warn!(view_id, error = %e, "enrichment failed"),
    }
}
