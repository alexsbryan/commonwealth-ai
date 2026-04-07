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
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_tokens: 2048,
            think_budget: 512,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMemory {
    pub current_goal: Option<String>,
    pub facts: Vec<String>,
    pub active_documents: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseProvenance {
    pub intent: String,
    pub search_method: Option<String>,
    pub sources: Vec<SourceSummary>,
    pub inference_backend: String,
    pub oicp_match: Option<String>,
    pub total_latency_ms: u64,
    pub tokens_used: usize,
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
