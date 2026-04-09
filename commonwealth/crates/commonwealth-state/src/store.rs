//! MeshStore — the distributed key-value store for mesh apps.
//!
//! Each entry is scoped to an `app_id` + `key`. Conflict resolution is LWW
//! (last-write-wins) using a Unix-second `timestamp`. The underlying storage
//! is SQLite (WAL mode) via `SqliteBackend`. Entries are replicated across
//! nodes through the gossip layer.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
        Ok(Self { backend: Arc::new(backend) })
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
            backend: Arc::new(crate::backend::SqliteBackend { conn: Mutex::new(conn) }),
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

    /// Return all entries for gossip broadcast.
    pub fn all_entries_for_gossip(&self) -> Result<Vec<StoreEntry>> {
        let rows = self.backend.all_rows()?;
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
    pub fn gc(&self, ttl_seconds: u64) -> Result<usize> {
        let cutoff = now_secs().saturating_sub(ttl_seconds);
        self.backend.delete_older_than(cutoff)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn node_id_from_bytes(bytes: &[u8]) -> Result<NodeId> {
    if bytes.len() != 16 {
        return Err(Error::NodeId(format!(
            "expected 16 bytes for NodeId, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(bytes);
    Ok(NodeId::from_u128(u128::from_le_bytes(arr)))
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
    }

    #[test]
    fn get_missing_returns_none() {
        let store = MeshStore::in_memory().unwrap();
        assert!(store.get("myapp", "nope").unwrap().is_none());
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
}
