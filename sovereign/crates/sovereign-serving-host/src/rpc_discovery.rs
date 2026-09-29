// SPDX-License-Identifier: AGPL-3.0-or-later
//! RPC-worker discovery: one tick's scan of the roster for peers serving a
//! ggml RPC worker, and the endpoint each one is dialled at, beside the
//! eligibility gate the tick feeds (`crate::worker_eligibility`).
//!
//! Moved from the svrn daemon (`EmbeddedDaemon::discover_rpc_workers`,
//! pb-serve-distributes): it reads the roster through the one membership
//! port (`MembershipReader`) and reaches peers through `PeerTransport`, so
//! the process that loads the distributed primary can run it over whichever
//! roster and transport its composition hands it.

use std::sync::Arc;

use kernel_types::NodeId;
use mesh_reach::{PeerContact, PeerTransport, TrafficClass};
use sovereign_contracts::daemon_wire::mesh::MemberStatus;
use sovereign_contracts::membership::MembershipReader;

mod endpoint;
#[cfg(test)]
mod tests;

/// How RPC-worker discovery uses the iroh bridge for ggml's raw-TCP
/// endpoint (`SOVEREIGN_RPC_TUNNEL`): `auto` (default) bridges only when
/// no direct member IP answers; `always` prefers the bridge (E2E forcing,
/// known-cross-network meshes); `never` disables bridging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RpcTunnelMode {
    Auto,
    Always,
    Never,
}

/// Pure parse — unit-testable without touching the process environment.
fn rpc_tunnel_mode_from(v: Option<&str>) -> RpcTunnelMode {
    match v.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("always") => RpcTunnelMode::Always,
        Some("never") | Some("off") | Some("0") => RpcTunnelMode::Never,
        None | Some("") | Some("auto") => RpcTunnelMode::Auto,
        Some(other) => {
            tracing::warn!(
                value = %other,
                "SOVEREIGN_RPC_TUNNEL: unknown value, using `auto` (accepted: auto|always|never)"
            );
            RpcTunnelMode::Auto
        }
    }
}

fn rpc_tunnel_mode() -> RpcTunnelMode {
    rpc_tunnel_mode_from(std::env::var("SOVEREIGN_RPC_TUNNEL").ok().as_deref())
}

/// A worker endpoint choice carried across discovery ticks so a single transient
/// probe miss can't flip a healthy worker's transport identity. `direct_misses`
/// counts consecutive ticks a *proven* direct-ip endpoint was unreachable while
/// we held it (reset the moment direct-ip answers again).
#[derive(Debug, Clone, PartialEq)]
struct StickyEndpoint {
    endpoint: String,
    via: String,
    direct_misses: u32,
}

impl StickyEndpoint {
    /// A direct raw-TCP endpoint to a member IP — the only transport we hold
    /// through a blip. The `via` label is the source of truth (set at selection).
    fn is_direct(&self) -> bool {
        self.via == "direct-ip"
    }

    /// A loopback endpoint served by an iroh bridge to the peer.
    fn is_bridge(&self) -> bool {
        self.via.starts_with("iroh-bridge")
    }
}

/// How a discovery tick should re-establish the endpoint of a peer we already
/// hold a choice for. Split out from the IO so the "never re-probe a known
/// worker over the link its own tensors are saturating" rule is a unit-testable
/// policy rather than a branch buried in a 200-line async method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reaffirm {
    /// Re-use the held endpoint verbatim — no network at all.
    Held,
    /// Re-resolve the peer's iroh bridge. Loopback-local against the transport's
    /// bridge cache: no WAN round-trip, so congestion can't starve it.
    Rebridge,
    /// Nothing worth re-affirming — run the full `/status` probe.
    FullProbe,
}

/// The re-affirm policy for one peer, given last tick's held choice.
///
/// Both known-worker cases rest on the SAME evidence: the peer already passed
/// this tick's gossip-Online + dialable membership filter, and gossip rides a
/// separate path with a looser budget than any probe we could run here. What a
/// probe would add is not liveness but noise — it rides the very link the RPC
/// tensor traffic is saturating.
///
/// For a bridged worker that noise was load-bearing (2026-07-26 tunnel e2e): the
/// `/status` probe travels the same iroh path as the tunnel, and each timeout
/// left `fresh = None`, which `sticky_endpoint` turns into "worker absent" for a
/// non-direct endpoint — read downstream as a flap. The endpoint never moved
/// (`127.0.0.1:40021` for six straight minutes) yet the tracker logged
/// `flaps=9 quarantine_count=5 cooldown_secs=300`, excluding a peer that was
/// serving the whole time. Re-minting the bridge instead touches only loopback.
///
/// A dead rpc-server behind live gossip is NOT this function's problem in either
/// case — it surfaces when ggml's RPC connection fails, via supervised reload
/// (DAEMON_RESILIENCE P0.4), not via a discovery probe.
///
/// **Stated trade-off:** a bridged worker is never re-probed for a direct IP, so
/// under `auto` a peer that fell back to the tunnel stays on it rather than
/// upgrading back to raw LAN TCP. This is deliberate and narrow: `auto` prefers
/// direct-ip at selection, so becoming bridged at all means direct was
/// unreachable at first sight; cross-network peers (the case this path exists
/// for) can never be direct; `always` wants the tunnel by definition; and the
/// pin clears on the peer's next Offline→Online cycle, which prunes stickiness.
/// The upgrade probe is deferrable, but if added it must be an UPGRADE ONLY —
/// its failure may never drop the worker, or it re-opens the flap this closed.
fn reaffirm_plan(prev: Option<&StickyEndpoint>, tunnel: RpcTunnelMode) -> Reaffirm {
    match prev {
        Some(p) if p.is_direct() => Reaffirm::Held,
        // `never` means the operator has opted out of bridging; re-probe so the
        // worker can move to a direct address (or drop out) rather than be
        // pinned to a tunnel we're no longer allowed to use.
        Some(p) if p.is_bridge() && tunnel != RpcTunnelMode::Never => Reaffirm::Rebridge,
        _ => Reaffirm::FullProbe,
    }
}

/// Consecutive direct-ip probe misses tolerated before a worker's endpoint is
/// allowed to flip to a fallback transport (iroh-bridge / probe-host) or be
/// dropped. Default 3 — roughly three ~15s discovery ticks (~45s) of a proven
/// direct-ip being unreachable before we treat the address as durably changed.
/// Env-overridable for pathological links; clamped to ≥1 (0 would disable the
/// guard and re-introduce the flip-on-one-miss bug).
fn rpc_endpoint_flip_threshold() -> u32 {
    std::env::var("SOVEREIGN_RPC_ENDPOINT_FLIP_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(3)
}

/// Hysteresis over the per-tick endpoint selection: a proven **direct-ip**
/// endpoint is not demoted to a fallback — nor dropped — on a transient miss.
/// We hold it for up to `flip_threshold` consecutive misses so a single
/// congested-Wi-Fi probe timeout can't flip the endpoint STRING that the
/// eligibility tracker and the reload loop key on (which reads as a flap +
/// full re-settle → live distribution collapses to local-only, 2026-07-19
/// 122B e2e).
///
/// - `prev`: last tick's held choice for this node (`None` on first sight).
/// - `fresh`: what raw probing selected THIS tick — `(endpoint, via)` — or
///   `None` when nothing was reachable at all.
///
/// Returns the choice to advertise this tick, or `None` to drop the worker.
/// A bridge/probe-host worker (no proven direct-ip to protect) is dropped the
/// moment it's unreachable — only direct-ip gets the hold.
fn sticky_endpoint(
    prev: Option<&StickyEndpoint>,
    fresh: Option<(String, String)>,
    flip_threshold: u32,
) -> Option<StickyEndpoint> {
    // Would holding `prev` for one more miss stay within budget?
    let can_hold = |p: &StickyEndpoint| p.is_direct() && p.direct_misses + 1 < flip_threshold;
    let held = |p: &StickyEndpoint| StickyEndpoint {
        endpoint: p.endpoint.clone(),
        via: p.via.clone(),
        direct_misses: p.direct_misses + 1,
    };
    match fresh {
        // Direct-ip verified reachable this tick — always take it, reset misses.
        Some((endpoint, via)) if via == "direct-ip" => Some(StickyEndpoint {
            endpoint,
            via,
            direct_misses: 0,
        }),
        // A fallback was selected → direct-ip missed. Hold the proven direct-ip
        // through the blip if we can; otherwise accept the fallback.
        Some((endpoint, via)) => match prev {
            Some(p) if can_hold(p) => Some(held(p)),
            _ => Some(StickyEndpoint {
                endpoint,
                via,
                direct_misses: 0,
            }),
        },
        // Nothing reachable at all. Hold a proven direct-ip through a transient
        // total miss; otherwise the worker is gone this tick.
        None => match prev {
            Some(p) if can_hold(p) => Some(held(p)),
            _ => None,
        },
    }
}

/// Mint (or reuse — the transport caches one bridge per peer per ALPN) a
/// bridge-local endpoint for `member`'s ggml rpc-server via the
/// `RpcTensor` traffic class. Returns `("127.0.0.1:<port>", via_label)` —
/// the scheme is stripped because ggml dials the authority verbatim.
/// `None` when the transport has no iroh path to the peer (plaintext
/// mesh, no pubkey, class pinned to ip).
///
/// Deliberately NOT TCP-probed: a loopback bridge accepts instantly
/// regardless of whether the peer is dialable, so a connect probe is a
/// false positive by construction. The peer's gossip-Online status (a
/// prerequisite for reaching this code) plus the eligibility settle gate
/// is the liveness evidence — the same ≤1-discovery-tick exposure window
/// raw-TCP workers already have.
async fn bridge_rpc_endpoint(
    transport: &Arc<dyn PeerTransport>,
    member: &PeerContact,
) -> Option<(String, String)> {
    let candidates = transport.endpoints(member, TrafficClass::RpcTensor).await;
    let ep = candidates.into_iter().next()?;
    let authority = ep.base_url.strip_prefix("http://")?.to_string();
    // The bridge hands back a loopback authority; anything else means a
    // transport misroute — refuse rather than hand ggml a bad endpoint.
    let addr: std::net::SocketAddr = authority.parse().ok()?;
    if !addr.ip().is_loopback() {
        tracing::warn!(
            endpoint = %authority,
            label = %ep.label,
            "rpc bridge endpoint is not loopback — refusing (transport misroute?)"
        );
        return None;
    }
    Some((authority, format!("iroh-bridge:{}", ep.label)))
}

impl RpcWorkerDiscovery {
    /// One discovery tick over `roster`, reaching peers through `transport`;
    /// `self_id` is left out of the scan.
    pub async fn discover(
        &self,
        roster: &dyn MembershipReader<Dial = PeerContact>,
        transport: &Arc<dyn PeerTransport>,
        self_id: NodeId,
    ) -> crate::worker_eligibility::DiscoveryOutcome {
        let members: Vec<_> = {
            roster
                .members()
                .await
                .into_iter()
                .filter(|m| m.node_id != self_id)
                .filter(|m| matches!(m.status, MemberStatus::Online | MemberStatus::Busy))
                .filter(|m| m.dialable)
                // Anchor-tier gate: only pull peers that declare themselves
                // shared-model anchors into the RPC layer-split. A peer that
                // explicitly advertises `can_anchor = false` is a consumer and
                // is excluded; legacy peers (no `anchor` field) get the benefit
                // of the doubt — they're still gated downstream by whether they
                // actually advertise an `rpc_worker` port.
                .filter(|m| m.capabilities.anchor.as_ref().is_none_or(|a| a.can_anchor))
                .collect()
        };

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(800))
            .build()
        {
            Ok(c) => c,
            Err(_) => return crate::worker_eligibility::DiscoveryOutcome::default(),
        };

        // Node ids of the currently gossip-Online members — used to prune sticky
        // endpoints for peers that have since gone offline, so a peer that changed
        // address while away is re-probed fresh on its return rather than
        // re-affirmed from stale cache.
        let online_ids: std::collections::HashSet<NodeId> =
            members.iter().map(|m| m.node_id).collect();
        // Snapshot of the worker history, read once: membership here is what
        // makes an unanswered probe reportable as "no statement" instead of
        // "absent".
        let known_workers: std::collections::HashMap<NodeId, std::time::Instant> = self
            .rpc_worker_last_seen
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        let polled = members.len();
        let mut unconfirmed: Vec<NodeId> = Vec::new();
        let mut out = Vec::new();
        for m in members {
            let name = m.name.clone();
            let node_id = m.node_id;
            // Stickiness identity + hold budget, read once up front — it decides
            // whether we even need the heavy `/status` re-probe this tick.
            let prev = self
                .rpc_worker_sticky
                .read()
                .ok()
                .and_then(|sticky| sticky.get(&node_id).cloned());
            let flip_threshold = rpc_endpoint_flip_threshold();

            // Fresh discovery for THIS tick — the endpoint ggml should dial, if we
            // can confirm it now. Stays `None` when the probe fails; because `m`
            // already passed the gossip-Online + dialable filter above, a `None`
            // means a transient probe blip on a LIVE peer, not a death, and the
            // stickiness guard below holds the last-good direct-ip rather than
            // dropping the worker and collapsing a live distribution. Two flap
            // sources feed this, both observed mid-decode 2026-07-19: a 600ms
            // direct-ip miss, and a starved `/status` probe (3 straight misses at
            // ~774ms gossip RTT under decode load) that dropped the peer entirely.
            let mut fresh: Option<(String, String)> = None;
            match reaffirm_plan(prev.as_ref(), rpc_tunnel_mode()) {
                // KNOWN direct-ip worker still gossip-Online (it passed the Online +
                // dialable membership filter above): re-affirm its cached endpoint
                // WITHOUT any network probe. Measured 2026-07-19: under active decode
                // the RPC tensor traffic saturates the shared Wi-Fi link, so EVERY
                // probe to the worker — /status HTTP *and* a raw TCP connect — times
                // out for the whole inference and, after `flip_threshold` misses,
                // drops a worker that is in fact alive and serving (tensors flowed at
                // ~8.7 tok/s while both probe types failed 3× straight, yet gossip
                // reach to the same peer stayed 58–143ms throughout). Gossip rides a
                // separate path + a looser budget and survives that load, so
                // gossip-Online membership IS the liveness signal for a known worker.
                // A moved endpoint is re-learned on the next Offline→Online cycle
                // (sticky is pruned for offline nodes after the loop); a dead
                // rpc-server with live gossip surfaces via the ggml RPC connection
                // failing → supervised reload (P0.4), not a discovery probe.
                Reaffirm::Held => {
                    fresh = prev.as_ref().map(|p| (p.endpoint.clone(), p.via.clone()));
                }
                // KNOWN bridged worker: re-mint its loopback endpoint straight from
                // the transport's bridge cache — same gossip-as-liveness argument,
                // and the `/status` probe it replaces rides the SAME iroh path as
                // the tunnel it would be checking (so decode load starves it on a
                // worker that is serving fine, and a non-direct endpoint has no
                // stickiness to survive the miss — see `reaffirm_plan`).
                Reaffirm::Rebridge => {
                    fresh = bridge_rpc_endpoint(&transport, &m.dial).await;
                }
                Reaffirm::FullProbe => {}
            }
            // Where the worker listens, as the roster's anchor record says it
            // (pb-serve-distributes): cw-rails' roster has no daemon `/status`
            // behind it, so a worker it names is chosen without one. There is
            // no probe host then, so no last-resort `probe-host` endpoint.
            let advertised = m
                .capabilities
                .anchor
                .as_ref()
                .and_then(|a| a.rpc_port.map(|port| (port, a.rpc_iroh)));
            if let (None, Some((rpc_port, iroh_advertised))) = (&fresh, advertised) {
                tracing::debug!(peer = %name, rpc_port, iroh_advertised, "rpc-discovery: the roster names this worker's port");
                fresh = endpoint::select_rpc_endpoint(
                    transport,
                    &m.dial,
                    rpc_port,
                    iroh_advertised,
                    None,
                )
                .await;
            } else if fresh.is_none() {
                // UNKNOWN worker (initial discovery) on a peer whose anchor record
                // names no rpc port (an older build), a probe-host worker, or a
                // bridged one whose iroh path just vanished (it may have moved onto
                // the LAN): run the full `/status` probe + endpoint selection.
                tracing::debug!(peer = %name, "rpc-discovery: no rpc port on the roster — reading the peer's /status");
                let probes = transport
                    .endpoints(&m.dial, TrafficClass::StatusProbe)
                    .await;
                for probe in &probes {
                    let status_url = format!("{}/status", probe.base_url);
                    // Fallback host only: the RPC worker speaks raw TCP and needs an
                    // IP-overlay address, but when `status_probe` is routed over iroh
                    // this probe authority is a loopback proxy (`127.0.0.1`). We
                    // prefer a direct member IP below (`reachable_rpc_endpoint`) and
                    // use this parsed probe host only when no advertised IP is reachable.
                    let Some(host) = probe
                        .base_url
                        .strip_prefix("http://")
                        .and_then(|a| a.rsplit_once(':'))
                        .map(|(host, _)| host.to_string())
                    else {
                        continue;
                    };
                    let Ok(resp) = client.get(&status_url).send().await else {
                        continue; // /status timed out — leave `fresh` None (blip guard below)
                    };
                    if !resp.status().is_success() {
                        continue;
                    }
                    let Ok(json) = resp.json::<serde_json::Value>().await else {
                        continue;
                    };
                    let Some(port) = json
                        .get("rpc_worker")
                        .and_then(|w| w.get("port"))
                        .and_then(|p| p.as_u64())
                    else {
                        continue;
                    };
                    let rpc_port = port as u16;
                    let iroh_advertised = json
                        .get("rpc_worker")
                        .and_then(|w| w.get("iroh"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    fresh = endpoint::select_rpc_endpoint(
                        transport,
                        &m.dial,
                        rpc_port,
                        iroh_advertised,
                        Some(host.as_str()),
                    )
                    .await;
                    break; // one reachable address per peer suffices
                }
            }
            let Some(choice) = sticky_endpoint(prev.as_ref(), fresh, flip_threshold) else {
                // Nothing to hold (no prior direct-ip, or the hold budget is
                // spent). We could not CONFIRM a worker here — which is not the
                // same statement as "there is no worker here". The peer passed
                // the gossip-Online + dialable filter above, so if we have ever
                // seen a worker on it, report the tick as unconfirmed and let
                // the eligibility layer hold its prior state (bounded by
                // `absence_grace`) rather than record a flap.
                if let Ok(mut sticky) = self.rpc_worker_sticky.write() {
                    sticky.remove(&node_id);
                }
                if known_workers.contains_key(&node_id) {
                    unconfirmed.push(node_id);
                }
                continue;
            };

            if choice.direct_misses > 0 {
                tracing::info!(
                    peer = %name,
                    endpoint = %choice.endpoint,
                    via = %choice.via,
                    miss = choice.direct_misses,
                    flip_threshold,
                    "rpc-discovery: probe miss on a gossip-Online worker — holding last-good endpoint (transient-blip guard)"
                );
            } else if choice.via == "probe-host" {
                tracing::warn!(
                    peer = %name,
                    endpoint = %choice.endpoint,
                    "no reachable direct IP and no iroh bridge for RPC worker; falling back to probe host (may be an iroh loopback proxy — distribution likely to fail)"
                );
            } else {
                tracing::info!(
                    peer = %name,
                    endpoint = %choice.endpoint,
                    via = %choice.via,
                    "discovered mesh RPC worker"
                );
            }

            // Record which member owns this endpoint BEFORE identity is dropped
            // into the bare-string RPC layer — the warm orchestrator resolves the
            // worker's mesh transport through this.
            if let Ok(mut dir) = self.rpc_endpoint_nodes.write() {
                dir.insert(choice.endpoint.clone(), node_id);
            }
            if let Ok(mut sticky) = self.rpc_worker_sticky.write() {
                sticky.insert(node_id, choice.clone());
            }
            if let Ok(mut seen) = self.rpc_worker_last_seen.write() {
                seen.insert(node_id, std::time::Instant::now());
            }
            out.push(crate::worker_eligibility::DiscoveredWorker {
                node_id,
                endpoint: choice.endpoint,
            });
        }
        // Prune sticky endpoints for peers that are no longer gossip-Online, so a
        // returning peer with a changed address is re-probed fresh (see `online_ids`).
        if let Ok(mut sticky) = self.rpc_worker_sticky.write() {
            sticky.retain(|nid, _| online_ids.contains(nid));
        }
        // Same pruning for the worker-history map, and it is load-bearing: a
        // peer gossip has dropped is no longer "known", so it stops being
        // eligible for an unconfirmed hold and its absence becomes POSITIVE
        // evidence on the next tick. That is what keeps `kill -9` of a worker
        // daemon converging at the pre-2026-07-28 speed (P0.4 acceptance).
        if let Ok(mut seen) = self.rpc_worker_last_seen.write() {
            seen.retain(|nid, _| online_ids.contains(nid));
        }
        crate::worker_eligibility::DiscoveryOutcome {
            workers: out,
            unconfirmed,
            // Engagement is the CALLER's knowledge (only the discovery loop
            // knows what its compute child is doing) — folded in there.
            engaged: Vec::new(),
            polled,
            scanned: true,
        }
    }

    /// Which mesh member owns `endpoint` (a discovered `ip:port` ggml-RPC
    /// worker endpoint), if discovery recorded one. Env-configured workers
    /// (`SOVEREIGN_RPC_WORKERS`) have no entry — callers fall back to raw-IP
    /// addressing for those.
    pub fn endpoint_node(&self, endpoint: &str) -> Option<NodeId> {
        self.rpc_endpoint_nodes
            .read()
            .ok()
            .and_then(|dir| dir.get(endpoint).copied())
    }
}

/// Discovery's memory across ticks: the endpoint each worker was last held
/// at, the workers ever confirmed, and which member owns each endpoint.
#[derive(Default)]
pub struct RpcWorkerDiscovery {
    /// Per-node sticky endpoint choice for RPC-worker discovery — the hysteresis
    /// state that stops a single transient direct-ip probe miss from flipping a
    /// worker's transport identity (direct-ip ↔ iroh-bridge loopback). Both the
    /// eligibility tracker and the reload loop key on the endpoint the discovery
    /// tick returns, so an unheld flip reads as a flap + full re-settle and
    /// collapses a live distribution to local-only (observed 2026-07-19, 122B
    /// e2e). Keyed by the worker's stable mesh node_id. `std::sync` lock — never
    /// held across an await (read to a clone, decide, write the result).
    rpc_worker_sticky: std::sync::RwLock<std::collections::HashMap<NodeId, StickyEndpoint>>,
    /// Peers we have EVER confirmed an RPC worker on, and when.
    ///
    /// Independent of `rpc_worker_sticky` on purpose. That map's hold budget is
    /// about endpoint STABILITY and it drops a bridged worker on its first miss
    /// (`sticky_endpoint`), so using it as the "do we know this peer?" set would
    /// make an unconfirmed hold last exactly one tick — useless for the bridged,
    /// multi-tick starvation that is the actual 2026-07-28 incident. This map
    /// answers a different question: have we ever seen a worker here, so that an
    /// unanswered probe is reportable as `unconfirmed` rather than as absence.
    rpc_worker_last_seen: std::sync::RwLock<std::collections::HashMap<NodeId, std::time::Instant>>,
    /// Endpoint→NodeId directory for discovered RPC workers: which mesh
    /// member owns each raw `ip:port` ggml-RPC endpoint. Written by
    /// [`RpcWorkerDiscovery::discover`] at the moment the endpoint string is
    /// derived — the one place identity and endpoint meet before identity
    /// is dropped into the bare-string RPC layer. Read by the warm
    /// orchestrator (`rpc_warm_http`) to resolve a worker's mesh transport
    /// (iroh bridge on an encrypted mesh) instead of reverse-parsing an IP
    /// from the endpoint string. Entries are never pruned: resolution
    /// re-reads the live membership, so a mapping for a vanished worker is
    /// inert. `std::sync` lock — never held across an await.
    rpc_endpoint_nodes: std::sync::RwLock<std::collections::HashMap<String, NodeId>>,
}

/// The mesh as one tick finds it: the roster, the transport to dial
/// through, and this node's id.
pub struct MeshNow {
    pub roster: Arc<dyn MembershipReader<Dial = PeerContact>>,
    pub transport: Arc<dyn PeerTransport>,
    pub self_id: NodeId,
}

/// Reads the mesh per tick: `None` while this node's mesh is not up (a
/// daemon's comes up after its engine, and a transport can be swapped under
/// a running one).
pub type MeshReader =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Option<MeshNow>> + Send + Sync>;

/// What the process that loads a distributed primary hands its discovery
/// loop (pb-serve-distributes): how to read the mesh, where to publish the
/// host role, and discovery's memory, which the warm orchestrator resolves
/// endpoints through. Until the flip, the svrn daemon hands its own mesh
/// (phase-b-33).
#[derive(Clone)]
pub struct MeshPorts {
    pub mesh: MeshReader,
    /// Told on every host-role transition (`/v1/mesh/status` reports it).
    pub on_host_role: Arc<dyn Fn(bool) + Send + Sync>,
    pub discovery: Arc<RpcWorkerDiscovery>,
    /// Where this node serves model files to peers for a warm: the internal
    /// port and the reachable bases on it (`http://ip:port`).
    pub model_origin:
        Arc<dyn Fn() -> futures::future::BoxFuture<'static, ModelOrigin> + Send + Sync>,
    /// This node's mesh proof for a peer's internal route, as the header
    /// `(name, value)`; `None` on a mesh with no credential or a mesh that is
    /// not up (a reported absence, never a default).
    pub proof: Arc<
        dyn Fn() -> futures::future::BoxFuture<'static, Option<(&'static str, String)>>
            + Send
            + Sync,
    >,
}

/// Where this node serves model files: the internal port, and the
/// reachable `http://ip:port` bases on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOrigin {
    pub internal_port: u16,
    pub bases: Vec<String>,
}

impl MeshPorts {
    /// One discovery tick. A mesh that is not up was not scanned at all:
    /// `scanned: false` says the tick is evidence about NOTHING, rather than
    /// silently reading as "every worker is gone".
    pub async fn discover(&self) -> crate::worker_eligibility::DiscoveryOutcome {
        match (self.mesh)().await {
            Some(now) => {
                self.discovery
                    .discover(now.roster.as_ref(), &now.transport, now.self_id)
                    .await
            }
            None => {
                tracing::debug!("rpc-discovery: the mesh is not up — this tick scanned nothing");
                crate::worker_eligibility::DiscoveryOutcome::default()
            }
        }
    }
}
