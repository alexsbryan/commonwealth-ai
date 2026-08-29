// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool bundles — how a host declares WHICH families of tools its turn
//! registry carries, without any crate having to hold the whole list.
//!
//! # Why this exists
//!
//! `quality/TOPOLOGY.md` §10 phase 7b. The shared recipe
//! (`sovereign-runtime-recipe`) wrote its tool registry out as one hardcoded
//! sequence of `register` calls, so "adopt the shared recipe" meant "take
//! exactly these eleven tools". Measured 2026-08-25, `sovereign-server`
//! registered 31 by type name and the recipe 11, and the two sets were not
//! nested in either direction — the server had no `knowledge_lookup` and no
//! `attached_document_search`, the recipe had no code intel and no notes. So
//! adoption read as a regression and the phase stalled on a question that
//! looked like policy ("which twenty tools belong to every host?") and was
//! actually structure: **nothing could add a family without editing the
//! shared list**, which is the open/closed principle stated as a defect.
//!
//! A bundle inverts that. The recipe depends on this trait; a host depends on
//! the concrete bundles it wants and hands them over as values. Adding a
//! family touches the host that has the family, and no shared file at all.
//!
//! # Why the capability question answers itself
//!
//! A bundle is constructed with the collaborators its tools need — the SCIP
//! graph handle, the note store, the workspace root. It cannot register a
//! tool over a resource it was not given. That is the whole of the
//! "code intel over another tenant's workspace" concern: a multi-tenant host
//! can only contribute a [`ToolBundle`] built from a handle it actually owns,
//! and a host that owns no such handle cannot express the capability at all.
//! The privilege is gated by construction rather than by a policy flag
//! somebody has to remember to set (ARCH §7, §10).
//!
//! # Absence is reported, never defaulted
//!
//! A family a host deliberately does not carry is [`Withheld`] — a bundle
//! that registers nothing and says why. It exists because a missing element
//! of a `Vec` is an omission and an omission is indistinguishable from a
//! forgotten wire, which is exactly the failure `ShellAccess::Withheld` was
//! written to prevent (ARCH §18.3). Every outcome, present or absent, comes
//! back in a [`BundleReport`] the caller traces.

use async_trait::async_trait;

use crate::registry::ToolRegistry;

/// A composable family of tools a host contributes to the turn registry.
///
/// Implementors hold their own collaborators; the only thing the trait
/// exposes is "put your tools in this registry and tell me what happened".
/// That is the whole seam — interface segregation on purpose, so a bundle
/// never sees the host, the recipe, or the other bundles.
///
/// # The pattern this is an instance of
///
/// **Collapse the drive** (ARCH §10.7): N sites re-implement one sequence, so
/// extract ONE implementation and make the differences a parameter. This is the
/// *composition* form — the recipe names no tool, a host composes
/// `Vec<Box<dyn ToolBundle>>`, and adoption therefore stopped costing a host its
/// capabilities. The *trait-object* form is `sovereign_core::runtime::TurnSink`.
///
/// Copy this shape rather than carving a new leaf crate: a carve-out moves mass,
/// this deletes it. The ratchet is a census that only shrinks — see
/// `sovereign-core/tests/runtime_commission_census.rs`, where `UNSHARED_RECIPES`
/// reached empty and turn-registry divergence went 26 to 23.
#[async_trait]
pub trait ToolBundle: Send + Sync {
    /// Stable family name, for tracing and for the host census. Not a tool
    /// id — one bundle registers many.
    fn name(&self) -> &'static str;

    /// Register this family's tools. Async because a family may have to reach
    /// a socket to learn what it offers (external MCP servers do).
    ///
    /// A partial result is normal and is not an error: a family whose backing
    /// store will not open reports the tools it dropped and the reason, and
    /// the turn proceeds without them.
    async fn register_into(&self, registry: &mut ToolRegistry) -> BundleReport;
}

/// The user-facing tool families a surface lets someone switch on and off.
///
/// # Why this is an enum
///
/// It is a CLOSED set (ARCH §2), and until 2026-08-26 it was four hand-kept
/// copies of the same five strings — `default_enabled_tools()`, the desktop
/// setup flow, `SettingsPanel.svelte`'s option list, and a five-arm
/// `enabled_tools.iter().any(|t| t == "…")` match in the desktop bootstrap.
/// They had already drifted: the setup flow omitted `knowledge_lookup`, so a
/// user who completed setup with an empty list silently lost the knowledge
/// front door that `default_enabled_tools` documents as default-on. That is
/// the §10.6 split-brain, in the config that gates the tools.
///
/// [`Self::ALL`] is now the ONE list; the defaults and the setup flow derive
/// from it, and a census pins the TypeScript option ids against it.
///
/// # What it is NOT
///
/// Not a grouping of tools by implementation. That is
/// [`ToolBundle`] — a bundle is built from the collaborators its tools need,
/// so membership answers "what can this host physically provide". A family
/// answers "what has this person permitted". They are orthogonal, and
/// conflating them is what would force bundles to shatter into one tool each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolFamily {
    /// Run shell commands on this machine.
    Shell,
    /// Direct queries to a web search provider.
    Search,
    /// Read a specific URL.
    WebFetch,
    /// Read attached files and ingested documents.
    Document,
    /// One deliberate sweep across library, memory and notes.
    KnowledgeLookup,
}

impl ToolFamily {
    /// Every family. The ONE list — defaults, setup and the UI derive from it.
    pub const ALL: &'static [ToolFamily] = &[
        ToolFamily::Shell,
        ToolFamily::Search,
        ToolFamily::WebFetch,
        ToolFamily::Document,
        ToolFamily::KnowledgeLookup,
    ];

    /// The one spelling that reaches config files and the Tauri wire.
    pub fn wire_id(self) -> &'static str {
        match self {
            ToolFamily::Shell => "shell",
            ToolFamily::Search => "search",
            ToolFamily::WebFetch => "web_fetch",
            ToolFamily::Document => "document",
            ToolFamily::KnowledgeLookup => "knowledge_lookup",
        }
    }

    /// The one parser. `None` for a value no family claims — reported by
    /// [`ToolPermissions::from_wire_ids`] rather than dropped (ARCH §18.3).
    ///
    /// `knowledge` and `web_search` are accepted because the desktop
    /// bootstrap matched them as aliases for `search`. No UI has ever
    /// offered either, so they cannot be in a config this program wrote —
    /// they are honoured for a hand-edited file and named here rather than
    /// left as an undocumented accident.
    pub fn parse_wire(id: &str) -> Option<Self> {
        match id {
            "shell" => Some(ToolFamily::Shell),
            "search" | "knowledge" | "web_search" => Some(ToolFamily::Search),
            "web_fetch" => Some(ToolFamily::WebFetch),
            "document" => Some(ToolFamily::Document),
            "knowledge_lookup" => Some(ToolFamily::KnowledgeLookup),
            _ => None,
        }
    }
}

/// Which families this registry may register — the user's answer, held in the
/// ONE place that can act on it.
///
/// Absent (`ToolRegistry` with no permissions wired) means UNGATED, which is
/// what every non-desktop host wants and is byte-identical to the behaviour
/// before this type existed. A surface with a settings panel wires the user's
/// set instead.
#[derive(Debug, Clone, Default)]
pub struct ToolPermissions {
    permitted: std::collections::BTreeSet<ToolFamily>,
}

impl ToolPermissions {
    /// Everything on. The daemon and the hub server, which have no per-user
    /// tool switches, and which therefore never gate.
    pub fn all() -> Self {
        Self {
            permitted: ToolFamily::ALL.iter().copied().collect(),
        }
    }

    /// Read a config's `enabled_tools`. Returns the set AND every id no family
    /// claimed, so a typo in a hand-edited config is reported rather than
    /// silently disabling a tool (ARCH §18.3).
    pub fn from_wire_ids<S: AsRef<str>>(ids: &[S]) -> (Self, Vec<String>) {
        let mut permitted = std::collections::BTreeSet::new();
        let mut unknown = Vec::new();
        for id in ids {
            match ToolFamily::parse_wire(id.as_ref()) {
                Some(f) => {
                    permitted.insert(f);
                }
                None => unknown.push(id.as_ref().to_string()),
            }
        }
        (Self { permitted }, unknown)
    }

    /// May a tool declaring `family` be registered? A tool that declares NO
    /// family is not governed by any switch and is always permitted.
    pub fn permits(&self, family: Option<ToolFamily>) -> bool {
        match family {
            None => true,
            Some(f) => self.permitted.contains(&f),
        }
    }

    /// The permitted families, for a boot banner.
    pub fn families(&self) -> impl Iterator<Item = ToolFamily> + '_ {
        self.permitted.iter().copied()
    }
}

/// What one bundle actually contributed.
#[derive(Debug, Clone)]
pub struct BundleReport {
    /// The [`ToolBundle::name`] that produced this.
    pub bundle: &'static str,
    /// Registry ids now callable because of this bundle.
    pub registered: Vec<String>,
    /// What this bundle did NOT contribute, and why. Never empty for a
    /// [`Withheld`] family, and non-empty for a degraded one.
    pub withheld: Vec<Withholding>,
}

impl BundleReport {
    /// A report that contributed nothing yet, ready to accumulate.
    pub fn new(bundle: &'static str) -> Self {
        Self {
            bundle,
            registered: Vec::new(),
            withheld: Vec::new(),
        }
    }

    /// Record a registered id.
    pub fn registered(mut self, id: impl Into<String>) -> Self {
        self.registered.push(id.into());
        self
    }

    /// Record whatever `register_reporting` did — registered, or withheld by
    /// the user's switch. The ONE place a gated tool becomes a `Withholding`,
    /// so no bundle can report a tool it did not actually contribute.
    pub fn record(self, outcome: crate::registry::Registration) -> Self {
        match outcome {
            crate::registry::Registration::Registered(id) => self.registered(id),
            crate::registry::Registration::Gated { id, family } => self.withheld(
                id,
                format!(
                    "the `{}` family is switched off for this surface",
                    family.wire_id()
                ),
            ),
        }
    }

    /// Record something this bundle could not or would not contribute.
    pub fn withheld(mut self, what: impl Into<String>, why: impl Into<String>) -> Self {
        self.withheld.push(Withholding {
            what: what.into(),
            why: why.into(),
        });
        self
    }

    /// One line for a boot banner or a trace.
    pub fn summary(&self) -> String {
        if self.withheld.is_empty() {
            format!("{}: {} tools", self.bundle, self.registered.len())
        } else {
            let reasons: Vec<String> = self
                .withheld
                .iter()
                .map(|w| format!("{} ({})", w.what, w.why))
                .collect();
            format!(
                "{}: {} tools, withheld {}",
                self.bundle,
                self.registered.len(),
                reasons.join("; ")
            )
        }
    }
}

/// One thing a bundle did not contribute, with the reason a reader needs.
#[derive(Debug, Clone)]
pub struct Withholding {
    /// Tool id, or the family name when the whole family is absent.
    pub what: String,
    /// Why. A decision ("no shell in a long-lived daemon") or a degradation
    /// ("notes.db would not open") — both are things an operator must be able
    /// to find without reading the source.
    pub why: String,
}

/// A family this host deliberately does not carry.
///
/// The null object of [`ToolBundle`]. Present in the host's bundle list so
/// that the decision is a value someone wrote down, rather than a line
/// missing from a file — which is what makes a withheld capability
/// distinguishable from a forgotten one (ARCH §18.3).
pub struct Withheld {
    name: &'static str,
    reason: &'static str,
}

impl Withheld {
    /// Name the family and the reason it is not wired here.
    pub fn new(name: &'static str, reason: &'static str) -> Self {
        Self { name, reason }
    }
}

#[async_trait]
impl ToolBundle for Withheld {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn register_into(&self, _registry: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name).withheld(self.name, self.reason)
    }
}

/// Fold every bundle into one registry, in order, and return what each did.
///
/// Order matters only for duplicate ids: `ToolRegistry::register` keeps the
/// first registration, so an earlier bundle wins. Callers put the baseline
/// first.
pub async fn install(
    registry: &mut ToolRegistry,
    bundles: &[Box<dyn ToolBundle>],
) -> Vec<BundleReport> {
    let mut reports = Vec::with_capacity(bundles.len());
    for bundle in bundles {
        let report = bundle.register_into(registry).await;
        for w in &report.withheld {
            tracing::info!(
                target: "tool_bundle",
                bundle = report.bundle,
                what = %w.what,
                why = %w.why,
                "tool withheld"
            );
        }
        tracing::debug!(
            target: "tool_bundle",
            bundle = report.bundle,
            registered = report.registered.len(),
            "bundle installed"
        );
        reports.push(report);
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Two;

    #[async_trait]
    impl ToolBundle for Two {
        fn name(&self) -> &'static str {
            "two"
        }
        async fn register_into(&self, _r: &mut ToolRegistry) -> BundleReport {
            BundleReport::new("two").registered("a").registered("b")
        }
    }

    // `futures::executor::block_on` rather than `#[tokio::test]`: this crate's
    // dependency budget is the one a third-party package must be able to lift,
    // and `futures` is already in it.
    #[test]
    fn withheld_reports_the_reason_rather_than_registering_nothing_silently() {
        futures::executor::block_on(async {
            let mut registry = ToolRegistry::new();
            let bundles: Vec<Box<dyn ToolBundle>> = vec![
                Box::new(Two),
                Box::new(Withheld::new("shell", "no shell in a long-lived daemon")),
            ];

            let reports = install(&mut registry, &bundles).await;

            assert_eq!(reports.len(), 2);
            assert_eq!(reports[0].registered.len(), 2);
            assert!(reports[0].withheld.is_empty());
            // The absent family is present in the OUTPUT, which is the property:
            // a reader can tell "decided against" from "never wired".
            assert!(reports[1].registered.is_empty());
            assert_eq!(reports[1].withheld.len(), 1);
            assert_eq!(reports[1].withheld[0].what, "shell");
            assert!(reports[1].withheld[0].why.contains("long-lived daemon"));
            assert!(reports[1].summary().contains("withheld"));
        });
    }
}
