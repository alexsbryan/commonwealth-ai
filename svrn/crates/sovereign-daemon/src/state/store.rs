//! The Serving fields the `serving` package may not name, held by the daemon.
//!
//! DC §4.2 assigns all twenty of Serving's fields to `sovereign-serving-host`,
//! and seventeen went there at `REVIEW-build-daemon-parts`. `inference_store`
//! (`crate::ledger_port::InferenceStatePort` since fp-93) and
//! `peer_preferences` (`PeerPreferencesPort` since fp-90) cannot cross the
//! crate line: they are backed by `commonwealth-state`, which is not a shared
//! leaf of the `serving` package — a dep would be a third `[[exception]]`, and
//! the campaign's K4 kill clause splits the cluster rather than widening the
//! ledger (`quality/campaigns/domains.toml:288`). The third, the rpc-warm
//! hook, is serve's since the flip (pb-mesh-exit-transport).
//!
//! Both stores are ports dialed to cw-rails since five-programs fp-88
//! ([`StoreSeed::rails`]); their home is still the node the daemon assembles.

use std::sync::Arc;

use crate::ledger_port::{
    ActivityLedgerPort, ContributionLedgerPort, InferenceStatePort, PeerPreferencesPort,
    ProcessedShardsPort,
};
use corpus_index::ingest_port::daemon::IngestPort;
use kernel_types::NodeId;
use sovereign_contracts::peer::ReplicatedKv;

use super::{fabric, node, serving, AppState};

/// The store ports `AppState` is assembled over — the one seam every backing
/// enters through (five-programs fp-97): [`StoreSeed::rails`] in production
/// (fp-88), `RecordingLedger::seed` in tests (`StoreSeed::local` and its
/// in-process `LocalLedger` retired at pb-mesh-exit-mesh).
pub struct StoreSeed {
    pub kv: Arc<dyn ReplicatedKv>,
    pub contributions: Arc<dyn ContributionLedgerPort>,
    pub activity: Arc<dyn ActivityLedgerPort>,
    pub peer_preferences: Arc<dyn PeerPreferencesPort>,
    pub inference: Arc<dyn InferenceStatePort>,
    pub processed_shards: Arc<dyn ProcessedShardsPort>,
}

impl StoreSeed {
    /// Every port dialed to cw-rails at `rails_base`: the five ledger ports
    /// through `RailsLedger`, and `kv` — the SAME `RailsKv` the work atlas and
    /// the notes sink write through (five-programs fp-88, decision -56).
    pub fn rails(kv: Arc<dyn ReplicatedKv>, rails_base: &str, self_node_id: NodeId) -> Self {
        let ledger = Arc::new(crate::rails_client::ledger::RailsLedger::new(
            rails_base,
            self_node_id,
        ));
        tracing::debug!(rails_base, "store seed: every store port dials cw-rails");
        Self {
            kv,
            contributions: ledger.clone(),
            activity: ledger.clone(),
            peer_preferences: ledger.clone(),
            inference: ledger.clone(),
            processed_shards: ledger,
        }
    }
}

impl AppState {
    /// Test-support: every seed plus the [`StoreSeed`], with Fabric over its
    /// own private store as [`AppState::new`] builds it — so a test hands in
    /// its store ports and names no store.
    pub fn new_with_seeds(
        self_node_id: NodeId,
        corpus_engine: Option<Arc<dyn IngestPort>>,
        in_flight_gauge: Option<sovereign_core::in_flight::LocalInFlightGauge>,
        fabric_seed: fabric::FabricSeed,
        serving_seed: serving::ServingSeed,
        node_seed: node::NodeSeed,
        store_seed: StoreSeed,
    ) -> Self {
        let fabric = Arc::new(fabric::FabricPart::new(self_node_id, fabric_seed));
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
    /// port (five-programs fp-93), dialed to cw-rails since fp-88.
    /// Every accessor reads and writes through it (§12 D4).
    pub inference_store: Arc<dyn InferenceStatePort>,
    /// Per-peer preference store (Ostrom sanctions). Local-only,
    /// never gossiped — see
    /// `oicp_types::peer_preference::PeerPreference` for the structural
    /// invariants. The manifest endpoint reads this on every
    /// fetch to apply per-requester affinity multipliers. Held as a port
    /// (five-programs fp-90), dialed to cw-rails since fp-88.
    pub peer_preferences: Arc<dyn PeerPreferencesPort>,
    /// The node's replicated KV as a port (five-programs-36 (2)), dialed to
    /// cw-rails since fp-88.
    pub mesh_store: Arc<dyn ReplicatedKv>,
    /// The contribution ledger as a port, dialed to cw-rails since fp-88.
    pub contribution_emitter: Arc<dyn ContributionLedgerPort>,
    /// The processed-shards announcements as a port (five-programs-46),
    /// dialed to cw-rails since fp-88.
    pub processed_shards: Arc<dyn ProcessedShardsPort>,
}
