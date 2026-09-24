// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /internal/guest/route` — the base URL a request under this node's
//! stored guest link must be sent to.
//!
//! The HOLDER's side of a guest link, where `/internal/guest/grant` is the
//! lender's. The daemon owns the mesh tunnel a link's dial string names
//! (`StoredGuestLink`, the one decider that turns a link into an address), so
//! a CLI dials this door rather than opening a tunnel of its own (§12 D6).
//! Operator-only, like the grant routes beside it.
//!
//! Absence is answered, never substituted (§18.3): no stored link is 412, a
//! tunnel that will not open is 502, and neither falls back to a local base.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovereign_mesh::guest_lender::{GuestRouteAbsence, StoredGuestLink};

use crate::http_response::json_error;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct GuestRouteResponse {
    /// Base URL of the lender's client API — the tunnel's loopback bridge
    /// when the link names an iroh endpoint. No trailing `/v1`.
    pub base_url: String,
}

/// GET /internal/guest/route
pub async fn guest_route(State(state): State<AppState>) -> Response {
    answer(&state.inner.node.guest_route).await
}

async fn answer(link: &StoredGuestLink) -> Response {
    match link.route().await {
        Ok(base_url) => {
            tracing::debug!(%base_url, "guest_route: served the stored link's base URL");
            Json(GuestRouteResponse { base_url }).into_response()
        }
        Err(absence) => {
            let status = match absence {
                GuestRouteAbsence::NoLink => StatusCode::PRECONDITION_FAILED,
                GuestRouteAbsence::TunnelRefused { .. } => StatusCode::BAD_GATEWAY,
            };
            tracing::info!(%status, %absence, "guest_route: no route to report");
            json_error(status, absence.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::to_bytes;
    use sovereign_mesh::guest_lender::{
        GuestLinkReader, GuestTunnelHandle, GuestTunnelOpener, LiveGuestLink, NoGuestLinks,
        NoTunnels,
    };

    use super::*;

    #[derive(Debug)]
    struct OneLink(LiveGuestLink);

    impl GuestLinkReader for OneLink {
        fn live_link(&self) -> Option<LiveGuestLink> {
            Some(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct Bridge;

    impl GuestTunnelHandle for Bridge {
        fn base_url(&self) -> &str {
            "https://bridge.example"
        }
    }

    #[derive(Debug)]
    struct OpensBridge;

    #[async_trait]
    impl GuestTunnelOpener for OpensBridge {
        async fn open(
            &self,
            _dial: &str,
            _relay_urls: Vec<String>,
            _discovery: Option<String>,
        ) -> Result<Arc<dyn GuestTunnelHandle>, String> {
            Ok(Arc::new(Bridge))
        }
    }

    fn link(dial: Option<&str>) -> OneLink {
        OneLink(LiveGuestLink {
            token: "t".into(),
            url: "http://lender:9741".into(),
            dial: dial.map(str::to_string),
        })
    }

    async fn drive(
        links: impl GuestLinkReader + 'static,
        opener: impl GuestTunnelOpener + 'static,
    ) -> (StatusCode, serde_json::Value) {
        let src = StoredGuestLink::new(Arc::new(links), Arc::new(opener));
        let response = answer(&src).await;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn a_dial_link_answers_with_the_tunnels_base_url() {
        let (status, body) = drive(link(Some("node1abc")), OpensBridge).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["base_url"], "https://bridge.example");
    }

    #[tokio::test]
    async fn a_plaintext_link_answers_with_its_own_url() {
        let (status, body) = drive(link(None), NoTunnels).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["base_url"], "http://lender:9741");
    }

    #[tokio::test]
    async fn no_stored_link_is_a_named_absence() {
        let (status, body) = drive(NoGuestLinks, OpensBridge).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("no live guest link"));
    }

    #[tokio::test]
    async fn a_tunnel_that_will_not_open_is_a_named_absence_not_a_fallback() {
        let (status, body) = drive(link(Some("node1abc")), NoTunnels).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("could not reach http://lender:9741 over the mesh tunnel"));
        assert!(error.contains("no plaintext fallback"));
    }
}
