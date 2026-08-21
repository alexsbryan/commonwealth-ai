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

impl Intent {
    /// The variant's name, and nothing else — the one rendering used wherever a
    /// route is RECORDED (`routed_intent` on turn metadata, chaos transcript
    /// rows).
    ///
    /// Deliberately not `{self:?}`: the payload-carrying variants would render
    /// as `SimpleAction { tool: ToolId("search") }` and `Continuation { task_id:
    /// … }`, so the same route would produce a different string every turn and
    /// could never be grouped or compared. A recorded route is a closed set of
    /// labels; the payload belongs to other fields.
    ///
    /// Distinct from [`crate::types::ui::ResponseProvenance::intent`], which is
    /// a free-form DISPLAY label for the desktop footer and carries hardcoded
    /// values on some routes. When you need to know how a turn was routed, this
    /// is the one to read.
    pub fn name(&self) -> &'static str {
        self.row().name
    }
}

// ─── The intent table ──────────────────────────────────────────

/// Which tools an intent may call. A closed set (ARCH §2.1): three shapes,
/// and adding a fourth is a variant here plus one render arm at the single
/// site that turns it into a `ToolFilter`.
///
/// Held as a column on [`IntentRow`] rather than as a `ToolFilter` because a
/// `ToolFilter` owns a `HashSet<String>` and a table row must be `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAccess {
    /// The full catalog, unfiltered. The intent's own handler decides.
    Full,
    /// Exactly these tool ids and no others. The empty slice is the
    /// load-bearing case: it means "no tools at all", structurally.
    Only(&'static [&'static str]),
    /// Only the tool named in the intent's own payload. The router already
    /// picked it; the filter's job is to stop the planner substituting
    /// another. Payload is not a per-variant attribute, so it cannot be a
    /// literal in the table — this variant says so out loud instead of
    /// leaving a column that lies.
    PayloadTool,
}

/// Every per-intent attribute, as one row.
///
/// **Adding an intent is a variant plus a row here plus exemplars in
/// `sovereign/router/exemplars.toml`.** Nothing else in the workspace has to
/// change, and nothing else may re-derive one of these columns — the whole
/// point is that a missing attribute is a COMPILE ERROR (this struct has no
/// `Default`, so a row that omits a field does not build) rather than a
/// fallback arm that silently hands the new intent someone else's policy.
///
/// WHY A TABLE AND NOT THIRTEEN `match`ES. Before this existed, these columns
/// lived in ten separate matches across four crates, three of them ending in a
/// `_ =>` catch-all — so a fourteenth intent compiled clean and silently
/// inherited `Speed::Slow`, a 700-token budget, and no `Operation`. The
/// campaign's `nc-extends` bar counts exactly that shape; see
/// `quality/campaigns/noun-convergence.toml`.
///
/// This is NOT a general licence to fold `match`es on `Intent` into the table.
/// Matching on a closed enum is what enums are for: handler DISPATCH
/// (`runtime/turn.rs`) and payload guards stay where they are. What belongs
/// here is a per-variant ATTRIBUTE — a value the runtime looks up, not a code
/// path it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntentRow {
    /// The variant's name, PascalCase. The one rendering used wherever a route
    /// is RECORDED (`routed_intent` on turn metadata, chaos transcript rows) —
    /// stable enough to group and compare across runs.
    pub name: &'static str,

    /// The wire key, snake_case. One vocabulary shared by the exemplar TOML
    /// (`sovereign/router/exemplars.toml`), eval banks' `expected_intent`,
    /// routing reports, and the desktop redirect payload. Payload-carrying
    /// variants suffix it with their payload (`simple_action:web_search`);
    /// the row holds the base.
    pub slug: &'static str,

    /// Banner phrase for `interpretation-proposed`, rendered as "I'm reading
    /// this as {interpretation}". Shown before the first token, so it must be
    /// derivable without a model call.
    pub interpretation: &'static str,

    /// Redirect-chip text on that banner. `{tool}` is substituted with the
    /// payload for [`Intent::SimpleAction`]; every other row is literal.
    pub redirect_label: &'static str,

    /// Clarification phrase for the `Ask` move, rendered as "my best read is
    /// {read_as}". Deliberately a separate column from `interpretation`: the
    /// banner narrates a decision already taken, the clarifier offers one not
    /// yet taken, and they have never been the same words.
    pub read_as: &'static str,

    /// The runtime handler this intent dispatches to. A TRACE LABEL, not a
    /// dispatch mechanism — `runtime::turn` still matches to pick the call,
    /// because a handler is code and code does not go in a table. Keeping the
    /// label here means the glassbox line and the actual dispatch cannot drift
    /// apart without this row changing.
    pub dispatch: &'static str,

    /// OICP defaults: `(capability hint, latency class)`, or `None` when the
    /// intent wants no envelope at all — the local Fast slot serves it without
    /// invoking the scheduler, because cross-network latency is not worth a
    /// marginal quality bump on a small ask.
    ///
    /// The hint is one of [`oicp::CapabilityHint::STANDARDIZED`], held as
    /// `&'static str` because `CapabilityHint` is not const-constructible;
    /// `intent_table_hints_are_standardized` is what keeps that honest.
    pub oicp: Option<(&'static str, oicp::LatencyClass)>,

    /// Slot to retrieve on when the corpus DID return evidence.
    pub speed_with_evidence: Speed,

    /// Slot to retrieve on when it did NOT. Two columns rather than one plus a
    /// flag: only `SimpleQuery` differs between them (knowledge found for a
    /// simple question upgrades to the primary model), and a per-row pair says
    /// so without the reader having to find the rule.
    pub speed_without_evidence: Speed,

    /// Depth floor for the synthesis output budget, in tokens — how thorough
    /// an answer the ask implies, before evidence breadth widens it. `usize`
    /// to match `OutputBudget::soft_target`, which it is added into.
    pub output_floor: usize,

    /// The referential [`Operation`] this intent performs, or `None` for the
    /// Jakobson/speech-act and action intents, which have no referential
    /// operation at all.
    pub operation: Option<Operation>,

    /// The operation when retrieval also pinned an atom-enum set — a
    /// set/roster question. Promotes `Answer` to `Enumerate` on the
    /// referential rows and changes nothing anywhere else.
    pub operation_with_atom_enum: Option<Operation>,

    /// Which tools the model may call on a turn classified as this intent.
    /// The catalog filter applied at dispatch time, so an intent that should
    /// not reach for tools cannot — structurally, not by prompt.
    pub tools: ToolAccess,
}

impl Intent {
    /// This intent's row. The single place any per-intent attribute is decided.
    pub const fn row(&self) -> IntentRow {
        use oicp::CapabilityHint as Cap;
        use oicp::LatencyClass as Lat;
        match self {
            Self::SimpleQuery => IntentRow {
                name: "SimpleQuery",
                slug: "simple_query",
                interpretation: "a quick factual answer",
                redirect_label: "Give me a quick answer",
                read_as: "a quick factual answer",
                dispatch: "handle_simple",
                oicp: None,
                // Knowledge found for a simple question upgrades to the
                // primary model; without evidence the fast slot answers from
                // general knowledge.
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Fast,
                output_floor: 400,
                operation: Some(Operation::Answer),
                operation_with_atom_enum: Some(Operation::Enumerate),
                // Chit-chat / smalltalk — the model answers from pretrained
                // knowledge; no tool needed.
                tools: ToolAccess::Only(&[]),
            },
            Self::DeepQuery => IntentRow {
                name: "DeepQuery",
                slug: "deep_query",
                interpretation: "a deeper explanation",
                redirect_label: "Walk me through it in depth",
                read_as: "a deeper explanation",
                dispatch: "handle_simple",
                // Reasoning-heavy: extended class tolerates higher TTFT in
                // exchange for deeper thinking budgets.
                oicp: Some((Cap::GENERAL, Lat::Extended)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 1200,
                operation: Some(Operation::Answer),
                operation_with_atom_enum: Some(Operation::Enumerate),
                // Synthesis-oriented: the unified front door plus the legacy
                // SearchTool (kept for web supplementation) and the
                // epistemic-graph tools. `document` is included because
                // research and analysis often pulls user documents in.
                tools: ToolAccess::Only(&[
                    "knowledge_lookup",
                    "search",
                    "knowledge",
                    "claim_search",
                    "epistemic_landscape",
                    "document",
                    "wikipedia_fetch",
                    "web_fetch",
                ]),
            },
            Self::KnowledgeQuery => IntentRow {
                name: "KnowledgeQuery",
                slug: "knowledge_query",
                interpretation: "a look in your installed knowledge",
                redirect_label: "Check my knowledge base",
                read_as: "a corpus lookup",
                dispatch: "handle_knowledge_query",
                // Retrieval-driven synthesis over a bounded chunk set.
                oicp: Some((Cap::GENERAL, Lat::Normal)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: Some(Operation::Answer),
                operation_with_atom_enum: Some(Operation::Enumerate),
                // Same synthesis family as DeepQuery — see its row.
                tools: ToolAccess::Only(&[
                    "knowledge_lookup",
                    "search",
                    "knowledge",
                    "claim_search",
                    "epistemic_landscape",
                    "document",
                    "wikipedia_fetch",
                    "web_fetch",
                ]),
            },
            Self::ComparisonQuery => IntentRow {
                name: "ComparisonQuery",
                slug: "comparison_query",
                interpretation: "a comparison between two things",
                redirect_label: "Compare them side by side",
                read_as: "a side-by-side comparison",
                dispatch: "handle_knowledge_query",
                // Bounded two-entity contrast — Fast slot, no reasoning
                // budget. The constrained synthesis prompt does the
                // structuring work the primary model would otherwise do.
                oicp: Some((Cap::GENERAL, Lat::Fast)),
                speed_with_evidence: Speed::Fast,
                speed_without_evidence: Speed::Fast,
                output_floor: 700,
                // Comparison is its own operation and outranks the atom-enum
                // pin, so both columns read `Compare`.
                operation: Some(Operation::Compare),
                operation_with_atom_enum: Some(Operation::Compare),
                // Same synthesis family as DeepQuery — see its row.
                tools: ToolAccess::Only(&[
                    "knowledge_lookup",
                    "search",
                    "knowledge",
                    "claim_search",
                    "epistemic_landscape",
                    "document",
                    "wikipedia_fetch",
                    "web_fetch",
                ]),
            },
            Self::MetalingualQuery => IntentRow {
                name: "MetalingualQuery",
                slug: "metalingual_query",
                interpretation: "a lookup in your codebase",
                redirect_label: "Look it up in this codebase",
                read_as: "a vocabulary lookup in our system",
                dispatch: "handle_metalingual_query",
                // Codebase lookup + brief synthesis against code corpora.
                // Fast slot is enough; no reasoning budget.
                oicp: Some((Cap::CODE, Lat::Fast)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // Project-internal vocabulary questions. Code-intelligence
                // tools plus the unified front door, for cross-cutting asks
                // that live in notes / project docs.
                tools: ToolAccess::Only(&[
                    "knowledge_lookup",
                    "symbol_lookup",
                    "code_search",
                    "recent_changes",
                    "find_callers",
                    "find_callees",
                    "blast_radius",
                ]),
            },
            Self::ConationQuery => IntentRow {
                name: "ConationQuery",
                slug: "conation_query",
                interpretation: "a tweak to my last reply",
                redirect_label: "Adjust the last reply",
                read_as: "an adjustment to my last reply",
                dispatch: "handle_conation_query",
                // Operates on the prior turn — no new retrieval, no
                // reclassification. The rebound classification's envelope is
                // what actually matters; this covers the rare case where
                // conation is dispatched without rebind context.
                oicp: Some((Cap::GENERAL, Lat::Fast)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // Emotive / commitment / imperative / creative — these should
                // not reach for tools at all. An empty allowlist makes that
                // structural rather than a prompt instruction.
                tools: ToolAccess::Only(&[]),
            },
            Self::CommissiveQuery => IntentRow {
                name: "CommissiveQuery",
                slug: "commissive_query",
                interpretation: "a commitment to save",
                redirect_label: "Save this as a commitment",
                read_as: "a commitment to save",
                dispatch: "handle_commissive_query",
                // Persistence-only path — no LLM synthesis required for the
                // storage step; a brief Fast-slot acknowledgment citing the
                // situated anchor is all we need.
                oicp: Some((Cap::GENERAL, Lat::Fast)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // See ConationQuery — no tools, structurally.
                tools: ToolAccess::Only(&[]),
            },
            Self::ExpressiveQuery => IntentRow {
                name: "ExpressiveQuery",
                slug: "expressive_query",
                interpretation: "an acknowledgment + help offer",
                redirect_label: "Hear me out and help",
                read_as: "an acknowledgment + targeted help",
                dispatch: "handle_expressive_query",
                // Acknowledge + situated help-offer. Fast slot synthesis
                // grounded in working_memory + last assistant turn; no
                // retrieval against the world corpus.
                oicp: Some((Cap::GENERAL, Lat::Fast)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // See ConationQuery — no tools, structurally.
                tools: ToolAccess::Only(&[]),
            },
            Self::GenerativeQuery => IntentRow {
                name: "GenerativeQuery",
                slug: "generative_query",
                interpretation: "something creative written for you",
                redirect_label: "Write something creative",
                read_as: "something creative written for you",
                dispatch: "handle_generative_query",
                // Creative generation — primary (Slow) slot for quality; no
                // retrieval, no thinking budget. Extended signals the
                // scheduler to favour the capable slot for a long piece.
                oicp: Some((Cap::GENERAL, Lat::Extended)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // See ConationQuery — no tools, structurally.
                tools: ToolAccess::Only(&[]),
            },
            Self::CodeQuery => IntentRow {
                name: "CodeQuery",
                slug: "code_query",
                interpretation: "a look in the indexed code",
                redirect_label: "Search the codebase",
                read_as: "a question about the code",
                dispatch: "handle_code_query",
                // First-class code route: retrieval over code-intel summaries
                // plus the SCIP call-graph trace, then synthesis. Code-capable
                // hint, normal latency — KnowledgeQuery's shape, code-scoped.
                oicp: Some((Cap::CODE, Lat::Normal)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 900,
                operation: Some(Operation::Answer),
                operation_with_atom_enum: Some(Operation::Enumerate),
                // First-class "how does this code work" questions. Same
                // code-intelligence set as MetalingualQuery.
                tools: ToolAccess::Only(&[
                    "knowledge_lookup",
                    "symbol_lookup",
                    "code_search",
                    "recent_changes",
                    "find_callers",
                    "find_callees",
                    "blast_radius",
                ]),
            },
            Self::SimpleAction { .. } => IntentRow {
                name: "SimpleAction",
                slug: "simple_action",
                interpretation: "a tool call",
                // The only row whose label carries payload — two
                // `SimpleAction`s naming different tools want different chips.
                redirect_label: "Use the {tool} tool",
                read_as: "an action",
                dispatch: "handle_simple",
                oicp: None,
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // The router already picked the tool; the filter enforces that
                // the planner does not substitute another.
                tools: ToolAccess::PayloadTool,
            },
            Self::ComplexTask => IntentRow {
                name: "ComplexTask",
                slug: "complex_task",
                interpretation: "a multi-step task",
                redirect_label: "Plan a multi-step task",
                read_as: "a multi-step task",
                dispatch: "handle_complex_task",
                // Tool-using plans want solid normal-latency responses;
                // extended would add round-trip overhead per tool step.
                oicp: Some((Cap::GENERAL, Lat::Normal)),
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // Multi-step planning. Full read catalog plus write tools — the
                // executor's approval gates govern write safety, not the
                // catalog filter.
                tools: ToolAccess::Only(&[
                    "knowledge_lookup",
                    "search",
                    "knowledge",
                    "claim_search",
                    "epistemic_landscape",
                    "document",
                    "document_operation",
                    "wikipedia_fetch",
                    "web_fetch",
                    "symbol_lookup",
                    "code_search",
                    "recent_changes",
                    "find_callers",
                    "find_callees",
                    "blast_radius",
                    "shell",
                    "file",
                    "file_write",
                    "note",
                    "run_tests",
                ]),
            },
            Self::Continuation { .. } => IntentRow {
                name: "Continuation",
                slug: "continuation",
                interpretation: "a follow-up to earlier work",
                redirect_label: "Continue prior task",
                read_as: "a continuation",
                dispatch: "handle_simple",
                oicp: None,
                speed_with_evidence: Speed::Slow,
                speed_without_evidence: Speed::Slow,
                output_floor: 700,
                operation: None,
                operation_with_atom_enum: None,
                // A continuation resumes a prior task; its policy comes from
                // that task's plan, not from this dispatch. Unrestricted here
                // lets the continuation handler decide.
                tools: ToolAccess::Full,
            },
        }
    }
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

/// A deterministic authority claim (FINANCIAL_CORPORA.md §7.3): a tool
/// asserting, from its own enumerable domain, that it is the AUTHORITATIVE
/// answer surface for a question. Produced by [`crate::traits::Tool::claims`]
/// and consulted by the router BEFORE any similarity-based intent
/// classification — the question the gate asks stops being "is this more
/// tool-like than knowledge-like" (a contest a typed store can never win
/// against knowledge exemplars) and becomes "does this store claim
/// authority here". No embeddings, no threshold; the failure direction is
/// good — an over-claiming tool produces an honest refusal naming what IS
/// available, never a wrong number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityClaim {
    /// Registry id of the claiming tool.
    pub tool_id: String,
    /// The corpus whose recipe declared this tool authoritative
    /// (`[authority]` block, registry data — never a user setting).
    pub corpus_id: String,
    /// The matched evidence, for glassbox logs: which entity term and
    /// which domain term fired (e.g. "entity 'apple' + concept term
    /// 'revenue'").
    pub matched: String,
}

/// Ambient state handed to every `Tool::execute` call: who is asking (conversation/task) and per-call flags.
///
/// `Default` exists so a caller names only the fields it actually has
/// (`ToolContext { conversation_id: id, ..Default::default() }`) — adding a
/// turn fact here must not be a workspace-wide edit every time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// The user's question for this turn, verbatim, when the executor
    /// knows it (plan steps carry the task goal; direct//in-process
    /// invocations leave it `None`).
    ///
    /// Exists so a tool that claims AUTHORITY over a question can
    /// enforce, in code, that what it answers is what was asked —
    /// `Tool::claims` already receives the question at routing time, so
    /// this is the same fact at execute time and keeps ONE decider for
    /// question-derived constraints at both ends (ARCH §10.6).
    /// Motivating failure (FINANCIAL_CORPORA §7.6, reproduced
    /// 2026-08-16): asked for CALENDAR 2025, the planner called
    /// `sec_facts` with `period: "FY2025"` — while its own next step
    /// explained that Apple's fiscal year is not calendar 2025. A model
    /// instruction is not a guarantee; only code is.
    ///
    /// Never a permission or trust input — it is the asker's own text.
    #[serde(default)]
    pub question: Option<String>,
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
    /// `~/.svrnmesh/recipes/` and benefit from a single approval
    /// gate covering the whole authoring loop. Carrying it as a
    /// separate variant lets the approval policy say "yes, this
    /// agent can iterate on recipes" without granting blanket
    /// filesystem write.
    RecipeAuthoring,
    /// Author / edit workflows — the umbrella authoring permission, distinct
    /// from `RecipeAuthoring` (which is the proprietary ingest/enrich stage).
    /// The workflow-author tools are allowlisted to `~/.svrnmesh/workflows/`,
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

#[cfg(test)]
mod intent_name_tests {
    use super::*;

    /// Every variant, so adding one without naming it is a compile error in
    /// `Intent::name` and a visible gap here.
    fn every_variant() -> Vec<Intent> {
        vec![
            Intent::SimpleQuery,
            Intent::DeepQuery,
            Intent::KnowledgeQuery,
            Intent::ComparisonQuery,
            Intent::MetalingualQuery,
            Intent::ConationQuery,
            Intent::CommissiveQuery,
            Intent::ExpressiveQuery,
            Intent::GenerativeQuery,
            Intent::CodeQuery,
            Intent::SimpleAction {
                tool: "search".to_string(),
            },
            Intent::ComplexTask,
            Intent::Continuation {
                task_id: "task-1".to_string(),
            },
        ]
    }

    #[test]
    fn every_route_has_its_own_name() {
        let names: Vec<&str> = every_variant().iter().map(Intent::name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "two routes share a name, so they cannot be told apart in telemetry: {names:?}"
        );
        assert!(names.iter().all(|n| !n.is_empty()));
    }

    #[test]
    fn a_recorded_route_is_a_label_not_a_payload() {
        // The reason this exists instead of `format!("{intent:?}")`: a payload
        // in the label makes every turn a distinct string, so routes can never
        // be grouped or counted.
        let action = Intent::SimpleAction {
            tool: "search".to_string(),
        };
        let continuation = Intent::Continuation {
            task_id: "task-1".to_string(),
        };
        assert_eq!(action.name(), "SimpleAction");
        assert_eq!(continuation.name(), "Continuation");
        for intent in [&action, &continuation] {
            let name = intent.name();
            assert!(
                !name.contains(['{', '(', '"']),
                "payload leaked into the recorded route: {name}"
            );
        }
    }

    // ─── The intent table ──────────────────────────────────────
    //
    // These pin the table against the ten `match`es it replaced on 2026-08-20
    // (noun-convergence rung nc-14). The transcription is the risk: a table
    // built from scattered arms is only worth having if it says exactly what
    // they said, so `the_table_says_what_the_matches_said` carries every
    // column of every row as a literal, lifted from the pre-change source.

    #[test]
    fn every_row_is_distinct_in_the_ways_that_identify_it() {
        let rows: Vec<IntentRow> = every_variant().iter().map(Intent::row).collect();
        for (field, mut vals) in [
            ("name", rows.iter().map(|r| r.name).collect::<Vec<_>>()),
            ("slug", rows.iter().map(|r| r.slug).collect::<Vec<_>>()),
        ] {
            let n = vals.len();
            vals.sort_unstable();
            vals.dedup();
            assert_eq!(
                vals.len(),
                n,
                "two rows share a `{field}`, so they cannot be told apart on the wire"
            );
        }
        assert!(rows
            .iter()
            .all(|r| !r.slug.is_empty() && !r.name.is_empty()));
    }

    #[test]
    fn intent_table_hints_are_standardized() {
        // `default_oicp_for_intent` expects on this. The hint is held as
        // `&'static str` because `CapabilityHint` is not const-constructible,
        // so this test is the only thing standing between a typo in a row and
        // a panic in the OICP envelope path.
        for intent in every_variant() {
            let Some((hint, _)) = intent.row().oicp else {
                continue;
            };
            assert!(
                oicp::CapabilityHint::STANDARDIZED.contains(&hint),
                "{}'s hint {hint:?} is not standardized",
                intent.name()
            );
            assert!(
                oicp::CapabilityHint::parse(hint).is_ok(),
                "{}'s hint {hint:?} does not parse",
                intent.name()
            );
        }
    }

    #[test]
    fn payload_columns_are_marked_and_only_on_payload_variants() {
        // Two columns cannot be pure literals because their value depends on
        // the variant's PAYLOAD, not on the variant. Both say so structurally
        // rather than holding a value that lies: `ToolAccess::PayloadTool`,
        // and a `{tool}` placeholder in `redirect_label`. Neither may appear
        // on a payload-free row.
        for intent in every_variant() {
            let row = intent.row();
            let carries_payload = matches!(intent, Intent::SimpleAction { .. });
            assert_eq!(
                row.tools == ToolAccess::PayloadTool,
                carries_payload,
                "{}: ToolAccess::PayloadTool belongs to SimpleAction alone",
                intent.name()
            );
            assert_eq!(
                row.redirect_label.contains("{tool}"),
                carries_payload,
                "{}: a `{{tool}}` placeholder belongs to SimpleAction alone",
                intent.name()
            );
        }
    }

    #[test]
    fn dispatch_column_names_a_runtime_handler() {
        // The column is the glassbox label `runtime::turn` logs before it
        // dispatches. Pinned as a closed set so a typo — `handle_knowlege_query`
        // — fails here rather than shipping a trace line that names a function
        // nobody can find.
        const HANDLERS: &[&str] = &[
            "handle_simple",
            "handle_knowledge_query",
            "handle_code_query",
            "handle_metalingual_query",
            "handle_conation_query",
            "handle_commissive_query",
            "handle_expressive_query",
            "handle_generative_query",
            "handle_complex_task",
        ];
        for intent in every_variant() {
            let d = intent.row().dispatch;
            assert!(
                HANDLERS.contains(&d),
                "{} dispatches to {d:?}, which is not a runtime handler",
                intent.name()
            );
        }
    }

    #[test]
    fn the_table_says_what_the_matches_said() {
        use oicp::CapabilityHint as Cap;
        use oicp::LatencyClass as Lat;
        // (intent, slug, dispatch, oicp, speed_with_evidence,
        //  speed_without_evidence, output_floor, operation, operation_with_atom_enum)
        //
        // Lifted from `intent_helpers::{default_oicp_for_intent, intent_hint}`,
        // `runtime::turn`'s dispatch label, and `runtime::evidence`'s
        // `speed_for_retrieval_intent` / `resolve_output_budget` /
        // `operation_of` as they stood at d8156d06. Three of those carried a
        // `_ =>` catch-all — `Speed::Slow`, `700`, `None` — so the rows that
        // were never named explicitly are the ones most worth pinning.
        #[allow(clippy::type_complexity)]
        let want: &[(
            Intent,
            &str,
            &str,
            Option<(&str, Lat)>,
            Speed,
            Speed,
            usize,
            Option<Operation>,
            Option<Operation>,
        )] = &[
            (
                Intent::SimpleQuery,
                "simple_query",
                "handle_simple",
                None,
                Speed::Slow,
                Speed::Fast,
                400,
                Some(Operation::Answer),
                Some(Operation::Enumerate),
            ),
            (
                Intent::DeepQuery,
                "deep_query",
                "handle_simple",
                Some((Cap::GENERAL, Lat::Extended)),
                Speed::Slow,
                Speed::Slow,
                1200,
                Some(Operation::Answer),
                Some(Operation::Enumerate),
            ),
            (
                Intent::KnowledgeQuery,
                "knowledge_query",
                "handle_knowledge_query",
                Some((Cap::GENERAL, Lat::Normal)),
                Speed::Slow,
                Speed::Slow,
                700,
                Some(Operation::Answer),
                Some(Operation::Enumerate),
            ),
            (
                Intent::ComparisonQuery,
                "comparison_query",
                "handle_knowledge_query",
                Some((Cap::GENERAL, Lat::Fast)),
                Speed::Fast,
                Speed::Fast,
                700,
                Some(Operation::Compare),
                Some(Operation::Compare),
            ),
            (
                Intent::MetalingualQuery,
                "metalingual_query",
                "handle_metalingual_query",
                Some((Cap::CODE, Lat::Fast)),
                Speed::Slow,
                Speed::Slow,
                700,
                None,
                None,
            ),
            (
                Intent::ConationQuery,
                "conation_query",
                "handle_conation_query",
                Some((Cap::GENERAL, Lat::Fast)),
                Speed::Slow,
                Speed::Slow,
                700,
                None,
                None,
            ),
            (
                Intent::CommissiveQuery,
                "commissive_query",
                "handle_commissive_query",
                Some((Cap::GENERAL, Lat::Fast)),
                Speed::Slow,
                Speed::Slow,
                700,
                None,
                None,
            ),
            (
                Intent::ExpressiveQuery,
                "expressive_query",
                "handle_expressive_query",
                Some((Cap::GENERAL, Lat::Fast)),
                Speed::Slow,
                Speed::Slow,
                700,
                None,
                None,
            ),
            (
                Intent::GenerativeQuery,
                "generative_query",
                "handle_generative_query",
                Some((Cap::GENERAL, Lat::Extended)),
                Speed::Slow,
                Speed::Slow,
                700,
                None,
                None,
            ),
            (
                Intent::CodeQuery,
                "code_query",
                "handle_code_query",
                Some((Cap::CODE, Lat::Normal)),
                Speed::Slow,
                Speed::Slow,
                900,
                Some(Operation::Answer),
                Some(Operation::Enumerate),
            ),
            (
                Intent::ComplexTask,
                "complex_task",
                "handle_complex_task",
                Some((Cap::GENERAL, Lat::Normal)),
                Speed::Slow,
                Speed::Slow,
                700,
                None,
                None,
            ),
        ];
        for (intent, slug, dispatch, oicp_, sp_ev, sp_no, floor, op, op_enum) in want {
            let r = intent.row();
            let who = intent.name();
            assert_eq!(r.slug, *slug, "{who}.slug");
            assert_eq!(r.dispatch, *dispatch, "{who}.dispatch");
            assert_eq!(r.oicp, *oicp_, "{who}.oicp");
            assert_eq!(r.speed_with_evidence, *sp_ev, "{who}.speed_with_evidence");
            assert_eq!(
                r.speed_without_evidence, *sp_no,
                "{who}.speed_without_evidence"
            );
            assert_eq!(r.output_floor, *floor, "{who}.output_floor");
            assert_eq!(r.operation, *op, "{who}.operation");
            assert_eq!(
                r.operation_with_atom_enum, *op_enum,
                "{who}.operation_with_atom_enum"
            );
        }
        // The two payload-carrying variants, whose rows the loop above cannot
        // name without inventing a payload.
        for (intent, slug) in [
            (
                Intent::SimpleAction {
                    tool: "search".to_string(),
                },
                "simple_action",
            ),
            (
                Intent::Continuation {
                    task_id: "task-1".to_string(),
                },
                "continuation",
            ),
        ] {
            let r = intent.row();
            assert_eq!(r.slug, slug);
            assert_eq!(r.dispatch, "handle_simple");
            assert_eq!(r.oicp, None, "payload variants took no OICP envelope");
            assert_eq!(r.speed_with_evidence, Speed::Slow);
            assert_eq!(r.speed_without_evidence, Speed::Slow);
            assert_eq!(r.output_floor, 700);
            assert_eq!(r.operation, None);
            assert_eq!(r.operation_with_atom_enum, None);
        }
    }

    #[test]
    fn tool_access_says_what_tool_filter_for_intent_said() {
        // Lifted from `intent_policy::tool_filter_for_intent` at d8156d06.
        // Only the SHAPE and the ids are pinned; the ordering is the table's.
        let synth = &[
            "knowledge_lookup",
            "search",
            "knowledge",
            "claim_search",
            "epistemic_landscape",
            "document",
            "wikipedia_fetch",
            "web_fetch",
        ];
        let code = &[
            "knowledge_lookup",
            "symbol_lookup",
            "code_search",
            "recent_changes",
            "find_callers",
            "find_callees",
            "blast_radius",
        ];
        for (intent, want) in [
            (Intent::KnowledgeQuery, ToolAccess::Only(synth)),
            (Intent::ComparisonQuery, ToolAccess::Only(synth)),
            (Intent::DeepQuery, ToolAccess::Only(synth)),
            (Intent::MetalingualQuery, ToolAccess::Only(code)),
            (Intent::CodeQuery, ToolAccess::Only(code)),
            // The four that must not reach for tools at all, and the one that
            // answers from pretrained knowledge. An EMPTY allowlist, not an
            // absent one — "no tools" is structural here, not a prompt.
            (Intent::ExpressiveQuery, ToolAccess::Only(&[])),
            (Intent::ConationQuery, ToolAccess::Only(&[])),
            (Intent::CommissiveQuery, ToolAccess::Only(&[])),
            (Intent::GenerativeQuery, ToolAccess::Only(&[])),
            (Intent::SimpleQuery, ToolAccess::Only(&[])),
            (
                Intent::Continuation {
                    task_id: "t".to_string(),
                },
                ToolAccess::Full,
            ),
            (
                Intent::SimpleAction {
                    tool: "search".to_string(),
                },
                ToolAccess::PayloadTool,
            ),
        ] {
            assert_eq!(intent.row().tools, want, "{}", intent.name());
        }
        // ComplexTask gets the read catalog plus the write tools; pinned by
        // membership rather than by a second copy of a twenty-id list.
        let ToolAccess::Only(plan) = Intent::ComplexTask.row().tools else {
            panic!("ComplexTask must carry an explicit allowlist");
        };
        for id in synth.iter().chain(code.iter()) {
            assert!(plan.contains(id), "ComplexTask lost the read tool {id}");
        }
        for id in [
            "document_operation",
            "shell",
            "file",
            "file_write",
            "note",
            "run_tests",
        ] {
            assert!(plan.contains(&id), "ComplexTask lost the write tool {id}");
        }
    }

    #[test]
    fn the_blind_surface_is_nameable() {
        // `ComplexTask` is the surface H4 had to identify by reading answer
        // prose. Naming it is the whole point of the field.
        assert_eq!(Intent::ComplexTask.name(), "ComplexTask");
    }
}
