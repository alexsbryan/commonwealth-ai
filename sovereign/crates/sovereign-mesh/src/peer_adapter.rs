// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh side of the two peer ports — the N>1 adapters.
//!
//! `sovereign-contracts::peer` declares what a daemon needs from its peers:
//! a replicated KV store and a pair of convergence stamps. It ships the honest
//! N=1 implementations of both. This module ships the other implementation,
//! the one backed by the actual mesh.
//!
//! **The arrow this reverses.** Until cw-lift 3b, `sovereign-cli-daemon` named
//! `commonwealth_state::MeshStore` and `commonwealth_api::state::ConvergenceRecord`
//! in its own bootstrap, so the local daemon could not link without the mesh
//! substrate. Now the daemon names the port, and this crate — which exists to
//! be the Commonwealth integration layer — supplies the mesh implementation of
//! it. "Mesh is an extension" stops being an aspiration about layering and
//! becomes a fact about which adapter is constructed.
//!
//! Both adapters own the underlying handle rather than borrowing it, because
//! two consumers must reach the SAME instance: the daemon's own note pipeline
//! (through the port) and the gossip/`/status` surfaces inside this crate
//! (through [`MeshPeerStore::inner`] / [`MeshConvergence::inner`], which are
//! crate-private on purpose). A second store would gossip nothing and a second
//! recorder would make `/status` report a liveness that no writer stamps.

use std::sync::Arc;

use bytes::Bytes;
use commonwealth_api::state::ConvergenceRecord;
use commonwealth_state::MeshStore;
use kernel_types::NodeId;
use sovereign_contracts::peer::{Convergence, PeerEntry, PeerStore, PeerStoreError};

/// [`PeerStore`] backed by the gossiped [`MeshStore`].
///
/// A thin projection: four of the store's fourteen methods, which is what the
/// port's consumers call. The rest — `append`, `merge_entry`,
/// `all_entries_for_gossip`, the four `gc_*` — are the mesh's own business and
/// stay reachable through [`MeshPeerStore::inner`] inside this crate.
#[derive(Clone)]
pub struct MeshPeerStore {
    inner: Arc<MeshStore>,
}

impl std::fmt::Debug for MeshPeerStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `MeshStore` has no Debug; name the shape rather than the contents.
        f.write_str("MeshPeerStore { .. }")
    }
}

impl MeshPeerStore {
    /// An in-memory mesh store.
    ///
    /// This is what the daemon's work atlas and notes rail run on: the
    /// atlas-relevant records have TTLs measured in hours and long-term
    /// persistence is the mesh itself, so restart cost is acceptable.
    pub fn in_memory() -> Result<Self, PeerStoreError> {
        MeshStore::in_memory()
            .map(|s| Self { inner: Arc::new(s) })
            .map_err(to_port_error)
    }

    /// Open (or create) the store at `path`.
    ///
    /// The persisted counterpart of [`MeshPeerStore::in_memory`], for the CLI
    /// surfaces that read a workstation's `mesh.db` rather than the daemon's
    /// live in-memory one.
    pub fn open(path: &std::path::Path) -> Result<Self, PeerStoreError> {
        MeshStore::open(path)
            .map(|s| Self { inner: Arc::new(s) })
            .map_err(to_port_error)
    }

    /// Wrap a store this crate already holds.
    pub fn from_store(inner: Arc<MeshStore>) -> Self {
        Self { inner }
    }

    /// The underlying store, for the mesh-side machinery that needs the whole
    /// surface (gossip enumeration, GC, merge). Crate-private: a consumer that
    /// could reach through the port to the concrete store would make the port
    /// decorative.
    pub(crate) fn inner(&self) -> Arc<MeshStore> {
        Arc::clone(&self.inner)
    }
}

fn to_port_error(e: commonwealth_state::error::Error) -> PeerStoreError {
    PeerStoreError::Backend(e.to_string())
}

fn to_port_entry(e: commonwealth_state::StoreEntry) -> PeerEntry {
    PeerEntry {
        app_id: e.app_id,
        key: e.key,
        value: e.value,
        timestamp: e.timestamp,
        origin: e.origin,
    }
}

impl PeerStore for MeshPeerStore {
    fn get(&self, app_id: &str, key: &str) -> Result<Option<PeerEntry>, PeerStoreError> {
        self.inner
            .get(app_id, key)
            .map(|o| o.map(to_port_entry))
            .map_err(to_port_error)
    }

    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: Bytes,
        origin: NodeId,
    ) -> Result<bool, PeerStoreError> {
        self.inner
            .set(app_id, key, value, origin)
            .map_err(to_port_error)
    }

    fn delete(&self, app_id: &str, key: &str) -> Result<bool, PeerStoreError> {
        self.inner.delete(app_id, key).map_err(to_port_error)
    }

    fn scan(&self, app_id: &str, prefix: &str) -> Result<Vec<PeerEntry>, PeerStoreError> {
        self.inner
            .scan(app_id, prefix)
            .map(|v| v.into_iter().map(to_port_entry).collect())
            .map_err(to_port_error)
    }
}

/// [`Convergence`] backed by the [`ConvergenceRecord`] that `/status` reads.
///
/// The record is installed onto `AppState` at daemon start
/// (`install_convergence_recorder`), so the writers reached through this port
/// and the `/status` reader are the same instance by construction.
#[derive(Debug, Clone)]
pub struct MeshConvergence {
    inner: Arc<ConvergenceRecord>,
}

impl Default for MeshConvergence {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshConvergence {
    /// A fresh recorder: both paths never-succeeded since boot.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ConvergenceRecord::new()),
        }
    }

    /// The underlying record, for installation onto `AppState`. Crate-private
    /// for the same reason as [`MeshPeerStore::inner`].
    pub(crate) fn inner(&self) -> Arc<ConvergenceRecord> {
        Arc::clone(&self.inner)
    }
}

impl Convergence for MeshConvergence {
    fn record_outbound_publish_success(&self, at_unix: i64) {
        self.inner.record_outbound_publish_success(at_unix);
    }

    fn record_inbound_ingest_success(&self, at_unix: i64) {
        self.inner.record_inbound_ingest_success(at_unix);
    }

    fn snapshot(&self) -> (Option<i64>, Option<i64>) {
        self.inner.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mesh adapter must answer the SAME questions the solo one does.
    /// Without this the port is two unrelated surfaces that happen to share a
    /// name, and a consumer written against one would misread the other.
    #[test]
    fn mesh_peer_store_agrees_with_the_solo_answers() {
        let store = MeshPeerStore::in_memory().expect("in-memory store");
        let origin = NodeId::from_u128(3);

        assert_eq!(store.get("notes", "k").unwrap(), None);
        assert!(!store.delete("notes", "k").unwrap());
        assert!(store
            .set("notes", "k", Bytes::from_static(b"a"), origin)
            .unwrap());
        assert!(
            !store
                .set("notes", "k", Bytes::from_static(b"a"), origin)
                .unwrap(),
            "identical bytes are not a change"
        );

        let got = store.get("notes", "k").unwrap().expect("just written");
        assert_eq!(got.app_id, "notes");
        assert_eq!(got.key, "k");
        assert_eq!(got.value, Bytes::from_static(b"a"));
        assert_eq!(got.origin, origin);

        store
            .set("notes", "k2", Bytes::from_static(b"b"), origin)
            .unwrap();
        store
            .set("notes-private", "k", Bytes::from_static(b"c"), origin)
            .unwrap();
        assert_eq!(store.scan("notes", "").unwrap().len(), 2);
        assert_eq!(store.scan("notes", "k2").unwrap().len(), 1);
        assert!(store.scan("never-written", "").unwrap().is_empty());

        assert!(store.delete("notes", "k").unwrap());
        assert!(!store.delete("notes", "k").unwrap());
    }

    #[test]
    fn mesh_convergence_reports_absence_until_a_path_runs() {
        let c = MeshConvergence::new();
        assert_eq!(c.snapshot(), (None, None));
        c.record_outbound_publish_success(1_700_000_000);
        assert_eq!(c.snapshot(), (Some(1_700_000_000), None));
        c.record_inbound_ingest_success(1_700_000_042);
        assert_eq!(c.snapshot(), (Some(1_700_000_000), Some(1_700_000_042)));
    }

    /// The installed record and the port write to the same place — the
    /// property `install_convergence_recorder`'s "ONE instance" comment
    /// claims and nothing asserted.
    #[test]
    fn mesh_convergence_port_and_installed_record_are_one_instance() {
        let c = MeshConvergence::new();
        let installed = c.inner();
        c.record_outbound_publish_success(99);
        assert_eq!(installed.snapshot(), (Some(99), None));
        installed.record_inbound_ingest_success(100);
        assert_eq!(c.snapshot(), (Some(99), Some(100)));
    }
}
