// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-document stamps (`change.document`): every claim carries its OWN
//! source document's date, thread and id.
//!
//! A section can hold chunks of several documents — `enrich init
//! --from-corpus` groups mail by subject, so one section is a thread of many
//! messages — and a claim's document is therefore not its section's. It is
//! the document whose chunk text holds the claim's evidence anchor: the span
//! Phase 1 quoted, already snapped to source by `AnchorSnapProcessor`. A
//! section of ONE document needs no anchor. Anything else — an anchor in no
//! document, in several, a section with no document metadata — stamps
//! nothing and is recorded as a [`PhaseFailure`]: a guessed document would
//! put the claim on the wrong date and in the wrong thread (ARCH 6).
//!
//! Field NAMES come from the recipe; this module reads only what
//! [`DocumentFieldsDecl`] names. The rows are what the build's LanceDB loader
//! already returns ([`EnrichmentChunkRow`]): a document is the chunks sharing
//! a `source_doc_id`, its metadata the extractor's JSON each chunk carries.

use std::collections::{BTreeMap, HashMap};

use corpus_index::index::EnrichmentChunkRow;
use serde_json::{Map, Value};
use tracing::{debug, info, trace};

use super::atoms::Claim;
use crate::enrichment::ontology::{metadata_date, DocumentFieldsDecl, DocumentStamp};
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind, PipelinePhase};

/// One source document as a section holds it.
#[derive(Debug, Clone)]
struct SourceDocument {
    /// The index's identity of the document (`source_doc_id`).
    key: String,
    /// The extractor's metadata object, parsed once.
    fields: Map<String, Value>,
    /// Each chunk's text, whitespace-folded for the anchor match.
    texts: Vec<String>,
}

/// Section id → the source documents whose chunks the section holds.
#[derive(Debug, Clone, Default)]
pub struct SectionDocuments {
    by_section: HashMap<String, Vec<SourceDocument>>,
    /// Section ids in manifest order, so a walk over every document is
    /// deterministic (`each_document`).
    order: Vec<String>,
}

impl SectionDocuments {
    /// Group chunk rows into documents per section. `sections` pairs each
    /// section id with its chunk ids as `chapters.json` lists them; `rows`
    /// are the LanceDB rows for those ids. A row with no `source_doc_id`
    /// cannot be matched to its siblings, so it stands as a document of its
    /// own — which can only make a claim ambiguous, never misdate it.
    pub fn from_chunk_rows<'a>(
        sections: impl IntoIterator<Item = (&'a str, &'a [u64])>,
        rows: &[EnrichmentChunkRow],
    ) -> Self {
        let by_id: HashMap<u64, &EnrichmentChunkRow> = rows.iter().map(|r| (r.id, r)).collect();
        let mut by_section = HashMap::new();
        let mut order = Vec::new();
        for (section_id, chunk_ids) in sections {
            order.push(section_id.to_string());
            let mut docs: Vec<SourceDocument> = Vec::new();
            for id in chunk_ids {
                let Some(row) = by_id.get(id) else {
                    debug!(
                        section = section_id,
                        chunk = id,
                        "atlas/resolution documents: chunk listed in the manifest was not loaded"
                    );
                    continue;
                };
                let key = match &row.source_doc_id {
                    Some(k) => k.clone(),
                    None => {
                        debug!(
                            section = section_id,
                            chunk = id,
                            "atlas/resolution documents: chunk has no source_doc_id; it stands alone"
                        );
                        format!("chunk:{}", row.id)
                    }
                };
                let text = fold_ws(&row.content);
                match docs.iter_mut().find(|d| d.key == key) {
                    Some(d) => d.texts.push(text),
                    None => docs.push(SourceDocument {
                        fields: metadata_object(row, section_id),
                        key,
                        texts: vec![text],
                    }),
                }
            }
            by_section.insert(section_id.to_string(), docs);
        }
        Self { by_section, order }
    }

    /// How many documents, across every section.
    pub fn document_count(&self) -> usize {
        self.by_section.values().map(Vec::len).sum()
    }

    /// Every document ONCE, in manifest order, as `(section it is first
    /// seen in, document key, metadata fields)`.
    pub(super) fn each_document(&self) -> Vec<(&str, &str, &Map<String, Value>)> {
        let mut seen = std::collections::HashSet::new();
        self.order
            .iter()
            .flat_map(|s| {
                self.by_section
                    .get(s)
                    .into_iter()
                    .flatten()
                    .map(move |d| (s, d))
            })
            .filter(|(_, d)| seen.insert(d.key.as_str()))
            .map(|(s, d)| (s.as_str(), d.key.as_str(), &d.fields))
            .collect()
    }
}

/// The chunk's metadata JSON as an object. Anything else reads as no fields,
/// so every declared stamp on its claims is recorded as unreadable.
fn metadata_object(row: &EnrichmentChunkRow, section_id: &str) -> Map<String, Value> {
    match row
        .metadata_raw
        .as_deref()
        .map(serde_json::from_str::<Value>)
    {
        Some(Ok(Value::Object(m))) => m,
        other => {
            debug!(
                section = section_id,
                chunk = row.id,
                parsed = ?other.map(|r| r.map(|v| v.is_object())),
                "atlas/resolution documents: chunk metadata is not a JSON object"
            );
            Map::new()
        }
    }
}

/// Collapse every whitespace run to one space, so an anchor matches across
/// the line breaks a chunker or a model moved. RESOLVE checks citations with
/// it too (`resolve_records::cite_found`).
pub(super) fn fold_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What stamping did, for the build report.
#[derive(Debug, Clone, Default)]
pub struct DocumentStampReport {
    /// Claims looked at.
    pub claims: usize,
    /// Claims whose one document resolved.
    pub located: usize,
    /// Stamps written, per claim attribute.
    pub stamped: BTreeMap<&'static str, usize>,
    /// One record per claim left without its document, and per declared
    /// field a located document could not supply.
    pub failures: Vec<PhaseFailure>,
}

impl DocumentStampReport {
    /// Failures of one kind.
    pub fn count(&self, kind: PhaseFailureKind) -> usize {
        self.failures.iter().filter(|f| f.kind == kind).count()
    }

    /// One line for the resolve step's output.
    pub fn summary(&self) -> String {
        let stamped = if self.stamped.is_empty() {
            "none stamped".to_string()
        } else {
            self.stamped
                .iter()
                .map(|(k, n)| format!("{k} ×{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "document stamps: {} of {} claim(s) located ({stamped}); {} unresolved, {} unreadable field(s)",
            self.located,
            self.claims,
            self.count(PhaseFailureKind::UnresolvedClaimDocument),
            self.count(PhaseFailureKind::UnreadableDocumentField),
        )
    }
}

/// Stamp every claim with the fields `decl` names, read from the ONE source
/// document its evidence lands in. A claim whose document does not resolve,
/// or a field its document cannot supply, is left unstamped and recorded.
pub fn stamp_claim_documents(
    claims: &mut [Claim],
    documents: &SectionDocuments,
    decl: &DocumentFieldsDecl,
) -> DocumentStampReport {
    let mut report = DocumentStampReport {
        claims: claims.len(),
        ..Default::default()
    };
    for claim in claims.iter_mut() {
        let subject = format!("atom:{}", claim.id.as_str());
        let doc = match locate(claim, documents) {
            Ok(doc) => doc,
            Err(reason) => {
                debug!(
                    claim = %claim.id.as_str(),
                    %reason,
                    "atlas/resolution documents: claim's document unresolved; not stamped"
                );
                report.failures.push(failure(
                    subject,
                    PhaseFailureKind::UnresolvedClaimDocument,
                    reason,
                ));
                continue;
            }
        };
        report.located += 1;
        for (stamp, field) in decl.declared() {
            match read_stamp(&doc.fields, stamp, field) {
                Ok(value) => {
                    trace!(
                        claim = %claim.id.as_str(),
                        document = %doc.key,
                        attr = stamp.attr(),
                        %value,
                        "atlas/resolution documents: stamped"
                    );
                    claim
                        .attributes
                        .insert(stamp.attr().to_string(), Value::String(value));
                    *report.stamped.entry(stamp.attr()).or_default() += 1;
                }
                Err(why) => {
                    debug!(
                        claim = %claim.id.as_str(),
                        document = %doc.key,
                        attr = stamp.attr(),
                        %why,
                        "atlas/resolution documents: field unreadable; stamp left off"
                    );
                    report.failures.push(failure(
                        subject.clone(),
                        PhaseFailureKind::UnreadableDocumentField,
                        format!(
                            "document `{}`: {why}; `{}` not stamped",
                            doc.key,
                            stamp.attr()
                        ),
                    ));
                }
            }
        }
    }
    info!(
        claims = report.claims,
        located = report.located,
        unresolved = report.count(PhaseFailureKind::UnresolvedClaimDocument),
        unreadable = report.count(PhaseFailureKind::UnreadableDocumentField),
        "atlas/resolution documents: claims stamped from their own documents"
    );
    report
}

pub(super) fn failure(subject: String, kind: PhaseFailureKind, reason: String) -> PhaseFailure {
    PhaseFailure {
        phase: PipelinePhase::Questions, // resolution rides on the Questions cache
        subject,
        kind,
        reason,
        raw_response_head: None,
    }
}

/// The one document `claim`'s evidence lands in, or why there is not one.
fn locate<'d>(
    claim: &Claim,
    documents: &'d SectionDocuments,
) -> Result<&'d SourceDocument, String> {
    let mut found: Vec<&SourceDocument> = Vec::new();
    let mut misses: Vec<String> = Vec::new();
    for ev in &claim.evidence {
        let section = ev.chunk_id.as_str();
        let docs = documents
            .by_section
            .get(section)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let anchor = ev.passage_preview.as_deref().map(fold_ws);
        let hits: Vec<&SourceDocument> = match (docs, anchor.as_deref()) {
            ([], _) => {
                misses.push(format!("section `{section}` holds no document metadata"));
                continue;
            }
            ([only], _) => vec![only],
            (many, None | Some("")) => {
                misses.push(format!(
                    "section `{section}` holds {} documents and the evidence carries no anchor",
                    many.len()
                ));
                continue;
            }
            (many, Some(a)) => many
                .iter()
                .filter(|d| d.texts.iter().any(|t| t.contains(a)))
                .collect(),
        };
        if hits.is_empty() {
            misses.push(format!(
                "anchor {:?} is in none of section `{section}`'s {} documents",
                anchor.unwrap_or_default(),
                docs.len()
            ));
        }
        for h in hits {
            if !found.iter().any(|f| f.key == h.key) {
                found.push(h);
            }
        }
    }
    match found.as_slice() {
        [one] => Ok(*one),
        [] if misses.is_empty() => Err("the claim carries no evidence".to_string()),
        [] => Err(misses.join("; ")),
        many => Err(format!(
            "evidence lands in {} documents ({}); none is guessed",
            many.len(),
            many.iter()
                .map(|d| d.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The value `field` holds in a document's metadata, in the stamp's form: a
/// date as ISO 8601, a thread or id verbatim. The one reader of a stamp, for
/// claim stamping here and for RESOLVE's document fields.
pub fn read_stamp(
    fields: &Map<String, Value>,
    stamp: DocumentStamp,
    field: &str,
) -> Result<String, String> {
    let raw = match fields.get(field) {
        None | Some(Value::Null) => return Err(format!("no `{field}` field")),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(_) => return Err(format!("`{field}` is not a single value")),
    };
    if raw.is_empty() {
        return Err(format!("`{field}` is empty"));
    }
    match stamp {
        DocumentStamp::Date => metadata_date(&raw)
            .ok_or_else(|| format!("`{field}` = {raw:?} is neither RFC 2822 nor ISO 8601")),
        DocumentStamp::Thread | DocumentStamp::Id => Ok(raw),
    }
}
