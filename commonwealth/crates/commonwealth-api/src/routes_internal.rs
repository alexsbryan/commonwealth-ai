use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_inference::inference_plan::InferencePlan;
use commonwealth_inference::oicp::KnowledgeSearchRequest;

use crate::state::AppState;

/// POST /internal/gossip — member state exchange.
pub async fn gossip(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Gossip exchange will be fully wired in when the gossip transport
    // is integrated. For now, accept and acknowledge.
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "accepted" })),
    )
}

/// POST /internal/scheduling/intent — scheduling lock acquisition.
pub async fn scheduling_intent(
    State(_state): State<AppState>,
    Json(_payload): Json<SchedulingIntent>,
) -> (StatusCode, Json<SchedulingIntentResponse>) {
    (
        StatusCode::OK,
        Json(SchedulingIntentResponse {
            granted: true,
            leader: String::new(),
        }),
    )
}

/// POST /internal/scheduling/plan — shard plan broadcast.
///
/// Peer nodes call this when they compute a new inference plan.
/// The plan is stored in MeshStore and propagated via gossip.
pub async fn scheduling_plan(
    State(state): State<AppState>,
    Json(plan): Json<InferencePlan>,
) -> StatusCode {
    state.inner.inference_store.set_plan(&plan);
    StatusCode::OK
}

/// POST /internal/model/transfer — peer-to-peer model file transfer.
pub async fn model_transfer(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// POST /internal/index/transfer — peer-to-peer corpus index transfer.
pub async fn index_transfer(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// POST /internal/knowledge/search — inter-node shard query (fan-out target).
///
/// Peer nodes call this to search corpus shards hosted on this node.
pub async fn knowledge_search(
    State(state): State<AppState>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "no corpus engine on this node" })),
            );
        }
    };

    // Fan-out: search local corpus index for each requested corpus.
    let corpora = request.corpora.as_deref().unwrap_or(&[]);
    let limit = request.effective_limit() as usize;
    let mut all_results = Vec::new();

    let search_corpora: Vec<String> = if corpora.is_empty() {
        engine
            .installed_indexes()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.corpus_id)
            .collect()
    } else {
        corpora.to_vec()
    };

    for corpus_id in &search_corpora {
        if let Ok(index) = engine.open_index_for_corpus(corpus_id).await {
            if let Ok(results) = index
                .search(&request.query_embedding, &request.query_text, limit)
                .await
            {
                all_results.extend(results.into_iter().map(|r| {
                    serde_json::json!({
                        "content": r.content,
                        "title": r.title,
                        "corpus_id": corpus_id,
                        "url": r.url,
                        "score": r.score,
                    })
                }));
            }
        }
    }

    all_results.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "results": all_results,
            "corpora_searched": search_corpora,
        })),
    )
}

/// GET /internal/latency/probe — RTT measurement endpoint.
pub async fn latency_probe() -> StatusCode {
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct SchedulingIntent {
    pub node_id: String,
    pub intent: String,
}

#[derive(Debug, Serialize)]
pub struct SchedulingIntentResponse {
    pub granted: bool,
    pub leader: String,
}
