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
use crate::routes_status;
use crate::state::AppState;

/// Build the client-facing API router (port 9741).
pub fn client_router(state: AppState) -> Router {
    Router::new()
        // OpenAI-compatible inference endpoints.
        .route(
            "/v1/chat/completions",
            post(routes_inference::chat_completions),
        )
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
        .route(
            "/internal/index/transfer",
            post(routes_internal::index_transfer),
        )
        .route(
            "/internal/knowledge/search",
            post(routes_internal::knowledge_search),
        )
        .route(
            "/internal/latency/probe",
            get(routes_internal::latency_probe),
        )
        // App gossip endpoints.
        .route("/internal/app/state", post(routes_app_internal::recv_app_state))
        .route("/internal/app/registry", post(routes_app_internal::recv_app_registry))
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
        assert_eq!(json["oicp_version"], "0.2.0");
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
    async fn internal_gossip_endpoint() {
        let app = internal_router(test_app_state());

        let body = serde_json::json!({"test": true});
        let response = app
            .oneshot(
                Request::post("/internal/gossip")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
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
