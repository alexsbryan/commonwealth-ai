//! Connectivity monitor: classifies the host link and emits
//! `connectivity-changed` on transitions. The Svelte side mirrors this
//! read-only (it never decides reachability itself — no split-brain).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::connection::store;
use crate::connectivity::reachability;
use crate::error::Error;
use crate::remote::ApiClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    /// No tailnet route — fail closed; no alternate path is attempted.
    OffTailnet,
    /// Tailnet up, host not answering.
    HostDown,
    /// Host answered `503 + Retry-After` — busy, not down.
    HostBusy,
    /// Authenticated probe returned 2xx.
    Reachable,
}

impl ConnState {
    /// The string persisted into `host_connection.last_status`.
    pub fn as_status(self) -> &'static str {
        match self {
            ConnState::OffTailnet => "off_tailnet",
            ConnState::HostDown => "host_down",
            // last_status only models the spec's three; busy collapses to
            // reachable for the persisted addressing record.
            ConnState::HostBusy | ConnState::Reachable => "reachable",
        }
    }
}

#[derive(Serialize, Clone)]
struct ConnectivityChanged {
    host_connection_id: String,
    state: ConnState,
    /// Set when `state == HostBusy`, mirroring REST `Retry-After`.
    retry_after_secs: Option<u64>,
}

/// Classify the link once. Uses `list_conversations` as the probe (it
/// exercises auth + a real route).
pub async fn classify(client: &ApiClient) -> (ConnState, Option<u64>) {
    if !reachability::tailnet_present() {
        return (ConnState::OffTailnet, None);
    }
    match client.list_conversations().await {
        Ok(_) => (ConnState::Reachable, None),
        // Host responded → reachable, even if the token is stale or it's
        // busy. Busy carries its Retry-After for the countdown UI.
        Err(Error::HostBusy { retry_after_secs }) => (ConnState::HostBusy, Some(retry_after_secs)),
        Err(Error::Unauthenticated) => (ConnState::Reachable, None),
        Err(Error::OffTailnet) => (ConnState::OffTailnet, None),
        Err(_) => (ConnState::HostDown, None),
    }
}

pub struct ConnectivityMonitor;

impl ConnectivityMonitor {
    /// Spawn a background poll loop. Emits `connectivity-changed` only on
    /// transitions and writes `host_connection.last_status` so a cold
    /// launch shows the right banner immediately.
    pub fn spawn(
        app: AppHandle,
        client: ApiClient,
        host_connection_id: String,
        db: Arc<Mutex<Connection>>,
        interval: Duration,
    ) {
        // `tauri::async_runtime::spawn` (not bare `tokio::spawn`): on iOS the
        // Tauri `setup()` closure runs inside the app-delegate's
        // `did_finish_launching` with no ambient Tokio runtime on that
        // thread, so `tokio::spawn` panics ("no reactor running") — and
        // across the ObjC FFI boundary that aborts the process instead of
        // unwinding. Tauri's runtime handle works from any context.
        tauri::async_runtime::spawn(async move {
            let mut last: Option<ConnState> = None;
            loop {
                let (state, retry) = classify(&client).await;
                if last != Some(state) {
                    last = Some(state);
                    if let Ok(conn) = db.lock() {
                        let _ = store::set_status(&conn, &host_connection_id, state.as_status());
                    }
                    let _ = app.emit(
                        "connectivity-changed",
                        ConnectivityChanged {
                            host_connection_id: host_connection_id.clone(),
                            state,
                            retry_after_secs: retry,
                        },
                    );
                }
                tokio::time::sleep(interval).await;
            }
        });
    }
}
