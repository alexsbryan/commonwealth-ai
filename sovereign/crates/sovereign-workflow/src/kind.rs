// SPDX-License-Identifier: AGPL-3.0-or-later
//! `StepKind` — the closed taxonomy of step kinds, parsed once from a `uses`
//! string.
//!
//! Per ARCH_PRINCIPLES §2.1 (type-safe dispatch over stringly-typed): the `uses`
//! string is a wire/authored form (§2.2 — it lives in TOML the user writes),
//! parsed at *this single boundary* into a typed value. Every downstream
//! consumer — the registry's resolver, the command's "does this need the
//! daemon?" check — switches on the enum and never re-parses the string. Adding
//! a kind is one variant plus the arms the compiler then forces; there is no
//! scattered `uses.starts_with("…")` that a new kind can silently slip past.
//!
//! That failure mode is not hypothetical: an earlier `uses.starts_with("model:")`
//! probe forgot `embed:`, so an embed-only workflow would have refused to
//! assemble the daemon provider. With `resources()` below as an exhaustive
//! match, that bug cannot compile.

use sovereign_core::error::{Error, Result};
use sovereign_core::oicp::LatencyClass;

use crate::model::ResourceNeed;

/// What a step's `uses` string resolves to — the single source of truth for the
/// `<kind>:<rest>` taxonomy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKind {
    /// `model:<class>` — a daemon-routed completion at an OICP latency class.
    /// The slot vocabulary *is* OICP's `LatencyClass` (`fast` | `normal` |
    /// `extended`), so a `model:` step builds a protocol-native request rather
    /// than leaning on the legacy `Speed` shim. Aliases `thoughtful`/`slow` map
    /// to `extended` (reasoning-heavy work).
    Model { latency: LatencyClass },
    /// `embed:<model>` — a daemon-routed embedding.
    Embed { model: String },
    /// `tool:<id>` — a built-in or already-registered tool.
    Tool { id: String },
    /// `mcp:<server>:<tool>` — an MCP tool, registered as `mcp_<server>_<tool>`.
    Mcp { server: String, tool: String },
    /// `transform:<name>` — a deterministic in-process transform.
    Transform { name: String },
    /// `recipe:<id>` — a coarse corpus ingest/enrich stage. References a cataloged
    /// recipe by id; runs it via the injected `CorpusInstaller` (which delegates to
    /// the existing corpus-install path). Recipe `[parameters]` come from the step's
    /// `params`.
    Recipe { id: String },
}

impl StepKind {
    /// Parse a `uses` string — the ONE place the wire form becomes typed.
    pub fn parse(uses: &str) -> Result<Self> {
        let (kind, rest) = uses.split_once(':').ok_or_else(|| {
            Error::Execution(format!("step `uses` must be `<kind>:<name>` — got `{uses}`"))
        })?;
        Ok(match kind {
            "model" => StepKind::Model {
                latency: parse_latency_class(rest)?,
            },
            "embed" => StepKind::Embed {
                model: rest.to_string(),
            },
            "tool" => StepKind::Tool {
                id: rest.to_string(),
            },
            "mcp" => {
                let (server, tool) = rest.split_once(':').ok_or_else(|| {
                    Error::Execution(format!(
                        "an `mcp:` step must be `mcp:<server>:<tool>` — got `{uses}`"
                    ))
                })?;
                StepKind::Mcp {
                    server: server.to_string(),
                    tool: tool.to_string(),
                }
            }
            "transform" => StepKind::Transform {
                name: rest.to_string(),
            },
            "recipe" => StepKind::Recipe {
                id: rest.to_string(),
            },
            other => {
                return Err(Error::Execution(format!(
                    "unknown step kind `{other}` in `{uses}`"
                )))
            }
        })
    }

    /// What this kind needs from the scheduler. Exhaustive over the enum, so a
    /// new variant cannot compile without declaring its resource need — this is
    /// the classifier that replaces scattered `uses.starts_with(…)` probes
    /// (e.g. "does this workflow need the daemon?").
    pub fn resources(&self) -> ResourceNeed {
        match self {
            StepKind::Model { .. } | StepKind::Embed { .. } => ResourceNeed::Inference,
            StepKind::Tool { .. } | StepKind::Mcp { .. } => ResourceNeed::Tool,
            StepKind::Transform { .. } => ResourceNeed::None,
            StepKind::Recipe { .. } => ResourceNeed::Install,
        }
    }
}

/// Parse a `model:` slot into an OICP `LatencyClass`. The canonical vocabulary
/// is the protocol's own (`fast`/`normal`/`extended`); `thoughtful`/`slow` are
/// kept as aliases for `extended` (§2.2 — friendly wire forms over the enum).
/// An unknown slot is a loud error, not a silent default.
fn parse_latency_class(slot: &str) -> Result<LatencyClass> {
    Ok(match slot {
        "fast" => LatencyClass::Fast,
        "normal" => LatencyClass::Normal,
        "extended" | "thoughtful" | "slow" => LatencyClass::Extended,
        other => {
            return Err(Error::Execution(format!(
                "unknown model latency `{other}` — use `fast` | `normal` | `extended` \
                 (aliases: `thoughtful`, `slow` → extended)"
            )))
        }
    })
}
