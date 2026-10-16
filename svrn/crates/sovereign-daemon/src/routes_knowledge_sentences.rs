// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/knowledge/search` at `granularity: sentence` (OICP v0.6 §2.11): the
//! query's vector scored against every indexed sentence of the scope
//! (`sentence_index`), the hits sentences best first, each read from its
//! stored address, so this route never locates a passage in its text
//! (`span_of`, the passage route's largest stage under PV1).
//!
//! The scorer is the data file's, from the closed set: `cosine` over the
//! stored vectors, or `rerank`, cosine's best reordered by the rerank slot.
//! One the host cannot run is refused `503 scorer unavailable` by name, never
//! answered by the other. A scope with sentences still pending is answered by
//! the passages route: the passage search's top hits, their sentences ranked
//! by the same scorer, and the reply says so with the pending count.
//!
//! Locality: sentences are answered from this host's corpora only.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use corpus_index::index::{CorpusIndex, StoredText};
use kernel_types::Sha256Hash;
use oicp_types::evidence::{Span, DEFAULT_CONTEXT};
use oicp_types::knowledge::reasons::SCORER_UNAVAILABLE;
use oicp_types::{
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse, Scorer, SentenceRoute,
    SentenceScorer, Truncated,
};
use sovereign_contracts::principal::Principal;

use crate::oicp_evidence::{readable_corpora, record_for, search_corpus, wire_document};
use crate::search_stage::SearchStage;
use crate::sentence_index::scan::{capped, dot, top_k, Scored};
use crate::sentence_index::store::{same_embedder, unit, PROBE_TEXT};
use crate::sentence_index::{read, Entry, SentenceSearchConfig, TARGET};
use crate::state::AppState;

/// Why a sentence search was not answered.
pub(crate) enum Refused {
    /// §2.11's refusal by name: `503 {"error": "scorer unavailable",
    /// "scorer": <name>}`.
    Scorer { scorer: SentenceScorer, why: String },
    /// No query text and no vector: `400` with an empty reply.
    EmptyQuery,
}

impl Refused {
    fn into_response(self) -> Response {
        match self {
            Refused::Scorer { scorer, why } => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": SCORER_UNAVAILABLE, "scorer": scorer.as_str(), "detail": why})),
            )
                .into_response(),
            Refused::EmptyQuery => (
                StatusCode::BAD_REQUEST,
                Json(KnowledgeSearchResponse::default()),
            )
                .into_response(),
        }
    }

    /// The refusal as one line, for a caller that answers it by name
    /// (verify's `could_not_judge`).
    pub(crate) fn reason(&self) -> String {
        match self {
            Refused::Scorer { scorer, why } => {
                format!("{SCORER_UNAVAILABLE} ({}): {why}", scorer.as_str())
            }
            Refused::EmptyQuery => "empty query".into(),
        }
    }
}

fn refuse(scorer: SentenceScorer, why: &str) -> Refused {
    tracing::warn!(target: TARGET, scorer = scorer.as_str(), %why, "sentence search: the scorer cannot run; refused by name");
    Refused::Scorer {
        scorer,
        why: why.to_string(),
    }
}

/// One scoped corpus, ready to scan or not.
struct Target {
    corpus: String,
    ix: CorpusIndex,
    entry: Arc<Entry>,
    rows: Vec<std::ops::Range<usize>>,
    in_scope: usize,
    pending: usize,
    /// The vectors are this embedder's.
    current: bool,
}

/// A sentence the reply may carry: its corpus (index into the targets), text,
/// address and score.
#[derive(Clone, Copy)]
struct Candidate {
    target: usize,
    text: Sha256Hash,
    row: usize,
    score: f32,
}

pub(crate) async fn search(
    state: AppState,
    principal: Option<&Principal>,
    request: KnowledgeSearchRequest,
    scope: Option<Vec<Sha256Hash>>,
    cfg: SentenceSearchConfig,
) -> Response {
    match sentences(
        state,
        principal,
        request,
        scope,
        cfg,
        DEFAULT_CONTEXT as usize,
    )
    .await
    {
        Ok(reply) => Json(reply).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The sentences for `request`, best first, each span carrying `context`
/// code points either side, or why there are none: the one retrieval path
/// sentence search and verify by meaning (§2.15) share.
pub(crate) async fn sentences(
    state: AppState,
    principal: Option<&Principal>,
    request: KnowledgeSearchRequest,
    scope: Option<Vec<Sha256Hash>>,
    cfg: SentenceSearchConfig,
    context: usize,
) -> Result<KnowledgeSearchResponse, Refused> {
    let admitted_at = Instant::now();
    let limit = request.effective_limit() as usize;
    let scorer = cfg.scorer;
    let Some(local) = state.inner.serving.local_inference.clone() else {
        return Err(refuse(scorer, "no local inference on this host"));
    };
    // `rerank` runs on the rerank slot, and names its model from it.
    let rerank_model = local
        .resident_slots()
        .into_iter()
        .find(|s| s.role == "rerank")
        .map(|s| s.model_id);
    if scorer == SentenceScorer::Rerank && rerank_model.is_none() {
        return Err(refuse(scorer, "this host holds no rerank slot"));
    }
    // The query prefix and the embedder's name, as the passage route reads
    // them (the manifest's source); a host that names none is not guessed at.
    let (prefix, embed_model_id) = match state.local_embed_model().await {
        Ok(Some(m)) => (m.query_instruction_prefix, m.model_id),
        Ok(None) => (String::new(), local.embed_model_id()),
        Err(e) => return Err(refuse(scorer, &format!("the embed model is unread: {e}"))),
    };
    if embed_model_id.trim().is_empty() || embed_model_id == "unknown" {
        return Err(refuse(scorer, "this host's embedder names no model"));
    }

    // The query's vector and the embedder's identity, in one call.
    let embed_at = Instant::now();
    let caller_vector = !request.query_embedding.is_empty();
    let mut inputs = Vec::new();
    if !caller_vector {
        if request.query_text.trim().is_empty() {
            return Err(Refused::EmptyQuery);
        }
        inputs.push(format!("{prefix}{}", request.query_text));
    }
    inputs.push(PROBE_TEXT.to_string());
    let mut vectors = match local.embed_batch(&inputs).await {
        Ok(v) if v.len() == inputs.len() => v,
        Ok(v) => {
            return Err(refuse(
                scorer,
                &format!("{} vectors for {} inputs", v.len(), inputs.len()),
            ))
        }
        Err(e) => return Err(refuse(scorer, &format!("the embed slot refused: {e}"))),
    };
    // `vectors` holds one per input (checked above), the probe's last.
    let Some(probe) = vectors.pop().map(unit) else {
        return Err(refuse(scorer, "the embed slot answered no probe"));
    };
    let query = unit(
        vectors
            .pop()
            .unwrap_or_else(|| request.query_embedding.clone()),
    );
    let embedding = embed_at.elapsed();
    SearchStage::QueryEmbed.emit(embedding, None, usize::from(!caller_vector));

    let (targets, unavailable, unhosted) =
        match admit(&state, principal, &request, scope.as_deref(), &cfg, &probe).await {
            Ok(t) => t,
            Err(refused) => return Err(refused),
        };
    SearchStage::Admission.emit(
        admitted_at.elapsed().saturating_sub(embedding),
        None,
        targets.len(),
    );

    let pending: usize = targets.iter().map(|t| t.pending).sum();
    let in_scope: usize = targets.iter().map(|t| t.in_scope).sum();
    let scan_at = Instant::now();
    let want = match scorer {
        SentenceScorer::Cosine => limit,
        SentenceScorer::Rerank => limit.max(cfg.rerank_candidates),
    };
    let (route, mut found, truncated, scored) = if pending == 0 {
        let (found, scanned) = match scan_index(&targets, &query, want, cfg.scan_ceiling).await {
            Ok(f) => f,
            Err(why) => return Err(refuse(scorer, &why)),
        };
        let truncated = (scanned < in_scope).then_some(Truncated {
            scanned: scanned as u64,
            in_scope: in_scope as u64,
        });
        (SentenceRoute::Index, found, truncated, scanned)
    } else {
        let found =
            match passage_candidates(&local, &targets, &request, &query, scope.as_deref(), &cfg)
                .await
            {
                Ok(f) => f,
                Err(why) => return Err(refuse(scorer, &why)),
            };
        let n = found.len();
        (SentenceRoute::Passages, found, None, n)
    };
    SearchStage::SentenceScan.emit(scan_at.elapsed(), None, scored);
    tracing::debug!(target: TARGET, route = ?route, in_scope, pending, scored, candidates = found.len(), truncated = truncated.is_some(), "sentence search: scored");

    let read_at = Instant::now();
    let mut texts: HashMap<(usize, Sha256Hash), Result<StoredText, String>> = HashMap::new();
    for c in &found {
        if let std::collections::hash_map::Entry::Vacant(v) = texts.entry((c.target, c.text)) {
            let read = match targets[c.target].ix.text(&c.text).await {
                Ok(Ok(t)) => Ok(t),
                Ok(Err(absence)) => Err(format!("{absence:?}")),
                Err(e) => Err(e.to_string()),
            };
            v.insert(read);
        }
    }
    SearchStage::TextRead.emit(read_at.elapsed(), None, texts.len());

    let model = match scorer {
        SentenceScorer::Cosine => embed_model_id.clone(),
        SentenceScorer::Rerank => {
            // A candidate whose text does not read is dropped before the
            // reranker sees it, never scored as an empty sentence.
            let before = found.len();
            let mut docs: Vec<String> = Vec::with_capacity(found.len());
            found.retain(|c| match sentence(&targets, &texts, c) {
                Ok(s) => {
                    docs.push(s);
                    true
                }
                Err(_) => false,
            });
            if found.len() < before {
                tracing::warn!(target: TARGET, dropped = before - found.len(), "sentence search: candidates whose text does not read, dropped before the rerank");
            }
            if !docs.is_empty() {
                match local.rerank_batch(&request.query_text, &docs).await {
                    Ok(scores) if scores.len() == found.len() => {
                        found.iter_mut().zip(scores).for_each(|(c, s)| c.score = s);
                    }
                    Ok(s) => {
                        return Err(refuse(
                            scorer,
                            &format!("{} scores for {} sentences", s.len(), docs.len()),
                        ))
                    }
                    Err(e) => return Err(refuse(scorer, &format!("the rerank slot refused: {e}"))),
                }
            }
            match rerank_model {
                Some(m) => m,
                None => return Err(refuse(scorer, "this host holds no rerank slot")),
            }
        }
    };
    found.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then((a.target, a.row).cmp(&(b.target, b.row)))
    });
    found.truncate(limit);

    let mut results = Vec::with_capacity(found.len());
    for c in &found {
        match hit(&targets, &texts, c, context) {
            Ok(h) => results.push(h),
            Err(why) => {
                tracing::warn!(target: TARGET, corpus = %targets[c.target].corpus, text = %c.text, row = c.row, %why, "sentence search: a hit's text does not read; dropped")
            }
        }
    }
    // v0.6 §2.16: each hit's document carries the record progress lists.
    for t in &targets {
        crate::records_by_identifier::attach_to_hits(&t.ix.path(), &t.corpus, &mut results);
    }
    // v0.6 §2.11 `knowledge:sentence_queue`: where each waiting scoped corpus
    // stands in the worker's line, read from the worker's own queue.
    let sentence_queue = (route == SentenceRoute::Passages).then(|| {
        let sentences = &state.inner.node.sentences;
        let waiting: Vec<(String, u64)> = targets
            .iter()
            .filter(|t| t.pending > 0)
            .map(|t| {
                (
                    t.corpus.clone(),
                    read(&t.entry.data).pending_under(&probe) as u64,
                )
            })
            .collect();
        let queue = sentences.queue();
        let places = crate::sentence_index::queue::places(&queue, &waiting, |c| {
            sentences.pending_of(c, &probe).map(|n| n as u64)
        });
        tracing::debug!(target: TARGET, embedding = ?queue.embedding, waiting = queue.waiting.len(), placed = places.len(), "sentence search: queue placed");
        places
    });
    tracing::debug!(target: TARGET, route = ?route, hits = results.len(), scorer = scorer.as_str(), %model, "sentence search: answered");
    Ok(KnowledgeSearchResponse {
        results,
        corpora_searched: targets.iter().map(|t| t.corpus.clone()).collect(),
        corpora_unavailable: unavailable,
        corpora_unhosted: unhosted,
        total_chunks_searched: None,
        scorer: Some(Scorer {
            name: scorer,
            model,
        }),
        route: Some(route),
        sentences_pending: (route == SentenceRoute::Passages).then_some(pending as u64),
        sentence_queue,
        truncated,
    })
}

type Admitted = (Vec<Target>, Vec<String>, Vec<String>);

/// The scoped corpora this caller may read here, each prepared, with its
/// pending count against the embedder's identity `probe`. A corpus named and
/// not readable here is unavailable and unhosted (locality: no peer's
/// sentences); one that cannot be sentence-indexed is unavailable.
async fn admit(
    state: &AppState,
    principal: Option<&Principal>,
    request: &KnowledgeSearchRequest,
    scope: Option<&[Sha256Hash]>,
    cfg: &SentenceSearchConfig,
    probe: &[f32],
) -> Result<Admitted, Refused> {
    let Some(engine) = state.inner.node.corpus_engine.as_ref() else {
        tracing::debug!(target: TARGET, "sentence search: no corpus engine; nothing searched");
        return Ok((
            Vec::new(),
            request.corpora.clone().unwrap_or_default(),
            request.corpora.clone().unwrap_or_default(),
        ));
    };
    let installed = match engine.installed_indexes().await {
        Ok(i) => i,
        Err(e) => {
            // Every named corpus then answers unavailable, never "held, empty".
            tracing::warn!(target: TARGET, error = %e, "sentence search: the installed corpora do not list");
            Vec::new()
        }
    };
    let readable: HashSet<String> = readable_corpora(state, principal, &installed)
        .into_iter()
        .collect();
    let (mut names, mut unavailable, mut unhosted) = (Vec::new(), Vec::new(), Vec::new());
    match request.corpora.as_deref() {
        Some(c) if !c.is_empty() => {
            for id in c.iter().collect::<std::collections::BTreeSet<_>>() {
                if readable.contains(id) {
                    names.push(id.clone());
                } else {
                    unavailable.push(id.clone());
                    unhosted.push(id.clone());
                }
            }
        }
        // Unconstrained, the scope is what is reachable: the corpora already
        // sentence-indexed here. A corpus is indexed when a search or an
        // install names it, never by a search that names none (this host
        // can hold thousands, each an embedding run on the embed slot).
        _ => {
            names = readable
                .iter()
                .filter(|id| {
                    state
                        .inner
                        .node
                        .sentences
                        .is_indexed(engine.index_dir(), id)
                })
                .cloned()
                .collect();
            tracing::debug!(target: TARGET, readable = readable.len(), indexed = names.len(), "sentence search: unconstrained; the sentence-indexed corpora searched");
        }
    }
    names.sort();
    let mut targets = Vec::new();
    for corpus in names {
        let chunks = installed
            .iter()
            .find(|i| i.corpus_id == corpus)
            .map_or(0, |i| i.chunk_count);
        if chunks > cfg.index_ceiling_chunks {
            tracing::info!(target: TARGET, %corpus, chunks, ceiling = cfg.index_ceiling_chunks, "sentence search: past the index ceiling; unavailable");
            unavailable.push(corpus);
            continue;
        }
        let ix = match engine.open_index_for_corpus(&corpus).await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(target: TARGET, %corpus, error = %e, "sentence search: the index does not open; unavailable");
                unavailable.push(corpus);
                continue;
            }
        };
        // A `documents` scope skips a corpus holding none of its texts before
        // any of its sentences are cut.
        if let Some(s) = scope {
            if !matches!(ix.documents_for(s).await, Ok(Ok(ref m)) if !m.is_empty()) {
                tracing::debug!(target: TARGET, %corpus, "sentence search: holds none of the scoped texts");
                continue;
            }
        }
        let entry = match state
            .inner
            .node
            .sentences
            .prepare(engine.index_dir(), &corpus, &ix)
            .await
        {
            Ok(e) => e,
            Err(why) => {
                tracing::warn!(target: TARGET, %corpus, %why, "sentence search: cannot be sentence-indexed; unavailable");
                unavailable.push(corpus);
                continue;
            }
        };
        let (rows, in_scope, pending, current) = {
            let d = read(&entry.data);
            let rows = d.scoped(scope);
            let in_scope: usize = rows.iter().map(|r| r.len()).sum();
            let current = same_embedder(&d.probe, probe);
            let pending = if current {
                rows.iter()
                    .flat_map(|r| r.clone())
                    .filter(|&r| !d.embedded[r])
                    .count()
            } else {
                in_scope
            };
            (rows, in_scope, pending, current)
        };
        if pending > 0 {
            state.inner.node.sentences.enqueue(state, &corpus);
        }
        tracing::debug!(target: TARGET, %corpus, in_scope, pending, current, "sentence search: scoped");
        targets.push(Target {
            corpus,
            ix,
            entry,
            rows,
            in_scope,
            pending,
            current,
        });
    }
    Ok((targets, unavailable, unhosted))
}

/// The index route: every scoped row scored, up to the ceiling across the
/// scope in corpus and text order. Returns the best `want` and the rows scanned.
async fn scan_index(
    targets: &[Target],
    query: &[f32],
    want: usize,
    ceiling: usize,
) -> Result<(Vec<Candidate>, usize), String> {
    let mut left = ceiling;
    let mut found = Vec::new();
    let mut scanned = 0usize;
    for (i, t) in targets.iter().enumerate() {
        let rows = capped(&t.rows, left);
        let n: usize = rows.iter().map(|r| r.len()).sum();
        left -= n;
        scanned += n;
        let (entry, q) = (Arc::clone(&t.entry), query.to_vec());
        let scored: Vec<(Scored, Option<Sha256Hash>)> = tokio::task::spawn_blocking(move || {
            let d = read(&entry.data);
            top_k(&d, &rows, &q, want)
                .into_iter()
                .map(|s| (s, d.text_of(s.row)))
                .collect()
        })
        .await
        .map_err(|e| format!("the scan of `{}` panicked: {e}", t.corpus))?;
        found.extend(scored.into_iter().filter_map(|(s, text)| {
            Some(Candidate {
                target: i,
                text: text?,
                row: s.row,
                score: s.score,
            })
        }));
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    found.truncate(want);
    Ok((found, scanned))
}

/// The passages route: the passage search for the same query and scope, and
/// the sentences its top hits touch, each scored against the query by its
/// stored vector where the embedder's own, else embedded now.
async fn passage_candidates(
    local: &Arc<dyn crate::state::LocalInferenceService>,
    targets: &[Target],
    request: &KnowledgeSearchRequest,
    query: &[f32],
    scope: Option<&[Sha256Hash]>,
    cfg: &SentenceSearchConfig,
) -> Result<Vec<Candidate>, String> {
    let mut found: Vec<Candidate> = Vec::new();
    let mut seen = HashSet::new();
    let mut unscored: Vec<(usize, String)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let hits = search_corpus(
            &t.ix,
            &t.corpus,
            query,
            &request.query_text,
            cfg.passage_hits,
            scope,
        )
        .await
        .map_err(|e| format!("the passage search failed: {e}"))?;
        let (mut unranged, mut stored) = (0usize, HashMap::new());
        for h in hits {
            let Some(span) = h.span else {
                unranged += 1;
                continue;
            };
            let Some(text) = Sha256Hash::from_hex(&span.document.text_sha256) else {
                continue;
            };
            let rows = read(&t.entry.data).overlapping(&text, span.start as u32, span.end as u32);
            for row in rows.into_iter().filter(|r| seen.insert((i, *r))) {
                // The stored vector where it is this embedder's, else the
                // address to embed it from.
                let scored = {
                    let d = read(&t.entry.data);
                    if t.current && d.embedded[row] {
                        Ok(dot(query, d.vector(row)))
                    } else {
                        Err(d.rows[row])
                    }
                };
                let r = match scored {
                    Ok(score) => {
                        found.push(Candidate {
                            target: i,
                            text,
                            row,
                            score,
                        });
                        continue;
                    }
                    Err(r) => r,
                };
                if let std::collections::hash_map::Entry::Vacant(v) = stored.entry(text) {
                    let body = match t.ix.text(&text).await {
                        Ok(Ok(s)) => Some(s.text),
                        Ok(Err(absence)) => {
                            tracing::warn!(target: TARGET, corpus = %t.corpus, %text, ?absence, "sentence search: a passage hit's text is not held; its sentences are not scored");
                            None
                        }
                        Err(e) => {
                            tracing::warn!(target: TARGET, corpus = %t.corpus, %text, error = %e, "sentence search: a passage hit's text does not read; its sentences are not scored");
                            None
                        }
                    };
                    v.insert(body);
                }
                if let Some(Some(body)) = stored.get(&text) {
                    unscored.push((
                        found.len(),
                        body[r.byte_start as usize..r.byte_end as usize].to_string(),
                    ));
                    found.push(Candidate {
                        target: i,
                        text,
                        row,
                        score: f32::NEG_INFINITY,
                    });
                }
            }
        }
        if unranged > 0 {
            tracing::debug!(target: TARGET, corpus = %t.corpus, unranged, "sentence search: passage hits with no range give no sentences");
        }
    }
    if !unscored.is_empty() {
        let docs: Vec<String> = unscored.iter().map(|(_, s)| s.clone()).collect();
        let vs = local
            .embed_batch(&docs)
            .await
            .map_err(|e| format!("the embed slot refused: {e}"))?;
        if vs.len() != docs.len() {
            return Err(format!("{} vectors for {} sentences", vs.len(), docs.len()));
        }
        for ((at, _), v) in unscored.iter().zip(vs) {
            found[*at].score = dot(query, &unit(v));
        }
    }
    tracing::debug!(target: TARGET, candidates = found.len(), embedded_now = unscored.len(), "sentence search: passages route scored");
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(found)
}

type Texts = HashMap<(usize, Sha256Hash), Result<StoredText, String>>;

fn sentence(targets: &[Target], texts: &Texts, c: &Candidate) -> Result<String, String> {
    let stored = texts
        .get(&(c.target, c.text))
        .ok_or("text not read")?
        .as_ref()
        .map_err(Clone::clone)?;
    let r = read(&targets[c.target].entry.data).rows[c.row];
    stored
        .text
        .get(r.byte_start as usize..r.byte_end as usize)
        .map(str::to_string)
        .ok_or_else(|| "the address is outside its text".into())
}

/// A candidate as a hit: its sentence, its text's record, its range with
/// `ctx` code points either side.
fn hit(
    targets: &[Target],
    texts: &Texts,
    c: &Candidate,
    ctx: usize,
) -> Result<KnowledgeResult, String> {
    let t = &targets[c.target];
    let stored = texts
        .get(&(c.target, c.text))
        .ok_or("text not read")?
        .as_ref()
        .map_err(Clone::clone)?;
    let r = read(&t.entry.data).rows[c.row];
    let text = stored.text.as_str();
    let (bs, be) = (r.byte_start as usize, r.byte_end as usize);
    let exact = text
        .get(bs..be)
        .ok_or("the address is outside its text")?
        .to_string();
    // `ctx` 0 is no prefix, not the one code point `nth(0)` would give.
    let before = match ctx.checked_sub(1) {
        Some(n) => text[..bs].char_indices().rev().nth(n).map_or(0, |(b, _)| b),
        None => bs,
    };
    let after = text[be..]
        .char_indices()
        .nth(ctx)
        .map_or(text.len(), |(b, _)| be + b);
    let record = record_for(&stored.documents, None).ok_or("the text has no record")?;
    let document = wire_document(record)?;
    let locator = crate::oicp_evidence::locator_at(stored.locators.as_deref(), u64::from(r.start))?;
    Ok(KnowledgeResult {
        content: exact.clone(),
        title: None,
        corpus_id: t.corpus.clone(),
        url: None,
        score: c.score,
        metadata: HashMap::new(),
        chunk_id: None,
        source_doc_id: Some(record.source_id.clone()),
        custody: None,
        grain: None,
        peer_name: None,
        peer_node_id: None,
        span: Some(Span {
            corpus_id: t.corpus.clone(),
            document: document.clone(),
            start: u64::from(r.start),
            end: u64::from(r.end),
            exact,
            prefix: text[before..bs].to_string(),
            suffix: text[be..after].to_string(),
            locator,
        }),
        span_absent: None,
        document: Some(document),
        document_absent: None,
    })
}
