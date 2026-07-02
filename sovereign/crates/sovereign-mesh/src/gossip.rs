// SPDX-License-Identifier: AGPL-3.0-or-later
//! Member-list gossip — the thing that keeps two peers' views of
//! the mesh converged after the initial join.
//!
//! Model: anti-entropy push-pull over plain HTTP on port 9742. Every
//! `interval` (default 10s) we pick up to `FANOUT` random members
//! and POST our current `Mesh` to their `/internal/gossip`; they
//! merge it into theirs and reply with their (now-updated) snapshot
//! which we then merge in. Convergence in one round per pair.
//!
//! Two side effects every round:
//! 1. Our own `last_seen` is bumped to `now()` so peers learn we're
//!    still here and don't decay us to Offline.
//! 2. Members whose `last_seen` is older than `offline_threshold`
//!    are marked `NodeStatus::Offline` locally — the mechanism that
//!    turns "the founder closed their laptop" from a silent stale
//!    member list into a visible offline indicator.
//!
//! Reuses `Mesh::merge_from` for the actual last-writer-wins
//! reconciliation. This module is just the network plumbing on top.
use std::time::{Duration, Instant};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::MeshId;
use commonwealth_core::mesh::{MemberRecord, Mesh, MeshPeering, NodeStatus};
use commonwealth_transport::{peer_contact, PeerContact, TrafficClass};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::capabilities::build_local_capabilities;

// Address ordering and the last-working-address promotion both live
// in the PeerTransport seam now (`IpTransport` in
// commonwealth-transport) — this module used to carry a
// process-global `last_working_address_cache`; the transport on
// `AppState` has the same effective lifetime (one per daemon run)
// and shares the hint with every other traffic class.

/// Default: send to at most this many peers per round. Small mesh
/// sizes make higher fan-out pointless; bandwidth is negligible at
/// 2 even with full-snapshot gossip.
const FANOUT: usize = 2;

/// Hard per-peer HTTP timeout. Mirrors `sovereign-mesh::join` so
/// slow/unreachable peers don't drag out a gossip round.
const PEER_TIMEOUT: Duration = Duration::from_secs(3);

/// After this long without a successful gossip contact, a peer is
/// marked Offline. Needs to be >> `interval` so a single missed
/// round doesn't flap peers offline — roughly 6× the interval is
/// a reasonable default.
pub const DEFAULT_OFFLINE_THRESHOLD: Duration = Duration::from_secs(60);

/// Default gossip cadence. Chosen to match the UI's 5s poll
/// comfortably (UI sees converged state within ~2× the cadence).
pub const DEFAULT_GOSSIP_INTERVAL: Duration = Duration::from_secs(10);

/// Handle to the spawned gossip task. Aborts the task when dropped
/// (mirrors `commonwealth_discovery::mdns::BrowseHandle`). The
/// `DaemonState::Running` variant holds one of these so stopping
/// the daemon cleanly tears down the gossip loop along with mDNS.
pub struct GossipHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for GossipHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// Spawn the periodic gossip task. Call once per daemon start.
///
/// `persist_dir` is the directory containing `mesh.json`. When
/// provided, every round re-persists the current mesh snapshot so
/// that mutations from any source — the `/internal/join` handler,
/// `merge_from` via gossip, `last_seen` bumps, status decays —
/// survive a daemon restart without needing a per-handler persist
/// callback. Costs one JSON file write per 10s (trivial). `None`
/// (test harnesses, CLI without persistence) skips persistence.
pub fn spawn_gossip_loop(
    app_state: AppState,
    interval: Duration,
    offline_threshold: Duration,
    persist_dir: Option<std::path::PathBuf>,
) -> GossipHandle {
    let task = tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            offline_threshold_secs = offline_threshold.as_secs(),
            persistence = persist_dir.is_some(),
            "gossip: loop started"
        );
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_one_round(&app_state, offline_threshold).await {
                warn!(error = %e, "gossip: round errored");
            }
            if let Some(dir) = persist_dir.as_deref() {
                let mesh = app_state.inner.mesh.read().await.clone();
                let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
                if let Err(e) = crate::persist::save(dir, &mesh, self_id) {
                    // Don't spam — persistence failure is rarely
                    // fatal to the running session, but the operator
                    // should know their mesh won't survive restart.
                    warn!(
                        error = %e,
                        "gossip: mesh.json re-persist failed"
                    );
                }
            }
        }
    });
    GossipHandle { _task: task }
}

/// Fire a single gossip round immediately — used as a "fast initial
/// sync" trigger right after the daemon starts so a restart doesn't
/// wait a full interval before reconciling with peers. Bounded by
/// `max_duration` so daemon startup stays prompt even when all
/// peers are unreachable.
pub async fn initial_sync(
    app_state: &AppState,
    offline_threshold: Duration,
    max_duration: Duration,
) {
    match tokio::time::timeout(max_duration, run_one_round(app_state, offline_threshold)).await {
        Ok(Ok(())) => {
            debug!("gossip: initial_sync completed");
        }
        Ok(Err(e)) => warn!(error = %e, "gossip: initial_sync errored"),
        Err(_) => {
            debug!(
                max_ms = max_duration.as_millis() as u64,
                "gossip: initial_sync timed out — continuing startup"
            );
        }
    }
}

/// One full gossip round. Touches own `last_seen`, decays stale
/// peers, then pair-gossips with up to `FANOUT` random members.
pub async fn run_one_round(
    app_state: &AppState,
    offline_threshold: Duration,
) -> Result<(), GossipError> {
    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    let now = app_state.clock().now_unix_secs();
    let threshold = offline_threshold.as_secs();

    // Build a fresh snapshot of our own capabilities BEFORE we take
    // the mesh write lock — `installed_indexes()` awaits a directory
    // read, and we don't want to pin the lock across that. The
    // engine is optional: test daemons and the CLI run without one.
    let availability = *app_state.inner.local_inference_availability.read().await;
    // Pull the live embed model from the inference store. This is
    // what `daemon::start_daemon` publishes after the fast slot
    // probes the GGUF. `None` on fresh daemons / pure-storage nodes;
    // the planner treats that as "don't include me in distribution".
    let embed_model = app_state.inner.inference_store.get_local_embed_model();
    let fresh_caps = build_local_capabilities(
        app_state.inner.corpus_engine.as_ref(),
        now,
        availability,
        embed_model,
        Some(app_state),
    )
    .await;
    // Step 1: touch self + decay stale peers. One write-lock window.
    // Compare current vs. fresh hosted_corpora so we can log at
    // info only when the advertised set changed (new corpus
    // installed, one removed) — the every-10s heartbeat otherwise
    // logs at debug. Same gating policy as `mesh_state: rebuilt`.
    let candidates: Vec<PeerContact> = {
        let mut mesh = app_state.inner.mesh.write().await;
        let prior_corpora: std::collections::BTreeSet<String> = mesh
            .members
            .get(&self_id)
            .map(|m| {
                m.capabilities
                    .hosted_corpora
                    .iter()
                    .map(|c| c.corpus_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let fresh_corpora: std::collections::BTreeSet<String> = fresh_caps
            .hosted_corpora
            .iter()
            .map(|c| c.corpus_id.clone())
            .collect();
        if fresh_corpora != prior_corpora {
            tracing::info!(
                hosted_corpora = ?fresh_corpora,
                system_ram_gb = fresh_caps.hardware.system_ram_gb,
                "gossip: hosted_corpora set changed — re-publishing"
            );
        } else {
            tracing::debug!(
                hosted_corpora = ?fresh_corpora,
                "gossip: publishing (unchanged)"
            );
        }
        if let Some(me) = mesh.members.get_mut(&self_id) {
            me.last_seen = now;
            me.status = NodeStatus::Online;
            // Replace capabilities with the freshly-sampled version
            // every round. This is the mechanism by which a newly-
            // installed SEP corpus becomes visible to peers within
            // one gossip interval — without it, `hosted_corpora`
            // stays frozen at whatever it was when the daemon
            // started (typically empty, since the user hasn't yet
            // run the install).
            me.capabilities = fresh_caps;
            // Stamp our identity pubkey every round. This is how a
            // node that upgraded in place (mesh created/joined
            // before identity keys existed) publishes its key
            // without a rejoin — within one gossip interval the
            // whole mesh learns it.
            if let Some(pubkey) = app_state.self_node_pubkey() {
                me.node_pubkey = Some(pubkey);
            }
            // Stamp our LIVE iroh dial info every round (W2). Unlike
            // the immutable pubkey, relay + hole-punched addrs appear
            // and change after the endpoint binds, so we re-read the
            // provider each round — peers learn our current
            // reachability within one interval. With this + the
            // pubkey, "known member" == "dialable by key". A `None`
            // provider (iroh disabled) leaves these fields at their
            // default empty, so a non-iroh node publishes nothing here.
            if let Some(info) = app_state.self_iroh_dialinfo() {
                let changed =
                    me.relay_url != info.relay_url || me.iroh_direct_addrs != info.direct_addrs;
                me.relay_url = info.relay_url;
                me.iroh_direct_addrs = info.direct_addrs;
                // WS-D anti-downgrade: SIGN our dial info so peers can
                // verify only we changed it (a gossip-strip attacker past
                // the join-key gate can't force us unreachable / downgrade
                // us). Bump the monotonic version on a real content change
                // so a replayed older signed record loses the merge
                // version check. Only commit version + sig together when a
                // signer is installed (iroh on); else stay unsigned.
                if changed || me.dial_info_sig.is_none() {
                    let next_version = if changed {
                        me.dial_info_version.saturating_add(1).max(1)
                    } else {
                        me.dial_info_version.max(1)
                    };
                    if let Some(sig) = app_state.sign_dial_info(
                        next_version,
                        me.relay_url.as_deref(),
                        &me.iroh_direct_addrs,
                    ) {
                        me.dial_info_version = next_version;
                        me.dial_info_sig = Some(sig);
                    }
                }
            }
        }
        for (id, m) in mesh.members.iter_mut() {
            if *id == self_id {
                continue;
            }
            // Decay measures LOCAL-observation staleness — the local-clock
            // time at which we last saw this peer's record advance (set via
            // `observe_peer_contact` in the merge paths below + the receive
            // handler) — NOT the peer's own gossiped `last_seen`. Comparing a
            // remote clock against ours is what caused the "~9 min flap" (todo
            // `f152dfe7` #4): a clock-skewed-but-live peer looked stale.
            // `peer_contact_or_init` lazy-inits a freshly-seen peer to `now`, a
            // full grace window, so we never decay a peer we just learned of.
            let last_contact = app_state.peer_contact_or_init(*id, now);
            if now.saturating_sub(last_contact) > threshold && m.status != NodeStatus::Offline {
                m.status = NodeStatus::Offline;
                info!(
                    peer = %m.node_id,
                    name = %m.name,
                    staleness_secs = now.saturating_sub(last_contact),
                    threshold_secs = threshold,
                    last_contact_unix = last_contact,
                    addrs = ?m.addresses,
                    "gossip: peer marked Offline (no local contact within threshold)"
                );
            }
            // The symmetric offline→online transition is observed where we
            // merge a peer's heartbeat (below) — that refreshes `last_contact`
            // and flips status back to Online. The decay pass only moves
            // Online→Offline, so no online-transition log here.
        }
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id)
            // The transport sorts candidates IPv4-first on
            // resolution and promotes the last-working address,
            // so the contact carries the raw gossiped list.
            .map(peer_contact)
            .collect()
    };

    if candidates.is_empty() {
        // Solo mesh — nothing to do. Still valuable to have fired
        // the round so self's `last_seen` stays current for the
        // moment a peer does arrive.
        return Ok(());
    }

    // Step 2: pick up to FANOUT peers at random. Scope the RNG so
    // the non-Send `ThreadRng` doesn't cross an `.await` below —
    // spawned futures must be `Send` and `rand::rng()` isn't.
    let selection = {
        let mut rng = rand::rng();
        let mut tmp = candidates;
        tmp.shuffle(&mut rng);
        tmp.truncate(FANOUT);
        tmp
    };

    // Step 3: snapshot our mesh once and POST it to each picked
    // peer. Using the same snapshot across the fan-out keeps rounds
    // cheap and means every peer sees the same view of us.
    let my_snapshot = { app_state.inner.mesh.read().await.clone() };
    let http = reqwest::Client::builder()
        .timeout(PEER_TIMEOUT)
        .build()
        .map_err(|e| GossipError::ClientBuild(e.to_string()))?;

    let transport = app_state.peer_transport();
    for contact in selection {
        let peer_id = contact.node_id;
        // The transport resolves and orders candidates: the address
        // that worked last round goes first. The common case is
        // "Tailscale 100.x stable, LAN 192.168.x stale because the
        // Mac is on a different subnet from linux-peer" — without
        // that hint, every round burns `PEER_TIMEOUT` (3s) on the
        // dead LAN address before falling through to Tailscale.
        // Best-effort: a stale hint just slows down THIS round, and
        // the next success rewrites it.
        let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
        if endpoints.is_empty() {
            debug!(peer = %peer_id, "gossip: no addresses on record, skipping");
            continue;
        }
        for ep in &endpoints {
            // Per-address timing so we can diagnose the Online↔Offline
            // flap (see todo `f152dfe7` #4). Each line is one address
            // attempt with elapsed ms and outcome, so offline decay can
            // be correlated with a run of failed reaches on a specific
            // address family (LAN vs Tailscale).
            let attempt_start = Instant::now();
            match gossip_with_peer(&http, &ep.base_url, &my_snapshot).await {
                Ok(their_view) => {
                    let reach_ms = attempt_start.elapsed().as_millis() as u64;
                    info!(
                        peer = %peer_id,
                        peer_addr = %ep.label,
                        reach_ms,
                        "gossip: reach ok"
                    );
                    // Pin this endpoint as the preferred starting
                    // point for the next round's resolution.
                    transport.note_success(peer_id, TrafficClass::Gossip, ep);
                    let mut mesh = app_state.inner.mesh.write().await;
                    let report = mesh.merge_from(self_id, &their_view);
                    // Stamp local-observation time for every peer whose record
                    // advanced in this merge (incl. transitively-relayed ones),
                    // so offline-decay sees them as freshly-observed.
                    for observed_id in &report.observed {
                        app_state.observe_peer_contact(*observed_id, now);
                    }
                    if report.added > 0 {
                        info!(
                            peer = %peer_id,
                            peer_addr = %ep.label,
                            added = report.added,
                            updated = report.updated,
                            "gossip: member added from peer's view"
                        );
                    } else if report.updated > 0 {
                        tracing::debug!(
                            peer = %peer_id,
                            peer_addr = %ep.label,
                            updated = report.updated,
                            "gossip: merged peer's view (last_seen refresh)"
                        );
                    }
                    // Also bump THIS peer's last_seen in case their
                    // view of themselves lagged — we successfully
                    // reached them just now, so they're Online.
                    //
                    // Log the offline→online transition at INFO so
                    // the operator can see "B is back" without
                    // polling mesh_state() by hand. Symmetric to the
                    // offline-decay log in the pass above.
                    if let Some(peer) = mesh.members.get_mut(&peer_id) {
                        let was_offline = peer.status == NodeStatus::Offline;
                        peer.last_seen = app_state.clock().now_unix_secs();
                        peer.status = NodeStatus::Online;
                        if was_offline {
                            info!(
                                peer = %peer_id,
                                peer_addr = %ep.label,
                                name = %peer.name,
                                "gossip: peer back Online"
                            );
                        }
                    }
                    break; // one working address is enough
                }
                Err(e) => {
                    let reach_ms = attempt_start.elapsed().as_millis() as u64;
                    // Demoted to debug: a single failed address is
                    // expected on multi-homed peers (e.g. a stale LAN
                    // IP behind a working Tailscale address). The
                    // address-cache reorder above means this typically
                    // fires at most once per peer per process — after
                    // that, the working address goes first and the
                    // dead one is never tried again. If reachability
                    // truly breaks, every attempt fails and the peer
                    // decays to Offline via the `last_seen` threshold,
                    // which logs at INFO from the decay path.
                    debug!(
                        peer = %peer_id,
                        peer_addr = %ep.label,
                        reach_ms,
                        error = %e,
                        "gossip: reach failed, trying next address"
                    );
                    continue;
                }
            }
        }
    }

    // ── Step 4: mesh_store replication ──────────────────────────────
    //
    // The Mesh gossip above only syncs the member list. Entries in
    // `mesh_store` (queue-mode IngestionHandoffs, app-state blobs,
    // app manifests) need their own push — `/internal/app/state`
    // has been the receiver since before the work-queue work, but
    // nothing has ever been the *sender*. Without this step, a
    // coordinator-registered pull-based handoff stays invisible to
    // peers, their `discover_and_spawn_pull_loops` finds nothing to
    // scan, and the queue path silently does nothing despite both
    // nodes thinking they set it up.
    //
    // Payload: full snapshot (anti-entropy). LWW merge on the receiver
    // (`mesh_store::merge_entry`) makes duplicates cheap — the cost is
    // one POST per online peer per round, and at 10s cadence with a
    // handful of handoffs that's negligible.
    if let Ok(entries) = app_state.inner.mesh_store.all_entries_for_gossip() {
        if !entries.is_empty() {
            let wire_entries: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "app_id": e.app_id,
                        "key": e.key,
                        // `/internal/app/state`'s base64_decode stub
                        // passes the value through as raw UTF-8. All
                        // current mesh_store entries are JSON blobs
                        // (handoffs, app manifests) that round-trip
                        // cleanly through UTF-8.
                        "value_b64": String::from_utf8_lossy(&e.value).into_owned(),
                        "timestamp": e.timestamp,
                        "origin_hex": hex::encode(e.origin.as_bytes()),
                    })
                })
                .collect();
            let store_body = serde_json::json!({ "entries": wire_entries });

            // Re-read the peer list — the earlier loop consumed `selection`.
            let store_targets: Vec<PeerContact> = {
                let mesh = app_state.inner.mesh.read().await;
                mesh.members
                    .values()
                    .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
                    .map(peer_contact)
                    .collect()
            };

            for contact in store_targets {
                let peer_id = contact.node_id;
                let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
                for ep in &endpoints {
                    let url = format!("{}/internal/app/state", ep.base_url);
                    let push_start = Instant::now();
                    match http.post(&url).json(&store_body).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            let push_ms = push_start.elapsed().as_millis() as u64;
                            tracing::debug!(
                                peer = %peer_id,
                                url = %url,
                                push_ms,
                                entries = entries.len(),
                                "gossip: mesh_store pushed to peer"
                            );
                            break; // one working address is enough
                        }
                        Ok(resp) => {
                            let push_ms = push_start.elapsed().as_millis() as u64;
                            tracing::debug!(
                                peer = %peer_id,
                                url = %url,
                                push_ms,
                                status = %resp.status(),
                                "gossip: mesh_store push rejected"
                            );
                            break;
                        }
                        Err(e) => {
                            let push_ms = push_start.elapsed().as_millis() as u64;
                            tracing::debug!(
                                peer = %peer_id,
                                url = %url,
                                push_ms,
                                error = %e,
                                "gossip: mesh_store push failed, trying next address"
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Announce graceful departure: tombstone our own `MemberRecord` and push the
/// snapshot to every online peer once, so they remove us mesh-wide instead of
/// re-gossiping our stale live record forever (the immortal-ghost bug). The
/// event-time LWW in `Mesh::merge_from` makes the tombstone out-compete a peer's
/// live copy of us — our `removed_at`/`last_seen` are stamped at departure,
/// strictly later than any peer's last-seen-of-us — and peers that receive it
/// re-gossip it onward, so it converges even to peers we couldn't reach
/// directly. Best-effort; called from `EmbeddedDaemon::leave` before teardown.
pub async fn announce_departure(app_state: &AppState) {
    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    let now = app_state.clock().now_unix_secs();
    let (snapshot, targets) = {
        let mut mesh = app_state.inner.mesh.write().await;
        if let Some(me) = mesh.members.get_mut(&self_id) {
            me.removed_at = Some(now);
            me.status = NodeStatus::Offline;
            me.last_seen = now; // event_time(self) = now, beating peers' stale copies
        }
        let targets: Vec<PeerContact> = mesh
            .members
            .values()
            .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online && m.is_active())
            .map(peer_contact)
            .collect();
        (mesh.clone(), targets)
    };
    let http = match reqwest::Client::builder().timeout(PEER_TIMEOUT).build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let transport = app_state.peer_transport();
    let mut announced = 0usize;
    for contact in &targets {
        let eps = transport.endpoints(contact, TrafficClass::Gossip).await;
        for ep in &eps {
            if gossip_with_peer(&http, &ep.base_url, &snapshot)
                .await
                .is_ok()
            {
                announced += 1;
                break;
            }
        }
    }
    info!(
        self_id = %self_id,
        peers = targets.len(),
        announced,
        "gossip: announced departure (self-tombstone pushed to online peers)"
    );
}

/// Fire-and-forget broadcast of a single mesh_store entry to every
/// online peer. Used by latency-sensitive writers (e.g. the work
/// atlas's `declare_scope`) that need a claim visible across the
/// mesh in the same round-trip rather than waiting up to one full
/// `DEFAULT_GOSSIP_INTERVAL` for the next anti-entropy round.
///
/// Best-effort: unreachable peers are logged at `warn` (one line per
/// failure) and skipped. The next gossip round will pick the entry
/// up via the normal anti-entropy path anyway, so a transient peer
/// outage doesn't lose the write.
///
/// Privacy: the caller is responsible for ensuring `app_id` is not
/// in `GOSSIP_EXCLUDED_APP_IDS`. The store's own `set` already
/// permits writes to excluded namespaces (the exclusion happens at
/// the gossip-read boundary, not the write boundary), so a sloppy
/// caller could in principle broadcast a Private record. The work
/// atlas's typed facade only calls `broadcast_now` for Public claims
/// — Private claims skip this entirely.
pub async fn broadcast_now(app_state: &AppState, app_id: &str, key: &str) {
    if commonwealth_state::is_gossip_excluded(app_id) {
        // Defence-in-depth: even if a caller passed a private app_id,
        // refuse to broadcast it. This is the third privacy layer
        // for the work atlas — store-level mapping + gossip filter +
        // this guard.
        tracing::warn!(app_id, "work_atlas:broadcast_now refused private app_id");
        return;
    }

    let entry = match app_state.inner.mesh_store.get(app_id, key) {
        Ok(Some(e)) => e,
        Ok(None) => {
            tracing::debug!(app_id, key, "broadcast_now: no entry");
            return;
        }
        Err(e) => {
            tracing::warn!(app_id, key, error = %e, "broadcast_now: store read failed");
            return;
        }
    };

    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    let wire = serde_json::json!({
        "entries": [{
            "app_id": entry.app_id,
            "key": entry.key,
            "value_b64": String::from_utf8_lossy(&entry.value).into_owned(),
            "timestamp": entry.timestamp,
            "origin_hex": hex::encode(entry.origin.as_bytes()),
        }]
    });

    let targets: Vec<PeerContact> = {
        let mesh = app_state.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
            .map(peer_contact)
            .collect()
    };

    if targets.is_empty() {
        return;
    }

    let http = match reqwest::Client::builder().timeout(PEER_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "broadcast_now: client build failed");
            return;
        }
    };

    // Fan out concurrently — claim writers shouldn't pay
    // serial latency for slow peers.
    let transport = app_state.peer_transport();
    let mut handles = Vec::with_capacity(targets.len());
    for contact in targets {
        let peer_id = contact.node_id;
        let http = http.clone();
        let body = wire.clone();
        let endpoints = transport.endpoints(&contact, TrafficClass::Gossip).await;
        handles.push(tokio::spawn(async move {
            for ep in endpoints {
                let url = format!("{}/internal/app/state", ep.base_url);
                match http.post(&url).json(&body).send().await {
                    Ok(resp) if resp.status().is_success() => return,
                    Ok(resp) => {
                        tracing::debug!(
                            peer = %peer_id,
                            url = %url,
                            status = %resp.status(),
                            "work_atlas:broadcast_now peer rejected"
                        );
                    }
                    Err(e) => {
                        tracing::debug!(
                            peer = %peer_id,
                            url = %url,
                            error = %e,
                            "work_atlas:broadcast_now peer unreachable, trying next addr"
                        );
                    }
                }
            }
            tracing::warn!(
                peer = %peer_id,
                "work_atlas:broadcast_now_failed (all addrs exhausted)"
            );
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

async fn gossip_with_peer(
    http: &reqwest::Client,
    base_url: &str,
    my_view: &Mesh,
) -> Result<Mesh, GossipError> {
    let body = GossipRequestWire {
        mesh: MeshWire::from(my_view),
    };
    let url = format!("{base_url}/internal/gossip");
    let response = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| GossipError::Transport(e.to_string()))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(GossipError::Rejected);
    }
    if !response.status().is_success() {
        return Err(GossipError::Transport(format!(
            "unexpected status {}",
            response.status()
        )));
    }

    let parsed: GossipResponseWire = response
        .json()
        .await
        .map_err(|e| GossipError::BadResponse(e.to_string()))?;
    Ok(parsed.mesh.into_mesh())
}

#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(String),
    #[error("peer rejected gossip (wrong mesh or key)")]
    Rejected,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("malformed peer response: {0}")]
    BadResponse(String),
}

// ── Wire types ───────────────────────────────────────────────
//
// Mirror of `commonwealth_api::routes_internal::{GossipRequest,
// GossipResponse, MeshWire}`. Duplicated here (like `join::MeshWire`)
// because the server-side type isn't re-exported and projecting
// HashMap<NodeId, MemberRecord> → Vec<MemberRecord> for serde is
// the whole reason MeshWire exists.

#[derive(Debug, Serialize)]
struct GossipRequestWire {
    mesh: MeshWire,
}

#[derive(Debug, Deserialize)]
struct GossipResponseWire {
    mesh: MeshWire,
}

#[derive(Debug, Serialize, Deserialize)]
struct MeshWire {
    id: MeshId,
    name: String,
    join_key_hash: [u8; 32],
    #[serde(default)]
    require_encryption: bool,
    members: Vec<MemberRecord>,
    peers: Vec<MeshPeering>,
}

impl From<&Mesh> for MeshWire {
    fn from(m: &Mesh) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            join_key_hash: m.join_key_hash,
            require_encryption: m.require_encryption,
            members: m.members.values().cloned().collect(),
            peers: m.peers.clone(),
        }
    }
}

impl MeshWire {
    fn into_mesh(self) -> Mesh {
        use std::collections::HashMap;
        let members = self
            .members
            .into_iter()
            .map(|m| (m.node_id, m))
            .collect::<HashMap<_, _>>();
        Mesh {
            id: self.id,
            name: self.name,
            join_key_hash: self.join_key_hash,
            require_encryption: self.require_encryption,
            members,
            peers: self.peers,
        }
    }
}
