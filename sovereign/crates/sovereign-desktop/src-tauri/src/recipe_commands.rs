// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands powering the desktop's "Add Knowledge Source"
//! panel.
//!
//! Two paths into the Knowledge view:
//!
//! - **Browse** — calls the existing
//!   [`crate::commands::list_corpora`] (registry snapshot + local
//!   merge). No new command needed.
//! - **Import** — paste TOML or drop a `.toml` file. Validates +
//!   writes under `~/.sovereign/recipes/<id>/recipe.toml`, then
//!   appends to `~/.sovereign/recipes/registry.toml` so the next
//!   `list_corpora` round-trip surfaces it as a local entry.
//!
//! Plus parameter discovery so the UI can render an install-time
//! form for parameterized recipes (SEC EDGAR entity list, date
//! ranges, …) before posting to `/internal/corpus/install`.
//!
//! The actual install POST lives in
//! [`crate::commands::install_corpus`] — we extend its body to
//! optionally carry the resolved parameter map. The daemon
//! validates them synchronously against
//! `[recipe.parameters]` before spawning the ingest task.

use std::collections::BTreeMap;
use std::path::PathBuf;

use corpus_engine::{ParameterKind, Recipe, RegistryEntry, RegistrySnapshot};
use serde::{Deserialize, Serialize};

/// Result of `corpus_import_recipe`. `success = false` carries
/// `errors` so the import dialog can show validation problems
/// inline; the recipe is NOT written to disk in that case.
#[derive(Debug, Clone, Serialize)]
pub struct ImportRecipeResult {
    pub success: bool,
    pub corpus_id: String,
    pub recipe_path: String,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Schema the UI uses to render an install-time parameter form.
/// One entry per declared `[parameters.<name>]`; `kind` drives
/// which input control shows up (text / multi-tag / date / number).
#[derive(Debug, Clone, Serialize)]
pub struct RecipeParameterSchema {
    pub corpus_id: String,
    pub parameters: Vec<RecipeParameter>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeParameter {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub required: bool,
    /// Default rendered as a JSON value so the frontend can
    /// pre-populate the form without round-tripping through TOML.
    /// `null` when the recipe declared no default.
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallWithParametersRequest {
    pub corpus_id: String,
    /// Map of parameter name → value (string, number, or string
    /// array). Forwarded to the daemon's `/internal/corpus/install`
    /// endpoint as-is.
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
}

/// `corpus_import_recipe` — accept a raw TOML string from the UI's
/// paste/drop surface, validate it, and stamp it under
/// `~/.sovereign/recipes/<corpus_id>/recipe.toml` plus a registry
/// entry. The next call to `list_corpora` will surface it as a
/// local entry.
///
/// This is the same flow `sovereign recipe publish` runs, minus
/// the upstream-PR template — desktop users who want to share
/// their recipe still go through the CLI. Validation runs in
/// `--offline` mode so the import isn't gated on network reach.
#[tauri::command]
pub async fn corpus_import_recipe(toml_text: String) -> Result<ImportRecipeResult, String> {
    let recipe =
        Recipe::from_toml(&toml_text).map_err(|e| format!("recipe TOML parse failed: {e}"))?;
    let corpus_id = recipe.corpus.id.clone();
    if corpus_id.is_empty() {
        return Err("recipe `[corpus] id` must not be empty".into());
    }

    // Validate using the same harness `recipe validate` runs.
    let engine = stub_engine();
    let options = corpus_engine::TestOptions {
        sample_size: 0,
        embed: false,
        offline: true,
        ..Default::default()
    };
    // Write to a tmp file so `test_recipe` can re-read by path —
    // the harness's entry point takes a path, not a parsed Recipe.
    let tmp = std::env::temp_dir().join(format!("recipe-import-{corpus_id}.toml"));
    std::fs::write(&tmp, &toml_text)
        .map_err(|e| format!("failed to stage TOML for validation: {e}"))?;
    let report = engine
        .test_recipe(&tmp, &options)
        .await
        .map_err(|e| format!("validation harness failed: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    if !report.validation.errors.is_empty() {
        return Ok(ImportRecipeResult {
            success: false,
            corpus_id,
            recipe_path: String::new(),
            errors: report.validation.errors.clone(),
            warnings: report.validation.warnings.clone(),
        });
    }

    let local_root = local_recipes_dir()?;
    let recipe_dir = local_root.join(&corpus_id);
    std::fs::create_dir_all(&recipe_dir)
        .map_err(|e| format!("failed to create {}: {e}", recipe_dir.display()))?;
    let recipe_path = recipe_dir.join("recipe.toml");
    let part = recipe_path.with_extension("toml.part");
    std::fs::write(&part, toml_text.as_bytes())
        .map_err(|e| format!("failed to stage recipe at {}: {e}", part.display()))?;
    std::fs::rename(&part, &recipe_path).map_err(|e| format!("failed to commit recipe: {e}"))?;

    upsert_local_registry(&local_root, &recipe, toml_text.as_bytes())?;

    Ok(ImportRecipeResult {
        success: true,
        corpus_id,
        recipe_path: recipe_path.display().to_string(),
        errors: Vec::new(),
        warnings: report.validation.warnings.clone(),
    })
}

/// `corpus_get_recipe_parameters` — pull the `[parameters]` block
/// from a recipe so the UI can render an install-time form. Works
/// for any recipe the registry can resolve (bundled, live, or
/// locally-imported).
#[tauri::command]
pub async fn corpus_get_recipe_parameters(
    corpus_id: String,
) -> Result<RecipeParameterSchema, String> {
    let local_dir = corpus_engine::RecipeRegistry::default_local_recipes_dir();
    let mut registry = corpus_engine::RecipeRegistry::from_bundled(local_dir.clone());
    if let Some(d) = &local_dir {
        registry = registry.with_local_registry(&d.join("registry.toml"));
    }
    let recipe = registry
        .fetch_recipe(&corpus_id)
        .await
        .map_err(|e| format!("failed to resolve recipe `{corpus_id}`: {e}"))?;

    let mut parameters = Vec::with_capacity(recipe.parameters.len());
    for (name, spec) in &recipe.parameters {
        parameters.push(RecipeParameter {
            name: name.clone(),
            kind: parameter_kind_label(&spec.kind).to_string(),
            description: spec.description.clone(),
            required: spec.required,
            default: spec.default.as_ref().map(toml_to_json),
        });
    }
    Ok(RecipeParameterSchema {
        corpus_id: recipe.corpus.id,
        parameters,
    })
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

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn local_recipes_dir() -> Result<PathBuf, String> {
    corpus_engine::RecipeRegistry::default_local_recipes_dir()
        .ok_or_else(|| "HOME is not set; cannot resolve ~/.sovereign/recipes/".to_string())
}

fn parameter_kind_label(k: &ParameterKind) -> &'static str {
    match k {
        ParameterKind::String => "string",
        ParameterKind::Int => "int",
        ParameterKind::Date => "date",
        ParameterKind::List => "list",
    }
}

fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, vv) in table {
                map.insert(k.clone(), toml_to_json(vv));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

fn stub_engine() -> corpus_engine::CorpusEngine {
    let stub: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
        Box::pin(async { Ok(vec![0f32; corpus_engine::DEFAULT_EMBED_DIM]) })
    });
    let tmp = std::env::temp_dir().join("sovereign-desktop-recipe-import");
    corpus_engine::CorpusEngine::new(tmp.clone(), tmp, stub)
}

/// Insert (or update) an entry in the user's local registry TOML so
/// the next `list_corpora` round-trip surfaces the imported recipe.
/// Reuses the same shape as `sovereign recipe publish` so the two
/// paths produce a consistent registry.
fn upsert_local_registry(
    local_root: &std::path::Path,
    recipe: &Recipe,
    bytes: &[u8],
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sha256: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let registry_path = local_root.join("registry.toml");
    let existing = std::fs::read_to_string(&registry_path).unwrap_or_default();
    let mut snapshot: RegistrySnapshot = if existing.is_empty() {
        RegistrySnapshot {
            schema_version: 1,
            generated_at: rfc3339_now(),
            registry_url: String::new(),
            entries: Vec::new(),
        }
    } else {
        toml::from_str(&existing).unwrap_or(RegistrySnapshot {
            schema_version: 1,
            generated_at: rfc3339_now(),
            registry_url: String::new(),
            entries: Vec::new(),
        })
    };
    snapshot.entries.retain(|e| e.id != recipe.corpus.id);
    snapshot.entries.push(RegistryEntry {
        id: recipe.corpus.id.clone(),
        name: recipe.corpus.name.clone(),
        description: recipe.corpus.description.clone(),
        license: recipe.corpus.license.clone(),
        size_compressed_gb: recipe.corpus.size_compressed_gb,
        size_indexed_gb: recipe.corpus.size_indexed_gb,
        toml_url: format!("file://{}/recipe.toml", recipe.corpus.id),
        sha256,
        enrichment_enabled: recipe
            .enrichment
            .as_ref()
            .map(|e| e.enabled)
            .unwrap_or(false),
        mesh_sharing: recipe.corpus.mesh_sharing,
        prebuilt: None,
        parent_corpus_id: recipe.corpus.parent_corpus_id.clone(),
        catalog_status: None,
    });
    snapshot.generated_at = rfc3339_now();
    let serialized =
        toml::to_string_pretty(&snapshot).map_err(|e| format!("serialize local registry: {e}"))?;
    let part = registry_path.with_extension("toml.part");
    std::fs::write(&part, serialized.as_bytes())
        .map_err(|e| format!("write local registry: {e}"))?;
    std::fs::rename(&part, &registry_path).map_err(|e| format!("commit local registry: {e}"))?;
    Ok(())
}

fn rfc3339_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_kind_labels_round_trip() {
        assert_eq!(parameter_kind_label(&ParameterKind::String), "string");
        assert_eq!(parameter_kind_label(&ParameterKind::Int), "int");
        assert_eq!(parameter_kind_label(&ParameterKind::Date), "date");
        assert_eq!(parameter_kind_label(&ParameterKind::List), "list");
    }

    #[test]
    fn toml_to_json_handles_arrays_and_strings() {
        let v = toml::Value::Array(vec![
            toml::Value::String("NVDA".into()),
            toml::Value::String("MSFT".into()),
        ]);
        let json = toml_to_json(&v);
        assert_eq!(json, serde_json::json!(["NVDA", "MSFT"]),);
    }
}
