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

// ─── The recipe dry run (`/internal/corpus/recipes/test`) ──────────────
//
// **Why these are not `oicp::RecipeTestReport`.** `POST /oicp/v1/recipe/test`
// (`commonwealth-api/src/routes_oicp_ingest.rs:71`) already dry-runs a recipe
// through the same `CorpusEngine::test_recipe`, and it was checked first
// (ARCH principle 11). It cannot serve the authoring panel, for three reasons
// its own client records:
//
// 1. Its report is `{stages, ok}` (`oicp-types/src/ingest.rs:132`). No
//    rendered markdown, no `source_reachable`, no chunk statistics — and `ok`
//    is deliberately weaker than `TestReport::passed()`, which also demands an
//    extraction rate over 80% and no over-limit chunks. The lossiness is
//    documented as a behaviour change at
//    `studio/crates/sovereign-recipe-author/src/http_tester.rs:9-30`.
// 2. Its `offline` flag is overloaded to mean sample size 0
//    (`routes_oicp_ingest.rs:105-110`), so "sample 100 documents without
//    touching the network" — the panel's own iterate-offline mode — cannot be
//    expressed on it at all.
// 3. It is synchronous over an unbounded download, with no job form.
//
// `oicp-types` is the cross-implementation protocol, versioned at §5.4;
// widening it with one surface's fields is the wrong direction. So this is a
// second shape under a name of its own rather than a second meaning under an
// existing one (principle 8).

/// Body of `POST /internal/corpus/recipes/test`.
///
/// The TOML travels, not a path: the recipe is a file the AUTHOR picked in
/// their own file dialog, and a daemon that reads client-supplied paths is a
/// different surface from one that reads its own data root. Same staging the
/// import route uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeDryRunRequest {
    pub toml_text: String,
    /// Source records to sample. `0` is validation-only: static checks plus,
    /// when `offline` is false, one HTTP HEAD on the source URL.
    pub sample_size: usize,
    /// Skip the reachability probe and any acquisition.
    pub offline: bool,
}

/// Answer of a recipe dry run — the union of what the authoring panel's
/// validate and test affordances each render, so one route serves both.
///
/// `report_markdown` is inline rather than a path: the file the author keeps
/// is written beside THEIR recipe by the surface that owns that directory,
/// and the daemon writes nothing outside its own root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeDryRunReport {
    /// `TestReport::passed()` — the strict verdict, not "produced chunks".
    pub passed: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub recipe_id: String,
    pub recipe_name: String,
    /// The HTTP HEAD verdict on the source URL. `None` means NOT ASKED
    /// (offline, or no URL to ask about) — which is a different fact from
    /// `Some(false)`, "asked, and it did not answer".
    #[serde(default)]
    pub source_reachable: Option<bool>,
    /// Zero for a validation-only run; extraction did not run.
    pub records_attempted: usize,
    pub records_succeeded: usize,
    pub extraction_rate: f32,
    pub total_chunks: usize,
    pub avg_chars: f32,
    /// `TestReport::to_markdown()`, verbatim.
    pub report_markdown: String,
}

/// Where one sampled dry run stands. Answer of
/// `GET /internal/corpus/recipes/test/{job}/progress`; the run is accepted by
/// `POST /internal/corpus/recipes/test` with an [`super::IngestJobAck`].
///
/// There is no `Idle`: a job id this daemon never minted is a 404, because
/// "no such job" and "a job that has not started" ask the caller for
/// different things (ARCH principle 6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeDryRunProgress {
    pub recipe_id: String,
    pub job_id: String,
    pub state: RecipeJobState,
    /// Set only in [`RecipeJobState::Complete`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<RecipeDryRunReport>,
    /// Set only in [`RecipeJobState::Error`]; the failure text verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The three states a recipe JOB can report — the sampled dry run and the
/// authoring harness both. One enum for both, because a client that polls
/// either writes the same match (ARCH principle 8).
///
/// It is deliberately NOT [`super::IndexBuildState`], which carries a fourth
/// arm: an index build is addressed by CORPUS, so "nobody ever asked" is a
/// state it must be able to report. These jobs are addressed by JOB ID, which
/// this daemon either minted or did not — the fourth arm would have no input
/// that produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeJobState {
    Running,
    Complete,
    Error,
}

// ─── The authoring harness (`/internal/corpus/recipes/harness`) ────────

/// Body of `POST /internal/corpus/recipes/harness`.
///
/// No `recapture`: the panel offers none, and a capture this daemon did not
/// ask for is a network round-trip a client cannot see. Add the field when a
/// surface grows the affordance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeHarnessRequest {
    pub toml_text: String,
    pub sample_size: usize,
    /// Run rung 6 — verify the atoms the daemon's installed corpus already
    /// holds. `false` leaves the rung absent, which is not the same as a
    /// rung that ran and passed.
    pub enrich: bool,
}

/// The deterministic verdict ladder, flattened for a surface to render.
///
/// `Run` is `sovereign_authoring_harness::HarnessRun`, which has no home at
/// this layer: the crate that defines it sits above `sovereign-contracts`,
/// and moving it down would add a dependency edge to hold seven pure-serde
/// types. So this follows the shape [`super::PreScanAnswerView`] set — the
/// daemon aliases it at the concrete type and serialises, a client that does
/// not link the harness passes `serde_json::Value` through, and the BYTES are
/// one definition's (ARCH principle 8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRunCardView<Run = serde_json::Value> {
    /// Roll-up: all stages pass -> green; any fail -> red. Warns never gate.
    pub green: bool,
    /// The full per-stage verdict ladder.
    pub run: Run,
    pub ran_at_unix: u64,
    /// Frozen-sample provenance — the "frozen: N docs" chip.
    pub frozen_docs: usize,
    pub frozen_captured_at: i64,
    /// True when THIS run performed the one networked capture step.
    pub frozen_captured_now: bool,
}

/// Where one harness run stands. Answer of
/// `GET /internal/corpus/recipes/harness/{job}/progress`; the run is accepted by
/// `POST /internal/corpus/recipes/harness` with an [`super::IngestJobAck`].
///
/// `Card` is [`HarnessRunCardView`] at whichever `Run` the reader can name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeHarnessProgress<Card = serde_json::Value> {
    pub recipe_id: String,
    pub job_id: String,
    pub state: RecipeJobState,
    /// Set only in [`RecipeJobState::Complete`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
    /// Set only in [`RecipeJobState::Error`]; the failure text verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
