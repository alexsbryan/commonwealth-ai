//! HOST_CONNECTION CRUD — the client-owned record (source of truth on
//! device). The token is NOT here (see `keychain.rs`); this row holds
//! only the addressing + status the phone authors.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConnection {
    pub id: String,
    pub display_name: String,
    /// MagicDNS name or overlay IP + port (e.g. `beefymac.tail-scale.ts:8080`).
    pub tailnet_address: String,
    pub is_default: bool,
    /// `reachable | host_down | off_tailnet` — last observed; the live
    /// value comes from the connectivity monitor.
    pub last_status: String,
    pub created_at: i64,
}

pub fn insert(conn: &Connection, hc: &HostConnection) -> Result<()> {
    // First connection becomes the default unless one already exists.
    let has_default: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM host_connection WHERE is_default = 1)",
        [],
        |r| r.get(0),
    )?;
    let is_default = hc.is_default || !has_default;
    if is_default {
        conn.execute("UPDATE host_connection SET is_default = 0", [])?;
    }
    conn.execute(
        "INSERT INTO host_connection
           (id, display_name, tailnet_address, is_default, last_status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            hc.id,
            hc.display_name,
            hc.tailnet_address,
            is_default as i64,
            hc.last_status,
            hc.created_at,
        ],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<HostConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, display_name, tailnet_address, is_default, last_status, created_at
         FROM host_connection ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(HostConnection {
                id: r.get(0)?,
                display_name: r.get(1)?,
                tailnet_address: r.get(2)?,
                is_default: r.get::<_, i64>(3)? != 0,
                last_status: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_default(conn: &Connection) -> Result<Option<HostConnection>> {
    Ok(list(conn)?.into_iter().find(|h| h.is_default))
}

pub fn set_default(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE host_connection SET is_default = 0", [])?;
    conn.execute(
        "UPDATE host_connection SET is_default = 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn set_status(conn: &Connection, id: &str, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE host_connection SET last_status = ?2 WHERE id = ?1",
        params![id, status],
    )?;
    Ok(())
}
