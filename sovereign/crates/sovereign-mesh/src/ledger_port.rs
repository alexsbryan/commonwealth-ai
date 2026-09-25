// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed ledger PORTS — what the daemon will hold instead of the
//! in-process writers once its store construction flips (five-programs fp-78,
//! decision five-programs-35 option (b); the fp-54 shape of [`crate::rail_port`]).
//!
//! The writers themselves — `ContributionEmitter`, `ActivityEmitter`,
//! `PeerPreferenceStore`, `InferenceStateStore`, the processed-shards key —
//! stay in commonwealth-state, the one decider of each key scheme (ARCH 8);
//! `cw-rails` serves them as doors (`commonwealth-rails/src/ledger.rs`) and
//! the daemon's dialing implementation lives in
//! `sovereign-daemon/src/rails_client/ledger.rs`. Each trait's method set is
//! the daemon's call set on the field it will replace, and no more.
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

use commonwealth_core::activity::{ActivityEventKind, ActivitySummary};
use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::ModelInfo;
use commonwealth_core::oicp::EmbedModelInfo;

/// The record types these ports carry that live in commonwealth-state,
/// re-exported so the dialing side names them without that crate.
pub use commonwealth_state::inference_plan::InferencePlan;
pub use commonwealth_state::peer_preferences::PeerPreference;

/// Why a ledger call produced no answer. The message names the process that
/// did not answer and why; it is reported, never defaulted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LedgerAbsent(pub String);

/// `Pin<Box<…>>` future returning `Result<T, LedgerAbsent>` — the boxed shape
/// [`crate::rail_port::RailFut`] uses.
pub type LedgerFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, LedgerAbsent>> + Send + 'a>>;

/// `AppState.fabric.contribution_emitter` as a port.
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

/// `AppState.node.activity_emitter` as a port.
pub trait ActivityLedgerPort: Send + Sync {
    /// `ActivityEmitter::record`.
    fn record(&self, kind: ActivityEventKind) -> LedgerFut<'_, ()>;
    /// `commonwealth_state::current_activity`.
    fn current_activity(&self, window_days: u32) -> LedgerFut<'_, ActivitySummary>;
}

/// `AppState.store.peer_preferences` as a port — the daemon only reads it.
pub trait PeerPreferencesPort: Send + Sync {
    /// `PeerPreferenceStore::list`.
    fn list(&self) -> LedgerFut<'_, Vec<(NodeId, PeerPreference)>>;
    /// `PeerPreferenceStore::get`.
    fn get(&self, peer: &NodeId) -> LedgerFut<'_, Option<PeerPreference>>;
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
