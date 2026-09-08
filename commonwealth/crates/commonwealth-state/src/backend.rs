// SPDX-License-Identifier: AGPL-3.0-or-later
//! SQLite backend for the mesh store.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};

/// The store's tables, in ONE spelling.
///
/// `MeshStore::in_memory` used to carry its own copy of the `store` DDL, so
/// the file store and the store every test runs against were two schemas that
/// happened to agree. They stopped agreeing the moment a second table
/// appeared (ARCH §10.6): a `rail_outbox` added here and forgotten there is a
/// crate whose tests all pass and whose deployed writes go nowhere.
pub(crate) const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS store (
    app_id    TEXT NOT NULL,
    key       TEXT NOT NULL,
    value     BLOB NOT NULL,
    timestamp INTEGER NOT NULL,
    origin    BLOB NOT NULL,
    PRIMARY KEY (app_id, key)
);
-- What this node has written and not yet put on the rail. One row per
-- write, drained by the pump in `sovereign-mesh`; see `MeshStore::set`
-- for why an excluded app_id can never appear in it.
CREATE TABLE IF NOT EXISTS rail_outbox (
    id      INTEGER PRIMARY KEY,
    app_id  TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   BLOB,
    t       INTEGER NOT NULL,
    deleted INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS rail_outbox_id ON rail_outbox (id);
";

pub struct SqliteBackend {
    pub(crate) conn: Mutex<Connection>,
}

impl SqliteBackend {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| Error::Backend(format!("failed to open database: {e}")))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Backend(format!("failed to set pragmas: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Backend(format!("failed to initialize schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Get the raw bytes and metadata for (app_id, key). Returns None if not found.
    pub fn get(&self, app_id: &str, key: &str) -> Result<Option<RawEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(
                "SELECT value, timestamp, origin FROM store WHERE app_id = ?1 AND key = ?2",
            )
            .map_err(|e| Error::Backend(format!("prepare failed: {e}")))?;

        let result = stmt.query_row(params![app_id, key], |row| {
            let value: Vec<u8> = row.get(0)?;
            let timestamp: u64 = row.get(1)?;
            let origin: Vec<u8> = row.get(2)?;
            Ok(RawEntry {
                value,
                timestamp,
                origin,
            })
        });

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::Backend(format!("query failed: {e}"))),
        }
    }

    /// Upsert if `timestamp` is newer than existing. Returns true if the row was written.
    ///
    /// Does NOT enqueue: this is the RECEIVE side (`merge_entry`, and through
    /// it `apply_projection`). A row learned from a peer that re-entered the
    /// outbox would be republished by every node that saw it, forever.
    pub fn upsert_if_newer(
        &self,
        app_id: &str,
        key: &str,
        value: &[u8],
        timestamp: u64,
        origin: &[u8],
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        upsert_if_newer_on(&conn, app_id, key, value, timestamp, origin)
    }

    /// Upsert if newer AND enqueue the write for the rail, in ONE
    /// transaction. Returns true if the row was written.
    ///
    /// The two halves are atomic because they are one fact: a row this node
    /// holds and has not told anyone about is a row that never replicates, and
    /// a queued write whose row was rolled back replicates a value nothing
    /// here has.
    ///
    /// The privacy filter lives INSIDE this call rather than at the call site.
    /// `is_gossip_excluded` is the one predicate (ARCH §7.1, §10.6), so an
    /// excluded namespace cannot enter the outbox even by a caller who forgot
    /// — there is no argument that would let it.
    pub fn upsert_if_newer_and_enqueue(
        &self,
        app_id: &str,
        key: &str,
        value: &[u8],
        timestamp: u64,
        origin: &[u8],
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| Error::Backend(format!("begin failed: {e}")))?;
        let written = upsert_if_newer_on(&tx, app_id, key, value, timestamp, origin)?;
        if written {
            enqueue_on(&tx, app_id, key, Some(value), timestamp)?;
        }
        tx.commit()
            .map_err(|e| Error::Backend(format!("commit failed: {e}")))?;
        Ok(written)
    }

    /// Delete a row AND enqueue a tombstone, in ONE transaction. Returns true
    /// if something was deleted.
    ///
    /// A delete of a key this node does not hold enqueues NOTHING. It is not a
    /// fact about the mesh — the key may be live on a peer that simply has not
    /// reached us yet, and a tombstone stamped `now` would take it.
    pub fn delete_and_enqueue(&self, app_id: &str, key: &str, t: u64) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| Error::Backend(format!("begin failed: {e}")))?;
        let deleted = delete_on(&tx, app_id, key)?;
        if deleted {
            enqueue_on(&tx, app_id, key, None, t)?;
        }
        tx.commit()
            .map_err(|e| Error::Backend(format!("commit failed: {e}")))?;
        Ok(deleted)
    }

    /// Delete a row only when what it holds is not NEWER than `t`. Returns
    /// true if something was deleted.
    ///
    /// One statement, so the read and the delete cannot straddle another
    /// writer: a check-then-delete would take a row a concurrent `set` had
    /// just made newer than the tombstone.
    pub fn delete_if_not_newer(&self, app_id: &str, key: &str, t: u64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "DELETE FROM store WHERE app_id = ?1 AND key = ?2 AND timestamp <= ?3",
                params![app_id, key, t],
            )
            .map_err(|e| Error::Backend(format!("tombstone delete failed: {e}")))?;
        Ok(n > 0)
    }

    /// Take up to `limit` queued writes, oldest first. They stay queued until
    /// [`SqliteBackend::outbox_ack`] — the pump appends first and acks after,
    /// so a crash between the two re-sends rather than loses.
    pub fn outbox_take(&self, limit: usize) -> Result<Vec<OutboxRawRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, app_id, key, value, t, deleted FROM rail_outbox \
                 ORDER BY id LIMIT ?1",
            )
            .map_err(|e| Error::Backend(format!("prepare failed: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(OutboxRawRow {
                    id: row.get(0)?,
                    app_id: row.get(1)?,
                    key: row.get(2)?,
                    value: row.get(3)?,
                    t: row.get(4)?,
                    deleted: row.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|e| Error::Backend(format!("query failed: {e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Backend(format!("row error: {e}")))?;
        Ok(rows)
    }

    /// Drop queued writes the pump has placed on the rail. Returns how many
    /// rows went away — a caller acking an id twice gets a smaller number,
    /// never an error.
    pub fn outbox_ack(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| Error::Backend(format!("begin failed: {e}")))?;
        let mut removed = 0usize;
        {
            let mut stmt = tx
                .prepare("DELETE FROM rail_outbox WHERE id = ?1")
                .map_err(|e| Error::Backend(format!("prepare failed: {e}")))?;
            for id in ids {
                removed += stmt
                    .execute(params![id])
                    .map_err(|e| Error::Backend(format!("ack failed: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| Error::Backend(format!("commit failed: {e}")))?;
        Ok(removed)
    }

    /// How many writes are waiting for the pump. For tests and for the
    /// operator surface; the pump itself drains with `outbox_take`.
    pub fn outbox_len(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM rail_outbox", [], |row| row.get(0))
            .map_err(|e| Error::Backend(format!("count failed: {e}")))?;
        Ok(n as usize)
    }

    /// List all keys for an app.
    pub fn list_keys(&self, app_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT key FROM store WHERE app_id = ?1 ORDER BY key")
            .map_err(|e| Error::Backend(format!("prepare failed: {e}")))?;

        let keys = stmt
            .query_map(params![app_id], |row| row.get::<_, String>(0))
            .map_err(|e| Error::Backend(format!("query failed: {e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Backend(format!("row error: {e}")))?;

        Ok(keys)
    }

    /// Return all rows whose key starts with `prefix` for the given app.
    pub fn scan_with_prefix(&self, app_id: &str, prefix: &str) -> Result<Vec<AllRow>> {
        // Escape LIKE special chars in prefix so they are treated literally.
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("{escaped}%");

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(
                "SELECT app_id, key, value, timestamp, origin \
                 FROM store WHERE app_id = ?1 AND key LIKE ?2 ESCAPE '\\'",
            )
            .map_err(|e| Error::Backend(format!("prepare failed: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![app_id, pattern], |row| {
                Ok(AllRow {
                    app_id: row.get(0)?,
                    key: row.get(1)?,
                    value: row.get(2)?,
                    timestamp: row.get(3)?,
                    origin: row.get(4)?,
                })
            })
            .map_err(|e| Error::Backend(format!("query failed: {e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Backend(format!("row error: {e}")))?;

        Ok(rows)
    }

    /// Return all rows for gossip replication.
    pub fn all_rows(&self) -> Result<Vec<AllRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT app_id, key, value, timestamp, origin FROM store")
            .map_err(|e| Error::Backend(format!("prepare failed: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(AllRow {
                    app_id: row.get(0)?,
                    key: row.get(1)?,
                    value: row.get(2)?,
                    timestamp: row.get(3)?,
                    origin: row.get(4)?,
                })
            })
            .map_err(|e| Error::Backend(format!("query failed: {e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Backend(format!("row error: {e}")))?;

        Ok(rows)
    }

    /// Delete all entries older than `cutoff_timestamp`.
    pub fn delete_older_than(&self, cutoff_timestamp: u64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "DELETE FROM store WHERE timestamp < ?1",
                params![cutoff_timestamp],
            )
            .map_err(|e| Error::Backend(format!("gc delete failed: {e}")))?;
        Ok(n)
    }

    /// Same cutoff, restricted to one `app_id`.
    ///
    /// The unrestricted form above is only safe on a store whose every
    /// app treats "old" as "dead". That is not true in general: a
    /// processed-shards dedup marker or a shard-placement record is
    /// deliberately never rewritten, and deleting it re-opens work the
    /// mesh already did. Callers that only need to bound ONE growing
    /// namespace use this and cannot collaterally delete another app's
    /// long-lived state.
    pub fn delete_older_than_in_app(&self, app_id: &str, cutoff_timestamp: u64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "DELETE FROM store WHERE app_id = ?1 AND timestamp < ?2",
                params![app_id, cutoff_timestamp],
            )
            .map_err(|e| Error::Backend(format!("scoped gc delete failed: {e}")))?;
        Ok(n)
    }
}

// ── The statements, once ─────────────────────────────────────
//
// Both forms of each write — the plain one and the enqueueing one — run the
// SAME SQL, because two spellings of "is this newer" is two answers to LWW
// (ARCH §10.6). They take a `&Connection` so a `Transaction` (which derefs to
// one) can pass itself in.

fn upsert_if_newer_on(
    conn: &Connection,
    app_id: &str,
    key: &str,
    value: &[u8],
    timestamp: u64,
    origin: &[u8],
) -> Result<bool> {
    let existing_ts: Option<u64> = conn
        .query_row(
            "SELECT timestamp FROM store WHERE app_id = ?1 AND key = ?2",
            params![app_id, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| Error::Backend(format!("query failed: {e}")))?;

    if let Some(ts) = existing_ts {
        if ts >= timestamp {
            return Ok(false);
        }
    }

    conn.execute(
        "INSERT OR REPLACE INTO store (app_id, key, value, timestamp, origin)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![app_id, key, value, timestamp, origin],
    )
    .map_err(|e| Error::Backend(format!("upsert failed: {e}")))?;

    Ok(true)
}

fn delete_on(conn: &Connection, app_id: &str, key: &str) -> Result<bool> {
    let n = conn
        .execute(
            "DELETE FROM store WHERE app_id = ?1 AND key = ?2",
            params![app_id, key],
        )
        .map_err(|e| Error::Backend(format!("delete failed: {e}")))?;
    Ok(n > 0)
}

/// Queue one write for the rail — unless this namespace never leaves the
/// machine.
///
/// THE SENDER-SIDE PRIVACY GUARD, and it is here rather than at the call site
/// on purpose (ARCH §7.1). `all_entries_for_gossip` was the old chokepoint and
/// worked the same way: one predicate, applied where the bytes leave, so a
/// private namespace is off the wire by construction rather than by every
/// caller remembering. Pinned by
/// `an_excluded_namespace_never_enters_the_outbox`.
fn enqueue_on(
    conn: &Connection,
    app_id: &str,
    key: &str,
    value: Option<&[u8]>,
    t: u64,
) -> Result<()> {
    if crate::peer_preferences::is_gossip_excluded(app_id) {
        tracing::debug!(app_id, key, "mesh_store.outbox_excluded");
        return Ok(());
    }
    conn.execute(
        "INSERT INTO rail_outbox (app_id, key, value, t, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![app_id, key, value, t, i64::from(value.is_none())],
    )
    .map_err(|e| Error::Backend(format!("outbox insert failed: {e}")))?;
    tracing::debug!(
        app_id,
        key,
        t,
        deleted = value.is_none(),
        "mesh_store.outbox_enqueued"
    );
    Ok(())
}

pub struct RawEntry {
    pub value: Vec<u8>,
    pub timestamp: u64,
    pub origin: Vec<u8>,
}

/// One queued write as the table holds it. [`crate::OutboxRow`] is the same
/// row with `Bytes` instead of `Vec<u8>`, for a caller above the backend.
pub struct OutboxRawRow {
    pub id: i64,
    pub app_id: String,
    pub key: String,
    pub value: Option<Vec<u8>>,
    pub t: u64,
    pub deleted: bool,
}

pub struct AllRow {
    pub app_id: String,
    pub key: String,
    pub value: Vec<u8>,
    pub timestamp: u64,
    pub origin: Vec<u8>,
}
