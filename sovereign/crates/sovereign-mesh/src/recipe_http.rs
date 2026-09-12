// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-registry writes and reads the desktop stops doing in-process
//! (thin-desktop order, 2026-09-11): importing an authored recipe into the
//! local registry, and reading a recipe's `[parameters]` for the install
//! form.
//!
//! Both go through the DAEMON's `CorpusEngine` and its `RecipeRegistry`
//! — the registry whose `overrides_dir` IS the recipes dir every install
//! resolves through. `recipe_commands.rs` used to build a stub engine in a
//! temp dir to validate, then write `~/.svrnmesh/recipes/<id>/recipe.toml`
//! and upsert `registry.toml` with a loop of its own (a second copy of the
//! CLI's `recipe publish` loop); both loops now call
//! `RecipeRegistry::install_local_recipe`, one decider (ARCH principle 8).

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use corpus_engine::{CorpusEngine, ParameterKind, Recipe, TestOptions};
use sovereign_contracts::daemon_wire::{
    ImportRecipeRequest, ImportRecipeResult, RecipeParameter, RecipeParameterSchema,
};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::Absence;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// The recipe-registry router. Mounted unconditionally beside
/// `corpus_catalog_http`; a daemon with no corpus engine answers 503
/// naming that, which is a different fact from an unmounted router's 404.
pub fn recipe_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/corpus/recipes/import", post(import))
        .route(
            "/internal/corpus/recipes/{corpus}/parameters",
            get(parameters),
        )
        .localhost_only_with(daemon)
}

/// POST `/internal/corpus/recipes/import` `{toml_text}` — validate the
/// recipe offline (`test_recipe`, sample size 0) and, when it passes,
/// install it into the daemon's recipes dir + local `registry.toml`.
/// Answers `ImportRecipeResult`: a recipe that fails validation is a 200
/// with `success: false` and the errors (the form renders them); a body
/// that is not a recipe at all, or a store the daemon cannot write, is
/// 400 / 500.
async fn import(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ImportRecipeRequest>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipe = Recipe::from_toml(&body.toml_text)
        .map_err(|e| Absence::invalid(format!("recipe TOML parse failed: {e}")))?;
    let corpus_id = recipe.corpus.id.clone();
    if corpus_id.is_empty() {
        return Err(Absence::invalid("recipe `[corpus] id` must not be empty"));
    }

    // The validation harness reads a file; stage the text beside the
    // engine's own recipes rather than in a shared system temp dir.
    let staging = engine
        .recipes_dir()
        .join("_import")
        .join(format!("{corpus_id}.toml"));
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Absence::internal(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(&staging, &body.toml_text)
        .map_err(|e| Absence::internal(format!("stage recipe for validation: {e}")))?;
    let options = TestOptions {
        sample_size: 0,
        embed: false,
        offline: true,
        ..Default::default()
    };
    let report = engine.test_recipe(&staging, &options).await;
    let _ = std::fs::remove_file(&staging);
    let report =
        report.map_err(|e| Absence::internal(format!("validation harness failed: {e}")))?;

    if !report.validation.errors.is_empty() {
        tracing::debug!(
            corpus_id = %corpus_id,
            errors = report.validation.errors.len(),
            "recipe_http: import refused by validation"
        );
        return Ok((
            StatusCode::OK,
            Json(ImportRecipeResult {
                success: false,
                corpus_id,
                recipe_path: String::new(),
                errors: report.validation.errors.clone(),
                warnings: report.validation.warnings.clone(),
            }),
        )
            .into_response());
    }

    let recipe_path = engine
        .registry()
        .install_local_recipe(&recipe, &body.toml_text)
        .map_err(|e| Absence::internal(format!("install recipe `{corpus_id}`: {e}")))?;
    tracing::info!(
        corpus_id = %corpus_id,
        path = %recipe_path.display(),
        warnings = report.validation.warnings.len(),
        "recipe_http: recipe imported into the local registry"
    );
    Ok((
        StatusCode::OK,
        Json(ImportRecipeResult {
            success: true,
            corpus_id,
            recipe_path: recipe_path.display().to_string(),
            errors: Vec::new(),
            warnings: report.validation.warnings.clone(),
        }),
    )
        .into_response())
}

/// GET `/internal/corpus/recipes/{corpus}/parameters` — the recipe's
/// declared `[parameters]`, resolved through the daemon's registry (local
/// override first, then the registry entry). Answers
/// `RecipeParameterSchema`; an unknown recipe is a 404 naming it.
async fn parameters(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipe = engine
        .registry()
        .fetch_recipe(&corpus)
        .await
        .map_err(|e| Absence::missing(format!("recipe `{corpus}`: {e}")))?;
    let parameters: Vec<RecipeParameter> = recipe
        .parameters
        .iter()
        .map(|(name, spec)| RecipeParameter {
            name: name.clone(),
            kind: parameter_kind_label(&spec.kind).to_string(),
            description: spec.description.clone(),
            required: spec.required,
            default: spec.default.as_ref().map(toml_to_json),
        })
        .collect();
    tracing::debug!(
        corpus_id = %corpus,
        parameters = parameters.len(),
        "recipe_http: parameter schema served"
    );
    Ok((
        StatusCode::OK,
        Json(RecipeParameterSchema {
            corpus_id: recipe.corpus.id,
            parameters,
        }),
    )
        .into_response())
}

fn engine_for(daemon: &Arc<EmbeddedDaemon>) -> Result<&Arc<CorpusEngine>, Absence> {
    daemon
        .corpus_engine()
        .ok_or_else(|| Absence::unavailable("corpus engine not initialised"))
}

/// The recipe's `type` label for a parameter, as the form keys on it.
pub fn parameter_kind_label(k: &ParameterKind) -> &'static str {
    match k {
        ParameterKind::String => "string",
        ParameterKind::Int => "int",
        ParameterKind::Date => "date",
        ParameterKind::List => "list",
    }
}

/// A TOML default rendered as JSON for the form.
pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
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
        assert_eq!(toml_to_json(&v), serde_json::json!(["NVDA", "MSFT"]));
    }
}
