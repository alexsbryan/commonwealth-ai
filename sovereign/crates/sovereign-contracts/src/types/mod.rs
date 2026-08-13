// SPDX-License-Identifier: AGPL-3.0-or-later
//! Core contract vocabulary: identity aliases, plan/task/step types, memory and
//! document rows, and the façade re-exports of the extracted submodules
//! (`completion`, `routing`, `conversation`, `narration`, `ui`, `document`).
use serde::{Deserialize, Serialize};

// ─── Identity Types ────────────────────────────────────────────

/// A tool's registry id (e.g. `"corpus_search"`) — the key `ToolRegistry` dispatches on. Plain `String`, no newtype validation.
pub type ToolId = String;
/// Identifier shared by a `Task` and its `Plan`; a uuid minted per planning run.
pub type TaskId = String;
/// Identifier of a `Conversation` — stable for the conversation's whole life and the correlation key on event payloads.
pub type ConversationId = String;
/// Identifier of a single `Message` within a conversation.
pub type MessageId = String;

/// Canonical entity names + aliases extracted from the live atlases, used by
/// the relationship-weighted memory-decay path. Relocated here from
/// `sovereign-core::memory` (which re-exports it) so the `LandscapeDigestProvider`
/// contract trait can name it without depending on the runtime.
pub type EntityInventory = std::collections::HashSet<String>;

// ─── Inference Types ───────────────────────────────────────────

/// The derived slot shadow of an OICP latency class (SLOT_POLICY §8).
/// A request's true routing input is its `InferenceRequirements`
/// envelope; `preferred_speed` is a legacy projection of that, written
/// only by `slot_policy::latency_to_speed` (never a free-hand literal
/// in new code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Speed {
    /// Interactive tier: routed to the fast slot (the small, always-resident model). The serde default.
    #[default]
    Fast,
    /// Retained ONLY for serde compatibility (stored `Plan`s embed the
    /// PascalCase `"Medium"` string, and dropping the variant would
    /// silently parse-fail them to empty) and for descriptive capability
    /// metadata (`ProviderCapabilities::relative_speed`). It is NOT a
    /// routing target: `latency_to_speed` never yields it, and
    /// construction sites use `Fast` or `Slow` (SLOT_POLICY §8). At the
    /// engine it is indistinguishable from `Slow` (both pick the primary
    /// slot).
    Medium,
    /// Quality tier: routed to the primary slot; latency is secondary.
    Slow,
}

/// Coarse reasoning-depth scale. Descriptive capability metadata
/// (`ProviderCapabilities::relative_reasoning`), not a routing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Depth {
    /// Surface-level: recall and single-hop answers.
    Shallow,
    /// Bounded multi-step reasoning — the tier the built-in local slots advertise.
    Moderate,
    /// Extended multi-hop reasoning — the strongest tier a provider can claim.
    Deep,
}

/// User-configurable inference parameters, sourced from `DesktopConfig`.
/// Passed to `Runtime::new()` and used when building every `CompletionRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceConfig {
    /// Generation temperature for conversational responses (0.0–1.0).
    pub temperature: f32,
    /// Maximum tokens to generate per response.
    pub max_tokens: usize,
    /// Maximum tokens allowed inside a `<think>` block before the
    /// generation loop force-closes it.
    pub think_budget: usize,
    /// Top-k sampling parameter. `None` defers to the model family default
    /// in `ModelQuirks::default_top_k` (or the sampler's hard fallback of 40).
    pub top_k: Option<u32>,
    /// Epistemic humility mode.
    ///
    /// After each synthesis the runtime audits its own answer: if the
    /// model judges that a specific external source would materially
    /// sharpen the response, it surfaces an `InformationRequest` card
    /// asking the user to paste one. On paste, the answer is re-
    /// synthesised with the source folded in; on skip, the original
    /// corpus-only answer stands.
    ///
    /// Costs one Fast-slot call (~200–500ms) per synthesis. The Slow-
    /// slot refinement only runs when the user actually provides
    /// content. Default **on**; retained as a flag so power users can
    /// disable it for cost or testing.
    #[serde(default = "default_auto_collaborate")]
    pub auto_collaborate: bool,
    /// User-authored standing instructions ("custom instructions" /
    /// persona). Appended as the final, outermost layer of the system
    /// prompt — layered ON TOP of the situated context, never replacing
    /// any of it. Global (applies to every conversation). `None`/empty
    /// yields a byte-identical prompt to the no-persona case. See
    /// `Runtime::build_system_message`.
    #[serde(default)]
    pub custom_instructions: Option<String>,
}

pub(crate) fn default_auto_collaborate() -> bool {
    true
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_tokens: 2048,
            think_budget: 512,
            top_k: None,
            auto_collaborate: default_auto_collaborate(),
            custom_instructions: None,
        }
    }
}

// ── extracted submodules (façade re-export; ARCH §3.2) ──
// EXPLICIT lists, not globs (quality program R1, 2026-07-11): a new pub item
// in a submodule joins the `types::*` surface — and therefore the
// sovereign-contracts and sovereign-core crate roots — only by being added
// HERE, as a reviewable diff. api-gate snapshots the resulting surface.
mod completion;
pub use completion::{
    CompletionRequest, CompletionResponse, FinishReason, PromptShape, ProviderCapabilities,
    SamplingMode, StreamFrame, StreamUsage, ToolSchema,
};
mod edit_slot;
pub use edit_slot::{EditSlotInfo, FimLane, FimStyle, NextEditFormat, NextEditLane};
// Generic local-journal machinery (file layout, rotation, caps,
// off-switches) — feature-agnostic on purpose; `svrn journal` hosts more
// than one stream. Per-feature vocabularies live in their own modules and
// re-export only their own names.
pub mod journal;
pub use journal::{
    journal_dir, JournalStream, DISABLED_MARKER, JOURNAL_DIR_ENV, JOURNAL_ENV, KEEP_DAYS,
    MAX_FILE_BYTES,
};
mod next_edit_journal;
pub use next_edit_journal::{
    append as journal_append, read_all as journal_read_all, stats as journal_stats, JournalLine,
    JournalStats, NextEditEpisode, NextEditOutcome, NextEditOutcomeLine, NEXT_EDIT_JOURNAL_SCHEMA,
    NEXT_EDIT_STREAM,
};
mod grounding_journal;
pub use grounding_journal::{
    append as grounding_journal_append, read_all as grounding_journal_read_all,
    stats as grounding_journal_stats, EvidenceRef, GateJudgeVerdict, GroundingDecisionLine,
    GroundingLine, GroundingStats, GROUNDING_JOURNAL_SCHEMA, GROUNDING_STREAM,
};
mod routing;
pub use routing::{
    compute_trust_level, Effect, Effort, Idempotency, Intent, Latency, Operation, Permission,
    Scope, ToolContext, ToolDescriptor, ToolExample, TrustLevel,
};
mod conversation;
pub use conversation::{
    Conversation, ConversationContext, ConversationTopicContext, HistoryRetrievalHit,
    LandscapeDigest, Message, Role, SearchedSourceEntry, TemporalTension, ToolDossier,
    ToolDossierEntry, ToolDossierOutcome, WorkingMemory,
};
mod epistemic;
pub use epistemic::{
    AcquisitionRoute, CitationTarget, CoverageLevel, Demand, DemandFacet, EpistemicState, Gap,
    GapCoverage, Holding, MemoryBand, Provenance, ReleasedCitation, TurnVerdict, Verification,
    EPISTEMIC_STATE_VERSION,
};
mod grounding_verdict;
pub use grounding_verdict::{
    AnswerSegment, DeciderId, GroundingDecision, GroundingVerdict, SegmentKind,
};
mod stage_attribution;
pub use stage_attribution::{
    ServedBy, StackOwner, StageCause, StageId, StageMechanism, StageRow, TurnStageLedger,
};
mod narration;
pub use narration::{
    build_next_step_offers, decide_policy, ClarificationOption, ClarificationRequest,
    ConfidenceThresholds, ConfidenceTier, IntentCandidate, InterpretationProposed, MoveKind,
    NarrationEvent, NarrationPhase, NextStepOffer, OfferContext, ProposedAlternative,
    ResumeSession, RouterClassification, RoutingPolicy, RoutingTiming, TurnNarration,
};

// ─── Plan Types ────────────────────────────────────────────────

/// An executable DAG of steps, produced by the planner from a user goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Fresh uuid minted per planning run — a replan gets a new id.
    pub id: TaskId,
    /// The goal this plan decomposes, as handed to the planner.
    pub goal: String,
    /// Plan nodes; `edges`, `StepInput::step_id` and `{N.key}` templates refer to steps by their `Step::id`.
    pub steps: Vec<Step>,
    /// Dependency edges `(from, to)`: step `to` may not start until `from` completes. Values are indices into `steps`.
    pub edges: Vec<(usize, usize)>,
}

impl Plan {
    /// Kahn's-algorithm layering of the DAG: each returned batch is a set of
    /// steps whose dependencies are all satisfied, so a batch can run in
    /// parallel. Out-of-range edges are ignored, and steps trapped on a cycle
    /// are silently omitted (the loop stops when no in-degree-0 step remains) —
    /// the planner separately rejects cyclic plans with `Error::Planning`.
    pub fn topological_batches(&self) -> Vec<Vec<&Step>> {
        let n = self.steps.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

        for &(from, to) in &self.edges {
            if from < n && to < n {
                adj[from].push(to);
                in_degree[to] += 1;
            }
        }

        let mut batches = Vec::new();
        let mut completed = vec![false; n];

        loop {
            let batch: Vec<usize> = (0..n)
                .filter(|&i| !completed[i] && in_degree[i] == 0)
                .collect();

            if batch.is_empty() {
                break;
            }

            let step_refs: Vec<&Step> = batch.iter().map(|&i| &self.steps[i]).collect();
            batches.push(step_refs);

            for &i in &batch {
                completed[i] = true;
                for &j in &adj[i] {
                    in_degree[j] -= 1;
                }
            }
        }

        batches
    }
}

/// One node of a `Plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Step number within the plan, referenced by `Plan::edges`, `StepInput::step_id` and `{N.key}` placeholders.
    pub id: usize,
    /// What this step does, in the planner's words — surfaced in progress UI and logs.
    pub description: String,
    /// Which execution strategy runs this step, with its parameters.
    pub kind: StepKind,
    /// Planner-declared "pause for user approval before running" flag. Not yet
    /// read by the executor (planners currently always emit `false`); approval
    /// today is enforced at the tool-permission layer instead.
    pub requires_approval: bool,
    /// Earlier steps' outputs to substitute into this step's prompt/params (`{stepN.key}` placeholders).
    pub inputs: Vec<StepInput>,
    /// Best-of-N sampling for `Reason`-family steps; `None` = single sample.
    #[serde(default)]
    pub sampling: Option<SamplingConfig>,
    /// Closed-loop self-check; `None` = accept the first output.
    #[serde(default)]
    pub evaluation: Option<EvaluationConfig>,
}

/// Executor dispatch for a step — each variant is one execution strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepKind {
    /// Single LLM call over a prompt template, no tool access.
    Reason {
        /// Prompt with `{N.key}` placeholders resolved from `Step::inputs` before the call.
        prompt_template: String,
        /// Slot tier the call is routed to.
        speed: Speed,
    },
    /// Invoke one registered tool with fixed (template-resolvable) params.
    Tool {
        /// Registry id of the tool to invoke.
        tool_id: ToolId,
        /// JSON arguments; `{N.key}` placeholders inside are substituted like prompt templates.
        params: serde_json::Value,
    },
    /// Pause the task and ask the user a short free-form question; the reply becomes the step output.
    UserInput {
        /// The question, shown to the user verbatim.
        question: String,
    },
    /// Two-way conditional. The executor resolves `condition`, asks the model
    /// yes/no over the completed-step context, and emits `StepOutput::Jump`.
    Branch {
        /// Natural-language condition put to the model as a yes/no question — not an expression language.
        condition: String,
        /// Step id jumped to when the model answers yes.
        if_true: usize,
        /// Step id jumped to when the model answers no.
        if_false: usize,
    },
    /// Iterative reasoning with tool access. The model thinks, calls tools,
    /// examines results, and decides whether to search again or synthesize.
    ReasonWithTools {
        /// Prompt with `{N.key}` placeholders, resolved before the loop starts.
        prompt_template: String,
        /// Slot tier for the loop's reasoning calls.
        speed: Speed,
        /// Tool subset the loop may call (least privilege).
        available_tools: Vec<ToolId>,
        /// Hard bound on think→search cycles before synthesis is forced (see `StepOutput::ReasonWithToolsResult::capped`).
        max_iterations: usize,
    },
    /// Delegate a focused subtask to a context-firewall worker. The worker
    /// runs a scoped rich-param tool loop in its OWN context — the raw tool
    /// observations (a page DOM, a spreadsheet's cells) accumulate inside the
    /// worker and never reach the orchestrator. It returns ONLY a typed
    /// summary matching `return_schema` plus an `anomalies` channel, so the
    /// orchestrator decides on a compact contract, not a wall of raw output.
    /// This is the §5.2 context-firewall: isolate where context is large and
    /// coupling is low ("pull four figures out of an 80-page PDF"). Unlike
    /// `ReasonWithTools` (a search loop keyed on `{query}`), the worker drives
    /// rich-param tools via the `{name, arguments}` protocol, so it can
    /// actuate (browser, etc.).
    Delegate {
        /// The subtask, as a self-contained instruction to the worker.
        goal: String,
        /// The tool subset the worker may use (least privilege).
        tools: Vec<ToolId>,
        /// JSON schema the worker's structured return is constrained to. An
        /// `anomalies` string field is always added (the surprises channel).
        return_schema: serde_json::Value,
        /// Worker loop bound, like `ReasonWithTools`'s `max_iterations`.
        max_iterations: usize,
    },
    /// Asynchronously surface a structured information request to the user
    /// and suspend the task until the user either pastes relevant content
    /// or skips. Unlike `UserInput` (which asks a short free-form question),
    /// this step presents a multi-field card describing the agent's current
    /// understanding, the precise gap, why it matters, and what kind of
    /// source would satisfy it.
    ///
    /// Step output is `StepOutput::Text(user_content)` when the user pastes
    /// content, or `StepOutput::Text("")` on skip. Subsequent steps can
    /// `{stepN.output}` the content into their prompts.
    AwaitUserInfo {
        /// The structured card content surfaced to the user.
        request: InformationRequest,
    },
}

/// Discriminates the two producers of `InformationRequest`. The UI
/// renders each kind with distinct chrome because the user-facing
/// contract differs:
///
/// - **`Refinement`** — post-answer epistemic-humility audit. The
///   conversation already has a complete answer; the card is an
///   optional "would source X sharpen this?" prompt. Skipping leaves
///   the original answer intact.
/// - **`StepBlock`** — a planned `StepKind::AwaitUserInfo` step that
///   has suspended a task. Skipping advances the task with empty
///   step output, which downstream steps will consume as a real
///   (empty) value — semantically different from "no card was ever
///   shown."
///
/// Producers must stamp the right variant; the UI uses this to pick
/// header text, dismiss semantics, and visual anchoring. `Default`
/// is `Refinement` because it's the conservative choice on the wire
/// (stale producers / older clients won't accidentally render a
/// task-blocking card).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InformationRequestKind {
    /// Post-answer epistemic-humility card: optional, and skipping leaves the already-delivered answer intact.
    #[default]
    Refinement,
    /// A suspended `AwaitUserInfo` step: skipping resumes the task with empty step output.
    StepBlock,
}

/// Structured information request surfaced when the agent has a specific,
/// nameable gap that the local corpus can't fill. Rendered in the UI as a
/// dedicated card (not a chat bubble) with the four fields spelled out.
///
/// Produced by `sovereign-core`'s `run_collaboration` on abstained turns
/// (detection is the gate's abstention signal — I4-C retired the LLM
/// gap judge; see bench/gap_check/DECISION.md) and by
/// `StepKind::AwaitUserInfo` for planned task steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InformationRequest {
    /// What the agent currently believes, with appropriate uncertainty.
    pub current_understanding: String,
    /// The precise gap as a specific question or claim to verify.
    pub gap: String,
    /// Why resolving the gap would change or sharpen the final answer.
    pub relevance: String,
    /// What kind of source would satisfy the request (a paper, a stat,
    /// a primary document, etc.). Concrete enough that the user knows
    /// when they've found the right thing.
    pub satisfying_source: String,
    /// Optional places to look or search terms to try.
    #[serde(default)]
    pub search_hints: Vec<String>,
    /// Task / step this request blocks. Populated by the executor before
    /// emitting — not required from the planner.
    #[serde(default)]
    pub task_id: String,
    /// Step index within the blocked task; populated alongside `task_id` by the executor.
    #[serde(default)]
    pub step_id: usize,
    /// Producer discriminator. See [`InformationRequestKind`].
    #[serde(default)]
    pub kind: InformationRequestKind,
    /// Human-readable task goal — populated by the executor for
    /// `StepBlock` cards so the UI can show "Task: <goal>" in the
    /// card header. Empty for `Refinement` cards.
    #[serde(default)]
    pub task_title: String,
    /// Catalog-grounded acquisition conjectures for this gap —
    /// concrete places the user could fetch what would fill it
    /// (install a recipe, connect a source, search the web). Resolved
    /// by the runtime's acquisition resolver (EPISTEMIC_STATE.md
    /// §4.3); structurally never model-invented. Empty when the
    /// resolver is disabled or couldn't rank a route.
    #[serde(default)]
    pub routes: Vec<AcquisitionRoute>,
}

/// Emitted after an already-streamed assistant message has been
/// re-synthesised with user-supplied content (see
/// `Runtime::maybe_collaborate`). The UI uses `message_id` to find
/// the existing bubble and replace its content in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRefinedPayload {
    /// Conversation containing the message being replaced.
    pub conversation_id: String,
    /// Id of the already-rendered assistant bubble whose content is swapped.
    pub message_id: String,
    /// Full replacement content (the re-synthesised answer), not a delta.
    pub new_content: String,
}

/// Fire-and-forget draft-lesson proposal (TEACHABLE P0 —
/// `sovereign-desktop/TEACHABLE.md`). Emitted by the conation
/// handler's detached capture spawn when a durative coaching turn
/// produced a draft. Carries the FULL draft so consent is stateless:
/// the desktop either passes this payload (possibly with an edited
/// `display`) to the lesson-save command later, or does nothing —
/// dismissal stores nothing and resolves no channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonProposedPayload {
    /// Draft uuid — journal correlation key. NOT the eventual note id
    /// (that is minted by the save command).
    pub id: String,
    /// Conversation the coaching turn happened in.
    pub conversation_id: String,
    /// Prior assistant message the coaching referred to. Empty when
    /// the conversation had no prior assistant turn.
    pub message_id: String,
    /// The rule as the user reads it (settings display sentence).
    pub display: String,
    /// Compiled minimal-token imperative; empty for param/transform
    /// rungs, which never ride the prompt.
    pub prompt_form: String,
    /// Enforcement rung: `"param"` | `"transform"` | `"prompt"`.
    pub enforcement: String,
    /// Rung-specific parameters, e.g. `{"soft_target_cap":300}` or
    /// `{"terms":["corpus","index"]}`. `{}` for prompt lessons.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Verbatim user coaching excerpt (provenance, truncated).
    pub taught_from: String,
}

/// Reference to an earlier step's output, substituted into a later step's
/// prompt or params as the `{step_id.key}` placeholder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepInput {
    /// Id of the producer step whose output is consumed.
    pub step_id: usize,
    /// `"output"` selects the whole text output; any other value indexes a key of a `StepOutput::Json` object.
    pub key: String,
}

/// What a completed step produced. Stored in `Task::completed_steps` and consumed by later steps' `StepInput`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepOutput {
    /// Plain text (Reason steps, user replies, pasted `AwaitUserInfo` content).
    Text(String),
    /// Structured output (tool results, `Delegate` returns) — indexable by `StepInput::key`.
    Json(serde_json::Value),
    /// Emitted by `Branch`: the step id to jump to. Carries no data; substitutes as empty text.
    Jump(usize),
    /// The step never ran (untaken branch arm); substitutes as empty text.
    Skipped,
    /// Result of a `ReasonWithTools` loop, carrying the search transcript for provenance.
    ReasonWithToolsResult {
        /// The final synthesized answer.
        text: String,
        /// One entry per tool call the loop made — glassbox provenance for the UI.
        search_log: Vec<SearchLogEntry>,
        /// Think→search cycles actually used.
        iterations: usize,
        /// True when the loop hit `max_iterations` and synthesis was forced rather than chosen.
        capped: bool,
    },
}

/// One tool call in a `ReasonWithTools` search log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchLogEntry {
    /// Loop iteration that issued the call.
    pub iteration: usize,
    /// Registry id of the tool invoked.
    pub tool_id: ToolId,
    /// The query string the model passed to the tool.
    pub query: String,
    /// Hits returned — 0 marks a dead-end search the model had to route around.
    pub result_count: usize,
}

/// Failure record for one step: what the executor reports and the replanner receives when deciding recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepError {
    /// Id of the failing step within the plan.
    pub step_id: usize,
    /// Failure detail, rendered from the underlying error.
    pub message: String,
}

// ─── Task Types ────────────────────────────────────────────────

/// A goal being executed as a plan. Durably persisted (the `tasks` table) so
/// execution survives restarts and can resume from `completed_steps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Task id (also what progress events correlate on).
    pub id: TaskId,
    /// Conversation the task was started from; progress and results are routed back to it.
    pub conversation_id: ConversationId,
    /// Verbatim user goal the plan was derived from.
    pub goal: String,
    /// The step DAG being executed.
    pub plan: Plan,
    /// Lifecycle state; `Paused` means waiting on user input or approval.
    pub status: TaskStatus,
    /// Success-only cache of `(step_id, output)` pairs — the resume fast path.
    /// The durable *attempt* ledger (crash/replay safety) is `StepExecution`.
    pub completed_steps: Vec<(usize, StepOutput)>,
    /// Creation time (Unix timestamp).
    pub created_at: i64,
    /// Last persisted mutation (Unix timestamp).
    pub updated_at: i64,
    /// Sync-readiness Lamport stamp: set to the write timestamp on every mutation; 0 on legacy rows.
    #[serde(default)]
    pub version: i64,
}

/// Task lifecycle. Serialized lowercase (`"running"`, ...) in the store and on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Steps are actively executing.
    Running,
    /// Suspended awaiting the user (a `UserInput`/`AwaitUserInfo` step or an approval).
    Paused,
    /// Every step finished; terminal.
    Completed,
    /// A step failed beyond retry/replan; terminal.
    Failed,
}

// ─── Step Execution (durable attempt ledger) ───────────────────

/// One durable record of an *attempt* to run a plan step's side effect —
/// the replay-safety + audit anchor that [`Task::completed_steps`] (a
/// success-only cache) cannot provide.
///
/// The executor writes a `Started` row **before** a write-effectful tool
/// runs and flips it to `Completed` **after** it returns. The gap between
/// those two writes is the crash-replay hazard: if the process dies after
/// the side effect but before the row completes, on resume a
/// `Started`-but-not-terminal row for a `NonIdempotent` tool tells the
/// executor "this may already have run — do not blind-replay."
///
/// `summary` + `anomalies` are the compressed, decision-relevant return
/// (the context-firewall hot path); the bulky raw observation lives as an
/// artifact addressed by handle elsewhere, never inline here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecution {
    /// Unique per attempt; a resume that re-runs the step writes a new row.
    pub id: String,
    /// Owning task.
    pub task_id: TaskId,
    /// Step within the task's plan this attempt ran.
    pub step_id: usize,
    /// The tool whose side effect this attempt ran (empty for non-tool steps).
    pub tool_id: String,
    /// Attempt lifecycle — a non-terminal `Started` row found on resume means the side effect may already have run.
    pub status: ExecutionStatus,
    /// Stable per `(task_id, step_id)` so a replay carries the same key —
    /// the handle a downstream service can dedupe on. v1 derives it as
    /// `"{task_id}:{step_id}"`; tools may later supply a content-derived key.
    pub idempotency_key: String,
    /// Compressed, decision-relevant result. `None` until terminal.
    pub summary: Option<String>,
    /// Dedicated channel for surprises the next decision must see
    /// (partial success, unexpected state, skipped sub-work).
    pub anomalies: Option<String>,
    /// When the `Started` row was written — i.e. just *before* the side effect ran (Unix timestamp).
    pub started_at: i64,
    /// `None` while `Started`; set when the row reaches `Completed`/`Failed`.
    pub ended_at: Option<i64>,
}

/// Lifecycle of a single [`StepExecution`]. `Started` is the danger state
/// on resume; `Completed` / `Failed` are terminal and replay-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionStatus {
    /// Row written just before the side effect runs; the non-terminal danger state on resume.
    Started,
    /// Side effect finished and its result was recorded; terminal, replay-safe.
    Completed,
    /// The attempt errored after starting; terminal (the side effect may or may not have landed).
    Failed,
}

impl ExecutionStatus {
    /// Canonical DB string. SSOT for the persisted form (ARCH §2.1) — the
    /// stores call this instead of re-listing the mapping per backend.
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionStatus::Started => "started",
            ExecutionStatus::Completed => "completed",
            ExecutionStatus::Failed => "failed",
        }
    }

    /// Parse a persisted status. An unrecognized value falls back to
    /// `Started` — the conservative choice: an unparseable row is treated
    /// as possibly-in-flight (suspends) rather than silently "done".
    pub fn from_db(s: &str) -> Self {
        match s {
            "completed" => ExecutionStatus::Completed,
            "failed" => ExecutionStatus::Failed,
            _ => ExecutionStatus::Started,
        }
    }
}

// ─── Memory Types ──────────────────────────────────────────────

/// What kind of memory a row is. The default (`Raw`) is what every
/// memory written before the rolling-compaction work (2026-05-23) was
/// implicitly. `Summary` rows are mechanically produced by the
/// compaction worker — they distill `source_memory_ids.len()` Raw
/// rows into a single bounded-length entry so a witness session that
/// stays on one topic doesn't grow its system prompt unboundedly.
///
/// Rendering treats both kinds identically except for a
/// `[summary of N entries, YYYY-MM-DD → YYYY-MM-DD]` prefix on
/// Summary rows so the model (and the writer, via ProvenancePanel)
/// can see when a recall is mechanical distillation vs verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MemoryKind {
    /// Verbatim extraction from a conversation — what every pre-compaction memory implicitly was. The default.
    #[default]
    Raw,
    /// Mechanical distillation of the rows listed in `Memory::source_memory_ids`, produced by the compaction worker.
    Summary,
}

/// One durable memory row: extracted from conversations, recalled into future
/// system prompts, decayed by age, and compacted into `Summary` rows over time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Memory {
    /// Store primary key.
    pub id: String,
    /// The memory text itself — what recall injects into the system prompt.
    pub content: String,
    /// Producer label, e.g. `"conversation_extraction"` — which path wrote the row, not a document reference.
    pub source: String,
    /// Extractor confidence in [0, 1]. Decays ~0.9^months since `last_used`;
    /// recall tiers memories by thresholds on the decayed value.
    pub confidence: f64,
    /// Extraction time (Unix seconds).
    pub created_at: i64,
    /// Last recall time (Unix seconds) — the anchor the confidence decay is measured from.
    pub last_used: i64,
    /// Sync-readiness Lamport stamp: set to the write timestamp on every mutation; 0 on legacy rows.
    #[serde(default)]
    pub version: i64,
    /// Soft-delete tombstone (user revocation). Kept as a row so a future device sync can propagate the deletion; `None` = live.
    #[serde(default)]
    pub deleted_at: Option<i64>,
    /// ID of the conversation this memory was extracted from, if any.
    /// Populated going forward by memory extraction paths that know the
    /// source conversation; `None` for memories predating the
    /// KnowledgeView migration or for memories extracted outside a
    /// conversational context.
    #[serde(default)]
    pub source_conversation_id: Option<String>,
    /// Skill scope this memory belongs to. Denormalized at extract
    /// time from `conversations.skill_id`. The recall layer enforces
    /// a bidirectional wall: in scoped contexts (e.g. inner-work),
    /// only memories with the matching scope surface; in general
    /// contexts, scoped memories are excluded so they can't leak
    /// across surfaces.
    ///
    /// `None` = "general pool" — recallable in general contexts,
    /// invisible to scoped contexts. Set at extract time inside
    /// `Runtime::end_conversation` based on the conversation's
    /// `skill_id`. Existing rows backfilled by
    /// `run_inner_work_memory_wall_migrations`.
    #[serde(default)]
    pub source_skill_id: Option<String>,
    /// Whether this row was written by the extraction path (`Raw`)
    /// or synthesized by the compaction worker (`Summary`).
    /// Backfilled as `Raw` for all pre-2026-05-23 rows.
    #[serde(default)]
    pub kind: MemoryKind,
    /// For `Summary` rows: the ids of the `Raw` (or earlier `Summary`)
    /// memories this row distills. Empty for `Raw`. The relationship
    /// is mechanical and rebuildable — `sovereign memory
    /// rebuild-summaries` drops and re-derives summaries for a
    /// conversation after a synthesis-prompt edit.
    #[serde(default)]
    pub source_memory_ids: Vec<String>,
    /// Set on a `Raw` (or earlier `Summary`) row that has been folded
    /// into a newer `Summary`. The value is the new summary's id.
    /// Retrieval filters `superseded_by IS NULL` so a superseded row
    /// stops surfacing in recall — but the body is preserved for
    /// provenance (`sovereign memory expand <summary-id>` walks the
    /// chain).
    ///
    /// Distinct from `deleted_at`: `deleted_at` is a user-initiated
    /// revocation; `superseded_by` is mechanical compaction. The two
    /// are independent — a memory can be both superseded and deleted.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// T1 persistent embedding of `content`, produced by the
    /// document-side embed path (`embed_batch`) — the SAME call recall
    /// uses on memory contents, so stored and freshly-computed vectors
    /// rank identically. `None` until computed (write-path compute in
    /// `save_with_contradiction_check` / compaction, or lazy backfill
    /// on first recall). Never sent over serde when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// `InferenceProvider::embed_model_id()` of the model that produced
    /// `embedding`. The staleness guard: recall reuses `embedding` only
    /// when this matches the current provider's id (and neither side is
    /// `"unknown"`) — a same-dimension different-model vector would
    /// silently mis-rank, so a mismatch means "re-embed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
}

/// User-feedback record of a routing mistake. The store surfaces only
/// `was_correct = false` rows, and the router folds them into its Pass-1
/// prompt as "previous classification mistakes (avoid these)".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingCorrection {
    /// Hash of the misclassified user message (see `router::message_hash`) — the raw text is not stored.
    pub message_hash: String,
    /// The intent label the router chose (wrongly).
    pub classified_as: String,
    /// User verdict on the classification. Persisted rows queried back are always `false` — see the type doc.
    pub was_correct: bool,
    /// When the correction was recorded (Unix timestamp).
    pub created_at: i64,
}

// ─── Document / RAG Types ──────────────────────────────────────

/// Provenance class of a `DocumentChunk` — what namespace its `source` field
/// lives in. Serde-tagged `type`; persisted via `to_db_columns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum SourceType {
    /// A file the user uploaded. The default, and what legacy/unknown rows map to.
    #[default]
    UserDocument,
    /// A chunk from an installed corpus.
    Corpus {
        /// Id of the installed corpus the chunk belongs to.
        corpus_id: String,
    },
    /// A chunk fetched from the web.
    WebSearch {
        /// Source page URL. NOT persisted: `to_db_columns` drops it and `from_db_columns` restores it empty.
        url: String,
    },
}

impl SourceType {
    /// Project to the persisted `(source_type, corpus_id)` column pair. Lossy for `WebSearch` — the url is dropped.
    pub fn to_db_columns(&self) -> (&'static str, Option<&str>) {
        match self {
            SourceType::UserDocument => ("user", None),
            SourceType::Corpus { corpus_id } => ("corpus", Some(corpus_id.as_str())),
            SourceType::WebSearch { .. } => ("web", None),
        }
    }

    /// Inverse of `to_db_columns`. Unknown type strings fall back to `UserDocument`; a `WebSearch` comes back with an empty url.
    pub fn from_db_columns(source_type: &str, corpus_id: Option<&str>) -> Self {
        match source_type {
            "corpus" => SourceType::Corpus {
                corpus_id: corpus_id.unwrap_or_default().to_string(),
            },
            "web" => SourceType::WebSearch { url: String::new() },
            _ => SourceType::UserDocument,
        }
    }
}

/// Which retrieval path produced an answer, with the reason for the choice.
/// Serde-tagged `method`, snake_case on the wire (a transparency payload for
/// surfaces; not constructed anywhere in the Rust workspace today).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum SearchMethod {
    /// Local corpus retrieval alone answered.
    LocalOnly,
    /// Local retrieval answered, supplemented by web results.
    LocalPlusWeb {
        /// Why local alone wasn't enough.
        reason: String,
    },
    /// Local-only answer that is known to be incomplete.
    LocalOnlyIncomplete {
        /// What is known to be missing from the answer.
        reason: String,
    },
    /// Nothing local was relevant; web results only.
    WebOnly {
        /// Why local retrieval contributed nothing.
        reason: String,
    },
    /// Neither local nor web produced usable results.
    NoResults {
        /// What was tried and came up empty.
        reason: String,
    },
}

/// Coverage judgment over local retrieval: do the local hits suffice, or is
/// web search needed? Serde-tagged `decision`, snake_case on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CoverageDecision {
    /// Local hits cover the question; no web call.
    Sufficient,
    /// Local hits help but leave a nameable gap worth a web call.
    SupplementWithWeb {
        /// The nameable gap the web call should fill.
        reason: String,
    },
    /// Local hits are not usable; the answer must come from the web.
    RequiresWeb {
        /// Why local coverage was judged unusable.
        reason: String,
    },
}

/// Provenance of a single cited source, for per-citation attribution.
/// Serde-tagged `origin`, snake_case on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum SourceOrigin {
    /// Cited from an installed corpus.
    Local {
        /// Id of the corpus the citation came from.
        corpus: String,
        /// Title of the cited article/document within the corpus.
        article_title: String,
    },
    /// Cited from a web page.
    Web {
        /// Full page URL.
        url: String,
        /// Domain extracted from `url` (display form).
        domain: String,
    },
    /// Cited from a file the user uploaded.
    UserDocument {
        /// Original filename of the upload.
        filename: String,
    },
}

/// Monthly web-search quota for one backend. Persisted in the
/// `search_budget` table and consulted before issuing web searches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchBudget {
    /// Search backend this row meters (the table key).
    pub backend: String,
    /// Allowed searches per month.
    pub monthly_limit: u32,
    /// Searches consumed since the last reset.
    pub used_this_month: u32,
    /// When `used_this_month` next resets (Unix timestamp).
    pub reset_date: i64,
    /// Sync-readiness Lamport stamp: set to the write timestamp on every mutation; 0 on legacy rows.
    #[serde(default)]
    pub version: i64,
}

/// Who may retrieve from a corpus on a shared (multi-user) hub. The server
/// boundary turns this into a per-request allow-list ceiling; the Runtime
/// never sees principals, only the resulting corpus-id set (see the
/// `sovereign-server` crate). Stored as JSON in the `corpus_state.visibility`
/// column; a `NULL` column (pre-migration rows) maps to `Org`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorpusVisibility {
    /// Visible to every authenticated principal — the operator's shared
    /// corpus. The default, and what every legacy corpus maps to.
    #[default]
    Org,
    /// Visible only to the principal whose id equals `owner` (a user's own
    /// upload).
    Private {
        /// The principal id allowed to retrieve from this corpus.
        owner: String,
    },
}

impl CorpusVisibility {
    /// The owner of a `Private` corpus, or `None` for `Org`.
    pub fn owner(&self) -> Option<&str> {
        match self {
            CorpusVisibility::Org => None,
            CorpusVisibility::Private { owner } => Some(owner),
        }
    }
}

/// Installed-corpus bookkeeping row (the `corpus_state` table): install/update
/// times, size stats, vector-index readiness, and the visibility boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusState {
    /// Corpus this row describes (the table key).
    pub corpus_id: String,
    /// Install time (Unix timestamp).
    pub installed_at: i64,
    /// Publisher-declared date of the corpus snapshot (free-form string from the corpus builder).
    pub source_date: String,
    /// Chunks in the installed index.
    pub chunks_count: i64,
    /// On-disk index size, megabytes.
    pub index_size_mb: i64,
    /// When the corpus content last changed (Unix timestamp).
    pub last_updated: i64,
    /// Sync-readiness Lamport stamp: set to the write timestamp on every mutation; 0 on legacy rows.
    #[serde(default)]
    pub version: i64,
    /// Soft-delete tombstone (uninstall). Kept as a row for sync-readiness; `None` = installed.
    #[serde(default)]
    pub deleted_at: Option<i64>,
    /// True when the IVF-PQ vector index is built for this corpus.
    /// When false, searches fall back to FTS only (no full-scan hang).
    #[serde(default)]
    pub vector_index_ready: bool,
    /// Who may retrieve from this corpus on a shared multi-user hub.
    /// Defaults to `Org` (shared), so single-user and operator-curated
    /// deployments are unaffected; per-user uploads set `Private { owner }`.
    #[serde(default)]
    pub visibility: CorpusVisibility,
}

/// A retrieval hit: chunk plus relevance score. In-process ranking currency only — deliberately not serializable.
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    /// The chunk that matched.
    pub chunk: DocumentChunk,
    /// Relevance assigned by the retrieval path; higher is better, comparable only within one result set.
    pub score: f32,
}

/// One embeddable unit of a source document — the retrieval currency across the store, the index, and RAG prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    /// Store primary key.
    pub id: String,
    /// Owning document key (which namespace it lives in is `source_type`'s job); matches `DocumentSession::source` for uploads.
    pub source: String,
    /// Chunk text, as embedded and as quoted into prompts.
    pub content: String,
    /// 0-based position within the source document.
    pub chunk_index: usize,
    /// Embedding of `content`; `None` until an embed pass computes it (such rows are FTS-only).
    pub embedding: Option<Vec<f32>>,
    /// Ingest time (Unix timestamp).
    pub created_at: i64,
    /// Provenance class; the serde default (`UserDocument`) covers legacy rows.
    #[serde(default)]
    pub source_type: SourceType,
    /// Sync-readiness Lamport stamp: set to the write timestamp on every mutation; 0 on legacy rows.
    #[serde(default)]
    pub version: i64,
    /// Soft-delete tombstone, kept for sync-readiness; `None` = live.
    #[serde(default)]
    pub deleted_at: Option<i64>,
}

// ─── Document Session Types ─────────────────────────────────────

/// A persistent session around an uploaded document.
/// Created when a user uploads a file and describes an operation.
/// Holds the planner-derived map/reduce prompts and the structured
/// output so follow-up questions can reference results cheaply
/// without re-running the full map-reduce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSession {
    /// Session id (store key).
    pub id: String,
    /// Conversation the upload happened in; the session is scoped to it.
    pub conversation_id: String,
    /// Original uploaded filename, for display.
    pub filename: String,
    /// Matches `DocumentChunk.source` — the key for chunk retrieval.
    pub source: String,
    /// Word count of the extracted text (sizing/progress display).
    pub word_count: usize,
    /// How many `DocumentChunk`s the document was split into.
    pub chunk_count: usize,
    /// Upload time (Unix timestamp).
    pub created_at: i64,
    /// The operation the user originally requested, in their words.
    pub operation: String,
    /// The map prompt the planner derived from the operation.
    pub map_prompt: String,
    /// The reduce prompt the planner derived from the operation.
    pub reduce_prompt: String,
    /// The structured output from the last completed operation.
    /// JSON — shape determined by the operation.
    pub last_output: Option<String>,
    /// Previous operations run on this document in this session.
    pub history: Vec<DocumentOperation>,
}

/// A completed operation within a document session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentOperation {
    /// What was asked, in the user's words.
    pub description: String,
    /// The operation's final output (shape depends on the operation).
    pub output: String,
    /// Completion time (Unix timestamp).
    pub completed_at: i64,
}

// ─── Execution Intelligence Types ─────────────────────────────

/// Retry configuration for tool execution on transient failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Retries *after* the first attempt — the default 2 allows up to 3 attempts total.
    pub max_retries: usize,
    /// Sleep before each retry, milliseconds, indexed by retry number (default `[1000, 3000]`).
    pub backoff_ms: Vec<u64>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_ms: vec![1000, 3000],
        }
    }
}

/// Best-of-N sampling configuration for Reason steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingConfig {
    /// How many candidates to sample.
    pub n: usize,
    /// How the winning candidate is chosen.
    pub selector: SampleSelector,
}

/// Strategy for picking the winner among the `n` sampled candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SampleSelector {
    /// Fast model reads all candidates and selects the best.
    /// `selection_prompt` overrides everything when set; otherwise
    /// `preset` determines the rubric (defaults to general
    /// accuracy + completeness when also unset).
    LlmJudge {
        /// Full override of the judge prompt. When set, `preset` is ignored.
        #[serde(default)]
        selection_prompt: Option<String>,
        /// Named rubric preset. `Voice` selects the
        /// glass-box-voice rubric defined in
        /// `executor::VOICE_JUDGE_PROMPT` (eight principles +
        /// avoid-list); `Default` is the pre-existing
        /// accuracy-focused rubric. Ignored when
        /// `selection_prompt` is supplied.
        #[serde(default)]
        preset: JudgePreset,
    },
    /// Take the most common first-line answer.
    MajorityVote,
    /// Run each candidate through a tool; first to pass wins.
    Verify {
        /// Registry id of the verifying tool each candidate is piped through.
        tool_id: ToolId,
    },
}

/// Named rubric preset for `SampleSelector::LlmJudge`. Lets plan
/// templates and harness callers ask for a specific rubric without
/// inlining the prompt every time. Backwards-compatible: the
/// default (`Default`) preserves prior `LlmJudge` behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgePreset {
    /// Pre-existing rubric: accuracy + completeness +
    /// well-reasoned + appropriately cited.
    #[default]
    Default,
    /// Glass-box-voice rubric. Scores candidates on the eight
    /// principles in `RELATIONAL_BASE_SYSTEM_PROMPT` (specific
    /// uncertainty, three registers, load-bearing questions,
    /// length discipline, edge-of-competence, disagreement
    /// permission, contradiction-across-time, self-honesty)
    /// and penalises the four avoid-list patterns. Used by the
    /// Tier-B `voice_eval` harness in sovereign-cli.
    Voice,
}

impl SampleSelector {
    /// Convenience: build an `LlmJudge` selector with the voice
    /// rubric preset and no overriding prompt — the rubric loads
    /// from `executor::VOICE_JUDGE_PROMPT`.
    pub fn voice_judge() -> Self {
        Self::LlmJudge {
            selection_prompt: None,
            preset: JudgePreset::Voice,
        }
    }

    /// Convenience: build an `LlmJudge` selector with the default
    /// rubric (pre-existing accuracy-focused selector).
    pub fn default_judge() -> Self {
        Self::LlmJudge {
            selection_prompt: None,
            preset: JudgePreset::Default,
        }
    }
}

/// Evaluation configuration for closed-loop self-correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationConfig {
    /// Rubric the evaluator model applies to the step output.
    pub eval_prompt: String,
    /// Regeneration attempts after a failed evaluation (default 1).
    #[serde(default = "default_eval_retries")]
    pub max_retries: usize,
    /// Slot tier for the evaluator call (default `Fast`).
    #[serde(default)]
    pub eval_speed: Speed,
}

pub(crate) fn default_eval_retries() -> usize {
    1
}

/// Difficulty estimate for adaptive test-time compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepDifficulty {
    /// Trim the budget — e.g. `ReasonWithTools` iterations are capped at 2.
    Routine,
    /// Keep the step's own budget unchanged; also the fallback when the estimator's answer doesn't parse.
    Moderate,
    /// Grant extra budget — e.g. two additional loop iterations.
    Hard,
}

/// Compute budget derived from difficulty estimation.
#[derive(Debug, Clone)]
pub struct ComputeBudget {
    /// Token cap for the step's generation.
    pub max_tokens: usize,
    /// Best-of-N sampling to apply; `None` = single sample.
    pub sampling: Option<SamplingConfig>,
    /// Self-check loop to apply; `None` = accept the first output.
    pub evaluation: Option<EvaluationConfig>,
    /// Force a slot tier regardless of the step's declared `speed`; `None` = keep the plan's choice.
    pub speed_override: Option<Speed>,
}

// ─── Response Types ────────────────────────────────────────────

/// What a turn returns to the surface: the assistant message, plus the task if one was spawned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// The assistant reply to render.
    pub message: Message,
    /// Set when the turn spawned a background task; `None` for plain chat turns.
    pub task: Option<Task>,
    /// Per-stage timing for diagnostic / perf-bench paths. Populated
    /// on the witness paths (`handle_expressive_query`,
    /// `handle_simple` Relational+DeepQuery branch); `None` on
    /// non-instrumented paths so we can grow the coverage
    /// incrementally. Voice-eval surfaces these in the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RuntimeMetrics>,
}

/// Per-turn millisecond breakdown across the multi-stage relational
/// pipeline. Each field is the wall-clock cost of that stage; `None`
/// means the stage was skipped (e.g. Pass A returns `None` early
/// when there are no memories).
///
/// Iter5 (2026-05-02): added after the 4B parsimony test showed
/// only ~5% speedup vs the 9B despite half the parameters — model
/// size isn't the binding constraint, so we need a stage-level
/// waterfall to know what is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeMetrics {
    /// Router::classify total. Includes Pass 1 LLM call when no
    /// pre-check fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_ms: Option<u64>,
    /// `memory::recall_relevant_memories_embed` total. Dominated by
    /// the per-memory `embed_batch` call; FTS fallback is fast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_recall_ms: Option<u64>,
    /// Iter6: per-call routing internals — pre-check chain, LLM
    /// Pass 1, parse. Surfaces whether the 6s routing slice is
    /// dominated by the LLM call (fast slot is too big) or the
    /// pre-check chain (heuristics getting fat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_breakdown: Option<RoutingTiming>,
    /// Iter6: `memory::compress_working_memory` time. Designed for
    /// code-task continuity but runs on every turn including
    /// relational. Hypothesis: skippable on Relational paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_memory_ms: Option<u64>,
    /// Iter6: `context::update_topic_context` time. Same hypothesis
    /// as working memory — may be a free win to skip on Relational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_context_ms: Option<u64>,
    /// `detect_contradiction` Pass A on Fast slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_a_ms: Option<u64>,
    /// `memory::detect_temporal_tensions` pre-pass on Fast slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tensions_ms: Option<u64>,
    /// Pass B synthesis call — the main chat completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_ms: Option<u64>,
    /// Iter6: total turn wall-clock from `handle_turn` entry to
    /// return. Used to compute "unaccounted" time = total -
    /// (sum of named stages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_turn_ms: Option<u64>,
}

// ─── Routing Decision ────────────────────────────────────────
//
// Two-layer model per the antifragile-routing design:
//
//   Router::classify → RouterClassification   (what the model/heuristics said)
//   decide_policy(classification, thresholds) → RoutingPolicy   (what the runtime does about it)
//
// The split keeps classification pure (a witness of the model's opinion)
// and policy tunable without touching the router. Threshold calibration
// (future PR4) mutates policy, not the Router trait.

// Explicit lists, not globs — see the submodule façade comment above.
mod ui;
pub use ui::{
    ActionPreview, CoverageNote, InsightNode, InsightPosition, InsightSinkState, InsightSource,
    PositionStyle, ResponseProvenance, SourceSummary, ThinFolder,
};
mod document;
pub use document::{
    ActionAtom, AssetMotif, AssetState, DocumentAsset, DocumentAssetOperation, DocumentSegment,
    DocumentSkeleton, DocumentTypeTag, EntityAppearances, EntityKind, MemRaptorNodeRow, QuoteSpan,
    RankedEntity, RaptorNode, SectionAnnotation, SectionFunction, StructuralMoment,
};

#[cfg(test)]
mod finish_reason_tests {
    use super::*;

    /// Pins the wire shape downstream consumers depend on: the desktop
    /// cutoff chip in `AssistantMessage.svelte` checks
    /// `provenance.finish_reason === "length"` (lowercase). Derive-default
    /// Serialize would emit `"Length"` (capitalized) and silently break
    /// the chip.
    #[test]
    fn finish_reason_serializes_as_openai_string() {
        assert_eq!(
            serde_json::to_string(&FinishReason::Stop).unwrap(),
            "\"stop\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::Length).unwrap(),
            "\"length\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ToolCalls).unwrap(),
            "\"tool_calls\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ContentFilter).unwrap(),
            "\"content_filter\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::Error("oom".into())).unwrap(),
            "\"error\""
        );
    }

    /// Pins backcompat for messages persisted under the shipped
    /// `ResponseProvenance.finish_reason: Option<String>` shape — the
    /// initial Phase A iteration wrote `"length"` directly. After
    /// flipping to `Option<FinishReason>`, the on-disk JSON must still
    /// decode to the typed enum.
    #[test]
    fn finish_reason_deserialize_round_trip() {
        let cases = [
            ("\"stop\"", FinishReason::Stop),
            ("\"length\"", FinishReason::Length),
            ("\"tool_calls\"", FinishReason::ToolCalls),
            ("\"content_filter\"", FinishReason::ContentFilter),
            ("\"cancelled\"", FinishReason::Cancelled),
        ];
        for (wire, expected) in cases {
            let got: FinishReason = serde_json::from_str(wire).unwrap();
            assert_eq!(got, expected, "wire {wire} should decode to {expected:?}");
        }
        // Error decodes with empty inner — the original message is
        // not on the wire, only the variant tag.
        let err: FinishReason = serde_json::from_str("\"error\"").unwrap();
        assert!(matches!(err, FinishReason::Error(ref m) if m.is_empty()));
    }

    /// Unknown OpenAI strings must surface as an error, not a silent
    /// `Stop`. A future server bug that emits e.g. `"max_tokens"`
    /// should fail loudly rather than mask itself as a clean stop.
    #[test]
    fn finish_reason_rejects_unknown_string() {
        let r: Result<FinishReason, _> = serde_json::from_str("\"bogus\"");
        assert!(
            r.is_err(),
            "unknown finish_reason should fail to deserialize"
        );
    }
}

#[cfg(test)]
mod knowledge_view_digest_tests {
    use super::*;

    fn base_context() -> ConversationContext {
        ConversationContext {
            conversation: Conversation {
                id: "c1".into(),
                title: None,
                messages: vec![],
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                skill_id: None,
                enabled_corpora: None,
                searched_sources: None,
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            corpus_ceiling: None,
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            history_retrieval_hits: None,
            tool_dossier: None,
            intent_policy: None,
        }
    }

    #[test]
    fn build_context_default_is_none() {
        let ctx = base_context();
        assert!(ctx.knowledge_view_digests.is_none());
    }

    #[test]
    fn set_landscape_digests_populates_field() {
        let mut ctx = base_context();
        ctx.set_landscape_digests(vec![LandscapeDigest {
            view_id: "personal-knowledge".into(),
            body: "body".into(),
        }]);
        let digests = ctx.knowledge_view_digests.as_ref().unwrap();
        assert_eq!(digests.len(), 1);
        assert_eq!(digests[0].view_id, "personal-knowledge");
    }

    #[test]
    fn set_landscape_digests_accepts_empty_vec() {
        // Spec invariant: post-routing the field is `Some(_)` even
        // when every view's digest was skipped (view not yet
        // enriched). Downstream callers can rely on
        // `knowledge_view_digests.is_some()`.
        let mut ctx = base_context();
        ctx.set_landscape_digests(vec![]);
        assert!(ctx.knowledge_view_digests.is_some());
        assert!(ctx.knowledge_view_digests.unwrap().is_empty());
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "knowledge_view_digests=None")]
    fn debug_assert_routed_panics_when_unpopulated() {
        let ctx = base_context();
        ctx.debug_assert_routed();
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_assert_routed_ok_when_populated() {
        let mut ctx = base_context();
        ctx.set_landscape_digests(vec![]);
        ctx.debug_assert_routed(); // must not panic
    }

    #[test]
    fn landscape_digest_round_trips_json() {
        let d = LandscapeDigest {
            view_id: "conversation-history".into(),
            body: "Active domains: foo, bar".into(),
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: LandscapeDigest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.view_id, "conversation-history");
        assert_eq!(back.body, "Active domains: foo, bar");
    }

    #[test]
    fn conversation_context_backwards_compatible_deserialization() {
        // A context serialized before the KnowledgeView migration has
        // no `knowledge_view_digests` field. `#[serde(default)]`
        // must accept it as `None`.
        let legacy = serde_json::json!({
            "conversation": {
                "id": "c1",
                "title": null,
                "messages": [],
                "created_at": 0,
                "updated_at": 0
            },
            "memories": [],
            "working_memory": null
        });
        let ctx: ConversationContext = serde_json::from_value(legacy).unwrap();
        assert!(ctx.knowledge_view_digests.is_none());
        assert!(ctx.topic_context.is_none());
    }
}

#[cfg(test)]
mod routing_policy_tests {
    use super::*;

    fn classification(confidence: f32) -> RouterClassification {
        RouterClassification {
            primary: IntentCandidate {
                intent: Intent::SimpleQuery,
                confidence,
            },
            alternatives: Vec::new(),
            rationale: None,
            coarse_intent: Some("SIMPLE".into()),
            self_assessment: None,
            timing: None,
            scope: None,
        }
    }

    #[test]
    fn high_confidence_commits() {
        let policy = decide_policy(&classification(0.95), &ConfidenceThresholds::default());
        assert_eq!(policy.tier, ConfidenceTier::High);
        assert_eq!(policy.move_kind, MoveKind::Commit);
    }

    #[test]
    fn boundary_exactly_at_high_commits() {
        // 0.80 is inclusive of the High tier.
        let policy = decide_policy(&classification(0.80), &ConfidenceThresholds::default());
        assert_eq!(policy.tier, ConfidenceTier::High);
    }

    #[test]
    fn moderate_confidence_proposes() {
        let policy = decide_policy(&classification(0.65), &ConfidenceThresholds::default());
        assert_eq!(policy.tier, ConfidenceTier::Moderate);
        assert_eq!(policy.move_kind, MoveKind::Propose);
    }

    #[test]
    fn boundary_exactly_at_moderate_proposes() {
        // 0.55 is inclusive of the Moderate tier.
        let policy = decide_policy(&classification(0.55), &ConfidenceThresholds::default());
        assert_eq!(policy.tier, ConfidenceTier::Moderate);
    }

    #[test]
    fn low_confidence_asks() {
        let policy = decide_policy(&classification(0.30), &ConfidenceThresholds::default());
        assert_eq!(policy.tier, ConfidenceTier::Low);
        assert_eq!(policy.move_kind, MoveKind::Ask);
    }

    #[test]
    fn just_under_moderate_asks() {
        let policy = decide_policy(&classification(0.549), &ConfidenceThresholds::default());
        assert_eq!(policy.tier, ConfidenceTier::Low);
    }

    #[test]
    fn thresholds_are_snapshotted_into_policy() {
        let thresholds = ConfidenceThresholds {
            high: 0.90,
            moderate: 0.70,
        };
        let policy = decide_policy(&classification(0.75), &thresholds);
        // With custom thresholds, 0.75 falls between 0.70 and 0.90 → Moderate.
        assert_eq!(policy.tier, ConfidenceTier::Moderate);
        // Glassbox: the thresholds used are visible on the returned
        // policy so the UI and operator log can see why this decision
        // was made, not just what the decision was.
        assert_eq!(policy.thresholds_used.high, 0.90);
        assert_eq!(policy.thresholds_used.moderate, 0.70);
    }

    #[test]
    fn policy_is_serde_roundtrippable() {
        // Glassbox metadata is written into message.metadata as JSON.
        // If the policy struct isn't round-trippable, the UI can't
        // render the tier badge / rationale.
        let policy = decide_policy(&classification(0.82), &ConfidenceThresholds::default());
        let json = serde_json::to_string(&policy).unwrap();
        let back: RoutingPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tier, policy.tier);
        assert_eq!(back.move_kind, policy.move_kind);
    }
}

#[cfg(test)]
mod next_step_offer_tests {
    use super::*;

    fn chunk(title: &str) -> serde_json::Value {
        serde_json::json!({
            "title": title,
            "corpus_id": "c",
            "snippet": "…",
            "provenance_tier": "corpus",
        })
    }

    #[test]
    fn empty_retrieval_emits_no_offers() {
        let ctx = OfferContext {
            user_message: "what is X",
            top_source_title: None,
            had_dominant_source: false,
            retrieved_chunks: &[],
            session_id: "sess-1",
            retrieval_missed: false,
        };
        let offers = build_next_step_offers(&ctx);
        assert!(offers.is_empty());
    }

    #[test]
    fn drill_down_offer_points_at_secondary_source() {
        let chunks = vec![
            chunk("Main Source"),
            chunk("Secondary Source"),
            chunk("Tertiary Source"),
        ];
        let ctx = OfferContext {
            user_message: "how does X work",
            top_source_title: Some("Main Source"),
            had_dominant_source: false,
            retrieved_chunks: &chunks,
            session_id: "sess-1",
            retrieval_missed: false,
        };
        let offers = build_next_step_offers(&ctx);
        assert_eq!(offers.len(), 1);
        assert!(offers[0].label.contains("Secondary Source"));
        assert_eq!(offers[0].session_ref.as_deref(), Some("sess-1"));
        assert_eq!(offers[0].intent_hint.as_deref(), Some("knowledge_query"));
    }

    #[test]
    fn dominant_source_adds_compare_offer() {
        let chunks = vec![chunk("Dominant"), chunk("Other")];
        let ctx = OfferContext {
            user_message: "explain X",
            top_source_title: Some("Dominant"),
            had_dominant_source: true,
            retrieved_chunks: &chunks,
            session_id: "sess-2",
            retrieval_missed: false,
        };
        let offers = build_next_step_offers(&ctx);
        assert_eq!(offers.len(), 2);
        assert!(offers[0].label.contains("Other"));
        assert!(offers[1].label.starts_with("Compare"));
        // The compare offer excludes the dominant source in its
        // follow-up query so the resumed synthesis reaches for
        // fresh perspectives instead of re-quoting the same doc.
        assert!(offers[1].follow_up_query.contains("besides"));
    }

    #[test]
    fn offers_capped_at_two() {
        // Even with a dominant source + a clean secondary, we
        // never emit more than two buttons.
        let chunks = vec![chunk("A"), chunk("B"), chunk("C"), chunk("D")];
        let ctx = OfferContext {
            user_message: "explain",
            top_source_title: Some("A"),
            had_dominant_source: true,
            retrieved_chunks: &chunks,
            session_id: "s",
            retrieval_missed: false,
        };
        let offers = build_next_step_offers(&ctx);
        assert!(offers.len() <= 2);
    }

    #[test]
    fn untitled_chunks_are_skipped() {
        let chunks = vec![
            serde_json::json!({ "title": "", "corpus_id": "c" }),
            chunk("Real Title"),
        ];
        let ctx = OfferContext {
            user_message: "q",
            top_source_title: Some("Main"),
            had_dominant_source: false,
            retrieved_chunks: &chunks,
            session_id: "s",
            retrieval_missed: false,
        };
        let offers = build_next_step_offers(&ctx);
        assert_eq!(offers.len(), 1);
        assert!(offers[0].label.contains("Real Title"));
    }

    #[test]
    fn retrieval_miss_suppresses_all_offers() {
        // PR5 — even with a dominant source + clean secondary,
        // `retrieval_missed = true` means the retrieval was
        // off-target; no offer should leak through. Otherwise a
        // "Commonwealth scheduler" miss would still surface a
        // "Tell me about Cartoon Reel" chip.
        let chunks = vec![chunk("Dominant"), chunk("Secondary")];
        let ctx = OfferContext {
            user_message: "anything",
            top_source_title: Some("Dominant"),
            had_dominant_source: true,
            retrieved_chunks: &chunks,
            session_id: "s",
            retrieval_missed: true,
        };
        let offers = build_next_step_offers(&ctx);
        assert!(
            offers.is_empty(),
            "miss must suppress all offers: {offers:?}"
        );
    }

    #[test]
    fn offers_are_serde_roundtrippable() {
        let offer = NextStepOffer {
            label: "Tell me about X".into(),
            description: Some("Drawn from retrieval".into()),
            follow_up_query: "what is x".into(),
            session_ref: Some("s".into()),
            intent_hint: Some("knowledge_query".into()),
        };
        let json = serde_json::to_string(&offer).unwrap();
        let back: NextStepOffer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.label, offer.label);
        assert_eq!(back.session_ref, offer.session_ref);
    }
}
