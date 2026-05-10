//! SQLite backend for the mesh store.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

pub struct SqliteBackend {
    pub(crate) conn: Mutex<Connection>,
}

impl SqliteBackend {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| Error::Backend(format!("failed to open database: {e}")))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS store (
                 app_id    TEXT NOT NULL,
                 key       TEXT NOT NULL,
                 value     BLOB NOT NULL,
                 timestamp INTEGER NOT NULL,
                 origin    BLOB NOT NULL,
                 PRIMARY KEY (app_id, key)
             );",
        )
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
            Ok(RawEntry { value, timestamp, origin })
        });

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::Backend(format!("query failed: {e}"))),
        }
    }

    /// Upsert if `timestamp` is newer than existing. Returns true if the row was written.
    pub fn upsert_if_newer(
        &self,
        app_id: &str,
        key: &str,
        value: &[u8],
        timestamp: u64,
        origin: &[u8],
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();

        // Check existing timestamp first.
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

    /// Delete a row. Returns true if something was deleted.
    pub fn delete(&self, app_id: &str, key: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "DELETE FROM store WHERE app_id = ?1 AND key = ?2",
                params![app_id, key],
            )
            .map_err(|e| Error::Backend(format!("delete failed: {e}")))?;
        Ok(n > 0)
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
        let escaped = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
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
}

pub struct RawEntry {
    pub value: Vec<u8>,
    pub timestamp: u64,
    pub origin: Vec<u8>,
}

pub struct AllRow {
    pub app_id: String,
    pub key: String,
    pub value: Vec<u8>,
    pub timestamp: u64,
    pub origin: Vec<u8>,
}
