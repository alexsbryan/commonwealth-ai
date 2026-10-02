// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's posture: which of its own surfaces a distribution serves
//! (FIVE_PROGRAMS §2c; phase-b-86, -87). One value, handed down through
//! `process::run`, decides web reach, the wikipedia bundle and the `/mcp`
//! route together. What a withheld surface loses is its registration and its
//! route, never its type: the tool ids stay linked as routing data.

/// Which of svrn's own surfaces this process serves. **CLOSED SET**.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Posture {
    /// Every surface: web reach, the wikipedia bundle, `/mcp`. The stock
    /// install's and svrn alone's; the default a test's node seed takes.
    #[default]
    Open,
    /// None of the three, each named: a turn records its bundle as
    /// `Withheld`, `/mcp` answers [`NO_MCP`] with a 503. The on-prem
    /// distribution's (phase-b-86).
    Sealed,
}

/// Why a sealed posture withholds web reach and the wikipedia bundle.
pub const SEALED: &str = "this distribution withholds svrn's open-web surfaces \
                          (the on-prem posture, phase-b-86)";

/// What a sealed box CAN do, the half of every sealed absence that points
/// somewhere (pc-onprem-followups): a pointer to a program this distribution
/// does not ship is no pointer. A macro so each message is one literal.
macro_rules! sealed_serves {
    () => {
        "it answers questions over its documents at /v1/conversations \
         (GET /v1/tools lists what a turn can use)"
    };
}

/// What `/mcp`, `/mcp/message` and `/mcp/stats` answer under a sealed posture.
pub const NO_MCP: &str = concat!("this distribution does not serve MCP; ", sealed_serves!());

impl Posture {
    /// `None` when this posture serves svrn's own surfaces; the reason it
    /// withholds them otherwise.
    pub fn withheld(self) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::Sealed => Some(SEALED),
        }
    }

    /// Where a request for the code program (its routes, its notes) is
    /// pointed when this process hosts none: `svrn code mcp` on an open
    /// install, and what the box does instead on a sealed one, which ships
    /// no code program at all.
    pub fn code_pointer(self) -> &'static str {
        match self {
            // `hosted_code::CODE_SERVER`, pinned by the test below.
            Self::Open => "run `svrn code mcp`",
            Self::Sealed => concat!(
                "this distribution ships no code program; ",
                sealed_serves!()
            ),
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

    /// Every sealed absence names what the box can do and never points at a
    /// program the distribution does not ship; an open one keeps
    /// `svrn code mcp`. Failing input: route the sealed code routes through
    /// the open pointer and the 503 says `svrn code mcp`.
    #[tokio::test]
    async fn a_sealed_absence_names_what_the_box_can_do() {
        assert!(Posture::Open
            .code_pointer()
            .contains(crate::hosted_code::CODE_SERVER));
        for text in [NO_MCP, Posture::Sealed.code_pointer()] {
            assert!(text.contains("/v1/conversations"), "{text}");
            assert!(!text.contains("svrn code"), "{text}");
        }
        for (posture, wants, refuses) in [
            (Posture::Sealed, "/v1/conversations", "svrn code"),
            (Posture::Open, "svrn code mcp", "/v1/conversations"),
        ] {
            let resp = crate::hosted_code::projects_absent_router(posture)
                .merge(crate::hosted_code::solve_absent_router(posture))
                .merge(crate::hosted_code::edit_door_absent_router(posture))
                .oneshot(
                    axum::http::Request::post("/v1/solve/jobs")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 503);
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body = String::from_utf8_lossy(&body);
            assert!(body.contains(wants), "{posture:?}: {body}");
            assert!(!body.contains(refuses), "{posture:?}: {body}");
        }
    }
}
