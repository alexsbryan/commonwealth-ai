// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshStore — the distributed key-value store for mesh apps.
//!
//! Each entry is scoped to an `app_id` + `key`. Conflict resolution is LWW
//! (last-write-wins) using a Unix-second `timestamp`. The underlying storage
//! is SQLite (WAL mode) via `SqliteBackend`. Entries are replicated across
//! nodes through the gossip layer.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;

use commonwealth_core::ids::NodeId;

use crate::backend::SqliteBackend;
use crate::error::{Error, Result};

/// A single entry in the mesh store.
#[derive(Debug, Clone)]
pub struct StoreEntry {
    pub app_id: String,
    pub key: String,
    pub value: Bytes,
    /// Unix seconds — last-write-wins conflict resolution.
    pub timestamp: u64,
    /// Node that originated this write.
    pub origin: NodeId,
}

/// The distributed KV store. Thread-safe; clone freely (backed by `Arc`).
#[derive(Clone)]
pub struct MeshStore {
    backend: Arc<SqliteBackend>,
}

impl MeshStore {
    /// Open (or create) the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let backend = SqliteBackend::open(path)?;
        Ok(Self {
            backend: Arc::new(backend),
        })
    }

    /// Create an in-memory store (useful for tests).
    pub fn in_memory() -> Result<Self> {
        use rusqlite::Connection;
        use std::sync::Mutex;
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Backend(format!("in-memory open failed: {e}")))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS store (
                 app_id    TEXT NOT NULL,
                 key       TEXT NOT NULL,
                 value     BLOB NOT NULL,
                 timestamp INTEGER NOT NULL,
                 origin    BLOB NOT NULL,
                 PRIMARY KEY (app_id, key)
             );",
        )
        .map_err(|e| Error::Backend(format!("in-memory init failed: {e}")))?;
        Ok(Self {
            backend: Arc::new(crate::backend::SqliteBackend {
                conn: Mutex::new(conn),
            }),
        })
    }

    /// Get an entry. Returns `None` if not found.
    pub fn get(&self, app_id: &str, key: &str) -> Result<Option<StoreEntry>> {
        match self.backend.get(app_id, key)? {
            None => Ok(None),
            Some(raw) => {
                let origin = node_id_from_bytes(&raw.origin)?;
                Ok(Some(StoreEntry {
                    app_id: app_id.to_string(),
                    key: key.to_string(),
                    value: Bytes::from(raw.value),
                    timestamp: raw.timestamp,
                    origin,
                }))
            }
        }
    }

    /// Write an entry with the current time as timestamp. Returns true if written (LWW).
    pub fn set(&self, app_id: &str, key: &str, value: Bytes, origin: NodeId) -> Result<bool> {
        let timestamp = now_secs();
        let entry = StoreEntry {
            app_id: app_id.to_string(),
            key: key.to_string(),
            value,
            timestamp,
            origin,
        };
        self.merge_entry(entry)
    }

    /// Append `value` to an existing entry or create it. Values are newline-joined.
    pub fn append(&self, app_id: &str, key: &str, value: Bytes, origin: NodeId) -> Result<()> {
        let existing = self.get(app_id, key)?;
        let new_value = match existing {
            Some(e) => {
                let mut combined = e.value.to_vec();
                combined.push(b'\n');
                combined.extend_from_slice(&value);
                Bytes::from(combined)
            }
            None => value,
        };
        self.set(app_id, key, new_value, origin)?;
        Ok(())
    }

    /// Delete an entry. Returns true if something was deleted.
    pub fn delete(&self, app_id: &str, key: &str) -> Result<bool> {
        self.backend.delete(app_id, key)
    }

    /// List all keys for an app.
    pub fn list_keys(&self, app_id: &str) -> Result<Vec<String>> {
        self.backend.list_keys(app_id)
    }

    /// Return all entries whose key starts with `prefix` for the given app.
    pub fn scan(&self, app_id: &str, prefix: &str) -> Result<Vec<StoreEntry>> {
        let rows = self.backend.scan_with_prefix(app_id, prefix)?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let origin = node_id_from_bytes(&row.origin)?;
            entries.push(StoreEntry {
                app_id: row.app_id,
                key: row.key,
                value: Bytes::from(row.value),
                timestamp: row.timestamp,
                origin,
            });
        }
        Ok(entries)
    }

    /// Return all entries for gossip broadcast. Filters out
    /// `app_id` namespaces that are explicitly local-only — see
    /// [`crate::peer_preferences::GOSSIP_EXCLUDED_APP_IDS`]. The
    /// exclusion is structural: a private operator preference
    /// must never propagate to the peer it penalizes, and the
    /// invariant is pinned by tests in the
    /// `peer_preferences` module.
    pub fn all_entries_for_gossip(&self) -> Result<Vec<StoreEntry>> {
        let rows = self.backend.all_rows()?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            if crate::peer_preferences::is_gossip_excluded(&row.app_id) {
                continue;
            }
            let origin = node_id_from_bytes(&row.origin)?;
            entries.push(StoreEntry {
                app_id: row.app_id,
                key: row.key,
                value: Bytes::from(row.value),
                timestamp: row.timestamp,
                origin,
            });
        }
        Ok(entries)
    }

    /// Merge an entry from gossip (LWW). Returns true if the entry was accepted.
    pub fn merge_entry(&self, entry: StoreEntry) -> Result<bool> {
        let origin_bytes = entry.origin.as_bytes().to_vec();
        self.backend.upsert_if_newer(
            &entry.app_id,
            &entry.key,
            &entry.value,
            entry.timestamp,
            &origin_bytes,
        )
    }

    /// Delete entries older than `ttl_seconds`. Returns count deleted.
    ///
    /// UNSCOPED — every app in the store is subject to the same cutoff.
    /// Correct only where every app's entries are refreshed or dead;
    /// prefer [`MeshStore::gc_app`] when you mean to bound one
    /// namespace.
    pub fn gc(&self, ttl_seconds: u64) -> Result<usize> {
        let cutoff = now_secs().saturating_sub(ttl_seconds);
        self.backend.delete_older_than(cutoff)
    }

    /// Delete entries older than `ttl_seconds` within a single
    /// `app_id`. Returns count deleted.
    pub fn gc_app(&self, app_id: &str, ttl_seconds: u64) -> Result<usize> {
        let cutoff = now_secs().saturating_sub(ttl_seconds);
        self.backend.delete_older_than_in_app(app_id, cutoff)
    }
}

use commonwealth_core::clock::unix_now_secs as now_secs;

fn node_id_from_bytes(bytes: &[u8]) -> Result<NodeId> {
    if bytes.len() != 16 {
        return Err(Error::NodeId(format!(
            "expected 16 bytes for NodeId, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(bytes);
    // Writers persist `origin.as_bytes().to_vec()` — a verbatim copy of
    // NodeId's internal byte array. NodeId stores its u128 big-endian
    // (`from_u128` calls `to_be_bytes`), so reading must invert with
    // `from_be_bytes`. Using `from_le_bytes` here silently reversed the
    // round-trip, leaving every `StoreEntry.origin` mis-identified.
    Ok(NodeId::from_u128(u128::from_be_bytes(arr)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;

    fn node(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    #[test]
    fn set_and_get_roundtrip() {
        let store = MeshStore::in_memory().unwrap();
        store
            .set("myapp", "greeting", Bytes::from("hello"), node(1))
            .unwrap();
        let entry = store.get("myapp", "greeting").unwrap().unwrap();
        assert_eq!(entry.value.as_ref(), b"hello");
        assert_eq!(entry.app_id, "myapp");
        assert_eq!(entry.key, "greeting");
        assert_eq!(entry.origin, node(1));
    }

    #[test]
    fn origin_round_trips_for_realistic_node_id() {
        // Regression test: real NodeIds (random 16 bytes, not low-int test
        // fixtures) used to silently byte-reverse on read because writes
        // were verbatim but reads went through `u128::from_le_bytes`. The
        // `node(1)` fixtures don't catch this — `0...01` looks identical
        // reversed at the display layer because Display only shows the
        // first 8 bytes — so we use a value whose low and high halves
        // differ.
        let store = MeshStore::in_memory().unwrap();
        let id = NodeId::from_u128(0x1122_3344_5566_7788_AABB_CCDD_EEFF_0011);
        store.set("a", "k", Bytes::from("v"), id).unwrap();
        let entry = store.get("a", "k").unwrap().unwrap();
        assert_eq!(entry.origin, id);
        let scanned = store.scan("a", "").unwrap();
        assert_eq!(scanned[0].origin, id);
    }

    #[test]
    fn get_missing_returns_none() {
        let store = MeshStore::in_memory().unwrap();
        assert!(store.get("myapp", "nope").unwrap().is_none());
    }

    /// Defence-in-depth pin for the peer-preferences privacy
    /// invariant (ARCH_PRINCIPLES §7.2 + §7.4). The
    /// `peer_preferences` namespace must NEVER appear in the
    /// gossip-broadcast set, even when an entry has been written
    /// to the store. A regression here would cause private
    /// affinity adjustments to leak to the peer being penalized
    /// — silently breaking the social-not-algorithmic sanction
    /// property.
    #[test]
    fn all_entries_for_gossip_excludes_peer_preferences_namespace() {
        let store = MeshStore::in_memory().unwrap();
        // Write a peer preference and a normal entry.
        store
            .set(
                "peer_preferences",
                "deadbeef",
                Bytes::from("private"),
                node(1),
            )
            .unwrap();
        store
            .set("contributions", "ev1", Bytes::from("public"), node(1))
            .unwrap();
        let gossipable = store.all_entries_for_gossip().unwrap();
        // Only the contributions entry survives the filter.
        assert_eq!(gossipable.len(), 1);
        assert_eq!(gossipable[0].app_id, "contributions");
        // But direct read still works — the entry IS persisted, just
        // not gossiped.
        assert!(store.get("peer_preferences", "deadbeef").unwrap().is_some());
    }

    #[test]
    fn merge_entry_lww() {
        let store = MeshStore::in_memory().unwrap();

        let old = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("old"),
            timestamp: 100,
            origin: node(1),
        };
        let new = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("new"),
            timestamp: 200,
            origin: node(2),
        };

        assert!(store.merge_entry(old).unwrap());
        // Older entry should be rejected.
        let stale = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("stale"),
            timestamp: 50,
            origin: node(3),
        };
        assert!(!store.merge_entry(stale).unwrap());
        // Newer entry should be accepted.
        assert!(store.merge_entry(new).unwrap());

        let e = store.get("a", "k").unwrap().unwrap();
        assert_eq!(e.value.as_ref(), b"new");
        assert_eq!(e.timestamp, 200);
    }

    /// **LWW tie-break determinism (clock-skew vector).** Consumer
    /// hardware clocks skew, so two nodes can stamp the same key with
    /// the *same* second. `upsert_if_newer` accepts only `ts > existing`
    /// (a later write at an equal timestamp is rejected), so locally
    /// the INCUMBENT wins a tie — deterministic, no last-arrival thrash.
    ///
    /// KNOWN LIMITATION pinned here on purpose: this makes ties
    /// *node-local* deterministic but NOT cross-node convergent. If A
    /// holds X@100 and B holds Y@100 for the same key (equal stamp,
    /// different origins), each rejects the other's value on merge and
    /// they stay diverged — origin is not a tiebreaker. Acceptable
    /// today because every gossiped namespace keys by origin/content
    /// (so two nodes don't co-write one key at one second); if a future
    /// shared-key namespace appears, add a deterministic tiebreaker
    /// (e.g. higher origin NodeId wins) rather than relying on this.
    #[test]
    fn merge_entry_equal_timestamp_keeps_incumbent() {
        let store = MeshStore::in_memory().unwrap();
        let first = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("incumbent"),
            timestamp: 100,
            origin: node(1),
        };
        assert!(store.merge_entry(first).unwrap());

        // Same timestamp, different value+origin — must be rejected,
        // deterministically, regardless of arrival order.
        let tie = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("challenger"),
            timestamp: 100,
            origin: node(2),
        };
        assert!(
            !store.merge_entry(tie).unwrap(),
            "equal-timestamp write must not displace the incumbent"
        );
        assert_eq!(
            store.get("a", "k").unwrap().unwrap().value.as_ref(),
            b"incumbent"
        );
    }

    /// **Wire-layer privacy: private namespaces never leave the node.**
    /// `all_entries_for_gossip` is the ONLY enumeration the gossip
    /// sender (`sovereign-mesh::gossip` Step 4) ships to peers, so it is
    /// the load-bearing chokepoint. Replicate node A → node B exactly
    /// as the sender does and assert no excluded namespace crosses,
    /// while the public namespace does. Mirrors the work-atlas
    /// `cross_node` tests at the layer where the namespace constants
    /// live (`peer_preferences`, `activity-private`).
    #[test]
    fn private_namespaces_never_enter_the_gossip_set() {
        use crate::{ACTIVITY_APP_ID, CONTRIBUTIONS_APP_ID};

        let a = MeshStore::in_memory().unwrap();
        a.set(
            crate::peer_preferences::PEER_PREFERENCES_APP_ID,
            "peer",
            Bytes::from("affinity"),
            node(1),
        )
        .unwrap();
        a.set(ACTIVITY_APP_ID, "usage", Bytes::from("tokens=42"), node(1))
            .unwrap();
        a.set(
            "work-atlas-private",
            "session",
            Bytes::from("scope"),
            node(1),
        )
        .unwrap();
        a.set("notes-private", "n1", Bytes::from("secret note"), node(1))
            .unwrap();
        a.set(CONTRIBUTIONS_APP_ID, "ev1", Bytes::from("served"), node(1))
            .unwrap();

        // The sender ships exactly this set.
        let gossiped = a.all_entries_for_gossip().unwrap();
        for e in &gossiped {
            assert!(
                !crate::peer_preferences::is_gossip_excluded(&e.app_id),
                "excluded namespace '{}' entered the gossip set",
                e.app_id
            );
        }
        assert_eq!(
            gossiped.len(),
            1,
            "only the public contributions entry gossips"
        );
        assert_eq!(gossiped[0].app_id, CONTRIBUTIONS_APP_ID);

        // Replicate into B as the sender→receiver path does.
        let b = MeshStore::in_memory().unwrap();
        for e in gossiped {
            b.merge_entry(e).unwrap();
        }

        // B learned the public entry and NONE of the private ones.
        assert!(b.get(CONTRIBUTIONS_APP_ID, "ev1").unwrap().is_some());
        for (app, key) in [
            (crate::peer_preferences::PEER_PREFERENCES_APP_ID, "peer"),
            (ACTIVITY_APP_ID, "usage"),
            ("work-atlas-private", "session"),
            ("notes-private", "n1"),
        ] {
            assert!(
                b.get(app, key).unwrap().is_none(),
                "private entry {app}/{key} leaked to peer B"
            );
        }
        // And A still has everything locally — excluded ≠ deleted.
        assert!(a.get(ACTIVITY_APP_ID, "usage").unwrap().is_some());
    }

    #[test]
    fn delete_removes_entry() {
        let store = MeshStore::in_memory().unwrap();
        store.set("a", "k", Bytes::from("v"), node(1)).unwrap();
        assert!(store.delete("a", "k").unwrap());
        assert!(store.get("a", "k").unwrap().is_none());
    }

    #[test]
    fn list_keys_scoped_to_app() {
        let store = MeshStore::in_memory().unwrap();
        store.set("app1", "k1", Bytes::from("v"), node(1)).unwrap();
        store.set("app1", "k2", Bytes::from("v"), node(1)).unwrap();
        store.set("app2", "k3", Bytes::from("v"), node(1)).unwrap();

        let keys = store.list_keys("app1").unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"k1".to_string()));
        assert!(keys.contains(&"k2".to_string()));
    }

    #[test]
    fn scan_filters_by_prefix() {
        let store = MeshStore::in_memory().unwrap();
        store
            .set("inf", "model:abc", Bytes::from("a"), node(1))
            .unwrap();
        store
            .set("inf", "model:def", Bytes::from("b"), node(1))
            .unwrap();
        store
            .set("inf", "ledger:xyz", Bytes::from("c"), node(1))
            .unwrap();
        store
            .set("other", "model:abc", Bytes::from("d"), node(1))
            .unwrap();

        let model_entries = store.scan("inf", "model:").unwrap();
        assert_eq!(model_entries.len(), 2);
        assert!(model_entries.iter().all(|e| e.key.starts_with("model:")));

        let ledger_entries = store.scan("inf", "ledger:").unwrap();
        assert_eq!(ledger_entries.len(), 1);
        assert_eq!(ledger_entries[0].key, "ledger:xyz");

        // Prefix not present for app returns empty.
        let empty = store.scan("inf", "nope:").unwrap();
        assert!(empty.is_empty());

        // Scan is scoped to app_id.
        let scoped = store.scan("other", "model:").unwrap();
        assert_eq!(scoped.len(), 1);
    }
}
