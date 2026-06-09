// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from the monolithic types.rs (ARCH §3.2); re-exported by types/mod.rs,
//! so every sovereign_core::types::* import path is unchanged (behaviour-preserving).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use crate::oicp;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

// ─── Routing Types ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Intent {
    SimpleQuery,
    DeepQuery,
    KnowledgeQuery,
    /// Two or more named things contrasted along shared axes. Bounded
    /// shape (a small set of contrast points), so it's served by the
    /// fast slot with a constrained synthesis prompt rather than the
    /// open-ended `DeepQuery` essay path. Retrieval should anchor on
    /// every named entity, not just the first.
    ComparisonQuery,
    /// Question about the *shared vocabulary of this system* — "what
    /// does X mean here / in this codebase / in this project / in our
    /// system / earlier in this conversation". Jakobson's metalingual
    /// function: foregrounding the *code* (the words themselves), not
    /// the world the words might point at.
    ///
    /// Routes to internal vocabulary sources — code corpora, notes,
    /// conversation history, project docs — NOT the general knowledge
    /// corpus. The Gricean signal that distinguishes metalingual from
    /// referential is the in-system locator: "what does sharding mean"
    /// is referential (KnowledgeQuery), "what does sharding mean here"
    /// is metalingual (this variant). Without this carve-out, the
    /// metalingual case hits the world corpus and confabulates a
    /// generic answer that misses the project-specific meaning.
    MetalingualQuery,
    /// Imperative command directed at the assistant referencing the
    /// prior turn ("stop", "try again", "shorter please", "skip the
    /// boilerplate", "more detail"). Operates on the prior turn as a
    /// situated artifact: the handler does NOT reclassify or re-extract
    /// — it rebinds the prior `QuerySession.classification` and
    /// transforms the response (cancel / regenerate / re-synthesize
    /// with a style directive). The user already said what they wanted
    /// last turn; conation just adjusts how it's expressed.
    ConationQuery,
    /// User committing to action ("I'll fix it tomorrow", "I'm going
    /// to refactor X", "remind me to check Friday"). Searle's
    /// commissive act. The handler persists the commitment to the
    /// notes store anchored to the situated `working_memory.current_goal`
    /// (or honestly anchorless when no goal is loaded), so the system's
    /// memory of decisions accumulates rather than evaporating into
    /// polite acknowledgments.
    CommissiveQuery,
    /// User expressing how they're feeling about the current work
    /// ("I'm stuck on this bug", "ugh, broken again", "I have no idea
    /// where to start"). Searle's expressive act. The handler grounds
    /// its response in situated context (`working_memory.current_goal`,
    /// last assistant turn, open commitments on this work) so "I'm
    /// stuck" produces a help-offer anchored to the actual current
    /// work, not a generic pep talk. When no situated context is
    /// loaded, the handler asks plainly what the user is working on
    /// — epistemic honesty as the natural path.
    ExpressiveQuery,
    SimpleAction {
        tool: ToolId,
    },
    ComplexTask,
    Continuation {
        task_id: TaskId,
    },
}

// ─── Tool Types ────────────────────────────────────────────────

/// Read/write classification. Gates approval routing and — via
/// Phase 1.5 — MCP-path approval parity. A `Read` tool never
/// mutates state; a `Write` tool produces a persistent or
/// session-scoped effect; `ReadWrite` tools (shell, compute) can
/// do either depending on inputs.
///
/// Deliberately has **no `Default`** — every tool must classify
/// itself explicitly so a future author cannot silently misclassify
/// via boilerplate copy-paste. See ARCH_PRINCIPLES.md §7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    /// No mutation. Safe to call speculatively, safe to retry.
    Read,
    /// Mutates persistent or session state. Retry only if also
    /// `Idempotent`; approval-gate when permissions are empty.
    Write,
    /// Both — e.g. `shell` reading then writing, `file` whose
    /// read/write split is dynamic. Treated as `Write` for retry /
    /// approval purposes.
    ReadWrite,
}

/// Whether calling the tool twice with identical arguments produces
/// the same effect. Drives the executor's retry loop: a
/// `NonIdempotent` tool never auto-retries on transient failure
/// regardless of `retry_config()`, because retry would duplicate
/// the original side-effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Idempotency {
    /// Second call with same args is a no-op (reads, idempotent
    /// upserts, `delete` against an already-deleted row).
    Idempotent,
    /// Second call creates a duplicate or second side-effect
    /// (`write_note`, `email`, `calendar`).
    NonIdempotent,
}

/// Expected latency class. Used by the planner to build realistic
/// DAGs (parallelise Fast reads; serialise Slow writes) and by
/// future timeout policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Latency {
    /// Milliseconds — in-memory, SQLite point lookup.
    Instant,
    /// Sub-second — FTS query, small embedding, simple shell exec.
    Fast,
    /// Seconds to minutes — LLM chain, test run, web fetch,
    /// document map-reduce.
    Slow,
    /// Long-running observation with incremental output.
    Streaming,
}

/// Where the tool's effect lives. Informs "can this tool leak
/// across sessions / machines?" and — via KnowledgeView precedent
/// — privacy-related decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Effect confined to the current conversation (working memory,
    /// transient shell state).
    Session,
    /// Effect persists across sessions in a local store (NoteStore,
    /// FeatureStore, corpus indexes).
    Persistent,
    /// Effect reaches outside this machine (email, web fetch, MCP
    /// bridge).
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub id: ToolId,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Concrete examples of correct tool invocations. Small models copy
    /// examples more reliably than they follow descriptions. Injected
    /// into planner prompts so the model sees what correct calls look like.
    #[serde(default)]
    pub examples: Vec<ToolExample>,
    /// Behavioural properties (Phase 1). Consulted by the executor's
    /// retry gate, approval gate, and the planner's prompt
    /// annotations so the agent can reason about a tool mechanically
    /// rather than by parsing prose.
    pub effect: Effect,
    pub idempotency: Idempotency,
    pub latency: Latency,
    pub scope: Scope,
    /// Shape of the tool's output (composition piece).
    ///
    /// When `Some`, the value is a JSON-schema-ish description of the
    /// keys a downstream step can reference via `{N.key}` templates
    /// in `resolve_inputs`. The planner surfaces this to the model so
    /// it can write compositional plans (e.g. "call `symbol_lookup`
    /// then pipe `{0.file}` into `find_callers`") deliberately rather
    /// than by guessing.
    ///
    /// When `None`, downstream steps either accept the full text via
    /// `{N.output}` or must route through a reasoning step. Common
    /// for truly opaque outputs (`shell`, `compute`) where the
    /// structure depends entirely on what command or script ran.
    ///
    /// Schema convention: loose JSON Schema — `type: object` plus a
    /// `properties` map is standard, but since we don't enforce the
    /// full spec, tools can also use informal "shape hints" that
    /// communicate intent to the LLM more clearly than strict JSON
    /// Schema.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
}

/// A concrete example of a correct tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExample {
    /// When to use this tool (e.g. "User asks about a research topic")
    pub situation: String,
    /// The exact JSON arguments for this invocation
    pub call: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContext {
    pub conversation_id: ConversationId,
    pub task_id: Option<TaskId>,
    pub working_directory: Option<String>,
    /// True when this tool is being called inside a ReasonWithTools loop.
    /// Tools may format results differently for reasoning vs. synthesis.
    #[serde(default)]
    pub in_reasoning_loop: bool,
    /// Identifier for the calling agent's session, used by the work
    /// atlas to group successive tool calls into a single
    /// coordination session. Populated by `mcp_router` from the
    /// `X-Agent-Session` HTTP header; falls back to a synthetic
    /// `conn:<mcp_session>` per-connection token when no header is
    /// present, and is `None` for in-process callers (CLI, tests,
    /// runtime-internal tool execution) that don't go through the
    /// MCP transport. `#[serde(default)]` so older serialized
    /// contexts decode cleanly.
    #[serde(default)]
    pub agent_session_token: Option<String>,
    /// Zero-based count of prior user turns in this conversation
    /// (Tier 1 result memory). Tools that return citation-shaped
    /// evidence call `EvidenceId::from_index_with_turn(idx,
    /// turn_index)` so the resulting handles are unique across
    /// the conversation's history. `#[serde(default)]` means
    /// pre-Tier-1 serialized contexts decode as turn 0 — degraded
    /// but valid (handles render as `ev-T0-NNNN`).
    #[serde(default)]
    pub turn_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    Network,
    FileRead,
    FileWrite,
    Shell,
    EmailRead,
    EmailWrite,
    CalendarRead,
    CalendarWrite,
    /// Author / publish recipes — distinct from generic FileWrite
    /// because the recipe-author tools are allowlisted to
    /// `~/.sovereign/recipes/` and benefit from a single approval
    /// gate covering the whole authoring loop. Carrying it as a
    /// separate variant lets the approval policy say "yes, this
    /// agent can iterate on recipes" without granting blanket
    /// filesystem write.
    RecipeAuthoring,
}

// ─── Trust ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TrustLevel {
    CommunityReviewed,
    AuthorSigned,
    #[default]
    Unsigned,
}

/// Compute trust level from signature fields.
pub fn compute_trust_level(signature: &Option<String>, signed_by: &Option<String>) -> TrustLevel {
    match (signature, signed_by) {
        (Some(_), Some(s)) if s == "sovereign-community" => TrustLevel::CommunityReviewed,
        (Some(_), _) => TrustLevel::AuthorSigned,
        _ => TrustLevel::Unsigned,
    }
}
