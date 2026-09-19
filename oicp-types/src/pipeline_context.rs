// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-pipeline context-injection settings — the flags a resolved pipeline
//! alias carries to whatever host injects context into the system prompt.
//!
//! # Why it lives here
//!
//! Until 2026-09-16 this struct sat in `serving-policy`, and the middleware
//! seam named it through `serving_policy::pipeline_aliases::PipelineContextConfig`.
//! Lifting the seam into `sovereign-contracts` (domains
//! `REVIEW-build-middleware-seam`) made that name illegal: `sovereign-contracts`
//! may not depend on `serving-policy`, because desktop and mobile reach
//! `serving-policy` transitively through `sovereign-contracts` and the
//! thin-surface closure refuses it (`quality/ARCH_LAYERS.toml` `[thin_surfaces]`).
//! The payload moves DOWN to the family-neutral floor instead, and
//! `serving-policy` re-exports it at the old path — the same shape
//! [`model_aliases`](crate::model_aliases) took when it left `commonwealth-core`
//! (2026-09-04): a value a host resolves belongs to the protocol rather than to
//! any one runtime.

use serde::{Deserialize, Serialize};

/// Inline context-injection settings that travel with a resolved pipeline and
/// are consumed by the host's context injector. Kept as data on the pipeline so
/// different aliases can share the same injector impl while producing different
/// preambles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineContextConfig {
    /// Prepend the session's recently-written notes to the system prompt.
    pub inject_notes: bool,
    /// Prepend the feature's `spec.md` to the system prompt.
    pub inject_spec: bool,
    /// When true, the `spec.md` injection is limited to the `## Invariants`
    /// section only (red-team case).
    pub inject_invariants_only: bool,
}

impl Default for PipelineContextConfig {
    fn default() -> Self {
        Self {
            inject_notes: true,
            inject_spec: true,
            inject_invariants_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_injects_notes_and_spec() {
        let cfg = PipelineContextConfig::default();
        assert!(cfg.inject_notes);
        assert!(cfg.inject_spec);
        assert!(!cfg.inject_invariants_only);
    }

    #[test]
    fn empty_toml_table_is_all_defaults() {
        // `#[serde(default)]` is the contract the alias table relies on: a
        // pipeline that declares no `[context]` block gets the defaults.
        let cfg: PipelineContextConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, PipelineContextConfig::default());
    }
}
