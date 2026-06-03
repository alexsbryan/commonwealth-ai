//! Knowledge fan-out target + latency probe.
//!
//! Both endpoints are inter-node primitives: peers POST a
//! `KnowledgeSearchRequest` and we open whichever shards we host that
//! match the corpus filter, returning a typed `KnowledgeSearchResponse`
//! that the caller merges with replies from other peers. The latency
//! probe is the lowest-rung discovery primitive — peers GET it to
//! measure RTT before deciding which manifest to fetch.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use commonwealth_inference::oicp::{
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse,
};

use crate::state::AppState;

/// POST /internal/knowledge/search — inter-node shard query (fan-out target).
///
/// Peer nodes call this to search corpus shards hosted on this node.
/// Returns the typed `KnowledgeSearchResponse` from `oicp-types`, the
/// same shape `/v1/knowledge/search` returns — so when the client-
/// side handler fans out to multiple peers it can deserialize all of
/// their replies into one container and merge-rank without a custom
/// wire format per peer.
pub async fn knowledge_search(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<KnowledgeSearchRequest>,
) -> (StatusCode, Json<KnowledgeSearchResponse>) {
    // Identify the requester so we can stamp this on emitted ledger
    // events. Local-origin requests (no X-Node-Id) skip emission —
    // the dimensional ledger is intra-mesh-only per the spec scope.
    let requester = crate::headers::parse_x_node_id(&headers);

    let engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            // Peers may have gossiped `hosted_corpora` that's since
            // been removed, or reach us during a brief pre-bootstrap
            // window; 503 + empty body tells them "not me, try
            // someone else" without poisoning their merge.
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(KnowledgeSearchResponse::default()),
            );
        }
    };

    let corpora = request.corpora.as_deref().unwrap_or(&[]);
    let limit = request.effective_limit() as usize;

    // Resolve the target corpora: either the caller's explicit list
    // (which MAY include corpora we don't host — we just skip those)
    // or all locally-installed corpora when the caller sent no
    // filter. Either way, we filter against what `installed_indexes`
    // actually reports so we never try to open an index we don't
    // have.
    let installed: std::collections::HashSet<String> = engine
        .installed_indexes()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.corpus_id)
        .collect();
    let search_corpora: Vec<String> = if corpora.is_empty() {
        installed.iter().cloned().collect()
    } else {
        corpora
            .iter()
            .filter(|c| installed.contains(*c))
            .cloned()
            .collect()
    };
    let corpora_unavailable: Vec<String> = corpora
        .iter()
        .filter(|c| !installed.contains(*c))
        .cloned()
        .collect();

    let mut all_results: Vec<KnowledgeResult> = Vec::new();
    // Per-corpus chunk counts: one ledger event per corpus this
    // request actually returned chunks from. Emitted post-truncation
    // so the count reflects what *the requester sees*, not the raw
    // pre-merge size.
    let mut per_corpus_chunks: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for corpus_id in &search_corpora {
        match engine.open_index_for_corpus(corpus_id).await {
            Ok(index) => {
                match index
                    .search(&request.query_embedding, &request.query_text, limit)
                    .await
                {
                    Ok(results) => {
                        all_results.extend(results.into_iter().map(|r| KnowledgeResult {
                            content: r.content,
                            title: r.title,
                            corpus_id: corpus_id.clone(),
                            url: r.url,
                            score: r.score,
                            metadata: Default::default(),
                            chunk_id: r.chunk_id,
                            source_doc_id: r.source_doc_id,
                        }));
                    }
                    Err(e) => {
                        tracing::warn!(
                            corpus = corpus_id,
                            error = %e,
                            "internal knowledge_search: search failed"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    error = %e,
                    "internal knowledge_search: open_index failed"
                );
            }
        }
    }

    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    // Count returned chunks per corpus AFTER truncation — the
    // ledger should reflect what we actually shipped to the
    // requester, not what we ranked internally.
    for r in &all_results {
        *per_corpus_chunks.entry(r.corpus_id.clone()).or_insert(0) += 1;
    }

    let hit_count = all_results.len();
    tracing::info!(
        corpora = ?search_corpora,
        hits = hit_count,
        "internal knowledge_search: served"
    );

    // Emit one `KnowledgeQueryServed` per corpus that contributed
    // chunks. Local-origin requests (requester==None) skip emission.
    if let Some(for_node) = requester {
        for (corpus_id, chunks) in per_corpus_chunks {
            state.inner.contribution_emitter.record(
                commonwealth_core::contributions::LedgerEventKind::KnowledgeQueryServed {
                    for_node,
                    corpus_id,
                    chunks_returned: chunks,
                },
            );
        }
    }

    (
        StatusCode::OK,
        Json(KnowledgeSearchResponse {
            results: all_results,
            corpora_searched: search_corpora,
            corpora_unavailable,
            total_chunks_searched: None,
        }),
    )
}

/// GET /internal/latency/probe — RTT measurement endpoint.
pub async fn latency_probe() -> StatusCode {
    StatusCode::OK
}
