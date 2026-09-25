// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure-Rust backend: what `MeshStore::in_memory` stands on.
//!
//! The same surface as the SQLite backend, statement for statement, so the
//! two answer every question alike (ARCH §10.6). Three SQLite behaviours are
//! reproduced on purpose rather than improved on, because a caller already
//! depends on each:
//!
//! - a prefix scan is SQLite's default `LIKE`: case-insensitive for ASCII,
//!   exact for every other byte;
//! - rows come back in key order (BINARY collation = byte order), which is
//!   what the `(app_id, key)` primary key hands the scan;
//! - an outbox id is `max(id) + 1`, so an emptied outbox starts again at 1 —
//!   `INTEGER PRIMARY KEY` without `AUTOINCREMENT`.
//!
//! One mutex over both tables is what makes the write-and-enqueue pairs one
//! transaction here.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::{AllRow, OutboxRawRow, RawEntry};
use crate::error::Result;

struct Row {
    value: Vec<u8>,
    timestamp: u64,
    origin: Vec<u8>,
}

#[derive(Default)]
struct Tables {
    /// `store`, keyed `(app_id, key)` like its primary key.
    store: BTreeMap<(String, String), Row>,
    /// `rail_outbox`, keyed by id.
    outbox: BTreeMap<i64, OutboxRawRow>,
}

#[derive(Default)]
pub struct MemoryBackend {
    tables: Mutex<Tables>,
}

fn k(app_id: &str, key: &str) -> (String, String) {
    (app_id.to_string(), key.to_string())
}

impl Tables {
    /// `upsert_if_newer_on`, with the same tie rule — see its comment in the
    /// SQLite backend for why an equal timestamp has three shapes.
    fn upsert_if_newer(
        &mut self,
        app_id: &str,
        key: &str,
        value: &[u8],
        timestamp: u64,
        origin: &[u8],
    ) -> bool {
        if let Some(held) = self.store.get(&k(app_id, key)) {
            if held.timestamp > timestamp
                || (held.timestamp == timestamp && (held.origin != origin || held.value == value))
            {
                return false;
            }
        }
        self.store.insert(
            k(app_id, key),
            Row {
                value: value.to_vec(),
                timestamp,
                origin: origin.to_vec(),
            },
        );
        true
    }

    /// `enqueue_on`, with the same sender-side privacy guard.
    fn enqueue(&mut self, app_id: &str, key: &str, value: Option<&[u8]>, t: u64) {
        if crate::peer_preferences::is_gossip_excluded(app_id) {
            tracing::debug!(app_id, key, "mesh_store.outbox_excluded");
            return;
        }
        let id = self.outbox.keys().next_back().map_or(1, |max| max + 1);
        self.outbox.insert(
            id,
            OutboxRawRow {
                id,
                app_id: app_id.to_string(),
                key: key.to_string(),
                value: value.map(<[u8]>::to_vec),
                t,
            },
        );
        tracing::debug!(
            app_id,
            key,
            t,
            deleted = value.is_none(),
            "mesh_store.outbox_enqueued"
        );
    }
}

/// SQLite's default `LIKE 'prefix%'`: ASCII letters fold, other bytes match
/// exactly.
fn like_prefix(key: &str, prefix: &str) -> bool {
    let (key, prefix) = (key.as_bytes(), prefix.as_bytes());
    key.len() >= prefix.len() && key[..prefix.len()].eq_ignore_ascii_case(prefix)
}

impl MemoryBackend {
    fn lock(&self) -> std::sync::MutexGuard<'_, Tables> {
        self.tables.lock().unwrap()
    }

    pub fn get(&self, app_id: &str, key: &str) -> Result<Option<RawEntry>> {
        Ok(self.lock().store.get(&k(app_id, key)).map(|r| RawEntry {
            value: r.value.clone(),
            timestamp: r.timestamp,
            origin: r.origin.clone(),
        }))
    }

    pub fn upsert_if_newer(
        &self,
        app_id: &str,
        key: &str,
        value: &[u8],
        timestamp: u64,
        origin: &[u8],
    ) -> Result<bool> {
        Ok(self
            .lock()
            .upsert_if_newer(app_id, key, value, timestamp, origin))
    }

    pub fn upsert_if_newer_and_enqueue(
        &self,
        app_id: &str,
        key: &str,
        value: &[u8],
        timestamp: u64,
        origin: &[u8],
    ) -> Result<bool> {
        let mut t = self.lock();
        let written = t.upsert_if_newer(app_id, key, value, timestamp, origin);
        if written {
            t.enqueue(app_id, key, Some(value), timestamp);
        }
        Ok(written)
    }

    pub fn delete_and_enqueue(&self, app_id: &str, key: &str, t: u64) -> Result<bool> {
        let mut tables = self.lock();
        let deleted = tables.store.remove(&k(app_id, key)).is_some();
        if deleted {
            tables.enqueue(app_id, key, None, t);
        }
        Ok(deleted)
    }

    pub fn delete_if_not_newer(&self, app_id: &str, key: &str, t: u64) -> Result<bool> {
        let mut tables = self.lock();
        let hit = matches!(tables.store.get(&k(app_id, key)), Some(r) if r.timestamp <= t);
        if hit {
            tables.store.remove(&k(app_id, key));
        }
        Ok(hit)
    }

    pub fn keys_with_origin(&self, app_id: &str, origin: &[u8]) -> Result<Vec<String>> {
        Ok(self
            .lock()
            .store
            .iter()
            .filter(|((a, _), r)| a == app_id && r.origin == origin)
            .map(|((_, key), _)| key.clone())
            .collect())
    }

    pub fn delete_of_origin(&self, app_id: &str, key: &str, origin: &[u8]) -> Result<bool> {
        let mut tables = self.lock();
        let hit = matches!(tables.store.get(&k(app_id, key)), Some(r) if r.origin == origin);
        if hit {
            tables.store.remove(&k(app_id, key));
        }
        Ok(hit)
    }

    pub fn outbox_take(&self, limit: usize) -> Result<Vec<OutboxRawRow>> {
        Ok(self
            .lock()
            .outbox
            .values()
            .take(limit)
            .map(|r| OutboxRawRow {
                id: r.id,
                app_id: r.app_id.clone(),
                key: r.key.clone(),
                value: r.value.clone(),
                t: r.t,
            })
            .collect())
    }

    pub fn outbox_ack(&self, ids: &[i64]) -> Result<usize> {
        let mut tables = self.lock();
        Ok(ids
            .iter()
            .filter(|id| tables.outbox.remove(id).is_some())
            .count())
    }

    pub fn outbox_len(&self) -> Result<usize> {
        Ok(self.lock().outbox.len())
    }

    pub fn scan_with_prefix(&self, app_id: &str, prefix: &str) -> Result<Vec<AllRow>> {
        Ok(self
            .lock()
            .store
            .iter()
            .filter(|((a, key), _)| a == app_id && like_prefix(key, prefix))
            .map(|((a, key), r)| AllRow {
                app_id: a.clone(),
                key: key.clone(),
                value: r.value.clone(),
                timestamp: r.timestamp,
                origin: r.origin.clone(),
            })
            .collect())
    }

    pub fn delete_older_than(&self, cutoff_timestamp: u64) -> Result<usize> {
        let mut tables = self.lock();
        let before = tables.store.len();
        tables.store.retain(|_, r| r.timestamp >= cutoff_timestamp);
        Ok(before - tables.store.len())
    }

    pub fn delete_older_than_in_app(&self, app_id: &str, cutoff_timestamp: u64) -> Result<usize> {
        let mut tables = self.lock();
        let before = tables.store.len();
        tables
            .store
            .retain(|(a, _), r| a != app_id || r.timestamp >= cutoff_timestamp);
        Ok(before - tables.store.len())
    }
}

#[cfg(test)]
mod tests {
    use super::like_prefix;

    /// The one SQLite behaviour a reader would "fix": `LIKE` folds ASCII case.
    #[test]
    fn prefix_match_is_sqlite_like() {
        assert!(like_prefix("Peer/a", "peer/"));
        assert!(like_prefix("peer_x", "peer_"));
        assert!(!like_prefix("peerx", "peer_"));
        assert!(!like_prefix("pé", "pÉ"));
        assert!(like_prefix("anything", ""));
    }
}
