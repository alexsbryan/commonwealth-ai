// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic-simulation (DST) mesh driver.
//!
//! Wraps a [`SimulatedMesh`] and drives the **real** gossip path
//! ([`crate::gossip::run_one_round`]) against the live in-process axum servers,
//! through a per-node [`FaultTransport`], so partition / crash / wire faults
//! from a seeded [`FaultSchedule`] actually affect convergence. This replaces
//! the harness's `sync_mesh_state` broadcast shortcut with emergent
//! anti-entropy — the same code the daemon runs every 10s.
//!
//! It lives in `sovereign-mesh` (not the test-harness crate) because only this
//! crate can name `run_one_round`; the harness sits *below* it in the
//! dependency graph. Gated behind the `dst` feature so production never links
//! the harness.
//!
//! ## Non-determinism discipline
//! The harness is not bit-deterministic (real tokio + TCP + an unseeded gossip
//! peer-selection RNG). Only the *fault schedule* is seeded. Assertions follow
//! the **quiesce-then-assert** rule: inject faults during scheduled rounds,
//! stop, drive to a fixpoint, then snapshot and check invariants. Two fixpoints
//! exist because the unseeded RNG makes gossip order-sensitive:
//! [`DstMesh::gossip_until_quiescent`] (views stopped changing — correct while a
//! partition is active, where the two sides *stably disagree*) and
//! [`DstMesh::gossip_until_quiescent_agreed`] (views stopped changing AND every
//! up node holds the identical view — the heal-then-assert fixpoint, so a stable
//! but not-yet-agreed plateau can't fire a spurious `Converged`). A node's
//! epoch-0 member records (the harness default) are kept fresh by a
//! [`TestClock`] with a small base and a large offline threshold, so nothing
//! decays unless the scenario advances the clock past the threshold.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::{aliased_endpoint_keys, EndpointClaim, NodeStatus};
use commonwealth_core::{partition, TestClock};
use commonwealth_test_harness::fault::{shared_policy, FaultProxy, FaultTransport, SharedPolicy};
use commonwealth_test_harness::simulated_mesh::SimulatedMesh;
use commonwealth_test_harness::simulated_node::SimulatedNodeBuilder;

use crate::gossip;

// Re-export the fault-authoring types so tests need only depend on
// `sovereign_mesh` (with `--features dst`), not the harness crate directly.
pub use commonwealth_test_harness::fault::{FaultEvent, FaultSchedule, WireFault};

/// Offline threshold used for DST rounds. Large relative to the [`TestClock`]
/// base so the harness's epoch-0 member records don't decay spuriously; a
/// scenario that wants decay advances the clock past this.
const DST_OFFLINE_THRESHOLD: Duration = Duration::from_secs(3600);

/// Starting wall-clock for the shared [`TestClock`] (seconds). Arbitrary but
/// non-zero so a small negative skew in a later scenario stays in range.
const DST_CLOCK_BASE_SECS: u64 = 1_000;

/// Outcome of driving gossip to a fixpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quiescence {
    /// Member views stabilised after `rounds` sweeps.
    Converged { rounds: usize },
    /// Views still changing at the round budget — treat as a failed invariant,
    /// not a flake.
    MaxRoundsExceeded { rounds: usize },
}

/// A fault-injecting driver over a [`SimulatedMesh`].
pub struct DstMesh {
    sim: SimulatedMesh,
    policy: SharedPolicy,
    clock: TestClock,
    /// Per-node typed transport handles (parallel to `sim.nodes`), so a
    /// scenario can adjust routes / proxies mid-run.
    transports: Vec<Arc<FaultTransport>>,
    /// Kept alive for the mesh's lifetime; only faulted edges ever dial them.
    _proxies: Vec<FaultProxy>,
    /// Harness ground truth of which nodes have been crashed.
    down: BTreeSet<NodeId>,
}

impl DstMesh {
    /// Build and start an `n`-node mesh: real axum servers on `127.0.0.1:0`, a
    /// per-node [`FaultTransport`] wired to every peer's internal listener, a
    /// shared (clean) [`FaultPolicy`], and a shared [`TestClock`]. Every node
    /// sees the full roster (via `sync_mesh_state`) as `Online`.
    pub async fn start(n: usize) -> Self {
        let mut sim = SimulatedMesh::new("dst");
        for i in 0..n {
            sim.add_node(SimulatedNodeBuilder::new(
                (i as u128) + 1,
                &format!("node-{i}"),
            ));
        }
        let addrs = sim.start_all().await;
        sim.sync_mesh_state().await;

        let policy = shared_policy();
        let clock = TestClock::new(DST_CLOCK_BASE_SECS);
        let node_ids = sim.node_ids();
        let internal_addrs: Vec<_> = addrs.iter().map(|(_, internal)| *internal).collect();

        let mut transports = Vec::with_capacity(n);
        let mut proxies = Vec::new();

        for (idx, node) in sim.nodes.iter().enumerate() {
            let self_id = node_ids[idx];

            // Per-node clock: a clone sharing the base (zero skew here; a skew
            // scenario calls `with_offset`). Routing every gossip `now` through
            // this is what makes injected time observable.
            node.state.install_clock(Arc::new(clock.clone()));

            let transport = Arc::new(FaultTransport::new(self_id, policy.clone()));
            for (j, &peer_id) in node_ids.iter().enumerate() {
                if j == idx {
                    continue;
                }
                transport.set_route(peer_id, internal_addrs[j]);
                // One proxy per (observer, target) edge — used only when the
                // edge carries a wire fault; clean edges dial direct.
                let proxy = FaultProxy::spawn(self_id, peer_id, internal_addrs[j], policy.clone())
                    .await
                    .expect("fault proxy bind on loopback");
                transport.set_proxy(peer_id, proxy.listen_addr);
                proxies.push(proxy);
            }
            node.state.install_peer_transport(transport.clone());
            transports.push(transport);
        }

        Self {
            sim,
            policy,
            clock,
            transports,
            _proxies: proxies,
            down: BTreeSet::new(),
        }
    }

    /// The shared fault policy — mutate it (or apply a [`FaultSchedule`]) to
    /// inject faults between rounds.
    pub fn policy(&self) -> &SharedPolicy {
        &self.policy
    }

    /// The shared test clock — advance it to drive offline-decay / skew.
    pub fn clock(&self) -> &TestClock {
        &self.clock
    }

    /// Typed transport handle for node `idx` (to adjust routes/proxies).
    pub fn transport(&self, idx: usize) -> &FaultTransport {
        &self.transports[idx]
    }

    /// Apply every event a [`FaultSchedule`] has for `round` to the policy.
    /// (Node crash/up events also need [`DstMesh::crash`] to stop the server;
    /// partition / wire faults are pure policy and take effect immediately.)
    pub fn apply_schedule_round(&self, schedule: &FaultSchedule, round: usize) {
        schedule.apply_round(round, &self.policy);
    }

    /// Crash node `idx`: stop its server and mark it unreachable from every
    /// peer. Pairs the real `shutdown` with the policy reflection so other
    /// nodes' transports stop dialing it.
    pub fn crash(&mut self, idx: usize) {
        let id = self.sim.node_ids()[idx];
        self.policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mark_down(id);
        self.sim.nodes[idx].shutdown();
        self.down.insert(id);
    }

    /// Node ids in index order (for authoring partition / fault events).
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.sim.node_ids()
    }

    /// Skew node `idx`'s clock by `offset_secs` vs the shared base (positive =
    /// that node's wall-clock runs ahead). Proves offline-decay is skew-immune.
    pub fn skew_node(&self, idx: usize, offset_secs: i64) {
        self.sim.nodes[idx]
            .state
            .install_clock(Arc::new(self.clock.with_offset(offset_secs)));
    }

    /// Install a slow-peer wire fault on the (observer → target) edge: throttle
    /// the proxy's upstream→client throughput to `bps`. Tests gossip resilience
    /// to a peer that dribbles bytes — assert on outcome CLASS (the round
    /// completes once healed, or times out), never on latency. Keep `bps` low
    /// enough that the outcome is unambiguous (categorical, not marginal).
    pub fn slow_peer(&self, observer: usize, target: usize, bps: u64) {
        let ids = self.node_ids();
        self.policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_wire(
                ids[observer],
                ids[target],
                commonwealth_test_harness::fault::WireFault {
                    throttle_bps: Some(bps),
                    ..Default::default()
                },
            );
    }

    /// Install a truncate-stream wire fault: cut the (observer → target) response
    /// after `n` bytes — a peer that dies mid-response. Surfaces partial-read
    /// handling in the gossip merge path (a truncated body must be rejected, not
    /// half-applied).
    pub fn truncate_stream(&self, observer: usize, target: usize, n: usize) {
        let ids = self.node_ids();
        self.policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_wire(
                ids[observer],
                ids[target],
                commonwealth_test_harness::fault::WireFault {
                    cut_after_bytes: Some(n),
                    ..Default::default()
                },
            );
    }

    /// Jump node `idx`'s clock BACKWARD by `secs` (an NTP step-back / non-
    /// monotonic wall clock). Thin alias over [`DstMesh::skew_node`] with a
    /// negative offset — offline-decay must not treat a backward jump as
    /// staleness (it measures local-observation advance, not the peer's clock).
    pub fn clock_jump_back(&self, idx: usize, secs: i64) {
        self.skew_node(idx, -secs);
    }

    /// Clear every injected fault (heal all partitions / wire faults / downs).
    /// Used to settle a chaos schedule before the final quiesce-and-assert.
    /// (Does not un-crash a node stopped via [`DstMesh::crash`] — that server
    /// is really gone.)
    pub fn clear_faults(&self) {
        *self
            .policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            commonwealth_test_harness::fault::FaultPolicy::default();
    }

    /// Write a mesh_store key on node `idx` (origin = that node).
    pub fn store_set(&self, idx: usize, app_id: &str, key: &str, value: &[u8]) {
        let node = &self.sim.nodes[idx];
        let _ = node.state.inner.mesh_store.set(
            app_id,
            key,
            Bytes::copy_from_slice(value),
            node.node_id,
        );
    }

    /// Read a mesh_store key on node `idx`, if present.
    pub fn store_get(&self, idx: usize, app_id: &str, key: &str) -> Option<Vec<u8>> {
        self.sim.nodes[idx]
            .state
            .inner
            .mesh_store
            .get(app_id, key)
            .ok()
            .flatten()
            .map(|e| e.value.to_vec())
    }

    /// Drive one real gossip round on node `idx` against the live servers.
    pub async fn drive_gossip_round(&self, idx: usize) {
        // Individual peer failures are logged inside `run_one_round` and do not
        // propagate; a round only errs on a fundamental fault.
        if let Err(e) =
            gossip::run_one_round(&self.sim.nodes[idx].state, DST_OFFLINE_THRESHOLD).await
        {
            tracing::debug!(node = idx, error = %e, "dst: gossip round error");
        }
    }

    /// One sweep: every non-crashed node executes a gossip round once.
    pub async fn sweep(&self) {
        let ids = self.sim.node_ids();
        for (idx, id) in ids.iter().enumerate() {
            if self.down.contains(id) {
                continue;
            }
            self.drive_gossip_round(idx).await;
        }
    }

    /// Drive sweeps until member views are stable across two consecutive
    /// sweeps (a structural fixpoint, no wall-clock sleeps), or the round
    /// budget is hit.
    ///
    /// This is STABILITY, not AGREEMENT. While a partition is active each side
    /// settles into a *stably disagreeing* view (it has decayed the unreachable
    /// group to offline), and that is the correct fixpoint to drive to
    /// mid-partition. For heal-then-assert calls — where every up node must
    /// converge to the SAME view before the invariant pack runs — use
    /// [`Self::gossip_until_quiescent_agreed`].
    pub async fn gossip_until_quiescent(&self, max_rounds: usize) -> Quiescence {
        self.gossip_until_quiescent_internal(max_rounds, false)
            .await
    }

    /// Like [`Self::gossip_until_quiescent`], but the fixpoint additionally
    /// requires every up node's member view to be pairwise identical — the mesh
    /// has not merely stopped changing, it has *agreed*.
    ///
    /// This closes the post-heal flake: a 2-sweep stability fixpoint can plateau
    /// on a not-yet-agreed state (gossip peer-selection is unseeded, so it's
    /// order-sensitive), making `gossip_until_quiescent` return `Converged` while
    /// `NoSplitBrain` still saw disagreeing live-sets. Requiring agreement before
    /// asserting removes the coin-flip.
    ///
    /// Use ONLY after healing every fault. An active partition legitimately
    /// yields disagreeing live-sets, so the agreed variant would (correctly) spin
    /// to the round budget while one is in force — that is asserted by
    /// `agreed_quiesce_rejects_stable_disagreement`.
    pub async fn gossip_until_quiescent_agreed(&self, max_rounds: usize) -> Quiescence {
        self.gossip_until_quiescent_internal(max_rounds, true).await
    }

    /// Shared driver for the two quiescence variants. `until_agreed` layers the
    /// pairwise-identical-views (agreement) requirement on top of the two-sweep
    /// stability fixpoint.
    async fn gossip_until_quiescent_internal(
        &self,
        max_rounds: usize,
        until_agreed: bool,
    ) -> Quiescence {
        let mut prev: Option<Vec<Vec<(NodeId, bool)>>> = None;
        for round in 1..=max_rounds {
            self.sweep().await;
            let cur = self.member_views().await;
            let stable = prev.as_ref() == Some(&cur);
            // Agreement: every up node holds an identical (member, is_live) view.
            // `windows(2)` over the per-node views turns "all equal" into a
            // consecutive-equal check; vacuously true for 0 or 1 up nodes.
            let agreed = !until_agreed || cur.windows(2).all(|w| w[0] == w[1]);
            if stable && agreed {
                return Quiescence::Converged { rounds: round };
            }
            prev = Some(cur);
        }
        Quiescence::MaxRoundsExceeded { rounds: max_rounds }
    }

    /// Per-up-node `(member, is_live)` views, for quiescence comparison.
    /// Excludes `last_seen` (which a non-advancing clock holds constant anyway)
    /// so the fixpoint is about membership + liveness, not timestamps.
    async fn member_views(&self) -> Vec<Vec<(NodeId, bool)>> {
        let ids = self.sim.node_ids();
        let mut views = Vec::new();
        for (idx, id) in ids.iter().enumerate() {
            if self.down.contains(id) {
                continue;
            }
            let mesh = self.sim.nodes[idx].state.inner.mesh.read().await;
            let mut v: Vec<(NodeId, bool)> = mesh
                .members
                .iter()
                .map(|(k, m)| (*k, m.status != NodeStatus::Offline))
                .collect();
            v.sort_by_key(|(k, _)| *k);
            views.push(v);
        }
        views
    }

    /// Freeze the mesh into a [`MeshSnapshot`] for invariant checking. All reads
    /// are in-process (no HTTP), taken at quiescence.
    pub async fn snapshot(&self) -> MeshSnapshot {
        let ids = self.sim.node_ids();
        let online_truth: BTreeSet<NodeId> = ids
            .iter()
            .copied()
            .filter(|id| !self.down.contains(id))
            .collect();
        let mut views = Vec::new();
        for (idx, id) in ids.iter().enumerate() {
            if self.down.contains(id) {
                continue;
            }
            let node = &self.sim.nodes[idx];
            let members = {
                let mesh = node.state.inner.mesh.read().await;
                mesh.members
                    .iter()
                    .map(|(k, m)| {
                        (
                            *k,
                            MemberStat {
                                live: m.status != NodeStatus::Offline,
                                last_seen: m.last_seen,
                                node_pubkey: m.node_pubkey,
                                active: m.is_active(),
                            },
                        )
                    })
                    .collect()
            };
            views.push(NodeView {
                self_id: *id,
                members,
                peer_inflight: node.state.peer_inflight_count(),
                inflight_ceiling: node.state.contribution_max_peer_inflight(),
            });
        }
        MeshSnapshot {
            views,
            online_truth,
        }
    }
}

/// One member, as seen in a node's view.
#[derive(Debug, Clone)]
pub struct MemberStat {
    /// `status != Offline` — i.e. considered reachable/present.
    pub live: bool,
    pub last_seen: u64,
    /// The endpoint key this member is dialed on.
    ///
    /// CARRIED SO AN INVARIANT CAN SEE IT. Until 2026-08-28 this snapshot held
    /// only `{live, last_seen}`, which made the whole pack structurally
    /// incapable of noticing that two rows named ONE endpoint — and an 8h soak
    /// duly ran clean over a mesh that had exactly that. A check cannot fail
    /// on a field its snapshot does not carry.
    pub node_pubkey: Option<NodePubkey>,
    /// `removed_at.is_none()` — NOT the same as [`Self::live`], which is a
    /// liveness/reachability judgement. Tombstoning is the qualifier
    /// [`UniqueEndpointKey`] scopes on, because a tombstoned row may
    /// legitimately share a key with a rejoined node.
    pub active: bool,
}

/// One node's view of the mesh at snapshot time.
#[derive(Debug, Clone)]
pub struct NodeView {
    pub self_id: NodeId,
    pub members: BTreeMap<NodeId, MemberStat>,
    pub peer_inflight: usize,
    pub inflight_ceiling: usize,
}

/// A frozen, all-node snapshot for invariant checks.
#[derive(Debug, Clone)]
pub struct MeshSnapshot {
    /// One per up node.
    pub views: Vec<NodeView>,
    /// Nodes the harness knows are actually up (not crashed).
    pub online_truth: BTreeSet<NodeId>,
}

/// A property the mesh must satisfy at quiescence.
pub trait MeshInvariant {
    fn name(&self) -> &'static str;
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation>;
}

/// A violated invariant, with enough detail to act on (and a seed to replay).
#[derive(Debug, Clone)]
pub struct Violation {
    pub invariant: &'static str,
    pub detail: String,
}

/// All up nodes agree on the set of members they know.
pub struct Convergence;
impl MeshInvariant for Convergence {
    fn name(&self) -> &'static str {
        "convergence"
    }
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation> {
        let mut iter = snap.views.iter();
        let Some(first) = iter.next() else {
            return Ok(());
        };
        let key0: BTreeSet<NodeId> = first.members.keys().copied().collect();
        for v in iter {
            let key: BTreeSet<NodeId> = v.members.keys().copied().collect();
            if key != key0 {
                return Err(Violation {
                    invariant: "convergence",
                    detail: format!(
                        "node {} knows {:?}; node {} knows {:?}",
                        first.self_id, key0, v.self_id, key
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Every up node elects the same leader over its live set (rendezvous owner of
/// a probe key likewise agrees) — i.e. no split brain.
pub struct NoSplitBrain;
impl MeshInvariant for NoSplitBrain {
    fn name(&self) -> &'static str {
        "no_split_brain"
    }
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation> {
        let mut leader: Option<Option<NodeId>> = None;
        let mut owner: Option<Option<NodeId>> = None;
        for v in &snap.views {
            let live: Vec<NodeId> = v
                .members
                .iter()
                .filter(|(_, m)| m.live)
                .map(|(k, _)| *k)
                .collect();
            let l = partition::elect_leader(&live);
            let o = partition::rendezvous_owner("dst-probe", &live);
            match leader {
                None => leader = Some(l),
                Some(prev) if prev != l => {
                    return Err(Violation {
                        invariant: "no_split_brain",
                        detail: format!(
                            "leader disagreement: {prev:?} vs {l:?} (node {})",
                            v.self_id
                        ),
                    });
                }
                _ => {}
            }
            match owner {
                None => owner = Some(o),
                Some(prev) if prev != o => {
                    return Err(Violation {
                        invariant: "no_split_brain",
                        detail: format!(
                            "owner disagreement: {prev:?} vs {o:?} (node {})",
                            v.self_id
                        ),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// No up node shows a member as live that the harness knows is down (a ghost
/// the gossip layer failed to decay).
pub struct NoGhostMembers;
impl MeshInvariant for NoGhostMembers {
    fn name(&self) -> &'static str {
        "no_ghost_members"
    }
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation> {
        for v in &snap.views {
            for (id, m) in &v.members {
                if m.live && !snap.online_truth.contains(id) {
                    return Err(Violation {
                        invariant: "no_ghost_members",
                        detail: format!(
                            "node {} still sees {} as live (it is down)",
                            v.self_id, id
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Peer in-flight never exceeds the ceiling, and is zero at quiescence (no
/// admission guard leaked).
pub struct AdmissionSafety;
impl MeshInvariant for AdmissionSafety {
    fn name(&self) -> &'static str {
        "admission_safety"
    }
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation> {
        for v in &snap.views {
            if v.peer_inflight > v.inflight_ceiling {
                return Err(Violation {
                    invariant: "admission_safety",
                    detail: format!(
                        "node {}: peer_inflight {} > ceiling {}",
                        v.self_id, v.peer_inflight, v.inflight_ceiling
                    ),
                });
            }
            if v.peer_inflight != 0 {
                return Err(Violation {
                    invariant: "admission_safety",
                    detail: format!(
                        "node {}: peer_inflight {} != 0 at quiescence (guard leak?)",
                        v.self_id, v.peer_inflight
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Every actually-up node is seen as live by every up node.
pub struct Liveness;
impl MeshInvariant for Liveness {
    fn name(&self) -> &'static str {
        "liveness"
    }
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation> {
        for v in &snap.views {
            for truth in &snap.online_truth {
                match v.members.get(truth) {
                    Some(m) if m.live => {}
                    _ => {
                        return Err(Violation {
                            invariant: "liveness",
                            detail: format!(
                                "node {} does not see up node {} as live",
                                v.self_id, truth
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// No endpoint key is claimed by two ACTIVE members, on any node's view.
///
/// The rule, the live defect that motivated it, and why it is scoped to
/// active rows are documented once, at the predicate:
/// [`commonwealth_core::mesh::aliased_endpoint_keys`]. This checks it per
/// node view; `merge_from_authenticated` enforces it at admission. Both call
/// that one function (§10.6).
pub struct UniqueEndpointKey;
impl MeshInvariant for UniqueEndpointKey {
    fn name(&self) -> &'static str {
        "unique_endpoint_key"
    }
    fn check(&self, snap: &MeshSnapshot) -> Result<(), Violation> {
        for v in &snap.views {
            // Delegate, don't re-derive. `aliased_endpoint_keys` is the ONE
            // implementation of this rule; the admission guard in
            // `merge_from_authenticated` asks the same function. A checker and
            // an admitter with separate opinions is the §10.6 duplicated
            // decider, and this is the last place you want a second one.
            let claims = v.members.iter().map(|(id, m)| EndpointClaim {
                node_id: *id,
                name: String::new(),
                node_pubkey: m.node_pubkey,
                active: m.active,
            });
            if let Some(alias) = aliased_endpoint_keys(claims).into_iter().next() {
                let ids: Vec<NodeId> = alias.members.iter().map(|(id, _)| *id).collect();
                return Err(Violation {
                    invariant: "unique_endpoint_key",
                    detail: format!(
                        "node {:?} sees {} ACTIVE members claiming endpoint key {}: {:?} \
                         — a cloned node identity; their liveness is split across rows \
                         and neither will read online",
                        v.self_id,
                        ids.len(),
                        &alias.node_pubkey.to_string()[..16],
                        ids
                    ),
                });
            }
        }
        Ok(())
    }
}

/// The default invariant pack checked at quiescence for membership/gossip
/// scenarios. (Knowledge-fan-out and double-emit invariants need heavier
/// fixtures and are added with their scenarios.)
pub fn default_invariants() -> Vec<Box<dyn MeshInvariant>> {
    vec![
        Box::new(Convergence),
        Box::new(NoSplitBrain),
        Box::new(NoGhostMembers),
        Box::new(AdmissionSafety),
        Box::new(Liveness),
        Box::new(UniqueEndpointKey),
    ]
}

/// Check the full default pack against a snapshot, collecting every violation.
pub fn check_all(snap: &MeshSnapshot) -> Vec<Violation> {
    default_invariants()
        .iter()
        .filter_map(|inv| inv.check(snap).err())
        .collect()
}

#[cfg(test)]
mod invariant_tests {
    use super::*;

    fn view(self_id: u128, rows: &[(u128, Option<u8>, bool)]) -> NodeView {
        NodeView {
            self_id: NodeId::from_u128(self_id),
            members: rows
                .iter()
                .map(|(id, key, active)| {
                    (
                        NodeId::from_u128(*id),
                        MemberStat {
                            live: true,
                            last_seen: 100,
                            node_pubkey: key.map(|b| NodePubkey([b; 32])),
                            active: *active,
                        },
                    )
                })
                .collect(),
            peer_inflight: 0,
            inflight_ceiling: 8,
        }
    }

    fn snap(views: Vec<NodeView>) -> MeshSnapshot {
        MeshSnapshot {
            online_truth: views.iter().map(|v| v.self_id).collect(),
            views,
        }
    }

    /// THE LIVE DEFECT, as a deterministic check.
    ///
    /// Mesh `27ba8166…` on 2026-08-28 carried two ACTIVE rows on one endpoint
    /// key (`Alexs-MacBook-Pro-2` + `BeefyMac`, both `86627fd5…`). Every
    /// invariant in the pack ran clean over it — including through an 8h soak
    /// — because `MemberStat` did not carry `node_pubkey` at all. This is the
    /// check that could not previously exist.
    #[test]
    fn a_cloned_node_identity_is_caught() {
        let s = snap(vec![view(
            1,
            &[
                (1, Some(0x86), true),
                (2, Some(0x86), true),
                (3, Some(0x11), true),
            ],
        )]);
        let v = UniqueEndpointKey
            .check(&s)
            .expect_err("two ACTIVE members on one endpoint key must violate");
        assert_eq!(v.invariant, "unique_endpoint_key");
        assert!(
            v.detail.contains("8686868686868686"),
            "the violation must name the aliased key so an operator can find \
             the rows: {}",
            v.detail
        );
    }

    /// The cry-wolf guard. A tombstoned row may legitimately hold the same key
    /// as a rejoined node — the rejoin stamps newer activity and wins the LWW.
    /// An invariant that fires on every honest rejoin is one that gets turned
    /// off, and it would take the real defect with it.
    #[test]
    fn a_tombstoned_row_sharing_a_key_is_not_a_violation() {
        let s = snap(vec![view(
            1,
            &[(1, Some(0x42), false), (2, Some(0x42), true)],
        )]);
        assert!(UniqueEndpointKey.check(&s).is_ok());
    }

    /// Keyless members are legacy pre-identity builds. `None` is not a key and
    /// two of them must never alias with each other.
    #[test]
    fn absent_keys_do_not_alias() {
        let s = snap(vec![view(1, &[(1, None, true), (2, None, true)])]);
        assert!(UniqueEndpointKey.check(&s).is_ok());
    }

    /// The pack must actually carry it — an invariant nobody runs is not a gate.
    #[test]
    fn the_default_pack_includes_the_endpoint_key_check() {
        assert!(
            default_invariants()
                .iter()
                .any(|i| i.name() == "unique_endpoint_key"),
            "UniqueEndpointKey must be in the default pack, or the soak and \
             `svrn mesh check-invariants` will keep running clean over a \
             cloned identity"
        );
    }
}
