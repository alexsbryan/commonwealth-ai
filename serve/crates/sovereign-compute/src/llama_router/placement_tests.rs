// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;

use sovereign_contracts::setup_config::SetupConfig;

use super::*;

fn addr(raw: &str) -> SocketAddr {
    raw.parse().expect("an address")
}

fn placement(endpoints: &[&str]) -> RouterPlacement {
    RouterPlacement::from_endpoints(endpoints.iter().copied())
        .unwrap_or_else(|e| panic!("{endpoints:?}: {e}"))
}

#[test]
fn no_endpoints_is_one_card_and_writes_no_rpc_key() {
    let local = placement(&[]);
    assert_eq!(local, RouterPlacement::Local);
    assert_eq!(local.rpc_list(), None);
    assert!(local.peers().is_empty());
    // Blank entries (a trailing comma in a config) are not peers.
    assert_eq!(placement(&["", "  "]), RouterPlacement::Local);
}

#[test]
fn the_endpoints_are_one_comma_list_in_the_order_written() {
    let p = placement(&["192.168.1.20:50052", "100.64.0.7:50052"]);
    assert_eq!(
        p.rpc_list().as_deref(),
        Some("192.168.1.20:50052,100.64.0.7:50052")
    );
    // `-ts` indexes devices in the order `rpc` wrote them, so the order is
    // the contract, not an accident of the map.
    assert_eq!(
        p.peers(),
        &[addr("192.168.1.20:50052"), addr("100.64.0.7:50052")]
    );
    assert_eq!(p.to_string(), "across 192.168.1.20:50052,100.64.0.7:50052");
}

#[test]
fn an_endpoint_no_peer_can_be_reached_at_is_refused_by_name() {
    for (raw, why) in [
        ("127.0.0.1:50052", "loopback"),
        ("[::1]:50052", "loopback"),
        ("0.0.0.0:50052", "every interface"),
        ("[::]:50052", "every interface"),
        ("192.168.1.20:0", "port 0"),
        ("peer.local:50052", "not an ip:port"),
    ] {
        let e = RouterPlacement::from_endpoints([raw]).expect_err(raw);
        assert!(e.contains("[compute] rpc_peers"), "{raw}: {e}");
        assert!(e.contains(why), "{raw}: {e}");
    }
}

#[test]
fn one_peer_named_twice_is_refused_rather_than_loaded_as_two_devices() {
    let e = RouterPlacement::from_endpoints(["192.168.1.20:50052", "192.168.1.20:50052"])
        .expect_err("a repeated peer");
    assert!(e.contains("twice"), "{e}");
}

#[test]
fn a_config_with_no_peers_is_the_single_card_it_always_was() {
    let config: SetupConfig = toml::from_str("[node]\n").expect("a config with no [compute]");
    assert_eq!(
        RouterPlacement::from_config(&config),
        Ok(RouterPlacement::Local)
    );
}

#[test]
fn the_configs_peer_list_is_the_rpc_key() {
    let config: SetupConfig =
        toml::from_str("[compute]\nrpc_peers = [\"10.1.2.3:50052\", \"10.1.2.4:50052\"]\n")
            .expect("a config with peers");
    let p = RouterPlacement::from_config(&config).expect("peer list");
    assert_eq!(
        p.rpc_list().as_deref(),
        Some("10.1.2.3:50052,10.1.2.4:50052")
    );
}

/// The line `/status` adds for a distributed model that is not loaded names
/// the peers it is split across and what happens next; a single-card router
/// adds nothing (there is no peer to lose).
#[test]
fn peer_loss_names_the_peers_on_a_distributed_router_only() {
    let p = placement(&["192.168.1.20:50052"]);
    let lost = p
        .peer_loss("Qwen3.5-9B", "failed (exit 1)")
        .expect("a distributed model that failed");
    assert!(
        lost.contains("failed (exit 1)")
            && lost.contains("peer loss")
            && lost.contains("192.168.1.20:50052")
            && lost.contains("Qwen3.5-9B"),
        "{lost}"
    );
    assert_eq!(
        RouterPlacement::Local.peer_loss("Qwen3.5-9B", "loaded"),
        None
    );
}
