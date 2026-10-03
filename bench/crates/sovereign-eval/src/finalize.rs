// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mark a run as ended.
//!
//! The opencode plugin records `tool.execute.before/after` events into
//! `atos_tool_events` against an open `atos_runs` row. When the operator
//! wakes up, they call `sovereign-eval finalize-run <run-id>` — that
//! sets `atos_runs.ended_at` so downstream analysis treats the run as
//! complete.
//!
//! Read-only against the daemon: just one UPDATE, idempotent.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use std::path::Path;

/// Finalize the run by setting `ended_at` if it isn't already set.
/// Returns whether the row was actually modified (false = was already
/// closed).
pub fn close_run(features_db: &Path, run_id: &str) -> Result<bool> {
    if !features_db.exists() {
        bail!("features.db not found at {}", features_db.display());
    }

    let conn = Connection::open_with_flags(
        features_db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", features_db.display()))?;

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM atos_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .context("checking run existence")?;
    if exists == 0 {
        bail!("run_id `{run_id}` not found in atos_runs");
    }

    let already_closed: Option<i64> = conn
        .query_row(
            "SELECT ended_at FROM atos_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .context("reading ended_at")?;

    if already_closed.is_some() {
        return Ok(false);
    }

    let now = chrono::Utc::now().timestamp();
    let affected = conn
        .execute(
            "UPDATE atos_runs SET ended_at = ?1 WHERE id = ?2 AND ended_at IS NULL",
            params![now, run_id],
        )
        .context("updating ended_at")?;
    Ok(affected > 0)
}
