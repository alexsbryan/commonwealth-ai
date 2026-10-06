// SPDX-License-Identifier: AGPL-3.0-or-later
//! The documents' own fields for `atlas_resolve`, loaded once for both of
//! their readers: a metadata `source` projects entity atoms from them before
//! Phase 3a (`corpus_engine::enrichment::atlas::project_source_atoms`); after
//! 3b, `change.document` stamps claims from them and RESOLVE places its
//! statements in them ([`apply`]). Split from `atlas_resolve.rs` to keep that
//! file under arch-gate's 800-line band.

use std::io::Write;
use std::path::Path;

use corpus_engine::enrichment::atlas::resolution_records::{resolve_declared_types, BuildAtoms};
use corpus_engine::enrichment::atlas::resolve_records::{Answerer, DocumentResolution};
use corpus_engine::enrichment::atlas::{
    project_source_atoms, stamp_claim_documents, SectionDocuments, SourceProjection,
};
use corpus_engine::enrichment::ontology::{OntologyPolicies, SourceDecl};
use corpus_engine::enrichment::pipeline::PhaseFailure;
use corpus_engine::InferenceFn;

use super::config::EnrichConfig;

/// RESOLVE's decisions beside the atlas, one document's resolution per line
/// with its type: what `resolve-statements` writes as `decisions.jsonl`.
pub const DECISIONS_FILE: &str = "resolve_decisions.jsonl";

/// What the resolve step reads from the documents themselves.
pub(crate) struct DocumentInputs {
    /// The rows grouped per section; `None` when nothing declared reads them.
    pub documents: Option<SectionDocuments>,
    /// The atoms projected from declared metadata sources, with their report.
    pub projection: Option<SourceProjection>,
}

/// Load the section documents when a type declares a metadata `source`, or
/// when `stamps` asks and `change.document` or an identity criterion is
/// declared; project the sourced types from them. A declaration that cannot
/// project refuses the step.
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
    let read = policies.change.document.is_some()
        || policies
            .shape
            .types
            .iter()
            .any(|t| t.identity_criterion.is_some());
    if !sourced && !(stamps && read) {
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

/// After 3b: stamp each claim with its own document's declared fields, then
/// let RESOLVE decide every type it decides (`resolution_records`), the model
/// answering as the adopted design asks (one forced choice per statement,
/// weighed at its declared precision). Without documents every statement is
/// unplaced and recorded; the Phase-1 atoms of a decided type are retired
/// either way.
pub(crate) async fn apply(
    atoms: BuildAtoms<'_>,
    documents: Option<&SectionDocuments>,
    policies: &OntologyPolicies,
    corpus_id: &str,
    infer: &InferenceFn,
    atlas_dir: &Path,
) -> Result<Vec<PhaseFailure>, String> {
    let mut failures = Vec::new();
    if let (Some(decl), Some(docs)) = (policies.change.document.as_ref(), documents) {
        let stamps = stamp_claim_documents(&mut *atoms.claims, docs, decl);
        println!("  ✓ {}", stamps.summary());
        failures.extend(stamps.failures);
    }
    if !policies
        .shape
        .types
        .iter()
        .any(|t| t.identity_criterion.is_some())
    {
        return Ok(failures);
    }
    let none = SectionDocuments::default();
    let path = atlas_dir.join(DECISIONS_FILE);
    let file =
        std::fs::File::create(&path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let mut written: Result<(), String> = Ok(());
    let mut sink = |ty: &str, r: &DocumentResolution| {
        if written.is_err() {
            return;
        }
        written = serde_json::to_value(r)
            .map_err(|e| e.to_string())
            .and_then(|mut line| {
                if let Some(m) = line.as_object_mut() {
                    m.insert("type".into(), ty.into());
                }
                writeln!(out, "{line}").map_err(|e| e.to_string())
            });
    };
    let (reports, refused) = resolve_declared_types(
        atoms,
        documents.unwrap_or(&none),
        policies,
        corpus_id,
        Answerer::Select(infer),
        &mut sink,
    )
    .await;
    written
        .and_then(|()| out.flush().map_err(|e| e.to_string()))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    for r in &reports {
        println!("  ✓ {}", r.summary());
    }
    println!("  ✓ RESOLVE decisions → {}", path.display());
    failures.extend(refused);
    Ok(failures)
}

/// Atoms projected from document fields (`source = { metadata = … }`), and
/// the model atoms merged into them on the identity key. Zero for a corpus
/// that declares no metadata source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounts {
    pub projected: usize,
    pub merged: usize,
}
