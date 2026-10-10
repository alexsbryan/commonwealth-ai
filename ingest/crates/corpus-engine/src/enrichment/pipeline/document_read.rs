// SPDX-License-Identifier: AGPL-3.0-or-later
//! Accountable per-document reading for recipe-declared claims.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod cache;
mod passes;
mod projection;
mod runner_support;
mod schema;
mod validation;

pub use cache::{
    cache_matches_chapter, checkpoint_processed_ids, context_fingerprint, contract_fingerprint,
    phase1_cache_matches, validate_checkpoint,
};
pub use passes::chosen_fields as reader_chosen_fields;
pub use passes::read as read_passes;
pub use projection::parse_response;
pub(super) use runner_support::{
    cache_text as runner_cache_text, load_exemplar_bank as runner_load_exemplar_bank,
    missing_source_documents as runner_missing_source_documents,
    phase1_inputs as runner_phase1_inputs, validate_response as runner_validate_response,
};
pub use schema::compose;
pub use validation::field_evidence_exists;
pub use validation::validate_and_stamp;

#[cfg(test)]
use schema::eligible_claim;

/// Internal attributes carried only between Phase 1 projection and RESOLVE.
pub const LOCAL_REF_ATTRIBUTE: &str = "__document_read_local_ref";
pub const SOURCE_DOCUMENT_ATTRIBUTE: &str = "__document_read_source_document";
pub const SUBJECT_FIELDS_ATTRIBUTE: &str = "__document_read_subject_fields";
/// Source-supported claim fields retained beside the flattened compatibility values.
pub const CLAIM_FIELDS_ATTRIBUTE: &str = "__document_read_fields";

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
