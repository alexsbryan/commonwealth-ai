// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SEC coverage card — the body of `GET /internal/corpus/{corpus}/coverage-card`
//! (sovereign-mesh corpus_catalog_http `coverage_card`). Computed by
//! `corpus_engine::enrichment::atlas::analysis::sec_facts::coverage_card`;
//! the shape lives here so a client can name it without linking the engine.
//! `AsOf` and `ConceptKind` are the two facts-store types the card embeds;
//! corpus-engine re-exports all six at their historical paths.

use serde::{Deserialize, Serialize};

/// The corpus's freshness anchor (F6): the reporting filing it was built
/// from. Periods ending after `latest_period_end` refuse by construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsOf {
    pub form: String,
    pub accession: String,
    pub filed: String,
    /// Latest period end date across every stored fact (ISO date).
    pub latest_period_end: String,
}

/// Closed set (ARCH §2): a concept is a flow over a period or a stock at
/// an instant. The renderer's concept map declares which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConceptKind {
    Duration,
    Instant,
}

/// What this corpus answers, over what period, as of which filing, and
/// what it structurally cannot answer — read from the store (§7.4).
///
/// Capability leads and boundaries sit beside it at equal weight
/// (§7.7(2)); the field order is the reading order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageCard {
    pub entity: String,
    pub ticker: String,
    pub cik: String,
    /// Capability, first (§7.7(2)). Concept id order is the store's
    /// `BTreeMap` order, so the card is deterministic.
    pub answers: Vec<AnsweredConcept>,
    /// The span across every answerable concept, e.g. `FY2015-FY2025`.
    /// Empty when the store carries no facts at all.
    pub period_label: String,
    /// Boundaries, as facts at equal weight (§7.7(2)) — never warnings.
    pub limits: Vec<CoverageLimit>,
    /// Always present (§7.7(5), F6): a corpus that cannot say how current
    /// it is cannot be trusted about periods.
    pub as_of: AsOf,
}

/// One concept the typed store answers authoritatively, with the periods
/// it actually carries facts for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnsweredConcept {
    pub id: String,
    pub label: String,
    pub kind: ConceptKind,
    /// `FY2025` for a single year, `FY2013-FY2025` for a span.
    pub period_label: String,
    pub fiscal_years: Vec<i32>,
}

/// A named boundary on what the store can answer. Closed set (ARCH §2):
/// each variant corresponds to a refusal the tool actually emits, which
/// is what makes the card "the refusal's voice at rest" (§7.7(4)) rather
/// than a separately-maintained disclaimer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// Pairs with [`SecRefusal::UnmappedConcept`] `consolidated_only`.
    Consolidated,
    /// Pairs with [`SecRefusal::UnmappedConcept`]: tags the filer reports
    /// that the concept map does not yet type.
    UntypedTags,
    /// Pairs with [`SecRefusal::BeyondAsOf`] (F6).
    BeyondAsOf,
}

/// A boundary stated as a fact. Deliberately carries no severity or
/// level field — see the module note above (§7.7(1)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageLimit {
    pub kind: LimitKind,
    pub statement: String,
}
