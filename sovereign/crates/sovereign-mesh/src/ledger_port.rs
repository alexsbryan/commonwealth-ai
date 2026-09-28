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

use commonwealth_core::activity::{ActivityEvent, ActivityEventKind, ActivitySummary};
use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::ModelInfo;
use commonwealth_core::oicp::EmbedModelInfo;

/// The storage-snapshot cadence, read by the loop that records through the
/// contribution port.
pub use commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL;
/// The record types these ports carry that live in commonwealth-state,
/// re-exported so the dialing side names them without that crate.
pub use commonwealth_state::inference_plan::{InferencePlan, ShardPlan};
pub use commonwealth_state::peer_preferences::{peer_preference, PeerPreference};

/// Why a ledger call produced no answer. The message names the process that
/// did not answer and why; it is reported, never defaulted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LedgerAbsent(pub String);

/// `Pin<Box<…>>` future returning `Result<T, LedgerAbsent>` — the boxed shape
/// [`crate::rail_port::RailFut`] uses.
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
/// `commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL`). Consumes
/// a `walker` closure that produces the per-corpus `(id, size_gb)` pairs to
/// record — the daemon supplies the walker, so this crate pulls in no
/// knowledge dep. An empty walker result records nothing.
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

/// Every ledger port, in process, over the store the daemon holds today —
/// the [`crate::rail_port::LocalRingRail`] precedent. Each method calls the
/// commonwealth-state writer or reader the port names; no key scheme lives
/// here (ARCH 8). The daemon's types flip onto this first (fp-80..fp-82) and
/// its backing onto the dial last (fp-88).
#[derive(Clone)]
pub struct LocalLedger {
    store: std::sync::Arc<commonwealth_state::MeshStore>,
    self_node_id: NodeId,
}

impl LocalLedger {
    pub fn new(store: std::sync::Arc<commonwealth_state::MeshStore>, self_node_id: NodeId) -> Self {
        Self {
            store,
            self_node_id,
        }
    }

    fn contributions(&self) -> commonwealth_state::ContributionEmitter {
        commonwealth_state::ContributionEmitter::new((*self.store).clone(), self.self_node_id)
    }

    fn preferences(&self) -> commonwealth_state::PeerPreferenceStore {
        commonwealth_state::PeerPreferenceStore::new((*self.store).clone(), self.self_node_id)
    }

    fn inference(&self) -> commonwealth_state::store_adapter::InferenceStateStore {
        commonwealth_state::store_adapter::InferenceStateStore::new(
            std::sync::Arc::clone(&self.store),
            self.self_node_id,
        )
    }
}

fn store_absent(what: &str, e: commonwealth_state::Error) -> LedgerAbsent {
    LedgerAbsent(format!("in-process mesh store: {what} failed: {e}"))
}

impl ContributionLedgerPort for LocalLedger {
    fn self_node_id(&self) -> NodeId {
        self.self_node_id
    }

    fn record(&self, kind: LedgerEventKind) -> LedgerFut<'_, ()> {
        Box::pin(async move {
            self.contributions().record(kind);
            Ok(())
        })
    }

    fn events(&self) -> LedgerFut<'_, Vec<LedgerEvent>> {
        Box::pin(async move {
            self.contributions()
                .events()
                .map_err(|e| store_absent("contributions", e))
        })
    }

    fn current_contributions(
        &self,
        peer_capabilities: &HashMap<NodeId, NodeCapabilities>,
        window_days: u32,
    ) -> LedgerFut<'_, HashMap<NodeId, NodeContributions>> {
        let peer_capabilities = peer_capabilities.clone();
        Box::pin(async move {
            commonwealth_state::current_contributions(&self.store, &peer_capabilities, window_days)
                .map_err(|e| store_absent("contributions/current", e))
        })
    }
}

impl ActivityLedgerPort for LocalLedger {
    fn record(&self, kind: ActivityEventKind) -> LedgerFut<'_, ()> {
        Box::pin(async move {
            commonwealth_state::ActivityEmitter::new((*self.store).clone(), self.self_node_id)
                .record(kind);
            Ok(())
        })
    }

    fn events(&self) -> LedgerFut<'_, Vec<ActivityEvent>> {
        Box::pin(async move {
            commonwealth_state::ActivityEmitter::new((*self.store).clone(), self.self_node_id)
                .events()
                .map_err(|e| store_absent("activity", e))
        })
    }

    fn current_activity(&self, window_days: u32) -> LedgerFut<'_, ActivitySummary> {
        Box::pin(async move {
            commonwealth_state::current_activity(&self.store, window_days)
                .map_err(|e| store_absent("activity/current", e))
        })
    }
}

impl PeerPreferencesPort for LocalLedger {
    fn list(&self) -> LedgerFut<'_, Vec<(NodeId, PeerPreference)>> {
        Box::pin(async move {
            self.preferences()
                .list()
                .map_err(|e| store_absent("peer-preferences", e))
        })
    }

    fn get(&self, peer: &NodeId) -> LedgerFut<'_, Option<PeerPreference>> {
        let peer = *peer;
        Box::pin(async move {
            self.preferences()
                .get(&peer)
                .map_err(|e| store_absent("peer-preferences/get", e))
        })
    }

    fn set(&self, peer: &NodeId, pref: PeerPreference) -> LedgerFut<'_, ()> {
        let peer = *peer;
        Box::pin(async move {
            self.preferences()
                .set(&peer, pref)
                .map_err(|e| store_absent("peer-preferences/set", e))
        })
    }

    fn clear(&self, peer: &NodeId) -> LedgerFut<'_, bool> {
        let peer = *peer;
        Box::pin(async move {
            self.preferences()
                .clear(&peer)
                .map_err(|e| store_absent("peer-preferences/clear", e))
        })
    }
}

impl ProcessedShardsPort for LocalLedger {
    fn publish(&self, corpus_id: &str, shards: &[usize]) -> LedgerFut<'_, bool> {
        let key = commonwealth_state::processed_shards_key(corpus_id, self.self_node_id);
        let payload = serde_json::to_vec(shards);
        Box::pin(async move {
            let payload = payload
                .map_err(|e| LedgerAbsent(format!("processed shards are not serializable: {e}")))?;
            self.store
                .set(
                    commonwealth_state::PROCESSED_SHARDS_APP_ID,
                    &key,
                    payload.into(),
                    self.self_node_id,
                )
                .map_err(|e| store_absent("processed-shards", e))
        })
    }

    fn union(&self, corpus_id: &str) -> LedgerFut<'_, BTreeSet<usize>> {
        let corpus_id = corpus_id.to_string();
        Box::pin(async move {
            Ok(commonwealth_state::union_processed_shards(
                &self.store,
                &corpus_id,
            ))
        })
    }
}

// `InferenceStateStore` swallows its store errors (its methods answer
// `Option`/`()`/`bool`); so does this, exactly as the cw-rails doors do.
impl InferenceStatePort for LocalLedger {
    fn get_plan(&self) -> LedgerFut<'_, Option<InferencePlan>> {
        Box::pin(async move { Ok(self.inference().get_plan()) })
    }

    fn set_plan(&self, plan: &InferencePlan) -> LedgerFut<'_, ()> {
        let plan = plan.clone();
        Box::pin(async move {
            self.inference().set_plan(&plan);
            Ok(())
        })
    }

    fn get_model_info(&self, model_id: ModelId) -> LedgerFut<'_, Option<ModelInfo>> {
        Box::pin(async move { Ok(self.inference().get_model_info(model_id)) })
    }

    fn set_model_info(&self, info: &ModelInfo) -> LedgerFut<'_, ()> {
        let info = info.clone();
        Box::pin(async move {
            self.inference().set_model_info(&info);
            Ok(())
        })
    }

    fn remove_model_info(&self, model_id: ModelId) -> LedgerFut<'_, bool> {
        Box::pin(async move { Ok(self.inference().remove_model_info(model_id)) })
    }

    fn list_models_with_origins(&self) -> LedgerFut<'_, Vec<(NodeId, ModelInfo)>> {
        Box::pin(async move { Ok(self.inference().list_models_with_origins()) })
    }

    fn get_llama_address(&self, model_id: ModelId) -> LedgerFut<'_, Option<String>> {
        Box::pin(async move { Ok(self.inference().get_llama_address(model_id)) })
    }

    fn set_llama_address(&self, model_id: ModelId, addr: &str) -> LedgerFut<'_, ()> {
        let addr = addr.to_string();
        Box::pin(async move {
            self.inference().set_llama_address(model_id, &addr);
            Ok(())
        })
    }

    fn set_local_embed_model(&self, info: &EmbedModelInfo) -> LedgerFut<'_, ()> {
        let info = info.clone();
        Box::pin(async move {
            self.inference().set_local_embed_model(&info);
            Ok(())
        })
    }

    fn get_local_embed_model(&self) -> LedgerFut<'_, Option<EmbedModelInfo>> {
        Box::pin(async move { Ok(self.inference().get_local_embed_model()) })
    }
}

#[cfg(test)]
mod tests;
