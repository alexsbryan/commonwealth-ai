// SPDX-License-Identifier: AGPL-3.0-or-later
//! GET /internal/atlas/status — per-corpus atlas readiness snapshot.
//!
//! Phase D1 — the desktop's "Knowledge readiness" panel polls this
//! endpoint to surface what the user actually has access to:
//!   - Has the structural atlas been built? (atom_count > 0)
//!   - How much Tier-2 enrichment has happened? (tier2_count)
//!   - Is the embed cache present so atlas grounding actually fires
//!     on the next chat turn?
//!   - Is background Tier-2 extraction in progress, and how far?
//!   - Token spend for the most recent extraction run.
//!
//! Pure read of on-disk state — same helpers as the
//! `sovereign corpus status` CLI so the two surfaces never drift.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use sovereign_tools::atlas_status::{compute_atlas_status, AtlasStatusRow};

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct AtlasStatusResponse {
    pub corpora: Vec<AtlasStatusRow>,
}

pub async fn atlas_status(
    State(state): State<AppState>,
) -> Result<Json<AtlasStatusResponse>, (StatusCode, Json<serde_json::Value>)> {
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "no corpus engine on this node"})),
        )
    })?;
    let indexes_dir = engine.index_dir().to_path_buf();
    // Enrichment dir is the indexes-dir's sibling (data_dir layout).
    let enrichment_dir = indexes_dir
        .parent()
        .map(|p| p.join("enrichment"))
        .unwrap_or_else(|| std::path::PathBuf::from("./enrichment"));
    let corpora = compute_atlas_status(&indexes_dir, &enrichment_dir);
    Ok(Json(AtlasStatusResponse { corpora }))
}
