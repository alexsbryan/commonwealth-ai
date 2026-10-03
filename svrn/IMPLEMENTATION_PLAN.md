# Sovereign: Implementation Plan

Every phase produces a binary you can run and demonstrate. No phase is "infrastructure only." The critical path is: workspace skeleton → core types → working inference → trivial Router → CLI harness that sends a message and gets a response. Everything else layers on top of that working vertical slice.

---

## Dependency Graph

```
Phase 0: Workspace + Core Types
  │
Phase 1: Single-Slot Inference (EmbeddedLlamaCpp)
  │
Phase 2: InMemoryStateStore + Runtime Loop + CLI
  │
Phase 3: SQLite StateStore
  │
Phase 4: LLM Router (Fast + Primary slots)
  │
Phase 5: Executor + LLM Planner
  │
Phase 6: Tool System + Embed Slot + Permissions
  ├──────────────────────────┐
Phase 7: RAG Pipeline        Phase 8: Web Search
  │                           │
Phase 9: Memory System ──────┘
  │
Phase 10: Skills + SkillRegistry
  │
Phase 11: Axum Server ───── Phase 13: Multi-Backend Hybrid/Postgres
  │
Phase 12: Tauri Desktop + Personas
  │
Phase 14: MCP + Additional Tools
```

Phases 7 and 8 can run in parallel. Phase 12 can start as early as Phase 6 if a frontend engineer is available. Phases 13 and 14 can run in parallel with Phase 12.

---

## Phase 0: Workspace Skeleton and Core Types

**Delivers:** `cargo build` succeeds across all crates. `cargo test -p sovereign-core` passes. No functionality yet, but every crate compiles and the dependency graph is validated.

### Files Created

```
sovereign/
  Cargo.toml                          # workspace definition
  crates/
    sovereign-core/Cargo.toml
    sovereign-core/src/lib.rs         # all 5 traits, all shared types
    sovereign-inference/Cargo.toml
    sovereign-inference/src/lib.rs    # empty, re-exports core
    sovereign-store/Cargo.toml
    sovereign-store/src/lib.rs        # empty, re-exports core
    sovereign-tools/Cargo.toml
    sovereign-tools/src/lib.rs        # empty, re-exports core
    sovereign-server/Cargo.toml
    sovereign-server/src/main.rs      # empty main
    sovereign-desktop/Cargo.toml
    sovereign-desktop/src/main.rs     # empty main
```

### What Is Real

- All five trait definitions: `InferenceProvider`, `Router`, `Planner`, `Tool`, `StateStore`
- All shared types: `CompletionRequest`, `CompletionResponse`, `ProviderCapabilities`, `Speed`, `Depth`, `Intent` (all 6 variants), `Plan`, `Step`, `StepKind`, `StepOutput`, `StepInput`, `StepError`, `Message`, `Conversation`, `ConversationContext`, `Task`, `Memory`, `DocumentChunk`, `Permission`, `ToolDescriptor`, `ToolContext`, `ToolId`, `TaskId`
- The `Runtime` struct with its 6 fields (`inference`, `router`, `planner`, `tools`, `store`, `skills`) and the `handle_message` signature (body can be `todo!()`)
- The `Executor` struct with `run` and `execute_step` signatures (bodies can be `todo!()`)
- The `ToolRegistry` struct with `register`, `get`, and `descriptors` methods
- The `SkillRegistry` struct with `routing_hints`, `planner_templates`, `prompt_overrides`, `memory_rules` methods (bodies can return empty/defaults)
- The `Skill` struct: `id`, `name`, `version`, `routing: RoutingHints`, `planner_templates: Vec<PlanTemplate>`, `tool_config: ToolPreferences`, `prompts: PromptOverrides`, `memory_rules: MemoryConfig`
- The `BackendSelector` trait (internal to `sovereign-inference`, not on Runtime): `select(&self, request, backends) -> Result<usize>`
- The `HealthTracker` struct: `record_success`, `record_failure`, `is_healthy`, `health_score`
- The `ApprovalChannel` trait

### What Is Stubbed

Every leaf crate is an empty lib.rs that depends on `sovereign-core`.

### Key Decisions to Lock

- **Error type:** Define `sovereign_core::Error` enum using `thiserror`. Every trait method returns `Result<T, sovereign_core::Error>`.
- **Async runtime:** `tokio`, selected at binary crates only, not in library crates.
- **Serialization:** `serde` for all types. `Plan`, `Step`, `Intent`, `StepOutput` must roundtrip through JSON (the `StateStore` persists them as JSON columns).

### Verification

1. `cargo check --workspace` succeeds
2. `cargo test -p sovereign-core` passes (type construction, serialization roundtrips)
3. Dependency graph is acyclic: `sovereign-core` depends on nothing project-internal
4. `cargo doc --workspace --no-deps` generates documentation for all traits

---

## Phase 1: In-Process Inference (Single Slot)

**Delivers:** A Rust binary loads a single GGUF model file from disk and completes a prompt, printing the response to stdout. No routing, no planning, no storage. Just: load model, send prompt, get text back.

### Files Created/Modified

- `crates/sovereign-inference/src/lib.rs` — module structure
- `crates/sovereign-inference/src/embedded.rs` — `EmbeddedLlamaCpp`, initially one slot only
- `crates/sovereign-inference/src/ffi.rs` — llama.cpp binding layer (or use `llama-cpp-2` crate)
- `crates/sovereign-inference/src/hardware.rs` — hardware detection: GPU vendor, VRAM, system RAM
- `models.toml` at workspace root — model manifest (one profile initially)
- `crates/sovereign-inference/examples/complete.rs` — demo binary

### What Is Real

- llama.cpp FFI: load a GGUF file, run completion, return generated text
- Streaming via `complete_stream` returning `Pin<Box<dyn Stream<Item = Result<String>> + Send>>>`
- `InferenceProvider` trait implementation (at least `complete` and `complete_stream`)
- Basic hardware detection (GPU exists?, VRAM amount, system RAM)

### What Is Stubbed

- `embed()` returns `Err(Error::NotImplemented)`
- `capabilities()` returns hardcoded values
- No model download — user provides a GGUF path via CLI arg
- No slot management — one model, always loaded

### Risk

**This is the highest-risk phase.** The llama.cpp FFI integration determines whether the "single binary" promise is achievable. Options:
1. `llama-cpp-2` crate — fastest to start, may lag upstream
2. `llama-cpp-sys` for raw FFI + safe wrapper — more work, more control
3. Vendoring llama.cpp + `bindgen` — most control, most build complexity

Recommend starting with `llama-cpp-2` and migrating to vendored bindings only if limiting.

### Verification

1. `cargo run --example complete -- --model path/to/model.gguf --prompt "What is 2+2?"` — coherent response printed
2. Model loads without crashing
3. Streaming works: tokens appear incrementally
4. GPU detection reports correct values

### Dependencies

Phase 0.

---

## Phase 2: In-Memory StateStore + Runtime Loop + CLI

**Delivers:** A CLI binary where you type a message, it flows through `Runtime.handle_message()`, uses a passthrough Router, calls `InferenceProvider.complete()`, stores the conversation in memory, and prints the response. Multi-turn conversation works.

### Files Created/Modified

- `crates/sovereign-store/src/memory.rs` — `InMemoryStateStore` (HashMap-backed, not persistent)
- `crates/sovereign-core/src/runtime.rs` — `Runtime::handle_message()` body for `SimpleQuery`/`DeepQuery`
- `crates/sovereign-core/src/context.rs` — `ConversationContext` assembly: loading history, formatting into prompt
- `crates/sovereign-tools/src/registry.rs` — `ToolRegistry` with empty tool list
- A `PassthroughRouter` that always returns `Intent::SimpleQuery`
- A `NoOpPlanner` that always returns an error
- `crates/sovereign-cli/Cargo.toml` + `src/main.rs` — new thin crate, reads stdin, calls runtime, prints response

### What Is Real

- Full `handle_message` dispatch for `SimpleQuery`
- Conversation storage and retrieval through `StateStore` trait
- Multi-turn context: second message includes first message+response in context
- `Runtime` constructor assembling trait objects

### What Is Stubbed

- Router always returns `SimpleQuery`
- Planner not called
- No tools registered
- No memory/RAG
- `InMemoryStateStore` loses everything on restart

### Verification

1. `cargo run -p sovereign-cli -- --model path/to/model.gguf` — start the CLI
2. "Hello, who are you?" — coherent response
3. "What did I just ask you?" — model references previous message (context accumulation works)
4. 10 messages — no crash, no memory leak

### Dependencies

Phase 0 + Phase 1.

---

## Phase 3: SQLite StateStore

**Delivers:** Conversations persist across restarts. Quit the CLI, relaunch, previous conversation is still there. Full-text search over past messages works.

### Files Created/Modified

- `crates/sovereign-store/src/sqlite.rs` — `SqliteStateStore` implementing `StateStore`
- `crates/sovereign-store/src/migrations.rs` — full schema creation (conversations, messages, messages_fts, tasks, documents, memories, permissions, routing_log)
- `crates/sovereign-cli/src/main.rs` — switch to `SqliteStateStore`, add `--data-dir` flag

### What Is Real

- All `StateStore` conversation methods: `save_message`, `get_conversation`, `search_messages`
- FTS5 virtual table for message search
- WAL mode for concurrent reads
- Schema creation/migration

### What Is Stubbed

- `save_task`/`get_task` — schema exists, stores/retrieves but not exercised
- `save_memory`/`get_relevant_memories` — schema exists, returns empty
- `store_chunks`/`search_documents` — schema exists, not exercised
- `get_permission`/`set_permission` — schema exists, returns `None`

### Verification

1. Send 3 messages, quit, restart with same `--data-dir` — conversation retrievable
2. Search for a word from message 2 — `search_messages` returns it
3. SQLite file exists at expected path
4. `sqlite3 data/sovereign.db ".tables"` shows all expected tables

### Dependencies

Phase 2.

---

## Phase 4: LLM Router + Dual Model Slots

**Delivers:** The system classifies messages before responding. Simple questions use the Fast model (instant). Deep questions use the Primary model (slower, better). The user sees different latencies depending on complexity.

### Three-Stage Selection (Implemented)

The routing architecture separates three concerns:

1. **Intent classification** (Router) — Fast model classifies the message into an Intent variant
2. **Intent-to-slot mapping** (Runtime) — Static match: SimpleQuery→Fast, DeepQuery→Slow, ComplexTask→Planner
3. **Slot-to-model resolution** (InferenceProvider) — EmbeddedLlamaCpp maps Speed to a concrete model slot

Each layer knows less than the one above it.

### Two-Pass Classification (Implemented)

The `LlmRouter` uses a two-pass approach instead of a single monolithic classification prompt:

- **Pass 1**: Coarse binary — SIMPLE (general knowledge), REASONING (analysis/creativity), or ACTION (tools/multi-step). A single focused question the small model handles reliably.
- **Pass 2** (ACTION only): Refine — SINGLE tool call, MULTI-step plan, or KNOWLEDGE query. Only runs when pass 1 detects an action need.

Both passes use `Speed::Fast`, `max_tokens=5`, `temperature=0.0`. Total <200ms on a 1-3B model.

### Working Memory in Router Context (Implemented)

The Router's `ConversationContext` now includes working memory (current goal, known facts) in addition to recent messages. This gives the Router visibility into the conversational arc — "now email that" after 10 messages of research correctly classifies as ACTION because the working memory tracks `current_goal: "researching EU AI Act"`.

### Files Created/Modified

- `crates/sovereign-inference/src/embedded.rs` — dual-slot: Fast (always loaded) + Primary (on-demand with 60s idle timeout)
- `crates/sovereign-core/src/router.rs` — `LlmRouter` with two-pass classification, working memory context
- `crates/sovereign-core/src/runtime.rs` — intent-to-speed dispatch
- `crates/sovereign-cli/src/main.rs` — `--primary-model`, `--router` flags

### Deferred Router Improvements

These fit within the existing `Router` trait contract but require later infrastructure:

**Semantic skill routing hints (Phase 10 — Skills):** Skill trigger phrases should be matched by embedding cosine similarity rather than lexical substring matching. When the Embed slot is available (Phase 6), skill hints get embedded at registration time and compared against the message embedding. A similarity threshold of ~0.75 against the skill's intent description avoids false positives like "research" triggering on "research restaurants."

**Few-shot self-correction from routing_log (Phase 9+):** The `routing_log` table tracks classification outcomes and implicit feedback (user re-asks suggest misclassification). The Router should inject the N most recent corrections as few-shot examples into its classification prompt. This improves accuracy over time without model training, bounded cost per call.

### What Is Real

- Two-slot model management: Fast always loaded, Primary loaded on demand
- Two-pass classification with working memory context
- `SimpleQuery` and `DeepQuery` classification
- Primary model auto-unload after 60s idle
- CLI with `--primary-model` and `--router` flags

### What Is Stubbed

- `KnowledgeQuery` → falls back to `DeepQuery` (no RAG)
- `SimpleAction` → falls back to `SimpleQuery` (no tools)
- `ComplexTask` → "I can't do multi-step tasks yet"
- Embed slot not loaded
- Routing log writes not yet implemented
- Few-shot self-correction deferred

### Verification

1. "What is 2+2?" routes to Fast, responds in <1s
2. "Explain quantum computing's implications for cryptography" routes to Primary (visible latency difference)
3. Two-pass classification visible in logs: `[router] "..." → DeepQuery (pass1=B)`
4. Primary unloads after 60s idle (observable via logs/VRAM)
5. Working memory context included in classification prompt when available

### Dependencies

Phase 3.

---

## Phase 5: Executor + LLM Planner

**Delivers:** User asks a multi-step question. The system generates a visible plan, executes steps in topological order, and synthesizes a final response. Steps are `Reason` type only (no tool calls yet).

### Files Created/Modified

- `crates/sovereign-core/src/planner.rs` — `LlmPlanner` using Primary slot for structured output → `Plan` DAGs
- `crates/sovereign-core/src/executor.rs` — `Executor::run()` and `execute_step()` for `StepKind::Reason` and `StepKind::Branch`
- `crates/sovereign-core/src/plan.rs` — `Plan::topological_batches()`, `Step::resolve_inputs()` for interpolating prior outputs
- `crates/sovereign-core/src/runtime.rs` — `ComplexTask` dispatch calls Planner then Executor
- `crates/sovereign-cli/src/main.rs` — display plan steps and progress inline

### What Is Real

- Plan generation: Primary model → JSON `Plan` with steps and edges
- Topological sort into execution batches
- Parallel execution within batches (`futures::join_all`)
- Step input resolution: `{0.output}` replaced with actual prior outputs
- `StepKind::Reason` and `StepKind::Branch` execution
- Basic replanning on step failure
- Task persistence after each step

### What Is Stubbed

- `StepKind::Tool` — returns error
- `StepKind::UserInput` — placeholder
- `ApprovalChannel` — auto-approves everything
- Working memory — full history used as context

### Risk

**Structured output from small models is fragile.** The Planner will need robust JSON parsing, retry logic, and possibly constrained decoding (llama.cpp's GBNF grammar-guided generation). Plan for iteration.

### Verification

1. "Compare Python and Rust for systems programming, then recommend which to learn first" — generates plan, executes, produces synthesized answer
2. Plan has ≥2 steps
3. Independent steps execute in parallel (timestamps in logs)
4. Task appears in `tasks` table with `status = 'completed'`
5. Branch step follows correct path
6. Forced failure triggers replanning

### Dependencies

Phase 4.

---

## Phase 6: Tool System + Embed Slot + Permissions

**Delivers:** The agent uses tools. Two concrete tools: `shell` (sandboxed command execution) and `knowledge` (search over documents). Permission prompts appear in CLI.

### Files Created/Modified

- `crates/sovereign-inference/src/embedded.rs` — add Embed slot (third model, always loaded)
- `crates/sovereign-tools/src/shell.rs` — `ShellTool` implementing `Tool`
- `crates/sovereign-tools/src/knowledge.rs` — `KnowledgeTool` implementing `Tool`
- `crates/sovereign-store/src/sqlite.rs` — `store_chunks`, `search_documents` with `sqlite-vec`
- `crates/sovereign-core/src/executor.rs` — `StepKind::Tool` execution, permission checking, approval flow
- `crates/sovereign-core/src/executor.rs` — `StepKind::UserInput` execution
- `crates/sovereign-cli/src/approval.rs` — CLI approval channel: print preview, read y/n

### What Is Real

- Embed slot loads embedding model, `embed()` works
- `ShellTool`: sandboxed subprocess, returns stdout/stderr
- `KnowledgeTool`: embed query, vector search over `documents` table
- `sqlite-vec` integration
- Permission system: `get_permission`/`set_permission`, "Always allow / Allow once / Deny"
- `StepKind::Tool` in Executor: resolve tool, pass params, handle output
- `StepKind::UserInput`: prompt in CLI, wait for response

### What Is Stubbed

- RAG ingestion pipeline (documents inserted manually/test script)
- Web search, email, calendar tools
- MCP adapter

### Verification

1. "What files are in the current directory?" — calls `ShellTool` with `ls`, shows approval, returns listing
2. Permission persists: "always allow" survives restart
3. Manually inserted doc chunks → "What does document X say?" returns relevant content
4. Multi-step plan with tool step works
5. Denied permission → step skipped, plan continues

### Dependencies

Phase 5.

---

## Phase 7: RAG Pipeline

**Delivers:** Point at a directory. It parses, chunks, embeds, indexes automatically. Ask questions, get answers grounded in document content.

### Files Created/Modified

- `crates/sovereign-tools/src/rag/mod.rs` — pipeline orchestrator
- `crates/sovereign-tools/src/rag/parse.rs` — parsers: TXT, MD first; PDF, DOCX later
- `crates/sovereign-tools/src/rag/chunk.rs` — semantic chunking (paragraph boundaries, 512 tokens, 64-token overlap)
- `crates/sovereign-tools/src/rag/ingest.rs` — background ingestion: parse → chunk → embed → store
- `crates/sovereign-store/src/sqlite.rs` — hybrid search: `sqlite-vec` similarity + FTS5 keywords, merged and re-ranked
- `crates/sovereign-core/src/runtime.rs` — `KnowledgeQuery` path: embed → search → inject → synthesize
- `crates/sovereign-cli/src/main.rs` — `--ingest /path/to/docs` command

### What Is Real

- TXT and MD parsing
- Semantic chunking with overlap
- Embedding via Embed slot
- Hybrid search (vector + FTS5)
- `KnowledgeQuery` intent flow

### What Is Stubbed

- PDF/DOCX parsing (basic, using crates like `pdf-extract`, `docx-rs`)
- File watchers for incremental updates (manual re-ingest)
- Fast model reranking (score-based ranking only initially)

### Verification

1. Ingest 10 markdown files → ask question answerable only from them → correct answer
2. Question not in documents → model knowledge, no hallucinated doc references
3. `documents` table has expected chunk count
4. Hybrid search: keyword query finds via FTS5, semantic query finds via vector
5. 100 documents ingest in under 5 minutes

### Dependencies

Phase 6. **Can run in parallel with Phase 8.**

---

## Phase 8: Web Search Tool

**Delivers:** User asks about current events. System decomposes query, searches web, extracts content, reranks, synthesizes cited answer. Works zero-config with DuckDuckGo.

### Files Created/Modified

- `crates/sovereign-tools/src/web_search.rs` — `WebSearchTool` with multi-stage pipeline
- `crates/sovereign-tools/src/web_search/backends.rs` — `SearchBackend`: DuckDuckGo, Brave, Tavily, SearXNG
- `crates/sovereign-tools/src/web_search/extract.rs` — readability-style HTML → clean text
- `crates/sovereign-tools/src/web_search/rerank.rs` — embedding + Fast model reranking
- `crates/sovereign-tools/src/web_fetch.rs` — `WebFetchTool`: single URL fetch + extract

### Pipeline Stages

| Stage | What | Duration |
|---|---|---|
| Query planning | Fast model generates 2-5 sub-queries | ~150ms |
| Search execution | Sub-queries in parallel | ~800ms |
| Content extraction | Top 6 URLs, parallel fetch + readability | ~1.2s |
| Reranking | Embed + Fast model scores & filters | ~300ms |
| Synthesis | Primary model reads passages, generates cited answer | ~3-5s |
| **Total** | | **~5-7s** |

### Backend Tiers

- **DuckDuckGo** (free, zero-config, default) — reliable but lower quality
- **Brave Search API** ($5/1k queries, API key) — independent index, LLM Context API
- **Tavily** (1000 free/month, API key) — pre-extracted content, 93.3% SimpleQA accuracy
- **SearXNG** (self-hosted, power users) — lower priority

### Verification

1. "What happened in the news today?" → current, cited answer
2. Response includes inline citations with URLs
3. Query decomposition visible in logs
4. Content extraction: known URL → clean text matches page
5. Total pipeline <10s

### Dependencies

Phase 6. **Can run in parallel with Phase 7.**

---

## Phase 9: Memory System

**Delivers:** The system remembers facts about the user across conversations. Mention you're a backend engineer who prefers Rust → new conversation references this without being told.

### Files Created/Modified

- `crates/sovereign-core/src/memory.rs` — `WorkingMemory` struct + serialization into system prompt
- `crates/sovereign-core/src/memory.rs` — long-term extraction (post-conversation, Primary slot)
- `crates/sovereign-store/src/sqlite.rs` — `save_memory`, `get_relevant_memories` with embedding retrieval + confidence decay
- `crates/sovereign-core/src/runtime.rs` — inject relevant memories into context before each model call

### What Is Real

- Working memory: scratchpad compressed from history, injected into system prompt
- Long-term extraction: Primary model extracts durable facts after conversation ends
- Memory retrieval: embed context → find similar memories → inject top-N
- Confidence decay: 10%/month, pruned below 0.2
- Contradiction handling: new facts delete conflicting old ones

### Verification

1. Conversation 1: "I'm a backend engineer and prefer Rust over Go"
2. End conversation → memory extracted
3. Conversation 2: "What language for my next project?" → references Rust without being told
4. Memories table has entries with confidence values
5. Contradictory statement deletes old memory

### Router Few-Shot Self-Correction (included in this phase)

Once the routing_log has accumulated data and the memory system provides the retrieval infrastructure, the `LlmRouter` gains few-shot self-correction:

- Query the `routing_log` for the N most recent misclassifications (where `was_correct = 0`)
- Inject them as few-shot examples into the classification prompt: "Message X was classified as Y but should have been Z"
- This improves accuracy over time without model training, with bounded per-call cost
- The `routing_log` table and `was_correct` column already exist in the schema from Phase 3

### Dependencies

Phase 7.

---

## Phase 10: Skills + SkillRegistry

**Delivers:** Users can drop a `skill.toml` into `~/.svrnmesh/skills/` and the system's behavior changes — Router classification is biased, Planner uses templates instead of generating from scratch, prompts are shaped for the domain, and memory extraction is tuned. Ships with 3 bundled skills.

### Files Created/Modified

- `crates/sovereign-core/src/skills.rs` — `SkillRegistry` implementation: scan skills directory, parse TOML, merge hints
- `crates/sovereign-core/src/skills.rs` — `Skill` struct deserialization from TOML, `RoutingHints`, `PlanTemplate`, `PromptOverrides`, `MemoryConfig`
- `crates/sovereign-core/src/router.rs` — `LlmRouter` reads `skills.routing_hints()` and incorporates trigger phrases + confidence thresholds into classification prompt
- `crates/sovereign-core/src/planner.rs` — `LlmPlanner` reads `skills.planner_templates()` and uses matching templates as starting points
- `crates/sovereign-core/src/executor.rs` — Reason steps inject `skills.prompt_overrides()` into system message; Tool steps pass `skills.tool_config()` as preferences
- `crates/sovereign-core/src/memory.rs` — memory extractor reads `skills.memory_rules()` and appends domain-specific extraction guidance
- `skills/research-analyst/skill.toml` — bundled skill
- `skills/code-review/skill.toml` — bundled skill
- `skills/personal-assistant/skill.toml` — bundled skill
- `crates/sovereign-cli/src/main.rs` — `--skill activate/deactivate/list` commands

### What Is Real

- TOML parsing of `skill.toml` files from `~/.svrnmesh/skills/`
- `SkillRegistry` on Runtime: `routing_hints()`, `planner_templates()`, `prompt_overrides()`, `memory_rules()`
- Router integration: skill trigger phrases matched by embedding cosine similarity (not lexical substring) via the Embed slot. Skill intent descriptions are embedded at registration time. Messages are compared at classification time with a ~0.75 similarity threshold, avoiding false positives like "research" triggering on "research restaurants"
- Planner integration: matching template selected as starting point (Planner still adapts)
- Executor integration: skill prompts injected into Reason steps, tool preferences passed to Tool steps
- Memory integration: extraction prompt augmented with skill-specific rules
- Skill composition: multiple active skills merge routing hints (union), templates (best match), prompts (concatenation), memory rules (merge)
- 3 bundled skills ship with the product

### What Is Stubbed

- Skill validation (malformed TOML logs warning, doesn't crash)
- Community skill index (just a convention: copy a directory)

### Verification

1. Place `research-analyst/skill.toml` in skills dir → `--skill list` shows it
2. Activate research skill → "research the impact of X" routes to `ComplexTask` (would have been `DeepQuery` without skill)
3. With research skill active, multi-step research query uses the `multi_source_research` template (visible in plan output) instead of a generated-from-scratch plan
4. Synthesis response follows research skill's citation discipline prompt
5. Memory extraction after research conversation captures topics researched but not raw search results
6. Two skills active simultaneously compose without conflict (different domains)
7. Deactivate skill → behavior reverts to default

### Dependencies

Phase 9 (memory system must exist for memory rules integration). Router, Planner, Executor, and Memory must all be functional.

---

## Phase 11: Axum Server (sovereign-server)

**Delivers:** Same Runtime accessible over HTTP and WebSocket. `curl` sends a message and gets a response. WebSocket client receives streaming tokens.

### Files Created/Modified

- `crates/sovereign-server/src/main.rs` — Axum bootstrap, Runtime construction from config
- `crates/sovereign-server/src/routes.rs` — all REST endpoints from architecture doc
- `crates/sovereign-server/src/ws.rs` — WebSocket handler for streaming
- `crates/sovereign-server/src/approval.rs` — `ServerApprovalChannel` (tokio channels)
- `crates/sovereign-server/src/config.rs` — `ServerConfig` from TOML + env vars
- `crates/sovereign-server/src/auth.rs` — API key authentication middleware
- `crates/sovereign-server/src/tenant.rs` — `TenantRuntime` with namespace isolation
- `sovereign-server.toml` — example configuration

### REST Endpoints

```
POST   /v1/conversations
POST   /v1/conversations/{id}/messages
GET    /v1/conversations/{id}
GET    /v1/conversations
POST   /v1/tasks/{id}/approve
DELETE /v1/conversations/{id}
GET    /v1/tools
POST   /v1/tools/connect
POST   /v1/documents
POST   /v1/search
WS     /v1/conversations/{id}/stream
```

### What Is Stubbed

- `PostgresStateStore` — use `SqliteStateStore` initially
- `RemoteApiProvider`/`HybridProvider` — use embedded inference
- JWT/OAuth — API key auth only

### Verification

1. `curl POST /v1/conversations` → conversation ID
2. `curl POST /v1/conversations/{id}/messages` → assistant response
3. WebSocket: streaming tokens
4. Two API keys see isolated conversations
5. Approval flow works over both REST and WebSocket
6. 5 concurrent conversations, no crashes

### Dependencies

Phase 6 minimum. Ideally after Phase 10 for full feature set.

---

## Phase 12: Tauri Desktop App + Personas

**Delivers:** Native desktop app with chat interface, streaming responses, conversation history, task progress inline, approval cards. First-run persona selection configures skills and tools automatically.

### Files Created/Modified

- `crates/sovereign-desktop/src-tauri/` — Tauri Rust backend
- `crates/sovereign-desktop/src/` — Svelte frontend
- Components: ChatView, MessageBubble, TaskProgress, ApprovalCard, ConversationList, SettingsPanel, SkillManager
- `crates/sovereign-desktop/src-tauri/src/commands.rs` — Tauri IPC wrapping Runtime
- `crates/sovereign-desktop/src-tauri/src/approval.rs` — `TauriApprovalChannel`
- `crates/sovereign-desktop/src/lib/setup/` — persona-aware setup wizard

### Setup Wizard (Persona-Driven)

```
Step 0: "How will you use Sovereign?"

  ┌─────────────────────┐  ┌─────────────────────┐  ┌───────────────────┐
  │  Research &          │  │  Personal            │  │  Developer        │
  │  Analysis            │  │  Assistant            │  │                   │
  │                      │  │                       │  │  Show me the      │
  │  Private research    │  │  Email, calendar,     │  │  models, the      │
  │  across web and      │  │  files, tasks —       │  │  config, and the  │
  │  your documents.     │  │  managed by AI on     │  │  trait boundaries. │
  │                      │  │  your machine.        │  │                   │
  └──────────────────────┘  └───────────────────────┘  └───────────────────┘
```

**Research & Analysis** → activates `research-analyst` skill, enables `web_search` + `knowledge`, prompts for document directory, disables email/calendar at setup (available in settings).

**Personal Assistant** → no domain skill, prompts for capability toggles (email/calendar/files), enables `web_search`, general memory extraction.

**Developer** → skips friendly labels, shows model names/sizes/quantization, opens config panel (model selection, inference backend, StateStore, tool toggles), shows `sovereign.toml` location and skill directory.

The persona is resolved into concrete configuration (active skills + enabled tools + preferences) at setup time, stored via `StateStore`, then forgotten. No `if persona == X` branches in Runtime code.

### What Is Real

- Chat UI with streaming responses
- Conversation list and history
- Task progress (step-by-step inline)
- Approval cards for tool permissions
- Settings panel (including skill activation/deactivation)
- System tray
- Persona-aware setup wizard (3 paths, each configuring skills/tools/preferences)

### What Is Stubbed

- Model download manager (manual placement initially, then real download with progress bar)
- Auto-updater
- Notifications

### Verification

1. `cargo tauri dev` launches app
2. First launch → persona selection → appropriate setup flow
3. Research persona → `research-analyst` skill active, web search enabled
4. Developer persona → model details shown, config panel accessible
5. Streaming chat works
6. Multi-step task shows progress inline
7. Approval cards appear and function
8. Conversation list, search, settings all work
9. Skills manageable from settings panel

### Dependencies

Phase 2+ (Runtime). Can start as early as Phase 6 with a frontend engineer. Persona setup requires Phase 10 (skills).

---

## Phase 13: Multi-Backend HybridProvider + RemoteApiProvider + PostgresStateStore + OICP Wire-Up

**Delivers:** Server can use multiple inference backends with health tracking, OICP capability-aware selection, and intelligent fallback. External OpenAI-compatible endpoints with OICP support. Postgres for server deployments. This is the foundation for eventual compute coops.

### Pre-existing Infrastructure (built in earlier phases)

The following OICP foundations are already in place and do NOT need to be built in Phase 13:

- **OICP types** (`sovereign-core/src/oicp.rs`): `Capability`, `InferenceRequirements`, `ProviderManifest`, `OicpResponseMeta`, `MatchQuality`, scoring functions (`satisfies_required`, `score_preferred`)
- **CompletionRequest.oicp** field: populated by Runtime/Executor from active skill requirements
- **CompletionResponse.oicp_meta** field: ready to carry provider response metadata
- **SkillInferenceConfig**: skills declare `preferred_capabilities`, `required_capabilities`, `min_context_tokens`, `privacy` in their TOML manifests
- **SkillRegistry.inference_requirements()**: merges active skills into a single `InferenceRequirements`
- **BackendEntry**: has `is_local` flag and `oicp_manifest: Arc<RwLock<Option<ProviderManifest>>>` for cached manifests
- **CapabilityAwareSelector**: filters by required capabilities, scores by preferred, respects `LocalOnly` privacy, falls back to priority selector when no OICP data
- **BackendSelector trait** + 4 implementations: `PrioritySelector`, `CostMinimizingSelector`, `LatencyMinimizingSelector`, `LocalFirstSelector`
- **HealthTracker**: latency EWMA, error rate, availability tracking

### Files Created/Modified

- `crates/sovereign-inference/src/remote.rs` — **NEW**: `RemoteApiProvider` (any OpenAI-compatible API)
  - Implements `InferenceProvider` trait
  - `build_request_body()`: includes `oicp` key from `CompletionRequest.oicp` in outgoing HTTP request body
  - `parse_response()`: extracts `oicp` metadata from response into `CompletionResponse.oicp_meta`
  - `fetch_oicp_manifest()`: `GET {endpoint}/oicp/v1/capabilities` → `Option<ProviderManifest>` (returns None if provider doesn't support OICP)
- `crates/sovereign-inference/src/hybrid.rs` — **NEW**: `HybridProvider` as a multi-backend router
  - Uses `CapabilityAwareSelector` (with `PrioritySelector` fallback) as default selector
  - Background health check loop (every 30s): pings backends, calls `fetch_oicp_manifest()` on remote backends, updates cached manifests on `BackendEntry.oicp_manifest`
  - Implements `InferenceProvider` by delegating to selected backend
- `crates/sovereign-store/src/postgres.rs` — **NEW**: `PostgresStateStore`
- `crates/sovereign-store/src/migrations.rs` — Add OICP observability columns to `routing_log`:
  ```sql
  ALTER TABLE routing_log ADD COLUMN oicp_match_quality TEXT;
  ALTER TABLE routing_log ADD COLUMN oicp_model_id TEXT;
  ```

### HybridProvider Architecture

```rust
pub struct HybridProvider {
    backends: Vec<(Box<dyn InferenceProvider>, BackendEntry)>,
    selector: Box<dyn BackendSelector>,
}
```

`BackendEntry` already carries `health`, `priority`, `cost_per_token`, `is_local`, and `oicp_manifest`. The `HybridProvider` pairs each provider with its entry.

Default selector chain: `CapabilityAwareSelector { fallback: PrioritySelector }`. When a `CompletionRequest` carries OICP requirements (populated by the Executor/Runtime from active skills), the selector:
1. Filters backends by `required` capability thresholds and `min_context_tokens`
2. Respects `privacy: LocalOnly` by restricting to local backends
3. Scores remaining backends by `preferred` capabilities using cached OICP manifests
4. Falls back to priority ordering if no backend has an OICP manifest

### RemoteApiProvider OICP Integration

**Outgoing requests**: When `CompletionRequest.oicp` is `Some`, serialize it into the request body under the `oicp` key. Non-OICP providers ignore unknown keys per the OpenAI API spec.

**Incoming responses**: If the response body contains an `oicp` key, deserialize it into `CompletionResponse.oicp_meta`. Log `match_quality` to the routing log for observability.

**Manifest polling**: `fetch_oicp_manifest()` is called by `HybridProvider`'s background health loop. Returns `None` (not an error) if the provider doesn't support OICP.

### What Does NOT Need OICP Changes

- **`EmbeddedLlamaCpp`**: ignores `CompletionRequest.oicp` entirely. Uses `preferred_speed` as before. `CompletionResponse.oicp_meta` is always `None`.
- **`InferenceProvider` trait**: signature is unchanged. OICP travels inside `CompletionRequest`/`CompletionResponse`, not as separate trait methods.

### Future Coop Foundation

The v1 `HybridProvider` becomes the coop foundation through extension, not rewrite:

| v1 (ships now) | Future coop |
|---|---|
| `backends` configured at startup from TOML | `backends` populated by discovery protocol |
| `HealthTracker` monitors known endpoints | Also handles nodes joining/leaving |
| `BackendSelector` chooses among static options | Also factors trust scores + contribution credits |
| OICP manifests polled per-backend | Manifests aggregated across mesh |
| No accounting | `ContributionTracker` records GPU-hours |

### Verification

1. `RemoteApiProvider` pointed at vLLM → completes a prompt
2. `RemoteApiProvider` sends `oicp` key in request body when `CompletionRequest.oicp` is present
3. `RemoteApiProvider` parses `oicp` response metadata into `CompletionResponse.oicp_meta`
4. `RemoteApiProvider.fetch_oicp_manifest()` returns `Some(manifest)` from an OICP-aware provider, `None` from a non-OICP provider
5. `HybridProvider` with 2 backends: routes to primary, falls back to secondary on failure
6. `HybridProvider` background loop updates OICP manifests every 30s
7. `HybridProvider` with one OICP-aware backend + one non-OICP backend: routes OICP requests to the OICP-aware backend
8. `HybridProvider` with only non-OICP backends: falls back to priority selection
9. Skill with `[inference] required_capabilities.code = 2` → request routed to backend with `code >= 2` model
10. `HealthTracker` marks unhealthy backend after 3 consecutive failures, recovers after probe succeeds
11. `CostMinimizingSelector` prefers local over remote when both healthy
12. `LocalFirstSelector` never routes remote while local is healthy
13. `PostgresStateStore` passes same test suite as `SqliteStateStore`
14. Server boots with Postgres config + multi-backend inference, handles conversations

### Dependencies

Phase 11. **Can run in parallel with Phase 12 and 14.**

---

## Phase 14: MCP Adapter + Additional Tools

**Delivers:** Connect an MCP server → its tools appear in tool list, usable in plans. Email and calendar tools work with real accounts.

### Files Created/Modified

- `crates/sovereign-tools/src/mcp.rs` — MCP client, `McpToolAdapter`
- `crates/sovereign-tools/src/email.rs` — IMAP read, SMTP send
- `crates/sovereign-tools/src/calendar.rs` — CalDAV read/write
- `crates/sovereign-tools/src/file.rs` — scoped filesystem read/write
- `crates/sovereign-tools/src/compute.rs` — sandboxed Python execution

### Verification

1. Connect filesystem MCP server → tools appear in `GET /v1/tools`
2. Agent uses MCP tool in a plan
3. Email read/send works with real IMAP/SMTP (send always requires approval)
4. Calendar read/write works with CalDAV

### Dependencies

Phase 6. **Can run in parallel with Phase 12 and 13.**

---

## Risk Summary

| Phase | Risk | Mitigation |
|---|---|---|
| **1** (llama.cpp FFI) | **Highest.** Build complexity, FFI correctness, VRAM management | Start with `llama-cpp-2` crate, migrate to vendored only if limiting |
| **4** (slot management) | **High.** VRAM leaks, concurrent slot access, idle timeout | Extensive stress testing, VRAM monitoring in CI |
| **5** (structured output) | **Medium.** Small models unreliable at generating valid JSON DAGs | GBNF grammar-guided generation, robust parsing, retry logic |
| **8** (web search) | **Medium.** DuckDuckGo may block, content extraction varies | Tiered backends, graceful degradation |
| **10** (skill composition) | **Low-medium.** Two skills covering the same domain create ambiguity | Start simple (most users run 1-2 skills), grow merge logic from real usage patterns |

## The CLI Is Not Throwaway

`sovereign-cli` is the third proof (after desktop and server) that the Runtime is truly UI-independent. Keep it maintained throughout all phases. It is also the fastest development feedback loop — every feature is testable through the CLI before any UI work begins.
