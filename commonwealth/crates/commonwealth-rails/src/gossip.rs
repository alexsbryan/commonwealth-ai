// SPDX-License-Identifier: AGPL-3.0-or-later
//! The outbound half of anti-entropy: say what we are, hear what they are.
//!
//! Four things happen per round, in this order, and the order matters:
//!
//! 1. **Self-stamp.** Our own row gets a fresh `last_seen`, our identity key,
//!    our LIVE relay and hole-punched addresses, and — when a media origin is
//!    configured — `capabilities.origins = [Media]`. That last field is how a
//!    peer's `svrn mesh media` learns this machine offers a library WITHOUT
//!    dialing it and reading the refusal. Reachability that CHANGED is
//!    re-signed under a bumped version, so a replayed older record loses the
//!    merge's version check.
//! 2. **Decay.** A member we have not heard from within `offline_threshold`
//!    goes `Offline`. Staleness is measured against OUR clock (the contact
//!    map), never against the peer's own gossiped `last_seen` — comparing a
//!    remote clock to ours is what makes a clock-skewed live peer look dead.
//! 3. **Exchange.** Up to three members, online first, rotating so a mesh
//!    larger than three still converges. Each is dialed by KEY through the
//!    transport's bridge; the reply is merged with the same authorization the
//!    inbound path applies, because initiating a round is not evidence about
//!    who answered.
//! 4. **Persist.**
//!
//! Nothing here holds the mesh lock across a round trip.

use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::capabilities::{
    AvailableResources, HardwareProfile, NodeCapabilities, OriginKind,
};
use commonwealth_core::clock::unix_now_secs;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::wire::{GossipRequest, GossipResponse};
use commonwealth_core::mesh::{GossipAuth, Mesh, MeshWire, NodeStatus, SecretDisclosure};
use commonwealth_transport::{peer_contact, PeerContact, TrafficClass};

use crate::{identity, note_contact, RailsDaemon};

/// How many peers one round talks to. Three is the inference daemon's fan and
/// the reason a round is cheap on a mesh of any size; rotation (below) is
/// what makes it converge anyway.
pub const PEERS_PER_ROUND: usize = 3;

/// A capability report for a process that runs no models and hosts no
/// corpora, with the origins it actually serves.
///
/// Every number is ZERO on purpose rather than absent: `NodeCapabilities` has
/// no `Default` impl, and inventing plausible hardware here would put a
/// scheduler's input on the wire for a node that will never take a job.
/// `available_for_mesh: false` and `inference_capable: false` say the same
/// thing in the two fields a scheduler actually reads.
pub fn minimal_capabilities(now: u64, origins: &[OriginKind]) -> NodeCapabilities {
    NodeCapabilities {
        hardware: HardwareProfile {
            gpus: Vec::new(),
            system_ram_gb: 0,
            cpu_cores: 0,
            total_storage_gb: 0,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        },
        available: AvailableResources {
            free_vram_gb: 0.0,
            free_ram_gb: 0.0,
            free_storage_gb: 0.0,
            gpu_utilization: 0.0,
            cpu_utilization: 0.0,
            available_for_mesh: false,
        },
        active_processes: Vec::new(),
        hosted_corpora: Vec::new(),
        reported_at: now,
        inference_availability: 0.0,
        inference_capable: false,
        loaded_models: Vec::new(),
        origins: origins.to_vec(),
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: None,
    }
}

/// What a round found out about this node's own reachability. Split out so
/// the self-stamp's decision — "did anything change, and does it need a fresh
/// signature?" — is testable without an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialInfo {
    pub relay_url: Option<String>,
    pub direct_addrs: Vec<std::net::SocketAddr>,
}

/// Whether a fresh signature is owed, and under which version.
///
/// `None` means the record already carries a current signature over exactly
/// this reachability — re-signing every round would bump the version on no
/// change, and a monotonic counter that moves without a content change is not
/// an anti-rollback key, it is a clock.
pub fn next_dial_version(current_version: u64, has_sig: bool, changed: bool) -> Option<u64> {
    if changed {
        Some(current_version.saturating_add(1).max(1))
    } else if !has_sig {
        Some(current_version.max(1))
    } else {
        None
    }
}

/// Loop until the task is dropped.
pub async fn run_forever(daemon: Arc<RailsDaemon>) {
    let interval = Duration::from_secs(daemon.node.config.gossip_interval_secs);
    let mut round: u64 = 0;
    loop {
        run_one_round(&daemon, round).await;
        round = round.wrapping_add(1);
        tokio::time::sleep(interval).await;
    }
}

/// One round. Never returns an error: a peer that cannot be reached is a
/// logged outcome, not a reason to stop being a member.
pub async fn run_one_round(daemon: &RailsDaemon, round: u64) {
    let now = unix_now_secs();
    let self_id = daemon.node.self_id;
    let threshold = daemon.node.config.offline_threshold_secs;

    let dial = {
        let addr = daemon.node.endpoint.addr();
        // Each read is its OWN statement so the borrowing iterator
        // `relay_urls()` hands back is dropped at that statement's end. As a
        // struct literal in the block's tail expression this is E0597 —
        // `addr` is dropped while the iterator's borrow is still live.
        let relay_url = addr.relay_urls().next().map(|r| r.to_string());
        let direct_addrs: Vec<std::net::SocketAddr> = addr.ip_addrs().copied().collect();
        DialInfo {
            relay_url,
            direct_addrs,
        }
    };
    let origins = if daemon.node.config.media.origin.is_some() {
        vec![OriginKind::Media]
    } else {
        Vec::new()
    };

    // Step 1 + 2 + 3's selection, in ONE write-lock window. Nothing awaits a
    // network inside it.
    let targets: Vec<(NodeId, String, PeerContact)> = {
        let mut mesh = daemon.mesh.write().await;
        self_stamp(&mut mesh, self_id, now, &dial, &origins, &daemon.node.key);
        decay(
            &mut mesh,
            self_id,
            now,
            threshold,
            &*daemon.contacts.lock().await,
        );
        select_peers(&mesh, self_id, round)
    };

    if targets.is_empty() {
        tracing::debug!(
            target: "gossip",
            round,
            "gossip: no dialable peer this round (a mesh of one, or no peer has a key yet)"
        );
        return;
    }

    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target: "gossip", error = %e, "gossip: no HTTP client");
            return;
        }
    };

    let mut reached = 0usize;
    for (peer_id, peer_name, contact) in targets {
        let endpoints = daemon
            .transport
            .endpoints(&contact, TrafficClass::Gossip)
            .await;
        let Some(ep) = endpoints.into_iter().next() else {
            tracing::info!(
                target: "gossip",
                round,
                peer = %peer_name,
                node_id = %peer_id,
                outcome = "no-addresses",
                "gossip: peer gossips no relay and no direct address — nothing to dial"
            );
            continue;
        };
        // The snapshot is taken fresh per peer and the lock released before
        // the POST. A round that held it would serialize the whole daemon on
        // the slowest peer.
        let body = {
            let mesh = daemon.mesh.read().await;
            GossipRequest {
                // Redacted: this daemon is post-split by construction and
                // only ever joins a mesh that minted a secret, so the raw
                // credential never needs to ride our request. A peer that
                // cannot authorize without it falls through to the legacy
                // arm on the `invite_key_hash` both sides already carry.
                mesh: MeshWire::for_peer(&mesh, SecretDisclosure::Redact),
                from: Some(self_id),
                mesh_proof: mesh.mesh_proof(self_id, now),
            }
        };
        let url = format!("{}/internal/gossip", ep.base_url);
        let response = match http.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::info!(
                    target: "gossip",
                    round,
                    peer = %peer_name,
                    node_id = %peer_id,
                    via = %ep.label,
                    outcome = "transport-error",
                    error = %e,
                    "gossip: round failed"
                );
                continue;
            }
        };
        if !response.status().is_success() {
            tracing::info!(
                target: "gossip",
                round,
                peer = %peer_name,
                node_id = %peer_id,
                status = response.status().as_u16(),
                outcome = "rejected",
                "gossip: the peer refused our round (wrong mesh or invite hash)"
            );
            continue;
        }
        let parsed: GossipResponse = match response.json().await {
            Ok(p) => p,
            Err(e) => {
                tracing::info!(
                    target: "gossip",
                    round,
                    peer = %peer_name,
                    outcome = "bad-response",
                    error = %e,
                    "gossip: the peer answered something that is not a GossipResponse"
                );
                continue;
            }
        };
        // The reply is an authorization boundary in its own direction — we
        // merge it, so it must prove itself. Having initiated the round says
        // nothing about who answered.
        let auth = GossipAuth {
            sender: parsed.from,
            proof: parsed.mesh_proof,
            now_secs: now,
        };
        let incoming = parsed.mesh.into_mesh();
        let report = {
            let mut mesh = daemon.mesh.write().await;
            mesh.merge_from_authenticated(self_id, &incoming, &auth)
        };
        if report.rejected() {
            tracing::warn!(
                target: "gossip",
                round,
                peer = %peer_name,
                node_id = %peer_id,
                outcome = "reply-rejected",
                "gossip: the peer's REPLY did not authorize — nothing merged"
            );
            continue;
        }
        reached += 1;
        for observed in report.observed() {
            note_contact(&daemon.contacts, *observed, now).await;
        }
        if let Some(from) = parsed.from {
            note_contact(&daemon.contacts, from, now).await;
        }
        note_contact(&daemon.contacts, peer_id, now).await;
        tracing::info!(
            target: "gossip",
            round,
            peer = %peer_name,
            node_id = %peer_id,
            via = %ep.label,
            outcome = "reached",
            added = report.added(),
            updated = report.updated(),
            "gossip: round complete"
        );
    }

    let mesh = daemon.mesh.read().await;
    tracing::debug!(
        target: "gossip",
        round,
        reached,
        members = mesh.members.len(),
        "gossip: round summary"
    );
    if let Err(e) = identity::save_mesh(&daemon.node.data_dir, &mesh) {
        tracing::warn!(target: "rails", error = %e, "gossip: could not persist the mesh");
    }
}

/// Step 1. Our own row is the only one this node may author.
pub fn self_stamp(
    mesh: &mut Mesh,
    self_id: NodeId,
    now: u64,
    dial: &DialInfo,
    origins: &[OriginKind],
    key: &ed25519_dalek::SigningKey,
) {
    let pubkey = commonwealth_transport::identity::node_pubkey(key);
    let Some(me) = mesh.members.get_mut(&self_id) else {
        tracing::warn!(
            target: "gossip",
            self_id = %self_id,
            "gossip: this node is not on its own roster — nothing to stamp"
        );
        return;
    };
    me.last_seen = now;
    me.status = NodeStatus::Online;
    me.node_pubkey = Some(pubkey);
    me.capabilities = minimal_capabilities(now, origins);
    let changed = me.relay_url != dial.relay_url || me.iroh_direct_addrs != dial.direct_addrs;
    me.relay_url = dial.relay_url.clone();
    me.iroh_direct_addrs = dial.direct_addrs.clone();
    if let Some(version) =
        next_dial_version(me.dial_info_version, me.dial_info_sig.is_some(), changed)
    {
        me.dial_info_sig = Some(commonwealth_transport::identity::sign_dial_info(
            key,
            version,
            me.relay_url.as_deref(),
            &me.iroh_direct_addrs,
        ));
        me.dial_info_version = version;
        tracing::debug!(
            target: "gossip",
            version,
            relay = ?me.relay_url,
            direct = me.iroh_direct_addrs.len(),
            "gossip: re-signed our dial info"
        );
    }
}

/// Step 2. Only Online → Offline; the reverse transition is observed where a
/// peer's heartbeat is merged.
pub fn decay(
    mesh: &mut Mesh,
    self_id: NodeId,
    now: u64,
    threshold: u64,
    contacts: &std::collections::HashMap<NodeId, u64>,
) {
    for (id, m) in mesh.members.iter_mut() {
        if *id == self_id || m.status == NodeStatus::Offline {
            continue;
        }
        // A peer we have never contacted is given a full grace window from
        // now, so a member learned this round is never decayed on the next.
        let Some(last) = contacts.get(id) else {
            continue;
        };
        let staleness = now.saturating_sub(*last);
        if staleness > threshold {
            m.status = NodeStatus::Offline;
            tracing::info!(
                target: "gossip",
                peer = %m.name,
                node_id = %m.node_id,
                staleness_secs = staleness,
                threshold_secs = threshold,
                "gossip: peer marked Offline (no local contact within threshold)"
            );
        }
    }
}

/// Step 3's pick: active members with a key, other than us, online first,
/// rotated by round so a mesh larger than [`PEERS_PER_ROUND`] still converges.
pub fn select_peers(
    mesh: &Mesh,
    self_id: NodeId,
    round: u64,
) -> Vec<(NodeId, String, PeerContact)> {
    let mut all: Vec<_> = mesh
        .members
        .values()
        .filter(|m| m.node_id != self_id && m.is_active() && m.node_pubkey.is_some())
        .collect();
    // Deterministic order first (a HashMap's iteration order is not one), so
    // the rotation below actually rotates rather than reshuffling.
    all.sort_by_key(|m| m.node_id);
    all.sort_by_key(|m| m.status != NodeStatus::Online);
    if all.is_empty() {
        return Vec::new();
    }
    let start = (round as usize) % all.len();
    all.iter()
        .cycle()
        .skip(start)
        .take(all.len().min(PEERS_PER_ROUND))
        .map(|m| (m.node_id, m.name.clone(), peer_contact(m)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::mesh::MemberRecord;
    use std::collections::HashMap;

    fn member(id: u128, name: &str, status: NodeStatus, keyed: bool) -> MemberRecord {
        MemberRecord {
            node_id: NodeId::from_u128(id),
            name: name.into(),
            invited_by: NodeId::from_u128(1),
            joined_at: 0,
            last_seen: 0,
            status,
            capabilities: minimal_capabilities(0, &[]),
            addresses: Vec::new(),
            node_pubkey: keyed.then(|| commonwealth_core::ids::NodePubkey([id as u8; 32])),
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            removed_at: None,
        }
    }

    fn mesh_of(records: Vec<MemberRecord>) -> Mesh {
        let (mut mesh, _k) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        mesh.members.clear();
        for r in records {
            mesh.members.insert(r.node_id, r);
        }
        mesh
    }

    const ME: u128 = 0xA11CE;

    fn key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[3u8; 32])
    }

    /// **The failing input for the self-stamp.** A version that moves every
    /// round is not an anti-rollback key, it is a clock — and a peer holding
    /// our record would re-verify a signature on every heartbeat forever. The
    /// bump happens on a CONTENT change and once to acquire a first
    /// signature, and not otherwise.
    #[test]
    fn the_dial_version_bumps_on_a_change_and_stands_still_otherwise() {
        assert_eq!(
            next_dial_version(0, false, false),
            Some(1),
            "first signature"
        );
        assert_eq!(next_dial_version(4, true, false), None, "no change, signed");
        assert_eq!(next_dial_version(4, true, true), Some(5), "relay moved");
        assert_eq!(next_dial_version(0, false, true), Some(1));
    }

    /// The same property through the real stamp: two rounds with identical
    /// reachability sign once.
    #[test]
    fn two_rounds_with_the_same_reachability_sign_once() {
        let mut mesh = mesh_of(vec![member(ME, "me", NodeStatus::Offline, false)]);
        let dial = DialInfo {
            relay_url: Some("https://relay.example/".into()),
            direct_addrs: vec!["192.168.1.8:41231".parse().unwrap()],
        };
        self_stamp(&mut mesh, NodeId::from_u128(ME), 100, &dial, &[], &key());
        let after_first = mesh.members[&NodeId::from_u128(ME)].clone();
        assert_eq!(after_first.dial_info_version, 1);
        assert!(after_first.dial_info_sig.is_some());
        assert_eq!(after_first.status, NodeStatus::Online);

        self_stamp(&mut mesh, NodeId::from_u128(ME), 110, &dial, &[], &key());
        let after_second = &mesh.members[&NodeId::from_u128(ME)];
        assert_eq!(after_second.dial_info_version, 1, "no content change");
        assert_eq!(after_second.dial_info_sig, after_first.dial_info_sig);
        assert_eq!(after_second.last_seen, 110, "the heartbeat still advances");

        let moved = DialInfo {
            relay_url: Some("https://other.example/".into()),
            ..dial.clone()
        };
        self_stamp(&mut mesh, NodeId::from_u128(ME), 120, &moved, &[], &key());
        assert_eq!(mesh.members[&NodeId::from_u128(ME)].dial_info_version, 2);
    }

    /// The catalogue's input. A configured origin is what puts `Media` on the
    /// wire; without one the field is empty, which reads to every peer as
    /// "advertises none" — absence reported, never defaulted to an offer.
    #[test]
    fn a_configured_origin_is_what_stamps_the_media_kind() {
        let mut mesh = mesh_of(vec![member(ME, "me", NodeStatus::Online, false)]);
        let dial = DialInfo {
            relay_url: None,
            direct_addrs: Vec::new(),
        };
        self_stamp(&mut mesh, NodeId::from_u128(ME), 1, &dial, &[], &key());
        assert!(mesh.members[&NodeId::from_u128(ME)]
            .capabilities
            .origins
            .is_empty());
        self_stamp(
            &mut mesh,
            NodeId::from_u128(ME),
            2,
            &dial,
            &[OriginKind::Media],
            &key(),
        );
        assert_eq!(
            mesh.members[&NodeId::from_u128(ME)].capabilities.origins,
            vec![OriginKind::Media]
        );
    }

    /// **The failing input for decay.** Staleness is our clock against the
    /// contact map. A peer we have never contacted has no entry, and decaying
    /// it would mark a member Offline the round after we learned of it —
    /// which is how a freshly-joined member disappears from `mesh media`
    /// before it has ever been dialed.
    #[test]
    fn a_peer_never_contacted_is_not_decayed_and_a_stale_one_is() {
        let mut mesh = mesh_of(vec![
            member(ME, "me", NodeStatus::Online, false),
            member(0xB0B, "LittleMac", NodeStatus::Online, true),
            member(0xC0DE, "Quiet", NodeStatus::Online, true),
        ]);
        let mut contacts = HashMap::new();
        contacts.insert(NodeId::from_u128(0xB0B), 100u64);
        decay(&mut mesh, NodeId::from_u128(ME), 1000, 60, &contacts);
        assert_eq!(
            mesh.members[&NodeId::from_u128(0xB0B)].status,
            NodeStatus::Offline,
            "900s of silence against a 60s threshold"
        );
        assert_eq!(
            mesh.members[&NodeId::from_u128(0xC0DE)].status,
            NodeStatus::Online,
            "never contacted is not the same fact as gone"
        );
        assert_eq!(
            mesh.members[&NodeId::from_u128(ME)].status,
            NodeStatus::Online,
            "a node never decays itself"
        );
    }

    /// Online first, keyed only, self never — and the rotation reaches every
    /// member of a mesh bigger than the fan.
    #[test]
    fn the_peer_pick_is_online_first_keyed_and_rotates() {
        let mesh = mesh_of(vec![
            member(ME, "me", NodeStatus::Online, true),
            member(0x01, "a", NodeStatus::Online, true),
            member(0x02, "b", NodeStatus::Offline, true),
            member(0x03, "c", NodeStatus::Online, true),
            member(0x04, "d", NodeStatus::Online, false),
            member(0x05, "e", NodeStatus::Online, true),
        ]);
        let me = NodeId::from_u128(ME);
        let first = select_peers(&mesh, me, 0);
        assert_eq!(first.len(), PEERS_PER_ROUND);
        let names: Vec<&str> = first.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "c", "e"], "online first, keyed only");
        assert!(!first.iter().any(|(id, _, _)| *id == me));
        assert!(
            !names.contains(&"d"),
            "a member with no key has nothing to dial by"
        );

        // Over four rounds every candidate — the offline one included — is
        // dialed at least once, which is what keeps a bigger mesh converging.
        let mut seen: Vec<String> = Vec::new();
        for round in 0..4 {
            for (_, name, _) in select_peers(&mesh, me, round) {
                if !seen.contains(&name) {
                    seen.push(name);
                }
            }
        }
        seen.sort();
        assert_eq!(seen, vec!["a", "b", "c", "e"]);
    }

    #[test]
    fn a_mesh_of_one_has_nobody_to_dial() {
        let mesh = mesh_of(vec![member(ME, "me", NodeStatus::Online, true)]);
        assert!(select_peers(&mesh, NodeId::from_u128(ME), 0).is_empty());
    }
}
