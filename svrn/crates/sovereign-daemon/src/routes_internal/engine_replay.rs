// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/engine/replay`: ask this daemon's local engine one
//! question and return its answer.
//!
//! The router's e2e test (`sovereign-stock` `llama_server_router_e2e`) asks
//! host truth through it: the provider's report about itself, the rerank
//! model it names, a rerank, and its estimated and exact token counts. The
//! call goes to the same provider the daemon serves from, so what comes back
//! is what the code that runs answers, not a copy of it. A rerank runs
//! inference on request, so it is mounted with `/internal/inference/warmup`
//! on the operator listener, never on the peer-reachable internal port.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sovereign_contracts::error::Error;
use sovereign_contracts::traits::{InferenceProvider, ServingLocus};
use sovereign_contracts::types::Speed;

use crate::state::AppState;

/// The `InferenceProvider` question the route asks, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMethod {
    /// `rerank_batch`.
    RerankBatch,
    /// `rerank_model_id`: the id a rerank request must name at this
    /// provider's far end.
    RerankModelId,
    /// `count_tokens`.
    CountTokens,
    /// `count_tokens_exact`: the server's own count.
    CountTokensExact,
    /// The provider's report about itself, read-only.
    Host,
}

/// One question to ask.
#[derive(Debug, Deserialize)]
pub struct ReplayRequest {
    /// The provider method. A name outside [`ReplayMethod`] is refused when
    /// the body is read.
    pub method: ReplayMethod,
    /// Its input: `{text}`, `{query, docs, model?}`, or `{}`.
    #[serde(default)]
    pub input: Value,
}

/// What the call answered.
#[derive(Debug, Serialize)]
pub struct ReplayResponse {
    /// `ok`, `refused` or `error`, with the message.
    pub outcome: Value,
    /// The method's answer: scores, a model id, a count or the host's
    /// self-report.
    pub answer: Value,
}

/// Run one call. A request the route cannot read is a 400; a call the engine
/// refuses or fails is a 200 whose outcome says so, since that is the result
/// being measured.
pub async fn engine_replay(
    State(state): State<AppState>,
    Json(req): Json<ReplayRequest>,
) -> Result<Json<ReplayResponse>, (StatusCode, String)> {
    let Some(service) = state.inner.serving.local_inference.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no local inference service is bound on this daemon".into(),
        ));
    };
    let provider: &dyn InferenceProvider = &*service;
    tracing::debug!(target: "engine_replay", method = ?req.method, "replaying an engine call");
    let (outcome, answer) = match run(provider, &req).await? {
        Ok(answer) => (json!({"kind": "ok"}), answer),
        Err(e) => {
            let kind = match e {
                Error::InvalidInput(_)
                | Error::NotImplemented(_)
                | Error::ContextExceeded { .. }
                | Error::PermissionDenied(_) => "refused",
                _ => "error",
            };
            tracing::debug!(target: "engine_replay", method = ?req.method, kind, error = %e, "the engine did not serve");
            (json!({"kind": kind, "message": e.to_string()}), Value::Null)
        }
    };
    Ok(Json(ReplayResponse { outcome, answer }))
}

/// The call itself. The outer `Result` is the route's own refusal of a
/// request it cannot read; the inner one is the engine's answer.
async fn run(
    p: &dyn InferenceProvider,
    req: &ReplayRequest,
) -> Result<sovereign_contracts::Result<Value>, (StatusCode, String)> {
    let text = |key: &str| -> Result<String, (StatusCode, String)> {
        req.input
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("input.{key} must be a string"),
                )
            })
    };
    Ok(match req.method {
        ReplayMethod::RerankBatch => {
            let docs: Vec<String> = serde_json::from_value(req.input["docs"].clone())
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("input.docs: {e}")))?;
            // `model` is optional: the reranker the far end serves, when the
            // caller knows it, else the id the provider itself answers.
            let model = req.input["model"].as_str();
            p.rerank_batch(model, &text("query")?, &docs)
                .await
                .map(|v| json!(v))
        }
        ReplayMethod::RerankModelId => Ok(json!(p.rerank_model_id())),
        ReplayMethod::CountTokens => Ok(json!(p.count_tokens(&text("text")?))),
        ReplayMethod::CountTokensExact => {
            p.count_tokens_exact(&text("text")?).await.map(|n| json!(n))
        }
        ReplayMethod::Host => Ok(host_report(p).await),
    })
}

/// What the provider reports about itself, under the keys the inventory's
/// host rows name. Read-only: nothing here loads or unloads a model.
async fn host_report(p: &dyn InferenceProvider) -> Value {
    let locus = match p.serving_locus() {
        ServingLocus::OwnWeights => "own-weights",
        ServingLocus::ForwardsOnBox => "forwards-on-box",
        ServingLocus::ForwardsOffBox => "forwards-off-box",
        ServingLocus::ForwardsToThirdParty => "forwards-to-third-party",
    };
    json!({
        "model_for_fast": p.model_id_for(Speed::Fast),
        "model_for_slow": p.model_id_for(Speed::Slow),
        "code_model": p.code_model_id(),
        "embed_model_id": p.embed_model_id(),
        "serving_locus": locus,
        "effective_context_size": p.effective_context_size(),
        "n_ctx_train": p.n_ctx_train_for_primary(),
        "resident_models": p
            .resident_slots()
            .into_iter()
            .filter(|s| s.resident)
            .map(|s| s.model_id)
            .collect::<Vec<_>>(),
        "resident_bytes": p.resident_slots().iter().filter_map(|s| s.size_bytes).sum::<u64>(),
        "extras": p.extras_inventory(),
        "compute_children": p.compute_children().len(),
        "peer_manifests": p.peer_manifests().await.len(),
        "lender_manifest": p.lender_manifest().await.map(|(id, _)| id),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sovereign_contracts::double::TestProvider;

    use super::{run, ReplayRequest};

    fn call(method: &str, input: serde_json::Value) -> ReplayRequest {
        serde_json::from_value(json!({"method": method, "input": input})).unwrap()
    }

    #[tokio::test]
    async fn the_host_report_is_the_providers_own() {
        let p = TestProvider::new().with_model_id("m");
        let answer = run(&p, &call("host", json!({}))).await.unwrap().unwrap();
        assert_eq!(answer["model_for_slow"], "m");
        assert_eq!(answer["model_for_fast"], "m");
    }

    #[tokio::test]
    async fn a_request_the_route_cannot_read_is_refused_by_the_route() {
        let p = TestProvider::new();
        for gone in [
            "complete",
            "probe",
            "stream_fault",
            "host_lifecycle",
            "logits",
        ] {
            assert!(
                serde_json::from_value::<ReplayRequest>(json!({"method": gone, "input": {}}))
                    .is_err(),
                "`{gone}` is outside ReplayMethod and is refused when the body is read"
            );
        }
        assert_eq!(
            run(&p, &call("count_tokens", json!({})))
                .await
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(
            run(&p, &call("rerank_batch", json!({"query": "q"})))
                .await
                .unwrap_err()
                .0,
            400
        );
    }
}
