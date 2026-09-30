// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed ledger ports' dialing implementation, over `cw-rails`'
//! `/v1/ledger/*` doors (five-programs fp-78; the doors are
//! `commonwealth-rails/src/ledger.rs`), and the read-through cache the four
//! synchronous `InferenceStateStore` readers keep (§12 D4).
//!
//! Nothing here is wired into `AppState` yet — the daemon's callers flip in
//! fp-80..fp-82. Every failed dial is a [`LedgerAbsent`] carrying the dial's
//! own sentence (it names the rails daemon's URL), never an empty answer.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use kernel_types::ModelId;
use kernel_types::NodeId;
use oicp_types::activity::{ActivityEvent, ActivityEventKind, ActivitySummary};
use oicp_types::capabilities::NodeCapabilities;
use oicp_types::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use oicp_types::model_catalog::ModelInfo;
use oicp_types::EmbedModelInfo;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sovereign_mesh::ledger_port::{
    ActivityLedgerPort, ContributionLedgerPort, InferencePlan, InferenceStatePort, LedgerAbsent,
    LedgerFut, PeerPreference, PeerPreferencesPort, ProcessedShardsPort,
};

use super::{get_answer, post_answer, RailsDial};

fn absent(e: RailsDial) -> LedgerAbsent {
    LedgerAbsent(e.to_string())
}

async fn ledger_get<T: DeserializeOwned>(base: &str, path: &str) -> Result<T, LedgerAbsent> {
    get_answer(base, path, path).await.map_err(absent)
}

async fn ledger_post<T: DeserializeOwned>(
    base: &str,
    path: &str,
    body: Value,
) -> Result<T, LedgerAbsent> {
    post_answer(base, path, &body).await.map_err(absent)
}

/// Every ledger port, dialed. `self_node_id` is the daemon's: every row a
/// door writes for this port carries it as origin.
#[derive(Clone)]
pub struct RailsLedger {
    base: String,
    self_node_id: NodeId,
}

impl RailsLedger {
    pub fn new(base: impl Into<String>, self_node_id: NodeId) -> Self {
        Self {
            base: base.into(),
            self_node_id,
        }
    }

    fn written(&self, record: impl serde::Serialize) -> Value {
        json!({ "node_id": self.self_node_id, "record": record })
    }
}

impl ContributionLedgerPort for RailsLedger {
    fn self_node_id(&self) -> NodeId {
        self.self_node_id
    }

    fn record(&self, kind: LedgerEventKind) -> LedgerFut<'_, ()> {
        let body = self.written(kind);
        Box::pin(async move { ledger_post(&self.base, "/v1/ledger/contributions", body).await })
    }

    fn events(&self) -> LedgerFut<'_, Vec<LedgerEvent>> {
        Box::pin(async move { ledger_get(&self.base, "/v1/ledger/contributions").await })
    }

    fn current_contributions(
        &self,
        peer_capabilities: &HashMap<NodeId, NodeCapabilities>,
        window_days: u32,
    ) -> LedgerFut<'_, HashMap<NodeId, NodeContributions>> {
        let pairs: Vec<(&NodeId, &NodeCapabilities)> = peer_capabilities.iter().collect();
        let body = json!({ "peer_capabilities": pairs, "window_days": window_days });
        Box::pin(async move {
            let pairs: Vec<(NodeId, NodeContributions)> =
                ledger_post(&self.base, "/v1/ledger/contributions/current", body).await?;
            Ok(pairs.into_iter().collect())
        })
    }
}

impl ActivityLedgerPort for RailsLedger {
    fn record(&self, kind: ActivityEventKind) -> LedgerFut<'_, ()> {
        let body = self.written(kind);
        Box::pin(async move { ledger_post(&self.base, "/v1/ledger/activity", body).await })
    }

    fn events(&self) -> LedgerFut<'_, Vec<ActivityEvent>> {
        Box::pin(async move { ledger_get(&self.base, "/v1/ledger/activity").await })
    }

    fn current_activity(&self, window_days: u32) -> LedgerFut<'_, ActivitySummary> {
        let body = json!({ "window_days": window_days });
        Box::pin(async move { ledger_post(&self.base, "/v1/ledger/activity/current", body).await })
    }
}

impl PeerPreferencesPort for RailsLedger {
    fn list(&self) -> LedgerFut<'_, Vec<(NodeId, PeerPreference)>> {
        Box::pin(async move { ledger_get(&self.base, "/v1/ledger/peer-preferences").await })
    }

    fn get(&self, peer: &NodeId) -> LedgerFut<'_, Option<PeerPreference>> {
        let body = json!({ "peer": peer });
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/peer-preferences/get", body).await },
        )
    }

    fn set(&self, peer: &NodeId, pref: PeerPreference) -> LedgerFut<'_, ()> {
        let body = json!({ "node_id": self.self_node_id, "peer": peer, "pref": pref });
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/peer-preferences/set", body).await },
        )
    }

    fn clear(&self, peer: &NodeId) -> LedgerFut<'_, bool> {
        let body = json!({ "peer": peer });
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/peer-preferences/clear", body).await },
        )
    }
}

impl ProcessedShardsPort for RailsLedger {
    fn publish(&self, corpus_id: &str, shards: &[usize]) -> LedgerFut<'_, bool> {
        let body =
            json!({ "corpus_id": corpus_id, "node_id": self.self_node_id, "shards": shards });
        Box::pin(async move { ledger_post(&self.base, "/v1/ledger/processed-shards", body).await })
    }

    fn union(&self, corpus_id: &str) -> LedgerFut<'_, BTreeSet<usize>> {
        let body = json!({ "corpus_id": corpus_id });
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/processed-shards/union", body).await },
        )
    }
}

impl InferenceStatePort for RailsLedger {
    fn get_plan(&self) -> LedgerFut<'_, Option<InferencePlan>> {
        Box::pin(async move { ledger_get(&self.base, "/v1/ledger/inference/plan").await })
    }

    fn set_plan(&self, plan: &InferencePlan) -> LedgerFut<'_, ()> {
        let body = self.written(plan);
        Box::pin(async move { ledger_post(&self.base, "/v1/ledger/inference/plan", body).await })
    }

    fn get_model_info(&self, model_id: ModelId) -> LedgerFut<'_, Option<ModelInfo>> {
        let body = json!({ "model_id": model_id });
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/inference/model/get", body).await },
        )
    }

    fn set_model_info(&self, info: &ModelInfo) -> LedgerFut<'_, ()> {
        let body = self.written(info);
        Box::pin(async move { ledger_post(&self.base, "/v1/ledger/inference/model", body).await })
    }

    fn remove_model_info(&self, model_id: ModelId) -> LedgerFut<'_, bool> {
        let body = json!({ "node_id": self.self_node_id, "model_id": model_id });
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/inference/model/remove", body).await },
        )
    }

    fn list_models_with_origins(&self) -> LedgerFut<'_, Vec<(NodeId, ModelInfo)>> {
        Box::pin(async move { ledger_get(&self.base, "/v1/ledger/inference/models").await })
    }

    fn get_llama_address(&self, model_id: ModelId) -> LedgerFut<'_, Option<String>> {
        let body = json!({ "model_id": model_id });
        Box::pin(async move {
            ledger_post(&self.base, "/v1/ledger/inference/llama-address/get", body).await
        })
    }

    fn set_llama_address(&self, model_id: ModelId, addr: &str) -> LedgerFut<'_, ()> {
        let body = json!({ "node_id": self.self_node_id, "model_id": model_id, "addr": addr });
        Box::pin(async move {
            ledger_post(&self.base, "/v1/ledger/inference/llama-address", body).await
        })
    }

    fn set_local_embed_model(&self, info: &EmbedModelInfo) -> LedgerFut<'_, ()> {
        let body = self.written(info);
        Box::pin(
            async move { ledger_post(&self.base, "/v1/ledger/inference/embed-model", body).await },
        )
    }

    fn get_local_embed_model(&self) -> LedgerFut<'_, Option<EmbedModelInfo>> {
        Box::pin(async move { ledger_get(&self.base, "/v1/ledger/inference/embed-model").await })
    }
}

// ── The synchronous readers' cache (§12 D4) ──────────────────

/// The cache has never been filled: there is no reading to serve. Reported
/// in place of an empty model map, which would claim "this mesh has no
/// models" (principle 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the inference state has never been read from the mesh's rails daemon")]
pub struct NeverFilled;

#[derive(Clone)]
struct Filled {
    models: Vec<(NodeId, ModelInfo)>,
    embed_model: Option<EmbedModelInfo>,
}

/// A daemon-local read-through cache over [`InferenceStatePort`] for the
/// readers the daemon calls synchronously — `list_models`,
/// `list_models_with_origins`, `get_local_embed_model` — plus the
/// synchronous `set_model_info`. [`InferenceCache::refill`] dials the doors;
/// a failed refill keeps the last value and traces why.
pub struct InferenceCache {
    port: Arc<dyn InferenceStatePort>,
    self_node_id: NodeId,
    filled: RwLock<Option<Filled>>,
}

impl InferenceCache {
    pub fn new(port: Arc<dyn InferenceStatePort>, self_node_id: NodeId) -> Self {
        Self {
            port,
            self_node_id,
            filled: RwLock::new(None),
        }
    }

    /// Read the models and the embed model through the doors and replace the
    /// cached value. Both answer or neither replaces: a half-refill would
    /// serve a reading no single moment held.
    pub async fn refill(&self) -> Result<(), LedgerAbsent> {
        let read = async {
            let models = self.port.list_models_with_origins().await?;
            let embed_model = self.port.get_local_embed_model().await?;
            Ok::<_, LedgerAbsent>(Filled {
                models,
                embed_model,
            })
        };
        match read.await {
            Ok(filled) => {
                tracing::debug!(
                    models = filled.models.len(),
                    embed_model = filled.embed_model.is_some(),
                    "inference cache: refilled"
                );
                *self.filled.write().unwrap_or_else(|p| p.into_inner()) = Some(filled);
                Ok(())
            }
            Err(e) => {
                let had = self
                    .filled
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_some();
                tracing::warn!(
                    error = %e,
                    keeping_last_value = had,
                    "inference cache: refill failed; the last value stands"
                );
                Err(e)
            }
        }
    }

    fn read<T>(&self, f: impl FnOnce(&Filled) -> T) -> Result<T, NeverFilled> {
        self.filled
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(f)
            .ok_or(NeverFilled)
    }

    /// `InferenceStateStore::list_models`: the same scan keyed by model id.
    pub fn list_models(&self) -> Result<HashMap<ModelId, ModelInfo>, NeverFilled> {
        self.read(|f| f.models.iter().map(|(_, m)| (m.id, m.clone())).collect())
    }

    /// `InferenceStateStore::list_models_with_origins`.
    pub fn list_models_with_origins(&self) -> Result<Vec<(NodeId, ModelInfo)>, NeverFilled> {
        self.read(|f| f.models.clone())
    }

    /// `InferenceStateStore::get_local_embed_model`.
    pub fn get_local_embed_model(&self) -> Result<Option<EmbedModelInfo>, NeverFilled> {
        self.read(|f| f.embed_model.clone())
    }

    /// `InferenceStateStore::set_model_info`, still synchronous: the row is
    /// written into a filled cache at once (as this node's), and the door
    /// write runs on the current runtime; its failure is traced, and the next
    /// refill reports what the store actually holds. An unfilled cache stays
    /// unfilled — one row is not a reading of the whole set.
    pub fn set_model_info(&self, info: &ModelInfo) {
        if let Some(filled) = self
            .filled
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            filled.models.retain(|(_, m)| m.id != info.id);
            filled.models.push((self.self_node_id, info.clone()));
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                model = %info.id,
                "inference cache: no runtime to carry the model-info write; it did not reach the store"
            );
            return;
        };
        let port = self.port.clone();
        let info = info.clone();
        runtime.spawn(async move {
            if let Err(e) = port.set_model_info(&info).await {
                tracing::warn!(
                    model = %info.id,
                    error = %e,
                    "inference cache: the model-info write did not reach the store"
                );
            }
        });
    }
}

#[cfg(test)]
#[path = "ledger/tests.rs"]
mod tests;
