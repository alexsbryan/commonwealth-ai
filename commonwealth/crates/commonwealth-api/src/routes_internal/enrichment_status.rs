//! `GET /internal/enrichment/status?corpus_id=<id>` — generic
//! per-corpus enrichment progress surface.
//!
//! Reads `<index>/_enrichment_state.json` written by any pipeline
//! that adopts the `EnrichmentProgressSink` trait (folder tiered,
//! structural atlas postinstall, conversation RAPTOR, …). The route
//! itself is pipeline-agnostic — it returns whatever phase the file
//! says, plus a derived `is_stalled` for callers that don't want to
//! match on the enum.
//!
//! Companion to `GET /internal/atlas/status` (which reports atlas
//! readiness as a static snapshot) and `GET /internal/corpus/status`
//! (which reports live ingest progress). Together those three give
//! the desktop a complete glassbox view: ingest progress, then
//! enrichment progress, then readiness.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use corpus_engine::enrichment::state::{
    EnrichmentPhase, EnrichmentState, EnrichmentStateFile,
};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    pub corpus_id: String,
}

#[derive(Debug, Serialize)]
pub struct EnrichmentStatusResponse {
    pub corpus_id: String,
    /// Present when the state file exists. Absent when the corpus
    /// hasn't been touched by any enrichment pipeline yet (fresh
    /// install pre-postinstall).
    pub state: Option<EnrichmentState>,
    /// Convenience flags so frontends don't re-enumerate the phase
    /// enum. `is_terminal` covers complete + failed + stalled.
    pub is_terminal: bool,
    pub is_stalled: bool,
    /// Coarse fraction-complete derived from phase + (when present)
    /// step counters. Range 0.0..=1.0. Always 0.0 for stalled /
    /// failed so the UI bar collapses to red empty.
    pub fraction_complete: f32,
}

pub async fn enrichment_status(
    State(state): State<AppState>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<EnrichmentStatusResponse>, (StatusCode, Json<serde_json::Value>)> {
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "no corpus engine on this node"})),
        )
    })?;
    let index_dir = engine.index_dir().join(&query.corpus_id);
    let parsed = EnrichmentStateFile::read(&index_dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("read state file: {e}")})),
        )
    })?;
    let (is_terminal, is_stalled, fraction_complete) = match &parsed {
        Some(s) => {
            let frac = derive_fraction(s);
            (s.phase.is_terminal(), matches!(s.phase, EnrichmentPhase::Stalled), frac)
        }
        None => (false, false, 0.0),
    };
    Ok(Json(EnrichmentStatusResponse {
        corpus_id: query.corpus_id,
        state: parsed,
        is_terminal,
        is_stalled,
        fraction_complete,
    }))
}

fn derive_fraction(state: &EnrichmentState) -> f32 {
    if matches!(state.phase, EnrichmentPhase::Complete) {
        return 1.0;
    }
    if matches!(state.phase, EnrichmentPhase::Failed | EnrichmentPhase::Stalled) {
        return 0.0;
    }
    let base = state.phase.coarse_fraction();
    // Refine the coarse fraction with per-step progress when the
    // pipeline supplies a denominator. Bound the within-phase
    // contribution so the bar can never regress: at most we add the
    // "headroom" until the next phase's coarse fraction.
    if state.step_total == 0 {
        return base;
    }
    let next_phase_fraction = next_phase_floor(state.phase);
    let headroom = (next_phase_fraction - base).max(0.0);
    let within = (state.step_current as f32) / (state.step_total as f32).max(1.0);
    (base + headroom * within.clamp(0.0, 1.0)).min(1.0)
}

fn next_phase_floor(phase: EnrichmentPhase) -> f32 {
    match phase {
        EnrichmentPhase::Starting => EnrichmentPhase::Scanning.coarse_fraction(),
        EnrichmentPhase::Scanning => EnrichmentPhase::EntityExtraction.coarse_fraction(),
        EnrichmentPhase::EntityExtraction => EnrichmentPhase::RaptorLeaves.coarse_fraction(),
        EnrichmentPhase::RaptorLeaves => EnrichmentPhase::RaptorTree.coarse_fraction(),
        EnrichmentPhase::RaptorTree => EnrichmentPhase::MotifExtraction.coarse_fraction(),
        EnrichmentPhase::MotifExtraction => EnrichmentPhase::Persisting.coarse_fraction(),
        EnrichmentPhase::AtomExtraction => EnrichmentPhase::Persisting.coarse_fraction(),
        EnrichmentPhase::Persisting => 1.0,
        EnrichmentPhase::Complete
        | EnrichmentPhase::Failed
        | EnrichmentPhase::Stalled => 1.0,
    }
}
