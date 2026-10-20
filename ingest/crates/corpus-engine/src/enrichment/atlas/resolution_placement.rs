// SPDX-License-Identifier: AGPL-3.0-or-later
//! Index-chunk placement: every evidence of a built atlas names the INSTALLED
//! corpus chunk that holds its quote (`ChunkRef::index_chunk`).
//!
//! Under the window reader an evidence's `chunk_id` is the window SECTION
//! (`sec_00001`), which groups many chunks and is no id the daemon's chunk
//! endpoint resolves (`/v1/corpora/{corpus}/chunks/{id}` wants a `u64`). The
//! section stays in `chunk_id`, where resolution reads it; this pass finds the
//! evidence's document the way stamping does (`evidence_documents`), then the
//! one chunk of it whose text holds the quote whole. Evidence it cannot place
//! keeps `index_chunk = None`, is counted by why, and reads by its section as
//! before.

use std::collections::BTreeMap;

use tracing::{debug, info, trace};

use super::atoms::ChunkRef;
use super::resolution_documents::{evidence_documents, fold_ws, SectionDocuments};
use super::resolution_records::BuildAtoms;

/// Why one evidence was left without its installed chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unplaced {
    /// The evidence carries no quote to find.
    NoAnchor,
    /// Its section holds no document it lands in (`evidence_documents`).
    NoDocument,
    /// The quote is in more than one of its section's documents.
    SeveralDocuments,
    /// The document's chunks joined hold the quote; no single chunk does.
    AcrossChunks,
    /// No chunk of the document holds the quote, joined or alone.
    InNoChunk,
}

impl Unplaced {
    fn label(self) -> &'static str {
        match self {
            Unplaced::NoAnchor => "no anchor",
            Unplaced::NoDocument => "no document",
            Unplaced::SeveralDocuments => "several documents",
            Unplaced::AcrossChunks => "anchor across chunks",
            Unplaced::InNoChunk => "anchor in no chunk",
        }
    }
}

/// What placement did, for the build report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlacementReport {
    /// Evidence looked at, across every atom.
    pub evidence: usize,
    /// Evidence this pass placed: the chunk holds its quote.
    pub placed: usize,
    /// Evidence placed before this pass and left as it was: a record projected
    /// from document metadata, cited by its document's first chunk
    /// (`resolution_sources::metadata_appearance`). Not a quote placement.
    pub projected: usize,
    /// Evidence left unplaced, by why.
    pub unplaced: BTreeMap<Unplaced, usize>,
}

impl PlacementReport {
    /// Unplaced evidence of one reason.
    pub fn count(&self, why: Unplaced) -> usize {
        self.unplaced.get(&why).copied().unwrap_or(0)
    }

    /// One line for the resolve step's output.
    pub fn summary(&self) -> String {
        let unplaced = if self.unplaced.is_empty() {
            "none unplaced".to_string()
        } else {
            self.unplaced
                .iter()
                .map(|(why, n)| format!("{} ×{n}", why.label()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "index chunks: {} of {} evidence placed by quote, {} by document metadata ({unplaced})",
            self.placed, self.evidence, self.projected
        )
    }
}

/// Place every evidence of every atom the build holds in its installed chunk.
/// The kinds walked are those [`super::atoms::AtomEnvelope::evidence`] covers
/// that a build holds: an entity's, position's and opposition's
/// `first_appearance`, every other kind's `evidence`. Evidence that arrives
/// already placed (a record projected from document metadata) is left
/// untouched and counted as `projected`.
pub fn place_index_chunks(
    atoms: &mut BuildAtoms<'_>,
    documents: &SectionDocuments,
) -> PlacementReport {
    let mut report = PlacementReport::default();
    let evidence = atoms
        .entities
        .iter_mut()
        .map(|a| &mut a.first_appearance)
        .chain(atoms.positions.iter_mut().map(|a| &mut a.first_appearance))
        .chain(
            atoms
                .oppositions
                .iter_mut()
                .map(|a| &mut a.first_appearance),
        )
        .chain(atoms.events.iter_mut().flat_map(|a| a.evidence.iter_mut()))
        .chain(atoms.states.iter_mut().flat_map(|a| a.evidence.iter_mut()))
        .chain(
            atoms
                .relations
                .iter_mut()
                .flat_map(|a| a.evidence.iter_mut()),
        )
        .chain(atoms.claims.iter_mut().flat_map(|a| a.evidence.iter_mut()))
        .chain(
            atoms
                .argument_reconstructions
                .iter_mut()
                .flat_map(|a| a.evidence.iter_mut()),
        );
    for ev in evidence {
        report.evidence += 1;
        if let Some(id) = ev.index_chunk {
            trace!(
                section = %ev.chunk_id,
                document = ?ev.source_doc_id,
                chunk = id,
                "atlas/placement: evidence already placed (document metadata); left as is"
            );
            report.projected += 1;
            continue;
        }
        match index_chunk(ev, documents) {
            Ok(id) => {
                ev.index_chunk = Some(id);
                report.placed += 1;
            }
            Err(why) => {
                debug!(
                    section = %ev.chunk_id,
                    document = ?ev.source_doc_id,
                    anchor = ?ev.passage_preview,
                    reason = why.label(),
                    "atlas/placement: evidence left without its installed chunk"
                );
                *report.unplaced.entry(why).or_default() += 1;
            }
        }
    }
    info!(
        evidence = report.evidence,
        placed = report.placed,
        projected = report.projected,
        unplaced = report.evidence - report.placed - report.projected,
        "atlas/placement: evidence placed in installed chunks"
    );
    report
}

/// The installed chunk holding `ev`'s quote, or why there is not one.
fn index_chunk(ev: &ChunkRef, documents: &SectionDocuments) -> Result<u64, Unplaced> {
    let anchor = ev
        .passage_preview
        .as_deref()
        .map(fold_ws)
        .filter(|a| !a.is_empty())
        .ok_or(Unplaced::NoAnchor)?;
    let doc = match evidence_documents(ev, documents) {
        Err(_) => return Err(Unplaced::NoDocument),
        Ok(hits) => match hits.as_slice() {
            [one] => *one,
            _ => return Err(Unplaced::SeveralDocuments),
        },
    };
    match doc.chunk_holding(&anchor) {
        Some(id) => Ok(id),
        None if doc.chunks_hold(&anchor) => Err(Unplaced::AcrossChunks),
        None => Err(Unplaced::InNoChunk),
    }
}

#[cfg(test)]
#[path = "resolution_placement_tests.rs"]
mod tests;
