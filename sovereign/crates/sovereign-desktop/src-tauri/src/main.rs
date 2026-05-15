mod approval;
mod atlas_commands;
mod bootstrap;
mod commands;
mod crash_bundle;
mod enrich_commands;
mod friendly_names;
mod insight_commands;
mod local_corpus_commands;
mod watched_folder_commands;
mod mesh_commands;
mod recipe_author_commands;
mod recipe_commands;
mod routing_events;
mod setup_flow;
mod smoketest;
mod state;
mod supervisor;
mod supervisor_setup;
mod tray;

use std::process::ExitCode;
use std::sync::Arc;

use tauri::{Emitter, Manager};

use crate::approval::TauriApprovalChannel;
use crate::state::AppState;

#[cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() -> ExitCode {
    // Smoketest mode: when invoked with `--smoketest --model <gguf>
    // [--gpu-layers N] [--ctx M]`, skip Tauri entirely and run a
    // minimal load + 1-token decode, then exit. The parent desktop
    // process spawns this mode in a subprocess to detect ggml
    // backend crashes (e.g., the Gemma 4 Metal SIGSEGV) before
    // loading models in the user-facing slot. See `smoketest.rs`.
    let argv: Vec<String> = std::env::args().collect();
    if let Some(code) = smoketest::detect_and_run(&argv) {
        return code;
    }

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
        // Eagerly warm the primary chat slot whenever the user
        // foregrounds the app. The 35B Q6 we ship by default takes
        // 10–20s to load on Metal, much longer on CPU; without
        // prewarm, every "user came back to the app and asked a
        // question" round-trip pays that load tax in the
        // foreground. Firing on `Focused(true)` covers most of
        // the typing window so the slot is hot by send.
        // Idempotent: a warm slot returns immediately. Spawned
        // inside `warmup_primary_slot` itself so this handler
        // returns without blocking the UI thread.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(true) = event {
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    if let Some(state) =
                        app.try_state::<std::sync::Arc<state::AppState>>()
                    {
                        let provider = {
                            let guard = state.inference.read().await;
                            guard.as_ref().map(std::sync::Arc::clone)
                        };
                        if let Some(provider) = provider {
                            let started = std::time::Instant::now();
                            match provider.warmup_primary().await {
                                Ok(()) => tracing::info!(
                                    latency_ms = started.elapsed().as_millis() as u64,
                                    "window-focus: primary slot warm"
                                ),
                                Err(e) => tracing::warn!(
                                    error = %e,
                                    "window-focus: warmup failed"
                                ),
                            }
                        }
                    }
                });
            }
        })
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

            // If `SOVEREIGN_USE_SUPERVISOR=1`, try to bring the daemon
            // up as a child process and switch to Attach against it.
            // Returns the original mode + None when the feature is off
            // or supervision fails to come up healthy. This is the
            // PR-2 dogfood path; PR-3 will flip the default. See
            // supervisor_setup.rs.
            let (bootstrap_mode, supervisor) =
                tauri::async_runtime::block_on(supervisor_setup::maybe_start(
                    bootstrap_mode,
                    handle.clone(),
                ));
            if supervisor.is_some() {
                tracing::info!(
                    ?bootstrap_mode,
                    "supervisor: bootstrap mode after supervision"
                );
            }

            // Create app state (loads config, no Runtime yet).
            let app_state = AppState::new_with_mode(
                Arc::clone(&approval),
                bootstrap_mode,
                supervisor,
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
                // Dev escape hatch: `SOVEREIGN_DEV_FORCE_SETUP=1`
                // makes the app behave as if it's a first launch —
                // routes to WelcomeThreshold → SetupFlow even when
                // the persisted `DesktopConfig.setup_complete` says
                // we're past it. Lets us iterate on the onboarding
                // surface without wiping `~/.sovereign/` and
                // re-downloading the multi-GB GGUFs (the planner's
                // download_gguf validates existing files and
                // short-circuits, so SetupFlow plays through fast).
                //
                // In-memory override only: not persisted to disk, so
                // restarting without the env var resumes the saved
                // setup state.
                let force_setup = std::env::var("SOVEREIGN_DEV_FORCE_SETUP")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                if force_setup {
                    tracing::info!(
                        "SOVEREIGN_DEV_FORCE_SETUP=1 — re-running onboarding \
                         (in-memory override; persisted setup state unchanged)"
                    );
                    let mut cfg = state_clone.config.write().await;
                    cfg.setup_complete = false;
                }

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
                        // Start the continuous corpus-status poller so
                        // the Knowledge settings pane reflects ingests
                        // the daemon is running regardless of whether
                        // Desktop initiated them (e.g. auto-collaborate
                        // resume after a previous session closed with
                        // Wikipedia mid-ingest).
                        commands::spawn_corpus_status_poller(
                            handle_clone.clone(),
                            Arc::clone(&state_clone),
                        );

                        // Re-apply the W4 consent ceiling at boot so a
                        // daemon restart doesn't silently revert to
                        // unlimited peer inference. No-op when the
                        // user hasn't recorded consent yet (the
                        // ConsentGate is about to render).
                        let consent_ceiling = state_clone
                            .config
                            .read()
                            .await
                            .first_mesh_consent
                            .as_ref()
                            .map(|c| c.ceiling);
                        if let Some(ceiling) = consent_ceiling {
                            if let Err(e) =
                                commands::set_contribution_ceiling(Some(ceiling)).await
                            {
                                tracing::warn!(
                                    error = %e,
                                    ceiling,
                                    "boot: failed to re-apply first_mesh_consent ceiling"
                                );
                            } else {
                                tracing::info!(
                                    ceiling,
                                    "boot: re-applied first_mesh_consent ceiling"
                                );
                            }
                        }

                        // Install OCR context if the manager came up
                        // and the Tesseract sidecar is bundled. No-op
                        // when not available — `lc_ocr_available`
                        // tells the UI to hide the OCR offer.
                        if let Some(mgr) = state_clone
                            .local_corpus
                            .read()
                            .await
                            .as_ref()
                            .cloned()
                        {
                            // Default daemon URL — same one the
                            // existing inference path uses.
                            let daemon_url = "http://127.0.0.1:9741".to_string();
                            // Resolve the chat model's name (file stem)
                            // so the cleanup pass can target it by name.
                            // The daemon registers each loaded slot under
                            // its file stem (see
                            // `register_local_model_slots` in
                            // sovereign-mesh), and there's no "fast"
                            // alias in the routing layer — passing
                            // `"fast"` would 503 on a CLI-daemon
                            // setup. Falls back to "fast" only as a
                            // last resort for older configs without a
                            // model_path set.
                            let cleanup_model = state_clone
                                .config
                                .read()
                                .await
                                .model_path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "fast".to_string());
                            local_corpus_commands::install_ocr_ctx_for_app(
                                &handle_clone,
                                &mgr,
                                daemon_url,
                                cleanup_model,
                            )
                            .await;
                        }
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
            commands::redirect_turn,
            commands::resume_session,
            commands::cancel_stream,
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
            commands::get_last_turn_provenance,
            commands::finalize_inner_work_conversation,
            commands::forget_memory,
            commands::weaken_memory,
            commands::get_config,
            commands::save_config,
            commands::is_setup_complete,
            commands::complete_setup,
            commands::complete_setup_auto,
            commands::start_default_corpus_install,
            commands::detect_hardware,
            commands::detect_bootstrap,
            commands::warmup_primary_slot,
            commands::search_web,
            commands::scan_for_models,
            commands::download_model,
            commands::list_corpora,
            commands::install_corpus,
            commands::lc_expand_corpus,
            commands::lc_can_expand,
            commands::lc_start_layered_setup,
            commands::remove_corpus,
            commands::pause_corpus,
            commands::get_ingest_budget,
            commands::set_ingest_budget,
            commands::get_mesh_quiesced,
            commands::set_mesh_quiesced,
            commands::get_contribution_status,
            commands::set_contribution_ceiling,
            commands::pause_contributions,
            commands::resume_contributions,
            commands::get_recent_contributions,
            commands::prepare_crash_report,
            commands::get_first_mesh_consent,
            commands::record_first_mesh_consent,
            commands::get_storage_budget,
            commands::set_storage_budget,
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
            commands::read_get_chunk,
            commands::read_get_chunk_neighbors,
            commands::read_get_atom_card,
            commands::read_get_atom_elsewhere,
            atlas_commands::atlas_list_corpora,
            atlas_commands::atlas_list_atoms,
            atlas_commands::atlas_get_atom_detail,
            commands::recipe_validate,
            commands::recipe_test,
            recipe_commands::corpus_import_recipe,
            recipe_commands::corpus_get_recipe_parameters,
            recipe_commands::corpus_install_with_parameters,
            recipe_author_commands::recipe_author_list_projects,
            recipe_author_commands::recipe_author_new_project,
            recipe_author_commands::recipe_author_dashboard_state,
            recipe_author_commands::recipe_author_restore_checkpoint,
            recipe_author_commands::recipe_author_set_workspace_active,
            mesh_commands::mesh_create,
            mesh_commands::mesh_join,
            mesh_commands::mesh_preview_join_link,
            mesh_commands::mesh_get_state,
            mesh_commands::mesh_is_running,
            mesh_commands::mesh_leave,
            mesh_commands::mesh_rotate_invite,
            mesh_commands::mesh_relay_candidates,
            mesh_commands::suggest_node_name,
            mesh_commands::mesh_diagnostics,
            mesh_commands::mesh_get_contributions,
            mesh_commands::mesh_set_peer_preference,
            mesh_commands::mesh_clear_peer_preference,
            mesh_commands::mesh_list_peer_preferences,
            insight_commands::clip_insight,
            insight_commands::list_insights,
            insight_commands::search_insights,
            insight_commands::delete_insight,
            insight_commands::get_sink_status,
            insight_commands::explore_insights,
            local_corpus_commands::lc_validate_path,
            local_corpus_commands::lc_ocr_available,
            local_corpus_commands::lc_pre_scan,
            local_corpus_commands::lc_ingest,
            local_corpus_commands::lc_list,
            local_corpus_commands::lc_remove,
            local_corpus_commands::lc_incomplete_jobs,
            local_corpus_commands::lc_search,
            local_corpus_commands::lc_cluster,
            local_corpus_commands::lc_get_preview,
            local_corpus_commands::lc_check_git,
            local_corpus_commands::lc_write_tags,
            local_corpus_commands::lc_list_snapshots,
            local_corpus_commands::lc_rollback,
            local_corpus_commands::lc_clean,
            local_corpus_commands::lc_cancel,
            watched_folder_commands::lc_watch_register,
            watched_folder_commands::lc_watch_list,
            watched_folder_commands::lc_watch_status,
            watched_folder_commands::lc_watch_state,
            watched_folder_commands::lc_watch_pause,
            watched_folder_commands::lc_watch_resume,
            watched_folder_commands::lc_watch_confirm_deletion,
            watched_folder_commands::lc_watch_sync_now,
            watched_folder_commands::lc_watch_details,
            watched_folder_commands::lc_watch_document,
            watched_folder_commands::lc_watch_add_root,
            watched_folder_commands::lc_watch_remove_root,
            watched_folder_commands::lc_watch_enrich_enable,
            watched_folder_commands::lc_watch_enrich_disable,
            watched_folder_commands::lc_watch_enrich_rebuild,
            watched_folder_commands::lc_watch_remove,
            watched_folder_commands::lc_watch_incomplete_jobs,
            enrich_commands::enrich_build_async,
            enrich_commands::enrich_cancel_build,
            enrich_commands::enrich_errors,
            enrich_commands::enrich_sep_ingest,
            enrich_commands::enrich_list_corpora,
            enrich_commands::enrich_init_for_local_corpus,
            enrich_commands::enrich_estimate,
            enrich_commands::enrich_get_active_job,
            enrich_commands::enrich_get_starter_questions,
            enrich_commands::is_first_run,
            enrich_commands::mark_first_run_complete,
        ])
        .run(tauri::generate_context!())
        .expect("error running Sovereign");
    ExitCode::SUCCESS
}
