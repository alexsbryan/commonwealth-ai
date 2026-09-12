// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn-provenance record — answer of `GET /v1/conversations/{id}/provenance`.
//!
//! Lived in `sovereign_core::runtime::types` until 2026-09-11 (sv-surface
//! svt-3), where the desktop could only name it by linking the runtime. Plain
//! serde, no runtime dependency; core re-exports at the historical path so
//! the handlers that FILL it are unchanged.

/// Glassbox snapshot of what the witness path actually sent to the
/// model on a given turn. Captured at dispatch time inside
/// [`super::Runtime::handle_expressive_query_stream`] and stashed in
/// [`super::Runtime::turn_provenance`] so the desktop's inner-work surface
/// can pull it back via Cmd+? without instrumenting the live stream.
///
/// The shape is meant to be readable by a human investigating a bad
/// witness response: full assembled system prompt, the recalled
/// memories the witness drew on, the conversation history slice
/// actually passed to the inference call (today: empty — the
/// streaming witness path sends only the current user message), the
/// model id + token budget, and Pass A timing. When a response feels
/// untethered, the provenance answers "did the model see what we
/// thought it saw?" without anyone having to re-run the turn.
///
/// History note: the streaming path's `prompt: message` field puts
/// only the latest user message in front of the model; there is no
/// list of prior turns. `history_summary.sent_to_model` is therefore
/// empty in current capture sites — that emptiness is itself a
/// diagnostic. When history-injection is wired (a likely outcome of
/// the very investigations this struct exists to enable) the field
/// populates without a schema change.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TurnProvenance {
    pub conversation_id: String,
    pub message_id: String,
    /// Epoch seconds. Matches the `i64` shape the rest of the runtime
    /// uses (see `fn now()`); the desktop side reads it as a JS number.
    pub captured_at: i64,
    pub register: String,
    pub user_message: String,
    pub system_prompt: String,
    pub system_prompt_chars: usize,
    pub recalled_memories: Vec<RecalledMemoryProv>,
    pub history_summary: HistorySummaryProv,
    /// Earlier turns of THIS conversation that retrieval-over-history
    /// spliced into the prompt. Empty when the conversation is still
    /// short enough that every turn is in the visible window, when no
    /// candidate cleared the similarity floor, or on provenance frames
    /// persisted before this field (2026-07-26).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_recall: Vec<HistoryRecallProv>,
    pub temporal_tensions: Vec<String>,
    pub contradiction: Option<ContradictionProv>,
    pub current_goal: Option<String>,
    pub recent_topic: Option<String>,
    pub last_assistant_excerpt: Option<String>,
    pub model_id: Option<String>,
    pub max_tokens: Option<usize>,
    pub enable_thinking: Option<bool>,
    pub pass_a_ms: Option<u64>,
    /// Outcome of the witness recall-grounding verifier for this turn
    /// (`runtime/memory_grounding.rs`). Previously computed and
    /// discarded after pinning; retained so the epistemic ledger can
    /// distinguish a verified recall from a fail-open one. `None` when
    /// the verifier didn't run (non-witness turns, older frames).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_verification: Option<RecallVerificationProv>,
}

/// Persisted outcome of the witness recall-grounding verifier.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecallVerificationProv {
    /// Whether the final reply's past-claims were confirmed contained
    /// in retrieved entries.
    pub grounded: bool,
    /// True when the verifier errored/declined and the reply shipped
    /// unchecked (the deliberate availability posture, made visible).
    pub fail_open: bool,
    /// 1-based index of the recalled entry the reply spoke about, when
    /// the verifier attributed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced: Option<usize>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecalledMemoryProv {
    pub id: String,
    pub content: String,
    pub created_at: i64,
    /// `"raw"` for an extraction; `"summary"` for a row written by
    /// the compaction worker. Optional in the JSON shape for
    /// backward-compat with provenance frames persisted before the
    /// compaction-fields wiring (2026-05-23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// For summaries: the ids of the source `Raw` memories this row
    /// folded. Empty (or absent) on raw memories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_memory_ids: Vec<String>,
    /// Stored confidence at recall time — the input the epistemic
    /// band derivation (`memory::band_for_confidence`) reads, retained
    /// so the ledger and the prompt agree on the band. Absent on
    /// provenance frames persisted before this field (2026-07-18).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HistorySummaryProv {
    /// Total messages on the conversation when the turn was dispatched.
    pub total_messages: usize,
    pub user_count: usize,
    pub assistant_count: usize,
    /// The slice that was actually passed to the inference call. The
    /// streaming witness path sends only the current user message
    /// today, so this is empty even when `total_messages` is large.
    pub sent_to_model: Vec<HistoryEntryProv>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntryProv {
    pub role: String,
    pub content_preview: String,
    pub full_chars: usize,
}

/// One earlier turn-pair that retrieval-over-history pulled back into
/// this turn's prompt (`Runtime::maybe_retrieve_relevant_history`).
///
/// The ledger's answer to "did it actually remember, or did it guess?"
/// — the recall channel is otherwise invisible after the turn ends
/// (`ConversationContext::history_retrieval_hits` is `#[serde(skip)]`
/// and recomputed every turn), so without this the provenance frame
/// showed a system prompt with an unexplained "Relevant earlier turns"
/// block in it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HistoryRecallProv {
    /// Index into the conversation's message list of the pair's lead
    /// message — the same `turn_index` the narration chip carries.
    pub turn_index: usize,
    /// Hybrid similarity (cosine, plus entity Jaccard when GLiNER is
    /// available) against the current user message.
    pub similarity: f32,
    /// Leading excerpt of the recalled pair, capped for the ledger.
    pub excerpt: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContradictionProv {
    pub prior_evidence: String,
    pub current_claim: String,
}
