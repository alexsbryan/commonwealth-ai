// SPDX-License-Identifier: AGPL-3.0-or-later
//! The documents' own fields for `atlas_resolve`, loaded once for both of
//! their readers: a metadata `source` projects entity atoms from them before
//! Phase 3a (`corpus_engine::enrichment::atlas::project_source_atoms`), and
//! `change.document` stamps claims from them after 3b. Split from
//! `atlas_resolve.rs` to keep that file under arch-gate's 800-line band.

use corpus_engine::enrichment::atlas::{project_source_atoms, SectionDocuments, SourceProjection};
use corpus_engine::enrichment::ontology::{OntologyPolicies, SourceDecl};

use super::config::EnrichConfig;

/// What the resolve step reads from the documents themselves.
pub(crate) struct DocumentInputs {
    /// The rows grouped per section; `None` when nothing declared reads them.
    pub documents: Option<SectionDocuments>,
    /// The atoms projected from declared metadata sources, with their report.
    pub projection: Option<SourceProjection>,
}

/// Load the section documents when a type declares a metadata `source`, or
/// when `stamps` asks and `change.document` is declared; project the sourced
/// types from them. A declaration that cannot project refuses the step.
pub(crate) fn load(
    cfg: &EnrichConfig,
    policies: &OntologyPolicies,
    stamps: bool,
) -> Result<DocumentInputs, String> {
    let sourced = policies
        .shape
        .types
        .iter()
        .any(|t| matches!(t.source, Some(SourceDecl::Metadata(_))));
    if !sourced && !(stamps && policies.change.document.is_some()) {
        return Ok(DocumentInputs {
            documents: None,
            projection: None,
        });
    }
    let documents = super::corpus_io::section_documents(cfg)
        .map_err(|e| format!("loading section documents: {e}"))?;
    let projection = if sourced {
        Some(project_source_atoms(&documents, policies, &cfg.corpus_id)?)
    } else {
        None
    };
    Ok(DocumentInputs {
        documents: Some(documents),
        projection,
    })
}

/// Atoms projected from document fields (`source = { metadata = … }`), and
/// the model atoms merged into them on the identity key. Zero for a corpus
/// that declares no metadata source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounts {
    pub projected: usize,
    pub merged: usize,
}
