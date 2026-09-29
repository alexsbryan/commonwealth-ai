// SPDX-License-Identifier: AGPL-3.0-or-later
//! Peer-to-peer model file distribution on the internal port, forwarded to
//! serve (pb-serve-distributes): model transfer is serve's (phase-b-19), and
//! serve answers both routes over the servable list it publishes
//! (`sovereign_compute::model_transfer`). Peers keep dialing this node's
//! internal port, behind its mesh gate, until the flip registers the class
//! with cw-rails. The file route streams (a GGUF can be tens of GB) and
//! relays the byte range and the integrity digest.

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::serve_client::ServingPath;

pub use oicp_types::model_transfer::{ModelFileInfo, ModelFileListing};

/// `GET /internal/v1/models/list`, answered by serve.
pub async fn list_model_files() -> Response {
    list_from(serve_base()).await
}

async fn list_from(base: Result<String, Response>) -> Response {
    let base = match base {
        Ok(base) => base,
        Err(refusal) => return refusal,
    };
    let path = oicp_types::model_transfer::MODELS_LIST_PATH;
    match crate::serve_client::forward(&base, axum::http::Method::GET, path, None).await {
        Ok((status, body)) => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(why) => refuse(why),
    }
}

/// `GET /internal/v1/models/file/{name}`, whole or by byte range, streamed
/// from serve.
pub async fn serve_model_file(Path(name): Path<String>, headers: HeaderMap) -> Response {
    file_from(serve_base(), &name, &headers).await
}

async fn file_from(base: Result<String, Response>, name: &str, headers: &HeaderMap) -> Response {
    let base = match base {
        Ok(base) => base,
        Err(refusal) => return refusal,
    };
    let path = oicp_types::model_transfer::model_file_url("", name);
    match crate::serve_client::forward_stream(&base, &path, headers).await {
        Ok(resp) => resp,
        Err(why) => refuse(why),
    }
}

/// Where serve is, as decided at boot; each absence named apart.
fn serve_base() -> Result<String, Response> {
    match ServingPath::decided_serve() {
        Some(Some(serve)) => Ok(serve.base.clone()),
        Some(None) => Err(refuse(
            "this node is a terminal and holds no model files; its entry node does".to_string(),
        )),
        None => Err(refuse(
            "no serving path was decided in this process, so model files have no serve to come \
             from"
                .to_string(),
        )),
    }
}

fn refuse(error: String) -> Response {
    tracing::warn!(target: "serving_path", error = %error, "model transfer not forwarded to serve");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": error })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub serve answering both routes the way model transfer does: the
    /// listing as JSON, and a byte range as a 206 with its headers.
    async fn stub_serve() -> String {
        use axum::routing::get;
        let file = |Path(name): Path<String>, headers: HeaderMap| async move {
            let range = headers
                .get(axum::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none")
                .to_string();
            (
                StatusCode::PARTIAL_CONTENT,
                [
                    ("content-range", "bytes 0-3/10".to_string()),
                    ("x-sha256", "feed".to_string()),
                ],
                format!("{name}|{range}"),
            )
        };
        let app = axum::Router::new()
            .route(
                oicp_types::model_transfer::MODELS_LIST_PATH,
                get(|| async { Json(serde_json::json!({ "files": [] })) }),
            )
            .route(oicp_types::model_transfer::MODEL_FILE_ROUTE, get(file));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        base
    }

    async fn text(resp: Response) -> (StatusCode, HeaderMap, String) {
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        (
            parts.status,
            parts.headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    /// Both routes reach serve: the listing relayed, the file streamed with
    /// the range the peer asked for and the headers it verifies against.
    /// Failing input: drop the range pass-through in `forward_stream`, and the
    /// body reads `m.gguf|none`.
    #[tokio::test]
    async fn model_transfer_reaches_serve_with_the_range_and_its_headers() {
        let base = stub_serve().await;
        let (status, _, body) = text(list_from(Ok(base.clone())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"files":[]}"#);
        let mut asked = HeaderMap::new();
        asked.insert(axum::http::header::RANGE, "bytes=0-3".parse().unwrap());
        let (status, headers, body) = text(file_from(Ok(base), "m.gguf", &asked).await).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(body, "m.gguf|bytes=0-3");
        assert_eq!(headers["content-range"], "bytes 0-3/10");
        assert_eq!(headers["x-sha256"], "feed");
    }
}
