// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands powering the desktop's "Add Knowledge Source"
//! panel.
//!
//! Two paths into the Knowledge view:
//!
//! - **Browse** — calls the existing [`crate::commands::list_corpora`]
//!   (registry snapshot + local merge). No new command needed.
//! - **Import** — paste TOML or drop a `.toml` file, which the DAEMON
//!   validates and installs into the registry it will resolve through:
//!   `POST /internal/corpus/recipes/import`.
//!
//! Plus parameter discovery (`GET /internal/corpus/recipes/{id}/
//! parameters`) so the UI can render an install-time form for
//! parameterized recipes (SEC EDGAR entity list, date ranges, …) before
//! posting to `/internal/corpus/install`.
//!
//! Both crossed the wire on 2026-09-11 (thin-desktop order). Until then
//! this file held a `CorpusEngine` of its own to run the validation
//! harness, and wrote `~/.svrnmesh/recipes/<id>/recipe.toml` plus a
//! `registry.toml` upsert with a loop of its own — a third copy of
//! `RecipeRegistry::install_local_recipe`, resolving THIS process's
//! default recipes dir, which is the daemon's only when the two agree
//! about the data root. The wire form cannot disagree.
//!
//! The actual install POST lives in [`crate::commands::install_corpus`];
//! `corpus_install_with_parameters` below is that call with the resolved
//! parameter map threaded through. The daemon validates the map against
//! `[recipe.parameters]` before spawning the ingest task.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use sovereign_contracts::daemon_wire::{
    ImportRecipeRequest, ImportRecipeResult, RecipeParameterSchema,
};
use sovereign_turn_client::TurnClient;
use tauri::State;

use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
pub struct InstallWithParametersRequest {
    pub corpus_id: String,
    /// Map of parameter name → value (string, number, or string
    /// array). Forwarded to the daemon's `/internal/corpus/install`
    /// endpoint as-is.
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
}

/// `corpus_import_recipe` — hand the pasted TOML to the daemon, which
/// validates it offline and installs it into its own registry.
///
/// A recipe that parses but fails validation comes back as
/// `success: false` with the errors the dialog renders inline, and
/// nothing is written; a body that is not a recipe at all, or a store
/// the daemon cannot write, is an `Err`. The two are different facts and
/// this boundary keeps them apart (ARCH principle 6).
#[tauri::command]
pub async fn corpus_import_recipe(
    state: State<'_, Arc<AppState>>,
    toml_text: String,
) -> Result<ImportRecipeResult, String> {
    TurnClient::new(state.internal_base_url())
        .import_recipe::<_, ImportRecipeResult>(&ImportRecipeRequest { toml_text })
        .await
        .map_err(|e| format!("corpus_import_recipe: {e}"))
}

/// `corpus_get_recipe_parameters` — the `[parameters]` block the daemon's
/// registry resolves for this recipe, so the UI can render an
/// install-time form. Works for any recipe that registry can resolve
/// (bundled, live, or locally-imported).
#[tauri::command]
pub async fn corpus_get_recipe_parameters(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<RecipeParameterSchema, String> {
    TurnClient::new(state.internal_base_url())
        .recipe_parameters::<RecipeParameterSchema>(&corpus_id)
        .await
        .map_err(|e| format!("corpus_get_recipe_parameters `{corpus_id}`: {e}"))
}

/// `corpus_install_with_parameters` — same as
/// [`crate::commands::install_corpus`] but threads the
/// install-time parameter map through to the daemon. The UI calls
/// this after the operator has filled the form rendered from
/// `corpus_get_recipe_parameters`.
#[tauri::command]
pub async fn corpus_install_with_parameters(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: InstallWithParametersRequest,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/corpus/install");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "corpus_id": request.corpus_id,
            "parameters": request.parameters,
        }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/install: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/install returned {status}: {body}"
        ));
    }

    // Mirror the install_corpus optimistic UI flip so the Install
    // button reacts immediately. The status poller catches up on
    // the next tick.
    let initial = crate::commands::CorpusProgressPayload {
        corpus_id: request.corpus_id.clone(),
        phase: "downloading".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Starting…".into()),
        ..Default::default()
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(request.corpus_id.clone(), initial.clone());
    }
    use tauri::Emitter;
    let _ = app_handle.emit("corpus-progress", initial);
    Ok(())
}
