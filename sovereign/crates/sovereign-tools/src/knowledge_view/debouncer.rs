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
    /// A memory was written → refresh the personal view, and queue the
    /// id for the incremental memory-tree drain (`mem_tree`) that runs
    /// on the same debounce window.
    MemoryTouched { memory_id: String },
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
///
/// `mem_atlas` is the late-installed handle pair for the memory-pool
/// RAPTOR rebuild (T3 of the tiered-retrieval memory port). It rides
/// the SAME debounce window as the personal view — every fire of
/// `ViewKind::Personal` also rebuilds the per-scope memory trees —
/// so memory writes never trigger synchronous enrichment on the
/// witness turn. `None` (never installed) = feature inert.
pub(crate) fn spawn_debouncer(
    engine: Arc<CorpusEngine>,
    inference: InferenceFn,
    views: Arc<RwLock<HashMap<String, ViewEntry>>>,
    mut rx: mpsc::UnboundedReceiver<ViewEvent>,
    mem_atlas: Arc<RwLock<Option<crate::mem_atlas::MemAtlasHandles>>>,
) {
    tokio::spawn(async move {
        let mut state: HashMap<String, PendingView> = HashMap::new();
        // Memory ids written since the last personal-view fire — the
        // incremental tree (`mem_tree`) drains exactly these instead of
        // re-clustering the whole pool.
        let mut pending_memory_ids: Vec<String> = Vec::new();

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
                            ViewEvent::MemoryTouched { memory_id } => {
                                note(&mut state, ViewKind::Personal.id());
                                pending_memory_ids.push(memory_id);
                            }
                            ViewEvent::ConversationTouched |
                            ViewEvent::ConversationDeleted => {
                                note(&mut state, ViewKind::Conversational.id());
                            }
                            ViewEvent::Manual { view_id } => {
                                // Manual triggers bypass the debounce window.
                                run_enrichment(&engine, inference.clone(), &views, &view_id).await;
                                if view_id == ViewKind::Personal.id() {
                                    drain_memory_atlas(&mem_atlas, std::mem::take(&mut pending_memory_ids)).await;
                                }
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
                if view_id == ViewKind::Personal.id() {
                    drain_memory_atlas(&mem_atlas, std::mem::take(&mut pending_memory_ids)).await;
                }
                state.remove(&view_id);
            }
        }
    });
}

/// Drain the debounced memory writes through the incremental tree —
/// one `mem_tree::insert_memory` per touched id, O(path) rows each,
/// instead of a whole-pool batch rebuild. (Bootstrap and degeneration
/// still fall through to the batch builder INSIDE `insert_memory`'s
/// ladder.) Soft-fails like `run_enrichment` — the debouncer loop must
/// survive a flaky inference provider.
///
/// Ids that no longer resolve in the pool (deleted or superseded
/// between write and drain) are skipped: recall never boosts an
/// out-of-pool id, and the tree's op-4 rebuild reconciles membership
/// wholesale.
async fn drain_memory_atlas(
    mem_atlas: &Arc<RwLock<Option<crate::mem_atlas::MemAtlasHandles>>>,
    ids: Vec<String>,
) {
    let handles = { mem_atlas.read().await.clone() };
    let Some(h) = handles else { return };
    if ids.is_empty() {
        return;
    }
    let pool = match h.store.get_all_memories().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "memory-tree drain: pool read failed");
            return;
        }
    };
    let by_id: HashMap<&str, &sovereign_core::types::Memory> =
        pool.iter().map(|m| (m.id.as_str(), m)).collect();

    let mut seen = std::collections::HashSet::new();
    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(memory) = by_id.get(id.as_str()) else {
            skipped += 1;
            continue;
        };
        let scope = sovereign_core::traits::MemoryScope::from_conversation_skill(
            memory.source_skill_id.as_deref(),
        );
        match crate::mem_tree::insert_memory(&h.inference, h.store.as_ref(), &scope, memory).await
        {
            Ok(trace) => {
                inserted += 1;
                tracing::debug!(memory_id = %id, op = ?trace.op, "memory-tree drain: inserted");
            }
            Err(e) => {
                tracing::warn!(memory_id = %id, error = %e, "memory-tree drain: insert failed");
            }
        }
    }
    tracing::info!(inserted, skipped, "memory-tree drain complete (debounced)");
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
