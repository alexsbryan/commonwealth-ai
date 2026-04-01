# Sovereign: Architecture Addendum

_Skills, personas, and the primitives that make scaling non-destructive._

---

## Preamble

The core architecture defines five trait boundaries and a Runtime that assembles them. That design is correct and this addendum does not modify it. What it adds are three things the core design implies but does not make explicit:

1. **A coordination layer above the traits** — Skills — that lets non-developers configure the system for specific domains without understanding its internals.
2. **A persona-aware surface** that translates "what are you here for?" into the right skill and tool configuration at first launch.
3. **Scaling primitives** inside the existing `InferenceProvider` boundary that make the eventual jump to networked inference incremental rather than architectural.

These are not speculative extensions. Each addresses a concrete gap between the current design and the stated goals: proliferation, immediate legibility to both engineers and non-technical users, and a scaling trajectory from one machine to many.

---

## 17. Skills: The Coordination Layer

### The Problem

The trait system gives developers five axes of extension. But the people who will determine whether Sovereign proliferates are not trait implementors — they are users who want a better experience for a specific kind of work, and contributors who want to share configurations that deliver that experience.

MCP provides tool-level extension: connect a new capability. Trait swaps provide subsystem-level extension: replace how inference or storage works. Neither provides _workflow-level_ extension — a coherent package that changes how the system behaves for a class of tasks across multiple components simultaneously.

A "research analyst" workflow needs all of the following at once:

- Router hints that recognize research-oriented queries and escalate them to `ComplexTask` rather than `SimpleQuery`.
- Planner templates that decompose research questions into multi-source search, cross-referencing, and cited synthesis — without burning an LLM call to reinvent this structure every time.
- Tool requirements and preferences — search must be available, and the skill can declare a quality floor.
- Prompt templates that shape how the Primary model reasons — citation discipline, source evaluation, conflict acknowledgment.
- Memory extraction rules tuned to the domain — what counts as a durable fact in research contexts differs from personal assistant contexts.

No single trait boundary gives you this. A skill spans boundaries. That is its purpose.

### What a Skill Is

A skill is a **declarative configuration bundle** that the Runtime consults at specific decision points. Skills do not contain executable code. They do not replace traits. They _parameterize_ the existing implementations.

```toml
# skills/research-analyst/skill.toml

[skill]
id = "research-analyst"
name = "Research & Analysis"
version = "0.1.0"
description = "Deep multi-source research with citations and source evaluation."

# ─── Router Hints ────────────────────────────────────────────
# These are weighted signals, not overrides. The LlmRouter incorporates
# them into its classification prompt when this skill is active.
[routing]
trigger_phrases = [
    "research", "investigate", "compare sources", "what do experts say",
    "find evidence", "literature on", "what's the consensus"
]
# When a trigger phrase matches, bias toward this intent.
default_intent = "ComplexTask"
# Confidence threshold below which the Router should ask for clarification
# rather than guess. Skills can tighten this for domains where
# misclassification is costly.
min_confidence = 0.75

# ─── Planner Templates ──────────────────────────────────────
# Named plan shapes the LlmPlanner can select instead of generating
# from scratch. The Planner still validates and adapts them — these
# are starting points, not rigid scripts.
[[planner.templates]]
name = "multi_source_research"
trigger = "User wants information synthesized from multiple sources"
steps = """
1. Decompose the question into 3-5 focused sub-queries.
2. Execute sub-queries in parallel via web_search.
3. Extract full content from the top results per sub-query.
4. Cross-reference findings: identify agreement, contradiction, and gaps.
5. Synthesize into a cited response. Every claim cites its source.
   If sources conflict, present both positions with credibility assessment.
"""

[[planner.templates]]
name = "document_analysis"
trigger = "User asks about content in their documents"
steps = """
1. Search the user's document corpus via the knowledge tool.
2. If document results are insufficient, supplement with web_search.
3. Synthesize with clear attribution: distinguish document-sourced
   claims from web-sourced claims.
"""

# ─── Tool Configuration ─────────────────────────────────────
[tools]
required = ["web_search"]
optional = ["knowledge"]

# Skill-specific tool preferences. The web_search tool reads these
# at execution time.
[tools.web_search]
# Minimum sub-queries for query decomposition. The default pipeline
# uses 1-3; research benefits from broader coverage.
min_sub_queries = 3
max_sub_queries = 5
# Prefer content-rich backends when available.
prefer_backends = ["tavily", "brave", "duckduckgo"]

# ─── Prompt Templates ───────────────────────────────────────
# Injected into the system prompt for inference calls made while
# this skill is active. These shape model behavior without
# replacing the base system prompt.
[prompts]
synthesis = """
You are a research analyst. Your standards:
- Every factual claim must cite its source by name.
- When sources conflict, present both positions and assess
  which has stronger evidence, explaining why.
- Distinguish between well-established consensus and emerging findings.
- If the evidence is insufficient to answer confidently, say so.
  Do not hedge with vague qualifiers — be specific about what is
  and isn't known.
"""

# ─── Memory Rules ────────────────────────────────────────────
# How the post-conversation memory extractor behaves when this
# skill was active during the conversation.
[memory]
extract_prompt_addendum = """
For research conversations, extract:
- Specific topics the user has researched (for future context linkage).
- Stated positions or conclusions the user has reached.
- Sources the user found particularly credible or dismissed.
Do NOT extract: intermediate search results, raw findings that
weren't synthesized into a conclusion.
"""
```

### SkillRegistry

```rust
pub struct Skill {
    pub id: String,
    pub name: String,
    pub version: String,
    pub routing: RoutingHints,
    pub planner_templates: Vec<PlanTemplate>,
    pub tool_config: ToolPreferences,
    pub prompts: PromptOverrides,
    pub memory_rules: MemoryConfig,
}

pub struct SkillRegistry {
    skills: Vec<Skill>,
    active: HashSet<String>,  // Which skills are currently enabled
}

impl SkillRegistry {
    /// Called by the Router before classification.
    /// Returns hints from all active skills, merged by priority.
    pub fn routing_hints(&self) -> MergedRoutingHints { /* ... */ }

    /// Called by the Planner before plan generation.
    /// Returns templates from the best-matching active skill.
    pub fn planner_templates(&self, intent: &Intent) -> Vec<PlanTemplate> { /* ... */ }

    /// Called by the inference layer before constructing prompts.
    /// Returns prompt overrides from the relevant active skill.
    pub fn prompt_overrides(&self, intent: &Intent) -> Option<PromptOverrides> { /* ... */ }

    /// Called by the memory extractor after conversation ends.
    pub fn memory_rules(&self) -> MergedMemoryConfig { /* ... */ }
}
```

The `SkillRegistry` is a new field on the `Runtime`:

```rust
pub struct Runtime {
    inference: Box<dyn InferenceProvider>,
    router: Box<dyn Router>,
    planner: Box<dyn Planner>,
    tools: ToolRegistry,
    store: Box<dyn StateStore>,
    skills: SkillRegistry,  // ← new
}
```

It is not a trait. There is no foreseeable alternative implementation of "a registry of declarative configuration bundles." It's a struct, following our own rule.

### How Skills Flow Through the Runtime

```
User message arrives
       │
       ▼
┌─────────────────────────────────┐
│ Router                          │
│  reads: skills.routing_hints()  │
│  effect: trigger phrases bias   │
│          classification toward  │
│          skill-preferred intent  │
└───────────────┬─────────────────┘
                │
                ▼
┌─────────────────────────────────┐
│ Planner                         │
│  reads: skills.planner_templates│
│  effect: selects a template as  │
│          starting point instead │
│          of generating from     │
│          scratch. Still adapts  │
│          to the specific query. │
└───────────────┬─────────────────┘
                │
                ▼
┌─────────────────────────────────┐
│ Executor                        │
│  reads: skills.prompt_overrides │
│  effect: Reason steps inject    │
│          skill prompts into     │
│          the system message.    │
│                                 │
│  reads: skills.tool_config      │
│  effect: Tool steps pass skill  │
│          preferences to tools.  │
└───────────────┬─────────────────┘
                │
                ▼
┌─────────────────────────────────┐
│ Memory Extractor (post-convo)   │
│  reads: skills.memory_rules()   │
│  effect: extraction prompt is   │
│          tuned to the domain.   │
└─────────────────────────────────┘
```

Skills are advisory at every stage. The Router can override a skill's preferred intent if its own classification is confident. The Planner can modify a template or discard it if the query doesn't fit. The Executor can ignore tool preferences if the preferred backend isn't configured. No skill can force the system into a state the underlying implementations don't support.

This is critical: skills are not code injection. They are structured input to existing decision points. A malicious or poorly written skill can degrade quality — it can bias the Router toward the wrong intent, or provide a bad prompt template. It cannot execute arbitrary code, access the filesystem, or bypass the permission system. The attack surface is "bad configuration," which is the same attack surface as a bad `models.toml`.

### Skill Composition

Multiple skills can be active simultaneously. The `SkillRegistry` merges their contributions:

- **Routing hints**: unioned. All trigger phrases from all active skills contribute to classification. If two skills claim the same intent for a message, the Router sees both signals — it doesn't need to arbitrate between skills, just between intents.
- **Planner templates**: selected by best match. The Planner sees templates from all active skills, tagged with their source. It selects the one whose `trigger` description best matches the current goal. If no template matches well, it falls back to pure LLM planning.
- **Prompt overrides**: concatenated with clear separation. If a "research-analyst" skill and a "legal-domain" skill are both active, the synthesis prompt includes both sets of instructions. This can create tension ("cite sources" + "caveat everything with jurisdiction") — which is actually desirable. The model resolves the tension in a way that pure configuration cannot.
- **Memory rules**: merged. Both skills' extraction guidance applies.

Composition isn't a solved problem and doesn't need to be. Two active skills covering different domains (research + coding) compose cleanly because they trigger on different query types. Two active skills covering the same domain create ambiguity — but that ambiguity is visible to the user ("You have two research skills active") and resolvable by them. Start simple: most users will run one or two skills. The merge logic can grow more sophisticated when real usage patterns reveal where it breaks.

### Skill Distribution

No marketplace. But skills need to move between people. The mechanism:

**A skill is a directory.** It contains `skill.toml` and nothing else. (Future versions might allow a `prompts/` subdirectory for longer prompt templates, or a `tests/` directory for validation cases.)

**Skills live in a well-known location:**

```
~/.sovereign/skills/
├── research-analyst/
│   └── skill.toml
├── code-review/
│   └── skill.toml
└── legal-research/
    └── skill.toml
```

**Installation is copying a directory.** `git clone`, a zip download, a shared folder — any mechanism that puts a directory in the right place. The system scans the skills directory on launch and makes all discovered skills available.

**Bundled skills ship with the product.** The installer includes 3-5 curated skills that cover the most common use cases. These live in the same directory and follow the same format. No special status.

**A community index is a git repository.** A `sovereign-skills` repo with a flat list of directories. Contributors submit PRs. Maintainers review for quality and safety (no prompt injection, reasonable configurations). Users clone it or download individual directories. This is the Homebrew model: curated but open, no infrastructure beyond git.

This is intentionally low-tech. A skill is 50-200 lines of TOML. The barrier to creation is low. The barrier to sharing is "push to a git repo." The barrier to installation is "copy a folder." When and if the ecosystem grows large enough to need a registry with search, versioning, and dependency resolution — that's a problem that earns its solution. Don't build the package manager before you have packages.

---

## 18. Personas: First-Run Configuration

### The Problem

The current setup wizard is generic: download models, toggle capabilities, start chatting. This fails both target users in different ways.

The engineer doesn't need a wizard at all — they want to see what's under the hood immediately. The wizard feels patronizing.

The researcher doesn't need to know about capabilities — they need to know what Sovereign does _for them_ that existing tools don't. The wizard feels like IT setup.

A single fork point fixes both problems without complicating the flow.

### First-Run Flow

```
Step 0: "How will you use Sovereign?"

  ┌─────────────────────┐  ┌─────────────────────┐  ┌───────────────────┐
  │  🔍 Research &      │  │  💬 Personal         │  │  ⚙️  Developer     │
  │     Analysis        │  │     Assistant        │  │                   │
  │                     │  │                      │  │  Show me the      │
  │  Private research   │  │  Email, calendar,    │  │  models, the      │
  │  across web and     │  │  files, tasks —      │  │  config, and the  │
  │  your documents.    │  │  managed by AI on    │  │  trait boundaries. │
  │  Nothing leaves     │  │  your machine.       │  │                   │
  │  your machine.      │  │                      │  │                   │
  └─────────────────────┘  └─────────────────────┘  └───────────────────┘

  (These are not exclusive. You can change this anytime.)
```

Each selection triggers a different configuration path — not a different binary, not a different Runtime, just different initial skill activation and capability defaults.

### What Each Persona Configures

**Research & Analysis:**

- Activates the `research-analyst` bundled skill.
- Enables `web_search` (DuckDuckGo tier, with a non-intrusive prompt to add Brave/Tavily key for better results).
- Enables `knowledge` tool with a prompt: "Point me at a folder and I'll index your documents."
- Enables the memory system with research-tuned extraction.
- Disables email, calendar, and other integration tools (available in settings, not shown at setup).
- First conversation begins with: "I'm ready. What would you like to research?"

**Personal Assistant:**

- Activates no domain skill (general-purpose behavior).
- Prompts for capability toggles: email (IMAP credentials), calendar (CalDAV), file access (directory scope).
- Enables `web_search` at the DuckDuckGo tier.
- Enables memory with general extraction rules.
- First conversation begins with: "Ready. Ask me anything, or connect your email and calendar in Settings."

**Developer:**

- Skips the progress-bar-with-friendly-label model download. Shows actual model names, sizes, and quantization.
- Opens a configuration panel after download: model selection, inference backend (local / remote / hybrid), StateStore choice, tool toggles.
- Exposes the `sovereign.toml` config file location.
- Activates no skill by default but shows the skill directory and how to create/install skills.
- First conversation begins with: "Runtime initialized. Type `/status` for system info."

### Persona as Stored Preference, Not Mode

The persona selection is stored in `dyn StateStore` as a user preference. It biases the Router's defaults and determines which skills are suggested — but it does not restrict access to anything. A researcher can enable email integration later. A developer can activate the research-analyst skill. The persona is a starting point, not a silo.

This means the Runtime has no concept of "persona" — it just has an active skill set, a set of enabled tools, and user preferences. The persona selection is resolved into these concrete configurations at setup time and then forgotten. No branches in the Runtime code check "if persona == researcher."

---

## 19. Inference Scaling Primitives

### The Problem

The core design handles two points on the scaling curve: single-machine (EmbeddedLlamaCpp) and static remote endpoint (RemoteApiProvider). The goal of eventually supporting dynamic, multi-provider inference — compute coops, federated GPU pools, heterogeneous clusters — requires primitives that the current design doesn't include but also doesn't preclude.

The principle from the core design applies: **no trait exists speculatively.** We don't build `InferenceNetwork` today. But we do need to make sure the `HybridProvider` — which ships in v1 — is robust enough to serve as the foundation that networked inference builds on later.

### What HybridProvider Must Handle

The current `HybridProvider` in the core design is sketched as a simple local/remote pair with a fallback policy. That's insufficient even for v1 use cases. A startup running `sovereign-server` might have:

- A local GPU for the Fast slot.
- Two remote endpoints: a self-hosted vLLM cluster for the Primary slot, and a commercial API (Anthropic, OpenAI) as overflow.
- Different latency, cost, and reliability characteristics for each.

The `HybridProvider` needs to be a **multi-backend router**, not a two-backend switch.

```rust
pub struct HybridProvider {
    backends: Vec<BackendEntry>,
    selector: Box<dyn BackendSelector>,
}

struct BackendEntry {
    provider: Box<dyn InferenceProvider>,
    name: String,
    health: Arc<HealthTracker>,
    priority: u32,             // Static preference ordering
    cost_per_token: Option<f64>, // For cost-aware routing
}

/// Selects a backend for a given request.
/// This is NOT a trait on the Runtime — it's internal to HybridProvider.
/// Making it a trait here is justified because backend selection genuinely
/// varies: cost-minimizing, latency-minimizing, privacy-maximizing, and
/// round-robin are all real strategies a single deployment might switch between.
#[async_trait]
pub trait BackendSelector: Send + Sync {
    async fn select(
        &self,
        request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize>;  // index into backends
}
```

### Health Tracking

Every backend gets a `HealthTracker` that records:

- **Latency**: exponentially weighted moving average of response times.
- **Error rate**: sliding window of failures over the last N requests.
- **Availability**: is the backend currently reachable?
- **Capacity**: if the backend reports queue depth or available slots, track it.

```rust
pub struct HealthTracker {
    latency_ewma_ms: AtomicU64,
    error_count: AtomicU32,
    request_count: AtomicU32,
    last_success: AtomicU64,    // unix timestamp
    last_failure: AtomicU64,
    available: AtomicBool,
}

impl HealthTracker {
    pub fn record_success(&self, latency_ms: u64) { /* ... */ }
    pub fn record_failure(&self) { /* ... */ }
    pub fn is_healthy(&self) -> bool { /* ... */ }
    pub fn health_score(&self) -> f64 {
        // 0.0 = dead, 1.0 = perfect.
        // Composite of error rate, latency relative to baseline,
        // and time since last success.
        /* ... */
    }
}
```

The health tracker runs a background probe: a lightweight ping (or a minimal completion request) every 30 seconds for backends that haven't been used recently. This means the `HybridProvider` always has a current view of backend health without waiting for a user request to discover that an endpoint is down.

### Backend Selection Strategies

```rust
/// Use the highest-priority healthy backend. Fall through on failure.
pub struct PrioritySelector;

/// Minimize estimated cost. Prefer local, then cheapest remote.
pub struct CostMinimizingSelector;

/// Minimize estimated latency. Use health_score * inverse_latency.
pub struct LatencyMinimizingSelector;

/// Never send requests to remote backends unless all local backends
/// are unhealthy. For privacy-sensitive deployments.
pub struct LocalFirstSelector;
```

These are concrete structs implementing `BackendSelector`. The default `HybridProvider` configuration uses `PrioritySelector` (prefer local, fall back to remote). A cost-sensitive deployment switches to `CostMinimizingSelector`. A privacy-focused deployment uses `LocalFirstSelector`.

### Why This Is the Right Foundation for Coops

A compute coop, when it eventually exists, is a `HybridProvider` whose `backends` list is **dynamic** rather than static. The pieces that change:

|v1 (ships now)|Future coop|
|---|---|
|`backends` is configured at startup from TOML|`backends` is populated by a discovery protocol|
|`BackendEntry.provider` is a `RemoteApiProvider` pointing at a known endpoint|`BackendEntry.provider` is a `RemoteApiProvider` whose endpoint was discovered at runtime|
|`HealthTracker` monitors known endpoints|`HealthTracker` monitors discovered endpoints + handles nodes joining/leaving|
|`BackendSelector` chooses among static options|`BackendSelector` also factors in trust scores and contribution credits|
|No accounting|A `ContributionTracker` records GPU-hours consumed and provided|

Every component in the left column becomes the corresponding component in the right column through extension, not replacement. The `HybridProvider` struct gains a `discovery: Option<Box<dyn DiscoveryService>>` field. The `BackendEntry` gains a `trust_score: Option<f64>` field. The `BackendSelector` implementations gain access to trust and contribution data. No existing code is rewritten.

This is why the `HybridProvider` must be well-designed in v1 even though coops don't exist yet. It's not premature abstraction — multi-backend inference with health tracking is a real v1 requirement for `sovereign-server`. But by building it as a proper multi-backend router rather than a simple two-way switch, the coop extension becomes a feature addition rather than a redesign.

### What We Explicitly Defer

- **Discovery protocols.** How nodes find each other. mDNS for local networks, DHT for the internet, or something built on libp2p. TBD when the use case is concrete.
- **Trust verification.** TEE attestations, inference proofs, or reputation systems. The crypto and hardware ecosystem isn't ready. The `trust_score` field on `BackendEntry` is a hook, not a commitment to a specific verification mechanism.
- **Economic accounting.** Contribution credits, billing, or reciprocity tracking. This is a coordination problem that depends on community structure. The `HealthTracker` pattern generalizes to a `ContributionTracker`, but the specific metrics and policies are undefined.
- **The `InferenceNetwork` trait.** It will exist someday. Its shape depends on which discovery, trust, and accounting mechanisms win. Defining it now would be guessing.

What we build now: a `HybridProvider` that handles multiple backends with health tracking, configurable selection strategies, and graceful degradation. This is useful on day one and extensible toward anything the network layer might eventually need.

---

## Summary: What Changes in the Runtime

```rust
pub struct Runtime {
    inference: Box<dyn InferenceProvider>,
    router: Box<dyn Router>,
    planner: Box<dyn Planner>,
    tools: ToolRegistry,
    store: Box<dyn StateStore>,
    skills: SkillRegistry,  // ← Added: coordination layer above traits
}
```

One new field. No new traits. The `SkillRegistry` is a struct that reads declarative TOML bundles and exposes their contents to the Router, Planner, Executor, and memory extractor at their existing decision points.

The persona system is not represented in the Runtime at all. It is a first-run UX flow that resolves into a skill configuration and a set of user preferences, both stored through `dyn StateStore`. After setup, it's gone.

The inference scaling primitives live inside `HybridProvider`, which is an implementation of the existing `InferenceProvider` trait. The `BackendSelector` trait is internal to `HybridProvider` and not exposed to the Runtime. No new trait boundary on the Runtime. No new crate. The `sovereign-inference` crate gains a more capable `HybridProvider` with health tracking and pluggable selection — a richer implementation behind the same interface.

The five trait boundaries hold. The system gains a coordination layer above them (skills), a UX layer above that (personas), and a richer implementation beneath them (multi-backend inference). The architecture scales in all three directions without modifying its core contracts.