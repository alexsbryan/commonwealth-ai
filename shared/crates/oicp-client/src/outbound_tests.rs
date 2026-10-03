// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sovereign_contracts::error::Error;
use sovereign_contracts::oicp::{InferenceRequirements, ShardingPrivacy};
use sovereign_contracts::traits::{InferenceProvider, ServingLocus};
use sovereign_contracts::types::CompletionRequest;

use super::FarEnd;
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
            msg.contains("third_party_allowed"),
            "{what}: the refusal names the declaration that would grant it: {msg}"
        ),
        Err(other) => panic!("{what}: refused with the wrong kind: {other}"),
        Ok(_) => panic!("{what}: sent to a third party without the declaration"),
    }
}

/// THE FAILING INPUT: the probe's phase-1 requests (no envelope) and its
/// composer requests (`local_only`) both reached DeepSeek or were refused for
/// the wrong reason. Every sending method is driven, so a method that forgets
/// to admit is the one that moves the counter.
#[tokio::test]
async fn a_third_party_is_sent_nothing_the_request_did_not_declare() {
    let (url, hits, _) = vendor().await;
    let p = RemoteApiProvider::third_party(&url, Some("key".into()), "vendor-model", 8192).unwrap();
    for sharding in [
        None,
        Some(ShardingPrivacy::LocalOnly),
        Some(ShardingPrivacy::MeshAllowed),
    ] {
        let req = request(sharding);
        refused(p.complete(&req).await, "complete");
        refused(p.complete_stream(&req).await, "complete_stream");
        refused(
            p.complete_stream_with_finish(&req).await,
            "complete_stream_with_finish",
        );
        refused(
            p.complete_batch(std::slice::from_ref(&req)).await,
            "complete_batch",
        );
    }
    refused(p.embed("chunk").await, "embed");
    refused(p.embed_query("query").await, "embed_query");
    refused(p.embed_batch(&["chunk".to_string()]).await, "embed_batch");
    refused(
        p.rerank_batch("query", &["chunk".to_string()]).await,
        "rerank_batch",
    );
    assert_eq!(hits.load(SeqCst), 0, "nothing reached the third party");
}

/// The other direction: a declared request is served, and our envelope stays
/// here. A vendor does not speak OICP.
#[tokio::test]
async fn a_declared_request_reaches_the_third_party_without_our_envelope() {
    let (url, hits, bodies) = vendor().await;
    let p = RemoteApiProvider::third_party(&url, None, "vendor-model", 8192).unwrap();
    let answer = p
        .complete(&request(Some(ShardingPrivacy::ThirdPartyAllowed)))
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
        SplitInferenceProvider::engine(chat, embed, None, "c".into(), "e".into(), 8192, None)
            .unwrap()
    };
    let vendor = pair("https://api.example.com/v1", "https://api.example.com/v1");
    assert_eq!(vendor.serving_locus(), ServingLocus::ForwardsToThirdParty);
    assert_eq!(vendor.embed.far_end(), FarEnd::ThirdParty);

    let local = pair("http://127.0.0.1:8000/v1", "http://127.0.0.1:8001/v1");
    assert_eq!(local.serving_locus(), ServingLocus::ForwardsOnBox);
    assert_eq!(
        (local.chat.far_end(), local.embed.far_end()),
        (FarEnd::Origin, FarEnd::Origin)
    );

    // Hosted chat, embeddings on this machine (the M1 shape).
    let split = pair("https://api.example.com/v1", "http://127.0.0.1:9741/v1");
    assert_eq!(split.serving_locus(), ServingLocus::ForwardsToThirdParty);
    assert_eq!(split.embed.far_end(), FarEnd::Origin);
}

/// The admission table, whole: every far end against every payload shape and
/// every declaration a completion can carry.
#[test]
fn admission_is_a_table_over_far_end_and_payload() {
    use super::{Payload, ThirdPartyRefusal};
    let env = |s| InferenceRequirements::new().with_sharding(s);
    let (local, mesh, third) = (
        env(ShardingPrivacy::LocalOnly),
        env(ShardingPrivacy::MeshAllowed),
        env(ShardingPrivacy::ThirdPartyAllowed),
    );
    let undeclared = |declared| Err(ThirdPartyRefusal::Undeclared { declared });
    let rows = [
        (Payload::Probe, Ok(())),
        (Payload::Texts, Err(ThirdPartyRefusal::Undeclarable)),
        (Payload::Completion(None), undeclared(None)),
        (
            Payload::Completion(Some(&local)),
            undeclared(Some(ShardingPrivacy::LocalOnly)),
        ),
        (
            Payload::Completion(Some(&mesh)),
            undeclared(Some(ShardingPrivacy::MeshAllowed)),
        ),
        (Payload::Completion(Some(&third)), Ok(())),
    ];
    for (payload, third_party) in rows {
        assert_eq!(FarEnd::Peer.admit(&payload), Ok(()), "peer: {payload:?}");
        assert_eq!(
            FarEnd::Origin.admit(&payload),
            Ok(()),
            "origin: {payload:?}"
        );
        assert_eq!(
            FarEnd::ThirdParty.admit(&payload),
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
