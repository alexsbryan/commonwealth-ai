// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dial-by-key host access (Track M, M2 — see
//! `sovereign/docs/specs/TRANSPORT_MIGRATION.md`).
//!
//! For a host row with `endpoint_kind = 'iroh'`, the address column
//! holds a pairing string `<endpoint-id-hex>@<relay-url>[,<addr>...]`
//! (copied from the host's `GET /status` → `iroh.dial`). This module
//! turns that into a localhost TCP bridge: one iroh QUIC connection
//! per accepted socket, dialed by the host's Ed25519 key — the QUIC
//! handshake itself verifies we reached the right machine. `ApiClient`
//! and the WebSocket stream then speak plain HTTP/WS to
//! `127.0.0.1:{port}` with zero changes.
//!
//! The phone's own iroh identity is **ephemeral** (fresh key per app
//! run): the host authenticates clients by bearer token at the HTTP
//! layer, not by client key, so persisting a phone identity would add
//! key-management surface for nothing.

use std::collections::HashMap;
use std::net::SocketAddr;

use commonwealth_transport::iroh::{
    parse_dial_string, presets, ring_crypto_provider, Endpoint, EndpointBuilder, HttpBridge,
    SecretKey, CLIENT_ALPN,
};

use crate::error::{Error, Result};

/// Per-host bridge registry + the app's single iroh endpoint (lazily
/// bound on first iroh-kind dial; never bound if every host is
/// tailnet-kind).
pub struct BridgeManager {
    endpoint: tokio::sync::OnceCell<Endpoint>,
    /// host_connection.id → live bridge. Dropping an entry aborts its
    /// accept loop.
    bridges: tokio::sync::Mutex<HashMap<String, HttpBridge>>,
}

impl Default for BridgeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeManager {
    pub fn new() -> Self {
        Self {
            endpoint: tokio::sync::OnceCell::new(),
            bridges: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    async fn endpoint(&self) -> Result<&Endpoint> {
        self.endpoint
            .get_or_try_init(|| async {
                // presets::N0: relay transports + address lookup on,
                // so a relay-URL target in the pairing string is
                // dialable from any network (LTE included).
                EndpointBuilder::new(presets::N0)
                    .crypto_provider(ring_crypto_provider())
                    .secret_key(SecretKey::generate())
                    .bind()
                    .await
                    .map_err(|e| Error::Other(format!("iroh endpoint bind failed: {e}")))
            })
            .await
    }

    /// Get or create the localhost bridge for `host_id`, dialing the
    /// pairing string `dial`. Returns the local socket address to
    /// point HTTP/WS at.
    pub async fn bridge_for(&self, host_id: &str, dial: &str) -> Result<SocketAddr> {
        let mut bridges = self.bridges.lock().await;
        if let Some(b) = bridges.get(host_id) {
            return Ok(b.local_addr());
        }
        let target = parse_dial_string(dial).map_err(Error::Other)?;
        let endpoint = self.endpoint().await?.clone();
        let bridge = HttpBridge::spawn(endpoint, target, CLIENT_ALPN)
            .await
            .map_err(|e| Error::Other(format!("iroh bridge bind failed: {e}")))?;
        let local = bridge.local_addr();
        tracing::info!(host = %host_id, local = %local, "iroh: bridge up for host");
        bridges.insert(host_id.to_string(), bridge);
        Ok(local)
    }

    /// Tear down a host's bridge (host removed, or its dial string
    /// changed). The next `bridge_for` re-creates it from the fresh
    /// address.
    pub async fn drop_bridge(&self, host_id: &str) {
        if self.bridges.lock().await.remove(host_id).is_some() {
            tracing::info!(host = %host_id, "iroh: bridge dropped");
        }
    }
}
