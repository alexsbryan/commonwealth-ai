// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a host publishes about a tool it can run: identity, argument schema,
//! worked examples, and the four behavioural properties an agent reasons over
//! mechanically rather than by parsing prose.
//!
//! # Why this is layer 0
//!
//! `commonwealth-api`'s middleware INJECTS tools — `tool_injector` renders
//! these into the OpenAI-shaped `tools` array, `context_injector` renders the
//! same set into a system-prompt catalog grouped by `Effect` × `Scope` (the
//! grouping `sovereign tools list` prints). So the mesh API genuinely needs
//! this vocabulary, and while it lived in `sovereign-contracts` a layer-1
//! crate had to reach UP into layer 2 for it — 60 of the 98 references on the
//! `commonwealth -> sovereign` backflow edge (noun-convergence rung 2c,
//! family A). Moving the definitions down removes the edge;
//! `sovereign_contracts::types` re-exports every one of them at its historical
//! path, so this is a MOVE and not a rename.
//!
//! [`ToolSchema`](crate::ToolSchema) is the same subject at the wire's
//! resolution — name, description, parameters — because that is all an OpenAI
//! `tools` array carries. This module is the fuller descriptor a host keeps
//! about the same tool, and the two live beside each other on purpose.
//!
//! # What deliberately did NOT come down
//!
//! Sovereign's tool POLICY. `Permission` (which capability grants the consent
//! layer manages), `AuthorityClaim` (which store may declare itself the
//! authoritative answer surface), `ToolContext` (the ambient state a
//! `Tool::execute` call receives) and the `Intent`/`Operation`/`Effort` route
//! taxonomy all stay in `sovereign_contracts::types::routing`. The protocol
//! describes what a tool IS; the policy decides who may call it, when it wins
//! a route, and what it is handed at execute time.

use serde::{Deserialize, Serialize};

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
    /// Registry id — the key the host's tool registry dispatches on. A plain
    /// `String` because a tool registry is an OPEN set (ARCH §2/§4); sovereign
    /// spells the same type `sovereign_contracts::types::ToolId`.
    pub id: String,
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
