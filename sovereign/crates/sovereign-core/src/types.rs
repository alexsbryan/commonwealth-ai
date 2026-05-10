use serde::{Deserialize, Serialize};

use crate::oicp;

// ─── Identity Types ────────────────────────────────────────────

pub type ToolId = String;
pub type TaskId = String;
pub type ConversationId = String;
pub type MessageId = String;

// ─── Inference Types ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Speed {
    #[default]
    Fast,
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
}

fn default_auto_collaborate() -> bool {
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
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub prompt: String,
    pub system_message: Option<String>,
    pub preferred_speed: Speed,
    pub max_tokens: Option<usize>,
    pub temperature: Option<f32>,
    pub structured_output: Option<serde_json::Value>,
    /// Overrides the default think-block token budget for this request.
    /// `None` falls back to the value in `InferenceConfig` (or the
    /// compiled-in `THINK_BUDGET` constant if unavailable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_budget: Option<usize>,
    /// Override the family-default top-k sampling parameter.
    /// `None` falls back to `ModelQuirks::default_top_k`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Override the family-default top-p (nucleus) sampling parameter.
    /// `None` falls back to `ModelQuirks::default_top_p`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// OICP capability requirements. Used by providers that support
    /// OICP to select the best model. Ignored by providers that don't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oicp: Option<oicp::InferenceRequirements>,
    /// Tool schemas the model may call. Present only when the caller is
    /// an agent driver (opencode, Claude Code via MCP) using
    /// OpenAI-compatible function-calling.
    ///
    /// Invariant: when `tools.is_some()`, `preferred_speed` must be
    /// `Slow` or `Medium` — Fast-slot models (Qwen3-1.7B in the current
    /// stack) do not have tools-aware chat templates. The slot router
    /// returns [`Error::InvalidInput`] for Fast + tools requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    /// Tool-choice hint (`"auto"` | `"none"` | `"required"` |
    /// `{"type":"function","function":{"name":"..."}}`). Raw JSON so
    /// forward-compatible shapes pass through untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Operator-declared model identifier. When set, the inference
    /// provider should route to the named slot if one matches; when
    /// `None`, the provider falls back to its default routing
    /// (Speed-based for `EmbeddedLlamaCpp`, OICP scoring for the
    /// mesh provider). The id is the model name as advertised on
    /// `/v1/models` — the gguf file stem.
    ///
    /// Threaded through from the OpenAI `model` field by
    /// `sovereign_mesh::inference_adapter::build_completion_request`,
    /// and from `ChatPrompt::phase_id` resolved against
    /// `EnrichConfig.chat_models` by the enrich-side inference
    /// client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Per-request override for the chat template's `enable_thinking`
    /// kwarg. Affects thinking-mode model families (Qwen3.x, …) where
    /// the Jinja template prepends a `<think>` block when this is
    /// `true` and skips it when `false`.
    ///
    /// Default (`None`) preserves the historical embedded-path
    /// behaviour: thinking is OFF, which on a heavily thinking-trained
    /// model means the planning text leaks as plain assistant prose
    /// (no `<think>` wrapper, but the verbosity remains). Setting
    /// this to `Some(true)` for a witness-register call lets the
    /// template wrap the planning trace formally so callers (or the
    /// `strip_think_blocks` post-process) can drop it cleanly and
    /// surface the post-`</think>` reply.
    ///
    /// Wire path: serialized into the OpenAI request body as
    /// `chat_template_kwargs: { enable_thinking: <bool> }` by
    /// `RemoteApiProvider::build_request`; parsed back out by the
    /// daemon's `inference_adapter::extract_enable_thinking` and
    /// applied at `embedded::apply_chat_template_oaicompat` time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
}

/// JSON-Schema view of a function the model may call. Mirrors
/// `commonwealth_api::openai_types::ToolFunction` but lives in the
/// provider-neutral core so `InferenceProvider` implementations don't
/// depend on the Commonwealth API crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

impl CompletionRequest {
    pub fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
            system_message: None,
            preferred_speed: Speed::Medium,
            max_tokens: None,
            temperature: None,
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
        }
    }

    pub fn with_speed(mut self, speed: Speed) -> Self {
        self.preferred_speed = speed;
        self
    }

    pub fn with_system(mut self, system: &str) -> Self {
        self.system_message = Some(system.to_string());
        self
    }

    pub fn with_oicp(mut self, requirements: oicp::InferenceRequirements) -> Self {
        self.oicp = Some(requirements);
        self
    }

    /// Tag this request with an explicit model id. The provider
    /// should route to the matching slot when one is loaded; when no
    /// match exists, the provider's default routing applies.
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    pub fn yes_no(condition: &str, context: &str) -> Self {
        Self {
            prompt: format!(
                "Given the following context:\n{context}\n\n\
                 Answer this yes/no question with only \"yes\" or \"no\":\n{condition}"
            ),
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(5),
            temperature: Some(0.0),
            structured_output: None,
            think_budget: Some(0), // No thinking needed for yes/no
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    /// Total tokens (prompt + completion). Kept as the historical
    /// "single number" telemetry stat. For the OpenAI Usage split,
    /// callers use `prompt_tokens` and derive completion as
    /// `tokens_used - prompt_tokens`.
    pub tokens_used: usize,
    /// Number of tokens in the formatted prompt (chat template +
    /// user content). Defaults to 0 for providers that don't track
    /// the split — the adapter then falls back to `tokens_used` in
    /// the legacy single-number form.
    #[serde(default)]
    pub prompt_tokens: usize,
    pub model_id: String,
    pub latency_ms: u64,
    /// OICP metadata from the provider, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oicp_meta: Option<oicp::OicpResponseMeta>,
}

impl CompletionResponse {
    pub fn as_bool(&self) -> bool {
        let lower = self.text.trim().to_lowercase();
        lower.starts_with("yes") || lower.starts_with("true")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub max_context_tokens: usize,
    pub supports_structured_output: bool,
    pub relative_speed: Speed,
    pub relative_reasoning: Depth,
}

// ─── Stream framing (typed finish_reason) ──────────────────────
//
// The OpenAI streaming surface ends each stream with a chunk
// carrying `finish_reason` ∈ {"stop","length","tool_calls",
// "content_filter"}. The legacy `Stream<Item = Result<String>>`
// shape on `InferenceProvider::complete_stream` could not carry
// that signal — every truncation looked identical to a clean stop
// at the wire, so the desktop saw a clipped reply with no way to
// tell whether the model actually finished or hit its budget.
//
// `complete_stream_with_finish` (new on `InferenceProvider`,
// optional override) yields `StreamFrame` instead, with a
// terminal `Finish { reason, usage }` frame. Providers that want
// accurate semantics override; the default impl wraps the legacy
// stream and synthesises `Stop`.

/// Why a stream stopped emitting tokens. Mirrors the OpenAI
/// `finish_reason` enum plus a `Cancelled` variant for our
/// CancellationToken / receiver-drop paths and a free-form
/// `Error` variant for provider-side faults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    /// Model emitted EOS (or an equivalent end-of-generation token).
    Stop,
    /// Generation hit `max_tokens`.
    Length,
    /// Model produced a tool-call payload. Reserved — streaming +
    /// tools is not currently supported on this surface, but the
    /// variant exists so callers don't have to guess.
    ToolCalls,
    /// Content filter or safety layer blocked the output.
    ContentFilter,
    /// Cancellation token tripped or the receiver was dropped.
    Cancelled,
    /// Provider-side error mid-stream. The string is the
    /// human-readable cause; the SSE bridge maps this to a wire
    /// `finish_reason: "error"`.
    Error(String),
}

impl FinishReason {
    /// OpenAI-compatible string for the SSE `finish_reason` field.
    /// `Cancelled` and `Error` are not OpenAI-native; we surface
    /// `"cancelled"` and `"error"` so clients that need to
    /// distinguish them can, while OpenAI-strict clients can treat
    /// any non-`"stop"` value as truncation.
    pub const fn as_openai_str(&self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::ContentFilter => "content_filter",
            FinishReason::Cancelled => "cancelled",
            FinishReason::Error(_) => "error",
        }
    }
}

/// Token-usage counters carried on the terminal stream frame.
/// Mirrors the OpenAI `usage` object so the SSE bridge can emit a
/// matching final chunk without a second source of truth.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// One frame on a typed completion stream. Replaces the legacy
/// `Result<String, Error>` items: `Token` carries a partial text
/// delta, `Finish` is the terminal frame with the reason we
/// stopped, and `Error` lets a provider abort mid-stream without
/// ambiguity. Streams MUST end with either `Finish` or `Error`;
/// receivers treat a closed channel without a terminal frame as
/// `Cancelled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamFrame {
    Token(String),
    Finish {
        reason: FinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<StreamUsage>,
    },
    Error(String),
}

// ─── Routing Types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    SimpleAction { tool: ToolId },
    ComplexTask,
    Continuation { task_id: TaskId },
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
pub enum TrustLevel {
    CommunityReviewed,
    AuthorSigned,
    Unsigned,
}

impl Default for TrustLevel {
    fn default() -> Self {
        TrustLevel::Unsigned
    }
}

/// Compute trust level from signature fields.
pub fn compute_trust_level(
    signature: &Option<String>,
    signed_by: &Option<String>,
) -> TrustLevel {
    match (signature, signed_by) {
        (Some(_), Some(s)) if s == "sovereign-community" => TrustLevel::CommunityReviewed,
        (Some(_), _) => TrustLevel::AuthorSigned,
        _ => TrustLevel::Unsigned,
    }
}

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

// ─── Memory Types ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub enum SourceType {
    UserDocument,
    Corpus { corpus_id: String },
    WebSearch { url: String },
}

impl Default for SourceType {
    fn default() -> Self {
        SourceType::UserDocument
    }
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
            "web" => SourceType::WebSearch {
                url: String::new(),
            },
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
    Local { corpus: String, article_title: String },
    Web { url: String, domain: String },
    UserDocument { filename: String },
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

fn default_eval_retries() -> usize {
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

/// A single (intent, confidence) candidate. The classifier emits one
/// primary plus up to a few alternatives.
#[derive(Debug, Clone)]
pub struct IntentCandidate {
    pub intent: Intent,
    /// Confidence in [0.0, 1.0]. A pre-checked heuristic (topic
    /// continuity, content processing, temporal signal) pins this to
    /// 1.0; an LLM Pass 1 returns whatever the model asserts.
    pub confidence: f32,
}

/// Returned by `Router::classify()`. Carries the primary intent, any
/// alternatives the classifier surfaced, and the diagnostic fields
/// that were previously squirreled away in `routing_log` and
/// invisible in the UI.
///
/// `alternatives` is empty in PR1 — the field is reserved for PR2 when
/// the `Ask` move uses a cheap keyword heuristic to suggest clickable
/// disambiguations. Keeping the field here (instead of building it in
/// the runtime) lets future classifiers populate it without a second
/// trait change.
#[derive(Debug, Clone)]
pub struct RouterClassification {
    pub primary: IntentCandidate,
    pub alternatives: Vec<IntentCandidate>,
    /// One-clause justification from the classifier, when available.
    /// Surfaced in the UI for glassbox integrity (ARCH §0.1). `None`
    /// when the classifier is a pre-check or a stub.
    pub rationale: Option<String>,
    /// Raw coarse-classification label: "SIMPLE", "LOOKUP",
    /// "REASONING", "ACTION", or "TOPIC_CONTINUITY" for the override.
    pub coarse_intent: Option<String>,
    /// Self-assessment result — populated only on SIMPLE paths that
    /// went through the gate: "Confident", "Uncertain",
    /// "NeedsWebSearch".
    pub self_assessment: Option<String>,
    /// Iter6: per-stage routing breakdown for performance
    /// instrumentation. None on pure-stub classifiers; populated by
    /// the LLM-backed router so the runtime can roll the slice into
    /// the response metrics.
    pub timing: Option<RoutingTiming>,
}

/// Iter6: per-call routing latency slice. Surfaces the cost of the
/// pre-check chain vs the LLM Pass 1 vs the parse step so the
/// 14% / 6s routing slice from the iter5 waterfall can be
/// diagnosed concretely.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingTiming {
    /// Wall-clock time spent walking the heuristic pre-check chain
    /// (force_conation, force_action, force_metalingual,
    /// force_commissive, force_comparison, force_expressive_short,
    /// force_expressive_memref, force_content_reasoning, force_deep).
    /// Sub-millisecond when none fire; instant when any short-circuits
    /// because we stop walking once the first fires.
    pub precheck_ms: u64,
    /// LLM Pass 1 call time (`classify_call_json`). Zero when a
    /// pre-check fired and the LLM call was skipped.
    pub llm_ms: u64,
    /// `parse_coarse` step. Should be sub-millisecond — included for
    /// completeness so the three slices sum to the router's total.
    pub parse_ms: u64,
    /// Whether the LLM Pass 1 actually fired. False = a pre-check
    /// short-circuited; True = `classify_call_json` ran.
    pub used_llm: bool,
}

/// Which of the three antifragile moves the runtime should take.
///
/// - `Commit`: proceed directly. No banner, no prompt. Default.
/// - `Propose`: stream a response AND show the interpretation banner
///   so the user can cheaply redirect. PR2 wires the UI; PR1 never
///   returns this variant.
/// - `Ask`: suppress synthesis and surface a clarification card. PR2
///   wires the UI; PR1 never returns this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveKind {
    Commit,
    Propose,
    Ask,
}

/// Bucketed confidence tier. Derived from `primary.confidence` and
/// the active `ConfidenceThresholds`. Kept as an enum (ARCH §2.1) so
/// downstream glassbox rendering is stringly-typed only at the
/// serialization boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceTier {
    High,
    Moderate,
    Low,
}

/// Thresholds consulted by `decide_policy`. Defaults err toward
/// committing so first-time users see a responsive system; the
/// "Propose" move activates in the moderate band where the
/// interpretation banner adds value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceThresholds {
    /// confidence ≥ high  → `ConfidenceTier::High`  / `MoveKind::Commit`
    pub high: f32,
    /// high > confidence ≥ moderate → `ConfidenceTier::Moderate` / `MoveKind::Propose`
    /// moderate > confidence        → `ConfidenceTier::Low`      / `MoveKind::Ask`
    pub moderate: f32,
}

impl Default for ConfidenceThresholds {
    fn default() -> Self {
        Self {
            high: 0.80,
            moderate: 0.55,
        }
    }
}

/// Runtime-side policy: what we're actually going to do with the
/// classifier's opinion. Pure function of `RouterClassification` +
/// `ConfidenceThresholds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub move_kind: MoveKind,
    pub tier: ConfidenceTier,
    /// Snapshot of the thresholds used to produce this decision.
    /// Surfaced in glassbox metadata so users and the operator log
    /// can see why the router picked what it picked (ARCH §0.1).
    pub thresholds_used: ConfidenceThresholds,
}

/// Which substantive phase a narration entry marks. Serialized to
/// the UI so narration chips can carry an icon per phase (retrieval,
/// routing, synthesis, etc.). Extend additively; the UI should
/// fallback gracefully for unknown variants (via `#[serde(other)]`
/// on the consuming side).
///
/// Two families coexist:
///
/// - **Legacy single-stage variants** (`RoutingCommitted`,
///   `PrimarySynthesisStart`, `GapCheckFired`, `RetrievalComplete`)
///   were emitted by the pre-team-pipeline dispatch path. They are
///   kept so existing callers and tests work unchanged.
/// - **Team-pipeline stage frames** (`RoutingStart` /
///   `RoutingComplete`, `RetrievalStart`, `CurationStart` /
///   `CurationComplete`, `DraftingStart` / `DraftingComplete`,
///   `PresentationStart` / `PresentationComplete`, `StageError`)
///   are emitted by the five-stage pipeline introduced by the
///   situated-team plan. The desktop renders each as an inline
///   chip; payloads label the chip ("Curated 5 of 18 chunks").
///
/// `RetrievalComplete` was migrated from a unit variant to a struct
/// variant — emit sites must now supply `chunks_in` and `corpora`.
/// The Copy derive was dropped because struct variants with `String`
/// / `Vec` payloads cannot be Copy; all known consumers move or
/// clone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NarrationPhase {
    // ── Legacy (single-stage) variants ────────────────────────
    /// Routing committed, substantive work about to begin.
    RoutingCommitted,
    /// Primary-slot synthesis beginning (Slow path).
    PrimarySynthesisStart,
    /// Gap-check fired and found a missing piece.
    GapCheckFired,

    // ── Team-pipeline stage frames ────────────────────────────
    /// Router invocation began. Pairs with `RoutingComplete`.
    RoutingStart,
    /// Router classified the turn. Carries the verdict so the
    /// desktop can label the stage chip.
    RoutingComplete {
        intent: String,
        register: String,
        confidence: f32,
    },
    /// Retriever began (vector + FTS + atlas).
    RetrievalStart,
    /// Retrieval finished. Carries shape so the chip can read
    /// e.g. "Read 12 chunks across [sep, wikipedia]". Migrated
    /// from the legacy unit variant; on the wire this is a struct
    /// variant under `#[serde(rename_all = "snake_case")]`.
    RetrievalComplete {
        chunks_in: usize,
        corpora: Vec<String>,
    },
    /// Curator began (Fast slot, structured output).
    CurationStart,
    /// Curator finished. `chunks_kept` is the number that
    /// survived curation; `skeleton` is the ordered list of
    /// section labels the Drafter will fill; `sufficient` is the
    /// glass-box honesty signal — `false` short-circuits the
    /// Drafter and routes the Presenter to an honest "I don't
    /// have grounding for this" message.
    CurationComplete {
        chunks_kept: usize,
        skeleton: Vec<String>,
        sufficient: bool,
    },
    /// Drafter began (Primary slot).
    DraftingStart,
    /// Drafter finished. `tokens` is `completion_tokens`;
    /// `finish_reason` is the OpenAI-style `stop` / `length` /
    /// `cancelled` / `error`, sourced from the typed
    /// `StreamFrame::Finish` introduced in the Phase 1.1 plumbing.
    DraftingComplete {
        tokens: u32,
        finish_reason: String,
    },
    /// Presenter began (Fast slot, voice-shaping pass).
    PresentationStart,
    /// Presenter finished. `judge_score` is the optional
    /// post-presentation voice-judge score (None when register
    /// is Factual or the judge is disabled). Arrives on a
    /// delayed narration frame from the async judge task.
    PresentationComplete {
        judge_score: Option<u8>,
    },
    /// Any stage emitted an error. The pipeline records this for
    /// telemetry; user-facing messaging is decided per stage.
    StageError {
        stage: String,
        error: String,
    },
}

/// One narration entry emitted in the model's voice during a long
/// turn. Accumulated in `QuerySession.narration` and streamed to the
/// UI as `turn-narration` Tauri events. PR2 emits these at
/// phase-boundary points; suppression < 5s total elapsed and a
/// 3-event cap keep the channel from polluting short turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrationEvent {
    pub phase: NarrationPhase,
    pub text: String,
    /// Milliseconds since turn start. Drives UI timeline rendering.
    pub elapsed_ms: u64,
}

// ─── Antifragile-routing UI event payloads ───────────────────

/// Emitted by the runtime when `decide_policy` picks `MoveKind::Propose`.
/// The UI renders an inline banner above the streaming message with
/// the `interpretation` text plus `alternatives` as redirect chips.
/// The banner persists through the turn; redirect stays cheap while
/// tokens are flowing (sampler cancels) and remains valid afterward
/// (full session retained for 30s).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationProposed {
    pub session_id: String,
    pub conversation_id: String,
    /// One-sentence statement of how the router read the input, e.g.
    /// "I'm reading this as a quick overview of the scheduler."
    pub interpretation: String,
    /// Ranked candidate interpretations the user can click to
    /// redirect. Drawn from `RouterClassification.alternatives`.
    pub alternatives: Vec<ProposedAlternative>,
    /// Confidence number for glassbox rendering (ARCH §0.1).
    pub confidence: f32,
}

/// One redirect option on an `InterpretationProposed` banner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAlternative {
    /// UI-facing label, e.g. "Walk me through the scoring function".
    pub label: String,
    /// Serialized `Intent` variant ("deep_query", "knowledge_query",
    /// etc.) the runtime will route to on redirect. Using a string
    /// rather than the full `Intent` enum here keeps the desktop
    /// payload simple; the runtime re-resolves on `redirect_turn`.
    pub intent_hint: String,
}

/// Emitted by the runtime when `decide_policy` picks `MoveKind::Ask`.
/// The UI renders a ClarificationCard with `options` as clickable
/// chips plus a free-text fallback. Synthesis is suppressed —
/// nothing streams until the user responds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationRequest {
    pub session_id: String,
    pub conversation_id: String,
    /// The question to show above the options, e.g. "I can approach
    /// this a few ways — are you trying to understand how it works,
    /// design changes to it, or debug it?"
    pub question: String,
    pub options: Vec<ClarificationOption>,
}

/// One clickable disambiguation on a `ClarificationRequest` card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationOption {
    pub label: String,
    /// The follow-up message that will be sent back as if the user
    /// had typed it. The runtime correlates to the session via a
    /// session_ref and skips routing.
    pub follow_up: String,
    pub intent_hint: String,
}

/// Emitted by the runtime at phase-boundary points on long turns.
/// Rendered as inline model-voice chips in the UI (see
/// `NarrationChip.svelte`). Capped at 3 per turn; suppressed when
/// turn elapsed < 5s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnNarration {
    pub session_id: String,
    pub conversation_id: String,
    pub event: NarrationEvent,
}

/// Wire parameters carrying a "continue this earlier turn" request
/// from the UI back into the runtime. Produced when the user clicks
/// a ClarificationCard option or a NextStepOffer button.
///
/// The runtime uses this to:
///   - skip router classification (the intent was already picked),
///   - correlate with the prior `QuerySession` (PR2c will also reuse
///     the cached retrieval from that session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeSession {
    pub session_id: String,
    /// Wire-form `Intent` hint produced by `intent_hint()` in the
    /// runtime. Parsed back via `parse_intent_hint`. Unknown or
    /// malformed hints fall back to `Intent::SimpleQuery` so the
    /// session-continuation path never hard-fails from a typo.
    pub intent_hint: String,
}

// ─── Next-step offers (PR3) ──────────────────────────────────
//
// After a substantive KnowledgeQuery turn finishes, the runtime
// surfaces up to two grounded follow-up actions the user can click.
// Offers are:
//
//   1. *grounded* — derived from what retrieval actually found (not
//      a generic "anything else?" prompt), and
//   2. *cheap* — when `session_ref` is live (<30s from completion),
//      clicking reuses the session via `resume_session_stream` and
//      skips router classification.
//
// SimpleQuery / DeepQuery don't emit offers today — they have no
// retrieval grounding to draw on. Extend here if future intents
// produce meaningful follow-ups.

/// One clickable next-step chip on a completed assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextStepOffer {
    /// UI button text. Short, action-shaped: "Tell me about X",
    /// "Compare other perspectives", "Go deeper on Y".
    pub label: String,
    /// Optional subtle hint rendered as a tooltip or below-label
    /// caption. Good place for "from <source_title>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The query that actually gets submitted if clicked. Usually
    /// a rephrased version of the offer, ready for synthesis.
    pub follow_up_query: String,
    /// Live `QuerySession.id` the runtime should resume against. The
    /// session's 30s retention window means a click more than 30s
    /// after render silently falls back to a fresh turn (runtime
    /// will return `session not found` → the UI must gracefully
    /// degrade to `send_message_stream`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    /// Wire-form `Intent` hint for the resume path (see
    /// `ResumeSession.intent_hint`). When `None`, the follow-up is
    /// treated as a fresh message that re-runs classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_hint: Option<String>,
}

/// Input to the offer generator. Decouples the pure function from
/// the specifics of the streaming pipeline's internal types so the
/// generator is trivially unit-testable.
#[derive(Debug, Clone)]
pub struct OfferContext<'a> {
    /// The user's original message — used to phrase a drill-down
    /// follow-up ("Tell me more about X in the context of this
    /// question").
    pub user_message: &'a str,
    /// Title of the source chunk that most shaped the answer. Used
    /// to offer "Compare other perspectives" when this source
    /// dominated retrieval.
    pub top_source_title: Option<&'a str>,
    /// Did the answer concentrate on one source? (Shape's
    /// `top_source_repeat_count >= 2`.) Governs whether the
    /// "compare perspectives" offer is worth surfacing.
    pub had_dominant_source: bool,
    /// Retrieved chunks in score order. The generator picks the
    /// highest-scoring one whose title differs from
    /// `top_source_title` as a drill-down target.
    pub retrieved_chunks: &'a [serde_json::Value],
    /// Live session id the UI should pass on click to take the
    /// cheap resume-session path.
    pub session_id: &'a str,
    /// PR5 — was the underlying retrieval off-target (dispersed
    /// noise, no title match, no source concentration)? When true,
    /// the generator returns zero offers: drilling down into
    /// irrelevant retrieval doubles down on the miss. Source of
    /// truth is `EvidenceShape::is_off_target()` in the runtime.
    pub retrieval_missed: bool,
}

/// Produce up to two grounded next-step offers from a completed
/// KnowledgeQuery turn. Pure — no I/O.
///
/// Offer priority:
///   1. Drill-down into the highest-scoring non-dominant source
///      (when one exists).
///   2. "Compare other perspectives" (when the answer concentrated
///      on a single dominant source).
pub fn build_next_step_offers(ctx: &OfferContext<'_>) -> Vec<NextStepOffer> {
    // PR5 — suppress offers entirely when retrieval was off-target.
    // Drilling into "Cartoon Reel" after asking about "Commonwealth
    // scheduler" doubles down on noise; better to surface nothing
    // than to surface misdirecting chips.
    if ctx.retrieval_missed {
        return Vec::new();
    }

    let mut offers = Vec::new();

    // Drill-down: find the first retrieved chunk whose title is
    // meaningfully different from the dominant one. Skip entries
    // without titles (conversation-history chunks, etc.).
    if let Some(secondary_title) = ctx.retrieved_chunks.iter().find_map(|c| {
        let title = c.get("title")?.as_str()?;
        if title.is_empty() {
            return None;
        }
        if let Some(dominant) = ctx.top_source_title {
            if title.eq_ignore_ascii_case(dominant) {
                return None;
            }
        }
        Some(title.to_string())
    }) {
        offers.push(NextStepOffer {
            label: format!("Tell me about \"{secondary_title}\""),
            description: Some("Drawn from your retrieval".to_string()),
            follow_up_query: format!(
                "Tell me what \"{secondary_title}\" says about this."
            ),
            session_ref: Some(ctx.session_id.to_string()),
            intent_hint: Some("knowledge_query".to_string()),
        });
    }

    // Dominant-source → offer a comparative read.
    if ctx.had_dominant_source {
        if let Some(dominant) = ctx.top_source_title {
            let dominant_trunc = if dominant.len() > 40 {
                format!("{}…", &dominant[..40])
            } else {
                dominant.to_string()
            };
            offers.push(NextStepOffer {
                label: "Compare other perspectives".to_string(),
                description: Some(format!(
                    "Your answer leaned on \"{dominant_trunc}\" — pull in more sources."
                )),
                follow_up_query: format!(
                    "{} — what do other sources in my knowledge base say, besides \"{dominant}\"?",
                    ctx.user_message.trim()
                ),
                session_ref: Some(ctx.session_id.to_string()),
                intent_hint: Some("knowledge_query".to_string()),
            });
        }
    }

    // Cap at 2. If a future trigger produces a third, we want a
    // hard limit — three buttons under every answer is clutter.
    offers.truncate(2);
    offers
}

/// Map classification confidence to a concrete (tier, move_kind)
/// decision. Pure — no I/O, no awaits, no model calls. PR1 only
/// ever reaches the `Commit` branch in the runtime dispatcher; the
/// other branches are precomputed here so PR2 can wire them without
/// a second types-layer change.
pub fn decide_policy(
    classification: &RouterClassification,
    thresholds: &ConfidenceThresholds,
) -> RoutingPolicy {
    let c = classification.primary.confidence;
    let (tier, move_kind) = if c >= thresholds.high {
        (ConfidenceTier::High, MoveKind::Commit)
    } else if c >= thresholds.moderate {
        (ConfidenceTier::Moderate, MoveKind::Propose)
    } else {
        (ConfidenceTier::Low, MoveKind::Ask)
    };
    RoutingPolicy {
        move_kind,
        tier,
        thresholds_used: *thresholds,
    }
}

// ─── Response Provenance ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseProvenance {
    pub intent: String,
    pub search_method: Option<String>,
    pub sources: Vec<SourceSummary>,
    pub inference_backend: String,
    pub oicp_match: Option<String>,
    pub total_latency_ms: u64,
    pub tokens_used: usize,
    /// Coarse router classification ("SIMPLE", "LOOKUP", "REASONING", "ACTION").
    /// `None` for old messages that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coarse_intent: Option<String>,
    /// Self-assessment gate result, set on SIMPLE paths only.
    /// `None` when not applicable or for old messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_assessment: Option<String>,
    /// Folder-ingest v1 §6.3: per-turn coverage assessment over the
    /// user's watched-folder corpora. `None` for turns where no
    /// folder corpus contributed retrieval (the common "talked to a
    /// public knowledge base" case). When `Some`, the chat surface
    /// renders a quiet chip enumerating thin folders so the user
    /// learns *what we don't have* without a second click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSummary {
    pub origin: String,
    pub count: usize,
    /// When set, this corpus's hits came from a mesh peer — the
    /// string is the peer's human-readable `node_name` (matching what
    /// the mesh UI shows). Rendered as `"sep (6) via BeefyMac"` by
    /// `RoutingMeta.svelte`. Locally-hosted corpora leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_peer: Option<String>,
    /// Folder-ingest v1 §6.3: when this corpus is a watched folder,
    /// the user-typed display name (e.g. "case files") that the
    /// chat surface renders instead of the opaque `corpus_id` slug.
    /// `None` for non-folder corpora (SEP, Wikipedia, mesh hits) so
    /// the UI keeps its existing label rendering for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Per-turn coverage assessment. `kind == "thin"` means at least one
/// folder corpus that contributed retrieval came back with fewer
/// than `thin_threshold` chunks — likely under-served by the user's
/// own materials. The chat surface renders a one-line chip listing
/// the thin folders so the user can decide whether to (a) accept
/// the result, (b) re-phrase, or (c) extend the folder's contents.
///
/// `kind == "ok"` is reserved for forward compatibility — today's
/// runtime simply omits the field (`coverage: None`) when coverage
/// is fine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageNote {
    pub kind: String,
    pub thin_threshold: usize,
    pub thin_folders: Vec<ThinFolder>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinFolder {
    pub corpus_id: String,
    pub display_name: String,
    pub chunks: usize,
    /// Files in this folder whose extension isn't in the watcher's
    /// accept list (e.g. `.pages`, `.key`). When non-zero, surfaces
    /// in the chip as ", N files in unsupported formats" so the
    /// user knows the gap is structural and not just retrieval-quality.
    pub skipped_files: usize,
    /// Files the watcher tried and failed to extract (encrypted,
    /// corrupt, etc). Surfaced same as `skipped_files`.
    pub failed_files: usize,
}

// ─── Action Preview (for approval) ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPreview {
    pub tool_id: ToolId,
    pub description: String,
    pub params: serde_json::Value,
}

// ─── Insight Types ────────────────────────────────────────────

/// A captured insight node — the output of a clip action.
/// Created when the user clips a paragraph from a conversation response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightNode {
    pub id: uuid::Uuid,
    /// The clipped paragraph text (verbatim).
    pub clipped_text: String,
    /// The conversation message this was clipped from.
    pub message_id: uuid::Uuid,
    /// The paragraph index within the message (for re-highlighting on revisit).
    pub paragraph_index: usize,
    /// Provenance: corpus and article.
    pub source: InsightSource,
    /// Field model position, if the paragraph carried position attribution.
    pub position: Option<InsightPosition>,
    /// System-inferred adjacent concepts (from embedding similarity).
    pub adjacent: Vec<String>,
    /// Embedding of the clipped text (for semantic search across the collection).
    pub embedding: Option<Vec<f32>>,
    /// When the clip was made.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Sink state: where this node lives / has been synced.
    pub sink_state: InsightSinkState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightSource {
    pub corpus_id: Option<String>,
    pub article_title: Option<String>,
    pub conversation_id: uuid::Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightPosition {
    pub name: String,
    pub style: PositionStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PositionStyle {
    Compatibilism,
    HardIncompatibilism,
    Libertarianism,
    /// For future field model positions not in the pre-defined set.
    /// Rendered with a neutral gray badge.
    Custom {
        bg: String,
        text: String,
        border: String,
    },
}

/// Where an insight currently lives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InsightSinkState {
    /// Stored in Sovereign's native SQLite insight store only.
    Local,
    /// Pending sync to a configured external sink (e.g. Obsidian vault).
    PendingSync,
    /// Successfully synced to an external sink.
    Synced {
        sink_id: String,
        synced_at: chrono::DateTime<chrono::Utc>,
    },
    /// Sync attempted but failed.
    SyncFailed {
        sink_id: String,
        error: String,
    },
}

// ─── Document Asset Types ─────────────────────────────────────
//
// A persistent document that has been ingested once and can be
// queried many times. Lives in the document library alongside
// corpora. The ingest cost is paid once; subsequent queries are
// fast because the embedding index and structural skeleton are
// already built.

/// A document that has been uploaded, parsed, embedded, and
/// structurally analysed. Created by `DocumentAssetManager::ingest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentAsset {
    pub id: String,
    pub title: String,
    pub filename: String,
    pub file_size_mb: f32,
    pub word_count: usize,
    pub chunk_count: usize,
    pub document_type: DocumentTypeTag,
    pub ingested_at: chrono::DateTime<chrono::Utc>,
    /// LanceDB index ID for this document's embedded chunks.
    pub index_id: String,
    /// Structural skeleton — built during ingest, stored permanently.
    /// None until the skeleton phase completes.
    pub skeleton: Option<DocumentSkeleton>,
    pub state: AssetState,
}

impl DocumentAsset {
    /// The source key used to look up this document's chunks in the
    /// `DocumentStore`. For assets ingested via `DocumentAssetManager`,
    /// this is `"asset:{id}"`. For legacy documents promoted from the
    /// old chunks table, this is the original file path stored in
    /// `index_id` (prefixed with `"legacy:"`).
    pub fn source_key(&self) -> String {
        if let Some(original) = self.index_id.strip_prefix("legacy:") {
            original.to_string()
        } else {
            format!("asset:{}", self.id)
        }
    }
}

/// Processing state of a document asset. Drives the UI's progress
/// display and determines which operations are available.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AssetState {
    /// File accepted. Processing not yet started.
    Pending,
    /// Embedding chunks into LanceDB. RAG not yet available.
    Indexing {
        chunks_done: usize,
        chunks_total: usize,
    },
    /// Embedding done. RAG available. Skeleton extraction running.
    /// Synthesis and coherent analysis available with degraded quality.
    PartiallyReady,
    /// Skeleton extraction in progress.
    BuildingSkeleton {
        chunks_done: usize,
        chunks_total: usize,
    },
    /// Fully ready. All operations available.
    Ready,
    /// Ingest failed.
    Failed { reason: String },
}

impl AssetState {
    /// True when the document has enough indexed data to answer
    /// RAG queries — embedding is complete even if the skeleton
    /// is still building.
    pub fn is_queryable(&self) -> bool {
        matches!(
            self,
            AssetState::PartiallyReady
                | AssetState::BuildingSkeleton { .. }
                | AssetState::Ready
        )
    }

    /// Short human-readable label for the UI.
    pub fn label(&self) -> &'static str {
        match self {
            AssetState::Pending => "Waiting",
            AssetState::Indexing { .. } => "Indexing",
            AssetState::PartiallyReady => "Partially ready",
            AssetState::BuildingSkeleton { .. } => "Building structure",
            AssetState::Ready => "Ready",
            AssetState::Failed { .. } => "Failed",
        }
    }

    /// Progress as a 0.0–1.0 fraction. Indexing is the first half,
    /// skeleton extraction is the second half.
    pub fn progress_fraction(&self) -> Option<f32> {
        match self {
            AssetState::Indexing {
                chunks_done,
                chunks_total,
            } if *chunks_total > 0 => Some(*chunks_done as f32 / *chunks_total as f32 * 0.5),
            AssetState::PartiallyReady => Some(0.5),
            AssetState::BuildingSkeleton {
                chunks_done,
                chunks_total,
            } if *chunks_total > 0 => {
                Some(0.5 + *chunks_done as f32 / *chunks_total as f32 * 0.5)
            }
            AssetState::Ready => Some(1.0),
            _ => None,
        }
    }
}

/// Coarse classification of a document's genre/type. Influences
/// which skeleton extraction prompts are used and which starter
/// chips are shown in the conversation view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DocumentTypeTag {
    /// Novels, memoirs, literary non-fiction.
    Narrative,
    /// Dissertations, essays, philosophy.
    Argument,
    /// Legal briefs, scientific papers.
    Evidence,
    /// History, biography, journalism.
    Chronicle,
    /// Manuals, specifications, documentation.
    Technical,
    /// Not yet classified or doesn't fit a category.
    Unknown,
}

impl DocumentTypeTag {
    pub fn label(&self) -> &'static str {
        match self {
            DocumentTypeTag::Narrative => "Narrative",
            DocumentTypeTag::Argument => "Argument",
            DocumentTypeTag::Evidence => "Evidence",
            DocumentTypeTag::Chronicle => "Chronicle",
            DocumentTypeTag::Technical => "Technical",
            DocumentTypeTag::Unknown => "Document",
        }
    }
}

impl Default for DocumentTypeTag {
    fn default() -> Self {
        Self::Unknown
    }
}

// ─── Document Skeleton ────────────────────────────────────────
//
// The structural skeleton is built by the ingest pipeline via
// batched LLM inference over the document's chunks. It enables
// synthesis (whole-document analysis) and entity-aware routing
// that plain RAG cannot do.

/// Structural skeleton of a document — entities, sections, and
/// key moments. Built once during ingest, stored permanently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSkeleton {
    /// Annotated sections with structural function labels.
    pub sections: Vec<SectionAnnotation>,
    /// Top entities ranked by presence across the document.
    pub main_entities: Vec<RankedEntity>,
    /// Entity name → chunk indices + representative quotes.
    pub entity_index: std::collections::HashMap<String, EntityAppearances>,
    /// Key turning points, revelations, or structural shifts.
    pub structural_moments: Vec<StructuralMoment>,
    /// One-paragraph overview used by the router to decide
    /// operation type without reading the full document.
    pub overview: String,
    pub built_at: chrono::DateTime<chrono::Utc>,
}

/// A chunk annotated with its structural role in the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionAnnotation {
    pub chunk_index: usize,
    pub function: SectionFunction,
    pub key_entities: Vec<String>,
    /// What this section establishes, advances, or resolves.
    pub establishes: String,
}

/// The narrative/argumentative role a section plays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SectionFunction {
    Introduces,
    Develops,
    Complicates,
    Resolves,
    Transitions,
    Evidences,
}

impl SectionFunction {
    pub fn label(&self) -> &'static str {
        match self {
            SectionFunction::Introduces => "Introduces",
            SectionFunction::Develops => "Develops",
            SectionFunction::Complicates => "Complicates",
            SectionFunction::Resolves => "Resolves",
            SectionFunction::Transitions => "Transitions",
            SectionFunction::Evidences => "Evidences",
        }
    }
}

/// An entity ranked by how prominently it appears in the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedEntity {
    pub name: String,
    pub kind: EntityKind,
    /// Fraction of sections where this entity appears (0.0–1.0).
    pub presence_rate: f32,
    /// First chunk index where this entity appears.
    pub first_appearance: usize,
    /// Last chunk index where this entity appears.
    pub last_appearance: usize,
}

/// Classification of an entity found in a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntityKind {
    Character,
    Argument,
    Concept,
    Claim,
    Evidence,
    Theme,
    Person,
    Event,
}

impl EntityKind {
    pub fn label(&self) -> &'static str {
        match self {
            EntityKind::Character => "Character",
            EntityKind::Argument => "Argument",
            EntityKind::Concept => "Concept",
            EntityKind::Claim => "Claim",
            EntityKind::Evidence => "Evidence",
            EntityKind::Theme => "Theme",
            EntityKind::Person => "Person",
            EntityKind::Event => "Event",
        }
    }
}

/// Where an entity appears in the document, with sample quotes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityAppearances {
    pub chunk_indices: Vec<usize>,
    /// Up to 3 representative quotes from the entity's appearances.
    pub quote_samples: Vec<String>,
}

/// A structurally significant moment in the document — a turning
/// point, key revelation, or major transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralMoment {
    pub chunk_index: usize,
    /// Short description: "Shevek departs Anarres", "Author
    /// concedes the counterargument".
    pub description: String,
    /// 0.0–1.0 importance score. Used to cap the skeleton at
    /// 15–40 moments for a full-length document.
    pub salience: f32,
}

// ─── Document Operations ──────────────────────────────────────
//
// The operation the router selected for a user's request. Stored
// alongside the response so the user can see how it was handled
// and so the UI can show the correct badge and explanation.

/// The operation type chosen by the document router for a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DocumentAssetOperation {
    /// Retrieved specific passages matching the query.
    Rag { query: String },
    /// Synthesised across the full document, tracing entities or
    /// themes through multiple sections.
    Synthesis {
        focus: String,
        entities: Vec<String>,
    },
    /// Searched every section for all instances of a pattern.
    Aggregation { query: String },
    /// Applied a transformation (edit, rewrite, extract).
    Transformation,
    /// The question had no clear connection to the attached document, so the
    /// system answered from general knowledge rather than retrieving passages.
    /// `reason` is a short phrase for the UI explanation ("unrelated domain",
    /// "retrieval found nothing", etc.).
    OffTopic { reason: String },
}

impl DocumentAssetOperation {
    /// Short label for the operation badge in the UI.
    pub fn label(&self) -> &'static str {
        match self {
            DocumentAssetOperation::Rag { .. } => "Retrieved passages",
            DocumentAssetOperation::Synthesis { .. } => "Synthesised across full document",
            DocumentAssetOperation::Aggregation { .. } => "Found all instances",
            DocumentAssetOperation::Transformation => "Applied transformation",
            DocumentAssetOperation::OffTopic { .. } => "Answered from general knowledge",
        }
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
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
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
        }
    }

    #[test]
    fn high_confidence_commits() {
        let policy = decide_policy(
            &classification(0.95),
            &ConfidenceThresholds::default(),
        );
        assert_eq!(policy.tier, ConfidenceTier::High);
        assert_eq!(policy.move_kind, MoveKind::Commit);
    }

    #[test]
    fn boundary_exactly_at_high_commits() {
        // 0.80 is inclusive of the High tier.
        let policy = decide_policy(
            &classification(0.80),
            &ConfidenceThresholds::default(),
        );
        assert_eq!(policy.tier, ConfidenceTier::High);
    }

    #[test]
    fn moderate_confidence_proposes() {
        let policy = decide_policy(
            &classification(0.65),
            &ConfidenceThresholds::default(),
        );
        assert_eq!(policy.tier, ConfidenceTier::Moderate);
        assert_eq!(policy.move_kind, MoveKind::Propose);
    }

    #[test]
    fn boundary_exactly_at_moderate_proposes() {
        // 0.55 is inclusive of the Moderate tier.
        let policy = decide_policy(
            &classification(0.55),
            &ConfidenceThresholds::default(),
        );
        assert_eq!(policy.tier, ConfidenceTier::Moderate);
    }

    #[test]
    fn low_confidence_asks() {
        let policy = decide_policy(
            &classification(0.30),
            &ConfidenceThresholds::default(),
        );
        assert_eq!(policy.tier, ConfidenceTier::Low);
        assert_eq!(policy.move_kind, MoveKind::Ask);
    }

    #[test]
    fn just_under_moderate_asks() {
        let policy = decide_policy(
            &classification(0.549),
            &ConfidenceThresholds::default(),
        );
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
        let policy = decide_policy(
            &classification(0.82),
            &ConfidenceThresholds::default(),
        );
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
        let chunks = vec![
            chunk("A"),
            chunk("B"),
            chunk("C"),
            chunk("D"),
        ];
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
        assert!(offers.is_empty(), "miss must suppress all offers: {offers:?}");
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
