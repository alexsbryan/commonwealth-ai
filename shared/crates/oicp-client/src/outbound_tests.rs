// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sovereign_contracts::error::Error;
use sovereign_contracts::oicp::{InferenceRequirements, ShardingPrivacy};
use sovereign_contracts::traits::{InferenceProvider, ServingLocus};
use sovereign_contracts::types::{CompletionRequest, Speed};

use super::{EngineEmbed, FarEnd};
use crate::{RemoteApiProvider, SplitInferenceProvider};

/// A vendor in miniature: answers chat, embeddings and rerank, counts every
/// request that reaches it, and keeps the chat bodies.
async fn vendor() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<Value>>>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (h, b) = (hits.clone(), bodies.clone());
    let chat = move |axum::Json(body): axum::Json<Value>| {
        h.fetch_add(1, SeqCst);
        b.lock().unwrap().push(body);
        async { axum::Json(json!({"choices": [{"message": {"content": "ok"}}]})) }
    };
    let h = hits.clone();
    let other = move || {
        h.fetch_add(1, SeqCst);
        async { axum::Json(json!({"data": [{"embedding": [0.0], "index": 0}]})) }
    };
    let app = axum::Router::new()
        .route("/v1/chat/completions", axum::routing::post(chat))
        .route("/v1/embeddings", axum::routing::post(other.clone()))
        .route("/v1/rerank", axum::routing::post(other));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}/v1"), hits, bodies)
}

fn request(sharding: Option<ShardingPrivacy>) -> CompletionRequest {
    CompletionRequest {
        prompt: "a chunk of somebody's mailbox".into(),
        model_id: Some("vendor-model".into()),
        oicp: sharding.map(|s| InferenceRequirements::new().with_sharding(s)),
        ..Default::default()
    }
}

fn refused<T>(result: sovereign_contracts::error::Result<T>, what: &str) {
    match result {
        Err(Error::PermissionDenied(msg)) => assert!(
            msg.contains("embed_path"),
            "{what}: the refusal names where embeddings belong: {msg}"
        ),
        Err(other) => panic!("{what}: refused with the wrong kind: {other}"),
        Ok(_) => panic!("{what}: texts sent to a third party"),
    }
}

/// The operator's `[engine]` is the release: a completion reaches the vendor
/// whatever its envelope says, and texts to embed or rerank never do. Every
/// sending method is driven, so a method that forgets admission is the one
/// that moves the counter.
#[tokio::test]
async fn a_third_party_gets_completions_and_never_texts() {
    let (url, hits, _) = vendor().await;
    let p = RemoteApiProvider::third_party(&url, Some("key".into()), "vendor-model", 8192).unwrap();
    for sharding in [
        None,
        Some(ShardingPrivacy::LocalOnly),
        Some(ShardingPrivacy::MeshAllowed),
    ] {
        assert_eq!(p.complete(&request(sharding)).await.unwrap().text, "ok");
    }
    assert_eq!(hits.load(SeqCst), 3);
    refused(p.embed("chunk").await, "embed");
    refused(p.embed_query("query").await, "embed_query");
    refused(p.embed_batch(&["chunk".to_string()]).await, "embed_batch");
    refused(
        p.rerank_batch("query", &["chunk".to_string()]).await,
        "rerank_batch",
    );
    assert_eq!(hits.load(SeqCst), 3, "no text reached the third party");
}

/// A vendor does not speak OICP: our envelope stays here.
#[tokio::test]
async fn a_completion_reaches_the_third_party_without_our_envelope() {
    let (url, hits, bodies) = vendor().await;
    let p = RemoteApiProvider::third_party(&url, None, "vendor-model", 8192).unwrap();
    let answer = p
        .complete(&request(Some(ShardingPrivacy::MeshAllowed)))
        .await
        .unwrap();
    assert_eq!(answer.text, "ok");
    assert_eq!(hits.load(SeqCst), 1);
    let body = bodies.lock().unwrap()[0].clone();
    assert!(
        body.get("oicp").is_none(),
        "envelope sent to a vendor: {body}"
    );
}

/// Admission binds only a third party: a peer and the caller's own daemon
/// are reached exactly as before, whatever the envelope says.
#[tokio::test]
async fn only_a_third_party_admits() {
    let (url, hits, _) = vendor().await;
    let peer = RemoteApiProvider::new(&url, None, "m", 8192);
    let own = RemoteApiProvider::new(&url, None, "m", 8192).originating();
    assert_eq!(
        (peer.far_end(), own.far_end()),
        (FarEnd::Peer, FarEnd::Origin)
    );
    peer.complete(&request(None)).await.unwrap();
    own.complete(&request(Some(ShardingPrivacy::LocalOnly)))
        .await
        .unwrap();
    assert_eq!(hits.load(SeqCst), 2);
}

/// D1: an engine endpoint off this machine is a third party, and the locus
/// follows the chat half. A loopback server is this machine.
#[test]
fn an_engine_off_this_machine_is_a_third_party() {
    let pair = |chat: &str, embed: &str| {
        let embed = EngineEmbed::Remote {
            endpoint_v1: embed.into(),
            model_id: "e".into(),
        };
        SplitInferenceProvider::engine(
            chat,
            embed,
            None,
            "c".into(),
            None,
            8192,
            None,
            Default::default(),
        )
        .unwrap()
    };
    let vendor = pair("https://api.example.com/v1", "http://127.0.0.1:8001/v1");
    assert_eq!(vendor.serving_locus(), ServingLocus::ForwardsToThirdParty);
    assert_eq!(vendor.chat.far_end(), FarEnd::ThirdParty);

    let local = pair("http://127.0.0.1:8000/v1", "http://127.0.0.1:8001/v1");
    assert_eq!(local.serving_locus(), ServingLocus::ForwardsOnBox);
    assert_eq!(local.chat.far_end(), FarEnd::Origin);
}

/// A hosted engine with `[engine] embed_path`: chat goes to the vendor, and
/// every embedding is answered in this process. The vendor counts what
/// reaches it, so an embedding that leaked would move the counter.
#[tokio::test]
async fn a_hosted_engine_embeds_in_this_process() {
    let (url, hits, _) = vendor().await;
    let local = sovereign_contracts::double::TestProvider::new()
        .with_embed_marker(|t| vec![t.len() as f32; 4]);
    let embed = EngineEmbed::Local {
        provider: Arc::new(local),
        model_id: "Qwen3-Embedding-0.6B-Q8_0".into(),
    };
    let engine = SplitInferenceProvider::engine(
        &url,
        embed,
        None,
        "vendor-model".into(),
        None,
        8192,
        None,
        Default::default(),
    )
    .unwrap();
    assert_eq!(engine.embed("chunk").await.unwrap(), vec![5.0; 4]);
    assert_eq!(engine.embed_query("q").await.unwrap(), vec![1.0; 4]);
    assert_eq!(
        engine.embed_batch(&["ab".to_string()]).await.unwrap(),
        vec![vec![2.0; 4]]
    );
    assert_eq!(engine.embed_model_id(), "Qwen3-Embedding-0.6B-Q8_0");
    assert_eq!(hits.load(SeqCst), 0, "an embedding reached the vendor");
    engine.complete(&request(None)).await.unwrap();
    assert_eq!(hits.load(SeqCst), 1, "chat goes to the vendor");
}

/// A hosted engine serves its vendor models, so it reports them as resident,
/// and a turn that names no model is pinned to the one for its speed. THE
/// FAILING INPUT: an unnamed fast turn went out with `"model": ""`, which a
/// vendor rejects.
#[tokio::test]
async fn a_hosted_engine_reports_its_models_and_pins_unnamed_turns() {
    let (url, _, bodies) = vendor().await;
    let local = sovereign_contracts::double::TestProvider::new().with_embed_marker(|_| vec![0.0]);
    let embed = EngineEmbed::Local {
        provider: Arc::new(local),
        model_id: "embed-gguf".into(),
    };
    let engine = SplitInferenceProvider::engine(
        &url,
        embed,
        None,
        "big".into(),
        Some("small".into()),
        8192,
        None,
        Default::default(),
    )
    .unwrap();
    let slots: Vec<_> = engine
        .resident_slots()
        .into_iter()
        .map(|s| (s.role, s.model_id, s.resident))
        .collect();
    let want = |r: &str, m: &str| (r.to_string(), m.to_string(), true);
    assert_eq!(
        slots,
        [
            want("primary", "big"),
            want("fast", "small"),
            want("embed", "embed-gguf")
        ]
    );
    assert_eq!(engine.model_id_for(Speed::Fast), "small");
    assert_eq!(engine.model_id_for(Speed::Slow), "big");

    for speed in [Speed::Fast, Speed::Slow] {
        let unnamed = CompletionRequest {
            prompt: "hi".into(),
            preferred_speed: speed,
            ..Default::default()
        };
        engine.complete(&unnamed).await.unwrap();
    }
    let models: Vec<String> = bodies
        .lock()
        .unwrap()
        .iter()
        .map(|b| b["model"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(models, ["small", "big"]);
}

/// Only a hosted engine reports slots. A forwarder holds nothing, and a slot
/// it reported would advertise its entry node's model as its own.
#[test]
fn a_forwarder_reports_no_slots() {
    let forwarder = SplitInferenceProvider::new(
        "http://127.0.0.1:9741/v1",
        "m".into(),
        "e".into(),
        8192,
        String::new(),
    );
    assert!(forwarder.resident_slots().is_empty());
}

/// The admission table, whole: every far end against every payload.
#[test]
fn admission_is_a_table_over_far_end_and_payload() {
    use super::{Payload, ThirdPartyRefusal};
    for (payload, third_party) in [
        (Payload::Probe, Ok(())),
        (Payload::Completion, Ok(())),
        (Payload::Texts, Err(ThirdPartyRefusal)),
    ] {
        assert_eq!(FarEnd::Peer.admit(payload), Ok(()), "peer: {payload:?}");
        assert_eq!(FarEnd::Origin.admit(payload), Ok(()), "origin: {payload:?}");
        assert_eq!(
            FarEnd::ThirdParty.admit(payload),
            third_party,
            "third party: {payload:?}"
        );
    }
}

/// D1's one classification: this machine is `Origin`, everything else, an
/// unreadable address included, is a third party.
#[test]
fn an_engine_endpoint_is_this_machine_only_when_it_says_loopback() {
    for (endpoint, far_end) in [
        ("http://127.0.0.1:8000/v1", FarEnd::Origin),
        ("http://localhost:9741/v1", FarEnd::Origin),
        ("http://[::1]:9741/v1", FarEnd::Origin),
        ("https://openrouter.ai/api/v1", FarEnd::ThirdParty),
        ("http://192.168.1.20:9741/v1", FarEnd::ThirdParty),
        ("http://127.example.com/v1", FarEnd::ThirdParty),
        ("not an address", FarEnd::ThirdParty),
        ("", FarEnd::ThirdParty),
    ] {
        assert_eq!(FarEnd::of_engine_endpoint(endpoint), far_end, "{endpoint}");
    }
}

/// A host that IGNORES `response_format` (Anthropic's OpenAI-compatible
/// endpoint documents it so) answers 200 with unconstrained text, which the
/// 400-driven fallback never sees. `[engine] structured_output` tells the
/// engine up front: the first schema request goes out as a forced function
/// call, with no `response_format` at all.
#[tokio::test]
async fn an_engine_told_tool_use_forced_sends_schemas_as_a_function_call() {
    let (url, _, bodies) = vendor().await;
    let local = sovereign_contracts::double::TestProvider::new().with_embed_marker(|_| vec![0.0]);
    let embed = EngineEmbed::Local {
        provider: Arc::new(local),
        model_id: "embed-gguf".into(),
    };
    let engine = SplitInferenceProvider::engine(
        &url,
        embed,
        None,
        "vendor-model".into(),
        None,
        8192,
        None,
        crate::StructuredOutputMode::ToolUseForced,
    )
    .unwrap();
    let schema =
        json!({"title": "atoms", "type": "object", "properties": {"a": {"type": "string"}}});
    let asked = CompletionRequest {
        structured_output: Some(schema),
        ..request(None)
    };
    // The miniature vendor answers in text whatever it is asked, so the
    // answer is not the subject here: the body that left is.
    let _ = engine.complete(&asked).await;
    let body = bodies
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the vendor was asked");
    assert!(body.get("response_format").is_none(), "{body}");
    assert_eq!(body["tool_choice"]["function"]["name"], "atoms", "{body}");
}
