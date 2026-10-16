// SPDX-License-Identifier: AGPL-3.0-or-later
//! Accountable per-document reading for recipe-declared claims.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

mod cache;
mod line_classes;
mod pick;
mod projection;
mod runner_support;
mod schema;
mod validation;
mod window;

pub use cache::{
    cache_matches_chapter, checkpoint_processed_ids, context_fingerprint, contract_fingerprint,
    phase1_cache_matches, validate_checkpoint,
};
pub use line_classes::LineClasses;
pub use projection::parse_response;
pub(super) use runner_support::{
    cache_text as runner_cache_text, load_exemplar_bank as runner_load_exemplar_bank,
    missing_source_documents as runner_missing_source_documents,
    phase1_inputs as runner_phase1_inputs, validate_response as runner_validate_response,
};
pub use schema::{compose, statement_attributes, unread_types};
pub use validation::field_evidence_exists;
pub use validation::validate_and_stamp;
pub use window::read as read_documents;

#[cfg(test)]
use schema::read_kind;

/// Internal attributes carried only between Phase 1 projection and RESOLVE.
pub const LOCAL_REF_ATTRIBUTE: &str = "__document_read_local_ref";
pub const SOURCE_DOCUMENT_ATTRIBUTE: &str = "__document_read_source_document";
/// A claim's readings of its subject's fields, each a [`DocumentReadField`]
/// as its own are under [`CLAIM_FIELDS_ATTRIBUTE`]. RESOLVE keys the subject
/// on their supported values (`atlas/resolution_records.rs`), 3b binds a
/// sourced subject by them (`resolution_identity/source_subject.rs`), and
/// they stay on the claim, cited. No stage writes them onto the record.
pub const SUBJECT_FIELDS_ATTRIBUTE: &str = "__document_read_subject_fields";
/// The chapter-wide subject a window read gave a claim ([`SubjectScope::Chapter`]):
/// claims in different documents that carry one value are one particular as
/// the reader read them, and RESOLVE keys them together (`resolution_records`).
pub const CHAPTER_SUBJECT_ATTRIBUTE: &str = "__document_read_chapter_subject";
/// Source-supported claim fields retained beside the flattened compatibility values.
pub const CLAIM_FIELDS_ATTRIBUTE: &str = "__document_read_fields";
/// The type of the particular a document is about, on the claim projected
/// from a [`DocumentReadParticular`]: such a claim declares no claim kind, so
/// this is where its subject's type is read ([`about_type`]).
pub const ABOUT_ATTRIBUTE: &str = "__document_read_about";
/// The name a window read ([`SubjectScope::Chapter`]) gave a statement's
/// subject: a reference reading of that name in the same document lands on
/// the record RESOLVE put the statement on (`resolution_derived/read_fields.rs`).
/// Stays on the claim after RESOLVE, which shows it beside the statement and on
/// the record's card (`resolve_records::Described`) and never compares it.
pub const SUBJECT_NAME_ATTRIBUTE: &str = "__document_read_subject_name";

/// The type a claim projected from a [`DocumentReadParticular`] is about.
pub fn about_type(attributes: &Map<String, Value>) -> Option<&str> {
    attributes.get(ABOUT_ATTRIBUTE).and_then(Value::as_str)
}

/// The name a window read gave a statement's subject.
pub fn window_subject_name(attributes: &Map<String, Value>) -> Option<&str> {
    attributes
        .get(SUBJECT_NAME_ATTRIBUTE)
        .and_then(Value::as_str)
}

/// The other names a window read gave the things a statement names: each name
/// (its subject's, or a reading's value) mapped to the names a document says
/// the same thing also has, from the answer's typed `other_names` list on a
/// particular (`window_particulars.rs::other_names_for`). An object; absent
/// when no thing the statement names has another name. A name is never split:
/// "Input / Output" is one name. RESOLVE shows the other names
/// (`resolve_records::Described`), never compares them, and a reference
/// reading lands on a record by any of them (`resolution_derived/read_fields.rs`).
pub const OTHER_NAMES_ATTRIBUTE: &str = "__document_read_other_names";

/// The other names `attributes` records for `name` ([`OTHER_NAMES_ATTRIBUTE`]),
/// matched as identity values are (`fold_identity_value`); none when it
/// records none.
pub fn other_names<'a>(attributes: &'a Map<String, Value>, name: &str) -> Vec<&'a str> {
    attributes
        .get(OTHER_NAMES_ATTRIBUTE)
        .and_then(Value::as_object)
        .map_or_else(Vec::new, |others| other_names_in(others, name))
}

/// The other names the [`OTHER_NAMES_ATTRIBUTE`] object `others` records for
/// `name` (see [`other_names`]).
pub fn other_names_in<'a>(others: &'a Map<String, Value>, name: &str) -> Vec<&'a str> {
    let Some(want) = fold_identity_value(name) else {
        return Vec::new();
    };
    others
        .iter()
        .filter(|(n, _)| fold_identity_value(n).as_deref() == Some(want.as_str()))
        .flat_map(|(_, others)| others.as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect()
}

/// The value of each supported reading among `fields`, by field.
pub fn supported_values(fields: &BTreeMap<String, DocumentReadField>) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|(name, field)| match field {
            DocumentReadField::Supported { value, .. } => Some((name.clone(), value.clone())),
            DocumentReadField::Unknown { .. } => None,
        })
        .collect()
}

/// A claim's field readings as the projection wrote them, under
/// [`CLAIM_FIELDS_ATTRIBUTE`] or [`SUBJECT_FIELDS_ATTRIBUTE`].
pub fn field_readings(value: &Value) -> Result<BTreeMap<String, DocumentReadField>, String> {
    serde_json::from_value(value.clone()).map_err(|e| format!("field readings do not parse: {e}"))
}

/// Durable source result for one Phase-1 chapter batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentRead {
    /// Policy-derived prompt/schema fingerprint only; document context and model/request identity are separate.
    pub contract_fingerprint: String,
    /// Runner-stamped identity of the actual supplied document contexts.
    #[serde(default)]
    pub context_fingerprint: String,
    /// Exactly one entry for each source document supplied to the reader.
    pub documents: Vec<DocumentReadOutcome>,
    /// How far a claim's `subject_local_ref` reaches. Reads cached before
    /// 2026-10-13 carry none: a document's own.
    #[serde(default, skip_serializing_if = "SubjectScope::is_document")]
    pub subjects: SubjectScope,
}

/// The closed set of reaches a reader's local subject reference has.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubjectScope {
    /// One reference is one particular within one document; across documents
    /// it says nothing and RESOLVE alone decides (the passes reader).
    #[default]
    Document,
    /// One reference is one particular across the chapter's documents: the
    /// window reader read them together (`window.rs`).
    Chapter,
}

impl SubjectScope {
    pub fn is_document(&self) -> bool {
        *self == SubjectScope::Document
    }
}

/// One answer about one supplied document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentReadOutcome {
    pub document_id: String,
    pub status: DocumentReadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<DocumentReadClaim>,
    /// Claims the model emitted whose citation did not verify. Kept beside
    /// the read (never projected) so a refusal is inspectable, not silent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<DocumentReadRefusal>,
    /// Particulars the document is about that no claim of it is about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub particulars: Vec<DocumentReadParticular>,
}

/// A particular a document is about that no claim of the document is about
/// (`window_particulars.rs`): an issue that opens its thread is about a case
/// before any line says where the case stands. Its type is one RESOLVE
/// decides; `evidence` is the line that names it, and its identifying fields
/// are read there. It reaches RESOLVE as a statement of its own, so the
/// document belongs to the particular's record whether or not a line states a
/// claim kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentReadParticular {
    pub subject_type: String,
    pub subject_local_ref: String,
    pub subject_name: String,
    pub evidence: String,
    pub subject_fields: BTreeMap<String, DocumentReadField>,
    /// The other names a window read gave what it names
    /// ([`OTHER_NAMES_ATTRIBUTE`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub other_names: BTreeMap<String, Vec<String>>,
}

/// One claim refused by evidence verification, with the reason it failed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentReadRefusal {
    pub kind: String,
    pub reason: String,
}

/// The closed set of per-document outcomes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentReadStatus {
    Read,
    NothingApplicable,
    CouldNotJudge,
    NotRead,
}

/// A declared claim and its typed local subject, with evidence retained per field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentReadClaim {
    pub kind: String,
    pub content: String,
    pub subject_type: String,
    pub subject_local_ref: String,
    pub subject_name: String,
    /// The voice in the passage, when stated. It is never inferred from metadata.author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Exact source-body passage supporting the assertion.
    pub evidence: String,
    /// Non-derived attributes declared on the claim type.
    pub fields: BTreeMap<String, DocumentReadField>,
    /// Non-derived attributes declared on its subject type.
    pub subject_fields: BTreeMap<String, DocumentReadField>,
    /// The other names a window read gave what it names
    /// ([`OTHER_NAMES_ATTRIBUTE`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub other_names: BTreeMap<String, Vec<String>>,
}

/// A field value with evidence, or an explicit absence with a reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocumentReadField {
    Supported {
        value: Value,
        evidence: String,
        /// What chose the value and its precision (C3). Reads cached before
        /// 2026-10-09 carry none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<crate::enrichment::atlas::precision::SourcePrecision>,
    },
    Unknown {
        reason: String,
    },
}

#[cfg(test)]
#[path = "document_read_tests.rs"]
mod tests;
