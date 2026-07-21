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
/// token the phone enters, plus the no-VPN iroh pairing code once the
/// supervised server reports one). Reads/creates `~/.sovereign/mobile-host.toml`.
#[tauri::command]
pub async fn get_mobile_pairing() -> Result<crate::mobile_host_setup::MobilePairing, String> {
    crate::mobile_host_setup::pairing().await
}

/// Start or stop the supervised mobile host at runtime (the toggle's runtime
/// half — persistence rides the normal `save_config`). Starting spawns a
/// `sovereign-server` child that delegates inference to the daemon; stopping
/// aborts the supervise task, whose `kill_on_drop` SIGKILLs the child.
#[tauri::command]
pub async fn set_mobile_access(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> Result<(), String> {
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
    let rebuild = config_needs_rebuild(&old, &config);

    // Mirror the non-model shared fields (data_dir + shared_model role)
    // into SetupConfig on disk. Model *paths* no longer flow through
    // here — they live solely in `config.toml` and are edited via the
    // dedicated `set_setup_model_slots` command (single source of
    // truth). Cheap when nothing changed (compares fields, writes only
    // on diff).
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
    // Resolve the client base URL in sync code BEFORE the spawn — the
    // async block can't borrow `state` (it may outlive this function).
    let reload_base = state.client_base_url();
    tokio::spawn(async move {
        if let Err(e) = request_daemon_reload(reload_base).await {
            tracing::warn!("save_config: admin/reload failed (background): {e}");
        }
    });

    *state.config.write().await = config;
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
    // Model *paths* are no longer DesktopConfig fields — they live in
    // `config.toml` and their edits route through `set_setup_model_slots`,
    // which does its own inference teardown + runtime rebuild. Only the
    // model *family* hints remain here (embed_family/code_family), and a
    // family change still warrants a rebuild (tokenizer/template quirks).
    old.embed_family != new.embed_family
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

/// Mirror the **non-model** shared fields (`data_dir` + shared-model
/// role/id) from `DesktopConfig` into `SetupConfig`. Model *paths* are
/// NOT mirrored here — they are the sole province of `config.toml` and
/// are written via [`set_setup_model_slots`] (single source of truth).
///
/// Requires an existing `SetupConfig` on disk. The fresh-install
/// creation of `config.toml` (load-bearing for bootstrap-mode
/// resolution: without it every boot resolved `DesktopLegacy` and the
/// supervised-child path, which intercepts only `CliSetup`, never
/// engaged — DAEMON_RESILIENCE.md P0.1) no longer happens here: both
/// setup entrypoints (`setup_flow::run` step 5 and `complete_setup`)
/// create it via `write_model_slots_to_setup` — fatally on error —
/// BEFORE this mirror runs, and legacy desktop.toml installs get it
/// from `DesktopConfig::load`'s migration. When it is still absent
/// (pre-setup), there is nothing to mirror onto — a `ModelsSection`
/// can't be synthesized without paths, and there are none to take from
/// `DesktopConfig` any more — so we log and skip.
pub(crate) async fn mirror_to_setup_config(desktop: &DesktopConfig) -> Result<(), String> {
    use sovereign_core::setup_config::SetupConfig;

    let mut cli = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(
                error = %e,
                "mirror_to_setup_config: no SetupConfig on disk yet — nothing to \
                 mirror (model paths land via write_model_slots_to_setup at setup time)"
            );
            return Ok(());
        }
    };

    let mut changed = false;
    let mut changed_fields: Vec<&'static str> = Vec::new();
    if cli.data.dir != desktop.data_dir {
        cli.data.dir = desktop.data_dir.clone();
        changed = true;
        changed_fields.push("data_dir");
    }
    // Shared-model cluster role + id. Dormant backbone: the desktop ships no
    // shared-model UI in the alpha (the feature lives on the CLI path — see
    // docs/RUN_GLM_5_2_ON_THE_MESH.md). The mirror still writes them so a
    // `sovereign daemon run` started from this config picks them up at startup
    // (apply_shared_model_role_to_env). Kept so re-adding the UI later needs
    // no config plumbing.
    if cli.shared_model.role != desktop.shared_model_role
        || cli.shared_model.model_id != desktop.shared_model_id
    {
        cli.shared_model.role = desktop.shared_model_role;
        cli.shared_model.model_id = desktop.shared_model_id.clone();
        changed = true;
        changed_fields.push("shared_model");
    }

    // No `first_write` forcing here any more: config.toml creation is
    // owned by `write_model_slots_to_setup` (both setup entrypoints,
    // fatal on failure) and the legacy migration — by the time this
    // mirror runs on a configured machine the file exists, and on a
    // pre-setup machine we already returned above.
    if changed {
        tracing::info!(
            fields = ?changed_fields,
            new_data_dir = %cli.data.dir.display(),
            "save_config: mirroring non-model shared fields → SetupConfig"
        );
        cli.save().map_err(|e| {
            tracing::error!(error = %e, "save_config: SetupConfig mirror write FAILED");
            e
        })?;
    }
    Ok(())
}

/// POST `<base_url>/v1/admin/reload` so a CLI-started daemon picks up
/// the `SetupConfig` changes we just wrote. `base_url` is the
/// caller-resolved client base (`state.client_base_url()`) so a
/// non-default port works. When the daemon replies
/// `{restart_required: true}` — typically a port or data_dir change —
/// fall back to `launchctl kickstart` / `systemctl --user restart`.
/// Swallows all errors: if no daemon is running, the next `sovereign
/// daemon run` will read the fresh config anyway.
async fn request_daemon_reload(base_url: String) -> Result<(), String> {
    let url = format!("{base_url}/v1/admin/reload");
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
pub(crate) fn kickstart_daemon() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // `id -u` avoids pulling in `libc` just for `getuid()`.
        let uid_out = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| format!("spawn id: {e}"))?;
        let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
        let label = format!("gui/{uid}/com.svrnmesh.daemon");
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
            .args(["--user", "restart", "svrnmesh"])
            .output()
            .map_err(|e| format!("spawn systemctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user restart svrnmesh failed: {}",
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

    // SetupConfig may not exist on a fresh install (the wizard writes it
    // first via `set_setup_model_slots`). Model paths no longer live on
    // `DesktopConfig`, so a pre-wizard fallback synthesizes an otherwise-
    // empty config carrying just the data dir — the wizard fills in the
    // model slots. The common path is the `Ok(c)` branch.
    let mut cfg = match SetupConfig::load() {
        Ok(c) => c,
        // Same guard as `write_model_slots_to_setup`: an existing but
        // unparseable config.toml must error, not be silently replaced
        // with a synthesized default (which would wipe its other sections
        // on the `save()` below).
        Err(e) if SetupConfig::exists() => {
            return Err(format!(
                "config.toml exists but can't be parsed — refusing to overwrite it. \
                 Fix the file (or delete it to start fresh) and retry: {e}"
            ));
        }
        Err(_) => {
            let data_dir = state.config.read().await.data_dir.clone();
            SetupConfig {
                compute: Default::default(),
                models: sovereign_core::setup_config::ModelsSection {
                    primary: std::path::PathBuf::new(),
                    fast: None,
                    embed: std::path::PathBuf::new(),
                    code: None,
                    context_size: None,
                    extra: std::collections::BTreeMap::new(),
                    max_extras_memory_gb: None,
                    primary_pool: None,
                    fim: None,
                },
                daemon: Default::default(),
                data: sovereign_core::setup_config::DataSection { dir: data_dir },
                watched_folders: Default::default(),
                memory: Default::default(),
                iroh: Default::default(),
                shared_model: Default::default(),
                discovery: Default::default(),
                mcp_servers: Vec::new(),
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

    // Resolve the client base URL in sync code BEFORE the spawn — the
    // async block can't borrow `state` (it may outlive this function).
    let reload_base = state.client_base_url();
    tokio::spawn(async move {
        if let Err(e) = request_daemon_reload(reload_base).await {
            tracing::warn!(
                error = %e,
                "set_setup_context_size: daemon admin/reload failed (background)"
            );
        }
    });

    *state.inference.write().await = None;
    crate::state::rebuild_runtime(&state).await
}

/// The four configured model-slot paths, read from / written to
/// `~/.sovereign/config.toml`'s `[models]` — the single on-disk home for
/// model paths. The Settings "Model slots" panel binds these (replacing
/// the removed `DesktopConfig` path fields), so the panel and the daemon
/// can never disagree about what is configured. `code_family` stays on
/// `DesktopConfig` (a load-time family hint) and is surfaced here for the
/// panel's convenience.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SetupModelSlots {
    /// Quick-responder ("fast") GGUF path. Empty string when it is
    /// subsumed by the primary (no distinct fast model).
    pub fast: String,
    /// Main-responder ("primary") GGUF path, or null.
    pub primary: Option<String>,
    /// Embedding GGUF path, or null (library search disabled).
    pub embed: Option<String>,
    /// Optional code-specialist GGUF path, or null.
    pub code: Option<String>,
    /// Code-slot model family (lives on `DesktopConfig`; surfaced here).
    #[serde(default)]
    pub code_family: sovereign_core::model_family::ModelFamily,
}

/// Write the model-slot paths into the canonical `SetupConfig`
/// (`~/.sovereign/config.toml`), applying the fast-subsume rule (store
/// `fast = None` when it equals or is subsumed by `primary`, so peers
/// aren't advertised a duplicate slot). Preserves every non-model section
/// of an existing config; synthesizes the surrounding sections when none
/// exists yet. Returns the saved path. Shared by `set_setup_model_slots`
/// and the wizard's `complete_setup`.
pub(crate) fn write_model_slots_to_setup(
    fast: Option<std::path::PathBuf>,
    primary: Option<std::path::PathBuf>,
    embed: std::path::PathBuf,
    code: Option<std::path::PathBuf>,
    data_dir: std::path::PathBuf,
) -> Result<std::path::PathBuf, String> {
    use sovereign_core::setup_config::{DataSection, ModelsSection, SetupConfig};

    // Primary is the anchor; a missing/empty fast folds into it.
    let primary_path = primary
        .clone()
        .or_else(|| fast.clone())
        .ok_or_else(|| "at least one chat model (fast or primary) must be set".to_string())?;
    let fast_field = match fast {
        Some(f) if !f.as_os_str().is_empty() && f != primary_path => Some(f),
        _ => None,
    };

    let mut cli = match SetupConfig::load() {
        Ok(c) => c,
        // Synthesize ONLY when the file is genuinely absent (fresh
        // install). An existing-but-unparseable config.toml (the user
        // hand-edits it for bench swaps) must surface as an error —
        // synthesizing + saving here would silently replace the whole
        // file, wiping every non-model section (daemon ports, iroh,
        // watched_folders, mcp_servers) that is still recoverable.
        Err(e) if SetupConfig::exists() => {
            return Err(format!(
                "config.toml exists but can't be parsed — refusing to overwrite it. \
                 Fix the file (or delete it to start fresh) and retry: {e}"
            ));
        }
        Err(_) => SetupConfig {
            compute: Default::default(),
            models: ModelsSection {
                primary: primary_path.clone(),
                fast: fast_field.clone(),
                embed: embed.clone(),
                code: code.clone(),
                context_size: None,
                extra: std::collections::BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
                fim: None,
            },
            daemon: Default::default(),
            data: DataSection { dir: data_dir },
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        },
    };
    cli.models.primary = primary_path;
    cli.models.fast = fast_field;
    cli.models.embed = embed;
    cli.models.code = code;
    cli.save()
}

/// Read the configured model slots from `SetupConfig`. Fresh install (no
/// `config.toml` yet): all paths empty/null, `code_family` from
/// `DesktopConfig`. Powers the Settings "Model slots" panel.
#[tauri::command]
pub async fn get_setup_model_slots(
    state: State<'_, Arc<AppState>>,
) -> Result<SetupModelSlots, String> {
    let code_family = state.config.read().await.code_family.clone();
    let show = |p: &std::path::Path| p.display().to_string();
    match sovereign_core::setup_config::SetupConfig::load() {
        Ok(cfg) => {
            let m = &cfg.models;
            Ok(SetupModelSlots {
                // EXPLICIT fast only — empty when subsumed by primary.
                // Returning the resolved `fast_path()` here materializes
                // the subsumed value: the panel would echo the OLD primary
                // back through `set_setup_model_slots` after a primary
                // edit, pinning it as an explicit always-resident fast
                // slot. The panel renders the subsumed case as "shares
                // the Main responder model" instead.
                fast: m.fast.as_deref().map(show).unwrap_or_default(),
                primary: Some(show(&m.primary)),
                embed: Some(show(&m.embed)).filter(|s| !s.is_empty()),
                code: m.code.as_deref().map(show),
                code_family,
            })
        }
        Err(_) => Ok(SetupModelSlots {
            fast: String::new(),
            primary: None,
            embed: None,
            code: None,
            code_family,
        }),
    }
}

/// Persist the configured model slots to `SetupConfig`'s `[models]` (the
/// single source of truth), stash `code_family` back on `DesktopConfig`,
/// kick a best-effort background daemon reload, then tear down the
/// desktop-embedded inference + Runtime so the next bootstrap reads the
/// fresh slots. Same teardown contract as `set_setup_context_size` — else
/// the running provider keeps serving the old weights until restart.
#[tauri::command]
pub async fn set_setup_model_slots(
    state: State<'_, Arc<AppState>>,
    slots: SetupModelSlots,
) -> Result<(), String> {
    use std::path::PathBuf;
    let to_path = |s: Option<String>| -> Option<PathBuf> {
        s.map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };

    let fast = to_path(Some(slots.fast));
    let primary = to_path(slots.primary);
    // Empty embed is allowed (library search disabled) — stored as an
    // empty path, matching the old "not set" affordance.
    let embed = to_path(slots.embed).unwrap_or_default();
    let code = to_path(slots.code);

    let data_dir = state.config.read().await.data_dir.clone();
    let path = write_model_slots_to_setup(fast, primary, embed, code, data_dir)?;
    tracing::info!(
        target = %path.display(),
        "set_setup_model_slots: SetupConfig [models] written"
    );

    // Persist the code-slot family hint (stays on DesktopConfig).
    {
        let mut cfg = state.config.write().await;
        cfg.code_family = slots.code_family;
        if let Err(e) = cfg.save() {
            tracing::warn!(
                error = %e,
                "set_setup_model_slots: failed to persist code_family to desktop.toml"
            );
        }
    }

    let reload_base = state.client_base_url();
    tokio::spawn(async move {
        if let Err(e) = request_daemon_reload(reload_base).await {
            tracing::warn!(
                error = %e,
                "set_setup_model_slots: daemon admin/reload failed (background)"
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

/// Pull-based readiness probe — the race-safe complement to the
/// push-only `backend-ready` Tauri event (emitted once from main.rs
/// after `bootstrap_with_progress`).
///
/// The native Tauri event system has NO replay: only listeners
/// registered at emit time receive an event. In Attach mode boot
/// finishes in ~1.4s, which can beat the webview's JS mount +
/// `initEventListeners` subscription — the `backend-ready` emit is then
/// lost and `App.svelte` hangs on the loading splash forever (there is
/// no timeout or re-probe). The command-bridge sticky buffer only
/// covers the Playwright harness, not the real webview.
///
/// `state.runtime` is set to `Some` inside `bootstrap_with_progress`
/// immediately before that emit, so `is_some()` is true exactly when
/// (or after) the event fired. The frontend calls this on mount, after
/// wiring its listeners, to catch a `backend-ready` it may have missed.
#[tauri::command]
pub async fn is_backend_ready(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.runtime.read().await.is_some())
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
    primary_file: Option<String>,
    // Optional BYOM override from the Setup Plan "Advanced" affordance. An
    // omitted arg deserializes to `None` (Tauri maps missing Option args),
    // so the existing frontend call stays valid until the UI is wired.
    primary_source: Option<crate::setup_flow::PrimarySource>,
) -> Result<(), String> {
    crate::setup_flow::run(
        app_handle,
        state.inner().clone(),
        primary_file,
        primary_source,
    )
    .await
}

/// Read the machine-readable setup report written at the end of setup
/// (`~/.sovereign/setup-report.json`). Powers the "What setup did" panel in
/// Settings → About. Returns the raw JSON string, or `None` if setup hasn't
/// run / the report is absent. The companion `setup-report.md` sits beside it
/// on disk for direct inspection.
#[tauri::command]
pub async fn get_setup_report() -> Result<Option<String>, String> {
    let path = dirs::home_dir()
        .map(|h| h.join(".sovereign").join("setup-report.json"))
        .ok_or_else(|| "could not resolve home directory".to_string())?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
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
    // Resolve the model picks: the wizard DTO when present, else what the
    // CLI (`sovereign setup`) already wrote to config.toml. Model PATHS go
    // straight to SetupConfig (the single source of truth) below — they are
    // no longer DesktopConfig fields.
    let cli_cfg = sovereign_core::setup_config::SetupConfig::load().ok();
    let fast: Option<std::path::PathBuf> = if !setup.model_path.is_empty() {
        Some(setup.model_path.clone().into())
    } else {
        cli_cfg.as_ref().map(|c| c.models.fast_path().to_path_buf())
    };
    let primary: Option<std::path::PathBuf> = setup
        .primary_model_path
        .clone()
        .map(std::path::PathBuf::from)
        .or_else(|| cli_cfg.as_ref().map(|c| c.models.primary.clone()));
    let embed: std::path::PathBuf = setup
        .embed_model_path
        .clone()
        .map(std::path::PathBuf::from)
        .or_else(|| cli_cfg.as_ref().map(|c| c.models.embed.clone()))
        .unwrap_or_default();
    // The DTO carries no code slot — preserve any existing one.
    let code: Option<std::path::PathBuf> = cli_cfg.as_ref().and_then(|c| c.models.code.clone());

    let mut config = state.config.write().await;
    if let Some(dir) = setup.data_dir.clone() {
        config.data_dir = dir.into();
    } else if let Some(c) = cli_cfg.as_ref() {
        config.data_dir = c.data.dir.clone();
    }
    let data_dir = config.data_dir.clone();
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

    // Write the model slots to config.toml FIRST — before setup_complete
    // persists below and before `state::bootstrap` reads them via
    // `ResolvedModelSlots`. Fatal, matching `setup_flow::run`'s treatment
    // of the same write, and ordered so a failed write never leaves
    // setup_complete=true on disk with no models configured (a confusing
    // setup→fail→setup loop instead of a clear error in the wizard).
    write_model_slots_to_setup(fast, primary, embed, code, data_dir)
        .map_err(|e| format!("write model slots to config.toml: {e}"))?;

    config.save()?;
    let config_for_mirror = config.clone();
    drop(config);
    if let Err(e) = mirror_to_setup_config(&config_for_mirror).await {
        tracing::warn!("complete_setup: could not mirror shared fields to SetupConfig: {e}");
    }
    if let Err(e) = request_daemon_reload(state.client_base_url()).await {
        tracing::warn!("complete_setup: admin/reload failed: {e}");
    }

    // First-session supervision (DAEMON_RESILIENCE.md P0.1): relaunch
    // so the fresh instance boots straight into the supervised child —
    // this session never bound :9741, so there is nothing to hand
    // over. On `false` (harnesses / kill-switch / spawn failure) keep
    // the legacy in-process completion below.
    if crate::supervisor_setup::maybe_restart_into_supervised(&app_handle).await {
        return Ok(());
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
