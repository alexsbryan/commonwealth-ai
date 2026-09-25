// SPDX-License-Identifier: AGPL-3.0-or-later
//! Coverage-first synthesis (`SOVEREIGN_COVERAGE_FIRST`, default off).
//!
//! The turn's demand set is built and stamped against the evidence pool
//! BEFORE synthesis (`epistemic::build_demands` + `stamp_coverage`, in
//! `prepare_knowledge_query_plan`), but until this module only the
//! post-release ledger read it. These are its two pre-release readers, and
//! both read the same facets, so the gap the prompt names is the gap the
//! card names (one decider):
//!
//! - [`render_coverage_brief`] tells the synthesis model, as a computed fact,
//!   which named facets the passages hold and which they do not. The model is
//!   told what it has instead of being asked to judge sufficiency — the ANS
//!   board (7baf4da8f) declined `list-igch0076-mints` with "I don't have
//!   reliable information on the Kyparissia hoard" while the hoard sat at
//!   walk hop 0 and 3 of its 6 gold mints were in the chunks.
//! - [`GapTrigger::for_turn`] arms the Refinement card on an ANSWERED turn
//!   whose named entities were not found, where before the card fired only
//!   on abstention (`collaboration.rs`, I4-C) — so a partial answer and its
//!   gap arrive together instead of the gap existing only if the model gave
//!   up.
//!
//! The Query facet is never rendered: `stamp_coverage` marks it Retrieved
//! whenever the pool is non-empty, which establishes nothing about the
//! question. SubQuestion facets are left out too — they come from a
//! heuristic split and "every token in one chunk", so their absence is weak
//! evidence and would prime exactly the decline this exists to remove.
//! Absence is lexical (the lowercased surface form), so it is worded "not
//! found by name in these passages", never "not in your sources".

use crate::types::{CoverageLevel, Demand, DemandFacet};

/// Names listed per side, and characters per name — bounds the prompt cost.
const MAX_NAMED: usize = 8;
const MAX_NAME_CHARS: usize = 80;

/// `SOVEREIGN_COVERAGE_FIRST=1|true|on|yes` turns on both readers. Default
/// off until the ANS full arm and `svrn quality check` settle it
/// (`sovereign/DEFAULTS_LEDGER.md`).
pub(crate) fn coverage_first_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_COVERAGE_FIRST")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Facets whose coverage is a surface-form match the brief may state.
fn is_named(d: &Demand) -> bool {
    matches!(
        d.facet,
        DemandFacet::Entity | DemandFacet::Stance | DemandFacet::Section
    )
}

fn clip(text: &str) -> String {
    text.trim().chars().take(MAX_NAME_CHARS).collect()
}

fn named_facets(demands: &[Demand], found: bool) -> Vec<String> {
    demands
        .iter()
        .filter(|d| is_named(d) && (d.covered != CoverageLevel::Absent) == found)
        .map(|d| clip(&d.text))
        .take(MAX_NAMED)
        .collect()
}

/// The prompt block: named facets found and not found in the pool. Empty
/// when the question named nothing the stamp could check, so a turn with no
/// named facets gets a byte-identical prompt.
pub(crate) fn render_coverage_brief(demands: &[Demand]) -> String {
    let found = named_facets(demands, true);
    let missing = named_facets(demands, false);
    tracing::debug!(
        target: "coverage_first",
        found = found.len(),
        missing = missing.len(),
        "coverage brief rendered from the stamped demand set"
    );
    if found.is_empty() && missing.is_empty() {
        return String::new();
    }
    let mut out = String::from("COVERAGE (computed from the passages below, not a judgment):");
    if !found.is_empty() {
        out.push_str("\n- Named in these passages: ");
        out.push_str(&found.join("; "));
    }
    if !missing.is_empty() {
        out.push_str("\n- Not found by name in these passages: ");
        out.push_str(&missing.join("; "));
    }
    out
}

/// The card's ask for an answered turn: the named entities the pool never
/// matched. Entities only — the facet the question itself names.
fn uncovered_ask(demands: &[Demand]) -> Option<String> {
    let missing: Vec<String> = demands
        .iter()
        .filter(|d| d.facet == DemandFacet::Entity && d.covered == CoverageLevel::Absent)
        .map(|d| clip(&d.text))
        .take(MAX_NAMED)
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "Not found by name in the retrieved passages: {}",
        missing.join("; ")
    ))
}

/// What fires the post-answer Refinement card (`run_collaboration`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GapTrigger {
    /// Answered, nothing named went uncovered — no card.
    Answered,
    /// The gate classified the turn as an abstention (the I4-C signal).
    Abstained,
    /// Answered, but named entities were not found in the pool. Carries the
    /// deterministic ask; produced only under `SOVEREIGN_COVERAGE_FIRST`.
    Uncovered(String),
}

impl GapTrigger {
    /// The trigger for a turn whose stamped demands are in hand.
    pub(crate) fn for_turn(abstained: bool, demands: &[Demand]) -> Self {
        if abstained {
            return Self::Abstained;
        }
        if !coverage_first_enabled() {
            return Self::Answered;
        }
        match uncovered_ask(demands) {
            Some(ask) => {
                tracing::info!(
                    target: "coverage_first",
                    ask = %ask,
                    "answered turn carries an uncovered named entity — gap card armed"
                );
                Self::Uncovered(ask)
            }
            None => Self::Answered,
        }
    }

    /// The trigger read off persisted message metadata (the streaming
    /// post-processor): demands come from `epistemic_state.demands`. A
    /// message with no ledger has no demands, so it falls back to the
    /// abstention signal alone — the pre-change behaviour.
    pub(crate) fn for_metadata(abstained: bool, metadata: Option<&serde_json::Value>) -> Self {
        let demands: Vec<Demand> = metadata
            .and_then(|m| m.get("epistemic_state"))
            .and_then(|s| s.get("demands"))
            .and_then(|d| serde_json::from_value(d.clone()).ok())
            .unwrap_or_default();
        Self::for_turn(abstained, &demands)
    }
}

impl From<bool> for GapTrigger {
    fn from(abstained: bool) -> Self {
        if abstained {
            Self::Abstained
        } else {
            Self::Answered
        }
    }
}

#[cfg(test)]
#[path = "coverage_first/tests.rs"]
mod tests;
