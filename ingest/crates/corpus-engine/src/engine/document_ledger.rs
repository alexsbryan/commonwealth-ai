// SPDX-License-Identifier: AGPL-3.0-or-later
//! Each document an ingest reads, ready or failed, named as the recipe names
//! it (OICP v0.6 §2.9): what a client polls to learn which of its library's
//! documents it can check against. The ingest loop records into it; its
//! parts become `IngestResult::{documents, documents_unlisted}`.

use corpus_index::ingest_port::daemon::{DocumentOutcome, DocumentState, DOCUMENTS_LISTED_CAP};
use kernel_types::Sha256Hash;

use crate::error::Error;
use crate::extractors::DocSource;
use crate::recipe_documents::DeclaredMetadata;

/// The documents of one ingest, up to [`DOCUMENTS_LISTED_CAP`], and how many
/// past it were counted only.
#[derive(Debug, Default)]
pub(super) struct DocumentLedger {
    documents: Vec<DocumentOutcome>,
    unlisted: u64,
}

impl DocumentLedger {
    /// A document stored as the text `text_sha256` names.
    pub(super) fn ready(
        &mut self,
        declared: &DeclaredMetadata,
        source_id: &str,
        source: &DocSource,
        text_sha256: Option<Sha256Hash>,
    ) {
        self.list(DocumentOutcome {
            source: Some(declared.source_name(source_id, source)),
            state: DocumentState::Ready { text_sha256 },
        });
    }

    /// A document an extractor failed on: named when its error says which
    /// (`Error::ExtractionOf`), unnamed otherwise. The name, for the log.
    pub(super) fn failed(&mut self, declared: &DeclaredMetadata, e: &Error) -> Option<String> {
        let (source, reason) = match e {
            Error::ExtractionOf { document, reason } => {
                (Some(declared.name_of(document)), reason.clone())
            }
            other => (None, other.to_string()),
        };
        self.list(DocumentOutcome {
            source: source.clone(),
            state: DocumentState::Failed { reason },
        });
        source
    }

    fn list(&mut self, outcome: DocumentOutcome) {
        if self.documents.len() < DOCUMENTS_LISTED_CAP {
            self.documents.push(outcome);
        } else {
            self.unlisted += 1;
        }
    }

    /// The listed documents and the count past the cap.
    pub(super) fn into_parts(self) -> (Vec<DocumentOutcome>, u64) {
        (self.documents, self.unlisted)
    }
}
