use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

use commonwealth_core::oicp::{
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse,
};

use crate::state::AppState;

/// POST /v1/knowledge/search — search the mesh's knowledge index (OICP §6).
///
/// The requesting node fans out to all nodes holding relevant corpus shards,
/// merges results, deduplicates, reranks by score, and returns global top-K.
pub async fn knowledge_search(
    State(state): State<AppState>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> impl IntoResponse {
    let knowledge_plan = state.inner.knowledge_plan.read().await;
    let limit = request.effective_limit() as usize;

    if knowledge_plan.assignments.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(KnowledgeSearchResponse::default()).unwrap(),
            ),
        );
    }

    // Determine which corpora to search. An omitted `corpora` field means
    // "search all available" per the spec.
    let target_corpora: HashSet<String> = match &request.corpora {
        Some(c) if !c.is_empty() => c.iter().cloned().collect(),
        _ => knowledge_plan
            .assignments
            .iter()
            .map(|a| a.corpus_id.clone())
            .collect(),
    };

    // Find nodes that hold the requested corpora (primary shards only).
    let relevant_assignments: Vec<_> = knowledge_plan
        .assignments
        .iter()
        .filter(|a| target_corpora.contains(&a.corpus_id))
        .filter(|a| !a.is_replica)
        .collect();

    if relevant_assignments.is_empty() {
        return (
            StatusCode::OK,
            Json(
                serde_json::to_value(KnowledgeSearchResponse {
                    corpora_searched: Vec::new(),
                    corpora_unavailable: target_corpora.into_iter().collect(),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
    }

    // TODO: When AppState.corpus_engine is Some, use corpus_engine::CorpusIndex::search()
    // to query local shards, then fan out to remote shard nodes via
    // POST /internal/knowledge/search and merge the results.
    // For now, return a stub response indicating which corpora would be searched.
    let mut results = Vec::new();
    for assignment in &relevant_assignments {
        let mut metadata = HashMap::new();
        metadata.insert("shard_node".into(), format!("{}", assignment.node_id));
        metadata.insert("is_stub".into(), "true".into());

        results.push(KnowledgeResult {
            content: format!(
                "Stub result from corpus '{}' (shard on node {})",
                assignment.corpus_id, assignment.node_id
            ),
            title: Some(format!("Result from {}", assignment.corpus_id)),
            corpus_id: assignment.corpus_id.clone(),
            url: None,
            score: 0.5,
            metadata,
        });
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);

    let corpora_searched: Vec<String> = relevant_assignments
        .iter()
        .map(|a| a.corpus_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let response = KnowledgeSearchResponse {
        results,
        corpora_searched,
        corpora_unavailable: Vec::new(),
        total_chunks_searched: None,
    };

    (StatusCode::OK, Json(serde_json::to_value(response).unwrap()))
}
