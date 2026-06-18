// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dial-by-key client access (Track M, M1 — see
//! `docs/specs/TRANSPORT_MIGRATION.md`).
//!
//! When `[iroh] enabled = true`, the server binds an iroh endpoint
//! from a persisted Ed25519 identity and forwards every accepted
//! bi-stream (ALPN `cwth/client/0`) to the local HTTP listener via
//! `commonwealth_transport::iroh::IrohAcceptor`. The HTTP stack —
//! router, auth middleware, WS upgrade — is untouched: auth stays
//! bearer-token, which is transport-independent by construction.
//!
//! Pairing surface: `GET /status` exposes `iroh.dial`
//! (`<endpoint-id-hex>@<relay-url>[,<direct-addr>...]`) — the exact
//! string the phone stores as its `endpoint_kind='iroh'` host
//! address.

use std::path::PathBuf;

use commonwealth_transport::iroh::{
    build_relayed_endpoint, format_dial_string, Endpoint, IrohAcceptor, CLIENT_ALPN,
};

use crate::config::ServerConfig;

/// Handle to the running iroh access path. Holds the endpoint (for
/// live status reads) and the acceptor task (aborted on drop).
pub struct IrohAccess {
    endpoint: Endpoint,
    _acceptor: IrohAcceptor,
}

/// Resolve where the identity seed lives: explicit `[iroh] key_path`,
/// else `node_key` beside the store DB.
fn key_dir_and_path(config: &ServerConfig) -> (PathBuf, Option<String>) {
    match &config.iroh.key_path {
        Some(p) => {
            // load_or_generate_node_key takes the DIRECTORY and uses
            // its fixed file name; honour an explicit file path by
            // splitting it.
            let dir = p.parent().map(PathBuf::from).unwrap_or_else(|| ".".into());
            let file = p.file_name().map(|f| f.to_string_lossy().into_owned());
            (dir, file)
        }
        None => {
            let dir = config
                .store
                .path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| ".".into());
            (dir, None)
        }
    }
}

impl IrohAccess {
    /// Bind the endpoint and start forwarding to
    /// `127.0.0.1:{http_port}`. Returns `None` (with a loud log) on
    /// any failure — iroh access is additive; the tailnet path must
    /// never be taken down by it.
    pub async fn start(config: &ServerConfig, http_port: u16) -> Option<IrohAccess> {
        if !config.iroh.enabled {
            return None;
        }
        let (key_dir, custom_file) = key_dir_and_path(config);
        if let Some(f) = &custom_file {
            // The shared loader owns the canonical file name; a
            // custom name would silently fork identities.
            if f != commonwealth_transport::identity::NODE_KEY_FILE {
                tracing::warn!(
                    requested = %f,
                    "iroh: key_path file name must be 'node_key' — using node_key in the same directory"
                );
            }
        }
        let identity = commonwealth_transport::identity::load_or_generate_node_key(&key_dir);
        let secret =
            commonwealth_transport::iroh::SecretKey::from_bytes(&identity.to_bytes());

        // Shared constructor (n0 public relays + address lookup; the
        // relay-fleet self-hosting swap is W4 of the migration doc).
        // The client serves only the client ALPN.
        let endpoint = match build_relayed_endpoint(secret, vec![CLIENT_ALPN.to_vec()]).await {
            Ok(ep) => ep,
            Err(e) => {
                tracing::error!(error = %e, "iroh: endpoint bind failed — dial-by-key access disabled");
                return None;
            }
        };

        let forward_to: std::net::SocketAddr = ([127, 0, 0, 1], http_port).into();
        let acceptor = IrohAcceptor::spawn(endpoint.clone(), forward_to);
        tracing::info!(
            endpoint_id = %endpoint.id(),
            forward_to = %forward_to,
            "iroh: dial-by-key access enabled (ALPN cwth/client/0)"
        );
        Some(IrohAccess {
            endpoint,
            _acceptor: acceptor,
        })
    }

    /// Live status for `GET /status` — also the pairing surface.
    pub fn status_json(&self) -> serde_json::Value {
        let addr = self.endpoint.addr();
        // The pairing string a human copies: `id@relay` ONLY, once a
        // relay is connected. The direct addresses are deliberately
        // omitted — iroh hole-punches direct paths AFTER the relay
        // connects, so they buy nothing at pair time and triple the
        // string length. `dial_full` keeps everything for debugging
        // and for relay-less LANs (hermetic setups).
        let id_hex = hex::encode(addr.id.as_bytes());
        let dial = addr
            .relay_urls()
            .next()
            .map(|relay| format!("{id_hex}@{relay}"))
            .or_else(|| format_dial_string(&addr));
        serde_json::json!({
            "endpoint_id": self.endpoint.id().to_string(),
            "dial": dial,
            "dial_full": format_dial_string(&addr),
            "relay_urls": addr.relay_urls().map(|r| r.to_string()).collect::<Vec<_>>(),
        })
    }
}
