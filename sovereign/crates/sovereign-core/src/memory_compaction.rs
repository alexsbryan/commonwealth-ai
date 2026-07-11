// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rolling-summary memory compaction (2026-05-23).
//!
//! Inner-work surface witness path
//! ([`crate::Runtime::handle_expressive_query_stream`]) renders up to
//! `PROMPT_RENDER_CAP=3` retrieved memories verbatim into its system
//! prompt. Each turn writes the user's prose as a new `Memory`;
//! retrieval surfaces them on the next turn; the system prompt grows
//! roughly +1500 tokens per turn for a session that stays on one
//! topic. On 2026-05-23 a single inner-work session walked the prompt
//! from 8910 → 16816 tokens across ~7 witness turns and overflowed
//! the loaded context window. See [[witness-memory-rolling-compaction]]
//! plan for the full background.
//!
//! This module is the structural answer to that growth: when a
//! conversation accumulates more than `threshold` non-superseded
//! memories, the worker folds the oldest `batch` into a single
//! `MemoryKind::Summary` row via a fast-slot synthesis call. The
//! originals are marked `superseded_by = <summary.id>` (body
//! preserved for `sovereign memory expand <summary-id>` provenance).
//! Retrieval filters `superseded_by IS NULL` so the prompt sees the
//! summary in place of the originals — bounded by `max_summary_chars`
//! instead of `batch × avg_memory_chars`.
//!
//! ## Recursive collapse
//!
//! When summary memories themselves cross the threshold, the worker
//! folds the oldest summaries the same way — so the total memory
//! count for a conversation is bounded as a geometric series.
//!
//! ## Async by default; sync for the CLI's `rebuild-summaries`
//!
//! [`CompactionWorker::spawn`] starts a background tokio task draining
//! an mpsc channel of conversation_ids. [`Self::maybe_enqueue`] is
//! fire-and-forget. The compaction itself runs on the worker thread,
//! so a save-time hook never blocks the writer's turn.
//!
//! [`Self::run_one_sync`] is the same code path exposed for
//! [`sovereign memory rebuild-summaries`] and integration tests —
//! same synthesis, same store writes, just awaited directly.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::Result;
use crate::slot_policy::Workload;
use crate::traits::{InferenceProvider, MemoryStore};
use crate::types::{CompletionRequest, Memory, MemoryKind};

// The operator-facing config knobs (`CompactionConfig`, `CompactionMode`,
// `DEFAULT_SYNTHESIS_PROMPT`) were relocated to `sovereign-contracts` so
// `SetupConfig` can embed them without dragging this worker. Re-exported here
// so `sovereign_core::memory_compaction::{CompactionConfig, …}` is unchanged.
pub use sovereign_contracts::memory_config::{
    CompactionConfig, CompactionMode, DEFAULT_SYNTHESIS_PROMPT,
};

/// Summary of one compaction pass — what the worker did. Returned by
/// [`CompactionWorker::run_one_sync`] so callers (CLI, tests) can
/// assert on the shape; the async path drops this and emits a
/// `tracing::info!` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionPass {
    pub conversation_id: String,
    pub source_memory_ids: Vec<String>,
    pub summary_id: Option<String>,
    pub summary_chars: usize,
}

/// Background worker that holds a `mpsc` queue of conversation_ids
/// and runs compaction as items arrive. Construct once at daemon
/// startup via [`Self::spawn`]; hand the returned `Arc` to the
/// runtime, which calls [`Self::maybe_enqueue`] from the memory-save
/// hook.
///
/// Single-consumer by design — concurrent compactions on different
/// conversations serialise. At inner-work cadence (≤ 1 compaction
/// per ~5 turns × seconds-of-synthesis) this is more than enough
/// throughput. Future surfaces that need parallel compaction can
/// either spawn additional workers or upgrade the consumer to a
/// `JoinSet`-backed multi-consumer.
pub struct CompactionWorker {
    tx: mpsc::UnboundedSender<String>,
    memory_store: Arc<dyn MemoryStore>,
    inference: Arc<dyn InferenceProvider>,
    config: CompactionConfig,
}

impl CompactionWorker {
    /// Spawn the background draining task and return an `Arc<Self>`
    /// the runtime can clone for its save-memory hook. When
    /// `config.mode == Disabled` the worker is still constructed
    /// (so the runtime can carry a non-optional handle) but the
    /// drain loop short-circuits — `maybe_enqueue` is a no-op.
    pub fn spawn(
        memory_store: Arc<dyn MemoryStore>,
        inference: Arc<dyn InferenceProvider>,
        config: CompactionConfig,
    ) -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let worker = Arc::new(Self {
            tx,
            memory_store: Arc::clone(&memory_store),
            inference: Arc::clone(&inference),
            config: config.clone(),
        });
        if matches!(config.mode, CompactionMode::Disabled) {
            tracing::info!("memory_compaction: disabled (mode = disabled)");
            return worker;
        }
        let drain_store = memory_store;
        let drain_inference = inference;
        let drain_config = config;
        tokio::spawn(async move {
            tracing::info!(
                threshold = drain_config.threshold,
                batch = drain_config.batch,
                "memory_compaction: worker started"
            );
            while let Some(conv_id) = rx.recv().await {
                match run_pass(
                    &conv_id,
                    drain_store.as_ref(),
                    drain_inference.as_ref(),
                    &drain_config,
                )
                .await
                {
                    Ok(Some(pass)) => tracing::info!(
                        conversation_id = %conv_id,
                        summary_id = ?pass.summary_id,
                        folded = pass.source_memory_ids.len(),
                        summary_chars = pass.summary_chars,
                        "memory_compaction: pass complete"
                    ),
                    Ok(None) => tracing::debug!(
                        conversation_id = %conv_id,
                        "memory_compaction: under threshold, no work"
                    ),
                    Err(e) => tracing::warn!(
                        conversation_id = %conv_id,
                        error = %e,
                        "memory_compaction: pass failed (will retry on next enqueue)"
                    ),
                }
            }
            tracing::info!("memory_compaction: worker shutting down (channel closed)");
        });
        worker
    }

    /// Notify the worker that `conversation_id` may have crossed the
    /// compaction threshold. Cheap — the worker re-checks the count
    /// before doing real work, so over-enqueuing is harmless.
    /// `mode = Disabled` makes this a no-op.
    pub fn maybe_enqueue(&self, conversation_id: &str) {
        if matches!(self.config.mode, CompactionMode::Disabled) {
            return;
        }
        if let Err(e) = self.tx.send(conversation_id.to_string()) {
            tracing::warn!(
                conversation_id = %conversation_id,
                error = %e,
                "memory_compaction: enqueue failed — worker channel closed"
            );
        }
    }

    /// Synchronous compaction pass. Used by `sovereign memory
    /// rebuild-summaries` and the integration tests where the caller
    /// needs to observe the result. Same code path as the async
    /// drain.
    ///
    /// Returns `Ok(None)` when the conversation is under the
    /// threshold (no work to do). Returns `Ok(Some(pass))` with the
    /// summary id + folded source ids on a successful pass. Errors
    /// only when the synthesis call or a store write fails.
    pub async fn run_one_sync(&self, conversation_id: &str) -> Result<Option<CompactionPass>> {
        run_pass(
            conversation_id,
            self.memory_store.as_ref(),
            self.inference.as_ref(),
            &self.config,
        )
        .await
    }

    pub fn config(&self) -> &CompactionConfig {
        &self.config
    }
}

/// Single compaction pass. The async drain and `run_one_sync` both
/// route through here so behaviour stays uniform.
async fn run_pass(
    conversation_id: &str,
    memory_store: &dyn MemoryStore,
    inference: &dyn InferenceProvider,
    config: &CompactionConfig,
) -> Result<Option<CompactionPass>> {
    if config.batch < 2 {
        // Folding one memory into a "summary of 1" wastes a
        // synthesis call. Treat as a misconfiguration but don't
        // panic — soft-fail with a no-op + warn so operators see it.
        tracing::warn!(
            batch = config.batch,
            "memory_compaction: batch < 2 — refusing to fold a single memory"
        );
        return Ok(None);
    }
    let memories = memory_store
        .list_memories_for_conversation(conversation_id)
        .await?;
    if memories.len() < config.threshold {
        return Ok(None);
    }
    // Oldest `batch` collapse. `list_memories_for_conversation`
    // returns ordered ascending by `created_at`, so the slice is
    // already the right ones.
    let take = config.batch.min(memories.len());
    let to_fold: Vec<Memory> = memories.into_iter().take(take).collect();

    let entries = to_fold
        .iter()
        .map(|m| {
            let dated = chrono::DateTime::<chrono::Utc>::from_timestamp(m.created_at, 0)
                .map(|dt| format!("[{}] ", dt.format("%Y-%m-%d")))
                .unwrap_or_default();
            format!("- {dated}{}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = config.synthesis_prompt.replace("{entries}", &entries);

    // SLOT_POLICY §3 Housekeep: memory-fold synthesis (advisory).
    let mut request = CompletionRequest::for_workload(Workload::Housekeep, prompt)
        .with_output_budget(config.max_summary_chars.div_ceil(2) as u32);
    request.temperature = Some(0.3);
    // Distilled Fast model narrates its plan without this; hard-off the
    // reasoning scaffold so the fold text isn't a preamble.
    request.enable_thinking = Some(false);
    let response = inference.complete(&request).await?;
    let mut summary_text = response.text.trim().to_string();
    if summary_text.is_empty() {
        // Synthesis returned nothing — soft-fail, don't write an
        // empty summary that would shadow the originals on retrieval.
        tracing::warn!(
            conversation_id,
            "memory_compaction: synthesis returned empty content — skipping pass"
        );
        return Ok(None);
    }
    if summary_text.chars().count() > config.max_summary_chars {
        // Truncate to character cap. Keep prefix; append ellipsis so
        // a reader can spot the cut. Char-aware to avoid splitting
        // mid-grapheme.
        summary_text = summary_text
            .chars()
            .take(config.max_summary_chars.saturating_sub(1))
            .collect::<String>();
        summary_text.push('…');
    }

    let summary_id = uuid::Uuid::new_v4().to_string();
    let source_ids: Vec<String> = to_fold.iter().map(|m| m.id.clone()).collect();
    // Pick fields from the oldest source so retrieval treats the
    // summary as occupying the oldest's position in time. Inherit
    // skill scope unconditionally — the privacy wall demands it
    // (a summary built from inner-work memories MUST be inner-work-
    // scoped; mixing scopes would leak across the surface wall).
    let oldest = &to_fold[0];
    let confidence = to_fold.iter().map(|m| m.confidence).sum::<f64>() / (to_fold.len() as f64);
    let mut summary = Memory {
        id: summary_id.clone(),
        content: summary_text,
        source: oldest.source.clone(),
        confidence,
        created_at: oldest.created_at,
        last_used: oldest.created_at,
        version: 0,
        deleted_at: None,
        source_conversation_id: Some(conversation_id.to_string()),
        source_skill_id: oldest.source_skill_id.clone(),
        kind: MemoryKind::Summary,
        source_memory_ids: source_ids.clone(),
        superseded_by: None,
        embedding: None,
        embedding_model: None,
    };
    // T1 compute-on-write — the worker already holds the inference
    // handle it synthesized with; recall lazy-backfills on failure.
    crate::memory::attach_content_embedding(inference, &mut summary).await;
    let summary_chars = summary.content.chars().count();
    memory_store.save_memory(&summary).await?;
    for src_id in &source_ids {
        memory_store.mark_superseded(src_id, &summary_id).await?;
    }
    Ok(Some(CompactionPass {
        conversation_id: conversation_id.to_string(),
        source_memory_ids: source_ids,
        summary_id: Some(summary_id),
        summary_chars,
    }))
}
