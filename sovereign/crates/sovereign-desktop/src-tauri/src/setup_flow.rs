// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-config first-launch orchestrator (the *lazy sunbeam* flow).
//!
//! Runs the entire setup chain — hardware probe → catalog resolve
//! → data dir → 3 sequential GGUF downloads → DesktopConfig persist
//! → bootstrap (DB open + model load + smoke test) → first-run
//! marker — with no user input. Streams a single `setup-progress`
//! Tauri event channel throughout so the desktop's `SetupFlow.svelte`
//! can render one sentence + one progress rule at a time.
//!
//! The CLI's `sovereign setup` flow is a separate code path that
//! still asks the user to pick a primary; this module is the
//! desktop's no-decisions version. The download / catalog / GGUF
//! validation logic is shared via `sovereign_inference::setup_planner`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use sovereign_core::models_manifest::SlotConfig;
use sovereign_inference::hardware::{self, HardwareProfile};
use sovereign_inference::setup_planner::{
    build_primary_catalog, download_gguf, hf_download_url, recommended_primary, resolve_slot,
    SlotKind,
};
use sovereign_inference::GgufExpectation;

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

/// A resolved download instruction for the primary slot (internal to `run`).
/// Unifies the manifest-slot path and the BYOM-URL path so the download
/// site below has one shape to consume.
struct PrimaryDownload {
    url: String,
    size_gb: f64,
    mb_total: Option<u64>,
    sentence: &'static str,
}

/// Run the full auto-setup flow. Returns Ok when the backend is
/// fully bootstrapped and ready to serve chat; returns Err with a
/// short diagnosis on any unrecoverable failure (the UI also
/// receives a `Failed` `setup-progress` event with the same
/// message before the error returns).
pub async fn run(
    app: AppHandle,
    state: Arc<AppState>,
    preferred_primary_file: Option<String>,
    primary_source: Option<PrimarySource>,
) -> Result<(), String> {
    // ── 1. Hardware probe ────────────────────────────────────────
    emit_indet(
        &app,
        SetupPhase::DetectingHardware,
        "Reading what this machine can do.",
    );
    let hw = tokio::task::spawn_blocking(HardwareProfile::detect)
        .await
        .map_err(|e| failed(&app, false, format!("hardware detect panicked: {e}")))?;
    let profile = hardware::select_profile(&hw);

    // ── 2. Resolve slots ─────────────────────────────────────────
    // Fast + embed come straight from the hardware profile. The primary
    // (thoughtful) model is resolved in step 4 (BYOM-aware): the catalog
    // pick / hardware recommendation by default, or the onboarding
    // "Advanced" override (a local GGUF path, or a pasted .gguf URL).
    // Nothing is fetched until the download phase — nothing was pulled
    // before the user consented.
    let fast_slot = resolve_slot(&profile, SlotKind::Fast).ok_or_else(|| {
        failed(
            &app,
            false,
            "bundled manifest is missing a fast slot for this hardware".into(),
        )
    })?;
    let embed_slot = resolve_slot(&profile, SlotKind::Embed).ok_or_else(|| {
        failed(
            &app,
            false,
            "bundled manifest is missing an embed slot for this hardware".into(),
        )
    })?;

    // ── 3. Prepare data dir ──────────────────────────────────────
    emit_indet(
        &app,
        SetupPhase::PreparingDataDir,
        "Preparing your storage.",
    );
    let existing_config = state.config.read().await.clone();
    // Prior model picks now live in SetupConfig (config.toml). Resolve them
    // so `pick_path` can reuse a valid existing GGUF instead of re-fetching.
    let existing_slots = crate::state::ResolvedModelSlots::load_or_default();
    let data_dir = existing_config.data_dir.clone();
    let models_dir = data_dir.join("models");
    if let Err(e) = std::fs::create_dir_all(&models_dir) {
        return Err(failed(
            &app,
            false,
            format!("could not create {}: {e}", models_dir.display()),
        ));
    }
    // The DB and indexes dirs get created by bootstrap when needed,
    // but seeding them here keeps the first-run filesystem layout
    // visible to anyone curling `~/.svrnmesh` mid-setup.
    let _ = std::fs::create_dir_all(data_dir.join("indexes"));
    let _ = std::fs::create_dir_all(data_dir.join("recipes"));

    // ── 4. Sequential downloads ──────────────────────────────────
    //
    // Each slot resolves to (a) the user's existing DesktopConfig
    // path if it already points at a valid GGUF for this slot, or
    // (b) the canonical `~/.svrnmesh/models/<slot.file>` location.
    // Reusing existing paths matters for two cases: BYOM users who
    // manually placed a GGUF outside the canonical dir, and dev
    // workflows (e.g. `SOVEREIGN_DEV_FORCE_SETUP=1`) where we want
    // SetupFlow to play through visually without re-downloading.
    //
    // When a path resolves to a file that's already valid, we skip
    // the download phase *entirely* — no UI frame, no "Downloading
    // X" sentence flashing through to 100%. The user moves directly
    // to the next phase (preparing data dir → opening database).
    // Only slots that genuinely need bytes pulled get a frame.
    // Resolve the primary (thoughtful) model. A BYOM override from the
    // onboarding "Advanced" affordance wins: a local GGUF is used in place
    // (never downloaded), a pasted URL is fetched to the canonical models
    // dir. With no override we keep the catalog/recommendation pick + the
    // pick_path reuse-existing-valid-GGUF behaviour exactly as before.
    // Also yield a `SlotConfig` for the primary in every case — real from the
    // manifest, or synthetic for a BYOM pick — so the setup report (step 7b)
    // describes all three slots with one uniform shape.
    let (primary_path, primary_download, primary_slot): (
        PathBuf,
        Option<PrimaryDownload>,
        SlotConfig,
    ) = match &primary_source {
        Some(PrimarySource::LocalPath { path }) => {
            let p = PathBuf::from(path.trim());
            if !is_valid_gguf_at(&p) {
                return Err(failed(
                    &app,
                    false,
                    format!(
                        "the model you chose isn't a readable GGUF file: {}",
                        p.display()
                    ),
                ));
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| "your model".to_string());
            let slot = SlotConfig {
                file: name.clone(),
                base_name: name,
                quant: "custom (local)".to_string(),
                ..Default::default()
            };
            (p, None, slot)
        }
        Some(PrimarySource::Url { url }) => {
            let (dl_url, file) = resolve_byom_url(url).map_err(|e| failed(&app, false, e))?;
            let slot = SlotConfig {
                file: file.clone(),
                base_name: file.clone(),
                quant: "custom (url)".to_string(),
                hf_url: dl_url.clone(),
                ..Default::default()
            };
            (
                models_dir.join(&file),
                Some(PrimaryDownload {
                    url: dl_url,
                    // Unknown ahead of time for BYOM; the downloader still
                    // enforces the GGUF magic + 1 MB sentinel floor.
                    size_gb: 0.0,
                    mb_total: None,
                    sentence: "Downloading your model.",
                }),
                slot,
            )
        }
        None => {
            let primary_slot = preferred_primary_file
                .as_deref()
                .and_then(|file| {
                    build_primary_catalog(&profile)
                        .into_iter()
                        .find(|opt| opt.slot.file == file)
                        .map(|opt| opt.slot)
                })
                .or_else(|| recommended_primary(&profile))
                .ok_or_else(|| {
                    failed(
                        &app,
                        false,
                        "bundled manifest has no primary candidate for this hardware".into(),
                    )
                })?;
            let path = pick_path(
                existing_slots.primary.as_deref(),
                models_dir.join(&primary_slot.file),
                primary_slot.size_gb,
            );
            let dl = PrimaryDownload {
                url: hf_download_url(&primary_slot),
                size_gb: primary_slot.size_gb,
                mb_total: Some((primary_slot.size_gb * 1024.0).round() as u64),
                sentence: "Downloading the main responder.",
            };
            (path, Some(dl), primary_slot)
        }
    };
    let fast_path = pick_path(
        (!existing_slots.fast.as_os_str().is_empty()).then(|| existing_slots.fast.as_path()),
        models_dir.join(&fast_slot.file),
        fast_slot.size_gb,
    );
    let embed_path = pick_path(
        existing_slots
            .has_embed()
            .then(|| existing_slots.embed.as_path()),
        models_dir.join(&embed_slot.file),
        embed_slot.size_gb,
    );

    // A local BYOM pick has `primary_download == None` (already validated,
    // nothing to fetch); every other case downloads only if the resolved
    // path isn't already a valid GGUF on disk.
    if let Some(dl) = &primary_download {
        if !is_valid_gguf_at(&primary_path) {
            download_with_progress_events(
                &app,
                &dl.url,
                &primary_path,
                dl.size_gb,
                SetupPhase::DownloadingPrimary {
                    mb_total: dl.mb_total,
                },
                dl.sentence,
            )
            .await?;
        }
    }

    if !is_valid_gguf_at(&fast_path) {
        download_with_progress_events(
            &app,
            &hf_download_url(&fast_slot),
            &fast_path,
            fast_slot.size_gb,
            SetupPhase::DownloadingFast,
            "Downloading the quick responder.",
        )
        .await?;
    }

    if !is_valid_gguf_at(&embed_path) {
        download_with_progress_events(
            &app,
            &hf_download_url(&embed_slot),
            &embed_path,
            embed_slot.size_gb,
            SetupPhase::DownloadingEmbed,
            "Downloading the knowledge embedder.",
        )
        .await?;
    }

    // ── 5. Persist the model slots to SetupConfig (config.toml), then
    //       the non-model DesktopConfig fields. ──
    //
    // Model PATHS are the sole province of `~/.svrnmesh/config.toml` (the
    // single source of truth the daemon reads). Write them FIRST — step 6's
    // bootstrap resolves them via `ResolvedModelSlots::load()`, so a missing
    // write here would leave the wizard "complete" but the daemon reading
    // stale/absent paths (the original desktop.toml-vs-config.toml divergence).
    // fast/primary are distinct GGUFs here, so the subsume rule keeps both.
    crate::commands::write_model_slots_to_setup(
        Some(fast_path.clone()),
        Some(primary_path.clone()),
        embed_path.clone(),
        // The auto-setup flow configures no code slot of its own, but a
        // wizard RE-run (e.g. after a stale fast model reset setup_complete)
        // must not wipe one the user configured via Settings or the CLI.
        existing_slots.code.clone(),
        data_dir.clone(),
    )
    .map_err(|e| {
        failed(
            &app,
            false,
            format!("write model slots to config.toml: {e}"),
        )
    })?;

    {
        let mut config = state.config.write().await;
        config.setup_complete = true;
        // The default tools (`shell`, `search`, `web_fetch`,
        // `document`) are already populated via `default_enabled_tools`
        // — leave them alone unless they're explicitly empty.
        if config.enabled_tools.is_empty() {
            config.enabled_tools = vec![
                "shell".into(),
                "search".into(),
                "web_fetch".into(),
                "document".into(),
            ];
        }
        config
            .save()
            .map_err(|e| failed(&app, false, format!("save config: {e}")))?;
    }

    // ── 5b. First-session supervision (DAEMON_RESILIENCE.md P0.1) ──
    // Mirror the wizard's picks into the shared `SetupConfig` (this
    // flow historically relied on a later `save_config` to do it — but
    // the relaunched instance needs it NOW to take the supervised
    // path), write the first-run marker + setup report (they must
    // survive the relaunch), then restart the app so it boots straight
    // into the supervised child daemon. This session has never bound
    // :9741, so the fresh instance finds a free port and a complete
    // config. Falls through to the legacy in-process bootstrap when
    // supervision is disabled (FORCE_LOCAL harnesses / kill-switch).
    {
        let desktop_cfg = state.config.read().await.clone();
        if let Err(e) = crate::commands::mirror_to_setup_config(&desktop_cfg).await {
            tracing::warn!("setup_flow: could not mirror to SetupConfig: {e}");
        }
    }
    if crate::supervisor_setup::is_enabled() {
        if let Err(e) = write_first_run_marker() {
            tracing::warn!(error = %e, "could not write first_run_complete marker");
        }
        write_setup_report(
            &hw,
            &profile,
            &[
                ("primary", &primary_slot, &primary_path),
                ("fast", &fast_slot, &fast_path),
                ("embed", &embed_slot, &embed_path),
            ],
            preferred_primary_file.is_some(),
        );
        let _ = app.emit(
            EVENT,
            SetupProgress {
                phase: SetupPhase::Ready,
                message: "Restarting Sovereign to finish setup…".into(),
                fraction: Some(1.0),
                eta_seconds: None,
                indeterminate: false,
            },
        );
        if crate::supervisor_setup::maybe_restart_into_supervised(&app).await {
            return Ok(());
        }
        // Restart didn't take (spawn failure) — continue in-process;
        // the duplicate marker/report writes below are idempotent.
    }

    // ── 6. Bootstrap with progress narration ────────────────────
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

    // ── 7. First-run marker ─────────────────────────────────────
    if let Err(e) = write_first_run_marker() {
        // Non-fatal: the user's onboarding succeeded; the marker
        // just records that fact for future relaunches. Log and
        // proceed.
        tracing::warn!(error = %e, "could not write first_run_complete marker");
    }

    // ── 7b. Setup report (glassbox: an auditable record of what we did) ──
    write_setup_report(
        &hw,
        &profile,
        &[
            ("primary", &primary_slot, &primary_path),
            ("fast", &fast_slot, &fast_path),
            ("embed", &embed_slot, &embed_path),
        ],
        preferred_primary_file.is_some(),
    );

    // ── 8. Ready signals ────────────────────────────────────────
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

/// Wrapper around `setup_planner::download_gguf` that emits
/// per-chunk progress events. ETA is computed from a 4-sample
/// rolling rate.
async fn download_with_progress_events(
    app: &AppHandle,
    url: &str,
    dest: &Path,
    size_gb: f64,
    phase: SetupPhase,
    message: &str,
) -> Result<(), String> {
    // Initial frame — render the sentence immediately so the
    // UI doesn't show a stale previous-phase string while
    // we wait for the first chunk.
    let _ = app.emit(
        EVENT,
        SetupProgress {
            phase: phase.clone(),
            message: message.into(),
            fraction: None,
            eta_seconds: None,
            indeterminate: true,
        },
    );

    let expected = GgufExpectation::from_size_gb(size_gb);
    let app_for_cb = app.clone();
    let phase_for_cb = phase.clone();
    let msg_for_cb = message.to_string();
    let samples: Mutex<Vec<(Instant, u64)>> = Mutex::new(Vec::with_capacity(8));

    let cb = move |done: u64, total: Option<u64>| {
        // Maintain a 4-sample rolling rate so the ETA doesn't
        // jitter on the first few chunks.
        let now = Instant::now();
        let eta_seconds = {
            let mut s = samples.lock().unwrap();
            s.push((now, done));
            if s.len() > 4 {
                let drop = s.len() - 4;
                s.drain(..drop);
            }
            if s.len() >= 2 {
                let (t0, b0) = s[0];
                let (t1, b1) = s[s.len() - 1];
                let dt = t1.duration_since(t0).as_secs_f64();
                let db = b1.saturating_sub(b0) as f64;
                if dt > 0.0 && db > 0.0 {
                    if let Some(t) = total {
                        let remaining = t.saturating_sub(done) as f64;
                        let rate = db / dt;
                        if rate > 0.0 {
                            Some((remaining / rate).round() as u64)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        let fraction = total.and_then(|t| {
            if t > 0 {
                Some((done as f64 / t as f64).clamp(0.0, 1.0))
            } else {
                None
            }
        });
        let _ = app_for_cb.emit(
            EVENT,
            SetupProgress {
                phase: phase_for_cb.clone(),
                message: msg_for_cb.clone(),
                fraction,
                eta_seconds,
                indeterminate: fraction.is_none(),
            },
        );
    };

    download_gguf(url, dest, &expected, &cb)
        .await
        .map_err(|e| failed(app, true, e))
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

fn profile_str(p: &hardware::ProfileName) -> &'static str {
    use hardware::ProfileName::*;
    match p {
        CpuOnly => "cpu_only",
        LowMem => "low_mem",
        Default => "default",
        High => "high",
        VeryHigh => "very_high",
    }
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
fn write_setup_report(
    hw: &HardwareProfile,
    profile: &hardware::ProfileName,
    models: &[(&str, &SlotConfig, &Path)],
    primary_customized: bool,
) {
    let now = chrono::Utc::now();
    let report = SetupReport {
        schema_version: 1,
        completed_at: now.to_rfc3339(),
        completed_at_unix: now.timestamp(),
        hardware: ReportHardware {
            effective_memory_gb: hw.effective_vram_gb() as f64,
            is_unified_memory: hw.is_unified_memory,
        },
        profile: profile_str(profile).to_string(),
        primary_customized,
        models: models
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

/// Resolve the destination path for a model slot. Prefer the
/// caller's existing config path when it points at a valid GGUF
/// (BYOM placements, dev re-runs); otherwise fall back to the
/// canonical `~/.svrnmesh/models/<slot.file>` location, where
/// the downloader will fetch + validate as usual.
fn pick_path(existing: Option<&Path>, canonical: PathBuf, _size_gb: f64) -> PathBuf {
    if let Some(p) = existing {
        if !p.as_os_str().is_empty() && is_valid_gguf_at(p) {
            return p.to_path_buf();
        }
    }
    canonical
}

/// True iff `path` exists and contains something that passes the
/// GGUF magic-byte check (plus a 1 MB sentinel floor that catches
/// HTML / LFS-pointer stubs).
///
/// We deliberately do NOT cross-check against the active profile's
/// `size_gb`. The user's existing slot might be a different but
/// perfectly valid model than what the current manifest profile
/// expects (e.g. Qwen3-Embedding-0.6B locally where the new
/// `very_high` profile would download a 4B variant) — forcing
/// validation against the profile size would re-download every
/// time the manifest's recommended slot changes. Trust what's on
/// disk; let the runtime surface real load errors if anything
/// truly broken slips through.
fn is_valid_gguf_at(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    sovereign_inference::validate_gguf(path, &GgufExpectation::unknown()).is_ok()
}

/// Turn a user-pasted "bring your own model" URL into `(download_url,
/// file_name)`, or an `Err` message the UI shows verbatim.
///
/// Accepts three shapes a "user who knows what they're doing" is likely
/// to paste:
///   1. a direct `…/resolve/main/<file>.gguf` raw link (used as-is);
///   2. a `…/blob/main/<file>.gguf` browser link (HTML page — rewritten
///      to the `/resolve/` raw path);
///   3. a HuggingFace quant *page* URL of the form
///      `…/<repo>?show_file_info=<file>.gguf` (what the HF file browser
///      puts in the address bar) — rebuilt into the `/resolve/` link.
/// Anything that doesn't resolve to a `.gguf` file (a repo root, a random
/// page) is rejected with guidance rather than downloading an HTML stub.
fn resolve_byom_url(raw: &str) -> Result<(String, String), String> {
    let url = raw.trim();

    // Shape 3: HF quant page `?show_file_info=<file>.gguf`.
    if let Some((base, query)) = url.split_once('?') {
        if let Some(file) = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("show_file_info="))
        {
            let file = file.split(['&', '#']).next().unwrap_or(file);
            if file.to_ascii_lowercase().ends_with(".gguf") {
                let repo = base.trim_end_matches('/');
                return Ok((format!("{repo}/resolve/main/{file}"), file.to_string()));
            }
        }
    }

    // Shapes 1 & 2: a direct file link. `/blob/` pages are HTML; the raw
    // bytes live under `/resolve/`.
    let dl = url.replace("/blob/", "/resolve/");
    let file = dl
        .split(['?', '#'])
        .next()
        .unwrap_or(&dl)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if file.to_ascii_lowercase().ends_with(".gguf") {
        Ok((dl, file))
    } else {
        Err(format!(
            "that link doesn't point at a .gguf file — paste the direct download \
             link to the model file (or its HuggingFace quant page), not the repo \
             root: {raw}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_byom_url;

    #[test]
    fn passes_through_resolve_link() {
        let (url, file) = resolve_byom_url(
            "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/resolve/main/Qwen3.5-9B-Q4_K_M.gguf",
        )
        .unwrap();
        assert!(url.ends_with("/resolve/main/Qwen3.5-9B-Q4_K_M.gguf"));
        assert_eq!(file, "Qwen3.5-9B-Q4_K_M.gguf");
    }

    #[test]
    fn rewrites_blob_to_resolve() {
        let (url, file) = resolve_byom_url(
            "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/blob/main/Qwen3.5-9B-Q4_K_M.gguf",
        )
        .unwrap();
        assert!(url.contains("/resolve/") && !url.contains("/blob/"));
        assert_eq!(file, "Qwen3.5-9B-Q4_K_M.gguf");
    }

    #[test]
    fn handles_hf_quant_page_show_file_info() {
        let (url, file) = resolve_byom_url(
            "https://huggingface.co/unsloth/gemma-4-31B-it-GGUF?show_file_info=gemma-4-31B-it-UD-Q4_K_XL.gguf",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/unsloth/gemma-4-31B-it-GGUF/resolve/main/gemma-4-31B-it-UD-Q4_K_XL.gguf"
        );
        assert_eq!(file, "gemma-4-31B-it-UD-Q4_K_XL.gguf");
    }

    #[test]
    fn strips_query_and_fragment_from_direct_link() {
        let (_, file) =
            resolve_byom_url("https://example.com/models/foo.gguf?download=true#frag").unwrap();
        assert_eq!(file, "foo.gguf");
    }

    #[test]
    fn rejects_repo_page_and_non_gguf() {
        assert!(resolve_byom_url("https://huggingface.co/unsloth/Qwen3.5-9B-GGUF").is_err());
        assert!(resolve_byom_url("not even a url").is_err());
        assert!(resolve_byom_url("https://example.com/readme.md").is_err());
    }
}
