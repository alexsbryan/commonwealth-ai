// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a value or a link came from, and how far that source is to be
//! trusted (campaign ontology-layer C3; ONTOLOGY_METHOD §Identity "evidential
//! agreement links only where its measured precision clears the type's bar").
//! Every link RESOLVE makes, every field the reader chose and every derived
//! value carries one per source, and says where its precision comes from:
//! the recipe declared it, it was estimated on this corpus, or nothing has
//! measured it yet. Absent is said, never defaulted (principle 6).

use serde::{Deserialize, Serialize};

/// A source's precision and where the number comes from. Closed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    /// The declaration makes it so by construction (an equal sufficient key: 1.0).
    Declared(f64),
    /// Estimated on this corpus from the sources' agreement (campaign E2).
    Estimated(f64),
    /// Nothing has measured it: the source decided only so that it can be
    /// measured (an argmax read before its precision is known).
    Unmeasured,
}

/// One source of a value or a link, and its precision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePrecision {
    /// What decided: a declared field or key (`identity_key:<key>`, a
    /// document stamp's attribute), `model_choice`, `proposed_answer`,
    /// `reader_choose`, `derived:<id>`.
    pub source: String,
    pub precision: Precision,
}

impl SourcePrecision {
    pub fn new(source: impl Into<String>, precision: Precision) -> Self {
        Self {
            source: source.into(),
            precision,
        }
    }
}
