// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moved to `commonwealth_rails::iroh_watchdog` (phase-b pb-rails-parity);
//! re-exported here at its historical path, and the daemon runs the same
//! watchdog over its own endpoint until the flip turns its copy off.

pub use commonwealth_rails::iroh_watchdog::{
    spawn, ReachPathObservation, ReachPathsFn, ReachabilityStatus, RebuildFn, RecoveryEvent,
    WatchdogConfig, WatchdogHandle,
};

/// The watchdog's snapshot as the daemon's `/v1/mesh/status.self_reachability`
/// row spells it (`sovereign_contracts::daemon_wire`). The one conversion
/// between the producer's record and the svrn client's, at the daemon's one
/// read site; field for field, so a field added on one side and not the
/// other fails to compile here or fails the spelling test below.
pub fn to_wire(s: &ReachabilityStatus) -> sovereign_contracts::daemon_wire::ReachabilityStatus {
    sovereign_contracts::daemon_wire::ReachabilityStatus {
        relay_homed: s.relay_homed,
        relay_urls: s.relay_urls.clone(),
        discovery_ok: s.discovery_ok,
        last_error: s.last_error.clone(),
        last_recovery: s.last_recovery.as_ref().map(|e| {
            sovereign_contracts::daemon_wire::RecoveryEvent {
                action: e.action.clone(),
                at_unix: e.at_unix,
                ok: e.ok,
            }
        }),
        rebuilds: s.rebuilds,
        peer_paths_total: s.peer_paths_total,
        peer_paths_active: s.peer_paths_active,
        peer_paths_wedged: s.peer_paths_wedged,
        degraded: s.degraded,
    }
}

#[cfg(test)]
mod tests {
    /// The producer's record serialises exactly as its wire conversion does:
    /// a serde rename on either side, or a field one side drops, fails here.
    #[test]
    fn the_watchdogs_status_spells_the_clients_row() {
        let s = super::ReachabilityStatus {
            relay_homed: true,
            relay_urls: vec!["https://relay.example/".into()],
            discovery_ok: Some(false),
            last_error: Some("gone".into()),
            last_recovery: Some(super::RecoveryEvent {
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
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::to_value(super::to_wire(&s)).unwrap()
        );
    }
}
