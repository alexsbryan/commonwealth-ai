// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moved to `commonwealth_discovery::mesh_discovery` (phase-b
//! pb-rails-parity); re-exported here at its historical path.

pub use commonwealth_discovery::mesh_discovery::{
    local_ip_candidates, reachable_addresses, read_advertise_addr_override, relay_candidates,
    RelayCandidate,
};

#[cfg(test)]
mod tests {
    /// The row has two definitions since the move: the producer's in
    /// commonwealth-discovery and the svrn client's decode in
    /// sovereign-contracts. This crate sees both, so it holds them to one
    /// spelling: a field renamed or dropped on either side fails here.
    #[test]
    fn the_producers_relay_candidate_decodes_as_the_clients() {
        let row = super::RelayCandidate {
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
}
