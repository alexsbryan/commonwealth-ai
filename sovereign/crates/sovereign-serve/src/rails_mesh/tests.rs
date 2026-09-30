// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use sovereign_contracts::launch::RpcServe;

const ME: &str = "0000000000000000000000000000000a";
const PEER: &str = "0000000000000000000000000000000b";

/// A `/v1/mesh/status` document in cw-rails' shape (commonwealth-rails
/// api.rs `status`): self, mesh, and one member per row.
fn doc(peer_hex: Option<&str>) -> serde_json::Value {
    let caps = |anchor: serde_json::Value| {
        serde_json::json!({
            "hardware": {"gpus": [], "system_ram_gb": 0, "cpu_cores": 0,
                         "total_storage_gb": 0, "free_storage_gb": 0},
            "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0, "free_storage_gb": 0.0,
                          "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                          "available_for_mesh": false},
            "hosted_corpora": [], "reported_at": 0, "anchor": anchor
        })
    };
    let mut peer = serde_json::json!({
        "name": "worker", "node_id": "node-0000000000000000", "status": "online",
        "last_seen": 7, "is_self": false,
        "capabilities": caps(serde_json::json!({"can_anchor": true, "vram_gb": 8,
                                                "rpc_port": 50060, "rpc_iroh": true})),
        "dial": {"relay_url": null, "iroh_direct_addrs": ["192.168.1.20:41000"]}
    });
    if let Some(hex) = peer_hex {
        peer["node_id_hex"] = hex.into();
    }
    serde_json::json!({
        "self": {"node_id": "node-0000000000000000", "node_id_hex": ME, "name": "host"},
        "mesh": {"id": "m", "name": "lift-mesh"},
        "members": [
            {"name": "host", "node_id": "node-0000000000000000", "node_id_hex": ME,
             "status": "online", "last_seen": 9, "is_self": true,
             "capabilities": caps(serde_json::Value::Null),
             "dial": {"relay_url": null, "iroh_direct_addrs": []}},
            peer
        ]
    })
}

#[test]
fn a_roster_row_becomes_a_member_with_its_full_id_anchor_and_direct_addresses() {
    let reading = parse(serde_json::from_value(doc(Some(PEER))).expect("doc")).expect("reading");
    assert_eq!(reading.mesh_name, "lift-mesh");
    assert_eq!(reading.self_id, NodeId::from_hex(ME).unwrap());
    let peer = reading
        .members
        .iter()
        .find(|m| m.name == "worker")
        .expect("the worker row");
    assert_eq!(peer.node_id, NodeId::from_hex(PEER).unwrap());
    assert_eq!(peer.dial.node_id, peer.node_id);
    assert!(peer.dialable, "a member with a direct address is dialable");
    assert!(
        peer.dial.addresses.is_empty(),
        "cw-rails fills no overlay address"
    );
    assert_eq!(
        peer.dial.iroh_direct_addrs,
        vec!["192.168.1.20:41000".parse::<SocketAddr>().unwrap()]
    );
    let anchor = peer
        .capabilities
        .anchor
        .as_ref()
        .expect("the anchor record");
    assert_eq!((anchor.rpc_port, anchor.rpc_iroh), (Some(50060), true));
    // The member that is this node has no path of its own to dial.
    let me = reading.members.iter().find(|m| m.name == "host").unwrap();
    assert!(!me.dialable);
}

#[test]
fn a_cw_rails_without_full_ids_is_named_not_an_empty_roster() {
    let err = parse(serde_json::from_value(doc(None)).expect("doc")).expect_err("no full id");
    assert!(
        err.contains("node_id_hex") && err.contains("worker"),
        "{err}"
    );
}

#[test]
fn serve_registers_its_peer_prefixes_and_its_rpc_worker_only_when_it_binds() {
    let listen: SocketAddr = "127.0.0.1:18000".parse().unwrap();
    let off = registrations_for(listen, RpcServe::resolve(None, false));
    assert_eq!(off.len(), 1, "no worker bind, no rpc origin: {off:?}");
    assert_eq!(off[0].alpn, "cwth/http/0");
    assert_eq!(off[0].port, 18000);
    assert_eq!(
        off[0].prefixes,
        vec![
            "/internal/v1/models/".to_string(),
            "/internal/rpc-warm".to_string()
        ]
    );
    assert_eq!(off[0].framing, Framing::Http);

    let on = registrations_for(listen, RpcServe::resolve(Some("127.0.0.1:50060"), false));
    let rpc = on
        .iter()
        .find(|r| r.alpn == "cwth/rpc/0")
        .expect("the rpc origin");
    assert_eq!((rpc.port, rpc.framing), (50060, Framing::Bytes));
    let anchor = rpc
        .claims
        .as_ref()
        .and_then(|c| c.anchor.as_ref())
        .expect("the rpc origin declares the anchor record");
    assert!(anchor.can_anchor);
    assert_eq!((anchor.rpc_port, anchor.rpc_iroh), (Some(50060), true));
}

#[test]
fn an_unspecified_listener_is_reached_on_loopback() {
    assert_eq!(
        origin_addr("0.0.0.0:7000".parse().unwrap()),
        "127.0.0.1:7000".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(
        origin_addr("127.0.0.1:7001".parse().unwrap()),
        "127.0.0.1:7001".parse::<SocketAddr>().unwrap()
    );
}
