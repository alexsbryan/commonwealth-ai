// SPDX-License-Identifier: AGPL-3.0-or-later
//! Operator-gated control-vector steering for chat slots.
//!
//! A control vector adds a fixed per-layer direction to the residual stream
//! at inference (llama.cpp `llama_set_adapter_cvec`). Vectors are derived
//! OFFLINE (see `research/jlens/`) and shipped as a raw f32 little-endian
//! file in llama.cpp's cvec layout: `(n_layers - 1) * n_embd` floats, where
//! block `i` steers the residual after 0-based layer `i + 1` (layer 0 is
//! never steered).
//!
//! Everything is env-gated and OFF by default — no env var, no behavior
//! change. A mismatched file (wrong model) is skipped with a warning, never
//! a load failure.
//!
//! - `SOVEREIGN_CVEC=<path>`          raw f32 LE file (required to enable)
//! - `SOVEREIGN_CVEC_SCALE=<f32>`     multiplier, default 1.0
//! - `SOVEREIGN_CVEC_LAYERS=<a>-<b>`  inclusive layer range, default all
//! - `SOVEREIGN_CVEC_MODEL=<substr>`  only apply when the gguf stem
//!                                    contains this substring (recommended)
//!
//! Scope: applied by the chat-slot loader (`ModelSlot::load`) and the
//! MTP-failure rebuild only. Deliberately NOT applied to the FastShort
//! sibling slot (`from_existing_model`) — that slot serves router/intent
//! classification, which steering must never distort.

use std::path::Path;

use crate::llama::cpp::context::LlamaContext;

pub(crate) struct CvecConfig {
    path: String,
    scale: f32,
    layers: Option<(i32, i32)>,
    model_filter: Option<String>,
}

pub(crate) fn config_from_env() -> Option<CvecConfig> {
    let path = std::env::var("SOVEREIGN_CVEC").ok()?;
    if path.trim().is_empty() {
        return None;
    }
    let scale = std::env::var("SOVEREIGN_CVEC_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(1.0);
    let layers = std::env::var("SOVEREIGN_CVEC_LAYERS").ok().and_then(|s| {
        let (a, b) = s.split_once('-')?;
        Some((a.trim().parse::<i32>().ok()?, b.trim().parse::<i32>().ok()?))
    });
    let model_filter = std::env::var("SOVEREIGN_CVEC_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    Some(CvecConfig {
        path,
        scale,
        layers,
        model_filter,
    })
}

/// Apply the operator-configured control vector to a freshly built chat
/// context. Call once per context creation; a no-op unless `SOVEREIGN_CVEC`
/// is set. All failure modes log and return — slot load must never fail
/// because steering config is wrong.
pub(crate) fn maybe_apply(ctx: &mut LlamaContext<'_>, model_id: &str, n_embd: i32, n_layer: i32) {
    let Some(cfg) = config_from_env() else {
        return;
    };
    if let Some(filter) = &cfg.model_filter {
        if !model_id.to_lowercase().contains(&filter.to_lowercase()) {
            tracing::debug!(
                model_id = %model_id,
                filter = %filter,
                "SOVEREIGN_CVEC set but model filter does not match — skipping"
            );
            return;
        }
    }
    let bytes = match std::fs::read(Path::new(&cfg.path)) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(path = %cfg.path, error = %e, "SOVEREIGN_CVEC unreadable — skipping");
            return;
        }
    };
    let expected = (n_layer as usize - 1) * n_embd as usize * 4;
    if bytes.len() != expected {
        tracing::warn!(
            path = %cfg.path,
            model_id = %model_id,
            file_bytes = bytes.len(),
            expected_bytes = expected,
            "SOVEREIGN_CVEC length does not match this model \
             ((n_layers-1)*n_embd*4) — vector is for a different model; skipping"
        );
        return;
    }
    let mut data: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for v in &mut data {
        *v *= cfg.scale;
    }
    let (il_start, il_end) = cfg.layers.unwrap_or((1, n_layer - 1));
    let il_end = il_end.min(n_layer - 1);
    match ctx.set_adapter_cvec(&data, n_embd, il_start, il_end) {
        Ok(()) => tracing::info!(
            model_id = %model_id,
            path = %cfg.path,
            scale = cfg.scale,
            il_start,
            il_end,
            "control vector applied to chat slot context"
        ),
        Err(code) => tracing::warn!(
            model_id = %model_id,
            path = %cfg.path,
            code,
            "set_adapter_cvec failed — steering NOT active"
        ),
    }
}
