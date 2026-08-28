// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest path, end to end and in one process: a caller with no mesh key
//! dials a lender by key, tunnels on `GUEST_ALPN`, and lands on a listener
//! that reads its bearer.
//!
//! The unit tests either side of this prove halves. `client_auth.rs` proves
//! the untrusting policy refuses a loopback caller; `iroh.rs` proves the ALPN
//! routes to a different listener. Neither proves the JOIN — that the address
//! a tunnelled request actually arrives from is the loopback one the policy
//! was built to distrust. That is the whole finding, and it is only visible
//! when the acceptor's real forward hop is in the path.
//!
//! Hermetic: `discovery = "none"` on the guest side and `presets::Minimal` +
//! `RelayMode::Disabled` under it, so nothing here contacts n0. The lender's
//! dial string carries loopback UDP sockets instead of a relay URL, which is
//! the same string shape `/v1/mesh/status` publishes.

use std::collections::HashMap;
use std::net::SocketAddr;

use commonwealth_api::client_auth::ClientAuthPolicy;
use commonwealth_api::server::client_router_with;
use commonwealth_core::ids::NodeId;
use commonwealth_transport::iroh::{
    format_dial_string, EndpointBuilder, IrohAcceptor, SecretKey, GUEST_ALPN,
};
use sovereign_mesh::guest_tunnel::GuestTunnel;

mod common;
use common::{client_app_state, spawn_router};

const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

/// iroh binds the wildcard, which is not dialable as-is — rewrite to loopback.
fn dialable_sockets(endpoint: &commonwealth_transport::iroh::Endpoint) -> Vec<SocketAddr> {
    endpoint
        .bound_sockets()
        .into_iter()
        .map(|mut a| {
            if a.ip().is_unspecified() {
                if a.is_ipv4() {
                    a.set_ip("127.0.0.1".parse().unwrap());
                } else {
                    a.set_ip("::1".parse().unwrap());
                }
            }
            a
        })
        .collect()
}

/// A lender: the guest bind of the client router, plus an acceptor routing
/// `GUEST_ALPN` to it. Returns the dial string a guest link would carry.
async fn spawn_lender(policy: ClientAuthPolicy) -> (String, IrohAcceptor) {
    // `require_encryption: true` is the shape that motivates the whole path —
    // such a mesh binds its client API loopback-only, so iroh is the only way
    // a guest gets in.
    let state = client_app_state(NodeId::from_u128(0xA11CE), Some(TOKEN), true);
    let guest_addr = spawn_router(client_router_with(state, policy)).await;

    let endpoint = EndpointBuilder::empty()
        .crypto_provider(commonwealth_transport::iroh::ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[31; 32]))
        .alpns(vec![GUEST_ALPN.to_vec()])
        .bind()
        .await
        .expect("lender endpoint binds");

    let mut routes = HashMap::new();
    routes.insert(GUEST_ALPN.to_vec(), guest_addr);
    let acceptor = IrohAcceptor::spawn_routed(endpoint.clone(), routes);

    let mut addr = endpoint.addr();
    addr.addrs.clear();
    for s in dialable_sockets(&endpoint) {
        addr = addr.with_ip_addr(s);
    }
    let dial = format_dial_string(&addr).expect("the lender has dialable sockets");
    (dial, acceptor)
}

/// THE test. A tunnelled request arrives from `127.0.0.1` — the acceptor's own
/// forward hop — and must still be made to prove who it is.
///
/// Watched failing: run this against `ClientAuthPolicy::default()` (the twin
/// below) and the same request is admitted with no credential at all, which is
/// every holder of a public dial string reaching the whole client API.
#[tokio::test]
async fn a_tunnelled_request_with_no_credential_is_refused() {
    let (dial, _acceptor) = spawn_lender(ClientAuthPolicy::UNTRUSTED_LOOPBACK).await;
    let tunnel = GuestTunnel::open(&dial, Vec::new(), Some("none"))
        .await
        .expect("the guest tunnel opens");

    let resp = reqwest::Client::new()
        .get(format!("{}/v1/models", tunnel.base_url()))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .expect("the request reaches the lender through the tunnel");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a tunnelled caller earns nothing by arriving on loopback"
    );
}

/// The twin, and the reason the one above is not merely "something is broken":
/// the DEFAULT policy admits that identical request. The policy is what
/// separates the two listeners, not the tunnel and not the route.
#[tokio::test]
async fn the_same_tunnelled_request_is_admitted_under_the_default_policy() {
    let (dial, _acceptor) = spawn_lender(ClientAuthPolicy::default()).await;
    let tunnel = GuestTunnel::open(&dial, Vec::new(), Some("none"))
        .await
        .expect("the guest tunnel opens");

    let resp = reqwest::Client::new()
        .get(format!("{}/v1/models", tunnel.base_url()))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .expect("the request reaches the lender through the tunnel");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "the loopback arm admits the acceptor's forward hop — this is the hole"
    );
}

/// And the feature works: a credential presented over the tunnel is read and
/// honoured. A listener that refused everything would pass the first test and
/// be useless.
#[tokio::test]
async fn a_credential_presented_over_the_tunnel_is_read_and_admitted() {
    let (dial, _acceptor) = spawn_lender(ClientAuthPolicy::UNTRUSTED_LOOPBACK).await;
    let tunnel = GuestTunnel::open(&dial, Vec::new(), Some("none"))
        .await
        .expect("the guest tunnel opens");

    let resp = reqwest::Client::new()
        .get(format!("{}/v1/models", tunnel.base_url()))
        .bearer_auth(TOKEN)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .expect("the request reaches the lender through the tunnel");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// `/status` is what `mesh use` probes before storing a link, and it is in
/// `AUTH_EXEMPT_PATHS` on every listener — including this one, where the
/// caller by definition has not yet proven anything.
#[tokio::test]
async fn the_exempt_health_surface_is_reachable_through_the_tunnel() {
    let (dial, _acceptor) = spawn_lender(ClientAuthPolicy::UNTRUSTED_LOOPBACK).await;
    let tunnel = GuestTunnel::open(&dial, Vec::new(), Some("none"))
        .await
        .expect("the guest tunnel opens");

    let resp = reqwest::Client::new()
        .get(format!("{}/status", tunnel.base_url()))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .expect("the request reaches the lender through the tunnel");
    assert!(resp.status().is_success(), "got {}", resp.status());
}
