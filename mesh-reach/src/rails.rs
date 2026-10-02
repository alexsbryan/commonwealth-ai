// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RailsTransport`: [`PeerTransport::endpoints`] over cw-rails' reach door.
//!
//! A program that is not the node's mesh endpoint (svrn, serve) does not dial
//! peers itself. It asks cw-rails, which holds the key, the roster and the
//! per-peer bridges, which endpoint to try. The answer is cached per peer and
//! class, since a bridge lives as long as cw-rails does, and dropped on
//! [`PeerTransport::note_failure`] so the next dial asks again.
//!
//! Resolving costs one loopback HTTP round trip per (peer, class) until a dial
//! fails. The data path adds no hop: the bridge cw-rails hands back is the
//! same loopback splice its own callers use.
//!
//! Endpoint labels are cw-rails' own (`iroh:127.0.0.1:54321→ab3f…`,
//! `ip:100.64.0.2:9742`), so a trace names the path the bytes take; this
//! transport's name is `rails`.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use kernel_types::NodeId;

use crate::door::{Reach, ReachQuery, REACH_PATH};
use crate::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

/// How svrn and serve reach peers: through cw-rails, by peer and class.
#[derive(Debug)]
pub struct RailsTransport {
    api: String,
    http: reqwest::Client,
    cache: Mutex<HashMap<(NodeId, TrafficClass), Vec<PeerEndpoint>>>,
}

impl RailsTransport {
    /// `api` is cw-rails' loopback API base, e.g. `http://127.0.0.1:9747`.
    pub fn new(api: impl Into<String>) -> Self {
        Self {
            api: api.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<(NodeId, TrafficClass), Vec<PeerEndpoint>>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask the door. `Err` carries why, for the trace: the trait's answer to
    /// a refusal is an empty list, and the reason must not vanish with it.
    async fn resolve(
        &self,
        peer: &PeerContact,
        class: TrafficClass,
    ) -> Result<Vec<PeerEndpoint>, String> {
        let url = format!("{}{REACH_PATH}", self.api);
        let query = ReachQuery {
            peer: peer.node_id.to_hex(),
            class: class.as_str().to_string(),
        };
        let resp = self
            .http
            .get(&url)
            .query(&query)
            .send()
            .await
            .map_err(|e| format!("cw-rails did not answer at {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("cw-rails refused ({status}): {body}"));
        }
        let reach: Reach = resp
            .json()
            .await
            .map_err(|e| format!("cw-rails' answer is not a reach: {e}"))?;
        Ok(reach.endpoints)
    }
}

#[async_trait::async_trait]
impl PeerTransport for RailsTransport {
    fn name(&self) -> &'static str {
        "rails"
    }

    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint> {
        let key = (peer.node_id, class);
        if let Some(hit) = self.cache().get(&key).cloned() {
            tracing::debug!(
                target: "transport",
                peer = %peer.node_id,
                class = class.as_str(),
                first = %hit[0].label,
                "rails transport: cached endpoints"
            );
            return hit;
        }
        match self.resolve(peer, class).await {
            Ok(endpoints) if !endpoints.is_empty() => {
                tracing::info!(
                    target: "transport",
                    peer = %peer.node_id,
                    class = class.as_str(),
                    first = %endpoints[0].label,
                    candidates = endpoints.len(),
                    "rails transport: resolved through cw-rails"
                );
                self.cache().insert(key, endpoints.clone());
                endpoints
            }
            Ok(_) => {
                tracing::warn!(
                    target: "transport",
                    peer = %peer.node_id,
                    class = class.as_str(),
                    "rails transport: cw-rails answered no endpoint as a success — treated as none"
                );
                Vec::new()
            }
            Err(why) => {
                tracing::info!(
                    target: "transport",
                    peer = %peer.node_id,
                    class = class.as_str(),
                    why = %why,
                    "rails transport: no endpoint"
                );
                Vec::new()
            }
        }
    }

    fn note_failure(&self, peer: NodeId, class: TrafficClass, endpoint: &PeerEndpoint) {
        let dropped = self.cache().remove(&(peer, class)).is_some();
        tracing::info!(
            target: "transport",
            peer = %peer,
            class = class.as_str(),
            endpoint = %endpoint.label,
            dropped,
            "rails transport: dial failed — the next dial asks cw-rails again"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;

    use super::*;

    /// A stand-in door that labels each endpoint with the class it was asked
    /// for, counts the asks, and refuses a peer named `refused`.
    async fn door() -> (String, Arc<AtomicUsize>) {
        async fn answer(
            State(asks): State<Arc<AtomicUsize>>,
            Query(q): Query<ReachQuery>,
        ) -> axum::response::Response {
            asks.fetch_add(1, Ordering::SeqCst);
            if q.peer == NodeId::from_u128(0xBAD).to_hex() {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "'refused' is offline"})),
                )
                    .into_response();
            }
            Json(Reach {
                peer: "peer".into(),
                node_id: q.peer,
                class: q.class.clone(),
                endpoints: vec![PeerEndpoint {
                    base_url: "http://127.0.0.1:1".into(),
                    label: format!("stub:{}", q.class),
                }],
            })
            .into_response()
        }
        let asks = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new()
            .route(REACH_PATH, axum::routing::get(answer))
            .with_state(asks.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}"), asks)
    }

    fn contact(id: u128) -> PeerContact {
        PeerContact {
            node_id: NodeId::from_u128(id),
            addresses: Vec::new(),
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
        }
    }

    #[test]
    fn every_class_name_parses_back_to_its_class() {
        for class in TrafficClass::ALL {
            assert_eq!(TrafficClass::from_name(class.as_str()), Some(class));
        }
        assert_eq!(TrafficClass::from_name("carrier_pigeon"), None);
    }

    /// The class crosses the door as its name, whatever it is: a transport
    /// that mapped a class to something else would be answered for the wrong
    /// one.
    #[tokio::test]
    async fn the_door_is_asked_for_the_class_the_caller_named() {
        let (api, _) = door().await;
        let t = RailsTransport::new(api);
        for class in TrafficClass::ALL {
            let eps = t.endpoints(&contact(1), class).await;
            assert_eq!(eps.len(), 1, "{class:?}");
            assert_eq!(eps[0].label, format!("stub:{}", class.as_str()));
        }
    }

    /// Cached per peer and class; a dial failure drops the entry and the next
    /// dial asks again.
    #[tokio::test]
    async fn a_resolved_bridge_is_reused_until_a_dial_fails() {
        let (api, asks) = door().await;
        let t = RailsTransport::new(api);
        let eps = t.endpoints(&contact(1), TrafficClass::Media).await;
        t.endpoints(&contact(1), TrafficClass::Media).await;
        assert_eq!(
            asks.load(Ordering::SeqCst),
            1,
            "the second ask hit the cache"
        );
        t.endpoints(&contact(1), TrafficClass::App).await;
        assert_eq!(
            asks.load(Ordering::SeqCst),
            2,
            "another class is another ask"
        );
        t.note_failure(NodeId::from_u128(1), TrafficClass::Media, &eps[0]);
        t.endpoints(&contact(1), TrafficClass::Media).await;
        assert_eq!(asks.load(Ordering::SeqCst), 3, "a failed dial re-resolves");
    }

    /// A refusal is no endpoint, and it is not cached: the next dial asks.
    #[tokio::test]
    async fn a_refusal_is_no_endpoint_and_is_asked_again() {
        let (api, asks) = door().await;
        let t = RailsTransport::new(api);
        assert!(t
            .endpoints(&contact(0xBAD), TrafficClass::Inference)
            .await
            .is_empty());
        assert!(t
            .endpoints(&contact(0xBAD), TrafficClass::Inference)
            .await
            .is_empty());
        assert_eq!(asks.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn no_cw_rails_is_no_endpoint() {
        let t = RailsTransport::new("http://127.0.0.1:1");
        assert!(t
            .endpoints(&contact(1), TrafficClass::Gossip)
            .await
            .is_empty());
    }
}
