//! Per-agent translation layer. An adapter knows how to:
//!
//! 1. Build the tool descriptors the agent presents to the model
//!    (`tool_descriptors`). For native this IS the canonical
//!    descriptor set; for pi it's pi's tool names (read, write,
//!    bash, find, grep, ls) but each maps to a canonical
//!    primitive under the hood.
//! 2. Translate one observed tool call (name + raw JSON args) into
//!    a canonical `Primitive` *or* a typed reason for rejection
//!    (`TranslateOutcome::Unrecognized`).
//!
//! The adapter does NOT execute tools. Pi's executor is pi itself
//! (its built-in subprocess tool layer); native's executor is
//! `commonwealth_agent_tools::executor::execute` invoked by the
//! agent loop. The adapter's job is normalization only.

pub mod native;
pub mod pi;

use serde_json::Value;

use crate::primitive::{Primitive, PrimitiveKind};

/// Result of attempting to translate a raw agent tool call into a
/// canonical `Primitive`.
#[derive(Debug, Clone)]
pub enum TranslateOutcome {
    /// Translated cleanly to a canonical primitive.
    Canonical {
        canonical: Primitive,
        canonical_kind: PrimitiveKind,
    },
    /// Recognized as a tool the agent uses but not mappable to the
    /// canonical set (e.g. pi `bash` with an arbitrary command).
    /// The bench records this honestly — it does NOT silently drop
    /// the call. Cross-agent comparison stays well-defined because
    /// `Unrecognized` is itself an explicit variant.
    Unrecognized {
        /// The agent-side tool name (e.g. "bash").
        tool_name: String,
        /// One-line summary of what the agent passed; used in
        /// telemetry. Not parsed back into args.
        args_summary: String,
        /// Free-text reason. The pi adapter populates this with
        /// "shell command not in {cargo build, cargo test --test
        /// integration}".
        reason: String,
    },
    /// Agent emitted a tool name the adapter doesn't know about at
    /// all (typo, schema drift, model hallucinated a new tool).
    Unknown { tool_name: String },
}

impl TranslateOutcome {
    /// Convenience: extract the canonical kind if available.
    pub fn canonical_kind(&self) -> Option<PrimitiveKind> {
        match self {
            TranslateOutcome::Canonical { canonical_kind, .. } => Some(*canonical_kind),
            _ => None,
        }
    }
}

/// Adapter trait. Each runner that adapts an agent (pi, codex,
/// opencode) implements this.
pub trait AgentToolAdapter {
    /// Stable id, used in telemetry and the bench registry.
    fn id(&self) -> &'static str;

    /// Tool descriptors the agent presents to the model. For
    /// native this is the canonical set; for pi this is pi's
    /// shape (since pi defines its own tools).
    fn tool_descriptors(&self) -> Vec<Value>;

    /// Set of canonical `PrimitiveKind`s this adapter can produce
    /// from agent tool calls. Used by the cross-adapter
    /// equivalence test (ARCH §7.2: pin the convergence invariant
    /// with a test, not a comment).
    fn canonical_coverage(&self) -> Vec<PrimitiveKind>;

    /// Translate one observed tool call.
    fn translate(&self, tool_name: &str, raw_args: &Value) -> TranslateOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// THE convergence assertion: every adapter exposes the same
    /// set of canonical primitives. If a future PR adds
    /// `propose_signature` to native but forgets to teach pi how to
    /// recognize it (or rejects it), this fails.
    #[test]
    fn pi_and_native_expose_the_same_canonical_set() {
        let native_set: HashSet<PrimitiveKind> =
            native::Adapter.canonical_coverage().into_iter().collect();
        let pi_set: HashSet<PrimitiveKind> =
            pi::Adapter::default().canonical_coverage().into_iter().collect();
        assert_eq!(
            native_set, pi_set,
            "native and pi adapter canonical coverage drifted apart"
        );
    }
}
