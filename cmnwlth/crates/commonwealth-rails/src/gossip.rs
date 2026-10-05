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

mod merge;
pub(crate) use merge::merge_round;
pub use merge::{split_generation_of, SplitGenerations};

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
pub fn minimal_capabilities(
    now: u64,
    origins: &[OriginKind],
    media_available: Option<f32>,
) -> NodeCapabilities {
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
        media_allow: Vec::new(),
        // The presence poll's last reading — `None` is "nobody answered",
        // never "free" (`presence`).
        media_available,
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: None,
        storage_remaining_bytes: None,
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
        if daemon.is_solo() {
            tracing::debug!(target: "gossip", round, "gossip: solo — no mesh, no round");
        } else {
            run_one_round(&daemon, round).await;
        }
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

    let dial = live_dial(daemon);
    // Exactly the origin kinds whose ALPN a registration serves (media from
    // `rails.toml`, apps while published, any program's offer), and what
    // every live registration declares — never a guess of cw-rails' own.
    let origins = daemon.origins.advertised_kinds();
    let declared = daemon.origins.declared_claims();
    // The node's own hardware, read once a registrant declares (a bare
    // endpoint takes no job and keeps its zeroed report). Off the runtime:
    // the detector walks disks and may spawn `nvidia-smi`.
    let measured = if declared.is_empty() {
        None
    } else {
        match tokio::task::spawn_blocking(crate::self_measure::SelfMeasurement::now).await {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(target: "gossip", round, error = %e,
                               "gossip: the hardware reading failed; this round advertises no hardware");
                None
            }
        }
    };

    // Step 1 + 2 + 3's selection, in ONE write-lock window. Nothing awaits a
    // network inside it. The presence reading is read here (never inside the
    // lock — the cell is a plain std lock held for the copy only).
    let media_available = *daemon
        .media_presence
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let targets: Vec<(NodeId, String, PeerContact)> = {
        let mut mesh = daemon.mesh.write().await;
        self_stamp(
            &mut mesh,
            self_id,
            now,
            &dial,
            &origins,
            &daemon.node.key,
            media_available,
        );
        stamp_media_allow(&mut mesh, self_id, &daemon.media());
        // Absent self is already warned by `self_stamp`.
        if let Some(me) = mesh.members.get_mut(&self_id) {
            crate::origins::merge_declared(&mut me.capabilities, measured.as_ref(), &declared);
            tracing::debug!(
                target: "gossip",
                round,
                origins = ?me.capabilities.origins,
                declarations = declared.len(),
                inference_capable = me.capabilities.inference_capable,
                "gossip: stamped what the registered origins declare"
            );
        }
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
        if exchange(daemon, &http, round, now, peer_id, &peer_name, &contact).await {
            reached += 1;
        }
    }

    // Under the verbs lock: a `leave` or `switch` that landed during this
    // round has already replaced the mesh, and a solo one is never written.
    let _verbs = daemon.verbs.lock().await;
    let mesh = daemon.mesh.read().await;
    tracing::debug!(
        target: "gossip",
        round,
        reached,
        members = mesh.members.len(),
        "gossip: round summary"
    );
    if daemon.is_solo() {
        tracing::debug!(target: "gossip", round, "gossip: the node went solo mid-round — nothing written");
        return;
    }
    if let Err(e) = identity::save_mesh(&daemon.node.data_dir, &mesh) {
        tracing::warn!(target: "rails", error = %e, "gossip: could not persist the mesh");
    }
}

/// The relay and direct addresses the daemon's endpoint holds right now —
/// what step 1 writes into our row.
fn live_dial(daemon: &RailsDaemon) -> DialInfo {
    let addr = daemon.endpoint().addr();
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
}

/// Step 1 on a mesh that is about to become the active one, before anyone can
/// read it ([`RailsDaemon::swap_membership`]). Returns the dial it stamped.
///
/// A row is otherwise stamped only by the round, up to `gossip_interval_secs`
/// after the mesh went live, and a joiner admitted inside that window leaves
/// with a founder row that carries no address — its only route back, since
/// the invite's dial string is not kept and a joiner may not author another
/// node's row. With no relay and no discovery (local-only) neither side could
/// ever dial the other (2026-10-03: a join 0 s after `POST /v1/mesh/create`
/// gossiped `no-addresses` on both nodes forever; one 12 s later converged).
///
/// The hardware the round merges is left to the next round: a mesh just made
/// active has no measurement owed yet.
pub(crate) fn stamp_before_publishing(daemon: &RailsDaemon, mesh: &mut Mesh, now: u64) -> DialInfo {
    let dial = live_dial(daemon);
    let origins = daemon.origins.advertised_kinds();
    let media_available = *daemon
        .media_presence
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    self_stamp(
        mesh,
        daemon.node.self_id,
        now,
        &dial,
        &origins,
        &daemon.node.key,
        media_available,
    );
    stamp_media_allow(mesh, daemon.node.self_id, &daemon.media());
    dial
}

/// The members `[media] allow` admits, on our own row: what a viewer's rail
/// shows as "offered to" (`commonwealth_core::mesh::offer_view`), and the
/// same list the acceptor enforces (`origins::stand_media`). Empty is every
/// member; with no origin offered there is nothing to narrow.
/// [`minimal_capabilities`] leaves it empty, so a round that skipped this
/// told every viewer a narrowed offer was open to all.
pub(crate) fn stamp_media_allow(
    mesh: &mut Mesh,
    self_id: NodeId,
    media: &crate::config::MediaSection,
) {
    if let Some(me) = mesh.members.get_mut(&self_id) {
        me.capabilities.media_allow = match media.origin {
            Some(_) => media.allow.clone(),
            None => Vec::new(),
        };
        // Absent self is already warned by `self_stamp`.
        tracing::debug!(target: "gossip", media_origin = ?media.origin,
                        media_allow = ?me.capabilities.media_allow,
                        "gossip: our row carries the media narrowing");
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
    media_available: Option<f32>,
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
    // Strictly newer than our row as it stands, as `announce_departure`
    // writes it: a departure in this same second pushes our event time past
    // the wall clock, and a stamp that wrote `now` under it moved the row
    // backwards — a tombstone sent next then landed at or below the copy the
    // founder held, which its merge kept (watched 2026-10-03: switch, then
    // leave, inside one second once a mesh going live was stamped).
    me.last_seen = now.max(me.event_time() + 1);
    me.status = NodeStatus::Online;
    me.node_pubkey = Some(pubkey);
    me.capabilities = minimal_capabilities(now, origins, media_available);
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

/// Tell the online members this node is stepping out of the active mesh:
/// `left` tombstones our row mesh-wide (a leave); otherwise it reads offline,
/// because a switch parks the mesh and means to come back. The inference
/// daemon's `announce_presence_change`, over this process's one exchange.
/// Best-effort: a peer that misses it decays our row on its own threshold.
/// Returns how many peers took it.
pub async fn announce_departure(daemon: &RailsDaemon, left: bool) -> usize {
    let self_id = daemon.node.self_id;
    let now = unix_now_secs();
    let targets: Vec<(NodeId, String, PeerContact)> = {
        let mut mesh = daemon.mesh.write().await;
        if let Some(me) = mesh.members.get_mut(&self_id) {
            // STRICTLY newer than any copy of our row a peer can hold: a merge
            // keeps the existing row on an equal event time, and a round or an
            // admission in this same second already stamped `now` (watched:
            // join, switch and leave inside one second left the founder
            // holding the member online).
            let event = now.max(me.event_time() + 1);
            if left {
                me.removed_at = Some(event);
            }
            me.status = NodeStatus::Offline;
            me.last_seen = event;
        }
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id && m.is_active() && m.node_pubkey.is_some())
            .filter(|m| m.status == NodeStatus::Online)
            .map(|m| (m.node_id, m.name.clone(), peer_contact(m)))
            .collect()
    };
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "gossip", error = %e, "gossip: no HTTP client — departure not announced");
            return 0;
        }
    };
    let mut told = 0usize;
    for (peer_id, peer_name, contact) in &targets {
        // Not a numbered round: the log field says which exchange this was.
        if exchange(daemon, &http, u64::MAX, now, *peer_id, peer_name, contact).await {
            told += 1;
        }
    }
    tracing::info!(target: "gossip", left, told, online = targets.len(),
                   "gossip: announced this node's departure from the active mesh");
    told
}

/// One exchange with one peer: POST our snapshot, merge the proven reply.
/// `true` when the peer answered and its reply merged.
async fn exchange(
    daemon: &RailsDaemon,
    http: &reqwest::Client,
    round: u64,
    now: u64,
    peer_id: NodeId,
    peer_name: &str,
    contact: &PeerContact,
) -> bool {
    let self_id = daemon.node.self_id;
    let endpoints = daemon
        .transport()
        .endpoints(contact, TrafficClass::Gossip)
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
        return false;
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
            return false;
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
        return false;
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
            return false;
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
        merge_round(
            &mut mesh,
            self_id,
            &incoming,
            &auth,
            Some(peer_id),
            &daemon.split_generation,
            &daemon.ring_nudge,
        )
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
        return false;
    }
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
    true
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
mod tests;
