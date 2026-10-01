// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails produces two rows that svrn's clients decode with
//! `sovereign_contracts::daemon_wire` types: its own reachability
//! (`/v1/mesh/status.self_reachability`) and the invite relay picker
//! (`/v1/mesh/relay-candidates`). cmnwlth may not name sovereign-contracts, so
//! each side owns its struct, and these pin that the two spell the same row.
//! Moved from sovereign-mesh's iroh_watchdog.rs and mesh_discovery.rs, where
//! they sat beside the re-exports this crate no longer reads
//! (pb-mesh-dissolve). A field added on one side only fails here.

#[test]
fn the_watchdogs_status_spells_the_clients_row() {
    let s = commonwealth_rails::iroh_watchdog::ReachabilityStatus {
        relay_homed: true,
        relay_urls: vec!["https://relay.example/".into()],
        discovery_ok: Some(false),
        last_error: Some("gone".into()),
        last_recovery: Some(commonwealth_rails::iroh_watchdog::RecoveryEvent {
            action: "relay_bounce".into(),
            at_unix: 7,
            ok: true,
        }),
        rebuilds: 2,
        peer_paths_total: 3,
        peer_paths_active: 1,
        peer_paths_wedged: true,
        degraded: true,
    };
    let json = serde_json::to_value(&s).unwrap();
    let client: sovereign_contracts::daemon_wire::ReachabilityStatus =
        serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&client).unwrap(), json);
}

#[test]
fn the_producers_relay_candidate_decodes_as_the_clients() {
    let row = commonwealth_discovery::mesh_discovery::RelayCandidate {
        ip: "100.64.0.2".into(),
        kind: "tailscale".into(),
        url_fragment: "100.64.0.2:9742".into(),
        recommended: true,
    };
    let json = serde_json::to_value(&row).unwrap();
    let client: sovereign_contracts::daemon_wire::RelayCandidate =
        serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&client).unwrap(), json);
}
