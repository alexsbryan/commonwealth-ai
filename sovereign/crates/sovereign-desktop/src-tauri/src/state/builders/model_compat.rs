// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPU/architecture compatibility gate for the chat slots, applied at boot.
//!
//! Some model architectures — Qwen3.5 "Gated DeltaNet" (`qwen35`), Mamba/SSM,
//! RWKV — SIGSEGV inside ggml's recurrent `SET` op during CPU prefill (see
//! [`sovereign_inference::cpu_compat`]). A native crash in C is unrecoverable,
//! so rather than load such a model on a CPU-only machine and hard-crash on the
//! user's first message, we detect the architecture from the GGUF header
//! **before** loading and either substitute a dense model or fail the boot with
//! a clear, in-app explanation — never a silent crash.
//!
//! Runs before [`super::inference::load_inference`]; mutates the (already
//! cloned) [`DesktopConfig`] so both the eager fast slot and the lazy primary
//! slot get a CPU-safe model.

use tauri::{AppHandle, Emitter};

use sovereign_inference::cpu_compat::{choose_cpu_safe_chat_model, ChatModelChoice};
use sovereign_inference::hardware::HardwareProfile;

use crate::state::ResolvedModelSlots;

/// Event the frontend listens on to show a non-fatal "we swapped your model"
/// banner. Payload is [`ModelNoticePayload`].
pub const MODEL_NOTICE_EVENT: &str = "model-notice";

/// Informational banner: the configured model couldn't run here, so a dense one
/// was substituted. Not an error — the app is up and working.
#[derive(Clone, serde::Serialize)]
pub struct ModelNoticePayload {
    /// Human-readable, ready to render in a banner/toast.
    pub message: String,
    /// The model the user configured (filename).
    pub requested_model: String,
    /// The architecture that isn't CPU-compatible (e.g. `"qwen35"`).
    pub requested_arch: String,
    /// The dense model running instead (filename).
    pub running_model: String,
    /// Its architecture (e.g. `"qwen3"`).
    pub running_arch: String,
}

/// True when the chat slots will compute on the CPU backend — the only case the
/// recurrent-`SET` crash occurs. Mirrors `ModelSlot::load`'s gate:
/// `SOVEREIGN_FORCE_CPU_CHAT`, or a machine with no GPU offload
/// (`recommended_gpu_layers == 0`, e.g. an Intel Mac where `detect_gpu()` is
/// false).
fn computes_on_cpu() -> bool {
    let force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    force_cpu || HardwareProfile::detect().recommended_gpu_layers == 0
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

fn parent_dir(p: &std::path::Path) -> std::path::PathBuf {
    p.parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Apply the CPU/arch compatibility policy to `config` in place.
///
/// - GPU machine → no-op (the crash is CPU-only; keep the user's models).
/// - Fast slot unsafe on CPU + a dense substitute exists → swap it in and emit
///   a [`MODEL_NOTICE_EVENT`] banner.
/// - Fast slot unsafe + NO substitute → `Err` so bootstrap surfaces a clear
///   `backend-error` instead of crashing.
/// - Primary (lazy Slow slot): same, but a missing substitute is non-fatal
///   (Slow degrades to the fast slot / a mesh peer) — we unset it and log.
pub fn apply_cpu_compat_policy(
    slots: &mut ResolvedModelSlots,
    app: &AppHandle,
) -> Result<(), String> {
    if !computes_on_cpu() {
        return Ok(()); // GPU backend is unaffected.
    }

    // ── Fast slot (eager at boot; the reported first-message crash path) ──
    let fast_dir = parent_dir(&slots.fast);
    match choose_cpu_safe_chat_model(&slots.fast, true, &fast_dir) {
        ChatModelChoice::Keep => {}
        ChatModelChoice::Substitute {
            path,
            unsafe_arch,
            safe_arch,
        } => {
            let payload = ModelNoticePayload {
                message: format!(
                    "{requested} needs a GPU here — its {unsafe_arch} architecture crashes the \
                     math on CPU, an upstream bug we don't paper over. You're running {running} \
                     instead, so nothing's blocked. To run {requested} itself, add a supported \
                     GPU, or pool a machine you trust on the mesh.",
                    requested = file_name(&slots.fast),
                    unsafe_arch = unsafe_arch,
                    running = file_name(&path),
                ),
                requested_model: file_name(&slots.fast),
                requested_arch: unsafe_arch,
                running_model: file_name(&path),
                running_arch: safe_arch,
            };
            tracing::warn!(
                requested = %slots.fast.display(),
                running = %path.display(),
                "cpu-compat: substituted a dense chat model for a CPU-incompatible one"
            );
            // Best-effort: a missing frontend listener must not fail boot.
            let _ = app.emit(MODEL_NOTICE_EVENT, payload);
            slots.fast = path;
        }
        ChatModelChoice::NoSafeModel { unsafe_arch } => {
            return Err(format!(
                "{requested} can't run on this machine's CPU — its {unsafe_arch} architecture \
                 crashes there, an upstream bug — and there's no CPU-friendly model alongside it \
                 to fall back on. Point Settings at a dense model (a Qwen3 or Llama GGUF), add a \
                 supported GPU, or pool a machine you trust on the mesh. Then it'll run.",
                requested = file_name(&slots.fast),
                unsafe_arch = unsafe_arch,
            ));
        }
    }

    // ── Primary slot (lazy; Slow synthesis). Missing dense = non-fatal ──
    if let Some(primary) = slots.primary.clone() {
        let primary_dir = parent_dir(&primary);
        match choose_cpu_safe_chat_model(&primary, true, &primary_dir) {
            ChatModelChoice::Keep => {}
            ChatModelChoice::Substitute { path, .. } => {
                // Don't double-load the same file the fast slot now uses.
                slots.primary = if path == slots.fast { None } else { Some(path) };
            }
            ChatModelChoice::NoSafeModel { unsafe_arch } => {
                tracing::warn!(
                    primary = %primary.display(),
                    arch = %unsafe_arch,
                    "cpu-compat: primary (Slow) model is CPU-incompatible and no dense substitute \
                     found — unsetting it; Slow work routes to the fast slot or a mesh peer"
                );
                slots.primary = None;
            }
        }
    }

    Ok(())
}
