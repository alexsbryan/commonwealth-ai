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

/// The router's turn classification — selects the dispatch path. The
/// referential variants are re-cut by `Operation` × `Effort` (see
/// `QUERY_TAXONOMY_MECE.md`); the speech-act variants carry their own handlers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Intent {
    /// Quick, self-contained ask — answered directly on the fast conversational path.
    SimpleQuery,
    /// Open-ended reasoning or essay-shaped ask — deep synthesis on the primary slot.
    DeepQuery,
    /// A question the installed corpora should answer: retrieval + grounded, cited synthesis.
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
    /// User requesting creative/generative output ("tell me a story", "write a
    /// poem", "compose a letter", "brainstorm names"). No corpus retrieval, no
    /// grounding gate, no tools, no situated/relational framing — the handler
    /// streams the requested piece behind a neutral creative system prompt
    /// (`handlers/generative.rs`). Short-circuited off the DeepQuery path by the
    /// router's `looks_like_creative_generation` heuristic, because routing a
    /// creative ask through retrieval+synthesis buffers every token behind the
    /// gate (a long blank screen, then a dump grounded in irrelevant corpora).
    GenerativeQuery,
    /// A question about how THIS codebase works — "how does inference run",
    /// "what calls gate_answer", "where is X implemented", "trace the request
    /// flow". A first-class referential route over CODE corpora: retrieval
    /// rides the intent-summary bridge (plain-English → symbol) and the answer
    /// is grounded in the SCIP call-graph trace, scoped to code corpora so the
    /// 30+ non-code corpora can't dilute it. Distinct from `MetalingualQuery`
    /// (vocabulary lookup — "what does X *mean* here") and from
    /// `KnowledgeQuery`/`DeepQuery` (which neither scope to code nor surface the
    /// call graph as primary evidence). Inert when no code corpus is installed:
    /// the handler detects that and falls back to the knowledge path, so a
    /// non-code deployment behaves exactly as before.
    CodeQuery,
    /// One direct tool invocation, no plan.
    SimpleAction {
        /// The tool to invoke.
        tool: ToolId,
    },
    /// Multi-step goal: plan first, then execute as a `Task`.
    ComplexTask,
    /// Follow-up that resumes an existing task.
    Continuation {
        /// The task being resumed.
        task_id: TaskId,
    },
}

/// Referential cognitive **operation** — *what an answer does*. The MECE
/// re-cut of the conflated `Simple`/`Knowledge`/`Deep`/`Comparison` intents
/// (see `sovereign/docs/QUERY_TAXONOMY_MECE.md`). Orthogonal to *effort*
/// (which model tier serves it) — that is a separate axis. Defined for the
/// referential-knowledge path ONLY; the Jakobson/speech-act intents
/// (`Metalingual`/`Conation`/`Commissive`/`Expressive`) and the action
/// intents keep their own handlers and have no `Operation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operation {
    /// Compose an answer from the corpus. Collapses `Simple` + `Knowledge` +
    /// `Deep` — one operation at different *effort*, not three operations.
    Answer,
    /// Bounded contrast of ≥2 named entities along shared axes (distinct
    /// answer *structure*, not just higher effort).
    Compare,
    /// A list / roster (distinct answer *structure*; today the gated
    /// atom-enum path).
    Enumerate,
}

/// The **effort** an answer demands — orthogonal to [`Operation`]. Picks the
/// model tier: `Low` → fast slot, `High` → primary slot. Derived from a
/// dedicated effort classifier (centroid over high/low-effort exemplars), not
/// from the intent label. See `sovereign/docs/QUERY_TAXONOMY_MECE.md`: an
/// "exhaustive, section-by-section account" and a "who-is-X" lookup are the
/// same `Answer` operation at opposite ends of this axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Effort {
    /// Single fact / short answer — the fast slot suffices.
    Low,
    /// Exhaustive / multi-section / deep-synthesis answer — needs the primary slot.
    High,
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

/// Everything the router/planner/executor needs to know about a tool without
/// running it: identity, parameter schema, examples, and behavioural properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Registry id — the `ToolRegistry` key.
    pub id: ToolId,
    /// Human-readable name shown in prompts and UI.
    pub name: String,
    /// What the tool does, phrased for the model choosing among tools.
    pub description: String,
    /// JSON schema of accepted arguments, injected into planner prompts.
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
    /// Whether a duplicate call duplicates the effect — drives the retry gate.
    pub idempotency: Idempotency,
    /// Expected cost class — drives plan parallelisation and timeouts.
    pub latency: Latency,
    /// Where the tool's effect lives (session / persistent / external).
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

/// Ambient state handed to every `Tool::execute` call: who is asking (conversation/task) and per-call flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContext {
    /// Conversation the call belongs to.
    pub conversation_id: ConversationId,
    /// Owning task when called from a plan step; `None` for direct invocations.
    pub task_id: Option<TaskId>,
    /// Working directory for filesystem-affecting tools, when one applies.
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

/// Capability grants the consent layer manages, declared per tool via
/// `Tool::required_permissions`. Coarse by design — one gate per
/// user-meaningful capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    /// Reach outside the machine over the network.
    Network,
    /// Read from the local filesystem.
    FileRead,
    /// Write to the local filesystem.
    FileWrite,
    /// Run shell commands.
    Shell,
    /// Read the user's email.
    EmailRead,
    /// Send or modify email.
    EmailWrite,
    /// Read calendar data.
    CalendarRead,
    /// Create or modify calendar entries.
    CalendarWrite,
    /// Author / publish recipes — distinct from generic FileWrite
    /// because the recipe-author tools are allowlisted to
    /// `~/.sovereign/recipes/` and benefit from a single approval
    /// gate covering the whole authoring loop. Carrying it as a
    /// separate variant lets the approval policy say "yes, this
    /// agent can iterate on recipes" without granting blanket
    /// filesystem write.
    RecipeAuthoring,
    /// Author / edit workflows — the umbrella authoring permission, distinct
    /// from `RecipeAuthoring` (which is the proprietary ingest/enrich stage).
    /// The workflow-author tools are allowlisted to `~/.sovereign/workflows/`,
    /// so a single gate covers the whole compose→validate→test loop without
    /// granting blanket filesystem write.
    WorkflowAuthoring,
    /// Download + index a corpus from a recipe (the `recipe:` workflow stage).
    /// One gate covers the heavy ingest (network fetch + large local compute +
    /// disk write) — more honest in a trigger-attach prompt than three generic
    /// permissions, and lets a policy grant "may build corpora" without blanket
    /// `Network`/`FileWrite`.
    CorpusIngest,
}

// ─── Trust ────────────────────────────────────────────────────

/// Provenance tier of a signed artifact (skill, recipe). Derived from the signature fields by `compute_trust_level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TrustLevel {
    /// Signed by the `sovereign-community` identity — reviewed and vouched.
    CommunityReviewed,
    /// Signed by an individual author identity.
    AuthorSigned,
    /// No signature. The default, and what unknown ids resolve to.
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
