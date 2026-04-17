Sovereign: Technical Design

_A purpose-built, self-hosted AI agent for everyone._

---

## Design Philosophy

Four principles govern every decision in this system.

**The model is a function, not the application.** LLMs are stateless reasoning engines called within a deterministic runtime. The application owns state, orchestration, memory, and tool execution. The model owns nothing. This means any model can be swapped in without changing the system's behavior guarantees.

**One process, no seams.** No Docker. No Ollama. No vector database server. No message queue. Everything compiles to a single binary with an embedded inference engine, embedded database, and embedded UI. The user downloads one thing, runs one thing, and everything works. The system complexity lives inside the binary, not in the user's infrastructure.

**The user never sees infrastructure.** No model names. No parameter counts. No quantization levels. No VRAM calculations. The system detects hardware, selects models, and manages resources automatically. The user sees: a text box, their conversations, and the results of the agent's actions.

**Closed to modification, open to extension.** This is a product, not a platform. There is no plugin marketplace, no app store, no third-party runtime. But every internal boundary is a trait. Every subsystem communicates through defined interfaces. Any component can be replaced by an alternative implementation without modifying the components around it. The system ships as a complete, opinionated product — and simultaneously as a set of composable parts that a different team could reassemble into something we didn't anticipate.

This fourth principle is not in tension with the first three. The user never encounters the trait boundaries. They exist for the developers, the community, and the future — so that when the network-level protocols for verifiable agent orchestration mature, this system can participate in them without being rewritten.

---

## Architectural Invariants (SOLID, Applied)

Before the component designs, the contracts between them. These are the load-bearing interfaces. Everything else is implementation detail.

### The Five Boundaries

```rust
/// S: Single Responsibility — each trait does one thing.
/// O: Open/Closed — new implementations, not new branches in existing code.
/// L: Liskov Substitution — any impl is a drop-in replacement.
/// I: Interface Segregation — consumers depend only on what they use.
/// D: Dependency Inversion — the runtime depends on traits, never on structs.

// ─── 1. Inference ───────────────────────────────────────────

/// Any system that can take a prompt and return a completion.
/// llama.cpp today. A remote API tomorrow. A cluster of machines next year.
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Run a completion and return the full response.
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse>;

    /// Stream a completion token by token.
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>>;

    /// Generate an embedding vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Report available capacity (used by the Router to make slot decisions).
    fn capabilities(&self) -> ProviderCapabilities;
}

pub struct ProviderCapabilities {
    pub max_context_tokens: usize,
    pub supports_structured_output: bool,
    pub relative_speed: Speed,     // Fast, Medium, Slow
    pub relative_reasoning: Depth, // Shallow, Moderate, Deep
}

// ─── 2. Routing ─────────────────────────────────────────────

/// Decides how to handle a user message.
/// The default implementation uses the Fast inference slot.
/// Could be replaced with a rule-based system, a classifier, or a remote service.
#[async_trait]
pub trait Router: Send + Sync {
    async fn classify(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Intent>;
}

// ─── 3. Planning ────────────────────────────────────────────

/// Turns an intent into an executable plan.
/// The default uses the Primary inference slot to generate a DAG.
/// Could be a template library, a domain-specific planner, or a hybrid.
#[async_trait]
pub trait Planner: Send + Sync {
    async fn plan(
        &self,
        goal: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Plan>;

    async fn replan(
        &self,
        original: &Plan,
        completed: &[StepOutput],
        failure: &StepError,
    ) -> Result<Plan>;
}

// ─── 4. Tool Execution ──────────────────────────────────────

/// Any capability the agent can invoke in the world.
/// Built-in tools and MCP-connected tools implement the same trait.
#[async_trait]
pub trait Tool: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;
    fn required_permissions(&self) -> Vec<Permission>;

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput>;

    /// Optional: tool can validate params before execution.
    fn validate(&self, params: &serde_json::Value) -> Result<()> { Ok(()) }
}

pub struct ToolDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema
}

// ─── 5. Storage ─────────────────────────────────────────────

/// All persistent state flows through this trait.
/// SQLite today. Postgres for a team deployment. Encrypted store for
/// high-security contexts. The runtime never touches the database directly.
#[async_trait]
pub trait StateStore: Send + Sync {
    // Conversations
    async fn save_message(&self, msg: &Message) -> Result<()>;
    async fn get_conversation(&self, id: &str) -> Result<Conversation>;
    async fn search_messages(&self, query: &str) -> Result<Vec<Message>>;

    // Tasks
    async fn save_task(&self, task: &Task) -> Result<()>;
    async fn get_task(&self, id: &str) -> Result<Task>;

    // Memory
    async fn save_memory(&self, memory: &Memory) -> Result<()>;
    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>>;

    // Documents (RAG)
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()>;
    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>>;

    // Permissions
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>>;
    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()>;
}
```

### Why These Five and Not More

These five traits define the _minimum set of seams_ needed for the system to be fully extensible without becoming a framework. Each represents a genuine axis of variation:

- **InferenceProvider** varies by hardware (local GPU, CPU, remote, distributed).
- **Router** varies by sophistication (LLM-based, rule-based, hybrid, learned).
- **Planner** varies by domain (general-purpose, code-specific, workflow-specific).
- **Tool** varies by capability (the open set of things an agent can do).
- **StateStore** varies by deployment (embedded, server, encrypted, synced).

No trait exists speculatively. Each has at least two concrete implementations that ship or are planned. If a boundary has only one implementation with no foreseeable second, it's a struct, not a trait. Premature abstraction is as harmful as premature optimization.

### The Runtime Assembles Traits

```rust
pub struct Runtime {
    inference: Box<dyn InferenceProvider>,
    router: Box<dyn Router>,
    planner: Box<dyn Planner>,
    tools: ToolRegistry,  // Vec<Box<dyn Tool>> behind a lookup
    store: Box<dyn StateStore>,
}

impl Runtime {
    /// The main entry point. Everything flows through here.
    pub async fn handle_message(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        // 1. Load context
        let context = self.store.get_conversation(conversation_id).await?;
        let memories = self.store.get_relevant_memories(message, 5).await?;
        let tools = self.tools.descriptors();

        // 2. Route
        let intent = self.router.classify(message, &context, &tools).await?;

        // 3. Execute based on intent
        match intent {
            Intent::SimpleQuery => {
                self.inference.complete(&request.with_slot(Speed::Fast)).await
            }
            Intent::DeepQuery => {
                self.inference.complete(&request.with_slot(Speed::Slow)).await
            }
            Intent::ComplexTask => {
                let plan = self.planner.plan(message, &context, &tools).await?;
                self.execute_plan(&plan, &context).await
            }
            // ... other variants
        }
    }
}
```

The `Runtime` struct has no `if cfg!(feature = ...)` branches. No `match provider_type`. No conditional compilation for different backends. It operates exclusively on trait objects. To change behavior, you swap an implementation at construction time, not at call time.

This is the Open/Closed principle in its purest form: the Runtime is closed to modification and open to extension through its constructor.

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                          Sovereign                               │
│                                                                  │
│  ┌──────────┐  ┌───────────────────────────────────────────────┐ │
│  │          │  │              Runtime                          │ │
│  │   UI     │  │  ┌────────────────────────────────────────┐   │ │
│  │  (Tauri  │◄►│  │        handle_message()                │   │ │
│  │  WebView)│  │  └────┬──────────┬──────────────┬─────────┘   │ │
│  │          │  │       │          │              │              │ │
│  └──────────┘  │  ┌────▼───┐ ┌───▼────┐  ┌──────▼──────┐      │ │
│                │  │ dyn    │ │ dyn    │  │    dyn      │      │ │
│                │  │ Router │ │Planner │  │  Executor   │      │ │
│                │  └────┬───┘ └───┬────┘  └──────┬──────┘      │ │
│                │       │         │              │              │ │
│                │  ┌────▼─────────▼──────────────▼───────────┐  │ │
│                │  │       dyn StateStore                    │  │ │
│                │  └────────────────────────────────────────┘  │ │
│                └───────────────────────────────────────────────┘ │
│                                                                  │
│  ┌───────────────────────────┐  ┌──────────────────────────────┐ │
│  │  dyn InferenceProvider    │  │   ToolRegistry               │ │
│  │  ┌─────────────────────┐  │  │   Vec<Box<dyn Tool>>         │ │
│  │  │  Default:            │  │  │                              │ │
│  │  │  EmbeddedLlamaCpp   │  │  │  ┌────────┐ ┌────────────┐   │ │
│  │  │  ┌────┐ ┌────────┐ │  │  │  │ Built- │ │   MCP      │   │ │
│  │  │  │Fast│ │Primary │ │  │  │  │ in     │ │  Adapter   │   │ │
│  │  │  │Slot│ │ Slot   │ │  │  │  │ Tools  │ │  (→ dyn    │   │ │
│  │  │  └────┘ └────────┘ │  │  │  │        │ │    Tool)   │   │ │
│  │  │  ┌──────────────┐  │  │  │  └────────┘ └────────────┘   │ │
│  │  │  │ Embed Slot   │  │  │  │  ┌────────────────────────┐   │ │
│  │  │  └──────────────┘  │  │  │  │  Permission Gate       │   │ │
│  │  └─────────────────────┘  │  │  └────────────────────────┘   │ │
│  └───────────────────────────┘  └──────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘

Every box with "dyn" is a trait boundary.
Every trait boundary is a potential extension point.
The default product ships with exactly one implementation per trait.
```

The entire system is a single Rust binary containing the default implementations:

- **Tauri WebView** for the UI (native OS webview, not Electron — ~5MB, not 200MB)
- **EmbeddedLlamaCpp** implementing `InferenceProvider` (llama.cpp via FFI)
- **SQLite** implementing `StateStore` (with FTS5 and sqlite-vec extensions)
- **LlmRouter** implementing `Router` (uses the Fast inference slot)
- **LlmPlanner** implementing `Planner` (uses the Primary inference slot)
- Built-in tools implementing `Tool` (email, calendar, files, web, knowledge)

---

## 1. InferenceProvider: Default Implementation

### Why Not Ollama

Ollama is excellent for developers. It is wrong for this system because it is a separate process, introduces IPC latency, manages its own model lifecycle independently of the application, and requires the user to understand that it exists. The default implementation, `EmbeddedLlamaCpp`, embeds llama.cpp directly via Rust FFI bindings. The application controls model loading, memory allocation, and scheduling at the library level.

However, because the rest of the system depends on `dyn InferenceProvider` and never on `EmbeddedLlamaCpp` directly, an alternative implementation can wrap Ollama, a remote API, or a cluster of machines — and the Runtime doesn't change. This is not hypothetical: the `RemoteApiProvider` implementation (wrapping any OpenAI-compatible endpoint) ships alongside the default for users who want cloud fallback.

```rust
// These are two implementations of the same trait.
// The Runtime accepts either. It never knows which one it has.

pub struct EmbeddedLlamaCpp { /* llama.cpp FFI state */ }
pub struct RemoteApiProvider { /* HTTP client, API key, endpoint */ }
pub struct HybridProvider {
    local: EmbeddedLlamaCpp,
    remote: RemoteApiProvider,
    policy: FallbackPolicy,  // e.g., "remote only if local queue > 5s"
}

// All three implement InferenceProvider identically from the Runtime's perspective.
```

### Model Slot Architecture

The `EmbeddedLlamaCpp` implementation manages three memory slots:

|Slot|Purpose|Loaded|Typical Size|
|---|---|---|---|
|**Fast**|Triage, classification, simple extraction|Always|1-3B params, ~1.5 GB|
|**Primary**|Reasoning, planning, synthesis, conversation|On demand|7-14B params, ~6 GB|
|**Embed**|Document embeddings for RAG|Always|~0.5 GB|

On a 12GB GPU, both always-resident slots use ~2GB, leaving ~10GB for the Primary slot. On 8GB, the Fast slot downgrades to a smaller quantization. On CPU-only, the Fast slot uses a 1B model and the Primary slot uses a 3-7B model with slower but functional performance.

The slot architecture is an implementation detail of `EmbeddedLlamaCpp`, not of the `InferenceProvider` trait. The trait exposes `ProviderCapabilities` so the Router can make informed decisions, but it doesn't know or care how those capabilities are achieved internally. A `RemoteApiProvider` has no slots — it just reports high speed and deep reasoning because the cloud model is large and fast.

### Model Selection

The system ships with a `models.toml` manifest:

```toml
[profiles.default]
fast = { repo = "Qwen/Qwen3-1.7B-GGUF", quant = "Q4_K_M", min_ram_gb = 4 }
primary = { repo = "Qwen/Qwen3-14B-GGUF", quant = "Q4_K_M", min_vram_gb = 10 }
embed = { repo = "Qwen/Qwen3-Embedding-0.6B-GGUF", quant = "F16" }

[profiles.low_mem]
fast = { repo = "Qwen/Qwen3-0.6B-GGUF", quant = "Q4_K_M", min_ram_gb = 2 }
primary = { repo = "Qwen/Qwen3-8B-GGUF", quant = "Q4_K_M", min_vram_gb = 6 }
embed = { repo = "Qwen/Qwen3-Embedding-0.6B-GGUF", quant = "Q8_0" }

[profiles.cpu_only]
fast = { repo = "Qwen/Qwen3-0.6B-GGUF", quant = "Q4_K_M" }
primary = { repo = "Qwen/Qwen3-4B-GGUF", quant = "Q4_K_M" }
embed = { repo = "Qwen/Qwen3-Embedding-0.6B-GGUF", quant = "Q4_K_M" }
```

On first launch, the system detects GPU vendor, VRAM, and system RAM. It selects the appropriate profile automatically. Models download from Hugging Face on demand. The user sees a progress bar that says "Setting up your assistant" — not model names, not quantization levels.

The manifest is updatable. When better models release, a manifest update pulls them automatically (with user consent). The user's experience improves without them doing anything.

### GPU Memory Management

The Primary slot loads and unloads dynamically. When the user sends a simple message ("what time is it in Tokyo?"), only the Fast slot responds. When the Router classifies a request as requiring deeper reasoning, the Primary model loads (typically 2-4 seconds on NVMe). After 60 seconds of inactivity, the Primary model unloads, freeing VRAM for other applications.

This is invisible to the user. From their perspective, simple questions are instant and complex ones take a few seconds to "think."

```rust
pub struct EmbeddedLlamaCpp {
    fast: LoadedModel,
    embed: LoadedModel,
    primary: Option<LoadedModel>,
    config: HardwareProfile,
    last_primary_use: Instant,
}

#[async_trait]
impl InferenceProvider for EmbeddedLlamaCpp {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        match request.preferred_speed {
            Speed::Fast => self.fast.complete(&request.prompt).await,
            Speed::Slow | Speed::Medium => {
                self.ensure_primary_loaded().await?;
                self.last_primary_use = Instant::now();
                self.primary.as_ref().unwrap().complete(&request.prompt).await
            }
        }
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed.embed(text).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: self.config.primary_context_size,
            supports_structured_output: true,
            relative_speed: if self.config.has_gpu { Speed::Medium } else { Speed::Slow },
            relative_reasoning: Depth::Deep,
        }
    }

    // ... complete_stream similarly
}
```

---

## 2. Router: Default Implementation

The Router is the first thing that processes every user message. The default implementation, `LlmRouter`, runs on the Fast inference slot and classifies the message:

```rust
pub enum Intent {
    /// Simple question answerable from model knowledge
    /// → Fast completion, direct response
    SimpleQuery,

    /// Needs deeper reasoning, analysis, creative work
    /// → Primary completion, direct response
    DeepQuery,

    /// Needs information from user's documents
    /// → Embed for retrieval, then Primary for synthesis
    KnowledgeQuery,

    /// Requires tool use (single tool, no planning needed)
    /// → One tool call, then synthesize
    SimpleAction { tool: ToolId },

    /// Complex multi-step task requiring planning
    /// → Planner generates DAG, Executor walks it
    ComplexTask,

    /// Continuation of an in-progress task
    /// → Resume existing execution context
    Continuation { task_id: TaskId },
}
```

The classification prompt is minimal and structured. It runs in <100ms on a 1.7B model:

```
Classify this user message into exactly one category.
Context: [last 2 messages for conversational continuity]
Available tools: [list of enabled tool names]
Message: "{user_message}"
Respond with only the JSON: {"intent": "...", "tool": "...", "confidence": 0.0}
```

When confidence is below 0.7, the Router asks the user a clarifying question rather than guessing. The system should be honest about uncertainty rather than confidently wrong.

### Why the Router is a Trait

A developer building a domain-specific fork — say, a legal assistant — might replace `LlmRouter` with a `DomainRouter` that uses pattern matching and keyword detection for known legal task types, falling back to the LLM only for ambiguous cases. This is faster, cheaper, and more deterministic for the common paths.

The Router trait accepts `&[ToolDescriptor]` as input. It never reaches into the ToolRegistry directly. This is Interface Segregation: the Router knows what tools exist (their descriptors) but not how they work (their implementations).

### Routing Statistics

The Router maintains a log of classification outcomes (written through `dyn StateStore`). If a user re-asks a question in a way that implies the first routing was wrong, the system logs this as implicit feedback. Over time, routing accuracy improves for that user's patterns.

---

## 3. Planner: Default Implementation

When the Router classifies a message as `ComplexTask`, the Planner takes over. The default `LlmPlanner` runs on the Primary slot and produces a task DAG.

### Plan Structure

```rust
pub struct Plan {
    id: TaskId,
    goal: String,
    steps: Vec<Step>,
    edges: Vec<(usize, usize)>,  // dependency edges
}

pub struct Step {
    id: usize,
    description: String,
    kind: StepKind,
    requires_approval: bool,
    inputs: Vec<StepInput>,  // references to outputs of prior steps
}

pub enum StepKind {
    Reason { prompt_template: String, speed: Speed },
    Tool { tool_id: ToolId, params: serde_json::Value },
    UserInput { question: String },
    Branch { condition: String, if_true: usize, if_false: usize },
}
```

### Example

User: "Find the cheapest flight from SF to Chicago next Tuesday, check if it conflicts with anything on my calendar, and if it's clear, draft an email to my team saying I'll be traveling."

```
Step 0: Tool(web_search, "cheapest flight SFO to ORD next Tuesday")
Step 1: Tool(calendar_read, { date: "next Tuesday", range: "all day" })
Step 2: Reason("Given flight options {0.output} and calendar {1.output},
         identify the best non-conflicting flight.")  [depends on: 0, 1]
Step 3: Branch("Conflict-free option exists?",
         if_true: 4, if_false: 5)  [depends on: 2]
Step 4: Reason("Draft a brief team email about traveling to Chicago
         on {flight from step 2}")  [depends on: 2]
         requires_approval: true
Step 5: UserInput("All flights conflict with your calendar.
         Show the options anyway?")
```

Steps 0 and 1 execute in parallel. Step 2 waits for both. The user sees each step's progress in real time.

### Plan-and-Execute, Not ReAct

This is a deliberate architectural choice. ReAct loops call the LLM at every step to decide what to do next. Plan-and-Execute calls the LLM once to generate the full plan, then executes deterministically. The LLM is only called again if a step fails and replanning is needed.

### Why the Planner is a Trait

The `Planner` trait takes `&[ToolDescriptor]` — it knows what tools can do, but not how. This is dependency inversion. The Planner depends on the abstraction (descriptors), not the implementation (tools).

A specialized planner for, say, data analysis workflows could skip the LLM entirely for known patterns: "analyze this CSV" always produces the same DAG shape (read file → detect columns → run requested analysis → format output). LLM planning is only needed for genuinely novel tasks. This hybrid approach — templates for the common path, LLM for the long tail — is more reliable and faster than pure LLM planning.

---

## 4. Executor

The Executor walks the plan DAG, running independent steps in parallel. Unlike the Router, Planner, and InferenceProvider, the Executor is NOT a trait. It is the one piece of the system that is genuinely fixed: its job is to topologically sort a DAG and execute it. There is no meaningful alternative implementation. Making it a trait would be premature abstraction.

The Executor does depend on traits: it calls `dyn InferenceProvider` for Reason steps, looks up `dyn Tool` for Tool steps, and persists state through `dyn StateStore`.

```rust
pub struct Executor {
    inference: Arc<dyn InferenceProvider>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn StateStore>,
}

impl Executor {
    pub async fn run(&self, plan: &Plan, ctx: &mut TaskContext) -> Result<TaskResult> {
        let mut completed: HashMap<usize, StepOutput> = HashMap::new();

        for batch in plan.topological_batches() {
            let results = futures::join_all(
                batch.iter().map(|step| self.execute_step(step, &completed, ctx))
            ).await;

            for (step, result) in batch.iter().zip(results) {
                match result {
                    Ok(output) => {
                        completed.insert(step.id, output.clone());
                        self.store.save_task(&ctx.task_snapshot(&completed)).await?;
                        ctx.emit_progress(step, &output);
                    }
                    Err(e) => return self.handle_failure(plan, step.id, &e, &completed, ctx).await,
                }
            }
        }

        self.synthesize(&plan.goal, &completed, ctx).await
    }

    async fn execute_step(
        &self,
        step: &Step,
        prior: &HashMap<usize, StepOutput>,
        ctx: &mut TaskContext,
    ) -> Result<StepOutput> {
        let resolved = step.resolve_inputs(prior)?;

        match &step.kind {
            StepKind::Reason { prompt_template, speed } => {
                let prompt = prompt_template.interpolate(&resolved);
                let request = CompletionRequest::new(&prompt).with_speed(*speed);
                let response = self.inference.complete(&request).await?;
                Ok(StepOutput::Text(response.text))
            }
            StepKind::Tool { tool_id, params } => {
                let tool = self.tools.get(tool_id)?;
                let params = params.interpolate(&resolved);

                if step.requires_approval {
                    let approved = ctx.request_approval(step, &params).await?;
                    if !approved { return Ok(StepOutput::Skipped); }
                }

                tool.execute(&params, &ctx.tool_context()).await
            }
            StepKind::UserInput { question } => {
                let q = question.interpolate(&resolved);
                Ok(StepOutput::Text(ctx.ask_user(&q).await?))
            }
            StepKind::Branch { condition, if_true, if_false } => {
                let request = CompletionRequest::yes_no(condition, &resolved);
                let response = self.inference.complete(&request).await?;
                let next = if response.as_bool() { *if_true } else { *if_false };
                Ok(StepOutput::Jump(next))
            }
        }
    }
}
```

### Replanning

When a step fails, the Executor doesn't retry blindly. It calls the Planner's `replan()` method with the original goal, completed steps, and the error. This typically succeeds because most failures are recoverable.

If replanning fails twice, the system surfaces the situation plainly: "I tried to find flights but the search isn't returning results. Want me to try a different approach?"

### Task Persistence

Every task's state persists through `dyn StateStore` after each step completion. If the application crashes or the user closes it mid-task, the task resumes on next launch.

---

## 5. StateStore: Default Implementation

Everything lives in a single SQLite database. No Redis. No Postgres. No vector database server. SQLite is embedded, zero-configuration, and handles concurrent reads with WAL mode.

### Schema

```sql
-- Conversations and messages
CREATE TABLE conversations (
    id          TEXT PRIMARY KEY,
    title       TEXT,
    created_at  INTEGER,
    updated_at  INTEGER
);

CREATE TABLE messages (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT REFERENCES conversations(id),
    role            TEXT CHECK(role IN ('user', 'assistant', 'system')),
    content         TEXT,
    created_at      INTEGER,
    metadata        TEXT  -- JSON: model used, tokens, latency, routing decision
);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    content, content=messages, content_rowid=rowid
);

-- Task execution state
CREATE TABLE tasks (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT REFERENCES conversations(id),
    goal            TEXT,
    plan            TEXT,    -- JSON serialized Plan
    state           TEXT,    -- JSON: completed steps, outputs, current position
    status          TEXT CHECK(status IN ('running', 'paused', 'completed', 'failed')),
    created_at      INTEGER,
    updated_at      INTEGER
);

-- RAG: document store and embeddings
CREATE TABLE documents (
    id          TEXT PRIMARY KEY,
    source      TEXT,
    content     TEXT,
    chunk_index INTEGER,
    embedding   BLOB,          -- f32 vector, used with sqlite-vec
    created_at  INTEGER
);

-- Long-term user memory
CREATE TABLE memories (
    id          TEXT PRIMARY KEY,
    content     TEXT,
    source      TEXT,
    confidence  REAL,
    created_at  INTEGER,
    last_used   INTEGER
);

-- Tool permissions
CREATE TABLE permissions (
    tool_id     TEXT,
    scope       TEXT,
    granted     INTEGER,
    granted_at  INTEGER,
    PRIMARY KEY (tool_id, scope)
);

-- Router performance
CREATE TABLE routing_log (
    id              INTEGER PRIMARY KEY,
    message_hash    TEXT,
    classified_as   TEXT,
    was_correct     INTEGER,
    latency_ms      INTEGER,
    created_at      INTEGER
);
```

### Why the StateStore is a Trait

Single-user SQLite is the right default. But the trait enables:

- **Encrypted storage** for sensitive deployments (same schema, encryption-at-rest layer).
- **Server-backed storage** for a hypothetical team version (Postgres or Turso behind the same interface).
- **Sync adapter** that writes locally and replicates to a personal cloud backup.
- **The eventual network layer** — when verifiable orchestration protocols exist, the StateStore could expose signed attestations of task execution history without modifying the Runtime.

This is the Liskov Substitution principle: any implementation of `StateStore` produces the same behavior from the Runtime's perspective. The Runtime asks "save this task" and "give me relevant memories." How those operations are fulfilled — encrypted, replicated, attested — is invisible to it.

### Vector Search

For RAG, the default implementation uses `sqlite-vec` (compiled in). Exact nearest-neighbor search on BLOB vectors. For collections under ~100k chunks, this is fast enough. No HNSW indexes or separate vector database needed.

---

## 6. Tool Runtime

### The Tool Trait as the Primary Extension Point

The `Tool` trait is the most important extension surface in the system. While Router, Planner, and InferenceProvider might be swapped by power users or fork maintainers, Tools are extended by _everyone_. Any new capability the agent gains comes through a new `Tool` implementation.

Built-in tools and MCP-bridged tools implement the same trait. From the Executor's perspective, there is no distinction between "email a tool that ships with the product" and "Slack is a tool connected via MCP." They are both `Box<dyn Tool>`.

### MCP Adapter

The MCP client is not a special subsystem. It is an adapter that turns an MCP server's tool descriptors into `Tool` trait objects:

```rust
pub struct McpToolAdapter {
    client: McpClient,
    server_tool: McpToolDescriptor,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn descriptor(&self) -> ToolDescriptor {
        // Translate MCP's tool descriptor to our ToolDescriptor
        ToolDescriptor {
            id: format!("mcp:{}:{}", self.client.server_name(), self.server_tool.name),
            name: self.server_tool.name.clone(),
            description: self.server_tool.description.clone(),
            parameters: self.server_tool.input_schema.clone(),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Network]  // MCP tools always need network
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let result = self.client.call_tool(&self.server_tool.name, params).await?;
        Ok(StepOutput::Text(result.content_as_text()))
    }
}
```

When a user connects an MCP server (via configuration), the system discovers the server's tools, wraps each in `McpToolAdapter`, and registers them in the `ToolRegistry`. The Router and Planner see them as available tools. The Executor executes them. No special case anywhere.

### Built-in Tools

|Tool|Permissions|Implementation|
|---|---|---|
|`web_search`|network|SearXNG instance or DuckDuckGo|
|`web_fetch`|network|HTTP client with readability extraction|
|`file_read`|fs:read|Local filesystem, scoped to allowed dirs|
|`file_write`|fs:write|Write to user-designated output directory|
|`email_read`|email:read|IMAP client|
|`email_send`|email:write|SMTP client (always requires approval)|
|`calendar_read`|calendar:read|CalDAV client|
|`calendar_write`|calendar:write|CalDAV write (always requires approval)|
|`shell`|shell:exec|Sandboxed command execution (requires approval)|
|`knowledge`|none|RAG search over ingested documents|
|`compute`|none|Sandboxed Python for calculations|

### Permission System

Every tool declares its required permissions via the trait. On first use:

> "To search the web, I need network access. Allow this?" **[Always allow]** **[Allow once]** **[Deny]**

Write operations (email_send, calendar_write, file_write, shell) ALWAYS require per-action approval. This is hardcoded in the Executor, not in the tools. The Executor checks `step.requires_approval` and gates on it. A tool cannot bypass this by lying about its permissions — the Executor independently enforces the gate for any step marked as side-effecting by the Planner.

This separation matters: the tool reports what permissions it needs (dependency inversion — the tool depends on the permission abstraction). The Executor enforces the policy (single responsibility — tools execute, the Executor governs). Neither reaches into the other's domain.

---

## 7. Memory System

### Working Memory

Each conversation maintains a working memory — a structured scratchpad compressed from the full message history.

```rust
pub struct WorkingMemory {
    current_goal: Option<String>,
    facts: Vec<String>,
    active_documents: Vec<DocumentRef>,
    context_window: Vec<ContextItem>,
}
```

Before each model call, the working memory is serialized into the system prompt. The model sees a crisp summary rather than the full message history — focused and token-efficient.

### Long-term Memory

After each conversation ends, the Primary model runs a background extraction:

```
Given this conversation, extract any durable facts about the user that
would be useful in future conversations. Only extract clearly true,
persistently relevant things. Return JSON array or empty array.
```

Memories are injected when relevant (matched by embedding similarity via `dyn StateStore`). Confidence decays 10% per month, pruned below 0.2. Contradictions delete immediately.

---

## 8. RAG Pipeline

Users can point the system at directories. A background pipeline processes them:

1. **Parse**: PDF, DOCX, TXT, MD, HTML via built-in parsers. No external services.
2. **Chunk**: Semantic chunking on paragraph boundaries, max 512 tokens, 64-token overlap.
3. **Embed**: Via `dyn InferenceProvider`'s `embed()` method.
4. **Index**: Stored through `dyn StateStore`'s `store_chunks()`.

Retrieval uses hybrid search: vector similarity for semantic matches, FTS5 for keyword matches, re-ranked by the Fast model.

Note how the RAG pipeline depends only on traits: `InferenceProvider` for embeddings, `StateStore` for persistence. If the InferenceProvider is swapped to a remote service that offers better embeddings, the RAG pipeline automatically benefits. If the StateStore is swapped to a distributed database, the document index is automatically distributed. No code changes.

OS file watchers handle incremental updates. The user never manually triggers re-indexing.

---

## 9. UI

### Framework: Tauri

Tauri uses the OS's native webview (WebKit on macOS, WebView2 on Windows, WebKitGTK on Linux). The binary stays small (~5MB for the Rust core). No Electron. No Chromium.

### Design Principles

**The interface is a conversation**, not a dashboard. Single-column chat. No sidebars full of settings, no model selectors, no temperature sliders. Configuration exists but lives in a settings panel most users never open.

**Task progress is transparent but unobtrusive.** Multi-step plans show inline:

```
┌────────────────────────────────────────┐
│ Finding flights...              ✓ done │
│ Checking your calendar...       ✓ done │
│ Comparing options...          ● active │
│ Draft email                   ○ next   │
└────────────────────────────────────────┘
```

Approval prompts interrupt inline. The user never leaves the conversation.

**First launch is a 3-step wizard:**

1. "Downloading your AI assistant" (model download, progress bar)
2. "What should I be able to do?" (toggles for capabilities, sub-forms for credentials)
3. "Ready. Ask me anything."

Download to first conversation in under 5 minutes.

### Frontend Stack

- **Svelte** — smaller bundle, faster rendering than React
- **Tailwind CSS** — utility-first styling
- **Tauri IPC** — typed `invoke` commands between Svelte and Rust

### The UI-Runtime Boundary

The Tauri IPC contract is the boundary between the frontend and the Runtime. It is a typed API, not a trait, because there is no meaningful alternative implementation of "display a conversation in a webview." But it is versioned and documented, so that alternative frontends — a CLI, a mobile app, a headless daemon for automation — can be built against the same Runtime without modifying it.

```rust
// The IPC contract. Frontend calls these. Runtime implements them.
#[tauri::command]
async fn send_message(conversation_id: String, message: String) -> Result<Response>;

#[tauri::command]
async fn approve_action(task_id: String, step_id: usize, approved: bool) -> Result<()>;

#[tauri::command]
async fn get_conversations(limit: usize, offset: usize) -> Result<Vec<ConversationSummary>>;

#[tauri::command]
async fn search(query: String) -> Result<Vec<SearchResult>>;

#[tauri::command]
async fn update_preferences(prefs: Preferences) -> Result<()>;

#[tauri::command]
async fn connect_tool(config: ToolConfig) -> Result<ToolDescriptor>;
```

This is small. Deliberately. The frontend asks the Runtime to do things and the Runtime reports results. The frontend never reaches through to the inference engine, the tools, or the store. Single responsibility: the frontend renders, the Runtime decides.

---

## 10. sovereign-server: The API

The same Runtime, no Tauri, exposed as an HTTP and WebSocket service. This is the product for startups: deploy on your own GPU box, your own cloud instance, or your own Kubernetes cluster. Get the full orchestration layer — routing, planning, multi-step execution, RAG, memory, tool use — through a clean API.

### Why This is Not a Second Product

It is the same product in a different shell. `sovereign-server` is ~500 lines of code. It constructs a `Runtime` with the same trait implementations and wraps it in Axum (Rust's async HTTP framework). Every behavior — how plans are generated, how tools execute, how memory works — is identical to the desktop app because it's the same `Runtime` struct.

This is the payoff of the crate structure. There was no "add server mode" refactoring. The Runtime never knew it was inside Tauri in the first place.

### Construction

```rust
// sovereign-server/src/main.rs
// This is the entire server bootstrap.

#[tokio::main]
async fn main() -> Result<()> {
    let config = ServerConfig::from_env()?;

    // Construct the Runtime from the same implementations as desktop.
    // The only difference: InferenceProvider might be a RemoteApiProvider
    // pointing at a GPU cluster, and StateStore might be Postgres.
    let inference: Box<dyn InferenceProvider> = match &config.inference {
        InferenceConfig::Local(hw) => Box::new(EmbeddedLlamaCpp::new(hw).await?),
        InferenceConfig::Remote(endpoint) => Box::new(RemoteApiProvider::new(endpoint)?),
        InferenceConfig::Hybrid { local, remote, policy } => {
            Box::new(HybridProvider::new(
                EmbeddedLlamaCpp::new(local).await?,
                RemoteApiProvider::new(remote)?,
                policy.clone(),
            ))
        }
    };

    let store: Box<dyn StateStore> = match &config.store {
        StoreConfig::Sqlite(path) => Box::new(SqliteStateStore::open(path).await?),
        StoreConfig::Postgres(url) => Box::new(PostgresStateStore::connect(url).await?),
    };

    let tools = ToolRegistry::from_config(&config.tools).await?;

    let runtime = Arc::new(Runtime::new(
        inference,
        Box::new(LlmRouter::new()),
        Box::new(LlmPlanner::new()),
        tools,
        store,
    ));

    let app = build_router(runtime);
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

### API Surface

The API mirrors the Tauri IPC contract — same operations, HTTP transport.

```
REST Endpoints
──────────────────────────────────────────────────────────

POST   /v1/conversations
       Create a new conversation.
       → { id, created_at }

POST   /v1/conversations/{id}/messages
       Send a message. Returns the full response when complete.
       Body: { content: "..." }
       → { message_id, role: "assistant", content, task?: { id, status, steps } }

GET    /v1/conversations/{id}
       Retrieve conversation history.
       → { id, messages[], active_tasks[] }

GET    /v1/conversations
       List conversations.
       ?limit=20&offset=0&search=query
       → { conversations[], total }

POST   /v1/tasks/{id}/approve
       Approve a pending action.
       Body: { step_id, approved: true }
       → { task_id, status, next_step }

DELETE /v1/conversations/{id}
       Delete a conversation and its tasks.

GET    /v1/tools
       List available tools and their permission status.
       → { tools: [{ id, name, description, permissions }] }

POST   /v1/tools/connect
       Connect a new MCP server or configure a built-in tool.
       Body: { type: "mcp", url: "..." } | { type: "email", config: {...} }
       → { tool: ToolDescriptor }

POST   /v1/documents
       Ingest a document into the RAG pipeline.
       Body: multipart file upload
       → { document_id, chunks_created }

POST   /v1/search
       Search across conversations and documents.
       Body: { query: "..." }
       → { results: [{ type, content, source }] }


WebSocket Endpoint
──────────────────────────────────────────────────────────

WS     /v1/conversations/{id}/stream

       Bidirectional. For real-time interaction during task execution.

       Server → Client events:
       { type: "token",        data: { content: "..." } }
       { type: "step_started", data: { task_id, step } }
       { type: "step_done",    data: { task_id, step, output } }
       { type: "approval_req", data: { task_id, step, action_preview } }
       { type: "user_input",   data: { task_id, step, question } }
       { type: "error",        data: { task_id, step?, message } }
       { type: "done",         data: { task_id, final_response } }

       Client → Server events:
       { type: "message",      data: { content: "..." } }
       { type: "approve",      data: { task_id, step_id, approved: bool } }
       { type: "user_reply",   data: { task_id, step_id, content: "..." } }
       { type: "cancel",       data: { task_id } }
```

### The Approval Problem Over HTTP

In the desktop app, an approval prompt appears as a UI card and the Executor `await`s the user's click. Over an API, this is a harder interaction pattern. Two modes:

**Synchronous (REST):** When the Executor reaches a step requiring approval, it pauses the task and returns a response with `status: "awaiting_approval"` and the action preview. The client calls `POST /v1/tasks/{id}/approve` to continue. The task resumes. This works for backends and scripts where the caller controls the flow.

**Asynchronous (WebSocket):** The server pushes an `approval_req` event. The client pushes an `approve` event back. The task continues. This is the natural mode for chat UIs built on top of the API — the startup's own frontend receives the approval request and renders it however they want.

The Executor doesn't know which mode it's in. It calls `ctx.request_approval()`, which is an async function that resolves when an approval arrives. In the desktop app, this is backed by a Tauri IPC channel. In the server, it's backed by a tokio channel that connects to either the REST handler or the WebSocket handler. The Executor's code is identical in both cases.

```rust
// The TaskContext abstracts over approval delivery.
// The Executor never knows if it's running in Tauri or Axum.

#[async_trait]
pub trait ApprovalChannel: Send + Sync {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool>;
    async fn ask_user(&self, question: &str) -> Result<String>;
    fn emit_progress(&self, step: &Step, output: &StepOutput);
}

// Desktop: backed by Tauri event emitter + IPC response channel
pub struct TauriApprovalChannel { /* ... */ }

// Server: backed by tokio broadcast channel tied to WebSocket or REST poll
pub struct ServerApprovalChannel { /* ... */ }
```

### Multi-Tenancy

The desktop app is single-user by design. The server needs to handle multiple users — the startup's customers.

The approach is simple and deliberately not clever: **one Runtime instance per tenant, isolated by conversation namespace.** The server maintains a pool of Runtime instances (or, more practically, a single Runtime with tenant-scoped StateStore access).

```rust
// The server wraps the Runtime with tenant isolation.
pub struct TenantRuntime {
    runtime: Arc<Runtime>,
    tenant_id: String,
}

impl TenantRuntime {
    pub async fn handle_message(
        &self,
        conversation_id: &str,
        message: &str,
    ) -> Result<Response> {
        // Scope the conversation ID to the tenant.
        let scoped_id = format!("{}:{}", self.tenant_id, conversation_id);
        self.runtime.handle_message(message, &scoped_id).await
    }
}
```

The `StateStore` implementation handles the scoping. `SqliteStateStore` uses a single database with tenant_id columns. `PostgresStateStore` can use schemas or row-level security. The Runtime doesn't know tenants exist — it just sees conversation IDs that happen to be prefixed.

Authentication is the server's responsibility, not the Runtime's. The server validates a bearer token or API key, resolves it to a tenant ID, and constructs a `TenantRuntime`. Standard middleware. The Runtime never sees credentials.

### Configuration

The server is configured by environment variables and a TOML file — the deployment pattern that startups already use.

```toml
# sovereign-server.toml

[server]
bind = "0.0.0.0:8080"
workers = 4

[auth]
# API key validation. Simple for v1. OAuth/OIDC is a future extension.
mode = "api_key"       # "api_key" | "jwt" | "none" (for internal use)
keys_file = "/etc/sovereign/keys.toml"

[inference]
# Local GPU inference — the common case for a startup with a GPU box.
mode = "local"
fast_model = "Qwen/Qwen3-1.7B-GGUF"
primary_model = "Qwen/Qwen3-14B-GGUF"
embed_model = "Qwen/Qwen3-Embedding-0.6B-GGUF"

# Or point at an existing inference endpoint:
# mode = "remote"
# endpoint = "http://vllm-cluster:8000/v1"

# Or hybrid:
# mode = "hybrid"
# local_fast = "Qwen/Qwen3-1.7B-GGUF"
# remote_primary = "http://vllm-cluster:8000/v1"

[store]
mode = "postgres"
url = "postgresql://sovereign:password@db:5432/sovereign"
# Or: mode = "sqlite", path = "/data/sovereign.db"

[tools]
# Which built-in tools to enable for all tenants
enabled = ["web_search", "web_fetch", "knowledge", "compute"]

# Tenant-specific tool configs go in tenant profiles
# (e.g., IMAP credentials per tenant, loaded from keys_file or env)

[models]
# Where to download/cache model weights
cache_dir = "/data/models"
```

### Deployment

The server ships as:

**A static binary.** `cargo build --release -p sovereign-server` produces a single binary with llama.cpp and SQLite statically linked. Copy it to a box with a GPU, point it at a config file, run it.

**A Docker image.** Based on NVIDIA's CUDA runtime image. ~2GB including CUDA libraries. Models mount as a volume.

```dockerfile
FROM nvidia/cuda:12.4-runtime-ubuntu24.04
COPY sovereign-server /usr/local/bin/
COPY models.toml /etc/sovereign/
EXPOSE 8080
ENTRYPOINT ["sovereign-server", "--config", "/etc/sovereign/server.toml"]
```

```yaml
# docker-compose.yml — a startup's minimal deployment
services:
  sovereign:
    image: sovereign/server:latest
    ports: ["8080:8080"]
    volumes:
      - ./models:/data/models
      - ./config:/etc/sovereign
    deploy:
      resources:
        reservations:
          devices:
            - capabilities: [gpu]
    environment:
      - SOVEREIGN_STORE_URL=postgresql://db:5432/sovereign

  db:
    image: postgres:17
    volumes: ["pgdata:/var/lib/postgresql/data"]
    environment:
      - POSTGRES_DB=sovereign
      - POSTGRES_PASSWORD=sovereign
```

**Helm chart.** For Kubernetes deployments, a chart that provisions the inference pods (with GPU node affinity), the API pods (stateless, horizontally scalable), and the database.

The startup's engineers don't learn a new deployment paradigm. It's a web service with a database. They already know how to run this.

### What the Startup Gets

To be concrete: a startup integrating sovereign-server gets:

- An API that accepts natural language, returns structured responses, and executes multi-step tasks autonomously.
- Intelligent routing across model sizes so their GPU budget goes further.
- Built-in RAG — upload their customers' documents and query them immediately.
- Tool execution with approval gates their frontend can render.
- Streaming responses via WebSocket for real-time UX.
- Multi-tenant isolation so one deployment serves many customers.
- The full memory and personalization system, scoped per tenant.
- The ability to swap `InferenceProvider` to point at their existing vLLM or TGI cluster if they have one — no migration, just change the config.

What they don't get: a frontend. That's their product. Sovereign provides the brain. They provide the face.

---

## 11. Build and Distribution

### Single Binary

The release artifact is a Tauri installer:

- **macOS**: `.dmg` (~30MB before models)
- **Windows**: `.msi` (~30MB before models)
- **Linux**: `.AppImage` or `.deb` (~25MB before models)

Models download on first launch. Total: ~30MB app + ~4GB models (default profile). Minimal profile: ~1.5GB models, upgradeable later.

### Crate Structure

The trait boundaries are not just conceptual — they are crate boundaries. This is the critical decision that makes the API server possible without refactoring.

```
sovereign/
├── crates/
│   ├── sovereign-core/        # Traits + Runtime + Executor
│   │   └── lib.rs             # Zero dependencies on UI, HTTP, or Tauri
│   │
│   ├── sovereign-inference/   # EmbeddedLlamaCpp, RemoteApiProvider, HybridProvider
│   │   └── lib.rs             # Depends on: sovereign-core (traits only)
│   │
│   ├── sovereign-store/       # SqliteStateStore, PostgresStateStore
│   │   └── lib.rs             # Depends on: sovereign-core (traits only)
│   │
│   ├── sovereign-tools/       # Built-in tools, McpToolAdapter, ToolRegistry
│   │   └── lib.rs             # Depends on: sovereign-core (traits only)
│   │
│   ├── sovereign-desktop/     # Tauri app — the consumer product
│   │   └── main.rs            # Constructs Runtime with defaults, wraps in Tauri
│   │
│   └── sovereign-server/      # HTTP/WebSocket API — the startup product
│       └── main.rs            # Constructs Runtime, wraps in Axum
│
├── Cargo.toml                 # Workspace
└── models.toml                # Model manifest (shared)
```

`sovereign-core` compiles in under 2 seconds and has no system dependencies. It is pure Rust: traits, the Runtime struct, the Executor, the Plan/Step/Intent types. Everything else is a leaf crate that provides an implementation.

`sovereign-desktop` and `sovereign-server` are two thin binaries that assemble the same Runtime from the same implementation crates. They share 95% of their code. They differ only in how they present the Runtime to the outside world: one through Tauri IPC, the other through HTTP.

### Build Pipeline

```
sovereign-core (traits, Runtime, Executor)
    ↑ depended on by everything, depends on nothing

sovereign-inference    sovereign-store    sovereign-tools
(llama.cpp FFI)        (SQLite/Postgres)  (IMAP, CalDAV, MCP...)
    ↑                      ↑                  ↑
    └──────────┬───────────┘──────────────────┘
               │
    ┌──────────┴──────────┐
    │                     │
sovereign-desktop     sovereign-server
(Tauri + Svelte)      (Axum + WebSocket)
    ↓                     ↓
.dmg / .msi / .deb    Docker image / binary
```

### Update Mechanism

Tauri's built-in updater (signed, differential) for the binary. Model manifest updates offer better models: "A faster model is available for your hardware. Download? (3.2 GB)"

---

## 12. What Four Engineers Build

### Engineer 1: Core Runtime + Inference (Rust)

Owns: Trait definitions (all five), `EmbeddedLlamaCpp`, `LlmRouter`, `LlmPlanner`, Executor, GPU memory management, llama.cpp FFI bindings, `Runtime` struct and message handling loop.

This is the hardest engineering. The llama.cpp integration must handle model loading/unloading without leaking VRAM, concurrent requests across slots, KV-cache management, and graceful degradation. The trait design is also here — getting the boundaries right is the most consequential architectural decision and hardest to change later.

### Engineer 2: Tool Runtime + Integrations (Rust + Python)

Owns: Built-in `Tool` implementations (IMAP, SMTP, CalDAV, HTTP, filesystem, shell sandbox), MCP client and `McpToolAdapter`, `ToolRegistry`, permission system, RAG pipeline.

This engineer needs to make "connect my email" work for Gmail (OAuth2), Outlook, FastMail, and generic IMAP in a 2-field form. The MCP adapter is critical: it must faithfully translate between MCP's protocol and the `Tool` trait without leaking MCP-specific concerns into the rest of the system.

### Engineer 3: UI + UX (Svelte + TypeScript)

Owns: Tauri shell, Svelte frontend, setup wizard, conversation UI, task progress display, approval cards, settings panel, system tray, notifications. Defines the IPC contract with Engineer 1.

This engineer is the product voice. They also own the first concrete alternative frontend: a minimal CLI that exercises the same IPC contract, proving the UI is genuinely decoupled from the Runtime.

### Engineer 4: Infrastructure, Distribution, and Server (Rust + DevOps)

Owns: Build pipeline (cross-compilation, workspace structure), hardware detection, model download manager, auto-updater, crash reporting, logging, `SqliteStateStore` and `PostgresStateStore` implementations, telemetry (opt-in, local-only default), CI/CD, integration tests.

Also owns: `sovereign-server` — the Axum HTTP/WebSocket layer, `ServerApprovalChannel`, `TenantRuntime`, authentication middleware, Docker image, Helm chart. And the `RemoteApiProvider` and `HybridProvider` implementations, proving that `InferenceProvider` actually works as a swappable trait in practice, not just in theory.

This engineer is the first customer of the crate architecture. If the crate boundaries are wrong — if `sovereign-server` needs to reach into `sovereign-desktop`'s internals to function — that's a structural failure caught early.

---

## 13. Boundaries, Not Walls

This system is a product, not a platform. There is no plugin marketplace. There is no third-party runtime. There is no extension API versioned for backwards compatibility across years.

But there is something more durable: **every internal boundary is a trait with a clear contract.** This means:

- A company can fork the project, replace `EmbeddedLlamaCpp` with their proprietary inference cluster, and everything else works.
- A security-focused team can replace `SqliteStateStore` with an encrypted store and the Runtime doesn't change.
- A community contributor can write a `DomainPlanner` for legal workflows and drop it in.
- When the network-level protocols for verifiable agent orchestration mature, a `VerifiableStateStore` can emit cryptographic attestations of every task execution — and the Executor, Planner, and Router are completely unaware.
- A `VerifiableInferenceProvider` can wrap inference calls in TEE attestations without touching the orchestration layer.

The difference between a platform and an extensible product: a platform says "build on us." An extensible product says "take us apart and rebuild if you need to." The traits are not an invitation to third-party developers. They are a guarantee to the future that the system's assumptions are explicit and its components are replaceable.

This is how you build software that participates in an ecosystem that doesn't exist yet — not by predicting what that ecosystem will need, but by making your assumptions visible and your commitments minimal.

---

## 14. What This Doesn't Do (Intentionally)

**No multi-user mode in the desktop app.** The desktop product is a personal agent. One user, one machine. The server product handles multi-tenancy through namespace isolation — but this is API-level separation, not a collaborative environment. Organizations wanting shared agents with shared memory and collaborative workflows are a different product.

**No model training or fine-tuning.** Personalization comes from the memory system and RAG pipeline, not from weight modification.

**No plugin marketplace.** MCP provides capability extension. The `Tool` trait provides implementation extension. A marketplace introduces curation, trust, and security review problems that conflict with the design philosophy.

**No cloud fallback by default.** `RemoteApiProvider` exists and ships. It is opt-in and clearly labeled. The default is fully local.

**No blockchain, no tokens, no decentralized infrastructure in the client.** This is the client-side component. The protocol layers sit above, at the network level. The trait boundaries — particularly `StateStore` and `InferenceProvider` — are where those protocol layers will eventually connect. But the client doesn't presuppose their design.

---

## 15. Timeline

**Months 1-2:** Crate structure, trait definitions, `EmbeddedLlamaCpp`, `LlmRouter`, and a CLI harness built on `sovereign-core`. Validate: does routing across model sizes produce good results on consumer hardware?

**Months 3-4:** `LlmPlanner`, Executor, `SqliteStateStore`. Multi-step task execution against a test suite of 50 representative tasks. Measure: plan quality, execution success, replanning recovery.

**Months 5-6:** Tool runtime, built-in tools, MCP adapter, RAG pipeline. The system starts feeling like a real assistant. Simultaneously: `sovereign-server` scaffolding — Axum routes wrapping the same Runtime that the CLI uses. The server becomes the second proof that the trait boundaries hold.

**Months 7-8:** Tauri UI, setup wizard, conversation interface. `PostgresStateStore` for server deployments. Server multi-tenancy and authentication. Closed beta: desktop app with 50-100 non-technical users, server API with 3-5 startup design partners.

**Months 8-9:** `RemoteApiProvider`, `HybridProvider`, Docker image, Helm chart. Polish, cross-platform builds, auto-updater. Public release of both desktop and server.

---

## 16. Success Metrics

**Primary:** A non-technical user completes setup and successfully executes a multi-step task involving at least one tool — with no external help — within 30 minutes of downloading the desktop application.

**Secondary:** A developer forks the project, replaces one trait implementation with their own, and passes the full test suite — with no modifications to any other component — within one working day.

**Tertiary:** A startup engineer, given the Docker image and API docs, has sovereign-server running on their infrastructure and successfully completes a multi-step agentic task through the API — within one afternoon.

The first metric validates the consumer product. The second validates the architecture. The third validates the developer product.




You're identifying a real retention killer. Let me dig into what actually makes frontier model web search good and what the open-source options look like.This is a deep problem. The research reveals that what makes frontier model search _feel_ good is a multi-stage pipeline that most open-source setups completely skip. Let me lay out the gap and the fix.

**Why OpenWebUI's search feels terrible**

The typical self-hosted search flow is: user asks question → SearXNG returns 10 result snippets (titles + 150-character previews) → snippets get stuffed into the context → the model tries to answer from those tiny fragments. This is like trying to write a research paper by only reading the back covers of books. The model gets titles and fragments but never actually reads the pages. The result is vague, unsourced, often wrong.

Traditional SERP APIs like SerpAPI and Serper return only search metadata — titles, URLs, and short snippets of 150-300 characters. That's what SearXNG gives you too. The model is reasoning over scraps.

**What frontier models actually do**

Perplexity's architecture starts with query intent parsing — using an LLM to understand what the user is actually asking, not just the literal words. Then it dispatches the parsed query to a real-time search index, retrieves relevant pages, and feeds full content to the model for synthesis. The critical steps that most open setups miss:

First, **query decomposition**. A single user query triggers dozens of sub-searches behind the scenes. If someone asks "how does the new EU AI Act affect startups using open source models?", a frontier system breaks this into sub-queries: "EU AI Act 2026 provisions", "EU AI Act open source exemptions", "EU AI Act startup compliance requirements." Each sub-query captures a different facet. Microsoft's agentic retrieval decomposes complex questions into focused searches, uses conversation history to understand context, and executes them in parallel across knowledge sources.

Second, **full content extraction**. Not snippets — the actual page content. ChatGPT generates search queries, sends them to Bing's API, retrieves a short list of URLs, then fetches the full content of selected URLs at runtime and processes them directly for synthesis. The model reads the pages, not the previews.

Third, **reranking**. Perplexity employs a sophisticated three-layer reranking system that applies machine learning filters after initial retrieval, and if too few results meet quality thresholds, it can discard the entire result set. The initial search results are just candidates. A second pass scores them for relevance, authority, and freshness before the model ever sees them.

Fourth, **citation grounding**. The model is prompted to cite specific sources for each claim. This isn't just a UX feature — it disciplines the model's reasoning by forcing it to anchor every statement to retrieved evidence.

**The architecture for Sovereign's search**

This maps cleanly onto our existing design. The `web_search` tool becomes a multi-stage pipeline rather than a single API call:

**Stage 1: Query planning (Fast slot).** The Fast model takes the user's question and generates 2-5 targeted sub-queries. This takes <200ms and dramatically improves recall. A single well-decomposed query outperforms the user's raw question almost every time.

**Stage 2: Search execution (parallel).** Sub-queries execute in parallel against the search backend. For a self-hosted default, SearXNG is completely free with no query limits since it runs on your own infrastructure — but it needs to be configured properly, aggregating from multiple engines (Google, Bing, DuckDuckGo, Brave) to improve coverage. For users willing to use an API key, Tavily aggregates content from up to 20 sources per query, ranks them using proprietary AI, and delivers parsed content ready for LLM consumption, with 93.3% accuracy on SimpleQA. Brave Search API has an independent index of 35+ billion pages and recently launched an LLM Context API specifically for AI grounding at $5 per 1,000 queries. The `web_search` tool should support all of these through the `Tool` trait — SearXNG as the default, Brave/Tavily as optional upgrades. The user sees one toggle: "web search." The backend picks the best available provider.

**Stage 3: Content extraction (parallel).** For the top 5-8 results, fetch the full page content and extract clean text. This is the step most open setups skip entirely. The implementation uses an HTTP client with a readability-style extraction algorithm (Mozilla's Readability is open source and well-tested) that strips navigation, ads, and boilerplate, returning just the article content as clean text. Firecrawl is open source and can be self-hosted for extraction-heavy workflows, handling JavaScript-rendered pages and bot-protected sites. For Sovereign, a lighter approach works: a built-in Rust HTTP client + a readability port. No headless browser needed for 90% of pages.

**Stage 4: Reranking (Embed + Fast slot).** The extracted text chunks get embedded and scored against the original query's embedding for semantic relevance. Then the Fast model does a quick pass: "Given the user's question and these 8 extracted passages, rank them 1-8 by relevance and discard any that don't help answer the question." This produces a focused, high-quality context that the Primary model can actually reason over. Two-stage retrieval using an embedding model for first-stage candidates and a reranker for refinement significantly outperforms single-stage retrieval. Qwen3-Reranker or a cross-encoder model running locally handles this.

**Stage 5: Synthesis (Primary slot).** The Primary model gets the user's question, the top-ranked extracted passages with source URLs, and a system prompt requiring citation of sources. It synthesizes an answer grounded in the actual content of the pages, with inline citations.

**The full pipeline, timed:**

|Stage|What|Duration|
|---|---|---|
|Query planning|Fast model generates 3 sub-queries|~150ms|
|Search execution|3 sub-queries, parallel|~800ms|
|Content extraction|Top 6 URLs, parallel fetch + readability|~1.2s|
|Reranking|Embed + Fast model scores & filters|~300ms|
|Synthesis|Primary model reads passages, generates answer|~3-5s|
|**Total**||**~5-7s**|

This is competitive with Perplexity's response time. The quality difference versus "stuff 10 snippets in the prompt" is enormous — the model is reading full paragraphs from real pages rather than guessing from titles.

**Implementation in the crate structure**

This doesn't require a new trait. It's a richer implementation of the existing `web_search` tool. The `Tool` trait handles it:

```rust
pub struct WebSearchTool {
    search_backend: SearchBackend,  // SearXNG, Brave, Tavily
    extractor: ContentExtractor,    // Readability-based HTML→text
    inference: Arc<dyn InferenceProvider>,  // For query planning + reranking
}
```

The tool uses `InferenceProvider` internally for the query planning and reranking stages. This is dependency injection working as intended — the tool doesn't know which model it's using, just that it can call `complete()` and `embed()`.

**The search backend as a pluggable component**

```rust
pub enum SearchBackend {
    SearXNG { endpoint: String },
    Brave { api_key: String },
    Tavily { api_key: String },
    Serper { api_key: String },
    Multi { backends: Vec<SearchBackend> },  // query all, deduplicate, merge
}
```

The `Multi` variant is powerful: query SearXNG _and_ Brave simultaneously, deduplicate by URL, and merge results. More coverage, better recall, still parallel.

**What this means for the user**

Setup wizard offers: "Enable web search? [Yes/No]." If yes: "Use free built-in search, or enter an API key for better results?" Free path auto-configures SearXNG in a sidecar container (or a bundled Rust reimplementation of basic metasearch). API key path accepts Brave ($5/1000 queries) or Tavily. Either way, the multi-stage pipeline runs identically. The user never sees the pipeline stages. They ask a question, wait 5-7 seconds, and get a well-sourced answer with citations.

This is the difference between "local AI with bad search" and "local AI that actually competes with Perplexity." The model quality isn't the bottleneck — the retrieval pipeline is. Fix the pipeline and a 14B open model with good context produces answers that are genuinely comparable to frontier offerings. The model was never the weak link. The plumbing was.



You're right to push on this. Let me check what SearXNG actually requires and whether there's a simpler path.No. It's not. And I should have been more honest about this from the start.

Google is actively blocking SearXNG instances. A fresh install gets blocked after roughly 5 searches in a row, likely through TLS and HTTP2 fingerprinting. Google search on SearXNG frequently returns "Suspended: access denied," requiring service restarts that only temporarily fix the problem. Open WebUI users are hitting this exact issue — SearXNG search failures often happen silently, so the user doesn't even know results are missing. The limiter requires a Valkey/Redis database just to function, the setup involves Python virtual environments or Docker with careful configuration, and even when everything is configured correctly, upstream engines are actively fighting you.

SearXNG is a privacy tool for technically sophisticated users who are willing to maintain it. It is the opposite of "just works." I was wrong to propose it as the default.

**The honest reality of self-hosted search**

Here's the uncomfortable truth: there is no free, self-hosted, zero-configuration web search that reliably returns good results. Web search is one of the most expensive pieces of infrastructure on the internet. Google, Bing, and Brave spend billions maintaining their indexes. You can't replicate that in a bundled binary on someone's laptop. The idea that we'd embed a working search engine is fantasy.

This means the `web_search` tool needs a different architecture than the other tools. Email, calendar, files — those connect to the user's own infrastructure. Web search connects to _someone else's index_. There's no way around this dependency.

**What actually works**

The pragmatic answer is a tiered approach, and being honest with the user about what each tier gives them:

**Tier 1: DuckDuckGo HTML scraping (free, zero-config, no API key).** DuckDuckGo doesn't aggressively block programmatic access the way Google does. A lightweight HTTP request to DuckDuckGo's HTML endpoint, parsing the results, gives you 10-20 results with titles, URLs, and snippets — reliably, without authentication, without rate limiting at normal personal usage levels. The quality is lower than Google but vastly better than nothing. This is the zero-configuration default. The user enables web search, it works immediately. No API key, no setup. This becomes the baseline that ensures no user ever has a broken search experience.

**Tier 2: Brave Search API ($0 to start, API key required).** Brave runs an independent index of 35+ billion pages and recently launched an LLM Context API specifically designed for AI grounding. It's $5 per 1,000 queries, and they include free monthly credits to start. An API key is one field in a settings form. The quality jump from DuckDuckGo scraping to Brave is significant — better ranking, fresher results, and the LLM Context API returns content pre-formatted for model consumption.

**Tier 3: Tavily ($0 to start, API key required).** Tavily aggregates content from up to 20 sources per query, ranks using proprietary AI, and delivers parsed content ready for LLM consumption — achieving 93.3% accuracy on SimpleQA benchmarks. 1,000 free searches per month. This is the closest thing to "Perplexity-grade search as an API." The results come pre-extracted and pre-ranked, which means stages 3 and 4 of our pipeline (content extraction, reranking) are largely handled server-side.

**How this fits the design**

The `SearchBackend` enum becomes:

```rust
pub enum SearchBackend {
    /// Free, zero-config. HTTP scraping of DuckDuckGo.
    /// Reliable but lower quality. Always available as fallback.
    DuckDuckGo,

    /// Independent index, good quality, $5/1k queries.
    /// API key required. LLM Context API for pre-formatted content.
    Brave { api_key: String },

    /// AI-native search, best quality, pre-extracted content.
    /// API key required. 1000 free queries/month.
    Tavily { api_key: String },

    /// Self-hosted metasearch for users who want full control.
    /// Requires external SearXNG instance. Power users only.
    SearXNG { endpoint: String },
}
```

**The setup wizard flow:**

> "Enable web search?" **[Yes]** **[No]**
> 
> _If Yes:_ "Web search is enabled using DuckDuckGo (free, no account needed). For better results, you can add a search API key:" **[Brave Search — free to start]** **[Tavily — free to start]** **[Skip, use free search]**

That's it. Every user gets working search immediately. Users who care about quality add one API key. The pipeline adjusts automatically — DuckDuckGo results get the full content-extraction and reranking treatment locally because the snippets are sparse. Brave results are richer and need less local processing. Tavily results arrive pre-processed and skip most of the pipeline.

**The pipeline adapts to the backend**

This is where the multi-stage search pipeline we designed earns its keep across different quality tiers:

With DuckDuckGo (weakest source): query decomposition matters _more_ because the underlying search is less semantic. Content extraction matters _more_ because snippets are tiny. Local reranking matters _more_ because ranking quality is lower. The pipeline compensates for the weaker source by doing more work locally.

With Tavily (strongest source): query decomposition still helps but Tavily handles some of it internally. Content extraction is largely done server-side. Local reranking is light. The pipeline is faster because less local work is needed.

The same pipeline, different load distribution. The Fast model does more work when the search backend is weaker, less when it's stronger. The user gets the best possible result regardless of which tier they're on.

**What this means honestly**

The zero-config free tier (DuckDuckGo) won't match Perplexity. It can't — Perplexity has its own index and enormous infrastructure. But with the full local pipeline (query decomposition, content extraction, reranking, grounded synthesis), it will be _dramatically_ better than what OpenWebUI gives you today, and good enough that most users won't immediately feel the gap for everyday questions.

The Brave or Tavily tier, with a free API key that takes 30 seconds to create, will approach Perplexity quality for most queries. That's the realistic claim: not "we match Perplexity for free" but "add one API key and you're in the same ballpark, with full privacy and no subscription."

The system should be transparent about this. If the user is on the free tier and a search returns thin results, the system can note: "I found limited results with free search. You can improve search quality in Settings → Search." Not naggy, not upselling — just honest about the tradeoff they're making. Respect the user enough to tell them the truth about what they're getting.