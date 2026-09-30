// SPDX-License-Identifier: AGPL-3.0-or-later

use axum::routing::post;

use super::*;

/// A serve double whose `/internal/rpc-warm` echoes the model it was asked
/// to warm, as the loader's warmer answers with its stats.
async fn serve_double() -> String {
    let app = axum::Router::new().route(
        "/internal/rpc-warm",
        post(|Json(body): Json<Value>| async move {
            Json(json!({ "warmed": body["model_id"], "tensors_written": 3 }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

/// A dialed serve answers the warm this node's peers still send here, and
/// its answer is relayed as serve gave it (pb-serve-distributes-standalone).
#[tokio::test]
async fn a_warm_with_no_local_warmer_is_answered_by_serve() {
    let base = serve_double().await;
    let resp = forward_to_serve(&base, &json!({ "model_id": "m.gguf" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(body["warmed"], "m.gguf");
}

/// A serve that does not answer is a 503 naming it, never a success.
#[tokio::test]
async fn an_unreachable_serve_is_a_named_refusal() {
    let resp = forward_to_serve("http://127.0.0.1:9", &json!({ "model_id": "m.gguf" })).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .expect("body");
    assert!(
        String::from_utf8_lossy(&body).contains("127.0.0.1:9"),
        "{body:?}"
    );
}
