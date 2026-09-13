// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-config first-launch orchestrator (the *lazy sunbeam* flow).
//!
//! Narrates the whole first run onto one `setup-progress` Tauri event channel
//! so `SetupFlow.svelte` can render one sentence + one progress rule at a
//! time, then relaunches into a session that has a daemon.
//!
//! # What it stopped doing (sv-surface svt-7, 2026-09-12)
//!
//! It used to BE the wizard: hardware probe, catalog resolve, three GGUF
//! downloads through `setup_planner::download_gguf`, and a `config.toml`
//! write — a second implementation of `svrn setup`, in a process that owns
//! neither the weights nor the config. The two drifted where you would
//! expect: the CLI asked the user to pick a primary and this did not, and
//! only one of them knew about `--repair`.
//!
//! The sidecar's `setup` verb runs it now. This module resolves the user's
//! pick into a `--primary` spec, spawns `setup --yes --json`
//! (`crate::setup_plan`), maps each `SetupProgressLine` onto the SetupPhase
//! frames the UI already renders, and owns the three things that are the
//! APP's: the DesktopConfig it keeps beside `config.toml`, the first-run
//! marker, and the relaunch.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_contracts::daemon_wire::{SetupProgressLine, SetupProgressPhase, SlotConfig};
use tauri::{AppHandle, Emitter};

use crate::state::{self, AppState, BootstrapPhase};

/// One frame of the setup narration the UI consumes. Always exactly
/// one phase + one sentence; download phases include `fraction` and
/// `eta_seconds` (deemphasized in the UI). Last event wins — the
/// frontend doesn't queue or merge.
#[derive(Serialize, Clone, Debug)]
pub struct SetupProgress {
    pub phase: SetupPhase,
    pub message: String,
    pub fraction: Option<f64>,
    pub eta_seconds: Option<u64>,
    pub indeterminate: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SetupPhase {
    DetectingHardware,
    PreparingDataDir,
    DownloadingPrimary { mb_total: Option<u64> },
    DownloadingFast,
    DownloadingEmbed,
    OpeningDatabase,
    LoadingModel,
    SmokeTesting,
    Ready,
    Failed { recoverable: bool },
}

const EVENT: &str = "setup-progress";

/// Where onboarding should source the **primary** (thoughtful) model when
/// the user opts out of the recommended catalog pick — sent from the Setup
/// Plan "Advanced — bring your own" affordance. `None` (the common path)
/// keeps the hardware recommendation / catalog choice untouched.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrimarySource {
    /// An existing GGUF already on disk (Browse… or a typed path). It is
    /// validated and used in place — never downloaded, never moved.
    LocalPath { path: String },
    /// A direct link to a `.gguf` file: a HuggingFace resolve/blob link, an
    /// HF quant page's `?show_file_info=<file>.gguf` URL, or any raw host.
    Url { url: String },
}

/// Run the full auto-setup flow. Returns Ok when the backend is fully
/// bootstrapped (or the app is relaunching into one); returns Err with a short
/// diagnosis on any unrecoverable failure — the UI also receives a `Failed`
/// `setup-progress` event with the same message before the error returns.
///
/// The downloads and the `config.toml` write belong to the sidecar's `setup`
/// verb. What stays here is what the APP owns: which model the user picked,
/// the DesktopConfig, the first-run marker, and the relaunch.
pub async fn run(
    app: AppHandle,
    state: Arc<AppState>,
    preferred_primary_file: Option<String>,
    primary_source: Option<PrimarySource>,
) -> Result<(), String> {
    emit_indet(
        &app,
        SetupPhase::DetectingHardware,
        "Reading what this machine can do.",
    );

    // ── The user's pick, as one `--primary` spec ──────────────────
    //
    // The three shapes the onboarding offers collapse to one flag the setup
    // verb already understands (`setup_cmd::resolve_primary_flag`): a catalog
    // file name, a pasted `.gguf` URL, or a `.gguf` already on disk. Resolving
    // them HERE and validating them THERE is deliberate — the validation is
    // the same in both entry points because there is only one of it now.
    let primary: Option<String> = match &primary_source {
        Some(PrimarySource::LocalPath { path }) => Some(path.trim().to_string()),
        Some(PrimarySource::Url { url }) => Some(url.trim().to_string()),
        None => preferred_primary_file.clone(),
    };

    let data_dir = state.config.read().await.data_dir.clone();

    // ── Run it ───────────────────────────────────────────────────
    let app_for_lines = app.clone();
    let terminal = crate::setup_plan::spawn_setup_run(&data_dir, primary.as_deref(), move |line| {
        let _ = app_for_lines.emit(EVENT, frame_for(line));
    })
    .await
    .map_err(|e| failed(&app, true, e))?;
    tracing::info!(
        config_path = terminal.config_path.as_deref().unwrap_or("<unreported>"),
        "setup_flow: the sidecar's setup verb finished"
    );

    // ── The DesktopConfig beside it ──────────────────────────────
    //
    // `config.toml` is the sidecar's and is already written. This is the
    // app's own file, and the two fields below have never been in it.
    {
        let mut config = state.config.write().await;
        config.setup_complete = true;
        // Populated via `default_enabled_tools` — leave them alone unless
        // explicitly empty. This branch used to carry its own four-member
        // literal that omitted `knowledge_lookup`, so completing setup with an
        // empty list silently dropped a tool documented as default-on. Both
        // sides derive from `ToolFamily::ALL` now (ARCH principle 8).
        if config.enabled_tools.is_empty() {
            config.enabled_tools = sovereign_contracts::tool_bundle::ToolFamily::ALL
                .iter()
                .map(|f| f.wire_id().to_string())
                .collect();
        }
        config
            .save()
            .map_err(|e| failed(&app, false, format!("save config: {e}")))?;
    }
    {
        let desktop_cfg = state.config.read().await.clone();
        if let Err(e) = crate::commands::mirror_to_setup_config(&desktop_cfg).await {
            tracing::warn!("setup_flow: could not mirror to SetupConfig: {e}");
        }
    }

    // ── Relaunch into a session that has a daemon ────────────────
    //
    // The app reaches a serving host exactly once, in
    // `serving_host::ensure_reachable`, which runs at startup — BEFORE the
    // wizard wrote `config.toml`, so this session found no config and brought
    // nothing up. The fresh instance finds it, reaches (or brings up) the
    // daemon, and attaches.
    if daemon_runs_elsewhere() {
        if let Err(e) = write_first_run_marker() {
            tracing::warn!(error = %e, "could not write first_run_complete marker");
        }
        write_setup_report(&state).await;
        let _ = app.emit(
            EVENT,
            SetupProgress {
                phase: SetupPhase::Ready,
                message: "Restarting Sovereign to finish setup\u{2026}".into(),
                fraction: Some(1.0),
                eta_seconds: None,
                indeterminate: false,
            },
        );
        relaunch_after_setup(&app).await;
        return Ok(());
    }

    // ── Bootstrap in place ──────────────────────────────────────
    //
    // The launch-topology environment asked THIS process to hold the weights
    // (`SOVEREIGN_FORCE_LOCAL=1`, the real-mode harnesses). There is no
    // relaunch to do, so narrate the bootstrap instead.
    let app_for_cb = app.clone();
    let cb: state::BootstrapProgressCb = Box::new(move |phase: BootstrapPhase| {
        let (sp, msg) = match phase {
            BootstrapPhase::SmokeTesting => (SetupPhase::SmokeTesting, "Testing the connection."),
            BootstrapPhase::LoadingModel => (SetupPhase::LoadingModel, "Bringing a model online."),
            BootstrapPhase::OpeningDatabase => (
                SetupPhase::OpeningDatabase,
                "Breaking ground on your library.",
            ),
            // The post-database phases reuse the OpeningDatabase
            // setup chip — they're sub-second in the common case and
            // don't warrant their own frontend states; the message
            // still narrates honestly for slow outliers.
            BootstrapPhase::AssemblingRouter => (SetupPhase::OpeningDatabase, "Tuning the router."),
            BootstrapPhase::RebuildingRouterEmbeddings => (
                SetupPhase::OpeningDatabase,
                "Adapting to your embedding model — one-time, this can take a few minutes.",
            ),
            BootstrapPhase::WiringKnowledge => {
                (SetupPhase::OpeningDatabase, "Connecting knowledge.")
            }
            BootstrapPhase::BuildingRuntime => (SetupPhase::OpeningDatabase, "Almost there."),
        };
        let _ = app_for_cb.emit(
            EVENT,
            SetupProgress {
                phase: sp,
                message: msg.into(),
                fraction: None,
                eta_seconds: None,
                indeterminate: true,
            },
        );
    });
    state::bootstrap_with_progress(&state, Some(cb))
        .await
        .map_err(|e| failed(&app, true, format!("bootstrap: {e}")))?;

    if let Err(e) = write_first_run_marker() {
        // Non-fatal: the user's onboarding succeeded; the marker
        // just records that fact for future relaunches. Log and
        // proceed.
        tracing::warn!(error = %e, "could not write first_run_complete marker");
    }
    write_setup_report(&state).await;

    let _ = app.emit(
        EVENT,
        SetupProgress {
            phase: SetupPhase::Ready,
            message: "Ready.".into(),
            fraction: Some(1.0),
            eta_seconds: None,
            indeterminate: false,
        },
    );
    // Legacy event — pre-existing listeners (corpus poller setup,
    // OCR install) hook this. Keep firing it so this module can
    // drop in without breaking the shell startup chain.
    let _ = app.emit("backend-ready", ());

    // Start the corpus-status poller now that the backend exists.
    // Mirrors the path in `main.rs`'s already-set-up branch.
    crate::commands::spawn_corpus_status_poller(app.clone(), Arc::clone(&state));

    Ok(())
}

/// Map one sidecar progress line onto the frame the UI already renders.
///
/// Every arm is a rename, not a decision: the `SetupProgressLine` phases were
/// chosen to be the ones `SetupPhase` already narrated, so there is no case
/// here that had to be invented and none that can go missing.
fn frame_for(line: &SetupProgressLine) -> SetupProgress {
    let phase = match line.phase {
        SetupProgressPhase::DetectingHardware => SetupPhase::DetectingHardware,
        SetupProgressPhase::PreparingDataDir => SetupPhase::PreparingDataDir,
        SetupProgressPhase::DownloadingPrimary => SetupPhase::DownloadingPrimary {
            // Megabytes, which is what the frontend's "of N MB" label reads.
            mb_total: line.total.map(|t| t / (1024 * 1024)),
        },
        SetupProgressPhase::DownloadingFast => SetupPhase::DownloadingFast,
        SetupProgressPhase::DownloadingEmbed => SetupPhase::DownloadingEmbed,
        SetupProgressPhase::WritingConfig => SetupPhase::OpeningDatabase,
        SetupProgressPhase::Done => SetupPhase::Ready,
        // `recoverable` because every failure the verb reports leaves the
        // machine re-runnable: it writes no config until the downloads
        // succeed, and its downloader resumes from the `.part` it left.
        SetupProgressPhase::Failed => SetupPhase::Failed { recoverable: true },
    };
    SetupProgress {
        phase,
        message: line.message.clone(),
        fraction: line.fraction,
        eta_seconds: line.eta_seconds,
        indeterminate: line.fraction.is_none(),
    }
}

fn emit_indet(app: &AppHandle, phase: SetupPhase, message: &str) {
    let _ = app.emit(
        EVENT,
        SetupProgress {
            phase,
            message: message.into(),
            fraction: None,
            eta_seconds: None,
            indeterminate: true,
        },
    );
}

/// Emit the `Failed` frame and return the same message the caller
/// will propagate as Err. Caller does `return Err(failed(...))?`
/// so the UI sees one final failed sentence and the Tauri command
/// also resolves with an error.
fn failed(app: &AppHandle, recoverable: bool, message: String) -> String {
    let _ = app.emit(
        EVENT,
        SetupProgress {
            phase: SetupPhase::Failed { recoverable },
            message: message.clone(),
            fraction: None,
            eta_seconds: None,
            indeterminate: false,
        },
    );
    message
}

/// Does this launch expect the daemon to be a SEPARATE process?
///
/// True by default, and false only when the launch-topology environment asked
/// THIS process to run the weights (`SOVEREIGN_FORCE_LOCAL=1`, or the
/// `SOVEREIGN_USE_SUPERVISOR=0` kill-switch) — the real-mode desktop harnesses
/// and the run-local-while-a-daemon-is-up case.
///
/// One reader of one already-resolved decision (`launch_mode::daemon_host`,
/// published by `main` beside `Launch::parse`), not a fresh parse of the
/// environment: two sites deciding what those flags mean is exactly the §10.6
/// duplicate TOPOLOGY Phase 10 closed. `is_supervised` keeps the name it had
/// when the separate process was a supervised child; the predicate is
/// unchanged and so is every harness that sets those vars.
pub(crate) fn daemon_runs_elsewhere() -> bool {
    crate::launch_mode::daemon_host().is_supervised()
}

/// Relaunch the app so the next session starts with a config on disk.
///
/// This was `supervisor_setup::maybe_restart_into_supervised` and it relaunched
/// into a SUPERVISED topology; the reason it still exists without a supervisor
/// is that the app's one look for a serving host (`serving_host::
/// ensure_reachable`) happens at startup, before the wizard has written
/// `config.toml`. The wizard session therefore has no daemon and cannot
/// acquire one; the fresh instance can.
///
/// # Why this asks Tauri rather than spawning
///
/// It used to be `std::process::Command::new(current_exe()).spawn()` followed
/// by `exit(0)`, and sv-surface's lifecycle census counted that as a thin
/// surface starting a process (`quality/ARCH_LAYERS.toml`, the row this
/// deletes). `AppHandle::request_restart` grants no ability the census is
/// refusing — it restarts THIS app and can address nothing else — and it is
/// strictly better at the job in three ways the hand-rolled version got wrong:
///
/// - **macOS bundles.** `current_exe()` inside a `.app` is
///   `Contents/MacOS/<binary>`; spawning that path directly re-launches the
///   executable outside its bundle. Tauri reads `CFBundleExecutable` out of
///   `Contents/Info.plist` and launches the bundle
///   (`tauri-2.11.1/src/process.rs:92-128`).
/// - **Argv.** The old spawn passed NONE, so a launch carrying arguments lost
///   them across setup. Tauri forwards `args_os[1..]` (`process.rs:83`).
/// - **Teardown.** `cleanup_before_exit` runs (`app.rs:606`); the old path
///   skipped it, taking the tray icon and any window state with it.
///
/// It returns `()` rather than the old `bool`: the caller's `false` branch
/// existed for `spawn()` failing, and there is no longer a spawn to fail.
/// `request_restart` sets the restart flag and asks the event loop to exit,
/// falling back to an immediate restart if that request cannot be delivered
/// (`app.rs:621-624`) — so every path leaves through a restart and the caller
/// must never bootstrap in-process after calling this.
///
/// What it does NOT do is make the relaunch unnecessary. That needs the app's
/// serving-host look to happen AFTER the wizard writes `config.toml`, which
/// means re-resolving `bootstrap_mode` — `main.rs` and `state.rs`, neither of
/// which this change owns.
pub(crate) async fn relaunch_after_setup(app_handle: &AppHandle) {
    // Let the wizard UI say why the window is about to close, and give the
    // webview a beat to paint it.
    let _ = app_handle.emit(
        "setup-restarting",
        serde_json::json!({ "reason": "connecting to your local backend" }),
    );
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    tracing::info!("setup complete — relaunching so the new config is read at startup");
    app_handle.request_restart();
}

/// Write `~/.svrnmesh/first_run_complete` with an ISO-8601
/// timestamp. Mirrors the existing helper in `enrich_commands.rs`
/// — we duplicate one short function rather than reach into a
/// sibling module's private state.
fn write_first_run_marker() -> Result<(), String> {
    let path = sovereign_root().join("first_run_complete");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, ts).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

// ─── Setup report (glassbox: "what setup did") ────────────────────

#[derive(Serialize)]
struct ReportModel {
    role: String,
    name: String,
    file: String,
    quant: String,
    size_gb: f64,
    repo: String,
    dest: String,
}

#[derive(Serialize)]
struct ReportHardware {
    effective_memory_gb: f64,
    is_unified_memory: bool,
}

#[derive(Serialize)]
struct SetupReport {
    schema_version: u32,
    completed_at: String,
    completed_at_unix: i64,
    hardware: ReportHardware,
    profile: String,
    primary_customized: bool,
    models: Vec<ReportModel>,
    smoke_passed: bool,
}

fn slot_repo(s: &SlotConfig) -> String {
    s.hf_url
        .trim_start_matches("https://huggingface.co/")
        .trim_start_matches("http://huggingface.co/")
        .trim_end_matches('/')
        .to_string()
}

/// Write a human + machine readable record of what setup did to
/// `~/.svrnmesh/setup-report.{json,md}` — mirroring the drift report's
/// dual-write so a fresh install is auditable after the fact (glassbox).
/// Best-effort: any write failure is logged, never fatal (onboarding has
/// already succeeded by the time this runs).
///
/// Assembled AFTER the run rather than during it, from two things that are
/// facts by then rather than intentions: the plan (whichever side answered
/// it) and the slots `config.toml` actually holds. It used to be built from
/// whatever the local downloader had in scope, which is how it could describe
/// a download that had been skipped.
async fn write_setup_report(state: &Arc<AppState>) {
    let base = state.client_base_url();
    let (hardware, profile) = match crate::setup_plan::hardware(&base).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "setup-report: no hardware answer — report skipped");
            return;
        }
    };
    let slots = crate::state::ResolvedModelSlots::load_or_default();
    // Describe each configured slot by the catalog row that names its file
    // when there is one, and by the file itself when the user brought their
    // own. A BYOM pick has no manifest row, and inventing one would put a size
    // and a repo on a model nobody published.
    //
    // A read that FAILS is not an empty catalog. With no rows, `describe`
    // below falls through to `quant: "custom"` for every slot — so a daemon
    // that did not answer would produce a report calling three manifest
    // models "custom", which reads as a fact and is not one. Skip the report
    // instead, the same as the hardware branch above (ARCH principle 6).
    let catalog = match crate::setup_plan::catalog(&base, None).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "setup-report: no catalog answer — report skipped");
            return;
        }
    };
    // A slot the manifest does not define for this tier is legitimately
    // `None`; only the ERROR is fatal to the report.
    let embed_slot = match crate::setup_plan::slot(&base, "embed", None).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "setup-report: no embed-slot answer — report skipped");
            return;
        }
    };
    let fast_slot = match crate::setup_plan::slot(&base, "fast", None).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "setup-report: no fast-slot answer — report skipped");
            return;
        }
    };
    let describe = |path: &std::path::Path| -> SlotConfig {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        catalog
            .iter()
            .map(|o| &o.slot)
            .chain(fast_slot.iter())
            .chain(embed_slot.iter())
            .find(|sc| sc.file == name)
            .cloned()
            .unwrap_or(SlotConfig {
                file: name.clone(),
                base_name: name,
                quant: "custom".to_string(),
                ..Default::default()
            })
    };

    let mut rows: Vec<(&str, SlotConfig, PathBuf)> = Vec::new();
    if let Some(p) = slots.primary.as_ref() {
        rows.push(("primary", describe(p), p.clone()));
    }
    if !slots.fast.as_os_str().is_empty() {
        rows.push(("fast", describe(&slots.fast), slots.fast.clone()));
    }
    if slots.has_embed() {
        rows.push(("embed", describe(&slots.embed), slots.embed.clone()));
    }

    let now = chrono::Utc::now();
    let report = SetupReport {
        schema_version: 1,
        completed_at: now.to_rfc3339(),
        completed_at_unix: now.timestamp(),
        hardware: ReportHardware {
            effective_memory_gb: hardware.effective_vram_gb() as f64,
            is_unified_memory: hardware.is_unified_memory,
        },
        profile: profile.as_str().to_string(),
        primary_customized: rows
            .iter()
            .any(|(role, slot, _)| *role == "primary" && slot.quant == "custom"),
        models: rows
            .iter()
            .map(|(role, slot, path)| ReportModel {
                role: (*role).to_string(),
                name: if slot.base_name.is_empty() {
                    slot.file.clone()
                } else {
                    slot.base_name.clone()
                },
                file: slot.file.clone(),
                quant: slot.quant.clone(),
                size_gb: slot.size_gb,
                repo: slot_repo(slot),
                dest: path.display().to_string(),
            })
            .collect(),
        smoke_passed: true,
    };

    let dir = sovereign_root();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, "setup-report: mkdir failed");
        return;
    }
    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            let p = dir.join("setup-report.json");
            if let Err(e) = std::fs::write(&p, json) {
                tracing::warn!(error = %e, path = %p.display(), "setup-report: json write failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, "setup-report: serialize failed"),
    }
    let p = dir.join("setup-report.md");
    if let Err(e) = std::fs::write(&p, render_setup_report_md(&report)) {
        tracing::warn!(error = %e, path = %p.display(), "setup-report: md write failed");
    }
    tracing::info!(dir = %dir.display(), "setup-report written");
}

fn render_setup_report_md(r: &SetupReport) -> String {
    let mut s = String::new();
    s.push_str("# svrnmesh — setup report\n\n");
    s.push_str(&format!("Completed: {}\n\n", r.completed_at));
    s.push_str(&format!(
        "Hardware: {:.0} GB {} · profile `{}`\n\n",
        r.hardware.effective_memory_gb,
        if r.hardware.is_unified_memory {
            "unified memory"
        } else {
            "GPU / RAM"
        },
        r.profile,
    ));
    s.push_str("## Models installed\n\n");
    for m in &r.models {
        s.push_str(&format!(
            "- **{}** — {} ({}, {:.1} GB) from `{}` -> `{}`\n",
            m.role, m.name, m.quant, m.size_gb, m.repo, m.dest,
        ));
    }
    s.push_str(if r.primary_customized {
        "\nPrimary model: customized by you at setup.\n"
    } else {
        "\nPrimary model: hardware-recommended default.\n"
    });
    s.push_str(
        "\nChange models in Settings -> Models. This report lives at \
         `~/.svrnmesh/setup-report.{json,md}`.\n",
    );
    s
}

fn sovereign_root() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root()
}
