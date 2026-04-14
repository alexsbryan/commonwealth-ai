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
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_tokens: 2048,
            think_budget: 512,
            top_k: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    pub tokens_used: usize,
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

// ─── Routing Types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Intent {
    SimpleQuery,
    DeepQuery,
    KnowledgeQuery,
    SimpleAction { tool: ToolId },
    ComplexTask,
    Continuation { task_id: TaskId },
}

// ─── Tool Types ────────────────────────────────────────────────

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
    LlmJudge {
        #[serde(default)]
        selection_prompt: Option<String>,
    },
    /// Take the most common first-line answer.
    MajorityVote,
    /// Run each candidate through a tool; first to pass wins.
    Verify { tool_id: ToolId },
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
}

// ─── Response Provenance ──────────────────────────────────────

/// Returned by `Router::classify()`. Carries the final intent alongside
/// the diagnostic routing detail that was previously only written to
/// `routing_log` and invisible in the UI.
#[derive(Debug, Clone)]
pub struct RoutingOutcome {
    pub intent: Intent,
    /// Raw coarse-classification label: "SIMPLE", "LOOKUP", "REASONING", "ACTION".
    pub coarse_intent: Option<String>,
    /// Self-assessment result — populated only on SIMPLE paths that went
    /// through the gate: "Confident", "Uncertain", "NeedsWebSearch".
    pub self_assessment: Option<String>,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSummary {
    pub origin: String,
    pub count: usize,
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
