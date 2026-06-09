// SPDX-License-Identifier: AGPL-3.0-or-later
//! Coordinator-held work queue for pull-based corpus ingestion.
//!
//! Replaces the static "each peer gets a contiguous slice of the corpus"
//! partitioning with a queue of `WorkUnit`s that peers pull from one at a
//! time. Fast peers naturally pull more units — compute-weighting emerges
//! from the pull rate without any explicit scheduler knowing about peer
//! capacity. Per-unit leases with heartbeats give fault tolerance: a peer
//! that crashes mid-unit has its lease expire, the unit returns to the
//! queue, and another peer picks it up on its next pull.
//!
//! # State model
//!
//! All queue state is in-memory on the coordinator (`WorkQueueManager`).
//! Gossip holds only the `IngestionHandoff` announcement that tells peers
//! "there's a queue here to pull from" — not the queue itself. Per-unit
//! status would be too chatty for LWW gossip and we need linearizable
//! reads ("who holds the lease on unit 42?") that gossip can't provide.
//!
//! # Lifecycle
//!
//! 1. Coordinator's `corpus_collaborate` builds a `Vec<WorkUnit>` from the
//!    recipe, calls `WorkQueueManager::register(handoff_id, units, ...)`,
//!    gossips the `IngestionHandoff` with `phase: Open`.
//! 2. Peers discover the handoff on their auto_ingest gossip tick, spawn a
//!    pull loop per eligible handoff.
//! 3. Each pull loop calls `POST /internal/corpus/next_unit` → server
//!    forwards to `WorkQueueManager::next_unit(handoff, peer)` → returns a
//!    unit + lease expiry.
//! 4. Peer runs `ingest_with_overrides` stamping `unit_id` onto every
//!    chunk it writes. Heartbeats every `LEASE_MS / 3`.
//! 5. On completion, peer calls `complete_unit` → queue transitions the
//!    unit to `Complete`. When all units terminate, phase flips
//!    `Open → Draining → Merging` and the existing `coordinate_merge` runs.
//! 6. On lease expiry, the reaper re-queues the unit with `attempts` bumped.
//!
//! # Correctness
//!
//! Lease expiry races mean the same `unit_id` can be processed by two
//! peers. Both peers write chunks into their own `<corpus>-partition-<self>/`
//! directories. At merge time, `merge_partitions` in corpus-engine dedupes
//! by `unit_id` (the LanceDB column added alongside this module). This is
//! the correctness mechanism — don't remove it without replacing it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_core::knowledge::{
    CompleteOutcome, HandoffPhase, UnitId, UnitStatus, WorkUnit, LEASE_MS, MAX_UNIT_ATTEMPTS,
};
use commonwealth_core::oicp::EmbedModelInfo;

/// How often the reaper scans for expired leases.
const REAPER_INTERVAL: Duration = Duration::from_secs(30);

/// Error type for queue operations exposed to HTTP handlers.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueError {
    /// No queue registered for this handoff (404).
    NotFound,
    /// The lease for this unit was reclaimed before the operation completed
    /// (410 Gone on heartbeat, 409 Conflict on complete_unit).
    LeaseReclaimed,
    /// The provided `unit_id` doesn't match anything in the queue (400).
    UnknownUnit,
    /// Peer's embed model no longer matches the handoff's (409).
    /// Caller-side re-validation — the queue itself doesn't enforce this;
    /// it's thrown by the HTTP handler after cross-checking gossip.
    EmbedModelMismatch,
}

/// Result of a heartbeat — either the lease was renewed, or it was gone.
#[derive(Debug, Clone, PartialEq)]
pub enum HeartbeatResult {
    Renewed {
        expires_at_ms: u64,
    },
    /// Peer must abort the in-flight unit; reaper gave the lease to someone
    /// else (or the handoff finished). HTTP handler returns 410.
    Reclaimed,
}

/// Stats emitted by one reaper sweep — fed back through tracing so ops can
/// see when leases are expiring (indicates peer instability).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReapStats {
    pub requeued: u32,
    pub terminal_failed: u32,
    pub phase_transitions: u32,
}

// -----------------------------------------------------------------
// HandoffQueue — one per active handoff on the coordinator
// -----------------------------------------------------------------

/// All per-handoff queue state. Held inside the `WorkQueueManager`'s
/// `Mutex<HashMap<HandoffId, HandoffQueue>>`.
#[derive(Debug, Clone)]
pub struct HandoffQueue {
    pub corpus_id: String,
    pub recipe_id: String,
    pub embed_model: EmbedModelInfo,
    pub phase: HandoffPhase,
    /// Parallel arrays keyed by `UnitId` (= position). Using a Vec keeps
    /// `UnitId → (unit, status)` lookup O(1); `queued` tracks the
    /// currently-pullable subset for O(log n) pop.
    pub units: Vec<(WorkUnit, UnitStatus)>,
    /// UnitIds whose status is `Queued`. Popped on `next_unit`, pushed back
    /// on lease expiry. BTreeSet gives us deterministic FIFO-ish ordering
    /// (lowest unit_id first) which makes logs readable.
    pub queued: BTreeSet<UnitId>,
    /// Peers that ever successfully pulled a unit. Used by the merge leader
    /// to know which `<corpus>-partition-<peer>/` dirs to tar-fetch.
    pub participating_peers: HashSet<NodeId>,
    pub merge_leader: NodeId,
    pub created_at_ms: u64,
    pub last_mutation_ms: u64,
}

impl HandoffQueue {
    /// Count units in each terminal state. Used for phase transitions and
    /// progress reporting.
    pub fn terminal_counts(&self) -> (u32, u32) {
        let mut complete = 0u32;
        let mut failed = 0u32;
        for (_unit, status) in &self.units {
            match status {
                UnitStatus::Complete { .. } => complete += 1,
                UnitStatus::Failed { .. } => failed += 1,
                _ => {}
            }
        }
        (complete, failed)
    }

    /// True when no unit is still Queued or Leased.
    pub fn all_terminal(&self) -> bool {
        self.units
            .iter()
            .all(|(_u, s)| matches!(s, UnitStatus::Complete { .. } | UnitStatus::Failed { .. }))
    }

    /// True when the queue has no more work to hand out but some leases
    /// are still outstanding.
    pub fn draining(&self) -> bool {
        self.queued.is_empty()
            && self
                .units
                .iter()
                .any(|(_u, s)| matches!(s, UnitStatus::Leased { .. }))
    }
}

// -----------------------------------------------------------------
// WorkQueueManager — the type held by AppStateInner
// -----------------------------------------------------------------

/// Coordinator-side state for all active pull-based handoffs. One instance
/// per daemon, held as `Arc<WorkQueueManager>` inside `AppStateInner`.
pub struct WorkQueueManager {
    inner: Arc<Mutex<HashMap<HandoffId, HandoffQueue>>>,
}

impl WorkQueueManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new pull-based handoff. Called by `corpus_collaborate`
    /// after it builds the unit list from the recipe's work source.
    pub async fn register(
        &self,
        handoff_id: HandoffId,
        corpus_id: impl Into<String>,
        recipe_id: impl Into<String>,
        embed_model: EmbedModelInfo,
        units: Vec<WorkUnit>,
        merge_leader: NodeId,
    ) {
        let now = now_ms();
        let mut unit_slots: Vec<(WorkUnit, UnitStatus)> = Vec::with_capacity(units.len());
        let mut queued: BTreeSet<UnitId> = BTreeSet::new();
        for (i, unit) in units.into_iter().enumerate() {
            unit_slots.push((unit, UnitStatus::Queued { prior_attempts: 0 }));
            queued.insert(i as UnitId);
        }
        let queue = HandoffQueue {
            corpus_id: corpus_id.into(),
            recipe_id: recipe_id.into(),
            embed_model,
            phase: HandoffPhase::Open,
            units: unit_slots,
            queued,
            participating_peers: HashSet::new(),
            merge_leader,
            created_at_ms: now,
            last_mutation_ms: now,
        };
        let mut guard = self.inner.lock().await;
        guard.insert(handoff_id, queue);
    }

    /// Lease the next available unit to `peer`. Returns `None` when the
    /// queue is empty (caller should respond 204 with current phase).
    pub async fn next_unit(
        &self,
        handoff_id: &HandoffId,
        peer: NodeId,
    ) -> Result<Option<LeasedUnit>, QueueError> {
        let mut guard = self.inner.lock().await;
        let queue = guard.get_mut(handoff_id).ok_or(QueueError::NotFound)?;

        // No work left to lease — the handoff is in Draining/Merging/Complete.
        let Some(&unit_id) = queue.queued.iter().next() else {
            return Ok(None);
        };
        queue.queued.remove(&unit_id);

        let now = now_ms();
        let expires_at_ms = now + LEASE_MS;

        // Bump attempts on re-lease. First lease: prior_attempts=0 → attempts=1.
        // After a failed+requeued lease: prior_attempts=N → attempts=N+1.
        let attempts = match &queue.units[unit_id as usize].1 {
            UnitStatus::Queued { prior_attempts } => prior_attempts + 1,
            // Leased / Failed / Complete should not be in `queued` — defensive
            // fallbacks; treat as first lease.
            _ => 1,
        };

        let unit = queue.units[unit_id as usize].0.clone();
        queue.units[unit_id as usize].1 = UnitStatus::Leased {
            peer,
            leased_at_ms: now,
            last_heartbeat_ms: now,
            expires_at_ms,
            attempts,
        };
        queue.participating_peers.insert(peer);
        queue.last_mutation_ms = now;

        Ok(Some(LeasedUnit {
            unit_id,
            unit,
            lease_expires_at_ms: expires_at_ms,
        }))
    }

    /// Extend the lease for a unit the peer still holds. Returns
    /// `Reclaimed` if the lease was taken away by the reaper (peer must
    /// abort the in-flight unit).
    pub async fn heartbeat(
        &self,
        handoff_id: &HandoffId,
        peer: NodeId,
        unit_id: UnitId,
    ) -> Result<HeartbeatResult, QueueError> {
        let mut guard = self.inner.lock().await;
        let queue = guard.get_mut(handoff_id).ok_or(QueueError::NotFound)?;
        let slot = queue
            .units
            .get_mut(unit_id as usize)
            .ok_or(QueueError::UnknownUnit)?;

        match &mut slot.1 {
            UnitStatus::Leased {
                peer: holder,
                last_heartbeat_ms,
                expires_at_ms,
                ..
            } if *holder == peer => {
                let now = now_ms();
                *last_heartbeat_ms = now;
                *expires_at_ms = now + LEASE_MS;
                queue.last_mutation_ms = now;
                Ok(HeartbeatResult::Renewed {
                    expires_at_ms: *expires_at_ms,
                })
            }
            // Lease is held by someone else now, or already Complete/Failed,
            // or still Queued (re-leased hasn't happened yet but the old
            // lease was reaped). Peer must stop.
            _ => Ok(HeartbeatResult::Reclaimed),
        }
    }

    /// Mark a unit terminal. Returns the handoff's new phase so the caller
    /// can wake the merge task if we just hit `Merging`.
    ///
    /// `Complete`: the unit transitions to `UnitStatus::Complete`.
    /// `Failed`: re-queue with `attempts+1` until `MAX_UNIT_ATTEMPTS`, then
    /// terminal `Failed`.
    pub async fn complete_unit(
        &self,
        handoff_id: &HandoffId,
        peer: NodeId,
        unit_id: UnitId,
        outcome: CompleteOutcome,
        reason: Option<String>,
    ) -> Result<HandoffPhase, QueueError> {
        let mut guard = self.inner.lock().await;
        let queue = guard.get_mut(handoff_id).ok_or(QueueError::NotFound)?;
        let slot = queue
            .units
            .get_mut(unit_id as usize)
            .ok_or(QueueError::UnknownUnit)?;

        // The peer's lease must still be active for its completion to count.
        // If the reaper already requeued this unit, 409 back so the peer
        // knows its output is untrusted (another peer may have redone it).
        let attempts = match &slot.1 {
            UnitStatus::Leased {
                peer: holder,
                attempts,
                ..
            } if *holder == peer => *attempts,
            _ => return Err(QueueError::LeaseReclaimed),
        };

        let now = now_ms();
        match outcome {
            CompleteOutcome::Complete => {
                slot.1 = UnitStatus::Complete {
                    peer,
                    completed_at_ms: now,
                };
            }
            CompleteOutcome::Failed => {
                if attempts >= MAX_UNIT_ATTEMPTS {
                    slot.1 = UnitStatus::Failed {
                        last_peer: peer,
                        reason: reason.unwrap_or_else(|| "max attempts exceeded".to_string()),
                        attempts,
                    };
                } else {
                    // Preserve attempts across requeue — the next lease will
                    // be attempts+1, counting total work on this unit rather
                    // than resetting per-peer.
                    slot.1 = UnitStatus::Queued {
                        prior_attempts: attempts,
                    };
                    queue.queued.insert(unit_id);
                }
            }
        }
        queue.last_mutation_ms = now;

        // Recompute phase. The actual transition to Merging wakes the merge
        // task — that wiring lives in the caller (AppStateInner / routes).
        self.update_phase(queue);
        Ok(queue.phase.clone())
    }

    /// Scan all queues, re-queue leases whose `expires_at_ms` has passed.
    /// Called every `REAPER_INTERVAL` from `spawn_reaper`.
    pub async fn reap_expired(&self) -> ReapStats {
        let mut stats = ReapStats::default();
        let now = now_ms();
        let mut guard = self.inner.lock().await;
        for queue in guard.values_mut() {
            let prev_phase = queue.phase.clone();
            for (unit_id_usize, (_unit, status)) in queue.units.iter_mut().enumerate() {
                let UnitStatus::Leased {
                    peer,
                    expires_at_ms,
                    attempts,
                    ..
                } = status
                else {
                    continue;
                };
                if now <= *expires_at_ms {
                    continue;
                }
                let unit_id = unit_id_usize as UnitId;
                let attempts = *attempts;
                let peer = *peer;
                if attempts >= MAX_UNIT_ATTEMPTS {
                    *status = UnitStatus::Failed {
                        last_peer: peer,
                        reason: format!("lease expired after {attempts} attempts"),
                        attempts,
                    };
                    stats.terminal_failed += 1;
                } else {
                    *status = UnitStatus::Queued {
                        prior_attempts: attempts,
                    };
                    queue.queued.insert(unit_id);
                    stats.requeued += 1;
                }
                queue.last_mutation_ms = now;
            }
            self.update_phase(queue);
            if queue.phase != prev_phase {
                stats.phase_transitions += 1;
            }
        }
        stats
    }

    /// Spawn a long-lived reaper task. Returns a handle so the daemon
    /// shutdown path can abort it cleanly.
    pub fn spawn_reaper(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REAPER_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let stats = self.reap_expired().await;
                if stats.requeued > 0 || stats.terminal_failed > 0 || stats.phase_transitions > 0 {
                    tracing::info!(
                        requeued = stats.requeued,
                        terminal_failed = stats.terminal_failed,
                        phase_transitions = stats.phase_transitions,
                        "work_queue reaper: swept expired leases"
                    );
                }
            }
        })
    }

    /// Read-only snapshot for CLI / debug. Clones the queue — don't call
    /// this on hot paths.
    pub async fn snapshot(&self, handoff_id: &HandoffId) -> Option<HandoffQueue> {
        self.inner.lock().await.get(handoff_id).cloned()
    }

    /// True if this node is coordinating the given handoff. Used by
    /// endpoint handlers to return 503 when we don't own the queue.
    pub async fn has_handoff(&self, handoff_id: &HandoffId) -> bool {
        self.inner.lock().await.contains_key(handoff_id)
    }

    /// Drop the queue. Called after merge completes successfully so the
    /// coordinator doesn't hold stale state.
    pub async fn retire(&self, handoff_id: &HandoffId) {
        self.inner.lock().await.remove(handoff_id);
    }

    // -----------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------

    /// Recompute `queue.phase` from its unit statuses. Monotonic: never
    /// walks a queue backwards through phases.
    fn update_phase(&self, queue: &mut HandoffQueue) {
        // Terminal phases don't advance.
        if queue.phase.is_terminal() {
            return;
        }
        if queue.all_terminal() {
            // All units terminal → ready for merge.
            queue.phase = HandoffPhase::Merging;
        } else if queue.queued.is_empty() {
            // No work left to hand out, some leases still outstanding.
            queue.phase = HandoffPhase::Draining;
        } else {
            queue.phase = HandoffPhase::Open;
        }
    }
}

impl Default for WorkQueueManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Payload returned by `next_unit` on success.
#[derive(Debug, Clone, PartialEq)]
pub struct LeasedUnit {
    pub unit_id: UnitId,
    pub unit: WorkUnit,
    pub lease_expires_at_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;

    fn sample_model() -> EmbedModelInfo {
        EmbedModelInfo {
            model_id: "qwen-embedding-0.6b".to_string(),
            dimensions: 1024,
            pooling: commonwealth_core::oicp::PoolingStrategy::Mean,
            normalization: commonwealth_core::oicp::NormalizationStrategy::Application,
        }
    }

    fn peer(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    async fn fixture(unit_count: usize) -> (WorkQueueManager, HandoffId) {
        let mgr = WorkQueueManager::new();
        let handoff = HandoffId::generate();
        let units: Vec<WorkUnit> = (0..unit_count).map(WorkUnit::JsonlShard).collect();
        mgr.register(
            handoff,
            "wikipedia",
            "wikipedia",
            sample_model(),
            units,
            peer(1),
        )
        .await;
        (mgr, handoff)
    }

    #[tokio::test]
    async fn next_unit_rotates_through_queue() {
        let (mgr, handoff) = fixture(3).await;
        let a = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();
        let b = mgr.next_unit(&handoff, peer(3)).await.unwrap().unwrap();
        let c = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();
        let empty = mgr.next_unit(&handoff, peer(2)).await.unwrap();
        assert_eq!(a.unit_id, 0);
        assert_eq!(b.unit_id, 1);
        assert_eq!(c.unit_id, 2);
        assert!(empty.is_none(), "queue should be empty after 3 pulls");
    }

    #[tokio::test]
    async fn heartbeat_extends_lease() {
        let (mgr, handoff) = fixture(1).await;
        let leased = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();
        let first_expiry = leased.lease_expires_at_ms;
        // Force a visible delta.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let hb = mgr
            .heartbeat(&handoff, peer(2), leased.unit_id)
            .await
            .unwrap();
        match hb {
            HeartbeatResult::Renewed { expires_at_ms } => assert!(expires_at_ms > first_expiry),
            HeartbeatResult::Reclaimed => panic!("lease was still valid"),
        }
    }

    #[tokio::test]
    async fn heartbeat_from_wrong_peer_is_reclaimed() {
        let (mgr, handoff) = fixture(1).await;
        let leased = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();
        let hb = mgr
            .heartbeat(&handoff, peer(999), leased.unit_id)
            .await
            .unwrap();
        assert_eq!(hb, HeartbeatResult::Reclaimed);
    }

    #[tokio::test]
    async fn complete_transitions_unit_and_phase() {
        let (mgr, handoff) = fixture(2).await;
        let a = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();
        let b = mgr.next_unit(&handoff, peer(3)).await.unwrap().unwrap();

        let phase_after_a = mgr
            .complete_unit(
                &handoff,
                peer(2),
                a.unit_id,
                CompleteOutcome::Complete,
                None,
            )
            .await
            .unwrap();
        // One unit still Leased → phase is Open (queued is empty) → Draining.
        assert_eq!(phase_after_a, HandoffPhase::Draining);

        let phase_after_b = mgr
            .complete_unit(
                &handoff,
                peer(3),
                b.unit_id,
                CompleteOutcome::Complete,
                None,
            )
            .await
            .unwrap();
        // All terminal → Merging.
        assert_eq!(phase_after_b, HandoffPhase::Merging);
    }

    #[tokio::test]
    async fn failed_unit_requeues_until_max_attempts() {
        let (mgr, handoff) = fixture(1).await;
        for expected_attempts in 1..=MAX_UNIT_ATTEMPTS {
            let leased = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();
            assert_eq!(leased.unit_id, 0);
            let snap = mgr.snapshot(&handoff).await.unwrap();
            let attempts = match &snap.units[0].1 {
                UnitStatus::Leased { attempts, .. } => *attempts,
                other => panic!("expected Leased, got {other:?}"),
            };
            assert_eq!(attempts, expected_attempts);
            let phase = mgr
                .complete_unit(
                    &handoff,
                    peer(2),
                    0,
                    CompleteOutcome::Failed,
                    Some("simulated".into()),
                )
                .await
                .unwrap();
            if expected_attempts < MAX_UNIT_ATTEMPTS {
                // Still re-queued → phase back to Open.
                assert_eq!(phase, HandoffPhase::Open);
            } else {
                // Terminal Failed after final attempt.
                assert_eq!(phase, HandoffPhase::Merging);
                let final_snap = mgr.snapshot(&handoff).await.unwrap();
                assert!(matches!(final_snap.units[0].1, UnitStatus::Failed { .. }));
            }
        }
    }

    #[tokio::test]
    async fn reaper_requeues_expired_lease() {
        // Use a custom queue where we manually poke the expiry into the past.
        let (mgr, handoff) = fixture(1).await;
        let _ = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();

        // Reach in and push expiry into the past.
        {
            let mut guard = mgr.inner.lock().await;
            let queue = guard.get_mut(&handoff).unwrap();
            if let UnitStatus::Leased { expires_at_ms, .. } = &mut queue.units[0].1 {
                *expires_at_ms = 0;
            }
        }

        let stats = mgr.reap_expired().await;
        assert_eq!(stats.requeued, 1);
        let snap = mgr.snapshot(&handoff).await.unwrap();
        assert!(matches!(snap.units[0].1, UnitStatus::Queued { .. }));
        assert!(snap.queued.contains(&0));
    }

    #[tokio::test]
    async fn complete_after_lease_reclaimed_returns_lease_reclaimed() {
        let (mgr, handoff) = fixture(1).await;
        let leased = mgr.next_unit(&handoff, peer(2)).await.unwrap().unwrap();

        // Simulate reaper grabbing the lease back.
        {
            let mut guard = mgr.inner.lock().await;
            let queue = guard.get_mut(&handoff).unwrap();
            queue.units[0].1 = UnitStatus::Queued { prior_attempts: 0 };
            queue.queued.insert(0);
        }

        let err = mgr
            .complete_unit(
                &handoff,
                peer(2),
                leased.unit_id,
                CompleteOutcome::Complete,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err, QueueError::LeaseReclaimed);
    }
}
