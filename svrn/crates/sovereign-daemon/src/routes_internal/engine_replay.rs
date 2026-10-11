// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/engine/replay`: run one recorded engine call against this
//! daemon's local engine and return what it answered and what the engine did
//! on the way (`sovereign_contracts::engine_observe`).
//!
//! It is the engine conformance battery's way in (`bench/lanes/engine-swap/`,
//! `svrn bench engine-conformance`). The call goes to the same provider the
//! daemon serves from, with an observation sink installed, so what comes back
//! is what the code that runs did, not a copy of it. It runs inference on
//! request, so it is mounted with `/internal/inference/warmup` on the
//! operator listener, never on the peer-reachable internal port.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sovereign_contracts::engine_observe::{observed, Observation};
use sovereign_contracts::error::Error;
use sovereign_contracts::traits::{InferenceProvider, ServingLocus};
use sovereign_contracts::types::{CompletionRequest, Speed, StreamFrame};

use crate::state::AppState;

/// One call to replay, as `[engine] capture` recorded it.
#[derive(Debug, Deserialize)]
pub struct ReplayRequest {
    /// The provider method: a `complete*` method, `embed`, `embed_query`,
    /// `embed_batch`, `rerank_batch`, `count_tokens`, or `host`.
    pub method: String,
    /// Its input: the `CompletionRequest`, or `{text}`, `{texts}`,
    /// `{query, docs}`, or `{}` for `host`.
    #[serde(default)]
    pub input: Value,
}

/// What the call answered, and what was observed while it ran.
#[derive(Debug, Serialize)]
pub struct ReplayResponse {
    /// `ok`, `refused` or `error`, with the message.
    pub outcome: Value,
    /// The method's answer: a completion, stream frames, vectors, scores, a
    /// count or the host's self-report.
    pub answer: Value,
    /// Everything the engine recorded while it ran.
    pub observations: Vec<Observation>,
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
    tracing::debug!(target: "engine_replay", method = %req.method, "replaying an engine call");
    let (result, observations) = observed(run(provider, &req)).await;
    let result = result?;
    let (outcome, answer) = match result {
        Ok(answer) => (json!({"kind": "ok"}), answer),
        Err(e) => {
            let kind = match e {
                Error::InvalidInput(_)
                | Error::NotImplemented(_)
                | Error::ContextExceeded { .. }
                | Error::PermissionDenied(_) => "refused",
                _ => "error",
            };
            tracing::debug!(target: "engine_replay", method = %req.method, kind, error = %e, "the engine did not serve");
            (json!({"kind": kind, "message": e.to_string()}), Value::Null)
        }
    };
    Ok(Json(ReplayResponse {
        outcome,
        answer,
        observations,
    }))
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
    let completion = || -> Result<CompletionRequest, (StatusCode, String)> {
        serde_json::from_value(req.input.clone()).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("input is not a CompletionRequest: {e}"),
            )
        })
    };
    Ok(match req.method.as_str() {
        "complete" | "complete_batch" => {
            let r = completion()?;
            p.complete(&r).await.and_then(|resp| {
                serde_json::to_value(&resp).map_err(|e| Error::Serialization(e.to_string()))
            })
        }
        // Every streaming method replays as the typed stream: it is the
        // one the daemon serves local streams through, and the untyped ones
        // are adaptations of it.
        "complete_stream"
        | "complete_stream_with_id"
        | "complete_stream_with_finish"
        | "complete_stream_with_id_and_finish" => {
            let r = completion()?;
            match p.complete_stream_with_finish(&r).await {
                Ok(stream) => Ok(Value::Array(stream.map(frame_json).collect().await)),
                Err(e) => Err(e),
            }
        }
        "embed" => p.embed(&text("text")?).await.map(|v| json!([v])),
        "embed_query" => p.embed_query(&text("text")?).await.map(|v| json!([v])),
        "embed_batch" => {
            let texts: Vec<String> = serde_json::from_value(req.input["texts"].clone())
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("input.texts: {e}")))?;
            p.embed_batch(&texts).await.map(|v| json!(v))
        }
        "rerank_batch" => {
            let docs: Vec<String> = serde_json::from_value(req.input["docs"].clone())
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("input.docs: {e}")))?;
            p.rerank_batch(&text("query")?, &docs)
                .await
                .map(|v| json!(v))
        }
        "count_tokens" => Ok(json!(p.count_tokens(&text("text")?))),
        "host" => Ok(host_report(p).await),
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("method `{other}` is not one this route replays"),
            ))
        }
    })
}

fn frame_json(frame: StreamFrame) -> Value {
    match frame {
        StreamFrame::Token(text) => json!({"kind": "token", "text": text}),
        StreamFrame::Finish { reason, usage } => json!({
            "kind": "finish",
            "reason": reason.as_openai_str(),
            "usage": usage.map(|u| json!({"prompt": u.prompt_tokens, "completion": u.completion_tokens})),
        }),
        StreamFrame::Error(message) => json!({"kind": "error", "message": message}),
    }
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
    use sovereign_contracts::types::{FinishReason, StreamFrame};

    use super::{run, ReplayRequest};

    fn call(method: &str, input: serde_json::Value) -> ReplayRequest {
        ReplayRequest {
            method: method.into(),
            input,
        }
    }

    #[tokio::test]
    async fn a_completion_replays_through_the_provider_it_is_given() {
        let p = TestProvider::new().with_complete_text("from the engine");
        let answer = run(
            &p,
            &call(
                "complete",
                json!({"prompt": "q", "preferred_speed": "Slow"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(answer["text"], "from the engine");
    }

    #[tokio::test]
    async fn a_stream_replays_as_its_frames_ending_in_finish() {
        let p = TestProvider::new().with_typed_frames(vec![
            StreamFrame::Token("hi".into()),
            StreamFrame::Finish {
                reason: FinishReason::Length,
                usage: None,
            },
        ]);
        let frames = run(
            &p,
            &call(
                "complete_stream_with_id_and_finish",
                json!({"prompt": "q", "preferred_speed": "Fast"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(frames[0], json!({"kind": "token", "text": "hi"}));
        assert_eq!(frames[1]["kind"], "finish");
        assert_eq!(frames[1]["reason"], "length");
    }

    #[tokio::test]
    async fn a_request_the_route_cannot_read_is_refused_by_the_route() {
        let p = TestProvider::new();
        assert_eq!(run(&p, &call("probe", json!({}))).await.unwrap_err().0, 400);
        assert_eq!(
            run(&p, &call("complete", json!({"no": "prompt"})))
                .await
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(run(&p, &call("embed", json!({}))).await.unwrap_err().0, 400);
    }
}
