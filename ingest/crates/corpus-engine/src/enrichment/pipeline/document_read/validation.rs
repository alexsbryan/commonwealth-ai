use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Map, Value};

use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{AttrDecl, AttrFamily, OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::atlas::SectionExtraction;
use crate::enrichment::pipeline::types::ChapterInput;
use crate::error::{Error, Result};

use super::{DocumentReadField, DocumentReadOutcome, DocumentReadStatus};

/// Validate outcomes against the runner's actual inputs. Structural problems
/// (unknown, duplicate, or missing document ids; a status inconsistent with
/// its claims) refuse the chapter; a claim whose citation does not verify is
/// refused BY NAME, kept beside the read as `refused`, and never projected.
/// A `read` whose every claim is refused becomes `could_not_judge`, and the
/// compatibility sketches are rebuilt from the surviving claims so nothing
/// refused is ever visible as data.
pub fn validate_and_stamp(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    extraction: &mut SectionExtraction,
) -> Result<()> {
    if !policies.reads_documents() {
        return Err(Error::InvalidInput(
            "document-read output arrived for a declaration that reads no documents".into(),
        ));
    }
    let expected_contract = super::cache::contract_fingerprint(policies);
    if chapter.source_documents.is_empty() {
        return Err(Error::InvalidInput(format!(
            "chapter `{}` has no hydrated source documents for declared document reading",
            chapter.chapter_id
        )));
    }
    let mut actual: HashMap<&str, &SourceDocument> = HashMap::new();
    for document in &chapter.source_documents {
        if actual.insert(document.key(), document).is_some() {
            return Err(Error::InvalidInput(format!(
                "chapter `{}` hydrated duplicate source document id `{}`",
                chapter.chapter_id,
                document.key()
            )));
        }
    }
    let mut reproject = false;
    {
        let read = extraction.document_read.as_mut().ok_or_else(|| {
            Error::Serialization("document-reading policy produced no document-read carrier".into())
        })?;
        if read.contract_fingerprint != expected_contract {
            return Err(Error::Serialization(
                "document-read contract fingerprint does not match the active ontology".into(),
            ));
        }
        if read.documents.len() != actual.len() {
            return Err(Error::Serialization(format!(
                "document-read returned {} outcomes for {} supplied documents",
                read.documents.len(),
                actual.len()
            )));
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for outcome in &mut read.documents {
            if !seen.insert(outcome.document_id.clone()) {
                return Err(Error::Serialization(format!(
                    "document-read returned duplicate outcome for `{}`",
                    outcome.document_id
                )));
            }
            let document = actual.get(outcome.document_id.as_str()).ok_or_else(|| {
                Error::Serialization(format!(
                    "document-read named `{}` which was not supplied to the runner",
                    outcome.document_id
                ))
            })?;
            reproject |= validate_outcome(outcome, document, policies)?;
        }
        if seen.len() != actual.len() {
            let missing: Vec<&str> = actual
                .keys()
                .copied()
                .filter(|id| !seen.contains(*id))
                .collect();
            return Err(Error::Serialization(format!(
                "document-read omitted supplied document outcome(s): {}",
                missing.join(", ")
            )));
        }
        read.context_fingerprint = super::cache::context_fingerprint(chapter);
    }
    if reproject {
        extraction.claims.clear();
        extraction.entities_introduced.clear();
        super::projection::project_compatibility_sketches(extraction, policies)?;
    }
    Ok(())
}

/// Validate one outcome and its claims. Returns whether any claim was
/// refused, which requires compatibility reprojection.
fn validate_outcome(
    outcome: &mut DocumentReadOutcome,
    document: &SourceDocument,
    policies: &OntologyPolicies,
) -> Result<bool> {
    match outcome.status {
        DocumentReadStatus::Read if outcome.claims.is_empty() && outcome.particulars.is_empty() => {
            return Err(Error::Serialization(format!(
                "document `{}` says read but emitted no claims and no particulars",
                outcome.document_id
            )))
        }
        DocumentReadStatus::NothingApplicable
            if !outcome.claims.is_empty()
                || !outcome.particulars.is_empty()
                || outcome.reason.as_deref().is_none_or(str::is_empty) =>
        {
            return Err(Error::Serialization(format!(
                "document `{}` must give a reason and zero claims for nothing_applicable",
                outcome.document_id
            )))
        }
        DocumentReadStatus::CouldNotJudge | DocumentReadStatus::NotRead
            if !outcome.claims.is_empty()
                || !outcome.particulars.is_empty()
                || outcome.reason.as_deref().is_none_or(str::is_empty) =>
        {
            return Err(Error::Serialization(format!(
                "document `{}` must give a reason and zero claims for {:?}",
                outcome.document_id, outcome.status
            )))
        }
        _ => {}
    }
    let index = TypeIndex::from_policies(policies);
    let eligible: HashMap<&str, _> = policies
        .shape
        .types
        .iter()
        .filter(|ty| super::schema::read_kind(ty, policies))
        .map(|ty| (ty.name.as_str(), ty))
        .collect();
    let before = outcome.claims.len();
    let mut kept = Vec::with_capacity(before);
    let mut refused = Vec::new();
    let mut local_subjects: HashMap<(String, String), (String, Map<String, Value>)> =
        HashMap::new();
    for claim in outcome.claims.drain(..) {
        let subject_key = (claim.subject_type.clone(), claim.subject_local_ref.clone());
        let supported: Map<String, Value> = claim
            .subject_fields
            .iter()
            .filter_map(|(name, field)| match field {
                DocumentReadField::Supported { value, .. } => Some((name.clone(), value.clone())),
                DocumentReadField::Unknown { .. } => None,
            })
            .collect();
        let refusal = match local_subjects.get(&subject_key) {
            Some((name, fields)) if name != &claim.subject_name || fields != &supported => {
                Some(format!(
                    "local subject reference `{}` changes label or fields within document `{}`; the first occurrence stands",
                    claim.subject_local_ref, outcome.document_id
                ))
            }
            None => {
                local_subjects.insert(subject_key, (claim.subject_name.clone(), supported));
                None
            }
            Some(_) => None,
        };
        let refusal =
            refusal.or_else(|| verify_claim(&claim, document, policies, &eligible, &index).err());
        match refusal {
            None => kept.push(claim),
            Some(reason) => {
                tracing::warn!(
                    document = %outcome.document_id,
                    kind = %claim.kind,
                    reason = %reason,
                    "phase1.document_read_claim_refused"
                );
                refused.push(super::DocumentReadRefusal {
                    kind: claim.kind,
                    reason,
                });
            }
        }
    }
    // A particular no claim is about: a claim sharing its reference is about
    // it, so the reader would not have emitted it.
    let offered = outcome.particulars.len();
    let mut particulars = Vec::with_capacity(offered);
    for particular in outcome.particulars.drain(..) {
        let key = (
            particular.subject_type.clone(),
            particular.subject_local_ref.clone(),
        );
        let refusal = if local_subjects.contains_key(&key) {
            Some(format!(
                "particular `{}` is one a claim of document `{}` is about",
                particular.subject_local_ref, outcome.document_id
            ))
        } else {
            verify_particular(&particular, document, policies, &index).err()
        };
        match refusal {
            None => particulars.push(particular),
            Some(reason) => {
                tracing::warn!(document = %outcome.document_id, of = %particular.subject_type, reason = %reason, "phase1.document_read_particular_refused");
                refused.push(super::DocumentReadRefusal {
                    kind: particular.subject_type,
                    reason,
                });
            }
        }
    }
    let changed = before > kept.len() || offered > particulars.len();
    outcome.claims = kept;
    outcome.particulars = particulars;
    // A reader's own refusals (the window reader's) stay beside validation's.
    outcome.refused.extend(refused);
    if outcome.status == DocumentReadStatus::Read
        && outcome.claims.is_empty()
        && outcome.particulars.is_empty()
    {
        outcome.status = DocumentReadStatus::CouldNotJudge;
        outcome.reason = Some(format!(
            "every claim ({before}) and particular ({offered}) was refused; `refused` names why \
             each one was"
        ));
    }
    Ok(changed)
}

/// Why a particular no claim is about cannot be projected, or `Ok(())` when
/// its type is one RESOLVE decides and its words and fields verify against
/// THIS document.
fn verify_particular(
    particular: &super::DocumentReadParticular,
    document: &SourceDocument,
    policies: &OntologyPolicies,
    index: &TypeIndex,
) -> std::result::Result<(), String> {
    let of = particular.subject_type.as_str();
    if !crate::enrichment::atlas::resolution_records::decides(index, of)
        && !crate::enrichment::atlas::resolution_records::decides_mentions(index, of)
    {
        return Err(format!(
            "`{of}` is not a type RESOLVE decides, so no particular of it is read"
        ));
    }
    if particular.subject_local_ref.trim().is_empty() || particular.subject_name.trim().is_empty() {
        return Err(format!("a particular of `{of}` lacks its local reference"));
    }
    refuse_voice(
        policies,
        &particular.subject_name,
        &particular.subject_local_ref,
    )?;
    if !claim_evidence_exists(document, &particular.evidence) {
        return Err(format!(
            "the line naming it is not an exact passage in document `{}`",
            document.key()
        ));
    }
    validate_fields(
        &particular.subject_fields,
        super::schema::subject_read_attributes(policies, index, of),
        &format!("subject `{of}` fields"),
    )
    .map_err(|error| error.to_string())?;
    fields_cited(&particular.subject_fields, document)
}

/// Why a subject cannot be a record: its name or the words of its `l{n}:`
/// local reference are a declared voice — the reader's
/// side of what the general extractor's parser enforces (`VoicesDecl::is_voice`).
fn refuse_voice(
    policies: &OntologyPolicies,
    name: &str,
    local_ref: &str,
) -> std::result::Result<(), String> {
    let voices = &policies.assertion.voices;
    match [Some(name), local_ref_words(local_ref)]
        .into_iter()
        .flatten()
        .find(|n| voices.is_voice(n))
    {
        Some(name) => Err(format!(
            "`{name}` is a declared voice (voices.not_entities), never a particular"
        )),
        None => Ok(()),
    }
}

/// The words of an `l{n}:{words}` local reference, the form the deleted passes
/// reader gave a particular; `None` for any other (a window's `sec_1/p1`).
fn local_ref_words(local_ref: &str) -> Option<&str> {
    let (line, words) = local_ref.strip_prefix('l')?.split_once(':')?;
    (!line.is_empty() && line.bytes().all(|b| b.is_ascii_digit())).then_some(words)
}

/// `Ok(())` when every supported field's evidence is in the document.
fn fields_cited(
    fields: &BTreeMap<String, DocumentReadField>,
    document: &SourceDocument,
) -> std::result::Result<(), String> {
    for field in fields.values() {
        if let DocumentReadField::Supported { evidence, .. } = field {
            if !field_evidence_exists(document, evidence) {
                return Err(format!(
                    "field evidence {evidence:?} is not present in document `{}` body or metadata",
                    document.key()
                ));
            }
        }
    }
    Ok(())
}

/// Whether `speaker` is words `document`'s body holds, whitespace and case
/// aside: what a claim's voice must be. The window reader keeps a voice only
/// when it is (`window_voice.rs`), so this check never costs it a claim.
pub(super) fn speaker_in_body(document: &SourceDocument, speaker: &str) -> bool {
    let speaker = fold_ws(speaker);
    !speaker.is_empty() && fold_ws(&document.raw_body()).contains(&speaker)
}

/// Why one claim cannot be projected, or `Ok(())` when its citation, voice
/// and fields all verify against THIS document.
fn verify_claim(
    claim: &super::DocumentReadClaim,
    document: &SourceDocument,
    policies: &OntologyPolicies,
    eligible: &HashMap<&str, &crate::enrichment::ontology::OntologyTypeDecl>,
    index: &TypeIndex,
) -> std::result::Result<(), String> {
    let decl = eligible
        .get(claim.kind.as_str())
        .ok_or_else(|| format!("claim kind `{}` is not eligible in this recipe", claim.kind))?;
    let subject_type = decl.read_subject().unwrap_or_default();
    if claim.subject_type != subject_type
        || claim.subject_local_ref.trim().is_empty()
        || claim.subject_name.trim().is_empty()
        || claim.content.trim().is_empty()
    {
        return Err(format!(
            "claim `{}` lacks its declared subject or local reference",
            claim.kind
        ));
    }
    refuse_voice(policies, &claim.subject_name, &claim.subject_local_ref)?;
    if !claim_evidence_exists(document, &claim.evidence) {
        return Err(format!(
            "evidence is not an exact passage in document `{}` nor one of its declared identity fields",
            document.key()
        ));
    }
    if claim
        .speaker
        .as_deref()
        .is_some_and(|speaker| !speaker_in_body(document, speaker))
    {
        return Err(format!(
            "claim `{}` names a speaker not present in the source body; metadata.author is context only",
            claim.kind
        ));
    }
    let attrs = super::schema::statement_attributes(index, decl);
    validate_fields(
        &claim.fields,
        attrs.iter().map(|a| &**a).collect(),
        &format!("claim `{}` fields", decl.name),
    )
    .map_err(|error| error.to_string())?;
    let subject_attrs = super::schema::subject_read_attributes(policies, index, subject_type);
    validate_fields(
        &claim.subject_fields,
        subject_attrs,
        &format!("subject `{subject_type}` fields"),
    )
    .map_err(|error| error.to_string())?;
    fields_cited(&claim.fields, document)?;
    fields_cited(&claim.subject_fields, document)
}

/// A claim may cite the document itself by an exact identity value (its key,
/// source id, url or title) — exact match only; a paraphrase still refuses.
fn document_identity_matches(document: &SourceDocument, folded_evidence: &str) -> bool {
    [
        document.key(),
        document.source_doc_id().unwrap_or_default(),
        document.url().unwrap_or_default(),
        document.title().unwrap_or_default(),
    ]
    .iter()
    .any(|value| !value.is_empty() && fold_ws(value) == folded_evidence)
}

pub(super) fn claim_evidence_exists(document: &SourceDocument, evidence: &str) -> bool {
    let folded = fold_ws(evidence);
    !folded.is_empty()
        && (fold_ws(&document.raw_body()).contains(&folded)
            || document_identity_matches(document, &folded))
}

pub(super) fn validate_fields(
    fields: &BTreeMap<String, DocumentReadField>,
    attrs: Vec<&AttrDecl>,
    context: &str,
) -> Result<()> {
    let expected: BTreeSet<&str> = attrs
        .iter()
        .filter(|attr| attr.derived.is_none())
        .map(|attr| attr.name.as_str())
        .collect();
    let actual: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
    if actual != expected {
        return Err(Error::Serialization(format!(
            "{context} must name every non-derived field exactly; expected {:?}, got {:?}",
            expected, actual
        )));
    }
    for attr in attrs.into_iter().filter(|attr| attr.derived.is_none()) {
        match &fields[&attr.name] {
            DocumentReadField::Supported {
                value, evidence, ..
            } => {
                if evidence.trim().is_empty() || !value_matches(&attr.family, value) {
                    return Err(Error::Serialization(format!(
                        "{context} field `{}` has an unsupported value or no field evidence",
                        attr.name
                    )));
                }
            }
            DocumentReadField::Unknown { reason } if reason.trim().is_empty() => {
                return Err(Error::Serialization(format!(
                    "{context} field `{}` has an empty unknown reason",
                    attr.name
                )))
            }
            DocumentReadField::Unknown { .. } => {}
        }
    }
    Ok(())
}

fn value_matches(family: &AttrFamily, value: &Value) -> bool {
    match family {
        AttrFamily::Text { values } => value
            .as_str()
            .is_some_and(|text| values.is_empty() || values.iter().any(|allowed| allowed == text)),
        AttrFamily::Quantity { .. } => value.is_number(),
        AttrFamily::Time { .. } | AttrFamily::Ref { .. } => value.as_str().is_some(),
    }
}

/// Whether recorded field evidence still exists in the source document.
pub fn field_evidence_exists(document: &SourceDocument, evidence: &str) -> bool {
    let folded = fold_ws(evidence);
    if folded.is_empty() {
        return false;
    }
    if fold_ws(&document.raw_body()).contains(&folded) {
        return true;
    }
    document.metadata().values().any(|value| {
        metadata_strings(value)
            .into_iter()
            .any(|text| fold_ws(&text).contains(&folded))
    })
}

pub(super) fn metadata_strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Number(number) => vec![number.to_string()],
        Value::Array(values) => values.iter().flat_map(metadata_strings).collect(),
        Value::Object(values) => values.values().flat_map(metadata_strings).collect(),
        Value::Null | Value::Bool(_) => Vec::new(),
    }
}

fn fold_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
