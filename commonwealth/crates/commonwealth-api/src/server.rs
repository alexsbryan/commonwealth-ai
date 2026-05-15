use std::net::SocketAddr;

use axum::routing::{any, delete, get, post};
use axum::Router;
use tokio::net::TcpListener;
use tracing::info;

use crate::routes_app_internal;
use crate::routes_apps;
use crate::routes_inference;
use crate::routes_internal;
use crate::routes_knowledge;
use crate::routes_oicp;
use crate::routes_responses;
use crate::routes_status;
use crate::state::AppState;

/// Build the client-facing API router (port 9741).
pub fn client_router(state: AppState) -> Router {
    // Per-route admission gate applied to peer-reachable inference
    // endpoints. Local requests (no `X-Node-Id`) pass through; peer
    // requests are checked against pause / foreground-yield / ceiling
    // and 503 with structured body + Retry-After when gated. See
    // `crate::admission`.
    let admission = || {
        axum::middleware::from_fn_with_state(
            state.clone(),
            crate::admission::peer_admission_layer,
        )
    };

    Router::new()
        // OpenAI-compatible inference endpoints.
        .route(
            "/v1/chat/completions",
            post(routes_inference::chat_completions).layer(admission()),
        )
        // OpenAI Responses API — adapter over /v1/chat/completions.
        // Required by `codex` and the OpenAI agents libraries since
        // their dropping `wire_api="chat"` (2026-05). See
        // `routes_responses` module docs for the translation contract.
        .route("/v1/responses", post(routes_responses::responses))
        .route("/v1/embeddings", post(routes_inference::embeddings))
        .route("/v1/models", get(routes_inference::list_models))
        // Knowledge search endpoint.
        .route(
            "/v1/knowledge/search",
            post(routes_knowledge::knowledge_search),
        )
        // Status endpoint.
        .route("/status", get(routes_status::status))
        // OICP capability manifest.
        .route("/oicp/v1/capabilities", get(routes_oicp::capabilities))
        // App management endpoints.
        .route("/v1/apps", get(routes_apps::list_apps))
        .route("/v1/apps/{app_id}/install", post(routes_apps::install_app))
        .route("/v1/apps/{app_id}/status", get(routes_apps::app_status))
        .route("/v1/apps/{app_id}", delete(routes_apps::uninstall_app))
        // Reverse proxy to locally running apps.
        .route("/app/{app_id}/{*path}", any(routes_apps::proxy_app))
        .with_state(state)
}

/// Build the internal mesh API router (port 9742).
pub fn internal_router(state: AppState) -> Router {
    // Same admission gate as the client router — applied to peer-
    // fan-out routes so a busy operator's machine 503s knowledge
    // searches from peers rather than starving local chat. See
    // `crate::admission`.
    let admission = || {
        axum::middleware::from_fn_with_state(
            state.clone(),
            crate::admission::peer_admission_layer,
        )
    };

    Router::new()
        .route("/internal/gossip", post(routes_internal::gossip))
        .route("/internal/join", post(routes_internal::join))
        .route(
            "/internal/scheduling/intent",
            post(routes_internal::scheduling_intent),
        )
        .route(
            "/internal/scheduling/plan",
            post(routes_internal::scheduling_plan),
        )
        .route(
            "/internal/model/transfer",
            post(routes_internal::model_transfer),
        )
        // Peer-to-peer GGUF distribution. See routes_internal::model_files.
        .route(
            "/internal/v1/models/list",
            get(routes_internal::list_model_files),
        )
        .route(
            "/internal/v1/models/file/{name}",
            get(routes_internal::serve_model_file),
        )
        .route(
            "/internal/index/transfer",
            post(routes_internal::index_transfer),
        )
        .route(
            "/internal/index/serve",
            get(routes_internal::index_serve),
        )
        .route(
            "/internal/knowledge/search",
            post(routes_internal::knowledge_search).layer(admission()),
        )
        .route(
            "/internal/atlas/status",
            get(routes_internal::atlas_status),
        )
        .route(
            "/internal/latency/probe",
            get(routes_internal::latency_probe),
        )
        .route(
            "/internal/corpus/collaborate",
            post(routes_internal::corpus_collaborate),
        )
        .route(
            "/internal/corpus/ingest_partition",
            post(routes_internal::corpus_ingest_partition),
        )
        // Pull-based work queue (new path; coexists with ingest_partition
        // while `SOVEREIGN_USE_WORK_QUEUE` gates the coordinator).
        .route(
            "/internal/corpus/next_unit",
            post(routes_internal::corpus_next_unit),
        )
        .route(
            "/internal/corpus/heartbeat",
            post(routes_internal::corpus_heartbeat),
        )
        .route(
            "/internal/corpus/complete_unit",
            post(routes_internal::corpus_complete_unit),
        )
        .route(
            "/internal/corpus/cancel",
            post(routes_internal::corpus_cancel),
        )
        .route(
            "/internal/corpus/pause",
            post(routes_internal::corpus_pause),
        )
        .route(
            // Mesh-aware pause of `sovereign pipeline run` drivers.
            // Local CLI sets `fanout: true`; the receiving daemon
            // forwards to each online peer with `fanout: false` so
            // peers run only their own /proc walk and the message
            // can't loop. See routes_internal/pipeline_pause.rs.
            "/internal/pipeline/pause",
            post(routes_internal::pipeline_pause),
        )
        .route(
            "/internal/corpus/install",
            post(routes_internal::corpus_install),
        )
        .route(
            "/internal/corpus/expand",
            post(routes_internal::corpus_expand),
        )
        .route(
            "/internal/corpus/progress",
            get(routes_internal::corpus_progress),
        )
        .route(
            "/internal/corpus/status",
            get(routes_internal::corpus_status),
        )
        // Phase 6 canonical-sync: peers fetch this node's canonical
        // index for `<corpus_id>` as a streaming tar.zst. Loopback-
        // gated like the other internal routes; the auth path is the
        // same one peers already use for `/internal/knowledge/search`
        // and friends.
        .route(
            "/internal/corpus/canonical/{corpus_id}",
            get(routes_internal::corpus_canonical_stream),
        )
        .route(
            "/internal/node/activity",
            post(routes_internal::node_activity),
        )
        // App gossip endpoints.
        .route("/internal/app/state", post(routes_app_internal::recv_app_state))
        .route("/internal/app/registry", post(routes_app_internal::recv_app_registry))
        // Runtime slot management — load/unload extras chat slots
        // without daemon restart. Complements the static
        // `[models.extra]` config table (loaded at startup) by
        // letting operators swap models mid-session.
        .route("/internal/models/load", post(routes_internal::models_load))
        .route("/internal/models/unload", post(routes_internal::models_unload))
        .route("/internal/models/inventory", get(routes_internal::models_inventory))
        // Eagerly warm the primary chat slot. Desktop fires this on
        // window-focus / chat-mount so the first turn after a
        // resume doesn't pay the 10–90s lazy-load tax.
        .route(
            "/internal/inference/warmup",
            post(routes_internal::inference_warmup),
        )
        // Contribution controls (W2). Read by the Settings panel and
        // the tray status chip; mutated by the pause/ceiling controls.
        // Loopback-only — the same guard that protects /internal/*.
        .route(
            "/internal/contribution/status",
            get(routes_internal::contribution_status),
        )
        .route(
            "/internal/contribution/ceiling",
            post(routes_internal::contribution_ceiling_set),
        )
        .route(
            "/internal/contribution/pause",
            post(routes_internal::contribution_pause),
        )
        .route(
            "/internal/contribution/resume",
            post(routes_internal::contribution_resume),
        )
        .route(
            "/internal/contribution/recent",
            get(routes_internal::contribution_recent),
        )
        // Foreground-yield introspection — read-only snapshot of the
        // atomics that decide whether ingest workers are pausing for
        // chat. No POST: the window is configured at startup via
        // `daemon.yield_to_foreground_secs`, not pushed at runtime.
        .route(
            "/internal/daemon/foreground_state",
            get(routes_internal::foreground_state),
        )
        // Mesh-quiesce control. GET reports current state; POST flips
        // it at runtime so an operator can stop participating in
        // shared ingests on this node without a daemon restart. Used
        // by the desktop's "Stop participating" affordance and the
        // peer-pause workflow when foreground inference is being
        // crushed by background ingest activity from another node.
        .route(
            "/internal/mesh/quiesce",
            get(routes_internal::mesh_quiesce_get).post(routes_internal::mesh_quiesce_set),
        )
        // Per-batch ingest throttle. GET reports the current factor;
        // POST sets it. `1.0` = full speed (default), `0.5` =
        // duty-cycle 50% (sleep after each embed batch equal to its
        // wall time). Use the pause route to fully stop a corpus —
        // `0.0` is rejected here.
        .route(
            "/internal/ingest/budget",
            get(routes_internal::ingest_budget_get).post(routes_internal::ingest_budget_set),
        )
        // Storage budget. GET reports the current ceiling, observed
        // usage, raw free disk, and a recommended baseline; POST
        // accepts `{ "budget_bytes": <≥1 GiB | null> }`. The
        // enforcement point is the gossip-tick capabilities builder
        // (`sovereign-mesh::capabilities::build_local_capabilities`)
        // which clamps the published `free_storage_gb` to budget
        // remaining — every existing scheduler picks up the cap
        // automatically.
        .route(
            "/internal/storage/budget",
            get(routes_internal::storage_budget_get).post(routes_internal::storage_budget_set),
        )
        .with_state(state)
}

/// Start both API servers. Returns when both are shut down.
pub async fn serve(
    state: AppState,
    client_addr: SocketAddr,
    internal_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client_app = client_router(state.clone());
    let internal_app = internal_router(state);

    let client_listener = TcpListener::bind(client_addr).await?;
    let internal_listener = TcpListener::bind(internal_addr).await?;

    info!(
        client = %client_addr,
        internal = %internal_addr,
        "API servers starting"
    );

    tokio::select! {
        result = axum::serve(client_listener, client_app) => {
            result?;
        }
        result = axum::serve(internal_listener, internal_app) => {
            result?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_app_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn status_endpoint() {
        let app = client_router(test_app_state());

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
        let app = client_router(test_app_state());

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
        let app = client_router(test_app_state());

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
        assert_eq!(
            json["oicp_version"],
            commonwealth_inference::oicp::OICP_VERSION
        );
        assert_eq!(json["provider"]["type"], "mesh");
    }

    #[tokio::test]
    async fn chat_completions_no_model_loaded() {
        let app = client_router(test_app_state());

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
        let app = client_router(test_app_state());
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
        let app = client_router(test_app_state());
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
        let app = client_router(test_app_state());
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
        let app = client_router(test_app_state());

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
        // an all-zero join_key_hash; posting a body with a different
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
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn models_endpoint_with_registered_model() {
        let state = test_app_state();

        // Register a model.
        use commonwealth_inference::model::{ModelArchitecture, ModelInfo};
        use commonwealth_inference::oicp::{Capability, CapabilityProfile};
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

        let app = client_router(state);
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
}
