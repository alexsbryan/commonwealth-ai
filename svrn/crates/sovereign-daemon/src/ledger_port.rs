// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed ledger PORTS the daemon holds instead of the in-process writers
//! (five-programs fp-78, decision five-programs-35 option (b)). Moved here
//! from `sovereign_mesh::ledger_port` beside their one implementation
//! (pb-mesh-exit-mesh); the in-process `LocalLedger` retired with the move,
//! and tests seed `crate::double::ledger_double::RecordingLedger`.
//!
//! The writers themselves — `ContributionEmitter`, `ActivityEmitter`,
//! `PeerPreferenceStore`, `InferenceStateStore`, the processed-shards key —
//! stay in commonwealth-state, the one decider of each key scheme (ARCH 8);
//! `cw-rails` serves them as doors (`commonwealth-rails/src/ledger.rs`) and
//! the daemon's dialing implementation lives in
//! `crate::rails_client::ledger`. Each trait's method set is the daemon's
//! call set on the field it replaced, and no more.
//!
//! **The writer's node id is the port's identity**, not a per-call argument:
//! the daemon's rows keep the daemon's attribution, and `self_node_id` is
//! answered from it without a dial.
//!
//! Every failure is a [`LedgerAbsent`] naming what did not answer — never an
//! empty list standing in for "could not ask" (principle 6).

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;

use kernel_types::{ModelId, NodeId};
use oicp_types::activity::{ActivityEvent, ActivityEventKind, ActivitySummary};
use oicp_types::capabilities::NodeCapabilities;
use oicp_types::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use oicp_types::model_catalog::ModelInfo;
use oicp_types::EmbedModelInfo;

/// The record types these ports carry, federation wire in oicp-types.
pub use oicp_types::inference_plan::{InferencePlan, ShardPlan};
pub use oicp_types::peer_preference::PeerPreference;

/// Why a ledger call produced no answer. The message names the process that
/// did not answer and why; it is reported, never defaulted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LedgerAbsent(pub String);

/// `Pin<Box<…>>` future returning `Result<T, LedgerAbsent>` — the boxed shape
/// `RailFut` uses.
pub type LedgerFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, LedgerAbsent>> + Send + 'a>>;

/// `ContributionEmitter` as a port.
pub trait ContributionLedgerPort: Send + Sync {
    /// The node id every event this port records is written as.
    fn self_node_id(&self) -> NodeId;
    /// `ContributionEmitter::record`.
    fn record(&self, kind: LedgerEventKind) -> LedgerFut<'_, ()>;
    /// `ContributionEmitter::events`.
    fn events(&self) -> LedgerFut<'_, Vec<LedgerEvent>>;
    /// `commonwealth_state::current_contributions`.
    fn current_contributions(
        &self,
        peer_capabilities: &HashMap<NodeId, NodeCapabilities>,
        window_days: u32,
    ) -> LedgerFut<'_, HashMap<NodeId, NodeContributions>>;
}

/// Long-running background task that records one `StorageSnapshot`
/// event per `interval` tick (the daemon passes
/// `oicp_types::contributions::STORAGE_SNAPSHOT_INTERVAL`). Consumes
/// a `walker` closure that produces the per-corpus `(id, size_gb)` pairs to
/// record — the daemon supplies the walker, so the loop names no corpus. An empty walker result records nothing.
///
/// A [`LedgerAbsent`] from `record` is traced at warn and the loop keeps
/// ticking: reported, never defaulted, never fatal (principle 6).
///
/// Shuts down cleanly when `shutdown` flips to true.
pub async fn run_storage_snapshot_loop<F, Fut>(
    emitter: std::sync::Arc<dyn ContributionLedgerPort>,
    mut walker: F,
    interval: std::time::Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Vec<(String, f64)>> + Send,
{
    let mut ticker = tokio::time::interval(interval);
    // The first tick fires immediately; we want a snapshot at boot
    // AND every interval after, so this is the desired behavior.
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let corpora = walker().await;
                if !corpora.is_empty() {
                    if let Err(absent) = emitter
                        .record(LedgerEventKind::StorageSnapshot { corpora })
                        .await
                    {
                        tracing::warn!(
                            error = %absent,
                            "storage_snapshot: StorageSnapshot not recorded"
                        );
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!(
                        "storage_snapshot: shutdown requested — exiting"
                    );
                    return;
                }
            }
        }
    }
}

/// `AppState.node.activity_emitter` as a port.
pub trait ActivityLedgerPort: Send + Sync {
    /// `ActivityEmitter::record`.
    fn record(&self, kind: ActivityEventKind) -> LedgerFut<'_, ()>;
    /// `ActivityEmitter::events`.
    fn events(&self) -> LedgerFut<'_, Vec<ActivityEvent>>;
    /// `commonwealth_state::current_activity`.
    fn current_activity(&self, window_days: u32) -> LedgerFut<'_, ActivitySummary>;
}

/// `AppState.store.peer_preferences` as a port — the daemon reads it on
/// every manifest fetch and writes it from the operator's `peer-preference`
/// routes.
pub trait PeerPreferencesPort: Send + Sync {
    /// `PeerPreferenceStore::list`.
    fn list(&self) -> LedgerFut<'_, Vec<(NodeId, PeerPreference)>>;
    /// `PeerPreferenceStore::get`.
    fn get(&self, peer: &NodeId) -> LedgerFut<'_, Option<PeerPreference>>;
    /// `PeerPreferenceStore::set`.
    fn set(&self, peer: &NodeId, pref: PeerPreference) -> LedgerFut<'_, ()>;
    /// `PeerPreferenceStore::clear` — whether a preference was there.
    fn clear(&self, peer: &NodeId) -> LedgerFut<'_, bool>;
}

/// The processed-shards announcements (`commonwealth_state::processed_shards`).
pub trait ProcessedShardsPort: Send + Sync {
    /// Publish this node's processed set for `corpus_id` under
    /// `processed_shards_key`; answers whether the stored value changed.
    fn publish(&self, corpus_id: &str, shards: &[usize]) -> LedgerFut<'_, bool>;
    /// `union_processed_shards`.
    fn union(&self, corpus_id: &str) -> LedgerFut<'_, BTreeSet<usize>>;
}

/// `AppState.store.inference_store` as a port. The four readers the daemon
/// calls synchronously sit over a read-through cache on the daemon side;
/// these are the doors that cache refills from.
pub trait InferenceStatePort: Send + Sync {
    fn get_plan(&self) -> LedgerFut<'_, Option<InferencePlan>>;
    fn set_plan(&self, plan: &InferencePlan) -> LedgerFut<'_, ()>;
    fn get_model_info(&self, model_id: ModelId) -> LedgerFut<'_, Option<ModelInfo>>;
    fn set_model_info(&self, info: &ModelInfo) -> LedgerFut<'_, ()>;
    fn remove_model_info(&self, model_id: ModelId) -> LedgerFut<'_, bool>;
    /// The one scan `list_models` and `list_models_with_origins` both fold.
    fn list_models_with_origins(&self) -> LedgerFut<'_, Vec<(NodeId, ModelInfo)>>;
    fn get_llama_address(&self, model_id: ModelId) -> LedgerFut<'_, Option<String>>;
    fn set_llama_address(&self, model_id: ModelId, addr: &str) -> LedgerFut<'_, ()>;
    fn set_local_embed_model(&self, info: &EmbedModelInfo) -> LedgerFut<'_, ()>;
    fn get_local_embed_model(&self) -> LedgerFut<'_, Option<EmbedModelInfo>>;
}
