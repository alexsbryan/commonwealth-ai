// SPDX-License-Identifier: AGPL-3.0-or-later
//! A decided record's evidence: one cited chunk per distinct document among
//! its statements, in statement order, each in the section where that
//! statement was read. Failing input, named: the news journey
//! (runs/loop/ship/journeys-5b, 2026-10-20), whose `mill-lane-fire` and
//! `workers-treated` happenings were stated in report-a and report-b but
//! cited the opening statement's document only, so report-b's quote was
//! found under no cited chunk.

use std::collections::{BTreeSet, HashMap};

use tracing::{debug, trace};

use super::super::atoms::ChunkRef;
use super::super::resolve_records::Record;

/// `rec`'s evidence: for each statement (paired with the evidence its fold
/// recorded) whose document no earlier statement cited, its surface in the
/// section `section_of` places that statement in. A statement with no
/// section cites nothing, traced; the record keeps the rest.
pub(super) fn of_record(rec: &Record, section_of: &HashMap<String, String>) -> Vec<ChunkRef> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    for (statement, e) in rec.statements.iter().zip(&rec.evidence) {
        if !seen.insert(e.document.as_str()) {
            trace!(record = %rec.id, %statement, document = %e.document, "atlas/resolve: a statement's document is cited already");
            continue;
        }
        let Some(section) = section_of.get(statement) else {
            debug!(record = %rec.id, %statement, document = %e.document, "atlas/resolve: a statement read in no section; its document is not cited");
            seen.remove(e.document.as_str());
            continue;
        };
        let mut chunk = ChunkRef::new(section.clone(), Some(e.surface.clone()));
        chunk.source_doc_id = Some(e.document.clone());
        out.push(chunk);
    }
    if out.len() > 1 {
        debug!(record = %rec.id, documents = out.len(), "atlas/resolve: a record cites each document that states it");
    }
    out
}
