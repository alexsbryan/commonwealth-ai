// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// Hard ceiling on how many corpora a single knowledge search opens, on either
/// the explicit-filter or the unsealed (no-filter) path. Bounds the
/// many-corpora amplification regardless of the caller's argument.
const MAX_FANOUT_CORPORA: usize = 16;

/// On the *unsealed* path (caller sent no `corpora` filter), refuse to open any
/// single corpus larger than this many chunks. Opening a giant corpus on an
/// unscoped "search everything" is the documented OOM vector (a 1.88M-row
/// wikipedia took the daemon down twice). An EXPLICIT request for a large
/// corpus bypasses this — the caller scoped the search on purpose. Conservative:
/// a properly-indexed corpus this size searches fine, but the ceiling protects
/// mid-ingest / unindexed giants from a flat scan on an unscoped query.
const MAX_UNSEALED_CORPUS_CHUNKS: u64 = 100_000;

/// Result of bounding the fan-out target set ([`select_fanout_corpora`]).
struct FanoutSelection {
    /// Corpora to actually open + search (already count-capped + size-filtered).
    searched: Vec<String>,
    /// Corpora skipped on the unsealed path for exceeding the per-corpus chunk
    /// ceiling — surfaced in the glassbox log so an operator can see why a
    /// broad search didn't include the big corpus.
    skipped_oversize: Vec<String>,
    /// Total chunks across the searched corpora (the scan scope).
    total_chunks: u64,
    /// True if the corpora-count cap truncated the set.
    capped: bool,
}

/// Choose the bounded set of corpora to search. `installed` is
/// `(corpus_id, chunk_count)` for every locally-hosted index; `filter` is the
/// caller's requested corpora (empty = unsealed "search everything").
///
/// The bound is **asymmetric**: an explicit filter is honored (the caller took
/// responsibility for scope, subject only to the hard count cap); an absent
/// filter is bounded aggressively — count-capped AND size-filtered so a single
/// giant corpus can't be opened on an unscoped query. Pure + deterministic
/// (sorted by corpus_id) so the cap is reproducible and unit-testable without a
/// corpus engine. This is the server-side structural bound that a missing
/// client-side `corpora` argument cannot bypass (defence behind the client seal).
fn select_fanout_corpora(installed: &[(String, u64)], filter: &[String]) -> FanoutSelection {
    let explicit = !filter.is_empty();
    let filter_set: std::collections::HashSet<&str> =
        filter.iter().map(|s| s.as_str()).collect();

    let mut candidates: Vec<(String, u64)> = installed
        .iter()
        .filter(|(cid, _)| !explicit || filter_set.contains(cid.as_str()))
        .cloned()
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic ordering for the cap

    let mut searched = Vec::new();
    let mut skipped_oversize = Vec::new();
    let mut total_chunks = 0u64;
    let mut capped = false;
    for (cid, chunks) in candidates {
        if searched.len() >= MAX_FANOUT_CORPORA {
            capped = true;
            break;
        }
        if !explicit && chunks > MAX_UNSEALED_CORPUS_CHUNKS {
            skipped_oversize.push(cid);
            continue;
        }
        total_chunks += chunks;
        searched.push(cid);
    }
    FanoutSelection {
        searched,
        skipped_oversize,
        total_chunks,
        capped,
    }
}

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

    // Resolve the target corpora and BOUND the fan-out structurally. An absent
    // `corpora` filter (broad research) previously meant "search every
    // installed index" — which OOM-killed the daemon when a 1.88M-row corpus
    // was hosted. `select_fanout_corpora` caps the fan-out by corpus count and,
    // on the unsealed path, refuses to open any single oversized corpus. The
    // bound lives here, server-side, so a missing client-side `corpora`
    // argument cannot bypass it (defence in depth behind the client-side seal).
    // We still filter against what `installed_indexes` reports so we never try
    // to open an index we don't have.
    let installed: Vec<(String, u64)> = engine
        .installed_indexes()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| (i.corpus_id, i.chunk_count))
        .collect();
    let installed_ids: std::collections::HashSet<String> =
        installed.iter().map(|(c, _)| c.clone()).collect();
    let corpora_unavailable: Vec<String> = corpora
        .iter()
        .filter(|c| !installed_ids.contains(*c))
        .cloned()
        .collect();
    let FanoutSelection {
        searched: search_corpora,
        skipped_oversize,
        total_chunks,
        capped,
    } = select_fanout_corpora(&installed, corpora);

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
        opened = ?search_corpora,
        skipped_oversize = ?skipped_oversize,
        capped,
        total_chunks_in_scope = total_chunks,
        hits = hit_count,
        "internal knowledge_search: served (fan-out bounded)"
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
            total_chunks_searched: Some(total_chunks),
        }),
    )
}

/// GET /internal/latency/probe — RTT measurement endpoint.
pub async fn latency_probe() -> StatusCode {
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> Vec<(String, u64)> {
        vec![
            ("personal".into(), 300),
            ("sep".into(), 50_000),
            ("wikipedia".into(), 1_880_000),
        ]
    }

    #[test]
    fn unsealed_search_skips_the_giant_corpus() {
        // No filter => "search everything", but wikipedia (1.88M chunks) must
        // NOT be opened — that is the documented OOM vector. The small/medium
        // corpora ARE searched, and the scan scope excludes the giant.
        let sel = select_fanout_corpora(&installed(), &[]);
        assert!(sel.searched.contains(&"personal".to_string()));
        assert!(sel.searched.contains(&"sep".to_string()));
        assert!(!sel.searched.contains(&"wikipedia".to_string()));
        assert_eq!(sel.skipped_oversize, vec!["wikipedia".to_string()]);
        assert_eq!(sel.total_chunks, 50_300);
    }

    #[test]
    fn explicit_request_for_a_giant_is_honored() {
        // The caller scoped the search to wikipedia deliberately — honor it,
        // even though it exceeds the unsealed per-corpus ceiling.
        let sel = select_fanout_corpora(&installed(), &["wikipedia".to_string()]);
        assert_eq!(sel.searched, vec!["wikipedia".to_string()]);
        assert!(sel.skipped_oversize.is_empty());
    }

    #[test]
    fn corpora_count_is_hard_capped() {
        let many: Vec<(String, u64)> = (0..40).map(|i| (format!("c{i:02}"), 10)).collect();
        let sel = select_fanout_corpora(&many, &[]);
        assert_eq!(sel.searched.len(), MAX_FANOUT_CORPORA);
        assert!(sel.capped);
    }

    #[test]
    fn unavailable_corpus_is_not_searched() {
        let sel = select_fanout_corpora(&installed(), &["does-not-exist".to_string()]);
        assert!(sel.searched.is_empty());
        assert!(!sel.capped);
    }
}
