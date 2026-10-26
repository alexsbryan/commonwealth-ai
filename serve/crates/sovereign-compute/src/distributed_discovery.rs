// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh RPC-worker discovery loop: every tick, discover workers over the
//! mesh the process hands it, gate them on eligibility, and elect the
//! shared-model host. Moved from the svrn daemon's bootstrap
//! (pb-serve-distributes); it reads the mesh only through `MeshPorts`
//! (sovereign-serving-host), so the process that loads the engine runs it.
//!
//! Its two effects on a worker-set change, reloading the in-process
//! primary and warm-and-respawning the distributed-primary child, both
//! drove the embedded engine and were deleted with it (ENGINE_SWAP_PLAN
//! G2). What is left discovers, judges eligibility and publishes the host
//! role; the router distributes over `[compute] rpc_peers` instead.

use kernel_types::NodeId;
use sovereign_serving_host::rpc_discovery::{Distribute, MeshPorts};

use crate::containment::rpc_discovery_armed;

/// The distribution this loader runs once the process's mesh is up:
/// RPC-worker discovery.
pub fn distribute() -> Distribute {
    Box::new(move |ports: MeshPorts, _router| {
        tracing::info!(
            target: "compute_child",
            "distribution: RPC-worker discovery starts"
        );
        spawn_rpc_worker_discovery(ports);
    })
}

/// Spawn the mesh RPC-worker auto-discovery loop (opt-in via `SOVEREIGN_RPC_DISCOVER`).
pub fn spawn_rpc_worker_discovery(ports: MeshPorts) {
    // With `SOVEREIGN_RPC_DISCOVER` set, this host periodically scans peers'
    // `/status` for advertised RPC workers, keeps their eligibility, and
    // elects the shared-model host.
    if rpc_discovery_armed() {
        // Worker eligibility gate — only PROVEN-STABLE workers count. See
        // `sovereign_serving_host::worker_eligibility`.
        let eligibility = std::sync::Arc::new(
            sovereign_serving_host::worker_eligibility::WorkerEligibility::default(),
        );
        sovereign_serving_host::worker_eligibility::set_global(std::sync::Arc::clone(&eligibility));
        let ports_for_disco = ports.clone();
        // Supervised: a panic here used to silently freeze worker
        // discovery + shared-model host failover for the rest of the
        // process's life (DAEMON_RESILIENCE.md P0.4). Loop state resets on
        // restart — rediscovery reconverges within a tick.
        host_kit::supervise::spawn_supervised("rpc_worker_discovery", move || {
            let ports_for_disco = ports_for_disco.clone();
            let eligibility = std::sync::Arc::clone(&eligibility);
            async move {
                // The ELIGIBLE set seen last tick, so a change is logged once.
                let mut current: Vec<String> = Vec::new();
                // Designated-host pin (parsed once). When present + eligible it wins;
                // otherwise the host role is the elected leader of the anchors.
                let host_pin = std::env::var("SOVEREIGN_SHARED_MODEL_HOST_NODE_ID")
                    .ok()
                    .and_then(|s| NodeId::from_hex(&s));
                let allowlist = worker_allowlist();
                if let Some(list) = &allowlist {
                    tracing::info!(
                        ?list,
                        "shared-model: RPC worker allowlist active — non-matching discovered workers are excluded"
                    );
                }
                let mut was_host = false;
                loop {
                    // Raw discovery → eligibility gate → only PROVEN-STABLE workers.
                    let mut raw = ports_for_disco.discover().await;
                    if let Some(list) = &allowlist {
                        let keep_node =
                            |hex: &str| list.iter().any(|p| hex.starts_with(p.as_str()));
                        raw.workers.retain(|w| {
                            let hex = w.node_id.to_hex();
                            let keep = keep_node(&hex);
                            if !keep {
                                tracing::debug!(
                                    worker = %hex,
                                    endpoint = %w.endpoint,
                                    "shared-model: discovered worker excluded by SOVEREIGN_RPC_WORKER_ALLOWLIST"
                                );
                            }
                            keep
                        });
                        // The unconfirmed set must be filtered by the SAME rule,
                        // or an allowlist-excluded peer could hold eligibility
                        // state it is never allowed to have.
                        raw.unconfirmed.retain(|n| keep_node(&n.to_hex()));
                    }
                    let now = std::time::Instant::now();
                    eligibility.observe_outcome(&raw, now);
                    let workers = eligibility.eligible(now); // sorted + deduped, eligible-only
                    if workers != current {
                        tracing::info!(
                            eligible = workers.len(),
                            discovered = raw.workers.len(),
                            // `unconfirmed` vs `polled` is what makes an
                            // "eligible=0 discovered=0" line readable: it says
                            // whether we heard "no worker" or heard nothing.
                            unconfirmed = raw.unconfirmed.len(),
                            polled = raw.polled,
                            workers = ?workers,
                            "mesh RPC eligible-worker set changed"
                        );
                        current = workers;
                    }
                    // Host-role decision, re-evaluated every tick over gossiped
                    // membership — this is the failover mechanism.
                    // `should_host` is deterministic over the anchor set, so
                    // all anchors converge without coordination. ONE call: the
                    // mesh handle owns the membership AND our identity, so it
                    // is what can answer "am I the host" (cw-lift 3b). A mesh
                    // of one answers `true`, by election over a roster of one.
                    let role = host_role(&ports_for_disco, host_pin).await;
                    let am_host = role.am_host;
                    if am_host != was_host {
                        tracing::info!(
                            am_host,
                            anchors = role.eligible_anchors,
                            pinned = role.pinned,
                            "shared-model: host-role transition"
                        );
                        // Publish for `/v1/mesh/status` so the mesh soak can assert
                        // the no-split-brain invariant (≤1 host across the fleet).
                        (ports_for_disco.on_host_role)(am_host);
                        was_host = am_host;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
            }
        });
    }
}

/// Optional operator scoping of the distributable-worker pool:
/// `SOVEREIGN_RPC_WORKER_ALLOWLIST` = comma-separated node-id hex prefixes.
/// Absent or empty = no filter (every discovered worker is a candidate).
/// Exists for controlled measurements — when several peers advertise RPC
/// serving, this pins a distributed load to the worker under test instead of
/// sharding across whoever happens to be online (first need: the 2026-07-27
/// cloud-tensor-peer proof, where a LAN peer still advertising RPC from an
/// earlier experiment would have contaminated the WAN decode number).
fn worker_allowlist() -> Option<Vec<String>> {
    let raw = std::env::var("SOVEREIGN_RPC_WORKER_ALLOWLIST").ok()?;
    let list: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

/// Whether THIS node runs the shared-model HOST role right now, and what
/// the decision saw.
///
/// ONE accessor for a question the daemon used to assemble itself from two
/// reads of this handle plus a `kernel_types::partition` call — which
/// is how a binary with no mesh configured still had to link the mesh
/// substrate to answer a question about itself (cw-lift 3b). `pin` is the
/// operator-designated host (`[shared_model] host_node_id`); it wins only
/// while it is actually an eligible anchor, so a pinned host that drops
/// out fails over to election instead of stranding the cluster.
///
/// **A mesh of one is not a special case.** A roster of one elects its
/// only member, so a solo node hosts — the correct answer, reached by the
/// same code path a fleet takes. The one `false` that is not an election
/// result is an unresolved identity: we cannot be the elected leader of a
/// set we are not yet in. Over the mesh `ports` read this tick; a mesh that
/// is not up answers that way.
pub async fn host_role(ports: &MeshPorts, pin: Option<NodeId>) -> HostRole {
    let Some(now) = (ports.mesh)().await else {
        tracing::debug!(
            pinned = pin.is_some(),
            "shared-model: identity not resolved yet — not hosting"
        );
        return HostRole {
            am_host: false,
            eligible_anchors: 0,
            pinned: pin.is_some(),
        };
    };
    let me = now.self_id;
    let anchors = sovereign_contracts::membership::eligible_anchors(&now.roster.members().await);
    let am_host = kernel_types::partition::should_host(me, pin, &anchors);
    tracing::debug!(
        am_host,
        me = %me.to_hex(),
        eligible_anchors = anchors.len(),
        pinned = pin.is_some(),
        "shared-model: host-role decided"
    );
    HostRole {
        am_host,
        eligible_anchors: anchors.len(),
        pinned: pin.is_some(),
    }
}

/// The shared-model host decision, with the two inputs that produced it.
///
/// Returned whole rather than as a bare `bool` so a caller's log line and its
/// branch cannot come from two different membership snapshots — the anchor
/// count and the verdict are read once, together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostRole {
    /// Whether THIS node runs the host role right now.
    pub am_host: bool,
    /// Eligible anchors the decision saw, self included when self is one.
    pub eligible_anchors: usize,
    /// Whether an operator pin was supplied — not whether it won.
    pub pinned: bool,
}
