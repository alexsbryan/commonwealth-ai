// SPDX-License-Identifier: AGPL-3.0-or-later
//! One document's words read, stored and listed, for the ingest loop: moved
//! out of `engine::ingest` whole so that file stays under its ceiling.

use kernel_types::Sha256Hash;

use crate::error::Result;
use crate::extractors::ExtractedDoc;
use crate::index::TextWriter;
use crate::recipe_documents::DeclaredMetadata;

use super::document_ledger::DocumentLedger;
use super::ingest_helpers::normalize_content;

/// `doc` read through its declared format (v0.6 §2.13, §2.14), stored with
/// the places its spans are located by, and listed ready: the words to chunk and the stored text's name.
/// A document its declared format does not read is listed failed, stores
/// nothing, and is `None`.
pub(super) fn store_read(
    texts: &mut TextWriter,
    declared: &DeclaredMetadata,
    documents: &mut DocumentLedger,
    corpus_id: &str,
    doc: &ExtractedDoc,
    named: &str,
    ordinal: u32,
) -> Result<Option<(String, Option<Sha256Hash>)>> {
    let read = match declared.read(&normalize_content(&doc.content), named, &doc.source) {
        Ok(read) => read,
        Err(e) => {
            let source = documents.failed(declared, &e);
            tracing::warn!(
                corpus = %corpus_id,
                source = source.as_deref().unwrap_or("(unnamed)"),
                error = %e,
                "skipping document: declared format does not read it"
            );
            return Ok(None);
        }
    };
    let text_sha256 = texts.store_document_located(
        declared.input(
            &read.text,
            named,
            ordinal,
            &doc.source,
            doc.metadata.as_ref(),
        ),
        read.locators.as_deref(),
    )?;
    documents.ready(declared, named, &doc.source, text_sha256);
    Ok(Some((read.text, text_sha256)))
}
