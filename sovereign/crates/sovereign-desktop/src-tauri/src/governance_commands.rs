// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop governance surface — the Tauri layer over a corpus's
//! event-sourced common law (FR-9). This is the "one UX panel" the
//! governance thesis ("one recipe + one pure fold + one UX panel") was
//! missing.
//!
//! # Where governance is decided (sv-surface D8)
//!
//! Eight commands hold NO oplog, no atlas dir and no recipe reader. Each
//! is one call onto `sovereign_mesh::governance_http`'s
//! `/internal/governance/{corpus}` routes over [`gov_client`] — ONE path
//! in both boot modes, because Local means the daemon is in-process over
//! this process's own `corpus_engine`. The daemon appends to the SAME
//! `governance_oplog.jsonl` the CLI `svrn govern` verbs write, so a
//! decision this panel makes is still seen by `govern ask`'s active-set
//! filter — it is now seen through one writer instead of two.
//!
//! What that deleted here, and where it lives now:
//!
//!   * the append lock, `Op`/`Oplog` construction, the `human:<user>`
//!     actor stamp and the four adjudication folds (resolve / accept /
//!     dismiss / undo) — the route owns them, and with them the
//!     pair-durability rule that every adjudication records its endpoint
//!     rules. Two writers appending to one un-locked JSONL was the race
//!     the desktop's process-wide mutex could only half-close.
//!   * the seed + post-build (migrate-ids THEN seed) order, which is
//!     load-bearing for living governance and now has one implementation
//!     (ARCH §10.6). The desktop's `is_governance_corpus` recipe read
//!     went with it: the route self-gates, answering `0` for a
//!     non-governance corpus.
//!   * `render_governance_recipe` + `GOVERNANCE_ONTOLOGY_GUIDANCE`,
//!     copied byte-identical into the route. The recipe is written to the
//!     DAEMON's `recipes_dir()`, which is the point — a desktop writing
//!     it into its own data dir on an attached boot would put it where
//!     the daemon never looks and the corpus would enrich down the wrong
//!     pipeline.
//!   * the view's five joins (section titles, section→chunk deep-links,
//!     scope names, recipe vocabulary, decision metadata) and the
//!     staleness heuristic. `GovernanceViewPayload` is re-exported from
//!     the route below rather than mirrored, so the bytes the webview
//!     receives are the route's own (ARCH §10.6).
//!
//! # User-visible change, named rather than absorbed (ARCH §18.3)
//!
//! `GovernanceView::from_atlas_dir` answered `Ok` + an EMPTY view for a
//! MISSING atlas, so an unenriched corpus rendered "no conflicts" — a
//! green verdict over a corpus nothing had looked at. `require_atlas`
//! makes that a 404 naming the path, which arrives here as `Err`. The
//! Conflicts panel now shows an error banner for an unenriched corpus
//! where it used to show a clean bill of health. "Not enriched yet" and
//! "no conflicts" are different banners.
//!
//! What could NOT cross, named: [`governance_export_write`] writes to a
//! path the USER picked in a save dialog. A daemon has no business
//! writing there, and the markdown it writes is composed frontend-side,
//! so there is no content for a route to serve in its place.

use std::sync::Arc;

use tauri::State;

use crate::state::AppState;

/// Everything the Conflicts panel renders for a corpus, in one call —
/// the ROUTE's type, re-exported rather than mirrored. Field-for-field
/// what this module used to declare (`view`, `section_titles`,
/// `section_chunks`, `scope_names`, `vocabulary`, `decisions`,
/// `docs_changed_since_build`), so the webview sees the same bytes.
///
/// (`DecisionMeta` and `VocabularyPayload` ride inside it and are named
/// there, not here — this is a binary crate, so a re-export nothing calls
/// is dead weight.)
pub use sovereign_mesh::governance_http::GovernanceViewPayload;

/// The client for the daemon's governance surface — the same oplog under
/// the same `index_dir`, reached over loopback instead (sv-surface D8).
fn gov_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

// ── get_view ─────────────────────────────────────────────────

/// Load the full governance panel payload for a corpus.
///
/// A corpus with no atlas is an `Err` carrying the host's words, never an
/// empty view — see the module header.
#[tauri::command]
pub async fn governance_get_view(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<GovernanceViewPayload, String> {
    gov_client(&state)
        .governance_view::<GovernanceViewPayload>(&corpus_id)
        .await
        .map_err(|e| format!("governance_get_view: {e}"))
}

// ── Adjudication commands ────────────────────────────────────

/// Resolve a conflict by keeping one rule (the other is superseded).
/// Returns the two appended op ids, in append order.
///
/// A `keep_rule_id` that is not one of the conflict's own two rules is an
/// `Err` naming it — the host refuses rather than picking one.
#[tauri::command]
pub async fn governance_resolve(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    tension_id: String,
    keep_rule_id: String,
    rationale: String,
) -> Result<Vec<String>, String> {
    gov_client(&state)
        .governance_resolve(&corpus_id, &tension_id, &keep_rule_id, &rationale)
        .await
        .map_err(|e| format!("governance_resolve: {e}"))
}

/// Accept a conflict as known-and-tolerated (both rules remain in force).
///
/// An empty rationale is an `Err`: an accepted conflict that records no
/// reason is the one decision a later reader cannot act on.
#[tauri::command]
pub async fn governance_accept(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    tension_id: String,
    rationale: String,
) -> Result<Vec<String>, String> {
    gov_client(&state)
        .governance_accept(&corpus_id, &tension_id, &rationale)
        .await
        .map_err(|e| format!("governance_accept: {e}"))
}

/// Dismiss a conflict as detector noise (not a real contradiction).
/// One-click, optional note — distinct from `accept`.
///
/// `None` sends an absent key, not an empty string: the optional note is
/// exactly what separates a dismissal from an acceptance.
#[tauri::command]
pub async fn governance_dismiss(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    tension_id: String,
    rationale: Option<String>,
) -> Result<Vec<String>, String> {
    gov_client(&state)
        .governance_dismiss(&corpus_id, &tension_id, rationale.as_deref())
        .await
        .map_err(|e| format!("governance_dismiss: {e}"))
}

/// Undo the current decision on a conflict (revert the adjudication
/// bundle atomically). Returns the appended revert op id.
///
/// An OPEN conflict is an `Err`, not a silent no-op: "there was nothing
/// to undo" is a fact the panel's button state depends on.
#[tauri::command]
pub async fn governance_undo_tension(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    tension_id: String,
) -> Result<String, String> {
    gov_client(&state)
        .governance_undo(&corpus_id, &tension_id)
        .await
        .map_err(|e| format!("governance_undo_tension: {e}"))
}

// ── Seed + post-build (living-governance durability) ─────────

/// Manual re-seed: establish or refresh the governed rule baseline.
/// Returns the count of NEWLY-asserted rules; `0` is the steady state
/// and a success.
#[tauri::command]
pub async fn governance_seed(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<u32, String> {
    gov_client(&state)
        .governance_seed(&corpus_id)
        .await
        .map_err(|e| format!("governance_seed: {e}"))
}

/// Automatic governance post-build: migrate atom ids to content-hash
/// THEN seed the rule baseline, in that order.
///
/// Called after an IN-PROCESS enrichment build completes (`lcEnrichNow` →
/// poll → onComplete). Self-gating on the host: a non-governance corpus
/// answers `0` and touches nothing, so completion handlers still call it
/// unconditionally. Do NOT compose it out of [`governance_seed`] plus a
/// migrate — the ORDER is what keeps every past decision resolving across
/// a rebuild, and the host owns it (ARCH §10.6).
#[tauri::command]
pub async fn governance_post_build_seed(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<u32, String> {
    gov_client(&state)
        .governance_post_build_seed(&corpus_id)
        .await
        .map_err(|e| format!("governance_post_build_seed: {e}"))
}

// ── Template recipe + export ─────────────────────────────────

/// Write a governance recipe for a freshly-added folder corpus, so
/// `recipe_enrich_init_from_corpus` selects the `custom_atlas` pipeline
/// and the post-build hook recognizes the corpus as governance-managed.
/// Returns the path it wrote, ON THE HOST — the daemon's `recipes_dir()`,
/// which is where enrichment actually looks.
#[tauri::command]
pub async fn governance_write_recipe(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    display_name: String,
    source_path: String,
) -> Result<String, String> {
    gov_client(&state)
        .governance_write_recipe(&corpus_id, &display_name, &source_path)
        .await
        .map_err(|e| format!("governance_write_recipe: {e}"))
}

/// Write arbitrary text to a path the user picked via the save dialog.
/// Pairs with the frontend's "Export current rules" / agenda save flow.
///
/// THE ONE THAT CANNOT CROSS, and why (ARCH §18.3 — named, not
/// swallowed): the destination is a user-picked path outside any data
/// root, and the markdown is composed in the webview from a view it
/// already holds. There is no content for a route to serve and no path a
/// daemon should be writing to.
#[tauri::command]
pub async fn governance_export_write(dest_path: String, content: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        std::fs::write(&dest_path, content.as_bytes())
            .map_err(|e| format!("writing export to {dest_path}: {e}"))
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}
