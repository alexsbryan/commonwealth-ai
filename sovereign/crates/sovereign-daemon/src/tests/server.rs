// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the server surface — see `server.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use crate::state::{test_app_state, test_app_state_with_token};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// THE GAP. `/v1/embeddings` was the one inference route with no
/// admission layer, so a peer could drive this host's embed slot while
/// contribution was paused — past the ceiling, past the foreground yield,
/// and tallied nowhere.
///
/// Measured against a live daemon 2026-08-31 before the fix: with one node
/// id at one moment, `/v1/chat/completions` answered **503** and
/// `/v1/embeddings` answered **200 and served**. Surfaced by a two-machine
/// terminal run whose embeddings reached their entry node while the entry
/// node's `peer_requests` stayed empty.
///
/// Asserted as a PAIR: chat is the control. A test that only checked
/// embeddings would pass just as well if the whole gate stopped working.
#[tokio::test]
async fn a_paused_host_refuses_peer_embeddings_exactly_as_it_refuses_peer_chat() {
    let state = test_app_state();
    // Paused far enough ahead that the window cannot lapse mid-test.
    state.set_contribution_paused_until(sovereign_time::unix_now() + 3600);
    let peer = commonwealth_core::ids::NodeId::from_u128(0xBEEF).to_hex();

    for path in ["/v1/chat/completions", "/v1/embeddings"] {
        let resp = mock_router(state.clone())
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .header("x-node-id", &peer)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .expect("the gate must answer, not hang");
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: a paused host must refuse a PEER request before the handler runs"
        );
    }
}

/// The other half, so the fix cannot be "503 everything": a LOCAL caller
/// carries no `X-Node-Id` and is never a peer, so a pause must not touch
/// the operator's own embeddings. Asserted as "not 503" rather than a
/// specific code — the handler's own outcome on a stub state is not this
/// test's business.
#[tokio::test]
async fn a_paused_host_still_serves_its_own_embeddings() {
    let state = test_app_state();
    state.set_contribution_paused_until(sovereign_time::unix_now() + 3600);
    let resp = mock_router(state)
        .oneshot(
            Request::post("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .expect("a local request must reach the handler");
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a pause rations PEERS; the operator's own machine is never gated"
    );
}

#[tokio::test]
async fn status_endpoint() {
    let app = mock_router(test_app_state());

    let response = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("node_id").is_some());
    assert!(json.get("mesh").is_some());
}

#[tokio::test]
async fn models_endpoint_empty() {
    let app = mock_router(test_app_state());

    let response = app
        .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["object"], "list");
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn oicp_capabilities_endpoint() {
    let app = mock_router(test_app_state());

    let response = app
        .oneshot(
            Request::get("/oicp/v1/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["oicp_version"], oicp_types::OICP_VERSION);
    assert_eq!(json["provider"]["type"], "mesh");
}

#[tokio::test]
async fn chat_completions_no_model_loaded() {
    let app = mock_router(test_app_state());

    let body = serde_json::json!({
        "messages": [{"role": "user", "content": "Hello"}]
    });

    let response = app
        .oneshot(
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should fail because no models are loaded.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn responses_endpoint_rejects_previous_response_id() {
    // The /v1/responses adapter doesn't implement server-side
    // conversation state. A request that carries
    // `previous_response_id` must 400 so codex falls back to
    // resending full history.
    let app = mock_router(test_app_state());
    let body = serde_json::json!({
        "model": "x",
        "input": "hi",
        "previous_response_id": "resp_old"
    });
    let response = app
        .oneshot(
            Request::post("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("previous_response_id"));
}

#[tokio::test]
async fn responses_endpoint_no_model_loaded_returns_503() {
    // With no local_inference and no loaded models, the inner
    // chat_completions handler returns 503. The adapter forwards
    // it as-is.
    let app = mock_router(test_app_state());
    let body = serde_json::json!({
        "model": "x",
        "input": "hello"
    });
    let response = app
        .oneshot(
            Request::post("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn responses_endpoint_accepts_codex_shape_request() {
    // Pin the wire shape codex actually sends so we know the
    // adapter parses the canonical request format. We don't drive
    // it through to a successful inference here — there's no model
    // loaded — but the request must at least deserialise and
    // reach the inner handler (i.e. 503, not 400).
    let app = mock_router(test_app_state());
    let body = serde_json::json!({
        "model": "primary",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }
        ],
        "instructions": "you are terse",
        "tools": [{
            "type": "function",
            "name": "shell",
            "description": "run a shell command",
            "parameters": {
                "type": "object",
                "properties": {"cmd": {"type": "string"}},
                "required": ["cmd"]
            }
        }],
        "tool_choice": "auto",
        "stream": false,
        "max_output_tokens": 1024,
        "store": false,
        "parallel_tool_calls": true,
        "reasoning": {"effort": "medium"}
    });
    let response = app
        .oneshot(
            Request::post("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    // Reached the inner handler => translation succeeded.
    // The inner handler 503s with no model loaded.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn chat_completions_rejects_local_only() {
    let app = mock_router(test_app_state());

    let body = serde_json::json!({
        "messages": [{"role": "user", "content": "Hello"}],
        "oicp": {
            "oicp_version": "0.1.0",
            "privacy": { "sharding": "local_only" }
        }
    });

    let response = app
        .oneshot(
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("local_only"));
}

#[tokio::test]
async fn internal_gossip_endpoint_rejects_wrong_mesh() {
    // After the gossip handler was wired for real (replacing the
    // accept-any-JSON stub), the minimal shape it accepts is a
    // full `MeshWire` payload. A test AppState has mesh_id=1 and
    // an all-zero invite_key_hash; posting a body with a different
    // mesh_id proves the auth guard fires. The full "merges
    // incoming delta" happy path is covered by the dedicated
    // tests/gossip_route.rs integration file.
    let app = internal_router(test_app_state());

    // MeshId serializes as a 16-byte array; hash as a 32-byte
    // array. Both built as vecs so `serde_json::json!` is happy.
    let mesh_id_bytes = vec![0u8; 16];
    let hash_bytes = vec![0u8; 32];
    // Flip one byte in the id to differ from test_app_state()'s
    // default, so the handler's mesh-id check fires.
    let mut foreign_id = mesh_id_bytes.clone();
    foreign_id[0] = 42;
    let body = serde_json::json!({
        "mesh": {
            "id": foreign_id,
            "name": "Other",
            "join_key_hash": hash_bytes,
            "members": [],
            "peers": []
        }
    });
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn internal_latency_probe_endpoint() {
    let app = internal_router(test_app_state());

    let response = app
        .oneshot(
            Request::get("/internal/latency/probe")
                // `internal_gate` reads a missing `ConnectInfo` as "not
                // loopback" and refuses; the real listener attaches one.
                .extension(axum::extract::ConnectInfo(SocketAddr::from((
                    [127, 0, 0, 1],
                    54321,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

/// The warm route must be reachable on the CLIENT port and absent
/// from the peer-reachable one. It sat only on `internal_router`
/// (`:9742`) until 2026-07-27: the desktop and `oicp-client` derive
/// the URL from their `/v1` endpoint — `:9741` — so every warm-up
/// POST 404'd and was swallowed as a best-effort no-op, which is
/// why Attach mode silently never warmed its model while looking
/// fully wired. Both directions are pinned, because moving it back
/// would re-disable warm-up in the shipped app without failing
/// anything else.
#[tokio::test]
async fn warmup_route_is_on_the_client_port_not_the_peer_port() {
    let response = mock_router(test_app_state())
        .oneshot(
            Request::post("/internal/inference/warmup")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "warmup must be routable on the client port the desktop actually calls"
    );

    let response = internal_router(test_app_state())
        .oneshot(
            Request::post("/internal/inference/warmup")
                .header("content-type", "application/json")
                .extension(axum::extract::ConnectInfo(SocketAddr::from((
                    [127, 0, 0, 1],
                    54321,
                ))))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "an 18.5 GB disk load must not be a lever any mesh peer can pull"
    );
}

/// The other half of that sentence, and the one that was false until
/// 2026-08-28. Keeping warmup off `:9742` was never enough: a MEMBER
/// dialling `CLIENT_ALPN` is forwarded to a bind of THIS router, and
/// arrives wearing the acceptor's loopback address, so `client_auth`
/// admits it before reading anything. The peer bind serves a router
/// where the route does not exist.
///
/// Each surface is driven with a credential it ACCEPTS, so the only
/// thing left to observe is whether the route is mounted. Asserting
/// 404 through a refusal would prove nothing — a 401 also is not 200.
#[tokio::test]
async fn the_peer_and_guest_surfaces_do_not_serve_the_operator_only_routes() {
    const OPERATOR_ONLY: &[&str] = &[
        "/internal/inference/warmup",
        "/internal/guest/grant",
        "/internal/guest/grant/revoke",
    ];
    const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

    // `Peer` trusts a loopback caller — a member's key was already proved
    // at the QUIC handshake — so the injected ConnectInfo admits us.
    for path in OPERATOR_ONLY {
        let response = mock_router_for(test_app_state(), ClientSurface::Peer)
            .oneshot(
                Request::post(*path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "peer surface must not serve {path}"
        );
    }

    // `Guest` and `Rail` do not trust loopback, so they need the daemon
    // token to get past auth. Once past it, the same routes are simply
    // absent.
    for surface in [ClientSurface::Guest, ClientSurface::Rail] {
        for path in OPERATOR_ONLY {
            let state = test_app_state_with_token(Some(TOKEN.into()));
            let response = mock_router_for(state, surface)
                .oneshot(
                    Request::post(*path)
                        .header("content-type", "application/json")
                        .header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{surface:?} surface must not serve {path}"
            );
        }
    }

    // And the control: the SAME request on the operator surface is served,
    // so the 404s above are the route set changing and not a broken probe.
    let response = mock_router_for(test_app_state(), ClientSurface::Operator)
        .oneshot(
            Request::get("/internal/guest/grant/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the operator surface must still serve what the others refuse"
    );
}

/// The `Rail` surface serves the ring-app rail and NOTHING else.
///
/// A deployed ring app is a guest that happens to run on this machine.
/// It must not be able to drive inference, search the operator's
/// corpora, or manage apps — and the guarantee is that those routes are
/// absent from the listener it can reach, not that a predicate refuses
/// them (§7.1). A 404 here is the route set, not a credential.
///
/// Driven with a credential the surface ACCEPTS, so the only variable
/// left is whether the route is mounted; the control at the end proves
/// the probe itself is not simply broken (§18.1).
#[tokio::test]
async fn the_rail_surface_does_not_serve_the_general_client_routes() {
    const GENERAL: &[(&str, &str)] = &[
        ("POST", "/v1/chat/completions"),
        ("POST", "/v1/knowledge/search"),
        ("GET", "/v1/models"),
        ("GET", "/v1/apps"),
        ("POST", "/api/chat"),
    ];
    const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

    let probe = |surface: ClientSurface, method: &str, path: &str| {
        let state = test_app_state_with_token(Some(TOKEN.into()));
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from("{}"))
            .unwrap();
        async move { mock_router_for(state, surface).oneshot(req).await.unwrap() }
    };

    for (method, path) in GENERAL {
        let response = probe(ClientSurface::Rail, method, path).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "rail surface must not serve {method} {path}"
        );
    }

    // Control: the same requests on the operator surface are routed —
    // so the 404s above are the route set changing, not a probe that
    // 404s everything. Any status but NOT_FOUND proves the route exists;
    // these handlers legitimately 4xx/5xx on an empty body.
    for (method, path) in GENERAL {
        let response = probe(ClientSurface::Operator, method, path).await;
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "operator surface must still serve {method} {path}"
        );
    }
}

#[tokio::test]
async fn models_endpoint_with_registered_model() {
    let state = test_app_state();

    // Register a model.
    use commonwealth_core::model::{ModelArchitecture, ModelInfo};
    use oicp_types::{Capability, CapabilityProfile};
    use std::collections::HashMap;

    let mut caps = CapabilityProfile::default();
    caps.insert(Capability::Code, 4);

    let model = ModelInfo {
        id: commonwealth_core::ModelId::from_u128(1),
        name: "test-coder".into(),
        repo: "test/model".into(),
        file: "model.gguf".into(),
        size_bytes: 17_000_000_000,
        total_layers: 64,
        architecture: ModelArchitecture::Qwen,
        available_on: HashMap::new(),
        oicp_capabilities: caps,
        quantization: "Q4_K_M".into(),
        // Fields added after the adaptive-mesh-scheduler change —
        // all have `#[serde(default)]` on the struct, so
        // defaults here are fine. Keeping them explicit documents
        // the shape the test expects.
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    };
    state.register_model(model);

    let app = mock_router(state);
    let response = app
        .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["data"].as_array().unwrap().len(), 1);
    assert_eq!(json["data"][0]["id"], "test-coder");
}
