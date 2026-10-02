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

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::LatencyClass;

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

/// One entry of the `<kind>:<rest>` step taxonomy, *as data*: the wire prefix, a
/// regex matching a whole valid `uses` of that kind, and a one-line summary. This
/// is the single source of truth a schema or UI reads to enumerate the step kinds
/// — it lives beside [`StepKind::parse`] (the one typing site) so the two cannot
/// drift. The tests below pin the invariant: every entry round-trips through
/// `parse`, and the catalog is *exhaustive* over the enum (a new variant fails to
/// compile the `prefix_of` match until it's added here too). §2.1: closed sets are
/// the enum's to own; the authoring schema *derives* from this, never re-lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireKind {
    /// The `<kind>` token before the first colon (`model`, `embed`, …).
    pub prefix: &'static str,
    /// A regex matching a full valid `uses` string of this kind, e.g.
    /// `^model:(fast|normal|extended|thoughtful|slow)$`. The authoring schema embeds
    /// this so the grammar constrains `uses` at generation time.
    pub uses_pattern: &'static str,
    /// One-line description for the authoring schema / consent surface.
    pub summary: &'static str,
}

impl StepKind {
    /// The `model:` latency vocabulary — the canonical OICP classes plus the
    /// friendly aliases [`parse_latency_class`] accepts. Single source of truth for
    /// the authoring schema's `model:` constraint; a test pins each entry parses,
    /// and that the `model` [`WireKind`] pattern still mentions each one.
    pub const MODEL_LATENCIES: &'static [&'static str] =
        &["fast", "normal", "extended", "thoughtful", "slow"];

    /// The `<kind>:<rest>` taxonomy as data — see [`WireKind`]. Ordered to match the
    /// enum. Adding a variant means adding an entry here (the exhaustiveness test
    /// fails otherwise).
    pub const WIRE_KINDS: &'static [WireKind] = &[
        WireKind {
            prefix: "model",
            uses_pattern: r"^model:(fast|normal|extended|thoughtful|slow)$",
            summary:
                "a completion from your local model at a latency class (model:fast|normal|extended)",
        },
        WireKind {
            prefix: "embed",
            uses_pattern: r"^embed:.+$",
            summary: "a daemon-routed embedding (embed:<model>, e.g. embed:default)",
        },
        WireKind {
            prefix: "tool",
            uses_pattern: r"^tool:.+$",
            summary: "a built-in or registered tool (tool:<id>, e.g. tool:web_fetch)",
        },
        WireKind {
            prefix: "mcp",
            uses_pattern: r"^mcp:[^:]+:.+$",
            summary: "a tool from a connected MCP server (mcp:<server>:<tool>)",
        },
        WireKind {
            prefix: "transform",
            uses_pattern: r"^transform:.+$",
            summary: "a deterministic in-process transform (transform:<name>, e.g. transform:json)",
        },
        WireKind {
            prefix: "recipe",
            uses_pattern: r"^recipe:.+$",
            summary: "a corpus ingest/enrich stage (recipe:<id>) — downloads + indexes a corpus",
        },
    ];
}

impl StepKind {
    /// Parse a `uses` string — the ONE place the wire form becomes typed.
    pub fn parse(uses: &str) -> Result<Self> {
        let (kind, rest) = uses.split_once(':').ok_or_else(|| {
            Error::Execution(format!(
                "step `uses` must be `<kind>:<name>` — got `{uses}`"
            ))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time exhaustiveness anchor: a new `StepKind` variant forces a new
    /// arm in `prefix_of`, which forces a new `WIRE_KINDS` entry (the equality
    /// assert below fails until it's added). This is the §2.1 trip-wire that keeps
    /// the data catalog from silently falling behind `parse`.
    fn prefix_of(k: &StepKind) -> &'static str {
        match k {
            StepKind::Model { .. } => "model",
            StepKind::Embed { .. } => "embed",
            StepKind::Tool { .. } => "tool",
            StepKind::Mcp { .. } => "mcp",
            StepKind::Transform { .. } => "transform",
            StepKind::Recipe { .. } => "recipe",
        }
    }

    #[test]
    fn wire_kinds_match_the_stepkind_variants_exactly() {
        // One minimal valid `uses` per kind. Every sample parses, its parsed
        // prefix matches the sample's prefix, AND the set of parsed prefixes
        // equals the WIRE_KINDS catalog — neither side can carry an extra kind.
        let samples = [
            "model:fast",
            "embed:default",
            "tool:web_fetch",
            "mcp:fs:write_file",
            "transform:json",
            "recipe:my-corpus",
        ];
        let mut parsed: Vec<&str> = samples
            .iter()
            .map(|u| {
                let k = StepKind::parse(u).unwrap_or_else(|e| panic!("`{u}` must parse: {e}"));
                let p = prefix_of(&k);
                assert!(
                    u.starts_with(&format!("{p}:")),
                    "`{u}` parsed to prefix `{p}`"
                );
                p
            })
            .collect();
        parsed.sort_unstable();
        let mut catalog: Vec<&str> = StepKind::WIRE_KINDS.iter().map(|w| w.prefix).collect();
        catalog.sort_unstable();
        assert_eq!(
            parsed, catalog,
            "WIRE_KINDS must enumerate exactly the StepKind variants"
        );
    }

    #[test]
    fn model_latencies_all_parse_and_the_pattern_mentions_each() {
        let model = StepKind::WIRE_KINDS
            .iter()
            .find(|w| w.prefix == "model")
            .expect("model WireKind");
        for lat in StepKind::MODEL_LATENCIES {
            // Each declared latency parses through the model path…
            let uses = format!("model:{lat}");
            StepKind::parse(&uses).unwrap_or_else(|e| panic!("`{uses}` must parse: {e}"));
            // …and the schema pattern still mentions it (so adding a latency to the
            // vocab without updating the pattern trips here, not in production).
            assert!(
                model.uses_pattern.contains(lat),
                "model uses_pattern `{}` is missing latency `{lat}`",
                model.uses_pattern
            );
        }
    }

    #[test]
    fn wire_kind_patterns_are_anchored() {
        // Each pattern is a full-string match (^…$) so the schema constrains the
        // whole `uses`, not a prefix substring.
        for w in StepKind::WIRE_KINDS {
            assert!(
                w.uses_pattern.starts_with('^') && w.uses_pattern.ends_with('$'),
                "`{}` pattern must be anchored: {}",
                w.prefix,
                w.uses_pattern
            );
        }
    }
}
