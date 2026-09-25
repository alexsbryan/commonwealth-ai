//! The three Serving fields the `serving` package may not name, held by the
//! daemon.
//!
//! DC §4.2 assigns all twenty of Serving's fields to `sovereign-serving-host`,
//! and seventeen went there at `REVIEW-build-daemon-parts`. These three cannot
//! cross the crate line:
//!
//! * `inference_store` (`sovereign_mesh::ledger_port::InferenceStatePort`
//!   since fp-93) and `peer_preferences` (`PeerPreferencesPort` since fp-90)
//!   are backed by `commonwealth-state`, which is not a shared leaf of the
//!   `serving` package — a dep would be a third `[[exception]]`, and the
//!   campaign's K4 kill clause splits the cluster rather than widening the
//!   ledger (`quality/campaigns/domains.toml:288`);
//! * `rpc_shard_warmer`'s trait method ([`RpcShardWarmer::warm_shard`]) takes
//!   the daemon's `AppState`, so the trait cannot move to the host either.
//!
//! Both stores are backed by the node's `MeshStore`, so their home is the node
//! the daemon assembles; a later row can move them once the store arrives
//! through a port.

use std::sync::Arc;

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use corpus_engine::CorpusEngine;
use sovereign_contracts::peer::ReplicatedKv;
use sovereign_mesh::ledger_port::{
    ActivityLedgerPort, ContributionLedgerPort, InferenceStatePort, PeerPreferencesPort,
    ProcessedShardsPort,
};
use sovereign_meshapp_registry::registry::AppRegistry;

use super::{fabric, node, serving, AppState, RpcShardWarmer};

/// The store ports `AppState` is assembled over — the one seam every backing
/// enters through (five-programs fp-97): [`StoreSeed::local`] in-process today,
/// a `rails_client` backing at fp-88, a recording double in tests.
pub struct StoreSeed {
    pub kv: Arc<dyn ReplicatedKv>,
    pub contributions: Arc<dyn ContributionLedgerPort>,
    pub activity: Arc<dyn ActivityLedgerPort>,
    pub peer_preferences: Arc<dyn PeerPreferencesPort>,
    pub inference: Arc<dyn InferenceStatePort>,
    pub processed_shards: Arc<dyn ProcessedShardsPort>,
}

impl StoreSeed {
    /// Every port over the node's own `MeshStore` through `LocalLedger`.
    pub fn local(mesh_store: Arc<MeshStore>, self_node_id: NodeId) -> Self {
        let kv_port: Arc<dyn sovereign_contracts::peer::ReplicatedKv> = Arc::new(
            sovereign_mesh::peer_adapter::MeshReplicatedKv::over(Arc::clone(&mesh_store)),
        );
        let local_ledger = Arc::new(sovereign_mesh::ledger_port::LocalLedger::new(
            Arc::clone(&mesh_store),
            self_node_id,
        ));
        let activity_emitter: Arc<dyn sovereign_mesh::ledger_port::ActivityLedgerPort> =
            local_ledger.clone();
        let peer_preferences: Arc<dyn sovereign_mesh::ledger_port::PeerPreferencesPort> =
            local_ledger.clone();
        let inference_store: Arc<dyn sovereign_mesh::ledger_port::InferenceStatePort> =
            local_ledger.clone();
        let contribution_port: Arc<dyn sovereign_mesh::ledger_port::ContributionLedgerPort> =
            local_ledger.clone();
        let processed_shards: Arc<dyn sovereign_mesh::ledger_port::ProcessedShardsPort> =
            local_ledger;
        Self {
            kv: kv_port,
            contributions: contribution_port,
            activity: activity_emitter,
            peer_preferences,
            inference: inference_store,
            processed_shards,
        }
    }
}

impl AppState {
    /// Test-support: every seed plus the [`StoreSeed`], with Fabric over its
    /// own in-memory `MeshStore` as [`AppState::new`] builds it — so a test
    /// hands in its store ports and names no store.
    pub fn new_with_seeds(
        self_node_id: NodeId,
        mesh: Mesh,
        corpus_engine: Option<Arc<CorpusEngine>>,
        in_flight_gauge: Option<sovereign_core::in_flight::LocalInFlightGauge>,
        fabric_seed: fabric::FabricSeed,
        serving_seed: serving::ServingSeed,
        node_seed: node::NodeSeed,
        store_seed: StoreSeed,
    ) -> Self {
        #[allow(clippy::expect_used)]
        let mesh_store = Arc::new(MeshStore::in_memory().expect("in-memory MeshStore failed"));
        let fabric = Arc::new(fabric::FabricPart::new(
            self_node_id,
            mesh,
            mesh_store,
            Arc::new(AppRegistry::new()),
            fabric_seed,
        ));
        Self::assemble_with_fabric(
            self_node_id,
            fabric,
            store_seed,
            corpus_engine,
            in_flight_gauge,
            serving_seed,
            node_seed,
        )
    }
}

/// The three Serving fields held by the daemon, read as `AppStateInner::store`.
pub struct StorePart {
    /// Inference plan, model info, ledger, and llama addresses, held as a
    /// port over `LocalLedger` (five-programs fp-93); in-process until fp-88.
    /// Every accessor reads and writes through it (§12 D4).
    pub inference_store: Arc<dyn InferenceStatePort>,
    /// Per-peer preference store (Ostrom sanctions). Local-only,
    /// never gossiped — see
    /// `commonwealth_state::peer_preferences` for the structural
    /// invariants. The manifest endpoint reads this on every
    /// fetch to apply per-requester affinity multipliers. Held as a port
    /// over `LocalLedger` (five-programs fp-90); in-process until fp-88.
    pub peer_preferences: Arc<dyn PeerPreferencesPort>,
    /// Worker-side auto-warm hook for distributed inference, passed at
    /// construction alongside `local_inference`; drives
    /// `POST /internal/rpc-warm`. `None` on a node that isn't an inference
    /// worker. See [`RpcShardWarmer`].
    pub rpc_shard_warmer: Option<Arc<dyn RpcShardWarmer>>,
    /// The node's replicated KV as a port — Fabric's `mesh_store` seen through
    /// `ReplicatedKv` (five-programs-36 (2)); in-process until fp-88.
    pub mesh_store: Arc<dyn ReplicatedKv>,
    /// The contribution ledger as a port — Fabric's `contribution_emitter`
    /// seen through `ContributionLedgerPort`; in-process until fp-88.
    pub contribution_emitter: Arc<dyn ContributionLedgerPort>,
    /// The processed-shards announcements as a port (five-programs-46);
    /// in-process until fp-88.
    pub processed_shards: Arc<dyn ProcessedShardsPort>,
}
