// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest half of an iroh-reached lend: dial a lender by key and expose it
//! as a plain `http://127.0.0.1:<port>` base.
//!
//! # Why a guest needs a tunnel at all
//!
//! A mesh with `require_encryption = true` forces its client API loopback-only
//! (`daemon::start_daemon`, "encrypted mesh: forcing client API to
//! loopback-only"). The address a guest link would otherwise carry answers
//! nothing; the only ingress is the iroh acceptor. Measured on 2026-08-27:
//! config said `client_bind = "0.0.0.0"`, the daemon bound `127.0.0.1`, and a
//! curl to the LAN address returned 000 (note `3ec305f3`).
//!
//! So the guest dials. [`GuestTunnel::open`] parses the lender's dial string,
//! binds an ephemeral iroh endpoint, and spawns an
//! [`HttpBridge`](commonwealth_transport::iroh::HttpBridge) on
//! [`GUEST_ALPN`] — the protocol the lender routes to its bearer-checking
//! listener rather than to the one that admits loopback. Point any HTTP client
//! at [`base_url`](GuestTunnel::base_url) and it rides QUIC.
//!
//! # The tunnel is the identity of the route
//!
//! Dropping a [`GuestTunnel`] aborts the bridge's accept loop, so the local
//! port stops answering. A caller that resolves a base URL and drops the
//! tunnel has a URL that dials nothing — hold it for as long as requests may
//! be sent. There is no fallback to the link's plaintext `url`: a mesh that
//! asked for encryption does not get quietly downgraded because a relay was
//! slow (§18.3).
//!
//! # Key
//!
//! An EPHEMERAL key, freshly generated per tunnel, not this machine's
//! `node_key`. Two reasons: a guest is by definition not a member, so it has
//! no mesh identity to present, and reusing `node_key` would bind a second
//! endpoint to a key a local daemon may already have bound. The lender does
//! not authenticate the dialer by key — the bearer is the whole credential.

use commonwealth_transport::iroh::{
    build_relayed_endpoint, parse_dial_string, Endpoint, HttpBridge, RelayConfig, SecretKey,
    GUEST_ALPN,
};

/// A live tunnel to a lender. Hold it for as long as the base URL is in use.
pub struct GuestTunnel {
    base_url: String,
    /// Aborts its accept loop on drop — the reason this is held and not
    /// discarded after the URL is read.
    _bridge: HttpBridge,
    /// Kept alive alongside the bridge: the bridge dials FROM this endpoint,
    /// and an unbound endpoint is a tunnel to nowhere.
    _endpoint: Endpoint,
}

impl std::fmt::Debug for GuestTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestTunnel")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl GuestTunnel {
    /// Dial `dial` and return a tunnel serving it at a loopback base URL.
    ///
    /// `relay_urls` / `discovery` are the guest's OWN `[iroh]` config, passed
    /// in the same two parts [`RelayConfig::from_parts`] takes — so a guest
    /// that severed n0 (`discovery = "none"`) is not quietly put back on it by
    /// accepting a lend. Taking the parts rather than the built type keeps the
    /// transport crate out of the CLI's dependency list.
    ///
    /// Errors name which step failed — an unparseable dial string and an
    /// unbindable endpoint have different repairs and must not read the same.
    pub async fn open(
        dial: &str,
        relay_urls: Vec<String>,
        discovery: Option<&str>,
    ) -> Result<GuestTunnel, String> {
        let target = parse_dial_string(dial)?;
        let relay_cfg = RelayConfig::from_parts(relay_urls, discovery);
        let secret = SecretKey::from_bytes(&rand::random::<[u8; 32]>());
        let endpoint = build_relayed_endpoint(secret, vec![GUEST_ALPN.to_vec()], &relay_cfg)
            .await
            .map_err(|e| format!("could not bind a local iroh endpoint to dial out from: {e}"))?;
        let bridge = HttpBridge::spawn(endpoint.clone(), target, GUEST_ALPN)
            .await
            .map_err(|e| format!("could not open the tunnel to {dial}: {e}"))?;
        let base_url = format!("http://{}", bridge.local_addr());
        tracing::info!(
            target: "transport",
            %base_url,
            %dial,
            "guest tunnel: dialing the lender on GUEST_ALPN"
        );
        Ok(GuestTunnel {
            base_url,
            _bridge: bridge,
            _endpoint: endpoint,
        })
    }

    /// Where to send requests. A plain HTTP base with no trailing `/v1`, the
    /// same shape a guest link's `url` has, so callers substitute one for the
    /// other without knowing which they hold.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure a guest actually hits: a link whose `dial=` survived a
    /// clipboard badly. It must name the dial string, not surface as a
    /// connection timeout minutes later.
    #[tokio::test]
    async fn a_malformed_dial_string_fails_before_any_socket_is_bound() {
        let err = GuestTunnel::open("not-a-dial-string", Vec::new(), None)
            .await
            .expect_err("a dial string with no '@' cannot name an endpoint");
        assert!(
            err.contains("missing '@'"),
            "the error must name the malformation: {err}"
        );
    }

    #[tokio::test]
    async fn an_endpoint_id_of_the_wrong_length_is_refused() {
        let err = GuestTunnel::open("beef@127.0.0.1:9999", Vec::new(), None)
            .await
            .expect_err("2 bytes is not an Ed25519 public key");
        assert!(err.contains("expected 32"), "{err}");
    }
}
