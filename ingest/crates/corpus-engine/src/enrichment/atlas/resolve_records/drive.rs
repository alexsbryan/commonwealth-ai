// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one loop both drivers resolve a type's documents through: the atlas
//! build (`resolution_records.rs`) and `resolve-statements`. Documents go in
//! one canonical order, the clock, a pure function of the documents, so any
//! input order gives the same records (Ring 2 of
//! `research/ontology-apps/resolve-prereg.md`). Before Ring 2 the atlas build
//! sorted by the clock and `resolve-statements` took its file's order, so a
//! reordered statements file (`statements.py --salt`) moved GVC's measures.
//! After the last document the statements the decider held are settled
//! (`settle.rs`, E3), and each document that had one is passed to
//! `on_document` once more as a resolution that `settles`.

use tracing::debug;

use super::propose::Proposers;
use super::settle::held_in;
use super::{Answerer, Criterion, Document, DocumentResolution, Resolver, Statement};
use crate::enrichment::ontology::DocumentStamp;

/// A document's place on the clock: dated documents first, oldest first, the
/// document id breaking ties.
pub fn clock<'a>(doc: &Document<'a>) -> (bool, Option<&'a str>, &'a str) {
    let date = doc.stamp(DocumentStamp::Date);
    (date.is_none(), date, doc.id)
}

/// Resolve every document of `documents` (sorted here, by the clock), each
/// against the candidates `proposers` offer from the documents before it;
/// `on_document` sees each resolution in that order with its position.
pub async fn resolve_in_clock_order<'a>(
    criterion: &Criterion,
    documents: &mut [(Document<'a>, &'a [Statement])],
    resolver: &mut Resolver,
    proposers: &mut Proposers,
    answerer: Answerer<'_>,
    on_document: &mut (dyn FnMut(usize, Document<'a>, &DocumentResolution) + Send),
) {
    documents.sort_by(|a, b| clock(&a.0).cmp(&clock(&b.0)));
    debug!(
        documents = documents.len(),
        first = documents.first().map(|d| d.0.id),
        "atlas/resolve: documents in clock order"
    );
    let mut held = Vec::new();
    for (k, (doc, statements)) in documents.iter().enumerate() {
        let candidates = proposers.propose(*doc);
        let r = resolver
            .resolve_document(criterion, *doc, statements, &candidates, answerer)
            .await;
        proposers.observe(*doc, &r);
        held_in(k, &r, &mut held);
        on_document(k, *doc, &r);
    }
    // E3: what the decider held is settled once every document is seen, and
    // each settled document is seen again, its outcomes replacing the held.
    if !held.is_empty() {
        for (k, r) in resolver.settle(criterion, documents, held, answerer).await {
            on_document(k, documents[k].0, &r);
        }
    }
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
