mod approval;
mod commands;
mod mesh_commands;
mod state;
mod tray;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use crate::approval::TauriApprovalChannel;
use crate::state::AppState;

#[cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sovereign_desktop=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            // Listen for sovereign:// deep links and forward them to the frontend.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        let url_str = url.to_string();
                        tracing::info!("Deep link received: {url_str}");
                        let _ = handle.emit("deep-link-received", url_str);
                    }
                });
            }

            let handle = app.handle().clone();

            // Create approval channel with app handle for event emission.
            let approval = Arc::new(TauriApprovalChannel::new(handle.clone()));

            // Create app state (loads config, no Runtime yet).
            let app_state = AppState::new(Arc::clone(&approval));
            let app_state = Arc::new(app_state);
            app.manage(app_state.clone());

            // Set up system tray.
            if let Err(e) = tray::setup_tray(app) {
                tracing::warn!("Failed to set up system tray: {e}");
            }

            // Bootstrap Runtime asynchronously if setup is complete.
            let state_clone = Arc::clone(&app_state);
            let handle_clone = handle.clone();
            tauri::async_runtime::spawn(async move {
                let config = state_clone.config.read().await;
                let setup_done = config.setup_complete;
                let model_exists = config.model_path.exists();
                drop(config);

                if !setup_done || !model_exists {
                    if !setup_done {
                        tracing::info!("First launch — waiting for setup wizard");
                    } else {
                        tracing::warn!("Model not found — clearing stale path and returning to setup wizard");
                        // Clear the stale model path so the wizard starts fresh.
                        let mut config = state_clone.config.write().await;
                        config.setup_complete = false;
                        config.model_path = std::path::PathBuf::new();
                        let _ = config.save();
                    }
                    let _ = handle_clone.emit("setup-required", ());
                    return;
                }

                match state::bootstrap(&state_clone).await {
                    Ok(()) => {
                        tracing::info!("Backend ready");
                        let _ = handle_clone.emit("backend-ready", ());
                    }
                    Err(e) => {
                        tracing::error!("Bootstrap failed: {e}");
                        let _ = handle_clone.emit(
                            "backend-error",
                            approval::ErrorPayload { message: e },
                        );
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::send_message,
            commands::send_message_stream,
            commands::create_conversation,
            commands::list_conversations,
            commands::get_conversation,
            commands::delete_conversation,
            commands::search_messages,
            commands::submit_approval,
            commands::submit_input,
            commands::list_skills,
            commands::toggle_skill,
            commands::get_config,
            commands::save_config,
            commands::is_setup_complete,
            commands::complete_setup,
            commands::detect_hardware,
            commands::search_web,
            commands::scan_for_models,
            commands::download_model,
            commands::list_corpora,
            commands::install_corpus,
            commands::remove_corpus,
            commands::get_corpus_progress,
            commands::get_corpus_health,
            commands::recipe_validate,
            commands::recipe_test,
            mesh_commands::mesh_create,
            mesh_commands::mesh_join,
            mesh_commands::mesh_preview_join_link,
            mesh_commands::mesh_get_state,
            mesh_commands::mesh_is_running,
            mesh_commands::mesh_leave,
        ])
        .run(tauri::generate_context!())
        .expect("error running Sovereign");
}
