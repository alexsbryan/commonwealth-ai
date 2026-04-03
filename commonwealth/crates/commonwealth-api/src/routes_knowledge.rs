use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

use crate::knowledge_types::*;
use crate::state::AppState;

/// POST /v1/knowledge/search — search the mesh's knowledge index.
///
/// The requesting node fans out to all nodes holding relevant corpus shards,
/// merges results, deduplicates, reranks by score, and returns global top-K.
pub async fn knowledge_search(
    State(state): State<AppState>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> impl IntoResponse {
    let knowledge_plan = state.inner.knowledge_plan.read().await;

    if knowledge_plan.assignments.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::to_value(KnowledgeSearchResponse { results: vec![] }).unwrap()),
        );
    }

    // Determine which corpora to search.
    let target_corpora: Vec<String> = if request.corpora.is_empty() {
        // Search all available corpora.
        knowledge_plan
            .assignments
            .iter()
            .map(|a| a.corpus_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        request.corpora.clone()
    };

    // Find nodes that hold the requested corpora.
    let relevant_assignments: Vec<_> = knowledge_plan
        .assignments
        .iter()
        .filter(|a| target_corpora.contains(&a.corpus_id))
        .filter(|a| !a.is_replica) // Prefer primary shards.
        .collect();

    if relevant_assignments.is_empty() {
        return (
            StatusCode::OK,
            Json(serde_json::to_value(KnowledgeSearchResponse { results: vec![] }).unwrap()),
        );
    }

    // In a full implementation, we would fan out queries to each shard node
    // via POST /internal/knowledge/search and merge the results.
    // For now, return a stub response indicating which corpora would be searched.
    let mut results = Vec::new();
    for assignment in &relevant_assignments {
        results.push(KnowledgeResult {
            content: format!(
                "Stub result from corpus '{}' (shard on node {})",
                assignment.corpus_id, assignment.node_id
            ),
            title: format!("Result from {}", assignment.corpus_id),
            corpus_id: assignment.corpus_id.clone(),
            score: 0.5,
            url: None,
            metadata: serde_json::json!({
                "shard_node": format!("{}", assignment.node_id),
                "is_stub": true,
            }),
        });
    }

    // Sort by score descending and limit.
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    results.truncate(request.limit as usize);

    (
        StatusCode::OK,
        Json(serde_json::to_value(KnowledgeSearchResponse { results }).unwrap()),
    )
}
