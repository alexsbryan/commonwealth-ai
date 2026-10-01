// SPDX-License-Identifier: AGPL-3.0-or-later
//! A RECORDING double of the store ports AppState holds (five-programs fp-80,
//! for fp-83..fp-86): every call is appended to [`RecordingLedger::calls`] and
//! answered empty, save two port contracts it keeps in memory: the model rows
//! (seeded with [`RecordingLedger::with_models`], fp-84, and written by
//! `set_model_info`/`remove_model_info` as this node's) and the peer
//! preferences (`set` then `get`/`list`/`clear`). Those two are what the
//! in-process `LocalLedger` answered for the daemon's own round-trip tests
//! before it retired (pb-mesh-exit-mesh). It implements no key scheme — the
//! writers in commonwealth-state are the one decider of those (ARCH 8).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use crate::ledger_port::{
    ActivityLedgerPort, ContributionLedgerPort, InferencePlan, InferenceStatePort, LedgerFut,
    PeerPreference, PeerPreferencesPort, ProcessedShardsPort,
};
use crate::state::store::StoreSeed;
use bytes::Bytes;
use kernel_types::{ModelId, NodeId};
use oicp_types::activity::{ActivityEvent, ActivityEventKind, ActivitySummary};
use oicp_types::capabilities::NodeCapabilities;
use oicp_types::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use oicp_types::model_catalog::ModelInfo;
use oicp_types::EmbedModelInfo;
use sovereign_contracts::peer::{ReplicatedKv, ReplicatedKvEntry, ReplicatedKvError};

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
    recorded: Arc<Mutex<Vec<LedgerEventKind>>>,
    models: Arc<Mutex<Vec<(NodeId, ModelInfo)>>>,
    preferences: Arc<Mutex<Vec<(NodeId, PeerPreference)>>>,
    plan: Arc<Mutex<Option<InferencePlan>>>,
    llama_addresses: Arc<Mutex<HashMap<ModelId, String>>>,
}

impl RecordingLedger {
    pub fn new(self_node_id: NodeId) -> Self {
        Self {
            self_node_id,
            calls: Arc::default(),
            recorded: Arc::default(),
            models: Arc::default(),
            preferences: Arc::default(),
            plan: Arc::default(),
            llama_addresses: Arc::default(),
        }
    }

    /// The `(origin, model)` rows `list_models_with_origins` answers — handed
    /// back verbatim, as a peer's gossiped rows would arrive.
    pub fn with_models(self, rows: Vec<(NodeId, ModelInfo)>) -> Self {
        *self.models.lock().unwrap() = rows;
        self
    }

    /// A [`StoreSeed`] whose every port is this double (five-programs fp-84).
    pub fn seed(self: &Arc<Self>) -> StoreSeed {
        StoreSeed {
            kv: self.clone(),
            contributions: self.clone(),
            activity: self.clone(),
            peer_preferences: self.clone(),
            inference: self.clone(),
            processed_shards: self.clone(),
        }
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<LedgerCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Every `contributions.record` kind so far, typed, in order — for a
    /// test that asserts on the event's fields rather than its `Debug` text.
    pub fn recorded_contributions(&self) -> Vec<LedgerEventKind> {
        self.recorded.lock().unwrap().clone()
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
        let args = format!("{kind:?}");
        self.recorded.lock().unwrap().push(kind);
        self.answer("contributions.record", args, ())
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

    fn events(&self) -> LedgerFut<'_, Vec<ActivityEvent>> {
        self.answer("activity.events", String::new(), Vec::new())
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
        let rows = self.preferences.lock().unwrap().clone();
        self.answer("peer_preferences.list", String::new(), rows)
    }

    fn get(&self, peer: &NodeId) -> LedgerFut<'_, Option<PeerPreference>> {
        let found = self
            .preferences
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| p == peer)
            .map(|(_, pref)| pref.clone());
        self.answer("peer_preferences.get", peer.to_string(), found)
    }

    fn set(&self, peer: &NodeId, pref: PeerPreference) -> LedgerFut<'_, ()> {
        let args = format!("{peer} {pref:?}");
        let mut rows = self.preferences.lock().unwrap();
        rows.retain(|(p, _)| p != peer);
        rows.push((*peer, pref));
        drop(rows);
        self.answer("peer_preferences.set", args, ())
    }

    fn clear(&self, peer: &NodeId) -> LedgerFut<'_, bool> {
        let mut rows = self.preferences.lock().unwrap();
        let before = rows.len();
        rows.retain(|(p, _)| p != peer);
        let was_there = rows.len() != before;
        drop(rows);
        self.answer("peer_preferences.clear", peer.to_string(), was_there)
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
        let plan = self.plan.lock().unwrap().clone();
        self.answer("inference.get_plan", String::new(), plan)
    }

    fn set_plan(&self, plan: &InferencePlan) -> LedgerFut<'_, ()> {
        *self.plan.lock().unwrap() = Some(plan.clone());
        self.answer("inference.set_plan", format!("{plan:?}"), ())
    }

    fn get_model_info(&self, model_id: ModelId) -> LedgerFut<'_, Option<ModelInfo>> {
        let found = self
            .models
            .lock()
            .unwrap()
            .iter()
            .find(|(_, m)| m.id == model_id)
            .map(|(_, m)| m.clone());
        self.answer("inference.get_model_info", format!("{model_id:?}"), found)
    }

    fn set_model_info(&self, info: &ModelInfo) -> LedgerFut<'_, ()> {
        let mut rows = self.models.lock().unwrap();
        rows.retain(|(origin, m)| !(*origin == self.self_node_id && m.id == info.id));
        rows.push((self.self_node_id, info.clone()));
        drop(rows);
        self.answer("inference.set_model_info", format!("{:?}", info.id), ())
    }

    fn remove_model_info(&self, model_id: ModelId) -> LedgerFut<'_, bool> {
        let mut rows = self.models.lock().unwrap();
        let before = rows.len();
        rows.retain(|(origin, m)| !(*origin == self.self_node_id && m.id == model_id));
        let was_there = rows.len() != before;
        drop(rows);
        self.answer(
            "inference.remove_model_info",
            format!("{model_id:?}"),
            was_there,
        )
    }

    fn list_models_with_origins(&self) -> LedgerFut<'_, Vec<(NodeId, ModelInfo)>> {
        let rows = self.models.lock().unwrap().clone();
        self.answer("inference.list_models_with_origins", String::new(), rows)
    }

    fn get_llama_address(&self, model_id: ModelId) -> LedgerFut<'_, Option<String>> {
        let addr = self.llama_addresses.lock().unwrap().get(&model_id).cloned();
        self.answer("inference.get_llama_address", format!("{model_id:?}"), addr)
    }

    fn set_llama_address(&self, model_id: ModelId, addr: &str) -> LedgerFut<'_, ()> {
        self.llama_addresses
            .lock()
            .unwrap()
            .insert(model_id, addr.to_string());
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
