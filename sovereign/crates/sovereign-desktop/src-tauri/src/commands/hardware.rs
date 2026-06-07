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

// ─── Hardware Detection ─────────────────────────────────────

#[derive(Serialize)]
pub struct HardwareInfo {
    pub system_ram_gb: f64,
    pub gpu_available: bool,
    pub gpu_name: Option<String>,
    /// Discrete GPU VRAM in GB. `None` on unified-memory systems (Apple
    /// Silicon) or when no GPU is present; UI should fall back to
    /// `system_ram_gb` for tier selection.
    pub gpu_memory_gb: Option<f64>,
    /// True on Apple Silicon (M-series). Determines whether the model
    /// recommender buckets by `system_ram_gb` (unified) or `gpu_memory_gb`
    /// (discrete VRAM).
    pub is_unified_memory: bool,
}

#[tauri::command]
pub async fn detect_hardware() -> Result<HardwareInfo, String> {
    let profile =
        tokio::task::spawn_blocking(sovereign_inference::hardware::HardwareProfile::detect)
            .await
            .map_err(|e| format!("Hardware detection failed: {e}"))?;

    let gpu_memory_gb = profile
        .gpu_memory_bytes
        .map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0));

    Ok(HardwareInfo {
        system_ram_gb: profile.system_ram_gb(),
        gpu_available: profile.gpu_available,
        gpu_name: profile.gpu_name,
        gpu_memory_gb,
        is_unified_memory: profile.is_unified_memory,
    })
}

// ─── Model Recommendation Catalog ────────────────────────────
//
// Single source of truth for "what model should the user pick on this
// machine" lives in `sovereign-inference::setup_planner` +
// `models.toml`. These commands expose the same catalog the CLI uses
// to the desktop so the setup wizard and the Settings → Models tab
// don't drift. Without this, the Svelte side hand-rolls thresholds and
// hardcodes filenames — and we get to discover the drift when a user
// follows a desktop recommendation the daemon's manifest doesn't carry.
//
// All three DTOs intentionally strip OICP capability annotations: the
// UI only needs human-readable names, sizes, and the HuggingFace URL
// it has to download from. Capability routing stays on the Rust side.

/// String form of `ProfileName` matching the keys in `models.toml`
/// (`"cpu_only" / "low_mem" / "default" / "high" / "very_high"`). The
/// desktop never wants the Rust enum directly — strings round-trip
/// cleanly through JSON and let the wizard compare them to manifest
/// section names without an extra translation layer.
fn profile_name_str(p: &sovereign_inference::hardware::ProfileName) -> &'static str {
    use sovereign_inference::hardware::ProfileName;
    match p {
        ProfileName::CpuOnly => "cpu_only",
        ProfileName::LowMem => "low_mem",
        ProfileName::Default => "default",
        ProfileName::High => "high",
        ProfileName::VeryHigh => "very_high",
    }
}

fn parse_profile_name(s: &str) -> Result<sovereign_inference::hardware::ProfileName, String> {
    use sovereign_inference::hardware::ProfileName;
    match s {
        "cpu_only" => Ok(ProfileName::CpuOnly),
        "low_mem" => Ok(ProfileName::LowMem),
        "default" => Ok(ProfileName::Default),
        "high" => Ok(ProfileName::High),
        "very_high" => Ok(ProfileName::VeryHigh),
        other => Err(format!("unknown profile: {other}")),
    }
}

#[derive(Serialize)]
pub struct RecommendedProfileDto {
    pub profile: String,
    pub effective_memory_gb: f64,
    pub is_unified_memory: bool,
}

#[derive(Serialize)]
pub struct PrimaryOptionDto {
    /// `"cpu_only" / "low_mem" / "default" / "high" / "very_high"` —
    /// the profile bucket this slot belongs to. The wizard ranks
    /// recommended-first then descending model size; this field lets
    /// it group "lighter alternatives" beneath the headline pick.
    pub profile: String,
    pub recommended: bool,
    pub file: String,
    pub base_name: String,
    pub family: String,
    pub quant: String,
    pub size_gb: f64,
    pub hf_url: String,
    /// Direct GGUF download URL — `setup_planner::hf_download_url`
    /// applies the `/resolve/main/<file>` convention so the desktop
    /// can `downloadModel({ url })` without re-implementing the
    /// HuggingFace path rules.
    pub download_url: String,
}

#[derive(Serialize)]
pub struct SlotConfigDto {
    pub file: String,
    pub base_name: String,
    pub family: String,
    pub quant: String,
    pub size_gb: f64,
    pub hf_url: String,
    pub download_url: String,
}

impl From<&sovereign_core::models_manifest::SlotConfig> for SlotConfigDto {
    fn from(s: &sovereign_core::models_manifest::SlotConfig) -> Self {
        SlotConfigDto {
            file: s.file.clone(),
            base_name: s.base_name.clone(),
            family: s.family.clone(),
            quant: s.quant.clone(),
            size_gb: s.size_gb,
            hf_url: s.hf_url.clone(),
            download_url: sovereign_inference::setup_planner::hf_download_url(s),
        }
    }
}

/// Return the recommended hardware profile for this machine plus the
/// effective memory the daemon's bucket logic saw. Effective memory
/// is unified RAM on Apple Silicon, GPU VRAM on discrete cards, system
/// RAM otherwise — matching `HardwareProfile::effective_vram_gb`.
#[tauri::command]
pub async fn recommended_profile() -> Result<RecommendedProfileDto, String> {
    let profile =
        tokio::task::spawn_blocking(sovereign_inference::hardware::HardwareProfile::detect)
            .await
            .map_err(|e| format!("Hardware detection failed: {e}"))?;

    let pname = sovereign_inference::hardware::select_profile(&profile);
    let effective_memory_gb = profile.effective_vram_gb() as f64;
    Ok(RecommendedProfileDto {
        profile: profile_name_str(&pname).to_string(),
        effective_memory_gb,
        is_unified_memory: profile.is_unified_memory,
    })
}

/// Return the curated primary-model catalog for `profile` (or the
/// detected profile if `None`). Wraps `setup_planner::build_primary_catalog`
/// so a single Rust function decides which models qualify for the user's
/// tier — the desktop just renders the result.
#[tauri::command]
pub async fn primary_catalog(profile: Option<String>) -> Result<Vec<PrimaryOptionDto>, String> {
    let pname = match profile {
        Some(s) => parse_profile_name(&s)?,
        None => {
            let hw = tokio::task::spawn_blocking(|| {
                sovereign_inference::hardware::HardwareProfile::detect()
            })
            .await
            .map_err(|e| format!("Hardware detection failed: {e}"))?;
            sovereign_inference::hardware::select_profile(&hw)
        }
    };
    let catalog = sovereign_inference::setup_planner::build_primary_catalog(&pname);
    Ok(catalog
        .into_iter()
        .map(|opt| PrimaryOptionDto {
            profile: opt.profile.to_string(),
            recommended: opt.recommended,
            download_url: sovereign_inference::setup_planner::hf_download_url(&opt.slot),
            file: opt.slot.file.clone(),
            base_name: opt.slot.base_name.clone(),
            family: opt.slot.family.clone(),
            quant: opt.slot.quant.clone(),
            size_gb: opt.slot.size_gb,
            hf_url: opt.slot.hf_url.clone(),
        })
        .collect())
}

/// List the model IDs the local daemon's `/v1/models` endpoint
/// advertises. Used by the Connect tab so it can show what's
/// currently registered without the renderer making raw HTTP calls
/// across Tauri's sandbox (which fails with Safari's "Load failed").
#[tauri::command]
pub async fn list_daemon_models() -> Result<Vec<String>, String> {
    let url = "http://127.0.0.1:9741/v1/models";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET /v1/models: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse /v1/models body: {e}"))?;
    if !status.is_success() {
        return Err(format!("/v1/models returned {status}"));
    }
    let mut ids: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}

/// Pooled mesh capacity for the sidebar's runtime readout. Reads the
/// daemon's `/status` summary — the one surface that aggregates free
/// VRAM / storage across online members. We deliberately surface only
/// the numbers that are *accurate today*: `/v1/models` carries no
/// per-model residency in embedded mode, and `/status`
/// `inference.loaded_models` is mesh-plan-derived (empty on a solo
/// embedded desktop), so a true local "what's resident + bytes" table
/// (the `ollama ps` analog) is a tracked follow-up requiring a small
/// `InferenceProvider` introspection method — not surfaced here rather
/// than shown as a fabricated zero.
#[derive(Serialize, Default)]
pub struct RuntimeStatus {
    pub members_online: u32,
    pub members_total: u32,
    pub pooled_vram_gb: f32,
    pub pooled_storage_gb: f32,
}

#[tauri::command]
pub async fn get_runtime_status() -> Result<RuntimeStatus, String> {
    let url = "http://127.0.0.1:9741/status";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET /status: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse /status body: {e}"))?;
    if !status.is_success() {
        return Err(format!("/status returned {status}"));
    }
    let mesh = body.get("mesh");
    let num = |k: &str| -> f64 {
        mesh.and_then(|m| m.get(k))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    };
    Ok(RuntimeStatus {
        members_online: num("members_online") as u32,
        members_total: num("members_total") as u32,
        pooled_vram_gb: num("pooled_vram_gb") as f32,
        pooled_storage_gb: num("pooled_storage_gb") as f32,
    })
}

/// Return the recommended slot for `kind` (`"fast"` or `"embed"`) on
/// `profile` (or detected). Wraps `setup_planner::resolve_slot`. The
/// thoughtful (primary) slot has its own catalog endpoint above; the
/// fast and embed slots are single-pick.
#[tauri::command]
pub async fn slot_recommendation(
    kind: String,
    profile: Option<String>,
) -> Result<Option<SlotConfigDto>, String> {
    let pname = match profile {
        Some(s) => parse_profile_name(&s)?,
        None => {
            let hw = tokio::task::spawn_blocking(|| {
                sovereign_inference::hardware::HardwareProfile::detect()
            })
            .await
            .map_err(|e| format!("Hardware detection failed: {e}"))?;
            sovereign_inference::hardware::select_profile(&hw)
        }
    };
    let slot_kind = match kind.as_str() {
        "fast" => sovereign_inference::setup_planner::SlotKind::Fast,
        "embed" => sovereign_inference::setup_planner::SlotKind::Embed,
        other => return Err(format!("unknown slot kind: {other}")),
    };
    Ok(
        sovereign_inference::setup_planner::resolve_slot(&pname, slot_kind)
            .as_ref()
            .map(SlotConfigDto::from),
    )
}

/// Expose the result of `bootstrap::detect` to the frontend so the
/// setup wizard can skip screens that are already covered by the
/// CLI-written `SetupConfig`. Called once at app start (or any time
/// the wizard wants to re-probe, e.g. after the user runs
/// `sovereign setup` in a terminal).
#[tauri::command]
pub async fn detect_bootstrap() -> Result<crate::bootstrap::BootstrapSnapshot, String> {
    let mode = crate::bootstrap::detect().await;
    Ok(crate::bootstrap::BootstrapSnapshot::from(&mode))
}

/// Eagerly load the primary chat slot so the next chat-completions
/// call doesn't pay the lazy-load tax.
///
/// Idempotent and fire-and-forget from the UI's perspective —
/// callers don't await on the load. The frontend dispatches this
/// on window-focus and ChatView mount so the slot is hot by the
/// time the user finishes typing.
///
/// Returns immediately as `Ok(())` when no inference provider has
/// been configured yet (pre-setup wizard, model files missing) so
/// the focus handler can stay a fire-and-forget without surfacing
/// errors that aren't user-actionable.
#[tauri::command]
pub async fn warmup_primary_slot(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let provider = {
        let guard = state.inference.read().await;
        guard.as_ref().map(Arc::clone)
    };
    let Some(provider) = provider else {
        // Setup hasn't run / model files unconfigured. Fire-and-
        // forget contract — this isn't an error from the UI's
        // perspective, just nothing to warm.
        return Ok(());
    };
    // Spawn so the Tauri command returns immediately. The load can
    // take 10–90s; we don't want the focus handler to block on it.
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match provider.warmup_primary().await {
            Ok(()) => tracing::info!(
                latency_ms = started.elapsed().as_millis() as u64,
                "warmup_primary_slot: complete"
            ),
            Err(e) => tracing::warn!(error = %e, "warmup_primary_slot: failed"),
        }
    });
    Ok(())
}
