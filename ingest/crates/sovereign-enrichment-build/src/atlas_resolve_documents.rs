// SPDX-License-Identifier: AGPL-3.0-or-later
//! The documents' own fields for `atlas_resolve`, loaded once for both of
//! their readers: a metadata `source` projects entity atoms from them before
//! Phase 3a (`corpus_engine::enrichment::atlas::project_source_atoms`); after
//! 3b, `change.document` stamps claims from them and RESOLVE places its
//! statements in them ([`apply`]). Split from `atlas_resolve.rs` to keep that
//! file under arch-gate's 800-line band.

use std::io::Write;
use std::path::Path;

use corpus_engine::enrichment::atlas::resolution_derived::{
    derive_attributes, derives, DeriveStage, DerivedReport,
};
use corpus_engine::enrichment::atlas::resolution_records::{resolve_declared_types, BuildAtoms};
use corpus_engine::enrichment::atlas::resolve_records::{Answerer, DocumentResolution};
use corpus_engine::enrichment::atlas::{
    project_source_atoms, stamp_claim_documents, Participants, SectionDocuments, SourceProjection,
};
use corpus_engine::enrichment::ontology::{OntologyPolicies, SourceDecl};
use corpus_engine::enrichment::pipeline::PhaseFailure;
use corpus_engine::InferenceFn;

use super::config::EnrichConfig;

/// RESOLVE's decisions beside the atlas, one document's resolution per line
/// with its type: what `resolve-statements` writes as `decisions.jsonl`.
pub const DECISIONS_FILE: &str = "resolve_decisions.jsonl";

/// Every derived attribute's value per atom, or why it has none, one line
/// each (`resolution_derived::DerivedValue`).
pub const DERIVED_FILE: &str = "derived_decisions.jsonl";

/// What the resolve step reads from the documents themselves.
pub(crate) struct DocumentInputs {
    /// The rows grouped per section; `None` when nothing declared reads them.
    pub documents: Option<SectionDocuments>,
    /// The atoms projected from declared metadata sources, with their report.
    pub projection: Option<SourceProjection>,
    /// Which atoms each document's fields projected, kept apart from
    /// `projection`, which 3a consumes, for the derived paths after it.
    pub participants: Participants,
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
    from_documents(
        || {
            super::corpus_io::section_documents(cfg)
                .map_err(|e| format!("loading section documents: {e}"))
        },
        policies,
        &cfg.corpus_id,
        stamps,
    )
}

/// [`load`] over documents the caller supplies: the corpus's own rows on the
/// CLI path, a fixture's in the contract tests (`layer_contract_tests`).
/// `documents` is called only when the declaration reads them.
pub(crate) fn from_documents(
    documents: impl FnOnce() -> Result<SectionDocuments, String>,
    policies: &OntologyPolicies,
    corpus_id: &str,
    stamps: bool,
) -> Result<DocumentInputs, String> {
    let sourced = policies
        .shape
        .types
        .iter()
        .any(|t| matches!(t.source, Some(SourceDecl::Metadata(_))));
    let read = policies.change.document.is_some()
        || derives(policies)
        || policies
            .shape
            .types
            .iter()
            .any(|t| t.identity_criterion.is_some());
    if !sourced && !(stamps && read) {
        return Ok(DocumentInputs {
            documents: None,
            projection: None,
            participants: Participants::new(),
        });
    }
    let documents = documents()?;
    let mut projection = if sourced {
        Some(project_source_atoms(&documents, policies, corpus_id)?)
    } else {
        None
    };
    let participants = projection
        .as_mut()
        .map(|p| std::mem::take(&mut p.participants))
        .unwrap_or_default();
    Ok(DocumentInputs {
        documents: Some(documents),
        projection,
        participants,
    })
}

/// After 3b: stamp each claim with its own document's declared fields; derive
/// the attributes RESOLVE does not wait for; let RESOLVE decide every type it
/// decides (`resolution_records`), the model answering as the adopted design
/// asks (one forced choice per statement, weighed at its declared precision);
/// then derive the attributes of the records it made. Without documents every
/// statement is unplaced and recorded; the Phase-1 atoms of a decided type are
/// retired either way. Each step's decisions land beside the atlas.
pub(crate) async fn apply(
    mut atoms: BuildAtoms<'_>,
    inputs: &DocumentInputs,
    policies: &OntologyPolicies,
    corpus_id: &str,
    infer: &InferenceFn,
    atlas_dir: &Path,
) -> Result<Vec<PhaseFailure>, String> {
    let mut failures = Vec::new();
    let none = SectionDocuments::default();
    let documents = inputs.documents.as_ref().unwrap_or(&none);
    if let (Some(decl), Some(docs)) = (policies.change.document.as_ref(), &inputs.documents) {
        let stamps = stamp_claim_documents(&mut *atoms.claims, docs, decl);
        println!("  ✓ {}", stamps.summary());
        failures.extend(stamps.failures);
    }
    let resolves = policies
        .shape
        .types
        .iter()
        .any(|t| t.identity_criterion.is_some());
    let derives = derives(policies);
    let mut derived = derives
        .then(|| Jsonl::create(atlas_dir.join(DERIVED_FILE)))
        .transpose()?;
    let mut derive = |atoms: &mut BuildAtoms<'_>, stage: DeriveStage| -> Result<(), String> {
        let Some(out) = derived.as_mut() else {
            return Ok(());
        };
        let report = derive_attributes(
            atoms,
            documents,
            &inputs.participants,
            policies,
            stage,
            &mut |v| out.line(v),
        )?;
        print_derived(stage, &report);
        Ok(())
    };
    derive(&mut atoms, DeriveStage::BeforeResolve)?;
    if resolves {
        let mut decisions = Jsonl::create(atlas_dir.join(DECISIONS_FILE))?;
        let (reports, refused) = resolve_declared_types(
            &mut atoms,
            documents,
            policies,
            corpus_id,
            Answerer::Select(infer),
            &mut |ty: &str, r: &DocumentResolution| decisions.typed_line(ty, r),
        )
        .await;
        let path = decisions.finish()?;
        for r in &reports {
            println!("  ✓ {}", r.summary());
        }
        println!("  ✓ RESOLVE decisions → {}", path.display());
        failures.extend(refused);
    }
    derive(&mut atoms, DeriveStage::AfterResolve)?;
    if let Some(out) = derived {
        println!("  ✓ derived values → {}", out.finish()?.display());
    }
    Ok(failures)
}

fn print_derived(stage: DeriveStage, report: &DerivedReport) {
    for (attr, t) in report {
        let outcomes: Vec<String> = t.outcomes.iter().map(|(k, n)| format!("{k} {n}")).collect();
        println!(
            "  ✓ derived {attr} ({stage:?}): {} atom(s), [{}]; {} excluded by a set, {} unjudged",
            t.atoms,
            outcomes.join(", "),
            t.excluded,
            t.unjudged
        );
    }
}

/// One JSON object per line; the first write error is kept and reported.
struct Jsonl {
    path: std::path::PathBuf,
    out: std::io::BufWriter<std::fs::File>,
    written: Result<(), String>,
}

impl Jsonl {
    fn create(path: std::path::PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            // A fresh corpus has no `atlas/` directory until the first store
            // write; a decision line must not fail for want of a parent.
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        let file = std::fs::File::create(&path)
            .map_err(|e| format!("creating {}: {e}", path.display()))?;
        Ok(Self {
            path,
            out: std::io::BufWriter::new(file),
            written: Ok(()),
        })
    }

    fn line(&mut self, v: &impl serde::Serialize) {
        if self.written.is_ok() {
            self.written = serde_json::to_string(v)
                .map_err(|e| e.to_string())
                .and_then(|l| writeln!(self.out, "{l}").map_err(|e| e.to_string()));
        }
    }

    /// A line with the type it is about added (`resolve-statements` keeps
    /// one type per run; the build keeps every type in one file).
    fn typed_line(&mut self, ty: &str, v: &impl serde::Serialize) {
        match serde_json::to_value(v) {
            Ok(mut line) => {
                if let Some(m) = line.as_object_mut() {
                    m.insert("type".into(), ty.into());
                }
                self.line(&line);
            }
            Err(e) => {
                if self.written.is_ok() {
                    self.written = Err(e.to_string());
                }
            }
        }
    }

    fn finish(mut self) -> Result<std::path::PathBuf, String> {
        self.written
            .and_then(|()| self.out.flush().map_err(|e| e.to_string()))
            .map_err(|e| format!("writing {}: {e}", self.path.display()))?;
        Ok(self.path)
    }
}

#[cfg(test)]
#[path = "atlas_resolve_documents_tests.rs"]
mod tests;

/// Atoms projected from document fields (`source = { metadata = … }`), and
/// the model atoms merged into them on the identity key. Zero for a corpus
/// that declares no metadata source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounts {
    pub projected: usize,
    pub merged: usize,
}
