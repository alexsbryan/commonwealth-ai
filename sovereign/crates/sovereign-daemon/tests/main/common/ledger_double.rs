// SPDX-License-Identifier: AGPL-3.0-or-later
//! A RECORDING double of the store ports AppState holds (five-programs fp-80,
//! for fp-83..fp-86): every call is appended to [`RecordingLedger::calls`] and
//! answered empty. It implements no key scheme — the writers in
//! commonwealth-state are the one decider of those (ARCH 8).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use commonwealth_core::activity::{ActivityEventKind, ActivitySummary};
use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::ModelInfo;
use commonwealth_core::oicp::EmbedModelInfo;
use sovereign_contracts::peer::{ReplicatedKv, ReplicatedKvEntry, ReplicatedKvError};
use sovereign_mesh::ledger_port::{
    ActivityLedgerPort, ContributionLedgerPort, InferencePlan, InferenceStatePort, LedgerFut,
    PeerPreference, PeerPreferencesPort, ProcessedShardsPort,
};

/// One recorded call: the port method and its arguments, `Debug`-rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerCall {
    pub method: &'static str,
    pub args: String,
}

#[derive(Clone)]
pub struct RecordingLedger {
    self_node_id: NodeId,
    calls: Arc<Mutex<Vec<LedgerCall>>>,
}

impl RecordingLedger {
    pub fn new(self_node_id: NodeId) -> Self {
        Self {
            self_node_id,
            calls: Arc::default(),
        }
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<LedgerCall> {
        self.calls.lock().unwrap().clone()
    }

    fn push(&self, method: &'static str, args: String) {
        self.calls.lock().unwrap().push(LedgerCall { method, args });
    }

    fn answer<T: Send + 'static>(
        &self,
        method: &'static str,
        args: String,
        t: T,
    ) -> LedgerFut<'_, T> {
        self.push(method, args);
        Box::pin(async move { Ok(t) })
    }
}

impl ReplicatedKv for RecordingLedger {
    fn get(&self, app_id: &str, key: &str) -> Result<Option<ReplicatedKvEntry>, ReplicatedKvError> {
        self.push("kv.get", format!("{app_id:?} {key:?}"));
        Ok(None)
    }

    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: Bytes,
        origin: NodeId,
    ) -> Result<bool, ReplicatedKvError> {
        self.push("kv.set", format!("{app_id:?} {key:?} {value:?} {origin}"));
        Ok(true)
    }

    fn delete(&self, app_id: &str, key: &str) -> Result<bool, ReplicatedKvError> {
        self.push("kv.delete", format!("{app_id:?} {key:?}"));
        Ok(false)
    }

    fn scan(
        &self,
        app_id: &str,
        prefix: &str,
    ) -> Result<Vec<ReplicatedKvEntry>, ReplicatedKvError> {
        self.push("kv.scan", format!("{app_id:?} {prefix:?}"));
        Ok(Vec::new())
    }
}

impl ContributionLedgerPort for RecordingLedger {
    fn self_node_id(&self) -> NodeId {
        self.self_node_id
    }

    fn record(&self, kind: LedgerEventKind) -> LedgerFut<'_, ()> {
        self.answer("contributions.record", format!("{kind:?}"), ())
    }

    fn events(&self) -> LedgerFut<'_, Vec<LedgerEvent>> {
        self.answer("contributions.events", String::new(), Vec::new())
    }

    fn current_contributions(
        &self,
        peer_capabilities: &HashMap<NodeId, NodeCapabilities>,
        window_days: u32,
    ) -> LedgerFut<'_, HashMap<NodeId, NodeContributions>> {
        let args = format!("{} peers, {window_days} days", peer_capabilities.len());
        self.answer("contributions.current", args, HashMap::new())
    }
}

impl ActivityLedgerPort for RecordingLedger {
    fn record(&self, kind: ActivityEventKind) -> LedgerFut<'_, ()> {
        self.answer("activity.record", format!("{kind:?}"), ())
    }

    fn current_activity(&self, window_days: u32) -> LedgerFut<'_, ActivitySummary> {
        self.answer(
            "activity.current",
            format!("{window_days} days"),
            ActivitySummary::default(),
        )
    }
}

impl PeerPreferencesPort for RecordingLedger {
    fn list(&self) -> LedgerFut<'_, Vec<(NodeId, PeerPreference)>> {
        self.answer("peer_preferences.list", String::new(), Vec::new())
    }

    fn get(&self, peer: &NodeId) -> LedgerFut<'_, Option<PeerPreference>> {
        self.answer("peer_preferences.get", peer.to_string(), None)
    }
}

impl ProcessedShardsPort for RecordingLedger {
    fn publish(&self, corpus_id: &str, shards: &[usize]) -> LedgerFut<'_, bool> {
        self.answer(
            "processed_shards.publish",
            format!("{corpus_id:?} {shards:?}"),
            true,
        )
    }

    fn union(&self, corpus_id: &str) -> LedgerFut<'_, BTreeSet<usize>> {
        self.answer(
            "processed_shards.union",
            format!("{corpus_id:?}"),
            BTreeSet::new(),
        )
    }
}

impl InferenceStatePort for RecordingLedger {
    fn get_plan(&self) -> LedgerFut<'_, Option<InferencePlan>> {
        self.answer("inference.get_plan", String::new(), None)
    }

    fn set_plan(&self, plan: &InferencePlan) -> LedgerFut<'_, ()> {
        self.answer("inference.set_plan", format!("{plan:?}"), ())
    }

    fn get_model_info(&self, model_id: ModelId) -> LedgerFut<'_, Option<ModelInfo>> {
        self.answer("inference.get_model_info", format!("{model_id:?}"), None)
    }

    fn set_model_info(&self, info: &ModelInfo) -> LedgerFut<'_, ()> {
        self.answer("inference.set_model_info", format!("{:?}", info.id), ())
    }

    fn remove_model_info(&self, model_id: ModelId) -> LedgerFut<'_, bool> {
        self.answer(
            "inference.remove_model_info",
            format!("{model_id:?}"),
            false,
        )
    }

    fn list_models_with_origins(&self) -> LedgerFut<'_, Vec<(NodeId, ModelInfo)>> {
        self.answer(
            "inference.list_models_with_origins",
            String::new(),
            Vec::new(),
        )
    }

    fn get_llama_address(&self, model_id: ModelId) -> LedgerFut<'_, Option<String>> {
        self.answer("inference.get_llama_address", format!("{model_id:?}"), None)
    }

    fn set_llama_address(&self, model_id: ModelId, addr: &str) -> LedgerFut<'_, ()> {
        self.answer(
            "inference.set_llama_address",
            format!("{model_id:?} {addr:?}"),
            (),
        )
    }

    fn set_local_embed_model(&self, info: &EmbedModelInfo) -> LedgerFut<'_, ()> {
        self.answer("inference.set_local_embed_model", format!("{info:?}"), ())
    }

    fn get_local_embed_model(&self) -> LedgerFut<'_, Option<EmbedModelInfo>> {
        self.answer("inference.get_local_embed_model", String::new(), None)
    }
}
