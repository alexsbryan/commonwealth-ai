// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh RPC-worker discovery loop: every tick, discover workers over the
//! mesh the process hands it, gate them on eligibility, elect the
//! shared-model host, and warm-and-respawn (or reload) the distributed
//! primary on a worker-set change. Moved from the svrn daemon's bootstrap
//! (pb-serve-distributes); it reads the mesh only through `MeshPorts`
//! (sovereign-serving-host), so the process that loads the engine runs it.

use std::sync::Arc;

use kernel_types::NodeId;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_serving_host::rpc_discovery::MeshPorts;

use crate::containment::rpc_discovery_armed;
use crate::discovery_policy;
use crate::distributed_respawn::{respawn_distributed_primary, ChildDistributionState};

/// Spawn the mesh RPC-worker auto-discovery loop (opt-in via `SOVEREIGN_RPC_DISCOVER`).
pub fn spawn_rpc_worker_discovery(
    ports: MeshPorts,
    engine_handle: Option<Arc<EmbeddedLlamaCpp>>,
    distributed_slot: Option<Arc<crate::manager::DynamicChildSlot>>,
) {
    // Mesh RPC-worker auto-discovery. With `SOVEREIGN_RPC_DISCOVER` set, this
    // host periodically scans peers' `/status` for advertised RPC workers and
    // feeds them to the embedded engine's worker provider — so distributing a
    // model across the cluster needs no manual `SOVEREIGN_RPC_WORKERS` list.
    // (Applies on the next model load after discovery populates; an eagerly
    // loaded model picks workers up on reload — see register_rpc_workers.)
    if rpc_discovery_armed() {
        let snapshot = Arc::new(std::sync::RwLock::new(Vec::<String>::new()));
        sovereign_inference::embedded::set_rpc_worker_provider({
            let snap = Arc::clone(&snapshot);
            move || snap.read().map(|v| v.clone()).unwrap_or_default()
        });
        // Worker eligibility gate — only distribute to PROVEN-STABLE workers, so a
        // flapping worker can neither thrash the reload loop nor (by crashing
        // mid-compute) GGML_ABORT the host. See `sovereign_serving_host::worker_eligibility`.
        let eligibility = std::sync::Arc::new(
            sovereign_serving_host::worker_eligibility::WorkerEligibility::default(),
        );
        sovereign_serving_host::worker_eligibility::set_global(std::sync::Arc::clone(&eligibility));
        let ports_for_disco = ports.clone();
        let engine_for_reload = engine_handle.clone();
        let distributed_slot = distributed_slot.clone();
        // Close the loop between the two independent respawn authorities. The
        // supervisor restarts a crashed child with identical argv, and the
        // child re-reads its handoff from disk — so when the workers it names
        // are gone, every restart re-dials a corpse and re-aborts on a budget
        // only THIS loop can refresh (3 futile respawns in 48s, 2026-07-28).
        // The gate lets the supervisor ask us before paying for that.
        if std::env::var("SOVEREIGN_COMPUTE_SPAWN_GATE").as_deref() == Ok("0") {
            tracing::warn!(
                target: "compute_child",
                "distributed primary: spawn gate DISABLED by SOVEREIGN_COMPUTE_SPAWN_GATE=0"
            );
        } else if let Some(slot) = &distributed_slot {
            crate::distributed_respawn::install_spawn_gate(slot, Arc::clone(&snapshot));
        }
        // Distributed-primary child state, shared with the warm task because a
        // warm can take minutes of GGUF transfer and must never block the 15s
        // discovery tick.
        let child_state: Arc<std::sync::Mutex<ChildDistributionState>> =
            Arc::new(std::sync::Mutex::new(ChildDistributionState::default()));
        let child_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Supervised: a panic here used to silently freeze worker
        // discovery + shared-model host failover for the rest of the
        // process's life (DAEMON_RESILIENCE.md P0.4). Loop state
        // (last_loaded / debounce) resets on restart — rediscovery
        // reconverges within a tick.
        host_kit::supervise::spawn_supervised("rpc_worker_discovery", move || {
            let ports_for_disco = ports_for_disco.clone();
            let engine_for_reload = engine_for_reload.clone();
            let snapshot = Arc::clone(&snapshot);
            let eligibility = std::sync::Arc::clone(&eligibility);
            let distributed_slot = distributed_slot.clone();
            let child_state = Arc::clone(&child_state);
            let child_busy = Arc::clone(&child_busy);
            async move {
                // Live child lifecycle, read each tick for engagement evidence.
                // Subscribed once — the receiver survives every respawn/retire
                // (the channel is owned by the slot, not a generation).
                let child_lifecycle_rx = distributed_slot.as_ref().map(|s| s.subscribe());
                // `last_loaded` = worker set the resident primary was loaded across;
                // `current` = ELIGIBLE set seen last tick (for debounce — wait for it
                // to stop changing before paying a reload).
                let mut last_loaded: Vec<String> = Vec::new();
                let mut current: Vec<String> = Vec::new();
                let mut stable_since = std::time::Instant::now();
                // When the eligible set first went EMPTY. Retiring the child is
                // gated on this persisting, because the peer most likely to look
                // absent is the one busy serving our own model warm.
                let mut empty_since: Option<std::time::Instant> = None;
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
                    // Raw discovery → eligibility gate → only PROVEN-STABLE workers
                    // reach the provider + the reload decision. A flapping worker stays
                    // out of `workers`, so the set the debounce compares never
                    // oscillates on a flap — the source of the 11-reloads-in-27min thrash.
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
                    // First-party engagement evidence: a worker carrying our
                    // own child's warm or serving session cannot answer a probe
                    // for as long as it does (ggml's RPC server accepts one
                    // connection at a time) — but that traffic is a better
                    // probe than the probe. Feeding the engaged endpoints into
                    // the tick is what makes the eligibility grace independent
                    // of load duration: before this, every model needing >120s
                    // to load was killed mid-load by its own absence grace
                    // (2026-08-02, notes 92d55ceb/16fc9204).
                    if let (Some(slot), Some(rx)) = (&distributed_slot, &child_lifecycle_rx) {
                        // A warm in flight is actively moving shard bytes to
                        // the targets recorded at Respawn time.
                        if child_busy.load(std::sync::atomic::Ordering::SeqCst) {
                            if let Ok(st) = child_state.lock() {
                                raw.engaged.extend(st.attempted.iter().cloned());
                            }
                        }
                        // A live child holds loading/serving RPC sessions
                        // across its pinned endpoints. Degraded, Restarting and
                        // Failed deliberately do NOT vouch: those are exactly
                        // the states where the worker may be the thing that
                        // died, and discovery must be allowed to see it.
                        use crate::child::ChildLifecycle as Lc;
                        if matches!(
                            rx.borrow().lifecycle,
                            Lc::Starting | Lc::Warming | Lc::Serving
                        ) {
                            raw.engaged.extend(slot.pinned_endpoints());
                        }
                        raw.engaged.sort();
                        raw.engaged.dedup();
                    }
                    let now = std::time::Instant::now();
                    eligibility.observe_outcome(&raw, now);
                    let workers = eligibility.eligible(now); // sorted + deduped, eligible-only
                    if let Ok(mut w) = snapshot.write() {
                        *w = workers.clone();
                    }
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
                        current = workers.clone();
                        stable_since = std::time::Instant::now();
                    }
                    // Maintained every tick, not just on change, so the grace
                    // measures how long the set has ACTUALLY been empty.
                    empty_since = if current.is_empty() {
                        empty_since.or_else(|| Some(std::time::Instant::now()))
                    } else {
                        None
                    };
                    // Host-role decision, re-evaluated every tick over gossiped
                    // membership — this is the failover mechanism. A non-host anchor
                    // still discovers + keeps its eligibility warm above, but does NOT
                    // distribute below, so at most the elected leader assembles the
                    // split. `should_host` is deterministic over the anchor set, so
                    // all anchors converge without coordination; a minority that can't
                    // see quorum still won't load (the quorum gate holds it "forming").
                    // ONE call, not three reads and a partition function: the
                    // mesh handle owns the membership AND our identity, so it
                    // is what can answer "am I the host" — and answering it
                    // here meant this binary linked the mesh substrate to ask
                    // about itself (cw-lift 3b). A mesh of one answers `true`,
                    // by election over a roster of one, not by a local branch.
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

                    // In child mode the loop can't track "what is loaded"
                    // locally — the warm completes asynchronously — so it reads
                    // the shared cell the warm task writes. The comparison is
                    // against the worker set we last ACTED ON, not the set that
                    // ended up warm: a warm can legitimately place on a subset
                    // (a worker that went ineligible between discovery and
                    // planning), and comparing against the subset would make
                    // `changed` true forever and respawn the child every tick.
                    let mut retry_due = false;
                    if distributed_slot.is_some() {
                        let st = child_state.lock().unwrap_or_else(|e| e.into_inner());
                        last_loaded = st.attempted.clone();
                        retry_due = st
                            .retry_at
                            .map(|t| std::time::Instant::now() >= t)
                            .unwrap_or(false);
                    }

                    // Reload when the worker set CHANGES (grow or shrink) vs what's
                    // loaded, once it's been stable briefly. A shrink prunes the dead
                    // worker's device on reload (live_device_list_if_pruning_needed).
                    let changed = current != last_loaded || retry_due;
                    // Shrink-fast-prune: if a worker the resident primary is loaded
                    // ACROSS dropped out of the eligible set, reload IMMEDIATELY — the
                    // dead worker must be pruned (live_device_list_if_pruning_needed)
                    // before it GGML_ABORTs the host mid-compute, and survivors' warm
                    // caches make the re-plan fast. A pure grow (new workers, all loaded
                    // ones still present) keeps the anti-thrash STABLE debounce.
                    let shrank = last_loaded.iter().any(|w| !current.contains(w));
                    if am_host && changed && shrank {
                        let lost: Vec<&String> = last_loaded
                            .iter()
                            .filter(|w| !current.contains(*w))
                            .collect();
                        tracing::info!(
                            ?lost,
                            "shared-model: anchor dropped — reloading now to prune + re-form on survivors"
                        );
                    }
                    match (&distributed_slot, &engine_for_reload) {
                        // ── Child mode: the primary lives in a supervised
                        // child, so a worker-set change is a KILL + RESPAWN,
                        // never an in-place reload. An in-place reload has to
                        // free the old sharded model's buffers on workers that
                        // may already be gone, and ggml's RPC client aborts the
                        // process on a dead endpoint — that is exactly how the
                        // daemon died on 2026-07-27 (note c4ef6fa0), from this
                        // very code path.
                        //
                        // The decision itself is `discovery_policy` — pure, so it
                        // can be exercised without a mesh. Only the EFFECTS live
                        // here.
                        (Some(slot), _) => {
                            let tick = discovery_policy::TickInputs {
                                am_host,
                                busy: child_busy.load(std::sync::atomic::Ordering::SeqCst),
                                current: &current,
                                last_loaded: &last_loaded,
                                retry_due,
                                stable_for: stable_since.elapsed(),
                                empty_for: empty_since.map(|t| t.elapsed()),
                                child_age: slot.spawned_at().map(|t| t.elapsed()),
                            };
                            match discovery_policy::decide_child_action(&tick) {
                                discovery_policy::ChildAction::Hold => {}
                                discovery_policy::ChildAction::Busy => {
                                    tracing::debug!(
                                        "distributed primary: warm/respawn already in flight — skipping this tick"
                                    );
                                }
                                // Stay unavailable rather than fall back to a
                                // local load that would starve the host.
                                discovery_policy::ChildAction::Retire { reason } => {
                                    slot.retire(&reason);
                                    if let Ok(mut st) = child_state.lock() {
                                        st.attempted.clear();
                                        st.retry_at = None;
                                    }
                                }
                                // Deliberately does NOT clear `attempted`.
                                // Leaving it populated is what keeps `changed`
                                // true so the grace is re-evaluated every tick —
                                // and what makes recovery free: when the worker
                                // returns, `current == attempted`, the tick is a
                                // plain Hold, and the still-serving child is
                                // never disturbed.
                                discovery_policy::ChildAction::WaitForWorkers {
                                    empty_for_secs,
                                    child_age_secs,
                                } => {
                                    tracing::info!(
                                        target: "compute_child",
                                        empty_for_secs,
                                        child_age_secs,
                                        "distributed primary: eligible set is empty — holding the \
                                         child while the grace burns down (a peer busy serving our \
                                         own warm looks absent)"
                                    );
                                }
                                discovery_policy::ChildAction::Respawn { workers } => {
                                    child_busy.store(true, std::sync::atomic::Ordering::SeqCst);
                                    // Record the attempt BEFORE it runs, so the
                                    // next tick compares against it and does not
                                    // queue a second warm behind this one.
                                    if let Ok(mut st) = child_state.lock() {
                                        st.attempted = workers.clone();
                                        st.retry_at = None;
                                    }
                                    let slot = Arc::clone(slot);
                                    let child_state = Arc::clone(&child_state);
                                    let child_busy = Arc::clone(&child_busy);
                                    // Detached: warming seeds every worker's
                                    // shard and can take minutes of GGUF
                                    // transfer. The 15s tick must keep running
                                    // (host election, eligibility) throughout.
                                    tokio::spawn(async move {
                                        respawn_distributed_primary(slot, workers, child_state)
                                            .await;
                                        child_busy
                                            .store(false, std::sync::atomic::Ordering::SeqCst);
                                    });
                                }
                            }
                        }
                        // ── In-process arm. Deliberately NOT sharing the policy
                        // function above: its consequences are different in kind
                        // (this `reload_primary` is the uncatchable GGML_ABORT of
                        // P0.4), and a shared decision would invite treating the
                        // two paths as interchangeable.
                        //
                        // Only the host distributes. A non-host anchor keeps its
                        // worker discovery + eligibility warm (above) so that, the
                        // moment it's elected host, `changed` vs its empty
                        // `last_loaded` triggers an immediate assemble on the
                        // already-settled survivors.
                        (None, engine) => {
                            if am_host
                                && changed
                                && (shrank || stable_since.elapsed() >= discovery_policy::STABLE)
                            {
                                match engine {
                                    Some(engine) => {
                                        tracing::info!(workers = ?current, "RPC worker set changed — reloading primary to redistribute");
                                        match engine.reload_primary().await {
                                            Ok(()) => last_loaded = current.clone(),
                                            Err(e) => {
                                                tracing::warn!(error = %e, "reload_primary failed; will retry next tick")
                                            }
                                        }
                                    }
                                    // No primary handle (provider build failed) —
                                    // keep the snapshot fresh so a later manual
                                    // load still picks workers up.
                                    None => last_loaded = current.clone(),
                                }
                            }
                        }
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
/// reads of this handle plus a `commonwealth_core::partition` call — which
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
    let am_host = commonwealth_core::partition::should_host(me, pin, &anchors);
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
