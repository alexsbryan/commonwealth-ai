// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest extension (v0.4 §5): endpoint advertisement, corpus
//! install/progress DTOs, and the recipe-test surface.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::manifest::KnowledgeManifest;

// -----------------------------------------------------------------
// Section 7 — Ingest Extension (v0.4 §5)
// -----------------------------------------------------------------

/// v0.4 §5: the corpus-ingest endpoints advertised in
/// [`KnowledgeManifest::ingest`]. Values are paths relative to the
/// manifest's origin (the same convention as `search_endpoint`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEndpoints {
    /// `POST` — install a corpus by recipe id. See [`CorpusInstallRequest`].
    pub install_endpoint: String,
    /// `GET` — poll ingest progress. See [`CorpusProgressResponse`].
    pub progress_endpoint: String,
    /// `POST` — optional dry-run recipe test (§5.4). Present iff the
    /// host advertises the `ingest:recipe_test` feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_endpoint: Option<String>,
}

/// `POST {install_endpoint}` — install a corpus by recipe id (§5.1).
/// Idempotent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusInstallRequest {
    pub corpus_id: String,
    /// Recipe `[parameters]` values, keyed by parameter name. Empty map
    /// when the recipe takes no parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, serde_json::Value>,
}

/// Response to [`CorpusInstallRequest`] (§5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusInstallResponse {
    pub corpus_id: String,
    /// `true` — a fresh ingest job started. `false` — the corpus is
    /// already installed or an ingest for it is already running.
    pub spawned: bool,
}

/// Coarse ingest phase (§5.2). A protocol type — deliberately does not
/// embed any implementation's internal progress enum, so a host may
/// implement ingest without linking the reference engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestPhase {
    Pending,
    Downloading,
    Embedding,
    Indexing,
    Optimizing,
    Enriching,
    Complete,
    Failed,
}

impl IngestPhase {
    /// True for `Complete` and `Failed` — the terminal phases of the
    /// §5.3 poll state machine.
    pub fn is_terminal(self) -> bool {
        matches!(self, IngestPhase::Complete | IngestPhase::Failed)
    }
}

/// Per-corpus ingest progress (§5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusIngestProgress {
    pub phase: IngestPhase,
    /// Best-effort completion fraction in `[0,1]`; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    /// Human-readable detail; the error message when `phase = Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `GET {progress_endpoint}` response (§5.2). Keyed by `corpus_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorpusProgressResponse {
    #[serde(default)]
    pub progress: BTreeMap<String, CorpusIngestProgress>,
}

/// `POST {test_endpoint}` — dry-run a recipe over a small sample (§5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeTestRequest {
    /// The full recipe TOML source.
    pub recipe_toml: String,
    #[serde(default)]
    pub options: RecipeTestOptions,
}

/// Options for [`RecipeTestRequest`] (§5.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeTestOptions {
    /// Cap the number of documents pulled per stage; `None` = host default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_limit: Option<u32>,
    /// Skip any network acquisition (test extract/chunk over cached input).
    #[serde(default)]
    pub offline: bool,
}

/// Per-stage diagnostics from a recipe test (§5.4). A protocol type —
/// no implementation internals on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeStageReport {
    /// Stage name, e.g. `"acquire"`, `"extract"`, `"chunk"`.
    pub name: String,
    pub docs_in: u32,
    pub docs_out: u32,
    /// Things the stage expected but did not find (e.g. missed sections).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub misses: Vec<String>,
    /// A few sample outputs, for the author to eyeball.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample: Vec<String>,
}

/// Response to [`RecipeTestRequest`] (§5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeTestReport {
    pub stages: Vec<RecipeStageReport>,
    /// `true` iff every stage produced output (a usable recipe).
    pub ok: bool,
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_phase_terminality() {
        assert!(IngestPhase::Complete.is_terminal());
        assert!(IngestPhase::Failed.is_terminal());
        assert!(!IngestPhase::Embedding.is_terminal());
        assert!(!IngestPhase::Pending.is_terminal());
    }

    #[test]
    fn ingest_dtos_round_trip() {
        let mut params = BTreeMap::new();
        params.insert("year".to_string(), serde_json::json!(2026));
        let req = CorpusInstallRequest {
            corpus_id: "acme-emails".into(),
            parameters: params,
        };
        let back: CorpusInstallRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back.corpus_id, "acme-emails");
        assert_eq!(back.parameters["year"], serde_json::json!(2026));

        // An install request with no parameters omits the map on the wire.
        let bare = CorpusInstallRequest {
            corpus_id: "x".into(),
            parameters: BTreeMap::new(),
        };
        let v = serde_json::to_value(&bare).unwrap();
        assert!(v.get("parameters").is_none(), "empty parameters omitted");

        let prog = CorpusProgressResponse {
            progress: BTreeMap::from([(
                "acme-emails".to_string(),
                CorpusIngestProgress {
                    phase: IngestPhase::Embedding,
                    fraction: Some(0.4),
                    detail: None,
                },
            )]),
        };
        let back: CorpusProgressResponse =
            serde_json::from_str(&serde_json::to_string(&prog).unwrap()).unwrap();
        assert_eq!(back.progress["acme-emails"].phase, IngestPhase::Embedding);
    }

    #[test]
    fn ingest_endpoints_test_endpoint_optional() {
        let e = IngestEndpoints {
            install_endpoint: "/oicp/v1/corpus/install".into(),
            progress_endpoint: "/oicp/v1/corpus/progress".into(),
            test_endpoint: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(
            v.get("test_endpoint").is_none(),
            "absent recipe-test endpoint omitted"
        );
    }
}
