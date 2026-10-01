// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's posture: which of its own surfaces a distribution serves
//! (FIVE_PROGRAMS §2c; phase-b-86, -87). One value, handed down through
//! `process::run`, decides web reach, the wikipedia bundle and the `/mcp`
//! route together. What a withheld surface loses is its registration and its
//! route, never its type: the tool ids stay linked as routing data.

/// Which of svrn's own surfaces this process serves. **CLOSED SET**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posture {
    /// Every surface: web reach, the wikipedia bundle, `/mcp`. The stock
    /// install's and svrn alone's.
    Open,
    /// None of the three, each named: a turn records its bundle as
    /// `Withheld`, `/mcp` answers [`NO_MCP`] with a 503. The on-prem
    /// distribution's (phase-b-86).
    Sealed,
}

/// Why a sealed posture withholds web reach and the wikipedia bundle.
pub const SEALED: &str = "this distribution withholds svrn's open-web surfaces \
                          (the on-prem posture, phase-b-86)";

/// What `/mcp`, `/mcp/message` and `/mcp/stats` answer under a sealed posture.
pub const NO_MCP: &str = "this distribution does not serve MCP";

impl Posture {
    /// `None` when this posture serves svrn's own surfaces; the reason it
    /// withholds them otherwise.
    pub fn withheld(self) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::Sealed => Some(SEALED),
        }
    }
}

/// `/mcp` and its two siblings under a sealed posture: a 503 naming the
/// absence, never a 404 that reads as "no such route" (FIVE_PROGRAMS §4 rule 3).
pub fn mcp_withheld_router() -> axum::Router {
    axum::Router::new()
        .route("/mcp", axum::routing::any(mcp_withheld))
        .route("/mcp/message", axum::routing::any(mcp_withheld))
        .route("/mcp/stats", axum::routing::any(mcp_withheld))
}

async fn mcp_withheld(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    tracing::debug!(path = %uri.path(), "mcp: withheld by this distribution's posture");
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({ "error": NO_MCP })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    /// Each of the three routes answers a 503 whose body names the absence.
    /// Failing input: drop a route and it answers 404.
    #[tokio::test]
    async fn every_mcp_route_answers_a_named_503() {
        for path in ["/mcp", "/mcp/message", "/mcp/stats"] {
            let resp = mcp_withheld_router()
                .oneshot(
                    axum::http::Request::post(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 503, "{path}");
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            assert!(
                String::from_utf8_lossy(&body).contains(NO_MCP),
                "{path}: {body:?}"
            );
        }
    }
}
