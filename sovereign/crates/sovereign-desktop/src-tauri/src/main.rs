mod approval;
mod bootstrap;
mod commands;
mod insight_commands;
mod mesh_commands;
mod state;
mod tray;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use crate::approval::TauriApprovalChannel;
use crate::state::AppState;

#[cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    // Default filter gives glass-box visibility into every inference path.
    // Set RUST_LOG to override (e.g. RUST_LOG=sovereign_core=debug for more detail).
    //
    // Levels chosen so a standard run emits:
    //   - turn-level events (info): routing decisions, dispatch, inference calls,
    //     document operations, slot loads/unloads
    //   - per-inference details (debug on core/tools/inference): prompt sizes,
    //     chunk counts, latencies, topic context updates
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Mesh-related crates default to info so join/discovery
                // logs surface without the user setting RUST_LOG. The
                // alternative (silent tracing calls) made every mesh
                // failure look identical from the user's perspective.
                "sovereign_desktop=info,\
                 sovereign_core=debug,\
                 sovereign_tools=debug,\
                 sovereign_inference=debug,\
                 sovereign_mesh=info,\
                 commonwealth_discovery=info,\
                 commonwealth_api=info,\
                 corpus_engine=debug"
                    .into()
            }),
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

            // Probe `:9741` to decide whether a CLI-started daemon is
            // already running. If so, we skip starting our own
            // `EmbeddedDaemon` (same port, same mesh.json — collision
            // inevitable) and route inference + mesh mutations over
            // HTTP instead. `detect()` is a ≤4s worst-case probe so
            // it's fine to block app setup on it.
            let bootstrap_mode =
                tauri::async_runtime::block_on(bootstrap::detect());
            tracing::info!(?bootstrap_mode, "bootstrap mode resolved");

            // Create app state (loads config, no Runtime yet).
            let app_state = AppState::new_with_mode(
                Arc::clone(&approval),
                bootstrap_mode,
            );
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
            commands::rename_conversation,
            commands::search_messages,
            commands::submit_approval,
            commands::submit_input,
            commands::submit_information_response,
            commands::list_skills,
            commands::toggle_skill,
            commands::get_config,
            commands::save_config,
            commands::is_setup_complete,
            commands::complete_setup,
            commands::detect_hardware,
            commands::detect_bootstrap,
            commands::search_web,
            commands::scan_for_models,
            commands::download_model,
            commands::list_corpora,
            commands::install_corpus,
            commands::remove_corpus,
            commands::build_corpus_index,
            commands::diagnose_corpus,
            commands::ingest_document,
            commands::upload_document_asset,
            commands::ask_document,
            commands::get_document_asset,
            commands::rebuild_document_skeleton,
            commands::list_document_assets,
            commands::list_legacy_documents,
            commands::promote_legacy_document,
            commands::delete_document_asset,
            commands::get_corpus_progress,
            commands::get_corpus_health,
            commands::retry_enrichment_failures,
            commands::recipe_validate,
            commands::recipe_test,
            mesh_commands::mesh_create,
            mesh_commands::mesh_join,
            mesh_commands::mesh_preview_join_link,
            mesh_commands::mesh_get_state,
            mesh_commands::mesh_is_running,
            mesh_commands::mesh_leave,
            mesh_commands::mesh_diagnostics,
            insight_commands::clip_insight,
            insight_commands::list_insights,
            insight_commands::search_insights,
            insight_commands::delete_insight,
            insight_commands::get_sink_status,
            insight_commands::explore_insights,
        ])
        .run(tauri::generate_context!())
        .expect("error running Sovereign");
}
