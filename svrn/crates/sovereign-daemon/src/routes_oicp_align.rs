// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /oicp/v1/align` — where a quotation stands in the stored texts, and
//! how it differs (OICP v0.5 §2.3, ADDRESSED_TEXT §5.3).
//!
//! Candidates come from the index that exists: each corpus's lexical search
//! over the quote returns its top `candidate_k` chunks (`align.toml`), and
//! the stored texts they were cut from are what `quote_align::align` reads.
//! Recall is the search's; precision is the aligner's.
//!
//! Mounted in `general` under `client_auth`, like every OICP route. Which
//! corpora a caller reaches is [`readable_corpora`], and every requested corpus
//! lands in exactly one of `corpora` (with its library digest) and
//! `corpora_unavailable` (with its reason).

use std::collections::HashMap;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use corpus_index::index::{CorpusIndex, DocumentRecord};
use corpus_index::ingest_port::daemon::IngestPort;
use kernel_types::Sha256Hash;
use oicp_types::evidence::{
    reasons, AlignRequest, AlignResponse, Alignment, CorpusTexts, Difference, DifferenceKind, Span,
    Unavailable,
};
use quote_align::{code_point_slice, AlignConfig, QuoteAlignment, QuoteEdit, QuoteEditKind};
use sovereign_contracts::principal::AttachedPrincipal;

use crate::oicp_evidence::{absence_reason, open_for_reading, readable_corpora, wire_document};
use crate::routes_internal::ErrorBody;
use crate::state::AppState;

type Refusal = (StatusCode, Json<ErrorBody>);

fn refuse(status: StatusCode, error: impl Into<String>) -> Refusal {
    (
        status,
        Json(ErrorBody {
            error: error.into(),
        }),
    )
}

/// One aligner difference as the wire's. The one mapping, exhaustive in
/// both directions of the closed set (§2.3): a kind added to the aligner
/// does not compile until it is named here.
pub(crate) fn wire_difference(edit: &QuoteEdit) -> Difference {
    let kind = match edit.kind {
        QuoteEditKind::Substituted => DifferenceKind::Substituted,
        QuoteEditKind::Added => DifferenceKind::Added,
        QuoteEditKind::Omitted => DifferenceKind::Omitted,
        QuoteEditKind::Elided => DifferenceKind::Elided,
        QuoteEditKind::Bracketed => DifferenceKind::Bracketed,
    };
    Difference {
        kind,
        quote: [edit.quote.start as u64, edit.quote.end as u64],
        source: [edit.source.start as u64, edit.source.end as u64],
    }
}

/// A text the lexical search led to, and the documents whose chunks matched.
struct Candidate {
    text: String,
    /// `(corpus_id, record)` of each document a matched chunk was cut from.
    matched: Vec<(String, DocumentRecord)>,
}

/// What one corpus contributed: its digest, and the `(text, matched)` pairs.
struct CorpusRead {
    digest: Sha256Hash,
    texts: Vec<(Sha256Hash, String, Vec<DocumentRecord>)>,
}

/// `POST /oicp/v1/align`.
pub async fn align(
    State(state): State<AppState>,
    principal: Option<Extension<AttachedPrincipal>>,
    Json(req): Json<AlignRequest>,
) -> Result<Json<AlignResponse>, Refusal> {
    if req.quote.trim().is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "empty quote"));
    }
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            crate::hosted_ingest::NO_INGEST,
        ));
    };
    let cfg = AlignConfig::shipped().map_err(|e| {
        tracing::error!(error = %e, "oicp align: the shipped align.toml does not load");
        refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let installed = engine.installed_indexes().await.map_err(|e| {
        tracing::warn!(error = %e, "oicp align: installed corpora unread");
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("installed corpora unread: {e}"),
        )
    })?;
    let readable = readable_corpora(
        &state,
        principal.as_ref().map(|Extension(p)| &p.0),
        &installed,
    );

    let mut corpora_unavailable: Vec<Unavailable> = Vec::new();
    let targets: Vec<String> = if req.corpora.is_empty() {
        readable.clone()
    } else {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for id in req.corpora.iter().filter(|id| seen.insert(id.as_str())) {
            if readable.contains(id) {
                out.push(id.clone());
            } else {
                tracing::debug!(corpus = %id, "oicp align: requested corpus not held for this caller");
                corpora_unavailable.push(Unavailable {
                    corpus_id: id.clone(),
                    reason: reasons::CORPUS_NOT_HELD.into(),
                });
            }
        }
        out
    };

    let mut corpora: Vec<CorpusTexts> = Vec::new();
    let mut candidates: Vec<(Sha256Hash, Candidate)> = Vec::new();
    for corpus_id in &targets {
        match read_corpus(engine.as_ref(), corpus_id, &req.quote, cfg.candidate_k).await {
            Ok(read) => {
                corpora.push(CorpusTexts {
                    corpus_id: corpus_id.clone(),
                    texts_digest: read.digest.to_hex(),
                });
                for (name, text, records) in read.texts {
                    let matched = records.into_iter().map(|r| (corpus_id.clone(), r));
                    match candidates.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, c)) => c.matched.extend(matched),
                        None => candidates.push((
                            name,
                            Candidate {
                                text,
                                matched: matched.collect(),
                            },
                        )),
                    }
                }
            }
            Err(reason) => {
                tracing::info!(corpus = %corpus_id, %reason, "oicp align: corpus unavailable");
                corpora_unavailable.push(Unavailable {
                    corpus_id: corpus_id.clone(),
                    reason,
                });
            }
        }
    }

    let texts: Vec<&str> = candidates.iter().map(|(_, c)| c.text.as_str()).collect();
    let aligned = quote_align::align(&req.quote, &texts, &cfg);
    let context = req.effective_context() as usize;
    let mut alignments = Vec::new();
    for a in aligned
        .alignments
        .iter()
        .take(req.effective_limit() as usize)
    {
        let (_, candidate) = &candidates[a.text];
        alignments.push(wire_alignment(a, candidate, context).map_err(|e| {
            tracing::error!(error = %e, "oicp align: a stored record does not map onto the wire");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, e)
        })?);
    }
    tracing::info!(
        readable = readable.len(),
        corpora = corpora.len(),
        unavailable = corpora_unavailable.len(),
        candidate_texts = candidates.len(),
        anchors = aligned.anchors,
        below_floor = aligned.below_floor,
        found = aligned.alignments.len(),
        returned = alignments.len(),
        "oicp align: answered"
    );
    Ok(Json(AlignResponse {
        alignments,
        aligner: quote_align::ALIGNER_ID.into(),
        corpora,
        corpora_unavailable,
    }))
}

/// One corpus's digest and the stored texts its lexical search led to, or
/// the reason it cannot be aligned against.
async fn read_corpus(
    engine: &dyn IngestPort,
    corpus_id: &str,
    quote: &str,
    k: u32,
) -> Result<CorpusRead, String> {
    let index = open_for_reading(engine, corpus_id)
        .await
        .map_err(|e| format!("corpus unreadable: {e}"))?;
    let digest = match engine.texts_digest(&index).await {
        Ok(Ok(d)) => d,
        Ok(Err(absence)) => return Err(absence_reason(absence).into()),
        Err(e) => return Err(format!("texts unreadable: {e}")),
    };
    let hits = index
        .search(&[], quote, k as usize)
        .await
        .map_err(|e| format!("lexical search failed: {e}"))?;
    let ids: Vec<u64> = hits.iter().filter_map(|h| h.chunk_id).collect();
    let names = index
        .chunk_text_sha256s(&ids)
        .await
        .map_err(|e| format!("chunk text names unread: {e}"))?;
    // Each distinct text, in hit order, with the source ids of its matched
    // chunks (`None`: the chunk names no source).
    let mut order: Vec<(Sha256Hash, Vec<Option<String>>)> = Vec::new();
    let mut unnamed = 0usize;
    for hit in &hits {
        let Some(name) = hit.chunk_id.and_then(|id| names.get(&id)).copied() else {
            unnamed += 1;
            continue;
        };
        let source = hit.source_doc_id.clone();
        match order.iter_mut().find(|(n, _)| *n == name) {
            Some((_, sources)) => sources.push(source),
            None => order.push((name, vec![source])),
        }
    }
    let mut texts = Vec::new();
    for (name, sources) in order {
        let stored = match index.text(&name).await {
            Ok(Ok(s)) => s,
            Ok(Err(absence)) => {
                tracing::debug!(corpus = %corpus_id, %name, reason = absence_reason(absence), "oicp align: a matched chunk's text is not readable; skipped");
                continue;
            }
            Err(e) => return Err(format!("text store unreadable: {e}")),
        };
        let matched = matched_documents(corpus_id, &name, &stored.documents, &sources);
        texts.push((name, stored.text, matched));
    }
    tracing::debug!(
        corpus = %corpus_id,
        hits = hits.len(),
        hits_naming_no_text = unnamed,
        texts = texts.len(),
        "oicp align: candidates read"
    );
    Ok(CorpusRead { digest, texts })
}

/// The documents of `records` whose chunks matched: those whose `source_id`
/// a matched chunk names. A chunk that names no source (or one no record
/// carries) leaves the whole set as the candidates, and says so.
fn matched_documents(
    corpus_id: &str,
    name: &Sha256Hash,
    records: &[DocumentRecord],
    sources: &[Option<String>],
) -> Vec<DocumentRecord> {
    let named: Vec<DocumentRecord> = records
        .iter()
        .filter(|r| sources.iter().flatten().any(|s| *s == r.source_id))
        .cloned()
        .collect();
    if named.is_empty() {
        tracing::debug!(
            corpus = %corpus_id,
            %name,
            ?sources,
            "oicp align: no matched chunk names one of this text's documents; every document of the text is a candidate"
        );
        return records.to_vec();
    }
    named
}

/// One aligner result on the wire. The document is the lowest
/// `(corpus_id, source.id, ordinal)` among those whose chunk matched.
fn wire_alignment(
    a: &QuoteAlignment,
    candidate: &Candidate,
    context: usize,
) -> Result<Alignment, String> {
    let (corpus_id, record) = candidate
        .matched
        .iter()
        .min_by(|(ca, ra), (cb, rb)| {
            (ca, &ra.source_id, ra.ordinal).cmp(&(cb, &rb.source_id, rb.ordinal))
        })
        .ok_or("an aligned text has no document")?;
    let text = candidate.text.as_str();
    let len = text.chars().count();
    let slice = |r: std::ops::Range<usize>| {
        code_point_slice(text, r.clone())
            .map(str::to_string)
            .ok_or_else(|| format!("aligner range {r:?} is outside its {len}-code-point text"))
    };
    let (start, end) = (a.source.start, a.source.end);
    Ok(Alignment {
        span: Span {
            corpus_id: corpus_id.clone(),
            document: wire_document(record)?,
            start: start as u64,
            end: end as u64,
            exact: slice(start..end)?,
            prefix: slice(start.saturating_sub(context)..start)?,
            suffix: slice(end..(end + context).min(len))?,
        },
        differences: a.edits.iter().map(wire_difference).collect(),
        coverage: a.coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping is one to one: no aligner kind becomes another wire kind,
    /// and every wire kind is reached.
    #[test]
    fn every_aligner_kind_is_its_wire_kind() {
        let kinds = [
            QuoteEditKind::Substituted,
            QuoteEditKind::Added,
            QuoteEditKind::Omitted,
            QuoteEditKind::Elided,
            QuoteEditKind::Bracketed,
        ];
        let wire: Vec<DifferenceKind> = kinds
            .iter()
            .map(|&kind| {
                wire_difference(&QuoteEdit {
                    kind,
                    quote: 1..3,
                    source: 4..9,
                })
                .kind
            })
            .collect();
        assert_eq!(wire, DifferenceKind::ALL.to_vec());
        let d = wire_difference(&QuoteEdit {
            kind: QuoteEditKind::Omitted,
            quote: 2..2,
            source: 5..11,
        });
        assert_eq!((d.quote, d.source), ([2, 2], [5, 11]));
    }

    #[test]
    fn a_span_binds_exactly_with_its_context() {
        let record = DocumentRecord {
            text_sha256: Sha256Hash::of_str("x"),
            extractor: "plain_text@1".into(),
            source_id: "b".into(),
            source_sha256: None,
            ordinal: 1,
            metadata: None,
            text_stored: true,
        };
        let mut low = record.clone();
        low.source_id = "a".into();
        let candidate = Candidate {
            text: "naïve café au lait".into(),
            matched: vec![("c2".into(), low.clone()), ("c1".into(), record.clone())],
        };
        let a = QuoteAlignment {
            text: 0,
            source: 6..10,
            edits: Vec::new(),
            coverage: 1.0,
        };
        let w = wire_alignment(&a, &candidate, 3).unwrap();
        assert_eq!(w.span.exact, "café");
        assert_eq!(
            (w.span.prefix.as_str(), w.span.suffix.as_str()),
            ("ve ", " au")
        );
        assert_eq!(
            (
                w.span.corpus_id.as_str(),
                w.span.document.source.id.as_str()
            ),
            ("c1", "b"),
            "the lowest (corpus_id, source.id, ordinal) names the span"
        );
    }
}
