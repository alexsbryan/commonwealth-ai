// SPDX-License-Identifier: AGPL-3.0-or-later
//! The distributed primary's warm-then-respawn: warm the mesh workers the
//! discovery tick chose, then respawn the compute child across exactly the
//! set that warmed (moved from the daemon's bootstrap, pb-serve-distributes).

use std::sync::Arc;

/// Distributed-primary child state shared between the discovery loop and the
/// detached warm task.
#[derive(Default)]
pub struct ChildDistributionState {
    /// The eligible worker set the last warm+respawn ACTED ON — not the set
    /// that ended up warm. Comparing against the attempt is what keeps a
    /// partial placement (a worker that went ineligible between discovery and
    /// planning) from looking like a permanent "changed" and respawning the
    /// child on every tick.
    pub attempted: Vec<String>,
    /// When to try again after a refusal, even though nothing changed. Without
    /// it, one transient warm failure against an otherwise stable worker set
    /// would leave the primary down until a worker happened to join or leave.
    pub retry_at: Option<std::time::Instant>,
}

/// How long to wait before re-attempting a refused warm against an unchanged
/// worker set. Long enough that a genuinely-forming cluster isn't hammered,
/// short enough that a transient failure isn't a permanent outage.
const CHILD_WARM_RETRY: std::time::Duration = std::time::Duration::from_secs(120);

/// Warm the mesh workers for the distributed primary, then respawn the compute
/// child across exactly the set that warmed.
///
/// The split of labour is forced by what each process can reach: only the
/// daemon can warm (the orchestrator needs the mesh member directory, the iroh
/// transport bases, and the daemon's own ports), and only the child should
/// load (ggml's RPC client aborts the process it runs in when a worker dies).
/// So the daemon plans + warms, writes what it decided into a handoff file, and
/// the child loads against it.
///
/// The plan crosses with the worker list on purpose. The shard plan is cached
/// per `(model, worker set)` precisely because a worker's free VRAM shifts by
/// its own cached shard, so re-planning after a warm cuts the blocks
/// differently — and that cache is process-local. A child that re-planned would
/// miss every warm cache and fall back to bulk weight send (the send()
/// deadlock). Pinning the daemon's plan in the child keeps warm-time and
/// load-time placement identical across the process boundary.
pub async fn respawn_distributed_primary(
    slot: Arc<crate::manager::DynamicChildSlot>,
    workers: Vec<String>,
    child_state: Arc<std::sync::Mutex<ChildDistributionState>>,
) {
    use sovereign_inference::embedded::DistributedWarmOutcome;

    /// Park the slot unavailable and schedule one retry, so a refusal against
    /// an unchanged worker set is a delay, not a permanent outage.
    fn refuse(
        slot: &crate::manager::DynamicChildSlot,
        state: &std::sync::Mutex<ChildDistributionState>,
        reason: &str,
    ) {
        slot.retire(reason);
        if let Ok(mut st) = state.lock() {
            st.retry_at = Some(std::time::Instant::now() + CHILD_WARM_RETRY);
        }
    }

    /// Park the slot unavailable with **no retry timer**, for a refusal that
    /// waiting cannot fix.
    ///
    /// A cluster that is still forming resolves itself as anchors join, so
    /// [`refuse`] retries. A device whose assigned share exceeds its memory
    /// does not: re-planning the same model across the same devices produces
    /// the same overflow, and a timer just repeats the refusal on a schedule.
    ///
    /// This costs no new state and no new timer. The discovery loop already
    /// re-plans for free when the worker set changes (`changed = current !=
    /// last_loaded`), which is exactly — and only — when the answer could
    /// differ.
    fn park(
        slot: &crate::manager::DynamicChildSlot,
        state: &std::sync::Mutex<ChildDistributionState>,
        reason: &str,
    ) {
        slot.retire(reason);
        if let Ok(mut st) = state.lock() {
            st.retry_at = None;
        }
    }

    tracing::info!(
        target: "compute_child",
        workers = ?workers,
        model = %slot.model_path().display(),
        "distributed primary: warming worker shards before respawning the child"
    );

    let model_path = slot.model_path().to_path_buf();
    // The context the CHILD will load with — the warm path's memory projection
    // sizes KV from it, so it must be the child's real value, not a guess.
    let child_ctx = slot
        .context_size()
        .unwrap_or(crate::child_main::DEFAULT_CTX);
    // Blocking: the warm orchestrator bridges to async with `block_on` and can
    // run for minutes. It must not run on a runtime worker thread.
    let outcome = match tokio::task::spawn_blocking(move || {
        sovereign_inference::embedded::warm_distributed_primary(&model_path, child_ctx)
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "distributed primary: warm task panicked");
            refuse(&slot, &child_state, "warm task panicked");
            return;
        }
    };

    match outcome {
        DistributedWarmOutcome::Warm { endpoints, plan } => {
            let handoff = crate::distribution::DistributionHandoff {
                endpoints: endpoints.clone(),
                plan,
            };
            match slot.respawn_distributed(&handoff) {
                Ok(()) => {
                    tracing::info!(
                        target: "compute_child",
                        attempted = ?workers,
                        warmed = ?endpoints,
                        "distributed primary: child respawned across the warmed worker set"
                    );
                    // The attempt already recorded `workers`; clear the retry
                    // timer. Placing on a SUBSET of the eligible set is a
                    // normal outcome (a worker can go ineligible between
                    // discovery and planning) and must not read as "changed".
                    if let Ok(mut st) = child_state.lock() {
                        st.retry_at = None;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "distributed primary: respawn failed");
                    refuse(&slot, &child_state, "child respawn failed");
                }
            }
        }
        // Every refusal below is "stay unavailable" — the same posture the
        // in-process path takes with InsufficientCluster/LocalUnfit. Falling
        // back to a local load of a model this size is what collapsed the
        // desktop session on 2026-07-27.
        DistributedWarmOutcome::InsufficientCluster { eligible, quorum } => {
            let reason =
                format!("cluster forming — {eligible} eligible anchor(s), quorum {quorum}");
            tracing::info!(target: "compute_child", eligible, quorum, "distributed primary: {reason}");
            refuse(&slot, &child_state, &reason);
        }
        DistributedWarmOutcome::WorkerUnfit(overflow) => {
            // Parked, not retried: the cluster is fully formed and has the
            // memory in aggregate — one device just cannot hold what it was
            // assigned. Waiting changes nothing; a worker-set change re-plans
            // for free.
            tracing::warn!(
                target: "compute_child",
                device = overflow.device_index,
                endpoint = ?overflow.endpoint,
                held_mb = overflow.held_mb,
                need_mb = overflow.need_mb,
                capacity_mb = overflow.capacity_mb,
                "distributed primary: per-device fit refusal — parking (not retrying)"
            );
            park(&slot, &child_state, &overflow.refusal());
        }
        DistributedWarmOutcome::Unplannable => {
            refuse(
                &slot,
                &child_state,
                "could not plan the shards (no RPC device, unreadable GGUF, or unmappable worker)",
            );
        }
        DistributedWarmOutcome::WarmFailed { error } => {
            tracing::warn!(target: "compute_child", %error, "distributed primary: warm failed");
            refuse(&slot, &child_state, &format!("worker warm failed: {error}"));
        }
    }
}

/// Install the supervisor's spawn gate on the distributed slot, so a respawn
/// the supervisor would make with identical argv asks first whether a pinned
/// worker is still eligible (`eligible`, the discovery tick's snapshot) and
/// whether the host's share still fits in memory. Moved from the daemon's
/// discovery loop (pb-serve-distributes).
pub fn install_spawn_gate(
    slot: &crate::manager::DynamicChildSlot,
    snapshot: Arc<std::sync::RwLock<Vec<String>>>,
) {
    let snap = snapshot;
    // Sized once: the GGUF set does not change under a running daemon, and
    // the gate is re-polled every 2s while held — stat'ing every shard on
    // each poll would be pure waste.
    let model_bytes = sovereign_inference::embedded::total_model_bytes(slot.model_path());
    let gate_model_path = slot.model_path().to_path_buf();
    let gate_child_ctx = slot
        .context_size()
        .unwrap_or(crate::child_main::DEFAULT_CTX);
    slot.set_spawn_gate(Arc::new(move |ctx: &crate::manager::SpawnContext<'_>| {
        let eligible = snap.read().map(|v| v.clone()).unwrap_or_default();
        // The manually configured workers never enter the eligible
        // snapshot (discovery only adds to them), so the gate unions them
        // back in or it would hold a manual setup forever.
        let env = sovereign_inference::embedded::rpc_workers_from_env();
        // Two independent preconditions. The worker question came
        // first; the memory question exists because a respawn into a
        // footprint that did not fit is how a contained child crash
        // becomes an unusable machine (notes 309c841b, 92d55ceb).
        let worker = crate::discovery_policy::spawn_gate_verdict(ctx.pinned, &eligible, &env);
        match worker {
            crate::supervisor::SpawnVerdict::Hold { .. } => worker,
            crate::supervisor::SpawnVerdict::Allow => {
                // One sample for both terms — a reserve sized off one
                // reading and a fit judged against another is the
                // failure mode this subsystem already has six of.
                let (available, total) = sovereign_inference::embedded::system_memory_bytes();
                // llama.cpp's projected KV/compute terms — cached
                // after the first success, so the 2s re-poll while
                // held does not re-pay the projection.
                let overheads = sovereign_inference::embedded::projected_overheads(
                    &gate_model_path,
                    gate_child_ctx,
                    false,
                );
                crate::discovery_policy::memory_headroom_verdict(
                    crate::discovery_policy::host_share_need_bytes(
                        model_bytes,
                        ctx.local_blocks,
                        ctx.total_blocks,
                        overheads.as_ref(),
                    ),
                    available,
                    sovereign_inference::embedded::host_reserve_bytes_detected(total),
                )
            }
        }
    }));
}
