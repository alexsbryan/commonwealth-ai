// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the method supplies where a document-reading declaration is silent
//! about how a record is decided (ONTOLOGY_METHOD §Reading).
//!
//! A type a read reference targets (a statement's `ref` field, a relation's
//! `to`, a read subject's `ref` field), that nothing else records (no
//! `identity_criterion`, no `source`), is decided by RESOLVE by name: it takes
//! [`DEFAULT_CRITERION`], so the window reader names its particulars, each
//! reaches RESOLVE as a statement of its own, and the references land on
//! records. A base kind such as `person`, referred to and never declared, is
//! declared here as an entity of that kind with the same criterion. Failing
//! input, named: the census of twelve blind-authored recipes
//! (`research/ontology-apps/census/judgments.json`, gap
//! `ref-target-unrecorded`), whose `owner`, `actor`, `requester`, `warehouse`,
//! `technician` and `supplier` readings were each removed from their
//! statements, naming no record.
//!
//! Every default is returned as a [`SuppliedDefault`], which `recipe validate`
//! prints, so none is silent.

use std::collections::BTreeSet;

use tracing::{debug, trace};
use understanding_vocab::ontology::decl::{AttrFamily, OntologyTypeDecl, TypeKind};
use understanding_vocab::ontology::OntologyPolicies;

use crate::enrichment::atlas::resolution_records::decides;
use crate::enrichment::ontology::TypeIndex;
use crate::enrichment::pipeline::atlas::EntityType;
use crate::enrichment::pipeline::document_read::{statement_attributes, subject_read_attributes};

/// The identity criterion of a type decided by name, `{type}` its name.
pub const DEFAULT_CRITERION: &str =
    "Two mentions are one {type} when they name the same {type}, however the name is written.";

/// The description of a base kind declared because a reference names it.
const IMPLICIT_DESCRIPTION: &str = "A {type} the statements refer to.";

/// One default the method supplied, for `recipe validate` to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppliedDefault {
    /// `type_name` is decided by RESOLVE under [`DEFAULT_CRITERION`]:
    /// declared with no criterion and no source, or (`declared == false`) a
    /// base kind declared here. `referred_by` lists each `holder.field` whose
    /// reading refers to it.
    ByName {
        type_name: String,
        declared: bool,
        referred_by: Vec<String>,
    },
}

impl SuppliedDefault {
    /// The derivation line `recipe validate` prints.
    pub fn note(&self) -> String {
        match self {
            Self::ByName {
                type_name,
                declared,
                referred_by,
            } => {
                let why = if *declared {
                    "no identity_criterion declared"
                } else {
                    "no declaration"
                };
                let refs: Vec<String> = referred_by.iter().map(|r| format!("`{r}`")).collect();
                let verb = if refs.len() == 1 { "refers" } else { "refer" };
                format!(
                    "identity: {type_name} → RESOLVE by name ({why}; {} {verb} to it)",
                    refs.join(", ")
                )
            }
        }
    }
}

/// [`DEFAULT_CRITERION`] for `type_name`.
pub fn default_criterion(type_name: &str) -> String {
    DEFAULT_CRITERION.replace("{type}", type_name)
}

/// Supply every default `policies` needs, returning each. Nothing when the
/// declaration is not read by the document reader: the general extractor
/// records every type it reads.
pub(crate) fn supply(policies: &mut OntologyPolicies) -> Vec<SuppliedDefault> {
    if !policies.reads_documents() {
        trace!("ontology: the document reader does not read this declaration; no resolve defaults");
        return Vec::new();
    }
    let mut supplied = Vec::new();
    // A type decided by name has read fields of its own, whose references
    // may name another type nothing records: until none is left.
    loop {
        let mut wanted: Vec<(String, Vec<String>)> = Vec::new();
        for (of, holder) in read_references(policies) {
            match wanted.iter_mut().find(|(t, _)| *t == of) {
                Some((_, holders)) => holders.push(holder),
                None => wanted.push((of, vec![holder])),
            }
        }
        let mut changed = false;
        for (of, referred_by) in wanted {
            let declared = match policies.type_decl(&of) {
                Some(t) if recorded_otherwise(t) => {
                    trace!(r#type = %of, "ontology: a referenced type is recorded by its own declaration");
                    continue;
                }
                Some(_) => true,
                None if EntityType::NAMED.contains(&of.as_str()) => false,
                None => {
                    debug!(r#type = %of, "ontology: a reference names neither a declared type nor a base kind; validate refuses it");
                    continue;
                }
            };
            if declared {
                if let Some(t) = policies.shape.types.iter_mut().find(|t| t.name == of) {
                    t.identity_criterion = Some(default_criterion(&of));
                }
            } else {
                policies.shape.types.push(OntologyTypeDecl {
                    name: of.clone(),
                    kind: TypeKind::Entity,
                    description: IMPLICIT_DESCRIPTION.replace("{type}", &of),
                    identity_criterion: Some(default_criterion(&of)),
                    ..Default::default()
                });
            }
            debug!(r#type = %of, declared, referred_by = ?referred_by, "ontology: a referenced type nothing records is decided by RESOLVE by name");
            supplied.push(SuppliedDefault::ByName {
                type_name: of,
                declared,
                referred_by,
            });
            changed = true;
        }
        if !changed {
            return supplied;
        }
    }
}

/// Whether `t` is recorded by something other than a default: its own
/// identity criterion, a source, or a kind RESOLVE never decides.
fn recorded_otherwise(t: &OntologyTypeDecl) -> bool {
    !matches!(t.kind, TypeKind::Entity | TypeKind::Event)
        || t.identity_criterion.is_some()
        || t.source.is_some()
}

/// Every `(referenced type, holder.field)` the document reader reads: the
/// `ref` fields of each read statement kind, and of each type it reads as a
/// subject (a statement kind's subject, or a type RESOLVE decides).
fn read_references(policies: &OntologyPolicies) -> Vec<(String, String)> {
    let index = TypeIndex::from_policies(policies);
    let types = &policies.shape.types;
    let mut out = Vec::new();
    let mut push = |holder: &str, field: &str, family: &AttrFamily| {
        if let AttrFamily::Ref { of } = family {
            out.push((of.clone(), format!("{holder}.{field}")));
        }
    };
    let kinds: Vec<&OntologyTypeDecl> = types
        .iter()
        .filter(|t| t.is_read_statement(types))
        .collect();
    for kind in &kinds {
        for a in statement_attributes(&index, kind) {
            push(&kind.name, &a.name, &a.family);
        }
    }
    let subjects: BTreeSet<&str> = kinds
        .iter()
        .filter_map(|k| k.read_subject())
        .chain(
            types
                .iter()
                .filter(|t| decides(&index, &t.name))
                .map(|t| t.name.as_str()),
        )
        .collect();
    for subject in subjects {
        for a in subject_read_attributes(policies, &index, subject) {
            push(subject, &a.name, &a.family);
        }
    }
    out
}

#[cfg(test)]
#[path = "resolve_defaults_tests.rs"]
mod tests;
