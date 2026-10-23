// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate`'s warnings about the ontology layer's default path
//! (campaign ontology-layer, order 2): a type RESOLVE decides with no claim
//! kind about it, which `enrich extract` refuses, what the document reader
//! leaves inert, and the retired reader keys. Split from `validate.rs` to keep
//! it under arch-gate's 800-line band.

use super::{OntologyPolicies, SourceDecl, TypeIndex, TypeKind};
use crate::enrichment::atlas::resolution_records::decides;
use crate::recipe::OntologyBlock;
use understanding_vocab::ontology::decl::PatternDecl;

pub(super) fn layer_warnings(
    block: &OntologyBlock,
    policies: &OntologyPolicies,
    warnings: &mut Vec<String>,
) {
    let orphans = crate::enrichment::atlas::resolution_records::types_without_statements(policies);
    if !orphans.is_empty() {
        warnings.push(format!(
            "RESOLVE decides {} by its identity_criterion, but no claim kind names it as its \
             `subject`: `enrich extract` refuses to build this, since no statement of it would be \
             read. Declare a claim kind about it.",
            orphans
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let unread: Vec<String> = crate::enrichment::pipeline::document_read::unread_types(policies)
        .into_iter()
        .filter(|t| !orphans.contains(&t.name))
        .map(|t| format!("`{}` ({})", t.name, super::validate::kind_key(t.kind)))
        .collect();
    if !unread.is_empty() {
        warnings.push(format!(
            "the document reader reads statement kinds, the types they are about and every type \
             RESOLVE decides, so it makes no record of {}: no statement kind is about them, \
             RESOLVE decides none of them and no source supplies them.",
            unread.join(", ")
        ));
    }
    warnings.extend(reader_inert(policies));
    for (key, instead) in crate::recipe_ontology::language::retired_keys(&block.body) {
        warnings.push(format!(
            "[enrichment.ontology] `{key}` is retired and ignored: {instead}. Remove the key."
        ));
    }
    for (t, key, instead) in crate::recipe_ontology::language::retired_type_keys(&block.body) {
        warnings.push(format!(
            "ontology type `{t}`: `{key}` is retired and ignored: {instead}. Remove the key."
        ));
    }
}

/// What a declaration the document reader reads also declares that the reader
/// leaves inert, one warning each, so a feature never drops out unsaid. Each
/// names its consumer's limit: argument reconstructions and attributing to a
/// voice are the general extractor's alone, a pattern walks relation atoms, which the reader
/// makes of a read relation statement only (`statement_atoms`), no stage reads
/// a table source yet, and RESOLVE does not inherit a criterion through
/// `specializes` (`Criterion::of`). Failing input, named: the ten templates
/// against the reader (inventory 2026-10-15): research-notebook's arguments
/// and voices, due-diligence's pattern, product-support's `ticket`,
/// numismatics' `sceatta`.
fn reader_inert(policies: &OntologyPolicies) -> Vec<String> {
    if !policies.reads_documents() {
        return Vec::new();
    }
    let index = TypeIndex::from_policies(policies);
    let types = &policies.shape.types;
    let mut out = Vec::new();
    let derivation = &policies.derivation;
    if derivation.arguments {
        out.push(
            "`derive.arguments` reconstructs arguments from the general extractor's output only; \
             the document reader reads this declaration, so it gets none."
                .to_string(),
        );
    }
    for p in &derivation.patterns {
        let (name, edges) = pattern_edges(p);
        let unread: Vec<&str> = edges
            .into_iter()
            .filter(|e| {
                !types.iter().any(|t| {
                    t.name == *e && t.kind == TypeKind::Relation && t.is_read_statement(types)
                })
            })
            .collect();
        if !unread.is_empty() {
            out.push(format!(
                "pattern `{name}` walks {}, which the document reader makes no relation atom of: \
                 it makes one of a read relation statement only, so the pattern finds none there.",
                unread
                    .iter()
                    .map(|e| format!("`{e}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let voices = &policies.assertion.voices;
    let declared: Vec<&str> = [
        ("attributed_to", !voices.attributed_to.is_empty()),
        ("self", voices.self_voice.is_some()),
    ]
    .into_iter()
    .filter_map(|(key, set)| set.then_some(key))
    .collect();
    if !declared.is_empty() {
        out.push(format!(
            "`voices` ({}) are applied by the general extractor only; the document reader \
             attributes no statement to a speaker.",
            declared.join(", ")
        ));
    }
    for t in types {
        if matches!(t.source, Some(SourceDecl::Table(_))) && !t.is_read_statement(types) {
            out.push(format!(
                "ontology type `{}` declares a table source, which no stage reads yet, and the \
                 document reader does not read it from text, so it gets no records.",
                t.name
            ));
        }
        let unsourced = matches!(t.kind, TypeKind::Entity | TypeKind::Event)
            && t.source.is_none()
            && t.identity_criterion.is_none();
        if let Some(parent) = types.iter().find(|p| {
            unsourced
                && p.name != t.name
                && index.is_a(&t.name, &p.name)
                && decides(&index, &p.name)
        }) {
            out.push(format!(
                "ontology type `{}` specializes `{}` but declares no identity_criterion of its \
                 own, and RESOLVE inherits none: the document reader reads its particulars as \
                 `{}` records and makes no `{}` record.",
                t.name, parent.name, parent.name, t.name
            ));
        }
    }
    out
}

/// A pattern's name and the edge types it walks. `custom_sql` walks none the
/// declaration names (`recipe validate` already says it does not run).
fn pattern_edges(p: &PatternDecl) -> (&str, Vec<&str>) {
    match p {
        PatternDecl::CircularFlow {
            name, edge_types, ..
        } => (name, edge_types.iter().map(String::as_str).collect()),
        PatternDecl::RoleOverlap {
            name, entity_roles, ..
        } => (
            name,
            entity_roles
                .values()
                .filter_map(|r| r.split('.').next())
                .collect(),
        ),
        PatternDecl::Threshold {
            name, edge_type, ..
        } => (name, vec![edge_type.as_str()]),
        PatternDecl::CustomSql { name, .. } => (name, Vec::new()),
    }
}

#[cfg(test)]
#[path = "validate_layer_tests.rs"]
mod tests;
