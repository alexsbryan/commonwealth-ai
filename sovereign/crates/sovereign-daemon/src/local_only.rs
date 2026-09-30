// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Is this daemon local-only?** — asked once, answered in one place.
//!
//! # Why this exists
//!
//! `cw-lift`'s `cw-local-only-daemon` bar claimed "a sovereign daemon can
//! compile and boot with zero commonwealth-\* and no iroh". Rungs 3a/3b took
//! the DIRECT deps to zero; the fusion census (2026-09-08) then measured what
//! remained and the bar's K2 fired:
//!
//! - 78 distinct `sovereign_mesh::` items reached from `sovereign-cli-daemon`,
//!   classified 25 mesh-only / 32 local-in-substance / 21 BOTH;
//! - **31 unmovable however the seam is drawn**, and 21 of those are the
//!   daemon's own lifecycle (`EmbeddedDaemon`, `daemon_services::assemble`,
//!   `rpc_warm_http`, `persist::resolve_self_node_id`) rather than mesh
//!   features;
//! - the best available crate seam moves **15,634 lines for zero deleted
//!   dependencies** — the shim K2 warns against, not a boundary.
//!
//! So the honest deliverable is a RUNTIME posture, and this type is it: the
//! daemon's network surface is a decision, made here, rather than a property
//! of which crates happen to be linked.
//!
//! # One decider (ARCH §10.6)
//!
//! Half a profile already existed and the halves did not know about each
//! other: `daemon::mdns_enabled_effective` decided mDNS from
//! `[discovery] mdns` + `SOVEREIGN_DISABLE_MDNS`, and
//! `iroh_access::resolve_enabled` decided iroh from `[iroh] enabled` + the
//! client-exposed marker + `SOVEREIGN_IROH`. Nothing answered "is this daemon
//! local-only", so nothing could gate the three unconditional loops
//! (gossip, auto-ingest collaborate, ring-sync) or the rail KV pump beside
//! them. Both existing gates now READ this profile instead of standing
//! parallel to it, so there is one answer and it is `LocalOnlyProfile`.
//!
//! # Not the skill privacy noun
//!
//! `ShardingPrivacy::LocalOnly` (sovereign-contracts/src/skills.rs) says a
//! SKILL's conversations never leave the machine. That is data-sharing
//! policy. This is the daemon's NETWORK posture: which background loops and
//! sockets exist at all. Different question, deliberately different type.
//!
//! # What the profile skips, and what it does not
//!
//! It skips the NETWORK, never the model. A local-only daemon still mints and
//! persists its `Mesh` with one member — the solo case is the honest N=1, and
//! an `Option<Mesh>` would fork every reader of membership into two shapes.
//! What it removes is the traffic: no multicast advertise/browse, no iroh
//! endpoint or relay contact, no gossip round, no peer-assisted ingest
//! handoff, no ring anti-entropy, no rail KV pump.

pub use sovereign_contracts::local_only::{LocalOnlyProfile, LocalOnlySource, ENV_VAR};

/// One background service a running daemon can have spawned.
///
/// A closed set (ARCH §2) so [`RunningServices`] cannot grow a member that no
/// reader visits, and so a new loop added to `start_daemon` without a census
/// entry is a compile-visible omission rather than an invisible one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MeshService {
    /// mDNS `_commonwealth._tcp` advertise (the multicast socket).
    MdnsAdvertise,
    /// mDNS browse loop populating the discovered-peers table.
    MdnsBrowse,
    /// The gossip heartbeat loop.
    Gossip,
    /// The peer-assisted ingest handoff loop (`auto_ingest`).
    AutoIngestCollaborate,
    /// Ring-ledger anti-entropy (`ring_sync`).
    RingSync,
    /// The mesh-store outbox pump that signs local writes onto their rings.
    RailKvPump,
    /// The `ingest:v1` execute origin (`crate::work_origin`), served on
    /// loopback and registered with cw-rails, whose donor forwards units to
    /// it. Spawned only on a node with a corpus engine.
    WorkOrigin,
    /// svrn's peer routes registered with cw-rails' origin table
    /// (`crate::peer_origin`), so a member reaches them through cw-rails.
    PeerOrigin,
    /// The iroh endpoint + acceptor.
    IrohEndpoint,
    /// The founder reachability watchdog (only ever with the endpoint).
    IrohWatchdog,
}

impl MeshService {
    /// Stable name — what the boot trace prints and what a test asserts on.
    pub fn as_str(self) -> &'static str {
        match self {
            MeshService::MdnsAdvertise => "mdns_advertise",
            MeshService::MdnsBrowse => "mdns_browse",
            MeshService::Gossip => "gossip",
            MeshService::AutoIngestCollaborate => "auto_ingest_collaborate",
            MeshService::RingSync => "ring_sync",
            MeshService::RailKvPump => "rail_kv_pump",
            MeshService::WorkOrigin => "work_origin",
            MeshService::PeerOrigin => "peer_origin",
            MeshService::IrohEndpoint => "iroh_endpoint",
            MeshService::IrohWatchdog => "iroh_watchdog",
        }
    }

    /// Every service the census can name. Used by the trace to print what was
    /// NOT spawned, which is the half a log of spawns cannot show.
    pub const ALL: &'static [MeshService] = &[
        MeshService::MdnsAdvertise,
        MeshService::MdnsBrowse,
        MeshService::Gossip,
        MeshService::AutoIngestCollaborate,
        MeshService::RingSync,
        MeshService::RailKvPump,
        MeshService::WorkOrigin,
        MeshService::PeerOrigin,
        MeshService::IrohEndpoint,
        MeshService::IrohWatchdog,
    ];
}

/// What a running daemon actually spawned, recorded at the spawn sites.
///
/// This is the profile's INSTRUMENT (ARCH §18.1): "no network traffic" is not
/// checkable from config, and grepping the log for absences proves nothing. A
/// list of the loops that were started is falsifiable — delete a gate and the
/// boot assertion names the service that reappeared.
///
/// Distinct from [`crate::daemon_services::DaemonServices`], which is what a
/// HOST supplies to the daemon (capabilities, routers, stores). This is what
/// the daemon SPAWNED as a consequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunningServices {
    spawned: std::collections::BTreeSet<MeshService>,
}

impl RunningServices {
    /// Record that `service` was spawned.
    pub fn record(&mut self, service: MeshService) {
        self.spawned.insert(service);
    }

    /// Was it spawned?
    pub fn contains(&self, service: MeshService) -> bool {
        self.spawned.contains(&service)
    }

    /// Everything spawned, in a stable order.
    pub fn spawned(&self) -> Vec<MeshService> {
        self.spawned.iter().copied().collect()
    }

    /// Names of everything spawned — the trace/assert-friendly form.
    pub fn names(&self) -> Vec<&'static str> {
        self.spawned.iter().map(|s| s.as_str()).collect()
    }

    /// Names of everything [`MeshService::ALL`] knows about that was NOT
    /// spawned. The local-only claim is about this list, so the boot trace
    /// prints it rather than leaving the operator to infer absence.
    pub fn skipped_names(&self) -> Vec<&'static str> {
        MeshService::ALL
            .iter()
            .filter(|s| !self.spawned.contains(s))
            .map(|s| s.as_str())
            .collect()
    }

    /// Did this boot start anything that touches the network?
    ///
    /// Every member of the closed set does, which is what makes it the right
    /// set: the census carries background loops that reach a peer, a relay or
    /// a multicast group, and nothing else. `auto_resume`'s local ingest
    /// re-spawn is deliberately not a member — it is work the operator asked
    /// for on this machine.
    pub fn any_network_service(&self) -> bool {
        !self.spawned.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_census_reports_both_halves() {
        let mut svc = RunningServices::default();
        assert!(!svc.any_network_service());
        assert_eq!(svc.skipped_names().len(), MeshService::ALL.len());

        svc.record(MeshService::Gossip);
        svc.record(MeshService::RingSync);
        assert!(svc.any_network_service());
        assert!(svc.contains(MeshService::Gossip));
        assert!(!svc.contains(MeshService::IrohEndpoint));
        assert_eq!(svc.names(), vec!["gossip", "ring_sync"]);
        assert!(svc.skipped_names().contains(&"rail_kv_pump"));
    }
}
