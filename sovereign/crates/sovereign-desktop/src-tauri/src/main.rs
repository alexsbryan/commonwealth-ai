// SPDX-License-Identifier: AGPL-3.0-or-later
mod approval;
mod atlas_commands;
mod bootstrap;
#[cfg(debug_assertions)]
mod command_bridge;
mod commands;
mod crash_bundle;
mod crash_report;
mod dev_flags;
mod enrich_commands;
mod error;
mod friendly_names;
mod governance_commands;
mod import_commands;
mod insight_commands;
mod local_corpus_commands;
mod mesh_commands;
mod meshapp;
mod mobile_host_setup;
mod recipe_author_commands;
mod recipe_commands;
mod routing_events;
mod setup_flow;
mod smoketest;
mod state;
mod attach_watch;
mod supervisor;
mod supervisor_setup;
mod tray;
mod update_commands;
mod collaborate_commands;
mod watched_folder_commands;
mod workflow_commands;

/// Shared test-only support. Lives in the crate root so every module's
/// `#[cfg(test)]` code can reach it via `crate::test_support`.
#[cfg(test)]
pub(crate) mod test_support {
    /// One process-wide lock for tests that mutate the **global** `HOME` env
    /// var. Both `crash_report` and `smoketest` derive their on-disk store
    /// from `home_dir()`, and their tests point `HOME` at a tempdir to isolate.
    /// They compile into the SAME test binary and run concurrently — so each
    /// having its own private mutex is not mutual exclusion at all: one test
    /// swaps `HOME` out from under another mid-closure, and the victim reads
    /// the wrong directory. This single shared lock is the actual guard. Any
    /// new HOME-mutating test in this crate must take it too.
    pub static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

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

    // Supervised child-daemon mode (DAEMON_RESILIENCE.md P0.1): when
    // relaunched as `--daemon-child`, this process IS the daemon — the
    // identical entry `sovereign-cli-daemon daemon run` uses (panic
    // hook, run lock, RAM-derived OOM limits, listener watchdog and
    // all) — and never initializes Tauri. The parent desktop spawns +
    // supervises it (see `supervisor_setup.rs`); a ggml SEGV kills only
    // this child, and the supervisor restarts it behind the reconnect
    // surface instead of the whole window dying.
    if argv.iter().any(|a| a == "--daemon-child") {
        std::process::exit(sovereign_cli_daemon::daemon_child_main());
    }

    // The grounding gate's verification note (the failed-claim caveat) rides
    // `metadata.grounding_gate.failed_claims` on THIS surface, rendered as a
    // collapsible disclosure by AssistantMessage.svelte — not appended to the
    // answer text. The in-text note is the safe default for surfaces with no
    // UI to carry the caveat (API/CLI: never-silent invariant), but on desktop
    // it owned the answer's final words and zeroed the grace gate's
    // agency/clean components (persona-QA receipts, 2026-07). Set the metadata
    // default only when unset so `SOVEREIGN_NOTE_AS_METADATA=0` can still force
    // the legacy in-text note for debugging.
    if std::env::var_os("SOVEREIGN_NOTE_AS_METADATA").is_none() {
        std::env::set_var("SOVEREIGN_NOTE_AS_METADATA", "1");
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
                // `bootstrap` + `mesh_state` are explicit event targets
                // (not module paths) used by the Attach/Local decision
                // and the mesh-state read — surfaced at info so the
                // "why is Members empty?" trace shows without RUST_LOG.
                //
                // Glass-box level for the four inference crates
                // (core/tools/inference/corpus_engine) is `debug` in dev
                // builds and `info` in release. A shipped app should not
                // firehose per-inference debug chatter (prompt sizes, chunk
                // counts) at every end user by default — but the visibility
                // stays one `RUST_LOG=sovereign_core=debug` away, so the
                // glass-box contract holds as an opt-in rather than a
                // default. Dev builds keep the firehose so day-to-day work
                // is fully instrumented with no ceremony.
                //
                // synth.lifecycle / synth.continue / synth.truncation / gate.call
                // are CUSTOM string targets (not module paths), so `sovereign_core`
                // does NOT enable them — they must be named explicitly. Kept at info
                // REGARDLESS of build so the answer-truncation lifecycle (draft finish
                // vs effective cap, soft-landing continuation rounds, gate rewrite cap)
                // is visible in EVERY run's log — a mid-`[Source:` or mid-word tail
                // becomes diagnosable without a contrived repro (glassbox: instrument
                // the real pipeline, don't hypothesize). Low volume: ~one line per
                // grounded turn, so they stay on even in release.
                let glassbox = if cfg!(debug_assertions) { "debug" } else { "info" };
                format!(
                    "sovereign_desktop=info,\
                     sovereign_core={glassbox},\
                     sovereign_tools={glassbox},\
                     sovereign_inference={glassbox},\
                     sovereign_mesh=info,\
                     commonwealth_discovery=info,\
                     commonwealth_api=info,\
                     corpus_engine={glassbox},\
                     bootstrap=info,\
                     mesh_state=info,\
                     synth.lifecycle=info,\
                     synth.continue=info,\
                     synth.truncation=info,\
                     synth.refusal_retry=info,\
                     synth.citation=info,\
                     synth.budget=info,\
                     gate.call=info,\
                     gate.lifecycle=info,\
                     grounding_gate=info,\
                     agentic_kq=info,\
                     retrieval_audit=info"
                )
                .into()
            }),
        )
        .init();

    // Capture Rust panics into a durable, local crash record (with a
    // backtrace) before delegating to the default hook. svrnmesh is
    // decentralized — there is no central error pipeline — so a panic on a
    // user's machine must be captured *there*, viewable and submittable. See
    // `crate::crash_report`. Native (SIGSEGV) model crashes are captured
    // separately via the crash-isolation subprocess.
    crash_report::install_panic_hook();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        // Auto-update via signed manifest from svrnme.sh.
        // Endpoint + pubkey configured in tauri.conf.json.
        .plugin(tauri_plugin_updater::Builder::new().build())
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
                    if let Some(state) = app.try_state::<std::sync::Arc<state::AppState>>() {
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
            // Command bridge: loopback HTTP surface for external test
            // harnesses (Playwright real-mode). Debug builds only,
            // opt-in via SOVEREIGN_COMMAND_BRIDGE=1. See command_bridge.rs.
            #[cfg(debug_assertions)]
            if command_bridge::enabled() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(command_bridge::serve(handle));
            }

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
            let bootstrap_mode = tauri::async_runtime::block_on(bootstrap::detect());
            tracing::info!(?bootstrap_mode, "bootstrap mode resolved");

            // When supervised mode is opted into (`SOVEREIGN_USE_SUPERVISOR=1`),
            // bring the daemon up as a supervised child and switch to Attach
            // against it. Returns the original mode + None when not supervised
            // or startup fails (→ in-process fall-back). See supervisor_setup.rs.
            let (bootstrap_mode, supervisor) = tauri::async_runtime::block_on(
                supervisor_setup::maybe_start(bootstrap_mode, handle.clone()),
            );
            if supervisor.is_some() {
                tracing::info!(
                    ?bootstrap_mode,
                    "supervisor: bootstrap mode after supervision"
                );
            }

            // Attach-mode daemon health watch (DAEMON_RESILIENCE.md
            // P0.2): only for an EXTERNALLY-owned daemon — true Attach
            // with no supervisor. The supervised child has its own 2s
            // heartbeat; in-process Local has nothing to poll.
            if supervisor.is_none() {
                if let bootstrap::BootstrapMode::Attach { client_port, .. } = &bootstrap_mode {
                    attach_watch::spawn(handle.clone(), *client_port);
                }
            }

            // Create app state (loads config, no Runtime yet).
            let app_state =
                AppState::new_with_mode(Arc::clone(&approval), bootstrap_mode, supervisor);
            let app_state = Arc::new(app_state);
            app.manage(app_state.clone());

            // Opt-in Mobile access: if enabled in the desktop config, start the
            // supervised `sovereign-server` host at launch. It delegates all
            // inference to the local daemon, so it loads no models of its own.
            {
                let st = Arc::clone(&app_state);
                let enabled = tauri::async_runtime::block_on(async {
                    st.config.read().await.mobile_access_enabled
                });
                if enabled {
                    match mobile_host_setup::start() {
                        Ok(h) => {
                            tauri::async_runtime::block_on(async {
                                *st.mobile_host_supervisor.write().await = Some(h);
                            });
                            tracing::info!("mobile-access: started at launch (config enabled)");
                        }
                        Err(e) => {
                            tracing::warn!("mobile-access: failed to start at launch: {e}")
                        }
                    }
                }
            }

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
                dev_flags::log_active();
                let force_setup = dev_flags::force_setup();
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
                drop(config);
                // Model paths live in `SetupConfig` (`config.toml`) now —
                // resolve the fast slot from there to decide whether a real
                // GGUF is present.
                let model_exists = crate::state::ResolvedModelSlots::load()
                    .map(|s| !s.fast.as_os_str().is_empty() && s.fast.exists())
                    .unwrap_or(false);

                if !setup_done || !model_exists {
                    if !setup_done {
                        tracing::info!("First launch — waiting for setup wizard");
                    } else {
                        tracing::warn!(
                            "Model not found — returning to setup wizard"
                        );
                        // Model paths live in SetupConfig now; clear only the
                        // desktop's setup flag so the wizard re-runs and
                        // rewrites config.toml's [models].
                        let mut config = state_clone.config.write().await;
                        config.setup_complete = false;
                        let _ = config.save();
                    }
                    let _ = handle_clone.emit("setup-required", ());
                    return;
                }

                // Phase-timing narration: one log line per bootstrap
                // phase with total elapsed + delta since the previous
                // phase. This is the glassbox answer to "why does the
                // splash take N seconds" — every user boot self-
                // attributes instead of needing a profiling session
                // (target `bootstrap.phase`; on by default at INFO).
                let boot_start = std::time::Instant::now();
                let last_phase = std::sync::Mutex::new((boot_start, String::from("start")));
                let progress: state::BootstrapProgressCb = Box::new(move |phase| {
                    let now = std::time::Instant::now();
                    let mut guard = match last_phase.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    let (prev_at, prev_name) =
                        std::mem::replace(&mut *guard, (now, format!("{phase:?}")));
                    tracing::info!(
                        phase = ?phase,
                        total_ms = boot_start.elapsed().as_millis() as u64,
                        prev_phase = %prev_name,
                        prev_took_ms = now.duration_since(prev_at).as_millis() as u64,
                        "bootstrap phase"
                    );
                });
                match state::bootstrap_with_progress(&state_clone, Some(progress)).await {
                    Ok(()) => {
                        tracing::info!(
                            total_ms = boot_start.elapsed().as_millis() as u64,
                            "bootstrap complete"
                        );
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
                            if let Err(e) = commands::set_contribution_ceiling_at(
                                &state_clone.internal_base_url(),
                                Some(ceiling),
                            )
                            .await
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
                        if let Some(mgr) = state_clone.local_corpus.read().await.as_ref().cloned() {
                            // Daemon client URL — resolved from the
                            // bootstrap mode so a non-default port works.
                            let daemon_url = state_clone.client_base_url();
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
                            let cleanup_model = crate::state::ResolvedModelSlots::load_or_default()
                                .fast
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_string())
                                .filter(|s| !s.is_empty())
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
                        let _ = handle_clone
                            .emit("backend-error", approval::ErrorPayload { message: e });
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
            commands::notebook_conversations,
            commands::get_conversation,
            commands::delete_conversation,
            commands::rename_conversation,
            commands::set_conversation_enabled_corpora,
            commands::search_messages,
            commands::export_answer,
            commands::submit_approval,
            commands::submit_input,
            commands::submit_information_response,
            commands::submit_information_search,
            commands::list_lessons,
            commands::save_lesson,
            commands::set_lesson_enabled,
            commands::delete_lesson,
            commands::list_skills,
            commands::toggle_skill,
            commands::get_last_turn_provenance,
            commands::finalize_inner_work_conversation,
            commands::forget_memory,
            commands::weaken_memory,
            commands::get_config,
            commands::save_config,
            commands::get_mobile_pairing,
            commands::set_mobile_access,
            commands::get_setup_context_size,
            commands::set_setup_context_size,
            commands::get_setup_model_slots,
            commands::set_setup_model_slots,
            commands::is_setup_complete,
            commands::is_backend_ready,
            commands::complete_setup,
            commands::complete_setup_auto,
            commands::get_setup_report,
            commands::start_default_corpus_install,
            commands::detect_hardware,
            commands::detect_bootstrap,
            commands::warmup_primary_slot,
            commands::search_web,
            commands::scan_for_models,
            commands::delete_model,
            commands::model_file_size,
            commands::recommended_profile,
            commands::primary_catalog,
            commands::slot_recommendation,
            commands::list_daemon_models,
            commands::get_runtime_status,
            commands::supervisor_reconnect,
            commands::supervisor_active,
            commands::attach_restart_daemon,
            commands::download_model,
            commands::list_corpora,
            commands::notebook_list,
            commands::install_corpus,
            commands::lc_expand_corpus,
            commands::lc_can_expand,
            commands::lc_start_layered_setup,
            commands::lc_newsworthy_status,
            commands::lc_newsworthy_tick,
            commands::lc_enrichment_status,
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
            commands::get_activity_summary,
            commands::get_activity_recent,
            commands::get_chat_activity,
            commands::prepare_crash_report,
            crash_report::list_crash_records,
            crash_report::read_crash_record,
            crash_report::delete_crash_record,
            crash_report::export_crash_record,
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
            atlas_commands::atlas_subgraph,
            atlas_commands::atlas_get_atom_detail,
            governance_commands::governance_get_view,
            governance_commands::governance_resolve,
            governance_commands::governance_accept,
            governance_commands::governance_dismiss,
            governance_commands::governance_undo_tension,
            governance_commands::governance_seed,
            governance_commands::governance_post_build_seed,
            governance_commands::governance_write_recipe,
            governance_commands::governance_export_write,
            atlas_commands::atlas_list_conv_corpora,
            atlas_commands::atlas_list_conversations,
            atlas_commands::atlas_get_conv_detail,
            atlas_commands::atlas_get_conv_entities,
            atlas_commands::atlas_check_gliner_model,
            atlas_commands::atlas_download_gliner_model,
            atlas_commands::atlas_get_chunk_entity_progress,
            atlas_commands::atlas_get_entity_aggregate,
            commands::recipe_validate,
            commands::recipe_test,
            commands::recipe_run_harness,
            recipe_commands::corpus_import_recipe,
            recipe_commands::corpus_get_recipe_parameters,
            recipe_commands::corpus_install_with_parameters,
            import_commands::import_anthropic_zip,
            import_commands::import_chatgpt_zip,
            import_commands::import_email_archive,
            recipe_author_commands::recipe_author_list_projects,
            recipe_author_commands::recipe_author_new_project,
            recipe_author_commands::recipe_author_dashboard_state,
            recipe_author_commands::recipe_author_save_edited_toml,
            recipe_author_commands::recipe_author_link_recent_artifact,
            recipe_author_commands::recipe_author_restore_checkpoint,
            recipe_author_commands::recipe_author_build_prelude,
            workflow_commands::workflow_list_runnable,
            workflow_commands::workflow_capabilities,
            workflow_commands::workflow_run,
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
            local_corpus_commands::lc_enrich_now,
            local_corpus_commands::lc_enrich_reset,
            local_corpus_commands::lc_reenrich_note,
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
            collaborate_commands::mesh_assist_eligible_peers,
            collaborate_commands::mesh_assist_start,
            collaborate_commands::mesh_assist_status,
            collaborate_commands::mesh_assist_revoke,
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
            enrich_commands::enrich_list_corpora,
            enrich_commands::enrich_get_starter_questions,
            enrich_commands::install_starter_corpus,
            enrich_commands::is_first_run,
            enrich_commands::mark_first_run_complete,
            update_commands::check_for_update,
            update_commands::install_update,
            commands::meshapp_capabilities,
            commands::meshapp_read_corpus,
            commands::meshapp_search_parcels,
            commands::meshapp_parcel_analytics,
            commands::meshapp_graph,
            commands::meshapp_node,
            commands::meshapp_findings,
            commands::meshapp_search_entities,
            commands::meshapp_claims,
            commands::meshapp_questions,
            commands::meshapp_reconciliation,
            commands::meshapp_subgraph,
            commands::meshapp_corpus_stats,
            commands::meshapp_timeline,
            commands::meshapp_read_chunk,
            commands::meshapp_document_feed,
            commands::meshapp_wrapped_artifact,
            commands::meshapp_open_outer_work,
            commands::meshapp_list_installs,
            commands::meshapp_record_install,
            commands::meshapp_uninstall,
            commands::meshapp_stage_corpus_recipe,
            commands::meshapp_installed_apps,
            commands::meshapp_open,
            commands::open_corpus_explorer,
            commands::mcp_list_servers,
            commands::mcp_add_server,
            commands::mcp_remove_server,
            commands::mcp_test_connection,
            commands::mcp_set_token,
            commands::mcp_clear_token,
        ])
        .build(tauri::generate_context!())
        .expect("error building svrnmesh")
        .run(|_app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Graceful shutdown: skip C++ static destructors so ggml-metal's
                // device sweeper can't abort under `__cxa_finalize` at process
                // exit (which pops a macOS crash dialog). Reuses the daemon's
                // proven fast-exit path — the kernel reclaims Metal/KV/mmaps.
                sovereign_inference::fast_exit_skip_destructors(0);
            }
        });
    ExitCode::SUCCESS
}
