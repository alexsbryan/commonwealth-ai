// SPDX-License-Identifier: AGPL-3.0-or-later
//! The routes of served model kinds (`/v1/rerank`, `/v1/ner`), forwarded to
//! serve's kind mount (pb-serve-distributes): the kinds, their registry and
//! their loads are serve's, and svrn keeps the address clients already dial.
//! The paths are the ones the shared contract names
//! (`sovereign_contracts::served_kinds`), which the registry is held equal to.

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, MethodRouter};
use axum::Json;
use sovereign_contracts::oicp::openai_types::ErrorResponse;
use sovereign_contracts::served_kinds::SERVED_KIND_PATHS;

use crate::serve_client::ServingPath;
use crate::state::AppState;

/// `(path, handler)` for every served kind's route.
pub fn served_kind_routes() -> Vec<(&'static str, MethodRouter<AppState>)> {
    SERVED_KIND_PATHS
        .iter()
        .map(|&path| (path, post(move |body: Bytes| forward_kind(path, body))))
        .collect()
}

/// Relay the request to serve and its answer back. Where there is no serve
/// to reach — a terminal, or a process no boot decided — the absence is a
/// named 503 in OpenAI's error shape, never an answer made up here.
async fn forward_kind(path: &'static str, body: Bytes) -> Response {
    forward_kind_to(ServingPath::decided_serve(), path, body).await
}

async fn forward_kind_to(
    target: Option<Option<&crate::serve_client::ServeBase>>,
    path: &'static str,
    body: Bytes,
) -> Response {
    let base = match target {
        Some(Some(serve)) => serve.base.as_str(),
        Some(None) => {
            return refuse(format!(
                "this node is a terminal and serves no model kinds; `{path}` is its entry \
                 node's"
            ))
        }
        None => {
            return refuse(format!(
                "no serving path was decided in this process, so `{path}` has no serve to \
                 reach"
            ))
        }
    };
    match crate::serve_client::forward(base, Method::POST, path, Some(body.to_vec())).await {
        Ok((status, body)) => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(why) => refuse(why),
    }
}

fn refuse(message: String) -> Response {
    tracing::warn!(target: "served_kind", error = %message, "served kind request not forwarded");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            serde_json::to_value(ErrorResponse::new(message, "no_local_inference_backend"))
                .unwrap_or_default(),
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve_client::{ServeBase, ServeBaseSource};

    /// A stub serve answering every kind path with what reached it.
    async fn stub_serve() -> String {
        let echo = |uri: axum::http::Uri, body: Bytes| async move {
            (
                StatusCode::IM_A_TEAPOT,
                format!("{} {}", uri.path(), String::from_utf8_lossy(&body)),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().fallback(echo))
                .await
                .ok()
        });
        base
    }

    async fn body_text(resp: Response) -> (StatusCode, String) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Each kind path reaches serve with its body, and serve's answer comes
    /// back as sent. Failing input: answer a kind in process again, and its
    /// row reads that answer instead of the teapot.
    #[tokio::test]
    async fn a_kind_request_reaches_serve_and_relays_its_answer() {
        let serve = ServeBase {
            base: stub_serve().await,
            source: ServeBaseSource::Default,
        };
        for &path in SERVED_KIND_PATHS {
            let resp = forward_kind_to(Some(Some(&serve)), path, Bytes::from_static(b"{}")).await;
            let (status, text) = body_text(resp).await;
            assert_eq!(status, StatusCode::IM_A_TEAPOT, "{path}");
            assert_eq!(text, format!("{path} {{}}"));
        }
    }

    /// A terminal, and a process no boot decided, each name their absence.
    #[tokio::test]
    async fn no_serve_to_reach_is_a_named_503() {
        let (status, text) =
            body_text(forward_kind_to(Some(None), "/v1/rerank", Bytes::new()).await).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(text.contains("terminal"), "{text}");
        let (status, text) =
            body_text(forward_kind_to(None, "/v1/rerank", Bytes::new()).await).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(text.contains("no serving path was decided"), "{text}");
    }
}
