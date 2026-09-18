// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `/v1/edit_predictions` wire contract: the request shapes and the caps
//! they are validated against. Split out of the route module by domains
//! `REVIEW-audit-4` (the route file had grown past arch-gate's growth slack);
//! the route module re-exports the public names at the historical path.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::openai_types::ErrorResponse;

/// Caps: a request past these is malformed, not merely large — the
/// first-party client enforces the same limits before sending.
pub(crate) const MAX_TEXT_BYTES: usize = 512 * 1024;
const MAX_HISTORY: usize = 32;
pub(crate) const MAX_UNIT_BYTES: usize = 2 * 1024;

/// Transport-level body cap for this route (`server.rs` applies it),
/// well under the router-wide 8 MB frontdoor. Sized so no *legal*
/// request can trip it: 512 KiB of text can JSON-escape to 1 MiB in
/// the worst case, plus 32 units × 4 fields × 2 KiB of history, plus
/// envelope. Anything larger cannot satisfy the caps below, so it is
/// refused before serde allocates it rather than after.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct EditPredictionsRequestWire {
    /// Coalesced edit units, oldest first.
    #[serde(default)]
    pub history: Vec<HistoryUnitWire>,
    /// Current document text (the search space for remaining sites).
    pub text: String,
    /// Cursor offset into `text`, in UTF-16 code units.
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub debug: bool,
    /// Opt-in to the model lane (P2). Off by default — the lane may
    /// not default-on until the §6 generalization bank says so
    /// (`gym/next-edit/gen/README.md`).
    #[serde(default)]
    pub model_lane: bool,
    /// Opt-in to the symbol lane: call-site NAVIGATION for a signature
    /// edit (`code_next_edit::next_edit_symbols`). Off by default, and it
    /// proposes no edits in any case — `navigation.sites` is a jump
    /// list. Requires `corpus_id`.
    #[serde(default)]
    pub symbol_lane: bool,
    /// Which indexed corpus describes this workspace. Supplied by the
    /// client, never guessed: enumerating installed indexes to find out
    /// opens every corpus on disk (~10 s), which is not a thing to do
    /// on the interactive editing path. Absent means the symbol lane
    /// declines and says so, rather than silently searching the wrong
    /// graph (ARCH §18.3).
    #[serde(default)]
    pub corpus_id: Option<String>,
    /// Absolute path of the workspace root the corpus was indexed from.
    /// The graph stores REPO-RELATIVE paths while the editor sends an
    /// absolute one, and nothing on the daemon can bridge that: the
    /// index root is `~/.svrnmesh/indexes`, which says nothing about
    /// where the source lives. Supplied by the client, which knows it;
    /// absent means the symbol lane declines.
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryUnitWire {
    pub before: String,
    pub after: String,
    #[serde(default)]
    pub left: String,
    #[serde(default)]
    pub right: String,
}

pub(crate) fn bad_request(msg: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::to_value(ErrorResponse::new(msg, "invalid_request")).unwrap_or_default()),
    )
        .into_response()
}

/// The wire caps, in one place. `Err` carries the 400 body — every
/// message names the offending field and its measured size, because a
/// 4xx here poisons the client's whole history window until the unit
/// ages out (NEXT_EDIT.md §9a).
///
/// Shared with the offline scorer for the same reason the pipeline is:
/// the caps were *already* the subject of a two-rulers bug once (client
/// counted UTF-16 chars, daemon counted UTF-8 bytes), so a second copy
/// here is the one thing this module must not grow.
pub fn validate_wire(wire: &EditPredictionsRequestWire) -> Result<(), String> {
    if wire.text.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "`text` is {} bytes; /v1/edit_predictions caps the search space at {} — send the \
             active file, not a corpus",
            wire.text.len(),
            MAX_TEXT_BYTES
        ));
    }
    if wire.history.len() > MAX_HISTORY {
        return Err(format!(
            "{} history units; the induction window never looks past {} — send the most recent",
            wire.history.len(),
            MAX_HISTORY
        ));
    }
    if let Some((field, len)) = wire.history.iter().find_map(|u| {
        [
            ("before", &u.before),
            ("after", &u.after),
            ("left", &u.left),
            ("right", &u.right),
        ]
        .into_iter()
        .find(|(_, s)| s.len() > MAX_UNIT_BYTES)
        .map(|(name, s)| (name, s.len()))
    }) {
        return Err(format!(
            "a history unit exceeds {MAX_UNIT_BYTES} bytes per field (`{field}` is {len} bytes; \
             note the cap is BYTES, not chars) — units are coalesced keystroke bursts, not pastes"
        ));
    }
    Ok(())
}
