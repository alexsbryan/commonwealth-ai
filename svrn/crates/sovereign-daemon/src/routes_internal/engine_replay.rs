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
use sovereign_contracts::engine_observe::{
    observed, HostLifecycleAnswer, LifecycleCall, Observation, ProbeRequest, ReplayMethod,
};
use sovereign_contracts::error::Error;
use sovereign_contracts::traits::{InferenceProvider, ServingLocus};
use sovereign_contracts::types::{CompletionRequest, Speed, StreamFrame};

use crate::state::AppState;

/// One call to replay, as `[engine] capture` recorded it.
#[derive(Debug, Deserialize)]
pub struct ReplayRequest {
    /// The provider method. A name outside [`ReplayMethod`] is refused when
    /// the body is read.
    pub method: ReplayMethod,
    /// Its input: the `CompletionRequest`, or `{text}`, `{texts}`,
    /// `{query, docs}`, a `ProbeRequest`, or `{}` for `host`.
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
    tracing::debug!(target: "engine_replay", method = ?req.method, "replaying an engine call");
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
            tracing::debug!(target: "engine_replay", method = ?req.method, kind, error = %e, "the engine did not serve");
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
        // `admission` is `#[serde(skip)]` on the request — a field a wire hop
        // must not be able to assert — so the tail of this comment carries it
        // through a `turn_admission` key written where the client's own
        // `write_turn_admission` puts it, for `cost.admission`'s scenario
        // (L I3). The battery names a turn it minted; nothing else changes.
        let admission = req.input.get("turn_admission").and_then(Value::as_str);
        let input = {
            let mut v = req.input.clone();
            if let Value::Object(m) = &mut v {
                m.remove("turn_admission");
            }
            v
        };
        let mut request: CompletionRequest = serde_json::from_value(input).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("input is not a CompletionRequest: {e}"),
            )
        })?;
        if let Some(id) = admission {
            request.admission = Some(sovereign_contracts::types::TurnAdmission::new(id));
        }
        Ok(request)
    };
    Ok(match req.method {
        ReplayMethod::Complete | ReplayMethod::CompleteBatch => {
            let r = completion()?;
            p.complete(&r).await.and_then(|resp| {
                serde_json::to_value(&resp).map_err(|e| Error::Serialization(e.to_string()))
            })
        }
        // Every streaming method replays as the typed stream: it is the
        // one the daemon serves local streams through, and the untyped ones
        // are adaptations of it.
        ReplayMethod::CompleteStream
        | ReplayMethod::CompleteStreamWithId
        | ReplayMethod::CompleteStreamWithFinish
        | ReplayMethod::CompleteStreamWithIdAndFinish => {
            let r = completion()?;
            match p.complete_stream_with_finish(&r).await {
                Ok(stream) => Ok(Value::Array(stream.map(frame_json).collect().await)),
                Err(e) => Err(e),
            }
        }
        ReplayMethod::Embed => p.embed(&text("text")?).await.map(|v| json!([v])),
        ReplayMethod::EmbedQuery => p.embed_query(&text("text")?).await.map(|v| json!([v])),
        ReplayMethod::EmbedBatch => {
            let texts: Vec<String> = serde_json::from_value(req.input["texts"].clone())
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("input.texts: {e}")))?;
            p.embed_batch(&texts).await.map(|v| json!(v))
        }
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
        ReplayMethod::HostLifecycle => Ok(with_lifecycle(
            host_report(p).await,
            lifecycle(p, req).await,
        )),
        ReplayMethod::StreamFault => Ok(stream_fault(p, req).await),
        ReplayMethod::Probe => {
            let probe: ProbeRequest = serde_json::from_value(req.input.clone()).map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("input is not a probe request: {e}"),
                )
            })?;
            p.probe_logprobs(&probe).await.and_then(|a| {
                serde_json::to_value(&a).map_err(|e| Error::Serialization(e.to_string()))
            })
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

/// The host report with `host.lifecycle`'s answer beside it, under the three
/// keys the row names: the calls' own endings, and the provider's report
/// after them. One object, because the row compares the call to the state it
/// left — a call that says `Ok` while the model is not resident is the
/// failure the row exists to catch.
fn with_lifecycle(report: Value, lifecycle: HostLifecycleAnswer) -> Value {
    let mut out = report;
    if let Value::Object(map) = &mut out {
        map.insert(
            "warmup_loaded".into(),
            serde_json::to_value(&lifecycle.warmup).unwrap_or(Value::Null),
        );
        // A step the run did not ask is `NotAsked`, never `null`: a null
        // would read as "no answer" and leave the row could-not-judge for the
        // wrong reason (ARCH 6).
        map.insert(
            "extra_loaded".into(),
            serde_json::to_value(
                lifecycle
                    .extra_load
                    .as_ref()
                    .unwrap_or(&LifecycleCall::NotAsked),
            )
            .unwrap_or(Value::Null),
        );
        map.insert(
            "extra_unloaded".into(),
            serde_json::to_value(
                lifecycle
                    .extra_unload
                    .as_ref()
                    .unwrap_or(&LifecycleCall::NotAsked),
            )
            .unwrap_or(Value::Null),
        );
    }
    out
}

/// Call the three lifecycle methods once each and report how they ended, L
/// I4's instrument. The request's `extra` names the slot to load and unload
/// (`{"slot": "...", "path": "...", "context_size": 4096}`); without it the
/// extra half is [`LifecycleCall::NotAsked`], never a silent success.
async fn lifecycle(p: &dyn InferenceProvider, req: &ReplayRequest) -> HostLifecycleAnswer {
    let warmup = match p.warmup_primary().await {
        Ok(()) => LifecycleCall::Ok {
            model: Some(p.model_id_for(Speed::Slow)),
        },
        Err(e) => LifecycleCall::Err {
            message: e.to_string(),
        },
    };
    let mut answer = HostLifecycleAnswer {
        warmup,
        ..Default::default()
    };
    let extra = req.input.get("extra").filter(|e| !e.is_null());
    let Some(extra) = extra else {
        tracing::debug!(target: "engine_replay", "host.lifecycle: no extra slot named, only warmup asked");
        return answer;
    };
    let Some(slot) = extra["slot"].as_str() else {
        // The request asked for an extra and named no slot: an Err for the
        // caller, not a skipped step (ARCH 6 — absence is reported).
        answer.extra_load = Some(LifecycleCall::Err {
            message: "input.extra.slot must be a string".into(),
        });
        return answer;
    };
    let Some(path) = extra["path"].as_str() else {
        answer.extra_load = Some(LifecycleCall::Err {
            message: "input.extra.path must be a string".into(),
        });
        return answer;
    };
    let context_size = extra["context_size"].as_u64().unwrap_or(4096) as u32;
    answer.extra_load = Some(call_outcome(
        p.load_extra_slot(
            slot.to_string(),
            std::path::PathBuf::from(path),
            context_size,
        )
        .map(|id| Some(id)),
    ));
    answer.extra_unload = Some(call_outcome(p.unload_extra_slot(slot)));
    tracing::debug!(
        target: "engine_replay",
        slot,
        path,
        context_size,
        load = ?answer.extra_load,
        unload = ?answer.extra_unload,
        "host.lifecycle: extra slot loaded and unloaded"
    );
    answer
}

/// One lifecycle call's `Result` as the answer reports it.
fn call_outcome(result: sovereign_contracts::Result<Option<String>>) -> LifecycleCall {
    match result {
        Ok(model) => LifecycleCall::Ok { model },
        Err(e) => LifecycleCall::Err {
            message: e.to_string(),
        },
    }
}

/// The typed stream, cut and corrupted on its way back (L I3): every frame
/// as the SSE line it is on the wire, then
/// - `fault = "split-line"`: the text delivered in two byte chunks that cut
///   one SSE line in half, so a reader without a line buffer loses the frame;
/// - `fault = "mid-stream"`: the answer's first tokens, then a `{"error":
///   …}` datum and no `[DONE]`, as llama-server fails a stream
///   (`server-context.cpp:4294`).
/// Anything else is a 400: the driver names the fault it wants, never gets a
/// silent default.
async fn stream_fault(p: &dyn InferenceProvider, req: &ReplayRequest) -> Value {
    let fault = req.input["fault"].as_str().unwrap_or_default();
    if !matches!(fault, "split-line" | "mid-stream") {
        // Name the fault first: the driver asks for one, and an unknown name
        // is answered as such, not as a malformed request.
        return json!({
            "kind": "error",
            "message": format!("`{fault}` is not a fault the stream injects (`split-line`, `mid-stream`)"),
        });
    }
    let Ok(r): Result<CompletionRequest, _> = serde_json::from_value(req.input.clone()) else {
        return json!({"kind": "error", "message": "input is not a CompletionRequest"});
    };
    let frames: Vec<Value> = match p.complete_stream_with_finish(&r).await {
        Ok(stream) => stream.map(frame_json).collect().await,
        Err(e) => return json!({"kind": "error", "message": e.to_string()}),
    };
    let lines: Vec<String> = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
    let (chunks, error) = match fault {
        "split-line" => (split_at_half(&lines), None),
        // A mid-stream failure replaces the tail: llama-server writes the
        // error datum where the rest of the stream would have gone and closes
        // without `[DONE]` (server-context.cpp:4294). So the tokens before it
        // stand, the finish frame is not sent, and the datum is one more SSE
        // line a reader must turn into `Finish{Error}`.
        "mid-stream" => {
            let tokens: Vec<String> = frames
                .iter()
                .filter(|f| f["kind"] == "token")
                .map(|f| format!("data: {f}\n\n"))
                .collect();
            let datum = json!({"error": {"message": "the model process died"}});
            let mut chunks = tokens;
            chunks.push(format!("data: {datum}\n\n"));
            (chunks, Some(datum))
        }
        _ => unreachable!("the fault name was checked before the stream ran"),
    };
    // Every chunk is exactly what a byte reader receives: `lines` whole,
    // except the one line each side of the split carries a half of.
    json!({"kind": "ok", "chunks": chunks, "error": error})
}

/// The frames as SSE bytes, with one line cut across two chunks (the first
/// chunk ends mid-line, so only a reader that buffers to a newline recovers
/// it). The cut lands after the first line's `data: `, which is a real
/// boundary in an SSE stream.
fn split_at_half(lines: &[String]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cut = false;
    for line in lines {
        if !cut && line.len() > "data: ".len() + 2 {
            let at = "data: ".len() + 2;
            chunks.push(line[..at].to_string());
            chunks.push(line[at..].to_string());
            cut = true;
        } else {
            chunks.push(line.clone());
        }
    }
    if !cut && !chunks.is_empty() {
        // Nothing long enough to split: cut the last chunk in two anyway, so
        // a reader that does not buffer loses the tail rather than passing.
        let last = chunks.pop().unwrap_or_default();
        let at = last.len() / 2;
        chunks.push(last[..at].to_string());
        chunks.push(last[at..].to_string());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sovereign_contracts::double::TestProvider;
    use sovereign_contracts::types::{FinishReason, StreamFrame};

    use super::{run, Error, ReplayRequest};

    fn call(method: &str, input: serde_json::Value) -> ReplayRequest {
        serde_json::from_value(json!({"method": method, "input": input})).unwrap()
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
        assert!(
            serde_json::from_value::<ReplayRequest>(json!({"method": "logits", "input": {}}))
                .is_err(),
            "a method outside ReplayMethod is refused when the body is read"
        );
        assert_eq!(
            run(&p, &call("complete", json!({"no": "prompt"})))
                .await
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(run(&p, &call("embed", json!({}))).await.unwrap_err().0, 400);
        assert_eq!(
            run(&p, &call("probe", json!({"text": "x"})))
                .await
                .unwrap_err()
                .0,
            400
        );
    }

    #[tokio::test]
    async fn a_provider_that_holds_no_weights_refuses_the_probe() {
        let p = TestProvider::new();
        let answer = run(
            &p,
            &call(
                "probe",
                json!({"text": "The three laws", "steps": 4, "top_k": 3}),
            ),
        )
        .await
        .unwrap();
        assert!(
            matches!(answer, Err(Error::NotImplemented(_))),
            "the default answers NotImplemented, which the route reports as refused: {answer:?}"
        );
    }

    #[tokio::test]
    async fn every_stream_fault_splits_a_line_whole_and_ends_a_corrupt_stream_in_error() {
        let frames = vec![
            StreamFrame::Token("the whole answer".into()),
            StreamFrame::Finish {
                reason: FinishReason::Stop,
                usage: None,
            },
        ];
        let p = TestProvider::new().with_typed_frames(frames);
        let split = run(
            &p,
            &call(
                "stream_fault",
                json!({"fault": "split-line", "prompt": "q", "preferred_speed": "Fast"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let chunks: Vec<String> = split["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        // The answer text is whole once the chunks are joined: the reader,
        // not the cut, decides what a line is.
        let joined = chunks.concat();
        assert!(
            joined.contains("\"text\":\"the whole answer\""),
            "the cut lost the token line: {joined}"
        );
        assert!(
            chunks.iter().any(|c| !c.ends_with("\n\n")),
            "no chunk ended mid-line: the cut did not split a line"
        );

        let p = TestProvider::new().with_typed_frames(vec![StreamFrame::Token("x".into())]);
        let corrupt = run(
            &p,
            &call(
                "stream_fault",
                json!({"fault": "mid-stream", "prompt": "q", "preferred_speed": "Fast"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            corrupt["error"]["error"]["message"],
            "the model process died"
        );

        let p = TestProvider::new();
        let refused = run(
            &p,
            &call(
                "stream_fault",
                json!({"fault": "no-such-fault", "prompt": "q", "preferred_speed": "Fast"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(refused["kind"], "error");
        assert!(refused["message"]
            .as_str()
            .unwrap()
            .contains("not a fault the stream injects"));
    }

    #[tokio::test]
    async fn a_lifecycle_replay_reports_each_call_and_stamps_the_admission_it_is_given() {
        // The double honours warmup (Ok) and refuses extra load/unload (the
        // trait default), which is what a provider with no extras reports.
        let p = TestProvider::new().with_model_id("m");
        let answer = run(&p, &call("host_lifecycle", json!({})))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(answer["warmup_loaded"]["kind"], "ok");
        assert_eq!(answer["warmup_loaded"]["model"], "m");
        assert_eq!(answer["extra_loaded"]["kind"], "not-asked");
        assert_eq!(answer["extra_unloaded"]["kind"], "not-asked");

        let answer = run(
            &p,
            &call(
                "host_lifecycle",
                json!({"extra": {"slot": "s", "path": "/m.gguf", "context_size": 4096}}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            answer["extra_loaded"]["kind"], "err",
            "the double refuses an extra slot, as a provider with none does"
        );
        assert_eq!(answer["extra_unloaded"]["kind"], "err");

        // A `turn_admission` on a completion is stamped onto the request the
        // engine sees, which `admission`'s scenario relies on.
        let p = TestProvider::new().with_complete_text("ok");
        let answer = run(
            &p,
            &call(
                "complete",
                json!({"prompt": "q", "preferred_speed": "Fast", "turn_admission": "turn-7"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(answer["text"], "ok");
    }
}
