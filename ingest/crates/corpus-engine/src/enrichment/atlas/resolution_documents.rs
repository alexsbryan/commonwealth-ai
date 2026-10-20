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

use super::atoms::{ChunkRef, Claim};
use crate::enrichment::ontology::{metadata_date, DocumentFieldsDecl, DocumentStamp};
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind, PipelinePhase};

/// Each document's canonical text, the string its chunks were cut from as the
/// corpus's text store holds it, keyed by the document's key
/// (`source_doc_id`). A document absent from it is read from its chunks, and
/// whoever built the map named why.
pub type DocumentTexts = BTreeMap<String, String>;

/// One source document as a section holds it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceDocument {
    /// The index's identity of the document (`source_doc_id`).
    pub(super) key: String,
    /// The source index's document id, absent when the row had no id.
    source_doc_id: Option<String>,
    /// The index's title for it, from its first chunk that has one.
    pub(super) title: Option<String>,
    /// The index's URL, from its first chunk that has one.
    url: Option<String>,
    /// The extractor's metadata object, parsed once.
    pub(super) fields: Map<String, Value>,
    /// Each chunk's text, whitespace-folded for the anchor match.
    texts: Vec<String>,
    /// Each chunk's installed id (`EnrichmentChunkRow::id`), parallel to
    /// `texts`: what the daemon's chunk endpoint resolves.
    #[serde(default)]
    chunk_ids: Vec<u64>,
    /// Original chunk bodies, retained for accountable reading.
    raw_texts: Vec<String>,
    /// The document's canonical text (`DocumentTexts`), when the corpus's
    /// text store gave it: the one string its chunks were cut from, so it
    /// holds no chunk title and no overlap read twice, and an extractor's
    /// offsets into it (`body_start`, `segments`) are exact.
    #[serde(default)]
    text: Option<String>,
    /// This section holds a part of the document: its chunks run across
    /// several windows (a document longer than a window, `window_manifest`).
    /// The reader reads the part (`raw_body`); RESOLVE anchors in the whole
    /// (`body`: the canonical text, or every section's chunks of it).
    #[serde(default)]
    part: bool,
}

impl SourceDocument {
    /// The text RESOLVE reads, whitespace-folded so the byte span of a folded
    /// anchor is a span of it: the canonical text when the store gave it,
    /// else the chunk texts joined by one space (chunks that overlap then
    /// repeat their overlap).
    pub(super) fn body(&self) -> String {
        match &self.text {
            Some(text) => fold_ws(text),
            None => self.texts.join(" "),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn source_doc_id(&self) -> Option<&str> {
        self.source_doc_id.as_deref()
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn metadata(&self) -> &Map<String, Value> {
        &self.fields
    }

    /// The text the reader reads: the canonical text when the store gave it,
    /// else the unnormalised chunks in manifest order joined by a blank line,
    /// each headed by its title and overlapping its neighbour as the index
    /// cut it (the substitution was named where the texts were read).
    pub fn raw_body(&self) -> String {
        if self.part {
            return self.raw_texts.join("\n\n");
        }
        match &self.text {
            Some(text) => text.clone(),
            None => self.raw_texts.join("\n\n"),
        }
    }

    /// The canonical text, when the store gave it: what an extractor's
    /// offsets index.
    pub fn stored_text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Whether this section holds only a part of the document (`part`).
    pub fn is_part(&self) -> bool {
        self.part
    }

    /// The installed chunk whose folded text holds `anchor` (already
    /// whitespace-folded) whole. Chunks that overlap can both hold it: the
    /// first in manifest order is the one. `None` when no single chunk
    /// holds it, a quote across a chunk boundary among them.
    pub fn chunk_holding(&self, anchor: &str) -> Option<u64> {
        if anchor.is_empty() {
            return None;
        }
        self.texts
            .iter()
            .zip(&self.chunk_ids)
            .find(|(t, _)| t.contains(anchor))
            .map(|(_, id)| *id)
    }

    /// The document's first installed chunk in manifest order: the chunk a
    /// record projected from the document's metadata cites, since every
    /// chunk of the document carries that metadata (the same tie rule as
    /// overlap in [`Self::chunk_holding`]).
    pub fn first_chunk(&self) -> Option<u64> {
        self.chunk_ids.first().copied()
    }

    /// Whether the document's chunks joined hold `anchor` (folded): a quote
    /// [`Self::chunk_holding`] cannot place is then across a chunk boundary.
    pub(super) fn chunks_hold(&self, anchor: &str) -> bool {
        self.texts.join(" ").contains(anchor)
    }
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
                let title = row
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty());
                let url = row.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
                let metadata = metadata_object(row, section_id);
                match docs.iter_mut().find(|d| d.key == key) {
                    Some(d) => {
                        d.texts.push(text);
                        d.chunk_ids.push(row.id);
                        d.raw_texts.push(row.content.clone());
                        for (name, value) in metadata {
                            d.fields.entry(name).or_insert(value);
                        }
                        if d.title.is_none() {
                            d.title = title.map(str::to_string);
                        }
                        if d.url.is_none() {
                            d.url = url.map(str::to_string);
                        }
                    }
                    None => docs.push(SourceDocument {
                        fields: metadata,
                        key,
                        source_doc_id: row.source_doc_id.clone(),
                        title: title.map(str::to_string),
                        url: url.map(str::to_string),
                        texts: vec![text],
                        chunk_ids: vec![row.id],
                        raw_texts: vec![row.content.clone()],
                        text: None,
                        part: false,
                    }),
                }
            }
            by_section.insert(section_id.to_string(), docs);
        }
        // A document whose chunks fall in several sections is read in parts;
        // each part anchors in all of its chunks, in section order, and the
        // chunk ids follow their texts.
        type Whole = (usize, Vec<String>, Vec<u64>);
        let mut whole: HashMap<String, Whole> = HashMap::new();
        for section in &order {
            for d in by_section.get(section).into_iter().flatten() {
                let e = whole.entry(d.key.clone()).or_default();
                e.0 += 1;
                e.1.extend(d.texts.iter().cloned());
                e.2.extend(d.chunk_ids.iter().copied());
            }
        }
        for d in by_section.values_mut().flatten() {
            if let Some((sections, texts, ids)) = whole.get(&d.key).filter(|(n, _, _)| *n > 1) {
                debug!(document = %d.key, sections, "atlas/resolution documents: a document read in parts across sections");
                d.part = true;
                d.texts = texts.clone();
                d.chunk_ids = ids.clone();
            }
        }
        Self { by_section, order }
    }

    /// Give each document the canonical text `texts` holds for its key. A
    /// document it lacks keeps reading its chunks; the reader of the store
    /// named why.
    pub fn with_texts(mut self, texts: &DocumentTexts) -> Self {
        let (mut given, mut from_chunks) = (0usize, 0usize);
        for d in self.by_section.values_mut().flatten() {
            match texts.get(&d.key) {
                Some(text) => {
                    d.text = Some(text.clone());
                    given += 1;
                }
                None => {
                    from_chunks += 1;
                    debug!(document = %d.key, "atlas/resolution documents: no canonical text; read from its chunks");
                }
            }
        }
        info!(
            given,
            from_chunks, "atlas/resolution documents: canonical texts attached"
        );
        self
    }

    /// How many documents, across every section.
    pub fn document_count(&self) -> usize {
        self.by_section.values().map(Vec::len).sum()
    }

    /// The source documents whose chunks make up one persisted chapter.
    pub fn documents_for_section(&self, section_id: &str) -> &[SourceDocument] {
        self.by_section
            .get(section_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Resolve an accountable read's document identity within its evidence section.
    pub fn document_for_section(
        &self,
        section_id: &str,
        document_id: &str,
    ) -> Option<&SourceDocument> {
        self.documents_for_section(section_id)
            .iter()
            .find(|document| document.key == document_id)
    }

    /// Every document by its key, as the first section holding it has it.
    pub(super) fn by_key(&self) -> HashMap<&str, &SourceDocument> {
        let mut out = HashMap::new();
        for d in self
            .order
            .iter()
            .filter_map(|s| self.by_section.get(s))
            .flatten()
        {
            out.entry(d.key.as_str()).or_insert(d);
        }
        out
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
/// Evidence that names its document (`source_doc_id`, stamped in 3b from the
/// read that cited it) is in that document, whatever else holds the same
/// words: equal text is not the same occurrence. Other evidence is placed by
/// its anchor, in the one document of its section that holds it. Stamping,
/// derivation and RESOLVE all place a claim with this.
pub(super) fn locate<'d>(
    claim: &Claim,
    documents: &'d SectionDocuments,
) -> Result<&'d SourceDocument, String> {
    let mut found: Vec<&SourceDocument> = Vec::new();
    let mut misses: Vec<String> = Vec::new();
    for ev in &claim.evidence {
        match evidence_documents(ev, documents) {
            Ok(hits) => {
                for h in hits {
                    if !found.iter().any(|f| f.key == h.key) {
                        found.push(h);
                    }
                }
            }
            Err(miss) => misses.push(miss),
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

/// The documents ONE evidence lands in (never empty), or why it lands in
/// none. Evidence naming its document is in that document of its section
/// (`chunk_id`); other evidence is in its section's one document, or in each
/// of its several documents whose chunk text holds the anchor. [`locate`]
/// and index-chunk placement (`resolution_placement`) both read it.
pub(super) fn evidence_documents<'d>(
    ev: &ChunkRef,
    documents: &'d SectionDocuments,
) -> Result<Vec<&'d SourceDocument>, String> {
    let section = ev.chunk_id.as_str();
    if let Some(named) = ev.source_doc_id.as_deref() {
        return match documents.document_for_section(section, named) {
            Some(doc) => Ok(vec![doc]),
            None => Err(format!(
                "the evidence names document `{named}`, which section `{section}` does not hold"
            )),
        };
    }
    let docs = documents
        .by_section
        .get(section)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let anchor = ev.passage_preview.as_deref().map(fold_ws);
    let hits: Vec<&SourceDocument> = match (docs, anchor.as_deref()) {
        ([], _) => return Err(format!("section `{section}` holds no document metadata")),
        ([only], _) => vec![only],
        (many, None | Some("")) => {
            return Err(format!(
                "section `{section}` holds {} documents and the evidence carries no anchor",
                many.len()
            ))
        }
        (many, Some(a)) => many
            .iter()
            .filter(|d| d.texts.iter().any(|t| t.contains(a)))
            .collect(),
    };
    if hits.is_empty() {
        return Err(format!(
            "anchor {:?} is in none of section `{section}`'s {} documents",
            anchor.unwrap_or_default(),
            docs.len()
        ));
    }
    Ok(hits)
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

#[cfg(test)]
#[path = "resolution_documents_tests.rs"]
mod tests;
