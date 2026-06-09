// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<DesktopConfig, String> {
    Ok(state.config.read().await.clone())
}

/// Pairing card for the Settings → Mobile access panel (address + tenant +
/// token the phone enters). Reads/creates `~/.sovereign/mobile-host.toml`.
#[tauri::command]
pub async fn get_mobile_pairing() -> Result<crate::mobile_host_setup::MobilePairing, String> {
    crate::mobile_host_setup::pairing()
}

/// Start or stop the supervised mobile host at runtime (the toggle's runtime
/// half — persistence rides the normal `save_config`). Starting spawns a
/// `sovereign-server` child that delegates inference to the daemon; stopping
/// aborts the supervise task, whose `kill_on_drop` SIGKILLs the child.
#[tauri::command]
pub async fn set_mobile_access(state: State<'_, Arc<AppState>>, enabled: bool) -> Result<(), String> {
    let mut guard = state.mobile_host_supervisor.write().await;
    if enabled {
        if guard.is_none() {
            *guard = Some(crate::mobile_host_setup::start()?);
        }
    } else if let Some(handle) = guard.take() {
        handle.abort();
    }
    Ok(())
}

#[tauri::command]
pub async fn save_config(
    state: State<'_, Arc<AppState>>,
    config: DesktopConfig,
) -> Result<(), String> {
    config.save()?;
    let old = state.config.read().await.clone();
    let old_embed = old.embed_model_path.clone();
    let new_embed = config.embed_model_path.clone();
    let rebuild = config_needs_rebuild(&old, &config);

    // Mirror shared fields (model paths + data_dir) into SetupConfig
    // on disk. Cheap when nothing structural changed (compares fields,
    // writes only on diff), so we run it unconditionally — a sampling-
    // only save just no-ops here.
    //
    // Best-effort: a failure to write SetupConfig must not block the
    // desktop's local save. We log and move on — next desktop save
    // attempt will retry the mirror.
    if let Err(e) = mirror_to_setup_config(&config).await {
        tracing::warn!("save_config: could not mirror to SetupConfig: {e}");
    }
    // Daemon reload is best-effort AND fire-and-forget. The HTTP call
    // has a 5s timeout and previously serialised the Save button's
    // "Saved" indicator behind whatever the daemon happened to be doing
    // when the user hit Save. Background it: the daemon will pick up
    // the new SetupConfig before the user's next inference turn, and
    // the desktop's local cache is already correct.
    tokio::spawn(async {
        if let Err(e) = request_daemon_reload().await {
            tracing::warn!("save_config: admin/reload failed (background): {e}");
        }
    });

    *state.config.write().await = config;
    // If the embedding model changed, drop the cached inference so bootstrap
    // reloads it with the new embed model path.
    if old_embed != new_embed {
        *state.inference.write().await = None;
    }
    if rebuild {
        state::rebuild_runtime(&state).await
    } else {
        tracing::info!("save_config: no structural changes — skipping runtime rebuild");
        Ok(())
    }
}

/// Decide whether a config change warrants tearing down + rebuilding
/// the Runtime. The cost of a rebuild is high (skill discovery, tool
/// registry construction, KnowledgeView wire-up, potential embed model
/// re-load) — measured at 10–20s on a typical desktop. Most saves are
/// sampling tweaks (temperature, max_tokens, think_budget) or UX
/// toggles (node_name, enable_recipe_authoring) that the Runtime
/// either reads at request time or doesn't read at all; rebuilding for
/// those is pure latency.
///
/// Returns `true` only when a field that the Runtime captures at
/// construction time has actually changed. Fields read at request time
/// (everything in `InferenceConfig`) intentionally skip the rebuild;
/// the next chat turn reads the new value from `state.config`.
fn config_needs_rebuild(old: &DesktopConfig, new: &DesktopConfig) -> bool {
    // `context_size` is deliberately absent — it now lives in
    // `SetupConfig` (see the migration in `DesktopConfig::load`) and
    // changes route through the dedicated `set_setup_context_size`
    // Tauri command, which calls `EmbeddedLlamaCpp::rebuild_chat_contexts`
    // directly without rebuilding the Runtime.
    old.model_path != new.model_path
        || old.primary_model_path != new.primary_model_path
        || old.embed_model_path != new.embed_model_path
        || old.code_model_path != new.code_model_path
        || old.embed_family != new.embed_family
        || old.code_family != new.code_family
        || old.skills_dir != new.skills_dir
        || old.active_skills != new.active_skills
        || old.enabled_tools != new.enabled_tools
        || old.search_backend.provider != new.search_backend.provider
        || old.search_backend.api_key != new.search_backend.api_key
        || old.knowledge_view_enabled != new.knowledge_view_enabled
        || old.auto_escalate_to_web != new.auto_escalate_to_web
        || old.data_dir != new.data_dir
        // `custom_instructions` is captured into the Runtime's
        // `InferenceConfig` at construction (state.rs) and the Runtime is
        // held as an immutable `Arc<Runtime>` — there is NO live-update
        // path, so a persona edit only takes effect after a rebuild.
        // (The sampling fields above are captured the same way; persona
        // is the one a user actively edits and expects applied on the
        // next turn, so we pay the rebuild for it.)
        || old.custom_instructions != new.custom_instructions
}

/// Mirror the three model paths + data_dir from `DesktopConfig` into
/// `SetupConfig`. Creates the config file on first write if it didn't
/// exist (matches `sovereign setup` behaviour). Leaves `daemon`
/// defaults in place — port changes go through the CLI's `sovereign
/// setup`, not the desktop Settings panel.
async fn mirror_to_setup_config(desktop: &DesktopConfig) -> Result<(), String> {
    use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};

    let mut cli = SetupConfig::load().unwrap_or_else(|_| SetupConfig {
        models: ModelsSection {
            primary: desktop
                .primary_model_path
                .clone()
                .unwrap_or_else(|| desktop.model_path.clone()),
            // Desktop config always carries a model_path (the
            // wizard requires one). Map it to an explicit fast.
            fast: Some(desktop.model_path.clone()),
            embed: desktop
                .embed_model_path
                .clone()
                .unwrap_or_else(|| desktop.model_path.clone()),
            code: desktop.code_model_path.clone(),
            context_size: None,
            extra: std::collections::BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
        },
        daemon: DaemonSection::default(),
        data: DataSection {
            dir: desktop.data_dir.clone(),
        },
        watched_folders: Default::default(),
        memory: Default::default(),
    });

    let cli_primary_before = cli.models.primary.clone();
    let cli_fast_before = cli.models.fast.clone();
    let cli_embed_before = cli.models.embed.clone();
    let cli_data_before = cli.data.dir.clone();

    let mut changed = false;
    let mut changed_fields: Vec<&'static str> = Vec::new();
    // Desktop's `model_path` is the operator's chosen fast slot; if it
    // differs from what we have, write it back as an explicit fast.
    // Comparing against fast_path() handles the subsumed case
    // naturally — when the desktop set the same path as primary, we
    // leave `fast` as None so the subsume relationship stays clean
    // instead of materialising a redundant explicit entry.
    let desktop_path = desktop.model_path.as_path();
    if desktop_path != cli.models.fast_path() {
        cli.models.fast = if desktop_path == cli.models.primary {
            None
        } else {
            Some(desktop.model_path.clone())
        };
        changed = true;
        changed_fields.push("fast");
    }
    if let Some(p) = &desktop.primary_model_path {
        if &cli.models.primary != p {
            cli.models.primary = p.clone();
            changed = true;
            changed_fields.push("primary");
        }
    }
    if let Some(e) = &desktop.embed_model_path {
        if &cli.models.embed != e {
            cli.models.embed = e.clone();
            changed = true;
            changed_fields.push("embed");
        }
    }
    if cli.data.dir != desktop.data_dir {
        cli.data.dir = desktop.data_dir.clone();
        changed = true;
        changed_fields.push("data_dir");
    }

    if changed {
        tracing::info!(
            fields = ?changed_fields,
            new_primary = %cli.models.primary.display(),
            new_fast = ?cli.models.fast.as_ref().map(|p| p.display().to_string()),
            new_embed = %cli.models.embed.display(),
            new_data_dir = %cli.data.dir.display(),
            "save_config: mirroring DesktopConfig → SetupConfig"
        );
        match cli.save() {
            Ok(path) => {
                tracing::info!(
                    target = %path.display(),
                    "save_config: SetupConfig written to disk"
                );
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "save_config: SetupConfig write FAILED — the desktop model \
                     pick will not propagate to the daemon on next start"
                );
                return Err(e);
            }
        }
    } else {
        // Surface the no-op decision too — if the user reports
        // "setting didn't sync" but mirror logs no-op, the bug is
        // upstream (Settings UI didn't push the new field).
        tracing::info!(
            cli_primary = %cli_primary_before.display(),
            cli_fast = ?cli_fast_before.as_ref().map(|p| p.display().to_string()),
            cli_embed = %cli_embed_before.display(),
            cli_data_dir = %cli_data_before.display(),
            desktop_primary = ?desktop.primary_model_path.as_ref().map(|p| p.display().to_string()),
            desktop_fast = %desktop.model_path.display(),
            desktop_embed = ?desktop.embed_model_path.as_ref().map(|p| p.display().to_string()),
            desktop_data_dir = %desktop.data_dir.display(),
            "save_config: mirror no-op — all shared fields already match SetupConfig"
        );
    }
    Ok(())
}

/// POST `http://127.0.0.1:9741/v1/admin/reload` so a CLI-started
/// daemon picks up the `SetupConfig` changes we just wrote. When the
/// daemon replies `{restart_required: true}` — typically a port or
/// data_dir change — fall back to `launchctl kickstart` / `systemctl
/// --user restart`. Swallows all errors: if no daemon is running,
/// the next `sovereign daemon run` will read the fresh config anyway.
async fn request_daemon_reload() -> Result<(), String> {
    let url = "http://127.0.0.1:9741/v1/admin/reload";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = client
        .post(url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| format!("POST admin/reload: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
    if !status.is_success() {
        return Err(format!("admin/reload returned {status}: {body}"));
    }
    let restart_required = body
        .get("restart_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    tracing::info!(
        reloaded = ?body.get("reloaded_fields"),
        restart_required,
        "save_config: admin/reload completed"
    );
    if restart_required {
        if let Err(e) = kickstart_daemon() {
            tracing::warn!("save_config: kickstart fallback failed: {e}");
        }
    }
    Ok(())
}

/// Best-effort restart of the `sovereign-daemon` service. Used only
/// when the admin/reload handler reported `restart_required` (port
/// or data_dir change) — hot reload can't rebind listeners.
fn kickstart_daemon() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // `id -u` avoids pulling in `libc` just for `getuid()`.
        let uid_out = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| format!("spawn id: {e}"))?;
        let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
        let label = format!("gui/{uid}/com.sovereign.daemon");
        let out = Command::new("launchctl")
            .args(["kickstart", "-k", &label])
            .output()
            .map_err(|e| format!("spawn launchctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "launchctl kickstart {label} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let out = Command::new("systemctl")
            .args(["--user", "restart", "sovereign"])
            .output()
            .map_err(|e| format!("spawn systemctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user restart sovereign failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("service restart is only supported on macOS and Linux".into())
    }
}

/// Snapshot of the canonical chat-slot context window. Returned by
/// `get_setup_context_size`; the Settings panel shows the three values
/// side-by-side so the user can see configured vs. effective vs.
/// gguf-trained ceiling and make an informed decision before changing
/// the value via `set_setup_context_size`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SetupContextWindow {
    /// Value persisted in `~/.sovereign/config.toml`'s
    /// `[models].context_size`, or the daemon-side default (16384) when
    /// no explicit value is set. This is the value the next slot load
    /// will pass to `LlamaContextParams::with_n_ctx`.
    pub configured: u32,
    /// The currently-running primary slot's `effective_context_size()`
    /// — what `clamp_max_tokens` is actually budgeting against right
    /// now. Usually equals `configured`; may differ immediately after
    /// editing the value via `set_setup_context_size` if the reload
    /// hasn't completed yet. `None` when the active inference provider
    /// is remote-only (no local slot).
    pub effective: Option<u32>,
    /// GGUF-trained ceiling (`n_ctx_train`). llama.cpp silently caps
    /// `configured` at this value without a RoPE-scaling rebuild;
    /// surfacing it lets the Settings UI render an "up to N without
    /// recompile" hint. `None` when no local model is loaded.
    pub n_ctx_train: Option<u32>,
}

/// Read the canonical chat-slot context window state, sourced from
/// `~/.sovereign/config.toml` (configured value) and the currently-
/// loaded inference provider (effective + gguf ceiling). Settings
/// panel consumes this to render the read-only "current state" block
/// next to the editor.
#[tauri::command]
pub async fn get_setup_context_size(
    state: State<'_, Arc<AppState>>,
) -> Result<SetupContextWindow, String> {
    let configured = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.models.effective_context_size())
        .unwrap_or(16384);
    let (effective, n_ctx_train) = match state.inference.read().await.as_ref() {
        Some(inf) => (inf.effective_context_size(), inf.n_ctx_train_for_primary()),
        None => (None, None),
    };
    Ok(SetupContextWindow {
        configured,
        effective,
        n_ctx_train,
    })
}

/// Update the canonical chat-slot context window. Writes
/// `~/.sovereign/config.toml`'s `[models].context_size`, kicks the
/// daemon to reload (best-effort, background), then tears down the
/// desktop-embedded inference + Runtime so the next bootstrap call
/// reads the fresh value.
///
/// Hard bounds [512, 1_048_576] guard against footguns — 512 is below
/// llama.cpp's 256-byte pad granularity (and useless for any real
/// chat); 1M is the ceiling llama recently capped n_seq_max-aware
/// allocation at, plus a KV-cache size that's pathological on any
/// consumer hardware. The Settings UI clamps further based on
/// `n_ctx_train_for_primary` so the user can't request more than the
/// gguf supports without an explicit override.
///
/// Latency: ~15-30s on Metal (weights re-mmap + context rebuild).
/// Future: an in-place `EmbeddedLlamaCpp::rebuild_chat_contexts` would
/// reuse the cached `Arc<LlamaModel>` and cut this to ~5-10s, but the
/// drop+rebuild path here reuses the existing `state::rebuild_runtime`
/// machinery without structural changes to the inference layer.
#[tauri::command]
pub async fn set_setup_context_size(
    state: State<'_, Arc<AppState>>,
    new_ctx: u32,
) -> Result<(), String> {
    use sovereign_core::setup_config::SetupConfig;

    if !(512..=1_048_576).contains(&new_ctx) {
        return Err(format!(
            "context_size {new_ctx} outside [512, 1048576] — refusing to write"
        ));
    }

    // SetupConfig may not exist on a fresh install — fall back to a
    // synthesised one populated from the in-memory DesktopConfig's
    // paths. Mirrors `mirror_to_setup_config`'s construction.
    let cfg_result = SetupConfig::load();
    let mut cfg = match cfg_result {
        Ok(c) => c,
        Err(_) => {
            let desktop = state.config.read().await.clone();
            SetupConfig {
                models: sovereign_core::setup_config::ModelsSection {
                    primary: desktop
                        .primary_model_path
                        .clone()
                        .unwrap_or_else(|| desktop.model_path.clone()),
                    fast: Some(desktop.model_path.clone()),
                    embed: desktop
                        .embed_model_path
                        .clone()
                        .unwrap_or_else(|| desktop.model_path.clone()),
                    code: desktop.code_model_path.clone(),
                    context_size: None,
                    extra: std::collections::BTreeMap::new(),
                    max_extras_memory_gb: None,
                    primary_pool: None,
                },
                daemon: Default::default(),
                data: sovereign_core::setup_config::DataSection {
                    dir: desktop.data_dir.clone(),
                },
                watched_folders: Default::default(),
                memory: Default::default(),
            }
        }
    };

    cfg.models.context_size = Some(new_ctx);
    let path = cfg.save().map_err(|e| format!("save SetupConfig: {e}"))?;
    tracing::info!(
        new_ctx,
        target = %path.display(),
        "set_setup_context_size: SetupConfig written"
    );

    tokio::spawn(async {
        if let Err(e) = request_daemon_reload().await {
            tracing::warn!(
                error = %e,
                "set_setup_context_size: daemon admin/reload failed (background)"
            );
        }
    });

    *state.inference.write().await = None;
    crate::state::rebuild_runtime(&state).await
}

#[tauri::command]
pub async fn is_setup_complete(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.config.read().await.setup_complete)
}

/// Auto-config first-launch flow. Takes no input — runs hardware
/// probe → catalog selection → 3-model download → DB open → model
/// load → smoke test, narrating progress on the `setup-progress`
/// Tauri event channel. Returns when the backend is ready to serve
/// chat. Drives the desktop's `SetupFlow.svelte` (the *lazy
/// sunbeam* onboarding flow); the legacy `complete_setup` stays
/// available for tests/scripts that hand-build a `SetupConfig`.
#[tauri::command]
pub async fn complete_setup_auto(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    crate::setup_flow::run(app_handle, state.inner().clone()).await
}

/// Fire-and-forget background install of the default `wikipedia` Core
/// corpus (curated Vital Articles L5; a prebuilt snapshot keeps the
/// first-run download a one-time cost). Idempotent — `install_corpus`
/// short-circuits when the daemon is already ingesting it. The
/// desktop's `App.svelte` calls this on the transition into chat after
/// first-launch setup completes; it runs silently, surfacing only on
/// the regular `corpus-progress` channel that `Settings → Knowledge`
/// already listens to.
///
/// Previously installed `wikipedia-simple` (the small Layer 0). Simple
/// is now parked in "Coming soon", so the default install points at
/// Core — "core by default". (Heavier first-run download; if that's
/// undesirable, make this a no-op and let the user install Core
/// explicitly from Settings → Knowledge.)
#[tauri::command]
pub async fn start_default_corpus_install(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    install_corpus(app_handle, state, "wikipedia".into()).await
}

#[tauri::command]
pub async fn complete_setup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    setup: SetupConfig,
) -> Result<(), String> {
    // When the wizard skipped the model picker (because `SetupConfig`
    // from `sovereign setup` was detected), the incoming `setup` has
    // empty model paths. Fall back to what the CLI already wrote on
    // disk rather than clobbering the desktop config with empties.
    let cli_cfg = sovereign_core::setup_config::SetupConfig::load().ok();
    let mut config = state.config.write().await;
    if !setup.model_path.is_empty() {
        config.model_path = setup.model_path.into();
    } else if let Some(c) = cli_cfg.as_ref() {
        // Desktop tracks one model_path that surfaces in the UI as
        // "the model loaded in the fast role". fast_path() returns
        // primary when fast is subsumed, so the same field is right
        // in either case — no separate branch needed.
        config.model_path = c.models.fast_path().to_path_buf();
    }
    config.primary_model_path = setup
        .primary_model_path
        .map(std::path::PathBuf::from)
        .or_else(|| cli_cfg.as_ref().map(|c| c.models.primary.clone()));
    config.embed_model_path = setup
        .embed_model_path
        .map(std::path::PathBuf::from)
        .or_else(|| cli_cfg.as_ref().map(|c| c.models.embed.clone()));
    if let Some(dir) = setup.data_dir {
        config.data_dir = dir.into();
    } else if let Some(c) = cli_cfg.as_ref() {
        config.data_dir = c.data.dir.clone();
    }
    config.active_skills = setup.active_skills;
    if !setup.enabled_tools.is_empty() {
        config.enabled_tools = setup.enabled_tools;
    }
    if let Some(provider) = setup.search_provider {
        config.search_backend.provider = provider;
    }
    config.search_backend.api_key = setup.search_api_key;
    config.selected_tier = setup.selected_tier.clone();
    if let Some(flag) = setup.enable_recipe_authoring {
        config.enable_recipe_authoring = flag;
    }
    if let Some(flag) = setup.auto_escalate_to_web {
        config.auto_escalate_to_web = flag;
    }
    config.setup_complete = true;

    config.save()?;
    // Mirror into `~/.sovereign/config.toml` so the CLI-side daemon
    // sees the wizard's model picks. Without this, the wizard could
    // complete but the daemon at next start would read stale paths
    // (or the bare defaults `sovereign setup` last wrote). Same
    // best-effort + warn-log pattern as `save_config`.
    let config_for_mirror = config.clone();
    drop(config);
    if let Err(e) = mirror_to_setup_config(&config_for_mirror).await {
        tracing::warn!("complete_setup: could not mirror to SetupConfig: {e}");
    }
    if let Err(e) = request_daemon_reload().await {
        tracing::warn!("complete_setup: admin/reload failed: {e}");
    }

    state::bootstrap(&state).await?;

    // Notify the frontend that the backend is ready so the loading screen unblocks.
    let _ = app_handle.emit("backend-ready", ());

    // Trigger background corpus installs for the selected tier.
    if let Some(ref tier) = setup.selected_tier {
        let tier = tier.clone();
        let state_ref = Arc::clone(&state);
        let app = app_handle.clone();
        tokio::spawn(async move {
            start_tier_installs(&app, &state_ref, &tier).await;
        });
    }

    Ok(())
}
