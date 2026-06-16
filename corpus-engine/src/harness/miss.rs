// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generalized extraction miss — one declared field absent from one document
//! (or, for section extractors, one source file). The Extract field-coverage
//! rung's evidence unit, lifted from the html_sections-specific `MissReport`.

use serde::{Deserialize, Serialize};

use crate::extractors::html_sections::MissReport;

/// A declared field that was not populated. `doc_id` is the document (or, for
/// section extractors, the source file) where it was expected; `nearby_text`
/// is the verbatim text near where it should have been, so the author sees
/// what the extractor saw — never just a count.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldMiss {
    pub field: String,
    pub doc_id: String,
    pub nearby_text: Option<String>,
}

impl From<MissReport> for FieldMiss {
    /// Lift the html_sections per-(file, section) miss into the generalized
    /// shape. The extractor already writes these to `_section_misses.json`;
    /// the harness unifies them with the per-document misses it computes for
    /// the other structured extractors.
    fn from(m: MissReport) -> Self {
        FieldMiss {
            field: m.section,
            doc_id: m.file,
            nearby_text: m.nearby_text,
        }
    }
}
