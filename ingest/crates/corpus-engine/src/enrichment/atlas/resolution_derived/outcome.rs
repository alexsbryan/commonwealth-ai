// SPDX-License-Identifier: AGPL-3.0-or-later
//! One atom's derivation as the build keeps it beside the atlas
//! (`atlas/derived_decisions.jsonl`), and what one stage did per attribute.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// Which side of RESOLVE a derivation runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveStage {
    BeforeResolve,
    AfterResolve,
}

/// What a derivation gave one atom. Closed: an absence says which kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DerivedOutcome {
    Decided {
        values: Vec<String>,
        /// The `from` entry that decided, for a fold whose order matters.
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<usize>,
        /// The documents the decided values came through.
        documents: Vec<String>,
        /// Other values later inputs reached, which the decision outranked.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        superseded: Vec<String>,
    },
    /// No input reached a value.
    Nothing,
    /// Every input that reached anything reached several values.
    Ambiguous { values: Vec<String> },
    /// `agree`: the inputs reached different values.
    Conflict { values: Vec<String> },
    /// `most`, `earliest`, `latest`: several values share the top place.
    Tie { values: Vec<String> },
    /// `earliest`, `latest`: values were reached, none through a dated document.
    Unordered,
    /// A declared protocol could apply, but a requested qualification or a
    /// total report-time order is unavailable.
    Pending {
        values: Vec<String>,
        reasons: Vec<String>,
    },
}

impl DerivedOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            DerivedOutcome::Decided { .. } => "decided",
            DerivedOutcome::Nothing => "nothing",
            DerivedOutcome::Ambiguous { .. } => "ambiguous",
            DerivedOutcome::Conflict { .. } => "conflict",
            DerivedOutcome::Tie { .. } => "tie",
            DerivedOutcome::Unordered => "unordered",
            DerivedOutcome::Pending { .. } => "pending",
        }
    }
}

/// Replayable provenance for one recipe-declared scalar protocol projection.
#[derive(Debug, Clone, Serialize)]
pub struct DerivedProtocolAudit {
    /// Content identity of the fold, its paths, qualifications and rule data.
    pub rule_fingerprint: String,
    /// Report time that selected the current state; never the transition's effective time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of_report_time: Option<String>,
    /// Effective-time values on the selected reports, retained without ordering them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effective_times: Vec<String>,
    /// Rules that matched or remained possible under missing qualifications.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rule_ids: Vec<String>,
    /// Every claim reached through the assigned-subject path, including excluded inputs.
    pub basis_claims: Vec<String>,
    /// The input claim → resolved record assignment used by this projection.
    pub assignment_dependencies: BTreeMap<String, String>,
    /// Supported states that remain possible when the fold is pending or conflicting.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// Every reached claim, its source document, citation, field evidence and disposition.
    pub history: Vec<Value>,
    /// Why no unique state was projected, if applicable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// One atom's derivation, as the build keeps it beside the atlas.
#[derive(Debug, Clone, Serialize)]
pub struct DerivedValue {
    pub stage: DeriveStage,
    #[serde(rename = "type")]
    pub type_name: String,
    pub attribute: String,
    pub atom: String,
    pub derived: String,
    #[serde(flatten)]
    pub outcome: DerivedOutcome,
    /// Atoms a `[!set]` filter dropped on the way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    /// Readings of a reference that name no record: kept here, never held.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedReading>,
    /// Records a reference names that the atom holds though their membership
    /// in an exclusion set over the type is unjudged (a condition's attribute
    /// absent, `conditions_hold` is `None`): served, and said so here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unjudged: Vec<String>,
    /// The value the atom held before, when the derivation changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<Value>,
    /// Rule, source, and assignment lineage for a protocol fold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<DerivedProtocolAudit>,
    /// The derivation as the value's source (C3). Code computes it from the
    /// declared path and fold, over inputs whose own precisions they carry;
    /// no precision of the fold's output is declared or estimated yet.
    pub by: crate::enrichment::atlas::precision::SourcePrecision,
}

/// A statement's reading of a reference that names no record of its type:
/// a proposal the build never mints into a record, cited where it was read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnresolvedReading {
    /// The statement that read it.
    pub claim: String,
    /// The statement's document, when it is in one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    /// What the statement read.
    pub words: Value,
    /// Where the document holds them.
    pub evidence: String,
    /// Why they name no record.
    pub why: String,
}

/// Per attribute, what one stage did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DerivedTally {
    pub atoms: usize,
    pub outcomes: BTreeMap<&'static str, usize>,
    /// Atoms a `[!set]` filter dropped, over every derivation of the attribute.
    pub excluded: usize,
    /// Atoms of a set's type lacking an attribute it reads: in no set, out of no set.
    pub unjudged: usize,
    /// Readings of a reference that named no record (`UnresolvedReading`).
    pub unresolved: usize,
    /// Per document read, how it came to be held or not (`membership`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub documents: BTreeMap<&'static str, usize>,
}

/// `(type.attribute, tally)` for each attribute one stage derived.
pub type DerivedReport = BTreeMap<String, DerivedTally>;
