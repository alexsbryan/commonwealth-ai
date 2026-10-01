// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dimensional contribution ledger's records: the append-only events that
//! gossip through the mesh and the per-node view they aggregate into.
//!
//! Moved here from `commonwealth_core::contributions` by pb-mesh-exit-core
//! (FIVE_PROGRAMS §12 3a rung 2): the events are gossip-replicated and served
//! through cw-rails' ledger doors, so two programs speak them. The aggregation
//! (`commonwealth_core::contributions::aggregate`) is a decision and stays
//! there; that module re-exports these records.

use serde::{Deserialize, Serialize};

use crate::JobKind;
use kernel_types::{HandoffId, NodeId};

/// One discrete event describing mesh activity. Append-only,
/// gossip-replicated, never mutated after emission.
///
/// `node_id` is the *origin* — the node that observed the event
/// and is now broadcasting it. For directed events (an inference
/// served by A for B), the counter-party id rides inside `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerEvent {
    /// Origin: the node that observed and emitted this event.
    pub node_id: NodeId,
    /// Unix seconds when the event was emitted.
    pub timestamp: u64,
    pub kind: LedgerEventKind,
}

/// The dimensional shape of a ledger event. Closed set —
/// per ARCH_PRINCIPLES §2.1, every variant is a distinct kind of
/// activity, not a stringly-typed bag. Add a new variant when a
/// genuinely new dimension of contribution arrives; do NOT bend an
/// existing variant by overloading it with a new payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum LedgerEventKind {
    /// This node served an inference request for `for_node`. The
    /// emitter is the *server* side of the exchange; the requester
    /// is captured in `for_node`.
    InferenceServed {
        for_node: NodeId,
        model_id: String,
        tokens_generated: u64,
        wall_seconds: f64,
    },
    /// This node received an inference response from `from_node`.
    /// Symmetric counterpart to `InferenceServed`. Both sides
    /// emit on every cross-mesh inference; aggregation cross-checks
    /// these to flag missing-event scenarios.
    InferenceReceived {
        from_node: NodeId,
        model_id: String,
        tokens_generated: u64,
    },
    /// Federated knowledge query served by this node for `for_node`.
    /// `chunks_returned` is the aggregator's count of chunks this
    /// peer contributed to the merged response (not the total).
    KnowledgeQueryServed {
        for_node: NodeId,
        corpus_id: String,
        chunks_returned: u32,
    },
    /// One peer's index/model shard transferred to another. Both
    /// the sender (`from_node`) and recipient (`to_node`) are
    /// captured explicitly so the puller in a request-response
    /// transfer can record the event on behalf of the peer that
    /// actually shipped the bytes — see `ShardManager::coordinate_merge`,
    /// where the merge leader pulls partitions from peers and is
    /// the only side that observes the transfer completing.
    ///
    /// `bytes` is the on-the-wire payload size (after any
    /// compression). Aggregator buckets `bytes` onto
    /// `from_node.bytes_served` and onto `to_node.bytes_received`,
    /// regardless of which node actually emitted the event — see
    /// `aggregate` for the special case.
    ShardTransferred {
        from_node: NodeId,
        to_node: NodeId,
        corpus_id: String,
        bytes: u64,
    },
    /// Hourly snapshot of the corpora this node is hosting on
    /// disk. Drives the "storage" dimension of contribution; we do
    /// not reconstruct hosting from a stream of install/uninstall
    /// events because the hourly cadence is sufficient for routing
    /// and reporting and it cleanly handles process restarts.
    StorageSnapshot { corpora: Vec<(String, f64)> },
    /// This node ran a unit of ANOTHER member's work to a verdict on
    /// its own metal — the donor half of the work plane (cw-lift 5h).
    ///
    /// A genuinely new dimension, and not a fourth spelling of an old
    /// one: it is neither inference (no model, no tokens), nor
    /// storage, nor bandwidth. Per principle 1 above it is counted in
    /// its own units and never folded into the others — a CI shard is
    /// not an inference request, and adding one to
    /// `inference_served.requests` would make that count a lie.
    ///
    /// **Emitted once, by the donor, when its own signed `Complete`
    /// act appends.** It is NOT derived by every node that folds that
    /// act; `sovereign_daemon::work_donor::credit_for` carries the full
    /// argument for why, and the short form is that this log converges
    /// by "one write site, one event" (principle 2) while the work
    /// journal converges by total order — deriving here would put one
    /// fact into this log once per ring member.
    ///
    /// `handoff` + `unit_hash` + `donor_actor` are the audit: they
    /// point at the signed `Complete` on the `work` journal that has
    /// to exist for this credit to be honest. `LedgerEvent.node_id` is
    /// the emitter's own self-reported id and `donor_actor` is the key
    /// admission verified (ARCH §7.5) — the two together are checkable
    /// against the journal and either one alone is not.
    JobUnitCompleted {
        /// The handoff the unit belongs to, as the `work` journal
        /// spells it.
        handoff: HandoffId,
        /// The unit's content hash — the work fold's idempotence key,
        /// carried so this credit can be matched back to exactly one
        /// admitted `Complete`.
        unit_hash: String,
        /// The rail actor the donor signed that `Complete` with: 64
        /// lowercase hex characters of an Ed25519 verifying key, in
        /// the one spelling `commonwealth_work::actor::ActorKey`
        /// enforces. A `String` here rather than that type because
        /// `commonwealth-work` sits ABOVE this crate; it is the same
        /// bytes in the same spelling, so there is no second name for
        /// one identity to drift.
        donor_actor: String,
        /// What kind of unit it was — `InferenceServed.model_id`'s
        /// counterpart, and the answer to "43 units of what?".
        kind: JobKind,
        /// Wall clock the donor's own metal spent on it. The same
        /// quantity `InferenceServed.wall_seconds` records and
        /// measured the same way, and the one field a third party
        /// folding the journal could not reconstruct: the rail carries
        /// lease-held time, which includes admit latency and heartbeat
        /// scheduling, not compute.
        wall_seconds: f64,
    },
}

/// Aggregated activity for a single inference role (served or
/// consumed). Three orthogonal counts so an operator can answer
/// "how many requests" / "how much output" / "how much wall-clock
/// did I spend" without inferring any one from the others.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InferenceActivity {
    pub requests: u64,
    pub total_tokens_generated: u64,
    pub wall_seconds: f64,
}

/// Aggregated compute a node donated to other members' work units.
///
/// Two counts, and deliberately not [`InferenceActivity`]: that struct
/// carries `total_tokens_generated`, which has no meaning for a CI
/// shard and would be a permanent zero pretending to be a
/// measurement (ARCH §18.3). The two numbers here are the two the
/// ledger already counts — a request count and a wall clock.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DonatedCompute {
    /// How many units this node ran to a verdict for somebody else.
    pub units: u64,
    /// Wall clock those units held this node's metal, summed.
    pub wall_seconds: f64,
}

/// One entry in `NodeContributions.corpora_hosted` describing one
/// corpus this node hosts.
///
/// `is_sole_host` is computed by checking the gossip-replicated
/// `NodeCapabilities.hosted_corpora` across all members at
/// aggregation time — a corpus only this node advertises is a
/// public-good signal worth surfacing in the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CorpusHosting {
    pub corpus_id: String,
    pub corpus_name: String,
    pub size_gb: f64,
    pub queries_served: u64,
    pub is_sole_host: bool,
}

/// Per-node dimensional aggregation. Computed locally on every
/// machine from the same gossip-replicated event stream — every
/// node with the same events produces identical `NodeContributions`.
///
/// Deliberately carries no `balance` field, no exchange rate, and
/// no ranking. Operators read it as "this peer served N inferences
/// for me, hosts M corpora (one of which they're the sole host of),
/// and shipped K GB of bytes" — three facts in three different
/// units.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NodeContributions {
    pub window_days: u32,
    pub inference_served: InferenceActivity,
    pub inference_consumed: InferenceActivity,
    pub corpora_hosted: Vec<CorpusHosting>,
    pub bytes_served: u64,
    pub bytes_received: u64,
    /// Work units this node ran on the work plane for other members.
    /// A fourth incommensurable dimension beside inference, storage
    /// and bytes — see [`LedgerEventKind::JobUnitCompleted`].
    pub compute_donated: DonatedCompute,
}

/// Default aggregation window. 30 days roughly matches the cadence
/// at which a healthy mesh churns peers — short enough that a peer
/// who joined yesterday isn't drowned out by historical totals,
/// long enough that day-to-day variance smooths out.
pub const DEFAULT_WINDOW_DAYS: u32 = 30;

/// The ring namespace contribution ledger events replicate on. Federation
/// wire, re-exported at `commonwealth_state::contributions`.
pub const CONTRIBUTIONS_APP_ID: &str = "contributions";
