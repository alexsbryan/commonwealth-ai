// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dial against a stub svrn over real HTTP and a real WebSocket.
//!
//! In-process, a lane scored `collect_turn`'s text and the metadata the
//! store held for that message, and read chunk text out of the index. The
//! scorers after that point are unchanged, so the verdict is the same iff
//! the dial hands them the same three things. The stub serves one canned
//! turn; the expected values below are what the in-process path produced
//! for that text and metadata, written before the dial ran.

use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::*;
use crate::bench_cmd::live_runner::run_live;
use crate::eval_cmd::bank::EvalBank;
use crate::eval_cmd::runner::run_bank_synth;

const ANSWER: &str = "<think>scratch</think>The capital of France is Paris.";
const CHUNK: &str = "France is a country in Europe whose capital is Paris.";

/// What the stub saw on the wire: conversation-create bodies and turn
/// requests, in order.
#[derive(Default)]
struct Seen {
    create: Vec<Value>,
    turns: Vec<Value>,
}
type Shared = Arc<Mutex<Seen>>;

fn persisted_metadata() -> Value {
    json!({
        "retrieved_chunks": [{
            "corpus_id": "geo",
            "chunk_id": 7,
            "title": "France",
            "snippet": "France is a country…",
        }],
        "grounding_gate": { "action": "released" },
        "provenance": { "sources": [{ "origin": "corpus" }] },
    })
}

async fn create(State(seen): State<Shared>, Json(body): Json<Value>) -> Json<Value> {
    seen.lock().unwrap().create.push(body.clone());
    let mut out = json!({ "id": "c1", "created_at": 1 });
    if let Some(allow) = body.get("enabled_corpora") {
        out["enabled_corpora"] = allow.clone();
    }
    Json(out)
}

async fn stream(State(seen): State<Shared>, ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(move |mut socket| async move {
        if let Some(Ok(Message::Text(t))) = socket.recv().await {
            let req: Value = serde_json::from_str(t.as_str()).unwrap();
            seen.lock().unwrap().turns.push(req);
            for frame in [
                json!({ "type": "token", "data": { "message_id": "m1", "chunk": ANSWER } }),
                json!({ "type": "complete", "data": { "message_id": "m1" } }),
            ] {
                let _ = socket.send(Message::Text(frame.to_string().into())).await;
            }
        }
    })
}

async fn history(Path(id): Path<String>) -> Json<Value> {
    Json(json!({
        "id": id, "title": null, "created_at": 1, "updated_at": 1,
        "messages": [
            { "id": "u1", "role": "user", "content": "q", "created_at": 1 },
            { "id": "m1", "role": "assistant", "content": ANSWER, "created_at": 1,
              "metadata": persisted_metadata() },
        ],
    }))
}

async fn chunk() -> Json<Value> {
    Json(json!({ "chunk_id": "7", "content": CHUNK, "title": "France" }))
}

async fn stub_svrn() -> (String, Shared) {
    let seen = Shared::default();
    let app = Router::new()
        .route("/v1/conversations", post(create))
        .route("/v1/conversations/{id}", get(history))
        .route("/v1/conversations/{id}/stream", get(stream))
        .route("/internal/meshapp/{corpus}/chunks/{id}", get(chunk))
        .with_state(Arc::clone(&seen));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

/// The dial as `SubjectDial::dial` builds it, minus the model probe the
/// stub does not serve.
fn dialed(base: &str, sampling: Option<SamplingOverrides>) -> SubjectDial {
    SubjectDial {
        client: TurnClient::new(base),
        base: base.to_string(),
        sampling,
        rerank: None,
        inference: Arc::new(oicp_client::SplitInferenceProvider::new_with_bearer(
            &format!("{base}/v1"),
            None,
            "chat".into(),
            "embed".into(),
            8192,
            String::new(),
        )),
        chat_model: "chat".into(),
    }
}

#[tokio::test]
async fn a_live_lane_gets_what_svrn_streamed_and_persisted() {
    let (base, seen) = stub_svrn().await;
    let pins = SamplingOverrides {
        temperature: Some(0.0),
        top_p: None,
        max_tokens: Some(512),
    };
    let live = run_live(
        &dialed(&base, Some(pins)),
        "geo",
        "What is the capital of France?",
    )
    .await;

    assert_eq!(live.visible, "The capital of France is Paris.");
    assert_eq!(live.retrieved_chunk_texts, vec![CHUNK.to_string()]);
    assert_eq!(live.gate_action.as_deref(), Some("released"));
    assert_eq!(live.draft, None);
    assert_eq!(live.metadata, persisted_metadata());

    let seen = seen.lock().unwrap();
    assert_eq!(seen.create[0]["enabled_corpora"], json!(["geo"]), "sealed");
    let data = &seen.turns[0]["data"];
    assert_eq!(data["content"], "What is the capital of France?");
    assert_eq!(
        data["sampling"],
        json!({ "temperature": 0.0, "max_tokens": 512 })
    );
}

#[tokio::test]
async fn eval_synth_scores_the_dialed_turn_as_it_scored_in_process() {
    let (base, seen) = stub_svrn().await;
    let bank: EvalBank = serde_json::from_value(json!({
        "bank": { "name": "stub", "corpus": "geo" },
        "questions": [{
            "id": "q1", "category": "factual",
            "question": "What is the capital of France?",
            "expected_facts": ["Paris"], "expected_sources": ["France"],
        }],
    }))
    .unwrap();
    let run = run_bank_synth(
        &dialed(&base, None),
        &bank,
        false,
        true,
        sovereign_contracts::types::TurnMode::Grounded,
    )
    .await
    .unwrap();

    let r = &run.results[0];
    assert_eq!(r.error, None);
    assert_eq!(r.fact_score.matched, vec!["Paris".to_string()]);
    assert!(r.fact_score.missing.is_empty());
    assert_eq!(r.source_score.matched, vec!["France".to_string()]);
    assert_eq!(r.corpora_hit, vec!["geo".to_string()]);
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.create[0]["enabled_corpora"],
        json!(["geo"]),
        "--isolate"
    );
    assert!(seen.turns[0]["data"].get("sampling").is_none());
    assert!(seen.turns[0]["data"].get("rerank").is_none());
}

/// A promote arm's rerank pins ride every turn the dial asks.
#[tokio::test]
async fn a_pinned_rerank_rides_the_turn() {
    let (base, seen) = stub_svrn().await;
    let mut dial = dialed(&base, None);
    dial.pin_rerank(RerankOverrides {
        enabled: Some(true),
        candidates_k: Some(80),
    });
    run_live(&dial, "geo", "What is the capital of France?").await;
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.turns[0]["data"]["rerank"],
        json!({ "enabled": true, "candidates_k": 80 })
    );
}

#[tokio::test]
async fn nothing_at_the_base_is_could_not_judge_naming_it() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut globals = sovereign_cli_base::chat_globals::default_globals_for_voice_eval();
    globals.daemon_base = format!("http://127.0.0.1:{port}");
    let err = SubjectDial::dial(&globals)
        .await
        .err()
        .expect("no svrn there");
    assert!(err.starts_with("could-not-judge:"), "{err}");
    assert!(err.contains(&globals.daemon_base), "{err}");
}

#[tokio::test]
async fn a_data_dir_pin_is_refused_by_name() {
    let mut globals = sovereign_cli_base::chat_globals::default_globals_for_voice_eval();
    globals.data_dir_explicit = true;
    let err = SubjectDial::dial(&globals).await.err().expect("refused");
    assert!(err.starts_with("--data-dir"), "{err}");
}
