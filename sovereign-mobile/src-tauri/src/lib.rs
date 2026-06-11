// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sovereign mobile — thin Tauri client. The Rust core owns transport,
//! security (token in keychain), the SQLite cache, and connectivity;
//! the Svelte frontend renders chat via the shared `@sovereign/chat-ui`.
//!
//! NB: Tauri embeds the frontend (dist/) into this crate at compile time via
//! `generate_context!()`. A FRONTEND-ONLY change won't reach the device unless
//! this crate recompiles — on iOS, `tauri ios build` relinks but doesn't always
//! re-embed a changed dist. Bump this marker (or touch any .rs) to force it.
//! frontend-embed-rev: 7

mod cache;
mod commands;
mod connection;
mod connectivity;
mod error;
mod iroh_bridge;
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
        .plugin(tauri_plugin_deep_link::init())
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
            app.manage(app_state);

            // `sovereign://pair#…` deep links (the desktop pairing QR).
            // Cold launch: the link that opened the app is stashed for
            // the pairing screen to drain on mount. Warm opens: emitted
            // as `pair-link` for the live listener. Both paths also
            // stash, so a link is never lost to a not-yet-mounted UI.
            {
                use tauri::Emitter;
                use tauri_plugin_deep_link::DeepLinkExt;
                let stash = |handle: &tauri::AppHandle, urls: Vec<tauri::Url>| {
                    if let Some(url) = urls.into_iter().next() {
                        let state = handle.state::<AppState>();
                        if let Ok(mut pending) = state.pending_pair_link.lock() {
                            *pending = Some(url.to_string());
                        }
                        let _ = handle.emit("pair-link", url.to_string());
                    }
                };
                let handle = app.handle().clone();
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    stash(&handle, urls);
                }
                let handle2 = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    stash(&handle2, event.urls());
                });
            }

            // Resume the connectivity monitor for the default host so the
            // banner is correct quickly on cold launch. Spawned (not
            // inline) because `active_client` is async now — an
            // iroh-kind default host lazily binds its bridge here.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                if let (Ok(client), Ok(host)) =
                    (state.active_client().await, state.active_host())
                {
                    ConnectivityMonitor::spawn(
                        handle.clone(),
                        client,
                        host.id,
                        state.db.clone(),
                        Duration::from_secs(15),
                    );
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::host::add_host_connection,
            commands::host::list_host_connections,
            commands::host::set_default_host,
            commands::host::remove_host_connection,
            commands::host::get_connectivity,
            commands::host::take_pending_pair_link,
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
