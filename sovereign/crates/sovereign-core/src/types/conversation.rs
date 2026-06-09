// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from the monolithic types.rs (ARCH §3.2); re-exported by types/mod.rs,
//! so every sovereign_core::types::* import path is unchanged (behaviour-preserving).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use crate::oicp;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

// ─── Conversation Types ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub role: Role,
    pub content: String,
    pub created_at: i64,
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Message {
    pub fn role_str(&self) -> &'static str {
        match self.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub title: Option<String>,
    pub messages: Vec<Message>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub deleted_at: Option<i64>,
    /// Skill active when this conversation started, if any.
    /// Used by the `conversation-history` KnowledgeView acquirer to
    /// filter conversations tagged with `privacy = "local_only"` skills
    /// (e.g. `inner-work`) out of the conversational knowledge corpus.
    /// `None` for conversations predating the KnowledgeView migration.
    #[serde(default)]
    pub skill_id: Option<String>,
    /// User-controlled allow-list of corpus IDs the conversation may
    /// retrieve from. `None` means "all installed corpora" — the
    /// default for fresh conversations and the implicit state for any
    /// row predating this column. `Some(vec)` is an explicit subset:
    /// only those corpus_ids (plus their layer/satellite children)
    /// participate in retrieval and appear in the model's
    /// `installed_corpora_display()` prompt.
    ///
    /// Stored as a JSON-encoded `Vec<String>` in the conversations
    /// table; the column is `NULL` for rows predating the
    /// `run_corpus_filter_migration` ALTER. Updated via
    /// `ConversationStore::set_conversation_enabled_corpora`.
    #[serde(default)]
    pub enabled_corpora: Option<Vec<String>>,
    /// Cumulative list of web sources surfaced to the user via
    /// `submit_information_search` across this conversation's turns.
    /// Each `submit_information_search` call dedupes new URLs against
    /// the existing set, bumping `last_referenced_turn` on duplicates
    /// and appending new entries with `first_seen_turn = current_turn`.
    ///
    /// Rendered in the synthesis system prompt as a "Web sources
    /// gathered so far" block so the model has cumulative awareness
    /// of which URLs the user has already been shown — preventing
    /// duplicate citations and the "the search you did three turns
    /// ago" coreference miss.
    ///
    /// `None` for conversations predating the M3 migration; an empty
    /// Vec is structurally distinct from `None` (the conversation ran
    /// no searches, but the migration has fired).
    ///
    /// Stored as JSON-encoded text in the conversations table.
    /// Updated via `ConversationStore::set_conversation_searched_sources`.
    #[serde(default)]
    pub searched_sources: Option<Vec<SearchedSourceEntry>>,
}

/// One entry in `Conversation.searched_sources` — a URL the user has
/// been shown via an `submit_information_search` call. Cumulative
/// across the conversation's turns; deduped by `url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchedSourceEntry {
    /// Canonical URL (the value the user could click). Used as the
    /// dedup key.
    pub url: String,
    /// Display title (search-result title). Empty string when the
    /// search backend doesn't surface one.
    #[serde(default)]
    pub title: String,
    /// 0-indexed turn during which this URL first entered the
    /// conversation's known set. Stable across re-references.
    pub first_seen_turn: usize,
    /// 0-indexed turn during which the URL was most recently
    /// referenced (e.g. via a subsequent search that returned the
    /// same URL). Equals `first_seen_turn` on the first sighting.
    pub last_referenced_turn: usize,
    /// The query string that initially surfaced this URL. Empty when
    /// the originating search didn't supply one.
    #[serde(default)]
    pub search_query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationContext {
    pub conversation: Conversation,
    pub memories: Vec<Memory>,
    pub working_memory: Option<WorkingMemory>,
    /// Corpus IDs of installed corpora at context-assembly time.
    /// Used by the router to inform classification and by prompts
    /// to tell the model what local knowledge is available.
    #[serde(default)]
    pub installed_corpora: Vec<String>,
    /// Active document session for this conversation (if any).
    /// When present, follow-up questions can reference the structured
    /// output without re-running the full map-reduce operation.
    #[serde(default)]
    pub document_session: Option<DocumentSession>,
    /// Topic context tracking across turns. Updated after each turn
    /// by a Fast-slot inference call. Used by the router to detect
    /// follow-ups vs. pivots and avoid misclassifying general knowledge
    /// questions as corpus queries.
    #[serde(default)]
    pub topic_context: Option<ConversationTopicContext>,
    /// KnowledgeView landscape digests spliced in by the Runtime
    /// **after** skill routing. `None` at `build_context()` time;
    /// populated by `KnowledgeViewManager::landscape_digest` for each
    /// active view before the prompt is assembled.
    ///
    /// A `None` value reaching the prompt-assembly site is a bug —
    /// either the Runtime forgot to splice, or a caller built a
    /// context without routing. The final-prompt path should
    /// `debug_assert!` this is `Some(_)` in debug builds to surface
    /// the oversight.
    #[serde(default)]
    pub knowledge_view_digests: Option<Vec<LandscapeDigest>>,
    /// Tensions between the current user message and prior
    /// high-confidence memories, detected by the Quick-slot
    /// pre-pass `memory::detect_temporal_tensions`. Spliced into
    /// the system prompt under "Notable tension across time:" by
    /// `Runtime::build_system_message` when the active skill
    /// register is `Relational`. Empty (or absent) when no
    /// tensions were found, the active skill is factual, or the
    /// pre-pass failed soft (it must never block a turn).
    #[serde(default)]
    pub temporal_tensions: Vec<TemporalTension>,
    /// Compacted summary of conversation turns that fell outside the
    /// rolling visible-history window. Populated by the runtime via
    /// a Fast-slot summarization call when `conversation.messages`
    /// exceeds `CONV_HISTORY_TURNS` (see `runtime.rs`). `None` when
    /// the conversation is still short enough that every turn fits
    /// in the visible window — no compaction needed.
    ///
    /// Consumed by `build_system_message` → `format_conversation_history`
    /// to prepend an "Earlier in the conversation:" block before the
    /// verbatim recent turns. Surfaced by
    /// `sovereign/bench/wikipedia_learn` 2026-05-17 marathon thread:
    /// turn 11's callback to "Babbage's original vision" (introduced
    /// in turn 0) fails when T0 has rolled off the visible window
    /// without a compacted anchor.
    #[serde(default)]
    pub compacted_history: Option<String>,
    /// Retrieval-over-history hits — top-K prior user/assistant message
    /// pairs (older than the visible window) selected by cosine
    /// similarity against the current user message. Mechanism replaces
    /// the lossy re-summarisation spiral that marathon_graceful v1/v2
    /// surfaced — instead of re-compressing the dropped tail every
    /// turn, retrieve the relevant 2-3 turns directly.
    ///
    /// Populated by `Runtime::maybe_retrieve_relevant_history` (gated
    /// on `SOVEREIGN_HISTORY_RETRIEVAL=1` for the spike phase) and
    /// consumed by `build_system_message` as a "Relevant earlier
    /// turns:" section in the prompt.
    ///
    /// `#[serde(skip)]` — embeddings are recomputed each turn from the
    /// visible message list; nothing persists.
    #[serde(skip)]
    pub history_retrieval_hits: Option<Vec<HistoryRetrievalHit>>,
    /// Tool-Mastery framework dossier: ambient context block listing
    /// the tools narrowed for the active skill on this turn, the
    /// recent tool-decision outcomes scoped to this conversation,
    /// and (placeholder) workspace freshness signals. Populated by
    /// `dossier::compute_tool_dossier` as a Fast-slot pre-pass and
    /// spliced into the system message by `build_system_message`.
    /// `None` on relational skills (inner-work) and when the active
    /// skill hasn't been resolved (CLI / test harness paths).
    #[serde(default)]
    pub tool_dossier: Option<ToolDossier>,
    /// Per-turn IntentPolicy computed at dispatch time from
    /// (intent, register, active_mode). Carries the effective
    /// register and the post-override effective intent so every
    /// downstream consumer reads from a single source of truth
    /// rather than re-querying `SkillRegistry::primary_skill_register()`
    /// independently at ~16 sites.
    ///
    /// `#[serde(skip)]` because the policy is rebuilt from
    /// in-memory state at every dispatch; never persisted, never
    /// restored. Legacy callers that construct a context without
    /// going through dispatch see `None` and fall back to factual
    /// defaults via [`Self::turn_register`].
    #[serde(skip)]
    pub intent_policy: Option<crate::intent_policy::IntentPolicy>,
}

impl ConversationContext {
    /// Return the per-turn voice register, falling back to
    /// `Factual` when no policy has been computed yet (test
    /// harnesses, headless boot, or any code path that built a
    /// context outside `handle_message_stream` / `handle_turn`).
    /// Replaces scattered `SkillRegistry::primary_skill_register()`
    /// queries throughout `runtime.rs`.
    pub fn turn_register(&self) -> crate::skills::SkillRegister {
        self.intent_policy
            .as_ref()
            .map(|p| p.register)
            .unwrap_or(crate::skills::SkillRegister::Factual)
    }

    /// Return the policy's `effective_intent` if available. Useful
    /// at dispatch-time when the caller has just bound the policy
    /// and wants the post-override intent for the handler call.
    pub fn turn_effective_intent(&self) -> Option<&crate::types::Intent> {
        self.intent_policy
            .as_ref()
            .and_then(|p| p.effective_intent.as_ref())
    }
}

/// Tool-Mastery dossier. Three sections per the Phase 3 plan:
/// 1. Tools available this turn (from the narrowed catalog).
/// 2. Outcome history this conversation (from `tool_decision` notes).
/// 3. Ambient workspace state (lint/test freshness; placeholder for now).
///
/// Stored on `ConversationContext` so multiple call sites
/// (`build_system_message` + the routing-footer renderer) can
/// consume the same computed value without re-running the
/// NoteStore read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDossier {
    /// Resolved id of the active skill at the time the dossier was
    /// computed — drives the per-skill "narrowed by" label in the
    /// routing footer. `None` when no skill was active.
    pub active_skill_id: Option<String>,
    /// One entry per tool the model can call this turn. Carries the
    /// canonical id + descriptor.description (no new asset — the
    /// descriptors are the source of truth per ARCH §6.2).
    pub tools_available: Vec<ToolDossierEntry>,
    /// Recent tool-decision outcomes (`useful` / `stale` /
    /// `wrong-tool` / `no-results`) scoped to this conversation.
    /// Capped at `MAX_DOSSIER_OUTCOMES` (see `dossier.rs`).
    pub outcome_history: Vec<ToolDossierOutcome>,
    /// Ambient-workspace freshness signals. Phase-3 plan punt — left
    /// as `None`; future PRs splice `lint_status` / `test_status`
    /// here without touching this struct.
    #[serde(default)]
    pub ambient_state: Option<String>,
}

/// One row of `ToolDossier.tools_available`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDossierEntry {
    pub tool_id: String,
    pub description: String,
}

/// One row of `ToolDossier.outcome_history` — a frozen view of a
/// past `ToolDecisionPayload` keyed to this conversation. Separate
/// from the payload type so the splice format is stable even if the
/// stored payload schema grows fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDossierOutcome {
    pub tool_id: String,
    /// Canonical wire-form: `"useful"` / `"stale"` / `"wrong-tool"`
    /// / `"no-results"`. String here (not the enum) so this type
    /// stays Serde-friendly without pulling the
    /// `ToolDecisionOutcome` enum into the public types module.
    pub outcome: String,
    pub reasoning: String,
    pub applied_at_unix: i64,
    /// Tier 1 result memory — one-line summary of what the tool
    /// actually returned (top-1 evidence title for knowledge_lookup,
    /// first matched symbol for code-intel, etc.). `None` for
    /// pre-Tier-1 payloads or sites that don't have the data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Tier 1 result memory — per-call ev-Tn-NNNN handles the
    /// model may cite cross-turn. Empty when the underlying tool
    /// doesn't return citation-shaped evidence (or when the call
    /// pre-dates Tier 1). The renderer surfaces these as
    /// `[ev-T2-0000..0003]` ranges so the model can address past
    /// evidence without re-fetching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    /// Tier 1 result memory — zero-based turn index this outcome
    /// was recorded against. Lets the renderer disambiguate when
    /// two outcomes are from the same tool: `T2` vs `T4` ids.
    #[serde(default)]
    pub turn_index: usize,
}

/// One retrieved earlier-turn pair (user question + assistant reply)
/// selected by cosine similarity against the current user message.
/// Produced by `Runtime::maybe_retrieve_relevant_history`; consumed
/// by `build_system_message` for the "Relevant earlier turns" prompt
/// section. Spike phase (2026-05-26) — gated on
/// `SOVEREIGN_HISTORY_RETRIEVAL=1` and not persisted (rebuilt per turn).
#[derive(Debug, Clone)]
pub struct HistoryRetrievalHit {
    /// Index of the user message in `Conversation.messages` (the
    /// pair's lead message). Useful for traceability in glassbox.
    pub turn_index: usize,
    /// Concatenated user+assistant body, truncated to ~600 chars per
    /// side so the section stays bounded.
    pub content: String,
    /// Cosine similarity of `content` embedding to the current
    /// user-message embedding. Surfaced for debug logging only.
    pub similarity: f32,
}

/// A pairwise tension between a prior memory the user expressed
/// and the user's current message. Produced by
/// `memory::detect_temporal_tensions`; consumed by the
/// prompt-assembly layer to surface principle 5 of the relational
/// voice contract ("you told me X in March; this sounds different
/// — did something shift?"). The model decides whether to
/// actually surface it; the system only ensures the cue is in
/// front of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalTension {
    /// Id of the prior `Memory` that's in tension. Lets the
    /// renderer reproduce the exact stored phrasing rather than
    /// paraphrasing.
    pub memory_id: String,
    /// The prior memory's content as the user originally
    /// expressed it.
    pub prior_content: String,
    /// `created_at` of the prior memory, propagated so the
    /// renderer can show "you told me on YYYY-MM-DD..." for
    /// memories with `source_conversation_id` set.
    pub prior_created_at: i64,
    /// Whether the prior memory carried a source-conversation id
    /// — controls whether the date prefix renders.
    pub prior_has_source_conversation: bool,
    /// The user's current message excerpt (bounded so the prompt
    /// doesn't bloat for very long messages).
    pub current_excerpt: String,
}

/// One view's contribution to the assembled context. Produced by
/// `KnowledgeViewManager::landscape_digest`; consumed by the
/// prompt-assembly layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LandscapeDigest {
    /// View id (e.g. `"personal-knowledge"`, `"conversation-history"`).
    pub view_id: String,
    /// Markdown-formatted digest body. Bounded by the token budget
    /// the Runtime passed to `landscape_digest`.
    pub body: String,
}

impl ConversationContext {
    /// Comma-separated display string for the installed corpora.
    pub fn installed_corpora_display(&self) -> String {
        if self.installed_corpora.is_empty() {
            "none installed".to_string()
        } else {
            self.installed_corpora.join(", ")
        }
    }

    /// Replace the `knowledge_view_digests` field. Used by the
    /// Runtime to splice in digests produced after skill routing.
    pub fn set_landscape_digests(&mut self, digests: Vec<LandscapeDigest>) {
        self.knowledge_view_digests = Some(digests);
    }

    /// Debug-build guard: assert the landscape-digest field has
    /// been spliced. Call this right before handing the context
    /// to the LLM prompt-assembly layer so that a missed splice
    /// fails loudly in tests rather than silently leaking an
    /// unfiltered digest into a user-facing prompt.
    ///
    /// In release builds this is a no-op — the Runtime is
    /// structured so all production paths splice, and we don't
    /// want to panic end-users on an edge case that integration
    /// tests would have caught.
    #[inline]
    pub fn debug_assert_routed(&self) {
        debug_assert!(
            self.knowledge_view_digests.is_some(),
            "ConversationContext reached the prompt-assembly site with \
             knowledge_view_digests=None. The Runtime must call \
             KnowledgeViewManager::splice_into between build_context() \
             and the final prompt. See sovereign_core::types::ConversationContext \
             field docs for the invariant."
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMemory {
    pub current_goal: Option<String>,
    pub facts: Vec<String>,
    pub active_documents: Vec<String>,
}

/// Lightweight topic context derived from the conversation arc.
/// Updated after each turn by a Fast-slot inference call.
/// Used by the router to avoid misclassifying follow-up questions
/// (e.g. a general knowledge question in a document conversation).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConversationTopicContext {
    /// The dominant topic being discussed (e.g. "Schrödinger's What is Life?").
    pub topic: Option<String>,
    /// The primary intellectual domain (e.g. "philosophy", "buddhism", "biology").
    pub domain: Option<String>,
    /// If the conversation is anchored to a specific document or corpus.
    pub anchored_source: Option<String>,
    /// Number of consecutive turns on this topic. Resets on pivot.
    pub turn_depth: u32,
}
