// SPDX-License-Identifier: AGPL-3.0-or-later
//! TEACHABLE P0 lesson CRUD — thin wrappers over `AppState.notes`
//! (`kind = "lesson"`). The payload schema is owned by
//! `sovereign_core::lessons::LessonPayload`; this module never
//! duplicates it. Save implements the per-rung supersede (one ACTIVE
//! lesson per enforcement rung — TEACHABLE §6's structural K=1), so
//! the "What I've learned" pane can render superseded rows
//! struck-through via their successor's `supersedes` link. Delete is
//! a hard delete (§5: deleting is real deletion). Dismissing a card
//! calls none of these — dismissals are never stored.

use std::sync::Arc;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};
use serde::{Deserialize, Serialize};
use sovereign_core::lessons::{LessonPayload, TaughtFrom, LESSON_KIND};
use sovereign_core::types::LessonProposedPayload;
use tauri::State;

use crate::state::AppState;

/// One row for the settings pane. Flattens the payload with the note
/// lifecycle fields; retired rows are included so supersede chains
/// render ("replaced by" resolves client-side via `supersedes`).
#[derive(Serialize)]
pub struct LessonRow {
    pub id: String,
    pub display: String,
    pub prompt_form: String,
    pub enforcement: String,
    pub params: serde_json::Value,
    pub scope: Vec<String>,
    pub taught_from: TaughtFrom,
    pub enabled: bool,
    pub created: i64,
    pub first_applied_at: Option<i64>,
    pub last_affirmed: Option<i64>,
    /// The pre-edit draft sentence when the user edited the card —
    /// the consented correction pair (TEACHABLE §11).
    pub drafted_display: Option<String>,
    pub retired_at: Option<i64>,
    pub retired_by: Option<String>,
    pub supersedes: Option<String>,
}

/// The card's save argument: the `lesson-proposed` payload (with a
/// possibly-edited `display`/`prompt_form`) plus the pre-edit sentence
/// when the user edited.
#[derive(Deserialize)]
pub struct LessonDraft {
    #[serde(flatten)]
    pub proposal: LessonProposedPayload,
    #[serde(default)]
    pub drafted_display: Option<String>,
}

async fn notes_handle(state: &Arc<AppState>) -> Result<Arc<NoteStore>, String> {
    state
        .notes
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| {
            "Lessons unavailable: notes.db is not open — try restarting the desktop.".to_string()
        })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_from_note(row: corpus_engine_notes::NoteRow) -> Option<LessonRow> {
    let raw = row.payload_json.as_deref()?;
    let payload: LessonPayload = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(note_id = %row.id, error = %e, "lessons: malformed payload skipped");
            return None;
        }
    };
    Some(LessonRow {
        id: row.id,
        display: payload.display,
        prompt_form: payload.prompt_form,
        enforcement: payload.enforcement.as_str().to_string(),
        params: payload.params,
        scope: payload.scope,
        taught_from: payload.taught_from,
        enabled: payload.enabled,
        created: payload.created,
        first_applied_at: payload.first_applied_at,
        last_affirmed: payload.last_affirmed,
        drafted_display: payload.drafted_display,
        retired_at: row.retired_at,
        retired_by: row.retired_by,
        supersedes: row.supersedes,
    })
}

/// All lessons, newest first, INCLUDING retired (superseded) rows —
/// the pane is the trust story and shows the whole chain.
#[tauri::command]
pub async fn list_lessons(state: State<'_, Arc<AppState>>) -> Result<Vec<LessonRow>, String> {
    let notes = notes_handle(&state).await?;
    let rows = notes
        .read_notes(None, &[], &[], &[LESSON_KIND.to_string()], 500, true)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().filter_map(row_from_note).collect())
}

/// Persist a kept lesson. Returns the new note id. Supersedes (and
/// retires) the previously-active lesson of the same enforcement rung.
#[tauri::command]
pub async fn save_lesson(
    state: State<'_, Arc<AppState>>,
    draft: LessonDraft,
) -> Result<String, String> {
    let notes = notes_handle(&state).await?;
    let mut payload = LessonPayload::from_proposed(&draft.proposal, unix_now());
    payload.drafted_display = draft
        .drafted_display
        .filter(|d| !d.trim().is_empty() && *d != payload.display);

    // Per-rung supersede: at most one ACTIVE lesson per rung.
    let active = notes
        .read_notes(None, &[], &[], &[LESSON_KIND.to_string()], 20, false)
        .await
        .map_err(|e| e.to_string())?;
    let superseded: Option<String> = active.iter().find_map(|row| {
        let raw = row.payload_json.as_deref()?;
        let existing: LessonPayload = serde_json::from_str(raw).ok()?;
        (existing.enforcement == payload.enforcement).then(|| row.id.clone())
    });

    let payload_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let new_id = notes
        .write_note_full_v9(
            LESSON_KIND,
            &payload.display,
            vec![],
            vec![],
            // session_id = provenance conversation (the teaching moment).
            &payload.taught_from.conversation_id,
            NoteScope::Global,
            None,
            None,
            NoteSource::Agent,
            superseded.as_deref(),
            Some(&payload_json),
            // Mesh privacy: node-default wiring is P3; P0 lessons are
            // node-local in practice (notes gossip only scope=global
            // non-private — acceptable either way per the plan).
            false,
        )
        .await
        .map_err(|e| e.to_string())?;
    if let Some(old_id) = &superseded {
        if let Err(e) = notes
            .retire_by_id(old_id, &format!("superseded by {new_id}"))
            .await
        {
            tracing::warn!(target: "lessons", old_id = %old_id, error = %e,
                "lesson supersede: retire of predecessor failed");
        }
    }
    tracing::info!(
        target: "lessons",
        note_id = %new_id,
        enforcement = payload.enforcement.as_str(),
        superseded = superseded.as_deref().unwrap_or(""),
        "lesson saved"
    );
    Ok(new_id)
}

/// Toggle a lesson without deleting it. Returns false for unknown ids.
#[tauri::command]
pub async fn set_lesson_enabled(
    state: State<'_, Arc<AppState>>,
    id: String,
    enabled: bool,
) -> Result<bool, String> {
    let notes = notes_handle(&state).await?;
    let Some(row) = notes.read_note_by_id(&id).await.map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let raw = row.payload_json.unwrap_or_else(|| "{}".to_string());
    let mut payload: LessonPayload =
        serde_json::from_str(&raw).map_err(|e| format!("malformed lesson payload: {e}"))?;
    payload.enabled = enabled;
    let json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    notes
        .update_note_payload(&id, &json)
        .await
        .map_err(|e| e.to_string())
}

/// Hard delete — real deletion, no tombstone, no recycle bin.
#[tauri::command]
pub async fn delete_lesson(state: State<'_, Arc<AppState>>, id: String) -> Result<bool, String> {
    notes_handle(&state)
        .await?
        .delete_note(&id)
        .await
        .map_err(|e| e.to_string())
}
