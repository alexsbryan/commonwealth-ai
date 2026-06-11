// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Opaque transport address, interpreted per `endpoint_kind`.
    /// For kind `tailnet`: a MagicDNS name or overlay IP + port
    /// (e.g. `beefymac.tail-scale.ts:8080`). The field name is kept
    /// for column/wire compat.
    pub tailnet_address: String,
    /// How `tailnet_address` is interpreted — see
    /// [`crate::connection::EndpointKind`]. `'tailnet'` today.
    #[serde(default = "default_endpoint_kind")]
    pub endpoint_kind: String,
    pub is_default: bool,
    /// `reachable | host_down | off_tailnet` — last observed; the live
    /// value comes from the connectivity monitor.
    pub last_status: String,
    pub created_at: i64,
}

fn default_endpoint_kind() -> String {
    "tailnet".to_string()
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
           (id, display_name, tailnet_address, endpoint_kind, is_default, last_status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            hc.id,
            hc.display_name,
            hc.tailnet_address,
            hc.endpoint_kind,
            is_default as i64,
            hc.last_status,
            hc.created_at,
        ],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<HostConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, display_name, tailnet_address, endpoint_kind, is_default, last_status, created_at
         FROM host_connection ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(HostConnection {
                id: r.get(0)?,
                display_name: r.get(1)?,
                tailnet_address: r.get(2)?,
                endpoint_kind: r.get(3)?,
                is_default: r.get::<_, i64>(4)? != 0,
                last_status: r.get(5)?,
                created_at: r.get(6)?,
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

/// Whether a host_connection row still exists. The connectivity monitor
/// polls this so it self-terminates once its host has been removed (see
/// `remove_host_connection`), rather than probing a gone address forever.
pub fn exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM host_connection WHERE id = ?1)",
        params![id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::EndpointKind;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::cache::schema::migrate(&conn).unwrap();
        conn
    }

    fn host(id: &str) -> HostConnection {
        HostConnection {
            id: id.into(),
            display_name: "Beefy Mac".into(),
            tailnet_address: "beefymac.tail-scale.ts:8080".into(),
            endpoint_kind: "tailnet".into(),
            is_default: false,
            last_status: "off_tailnet".into(),
            created_at: 1,
        }
    }

    #[test]
    fn crud_round_trips_endpoint_kind() {
        let conn = fresh_db();
        insert(&conn, &host("h1")).unwrap();
        let got = get_default(&conn).unwrap().expect("first host is default");
        assert_eq!(got.endpoint_kind, "tailnet");
        assert_eq!(got.tailnet_address, "beefymac.tail-scale.ts:8080");
    }

    /// A DB created BEFORE the endpoint_kind column existed must
    /// migrate in place: column added, existing rows readable as
    /// kind 'tailnet'.
    #[test]
    fn pre_endpoint_kind_db_migrates_with_tailnet_default() {
        let conn = Connection::open_in_memory().unwrap();
        // Old-shape table + a row, as a pre-seam app build wrote it.
        conn.execute_batch(
            r#"
            CREATE TABLE host_connection (
                id              TEXT PRIMARY KEY,
                display_name    TEXT NOT NULL,
                tailnet_address TEXT NOT NULL,
                is_default      INTEGER NOT NULL DEFAULT 0,
                last_status     TEXT NOT NULL DEFAULT 'off_tailnet',
                created_at      INTEGER NOT NULL
            );
            INSERT INTO host_connection
                (id, display_name, tailnet_address, is_default, last_status, created_at)
            VALUES ('old', 'Old Host', '100.64.0.5:8080', 1, 'reachable', 1);
            "#,
        )
        .unwrap();

        crate::cache::schema::migrate(&conn).unwrap();
        // Idempotency: a second migrate must not error on the
        // now-present column.
        crate::cache::schema::migrate(&conn).unwrap();

        let hosts = list(&conn).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].endpoint_kind, "tailnet");
        assert_eq!(hosts[0].tailnet_address, "100.64.0.5:8080");
    }

    #[test]
    fn endpoint_kind_parse_is_loud_on_unknown() {
        assert_eq!(
            EndpointKind::parse("tailnet").unwrap(),
            EndpointKind::Tailnet
        );
        assert_eq!(EndpointKind::parse("iroh").unwrap(), EndpointKind::Iroh);
        let err = EndpointKind::parse("carrier-pigeon").unwrap_err();
        assert!(
            err.to_string().contains("endpoint_kind"),
            "error must name the field: {err}"
        );
    }

    /// An iroh-kind host row round-trips: the address column holds
    /// the opaque pairing string, the kind tags how to dial it.
    #[test]
    fn iroh_kind_row_round_trips() {
        use commonwealth_transport::iroh::{
            format_dial_string, parse_dial_string, EndpointAddr, SecretKey,
        };
        let conn = fresh_db();
        // A REAL key — arbitrary 32 bytes are not a valid Ed25519
        // point and parse_dial_string rightly rejects them.
        let pk = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(pk)
            .with_relay_url("https://relay.example.com/".parse().unwrap());
        let pairing = format_dial_string(&addr).unwrap();

        let mut h = host("iroh-host");
        h.endpoint_kind = "iroh".into();
        h.tailnet_address = pairing.clone();
        insert(&conn, &h).unwrap();

        let got = get_default(&conn).unwrap().unwrap();
        assert_eq!(got.endpoint_kind, "iroh");
        assert_eq!(got.tailnet_address, pairing);
        // And the stored pairing string is dialable-shaped.
        assert!(parse_dial_string(&got.tailnet_address).is_ok());
    }
}

/// Delete a host connection. If it was the default and other hosts
/// remain, the oldest survivor is promoted so the app always has a
/// well-defined active host (or none, returning the UI to pairing).
/// The caller is responsible for the keychain token + `credential` row.
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let was_default: bool = conn
        .query_row(
            "SELECT is_default FROM host_connection WHERE id = ?1",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false);
    conn.execute("DELETE FROM host_connection WHERE id = ?1", params![id])?;
    if was_default {
        if let Some(next) = list(conn)?.into_iter().next() {
            set_default(conn, &next.id)?;
        }
    }
    Ok(())
}
