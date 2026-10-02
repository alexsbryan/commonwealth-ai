// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-tiered retrieval surfaces: PPR entity-graph
//! rerank of conv-corpus chunks + the tiered briefing block.

use super::super::*;

impl Runtime {
    /// Render the conversation-tiered briefing block for the retrieval
    /// hit set. Returns an empty string when no reader is wired or no
    /// conv-category chunks made the cut — preserves the pre-tiered
    /// prompt layout exactly in those cases.
    ///
    /// Two-part output:
    ///   1. Per-source briefing (conv / vault / watched_folder) — the
    ///      pre-existing tiered surface.
    ///   2. Vault-wide synthesis briefing — cross-note themes from
    ///      `vault_themes`, present only when vault-category chunks are
    ///      in the hit set AND themes intersect the hit notes.
    ///
    /// Stitched in the canonical order: per-source first (more
    /// concrete, more retrievable-back) then synthesis (broader
    /// pattern context for the synth model to ground generalisations).
    pub(crate) async fn build_conv_briefing_block(
        &self,
        chunks: &[corpus_index::types::ScoredChunk],
        display_categories: &std::collections::HashMap<String, String>,
        lane: &crate::runtime::Lane,
    ) -> String {
        let Some(reader) = lane.conv_tiered.as_ref() else {
            return String::new();
        };
        let cats_opt = if display_categories.is_empty() {
            None
        } else {
            Some(display_categories)
        };
        let payload =
            crate::conv_briefing::build_conv_tiered_briefings(reader, chunks, cats_opt).await;
        if !payload.rendered.is_empty() {
            tracing::debug!(
                mode = payload.mode.label(),
                convs = payload.conv_count,
                "conv_briefing: surfaced tiered context"
            );
        }
        let vault_block =
            crate::conv_briefing::build_vault_synthesis_briefings(reader, chunks, cats_opt).await;
        if !vault_block.is_empty() {
            tracing::debug!(
                bytes = vault_block.len(),
                "conv_briefing: surfaced vault synthesis themes"
            );
        }
        if payload.rendered.is_empty() {
            vault_block
        } else if vault_block.is_empty() {
            payload.rendered
        } else {
            format!("{}\n{}", payload.rendered, vault_block)
        }
    }
}
