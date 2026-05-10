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

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use sovereign_inference::hardware::{self, HardwareProfile};
use sovereign_inference::setup_planner::{
    download_gguf, hf_download_url, recommended_primary, resolve_slot, SlotKind,
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

/// Run the full auto-setup flow. Returns Ok when the backend is
/// fully bootstrapped and ready to serve chat; returns Err with a
/// short diagnosis on any unrecoverable failure (the UI also
/// receives a `Failed` `setup-progress` event with the same
/// message before the error returns).
pub async fn run(app: AppHandle, state: Arc<AppState>) -> Result<(), String> {
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

    // ── 2. Resolve catalog ───────────────────────────────────────
    let primary_slot = recommended_primary(&profile).ok_or_else(|| {
        failed(
            &app,
            false,
            "bundled manifest has no primary candidate for this hardware".into(),
        )
    })?;
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
    // visible to anyone curling `~/.sovereign` mid-setup.
    let _ = std::fs::create_dir_all(data_dir.join("indexes"));
    let _ = std::fs::create_dir_all(data_dir.join("recipes"));

    // ── 4. Sequential downloads ──────────────────────────────────
    //
    // Each slot resolves to (a) the user's existing DesktopConfig
    // path if it already points at a valid GGUF for this slot, or
    // (b) the canonical `~/.sovereign/models/<slot.file>` location.
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
    let primary_path = pick_path(
        existing_config.primary_model_path.as_deref(),
        models_dir.join(&primary_slot.file),
        primary_slot.size_gb,
    );
    let fast_path = pick_path(
        Some(&existing_config.model_path),
        models_dir.join(&fast_slot.file),
        fast_slot.size_gb,
    );
    let embed_path = pick_path(
        existing_config.embed_model_path.as_deref(),
        models_dir.join(&embed_slot.file),
        embed_slot.size_gb,
    );

    if !is_valid_gguf_at(&primary_path) {
        download_with_progress_events(
            &app,
            &hf_download_url(&primary_slot),
            &primary_path,
            primary_slot.size_gb,
            SetupPhase::DownloadingPrimary {
                mb_total: Some((primary_slot.size_gb * 1024.0).round() as u64),
            },
            "Downloading the main responder.",
        )
        .await?;
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

    // ── 5. Persist DesktopConfig ────────────────────────────────
    {
        let mut config = state.config.write().await;
        // The "fast" slot is what the desktop loads as model_path
        // (the always-resident chat model); primary is the lazy-
        // loaded thoughtful slot.
        config.model_path = fast_path.clone();
        config.primary_model_path = Some(primary_path.clone());
        config.embed_model_path = Some(embed_path.clone());
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

    // ── 6. Bootstrap with progress narration ────────────────────
    let app_for_cb = app.clone();
    let cb: state::BootstrapProgressCb = Box::new(move |phase: BootstrapPhase| {
        let (sp, msg) = match phase {
            BootstrapPhase::SmokeTesting => {
                (SetupPhase::SmokeTesting, "Testing the connection.")
            }
            BootstrapPhase::LoadingModel => {
                (SetupPhase::LoadingModel, "Bringing the model online.")
            }
            BootstrapPhase::OpeningDatabase => {
                (SetupPhase::OpeningDatabase, "Opening your library.")
            }
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

/// Write `~/.sovereign/first_run_complete` with an ISO-8601
/// timestamp. Mirrors the existing helper in `enrich_commands.rs`
/// — we duplicate one short function rather than reach into a
/// sibling module's private state.
fn write_first_run_marker() -> Result<(), String> {
    let path = sovereign_root().join("first_run_complete");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, ts)
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

fn sovereign_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign")
}

/// Resolve the destination path for a model slot. Prefer the
/// caller's existing config path when it points at a valid GGUF
/// (BYOM placements, dev re-runs); otherwise fall back to the
/// canonical `~/.sovereign/models/<slot.file>` location, where
/// the downloader will fetch + validate as usual.
fn pick_path(
    existing: Option<&Path>,
    canonical: PathBuf,
    _size_gb: f64,
) -> PathBuf {
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
