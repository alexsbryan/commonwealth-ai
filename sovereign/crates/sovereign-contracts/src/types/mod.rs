// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

// ─── Identity Types ────────────────────────────────────────────

pub type ToolId = String;
pub type TaskId = String;
pub type ConversationId = String;
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
    Slow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Depth {
    Shallow,
    Moderate,
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
mod completion;
pub use completion::*;
mod routing;
pub use routing::*;
mod conversation;
pub use conversation::*;
mod narration;
pub use narration::*;

// ─── Plan Types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: TaskId,
    pub goal: String,
    pub steps: Vec<Step>,
    pub edges: Vec<(usize, usize)>,
}

impl Plan {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: usize,
    pub description: String,
    pub kind: StepKind,
    pub requires_approval: bool,
    pub inputs: Vec<StepInput>,
    #[serde(default)]
    pub sampling: Option<SamplingConfig>,
    #[serde(default)]
    pub evaluation: Option<EvaluationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepKind {
    Reason {
        prompt_template: String,
        speed: Speed,
    },
    Tool {
        tool_id: ToolId,
        params: serde_json::Value,
    },
    UserInput {
        question: String,
    },
    Branch {
        condition: String,
        if_true: usize,
        if_false: usize,
    },
    /// Iterative reasoning with tool access. The model thinks, calls tools,
    /// examines results, and decides whether to search again or synthesize.
    ReasonWithTools {
        prompt_template: String,
        speed: Speed,
        available_tools: Vec<ToolId>,
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
    #[default]
    Refinement,
    StepBlock,
}

/// Structured information request surfaced when the agent has a specific,
/// nameable gap that the local corpus can't fill. Rendered in the UI as a
/// dedicated card (not a chat bubble) with the four fields spelled out.
///
/// See `sovereign-core/src/gap.rs::identify_gap` for how these are produced
/// and `StepKind::AwaitUserInfo` for how they're surfaced.
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
}

/// Emitted after an already-streamed assistant message has been
/// re-synthesised with user-supplied content (see
/// `Runtime::maybe_collaborate`). The UI uses `message_id` to find
/// the existing bubble and replace its content in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRefinedPayload {
    pub conversation_id: String,
    pub message_id: String,
    pub new_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepInput {
    pub step_id: usize,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepOutput {
    Text(String),
    Json(serde_json::Value),
    Jump(usize),
    Skipped,
    ReasonWithToolsResult {
        text: String,
        search_log: Vec<SearchLogEntry>,
        iterations: usize,
        capped: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchLogEntry {
    pub iteration: usize,
    pub tool_id: ToolId,
    pub query: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepError {
    pub step_id: usize,
    pub message: String,
}

// ─── Task Types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub conversation_id: ConversationId,
    pub goal: String,
    pub plan: Plan,
    pub status: TaskStatus,
    pub completed_steps: Vec<(usize, StepOutput)>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Running,
    Paused,
    Completed,
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
    pub task_id: TaskId,
    pub step_id: usize,
    /// The tool whose side effect this attempt ran (empty for non-tool steps).
    pub tool_id: String,
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
    pub started_at: i64,
    /// `None` while `Started`; set when the row reaches `Completed`/`Failed`.
    pub ended_at: Option<i64>,
}

/// Lifecycle of a single [`StepExecution`]. `Started` is the danger state
/// on resume; `Completed` / `Failed` are terminal and replay-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionStatus {
    Started,
    Completed,
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
    #[default]
    Raw,
    Summary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub source: String,
    pub confidence: f64,
    pub created_at: i64,
    pub last_used: i64,
    #[serde(default)]
    pub version: i64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingCorrection {
    pub message_hash: String,
    pub classified_as: String,
    pub was_correct: bool,
    pub created_at: i64,
}

// ─── Document / RAG Types ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum SourceType {
    #[default]
    UserDocument,
    Corpus {
        corpus_id: String,
    },
    WebSearch {
        url: String,
    },
}

impl SourceType {
    pub fn to_db_columns(&self) -> (&'static str, Option<&str>) {
        match self {
            SourceType::UserDocument => ("user", None),
            SourceType::Corpus { corpus_id } => ("corpus", Some(corpus_id.as_str())),
            SourceType::WebSearch { .. } => ("web", None),
        }
    }

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum SearchMethod {
    LocalOnly,
    LocalPlusWeb { reason: String },
    LocalOnlyIncomplete { reason: String },
    WebOnly { reason: String },
    NoResults { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CoverageDecision {
    Sufficient,
    SupplementWithWeb { reason: String },
    RequiresWeb { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum SourceOrigin {
    Local {
        corpus: String,
        article_title: String,
    },
    Web {
        url: String,
        domain: String,
    },
    UserDocument {
        filename: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchBudget {
    pub backend: String,
    pub monthly_limit: u32,
    pub used_this_month: u32,
    pub reset_date: i64,
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
    Private { owner: String },
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusState {
    pub corpus_id: String,
    pub installed_at: i64,
    pub source_date: String,
    pub chunks_count: i64,
    pub index_size_mb: i64,
    pub last_updated: i64,
    #[serde(default)]
    pub version: i64,
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

#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: DocumentChunk,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    pub id: String,
    pub source: String,
    pub content: String,
    pub chunk_index: usize,
    pub embedding: Option<Vec<f32>>,
    pub created_at: i64,
    #[serde(default)]
    pub source_type: SourceType,
    #[serde(default)]
    pub version: i64,
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
    pub id: String,
    pub conversation_id: String,
    pub filename: String,
    /// Matches `DocumentChunk.source` — the key for chunk retrieval.
    pub source: String,
    pub word_count: usize,
    pub chunk_count: usize,
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
    pub description: String,
    pub output: String,
    pub completed_at: i64,
}

// ─── Execution Intelligence Types ─────────────────────────────

/// Retry configuration for tool execution on transient failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries: usize,
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
    pub n: usize,
    pub selector: SampleSelector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SampleSelector {
    /// Fast model reads all candidates and selects the best.
    /// `selection_prompt` overrides everything when set; otherwise
    /// `preset` determines the rubric (defaults to general
    /// accuracy + completeness when also unset).
    LlmJudge {
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
    Verify { tool_id: ToolId },
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
    pub eval_prompt: String,
    #[serde(default = "default_eval_retries")]
    pub max_retries: usize,
    #[serde(default)]
    pub eval_speed: Speed,
}

pub(crate) fn default_eval_retries() -> usize {
    1
}

/// Difficulty estimate for adaptive test-time compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepDifficulty {
    Routine,
    Moderate,
    Hard,
}

/// Compute budget derived from difficulty estimation.
#[derive(Debug, Clone)]
pub struct ComputeBudget {
    pub max_tokens: usize,
    pub sampling: Option<SamplingConfig>,
    pub evaluation: Option<EvaluationConfig>,
    pub speed_override: Option<Speed>,
}

// ─── Response Types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub message: Message,
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

mod ui;
pub use ui::*;
mod document;
pub use document::*;

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
