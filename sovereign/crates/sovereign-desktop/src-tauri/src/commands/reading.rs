// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
use std::sync::Arc;

use tauri::State;

use crate::state::AppState;

// ─── Reading Surface ─────────────────────────────────────────────────────────
//
// Backs the desktop's glass-box reading UI. Frontend calls
// `read_get_chunk_neighbors(corpus, chunkId, radius)` after the user clicks a
// citation; the answer comes from the daemon's `reading_http` routes in BOTH
// boot modes (sv-surface D1). "Local" does not mean "a second reading
// implementation" — it means the daemon is IN-PROCESS: `state.rs` commissions
// it with THIS process's `corpus_engine` and `state_store` (`ServingCore`) and
// refuses to finish bootstrap unless it binds `client_port`, so the same four
// routes answer over loopback that an attached daemon answers over the wire.
//
// Parity is by construction and needs no mirror to keep: these commands return
// the daemon's response bytes VERBATIM — `serde_json::Value`, parsed by nobody
// on the way through — so there is no second serializer that could disagree
// with `reading_http`'s. The hand-kept `*Dto` mirrors this file used to carry
// (44 refs, "byte-compatible by comment") had already drifted when they were
// deleted: they lacked the wire types' `skip_serializing_if` attributes, so an
// in-process chunk emitted `"title": null` where the daemon's JSON omitted the
// key — the exact break the mirrors existed to prevent. The census
// (tests/reading_wire_types_census.rs) keeps both a second spelling and a
// second path out.

/// GET one of the daemon's loopback `reading_http` routes
/// (`/internal/corpus/...`, merged into the client router on `client_port`)
/// and return its body verbatim. A 404 — chunk/atom/corpus absent, or an older
/// daemon without the route — maps to `Ok(None)`, which is the shape the
/// frontend already reads.
async fn daemon_reading_get(
    base_url: &str,
    path: &str,
) -> Result<Option<serde_json::Value>, String> {
    let url = format!("{base_url}{path}");
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("daemon reading GET {url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("daemon reading {url} → {status}: {body}"));
    }
    resp.json::<serde_json::Value>()
        .await
        .map(Some)
        .map_err(|e| format!("daemon reading decode {url}: {e}"))
}

#[tauri::command]
pub async fn read_get_chunk(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
) -> Result<Option<serde_json::Value>, String> {
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}"),
    )
    .await
}

#[tauri::command]
pub async fn read_get_chunk_neighbors(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
    radius: Option<usize>,
) -> Result<Option<serde_json::Value>, String> {
    let radius = radius.unwrap_or(1).min(5);
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}/neighbors?radius={radius}"),
    )
    .await
}

// ─── Atom Panel ──────────────────────────────────────────────────────────────
//
// Two routes back the desktop's atom panel: `read_get_atom_card` returns the
// atom card (canonical_name, description, salience, one-hop relations,
// cross-corpus bridges) and `read_get_atom_elsewhere` returns the section list
// + cross-corpus links so the user can jump to other places the atom appears.
// The section→chunk projection happens in the route
// (`reading_http`'s `resolve_sections_to_chunks`), so the desktop receives
// ready-to-click chunk_ids in both boot modes.

#[tauri::command]
pub async fn read_get_atom_card(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<serde_json::Value>, String> {
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/atoms/{atom_id}"),
    )
    .await
}

#[tauri::command]
pub async fn read_get_atom_elsewhere(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<serde_json::Value>, String> {
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/atoms/{atom_id}/elsewhere"),
    )
    .await
}
