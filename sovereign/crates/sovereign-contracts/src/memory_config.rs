// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory-compaction configuration types.
//!
//! Extracted from `sovereign-core`'s `memory_compaction` worker so that
//! `setup_config::SetupConfig` (a contract type) can embed the operator knobs
//! without dragging the worker (tokio, the `MemoryStore` runtime) into the
//! contract crate. The worker in `sovereign-core::memory_compaction` re-exports
//! these at their historical paths, so `sovereign_core::memory_compaction::
//! {CompactionConfig, CompactionMode, DEFAULT_SYNTHESIS_PROMPT}` is unchanged.

use serde::{Deserialize, Serialize};

/// How the worker decides when (and whether) to run compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CompactionMode {
    /// Save-time hook is a no-op. Existing memories never collapse.
    /// For operators who want the schema migration applied without
    /// the runtime behaviour change.
    Disabled,
    /// Save-time hook runs compaction inline before returning. The
    /// caller's turn pauses for the synthesis call. Used by
    /// `sovereign memory rebuild-summaries` and integration tests
    /// where deterministic ordering matters.
    Sync,
    /// Save-time hook fires-and-forgets to the worker thread. The
    /// caller's turn returns immediately; compaction lands by the
    /// next turn (worst case: one extra raw memory in the next
    /// prompt before the worker catches up). This is the default
    /// production shape.
    #[default]
    Async,
}

/// Operator-tunable knobs for the compaction worker. Persisted under
/// `[memory.compaction]` in the daemon's `config.toml`.
///
/// The defaults are inner-work-tuned (threshold=6, batch=3) — that's
/// where the bench at `sovereign/bench/inner_work/compaction.toml`
/// shows the prompt staying bounded under 8K tokens for a 12-turn
/// session without losing witness quality on the first five turns.
/// Other surfaces (factual chat, recipe author) can opt out via
/// `mode = "disabled"` until their own benches exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Non-superseded memory count per conversation that triggers a
    /// compaction pass.
    pub threshold: usize,
    /// Number of oldest memories collapsed into a single summary on
    /// each pass. Must be ≥ 2 (folding one memory into a "summary
    /// of 1" wastes a synthesis call).
    pub batch: usize,
    /// Trigger model. See [`CompactionMode`].
    pub mode: CompactionMode,
    /// Hard upper bound on the synthesized summary's `content`
    /// length. The synthesis prompt asks for a few sentences;
    /// truncation past the cap protects the prompt budget against a
    /// runaway response.
    pub max_summary_chars: usize,
    /// Synthesis prompt template. The worker substitutes
    /// `{entries}` with the bullet-listed source memories.
    /// Skill-specific overrides live in `skill.toml`
    /// `[memory_compaction] prompt = "..."`.
    pub synthesis_prompt: String,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            threshold: 6,
            batch: 3,
            mode: CompactionMode::Async,
            max_summary_chars: 800,
            synthesis_prompt: DEFAULT_SYNTHESIS_PROMPT.to_string(),
        }
    }
}

/// Inner-work-tuned default. Preserves emotional tone and recurring
/// concerns at the expense of one-off detail — the witness register
/// cares more about pattern than fact. Replace via
/// `[memory.compaction] synthesis_prompt = "..."` for other surfaces.
pub const DEFAULT_SYNTHESIS_PROMPT: &str =
    "Distill these inner-work entries into 2-3 sentences. Preserve \
     emotional tone, recurring concerns, and named people / projects. \
     Drop one-off detail and verbatim phrasing. Write in the writer's \
     register, not as a report. No headers, no bullets, just prose.\n\
     \n\
     Entries:\n\
     {entries}";
