// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

use serde_json::{json, Value};
use sovereign_contracts::oicp::forced_choice;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::CompletionRequest;

use super::label_distribution;
use crate::RemoteApiProvider;

/// The labels' mass, renormalised; a label spelled with the tokenizer's
/// leading space counts; anything else is ignored; no label at all is None.
#[test]
fn the_distribution_is_the_labels_mass_renormalised() {
    let top = vec![
        json!({"token": "A", "logprob": 0.6f64.ln()}),
        json!({"token": " B", "logprob": 0.2f64.ln()}),
        json!({"token": "The", "logprob": 0.15f64.ln()}),
    ];
    let labels = vec!["A".to_string(), "B".to_string()];
    let d = label_distribution(&top, &labels).unwrap();
    assert!((d["A"] - 0.75).abs() < 1e-9, "{d:?}");
    assert!((d["B"] - 0.25).abs() < 1e-9, "{d:?}");
    assert_eq!(label_distribution(&top[2..], &labels), None);
}

/// A host that samples the label (llama-server): the native answer is not a
/// map, so the call is answered from one token's logprobs; the second call
/// goes straight there. The host counts which kind of request it saw.
#[tokio::test]
async fn a_host_that_samples_the_label_is_answered_from_its_logprobs() {
    let (native, logprobs) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (n, l) = (native.clone(), logprobs.clone());
    let chat = move |axum::Json(body): axum::Json<Value>| {
        let answer = if body.get("logprobs") == Some(&json!(true)) {
            l.fetch_add(1, SeqCst);
            assert_eq!(body["max_tokens"], 1);
            assert!(
                body.get("response_format").is_none(),
                "the schema is removed: {body}"
            );
            json!({"choices": [{"message": {"content": "A"}, "logprobs": {"content": [{"token": "A",
                "top_logprobs": [{"token": "A", "logprob": 0.9f64.ln()}, {"token": "B", "logprob": 0.1f64.ln()}]}]}}]})
        } else {
            n.fetch_add(1, SeqCst);
            json!({"choices": [{"message": {"content": "\"A\""}}]})
        };
        async move { axum::Json(answer) }
    };
    let url = serve(chat).await;
    let provider = RemoteApiProvider::new(&url, None, "m", 8192).originating();
    let request = CompletionRequest {
        prompt: "is it A or B?".into(),
        structured_output: Some(forced_choice::schema(&["A", "B"])),
        ..Default::default()
    };

    let first = forced_choice::parse(&provider.complete(&request).await.unwrap().text).unwrap();
    assert!((first["A"] - 0.9).abs() < 1e-9, "{first:?}");
    assert_eq!((native.load(SeqCst), logprobs.load(SeqCst)), (1, 1));

    let second = forced_choice::parse(&provider.complete(&request).await.unwrap().text).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        (native.load(SeqCst), logprobs.load(SeqCst)),
        (1, 2),
        "remembered: the second call skips the native ask"
    );
}

/// A hosted vendor as DeepSeek answered on 2026-10-13: it refuses the
/// forced-choice schema (400 as `response_format`, 400 as a forced function,
/// which takes only an object) and reasons unless thinking is switched off,
/// spending the one token on it and answering no logprobs. Asked first
/// through its logprobs with thinking off, it answers; the schema never goes.
#[tokio::test]
async fn a_third_party_is_asked_through_its_logprobs_first_with_thinking_off() {
    let schema_sent = Arc::new(AtomicUsize::new(0));
    let s = schema_sent.clone();
    let chat = move |axum::Json(body): axum::Json<Value>| {
        let schema = body.get("response_format").is_some() || body.get("tools").is_some();
        if schema {
            s.fetch_add(1, SeqCst);
        }
        let thinking_off = body.get("thinking") == Some(&json!({"type": "disabled"}));
        async move {
            if schema {
                let refusal =
                    json!({"error": {"message": "This response_format type is unavailable now"}});
                return (axum::http::StatusCode::BAD_REQUEST, axum::Json(refusal));
            }
            let answer = if thinking_off {
                json!({"choices": [{"message": {"content": "A"}, "logprobs": {"content": [{"token": "A",
                    "top_logprobs": [{"token": "A", "logprob": 0.8f64.ln()}, {"token": "B", "logprob": 0.2f64.ln()}]}]}}]})
            } else {
                json!({"choices": [{"message": {"content": "", "reasoning_content": "We"}, "logprobs": {"content": []}}]})
            };
            (axum::http::StatusCode::OK, axum::Json(answer))
        }
    };
    let url = serve(chat).await;
    let provider = RemoteApiProvider::third_party(&url, None, "m", 8192).unwrap();
    let request = CompletionRequest {
        prompt: "is it A or B?".into(),
        structured_output: Some(forced_choice::schema(&["A", "B"])),
        ..Default::default()
    };
    let d = forced_choice::parse(&provider.complete(&request).await.unwrap().text).unwrap();
    assert!((d["A"] - 0.8).abs() < 1e-9, "{d:?}");
    assert_eq!(
        schema_sent.load(SeqCst),
        0,
        "a vendor is never sent the schema of a forced choice"
    );
}

/// A host that answers with the map (a Sovereign daemon) is asked once.
#[tokio::test]
async fn a_host_that_answers_with_the_map_is_asked_once() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let chat = move |axum::Json(_): axum::Json<Value>| {
        h.fetch_add(1, SeqCst);
        async {
            axum::Json(json!({"choices": [{"message": {"content": "{\"A\":0.3,\"B\":0.7}"}}]}))
        }
    };
    let url = serve(chat).await;
    let provider = RemoteApiProvider::new(&url, None, "m", 8192).originating();
    let request = CompletionRequest {
        prompt: "is it A or B?".into(),
        structured_output: Some(forced_choice::schema(&["A", "B"])),
        ..Default::default()
    };
    let d = forced_choice::parse(&provider.complete(&request).await.unwrap().text).unwrap();
    assert_eq!(d["B"], 0.7);
    assert_eq!(hits.load(SeqCst), 1);
}

/// Under a conformance sink the client records each body it sends and each
/// raw body it receives, in order: here the native call and the logprobs
/// call that follows it.
#[tokio::test]
async fn every_wire_exchange_is_observed_in_order() {
    use sovereign_contracts::engine_observe::{observed, Observation};
    let chat = move |axum::Json(body): axum::Json<Value>| async move {
        axum::Json(if body.get("logprobs") == Some(&json!(true)) {
            json!({"choices": [{"message": {"content": "A"}, "logprobs": {"content": [{"token": "A",
                "top_logprobs": [{"token": "A", "logprob": 0.9f64.ln()}, {"token": "B", "logprob": 0.1f64.ln()}]}]}}]})
        } else {
            json!({"choices": [{"message": {"content": "\"A\""}}]})
        })
    };
    let url = serve(chat).await;
    let provider = RemoteApiProvider::new(&url, None, "m", 8192).originating();
    let request = CompletionRequest {
        prompt: "is it A or B?".into(),
        structured_output: Some(forced_choice::schema(&["A", "B"])),
        ..Default::default()
    };

    let (answer, seen) = observed(provider.complete(&request)).await;
    answer.unwrap();
    let kinds: Vec<&str> = seen
        .iter()
        .map(|o| match o {
            Observation::WireRequest { body } if body.get("logprobs").is_some() => {
                "logprobs request"
            }
            Observation::WireRequest { .. } => "native request",
            Observation::WireResponse { status: 200, .. } => "response",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["native request", "response", "logprobs request", "response"]
    );
    let Observation::WireRequest { body } = &seen[0] else {
        unreachable!()
    };
    assert_eq!(body["messages"][0]["content"], "is it A or B?");
}

async fn serve<H, T>(chat: H) -> String
where
    H: axum::handler::Handler<T, ()>,
    T: 'static,
{
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// C5 (`projection.usage-forced-choice`): a logprobs answer reports the
/// usage the engine reports for its one forward pass, the real prompt count
/// and no completion, whichever call reached it. Case da62919cb6f3 reported
/// prompt 191, completion 1 (the native ask's usage); the remembered route
/// reported 0 and 0.
#[tokio::test]
async fn a_logprobs_answer_reports_its_prompt_and_no_completion() {
    let chat = move |axum::Json(body): axum::Json<Value>| async move {
        axum::Json(if body.get("logprobs") == Some(&json!(true)) {
            json!({"choices": [{"finish_reason": "length", "message": {"content": "A"},
                "logprobs": {"content": [{"token": "A", "top_logprobs": [
                    {"token": "A", "logprob": 0.9f64.ln()}, {"token": "B", "logprob": 0.1f64.ln()}]}]}}],
                "usage": {"prompt_tokens": 191, "completion_tokens": 1, "total_tokens": 192}})
        } else {
            json!({"choices": [{"finish_reason": "stop", "message": {"content": "\"A\""}}],
                "usage": {"prompt_tokens": 195, "completion_tokens": 4, "total_tokens": 199}})
        })
    };
    let url = serve(chat).await;
    let provider = RemoteApiProvider::new(&url, None, "m", 8192).originating();
    let request = CompletionRequest {
        prompt: "is it A or B?".into(),
        structured_output: Some(forced_choice::schema(&["A", "B"])),
        ..Default::default()
    };
    for call in ["native first", "remembered"] {
        let r = provider.complete(&request).await.unwrap();
        assert!(
            forced_choice::parse(&r.text).is_some(),
            "{call}: {}",
            r.text
        );
        assert_eq!(
            (r.prompt_tokens, r.tokens_used, r.completion_tokens),
            (191, 191, Some(0)),
            "{call}"
        );
        assert_eq!(
            r.finish_reason,
            Some(sovereign_contracts::types::FinishReason::Stop),
            "{call}"
        );
    }
}
