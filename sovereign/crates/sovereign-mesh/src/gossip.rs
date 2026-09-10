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
//!
//! # The member list, and nothing else
//!
//! Until cw-lift rung 2e this loop had a fourth step: a full `mesh_store`
//! snapshot POSTed to EVERY online peer on the same ten-second round, plus a
//! `broadcast_now` that pushed one entry the same way for the work atlas.
//! Both are gone, and with them the route they wrote to
//! (`POST /internal/app/state`) and the enumeration they read from
//! (`MeshStore::all_entries_for_gossip`).
//!
//! Store state replicates on the ring now: a write is queued in the store's
//! own transaction, [`crate::rail_kv_pump`] signs it onto its namespace's
//! journal, and [`crate::ring_sync`] carries it by digest. That leaves ONE
//! sender of replicated state in the workspace — `/internal/ring/sync` — which
//! is `cw-twin-visibility`'s instrument and is pinned by
//! `tests/main/replication_sender_census.rs::every_sender_of_replicated_state_is_declared`.
//! So `FANOUT` now governs the whole module rather than three of its four
//! steps.
use std::time::{Duration, Instant};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
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
/// `pub(crate)` so the ring-sync loop shares this exact client: one
/// connection pool and one timeout policy for all peer HTTP, rather than a
/// second answer to "how long do we wait on a peer" (ARCH §10.6).
pub(crate) fn gossip_client() -> Result<&'static reqwest::Client, &'static str> {
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
/// Whether this member is worth dialing at all — asked once, before
/// [`select_round_peers`] decides which of the candidates get this round's
/// slots (ARCH §10.6: one decider for "who do we talk to").
///
/// Two exclusions, and the second is not an optimisation.
///
/// - **Ourselves.** We are authoritative for our own record.
/// - **A tombstone** (`!is_active()`, i.e. `removed_at` is set). The member
///   departed and said so. Dialing it cannot converge anything: the tombstone
///   travels in the snapshot we push to LIVE peers, and a genuine rejoin dials
///   US — the receive-side merge is the documented offline→online path, which
///   [`select_round_peers`] already relies on for resurrection.
///
/// The harm is concrete, not theoretical. A departed row sharing an endpoint
/// key with a live one is the LEGITIMATE rejoin shape that
/// `commonwealth_core::mesh_identity`'s alias rule deliberately permits — a
/// tombstone is explicitly never refused. But `IrohTransport` keys its bridge
/// cache on `(pubkey, alpn)`, so both rows resolve to ONE bridge, and their
/// gossiped dial info differs. Dialing the dead row retargets the LIVE peer's
/// tunnel to a stale address, and the next round retargets it back.
///
/// Measured on RuggedFox 2026-09-09 (note `1ca75415`): 181 retargets in 12
/// minutes on one endpoint, gossip to the live Mac failing at exactly
/// `PEER_TIMEOUT`, and that peer decaying to Offline — while iroh reported an
/// ACTIVE path throughout, which is why relay-home, self-discovery and the
/// peer-path health term all read green through it.
///
/// `announce_presence_change` has filtered `is_active()` on its own push
/// targets all along; the ordinary round simply never got the same predicate.
fn is_gossip_candidate(m: &MemberRecord, self_id: NodeId) -> bool {
    m.node_id != self_id && m.is_active()
}

/// Collapse live members that share ONE endpoint key down to one row each,
/// so a round dials a physical machine once however many names it joined
/// under (ARCH §7.5 — identity is the Ed25519 key; `node_id` is a second name
/// for the same thing).
///
/// # Why this is a WINNER and not a refusal
///
/// `Mesh::merge_from`'s `alias_clash` already refuses to ADMIT a second active
/// record on one key. This is the read-side companion for the roster that
/// already contains one — and it deliberately makes the opposite choice about
/// what to do when it finds one, because refusing is what produced the failure
/// this exists to prevent:
///
/// `is_gossip_candidate` excluded a tombstoned row, one physical Mac held two
/// roster rows, and the row that came back was the tombstoned one. Nobody
/// dialed it, so no inbound merge could clear it, so nobody dialed it. A
/// reachable member was unreachable for 11.8 days and the state could not
/// decay. A resilient roster must never contain a partition that cannot heal,
/// so when this finds an ambiguity it RESOLVES it rather than dropping every
/// row involved.
///
/// # Why the collapse, and what it costs when it is wrong
///
/// `IrohTransport::bridge_for` caches on `(pubkey, alpn)`, so two rows on one
/// key share ONE bridge and their differing dial info retargets it under every
/// round — measured at ~14/min on this host both before the tombstone filter
/// (note `1ca75415`, 181 in 12 min) and again the moment the second row came
/// back to life (83 in 6 min). Retargeting repoints a live tunnel mid-flight,
/// so in-flight gossip dies at exactly `PEER_TIMEOUT` and the peer decays
/// while iroh reports a healthy path throughout.
///
/// Picking the wrong row costs nothing structural: both names resolve to the
/// same endpoint key, so the dial reaches the same machine either way. What
/// would cost something is picking a DIFFERENT row each round — that is the
/// retarget storm with extra steps — so the choice has to be stable, not
/// merely correct.
///
/// # The winner, and why it converges
///
/// Greatest [`MemberRecord::event_time`], `node_id` as a deterministic
/// tiebreak. This is self-reinforcing in the right direction: the winner is
/// dialed, answers, and its record advances on merge, while the loser is not
/// dialed and its `event_time` stands still — so the gap widens and the choice
/// stops moving. A row with NO pubkey cannot collide on a key it does not
/// have and is always kept; `None` is not an identity.
fn one_row_per_endpoint_key<'a>(members: Vec<&'a MemberRecord>) -> Vec<&'a MemberRecord> {
    use std::collections::HashMap;
    let mut best: HashMap<commonwealth_core::ids::NodePubkey, &'a MemberRecord> = HashMap::new();
    let mut keyless: Vec<&'a MemberRecord> = Vec::new();
    for m in members {
        let Some(key) = m.node_pubkey else {
            keyless.push(m);
            continue;
        };
        match best.get(&key) {
            Some(held) if (held.event_time(), held.node_id) >= (m.event_time(), m.node_id) => {}
            _ => {
                best.insert(key, m);
            }
        }
    }
    let mut out: Vec<&'a MemberRecord> = best.into_values().collect();
    out.extend(keyless);
    out
}

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
    // Recompute availability from BOTH its inputs before publishing it.
    // The daemon is the second caller of the one availability writer (the
    // first is sovereign-server's ActivityReporter, via
    // `update_local_availability`): the yield-to-local-user half is a pure
    // function of a timestamp and a window, so it has no transition event to
    // hook and would otherwise never be published at all. Until 2026-08-14
    // nothing in the daemon wrote this field, so a node refusing every peer
    // request with `yielded_to_local` gossiped `availability: 1.0` for as
    // long as it kept refusing (note 3234d770). Recomputing HERE — one line
    // above the read, inside the round that publishes it — is what makes the
    // advertised number true at the moment it goes out.
    let availability = app_state.recompute_local_availability().await;
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
        let live: Vec<&MemberRecord> = mesh
            .members
            .values()
            .filter(|m| is_gossip_candidate(m, self_id))
            .collect();
        let before = live.len();
        let deduped = one_row_per_endpoint_key(live);
        if deduped.len() < before {
            // SAY IT OUT LOUD (§9.1). This is a roster defect the operator has
            // to repair — one machine holding two node_ids — and the whole
            // reason it went unnoticed for 11.8 days is that nothing ever
            // named it. `mesh_identity` makes the repair an operator act, so
            // the least this round can do is report the condition.
            debug!(
                target: "mesh.peer_path",
                collapsed = before - deduped.len(),
                dialing = deduped.len(),
                "gossip: members share an endpoint key — dialing one row per key"
            );
        }
        deduped
            .into_iter()
            // The transport sorts candidates IPv4-first on
            // resolution and promotes the last-working address,
            // so the contact carries the raw gossiped list.
            //
            // Status AND selection staleness ride along, because the round's
            // SELECTION depends on both — see `select_round_peers`.
            //
            // THE STALENESS HERE IS THE ATTEMPT CLOCK, NOT THE CONTACT CLOCK,
            // and the difference is the whole of the starvation fix. The decay
            // pass above rightly measures `peer_contact_or_init` — liveness
            // evidence, stamped only when a peer actually answers. Ordering
            // SELECTION by that same number means a peer that never answers
            // never advances and so is picked every round for ever, which is
            // not a fairness wobble but a permanent exclusion of everyone else:
            // 74 dials each to two dead peers and zero to the other five,
            // measured 2026-09-09 with a live peer among the five.
            .map(|m| {
                (
                    peer_contact(m),
                    m.status != NodeStatus::Offline,
                    app_state.peer_attempt_or_init(m.node_id, now),
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
        // SPENDING THE SLOT IS THE EVENT THIS STAMPS, not the outcome. It goes
        // here, before the dial, so a refusal and a `PEER_TIMEOUT` advance the
        // selection order exactly as a success does — that is what stops two
        // unreachable peers from holding both slots for ever. Liveness is
        // stamped separately and only on a completed round-trip
        // (`observe_peer_contact`, below).
        app_state.note_peer_attempt(peer_id, now);
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
            // INFO, not debug. At debug this round leaves NO trace, and the
            // operator reads a log that goes `reach ok … reach ok … [nothing]
            // … peer marked Offline` — from which "we stopped trying" and "we
            // tried and failed" are indistinguishable. Both were live
            // candidates in the 2026-09-09 capture (note `a3f3fbff`) and
            // neither could be ruled out from the record. One line per peer
            // per round, the same volume the success path already emits.
            info!(
                peer = %peer_id,
                transport = transport.name(),
                outcome = "no-addresses",
                "gossip: round skipped — the transport resolved no dialable address for this peer"
            );
            continue;
        }
        // Whether ANY address worked this round. The per-address lines stay at
        // debug (a multi-homed peer failing one address is routine); this is
        // the per-peer verdict, and it is emitted on every path out of the
        // loop below so a round is never silent about a peer it selected.
        let mut reached = false;
        let attempts = endpoints.len();
        for ep in &endpoints {
            // Per-address timing so we can diagnose the Online↔Offline
            // flap (see todo `f152dfe7` #4). Each line is one address
            // attempt with elapsed ms and outcome, so offline decay can
            // be correlated with a run of failed reaches on a specific
            // address family (LAN vs Tailscale).
            let attempt_start = Instant::now();
            match gossip_with_peer(
                http,
                &ep.base_url,
                &my_snapshot,
                self_id,
                now,
                app_state.peer_confirmed_post_split(peer_id),
            )
            .await
            {
                Ok((their_view, their_auth)) => {
                    let reach_ms = attempt_start.elapsed().as_millis() as u64;
                    info!(
                        peer = %peer_id,
                        peer_addr = %ep.label,
                        reach_ms,
                        "gossip: reach ok"
                    );
                    reached = true;
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
                    let report = mesh.merge_from_authenticated(self_id, &their_view, &their_auth);
                    // A REFUSED merge on this path used to be completely
                    // silent: `added` and `updated` are both 0, so it fell
                    // through both arms below and logged nothing, while
                    // `observe_peer_contact` above had ALREADY stamped the
                    // peer Online. A node whose every gossip round was
                    // rejected therefore read as a healthy peer forever —
                    // reach ok every round, roster intact, converging on
                    // nothing. That is a partition wearing a green light, and
                    // it is the shape of the bug this whole change exists to
                    // remove. Say it out loud (ARCH §9.1).
                    if report.rejected() {
                        tracing::warn!(
                            peer = %peer_id,
                            peer_addr = %ep.label,
                            "gossip: REJECTED — peer answered but its mesh did not \
                             authorize (mesh_id or mesh_secret mismatch). We are \
                             reachable but NOT converging with this peer."
                        );
                    } else {
                        // Which credential generation this peer runs is visible
                        // ONLY in the payload we just merged. Retain it —
                        // `rotate_invite` needs it to refuse rather than
                        // partition this peer, and has no other way to learn it.
                        app_state.observe_peer_split_generation(peer_id, !report.peer_pre_split());
                        tracing::debug!(
                            peer = %peer_id,
                            pre_split = report.peer_pre_split(),
                            "gossip: recorded peer credential generation"
                        );
                    }
                    // Stamp local-observation time for every peer whose record
                    // advanced in this merge (incl. transitively-relayed ones),
                    // so offline-decay sees them as freshly-observed.
                    for observed_id in report.observed() {
                        app_state.observe_peer_contact(*observed_id, now);
                    }
                    if report.added() > 0 {
                        info!(
                            peer = %peer_id,
                            peer_addr = %ep.label,
                            added = report.added(),
                            updated = report.updated(),
                            "gossip: member added from peer's view"
                        );
                    } else if report.updated() > 0 {
                        tracing::debug!(
                            peer = %peer_id,
                            peer_addr = %ep.label,
                            updated = report.updated(),
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
        if !reached {
            // The other half of the four-verdict rule (ARCH §18.2): a round
            // that reached nobody must SAY so, at the same level the success
            // says it. Until 2026-09-09 this case was invisible above debug,
            // which is why a peer decaying to Offline looked like the gossip
            // loop had stopped running rather than like every dial failing.
            warn!(
                peer = %peer_id,
                transport = transport.name(),
                attempts,
                outcome = "unreachable",
                "gossip: round FAILED — every address for this peer refused or timed out \
                 (run with RUST_LOG=debug for the per-address errors)"
            );
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
/// Why this node is stepping out of the mesh — the one thing that differs
/// between leaving and parking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceChange {
    /// Giving up membership. Stamps a `removed_at` tombstone, which the
    /// event-time LWW in `Mesh::merge_from` uses to out-compete any stale live
    /// copy a peer still holds.
    Left,
    /// Setting this mesh down while staying a member — the multi-mesh switch.
    /// Marks us `Offline` and stops there: a tombstone would tell peers we
    /// departed, and the whole point of parking is that we intend to come back.
    Parked,
}

/// Leaving: tombstone + offline. Thin wrapper so existing callers read the same.
pub async fn announce_departure(app_state: &AppState) {
    announce_presence_change(app_state, PresenceChange::Left).await
}

pub async fn announce_presence_change(app_state: &AppState, change: PresenceChange) {
    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    let now = app_state.clock().now_unix_secs();
    let (snapshot, targets) = {
        let mut mesh = app_state.inner.mesh.write().await;
        if let Some(me) = mesh.members.get_mut(&self_id) {
            if change == PresenceChange::Left {
                me.removed_at = Some(now);
            }
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
            if gossip_with_peer(
                http,
                &ep.base_url,
                &snapshot,
                self_id,
                now,
                app_state.peer_confirmed_post_split(contact.node_id),
            )
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

async fn gossip_with_peer(
    http: &reqwest::Client,
    base_url: &str,
    my_view: &Mesh,
    self_id: NodeId,
    now_secs: u64,
    peer_is_post_split: bool,
) -> Result<(Mesh, commonwealth_core::mesh::GossipAuth), GossipError> {
    // Stop putting the raw credential on the wire the moment the peer can
    // authorize without it. `peer_is_post_split` is our own observation from a
    // previous round (`AppState::peer_confirmed_post_split`), not the peer's
    // claim, so a caller cannot talk us into sending it.
    //
    // A peer we have NOT confirmed still gets it: it may be running a build
    // that authorizes on raw-secret comparison, and withholding would partition
    // it. That is the back-compat half, and it retires itself as the fleet
    // upgrades — no flag day, and no round where both sides are stuck.
    let wire = MeshWire::for_peer(
        my_view,
        if peer_is_post_split {
            SecretDisclosure::Redact
        } else {
            SecretDisclosure::Disclose
        },
    );
    let body = GossipRequestWire {
        mesh: wire,
        from: Some(self_id),
        mesh_proof: my_view.mesh_proof(self_id, now_secs),
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
    // The reply is an auth boundary in its own direction — we merge it, so it
    // must prove itself. Initiating the round is not evidence about who
    // answered.
    let auth = commonwealth_core::mesh::GossipAuth {
        sender: parsed.from,
        proof: parsed.mesh_proof,
        now_secs,
    };
    Ok((parsed.mesh.into_mesh(), auth))
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

// `GossipRequestWire` / `GossipResponseWire` were mirrors of the server's own
// types. The request one existed because that side derived `Deserialize` only;
// the RESPONSE one had no reason at all — `GossipResponse` already derived
// both halves and was already re-exported. It was a pure duplicate.
use commonwealth_api::routes_internal::{
    GossipRequest as GossipRequestWire, GossipResponse as GossipResponseWire,
};

/// The gossip round's wire shape. A THIRD mirror of
/// `commonwealth_api::routes_internal::MeshWire` (the others live in
/// `join.rs` and in the api crate itself); they must agree field for field or
/// the round-trip 422s.
///
/// Both new fields are load-bearing here, not decoration:
/// - `mesh_secret` must survive the round trip. Dropping it would zero the
///   remote's secret on every merge, silently pushing every peer through the
///   pre-split compat arm forever — a degradation nothing would report.
/// - `invite_key_hash` keeps its historical wire name. Renaming the Rust field
///   without this made every gossip round fail with 422, which is exactly how
///   the two `gossip_integration` round-trip tests caught it.
// The third of four declarations of this shape lived here, with its own
// `From<&Mesh>` and `into_mesh`. It is now `commonwealth_core::mesh::
// MeshSnapshot` — the projection and both conversions in one place, which is
// where this shape failed twice: a zero-filled `mesh_secret` on 2026-08-26 and
// an `invite_version` that pinned every peer's invite at 0, both green.
use commonwealth_core::mesh::{MeshWire, SecretDisclosure};

#[cfg(test)]
mod gossip_candidate_tests {
    use super::{is_gossip_candidate, one_row_per_endpoint_key};
    // The record builder already lives in `ring_roster::tests` and is
    // `pub(crate)` for exactly this (ARCH §19 — reuse before minting).
    use crate::ring_roster::tests::member;
    use commonwealth_core::ids::{NodeId, NodePubkey};
    use commonwealth_core::mesh::NodeStatus;

    const SELF: u128 = 1;

    fn me() -> NodeId {
        NodeId::from_u128(SELF)
    }

    #[test]
    fn a_live_peer_is_a_candidate() {
        assert!(is_gossip_candidate(
            &member(NodeId::from_u128(2), "live", None),
            me()
        ));
    }

    #[test]
    fn we_never_dial_ourselves() {
        assert!(!is_gossip_candidate(&member(me(), "me", None), me()));
    }

    /// A departed member is not dialed. Until 2026-09-09 it was: the round
    /// filtered only `self`, so every tombstone in the roster drew a dial
    /// attempt forever.
    #[test]
    fn a_tombstoned_member_is_not_dialed() {
        let mut gone = member(NodeId::from_u128(2), "departed", None);
        gone.removed_at = Some(200);
        assert!(!is_gossip_candidate(&gone, me()));
    }

    /// A tombstone can still read Online — the decay pass demotes on
    /// staleness, not on departure — so a filter keyed on `status` would have
    /// gone on dialing it. The predicate reads `removed_at`, not liveness.
    #[test]
    fn a_tombstone_is_excluded_even_while_it_still_reads_online() {
        let mut gone = member(NodeId::from_u128(2), "departed-but-fresh", None);
        gone.removed_at = Some(200);
        gone.status = NodeStatus::Online;
        assert!(!is_gossip_candidate(&gone, me()));
    }

    /// THE REGRESSION, as the live roster actually held it (note `1ca75415`).
    ///
    /// One machine, two rows, one endpoint key: `BeefyMac` retired,
    /// `Alexs-MacBook-Pro-2` live. The alias rule deliberately PERMITS this —
    /// a tombstone sharing a key with a rejoined node is a legitimate rejoin
    /// and is never refused — so the roster is correct and the round must
    /// still dial only the live row. Dial both and they resolve to one
    /// `(pubkey, alpn)` bridge and retarget it against each other.
    #[test]
    fn a_retired_twin_sharing_an_endpoint_key_is_dropped_and_the_live_row_kept() {
        let key = NodePubkey([0x86; 32]);
        let mut retired = member(NodeId::from_u128(2), "BeefyMac", Some(key));
        retired.removed_at = Some(200);
        let live = member(NodeId::from_u128(3), "Alexs-MacBook-Pro-2", Some(key));

        assert!(
            !is_gossip_candidate(&retired, me()),
            "the retired twin must not be dialed — it shares the live row's bridge"
        );
        assert!(
            is_gossip_candidate(&live, me()),
            "the live row must still be dialed; dropping both would strand the peer"
        );
    }

    // ── one row per endpoint key ──────────────────────────────────────────

    /// THE FAILING INPUT, and it is the state this host was left in an hour
    /// ago. The test above holds only while ONE of the twins is a tombstone.
    /// Resurrect the retired row — which is correct, it is a live machine —
    /// and both are candidates again, both dial, and they share one
    /// `(pubkey, alpn)` bridge: measured 83 retargets in six minutes, against
    /// 181 in twelve before the tombstone filter (note `1ca75415`).
    #[test]
    fn two_live_rows_on_one_endpoint_key_are_dialed_once() {
        let key = NodePubkey([0x86; 32]);
        let mut older = member(NodeId::from_u128(2), "Alexs-MacBook-Pro-2", Some(key));
        older.last_seen = 100;
        let mut newer = member(NodeId::from_u128(3), "BeefyMac", Some(key));
        newer.last_seen = 200;

        let picked = one_row_per_endpoint_key(vec![&older, &newer]);
        assert_eq!(picked.len(), 1, "one machine, one dial");
        assert_eq!(
            picked[0].node_id, newer.node_id,
            "the freshest row wins, so the choice converges as it keeps answering"
        );
    }

    /// THE CONTROL THAT MATTERS: this collapses an AMBIGUITY, never a mesh.
    /// Without it the assertion above is satisfiable by a function that
    /// returns one row full stop — which would quietly reduce every round to a
    /// single peer while still looking like a working mesh.
    #[test]
    fn distinct_endpoint_keys_are_all_kept() {
        let a = member(NodeId::from_u128(2), "a", Some(NodePubkey([1; 32])));
        let b = member(NodeId::from_u128(3), "b", Some(NodePubkey([2; 32])));
        let c = member(NodeId::from_u128(4), "c", Some(NodePubkey([3; 32])));
        assert_eq!(one_row_per_endpoint_key(vec![&a, &b, &c]).len(), 3);
    }

    /// `None` is not an identity. Pre-identity builds gossip no pubkey, and
    /// keying on the `Option` would collapse every one of them into a single
    /// row — a mesh of older nodes silently reduced to one reachable peer,
    /// which is the same un-healable partition this change exists to remove.
    #[test]
    fn members_without_a_pubkey_are_never_collapsed_together() {
        let a = member(NodeId::from_u128(2), "old-a", None);
        let b = member(NodeId::from_u128(3), "old-b", None);
        assert_eq!(one_row_per_endpoint_key(vec![&a, &b]).len(), 2);
    }

    /// The choice must not move between rounds: alternating winners IS the
    /// retarget storm, paced by the gossip interval instead of the bridge
    /// cache. Both input orders are fed in because `HashMap` iteration order
    /// must not be able to decide who gets dialed.
    #[test]
    fn the_winner_is_stable_across_repeated_selection() {
        let key = NodePubkey([0x86; 32]);
        let mut x = member(NodeId::from_u128(2), "x", Some(key));
        let mut y = member(NodeId::from_u128(3), "y", Some(key));
        x.last_seen = 200;
        y.last_seen = 200; // a tie — the node_id tiebreak must settle it
        let first = one_row_per_endpoint_key(vec![&x, &y])[0].node_id;
        for _ in 0..20 {
            assert_eq!(one_row_per_endpoint_key(vec![&x, &y])[0].node_id, first);
            assert_eq!(one_row_per_endpoint_key(vec![&y, &x])[0].node_id, first);
        }
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

    /// THE FAILING INPUT. Three unreachable peers, `FANOUT = 2`, and a
    /// selection key that only advances when a dial SUCCEEDS. Measured on
    /// RuggedFox 2026-09-09: 74 dials to each of two peers in twenty minutes
    /// and zero to the other five, one of whose daemons was up throughout.
    ///
    /// The two rounds are driven through `select_round_peers` with the stamping
    /// rule the round applies, so this fails against the pre-fix code path
    /// (stamp only on success) and passes against the fixed one (stamp on every
    /// dial). It is the rotation `select_round_peers`' own doc promises.
    #[test]
    fn a_failed_dial_gives_up_its_slot_to_the_next_peer() {
        use std::collections::HashMap;
        // The attempt clock: peer -> when a round last spent a slot on it.
        let mut attempt: HashMap<&str, u64> = HashMap::new();
        attempt.insert("dead-a", 100);
        attempt.insert("dead-b", 200);
        attempt.insert("live-c", 300);

        let round = |attempt: &mut HashMap<&str, u64>, now: u64| -> Vec<&'static str> {
            let mut offline: Vec<(&'static str, u64)> =
                vec![("dead-a", 0), ("dead-b", 0), ("live-c", 0)]
                    .into_iter()
                    .map(|(p, _)| (p, attempt[p]))
                    .collect();
            offline.sort_by_key(|(p, _)| *p);
            let picked = select_round_peers(Vec::new(), offline, 2);
            // Every peer given a slot is stamped, whether or not it answered.
            for p in &picked {
                attempt.insert(p, now);
            }
            picked
        };

        let first = round(&mut attempt, 1_000);
        assert_eq!(first, vec!["dead-a", "dead-b"], "most stale first");

        let second = round(&mut attempt, 1_010);
        assert!(
            second.contains(&"live-c"),
            "the peer nobody has dialed must get a slot in round two, got {second:?} \
             — two unreachable peers holding both slots for ever is the starvation"
        );
    }

    /// The control: stamping ONLY on success is the pre-fix rule, and under it
    /// the third peer is never reached. Without this, the assertion above is
    /// satisfiable by any selection that happens to rotate, and the reader
    /// cannot see what was actually wrong.
    #[test]
    fn stamping_only_on_success_starves_every_other_peer() {
        use std::collections::HashMap;
        let mut clock: HashMap<&str, u64> = HashMap::new();
        clock.insert("dead-a", 100);
        clock.insert("dead-b", 200);
        clock.insert("live-c", 300);

        let mut ever_picked_c = false;
        for _ in 0..50 {
            let mut offline: Vec<(&'static str, u64)> =
                vec![("dead-a", 0), ("dead-b", 0), ("live-c", 0)]
                    .into_iter()
                    .map(|(p, _)| (p, clock[p]))
                    .collect();
            offline.sort_by_key(|(p, _)| *p);
            let picked = select_round_peers(Vec::new(), offline, 2);
            // The pre-fix rule: dead-a and dead-b never answer, so nothing is
            // stamped and their keys never move.
            if picked.contains(&"live-c") {
                ever_picked_c = true;
            }
        }
        assert!(
            !ever_picked_c,
            "this test documents the BUG: under success-only stamping the third \
             peer is starved for ever, which is why the attempt clock exists"
        );
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

    /// The rail the loop now checks. Named here so the formula's
    /// operating meaning is pinned next to the code that warns on it:
    /// at the shipped fanout/interval/threshold a mesh has room for 12
    /// online peers under the worst-case (no-relay) condition.
    #[test]
    fn the_online_population_rail_matches_the_shipped_constants() {
        let ceiling = max_online_peers_before_false_offline(
            FANOUT,
            DEFAULT_GOSSIP_INTERVAL,
            DEFAULT_OFFLINE_THRESHOLD,
        );
        assert_eq!(ceiling, 12, "fanout 2 × floor(60s / 10s)");
        assert!(12 > ceiling - 1);

        // A zero interval cannot be divided by; the formula must not
        // panic or report a rail of 0 (which would warn forever).
        assert_eq!(
            max_online_peers_before_false_offline(
                2,
                std::time::Duration::ZERO,
                DEFAULT_OFFLINE_THRESHOLD
            ),
            usize::MAX
        );
    }
}
