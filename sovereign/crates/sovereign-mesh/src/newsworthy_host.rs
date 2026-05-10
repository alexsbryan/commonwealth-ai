//! Concrete `NewsworthyHost` impl for the embedded Commonwealth daemon.
//!
//! Bridges between corpus-engine's host-agnostic
//! [`corpus_engine::update::newsworthy_watcher::NewsworthyHost`] trait
//! and the live mesh state held by `commonwealth_api::AppState`.
//!
//! - Mesh-state queries (`is_leader`, `is_owner_of`) read the
//!   `Arc<RwLock<Mesh>>` carried on `AppStateInner` and run the
//!   answer through `commonwealth_core::partition::{is_leader,
//!   is_owner}`.
//! - KV operations forward to `MeshStore` directly. SQLite is
//!   blocking-friendly under tokio's full runtime; the existing
//!   `peer_preferences` store does the same and ships in production
//!   today.
//!
//! The watcher itself never sees `MeshStore`, `Mesh`, or `NodeId` —
//! the trait keeps `corpus-engine` free of any Commonwealth dependency
//! per the architectural seam in §6 of `SYSTEM_OVERVIEW.md`.

use std::sync::Arc;

use bytes::Bytes;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_core::partition;
use commonwealth_state::MeshStore;
use corpus_engine::error::{Error as CorpusError, Result as CorpusResult};
use corpus_engine::update::newsworthy_watcher::NewsworthyHost;

pub struct MeshNewsworthyHost {
    app_state: AppState,
}

impl MeshNewsworthyHost {
    pub fn new(app_state: AppState) -> Self {
        Self { app_state }
    }

    fn mesh_store(&self) -> &Arc<MeshStore> {
        &self.app_state.inner.mesh_store
    }

    fn self_node_id(&self) -> NodeId {
        self.app_state.self_node_id()
    }

    /// Snapshot the current online member set. Online = `Online` *or*
    /// `Busy` *or* `Away` — i.e. anything that isn't `Offline`. The
    /// inference scheduler treats Busy/Away the same way for leader
    /// election, so we follow suit to keep behaviour consistent across
    /// daemons.
    async fn online_members(&self) -> Vec<NodeId> {
        let mesh = self.app_state.inner.mesh.read().await;
        mesh.members
            .iter()
            .filter(|(_, m)| m.status != NodeStatus::Offline)
            .map(|(id, _)| *id)
            .collect()
    }
}

#[async_trait::async_trait]
impl NewsworthyHost for MeshNewsworthyHost {
    fn self_node_id_str(&self) -> String {
        self.self_node_id().to_string()
    }

    async fn is_leader(&self) -> bool {
        let online = self.online_members().await;
        partition::is_leader(self.self_node_id(), &online)
    }

    async fn is_owner_of(&self, partition_key: &str) -> bool {
        let online = self.online_members().await;
        partition::is_owner(self.self_node_id(), partition_key, &online)
    }

    fn store_get(&self, app_id: &str, key: &str) -> CorpusResult<Option<Vec<u8>>> {
        match self
            .mesh_store()
            .get(app_id, key)
            .map_err(|e| CorpusError::Database(format!("MeshStore.get: {e}")))?
        {
            Some(entry) => Ok(Some(entry.value.to_vec())),
            None => Ok(None),
        }
    }

    fn store_set(&self, app_id: &str, key: &str, value: Vec<u8>) -> CorpusResult<()> {
        self.mesh_store()
            .set(app_id, key, Bytes::from(value), self.self_node_id())
            .map(|_| ())
            .map_err(|e| CorpusError::Database(format!("MeshStore.set: {e}")))
    }

    fn store_scan(
        &self,
        app_id: &str,
        prefix: &str,
    ) -> CorpusResult<Vec<(String, Vec<u8>)>> {
        let entries = self
            .mesh_store()
            .scan(app_id, prefix)
            .map_err(|e| CorpusError::Database(format!("MeshStore.scan: {e}")))?;
        Ok(entries
            .into_iter()
            .map(|e| (e.key, e.value.to_vec()))
            .collect())
    }

    fn store_delete(&self, app_id: &str, key: &str) -> CorpusResult<bool> {
        self.mesh_store()
            .delete(app_id, key)
            .map_err(|e| CorpusError::Database(format!("MeshStore.delete: {e}")))
    }
}
