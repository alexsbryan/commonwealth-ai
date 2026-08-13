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

/// The gossip HTTP client — built ONCE per process, shared by every
/// round and every peer.
///
/// WHY THIS IS NOT BUILT PER ROUND (fixed 2026-07-29). A
/// `reqwest::Client` owns its connection pool; a client built inside a
/// round is dropped with the round, taking the pool with it. Every
/// round therefore opened a *new* TCP connection to the peer's local
/// iroh bridge — and `HttpBridge::spawn` dials a **fresh QUIC
/// connection per accepted TCP connection**. So gossip paid a full
/// QUIC handshake to every peer on every round, forever, and never
/// benefited from an established path. The handshake is also the thing
/// that times out: a `dial failed … error=timed out` warning is one
/// round's handshake giving up, which is how selection-independent
/// staleness crept back in even after `select_round_peers` was made
/// deterministic.
///
/// Measured live, RuggedFox → BeefyMac over iroh on a healthy idle LAN
/// (raw TCP RTT to the same host: p50 6.9 ms), concurrent A/B across
/// one identical 3-minute window:
///
/// | | p50 | p90 | max | dial timeouts |
/// |---|---|---|---|---|
/// | fresh client per round | 189 ms | 1327 ms | 2273 ms | 2 |
/// | reused connection | 38.8 ms | 391 ms | 1227 ms | 0 |
///
/// A warm round reaches 6.4 ms — the raw LAN RTT — because it does no
/// handshake at all.
///
/// The build is fallible (TLS backend init), and deterministically so:
/// caching the failure is correct, not a lost retry.
fn gossip_client() -> Result<&'static reqwest::Client, &'static str> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(PEER_TIMEOUT)
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(String::as_str)
}

/// Warn once the mesh_store snapshot reaches half the receiver's body
/// limit. Derived from `MAX_REQUEST_BODY_BYTES`, never re-typed: the
/// number a sender warns against and the number a receiver enforces
/// must be the same number (§10.6).
///
/// WHY A GAUGE AT ALL. mesh_store replication is full-snapshot
/// anti-entropy, so the payload only grows. Two ceilings sit above it,
/// and both fail silently today: the receiver's 8 MiB body limit
/// (a 413 that this module logged at `debug`), and — nearer — the
/// shared client's 3s total POST timeout, which a multi-MB body over a
/// relay-class link trips well before 8 MiB
/// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.2). Half the limit is
/// the point where an operator still has room to act.
const MESH_STORE_PAYLOAD_WARN_BYTES: usize = commonwealth_api::server::MAX_REQUEST_BODY_BYTES / 2;

/// The outcome of one round's mesh_store push to one peer. A closed
/// set, so an enum — the whole point is that "rejected with a status"
/// and "never got a reply" are DIFFERENT failures with different
/// operator responses, and collapsing them into a bool would lose
/// exactly the distinction the rail exists to surface (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushOutcome {
    /// Some address accepted the snapshot.
    Ok,
    /// A peer answered with a non-success status (413 when the
    /// snapshot outgrew its body limit, 401/404 on a version skew).
    Rejected(u16),
    /// No address produced a reply at all — dial failure, TLS, or the
    /// 3s client timeout.
    Transport,
}

impl PushOutcome {
    fn label(&self) -> &'static str {
        match self {
            PushOutcome::Ok => "ok",
            PushOutcome::Rejected(_) => "rejected",
            PushOutcome::Transport => "transport_error",
        }
    }
}

/// Rate limiter for push-failure surfacing: remembers the last outcome
/// per peer so a persistent failure is reported ONCE, on the
/// transition, instead of once per 10s round forever.
///
/// Per-peer per-TRANSITION rather than per-round is the whole design.
/// A round-rate warn on a 4-peer mesh with one broken peer is 8,640
/// lines a day, which operators filter out, which is functionally the
/// same silence this replaces. A transition-rate warn is one line when
/// it breaks and one when it recovers — and the recovery line is why
/// `Ok` is recorded too rather than just clearing the entry.
#[derive(Default)]
struct PushStatusLedger {
    last: std::collections::HashMap<commonwealth_core::ids::NodeId, PushOutcome>,
}

impl PushStatusLedger {
    /// Record `outcome` for `peer`. Returns `true` when it differs from
    /// the last recorded outcome (including the first sighting of a
    /// non-`Ok` outcome) — i.e. when it is worth a line.
    fn note(&mut self, peer: commonwealth_core::ids::NodeId, outcome: PushOutcome) -> bool {
        match self.last.insert(peer, outcome) {
            // First ever sighting: worth a line only if it is a failure.
            // A first success is the expected state, not news.
            None => outcome != PushOutcome::Ok,
            Some(previous) => previous != outcome,
        }
    }
}

fn push_status_ledger() -> &'static std::sync::Mutex<PushStatusLedger> {
    static LEDGER: std::sync::OnceLock<std::sync::Mutex<PushStatusLedger>> =
        std::sync::OnceLock::new();
    LEDGER.get_or_init(|| std::sync::Mutex::new(PushStatusLedger::default()))
}

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
        // Latch for the online-population rail below. Per-loop rather
        // than a process static because there is one gossip loop per
        // daemon, and a latch that outlives the loop it describes would
        // be a lie after a mesh leave/rejoin.
        let mut over_rail = false;
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_one_round(&app_state, offline_threshold).await {
                warn!(error = %e, "gossip: round errored");
            }

            // ── Online-population rail ────────────────────────────
            //
            // `max_online_peers_before_false_offline` is a computed,
            // checkable ceiling — and until now nothing checked it. It
            // is a WORST-CASE sufficient condition (no relay possible),
            // not an operating ceiling: liveness is also stamped by
            // receive-side merges and transitively through any member
            // whose record advanced, so a real mesh disseminates
            // epidemically and runs happily above this number
            // (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.2 corrects
            // §3's headline on exactly this point). That is precisely
            // why this is a WARN-RAIL and not a limit: crossing it says
            // "direct contact alone no longer guarantees liveness here
            // — if peers start flapping Offline, this is why", and
            // raising fanout is NOT the indicated fix.
            let online_peers = {
                let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
                let mesh = app_state.inner.mesh.read().await;
                mesh.members
                    .values()
                    .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
                    .count()
            };
            let ceiling =
                max_online_peers_before_false_offline(FANOUT, interval, offline_threshold);
            let now_over = online_peers > ceiling;
            if now_over != over_rail {
                over_rail = now_over;
                if now_over {
                    warn!(
                        online_peers,
                        ceiling,
                        fanout = FANOUT,
                        interval_secs = interval.as_secs(),
                        offline_threshold_secs = offline_threshold.as_secs(),
                        "gossip: online peers past the direct-contact ceiling \
                         (fanout × floor(threshold/interval)) — direct contact alone can no \
                         longer refresh every peer inside the offline threshold; relayed \
                         liveness is now load-bearing. Watch for Offline flaps on reachable peers"
                    );
                } else {
                    info!(
                        online_peers,
                        ceiling, "gossip: online peers back under the direct-contact ceiling"
                    );
                }
            } else {
                tracing::debug!(
                    online_peers,
                    ceiling,
                    over_rail,
                    "gossip: online-population rail"
                );
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

/// Choose this round's gossip targets from `(peer, last_contact_unix)`
/// pairs: online peers first, and **most-stale first within each group** —
/// the peer closest to the offline threshold is the one a round can least
/// afford to skip. Offline peers take whatever slots are left. Returned in
/// dial order, so live peers are contacted before any unreachable member
/// can burn `PEER_TIMEOUT`.
///
/// THE RULE THIS ENFORCES: *selection misses alone must never be able to
/// carry a reachable peer past `offline_threshold`.* The previous selection
/// shuffled ALL members together and truncated to `FANOUT`, making a live
/// peer's per-round contact chance `FANOUT / members` — while
/// `DEFAULT_OFFLINE_THRESHOLD` is sized at "roughly 6× the interval" on the
/// unstated assumption that a round *contacts* the peer. Random sampling
/// silently violated the assumption its own constant was chosen under.
///
/// Ordering by staleness converts that coin flip into a bound: with `n`
/// online peers a peer waits at most `ceil(n / FANOUT)` rounds, because
/// every round it goes unpicked it moves up the order. No RNG, no
/// per-round state, and it degrades gracefully — the mesh only needs
/// `ceil(n / FANOUT) * interval < offline_threshold`, which is a
/// computable condition rather than a silent probability
/// (`max_online_peers_before_false_offline`).
///
/// MEASURED, not theorised. Meshsonics 2026-07-29: 4 members, one live
/// peer (BeefyMac) + two long-dead ones, `FANOUT = 2`. BeefyMac was picked
/// ~2/3 of rounds while the dead peers each burned a ~3s iroh dial,
/// stretching rounds 10s → ~16s, so four misses cleared 60s. It flapped
/// Offline three times in fourteen minutes (staleness 68s / 63s / 62s)
/// with gossip reach at 65–600ms on either side of every lapse. Because
/// gossip-Online membership *is* the RPC-worker liveness signal, each flap
/// emptied the eligible-worker set; the third retired a healthy
/// distributed 122B eleven minutes into serving. Fourteen seconds after
/// that child was SIGKILLed: `gossip: reach ok reach_ms=548`.
///
/// A member that cannot contribute to a given workload is NOT the thing
/// being filtered here, and must never be: peers belong to a mesh for
/// their own reasons, and a run drawing on a subset of them is the normal
/// case, not a degraded one. The only axis this reads is reachability.
///
/// Resurrection is unaffected when the online set fills the fan-out: a
/// returning peer runs this same loop and dials US, and the receive-side
/// merge stamps `observe_peer_contact` — already the documented
/// offline→online path, not a new assumption.
fn select_round_peers<T>(
    mut online: Vec<(T, u64)>,
    mut offline: Vec<(T, u64)>,
    fanout: usize,
) -> Vec<T> {
    // Ascending `last_contact` == longest-unseen first.
    online.sort_by_key(|(_, last_contact)| *last_contact);
    offline.sort_by_key(|(_, last_contact)| *last_contact);
    let online_take = online.len().min(fanout);
    let mut out: Vec<T> = online.drain(..online_take).map(|(c, _)| c).collect();
    let offline_take = offline.len().min(fanout - out.len());
    out.extend(offline.drain(..offline_take).map(|(c, _)| c));
    out
}

/// How many online peers this mesh can hold before a reachable peer can be
/// carried past `offline_threshold` by selection pressure alone. Above
/// this, `FANOUT` (or the threshold, or the interval) has to grow — the
/// point of stating it as a function is that the ceiling is checkable
/// instead of being an emergent property of a shuffle.
fn max_online_peers_before_false_offline(
    fanout: usize,
    interval: Duration,
    offline_threshold: Duration,
) -> usize {
    if interval.is_zero() {
        return usize::MAX;
    }
    // A peer must be reached within `rounds` rounds; staleness ordering
    // reaches every online peer within ceil(n / fanout).
    let rounds = (offline_threshold.as_secs() / interval.as_secs()) as usize;
    fanout.saturating_mul(rounds)
}

/// One full gossip round. Touches own `last_seen`, decays stale
/// peers, then pair-gossips with up to `FANOUT` members — online ones
/// first (`select_round_peers`).
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
    let candidates: Vec<(PeerContact, bool, u64)> = {
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
            //
            // Status AND local-contact staleness ride along, because the
            // round's SELECTION depends on both — see `select_round_peers`.
            // Read here, inside the same lock that just ran the offline-decay
            // pass and via the same `peer_contact_or_init` it used, so the
            // selection sees this round's numbers and not last round's.
            .map(|m| {
                (
                    peer_contact(m),
                    m.status != NodeStatus::Offline,
                    app_state.peer_contact_or_init(m.node_id, now),
                )
            })
            .collect()
    };

    if candidates.is_empty() {
        // Solo mesh — nothing to do. Still valuable to have fired
        // the round so self's `last_seen` stays current for the
        // moment a peer does arrive.
        return Ok(());
    }

    // Step 2: pick up to FANOUT peers — online ones first, most-stale
    // first within each group. Not a heuristic: see `select_round_peers`
    // for why random sampling here is what decays healthy peers to
    // Offline. No RNG is involved any more, which is also why nothing
    // needs scoping around the `.await`s below.
    let selection = {
        let (online, offline): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|(_, up, _)| *up);
        select_round_peers(
            online.into_iter().map(|(c, _, s)| (c, s)).collect(),
            offline.into_iter().map(|(c, _, s)| (c, s)).collect(),
            FANOUT,
        )
    };

    // Step 3: snapshot our mesh once and POST it to each picked
    // peer. Using the same snapshot across the fan-out keeps rounds
    // cheap and means every peer sees the same view of us.
    let my_snapshot = { app_state.inner.mesh.read().await.clone() };
    let http = gossip_client().map_err(|e| GossipError::ClientBuild(e.to_string()))?;

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
            match gossip_with_peer(http, &ep.base_url, &my_snapshot).await {
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
                    // A COMPLETED ROUND-TRIP IS THE STRONGEST LIVENESS
                    // EVIDENCE THERE IS — stamp it UNCONDITIONALLY, and
                    // before the merge (fixed 2026-07-29).
                    //
                    // The stamping below is driven by `report.observed`,
                    // which by contract holds only the peers whose RECORD
                    // ADVANCED in this merge. That is the right rule for
                    // peers we learned about transitively, and the wrong
                    // rule for the peer we just spoke to: in steady state
                    // its record does not advance, so a peer answering us
                    // every round was never stamped at all and decayed to
                    // Offline on schedule while `gossip: reach ok` kept
                    // logging success.
                    //
                    // Observed live 2026-07-29 — reach ok at 46/55/63/69 ms
                    // in the four rounds immediately preceding
                    // `peer marked Offline … staleness_secs=67`, on a peer
                    // that was answering TCP in 3-9 ms at the time. That
                    // false Offline emptied the eligible-worker set and
                    // cost the distributed 122B its remote shard.
                    //
                    // Liveness must never be a side effect of payload
                    // change. Talking to someone IS the evidence.
                    app_state.observe_peer_contact(peer_id, now);
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

            // ── Payload gauge ──────────────────────────────────────
            //
            // Serialise ONCE, here, and post the bytes: the gauge then
            // measures the exact body that goes on the wire rather
            // than an estimate of it, and the fan-out below stops
            // re-serialising the same snapshot per peer per address.
            let store_bytes = match serde_json::to_vec(&store_body) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "gossip: mesh_store snapshot failed to serialise — skipping replication this round");
                    return Ok(());
                }
            };
            let payload_bytes = store_bytes.len();
            tracing::debug!(
                payload_bytes,
                entries = entries.len(),
                warn_at_bytes = MESH_STORE_PAYLOAD_WARN_BYTES,
                limit_bytes = commonwealth_api::server::MAX_REQUEST_BODY_BYTES,
                "gossip: mesh_store payload gauge"
            );
            if payload_bytes >= MESH_STORE_PAYLOAD_WARN_BYTES {
                warn!(
                    payload_bytes,
                    entries = entries.len(),
                    warn_at_bytes = MESH_STORE_PAYLOAD_WARN_BYTES,
                    limit_bytes = commonwealth_api::server::MAX_REQUEST_BODY_BYTES,
                    pct_of_limit = (payload_bytes * 100)
                        / commonwealth_api::server::MAX_REQUEST_BODY_BYTES.max(1),
                    "gossip: mesh_store snapshot is past half the receiver's body limit — \
                     full-snapshot replication only grows; past the limit peers stop \
                     converging, and the 3s POST timeout trips before that on a relay link"
                );
            }

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
                // The PEER-level outcome, decided after every address
                // has had its turn. Per-ADDRESS failure is expected on
                // a multi-homed peer (a stale LAN IP behind a working
                // Tailscale address) and stays at debug; what an
                // operator needs surfaced is "this peer is not taking
                // our snapshot", which is only knowable once the
                // address list is exhausted.
                let mut outcome = PushOutcome::Transport;
                let mut last_detail = String::new();
                for ep in &endpoints {
                    let url = format!("{}/internal/app/state", ep.base_url);
                    let push_start = Instant::now();
                    match http
                        .post(&url)
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(store_bytes.clone())
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            let push_ms = push_start.elapsed().as_millis() as u64;
                            tracing::debug!(
                                peer = %peer_id,
                                url = %url,
                                push_ms,
                                payload_bytes,
                                entries = entries.len(),
                                "gossip: mesh_store pushed to peer"
                            );
                            outcome = PushOutcome::Ok;
                            break; // one working address is enough
                        }
                        Ok(resp) => {
                            let push_ms = push_start.elapsed().as_millis() as u64;
                            let status = resp.status();
                            tracing::debug!(
                                peer = %peer_id,
                                url = %url,
                                push_ms,
                                status = %status,
                                "gossip: mesh_store push rejected"
                            );
                            outcome = PushOutcome::Rejected(status.as_u16());
                            last_detail = status.to_string();
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
                            outcome = PushOutcome::Transport;
                            last_detail = e.to_string();
                        }
                    }
                }
                if endpoints.is_empty() {
                    last_detail = "no addresses on record".to_string();
                }

                // ── Surfacing rail ────────────────────────────────
                //
                // Both failure branches were `debug`, which the shipped
                // daemon never emits — so mesh_store replication could
                // stop working entirely and every surface stayed green.
                // Rate-limited per peer per TRANSITION, not per round:
                // see `PushStatusLedger`.
                let transition = push_status_ledger()
                    .lock()
                    .map(|mut l| l.note(peer_id, outcome))
                    .unwrap_or(true);
                if transition {
                    match outcome {
                        PushOutcome::Ok => info!(
                            peer = %peer_id,
                            outcome = outcome.label(),
                            payload_bytes,
                            entries = entries.len(),
                            "gossip: mesh_store replication to peer RECOVERED"
                        ),
                        PushOutcome::Rejected(status) => warn!(
                            peer = %peer_id,
                            outcome = outcome.label(),
                            status,
                            detail = %last_detail,
                            payload_bytes,
                            entries = entries.len(),
                            addresses_tried = endpoints.len(),
                            "gossip: mesh_store push REJECTED by peer — this peer's view of \
                             app state, handoffs and manifests is no longer converging \
                             (413 here means the snapshot outgrew the receiver's body limit)"
                        ),
                        PushOutcome::Transport => warn!(
                            peer = %peer_id,
                            outcome = outcome.label(),
                            detail = %last_detail,
                            payload_bytes,
                            entries = entries.len(),
                            addresses_tried = endpoints.len(),
                            "gossip: mesh_store push FAILED on every address — this peer's \
                             view of app state, handoffs and manifests is no longer converging"
                        ),
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
    let Ok(http) = gossip_client() else {
        return;
    };
    let transport = app_state.peer_transport();
    let mut announced = 0usize;
    for contact in &targets {
        let eps = transport.endpoints(contact, TrafficClass::Gossip).await;
        for ep in &eps {
            if gossip_with_peer(http, &ep.base_url, &snapshot)
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

    let http = match gossip_client() {
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

#[cfg(test)]
mod gossip_client_tests {
    use super::gossip_client;

    /// The gossip HTTP client must be ONE process-wide instance, because a
    /// `reqwest::Client` owns its connection pool: a fresh client per round
    /// drops the pool, and the peer's iroh `HttpBridge` then dials a fresh
    /// QUIC connection for the new TCP connection. Measured cost of getting
    /// this wrong (RuggedFox → BeefyMac, concurrent A/B, one 3-min window):
    /// p50 189 ms and 2 dial timeouts, versus p50 38.8 ms and 0 when the
    /// connection is reused.
    ///
    /// Pointer identity is the honest assertion here — two `Client` values
    /// that merely compare equal would still be two pools.
    #[test]
    fn gossip_client_is_one_shared_pool_per_process() {
        let a = gossip_client().expect("gossip client builds");
        let b = gossip_client().expect("gossip client builds");
        assert!(
            std::ptr::eq(a, b),
            "gossip must reuse ONE reqwest::Client — a per-round client throws \
             away the connection pool and forces a fresh QUIC handshake to every \
             peer on every round"
        );
    }
}

#[cfg(test)]
mod select_round_peers_tests {
    use super::{
        max_online_peers_before_false_offline, select_round_peers, DEFAULT_GOSSIP_INTERVAL,
        DEFAULT_OFFLINE_THRESHOLD, FANOUT,
    };

    /// Simulate `rounds` gossip rounds over a fixed member set and return, for
    /// each online peer, the WORST gap (in rounds) it ever went uncontacted.
    /// A peer contacted every round has a gap of 1.
    ///
    /// This is the only honest way to test the property at issue: the bug was
    /// never visible in a single round, only in a streak of them.
    fn worst_contact_gap(online: usize, offline: usize, fanout: usize, rounds: usize) -> usize {
        let mut last_contact: Vec<u64> = vec![0; online + offline];
        let mut worst = vec![0usize; online];
        for round in 1..=rounds {
            let up: Vec<(usize, u64)> = (0..online).map(|i| (i, last_contact[i])).collect();
            let down: Vec<(usize, u64)> = (online..online + offline)
                .map(|i| (i, last_contact[i]))
                .collect();
            for id in select_round_peers(up, down, fanout) {
                if id < online {
                    worst[id] = worst[id].max(round - last_contact[id] as usize);
                }
                // Contact stamps the round number, exactly as
                // `observe_peer_contact` stamps the clock in the real loop.
                last_contact[id] = round as u64;
            }
        }
        // Count the TRAILING gap too. Without this a peer that is never
        // selected at all keeps a worst-gap of 0 and the assertion passes
        // vacuously — the exact shape of the bug being tested for.
        for (id, w) in worst.iter_mut().enumerate() {
            *w = (*w).max(rounds - last_contact[id] as usize);
        }
        worst.into_iter().max().unwrap_or(0)
    }

    /// How many rounds fit inside the offline threshold — the budget a peer
    /// has to be contacted within, and the number the threshold's own
    /// "roughly 6× the interval" doc comment is reasoning about.
    fn rounds_before_decay() -> usize {
        (DEFAULT_OFFLINE_THRESHOLD.as_secs() / DEFAULT_GOSSIP_INTERVAL.as_secs()) as usize
    }

    /// The live Meshsonics shape that retired a healthy distributed 122B: one
    /// online peer, two long-dead members, `FANOUT = 2`. The online peer must
    /// be contacted EVERY round — the threshold is counted in rounds, and a
    /// miss streak is what decayed it.
    #[test]
    fn the_only_online_peer_is_contacted_every_round_when_corpses_outnumber_it() {
        assert_eq!(worst_contact_gap(1, 2, FANOUT, 200), 1);
        let picked = select_round_peers(vec![("BeefyMac", 10)], vec![("LittleMac", 0)], FANOUT);
        assert_eq!(
            picked[0], "BeefyMac",
            "the live peer must be dialed FIRST — before an unreachable member burns PEER_TIMEOUT — \
             even though it is the FRESHER of the two"
        );
    }

    /// The generalised rule, and the one a growing mesh actually needs: a
    /// reachable peer must never be carried past the offline threshold by
    /// selection pressure alone. Asserted across every mesh size the
    /// configured fan-out is supposed to cover, with corpses mixed in.
    ///
    /// The old shuffle-and-truncate fails this at EVERY size, corpses or not
    /// — simulated over 5000 rounds at `FANOUT = 2` against a 6-round budget,
    /// worst gap in rounds: 1 online + 2 offline → 8; **3 online + 0 offline
    /// → 12**; 6 online → 26; 6 online + 2 offline → 32; 12 online → 59. The
    /// dead members on Meshsonics made it fire sooner, but three healthy
    /// peers and no corpses at all is already past the threshold. Random
    /// sampling was never sound here; the corpses only set the rate.
    #[test]
    fn no_reachable_peer_is_ever_carried_past_the_offline_threshold() {
        let budget = rounds_before_decay();
        let ceiling = max_online_peers_before_false_offline(
            FANOUT,
            DEFAULT_GOSSIP_INTERVAL,
            DEFAULT_OFFLINE_THRESHOLD,
        );
        assert!(ceiling >= 2, "fan-out must cover at least a pair");
        for online in 1..=ceiling {
            for corpses in 0..4 {
                let gap = worst_contact_gap(online, corpses, FANOUT, 300);
                assert!(
                    gap <= budget,
                    "{online} online + {corpses} offline: a healthy peer went {gap} rounds \
                     uncontacted, past the {budget}-round offline budget"
                );
            }
        }
    }

    /// Staleness ordering is what produces that bound: the longest-unseen
    /// peer goes first, so a peer that misses a round is promoted, not
    /// re-entered into a fresh lottery.
    #[test]
    fn the_longest_unseen_peer_is_selected_first() {
        let picked = select_round_peers(
            vec![("fresh", 100), ("stalest", 5), ("middling", 50)],
            Vec::new(),
            2,
        );
        assert_eq!(picked, vec!["stalest", "middling"]);
    }

    /// Reachability is the ONLY axis. A peer that is online but useless for
    /// the workload at hand is still gossiped with every round like any
    /// other — members belong to a mesh for their own reasons, and a run
    /// drawing on a subset of them is normal, not degraded.
    #[test]
    fn selection_reads_reachability_only_never_capability() {
        // Same shape, twice: the function has no input by which "can this
        // peer serve a 122B shard" could possibly influence the outcome.
        let a = select_round_peers(vec![("tiny-laptop", 1), ("big-gpu-box", 2)], Vec::new(), 2);
        assert_eq!(a, vec!["tiny-laptop", "big-gpu-box"]);
    }

    /// Every online peer we have room for is taken before any offline one.
    #[test]
    fn online_peers_fill_the_fanout_before_offline_peers_get_a_slot() {
        let picked = select_round_peers(vec![("a", 1), ("b", 2), ("c", 3)], vec![("dead", 0)], 2);
        assert_eq!(picked, vec!["a", "b"]);
    }

    /// A fully-partitioned node — nothing online — must still probe, or it
    /// could never rejoin.
    #[test]
    fn a_node_with_no_online_peers_still_probes_the_offline_ones() {
        let picked = select_round_peers(
            Vec::<(&str, u64)>::new(),
            vec![("dead-a", 1), ("dead-b", 2), ("dead-c", 3)],
            2,
        );
        assert_eq!(picked, vec!["dead-a", "dead-b"]);
    }

    /// Fewer candidates than the fan-out is not an error, and a solo mesh
    /// selects nothing rather than panicking on the `fanout - out.len()` math.
    #[test]
    fn short_candidate_lists_and_an_empty_mesh_are_handled() {
        assert_eq!(select_round_peers(vec![("a", 0)], Vec::new(), 2), vec!["a"]);
        assert!(select_round_peers(Vec::<(&str, u64)>::new(), Vec::new(), 2).is_empty());
        assert!(select_round_peers(vec![("a", 0)], vec![("b", 0)], 0).is_empty());
    }

    /// The ceiling is a real number the operator could act on, not a
    /// formality: at the shipped constants it must cover a mesh comfortably
    /// larger than the current one.
    #[test]
    fn the_documented_ceiling_matches_the_shipped_constants() {
        assert_eq!(
            max_online_peers_before_false_offline(
                FANOUT,
                DEFAULT_GOSSIP_INTERVAL,
                DEFAULT_OFFLINE_THRESHOLD
            ),
            12,
            "FANOUT=2 × (60s / 10s) = 12 online peers"
        );
    }
}

#[cfg(test)]
mod push_surfacing_tests {
    use super::{
        max_online_peers_before_false_offline, PushOutcome, PushStatusLedger,
        MESH_STORE_PAYLOAD_WARN_BYTES,
    };
    use commonwealth_core::ids::NodeId;
    use std::time::Duration;

    fn nid(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    /// RED-FIRST (order mesh-scale-t0, item 1). Before the fix there was
    /// no ledger at all — both push-failure branches logged at `debug`,
    /// which the shipped daemon never emits, so mesh_store replication
    /// could stop entirely with every surface staying green. This test
    /// does not compile against the pre-fix module.
    ///
    /// What it pins is the rate-limiting CONTRACT the order asked for:
    /// per peer, per status TRANSITION, not per round.
    #[test]
    fn failures_surface_once_per_transition_not_once_per_round() {
        let mut ledger = PushStatusLedger::default();
        let peer = nid(1);

        // First success is the expected state — not news.
        assert!(!ledger.note(peer, PushOutcome::Ok));

        // It breaks: one line.
        assert!(ledger.note(peer, PushOutcome::Transport));
        // …and stays broken for the next 8,639 rounds of the day: silence.
        for _ in 0..8_639 {
            assert!(!ledger.note(peer, PushOutcome::Transport));
        }

        // The failure CHANGES SHAPE — a peer that was unreachable now
        // answers with 413. Different failure, different operator
        // response, so it must not be swallowed by the rate limiter.
        assert!(ledger.note(peer, PushOutcome::Rejected(413)));
        assert!(!ledger.note(peer, PushOutcome::Rejected(413)));
        // A different status is a different transition.
        assert!(ledger.note(peer, PushOutcome::Rejected(401)));

        // Recovery is news too — otherwise the operator is left holding
        // a warn with no matching all-clear.
        assert!(ledger.note(peer, PushOutcome::Ok));
        assert!(!ledger.note(peer, PushOutcome::Ok));
    }

    /// A first sighting that is already a failure must surface — the
    /// "no previous entry" case is the one a naive `!= previous` gets
    /// wrong, and it is also the common one after a daemon restart.
    #[test]
    fn a_peer_broken_from_the_first_round_still_surfaces() {
        let mut ledger = PushStatusLedger::default();
        assert!(ledger.note(nid(2), PushOutcome::Rejected(413)));
    }

    /// Peers are tracked independently — one broken peer must not
    /// suppress another's first failure.
    #[test]
    fn the_ledger_is_per_peer() {
        let mut ledger = PushStatusLedger::default();
        assert!(ledger.note(nid(1), PushOutcome::Transport));
        assert!(ledger.note(nid(2), PushOutcome::Transport));
    }

    /// The gauge threshold is DERIVED from the receiver's limit, not a
    /// second copy of the number. If someone retypes it, this fails.
    #[test]
    fn the_payload_warn_is_half_the_receivers_limit() {
        assert_eq!(
            MESH_STORE_PAYLOAD_WARN_BYTES * 2,
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
        );
        assert_eq!(MESH_STORE_PAYLOAD_WARN_BYTES, 4 * 1024 * 1024);
    }

    /// The rail the loop now checks. Named here so the formula's
    /// operating meaning is pinned next to the code that warns on it:
    /// at the shipped fanout/interval/threshold a mesh has room for 12
    /// online peers under the worst-case (no-relay) condition.
    #[test]
    fn the_online_population_rail_matches_the_shipped_constants() {
        let ceiling = max_online_peers_before_false_offline(
            super::FANOUT,
            super::DEFAULT_GOSSIP_INTERVAL,
            super::DEFAULT_OFFLINE_THRESHOLD,
        );
        assert_eq!(ceiling, 12, "fanout 2 × floor(60s / 10s)");
        assert!(12 > ceiling - 1);

        // A zero interval cannot be divided by; the formula must not
        // panic or report a rail of 0 (which would warn forever).
        assert_eq!(
            max_online_peers_before_false_offline(
                2,
                Duration::ZERO,
                super::DEFAULT_OFFLINE_THRESHOLD
            ),
            usize::MAX
        );
    }
}
