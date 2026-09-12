// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire shapes of the recipe-registry routes —
//! `POST /internal/corpus/recipes/import` and
//! `GET /internal/corpus/recipes/{corpus}/parameters`
//! (`sovereign_mesh::recipe_http`). Moved below the daemon 2026-09-11
//! (thin-desktop order): the desktop validated a recipe with an engine of
//! its own, wrote it into the daemon's recipes dir itself and read the
//! registry in-process to render a parameter form — three reasons a thin
//! client linked the knowledge engine.

use serde::{Deserialize, Serialize};

/// Answer of `POST /internal/corpus/recipes/import`. `success == false`
/// is a VALIDATION verdict with `errors` naming why (the TOML parsed, the
/// recipe did not pass); a TOML that does not parse, or a store the daemon
/// cannot write, is an HTTP error, not this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRecipeResult {
    pub success: bool,
    pub corpus_id: String,
    /// Where the recipe landed under the daemon's recipes dir; empty when
    /// `success == false`.
    pub recipe_path: String,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Body of `POST /internal/corpus/recipes/import`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRecipeRequest {
    pub toml_text: String,
}

/// Answer of `GET /internal/corpus/recipes/{corpus}/parameters` — the
/// `[parameters]` a recipe declares, for the install form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeParameterSchema {
    pub corpus_id: String,
    pub parameters: Vec<RecipeParameter>,
}

/// One declared parameter. `kind` is the recipe's `type` label verbatim
/// (`string` | `int` | `date` | `list`); `default` is the TOML default
/// rendered as JSON, `null` when none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeParameter {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub required: bool,
    pub default: Option<serde_json::Value>,
}
