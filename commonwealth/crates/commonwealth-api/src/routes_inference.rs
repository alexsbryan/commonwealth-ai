use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use tracing::warn;

use commonwealth_core::oicp::ShardingPrivacy;

use crate::openai_types::*;
use crate::state::AppState;

/// POST /v1/chat/completions — OpenAI-compatible chat completions.
pub async fn chat_completions(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    // Privacy enforcement: reject local_only requests.
    if let Some(ref oicp) = request.oicp {
        if let Some(ref privacy) = oicp.privacy {
            if privacy.sharding == ShardingPrivacy::LocalOnly {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(
                        serde_json::to_value(ErrorResponse::new(
                            "Requests with privacy 'local_only' must be handled by the client's \
                         local inference engine, not sent to Commonwealth. This is likely a \
                         client misconfiguration.",
                            "invalid_request_error",
                        ))
                        .unwrap(),
                    ),
                )
                    .into_response();
            }
        }
    }

    // Determine which model to route to.
    let model_id = if let Some(ref oicp) = request.oicp {
        // OICP-aware model selection: find best matching loaded model.
        let models = state.inner.models.read().await;
        let plan = state.inner.inference_plan.read().await;

        let mut best_model = None;
        let mut best_score = -1.0f32;

        for shard_plan in &plan.model_plans {
            if let Some(model_info) = models.get(&shard_plan.model) {
                if model_info
                    .oicp_capabilities
                    .satisfies(&oicp.capabilities.required)
                {
                    let score = model_info
                        .oicp_capabilities
                        .score_against(&oicp.capabilities.preferred);
                    if score > best_score {
                        best_score = score;
                        best_model = Some(shard_plan.model);
                    }
                }
            }
        }

        match best_model {
            Some(id) => id,
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        serde_json::to_value(ErrorResponse::new(
                            "No loaded model satisfies the OICP requirements",
                            "model_not_available",
                        ))
                        .unwrap(),
                    ),
                )
                    .into_response();
            }
        }
    } else {
        // No OICP: use default model.
        match state.default_model_id().await {
            Some(id) => id,
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        serde_json::to_value(ErrorResponse::new(
                            "No models are currently loaded on the mesh",
                            "model_not_available",
                        ))
                        .unwrap(),
                    ),
                )
                    .into_response();
            }
        }
    };

    // Get the llama-server address for this model.
    let llama_addr = match state.get_llama_server_address(model_id).await {
        Some(addr) => addr,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        "Model is scheduled but llama-server is not yet ready",
                        "model_not_ready",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };

    // Forward the request to llama-server.
    // In production, this would use hyper to proxy the request (including streaming).
    // For now, we use a TCP connection + raw HTTP forwarding.
    let forward_body = serde_json::to_string(&request).unwrap_or_default();
    match forward_to_llama_server(&llama_addr, &forward_body).await {
        Ok(response_body) => {
            // Parse and return the response.
            match serde_json::from_str::<serde_json::Value>(&response_body) {
                Ok(value) => (StatusCode::OK, Json(value)).into_response(),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("retry-after", "10")],
                    Json(
                        serde_json::to_value(ErrorResponse::new(
                            "Invalid response from inference backend",
                            "backend_error",
                        ))
                        .unwrap(),
                    ),
                )
                    .into_response(),
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to forward to llama-server");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [("retry-after", "10")],
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        format!(
                            "Inference backend unavailable: {e}. \
                             The mesh is recovering — retry shortly."
                        ),
                        "backend_unavailable",
                    ))
                    .unwrap(),
                ),
            )
                .into_response()
        }
    }
}

/// Forward a request to a llama-server instance.
async fn forward_to_llama_server(
    address: &str,
    body: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let stream = tokio::net::TcpStream::connect(address).await?;

    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: {address}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    stream.writable().await?;
    stream.try_write(request.as_bytes())?;

    // Read response.
    let mut response = Vec::new();
    loop {
        stream.readable().await?;
        let mut buf = [0u8; 4096];
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }

    let response_str = String::from_utf8_lossy(&response);

    // Extract body after HTTP headers.
    if let Some(body_start) = response_str.find("\r\n\r\n") {
        Ok(response_str[body_start + 4..].to_string())
    } else {
        Ok(response_str.to_string())
    }
}

/// GET /v1/models — list available models.
pub async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let models = state.inner.models.read().await;
    let plan = state.inner.inference_plan.read().await;
    let addresses = state.inner.llama_server_addresses.read().await;

    let data: Vec<ModelObject> = models
        .values()
        .map(|model| {
            let shard_plan = plan.model_plans.iter().find(|p| p.model == model.id);
            let loaded = addresses.contains_key(&model.id);

            ModelObject {
                id: model.name.clone(),
                object: "model".into(),
                created: 0,
                owned_by: "mesh".into(),
                capabilities: Some(serde_json::to_value(&model.oicp_capabilities).unwrap()),
                performance: shard_plan.map(|p| ModelPerformance {
                    estimated_tokens_per_sec: p.estimated_tokens_per_sec,
                    estimated_ttft_ms: p.estimated_ttft_ms,
                    loaded,
                }),
            }
        })
        .collect();

    Json(ModelListResponse {
        object: "list".into(),
        data,
    })
}
