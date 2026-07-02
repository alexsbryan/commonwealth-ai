// SPDX-License-Identifier: AGPL-3.0-or-later
//! UDP-blocked, relay-over-TCP connectivity — the hermetic core of
//! enterprise-hardening item (c): prove mesh traffic still flows when
//! there is NO direct UDP path and the relay itself offers only TCP.
//!
//! The literal corporate scenario (OS drops all UDP, traffic egresses
//! over TCP:443 through an authenticated proxy) needs a packet filter +
//! a proxy + a reachable relay — infrastructure this CI box lacks. So
//! we reproduce the LOAD-BEARING condition at the library level, with
//! iroh's own test relay:
//!
//!   * `run_relay_server_with(false)` — a relay with **no QUIC/UDP**
//!     listener, i.e. reachable only over its TCP (WebSocket/TLS) path.
//!     This IS "UDP blocked at the relay".
//!   * both endpoints `clear_ip_transports()` — **no direct IP/UDP
//!     paths at all**, so nothing can hole-punch; the relay is the only
//!     way through. This IS "UDP blocked between peers".
//!   * the client dials by KEY with an address carrying ONLY the relay
//!     URL (no direct socket addrs), so even the address book offers no
//!     UDP shortcut.
//!
//! A successful byte round-trip then proves data crossed a TCP-only
//! relay with every UDP/direct path removed — the exact guarantee a
//! UDP-blocked corporate network relies on. (The proxy + OS-firewall
//! variant remains a documented manual gate; see MESH_NETOPS.md §5.)
//!
//! Run: cargo test -p commonwealth-transport --features iroh-test-utils
//!      --test relay_tcp_only
#![cfg(feature = "iroh-test-utils")]

use commonwealth_transport::iroh::{ring_crypto_provider, EndpointAddr, EndpointBuilder, ALPN};
use iroh::test_utils::run_relay_server_with;
use iroh::tls::CaTlsConfig;
use iroh::{RelayMode, SecretKey};

/// Build a relay-only endpoint: custom relay = the TCP-only test relay,
/// all direct IP transports cleared, so the relay is the sole path out.
/// `insecure_skip_verify` trusts the test relay's self-signed cert (the
/// same knob iroh's own relay tests use — test-only).
async fn relay_only_endpoint(seed: u8, relay: RelayMode) -> iroh::Endpoint {
    EndpointBuilder::empty()
        .crypto_provider(ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[seed; 32]))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(relay)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports() // no direct UDP paths — relay or nothing
        .bind()
        .await
        .expect("relay-only endpoint binds")
}

#[tokio::test]
async fn traffic_flows_over_tcp_only_relay_with_direct_disabled() {
    // A relay with QUIC/UDP DISABLED — reachable only over TCP.
    let (relay_map, relay_url, _server) = run_relay_server_with(false)
        .await
        .expect("spawn TCP-only test relay");

    // ── Server endpoint: relay-only, accepts one bi-stream and echoes ──
    let server_ep = relay_only_endpoint(1, RelayMode::Custom(relay_map.clone())).await;
    let server_id = server_ep.id();
    // Wait until the server is actually reachable via the relay before the
    // client dials — otherwise the relay has no route to it yet.
    server_ep.online().await;
    let server_task = tokio::spawn(async move {
        let incoming = server_ep.accept().await.expect("incoming connection");
        let conn = incoming.await.expect("handshake completes over the relay");
        let (mut send, mut recv) = conn.accept_bi().await.expect("accept bi-stream");
        // iroh's RecvStream::read_to_end(size_limit) RETURNS the bytes.
        let got = recv
            .read_to_end(64 * 1024)
            .await
            .expect("read client bytes");
        assert_eq!(&got, b"ping-over-tcp-relay");
        send.write_all(b"pong-over-tcp-relay").await.expect("echo");
        let _ = send.finish();
        // Keep the endpoint alive until the client has read the reply.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });

    // ── Client endpoint: relay-only, dials the server BY KEY via an
    // address that contains ONLY the relay URL (no direct socket) ──
    let client_ep = relay_only_endpoint(2, RelayMode::Custom(relay_map)).await;
    let addr = EndpointAddr::new(server_id).with_relay_url(relay_url);

    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client_ep.connect(addr, ALPN),
    )
    .await
    .expect("connect did not hang")
    .expect("connect over the TCP-only relay succeeds");

    let (mut send, mut recv) = conn.open_bi().await.expect("open bi-stream");
    send.write_all(b"ping-over-tcp-relay").await.expect("send");
    send.finish().expect("finish send");
    let reply = recv.read_to_end(64 * 1024).await.expect("read reply");

    assert_eq!(
        &reply, b"pong-over-tcp-relay",
        "a full round-trip must complete over the TCP-only relay with all \
         direct/UDP paths cleared — the UDP-blocked corporate case"
    );

    server_task.await.expect("server task");
}
