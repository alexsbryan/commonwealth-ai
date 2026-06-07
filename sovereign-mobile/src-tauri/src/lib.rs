//! Sovereign mobile — thin Tauri client. The Rust core owns transport,
//! security (token in keychain), the SQLite cache, and connectivity;
//! the Svelte frontend renders chat via the shared `@sovereign/chat-ui`.
//!
//! NB: Tauri embeds the frontend (dist/) into this crate at compile time via
//! `generate_context!()`. A FRONTEND-ONLY change won't reach the device unless
//! this crate recompiles — on iOS, `tauri ios build` relinks but doesn't always
//! re-embed a changed dist. Bump this marker (or touch any .rs) to force it.
//! frontend-embed-rev: 4

mod cache;
mod commands;
mod connection;
mod connectivity;
mod error;
mod remote;
mod state;

use std::time::Duration;

use rusqlite::Connection;
use tauri::Manager;

use crate::connection::DevFileCredentialStore;
use crate::connectivity::ConnectivityMonitor;
use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            std::fs::create_dir_all(&data_dir).ok();

            // SQLite cache (client-owned records + cached projections).
            let db = Connection::open(data_dir.join("sovereign-mobile.db"))
                .expect("open cache db");
            cache::schema::migrate(&db).expect("migrate cache schema");

            // CREDENTIAL store. DEV-ONLY file backing for now — replace
            // with the OS keychain plugin before ship (see keychain.rs).
            let credentials = Box::new(DevFileCredentialStore::new(data_dir.join("keychain-dev")));

            let app_state = AppState::new(db, credentials);

            // Resume the connectivity monitor for the default host so the
            // banner is correct immediately on cold launch.
            if let (Ok(client), Ok(host)) = (app_state.active_client(), app_state.active_host()) {
                ConnectivityMonitor::spawn(
                    app.handle().clone(),
                    client,
                    host.id,
                    app_state.db.clone(),
                    Duration::from_secs(15),
                );
            }

            app.manage(app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::host::add_host_connection,
            commands::host::list_host_connections,
            commands::host::set_default_host,
            commands::host::remove_host_connection,
            commands::host::get_connectivity,
            commands::conversation::create_conversation,
            commands::conversation::list_conversations,
            commands::conversation::get_conversation,
            commands::conversation::delete_conversation,
            commands::chat::send_message_stream,
            commands::corpus::list_corpora,
            commands::corpus::resolve_citation,
            commands::corpus::read_citation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running sovereign-mobile");
}
