//! Host-connection + connectivity commands. `add_host_connection` is
//! the pairing entry point: it writes the client-owned HOST_CONNECTION
//! row, stores the token in the keychain, and starts the connectivity
//! monitor for the host.

use std::time::Duration;

use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::connection::{store as conn_store, HostConnection};
use crate::connectivity::ConnectivityMonitor;
use crate::error::{Error, Result};
use crate::state::AppState;

#[tauri::command]
pub async fn add_host_connection(
    app: AppHandle,
    state: State<'_, AppState>,
    display_name: String,
    tailnet_address: String,
    tenant_id: String,
    token: String,
) -> Result<HostConnection> {
    let id = Uuid::new_v4().to_string();
    let now = now_unix();
    let hc = HostConnection {
        id: id.clone(),
        display_name,
        tailnet_address,
        is_default: false, // store::insert promotes the first one
        last_status: "off_tailnet".into(),
        created_at: now,
    };

    // Token → keychain (never SQLite). Connection row + credential
    // metadata → SQLite.
    state.credentials.set_token(&id, &token)?;
    {
        let conn = state.db.lock().map_err(|_| Error::Other("db poisoned".into()))?;
        conn_store::insert(&conn, &hc)?;
        conn.execute(
            "INSERT INTO credential (id, host_connection_id, tenant_id, issued_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            rusqlite::params![Uuid::new_v4().to_string(), id, tenant_id, now],
        )?;
    }

    // Start the connectivity monitor for this host.
    if let Ok(client) = state.active_client() {
        ConnectivityMonitor::spawn(
            app,
            client,
            id.clone(),
            state.db.clone(),
            Duration::from_secs(15),
        );
    }

    state.active_host()
}

#[tauri::command]
pub async fn list_host_connections(state: State<'_, AppState>) -> Result<Vec<HostConnection>> {
    let conn = state.db.lock().map_err(|_| Error::Other("db poisoned".into()))?;
    conn_store::list(&conn)
}

#[tauri::command]
pub async fn set_default_host(state: State<'_, AppState>, id: String) -> Result<()> {
    let conn = state.db.lock().map_err(|_| Error::Other("db poisoned".into()))?;
    conn_store::set_default(&conn, &id)
}

/// Current persisted status for the active host (`reachable` /
/// `host_down` / `off_tailnet`). The monitor keeps it fresh; the UI
/// reads this on cold launch and then follows `connectivity-changed`.
#[tauri::command]
pub async fn get_connectivity(state: State<'_, AppState>) -> Result<String> {
    Ok(state.active_host()?.last_status)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
