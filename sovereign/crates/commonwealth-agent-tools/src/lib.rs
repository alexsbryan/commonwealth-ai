//! Commonwealth canonical tool layer.
//!
//! Essential primitives that all agent runners (pi, codex, opencode,
//! native) translate to/from via per-agent adapters. The set is
//! small, composed, and structurally closed — adding a primitive
//! requires touching every module that matches on `Primitive`,
//! which forces the convergence test (does this primitive cover a
//! CLASS of behavior, not an instance?) to land in code review.
//!
//! Layering:
//!
//! - `primitive::Primitive` — the closed enum. Five variants.
//! - `descriptor` — JSON Schema per variant; this is what the model
//!   sees in the `tools` array of the OpenAI chat completion shape.
//! - `executor` — Rust impls (one async fn per variant). Pure of
//!   `&ExecCtx { workdir }`. No subprocess except the explicit
//!   `cargo_build` / `cargo_smoke` paths.
//! - `result::ToolResult` — structured envelope every executor
//!   returns. Closed `ToolError` enum so adapters can map per-agent
//!   error surfaces without inventing strings.
//! - `registry::Registry` — open table of executor functions keyed
//!   by primitive id; agent loops dispatch through it.
//! - `adapter` — per-agent translation layer. `native::Adapter` is
//!   identity; `pi::Adapter` is observer-only (pi keeps its tools,
//!   we record the canonical shape for cross-agent comparison).
//!
//! Design rationale and methodology lives in
//! `~/.claude/plans/autonomous-loop-tick-tingly-clock.md`. The
//! convergence criterion: every primitive closes a NAMED CLASS of
//! model failure, with an analytical argument for why it closes
//! variants we haven't seen yet.

pub mod adapter;
pub mod descriptor;
pub mod executor;
pub mod primitive;
pub mod registry;
pub mod result;
pub mod role;

pub use primitive::{
    AgentDoneArgs, AgentPlanArgs, HandoffToEvaluatorArgs, HandoffToImplementerArgs,
    InspectIntent, Primitive, PrimitiveKind, SmokeArgs, WriteFileArgs,
};
pub use result::{ToolError, ToolResult};
pub use registry::Registry;
pub use role::{Role, RoleDossier, RoleProfile};
pub use role::dossier::summarize as summarize_for_dossier;
