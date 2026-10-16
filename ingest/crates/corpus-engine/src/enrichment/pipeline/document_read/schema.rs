use std::borrow::Cow;
use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::enrichment::atlas::resolution_records::decides;
use crate::enrichment::ontology::{
    AttrDecl, OntologyPolicies, OntologyTypeDecl, SourceDecl, TypeIndex, TypeKind,
};
use crate::enrichment::pipeline::types::{ChapterInput, ChatPrompt};

/// v4: one reader remains (passes); the one-shot prompt, its decoder schema and
/// its citation handles are gone, so reads cached under v3 are not reused.
/// v5: prefill, line classes, Point, Mention and Pick; a v4 read left open
/// fields and references unknown, so it is not reused.
/// v6: a claim carries its subject's readings whole and the projection
/// sketches no subject; a v5 section holds bare values and subject sketches,
/// so it is not reused.
/// v7: a window read's statements and particulars carry their subject's name
/// (`SUBJECT_NAME_ATTRIBUTE`); a v6 section lacks it, so it is not reused.
/// v8: a window reference written as a particular's label reads as its name;
/// a v7 section left it unknown, so it is not reused.
/// v9: a window claim citing lines its document quotes or embeds, not its
/// own, is refused; a v8 section may hold one, so it is not reused.
const CONTRACT_VERSION: u32 = 9;

/// The Phase-1 prompt of a declared reading: the plan this chapter will be read
/// by, never dispatched. The passes reader asks its own small questions
/// (`passes::read`); this is what `--dry-run` shows and part of what keys the
/// section cache (`runner_support::cache_text`).
pub fn compose(chapter: &ChapterInput, policies: &OntologyPolicies, phase_id: &str) -> ChatPrompt {
    let documents: Vec<&str> = chapter.source_documents.iter().map(|d| d.key()).collect();
    tracing::debug!(
        chapter = %chapter.chapter_id,
        documents = documents.len(),
        claim_types = policies
            .shape
            .types
            .iter()
            .filter(|claim| read_kind(claim, policies))
            .count(),
        "phase1.document_read_plan_composed"
    );
    let user = serde_json::to_string_pretty(&json!({
        "chapter_id": chapter.chapter_id,
        "documents": documents,
        "declared_read_contract": contract_value(policies),
    }))
    .expect("serialising the document-read plan is infallible");
    ChatPrompt::new("", user).with_phase_id(phase_id)
}

pub(super) fn contract_value(policies: &OntologyPolicies) -> Value {
    let index = TypeIndex::from_policies(policies);
    let eligible_claims: Vec<Value> = policies
        .shape
        .types
        .iter()
        .filter(|claim| read_kind(claim, policies))
        .map(|claim| {
            json!({
                "name": claim.name,
                "description": claim.description,
                "force": claim.force,
                "subject": claim.read_subject(),
                "scope": claim.scope,
                "attributes": attrs_value(statement_attributes(&index, claim).iter().map(|a| &**a).collect()),
            })
        })
        .collect();
    let subject_names: BTreeSet<&str> = eligible_claims
        .iter()
        .filter_map(|claim| claim.get("subject").and_then(Value::as_str))
        .collect();
    let subjects: Vec<Value> = policies
        .shape
        .types
        .iter()
        .filter(|ty| {
            matches!(ty.kind, TypeKind::Entity | TypeKind::Event)
                && subject_names.contains(ty.name.as_str())
        })
        .map(|ty| {
            let mut subject = json!({
                "name": ty.name,
                "description": ty.description,
                "source": ty.source,
                "attributes": attrs_value(subject_read_attributes(policies, &index, &ty.name)),
            });
            // A subject its metadata source names is picked among the
            // source's records, no longer pointed at in the statement's words
            // (2026-10-11): only such a contract's fingerprint moves.
            if matches!(ty.source, Some(SourceDecl::Metadata(_))) {
                subject["read"] = json!("pick");
            }
            subject
        })
        .collect();
    let metadata_sources: Vec<Value> = policies
        .shape
        .types
        .iter()
        .filter_map(|ty| match &ty.source {
            Some(SourceDecl::Metadata(source)) => Some(json!({
                "type": ty.name,
                "metadata": source.metadata,
                "attributes": source.attributes,
                "refs": source.refs,
            })),
            _ => None,
        })
        .collect();
    json!({
        "contract_version": CONTRACT_VERSION,
        "guidance": policies.prose.guidance,
        "voices": policies.assertion.voices,
        "claim_types": eligible_claims,
        "subject_types": subjects,
        "metadata_sources": metadata_sources,
        // The key predates the window reader; renaming it would change
        // every cached read's fingerprint for no change in what is asked.
        "passes": super::window::contract_value(policies),
    })
}

fn attrs_value(attrs: Vec<&AttrDecl>) -> Vec<Value> {
    attrs
        .into_iter()
        .filter(|attr| attr.derived.is_none())
        .map(|attr| serde_json::to_value(attr).expect("attribute declarations are serializable"))
        .collect()
}

/// Fields a read may use for one declared subject: all extracted fields for a
/// source-free record, only declared identity keys for a metadata projection.
pub(super) fn subject_read_attributes<'a>(
    policies: &'a OntologyPolicies,
    index: &TypeIndex<'a>,
    subject_name: &str,
) -> Vec<&'a AttrDecl> {
    let Some(subject) = policies.type_decl(subject_name) else {
        return Vec::new();
    };
    let attributes = index.extracted_attributes(subject_name);
    match subject.source.as_ref() {
        None => attributes,
        Some(SourceDecl::Metadata(_)) => {
            let identity = index.effective_identity(subject_name);
            attributes
                .into_iter()
                .filter(|attribute| {
                    attribute.derived.is_none() && identity.iter().any(|key| key == &attribute.name)
                })
                .collect()
        }
        Some(SourceDecl::Table(_)) => Vec::new(),
    }
}

/// Whether the reader reads statements of `ty`: an eligible claim, or a state or
/// relation whose holder RESOLVE decides (`OntologyTypeDecl::is_read_statement`).
pub(super) fn read_kind(ty: &OntologyTypeDecl, policies: &OntologyPolicies) -> bool {
    ty.is_read_statement(&policies.shape.types)
}

/// The fields a read statement of `decl` carries: those its shape implies (a
/// state's value, a relation's `to`: `OntologyTypeDecl::implied_fields`), then
/// its declared attributes.
pub fn statement_attributes<'a>(
    index: &TypeIndex<'a>,
    decl: &'a OntologyTypeDecl,
) -> Vec<Cow<'a, AttrDecl>> {
    decl.implied_fields()
        .into_iter()
        .map(Cow::Owned)
        .chain(
            index
                .extracted_attributes(&decl.name)
                .into_iter()
                .map(Cow::Borrowed),
        )
        .collect()
}

/// The declared types a document read makes no record of. It reads the
/// statement kinds `read_kind` admits (claims, and states and relations whose
/// holder RESOLVE decides), the types they are about, and every type RESOLVE
/// decides (About, the window's particulars), each with its `specializes`
/// kin; so a state or relation whose holder RESOLVE cannot decide, or an
/// entity with no identity criterion no statement is about, gets none unless
/// a source supplies it. Empty when the general extractor reads the
/// declaration, which reads every kind.
pub fn unread_types(
    policies: &OntologyPolicies,
) -> Vec<&crate::enrichment::ontology::OntologyTypeDecl> {
    if !policies.reads_documents() {
        return Vec::new();
    }
    let index = TypeIndex::from_policies(policies);
    let read: Vec<&str> = policies
        .shape
        .types
        .iter()
        .filter(|t| read_kind(t, policies) || decides(&index, &t.name))
        .flat_map(|c| std::iter::once(c.name.as_str()).chain(c.read_subject()))
        .collect();
    policies
        .shape
        .types
        .iter()
        .filter(|t| t.source.is_none())
        .filter(|t| {
            !read
                .iter()
                .any(|r| index.is_a(&t.name, r) || index.is_a(r, &t.name))
        })
        .collect()
}
