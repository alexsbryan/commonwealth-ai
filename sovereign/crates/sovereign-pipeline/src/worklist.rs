// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable worklist primitive backed by SQLite.
//!
//! A worklist holds the units of work for one or more pipelines. Each
//! unit is keyed by `(recipe_id, key)` so the same database can host
//! many recipes side-by-side.
//!
//! ## Lifecycle
//!
//! ```text
//!         seed            claim           ack_success
//! pending ─────► pending ───────► claimed ────────────► done
//!                  ▲                 │
//!                  │ ack_failure     │ ack_failure (attempts < max)
//!                  └─────────────────┘
//!                                    │
//!                                    │ ack_failure (attempts >= max)
//!                                    └──────────────────► failed
//! ```
//!
//! Plus `sweep_expired_leases` which atomically returns abandoned
//! `claimed` rows whose lease has expired back to `pending`. This is
//! what makes pause-and-resume safe: a `SIGKILL`'d driver leaves
//! claims dangling; the next driver run sweeps them.
//!
//! All mutations happen inside short transactions, so two drivers
//! racing on the same DB cannot double-claim a unit.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorklistError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, WorklistError>;

/// One unit of work — what the driver claims, executes, and acks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnit {
    pub recipe_id: String,
    pub key: String,
    pub state: State,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub failure_bucket: Option<String>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<i64>,
    pub lease_until: Option<i64>,
    pub completed_at: Option<i64>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Claimed,
    Done,
    Failed,
}

impl State {
    fn as_str(&self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Claimed => "claimed",
            State::Done => "done",
            State::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "pending" => State::Pending,
            "claimed" => State::Claimed,
            "done" => State::Done,
            "failed" => State::Failed,
            _ => State::Pending,
        }
    }
}

/// Summary counts for a recipe — the cheap aggregate `status` reads.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub pending: u64,
    pub claimed: u64,
    pub done: u64,
    pub failed: u64,
    pub total: u64,
    /// Histogram of `failure_bucket` over rows in `failed` state.
    pub failure_buckets: std::collections::BTreeMap<String, u64>,
}

pub struct Worklist {
    conn: Connection,
}

impl Worklist {
    /// Open or create the worklist DB. Schema is created idempotently.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    /// In-memory worklist — for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS work_units (
                recipe_id       TEXT    NOT NULL,
                key             TEXT    NOT NULL,
                state           TEXT    NOT NULL DEFAULT 'pending',
                attempts        INTEGER NOT NULL DEFAULT 0,
                last_error      TEXT,
                failure_bucket  TEXT,
                claimed_by      TEXT,
                claimed_at      INTEGER,
                lease_until     INTEGER,
                completed_at    INTEGER,
                payload         TEXT    NOT NULL DEFAULT '{}',
                PRIMARY KEY (recipe_id, key)
            );
            CREATE INDEX IF NOT EXISTS work_units_state_idx
                ON work_units(recipe_id, state);
            CREATE INDEX IF NOT EXISTS work_units_lease_idx
                ON work_units(recipe_id, state, lease_until);
            ",
        )?;
        Ok(())
    }

    /// Insert pending rows for the given keys. Existing rows are left
    /// alone — re-seeding is idempotent and safe to call on every
    /// driver start. Returns the number of newly-inserted rows.
    pub fn seed<I, S>(&mut self, recipe_id: &str, keys: I) -> Result<u64>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let tx = self.conn.transaction()?;
        let mut inserted: u64 = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO work_units (recipe_id, key, state)
                 VALUES (?1, ?2, 'pending')",
            )?;
            for k in keys {
                inserted += stmt.execute(params![recipe_id, k.as_ref()])? as u64;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Atomically claim up to `n` pending units for `recipe_id`.
    /// Returns the claimed keys (in arbitrary order). Each row's
    /// lease expires at `now + lease_secs`.
    ///
    /// The `claimed_by` field is a free-form driver identifier — the
    /// caller typically passes a UUID per driver instance.
    pub fn claim(
        &mut self,
        recipe_id: &str,
        claimed_by: &str,
        n: u32,
        lease_secs: u32,
    ) -> Result<Vec<String>> {
        if n == 0 {
            return Ok(vec![]);
        }
        let now = unix_now();
        let lease_until = now + lease_secs as i64;
        let tx = self.conn.transaction()?;
        let keys: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT key FROM work_units
                 WHERE recipe_id = ?1 AND state = 'pending'
                 ORDER BY attempts ASC, key ASC
                 LIMIT ?2",
            )?;
            let collected = stmt
                .query_map(params![recipe_id, n as i64], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            collected
        };
        if keys.is_empty() {
            tx.commit()?;
            return Ok(vec![]);
        }
        {
            let mut upd = tx.prepare(
                "UPDATE work_units
                 SET state = 'claimed',
                     attempts = attempts + 1,
                     claimed_by = ?3,
                     claimed_at = ?4,
                     lease_until = ?5
                 WHERE recipe_id = ?1 AND key = ?2 AND state = 'pending'",
            )?;
            for k in &keys {
                upd.execute(params![recipe_id, k, claimed_by, now, lease_until])?;
            }
        }
        tx.commit()?;
        Ok(keys)
    }

    /// Mark a claimed unit as successfully completed.
    pub fn ack_success(&mut self, recipe_id: &str, key: &str) -> Result<()> {
        let now = unix_now();
        self.conn.execute(
            "UPDATE work_units
             SET state = 'done',
                 last_error = NULL,
                 failure_bucket = NULL,
                 claimed_by = NULL,
                 claimed_at = NULL,
                 lease_until = NULL,
                 completed_at = ?3
             WHERE recipe_id = ?1 AND key = ?2",
            params![recipe_id, key, now],
        )?;
        Ok(())
    }

    /// Mark a claimed unit as failed. If `attempts < max_attempts`,
    /// the row goes back to `pending` for retry. Otherwise it lands
    /// in `failed` with the bucket recorded.
    pub fn ack_failure(
        &mut self,
        recipe_id: &str,
        key: &str,
        error: &str,
        bucket: &str,
        max_attempts: u32,
    ) -> Result<State> {
        let tx = self.conn.transaction()?;
        let attempts: u32 = tx
            .query_row(
                "SELECT attempts FROM work_units WHERE recipe_id = ?1 AND key = ?2",
                params![recipe_id, key],
                |r| r.get::<_, i64>(0).map(|v| v as u32),
            )
            .optional()?
            .unwrap_or(0);
        let next_state = if attempts >= max_attempts {
            State::Failed
        } else {
            State::Pending
        };
        tx.execute(
            "UPDATE work_units
             SET state = ?3,
                 last_error = ?4,
                 failure_bucket = ?5,
                 claimed_by = NULL,
                 claimed_at = NULL,
                 lease_until = NULL
             WHERE recipe_id = ?1 AND key = ?2",
            params![recipe_id, key, next_state.as_str(), error, bucket],
        )?;
        tx.commit()?;
        Ok(next_state)
    }

    /// Return abandoned `claimed` rows whose lease has expired back
    /// to `pending`. Run at driver startup. Returns count.
    pub fn sweep_expired_leases(&mut self, recipe_id: &str) -> Result<u64> {
        let now = unix_now();
        let affected = self.conn.execute(
            "UPDATE work_units
             SET state = 'pending',
                 claimed_by = NULL,
                 claimed_at = NULL,
                 lease_until = NULL
             WHERE recipe_id = ?1
               AND state = 'claimed'
               AND lease_until IS NOT NULL
               AND lease_until < ?2",
            params![recipe_id, now],
        )?;
        Ok(affected as u64)
    }

    /// Aggregate counts + failure-bucket histogram.
    pub fn stats(&self, recipe_id: &str) -> Result<Stats> {
        let mut stats = Stats::default();
        let mut stmt = self.conn.prepare(
            "SELECT state, COUNT(*) FROM work_units
             WHERE recipe_id = ?1 GROUP BY state",
        )?;
        let rows = stmt.query_map(params![recipe_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (state, count) = row?;
            let count = count as u64;
            stats.total += count;
            match State::parse(&state) {
                State::Pending => stats.pending = count,
                State::Claimed => stats.claimed = count,
                State::Done => stats.done = count,
                State::Failed => stats.failed = count,
            }
        }
        let mut bstmt = self.conn.prepare(
            "SELECT COALESCE(failure_bucket, 'unknown'), COUNT(*)
             FROM work_units
             WHERE recipe_id = ?1 AND state = 'failed'
             GROUP BY failure_bucket",
        )?;
        let brows = bstmt.query_map(params![recipe_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in brows {
            let (bucket, count) = row?;
            stats.failure_buckets.insert(bucket, count as u64);
        }
        Ok(stats)
    }

    /// List every `recipe_id` that has at least one row.
    pub fn list_recipe_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT recipe_id FROM work_units ORDER BY 1")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Count rows completed strictly after `since_unix`. Drives the
    /// throughput readout.
    pub fn completed_since(&self, recipe_id: &str, since_unix: i64) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM work_units
             WHERE recipe_id = ?1 AND state = 'done' AND completed_at > ?2",
            params![recipe_id, since_unix],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_three(wl: &mut Worklist, recipe: &str) {
        let n = wl.seed(recipe, ["a", "b", "c"]).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn seed_is_idempotent() {
        let mut wl = Worklist::open_in_memory().unwrap();
        assert_eq!(wl.seed("r", ["a", "b"]).unwrap(), 2);
        assert_eq!(wl.seed("r", ["a", "b", "c"]).unwrap(), 1);
        let stats = wl.stats("r").unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.pending, 3);
    }

    #[test]
    fn claim_returns_pending_and_advances_state() {
        let mut wl = Worklist::open_in_memory().unwrap();
        seed_three(&mut wl, "r");
        let claimed = wl.claim("r", "drv-1", 2, 60).unwrap();
        assert_eq!(claimed.len(), 2);
        let stats = wl.stats("r").unwrap();
        assert_eq!(stats.claimed, 2);
        assert_eq!(stats.pending, 1);
    }

    #[test]
    fn claim_respects_zero_request() {
        let mut wl = Worklist::open_in_memory().unwrap();
        seed_three(&mut wl, "r");
        assert!(wl.claim("r", "drv", 0, 60).unwrap().is_empty());
    }

    #[test]
    fn claim_drains_then_stops() {
        let mut wl = Worklist::open_in_memory().unwrap();
        seed_three(&mut wl, "r");
        let a = wl.claim("r", "drv", 10, 60).unwrap();
        assert_eq!(a.len(), 3);
        let b = wl.claim("r", "drv", 10, 60).unwrap();
        assert!(b.is_empty());
    }

    #[test]
    fn ack_success_moves_to_done() {
        let mut wl = Worklist::open_in_memory().unwrap();
        seed_three(&mut wl, "r");
        let claimed = wl.claim("r", "drv", 3, 60).unwrap();
        for k in &claimed {
            wl.ack_success("r", k).unwrap();
        }
        let stats = wl.stats("r").unwrap();
        assert_eq!(stats.done, 3);
        assert_eq!(stats.claimed, 0);
    }

    #[test]
    fn ack_failure_retries_under_max_attempts() {
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["x"]).unwrap();
        let claimed = wl.claim("r", "drv", 1, 60).unwrap();
        let next = wl
            .ack_failure("r", &claimed[0], "boom", "timeout", 3)
            .unwrap();
        assert_eq!(next, State::Pending);
        let stats = wl.stats("r").unwrap();
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.failed, 0);
    }

    #[test]
    fn ack_failure_lands_in_failed_at_max() {
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["x"]).unwrap();
        // First attempt fails — attempts becomes 1.
        let a = wl.claim("r", "drv", 1, 60).unwrap();
        let s = wl.ack_failure("r", &a[0], "boom", "timeout", 1).unwrap();
        assert_eq!(s, State::Failed);
        let stats = wl.stats("r").unwrap();
        assert_eq!(stats.failed, 1);
        assert_eq!(*stats.failure_buckets.get("timeout").unwrap(), 1);
    }

    #[test]
    fn sweep_returns_expired_leases_to_pending() {
        // Use a lease of 0s so we expire instantly.
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["x"]).unwrap();
        let _ = wl.claim("r", "drv", 1, 0).unwrap();
        // Lease is `now + 0`; sweep checks `lease_until < now`, so
        // we need to wait one second before the sweep sees it as
        // expired. Fake-time the row instead to keep tests fast.
        wl.conn
            .execute(
                "UPDATE work_units SET lease_until = 1 WHERE recipe_id = 'r'",
                [],
            )
            .unwrap();
        let n = wl.sweep_expired_leases("r").unwrap();
        assert_eq!(n, 1);
        let stats = wl.stats("r").unwrap();
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.claimed, 0);
    }

    #[test]
    fn sweep_leaves_fresh_claims_alone() {
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["x"]).unwrap();
        let _ = wl.claim("r", "drv", 1, 600).unwrap();
        let n = wl.sweep_expired_leases("r").unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn claim_uses_lowest_attempts_first() {
        // After a retry, the retried row should sort *after* fresh
        // pending rows — avoids starving fresh work with a hot fail.
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["a", "b"]).unwrap();
        let first = wl.claim("r", "drv", 1, 60).unwrap();
        wl.ack_failure("r", &first[0], "x", "timeout", 5).unwrap();
        let next = wl.claim("r", "drv", 1, 60).unwrap();
        // Whichever row was NOT the first claim should come next.
        assert_ne!(next[0], first[0]);
    }
}
